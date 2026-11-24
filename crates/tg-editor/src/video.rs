//! UI-independent video editor model and FFmpeg export pipeline.
//!
//! Port of `Telegram/SourceFiles/editor/video/` (`video_editor.{h,cpp}`,
//! `video_editor_common.{h,cpp}`, `video_editor_quality.{h,cpp}`,
//! `video_timeline.{h,cpp}`, `video_clip.{h,cpp}`, `video_segment_player.{h,cpp}`,
//! `video_timeline_seeker.{h,cpp}`), `Telegram/SourceFiles/editor/video_item_timeline.{h,cpp}`,
//! and `Telegram/SourceFiles/editor/editor_trim_timeline.{h,cpp}`.

pub mod clip;
pub mod common;
pub mod export;
pub mod quality;
pub mod segment_player;
pub mod timeline;

use std::path::{Path, PathBuf};

use image::RgbaImage;

use crate::EditorError;
use crate::common::PhotoModifications;
use crate::geometry::{Rect, Size, normalize_angle_90};

pub use clip::{
    ClipGlobalSpan, ClipId, ClipPosition, MultiClipTimeline, VideoClip, VideoClipSource,
    VideoClipState, VideoTrim,
};
pub use common::{
    AUDIO_BITRATE_PER_CHANNEL, EXPORT_AUDIO_SAMPLE_RATE, MAX_GIF_SIZE_BYTES, MAX_VIDEO_BITRATE,
    MIN_VIDEO_BITRATE, SILENT_AUDIO_BITRATE, VideoAudioTrack, VideoEditorData, VideoModifications,
    VideoSourceInfo, downscaled_size, edited_frame_size, estimate_transcoded_size, even_down,
    even_size, sanitized_fps, target_video_bitrate, transcoded_duration_ms, transcoded_size,
    video_edited,
};
pub use export::{
    MultiClipExportOptions, MultiClipExportPlan, MultiClipInputSpec, OverlayFilterSpec,
    VideoExportResult, build_multi_clip_export_plan, export_video, extract_cover_frame,
    extract_timeline_thumbnails, is_ffmpeg_available, probe_video_source,
};
pub use quality::{
    QUALITY_LEVEL_SIDES, VideoQualityLevel, VideoQualitySelection, video_quality_label,
    video_quality_levels,
};
pub use segment_player::{
    MultiClipPlayerEvent, MultiClipSegmentPlayer, SEEK_THROTTLE_MS, SegmentPlaybackEvent,
    SegmentPlaybackMode, SegmentPlayer, SegmentPlayerOptions, TimelineSeeker,
};
pub use timeline::{
    FRAME_WIDTH_TOLERANCE, ItemVisibilityRange, MAX_CACHED_FRAME_SETS, MAX_TIMELINE_FRAMES,
    MIN_SELECTION_MS, MIN_VISIBLE_SPAN_MS, OverlayVisibilityTrack, STAMP_PRECISION,
    TrimTimelineDescriptor, TrimTimelineModel, VideoItemTimelineModel, VideoTimelineFrames,
    VideoTimelineFramesCache, format_stamp, parse_stamp,
};

/// UI-independent controller uniting trim timeline, quality levels, audio track/mute/GIF,
/// cover-frame selection, geometry transforms, and FFmpeg export (`Editor::VideoEditor`).
#[derive(Debug, Clone)]
pub struct VideoEditorModel {
    /// Optional source file path on disk (`None` for synthetic/in-memory preview clips).
    pub source_path: Option<PathBuf>,
    /// Probed or provided source metadata.
    pub source_info: VideoSourceInfo,
    /// Editor configuration & constraints.
    pub data: VideoEditorData,
    /// Trim & cover timeline state machine.
    pub timeline: TrimTimelineModel,
    /// Available & selected quality levels.
    pub quality: VideoQualitySelection,
    /// Geometry modifications (crop, rotation, flip).
    pub geometry: PhotoModifications,
    /// Audio track mix / mute state.
    pub audio: VideoAudioTrack,
    /// Send as silent looping GIF.
    pub gif: bool,
    /// Whether GIF toggle is allowed (`file_size <= MAX_GIF_SIZE_BYTES`).
    pub gif_allowed: bool,
    /// Cached thumbnail frame strips for the trim timeline.
    pub frames_cache: VideoTimelineFramesCache,
}

