//! Async-to-egui bridge connecting `tg_session::SessionRuntime` change/event streams
//! to synchronous `egui` repaint requests and `NotifyBackend` event queues, plus a
//! Tokio-dispatched `RpcClient` adapter so `IntroWidget` can safely drive `MtprotoRpc`.
//!
//! Port origin: `Telegram/SourceFiles/main/main_session.cpp` and
//! `Telegram/SourceFiles/window/notifications_manager.cpp` (event wiring).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::{
    ChangeEvent, HistoryUpdateFlags, MessageUpdateFlags, MsgId as DataMsgId, PeerId as DataPeerId,
    PeerUpdateFlags, Session, TopicUpdateFlags,
};
use tg_session::{SessionEvent, SessionRuntime};
use tg_tl::{Deserializable, RemoteCall, Serializable, enums as tl_enums};
use tokio::runtime::Handle;
use tokio::sync::{RwLock, broadcast};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::convert::{from_data_msg, from_data_peer, message_to_incoming_notification};
use tg_app_core::notifications::backend::IncomingNotifyEvent;
use tg_app_core::stand_in::types::{ConnectionStatus, MsgId};

/// Wrapper allowing any borrowed [`RemoteCall`] to be serialized once and executed on a
/// different Tokio runtime [`Handle`] (bridging `IntroWidget`'s time-only runtime onto the
/// multi-threaded network Tokio runtime with I/O enabled).
struct SerializedRawCall {
    bytes: Vec<u8>,
}

impl Serializable for SerializedRawCall {
    fn serialize(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&self.bytes);
    }
}

impl RemoteCall for SerializedRawCall {
    type Return = tg_tl::RawObject;
}

/// [`RpcClient`] adapter that dispatches every `invoke` / `invoke_in_dc` onto a dedicated
/// multi-thread Tokio [`Handle`] (with full I/O + timer drivers).
///
/// Required when plugging [`tg_session::MtprotoRpc`] into [`crate::intro::IntroWidget::with_rpc_client`],
/// because `IntroWidget` blocks on an internal `current_thread` runtime without an I/O reactor.
#[derive(Clone)]
pub struct RuntimeBridgedRpc<C: RpcClient + Clone + 'static> {
    inner: C,
    handle: Handle,
}

impl<C: RpcClient + Clone + 'static> RuntimeBridgedRpc<C> {
    /// Creates a new bridged RPC client running all calls on `handle`.
    #[must_use]
    pub fn new(inner: C, handle: Handle) -> Self {
        Self { inner, handle }
    }

    /// Borrows the inner RPC client.
    #[must_use]
    pub fn inner(&self) -> &C {
        &self.inner
    }
}

impl<C: RpcClient + Clone + 'static> RpcClient for RuntimeBridgedRpc<C> {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let call = SerializedRawCall {
            bytes: req.to_bytes(),
        };
        let rpc = self.inner.clone();
        let raw = self
            .handle
            .spawn(async move { rpc.invoke(&call).await.map_err(Into::into) })
            .await
            .map_err(|_| InvocationError::Dropped)??;
        R::Return::from_bytes(&raw.0).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let call = SerializedRawCall {
            bytes: req.to_bytes(),
        };
        let rpc = self.inner.clone();
        let raw = self
            .handle
            .spawn(async move { rpc.invoke_in_dc(dc_id, &call).await.map_err(Into::into) })
            .await
            .map_err(|_| InvocationError::Dropped)??;
        R::Return::from_bytes(&raw.0).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    fn main_dc_id(&self) -> DcId {
        self.inner.main_dc_id()
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        self.inner.set_main_dc_id(dc_id);
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        let _guard = self.handle.enter();
        self.inner.subscribe_updates()
    }
}

/// Maximum individual message IDs tracked per peer before collapsing into a slice reload.
const MAX_HISTORY_DELTAS_PER_PEER: usize = 256;

/// Maximum distinct peers retained in `history_deltas` before collapsing into a global reload flag.
const MAX_TRACKED_HISTORY_DELTA_PEERS: usize = 128;

/// Most notification events queued for the UI; beyond it the oldest message notifications are
/// dropped first (read and delete events keep stale popups from staying on screen).
pub const MAX_QUEUED_NOTIFY_EVENTS: usize = 1024;

/// Most session events queued for the UI. Critical events are never dropped.
pub const MAX_QUEUED_SESSION_EVENTS: usize = 256;

/// Most story updates queued for the UI (they wait while the stories lists load).
pub const MAX_QUEUED_STORY_UPDATES: usize = 2048;

