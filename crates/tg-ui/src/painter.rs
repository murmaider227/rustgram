//! Painter helpers, rounded rectangle cache, 9-slice blurred `BoxShadow`,
//! image rounding/circle preparation, and button interaction state.
//!
//! Port of `Telegram/lib_ui/ui/painter.h`, `Telegram/lib_ui/ui/rect_part.h`,
//! `Telegram/lib_ui/ui/round_rect.cpp`, `Telegram/lib_ui/ui/widgets/shadow.cpp`,
//! `Telegram/lib_ui/ui/image/image_prepare.cpp`, and
//! `Telegram/lib_ui/ui/abstract_button.h`.

use std::collections::HashMap;

use egui::{
    Color32, ColorImage, CornerRadius, Id, Mesh, Painter, Pos2, Rect, Shape, Stroke, StrokeKind,
    TextureHandle, TextureOptions, Vec2,
};

use crate::anim::{SimpleAnimation, Transition};
use crate::style::{BoxShadowStyle, Margins};

/// Bitflags identifying corners and sides of a rectangle.
///
/// Mirrors `RectPart` / `RectParts` in `Telegram/lib_ui/ui/rect_part.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct RectParts(u16);

impl RectParts {
    pub const NONE: Self = Self(0);
    pub const TOP_LEFT: Self = Self(1 << 0);
    pub const TOP_RIGHT: Self = Self(1 << 1);
    pub const BOTTOM_LEFT: Self = Self(1 << 2);
    pub const BOTTOM_RIGHT: Self = Self(1 << 3);
    pub const TOP: Self = Self(1 << 4);
    pub const BOTTOM: Self = Self(1 << 5);
    pub const LEFT: Self = Self(1 << 6);
    pub const RIGHT: Self = Self(1 << 7);
    pub const CENTER: Self = Self(1 << 8);

    pub const ALL_CORNERS: Self =
        Self(Self::TOP_LEFT.0 | Self::TOP_RIGHT.0 | Self::BOTTOM_LEFT.0 | Self::BOTTOM_RIGHT.0);
    pub const FULL_TOP: Self = Self(Self::TOP_LEFT.0 | Self::TOP.0 | Self::TOP_RIGHT.0);
    pub const FULL_BOTTOM: Self = Self(Self::BOTTOM_LEFT.0 | Self::BOTTOM.0 | Self::BOTTOM_RIGHT.0);
    pub const FULL: Self = Self(0x1FF);

    /// Combine two `RectParts` masks.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether `self` contains all bits of `other`.
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

/// Cached rounded-rectangle helper mirroring `Ui::RoundRect`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoundRect {
    radius: f32,
    color: Color32,
}

impl RoundRect {
    /// Construct a new `RoundRect` with corner `radius` and fill `color`.
    pub const fn new(radius: f32, color: Color32) -> Self {
        Self { radius, color }
    }

    /// Update fill color.
    pub fn set_color(&mut self, color: Color32) {
        self.color = color;
    }

    /// Current fill color.
    pub const fn color(&self) -> Color32 {
        self.color
    }

    /// Current corner radius.
    pub const fn radius(&self) -> f32 {
        self.radius
    }

    /// Paint a rounded rectangle with all corners rounded.
    pub fn paint(&self, painter: &Painter, rect: Rect) {
        self.paint_some_rounded(painter, rect, RectParts::ALL_CORNERS);
    }

    /// Paint a rectangle rounding only the corners present in `corners`.
    pub fn paint_some_rounded(&self, painter: &Painter, rect: Rect, corners: RectParts) {
        let r = self
            .radius
            .clamp(0.0, rect.width().min(rect.height()) * 0.5)
            .round() as u8;
        let cr = CornerRadius {
            nw: if corners.contains(RectParts::TOP_LEFT) {
                r
            } else {
                0
            },
            ne: if corners.contains(RectParts::TOP_RIGHT) {
                r
            } else {
                0
            },
            sw: if corners.contains(RectParts::BOTTOM_LEFT) {
                r
            } else {
                0
            },
            se: if corners.contains(RectParts::BOTTOM_RIGHT) {
                r
            } else {
                0
            },
        };
        painter.rect_filled(rect, cr, self.color);
    }

