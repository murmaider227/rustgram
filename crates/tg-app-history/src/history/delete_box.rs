//! Delete Messages confirmation box (`boxes/delete_messages_box.cpp`, `DeleteMessagesBox` for
//! selected messages, TGB-3052).
//!
//! The box has no title: the question (`lng_selected_delete_sure_this` / `lng_selected_delete_sure`),
//! an optional detail paragraph, the "delete for everyone" checkbox when every message allows it
//! (`revokeText`), then the "Delete" and "Cancel" buttons. Which checkbox and hint appear depends on
//! the chat and on `HistoryItem::canDeleteForEveryone` for each message.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2,
};
use tg_app_core::history::backend::HistoryMessage;
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind};

use crate::history::theme::HistoryColors;

/// `st::boxWidth`.
const BOX_WIDTH: f32 = 320.0;
/// `st::boxPadding` (left, top, right, bottom).
const BOX_PADDING: (f32, f32, f32, f32) = (24.0, 14.0, 24.0, 8.0);
/// `st::boxMediumSkip`: between the text and the checkbox.
const BOX_MEDIUM_SKIP: f32 = 20.0;
/// `st::boxLabel` font size (`boxTextFont`, 14px) and line height (`boxLabelStyle`, 22px).
const BOX_LABEL_FONT: f32 = 14.0;
/// `st::defaultBoxButton` height and `st::boxButtonPadding` (top, right, bottom, between).
const BOX_BUTTON_HEIGHT: f32 = 36.0;
const BOX_BUTTON_PADDING: (f32, f32, f32, f32) = (8.0, 8.0, 8.0, 8.0);
/// `st::defaultBoxButton` text padding on each side.
const BOX_BUTTON_TEXT_PAD: f32 = 16.0;
/// `st::defaultBoxCheckbox`: 22px check box, 8px gap before the text.
const CHECK_SIZE: f32 = 22.0;
const CHECK_TEXT_GAP: f32 = 8.0;

/// The chat the selected messages belong to (`DeleteMessagesBox::checkFromSinglePeer`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteChat {
    /// Saved Messages (`peer->isSelf()`): no checkbox and no hint.
    SavedMessages,
    /// A private chat.
    User {
        /// The other user's first name, for `lng_delete_for_other_check`.
        first_name: String,
        /// Bots receive every message, so nothing is revoked from them.
        is_bot: bool,
    },
    /// A legacy group (`peer->isChat()`).
    BasicGroup {
        /// Whether we may delete other members' messages (`ChatData::canDeleteMessages`).
        can_delete_messages: bool,
    },
    /// A supergroup (`peer->isMegagroup()`): deleting is always for everyone.
    Supergroup,
    /// A broadcast channel: deleting is always for everyone.
    Channel,
}

impl DeleteChat {
    /// Classifies the open chat. `title` stands in for the user's first name: the history peer
    /// carries only the display name, so its first word is used.
    #[must_use]
    pub fn from_peer(
        peer_id: PeerId,
        kind: PeerKind,
        title: &str,
        is_saved_messages: bool,
        is_bot: bool,
        can_delete_messages: bool,
    ) -> Self {
        if is_saved_messages {
            return Self::SavedMessages;
        }
        match kind {
            PeerKind::User => Self::User {
                first_name: title.split_whitespace().next().unwrap_or(title).to_owned(),
                is_bot,
            },
            PeerKind::Channel => Self::Channel,
            PeerKind::Group if peer_id.kind() == PeerKind::Group => Self::BasicGroup {
                can_delete_messages,
            },
            PeerKind::Group => Self::Supergroup,
        }
    }
}

/// What `canDeleteForEveryone` needs to know about one message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeleteItem {
    /// Sent by us (`HistoryItem::out`).
    pub outgoing: bool,
    /// Unix send time.
    pub date_unix: i32,
    /// A channel post (`HistoryItem::isPost`).
    pub is_post: bool,
}

