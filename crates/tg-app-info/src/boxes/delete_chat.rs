//! "Delete chat" / "Leave group" / "Leave channel" confirmation with the chat's userpic header and
//! the "Delete for everyone", "Stop and block bot" and "Remove from all folders" checkboxes.
//!
//! Port origin: `DeleteChatBox`, `CanDeleteChatForEveryone`, `FiltersWithChat` and
//! `RemoveChatFromFilters` (`Telegram/SourceFiles/boxes/moderate_messages_box.cpp`), opened by
//! `DeleteAndLeaveHandler` (`Telegram/SourceFiles/window/window_peer_menu.cpp`).

use egui::{Align2, FontId, Pos2, Rect, Sense, Ui, Vec2};
use tg_app_core::core_types::dto::engagement::FutureChatCreatorInfo;
use tg_lang::tr;
use tg_ui::fonts::family_semibold;
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{CheckStyle, CheckboxStyle, Colors, FlatLabelStyle};
use tg_ui::tg_style::st;
use tg_ui::userpic::EmptyUserpic;
use tg_ui::widgets::checkbox::Checkbox;
use tg_ui::widgets::labels::FlatLabel;

use crate::info::backend::{InfoBackend, InfoPeerKind, PeerProfile};
use tg_app_core::dialogs::backend::DialogsBackend;
use tg_app_core::history::backend::HistoryBackend;
use tg_app_core::stand_in::types::PeerId;

/// `st::mainMenuUserpic.size` (48 px).
const USERPIC_SIZE: f32 = 48.0;
/// `Ui::AddSkip` (`st::defaultVerticalListSkip`).
const LIST_SKIP: f32 = 6.0;
/// `st::boxTitleFont` (16 px semibold).
const TITLE_FONT_SIZE: f32 = 16.0;
/// `st::defaultBoxCheckbox.textPosition.x`: gap between the userpic and the title
/// (`Ui::IconWithTitle`).
const TITLE_SKIP: f32 = 12.0;
/// `Ui::ForumUserpicRadiusMultiplier()`.
const FORUM_RADIUS_MULTIPLIER: f32 = 0.3;

/// What "Delete" / "Leave" in [`DeleteChatBoxState`] asks the shell to do (`finish` in
/// `DeleteChatBox`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeleteChatChoice {
    /// Chat to delete or leave.
    pub peer_id: PeerId,
    /// "Also delete for {user}" / "Delete for all members" was checked.
    pub revoke: bool,
    /// "Stop and block bot" was checked (`blockedPeers().block`).
    pub block_bot: bool,
    /// "Remove … from all folders" was checked (`RemoveChatFromFilters`).
    pub remove_from_folders: bool,
}

/// Stateful `DeleteChatBox`.
#[derive(Debug, Clone)]
pub struct DeleteChatBoxState {
    /// Chat to delete or leave.
    pub peer_id: PeerId,
    /// Chat title (`peer->name()`).
    pub peer_title: String,
    /// Chat category.
    pub peer_kind: InfoPeerKind,
    /// Empty-userpic color index.
    pub color_index: u8,
    /// Forum groups draw a rounded-square userpic (`peer->userpicShape()`).
    pub is_forum: bool,
    /// The user's first name for "Also delete for {user}" (`UserData::firstName`).
    pub first_name: Option<String>,
    /// `CanDeleteChatForEveryone`: shows the revoke checkbox.
    pub can_revoke: bool,
    /// The revoke checkbox.
    pub revoke: bool,
    /// The "Stop and block bot" checkbox (`Some` for bots only).
    pub block_bot: Option<bool>,
    /// The "Remove … from all folders" checkbox (`Some` when `FiltersWithChat` is not empty,
    /// for bots, groups and channels).
    pub remove_from_folders: Option<bool>,
    /// The next owner when the account owns the chat (`messages.getFutureChatCreatorAfterLeave`);
    /// leaving without "Delete for all" then shows `SelectFutureOwnerbox`.
    pub future_creator: Option<FutureChatCreatorInfo>,
}

