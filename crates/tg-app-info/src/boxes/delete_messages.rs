//! Delete Messages and Delete/Leave Chat confirmation box with "Delete for everyone" option.
//!
//! Port origin: `Telegram/SourceFiles/boxes/delete_messages_box.h`
//! and `Telegram/SourceFiles/boxes/delete_messages_box.cpp`.

use egui::{Align2, CornerRadius, FontId, Pos2, Rect, Stroke, StrokeKind, Ui, Vec2};
use tg_app_core::core_types::dto::engagement::FutureChatCreatorInfo;
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{CheckStyle, CheckboxStyle, Colors, FlatLabelStyle, LinkButtonStyle};
use tg_ui::tg_style::st;
use tg_ui::userpic::EmptyUserpic;
use tg_ui::widgets::buttons::LinkButton;
use tg_ui::widgets::checkbox::Checkbox;
use tg_ui::widgets::labels::FlatLabel;

use crate::boxes::strings;
use crate::info::backend::{InfoBackend, InfoPeerKind, PeerProfile};
use tg_app_core::stand_in::types::PeerId;

/// Mode of [`DeleteMessagesBoxState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteMode {
    /// Delete specific selected message IDs.
    Messages(Vec<i32>),
    /// Clear chat history without leaving.
    ClearHistory,
    /// Delete chat / leave group or channel.
    DeleteAndLeave,
}

/// Stateful Delete Messages / Leave Chat modal (`DeleteMessagesBox`).
#[derive(Debug, Clone)]
pub struct DeleteMessagesBoxState {
    /// Target peer id.
    pub peer_id: PeerId,
    /// Target peer title.
    pub peer_title: String,
    /// Target peer category.
    pub peer_kind: InfoPeerKind,
    /// Delete operation mode.
    pub mode: DeleteMode,
    /// Whether "Delete for everyone / Also delete for <peer>" is allowed.
    pub can_revoke_for_everyone: bool,
    /// Default value for `revoke_for_everyone` (used to decide when to show "Remember my choice").
    pub default_revoke: bool,
    /// Current state of the "Delete for everyone" checkbox.
    pub revoke_for_everyone: bool,
    /// Optional "Remember my choice" checkbox revealed when user diverges from default.
    pub remember_choice: bool,
    /// Optional future chat/channel creator who will inherit ownership if the current owner leaves
    /// (`messages.getFutureChatCreatorAfterLeave`, `window_peer_menu.cpp:4367-4389`).
    pub future_creator: Option<FutureChatCreatorInfo>,
    /// The user's first name for "Also delete for {user}" (`UserData::firstName`).
    pub first_name: Option<String>,
    /// "Clear history" shows the "Enable Auto-Delete" / "Edit Auto-Delete Settings" link when
    /// the account may set the chat's timer (`TTLMenu::TTLValidator::can`); holds the current
    /// timer in seconds (`0` = off).
    pub auto_delete_ttl: Option<u32>,
    /// Carried from `DeleteChatBox` when this box is the next-owner step of leaving a chat.
    pub remove_from_folders: bool,
}

/// Bare ids of the service notifications user (`PeerData::isNotificationsUser`) and of
/// "Verification Codes" (`UserData::isVerifyCodes`).
const SERVICE_NOTIFICATIONS_USER: u64 = 777_000;
const VERIFY_CODES_USER: u64 = 489_000;

/// `TTLMenu::TTLValidator::can`: another (non-service) user, or a group / channel whose info
/// the account may edit.
#[must_use]
pub fn can_set_auto_delete(peer: &PeerProfile) -> bool {
    match peer.kind {
        InfoPeerKind::User | InfoPeerKind::Bot => !matches!(
            peer.id.bare(),
            SERVICE_NOTIFICATIONS_USER | VERIFY_CODES_USER
        ),
        InfoPeerKind::SavedMessages => false,
        InfoPeerKind::Group | InfoPeerKind::Supergroup | InfoPeerKind::Channel => peer.can_edit,
    }
}

