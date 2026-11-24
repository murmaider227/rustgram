//! Animation value interpolation and easing functions.
//!
//! Port of `Telegram/lib_ui/ui/effects/animation_value.{h,cpp}` and
//! `Telegram/lib_ui/ui/effects/animation_value_f.h`.

use std::f64::consts::PI;

/// Easing transition function mapping `(delta, dt)` to an interpolated offset.
pub type Transition = fn(f64, f64) -> f64;

/// Linear transition: `delta * dt`.
#[must_use]
pub fn linear(delta: f64, dt: f64) -> f64 {
    delta * dt
}

/// Sine ease-in-out transition: `-(delta / 2) * (cos(pi * dt) - 1)`.
#[must_use]
pub fn sine_in_out(delta: f64, dt: f64) -> f64 {
    -(delta / 2.0) * ((PI * dt).cos() - 1.0)
}

/// Cubic ease-in transition: `delta * dt^3`.
#[must_use]
pub fn ease_in_cubic(delta: f64, dt: f64) -> f64 {
    delta * dt * dt * dt
}

/// Cubic ease-out transition: `delta * ((dt - 1)^3 + 1)`.
#[must_use]
pub fn ease_out_cubic(delta: f64, dt: f64) -> f64 {
    let t = dt - 1.0;
    delta * (t * t * t + 1.0)
}

/// Circular ease-out transition: `delta * sqrt(1 - (dt - 1)^2)`.
#[must_use]
pub fn ease_out_circ(delta: f64, dt: f64) -> f64 {
    let t = dt - 1.0;
    delta * (1.0 - t * t).max(0.0).sqrt()
}

/// Linearly interpolates between `a` and `b` using `ratio`.
///
/// Port of `anim::interpolateF`.
#[must_use]
pub fn interpolate_f(a: f64, b: f64, ratio: f64) -> f64 {
    a + (b - a) * ratio
}

/// Returns the inverse interpolation ratio `(result - from) / (to - from)`.
///
/// Matches `InterpolationRatio` in `statistics/chart_widget.cpp` and
/// `statistics/view/stack_linear_chart_view.cpp`.
#[must_use]
pub fn interpolation_ratio(from: f64, to: f64, result: f64) -> f64 {
    let span = to - from;
    if span == 0.0 {
        0.0
    } else {
        (result - from) / span
    }
}

/// Basic animated scalar value matching `anim::value` in `lib_ui`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AnimValue {
    cur: f64,
    from: f64,
    delta: f64,
}

impl AnimValue {
    /// Creates an animated value already settled at `from`.
    #[must_use]
    pub const fn constant(from: f64) -> Self {
        Self {
            cur: from,
            from,
            delta: 0.0,
        }
    }

    /// Creates an animated value transitioning from `from` to `to`.
    #[must_use]
    pub fn new(from: f64, to: f64) -> Self {
        Self {
            cur: from,
            from,
            delta: to - from,
        }
    }

    /// Starts a new transition from the current value to `to`.
    pub fn start(&mut self, to: f64) {
        self.from = self.cur;
        self.delta = to - self.from;
    }

    /// Restarts the transition from the current value toward the existing target.
    pub fn restart(&mut self) {
        self.delta = self.from + self.delta - self.cur;
        self.from = self.cur;
    }

    /// Returns the starting value of the active transition.
    #[must_use]
    pub const fn from(&self) -> f64 {
        self.from
    }

    /// Returns the current interpolated value.
    #[must_use]
    pub const fn current(&self) -> f64 {
        self.cur
    }

    /// Returns the target value of the transition.
    #[must_use]
    pub fn to(&self) -> f64 {
        self.from + self.delta
    }

    /// Returns `true` when `current == to`.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.cur == self.to()
    }

    /// Advances the current value using progress `dt` in `0.0..=1.0` and easing `func`.
    pub fn update(&mut self, dt: f64, func: Transition) -> &mut Self {
        self.cur = self.from + func(self.delta, dt);
        self
    }

    /// Immediately jumps to the target value and clears `delta`.
    pub fn finish(&mut self) {
        self.cur = self.from + self.delta;
        self.from = self.cur;
        self.delta = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easings_match_endpoints_and_midpoints() {
        assert!((linear(10.0, 0.5) - 5.0).abs() < 1e-12);
        assert!((ease_in_cubic(10.0, 0.5) - 1.25).abs() < 1e-12);
        assert!((ease_out_cubic(10.0, 0.5) - 8.75).abs() < 1e-12);
        assert!((sine_in_out(10.0, 0.5) - 5.0).abs() < 1e-12);
        assert!((ease_out_circ(10.0, 1.0) - 10.0).abs() < 1e-12);
        assert!((ease_out_circ(10.0, 0.0) - 0.0).abs() < 1e-12);
    }

    #[test]
    fn anim_value_lifecycle() {
        let mut v = AnimValue::new(0.0, 100.0);
        assert!(!v.is_finished());
        v.update(0.5, ease_in_cubic);
        assert!((v.current() - 12.5).abs() < 1e-12);
        v.start(50.0);
        assert!((v.from() - 12.5).abs() < 1e-12);
        assert!((v.to() - 50.0).abs() < 1e-12);
        v.finish();
        assert!(v.is_finished());
        assert!((v.current() - 50.0).abs() < 1e-12);
    }
}
