//! Forum topics protocol layer (`messages.getForumTopics`, `messages.getForumTopicsByID`,
//! `messages.createForumTopic`, `messages.editForumTopic`, `messages.updatePinnedForumTopic`,
//! `messages.reorderPinnedForumTopics`, `messages.deleteTopicHistory`, `messages.readDiscussion`).
//!
//! Port origin: `data/data_forum.cpp`, `data/data_forum_topic.cpp`, `data/data_replies_list.cpp`,
//! `apiwrap.cpp` (`savePinnedOrder`), `window/window_peer_menu.cpp` (`PeerMenuDeleteTopic`),
//! `boxes/peers/edit_forum_topic_box.cpp`, `data/data_histories.cpp` (`sendCreateTopicRequest`).

use tg_data::{
    ChannelFlags, DocumentId, FORUM_GENERAL_TOPIC_ID, ForumOffsets, MsgId, Peer, PeerId,
    PeerUpdateFlags, Session, TOPICS_FIRST_LOAD, TOPICS_PER_PAGE, TOPICS_STALE_PER_REQUEST, TimeId,
    TopicUpdateFlags,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::convert::{
    apply_tl_message, apply_tl_users_and_chats, draft_from_tl, input_peer_for, peer_id_from_tl,
    peer_notify_settings_from_tl,
};
use crate::error::ApiError;
use crate::rpc::{InvocationError, RpcClient};
use crate::updates::UpdatesHandler;

/// Maximum `messages.deleteTopicHistory` loop iterations to guard against runaway server responses.
pub const MAX_DELETE_TOPIC_HISTORY_LOOPS: usize = 64;

/// Sparse patch for `messages.editForumTopic` (`boxes/peers/edit_forum_topic_box.cpp:561-573`,
/// `data/data_forum_topic.cpp:475-483`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForumTopicEdit {
    /// New title, if changing.
    pub title: Option<String>,
    /// New custom emoji icon document ID (`Some(0)` clears custom icon), if changing.
    ///
    /// Automatically omitted when editing the General topic (`root_id == 1`, matching
    /// `edit_forum_topic_box.cpp:566`).
    pub icon_emoji_id: Option<i64>,
    /// New closed state, if changing.
    pub closed: Option<bool>,
    /// New hidden state (General topic), if changing.
    pub hidden: Option<bool>,
}

/// Result of one `messages.getForumTopics` page load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForumTopicsPageResult {
    /// Topic root IDs applied in this page (excluding deleted tombstones).
    pub topic_ids: Vec<MsgId>,
    /// Topic root IDs deleted via `forumTopicDeleted`.
    pub deleted_ids: Vec<MsgId>,
    /// Total count reported by `messages.forumTopics.count`.
    pub total_count: i32,
    /// Server `pts` from `messages.forumTopics`.
    pub pts: i32,
    /// Whether all topics for this forum are now loaded.
    pub all_loaded: bool,
    /// Updated pagination offsets (`offset_date`, `offset_id`, `offset_topic`).
    pub next_offset: ForumOffsets,
}

