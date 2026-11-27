//! Theme palette bridge backed by `tg-ui`'s [`tg_ui::theme::ThemeManager`] and `tg-theme`.
//!
//! Port origin: `resources/colors.palette`, `Telegram/SourceFiles/window/window.style`,
//! and `Telegram/SourceFiles/dialogs/dialogs.style`.

use egui::{Color32, Visuals};
use serde::{Deserialize, Serialize};
use tg_theme::EmbeddedType;
use tg_ui::theme::{Colors, PaletteColor, ThemeManager};

#[cfg(test)]
#[path = "theme_palette_cache_tests.rs"]
mod palette_cache_tests;

thread_local! {
    /// Last [`Palette::from_theme_manager`] result and the theme key it was built for.
    static DERIVED_PALETTE: std::cell::Cell<Option<((u64, u64), Palette)>> =
        const { std::cell::Cell::new(None) };
}

/// Selected color scheme mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ThemeMode {
    /// Classic Telegram Desktop Day palette (`resources/colors.palette`).
    #[default]
    Day,
    /// Telegram Desktop Night palette.
    Night,
}

impl ThemeMode {
    /// Toggles between Day and Night modes.
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Day => Self::Night,
            Self::Night => Self::Day,
        }
    }

    /// Returns `true` when Night Mode is active.
    #[must_use]
    pub const fn is_night(self) -> bool {
        matches!(self, Self::Night)
    }

    /// Converts a `tg-theme` [`EmbeddedType`] into coarse Day/Night [`ThemeMode`].
    #[must_use]
    pub const fn from_embedded(kind: EmbeddedType) -> Self {
        if kind.is_dark() {
            Self::Night
        } else {
            Self::Day
        }
    }

    /// Maps this coarse mode onto its default `tg-theme` [`EmbeddedType`].
    #[must_use]
    pub const fn to_embedded(self) -> EmbeddedType {
        match self {
            Self::Day => EmbeddedType::Default,
            Self::Night => EmbeddedType::Night,
        }
    }
}

/// Semantic color palette mirroring `tdesktop`'s `colors.palette`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// Primary window surface (`windowBg`).
    pub window_bg: Color32,
    /// Hovered row / subtle surface (`windowBgOver`).
    pub window_bg_over: Color32,
    /// Primary text (`windowFg`).
    pub window_fg: Color32,
    /// Bold header text (`windowBoldFg`).
    pub window_bold_fg: Color32,
    /// Secondary / muted text (`windowSubTextFg`).
    pub window_sub_text_fg: Color32,
    /// Active accent blue (`windowBgActive`).
    pub window_bg_active: Color32,
    /// Text on accent background (`windowFgActive`).
    pub window_fg_active: Color32,
    /// Active inline link / online status text (`windowActiveTextFg`).
    pub window_active_text_fg: Color32,
    /// 1px divider / shadow line (`shadowFg` / `menuSeparatorFg`).
    pub divider: Color32,
    /// Danger / error accent (`attentionButtonFg`).
    pub attention_fg: Color32,
    /// Search input inactive background (`filterInputInactiveBg`).
    pub input_bg: Color32,
    /// Search input active background (`filterInputActiveBg`).
    pub input_active_bg: Color32,
    /// Search input focus ring (`filterInputBorderFg`).
    pub input_border_active: Color32,
    /// Dialogs selected row background (`dialogsBgActive`).
    pub dialogs_bg_active: Color32,
    /// Dialogs unread badge background (unmuted).
    pub unread_badge_bg: Color32,
    /// Dialogs unread badge background (muted).
    pub unread_badge_muted_bg: Color32,
    /// Chat canvas background behind message bubbles.
    pub chat_canvas_bg: Color32,
    /// Incoming message bubble fill (`msgInBg`).
    pub msg_in_bg: Color32,
    /// Outgoing message bubble fill (`msgOutBg`).
    pub msg_out_bg: Color32,
    /// Message timestamp text (`msgInDateFg` / `msgOutDateFg`).
    pub msg_date_fg: Color32,
    /// Left folders sidebar background (`sideBarBg`).
    pub sidebar_bg: Color32,
    /// Active folder highlight in sidebar (`sideBarBgActive`).
    pub sidebar_bg_active: Color32,
    /// Sidebar icon/text tint (`sideBarTextFg`).
    pub sidebar_fg: Color32,
    /// Sidebar active icon/text tint (`sideBarTextFgActive`).
    pub sidebar_fg_active: Color32,
    /// Modal backdrop dimming scrim (`slideFadeOutBg`).
    pub layer_backdrop: Color32,
    /// Group member name colours by palette slot (`historyPeer1NameFg`..`historyPeer8NameFg`).
    pub peer_name_fg: [Color32; 8],
    /// The same in a selected message (`historyPeerNNameFgSelected`).
    pub peer_name_fg_selected: [Color32; 8],
}

