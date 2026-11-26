//! Saved Messages dialogs, monoforum sublists, saved history pagination, and Saved Messages
//! reaction tags protocol layer.
//!
//! Port origin: `data/data_saved_messages.cpp`, `data/data_saved_sublist.cpp`,
//! `apiwrap.cpp` (`savePinnedOrder(SavedMessages*)`, `deleteSublistHistory`),
//! `window/window_peer_menu.cpp:484-500`, `data/data_message_reactions.cpp:581-586,1053-1077`.

use tg_data::{
    ChannelId, FullMsgId, MsgId, MsgRange, PeerId, ReactionId, SAVED_LIST_FIRST_PER_PAGE,
    SAVED_LIST_PER_PAGE, SAVED_MESSAGES_PER_PAGE, SAVED_STALE_PER_REQUEST, SERVER_MAX_MSG_ID,
    SavedDialogsOffset, SavedMessagesState, SavedReactionTagData, Session, TimeId,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::convert::{
    apply_tl_message, apply_tl_users_and_chats, draft_from_tl, input_peer_for, message_from_tl,
    peer_id_from_tl, reaction_from_tl, reaction_to_tl,
};
use crate::error::ApiError;
use crate::history::{HistorySlice, MAX_OFFSET_ID, parse_history_result};
use crate::rpc::{InvocationError, RpcClient};
use crate::types::LoadDirection;
use crate::updates::UpdatesHandler;

/// Maximum `messages.deleteSavedHistory` pagination loops.
pub const MAX_DELETE_SAVED_HISTORY_LOOPS: usize = 64;

/// Output of one `messages.getSavedDialogs` page load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedDialogsPageResult {
    /// Sublist peer IDs returned in this page.
    pub sublist_peers: Vec<PeerId>,
    /// Pinned sublist peer IDs fetched alongside the first page (own Saved Messages only).
    pub pinned_peers: Option<Vec<PeerId>>,
    /// Total count reported by `messages.savedDialogsSlice`, if known.
    pub total_count: Option<i32>,
    /// Whether all unpinned saved dialog pages are now loaded.
    pub all_loaded: bool,
    /// Next pagination offset.
    pub next_offset: SavedDialogsOffset,
}

fn state_for(session: &mut Session, parent_chat: Option<ChannelId>) -> &mut SavedMessagesState {
    match parent_chat {
        Some(ch) => session.ensure_monoforum(ch),
        None => session.saved_messages_mut(),
    }
}