    /// Paint with explicit per-corner radii.
    pub fn paint_complex(
        &self,
        painter: &Painter,
        rect: Rect,
        top_left: f32,
        top_right: f32,
        bottom_left: f32,
        bottom_right: f32,
    ) {
        let cr = CornerRadius {
            nw: top_left.round().clamp(0.0, 255.0) as u8,
            ne: top_right.round().clamp(0.0, 255.0) as u8,
            sw: bottom_left.round().clamp(0.0, 255.0) as u8,
            se: bottom_right.round().clamp(0.0, 255.0) as u8,
        };
        painter.rect_filled(rect, cr, self.color);
    }
}

/// Cache of [`RoundRect`] configurations keyed by `(radius_tenths, color)`.
#[derive(Debug, Default)]
pub struct RoundRectCache {
    entries: HashMap<(u32, Color32), RoundRect>,
}

impl RoundRectCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get or insert a `RoundRect` for `(radius, color)`.
    pub fn get(&mut self, radius: f32, color: Color32) -> RoundRect {
        let key = ((radius.max(0.0) * 10.0).round() as u32, color);
        *self
            .entries
            .entry(key)
            .or_insert_with(|| RoundRect::new(radius, color))
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Clear cached entries (e.g. on palette change).
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// 9-slice blurred outer drop shadow mirroring `Ui::BoxShadow` in
/// `Telegram/lib_ui/ui/widgets/shadow.cpp`.
#[derive(Debug, Clone, Copy)]
pub struct BoxShadow {
    style: BoxShadowStyle,
}

#[derive(Clone)]
struct BoxShadowTextureEntry {
    texture: TextureHandle,
    corner_l: f32,
    corner_t: f32,
    corner_r: f32,
    corner_b: f32,
    middle: f32,
    tex_w: f32,
    tex_h: f32,
}

impl BoxShadow {
    /// Construct a `BoxShadow` from its style parameters.
    pub const fn new(style: BoxShadowStyle) -> Self {
        Self { style }
    }

    /// Exact outer shadow margins matching `BoxShadow::ExtendFor` in `shadow.cpp`.
    pub fn extend_for(st: &BoxShadowStyle) -> Margins {
        let blur = st.blur_radius.max(0.0);
        let ox = st.offset.x;
        let oy = st.offset.y;
        Margins::new(
            (blur - ox).max(0.0),
            (blur - oy).max(0.0),
            (blur + ox).max(0.0),
            (blur + oy).max(0.0),
        )
    }

    /// Outer margins of this shadow.
    pub fn extend(&self) -> Margins {
        Self::extend_for(&self.style)
    }

    /// Paint the 9-slice blurred outer shadow around `box_rect` with `corner_radius`.
    pub fn paint(
        &self,
        ctx: &egui::Context,
        painter: &Painter,
        box_rect: Rect,
        corner_radius: f32,
    ) {
        self.paint_with_opacity(ctx, painter, box_rect, corner_radius, self.style.opacity);
    }

    /// Paint the 9-slice shadow with a custom opacity multiplier (e.g. during fade animation).
    pub fn paint_with_opacity(
        &self,
        ctx: &egui::Context,
        painter: &Painter,
        box_rect: Rect,
        corner_radius: f32,
        opacity: f32,
    ) {
        if opacity <= 0.001 || self.style.blur_radius <= 0.0 {
            return;
        }
        let cache_key = Id::unique((
            "tg_box_shadow",
            (self.style.blur_radius * 10.0).round() as i32,
            (self.style.offset.x * 10.0).round() as i32,
            (self.style.offset.y * 10.0).round() as i32,
            (corner_radius * 10.0).round() as i32,
        ));

        let entry = ctx
            .data_mut(|d| d.get_temp::<BoxShadowTextureEntry>(cache_key))
            .unwrap_or_else(|| {
                let built = self.build_texture_entry(ctx, corner_radius);
                ctx.data_mut(|d| d.insert_temp(cache_key, built.clone()));
                built
            });

        let ext = self.extend();
        let outer = Rect::from_min_max(
            Pos2::new(box_rect.min.x - ext.left, box_rect.min.y - ext.top),
            Pos2::new(box_rect.max.x + ext.right, box_rect.max.y + ext.bottom),
        );

        let cl_l = entry.corner_l;
        let cl_t = entry.corner_t;
        let cl_r = entry.corner_r;
        let cl_b = entry.corner_b;
        let cw = (outer.width() - cl_l - cl_r).max(0.0);
        let ch = (outer.height() - cl_t - cl_b).max(0.0);

        let tint = Color32::from_rgba_unmultiplied(
            0,
            0,
            0,
            (opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
        );

        let u0 = 0.0;
        let u1 = cl_l / entry.tex_w;
        let u2 = (cl_l + entry.middle) / entry.tex_w;
        let u3 = 1.0;

        let v0 = 0.0;
        let v1 = cl_t / entry.tex_h;
        let v2 = (cl_t + entry.middle) / entry.tex_h;
        let v3 = 1.0;

        let x0 = outer.min.x;
        let x1 = x0 + cl_l;
        let x2 = x1 + cw;
        let x3 = x2 + cl_r;

        let y0 = outer.min.y;
        let y1 = y0 + cl_t;
        let y2 = y1 + ch;
        let y3 = y2 + cl_b;

        let mut mesh = Mesh::with_texture(entry.texture.id());
        let mut push_quad = |dst: Rect, uv: Rect| {
            if dst.width() <= 0.0 || dst.height() <= 0.0 {
                return;
            }
            mesh.add_rect_with_uv(dst, uv, tint);
        };

        // 4 corners
        push_quad(
            Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x1, y1)),
            Rect::from_min_max(Pos2::new(u0, v0), Pos2::new(u1, v1)),
        );
        push_quad(
            Rect::from_min_max(Pos2::new(x2, y0), Pos2::new(x3, y1)),
            Rect::from_min_max(Pos2::new(u2, v0), Pos2::new(u3, v1)),
        );
        push_quad(
            Rect::from_min_max(Pos2::new(x0, y2), Pos2::new(x1, y3)),
            Rect::from_min_max(Pos2::new(u0, v2), Pos2::new(u1, v3)),
        );
        push_quad(
            Rect::from_min_max(Pos2::new(x2, y2), Pos2::new(x3, y3)),
            Rect::from_min_max(Pos2::new(u2, v2), Pos2::new(u3, v3)),
        );
        // 4 sides
        push_quad(
            Rect::from_min_max(Pos2::new(x1, y0), Pos2::new(x2, y1)),
            Rect::from_min_max(Pos2::new(u1, v0), Pos2::new(u2, v1)),
        );
        push_quad(
            Rect::from_min_max(Pos2::new(x1, y2), Pos2::new(x2, y3)),
            Rect::from_min_max(Pos2::new(u1, v2), Pos2::new(u2, v3)),
        );
        push_quad(
            Rect::from_min_max(Pos2::new(x0, y1), Pos2::new(x1, y2)),
            Rect::from_min_max(Pos2::new(u0, v1), Pos2::new(u1, v2)),
        );
        push_quad(
            Rect::from_min_max(Pos2::new(x2, y1), Pos2::new(x3, y2)),
            Rect::from_min_max(Pos2::new(u2, v1), Pos2::new(u3, v2)),
        );

        painter.add(Shape::mesh(mesh));
    }

    fn build_texture_entry(
        &self,
        ctx: &egui::Context,
        corner_radius: f32,
    ) -> BoxShadowTextureEntry {
        let blur = self.style.blur_radius.round().max(1.0);
        let ext = self.extend();
        let cr = corner_radius.max(0.0).round();
        let middle = 2.0_f32;
        let corner_l = ext.left + cr;
        let corner_t = ext.top + cr;
        let corner_r = ext.right + cr;
        let corner_b = ext.bottom + cr;
        let body = 2.0 * cr + middle;
        let w = (corner_l + middle + corner_r).ceil().max(4.0) as usize;
        let h = (corner_t + middle + corner_b).ceil().max(4.0) as usize;

        // Rasterize rounded-rect alpha mask at (blur, blur) matching shadow.cpp.
        let mut mask = vec![0.0_f32; w * h];
        let shape_x0 = blur;
        let shape_y0 = blur;
        let shape_x1 = blur + body;
        let shape_y1 = blur + body;

        for y in 0..h {
            for x in 0..w {
                let d = sdf_round_rect(
                    x as f32 + 0.5,
                    y as f32 + 0.5,
                    shape_x0,
                    shape_y0,
                    shape_x1,
                    shape_y1,
                    cr,
                );
                mask[y * w + x] = (0.5 - d).clamp(0.0, 1.0);
            }
        }

        // Three-pass box blur approximates Gaussian blur cleanly.
        let radius_px = (blur * 0.55).round().max(1.0) as usize;
        box_blur_alpha(&mut mask, w, h, radius_px);
        box_blur_alpha(&mut mask, w, h, radius_px);
        box_blur_alpha(&mut mask, w, h, radius_px);

        // Cut out the interior body rect shifted by -offset (DestinationOut).
        let cut_x0 = ext.left;
        let cut_y0 = ext.top;
        let cut_x1 = ext.left + body;
        let cut_y1 = ext.top + body;

        let mut pixels = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let d_cut = sdf_round_rect(
                    x as f32 + 0.5,
                    y as f32 + 0.5,
                    cut_x0,
                    cut_y0,
                    cut_x1,
                    cut_y1,
                    cr,
                );
                let inside = (0.5 - d_cut).clamp(0.0, 1.0);
                let a = (mask[y * w + x] * (1.0 - inside)).clamp(0.0, 1.0);
                let alpha = (a * 255.0).round() as u8;
                // White with alpha so vertex tint controls shadow color/opacity.
                pixels.push(Color32::from_rgba_unmultiplied(255, 255, 255, alpha));
            }
        }

        let image = ColorImage {
            size: [w, h],
            source_size: Vec2::new(w as f32, h as f32),
            pixels,
        };
        let texture = ctx.load_texture(
            format!("tg_shadow_{blur}_{cr}"),
            image,
            TextureOptions::LINEAR,
        );

        BoxShadowTextureEntry {
            texture,
            corner_l,
            corner_t,
            corner_r,
            corner_b,
            middle,
            tex_w: w as f32,
            tex_h: h as f32,
        }
    }
}

