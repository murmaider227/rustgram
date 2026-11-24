//! RGBA color representation and Telegram Desktop statistics palette colors.
//!
//! Port of color definitions from `resources/colors.palette` and
//! `FillLineColorsByKey` in `Telegram/SourceFiles/statistics/chart_widget.cpp`.

/// An 8-bit sRGBA color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Color {
    /// Red channel (0..=255).
    pub r: u8,
    /// Green channel (0..=255).
    pub g: u8,
    /// Blue channel (0..=255).
    pub b: u8,
    /// Alpha channel (0..=255).
    pub a: u8,
}

impl Color {
    /// Fully transparent black.
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);
    /// Opaque white (`#ffffff`).
    pub const WHITE: Self = Self::rgb(0xff, 0xff, 0xff);
    /// Opaque black (`#000000`).
    pub const BLACK: Self = Self::rgb(0x00, 0x00, 0x00);

    /// Creates an opaque RGB color.
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Creates an RGBA color.
    #[must_use]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Parses `#RRGGBB`, `#RRGGBBAA`, or `#RGB` hex strings.
    #[must_use]
    pub fn from_hex(hex: &str) -> Option<Self> {
        let s = hex.strip_prefix('#')?;
        match s.len() {
            3 => {
                let r = u8::from_str_radix(&s[0..1], 16).ok()?;
                let g = u8::from_str_radix(&s[1..2], 16).ok()?;
                let b = u8::from_str_radix(&s[2..3], 16).ok()?;
                Some(Self::rgb(r * 17, g * 17, b * 17))
            }
            6 => {
                let r = u8::from_str_radix(&s[0..2], 16).ok()?;
                let g = u8::from_str_radix(&s[2..4], 16).ok()?;
                let b = u8::from_str_radix(&s[4..6], 16).ok()?;
                Some(Self::rgb(r, g, b))
            }
            8 => {
                let r = u8::from_str_radix(&s[0..2], 16).ok()?;
                let g = u8::from_str_radix(&s[2..4], 16).ok()?;
                let b = u8::from_str_radix(&s[4..6], 16).ok()?;
                let a = u8::from_str_radix(&s[6..8], 16).ok()?;
                Some(Self::rgba(r, g, b, a))
            }
            _ => None,
        }
    }

    /// Returns a copy of this color with its alpha multiplied by `factor` (clamped to `0.0..=1.0`).
    #[must_use]
    pub fn with_alpha_factor(self, factor: f64) -> Self {
        let clamped = factor.clamp(0.0, 1.0);
        let new_a = (f64::from(self.a) * clamped).round().clamp(0.0, 255.0) as u8;
        Self { a: new_a, ..self }
    }

    /// Converts to a `tiny_skia::Color`.
    #[must_use]
    pub fn to_skia(self) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(self.r, self.g, self.b, self.a)
    }

    /// Formats as `#rrggbb` (or `#rrggbbaa` if translucent).
    #[must_use]
    pub fn to_hex(self) -> String {
        if self.a == 255 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
    }
}