impl VideoEditorModel {
    /// Builds a video editor model from probed [`VideoSourceInfo`], [`VideoEditorData`],
    /// and initial [`VideoModifications`].
    #[must_use]
    pub fn new(
        source_path: Option<PathBuf>,
        source_info: VideoSourceInfo,
        data: VideoEditorData,
        initial: VideoModifications,
    ) -> Self {
        let mut geometry = initial.geometry.clone();
        geometry.crop_type = data.editor.crop_type;
        geometry.crop_mode = data.editor.crop_mode;

        let timeline = TrimTimelineModel::new(TrimTimelineDescriptor {
            duration_ms: source_info.duration_ms.max(1),
            max_duration_ms: data.max_duration_ms,
            min_duration_ms: data.min_duration_ms,
            from_ms: initial.from_ms,
            till_ms: initial.till_ms,
            cover_ms: initial.cover_ms,
            trim_only: false,
        });

        let edited_dim = edited_frame_size(source_info.dimensions, &geometry);
        let mut quality = VideoQualitySelection::for_size(edited_dim);
        quality.set_shorter_side(initial.quality);

        let mut audio = source_info.audio.clone();
        audio.set_volume(initial.audio_volume);
        if initial.muted || initial.gif || data.remove_audio {
            audio.set_muted(true);
        }

        let gif_allowed =
            data.allow_gif && !data.remove_audio && source_info.file_size <= MAX_GIF_SIZE_BYTES;
        let gif = gif_allowed && initial.gif;

        Self {
            source_path,
            source_info,
            data,
            timeline,
            quality,
            geometry,
            audio,
            gif,
            gif_allowed,
            frames_cache: VideoTimelineFramesCache::new(),
        }
    }

    /// Opens a real video file on disk, probing its metadata via `tg-video` / FFmpeg.
    pub fn open_file(
        path: &Path,
        data: VideoEditorData,
        initial: VideoModifications,
    ) -> Result<Self, EditorError> {
        let info = probe_video_source(path)?;
        Ok(Self::new(Some(path.to_path_buf()), info, data, initial))
    }

    /// Rotates the video clockwise by 90 degrees and refreshes quality levels.
    pub fn rotate_cw(&mut self) {
        self.geometry.angle = normalize_angle_90(self.geometry.angle + 90);
        self.refresh_quality_levels();
    }

    /// Toggles horizontal flip.
    pub fn toggle_flip(&mut self) {
        self.geometry.flipped = !self.geometry.flipped;
    }

    /// Updates the crop rectangle (in source display pixels) and refreshes quality levels.
    pub fn set_crop(&mut self, crop: Option<Rect>) {
        self.geometry.crop = crop;
        self.refresh_quality_levels();
    }

    /// Toggles or sets mute on the audio track.
    pub fn set_muted(&mut self, muted: bool) {
        self.audio.set_muted(muted);
        if !muted {
            self.gif = false;
        }
    }

    /// Sets the audio track gain (`0.0..=1.0`).
    pub fn set_audio_volume(&mut self, volume: f32) {
        self.audio.set_volume(volume);
        if self.audio.is_sounding() {
            self.gif = false;
        }
    }

    /// Toggles "Send as GIF" (silent loop) if allowed by file size and editor data.
    pub fn set_gif(&mut self, gif: bool) {
        if !self.gif_allowed {
            return;
        }
        self.gif = gif;
        if gif {
            self.audio.set_muted(true);
        }
    }

    /// Resets trim, cover, quality, geometry, mute, and GIF back to original defaults.
    pub fn reset(&mut self) {
        self.geometry = PhotoModifications {
            crop_type: self.data.editor.crop_type,
            crop_mode: self.data.editor.crop_mode,
            ..PhotoModifications::default()
        };
        self.timeline = TrimTimelineModel::new(TrimTimelineDescriptor {
            duration_ms: self.source_info.duration_ms.max(1),
            max_duration_ms: self.data.max_duration_ms,
            min_duration_ms: self.data.min_duration_ms,
            from_ms: 0,
            till_ms: 0,
            cover_ms: 0,
            trim_only: false,
        });
        self.gif = false;
        self.audio.muted = self.data.remove_audio;
        self.audio.volume = if self.data.remove_audio { 0.0 } else { 1.0 };
        self.refresh_quality_levels();
        self.quality.set_shorter_side(0);
    }

