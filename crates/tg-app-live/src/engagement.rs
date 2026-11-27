//! WP-53 Chat Engagement & Read-Receipt Protocol live implementation for [`LiveSession`].
//!
//! Port origin:
//! - `Telegram/SourceFiles/api/api_views.{h,cpp}` (`ViewsManager`: debounced
//!   `messages.getMessagesViews(increment=true)` and `messages.getExtendedMedia`)
//! - `Telegram/SourceFiles/api/api_unread_things.{h,cpp}` & `history/history_unread_things.cpp`
//!   (`messages.getUnread{Mentions,Reactions,PollVotes}`)
//! - `Telegram/SourceFiles/menu/menu_send.cpp`
//!   (`messages.read{Mentions,Reactions,PollVotes}` with `AffectedHistory` pagination)
//! - `Telegram/SourceFiles/api/api_who_reacted.{h,cpp}`
//!   (`messages.getMessageReadParticipants`, `messages.getOutboxReadDate`)
//! - `Telegram/SourceFiles/data/data_replies_list.cpp` (`messages.readDiscussion`)
//! - `Telegram/SourceFiles/data/data_histories.cpp` (`messages.reportMessagesDelivery`)
//! - `Telegram/SourceFiles/data/components/ephemeral_messages.{h,cpp}` (`ephemeral.deleteMessage`)
//! - `Telegram/SourceFiles/api/api_report.cpp` (`ephemeral.reportMessage`)
//! - `Telegram/SourceFiles/apiwrap.cpp` (`channels.exportMessageLink`, `messages.migrateChat`)
//! - `Telegram/SourceFiles/history/view/history_view_context_menu.cpp` (`channels.getMessageAuthor`)
//! - `Telegram/SourceFiles/window/window_peer_menu.cpp` (`messages.getFutureChatCreatorAfterLeave`)

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use tg_api::RpcClient;
use tg_api::convert::{
    apply_tl_users_and_chats, input_user_for, message_from_tl, message_media_from_tl,
    peer_id_from_tl,
};
use tg_api::rpc::InvocationError;
use tg_data::{
    ChannelFlags, ChannelId, ChatFlags, ExtendedMedia, FullMsgId as DataFullMsgId, MessageContent,
    MessageFlags, MessageMedia, MessageUpdateFlags, MsgId as DataMsgId, Peer, PeerId as DataPeerId,
    Session, UserId as DataUserId,
};
use tg_tl::{RemoteCall, enums as tl_enums, functions as tl_fns, types as tl_types};
use tokio::sync::{RwLock, broadcast};
use tracing::{debug, warn};

use crate::convert::{
    from_data_msg, from_data_peer, message_to_history_message, peer_display_title, to_data_msg,
    to_data_peer,
};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::history::backend::{
    ChatMigrationResult, EphemeralReportOption, EphemeralReportOutcome, ExportedMessageLinkInfo,
    ExtendedMediaRefreshEntry, FutureChatCreatorInfo, MAX_EXTENDED_MEDIA_POLL_PER_REQUEST,
    MessageAuthorInfo, MessageReadParticipant, MessageViewsInfo, OutboxReadDateStatus,
    POLL_EXTENDED_MEDIA_PERIOD_MS, REPORT_DELIVERIES_PER_REQUEST, UnreadThingsCounts,
    UnreadThingsKind, UnreadThingsReadResult, UnreadThingsScope, UnreadThingsSlice,
    VIEWS_SEND_TIMEOUT_MS, unread_things_pagination_params,
};
use tg_app_core::stand_in::types::{MsgId, PeerId};

/// Default timeout for synchronous engagement RPC invocations.
const ENGAGEMENT_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Safety cap for `messages.AffectedHistory` pagination loops
/// (`readMentions`, `readReactions`, `readPollVotes`).
const MAX_AFFECTED_HISTORY_LOOPS: usize = 64;

/// Milliseconds on a clock that never goes back (`crl::now()`): the debounce deadlines below
/// must not move when the wall clock is changed.
fn monotonic_now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    u64::try_from(START.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Per-peer pending `messages.getExtendedMedia` batch (`ViewsManager::PollExtendedMediaRequest`).
#[derive(Debug, Default)]
struct PendingExtendedMediaPoll {
    /// When the batch is due; `None` until it is scheduled.
    when_ms: Option<u64>,
    ids: BTreeSet<MsgId>,
    forced: bool,
}

/// Per-scope loaded unread items state (`HistoryUnreadThings::List`).
#[derive(Debug, Default)]
struct UnreadThingsListState {
    count: Option<i32>,
    loaded_ids: BTreeSet<MsgId>,
}

/// Tracked incoming/outgoing ephemeral message metadata (`Data::EphemeralMessages::Entry`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedEphemeralEntry {
    /// Owning peer ID.
    pub peer_id: PeerId,
    /// Server ephemeral message ID.
    pub ephemeral_id: i32,
    /// Receiver user ID (bot or target user).
    pub receiver_user_id: PeerId,
    /// Sender peer ID.
    pub from_id: PeerId,
    /// Text body.
    pub text: String,
    /// Optional anchor regular message ID.
    pub anchor_msg_id: Option<MsgId>,
    /// Unix creation timestamp.
    pub date_unix: i32,
    /// True if outgoing.
    pub outgoing: bool,
}

#[derive(Default)]
struct LiveEngagementState {
    updates_listener_started: bool,
    // ViewsManager (`api_views.cpp`)
    incremented_views: HashMap<PeerId, HashSet<MsgId>>,
    to_increment_views: HashMap<PeerId, BTreeSet<MsgId>>,
    views_due_at_ms: HashMap<PeerId, u64>,
    extended_media_polls: HashMap<PeerId, PendingExtendedMediaPoll>,
    // UnreadThings (`api_unread_things.cpp` / `history_unread_things.cpp`)
    unread_mentions: HashMap<UnreadThingsScope, UnreadThingsListState>,
    unread_reactions: HashMap<UnreadThingsScope, UnreadThingsListState>,
    unread_poll_votes: HashMap<UnreadThingsScope, UnreadThingsListState>,
    // Pending delivery reports (`data_histories.cpp`)
    pending_delivery_reports: HashMap<PeerId, BTreeSet<MsgId>>,
    // Tracked ephemeral messages (`data/components/ephemeral_messages.cpp`)
    ephemeral_entries: HashMap<(PeerId, i32), TrackedEphemeralEntry>,
    // Cached exported message links (`apiwrap.cpp` `_unlikelyMessageLinks`)
    exported_links: HashMap<(PeerId, MsgId, bool, bool), ExportedMessageLinkInfo>,
    // Cached channel message authors (`history_view_context_menu.cpp` `LookupMessageAuthor`)
    message_authors: HashMap<(PeerId, MsgId), MessageAuthorInfo>,
}

struct RegistryEntry {
    session_weak: Weak<RwLock<Session>>,
    state: Arc<Mutex<LiveEngagementState>>,
}

fn registry() -> &'static Mutex<HashMap<usize, RegistryEntry>> {
    static REG: OnceLock<Mutex<HashMap<usize, RegistryEntry>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn state_for_session_arc(session: &Arc<RwLock<Session>>) -> Arc<Mutex<LiveEngagementState>> {
    let key = Arc::as_ptr(session) as usize;
    let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    reg.retain(|_, entry| entry.session_weak.strong_count() > 0);
    if let Some(existing) = reg.get(&key)
        && existing.session_weak.ptr_eq(&Arc::downgrade(session))
    {
        return Arc::clone(&existing.state);
    }
    let state = Arc::new(Mutex::new(LiveEngagementState::default()));
    reg.insert(
        key,
        RegistryEntry {
            session_weak: Arc::downgrade(session),
            state: Arc::clone(&state),
        },
    );
    state
}

fn state_for_live(live: &LiveSession) -> Arc<Mutex<LiveEngagementState>> {
    let session = live.runtime().session();
    let state = state_for_session_arc(session);
    ensure_updates_listener(live, &state, Arc::downgrade(session));
    state
}

/// Test hook: gives every scope in `scopes` a cached unread-reactions count, as a loaded
/// `messages.getUnreadReactions` page would.
#[cfg(test)]
pub(crate) fn seed_unread_reaction_scopes(live: &LiveSession, scopes: &[UnreadThingsScope]) {
    let state = state_for_live(live);
    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
    for scope in scopes {
        guard.unread_reactions.entry(*scope).or_default().count = Some(1);
    }
}

/// Attaches the background `subscribe_updates()` listener for WP-53 engagement updates
/// (`updateMessageExtendedMedia`, `updateNewEphemeralMessage`, `updateEditEphemeralMessage`,
/// `updateDeleteEphemeralMessages`).
pub(crate) fn attach_engagement_listener(live: &LiveSession) {
    let _ = state_for_live(live);
}

