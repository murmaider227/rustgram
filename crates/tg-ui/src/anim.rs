//! Animation curves, color interpolation, and stateful value animations.
//!
//! Port of `Telegram/lib_ui/ui/effects/animation_value.h`,
//! `Telegram/lib_ui/ui/effects/animation_value.cpp`,
//! `Telegram/lib_ui/ui/effects/animations.h`, and
//! `Telegram/lib_ui/ui/effects/animations.cpp`.

use std::f64::consts::PI;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use egui::Color32;

static ANIMATIONS_DISABLED: AtomicBool = AtomicBool::new(false);
static SLOW_MULTIPLIER: AtomicU32 = AtomicU32::new(1);

/// Whether animations are globally disabled (e.g., power saving / reduced motion).
pub fn disabled() -> bool {
    ANIMATIONS_DISABLED.load(Ordering::Relaxed)
}

/// Enable or disable animations globally.
pub fn set_disabled(disabled: bool) {
    ANIMATIONS_DISABLED.store(disabled, Ordering::Relaxed);
}

/// Global animation slow-down factor (`1` = normal speed, `10` = 10x slower).
pub fn slow_multiplier() -> u32 {
    SLOW_MULTIPLIER.load(Ordering::Relaxed).max(1)
}

/// Set global slow-down multiplier (clamped to `>= 1`).
pub fn set_slow_multiplier(multiplier: u32) {
    SLOW_MULTIPLIER.store(multiplier.max(1), Ordering::Relaxed);
}

/// Animation trigger mode mirroring `anim::type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnimType {
    #[default]
    Normal,
    Instant,
}

/// Easing curves ported directly from `Telegram/lib_ui/ui/effects/animation_value.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Transition {
    #[default]
    Linear,
    SineInOut,
    HalfSine,
    EaseOutBack,
    EaseInCirc,
    EaseOutCirc,
    EaseInCubic,
    EaseOutCubic,
    EaseInQuint,
    EaseOutQuint,
    Bumpy(f64),
}

impl Transition {
    /// Evaluate the transition for `delta` at normalized time `dt` in `[0.0, 1.0]`.
    pub fn eval(self, delta: f64, dt: f64) -> f64 {
        let dt = dt.clamp(0.0, 1.0);
        match self {
            Self::Linear => linear(delta, dt),
            Self::SineInOut => sine_in_out(delta, dt),
            Self::HalfSine => half_sine(delta, dt),
            Self::EaseOutBack => ease_out_back(delta, dt),
            Self::EaseInCirc => ease_in_circ(delta, dt),
            Self::EaseOutCirc => ease_out_circ(delta, dt),
            Self::EaseInCubic => ease_in_cubic(delta, dt),
            Self::EaseOutCubic => ease_out_cubic(delta, dt),
            Self::EaseInQuint => ease_in_quint(delta, dt),
            Self::EaseOutQuint => ease_out_quint(delta, dt),
            Self::Bumpy(bump) => bumpy(bump, delta, dt),
        }
    }
}

/// `anim::linear`: `delta * dt`.
#[inline]
pub fn linear(delta: f64, dt: f64) -> f64 {
    delta * dt
}

/// `anim::sineInOut`: `-(delta / 2) * (cos(pi * dt) - 1)`.
#[inline]
pub fn sine_in_out(delta: f64, dt: f64) -> f64 {
    -(delta / 2.0) * ((PI * dt).cos() - 1.0)
}

/// `anim::halfSine`: `delta * sin(pi * dt / 2)`.
#[inline]
pub fn half_sine(delta: f64, dt: f64) -> f64 {
    delta * (PI * dt / 2.0).sin()
}

/// `anim::easeOutBack` with overshoot constant `s = 1.70158`.
#[inline]
pub fn ease_out_back(delta: f64, dt: f64) -> f64 {
    const S: f64 = 1.70158;
    let t = dt - 1.0;
    delta * (t * t * ((S + 1.0) * t + S) + 1.0)
}