/// Applies a single TL `ForumTopic` into `session` for `peer_id`
/// (`ForumTopic::applyTopic` in `data/data_forum_topic.cpp:388-432` and
/// `Forum::applyReceivedTopics` in `data/data_forum.cpp:404-452`).
///
/// Honors tdesktop's `if (!min)` (`!data.short`) guard: when `short == true`, preserves
/// existing `pinned`, `notify`, `draft`, `read_inbox_max_id`, `read_outbox_max_id`,
/// `top_message_id`, and unread counters instead of clobbering them with zeroes.
///
/// Returns `Ok(Some(root_id))` when a topic was applied, or `Ok(None)` when `forumTopicDeleted`
/// removed a non-General topic.
pub fn apply_tl_forum_topic(
    session: &mut Session,
    peer_id: PeerId,
    tl_topic: &tl_enums::ForumTopic,
    now: TimeId,
) -> Option<MsgId> {
    let is_bot = session
        .peer(peer_id)
        .and_then(Peer::as_user)
        .is_some_and(|u| u.is_bot());

    match tl_topic {
        tl_enums::ForumTopic::Deleted(d) => {
            let root_id = MsgId(i64::from(d.id));
            // Match `data_forum.cpp:441-447`: never delete General topic (id=1),
            // especially in bot forums where `forumTopicDeleted(1)` may appear.
            if root_id == FORUM_GENERAL_TOPIC_ID {
                session.ensure_forum(peer_id).ensure_topic(root_id);
                Some(root_id)
            } else {
                session.delete_forum_topic(peer_id, root_id, false);
                None
            }
        }
        tl_enums::ForumTopic::ForumTopic(t) => {
            let root_id = MsgId(i64::from(t.id));
            let top_msg_date = if t.top_message > 0 {
                session
                    .message(tg_data::FullMsgId::new(
                        peer_id,
                        MsgId(i64::from(t.top_message)),
                    ))
                    .map(|m| m.date)
            } else {
                None
            };
            let notify = peer_notify_settings_from_tl(&t.notify_settings);
            let is_muted = notify.is_muted(now);
            let draft = t.draft.as_ref().and_then(draft_from_tl);
            let min = t.short;

            let forum = session.ensure_forum(peer_id);
            let topic = forum.ensure_topic(root_id);
            topic.creator_id = peer_id_from_tl(&t.from_id);
            topic.creation_date = t.date;
            topic.title = t.title.clone();
            topic.icon_emoji_id = t
                .icon_emoji_id
                .filter(|&id| id != 0)
                .map(|id| DocumentId(id as u64));
            topic.icon_color = t.icon_color;
            topic.my = t.my;
            topic.closed = t.closed;
            topic.hidden = t.hidden;
            topic.title_missing = t.title_missing;

            if !min {
                topic.pinned = t.pinned;
                topic.notify = notify;
                topic.muted = is_muted;
                topic.draft = draft;
                if t.top_message > 0 {
                    let top_id = MsgId(i64::from(t.top_message));
                    topic.top_message_id = Some(top_id);
                    if top_id > topic.last_known_server_msg_id {
                        topic.last_known_server_msg_id = top_id;
                    }
                    if let Some(d) = top_msg_date {
                        topic.last_message_date = d;
                    } else if topic.last_message_date == 0 {
                        topic.last_message_date = t.date;
                    }
                }
                topic.apply_inbox_read_till(
                    MsgId(i64::from(t.read_inbox_max_id)),
                    Some(t.unread_count.max(0)),
                );
                topic.apply_outbox_read_till(MsgId(i64::from(t.read_outbox_max_id)));
                topic.unread_mentions_count = t.unread_mentions_count.max(0);
                topic.unread_reactions_count = t.unread_reactions_count.max(0);
                topic.unread_poll_votes_count = t.unread_poll_votes_count.max(0);

                if t.pinned {
                    if !forum.pinned_order.contains(&root_id) {
                        forum.pinned_order.push(root_id);
                    }
                } else {
                    forum.pinned_order.retain(|&id| id != root_id);
                }
            } else if is_bot && root_id == FORUM_GENERAL_TOPIC_ID {
                topic.last_message_date = topic.last_message_date.max(t.date);
            }

            session.changes().topic_updated(
                peer_id,
                root_id,
                TopicUpdateFlags::TITLE
                    | TopicUpdateFlags::ICON_ID
                    | TopicUpdateFlags::CLOSED
                    | TopicUpdateFlags::UNREAD_VIEW,
            );
            Some(root_id)
        }
    }
}

