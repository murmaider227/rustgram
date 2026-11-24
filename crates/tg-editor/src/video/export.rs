//! UI-independent FFmpeg video probing, thumbnail extraction, cover extraction,
//! and MP4 export pipeline backed by `tg-video`.
//!
//! Port of `Telegram/SourceFiles/editor/video/video_editor_common.cpp` (`ExtractCoverImage`,
//! `ComposeVideoSource`) and `Telegram/SourceFiles/media/media_video_encode.cpp` (`TranscodeVideo`).

use std::path::{Path, PathBuf};

use image::RgbaImage;
use tg_video::{
    FfmpegMedia, FfmpegOpenOptions, MAX_FRAME_AREA, Size as VideoSize, VideoEncodeConfig,
    extract_frame_at_ms, extract_timeline_frames, ffmpeg_runtime_available, transcode_video_file,
};

use crate::EditorError;
use crate::apply::apply;
use crate::common::PhotoModifications;
use crate::export::{PhotoExportSettings, export_jpeg};
use crate::geometry::{Rect, Size, normalize_angle_90};
use crate::scene::{ItemId, PaintScene, SceneItemKind};
use crate::video::clip::{ClipId, MultiClipTimeline};
use crate::video::common::{
    EXPORT_AUDIO_SAMPLE_RATE, VideoAudioTrack, VideoEditorData, VideoModifications,
    VideoSourceInfo, downscaled_size, edited_frame_size, even_size, sanitized_fps,
    target_video_bitrate, transcoded_size,
};
use crate::video::timeline::OverlayVisibilityTrack;

/// Result of exporting an edited video clip via [`export_video`].
#[derive(Debug, Clone)]
pub struct VideoExportResult {
    /// Path to the exported MP4 file.
    pub output_path: PathBuf,
    /// Encoded output dimensions.
    pub dimensions: Size,
    /// Exported clip duration in milliseconds.
    pub duration_ms: i64,
    /// Whether the exported MP4 contains an audio stream.
    pub has_audio: bool,
    /// Size of the exported MP4 file in bytes.
    pub file_size: u64,
    /// Optional extracted cover frame RGBA image (with geometry transforms applied).
    pub cover_rgba: Option<RgbaImage>,
    /// Optional Telegram-compliant JPEG thumbnail bytes encoded from `cover_rgba`.
    pub cover_jpeg: Option<Vec<u8>>,
}

/// Returns `true` when the FFmpeg runtime DLLs are available for probing/transcoding.
#[must_use]
pub fn is_ffmpeg_available() -> bool {
    ffmpeg_runtime_available()
}

/// Probes a video file on disk using `tg-video`'s FFmpeg demuxer (`Media::Encode::ProbeSource`).
pub fn probe_video_source(path: &Path) -> Result<VideoSourceInfo, EditorError> {
    let file_size = std::fs::metadata(path)
        .map_err(|e| EditorError::VideoProbe(format!("metadata failed: {e}")))?
        .len();
    let media = FfmpegMedia::open_file(
        path,
        FfmpegOpenOptions {
            seekable: true,
            ignore_editlist: false,
            hw_allowed: false,
            open_video: false,
            open_audio: false,
            video_max_area: MAX_FRAME_AREA,
            thread_count: 1,
        },
    )
    .map_err(|e| EditorError::VideoProbe(e.to_string()))?;

    let info = media.info();
    let (disp_w, disp_h) = if info.video_rotation.swaps_width_height() {
        (info.video_height, info.video_width)
    } else {
        (info.video_width, info.video_height)
    };
    let duration_ms = if info.video_duration_ms > 0 {
        info.video_duration_ms
    } else if info.format_duration_ms > 0 {
        info.format_duration_ms
    } else {
        0
    };
    let audio = VideoAudioTrack {
        has_audio: info.has_audio,
        channels: info.audio_channels,
        sample_rate: info.audio_sample_rate,
        bitrate: 0,
        codec_name: info.audio_codec_name.clone(),
        muted: false,
        volume: 1.0,
    };
    let approx_video_bitrate = if duration_ms > 0 && file_size > 0 {
        ((file_size as f64 * 8.0) / (duration_ms as f64 / 1000.0)).round() as i64
    } else {
        0
    };

    Ok(VideoSourceInfo {
        dimensions: Size::new(disp_w as i32, disp_h as i32),
        duration_ms,
        fps: if info.video_fps > 0.0 {
            info.video_fps
        } else {
            30.0
        },
        file_size,
        video_bitrate: approx_video_bitrate,
        audio,
        video_codec: info.video_codec_name.clone(),
        rotation: info.video_rotation.degrees(),
    })
}

