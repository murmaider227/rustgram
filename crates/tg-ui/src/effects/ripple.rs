//! Press ripple animation effect.
//!
//! Port of `Telegram/lib_ui/ui/effects/ripple_animation.h` and
//! `Telegram/lib_ui/ui/effects/ripple_animation.cpp`.

use std::collections::VecDeque;

use egui::{Color32, CornerRadius, Id, Painter, Pos2, Rect, Vec2};

use crate::anim::{SimpleAnimation, Transition, interpolate_f32, with_alpha};
use crate::style::RippleAnimationStyle;

/// Mask shape used to clip expanding ripple circles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RippleMask {
    /// Axis-aligned rectangle.
    Rect,
    /// Rounded rectangle with uniform corner radius.
    RoundRect(f32),
    /// Pill / ellipse inscribed in the mask rect.
    Ellipse,
}

#[derive(Debug, Clone)]
struct SingleRipple {
    /// Origin relative to mask top-left.
    origin: Vec2,
    radius_from: f32,
    radius_to: f32,
    hiding: bool,
    show: SimpleAnimation,
    hide: SimpleAnimation,
}

impl SingleRipple {
    fn new(
        origin: Vec2,
        start_radius: f32,
        mask_size: Vec2,
        style: &RippleAnimationStyle,
        now_sec: f64,
    ) -> Self {
        let corners = [
            Vec2::new(0.0, 0.0),
            Vec2::new(mask_size.x, 0.0),
            Vec2::new(mask_size.x, mask_size.y),
            Vec2::new(0.0, mask_size.y),
        ];
        let mut max_dist_sq: f32 = 0.0;
        for c in corners {
            let d = origin - c;
            max_dist_sq = max_dist_sq.max(d.x * d.x + d.y * d.y);
        }
        let radius_to = max_dist_sq.sqrt().round();

        let mut show = SimpleAnimation::new();
        show.start(
            now_sec,
            0.0,
            1.0,
            style.show_duration_ms,
            Transition::EaseOutQuint,
        );

        Self {
            origin,
            radius_from: start_radius,
            radius_to,
            hiding: false,
            show,
            hide: SimpleAnimation::new(),
        }
    }

    fn new_fading(mask_size: Vec2, style: &RippleAnimationStyle, now_sec: f64) -> Self {
        let r = mask_size.x + mask_size.y;
        let mut hide = SimpleAnimation::new();
        hide.start(
            now_sec,
            0.0,
            1.0,
            style.hide_duration_ms,
            Transition::Linear,
        );
        Self {
            origin: mask_size * 0.5,
            radius_from: r,
            radius_to: r,
            hiding: false,
            show: SimpleAnimation::new(),
            hide,
        }
    }

    fn stop(&mut self, style: &RippleAnimationStyle, now_sec: f64) {
        if !self.hiding {
            self.hiding = true;
            self.hide.start(
                now_sec,
                1.0,
                0.0,
                style.hide_duration_ms,
                Transition::Linear,
            );
        }
    }

    fn unstop(&mut self, style: &RippleAnimationStyle, now_sec: f64) {
        if self.hiding {
            if self.hide.animating() {
                self.hide
                    .change(now_sec, 1.0, style.hide_duration_ms, Transition::Linear);
            }
            self.hiding = false;
        }
    }

    fn finished(&self) -> bool {
        self.hiding && !self.hide.animating()
    }
}

/// Manages active expanding/fading ripples for a widget.
#[derive(Debug, Clone, Default)]
pub struct RippleState {
    ripples: VecDeque<SingleRipple>,
    was_down: bool,
}

impl RippleState {
    /// Create an empty ripple state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a new expanding ripple from `origin` (relative to `mask_rect.min`).
    pub fn add(
        &mut self,
        origin: Vec2,
        start_radius: f32,
        mask_size: Vec2,
        style: &RippleAnimationStyle,
        now_sec: f64,
    ) {
        if style.show_duration_ms == 0 {
            return;
        }
        self.last_stop(style, now_sec);
        self.ripples.push_back(SingleRipple::new(
            origin,
            start_radius,
            mask_size,
            style,
            now_sec,
        ));
    }

