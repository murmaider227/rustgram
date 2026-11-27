//! HSV palette colorizer for embedded theme accent color shifting.
//!
//! Port of `lib_ui/ui/style/style_palette_colorizer.h` and `style_palette_colorizer.cpp`.

use std::collections::{BTreeMap, BTreeSet};

use image::RgbaImage;

use crate::color::{Color, Hsv};

const ENOUGH_LIGHTNESS_FOR_CONTRAST: i32 = 64;

/// HSV triple used by `Colorizer` (`style::colorizer::Color`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColorizerColor {
    /// Hue in degrees (`0..=359`, or `-1` for achromatic).
    pub hue: i32,
    /// Saturation (`0..=255`).
    pub saturation: i32,
    /// Value (`0..=255`).
    pub value: i32,
}

impl ColorizerColor {
    /// Converts a `Color` into a `ColorizerColor` via Qt-compatible `getHsv`.
    #[must_use]
    pub fn from_color(color: Color) -> Self {
        let Hsv { h, s, v } = color.to_hsv();
        Self {
            hue: h,
            saturation: s,
            value: v,
        }
    }

    /// Parses a 6-digit hex string (`rrggbb` or `#rrggbb`) into a `ColorizerColor`.
    #[must_use]
    pub fn from_hex(hex: &str) -> Option<Self> {
        Color::from_hex(hex).map(Self::from_color)
    }

    /// Converts back into an opaque sRGB `Color`.
    #[must_use]
    pub fn to_color(self, alpha: u8) -> Color {
        Color::from_hsv(self.hue, self.saturation, self.value, alpha)
    }
}

/// Configuration for HSV accent-shifting a palette or image (`style::colorizer`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Colorizer {
    /// Maximum hue distance from `was.hue` to shift (0 disables the colorizer).
    pub hue_threshold: i32,
    /// Minimum HSL lightness allowed for the target accent (`0..=255`).
    pub lightness_min: i32,
    /// Maximum HSL lightness allowed for the target accent (`0..=255`).
    pub lightness_max: i32,
    /// Original theme accent in HSV.
    pub was: ColorizerColor,
    /// Target accent in HSV (after lightness clamping).
    pub now: ColorizerColor,
    /// Palette keys excluded from colorization.
    pub ignore_keys: BTreeSet<String>,
    /// Palette keys that must maintain minimum lightness contrast against a reference
    /// color (`first`), falling back to a replacement color (`second`) if contrast is too low.
    pub keep_contrast: BTreeMap<String, (ColorizerColor, ColorizerColor)>,
}

impl Default for Colorizer {
    fn default() -> Self {
        Self {
            hue_threshold: 0,
            lightness_min: 0,
            lightness_max: 255,
            was: ColorizerColor::default(),
            now: ColorizerColor::default(),
            ignore_keys: BTreeSet::new(),
            keep_contrast: BTreeMap::new(),
        }
    }
}

impl Colorizer {
    /// Returns `true` when the colorizer is active (`hue_threshold > 0`).
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.hue_threshold > 0
    }
}

/// Core HSV shifting formula from `style_palette_colorizer.cpp`.
///
/// Returns `None` if the color's hue is outside `with.hue_threshold` of `with.was.hue`.
#[must_use]
pub fn colorize_hsv(color: ColorizerColor, with: &Colorizer) -> Option<ColorizerColor> {
    let change_color = (color.hue - with.was.hue).abs() < with.hue_threshold;
    if !change_color {
        return None;
    }
    let now_hue = color.hue + (with.now.hue - with.was.hue);
    let now_saturation = if (color.saturation > with.was.saturation)
        && (with.now.saturation > with.was.saturation)
    {
        ((with.now.saturation * (255 - with.was.saturation))
            + ((color.saturation - with.was.saturation) * (255 - with.now.saturation)))
            / (255 - with.was.saturation)
    } else if (color.saturation != with.was.saturation) && (with.was.saturation != 0) {
        (color.saturation * with.now.saturation) / with.was.saturation
    } else {
        with.now.saturation
    };
    let now_value = if color.value > with.was.value {
        ((with.now.value * (255 - with.was.value))
            + ((color.value - with.was.value) * (255 - with.now.value)))
            / (255 - with.was.value)
    } else if color.value < with.was.value {
        (color.value * with.now.value) / with.was.value
    } else {
        with.now.value
    };
    Some(ColorizerColor {
        hue: (now_hue + 360) % 360,
        saturation: now_saturation,
        value: now_value,
    })
}

/// Colorizes a `Color`, returning `Some(new_color)` if its hue was within `hue_threshold`.
#[must_use]
pub fn colorize_color(color: Color, with: &Colorizer) -> Option<Color> {
    let hsv = ColorizerColor::from_color(color);
    let shifted = colorize_hsv(hsv, with)?;
    Some(shifted.to_color(color.a))
}

/// Colorizes RGB channels in place (alpha untouched).
pub fn colorize_rgb(r: &mut u8, g: &mut u8, b: &mut u8, with: &Colorizer) {
    if let Some(changed) = colorize_color(Color::rgb(*r, *g, *b), with) {
        *r = changed.r;
        *g = changed.g;
        *b = changed.b;
    }
}

