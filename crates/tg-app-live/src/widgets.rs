//! Live Telegram glue for sandboxed HTML widget messages (`tg-tgw`).
//!
//! Port origin: `Telegram/SourceFiles/tgw/tgw_bridge.cpp`, `tgw_state.cpp`,
//! `tgw_media.cpp`, `tgw_runtime.cpp`, and `tgw_wallet.cpp`.
//!
//! Wires `LiveSession` to the Layer 233 widget TL RPCs:
//! - `messages.getBotWidgetQueryAnswer` (Data / SignData / SentTransaction)
//! - `messages.getWidgetPublicData` (`WidgetPublicData` & `WidgetPublicDataNotModified`)
//! - `messages.editMessageWidgetData`
//! - `messages.editWidgetData`
//! - `updateMessageWidgetData` state synchronization across shared `widget_id` carriers
//! - [`LiveWidgetHost`] combining [`tg_tgw::WidgetStateMachine`], [`tg_tgw::SandboxPolicy`],
//!   and [`tg_tgw::WebViewWidgetHost`].

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::convert::{input_peer_for, peer_id_from_tl};
use tg_api::{InvocationError, RpcClient};
use tg_data::{FullMsgId as DataFullMsgId, MsgId as DataMsgId, PeerId as DataPeerId, Session};
use tg_tgw::{
    Anchors, BotQueryTracker, DiskStorageConfig, DiskWriteOutcome, HeightRecord, MeterConfig,
    MeterSnapshot, MeterVerdict, PublicDataPollResult, PublicDataPoller, StorageArea, StorageMeta,
    WidgetFields, WidgetFlags, WidgetMeter, WidgetMsgKey, WidgetRequestFailure,
    WidgetRequestOutcome, WidgetState, WidgetStateEdit, WidgetStorageDisk, clamp_poll_period_ms,
    extract_html_text_preview, extract_html_title, map_rpc_error_to_outcome, unpack_payload,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tokio::sync::RwLock;

use crate::convert::{from_data_msg, from_data_peer, now_unix_i64, to_data_msg, to_data_peer};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::history::backend::{HistoryMedia, HistoryMessage, WidgetAttachment};
use tg_app_core::stand_in::types::{MsgId, PeerId};

/// RPC timeout matching `kRequestTimeout = 30000` (`tgw_bridge.cpp:41`).
const WIDGET_RPC_TIMEOUT: Duration = Duration::from_secs(30);

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

/// Cached per-message widget state entry: `(state, public_data_hash, widget_id)`.
pub type CachedWidgetState = (WidgetState, Option<i64>, Option<i64>);

/// Per-session live widget state registry (kept out of `LiveSession`'s struct so parallel
/// branches editing `live/session.rs` merge cleanly).
#[derive(Debug, Default)]
pub struct LiveWidgetsState {
    /// Shared public-data polling registry keyed by `widget_id`.
    pub poller: PublicDataPoller,
    /// Per-message cached widget state blobs (`chat_data`, `public_data`, `public_data_hash`).
    pub message_states: HashMap<(PeerId, MsgId), CachedWidgetState>,
    /// In-flight bot query deadline tracker.
    pub query_tracker: BotQueryTracker,
    /// Optional crash-safe disk-backed widget local storage (`tg_tgw::WidgetStorageDisk`).
    pub disk_storage: Option<WidgetStorageDisk>,
    /// Per-message frame-time, CPU-budget, and memory watchdogs (`tg_tgw::WidgetMeter`).
    pub meters: HashMap<(PeerId, MsgId), WidgetMeter>,
    /// Default [`MeterConfig`] applied to newly created per-widget meters.
    pub default_meter_config: MeterConfig,
    /// Monotonic query correlation ID.
    next_query_id: u64,
}

impl LiveWidgetsState {
    fn alloc_query_id(&mut self) -> u64 {
        self.next_query_id = self.next_query_id.saturating_add(1);
        self.next_query_id
    }

    /// Returns a mutable reference to the [`WidgetMeter`] for `(peer_id, msg_id)`, creating one
    /// with `default_meter_config` if absent.
    pub fn meter_for_mut(&mut self, peer_id: PeerId, msg_id: MsgId) -> &mut WidgetMeter {
        let cfg = self.default_meter_config;
        self.meters
            .entry((peer_id, msg_id))
            .or_insert_with(|| WidgetMeter::with_config(cfg))
    }

    /// Evaluates the active [`MeterVerdict`] for `(peer_id, msg_id)` at `now_secs` (`Allow` if no
    /// meter has recorded activity yet).
    #[must_use]
    pub fn meter_verdict(&self, peer_id: PeerId, msg_id: MsgId, now_secs: f64) -> MeterVerdict {
        self.meters
            .get(&(peer_id, msg_id))
            .map_or(MeterVerdict::Allow, |m| m.verdict(now_secs))
    }
}

/// Converts UI `(PeerId, MsgId)` into a `tg-tgw` [`WidgetMsgKey`].
#[must_use]
pub const fn widget_msg_key(peer_id: PeerId, msg_id: MsgId) -> WidgetMsgKey {
    WidgetMsgKey {
        peer_id: peer_id.0,
        msg_id: msg_id.0,
    }
}

/// Opens and attaches a crash-safe [`WidgetStorageDisk`] store to the session's [`LiveWidgetsState`].
pub fn attach_widget_disk_storage(
    session: &Arc<RwLock<Session>>,
    dir: impl AsRef<Path>,
    config: DiskStorageConfig,
) -> Result<(), String> {
    let store = WidgetStorageDisk::open(dir, config).map_err(|e| e.to_string())?;
    let state_arc = widgets_state_for_session(session);
    let mut st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    st.disk_storage = Some(store);
    Ok(())
}

/// Reads persisted per-widget local storage from the session's [`WidgetStorageDisk`].
pub fn widget_storage_read_live(
    session: &Arc<RwLock<Session>>,
    peer_id: PeerId,
    msg_id: MsgId,
) -> Result<Option<StorageArea>, String> {
    let state_arc = widgets_state_for_session(session);
    let mut st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    let Some(disk) = st.disk_storage.as_mut() else {
        return Ok(None);
    };
    disk.read(widget_msg_key(peer_id, msg_id))
        .map_err(|e| e.to_string())
}

/// Atomically writes per-widget local storage to the session's [`WidgetStorageDisk`], enforcing
/// per-widget and global byte quotas with LRU eviction.
pub fn widget_storage_write_live(
    session: &Arc<RwLock<Session>>,
    peer_id: PeerId,
    msg_id: MsgId,
    meta: StorageMeta,
    local: &StorageArea,
) -> Result<DiskWriteOutcome, String> {
    let state_arc = widgets_state_for_session(session);
    let mut st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    let Some(disk) = st.disk_storage.as_mut() else {
        return Err("Widget disk storage is not attached to this session.".to_owned());
    };
    disk.write(widget_msg_key(peer_id, msg_id), meta, local)
        .map_err(|e| e.to_string())
}

/// Reads the persisted [`HeightRecord`] for `(peer_id, msg_id)` from [`WidgetStorageDisk`].
#[must_use]
pub fn widget_storage_height_live(
    session: &Arc<RwLock<Session>>,
    peer_id: PeerId,
    msg_id: MsgId,
) -> Option<HeightRecord> {
    let state_arc = widgets_state_for_session(session);
    let st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    st.disk_storage
        .as_ref()
        .and_then(|disk| disk.height(widget_msg_key(peer_id, msg_id)))
}

/// Caches a positive finite [`HeightRecord`] for `(peer_id, msg_id)` in [`WidgetStorageDisk`].
pub fn widget_storage_set_height_live(
    session: &Arc<RwLock<Session>>,
    peer_id: PeerId,
    msg_id: MsgId,
    meta: StorageMeta,
    height: HeightRecord,
) -> Result<bool, String> {
    let state_arc = widgets_state_for_session(session);
    let mut st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    let Some(disk) = st.disk_storage.as_mut() else {
        return Err("Widget disk storage is not attached to this session.".to_owned());
    };
    Ok(disk.set_height(widget_msg_key(peer_id, msg_id), meta, height))
}

/// Prunes expired persisted widget records from [`WidgetStorageDisk`].
pub fn widget_storage_prune_expired_live(
    session: &Arc<RwLock<Session>>,
    now_unix: i32,
) -> Result<usize, String> {
    let state_arc = widgets_state_for_session(session);
    let mut st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    let Some(disk) = st.disk_storage.as_mut() else {
        return Ok(0);
    };
    disk.prune_expired(now_unix).map_err(|e| e.to_string())
}

/// Removes all persisted widget records belonging to `peer_id` from [`WidgetStorageDisk`].
pub fn widget_storage_clear_chat_live(
    session: &Arc<RwLock<Session>>,
    peer_id: PeerId,
) -> Result<usize, String> {
    let state_arc = widgets_state_for_session(session);
    let mut st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    let Some(disk) = st.disk_storage.as_mut() else {
        return Ok(0);
    };
    disk.clear_chat(peer_id.0).map_err(|e| e.to_string())
}

/// Flushes dirty index metadata in [`WidgetStorageDisk`] to disk.
pub fn widget_storage_flush_live(session: &Arc<RwLock<Session>>) -> Result<(), String> {
    let state_arc = widgets_state_for_session(session);
    let mut st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    let Some(disk) = st.disk_storage.as_mut() else {
        return Ok(());
    };
    disk.flush().map_err(|e| e.to_string())
}

/// Configures a custom [`MeterConfig`] for a specific widget's [`WidgetMeter`].
pub fn widget_meter_configure_live(
    session: &Arc<RwLock<Session>>,
    peer_id: PeerId,
    msg_id: MsgId,
    config: MeterConfig,
) {
    let state_arc = widgets_state_for_session(session);
    let mut st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    st.meters
        .insert((peer_id, msg_id), WidgetMeter::with_config(config));
}

/// Records user input on `(peer_id, msg_id)` to grant the post-input throttle exemption window.
pub fn widget_meter_note_input_live(
    session: &Arc<RwLock<Session>>,
    peer_id: PeerId,
    msg_id: MsgId,
    now_secs: f64,
) {
    let state_arc = widgets_state_for_session(session);
    let mut st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    st.meter_for_mut(peer_id, msg_id).note_input(now_secs);
}

/// Records a widget frame duration in the session's [`WidgetMeter`] and returns its [`MeterVerdict`].
#[must_use]
pub fn widget_meter_record_frame_live(
    session: &Arc<RwLock<Session>>,
    peer_id: PeerId,
    msg_id: MsgId,
    now_secs: f64,
    frame_secs: f64,
) -> MeterVerdict {
    let state_arc = widgets_state_for_session(session);
    let mut st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    st.meter_for_mut(peer_id, msg_id)
        .record_frame(now_secs, frame_secs)
}

/// Updates memory usage in the session's [`WidgetMeter`] and returns its [`MeterVerdict`].
#[must_use]
pub fn widget_meter_update_memory_live(
    session: &Arc<RwLock<Session>>,
    peer_id: PeerId,
    msg_id: MsgId,
    bytes: usize,
    now_secs: f64,
) -> MeterVerdict {
    let state_arc = widgets_state_for_session(session);
    let mut st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    st.meter_for_mut(peer_id, msg_id)
        .update_memory(bytes, now_secs)
}

/// Evaluates the current [`MeterVerdict`] for `(peer_id, msg_id)` at `now_secs`.
#[must_use]
pub fn widget_meter_verdict_live(
    session: &Arc<RwLock<Session>>,
    peer_id: PeerId,
    msg_id: MsgId,
    now_secs: f64,
) -> MeterVerdict {
    let state_arc = widgets_state_for_session(session);
    let st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    st.meter_verdict(peer_id, msg_id, now_secs)
}

/// Returns a full diagnostic [`MeterSnapshot`] for `(peer_id, msg_id)` at `now_secs`.
#[must_use]
pub fn widget_meter_snapshot_live(
    session: &Arc<RwLock<Session>>,
    peer_id: PeerId,
    msg_id: MsgId,
    now_secs: f64,
) -> MeterSnapshot {
    let state_arc = widgets_state_for_session(session);
    let mut st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    st.meter_for_mut(peer_id, msg_id).snapshot(now_secs)
}

/// Clears any latched suspension state on `(peer_id, msg_id)`'s [`WidgetMeter`].
pub fn widget_meter_reset_live(session: &Arc<RwLock<Session>>, peer_id: PeerId, msg_id: MsgId) {
    let state_arc = widgets_state_for_session(session);
    let mut st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(meter) = st.meters.get_mut(&(peer_id, msg_id)) {
        meter.reset_suspension();
    }
}

type WidgetsStateRegistry = Mutex<Vec<(Weak<RwLock<Session>>, Arc<Mutex<LiveWidgetsState>>)>>;

fn widgets_state_registry() -> &'static WidgetsStateRegistry {
    static REGISTRY: OnceLock<WidgetsStateRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Returns the per-session [`LiveWidgetsState`] handle.
#[must_use]
pub fn widgets_state_for_session(session: &Arc<RwLock<Session>>) -> Arc<Mutex<LiveWidgetsState>> {
    let key = Arc::downgrade(session);
    let mut guard = widgets_state_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    guard.retain(|(weak, _)| weak.strong_count() > 0);
    if let Some((_, state)) = guard.iter().find(|(weak, _)| Weak::ptr_eq(weak, &key)) {
        return Arc::clone(state);
    }
    let state = Arc::new(Mutex::new(LiveWidgetsState::new_default()));
    guard.push((key, Arc::clone(&state)));
    state
}

impl LiveWidgetsState {
    fn new_default() -> Self {
        Self::default()
    }
}

/// Converts a raw Layer-233 TL [`tl_types::MessageMediaWidget`] into [`tg_tgw::WidgetFields`],
/// preserving every flag and data blob (`ParseWidgetFields` in `tgw_media.cpp:101-124`).
#[must_use]
pub fn tl_message_media_widget_to_fields(w: &tl_types::MessageMediaWidget) -> WidgetFields {
    WidgetFields {
        anchors: Anchors {
            width: w.width,
            height: w.height,
        },
        payload: w.payload.clone(),
        state: WidgetState {
            chat_data: w.chat_data.as_ref().map(|s| s.as_bytes().to_vec()),
            public_data: w.public_data.as_ref().map(|s| s.as_bytes().to_vec()),
        },
        public_data_hash: w.public_data_hash,
        widget_id: w.widget_id,
        flags: WidgetFlags {
            is_copy: w.is_copy,
            can_query: w.can_query,
            background_query: w.background_query,
        },
    }
}

/// Converts a raw Layer-233 TL [`tl_types::MessageMediaWidget`] into a UI [`WidgetAttachment`],
/// unpacking the zlib/gzip payload and extracting title & text preview.
#[must_use]
pub fn tl_message_media_widget_to_attachment(w: &tl_types::MessageMediaWidget) -> WidgetAttachment {
    let html = unpack_payload(&w.payload)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok());
    let title = html
        .as_deref()
        .and_then(extract_html_title)
        .unwrap_or_else(|| "Interactive Widget".to_owned());
    let preview_text = html
        .as_deref()
        .map(|h| extract_html_text_preview(h, 180))
        .unwrap_or_default();
    WidgetAttachment {
        widget_id: w.widget_id,
        width: w.width,
        height: w.height,
        payload: w.payload.clone(),
        html,
        title,
        preview_text,
        is_copy: w.is_copy,
        can_query: w.can_query,
        background_query: w.background_query,
        public_data: w.public_data.clone(),
        public_data_hash: w.public_data_hash,
        chat_data: w.chat_data.clone(),
    }
}