    fn refresh_quality_levels(&mut self) {
        let edited = edited_frame_size(self.source_info.dimensions, &self.geometry);
        self.quality.update_for_size(edited);
    }

    /// Collects the current non-destructive [`VideoModifications`] (`VideoEditor::collect`).
    #[must_use]
    pub fn collect(&self) -> VideoModifications {
        VideoModifications {
            geometry: self.geometry.clone(),
            from_ms: self.timeline.from_ms(),
            till_ms: self.timeline.till_ms(),
            cover_ms: self.timeline.cover_ms(),
            quality: if self.data.allow_quality {
                self.quality.selected_shorter_side()
            } else {
                0
            },
            gif: self.gif,
            muted: self.audio.muted,
            audio_volume: self.audio.volume,
        }
    }

    /// Returns `true` if any edit has been applied (`VideoEditor::edited`).
    #[must_use]
    pub fn is_edited(&self) -> bool {
        video_edited(
            &self.collect(),
            self.source_info.dimensions,
            self.source_info.duration_ms,
            self.source_info.audio.has_audio,
        )
    }

    /// Returns the estimated output MP4 file size in bytes.
    #[must_use]
    pub fn estimated_output_size(&self) -> u64 {
        estimate_transcoded_size(&self.collect(), &self.data, &self.source_info)
    }

    /// Returns the target output video dimensions for the active edits.
    #[must_use]
    pub fn output_dimensions(&self) -> Size {
        transcoded_size(self.source_info.dimensions, &self.collect())
    }

    /// Extracts (or returns cached) timeline thumbnail frames for a strip of `strip_width x strip_height`.
    pub fn load_timeline_thumbnails(
        &mut self,
        strip_width: i32,
        strip_height: i32,
    ) -> Result<VideoTimelineFrames, EditorError> {
        let (positions, box_size) = self.timeline.thumbnail_sample_positions(
            strip_width,
            strip_height,
            self.source_info.dimensions,
        );
        let from_ms = self.timeline.visible_from_ms();
        let span_ms = (self.timeline.visible_till_ms() - from_ms).max(1);
        if let Some(cached) = self
            .frames_cache
            .find(positions.len(), from_ms, span_ms, box_size)
        {
            return Ok(cached.clone());
        }
        let Some(ref path) = self.source_path else {
            return Err(EditorError::VideoProbe(
                "no source video file path set".to_owned(),
            ));
        };
        let frames = extract_timeline_thumbnails(path, &positions, box_size)?;
        let set = VideoTimelineFrames {
            frames,
            from_ms,
            span_ms,
            box_size,
        };
        self.frames_cache.add(set.clone());
        Ok(set)
    }

    /// Extracts the current cover frame preview image from the source video.
    pub fn extract_cover_preview(&self, max_side: u32) -> Result<RgbaImage, EditorError> {
        let Some(ref path) = self.source_path else {
            return Err(EditorError::VideoProbe(
                "no source video file path set".to_owned(),
            ));
        };
        extract_cover_frame(path, &self.collect(), self.source_info.dimensions, max_side)
    }