/// Theme color palette used by statistics charts and overview cards.
///
/// Defaults match Telegram Desktop's light theme (`resources/colors.palette`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChartPalette {
    /// Chart canvas background (`boxBg`).
    pub box_bg: Color,
    /// Primary text and ruler line base color (`boxTextFg`).
    pub box_text_fg: Color,
    /// Divider / empty area fill (`boxDividerBg`).
    pub box_divider_bg: Color,
    /// Secondary caption text (`windowSubTextFg`).
    pub window_sub_text_fg: Color,
    /// Header title color (`windowActiveTextFg`).
    pub window_active_text_fg: Color,
    /// White foreground used for pie slice labels and footer handle grips (`premiumButtonFg`).
    pub premium_button_fg: Color,
    /// Footer inactive dimmed region (`statisticsChartInactive`).
    pub chart_inactive: Color,
    /// Footer active handle frame (`statisticsChartActive`).
    pub chart_active: Color,
    /// Positive delta text color (`settingsIconBg2`).
    pub delta_positive: Color,
    /// Negative delta text color (`menuIconAttentionColor` / `attentionButtonFg`).
    pub delta_negative: Color,
    /// Named chart line color: `BLUE`.
    pub line_blue: Color,
    /// Named chart line color: `GREEN`.
    pub line_green: Color,
    /// Named chart line color: `RED`.
    pub line_red: Color,
    /// Named chart line color: `GOLDEN`.
    pub line_golden: Color,
    /// Named chart line color: `LIGHTBLUE`.
    pub line_lightblue: Color,
    /// Named chart line color: `LIGHTGREEN`.
    pub line_lightgreen: Color,
    /// Named chart line color: `ORANGE`.
    pub line_orange: Color,
    /// Named chart line color: `INDIGO`.
    pub line_indigo: Color,
    /// Named chart line color: `PURPLE`.
    pub line_purple: Color,
    /// Named chart line color: `CYAN`.
    pub line_cyan: Color,
}

impl Default for ChartPalette {
    fn default() -> Self {
        Self::light()
    }
}

impl ChartPalette {
    /// Telegram Desktop default light palette (`resources/colors.palette`).
    #[must_use]
    pub const fn light() -> Self {
        Self {
            box_bg: Color::rgb(0xff, 0xff, 0xff),
            box_text_fg: Color::rgb(0x00, 0x00, 0x00),
            box_divider_bg: Color::rgb(0xf1, 0xf1, 0xf1),
            window_sub_text_fg: Color::rgb(0x99, 0x99, 0x99),
            window_active_text_fg: Color::rgb(0x16, 0x8a, 0xcd),
            premium_button_fg: Color::rgb(0xff, 0xff, 0xff),
            chart_inactive: Color::rgba(0xe2, 0xee, 0xf9, 0x99),
            chart_active: Color::rgba(0xba, 0xcc, 0xd9, 0xd8),
            delta_positive: Color::rgb(0x6d, 0xc5, 0x34),
            delta_negative: Color::rgb(0xd1, 0x4e, 0x4e),
            line_blue: Color::rgb(0x32, 0x7f, 0xe5),
            line_green: Color::rgb(0x61, 0xc7, 0x52),
            line_red: Color::rgb(0xe0, 0x53, 0x56),
            line_golden: Color::rgb(0xeb, 0xa5, 0x2d),
            line_lightblue: Color::rgb(0x58, 0xa8, 0xed),
            line_lightgreen: Color::rgb(0x8f, 0xcf, 0x39),
            line_orange: Color::rgb(0xf2, 0x8c, 0x39),
            line_indigo: Color::rgb(0x7f, 0x79, 0xf3),
            line_purple: Color::rgb(0x9f, 0x79, 0xe8),
            line_cyan: Color::rgb(0x40, 0xd0, 0xca),
        }
    }

    /// Telegram Desktop night palette variant for chart rendering.
    #[must_use]
    pub const fn dark() -> Self {
        Self {
            box_bg: Color::rgb(0x17, 0x21, 0x2b),
            box_text_fg: Color::rgb(0xf5, 0xf5, 0xf5),
            box_divider_bg: Color::rgb(0x0e, 0x16, 0x21),
            window_sub_text_fg: Color::rgb(0x70, 0x84, 0x99),
            window_active_text_fg: Color::rgb(0x6a, 0xb2, 0xf2),
            premium_button_fg: Color::rgb(0xff, 0xff, 0xff),
            chart_inactive: Color::rgba(0x0e, 0x16, 0x21, 0x99),
            chart_active: Color::rgba(0x4b, 0x62, 0x79, 0xd8),
            delta_positive: Color::rgb(0x6d, 0xc5, 0x34),
            delta_negative: Color::rgb(0xec, 0x66, 0x66),
            line_blue: Color::rgb(0x32, 0x7f, 0xe5),
            line_green: Color::rgb(0x61, 0xc7, 0x52),
            line_red: Color::rgb(0xe0, 0x53, 0x56),
            line_golden: Color::rgb(0xeb, 0xa5, 0x2d),
            line_lightblue: Color::rgb(0x58, 0xa8, 0xed),
            line_lightgreen: Color::rgb(0x8f, 0xcf, 0x39),
            line_orange: Color::rgb(0xf2, 0x8c, 0x39),
            line_indigo: Color::rgb(0x7f, 0x79, 0xf3),
            line_purple: Color::rgb(0x9f, 0x79, 0xe8),
            line_cyan: Color::rgb(0x40, 0xd0, 0xca),
        }
    }