/// Maps an [`InvocationError`] into a [`WidgetRequestOutcome`] matching `ErrorOutcome`
/// (`tgw_bridge.cpp:59-67`).
#[must_use]
pub fn invocation_error_to_widget_outcome(err: &InvocationError) -> WidgetRequestOutcome {
    match err {
        InvocationError::Rpc(rpc) => {
            let is_flood = rpc.code == 420 || rpc.name.starts_with("FLOOD_WAIT");
            map_rpc_error_to_outcome(&rpc.name, is_flood)
        }
        _ => WidgetRequestOutcome::err(WidgetRequestFailure::Failed),
    }
}

/// Invokes `messages.getBotWidgetQueryAnswer` with `widgetQueryTypeData` on `LiveSession`.
pub fn widget_query_live(
    live: &LiveSession,
    peer_id: PeerId,
    msg_id: MsgId,
    data: &[u8],
    with_wallet: bool,
) -> Result<WidgetRequestOutcome, String> {
    if msg_id.0 <= 0 {
        return Ok(WidgetRequestOutcome::err(WidgetRequestFailure::Failed));
    }
    let query_type = tl_enums::WidgetQueryType::Data(tl_types::WidgetQueryTypeData { with_wallet });
    widget_query_typed_live(live, peer_id, msg_id, query_type, data)
}

