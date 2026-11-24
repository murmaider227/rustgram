//! Determinate and infinite radial progress spinners.
//!
//! Port of `Telegram/lib_ui/ui/effects/radial_animation.h` and
//! `Telegram/lib_ui/ui/effects/radial_animation.cpp`.

use std::f64::consts::{PI, TAU};

use egui::{Color32, Painter, Pos2, Rect, Stroke, Vec2};

use crate::anim::{disabled, sine_in_out, with_alpha};
use crate::style::InfiniteRadialAnimationStyle;

/// Computed arc geometry in radians for radial painting.
#[derive(Debug, Clone, Copy)]
pub struct RadialState {
    /// Opacity factor in `[0.0, 1.0]`.
    pub shown: f64,
    /// Starting angle in radians.
    pub arc_from_rad: f64,
    /// Arc sweep length in radians.
    pub arc_length_rad: f64,
}

/// Infinite Material/tdesktop-style spinning arc animation.
#[derive(Debug, Clone, Copy)]
pub struct InfiniteRadialAnimation {
    style: InfiniteRadialAnimationStyle,
    started_sec: Option<f64>,
}

impl InfiniteRadialAnimation {
    /// Create a new infinite radial animation with `style`.
    pub const fn new(style: InfiniteRadialAnimationStyle) -> Self {
        Self {
            style,
            started_sec: None,
        }
    }

    /// Start spinning at `now_sec`.
    pub fn start(&mut self, now_sec: f64) {
        if self.started_sec.is_none() {
            self.started_sec = Some(now_sec);
        }
    }

    /// Stop spinning.
    pub fn stop(&mut self) {
        self.started_sec = None;
    }

    /// Compute the current arc angles at `now_sec`, matching tdesktop's
    /// `linearPeriod` + `sinePeriod` + `sineDuration` + `sineShift` formula.
    pub fn compute_state(&self, now_sec: f64) -> RadialState {
        let now_ms = (now_sec.max(0.0) * 1000.0) as u64;
        let linear_period = u64::from(self.style.linear_period_ms.max(1));
        let sine_period = u64::from(self.style.sine_period_ms.max(1));
        let sine_duration = u64::from(self.style.sine_duration_ms.max(1));
        let sine_shift = u64::from(self.style.sine_shift_ms);

        let linear_frac = (now_ms % linear_period) as f64 / linear_period as f64;
        let sine_phase = now_ms % sine_period;

        let arc_min = self.style.arc_min;
        let arc_max = self.style.arc_max;
        let delta = arc_max - arc_min;

        let (extra_rot, length_frac) = if sine_phase < sine_duration {
            let dt = sine_phase as f64 / sine_duration as f64;
            let grow = sine_in_out(delta, dt);
            (0.0, arc_min + grow)
        } else if sine_phase < sine_shift {
            (0.0, arc_max)
        } else if sine_phase < sine_shift + sine_duration {
            let dt = (sine_phase - sine_shift) as f64 / sine_duration as f64;
            let shrink = sine_in_out(delta, dt);
            (shrink, arc_max - shrink)
        } else {
            (delta, arc_min)
        };

        let start_frac = (linear_frac + extra_rot) % 1.0;
        RadialState {
            shown: 1.0,
            arc_from_rad: start_frac * TAU - PI * 0.5,
            arc_length_rad: (length_frac * TAU).max(0.1),
        }
    }

    /// Draw the spinner inside `rect` and request repaint if animated.
    pub fn paint(&self, ctx: &egui::Context, painter: &Painter, rect: Rect) {
        let now = ctx.input(|i| i.time);
        if disabled() {
            paint_static_loading(painter, rect, self.style.thickness, self.style.color);
            return;
        }
        let state = self.compute_state(now);
        paint_arc(
            painter,
            rect,
            state.arc_from_rad,
            state.arc_length_rad,
            self.style.thickness,
            with_alpha(self.style.color, state.shown),
        );
        ctx.request_repaint();
    }
}

/// Paint a circular arc inside `rect` using a smooth polyline.
pub fn paint_arc(
    painter: &Painter,
    rect: Rect,
    start_rad: f64,
    sweep_rad: f64,
    thickness: f32,
    color: Color32,
) {
    let radius = (rect.width().min(rect.height()) - thickness).max(1.0) * 0.5;
    let center = rect.center();
    let steps = ((sweep_rad.abs() / TAU) * 48.0).ceil().clamp(6.0, 64.0) as usize;
    let mut pts = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let a = start_rad + sweep_rad * (i as f64 / steps as f64);
        pts.push(Pos2::new(
            center.x + radius * a.cos() as f32,
            center.y + radius * a.sin() as f32,
        ));
    }
    painter.add(egui::Shape::line(pts, Stroke::new(thickness, color)));
    // Round caps at endpoints to match Qt::RoundCap.
    let cap_r = thickness * 0.5;
    if let (Some(&first), Some(&last)) = (
        // recompute endpoints cleanly
        Some(&Pos2::new(
            center.x + radius * start_rad.cos() as f32,
            center.y + radius * start_rad.sin() as f32,
        )),
        Some(&Pos2::new(
            center.x + radius * (start_rad + sweep_rad).cos() as f32,
            center.y + radius * (start_rad + sweep_rad).sin() as f32,
        )),
    ) {
        painter.circle_filled(first, cap_r, color);
        painter.circle_filled(last, cap_r, color);
    }
}

/// Port of `anim::DrawStaticLoading` (clock icon used when animations are disabled).
pub fn paint_static_loading(painter: &Painter, rect: Rect, stroke: f32, color: Color32) {
    let radius = (rect.width().min(rect.height()) - stroke).max(1.0) * 0.5;
    let center = rect.center();
    painter.circle_stroke(center, radius, Stroke::new(stroke, color));
    let first = Pos2::new(center.x, rect.min.y + 1.5 * stroke);
    let delta = center.y - first.y;
    if delta > 0.0 {
        let second = center + Vec2::new(delta * 2.0 / 3.0, 0.0);
        painter.add(egui::Shape::line(
            vec![first, center, second],
            Stroke::new(stroke, color),
        ));
    }
}
