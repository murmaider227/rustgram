//! DPI scaling constants and rounding rules.
//!
//! Port of `Telegram/lib_ui/ui/style/style_core_scale.h` and `style_core_scale.cpp`.

use std::sync::atomic::{AtomicI32, Ordering};

/// Automatic scale sentinel (`0%`).
pub const SCALE_AUTO: i32 = 0;
/// Minimum allowed UI scale percentage (`50%`).
pub const SCALE_MIN: i32 = 50;
/// Default UI scale percentage (`100%`).
pub const SCALE_DEFAULT: i32 = 100;
/// Maximum allowed UI scale percentage (`300%`).
pub const SCALE_MAX: i32 = 300;
/// Maximum scale always allowed regardless of device pixel ratio (`200%`).
pub const SCALE_ALWAYS_ALLOW_MAX: i32 = 200;

/// Standard tdesktop UI scale percentages.
pub const STANDARD_SCALES: [Scale; 10] = [
    Scale::S100,
    Scale::S110,
    Scale::S125,
    Scale::S150,
    Scale::S175,
    Scale::S200,
    Scale::S225,
    Scale::S250,
    Scale::S275,
    Scale::S300,
];

/// UI scale factor expressed as an integer percentage (e.g. `100`, `125`, `150`, `200`, `300`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Scale(i32);

impl Scale {
    /// Automatic scale (`0%`).
    pub const AUTO: Self = Self(SCALE_AUTO);
    /// Minimum scale (`50%`).
    pub const MIN: Self = Self(SCALE_MIN);
    /// Default scale (`100%`).
    pub const DEFAULT: Self = Self(SCALE_DEFAULT);
    /// Maximum scale (`300%`).
    pub const MAX: Self = Self(SCALE_MAX);

    /// 100% scale.
    pub const S100: Self = Self(100);
    /// 110% scale.
    pub const S110: Self = Self(110);
    /// 125% scale.
    pub const S125: Self = Self(125);
    /// 150% scale.
    pub const S150: Self = Self(150);
    /// 175% scale.
    pub const S175: Self = Self(175);
    /// 200% scale.
    pub const S200: Self = Self(200);
    /// 225% scale.
    pub const S225: Self = Self(225);
    /// 250% scale.
    pub const S250: Self = Self(250);
    /// 275% scale.
    pub const S275: Self = Self(275);
    /// 300% scale.
    pub const S300: Self = Self(300);

    /// Create a scale from a percentage value (clamped to `SCALE_MIN..=SCALE_MAX` if non-zero).
    #[must_use]
    pub const fn new(percent: i32) -> Self {
        if percent == SCALE_AUTO {
            Self(SCALE_AUTO)
        } else if percent < SCALE_MIN {
            Self(SCALE_MIN)
        } else if percent > SCALE_MAX {
            Self(SCALE_MAX)
        } else {
            Self(percent)
        }
    }

    /// Create a scale from an unchecked percentage value (matches C++ raw `int scale`).
    #[must_use]
    pub const fn from_raw(percent: i32) -> Self {
        Self(percent)
    }

    /// Return the scale percentage (e.g. `150` for 150%).
    #[must_use]
    pub const fn percent(self) -> i32 {
        self.0
    }

    /// Return the scale factor as `f64` (e.g. `1.5` for 150%).
    #[must_use]
    pub fn factor(self) -> f64 {
        f64::from(self.0) / 100.0
    }
}

impl Default for Scale {
    fn default() -> Self {
        Self::DEFAULT
    }
}

static DEVICE_PIXEL_RATIO: AtomicI32 = AtomicI32::new(1);
static CURRENT_SCALE: AtomicI32 = AtomicI32::new(SCALE_DEFAULT);

/// Current global device pixel ratio (`1..=6`, default `1`).
#[must_use]
pub fn device_pixel_ratio() -> i32 {
    DEVICE_PIXEL_RATIO.load(Ordering::Relaxed)
}

/// Set the global device pixel ratio, clamped to `1..=(SCALE_MAX / SCALE_MIN)` (`1..=6`).
pub fn set_device_pixel_ratio(ratio: i32) {
    let clamped = ratio.clamp(1, SCALE_MAX / SCALE_MIN);
    DEVICE_PIXEL_RATIO.store(clamped, Ordering::Relaxed);
}