/// Invokes `messages.getBotWidgetQueryAnswer` with any [`tl_enums::WidgetQueryType`]
/// (`Data`, `SignData`, or `SentTransaction`).
pub fn widget_query_typed_live(
    live: &LiveSession,
    peer_id: PeerId,
    msg_id: MsgId,
    query_type: tl_enums::WidgetQueryType,
    data: &[u8],
) -> Result<WidgetRequestOutcome, String> {
    if msg_id.0 <= 0 {
        return Ok(WidgetRequestOutcome::err(WidgetRequestFailure::Failed));
    }
    let data_peer = to_data_peer(peer_id);
    let session_arc = Arc::clone(live.runtime().session());
    let input_peer = read_session(live, |s| input_peer_for(s, data_peer));
    if matches!(input_peer, tl_enums::InputPeer::Empty) {
        return Err("Unknown peer for widget query.".to_owned());
    }

    let state_arc = widgets_state_for_session(&session_arc);
    let now_secs = now_unix_i64().max(0) as f64;
    let now_ms = (now_unix_i64().max(0) as u64).saturating_mul(1_000);
    let req_id = {
        let mut st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
        match st.meter_verdict(peer_id, msg_id, now_secs) {
            MeterVerdict::Allow => {}
            MeterVerdict::Throttle { .. } => {
                return Ok(WidgetRequestOutcome::err(WidgetRequestFailure::RateLimited));
            }
            MeterVerdict::Suspend { .. } => {
                return Ok(WidgetRequestOutcome::err(WidgetRequestFailure::Failed));
            }
        }
        let id = st.alloc_query_id();
        let with_wallet = matches!(
            query_type,
            tl_enums::WidgetQueryType::Data(tl_types::WidgetQueryTypeData { with_wallet: true })
                | tl_enums::WidgetQueryType::SignData(_)
                | tl_enums::WidgetQueryType::SentTransaction(_)
        );
        st.query_tracker.register(id, with_wallet, now_ms);
        id
    };

    let req = tl_fns::messages::GetBotWidgetQueryAnswer {
        peer: input_peer,
        msg_id: msg_id.0,
        query_type,
        data: data.to_vec(),
    };
    let rpc = live.runtime().rpc().clone();
    let res = block_on_runtime(live.runtime().tokio_handle(), async move {
        tokio::time::timeout(WIDGET_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
    });

    {
        let mut st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
        st.query_tracker.complete(req_id);
    }

    match res {
        Ok(Ok(tl_enums::messages::BotWidgetQueryAnswer::BotWidgetQueryAnswer(ans))) => {
            Ok(WidgetRequestOutcome::ok(ans.result))
        }
        Ok(Err(ref inv_err)) => Ok(invocation_error_to_widget_outcome(inv_err)),
        // Local 30s deadline maps to `Failure::Failed` (`Failure::Timeout` is reserved exclusively
        // for server `WIDGET_RESPONSE_TIMEOUT` in `Telegram/SourceFiles/tgw/tgw_bridge.cpp:59-67,179-182`).
        Err(_elapsed) => Ok(WidgetRequestOutcome::err(WidgetRequestFailure::Failed)),
    }
}

/// Invokes `messages.getWidgetPublicData` and updates the per-session [`PublicDataPoller`]
/// and cached widget state for all messages sharing `widget_id` (`tgw_state.cpp:108-165`).
pub fn widget_poll_public_data_live(
    live: &LiveSession,
    peer_id: PeerId,
    msg_id: MsgId,
    hash: i64,
) -> Result<PublicDataPollResult, String> {
    if msg_id.0 <= 0 {
        return Err("Cannot poll public data for non-server message ID.".to_owned());
    }
    let data_peer = to_data_peer(peer_id);
    let data_msg = to_data_msg(msg_id);
    let session_arc = Arc::clone(live.runtime().session());
    let (input_peer, widget_id) = read_session(live, |s| {
        let ip = input_peer_for(s, data_peer);
        let wid = s
            .message(DataFullMsgId::new(data_peer, data_msg))
            .and_then(|m| match m.media() {
                Some(tg_data::MessageMedia::Widget(w)) => w.widget_id,
                _ => None,
            });
        (ip, wid)
    });
    if matches!(input_peer, tl_enums::InputPeer::Empty) {
        return Err("Unknown peer for widget public data poll.".to_owned());
    }

    let state_arc = widgets_state_for_session(&session_arc);
    {
        let now_secs = now_unix_i64().max(0) as f64;
        let st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
        match st.meter_verdict(peer_id, msg_id, now_secs) {
            MeterVerdict::Allow => {}
            MeterVerdict::Throttle { hold_ms, .. } => {
                let secs = hold_ms.div_ceil(1_000).clamp(1, i32::MAX as u64) as i32;
                return Ok(PublicDataPollResult::NotModified {
                    period_ms: clamp_poll_period_ms(secs),
                });
            }
            MeterVerdict::Suspend { detail, .. } => {
                return Err(format!("Widget suspended by watchdog: {detail}"));
            }
        }
    }

    let req = tl_fns::messages::GetWidgetPublicData {
        peer: input_peer,
        msg_id: msg_id.0,
        hash,
    };
    let rpc = live.runtime().rpc().clone();
    let resp = block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(WIDGET_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
    })
    .map_err(|e| format!("messages.getWidgetPublicData failed: {e}"))?;

    let now_ms = (now_unix_i64().max(0) as u64).saturating_mul(1_000);
    let result = match resp {
        tl_enums::messages::WidgetPublicData::NotModified(nm) => {
            PublicDataPollResult::NotModified {
                period_ms: clamp_poll_period_ms(nm.period),
            }
        }
        tl_enums::messages::WidgetPublicData::WidgetPublicData(d) => {
            PublicDataPollResult::Updated {
                hash: d.hash,
                data: d.data,
                period_ms: clamp_poll_period_ms(d.period),
            }
        }
    };

    let state_arc = widgets_state_for_session(&session_arc);
    let mut st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(wid) = widget_id {
        st.poller.apply_poll_result(wid, &result, now_ms);
    }
    if let PublicDataPollResult::Updated {
        hash: new_hash,
        ref data,
        ..
    } = result
    {
        let entry = st
            .message_states
            .entry((peer_id, msg_id))
            .or_insert_with(|| (WidgetState::default(), None, widget_id));
        entry.0.public_data = Some(data.as_bytes().to_vec());
        entry.1 = Some(new_hash);
        if let Some(wid) = widget_id {
            entry.2 = Some(wid);
            for ((_, _), (ws, h, w)) in &mut st.message_states {
                if *w == Some(wid) {
                    ws.public_data = Some(data.as_bytes().to_vec());
                    *h = Some(new_hash);
                }
            }
        }
    }

    Ok(result)
}