    /// Exports the edited video to `output_path` through FFmpeg / `tg-video`.
    pub fn export_to(
        &self,
        output_path: &Path,
        include_cover_jpeg: bool,
    ) -> Result<VideoExportResult, EditorError> {
        let Some(ref path) = self.source_path else {
            return Err(EditorError::VideoExport(
                "no source video file path set".to_owned(),
            ));
        };
        export_video(
            path,
            output_path,
            &self.collect(),
            &self.data,
            include_cover_jpeg,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_source_info() -> VideoSourceInfo {
        VideoSourceInfo {
            dimensions: Size::new(1920, 1080),
            duration_ms: 12_000,
            fps: 30.0,
            file_size: 4_200_000,
            video_bitrate: 2_800_000,
            audio: VideoAudioTrack {
                has_audio: true,
                channels: 2,
                sample_rate: 48_000,
                bitrate: 128_000,
                codec_name: "aac".to_owned(),
                muted: false,
                volume: 1.0,
            },
            video_codec: "h264".to_owned(),
            rotation: 0,
        }
    }

    #[test]
    fn trim_ranges_clamp_min_max_and_cover() {
        let mut tl = TrimTimelineModel::new(TrimTimelineDescriptor {
            duration_ms: 10_000,
            max_duration_ms: 6_000,
            min_duration_ms: 500,
            from_ms: 1_000,
            till_ms: 8_500,  // exceeds max_duration (6000) -> clamped to 7000
            cover_ms: 9_000, // outside [1000, 7000] -> clamped to 7000
            trim_only: false,
        });
        assert_eq!(tl.from_ms(), 1_000);
        assert_eq!(tl.till_ms(), 7_000);
        assert_eq!(tl.cover_ms(), 7_000);

        // Moving left handle too close to right respects min_duration_ms = 500
        tl.set_from(6_900);
        assert_eq!(tl.from_ms(), 6_500);
        assert_eq!(tl.till_ms(), 7_000);

        // Selecting cover outside current range clamps into [from, till]
        tl.set_cover(2_000);
        assert_eq!(tl.cover_ms(), 6_500);
        tl.set_cover(6_750);
        assert_eq!(tl.cover_ms(), 6_750);

        // Sliding window preserves span and clamps cover
        tl.move_window_to(3_000);
        assert_eq!(tl.till_ms() - tl.from_ms(), 500);
        assert!(tl.cover_ms() >= tl.from_ms() && tl.cover_ms() <= tl.till_ms());

        // Range text editing ("0:02.0 – 0:05.5")
        assert!(tl.apply_duration_text("0:02.0 - 0:05.5"));
        assert_eq!(tl.from_ms(), 2_000);
        assert_eq!(tl.till_ms(), 5_500);
        assert_eq!(tl.duration_label_text(), "0:02.0 – 0:05.5");
    }

    #[test]
    fn stamp_format_and_parse_round_trip() {
        assert_eq!(format_stamp(0, 1), "0:00.0");
        assert_eq!(format_stamp(65_432, 3), "1:05.432");
        assert_eq!(format_stamp(3_661_200, 1), "1:01:01.2");
        assert_eq!(parse_stamp("1:05.432"), Some(65_432));
        assert_eq!(parse_stamp("0:02.5"), Some(2_500));
        assert_eq!(parse_stamp("12"), Some(12_000));
        assert_eq!(parse_stamp("1:01:01.2"), Some(3_661_200));
        assert_eq!(parse_stamp("bad"), None);
    }

    #[test]
    fn quality_levels_and_resolution_math_match_tdesktop() {
        // 1920x1080 -> shorter side = 1080 -> [360p, 480p, 720p, Original]
        let levels = video_quality_levels(Size::new(1920, 1080));
        assert_eq!(levels.len(), 4);
        assert_eq!(levels[0].shorter_side, 360);
        assert_eq!(levels[0].resolution, Size::new(640, 360));
        assert_eq!(video_quality_label(&levels[0]), "360p");
        assert_eq!(levels[2].shorter_side, 720);
        assert_eq!(levels[2].resolution, Size::new(1280, 720));
        assert_eq!(levels[3].shorter_side, 0);
        assert_eq!(video_quality_label(&levels[3]), "Original");

        // 4K (3840x2160) includes 1080p before Original
        let levels_4k = video_quality_levels(Size::new(3840, 2160));
        assert_eq!(levels_4k.len(), 5);
        assert_eq!(levels_4k[3].shorter_side, 1080);
        assert_eq!(levels_4k[3].resolution, Size::new(1920, 1080));

        // 320x240 has only Original
        let levels_tiny = video_quality_levels(Size::new(320, 240));
        assert_eq!(levels_tiny.len(), 1);
        assert!(levels_tiny[0].is_original());
    }

    #[test]
    fn video_editor_model_mute_audio_gif_cover_and_size_estimate() {
        let mut model = VideoEditorModel::new(
            None,
            sample_source_info(),
            VideoEditorData::default(),
            VideoModifications::default(),
        );
        assert!(!model.is_edited());
        assert!(model.audio.is_sounding());
        assert!(model.gif_allowed);

        // Trim + pick cover + downscale to 480p + mute
        model.timeline.set_trim(2_000, 7_000);
        model.timeline.set_cover(4_500);
        model.quality.set_shorter_side(480);
        model.set_muted(true);

        let mods = model.collect();
        assert!(model.is_edited());
        assert_eq!(mods.from_ms, 2_000);
        assert_eq!(mods.till_ms, 7_000);
        assert_eq!(mods.cover_ms, 4_500);
        assert_eq!(mods.quality, 480);
        assert!(mods.muted);
        assert!(!model.audio.is_sounding());
        // 1920 * (480 / 1080) = 853.33 -> round 853 -> EvenDown = 852 (matches media_video_encode.cpp)
        assert_eq!(model.output_dimensions(), Size::new(852, 480));

        let est_muted = model.estimated_output_size();
        model.set_muted(false);
        model.set_audio_volume(0.8);
        let est_audio = model.estimated_output_size();
        assert!(
            est_audio > est_muted,
            "adding audio should increase estimate"
        );

        // Thumbnail sample positions cover visible range
        let (positions, box_sz) =
            model
                .timeline
                .thumbnail_sample_positions(360, 48, model.source_info.dimensions);
        assert!(!positions.is_empty() && positions.len() <= MAX_TIMELINE_FRAMES);
        assert!(box_sz.width > 0 && box_sz.height == 48);

        // Reset restores unedited state
        model.reset();
        assert!(!model.is_edited());
        assert_eq!(model.timeline.from_ms(), 0);
        assert_eq!(model.timeline.till_ms(), 12_000);
        assert_eq!(model.timeline.cover_ms(), 0);
    }

    #[test]
    fn ffmpeg_export_tiny_generated_clip_to_temp_dir() {
        if !is_ffmpeg_available() {
            eprintln!("skipping ffmpeg_export test: FFmpeg runtime DLLs unavailable");
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let src_path = dir.path().join("input.mp4");
        let out_path = dir.path().join("exported.mp4");

        // Generate a tiny 1.5s 160x120 clip with 440Hz sine audio via tg-video FFmpeg shim.
        tg_video::generate_test_clip(&src_path, 160, 120, 15, 1_500, true)
            .expect("generate synthetic clip");
        assert!(src_path.is_file());

        // Open in VideoEditorModel, extract timeline thumbnails, trim [300ms..1100ms], select cover,
        // mute audio, and export to temp MP4.
        let mut model = VideoEditorModel::open_file(
            &src_path,
            VideoEditorData::default(),
            VideoModifications::default(),
        )
        .expect("probe synthetic clip");
        assert_eq!(model.source_info.dimensions, Size::new(160, 120));
        assert!(model.source_info.duration_ms >= 1_200);
        assert!(model.source_info.audio.has_audio);

        let thumbs = model
            .load_timeline_thumbnails(240, 40)
            .expect("extract timeline thumbnails");
        assert!(!thumbs.frames.is_empty());

        model.timeline.set_trim(300, 1_100);
        model.timeline.set_cover(650);
        model.set_muted(true);

        let exported = model
            .export_to(&out_path, true)
            .expect("export trimmed muted clip");
        assert!(exported.output_path.is_file());
        assert!(exported.file_size > 0);
        assert_eq!(exported.dimensions, Size::new(160, 120));
        assert!(!exported.has_audio);
        assert!(
            exported.duration_ms >= 500 && exported.duration_ms <= 1_100,
            "unexpected trimmed duration: {}ms",
            exported.duration_ms
        );
        let cover_jpeg = exported.cover_jpeg.expect("cover JPEG bytes");
        assert!(cover_jpeg.starts_with(&[0xFF, 0xD8]), "valid JPEG header");

        // Verify exported file is probeable back through FFmpeg.
        let probed_out = probe_video_source(&out_path).expect("probe exported MP4");
        assert_eq!(probed_out.dimensions, Size::new(160, 120));
        assert!(!probed_out.audio.has_audio);

        // Also verify unmuted audio export keeps the audio track.
        let out_with_audio = dir.path().join("exported_audio.mp4");
        model.set_muted(false);
        model.set_audio_volume(0.75);
        let exported_audio = model
            .export_to(&out_with_audio, false)
            .expect("export with audio");
        assert!(exported_audio.has_audio);
        assert!(exported_audio.file_size > 0);
    }
}
