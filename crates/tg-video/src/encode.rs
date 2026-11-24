//! FFmpeg video transcoding, frame extraction, and synthetic clip generation.
//!
//! Port of `Telegram/SourceFiles/media/media_video_encode.cpp` and
//! `Telegram/SourceFiles/media/media_video_frames.cpp`.

use std::ffi::{CStr, CString, c_char, c_int};
use std::path::Path;

use image::RgbaImage;

use crate::error::{Result, VideoError};
use crate::ffmpeg::{DecodedMediaFrame, FfmpegMedia, FfmpegOpenOptions, ffmpeg_versions, sys};
use crate::types::{FrameData, MAX_FRAME_AREA, OutputPixelFormat, Size};

/// Configuration for transcoding / exporting a video via FFmpeg.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoEncodeConfig {
    /// Trim start in milliseconds (`>= 0`).
    pub from_ms: i64,
    /// Trim end in milliseconds (`<= 0` means end of source).
    pub till_ms: i64,
    /// Target output width in pixels (rounded down to even; `0` keeps edited width).
    pub target_width: i32,
    /// Target output height in pixels (rounded down to even; `0` keeps edited height).
    pub target_height: i32,
    /// Optional crop X coordinate in display pixels.
    pub crop_x: i32,
    /// Optional crop Y coordinate in display pixels.
    pub crop_y: i32,
    /// Optional crop width (`0` means no crop).
    pub crop_w: i32,
    /// Optional crop height (`0` means no crop).
    pub crop_h: i32,
    /// Additional clockwise rotation in degrees (`0`, `90`, `180`, `270`).
    pub rotation: i32,
    /// Horizontal flip toggle.
    pub flipped: bool,
    /// Strip audio track (muted or GIF export).
    pub remove_audio: bool,
    /// Audio track gain multiplier (`0.0` = mute, `1.0` = unity, up to `2.0`).
    pub audio_volume: f32,
    /// Optional maximum frame rate (`0.0` keeps source FPS).
    pub fps_limit: f64,
    /// Target video bitrate in bits per second (`0` auto-computes).
    pub video_bitrate: i64,
}

impl Default for VideoEncodeConfig {
    fn default() -> Self {
        Self {
            from_ms: 0,
            till_ms: 0,
            target_width: 0,
            target_height: 0,
            crop_x: 0,
            crop_y: 0,
            crop_w: 0,
            crop_h: 0,
            rotation: 0,
            flipped: false,
            remove_audio: false,
            audio_volume: 1.0,
            fps_limit: 0.0,
            video_bitrate: 0,
        }
    }
}

/// Summary of an exported MP4 file produced by [`transcode_video_file`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoEncodeResult {
    /// Final encoded video width in pixels.
    pub output_width: u32,
    /// Final encoded video height in pixels.
    pub output_height: u32,
    /// Exported clip duration in milliseconds.
    pub duration_ms: i64,
    /// Whether the exported MP4 contains an audio track.
    pub has_audio: bool,
    /// Final output file size in bytes.
    pub file_size: u64,
}

/// Returns `true` when the linked FFmpeg shared libraries are loaded and report valid versions.
#[must_use]
pub fn ffmpeg_runtime_available() -> bool {
    let v = ffmpeg_versions();
    v.avformat > 0 && v.avcodec > 0 && v.avutil > 0 && v.swscale > 0 && v.swresample > 0
}

fn path_to_cstring(path: &Path) -> Result<CString> {
    let s = path.to_str().ok_or_else(|| {
        VideoError::InvalidArgument(format!("non-UTF-8 path: {}", path.display()))
    })?;
    CString::new(s).map_err(|_| {
        VideoError::InvalidArgument(format!("path contains NUL byte: {}", path.display()))
    })
}

