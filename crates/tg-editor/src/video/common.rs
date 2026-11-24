//! Common video editor types, resolution math, edit detection, and output size estimation.
//!
//! Port of `Telegram/SourceFiles/editor/video/video_editor_common.{h,cpp}` and
//! sizing/estimation helpers from `Telegram/SourceFiles/media/media_video_encode.cpp`.

use serde::{Deserialize, Serialize};

use crate::common::{EditorData, PhotoModifications};
use crate::geometry::{Rect, Size, flip_size_by_rotation, normalize_angle_90};

/// Maximum source file size (10 MiB) allowed for "Send as GIF" toggle (`kMaxGifSize`).
pub const MAX_GIF_SIZE_BYTES: u64 = 10 * 1024 * 1024;

/// Minimum auto-computed video bitrate in bits per second (`kMinBitrate`).
pub const MIN_VIDEO_BITRATE: i64 = 600_000;

/// Maximum auto-computed video bitrate in bits per second (`kMaxBitrate`).
pub const MAX_VIDEO_BITRATE: i64 = 6_800_000;

/// Target audio bitrate per channel in bits per second (`kAudioBitratePerChannel`).
pub const AUDIO_BITRATE_PER_CHANNEL: i64 = 64_000;

/// Estimated bitrate of an encoded silent AAC track (`kSilentAudioBitrate`).
pub const SILENT_AUDIO_BITRATE: i64 = 7_000;

/// Audio sample rate for exported MP4 AAC tracks (`kAudioFrequency`).
pub const EXPORT_AUDIO_SAMPLE_RATE: u32 = 48_000;

const RATE_CONTROL_FPS: f64 = 60.0;
const CONTAINER_OVERHEAD_BYTES: f64 = 2_000.0;
const CONTAINER_BYTES_PER_VIDEO_FRAME: f64 = 12.0;
const CONTAINER_BYTES_PER_AUDIO_FRAME: f64 = 10.0;
const AUDIO_FRAME_SAMPLES: f64 = 1_024.0;

/// Audio track metadata and mix controls for a video clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoAudioTrack {
    /// Whether the source video contains an audio stream.
    pub has_audio: bool,
    /// Number of audio channels in the source stream (`0` if none).
    pub channels: u16,
    /// Sample rate in Hz (`0` if none).
    pub sample_rate: u32,
    /// Source audio bitrate in bps (`0` if unknown).
    pub bitrate: i64,
    /// Codec name (e.g. `"aac"`, `"opus"`).
    pub codec_name: String,
    /// True if audio is muted in the editor.
    pub muted: bool,
    /// Audio track volume multiplier in `0.0..=1.0` (matches `VideoClip::volume`).
    pub volume: f32,
}

impl Default for VideoAudioTrack {
    fn default() -> Self {
        Self {
            has_audio: false,
            channels: 0,
            sample_rate: 0,
            bitrate: 0,
            codec_name: String::new(),
            muted: false,
            volume: 1.0,
        }
    }
}

impl VideoAudioTrack {
    /// Returns `true` when the exported clip should contain audible audio.
    #[must_use]
    pub fn is_sounding(&self) -> bool {
        self.has_audio && !self.muted && self.volume > 0.001
    }

    /// Clamps and sets the audio track volume in `0.0..=1.0`.
    pub fn set_volume(&mut self, volume: f32) {
        let clamped = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.volume = clamped;
        self.muted = clamped <= 0.001;
    }

    /// Enables or mutes audio playback/export on this track.
    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        if !muted && self.volume <= 0.001 {
            self.volume = 1.0;
        }
    }
}

/// Non-destructive video editing parameters (`Editor::VideoModifications`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoModifications {
    /// Geometry modifications (crop rect, 90-degree rotation, horizontal flip).
    pub geometry: PhotoModifications,
    /// Trim start timestamp in milliseconds.
    pub from_ms: i64,
    /// Trim end timestamp in milliseconds (`0` means end of clip).
    pub till_ms: i64,
    /// Selected cover-frame timestamp in milliseconds.
    pub cover_ms: i64,
    /// Target shorter side in pixels (`0` means keep original resolution).
    pub quality: i32,
    /// Send as silent looping GIF (`gif = true` strips audio).
    pub gif: bool,
    /// Explicit mute toggle (strips audio without forcing GIF mode).
    pub muted: bool,
    /// Audio track gain multiplier in `0.0..=1.0`.
    pub audio_volume: f32,
}

impl Default for VideoModifications {
    fn default() -> Self {
        Self {
            geometry: PhotoModifications::default(),
            from_ms: 0,
            till_ms: 0,
            cover_ms: 0,
            quality: 0,
            gif: false,
            muted: false,
            audio_volume: 1.0,
        }
    }
}

impl VideoModifications {
    /// Returns `true` if audio should be removed during export.
    #[must_use]
    pub fn strips_audio(&self, data: &VideoEditorData) -> bool {
        data.remove_audio || self.gif || self.muted || self.audio_volume <= 0.001
    }
}

