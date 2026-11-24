//! Applying `PhotoModifications` (enhance, paint, crop/expand, rotate, flip, shape mask)
//! to an RGBA image.
//!
//! Port of `Editor::ImageModified` and `Editor::ApplyShapeMask` from
//! `editor/photo_editor_common.cpp`.

use image::{Rgba, RgbaImage};
use tiny_skia::{BlendMode, Path, PathBuilder, Pixmap, PixmapPaint, Rect as SkiaRect, Transform};

use crate::canvas_bg::{dominant_canvas_background, paint_canvas_background};
use crate::common::{CropMode, CropType, PhotoModifications};
use crate::enhance::enhance_image;
use crate::geometry::{Affine2D, Rect, RectF, Size, normalize_angle_90};
use crate::scene::{StickerRef, pixmap_to_rgba, rgba_to_pixmap, solid_white_paint};

/// Applies `modifications` to `image`, matching `Editor::ImageModified(image, mods)`.
///
/// Pipeline order (exact match to `photo_editor_common.cpp`):
/// 1. Auto-enhance (`mods.enhance > 0`).
/// 2. If `mods.crop` extends outside the image bounds (expanded canvas):
///    create transparent canvas of `crop.size()`, draw image at `-crop.top_left()`,
///    render paint scene onto expanded canvas.
///    Otherwise: render paint scene onto image, then crop to `mods.crop` if valid.
/// 3. Transform by clockwise rotation `mods.angle` (90° steps), then horizontal flip
///    if `mods.flipped` (exact Qt `QTransform::scale(-1, 1); rotate(angle)` order).
/// 4. If canvas was expanded, fill exposed area behind the original image with
///    `dominant_canvas_background(enhanced_image)`.
///
/// Any sticker items with inline `image` are composited automatically. To resolve
/// external stickers by [`StickerRef`], use [`apply_with_stickers`].
#[must_use]
pub fn apply(image: &RgbaImage, modifications: &PhotoModifications) -> RgbaImage {
    apply_with_stickers(image, modifications, |_| None)
}

/// Variant of [`apply`] that resolves external sticker RGBA layers via `sticker_resolver`.
#[must_use]
pub fn apply_with_stickers<'a, F>(
    image: &RgbaImage,
    modifications: &PhotoModifications,
    mut sticker_resolver: F,
) -> RgbaImage
where
    F: FnMut(&StickerRef) -> Option<&'a RgbaImage>,
{
    if image.width() == 0 || image.height() == 0 {
        return image.clone();
    }
    if modifications.is_empty() {
        return image.clone();
    }

    let base = if modifications.enhance > 0 {
        enhance_image(image, modifications.enhance)
    } else {
        image.clone()
    };

    let image_rect = Rect::from_size(Size::new(base.width() as i32, base.height() as i32));
    let crop_opt = modifications.crop.filter(|c| c.is_valid());
    let expanded = crop_opt.is_some_and(|crop| !image_rect.contains_rect(crop));

    let cropped = if expanded {
        let crop = crop_opt.unwrap_or(image_rect);
        expand_canvas(
            &base,
            crop,
            modifications.paint.as_ref(),
            &mut sticker_resolver,
        )
    } else {
        let painted = if let Some(scene) = modifications
            .paint
            .as_ref()
            .filter(|s| s.has_active_items())
        {
            if let Some(mut pixmap) = rgba_to_pixmap(&base) {
                let bounds = image_rect.to_f64();
                scene.render_baked(&mut pixmap, bounds, bounds, &mut sticker_resolver);
                pixmap_to_rgba(&pixmap)
            } else {
                base.clone()
            }
        } else {
            base.clone()
        };

        if let Some(crop) = crop_opt {
            crop_subimage(&painted, crop)
        } else {
            painted
        }
    };

    let angle = normalize_angle_90(modifications.angle);
    let mut result = rotate_and_flip(&cropped, angle, modifications.flipped);

    if expanded
        && let (Some(crop), Some(bg)) = (crop_opt, dominant_canvas_background(&base))
        && let Some(mut pixmap) = rgba_to_pixmap(&result)
    {
        let transform = qt_rotate_flip_transform(angle, modifications.flipped)
            .true_matrix(cropped.width() as f64, cropped.height() as f64);
        let keep = transform
            .map_rect(RectF::new(
                (-crop.x) as f64,
                (-crop.y) as f64,
                base.width() as f64,
                base.height() as f64,
            ))
            .to_rect();
        fill_canvas_background_outside(&mut pixmap, bg, keep);
        result = pixmap_to_rgba(&pixmap);
    }

    result
}

