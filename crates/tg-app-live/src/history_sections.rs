//! Live extended history sections, polls, forwarding options, rich drafts, and real-time
//! section updates for [`LiveSession`].
//!
//! Port origin:
//! - `Telegram/SourceFiles/data/data_forum.cpp`, `data/data_forum_topic.cpp`
//! - `Telegram/SourceFiles/data/components/scheduled_messages.cpp`
//! - `Telegram/SourceFiles/data/data_replies_list.cpp`
//! - `Telegram/SourceFiles/history/admin_log/history_admin_log_inner.cpp`
//! - `Telegram/SourceFiles/data/data_saved_messages.cpp`, `data/data_saved_sublist.cpp`
//! - `Telegram/SourceFiles/api/api_polls.cpp`, `data/data_poll.cpp`
//! - `Telegram/SourceFiles/api/api_editing.cpp` (rescheduling)
//! - `Telegram/SourceFiles/data/data_drafts.cpp` (rich per-thread drafts)

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tg_api::RpcClient;
use tg_api::convert::{
    apply_tl_users_and_chats, entities_from_tl, input_peer_for, input_user_for, message_from_tl,
    peer_id_from_tl, text_with_entities_from_tl,
};
use tg_data::{
    ChannelFlags, ChannelId, Draft, DraftKey, FullMsgId as DataFullMsgId, MessageContent,
    MessageMedia, MsgId as DataMsgId, PeerId as DataPeerId, PeerUpdateFlags, PollAnswer,
    PollAnswerVoters, PollData, PollId, PollResults, ReplyInfo, Session, UserId as DataUserId,
    WebPageDraft, scheduled_local_to_remote_id,
};
use tg_tl::{RemoteCall, enums as tl_enums, functions as tl_fns, types as tl_types};
use tokio::sync::{RwLock, broadcast};
use tracing::debug;

use crate::bridge::LiveBridgeState;
use crate::convert::{
    avatar_rgb_for_color_index, data_text_to_ui_text, from_data_msg, from_data_peer,
    history_messages_ordered, initials_from_name, known_peer_ids, message_to_history_message,
    now_unix_i32, peer_display_title, to_data_msg, to_data_peer, ui_text_to_data_text,
    ui_text_to_tl_entities,
};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::core_types::dto::rich_compose::RichThreadDraft;
use tg_app_core::history::backend::{
    HistoryMedia, HistoryMessage, MessageExtra, PollAttachment, PollOption, SendMessageExtended,
};
use tg_app_core::history::sections_model::{
    AdminLogAdmin, AdminLogEntry, AdminLogFilter, AdminLogFilterFlags, CreatePollRequest,
    FORUM_GENERAL_TOPIC_ID, FORUM_TOPIC_ICON_COLORS, ForumTopic, ForumTopicIconEntry,
    ForwardOptions, POLL_MAX_OPTIONS, ReplyQuote, SavedSublist, ScheduleTarget, ThreadKey,
    default_forum_topic_icons, validate_create_poll,
};
use tg_app_core::stand_in::types::{MsgId, PeerId};

/// Default timeout for synchronous history-section RPCs.
const SECTIONS_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Default page limit for `messages.getReplies`, `messages.getSavedHistory`, etc.
const SECTION_HISTORY_LIMIT: i32 = 50;

/// Default page limit for `messages.getForumTopics`, `channels.getAdminLog`, and `messages.getSavedDialogs`.
const SECTION_LIST_LIMIT: i32 = 100;

/// Edit parameters for `messages.editForumTopic` (rename / icon / close / hide).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForumTopicEdit {
    /// New topic title, if renaming.
    pub title: Option<String>,
    /// New custom emoji document ID (`Some(0)` clears custom icon), if changing icon.
    pub icon_emoji_id: Option<i64>,
    /// New closed/locked state, if toggling.
    pub closed: Option<bool>,
    /// New hidden state (for the General topic), if toggling.
    pub hidden: Option<bool>,
}

/// Extended options for `messages.forwardMessages` (drop author, drop captions, silent, schedule, topic, send-as).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LiveForwardOptions {
    /// Hide original author (`drop_author`).
    pub drop_author: bool,
    /// Strip captions from forwarded media (`drop_media_captions`).
    pub drop_media_captions: bool,
    /// Send silently without notification sound (`silent`).
    pub silent: bool,
    /// Optional scheduled Unix timestamp (or `SCHEDULED_UNTIL_ONLINE_TIMESTAMP`).
    pub schedule_date: Option<i32>,
    /// Optional forum topic / thread root ID in the destination chat (`top_msg_id`).
    pub top_msg_id: Option<MsgId>,
    /// Optional peer identity to forward as (`send_as`).
    pub send_as: Option<PeerId>,
}

impl From<ForwardOptions> for LiveForwardOptions {
    fn from(opts: ForwardOptions) -> Self {
        Self {
            drop_author: opts.drop_author,
            drop_media_captions: opts.drop_media_captions,
            ..Self::default()
        }
    }
}

/// Additional poll timing parameters supported by `inputMediaPoll` / `poll`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LivePollOptions {
    /// Optional countdown duration in seconds (`poll.close_period`).
    pub close_period: Option<i32>,
    /// Optional explicit Unix timestamp when the poll closes (`poll.close_date`).
    pub close_date: Option<i32>,
}

/// Read-state snapshot for a channel discussion / replies thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscussionReadState {
    /// Linked discussion supergroup peer ID hosting the comments.
    pub discussion_peer_id: PeerId,
    /// Root forwarded post message ID inside the discussion supergroup.
    pub discussion_root_msg_id: MsgId,
    /// Newest comment message ID, if known.
    pub max_id: Option<MsgId>,
    /// Highest read incoming comment ID (`read_inbox_max_id`).
    pub read_inbox_max_id: Option<MsgId>,
    /// Highest read outgoing comment ID (`read_outbox_max_id`).
    pub read_outbox_max_id: Option<MsgId>,
    /// Unread comment count reported by server or updated by `updateReadChannelDiscussionInbox`.
    pub unread_count: i32,
}

#[derive(Debug, Clone)]
struct DiscussionThreadMeta {
    discussion_peer_id: PeerId,
    discussion_root_msg_id: MsgId,
    root_post: Option<HistoryMessage>,
    max_id: Option<MsgId>,
    read_inbox_max_id: Option<MsgId>,
    read_outbox_max_id: Option<MsgId>,
    unread_count: i32,
    comments_loaded: bool,
    comments: Vec<HistoryMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AdminLogCacheKey {
    flags_bits: Option<u32>,
    admin_ids: Option<Vec<i64>>,
    query: String,
}

impl AdminLogCacheKey {
    fn from_filter(filter: &AdminLogFilter) -> Self {
        let flags_bits = filter.flags.and_then(|f| (!f.is_all()).then_some(f.0));
        let admin_ids = filter.admin_ids.as_ref().map(|ids| {
            let mut v: Vec<i64> = ids.iter().map(|p| p.0).collect();
            v.sort_unstable();
            v.dedup();
            v
        });
        Self {
            flags_bits,
            admin_ids,
            query: filter.search_query.trim().to_owned(),
        }
    }
}

/// Per-session mutable state for extended history sections, synchronized across clones of [`LiveSession`].
struct LiveHistorySectionsState {
    updates_listener_started: bool,
    next_random_id: u64,
    // Forum topics
    forum_topics_loaded: HashSet<PeerId>,
    forum_topics: HashMap<PeerId, Vec<ForumTopic>>,
    forum_pinned_order: HashMap<PeerId, Vec<MsgId>>,
    forum_hidden_topics: HashSet<(PeerId, MsgId)>,
    forum_topic_icons_loaded: bool,
    forum_topic_icons: Vec<ForumTopicIconEntry>,
    topic_messages_loaded: HashSet<(PeerId, MsgId)>,
    topic_messages: HashMap<(PeerId, MsgId), Vec<HistoryMessage>>,
    // Peer member custom ranks / tags (`messages.editChatParticipantRank`)
    member_ranks: HashMap<(PeerId, PeerId), String>,
    // Scheduled messages
    scheduled_loaded: HashSet<PeerId>,
    scheduled_messages: HashMap<PeerId, Vec<HistoryMessage>>,
    // Channel discussion / replies threads keyed by (channel_peer_id, channel_post_msg_id)
    discussion_threads: HashMap<(PeerId, MsgId), DiscussionThreadMeta>,
    // Admin log
    admin_log_cache: HashMap<(PeerId, AdminLogCacheKey), Vec<AdminLogEntry>>,
    admin_log_admins_loaded: HashSet<PeerId>,
    admin_log_admins: HashMap<PeerId, Vec<AdminLogAdmin>>,
    // Saved messages sublists
    saved_sublists_loaded: HashSet<PeerId>,
    saved_sublists: HashMap<PeerId, Vec<SavedSublist>>,
    saved_pinned_order: HashMap<PeerId, Vec<PeerId>>,
    saved_sublist_messages_loaded: HashSet<(PeerId, PeerId)>,
    saved_sublist_messages: HashMap<(PeerId, PeerId), Vec<HistoryMessage>>,
    // Polls index & live voter tracking
    poll_message_index: HashMap<i64, Vec<(DataPeerId, DataMsgId)>>,
    poll_voters_by_option: HashMap<i64, BTreeMap<Vec<u8>, Vec<PeerId>>>,
    // Rich per-thread drafts
    rich_thread_drafts: HashMap<ThreadKey, RichThreadDraft>,
}

impl LiveHistorySectionsState {
    fn new() -> Self {
        Self {
            updates_listener_started: false,
            next_random_id: 1,
            forum_topics_loaded: HashSet::new(),
            forum_topics: HashMap::new(),
            forum_pinned_order: HashMap::new(),
            forum_hidden_topics: HashSet::new(),
            forum_topic_icons_loaded: false,
            forum_topic_icons: Vec::new(),
            topic_messages_loaded: HashSet::new(),
            topic_messages: HashMap::new(),
            member_ranks: HashMap::new(),
            scheduled_loaded: HashSet::new(),
            scheduled_messages: HashMap::new(),
            discussion_threads: HashMap::new(),
            admin_log_cache: HashMap::new(),
            admin_log_admins_loaded: HashSet::new(),
            admin_log_admins: HashMap::new(),
            saved_sublists_loaded: HashSet::new(),
            saved_sublists: HashMap::new(),
            saved_pinned_order: HashMap::new(),
            saved_sublist_messages_loaded: HashSet::new(),
            saved_sublist_messages: HashMap::new(),
            poll_message_index: HashMap::new(),
            poll_voters_by_option: HashMap::new(),
            rich_thread_drafts: HashMap::new(),
        }
    }

    fn next_random_id(&mut self) -> i64 {
        let now = now_unix_i32() as u64;
        self.next_random_id = self.next_random_id.wrapping_add(1);
        ((now << 24) ^ self.next_random_id ^ 0x5EC7_10A5_1234) as i64
    }
}

struct RegistryEntry {
    session_weak: Weak<RwLock<Session>>,
    state: Arc<Mutex<LiveHistorySectionsState>>,
}

fn sections_registry() -> &'static Mutex<HashMap<usize, RegistryEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, RegistryEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn sections_state_for(live: &LiveSession) -> Arc<Mutex<LiveHistorySectionsState>> {
    let session_arc = live.runtime().session();
    let key = Arc::as_ptr(session_arc) as usize;
    let session_weak = Arc::downgrade(session_arc);

    let state = {
        let mut map = sections_registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.retain(|_, entry| entry.session_weak.strong_count() > 0);
        if let Some(entry) = map.get(&key)
            && entry.session_weak.ptr_eq(&session_weak)
        {
            Arc::clone(&entry.state)
        } else {
            let created = Arc::new(Mutex::new(LiveHistorySectionsState::new()));
            map.insert(
                key,
                RegistryEntry {
                    session_weak: session_weak.clone(),
                    state: Arc::clone(&created),
                },
            );
            created
        }
    };

    ensure_updates_listener(live, &state, session_weak);
    state
}