/// Applies an entire `messages.ForumTopics` response into `session` (users, chats, messages,
/// channel PTS, and topics) and computes updated pagination offsets
/// (`Forum::applyReceivedTopics` in `data/data_forum.cpp:368-452`).
pub fn apply_tl_forum_topics_response(
    session: &mut Session,
    updates_handler: Option<&mut UpdatesHandler>,
    peer_id: PeerId,
    resp: &tl_types::messages::ForumTopics,
    now: TimeId,
) -> (Vec<MsgId>, Vec<MsgId>, ForumOffsets) {
    apply_tl_users_and_chats(session, &resp.users, &resp.chats);
    for msg in &resp.messages {
        apply_tl_message(session, msg);
    }
    if let Some(ch_id) = peer_id.to_channel()
        && resp.pts > 0
        && let Some(handler) = updates_handler
    {
        handler.init_channel_pts(ch_id, resp.pts);
    }

    let mut next_offset = session
        .forum(peer_id)
        .map_or_else(ForumOffsets::default, |f| f.offset);
    {
        let forum = session.ensure_forum(peer_id);
        forum.order_by_create_date = resp.order_by_create_date;
    }

    let mut applied = Vec::new();
    let mut deleted = Vec::new();
    for raw_topic in &resp.topics {
        match apply_tl_forum_topic(session, peer_id, raw_topic, now) {
            Some(root_id) => {
                applied.push(root_id);
                // Match `Forum::applyReceivedTopics` (`data_forum.cpp:371-377`):
                // update offset_date / offset_id from the topic's lastServerMessage,
                // and always update offset_topic to topic->rootId().
                if let Some(topic) = session.forum_topic(peer_id, root_id) {
                    if let Some(top_id) = topic.top_message_id {
                        if let Some(msg) = session.message(tg_data::FullMsgId::new(peer_id, top_id))
                        {
                            next_offset.date = msg.date;
                            next_offset.id = msg.id;
                        } else if topic.last_message_date > 0 {
                            next_offset.date = topic.last_message_date;
                            next_offset.id = top_id;
                        }
                    }
                    next_offset.topic_id = root_id;
                }
            }
            None => {
                if let tl_enums::ForumTopic::Deleted(d) = raw_topic {
                    deleted.push(MsgId(i64::from(d.id)));
                }
            }
        }
    }
    (applied, deleted, next_offset)
}

/// Loads the next page of forum topics via `messages.getForumTopics`
/// (`Forum::requestTopics` in `data/data_forum.cpp:175-217`).
///
/// Uses `limit = 20` on the first load (`offset.date == 0`) and `limit = 500` on subsequent
/// pages. Terminates when the returned list is empty, `topics.len() == count`, or offsets did
/// not advance. On `CHANNEL_FORUM_MISSING`, clears `ChannelFlags::FORUM` and marks loaded.
pub async fn load_forum_topics_page<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    updates_handler: Option<&mut UpdatesHandler>,
    peer_id: PeerId,
    query: Option<String>,
    now: TimeId,
) -> Result<Option<ForumTopicsPageResult>, ApiError> {
    let (already_loaded, prev_offset) = {
        let forum = session.ensure_forum(peer_id);
        if query.is_none() && forum.all_loaded {
            return Ok(None);
        }
        (
            false,
            if query.is_some() {
                ForumOffsets::default()
            } else {
                forum.offset
            },
        )
    };
    let _ = already_loaded;

    let first_load = prev_offset.date == 0;
    let limit = if first_load {
        TOPICS_FIRST_LOAD
    } else {
        TOPICS_PER_PAGE
    };
    let peer = input_peer_for(session, peer_id);
    let req = tl_fns::messages::GetForumTopics {
        peer,
        q: query.clone().filter(|q| !q.is_empty()),
        offset_date: prev_offset.date,
        offset_id: prev_offset.id.as_i32().unwrap_or(0),
        offset_topic: prev_offset.topic_id.as_i32().unwrap_or(0),
        limit,
    };

    let tl_enums::messages::ForumTopics::ForumTopics(resp) = match rpc.invoke(&req).await {
        Ok(r) => r,
        Err(err) => {
            let inv: InvocationError = err.into();
            if let InvocationError::Rpc(ref rpc_err) = inv
                && rpc_err.name == "CHANNEL_FORUM_MISSING"
            {
                // Match `data_forum.cpp:210-212`: clear `ChannelDataFlag::Forum`.
                if let Some(ch_id) = peer_id.to_channel()
                    && let Some(ch) = session.channel_mut(ch_id)
                {
                    ch.flags.remove(ChannelFlags::FORUM);
                    session
                        .changes()
                        .peer_updated(peer_id, PeerUpdateFlags::FULL_INFO);
                }
                session.ensure_forum(peer_id).all_loaded = true;
            }
            return Err(ApiError::Invocation(inv));
        }
    };

    let (topic_ids, deleted_ids, next_offset) =
        apply_tl_forum_topics_response(session, updates_handler, peer_id, &resp, now);

    // Match `data_forum.cpp:195-199`:
    // mark loaded if list is empty, `list.size() == result.count`, or offset did not change.
    let list_len = resp.topics.len() as i32;
    let all_loaded = resp.topics.is_empty() || list_len == resp.count || next_offset == prev_offset;
    if query.is_none() {
        let forum = session.ensure_forum(peer_id);
        forum.offset = next_offset;
        if all_loaded {
            forum.all_loaded = true;
        }
    }

    Ok(Some(ForumTopicsPageResult {
        topic_ids,
        deleted_ids,
        total_count: resp.count,
        pts: resp.pts,
        all_loaded,
        next_offset,
    }))
}