/// Applies the crop shape alpha mask (`Ellipse` or `RoundedRect`) when
/// `mods.crop_mode == CropMode::Mask` (`Editor::ApplyShapeMask`).
pub fn apply_shape_mask(image: &mut RgbaImage, mods: &PhotoModifications) {
    if mods.crop_mode != CropMode::Mask {
        return;
    }
    if mods.crop_type == CropType::Rect {
        return;
    }
    let multiplier = match mods.crop_type {
        CropType::RoundedRect => mods.corners_level.multiplier(),
        CropType::Ellipse => 0.5,
        CropType::Rect => return,
    };
    if mods.crop_type == CropType::RoundedRect && multiplier <= 0.0 {
        return;
    }
    let w = image.width();
    let h = image.height();
    if w == 0 || h == 0 {
        return;
    }
    let Some(mut pixmap) = rgba_to_pixmap(image) else {
        return;
    };
    let Some(mut mask) = Pixmap::new(w, h) else {
        return;
    };
    let path = match mods.crop_type {
        CropType::Ellipse => {
            SkiaRect::from_xywh(0.0, 0.0, w as f32, h as f32).and_then(PathBuilder::from_oval)
        }
        CropType::RoundedRect => {
            let radius = (w.min(h) as f64 * multiplier) as f32;
            rounded_rect_path(0.0, 0.0, w as f32, h as f32, radius)
        }
        CropType::Rect => None,
    };
    let Some(path) = path else {
        return;
    };
    mask.fill_path(
        &path,
        &solid_white_paint(),
        tiny_skia::FillRule::Winding,
        Transform::identity(),
        None,
    );
    let paint = PixmapPaint {
        blend_mode: BlendMode::DestinationIn,
        ..PixmapPaint::default()
    };
    pixmap.draw_pixmap(0, 0, mask.as_ref(), &paint, Transform::identity(), None);
    *image = pixmap_to_rgba(&pixmap);
}

fn expand_canvas<'a, F>(
    image: &RgbaImage,
    crop: Rect,
    scene: Option<&crate::scene::PaintScene>,
    sticker_resolver: &mut F,
) -> RgbaImage
where
    F: FnMut(&StickerRef) -> Option<&'a RgbaImage>,
{
    let w = crop.width.max(1) as u32;
    let h = crop.height.max(1) as u32;
    let Some(mut pixmap) = Pixmap::new(w, h) else {
        return RgbaImage::new(w, h);
    };
    if let Some(src_pixmap) = rgba_to_pixmap(image) {
        let paint = PixmapPaint {
            blend_mode: BlendMode::Source,
            ..PixmapPaint::default()
        };
        pixmap.draw_pixmap(
            -crop.x,
            -crop.y,
            src_pixmap.as_ref(),
            &paint,
            Transform::identity(),
            None,
        );
    }
    if let Some(scene) = scene.filter(|s| s.has_active_items()) {
        scene.render_baked(
            &mut pixmap,
            RectF::new(0.0, 0.0, w as f64, h as f64),
            crop.to_f64(),
            sticker_resolver,
        );
    }
    pixmap_to_rgba(&pixmap)
}

fn crop_subimage(image: &RgbaImage, crop: Rect) -> RgbaImage {
    let w = crop.width.max(1) as u32;
    let h = crop.height.max(1) as u32;
    let mut out = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));
    let src_w = image.width() as i32;
    let src_h = image.height() as i32;
    for y in 0..crop.height {
        let sy = crop.y + y;
        if sy < 0 || sy >= src_h {
            continue;
        }
        for x in 0..crop.width {
            let sx = crop.x + x;
            if sx < 0 || sx >= src_w {
                continue;
            }
            out.put_pixel(x as u32, y as u32, *image.get_pixel(sx as u32, sy as u32));
        }
    }
    out
}

