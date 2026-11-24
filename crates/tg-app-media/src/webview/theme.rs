//! `tg-theme` palette integration for Instant View and Bot Mini App windows.
//!
//! Port origin: `Telegram/SourceFiles/window/themes/window_theme.cpp` (`WebViewParams`),
//! `Telegram/SourceFiles/ui/chat/attach/attach_bot_webview.cpp` (`ComputeContrastColors`).

use egui::{Color32, Visuals};
use tg_theme::{Color, Palette as TgPalette, is_light_background};
use tg_ui::stand_in::Colors;
use tg_ui::tg_style::ColorResolver;

/// Builds a full `tg-theme` [`TgPalette`] via `tg-ui`'s [`ThemeManager`] for [`ThemeMode`].
pub use tg_app_core::core_types::misc::tg_palette_for_mode;

/// Converts a `tg-theme` [`Color`] to an `egui::Color32`.
#[inline]
#[must_use]
pub fn color_to_egui(c: Color) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

/// Converts an `egui::Color32` to a `tg-theme` [`Color`].
#[inline]
#[must_use]
pub fn egui_to_color(c: Color32) -> Color {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    Color::rgba(r, g, b, a)
}

/// Computes `tg-theme` [`WebViewThemeParams`] for WebView2 background, scrollbars, and Mini App JSON.
pub use tg_app_core::core_types::misc::build_webview_theme_params;

/// Adapter exposing a live `tg-theme` [`TgPalette`] as `tg-ui`'s [`Colors`] lookup trait.
#[derive(Debug, Clone, Copy)]
pub struct TgThemeColors<'a>(pub &'a TgPalette);

impl Colors for TgThemeColors<'_> {
    fn get(&self, name: &str) -> Option<Color32> {
        self.0.color_by_name(name).map(color_to_egui)
    }
}

impl ColorResolver for TgThemeColors<'_> {
    fn resolve(&self, name: &str) -> Option<tg_ui::tg_style::Color> {
        self.0
            .color_by_name(name)
            .map(|c| tg_ui::tg_style::Color::rgba(c.r, c.g, c.b, c.a))
    }
}

/// Foreground and subtle control colors computed for an arbitrary header or bottom-bar background.
///
/// Mirrors `ComputeContrastColors` in `ui/chat/attach/attach_bot_webview.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContrastColors {
    /// Primary title text color.
    pub text: Color32,
    /// Secondary/subtitle text color.
    pub muted: Color32,
    /// Header icon button foreground color.
    pub control: Color32,
    /// Hover fill tint for header icon buttons.
    pub control_hover: Color32,
    /// 1px separator color along the chrome edge.
    pub divider: Color32,
    /// Whether the background is considered light (`true`) or dark (`false`).
    pub is_light: bool,
}

/// Computes high-contrast chrome foreground colors for `background`.
#[must_use]
pub fn compute_contrast_colors(background: Color) -> ContrastColors {
    let is_light = is_light_background(background);
    if is_light {
        ContrastColors {
            text: Color32::from_rgb(0x22, 0x22, 0x22),
            muted: Color32::from_rgb(0x70, 0x75, 0x79),
            control: Color32::from_rgb(0x70, 0x75, 0x79),
            control_hover: Color32::from_rgba_unmultiplied(0x00, 0x00, 0x00, 0x16),
            divider: Color32::from_rgba_unmultiplied(0x00, 0x00, 0x00, 0x1A),
            is_light: true,
        }
    } else {
        ContrastColors {
            text: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            muted: Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0xB8),
            control: Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0xD8),
            control_hover: Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0x22),
            divider: Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0x1E),
            is_light: false,
        }
    }
}

/// Builds `egui::Visuals` from a `tg-theme` [`TgPalette`] for secondary WebView windows.
#[must_use]
pub fn egui_visuals_from_tg_palette(palette: &TgPalette, is_night: bool) -> Visuals {
    let mut visuals = if is_night {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    let window_bg = color_to_egui(palette.window_bg);
    let window_fg = color_to_egui(palette.window_fg);
    let window_bg_over = color_to_egui(palette.window_bg_over);
    let active_bg = color_to_egui(palette.window_bg_active);
    let active_fg = color_to_egui(palette.window_fg_active);
    let divider = color_to_egui(palette.menu_separator_fg);

    visuals.panel_fill = window_bg;
    visuals.window_fill = window_bg;
    visuals.faint_bg_color = window_bg_over;
    visuals.override_text_color = Some(window_fg);
    visuals.selection.bg_fill = active_bg;
    visuals.selection.stroke.color = active_fg;
    visuals.widgets.noninteractive.bg_fill = window_bg;
    visuals.widgets.noninteractive.bg_stroke.color = divider;
    visuals.widgets.inactive.bg_fill = window_bg_over;
    visuals.widgets.inactive.bg_stroke.color = divider;
    visuals.widgets.hovered.bg_fill = window_bg_over;
    visuals.widgets.hovered.bg_stroke.color = active_bg;
    visuals.widgets.active.bg_fill = active_bg;
    visuals.widgets.active.bg_stroke.color = active_bg;
    visuals
}