impl DeleteMessagesBoxState {
    /// Creates a box for deleting specific messages in `peer`.
    #[must_use]
    pub fn for_messages(peer: &PeerProfile, message_ids: Vec<i32>) -> Self {
        let can_revoke = peer.kind != InfoPeerKind::SavedMessages && peer.kind != InfoPeerKind::Bot;
        Self {
            peer_id: peer.id,
            peer_title: peer.title.clone(),
            peer_kind: peer.kind,
            mode: DeleteMode::Messages(message_ids),
            can_revoke_for_everyone: can_revoke,
            default_revoke: can_revoke,
            revoke_for_everyone: can_revoke,
            remember_choice: false,
            future_creator: None,
            first_name: None,
            auto_delete_ttl: None,
            remove_from_folders: false,
        }
    }

    /// Creates a box for deleting chat history or leaving a group/channel.
    #[must_use]
    pub fn for_delete_or_leave(peer: &PeerProfile, just_clear: bool) -> Self {
        let can_revoke = peer.kind == InfoPeerKind::User || (peer.can_edit && peer.kind.is_group());
        Self {
            peer_id: peer.id,
            peer_title: peer.title.clone(),
            peer_kind: peer.kind,
            mode: if just_clear {
                DeleteMode::ClearHistory
            } else {
                DeleteMode::DeleteAndLeave
            },
            auto_delete_ttl: (just_clear && can_set_auto_delete(peer))
                .then_some(peer.auto_delete_ttl_seconds),
            remove_from_folders: false,
            can_revoke_for_everyone: can_revoke,
            default_revoke: false,
            revoke_for_everyone: false,
            remember_choice: false,
            future_creator: None,
            first_name: None,
        }
    }

    /// Sets the "Also delete for {user}" / "Delete for all members" checkbox from
    /// `PeerData::canRevokeFullHistory` (`DeleteMessagesBox::revokeText`). A broadcast channel's
    /// "Clear history" never has it (`_revokeJustClearForChannel`).
    #[must_use]
    pub fn with_revoke_full_history(
        mut self,
        can_revoke: bool,
        first_name: Option<String>,
    ) -> Self {
        let channel_clear =
            self.mode == DeleteMode::ClearHistory && self.peer_kind == InfoPeerKind::Channel;
        self.can_revoke_for_everyone = can_revoke && !channel_clear;
        self.revoke_for_everyone = false;
        self.default_revoke = false;
        self.first_name = first_name;
        self
    }

    /// Whether this box wipes a whole chat ("Clear history" / "Delete chat" / "Leave").
    #[must_use]
    pub fn wipes_history(&self) -> bool {
        matches!(
            self.mode,
            DeleteMode::ClearHistory | DeleteMode::DeleteAndLeave
        )
    }

    /// Attaches the future owner candidate returned by `messages.getFutureChatCreatorAfterLeave`.
    #[must_use]
    pub fn with_future_creator(mut self, creator: Option<FutureChatCreatorInfo>) -> Self {
        self.future_creator = creator;
        self
    }

    /// Computes the primary action button label (`"Delete"` vs `"Leave"`).
    #[must_use]
    pub fn confirm_label(&self) -> String {
        match self.mode {
            DeleteMode::DeleteAndLeave
                if (self.peer_kind.is_group() || self.peer_kind.is_channel())
                    && !self.revoke_for_everyone =>
            {
                tr::lng_box_leave()
            }
            _ => tr::lng_box_delete(),
        }
    }