/// Extracts timeline thumbnail frames at `positions_ms` scaled to fit `thumb_box`.
pub fn extract_timeline_thumbnails(
    path: &Path,
    positions_ms: &[i64],
    thumb_box: Size,
) -> Result<Vec<RgbaImage>, EditorError> {
    let box_sz = VideoSize::new(
        thumb_box.width.max(2) as u32,
        thumb_box.height.max(2) as u32,
    );
    extract_timeline_frames(path, positions_ms, box_sz)
        .map_err(|e| EditorError::VideoProbe(e.to_string()))
}

/// Extracts the selected cover frame from `path` and applies crop/rotate/flip transforms
/// (`Editor::ExtractCoverImage`).
pub fn extract_cover_frame(
    path: &Path,
    mods: &VideoModifications,
    dimensions: Size,
    max_side: u32,
) -> Result<RgbaImage, EditorError> {
    if dimensions.is_empty() || max_side == 0 {
        return Err(EditorError::EmptyImage);
    }
    let mut geom = mods.geometry.clone();
    geom.paint = None;
    let crop_side = match geom.crop {
        Some(c) if !c.is_empty() => c.width.min(c.height),
        _ => dimensions.width.min(dimensions.height),
    };
    let ratio = if crop_side > 0 {
        (f64::from(max_side) / f64::from(crop_side)).min(1.0)
    } else {
        1.0
    };
    let box_sz = VideoSize::new(
        ((f64::from(dimensions.width) * ratio).round() as u32).max(2),
        ((f64::from(dimensions.height) * ratio).round() as u32).max(2),
    );
    let raw = extract_frame_at_ms(path, mods.cover_ms.max(0), Some(box_sz))
        .map_err(|e| EditorError::VideoProbe(e.to_string()))?;

    if let Some(crop) = geom.crop
        && !crop.is_empty()
        && dimensions.width > 0
    {
        let scale = f64::from(raw.width()) / f64::from(dimensions.width);
        geom.crop = Some(Rect::new(
            (f64::from(crop.x) * scale).round() as i32,
            (f64::from(crop.y) * scale).round() as i32,
            ((f64::from(crop.width) * scale).round() as i32).max(1),
            ((f64::from(crop.height) * scale).round() as i32).max(1),
        ));
    }

    Ok(apply(&raw, &geom))
}

/// Exports an edited video clip to `output_path` through `tg-video`'s FFmpeg C shim,
/// applying trim `[from_ms, till_ms]`, quality downscaling, crop/rotate/flip, audio mute/volume,
/// and optional cover-frame JPEG extraction.
pub fn export_video(
    input_path: &Path,
    output_path: &Path,
    mods: &VideoModifications,
    data: &VideoEditorData,
    include_cover_jpeg: bool,
) -> Result<VideoExportResult, EditorError> {
    let source = probe_video_source(input_path)?;
    let target = transcoded_size(source.dimensions, mods);
    let (crop_x, crop_y, crop_w, crop_h) = match mods.geometry.crop {
        Some(c) if !c.is_empty() => (c.x, c.y, c.width, c.height),
        _ => (0, 0, 0, 0),
    };
    let strip_audio = mods.strips_audio(data);
    let out_fps = if data.fps_limit > 1.0 && data.fps_limit < source.fps {
        data.fps_limit
    } else {
        source.fps
    };

    let cfg = VideoEncodeConfig {
        from_ms: mods.from_ms.max(0),
        till_ms: mods.till_ms.max(0),
        target_width: target.width,
        target_height: target.height,
        crop_x,
        crop_y,
        crop_w,
        crop_h,
        rotation: normalize_angle_90(mods.geometry.angle),
        flipped: mods.geometry.flipped,
        remove_audio: strip_audio,
        audio_volume: if strip_audio { 0.0 } else { mods.audio_volume },
        fps_limit: data.fps_limit,
        video_bitrate: target_video_bitrate(target, out_fps),
    };

    let encoded = transcode_video_file(input_path, output_path, &cfg)
        .map_err(|e| EditorError::VideoExport(e.to_string()))?;

    let (cover_rgba, cover_jpeg) = if include_cover_jpeg {
        let side = target.width.max(target.height).clamp(160, 640) as u32;
        match extract_cover_frame(input_path, mods, source.dimensions, side) {
            Ok(img) => {
                let jpeg = export_jpeg(&img, &PhotoExportSettings::standard())
                    .ok()
                    .map(|j| j.bytes);
                (Some(img), jpeg)
            }
            Err(_) => (None, None),
        }
    } else {
        (None, None)
    };

    Ok(VideoExportResult {
        output_path: output_path.to_path_buf(),
        dimensions: Size::new(encoded.output_width as i32, encoded.output_height as i32),
        duration_ms: encoded.duration_ms,
        has_audio: encoded.has_audio,
        file_size: encoded.file_size,
        cover_rgba,
        cover_jpeg,
    })
}