/// Current global UI scale (default `Scale::S100`).
#[must_use]
pub fn scale() -> Scale {
    Scale(CURRENT_SCALE.load(Ordering::Relaxed))
}

/// Set the global UI scale. Ignores `0` (`SCALE_AUTO`), matching tdesktop's `Expects(scale != 0)`.
pub fn set_scale(new_scale: Scale) {
    if new_scale.0 != 0 {
        CURRENT_SCALE.store(new_scale.0, Ordering::Relaxed);
    }
}

/// Maximum allowed scale percentage for a given device pixel ratio.
///
/// Port of `style::MaxScaleForRatio`.
#[must_use]
pub const fn max_scale_for_ratio(ratio: i32) -> i32 {
    if ratio <= 0 {
        return SCALE_ALWAYS_ALLOW_MAX;
    }
    let by_ratio = SCALE_MAX / ratio;
    if by_ratio > SCALE_ALWAYS_ALLOW_MAX {
        by_ratio
    } else {
        SCALE_ALWAYS_ALLOW_MAX
    }
}

/// Validate and clamp a requested scale percentage against the current device pixel ratio.
///
/// Port of `style::CheckScale`.
#[must_use]
pub fn check_scale(requested: i32) -> i32 {
    if requested == SCALE_AUTO {
        SCALE_AUTO
    } else {
        requested.clamp(SCALE_MIN, max_scale_for_ratio(device_pixel_ratio()))
    }
}

/// Base logical DPI for the current platform (`72` on macOS, `96` elsewhere).
///
/// Port of `style::BaseDpi` fallback.
#[must_use]
pub const fn base_dpi() -> i32 {
    if cfg!(target_os = "macos") { 72 } else { 96 }
}

/// Scale an integer pixel value using tdesktop's exact `ConvertScale` rounding rules.
///
/// Port of `style::ConvertScale<int>(int value, int scale)`:
/// - Negative values are negated, scaled, and negated back.
/// - `0` always maps to `0`.
/// - Positive values compute `round((value * scale / 100.0) - 0.01)` so exact `.5` values round
///   down (e.g. `1px` at `150%` -> `1px`, `3px` at `150%` -> `4px`).
/// - Non-zero inputs never collapse to `0` (minimum magnitude is `1`).
#[must_use]
pub fn convert_scale(value: i32, target_scale: Scale) -> i32 {
    if value == 0 {
        return 0;
    }
    if value < 0 {
        let abs = i64::from(value).unsigned_abs();
        let scaled = ((abs as f64) * f64::from(target_scale.0) / 100.0) - 0.01;
        let rounded = scaled.round() as i64;
        let non_zero = if rounded != 0 { rounded } else { 1 };
        return (-non_zero).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    }
    let scaled = (f64::from(value) * f64::from(target_scale.0) / 100.0) - 0.01;
    let result = scaled.round() as i32;
    if result != 0 { result } else { 1 }
}

/// Scale an integer pixel value using the current global [`scale()`].
#[must_use]
pub fn convert_scale_current(value: i32) -> i32 {
    convert_scale(value, scale())
}

/// Scale a floating-point value using tdesktop's `ConvertScale<double>` formula.
#[must_use]
pub fn convert_scale_f64(value: f64, target_scale: Scale) -> f64 {
    if value < 0.0 {
        return -convert_scale_f64(-value, target_scale);
    }
    ((value * f64::from(target_scale.0) / 100.0) - 0.01).round()
}

/// Exact (unrounded) scaling for floating-point values.
///
/// Port of `style::ConvertScaleExact<double>`.
#[must_use]
pub fn convert_scale_exact_f64(value: f64, target_scale: Scale) -> f64 {
    if value < 0.0 {
        -convert_scale_f64(-value, target_scale)
    } else {
        value * f64::from(target_scale.0) / 100.0
    }
}

/// High-precision float scaling (`ConvertFloatScale` in `style_core_scale.h`).
#[must_use]
pub fn convert_float_scale(value: f64, target_scale: Scale) -> f64 {
    const PRECISION: f64 = 1_000_000.0;
    convert_scale_f64(value * PRECISION, target_scale) / PRECISION
}