/// `anim::easeInCirc`: `-delta * (sqrt(1 - dt^2) - 1)`.
#[inline]
pub fn ease_in_circ(delta: f64, dt: f64) -> f64 {
    -delta * ((1.0 - dt * dt).max(0.0).sqrt() - 1.0)
}

/// `anim::easeOutCirc`: `delta * sqrt(1 - (dt - 1)^2)`.
#[inline]
pub fn ease_out_circ(delta: f64, dt: f64) -> f64 {
    let t = dt - 1.0;
    delta * (1.0 - t * t).max(0.0).sqrt()
}

/// `anim::easeInCubic`: `delta * dt^3`.
#[inline]
pub fn ease_in_cubic(delta: f64, dt: f64) -> f64 {
    delta * dt * dt * dt
}

/// `anim::easeOutCubic`: `delta * ((dt - 1)^3 + 1)`.
#[inline]
pub fn ease_out_cubic(delta: f64, dt: f64) -> f64 {
    let t = dt - 1.0;
    delta * (t * t * t + 1.0)
}

/// `anim::easeInQuint`: `delta * dt^5`.
#[inline]
pub fn ease_in_quint(delta: f64, dt: f64) -> f64 {
    let t2 = dt * dt;
    delta * t2 * t2 * dt
}

/// `anim::easeOutQuint`: `delta * ((dt - 1)^5 + 1)`.
#[inline]
pub fn ease_out_quint(delta: f64, dt: f64) -> f64 {
    let t = dt - 1.0;
    let t2 = t * t;
    delta * (t2 * t2 * t + 1.0)
}

/// `anim::bumpy(bump)`: parabolic overshoot curve peaking at `bump` (`bump > 1.0`).
#[inline]
pub fn bumpy(bump: f64, delta: f64, dt: f64) -> f64 {
    let dt0 = bump - (bump * (bump - 1.0)).max(0.0).sqrt();
    let k = 1.0 / (2.0 * dt0 - 1.0);
    delta * (bump - k * (dt - dt0) * (dt - dt0))
}

/// `base::SafeRound` equivalent for integer rounding.
#[inline]
pub fn safe_round(v: f64) -> i32 {
    if v >= 0.0 {
        (v + 0.5).floor() as i32
    } else {
        (v - 0.5).ceil() as i32
    }
}

/// `anim::interpolateToF`: linear float interpolation between two integers.
#[inline]
pub fn interpolate_to_f(a: i32, b: i32, b_ratio: f64) -> f64 {
    f64::from(a) + f64::from(b - a) * b_ratio
}

/// `anim::interpolate`: rounded integer interpolation between `a` and `b`.
#[inline]
pub fn interpolate_i32(a: i32, b: i32, b_ratio: f64) -> i32 {
    safe_round(interpolate_to_f(a, b, b_ratio))
}

/// Linear interpolation between two `f32` values.
#[inline]
pub fn interpolate_f32(a: f32, b: f32, b_ratio: f32) -> f32 {
    a + (b - a) * b_ratio
}

/// Linear interpolation between two `f64` values.
#[inline]
pub fn interpolate_f64(a: f64, b: f64, b_ratio: f64) -> f64 {
    a + (b - a) * b_ratio
}

/// `anim::color(a, b, b_ratio)` ported from the exact 64-bit shifted integer
/// blending formula in `animation_value.h` (lines 260-277).
///
/// Works on unmultiplied RGBA channels identically to Qt's `QColor`.
pub fn color_lerp(a: Color32, b: Color32, b_ratio: f64) -> Color32 {
    let [ar, ag, ab, aa] = a.to_srgba_unmultiplied();
    let [br, bg, bb, ba] = b.to_srgba_unmultiplied();

    let b_opacity = (interpolate_i32(0, 255, b_ratio).clamp(0, 255) + 1) as u64;
    let a_opacity = 256 - b_opacity;

    let pack = |r: u8, g: u8, b: u8, a: u8| -> u64 {
        u64::from(b) | (u64::from(g) << 16) | (u64::from(r) << 32) | (u64::from(a) << 48)
    };

    let components = pack(ar, ag, ab, aa) * a_opacity + pack(br, bg, bb, ba) * b_opacity;
    let r = ((components >> 40) & 0xFF) as u8;
    let g = ((components >> 24) & 0xFF) as u8;
    let bl = ((components >> 8) & 0xFF) as u8;
    let al = ((components >> 56) & 0xFF) as u8;
    Color32::from_rgba_unmultiplied(r, g, bl, al)
}