/// Configuration and constraints for a video editor session (`Editor::VideoEditorData`).
#[derive(Debug, Clone, PartialEq)]
pub struct VideoEditorData {
    /// Shared photo/crop editor flags.
    pub editor: EditorData,
    /// Optional maximum allowed trimmed duration in milliseconds (`0` = unlimited).
    pub max_duration_ms: i64,
    /// Optional minimum allowed trimmed duration in milliseconds (`0` = default 100ms).
    pub min_duration_ms: i64,
    /// Optional output frame rate cap (`0.0` = source FPS).
    pub fps_limit: f64,
    /// Force-strip audio track regardless of user mute/GIF toggles.
    pub remove_audio: bool,
    /// Allow quality/resolution downscaling selection.
    pub allow_quality: bool,
    /// Allow toggling "Send as GIF" (when file size <= `MAX_GIF_SIZE_BYTES`).
    pub allow_gif: bool,
    /// Always re-encode even when no modifications were made.
    pub transcode_always: bool,
}

impl Default for VideoEditorData {
    fn default() -> Self {
        Self {
            editor: EditorData::default(),
            max_duration_ms: 0,
            min_duration_ms: 0,
            fps_limit: 0.0,
            remove_audio: false,
            allow_quality: true,
            allow_gif: true,
            transcode_always: false,
        }
    }
}

/// Probed metadata of the source video file (`Media::Encode::SourceInfo`).
#[derive(Debug, Clone, PartialEq)]
pub struct VideoSourceInfo {
    /// Display-oriented dimensions (after file rotation metadata).
    pub dimensions: Size,
    /// Total duration in milliseconds.
    pub duration_ms: i64,
    /// Source frame rate in FPS.
    pub fps: f64,
    /// Source file size in bytes.
    pub file_size: u64,
    /// Estimated source video bitrate in bps (`0` if unknown).
    pub video_bitrate: i64,
    /// Source audio track descriptor.
    pub audio: VideoAudioTrack,
    /// Video codec name (e.g. `"h264"`).
    pub video_codec: String,
    /// Container display rotation in degrees (`0`, `90`, `180`, `270`).
    pub rotation: i32,
}

impl Default for VideoSourceInfo {
    fn default() -> Self {
        Self {
            dimensions: Size::new(0, 0),
            duration_ms: 0,
            fps: 30.0,
            file_size: 0,
            video_bitrate: 0,
            audio: VideoAudioTrack::default(),
            video_codec: String::new(),
            rotation: 0,
        }
    }
}

/// Rounds a positive dimension down to an even integer (minimum `2`), matching `EvenDown`.
#[must_use]
pub const fn even_down(value: i32) -> i32 {
    let e = value & !1;
    if e < 2 { 2 } else { e }
}

/// Rounds both sides of `size` down to even integers (`>= 2`).
#[must_use]
pub const fn even_size(size: Size) -> Size {
    if size.width <= 0 || size.height <= 0 {
        Size::new(0, 0)
    } else {
        Size::new(even_down(size.width), even_down(size.height))
    }
}

/// Sanitizes an arbitrary FPS value into `(1.0, 121.0)` with fallback `30.0` (`SanitizedFps`).
#[must_use]
pub fn sanitized_fps(fps: f64) -> f64 {
    if fps.is_finite() && fps > 1.0 && fps < 121.0 {
        fps
    } else {
        30.0
    }
}

/// Computes the post-crop, post-rotation frame dimensions (`Editor::EditedFrameSize`).
#[must_use]
pub fn edited_frame_size(dimensions: Size, geometry: &PhotoModifications) -> Size {
    if dimensions.is_empty() {
        return Size::new(0, 0);
    }
    let full = Rect::new(0, 0, dimensions.width, dimensions.height);
    let cropped = match geometry.crop {
        Some(c) if !c.is_empty() => {
            let x1 = c.x.clamp(0, full.width);
            let y1 = c.y.clamp(0, full.height);
            let x2 = c.right_edge().clamp(0, full.width);
            let y2 = c.bottom_edge().clamp(0, full.height);
            let w = (x2 - x1).max(0);
            let h = (y2 - y1).max(0);
            if w > 0 && h > 0 {
                Size::new(w, h)
            } else {
                dimensions
            }
        }
        _ => dimensions,
    };
    flip_size_by_rotation(cropped, normalize_angle_90(geometry.angle))
}

/// Computes even downscaled dimensions capping the shorter side to `target_shorter_side`
/// (`Media::Encode::DownscaledSize`). Returns `None` if already `<= target_shorter_side`.
#[must_use]
pub fn downscaled_size(original: Size, target_shorter_side: i32) -> Option<Size> {
    if original.width <= 0 || original.height <= 0 || target_shorter_side <= 0 {
        return None;
    }
    let shorter = original.width.min(original.height);
    if shorter <= target_shorter_side {
        return None;
    }
    let scale = f64::from(target_shorter_side) / f64::from(shorter);
    Some(Size::new(
        even_down((f64::from(original.width) * scale).round() as i32),
        even_down((f64::from(original.height) * scale).round() as i32),
    ))
}