/// Events the UI must see even when the queue overflows: sign-out, sync state, security alerts
/// and full-reload requests.
fn is_critical_session_event(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::Ready
            | SessionEvent::SyncError(_)
            | SessionEvent::ForceLogout { .. }
            | SessionEvent::NewAuthorization(_)
            | SessionEvent::DifferenceTooLong { .. }
            | SessionEvent::ChannelTooLong { .. }
            | SessionEvent::LangPackTooLong { .. }
    )
}

/// Whether a queued `old` event is made redundant by the newer `new` one.
fn session_event_supersedes(new: &SessionEvent, old: &SessionEvent) -> bool {
    use SessionEvent as E;
    match (new, old) {
        (E::SyncError(_), E::SyncError(_))
        | (E::DifferenceTooLong { .. }, E::DifferenceTooLong { .. })
        | (E::QuickReplies(_), E::QuickReplies(_)) => true,
        (E::ChannelTooLong { channel_id: a, .. }, E::ChannelTooLong { channel_id: b, .. }) => {
            a == b
        }
        (E::NotifySettingsUpdated { peer: a, .. }, E::NotifySettingsUpdated { peer: b, .. }) => {
            a == b
        }
        (
            E::StickerSetsOrder {
                masks: m1,
                emojis: e1,
                ..
            },
            E::StickerSetsOrder {
                masks: m2,
                emojis: e2,
                ..
            },
        ) => m1 == m2 && e1 == e2,
        _ => new == old,
    }
}

/// Queues `event`, replacing an older event it supersedes. On overflow the oldest non-critical
/// events are dropped and one [`SessionEvent::Ready`] is kept queued: every consumer treats it
/// as "reload cached state" (`refresh_cached_views`). Returns `true` if events were dropped.
fn push_session_event(queue: &mut VecDeque<SessionEvent>, event: SessionEvent) -> bool {
    if let Some(pos) = queue
        .iter()
        .position(|old| session_event_supersedes(&event, old))
    {
        queue.remove(pos);
    }
    queue.push_back(event);
    let mut dropped = false;
    while queue.len() > MAX_QUEUED_SESSION_EVENTS {
        let victim = queue
            .iter()
            .position(|e| !is_critical_session_event(e))
            .or_else(|| {
                // Only critical events left (distinct new-authorization alerts, per-channel
                // reloads): keep sign-out and the reload request, drop the oldest of the rest.
                queue.iter().position(|e| {
                    !matches!(e, SessionEvent::ForceLogout { .. } | SessionEvent::Ready)
                })
            });
        let Some(pos) = victim else {
            break;
        };
        queue.remove(pos);
        dropped = true;
    }
    if dropped && !queue.iter().any(|e| matches!(e, SessionEvent::Ready)) {
        queue.push_back(SessionEvent::Ready);
    }
    dropped
}

/// Queues a notification event, coalescing read events per chat and topic. On overflow the
/// oldest message notifications are dropped first. Returns `true` if events were dropped.
fn push_notify_event(
    queue: &mut VecDeque<IncomingNotifyEvent>,
    event: IncomingNotifyEvent,
) -> bool {
    match &event {
        IncomingNotifyEvent::HistoryRead {
            session_id,
            peer_id,
            ..
        } => queue.retain(|old| {
            !matches!(old, IncomingNotifyEvent::HistoryRead { session_id: s, peer_id: p, .. }
                if s == session_id && p == peer_id)
        }),
        IncomingNotifyEvent::TopicRead {
            session_id,
            peer_id,
            topic_root_id,
            ..
        } => queue.retain(|old| {
            !matches!(old, IncomingNotifyEvent::TopicRead {
                session_id: s, peer_id: p, topic_root_id: t, ..
            } if s == session_id && p == peer_id && t == topic_root_id)
        }),
        _ => {}
    }
    queue.push_back(event);
    let mut dropped = false;
    while queue.len() > MAX_QUEUED_NOTIFY_EVENTS {
        let pos = queue
            .iter()
            .position(|e| matches!(e, IncomingNotifyEvent::Notification(_)))
            .unwrap_or(0);
        queue.remove(pos);
        dropped = true;
    }
    dropped
}

/// Queues a story update, coalescing read marks per peer and stealth-mode changes. On overflow
/// the oldest updates are dropped. Returns `true` if updates were dropped.
fn push_story_update(queue: &mut VecDeque<tl_enums::Update>, update: tl_enums::Update) -> bool {
    match &update {
        tl_enums::Update::ReadStories(new) => queue.retain(|old| {
            !matches!(old, tl_enums::Update::ReadStories(o)
                if o.peer == new.peer && o.max_id <= new.max_id)
        }),
        tl_enums::Update::StoriesStealthMode(_) => {
            queue.retain(|old| !matches!(old, tl_enums::Update::StoriesStealthMode(_)));
        }
        _ => {}
    }
    queue.push_back(update);
    let mut dropped = false;
    while queue.len() > MAX_QUEUED_STORY_UPDATES {
        queue.pop_front();
        dropped = true;
    }
    dropped
}