    /// Start a full-area fading ripple (used for `forceRippled`).
    pub fn add_fading(&mut self, mask_size: Vec2, style: &RippleAnimationStyle, now_sec: f64) {
        if style.hide_duration_ms == 0 {
            return;
        }
        self.last_stop(style, now_sec);
        self.ripples
            .push_back(SingleRipple::new_fading(mask_size, style, now_sec));
    }

    /// Begin fading out the most recent ripple.
    pub fn last_stop(&mut self, style: &RippleAnimationStyle, now_sec: f64) {
        if let Some(last) = self.ripples.back_mut() {
            last.stop(style, now_sec);
        }
    }

    /// Resume the most recent ripple if it is fading out.
    pub fn last_unstop(&mut self, style: &RippleAnimationStyle, now_sec: f64) {
        if let Some(last) = self.ripples.back_mut() {
            last.unstop(style, now_sec);
        }
    }

    /// Remove all ripples immediately.
    pub fn clear(&mut self) {
        self.ripples.clear();
    }

    /// Whether no ripples are currently active.
    pub fn is_empty(&self) -> bool {
        self.ripples.is_empty()
    }

    /// Synchronize with an egui widget press state (starts on press down, fades on release).
    pub fn sync_press(
        &mut self,
        is_down: bool,
        pointer_pos: Option<Pos2>,
        mask_rect: Rect,
        style: &RippleAnimationStyle,
        now_sec: f64,
    ) {
        if is_down && !self.was_down {
            let origin = pointer_pos
                .map(|p| p - mask_rect.min)
                .unwrap_or_else(|| mask_rect.size() * 0.5);
            self.add(origin, 0.0, mask_rect.size(), style, now_sec);
        } else if !is_down && self.was_down {
            self.last_stop(style, now_sec);
        }
        self.was_down = is_down;
    }

    /// Advance and paint all active ripples clipped to `mask_rect` and `mask`.
    /// Returns `true` if another frame should be requested.
    pub fn paint(
        &mut self,
        painter: &Painter,
        mask_rect: Rect,
        mask: RippleMask,
        style: &RippleAnimationStyle,
        color_override: Option<Color32>,
        now_sec: f64,
    ) -> bool {
        if self.ripples.is_empty() {
            return false;
        }
        let base_color = color_override.unwrap_or(style.color);
        let clipped = painter.with_clip_rect(painter.clip_rect().intersect(mask_rect));
        let mut needs_repaint = false;

        for ripple in &mut self.ripples {
            let opacity = ripple
                .hide
                .sample(now_sec, if ripple.hiding { 0.0 } else { 1.0 });
            let shown = ripple.show.sample(now_sec, 1.0);
            if ripple.show.animating() || ripple.hide.animating() {
                needs_repaint = true;
            }
            if opacity <= 0.0 {
                continue;
            }
            let radius = interpolate_f32(ripple.radius_from, ripple.radius_to, shown as f32);
            let center = mask_rect.min + ripple.origin;
            let color = with_alpha(base_color, opacity);

            match mask {
                RippleMask::Rect => {
                    clipped.circle_filled(center, radius, color);
                }
                RippleMask::RoundRect(r) => {
                    let cr = r.min(mask_rect.width() * 0.5).min(mask_rect.height() * 0.5);
                    if radius >= ripple.radius_to - 0.5 {
                        clipped.rect_filled(mask_rect, CornerRadius::same(cr.round() as u8), color);
                    } else {
                        paint_clipped_circle_in_round_rect(
                            &clipped, mask_rect, cr, center, radius, color,
                        );
                    }
                }
                RippleMask::Ellipse => {
                    let max_r = mask_rect.width().min(mask_rect.height()) * 0.5;
                    let draw_r = radius.min(max_r);
                    // Keep circle inside the ellipse bounds by clamping center shift.
                    let ellipse_center = mask_rect.center();
                    let delta = center - ellipse_center;
                    let max_shift = (max_r - draw_r).max(0.0);
                    let len = delta.length();
                    let clamped_center = if len > max_shift && len > 0.001 {
                        ellipse_center + delta * (max_shift / len)
                    } else {
                        center
                    };
                    clipped.circle_filled(clamped_center, draw_r, color);
                }
            }
        }

        while self.ripples.front().is_some_and(SingleRipple::finished) {
            self.ripples.pop_front();
        }
        needs_repaint || !self.ripples.is_empty()
    }
}

