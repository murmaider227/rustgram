//! Block confirmation box.
//!
//! Port origin: `Window::PeerMenuBlockUserBox` in
//! `Telegram/SourceFiles/window/window_peer_menu.cpp`.

use egui::Ui;
use tg_lang::tr;
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{CheckStyle, CheckboxStyle, Colors, FlatLabelStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::checkbox::Checkbox;
use tg_ui::widgets::labels::FlatLabel;

use tg_app_core::stand_in::types::PeerId;

/// State of the "Block {name}" box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockUserBoxState {
    /// The user to block.
    pub peer_id: PeerId,
    /// `user->shortName()`, used in the title and the text.
    pub name: String,
    /// "Report Spam" checkbox state; `None` when the box does not offer it
    /// (`suggestReport` / `PeerBarSetting::ReportSpam`).
    pub report_spam: Option<bool>,
    /// "Delete this chat" checkbox state; `None` unless the box was opened with `ClearChat`.
    pub delete_chat: Option<bool>,
}

impl BlockUserBoxState {
    /// The box as the profile and the chat menu open it (`suggestReport` and `suggestClear`
    /// null): no "Delete this chat", "Report Spam" only when the peer's bar suggests it.
    #[must_use]
    pub fn new(peer_id: PeerId, name: impl Into<String>, suggest_report: bool) -> Self {
        Self {
            peer_id,
            name: name.into(),
            report_spam: suggest_report.then_some(true),
            delete_chat: None,
        }
    }

    /// Adds the checked "Delete this chat" option (`ClearChat`, the contact status bar's Block).
    #[must_use]
    pub const fn with_delete_chat(mut self) -> Self {
        self.delete_chat = Some(true);
        self
    }

    /// Builds the modal: `lng_blocked_list_confirm_title`, Cancel and the red "Block".
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let title = tr::lng_blocked_list_confirm_title(&self.name);
        BoxModal::new(id, &title)
            .width(st::box_width.0 as f32 + 20.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::attention(
                "block",
                tr::lng_blocked_list_confirm_ok(),
            ))
    }

    /// Renders the text and the optional checkboxes.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        let text = tr::lng_blocked_list_confirm_text(&self.name);
        FlatLabel::plain(&text, FlatLabelStyle::box_label(colors))
            .id_salt("block_user_text")
            .show(ui);
        let skip = st::box_medium_skip.0 as f32;
        if let Some(checked) = self.report_spam.as_mut() {
            ui.add_space(skip);
            Checkbox::new(
                checked,
                &tr::lng_report_spam(),
                CheckboxStyle::box_checkbox(colors),
                CheckStyle::default_check(colors),
            )
            .show(ui);
        }
        if let Some(checked) = self.delete_chat.as_mut() {
            ui.add_space(skip);
            Checkbox::new(
                checked,
                &tr::lng_blocked_list_confirm_clear(),
                CheckboxStyle::box_checkbox(colors),
                CheckStyle::default_check(colors),
            )
            .show(ui);
        }
    }

    /// The outcome of pressing "Block".
    #[must_use]
    pub fn confirmed(&self) -> BlockUserChoice {
        BlockUserChoice {
            peer_id: self.peer_id,
            name: self.name.clone(),
            report_spam: self.report_spam.unwrap_or(false),
            delete_chat: self.delete_chat.unwrap_or(false),
        }
    }
}

/// What the user chose in the block box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockUserChoice {
    /// The user to block.
    pub peer_id: PeerId,
    /// Short name for the "{user} is now blocked" toast.
    pub name: String,
    /// Also `messages.reportSpam`.
    pub report_spam: bool,
    /// Also delete the conversation (`deleteConversation`) and go back.
    pub delete_chat: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_block_box_has_no_delete_chat_option() {
        let state = BlockUserBoxState::new(PeerId::from_user(7), "Riley", false);
        assert_eq!(state.report_spam, None);
        assert_eq!(state.delete_chat, None);
        let choice = state.confirmed();
        assert!(!choice.report_spam && !choice.delete_chat);
    }

    #[test]
    fn contact_bar_block_box_checks_report_and_delete_by_default() {
        let state = BlockUserBoxState::new(PeerId::from_user(7), "Riley", true).with_delete_chat();
        let choice = state.confirmed();
        assert!(choice.report_spam && choice.delete_chat);
        assert_eq!(choice.peer_id, PeerId::from_user(7));
    }
}