/// Coalesced per-peer history deltas captured from `tg_data::ChangeEvent`s for an open chat view.
#[derive(Debug, Default, Clone)]
pub struct PeerHistoryDelta {
    /// Newly added message IDs in arrival order.
    pub added: Vec<DataMsgId>,
    /// Edited or metadata-updated message IDs (text, reactions, views, pinned flag, etc.).
    pub edited: Vec<DataMsgId>,
    /// Deleted/destroyed message IDs.
    pub deleted: Vec<DataMsgId>,
    /// True if `HistoryUpdateFlags::OUTBOX_READ` fired for this peer.
    pub outbox_read_changed: bool,
    /// True if `HistoryUpdateFlags::UNREAD_VIEW` fired for this peer.
    pub unread_changed: bool,
    /// True if the open history should reload its slice (e.g. broadcast lag or delta overflow).
    pub reload_required: bool,
}

impl PeerHistoryDelta {
    fn maybe_collapse_if_oversized(&mut self) {
        if self.added.len() + self.edited.len() + self.deleted.len() > MAX_HISTORY_DELTAS_PER_PEER {
            self.added.clear();
            self.edited.clear();
            self.deleted.clear();
            self.reload_required = true;
        }
    }
}

/// Most chats with their own version; beyond it the oldest entries fold into the shared epoch.
const MAX_VERSIONED_CHATS: usize = 4096;

/// Version counters for the UI's cached views.
///
/// One sequence counts every change. A change tied to one chat records the new value for that
/// chat; any other change moves the shared epoch. A chat's version is the larger of the two, the
/// chat list's version is the sequence itself, so both only grow and change on every relevant
/// change.
#[derive(Debug, Default)]
pub(crate) struct ViewVersions {
    seq: AtomicU64,
    epoch: AtomicU64,
    chats: Mutex<HashMap<DataPeerId, u64>>,
}

impl ViewVersions {
    fn next(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::AcqRel).wrapping_add(1)
    }

    /// A change any view may show.
    pub(crate) fn bump_all(&self) {
        let seq = self.next();
        self.epoch.fetch_max(seq, Ordering::AcqRel);
    }

    /// A change only `peer`'s chat view and the chat list show.
    pub(crate) fn bump_chat(&self, peer: DataPeerId) {
        let seq = self.next();
        let Ok(mut chats) = self.chats.lock() else {
            self.epoch.fetch_max(seq, Ordering::AcqRel);
            return;
        };
        if chats.len() >= MAX_VERSIONED_CHATS && !chats.contains_key(&peer) {
            // Every forgotten chat falls back to the epoch, which must not be older than any
            // version handed out for it.
            chats.clear();
            self.epoch.fetch_max(seq, Ordering::AcqRel);
            return;
        }
        let slot = chats.entry(peer).or_insert(0);
        *slot = (*slot).max(seq);
    }

    pub(crate) fn dialogs(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }

    pub(crate) fn chat(&self, peer: DataPeerId) -> u64 {
        let own = self
            .chats
            .lock()
            .ok()
            .and_then(|chats| chats.get(&peer).copied())
            .unwrap_or(0);
        own.max(self.epoch.load(Ordering::Acquire))
    }
}

/// The one chat a change event is about, or `None` when other chats' views may show it too
/// (peer names and statuses appear in every chat that mentions the peer, folders in the list).
fn change_event_chat(event: &ChangeEvent) -> Option<DataPeerId> {
    match event {
        ChangeEvent::MessageUpdated { id, .. } => Some(id.peer),
        ChangeEvent::HistoryUpdated { peer_id, .. }
        | ChangeEvent::TopicUpdated { peer_id, .. }
        | ChangeEvent::ChatAdminChanged { peer_id, .. }
        | ChangeEvent::UserTyping { peer_id, .. } => Some(*peer_id),
        ChangeEvent::EntryUpdated { key, .. } => match key {
            tg_data::DialogKey::History(peer)
            | tg_data::DialogKey::ForumTopic { peer, .. }
            | tg_data::DialogKey::SavedSublist { parent: peer, .. } => Some(*peer),
            tg_data::DialogKey::Folder(_) => None,
        },
        ChangeEvent::PeerUpdated { .. }
        | ChangeEvent::NameUpdated { .. }
        | ChangeEvent::FiltersChanged
        | ChangeEvent::NotifyDefaultsChanged => None,
    }
}