impl From<&HistoryMessage> for DeleteItem {
    fn from(msg: &HistoryMessage) -> Self {
        Self {
            outgoing: msg.outgoing,
            date_unix: msg.date_unix,
            is_post: msg.is_post,
        }
    }
}

/// Server revoke limits (`help.getConfig`: `revoke_time_limit`, `revoke_pm_time_limit`,
/// `revoke_pm_inbox`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevokeLimits {
    /// Seconds a group message can still be deleted for everyone.
    pub time_limit: i32,
    /// Seconds a private-chat message can still be deleted for everyone.
    pub private_time_limit: i32,
    /// Whether incoming private messages can be deleted for both sides.
    pub private_inbox: bool,
}

impl Default for RevokeLimits {
    /// The values Telegram's servers send today (no time limit, inbox revoke allowed).
    fn default() -> Self {
        Self {
            time_limit: i32::MAX,
            private_time_limit: i32::MAX,
            private_inbox: true,
        }
    }
}

/// `HistoryItem::canDeleteForEveryone` (`history/history_item.cpp`).
#[must_use]
pub fn can_delete_for_everyone(
    chat: &DeleteChat,
    item: DeleteItem,
    now_unix: i64,
    limits: RevokeLimits,
) -> bool {
    let age = now_unix - i64::from(item.date_unix);
    let too_old = match chat {
        DeleteChat::SavedMessages => return false,
        DeleteChat::User { .. } => age >= i64::from(limits.private_time_limit),
        _ => age >= i64::from(limits.time_limit),
    };
    if too_old || item.is_post {
        return false;
    }
    match chat {
        DeleteChat::SavedMessages | DeleteChat::Supergroup | DeleteChat::Channel => false,
        DeleteChat::User { is_bot: true, .. } => false,
        DeleteChat::User { .. } => item.outgoing || limits.private_inbox,
        DeleteChat::BasicGroup {
            can_delete_messages,
        } => item.outgoing || *can_delete_messages,
    }
}

/// The revoke checkbox (`DeleteMessagesBox::revokeText`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevokeCheckbox {
    /// Every message can be revoked: "Also delete for {user}" or "Delete for everyone".
    All,
    /// Only some of our own messages can be revoked: "Unsend my messages" plus a description.
    OutgoingOnly {
        /// How many of our messages can be unsent.
        count: usize,
    },
}

/// `DeleteMessagesBox::revokeText`: `items` is `None` when some selected message is not loaded.
#[must_use]
pub fn revoke_checkbox(
    chat: &DeleteChat,
    items: Option<&[DeleteItem]>,
    now_unix: i64,
    limits: RevokeLimits,
) -> Option<RevokeCheckbox> {
    let items = items?;
    if items.is_empty() {
        return None;
    }
    let can = |item: &&DeleteItem| can_delete_for_everyone(chat, **item, now_unix, limits);
    if items.iter().all(|item| can(&item)) {
        return Some(RevokeCheckbox::All);
    }
    let count = items
        .iter()
        .filter(|item| item.outgoing)
        .filter(can)
        .count();
    (count > 0).then_some(RevokeCheckbox::OutgoingOnly { count })
}

/// The texts of the box, computed once when it opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteBoxTexts {
    /// The question plus the detail paragraph (`details`).
    pub text: String,
    /// The checkbox label, when there is a checkbox.
    pub checkbox: Option<String>,
}