/// Applies a `messages.SavedDialogs` response into `session` (`SavedMessages::applyReceivedSublists`
/// in `data/data_saved_messages.cpp:374-466`).
///
/// Returns `(applied_peers, next_offset, all_loaded, total_count)`.
pub fn apply_tl_saved_dialogs(
    session: &mut Session,
    parent_chat: Option<ChannelId>,
    resp: &tl_enums::messages::SavedDialogs,
    pinned_call: bool,
) -> (Vec<PeerId>, SavedDialogsOffset, bool, Option<i32>) {
    let (dialogs, messages, chats, users, is_complete_variant, total_count) = match resp {
        tl_enums::messages::SavedDialogs::SavedDialogs(d) => (
            &d.dialogs,
            &d.messages,
            &d.chats,
            &d.users,
            true,
            Some(d.dialogs.len() as i32),
        ),
        tl_enums::messages::SavedDialogs::Slice(s) => (
            &s.dialogs,
            &s.messages,
            &s.chats,
            &s.users,
            false,
            Some(s.count),
        ),
        tl_enums::messages::SavedDialogs::NotModified(nm) => {
            return (
                Vec::new(),
                SavedDialogsOffset::default(),
                true,
                Some(nm.count),
            );
        }
    };

    apply_tl_users_and_chats(session, users, chats);
    for msg in messages {
        apply_tl_message(session, msg);
    }

    let parent_peer_id = parent_chat
        .map(PeerId::from_channel)
        .unwrap_or_else(|| session.self_peer_id());

    let mut applied_peers = Vec::with_capacity(dialogs.len());
    let mut server_pinned = Vec::new();
    let mut next_offset = SavedDialogsOffset::default();
    let mut last_valid = false;

    for raw_dialog in dialogs {
        match raw_dialog {
            tl_enums::SavedDialog::SavedDialog(d) => {
                let peer_id = peer_id_from_tl(&d.peer);
                let top_id = MsgId(i64::from(d.top_message));
                let entry_pinned = pinned_call || d.pinned;
                if entry_pinned {
                    server_pinned.push(peer_id);
                }
                let top_date = session
                    .message(FullMsgId::new(parent_peer_id, top_id))
                    .filter(|m| !m.is_service())
                    .map(|m| m.date);
                if let Some(date) = top_date {
                    next_offset = SavedDialogsOffset {
                        date,
                        id: top_id,
                        peer_id,
                    };
                    last_valid = true;
                    let sub = state_for(session, parent_chat).ensure_sublist(peer_id);
                    sub.top_message_id = Some(top_id);
                    sub.last_message_date = date;
                    sub.pinned = entry_pinned;
                    applied_peers.push(peer_id);
                } else {
                    last_valid = false;
                }
            }
            tl_enums::SavedDialog::MonoForumDialog(d) => {
                let peer_id = peer_id_from_tl(&d.peer);
                let top_id = MsgId(i64::from(d.top_message));
                if pinned_call {
                    server_pinned.push(peer_id);
                }
                let draft = d.draft.as_ref().and_then(draft_from_tl);
                let top_date = session
                    .message(FullMsgId::new(parent_peer_id, top_id))
                    .filter(|m| !m.is_service())
                    .map(|m| m.date);
                if let Some(date) = top_date {
                    next_offset = SavedDialogsOffset {
                        date,
                        id: top_id,
                        peer_id,
                    };
                    last_valid = true;
                    let sub = state_for(session, parent_chat).ensure_sublist(peer_id);
                    sub.top_message_id = Some(top_id);
                    sub.last_message_date = date;
                    sub.unread_mark = d.unread_mark;
                    sub.nopaid_messages_exception = d.nopaid_messages_exception;
                    sub.apply_inbox_read_till(
                        MsgId(i64::from(d.read_inbox_max_id)),
                        Some(d.unread_count.max(0)),
                    );
                    sub.apply_outbox_read_till(MsgId(i64::from(d.read_outbox_max_id)));
                    sub.unread_reactions_count = d.unread_reactions_count.max(0);
                    sub.draft = draft;
                    applied_peers.push(peer_id);
                } else {
                    last_valid = false;
                }
            }
        }
    }

    let all_loaded = if pinned_call {
        state_for(session, parent_chat).reconcile_pinned_list(&server_pinned);
        false
    } else {
        !last_valid || is_complete_variant
    };

    (applied_peers, next_offset, all_loaded, total_count)
}

/// Fetches pinned Saved Messages dialogs via `messages.getPinnedSavedDialogs`
/// (`SavedMessages::loadPinned` in `data/data_saved_messages.cpp:345-372`).
pub async fn load_pinned_saved_dialogs<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
) -> Result<Vec<PeerId>, ApiError> {
    let resp = match rpc.invoke(&tl_fns::messages::GetPinnedSavedDialogs).await {
        Ok(r) => r,
        Err(err) => {
            let inv: InvocationError = err.into();
            if let InvocationError::Rpc(ref rpc_err) = inv
                && rpc_err.name == "SAVED_DIALOGS_UNSUPPORTED"
            {
                session.saved_messages_mut().mark_unsupported();
            } else {
                session.saved_messages_mut().pinned_loaded = true;
            }
            return Err(ApiError::Invocation(inv));
        }
    };
    let (peers, _, _, _) = apply_tl_saved_dialogs(session, None, &resp, true);
    Ok(peers)
}