/// Signed distance to a rounded rectangle (negative inside, positive outside).
pub fn sdf_round_rect(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, radius: f32) -> f32 {
    let cx = (x0 + x1) * 0.5;
    let cy = (y0 + y1) * 0.5;
    let hx = ((x1 - x0) * 0.5 - radius).max(0.0);
    let hy = ((y1 - y0) * 0.5 - radius).max(0.0);
    let dx = (px - cx).abs() - hx;
    let dy = (py - cy).abs() - hy;
    let outside = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt();
    let inside = dx.max(dy).min(0.0);
    outside + inside - radius
}

fn box_blur_alpha(buf: &mut [f32], w: usize, h: usize, radius: usize) {
    if radius == 0 || w == 0 || h == 0 {
        return;
    }
    let mut tmp = vec![0.0_f32; w * h];
    let diam = (2 * radius + 1) as f32;

    // Horizontal pass.
    for y in 0..h {
        let row = y * w;
        for x in 0..w {
            let mut sum = 0.0;
            for k in -(radius as isize)..=(radius as isize) {
                let sx = (x as isize + k).clamp(0, w as isize - 1) as usize;
                sum += buf[row + sx];
            }
            tmp[row + x] = sum / diam;
        }
    }

    // Vertical pass.
    for x in 0..w {
        for y in 0..h {
            let mut sum = 0.0;
            for k in -(radius as isize)..=(radius as isize) {
                let sy = (y as isize + k).clamp(0, h as isize - 1) as usize;
                sum += tmp[sy * w + x];
            }
            buf[y * w + x] = sum / diam;
        }
    }
}