/// Shared state updated asynchronously by [`LiveRuntimeBridge`] and polled synchronously
/// from the `egui` UI thread.
#[derive(Debug)]
pub struct LiveBridgeState {
    /// Optional `egui::Context` woken whenever backend state mutates.
    pub egui_ctx: Mutex<Option<egui::Context>>,
    /// A repaint was requested since the UI last started a frame ([`Self::set_egui_ctx`]); more
    /// changes before that frame need no request of their own.
    repaint_pending: AtomicBool,
    /// Repaint requests actually sent to `egui` (diagnostics and tests).
    repaint_requests: AtomicU64,
    /// Queued notification events translated from incoming `ChangeEvent`s.
    pub notify_events: Mutex<VecDeque<IncomingNotifyEvent>>,
    /// Queued high-level `SessionEvent`s (e.g. `Ready`, `SyncError`, `ForceLogout`).
    pub session_events: Mutex<VecDeque<SessionEvent>>,
    /// Queued story updates delivered via [`SessionEvent::StoryUpdate`] (from both live updates
    /// and `updates.getDifference` / `updates.getChannelDifference`, matching
    /// `Updates::applyUpdate` in `api/api_updates.cpp:2947-2958`).
    pub story_updates: Mutex<VecDeque<tl_enums::Update>>,
    /// Per-peer coalesced history change deltas drained by an open `HistoryWidget`.
    pub history_deltas: Mutex<HashMap<DataPeerId, PeerHistoryDelta>>,
    /// Set when the change stream lagged or tracked too many unopened peers; forces next open
    /// history drain to emit `ReloadRequired`.
    pub history_deltas_lagged: AtomicBool,
    /// Current connection indicator state.
    pub connection_status: Mutex<ConnectionStatus>,
    /// Set once a live MTProto transport reports its own state into
    /// [`Self::connection_status`] ([`Self::follow_transport_connection_state`]).
    connection_from_transport: AtomicBool,
    /// Last sync or RPC error banner message, if any.
    pub last_error: Mutex<Option<String>>,
    /// Monotonically increasing change counter bumped on every change event.
    pub generation: AtomicU64,
    /// Bumped whenever a notification setting of this account may have changed (peer, topic or
    /// per-type default, or a lagged change stream); see [`crate::notify_sync`].
    notify_settings_generation: AtomicU64,
    /// Peers whose own notification settings a notification waits for; see
    /// [`crate::notify_sync`].
    pub(crate) notify_peer_requests: Mutex<crate::notify_sync::PeerSettingsRequests>,
    /// View versions behind `DialogsBackend::dialogs_version` / `HistoryBackend::chat_version`.
    pub(crate) versions: ViewVersions,
    /// Set when story updates were dropped because the queue was full; the stories lists should
    /// be reloaded from the server ([`Self::take_story_updates_overflowed`]).
    pub story_updates_overflowed: AtomicBool,
}