/// Approximate circle clipped to a rounded rectangle via polygon intersection.
fn paint_clipped_circle_in_round_rect(
    painter: &Painter,
    rect: Rect,
    radius: f32,
    center: Pos2,
    circle_r: f32,
    color: Color32,
) {
    if circle_r <= 0.5 {
        return;
    }
    const SEGMENTS: usize = 36;
    let mut points = Vec::with_capacity(SEGMENTS);
    for i in 0..SEGMENTS {
        let angle = (i as f32 / SEGMENTS as f32) * std::f32::consts::TAU;
        let raw = Pos2::new(
            center.x + circle_r * angle.cos(),
            center.y + circle_r * angle.sin(),
        );
        points.push(clamp_point_to_round_rect(raw, rect, radius));
    }
    painter.add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
}

/// Clamp a point to the boundary of a rounded rectangle.
pub fn clamp_point_to_round_rect(p: Pos2, rect: Rect, radius: f32) -> Pos2 {
    let r = radius.clamp(0.0, rect.width().min(rect.height()) * 0.5);
    let mut x = p.x.clamp(rect.min.x, rect.max.x);
    let mut y = p.y.clamp(rect.min.y, rect.max.y);
    if r <= 0.0 {
        return Pos2::new(x, y);
    }

    let inner_min_x = rect.min.x + r;
    let inner_max_x = rect.max.x - r;
    let inner_min_y = rect.min.y + r;
    let inner_max_y = rect.max.y - r;

    let corner = if x < inner_min_x && y < inner_min_y {
        Some(Pos2::new(inner_min_x, inner_min_y))
    } else if x > inner_max_x && y < inner_min_y {
        Some(Pos2::new(inner_max_x, inner_min_y))
    } else if x < inner_min_x && y > inner_max_y {
        Some(Pos2::new(inner_min_x, inner_max_y))
    } else if x > inner_max_x && y > inner_max_y {
        Some(Pos2::new(inner_max_x, inner_max_y))
    } else {
        None
    };

    if let Some(c) = corner {
        let dx = x - c.x;
        let dy = y - c.y;
        let dist = (dx * dx + dy * dy).sqrt();
        if dist > r && dist > 0.0001 {
            x = c.x + dx * (r / dist);
            y = c.y + dy * (r / dist);
        }
    }
    Pos2::new(x, y)
}

/// Convenience helper storing [`RippleState`] in egui temporary memory keyed by `id`.
pub fn update_and_paint_ripple(
    painter: &Painter,
    id: Id,
    mask_rect: Rect,
    mask: RippleMask,
    style: &RippleAnimationStyle,
    is_down: bool,
    color_override: Option<Color32>,
) {
    let ctx = painter.ctx();
    let now = ctx.input(|i| i.time);
    let pointer_pos = ctx.pointer_interact_pos();
    let ripple_id = id.with("_tg_ripple");
    let mut state = ctx
        .data_mut(|d| d.get_temp::<RippleState>(ripple_id))
        .unwrap_or_default();

    state.sync_press(is_down, pointer_pos, mask_rect, style, now);
    let active = state.paint(painter, mask_rect, mask, style, color_override, now);
    if active {
        ctx.request_repaint();
    }
    ctx.data_mut(|d| d.insert_temp(ripple_id, state));
}