// ---------------------------------------------------------------------------
// Multi-clip FFmpeg filtergraph & concat export plan
// ---------------------------------------------------------------------------

/// Options controlling multi-clip FFmpeg filtergraph & concat export plan generation.
#[derive(Debug, Clone, PartialEq)]
pub struct MultiClipExportOptions {
    /// Global geometry transforms (crop, 90° rotation, horizontal flip).
    pub geometry: PhotoModifications,
    /// Optional shorter-side quality cap (`0` = keep original resolution).
    pub quality: i32,
    /// Optional output frame rate cap (`0.0` = source maximum FPS).
    pub fps_limit: f64,
    /// Export as silent looping GIF (strips audio).
    pub gif: bool,
    /// Force-strip audio tracks.
    pub remove_audio: bool,
    /// Target audio sample rate in Hz (default `EXPORT_AUDIO_SAMPLE_RATE`).
    pub audio_sample_rate: u32,
    /// Optional explicit output canvas dimensions (rounded down to even).
    pub target_dimensions: Option<Size>,
}

impl Default for MultiClipExportOptions {
    fn default() -> Self {
        Self {
            geometry: PhotoModifications::default(),
            quality: 0,
            fps_limit: 0.0,
            gif: false,
            remove_audio: false,
            audio_sample_rate: EXPORT_AUDIO_SAMPLE_RATE,
            target_dimensions: None,
        }
    }
}

/// One clip input specification inside a [`MultiClipExportPlan`].
#[derive(Debug, Clone, PartialEq)]
pub struct MultiClipInputSpec {
    /// Zero-based FFmpeg `-i` input index.
    pub input_index: usize,
    /// Identifier of the source clip in the timeline.
    pub clip_id: ClipId,
    /// Optional path on disk for `-i`.
    pub path: Option<PathBuf>,
    /// Effective trim start inside this clip (ms).
    pub from_ms: i64,
    /// Effective trim end inside this clip (ms).
    pub till_ms: i64,
    /// Active duration contributed to the concatenated output (ms).
    pub duration_ms: i64,
    /// Whether this clip contributes real source audio (vs synthesized silence).
    pub has_audio: bool,
    /// Effective audio volume multiplier (`0.0` when muted or silent).
    pub effective_volume: f64,
    /// Filtergraph video pad label (e.g. `"v0"`).
    pub video_label: String,
    /// Filtergraph audio pad label (e.g. `"a0"`) when output contains audio.
    pub audio_label: Option<String>,
}

/// Scheduled overlay item metadata included in a [`MultiClipExportPlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayFilterSpec {
    /// Scene item identifier.
    pub item_id: ItemId,
    /// Human-readable item kind tag (`"text"`, `"sticker"`, `"shape"`, etc.).
    pub kind: String,
    /// Optional FFmpeg `enable='between(t,start,end)'` expression when visibility is bounded.
    pub enable_expr: Option<String>,
}

/// Complete, deterministic FFmpeg multi-clip filtergraph & concat export plan.
#[derive(Debug, Clone, PartialEq)]
pub struct MultiClipExportPlan {
    /// Per-clip input specs in playback order.
    pub inputs: Vec<MultiClipInputSpec>,
    /// Final even-aligned output dimensions.
    pub output_dimensions: Size,
    /// Target output frame rate.
    pub output_fps: f64,
    /// Total concatenated output duration in milliseconds.
    pub total_duration_ms: i64,
    /// Whether the concatenated output contains an audio stream.
    pub has_audio: bool,
    /// Target H.264 video bitrate in bits per second.
    pub video_bitrate: i64,
    /// Full `-filter_complex` FFmpeg filtergraph string.
    pub filter_complex: String,
    /// Output video map label (`"[vout]"`).
    pub video_map_label: String,
    /// Output audio map label (`Some("[aout]")`) when `has_audio` is `true`.
    pub audio_map_label: Option<String>,
    /// Ordered overlay visibility filter specs derived from the scene & visibility track.
    pub overlays: Vec<OverlayFilterSpec>,
    /// Complete ordered FFmpeg CLI argument vector ready for execution.
    pub ffmpeg_args: Vec<String>,
}