impl Default for LiveBridgeState {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveBridgeState {
    /// Creates a new bridge state starting in `ConnectionStatus::Connecting`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            egui_ctx: Mutex::new(None),
            repaint_pending: AtomicBool::new(false),
            repaint_requests: AtomicU64::new(0),
            notify_events: Mutex::new(VecDeque::new()),
            session_events: Mutex::new(VecDeque::new()),
            story_updates: Mutex::new(VecDeque::new()),
            history_deltas: Mutex::new(HashMap::new()),
            history_deltas_lagged: AtomicBool::new(false),
            connection_status: Mutex::new(ConnectionStatus::Connecting),
            connection_from_transport: AtomicBool::new(false),
            last_error: Mutex::new(None),
            generation: AtomicU64::new(0),
            notify_settings_generation: AtomicU64::new(0),
            notify_peer_requests: Mutex::default(),
            versions: ViewVersions::default(),
            story_updates_overflowed: AtomicBool::new(false),
        }
    }

    /// Queues a notification event (bounded, see [`MAX_QUEUED_NOTIFY_EVENTS`]).
    pub fn push_notify_event(&self, event: IncomingNotifyEvent) {
        if let Ok(mut guard) = self.notify_events.lock()
            && push_notify_event(&mut guard, event)
        {
            warn!("notification queue full; dropped the oldest notifications");
        }
    }

    /// Queues a session event (bounded, see [`MAX_QUEUED_SESSION_EVENTS`]). Critical events are
    /// never dropped; an overflow queues [`SessionEvent::Ready`] so the UI reloads its state.
    pub fn push_session_event(&self, event: SessionEvent) {
        if let Ok(mut guard) = self.session_events.lock()
            && push_session_event(&mut guard, event)
        {
            warn!("session event queue full; requesting a state reload");
            drop(guard);
            self.mark_history_deltas_lagged();
        }
    }

    /// Queues a story update (bounded, see [`MAX_QUEUED_STORY_UPDATES`]).
    pub fn push_story_update(&self, update: tl_enums::Update) {
        if let Ok(mut guard) = self.story_updates.lock()
            && push_story_update(&mut guard, update)
        {
            warn!("story update queue full; dropped the oldest story updates");
            self.story_updates_overflowed.store(true, Ordering::Release);
        }
    }

    /// Returns (and clears) whether story updates were dropped since the last call.
    pub fn take_story_updates_overflowed(&self) -> bool {
        self.story_updates_overflowed.swap(false, Ordering::AcqRel)
    }

    fn with_peer_delta(&self, peer_id: DataPeerId, f: impl FnOnce(&mut PeerHistoryDelta)) {
        let Ok(mut guard) = self.history_deltas.lock() else {
            return;
        };
        if !guard.contains_key(&peer_id) && guard.len() >= MAX_TRACKED_HISTORY_DELTA_PEERS {
            guard.clear();
            self.history_deltas_lagged.store(true, Ordering::Release);
            return;
        }
        let delta = guard.entry(peer_id).or_default();
        if !delta.reload_required {
            f(delta);
            delta.maybe_collapse_if_oversized();
        }
    }

    /// Records a newly added message for `peer_id`.
    pub fn record_message_added(&self, peer_id: DataPeerId, msg_id: DataMsgId) {
        self.with_peer_delta(peer_id, |delta| {
            delta.deleted.retain(|&id| id != msg_id);
            if !delta.added.contains(&msg_id) {
                delta.added.push(msg_id);
            }
        });
    }

    /// Records an edited or updated message for `peer_id`.
    pub fn record_message_edited(&self, peer_id: DataPeerId, msg_id: DataMsgId) {
        self.with_peer_delta(peer_id, |delta| {
            if !delta.added.contains(&msg_id) && !delta.edited.contains(&msg_id) {
                delta.edited.push(msg_id);
            }
        });
    }

    /// Records a deleted/destroyed message for `peer_id`.
    pub fn record_message_deleted(&self, peer_id: DataPeerId, msg_id: DataMsgId) {
        self.with_peer_delta(peer_id, |delta| {
            delta.added.retain(|&id| id != msg_id);
            delta.edited.retain(|&id| id != msg_id);
            if !delta.deleted.contains(&msg_id) {
                delta.deleted.push(msg_id);
            }
        });
    }

    /// Records that outgoing messages were read by the recipient in `peer_id`.
    pub fn record_outbox_read(&self, peer_id: DataPeerId) {
        self.with_peer_delta(peer_id, |delta| {
            delta.outbox_read_changed = true;
        });
    }

    /// Records that inbox read watermark or unread count changed for `peer_id`.
    pub fn record_unread_changed(&self, peer_id: DataPeerId) {
        self.with_peer_delta(peer_id, |delta| {
            delta.unread_changed = true;
        });
    }

    /// Marks the change stream as lagged so the next open history drain reloads its slice.
    pub fn mark_history_deltas_lagged(&self) {
        if let Ok(mut guard) = self.history_deltas.lock() {
            guard.clear();
        }
        self.history_deltas_lagged.store(true, Ordering::Release);
    }

    /// Clears pending deltas for `peer_id` (called right before loading a fresh full slice).
    pub fn clear_history_delta(&self, peer_id: DataPeerId) {
        if let Ok(mut guard) = self.history_deltas.lock() {
            guard.remove(&peer_id);
        }
    }

    /// Drains any pending history delta for `peer_id`.
    pub fn take_history_delta(&self, peer_id: DataPeerId) -> Option<PeerHistoryDelta> {
        let lagged = self.history_deltas_lagged.swap(false, Ordering::AcqRel);
        let mut delta = self
            .history_deltas
            .lock()
            .ok()
            .and_then(|mut guard| guard.remove(&peer_id));
        if lagged {
            let entry = delta.get_or_insert_with(PeerHistoryDelta::default);
            entry.reload_required = true;
        }
        delta
    }

    /// Registers the active `egui::Context` so background updates trigger immediate repaints.
    ///
    /// The live host calls it at the start of every frame, before it reads any state: from here
    /// on a change needs a new repaint request, while changes made before are visible to this
    /// frame's reads (the data is written before the change is announced).
    pub fn set_egui_ctx(&self, ctx: egui::Context) {
        self.repaint_pending.store(false, Ordering::Release);
        if let Ok(mut guard) = self.egui_ctx.lock() {
            *guard = Some(ctx);
        }
    }

    /// Requests one repaint per frame however many changes arrive before it starts.
    fn wake_ui(&self) {
        if self.repaint_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Ok(guard) = self.egui_ctx.lock()
            && let Some(ctx) = guard.as_ref()
        {
            self.repaint_requests.fetch_add(1, Ordering::Relaxed);
            ctx.request_repaint();
        }
    }

    /// Repaint requests sent to `egui` so far.
    #[must_use]
    pub fn repaint_requests(&self) -> u64 {
        self.repaint_requests.load(Ordering::Relaxed)
    }

    /// Bumps the change generation and requests an immediate `egui` repaint if registered.
    /// Every view version changes too: the change may touch any chat.
    pub fn notify_ui(&self) {
        self.versions.bump_all();
        self.generation.fetch_add(1, Ordering::Release);
        self.wake_ui();
    }

    /// Current monotonically increasing change generation counter.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Like [`Self::notify_ui`] for a change that only `peer`'s chat view (and the chat list)
    /// can show: other chats keep their version.
    pub fn notify_chat_changed(&self, peer: DataPeerId) {
        self.versions.bump_chat(peer);
        self.generation.fetch_add(1, Ordering::Release);
        self.wake_ui();
    }

    /// Changes every view version without waking the UI (a change the caller shows itself, made
    /// on the UI thread).
    pub fn mark_views_changed(&self) {
        self.versions.bump_all();
    }

    /// Version of the chat list's data (`DialogsBackend::dialogs_version`).
    #[must_use]
    pub fn dialogs_version(&self) -> u64 {
        self.versions.dialogs()
    }

    /// Version of `peer`'s chat view data (`HistoryBackend::chat_version`).
    #[must_use]
    pub fn chat_version(&self, peer: DataPeerId) -> u64 {
        self.versions.chat(peer)
    }

    /// Marks the account's notification settings as changed.
    pub fn bump_notify_settings(&self) {
        self.notify_settings_generation
            .fetch_add(1, Ordering::AcqRel);
    }

    /// Counter behind [`Self::bump_notify_settings`].
    #[must_use]
    pub fn notify_settings_generation(&self) -> u64 {
        self.notify_settings_generation.load(Ordering::Acquire)
    }

    /// Drains all queued [`IncomingNotifyEvent`]s.
    pub fn drain_notify_events(&self) -> Vec<IncomingNotifyEvent> {
        let Ok(mut guard) = self.notify_events.lock() else {
            return Vec::new();
        };
        guard.drain(..).collect()
    }

    /// Drains all queued [`SessionEvent`]s.
    pub fn drain_session_events(&self) -> Vec<SessionEvent> {
        let Ok(mut guard) = self.session_events.lock() else {
            return Vec::new();
        };
        guard.drain(..).collect()
    }

    /// Drains all queued story updates received via [`SessionEvent::StoryUpdate`].
    pub fn drain_story_updates(&self) -> Vec<tl_enums::Update> {
        let Ok(mut guard) = self.story_updates.lock() else {
            return Vec::new();
        };
        guard.drain(..).collect()
    }

    /// Reads the current connection status.
    #[must_use]
    pub fn connection_status(&self) -> ConnectionStatus {
        self.connection_status
            .lock()
            .map_or(ConnectionStatus::Connected, |g| *g)
    }

    /// Updates connection status and wakes the UI.
    pub fn set_connection_status(&self, status: ConnectionStatus) {
        if let Ok(mut guard) = self.connection_status.lock() {
            *guard = status;
        }
        self.notify_ui();
    }

    /// From now on the connection indicator follows the MTProto transport's own state, as
    /// tdesktop's `ConnectionState` follows `MTP::Instance::state`. [`SessionEvent::Ready`] then
    /// no longer marks the connection as up: it says the session finished its startup, and when
    /// it arrives late (a busy machine) it would hide a reconnect already under way, e.g. right
    /// after a proxy switch (TGB-3077).
    pub fn follow_transport_connection_state(&self) {
        self.connection_from_transport
            .store(true, Ordering::Release);
    }

    /// [`SessionEvent::Ready`]: the connection is up unless the transport reports it itself.
    fn on_session_ready(&self) {
        if !self.connection_from_transport.load(Ordering::Acquire) {
            self.set_connection_status(ConnectionStatus::Connected);
        }
    }

    /// Reads the latest error message, if any.
    #[must_use]
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|g| g.clone())
    }

    /// Sets or clears the last error banner message.
    pub fn set_last_error(&self, err: Option<String>) {
        if let Ok(mut guard) = self.last_error.lock() {
            *guard = err;
        }
        self.notify_ui();
    }
}

