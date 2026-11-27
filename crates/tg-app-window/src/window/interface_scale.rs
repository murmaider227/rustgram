//! Interface scale as tdesktop computes it (TGB-3040).
//!
//! tdesktop rounds the screen's device pixel ratio with Qt's `RoundPreferFloor` policy
//! (`core/launcher.cpp`), keeps that integer ratio (`style::SetDevicePixelRatio`, `core/sandbox.cpp`)
//! and scales its own styles for the rest: the screen scale is the remaining DPI rounded to 5 %
//! (`Sandbox::setupScreenScale`), clamped to `MaxScaleForRatio`, and a `-scale` option or the
//! saved "Default interface scale" replaces it (`cEvalScale`). At 150 % that is ratio 1 and scale
//! 150; at 175 % it is ratio 2 and scale 90, i.e. 1.8 physical pixels per point.
//!
//! egui has one factor, pixels per point. [`apply`] sets egui's zoom so pixels per point equal
//! `ratio * scale / 100`. Font sizes are not rounded to `ConvertScale` pixels: tdesktop's GDI
//! hinting rounds glyph advances to whole pixels, so its 19 px line at 150 % is as wide as egui's
//! unhinted 19.5 px one (measured in TGB-3040), and unhinted 19 px text came out 1.6 % narrower.

use tg_core::{SCALE_AUTO, SCALE_MAX, SCALE_MIN};

/// `style::kScaleAlwaysAllowMax`.
const SCALE_ALWAYS_ALLOW_MAX: i32 = 200;

/// The evaluated interface scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterfaceScale {
    /// Integer device pixel ratio (`style::DevicePixelRatio`).
    pub device_pixel_ratio: i32,
    /// Style scale in percent (`style::Scale`).
    pub scale: i32,
}

impl InterfaceScale {
    /// tdesktop's scale for a screen with `native_ppp` physical pixels per logical pixel and the
    /// configured scale (`SCALE_AUTO` or a percentage, e.g. from `-scale`).
    #[must_use]
    pub fn evaluate(native_ppp: f32, config_scale: i32) -> Self {
        let device_pixel_ratio = device_pixel_ratio(native_ppp);
        let scale = if config_scale == SCALE_AUTO {
            screen_scale(native_ppp, device_pixel_ratio)
        } else {
            check_scale(config_scale, device_pixel_ratio)
        };
        Self {
            device_pixel_ratio,
            scale,
        }
    }

    /// Physical pixels per egui point.
    #[must_use]
    pub fn pixels_per_point(self) -> f32 {
        self.device_pixel_ratio as f32 * self.scale as f32 / 100.0
    }
}

/// Qt's `HighDpiScaleFactorRoundingPolicy::RoundPreferFloor` (fractions of .75 and up round up),
/// then `std::clamp(int(std::ceil(ratio)), 1, 3)` as in `Sandbox::setupScreenScale`.
#[must_use]
pub fn device_pixel_ratio(native_ppp: f32) -> i32 {
    if !native_ppp.is_finite() || native_ppp <= 0.0 {
        return 1;
    }
    let floor = native_ppp.floor();
    let rounded = if native_ppp - floor >= 0.75 {
        floor + 1.0
    } else {
        floor
    };
    (rounded as i32).clamp(1, 3)
}

/// `style::MaxScaleForRatio`.
#[must_use]
pub fn max_scale_for_ratio(ratio: i32) -> i32 {
    (SCALE_MAX / ratio.max(1)).max(SCALE_ALWAYS_ALLOW_MAX)
}

/// `style::CheckScale` for a set (non-auto) scale.
#[must_use]
pub fn check_scale(scale: i32, ratio: i32) -> i32 {
    scale.clamp(SCALE_MIN, max_scale_for_ratio(ratio))
}

/// The screen scale: logical DPI over the base DPI, rounded to 5 % (`SafeRound(exact * 20) * 5`).
#[must_use]
pub fn screen_scale(native_ppp: f32, ratio: i32) -> i32 {
    let exact = f64::from(native_ppp) / f64::from(ratio.max(1));
    let scale = if exact.is_finite() {
        ((exact * 20.0).round() as i32).saturating_mul(5)
    } else {
        100
    };
    scale.clamp(SCALE_MIN, max_scale_for_ratio(ratio))
}

