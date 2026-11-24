//! Profile photo (userpic) validation, initial square crop, and finalization flow.
//!
//! Port of `Editor::PrepareProfilePhoto` from `editor/photo_editor_layer_widget.h`
//! and `editor/photo_editor_layer_widget.cpp`.

use image::RgbaImage;
use image::imageops::{FilterType, resize};

use crate::EditorError;
use crate::apply::{apply_shape_mask, apply_with_stickers};
use crate::common::{CropMode, CropType, EditorData, PhotoModifications};
use crate::geometry::{Rect, Size};
use crate::scene::StickerRef;

/// Minimum side length in pixels for Telegram profile photos (`Editor::kProfilePhotoSize`).
pub const PROFILE_PHOTO_SIZE: u32 = 640;

/// Maximum allowed aspect ratio for profile photos (`10:1` in `PrepareProfilePhoto`).
pub const MAX_PROFILE_ASPECT_RATIO: u32 = 10;

/// Prepared input ready to open the userpic editor.
#[derive(Debug, Clone)]
pub struct PreparedProfilePhoto {
    /// Source image scaled up to at least `640px` on both sides (`KeepAspectRatioByExpanding`).
    pub image: RgbaImage,
    /// Initial modifications with centered square crop (`min_side x min_side`).
    pub modifications: PhotoModifications,
    /// Editor configuration with locked square aspect ratio and chosen crop shape.
    pub editor_data: EditorData,
}

/// Validates dimensions for a profile photo (`width > 0 && height > 0 && ratio <= 10:1`).
#[must_use]
pub const fn validate_profile_photo_dimensions(width: u32, height: u32) -> bool {
    width > 0
        && height > 0
        && width <= MAX_PROFILE_ASPECT_RATIO.saturating_mul(height)
        && height <= MAX_PROFILE_ASPECT_RATIO.saturating_mul(width)
}

/// Prepares an image for userpic editing (`Editor::PrepareProfilePhoto`).
///
/// - Rejects empty images or aspect ratios exceeding `10:1`.
/// - Scales up small images so both sides are at least [`PROFILE_PHOTO_SIZE`] (640 px)
///   using `KeepAspectRatioByExpanding`.
/// - Computes the centered square initial crop.
/// - Forces `keep_aspect_ratio = true` on `editor_data` and propagates `crop_type`/`crop_mode`.
pub fn prepare_profile_photo_input(
    image: &RgbaImage,
    mut editor_data: EditorData,
) -> Result<PreparedProfilePhoto, EditorError> {
    let w = image.width();
    let h = image.height();
    if !validate_profile_photo_dimensions(w, h) {
        return Err(EditorError::InvalidPhotoDimensions {
            width: w,
            height: h,
        });
    }

    let resized = resize_to_min_expanding(image, PROFILE_PHOTO_SIZE);
    let rw = resized.width() as i32;
    let rh = resized.height() as i32;
    let min_side = rw.min(rh);
    let crop = Rect::new((rw - min_side) / 2, (rh - min_side) / 2, min_side, min_side);

    editor_data.keep_aspect_ratio = true;
    if editor_data.crop_type == CropType::Rect {
        editor_data.crop_type = CropType::Ellipse;
    }

    let modifications = PhotoModifications {
        crop: Some(crop),
        crop_type: editor_data.crop_type,
        crop_mode: editor_data.crop_mode,
        ..PhotoModifications::default()
    };

    Ok(PreparedProfilePhoto {
        image: resized,
        modifications,
        editor_data,
    })
}

/// Finalizes a profile photo after editing (`applyModifications` in `PrepareProfilePhoto`).
///
/// Applies `mods`, ensures the output is at least [`PROFILE_PHOTO_SIZE`] (640 px) on its
/// larger side (`KeepAspectRatio`), and applies the shape mask if `mods.crop_mode == CropMode::Mask`.
pub fn finalize_profile_photo(
    image: &RgbaImage,
    mods: &PhotoModifications,
) -> Result<RgbaImage, EditorError> {
    finalize_profile_photo_with_stickers(image, mods, |_| None)
}

/// Variant of [`finalize_profile_photo`] that resolves external stickers via `sticker_resolver`.
pub fn finalize_profile_photo_with_stickers<'a, F>(
    image: &RgbaImage,
    mods: &PhotoModifications,
    sticker_resolver: F,
) -> Result<RgbaImage, EditorError>
where
    F: FnMut(&StickerRef) -> Option<&'a RgbaImage>,
{
    if image.width() == 0 || image.height() == 0 {
        return Err(EditorError::EmptyImage);
    }
    let modified = apply_with_stickers(image, mods, sticker_resolver);
    let mut out = resize_to_min_keep_aspect(&modified, PROFILE_PHOTO_SIZE);
    if mods.crop_mode == CropMode::Mask {
        apply_shape_mask(&mut out, mods);
    }
    Ok(out)
}

fn resize_to_min_expanding(image: &RgbaImage, min_side: u32) -> RgbaImage {
    if image.width() >= min_side && image.height() >= min_side {
        return image.clone();
    }
    let target = Size::new(image.width() as i32, image.height() as i32)
        .scaled_keep_aspect_expanding(Size::new(min_side as i32, min_side as i32));
    resize(
        image,
        target.width.max(1) as u32,
        target.height.max(1) as u32,
        FilterType::CatmullRom,
    )
}

fn resize_to_min_keep_aspect(image: &RgbaImage, min_side: u32) -> RgbaImage {
    if image.width() >= min_side && image.height() >= min_side {
        return image.clone();
    }
    let target = Size::new(image.width() as i32, image.height() as i32)
        .scaled_keep_aspect(Size::new(min_side as i32, min_side as i32));
    resize(
        image,
        target.width.max(1) as u32,
        target.height.max(1) as u32,
        FilterType::CatmullRom,
    )
}
