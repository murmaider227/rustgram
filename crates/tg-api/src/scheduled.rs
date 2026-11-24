//! Scheduled messages and Business quick-reply shortcuts protocol layer.
//!
//! Port origin: `data/components/scheduled_messages.cpp`,
//! `data/business/data_shortcut_messages.cpp`, `data/data_histories.cpp:1031-1047`,
//! `window/window_peer_menu.cpp:4062-4080`, `apiwrap.cpp:4512-4526`.

use tg_data::{
    BusinessShortcutId, MsgId, Peer, PeerId, QuickReplyShortcutData, Session,
    scheduled_local_to_remote_id, shortcut_local_to_remote_id, shortcut_remote_to_local_id,
};
use tg_tl::{enums as tl_enums, functions as tl_fns};

use crate::convert::{apply_tl_users_and_chats, input_peer_for, message_from_tl};
use crate::error::ApiError;
use crate::rpc::{InvocationError, RpcClient};

/// Result of calling [`get_scheduled_history`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduledHistoryOutcome {
    /// Skipped because the peer is a broadcast channel without send permission or
    /// the 60-second `kRequestTimeLimit` cooldown has not expired.
    Skipped,
    /// Server returned `messages.messagesNotModified` (cached list is still fresh).
    NotModified,
    /// Server returned an updated list with this many scheduled messages.
    Updated(usize),
}

/// Fetches scheduled messages for `peer_id` via `messages.getScheduledHistory`
/// (`ScheduledMessages::request` / `parse` in `data/components/scheduled_messages.cpp:489-547`).
///
/// - Skips broadcast channels where the current account cannot post (`can_post_messages == false`).
/// - Skips requests within `SCHEDULED_REQUEST_TIME_LIMIT_MS` (60s) unless `force == true`.
/// - Sends `ScheduledMessagesStore::count_list_hash(peer_id)` as `hash`.
/// - Clears cooldown on RPC failure.
pub async fn get_scheduled_history<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    peer_id: PeerId,
    now_ms: u64,
    force: bool,
) -> Result<ScheduledHistoryOutcome, ApiError> {
    if let Some(Peer::Channel(ch)) = session.peer(peer_id)
        && ch.is_broadcast()
        && !ch.can_post_messages()
    {
        return Ok(ScheduledHistoryOutcome::Skipped);
    }
    if !force
        && session
            .scheduled_messages()
            .too_early_for_request(peer_id, now_ms)
    {
        return Ok(ScheduledHistoryOutcome::Skipped);
    }

    let hash = session.scheduled_messages().count_list_hash(peer_id);
    let req = tl_fns::messages::GetScheduledHistory {
        peer: input_peer_for(session, peer_id),
        hash,
    };

    let resp = match rpc.invoke(&req).await {
        Ok(r) => r,
        Err(err) => {
            session
                .scheduled_messages_mut()
                .clear_request_state(peer_id);
            return Err(ApiError::Invocation(err.into()));
        }
    };

    match resp {
        tl_enums::messages::Messages::NotModified(_) => {
            session
                .scheduled_messages_mut()
                .mark_received(peer_id, now_ms);
            Ok(ScheduledHistoryOutcome::NotModified)
        }
        tl_enums::messages::Messages::Messages(d) => apply_scheduled_messages_payload(
            session,
            peer_id,
            &d.users,
            &d.chats,
            &d.messages,
            now_ms,
        ),
        tl_enums::messages::Messages::Slice(d) => apply_scheduled_messages_payload(
            session,
            peer_id,
            &d.users,
            &d.chats,
            &d.messages,
            now_ms,
        ),
        tl_enums::messages::Messages::ChannelMessages(d) => apply_scheduled_messages_payload(
            session,
            peer_id,
            &d.users,
            &d.chats,
            &d.messages,
            now_ms,
        ),
    }
}

fn apply_scheduled_messages_payload(
    session: &mut Session,
    peer_id: PeerId,
    users: &[tl_enums::User],
    chats: &[tl_enums::Chat],
    messages: &[tl_enums::Message],
    now_ms: u64,
) -> Result<ScheduledHistoryOutcome, ApiError> {
    apply_tl_users_and_chats(session, users, chats);
    let self_id = session.self_user_id();
    let converted: Vec<tg_data::Message> = messages
        .iter()
        .filter_map(|m| message_from_tl(m, self_id))
        .collect();
    session
        .scheduled_messages_mut()
        .apply_history_response(peer_id, converted, now_ms);
    Ok(ScheduledHistoryOutcome::Updated(
        session.scheduled_messages().count(peer_id),
    ))
}

