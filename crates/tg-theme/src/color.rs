//! Color representation, Qt-compatible HSV/HSL conversions, serialization, and contrast math.
//!
//! Port of `lib_ui/ui/style/style_core_color.h`, `lib_ui/ui/style/style_core.cpp`,
//! `SourceFiles/ui/color_contrast.cpp`, `SourceFiles/ui/color_int_conversion.cpp`,
//! and `SourceFiles/storage/serialize_common.cpp`.

use std::f64::consts::PI;

const MIN_CONTRAST_ALPHA: i32 = 64;
const MIN_CONTRAST_DISTANCE: i32 = 64 * 64 * 4;
const CONTRAST_DELTA_L: i32 = 64;
const GAMUT_ITERATIONS: usize = 30;
const GAMUT_EPSILON: f64 = 1e-4;

/// Plain 8-bit sRGBA color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Color {
    /// Red channel (`0..=255`).
    pub r: u8,
    /// Green channel (`0..=255`).
    pub g: u8,
    /// Blue channel (`0..=255`).
    pub b: u8,
    /// Alpha channel (`0..=255`, 255 = opaque).
    pub a: u8,
}

/// Integer HSV representation matching Qt's `QColor::getHsv` / `QColor::fromHsv`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Hsv {
    /// Hue in degrees `0..=359`, or `-1` when achromatic.
    pub h: i32,
    /// Saturation in `0..=255`.
    pub s: i32,
    /// Value (brightness) in `0..=255`.
    pub v: i32,
}

/// Integer HSL representation matching Qt's `QColor::getHsl` / `QColor::fromHsl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Hsl {
    /// Hue in degrees `0..=359`, or `-1` when achromatic.
    pub h: i32,
    /// Saturation in `0..=255`.
    pub s: i32,
    /// Lightness in `0..=255`.
    pub l: i32,
}

/// OKLab color coordinates (port of `Ui::Lab` in `ui/color_contrast.h`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Oklab {
    /// Perceptual lightness (`0.0..=1.0`).
    pub lightness: f64,
    /// Green-red axis.
    pub a: f64,
    /// Blue-yellow axis.
    pub b: f64,
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn hex_byte(hi: u8, lo: u8) -> Option<u8> {
    Some((hex_nibble(hi)? << 4) | hex_nibble(lo)?)
}

fn float_to_u8_qt(v: f64) -> u8 {
    // Qt stores 16-bit channels via qRound(v * 65535.0) and reads 8-bit via >> 8.
    let clamped = v.clamp(0.0, 1.0);
    let u16_val = (clamped * 65535.0).round() as u32;
    (u16_val >> 8).min(255) as u8
}

impl Color {
    /// Special transparent white `(255, 255, 255, 0)` matching `st::transparent`.
    pub const TRANSPARENT: Self = Self::rgba(255, 255, 255, 0);
    /// Opaque white `(255, 255, 255, 255)`.
    pub const WHITE: Self = Self::rgb(255, 255, 255);
    /// Opaque black `(0, 0, 0, 255)`.
    pub const BLACK: Self = Self::rgb(0, 0, 0);

    /// Constructs a color with explicit RGBA channels.
    #[must_use]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Constructs an opaque color (`a = 255`).
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Returns a copy with alpha replaced.
    #[must_use]
    pub const fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// Returns `[r, g, b, a]`.
    #[must_use]
    pub const fn to_rgba_array(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }

    /// Parses `#rrggbb`, `#rrggbbaa`, `rrggbb`, or `rrggbbaa`.
    #[must_use]
    pub fn from_hex(hex: &str) -> Option<Self> {
        let stripped = hex.strip_prefix('#').unwrap_or(hex);
        let bytes = stripped.as_bytes();
        if bytes.len() != 6 && bytes.len() != 8 {
            return None;
        }
        let r = hex_byte(bytes[0], bytes[1])?;
        let g = hex_byte(bytes[2], bytes[3])?;
        let b = hex_byte(bytes[4], bytes[5])?;
        let a = if bytes.len() == 8 {
            hex_byte(bytes[6], bytes[7])?
        } else {
            255
        };
        Some(Self::rgba(r, g, b, a))
    }

    /// Formats as lowercase `#rrggbb` (when `a == 255`) or `#rrggbbaa`.
    /// Matches `Window::Theme::ColorHexString`.
    #[must_use]
    pub fn to_hex_string(self) -> String {
        if self.a == 255 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
    }

