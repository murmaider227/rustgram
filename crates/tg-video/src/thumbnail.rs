//! Video thumbnail extraction and encoding.
//!
//! Port of cover/thumbnail extraction from
//! `Telegram/SourceFiles/media/streaming/media_streaming_document.cpp`
//! (`Document::validateGoodThumbnail`) and `media_streaming_utility.cpp`.

use std::io::Cursor;
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::{ImageFormat, RgbaImage};

use crate::clip::apply_circle_mask_rgba;
use crate::error::{Result, StreamingError, VideoError};
use crate::ffmpeg::{DecodedMediaFrame, FfmpegMedia, FfmpegOpenOptions, correct_by_aspect};
use crate::types::{FrameData, MAX_FRAME_AREA, OutputPixelFormat, Size, VideoFrame};

/// JPEG thumbnail encoding quality matching tdesktop (`kGoodThumbQuality` = 87).
pub const GOOD_THUMB_QUALITY: u8 = 87;

/// Options controlling video thumbnail extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ThumbnailOptions {
    /// Timestamp in milliseconds to seek to (`0` = first frame).
    pub position_ms: i64,
    /// Optional bounding box to fit the thumbnail into (preserving aspect ratio).
    pub max_size: Option<Size>,
    /// Keep alpha channel (true for video stickers; false composites over white).
    pub keep_alpha: bool,
    /// Apply circular mask (for round video notes).
    pub round_circle: bool,
}

/// Output image format for encoded thumbnails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailEncoding {
    /// PNG (lossless, preserves alpha).
    Png,
    /// JPEG with quality `GOOD_THUMB_QUALITY` (87, opaque).
    Jpeg,
}

/// Composites an RGBA image over an opaque white background in-place
/// (mirrors `Images::Opaque` for non-sticker video thumbnails).
pub fn flatten_alpha_over_white(rgba: &mut [u8]) {
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = u16::from(px[3]);
        if a < 255 {
            let inv = 255 - a;
            px[0] = ((u16::from(px[0]) * a + 255 * inv + 127) / 255) as u8;
            px[1] = ((u16::from(px[1]) * a + 255 * inv + 127) / 255) as u8;
            px[2] = ((u16::from(px[2]) * a + 255 * inv + 127) / 255) as u8;
            px[3] = 255;
        }
    }
}

fn extract_from_media(mut media: FfmpegMedia, options: ThumbnailOptions) -> Result<VideoFrame> {
    if !media.info().has_video {
        return Err(StreamingError::OpenFailed.into());
    }
    let info = media.info().clone();
    if options.position_ms > 0 {
        let _ = media.seek_ms(options.position_ms, true);
    }

    let raw_size = Size::new(info.video_width, info.video_height);
    let corrected = info
        .video_rotation
        .transpose_size(correct_by_aspect(raw_size, info.video_aspect));
    let target = match options.max_size {
        Some(bounds) if !bounds.is_empty() => Some(corrected.scaled_keep_aspect(bounds)),
        _ => (corrected != info.video_rotation.transpose_size(raw_size)).then_some(corrected),
    };

    let mut chosen: Option<VideoFrame> = None;
    loop {
        match media.decode_next(true, false, OutputPixelFormat::Rgba, target, true, 0, 0)? {
            Some(DecodedMediaFrame::Video(frame)) => {
                if frame.pts_ms >= options.position_ms {
                    chosen = Some(frame);
                    break;
                }
                chosen = Some(frame);
            }
            Some(DecodedMediaFrame::Audio(_)) => {}
            None => break,
        }
    }

    let mut frame = chosen.ok_or(StreamingError::InvalidData)?;
    if let FrameData::Rgba(ref mut pixels) = frame.data {
        if frame.has_alpha && !options.keep_alpha {
            flatten_alpha_over_white(pixels);
            frame.has_alpha = false;
        }
        if options.round_circle {
            apply_circle_mask_rgba(pixels, frame.width, frame.height);
            frame.has_alpha = true;
        }
    }
    Ok(frame)
}

/// Extracts a single video thumbnail frame from a local file path.
pub fn extract_thumbnail(path: &Path, options: ThumbnailOptions) -> Result<VideoFrame> {
    let media = FfmpegMedia::open_file(
        path,
        FfmpegOpenOptions {
            seekable: true,
            ignore_editlist: false,
            hw_allowed: false,
            open_video: true,
            open_audio: false,
            video_max_area: MAX_FRAME_AREA,
            thread_count: 0,
        },
    )?;
    extract_from_media(media, options)
}

/// Extracts a single video thumbnail frame from in-memory container bytes.
pub fn extract_thumbnail_from_bytes(
    bytes: Vec<u8>,
    options: ThumbnailOptions,
) -> Result<VideoFrame> {
    let media = FfmpegMedia::open_bytes(
        bytes,
        FfmpegOpenOptions {
            seekable: true,
            ignore_editlist: false,
            hw_allowed: false,
            open_video: true,
            open_audio: false,
            video_max_area: MAX_FRAME_AREA,
            thread_count: 0,
        },
    )?;
    extract_from_media(media, options)
}

/// Encodes an RGBA [`VideoFrame`] to PNG or JPEG bytes.
pub fn encode_thumbnail(frame: &VideoFrame, encoding: ThumbnailEncoding) -> Result<Vec<u8>> {
    let rgba = frame.data.as_rgba().ok_or_else(|| {
        VideoError::InvalidArgument("thumbnail encoding requires FrameData::Rgba".to_string())
    })?;
    let img = RgbaImage::from_raw(frame.width, frame.height, rgba.to_vec()).ok_or_else(|| {
        VideoError::InvalidArgument("RGBA buffer size does not match frame dimensions".to_string())
    })?;

    let mut out = Vec::new();
    match encoding {
        ThumbnailEncoding::Png => {
            img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)?;
        }
        ThumbnailEncoding::Jpeg => {
            let rgb = image::DynamicImage::ImageRgba8(img).to_rgb8();
            let mut encoder = JpegEncoder::new_with_quality(&mut out, GOOD_THUMB_QUALITY);
            encoder.encode_image(&rgb)?;
        }
    }
    Ok(out)
}