/// `DeleteMessagesBox::prepare` for selected messages.
#[must_use]
pub fn delete_box_texts(
    chat: &DeleteChat,
    count: usize,
    revoke: Option<&RevokeCheckbox>,
) -> DeleteBoxTexts {
    let mut text = if count == 1 {
        tg_lang::tr::lng_selected_delete_sure_this()
    } else {
        tg_lang::tr::lng_selected_delete_sure(count)
    };
    let mut append = |details: String| {
        text.push_str("\n\n");
        text.push_str(&details);
    };
    let checkbox = match (revoke, chat) {
        (Some(RevokeCheckbox::All), DeleteChat::User { first_name, .. }) => {
            Some(tg_lang::tr::lng_delete_for_other_check(first_name))
        }
        (Some(RevokeCheckbox::All), _) => Some(tg_lang::tr::lng_delete_for_everyone_check()),
        (Some(RevokeCheckbox::OutgoingOnly { count }), chat) => {
            append(match (chat, *count) {
                (DeleteChat::User { first_name, .. }, 1) => {
                    tg_lang::tr::lng_selected_unsend_about_user_one(first_name)
                }
                (DeleteChat::User { first_name, .. }, n) => {
                    tg_lang::tr::lng_selected_unsend_about_user(n, first_name)
                }
                (_, 1) => tg_lang::tr::lng_selected_unsend_about_group_one(),
                (_, n) => tg_lang::tr::lng_selected_unsend_about_group(n),
            });
            Some(tg_lang::tr::lng_delete_for_other_my())
        }
        (None, DeleteChat::Supergroup) => {
            append(tg_lang::tr::lng_delete_for_everyone_hint(count));
            None
        }
        (None, DeleteChat::BasicGroup { .. }) => {
            append(tg_lang::tr::lng_delete_for_me_chat_hint(count));
            None
        }
        (None, DeleteChat::User { .. }) => {
            append(tg_lang::tr::lng_delete_for_me_hint(count));
            None
        }
        (None, DeleteChat::SavedMessages | DeleteChat::Channel) => None,
    };
    DeleteBoxTexts { text, checkbox }
}

/// Action emitted by [`DeleteConfirmBoxState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteConfirmAction {
    /// Confirmed deletion with `revoke_for_everyone`.
    Confirm {
        /// Target message ids.
        msg_ids: Vec<MsgId>,
        /// True to delete for all participants.
        revoke_for_everyone: bool,
    },
    /// User cancelled.
    Cancelled,
}

/// Delete Messages confirmation box state.
#[derive(Debug, Clone)]
pub struct DeleteConfirmBoxState {
    /// Message ids queued for deletion.
    pub msg_ids: Vec<MsgId>,
    /// Question, details and checkbox label.
    pub texts: DeleteBoxTexts,
    /// Whether the revoke checkbox is checked (checked by default, as `revokeByDefault` is
    /// until "Remember this choice" stores otherwise).
    pub revoke_for_everyone: bool,
}

impl DeleteConfirmBoxState {
    /// Opens the box for `msg_ids` in `chat`; `items` has one entry per id when all are loaded.
    #[must_use]
    pub fn new(
        msg_ids: Vec<MsgId>,
        chat: &DeleteChat,
        items: Option<&[DeleteItem]>,
        now_unix: i64,
        limits: RevokeLimits,
    ) -> Self {
        let items = items.filter(|items| items.len() == msg_ids.len());
        let revoke = revoke_checkbox(chat, items, now_unix, limits);
        let texts = delete_box_texts(chat, msg_ids.len(), revoke.as_ref());
        let revoke_for_everyone = texts.checkbox.is_some();
        Self {
            msg_ids,
            texts,
            revoke_for_everyone,
        }
    }

    /// Opens the box for loaded `messages` (the ones of `msg_ids` that are in memory).
    #[must_use]
    pub fn for_messages(
        msg_ids: Vec<MsgId>,
        chat: &DeleteChat,
        messages: &[HistoryMessage],
        now_unix: i64,
    ) -> Self {
        let items: Vec<DeleteItem> = msg_ids
            .iter()
            .filter_map(|id| messages.iter().find(|m| m.id == *id))
            .map(DeleteItem::from)
            .collect();
        Self::new(
            msg_ids,
            chat,
            Some(&items),
            now_unix,
            RevokeLimits::default(),
        )
    }

