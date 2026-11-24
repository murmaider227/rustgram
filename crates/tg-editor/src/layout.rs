//! Photo editor inner viewport and controls layout math.
//!
//! Port of layout calculations from `editor/photo_editor.cpp`,
//! `editor/photo_editor_content.cpp`, and `editor/editor.style`.

use crate::common::PhotoModifications;
use crate::crop::CROP_POINT_SIZE;
use crate::geometry::{
    Affine2D, Margins, Rect, RectF, Size, SizeF, flip_size_by_rotation, normalize_angle_90,
};

/// Scale ratio applied when holding Ctrl to make room for canvas expansion (`kExpansionRoomRatio`).
pub const EXPANSION_ROOM_RATIO: f64 = 0.7;

/// Total height reserved for the bottom controls bars (`photoEditorControlsHeight`).
pub const CONTROLS_HEIGHT: i32 = 146;

/// Outer skip around the editor content (`photoEditorControlsBottomSkip`).
pub const CONTROLS_BOTTOM_SKIP: i32 = 20;

/// Vertical gap between top and bottom paint bars (`photoEditorControlsCenterSkip`).
pub const CONTROLS_CENTER_SKIP: i32 = 6;

/// Height of a single editor button bar (`photoEditorButtonBarHeight`).
pub const BUTTON_BAR_HEIGHT: i32 = 48;

/// Maximum width of an editor button bar (`photoEditorButtonBarWidth`).
pub const BUTTON_BAR_WIDTH: i32 = 422;

/// Default margins around `PhotoEditorContent` inside `PhotoEditor` (`photoEditorContentMargins`).
pub const CONTENT_MARGINS: Margins = Margins::new(
    CONTROLS_BOTTOM_SKIP,
    CONTROLS_BOTTOM_SKIP,
    CONTROLS_BOTTOM_SKIP,
    CONTROLS_HEIGHT,
);

/// Result of laying out the photo, expanded canvas, and controls within an editor viewport.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhotoEditorInnerLayout {
    /// Unrotated image rect centered around the viewport origin before `image_matrix`.
    pub image_rect_f: RectF,
    /// Rounded integer image rect before `image_matrix`.
    pub image_rect: Rect,
    /// Transformed image geometry in content widget coordinates.
    pub image_geometry: Rect,
    /// Transformed canvas (image united with crop) geometry in content widget coordinates.
    pub canvas_geometry: Rect,
    /// Transform mapping `image_rect` into content widget coordinates.
    pub image_matrix: Affine2D,
    /// Scale factor from original photo pixels to displayed image pixels.
    pub scale: f64,
    /// Rectangle allocated to the bottom controls widget inside the outer editor rect.
    pub controls_rect: Rect,
}

/// Computes the target unrotated image rectangle inside `content_size`
/// (`PhotoEditorContent::layoutTarget`).
#[must_use]
pub fn layout_target(
    angle: i32,
    content_size: Size,
    photo_size: Size,
    canvas: Rect,
    crop_margins: Margins,
    expansion_room: bool,
) -> RectF {
    if content_size.is_empty() || photo_size.is_empty() || canvas.is_empty() {
        return RectF::default();
    }
    let rotated_container = flip_size_by_rotation(content_size, angle);
    let full = Size::new(
        (rotated_container.width - crop_margins.horizontal()).max(1),
        (rotated_container.height - crop_margins.vertical()).max(1),
    );
    let fit = if expansion_room {
        SizeF::new(
            full.width as f64 * EXPANSION_ROOM_RATIO,
            full.height as f64 * EXPANSION_ROOM_RATIO,
        )
    } else {
        full.to_f64()
    };
    let scale = 1.0f64
        .min(fit.width / canvas.width as f64)
        .min(fit.height / canvas.height as f64);
    let canvas_size = SizeF::new(canvas.width as f64 * scale, canvas.height as f64 * scale);
    RectF::new(
        -canvas_size.width / 2.0 - canvas.x as f64 * scale,
        -canvas_size.height / 2.0 - canvas.y as f64 * scale,
        photo_size.width as f64 * scale,
        photo_size.height as f64 * scale,
    )
}

/// Computes the full inner layout of `PhotoEditor` + `PhotoEditorContent`
/// for a given outer viewport size, photo size, and modifications.
#[must_use]
pub fn compute_inner_layout(
    outer_size: Size,
    photo_size: Size,
    mods: &PhotoModifications,
    crop_margins: Margins,
    expansion_room: bool,
) -> Option<PhotoEditorInnerLayout> {
    if outer_size.is_empty() || photo_size.is_empty() {
        return None;
    }
    let content_rect = Rect::from_size(outer_size).shrink(CONTENT_MARGINS);
    if content_rect.is_empty() {
        return None;
    }
    let content_size = content_rect.size();
    let canvas = match mods.crop.filter(|c| c.is_valid()) {
        Some(crop) => Rect::from_size(photo_size).united(crop),
        None => Rect::from_size(photo_size),
    };
    let angle = normalize_angle_90(mods.angle);
    let image_rect_f = layout_target(
        angle,
        content_size,
        photo_size,
        canvas,
        crop_margins,
        expansion_room,
    );
    if image_rect_f.is_empty() {
        return None;
    }

    let scale = image_rect_f.width / photo_size.width as f64;
    let image_rect = Rect::new(
        image_rect_f.x.round() as i32,
        image_rect_f.y.round() as i32,
        image_rect_f.width.round() as i32,
        image_rect_f.height.round() as i32,
    );
    let canvas_rect = Rect::new(
        image_rect.x + (canvas.x as f64 * scale).round() as i32,
        image_rect.y + (canvas.y as f64 * scale).round() as i32,
        (canvas.width as f64 * scale).round() as i32,
        (canvas.height as f64 * scale).round() as i32,
    );

    // Exact match to PhotoEditorContent::applyLayout:
    // _imageMatrix.translate(w/2, h/2); if (flipped) scale(-1, 1); rotate(angle);
    let mut image_matrix = Affine2D::identity().qt_translate(
        (content_size.width / 2) as f64,
        (content_size.height / 2) as f64,
    );
    if mods.flipped {
        image_matrix = image_matrix.qt_scale(-1.0, 1.0);
    }
    image_matrix = image_matrix.qt_rotate(angle as f64);

    let image_geometry = image_matrix.map_rect(image_rect.to_f64()).to_rect();
    let canvas_geometry = image_matrix.map_rect(canvas_rect.to_f64()).to_rect();

    let inner_top = content_rect.y + canvas_geometry.y;
    let controls_top = inner_top + canvas_geometry.height + CROP_POINT_SIZE;
    let controls_rect = Rect::new(
        0,
        controls_top,
        outer_size.width,
        (outer_size.height - controls_top).max(0),
    );

    Some(PhotoEditorInnerLayout {
        image_rect_f,
        image_rect,
        image_geometry,
        canvas_geometry,
        image_matrix,
        scale,
        controls_rect,
    })
}
