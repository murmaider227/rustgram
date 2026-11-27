//! [`PeerMenuCaps`] of a live chat: the rights `Window::Filler` reads for the chat's "three dots"
//! menu (`window/window_peer_menu.cpp`), taken from the `tg-data` peer.

use tg_app_core::history::peer_menu::{PeerMenuCaps, PeerMenuKind};
use tg_app_core::stand_in::types::PeerId;
use tg_data::{ChannelFlags, ChatBannedRights, Peer, Session};

use crate::convert::{from_data_peer, to_data_peer};

/// Reads [`PeerMenuCaps`] for `peer_id` from `session` (mute state excluded: the caller adds it).
#[must_use]
pub fn peer_menu_caps(session: &Session, peer_id: PeerId) -> Option<PeerMenuCaps> {
    let data_peer = to_data_peer(peer_id);
    let peer = session.peer(data_peer)?;
    let ctx = session.send_context();
    let history = session.history(data_peer);
    let has_messages = history.is_some_and(|h| h.top_message_id.is_some());
    let mut caps = PeerMenuCaps {
        peer_id,
        am_in: true,
        ..PeerMenuCaps::default()
    };
    // `Data::CanSend(peer, ChatRestriction::SendPolls)`.
    let can_send_polls = peer.can_send(ChatBannedRights::SEND_POLLS, true, ctx);
    match peer {
        Peer::User(user) => {
            let bot = user.bot_info.as_ref();
            caps.kind = if user.is_self() {
                PeerMenuKind::SavedMessages
            } else if bot.is_some() {
                PeerMenuKind::Bot
            } else {
                PeerMenuKind::User
            };
            caps.replies_or_verify_codes =
                user.is_replies_chat(ctx.is_test_environment) || user.is_verify_codes();
            caps.service_user = user.is_service_user();
            caps.inaccessible = user.is_inaccessible();
            caps.support = user.is_support();
            caps.blocked = peer.common().block_status == tg_data::BlockStatus::Blocked;
            caps.premium_required_to_write =
                user.requires_premium_to_write() && !ctx.account_premium;
            caps.bot_start_pending =
                bot.is_some_and(|b| !b.start_token.is_empty()) || (bot.is_some() && !has_messages);
            // `EditPeerInfoBox::Available`: `bot->botInfo->canEditInformation`.
            caps.can_manage = bot.is_some_and(|b| b.can_edit_information);
            caps.forum = bot.is_some() && peer.is_forum();
            caps.can_create_topics = caps.forum;
            // `PeerData::canCreatePolls` for users.
            caps.can_create_polls = user.is_self()
                || (bot.is_some() && !user.is_support() && !caps.replies_or_verify_codes);
            caps.can_create_todo_lists = ctx.account_premium;
            caps.can_post_stories = user.is_self();
            caps.sharing_disabled = !user.allows_forwarding();
        }
        Peer::Chat(chat) => {
            caps.kind = PeerMenuKind::BasicGroup;
            caps.am_in = chat.am_in();
            caps.am_creator = chat.am_creator();
            caps.can_delete_messages = chat.can_delete_messages();
            // `ChatData::canEditInformation() || canEditPermissions()`.
            caps.can_manage = (chat.am_in()
                && !peer
                    .am_restricted(ChatBannedRights::CHANGE_INFO, ctx)
                    .is_restricted())
                || chat.can_edit_permissions();
            caps.can_create_polls = can_send_polls;
            caps.can_create_todo_lists = ctx.account_premium && can_send_polls;
        }
        Peer::Channel(channel) => {
            caps.kind = if channel.is_megagroup() {
                PeerMenuKind::Megagroup
            } else {
                PeerMenuKind::Broadcast
            };
            caps.am_in = channel.am_in();
            caps.am_creator = channel.am_creator();
            caps.can_delete_messages = channel.can_delete_messages();
            caps.public = channel.is_public();
            caps.forum = channel.is_forum();
            caps.forum_tabs = channel.flags.contains(ChannelFlags::FORUM_TABS);
            caps.monoforum = channel.is_monoforum();
            // `EditPeerInfoBox::Available`: `canViewBanned() || canEditInformation() ||
            // canEditPermissions() || hasAdminRights() || amCreator()`; a supergroup member may
            // edit the info unless `ChangeInfo` is restricted.
            let megagroup_info = channel.is_megagroup()
                && !peer
                    .am_restricted(ChatBannedRights::CHANGE_INFO, ctx)
                    .is_restricted();
            caps.can_manage = !channel.is_monoforum()
                && (channel.has_admin_rights() || channel.am_creator() || megagroup_info);
            caps.can_create_topics = channel.is_forum()
                && !peer
                    .am_restricted(ChatBannedRights::MANAGE_TOPICS, ctx)
                    .is_restricted();
            caps.can_create_polls = !channel.is_monoforum() && can_send_polls;
            caps.can_create_todo_lists = ctx.account_premium
                && !channel.is_monoforum()
                && channel.is_megagroup()
                && can_send_polls;
            caps.can_post_stories = channel.can_post_stories();
            caps.can_edit_stories = channel.can_edit_stories();
            if channel.is_broadcast() {
                caps.discussion = channel
                    .discussion_link
                    .map(|id| from_data_peer(tg_data::PeerId::from_channel(id)));
                // `broadcastMonoforum() && monoforum->amMonoforumAdmin()`: the monoforum's
                // `MonoforumAdmin` flag follows the broadcast's `canAccessMonoforum()`
                // (`ChannelData::setAdminRights`, `data_channel.cpp`).
                caps.direct_messages =
                    channel.monoforum_link.is_some() && channel.can_access_monoforum();
            }
        }
    }
    // `PeerData::canExportChatHistory`: never Replies / Verification Codes or a chat with
    // "Restrict saving content", and only once the chat has a message.
    caps.can_export = !caps.replies_or_verify_codes && peer.allows_forwarding() && has_messages;
    Some(caps)
}