    /// Computes the body prompt text matching `DeleteMessagesBox::prepare`.
    #[must_use]
    pub fn prompt_text(&self) -> String {
        match &self.mode {
            DeleteMode::Messages(ids) => {
                if ids.len() <= 1 {
                    tr::lng_selected_delete_sure_this()
                } else {
                    format!(
                        "{} {} {}",
                        strings::DELETE_SELECTED_PREFIX,
                        ids.len(),
                        strings::DELETE_SELECTED_SUFFIX
                    )
                }
            }
            // `DeleteMessagesBox::prepare`, `_wipeHistoryJustClear`.
            DeleteMode::ClearHistory => {
                let text = match self.peer_kind {
                    InfoPeerKind::Channel => {
                        tr::lng_sure_delete_channel_history(&self.peer_title).replace("**", "")
                    }
                    InfoPeerKind::SavedMessages => tr::lng_sure_delete_saved_messages(),
                    InfoPeerKind::User | InfoPeerKind::Bot => {
                        tr::lng_sure_delete_history(&self.peer_title)
                    }
                    InfoPeerKind::Group | InfoPeerKind::Supergroup => {
                        tr::lng_sure_delete_group_history(&self.peer_title)
                    }
                };
                // Groups without the revoke checkbox explain that only your copy is cleared.
                if !self.can_revoke_for_everyone && self.peer_kind.is_group() {
                    format!("{text}\n\n{}", tr::lng_delete_clear_for_me())
                } else {
                    text
                }
            }
            DeleteMode::DeleteAndLeave => match self.peer_kind {
                InfoPeerKind::SavedMessages => tr::lng_sure_delete_saved_messages(),
                InfoPeerKind::User | InfoPeerKind::Bot => {
                    tr::lng_sure_delete_history(&self.peer_title)
                }
                InfoPeerKind::Group => tr::lng_sure_delete_and_exit(&self.peer_title),
                InfoPeerKind::Supergroup => tr::lng_sure_leave_group(),
                InfoPeerKind::Channel => tr::lng_sure_leave_channel(),
            },
        }
    }

    /// Computes the checkbox label for `revoke_for_everyone`.
    #[must_use]
    pub fn revoke_checkbox_label(&self) -> String {
        match self.peer_kind {
            InfoPeerKind::User => tr::lng_delete_for_other_check(
                self.first_name.as_deref().unwrap_or(&self.peer_title),
            ),
            _ => tr::lng_delete_for_everyone_check(),
        }
    }

    /// Executes the deletion against `backend`.
    pub fn submit(&self, backend: &mut dyn InfoBackend) -> Result<usize, String> {
        if self.peer_kind == InfoPeerKind::Group
            && matches!(self.mode, DeleteMode::DeleteAndLeave)
            && self.revoke_for_everyone
            && backend.delete_basic_chat(self.peer_id).is_ok()
        {
            return Ok(0);
        }
        let ids = match &self.mode {
            DeleteMode::Messages(ids) => ids.clone(),
            DeleteMode::ClearHistory | DeleteMode::DeleteAndLeave => Vec::new(),
        };
        backend.delete_messages(self.peer_id, &ids, self.revoke_for_everyone)
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let title = match self.mode {
            DeleteMode::Messages(ref ids) if ids.len() > 1 => {
                strings::DELETE_MESSAGES_TITLE.to_owned()
            }
            DeleteMode::Messages(_) => strings::DELETE_MESSAGE_TITLE.to_owned(),
            DeleteMode::ClearHistory => tr::lng_profile_clear_history(),
            DeleteMode::DeleteAndLeave => {
                if self.future_creator.is_some() {
                    if self.peer_kind.is_group() {
                        tr::lng_leave_next_owner_box_title_group()
                    } else if self.peer_kind.is_channel() {
                        tr::lng_leave_next_owner_box_title()
                    } else {
                        tr::lng_profile_delete_conversation()
                    }
                } else if self.peer_kind.is_group() {
                    tr::lng_profile_leave_group()
                } else if self.peer_kind.is_channel() {
                    tr::lng_profile_leave_channel()
                } else {
                    tr::lng_profile_delete_conversation()
                }
            }
        };
        // `DeleteMessagesBox` has no title and no close button when it wipes a whole chat;
        // the future-owner variant is `SelectFutureOwnerbox`, which has a title.
        let untitled = self.wipes_history() && self.future_creator.is_none();
        let mut modal = BoxModal::new(id, if untitled { String::new() } else { title })
            .width(st::box_width.0 as f32 + 28.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::attention("delete", self.confirm_label()));
        if untitled {
            modal.show_close_button = false;
        }
        modal
    }

    /// Text of the auto-delete link: "Edit Auto-Delete Settings" when a timer is set, else
    /// "Enable Auto-Delete" (`None` when the box has no link).
    #[must_use]
    pub fn auto_delete_link_text(&self) -> Option<String> {
        self.auto_delete_ttl.map(|ttl| {
            if ttl > 0 {
                tr::lng_edit_auto_delete_settings()
            } else {
                tr::lng_enable_auto_delete()
            }
        })
    }

