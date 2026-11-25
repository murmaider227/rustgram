//! "Clear history" and "Delete chat" with the `DeleteMessagesBox` "also delete for" choice.
//!
//! Port origin: `Telegram/SourceFiles/apiwrap.cpp` (`ApiWrap::clearHistory`,
//! `ApiWrap::deleteConversation`, `ApiWrap::deleteHistory`),
//! `Telegram/SourceFiles/data/data_histories.cpp` (`Histories::deleteAllMessages`) and
//! `Telegram/SourceFiles/data/data_peer.cpp` (`PeerData::canRevokeFullHistory`).

use tg_app_core::stand_in::types::PeerId;
use tg_data::{MsgId as DataMsgId, Peer};
use tg_tl::{enums as tl_enums, functions as tl_fns};

use crate::convert::{history_messages_ordered, to_data_peer};
use crate::session::LiveSession;

/// `PeerData::canRevokeFullHistory`: another user (not a bot unless support, not deleted), or a
/// basic group / supergroup the account created.
pub(crate) fn can_revoke_full_history(live: &LiveSession, peer_id: PeerId) -> bool {
    let data_peer = to_data_peer(peer_id);
    live.read_session(|s| match s.peer(data_peer) {
        Some(Peer::User(u)) => {
            !u.is_self() && (!u.is_bot() || u.is_support()) && !u.is_inaccessible()
        }
        Some(Peer::Chat(chat)) => chat.am_creator(),
        Some(Peer::Channel(ch)) => ch.is_megagroup() && ch.am_creator(),
        None => false,
    })
}

/// `UserData::firstName` of user `peer_id`.
pub(crate) fn peer_first_name(live: &LiveSession, peer_id: PeerId) -> Option<String> {
    let data_peer = to_data_peer(peer_id);
    live.read_session(|s| match s.peer(data_peer) {
        Some(Peer::User(u)) if !u.first_name.is_empty() => Some(u.first_name.clone()),
        _ => None,
    })
}

fn last_message_id(live: &LiveSession, peer_id: PeerId) -> i32 {
    let data_peer = to_data_peer(peer_id);
    live.read_session(|s| {
        s.history(data_peer)
            .and_then(|h| h.top_message_id.or_else(|| h.loaded_ranges.max_id()))
            .map(|m| m.0 as i32)
            .unwrap_or(0)
    })
}

/// `ApiWrap::clearHistory(peer, revoke)`: `channels.deleteHistory` (`for_everyone` when
/// `revoke`) for supergroups and channels, `messages.deleteHistory` with `just_clear` (and
/// `revoke`) for the rest, then clears the loaded messages locally.
pub(crate) fn clear_history_for(
    live: &LiveSession,
    peer_id: PeerId,
    revoke: bool,
) -> Result<(), String> {
    let max_id = last_message_id(live, peer_id);
    if let Some(channel) = live.input_channel(peer_id) {
        let updates = live.invoke_rpc(tl_fns::channels::DeleteHistory {
            for_everyone: revoke,
            channel,
            max_id,
        })?;
        let _ = live.apply_updates_to_session(&updates);
    } else if let Some(peer) = live.input_peer(peer_id) {
        for _ in 0..tg_api::DELETE_HISTORY_RETRY_LIMIT {
            let tl_enums::messages::AffectedHistory::AffectedHistory(aff) =
                live.invoke_rpc(tl_fns::messages::DeleteHistory {
                    just_clear: true,
                    revoke,
                    peer: peer.clone(),
                    max_id,
                    min_date: None,
                    max_date: None,
                })?;
            let _ = live.apply_affected_history(Some(peer_id), aff.pts, aff.pts_count);
            if aff.offset <= 0 {
                break;
            }
        }
    } else {
        return Err("Peer not found in session for clear_history_for.".to_owned());
    }
    let data_peer = to_data_peer(peer_id);
    let ids: Vec<DataMsgId> = live.read_session(|s| {
        history_messages_ordered(s, data_peer)
            .into_iter()
            .map(|m| m.id)
            .collect()
    });
    live.write_session(|s| {
        s.delete_messages(data_peer, &ids);
        s.set_history_unread_count(data_peer, 0);
        s.set_history_unread_mark(data_peer, false);
    });
    live.bridge.state().notify_ui();
    Ok(())
}

/// `ApiWrap::deleteConversation(peer, revoke)`: with `revoke`, a supergroup the account created
/// is deleted (`channels.deleteChannel`) and a basic group it created is deleted
/// (`messages.deleteChat`); every other case leaves / deletes the history like before.
pub(crate) fn delete_conversation(
    live: &LiveSession,
    peer_id: PeerId,
    revoke: bool,
) -> Result<(), String> {
    let data_peer = to_data_peer(peer_id);
    let (owned_supergroup, owned_basic_chat) = live.read_session(|s| match s.peer(data_peer) {
        Some(Peer::Channel(ch)) => (ch.is_megagroup() && ch.am_creator(), None),
        Some(Peer::Chat(chat)) if chat.am_creator() => (false, data_peer.to_chat()),
        _ => (false, None),
    });
    if revoke && owned_supergroup {
        let channel = live
            .input_channel(peer_id)
            .ok_or_else(|| "Channel not found in session.".to_owned())?;
        let updates = live.invoke_rpc(tl_fns::channels::DeleteChannel { channel })?;
        let _ = live.apply_updates_to_session(&updates);
    } else if revoke && let Some(chat_id) = owned_basic_chat {
        let _ = live.invoke_rpc(tl_fns::messages::DeleteChat {
            chat_id: chat_id.to_tl(),
        })?;
    } else {
        return crate::dialogs_actions::delete_chat(live, peer_id, revoke);
    }
    live.write_session(|s| {
        let ids: Vec<DataMsgId> = history_messages_ordered(s, data_peer)
            .into_iter()
            .map(|m| m.id)
            .collect();
        if !ids.is_empty() {
            s.delete_messages(data_peer, &ids);
        }
        if let Some(h) = s.history_mut(data_peer) {
            h.in_chat_list = false;
        }
        s.sync_dialog_lists(data_peer);
    });
    live.bridge.state().notify_ui();
    Ok(())
}