    /// Renders the box; Enter confirms and Escape cancels like `DeleteMessagesBox::keyPressEvent`.
    pub fn show(&mut self, ui: &mut Ui, colors: &HistoryColors) -> Option<DeleteConfirmAction> {
        let mut action = None;
        let ctx = ui.ctx().clone();
        let screen = ctx.viewport_rect();
        egui::Area::new(egui::Id::unique("tg_delete_messages_box_backdrop"))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .show(&ctx, |ui| {
                let (rect, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter().rect_filled(
                    rect,
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(0, 0, 0, 110),
                );
                if resp.clicked() {
                    action = Some(DeleteConfirmAction::Cancelled);
                }
            });
        egui::Area::new(egui::Id::unique("tg_delete_messages_box"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(&ctx, |ui| {
                if let Some(a) = self.paint_box(ui, colors) {
                    action = Some(a);
                }
            });
        ctx.input(|i| {
            if i.key_pressed(egui::Key::Escape) {
                action = Some(DeleteConfirmAction::Cancelled);
            } else if i.key_pressed(egui::Key::Enter) {
                action = Some(self.confirm());
            }
        });
        action
    }

    fn confirm(&self) -> DeleteConfirmAction {
        DeleteConfirmAction::Confirm {
            msg_ids: self.msg_ids.clone(),
            revoke_for_everyone: self.texts.checkbox.is_some() && self.revoke_for_everyone,
        }
    }

    fn paint_box(&mut self, ui: &mut Ui, colors: &HistoryColors) -> Option<DeleteConfirmAction> {
        let (pad_l, pad_t, pad_r, pad_b) = BOX_PADDING;
        let inner_w = BOX_WIDTH - pad_l - pad_r;
        let text_galley = ui.painter().layout(
            self.texts.text.clone(),
            FontId::proportional(BOX_LABEL_FONT),
            colors.text_fg,
            inner_w,
        );
        let checkbox_galley = self.texts.checkbox.as_ref().map(|label| {
            ui.painter().layout(
                label.clone(),
                FontId::proportional(BOX_LABEL_FONT),
                colors.text_fg,
                inner_w - CHECK_SIZE - CHECK_TEXT_GAP,
            )
        });
        let mut content_h = pad_t + text_galley.size().y + pad_b;
        if let Some(g) = &checkbox_galley {
            content_h += BOX_MEDIUM_SKIP + g.size().y.max(CHECK_SIZE);
        }
        let (btn_t, btn_r, btn_b, btn_between) = BOX_BUTTON_PADDING;
        let full_h = content_h + btn_t + BOX_BUTTON_HEIGHT + btn_b;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(BOX_WIDTH, full_h), Sense::hover());
        let painter = ui.painter().clone();
        painter.rect(
            rect,
            CornerRadius::same(6),
            colors.bar_bg,
            Stroke::NONE,
            egui::StrokeKind::Inside,
        );

        let text_pos = Pos2::new(rect.left() + pad_l, rect.top() + pad_t);
        let text_h = text_galley.size().y;
        let text_rect = Rect::from_min_size(text_pos, text_galley.size());
        painter.galley(text_pos, text_galley, colors.text_fg);
        // The question is a FlatLabel in tdesktop: keep it in the accessibility tree.
        let text = self.texts.text.clone();
        ui.interact(text_rect, ui.scope_id().with("text"), Sense::hover())
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &text));

        if let Some(galley) = checkbox_galley {
            let top = text_pos.y + text_h + BOX_MEDIUM_SKIP;
            let row = Rect::from_min_size(
                Pos2::new(rect.left() + pad_l, top),
                Vec2::new(inner_w, galley.size().y.max(CHECK_SIZE)),
            );
            let resp = ui
                .interact(row, ui.scope_id().with("revoke"), Sense::click())
                .on_hover_cursor(CursorIcon::PointingHand);
            let label = self.texts.checkbox.clone().unwrap_or_default();
            resp.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::Role::CheckBox,
                    true,
                    self.revoke_for_everyone,
                    &label,
                )
            });
            if resp.clicked() {
                self.revoke_for_everyone = !self.revoke_for_everyone;
            }
            let check = Rect::from_min_size(row.min, Vec2::splat(CHECK_SIZE));
            let check_style = tg_ui::style::CheckStyle {
                bg: colors.bar_bg,
                untoggled_fg: colors.sub_text_fg,
                toggled_fg: colors.accent,
                check_fg: Color32::WHITE,
                diameter: CHECK_SIZE,
                thickness: 2.0,
                duration_ms: 120,
                ripple_area_padding: 0.0,
            };
            tg_ui::widgets::checkbox::paint_check_view(
                &painter,
                check,
                &check_style,
                if self.revoke_for_everyone { 1.0 } else { 0.0 },
                1.0,
            );
            let text_y = row.top() + (CHECK_SIZE - galley.size().y).max(0.0) * 0.5;
            painter.galley(
                Pos2::new(check.right() + CHECK_TEXT_GAP, text_y),
                galley,
                colors.text_fg,
            );
        }

        let mut action = None;
        let mut right = rect.right() - btn_r;
        let btn_top = rect.top() + content_h + btn_t;
        let delete = tg_lang::tr::lng_box_delete();
        let cancel = tg_lang::tr::lng_cancel();
        for (label, is_delete) in [(delete, true), (cancel, false)] {
            let galley = painter.layout_no_wrap(
                label.clone(),
                FontId::new(BOX_LABEL_FONT, tg_ui::fonts::family_semibold()),
                colors.active_text_fg,
            );
            let w = galley.size().x + BOX_BUTTON_TEXT_PAD * 2.0;
            let btn = Rect::from_min_size(
                Pos2::new(right - w, btn_top),
                Vec2::new(w, BOX_BUTTON_HEIGHT),
            );
            right = btn.left() - btn_between;
            let resp = ui
                .interact(
                    btn,
                    ui.scope_id().with(("box_button", is_delete)),
                    Sense::click(),
                )
                .on_hover_cursor(CursorIcon::PointingHand);
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
            if resp.hovered() {
                painter.rect_filled(btn, CornerRadius::same(4), colors.bar_bg_over);
            }
            painter.galley(
                Pos2::new(
                    btn.left() + BOX_BUTTON_TEXT_PAD,
                    btn.center().y - galley.size().y * 0.5,
                ),
                galley,
                colors.active_text_fg,
            );
            if resp.clicked() {
                action = Some(if is_delete {
                    self.confirm()
                } else {
                    DeleteConfirmAction::Cancelled
                });
            }
        }
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn item(outgoing: bool) -> DeleteItem {
        DeleteItem {
            outgoing,
            date_unix: (NOW - 60) as i32,
            is_post: false,
        }
    }

    fn user() -> DeleteChat {
        DeleteChat::User {
            first_name: "Avery".to_owned(),
            is_bot: false,
        }
    }

    fn texts(chat: &DeleteChat, items: &[DeleteItem], limits: RevokeLimits) -> DeleteBoxTexts {
        let ids: Vec<MsgId> = (1..=items.len() as i32).map(MsgId).collect();
        DeleteConfirmBoxState::new(ids, chat, Some(items), NOW, limits).texts
    }

    #[test]
    fn private_chat_asks_like_tdesktop_and_offers_also_delete_for_the_first_name() {
        let t = texts(&user(), &[item(true)], RevokeLimits::default());
        assert_eq!(t.text, "Do you want to delete this message?");
        assert_eq!(t.checkbox.as_deref(), Some("Also delete for Avery"));
        let t = texts(&user(), &[item(true), item(false)], RevokeLimits::default());
        assert_eq!(t.text, "Do you want to delete 2 messages?");
        assert_eq!(t.checkbox.as_deref(), Some("Also delete for Avery"));
    }

    #[test]
    fn old_rustgram_wording_is_gone() {
        let t = texts(&user(), &[item(true)], RevokeLimits::default());
        let all = format!("{} {:?}", t.text, t.checkbox);
        assert!(!all.contains("(everyone)"), "{all}");
        assert!(!all.contains("Are you sure"), "{all}");
    }

    #[test]
    fn groups_and_channels_get_their_tdesktop_variants() {
        let limits = RevokeLimits::default();
        let basic = DeleteChat::BasicGroup {
            can_delete_messages: false,
        };
        let t = texts(&basic, &[item(true)], limits);
        assert_eq!(t.checkbox.as_deref(), Some("Delete for everyone"));

        let t = texts(&basic, &[item(true), item(false)], limits);
        assert_eq!(t.checkbox.as_deref(), Some("Unsend my messages"));
        assert!(t.text.ends_with(
            "You can also delete the message you sent from the inboxes of other group members \
             by checking \"Unsend my messages\"."
        ));

        let t = texts(&basic, &[item(false)], limits);
        assert_eq!(t.checkbox, None);
        assert!(t.text.ends_with(
            "This will delete it just for you, not for other participants of the chat."
        ));

        let t = texts(&DeleteChat::Supergroup, &[item(true), item(true)], limits);
        assert_eq!(t.checkbox, None);
        assert!(
            t.text
                .ends_with("This will delete them for everyone in this chat.")
        );

        let t = texts(&DeleteChat::Channel, &[item(true)], limits);
        assert_eq!(
            (t.text.as_str(), t.checkbox),
            ("Do you want to delete this message?", None)
        );
        let t = texts(&DeleteChat::SavedMessages, &[item(true)], limits);
        assert_eq!(
            (t.text.as_str(), t.checkbox),
            ("Do you want to delete this message?", None)
        );
    }

    #[test]
    fn bots_and_expired_messages_are_deleted_just_for_me() {
        let bot = DeleteChat::User {
            first_name: "Helper".to_owned(),
            is_bot: true,
        };
        let t = texts(&bot, &[item(true)], RevokeLimits::default());
        assert_eq!(t.checkbox, None);
        assert!(t.text.ends_with("This will delete it just for you."));

        let strict = RevokeLimits {
            time_limit: 30,
            private_time_limit: 30,
            private_inbox: false,
        };
        let t = texts(&user(), &[item(true)], strict);
        assert_eq!(t.checkbox, None);

        let fresh = DeleteItem {
            date_unix: NOW as i32,
            ..item(true)
        };
        let t = texts(&user(), &[fresh, item(false)], strict);
        assert_eq!(t.checkbox.as_deref(), Some("Unsend my messages"));
        assert!(t.text.ends_with(
            "You can also delete the message you sent from Avery's inbox by checking \
             \"Unsend my messages\"."
        ));
    }

    #[test]
    fn unloaded_messages_give_no_checkbox_and_confirm_never_revokes_then() {
        let ids = vec![MsgId(1), MsgId(2)];
        let state = DeleteConfirmBoxState::new(
            ids.clone(),
            &user(),
            Some(&[item(true)]),
            NOW,
            RevokeLimits::default(),
        );
        assert_eq!(state.texts.checkbox, None);
        assert_eq!(
            state.confirm(),
            DeleteConfirmAction::Confirm {
                msg_ids: ids,
                revoke_for_everyone: false
            }
        );
    }

    #[test]
    fn classifies_basic_groups_by_the_peer_id() {
        let basic = DeleteChat::from_peer(
            PeerId::from_chat(5),
            PeerKind::Group,
            "Team",
            false,
            false,
            true,
        );
        assert_eq!(
            basic,
            DeleteChat::BasicGroup {
                can_delete_messages: true
            }
        );
        let mega = DeleteChat::from_peer(
            PeerId::from_channel(5),
            PeerKind::Group,
            "Team",
            false,
            false,
            true,
        );
        assert_eq!(mega, DeleteChat::Supergroup);
        let first = DeleteChat::from_peer(
            PeerId::from_user(5),
            PeerKind::User,
            "Avery Stone",
            false,
            false,
            false,
        );
        assert_eq!(first, user());
    }
}
