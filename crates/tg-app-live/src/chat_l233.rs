//! Live Layer-233 chat coordination for WP-38:
//! - Streamed bot drafts (`history_streamed_drafts`: `sendMessageTextDraftAction`,
//!   `sendMessageRichMessageDraftAction`, `sendMessageStopDraftAction`, `adoptIncoming`)
//! - Per-session compose stash store (`data_compose_stash`)
//! - Batched `messages.reportReadMetrics` dispatcher (`api_read_metrics`)
//! - Interactive emoji tap sync (`SendMessageEmojiInteraction` / `SendMessageEmojiInteractionSeen`)
//! - 1:1 protected-chat sharing request & toggle (`messages.toggleNoForwards`)
//!
//! Port origin: `Telegram/SourceFiles/history/history_streamed_drafts.cpp`,
//! `Telegram/SourceFiles/chat_helpers/emoji_interactions.cpp`,
//! `Telegram/SourceFiles/api/api_read_metrics.cpp`, and
//! `Telegram/SourceFiles/api/api_suggest_post.cpp` (`RespondToNoForwardsRequest`).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use tg_api::RpcClient;
use tg_api::convert::entities_from_tl;
use tg_data::{FullMsgId as DataFullMsgId, Session, TextWithEntities as DataText, UserFlags};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tokio::sync::{RwLock, broadcast};
use tracing::debug;

use crate::convert::{
    data_text_to_ui_text, now_unix_i32, peer_display_title, to_data_msg, to_data_peer,
};
use crate::read_metrics::{FinalizedReadMetric, ReadMetricsBatcher, send_read_metrics_live};
use crate::session::LiveSession;
use tg_app_core::core_types::dto::compose_stash::{
    ComposeStash, ComposeStashKey, ComposeStashStore,
};
use tg_app_core::core_types::dto::emoji_interactions::{
    EmojiInteractionsBunch, IncomingEmojiInteraction, interactions_to_json, parse_interactions_json,
};
use tg_app_core::core_types::dto::rich_message::payload_from_tl_enum;
use tg_app_core::core_types::dto::streamed_drafts::{AdoptedStreamedDraft, HistoryStreamedDrafts};
use tg_app_core::history::backend::HistoryMessage;
use tg_app_core::stand_in::types::{MsgId, PeerId};

fn monotonic_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[derive(Default)]
struct LiveChatL233State {
    updates_listener_started: bool,
    streamed_drafts: HashMap<PeerId, HistoryStreamedDrafts>,
    last_adopted: Vec<(PeerId, AdoptedStreamedDraft)>,
    compose_stashes: ComposeStashStore,
    read_metrics_batcher: ReadMetricsBatcher,
    incoming_emoji_interactions: HashMap<PeerId, Vec<IncomingEmojiInteraction>>,
    incoming_emoji_seen: HashMap<PeerId, Vec<String>>,
    no_forwards_acted_msgs: HashSet<(PeerId, MsgId)>,
}

struct RegistryEntry {
    session_weak: Weak<RwLock<Session>>,
    state: Arc<Mutex<LiveChatL233State>>,
}

fn registry() -> &'static Mutex<HashMap<usize, RegistryEntry>> {
    static REG: OnceLock<Mutex<HashMap<usize, RegistryEntry>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn state_for_session_arc(session: &Arc<RwLock<Session>>) -> Arc<Mutex<LiveChatL233State>> {
    let key = Arc::as_ptr(session) as usize;
    let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    reg.retain(|_, entry| entry.session_weak.strong_count() > 0);
    if let Some(existing) = reg.get(&key)
        && existing.session_weak.ptr_eq(&Arc::downgrade(session))
    {
        return Arc::clone(&existing.state);
    }
    let state = Arc::new(Mutex::new(LiveChatL233State::default()));
    reg.insert(
        key,
        RegistryEntry {
            session_weak: Arc::downgrade(session),
            state: Arc::clone(&state),
        },
    );
    state
}

fn state_for_live(live: &LiveSession) -> Arc<Mutex<LiveChatL233State>> {
    let session = live.runtime().session();
    let state = state_for_session_arc(session);
    ensure_updates_listener(live, &state, Arc::downgrade(session));
    state
}

/// Attaches the background `subscribe_updates()` listener for WP-38 Layer-233 updates
/// (streamed bot drafts, emoji interactions, and no-forwards updates).
pub(crate) fn attach_chat_l233_listener(live: &LiveSession) {
    let _ = state_for_live(live);
}