/// Invokes `messages.editMessageWidgetData` to update per-message `public_data` and/or `chat_data`.
pub fn widget_edit_message_data_live(
    live: &LiveSession,
    peer_id: PeerId,
    msg_id: MsgId,
    public_data: Option<&str>,
    chat_data: Option<&str>,
) -> Result<(), String> {
    if msg_id.0 <= 0 {
        return Err("Cannot edit widget data for non-server message ID.".to_owned());
    }
    let data_peer = to_data_peer(peer_id);
    let session_arc = Arc::clone(live.runtime().session());
    let input_peer = read_session(live, |s| input_peer_for(s, data_peer));
    if matches!(input_peer, tl_enums::InputPeer::Empty) {
        return Err("Unknown peer for widget edit.".to_owned());
    }

    let req = tl_fns::messages::EditMessageWidgetData {
        peer: input_peer,
        id: msg_id.0,
        public_data: public_data.map(ToOwned::to_owned),
        chat_data: chat_data.map(ToOwned::to_owned),
    };
    let rpc = live.runtime().rpc().clone();
    let updates = block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(WIDGET_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
    })
    .map_err(|e| format!("messages.editMessageWidgetData failed: {e}"))?;

    // Route returned `Updates` through `SessionRuntime`'s single `UpdatesHandler`
    // (which also calls `record_widget_updates_from_batch` via side-caches).
    let _ = live.apply_updates_to_session(&updates);

    let state_arc = widgets_state_for_session(&session_arc);
    let mut st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    let entry = st
        .message_states
        .entry((peer_id, msg_id))
        .or_insert_with(|| (WidgetState::default(), None, None));
    if let Some(pd) = public_data {
        entry.0.public_data = Some(pd.as_bytes().to_vec());
    }
    if let Some(cd) = chat_data {
        entry.0.chat_data = Some(cd.as_bytes().to_vec());
    }
    Ok(())
}

