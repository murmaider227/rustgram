//! Image decoding, inline stripped/vector thumbnails, EXIF orientation, progressive JPEG
//! inspection, photo size selection, and image preparation operations.
//!
//! Port of `Telegram/lib_ui/ui/image/image_prepare.cpp` and `Telegram/SourceFiles/ui/image/*`.

pub mod inline;
pub mod jpeg;
pub mod prepare;
pub mod sizes;

use crate::error::{MediaError, Result};
use flate2::read::GzDecoder;
use image::{ImageFormat, ImageReader, RgbaImage, imageops::FilterType};
use std::io::{Cursor, Read};
use std::path::Path;

/// Maximum input file size accepted by [`read_image`] (`kReadBytesLimit` in `image_prepare.h`).
pub const READ_BYTES_LIMIT: usize = 64 * 1024 * 1024;

/// Maximum pixel area (`width * height`) accepted by [`read_image`] (`kReadMaxArea` in `image_prepare.h`).
pub const READ_MAX_AREA: u64 = 12_032 * 9_024;

/// Maximum uncompressed payload size for [`unpack_gzip`] (`kMaxGzipFileSize` in `image_prepare.cpp`).
pub const MAX_GZIP_FILE_SIZE: usize = 5 * 1024 * 1024;

/// Arguments for [`read_image`], mirroring `Images::ReadArgs` (`image_prepare.h:111`).
#[derive(Debug, Clone, Default)]
pub struct ReadArgs {
    /// Raw encoded image bytes. If empty and `path` is provided, bytes are read from disk.
    pub content: Vec<u8>,
    /// Optional path to read from when `content` is empty.
    pub path: Option<std::path::PathBuf>,
    /// Optional maximum `(width, height)` to scale down to while preserving aspect ratio.
    pub max_size: Option<(u32, u32)>,
    /// If `true` and format is not JPEG, compositing over white removes alpha (`Images::Opaque`).
    pub force_opaque: bool,
    /// If `true`, return the raw encoded bytes inside [`ReadResult::content`].
    pub return_content: bool,
}

/// Decoded image and metadata returned by [`read_image`], mirroring `Images::ReadResult`.
#[derive(Debug, Clone)]
pub struct ReadResult {
    /// Decoded RGBA image (already EXIF-oriented and optionally scaled/opaque).
    pub image: RgbaImage,
    /// Raw encoded bytes if `ReadArgs::return_content` was set.
    pub content: Vec<u8>,
    /// Lowercase format name (`"jpeg"`, `"png"`, `"webp"`, `"gif"`).
    pub format: String,
    /// Scale factor applied due to `max_size` (`1.0` if unscaled).
    pub scale: f64,
    /// `true` if the source is an animated GIF or animated WebP with > 1 frame.
    pub animated: bool,
}

/// Decompresses a gzip buffer with tdesktop's 5 MiB safety limit (`Images::UnpackGzip`).
///
/// If the input is not valid gzip or exceeds [`MAX_GZIP_FILE_SIZE`], returns a [`MediaError`].
pub fn unpack_gzip(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = GzDecoder::new(bytes);
    let mut out = Vec::new();
    let mut limited = (&mut decoder).take((MAX_GZIP_FILE_SIZE + 1) as u64);
    limited
        .read_to_end(&mut out)
        .map_err(|e| MediaError::InvalidFormat(format!("gzip decompress failed: {e}")))?;
    if out.len() > MAX_GZIP_FILE_SIZE {
        return Err(MediaError::LimitExceeded(format!(
            "uncompressed gzip exceeds {MAX_GZIP_FILE_SIZE} bytes"
        )));
    }
    out.shrink_to_fit();
    Ok(out)
}

/// Returns `true` if `format` is `"svg"` or `"svgz"` (case-insensitive), matching `Images::IsSvgFormat`.
#[must_use]
pub fn is_svg_format(format: &str) -> bool {
    format.eq_ignore_ascii_case("svg") || format.eq_ignore_ascii_case("svgz")
}