    /// Renders the confirmation copy, optional future-owner transfer callout, delete-for-everyone
    /// checkboxes and the auto-delete link. Returns `true` when the auto-delete link was clicked
    /// (`validator.showBox()`: the caller opens the Auto-Delete box on top).
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) -> bool {
        let prompt = self.prompt_text();
        FlatLabel::plain(&prompt, FlatLabelStyle::box_label(colors))
            .id_salt("delete_prompt")
            .show(ui);

        // `messages.getFutureChatCreatorAfterLeave` ownership transfer callout
        // (`select_future_owner_box.cpp:278-314`).
        if matches!(self.mode, DeleteMode::DeleteAndLeave)
            && !self.revoke_for_everyone
            && let Some(ref creator) = self.future_creator
        {
            ui.add_space(12.0);
            let avail_w = ui.available_width().max(180.0);
            let notice = if self.peer_kind == InfoPeerKind::Group {
                tr::lng_leave_next_owner_box_about_legacy(&creator.name, &self.peer_title)
            } else {
                tr::lng_leave_next_owner_box_about(&creator.name, &self.peer_title)
            }
            .replace("**", "");
            let notice_galley = ui.painter().layout(
                notice.clone(),
                FontId::new(12.5, family_regular()),
                colors.color("windowFg"),
                (avail_w - 64.0).max(110.0),
            );
            let card_h = (notice_galley.size().y + 38.0).max(66.0);
            let (card_rect, card_resp) =
                ui.allocate_exact_size(Vec2::new(avail_w, card_h), egui::Sense::hover());
            let a11y_label = format!("{}: {notice}", creator.name);
            card_resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::accesskit::Role::Label, true, a11y_label.clone())
            });
            ui.painter().rect_filled(
                card_rect,
                CornerRadius::same(10),
                colors.color("windowBgOver"),
            );
            ui.painter().rect_stroke(
                card_rect,
                CornerRadius::same(10),
                Stroke::new(1.0, colors.color("shadowFg")),
                StrokeKind::Inside,
            );

            let av_rect = Rect::from_center_size(
                Pos2::new(card_rect.left() + 28.0, card_rect.center().y),
                Vec2::splat(36.0),
            );
            EmptyUserpic::for_peer(creator.user_id.bare(), &creator.name, colors)
                .paint_circle_or_photo(ui.painter(), av_rect, creator.user_id.0);

            let text_left = av_rect.right() + 12.0;
            let header_text = match creator.username.as_deref() {
                Some(u) if !u.is_empty() => format!("{} (@{u})", creator.name),
                _ => creator.name.clone(),
            };
            ui.painter().text(
                Pos2::new(text_left, card_rect.top() + 10.0),
                Align2::LEFT_TOP,
                header_text,
                FontId::new(13.0, family_semibold()),
                colors.color("windowBoldFg"),
            );
            ui.painter().galley(
                Pos2::new(text_left, card_rect.top() + 28.0),
                notice_galley,
                colors.color("windowFg"),
            );
        }

        if self.can_revoke_for_everyone {
            ui.add_space(12.0);
            let revoke_label = self.revoke_checkbox_label();
            Checkbox::new(
                &mut self.revoke_for_everyone,
                &revoke_label,
                CheckboxStyle::box_checkbox(colors),
                CheckStyle::default_check(colors),
            )
            .show(ui);

            // Reveal "Remember my choice" when toggled away from default (`delete_messages_box.cpp:208-213`).
            if matches!(self.mode, DeleteMode::Messages(_))
                && self.revoke_for_everyone != self.default_revoke
            {
                Checkbox::new(
                    &mut self.remember_choice,
                    strings::REMEMBER_MY_CHOICE,
                    CheckboxStyle::box_checkbox(colors),
                    CheckStyle::default_check(colors),
                )
                .show(ui);
            }
        }

        // `_autoDeleteSettings` (`delete_messages_box.cpp:246-262`), placed `boxMediumSkip`
        // below the revoke checkbox, or `2 * boxMediumSkip - boxLittleSkip` below the text.
        let Some(link) = self.auto_delete_link_text() else {
            return false;
        };
        let medium = st::box_medium_skip.0 as f32;
        let little = st::box_little_skip.0 as f32;
        ui.add_space(if self.can_revoke_for_everyone {
            medium
        } else {
            2.0 * medium - little
        });
        LinkButton::new(&link, LinkButtonStyle::box_link(colors))
            .show(ui)
            .clicked()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(kind: InfoPeerKind, title: &str) -> PeerProfile {
        let mut p = crate::info::mock::MockBackend::new()
            .peer_profile(crate::info::mock::mock_peers::USER_MIRA)
            .expect("mock profile");
        p.kind = kind;
        p.title = title.to_owned();
        p
    }

    #[test]
    fn clear_history_texts_follow_delete_messages_box_prepare() {
        let user = DeleteMessagesBoxState::for_delete_or_leave(
            &profile(InfoPeerKind::User, "Sample Friend"),
            true,
        )
        .with_revoke_full_history(true, Some("Sample".to_owned()));
        assert_eq!(
            user.prompt_text(),
            tr::lng_sure_delete_history("Sample Friend")
        );
        assert_eq!(
            user.revoke_checkbox_label(),
            tr::lng_delete_for_other_check("Sample")
        );
        assert!(user.can_revoke_for_everyone && !user.revoke_for_everyone);
        assert_eq!(user.confirm_label(), tr::lng_box_delete());
        let modal = user.to_modal("x");
        assert!(modal.title.is_empty() && !modal.show_close_button);

        let channel = DeleteMessagesBoxState::for_delete_or_leave(
            &profile(InfoPeerKind::Channel, "Sample Channel"),
            true,
        )
        .with_revoke_full_history(true, None);
        assert!(
            !channel.can_revoke_for_everyone,
            "_revokeJustClearForChannel"
        );
        assert!(
            channel
                .prompt_text()
                .starts_with("Are you sure you want to delete all messages in")
        );
        assert!(!channel.prompt_text().contains("**"));

        let group = DeleteMessagesBoxState::for_delete_or_leave(
            &profile(InfoPeerKind::Supergroup, "Sample Group"),
            true,
        )
        .with_revoke_full_history(false, None);
        assert!(
            group
                .prompt_text()
                .ends_with(&tr::lng_delete_clear_for_me())
        );

        let saved = DeleteMessagesBoxState::for_delete_or_leave(
            &profile(InfoPeerKind::SavedMessages, "Saved Messages"),
            true,
        );
        assert_eq!(saved.prompt_text(), tr::lng_sure_delete_saved_messages());
    }

    #[test]
    fn delete_and_leave_texts_follow_delete_messages_box_prepare() {
        let basic = DeleteMessagesBoxState::for_delete_or_leave(
            &profile(InfoPeerKind::Group, "Sample Group"),
            false,
        )
        .with_revoke_full_history(true, None);
        assert_eq!(
            basic.prompt_text(),
            tr::lng_sure_delete_and_exit("Sample Group")
        );
        assert_eq!(
            basic.revoke_checkbox_label(),
            tr::lng_delete_for_everyone_check()
        );
        assert_eq!(basic.confirm_label(), tr::lng_box_leave());

        let mut mega = DeleteMessagesBoxState::for_delete_or_leave(
            &profile(InfoPeerKind::Supergroup, "Sample Group"),
            false,
        )
        .with_revoke_full_history(true, None);
        assert_eq!(mega.prompt_text(), tr::lng_sure_leave_group());
        mega.revoke_for_everyone = true;
        assert_eq!(mega.confirm_label(), tr::lng_box_delete());

        let channel = DeleteMessagesBoxState::for_delete_or_leave(
            &profile(InfoPeerKind::Channel, "Sample Channel"),
            false,
        )
        .with_revoke_full_history(false, None);
        assert_eq!(channel.prompt_text(), tr::lng_sure_leave_channel());
        assert!(!channel.can_revoke_for_everyone);
    }
}