/// Invokes `messages.editWidgetData` to update a shared widget's `public_data` by `widget_id`.
pub fn widget_edit_shared_data_live(
    live: &LiveSession,
    widget_id: i64,
    public_data: &str,
    cache_time: Option<i32>,
) -> Result<bool, String> {
    let req = tl_fns::messages::EditWidgetData {
        widget_id,
        public_data: public_data.to_owned(),
        cache_time,
    };
    let rpc = live.runtime().rpc().clone();
    let ok = block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(WIDGET_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
    })
    .map_err(|e| format!("messages.editWidgetData failed: {e}"))?;
    if ok {
        let session_arc = Arc::clone(live.runtime().session());
        let state_arc = widgets_state_for_session(&session_arc);
        let mut st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
        for (ws, _, wid) in st.message_states.values_mut() {
            if *wid == Some(widget_id) {
                ws.public_data = Some(public_data.as_bytes().to_vec());
            }
        }
    }
    Ok(ok)
}

/// Applies an incoming `updateMessageWidgetData` to the per-session widget state cache
/// (`Tgw::ApplyStateUpdate` in `tgw_state.cpp:258-281`).
pub fn apply_widget_data_update(
    session: &Arc<RwLock<Session>>,
    upd: &tl_types::UpdateMessageWidgetData,
) -> bool {
    let data_peer: DataPeerId = peer_id_from_tl(&upd.peer);
    let peer_id = from_data_peer(data_peer);
    let msg_id = from_data_msg(DataMsgId(i64::from(upd.msg_id)));

    let edit = WidgetStateEdit {
        chat_data: upd.chat_data.as_ref().map(|s| s.as_bytes().to_vec()),
        public_data: upd.public_data.as_ref().map(|s| s.as_bytes().to_vec()),
        public_data_hash: upd.public_data_hash,
    };

    let state_arc = widgets_state_for_session(session);
    let mut st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    let entry = st
        .message_states
        .entry((peer_id, msg_id))
        .or_insert_with(|| (WidgetState::default(), None, None));

    let mut changed = false;
    if let Some(cd) = edit.chat_data
        && entry.0.chat_data.as_deref() != Some(cd.as_slice())
    {
        entry.0.chat_data = Some(cd);
        changed = true;
    }
    if let Some(pd) = edit.public_data
        && (entry.0.public_data.as_deref() != Some(pd.as_slice())
            || entry.1 != edit.public_data_hash)
    {
        entry.0.public_data = Some(pd);
        entry.1 = edit.public_data_hash;
        changed = true;
    }
    changed
}