/// Sets egui's zoom so the frame renders at tdesktop's scale for `config_scale`. Called every
/// frame: the native pixels per point change when the window moves to another screen.
pub fn apply(ctx: &egui::Context, config_scale: i32) {
    let Some(native) = ctx
        .native_pixels_per_point()
        .filter(|n| n.is_finite() && *n > 0.0)
    else {
        return;
    };
    let target = InterfaceScale::evaluate(native, config_scale).pixels_per_point();
    let zoom = target / native;
    // tdesktop has no keyboard zoom of the whole window; egui's Ctrl +/- would fight this.
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
    if (ctx.zoom_factor() - zoom).abs() > 1e-4 {
        ctx.set_zoom_factor(zoom);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_pixel_ratio_rounds_like_round_prefer_floor() {
        for (native, ratio) in [
            (1.0, 1),
            (1.25, 1),
            (1.5, 1),
            (1.74, 1),
            (1.75, 2),
            (2.0, 2),
            (2.5, 2),
            (2.75, 3),
            (3.5, 3),
            (5.0, 3),
            (0.5, 1),
            (0.0, 1),
            (f32::NAN, 1),
            (-2.0, 1),
        ] {
            assert_eq!(device_pixel_ratio(native), ratio, "native {native}");
        }
    }

    #[test]
    fn auto_scale_matches_tdesktop_screen_scale() {
        let eval = |native| InterfaceScale::evaluate(native, SCALE_AUTO);
        assert_eq!(
            eval(1.0),
            InterfaceScale {
                device_pixel_ratio: 1,
                scale: 100
            }
        );
        assert_eq!(
            eval(1.25),
            InterfaceScale {
                device_pixel_ratio: 1,
                scale: 125
            }
        );
        // 150 %: ratio 1, every style is scaled by ConvertScale(…, 150).
        assert_eq!(
            eval(1.5),
            InterfaceScale {
                device_pixel_ratio: 1,
                scale: 150
            }
        );
        // 175 %: ratio 2 and 87.5 % rounds to 90 %, so tdesktop draws at 1.8x.
        assert_eq!(
            eval(1.75),
            InterfaceScale {
                device_pixel_ratio: 2,
                scale: 90
            }
        );
        assert!((eval(1.75).pixels_per_point() - 1.8).abs() < 1e-6);
        assert_eq!(
            eval(2.0),
            InterfaceScale {
                device_pixel_ratio: 2,
                scale: 100
            }
        );
        assert_eq!(
            eval(2.5),
            InterfaceScale {
                device_pixel_ratio: 2,
                scale: 125
            }
        );
        assert_eq!(
            eval(3.0),
            InterfaceScale {
                device_pixel_ratio: 3,
                scale: 100
            }
        );
        // Below 50 % the scale is clamped to kScaleMin.
        assert_eq!(
            eval(0.25),
            InterfaceScale {
                device_pixel_ratio: 1,
                scale: 50
            }
        );
    }

    #[test]
    fn scale_option_replaces_the_screen_scale_within_the_ratio_limit() {
        let eval = InterfaceScale::evaluate;
        // `-scale 200` on a 150 % screen: ratio 1, scale 200.
        assert_eq!(
            eval(1.5, 200),
            InterfaceScale {
                device_pixel_ratio: 1,
                scale: 200
            }
        );
        assert!((eval(1.5, 200).pixels_per_point() - 2.0).abs() < 1e-6);
        assert_eq!(eval(1.0, 125).pixels_per_point(), 1.25);
        // MaxScaleForRatio: 300 % at ratio 1, max(150, 200) = 200 % at ratio 2.
        assert_eq!(eval(1.0, 300).scale, 300);
        assert_eq!(eval(2.0, 300).scale, 200);
        assert_eq!(eval(1.0, 10).scale, 50);
    }

    #[test]
    fn max_scale_for_ratio_matches_tdesktop() {
        assert_eq!(max_scale_for_ratio(1), 300);
        assert_eq!(max_scale_for_ratio(2), 200);
        assert_eq!(max_scale_for_ratio(3), 200);
        assert_eq!(max_scale_for_ratio(0), 300);
    }

    fn run(ctx: &egui::Context, input: egui::RawInput, ui: impl FnMut(&mut egui::Ui)) {
        ctx.run_ui(input, ui).textures_delta.clear();
    }

    #[test]
    fn apply_sets_the_zoom_for_the_native_ratio_and_disables_keyboard_zoom() {
        let ctx = egui::Context::default();
        // No native ratio known yet (before the first window event): nothing changes.
        apply(&ctx, 200);
        assert_eq!(ctx.zoom_factor(), 1.0);

        let mut input = egui::RawInput::default();
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(1.5);
        run(&ctx, input.clone(), |_| {});
        assert!((ctx.pixels_per_point() - 1.5).abs() < 1e-6);
        // `-scale 200` on a 150 % screen: the next frame renders at 2.0 pixels per point.
        apply(&ctx, 200);
        assert!(!ctx.options(|o| o.zoom_with_keyboard));
        run(&ctx, input.clone(), |_| {});
        assert!((ctx.zoom_factor() - 2.0 / 1.5).abs() < 1e-6);
        assert!((ctx.pixels_per_point() - 2.0).abs() < 1e-6);

        apply(&ctx, SCALE_AUTO);
        run(&ctx, input, |_| {});
        assert!((ctx.zoom_factor() - 1.0).abs() < 1e-6);
        assert!((ctx.pixels_per_point() - 1.5).abs() < 1e-6);
    }
}