/// Exact pixel-accurate rotation (90° CW steps) followed by optional horizontal flip,
/// matching Qt's `QTransform t; if (flipped) t.scale(-1, 1); if (angle) t.rotate(angle);`.
pub(crate) fn rotate_and_flip(image: &RgbaImage, angle: i32, flipped: bool) -> RgbaImage {
    let angle = normalize_angle_90(angle);
    if angle == 0 && !flipped {
        return image.clone();
    }
    let w = image.width();
    let h = image.height();
    let (out_w, out_h) = match angle {
        90 | 270 => (h, w),
        _ => (w, h),
    };
    let mut out = RgbaImage::new(out_w, out_h);

    for y in 0..h {
        for x in 0..w {
            let px = *image.get_pixel(x, y);
            // Step 1: Rotate clockwise by `angle`.
            let (rx, ry) = match angle {
                0 => (x, y),
                90 => (h - 1 - y, x),
                180 => (w - 1 - x, h - 1 - y),
                270 => (y, w - 1 - x),
                _ => (x, y),
            };
            // Step 2: Horizontal flip if requested.
            let fx = if flipped { out_w - 1 - rx } else { rx };
            out.put_pixel(fx, ry, px);
        }
    }
    out
}

fn qt_rotate_flip_transform(angle: i32, flipped: bool) -> Affine2D {
    let mut t = Affine2D::identity();
    if flipped {
        t = t.qt_scale(-1.0, 1.0);
    }
    if angle != 0 {
        t = t.qt_rotate(angle as f64);
    }
    t
}

fn fill_canvas_background_outside(
    pixmap: &mut Pixmap,
    background: crate::canvas_bg::CanvasBackground,
    keep: Rect,
) {
    let full = Rect::new(0, 0, pixmap.width() as i32, pixmap.height() as i32);
    if keep.is_empty() {
        paint_canvas_background(pixmap, full, background, BlendMode::DestinationOver);
        return;
    }
    let Some(mut bg_pixmap) = Pixmap::new(pixmap.width(), pixmap.height()) else {
        return;
    };
    paint_canvas_background(&mut bg_pixmap, full, background, BlendMode::Source);

    // Clear the `keep` region so we do not paint background behind transparent pixels
    // inside the original image rectangle (matches `QRegion(canvas) - QRegion(keep)`).
    let clipped_keep = full.intersected(keep);
    if clipped_keep.is_valid()
        && let Some(skia_keep) = SkiaRect::from_xywh(
            clipped_keep.x as f32,
            clipped_keep.y as f32,
            clipped_keep.width as f32,
            clipped_keep.height as f32,
        )
    {
        let clear_paint = tiny_skia::Paint {
            blend_mode: BlendMode::Clear,
            ..tiny_skia::Paint::default()
        };
        bg_pixmap.fill_rect(skia_keep, &clear_paint, Transform::identity(), None);
    }

    let dst_over = PixmapPaint {
        blend_mode: BlendMode::DestinationOver,
        ..PixmapPaint::default()
    };
    pixmap.draw_pixmap(
        0,
        0,
        bg_pixmap.as_ref(),
        &dst_over,
        Transform::identity(),
        None,
    );
}

fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, radius: f32) -> Option<Path> {
    let r = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    if r <= 0.0 {
        return SkiaRect::from_xywh(x, y, w, h).map(PathBuilder::from_rect);
    }
    // Cubic Bézier approximation of a quarter circle: kappa = 4*(sqrt(2)-1)/3
    const KAPPA: f32 = 0.552_284_8;
    let c = r * KAPPA;
    let right = x + w;
    let bottom = y + h;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(right - r, y);
    pb.cubic_to(right - r + c, y, right, y + r - c, right, y + r);
    pb.line_to(right, bottom - r);
    pb.cubic_to(
        right,
        bottom - r + c,
        right - r + c,
        bottom,
        right - r,
        bottom,
    );
    pb.line_to(x + r, bottom);
    pb.cubic_to(x + r - c, bottom, x, bottom - r + c, x, bottom - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - c, x + r - c, y, x + r, y);
    pb.close();
    pb.finish()
}
