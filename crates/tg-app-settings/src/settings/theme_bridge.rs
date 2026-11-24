//! Bridge between `tg-theme` embedded palettes/accent colorizer and `tg-ui` [`Colors`].
//!
//! Port origin: `Telegram/SourceFiles/window/themes/window_themes_embedded.cpp`
//! and `Telegram/SourceFiles/settings/sections/settings_chat.cpp`.

use egui::Color32;
use tg_core::RgbaColor;
use tg_core::settings::EmbeddedThemeType;
use tg_theme::{
    Color, EmbeddedType, Palette as ThemePalette, WallPaper, embedded_themes,
    prepare_embedded_palette,
};
use tg_ui::stand_in::Colors;

/// Converts `tg_core::settings::EmbeddedThemeType` to `tg_theme::EmbeddedType`.
#[must_use]
pub const fn core_to_theme_embedded(kind: EmbeddedThemeType) -> EmbeddedType {
    match kind {
        EmbeddedThemeType::DayBlue => EmbeddedType::DayBlue,
        EmbeddedThemeType::Default => EmbeddedType::Default,
        EmbeddedThemeType::Night => EmbeddedType::Night,
        EmbeddedThemeType::NightGreen => EmbeddedType::NightGreen,
    }
}

/// Converts `tg_theme::EmbeddedType` to `tg_core::settings::EmbeddedThemeType`.
#[must_use]
pub const fn theme_to_core_embedded(kind: EmbeddedType) -> EmbeddedThemeType {
    match kind {
        EmbeddedType::DayBlue => EmbeddedThemeType::DayBlue,
        EmbeddedType::Default => EmbeddedThemeType::Default,
        EmbeddedType::Night => EmbeddedThemeType::Night,
        EmbeddedType::NightGreen => EmbeddedThemeType::NightGreen,
    }
}

/// Converts a `tg_theme::Color` into an `egui::Color32`.
#[must_use]
pub fn theme_color_to_egui(c: Color) -> Color32 {
    if c.a == 255 {
        Color32::from_rgb(c.r, c.g, c.b)
    } else {
        Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
    }
}

/// Converts a `tg_theme::Color` into `tg_core::RgbaColor`.
#[must_use]
pub const fn theme_color_to_core(c: Color) -> RgbaColor {
    RgbaColor {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    }
}

/// Converts a `tg_core::RgbaColor` into `tg_theme::Color`.
#[must_use]
pub const fn core_color_to_theme(c: RgbaColor) -> Color {
    Color::rgba(c.r, c.g, c.b, c.a)
}

/// Live `tg-theme` palette implementing `tg-ui`'s [`Colors`] trait.
#[derive(Debug, Clone)]
pub struct LiveThemeColors {
    /// Active embedded theme kind.
    pub kind: EmbeddedType,
    /// Optional custom accent override for `kind`.
    pub accent: Option<Color>,
    /// Finalized `tg-theme` palette.
    pub palette: ThemePalette,
}

impl LiveThemeColors {
    /// Builds a [`LiveThemeColors`] for `kind` and optional `accent`.
    #[must_use]
    pub fn build(kind: EmbeddedType, accent: Option<Color>) -> Self {
        let palette = prepare_embedded_palette(kind, accent).unwrap_or_default();
        Self {
            kind,
            accent,
            palette,
        }
    }

    /// Returns `true` if the active embedded theme is dark (`Night` or `NightGreen`).
    #[must_use]
    pub const fn is_dark(&self) -> bool {
        self.kind.is_dark()
    }

    /// Rebuilds the palette if `kind` or `accent` changed.
    pub fn update(&mut self, kind: EmbeddedType, accent: Option<Color>) {
        if self.kind != kind || self.accent != accent {
            self.kind = kind;
            self.accent = accent;
            self.palette = prepare_embedded_palette(kind, accent).unwrap_or_default();
        }
    }

    /// Returns human-readable name for the active embedded theme.
    #[must_use]
    pub fn theme_name(&self) -> &'static str {
        embedded_themes()
            .into_iter()
            .find(|s| s.kind == self.kind)
            .map_or("Classic", |s| s.name)
    }
}