/// Fetches specific forum topics by root ID via `messages.getForumTopicsByID`
/// (`Forum::requestSomeStale` in `data/data_forum.cpp:454-503`), automatically batching into
/// chunks of `TOPICS_STALE_PER_REQUEST` (100).
pub async fn load_forum_topics_by_id<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    mut updates_handler: Option<&mut UpdatesHandler>,
    peer_id: PeerId,
    root_ids: &[MsgId],
    now: TimeId,
) -> Result<(Vec<MsgId>, Vec<MsgId>), ApiError> {
    let mut all_applied = Vec::new();
    let mut all_deleted = Vec::new();
    let peer = input_peer_for(session, peer_id);

    for chunk in root_ids.chunks(TOPICS_STALE_PER_REQUEST) {
        let topics: Vec<i32> = chunk.iter().filter_map(|id| id.as_i32()).collect();
        if topics.is_empty() {
            continue;
        }
        let tl_enums::messages::ForumTopics::ForumTopics(resp) = rpc
            .invoke(&tl_fns::messages::GetForumTopicsByID {
                peer: peer.clone(),
                topics,
            })
            .await
            .map_err(Into::into)?;
        let (applied, deleted, _) = apply_tl_forum_topics_response(
            session,
            updates_handler.as_deref_mut(),
            peer_id,
            &resp,
            now,
        );
        all_applied.extend(applied);
        all_deleted.extend(deleted);
    }

    Ok((all_applied, all_deleted))
}

/// Creates a new forum topic via `messages.createForumTopic`
/// (`Histories::sendCreateTopicRequest` in `data/data_histories.cpp:1112-1140`).
///
/// Automatically sets `title_missing = true` when creating a topic in a bot forum
/// (`data_histories.cpp:1128`).
#[allow(clippy::too_many_arguments)]
pub async fn create_forum_topic<C: RpcClient>(
    rpc: &C,
    session: &Session,
    peer_id: PeerId,
    title: String,
    icon_color: Option<i32>,
    icon_emoji_id: Option<i64>,
    random_id: i64,
    send_as: Option<tl_enums::InputPeer>,
) -> Result<tl_enums::Updates, ApiError> {
    let is_bot = session
        .peer(peer_id)
        .and_then(Peer::as_user)
        .is_some_and(|u| u.is_bot());
    let req = tl_fns::messages::CreateForumTopic {
        title_missing: is_bot,
        peer: input_peer_for(session, peer_id),
        title,
        icon_color,
        icon_emoji_id: icon_emoji_id.filter(|&id| id != 0),
        random_id,
        send_as,
    };
    let updates = rpc.invoke(&req).await.map_err(Into::into)?;
    Ok(updates)
}

/// Edits a forum topic via `messages.editForumTopic`
/// (`edit_forum_topic_box.cpp:561-587`, `data_forum_topic.cpp:475-491`).
///
/// - Omits `icon_emoji_id` when editing the General topic (`topic_id == 1`).
/// - Treats `TOPIC_NOT_MODIFIED` as a benign `Ok(None)` matching tdesktop.
pub async fn edit_forum_topic<C: RpcClient>(
    rpc: &C,
    session: &Session,
    peer_id: PeerId,
    topic_id: MsgId,
    edit: ForumTopicEdit,
) -> Result<Option<tl_enums::Updates>, ApiError> {
    let is_general = topic_id == FORUM_GENERAL_TOPIC_ID;
    let req = tl_fns::messages::EditForumTopic {
        peer: input_peer_for(session, peer_id),
        topic_id: topic_id.as_i32().unwrap_or(0),
        title: edit.title,
        icon_emoji_id: if is_general { None } else { edit.icon_emoji_id },
        closed: edit.closed,
        hidden: edit.hidden,
    };
    match rpc.invoke(&req).await {
        Ok(updates) => Ok(Some(updates)),
        Err(err) => {
            let inv: InvocationError = err.into();
            if let InvocationError::Rpc(ref rpc_err) = inv
                && rpc_err.name == "TOPIC_NOT_MODIFIED"
            {
                return Ok(None);
            }
            Err(ApiError::Invocation(inv))
        }
    }
}