/// Image rounding helpers mirroring `Telegram/lib_ui/ui/image/image_prepare.cpp`.
pub mod image_prepare {
    use egui::{Color32, ColorImage, Vec2};

    use super::sdf_round_rect;

    /// Apply an anti-aliased circular alpha mask to a square `ColorImage`.
    pub fn round_circle(mut img: ColorImage) -> ColorImage {
        let [w, h] = img.size;
        if w == 0 || h == 0 {
            return img;
        }
        let cx = w as f32 * 0.5;
        let cy = h as f32 * 0.5;
        let r = cx.min(cy);
        for y in 0..h {
            for x in 0..w {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                let d = (dx * dx + dy * dy).sqrt() - r;
                let cov = (0.5 - d).clamp(0.0, 1.0);
                let px = &mut img.pixels[y * w + x];
                let [pr, pg, pb, pa] = px.to_srgba_unmultiplied();
                let na = (f32::from(pa) * cov).round() as u8;
                *px = Color32::from_rgba_unmultiplied(pr, pg, pb, na);
            }
        }
        img
    }

    /// Apply an anti-aliased rounded-rectangle mask with radius `radius` to `img`.
    pub fn round_corners(mut img: ColorImage, radius: f32) -> ColorImage {
        let [w, h] = img.size;
        if w == 0 || h == 0 || radius <= 0.0 {
            return img;
        }
        let r = radius.min(w.min(h) as f32 * 0.5);
        for y in 0..h {
            for x in 0..w {
                let d = sdf_round_rect(
                    x as f32 + 0.5,
                    y as f32 + 0.5,
                    0.0,
                    0.0,
                    w as f32,
                    h as f32,
                    r,
                );
                let cov = (0.5 - d).clamp(0.0, 1.0);
                let px = &mut img.pixels[y * w + x];
                let [pr, pg, pb, pa] = px.to_srgba_unmultiplied();
                let na = (f32::from(pa) * cov).round() as u8;
                *px = Color32::from_rgba_unmultiplied(pr, pg, pb, na);
            }
        }
        img
    }