impl DeleteChatBoxState {
    /// Builds the box for `peer`. `can_delete_for_everyone` is `CanDeleteChatForEveryone`,
    /// `in_folders` whether `FiltersWithChat` is not empty.
    #[must_use]
    pub fn new(
        peer: &PeerProfile,
        can_delete_for_everyone: bool,
        first_name: Option<String>,
        in_folders: bool,
    ) -> Self {
        let is_bot = peer.kind == InfoPeerKind::Bot;
        let is_user = matches!(
            peer.kind,
            InfoPeerKind::User | InfoPeerKind::Bot | InfoPeerKind::SavedMessages
        );
        Self {
            peer_id: peer.id,
            peer_title: peer.title.clone(),
            peer_kind: peer.kind,
            color_index: peer.color_index,
            is_forum: peer.is_forum,
            first_name,
            can_revoke: can_delete_for_everyone,
            revoke: false,
            block_bot: is_bot.then_some(false),
            remove_from_folders: ((is_bot || !is_user) && in_folders).then_some(false),
            future_creator: None,
        }
    }

    /// Attaches the next owner candidate (`leaveAsCreator`).
    #[must_use]
    pub fn with_future_creator(mut self, creator: Option<FutureChatCreatorInfo>) -> Self {
        self.future_creator = creator;
        self
    }

    fn is_user(&self) -> bool {
        matches!(
            self.peer_kind,
            InfoPeerKind::User | InfoPeerKind::Bot | InfoPeerKind::SavedMessages
        )
    }

    /// Bold title next to the userpic: "Saved Messages", "Delete chat" for users, else the
    /// chat name.
    #[must_use]
    pub fn header_title(&self) -> String {
        match self.peer_kind {
            InfoPeerKind::SavedMessages => tr::lng_saved_messages(),
            InfoPeerKind::User | InfoPeerKind::Bot => tr::lng_profile_delete_conversation(),
            _ => self.peer_title.clone(),
        }
    }

    /// The `st::boxLabel` text under the header.
    #[must_use]
    pub fn prompt_text(&self) -> String {
        match self.peer_kind {
            InfoPeerKind::SavedMessages => tr::lng_sure_delete_saved_messages(),
            InfoPeerKind::User | InfoPeerKind::Bot => tr::lng_sure_delete_history(&self.peer_title),
            InfoPeerKind::Channel => tr::lng_sure_leave_channel(),
            InfoPeerKind::Group | InfoPeerKind::Supergroup => tr::lng_sure_leave_group(),
        }
    }

    /// Label of the revoke checkbox.
    #[must_use]
    pub fn revoke_label(&self) -> String {
        if self.is_user() {
            tr::lng_delete_for_other_check(self.first_name.as_deref().unwrap_or(&self.peer_title))
        } else {
            tr::lng_delete_for_everyone_check()
        }
    }

    /// Label of the folders checkbox (`lng_filters_checkbox_remove_*`).
    #[must_use]
    pub fn folders_label(&self) -> String {
        if self.block_bot.is_some() {
            tr::lng_filters_checkbox_remove_bot()
        } else if self.peer_kind == InfoPeerKind::Channel {
            tr::lng_filters_checkbox_remove_channel()
        } else {
            tr::lng_filters_checkbox_remove_group()
        }
    }

    /// "Delete" for users; "Leave" for chats unless "Delete for all members" is checked.
    #[must_use]
    pub fn confirm_label(&self) -> String {
        if self.is_user() || (self.can_revoke && self.revoke) {
            tr::lng_box_delete()
        } else {
            tr::lng_box_leave()
        }
    }

    /// The checkbox values pressed with "Delete" / "Leave".
    #[must_use]
    pub fn choice(&self) -> DeleteChatChoice {
        DeleteChatChoice {
            peer_id: self.peer_id,
            revoke: self.can_revoke && self.revoke,
            block_bot: self.block_bot.unwrap_or(false),
            remove_from_folders: self.remove_from_folders.unwrap_or(false),
        }
    }

    /// Whether confirming must first show `SelectFutureOwnerbox` (`!revoke && leaveAsCreator`).
    #[must_use]
    pub fn needs_future_owner_box(&self) -> bool {
        !self.choice().revoke && self.future_creator.is_some()
    }