impl Default for LiveThemeColors {
    fn default() -> Self {
        Self::build(EmbeddedType::Default, None)
    }
}

impl Colors for LiveThemeColors {
    fn get(&self, name: &str) -> Option<Color32> {
        self.palette.get(name)
    }
}

/// Built-in chat background preset backed by [`tg_theme::WallPaper`].
#[derive(Debug, Clone)]
pub struct BackgroundPreset {
    /// Stable preset identifier.
    pub id: &'static str,
    /// Human-readable wallpaper label.
    pub name: &'static str,
    /// Underlying `tg-theme` wallpaper model.
    pub wallpaper: WallPaper,
    /// Top-left preview gradient color.
    pub color_a: Color32,
    /// Bottom-right preview gradient color.
    pub color_b: Color32,
    /// Optional third accent blend color.
    pub color_c: Color32,
}

/// Returns built-in day/night gradient & solid chat background presets.
#[must_use]
pub fn builtin_backgrounds() -> Vec<BackgroundPreset> {
    struct BgSpec {
        id: &'static str,
        name: &'static str,
        a: [u8; 3],
        b: [u8; 3],
        c: [u8; 3],
    }
    let specs: &[BgSpec] = &[
        BgSpec {
            id: "classic_meadow",
            name: "Classic Meadow",
            a: [0xDB, 0xDD, 0xBB],
            b: [0x6B, 0xA5, 0x87],
            c: [0xD5, 0xD8, 0x8D],
        },
        BgSpec {
            id: "day_sky",
            name: "Day Sky",
            a: [0xD7, 0xEC, 0xF8],
            b: [0x93, 0xCB, 0xEB],
            c: [0xE6, 0xF4, 0xFD],
        },
        BgSpec {
            id: "arctic_mist",
            name: "Arctic Mist",
            a: [0xE3, 0xEA, 0xF2],
            b: [0xB5, 0xC9, 0xDC],
            c: [0xF0, 0xF4, 0xF8],
        },
        BgSpec {
            id: "warm_sand",
            name: "Warm Sand",
            a: [0xF5, 0xE6, 0xD3],
            b: [0xE4, 0xBF, 0x99],
            c: [0xFA, 0xF0, 0xE2],
        },
        BgSpec {
            id: "rose_quartz",
            name: "Rose Quartz",
            a: [0xF2, 0xD9, 0xE6],
            b: [0xD4, 0xA5, 0xC9],
            c: [0xFA, 0xEC, 0xF2],
        },
        BgSpec {
            id: "midnight_blue",
            name: "Midnight Blue",
            a: [0x0E, 0x16, 0x21],
            b: [0x1A, 0x2A, 0x3D],
            c: [0x12, 0x20, 0x30],
        },
        BgSpec {
            id: "pine_night",
            name: "Pine Night",
            a: [0x13, 0x21, 0x1E],
            b: [0x1F, 0x3A, 0x34],
            c: [0x17, 0x2B, 0x26],
        },
        BgSpec {
            id: "obsidian",
            name: "Obsidian",
            a: [0x14, 0x14, 0x18],
            b: [0x24, 0x24, 0x2E],
            c: [0x1B, 0x1B, 0x22],
        },
    ];

    specs
        .iter()
        .map(|s| {
            let wp = WallPaper::custom().with_background_colors(vec![
                Color::rgb(s.a[0], s.a[1], s.a[2]),
                Color::rgb(s.b[0], s.b[1], s.b[2]),
                Color::rgb(s.c[0], s.c[1], s.c[2]),
            ]);
            BackgroundPreset {
                id: s.id,
                name: s.name,
                wallpaper: wp,
                color_a: Color32::from_rgb(s.a[0], s.a[1], s.a[2]),
                color_b: Color32::from_rgb(s.b[0], s.b[1], s.b[2]),
                color_c: Color32::from_rgb(s.c[0], s.c[1], s.c[2]),
            }
        })
        .collect()
}