/// `historyPeerNNameFg` names by palette slot.
pub const PEER_NAME_FG_KEYS: [&str; 8] = [
    "historyPeer1NameFg",
    "historyPeer2NameFg",
    "historyPeer3NameFg",
    "historyPeer4NameFg",
    "historyPeer5NameFg",
    "historyPeer6NameFg",
    "historyPeer7NameFg",
    "historyPeer8NameFg",
];

/// `historyPeerNNameFgSelected` names by palette slot.
pub const PEER_NAME_FG_SELECTED_KEYS: [&str; 8] = [
    "historyPeer1NameFgSelected",
    "historyPeer2NameFgSelected",
    "historyPeer3NameFgSelected",
    "historyPeer4NameFgSelected",
    "historyPeer5NameFgSelected",
    "historyPeer6NameFgSelected",
    "historyPeer7NameFgSelected",
    "historyPeer8NameFgSelected",
];

/// Day `historyPeerNNameFg` (`colors.palette`; slot 4 is `windowActiveTextFg`). Selected
/// messages use the same colours there.
const DAY_PEER_NAME_FG: [Color32; 8] = [
    Color32::from_rgb(0xC0, 0x3D, 0x33),
    Color32::from_rgb(0x4F, 0xAD, 0x2D),
    Color32::from_rgb(0xD0, 0x93, 0x06),
    Color32::from_rgb(0x16, 0x8A, 0xCD),
    Color32::from_rgb(0x85, 0x44, 0xD6),
    Color32::from_rgb(0xCD, 0x40, 0x73),
    Color32::from_rgb(0x29, 0x96, 0xAD),
    Color32::from_rgb(0xCE, 0x67, 0x1B),
];

/// Night `historyPeerNNameFg` (`night.tdesktop-theme`); selected messages use white there.
const NIGHT_PEER_NAME_FG: [Color32; 8] = [
    Color32::from_rgb(0xFB, 0x61, 0x69),
    Color32::from_rgb(0x85, 0xDE, 0x85),
    Color32::from_rgb(0xF3, 0xBC, 0x5C),
    Color32::from_rgb(0x65, 0xBD, 0xF3),
    Color32::from_rgb(0xB4, 0x8B, 0xF2),
    Color32::from_rgb(0xFF, 0x56, 0x94),
    Color32::from_rgb(0x62, 0xD4, 0xE3),
    Color32::from_rgb(0xFA, 0xA3, 0x57),
];