fn ensure_updates_listener(
    live: &LiveSession,
    state: &Arc<Mutex<LiveChatL233State>>,
    session_weak: Weak<RwLock<Session>>,
) {
    let should_spawn = {
        let Ok(mut guard) = state.lock() else {
            return;
        };
        if guard.updates_listener_started {
            false
        } else {
            guard.updates_listener_started = true;
            true
        }
    };
    if !should_spawn {
        return;
    }
    let Some(mut rx) = live.runtime().rpc().subscribe_updates() else {
        return;
    };
    let state = Arc::clone(state);
    let bridge_state = Arc::clone(live.bridge_state());
    live.runtime().tokio_handle().spawn(async move {
        loop {
            match rx.recv().await {
                Ok(updates) => {
                    let Some(session_arc) = session_weak.upgrade() else {
                        break;
                    };
                    let self_uid = {
                        let s = session_arc.read().await;
                        s.self_user_id().0 as i64
                    };
                    let changed = {
                        let s = session_arc.read().await;
                        if let Ok(mut guard) = state.lock() {
                            apply_updates_container(
                                &s,
                                &mut guard,
                                &updates,
                                self_uid,
                                monotonic_now_ms(),
                            )
                        } else {
                            false
                        }
                    };
                    if changed {
                        bridge_state.notify_ui();
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    debug!(skipped, "wp38 chat_l233 updates listener lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Synchronously applies a TL [`tl_enums::Updates`] batch to the WP-38 live state.
pub(crate) fn apply_updates_for_live_session(live: &LiveSession, updates: &tl_enums::Updates) {
    apply_updates_for_live_session_at(live, updates, monotonic_now_ms());
}

/// Synchronously applies a TL [`tl_enums::Updates`] batch at an explicit `now_ms` timestamp
/// (deterministic entry point for tests).
pub fn apply_updates_for_live_session_at(
    live: &LiveSession,
    updates: &tl_enums::Updates,
    now_ms: u64,
) -> bool {
    let state = state_for_live(live);
    let changed = live.read_session(|session| {
        let self_uid = session.self_user_id().0 as i64;
        if let Ok(mut guard) = state.lock() {
            apply_updates_container(session, &mut guard, updates, self_uid, now_ms)
        } else {
            false
        }
    });
    if changed {
        live.bridge_state().notify_ui();
    }
    changed
}

fn apply_updates_container(
    session: &Session,
    state: &mut LiveChatL233State,
    updates: &tl_enums::Updates,
    self_uid: i64,
    now_ms: u64,
) -> bool {
    let mut changed = false;
    match updates {
        tl_enums::Updates::UpdateShort(s) => {
            changed |= apply_single_update(session, state, &s.update, self_uid, now_ms);
        }
        tl_enums::Updates::Updates(u) => {
            for upd in &u.updates {
                changed |= apply_single_update(session, state, upd, self_uid, now_ms);
            }
        }
        tl_enums::Updates::Combined(c) => {
            for upd in &c.updates {
                changed |= apply_single_update(session, state, upd, self_uid, now_ms);
            }
        }
        _ => {}
    }
    changed
}

fn tl_peer_to_ui_peer(peer: &tl_enums::Peer) -> PeerId {
    match peer {
        tl_enums::Peer::User(u) => PeerId::from_user(u.user_id as u64),
        tl_enums::Peer::Chat(c) => PeerId::from_chat(c.chat_id as u64),
        tl_enums::Peer::Channel(ch) => PeerId::from_channel(ch.channel_id as u64),
    }
}

fn tl_message_peer(m: &tl_types::Message, self_uid: i64) -> (PeerId, PeerId) {
    let peer_id = match &m.peer_id {
        tl_enums::Peer::User(u) => {
            if !m.out
                && u.user_id == self_uid
                && let Some(tl_enums::Peer::User(fu)) = m.from_id.as_ref()
            {
                PeerId::from_user(fu.user_id as u64)
            } else {
                PeerId::from_user(u.user_id as u64)
            }
        }
        other => tl_peer_to_ui_peer(other),
    };
    let from_id = m
        .from_id
        .as_ref()
        .map(tl_peer_to_ui_peer)
        .unwrap_or(peer_id);
    (peer_id, from_id)
}

fn apply_send_action(
    state: &mut LiveChatL233State,
    peer_id: PeerId,
    from_id: PeerId,
    top_msg_id: Option<MsgId>,
    action: &tl_enums::SendMessageAction,
    now_ms: u64,
) -> bool {
    let when = now_unix_i32().max(1);
    match action {
        tl_enums::SendMessageAction::SendMessageTextDraftAction(a) => {
            let tl_enums::TextWithEntities::TextWithEntities(tw) = &a.text;
            let data_text = DataText {
                text: tw.text.clone(),
                entities: entities_from_tl(&tw.entities),
            };
            let ui_text = data_text_to_ui_text(&data_text);
            let drafts = state
                .streamed_drafts
                .entry(peer_id)
                .or_insert_with(|| HistoryStreamedDrafts::new(peer_id));
            drafts.apply_text_draft(
                top_msg_id,
                from_id,
                when,
                a.random_id,
                ui_text,
                a.can_stop,
                a.keep_on_stop,
                now_ms,
            )
        }
        tl_enums::SendMessageAction::SendMessageRichMessageDraftAction(a) => {
            let payload = payload_from_tl_enum(&a.rich_message);
            let drafts = state
                .streamed_drafts
                .entry(peer_id)
                .or_insert_with(|| HistoryStreamedDrafts::new(peer_id));
            drafts.apply_rich_draft(
                top_msg_id,
                from_id,
                when,
                a.random_id,
                payload,
                a.can_stop,
                a.keep_on_stop,
                now_ms,
            )
        }
        tl_enums::SendMessageAction::SendMessageStopDraftAction(a) => {
            let Some(drafts) = state.streamed_drafts.get_mut(&peer_id) else {
                return false;
            };
            // Keep the per-peer `HistoryStreamedDrafts` entry so `stopped_random_ids`
            // continues ignoring late in-flight chunks for this stopped `random_id`.
            drafts.apply_stop(a.random_id)
        }
        tl_enums::SendMessageAction::SendMessageEmojiInteraction(a) => {
            let tl_enums::DataJSON::DataJSON(ref dj) = a.interaction;
            let bunch = parse_interactions_json(&dj.data);
            if bunch.interactions.is_empty() {
                return false;
            }
            state
                .incoming_emoji_interactions
                .entry(peer_id)
                .or_default()
                .push(IncomingEmojiInteraction {
                    peer_id,
                    msg_id: MsgId(a.msg_id),
                    emoticon: a.emoticon.clone(),
                    bunch,
                });
            true
        }
        tl_enums::SendMessageAction::SendMessageEmojiInteractionSeen(a) => {
            state
                .incoming_emoji_seen
                .entry(peer_id)
                .or_default()
                .push(a.emoticon.clone());
            true
        }
        _ => false,
    }
}

fn apply_single_update(
    session: &Session,
    state: &mut LiveChatL233State,
    upd: &tl_enums::Update,
    self_uid: i64,
    now_ms: u64,
) -> bool {
    match upd {
        tl_enums::Update::UserTyping(u) => {
            let peer_id = PeerId::from_user(u.user_id as u64);
            apply_send_action(
                state,
                peer_id,
                peer_id,
                u.top_msg_id.map(MsgId),
                &u.action,
                now_ms,
            )
        }
        tl_enums::Update::ChatUserTyping(u) => {
            let peer_id = PeerId::from_chat(u.chat_id as u64);
            let from_id = tl_peer_to_ui_peer(&u.from_id);
            apply_send_action(state, peer_id, from_id, None, &u.action, now_ms)
        }
        tl_enums::Update::ChannelUserTyping(u) => {
            let peer_id = PeerId::from_channel(u.channel_id as u64);
            let from_id = tl_peer_to_ui_peer(&u.from_id);
            apply_send_action(
                state,
                peer_id,
                from_id,
                u.top_msg_id.map(MsgId),
                &u.action,
                now_ms,
            )
        }
        tl_enums::Update::NewMessage(u) => {
            adopt_incoming_tl_message(session, state, &u.message, self_uid)
        }
        tl_enums::Update::NewChannelMessage(u) => {
            adopt_incoming_tl_message(session, state, &u.message, self_uid)
        }
        _ => false,
    }
}

fn adopt_incoming_tl_message(
    session: &Session,
    state: &mut LiveChatL233State,
    tl_msg: &tl_enums::Message,
    self_uid: i64,
) -> bool {
    match tl_msg {
        tl_enums::Message::Message(m) => {
            let (peer_id, from_id) = tl_message_peer(m, self_uid);
            let Some(drafts) = state.streamed_drafts.get_mut(&peer_id) else {
                return false;
            };
            let already_exists = session
                .message(DataFullMsgId::new(
                    to_data_peer(peer_id),
                    to_data_msg(MsgId(m.id)),
                ))
                .is_some();
            if let Some(adopted) = drafts.adopt_incoming(m, from_id, already_exists) {
                state.last_adopted.push((peer_id, adopted));
                if drafts.is_empty() {
                    state.streamed_drafts.remove(&peer_id);
                }
                return true;
            }
            false
        }
        tl_enums::Message::Service(svc) => {
            // If a NoForwardsToggle service message arrives, mark prior pending NoForwardsRequest
            // messages in this peer as action_taken (`history.cpp:1872-1876`).
            if matches!(svc.action, tl_enums::MessageAction::NoForwardsToggle(_)) {
                let peer_id = match &svc.peer_id {
                    tl_enums::Peer::User(u) => {
                        if !svc.out
                            && u.user_id == self_uid
                            && let Some(tl_enums::Peer::User(fu)) = svc.from_id.as_ref()
                        {
                            PeerId::from_user(fu.user_id as u64)
                        } else {
                            PeerId::from_user(u.user_id as u64)
                        }
                    }
                    other => tl_peer_to_ui_peer(other),
                };
                if let Some(tl_enums::MessageReplyHeader::MessageReplyHeader(ref h)) = svc.reply_to
                    && let Some(req_id) = h.reply_to_msg_id
                {
                    state
                        .no_forwards_acted_msgs
                        .insert((peer_id, MsgId(req_id)));
                    return true;
                }
            }
            false
        }
        tl_enums::Message::Empty(_) => false,
    }
}

// ============================================================================
// Streamed Bot Drafts Live API
// ============================================================================

/// Returns synthetic [`HistoryMessage`] bubbles for active streamed bot drafts in `peer_id`.
#[must_use]
pub fn streamed_draft_messages_live(
    live: &LiveSession,
    peer_id: PeerId,
    root_id: Option<MsgId>,
) -> Vec<HistoryMessage> {
    let state = state_for_live(live);
    let now_ms = monotonic_now_ms();
    live.read_session(|session| {
        let Ok(mut guard) = state.lock() else {
            return Vec::new();
        };
        let Some(drafts) = guard.streamed_drafts.get_mut(&peer_id) else {
            return Vec::new();
        };
        let _ = drafts.expire_stale(now_ms);
        drafts.to_history_messages(root_id, |from_pid| {
            peer_display_title(session, to_data_peer(from_pid))
        })
    })
}

/// Appends any active streamed bot draft messages for `peer_id` to `messages`.
pub(crate) fn append_streamed_drafts_to_slice(
    live: &LiveSession,
    peer_id: PeerId,
    messages: &mut Vec<HistoryMessage>,
) {
    let drafts = streamed_draft_messages_live(live, peer_id, None);
    if !drafts.is_empty() {
        messages.extend(drafts);
    }
}

/// Returns `true` if `peer_id` has an active stoppable streamed draft for `root_id`.
#[must_use]
pub fn has_stoppable_streamed_draft_live(
    live: &LiveSession,
    peer_id: PeerId,
    root_id: Option<MsgId>,
) -> bool {
    let state = state_for_live(live);
    state
        .lock()
        .ok()
        .and_then(|g| {
            g.streamed_drafts
                .get(&peer_id)
                .and_then(|d| d.stoppable_for(root_id))
        })
        .is_some()
}

/// Sends `messages.setTyping(sendMessageStopDraftAction)` for the newest stoppable streamed draft
/// in `(peer_id, root_id)` and applies local stop state (`HistoryStreamedDrafts::requestStop`).
pub fn stop_streamed_draft_live(
    live: &LiveSession,
    peer_id: PeerId,
    root_id: Option<MsgId>,
) -> Result<bool, String> {
    let state = state_for_live(live);
    let target = state.lock().ok().and_then(|g| {
        g.streamed_drafts
            .get(&peer_id)
            .and_then(|d| d.stoppable_for(root_id))
    });
    let Some((random_id, top_msg_id)) = target else {
        return Ok(false);
    };
    let Some(input_peer) = live.input_peer(peer_id) else {
        return Err("Peer not found in session.".to_owned());
    };
    let req = tl_fns::messages::SetTyping {
        peer: input_peer,
        top_msg_id: top_msg_id.map(|m| m.0),
        action: tl_enums::SendMessageAction::SendMessageStopDraftAction(
            tl_types::SendMessageStopDraftAction { random_id },
        ),
    };
    let _ = live.invoke_rpc(req)?;
    if let Ok(mut guard) = state.lock()
        && let Some(drafts) = guard.streamed_drafts.get_mut(&peer_id)
    {
        // Keep the entry so `stopped_random_ids` suppresses late chunks for `random_id`.
        drafts.apply_stop(random_id);
    }
    live.bridge_state().notify_ui();
    Ok(true)
}

/// Returns and clears adopted streamed draft records (useful for verification and UI transitions).
#[must_use]
pub fn take_adopted_streamed_drafts(live: &LiveSession) -> Vec<(PeerId, AdoptedStreamedDraft)> {
    let state = state_for_live(live);
    state
        .lock()
        .map(|mut g| std::mem::take(&mut g.last_adopted))
        .unwrap_or_default()
}

// ============================================================================
// Compose Stash Live Store
// ============================================================================

/// Returns the stashed compose state for `key` in this live session.
#[must_use]
pub fn compose_stash_live(live: &LiveSession, key: ComposeStashKey) -> Option<ComposeStash> {
    let state = state_for_live(live);
    state
        .lock()
        .ok()
        .and_then(|g| g.compose_stashes.get(key).cloned())
}

/// Saves or clears the stashed compose state for `key` in this live session.
pub fn set_compose_stash_live(
    live: &LiveSession,
    key: ComposeStashKey,
    stash: Option<ComposeStash>,
) {
    let state = state_for_live(live);
    if let Ok(mut guard) = state.lock() {
        guard.compose_stashes.set(key, stash);
    }
    live.bridge_state().notify_ui();
}

// ============================================================================
// Read Metrics Live Batcher
// ============================================================================

/// Enqueues finalized read metrics for `peer_id` into the 5-second session batcher.
pub fn report_read_metrics_live(
    live: &LiveSession,
    peer_id: PeerId,
    metrics: Vec<FinalizedReadMetric>,
) {
    report_read_metrics_live_at(live, peer_id, metrics, monotonic_now_ms());
}

/// Enqueues finalized read metrics at an explicit `now_ms` timestamp.
pub fn report_read_metrics_live_at(
    live: &LiveSession,
    peer_id: PeerId,
    metrics: Vec<FinalizedReadMetric>,
    now_ms: u64,
) {
    let state = state_for_live(live);
    if let Ok(mut guard) = state.lock() {
        guard.read_metrics_batcher.add(peer_id, metrics, now_ms);
    }
}

/// Flushes due (or all, when `force == true`) queued read metrics via `messages.reportReadMetrics`.
///
/// Returns the total number of metrics sent across all flushed peers.
pub fn flush_read_metrics_live(live: &LiveSession, force: bool) -> usize {
    flush_read_metrics_live_at(live, monotonic_now_ms(), force)
}

/// Flushes queued read metrics at an explicit `now_ms` timestamp.
pub fn flush_read_metrics_live_at(live: &LiveSession, now_ms: u64, force: bool) -> usize {
    let state = state_for_live(live);
    let ready = state
        .lock()
        .map(|mut g| g.read_metrics_batcher.drain_ready(now_ms, force))
        .unwrap_or_default();
    let mut sent_total = 0usize;
    for (peer_id, batch) in ready {
        let count = batch.len();
        if send_read_metrics_live(live, peer_id, &batch).is_ok() {
            sent_total += count;
        }
        if let Ok(mut guard) = state.lock() {
            guard.read_metrics_batcher.finish_peer(peer_id, now_ms);
        }
    }
    sent_total
}

// ============================================================================
// Emoji Interactions Live RPCs & Incoming Queue
// ============================================================================

/// Sends an accumulated outgoing emoji tap bunch via `messages.setTyping(sendMessageEmojiInteraction)`.
pub fn send_emoji_interaction_live(
    live: &LiveSession,
    peer_id: PeerId,
    msg_id: MsgId,
    emoticon: &str,
    bunch: &EmojiInteractionsBunch,
) -> Result<(), String> {
    if bunch.interactions.is_empty() {
        return Ok(());
    }
    let Some(input_peer) = live.input_peer(peer_id) else {
        return Err("Peer not found in session.".to_owned());
    };
    let json = interactions_to_json(bunch);
    let req = tl_fns::messages::SetTyping {
        peer: input_peer,
        top_msg_id: None,
        action: tl_enums::SendMessageAction::SendMessageEmojiInteraction(
            tl_types::SendMessageEmojiInteraction {
                emoticon: emoticon.to_owned(),
                msg_id: msg_id.0,
                interaction: tl_enums::DataJSON::DataJSON(tl_types::DataJSON { data: json }),
            },
        ),
    };
    let _ = live.invoke_rpc(req)?;
    Ok(())
}

/// Acknowledges watching an incoming interactive emoji animation via
/// `messages.setTyping(sendMessageEmojiInteractionSeen)`.
pub fn send_emoji_interaction_seen_live(
    live: &LiveSession,
    peer_id: PeerId,
    emoticon: &str,
) -> Result<(), String> {
    let Some(input_peer) = live.input_peer(peer_id) else {
        return Err("Peer not found in session.".to_owned());
    };
    let req = tl_fns::messages::SetTyping {
        peer: input_peer,
        top_msg_id: None,
        action: tl_enums::SendMessageAction::SendMessageEmojiInteractionSeen(
            tl_types::SendMessageEmojiInteractionSeen {
                emoticon: emoticon.to_owned(),
            },
        ),
    };
    let _ = live.invoke_rpc(req)?;
    Ok(())
}

/// Drains incoming remote emoji interaction bunches received for `peer_id`.
#[must_use]
pub fn drain_incoming_emoji_interactions_live(
    live: &LiveSession,
    peer_id: PeerId,
) -> Vec<IncomingEmojiInteraction> {
    let state = state_for_live(live);
    state
        .lock()
        .ok()
        .and_then(|mut g| g.incoming_emoji_interactions.remove(&peer_id))
        .unwrap_or_default()
}

/// Drains incoming `"seen"` emoticon notifications received for `peer_id`.
#[must_use]
pub fn drain_emoji_interaction_seen_live(live: &LiveSession, peer_id: PeerId) -> Vec<String> {
    let state = state_for_live(live);
    state
        .lock()
        .ok()
        .and_then(|mut g| g.incoming_emoji_seen.remove(&peer_id))
        .unwrap_or_default()
}

// ============================================================================
// 1:1 No-Forwards Request & Toggle (`messages.toggleNoForwards`)
// ============================================================================

/// Returns `true` if `peer_id` (a 1:1 user chat) currently has content-sharing disabled by us
/// (`UserFlags::NO_FORWARDS_MY_ENABLED`).
#[must_use]
pub fn peer_no_forwards_enabled_live(live: &LiveSession, peer_id: PeerId) -> bool {
    let data_peer = to_data_peer(peer_id);
    live.read_session(|session| {
        session
            .peer(data_peer)
            .and_then(|p| p.as_user())
            .is_some_and(|u| u.flags.contains(UserFlags::NO_FORWARDS_MY_ENABLED))
    })
}

/// Returns `true` if a `NoForwardsRequest` message at `(peer_id, msg_id)` has already been
/// accepted or declined.
#[must_use]
pub fn is_no_forwards_request_acted_live(
    live: &LiveSession,
    peer_id: PeerId,
    msg_id: MsgId,
) -> bool {
    let state = state_for_live(live);
    state
        .lock()
        .ok()
        .is_some_and(|g| g.no_forwards_acted_msgs.contains(&(peer_id, msg_id)))
}

/// Toggles 1:1 protected-chat content sharing or responds to a `messageActionNoForwardsRequest`
/// via `messages.toggleNoForwards` (`api_suggest_post.cpp:546-557` / `window_peer_menu.cpp:1616`).
pub fn toggle_peer_no_forwards_live(
    live: &LiveSession,
    peer_id: PeerId,
    enabled: bool,
    request_msg_id: Option<MsgId>,
) -> Result<(), String> {
    let Some(input_peer) = live.input_peer(peer_id) else {
        return Err("Peer not found in session.".to_owned());
    };
    let req = tl_fns::messages::ToggleNoForwards {
        peer: input_peer,
        enabled,
        request_msg_id: request_msg_id.map(|m| m.0),
    };
    let updates = live.invoke_rpc(req)?;
    let _ = live.apply_updates_to_session(&updates);

    let data_peer = to_data_peer(peer_id);
    live.write_session(|session| {
        if let Some(peer) = session.peer_mut(data_peer)
            && let Some(user) = peer.as_user_mut()
        {
            user.flags.set(UserFlags::NO_FORWARDS_MY_ENABLED, enabled);
        }
    });
    if let Some(req_id) = request_msg_id {
        let state = state_for_live(live);
        if let Ok(mut guard) = state.lock() {
            guard.no_forwards_acted_msgs.insert((peer_id, req_id));
        }
    }
    live.bridge_state().notify_ui();
    Ok(())
}