/// Colorizes RGB channels with a `keep_contrast` check/replacement pair.
/// Port of `style::colorize(const std::pair<Color, Color> &contrast, ...)`.
pub fn colorize_with_contrast(
    contrast: (ColorizerColor, ColorizerColor),
    r: &mut u8,
    g: &mut u8,
    b: &mut u8,
    with: &Colorizer,
) {
    let check = contrast.0;
    let rgb = Color::rgb(*r, *g, *b);
    let changed = colorize_color(rgb, with);
    let checked = colorize_hsv(check, with).unwrap_or(check);

    let hsv_lightness = |c: ColorizerColor| -> i32 { c.value - (c.value * c.saturation) / 511 };

    let changed_hsv = ColorizerColor::from_color(changed.unwrap_or(rgb));
    let checked_hsv = ColorizerColor::from_color(checked.to_color(255));
    let delta = (hsv_lightness(changed_hsv) - hsv_lightness(checked_hsv)).abs();
    if delta >= ENOUGH_LIGHTNESS_FOR_CONTRAST {
        if let Some(c) = changed {
            *r = c.r;
            *g = c.g;
            *b = c.b;
        }
        return;
    }
    let replace = contrast.1;
    let result = colorize_hsv(replace, with).unwrap_or(replace).to_color(255);
    *r = result.r;
    *g = result.g;
    *b = result.b;
}

/// Colorizes RGB channels for a named palette key, respecting `ignore_keys` and `keep_contrast`.
pub fn colorize_named(name: &str, r: &mut u8, g: &mut u8, b: &mut u8, with: &Colorizer) {
    if with.ignore_keys.contains(name) {
        return;
    }
    if let Some(&contrast) = with.keep_contrast.get(name) {
        colorize_with_contrast(contrast, r, g, b, with);
    } else {
        colorize_rgb(r, g, b, with);
    }
}

/// Colorizes a `#rrggbb` or `#rrggbbaa` string, preserving prefix `#` and any trailing alpha bytes.
/// Port of `style::colorize(QLatin1String hexColor, const colorizer &with)`.
#[must_use]
pub fn colorize_hex(hex_color: &str, with: &Colorizer) -> Option<String> {
    if (hex_color.len() != 7 && hex_color.len() != 9)
        || !hex_color.starts_with('#')
        || !hex_color.is_ascii()
    {
        return None;
    }
    let color = Color::from_hex(&hex_color[1..7])?;
    let changed = colorize_color(color, with).unwrap_or(color);
    let mut out = String::with_capacity(hex_color.len());
    out.push('#');
    out.push_str(&changed.to_rgb_hex());
    if hex_color.len() == 9 {
        out.push_str(&hex_color[7..9]);
    }
    Some(out)
}

/// Colorizes every pixel of an `RgbaImage` in place (preserving alpha).
/// Port of `style::colorize(QImage &image, const colorizer &with)`.
pub fn colorize_image(image: &mut RgbaImage, with: &Colorizer) {
    if !with.is_active() {
        return;
    }
    for pixel in image.pixels_mut() {
        let [mut r, mut g, mut b, a] = pixel.0;
        colorize_rgb(&mut r, &mut g, &mut b, with);
        pixel.0 = [r, g, b, a];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_hsv_shift_known_answers() {
        // Accent shift from classic blue #40a7e3 (h=202, s=183, v=227)
        // to green #52b440 (h=111, s=165, v=180) with threshold 15.
        let was_c = Color::from_hex("#40a7e3").unwrap();
        let now_c = Color::from_hex("#52b440").unwrap();
        let colorizer = Colorizer {
            hue_threshold: 15,
            lightness_min: 0,
            lightness_max: 255,
            was: ColorizerColor::from_color(was_c),
            now: ColorizerColor::from_color(now_c),
            ignore_keys: BTreeSet::new(),
            keep_contrast: BTreeMap::new(),
        };

        // Exact accent maps to target accent HSV (integer hue 111° maps back to #53b440)
        let exact = colorize_hsv(colorizer.was, &colorizer).unwrap();
        assert_eq!(exact, colorizer.now);
        assert_eq!(
            colorize_color(was_c, &colorizer).unwrap(),
            Color::rgb(83, 180, 64)
        );

        // Hue outside threshold (red: h=0, |0 - 202| = 202 >= 15) returns None
        let red = Color::from_hex("#d14e4e").unwrap();
        assert_eq!(colorize_color(red, &colorizer), None);

        // Hue within threshold (e.g. #39a5db: h=200, |200 - 202| = 2 < 15) shifts!
        let over = Color::from_hex("#39a5db").unwrap();
        let over_hsv = ColorizerColor::from_color(over);
        assert_eq!(over_hsv.h_diff(colorizer.was.hue), 2);
        let shifted = colorize_hsv(over_hsv, &colorizer).unwrap();
        assert_eq!(
            shifted.hue,
            (over_hsv.hue + (colorizer.now.hue - colorizer.was.hue) + 360) % 360
        );
        // Verify exact saturation & value integer arithmetic
        assert_eq!(
            shifted.saturation,
            (over_hsv.saturation * colorizer.now.saturation) / colorizer.was.saturation
        );
        assert_eq!(
            shifted.value,
            (over_hsv.value * colorizer.now.value) / colorizer.was.value
        );

        // Hex colorize preserves alpha bytes untouched
        let hex_out = colorize_hex("#40a7e380", &colorizer).unwrap();
        assert_eq!(hex_out, "#53b44080");
    }

    impl ColorizerColor {
        fn h_diff(self, other: i32) -> i32 {
            (self.hue - other).abs()
        }
    }
}
