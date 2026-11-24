//! Theme color adapter bridging `tg-app`'s Day/Night [`ThemeMode`] to `tg-ui`'s [`Colors`] trait.
//!
//! Uses `tg_theme::prepare_embedded_palette` so all 588 Telegram Desktop palette tokens
//! (including `infoIconFg`, `profileVerifiedCheckBg`, `overviewCheckBg*`, `dialogsOnlineBadgeFg`,
//! `premiumButtonBg2`, `white`, `transparent`) match tdesktop in both Day and Night themes.
//!
//! Port origin: `Telegram/lib_ui/ui/colors.palette` and `Telegram/SourceFiles/info/info.style`.

use egui::Color32;
use tg_theme::{EmbeddedType, Palette, prepare_embedded_palette};
use tg_ui::stand_in::Colors;

use tg_app_core::stand_in::theme::ThemeMode;

/// `tg-ui` [`Colors`] implementation backed by the embedded Telegram Desktop Day/Night palette.
#[derive(Debug, Clone)]
pub struct InfoColors {
    palette: Palette,
}

impl InfoColors {
    /// Creates a `Colors` adapter matching `mode`.
    #[must_use]
    pub fn for_mode(mode: ThemeMode) -> Self {
        let kind = match mode {
            ThemeMode::Day => EmbeddedType::Default,
            ThemeMode::Night => EmbeddedType::Night,
        };
        let palette = prepare_embedded_palette(kind, None).unwrap_or_else(|_| Palette::default());
        Self { palette }
    }

    /// Creates a Day-theme color palette.
    #[must_use]
    pub fn day() -> Self {
        Self::for_mode(ThemeMode::Day)
    }

    /// Creates a Night-theme color palette.
    #[must_use]
    pub fn night() -> Self {
        Self::for_mode(ThemeMode::Night)
    }
}

impl Default for InfoColors {
    fn default() -> Self {
        Self::day()
    }
}

impl Colors for InfoColors {
    fn get(&self, name: &str) -> Option<Color32> {
        let c = self.palette.get(name)?;
        Some(Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), c.a()))
    }
}