/// Scans an incoming TL [`tl_enums::Updates`] batch and applies any `Update::MessageWidgetData`
/// or `MessageMedia::Widget` metadata (`public_data`, `public_data_hash`, `chat_data`) into the
/// per-session [`LiveWidgetsState`] cache.
pub fn record_widget_updates_from_batch(
    session: &Arc<RwLock<Session>>,
    updates: &tl_enums::Updates,
) -> bool {
    let mut changed = false;
    let mut handle = |upd: &tl_enums::Update| match upd {
        tl_enums::Update::MessageWidgetData(w) => {
            if apply_widget_data_update(session, w) {
                changed = true;
            }
        }
        tl_enums::Update::NewMessage(u) => {
            changed |= record_widget_from_tl_message(session, &u.message);
        }
        tl_enums::Update::NewChannelMessage(u) => {
            changed |= record_widget_from_tl_message(session, &u.message);
        }
        tl_enums::Update::EditMessage(u) => {
            changed |= record_widget_from_tl_message(session, &u.message);
        }
        tl_enums::Update::EditChannelMessage(u) => {
            changed |= record_widget_from_tl_message(session, &u.message);
        }
        _ => {}
    };
    match updates {
        tl_enums::Updates::UpdateShort(s) => handle(&s.update),
        tl_enums::Updates::Updates(u) => {
            for upd in &u.updates {
                handle(upd);
            }
        }
        tl_enums::Updates::Combined(c) => {
            for upd in &c.updates {
                handle(upd);
            }
        }
        _ => {}
    }
    changed
}