/// Loads the next page of Saved Messages or monoforum sublists via `messages.getSavedDialogs`
/// (`SavedMessages::sendLoadMore` in `data/data_saved_messages.cpp:298-343`).
///
/// Matches tdesktop:
/// - Always sets `exclude_pinned = true` and `hash = 0`.
/// - For own Saved Messages (`parent_chat == None`), loads pinned dialogs via
///   `messages.getPinnedSavedDialogs` if not yet loaded.
/// - Uses `limit = 20` (`SAVED_LIST_FIRST_PER_PAGE`) when `offset.id == 0`, else `100`
///   (`SAVED_LIST_PER_PAGE`).
/// - Marks loaded if `all_loaded`, `offset` did not change, or server returned out-of-order
///   `applied.offset.date > prev_offset.date` (`data_saved_messages.cpp:323-325`).
/// - Marks unsupported on `SAVED_DIALOGS_UNSUPPORTED`.
pub async fn load_saved_dialogs_page<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    parent_chat: Option<ChannelId>,
) -> Result<Option<SavedDialogsPageResult>, ApiError> {
    let (already_loaded, need_pinned, prev_offset) = {
        let st = state_for(session, parent_chat);
        (
            st.all_loaded || st.unsupported,
            parent_chat.is_none() && !st.pinned_loaded,
            st.offset,
        )
    };
    if already_loaded {
        return Ok(None);
    }

    let pinned_peers = if need_pinned {
        match load_pinned_saved_dialogs(rpc, session).await {
            Ok(peers) => Some(peers),
            Err(ApiError::Invocation(InvocationError::Rpc(ref e)))
                if e.name == "SAVED_DIALOGS_UNSUPPORTED" =>
            {
                return Err(ApiError::Invocation(InvocationError::Rpc(e.clone())));
            }
            Err(_) => None,
        }
    } else {
        None
    };

    let parent_peer = parent_chat.map(|ch| input_peer_for(session, PeerId::from_channel(ch)));
    let offset_peer = if prev_offset.peer_id.is_zero() {
        tl_enums::InputPeer::Empty
    } else {
        input_peer_for(session, prev_offset.peer_id)
    };
    let limit = if prev_offset.id.is_zero() {
        SAVED_LIST_FIRST_PER_PAGE
    } else {
        SAVED_LIST_PER_PAGE
    };

    let req = tl_fns::messages::GetSavedDialogs {
        exclude_pinned: true,
        parent_peer,
        offset_date: prev_offset.date,
        offset_id: prev_offset.id.as_i32().unwrap_or(0),
        offset_peer,
        limit,
        hash: 0,
    };

    let resp = match rpc.invoke(&req).await {
        Ok(r) => r,
        Err(err) => {
            let inv: InvocationError = err.into();
            if let InvocationError::Rpc(ref rpc_err) = inv
                && rpc_err.name == "SAVED_DIALOGS_UNSUPPORTED"
            {
                state_for(session, parent_chat).mark_unsupported();
            } else {
                state_for(session, parent_chat).all_loaded = true;
            }
            return Err(ApiError::Invocation(inv));
        }
    };

    let (sublist_peers, applied_offset, resp_all_loaded, total_count) =
        apply_tl_saved_dialogs(session, parent_chat, &resp, false);

    // Match `data_saved_messages.cpp:321-328`:
    // mark loaded if all_loaded, offset unchanged, or date moved backwards (out-of-order slice).
    let out_of_order = prev_offset.date > 0 && applied_offset.date > prev_offset.date;
    let done = resp_all_loaded || applied_offset == prev_offset || out_of_order;
    {
        let st = state_for(session, parent_chat);
        if done {
            st.all_loaded = true;
        } else {
            st.offset = applied_offset;
        }
    }

    Ok(Some(SavedDialogsPageResult {
        sublist_peers,
        pinned_peers,
        total_count,
        all_loaded: done,
        next_offset: if done { prev_offset } else { applied_offset },
    }))
}