/// Pins or unpins a forum topic via `messages.updatePinnedForumTopic`
/// (`window_peer_menu.cpp:474-483`).
pub async fn update_pinned_forum_topic<C: RpcClient>(
    rpc: &C,
    session: &Session,
    peer_id: PeerId,
    topic_id: MsgId,
    pinned: bool,
) -> Result<tl_enums::Updates, ApiError> {
    let req = tl_fns::messages::UpdatePinnedForumTopic {
        peer: input_peer_for(session, peer_id),
        topic_id: topic_id.as_i32().unwrap_or(0),
        pinned,
    };
    let updates = rpc.invoke(&req).await.map_err(Into::into)?;
    Ok(updates)
}

/// Reorders pinned forum topics via `messages.reorderPinnedForumTopics` with `force = true`
/// (`ApiWrap::savePinnedOrder(Data::Forum*)` in `apiwrap.cpp:473-494`).
pub async fn reorder_pinned_forum_topics<C: RpcClient>(
    rpc: &C,
    session: &Session,
    peer_id: PeerId,
    order: &[MsgId],
) -> Result<tl_enums::Updates, ApiError> {
    let req = tl_fns::messages::ReorderPinnedForumTopics {
        force: true,
        peer: input_peer_for(session, peer_id),
        order: order.iter().filter_map(|id| id.as_i32()).collect(),
    };
    let updates = rpc.invoke(&req).await.map_err(Into::into)?;
    Ok(updates)
}

/// Deletes a forum topic and all its messages via `messages.deleteTopicHistory`
/// (`PeerMenuDeleteTopic` in `window/window_peer_menu.cpp:2325-2341`).
///
/// Loops while `messages.affectedHistory.offset > 0`, feeds each page's `(pts, pts_count)`
/// into `updates_handler` (`applyAffectedHistory`), and removes the topic from `session` once
/// `offset == 0`.
pub async fn delete_topic_history<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    mut updates_handler: Option<&mut UpdatesHandler>,
    peer_id: PeerId,
    topic_id: MsgId,
    now_ms: u64,
) -> Result<Vec<tl_types::messages::AffectedHistory>, ApiError> {
    let peer = input_peer_for(session, peer_id);
    let channel_id = peer_id.to_channel();
    let mut pages = Vec::new();

    for _ in 0..MAX_DELETE_TOPIC_HISTORY_LOOPS {
        let tl_enums::messages::AffectedHistory::AffectedHistory(aff) = rpc
            .invoke(&tl_fns::messages::DeleteTopicHistory {
                peer: peer.clone(),
                top_msg_id: topic_id.as_i32().unwrap_or(0),
            })
            .await
            .map_err(Into::into)?;

        if let Some(handler) = updates_handler.as_deref_mut() {
            handler
                .apply_pts_only(rpc, session, channel_id, aff.pts, aff.pts_count, now_ms)
                .await?;
        }
        let next_offset = aff.offset;
        pages.push(aff);
        if next_offset <= 0 {
            break;
        }
    }

    session.delete_forum_topic(peer_id, topic_id, false);
    Ok(pages)
}

/// Marks a forum topic / discussion thread read up to `read_max_id` via
/// `messages.readDiscussion` (`RepliesList::sendReadTillRequest` in
/// `data/data_replies_list.cpp:967-982`).
pub async fn read_topic_discussion<C: RpcClient>(
    rpc: &C,
    session: &mut Session,
    peer_id: PeerId,
    topic_id: MsgId,
    read_max_id: MsgId,
) -> Result<bool, ApiError> {
    let req = tl_fns::messages::ReadDiscussion {
        peer: input_peer_for(session, peer_id),
        msg_id: topic_id.as_i32().unwrap_or(0),
        read_max_id: read_max_id.as_i32().unwrap_or(0),
    };
    let ok = rpc.invoke(&req).await.map_err(Into::into)?;
    if ok {
        session.mark_topic_inbox_read(peer_id, topic_id, read_max_id, None);
    }
    Ok(ok)
}