fn ensure_updates_listener(
    live: &LiveSession,
    state: &Arc<Mutex<LiveEngagementState>>,
    session_weak: Weak<RwLock<Session>>,
) {
    {
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        if guard.updates_listener_started {
            return;
        }
        guard.updates_listener_started = true;
    }

    let Some(mut rx) = live.runtime().rpc().subscribe_updates() else {
        return;
    };
    let state_clone = Arc::clone(state);
    let bridge_state = Arc::clone(live.bridge_state());
    live.runtime().tokio_handle().spawn(async move {
        loop {
            match rx.recv().await {
                Ok(updates) => {
                    let Some(session_arc) = session_weak.upgrade() else {
                        break;
                    };
                    let changed = {
                        let mut session = session_arc.write().await;
                        let mut eng = state_clone.lock().unwrap_or_else(|e| e.into_inner());
                        apply_engagement_updates(&mut session, &mut eng, &updates)
                    };
                    if changed {
                        bridge_state.notify_ui();
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

fn invoke_rpc_raw<R>(live: &LiveSession, req: R) -> Result<R::Return, InvocationError>
where
    R: RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
{
    let rpc = live.runtime().rpc().clone();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        tokio::time::timeout(ENGAGEMENT_RPC_TIMEOUT, rpc.invoke(&req))
            .await
            .map_err(|_| InvocationError::Dropped)?
    })
}

fn invoke_rpc<R>(live: &LiveSession, req: R) -> Result<R::Return, String>
where
    R: RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
{
    let rpc = live.runtime().rpc().clone();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(
            ENGAGEMENT_RPC_TIMEOUT,
            async move { rpc.invoke(&req).await },
        )
        .await
    })
}

fn input_user(live: &LiveSession, user_peer_id: PeerId) -> Option<tl_enums::InputUser> {
    let data_peer = to_data_peer(user_peer_id);
    let uid = data_peer.to_user()?;
    let user = live.read_session(|s| input_user_for(s, uid));
    if matches!(user, tl_enums::InputUser::Empty) {
        None
    } else {
        Some(user)
    }
}

fn extended_media_from_tl(em: &tl_enums::MessageExtendedMedia) -> ExtendedMedia {
    match em {
        tl_enums::MessageExtendedMedia::Preview(p) => {
            let thumb = p.thumb.as_ref().and_then(|sz| match sz {
                tl_enums::PhotoSize::PhotoStrippedSize(s) => Some(s.bytes.clone()),
                tl_enums::PhotoSize::PhotoCachedSize(s) => Some(s.bytes.clone()),
                _ => None,
            });
            ExtendedMedia::Preview {
                width: p.w,
                height: p.h,
                thumb,
                duration: p.video_duration,
            }
        }
        tl_enums::MessageExtendedMedia::MessageExtendedMedia(m) => {
            ExtendedMedia::Media(Box::new(message_media_from_tl(&m.media)))
        }
    }
}

fn apply_extended_media_update(
    session: &mut Session,
    u: &tl_types::UpdateMessageExtendedMedia,
) -> ExtendedMediaRefreshEntry {
    let data_peer = peer_id_from_tl(&u.peer);
    let data_msg = DataMsgId(i64::from(u.msg_id));
    let full_id = DataFullMsgId::new(data_peer, data_msg);
    let converted: Vec<ExtendedMedia> = u
        .extended_media
        .iter()
        .map(extended_media_from_tl)
        .collect();
    let total_items = converted.len();
    let unlocked_items = converted
        .iter()
        .filter(|m| matches!(m, ExtendedMedia::Media(_)))
        .count();

    if let Some(msg) = session.message_mut(full_id) {
        let mut applied = false;
        if let MessageContent::Regular { media, .. } = &mut msg.content {
            match media {
                MessageMedia::PaidMedia(paid) => {
                    paid.extended_media = converted;
                    applied = true;
                }
                MessageMedia::Invoice(inv) => {
                    inv.extended_media = converted.into_iter().next().map(Box::new);
                    applied = true;
                }
                _ => {}
            }
        }
        if applied {
            session
                .changes()
                .message_updated(full_id, MessageUpdateFlags::EDITED);
        }
    }

    ExtendedMediaRefreshEntry {
        peer_id: from_data_peer(data_peer),
        msg_id: MsgId(u.msg_id),
        total_items,
        unlocked_items,
    }
}

fn ephemeral_peer_id_from_tl(data: &tl_types::EphemeralMessage) -> Option<PeerId> {
    if let Some(ref peer) = data.peer_id {
        let pid = peer_id_from_tl(peer);
        if !pid.is_user() {
            return Some(from_data_peer(pid));
        }
    }
    if data.out {
        Some(from_data_peer(DataPeerId::from_user(DataUserId::from_tl(
            data.receiver_id,
        ))))
    } else {
        Some(from_data_peer(peer_id_from_tl(&data.from_id)))
    }
}

fn upsert_ephemeral_from_tl(
    state: &mut LiveEngagementState,
    msg: &tl_enums::EphemeralMessage,
) -> bool {
    let tl_enums::EphemeralMessage::EphemeralMessage(data) = msg;
    if data.welcome_template {
        return false;
    }
    let Some(peer_id) = ephemeral_peer_id_from_tl(data) else {
        return false;
    };
    let entry = TrackedEphemeralEntry {
        peer_id,
        ephemeral_id: data.id,
        receiver_user_id: from_data_peer(DataPeerId::from_user(DataUserId::from_tl(
            data.receiver_id,
        ))),
        from_id: from_data_peer(peer_id_from_tl(&data.from_id)),
        text: data.message.clone(),
        anchor_msg_id: data.anchor_msg_id.map(MsgId),
        date_unix: data.date,
        outgoing: data.out,
    };
    state.ephemeral_entries.insert((peer_id, data.id), entry);
    true
}

fn apply_engagement_updates(
    session: &mut Session,
    state: &mut LiveEngagementState,
    updates: &tl_enums::Updates,
) -> bool {
    let mut changed = false;
    let slice: &[tl_enums::Update] = match updates {
        tl_enums::Updates::Updates(u) => &u.updates,
        tl_enums::Updates::Combined(c) => &c.updates,
        tl_enums::Updates::UpdateShort(s) => std::slice::from_ref(&s.update),
        _ => &[],
    };

    for upd in slice {
        match upd {
            tl_enums::Update::MessageExtendedMedia(u) => {
                let _ = apply_extended_media_update(session, u);
                changed = true;
            }
            tl_enums::Update::NewEphemeralMessage(u) => {
                if upsert_ephemeral_from_tl(state, &u.message) {
                    changed = true;
                }
            }
            tl_enums::Update::EditEphemeralMessage(u) => {
                if upsert_ephemeral_from_tl(state, &u.message) {
                    changed = true;
                }
            }
            tl_enums::Update::DeleteEphemeralMessages(u) => {
                let peer_id = from_data_peer(peer_id_from_tl(&u.peer));
                for &id in &u.ids {
                    if state.ephemeral_entries.remove(&(peer_id, id)).is_some() {
                        changed = true;
                    }
                }
            }
            _ => {}
        }
    }
    changed
}

// =============================================================================
// 1. ViewsManager (`api/api_views.{h,cpp}`)
// =============================================================================

/// Enqueues `msg_ids` for debounced `messages.getMessagesViews(increment = true)`
/// (`ViewsManager::scheduleIncrement` in `api/api_views.cpp:35-52`).
pub(crate) fn schedule_message_views_increment(
    live: &LiveSession,
    peer_id: PeerId,
    msg_ids: &[MsgId],
) {
    if msg_ids.is_empty() {
        return;
    }
    let now = monotonic_now_ms();
    let state = state_for_live(live);
    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
    let mut newly_queued = Vec::new();
    {
        let incremented = guard.incremented_views.entry(peer_id).or_default();
        for &id in msg_ids {
            if id.0 > 0 && incremented.insert(id) {
                newly_queued.push(id);
            }
        }
    }
    if newly_queued.is_empty() {
        return;
    }
    let is_first_for_peer = !guard.to_increment_views.contains_key(&peer_id);
    let pending = guard.to_increment_views.entry(peer_id).or_default();
    for id in newly_queued {
        pending.insert(id);
    }
    if is_first_for_peer {
        guard
            .views_due_at_ms
            .insert(peer_id, now.saturating_add(VIEWS_SEND_TIMEOUT_MS));
    }
}

/// Clears the per-peer incremented views deduplication cache
/// (`ViewsManager::removeIncremented` in `api/api_views.cpp:54-56`).
pub(crate) fn remove_incremented_views(live: &LiveSession, peer_id: PeerId) {
    let state = state_for_live(live);
    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
    guard.incremented_views.remove(&peer_id);
}

/// Flushes due (or all when `force == true`) queued view-increment batches via
/// `messages.getMessagesViews(increment = true)` (`ViewsManager::viewsIncrement`).
pub(crate) fn flush_message_views(
    live: &LiveSession,
    peer_filter: Option<PeerId>,
    force: bool,
) -> Result<Vec<MessageViewsInfo>, String> {
    let now = monotonic_now_ms();
    let state = state_for_live(live);
    let batches: Vec<(PeerId, Vec<MsgId>)> = {
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        let due_peers: Vec<PeerId> = guard
            .to_increment_views
            .keys()
            .copied()
            .filter(|pid| peer_filter.is_none_or(|target| target == *pid))
            .filter(|pid| force || guard.views_due_at_ms.get(pid).is_none_or(|&due| due <= now))
            .collect();
        let mut out = Vec::with_capacity(due_peers.len());
        for pid in due_peers {
            guard.views_due_at_ms.remove(&pid);
            if let Some(ids) = guard.to_increment_views.remove(&pid)
                && !ids.is_empty()
            {
                out.push((pid, ids.into_iter().collect()));
            }
        }
        out
    };

    let mut all_infos = Vec::new();
    for (peer_id, ids) in batches {
        let mut infos = get_messages_views(live, peer_id, &ids, true)?;
        all_infos.append(&mut infos);
    }
    Ok(all_infos)
}

/// Invokes `messages.getMessagesViews` directly and applies returned view, forward, and
/// reply counts into `tg_data::Session` (`ViewsManager::done` in `api/api_views.cpp:199-238`).
pub(crate) fn get_messages_views(
    live: &LiveSession,
    peer_id: PeerId,
    msg_ids: &[MsgId],
    increment: bool,
) -> Result<Vec<MessageViewsInfo>, String> {
    if msg_ids.is_empty() {
        return Ok(Vec::new());
    }
    let Some(peer) = live.input_peer(peer_id) else {
        return Err("Peer not found in session for messages.getMessagesViews.".to_owned());
    };
    let raw_ids: Vec<i32> = msg_ids.iter().map(|m| m.0).collect();
    let resp = invoke_rpc(
        live,
        tl_fns::messages::GetMessagesViews {
            peer,
            id: raw_ids,
            increment,
        },
    )?;

    let tl_enums::messages::MessageViews::MessageViews(data) = resp;
    let data_peer = to_data_peer(peer_id);
    let mut infos = Vec::new();

    live.write_session(|session| {
        apply_tl_users_and_chats(session, &data.users, &data.chats);
        // Exact tdesktop length guard (`api/api_views.cpp:208`).
        if msg_ids.len() != data.views.len() {
            warn!(
                requested = msg_ids.len(),
                received = data.views.len(),
                "messages.getMessagesViews returned mismatched views vector length"
            );
            return;
        }
        for (&msg_id, mv) in msg_ids.iter().zip(data.views.iter()) {
            let tl_enums::MessageViews::MessageViews(v) = mv;
            let full_id = DataFullMsgId::new(data_peer, to_data_msg(msg_id));
            if let Some(views) = v.views {
                session.update_message_views(full_id, views);
            }
            if let Some(forwards) = v.forwards {
                session.update_message_forwards(full_id, forwards);
            }
            let (replies_count, comments, discussion_channel_id, max_id, read_max_id) =
                match v.replies.as_ref() {
                    Some(tl_enums::MessageReplies::MessageReplies(r)) => {
                        if let Some(msg) = session.message_mut(full_id)
                            && msg.replies_count != Some(r.replies)
                        {
                            msg.replies_count = Some(r.replies);
                            session
                                .changes()
                                .message_updated(full_id, MessageUpdateFlags::EDITED);
                        }
                        (
                            Some(r.replies),
                            r.comments,
                            r.channel_id,
                            r.max_id.map(MsgId),
                            r.read_max_id.map(MsgId),
                        )
                    }
                    None => (None, false, None, None, None),
                };
            infos.push(MessageViewsInfo {
                msg_id,
                views: v.views,
                forwards: v.forwards,
                replies_count,
                comments,
                discussion_channel_id,
                max_id,
                read_max_id,
            });
        }
    });

    if !infos.is_empty() {
        live.bridge_state().notify_ui();
    }
    Ok(infos)
}

/// Queues `msg_ids` for `messages.getExtendedMedia` polling (`ViewsManager::pollExtendedMedia`
/// in `api/api_views.cpp:58-83`). When `force == true`, flushes immediately in chunks of at
/// most `MAX_EXTENDED_MEDIA_POLL_PER_REQUEST` (100).
pub(crate) fn poll_extended_media(
    live: &LiveSession,
    peer_id: PeerId,
    msg_ids: &[MsgId],
    force: bool,
) -> Result<Vec<ExtendedMediaRefreshEntry>, String> {
    if msg_ids.is_empty() {
        return Ok(Vec::new());
    }
    let now = monotonic_now_ms();
    {
        let state = state_for_live(live);
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        let req = guard.extended_media_polls.entry(peer_id).or_default();
        for &id in msg_ids {
            if id.0 > 0 {
                req.ids.insert(id);
            }
        }
        if force {
            req.forced = true;
            req.when_ms = Some(now);
        } else if req.when_ms.is_none() {
            req.when_ms = Some(now.saturating_add(POLL_EXTENDED_MEDIA_PERIOD_MS));
        }
    }
    if force {
        flush_extended_media_polls(live, true)
    } else {
        Ok(Vec::new())
    }
}

/// Flushes due (or all when `force == true`) queued `messages.getExtendedMedia` polls,
/// chunked to at most `MAX_EXTENDED_MEDIA_POLL_PER_REQUEST` (100) IDs per RPC
/// (`ViewsManager::sendPollRequests` in `api/api_views.cpp:114-197`).
pub(crate) fn flush_extended_media_polls(
    live: &LiveSession,
    force: bool,
) -> Result<Vec<ExtendedMediaRefreshEntry>, String> {
    let now = monotonic_now_ms();
    let state = state_for_live(live);
    let mut batches: Vec<(PeerId, Vec<MsgId>)> = Vec::new();
    {
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        let peers: Vec<PeerId> = guard
            .extended_media_polls
            .iter()
            .filter(|(_, req)| {
                !req.ids.is_empty()
                    && (force || req.forced || req.when_ms.is_none_or(|due| due <= now))
            })
            .map(|(&pid, _)| pid)
            .collect();
        for pid in peers {
            if let Some(req) = guard.extended_media_polls.get_mut(&pid) {
                while !req.ids.is_empty() {
                    let take_count = req.ids.len().min(MAX_EXTENDED_MEDIA_POLL_PER_REQUEST);
                    let chunk: Vec<MsgId> = req.ids.iter().copied().take(take_count).collect();
                    for id in &chunk {
                        req.ids.remove(id);
                    }
                    batches.push((pid, chunk));
                    if !force && !req.forced {
                        // In non-forced timer mode, tdesktop sends one batch per timer tick
                        // and reschedules remaining items (`api_views.cpp:125-139`).
                        break;
                    }
                }
                if req.ids.is_empty() {
                    guard.extended_media_polls.remove(&pid);
                } else {
                    req.when_ms = Some(now.saturating_add(if req.forced {
                        1
                    } else {
                        POLL_EXTENDED_MEDIA_PERIOD_MS
                    }));
                }
            }
        }
    }

    let mut refreshed = Vec::new();
    for (peer_id, chunk) in batches {
        let Some(peer) = live.input_peer(peer_id) else {
            return Err("Peer not found in session for messages.getExtendedMedia.".to_owned());
        };
        let updates = invoke_rpc(
            live,
            tl_fns::messages::GetExtendedMedia {
                peer,
                id: chunk.iter().map(|m| m.0).collect(),
            },
        )?;
        let entries = apply_extended_media_updates_live(live, &updates);
        refreshed.extend(entries);
    }

    if !refreshed.is_empty() {
        live.bridge_state().notify_ui();
    }
    Ok(refreshed)
}

fn apply_extended_media_updates_live(
    live: &LiveSession,
    updates: &tl_enums::Updates,
) -> Vec<ExtendedMediaRefreshEntry> {
    let _ = live.apply_updates_to_session(updates);
    let mut entries = Vec::new();
    live.write_session(|session| {
        let slice: &[tl_enums::Update] = match updates {
            tl_enums::Updates::Updates(u) => &u.updates,
            tl_enums::Updates::Combined(c) => &c.updates,
            tl_enums::Updates::UpdateShort(s) => std::slice::from_ref(&s.update),
            _ => &[],
        };
        for upd in slice {
            if let tl_enums::Update::MessageExtendedMedia(u) = upd {
                entries.push(apply_extended_media_update(session, u));
            }
        }
    });
    entries
}

// =============================================================================
// 2. Unread Mentions, Reactions & Poll Votes (`api/api_unread_things.cpp`)
// =============================================================================

/// Returns tdesktop's tracking eligibility for `kind` on `peer_id`
/// (`UnreadThings::track{Mentions,Reactions,PollVotes}` in `api_unread_things.cpp:33-50`).
#[must_use]
pub fn tracks_unread_things(live: &LiveSession, peer_id: PeerId, kind: UnreadThingsKind) -> bool {
    let data_peer = to_data_peer(peer_id);
    live.read_session(|session| {
        let Some(peer) = session.peer(data_peer) else {
            return false;
        };
        match kind {
            UnreadThingsKind::Mentions | UnreadThingsKind::PollVotes => match peer {
                Peer::Chat(_) => true,
                Peer::Channel(ch) => {
                    ch.flags.contains(ChannelFlags::MEGAGROUP)
                        && !ch.flags.contains(ChannelFlags::MONOFORUM)
                }
                Peer::User(_) => false,
            },
            UnreadThingsKind::Reactions => match peer {
                Peer::User(_) | Peer::Chat(_) => true,
                Peer::Channel(ch) => ch.flags.contains(ChannelFlags::MEGAGROUP),
            },
        }
    })
}

fn unread_map_mut(
    state: &mut LiveEngagementState,
    kind: UnreadThingsKind,
) -> &mut HashMap<UnreadThingsScope, UnreadThingsListState> {
    match kind {
        UnreadThingsKind::Mentions => &mut state.unread_mentions,
        UnreadThingsKind::Reactions => &mut state.unread_reactions,
        UnreadThingsKind::PollVotes => &mut state.unread_poll_votes,
    }
}

fn unread_map(
    state: &LiveEngagementState,
    kind: UnreadThingsKind,
) -> &HashMap<UnreadThingsScope, UnreadThingsListState> {
    match kind {
        UnreadThingsKind::Mentions => &state.unread_mentions,
        UnreadThingsKind::Reactions => &state.unread_reactions,
        UnreadThingsKind::PollVotes => &state.unread_poll_votes,
    }
}

/// Paginates `messages.getUnread{Mentions,Reactions,PollVotes}` using tdesktop's exact
/// `(offset_id, add_offset, limit)` rules (`api_unread_things.cpp:118-221`) and applies
/// the slice into both `tg_data::Session` and local unread tracking (`history_unread_things.cpp:157-237`).
pub(crate) fn load_unread_things_page(
    live: &LiveSession,
    scope: UnreadThingsScope,
    kind: UnreadThingsKind,
) -> Result<UnreadThingsSlice, String> {
    if scope.saved_peer_id.is_some()
        && matches!(
            kind,
            UnreadThingsKind::Mentions | UnreadThingsKind::PollVotes
        )
    {
        // tdesktop `requestMentions` / `requestPollVotes` reject saved sublists (`api_unread_things.cpp:121,192`).
        return Ok(UnreadThingsSlice::empty(kind));
    }

    let Some(peer) = live.input_peer(scope.peer_id) else {
        return Err("Peer not found in session for messages.getUnread*.".to_owned());
    };
    let saved_peer = match scope.saved_peer_id {
        Some(sp) => Some(
            live.input_peer(sp)
                .ok_or_else(|| "Saved sublist peer not found in session.".to_owned())?,
        ),
        None => None,
    };

    let state = state_for_live(live);
    let (already_loaded, max_loaded) = {
        let guard = state.lock().unwrap_or_else(|e| e.into_inner());
        match unread_map(&guard, kind).get(&scope) {
            Some(list) => (
                list.loaded_ids.len(),
                list.loaded_ids.iter().next_back().copied(),
            ),
            None => (0, None),
        }
    };
    let (offset_id, add_offset, limit) =
        unread_things_pagination_params(already_loaded, max_loaded);
    let top_msg_id = scope.top_msg_id.map(|m| m.0);

    let resp = match kind {
        UnreadThingsKind::Mentions => invoke_rpc(
            live,
            tl_fns::messages::GetUnreadMentions {
                peer,
                top_msg_id,
                offset_id,
                add_offset,
                limit,
                max_id: 0,
                min_id: 0,
            },
        )?,
        UnreadThingsKind::Reactions => invoke_rpc(
            live,
            tl_fns::messages::GetUnreadReactions {
                peer,
                top_msg_id,
                saved_peer_id: saved_peer,
                offset_id,
                add_offset,
                limit,
                max_id: 0,
                min_id: 0,
            },
        )?,
        UnreadThingsKind::PollVotes => invoke_rpc(
            live,
            tl_fns::messages::GetUnreadPollVotes {
                peer,
                top_msg_id,
                offset_id,
                add_offset,
                limit,
                max_id: 0,
                min_id: 0,
            },
        )?,
    };

    let (mut server_full_count, raw_messages, chats, users, channel_pts) = match resp {
        tl_enums::messages::Messages::NotModified(_) => {
            warn!("Received messages.messagesNotModified in load_unread_things_page");
            (0, Vec::new(), Vec::new(), Vec::new(), None)
        }
        tl_enums::messages::Messages::Messages(m) => {
            (m.messages.len() as i32, m.messages, m.chats, m.users, None)
        }
        tl_enums::messages::Messages::Slice(s) => (s.count, s.messages, s.chats, s.users, None),
        tl_enums::messages::Messages::ChannelMessages(c) => (
            c.count,
            c.messages,
            c.chats,
            c.users,
            (c.pts > 0).then_some(c.pts),
        ),
    };
    if let Some(pts) = channel_pts
        && let Some(ch_id) = to_data_peer(scope.peer_id).to_channel()
    {
        let _ = live.apply_channel_pts(ch_id, pts, 0);
    }

    let mut page_messages = Vec::with_capacity(raw_messages.len());
    let mut matched_ids = Vec::new();
    let data_peer = to_data_peer(scope.peer_id);

    live.write_session(|session| {
        apply_tl_users_and_chats(session, &users, &chats);
        let self_uid = session.self_user_id();
        for raw in &raw_messages {
            let Some(dm) = message_from_tl(raw, self_uid) else {
                continue;
            };
            let msg_id = from_data_msg(dm.id);
            let matches_kind = match kind {
                UnreadThingsKind::Mentions => {
                    dm.flags.contains(MessageFlags::MENTIONS_ME)
                        && dm.flags.contains(MessageFlags::MEDIA_IS_UNREAD)
                }
                UnreadThingsKind::Reactions => {
                    dm.has_unread_reaction()
                        || dm
                            .reactions
                            .as_ref()
                            .is_some_and(|r| r.recent_reactors.iter().any(|rr| rr.unread))
                }
                // tdesktop marks every message returned by `getUnreadPollVotes` as having
                // an unread poll vote (`history_unread_things.cpp:211-213`).
                UnreadThingsKind::PollVotes => true,
            };
            let hm = message_to_history_message(session, &dm);
            session.insert_existing_message(dm);
            if matches_kind {
                matched_ids.push(msg_id);
            }
            page_messages.push(hm);
        }
    });

    let (full_count, loaded_ids) = {
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        let entry = unread_map_mut(&mut guard, kind).entry(scope).or_default();
        if already_loaded == 0 {
            entry.loaded_ids.clear();
        }
        let mut added = false;
        for &id in &matched_ids {
            entry.loaded_ids.insert(id);
            added = true;
        }
        if !added {
            server_full_count = entry.loaded_ids.len() as i32;
        }
        let clamped_count = server_full_count.max(entry.loaded_ids.len() as i32);
        entry.count = Some(clamped_count);
        let ids: Vec<MsgId> = entry.loaded_ids.iter().copied().collect();
        (clamped_count, ids)
    };

    // Sync local counts back into `tg_data::Session`.
    live.write_session(|session| {
        sync_unread_count_to_session(session, data_peer, scope, kind, full_count, &loaded_ids);
    });
    live.bridge_state().notify_ui();

    let has_more = (loaded_ids.len() as i32) < full_count;
    Ok(UnreadThingsSlice {
        kind,
        full_count,
        loaded_ids,
        messages: page_messages,
        has_more,
    })
}

fn sync_unread_count_to_session(
    session: &mut Session,
    data_peer: DataPeerId,
    scope: UnreadThingsScope,
    kind: UnreadThingsKind,
    count: i32,
    loaded_ids: &[MsgId],
) {
    let clamped = count.max(0);
    if let Some(top_id) = scope.top_msg_id {
        if let Some(topic) = session
            .forum_mut(data_peer)
            .and_then(|f| f.topic_mut(to_data_msg(top_id)))
        {
            match kind {
                UnreadThingsKind::Mentions => topic.unread_mentions_count = clamped,
                UnreadThingsKind::Reactions => topic.unread_reactions_count = clamped,
                UnreadThingsKind::PollVotes => topic.unread_poll_votes_count = clamped,
            }
        }
        return;
    }
    if let Some(saved_peer) = scope.saved_peer_id {
        if matches!(kind, UnreadThingsKind::Reactions) {
            let saved_data = to_data_peer(saved_peer);
            if let Some(ch_id) = data_peer.to_channel() {
                session
                    .ensure_monoforum(ch_id)
                    .ensure_sublist(saved_data)
                    .unread_reactions_count = clamped;
            } else {
                session
                    .saved_messages_mut()
                    .ensure_sublist(saved_data)
                    .unread_reactions_count = clamped;
            }
        }
        return;
    }

    let history = session.ensure_history(data_peer);
    match kind {
        UnreadThingsKind::Mentions => {
            history.unread_mentions_count = clamped;
            history.unread_mentions.clear();
            for &id in loaded_ids {
                history.unread_mentions.insert(to_data_msg(id));
            }
        }
        UnreadThingsKind::Reactions => {
            history.unread_reactions_count = clamped;
            history.unread_reactions.clear();
            for &id in loaded_ids {
                history.unread_reactions.insert(to_data_msg(id));
            }
        }
        UnreadThingsKind::PollVotes => {
            history.unread_poll_votes_count = clamped;
        }
    }
}

/// Returns locally tracked unread mentions, reactions, and poll-votes counts for `scope`.
#[must_use]
pub(crate) fn unread_things_counts(
    live: &LiveSession,
    scope: UnreadThingsScope,
) -> UnreadThingsCounts {
    let data_peer = to_data_peer(scope.peer_id);
    let session_counts = live.read_session(|session| {
        if let Some(top_id) = scope.top_msg_id {
            if let Some(topic) = session
                .forum(data_peer)
                .and_then(|f| f.topic(to_data_msg(top_id)))
            {
                return UnreadThingsCounts {
                    mentions: topic.unread_mentions_count,
                    reactions: topic.unread_reactions_count,
                    poll_votes: topic.unread_poll_votes_count,
                };
            }
            return UnreadThingsCounts::default();
        }
        if let Some(saved_peer) = scope.saved_peer_id {
            let saved_data = to_data_peer(saved_peer);
            let reactions = if let Some(ch_id) = data_peer.to_channel() {
                session
                    .monoforum(ch_id)
                    .and_then(|m| m.sublist(saved_data))
                    .map_or(0, |s| s.unread_reactions_count)
            } else {
                session
                    .saved_messages()
                    .sublist(saved_data)
                    .map_or(0, |s| s.unread_reactions_count)
            };
            return UnreadThingsCounts {
                mentions: 0,
                reactions,
                poll_votes: 0,
            };
        }
        session
            .history(data_peer)
            .map_or_else(UnreadThingsCounts::default, |h| UnreadThingsCounts {
                mentions: h.unread_mentions_count,
                reactions: h.unread_reactions_count,
                poll_votes: h.unread_poll_votes_count,
            })
    });
    // The session's counters are the one count per thread (`thread->unreadMentions().count()`):
    // a loaded page and "mark all read" write theirs into them, and new messages add to them.
    // A count kept from the last page would hide a mention that arrives later (TGB-3161).
    session_counts
}

fn clear_local_unread_things(live: &LiveSession, scope: UnreadThingsScope, kind: UnreadThingsKind) {
    let cleared_ids: Vec<MsgId> = {
        let state = state_for_live(live);
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        let entry = unread_map_mut(&mut guard, kind).entry(scope).or_default();
        let ids: Vec<MsgId> = entry.loaded_ids.iter().copied().collect();
        entry.loaded_ids.clear();
        entry.count = Some(0);
        ids
    };

    let data_peer = to_data_peer(scope.peer_id);
    live.write_session(|session| {
        // Also clear per-message flags on any previously tracked messages.
        let mut ids_to_clear: Vec<DataMsgId> =
            cleared_ids.iter().map(|&id| to_data_msg(id)).collect();
        if scope.top_msg_id.is_none()
            && scope.saved_peer_id.is_none()
            && let Some(history) = session.history(data_peer)
        {
            match kind {
                UnreadThingsKind::Mentions => {
                    ids_to_clear.extend(history.unread_mentions.iter().copied());
                }
                UnreadThingsKind::Reactions => {
                    ids_to_clear.extend(history.unread_reactions.iter().copied());
                }
                UnreadThingsKind::PollVotes => {}
            }
        }
        for mid in ids_to_clear {
            let full_id = DataFullMsgId::new(data_peer, mid);
            if let Some(msg) = session.message_mut(full_id) {
                match kind {
                    UnreadThingsKind::Mentions => {
                        msg.flags.remove(MessageFlags::MEDIA_IS_UNREAD);
                    }
                    UnreadThingsKind::Reactions => {
                        msg.flags.remove(MessageFlags::HAS_UNREAD_REACTION);
                        if let Some(r) = msg.reactions.as_mut() {
                            for rr in &mut r.recent_reactors {
                                rr.unread = false;
                            }
                        }
                    }
                    UnreadThingsKind::PollVotes => {}
                }
            }
        }
        sync_unread_count_to_session(session, data_peer, scope, kind, 0, &[]);
    });
    live.bridge_state().notify_ui();
}

/// Marks all unread mentions read via `messages.readMentions`, looping while
/// `messages.AffectedHistory.offset > 0` (`menu/menu_send.cpp:983-997`).
pub(crate) fn read_unread_mentions(
    live: &LiveSession,
    peer_id: PeerId,
    top_msg_id: Option<MsgId>,
) -> Result<UnreadThingsReadResult, String> {
    let Some(peer) = live.input_peer(peer_id) else {
        return Err("Peer not found in session for messages.readMentions.".to_owned());
    };
    let mut result = UnreadThingsReadResult::default();
    for _ in 0..MAX_AFFECTED_HISTORY_LOOPS {
        let tl_enums::messages::AffectedHistory::AffectedHistory(aff) = invoke_rpc(
            live,
            tl_fns::messages::ReadMentions {
                peer: peer.clone(),
                top_msg_id: top_msg_id.map(|m| m.0),
            },
        )?;
        let _ = live.apply_affected_history(Some(peer_id), aff.pts, aff.pts_count);
        result.pages += 1;
        result.last_pts = aff.pts;
        result.total_pts_count += aff.pts_count;
        if aff.offset <= 0 {
            break;
        }
    }
    let scope = UnreadThingsScope {
        peer_id,
        top_msg_id,
        saved_peer_id: None,
    };
    clear_local_unread_things(live, scope, UnreadThingsKind::Mentions);
    Ok(result)
}

/// Marks all unread reactions read via `messages.readReactions`, looping while
/// `messages.AffectedHistory.offset > 0` (`menu/menu_send.cpp:1025-1043`).
pub(crate) fn read_unread_reactions(
    live: &LiveSession,
    peer_id: PeerId,
    top_msg_id: Option<MsgId>,
    saved_peer_id: Option<PeerId>,
) -> Result<UnreadThingsReadResult, String> {
    let Some(peer) = live.input_peer(peer_id) else {
        return Err("Peer not found in session for messages.readReactions.".to_owned());
    };
    let saved_peer = match saved_peer_id {
        Some(sp) => Some(
            live.input_peer(sp)
                .ok_or_else(|| "Saved sublist peer not found in session.".to_owned())?,
        ),
        None => None,
    };
    let mut result = UnreadThingsReadResult::default();
    for _ in 0..MAX_AFFECTED_HISTORY_LOOPS {
        let tl_enums::messages::AffectedHistory::AffectedHistory(aff) = invoke_rpc(
            live,
            tl_fns::messages::ReadReactions {
                peer: peer.clone(),
                top_msg_id: top_msg_id.map(|m| m.0),
                saved_peer_id: saved_peer.clone(),
            },
        )?;
        let _ = live.apply_affected_history(Some(peer_id), aff.pts, aff.pts_count);
        result.pages += 1;
        result.last_pts = aff.pts;
        result.total_pts_count += aff.pts_count;
        if aff.offset <= 0 {
            break;
        }
    }
    let scope = UnreadThingsScope {
        peer_id,
        top_msg_id,
        saved_peer_id,
    };
    clear_local_unread_things(live, scope, UnreadThingsKind::Reactions);
    Ok(result)
}

/// Marks unread reactions on currently visible `msg_ids` in `peer_id` as read like tdesktop
/// (`history_inner_widget.cpp:1572`, `apiwrap.cpp:1478-1508`):
/// - Clears `HAS_UNREAD_REACTION` / unread recent reactors and decrements local unread-reaction
///   counts in `tg_data::Session` (and `LiveEngagementState`'s cached counts) so both the floating
///   "♥ N" jump button and the chat-row heart badge clear immediately.
/// - Sends `channels.readMessageContents` for supergroups/channels or `messages.readMessageContents`
///   for DMs/basic groups (feeding `AffectedMessages` PTS into `SessionRuntime`).
/// - Returns the subset of `msg_ids` that actually had unread reactions (empty -> zero RPCs).
pub(crate) fn mark_visible_reactions_read(
    live: &LiveSession,
    peer_id: PeerId,
    msg_ids: &[MsgId],
) -> Result<Vec<MsgId>, String> {
    let data_ids: Vec<DataMsgId> = msg_ids
        .iter()
        .copied()
        .filter(|id| id.0 > 0)
        .map(to_data_msg)
        .collect();
    if data_ids.is_empty() {
        return Ok(Vec::new());
    }
    let data_peer = to_data_peer(peer_id);
    let (reactions_read, mentions_read) = live.write_session(|session| {
        // Read first: marking reactions read keeps the mention flags it looks at.
        let mentions = visible_unread_mentions(session, data_peer, &data_ids);
        let reactions = session.mark_messages_reactions_read(data_peer, &data_ids);
        if !mentions.is_empty() {
            let _ = session.mark_messages_contents_read(Some(data_peer), &mentions);
        }
        (reactions, mentions)
    });
    let mut marked_data = reactions_read.clone();
    for id in &mentions_read {
        if !marked_data.contains(id) {
            marked_data.push(*id);
        }
    }
    if marked_data.is_empty() {
        return Ok(Vec::new());
    }
    let marked_ui: Vec<MsgId> = marked_data.iter().copied().map(from_data_msg).collect();
    let mentions_ui: Vec<MsgId> = mentions_read.iter().copied().map(from_data_msg).collect();

    // Synchronize `LiveEngagementState`'s cached unread_reactions counters / loaded_ids so
    // `unread_things_counts(scope)` immediately reflects the newly read reactions. The state is
    // locked inside the session read, the same order as the updates listener (session write lock,
    // then state): waiting for the session while holding the state deadlocked with it.
    let state = state_for_live(live);
    live.read_session(|session| {
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        for (scope, entry) in guard
            .unread_reactions
            .iter_mut()
            .filter(|(scope, _)| scope.peer_id == peer_id)
        {
            for id in &marked_ui {
                entry.loaded_ids.remove(id);
            }
            if entry.count.is_some() {
                entry.count = Some(scope_unread_reactions(session, data_peer, scope).max(0));
            }
        }
        // The "@" button reads these counts; a cached server count would keep it at 1 after
        // the mention was seen (TGB-3125).
        if !mentions_ui.is_empty() {
            for (scope, entry) in guard
                .unread_mentions
                .iter_mut()
                .filter(|(scope, _)| scope.peer_id == peer_id)
            {
                for id in &mentions_ui {
                    entry.loaded_ids.remove(id);
                }
                if entry.count.is_some() {
                    entry.count = Some(scope_unread_mentions(session, data_peer, scope).max(0));
                }
            }
        }
    });
    live.bridge_state().notify_ui();

    // `ApiWrap::markContentsRead` (`apiwrap.cpp:1478-1508`): the local state changed above and
    // the request goes out without the UI waiting for it, ordered with read receipts and typing.
    let channel = live.input_channel(peer_id);
    let runtime = Arc::clone(live.runtime());
    live.ui_rpc.push(async move {
        let rpc = runtime.rpc().clone();
        let sent = if let Some(channel) = channel {
            let req = tg_api::channels::build_read_channel_message_contents(channel, &marked_data);
            with_rpc_timeout(
                ENGAGEMENT_RPC_TIMEOUT,
                async move { rpc.invoke(&req).await },
            )
            .await
            .map(|_| ())
        } else {
            let req = tg_api::channels::build_read_common_message_contents(&marked_data);
            match with_rpc_timeout(
                ENGAGEMENT_RPC_TIMEOUT,
                async move { rpc.invoke(&req).await },
            )
            .await
            {
                Ok(tl_enums::messages::AffectedMessages::AffectedMessages(aff)) => runtime
                    .apply_affected_history(None, aff.pts, aff.pts_count)
                    .await
                    .map_err(|e| e.to_string()),
                Err(err) => Err(err),
            }
        };
        if let Err(err) = sent {
            debug!(peer = peer_id.0, error = %err, "readMessageContents failed");
        }
    });

    Ok(marked_ui)
}

/// Shown messages whose mention of the user is unread and that are not unplayed voice or
/// round messages (those are read by playing them): `HistoryInner::paintEvent` reads them with
/// `markContentsRead` (`history_inner_widget.cpp:1574-1576`, `HistoryItem::isUnreadMention` /
/// `isUnreadMedia`).
fn visible_unread_mentions(
    session: &Session,
    peer: DataPeerId,
    ids: &[DataMsgId],
) -> Vec<DataMsgId> {
    let mut out: Vec<DataMsgId> = Vec::new();
    for &id in ids {
        if !id.is_server() || out.contains(&id) {
            continue;
        }
        let Some(msg) = session.message(tg_data::FullMsgId::new(peer, id)) else {
            continue;
        };
        let unread_mention = !msg.is_outgoing()
            && msg.flags.contains(MessageFlags::MENTIONS_ME)
            && msg.flags.contains(MessageFlags::MEDIA_IS_UNREAD);
        let unplayed_media = match msg.media() {
            Some(tg_data::MessageMedia::Document(doc)) => {
                matches!(
                    doc.kind,
                    tg_data::DocumentKind::Voice | tg_data::DocumentKind::RoundVideo
                ) || doc.ttl_seconds.is_some()
            }
            Some(tg_data::MessageMedia::Photo(photo)) => photo.ttl_seconds.is_some(),
            _ => false,
        };
        if unread_mention && !unplayed_media {
            out.push(id);
        }
    }
    out
}

/// Unread mentions of `scope` (a forum topic or the main history) as the session counts them.
fn scope_unread_mentions(
    session: &Session,
    data_peer: DataPeerId,
    scope: &UnreadThingsScope,
) -> i32 {
    if let Some(top_id) = scope.top_msg_id {
        return session
            .forum(data_peer)
            .and_then(|f| f.topic(to_data_msg(top_id)))
            .map_or(0, |t| t.unread_mentions_count);
    }
    session
        .history(data_peer)
        .map_or(0, |h| h.unread_mentions_count)
}

/// Unread reactions of `scope` (a forum topic, a saved sublist or the main history) as the
/// session counts them.
fn scope_unread_reactions(
    session: &Session,
    data_peer: DataPeerId,
    scope: &UnreadThingsScope,
) -> i32 {
    if let Some(top_id) = scope.top_msg_id {
        session
            .forum(data_peer)
            .and_then(|f| f.topic(to_data_msg(top_id)))
            .map_or(0, |t| t.unread_reactions_count)
    } else if let Some(saved_peer) = scope.saved_peer_id {
        let saved_data = to_data_peer(saved_peer);
        if let Some(ch_id) = data_peer.to_channel() {
            session
                .monoforum(ch_id)
                .and_then(|m| m.sublist(saved_data))
                .map_or(0, |s| s.unread_reactions_count)
        } else {
            session
                .saved_messages()
                .sublist(saved_data)
                .map_or(0, |s| s.unread_reactions_count)
        }
    } else {
        session
            .history(data_peer)
            .map_or(0, |h| h.unread_reactions_count)
    }
}

/// Marks all unread poll votes read via `messages.readPollVotes`, looping while
/// `messages.AffectedHistory.offset > 0` (`menu/menu_send.cpp:1070-1085`).
pub(crate) fn read_unread_poll_votes(
    live: &LiveSession,
    peer_id: PeerId,
    top_msg_id: Option<MsgId>,
) -> Result<UnreadThingsReadResult, String> {
    let Some(peer) = live.input_peer(peer_id) else {
        return Err("Peer not found in session for messages.readPollVotes.".to_owned());
    };
    let mut result = UnreadThingsReadResult::default();
    for _ in 0..MAX_AFFECTED_HISTORY_LOOPS {
        let tl_enums::messages::AffectedHistory::AffectedHistory(aff) = invoke_rpc(
            live,
            tl_fns::messages::ReadPollVotes {
                peer: peer.clone(),
                top_msg_id: top_msg_id.map(|m| m.0),
            },
        )?;
        let _ = live.apply_affected_history(Some(peer_id), aff.pts, aff.pts_count);
        result.pages += 1;
        result.last_pts = aff.pts;
        result.total_pts_count += aff.pts_count;
        if aff.offset <= 0 {
            break;
        }
    }
    let scope = UnreadThingsScope {
        peer_id,
        top_msg_id,
        saved_peer_id: None,
    };
    clear_local_unread_things(live, scope, UnreadThingsKind::PollVotes);
    Ok(result)
}

// =============================================================================
// 3. Read Participants & 1:1 Outbox Read Date (`api/api_who_reacted.cpp`)
// =============================================================================

/// Fetches group read-receipt participants via `messages.getMessageReadParticipants`
/// (`api/api_who_reacted.cpp:319-342`).
pub(crate) fn get_message_read_participants(
    live: &LiveSession,
    peer_id: PeerId,
    msg_id: MsgId,
) -> Result<Vec<MessageReadParticipant>, String> {
    let Some(peer) = live.input_peer(peer_id) else {
        return Err(
            "Peer not found in session for messages.getMessageReadParticipants.".to_owned(),
        );
    };
    let list = invoke_rpc(
        live,
        tl_fns::messages::GetMessageReadParticipants {
            peer,
            msg_id: msg_id.0,
        },
    )?;

    let participants = live.read_session(|session| {
        list.into_iter()
            .map(|entry| {
                let tl_enums::ReadParticipantDate::ReadParticipantDate(d) = entry;
                let data_uid = DataUserId::from_tl(d.user_id);
                let data_pid = DataPeerId::from_user(data_uid);
                let user_peer = from_data_peer(data_pid);
                let (name, username, color_index) = match session.peer(data_pid) {
                    Some(peer) => {
                        let title = peer_display_title(session, data_pid);
                        let uname = peer
                            .as_user()
                            .map(|u| u.usernames.username())
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned);
                        let color = (d.user_id.unsigned_abs() % 7) as u8;
                        (title, uname, color)
                    }
                    None => (
                        format!("User {}", d.user_id),
                        None,
                        (d.user_id.unsigned_abs() % 7) as u8,
                    ),
                };
                MessageReadParticipant {
                    user_id: user_peer,
                    name,
                    username,
                    read_date_unix: d.date,
                    color_index,
                }
            })
            .collect()
    });
    Ok(participants)
}

/// Fetches the 1:1 DM outgoing message read timestamp via `messages.getOutboxReadDate`
/// (`api/api_who_reacted.cpp:288-317`), preserving tdesktop's exact privacy/age error mapping.
pub(crate) fn get_outbox_read_date(
    live: &LiveSession,
    peer_id: PeerId,
    msg_id: MsgId,
) -> Result<OutboxReadDateStatus, String> {
    let Some(peer) = live.input_peer(peer_id) else {
        return Err("Peer not found in session for messages.getOutboxReadDate.".to_owned());
    };
    match invoke_rpc_raw(
        live,
        tl_fns::messages::GetOutboxReadDate {
            peer,
            msg_id: msg_id.0,
        },
    ) {
        Ok(tl_enums::OutboxReadDate::OutboxReadDate(d)) => {
            Ok(OutboxReadDateStatus::Read { date_unix: d.date })
        }
        Err(InvocationError::Rpc(rpc_err)) => match rpc_err.name.as_str() {
            "YOUR_PRIVACY_RESTRICTED" => Ok(OutboxReadDateStatus::MyPrivacyHidden),
            "USER_PRIVACY_RESTRICTED" => Ok(OutboxReadDateStatus::PeerPrivacyHidden),
            "MESSAGE_TOO_OLD" => Ok(OutboxReadDateStatus::TooOld),
            "MESSAGE_ID_INVALID" => Ok(OutboxReadDateStatus::Unavailable),
            _ => Err(format!("RPC {} {}", rpc_err.code, rpc_err.name)),
        },
        Err(other) => Err(format!("messages.getOutboxReadDate failed: {other}")),
    }
}

// =============================================================================
// 4. Discussion / Replies Thread Read Sync (`data/data_replies_list.cpp:967`)
// =============================================================================

/// Marks a channel comments / discussion thread or forum topic read up to `read_max_id`
/// via `messages.readDiscussion` and updates both `tg_data::Session` and
/// `history_sections` cached `DiscussionReadState`.
pub(crate) fn read_discussion(
    live: &LiveSession,
    peer_id: PeerId,
    top_msg_id: MsgId,
    read_max_id: MsgId,
) -> Result<bool, String> {
    let Some(peer) = live.input_peer(peer_id) else {
        return Err("Peer not found in session for messages.readDiscussion.".to_owned());
    };
    let ok = invoke_rpc(
        live,
        tl_fns::messages::ReadDiscussion {
            peer,
            msg_id: top_msg_id.0,
            read_max_id: read_max_id.0,
        },
    )?;

    if ok {
        let data_peer = to_data_peer(peer_id);
        let data_top = to_data_msg(top_msg_id);
        let data_read_max = to_data_msg(read_max_id);
        live.write_session(|session| {
            let unread_override = session
                .forum(data_peer)
                .and_then(|f| f.topic(data_top))
                .and_then(|t| {
                    (t.last_known_server_msg_id.0 == 0
                        || data_read_max >= t.last_known_server_msg_id)
                        .then_some(0)
                });
            let _ =
                session.mark_topic_inbox_read(data_peer, data_top, data_read_max, unread_override);
        });
        live.mark_discussion_thread_read_local(peer_id, top_msg_id, read_max_id);
        live.bridge_state().notify_ui();
    }
    Ok(ok)
}

// =============================================================================
// 5. Message Delivery Reports (`data/data_histories.cpp:669-719`)
// =============================================================================

/// Queues message IDs for delivery reporting (`Histories::reportDelivery`).
pub fn queue_message_delivery_report(live: &LiveSession, peer_id: PeerId, msg_ids: &[MsgId]) {
    if msg_ids.is_empty() {
        return;
    }
    let state = state_for_live(live);
    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
    let set = guard.pending_delivery_reports.entry(peer_id).or_default();
    for &id in msg_ids {
        if id.0 > 0 {
            set.insert(id);
        }
    }
}

/// Flushes queued delivery reports in chunks of `REPORT_DELIVERIES_PER_REQUEST` (50)
/// via `messages.reportMessagesDelivery` (`Histories::reportPendingDeliveries`).
pub fn flush_delivery_reports(live: &LiveSession, push: bool) -> Result<usize, String> {
    let pending: Vec<(PeerId, Vec<MsgId>)> = {
        let state = state_for_live(live);
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .pending_delivery_reports
            .drain()
            .filter(|(_, ids)| !ids.is_empty())
            .map(|(pid, ids)| (pid, ids.into_iter().collect()))
            .collect()
    };
    let mut total_reported = 0;
    for (peer_id, ids) in pending {
        total_reported += report_messages_delivery(live, peer_id, &ids, push)?;
    }
    Ok(total_reported)
}

/// Reports message delivery to Telegram via `messages.reportMessagesDelivery`,
/// deduplicated and chunked to at most `REPORT_DELIVERIES_PER_REQUEST` (50) IDs per RPC
/// (`data/data_histories.cpp:687-717`).
pub(crate) fn report_messages_delivery(
    live: &LiveSession,
    peer_id: PeerId,
    msg_ids: &[MsgId],
    push: bool,
) -> Result<usize, String> {
    let unique: Vec<i32> = {
        let mut set = BTreeSet::new();
        for &m in msg_ids {
            if m.0 > 0 {
                set.insert(m.0);
            }
        }
        set.into_iter().collect()
    };
    if unique.is_empty() {
        return Ok(0);
    }
    let Some(peer) = live.input_peer(peer_id) else {
        return Err("Peer not found in session for messages.reportMessagesDelivery.".to_owned());
    };

    let mut reported = 0;
    for chunk in unique.chunks(REPORT_DELIVERIES_PER_REQUEST) {
        let ok = invoke_rpc(
            live,
            tl_fns::messages::ReportMessagesDelivery {
                push,
                peer: peer.clone(),
                id: chunk.to_vec(),
            },
        )?;
        if ok {
            reported += chunk.len();
        }
    }
    Ok(reported)
}

// =============================================================================
// 6. Ephemeral Messages Delete & Report (`ephemeral_messages.cpp`, `api_report.cpp`)
// =============================================================================

/// Returns a tracked ephemeral message entry received via `updateNewEphemeralMessage`, if any.
#[must_use]
pub fn tracked_ephemeral_entry(
    live: &LiveSession,
    peer_id: PeerId,
    ephemeral_id: i32,
) -> Option<TrackedEphemeralEntry> {
    let state = state_for_live(live);
    let guard = state.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .ephemeral_entries
        .get(&(peer_id, ephemeral_id))
        .cloned()
}

/// Deletes an ephemeral message via `ephemeral.deleteMessage` with `Flag::f_peer` set
/// (`EphemeralMessages::deleteMessage` in `data/components/ephemeral_messages.cpp:1059-1075`).
pub(crate) fn ephemeral_delete_message(
    live: &LiveSession,
    peer_id: PeerId,
    receiver_user_id: PeerId,
    ephemeral_id: i32,
) -> Result<bool, String> {
    let Some(peer) = live.input_peer(peer_id) else {
        return Err("Peer not found in session for ephemeral.deleteMessage.".to_owned());
    };
    let Some(receiver_id) = input_user(live, receiver_user_id) else {
        return Err("Receiver user not found in session for ephemeral.deleteMessage.".to_owned());
    };
    let ok = invoke_rpc(
        live,
        tl_fns::ephemeral::DeleteMessage {
            peer: Some(peer),
            receiver_id,
            id: ephemeral_id,
        },
    )?;

    let state = state_for_live(live);
    if let Ok(mut guard) = state.lock() {
        guard.ephemeral_entries.remove(&(peer_id, ephemeral_id));
    }
    live.bridge_state().notify_ui();
    Ok(ok)
}

/// Reports an ephemeral message via `ephemeral.reportMessage`
/// (`api/api_report.cpp:162-184`).
pub(crate) fn ephemeral_report_message(
    live: &LiveSession,
    peer_id: PeerId,
    ephemeral_id: i32,
    option: &[u8],
    comment: &str,
) -> Result<EphemeralReportOutcome, String> {
    let Some(peer) = live.input_peer(peer_id) else {
        return Err("Peer not found in session for ephemeral.reportMessage.".to_owned());
    };
    let resp = invoke_rpc(
        live,
        tl_fns::ephemeral::ReportMessage {
            peer,
            id: ephemeral_id,
            option: option.to_vec(),
            message: comment.to_owned(),
        },
    )?;
    Ok(match resp {
        tl_enums::ReportResult::ChooseOption(c) => EphemeralReportOutcome::ChooseOption {
            title: c.title,
            options: c
                .options
                .into_iter()
                .map(|o| {
                    let tl_enums::MessageReportOption::MessageReportOption(opt) = o;
                    EphemeralReportOption {
                        option: opt.option,
                        text: opt.text,
                    }
                })
                .collect(),
        },
        tl_enums::ReportResult::AddComment(a) => EphemeralReportOutcome::AddComment {
            optional: a.optional,
            option: a.option,
        },
        tl_enums::ReportResult::Reported => EphemeralReportOutcome::Reported,
    })
}

// =============================================================================
// 7. Export Message Link & Get Message Author (`apiwrap.cpp`, `history_view_context_menu.cpp`)
// =============================================================================

/// Exports a direct message link via `channels.exportMessageLink`
/// (`ApiWrap::exportDirectMessageLink` in `apiwrap.cpp:878-889`), caching by
/// `(peer_id, msg_id, grouped, thread)`.
pub(crate) fn export_message_link(
    live: &LiveSession,
    peer_id: PeerId,
    msg_id: MsgId,
    grouped: bool,
    thread: bool,
) -> Result<ExportedMessageLinkInfo, String> {
    let Some(channel) = live.input_channel(peer_id) else {
        return Err("Peer is not a channel/supergroup for channels.exportMessageLink.".to_owned());
    };
    let tl_enums::ExportedMessageLink::ExportedMessageLink(exported) = invoke_rpc(
        live,
        tl_fns::channels::ExportMessageLink {
            grouped,
            thread,
            channel,
            id: msg_id.0,
        },
    )?;
    let info = ExportedMessageLinkInfo {
        link: exported.link,
        html: exported.html,
    };
    let state = state_for_live(live);
    if let Ok(mut guard) = state.lock() {
        guard
            .exported_links
            .insert((peer_id, msg_id, grouped, thread), info.clone());
    }
    Ok(info)
}

/// Resolves the author of a channel/anonymous message via `channels.getMessageAuthor`
/// (`LookupMessageAuthor` in `history_view_context_menu.cpp:1556-1616`), caching per
/// `(peer_id, msg_id)` and storing the returned user in `tg_data::Session`.
pub(crate) fn get_message_author(
    live: &LiveSession,
    peer_id: PeerId,
    msg_id: MsgId,
) -> Result<MessageAuthorInfo, String> {
    {
        let state = state_for_live(live);
        let guard = state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = guard.message_authors.get(&(peer_id, msg_id)) {
            return Ok(cached.clone());
        }
    }

    let Some(channel) = live.input_channel(peer_id) else {
        return Err("Peer is not a channel/supergroup for channels.getMessageAuthor.".to_owned());
    };
    let user = invoke_rpc(
        live,
        tl_fns::channels::GetMessageAuthor {
            channel,
            id: msg_id.0,
        },
    )?;

    let info = match &user {
        tl_enums::User::Empty(_) => {
            return Err("Message author is unavailable (userEmpty).".to_owned());
        }
        tl_enums::User::User(u) => {
            let name = format_tl_user_name(u);
            MessageAuthorInfo {
                user_id: from_data_peer(DataPeerId::from_user(DataUserId::from_tl(u.id))),
                name,
                username: u.username.clone(),
                is_deleted: u.deleted,
            }
        }
    };

    live.write_session(|session| {
        apply_tl_users_and_chats(session, std::slice::from_ref(&user), &[]);
    });

    let state = state_for_live(live);
    if let Ok(mut guard) = state.lock() {
        guard
            .message_authors
            .insert((peer_id, msg_id), info.clone());
    }
    Ok(info)
}

fn format_tl_user_name(u: &tl_types::User) -> String {
    match (u.first_name.as_deref(), u.last_name.as_deref()) {
        (Some(f), Some(l)) if !f.is_empty() && !l.is_empty() => format!("{f} {l}"),
        (Some(f), _) if !f.is_empty() => f.to_owned(),
        (_, Some(l)) if !l.is_empty() => l.to_owned(),
        _ => u
            .username
            .as_deref()
            .map_or_else(|| format!("User {}", u.id), |un| format!("@{un}")),
    }
}

// =============================================================================
// 8. Basic Group Migration & Future Creator (`apiwrap.cpp`, `window_peer_menu.cpp`)
// =============================================================================

/// Upgrades a basic group chat to a supergroup via `messages.migrateChat`
/// (`ApiWrap::migrateChat` in `apiwrap.cpp:1400-1476`).
pub(crate) fn migrate_chat(
    live: &LiveSession,
    chat_peer_id: PeerId,
) -> Result<ChatMigrationResult, String> {
    let data_peer = to_data_peer(chat_peer_id);
    let precheck = live.read_session(|session| {
        let Some(peer) = session.peer(data_peer) else {
            return Err("Chat not found in session for messages.migrateChat.".to_owned());
        };
        let Some(chat) = peer.as_chat() else {
            return Err("BAD_MIGRATION: Peer is not a basic group chat.".to_owned());
        };
        if let Some(target_channel) = chat.migrated_to {
            return Ok(Err(ChatMigrationResult {
                old_chat_id: chat_peer_id,
                new_channel_id: from_data_peer(DataPeerId::from_channel(target_channel)),
                already_migrated: true,
            }));
        }
        if chat.flags.contains(ChatFlags::DEACTIVATED) {
            return Err("BAD_MIGRATION: Chat is already deactivated.".to_owned());
        }
        if !chat.flags.contains(ChatFlags::CREATOR) {
            return Err("BAD_MIGRATION: Current user is not the creator of that chat.".to_owned());
        }
        Ok(Ok(chat.chat_id()))
    })?;

    let chat_id = match precheck {
        Ok(id) => id,
        Err(already) => return Ok(already),
    };

    let updates = match invoke_rpc_raw(
        live,
        tl_fns::messages::MigrateChat {
            chat_id: chat_id.0 as i64,
        },
    ) {
        Ok(u) => u,
        Err(InvocationError::Rpc(rpc_err)) => {
            return Err(format!("RPC {} {}", rpc_err.code, rpc_err.name));
        }
        Err(other) => return Err(format!("messages.migrateChat failed: {other}")),
    };

    let _ = live.apply_updates_to_session(&updates);

    let migrated_channel = live.write_session(|session| {
        let mut target_channel: Option<ChannelId> = None;
        let (chats, upd_list): (&[tl_enums::Chat], &[tl_enums::Update]) = match &updates {
            tl_enums::Updates::Updates(u) => (&u.chats, &u.updates),
            tl_enums::Updates::Combined(c) => (&c.chats, &c.updates),
            tl_enums::Updates::UpdateShort(s) => (&[], std::slice::from_ref(&s.update)),
            _ => (&[], &[]),
        };

        // 1. Check if `Chat::migrated_to` was set by `UpdatesHandler`.
        if let Some(ch) = session
            .peer(data_peer)
            .and_then(Peer::as_chat)
            .and_then(|c| c.migrated_to)
        {
            target_channel = Some(ch);
        }

        // 2. Check `messageActionChatMigrateTo` in returned updates.
        if target_channel.is_none() {
            for upd in upd_list {
                let raw_msg = match upd {
                    tl_enums::Update::NewMessage(u) => Some(&u.message),
                    tl_enums::Update::NewChannelMessage(u) => Some(&u.message),
                    _ => None,
                };
                if let Some(tl_enums::Message::Service(svc)) = raw_msg
                    && let tl_enums::MessageAction::ChatMigrateTo(act) = &svc.action
                {
                    target_channel = Some(ChannelId::from_tl(act.channel_id));
                    break;
                }
            }
        }

        // 3. Fallback: pick a megagroup/channel returned in `updates.chats`.
        if target_channel.is_none() {
            for c in chats {
                if let tl_enums::Chat::Channel(ch) = c {
                    target_channel = Some(ChannelId::from_tl(ch.id));
                    break;
                }
            }
        }

        if let Some(ch_id) = target_channel {
            session.apply_migration(chat_id, ch_id);
        }
        target_channel
    });

    let Some(channel_id) = migrated_channel else {
        return Err("MIGRATION_FAIL: No target supergroup channel returned.".to_owned());
    };
    live.bridge_state().notify_ui();
    Ok(ChatMigrationResult {
        old_chat_id: chat_peer_id,
        new_channel_id: from_data_peer(DataPeerId::from_channel(channel_id)),
        already_migrated: false,
    })
}

/// Queries who would become the new creator if the current creator leaves `peer_id` via
/// `messages.getFutureChatCreatorAfterLeave` (`window_peer_menu.cpp:4367-4389`).
/// Returns `Ok(None)` when the server responds with `userEmpty`.
pub(crate) fn future_chat_creator_after_leave(
    live: &LiveSession,
    peer_id: PeerId,
) -> Result<Option<FutureChatCreatorInfo>, String> {
    let Some(peer) = live.input_peer(peer_id) else {
        return Err(
            "Peer not found in session for messages.getFutureChatCreatorAfterLeave.".to_owned(),
        );
    };
    let resp = invoke_rpc(
        live,
        tl_fns::messages::GetFutureChatCreatorAfterLeave { peer },
    )?;
    match &resp {
        tl_enums::User::Empty(_) => Ok(None),
        tl_enums::User::User(u) => {
            let info = FutureChatCreatorInfo {
                user_id: from_data_peer(DataPeerId::from_user(DataUserId::from_tl(u.id))),
                name: format_tl_user_name(u),
                username: u.username.clone(),
            };
            live.write_session(|session| {
                apply_tl_users_and_chats(session, std::slice::from_ref(&resp), &[]);
            });
            debug!(
                peer = peer_id.0,
                future_creator = info.user_id.0,
                "Resolved future chat creator after leave"
            );
            Ok(Some(info))
        }
    }
}