    /// Generate a vertical gradient circular `ColorImage` of `size x size`.
    pub fn vertical_gradient_circle(size: usize, top: Color32, bottom: Color32) -> ColorImage {
        let size = size.max(1);
        let mut pixels = Vec::with_capacity(size * size);
        for y in 0..size {
            let t = if size > 1 {
                y as f64 / (size - 1) as f64
            } else {
                0.0
            };
            let c = crate::anim::color_lerp(top, bottom, t);
            for _ in 0..size {
                pixels.push(c);
            }
        }
        round_circle(ColorImage {
            size: [size, size],
            source_size: Vec2::new(size as f32, size as f32),
            pixels,
        })
    }
}

/// Tracks animated hover (`over`) factor for buttons/widgets (`AbstractButton` / `RpWidget` helper).
#[derive(Debug, Clone, Copy, Default)]
pub struct HoverAnimState {
    over: bool,
    anim: SimpleAnimation,
}

impl HoverAnimState {
    /// Update hover animation and return the current factor in `[0.0, 1.0]`.
    pub fn update(
        ctx: &egui::Context,
        id: Id,
        is_over: bool,
        duration_ms: u32,
        transition: Transition,
    ) -> f64 {
        let now = ctx.input(|i| i.time);
        let key = id.with("_tg_hover_anim");
        let mut state = ctx
            .data_mut(|d| d.get_temp::<Self>(key))
            .unwrap_or_default();

        if state.over != is_over {
            state.over = is_over;
            if duration_ms > 0 {
                state.anim.change(
                    now,
                    if is_over { 1.0 } else { 0.0 },
                    duration_ms,
                    transition,
                );
            } else {
                state.anim.stop();
            }
        }
        let val = state.anim.sample(now, if state.over { 1.0 } else { 0.0 });
        if state.anim.animating() {
            ctx.request_repaint();
        }
        ctx.data_mut(|d| d.insert_temp(key, state));
        val
    }
}

/// Paint a crisp 1px horizontal divider / plain shadow (`PlainShadow`).
pub fn paint_plain_shadow(painter: &Painter, rect: Rect, color: Color32) {
    painter.rect_filled(rect, CornerRadius::ZERO, color);
}

/// Paint a rounded border stroke cleanly inside `rect`.
pub fn paint_rounded_border(painter: &Painter, rect: Rect, radius: f32, stroke: Stroke) {
    painter.rect_stroke(
        rect,
        CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8),
        stroke,
        StrokeKind::Inside,
    );
}