    /// Resolves a Telegram chart color key (e.g. `"BLUE"`, `"GREEN"`) to a palette color.
    ///
    /// Matches `FillLineColorsByKey` in `statistics/chart_widget.cpp`.
    #[must_use]
    pub fn line_color_by_key(&self, key: &str) -> Option<Color> {
        match key {
            "BLUE" => Some(self.line_blue),
            "GREEN" => Some(self.line_green),
            "RED" => Some(self.line_red),
            "GOLDEN" => Some(self.line_golden),
            "LIGHTBLUE" => Some(self.line_lightblue),
            "LIGHTGREEN" => Some(self.line_lightgreen),
            "ORANGE" => Some(self.line_orange),
            "INDIGO" => Some(self.line_indigo),
            "PURPLE" => Some(self.line_purple),
            "CYAN" => Some(self.line_cyan),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_colors() {
        assert_eq!(
            Color::from_hex("#327fe5"),
            Some(Color::rgb(0x32, 0x7f, 0xe5))
        );
        assert_eq!(
            Color::from_hex("#e2eef999"),
            Some(Color::rgba(0xe2, 0xee, 0xf9, 0x99))
        );
        assert_eq!(Color::from_hex("#f0a"), Some(Color::rgb(0xff, 0x00, 0xaa)));
        assert_eq!(Color::from_hex("327fe5"), None);
        assert_eq!(Color::from_hex("#zzzzzz"), None);
    }

    #[test]
    fn resolves_all_named_chart_line_keys() {
        let p = ChartPalette::light();
        assert_eq!(
            p.line_color_by_key("BLUE"),
            Some(Color::rgb(0x32, 0x7f, 0xe5))
        );
        assert_eq!(
            p.line_color_by_key("GREEN"),
            Some(Color::rgb(0x61, 0xc7, 0x52))
        );
        assert_eq!(
            p.line_color_by_key("RED"),
            Some(Color::rgb(0xe0, 0x53, 0x56))
        );
        assert_eq!(
            p.line_color_by_key("GOLDEN"),
            Some(Color::rgb(0xeb, 0xa5, 0x2d))
        );
        assert_eq!(
            p.line_color_by_key("LIGHTBLUE"),
            Some(Color::rgb(0x58, 0xa8, 0xed))
        );
        assert_eq!(
            p.line_color_by_key("LIGHTGREEN"),
            Some(Color::rgb(0x8f, 0xcf, 0x39))
        );
        assert_eq!(
            p.line_color_by_key("ORANGE"),
            Some(Color::rgb(0xf2, 0x8c, 0x39))
        );
        assert_eq!(
            p.line_color_by_key("INDIGO"),
            Some(Color::rgb(0x7f, 0x79, 0xf3))
        );
        assert_eq!(
            p.line_color_by_key("PURPLE"),
            Some(Color::rgb(0x9f, 0x79, 0xe8))
        );
        assert_eq!(
            p.line_color_by_key("CYAN"),
            Some(Color::rgb(0x40, 0xd0, 0xca))
        );
        assert_eq!(p.line_color_by_key("UNKNOWN"), None);
    }
}
