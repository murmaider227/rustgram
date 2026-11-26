//! Clicks on hashtags, cashtags, bot commands and name mentions in message text.
//!
//! Port origin: `HashtagClickHandler` / `CashtagClickHandler` (`SearchByHashtag`),
//! `BotCommandClickHandler` and `MentionNameClickHandler` (`core/click_handler_types.cpp`), which
//! reach `HistoryInner::elementSearchInList` / `elementSendBotCommand` and
//! `SessionController::showPeerInfo`, and `Bot::WrapCommandInChat` (`chat_helpers/bot_command.cpp`).
//!
//! The text renderers report these entities as [`EntityLink`] targets; the inner widget turns them
//! into [`InnerEvent::EntityLink`](crate::history::inner::InnerEvent::EntityLink) with the message
//! id, and the history widget handles them here, before the generic link router.

use tg_bots::commands::{BotStatus, wrap_command_in_chat};
use tg_bots::stand_in::PeerKind as BotPeerKind;
use tg_text::TextWithEntities;
use tg_ui::entity_link::EntityLink;

use crate::history::backend::{HistoryBackend, HistoryPeerInfo, SendMessageRequest, to_bot_peer};
use crate::history::{HistoryAction, HistoryWidget};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind};

/// `Bot::WrapCommandInChat` for a command clicked in a message: only a group (`isChat` /
/// `isMegagroup`) adds `@bot`, and only for a message from a bot with a username.
pub(crate) fn clicked_command_text(
    chat_kind: PeerKind,
    command: &str,
    sender_bot_username: Option<&str>,
    group_bot_status: BotStatus,
) -> String {
    match (chat_kind, sender_bot_username) {
        (PeerKind::Group, Some(bot)) => wrap_command_in_chat(group_bot_status, command, bot),
        _ => command.to_owned(),
    }
}

impl HistoryWidget {
    /// Opens the in-chat search with `query` (`HistoryInner::elementSearchInList`).
    pub fn search_in_chat(&mut self, query: &str) {
        self.compose_search.open(query);
        self.search_open = true;
    }

    /// Handles a click on `link` in the text of message `msg_id`.
    pub(super) fn handle_entity_link(
        &mut self,
        msg_id: MsgId,
        link: EntityLink,
        peer_id: PeerId,
        peer_info: &HistoryPeerInfo,
        backend: &mut dyn HistoryBackend,
        now_secs: f64,
    ) -> Option<HistoryAction> {
        match link {
            EntityLink::Hashtag(tag) | EntityLink::Cashtag(tag) => {
                self.search_in_chat(&tag);
                None
            }
            EntityLink::BotCommand(command) => {
                let text = self.command_in_chat(msg_id, &command, peer_id, peer_info, backend);
                self.send_bot_command_text(text, peer_id, peer_info, backend, now_secs)
            }
            EntityLink::MentionName(user_id) => {
                Some(HistoryAction::OpenPeerProfile(PeerId::from_user(user_id)))
            }
        }
    }

    /// `Bot::WrapCommandInChat(peer, command, context)`: in a group, a command from a bot's
    /// message gets `@bot` unless it already names one.
    fn command_in_chat(
        &self,
        msg_id: MsgId,
        command: &str,
        peer_id: PeerId,
        peer_info: &HistoryPeerInfo,
        backend: &dyn HistoryBackend,
    ) -> String {
        let kind = peer_info.summary.kind;
        let bots = backend
            .as_bots_backend()
            .filter(|_| kind == PeerKind::Group);
        let from_id = self.find_loaded_message(msg_id).map(|m| m.from_id);
        let (Some(bots), Some(from_id)) = (bots, from_id) else {
            return command.to_owned();
        };
        let sender_bot = bots
            .load_bot_chat_for_peer(to_bot_peer(from_id))
            .filter(|s| s.peer_kind == BotPeerKind::Bot)
            .map(|s| s.bot_username);
        let status = bots
            .load_bot_chat_for_peer(to_bot_peer(peer_id))
            .map_or(BotStatus::Unknown, |s| s.peer_bot_status);
        clicked_command_text(kind, command, sender_bot.as_deref(), status)
    }

    /// Sends a command clicked in a message (`HistoryWidget::sendBotCommand` without a reply).
    fn send_bot_command_text(
        &mut self,
        text: String,
        peer_id: PeerId,
        peer_info: &HistoryPeerInfo,
        backend: &mut dyn HistoryBackend,
        now_secs: f64,
    ) -> Option<HistoryAction> {
        let request = SendMessageRequest {
            text: TextWithEntities::simple(text.clone()),
            reply_to: None,
            silent: self.compose.silent_mode,
        };
        match backend.send_message(peer_id, request) {
            Ok(sent) => {
                self.inner.append_message(sent, peer_info.summary.kind);
                Some(HistoryAction::SendMessage { peer_id, text })
            }
            Err(error) => {
                self.show_send_failure(&error, peer_info.summary.kind, now_secs);
                None
            }
        }
    }
}
