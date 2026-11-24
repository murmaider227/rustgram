//! Dominant canvas background gradient and placement math for expanded crops.
//!
//! Port of `media/media_video_canvas.h` and `media/media_video_canvas.cpp`.

use image::RgbaImage;
use image::imageops::{FilterType, resize};
use serde::{Deserialize, Serialize};
use tiny_skia::{
    BlendMode, GradientStop, LinearGradient, Paint, Pixmap, Point, Rect as SkiaRect, SpreadMode,
    Transform,
};

use crate::color_picker::{hsv_f_to_rgb, rgb_to_hsv_f};
use crate::geometry::{Color, Rect, RectF, Size};

const SAMPLE_SIDE: u32 = 240;
const TALL_RATIO: f64 = 1.29;
const SAMPLE_TOP: f64 = 0.1;
const SAMPLE_BOTTOM: f64 = 0.9;

/// Two-stop vertical background gradient (`Media::Encode::CanvasBackground`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanvasBackground {
    /// Gradient color at the top edge.
    pub top: Color,
    /// Gradient color at the bottom edge.
    pub bottom: Color,
}

fn adapt_color(color: Color) -> Color {
    let (hue, mut sat, mut val) = rgb_to_hsv_f(color);
    val = (val - 0.05).clamp(0.15, 0.85);
    if sat > 0.1 && sat <= 0.95 {
        if sat <= 0.5 {
            sat = (sat + 0.2).min(1.0);
        } else if sat > 0.8 {
            sat = (sat - 0.4).max(0.0);
        }
    }
    hsv_f_to_rgb(hue, sat, val)
}

/// Computes the dominant top/bottom gradient colors from an image
/// (`Media::Encode::DominantCanvasBackground`).
#[must_use]
pub fn dominant_canvas_background(image: &RgbaImage) -> Option<CanvasBackground> {
    if image.width() == 0 || image.height() == 0 {
        return None;
    }
    let scaled = Size::new(image.width() as i32, image.height() as i32)
        .scaled_keep_aspect(Size::new(SAMPLE_SIDE as i32, SAMPLE_SIDE as i32));
    if scaled.is_empty() {
        return None;
    }
    let sample = resize(
        image,
        scaled.width as u32,
        scaled.height as u32,
        FilterType::Triangle,
    );
    let x = sample.width() / 2;
    let y_top = ((sample.height() as f64) * SAMPLE_TOP) as u32;
    let y_bottom = (((sample.height() as f64) * SAMPLE_BOTTOM) as u32).min(sample.height() - 1);
    let top_px = sample.get_pixel(x, y_top);
    let bottom_px = sample.get_pixel(x, y_bottom);
    Some(CanvasBackground {
        top: adapt_color(Color::rgb(top_px[0], top_px[1], top_px[2])),
        bottom: adapt_color(Color::rgb(bottom_px[0], bottom_px[1], bottom_px[2])),
    })
}

/// Paints a vertical gradient fill into `pixmap` using the given blend mode.
pub fn paint_canvas_background(
    pixmap: &mut Pixmap,
    rect: Rect,
    background: CanvasBackground,
    blend_mode: BlendMode,
) {
    if rect.is_empty() {
        return;
    }
    let Some(skia_rect) = SkiaRect::from_xywh(
        rect.x as f32,
        rect.y as f32,
        rect.width as f32,
        rect.height as f32,
    ) else {
        return;
    };
    let Some(shader) = LinearGradient::new(
        Point::from_xy(rect.x as f32, rect.y as f32),
        Point::from_xy(rect.x as f32, (rect.y + rect.height) as f32),
        vec![
            GradientStop::new(0.0, background.top.to_skia()),
            GradientStop::new(1.0, background.bottom.to_skia()),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    ) else {
        return;
    };
    let paint = Paint {
        shader,
        blend_mode,
        anti_alias: false,
        ..Paint::default()
    };
    pixmap.fill_rect(skia_rect, &paint, Transform::identity(), None);
}

/// Computes centered placement of an image on a target canvas
/// (`Media::Encode::CanvasPlacement`).
#[must_use]
pub fn canvas_placement(image: Size, canvas: Size) -> Rect {
    if image.is_empty() || canvas.is_empty() {
        return Rect::from_size(canvas);
    }
    let mut scale = canvas.width as f64 / image.width as f64;
    if image.height as f64 / image.width as f64 > TALL_RATIO {
        scale = scale.max(canvas.height as f64 / image.height as f64);
    }
    let w = (image.width as f64 * scale).round().max(1.0) as i32;
    let h = (image.height as f64 * scale).round().max(1.0) as i32;
    Rect::new((canvas.width - w) / 2, (canvas.height - h) / 2, w, h)
}

/// Rounds a floating-point canvas rect to integer coordinates (`Media::Encode::RoundedCanvasRect`).
#[must_use]
pub fn rounded_canvas_rect(rect: RectF) -> Rect {
    Rect::new(
        rect.x.round() as i32,
        rect.y.round() as i32,
        rect.width.round().max(1.0) as i32,
        rect.height.round().max(1.0) as i32,
    )
}