impl Palette {
    /// Returns the palette corresponding to `mode`.
    #[must_use]
    pub const fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Day => Self::day(),
            ThemeMode::Night => Self::night(),
        }
    }

    /// Classic Telegram Desktop Day palette (`resources/colors.palette`).
    #[must_use]
    pub const fn day() -> Self {
        Self {
            window_bg: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            window_bg_over: Color32::from_rgb(0xF1, 0xF1, 0xF1),
            window_fg: Color32::from_rgb(0x00, 0x00, 0x00),
            window_bold_fg: Color32::from_rgb(0x22, 0x22, 0x22),
            window_sub_text_fg: Color32::from_rgb(0x70, 0x75, 0x79),
            window_bg_active: Color32::from_rgb(0x40, 0xA7, 0xE3),
            window_fg_active: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            window_active_text_fg: Color32::from_rgb(0x16, 0x8A, 0xCD),
            divider: Color32::from_rgb(0xE6, 0xE6, 0xE6),
            attention_fg: Color32::from_rgb(0xD1, 0x4E, 0x4E),
            input_bg: Color32::from_rgb(0xF1, 0xF1, 0xF1),
            input_active_bg: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            input_border_active: Color32::from_rgb(0x54, 0xC3, 0xF3),
            dialogs_bg_active: Color32::from_rgb(0x41, 0x9F, 0xD9),
            unread_badge_bg: Color32::from_rgb(0x40, 0xA7, 0xE3),
            unread_badge_muted_bg: Color32::from_rgb(0xBB, 0xBB, 0xBB),
            chat_canvas_bg: Color32::from_rgb(0xDB, 0xE7, 0xF0),
            msg_in_bg: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            msg_out_bg: Color32::from_rgb(0xEF, 0xFD, 0xDE),
            msg_date_fg: Color32::from_rgb(0x78, 0x8A, 0x96),
            sidebar_bg: Color32::from_rgb(0x29, 0x3A, 0x4C),
            sidebar_bg_active: Color32::from_rgb(0x1E, 0x2C, 0x3A),
            sidebar_fg: Color32::from_rgb(0x95, 0xA5, 0xB5),
            sidebar_fg_active: Color32::from_rgb(0x5E, 0xB5, 0xF7),
            layer_backdrop: Color32::from_rgba_premultiplied(0, 0, 0, 96),
            peer_name_fg: DAY_PEER_NAME_FG,
            peer_name_fg_selected: DAY_PEER_NAME_FG,
        }
    }

    /// Telegram Desktop Night palette.
    #[must_use]
    pub const fn night() -> Self {
        Self {
            window_bg: Color32::from_rgb(0x17, 0x21, 0x2B),
            window_bg_over: Color32::from_rgb(0x20, 0x2B, 0x36),
            window_fg: Color32::from_rgb(0xF5, 0xF5, 0xF5),
            window_bold_fg: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            window_sub_text_fg: Color32::from_rgb(0x7F, 0x91, 0xA4),
            window_bg_active: Color32::from_rgb(0x52, 0x88, 0xC1),
            window_fg_active: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            window_active_text_fg: Color32::from_rgb(0x6A, 0xB2, 0xF2),
            divider: Color32::from_rgb(0x10, 0x19, 0x21),
            attention_fg: Color32::from_rgb(0xEC, 0x5B, 0x5B),
            input_bg: Color32::from_rgb(0x24, 0x2F, 0x3D),
            input_active_bg: Color32::from_rgb(0x24, 0x2F, 0x3D),
            input_border_active: Color32::from_rgb(0x52, 0x88, 0xC1),
            dialogs_bg_active: Color32::from_rgb(0x2B, 0x52, 0x78),
            unread_badge_bg: Color32::from_rgb(0x52, 0x88, 0xC1),
            unread_badge_muted_bg: Color32::from_rgb(0x3E, 0x54, 0x6A),
            chat_canvas_bg: Color32::from_rgb(0x0E, 0x16, 0x21),
            msg_in_bg: Color32::from_rgb(0x18, 0x25, 0x33),
            msg_out_bg: Color32::from_rgb(0x2B, 0x52, 0x78),
            msg_date_fg: Color32::from_rgb(0x7F, 0x91, 0xA4),
            sidebar_bg: Color32::from_rgb(0x0E, 0x16, 0x21),
            sidebar_bg_active: Color32::from_rgb(0x17, 0x21, 0x2B),
            sidebar_fg: Color32::from_rgb(0x76, 0x8C, 0x9E),
            sidebar_fg_active: Color32::from_rgb(0x6A, 0xB2, 0xF2),
            layer_backdrop: Color32::from_rgba_premultiplied(0, 0, 0, 140),
            peer_name_fg: NIGHT_PEER_NAME_FG,
            peer_name_fg_selected: [Color32::WHITE; 8],
        }
    }

    /// Builds a [`Palette`] directly from a live `tg-ui` [`ThemeManager`] (respecting
    /// embedded Day/Night/Tinted/DayBlue themes and custom accent colors).
    #[must_use]
    ///
    /// The result is cached per thread by [`ThemeManager::palette_cache_key`], which changes on
    /// every palette change, so the per-frame callers get a copy instead of a rebuild.
    pub fn from_theme_manager(tm: &ThemeManager) -> Self {
        let key = tm.palette_cache_key();
        if let Some((_, cached)) =
            DERIVED_PALETTE.with(|slot| slot.get().filter(|(k, _)| *k == key))
        {
            return cached;
        }
        let palette = Self::from_colors(tm, tm.is_dark());
        DERIVED_PALETTE.with(|slot| slot.set(Some((key, palette))));
        palette
    }

    /// Builds a [`Palette`] from any `tg-ui` [`Colors`] resolver.
    #[must_use]
    pub fn from_colors(colors: &dyn Colors, is_dark: bool) -> Self {
        let fallback = if is_dark { Self::night() } else { Self::day() };
        let pick = |key: PaletteColor, def: Color32| colors.get_at(key).unwrap_or(def);
        let chat_canvas_bg = if is_dark {
            pick(
                PaletteColor::HISTORY_COMPOSE_AREA_BG,
                fallback.chat_canvas_bg,
            )
        } else {
            // Classic Day chat canvas backdrop when no image wallpaper is active.
            fallback.chat_canvas_bg
        };
        Self {
            window_bg: pick(PaletteColor::WINDOW_BG, fallback.window_bg),
            window_bg_over: pick(PaletteColor::WINDOW_BG_OVER, fallback.window_bg_over),
            window_fg: pick(PaletteColor::WINDOW_FG, fallback.window_fg),
            window_bold_fg: pick(PaletteColor::WINDOW_BOLD_FG, fallback.window_bold_fg),
            window_sub_text_fg: pick(
                PaletteColor::WINDOW_SUB_TEXT_FG,
                fallback.window_sub_text_fg,
            ),
            window_bg_active: pick(PaletteColor::WINDOW_BG_ACTIVE, fallback.window_bg_active),
            window_fg_active: pick(PaletteColor::WINDOW_FG_ACTIVE, fallback.window_fg_active),
            window_active_text_fg: pick(
                PaletteColor::WINDOW_ACTIVE_TEXT_FG,
                fallback.window_active_text_fg,
            ),
            divider: pick(PaletteColor::SHADOW_FG, fallback.divider),
            attention_fg: pick(PaletteColor::ATTENTION_BUTTON_FG, fallback.attention_fg),
            input_bg: pick(PaletteColor::FILTER_INPUT_INACTIVE_BG, fallback.input_bg),
            input_active_bg: pick(
                PaletteColor::FILTER_INPUT_ACTIVE_BG,
                fallback.input_active_bg,
            ),
            input_border_active: pick(
                PaletteColor::FILTER_INPUT_BORDER_FG,
                fallback.input_border_active,
            ),
            dialogs_bg_active: pick(PaletteColor::DIALOGS_BG_ACTIVE, fallback.dialogs_bg_active),
            unread_badge_bg: pick(PaletteColor::DIALOGS_UNREAD_BG, fallback.unread_badge_bg),
            unread_badge_muted_bg: pick(
                PaletteColor::DIALOGS_UNREAD_BG_MUTED,
                fallback.unread_badge_muted_bg,
            ),
            chat_canvas_bg,
            msg_in_bg: pick(PaletteColor::MSG_IN_BG, fallback.msg_in_bg),
            msg_out_bg: pick(PaletteColor::MSG_OUT_BG, fallback.msg_out_bg),
            msg_date_fg: pick(PaletteColor::MSG_IN_DATE_FG, fallback.msg_date_fg),
            sidebar_bg: pick(PaletteColor::SIDE_BAR_BG, fallback.sidebar_bg),
            sidebar_bg_active: pick(PaletteColor::SIDE_BAR_BG_ACTIVE, fallback.sidebar_bg_active),
            sidebar_fg: pick(PaletteColor::SIDE_BAR_TEXT_FG, fallback.sidebar_fg),
            sidebar_fg_active: pick(
                PaletteColor::SIDE_BAR_TEXT_FG_ACTIVE,
                fallback.sidebar_fg_active,
            ),
            layer_backdrop: pick(PaletteColor::LAYER_BG, fallback.layer_backdrop),
            peer_name_fg: std::array::from_fn(|i| {
                colors
                    .get(PEER_NAME_FG_KEYS[i])
                    .unwrap_or(fallback.peer_name_fg[i])
            }),
            peer_name_fg_selected: std::array::from_fn(|i| {
                colors
                    .get(PEER_NAME_FG_SELECTED_KEYS[i])
                    .unwrap_or(fallback.peer_name_fg_selected[i])
            }),
        }
    }

    /// Builds matching `egui::Visuals` for this palette.
    #[must_use]
    pub fn to_egui_visuals(&self, mode: ThemeMode) -> Visuals {
        let mut visuals = match mode {
            ThemeMode::Day => Visuals::light(),
            ThemeMode::Night => Visuals::dark(),
        };
        tg_ui::fonts::apply_tdesktop_text_rendering(&mut visuals);
        visuals.panel_fill = self.window_bg;
        visuals.window_fill = self.window_bg;
        visuals.extreme_bg_color = self.input_bg;
        visuals.faint_bg_color = self.window_bg_over;
        visuals.override_text_color = Some(self.window_fg);
        visuals.selection.bg_fill = self.window_bg_active;
        visuals.selection.stroke.color = self.window_fg_active;
        visuals.widgets.noninteractive.bg_fill = self.window_bg;
        visuals.widgets.noninteractive.bg_stroke.color = self.divider;
        visuals.widgets.inactive.bg_fill = self.input_bg;
        visuals.widgets.inactive.bg_stroke.color = self.divider;
        visuals.widgets.hovered.bg_fill = self.window_bg_over;
        visuals.widgets.hovered.bg_stroke.color = self.input_border_active;
        visuals.widgets.active.bg_fill = self.window_bg_active;
        visuals.widgets.active.bg_stroke.color = self.window_bg_active;
        visuals
    }
}