fn record_widget_from_tl_message(
    session: &Arc<RwLock<Session>>,
    tl_msg: &tl_enums::Message,
) -> bool {
    let tl_enums::Message::Message(m) = tl_msg else {
        return false;
    };
    let Some(tl_enums::MessageMedia::Widget(w)) = &m.media else {
        return false;
    };
    // Incoming 1:1 DMs carry `peer_id = PeerUser(self)` and `from_id = PeerUser(sender)`;
    // resolve the chat peer identically to `tg_api::convert` / `live/rich_message.rs:167-187`.
    let self_uid = session
        .try_read()
        .map(|s| s.self_user_id().0 as i64)
        .unwrap_or(0);
    let peer_id = match &m.peer_id {
        tl_enums::Peer::User(u)
            if !m.out
                && self_uid != 0
                && u.user_id == self_uid
                && let Some(tl_enums::Peer::User(fu)) = m.from_id.as_ref() =>
        {
            PeerId::from_user(fu.user_id as u64)
        }
        _ => from_data_peer(peer_id_from_tl(&m.peer_id)),
    };
    let msg_id = from_data_msg(DataMsgId(i64::from(m.id)));
    let state_arc = widgets_state_for_session(session);
    let mut st = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    let entry = st
        .message_states
        .entry((peer_id, msg_id))
        .or_insert_with(|| (WidgetState::default(), None, w.widget_id));
    entry.2 = w.widget_id;
    let mut changed = false;
    if let Some(ref pd) = w.public_data {
        let bytes = pd.as_bytes();
        if entry.0.public_data.as_deref() != Some(bytes) || entry.1 != w.public_data_hash {
            entry.0.public_data = Some(bytes.to_vec());
            entry.1 = w.public_data_hash;
            changed = true;
        }
    }
    if let Some(ref cd) = w.chat_data {
        let bytes = cd.as_bytes();
        if entry.0.chat_data.as_deref() != Some(bytes) {
            entry.0.chat_data = Some(bytes.to_vec());
            changed = true;
        }
    }
    changed
}