/// Loads specific monoforum sublists by peer ID via `messages.getSavedDialogsByID`
/// (`SavedMessages::requestSomeStale` in `data/data_saved_messages.cpp:169-230`), batched in
/// chunks of `SAVED_STALE_PER_REQUEST` (100).
pub async fn load_saved_dialogs_by_id<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    parent_chat: ChannelId,
    peer_ids: &[PeerId],
) -> Result<Vec<PeerId>, ApiError> {
    let parent_peer = Some(input_peer_for(session, PeerId::from_channel(parent_chat)));
    let mut loaded = Vec::new();
    for chunk in peer_ids.chunks(SAVED_STALE_PER_REQUEST) {
        let ids: Vec<tl_enums::InputPeer> = chunk
            .iter()
            .map(|&pid| input_peer_for(session, pid))
            .collect();
        if ids.is_empty() {
            continue;
        }
        let resp = rpc
            .invoke(&tl_fns::messages::GetSavedDialogsByID {
                parent_peer: parent_peer.clone(),
                ids,
            })
            .await
            .map_err(Into::into)?;
        let (peers, _, _, _) = apply_tl_saved_dialogs(session, Some(parent_chat), &resp, false);
        loaded.extend(peers);
    }
    Ok(loaded)
}

/// Pins or unpins a Saved Messages sublist via `messages.toggleSavedDialogPin`
/// (`window_peer_menu.cpp:484-496`).
pub async fn toggle_saved_dialog_pin<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    sublist_peer_id: PeerId,
    pinned: bool,
) -> Result<bool, ApiError> {
    let req = tl_fns::messages::ToggleSavedDialogPin {
        pinned,
        peer: tl_enums::InputDialogPeer::InputDialogPeer(tl_types::InputDialogPeer {
            peer: input_peer_for(session, sublist_peer_id),
        }),
    };
    let ok = rpc.invoke(&req).await.map_err(Into::into)?;
    if ok {
        session
            .saved_messages_mut()
            .set_sublist_pinned(sublist_peer_id, pinned);
    }
    Ok(ok)
}

/// Reorders pinned Saved Messages sublists via `messages.reorderPinnedSavedDialogs` with
/// `force = true` (`ApiWrap::savePinnedOrder(Data::SavedMessages*)` in `apiwrap.cpp:496-517`).
pub async fn reorder_pinned_saved_dialogs<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    order: &[PeerId],
) -> Result<bool, ApiError> {
    let peers = order
        .iter()
        .map(|&pid| {
            tl_enums::InputDialogPeer::InputDialogPeer(tl_types::InputDialogPeer {
                peer: input_peer_for(session, pid),
            })
        })
        .collect();
    let req = tl_fns::messages::ReorderPinnedSavedDialogs {
        force: true,
        order: peers,
    };
    let ok = rpc.invoke(&req).await.map_err(Into::into)?;
    if ok {
        let _ = session.saved_messages_mut().apply_pinned_order(Some(order));
    }
    Ok(ok)
}

/// Builds a `messages.getSavedHistory` request matching `SavedSublist::{loadAround,loadBefore,loadAfter}`
/// (`data/data_saved_sublist.cpp:1105-1246`).
#[must_use]
pub fn prepare_saved_history_request(
    parent_peer: Option<tl_enums::InputPeer>,
    sublist_peer: tl_enums::InputPeer,
    message_id: MsgId,
    direction: LoadDirection,
) -> tl_fns::messages::GetSavedHistory {
    let limit = SAVED_MESSAGES_PER_PAGE;
    let raw_offset_id = match direction {
        LoadDirection::Before | LoadDirection::Around => message_id.0,
        LoadDirection::After => message_id.0.saturating_add(1),
    };
    let offset_id = raw_offset_id.clamp(0, i64::from(MAX_OFFSET_ID)) as i32;
    let add_offset = match direction {
        LoadDirection::Before => 0,
        LoadDirection::Around => {
            if message_id.is_zero() {
                0
            } else {
                -limit / 2
            }
        }
        LoadDirection::After => -limit,
    };
    tl_fns::messages::GetSavedHistory {
        parent_peer,
        peer: sublist_peer,
        offset_id,
        offset_date: 0,
        add_offset,
        limit,
        max_id: 0,
        min_id: 0,
        hash: 0,
    }
}

