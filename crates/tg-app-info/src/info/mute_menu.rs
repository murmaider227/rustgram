//! The profile Mute button's right-click menu.
//!
//! Port origin: `Telegram/SourceFiles/info/profile/info_profile_top_bar.cpp`
//! (`MuteMenu::SetupMuteMenu` on the notifications `TopBarActionButton`) and
//! `Telegram/SourceFiles/menu/menu_mute.cpp` (`FillMuteMenu`, `MuteItem`).

use egui::{Id, Pos2};
use tg_lang::tr;
use tg_ui::stand_in::{Colors, PopupMenuStyle, ToggleStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::menu::{MenuItem, PopupMenuState};

/// Menu id of "Select tone" (opens `RingtonesBox`).
pub const SELECT_TONE: &str = "profile_mute_select_tone";
/// Menu id of "Disable sound" / "Enable sound".
pub const SOUND_TOGGLE: &str = "profile_mute_sound_toggle";
/// Menu id of "Mute for..." (opens `PickMuteBox`).
pub const MUTE_FOR: &str = "profile_mute_for";
/// Menu id of "Mute forever" / "Unmute" (`MuteItem`).
pub const MUTE_FOREVER_OR_UNMUTE: &str = "profile_mute_forever";

/// A row picked in the profile mute menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuteMenuCommand {
    /// "Select tone".
    SelectTone,
    /// "Disable sound" (`enable == false`) or "Enable sound" (`enable == true`).
    SetSoundEnabled {
        /// Whether the chat's notification sound is switched on.
        enable: bool,
    },
    /// "Mute for...".
    MuteFor,
    /// "Mute forever" on an unmuted chat, "Unmute" on a muted one.
    ToggleMuteForever,
}

/// `FillMuteMenu` rows for a chat: "Select tone", "Disable sound" ("Enable sound" when the chat
/// is silent), "Mute for...", then `MuteItem`: red "Mute forever", or "Unmute" when muted.
/// The recently used mute periods (`session->settings().mutePeriods()`) are not stored by
/// Rustgram, so their rows never appear.
#[must_use]
pub fn mute_menu_items(muted: bool, silent: bool) -> Vec<MenuItem> {
    let sound = if silent {
        MenuItem::action(SOUND_TOGGLE, tr::lng_mute_menu_sound_on())
            .with_style_icon(st::menu_icon_sound_on)
    } else {
        MenuItem::action(SOUND_TOGGLE, tr::lng_mute_menu_sound_off())
            .with_style_icon(st::menu_icon_sound_off)
    };
    let last = if muted {
        MenuItem::action(MUTE_FOREVER_OR_UNMUTE, tr::lng_mute_menu_duration_unmute())
            .with_style_icon(st::menu_icon_unmute)
    } else {
        MenuItem::action(MUTE_FOREVER_OR_UNMUTE, tr::lng_mute_menu_duration_forever())
            .with_style_icon(st::menu_icon_mute)
            .attention()
    };
    vec![
        MenuItem::action(SELECT_TONE, tr::lng_mute_menu_sound_select())
            .with_style_icon(st::menu_icon_sound_select),
        sound,
        MenuItem::action(MUTE_FOR, tr::lng_mute_menu_duration())
            .with_style_icon(st::menu_icon_mute_for),
        last,
    ]
}

/// Open state of the profile mute menu.
#[derive(Debug, Default)]
pub struct ProfileMuteMenu {
    state: PopupMenuState,
}

impl ProfileMuteMenu {
    /// Opens the menu with its top-left corner at `anchor`.
    pub fn open_at(&mut self, ctx: &egui::Context, anchor: Pos2, colors: &dyn Colors) {
        self.state
            .open_at(ctx, anchor, &PopupMenuStyle::default_popup(colors));
    }

    /// Whether the menu is visible.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.state.is_open()
    }

    /// Closes the menu.
    pub fn close(&mut self) {
        self.state.close();
    }

    /// Draws the open menu for a chat in the given `muted` / `silent` state and returns the
    /// picked row.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        id: Id,
        muted: bool,
        silent: bool,
        colors: &dyn Colors,
    ) -> Option<MuteMenuCommand> {
        if !self.state.is_open() {
            return None;
        }
        let items = mute_menu_items(muted, silent);
        let picked = self.state.show(
            ctx,
            id,
            &items,
            &PopupMenuStyle::default_popup(colors),
            &ToggleStyle::default_toggle(colors),
        )?;
        match picked.as_str() {
            SELECT_TONE => Some(MuteMenuCommand::SelectTone),
            SOUND_TOGGLE => Some(MuteMenuCommand::SetSoundEnabled { enable: silent }),
            MUTE_FOR => Some(MuteMenuCommand::MuteFor),
            MUTE_FOREVER_OR_UNMUTE => Some(MuteMenuCommand::ToggleMuteForever),
            _ => None,
        }
    }
}