fn extract_err_string(buf: &[c_char]) -> String {
    // SAFETY: `buf` is NUL-terminated before calling this helper.
    unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

/// Transcodes, trims, scales, rotates/flips/crops, and mutes/adjusts audio of `input_path`
/// into a faststart MP4 written to `output_path` using the FFmpeg C shim.
pub fn transcode_video_file(
    input_path: &Path,
    output_path: &Path,
    config: &VideoEncodeConfig,
) -> Result<VideoEncodeResult> {
    let in_c = path_to_cstring(input_path)?;
    let out_c = path_to_cstring(output_path)?;
    let raw_cfg = sys::TgVideoEncodeConfig {
        from_ms: config.from_ms.max(0),
        till_ms: config.till_ms.max(0),
        target_width: config.target_width as c_int,
        target_height: config.target_height as c_int,
        crop_x: config.crop_x as c_int,
        crop_y: config.crop_y as c_int,
        crop_w: config.crop_w as c_int,
        crop_h: config.crop_h as c_int,
        rotation: config.rotation as c_int,
        flipped: c_int::from(config.flipped),
        remove_audio: c_int::from(config.remove_audio),
        audio_volume: config.audio_volume,
        fps_limit: config.fps_limit,
        video_bitrate: config.video_bitrate,
    };
    let mut raw_res = sys::TgVideoEncodeResult::default();
    let mut err_buf = [0 as c_char; 256];

    // SAFETY: All pointers are valid C strings or stack-allocated structs/buffers for the duration
    // of the synchronous FFI call.
    let rc = unsafe {
        sys::tg_video_transcode_file(
            in_c.as_ptr(),
            out_c.as_ptr(),
            &raw_cfg,
            &mut raw_res,
            err_buf.as_mut_ptr(),
            err_buf.len(),
        )
    };
    if rc < 0 {
        *err_buf.last_mut().unwrap_or(&mut 0) = 0;
        let msg = extract_err_string(&err_buf);
        return Err(VideoError::Ffmpeg {
            context: "tg_video_transcode_file",
            code: rc,
            message: if msg.is_empty() {
                crate::ffmpeg::av_error_string(rc)
            } else {
                msg
            },
        });
    }

    Ok(VideoEncodeResult {
        output_width: raw_res.output_width.max(0) as u32,
        output_height: raw_res.output_height.max(0) as u32,
        duration_ms: raw_res.duration_ms.max(0),
        has_audio: raw_res.has_audio != 0,
        file_size: raw_res.file_size.max(0) as u64,
    })
}

/// Generates a tiny synthetic MP4 clip (moving color-bars video + optional 440Hz sine audio)
/// directly through FFmpeg into `output_path`. Useful for self-contained export tests.
pub fn generate_test_clip(
    output_path: &Path,
    width: u32,
    height: u32,
    fps: u32,
    duration_ms: u32,
    with_audio: bool,
) -> Result<()> {
    let out_c = path_to_cstring(output_path)?;
    let mut err_buf = [0 as c_char; 256];
    // SAFETY: `out_c` is a valid NUL-terminated path and `err_buf` is a writable stack buffer.
    let rc = unsafe {
        sys::tg_video_generate_test_clip(
            out_c.as_ptr(),
            width as c_int,
            height as c_int,
            fps as c_int,
            duration_ms as c_int,
            c_int::from(with_audio),
            err_buf.as_mut_ptr(),
            err_buf.len(),
        )
    };
    if rc < 0 {
        *err_buf.last_mut().unwrap_or(&mut 0) = 0;
        let msg = extract_err_string(&err_buf);
        return Err(VideoError::Ffmpeg {
            context: "tg_video_generate_test_clip",
            code: rc,
            message: if msg.is_empty() {
                crate::ffmpeg::av_error_string(rc)
            } else {
                msg
            },
        });
    }
    Ok(())
}

/// Scales `src` to fit inside `max_box` while preserving aspect ratio (minimum `2x2`).
fn fit_size_in_box(src: Size, max_box: Size) -> Size {
    if src.is_empty() || max_box.is_empty() {
        return src;
    }
    let scale = (f64::from(max_box.width) / f64::from(src.width))
        .min(f64::from(max_box.height) / f64::from(src.height))
        .min(1.0);
    Size::new(
        ((f64::from(src.width) * scale).round() as u32).max(2),
        ((f64::from(src.height) * scale).round() as u32).max(2),
    )
}

fn info_display_size(info: &crate::ffmpeg::MediaStreamInfo) -> Size {
    let raw = Size::new(info.video_width, info.video_height);
    if info.video_rotation.swaps_width_height() {
        raw.transposed()
    } else {
        raw
    }
}

fn decode_frame_near(
    media: &mut FfmpegMedia,
    time_ms: i64,
    resize: Option<Size>,
) -> Result<Option<RgbaImage>> {
    let target_ms = time_ms.max(0);
    if target_ms > 0 {
        let _ = media.seek_ms(target_ms, true);
    } else {
        let _ = media.seek_to_start();
    }

    let mut fallback: Option<(u32, u32, Vec<u8>)> = None;
    for _ in 0..240 {
        let Some(DecodedMediaFrame::Video(frame)) =
            media.decode_next(true, false, OutputPixelFormat::Rgba, resize, true, 0, 0)?
        else {
            break;
        };
        let FrameData::Rgba(pixels) = frame.data else {
            continue;
        };
        let pts = frame.pts_ms;
        if pts >= target_ms || target_ms == 0 {
            return Ok(RgbaImage::from_raw(frame.width, frame.height, pixels));
        }
        fallback = Some((frame.width, frame.height, pixels));
    }

    Ok(fallback.and_then(|(w, h, px)| RgbaImage::from_raw(w, h, px)))
}

/// Extracts a single RGBA frame nearest to `time_ms` from `path`, optionally downscaled to fit
/// within `max_box`.
pub fn extract_frame_at_ms(path: &Path, time_ms: i64, max_box: Option<Size>) -> Result<RgbaImage> {
    let mut media = FfmpegMedia::open_file(
        path,
        FfmpegOpenOptions {
            seekable: true,
            ignore_editlist: false,
            hw_allowed: false,
            open_video: true,
            open_audio: false,
            video_max_area: MAX_FRAME_AREA,
            thread_count: 1,
        },
    )?;
    let display = info_display_size(media.info());
    let resize = max_box.map(|b| fit_size_in_box(display, b));
    decode_frame_near(&mut media, time_ms, resize)?
        .ok_or_else(|| VideoError::InvalidArgument("no decodable video frame found".to_owned()))
}

/// Extracts RGBA thumbnail frames at each timestamp in `positions_ms` using a single demuxer.
pub fn extract_timeline_frames(
    path: &Path,
    positions_ms: &[i64],
    thumb_box: Size,
) -> Result<Vec<RgbaImage>> {
    if positions_ms.is_empty() {
        return Ok(Vec::new());
    }
    let mut media = FfmpegMedia::open_file(
        path,
        FfmpegOpenOptions {
            seekable: true,
            ignore_editlist: false,
            hw_allowed: false,
            open_video: true,
            open_audio: false,
            video_max_area: MAX_FRAME_AREA,
            thread_count: 1,
        },
    )?;
    let display = info_display_size(media.info());
    let resize = (!thumb_box.is_empty()).then(|| fit_size_in_box(display, thumb_box));
    let mut frames = Vec::with_capacity(positions_ms.len());
    for &pos in positions_ms {
        if let Some(img) = decode_frame_near(&mut media, pos, resize)? {
            frames.push(img);
        }
    }
    Ok(frames)
}