/// Immediately sends scheduled messages via `messages.sendScheduledMessages`
/// (`window/window_peer_menu.cpp:4062-4079`).
///
/// Accepts either local scheduled `MsgId`s (`> SERVER_MAX_MSG_ID`) or remote server IDs,
/// sorts them by ascending scheduled `date`, and converts them to remote `i32` IDs on the wire.
pub async fn send_scheduled_messages<C: RpcClient>(
    rpc: &C,
    session: &Session,
    peer_id: PeerId,
    ids: &[MsgId],
) -> Result<tl_enums::Updates, ApiError> {
    let remote_ids = session
        .scheduled_messages()
        .sorted_remote_ids_for_send(peer_id, ids);
    let req = tl_fns::messages::SendScheduledMessages {
        peer: input_peer_for(session, peer_id),
        id: remote_ids,
    };
    let updates = rpc.invoke(&req).await.map_err(Into::into)?;
    Ok(updates)
}

/// Deletes scheduled messages via `messages.deleteScheduledMessages`
/// (`data/data_histories.cpp:1031-1038`).
///
/// Accepts either local scheduled `MsgId`s or remote server IDs and converts them to remote
/// `i32` IDs on the wire.
pub async fn delete_scheduled_messages<C: RpcClient>(
    rpc: &C,
    session: &Session,
    peer_id: PeerId,
    ids: &[MsgId],
) -> Result<tl_enums::Updates, ApiError> {
    let remote_ids: Vec<i32> = ids
        .iter()
        .filter_map(|&id| {
            let remote = scheduled_local_to_remote_id(id).unwrap_or(id);
            remote.as_i32().filter(|&v| v > 0)
        })
        .collect();
    let req = tl_fns::messages::DeleteScheduledMessages {
        peer: input_peer_for(session, peer_id),
        id: remote_ids,
    };
    let updates = rpc.invoke(&req).await.map_err(Into::into)?;
    Ok(updates)
}

// ============================================================================
// Business Quick-Reply Shortcuts (`data/business/data_shortcut_messages.cpp`)
// ============================================================================

/// Converts a TL `QuickReply` into `tg_data::QuickReplyShortcutData`
/// (`ShortcutMessages::parseShortcut` in `data_shortcut_messages.cpp:229-237`).
#[must_use]
pub fn quick_reply_from_tl(reply: &tl_enums::QuickReply) -> QuickReplyShortcutData {
    let tl_enums::QuickReply::QuickReply(r) = reply;
    let remote_top = MsgId(i64::from(r.top_message));
    let top_message_id = shortcut_remote_to_local_id(remote_top).unwrap_or(MsgId::ZERO);
    QuickReplyShortcutData {
        id: r.shortcut_id,
        name: r.shortcut.clone(),
        top_message_id,
        count: r.count,
    }
}

/// Result of calling [`get_quick_replies`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickRepliesOutcome {
    /// Server returned `messages.quickRepliesNotModified`.
    NotModified,
    /// Server returned updated shortcuts (count of shortcuts now stored).
    Updated(usize),
}

/// Fetches the Business quick-reply shortcuts list via `messages.getQuickReplies`
/// (`ShortcutMessages::preloadShortcuts` in `data/business/data_shortcut_messages.cpp:460-479`).
pub async fn get_quick_replies<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
) -> Result<QuickRepliesOutcome, ApiError> {
    let hash = session.shortcut_messages().shortcuts_hash();
    let resp = rpc
        .invoke(&tl_fns::messages::GetQuickReplies { hash })
        .await
        .map_err(Into::into)?;

    match resp {
        tl_enums::messages::QuickReplies::NotModified => {
            session.shortcut_messages_mut().mark_shortcuts_loaded();
            Ok(QuickRepliesOutcome::NotModified)
        }
        tl_enums::messages::QuickReplies::QuickReplies(data) => {
            apply_tl_users_and_chats(session, &data.users, &data.chats);
            let shortcuts: Vec<QuickReplyShortcutData> =
                data.quick_replies.iter().map(quick_reply_from_tl).collect();
            session.shortcut_messages_mut().update_shortcuts(shortcuts);
            // Also ingest preview messages accompanying `messages.quickReplies` so
            // `has_unknown_top_messages()` sees them immediately, while preserving the
            // server-reported `QuickReply::count` (`ShortcutMessages::preloadShortcuts`,
            // `data/business/data_shortcut_messages.cpp:460-479`).
            let self_id = session.self_user_id();
            for raw in &data.messages {
                if let Some(msg) = message_from_tl(raw, self_id)
                    && let Some(sc_id) = msg.shortcut_id
                {
                    let _ = session
                        .shortcut_messages_mut()
                        .upsert_preview_message(sc_id, msg);
                }
            }
            Ok(QuickRepliesOutcome::Updated(
                session.shortcut_messages().shortcuts().len(),
            ))
        }
    }
}

