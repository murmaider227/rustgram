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

/// `st::radialDuration` (`lib_ui/ui/basic.style`): arc catch-up and fade time, in ms.
pub const RADIAL_DURATION_MS: f64 = 350.0;
/// `st::radialPeriod` (`lib_ui/ui/basic.style`): one full turn of the arc start, in ms.
pub const RADIAL_PERIOD_MS: f64 = 3000.0;
/// `arc::kFullLength`: Qt arc units (1/16 degree) in a full circle.
const ARC_FULL: f64 = 360.0 * 16.0;
const ARC_QUARTER: f64 = ARC_FULL / 4.0;
const ARC_MIN: f64 = ARC_FULL / 360.0;
const ARC_ALMOST_FULL: f64 = ARC_FULL - ARC_MIN;

/// `anim::value` with linear interpolation: `current` moves from `from` to `to`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct ArcValue {
    from: f64,
    to: f64,
    current: f64,
}

impl ArcValue {
    const fn new(from: f64, to: f64) -> Self {
        Self {
            from,
            to,
            current: from,
        }
    }

    const fn start(&mut self, to: f64) {
        self.from = self.current;
        self.to = to;
    }

    fn update(&mut self, dt: f64) {
        self.current = (self.to - self.from).mul_add(dt, self.from);
    }
}

/// Determinate progress arc that also turns while it waits for progress
/// (`Ui::RadialAnimation`, `lib_ui/ui/effects/radial_animation.cpp`).
///
/// The owner calls [`Self::update`] every frame while [`Self::animating`] and repaints: the arc
/// start turns once per [`RADIAL_PERIOD_MS`], the arc end eases toward the progress, and after
/// the load finishes the arc completes and fades out over [`RADIAL_DURATION_MS`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RadialAnimation {
    first_start_ms: f64,
    last_start_ms: f64,
    last_time_ms: f64,
    arc_end: ArcValue,
    arc_start: f64,
    opacity: f64,
    finished: bool,
    animating: bool,
}

impl RadialAnimation {
    /// `RadialAnimation::start(prg)`.
    pub fn start(&mut self, progress: f64, now_ms: f64) {
        self.first_start_ms = now_ms;
        self.last_start_ms = now_ms;
        self.last_time_ms = now_ms;
        let iprg = (progress.max(0.0001) * ARC_ALMOST_FULL).round();
        let strict = (progress * ARC_ALMOST_FULL).round();
        self.arc_end = ArcValue::new(strict, iprg);
        self.arc_start = 0.0;
        self.opacity = 0.0;
        self.finished = false;
        self.animating = true;
    }

    /// `RadialAnimation::update(prg, finished, ms)`; returns whether the target changed.
    pub fn update(&mut self, progress: f64, finished: bool, now_ms: f64) -> bool {
        let iprg = (progress.max(0.0001) * ARC_ALMOST_FULL).round();
        let result = iprg != self.arc_end.to.round() || self.finished != finished;
        if self.finished != finished {
            self.arc_end.start(iprg);
            self.finished = finished;
            self.last_start_ms = self.last_time_ms;
        } else if result {
            self.arc_end.start(iprg);
            self.last_start_ms = self.last_time_ms;
        }
        self.last_time_ms = now_ms;

        let dt = now_ms - self.last_start_ms;
        let fulldt = now_ms - self.first_start_ms;
        let opacitydt = if self.finished {
            self.last_start_ms - self.first_start_ms
        } else {
            fulldt
        };
        self.opacity = (opacitydt / RADIAL_DURATION_MS).min(1.0);
        if disabled() {
            self.arc_end.update(1.0);
            if finished {
                self.stop();
            }
        } else if !finished {
            self.arc_end
                .update(1.0 - RADIAL_DURATION_MS / (RADIAL_DURATION_MS + dt));
        } else if dt >= RADIAL_DURATION_MS {
            self.arc_end.update(1.0);
            self.stop();
        } else {
            let r = dt / RADIAL_DURATION_MS;
            self.arc_end.update(r);
            self.opacity *= 1.0 - r;
        }
        let fromstart = fulldt / RADIAL_PERIOD_MS;
        self.arc_start = fromstart - fromstart.floor();
        result
    }

    /// `RadialAnimation::stop()`.
    pub fn stop(&mut self) {
        *self = Self::default();
    }

    /// Whether the arc is still shown and needs frames.
    #[must_use]
    pub const fn animating(&self) -> bool {
        self.animating
    }

    /// Current opacity (fades in on start and out after the finish).
    #[must_use]
    pub const fn opacity(&self) -> f64 {
        self.opacity
    }

    /// `RadialAnimation::computeState()` in radians for [`paint_arc`] (screen angles, y down).
    #[must_use]
    pub fn compute_state(&self) -> RadialState {
        let length = ARC_MIN + self.arc_end.current.round();
        let shift = if disabled() {
            0.0
        } else {
            (self.arc_start * ARC_FULL).round()
        };
        let from = ARC_QUARTER - length - shift;
        let to_rad = |units: f64| units / ARC_FULL * TAU;
        // Qt draws from `from` counter-clockwise (y up) by `length`; on screen (y down) that is
        // the clockwise sweep from `-(from + length)` to `-from`.
        RadialState {
            shown: self.opacity,
            arc_from_rad: -to_rad(from + length),
            arc_length_rad: to_rad(length),
        }
    }

    /// `RadialAnimation::draw`: paints the arc inside `rect` with the current opacity.
    pub fn draw(&self, painter: &Painter, rect: Rect, thickness: f32, color: Color32) {
        let state = self.compute_state();
        paint_arc(
            painter,
            rect,
            state.arc_from_rad,
            state.arc_length_rad,
            thickness,
            with_alpha(color, state.shown),
        );
    }
}

#[cfg(test)]
mod determinate_tests {
    use super::*;

    #[test]
    fn the_arc_turns_while_progress_does_not_change() {
        let mut radial = RadialAnimation::default();
        radial.start(0.3, 1000.0);
        radial.update(0.3, false, 1500.0);
        let a = radial.compute_state();
        radial.update(0.3, false, 2000.0);
        let b = radial.compute_state();
        assert!(radial.animating());
        assert!(
            (a.arc_from_rad - b.arc_from_rad).abs() > 0.5,
            "the start turns 60 degrees in 500 ms: {} vs {}",
            a.arc_from_rad,
            b.arc_from_rad
        );
        assert!((a.arc_length_rad - b.arc_length_rad).abs() < 0.05);
    }

    #[test]
    fn the_arc_eases_toward_progress_and_fades_out_after_the_finish() {
        let mut radial = RadialAnimation::default();
        radial.start(0.0, 0.0);
        radial.update(0.0, false, 400.0);
        assert!((radial.opacity() - 1.0).abs() < f64::EPSILON);
        radial.update(0.5, false, 450.0);
        radial.update(0.5, false, 2000.0);
        let half = radial.compute_state().arc_length_rad;
        assert!(
            half > PI * 0.7 && half < PI * 1.05,
            "eased to ~half: {half}"
        );
        radial.update(1.0, true, 2100.0);
        radial.update(1.0, true, 2200.0);
        assert!(radial.animating() && radial.opacity() < 1.0);
        radial.update(1.0, true, 2100.0 + RADIAL_DURATION_MS);
        assert!(!radial.animating(), "stops after radialDuration");
    }
}
