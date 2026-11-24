//! Photo preparation and JPEG export matching Telegram Desktop's sending pipeline.
//!
//! Port of `PhotoSideLimit`, `ComputePhotoJpegBytes`, `PrepareFileThumbnail` from
//! `storage/localimageloader.cpp`, `ValidateThumbDimensions` from
//! `ui/chat/attach/attach_prepare.cpp`, and `Images::Opaque` from
//! `lib_ui/ui/image/image_prepare.cpp`.

use image::codecs::jpeg::JpegEncoder;
use image::imageops::{FilterType, resize};
use image::{ExtendedColorType, Rgb, RgbImage, RgbaImage};
use serde::{Deserialize, Serialize};

use crate::EditorError;
use crate::apply::apply_with_stickers;
use crate::common::PhotoModifications;
use crate::geometry::Size;
use crate::scene::StickerRef;

/// Standard Telegram photo max side in pixels (`PhotoSideLimit(false)`).
pub const PHOTO_SIDE_LIMIT_STANDARD: u32 = 1280;

/// High-definition / large Telegram photo max side in pixels (`PhotoSideLimit(true)`).
pub const PHOTO_SIDE_LIMIT_LARGE: u32 = 2560;

/// JPEG quality used by Telegram Desktop when encoding photos (`ComputePhotoJpegBytes`).
pub const PHOTO_JPEG_QUALITY: u8 = 87;

/// Maximum thumbnail side length in pixels (`kThumbnailSize` in `localimageloader.cpp`).
pub const THUMBNAIL_MAX_SIDE: u32 = 320;

/// JPEG quality for thumbnails (`kThumbnailQuality` in `localimageloader.cpp`).
pub const THUMBNAIL_JPEG_QUALITY: u8 = 87;

/// Bits-per-pixel threshold above which an existing JPEG is recompressed (`kRecompressAfterBpp`).
pub const RECOMPRESS_AFTER_BPP: u64 = 4;

/// Maximum aspect ratio (`20:1`) allowed for sending an image as a compressed photo
/// (`Ui::ValidateThumbDimensions`).
pub const MAX_THUMB_ASPECT_RATIO: u32 = 20;

/// Returns Telegram's photo side limit (`2560` for large/HD photos, `1280` for standard).
#[must_use]
pub const fn photo_side_limit(large: bool) -> u32 {
    if large {
        PHOTO_SIDE_LIMIT_LARGE
    } else {
        PHOTO_SIDE_LIMIT_STANDARD
    }
}

/// Validates whether `(width, height)` can be sent as a Telegram photo/thumbnail
/// (`Ui::ValidateThumbDimensions`: both > 0 and aspect ratio <= 20:1).
#[must_use]
pub const fn validate_thumb_dimensions(width: u32, height: u32) -> bool {
    width > 0
        && height > 0
        && width <= MAX_THUMB_ASPECT_RATIO.saturating_mul(height)
        && height <= MAX_THUMB_ASPECT_RATIO.saturating_mul(width)
}

/// Settings controlling JPEG photo export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhotoExportSettings {
    /// Maximum width or height in pixels (`2560` for HD photos, `1280` for standard).
    pub max_side: u32,
    /// JPEG encoder quality (`1..=100`, default `87`).
    pub quality: u8,
    /// Composite semi-transparent pixels onto solid white before JPEG encoding
    /// (matches `Images::Opaque`).
    pub flatten_alpha_on_white: bool,
    /// Whether to reject images with aspect ratio > 20:1 (sent as uncompressed file in tdesktop).
    pub validate_photo_aspect: bool,
}

impl Default for PhotoExportSettings {
    fn default() -> Self {
        Self::large()
    }
}

impl PhotoExportSettings {
    /// Standard Telegram photo export (`max_side = 1280`, `quality = 87`).
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            max_side: PHOTO_SIDE_LIMIT_STANDARD,
            quality: PHOTO_JPEG_QUALITY,
            flatten_alpha_on_white: true,
            validate_photo_aspect: true,
        }
    }

    /// Large / HD Telegram photo export (`max_side = 2560`, `quality = 87`).
    #[must_use]
    pub const fn large() -> Self {
        Self {
            max_side: PHOTO_SIDE_LIMIT_LARGE,
            quality: PHOTO_JPEG_QUALITY,
            flatten_alpha_on_white: true,
            validate_photo_aspect: true,
        }
    }

    /// Thumbnail export (`max_side = 320`, `quality = 87`).
    #[must_use]
    pub const fn thumbnail() -> Self {
        Self {
            max_side: THUMBNAIL_MAX_SIDE,
            quality: THUMBNAIL_JPEG_QUALITY,
            flatten_alpha_on_white: true,
            validate_photo_aspect: true,
        }
    }
}