/// Loads a page of Saved Messages or monoforum sublist history via `messages.getSavedHistory`
/// and inserts the resulting messages into `session` and the sublist's `SparseIdsList`.
pub async fn load_saved_history<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    parent_chat: Option<ChannelId>,
    sublist_peer_id: PeerId,
    message_id: MsgId,
    direction: LoadDirection,
) -> Result<HistorySlice, ApiError> {
    let parent_peer = parent_chat.map(|ch| input_peer_for(session, PeerId::from_channel(ch)));
    let sublist_peer = input_peer_for(session, sublist_peer_id);
    let req = prepare_saved_history_request(parent_peer, sublist_peer, message_id, direction);

    let resp = match rpc.invoke(&req).await {
        Ok(r) => r,
        Err(err) => {
            let inv: InvocationError = err.into();
            if let InvocationError::Rpc(ref rpc_err) = inv
                && rpc_err.name == "SAVED_DIALOGS_UNSUPPORTED"
            {
                state_for(session, parent_chat).mark_unsupported();
            }
            return Err(ApiError::Invocation(inv));
        }
    };

    let slice = parse_history_result(message_id, direction, resp);
    apply_tl_users_and_chats(session, &slice.users, &slice.chats);
    let self_id = session.self_user_id();
    let mut ids = Vec::with_capacity(slice.messages.len());
    let mut latest_top: Option<(MsgId, TimeId)> = None;
    for raw in &slice.messages {
        if let Some(msg) = message_from_tl(raw, self_id) {
            ids.push(msg.id);
            if latest_top.is_none_or(|(id, _)| msg.id > id) {
                latest_top = Some((msg.id, msg.date));
            }
            session.insert_existing_message(msg);
        }
    }

    let st = state_for(session, parent_chat);
    let sub = st.ensure_sublist(sublist_peer_id);
    if ids.is_empty() && message_id.is_zero() {
        sub.messages.remove_all();
    } else {
        let range = if message_id.is_zero() && !ids.is_empty() {
            let min_id = ids.iter().copied().min().unwrap_or(MsgId::ZERO);
            MsgRange::new(min_id, SERVER_MAX_MSG_ID)
        } else {
            slice.no_skip_range
        };
        sub.messages.add_slice(ids, range, slice.full_count);
        if let Some((top_id, date)) = latest_top
            && sub.top_message_id.is_none_or(|cur| top_id >= cur)
        {
            sub.top_message_id = Some(top_id);
            sub.last_message_date = date;
        }
    }

    Ok(slice)
}

/// Deletes saved history for `sublist_peer_id` via `messages.deleteSavedHistory`
/// (`ApiWrap::deleteSublistHistorySend` in `apiwrap.cpp:1610-1628`).
///
/// Loops while `affected.offset > 0`, feeds `(pts, pts_count)` into `updates_handler`, and
/// removes the sublist from `session` when deleting full history (`max_id == 0` and no date
/// bounds).
#[allow(clippy::too_many_arguments)]
pub async fn delete_saved_history<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    mut updates_handler: Option<&mut UpdatesHandler>,
    parent_chat: Option<ChannelId>,
    sublist_peer_id: PeerId,
    max_id: MsgId,
    min_date: Option<TimeId>,
    max_date: Option<TimeId>,
    now_ms: u64,
) -> Result<Vec<tl_types::messages::AffectedHistory>, ApiError> {
    let parent_peer = parent_chat.map(|ch| input_peer_for(session, PeerId::from_channel(ch)));
    let peer = input_peer_for(session, sublist_peer_id);
    let mut pages = Vec::new();

    for _ in 0..MAX_DELETE_SAVED_HISTORY_LOOPS {
        let tl_enums::messages::AffectedHistory::AffectedHistory(aff) = rpc
            .invoke(&tl_fns::messages::DeleteSavedHistory {
                parent_peer: parent_peer.clone(),
                peer: peer.clone(),
                max_id: max_id.as_i32().unwrap_or(0),
                min_date,
                max_date,
            })
            .await
            .map_err(Into::into)?;

        if let Some(handler) = updates_handler.as_deref_mut() {
            handler
                .apply_pts_only(rpc, session, parent_chat, aff.pts, aff.pts_count, now_ms)
                .await?;
        }
        let next_offset = aff.offset;
        pages.push(aff);
        if next_offset <= 0 {
            break;
        }
    }

    if max_id.is_zero() && min_date.is_none() && max_date.is_none() {
        state_for(session, parent_chat).apply_sublist_deleted(sublist_peer_id);
    }
    Ok(pages)
}

