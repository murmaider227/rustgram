//! Async-to-egui bridge connecting `tg_session::SessionRuntime` change/event streams
//! to synchronous `egui` repaint requests and `NotifyBackend` event queues, plus a
//! Tokio-dispatched `RpcClient` adapter so `IntroWidget` can safely drive `MtprotoRpc`.
//!
//! Port origin: `Telegram/SourceFiles/main/main_session.cpp` and
//! `Telegram/SourceFiles/window/notifications_manager.cpp` (event wiring).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::{ChangeEvent, MessageUpdateFlags, Session};
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

/// Shared state updated asynchronously by [`LiveRuntimeBridge`] and polled synchronously
/// from the `egui` UI thread.
#[derive(Debug)]
pub struct LiveBridgeState {
    /// Optional `egui::Context` woken whenever backend state mutates.
    pub egui_ctx: Mutex<Option<egui::Context>>,
    /// Queued notification events translated from incoming `ChangeEvent`s.
    pub notify_events: Mutex<VecDeque<IncomingNotifyEvent>>,
    /// Queued high-level `SessionEvent`s (e.g. `Ready`, `SyncError`, `ForceLogout`).
    pub session_events: Mutex<VecDeque<SessionEvent>>,
    /// Queued story updates delivered via [`SessionEvent::StoryUpdate`] (from both live updates
    /// and `updates.getDifference` / `updates.getChannelDifference`, matching
    /// `Updates::applyUpdate` in `api/api_updates.cpp:2947-2958`).
    pub story_updates: Mutex<VecDeque<tl_enums::Update>>,
    /// Current connection indicator state.
    pub connection_status: Mutex<ConnectionStatus>,
    /// Last sync or RPC error banner message, if any.
    pub last_error: Mutex<Option<String>>,
    /// Monotonically increasing change counter bumped on every change event.
    pub generation: AtomicU64,
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
            notify_events: Mutex::new(VecDeque::new()),
            session_events: Mutex::new(VecDeque::new()),
            story_updates: Mutex::new(VecDeque::new()),
            connection_status: Mutex::new(ConnectionStatus::Connecting),
            last_error: Mutex::new(None),
            generation: AtomicU64::new(0),
        }
    }

    /// Registers the active `egui::Context` so background updates trigger immediate repaints.
    pub fn set_egui_ctx(&self, ctx: egui::Context) {
        if let Ok(mut guard) = self.egui_ctx.lock() {
            *guard = Some(ctx);
        }
    }

    /// Bumps the change generation and requests an immediate `egui` repaint if registered.
    pub fn notify_ui(&self) {
        self.generation.fetch_add(1, Ordering::Release);
        if let Ok(guard) = self.egui_ctx.lock()
            && let Some(ctx) = guard.as_ref()
        {
            ctx.request_repaint();
        }
    }

    /// Current monotonically increasing change generation counter.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
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
                        handle_change_event(&session_arc, &state_changes, session_id, ev).await;
                        state_changes.notify_ui();
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "LiveRuntimeBridge ChangeEvent receiver lagged");
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
                                state_events.set_connection_status(ConnectionStatus::Connected);
                                state_events.set_last_error(None);
                            }
                            SessionEvent::SyncError(msg) => {
                                state_events.set_last_error(Some(msg.clone()));
                            }
                            SessionEvent::ForceLogout { notification_type } => {
                                warn!(notification_type, "server requested forced logout");
                            }
                            SessionEvent::StoryUpdate(_) => {
                                if let SessionEvent::StoryUpdate(update) = ev {
                                    if let Ok(mut guard) = state_events.story_updates.lock() {
                                        guard.push_back(*update);
                                    }
                                    state_events.notify_ui();
                                }
                                continue;
                            }
                            _ => {}
                        }
                        if let Ok(mut guard) = state_events.session_events.lock() {
                            guard.push_back(ev);
                        }
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
        ChangeEvent::MessageUpdated { id, flags } => {
            if flags.contains(MessageUpdateFlags::NEW_ADDED) {
                let maybe_notif = {
                    let session = session_lock.read().await;
                    session.message(id).and_then(|msg| {
                        // Only emit desktop notifications for incoming regular/service messages
                        // once initial sync is ready (or any incoming non-outgoing message).
                        if msg.is_outgoing() {
                            return None;
                        }
                        Some(message_to_incoming_notification(&session, msg, session_id))
                    })
                };
                if let Some(notif) = maybe_notif
                    && let Ok(mut guard) = state.notify_events.lock()
                {
                    guard.push_back(IncomingNotifyEvent::Notification(Box::new(notif)));
                }
            } else if flags.contains(MessageUpdateFlags::DESTROYED)
                && let Ok(mut guard) = state.notify_events.lock()
            {
                guard.push_back(IncomingNotifyEvent::MessageDeleted {
                    session_id,
                    peer_id: from_data_peer(id.peer),
                    msg_id: from_data_msg(id.msg),
                });
            }
        }
        ChangeEvent::HistoryUpdated { peer_id, flags }
            if flags.contains(tg_data::HistoryUpdateFlags::UNREAD_VIEW) =>
        {
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
            if let Some(read_till_id) = read_till
                && let Ok(mut guard) = state.notify_events.lock()
            {
                guard.push_back(IncomingNotifyEvent::HistoryRead {
                    session_id,
                    peer_id: from_data_peer(peer_id),
                    read_till_id,
                });
            }
        }
        _ => {}
    }
}