/// Reads and decodes a JPEG, PNG, WebP, or GIF image with EXIF auto-orientation,
/// area/byte limits, animation detection, optional downscaling, and optional white-background opaquing.
///
/// Mirrors `Images::Read` (`image_prepare.cpp:542`).
pub fn read_image(mut args: ReadArgs) -> Result<ReadResult> {
    if args.content.is_empty() {
        let Some(ref path) = args.path else {
            return Err(MediaError::InvalidFormat("empty image input".into()));
        };
        args.content = read_file_limited(path, READ_BYTES_LIMIT)?;
    }
    if args.content.len() > READ_BYTES_LIMIT {
        return Err(MediaError::LimitExceeded(format!(
            "image bytes {} exceed limit {}",
            args.content.len(),
            READ_BYTES_LIMIT
        )));
    }

    let reader = ImageReader::new(Cursor::new(args.content.as_slice()))
        .with_guessed_format()
        .map_err(|e| MediaError::InvalidFormat(e.to_string()))?;

    let format = reader
        .format()
        .ok_or_else(|| MediaError::InvalidFormat("unrecognized image format".into()))?;

    let (w, h) = reader
        .into_dimensions()
        .map_err(|e| MediaError::InvalidFormat(e.to_string()))?;

    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > READ_MAX_AREA {
        return Err(MediaError::LimitExceeded(format!(
            "image dimensions {w}x{h} exceed area limit {READ_MAX_AREA}"
        )));
    }

    let animated = detect_animated(&args.content, format);
    let format_name = match format {
        ImageFormat::Jpeg => "jpeg",
        ImageFormat::Png => "png",
        ImageFormat::WebP => "webp",
        ImageFormat::Gif => "gif",
        other => {
            return Err(MediaError::InvalidFormat(format!(
                "unsupported format: {other:?}"
            )));
        }
    };

    let decoded = ImageReader::with_format(Cursor::new(args.content.as_slice()), format)
        .decode()
        .map_err(|e| MediaError::InvalidFormat(e.to_string()))?;

    let mut image = decoded.to_rgba8();

    // Apply EXIF auto-transform for JPEGs (mirrors `reader.setAutoTransform(true)`).
    if format == ImageFormat::Jpeg {
        let orientation = jpeg::read_jpeg_exif_orientation(&args.content);
        if orientation != jpeg::ExifOrientation::Normal {
            image = jpeg::apply_exif_orientation(image, orientation);
        }
    }

    let orig_w = image.width();
    let orig_h = image.height();
    let mut scale = 1.0_f64;

    if let Some((max_w, max_h)) = args.max_size
        && max_w > 0
        && max_h > 0
        && (orig_w > max_w || orig_h > max_h)
    {
        let (new_w, new_h) = prepare::shrink_to_keep_aspect(orig_w, orig_h, max_w, max_h);
        image = image::imageops::resize(&image, new_w, new_h, FilterType::Triangle);
        scale = if orig_w > orig_h {
            f64::from(image.width()) / f64::from(orig_w)
        } else {
            f64::from(image.height()) / f64::from(orig_h)
        };
    }

    if args.force_opaque && format_name != "jpeg" {
        image = prepare::opaque(image);
    }

    let content = if args.return_content {
        args.content
    } else {
        Vec::new()
    };

    Ok(ReadResult {
        image,
        content,
        format: format_name.to_string(),
        scale,
        animated,
    })
}

fn read_file_limited(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > limit as u64 {
        return Err(MediaError::LimitExceeded(format!(
            "file {} ({} bytes) exceeds limit {}",
            path.display(),
            meta.len(),
            limit
        )));
    }
    Ok(std::fs::read(path)?)
}

fn detect_animated(bytes: &[u8], format: ImageFormat) -> bool {
    match format {
        ImageFormat::Gif => {
            use image::AnimationDecoder as _;
            image::codecs::gif::GifDecoder::new(Cursor::new(bytes))
                .ok()
                .map(|dec| dec.into_frames().take(2).count() > 1)
                .unwrap_or(false)
        }
        ImageFormat::WebP => {
            // Check VP8X extended WebP header animation flag (bit 1 of byte 20).
            if bytes.len() >= 21 && &bytes[0..4] == b"RIFF" && &bytes[8..16] == b"WEBPVP8X" {
                (bytes[20] & 0x02) != 0
            } else {
                false
            }
        }
        _ => false,
    }
}