    /// Formats RGB channels as 6 lowercase hex digits without `#`.
    /// Matches `StringFromColor` in `data_wall_paper.cpp`.
    #[must_use]
    pub fn to_rgb_hex(self) -> String {
        format!("{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Decodes a 24-bit RGB integer `0x00RRGGBB` (`Ui::ColorFromSerialized`).
    #[must_use]
    pub const fn from_serialized_rgb(serialized: u32) -> Self {
        Self::rgb(
            ((serialized >> 16) & 0xff) as u8,
            ((serialized >> 8) & 0xff) as u8,
            (serialized & 0xff) as u8,
        )
    }

    /// Decodes an optional 24-bit RGB integer where `u32::MAX` means `None`.
    #[must_use]
    pub const fn maybe_from_serialized_rgb(serialized: u32) -> Option<Self> {
        if serialized == u32::MAX {
            None
        } else {
            Some(Self::from_serialized_rgb(serialized))
        }
    }

    /// Encodes as 24-bit RGB integer `0x00RRGGBB` (`SerializeColor`).
    #[must_use]
    pub const fn to_serialized_rgb(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }

    /// Decodes a 32-bit packed RGBA integer `r | (g << 8) | (b << 16) | (a << 24)`
    /// (`Ui::Color32FromSerialized` / `Serialize::readColor`).
    #[must_use]
    pub const fn from_serialized_rgba32(serialized: u32) -> Self {
        Self::rgba(
            (serialized & 0xff) as u8,
            ((serialized >> 8) & 0xff) as u8,
            ((serialized >> 16) & 0xff) as u8,
            ((serialized >> 24) & 0xff) as u8,
        )
    }

    /// Encodes as `r | (g << 8) | (b << 16) | (a << 24)`
    /// (`Ui::Color32ToSerialized` / `Serialize::writeColor`).
    #[must_use]
    pub const fn to_serialized_rgba32(self) -> u32 {
        (self.r as u32) | ((self.g as u32) << 8) | ((self.b as u32) << 16) | ((self.a as u32) << 24)
    }

    /// Red channel normalized to `0.0..=1.0`.
    #[must_use]
    pub fn red_f(self) -> f64 {
        f64::from(self.r) / 255.0
    }

    /// Green channel normalized to `0.0..=1.0`.
    #[must_use]
    pub fn green_f(self) -> f64 {
        f64::from(self.g) / 255.0
    }

    /// Blue channel normalized to `0.0..=1.0`.
    #[must_use]
    pub fn blue_f(self) -> f64 {
        f64::from(self.b) / 255.0
    }

    /// Alpha channel normalized to `0.0..=1.0`.
    #[must_use]
    pub fn alpha_f(self) -> f64 {
        f64::from(self.a) / 255.0
    }

    /// HSV value channel normalized to `0.0..=1.0` (`QColor::valueF()`).
    #[must_use]
    pub fn value_f(self) -> f64 {
        f64::from(self.r.max(self.g).max(self.b)) / 255.0
    }

    /// Converts RGB to Qt-compatible integer HSV (`QColor::getHsv`).
    #[must_use]
    pub fn to_hsv(self) -> Hsv {
        let r = f64::from(self.r) / 255.0;
        let g = f64::from(self.g) / 255.0;
        let b = f64::from(self.b) / 255.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;

        let v = self.r.max(self.g).max(self.b) as i32;
        if delta == 0.0 || max == 0.0 {
            return Hsv { h: -1, s: 0, v };
        }

        let s16 = ((delta / max) * 65535.0).round() as u32;
        let s = (s16 >> 8) as i32;

        let mut hue = if (r - max).abs() == 0.0 {
            (g - b) / delta
        } else if (g - max).abs() == 0.0 {
            2.0 + (b - r) / delta
        } else {
            4.0 + (r - g) / delta
        };
        hue *= 60.0;
        if hue < 0.0 {
            hue += 360.0;
        }
        let mut h100 = (hue * 100.0).round() as i32;
        if h100 >= 36000 {
            h100 = 0;
        }
        Hsv {
            h: h100 / 100,
            s,
            v,
        }
    }

    /// Constructs a color from Qt-compatible integer HSV (`QColor::fromHsv`).
    #[must_use]
    pub fn from_hsv(h: i32, s: i32, v: i32, a: u8) -> Self {
        let s = s.clamp(0, 255);
        let v = v.clamp(0, 255);
        if h < 0 || s == 0 {
            let gray = v as u8;
            return Self::rgba(gray, gray, gray, a);
        }
        let h_norm = h.rem_euclid(360);
        let hf = f64::from(h_norm) / 60.0;
        let sector = (h_norm / 60) % 6;
        let f = hf - f64::from(sector);
        let sf = f64::from(s) / 255.0;
        let vf = f64::from(v) / 255.0;
        let p = vf * (1.0 - sf);
        let q = vf * (1.0 - sf * f);
        let t = vf * (1.0 - sf * (1.0 - f));
        let (rf, gf, bf) = match sector {
            0 => (vf, t, p),
            1 => (q, vf, p),
            2 => (p, vf, t),
            3 => (p, q, vf),
            4 => (t, p, vf),
            _ => (vf, p, q),
        };
        Self::rgba(
            float_to_u8_qt(rf),
            float_to_u8_qt(gf),
            float_to_u8_qt(bf),
            a,
        )
    }

    /// Converts RGB to Qt-compatible integer HSL (`QColor::getHsl`).
    #[must_use]
    pub fn to_hsl(self) -> Hsl {
        let r = f64::from(self.r) / 255.0;
        let g = f64::from(self.g) / 255.0;
        let b = f64::from(self.b) / 255.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;
        let sum = max + min;
        let l = sum * 0.5;
        let l16 = (l * 65535.0).round() as u32;
        let l_int = (l16 >> 8) as i32;

        if delta == 0.0 {
            return Hsl {
                h: -1,
                s: 0,
                l: l_int,
            };
        }

        let s = if l <= 0.5 {
            delta / sum
        } else {
            delta / (2.0 - sum)
        };
        let s16 = (s.clamp(0.0, 1.0) * 65535.0).round() as u32;
        let s_int = (s16 >> 8) as i32;

        let mut hue = if (r - max).abs() == 0.0 {
            (g - b) / delta
        } else if (g - max).abs() == 0.0 {
            2.0 + (b - r) / delta
        } else {
            4.0 + (r - g) / delta
        };
        hue *= 60.0;
        if hue < 0.0 {
            hue += 360.0;
        }
        let mut h100 = (hue * 100.0).round() as i32;
        if h100 >= 36000 {
            h100 = 0;
        }
        Hsl {
            h: h100 / 100,
            s: s_int,
            l: l_int,
        }
    }

    /// Returns HSL lightness in `0..=255` (`QColor::lightness()`).
    #[must_use]
    pub fn lightness(self) -> i32 {
        self.to_hsl().l
    }

    /// Converts RGB to floating-point HSL matching Qt's `hslHueF`, `hslSaturationF`, `lightnessF`.
    /// Hue is in `[0.0, 1.0)` or `-1.0` when achromatic.
    #[must_use]
    pub fn to_hsl_f(self) -> (f64, f64, f64) {
        let r = f64::from(self.r) / 255.0;
        let g = f64::from(self.g) / 255.0;
        let b = f64::from(self.b) / 255.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;
        let sum = max + min;
        let l = sum * 0.5;

        if delta == 0.0 {
            return (-1.0, 0.0, l);
        }
        let s = if l <= 0.5 {
            delta / sum
        } else {
            delta / (2.0 - sum)
        };
        let mut hue = if (r - max).abs() == 0.0 {
            (g - b) / delta
        } else if (g - max).abs() == 0.0 {
            2.0 + (b - r) / delta
        } else {
            4.0 + (r - g) / delta
        };
        hue /= 6.0;
        if hue < 0.0 {
            hue += 1.0;
        }
        (hue, s.clamp(0.0, 1.0), l.clamp(0.0, 1.0))
    }

    /// Constructs a color from Qt-compatible integer HSL (`QColor::fromHsl`).
    #[must_use]
    pub fn from_hsl(h: i32, s: i32, l: i32, a: u8) -> Self {
        let hf = if h < 0 {
            -1.0
        } else {
            f64::from(h.rem_euclid(360)) / 360.0
        };
        let sf = f64::from(s.clamp(0, 255)) / 255.0;
        let lf = f64::from(l.clamp(0, 255)) / 255.0;
        Self::from_hsl_f(hf, sf, lf, f64::from(a) / 255.0)
    }

    /// Constructs a color from floating-point HSL (`QColor::fromHslF`).
    /// `h` is in `[0.0, 1.0]` or `< 0.0` for achromatic.
    #[must_use]
    pub fn from_hsl_f(h: f64, s: f64, l: f64, a: f64) -> Self {
        let s = s.clamp(0.0, 1.0);
        let l = l.clamp(0.0, 1.0);
        let alpha = float_to_u8_qt(a);
        if h < 0.0 || s == 0.0 {
            let gray = float_to_u8_qt(l);
            return Self::rgba(gray, gray, gray, alpha);
        }
        let h = h.rem_euclid(1.0);
        let q = if l < 0.5 {
            l * (1.0 + s)
        } else {
            l + s - l * s
        };
        let p = 2.0 * l - q;
        let hue_to_rgb = |mut t: f64| -> f64 {
            if t < 0.0 {
                t += 1.0;
            }
            if t > 1.0 {
                t -= 1.0;
            }
            if t < 1.0 / 6.0 {
                p + (q - p) * 6.0 * t
            } else if t < 0.5 {
                q
            } else if t < 2.0 / 3.0 {
                p + (q - p) * (2.0 / 3.0 - t) * 6.0
            } else {
                p
            }
        };
        let r = hue_to_rgb(h + 1.0 / 3.0);
        let g = hue_to_rgb(h);
        let b = hue_to_rgb(h - 1.0 / 3.0);
        Self::rgba(
            float_to_u8_qt(r),
            float_to_u8_qt(g),
            float_to_u8_qt(b),
            alpha,
        )
    }

    /// Converts to `tg_style::Color`.
    #[must_use]
    pub const fn to_style_color(self) -> tg_style::Color {
        tg_style::Color::rgba(self.r, self.g, self.b, self.a)
    }

    /// Constructs from `tg_style::Color`.
    #[must_use]
    pub const fn from_style_color(color: tg_style::Color) -> Self {
        Self::rgba(color.r, color.g, color.b, color.a)
    }
}

impl From<Color> for tg_style::Color {
    fn from(c: Color) -> Self {
        c.to_style_color()
    }
}

impl From<tg_style::Color> for Color {
    fn from(c: tg_style::Color) -> Self {
        Self::from_style_color(c)
    }
}

/// Checks whether two colors have sufficient perceptual RGB distance.
/// Port of `style::internal::GoodForContrast` in `style_core.cpp`.
#[must_use]
pub fn good_for_contrast(c1: Color, c2: Color) -> bool {
    let r1 = i32::from(c1.r);
    let g1 = i32::from(c1.g);
    let b1 = i32::from(c1.b);
    let r2 = i32::from(c2.r);
    let g2 = i32::from(c2.g);
    let b2 = i32::from(c2.b);
    let r_mean = (r1 + r2) / 2;
    let r = r1 - r2;
    let g = g1 - g2;
    let b = b1 - b2;
    let distance = (((512 + r_mean) * r * r) >> 8) + (4 * g * g) + (((767 - r_mean) * b * b) >> 8);
    distance > MIN_CONTRAST_DISTANCE
}

/// Ensures foreground `over` has minimum contrast and alpha over `under`.
/// Port of `style::internal::EnsureContrast` in `style_core.cpp`.
#[must_use]
pub fn ensure_contrast(over: Color, under: Color) -> Color {
    let over_hsl = over.to_hsl();
    let over_a = i32::from(over.a);
    let under_hsl = under.to_hsl();
    if over_a >= MIN_CONTRAST_ALPHA && good_for_contrast(over, under) {
        return over;
    }
    let new_a = over_a.max(MIN_CONTRAST_ALPHA) as u8;
    let new_l = if over_hsl.l > under_hsl.l && over_hsl.l + CONTRAST_DELTA_L <= 255 {
        over_hsl.l + CONTRAST_DELTA_L
    } else if over_hsl.l < under_hsl.l && over_hsl.l - CONTRAST_DELTA_L >= 0 {
        over_hsl.l - CONTRAST_DELTA_L
    } else if under_hsl.l > 128 {
        under_hsl.l - CONTRAST_DELTA_L
    } else {
        under_hsl.l + CONTRAST_DELTA_L
    };
    Color::from_hsl(over_hsl.h, over_hsl.s, new_l, new_a)
}

/// Computes WCAG relative luminance contrast ratio (`1.0..=21.0`).
/// Port of `Ui::CountContrast` in `ui/color_contrast.cpp`.
#[must_use]
pub fn count_contrast(a: Color, b: Color) -> f64 {
    let luminance = |c: Color| -> f64 {
        let map = |v: f64| -> f64 {
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        map(c.red_f()) * 0.2126 + map(c.green_f()) * 0.7152 + map(c.blue_f()) * 0.0722
    };
    let l1 = luminance(a);
    let l2 = luminance(b);
    let brightest = l1.max(l2);
    let darkest = l1.min(l2);
    (brightest + 0.05) / (darkest + 0.05)
}

/// Computes perceived brightness in `0.0..=1.0`.
/// Port of `Ui::CountPerceivedBrightness` in `ui/color_contrast.cpp`.
#[must_use]
pub fn count_perceived_brightness(color: Color) -> f64 {
    (f64::from(color.r) * 0.2126 + f64::from(color.g) * 0.7152 + f64::from(color.b) * 0.0722)
        / 255.0
}

/// Returns true if `background` is considered light (`> 0.72` perceived brightness).
/// Port of `Ui::IsLightBackground` in `ui/color_contrast.cpp`.
#[must_use]
pub fn is_light_background(background: Color) -> bool {
    count_perceived_brightness(background) > 0.72
}

/// Adjusts `original` to take the HSL hue and saturation of `background` while
/// preserving `original`'s lightness and alpha.
/// Port of `Ui::ThemeAdjustedColor` in `ui/chat/chat_theme.cpp`.
#[must_use]
pub fn theme_adjusted_color(original: Color, background: Color) -> Color {
    let (bg_h, bg_s, _) = background.to_hsl_f();
    let (_, _, orig_l) = original.to_hsl_f();
    Color::from_hsl_f(bg_h, bg_s, orig_l, original.alpha_f())
}

fn srgb_u8_to_linear(channel: u8) -> f64 {
    let value = f64::from(channel) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb_f64(value: f64) -> f64 {
    let value = value.clamp(0.0, 1.0);
    if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

fn linear_from_lch(lightness: f64, chroma: f64, hue: f64) -> [f64; 3] {
    let radians = hue * PI / 180.0;
    let a = chroma * radians.cos();
    let b = chroma * radians.sin();
    let l = lightness + 0.396_337_777_4 * a + 0.215_803_757_3 * b;
    let m = lightness - 0.105_561_345_8 * a - 0.063_854_172_8 * b;
    let s = lightness - 0.089_484_177_5 * a - 1.291_485_548_0 * b;
    let l3 = l * l * l;
    let m3 = m * m * m;
    let s3 = s * s * s;
    [
        4.076_741_662_1 * l3 - 3.307_711_591_3 * m3 + 0.230_969_929_2 * s3,
        -1.268_438_004_6 * l3 + 2.609_757_401_1 * m3 - 0.341_319_396_5 * s3,
        -0.004_196_086_3 * l3 - 0.703_418_614_7 * m3 + 1.707_614_701_0 * s3,
    ]
}

fn in_gamut(lightness: f64, chroma: f64, hue: f64) -> bool {
    linear_from_lch(lightness, chroma, hue)
        .into_iter()
        .all(|v| (-GAMUT_EPSILON..=1.0 + GAMUT_EPSILON).contains(&v))
}

/// Converts an sRGB color to OKLab (`Ui::ToLab`).
#[must_use]
pub fn to_lab(color: Color) -> Oklab {
    let r = srgb_u8_to_linear(color.r);
    let g = srgb_u8_to_linear(color.g);
    let b = srgb_u8_to_linear(color.b);
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    Oklab {
        lightness: 0.210_454_255_3 * l + 0.793_617_785_0 * m - 0.004_072_046_8 * s,
        a: 1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s,
        b: 0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s,
    }
}

/// Computes OKLab chroma (`Ui::Chroma`).
#[must_use]
pub fn chroma(lab: Oklab) -> f64 {
    lab.a.hypot(lab.b)
}

/// Computes OKLab hue in `[0.0, 360.0)` (`Ui::Hue`).
#[must_use]
pub fn hue(lab: Oklab) -> f64 {
    let result = (lab.b.atan2(lab.a) * 180.0 / PI) % 360.0;
    if result < 0.0 { result + 360.0 } else { result }
}

/// Converts OKLCH to gamut-clamped sRGB (`Ui::FromLch`).
#[must_use]
pub fn from_lch(lightness: f64, mut chroma_val: f64, hue_deg: f64) -> Color {
    if !in_gamut(lightness, chroma_val, hue_deg) {
        let mut low = 0.0;
        let mut high = chroma_val;
        for _ in 0..GAMUT_ITERATIONS {
            let middle = (low + high) * 0.5;
            if in_gamut(lightness, middle, hue_deg) {
                low = middle;
            } else {
                high = middle;
            }
        }
        chroma_val = low;
    }
    let rgb = linear_from_lch(lightness, chroma_val, hue_deg);
    let channel =
        |v: f64| -> u8 { (linear_to_srgb_f64(v) * 255.0).round().clamp(0.0, 255.0) as u8 };
    Color::rgb(channel(rgb[0]), channel(rgb[1]), channel(rgb[2]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_serialization_round_trip() {
        let c = Color::from_hex("#40a7e3").unwrap();
        assert_eq!(c, Color::rgb(0x40, 0xa7, 0xe3));
        assert_eq!(c.to_hex_string(), "#40a7e3");
        assert_eq!(c.to_rgb_hex(), "40a7e3");

        let ca = Color::from_hex("#11223380").unwrap();
        assert_eq!(ca, Color::rgba(0x11, 0x22, 0x33, 0x80));
        assert_eq!(ca.to_hex_string(), "#11223380");

        let ser = c.to_serialized_rgb();
        assert_eq!(ser, 0x0040_a7e3);
        assert_eq!(Color::from_serialized_rgb(ser), c);
        assert_eq!(Color::maybe_from_serialized_rgb(u32::MAX), None);

        let ser32 = ca.to_serialized_rgba32();
        assert_eq!(Color::from_serialized_rgba32(ser32), ca);
    }

    #[test]
    fn hsv_and_hsl_known_values() {
        // #40a7e3 = (64, 167, 227): max=227, min=64, delta=163
        // hue = (240 + 60*(64-167)/163) = 240 - 37.914 = 202.086 -> 202
        // sat = round(163/227 * 65535) >> 8 = 47058 >> 8 = 183
        // val = 227
        let blue = Color::rgb(0x40, 0xa7, 0xe3);
        let hsv = blue.to_hsv();
        assert_eq!(hsv.h, 202);
        assert_eq!(hsv.s, 183);
        assert_eq!(hsv.v, 227);

        let gray = Color::rgb(128, 128, 128);
        assert_eq!(
            gray.to_hsv(),
            Hsv {
                h: -1,
                s: 0,
                v: 128
            }
        );
        assert_eq!(
            gray.to_hsl(),
            Hsl {
                h: -1,
                s: 0,
                l: 128
            }
        );

        // Round-trip primary colors (HSV exact, floating HSL exact, 8-bit HSL within 1 LSB
        // because 0.5 lightness maps to 128/255 in Qt's integer HSL)
        for c in [
            Color::rgb(255, 0, 0),
            Color::rgb(0, 255, 0),
            Color::rgb(0, 0, 255),
            Color::rgb(255, 255, 0),
            Color::rgb(0, 255, 255),
            Color::rgb(255, 0, 255),
        ] {
            let h = c.to_hsv();
            assert_eq!(Color::from_hsv(h.h, h.s, h.v, 255), c);
            let (hf, sf, lf) = c.to_hsl_f();
            assert_eq!(Color::from_hsl_f(hf, sf, lf, 1.0), c);
            let l = c.to_hsl();
            let rt = Color::from_hsl(l.h, l.s, l.l, 255);
            assert!(rt.r.abs_diff(c.r) <= 1);
            assert!(rt.g.abs_diff(c.g) <= 1);
            assert!(rt.b.abs_diff(c.b) <= 1);
        }
    }

    #[test]
    fn contrast_and_ensure_contrast() {
        assert!((count_contrast(Color::WHITE, Color::BLACK) - 21.0).abs() < 1e-6);
        assert!((count_contrast(Color::WHITE, Color::WHITE) - 1.0).abs() < 1e-6);
        assert!(good_for_contrast(Color::BLACK, Color::WHITE));
        assert!(!good_for_contrast(Color::WHITE, Color::WHITE));
        let fixed = ensure_contrast(Color::WHITE, Color::WHITE);
        assert_ne!(fixed, Color::WHITE);
        assert!(fixed.lightness() < Color::WHITE.lightness());
    }
}