/// Generates a deterministic multi-clip FFmpeg `-filter_complex` + `concat` export plan
/// from `timeline`, optional `(scene, overlay_visibility)`, and `options`.
pub fn build_multi_clip_export_plan(
    timeline: &MultiClipTimeline,
    overlays: Option<(&PaintScene, &OverlayVisibilityTrack)>,
    options: &MultiClipExportOptions,
    output_path: &Path,
) -> Result<MultiClipExportPlan, EditorError> {
    timeline.validate()?;
    let active_clips: Vec<_> = timeline
        .clips()
        .iter()
        .filter(|c| c.loop_duration_ms() > 0)
        .collect();
    if active_clips.is_empty() {
        return Err(EditorError::VideoExport(
            "cannot build multi-clip export plan for an empty timeline".to_owned(),
        ));
    }

    let base_dim = options.target_dimensions.unwrap_or_else(|| {
        active_clips
            .iter()
            .map(|c| c.source().dimensions)
            .find(|d| !d.is_empty())
            .unwrap_or(Size::new(1280, 720))
    });
    let edited_dim = edited_frame_size(base_dim, &options.geometry);
    let output_dimensions = if options.quality > 0 {
        downscaled_size(edited_dim, options.quality).unwrap_or_else(|| even_size(edited_dim))
    } else {
        even_size(edited_dim)
    };
    if output_dimensions.is_empty() {
        return Err(EditorError::VideoExport(
            "computed output dimensions are empty".to_owned(),
        ));
    }

    let max_src_fps = active_clips
        .iter()
        .map(|c| sanitized_fps(c.source().fps))
        .fold(30.0f64, f64::max);
    let output_fps = if options.fps_limit > 1.0 && max_src_fps > options.fps_limit {
        sanitized_fps(options.fps_limit)
    } else {
        max_src_fps
    };

    let strip_audio = options.gif || options.remove_audio;
    let has_audio = !strip_audio && active_clips.iter().any(|c| c.sounding());
    let sample_rate = options.audio_sample_rate.max(8_000);
    let total_duration_ms = timeline.total_duration_ms();
    let video_bitrate = target_video_bitrate(output_dimensions, output_fps);

    let geom_filters = build_geometry_filters(&options.geometry);
    let mut filter_parts = Vec::with_capacity(active_clips.len() * 2 + 1);
    let mut inputs = Vec::with_capacity(active_clips.len());
    let mut concat_pads = String::new();

    for (idx, clip) in active_clips.iter().enumerate() {
        let from_ms = clip.effective_from_ms();
        let till_ms = clip.effective_till_ms();
        let dur_ms = (till_ms - from_ms).max(1);
        let from_s = format_seconds(from_ms);
        let till_s = format_seconds(till_ms);
        let dur_s = format_seconds(dur_ms);

        let v_label = format!("v{idx}");
        let mut v_chain = format!("[{idx}:v]trim=start={from_s}:end={till_s},setpts=PTS-STARTPTS");
        if !geom_filters.is_empty() {
            v_chain.push(',');
            v_chain.push_str(&geom_filters);
        }
        v_chain.push_str(&format!(
            ",scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps={fps:.3}[{v_label}]",
            w = output_dimensions.width,
            h = output_dimensions.height,
            fps = output_fps,
        ));
        filter_parts.push(v_chain);
        concat_pads.push_str(&format!("[{v_label}]"));

        let audio_label = if has_audio {
            let a_label = format!("a{idx}");
            if clip.sounding() {
                let vol = clip.effective_volume();
                filter_parts.push(format!(
                    "[{idx}:a]atrim=start={from_s}:end={till_s},asetpts=PTS-STARTPTS,volume={vol:.3},aresample={sample_rate},aformat=sample_fmts=fltp:channel_layouts=stereo[{a_label}]"
                ));
            } else {
                // Synthesize matching silence so mixed audio/muted clips concatenate cleanly.
                filter_parts.push(format!(
                    "anullsrc=channel_layout=stereo:sample_rate={sample_rate},atrim=duration={dur_s},asetpts=PTS-STARTPTS[{a_label}]"
                ));
            }
            concat_pads.push_str(&format!("[{a_label}]"));
            Some(a_label)
        } else {
            None
        };

        inputs.push(MultiClipInputSpec {
            input_index: idx,
            clip_id: clip.id(),
            path: clip.source().path.clone(),
            from_ms,
            till_ms,
            duration_ms: dur_ms,
            has_audio: clip.sounding(),
            effective_volume: clip.effective_volume(),
            video_label: v_label,
            audio_label,
        });
    }

    let video_map_label = "[vout]".to_owned();
    let audio_map_label = if has_audio {
        Some("[aout]".to_owned())
    } else {
        None
    };

    let n = active_clips.len();
    if has_audio {
        filter_parts.push(format!("{concat_pads}concat=n={n}:v=1:a=1[vout][aout]"));
    } else {
        filter_parts.push(format!("{concat_pads}concat=n={n}:v=1:a=0[vout]"));
    }

    let filter_complex = filter_parts.join(";");

    let overlay_specs = match overlays {
        Some((scene, track)) => scene
            .active_items_by_z()
            .into_iter()
            .map(|entry| {
                let enable_expr = track.range_for(entry.id).map(|r| {
                    format!(
                        "between(t,{},{})",
                        format_seconds(r.start_ms),
                        format_seconds(r.end_ms)
                    )
                });
                OverlayFilterSpec {
                    item_id: entry.id,
                    kind: scene_kind_name(&entry.item).to_owned(),
                    enable_expr,
                }
            })
            .collect(),
        None => Vec::new(),
    };

    let mut ffmpeg_args = vec!["-y".to_owned()];
    for input in &inputs {
        ffmpeg_args.push("-i".to_owned());
        let path_str = input
            .path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("clip_{}.mp4", input.clip_id.0));
        ffmpeg_args.push(path_str);
    }
    ffmpeg_args.push("-filter_complex".to_owned());
    ffmpeg_args.push(filter_complex.clone());
    ffmpeg_args.push("-map".to_owned());
    ffmpeg_args.push(video_map_label.clone());
    if let Some(ref a_map) = audio_map_label {
        ffmpeg_args.push("-map".to_owned());
        ffmpeg_args.push(a_map.clone());
    }
    ffmpeg_args.extend([
        "-c:v".to_owned(),
        "libx264".to_owned(),
        "-pix_fmt".to_owned(),
        "yuv420p".to_owned(),
        "-b:v".to_owned(),
        video_bitrate.to_string(),
    ]);
    if has_audio {
        ffmpeg_args.extend([
            "-c:a".to_owned(),
            "aac".to_owned(),
            "-ar".to_owned(),
            sample_rate.to_string(),
        ]);
    } else {
        ffmpeg_args.push("-an".to_owned());
    }
    ffmpeg_args.extend([
        "-movflags".to_owned(),
        "+faststart".to_owned(),
        output_path.to_string_lossy().into_owned(),
    ]);

    Ok(MultiClipExportPlan {
        inputs,
        output_dimensions,
        output_fps,
        total_duration_ms,
        has_audio,
        video_bitrate,
        filter_complex,
        video_map_label,
        audio_map_label,
        overlays: overlay_specs,
        ffmpeg_args,
    })
}