/// Result of calling [`get_quick_reply_messages`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickReplyMessagesOutcome {
    /// Skipped due to the 60-second `kRequestTimeLimit` cooldown.
    Skipped,
    /// Server returned `messages.messagesNotModified`.
    NotModified,
    /// Server returned updated shortcut messages (count stored for this shortcut).
    Updated(usize),
}

/// Fetches messages for `shortcut_id` via `messages.getQuickReplyMessages`
/// (`ShortcutMessages::request` / `parse` in `data/business/data_shortcut_messages.cpp:602-660`).
///
/// Sends `ShortcutMessagesStore::count_list_hash(shortcut_id)` (which hashes `(remote_id,
/// edit_date_or_0)` in reverse order WITHOUT `date`!). Respects the 60-second cooldown
/// unless `force == true`.
pub async fn get_quick_reply_messages<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    shortcut_id: BusinessShortcutId,
    now_ms: u64,
    force: bool,
) -> Result<QuickReplyMessagesOutcome, ApiError> {
    if !force
        && session
            .shortcut_messages()
            .too_early_for_request(shortcut_id, now_ms)
    {
        return Ok(QuickReplyMessagesOutcome::Skipped);
    }

    let hash = session.shortcut_messages().count_list_hash(shortcut_id);
    let req = tl_fns::messages::GetQuickReplyMessages {
        shortcut_id,
        id: None,
        hash,
    };

    let resp = match rpc.invoke(&req).await {
        Ok(r) => r,
        Err(err) => {
            session
                .shortcut_messages_mut()
                .clear_request_state(shortcut_id);
            return Err(ApiError::Invocation(err.into()));
        }
    };

    match resp {
        tl_enums::messages::Messages::NotModified(_) => Ok(QuickReplyMessagesOutcome::NotModified),
        tl_enums::messages::Messages::Messages(d) => apply_quick_reply_messages_payload(
            session,
            shortcut_id,
            &d.users,
            &d.chats,
            &d.messages,
            now_ms,
        ),
        tl_enums::messages::Messages::Slice(d) => apply_quick_reply_messages_payload(
            session,
            shortcut_id,
            &d.users,
            &d.chats,
            &d.messages,
            now_ms,
        ),
        tl_enums::messages::Messages::ChannelMessages(d) => apply_quick_reply_messages_payload(
            session,
            shortcut_id,
            &d.users,
            &d.chats,
            &d.messages,
            now_ms,
        ),
    }
}

fn apply_quick_reply_messages_payload(
    session: &mut Session,
    shortcut_id: BusinessShortcutId,
    users: &[tl_enums::User],
    chats: &[tl_enums::Chat],
    messages: &[tl_enums::Message],
    now_ms: u64,
) -> Result<QuickReplyMessagesOutcome, ApiError> {
    apply_tl_users_and_chats(session, users, chats);
    let self_id = session.self_user_id();
    let converted: Vec<tg_data::Message> = messages
        .iter()
        .filter_map(|m| message_from_tl(m, self_id))
        .collect();
    session
        .shortcut_messages_mut()
        .apply_messages_response(shortcut_id, converted, now_ms);
    Ok(QuickReplyMessagesOutcome::Updated(
        session.shortcut_messages().messages(shortcut_id).len(),
    ))
}

/// Sends quick-reply shortcut messages to `peer_id` via `messages.sendQuickReplyMessages`
/// (`ApiWrap::sendShortcutMessages` in `apiwrap.cpp:4512-4526`).
///
/// Converts any local shortcut `MsgId`s into remote server `i32` IDs. Passing empty `ids` and
/// `random_ids` sends the entire shortcut (matching `apiwrap.cpp:4515-4521`).
pub async fn send_quick_reply_messages<C: RpcClient>(
    rpc: &C,
    session: &Session,
    peer_id: PeerId,
    shortcut_id: BusinessShortcutId,
    ids: &[MsgId],
    random_ids: Vec<i64>,
) -> Result<tl_enums::Updates, ApiError> {
    let remote_ids: Vec<i32> = ids
        .iter()
        .filter_map(|&id| {
            let remote = shortcut_local_to_remote_id(id).unwrap_or(id);
            remote.as_i32().filter(|&v| v > 0)
        })
        .collect();
    let req = tl_fns::messages::SendQuickReplyMessages {
        peer: input_peer_for(session, peer_id),
        shortcut_id,
        id: remote_ids,
        random_id: random_ids,
    };
    let updates = rpc.invoke(&req).await.map_err(Into::into)?;
    Ok(updates)
}