/// Attaches the background updates listener for history sections (scheduled messages, polls,
/// forum topics, discussion read state, saved dialogs) when a [`LiveSession`] is constructed.
pub(crate) fn attach_history_sections_listener(live: &LiveSession) {
    let _ = sections_state_for(live);
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Applies section-specific updates from an RPC-returned `tl_enums::Updates` synchronously.
pub(crate) fn apply_updates_for_live_session(live: &LiveSession, updates: &tl_enums::Updates) {
    let state = sections_state_for(live);
    write_session(live, |session| {
        if let Ok(mut guard) = state.lock() {
            apply_tl_updates_container(session, &mut guard, updates);
        }
    });
    live.bridge_state().notify_ui();
}

fn ensure_updates_listener(
    live: &LiveSession,
    state: &Arc<Mutex<LiveHistorySectionsState>>,
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

    let handle = live.runtime().tokio_handle();
    let updates_rx = {
        let _guard = handle.enter();
        live.runtime().rpc().subscribe_updates()
    };
    let Some(mut rx) = updates_rx else {
        return;
    };

    let state_weak = Arc::downgrade(state);
    let bridge_weak: Weak<LiveBridgeState> = Arc::downgrade(live.bridge_state());

    handle.spawn(async move {
        loop {
            match rx.recv().await {
                Ok(updates) => {
                    let Some(state_arc) = state_weak.upgrade() else {
                        break;
                    };
                    let Some(session_arc) = session_weak.upgrade() else {
                        break;
                    };
                    let changed = {
                        let mut session = session_arc.write().await;
                        let Ok(mut guard) = state_arc.lock() else {
                            continue;
                        };
                        apply_tl_updates_container(&mut session, &mut guard, &updates)
                    };
                    if changed && let Some(bridge) = bridge_weak.upgrade() {
                        bridge.notify_ui();
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    debug!(skipped, "live history sections updates listener lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

// =============================================================================
// Internal RPC & Session helpers
// =============================================================================

fn read_session<R>(live: &LiveSession, f: impl FnOnce(&Session) -> R + Send) -> R
where
    R: Send,
{
    let lock = live.runtime().session();
    if let Ok(guard) = lock.try_read() {
        return f(&guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let guard = arc.read().await;
        f(&guard)
    })
}

fn write_session<R>(live: &LiveSession, f: impl FnOnce(&mut Session) -> R + Send) -> R
where
    R: Send,
{
    let lock = live.runtime().session();
    if let Ok(mut guard) = lock.try_write() {
        return f(&mut guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let mut guard = arc.write().await;
        f(&mut guard)
    })
}

fn invoke_rpc<R>(live: &LiveSession, req: R) -> Result<R::Return, String>
where
    R: RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
{
    let rpc = live.runtime().rpc().clone();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(SECTIONS_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
    })
}

fn input_peer(live: &LiveSession, peer_id: PeerId) -> Option<tl_enums::InputPeer> {
    let data_peer = to_data_peer(peer_id);
    let ip = read_session(live, |s| input_peer_for(s, data_peer));
    (!matches!(ip, tl_enums::InputPeer::Empty)).then_some(ip)
}

fn input_channel(live: &LiveSession, peer_id: PeerId) -> Option<tl_enums::InputChannel> {
    let data_peer = to_data_peer(peer_id);
    read_session(live, |s| {
        let ch = s.peer(data_peer)?.as_channel()?;
        Some(tl_enums::InputChannel::InputChannel(
            tl_types::InputChannel {
                channel_id: ch.channel_id().0 as i64,
                access_hash: ch.access_hash as i64,
            },
        ))
    })
}

fn extract_messages_payload(
    resp: tl_enums::messages::Messages,
) -> (
    Vec<tl_enums::Message>,
    Vec<tl_enums::Chat>,
    Vec<tl_enums::User>,
    Option<i32>,
) {
    match resp {
        tl_enums::messages::Messages::Messages(m) => (m.messages, m.chats, m.users, None),
        tl_enums::messages::Messages::Slice(s) => (s.messages, s.chats, s.users, None),
        tl_enums::messages::Messages::ChannelMessages(c) => {
            (c.messages, c.chats, c.users, (c.pts > 0).then_some(c.pts))
        }
        tl_enums::messages::Messages::NotModified(_) => (Vec::new(), Vec::new(), Vec::new(), None),
    }
}

fn parse_icon_emoji_id(raw: Option<&str>) -> Option<i64> {
    let trimmed = raw?.trim();
    if trimmed.is_empty() {
        return None;
    }
    let digits = trimmed.strip_prefix("custom:").unwrap_or(trimmed);
    digits.parse::<i64>().ok()
}

fn make_reply_to_message(
    reply_to_msg_id: i32,
    top_msg_id: Option<i32>,
    quote: Option<&ReplyQuote>,
    session: &Session,
) -> tl_enums::InputReplyTo {
    let (quote_text, quote_entities, quote_offset) = match quote {
        Some(q) if !q.text.is_empty() => {
            let ui = tg_text::TextWithEntities {
                text: q.text.clone(),
                entities: q.entities.clone(),
            };
            let ents = ui_text_to_tl_entities(&ui, session);
            (
                Some(q.text.clone()),
                (!ents.is_empty()).then_some(ents),
                Some(q.offset_utf16),
            )
        }
        _ => (None, None, None),
    };
    tl_enums::InputReplyTo::Message(tl_types::InputReplyToMessage {
        reply_to_msg_id,
        top_msg_id,
        reply_to_peer_id: None,
        quote_text,
        quote_entities,
        quote_offset,
        monoforum_peer_id: None,
        todo_item_id: None,
        poll_option: None,
        quote_rich: None,
    })
}

// =============================================================================
// Public live methods on LiveSession
// =============================================================================

impl LiveSession {
    /// Returns the tracked channel PTS recorded by `SessionActor`'s single updates handler
    /// (populated by `messages.getForumTopics`, `messages.deleteTopicHistory`,
    /// `messages.search` `ChannelMessages`, dialogs, history, and pushed channel updates).
    #[must_use]
    pub fn updates_channel_pts(&self, channel_id: ChannelId) -> Option<i32> {
        let runtime = Arc::clone(self.runtime());
        block_on_runtime(self.runtime().tokio_handle(), async move {
            runtime
                .persisted_updates_state()
                .await
                .ok()
                .and_then(|s| s.channel_pts.get(&channel_id).copied())
        })
    }

    /// Returns the tracked common (non-channel) PTS recorded by `SessionActor`'s single updates
    /// handler (updated by `messages.deleteSavedHistory`, pushed updates, etc.).
    #[must_use]
    pub fn updates_common_pts(&self) -> i32 {
        let runtime = Arc::clone(self.runtime());
        block_on_runtime(self.runtime().tokio_handle(), async move {
            runtime.persisted_updates_state().await.map_or(0, |s| s.pts)
        })
    }

    /// Seeds initial common and/or channel PTS watermarks on `SessionActor`'s single updates
    /// handler.
    pub fn seed_updates_pts(&self, common_pts: i32, channel_pts: Option<(ChannelId, i32)>) {
        let runtime = Arc::clone(self.runtime());
        let mut persisted = tg_api::PersistedUpdatesState {
            pts: common_pts,
            ..tg_api::PersistedUpdatesState::default()
        };
        if let Some((ch_id, ch_pts)) = channel_pts {
            persisted.channel_pts.insert(ch_id, ch_pts);
        }
        block_on_runtime(self.runtime().tokio_handle(), async move {
            let _ = runtime.restore_updates_state(persisted).await;
        });
    }

    /// Applies a `messages.affectedHistory` `(pts, pts_count)` increment through `SessionActor`'s
    /// single updates handler (`ApiWrap::applyAffectedHistory`, `apiwrap.cpp:2338-2348`).
    pub fn apply_affected_history(
        &self,
        peer_id: Option<PeerId>,
        pts: i32,
        pts_count: i32,
    ) -> Result<(), String> {
        let runtime = Arc::clone(self.runtime());
        let data_peer = peer_id.map(to_data_peer);
        block_on_runtime(self.runtime().tokio_handle(), async move {
            runtime
                .apply_affected_history(data_peer, pts, pts_count)
                .await
                .map_err(|e| e.to_string())
        })
    }

    /// Initializes the PTS watermark for `channel_id` on `SessionActor`'s single updates handler
    /// (`ChannelData::ptsInit`, `data/data_channel.h:480-482`).
    pub fn init_channel_pts(&self, channel_id: ChannelId, pts: i32) -> Result<(), String> {
        let runtime = Arc::clone(self.runtime());
        block_on_runtime(self.runtime().tokio_handle(), async move {
            runtime
                .init_channel_pts(channel_id, pts)
                .await
                .map_err(|e| e.to_string())
        })
    }

    /// Feeds a channel `(pts, pts_count)` into `SessionActor`'s single updates handler
    /// (`ChannelData::ptsReceived` when `pts_count == 0`, `ChannelData::ptsUpdateAndApply`
    /// otherwise; `data/data_channel.h:483-488`).
    pub fn apply_channel_pts(
        &self,
        channel_id: ChannelId,
        pts: i32,
        pts_count: i32,
    ) -> Result<(), String> {
        let runtime = Arc::clone(self.runtime());
        block_on_runtime(self.runtime().tokio_handle(), async move {
            runtime
                .apply_channel_pts(channel_id, pts, pts_count)
                .await
                .map_err(|e| e.to_string())
        })
    }

    // -------------------------------------------------------------------------
    // 1. Forum Topics
    // -------------------------------------------------------------------------

    /// Loads forum topics for `peer_id` via `messages.getForumTopics` (cached after first fetch).
    #[must_use]
    pub fn get_forum_topics_live(&self, peer_id: PeerId) -> Vec<ForumTopic> {
        let is_known_non_forum = read_session(self, |s| {
            s.peer(to_data_peer(peer_id))
                .is_some_and(|p| !p.as_channel().is_some_and(tg_data::Channel::is_forum))
        });
        if is_known_non_forum {
            return Vec::new();
        }
        let state = sections_state_for(self);
        let already_loaded = state
            .lock()
            .map(|g| g.forum_topics_loaded.contains(&peer_id))
            .unwrap_or(true);
        if !already_loaded {
            if let Ok(mut g) = state.lock() {
                g.forum_topics_loaded.insert(peer_id);
            }
            if let Err(err) = self.load_more_forum_topics(peer_id) {
                debug!(peer = peer_id.0, error = %err, "messages.getForumTopics failed");
            }
        }
        state
            .lock()
            .ok()
            .and_then(|g| g.forum_topics.get(&peer_id).cloned())
            .unwrap_or_default()
    }

    /// Loads the next paginated page of forum topics via `messages.getForumTopics`
    /// (`Forum::requestTopics` in `data/data_forum.cpp:175-217`), advancing
    /// `(offset_date, offset_id, offset_topic)`, feeding `resp.pts` into the updates handler,
    /// handling `forumTopicDeleted`, and clearing `ChannelFlags::FORUM` on
    /// `CHANNEL_FORUM_MISSING`.
    ///
    /// Returns `Ok(true)` if more pages remain, or `Ok(false)` when all topics are loaded.
    pub fn load_more_forum_topics(&self, peer_id: PeerId) -> Result<bool, String> {
        if input_peer(self, peer_id).is_none() {
            return Err("Forum peer not found in session.".to_owned());
        }
        let data_peer = to_data_peer(peer_id);
        let now = now_unix_i32();
        let rpc = self.runtime().rpc().clone();
        let session_arc = Arc::clone(self.runtime().session());
        let runtime = Arc::clone(self.runtime());

        let result = block_on_runtime(self.runtime().tokio_handle(), async move {
            let fut = async move {
                let page_opt = {
                    let mut session = session_arc.write().await;
                    tg_api::forum::load_forum_topics_page(
                        &rpc,
                        &mut session,
                        None,
                        data_peer,
                        None,
                        now,
                    )
                    .await?
                };
                // Ported from `Forum::applyReceivedTopics` (`data/data_forum.cpp:390-392`):
                // feed `resp.pts` (`channel->ptsReceived(pts)`) into `SessionActor`'s single
                // `UpdatesHandler`.
                if let Some(ref page) = page_opt
                    && let Some(ch_id) = data_peer.to_channel()
                    && page.pts > 0
                {
                    let _ = runtime.apply_channel_pts(ch_id, page.pts, 0).await;
                }
                Ok(page_opt)
            };
            match tokio::time::timeout(SECTIONS_RPC_TIMEOUT, fut).await {
                Ok(res) => res,
                Err(_) => Err(tg_api::ApiError::Invocation(
                    tg_api::InvocationError::Dropped,
                )),
            }
        });

        match result {
            Ok(Some(page)) => {
                self.sync_forum_topics_cache(peer_id, now);
                Ok(!page.all_loaded)
            }
            Ok(None) => Ok(false),
            Err(err) => {
                if matches!(
                    err,
                    tg_api::ApiError::Invocation(tg_api::InvocationError::Rpc(ref rpc_err))
                        if rpc_err.name == "CHANNEL_FORUM_MISSING"
                ) {
                    let state = sections_state_for(self);
                    if let Ok(mut g) = state.lock() {
                        g.forum_topics.remove(&peer_id);
                        g.forum_pinned_order.remove(&peer_id);
                        g.forum_topics_loaded.insert(peer_id);
                    }
                    self.bridge_state().notify_ui();
                }
                Err(err.to_string())
            }
        }
    }

    /// Fetches specific forum topics by root message ID via `messages.getForumTopicsByID`
    /// (`Forum::requestSomeStale` in `data/data_forum.cpp:454-503`).
    pub fn load_forum_topics_by_id(
        &self,
        peer_id: PeerId,
        topic_ids: &[MsgId],
    ) -> Result<Vec<ForumTopic>, String> {
        if topic_ids.is_empty() {
            return Ok(Vec::new());
        }
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Forum peer not found in session.".to_owned());
        };
        let data_peer = to_data_peer(peer_id);
        let data_ids: Vec<DataMsgId> = topic_ids.iter().copied().map(to_data_msg).collect();
        let now = now_unix_i32();
        let rpc = self.runtime().rpc().clone();
        let session_arc = Arc::clone(self.runtime().session());
        let runtime = Arc::clone(self.runtime());

        let (applied_ids, _deleted_ids) =
            block_on_runtime(self.runtime().tokio_handle(), async move {
                let fut = async move {
                    let mut all_applied = Vec::new();
                    let mut all_deleted = Vec::new();
                    for chunk in data_ids.chunks(tg_data::TOPICS_STALE_PER_REQUEST) {
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
                            .map_err(tg_api::ApiError::from)?;
                        {
                            let mut session = session_arc.write().await;
                            let (applied, deleted, _) =
                                tg_api::forum::apply_tl_forum_topics_response(
                                    &mut session,
                                    None,
                                    data_peer,
                                    &resp,
                                    now,
                                );
                            all_applied.extend(applied);
                            all_deleted.extend(deleted);
                        }
                        // Ported from `Forum::applyReceivedTopics` (`data/data_forum.cpp:390-392`):
                        // feed `resp.pts` (`channel->ptsReceived(pts)`) into `SessionActor`'s
                        // single `UpdatesHandler`.
                        if let Some(ch_id) = data_peer.to_channel()
                            && resp.pts > 0
                        {
                            let _ = runtime.apply_channel_pts(ch_id, resp.pts, 0).await;
                        }
                    }
                    Ok::<_, tg_api::ApiError>((all_applied, all_deleted))
                };
                match tokio::time::timeout(SECTIONS_RPC_TIMEOUT, fut).await {
                    Ok(Ok(res)) => Ok(res),
                    Ok(Err(err)) => Err(err.to_string()),
                    Err(_) => Err("RPC timed out".to_owned()),
                }
            })?;

        self.sync_forum_topics_cache(peer_id, now);
        let topics = read_session(self, |session| {
            applied_ids
                .iter()
                .filter_map(|&root_id| {
                    session
                        .forum_topic(data_peer, root_id)
                        .map(|t| forum_topic_from_data(session, peer_id, t, now))
                })
                .collect()
        });
        Ok(topics)
    }

    /// Queries forum topics with an explicit search query / pagination offsets via `messages.getForumTopics`.
    pub fn search_forum_topics(
        &self,
        peer_id: PeerId,
        query: Option<&str>,
        offset_date: i32,
        offset_id: i32,
        offset_topic: i32,
        limit: i32,
    ) -> Result<Vec<ForumTopic>, String> {
        let q = query
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let cache_full = q.is_none() && offset_date == 0 && offset_id == 0 && offset_topic == 0;
        self.fetch_forum_topics_inner(
            peer_id,
            q,
            offset_date,
            offset_id,
            offset_topic,
            limit.clamp(1, SECTION_LIST_LIMIT),
            cache_full,
        )
    }

    /// Invalidates cached forum topics for `peer_id` so the next call re-fetches from Telegram.
    pub fn reload_forum_topics(&self, peer_id: PeerId) {
        let state = sections_state_for(self);
        if let Ok(mut g) = state.lock() {
            g.forum_topics_loaded.remove(&peer_id);
        }
        write_session(self, |session| {
            session.ensure_forum(to_data_peer(peer_id)).begin_reload();
        });
    }

    fn sync_forum_topics_cache(&self, peer_id: PeerId, now: i32) -> Vec<ForumTopic> {
        let (mut topics, hidden_ids, pinned_ids) = read_session(self, |session| {
            rebuild_forum_topics_from_session(session, peer_id, now)
        });
        let state = sections_state_for(self);
        if let Ok(mut guard) = state.lock() {
            guard.forum_hidden_topics.retain(|&(p, _)| p != peer_id);
            for hid in hidden_ids {
                guard.forum_hidden_topics.insert((peer_id, hid));
            }
            if !pinned_ids.is_empty() {
                guard.forum_pinned_order.insert(peer_id, pinned_ids);
            } else {
                guard.forum_pinned_order.remove(&peer_id);
            }
            let order = guard.forum_pinned_order.get(&peer_id).cloned();
            sort_forum_topics(&mut topics, order.as_deref());
            guard.forum_topics_loaded.insert(peer_id);
            guard.forum_topics.insert(peer_id, topics.clone());
        }
        topics
    }

    /// Returns `true` if `topic_root_id` (e.g. the General topic) is currently marked hidden.
    #[must_use]
    pub fn is_forum_topic_hidden(&self, peer_id: PeerId, topic_root_id: MsgId) -> bool {
        let state = sections_state_for(self);
        state
            .lock()
            .map(|g| g.forum_hidden_topics.contains(&(peer_id, topic_root_id)))
            .unwrap_or(false)
    }

    /// Loads default forum topic custom emoji icons (`Data::ForumIcons` in `data/data_forum_icons.cpp`)
    /// via `messages.getStickerSet(inputStickerSetEmojiDefaultTopicIcons, hash: 0)`.
    pub fn get_forum_topic_icons(&self) -> Result<Vec<ForumTopicIconEntry>, String> {
        let state = sections_state_for(self);
        if let Ok(guard) = state.lock()
            && guard.forum_topic_icons_loaded
            && !guard.forum_topic_icons.is_empty()
        {
            return Ok(guard.forum_topic_icons.clone());
        }
        self.refresh_forum_topic_icons()
    }

    /// Forces a fresh `messages.getStickerSet(inputStickerSetEmojiDefaultTopicIcons)` fetch
    /// and updates the per-session forum topic icon cache.
    pub fn refresh_forum_topic_icons(&self) -> Result<Vec<ForumTopicIconEntry>, String> {
        let resp = invoke_rpc(
            self,
            tl_fns::messages::GetStickerSet {
                stickerset: tl_enums::InputStickerSet::EmojiDefaultTopicIcons,
                hash: 0,
            },
        )?;
        let icons = match resp {
            tl_enums::messages::StickerSet::StickerSet(set) => {
                let tl_enums::StickerSet::StickerSet(meta) = &set.set;
                let set_title = meta.title.clone();
                set.documents
                    .into_iter()
                    .filter_map(|doc_enum| match doc_enum {
                        tl_enums::Document::Document(doc) => {
                            let alt = doc
                                .attributes
                                .iter()
                                .find_map(|attr| match attr {
                                    tl_enums::DocumentAttribute::CustomEmoji(c)
                                        if !c.alt.is_empty() =>
                                    {
                                        Some(c.alt.clone())
                                    }
                                    tl_enums::DocumentAttribute::Sticker(s)
                                        if !s.alt.is_empty() =>
                                    {
                                        Some(s.alt.clone())
                                    }
                                    _ => None,
                                })
                                .unwrap_or_else(|| "💬".to_owned());
                            let is_animated = doc.mime_type == "application/x-tgsticker"
                                || doc.mime_type == "video/webm";
                            Some(ForumTopicIconEntry {
                                document_id: doc.id,
                                alt_emoji: alt,
                                is_animated,
                                title: set_title.clone(),
                            })
                        }
                        tl_enums::Document::Empty(_) => None,
                    })
                    .collect()
            }
            tl_enums::messages::StickerSet::NotModified => {
                let state = sections_state_for(self);
                state
                    .lock()
                    .map(|g| g.forum_topic_icons.clone())
                    .unwrap_or_default()
            }
        };
        let state = sections_state_for(self);
        if let Ok(mut guard) = state.lock() {
            guard.forum_topic_icons_loaded = true;
            guard.forum_topic_icons = icons.clone();
        }
        self.bridge_state().notify_ui();
        Ok(icons)
    }

    /// Returns cached forum topic icons, fetching lazily once if not loaded yet (falling back
    /// to [`default_forum_topic_icons`] on RPC failure so the picker always remains usable).
    #[must_use]
    pub fn forum_topic_icons_live(&self) -> Vec<ForumTopicIconEntry> {
        let state = sections_state_for(self);
        let already_loaded = state
            .lock()
            .map(|g| g.forum_topic_icons_loaded)
            .unwrap_or(true);
        if !already_loaded {
            if let Ok(mut guard) = state.lock() {
                guard.forum_topic_icons_loaded = true;
            }
            if let Err(err) = self.refresh_forum_topic_icons() {
                debug!(error = %err, "messages.getStickerSet(EmojiDefaultTopicIcons) failed");
            }
        }
        let cached = state
            .lock()
            .map(|g| g.forum_topic_icons.clone())
            .unwrap_or_default();
        if cached.is_empty() {
            default_forum_topic_icons()
        } else {
            cached
        }
    }

    /// Updates or clears the custom label of a Saved Messages reaction tag
    /// (`messages.updateSavedReactionTag`).
    pub fn update_saved_reaction_tag(
        &self,
        emoji: &str,
        title: Option<&str>,
    ) -> Result<bool, String> {
        let reaction = if let Some(custom_id) = emoji.trim().strip_prefix("custom:") {
            let doc_id = custom_id
                .parse::<i64>()
                .map_err(|_| "Invalid custom emoji reaction ID.".to_owned())?;
            tl_enums::Reaction::CustomEmoji(tl_types::ReactionCustomEmoji {
                document_id: doc_id,
            })
        } else {
            let trimmed = emoji.trim();
            if trimmed.is_empty() {
                return Err("Reaction emoji cannot be empty.".to_owned());
            }
            tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                emoticon: trimmed.to_owned(),
            })
        };
        let clean_title = title
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(12).collect::<String>());
        let ok = invoke_rpc(
            self,
            tl_fns::messages::UpdateSavedReactionTag {
                reaction,
                title: clean_title,
            },
        )?;
        self.bridge_state().notify_ui();
        Ok(ok)
    }

    /// Tags one or more messages forwarded to Saved Messages (`history_view_self_forwards_tagger.cpp`)
    /// by invoking `messages.sendReaction` on each saved message ID.
    pub fn tag_self_forwarded_messages(
        &self,
        saved_peer_id: PeerId,
        msg_ids: &[MsgId],
        emoji: &str,
    ) -> Result<usize, String> {
        if msg_ids.is_empty() {
            return Ok(0);
        }
        let Some(peer) = input_peer(self, saved_peer_id) else {
            return Err("Saved Messages peer not found in session.".to_owned());
        };
        let reaction = if let Some(custom_id) = emoji.trim().strip_prefix("custom:") {
            let doc_id = custom_id
                .parse::<i64>()
                .map_err(|_| "Invalid custom emoji reaction ID.".to_owned())?;
            tl_enums::Reaction::CustomEmoji(tl_types::ReactionCustomEmoji {
                document_id: doc_id,
            })
        } else {
            let trimmed = emoji.trim();
            if trimmed.is_empty() {
                return Err("Reaction emoji cannot be empty.".to_owned());
            }
            tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                emoticon: trimmed.to_owned(),
            })
        };

        let mut tagged = 0usize;
        for &msg_id in msg_ids {
            let updates = invoke_rpc(
                self,
                tl_fns::messages::SendReaction {
                    big: false,
                    add_to_recent: true,
                    peer: peer.clone(),
                    msg_id: msg_id.0,
                    reaction: Some(vec![reaction.clone()]),
                },
            )?;
            let _ = self.apply_updates_to_session(&updates);
            tagged += 1;
        }
        self.bridge_state().notify_ui();
        Ok(tagged)
    }

    /// Edits a group or supergroup participant's custom rank/tag
    /// (`messages.editChatParticipantRank` / `SaveMemberRank` in `edit_participants_box.cpp`).
    pub fn edit_chat_participant_rank(
        &self,
        peer_id: PeerId,
        user_id: PeerId,
        rank: &str,
    ) -> Result<String, String> {
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Chat peer not found in session.".to_owned());
        };
        let Some(participant) = input_peer(self, user_id) else {
            return Err("Participant peer not found in session.".to_owned());
        };
        let clean_rank = tg_app_core::info::backend::sanitize_peer_tag_rank(rank);
        let updates = invoke_rpc(
            self,
            tl_fns::messages::EditChatParticipantRank {
                peer,
                participant,
                rank: clean_rank.clone(),
            },
        )?;
        let _ = self.apply_updates_to_session(&updates);
        self.cache_member_rank(
            peer_id,
            user_id,
            (!clean_rank.is_empty()).then(|| clean_rank.clone()),
        );
        self.bridge_state().notify_ui();
        Ok(clean_rank)
    }

    /// Updates the cached custom member rank/tag for `(peer_id, user_id)` in both the local
    /// sections cache and `tg_data::Channel::mg_info.member_ranks`.
    pub(crate) fn cache_member_rank(&self, peer_id: PeerId, user_id: PeerId, rank: Option<String>) {
        let clean = rank
            .map(|r| tg_app_core::info::backend::sanitize_peer_tag_rank(&r))
            .filter(|r| !r.is_empty());
        let state = sections_state_for(self);
        if let Ok(mut guard) = state.lock() {
            match &clean {
                Some(r) => {
                    guard.member_ranks.insert((peer_id, user_id), r.clone());
                }
                None => {
                    guard.member_ranks.remove(&(peer_id, user_id));
                }
            }
        }
        let data_peer = to_data_peer(peer_id);
        let data_user = to_data_peer(user_id);
        if let (Some(cid), Some(uid)) = (data_peer.to_channel(), data_user.to_user()) {
            write_session(self, |session| {
                if let Some(ch) = session.channel_mut(cid)
                    && let Some(mg) = ch.mg_info.as_mut()
                {
                    match &clean {
                        Some(r) => {
                            mg.member_ranks.insert(uid, r.clone());
                        }
                        None => {
                            mg.member_ranks.remove(&uid);
                        }
                    }
                }
            });
        }
    }

    /// Returns the cached custom member rank/tag for `(peer_id, user_id)`, if any.
    #[must_use]
    pub fn cached_member_rank(&self, peer_id: PeerId, user_id: PeerId) -> Option<String> {
        let state = sections_state_for(self);
        if let Some(rank) = state
            .lock()
            .ok()
            .and_then(|g| g.member_ranks.get(&(peer_id, user_id)).cloned())
        {
            return Some(rank);
        }
        let data_peer = to_data_peer(peer_id);
        let data_user = to_data_peer(user_id);
        let (Some(cid), Some(uid)) = (data_peer.to_channel(), data_user.to_user()) else {
            return None;
        };
        read_session(self, |session| {
            session
                .channel(cid)
                .and_then(|ch| ch.mg_info.as_ref())
                .and_then(|mg| mg.member_ranks.get(&uid).cloned())
                .filter(|r| !r.is_empty())
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn fetch_forum_topics_inner(
        &self,
        peer_id: PeerId,
        q: Option<String>,
        offset_date: i32,
        offset_id: i32,
        offset_topic: i32,
        limit: i32,
        store_in_cache: bool,
    ) -> Result<Vec<ForumTopic>, String> {
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Forum peer not found in session.".to_owned());
        };
        let data_peer = to_data_peer(peer_id);
        let req = tl_fns::messages::GetForumTopics {
            peer,
            q,
            offset_date,
            offset_id,
            offset_topic,
            limit,
        };
        let rpc = self.runtime().rpc().clone();
        let resp_res = block_on_runtime(self.runtime().tokio_handle(), async move {
            match tokio::time::timeout(SECTIONS_RPC_TIMEOUT, rpc.invoke(&req)).await {
                Ok(r) => r,
                Err(_) => Err(tg_api::InvocationError::Dropped),
            }
        });
        let tl_enums::messages::ForumTopics::ForumTopics(resp) = match resp_res {
            Ok(r) => r,
            Err(err) => {
                // Match `Forum::requestTopics` (`data/data_forum.cpp:210-212`):
                // clear `ChannelFlags::FORUM` on `CHANNEL_FORUM_MISSING`.
                if let tg_api::InvocationError::Rpc(ref rpc_err) = err
                    && rpc_err.name == "CHANNEL_FORUM_MISSING"
                {
                    write_session(self, |session| {
                        if let Some(ch_id) = data_peer.to_channel()
                            && let Some(ch) = session.channel_mut(ch_id)
                        {
                            ch.flags.remove(ChannelFlags::FORUM);
                            session
                                .changes()
                                .peer_updated(data_peer, PeerUpdateFlags::FULL_INFO);
                        }
                        session.ensure_forum(data_peer).all_loaded = true;
                    });
                    let state = sections_state_for(self);
                    if let Ok(mut g) = state.lock() {
                        g.forum_topics.remove(&peer_id);
                        g.forum_pinned_order.remove(&peer_id);
                        g.forum_topics_loaded.insert(peer_id);
                    }
                    self.bridge_state().notify_ui();
                }
                return Err(err.to_string());
            }
        };

        let now = now_unix_i32();
        let runtime = Arc::clone(self.runtime());
        let session_arc = Arc::clone(self.runtime().session());
        let (applied_ids, _deleted_ids) =
            block_on_runtime(self.runtime().tokio_handle(), async move {
                let (applied, deleted) = {
                    let mut session = session_arc.write().await;
                    let (applied, deleted, next_offset) =
                        tg_api::forum::apply_tl_forum_topics_response(
                            &mut session,
                            None,
                            data_peer,
                            &resp,
                            now,
                        );
                    if store_in_cache {
                        let list_len = resp.topics.len() as i32;
                        let forum = session.ensure_forum(data_peer);
                        let prev_offset = forum.offset;
                        forum.offset = next_offset;
                        if resp.topics.is_empty()
                            || list_len == resp.count
                            || next_offset == prev_offset
                        {
                            forum.all_loaded = true;
                        }
                    }
                    (applied, deleted)
                };
                // Ported from `Forum::applyReceivedTopics` (`data/data_forum.cpp:390-392`):
                // feed `resp.pts` (`channel->ptsReceived(pts)`) into `SessionActor`'s single
                // `UpdatesHandler`.
                if let Some(ch_id) = data_peer.to_channel()
                    && resp.pts > 0
                {
                    let _ = runtime.apply_channel_pts(ch_id, resp.pts, 0).await;
                }
                (applied, deleted)
            });

        if store_in_cache {
            Ok(self.sync_forum_topics_cache(peer_id, now))
        } else {
            let topics = read_session(self, |session| {
                applied_ids
                    .iter()
                    .filter_map(|&root_id| {
                        session
                            .forum_topic(data_peer, root_id)
                            .map(|t| forum_topic_from_data(session, peer_id, t, now))
                    })
                    .collect()
            });
            Ok(topics)
        }
    }

    /// Creates a new forum topic via `messages.createForumTopic`.
    pub fn create_forum_topic_live(
        &mut self,
        peer_id: PeerId,
        title: String,
        icon_color: u32,
        custom_emoji: Option<String>,
    ) -> Result<ForumTopic, String> {
        let trimmed = title.trim().to_owned();
        if trimmed.is_empty() {
            return Err("Topic title cannot be empty.".to_owned());
        }
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Forum peer not found in session.".to_owned());
        };
        let state = sections_state_for(self);
        let random_id = state
            .lock()
            .map_err(|_| "history sections state lock poisoned".to_owned())?
            .next_random_id();
        let icon_emoji_id = parse_icon_emoji_id(custom_emoji.as_deref());
        let color = if icon_color == 0 {
            FORUM_TOPIC_ICON_COLORS[0].0
        } else {
            icon_color
        };

        let req = tl_fns::messages::CreateForumTopic {
            title_missing: false,
            peer,
            title: trimmed.clone(),
            icon_color: Some(color as i32),
            icon_emoji_id,
            random_id,
            send_as: None,
        };
        let updates = invoke_rpc(self, req)?;
        let created_id = extract_created_topic_id(&updates).unwrap_or(MsgId(0));

        let self_name = read_session(self, |s| peer_display_title(s, s.self_peer_id()));
        write_session(self, |session| {
            if let Ok(mut guard) = state.lock() {
                apply_tl_updates_container(session, &mut guard, &updates);
            }
        });

        let mut topic = ForumTopic {
            root_id: created_id,
            peer_id,
            title: trimmed,
            icon_color: color,
            custom_emoji: icon_emoji_id.map(|id| id.to_string()).or(custom_emoji),
            is_general: created_id == FORUM_GENERAL_TOPIC_ID,
            is_pinned: false,
            is_closed: false,
            muted: false,
            unread_count: 0,
            unread_mentions: 0,
            last_sender: self_name,
            last_preview: "Topic created".to_owned(),
            last_date_unix: now_unix_i32(),
            draft_preview: None,
        };

        if let Ok(mut guard) = state.lock() {
            let order = guard.forum_pinned_order.get(&peer_id).cloned();
            let list = guard.forum_topics.entry(peer_id).or_default();
            if let Some(existing) = list.iter().find(|t| t.root_id == created_id).cloned() {
                topic = existing;
            } else {
                list.push(topic.clone());
                sort_forum_topics(list, order.as_deref());
            }
            guard.forum_topics_loaded.insert(peer_id);
        }
        self.bridge_state().notify_ui();
        Ok(topic)
    }

    /// Edits a forum topic's title, custom emoji icon, closed state, or hidden state via
    /// `messages.editForumTopic` (`boxes/peers/edit_forum_topic_box.cpp:561-587`,
    /// `data/data_forum_topic.cpp:475-491`).
    ///
    /// Omits `icon_emoji_id` when editing the General topic (`id == 1`) and treats
    /// `TOPIC_NOT_MODIFIED` as a benign no-op.
    pub fn edit_forum_topic(
        &mut self,
        peer_id: PeerId,
        topic_root_id: MsgId,
        edit: ForumTopicEdit,
    ) -> Result<ForumTopic, String> {
        if input_peer(self, peer_id).is_none() {
            return Err("Forum peer not found in session.".to_owned());
        }
        let is_general = topic_root_id == FORUM_GENERAL_TOPIC_ID;
        let effective_icon_emoji_id = if is_general { None } else { edit.icon_emoji_id };
        let data_peer = to_data_peer(peer_id);
        let data_topic_id = to_data_msg(topic_root_id);
        let api_edit = tg_api::forum::ForumTopicEdit {
            title: edit.title.clone(),
            icon_emoji_id: effective_icon_emoji_id,
            closed: edit.closed,
            hidden: edit.hidden,
        };
        let rpc = self.runtime().rpc().clone();
        let session_arc = Arc::clone(self.runtime().session());
        let updates_opt = block_on_runtime(self.runtime().tokio_handle(), async move {
            let fut = async move {
                let session = session_arc.read().await;
                tg_api::forum::edit_forum_topic(&rpc, &session, data_peer, data_topic_id, api_edit)
                    .await
            };
            match tokio::time::timeout(SECTIONS_RPC_TIMEOUT, fut).await {
                Ok(Ok(val)) => Ok(val),
                Ok(Err(err)) => Err(err.to_string()),
                Err(_) => Err("RPC timed out".to_owned()),
            }
        })?;

        let state = sections_state_for(self);
        write_session(self, |session| {
            if let Some(ref updates) = updates_opt
                && let Ok(mut guard) = state.lock()
            {
                apply_tl_updates_container(session, &mut guard, updates);
            }
            if let Some(topic) = session
                .ensure_forum(data_peer)
                .topics
                .get_mut(&data_topic_id)
            {
                if let Some(ref title) = edit.title {
                    topic.title = title.clone();
                }
                if let Some(icon_id) = effective_icon_emoji_id {
                    topic.icon_emoji_id =
                        (icon_id != 0).then_some(tg_data::DocumentId(icon_id as u64));
                }
                if let Some(closed) = edit.closed {
                    topic.closed = closed;
                }
                if let Some(hidden) = edit.hidden {
                    topic.hidden = hidden;
                }
            }
        });

        let updated = if let Ok(mut guard) = state.lock() {
            if let Some(hidden) = edit.hidden {
                if hidden {
                    guard.forum_hidden_topics.insert((peer_id, topic_root_id));
                } else {
                    guard.forum_hidden_topics.remove(&(peer_id, topic_root_id));
                }
            }
            let list = guard.forum_topics.entry(peer_id).or_default();
            if let Some(topic) = list.iter_mut().find(|t| t.root_id == topic_root_id) {
                if let Some(ref title) = edit.title {
                    topic.title = title.clone();
                }
                if let Some(icon_id) = effective_icon_emoji_id {
                    topic.custom_emoji = (icon_id != 0).then(|| icon_id.to_string());
                }
                if let Some(closed) = edit.closed {
                    topic.is_closed = closed;
                }
                topic.clone()
            } else {
                let topic = ForumTopic {
                    root_id: topic_root_id,
                    peer_id,
                    title: edit
                        .title
                        .unwrap_or_else(|| format!("Topic #{}", topic_root_id.0)),
                    icon_color: FORUM_TOPIC_ICON_COLORS[0].0,
                    custom_emoji: effective_icon_emoji_id
                        .filter(|&id| id != 0)
                        .map(|id| id.to_string()),
                    is_general,
                    is_pinned: false,
                    is_closed: edit.closed.unwrap_or(false),
                    muted: false,
                    unread_count: 0,
                    unread_mentions: 0,
                    last_sender: String::new(),
                    last_preview: String::new(),
                    last_date_unix: now_unix_i32(),
                    draft_preview: None,
                };
                list.push(topic.clone());
                topic
            }
        } else {
            return Err("history sections state lock poisoned".to_owned());
        };

        self.bridge_state().notify_ui();
        Ok(updated)
    }

    /// Toggles closed/locked state of a forum topic via `messages.editForumTopic`.
    pub fn toggle_forum_topic_closed_live(
        &mut self,
        peer_id: PeerId,
        topic_root_id: MsgId,
    ) -> Result<bool, String> {
        let state = sections_state_for(self);
        let current = state
            .lock()
            .ok()
            .and_then(|g| {
                g.forum_topics
                    .get(&peer_id)?
                    .iter()
                    .find(|t| t.root_id == topic_root_id)
                    .map(|t| t.is_closed)
            })
            .unwrap_or(false);
        let next = !current;
        let updated = self.edit_forum_topic(
            peer_id,
            topic_root_id,
            ForumTopicEdit {
                closed: Some(next),
                ..ForumTopicEdit::default()
            },
        )?;
        Ok(updated.is_closed)
    }

    /// Explicitly pins or unpins a forum topic via `messages.updatePinnedForumTopic`.
    pub fn set_forum_topic_pinned(
        &mut self,
        peer_id: PeerId,
        topic_root_id: MsgId,
        pinned: bool,
    ) -> Result<bool, String> {
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Forum peer not found in session.".to_owned());
        };
        let req = tl_fns::messages::UpdatePinnedForumTopic {
            peer,
            topic_id: topic_root_id.0,
            pinned,
        };
        let updates = invoke_rpc(self, req)?;
        let state = sections_state_for(self);
        write_session(self, |session| {
            if let Ok(mut guard) = state.lock() {
                apply_tl_updates_container(session, &mut guard, &updates);
                apply_forum_topic_pin_state(&mut guard, peer_id, topic_root_id, pinned);
            }
        });
        self.bridge_state().notify_ui();
        Ok(pinned)
    }

    /// Toggles pin state of a forum topic via `messages.updatePinnedForumTopic`.
    pub fn toggle_forum_topic_pin_live(
        &mut self,
        peer_id: PeerId,
        topic_root_id: MsgId,
    ) -> Result<bool, String> {
        let state = sections_state_for(self);
        let current = state
            .lock()
            .ok()
            .and_then(|g| {
                g.forum_topics
                    .get(&peer_id)?
                    .iter()
                    .find(|t| t.root_id == topic_root_id)
                    .map(|t| t.is_pinned)
            })
            .unwrap_or(false);
        self.set_forum_topic_pinned(peer_id, topic_root_id, !current)
    }

    /// Reorders pinned forum topics via `messages.reorderPinnedForumTopics`.
    pub fn reorder_pinned_forum_topics(
        &mut self,
        peer_id: PeerId,
        order: &[MsgId],
        force: bool,
    ) -> Result<(), String> {
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Forum peer not found in session.".to_owned());
        };
        let req = tl_fns::messages::ReorderPinnedForumTopics {
            force,
            peer,
            order: order.iter().map(|m| m.0).collect(),
        };
        let updates = invoke_rpc(self, req)?;
        let state = sections_state_for(self);
        write_session(self, |session| {
            if let Ok(mut guard) = state.lock() {
                apply_tl_updates_container(session, &mut guard, &updates);
                apply_forum_pinned_order(&mut guard, peer_id, order.to_vec());
            }
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    /// Deletes a forum topic and all messages in it via `messages.deleteTopicHistory`
    /// (`PeerMenuDeleteTopic` in `window/window_peer_menu.cpp:2325-2341`), feeding each
    /// `messages.affectedHistory` `(pts, pts_count)` page into the updates handler
    /// (`applyAffectedHistory`).
    pub fn delete_topic_history(
        &mut self,
        peer_id: PeerId,
        topic_root_id: MsgId,
    ) -> Result<i32, String> {
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Forum peer not found in session.".to_owned());
        };
        let data_peer = to_data_peer(peer_id);
        let data_topic_id = to_data_msg(topic_root_id);
        let rpc = self.runtime().rpc().clone();
        let session_arc = Arc::clone(self.runtime().session());
        let runtime = Arc::clone(self.runtime());

        let pages = block_on_runtime(self.runtime().tokio_handle(), async move {
            let fut = async move {
                let mut pages = Vec::new();
                for _ in 0..tg_api::forum::MAX_DELETE_TOPIC_HISTORY_LOOPS {
                    let tl_enums::messages::AffectedHistory::AffectedHistory(aff) = rpc
                        .invoke(&tl_fns::messages::DeleteTopicHistory {
                            peer: peer.clone(),
                            top_msg_id: data_topic_id.as_i32().unwrap_or(0),
                        })
                        .await
                        .map_err(tg_api::ApiError::from)?;
                    // Ported from `PeerMenuDeleteTopic` (`window/window_peer_menu.cpp:2325-2341`)
                    // and `ApiWrap::applyAffectedHistory` (`apiwrap.cpp:2338-2348`): feed each
                    // page's `(pts, pts_count)` into `SessionActor`'s single `UpdatesHandler`.
                    let _ = runtime
                        .apply_affected_history(Some(data_peer), aff.pts, aff.pts_count)
                        .await;
                    let next_offset = aff.offset;
                    pages.push(aff);
                    if next_offset <= 0 {
                        break;
                    }
                }
                {
                    let mut session = session_arc.write().await;
                    session.delete_forum_topic(data_peer, data_topic_id, false);
                }
                Ok::<_, tg_api::ApiError>(pages)
            };
            match tokio::time::timeout(SECTIONS_RPC_TIMEOUT, fut).await {
                Ok(Ok(pages)) => Ok(pages),
                Ok(Err(err)) => Err(err.to_string()),
                Err(_) => Err("RPC timed out".to_owned()),
            }
        })?;
        let last_pts = pages.last().map_or(0, |a| a.pts);

        let state = sections_state_for(self);
        if let Ok(mut guard) = state.lock() {
            if let Some(list) = guard.forum_topics.get_mut(&peer_id) {
                list.retain(|t| t.root_id != topic_root_id);
            }
            if let Some(order) = guard.forum_pinned_order.get_mut(&peer_id) {
                order.retain(|&id| id != topic_root_id);
            }
            guard.forum_hidden_topics.remove(&(peer_id, topic_root_id));
            guard.topic_messages.remove(&(peer_id, topic_root_id));
            guard
                .topic_messages_loaded
                .remove(&(peer_id, topic_root_id));
        }
        self.bridge_state().notify_ui();
        Ok(last_pts)
    }

    /// Loads messages belonging to a forum topic via `messages.getReplies`.
    #[must_use]
    pub fn load_topic_messages_live(
        &self,
        peer_id: PeerId,
        topic_root_id: MsgId,
    ) -> Vec<HistoryMessage> {
        let state = sections_state_for(self);
        let key = (peer_id, topic_root_id);
        let already_loaded = state
            .lock()
            .map(|g| g.topic_messages_loaded.contains(&key))
            .unwrap_or(true);
        if !already_loaded {
            if let Ok(mut g) = state.lock() {
                g.topic_messages_loaded.insert(key);
            }
            if let Some(peer) = input_peer(self, peer_id) {
                let req = tl_fns::messages::GetReplies {
                    peer,
                    msg_id: topic_root_id.0,
                    offset_id: 0,
                    offset_date: 0,
                    add_offset: 0,
                    limit: SECTION_HISTORY_LIMIT,
                    max_id: 0,
                    min_id: 0,
                    hash: 0,
                };
                match invoke_rpc(self, req) {
                    Ok(resp) => {
                        let (raw_msgs, chats, users, channel_pts) = extract_messages_payload(resp);
                        if let Some(pts) = channel_pts
                            && let Some(ch_id) = to_data_peer(peer_id).to_channel()
                        {
                            let _ = self.apply_channel_pts(ch_id, pts, 0);
                        }
                        let mut converted = write_session(self, |session| {
                            apply_tl_users_and_chats(session, &users, &chats);
                            let self_uid = session.self_user_id();
                            let mut out = Vec::with_capacity(raw_msgs.len());
                            for raw in &raw_msgs {
                                if let Some(dm) = message_from_tl(raw, self_uid) {
                                    let mut hm = message_to_history_message(session, &dm);
                                    hm.extra.thread_id = Some(topic_root_id);
                                    session.insert_message(dm);
                                    out.push(hm);
                                }
                            }
                            out
                        });
                        converted.sort_by_key(|m| m.id.0);
                        if let Ok(mut g) = state.lock() {
                            g.topic_messages.insert(key, converted);
                        }
                    }
                    Err(err) => {
                        debug!(
                            peer = peer_id.0,
                            topic = topic_root_id.0,
                            error = %err,
                            "messages.getReplies for forum topic failed"
                        );
                    }
                }
            }
        }
        state
            .lock()
            .ok()
            .and_then(|g| g.topic_messages.get(&key).cloned())
            .unwrap_or_default()
    }

    // -------------------------------------------------------------------------
    // 2. Scheduled Messages
    // -------------------------------------------------------------------------

    /// Refreshes scheduled messages for `peer_id` via `messages.getScheduledHistory`
    /// (`ScheduledMessages::request` / `parse` in `data/components/scheduled_messages.cpp:489-547`),
    /// sending `ScheduledMessagesStore::count_list_hash(peer_id)` and honoring the 60-second
    /// `SCHEDULED_REQUEST_TIME_LIMIT_MS` cooldown unless `force == true`.
    pub fn refresh_scheduled_messages(
        &self,
        peer_id: PeerId,
        now_ms: u64,
        force: bool,
    ) -> Result<tg_api::scheduled::ScheduledHistoryOutcome, String> {
        if input_peer(self, peer_id).is_none() {
            return Err("Peer not found in session.".to_owned());
        }
        let data_peer = to_data_peer(peer_id);
        let rpc = self.runtime().rpc().clone();
        let session_arc = Arc::clone(self.runtime().session());
        let outcome = block_on_runtime(self.runtime().tokio_handle(), async move {
            let fut = async move {
                let mut session = session_arc.write().await;
                tg_api::scheduled::get_scheduled_history(
                    &rpc,
                    &mut session,
                    data_peer,
                    now_ms,
                    force,
                )
                .await
            };
            match tokio::time::timeout(SECTIONS_RPC_TIMEOUT, fut).await {
                Ok(Ok(outcome)) => Ok(outcome),
                Ok(Err(err)) => Err(err.to_string()),
                Err(_) => Err("RPC timed out".to_owned()),
            }
        })?;
        self.sync_scheduled_messages_cache(peer_id);
        Ok(outcome)
    }

    fn sync_scheduled_messages_cache(&self, peer_id: PeerId) -> Vec<HistoryMessage> {
        let data_peer = to_data_peer(peer_id);
        let mut converted = read_session(self, |session| {
            session
                .scheduled_messages()
                .list(data_peer)
                .iter()
                .map(|dm| scheduled_history_message_from_data(session, dm))
                .collect::<Vec<_>>()
        });
        sort_scheduled_messages(&mut converted);
        let state = sections_state_for(self);
        if let Ok(mut g) = state.lock() {
            g.scheduled_loaded.insert(peer_id);
            g.scheduled_messages.insert(peer_id, converted.clone());
        }
        converted
    }

    /// Loads scheduled messages for `peer_id` (optionally filtered to `topic_root_id`) via
    /// `messages.getScheduledHistory` (`ScheduledMessages::request`,
    /// `data/components/scheduled_messages.cpp:489-547`).
    #[must_use]
    pub fn scheduled_messages_live(
        &self,
        peer_id: PeerId,
        topic_root_id: Option<MsgId>,
    ) -> Vec<HistoryMessage> {
        let state = sections_state_for(self);
        let already_loaded = state
            .lock()
            .map(|g| g.scheduled_loaded.contains(&peer_id))
            .unwrap_or(true);
        if !already_loaded {
            if let Ok(mut g) = state.lock() {
                g.scheduled_loaded.insert(peer_id);
            }
            if let Err(err) = self.refresh_scheduled_messages(peer_id, now_unix_ms(), false) {
                debug!(peer = peer_id.0, error = %err, "messages.getScheduledHistory failed");
            }
        }

        let all = state
            .lock()
            .ok()
            .and_then(|g| g.scheduled_messages.get(&peer_id).cloned())
            .unwrap_or_default();
        match topic_root_id {
            Some(tid) => all
                .into_iter()
                .filter(|m| m.extra.thread_id == Some(tid))
                .collect(),
            None => all,
        }
    }

    /// Immediately sends a scheduled message via `messages.sendScheduledMessages`.
    pub fn send_scheduled_now_live(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Result<HistoryMessage, String> {
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Peer not found in session.".to_owned());
        };
        let state = sections_state_for(self);
        let cached_msg = state.lock().ok().and_then(|g| {
            g.scheduled_messages
                .get(&peer_id)?
                .iter()
                .find(|m| m.id == msg_id)
                .cloned()
        });

        let updates = invoke_rpc(
            self,
            tl_fns::messages::SendScheduledMessages {
                peer,
                id: vec![msg_id.0],
            },
        )?;

        let data_peer = to_data_peer(peer_id);
        let sent_ids = self.apply_updates_to_session(&updates);
        write_session(self, |session| {
            session.scheduled_messages_mut().apply_delete_scheduled(
                data_peer,
                &[to_data_msg(msg_id)],
                None,
            );
            if let Ok(mut guard) = state.lock()
                && let Some(list) = guard.scheduled_messages.get_mut(&peer_id)
            {
                list.retain(|m| m.id != msg_id);
            }
        });

        let result = read_session(self, |session| {
            sent_ids
                .into_iter()
                .rev()
                .find_map(|mid| {
                    session
                        .message(DataFullMsgId::new(data_peer, mid))
                        .map(|m| message_to_history_message(session, m))
                })
                .or_else(|| {
                    cached_msg.map(|mut m| {
                        m.extra.scheduled_date = None;
                        m.date_unix = now_unix_i32();
                        m
                    })
                })
        })
        .ok_or_else(|| {
            "Scheduled message sent, but resulting message was unavailable.".to_owned()
        })?;

        self.bridge_state().notify_ui();
        Ok(result)
    }

    /// Updates the scheduled delivery date/time via `messages.editMessage` (`schedule_date`).
    pub fn reschedule_message_live(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        target: ScheduleTarget,
    ) -> Result<HistoryMessage, String> {
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Peer not found in session.".to_owned());
        };
        let state = sections_state_for(self);
        let cached_msg = state.lock().ok().and_then(|g| {
            g.scheduled_messages
                .get(&peer_id)?
                .iter()
                .find(|m| m.id == msg_id)
                .cloned()
        });

        let (message_opt, entities_opt) = match &cached_msg {
            Some(m) if !m.text.text.is_empty() => {
                let ents = read_session(self, |s| ui_text_to_tl_entities(&m.text, s));
                (
                    Some(m.text.text.clone()),
                    (!ents.is_empty()).then_some(ents),
                )
            }
            _ => (None, None),
        };

        let new_ts = target.to_time_id();
        let req = tl_fns::messages::EditMessage {
            no_webpage: false,
            invert_media: false,
            peer,
            id: msg_id.0,
            message: message_opt,
            media: None,
            reply_markup: None,
            entities: entities_opt,
            schedule_date: Some(new_ts),
            schedule_repeat_period: None,
            quick_reply_shortcut_id: None,
            rich_message: None,
        };
        let maybe_updates = self.edit_message_via_runtime(req)?;
        if let Some(ref updates) = maybe_updates {
            self.apply_updates_side_caches(updates);
        }

        let data_peer = to_data_peer(peer_id);
        write_session(self, |session| {
            if let Some(mut existing) = session
                .scheduled_messages()
                .lookup_remote(data_peer, to_data_msg(msg_id))
                .cloned()
            {
                existing.id = to_data_msg(msg_id);
                existing.date = new_ts;
                let _ = session
                    .scheduled_messages_mut()
                    .upsert_server_message(data_peer, existing);
            }
            if let Ok(mut guard) = state.lock() {
                let list = guard.scheduled_messages.entry(peer_id).or_default();
                if let Some(item) = list.iter_mut().find(|m| m.id == msg_id) {
                    item.date_unix = new_ts;
                    item.extra.scheduled_date = Some(new_ts);
                } else if let Some(mut item) = cached_msg.clone() {
                    item.date_unix = new_ts;
                    item.extra.scheduled_date = Some(new_ts);
                    list.push(item);
                }
                sort_scheduled_messages(list);
            }
        });

        let updated = state
            .lock()
            .ok()
            .and_then(|g| {
                g.scheduled_messages
                    .get(&peer_id)?
                    .iter()
                    .find(|m| m.id == msg_id)
                    .cloned()
            })
            .or_else(|| {
                cached_msg.map(|mut m| {
                    m.date_unix = new_ts;
                    m.extra.scheduled_date = Some(new_ts);
                    m
                })
            })
            .ok_or_else(|| "Scheduled message not found after rescheduling.".to_owned())?;

        self.bridge_state().notify_ui();
        Ok(updated)
    }

    /// Deletes one or more scheduled messages via `messages.deleteScheduledMessages`.
    pub fn delete_scheduled_messages_live(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
    ) -> Result<(), String> {
        if msg_ids.is_empty() {
            return Ok(());
        }
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Peer not found in session.".to_owned());
        };
        let req = tl_fns::messages::DeleteScheduledMessages {
            peer,
            id: msg_ids.iter().map(|m| m.0).collect(),
        };
        let updates = invoke_rpc(self, req)?;
        let state = sections_state_for(self);
        let id_set: HashSet<MsgId> = msg_ids.iter().copied().collect();
        let data_peer = to_data_peer(peer_id);
        let remote_ids: Vec<DataMsgId> = msg_ids.iter().copied().map(to_data_msg).collect();
        write_session(self, |session| {
            session
                .scheduled_messages_mut()
                .apply_delete_scheduled(data_peer, &remote_ids, None);
            if let Ok(mut guard) = state.lock() {
                apply_tl_updates_container(session, &mut guard, &updates);
                if let Some(list) = guard.scheduled_messages.get_mut(&peer_id) {
                    list.retain(|m| !id_set.contains(&m.id));
                }
            }
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    // -------------------------------------------------------------------------
    // 3. Channel Comments / Replies Threads
    // -------------------------------------------------------------------------

    /// Loads the root channel post and its discussion comments via `messages.getDiscussionMessage`
    /// followed by `messages.getReplies`.
    #[must_use]
    pub fn replies_root_and_comments_live(
        &self,
        peer_id: PeerId,
        root_msg_id: MsgId,
    ) -> Option<(HistoryMessage, Vec<HistoryMessage>)> {
        let state = sections_state_for(self);
        let key = (peer_id, root_msg_id);
        let already_loaded = state
            .lock()
            .ok()
            .and_then(|g| g.discussion_threads.get(&key).map(|m| m.comments_loaded))
            .unwrap_or(false);

        if !already_loaded {
            self.load_discussion_thread_inner(peer_id, root_msg_id);
        }

        let guard = state.lock().ok()?;
        let meta = guard.discussion_threads.get(&key)?;
        let root = meta.root_post.clone()?;
        Some((root, meta.comments.clone()))
    }

    /// Returns the cached read-state metadata for a channel comments / discussion thread.
    #[must_use]
    pub fn discussion_read_state(
        &self,
        peer_id: PeerId,
        root_msg_id: MsgId,
    ) -> Option<DiscussionReadState> {
        let state = sections_state_for(self);
        let guard = state.lock().ok()?;
        let meta = guard.discussion_threads.get(&(peer_id, root_msg_id))?;
        Some(DiscussionReadState {
            discussion_peer_id: meta.discussion_peer_id,
            discussion_root_msg_id: meta.discussion_root_msg_id,
            max_id: meta.max_id,
            read_inbox_max_id: meta.read_inbox_max_id,
            read_outbox_max_id: meta.read_outbox_max_id,
            unread_count: meta.unread_count,
        })
    }

    /// Updates local cached discussion thread and forum topic read state after
    /// `messages.readDiscussion` succeeds (`WP-53`).
    pub(crate) fn mark_discussion_thread_read_local(
        &self,
        peer_id: PeerId,
        top_msg_id: MsgId,
        read_max_id: MsgId,
    ) {
        let state = sections_state_for(self);
        if let Ok(mut guard) = state.lock() {
            for (&(chan_peer, chan_root), meta) in &mut guard.discussion_threads {
                let matches_direct =
                    meta.discussion_peer_id == peer_id && meta.discussion_root_msg_id == top_msg_id;
                let matches_channel = chan_peer == peer_id && chan_root == top_msg_id;
                if matches_direct || matches_channel {
                    let next_max = meta
                        .read_inbox_max_id
                        .map_or(read_max_id, |prev| MsgId(prev.0.max(read_max_id.0)));
                    meta.read_inbox_max_id = Some(next_max);
                    if meta.max_id.is_none_or(|max_id| next_max.0 >= max_id.0) {
                        meta.unread_count = 0;
                    } else if !meta.comments.is_empty() {
                        meta.unread_count = meta
                            .comments
                            .iter()
                            .filter(|m| !m.outgoing && m.id.0 > next_max.0)
                            .count() as i32;
                    } else {
                        meta.unread_count = 0;
                    }
                }
            }
            if let Some(topics) = guard.forum_topics.get_mut(&peer_id)
                && let Some(topic) = topics.iter_mut().find(|t| t.root_id == top_msg_id)
            {
                topic.unread_count = 0;
            }
        }
    }

    fn load_discussion_thread_inner(&self, peer_id: PeerId, root_msg_id: MsgId) {
        let Some(peer) = input_peer(self, peer_id) else {
            return;
        };
        let state = sections_state_for(self);
        let key = (peer_id, root_msg_id);

        let dm_resp = match invoke_rpc(
            self,
            tl_fns::messages::GetDiscussionMessage {
                peer,
                msg_id: root_msg_id.0,
            },
        ) {
            Ok(tl_enums::messages::DiscussionMessage::DiscussionMessage(dm)) => dm,
            Err(err) => {
                debug!(
                    peer = peer_id.0,
                    msg = root_msg_id.0,
                    error = %err,
                    "messages.getDiscussionMessage failed"
                );
                return;
            }
        };

        let (discussion_peer_id, discussion_root_msg_id, root_post) =
            write_session(self, |session| {
                apply_tl_users_and_chats(session, &dm_resp.users, &dm_resp.chats);
                let self_uid = session.self_user_id();
                let mut disc_peer = peer_id;
                let mut disc_root = root_msg_id;
                let mut fallback_root = None;

                if let Some(first_raw) = dm_resp.messages.first()
                    && let Some(dm) = message_from_tl(first_raw, self_uid)
                {
                    disc_peer = from_data_peer(dm.peer_id);
                    disc_root = from_data_msg(dm.id);
                    let hm = message_to_history_message(session, &dm);
                    session.insert_message(dm);
                    fallback_root = Some(hm);
                }

                let channel_root = session
                    .message(DataFullMsgId::new(
                        to_data_peer(peer_id),
                        to_data_msg(root_msg_id),
                    ))
                    .map(|m| message_to_history_message(session, m));

                (disc_peer, disc_root, channel_root.or(fallback_root))
            });

        let mut comments = Vec::new();
        if let Some(disc_input) = input_peer(self, discussion_peer_id) {
            let replies_req = tl_fns::messages::GetReplies {
                peer: disc_input,
                msg_id: discussion_root_msg_id.0,
                offset_id: 0,
                offset_date: 0,
                add_offset: 0,
                limit: SECTION_HISTORY_LIMIT,
                max_id: 0,
                min_id: 0,
                hash: 0,
            };
            match invoke_rpc(self, replies_req) {
                Ok(resp) => {
                    let (raw_msgs, chats, users, channel_pts) = extract_messages_payload(resp);
                    if let Some(pts) = channel_pts
                        && let Some(ch_id) = to_data_peer(discussion_peer_id).to_channel()
                    {
                        let _ = self.apply_channel_pts(ch_id, pts, 0);
                    }
                    comments = write_session(self, |session| {
                        apply_tl_users_and_chats(session, &users, &chats);
                        let self_uid = session.self_user_id();
                        let mut out = Vec::with_capacity(raw_msgs.len());
                        for raw in &raw_msgs {
                            if let Some(dm) = message_from_tl(raw, self_uid) {
                                let mut hm = message_to_history_message(session, &dm);
                                hm.extra.thread_id = Some(root_msg_id);
                                if let Some(outbox_max) = dm_resp.read_outbox_max_id
                                    && hm.outgoing
                                {
                                    hm.is_read = hm.id.0 <= outbox_max;
                                }
                                session.insert_message(dm);
                                out.push(hm);
                            }
                        }
                        out
                    });
                    comments.sort_by_key(|m| m.id.0);
                }
                Err(err) => {
                    debug!(
                        disc_peer = discussion_peer_id.0,
                        disc_root = discussion_root_msg_id.0,
                        error = %err,
                        "messages.getReplies for discussion thread failed"
                    );
                }
            }
        }

        if let Ok(mut guard) = state.lock() {
            guard.discussion_threads.insert(
                key,
                DiscussionThreadMeta {
                    discussion_peer_id,
                    discussion_root_msg_id,
                    root_post,
                    max_id: dm_resp.max_id.map(MsgId),
                    read_inbox_max_id: dm_resp.read_inbox_max_id.map(MsgId),
                    read_outbox_max_id: dm_resp.read_outbox_max_id.map(MsgId),
                    unread_count: dm_resp.unread_count,
                    comments_loaded: true,
                    comments,
                },
            );
        }
    }

    // -------------------------------------------------------------------------
    // 4. Channel Admin Log / Recent Actions
    // -------------------------------------------------------------------------

    /// Loads filtered channel admin log entries via `channels.getAdminLog`.
    #[must_use]
    pub fn admin_log_entries_live(
        &self,
        peer_id: PeerId,
        filter: &AdminLogFilter,
    ) -> Vec<AdminLogEntry> {
        let state = sections_state_for(self);
        let cache_key = (peer_id, AdminLogCacheKey::from_filter(filter));
        if let Ok(g) = state.lock()
            && let Some(cached) = g.admin_log_cache.get(&cache_key)
        {
            return cached.clone();
        }

        let Some(channel) = input_channel(self, peer_id) else {
            return Vec::new();
        };

        // Explicit empty admin list means "no admin matches" (matches `AdminLogFilter::matches`).
        if filter.admin_ids.as_deref() == Some(&[]) {
            return Vec::new();
        }

        let events_filter = filter.flags.and_then(|f| {
            if f.is_all() {
                None
            } else {
                Some(admin_log_flags_to_tl(f))
            }
        });
        let admins = filter.admin_ids.as_ref().map(|ids| {
            read_session(self, |session| {
                ids.iter()
                    .filter_map(|&pid| {
                        let uid = to_data_peer(pid).to_user()?;
                        let iu = input_user_for(session, uid);
                        (!matches!(iu, tl_enums::InputUser::Empty)).then_some(iu)
                    })
                    .collect::<Vec<_>>()
            })
        });

        let req = tl_fns::channels::GetAdminLog {
            channel,
            q: filter.search_query.trim().to_owned(),
            events_filter,
            admins,
            max_id: 0,
            min_id: 0,
            limit: SECTION_LIST_LIMIT,
        };
        let entries = match invoke_rpc(self, req) {
            Ok(tl_enums::channels::AdminLogResults::AdminLogResults(res)) => {
                write_session(self, |session| {
                    apply_tl_users_and_chats(session, &res.users, &res.chats);
                    res.events
                        .iter()
                        .map(|ev| admin_log_event_from_tl(session, ev))
                        .collect::<Vec<_>>()
                })
            }
            Err(err) => {
                debug!(peer = peer_id.0, error = %err, "channels.getAdminLog failed");
                Vec::new()
            }
        };

        if let Ok(mut g) = state.lock() {
            g.admin_log_cache.insert(cache_key, entries.clone());
        }
        entries
    }

    /// Loads channel administrators for the Admin Log Filter box via
    /// `channels.getParticipants(channelParticipantsAdmins)`.
    #[must_use]
    pub fn admin_log_admins_live(&self, peer_id: PeerId) -> Vec<AdminLogAdmin> {
        let state = sections_state_for(self);
        let already_loaded = state
            .lock()
            .map(|g| g.admin_log_admins_loaded.contains(&peer_id))
            .unwrap_or(true);
        if !already_loaded {
            if let Ok(mut g) = state.lock() {
                g.admin_log_admins_loaded.insert(peer_id);
            }
            if let Some(channel) = input_channel(self, peer_id) {
                let req = tl_fns::channels::GetParticipants {
                    channel,
                    filter: tl_enums::ChannelParticipantsFilter::ChannelParticipantsAdmins,
                    offset: 0,
                    limit: 200,
                    hash: 0,
                };
                match invoke_rpc(self, req) {
                    Ok(tl_enums::channels::ChannelParticipants::ChannelParticipants(cp)) => {
                        let admins = write_session(self, |session| {
                            apply_tl_users_and_chats(session, &cp.users, &cp.chats);
                            cp.participants
                                .iter()
                                .filter_map(|p| channel_participant_to_admin(session, p))
                                .collect::<Vec<_>>()
                        });
                        if let Ok(mut g) = state.lock() {
                            g.admin_log_admins.insert(peer_id, admins);
                        }
                    }
                    Ok(tl_enums::channels::ChannelParticipants::NotModified) => {}
                    Err(err) => {
                        debug!(
                            peer = peer_id.0,
                            error = %err,
                            "channels.getParticipants(Admins) failed"
                        );
                    }
                }
            }
        }
        state
            .lock()
            .ok()
            .and_then(|g| g.admin_log_admins.get(&peer_id).cloned())
            .unwrap_or_default()
    }

    /// Invalidates cached admin log entries for `peer_id`.
    pub fn invalidate_admin_log_cache(&self, peer_id: PeerId) {
        let state = sections_state_for(self);
        if let Ok(mut g) = state.lock() {
            g.admin_log_cache.retain(|(p, _), _| *p != peer_id);
            g.admin_log_admins_loaded.remove(&peer_id);
        }
    }

    // -------------------------------------------------------------------------
    // 5. Saved Messages Sublists
    // -------------------------------------------------------------------------

    /// Returns `true` if the server previously rejected Saved Messages dialogs with
    /// `SAVED_DIALOGS_UNSUPPORTED` (`SavedMessages::markUnsupported` in
    /// `data/data_saved_messages.cpp:127`).
    #[must_use]
    pub fn is_saved_dialogs_unsupported(&self) -> bool {
        read_session(self, |s| s.saved_messages().unsupported)
    }

    /// Loads the next paginated page of Saved Messages sublists via
    /// `messages.getPinnedSavedDialogs` (on first load) +
    /// `messages.getSavedDialogs(exclude_pinned: true)`
    /// (`SavedMessages::sendLoadMore` / `loadPinned` in `data/data_saved_messages.cpp:298-372`).
    ///
    /// Handles cursor advancement, out-of-order date termination, and
    /// `SAVED_DIALOGS_UNSUPPORTED`. Returns `Ok(true)` if more pages remain.
    pub fn load_more_saved_sublists(&self, peer_id: PeerId) -> Result<bool, String> {
        let rpc = self.runtime().rpc().clone();
        let session_arc = Arc::clone(self.runtime().session());
        let page_opt = block_on_runtime(self.runtime().tokio_handle(), async move {
            let fut = async move {
                let mut session = session_arc.write().await;
                tg_api::saved::load_saved_dialogs_page(&rpc, &mut session, None).await
            };
            match tokio::time::timeout(SECTIONS_RPC_TIMEOUT, fut).await {
                Ok(Ok(page)) => Ok(page),
                Ok(Err(err)) => Err(err.to_string()),
                Err(_) => Err("RPC timed out".to_owned()),
            }
        })?;
        self.sync_saved_sublists_cache(peer_id);
        Ok(page_opt.is_some_and(|p| !p.all_loaded))
    }

    fn sync_saved_sublists_cache(&self, peer_id: PeerId) -> Vec<SavedSublist> {
        let (mut sublists, pinned_ids) = read_session(self, rebuild_saved_sublists_from_session);
        let state = sections_state_for(self);
        if let Ok(mut g) = state.lock() {
            if !pinned_ids.is_empty() {
                g.saved_pinned_order.insert(peer_id, pinned_ids);
            } else {
                g.saved_pinned_order.remove(&peer_id);
            }
            let order = g.saved_pinned_order.get(&peer_id).cloned();
            sort_saved_sublists(&mut sublists, order.as_deref());
            g.saved_sublists_loaded.insert(peer_id);
            g.saved_sublists.insert(peer_id, sublists.clone());
        }
        sublists
    }

    /// Loads Saved Messages per-chat sublists via `messages.getPinnedSavedDialogs` +
    /// `messages.getSavedDialogs(exclude_pinned: true)`
    /// (`data/data_saved_messages.cpp:298-372`).
    #[must_use]
    pub fn saved_sublists_live(&self, peer_id: PeerId) -> Vec<SavedSublist> {
        let state = sections_state_for(self);
        let already_loaded = state
            .lock()
            .map(|g| g.saved_sublists_loaded.contains(&peer_id))
            .unwrap_or(true);
        if !already_loaded {
            if let Ok(mut g) = state.lock() {
                g.saved_sublists_loaded.insert(peer_id);
            }
            if let Err(err) = self.load_more_saved_sublists(peer_id) {
                debug!(peer = peer_id.0, error = %err, "messages.getSavedDialogs failed");
            }
        }
        state
            .lock()
            .ok()
            .and_then(|g| g.saved_sublists.get(&peer_id).cloned())
            .unwrap_or_default()
    }

    /// Loads messages belonging to a Saved Messages sublist via `messages.getSavedHistory`.
    #[must_use]
    pub fn load_saved_sublist_messages_live(
        &self,
        peer_id: PeerId,
        sublist_peer_id: PeerId,
    ) -> Vec<HistoryMessage> {
        let state = sections_state_for(self);
        let key = (peer_id, sublist_peer_id);
        let already_loaded = state
            .lock()
            .map(|g| g.saved_sublist_messages_loaded.contains(&key))
            .unwrap_or(true);
        if !already_loaded {
            if let Ok(mut g) = state.lock() {
                g.saved_sublist_messages_loaded.insert(key);
            }
            if let Some(sub_input) = input_peer(self, sublist_peer_id) {
                let req = tl_fns::messages::GetSavedHistory {
                    parent_peer: None,
                    peer: sub_input,
                    offset_id: 0,
                    offset_date: 0,
                    add_offset: 0,
                    limit: SECTION_HISTORY_LIMIT,
                    max_id: 0,
                    min_id: 0,
                    hash: 0,
                };
                match invoke_rpc(self, req) {
                    Ok(resp) => {
                        let (raw_msgs, chats, users, channel_pts) = extract_messages_payload(resp);
                        if let Some(pts) = channel_pts
                            && let Some(ch_id) = to_data_peer(peer_id)
                                .to_channel()
                                .or_else(|| to_data_peer(sublist_peer_id).to_channel())
                        {
                            let _ = self.apply_channel_pts(ch_id, pts, 0);
                        }
                        let mut converted = write_session(self, |session| {
                            apply_tl_users_and_chats(session, &users, &chats);
                            let self_uid = session.self_user_id();
                            let mut out = Vec::with_capacity(raw_msgs.len());
                            for raw in &raw_msgs {
                                if let Some(dm) = message_from_tl(raw, self_uid) {
                                    let mut hm = message_to_history_message(session, &dm);
                                    hm.extra.saved_from_peer = Some(sublist_peer_id);
                                    out.push(hm);
                                }
                            }
                            out
                        });
                        converted.sort_by_key(|m| m.id.0);
                        let count = converted.len() as u32;
                        if let Ok(mut g) = state.lock() {
                            if let Some(list) = g.saved_sublists.get_mut(&peer_id)
                                && let Some(sub) = list
                                    .iter_mut()
                                    .find(|s| s.sublist_peer_id == sublist_peer_id)
                            {
                                sub.message_count = count;
                            }
                            g.saved_sublist_messages.insert(key, converted);
                        }
                    }
                    Err(err) => {
                        debug!(
                            sublist = sublist_peer_id.0,
                            error = %err,
                            "messages.getSavedHistory failed"
                        );
                    }
                }
            }
        }
        state
            .lock()
            .ok()
            .and_then(|g| g.saved_sublist_messages.get(&key).cloned())
            .unwrap_or_default()
    }

    /// Deletes saved messages for `sublist_peer_id` via `messages.deleteSavedHistory`
    /// (`ApiWrap::deleteSublistHistorySend` in `apiwrap.cpp:1610-1628`), feeding each
    /// `messages.affectedHistory` `(pts, pts_count)` page into the updates handler
    /// (`applyAffectedHistory`).
    pub fn delete_saved_history(
        &mut self,
        peer_id: PeerId,
        sublist_peer_id: PeerId,
        max_id: MsgId,
    ) -> Result<i32, String> {
        let Some(sublist_input) = input_peer(self, sublist_peer_id) else {
            return Err("Saved sublist peer not found in session.".to_owned());
        };
        let data_sublist = to_data_peer(sublist_peer_id);
        let data_max_id = to_data_msg(max_id);
        let rpc = self.runtime().rpc().clone();
        let session_arc = Arc::clone(self.runtime().session());
        let runtime = Arc::clone(self.runtime());

        let pages = block_on_runtime(self.runtime().tokio_handle(), async move {
            let fut = async move {
                let mut pages = Vec::new();
                for _ in 0..tg_api::saved::MAX_DELETE_SAVED_HISTORY_LOOPS {
                    let tl_enums::messages::AffectedHistory::AffectedHistory(aff) = rpc
                        .invoke(&tl_fns::messages::DeleteSavedHistory {
                            parent_peer: None,
                            peer: sublist_input.clone(),
                            max_id: data_max_id.as_i32().unwrap_or(0),
                            min_date: None,
                            max_date: None,
                        })
                        .await
                        .map_err(tg_api::ApiError::from)?;
                    // Ported from `ApiWrap::deleteSublistHistorySend` (`apiwrap.cpp:1610-1628`)
                    // and `ApiWrap::applyAffectedHistory` (`apiwrap.cpp:2338-2348`): feed each
                    // page's `(pts, pts_count)` into `SessionActor`'s single `UpdatesHandler`.
                    let _ = runtime
                        .apply_affected_history(None, aff.pts, aff.pts_count)
                        .await;
                    let next_offset = aff.offset;
                    pages.push(aff);
                    if next_offset <= 0 {
                        break;
                    }
                }
                if data_max_id.is_zero() {
                    let mut session = session_arc.write().await;
                    session
                        .saved_messages_mut()
                        .apply_sublist_deleted(data_sublist);
                }
                Ok::<_, tg_api::ApiError>(pages)
            };
            match tokio::time::timeout(SECTIONS_RPC_TIMEOUT, fut).await {
                Ok(Ok(pages)) => Ok(pages),
                Ok(Err(err)) => Err(err.to_string()),
                Err(_) => Err("RPC timed out".to_owned()),
            }
        })?;
        let last_pts = pages.last().map_or(0, |a| a.pts);

        let state = sections_state_for(self);
        if let Ok(mut g) = state.lock() {
            let key = (peer_id, sublist_peer_id);
            let remaining_len = if let Some(msgs) = g.saved_sublist_messages.get_mut(&key) {
                if max_id.0 <= 0 {
                    msgs.clear();
                } else {
                    msgs.retain(|m| m.id.0 > max_id.0);
                }
                msgs.len()
            } else {
                0
            };
            if max_id.0 <= 0 || remaining_len == 0 {
                g.saved_sublist_messages.remove(&key);
                g.saved_sublist_messages_loaded.remove(&key);
                if let Some(list) = g.saved_sublists.get_mut(&peer_id) {
                    list.retain(|s| s.sublist_peer_id != sublist_peer_id);
                }
            } else if let Some(list) = g.saved_sublists.get_mut(&peer_id)
                && let Some(sub) = list
                    .iter_mut()
                    .find(|s| s.sublist_peer_id == sublist_peer_id)
            {
                sub.message_count = remaining_len as u32;
            }
        }
        self.bridge_state().notify_ui();
        Ok(last_pts)
    }

    /// Pins or unpins a Saved Messages sublist via `messages.toggleSavedDialogPin`.
    pub fn toggle_saved_dialog_pin(
        &mut self,
        peer_id: PeerId,
        sublist_peer_id: PeerId,
        pinned: bool,
    ) -> Result<bool, String> {
        let Some(sub_input) = input_peer(self, sublist_peer_id) else {
            return Err("Saved sublist peer not found in session.".to_owned());
        };
        let req = tl_fns::messages::ToggleSavedDialogPin {
            pinned,
            peer: tl_enums::InputDialogPeer::InputDialogPeer(tl_types::InputDialogPeer {
                peer: sub_input,
            }),
        };
        let ok = invoke_rpc(self, req)?;
        if !ok {
            return Err("Telegram rejected toggleSavedDialogPin.".to_owned());
        }
        write_session(self, |session| {
            let _ = session
                .saved_messages_mut()
                .set_sublist_pinned(to_data_peer(sublist_peer_id), pinned);
        });
        let state = sections_state_for(self);
        if let Ok(mut g) = state.lock() {
            apply_saved_dialog_pin_state(&mut g, peer_id, sublist_peer_id, pinned);
        }
        self.bridge_state().notify_ui();
        Ok(pinned)
    }

    /// Reorders pinned Saved Messages sublists via `messages.reorderPinnedSavedDialogs`.
    pub fn reorder_pinned_saved_dialogs(
        &mut self,
        peer_id: PeerId,
        order: &[PeerId],
        force: bool,
    ) -> Result<(), String> {
        let mut tl_order = Vec::with_capacity(order.len());
        for &sub_pid in order {
            let Some(ip) = input_peer(self, sub_pid) else {
                return Err(format!(
                    "Saved sublist peer {} not found in session.",
                    sub_pid.0
                ));
            };
            tl_order.push(tl_enums::InputDialogPeer::InputDialogPeer(
                tl_types::InputDialogPeer { peer: ip },
            ));
        }
        let ok = invoke_rpc(
            self,
            tl_fns::messages::ReorderPinnedSavedDialogs {
                force,
                order: tl_order,
            },
        )?;
        if !ok {
            return Err("Telegram rejected reorderPinnedSavedDialogs.".to_owned());
        }
        let data_order: Vec<DataPeerId> = order.iter().copied().map(to_data_peer).collect();
        write_session(self, |session| {
            let _ = session
                .saved_messages_mut()
                .apply_pinned_order(Some(&data_order));
        });
        let state = sections_state_for(self);
        if let Ok(mut g) = state.lock() {
            apply_saved_pinned_order(&mut g, peer_id, order.to_vec());
        }
        self.bridge_state().notify_ui();
        Ok(())
    }

    // -------------------------------------------------------------------------
    // 6. Polls (`messages.sendMedia` + `inputMediaPoll`)
    // -------------------------------------------------------------------------

    /// Creates and sends a poll or quiz via `messages.sendMedia(inputMediaPoll)`.
    pub fn send_poll_live(
        &mut self,
        peer_id: PeerId,
        thread_key: Option<ThreadKey>,
        request: CreatePollRequest,
    ) -> Result<HistoryMessage, String> {
        self.send_poll_with_options(peer_id, thread_key, request, LivePollOptions::default())
    }

    /// Creates and sends a poll or quiz with explicit `close_period` / `close_date` via
    /// `messages.sendMedia(inputMediaPoll)`.
    pub fn send_poll_with_options(
        &mut self,
        peer_id: PeerId,
        thread_key: Option<ThreadKey>,
        request: CreatePollRequest,
        poll_options: LivePollOptions,
    ) -> Result<HistoryMessage, String> {
        validate_create_poll(&request, POLL_MAX_OPTIONS).map_err(|e| e.message())?;
        let state = sections_state_for(self);
        let (target_peer, reply_to) =
            self.resolve_thread_send_target(peer_id, thread_key, None, None);
        let Some(tl_peer) = input_peer(self, target_peer) else {
            return Err("Peer not found in session.".to_owned());
        };
        let random_id = state
            .lock()
            .map_err(|_| "history sections state lock poisoned".to_owned())?
            .next_random_id();

        let trimmed_options: Vec<String> = request
            .options
            .iter()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        let answers: Vec<tl_enums::PollAnswer> = trimmed_options
            .iter()
            .enumerate()
            .map(|(idx, text)| {
                tl_enums::PollAnswer::PollAnswer(tl_types::PollAnswer {
                    text: tl_enums::TextWithEntities::TextWithEntities(
                        tl_types::TextWithEntities {
                            text: text.clone(),
                            entities: Vec::new(),
                        },
                    ),
                    option: vec![idx as u8],
                    media: None,
                    added_by: None,
                    date: None,
                })
            })
            .collect();

        let correct_answers = if request.is_quiz {
            request.correct_option.map(|idx| vec![idx as i32])
        } else {
            None
        };
        let solution = if request.is_quiz {
            request
                .explanation
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        } else {
            None
        };
        let solution_entities = solution.as_ref().map(|_| Vec::new());

        let tl_poll = tl_enums::Poll::Poll(tl_types::Poll {
            id: random_id,
            closed: false,
            public_voters: !request.is_anonymous,
            multiple_choice: request.multiple_choice && !request.is_quiz,
            quiz: request.is_quiz,
            open_answers: false,
            revoting_disabled: false,
            shuffle_answers: false,
            hide_results_until_close: false,
            creator: true,
            subscribers_only: false,
            question: tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                text: request.question.trim().to_owned(),
                entities: Vec::new(),
            }),
            answers,
            close_period: poll_options.close_period,
            close_date: poll_options.close_date,
            countries_iso2: None,
            hash: 0,
        });

        let req = tl_fns::messages::SendMedia {
            silent: request.silent,
            background: false,
            clear_draft: false,
            noforwards: false,
            update_stickersets_order: false,
            invert_media: false,
            allow_paid_floodskip: false,
            peer: tl_peer,
            reply_to,
            media: tl_enums::InputMedia::Poll(tl_types::InputMediaPoll {
                poll: tl_poll,
                correct_answers,
                attached_media: None,
                solution: solution.clone(),
                solution_entities,
                solution_media: None,
            }),
            message: String::new(),
            random_id,
            reply_markup: None,
            entities: None,
            schedule_date: request.schedule_date,
            schedule_repeat_period: None,
            send_as: None,
            quick_reply_shortcut: None,
            effect: None,
            allow_paid_stars: None,
            suggested_post: None,
        };
        let updates = invoke_rpc(self, req)?;

        let data_target_peer = to_data_peer(target_peer);
        let new_ids = self.apply_updates_to_session(&updates);
        read_session(self, |session| {
            if let Ok(mut guard) = state.lock() {
                for &mid in &new_ids {
                    if let Some(msg) = session.message(DataFullMsgId::new(data_target_peer, mid))
                        && let Some(MessageMedia::Poll(poll)) = msg.media()
                    {
                        index_poll_message(&mut guard, poll.id.0 as i64, data_target_peer, mid);
                    }
                }
            }
        });

        let msg = read_session(self, |session| {
            new_ids.iter().rev().find_map(|&mid| {
                session
                    .message(DataFullMsgId::new(data_target_peer, mid))
                    .map(|m| message_to_history_message(session, m))
            })
        })
        .or_else(|| {
            if request.schedule_date.is_some() {
                state
                    .lock()
                    .ok()
                    .and_then(|g| g.scheduled_messages.get(&target_peer)?.last().cloned())
            } else {
                None
            }
        })
        .unwrap_or_else(|| {
            let (self_pid, self_name) = read_session(self, |s| {
                let p = s.self_peer_id();
                (from_data_peer(p), peer_display_title(s, p))
            });
            let options = trimmed_options
                .iter()
                .enumerate()
                .map(|(i, text)| PollOption {
                    text: text.clone(),
                    votes: 0,
                    chosen: false,
                    correct: request.is_quiz && request.correct_option == Some(i),
                })
                .collect();
            HistoryMessage {
                id: new_ids.last().copied().map_or(MsgId(0), from_data_msg),
                peer_id: target_peer,
                from_id: self_pid,
                sender_name: self_name,
                date_unix: request.schedule_date.unwrap_or_else(now_unix_i32),
                outgoing: true,
                is_post: false,
                text: tg_text::TextWithEntities::default(),
                media: Some(HistoryMedia::Poll(PollAttachment {
                    question: request.question.trim().to_owned(),
                    is_quiz: request.is_quiz,
                    is_anonymous: request.is_anonymous,
                    multiple_choice: request.multiple_choice && !request.is_quiz,
                    closed: false,
                    options,
                    total_voters: 0,
                    explanation: solution,
                })),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: false,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra {
                    thread_id: thread_key_msg_id(thread_key),
                    scheduled_date: request.schedule_date,
                    silent: request.silent,
                    ..MessageExtra::default()
                },
            }
        });

        self.bridge_state().notify_ui();
        Ok(msg)
    }

    /// Returns peers observed voting for `option` on `poll_id` via `updateMessagePollVote`.
    #[must_use]
    pub fn poll_recent_voters(&self, poll_id: i64, option: &[u8]) -> Vec<PeerId> {
        let state = sections_state_for(self);
        state
            .lock()
            .ok()
            .and_then(|g| g.poll_voters_by_option.get(&poll_id)?.get(option).cloned())
            .unwrap_or_default()
    }

    // -------------------------------------------------------------------------
    // 7. Forwarding with options (`drop_author`, `drop_media_captions`, `silent`, `schedule_date`)
    // -------------------------------------------------------------------------

    /// Forwards `msg_ids` from `from_peer` into `to_peer` honoring [`ForwardOptions`].
    pub fn forward_messages_with_options_live(
        &mut self,
        from_peer: PeerId,
        msg_ids: &[MsgId],
        to_peer: PeerId,
        options: ForwardOptions,
    ) -> Result<Vec<HistoryMessage>, String> {
        self.forward_messages_extended(from_peer, msg_ids, to_peer, options.into())
    }

    /// Forwards `msg_ids` from `from_peer` into `to_peer` with full [`LiveForwardOptions`]
    /// (`drop_author`, `drop_media_captions`, `silent`, `schedule_date`, `top_msg_id`, `send_as`).
    pub fn forward_messages_extended(
        &mut self,
        from_peer: PeerId,
        msg_ids: &[MsgId],
        to_peer: PeerId,
        options: LiveForwardOptions,
    ) -> Result<Vec<HistoryMessage>, String> {
        if msg_ids.is_empty() {
            return Ok(Vec::new());
        }
        let Some(from_input) = input_peer(self, from_peer) else {
            return Err("Source peer not found.".to_owned());
        };
        let Some(to_input) = input_peer(self, to_peer) else {
            return Err("Destination peer not found.".to_owned());
        };
        let send_as = options.send_as.and_then(|p| input_peer(self, p));
        let state = sections_state_for(self);
        let random_ids: Vec<i64> = {
            let mut g = state
                .lock()
                .map_err(|_| "history sections state lock poisoned".to_owned())?;
            msg_ids.iter().map(|_| g.next_random_id()).collect()
        };

        let req = tl_fns::messages::ForwardMessages {
            silent: options.silent,
            background: false,
            with_my_score: false,
            drop_author: options.drop_author,
            drop_media_captions: options.drop_media_captions,
            noforwards: false,
            allow_paid_floodskip: false,
            from_ephemeral: false,
            from_peer: from_input,
            id: msg_ids.iter().map(|m| m.0).collect(),
            random_id: random_ids,
            to_peer: to_input,
            top_msg_id: options.top_msg_id.map(|m| m.0),
            reply_to: None,
            schedule_date: options.schedule_date,
            schedule_repeat_period: None,
            send_as,
            quick_reply_shortcut: None,
            effect: None,
            video_timestamp: None,
            allow_paid_stars: None,
            suggested_post: None,
        };
        let updates = invoke_rpc(self, req)?;

        let dest_data_peer = to_data_peer(to_peer);
        let new_regular_ids = self.apply_updates_to_session(&updates);
        let scheduled_msgs = if options.schedule_date.is_some() {
            state
                .lock()
                .ok()
                .and_then(|g| g.scheduled_messages.get(&to_peer).cloned())
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let forwarded = if !new_regular_ids.is_empty() {
            read_session(self, |s| {
                new_regular_ids
                    .into_iter()
                    .filter_map(|mid| {
                        s.message(DataFullMsgId::new(dest_data_peer, mid))
                            .map(|m| message_to_history_message(s, m))
                    })
                    .collect()
            })
        } else if options.schedule_date.is_some() {
            scheduled_msgs
        } else {
            Vec::new()
        };

        self.bridge_state().notify_ui();
        Ok(forwarded)
    }

    // -------------------------------------------------------------------------
    // 8. Rich per-thread drafts (`messages.saveDraft` preserving entities & quotes)
    // -------------------------------------------------------------------------

    /// Saves or clears a per-thread compose draft preserving `tg-text` UTF-16 entities,
    /// reply target, quoted text/entities, `no_webpage`, and forum/replies `top_msg_id`.
    pub fn save_rich_draft_live(&mut self, thread: ThreadKey, draft: Option<RichThreadDraft>) {
        let state = sections_state_for(self);
        let (target_peer, top_msg_id, saved_peer_id) = match thread {
            ThreadKey::Main(p) => (p, None, None),
            ThreadKey::Topic {
                peer_id,
                topic_root_id,
            } => (peer_id, Some(topic_root_id), None),
            ThreadKey::Replies {
                peer_id,
                root_msg_id,
            } => {
                let resolved = state.lock().ok().and_then(|g| {
                    let meta = g.discussion_threads.get(&(peer_id, root_msg_id))?;
                    Some((meta.discussion_peer_id, meta.discussion_root_msg_id))
                });
                match resolved {
                    Some((dp, dr)) => (dp, Some(dr), None),
                    None => (peer_id, Some(root_msg_id), None),
                }
            }
            ThreadKey::SavedSublist {
                peer_id,
                sublist_peer_id,
            } => (peer_id, None, Some(sublist_peer_id)),
        };

        let effective = draft.filter(|d| !d.text.text.is_empty() || d.reply_to.is_some());

        if let Some(tl_peer) = input_peer(self, target_peer) {
            let (message, tl_entities, no_webpage, reply_to) =
                read_session(self, |session| match &effective {
                    Some(d) => {
                        let ents = ui_text_to_tl_entities(&d.text, session);
                        let reply = match (d.reply_to, top_msg_id) {
                            (Some(rid), top) => Some(make_reply_to_message(
                                rid.0,
                                top.map(|m| m.0),
                                d.reply_quote.as_ref(),
                                session,
                            )),
                            (None, Some(top)) => Some(make_reply_to_message(
                                top.0,
                                Some(top.0),
                                d.reply_quote.as_ref(),
                                session,
                            )),
                            (None, None) => None,
                        };
                        (
                            d.text.text.clone(),
                            (!ents.is_empty()).then_some(ents),
                            d.no_webpage,
                            reply,
                        )
                    }
                    None => {
                        let reply = top_msg_id
                            .map(|top| make_reply_to_message(top.0, Some(top.0), None, session));
                        (String::new(), None, false, reply)
                    }
                });

            let req = tl_fns::messages::SaveDraft {
                no_webpage,
                invert_media: false,
                reply_to,
                peer: tl_peer,
                message,
                entities: tl_entities,
                media: None,
                effect: None,
                suggested_post: None,
                rich_message: None,
            };
            if let Err(err) = invoke_rpc(self, req) {
                debug!(peer = target_peer.0, error = %err, "messages.saveDraft (rich) failed");
            }
        }

        let data_peer = to_data_peer(target_peer);
        let key_top = top_msg_id.map_or(DataMsgId::ZERO, to_data_msg);
        let key_saved = saved_peer_id.map_or(DataPeerId::ZERO, to_data_peer);
        let local_key = DraftKey::local(key_top, key_saved);
        let cloud_key = DraftKey::cloud(key_top, key_saved);
        let now = now_unix_i32();

        write_session(self, |session| match &effective {
            Some(d) => {
                let data_text = ui_text_to_data_text(&d.text);
                let quote_data = d.reply_quote.as_ref().map(|q| {
                    ui_text_to_data_text(&tg_text::TextWithEntities {
                        text: q.text.clone(),
                        entities: q.entities.clone(),
                    })
                });
                let reply = if d.reply_to.is_some() || top_msg_id.is_some() {
                    Some(ReplyInfo {
                        reply_to_msg_id: d.reply_to.map(to_data_msg),
                        top_msg_id: top_msg_id.map(to_data_msg),
                        quote_text: quote_data,
                        quote_offset: d.reply_quote.as_ref().map(|q| q.offset_utf16),
                        ..ReplyInfo::default()
                    })
                } else {
                    None
                };
                let data_draft = Draft {
                    date: now,
                    text: data_text,
                    reply,
                    webpage: WebPageDraft {
                        removed: d.no_webpage,
                        ..WebPageDraft::default()
                    },
                    cursor_position: 0,
                    cursor_anchor: 0,
                    effect_id: None,
                };
                session.set_draft(data_peer, local_key, data_draft.clone());
                session.set_draft(data_peer, cloud_key, data_draft);
            }
            None => {
                let _ = session.clear_draft(data_peer, local_key);
                let _ = session.clear_draft(data_peer, cloud_key);
            }
        });

        if let Ok(mut guard) = state.lock() {
            match effective {
                Some(d) => {
                    if let ThreadKey::Topic {
                        peer_id,
                        topic_root_id,
                    } = thread
                        && let Some(list) = guard.forum_topics.get_mut(&peer_id)
                        && let Some(topic) = list.iter_mut().find(|t| t.root_id == topic_root_id)
                    {
                        topic.draft_preview =
                            (!d.text.text.trim().is_empty()).then(|| d.text.text.clone());
                    }
                    guard.rich_thread_drafts.insert(thread, d);
                }
                None => {
                    guard.rich_thread_drafts.remove(&thread);
                    if let ThreadKey::Topic {
                        peer_id,
                        topic_root_id,
                    } = thread
                        && let Some(list) = guard.forum_topics.get_mut(&peer_id)
                        && let Some(topic) = list.iter_mut().find(|t| t.root_id == topic_root_id)
                    {
                        topic.draft_preview = None;
                    }
                }
            }
        }
        self.bridge_state().notify_ui();
    }

    /// Loads a saved per-thread compose draft preserving `tg-text` UTF-16 entities.
    #[must_use]
    pub fn load_rich_draft_live(&self, thread: ThreadKey) -> Option<RichThreadDraft> {
        let state = sections_state_for(self);
        if let Ok(g) = state.lock()
            && let Some(d) = g.rich_thread_drafts.get(&thread)
        {
            return Some(d.clone());
        }

        let (data_peer, key_top, key_saved) = match thread {
            ThreadKey::Main(p) => (to_data_peer(p), DataMsgId::ZERO, DataPeerId::ZERO),
            ThreadKey::Topic {
                peer_id,
                topic_root_id,
            } => (
                to_data_peer(peer_id),
                to_data_msg(topic_root_id),
                DataPeerId::ZERO,
            ),
            ThreadKey::Replies {
                peer_id,
                root_msg_id,
            } => {
                let resolved = state.lock().ok().and_then(|g| {
                    let meta = g.discussion_threads.get(&(peer_id, root_msg_id))?;
                    Some((meta.discussion_peer_id, meta.discussion_root_msg_id))
                });
                match resolved {
                    Some((dp, dr)) => (to_data_peer(dp), to_data_msg(dr), DataPeerId::ZERO),
                    None => (
                        to_data_peer(peer_id),
                        to_data_msg(root_msg_id),
                        DataPeerId::ZERO,
                    ),
                }
            }
            ThreadKey::SavedSublist {
                peer_id,
                sublist_peer_id,
            } => (
                to_data_peer(peer_id),
                DataMsgId::ZERO,
                to_data_peer(sublist_peer_id),
            ),
        };

        read_session(self, |session| {
            let h = session.history(data_peer)?;
            let d = h
                .draft(DraftKey::local(key_top, key_saved))
                .or_else(|| h.draft(DraftKey::cloud(key_top, key_saved)))?;
            if d.is_empty() {
                return None;
            }
            let reply_to = d
                .reply
                .as_ref()
                .and_then(|r| r.reply_to_msg_id)
                .map(from_data_msg);
            let reply_quote = d.reply.as_ref().and_then(|r| {
                let qt = r.quote_text.as_ref()?;
                let ui_qt = data_text_to_ui_text(qt);
                Some(ReplyQuote {
                    text: ui_qt.text,
                    entities: ui_qt.entities,
                    offset_utf16: r.quote_offset.unwrap_or(0),
                })
            });
            Some(RichThreadDraft {
                text: data_text_to_ui_text(&d.text),
                reply_to,
                reply_quote,
                no_webpage: d.webpage.removed,
            })
        })
    }

    // -------------------------------------------------------------------------
    // 9. Thread-scoped & scheduled extended send helper
    // -------------------------------------------------------------------------

    /// Sends a thread-scoped (forum topic / channel comments / saved sublist) or scheduled
    /// message via `messages.sendMessage` so topic `top_msg_id`, discussion group routing,
    /// `send_as`, `effect`, and `Update::NewScheduledMessage` are handled cleanly.
    pub fn send_thread_or_scheduled_extended(
        &mut self,
        peer_id: PeerId,
        request: SendMessageExtended,
    ) -> Result<HistoryMessage, String> {
        if request.text.text.trim().is_empty() {
            return Err("Cannot send an empty message.".to_owned());
        }
        let state = sections_state_for(self);
        let (target_peer, reply_to) = self.resolve_thread_send_target(
            peer_id,
            request.thread_key,
            request.reply_to,
            request.reply_quote.as_ref(),
        );
        let Some(tl_peer) = input_peer(self, target_peer) else {
            return Err("Peer not found in session.".to_owned());
        };
        let send_as = request.send_as.and_then(|p| input_peer(self, p));
        let entities = read_session(self, |s| ui_text_to_tl_entities(&request.text, s));
        let random_id = state
            .lock()
            .map_err(|_| "history sections state lock poisoned".to_owned())?
            .next_random_id();

        let req = tl_fns::messages::SendMessage {
            no_webpage: request.no_webpage,
            silent: request.silent,
            background: false,
            clear_draft: true,
            noforwards: false,
            update_stickersets_order: false,
            invert_media: request
                .link_preview
                .as_ref()
                .is_some_and(|lp| lp.above_text),
            allow_paid_floodskip: false,
            peer: tl_peer,
            reply_to,
            message: request.text.text.clone(),
            random_id,
            reply_markup: None,
            entities: (!entities.is_empty()).then_some(entities),
            schedule_date: request.schedule_date,
            schedule_repeat_period: None,
            send_as,
            quick_reply_shortcut: None,
            effect: request.effect_id.map(|e| e as i64),
            allow_paid_stars: None,
            suggested_post: None,
            rich_message: None,
        };
        let updates = invoke_rpc(self, req)?;

        let data_target = to_data_peer(target_peer);
        let regular_ids = self.apply_updates_to_session(&updates);

        let mut sent = read_session(self, |session| {
            regular_ids.iter().rev().find_map(|&mid| {
                session
                    .message(DataFullMsgId::new(data_target, mid))
                    .map(|m| message_to_history_message(session, m))
            })
        });

        if sent.is_none() && request.schedule_date.is_some() {
            sent = state
                .lock()
                .ok()
                .and_then(|g| g.scheduled_messages.get(&target_peer)?.last().cloned());
        }

        let mut result = sent.unwrap_or_else(|| {
            let (self_pid, self_name) = read_session(self, |s| {
                let p = s.self_peer_id();
                (from_data_peer(p), peer_display_title(s, p))
            });
            HistoryMessage {
                id: regular_ids.last().copied().map_or(MsgId(0), from_data_msg),
                peer_id: target_peer,
                from_id: request.send_as.unwrap_or(self_pid),
                sender_name: self_name,
                date_unix: request.schedule_date.unwrap_or_else(now_unix_i32),
                outgoing: true,
                is_post: false,
                text: request.text.clone(),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: false,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra {
                    thread_id: thread_key_msg_id(request.thread_key),
                    saved_from_peer: match request.thread_key {
                        Some(ThreadKey::SavedSublist {
                            sublist_peer_id, ..
                        }) => Some(sublist_peer_id),
                        _ => None,
                    },
                    scheduled_date: request.schedule_date,
                    silent: request.silent,
                    effect_id: request.effect_id,
                    ..MessageExtra::default()
                },
            }
        });

        // Mirror into the active thread/section cache so immediate UI reads reflect the sent message.
        if let Ok(mut guard) = state.lock() {
            if let Some(sched_ts) = request.schedule_date {
                result.extra.scheduled_date = Some(sched_ts);
                if let Some(tid) = thread_key_msg_id(request.thread_key) {
                    result.extra.thread_id = Some(tid);
                }
                upsert_scheduled_message(&mut guard, target_peer, result.clone());
            } else {
                match request.thread_key {
                    Some(ThreadKey::Topic {
                        peer_id: fp,
                        topic_root_id,
                    }) => {
                        result.extra.thread_id = Some(topic_root_id);
                        upsert_topic_message(&mut guard, fp, topic_root_id, result.clone());
                    }
                    Some(ThreadKey::Replies {
                        peer_id: cp,
                        root_msg_id,
                    }) => {
                        result.extra.thread_id = Some(root_msg_id);
                        if let Some(meta) = guard.discussion_threads.get_mut(&(cp, root_msg_id)) {
                            upsert_ordered_message(&mut meta.comments, result.clone());
                        }
                    }
                    Some(ThreadKey::SavedSublist {
                        peer_id: sp,
                        sublist_peer_id,
                    }) => {
                        result.extra.saved_from_peer = Some(sublist_peer_id);
                        let list = guard
                            .saved_sublist_messages
                            .entry((sp, sublist_peer_id))
                            .or_default();
                        upsert_ordered_message(list, result.clone());
                    }
                    _ => {}
                }
            }
        }

        self.bridge_state().notify_ui();
        Ok(result)
    }

    fn resolve_thread_send_target(
        &self,
        peer_id: PeerId,
        thread_key: Option<ThreadKey>,
        reply_to: Option<MsgId>,
        reply_quote: Option<&ReplyQuote>,
    ) -> (PeerId, Option<tl_enums::InputReplyTo>) {
        let state = sections_state_for(self);
        let (target_peer, top_msg_id) = match thread_key {
            Some(ThreadKey::Topic {
                peer_id: fp,
                topic_root_id,
            }) => (fp, Some(topic_root_id)),
            Some(ThreadKey::Replies {
                peer_id: cp,
                root_msg_id,
            }) => {
                let resolved = state.lock().ok().and_then(|g| {
                    let meta = g.discussion_threads.get(&(cp, root_msg_id))?;
                    Some((meta.discussion_peer_id, meta.discussion_root_msg_id))
                });
                match resolved {
                    Some((dp, dr)) => (dp, Some(dr)),
                    None => (cp, Some(root_msg_id)),
                }
            }
            Some(ThreadKey::SavedSublist { peer_id: sp, .. }) => (sp, None),
            Some(ThreadKey::Main(p)) => (p, None),
            None => (peer_id, None),
        };

        let tl_reply = read_session(self, |session| match (reply_to, top_msg_id) {
            (Some(rid), top) => Some(make_reply_to_message(
                rid.0,
                top.map(|m| m.0),
                reply_quote,
                session,
            )),
            (None, Some(top)) => Some(make_reply_to_message(
                top.0,
                Some(top.0),
                reply_quote,
                session,
            )),
            (None, None) => None,
        });

        (target_peer, tl_reply)
    }
}

// =============================================================================
// Real-time TL updates handling
// =============================================================================

fn extract_created_topic_id(updates: &tl_enums::Updates) -> Option<MsgId> {
    let list: &[tl_enums::Update] = match updates {
        tl_enums::Updates::Updates(u) => &u.updates,
        tl_enums::Updates::Combined(c) => &c.updates,
        tl_enums::Updates::UpdateShort(s) => std::slice::from_ref(&s.update),
        tl_enums::Updates::UpdateShortSentMessage(short) => return Some(MsgId(short.id)),
        _ => return None,
    };
    for upd in list {
        match upd {
            tl_enums::Update::NewChannelMessage(u) => {
                if let Some(id) = tl_message_id(&u.message) {
                    return Some(MsgId(id));
                }
            }
            tl_enums::Update::NewMessage(u) => {
                if let Some(id) = tl_message_id(&u.message) {
                    return Some(MsgId(id));
                }
            }
            tl_enums::Update::MessageID(m) => return Some(MsgId(m.id)),
            _ => {}
        }
    }
    None
}

fn tl_message_id(msg: &tl_enums::Message) -> Option<i32> {
    match msg {
        tl_enums::Message::Message(m) => Some(m.id),
        tl_enums::Message::Service(s) => Some(s.id),
        tl_enums::Message::Empty(e) => Some(e.id),
    }
}

fn apply_tl_updates_container(
    session: &mut Session,
    state: &mut LiveHistorySectionsState,
    updates: &tl_enums::Updates,
) -> bool {
    let mut changed = false;
    match updates {
        tl_enums::Updates::Updates(u) => {
            for upd in &u.updates {
                if apply_single_section_update(session, state, upd) {
                    changed = true;
                }
            }
        }
        tl_enums::Updates::Combined(c) => {
            for upd in &c.updates {
                if apply_single_section_update(session, state, upd) {
                    changed = true;
                }
            }
        }
        tl_enums::Updates::UpdateShort(s) => {
            changed = apply_single_section_update(session, state, &s.update);
        }
        _ => {}
    }
    changed
}

fn apply_single_section_update(
    session: &mut Session,
    state: &mut LiveHistorySectionsState,
    update: &tl_enums::Update,
) -> bool {
    match update {
        tl_enums::Update::NewScheduledMessage(u) => {
            let self_uid = session.self_user_id();
            let Some(dm) = message_from_tl(&u.message, self_uid) else {
                return false;
            };
            let peer_id = from_data_peer(dm.peer_id);
            let mut hm = message_to_history_message(session, &dm);
            hm.extra.scheduled_date = Some(hm.date_unix);
            upsert_scheduled_message(state, peer_id, hm);
            true
        }
        tl_enums::Update::DeleteScheduledMessages(u) => {
            let peer_id = from_data_peer(peer_id_from_tl(&u.peer));
            let del_set: HashSet<i32> = u.messages.iter().copied().collect();
            if let Some(list) = state.scheduled_messages.get_mut(&peer_id) {
                let before = list.len();
                list.retain(|m| !del_set.contains(&m.id.0));
                return list.len() != before;
            }
            false
        }
        tl_enums::Update::MessagePoll(u) => apply_update_message_poll(session, state, u),
        tl_enums::Update::MessagePollVote(u) => apply_update_message_poll_vote(session, state, u),
        tl_enums::Update::PinnedForumTopic(u) => {
            let peer_id = from_data_peer(peer_id_from_tl(&u.peer));
            let topic_id = MsgId(u.topic_id);
            apply_forum_topic_pin_state(state, peer_id, topic_id, u.pinned);
            true
        }
        tl_enums::Update::PinnedForumTopics(u) => {
            let peer_id = from_data_peer(peer_id_from_tl(&u.peer));
            let order: Vec<MsgId> = u
                .order
                .as_deref()
                .unwrap_or_default()
                .iter()
                .copied()
                .map(MsgId)
                .collect();
            apply_forum_pinned_order(state, peer_id, order);
            true
        }
        tl_enums::Update::EditForumTopic(u) => {
            let peer_id = from_data_peer(peer_id_from_tl(&u.peer));
            let topic_id = MsgId(u.topic_id);
            let mut changed = false;
            if let Some(hidden) = u.hidden {
                if hidden {
                    state.forum_hidden_topics.insert((peer_id, topic_id));
                } else {
                    state.forum_hidden_topics.remove(&(peer_id, topic_id));
                }
                changed = true;
            }
            if let Some(list) = state.forum_topics.get_mut(&peer_id)
                && let Some(topic) = list.iter_mut().find(|t| t.root_id == topic_id)
            {
                if let Some(ref title) = u.title {
                    topic.title = title.clone();
                }
                if let Some(icon_id) = u.icon_emoji_id {
                    topic.custom_emoji = (icon_id != 0).then(|| icon_id.to_string());
                }
                if let Some(closed) = u.closed {
                    topic.is_closed = closed;
                }
                changed = true;
            }
            changed
        }
        tl_enums::Update::DraftMessage(u) => apply_draft_update_to_section_caches(state, u),
        tl_enums::Update::ReadChannelDiscussionInbox(u) => {
            apply_read_channel_discussion_inbox(state, u)
        }
        tl_enums::Update::ReadChannelDiscussionOutbox(u) => {
            apply_read_channel_discussion_outbox(state, u)
        }
        tl_enums::Update::SavedDialogPinned(u) => {
            if let tl_enums::DialogPeer::DialogPeer(dp) = &u.peer {
                let self_peer = from_data_peer(session.self_peer_id());
                let sub_peer = from_data_peer(peer_id_from_tl(&dp.peer));
                apply_saved_dialog_pin_state(state, self_peer, sub_peer, u.pinned);
                return true;
            }
            false
        }
        tl_enums::Update::PinnedSavedDialogs(u) => {
            let self_peer = from_data_peer(session.self_peer_id());
            let order: Vec<PeerId> = u
                .order
                .as_deref()
                .unwrap_or_default()
                .iter()
                .filter_map(|dp| match dp {
                    tl_enums::DialogPeer::DialogPeer(p) => {
                        Some(from_data_peer(peer_id_from_tl(&p.peer)))
                    }
                    _ => None,
                })
                .collect();
            apply_saved_pinned_order(state, self_peer, order);
            true
        }
        tl_enums::Update::NewChannelMessage(u) => {
            apply_message_to_section_caches(session, state, &u.message)
        }
        tl_enums::Update::EditChannelMessage(u) => {
            apply_message_to_section_caches(session, state, &u.message)
        }
        tl_enums::Update::NewMessage(u) => {
            apply_message_to_section_caches(session, state, &u.message)
        }
        tl_enums::Update::EditMessage(u) => {
            apply_message_to_section_caches(session, state, &u.message)
        }
        tl_enums::Update::DeleteMessages(u) => {
            let del_set: HashSet<i32> = u.messages.iter().copied().collect();
            let mut changed = false;
            let mut updated_counts: Vec<(PeerId, PeerId, usize)> = Vec::new();
            for (&(owner_peer, sub_peer), msgs) in &mut state.saved_sublist_messages {
                let before = msgs.len();
                msgs.retain(|m| !del_set.contains(&m.id.0));
                if msgs.len() != before {
                    changed = true;
                    updated_counts.push((owner_peer, sub_peer, msgs.len()));
                }
            }
            for (owner_peer, sub_peer, remaining) in updated_counts {
                if let Some(list) = state.saved_sublists.get_mut(&owner_peer)
                    && let Some(sub) = list.iter_mut().find(|s| s.sublist_peer_id == sub_peer)
                {
                    sub.message_count = remaining as u32;
                }
            }
            changed
        }
        tl_enums::Update::DeleteChannelMessages(u) => {
            let peer_id =
                from_data_peer(DataPeerId::from_channel(ChannelId::from_tl(u.channel_id)));
            let del_set: HashSet<i32> = u.messages.iter().copied().collect();
            let mut changed = false;
            for ((p, _), msgs) in &mut state.topic_messages {
                if *p == peer_id {
                    let before = msgs.len();
                    msgs.retain(|m| !del_set.contains(&m.id.0));
                    if msgs.len() != before {
                        changed = true;
                    }
                }
            }
            for meta in state.discussion_threads.values_mut() {
                if meta.discussion_peer_id == peer_id {
                    let before = meta.comments.len();
                    meta.comments.retain(|m| !del_set.contains(&m.id.0));
                    if meta.comments.len() != before {
                        changed = true;
                    }
                }
            }
            changed
        }
        _ => false,
    }
}

fn apply_message_to_section_caches(
    session: &Session,
    state: &mut LiveHistorySectionsState,
    raw_msg: &tl_enums::Message,
) -> bool {
    let mut changed = false;
    // 1. Handle topic create/edit service messages directly.
    if let tl_enums::Message::Service(svc) = raw_msg {
        let peer_id = from_data_peer(peer_id_from_tl(&svc.peer_id));
        let reply_top_id = svc.reply_to.as_ref().and_then(|r| match r {
            tl_enums::MessageReplyHeader::MessageReplyHeader(h) => {
                h.reply_to_top_id.or(h.reply_to_msg_id)
            }
            _ => None,
        });
        match &svc.action {
            tl_enums::MessageAction::TopicCreate(tc) => {
                let root_id = MsgId(svc.id);
                let list = state.forum_topics.entry(peer_id).or_default();
                if let Some(existing) = list.iter_mut().find(|t| t.root_id == root_id) {
                    existing.title = tc.title.clone();
                    existing.icon_color = tc.icon_color as u32;
                    existing.custom_emoji = tc
                        .icon_emoji_id
                        .filter(|&id| id != 0)
                        .map(|id| id.to_string());
                } else {
                    let from_name = svc
                        .from_id
                        .as_ref()
                        .map(|p| peer_display_title(session, peer_id_from_tl(p)))
                        .unwrap_or_default();
                    list.push(ForumTopic {
                        root_id,
                        peer_id,
                        title: tc.title.clone(),
                        icon_color: tc.icon_color as u32,
                        custom_emoji: tc
                            .icon_emoji_id
                            .filter(|&id| id != 0)
                            .map(|id| id.to_string()),
                        is_general: root_id == FORUM_GENERAL_TOPIC_ID,
                        is_pinned: false,
                        is_closed: false,
                        muted: false,
                        unread_count: 0,
                        unread_mentions: 0,
                        last_sender: from_name,
                        last_preview: "Topic created".to_owned(),
                        last_date_unix: svc.date,
                        draft_preview: None,
                    });
                    let order = state.forum_pinned_order.get(&peer_id).cloned();
                    sort_forum_topics(list, order.as_deref());
                }
                changed = true;
            }
            tl_enums::MessageAction::TopicEdit(te) => {
                let topic_id = MsgId(reply_top_id.unwrap_or(FORUM_GENERAL_TOPIC_ID.0));
                if let Some(hidden) = te.hidden {
                    if hidden {
                        state.forum_hidden_topics.insert((peer_id, topic_id));
                    } else {
                        state.forum_hidden_topics.remove(&(peer_id, topic_id));
                    }
                    changed = true;
                }
                if let Some(list) = state.forum_topics.get_mut(&peer_id)
                    && let Some(topic) = list.iter_mut().find(|t| t.root_id == topic_id)
                {
                    if let Some(ref title) = te.title {
                        topic.title = title.clone();
                    }
                    if let Some(icon_id) = te.icon_emoji_id {
                        topic.custom_emoji = (icon_id != 0).then(|| icon_id.to_string());
                    }
                    if let Some(closed) = te.closed {
                        topic.is_closed = closed;
                    }
                    changed = true;
                }
            }
            _ => {}
        }
    }

    // 2. Route incoming/edited regular messages into cached topic/discussion feeds & poll index.
    let self_uid = session.self_user_id();
    let Some(dm) = message_from_tl(raw_msg, self_uid) else {
        return changed;
    };
    if let Some(MessageMedia::Poll(poll)) = dm.media() {
        index_poll_message(state, poll.id.0 as i64, dm.peer_id, dm.id);
    }
    let peer_id = from_data_peer(dm.peer_id);
    let top_msg_id = dm
        .reply_to
        .as_ref()
        .and_then(|r| r.top_msg_id.or(r.reply_to_msg_id))
        .map(from_data_msg);

    if let Some(tid) = top_msg_id {
        if state.topic_messages.contains_key(&(peer_id, tid)) {
            let mut hm = message_to_history_message(session, &dm);
            hm.extra.thread_id = Some(tid);
            upsert_topic_message(state, peer_id, tid, hm);
            changed = true;
        } else if let Some(list) = state.forum_topics.get_mut(&peer_id)
            && let Some(topic) = list.iter_mut().find(|t| t.root_id == tid)
        {
            let hm = message_to_history_message(session, &dm);
            topic.last_sender = hm.sender_name.clone();
            topic.last_preview = hm.preview_snippet();
            topic.last_date_unix = hm.date_unix;
            changed = true;
        }

        for ((_, root_msg_id), meta) in &mut state.discussion_threads {
            if meta.discussion_peer_id == peer_id && meta.discussion_root_msg_id == tid {
                let mut hm = message_to_history_message(session, &dm);
                hm.extra.thread_id = Some(*root_msg_id);
                upsert_ordered_message(&mut meta.comments, hm);
                if meta.max_id.is_none_or(|cur| from_data_msg(dm.id).0 > cur.0) {
                    meta.max_id = Some(from_data_msg(dm.id));
                }
                changed = true;
            }
        }
    }

    // Mirror new/edited messages in Saved Messages into cached saved sublists.
    if dm.peer_id == session.self_peer_id() {
        let sub_peer = dm
            .forward_from
            .as_ref()
            .and_then(|f| f.saved_from_peer)
            .map_or(peer_id, from_data_peer);
        if state
            .saved_sublist_messages
            .contains_key(&(peer_id, sub_peer))
        {
            let mut hm = message_to_history_message(session, &dm);
            hm.extra.saved_from_peer = Some(sub_peer);
            let preview = hm.preview_snippet();
            let date_unix = hm.date_unix;
            let list = state
                .saved_sublist_messages
                .entry((peer_id, sub_peer))
                .or_default();
            upsert_ordered_message(list, hm);
            let count = list.len() as u32;
            if let Some(subs) = state.saved_sublists.get_mut(&peer_id)
                && let Some(sub) = subs.iter_mut().find(|s| s.sublist_peer_id == sub_peer)
            {
                sub.message_count = count;
                if date_unix >= sub.last_date_unix {
                    sub.last_preview = preview;
                    sub.last_date_unix = date_unix;
                }
            }
            changed = true;
        }
    }

    changed
}

fn apply_draft_update_to_section_caches(
    state: &mut LiveHistorySectionsState,
    u: &tl_types::UpdateDraftMessage,
) -> bool {
    let peer_id = from_data_peer(peer_id_from_tl(&u.peer));
    let rich_draft = match &u.draft {
        tl_enums::DraftMessage::DraftMessage(dm) => {
            let data_ents = dm
                .entities
                .as_deref()
                .map(entities_from_tl)
                .unwrap_or_default();
            let data_text = tg_data::TextWithEntities::new(dm.message.clone(), data_ents);
            let ui_text = data_text_to_ui_text(&data_text);
            let (reply_to, reply_quote) = match dm.reply_to.as_ref() {
                Some(tl_enums::InputReplyTo::Message(r)) => {
                    let quote = r.quote_text.as_ref().filter(|s| !s.is_empty()).map(|qt| {
                        let q_ents = r
                            .quote_entities
                            .as_deref()
                            .map(entities_from_tl)
                            .unwrap_or_default();
                        let ui_qt = data_text_to_ui_text(&tg_data::TextWithEntities::new(
                            qt.clone(),
                            q_ents,
                        ));
                        ReplyQuote {
                            text: ui_qt.text,
                            entities: ui_qt.entities,
                            offset_utf16: r.quote_offset.unwrap_or(0),
                        }
                    });
                    let rid = (r.reply_to_msg_id > 0).then_some(MsgId(r.reply_to_msg_id));
                    (rid, quote)
                }
                _ => (None, None),
            };
            if ui_text.text.is_empty() && reply_to.is_none() {
                None
            } else {
                Some(RichThreadDraft {
                    text: ui_text,
                    reply_to,
                    reply_quote,
                    no_webpage: dm.no_webpage,
                })
            }
        }
        tl_enums::DraftMessage::Empty(_) => None,
    };

    let mut keys = Vec::new();
    if let Some(ref sp) = u.saved_peer_id {
        keys.push(ThreadKey::SavedSublist {
            peer_id,
            sublist_peer_id: from_data_peer(peer_id_from_tl(sp)),
        });
    } else if let Some(top) = u.top_msg_id {
        let top_id = MsgId(top);
        keys.push(ThreadKey::Topic {
            peer_id,
            topic_root_id: top_id,
        });
        for (&(chan_peer, root_msg_id), meta) in &state.discussion_threads {
            if meta.discussion_peer_id == peer_id && meta.discussion_root_msg_id == top_id {
                keys.push(ThreadKey::Replies {
                    peer_id: chan_peer,
                    root_msg_id,
                });
            }
        }
        if let Some(list) = state.forum_topics.get_mut(&peer_id)
            && let Some(topic) = list.iter_mut().find(|t| t.root_id == top_id)
        {
            topic.draft_preview = rich_draft
                .as_ref()
                .filter(|d| !d.text.text.trim().is_empty())
                .map(|d| d.text.text.clone());
        }
    } else {
        keys.push(ThreadKey::Main(peer_id));
    }

    for key in keys {
        match &rich_draft {
            Some(d) => {
                state.rich_thread_drafts.insert(key, d.clone());
            }
            None => {
                state.rich_thread_drafts.remove(&key);
            }
        }
    }
    true
}

fn apply_update_message_poll(
    session: &mut Session,
    state: &mut LiveHistorySectionsState,
    u: &tl_types::UpdateMessagePoll,
) -> bool {
    let poll_id_u64 = u.poll_id as u64;
    let mut targets: Vec<(DataPeerId, DataMsgId)> = Vec::new();

    if let (Some(peer), Some(msg_id)) = (&u.peer, u.msg_id) {
        let pair = (peer_id_from_tl(peer), DataMsgId(i64::from(msg_id)));
        targets.push(pair);
        index_poll_message(state, u.poll_id, pair.0, pair.1);
    }
    if let Some(indexed) = state.poll_message_index.get(&u.poll_id) {
        for &pair in indexed {
            if !targets.contains(&pair) {
                targets.push(pair);
            }
        }
    }
    // Fallback scan across loaded histories in `Session` if poll was loaded before index saw it.
    if targets.is_empty() {
        for pid in known_peer_ids(session) {
            for msg in history_messages_ordered(session, pid) {
                if let Some(MessageMedia::Poll(p)) = msg.media()
                    && p.id.0 == poll_id_u64
                {
                    let pair = (pid, msg.id);
                    if !targets.contains(&pair) {
                        targets.push(pair);
                    }
                }
            }
        }
        for &(pid, mid) in &targets {
            index_poll_message(state, u.poll_id, pid, mid);
        }
    }

    let mut updated_any = false;
    let mut updated_attachments: Vec<(DataPeerId, DataMsgId, HistoryMessage)> = Vec::new();
    for (pid, mid) in targets {
        let full_id = DataFullMsgId::new(pid, mid);
        if let Some(msg) = session.message_mut(full_id)
            && let MessageContent::Regular {
                media: MessageMedia::Poll(ref mut poll_data),
                ..
            } = msg.content
        {
            apply_poll_update_to_data(poll_data, u.poll.as_ref(), &u.results);
            updated_any = true;
            session
                .changes()
                .message_updated(full_id, tg_data::MessageUpdateFlags::EDITED);
        }
        if updated_any && let Some(msg) = session.message(full_id) {
            updated_attachments.push((pid, mid, message_to_history_message(session, msg)));
        }
    }

    // Propagate refreshed HistoryMessage into any section caches holding this message.
    for (dpid, dmid, hm) in updated_attachments {
        let pid = from_data_peer(dpid);
        let mid = from_data_msg(dmid);
        for ((p, _), msgs) in &mut state.topic_messages {
            if *p == pid
                && let Some(slot) = msgs.iter_mut().find(|m| m.id == mid)
            {
                slot.media = hm.media.clone();
            }
        }
        for meta in state.discussion_threads.values_mut() {
            if meta.discussion_peer_id == pid
                && let Some(slot) = meta.comments.iter_mut().find(|m| m.id == mid)
            {
                slot.media = hm.media.clone();
            }
        }
    }

    updated_any
}

fn apply_poll_update_to_data(
    poll_data: &mut PollData,
    tl_poll_opt: Option<&tl_enums::Poll>,
    tl_results: &tl_enums::PollResults,
) {
    if let Some(tl_enums::Poll::Poll(p)) = tl_poll_opt {
        poll_data.id = PollId::from_tl(p.id);
        poll_data.question = text_with_entities_from_tl(&p.question);
        poll_data.closed = p.closed;
        poll_data.public_voters = p.public_voters;
        poll_data.multiple_choice = p.multiple_choice;
        poll_data.quiz = p.quiz;
        poll_data.close_period = p.close_period;
        poll_data.close_date = p.close_date;
        poll_data.answers = p
            .answers
            .iter()
            .map(|a| match a {
                tl_enums::PollAnswer::PollAnswer(ans) => PollAnswer {
                    text: text_with_entities_from_tl(&ans.text),
                    option: ans.option.clone(),
                },
                tl_enums::PollAnswer::InputPollAnswer(ans) => PollAnswer {
                    text: text_with_entities_from_tl(&ans.text),
                    option: Vec::new(),
                },
            })
            .collect();
    }

    let tl_enums::PollResults::PollResults(res) = tl_results;
    let is_min = res.min;
    if let Some(total) = res.total_voters {
        poll_data.results.total_voters = Some(total);
    }
    if let Some(ref voters) = res.recent_voters {
        poll_data.results.recent_voters = voters.iter().map(peer_id_from_tl).collect();
    }
    if let Some(ref sol) = res.solution {
        let ents = res
            .solution_entities
            .as_deref()
            .map(entities_from_tl)
            .unwrap_or_default();
        poll_data.results.solution = Some(tg_data::TextWithEntities::new(sol.clone(), ents));
    }
    if let Some(ref incoming_results) = res.results {
        let mut next_results: Vec<PollAnswerVoters> = Vec::with_capacity(incoming_results.len());
        for raw_av in incoming_results {
            let tl_enums::PollAnswerVoters::PollAnswerVoters(av) = raw_av;
            let prev = poll_data
                .results
                .results
                .iter()
                .find(|r| r.option == av.option);
            // Matches tdesktop `PollData::applyResultToAnswers`: when `min == true`, preserve `chosen`.
            let chosen = if is_min {
                prev.is_some_and(|p| p.chosen)
            } else {
                av.chosen
            };
            let correct = av.correct || prev.is_some_and(|p| p.correct);
            let voters = av.voters.unwrap_or_else(|| prev.map_or(0, |p| p.voters));
            next_results.push(PollAnswerVoters {
                option: av.option.clone(),
                voters,
                chosen,
                correct,
            });
        }
        poll_data.results.results = next_results;
    } else if !is_min && poll_data.results.results.is_empty() {
        poll_data.results = PollResults {
            total_voters: res.total_voters,
            ..PollResults::default()
        };
    }
}

fn apply_update_message_poll_vote(
    session: &mut Session,
    state: &mut LiveHistorySectionsState,
    u: &tl_types::UpdateMessagePollVote,
) -> bool {
    let voter_data_peer = peer_id_from_tl(&u.peer);
    let voter_peer = from_data_peer(voter_data_peer);

    // Update voter-by-option map: first remove previous choices of this voter, then add new ones.
    let by_option = state.poll_voters_by_option.entry(u.poll_id).or_default();
    for voters in by_option.values_mut() {
        voters.retain(|&p| p != voter_peer);
    }
    for opt in &u.options {
        let entry = by_option.entry(opt.clone()).or_default();
        if !entry.contains(&voter_peer) {
            entry.push(voter_peer);
        }
    }

    // Mirror recent_voters on any matching loaded PollData in `Session`.
    let poll_id_u64 = u.poll_id as u64;
    let mut targets = state
        .poll_message_index
        .get(&u.poll_id)
        .cloned()
        .unwrap_or_default();
    if targets.is_empty() {
        for pid in known_peer_ids(session) {
            for msg in history_messages_ordered(session, pid) {
                if let Some(MessageMedia::Poll(p)) = msg.media()
                    && p.id.0 == poll_id_u64
                {
                    targets.push((pid, msg.id));
                }
            }
        }
    }
    for (pid, mid) in targets {
        let full_id = DataFullMsgId::new(pid, mid);
        if let Some(msg) = session.message_mut(full_id)
            && let MessageContent::Regular {
                media: MessageMedia::Poll(ref mut poll_data),
                ..
            } = msg.content
        {
            if u.options.is_empty() {
                poll_data
                    .results
                    .recent_voters
                    .retain(|&p| p != voter_data_peer);
            } else if !poll_data.results.recent_voters.contains(&voter_data_peer) {
                poll_data.results.recent_voters.push(voter_data_peer);
            }
            session
                .changes()
                .message_updated(full_id, tg_data::MessageUpdateFlags::EDITED);
        }
    }
    true
}

fn apply_read_channel_discussion_inbox(
    state: &mut LiveHistorySectionsState,
    u: &tl_types::UpdateReadChannelDiscussionInbox,
) -> bool {
    let channel_peer = from_data_peer(DataPeerId::from_channel(ChannelId::from_tl(u.channel_id)));
    let top_msg_id = MsgId(u.top_msg_id);
    let read_max = MsgId(u.read_max_id);
    let mut changed = false;

    if let (Some(bcast_id), Some(bcast_post)) = (u.broadcast_id, u.broadcast_post) {
        let bcast_peer = from_data_peer(DataPeerId::from_channel(ChannelId::from_tl(bcast_id)));
        if let Some(meta) = state
            .discussion_threads
            .get_mut(&(bcast_peer, MsgId(bcast_post)))
        {
            meta.read_inbox_max_id = Some(read_max);
            if meta.max_id.is_some_and(|max_id| read_max.0 >= max_id.0) {
                meta.unread_count = 0;
            } else if !meta.comments.is_empty() {
                meta.unread_count = meta
                    .comments
                    .iter()
                    .filter(|m| !m.outgoing && m.id.0 > read_max.0)
                    .count() as i32;
            }
            changed = true;
        }
    }

    for meta in state.discussion_threads.values_mut() {
        if meta.discussion_peer_id == channel_peer && meta.discussion_root_msg_id == top_msg_id {
            meta.read_inbox_max_id = Some(read_max);
            if meta.max_id.is_some_and(|max_id| read_max.0 >= max_id.0) {
                meta.unread_count = 0;
            } else if !meta.comments.is_empty() {
                meta.unread_count = meta
                    .comments
                    .iter()
                    .filter(|m| !m.outgoing && m.id.0 > read_max.0)
                    .count() as i32;
            }
            changed = true;
        }
    }

    // Also update forum topic unread counter if `channel_peer` is a forum.
    if let Some(list) = state.forum_topics.get_mut(&channel_peer)
        && let Some(topic) = list.iter_mut().find(|t| t.root_id == top_msg_id)
    {
        let unread_remaining = state
            .topic_messages
            .get(&(channel_peer, top_msg_id))
            .map(|msgs| {
                msgs.iter()
                    .filter(|m| !m.outgoing && m.id.0 > read_max.0)
                    .count() as u32
            })
            .unwrap_or(0);
        topic.unread_count = unread_remaining;
        changed = true;
    }

    changed
}

fn apply_read_channel_discussion_outbox(
    state: &mut LiveHistorySectionsState,
    u: &tl_types::UpdateReadChannelDiscussionOutbox,
) -> bool {
    let channel_peer = from_data_peer(DataPeerId::from_channel(ChannelId::from_tl(u.channel_id)));
    let top_msg_id = MsgId(u.top_msg_id);
    let read_max = MsgId(u.read_max_id);
    let mut changed = false;

    for meta in state.discussion_threads.values_mut() {
        if meta.discussion_peer_id == channel_peer && meta.discussion_root_msg_id == top_msg_id {
            meta.read_outbox_max_id = Some(read_max);
            for msg in &mut meta.comments {
                if msg.outgoing && msg.id.0 <= read_max.0 {
                    msg.is_read = true;
                }
            }
            changed = true;
        }
    }

    if let Some(msgs) = state.topic_messages.get_mut(&(channel_peer, top_msg_id)) {
        for msg in msgs {
            if msg.outgoing && msg.id.0 <= read_max.0 {
                msg.is_read = true;
                changed = true;
            }
        }
    }

    changed
}

// =============================================================================
// Cache maintenance & sorting helpers
// =============================================================================

fn index_poll_message(
    state: &mut LiveHistorySectionsState,
    poll_id: i64,
    peer_id: DataPeerId,
    msg_id: DataMsgId,
) {
    let list = state.poll_message_index.entry(poll_id).or_default();
    let pair = (peer_id, msg_id);
    if !list.contains(&pair) {
        list.push(pair);
    }
}

fn apply_forum_topic_pin_state(
    state: &mut LiveHistorySectionsState,
    peer_id: PeerId,
    topic_root_id: MsgId,
    pinned: bool,
) {
    let order = state.forum_pinned_order.entry(peer_id).or_default();
    order.retain(|&id| id != topic_root_id);
    if pinned {
        order.insert(0, topic_root_id);
    }
    let order_clone = order.clone();
    if let Some(list) = state.forum_topics.get_mut(&peer_id) {
        if let Some(topic) = list.iter_mut().find(|t| t.root_id == topic_root_id) {
            topic.is_pinned = pinned;
        }
        sort_forum_topics(list, Some(&order_clone));
    }
}

fn apply_forum_pinned_order(
    state: &mut LiveHistorySectionsState,
    peer_id: PeerId,
    order: Vec<MsgId>,
) {
    let pinned_set: HashSet<MsgId> = order.iter().copied().collect();
    state.forum_pinned_order.insert(peer_id, order.clone());
    if let Some(list) = state.forum_topics.get_mut(&peer_id) {
        for topic in list.iter_mut() {
            topic.is_pinned = pinned_set.contains(&topic.root_id);
        }
        sort_forum_topics(list, Some(&order));
    }
}

fn sort_forum_topics(topics: &mut [ForumTopic], pinned_order: Option<&[MsgId]>) {
    topics.sort_by(|a, b| {
        b.is_pinned
            .cmp(&a.is_pinned)
            .then_with(|| {
                if a.is_pinned
                    && b.is_pinned
                    && let Some(order) = pinned_order
                {
                    let ia = order
                        .iter()
                        .position(|&id| id == a.root_id)
                        .unwrap_or(usize::MAX);
                    let ib = order
                        .iter()
                        .position(|&id| id == b.root_id)
                        .unwrap_or(usize::MAX);
                    return ia.cmp(&ib);
                }
                std::cmp::Ordering::Equal
            })
            .then_with(|| b.last_date_unix.cmp(&a.last_date_unix))
            .then_with(|| b.root_id.0.cmp(&a.root_id.0))
    });
}

fn apply_saved_dialog_pin_state(
    state: &mut LiveHistorySectionsState,
    peer_id: PeerId,
    sublist_peer_id: PeerId,
    pinned: bool,
) {
    let order = state.saved_pinned_order.entry(peer_id).or_default();
    order.retain(|&p| p != sublist_peer_id);
    if pinned {
        order.insert(0, sublist_peer_id);
    }
    let order_clone = order.clone();
    if let Some(list) = state.saved_sublists.get_mut(&peer_id) {
        if let Some(sub) = list
            .iter_mut()
            .find(|s| s.sublist_peer_id == sublist_peer_id)
        {
            sub.is_pinned = pinned;
        }
        sort_saved_sublists(list, Some(&order_clone));
    }
}

fn apply_saved_pinned_order(
    state: &mut LiveHistorySectionsState,
    peer_id: PeerId,
    order: Vec<PeerId>,
) {
    let pinned_set: HashSet<PeerId> = order.iter().copied().collect();
    state.saved_pinned_order.insert(peer_id, order.clone());
    if let Some(list) = state.saved_sublists.get_mut(&peer_id) {
        for sub in list.iter_mut() {
            sub.is_pinned = pinned_set.contains(&sub.sublist_peer_id);
        }
        sort_saved_sublists(list, Some(&order));
    }
}

fn sort_saved_sublists(sublists: &mut [SavedSublist], pinned_order: Option<&[PeerId]>) {
    sublists.sort_by(|a, b| {
        b.is_pinned
            .cmp(&a.is_pinned)
            .then_with(|| {
                if a.is_pinned
                    && b.is_pinned
                    && let Some(order) = pinned_order
                {
                    let ia = order
                        .iter()
                        .position(|&p| p == a.sublist_peer_id)
                        .unwrap_or(usize::MAX);
                    let ib = order
                        .iter()
                        .position(|&p| p == b.sublist_peer_id)
                        .unwrap_or(usize::MAX);
                    return ia.cmp(&ib);
                }
                std::cmp::Ordering::Equal
            })
            .then_with(|| b.last_date_unix.cmp(&a.last_date_unix))
    });
}

fn scheduled_history_message_from_data(session: &Session, dm: &tg_data::Message) -> HistoryMessage {
    let mut hm = message_to_history_message(session, dm);
    let remote_id = scheduled_local_to_remote_id(dm.id).unwrap_or(dm.id);
    hm.id = from_data_msg(remote_id);
    hm.extra.scheduled_date = Some(hm.date_unix);
    hm
}

fn forum_topic_from_data(
    session: &Session,
    peer_id: PeerId,
    topic: &tg_data::ForumTopicData,
    now: i32,
) -> ForumTopic {
    let data_peer = to_data_peer(peer_id);
    let root_id = from_data_msg(topic.root_id);
    let (last_sender, last_preview, last_date_unix) = topic
        .top_message_id
        .and_then(|top_id| session.message(DataFullMsgId::new(data_peer, top_id)))
        .map(|msg| {
            let hm = message_to_history_message(session, msg);
            (hm.sender_name.clone(), hm.preview_snippet(), hm.date_unix)
        })
        .unwrap_or_else(|| {
            let sender = if topic.creator_id.is_zero() {
                String::new()
            } else {
                peer_display_title(session, topic.creator_id)
            };
            let date = if topic.last_message_date > 0 {
                topic.last_message_date
            } else {
                topic.creation_date
            };
            (sender, String::new(), date)
        });
    let draft_preview = topic
        .draft
        .as_ref()
        .filter(|d| !d.text.text.is_empty())
        .map(|d| d.text.text.clone());
    ForumTopic {
        root_id,
        peer_id,
        title: topic.title.clone(),
        icon_color: topic.icon_color as u32,
        custom_emoji: topic.icon_emoji_id.map(|id| id.0.to_string()),
        is_general: root_id == FORUM_GENERAL_TOPIC_ID,
        is_pinned: topic.pinned,
        is_closed: topic.closed,
        muted: topic.notify.is_muted(now),
        unread_count: topic.unread_count.unwrap_or(0).max(0) as u32,
        unread_mentions: topic.unread_mentions_count.max(0) as u32,
        last_sender,
        last_preview,
        last_date_unix,
        draft_preview,
    }
}

fn rebuild_forum_topics_from_session(
    session: &Session,
    peer_id: PeerId,
    now: i32,
) -> (Vec<ForumTopic>, Vec<MsgId>, Vec<MsgId>) {
    let Some(forum) = session.forum(to_data_peer(peer_id)) else {
        return (Vec::new(), Vec::new(), Vec::new());
    };
    let mut topics = Vec::with_capacity(forum.topics.len());
    let mut hidden = Vec::new();
    for topic in forum.topics.values() {
        let ui = forum_topic_from_data(session, peer_id, topic, now);
        if topic.hidden {
            hidden.push(ui.root_id);
        }
        topics.push(ui);
    }
    let pinned: Vec<MsgId> = forum
        .pinned_order
        .iter()
        .copied()
        .map(from_data_msg)
        .collect();
    (topics, hidden, pinned)
}

fn rebuild_saved_sublists_from_session(session: &Session) -> (Vec<SavedSublist>, Vec<PeerId>) {
    let saved = session.saved_messages();
    let self_data_peer = session.self_peer_id();
    let self_pid = from_data_peer(self_data_peer);
    let mut sublists = Vec::with_capacity(saved.sublists.len());
    for sub in saved.sublists.values() {
        let sub_data_peer = sub.peer_id;
        let sub_peer = from_data_peer(sub_data_peer);
        let is_my_notes = sub_peer == self_pid;
        let title = if is_my_notes {
            "My Notes".to_owned()
        } else {
            peer_display_title(session, sub_data_peer)
        };
        let color_idx = session.peer(sub_data_peer).map_or_else(
            || tg_data::decide_color_index(sub_data_peer),
            |p| p.common().color_index,
        );
        let (last_preview, last_date_unix) = sub
            .top_message_id
            .and_then(|top_id| session.message(DataFullMsgId::new(self_data_peer, top_id)))
            .map(|m| {
                let hm = message_to_history_message(session, m);
                (hm.preview_snippet(), hm.date_unix)
            })
            .unwrap_or_else(|| (String::new(), sub.last_message_date));
        sublists.push(SavedSublist {
            sublist_peer_id: sub_peer,
            initials: initials_from_name(&title),
            title,
            avatar_rgb: avatar_rgb_for_color_index(color_idx),
            is_my_notes,
            is_pinned: sub.pinned,
            message_count: u32::from(sub.top_message_id.is_some_and(|id| id.0 > 0)),
            last_preview,
            last_date_unix,
        });
    }
    let pinned: Vec<PeerId> = saved
        .pinned_order
        .iter()
        .copied()
        .map(from_data_peer)
        .collect();
    (sublists, pinned)
}

fn sort_scheduled_messages(msgs: &mut [HistoryMessage]) {
    msgs.sort_by_key(|m| (m.extra.scheduled_date.unwrap_or(m.date_unix), m.id.0));
}

fn upsert_scheduled_message(
    state: &mut LiveHistorySectionsState,
    peer_id: PeerId,
    msg: HistoryMessage,
) {
    let list = state.scheduled_messages.entry(peer_id).or_default();
    if let Some(existing) = list.iter_mut().find(|m| m.id == msg.id) {
        *existing = msg;
    } else {
        list.push(msg);
    }
    sort_scheduled_messages(list);
}

fn upsert_topic_message(
    state: &mut LiveHistorySectionsState,
    peer_id: PeerId,
    topic_root_id: MsgId,
    msg: HistoryMessage,
) {
    if let Some(list) = state.forum_topics.get_mut(&peer_id)
        && let Some(topic) = list.iter_mut().find(|t| t.root_id == topic_root_id)
    {
        topic.last_sender = msg.sender_name.clone();
        topic.last_preview = msg.preview_snippet();
        topic.last_date_unix = msg.date_unix;
    }
    let list = state
        .topic_messages
        .entry((peer_id, topic_root_id))
        .or_default();
    upsert_ordered_message(list, msg);
}

fn upsert_ordered_message(list: &mut Vec<HistoryMessage>, msg: HistoryMessage) {
    if let Some(existing) = list.iter_mut().find(|m| m.id == msg.id) {
        *existing = msg;
    } else {
        list.push(msg);
    }
    list.sort_by_key(|m| m.id.0);
}

const fn thread_key_msg_id(key: Option<ThreadKey>) -> Option<MsgId> {
    match key {
        Some(ThreadKey::Topic { topic_root_id, .. }) => Some(topic_root_id),
        Some(ThreadKey::Replies { root_msg_id, .. }) => Some(root_msg_id),
        _ => None,
    }
}

// =============================================================================
// Admin log TL conversion helpers
// =============================================================================

fn admin_log_flags_to_tl(flags: AdminLogFilterFlags) -> tl_enums::ChannelAdminLogEventsFilter {
    tl_enums::ChannelAdminLogEventsFilter::ChannelAdminLogEventsFilter(
        tl_types::ChannelAdminLogEventsFilter {
            join: flags.contains(AdminLogFilterFlags::JOIN),
            leave: flags.contains(AdminLogFilterFlags::LEAVE),
            invite: flags.contains(AdminLogFilterFlags::INVITE),
            ban: flags.contains(AdminLogFilterFlags::BAN),
            unban: flags.contains(AdminLogFilterFlags::UNBAN),
            kick: flags.contains(AdminLogFilterFlags::KICK),
            unkick: flags.contains(AdminLogFilterFlags::UNKICK),
            promote: flags.contains(AdminLogFilterFlags::PROMOTE),
            demote: flags.contains(AdminLogFilterFlags::DEMOTE),
            info: flags.contains(AdminLogFilterFlags::INFO),
            settings: flags.contains(AdminLogFilterFlags::SETTINGS),
            pinned: flags.contains(AdminLogFilterFlags::PINNED),
            edit: flags.contains(AdminLogFilterFlags::EDIT),
            delete: flags.contains(AdminLogFilterFlags::DELETE),
            group_call: flags.contains(AdminLogFilterFlags::GROUP_CALL),
            invites: flags.contains(AdminLogFilterFlags::INVITES),
            send: false,
            forums: flags.contains(AdminLogFilterFlags::TOPICS),
            sub_extend: flags.contains(AdminLogFilterFlags::SUB_EXTEND),
            edit_rank: flags.contains(AdminLogFilterFlags::EDIT_RANK),
        },
    )
}

fn channel_participant_to_admin(
    session: &Session,
    participant: &tl_enums::ChannelParticipant,
) -> Option<AdminLogAdmin> {
    let (uid, role) = match participant {
        tl_enums::ChannelParticipant::Creator(c) => (
            DataUserId::from_tl(c.user_id),
            c.rank
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("Owner")
                .to_owned(),
        ),
        tl_enums::ChannelParticipant::Admin(a) => (
            DataUserId::from_tl(a.user_id),
            a.rank
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("Admin")
                .to_owned(),
        ),
        tl_enums::ChannelParticipant::ChannelParticipant(p) => (
            DataUserId::from_tl(p.user_id),
            p.rank
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("Admin")
                .to_owned(),
        ),
        tl_enums::ChannelParticipant::Self_(p) => (
            DataUserId::from_tl(p.user_id),
            p.rank
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("Admin")
                .to_owned(),
        ),
        _ => return None,
    };
    let dpid = DataPeerId::from_user(uid);
    Some(AdminLogAdmin {
        peer_id: from_data_peer(dpid),
        name: peer_display_title(session, dpid),
        role,
    })
}

fn tl_message_text_preview(session: &Session, msg: &tl_enums::Message) -> Option<String> {
    match msg {
        tl_enums::Message::Message(m) => {
            if !m.message.is_empty() {
                Some(m.message.clone())
            } else {
                message_from_tl(msg, session.self_user_id())
                    .map(|dm| message_to_history_message(session, &dm).preview_snippet())
            }
        }
        tl_enums::Message::Service(_) => message_from_tl(msg, session.self_user_id())
            .map(|dm| message_to_history_message(session, &dm).preview_snippet()),
        tl_enums::Message::Empty(_) => None,
    }
}

fn tl_topic_title(topic: &tl_enums::ForumTopic) -> String {
    match topic {
        tl_enums::ForumTopic::ForumTopic(t) => t.title.clone(),
        tl_enums::ForumTopic::Deleted(d) => format!("Topic #{}", d.id),
    }
}

fn tl_participant_peer_name(session: &Session, p: &tl_enums::ChannelParticipant) -> String {
    match p {
        tl_enums::ChannelParticipant::ChannelParticipant(x) => peer_display_title(
            session,
            DataPeerId::from_user(DataUserId::from_tl(x.user_id)),
        ),
        tl_enums::ChannelParticipant::Self_(x) => peer_display_title(
            session,
            DataPeerId::from_user(DataUserId::from_tl(x.user_id)),
        ),
        tl_enums::ChannelParticipant::Creator(x) => peer_display_title(
            session,
            DataPeerId::from_user(DataUserId::from_tl(x.user_id)),
        ),
        tl_enums::ChannelParticipant::Admin(x) => peer_display_title(
            session,
            DataPeerId::from_user(DataUserId::from_tl(x.user_id)),
        ),
        tl_enums::ChannelParticipant::Banned(x) => {
            peer_display_title(session, peer_id_from_tl(&x.peer))
        }
        tl_enums::ChannelParticipant::Left(x) => {
            peer_display_title(session, peer_id_from_tl(&x.peer))
        }
    }
}

fn tl_invite_link(invite: &tl_enums::ExportedChatInvite) -> String {
    match invite {
        tl_enums::ExportedChatInvite::ChatInviteExported(i) => i.link.clone(),
        tl_enums::ExportedChatInvite::ChatInvitePublicJoinRequests => {
            "Public join requests".to_owned()
        }
    }
}

fn admin_log_event_from_tl(
    session: &Session,
    event: &tl_enums::ChannelAdminLogEvent,
) -> AdminLogEntry {
    let tl_enums::ChannelAdminLogEvent::ChannelAdminLogEvent(ev) = event;
    let actor_data_peer = DataPeerId::from_user(DataUserId::from_tl(ev.user_id));
    let actor_id = from_data_peer(actor_data_peer);
    let actor_name = peer_display_title(session, actor_data_peer);

    use tl_enums::ChannelAdminLogEventAction as A;
    let (flag, summary, before_text, after_text) = match &ev.action {
        A::ChangeTitle(a) => (
            AdminLogFilterFlags::INFO,
            "Changed group/channel title".to_owned(),
            Some(a.prev_value.clone()),
            Some(a.new_value.clone()),
        ),
        A::ChangeAbout(a) => (
            AdminLogFilterFlags::INFO,
            "Changed description".to_owned(),
            (!a.prev_value.is_empty()).then(|| a.prev_value.clone()),
            (!a.new_value.is_empty()).then(|| a.new_value.clone()),
        ),
        A::ChangeUsername(a) => (
            AdminLogFilterFlags::INFO,
            "Changed username".to_owned(),
            (!a.prev_value.is_empty()).then(|| format!("@{}", a.prev_value)),
            (!a.new_value.is_empty()).then(|| format!("@{}", a.new_value)),
        ),
        A::ChangeUsernames(a) => (
            AdminLogFilterFlags::INFO,
            "Updated usernames".to_owned(),
            (!a.prev_value.is_empty()).then(|| a.prev_value.join(", ")),
            (!a.new_value.is_empty()).then(|| a.new_value.join(", ")),
        ),
        A::ChangePhoto(_) => (
            AdminLogFilterFlags::INFO,
            "Changed group/channel photo".to_owned(),
            None,
            None,
        ),
        A::ToggleInvites(a) => (
            AdminLogFilterFlags::SETTINGS,
            if a.new_value {
                "Enabled member invitations".to_owned()
            } else {
                "Disabled member invitations".to_owned()
            },
            None,
            None,
        ),
        A::ToggleSignatures(a) => (
            AdminLogFilterFlags::SETTINGS,
            if a.new_value {
                "Enabled post signatures".to_owned()
            } else {
                "Disabled post signatures".to_owned()
            },
            None,
            None,
        ),
        A::ToggleSignatureProfiles(a) => (
            AdminLogFilterFlags::SETTINGS,
            if a.new_value {
                "Enabled author profiles on signatures".to_owned()
            } else {
                "Disabled author profiles on signatures".to_owned()
            },
            None,
            None,
        ),
        A::UpdatePinned(a) => (
            AdminLogFilterFlags::PINNED,
            "Updated pinned message".to_owned(),
            None,
            tl_message_text_preview(session, &a.message),
        ),
        A::EditMessage(a) => (
            AdminLogFilterFlags::EDIT,
            "Edited a message".to_owned(),
            tl_message_text_preview(session, &a.prev_message),
            tl_message_text_preview(session, &a.new_message),
        ),
        A::DeleteMessage(a) => (
            AdminLogFilterFlags::DELETE,
            "Deleted a message".to_owned(),
            tl_message_text_preview(session, &a.message),
            None,
        ),
        A::ParticipantJoin => (
            AdminLogFilterFlags::JOIN,
            "Joined the group/channel".to_owned(),
            None,
            None,
        ),
        A::ParticipantLeave => (
            AdminLogFilterFlags::LEAVE,
            "Left the group/channel".to_owned(),
            None,
            None,
        ),
        A::ParticipantInvite(a) => (
            AdminLogFilterFlags::INVITE,
            format!(
                "Invited {}",
                tl_participant_peer_name(session, &a.participant)
            ),
            None,
            None,
        ),
        A::ParticipantToggleBan(a) => {
            let target = tl_participant_peer_name(session, &a.new_participant);
            let is_banned = matches!(a.new_participant, tl_enums::ChannelParticipant::Banned(_));
            (
                if is_banned {
                    AdminLogFilterFlags::BAN
                } else {
                    AdminLogFilterFlags::UNBAN
                },
                if is_banned {
                    format!("Restricted/banned {target}")
                } else {
                    format!("Unbanned {target}")
                },
                None,
                None,
            )
        }
        A::ParticipantToggleAdmin(a) => {
            let target = tl_participant_peer_name(session, &a.new_participant);
            let promoted = matches!(
                a.new_participant,
                tl_enums::ChannelParticipant::Admin(_) | tl_enums::ChannelParticipant::Creator(_)
            );
            (
                if promoted {
                    AdminLogFilterFlags::PROMOTE
                } else {
                    AdminLogFilterFlags::DEMOTE
                },
                if promoted {
                    format!("Updated admin rights for {target}")
                } else {
                    format!("Removed admin rights from {target}")
                },
                None,
                None,
            )
        }
        A::ToggleSlowMode(a) => (
            AdminLogFilterFlags::SETTINGS,
            "Changed Slow Mode interval".to_owned(),
            Some(format!("{}s", a.prev_value)),
            Some(format!("{}s", a.new_value)),
        ),
        A::ChangeHistoryTTL(a) => (
            AdminLogFilterFlags::SETTINGS,
            "Changed auto-delete timer".to_owned(),
            Some(format!("{}s", a.prev_value)),
            Some(format!("{}s", a.new_value)),
        ),
        A::TogglePreHistoryHidden(a) => (
            AdminLogFilterFlags::SETTINGS,
            if a.new_value {
                "Hid chat history for new members".to_owned()
            } else {
                "Made chat history visible for new members".to_owned()
            },
            None,
            None,
        ),
        A::ToggleNoForwards(a) => (
            AdminLogFilterFlags::SETTINGS,
            if a.new_value {
                "Enabled content protection (no forwards)".to_owned()
            } else {
                "Disabled content protection".to_owned()
            },
            None,
            None,
        ),
        A::DefaultBannedRights(_) => (
            AdminLogFilterFlags::SETTINGS,
            "Updated default member permissions".to_owned(),
            None,
            None,
        ),
        A::StopPoll(a) => (
            AdminLogFilterFlags::EDIT,
            "Stopped a poll".to_owned(),
            tl_message_text_preview(session, &a.message),
            None,
        ),
        A::ChangeLinkedChat(a) => (
            AdminLogFilterFlags::SETTINGS,
            "Changed linked discussion chat".to_owned(),
            (a.prev_value != 0).then(|| a.prev_value.to_string()),
            (a.new_value != 0).then(|| a.new_value.to_string()),
        ),
        A::StartGroupCall(_) => (
            AdminLogFilterFlags::GROUP_CALL,
            "Started a video chat".to_owned(),
            None,
            None,
        ),
        A::DiscardGroupCall(_) => (
            AdminLogFilterFlags::GROUP_CALL,
            "Ended a video chat".to_owned(),
            None,
            None,
        ),
        A::ParticipantMute(_) => (
            AdminLogFilterFlags::GROUP_CALL,
            "Muted a video chat participant".to_owned(),
            None,
            None,
        ),
        A::ParticipantUnmute(_) => (
            AdminLogFilterFlags::GROUP_CALL,
            "Unmuted a video chat participant".to_owned(),
            None,
            None,
        ),
        A::ToggleGroupCallSetting(a) => (
            AdminLogFilterFlags::GROUP_CALL,
            if a.join_muted {
                "Set video chat participants to join muted".to_owned()
            } else {
                "Allowed video chat participants to speak on join".to_owned()
            },
            None,
            None,
        ),
        A::ParticipantVolume(_) => (
            AdminLogFilterFlags::GROUP_CALL,
            "Adjusted a video chat participant's volume".to_owned(),
            None,
            None,
        ),
        A::ParticipantJoinByInvite(a) => (
            AdminLogFilterFlags::JOIN,
            "Joined via invite link".to_owned(),
            None,
            Some(tl_invite_link(&a.invite)),
        ),
        A::ParticipantJoinByRequest(a) => (
            AdminLogFilterFlags::JOIN,
            format!(
                "Joined via request (approved by {})",
                peer_display_title(
                    session,
                    DataPeerId::from_user(DataUserId::from_tl(a.approved_by))
                )
            ),
            None,
            Some(tl_invite_link(&a.invite)),
        ),
        A::ExportedInviteDelete(a) => (
            AdminLogFilterFlags::INVITES,
            "Deleted invite link".to_owned(),
            Some(tl_invite_link(&a.invite)),
            None,
        ),
        A::ExportedInviteRevoke(a) => (
            AdminLogFilterFlags::INVITES,
            "Revoked invite link".to_owned(),
            Some(tl_invite_link(&a.invite)),
            None,
        ),
        A::ExportedInviteEdit(a) => (
            AdminLogFilterFlags::INVITES,
            "Edited invite link".to_owned(),
            Some(tl_invite_link(&a.prev_invite)),
            Some(tl_invite_link(&a.new_invite)),
        ),
        A::ToggleForum(a) => (
            AdminLogFilterFlags::TOPICS,
            if a.new_value {
                "Enabled forum topics".to_owned()
            } else {
                "Disabled forum topics".to_owned()
            },
            None,
            None,
        ),
        A::CreateTopic(a) => (
            AdminLogFilterFlags::TOPICS,
            format!("Created forum topic \"{}\"", tl_topic_title(&a.topic)),
            None,
            None,
        ),
        A::EditTopic(a) => (
            AdminLogFilterFlags::TOPICS,
            format!("Edited forum topic \"{}\"", tl_topic_title(&a.new_topic)),
            Some(tl_topic_title(&a.prev_topic)),
            Some(tl_topic_title(&a.new_topic)),
        ),
        A::DeleteTopic(a) => (
            AdminLogFilterFlags::TOPICS,
            format!("Deleted forum topic \"{}\"", tl_topic_title(&a.topic)),
            Some(tl_topic_title(&a.topic)),
            None,
        ),
        A::PinTopic(a) => (
            AdminLogFilterFlags::TOPICS,
            match (&a.prev_topic, &a.new_topic) {
                (_, Some(nt)) => format!("Pinned forum topic \"{}\"", tl_topic_title(nt)),
                (Some(pt), None) => format!("Unpinned forum topic \"{}\"", tl_topic_title(pt)),
                (None, None) => "Updated pinned forum topic".to_owned(),
            },
            a.prev_topic.as_ref().map(tl_topic_title),
            a.new_topic.as_ref().map(tl_topic_title),
        ),
        A::ParticipantSubExtend(a) => (
            AdminLogFilterFlags::SUB_EXTEND,
            format!(
                "Extended subscription for {}",
                tl_participant_peer_name(session, &a.new_participant)
            ),
            None,
            None,
        ),
        A::ParticipantEditRank(a) => (
            AdminLogFilterFlags::EDIT_RANK,
            format!(
                "Edited rank for {}",
                peer_display_title(
                    session,
                    DataPeerId::from_user(DataUserId::from_tl(a.user_id))
                )
            ),
            (!a.prev_rank.is_empty()).then(|| a.prev_rank.clone()),
            (!a.new_rank.is_empty()).then(|| a.new_rank.clone()),
        ),
        A::SendMessage(a) => (
            AdminLogFilterFlags::INFO,
            "Sent a message".to_owned(),
            None,
            tl_message_text_preview(session, &a.message),
        ),
        A::ChangeStickerSet(_)
        | A::ChangeEmojiStickerSet(_)
        | A::ChangeLocation(_)
        | A::ChangeAvailableReactions(_)
        | A::ToggleAntiSpam(_)
        | A::ChangePeerColor(_)
        | A::ChangeProfilePeerColor(_)
        | A::ChangeWallpaper(_)
        | A::ChangeEmojiStatus(_)
        | A::ToggleAutotranslation(_) => (
            AdminLogFilterFlags::SETTINGS,
            "Updated group/channel settings".to_owned(),
            None,
            None,
        ),
    };

    AdminLogEntry {
        id: ev.id as u64,
        date_unix: ev.date,
        actor_id,
        actor_name,
        flag,
        summary,
        before_text,
        after_text,
    }
}