/// Marks a monoforum sublist read up to `max_id` via `messages.readSavedHistory`
/// (`SavedSublist::sendReadTillRequest` in `data/data_saved_sublist.cpp:648-668`).
pub async fn read_saved_history<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    parent_chat: ChannelId,
    sublist_peer_id: PeerId,
    max_id: MsgId,
) -> Result<bool, ApiError> {
    let req = tl_fns::messages::ReadSavedHistory {
        parent_peer: input_peer_for(session, PeerId::from_channel(parent_chat)),
        peer: input_peer_for(session, sublist_peer_id),
        max_id: max_id.as_i32().unwrap_or(0),
    };
    let ok = rpc.invoke(&req).await.map_err(Into::into)?;
    if ok {
        session
            .ensure_monoforum(parent_chat)
            .ensure_sublist(sublist_peer_id)
            .apply_inbox_read_till(max_id, None);
    }
    Ok(ok)
}

/// Fetches Saved Messages reaction tags via `messages.getSavedReactionTags`
/// (`Reactions::requestMyTags` in `data/data_message_reactions.cpp:1053-1077`).
///
/// Sends the stored per-sublist `hash`, handles `savedReactionTagsNotModified`, updates
/// the stored hash & tag list on full response, and resets `hash = 0` on RPC failure.
/// Returns `Ok(true)` if the tag list was modified, `Ok(false)` on `NotModified`.
pub async fn get_saved_reaction_tags<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    sublist_peer: Option<PeerId>,
) -> Result<bool, ApiError> {
    let hash = session.saved_reaction_tags().hash_for(sublist_peer);
    let peer = sublist_peer.map(|pid| input_peer_for(session, pid));
    let req = tl_fns::messages::GetSavedReactionTags { peer, hash };

    let resp = match rpc.invoke(&req).await {
        Ok(r) => r,
        Err(err) => {
            // Match `data_message_reactions.cpp:1075`: reset hash to 0 on error.
            session
                .saved_reaction_tags_mut()
                .reset_hash_on_error(sublist_peer);
            return Err(ApiError::Invocation(err.into()));
        }
    };

    match resp {
        tl_enums::messages::SavedReactionTags::NotModified => {
            session
                .saved_reaction_tags_mut()
                .mark_not_modified(sublist_peer);
            Ok(false)
        }
        tl_enums::messages::SavedReactionTags::SavedReactionTags(data) => {
            let tags: Vec<SavedReactionTagData> = data
                .tags
                .into_iter()
                .filter_map(|tl_enums::SavedReactionTag::SavedReactionTag(t)| {
                    let reaction = reaction_from_tl(&t.reaction)?;
                    Some(SavedReactionTagData {
                        reaction,
                        title: t.title.filter(|s| !s.is_empty()),
                        count: t.count.max(0),
                    })
                })
                .collect();
            session
                .saved_reaction_tags_mut()
                .apply_tags(sublist_peer, data.hash, tags);
            Ok(true)
        }
    }
}

/// Updates or clears the title of a Saved Messages reaction tag via
/// `messages.updateSavedReactionTag` (`Reactions::renameTag` in
/// `data/data_message_reactions.cpp:581-586`).
///
/// Omits `f_title` (`title: None`) when `title` is empty/blank so Telegram clears the label.
pub async fn update_saved_reaction_tag<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    reaction: &ReactionId,
    title: Option<&str>,
) -> Result<bool, ApiError> {
    let normalized = title
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let req = tl_fns::messages::UpdateSavedReactionTag {
        reaction: reaction_to_tl(reaction),
        title: normalized.clone(),
    };
    let ok = rpc.invoke(&req).await.map_err(Into::into)?;
    if ok {
        session
            .saved_reaction_tags_mut()
            .rename_tag(reaction, normalized);
    }
    Ok(ok)
}