/// Overlays cached per-message / shared-`widget_id` widget state (`public_data`,
/// `public_data_hash`, `chat_data`) onto any [`HistoryMedia::Widget`] items in `messages`.
pub fn attach_cached_widget_states(
    session: &Arc<RwLock<Session>>,
    messages: &mut [HistoryMessage],
) {
    let state_arc = widgets_state_for_session(session);
    let Ok(st) = state_arc.lock() else {
        return;
    };
    for msg in messages {
        let Some(HistoryMedia::Widget(ref mut att)) = msg.media else {
            continue;
        };
        if let Some((ws, hash, _)) = st.message_states.get(&(msg.peer_id, msg.id)) {
            if let Some(ref pd) = ws.public_data
                && let Ok(text) = std::str::from_utf8(pd)
            {
                att.public_data = Some(text.to_owned());
            }
            if hash.is_some() {
                att.public_data_hash = *hash;
            }
            if let Some(ref cd) = ws.chat_data
                && let Ok(text) = std::str::from_utf8(cd)
            {
                att.chat_data = Some(text.to_owned());
            }
        }
        if let Some(wid) = att.widget_id
            && let Some((hash, data)) = st.poller.latest_public_data(wid)
        {
            att.public_data = Some(data.to_owned());
            if hash.is_some() {
                att.public_data_hash = hash;
            }
        }
    }
}