/// Background task handle forwarding `SessionRuntime`'s `ChangeEvent` and `SessionEvent`
/// streams into [`LiveBridgeState`] and waking `egui::Context`.
pub struct LiveRuntimeBridge {
    state: Arc<LiveBridgeState>,
    changes_task: JoinHandle<()>,
    events_task: JoinHandle<()>,
}

impl LiveRuntimeBridge {
    /// Spawns background bridge tasks on `runtime.tokio_handle()` for `session_id`.
    #[must_use]
    pub fn spawn<C: RpcClient + Clone + 'static>(
        runtime: &SessionRuntime<C>,
        session_id: u64,
    ) -> Self {
        let state = Arc::new(LiveBridgeState::new());
        if runtime.is_ready() {
            state.set_connection_status(ConnectionStatus::Connected);
        }

        let session_arc = Arc::clone(runtime.session());
        let mut changes_rx = runtime.subscribe_changes();
        let state_changes = Arc::clone(&state);
        let changes_task = runtime.tokio_handle().spawn(async move {
            loop {
                match changes_rx.recv().await {
                    Ok(ev) => {
                        let chat = change_event_chat(&ev);
                        handle_change_event(&session_arc, &state_changes, session_id, ev).await;
                        match chat {
                            Some(peer) => state_changes.notify_chat_changed(peer),
                            None => state_changes.notify_ui(),
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "LiveRuntimeBridge ChangeEvent receiver lagged");
                        state_changes.mark_history_deltas_lagged();
                        state_changes.bump_notify_settings();
                        state_changes.notify_ui();
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        let mut events_rx = runtime.subscribe_events();
        let state_events = Arc::clone(&state);
        let events_task = runtime.tokio_handle().spawn(async move {
            loop {
                match events_rx.recv().await {
                    Ok(ev) => {
                        match &ev {
                            SessionEvent::Ready => {
                                state_events.on_session_ready();
                                state_events.set_last_error(None);
                            }
                            SessionEvent::SyncError(msg) => {
                                state_events.set_last_error(Some(msg.clone()));
                            }
                            SessionEvent::ForceLogout { reason } => {
                                warn!(?reason, "account logged out by the server");
                            }
                            SessionEvent::StoryUpdate(_) => {
                                if let SessionEvent::StoryUpdate(update) = ev {
                                    state_events.push_story_update(*update);
                                    state_events.notify_ui();
                                }
                                continue;
                            }
                            _ => {}
                        }
                        state_events.push_session_event(ev);
                        state_events.notify_ui();
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        debug!(skipped, "LiveRuntimeBridge SessionEvent receiver lagged");
                        state_events.notify_ui();
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        Self {
            state,
            changes_task,
            events_task,
        }
    }

    /// Returns the shared [`LiveBridgeState`].
    #[must_use]
    pub fn state(&self) -> &Arc<LiveBridgeState> {
        &self.state
    }
}

impl Drop for LiveRuntimeBridge {
    fn drop(&mut self) {
        self.changes_task.abort();
        self.events_task.abort();
    }
}

async fn handle_change_event(
    session_lock: &Arc<RwLock<Session>>,
    state: &LiveBridgeState,
    session_id: u64,
    event: ChangeEvent,
) {
    match event {
        // Bumped before any later message of the same stream is queued, so the notification
        // system re-syncs its mute view before it evaluates that message (TGB-2969).
        ChangeEvent::PeerUpdated { flags, .. }
            if flags.contains(PeerUpdateFlags::NOTIFICATIONS) =>
        {
            state.bump_notify_settings();
        }
        ChangeEvent::TopicUpdated { flags, .. }
            if flags.contains(TopicUpdateFlags::NOTIFICATIONS) =>
        {
            state.bump_notify_settings();
        }
        ChangeEvent::NotifyDefaultsChanged => state.bump_notify_settings(),
        ChangeEvent::MessageUpdated { id, flags } => {
            if flags.intersects(MessageUpdateFlags::NEW_ADDED | MessageUpdateFlags::EDITED)
                && !flags.contains(MessageUpdateFlags::DESTROYED)
            {
                log_saved_messages_side(session_lock, id, flags).await;
            }
            if flags.contains(MessageUpdateFlags::DESTROYED) {
                state.record_message_deleted(id.peer, id.msg);
                state.push_notify_event(IncomingNotifyEvent::MessageDeleted {
                    session_id,
                    peer_id: from_data_peer(id.peer),
                    msg_id: from_data_msg(id.msg),
                });
            } else if flags.contains(MessageUpdateFlags::NEW_ADDED) {
                state.record_message_added(id.peer, id.msg);
                // Only messages that arrived as live updates (`NewMessageType::Unread`) and are
                // still unread may notify (`History::newItemAdded`, `history/history.cpp:1688-1699`);
                // history slices, dialogs pages and the startup catch-up never do.
                let maybe_notif = if flags.contains(MessageUpdateFlags::NEW_UNREAD) {
                    let session = session_lock.read().await;
                    session
                        .message(id)
                        .filter(|msg| show_notification(&session, msg))
                        .map(|msg| {
                            crate::notify_sync::want_mention_author_settings(&session, msg, state);
                            message_to_incoming_notification(&session, msg, session_id)
                        })
                } else {
                    None
                };
                if let Some(notif) = maybe_notif {
                    state.push_notify_event(IncomingNotifyEvent::Notification(Box::new(notif)));
                }
            } else if flags.intersects(
                MessageUpdateFlags::EDITED
                    | MessageUpdateFlags::REPLY_MARKUP
                    | MessageUpdateFlags::NEW_UNREAD_REACTION,
            ) {
                state.record_message_edited(id.peer, id.msg);
            }
        }
        ChangeEvent::HistoryUpdated { peer_id, flags } => {
            if flags.contains(HistoryUpdateFlags::OUTBOX_READ) {
                state.record_outbox_read(peer_id);
            }
            if flags.contains(HistoryUpdateFlags::UNREAD_VIEW) {
                state.record_unread_changed(peer_id);
                let read_till = {
                    let session = session_lock.read().await;
                    session.history(peer_id).and_then(|h| {
                        let till = h.inbox_read_till_id();
                        if till.0 > 0 {
                            Some(from_data_msg(till))
                        } else if h.unread_count == Some(0) && !h.unread_mark {
                            Some(MsgId::SHOW_AT_END)
                        } else {
                            None
                        }
                    })
                };
                if let Some(read_till_id) = read_till {
                    state.push_notify_event(IncomingNotifyEvent::HistoryRead {
                        session_id,
                        peer_id: from_data_peer(peer_id),
                        read_till_id,
                    });
                }
            }
        }
        _ => {}
    }
}

/// TGB-2908 diagnostics: logs (INFO, target `saved_side`, reaches `log.txt`) which side the
/// Saved Messages layout picks for a self-chat message each time it is added or replaced
/// (`Message::hasOutLayout`). Never logs text or names.
async fn log_saved_messages_side(
    session_lock: &Arc<RwLock<Session>>,
    id: tg_data::FullMsgId,
    flags: MessageUpdateFlags,
) {
    let session = session_lock.read().await;
    let self_peer = session.self_peer_id();
    if id.peer != self_peer {
        return;
    }
    let Some(msg) = session.message(id) else {
        return;
    };
    let side = if crate::preview_text::has_saved_messages_out_layout(&session, msg) {
        "out"
    } else {
        "in"
    };
    tracing::info!(
        target: tg_api::saved_side_log::TARGET,
        stage = "layout",
        msg_id = id.msg.0,
        added = flags.contains(MessageUpdateFlags::NEW_ADDED),
        pending = msg.flags.contains(tg_data::MessageFlags::BEING_SENT),
        out = msg.is_outgoing(),
        from = tg_api::saved_side_log::from_kind(msg.from_id, self_peer),
        fwd = msg.forward_from.is_some(),
        side,
        "saved messages: layout side"
    );
}

/// Whether a newly added `msg` should raise a desktop notification
/// (`HistoryItem::showNotification` + `HistoryItem::unread`, `history/history_item.cpp:4641-4671,4900-4908`):
/// only incoming, server-side unread messages of chats the account is still in.
pub(crate) fn show_notification(session: &Session, msg: &tg_data::Message) -> bool {
    let peer_id = msg.peer_id;
    if msg.is_outgoing() || peer_id == session.self_peer_id() || !msg.is_regular() {
        return false;
    }
    if let Some(peer) = session.peer(peer_id) {
        if peer.as_channel().is_some_and(|c| !c.am_in()) {
            return false;
        }
        if peer.as_chat().is_some_and(|c| c.migrated_to.is_some()) {
            return false;
        }
    }
    session
        .history(peer_id)
        .is_some_and(|h| h.is_server_side_unread(msg))
}

#[cfg(test)]
#[path = "bridge_sec3_tests.rs"]
mod sec3_tests;

#[cfg(test)]
#[path = "bridge_notify_tests.rs"]
mod notify_tests;

#[cfg(test)]
#[path = "bridge_versions_tests.rs"]
mod versions_tests;