/// Renames a Business quick-reply shortcut via `messages.editQuickReplyShortcut`
/// (`ShortcutMessages::editShortcut` in `data/business/data_shortcut_messages.cpp:528-573`).
///
/// - Rejects blank names or collisions with an existing non-empty shortcut (`SHORTCUT_OCCUPIED`).
/// - Treats `SHORTCUT_ID_INVALID` (local placeholder not yet created on server) as local success.
pub async fn edit_quick_reply_shortcut<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    shortcut_id: BusinessShortcutId,
    new_name: &str,
) -> Result<bool, ApiError> {
    let trimmed = new_name.trim();
    if trimmed.is_empty() {
        return Err(ApiError::Validation(
            "Quick reply shortcut name cannot be empty".to_owned(),
        ));
    }
    match session
        .shortcut_messages()
        .check_rename_collision(shortcut_id, trimmed)
    {
        tg_data::ShortcutRenameCollision::Occupied => {
            return Err(ApiError::Invocation(InvocationError::Rpc(
                crate::rpc::RpcError::new(400, "SHORTCUT_OCCUPIED"),
            )));
        }
        tg_data::ShortcutRenameCollision::ReplaceEmpty(empty_dup_id) => {
            let _ = delete_quick_reply_shortcut(rpc, session, empty_dup_id).await;
        }
        tg_data::ShortcutRenameCollision::None => {}
    }

    let req = tl_fns::messages::EditQuickReplyShortcut {
        shortcut_id,
        shortcut: trimmed.to_owned(),
    };
    match rpc.invoke(&req).await {
        Ok(ok) => {
            if ok {
                session
                    .shortcut_messages_mut()
                    .apply_shortcut_renamed(shortcut_id, trimmed.to_owned());
            }
            Ok(ok)
        }
        Err(err) => {
            let inv: InvocationError = err.into();
            if let InvocationError::Rpc(ref rpc_err) = inv
                && rpc_err.name == "SHORTCUT_ID_INVALID"
            {
                // Match `data_shortcut_messages.cpp:566-569`: not yet on server, update locally.
                session
                    .shortcut_messages_mut()
                    .apply_shortcut_renamed(shortcut_id, trimmed.to_owned());
                return Ok(true);
            }
            Err(ApiError::Invocation(inv))
        }
    }
}

/// Deletes a Business quick-reply shortcut via `messages.deleteQuickReplyShortcut`
/// (`ShortcutMessages::removeShortcut` in `data/business/data_shortcut_messages.cpp:575-592`).
pub async fn delete_quick_reply_shortcut<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    shortcut_id: BusinessShortcutId,
) -> Result<bool, ApiError> {
    session
        .shortcut_messages_mut()
        .apply_delete_quick_reply(shortcut_id);
    let ok = rpc
        .invoke(&tl_fns::messages::DeleteQuickReplyShortcut { shortcut_id })
        .await
        .map_err(Into::into)?;
    Ok(ok)
}

/// Deletes messages from a Business quick-reply shortcut via `messages.deleteQuickReplyMessages`
/// (`data/data_histories.cpp:1039-1047`).
pub async fn delete_quick_reply_messages<C: RpcClient>(
    rpc: &C,
    _session: &Session,
    shortcut_id: BusinessShortcutId,
    ids: &[MsgId],
) -> Result<tl_enums::Updates, ApiError> {
    let remote_ids: Vec<i32> = ids
        .iter()
        .filter_map(|&id| {
            let remote = shortcut_local_to_remote_id(id).unwrap_or(id);
            remote.as_i32().filter(|&v| v > 0)
        })
        .collect();
    let updates = rpc
        .invoke(&tl_fns::messages::DeleteQuickReplyMessages {
            shortcut_id,
            id: remote_ids,
        })
        .await
        .map_err(Into::into)?;
    Ok(updates)
}