    /// Builds the `tg-ui` [`BoxModal`]: a `GenericBox` without a title, with the attention
    /// "Delete" / "Leave" button and "Cancel".
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let mut modal = BoxModal::new(id, String::new())
            .width(st::box_width.0 as f32 + 28.0)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::attention("delete", self.confirm_label()));
        modal.show_close_button = false;
        modal
    }

    /// Paints the userpic header, the text and the checkboxes. Returns `true` when a checkbox
    /// that changes the button text was toggled (the modal must be rebuilt).
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) -> bool {
        let label_before = self.confirm_label();
        ui.add_space(LIST_SKIP * 2.0);
        self.paint_header(ui, colors);
        ui.add_space(LIST_SKIP * 2.0);

        FlatLabel::plain(&self.prompt_text(), FlatLabelStyle::box_label(colors))
            .id_salt("delete_chat_prompt")
            .show(ui);

        if self.can_revoke {
            ui.add_space(LIST_SKIP * 2.0);
            let label = self.revoke_label();
            Checkbox::new(
                &mut self.revoke,
                &label,
                CheckboxStyle::box_checkbox(colors),
                CheckStyle::default_check(colors),
            )
            .show(ui);
        }
        if let Some(block) = self.block_bot.as_mut() {
            ui.add_space(LIST_SKIP * 2.0);
            Checkbox::new(
                block,
                &tr::lng_profile_block_bot(),
                CheckboxStyle::box_checkbox(colors),
                CheckStyle::default_check(colors),
            )
            .show(ui);
        }
        let folders_label = self.folders_label();
        if let Some(remove) = self.remove_from_folders.as_mut() {
            ui.add_space(LIST_SKIP * 2.0);
            Checkbox::new(
                remove,
                &folders_label,
                CheckboxStyle::box_checkbox(colors),
                CheckStyle::default_check(colors),
            )
            .show(ui);
        }
        ui.add_space(LIST_SKIP);
        self.confirm_label() != label_before
    }

    /// `Ui::IconWithTitle`: the 48 px userpic and the bold title centered beside it.
    fn paint_header(&self, ui: &mut Ui, colors: &dyn Colors) {
        let width = ui.available_width().max(USERPIC_SIZE);
        let (row, resp) = ui.allocate_exact_size(Vec2::new(width, USERPIC_SIZE), Sense::hover());
        let title = self.header_title();
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::accesskit::Role::Label, true, title.clone())
        });
        let painter = ui.painter();
        let pic = Rect::from_min_size(row.min, Vec2::splat(USERPIC_SIZE));
        if self.peer_kind == InfoPeerKind::SavedMessages {
            // `showSavedMessagesOnSelf(true)`.
            EmptyUserpic::paint_saved_messages(painter, pic, colors);
        } else {
            let empty = EmptyUserpic::for_color_index(self.color_index, &self.peer_title, colors);
            if self.is_forum {
                empty.paint_rounded_or_photo(
                    painter,
                    pic,
                    USERPIC_SIZE * FORUM_RADIUS_MULTIPLIER,
                    self.peer_id.0,
                );
            } else {
                empty.paint_circle_or_photo(painter, pic, self.peer_id.0);
            }
        }
        let left = pic.right() + TITLE_SKIP;
        let font = FontId::new(TITLE_FONT_SIZE, family_semibold());
        let galley = painter.layout_no_wrap(title, font, colors.color("boxTitleFg"));
        let max_w = (row.right() - left).max(0.0);
        let clip = Rect::from_min_max(Pos2::new(left, row.top()), row.max);
        painter.with_clip_rect(clip).galley(
            Align2::LEFT_CENTER
                .align_size_within_rect(
                    Vec2::new(galley.size().x.min(max_w), galley.size().y),
                    clip,
                )
                .min,
            galley,
            colors.color("boxTitleFg"),
        );
    }
}

/// `DeleteAndLeaveHandler`: builds the box for `peer` from the backend (`CanDeleteChatForEveryone`,
/// `FiltersWithChat`, and the next owner when the account owns the group or channel).
pub fn delete_chat_box<B>(backend: &B, peer: &PeerProfile) -> DeleteChatBoxState
where
    B: DialogsBackend + HistoryBackend + ?Sized,
{
    let future_creator = if peer.kind.is_group() || peer.kind.is_channel() {
        backend
            .future_chat_creator_after_leave(peer.id)
            .ok()
            .flatten()
    } else {
        None
    };
    DeleteChatBoxState::new(
        peer,
        backend.can_delete_chat_for_everyone(peer.id),
        backend.peer_first_name(peer.id),
        !backend.folders_including_peer(peer.id).is_empty(),
    )
    .with_future_creator(future_creator)
}