/// `anim::with_alpha(color, alpha)`: multiply a color's alpha channel by `alpha` in `[0, 1]`.
pub fn with_alpha(color: Color32, alpha: f64) -> Color32 {
    let [r, g, b, a] = color.to_srgba_unmultiplied();
    let scaled = (f64::from(a) * alpha.clamp(0.0, 1.0))
        .round()
        .clamp(0.0, 255.0) as u8;
    Color32::from_rgba_unmultiplied(r, g, b, scaled)
}

/// `anim::getPremultiplied(color)` returning packed premultiplied ARGB32 (`0xAARRGGBB`).
pub fn premultiplied_argb(color: Color32) -> u32 {
    let [r, g, b, a] = color.to_srgba_unmultiplied();
    let alpha = u64::from(a) + 1;
    let components = u64::from(b) | (u64::from(g) << 16) | (u64::from(r) << 32) | (255u64 << 48);
    let mul = components * alpha;
    (((mul & 0x0000_0000_0000_FF00) >> 8)
        | ((mul & 0x0000_0000_FF00_0000) >> 16)
        | ((mul & 0x0000_FF00_0000_0000) >> 24)
        | ((mul & 0xFF00_0000_0000_0000) >> 32)) as u32
}

/// Relative luminance (WCAG 2.1) of a color in `[0.0, 1.0]` for contrast checks.
pub fn relative_luminance(color: Color32) -> f64 {
    let [r, g, b, _] = color.to_srgba_unmultiplied();
    let linearize = |c: u8| -> f64 {
        let s = f64::from(c) / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linearize(r) + 0.7152 * linearize(g) + 0.0722 * linearize(b)
}

/// Contrast ratio between two colors (`1.0` to `21.0`).
pub fn contrast_ratio(a: Color32, b: Color32) -> f64 {
    let la = relative_luminance(a);
    let lb = relative_luminance(b);
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Stateful animated float value mirroring `Ui::Animations::Simple`.
///
/// Timestamps use seconds (`f64`, matching `egui::InputState::time`), while
/// durations are specified in milliseconds (`u32`) to match `.style` constants.
#[derive(Debug, Clone, Copy, Default)]
pub struct SimpleAnimation {
    active: bool,
    started_sec: f64,
    duration_sec: f64,
    from: f64,
    delta: f64,
    current: f64,
    transition: Transition,
}

impl SimpleAnimation {
    /// Create a stopped animation.
    pub const fn new() -> Self {
        Self {
            active: false,
            started_sec: 0.0,
            duration_sec: 0.0,
            from: 0.0,
            delta: 0.0,
            current: 0.0,
            transition: Transition::Linear,
        }
    }

    /// Start animating from `from` to `to` over `duration_ms` milliseconds.
    pub fn start(
        &mut self,
        now_sec: f64,
        from: f64,
        to: f64,
        duration_ms: u32,
        transition: Transition,
    ) {
        if disabled() || duration_ms == 0 {
            self.active = false;
            self.from = to;
            self.delta = 0.0;
            self.current = to;
            self.duration_sec = 0.0;
            self.transition = transition;
            return;
        }
        self.active = true;
        self.started_sec = now_sec;
        self.duration_sec = (f64::from(duration_ms) * f64::from(slow_multiplier())) / 1000.0;
        self.from = from;
        self.delta = to - from;
        self.current = from;
        self.transition = transition;
    }

    /// Retarget the animation smoothly from its current value to `to`.
    pub fn change(&mut self, now_sec: f64, to: f64, duration_ms: u32, transition: Transition) {
        let cur = if self.active {
            self.sample(now_sec, to)
        } else {
            self.current
        };
        self.start(now_sec, cur, to, duration_ms, transition);
    }

    /// Stop the animation immediately.
    pub fn stop(&mut self) {
        self.active = false;
    }

    /// Whether the animation is currently running at `now_sec`.
    pub fn animating(&self) -> bool {
        self.active
    }

    /// Advance and read the current value at `now_sec`. If stopped, returns `final_value`.
    pub fn sample(&mut self, now_sec: f64, final_value: f64) -> f64 {
        if !self.active {
            self.current = final_value;
            return final_value;
        }
        let elapsed = (now_sec - self.started_sec).max(0.0);
        if disabled() || elapsed >= self.duration_sec || self.duration_sec <= 0.0 {
            self.current = self.from + self.delta;
            self.active = false;
            return self.current;
        }
        let ratio = (elapsed / self.duration_sec).clamp(0.0, 1.0);
        self.current = self.from + self.transition.eval(self.delta, ratio);
        self.current
    }

    /// Read the current value without mutating state (for pure painting).
    pub fn value_at(&self, now_sec: f64, final_value: f64) -> f64 {
        if !self.active {
            return final_value;
        }
        let elapsed = (now_sec - self.started_sec).max(0.0);
        if disabled() || elapsed >= self.duration_sec || self.duration_sec <= 0.0 {
            return self.from + self.delta;
        }
        let ratio = (elapsed / self.duration_sec).clamp(0.0, 1.0);
        self.from + self.transition.eval(self.delta, ratio)
    }

    /// Sample and request an egui repaint while animating.
    pub fn animate_egui(&mut self, ctx: &egui::Context, final_value: f64) -> f64 {
        let now = ctx.input(|i| i.time);
        let v = self.sample(now, final_value);
        if self.active {
            ctx.request_repaint();
        }
        v
    }
}

/// Continuous linear velocity tracker mirroring `anim::continuous_value`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ContinuousValue {
    duration_ms: f64,
    to: f64,
    cur: f64,
    delta_per_ms: f64,
}

impl ContinuousValue {
    /// Create a continuous value with default transition `duration_ms`.
    pub const fn new(duration_ms: f64) -> Self {
        Self {
            duration_ms,
            to: 0.0,
            cur: 0.0,
            delta_per_ms: 0.0,
        }
    }

    /// Start moving towards `to` over `duration_ms`.
    pub fn start(&mut self, to: f64, duration_ms: f64) {
        self.to = to;
        if duration_ms <= 0.0 {
            self.cur = to;
            self.delta_per_ms = 0.0;
        } else {
            self.delta_per_ms = (self.to - self.cur) / duration_ms;
        }
    }

    /// Start moving towards `to` over the default duration.
    pub fn start_default(&mut self, to: f64) {
        self.start(to, self.duration_ms);
    }

    /// Advance by `dt_ms` milliseconds.
    pub fn update(&mut self, dt_ms: f64) -> f64 {
        if (self.to - self.cur).abs() > f64::EPSILON {
            self.cur += self.delta_per_ms * dt_ms;
            if (self.delta_per_ms > 0.0 && self.cur > self.to)
                || (self.delta_per_ms < 0.0 && self.cur < self.to)
            {
                self.cur = self.to;
            }
        }
        self.cur
    }

    /// Current value.
    pub const fn current(&self) -> f64 {
        self.cur
    }

    /// Target value.
    pub const fn target(&self) -> f64 {
        self.to
    }

    /// Reset to zero.
    pub fn reset(&mut self) {
        self.to = 0.0;
        self.cur = 0.0;
        self.delta_per_ms = 0.0;
    }
}