/// Computes the final output video dimensions for `mods` applied to `dimensions`
/// (`Media::Encode::TranscodedSize`).
#[must_use]
pub fn transcoded_size(dimensions: Size, mods: &VideoModifications) -> Size {
    let edited = edited_frame_size(dimensions, &mods.geometry);
    if edited.is_empty() {
        return Size::new(0, 0);
    }
    if mods.quality > 0
        && let Some(down) = downscaled_size(edited, mods.quality)
    {
        return down;
    }
    even_size(edited)
}

/// Computes the trimmed output duration in milliseconds (`Media::Encode::TranscodedDuration`).
#[must_use]
pub fn transcoded_duration_ms(from_ms: i64, till_ms: i64, total_duration_ms: i64) -> i64 {
    let total = total_duration_ms.max(0);
    let from = from_ms.clamp(0, total);
    let till = if till_ms > from {
        till_ms.min(total)
    } else {
        total
    };
    (till - from).max(0)
}

/// Returns `true` if any video modification differs from the original clip (`Editor::VideoEdited`).
#[must_use]
pub fn video_edited(
    mods: &VideoModifications,
    dimensions: Size,
    duration_ms: i64,
    has_audio: bool,
) -> bool {
    let full = Rect::new(0, 0, dimensions.width, dimensions.height);
    let audio_changed = if mods.gif || mods.muted {
        has_audio
    } else {
        has_audio && (mods.audio_volume - 1.0).abs() > 0.01
    };
    mods.quality > 0
        || audio_changed
        || normalize_angle_90(mods.geometry.angle) != 0
        || mods.geometry.flipped
        || mods
            .geometry
            .crop
            .is_some_and(|c| !c.is_empty() && c != full)
        || mods.from_ms > 0
        || (mods.till_ms > 0 && mods.till_ms < duration_ms)
}

/// Computes the target H.264 video bitrate in bps (`TargetBitrate` in `media_video_encode.cpp`).
#[must_use]
pub fn target_video_bitrate(size: Size, fps: f64) -> i64 {
    let pixels = i64::from(size.width.max(0)) * i64::from(size.height.max(0));
    let bits = (pixels as f64) * sanitized_fps(fps) * 0.07;
    bits.clamp(MIN_VIDEO_BITRATE as f64, MAX_VIDEO_BITRATE as f64) as i64
}

/// Estimates the exported MP4 byte size (`Media::Encode::EstimateTranscodedSize`).
#[must_use]
pub fn estimate_transcoded_size(
    mods: &VideoModifications,
    data: &VideoEditorData,
    info: &VideoSourceInfo,
) -> u64 {
    if info.dimensions.is_empty() {
        return 0;
    }
    let target = transcoded_size(info.dimensions, mods);
    if target.is_empty() {
        return 0;
    }
    let span_ms = transcoded_duration_ms(mods.from_ms, mods.till_ms, info.duration_ms);
    if span_ms <= 0 {
        return 0;
    }

    let transcoded = data.transcode_always
        || video_edited(
            mods,
            info.dimensions,
            info.duration_ms,
            info.audio.has_audio,
        );
    if !transcoded && info.file_size > 0 {
        return info.file_size;
    }

    let limiting = data.fps_limit > 0.0 && info.fps > data.fps_limit * 1.05;
    let fps = sanitized_fps(if limiting { data.fps_limit } else { info.fps });
    let seconds = span_ms as f64 / 1000.0;

    let wanted_video = target_video_bitrate(target, fps);
    let clamped_video = if info.video_bitrate > 0 {
        wanted_video.min(info.video_bitrate)
    } else {
        wanted_video
    };
    let video_bitrate = (clamped_video as f64) * (RATE_CONTROL_FPS / fps).min(1.0);

    let strip_audio = mods.strips_audio(data);
    let audio_bitrate = if strip_audio || !info.audio.has_audio {
        0.0
    } else if info.audio.bitrate > 0 {
        info.audio.bitrate as f64
    } else {
        (AUDIO_BITRATE_PER_CHANNEL * i64::from(info.audio.channels.clamp(1, 2))) as f64
    };

    let container = CONTAINER_OVERHEAD_BYTES
        + CONTAINER_BYTES_PER_VIDEO_FRAME * seconds * fps
        + if audio_bitrate > 0.0 {
            CONTAINER_BYTES_PER_AUDIO_FRAME * seconds * f64::from(EXPORT_AUDIO_SAMPLE_RATE)
                / AUDIO_FRAME_SAMPLES
        } else {
            0.0
        };
    let bytes = ((video_bitrate + audio_bitrate) * seconds / 8.0 + container).round();
    (bytes.max(1.0)) as u64
}