/// The steps of `finish` in `DeleteChatBox` that come before `deleteConversation`: block the bot
/// (`blockedPeers().block`) and remove the chat from its folders (`RemoveChatFromFilters`).
pub fn before_delete_conversation<B>(
    backend: &mut B,
    peer_id: PeerId,
    block_bot: bool,
    remove_from_folders: bool,
) where
    B: DialogsBackend + InfoBackend + ?Sized,
{
    if block_bot && !InfoBackend::peer_profile(backend, peer_id).is_some_and(|p| p.is_blocked) {
        InfoBackend::toggle_block_user(backend, peer_id);
    }
    if remove_from_folders {
        backend.remove_peer_from_all_folders(peer_id);
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
    fn user_box_matches_delete_chat_box() {
        let user = DeleteChatBoxState::new(
            &profile(InfoPeerKind::User, "Sample Friend"),
            true,
            Some("Sample".to_owned()),
            true,
        );
        assert_eq!(user.header_title(), tr::lng_profile_delete_conversation());
        assert_eq!(
            user.prompt_text(),
            tr::lng_sure_delete_history("Sample Friend")
        );
        assert_eq!(
            user.revoke_label(),
            tr::lng_delete_for_other_check("Sample")
        );
        assert_eq!(user.block_bot, None);
        assert_eq!(
            user.remove_from_folders, None,
            "users never get the folders row"
        );
        assert_eq!(user.confirm_label(), tr::lng_box_delete());
        let modal = user.to_modal("x");
        assert!(modal.title.is_empty() && !modal.show_close_button);

        let saved = DeleteChatBoxState::new(
            &profile(InfoPeerKind::SavedMessages, "Saved Messages"),
            false,
            None,
            false,
        );
        assert_eq!(saved.header_title(), tr::lng_saved_messages());
        assert_eq!(saved.prompt_text(), tr::lng_sure_delete_saved_messages());
        assert!(!saved.can_revoke);
    }

    #[test]
    fn bot_box_offers_block_and_folders() {
        let mut bot = DeleteChatBoxState::new(
            &profile(InfoPeerKind::Bot, "Sample Bot"),
            true,
            Some("Sample Bot".to_owned()),
            true,
        );
        assert_eq!(bot.block_bot, Some(false));
        assert_eq!(bot.remove_from_folders, Some(false));
        assert_eq!(bot.folders_label(), tr::lng_filters_checkbox_remove_bot());
        bot.block_bot = Some(true);
        bot.remove_from_folders = Some(true);
        assert_eq!(
            bot.choice(),
            DeleteChatChoice {
                peer_id: bot.peer_id,
                revoke: false,
                block_bot: true,
                remove_from_folders: true,
            }
        );
        let not_in_folder =
            DeleteChatBoxState::new(&profile(InfoPeerKind::Bot, "Sample Bot"), true, None, false);
        assert_eq!(not_in_folder.remove_from_folders, None);
    }

    #[test]
    fn group_and_channel_buttons_follow_the_revoke_checkbox() {
        let mut group = DeleteChatBoxState::new(
            &profile(InfoPeerKind::Group, "Sample Group"),
            true,
            None,
            true,
        );
        assert_eq!(group.header_title(), "Sample Group");
        // `DeleteChatBox` says "leave group" for basic groups too.
        assert_eq!(group.prompt_text(), tr::lng_sure_leave_group());
        assert_eq!(group.revoke_label(), tr::lng_delete_for_everyone_check());
        assert_eq!(
            group.folders_label(),
            tr::lng_filters_checkbox_remove_group()
        );
        assert_eq!(group.confirm_label(), tr::lng_box_leave());
        group.revoke = true;
        assert_eq!(group.confirm_label(), tr::lng_box_delete());
        assert!(group.choice().revoke);

        let channel = DeleteChatBoxState::new(
            &profile(InfoPeerKind::Channel, "Sample Channel"),
            false,
            None,
            true,
        );
        assert_eq!(channel.prompt_text(), tr::lng_sure_leave_channel());
        assert_eq!(
            channel.folders_label(),
            tr::lng_filters_checkbox_remove_channel()
        );
        assert_eq!(channel.confirm_label(), tr::lng_box_leave());
    }

    #[test]
    fn owner_leaving_without_revoke_needs_the_future_owner_box() {
        let creator = FutureChatCreatorInfo {
            user_id: PeerId::from_user(42),
            name: "Sample Admin".to_owned(),
            username: None,
        };
        let mut group = DeleteChatBoxState::new(
            &profile(InfoPeerKind::Supergroup, "Sample Group"),
            true,
            None,
            false,
        )
        .with_future_creator(Some(creator));
        assert!(group.needs_future_owner_box());
        group.revoke = true;
        assert!(!group.needs_future_owner_box());
    }
}