/// Result of exporting a photo to JPEG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedJpeg {
    /// Encoded JPEG bytes.
    pub bytes: Vec<u8>,
    /// Exported image width in pixels (after any downscaling).
    pub width: u32,
    /// Exported image height in pixels (after any downscaling).
    pub height: u32,
}

/// Computes downscaled dimensions fitting inside `max_side x max_side` while keeping
/// aspect ratio; images already within the limit keep their original dimensions.
#[must_use]
pub fn scale_to_fit(width: u32, height: u32, max_side: u32) -> (u32, u32) {
    if max_side == 0 || (width <= max_side && height <= max_side) {
        return (width, height);
    }
    let scaled = Size::new(width as i32, height as i32)
        .scaled_keep_aspect(Size::new(max_side as i32, max_side as i32));
    (scaled.width.max(1) as u32, scaled.height.max(1) as u32)
}

/// Composites an RGBA image onto a solid white background (`Images::Opaque`).
#[must_use]
pub fn opaque_on_white(image: &RgbaImage) -> RgbImage {
    let w = image.width();
    let h = image.height();
    let mut out = RgbImage::new(w, h);
    for (src, dst) in image.pixels().zip(out.pixels_mut()) {
        let [r, g, b, a] = src.0;
        if a == 255 {
            *dst = Rgb([r, g, b]);
        } else if a == 0 {
            *dst = Rgb([255, 255, 255]);
        } else {
            let alpha = u16::from(a);
            let inv = 255 - alpha;
            let blend = |ch: u8| -> u8 { ((u16::from(ch) * alpha + 255 * inv + 127) / 255) as u8 };
            *dst = Rgb([blend(r), blend(g), blend(b)]);
        }
    }
    out
}

/// Checks whether an existing raw JPEG byte slice is small enough to pass through
/// without re-encoding (`bytes.len() <= width * height * kRecompressAfterBpp / 8`).
#[must_use]
pub const fn within_recompress_threshold(byte_len: usize, width: u32, height: u32) -> bool {
    let max_bytes = (width as u64)
        .saturating_mul(height as u64)
        .saturating_mul(RECOMPRESS_AFTER_BPP)
        / 8;
    (byte_len as u64) <= max_bytes
}

/// Exports `image` to JPEG using `settings` (downscaling if larger than `settings.max_side`,
/// compositing alpha onto white, and encoding with `settings.quality`).
pub fn export_jpeg(
    image: &RgbaImage,
    settings: &PhotoExportSettings,
) -> Result<ExportedJpeg, EditorError> {
    let w = image.width();
    let h = image.height();
    if w == 0 || h == 0 {
        return Err(EditorError::EmptyImage);
    }
    if settings.validate_photo_aspect && !validate_thumb_dimensions(w, h) {
        return Err(EditorError::InvalidPhotoDimensions {
            width: w,
            height: h,
        });
    }

    let (target_w, target_h) = scale_to_fit(w, h, settings.max_side);
    let scaled_cow;
    let working = if target_w != w || target_h != h {
        scaled_cow = resize(image, target_w, target_h, FilterType::CatmullRom);
        &scaled_cow
    } else {
        image
    };

    let rgb = if settings.flatten_alpha_on_white {
        opaque_on_white(working)
    } else {
        let mut out = RgbImage::new(working.width(), working.height());
        for (src, dst) in working.pixels().zip(out.pixels_mut()) {
            *dst = Rgb([src[0], src[1], src[2]]);
        }
        out
    };

    let quality = settings.quality.clamp(1, 100);
    let mut bytes = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut bytes, quality);
    encoder
        .encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            ExtendedColorType::Rgb8,
        )
        .map_err(|e| EditorError::JpegEncode(e.to_string()))?;

    Ok(ExportedJpeg {
        bytes,
        width: rgb.width(),
        height: rgb.height(),
    })
}

/// Applies `modifications` to `image` and exports the resulting photo as JPEG.
pub fn apply_and_export_jpeg(
    image: &RgbaImage,
    modifications: &PhotoModifications,
    settings: &PhotoExportSettings,
) -> Result<ExportedJpeg, EditorError> {
    apply_and_export_jpeg_with_stickers(image, modifications, settings, |_| None)
}

/// Applies `modifications` (with external sticker resolution) and exports as JPEG.
pub fn apply_and_export_jpeg_with_stickers<'a, F>(
    image: &RgbaImage,
    modifications: &PhotoModifications,
    settings: &PhotoExportSettings,
    sticker_resolver: F,
) -> Result<ExportedJpeg, EditorError>
where
    F: FnMut(&StickerRef) -> Option<&'a RgbaImage>,
{
    if image.width() == 0 || image.height() == 0 {
        return Err(EditorError::EmptyImage);
    }
    let modified = apply_with_stickers(image, modifications, sticker_resolver);
    export_jpeg(&modified, settings)
}