fn format_seconds(ms: i64) -> String {
    let clamped = ms.max(0);
    format!("{}.{:03}", clamped / 1000, clamped % 1000)
}

fn build_geometry_filters(geometry: &PhotoModifications) -> String {
    let mut parts = Vec::new();
    if let Some(crop) = geometry.crop
        && crop.is_valid()
    {
        parts.push(format!(
            "crop={}:{}:{}:{}",
            crop.width,
            crop.height,
            crop.x.max(0),
            crop.y.max(0)
        ));
    }
    match normalize_angle_90(geometry.angle) {
        90 => parts.push("transpose=1".to_owned()),
        180 => {
            parts.push("hflip".to_owned());
            parts.push("vflip".to_owned());
        }
        270 => parts.push("transpose=2".to_owned()),
        _ => {}
    }
    if geometry.flipped {
        parts.push("hflip".to_owned());
    }
    parts.join(",")
}

fn scene_kind_name(kind: &SceneItemKind) -> &'static str {
    match kind {
        SceneItemKind::Line(_) => "line",
        SceneItemKind::Sticker(_) => "sticker",
        SceneItemKind::Text(_) => "text",
        SceneItemKind::Shape(_) => "shape",
        SceneItemKind::Location(_) => "location",
        SceneItemKind::Weather(_) => "weather",
        SceneItemKind::StoryLink(_) => "story_link",
        SceneItemKind::StoryReaction(_) => "story_reaction",
        SceneItemKind::StoryChannelPost(_) => "story_channel_post",
        SceneItemKind::Eraser(_) => "eraser",
        SceneItemKind::Placement(_) => "placement",
        SceneItemKind::ZOrder(_) => "z_order",
        SceneItemKind::Remove(_) => "remove",
    }
}
