//! Port of `mtproto/session.cpp` and `mtproto/session_private.cpp`.
//!
//! Splits cleanly into:
//! - [`SessionState`]: a deterministic, synchronous MTProto 2.0 session state machine
//!   (containers, `msg_id`/`seq_no`, salts, acks, resend, `bad_msg_notification`,
//!   `bad_server_salt`, `new_session_created`, `msgs_state_req`/`info`, `msg_resend_req`,
//!   `gzip_packed`, `invokeAfterMsg`, `initConnection`/`invokeWithLayer`, PFS temp-key binding).
//! - [`SessionHandle`]: an async `tokio` session actor driving a [`PacketConnection`] with
//!   DH auth key creation, keepalive `ping_delay_disconnect`, exponential-backoff reconnect,
//!   state sync after reconnect, request cancellation, and update forwarding.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use rand::RngCore;
use tg_crypto::{AuthKey, Side};
use tg_tl::mtproto::{
    CoreMessage, GzipPacked, MsgContainer, RpcResult, enums as mtp_enums,
    functions as mtp_functions, types as mtp_types,
};
use tg_tl::{
    Boxed, Deserializable, Identifiable, LAYER, RawObject, Serializable, enums as tl_enums,
};
use tg_transport::{Error as TransportError, ProxyData, TransportErrorCode};
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{debug, trace, warn};

use crate::auth_key::{
    CreatingKeyType, DcKeyBindState, DcKeyRequest, Dcenter, KEY_OLD_ENOUGH_FOR_DESTROY,
    ManagedAuthKey, TEMPORARY_EXPIRES_IN_SECS, TemporaryKeyType, create_dc_keys,
    handle_bind_temp_key_response, prepare_bind_temp_key_request,
};
use crate::config::ClientConfig;
use crate::dc_options::{
    DcId, DcOptions, DcType, ShiftedDcId, bare_dc_id, get_dc_id_shift, protocol_dc_id,
};
use crate::error::{InvocationError, RpcError};
use crate::message::{
    CUT_CONTAINER_ON_BYTES, DecryptedEnvelope, MAX_GZIP_NESTING, MAX_UNPACKED_MESSAGE_LENGTH,
    RequestId, body_needs_ack, build_encrypted_packet, gzip_pack_if_smaller, pack_container,
    parse_encrypted_packet, unpack_container, unwrap_gzip_layers, wrap_init_connection,
    wrap_invoke_after_msg,
};
use crate::received_ids::{IDS_BUFFER_SIZE, ReceivedIdState, ReceivedIdsManager, RegisterIdResult};
use crate::salts::SaltManager;
use crate::time::{MtprotoClock, is_bad_server_time, is_valid_server_msg_id};
use crate::transport_adapter::{PacketConnection, TransportFactory};

/// How long to wait before sending batched `msgs_ack` (`kAckSendWaiting = 10s`).
pub const ACK_SEND_WAITING: Duration = Duration::from_secs(10);
/// How long an unacknowledged sent request waits before `msgs_state_req` (`kCheckSentRequestTimeout = 10s`).
pub const CHECK_SENT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Lifetime of tracked sent containers before resending unacked inner messages (`kSentContainerLives = 600s`).
pub const SENT_CONTAINER_LIVES: Duration = Duration::from_secs(600);
/// Threshold for fast-request unixtime calibration (`kFastRequestDuration = 500ms`).
pub const FAST_REQUEST_DURATION: Duration = Duration::from_millis(500);
/// Maximum reconnect backoff delay (`64000ms` from `SessionPrivate::retryByTimer`).
pub const MAX_RETRY_BACKOFF_MS: u64 = 64_000;
/// Number of future salts requested in `get_future_salts`.
pub const DEFAULT_FUTURE_SALTS_COUNT: i32 = 32;
/// `http_wait.max_delay` in milliseconds (`MTP_http_wait(100, 30, 25000)` in `session_private.cpp`).
pub const HTTP_WAIT_MAX_DELAY_MS: i32 = 100;
/// `http_wait.wait_after` in milliseconds (`session_private.cpp`).
pub const HTTP_WAIT_WAIT_AFTER_MS: i32 = 30;
/// `http_wait.max_wait` in milliseconds (`session_private.cpp`).
pub const HTTP_WAIT_MAX_WAIT_MS: i32 = 25_000;

/// Server-pushed update (or `new_session_created` notification) forwarded to callers.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdatesMsg {
    /// Bare DC ID that produced this update.
    pub dc_id: DcId,
    /// Outer MTProto `msg_id` of the server message.
    pub outer_msg_id: u64,
    /// Parsed `tg_tl::enums::Updates` (boxed to keep channel messages compact), when the
    /// payload decodes as a Telegram `Updates` object (`None` for non-`Updates` notifications
    /// such as `new_session_created`).
    pub updates: Option<Box<tl_enums::Updates>>,
    /// Raw serialized TL body of the update object.
    pub body: Vec<u8>,
}

impl UpdatesMsg {
    /// Returns a borrowed reference to the parsed [`tg_tl::enums::Updates`], if present.
    pub fn updates(&self) -> Option<&tl_enums::Updates> {
        self.updates.as_deref()
    }
}

/// Request waiting to be packed and sent on a session.
#[derive(Debug, Clone)]
pub struct QueuedRequest {
    /// Client-assigned request identifier.
    pub request_id: RequestId,
    /// Serialized TL request body (unwrapped).
    pub body: Vec<u8>,
    /// Assigned `msg_id` (populated when first sent, preserved/replaced on resend).
    pub msg_id: Option<u64>,
    /// Assigned `seq_no`.
    pub seq_no: Option<u32>,
    /// Whether this request expects an ack (content-related).
    pub need_ack: bool,
    /// Whether this request should be wrapped in `invokeWithLayer(initConnection(...))` if the DC
    /// is not yet initialized.
    pub needs_layer: bool,
    /// Optional predecessor request ID to enforce ordering via `invokeAfterMsg`.
    pub after_request_id: Option<RequestId>,
    /// If `true`, force wrapping in a `msg_container` (used on resends as in tdesktop).
    pub force_send_in_container: bool,
    /// Timestamp when this request was last transmitted.
    pub last_sent_at: Option<Instant>,
    /// Whether the caller expects an RPC response (controls ACK-without-response retention).
    pub expects_response: bool,
}

impl QueuedRequest {
    /// Creates a standard RPC request with `needs_layer = true` and `expects_response = true`.
    pub fn new(request_id: RequestId, body: Vec<u8>) -> Self {
        let need_ack = body_needs_ack(&body);
        Self {
            request_id,
            body,
            msg_id: None,
            seq_no: None,
            need_ack,
            needs_layer: true,
            after_request_id: None,
            force_send_in_container: false,
            last_sent_at: None,
            expects_response: true,
        }
    }
}

#[derive(Debug, Clone)]
struct SentContainer {
    sent_at: Instant,
    messages: Vec<u64>,
}

/// Outbound unencrypted envelope ready to be encrypted and transmitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundMessage {
    /// Server salt to place in the encrypted header.
    pub salt: u64,
    /// Session ID to place in the encrypted header.
    pub session_id: u64,
    /// Outer `msg_id`.
    pub msg_id: u64,
    /// Outer `seq_no`.
    pub seq_no: u32,
    /// Serialized inner TL payload (single message or `msg_container`).
    pub body: Vec<u8>,
    /// Whether this packet expects any server response.
    pub needs_response: bool,
    /// Whether more queued requests remain in `to_send`.
    pub has_more: bool,
}

/// Event emitted by [`SessionState::handle_decrypted`].
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    /// RPC response payload (either normal TL return value or `rpc_error#2144ca19`) for `request_id`.
    RpcResult {
        request_id: RequestId,
        outer_msg_id: u64,
        body: Vec<u8>,
    },
    /// Server-pushed update (or `new_session_created`).
    Update(UpdatesMsg),
    /// Server `pong` response.
    Pong(mtp_types::Pong),
    /// `auth.bindTempAuthKey` succeeded; session may now send regular user requests.
    TempKeyBound,
    /// Persistent key was confirmed forgotten (`ENCRYPTED_MESSAGE_INVALID` on old key).
    PersistentKeyForgotten(u64),
}

/// Transport connection state for a session (`MTP::dcstate` in `mtproto/session_private.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionState {
    /// No active transport connection yet.
    #[default]
    Disconnected,
    /// Actively dialing or handshaking transport / auth key.
    Connecting,
    /// Connected and ready for encrypted RPC traffic.
    Connected,
    /// Waiting `retry_after_ms` milliseconds before reconnecting.
    Waiting {
        /// Backoff delay in milliseconds before the next connect attempt.
        retry_after_ms: u64,
    },
}

/// Control action requested by [`SessionState::handle_decrypted`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAction {
    /// Continue normal operation.
    Continue,
    /// Flush outbound queue immediately (e.g. resends, state requests, or acks queued).
    FlushOutbound,
    /// Restart TCP connection preserving session state.
    RestartConnection,
    /// Reset MTProto `session_id` and restart connection.
    ResetSession,
    /// Destroy current temporary key and restart connection.
    DestroyTemporaryKey,
}

/// Combined outcome of processing one decrypted incoming packet.
#[derive(Debug, Default)]
pub struct HandleOutcome {
    /// Control action for the session runner.
    pub action: Option<SessionAction>,
    /// Emitted events (RPC results, updates, pongs).
    pub events: Vec<SessionEvent>,
    /// Whether any acks are pending to be flushed (on ack timer or next send).
    pub has_pending_acks: bool,
}

#[derive(Debug, Clone)]
struct OuterInfo {
    outer_msg_id: u64,
    server_salt: u64,
    server_time: i32,
    bad_time: bool,
}

/// Deterministic MTProto 2.0 session protocol state machine (port of `SessionPrivate` core logic).
pub struct SessionState {
    shifted_dc_id: ShiftedDcId,
    dc_type: DcType,
    config: ClientConfig,
    clock: MtprotoClock,
    dcenter: Dcenter,

    session_id: u64,
    salts: SaltManager,
    messages_counter: u32,
    session_marked_as_started: bool,
    connected_state: bool,
    sync_time_request_duration: Duration,

    to_send: BTreeMap<RequestId, QueuedRequest>,
    have_sent: BTreeMap<u64, QueuedRequest>,
    req_to_msg: HashMap<RequestId, u64>,
    resending_ids: BTreeMap<u64, RequestId>,
    acked_ids: BTreeMap<u64, RequestId>,
    sent_containers: BTreeMap<u64, SentContainer>,
    state_and_resend_requests: BTreeMap<u64, Vec<u64>>,
    received_ids: ReceivedIdsManager,

    ack_queue: Vec<u64>,
    resend_req_queue: Vec<u64>,
    state_req_queue: BTreeSet<u64>,

    ping_id_to_send: Option<u64>,
    pending_ping_id: Option<u64>,
    ping_msg_id: Option<u64>,

    want_future_salts: bool,
    future_salts_msg_id: Option<u64>,

    // HTTP transport long-poll (`http_wait`) state.
    using_http_wait: bool,
    http_wait_needed: bool,

    // PFS temporary key binding state.
    bind_pending: Option<(ManagedAuthKey, ManagedAuthKey, CreatingKeyType)>,
    bind_msg_id: Option<u64>,
    bind_sent_at: Option<Instant>,
}

/// Outcome of [`SessionState::check_sent_requests`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckSentOutcome {
    /// No requests needed state checking.
    NothingDue,
    /// One or more `msgs_state_req` entries were queued.
    QueuedStateRequests,
    /// Pending PFS temporary-key bind timed out; restart connection.
    BindTimedOut,
}

impl SessionState {
    /// Creates a new `SessionState` with a freshly generated random 64-bit `session_id`.
    pub fn new(
        shifted_dc_id: ShiftedDcId,
        dc_type: DcType,
        config: ClientConfig,
        clock: MtprotoClock,
        dcenter: Dcenter,
        initial_salt: u64,
    ) -> Self {
        let mut session_id = 0u64;
        while session_id == 0 {
            session_id = rand::thread_rng().next_u64();
        }
        Self {
            shifted_dc_id,
            dc_type,
            config,
            clock,
            dcenter,
            session_id,
            salts: SaltManager::new(initial_salt),
            messages_counter: 0,
            session_marked_as_started: false,
            connected_state: initial_salt != 0,
            sync_time_request_duration: FAST_REQUEST_DURATION,
            to_send: BTreeMap::new(),
            have_sent: BTreeMap::new(),
            req_to_msg: HashMap::new(),
            resending_ids: BTreeMap::new(),
            acked_ids: BTreeMap::new(),
            sent_containers: BTreeMap::new(),
            state_and_resend_requests: BTreeMap::new(),
            received_ids: ReceivedIdsManager::new(),
            ack_queue: Vec::new(),
            resend_req_queue: Vec::new(),
            state_req_queue: BTreeSet::new(),
            ping_id_to_send: None,
            pending_ping_id: None,
            ping_msg_id: None,
            want_future_salts: false,
            future_salts_msg_id: None,
            using_http_wait: false,
            http_wait_needed: false,
            bind_pending: None,
            bind_msg_id: None,
            bind_sent_at: None,
        }
    }

    /// Enables or disables `http_wait` long-poll framing for HTTP transports
    /// (`AbstractConnection::usingHttpWait`).
    pub fn set_using_http_wait(&mut self, enabled: bool) {
        self.using_http_wait = enabled;
        if !enabled {
            self.http_wait_needed = false;
        }
    }

    /// Returns whether `http_wait` framing is currently active on this session.
    pub const fn is_using_http_wait(&self) -> bool {
        self.using_http_wait
    }

    /// Requests a standalone `http_wait` long-poll packet on the next [`Self::build_next_outbound`]
    /// if no other messages are queued (`HttpConnection::needHttpWait`).
    pub fn request_http_wait(&mut self) {
        if self.using_http_wait {
            self.http_wait_needed = true;
        }
    }

    /// Current 64-bit MTProto `session_id`.
    pub const fn session_id(&self) -> u64 {
        self.session_id
    }

    /// Active server salt manager.
    pub const fn salts(&self) -> &SaltManager {
        &self.salts
    }

    /// Mutable access to server salt manager.
    pub fn salts_mut(&mut self) -> &mut SaltManager {
        &mut self.salts
    }

    /// Current `messages_counter` (number of content-related messages sent in this session).
    pub const fn messages_counter(&self) -> u32 {
        self.messages_counter
    }

    /// Returns `true` if this is a main DC session (shift == 0).
    pub const fn is_main_session(&self) -> bool {
        get_dc_id_shift(self.shifted_dc_id) == 0
    }

    /// Returns `true` if the session has confirmed a salt and is in `ConnectedState`.
    pub const fn is_connected_state(&self) -> bool {
        self.connected_state
    }

    /// Returns `true` if the session is currently waiting to bind a PFS temporary key.
    pub const fn is_binding_temp_key(&self) -> bool {
        self.bind_pending.is_some()
    }

    /// Returns the temporary key of a bind that is still pending (for example after the
    /// connection restarted mid-bind) and schedules the bind request to be sent again.
    pub fn resume_pending_bind(&mut self) -> Option<AuthKey> {
        let key = self
            .bind_pending
            .as_ref()
            .map(|(_, temp, _)| temp.key.clone())?;
        self.bind_msg_id = None;
        self.bind_sent_at = None;
        Some(key)
    }

    /// Configures pending PFS temporary key binding (`auth.bindTempAuthKey`).
    pub fn set_pending_temp_key_bind(
        &mut self,
        persistent_key: ManagedAuthKey,
        temporary_key: ManagedAuthKey,
        creating_type: CreatingKeyType,
    ) {
        self.bind_pending = Some((persistent_key, temporary_key, creating_type));
        self.bind_msg_id = None;
        self.bind_sent_at = None;
    }

    /// Aborts any pending PFS temporary key bind and returns the claimed [`CreatingKeyType`]
    /// so the caller can release the creation lock (`SessionPrivate::destroyTemporaryKey` in
    /// `mtproto/session_private.cpp:2703`).
    pub fn abort_pending_bind(&mut self) -> Option<CreatingKeyType> {
        self.bind_msg_id = None;
        self.bind_sent_at = None;
        self.bind_pending
            .take()
            .map(|(_, _, creating_type)| creating_type)
    }

    /// Generates the next `seq_no` (`SessionPrivate::nextRequestSeqNumber`).
    ///
    /// Content-related messages receive `2 * n + 1` and increment `n`; non-content-related
    /// messages receive `2 * n` without incrementing `n`.
    pub fn next_seq_no(&mut self, need_ack: bool) -> u32 {
        let base = self.messages_counter;
        if need_ack {
            self.messages_counter = self.messages_counter.wrapping_add(1);
            base * 2 + 1
        } else {
            base * 2
        }
    }

    /// Resets `session_id`, `messages_counter`, and session-scoped ack/received-id tables
    /// (`SessionPrivate::changeSessionId`), and moves all in-flight requests back to `to_send`.
    pub fn reset_session(&mut self) {
        let old = self.session_id;
        let mut next = old;
        while next == old || next == 0 {
            next = rand::thread_rng().next_u64();
        }
        self.session_id = next;
        self.messages_counter = 0;
        self.session_marked_as_started = false;
        // Match `SessionPrivate::changeSessionId` / `SessionPrivate::tryToSend`
        // (`mtproto/session_private.cpp`): a reset session must re-initialize layer/connection.
        self.dcenter.set_connection_inited(false);
        self.ack_queue.clear();
        self.resend_req_queue.clear();
        self.state_req_queue.clear();
        self.received_ids.clear();
        self.sent_containers.clear();
        self.state_and_resend_requests.clear();
        self.bind_msg_id = None;
        self.ping_msg_id = None;
        self.pending_ping_id = None;
        self.future_salts_msg_id = None;
        // Clear stale msg_id/seq_no on requests already queued in to_send (e.g. from an earlier
        // resend) so they do not reuse pre-reset sequence numbers after session_id rotates.
        // See tdesktop: Telegram/SourceFiles/mtproto/session_private.cpp:417-432, 470-472.
        for req in self.to_send.values_mut() {
            req.msg_id = None;
            req.seq_no = None;
            self.req_to_msg.remove(&req.request_id);
        }
        // Move all in-flight requests back to to_send with cleared msg_id so they get fresh IDs.
        let sent = std::mem::take(&mut self.have_sent);
        self.resending_ids.clear();
        for (_, mut req) in sent {
            req.msg_id = None;
            req.seq_no = None;
            self.req_to_msg.remove(&req.request_id);
            self.to_send.insert(req.request_id, req);
        }
    }

    /// Updates the proxy stored in `self.config` so `initConnection.proxy` reflects proxy changes.
    pub fn set_proxy(&mut self, proxy: ProxyData) {
        self.config.proxy = proxy;
    }

    /// Called after a TCP reconnect on the same auth key (`authKeyChecked`).
    pub fn on_transport_connected(&mut self) {
        // Re-wrap the first request after a connect/reconnect in
        // `invokeWithLayer(LAYER, initConnection(...))` (`SessionPrivate::tryToSend` in
        // `mtproto/session_private.cpp`), then clear once the first RPC succeeds on this connection.
        self.dcenter.set_connection_inited(false);
        self.ping_msg_id = None;
        if let Some(in_flight_ping) = self.pending_ping_id.take()
            && self.ping_id_to_send.is_none()
        {
            self.ping_id_to_send = Some(in_flight_ping);
        }
        self.bind_msg_id = None;
        self.future_salts_msg_id = None;
        let active_salt = self.salts.current_salt(self.clock.now_secs());
        if active_salt != 0 {
            self.connected_state = true;
            self.resend_all();
        } else {
            self.connected_state = false;
        }
        // Always ensure at least one initial ping is scheduled to confirm liveness / fetch salt,
        // without clobbering an explicit caller ping_id already queued.
        if self.ping_id_to_send.is_none() {
            let mut ping_id = rand::thread_rng().next_u64();
            if ping_id == 0 {
                ping_id = 1;
            }
            self.ping_id_to_send = Some(ping_id);
        }
    }

    /// Enqueues a user request to be sent.
    pub fn enqueue_request(&mut self, mut req: QueuedRequest) {
        req.msg_id = None;
        req.seq_no = None;
        self.to_send.insert(req.request_id, req);
    }

    /// Cancels a pending or in-flight request (`Session::cancel`).
    pub fn cancel_request(&mut self, request_id: RequestId) -> bool {
        let mut removed = self.to_send.remove(&request_id).is_some();
        if let Some(msg_id) = self.req_to_msg.remove(&request_id) {
            if self.have_sent.remove(&msg_id).is_some() {
                removed = true;
            }
            self.resending_ids.remove(&msg_id);
            self.state_req_queue.remove(&msg_id);
        }
        removed
    }

    /// Schedules a keepalive or explicit ping with `ping_id`.
    pub fn schedule_ping(&mut self, ping_id: u64) {
        self.ping_id_to_send = Some(ping_id);
    }

    /// Schedules a `get_future_salts` service message if not already in flight.
    pub fn schedule_future_salts(&mut self) {
        if self.future_salts_msg_id.is_none() {
            self.want_future_salts = true;
        }
    }

    /// Returns number of queued unsent requests.
    pub fn to_send_len(&self) -> usize {
        self.to_send.len()
    }

    /// Returns number of in-flight sent requests awaiting response/ack.
    pub fn have_sent_len(&self) -> usize {
        self.have_sent.len()
    }

    /// Returns pending ack count.
    pub fn pending_ack_len(&self) -> usize {
        self.ack_queue.len()
    }

    /// Checks in-flight sent requests older than [`CHECK_SENT_REQUEST_TIMEOUT`] and queues
    /// `msgs_state_req` for them (`SessionPrivate::checkSentRequests`).
    pub fn check_sent_requests(&mut self, now: Instant) -> CheckSentOutcome {
        if let Some(bind_at) = self.bind_sent_at
            && self.bind_msg_id.is_some()
            && now.saturating_duration_since(bind_at) >= CHECK_SENT_REQUEST_TIMEOUT
        {
            return CheckSentOutcome::BindTimedOut;
        }
        let mut queued = false;
        for (&msg_id, req) in &mut self.have_sent {
            if let Some(sent_at) = req.last_sent_at
                && now.saturating_duration_since(sent_at) >= CHECK_SENT_REQUEST_TIMEOUT
            {
                req.last_sent_at = Some(now);
                if self.state_req_queue.insert(msg_id) {
                    queued = true;
                }
            }
        }
        if queued {
            CheckSentOutcome::QueuedStateRequests
        } else {
            CheckSentOutcome::NothingDue
        }
    }

    /// Evicts sent containers older than [`SENT_CONTAINER_LIVES`] and resends their still-pending
    /// inner messages (`SessionPrivate::clearOldContainers`).
    pub fn clear_old_containers(&mut self, now: Instant) -> bool {
        let expired_ids: Vec<u64> = self
            .sent_containers
            .iter()
            .filter_map(|(&cid, c)| {
                (now.saturating_duration_since(c.sent_at) >= SENT_CONTAINER_LIVES).then_some(cid)
            })
            .collect();
        let mut resent = false;
        for cid in expired_ids {
            if let Some(container) = self.sent_containers.remove(&cid) {
                for inner_id in container.messages {
                    if self.resend(inner_id) {
                        resent = true;
                    }
                }
            }
        }
        resent
    }

    /// Moves a single in-flight message (or all messages in a sent container) back into `to_send`
    /// with `force_send_in_container = true` (`SessionPrivate::resend`).
    pub fn resend(&mut self, msg_id: u64) -> bool {
        let mut resent_special = false;
        if self.ping_msg_id == Some(msg_id) {
            if let Some(pid) = self.pending_ping_id.take()
                && self.ping_id_to_send.is_none()
            {
                self.ping_id_to_send = Some(pid);
            }
            self.ping_msg_id = None;
            resent_special = true;
        }
        self.clear_special_msg_id(msg_id);
        if let Some(container) = self.sent_containers.remove(&msg_id) {
            let mut any = resent_special;
            for inner in container.messages {
                if self.resend(inner) {
                    any = true;
                }
            }
            return any;
        }
        if let Some(mut req) = self.have_sent.remove(&msg_id) {
            req.last_sent_at = Some(Instant::now());
            req.force_send_in_container = true;
            self.resending_ids.insert(msg_id, req.request_id);
            self.to_send.insert(req.request_id, req);
            return true;
        }
        resent_special
    }

    /// Moves all currently in-flight `have_sent` requests back to `to_send` (`SessionPrivate::resendAll`).
    pub fn resend_all(&mut self) {
        let now = Instant::now();
        let sent = std::mem::take(&mut self.have_sent);
        for (msg_id, mut req) in sent {
            req.last_sent_at = Some(now);
            req.force_send_in_container = true;
            self.resending_ids.insert(msg_id, req.request_id);
            self.to_send.insert(req.request_id, req);
        }
    }

    fn clear_special_msg_id(&mut self, msg_id: u64) {
        if self.ping_msg_id == Some(msg_id) {
            self.ping_msg_id = None;
            self.pending_ping_id = None;
        }
        if self.bind_msg_id == Some(msg_id) {
            self.bind_msg_id = None;
            self.bind_sent_at = None;
        }
        if self.future_salts_msg_id == Some(msg_id) {
            self.future_salts_msg_id = None;
        }
    }

    /// Builds the next outbound MTProto message (either a single message or a `msg_container`)
    /// from pending service messages and queued user requests (`SessionPrivate::tryToSend`).
    pub fn build_next_outbound(&mut self) -> Option<OutboundMessage> {
        let send_only_first_ping = !self.connected_state;
        let send_all = !send_only_first_ping && self.bind_pending.is_none();

        if send_only_first_ping && self.ping_id_to_send.is_none() {
            return None;
        }

        let force_new_msg_id = send_all && !self.session_marked_as_started;
        if force_new_msg_id {
            self.session_marked_as_started = true;
        }

        let needs_layer = !self.dcenter.connection_inited();
        let is_cdn = self.dc_type == DcType::Cdn;
        let now_instant = Instant::now();

        // Build service messages.
        let mut service_msgs: Vec<ServiceOutbound> = Vec::new();

        if let Some(ping_id) = self.ping_id_to_send.take() {
            let body = if send_only_first_ping || !self.is_main_session() {
                mtp_functions::Ping {
                    ping_id: ping_id as i64,
                }
                .to_bytes()
            } else {
                mtp_functions::PingDelayDisconnect {
                    ping_id: ping_id as i64,
                    disconnect_delay: self.config.ping_disconnect_delay_secs,
                }
                .to_bytes()
            };
            self.pending_ping_id = Some(ping_id);
            service_msgs.push(ServiceOutbound {
                kind: ServiceKind::Ping,
                msg_id: None,
                need_ack: true,
                body,
            });
        }

        if !send_only_first_ping {
            if self.bind_msg_id.is_none()
                && let Some((perm_key, temp_key, _)) = &self.bind_pending
                && let Ok((bind_id, bind_body)) =
                    prepare_bind_temp_key_request(perm_key, temp_key, self.session_id, &self.clock)
            {
                service_msgs.push(ServiceOutbound {
                    kind: ServiceKind::BindTempKey,
                    msg_id: Some(bind_id),
                    need_ack: true,
                    body: bind_body,
                });
            }

            if self.want_future_salts && self.future_salts_msg_id.is_none() {
                self.want_future_salts = false;
                service_msgs.push(ServiceOutbound {
                    kind: ServiceKind::FutureSalts,
                    msg_id: None,
                    need_ack: true,
                    body: mtp_functions::GetFutureSalts {
                        num: DEFAULT_FUTURE_SALTS_COUNT,
                    }
                    .to_bytes(),
                });
            }

            if !self.state_req_queue.is_empty() {
                let ids: Vec<u64> = std::mem::take(&mut self.state_req_queue)
                    .into_iter()
                    .collect();
                let body = mtp_enums::MsgsStateReq::MsgsStateReq(mtp_types::MsgsStateReq {
                    msg_ids: ids.iter().map(|&id| id as i64).collect(),
                })
                .to_bytes();
                service_msgs.push(ServiceOutbound {
                    kind: ServiceKind::StateReq(ids),
                    msg_id: None,
                    need_ack: true,
                    body,
                });
            }

            if !self.resend_req_queue.is_empty() {
                let ids = std::mem::take(&mut self.resend_req_queue);
                let body = mtp_enums::MsgResendReq::MsgResendReq(mtp_types::MsgResendReq {
                    msg_ids: ids.iter().map(|&id| id as i64).collect(),
                })
                .to_bytes();
                service_msgs.push(ServiceOutbound {
                    kind: ServiceKind::ResendReq(ids),
                    msg_id: None,
                    need_ack: true,
                    body,
                });
            }

            if !self.ack_queue.is_empty() {
                let ids = std::mem::take(&mut self.ack_queue);
                let body = mtp_enums::MsgsAck::MsgsAck(mtp_types::MsgsAck {
                    msg_ids: ids.into_iter().map(|id| id as i64).collect(),
                })
                .to_bytes();
                service_msgs.push(ServiceOutbound {
                    kind: ServiceKind::Ack,
                    msg_id: None,
                    need_ack: false,
                    body,
                });
            }
        }

        // Select user requests up to CUT_CONTAINER_ON_BYTES.
        let mut user_reqs: Vec<QueuedRequest> = Vec::new();
        let mut has_more = false;
        if send_all && !self.to_send.is_empty() {
            let mut combined_bytes = 0usize;
            let keys: Vec<RequestId> = self.to_send.keys().copied().collect();
            let mut take_count = 0usize;
            for req_id in &keys {
                if let Some(req) = self.to_send.get(req_id) {
                    combined_bytes += req.body.len() + 32;
                    take_count += 1;
                    if combined_bytes >= CUT_CONTAINER_ON_BYTES {
                        if take_count < keys.len() {
                            has_more = true;
                        }
                        break;
                    }
                }
            }
            for req_id in keys.into_iter().take(take_count) {
                if let Some(req) = self.to_send.remove(&req_id) {
                    user_reqs.push(req);
                }
            }
        }

        let has_other_messages = !service_msgs.is_empty() || !user_reqs.is_empty();
        if !send_only_first_ping
            && self.using_http_wait
            && (has_other_messages || self.http_wait_needed)
        {
            self.http_wait_needed = false;
            service_msgs.push(ServiceOutbound {
                kind: ServiceKind::HttpWait,
                msg_id: None,
                need_ack: false,
                body: mtp_enums::HttpWait::HttpWait(mtp_types::HttpWait {
                    max_delay: HTTP_WAIT_MAX_DELAY_MS,
                    wait_after: HTTP_WAIT_WAIT_AFTER_MS,
                    max_wait: HTTP_WAIT_MAX_WAIT_MS,
                })
                .to_bytes(),
            });
        }

        let total_items = service_msgs.len() + user_reqs.len();
        if total_items == 0 {
            return None;
        }

        let salt = self.salts.current_salt(self.clock.now_secs());
        let force_container =
            total_items > 1 || user_reqs.first().is_some_and(|r| r.force_send_in_container);

        if !force_container {
            // Single message (no msg_container wrapper).
            if let Some(svc) = service_msgs.into_iter().next() {
                let msg_id = svc.msg_id.unwrap_or_else(|| self.clock.next_msg_id());
                let seq_no = self.next_seq_no(svc.need_ack);
                let needs_response = self.record_service_sent(svc.kind, msg_id, now_instant);
                return Some(OutboundMessage {
                    salt,
                    session_id: self.session_id,
                    msg_id,
                    seq_no,
                    body: svc.body,
                    needs_response,
                    has_more,
                });
            }
            if let Some(mut req) = user_reqs.into_iter().next() {
                let (msg_id, seq_no) = self.assign_request_id(&mut req, force_new_msg_id);
                let wire_body = self.prepare_user_wire_body(&req, needs_layer, is_cdn);
                let needs_response = req.need_ack;
                req.last_sent_at = Some(now_instant);
                if req.need_ack {
                    self.req_to_msg.insert(req.request_id, msg_id);
                    self.have_sent.insert(msg_id, req);
                } else {
                    self.acked_ids.insert(msg_id, req.request_id);
                }
                return Some(OutboundMessage {
                    salt,
                    session_id: self.session_id,
                    msg_id,
                    seq_no,
                    body: wire_body,
                    needs_response,
                    has_more,
                });
            }
            return None;
        }

        // Pack into msg_container#73f1f8dc.
        let mut container_items = Vec::with_capacity(total_items);
        let mut tracked_inner_ids = Vec::with_capacity(total_items);
        let mut needs_response = false;

        // Place BindTempKey & Ping first (matches tdesktop ordering).
        let (front_svcs, back_svcs): (Vec<_>, Vec<_>) = service_msgs
            .into_iter()
            .partition(|s| matches!(s.kind, ServiceKind::BindTempKey | ServiceKind::Ping));

        // The bind request carries a msg_id fixed when it was prepared (it is part of the
        // encrypted bind payload), so it can be older than a fresh ping id. Within a session a
        // message with a higher msg_id must have a higher seq_no (otherwise the server answers
        // bad_msg_notification 33), so hand out seq_no in msg_id order.
        let mut front_svcs: Vec<(u64, ServiceOutbound)> = front_svcs
            .into_iter()
            .map(|svc| (svc.msg_id.unwrap_or_else(|| self.clock.next_msg_id()), svc))
            .collect();
        front_svcs.sort_by_key(|(msg_id, _)| *msg_id);
        for (msg_id, svc) in front_svcs {
            let seq_no = self.next_seq_no(svc.need_ack);
            if self.record_service_sent(svc.kind, msg_id, now_instant) {
                needs_response = true;
            }
            tracked_inner_ids.push(msg_id);
            container_items.push(CoreMessage {
                msg_id: msg_id as i64,
                seqno: seq_no as i32,
                body: RawObject(svc.body),
            });
        }

        for mut req in user_reqs {
            let (msg_id, seq_no) = self.assign_request_id(&mut req, force_new_msg_id);
            let wire_body = self.prepare_user_wire_body(&req, needs_layer, is_cdn);
            req.last_sent_at = Some(now_instant);
            if req.need_ack {
                self.req_to_msg.insert(req.request_id, msg_id);
                self.have_sent.insert(msg_id, req);
                tracked_inner_ids.push(msg_id);
                needs_response = true;
            } else {
                self.acked_ids.insert(msg_id, req.request_id);
            }
            container_items.push(CoreMessage {
                msg_id: msg_id as i64,
                seqno: seq_no as i32,
                body: RawObject(wire_body),
            });
        }

        for svc in back_svcs {
            let msg_id = svc.msg_id.unwrap_or_else(|| self.clock.next_msg_id());
            let seq_no = self.next_seq_no(svc.need_ack);
            if self.record_service_sent(svc.kind, msg_id, now_instant) {
                needs_response = true;
            }
            container_items.push(CoreMessage {
                msg_id: msg_id as i64,
                seqno: seq_no as i32,
                body: RawObject(svc.body),
            });
        }

        // Container's outer msg_id MUST be strictly greater than all inner msg_ids, and its
        // seq_no is even (not content-related).
        let container_msg_id = self.clock.next_msg_id();
        let container_seq_no = self.next_seq_no(false);
        self.sent_containers.insert(
            container_msg_id,
            SentContainer {
                sent_at: now_instant,
                messages: tracked_inner_ids,
            },
        );
        let body = pack_container(container_items);

        Some(OutboundMessage {
            salt,
            session_id: self.session_id,
            msg_id: container_msg_id,
            seq_no: container_seq_no,
            body,
            needs_response,
            has_more,
        })
    }

    fn assign_request_id(&mut self, req: &mut QueuedRequest, force_new_msg_id: bool) -> (u64, u32) {
        let candidate = self.clock.next_msg_id();
        if let Some(old_id) = req.msg_id {
            self.resending_ids.remove(&old_id);
            if force_new_msg_id || old_id > candidate {
                let mut new_id = candidate;
                while self.resending_ids.contains_key(&new_id)
                    || self.acked_ids.contains_key(&new_id)
                    || self.have_sent.contains_key(&new_id)
                {
                    new_id = self.clock.next_msg_id();
                }
                self.replace_msg_id_references(old_id, new_id);
                let seq_no = self.next_seq_no(req.need_ack);
                req.msg_id = Some(new_id);
                req.seq_no = Some(seq_no);
                return (new_id, seq_no);
            }
            let seq_no = req.seq_no.unwrap_or_else(|| self.next_seq_no(req.need_ack));
            req.seq_no = Some(seq_no);
            return (old_id, seq_no);
        }
        let seq_no = self.next_seq_no(req.need_ack);
        req.msg_id = Some(candidate);
        req.seq_no = Some(seq_no);
        (candidate, seq_no)
    }

    fn replace_msg_id_references(&mut self, old_id: u64, new_id: u64) {
        if let Some(req_id) = self.resending_ids.remove(&old_id) {
            self.resending_ids.insert(new_id, req_id);
        }
        if let Some(req_id) = self.acked_ids.remove(&old_id) {
            self.acked_ids.insert(new_id, req_id);
        }
        if let Some(req) = self.have_sent.remove(&old_id) {
            self.req_to_msg.insert(req.request_id, new_id);
            self.have_sent.insert(new_id, req);
        }
        for container in self.sent_containers.values_mut() {
            for inner in &mut container.messages {
                if *inner == old_id {
                    *inner = new_id;
                }
            }
        }
    }

    fn prepare_user_wire_body(
        &self,
        req: &QueuedRequest,
        needs_layer: bool,
        is_cdn: bool,
    ) -> Vec<u8> {
        let mut body = req.body.clone();
        if let Some(after_req_id) = req.after_request_id
            && let Some(&after_msg_id) = self.req_to_msg.get(&after_req_id)
            && self.have_sent.contains_key(&after_msg_id)
        {
            body = wrap_invoke_after_msg(after_msg_id, &body);
        }
        if needs_layer && req.needs_layer {
            body = wrap_init_connection(&self.config, LAYER, is_cdn, &body);
        }
        gzip_pack_if_smaller(&body)
    }

    fn record_service_sent(&mut self, kind: ServiceKind, msg_id: u64, now: Instant) -> bool {
        match kind {
            ServiceKind::Ping => {
                self.ping_msg_id = Some(msg_id);
                true
            }
            ServiceKind::BindTempKey => {
                self.bind_msg_id = Some(msg_id);
                self.bind_sent_at = Some(now);
                true
            }
            ServiceKind::FutureSalts => {
                self.future_salts_msg_id = Some(msg_id);
                true
            }
            ServiceKind::StateReq(ids) | ServiceKind::ResendReq(ids) => {
                self.state_and_resend_requests.insert(msg_id, ids);
                true
            }
            ServiceKind::Ack => false,
            ServiceKind::HttpWait => true,
        }
    }

    /// Processes one decrypted incoming MTProto envelope (`SessionPrivate::handleReceived`).
    pub fn handle_decrypted(&mut self, envelope: DecryptedEnvelope) -> HandleOutcome {
        let mut outcome = HandleOutcome::default();

        if envelope.session_id != self.session_id {
            warn!(
                expected = self.session_id,
                got = envelope.session_id,
                "bad server session_id received"
            );
            outcome.action = Some(SessionAction::RestartConnection);
            return outcome;
        }
        if !is_valid_server_msg_id(envelope.msg_id) {
            warn!(msg_id = envelope.msg_id, "bad server msg_id parity");
            outcome.action = Some(SessionAction::RestartConnection);
            return outcome;
        }

        let server_time = envelope.server_time();
        let client_time = self.clock.now_secs();
        let bad_time = is_bad_server_time(server_time, client_time);
        let was_connected = self.connected_state;

        let mut passed_salt = envelope.server_salt;
        if envelope.server_salt != self.salts.active_salt() {
            if !bad_time {
                self.salts.set_salt(envelope.server_salt);
                if !self.connected_state {
                    self.connected_state = true;
                    self.resend_all();
                }
            }
        } else {
            passed_salt = 0;
        }

        let need_ack = envelope.need_ack();
        if need_ack {
            self.ack_queue.push(envelope.msg_id);
        }

        let action = match self.received_ids.register_msg_id(envelope.msg_id, need_ack) {
            RegisterIdResult::Duplicate => SessionAction::Continue,
            RegisterIdResult::TooOld => SessionAction::ResetSession,
            RegisterIdResult::Success => {
                let mut budget = MAX_UNPACKED_MESSAGE_LENGTH;
                let mut info = OuterInfo {
                    outer_msg_id: envelope.msg_id,
                    server_salt: passed_salt,
                    server_time,
                    bad_time,
                };
                self.handle_one_message(
                    &envelope.body,
                    &mut info,
                    &mut budget,
                    0,
                    &mut outcome.events,
                )
            }
        };
        self.received_ids.shrink();

        outcome.has_pending_acks = !self.ack_queue.is_empty();
        if action != SessionAction::Continue {
            outcome.action = Some(action);
        } else if (!was_connected && self.connected_state)
            || !self.to_send.is_empty()
            || !self.resend_req_queue.is_empty()
            || !self.state_req_queue.is_empty()
            || self.ping_id_to_send.is_some()
        {
            outcome.action = Some(SessionAction::FlushOutbound);
        }
        outcome
    }

    fn handle_one_message(
        &mut self,
        body: &[u8],
        info: &mut OuterInfo,
        unpacked_budget: &mut usize,
        gzip_depth: usize,
        events: &mut Vec<SessionEvent>,
    ) -> SessionAction {
        if body.len() < 4 {
            return SessionAction::RestartConnection;
        }
        let ctor = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
        match ctor {
            GzipPacked::CONSTRUCTOR_ID => {
                if gzip_depth >= MAX_GZIP_NESTING {
                    return SessionAction::RestartConnection;
                }
                let Ok(unpacked) = unwrap_gzip_layers(body, unpacked_budget, gzip_depth) else {
                    return SessionAction::RestartConnection;
                };
                self.handle_one_message(&unpacked, info, unpacked_budget, gzip_depth + 1, events)
            }

            MsgContainer::CONSTRUCTOR_ID => {
                let Ok(messages) = unpack_container(body) else {
                    return SessionAction::RestartConnection;
                };
                let mut result_action = SessionAction::Continue;
                for inner in messages {
                    let inner_msg_id = inner.msg_id as u64;
                    if !is_valid_server_msg_id(inner_msg_id) {
                        return SessionAction::RestartConnection;
                    }
                    let need_ack = (inner.seqno & 1) != 0;
                    if need_ack {
                        self.ack_queue.push(inner_msg_id);
                    }
                    match self.received_ids.register_msg_id(inner_msg_id, need_ack) {
                        RegisterIdResult::Duplicate => continue,
                        RegisterIdResult::TooOld => return SessionAction::ResetSession,
                        RegisterIdResult::Success => {
                            let res = self.handle_one_message(
                                &inner.body.0,
                                info,
                                unpacked_budget,
                                gzip_depth,
                                events,
                            );
                            // Preserve FlushOutbound from inner handlers (e.g. bad_server_salt /
                            // bad_msg_notification resending a ping) and only clear info.bad_time
                            // inside handlers that actually validated time/salt.
                            // See tdesktop: Telegram/SourceFiles/mtproto/session_private.cpp:1546-1560, 2315-2321.
                            match res {
                                SessionAction::Continue => {}
                                SessionAction::FlushOutbound => {
                                    result_action = SessionAction::FlushOutbound;
                                }
                                other => return other,
                            }
                        }
                    }
                }
                result_action
            }

            mtp_types::MsgsAck::CONSTRUCTOR_ID => {
                let Ok(mtp_enums::MsgsAck::MsgsAck(ack)) = mtp_enums::MsgsAck::from_bytes(body)
                else {
                    return SessionAction::RestartConnection;
                };
                if ack.msg_ids.is_empty() {
                    return SessionAction::Continue;
                }
                let ack_ids: Vec<u64> = ack.msg_ids.into_iter().map(|id| id as u64).collect();
                if info.bad_time {
                    if !self.requests_fix_time_salt(&ack_ids, info) {
                        return SessionAction::Continue;
                    }
                    info.bad_time = false;
                } else {
                    self.correct_unixtime_by_fast_request(&ack_ids, info.server_time);
                }
                self.requests_acked(&ack_ids, false);
                SessionAction::Continue
            }

            mtp_types::BadMsgNotification::CONSTRUCTOR_ID
            | mtp_types::BadServerSalt::CONSTRUCTOR_ID => {
                let Ok(parsed) = mtp_enums::BadMsgNotification::from_bytes(body) else {
                    return SessionAction::RestartConnection;
                };
                match parsed {
                    mtp_enums::BadMsgNotification::BadMsgNotification(notif) => {
                        let resend_id = notif.bad_msg_id as u64;
                        let code = notif.error_code;
                        debug!(
                            bad_msg_id = resend_id,
                            seq_no = notif.bad_msg_seqno,
                            code,
                            "received bad_msg_notification"
                        );
                        if matches!(code, 16 | 17 | 32 | 33 | 64) {
                            if self.was_sent(resend_id).is_none() {
                                return SessionAction::Continue;
                            }
                            if matches!(code, 16 | 17 | 64) {
                                if info.server_salt != 0 {
                                    self.salts.set_salt(info.server_salt);
                                }
                                self.correct_unixtime_with_bad_local(info.server_time);
                                info.bad_time = false;
                                self.resend(resend_id);
                                return SessionAction::FlushOutbound;
                            }
                            // Codes 32 / 33: seq_no inconsistent -> reset session.
                            if info.bad_time {
                                if info.server_salt != 0 {
                                    self.salts.set_salt(info.server_salt);
                                }
                                self.correct_unixtime_with_bad_local(info.server_time);
                                info.bad_time = false;
                            }
                            if self.bind_msg_id.is_some() {
                                return SessionAction::RestartConnection;
                            }
                            return SessionAction::ResetSession;
                        }
                        // Fatal bad_msg_notification.
                        if self.bind_msg_id.is_some() {
                            return SessionAction::RestartConnection;
                        }
                        if let Some(SentKind::User(req_id)) = self.was_sent(resend_id) {
                            self.have_sent.remove(&resend_id);
                            self.req_to_msg.remove(&req_id);
                            let synthetic = mtp_enums::RpcError::RpcError(mtp_types::RpcError {
                                error_code: 500,
                                error_message: "PROTOCOL_ERROR".to_string(),
                            })
                            .to_bytes();
                            events.push(SessionEvent::RpcResult {
                                request_id: req_id,
                                outer_msg_id: info.outer_msg_id,
                                body: synthetic,
                            });
                        }
                        SessionAction::Continue
                    }
                    mtp_enums::BadMsgNotification::BadServerSalt(bad) => {
                        let bad_msg_id = bad.bad_msg_id as u64;
                        if self.was_sent(bad_msg_id).is_none() {
                            return SessionAction::Continue;
                        }
                        self.salts.set_salt(bad.new_server_salt as u64);
                        self.clock.update_server_time(info.server_time, false);
                        if self.bind_msg_id.is_some() {
                            return SessionAction::RestartConnection;
                        }
                        if !self.connected_state {
                            self.connected_state = true;
                            self.resend_all();
                        }
                        info.bad_time = false;
                        self.resend(bad_msg_id);
                        SessionAction::FlushOutbound
                    }
                }
            }

            mtp_types::MsgsStateInfo::CONSTRUCTOR_ID => {
                let Ok(mtp_enums::MsgsStateInfo::MsgsStateInfo(state_info)) =
                    mtp_enums::MsgsStateInfo::from_bytes(body)
                else {
                    return SessionAction::RestartConnection;
                };
                let req_msg_id = state_info.req_msg_id as u64;
                let Some(queried_ids) = self.state_and_resend_requests.remove(&req_msg_id) else {
                    return SessionAction::Continue;
                };
                if info.bad_time {
                    if info.server_salt != 0 {
                        self.salts.set_salt(info.server_salt);
                    }
                    self.correct_unixtime_with_bad_local(info.server_time);
                    info.bad_time = false;
                }
                self.handle_msgs_states(&queried_ids, &state_info.info);
                SessionAction::Continue
            }

            mtp_types::MsgsAllInfo::CONSTRUCTOR_ID => {
                if info.bad_time {
                    return SessionAction::Continue;
                }
                let Ok(mtp_enums::MsgsAllInfo::MsgsAllInfo(all_info)) =
                    mtp_enums::MsgsAllInfo::from_bytes(body)
                else {
                    return SessionAction::RestartConnection;
                };
                let msg_ids: Vec<u64> = all_info.msg_ids.into_iter().map(|id| id as u64).collect();
                self.handle_msgs_states(&msg_ids, &all_info.info);
                SessionAction::Continue
            }

            mtp_types::MsgDetailedInfo::CONSTRUCTOR_ID
            | mtp_types::MsgNewDetailedInfo::CONSTRUCTOR_ID => {
                let Ok(parsed) = mtp_enums::MsgDetailedInfo::from_bytes(body) else {
                    return SessionAction::RestartConnection;
                };
                match parsed {
                    mtp_enums::MsgDetailedInfo::MsgDetailedInfo(detail) => {
                        let msg_id = detail.msg_id as u64;
                        let answer_msg_id = detail.answer_msg_id as u64;
                        if info.bad_time {
                            if self.requests_fix_time_salt(&[msg_id], info) {
                                info.bad_time = false;
                            } else {
                                return SessionAction::Continue;
                            }
                        }
                        self.requests_acked(&[msg_id], false);
                        if self.received_ids.lookup(answer_msg_id) != ReceivedIdState::NotFound {
                            self.ack_queue.push(answer_msg_id);
                        } else {
                            self.resend_req_queue.push(answer_msg_id);
                        }
                        SessionAction::Continue
                    }
                    mtp_enums::MsgDetailedInfo::MsgNewDetailedInfo(detail) => {
                        if info.bad_time {
                            return SessionAction::Continue;
                        }
                        let answer_msg_id = detail.answer_msg_id as u64;
                        if self.received_ids.lookup(answer_msg_id) != ReceivedIdState::NotFound {
                            self.ack_queue.push(answer_msg_id);
                        } else {
                            self.resend_req_queue.push(answer_msg_id);
                        }
                        SessionAction::Continue
                    }
                }
            }

            mtp_types::MsgResendReq::CONSTRUCTOR_ID => {
                // Ignore unsolicited resend requests outside the valid server time window.
                // See tdesktop: Telegram/SourceFiles/mtproto/session_private.cpp:1781, 1832.
                if info.bad_time {
                    return SessionAction::Continue;
                }
                // Server asks client to resend specific client messages.
                let Ok(mtp_enums::MsgResendReq::MsgResendReq(req)) =
                    mtp_enums::MsgResendReq::from_bytes(body)
                else {
                    return SessionAction::RestartConnection;
                };
                for id in req.msg_ids {
                    self.resend(id as u64);
                }
                SessionAction::FlushOutbound
            }

            mtp_types::FutureSalts::CONSTRUCTOR_ID => {
                let Ok(mtp_enums::FutureSalts::FutureSalts(fs)) =
                    mtp_enums::FutureSalts::from_bytes(body)
                else {
                    return SessionAction::RestartConnection;
                };
                let req_msg_id = fs.req_msg_id as u64;
                // Require matching sent request to fix time/salt when outer msg_id has bad_time.
                // See tdesktop: Telegram/SourceFiles/mtproto/session_private.cpp:1868-1875, 2008-2011.
                if info.bad_time {
                    if self.requests_fix_time_salt(&[req_msg_id], info) {
                        info.bad_time = false;
                    } else {
                        return SessionAction::Continue;
                    }
                }
                if self.future_salts_msg_id == Some(req_msg_id) {
                    self.future_salts_msg_id = None;
                }
                self.salts.apply_future_salts(fs.now, fs.salts);
                self.requests_acked(&[req_msg_id], true);
                SessionAction::Continue
            }

            RpcResult::CONSTRUCTOR_ID => {
                let Ok(Boxed(rpc)) = Boxed::<RpcResult>::from_bytes(body) else {
                    return SessionAction::RestartConnection;
                };
                let req_msg_id = rpc.req_msg_id as u64;
                if info.bad_time {
                    if self.requests_fix_time_salt(&[req_msg_id], info) {
                        info.bad_time = false;
                    } else {
                        return SessionAction::Continue;
                    }
                }
                let result_body =
                    match unwrap_gzip_layers(&rpc.result.0, unpacked_budget, gzip_depth) {
                        Ok(b) => b,
                        Err(_) => return SessionAction::RestartConnection,
                    };
                if result_body.len() < 4 {
                    return SessionAction::RestartConnection;
                }
                let res_ctor = u32::from_le_bytes([
                    result_body[0],
                    result_body[1],
                    result_body[2],
                    result_body[3],
                ]);
                if res_ctor == mtp_types::RpcError::CONSTRUCTOR_ID {
                    if let Ok(mtp_enums::RpcError::RpcError(tl_err)) =
                        mtp_enums::RpcError::from_bytes(&result_body)
                    {
                        let parsed = RpcError::parse(tl_err.error_code, &tl_err.error_message);
                        if parsed.is_destroyed_temp_key() {
                            return SessionAction::DestroyTemporaryKey;
                        }
                    }
                } else if self.bind_msg_id != Some(req_msg_id) {
                    self.dcenter.set_connection_inited(true);
                }

                // Look up user request_id BEFORE requests_acked removes it from have_sent.
                let sent_kind = self.was_sent(req_msg_id);
                self.requests_acked(&[req_msg_id], true);

                if self.bind_msg_id == Some(req_msg_id) {
                    return self.handle_bind_response(&result_body, events);
                }

                if let Some(SentKind::User(request_id)) = sent_kind {
                    self.req_to_msg.remove(&request_id);
                    events.push(SessionEvent::RpcResult {
                        request_id,
                        outer_msg_id: info.outer_msg_id,
                        body: result_body,
                    });
                }
                SessionAction::Continue
            }

            mtp_types::NewSessionCreated::CONSTRUCTOR_ID => {
                let Ok(mtp_enums::NewSession::Created(created)) =
                    mtp_enums::NewSession::from_bytes(body)
                else {
                    return SessionAction::RestartConnection;
                };
                let first_msg_id = created.first_msg_id as u64;
                if info.bad_time {
                    if self.requests_fix_time_salt(&[first_msg_id], info) {
                        info.bad_time = false;
                    } else {
                        return SessionAction::Continue;
                    }
                }
                self.salts.set_salt(created.server_salt as u64);
                if !self.connected_state {
                    self.connected_state = true;
                }
                let to_resend: Vec<u64> = self
                    .have_sent
                    .keys()
                    .copied()
                    .take_while(|&id| id < first_msg_id)
                    .collect();
                for id in to_resend {
                    self.resend(id);
                }
                // Match `Session::tryToReceive` (`mtproto/session.cpp:573-580`):
                // `const auto main = (_shiftedDcId == BareDcId(_shiftedDcId));`
                // Process updates and `new_session_created` notifications ONLY on unshifted
                // regular sessions so shifted upload/download media sessions do not trigger
                // spurious `updates.getDifference` bursts on the main DC.
                if self.dc_type == DcType::Regular
                    && self.shifted_dc_id == bare_dc_id(self.shifted_dc_id)
                {
                    events.push(SessionEvent::Update(UpdatesMsg {
                        dc_id: bare_dc_id(self.shifted_dc_id),
                        outer_msg_id: info.outer_msg_id,
                        updates: None,
                        body: body.to_vec(),
                    }));
                }
                SessionAction::Continue
            }

            mtp_types::Pong::CONSTRUCTOR_ID => {
                let Ok(mtp_enums::Pong::Pong(pong)) = mtp_enums::Pong::from_bytes(body) else {
                    return SessionAction::RestartConnection;
                };
                let pong_msg_id = pong.msg_id as u64;
                let pong_ping_id = pong.ping_id as u64;
                if self.was_sent(pong_msg_id).is_none() {
                    return SessionAction::Continue;
                }
                if self.pending_ping_id == Some(pong_ping_id) {
                    self.pending_ping_id = None;
                }
                if self.ping_msg_id == Some(pong_msg_id) {
                    self.ping_msg_id = None;
                }
                if info.bad_time {
                    if self.requests_fix_time_salt(&[pong_msg_id], info) {
                        info.bad_time = false;
                    } else {
                        return SessionAction::Continue;
                    }
                }
                self.requests_acked(&[pong_msg_id], true);
                if !self.connected_state {
                    self.connected_state = true;
                    self.resend_all();
                }
                events.push(SessionEvent::Pong(pong));
                SessionAction::Continue
            }

            _ => {
                if info.bad_time {
                    return SessionAction::ResetSession;
                }
                // Match `Session::tryToReceive` (`mtproto/session.cpp:573-580`):
                // only unshifted regular sessions forward server-pushed updates.
                if self.dc_type == DcType::Regular
                    && self.shifted_dc_id == bare_dc_id(self.shifted_dc_id)
                {
                    let updates = tl_enums::Updates::from_bytes(body).ok().map(Box::new);
                    events.push(SessionEvent::Update(UpdatesMsg {
                        dc_id: bare_dc_id(self.shifted_dc_id),
                        outer_msg_id: info.outer_msg_id,
                        updates,
                        body: body.to_vec(),
                    }));
                } else {
                    trace!(
                        dc_type = ?self.dc_type,
                        shifted_dc_id = self.shifted_dc_id,
                        "ignoring update on non-regular or shifted DC session"
                    );
                }
                SessionAction::Continue
            }
        }
    }

    fn handle_bind_response(
        &mut self,
        response_body: &[u8],
        events: &mut Vec<SessionEvent>,
    ) -> SessionAction {
        self.bind_msg_id = None;
        self.bind_sent_at = None;
        match handle_bind_temp_key_response(response_body) {
            DcKeyBindState::Success => {
                if let Some((perm, temp, creating)) = self.bind_pending.take() {
                    if !self
                        .dcenter
                        .release_key_creation_on_done(creating, temp, Some(perm))
                    {
                        return SessionAction::DestroyTemporaryKey;
                    }
                    events.push(SessionEvent::TempKeyBound);
                }
                SessionAction::FlushOutbound
            }
            DcKeyBindState::DefinitelyDestroyed => {
                if let Some((perm, _, _)) = &self.bind_pending
                    && perm.created_at.elapsed() >= KEY_OLD_ENOUGH_FOR_DESTROY
                {
                    let key_id = perm.key_id();
                    events.push(SessionEvent::PersistentKeyForgotten(key_id));
                    return SessionAction::DestroyTemporaryKey;
                }
                SessionAction::FlushOutbound
            }
            DcKeyBindState::Failed => SessionAction::FlushOutbound,
        }
    }

    fn was_sent(&self, msg_id: u64) -> Option<SentKind> {
        if self.ping_msg_id == Some(msg_id)
            || self.bind_msg_id == Some(msg_id)
            || self.future_salts_msg_id == Some(msg_id)
            || self.sent_containers.contains_key(&msg_id)
            || self.state_and_resend_requests.contains_key(&msg_id)
        {
            return Some(SentKind::Service);
        }
        if let Some(&req_id) = self.resending_ids.get(&msg_id) {
            return Some(SentKind::User(req_id));
        }
        if let Some(&req_id) = self.acked_ids.get(&msg_id) {
            return Some(SentKind::User(req_id));
        }
        if let Some(req) = self.have_sent.get(&msg_id) {
            return Some(SentKind::User(req.request_id));
        }
        None
    }

    fn requests_fix_time_salt(&mut self, ids: &[u64], info: &OuterInfo) -> bool {
        for &id in ids {
            if self.was_sent(id).is_some() {
                if info.server_salt != 0 {
                    self.salts.set_salt(info.server_salt);
                }
                self.correct_unixtime_with_bad_local(info.server_time);
                return true;
            }
        }
        false
    }

    fn correct_unixtime_by_fast_request(&mut self, ids: &[u64], server_time: i32) {
        let now = Instant::now();
        for &id in ids {
            if let Some(req) = self.have_sent.get(&id)
                && let Some(sent_at) = req.last_sent_at
            {
                let dur = now.saturating_duration_since(sent_at);
                if dur <= self.sync_time_request_duration {
                    self.sync_time_request_duration = dur;
                    self.clock.update_server_time(server_time, false);
                    return;
                }
            }
        }
    }

    fn correct_unixtime_with_bad_local(&mut self, server_time: i32) {
        self.sync_time_request_duration = FAST_REQUEST_DURATION;
        self.clock.update_server_time(server_time, true);
    }

    /// Marks `ids` as acknowledged, unpacking container acks (`SessionPrivate::requestsAcked`).
    fn requests_acked(&mut self, ids: &[u64], by_response: bool) {
        let mut nested = Vec::new();
        for &msg_id in ids {
            if let Some(container) = self.sent_containers.remove(&msg_id) {
                nested.extend(container.messages);
                continue;
            }
            if self.state_and_resend_requests.remove(&msg_id).is_some() {
                continue;
            }
            if let Some(req) = self.have_sent.get(&msg_id) {
                // Keep RPC-response-bearing requests in have_sent until rpc_result arrives
                // (mirrors tdesktop's `!byResponse && _instance->hasCallback(requestId)`).
                if !by_response && req.expects_response {
                    continue;
                }
                let req_id = req.request_id;
                self.have_sent.remove(&msg_id);
                self.acked_ids.insert(msg_id, req_id);
                continue;
            }
            if let Some(&req_id) = self.resending_ids.get(&msg_id) {
                let expects_resp = self
                    .to_send
                    .get(&req_id)
                    .is_some_and(|r| r.expects_response);
                if !by_response && expects_resp {
                    continue;
                }
                self.resending_ids.remove(&msg_id);
                self.to_send.remove(&req_id);
                self.acked_ids.insert(msg_id, req_id);
            }
        }

        while self.acked_ids.len() > IDS_BUFFER_SIZE {
            if let Some(&oldest) = self.acked_ids.keys().next() {
                self.acked_ids.remove(&oldest);
            } else {
                break;
            }
        }

        if !nested.is_empty() {
            self.requests_acked(&nested, false);
        }
    }

    fn handle_msgs_states(&mut self, ids: &[u64], states: &[u8]) {
        if ids.is_empty() || states.len() != ids.len() {
            return;
        }
        let mut acked = Vec::new();
        for (&msg_id, &state) in ids.iter().zip(states.iter()) {
            let received = (state & 0x07) == 0x04;
            if self.have_sent.contains_key(&msg_id) {
                if received {
                    acked.push(msg_id);
                } else {
                    self.resend(msg_id);
                }
            } else if self.resending_ids.contains_key(&msg_id) && received {
                acked.push(msg_id);
            }
        }
        if !acked.is_empty() {
            self.requests_acked(&acked, false);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SentKind {
    Service,
    User(RequestId),
}

enum ServiceKind {
    Ping,
    BindTempKey,
    FutureSalts,
    StateReq(Vec<u64>),
    ResendReq(Vec<u64>),
    Ack,
    HttpWait,
}

struct ServiceOutbound {
    kind: ServiceKind,
    msg_id: Option<u64>,
    need_ack: bool,
    body: Vec<u8>,
}

/// Computes the next reconnect backoff in milliseconds (`SessionPrivate::retryByTimer`).
///
/// Sequence: `1 -> 2 -> 3 -> 1000 -> 2000 -> 4000 -> ... -> 64000`.
pub const fn next_retry_backoff_ms(current_ms: u64) -> u64 {
    if current_ms < 3 {
        current_ms + 1
    } else if current_ms == 3 {
        1_000
    } else if current_ms < MAX_RETRY_BACKOFF_MS {
        let doubled = current_ms * 2;
        if doubled > MAX_RETRY_BACKOFF_MS {
            MAX_RETRY_BACKOFF_MS
        } else {
            doubled
        }
    } else {
        MAX_RETRY_BACKOFF_MS
    }
}

// ============================================================================
// Async Session Runner & Handle (tokio)
// ============================================================================

enum SessionCommand {
    Send {
        request: QueuedRequest,
        reply_tx: oneshot::Sender<Result<Vec<u8>, InvocationError>>,
    },
    Cancel(RequestId),
    Ping {
        ping_id: u64,
        reply_tx: oneshot::Sender<Result<mtp_types::Pong, InvocationError>>,
    },
    Restart,
    Shutdown,
}

/// Async handle to one per-DC MTProto session background task (`MTP::details::Session`).
#[derive(Clone)]
pub struct SessionHandle {
    shifted_dc_id: ShiftedDcId,
    cmd_tx: mpsc::UnboundedSender<SessionCommand>,
}

impl SessionHandle {
    /// Spawns a new background session task for `shifted_dc_id`.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        shifted_dc_id: ShiftedDcId,
        config: ClientConfig,
        dc_options: DcOptions,
        clock: MtprotoClock,
        dcenter: Dcenter,
        transport_factory: Arc<dyn TransportFactory>,
        updates_tx: mpsc::UnboundedSender<UpdatesMsg>,
    ) -> Self {
        let proxy = Arc::new(RwLock::new(config.proxy.clone()));
        Self::spawn_with_options(
            shifted_dc_id,
            config,
            dc_options,
            clock,
            dcenter,
            transport_factory,
            updates_tx,
            proxy,
            false,
        )
    }

    /// Spawns a background session task sharing a dynamic proxy handle and optional
    /// `keys_destroyer_mode` (uses persistent key directly and treats `-404` as terminal).
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_options(
        shifted_dc_id: ShiftedDcId,
        config: ClientConfig,
        dc_options: DcOptions,
        clock: MtprotoClock,
        dcenter: Dcenter,
        transport_factory: Arc<dyn TransportFactory>,
        updates_tx: mpsc::UnboundedSender<UpdatesMsg>,
        proxy: Arc<RwLock<ProxyData>>,
        keys_destroyer_mode: bool,
    ) -> Self {
        Self::spawn_with_broadcast(
            shifted_dc_id,
            config,
            dc_options,
            clock,
            dcenter,
            transport_factory,
            updates_tx,
            None,
            proxy,
            keys_destroyer_mode,
        )
    }

    /// Spawns a background session task that also forwards updates to an optional broadcast channel.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spawn_with_broadcast(
        shifted_dc_id: ShiftedDcId,
        config: ClientConfig,
        dc_options: DcOptions,
        clock: MtprotoClock,
        dcenter: Dcenter,
        transport_factory: Arc<dyn TransportFactory>,
        updates_tx: mpsc::UnboundedSender<UpdatesMsg>,
        updates_broadcast_tx: Option<broadcast::Sender<UpdatesMsg>>,
        proxy: Arc<RwLock<ProxyData>>,
        keys_destroyer_mode: bool,
    ) -> Self {
        let try_ipv6 = Arc::new(AtomicBool::new(config.use_ipv6));
        Self::spawn_with_state(
            shifted_dc_id,
            config,
            dc_options,
            clock,
            dcenter,
            transport_factory,
            updates_tx,
            updates_broadcast_tx,
            proxy,
            try_ipv6,
            None,
            keys_destroyer_mode,
        )
    }

    /// Spawns a background session task with dynamic `proxy`, dynamic `try_ipv6`, and optional
    /// connection state reporting.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spawn_with_state(
        shifted_dc_id: ShiftedDcId,
        config: ClientConfig,
        dc_options: DcOptions,
        clock: MtprotoClock,
        dcenter: Dcenter,
        transport_factory: Arc<dyn TransportFactory>,
        updates_tx: mpsc::UnboundedSender<UpdatesMsg>,
        updates_broadcast_tx: Option<broadcast::Sender<UpdatesMsg>>,
        proxy: Arc<RwLock<ProxyData>>,
        try_ipv6: Arc<AtomicBool>,
        state_watch: Option<(
            Arc<RwLock<ConnectionState>>,
            broadcast::Sender<ConnectionState>,
        )>,
        keys_destroyer_mode: bool,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let runner = SessionRunner {
            shifted_dc_id,
            config,
            dc_options,
            clock,
            dcenter,
            transport_factory,
            updates_tx,
            updates_broadcast_tx,
            proxy,
            try_ipv6,
            state_watch,
            keys_destroyer_mode,
            cmd_rx,
            waiters: HashMap::new(),
            ping_waiters: HashMap::new(),
            retry_ms: 1,
        };
        tokio::spawn(runner.run());
        Self {
            shifted_dc_id,
            cmd_tx,
        }
    }

    /// Shifted DC ID owned by this session.
    pub const fn shifted_dc_id(&self) -> ShiftedDcId {
        self.shifted_dc_id
    }

    /// Sends a serialized TL request on this session and awaits the raw response body (or RPC error).
    pub async fn send_raw(&self, request: QueuedRequest) -> Result<Vec<u8>, InvocationError> {
        let req_id = request.request_id;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::Send { request, reply_tx })
            .map_err(|_| InvocationError::Dropped)?;

        let guard = CancelOnDrop {
            request_id: req_id,
            cmd_tx: self.cmd_tx.clone(),
            armed: true,
        };
        let res = reply_rx.await.map_err(|_| InvocationError::Dropped)?;
        std::mem::forget(guard);
        res
    }

    /// Cancels an in-flight or queued request by ID.
    pub fn cancel(&self, request_id: RequestId) {
        let _ = self.cmd_tx.send(SessionCommand::Cancel(request_id));
    }

    /// Sends an encrypted MTProto `ping` and waits for the matching `pong`.
    pub async fn ping(&self) -> Result<mtp_types::Pong, InvocationError> {
        let mut ping_id = rand::thread_rng().next_u64();
        if ping_id == 0 {
            ping_id = 1;
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::Ping { ping_id, reply_tx })
            .map_err(|_| InvocationError::Dropped)?;
        reply_rx.await.map_err(|_| InvocationError::Dropped)?
    }

    /// Forces the session to drop its current connection and reconnect immediately.
    pub fn restart(&self) {
        let _ = self.cmd_tx.send(SessionCommand::Restart);
    }

    /// Stops the session task and drops all pending waiters.
    pub fn shutdown(&self) {
        let _ = self.cmd_tx.send(SessionCommand::Shutdown);
    }
}

struct CancelOnDrop {
    request_id: RequestId,
    cmd_tx: mpsc::UnboundedSender<SessionCommand>,
    armed: bool,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.cmd_tx.send(SessionCommand::Cancel(self.request_id));
        }
    }
}

struct SessionRunner {
    shifted_dc_id: ShiftedDcId,
    config: ClientConfig,
    dc_options: DcOptions,
    clock: MtprotoClock,
    dcenter: Dcenter,
    transport_factory: Arc<dyn TransportFactory>,
    updates_tx: mpsc::UnboundedSender<UpdatesMsg>,
    updates_broadcast_tx: Option<broadcast::Sender<UpdatesMsg>>,
    proxy: Arc<RwLock<ProxyData>>,
    try_ipv6: Arc<AtomicBool>,
    state_watch: Option<(
        Arc<RwLock<ConnectionState>>,
        broadcast::Sender<ConnectionState>,
    )>,
    keys_destroyer_mode: bool,
    cmd_rx: mpsc::UnboundedReceiver<SessionCommand>,
    waiters: HashMap<RequestId, oneshot::Sender<Result<Vec<u8>, InvocationError>>>,
    ping_waiters: HashMap<u64, oneshot::Sender<Result<mtp_types::Pong, InvocationError>>>,
    retry_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriveExit {
    Shutdown,
    Restart,
    Disconnected,
}

impl SessionRunner {
    fn set_connection_state(&self, next: ConnectionState) {
        if let Some((slot, tx)) = &self.state_watch {
            let changed = {
                let mut guard = slot.write();
                if *guard != next {
                    *guard = next;
                    true
                } else {
                    false
                }
            };
            if changed {
                let _ = tx.send(next);
            }
        }
    }

    async fn run(mut self) {
        let dc_type = self.dc_options.dc_type(self.shifted_dc_id);
        let mut state = SessionState::new(
            self.shifted_dc_id,
            dc_type,
            self.config.clone(),
            self.clock.clone(),
            self.dcenter.clone(),
            0,
        );
        let mut last_key_id: Option<u64> = None;

        loop {
            // Drain any immediately queued commands before connecting (including Shutdown).
            while let Ok(cmd) = self.cmd_rx.try_recv() {
                if self.apply_command(cmd, &mut state) {
                    self.set_connection_state(ConnectionState::Disconnected);
                    self.fail_all_waiters(InvocationError::Dropped);
                    return;
                }
            }

            let bare = bare_dc_id(self.shifted_dc_id);
            let proxy = self.proxy.read().clone();
            self.config.proxy = proxy.clone();
            self.config.use_ipv6 = self.try_ipv6.load(Ordering::Acquire);
            state.set_proxy(proxy.clone());
            self.set_connection_state(ConnectionState::Connecting);
            let proto_dc =
                protocol_dc_id(self.shifted_dc_id, dc_type, self.dc_options.is_test_mode());

            enum ConnectOutcome {
                Connected(Box<dyn PacketConnection>),
                Failed(TransportError),
                Restart,
                Shutdown,
            }

            let connect_outcome = {
                let factory = Arc::clone(&self.transport_factory);
                let dc_options = self.dc_options.clone();
                let connect_cfg = self.config.clone();
                let mut connect_fut =
                    factory.connect_dc(bare, dc_type, &dc_options, &connect_cfg, &proxy);
                loop {
                    tokio::select! {
                        res = &mut connect_fut => {
                            break match res {
                                Ok(c) => ConnectOutcome::Connected(c),
                                Err(err) => ConnectOutcome::Failed(err),
                            };
                        }
                        cmd = self.cmd_rx.recv() => {
                            match cmd {
                                None | Some(SessionCommand::Shutdown) => break ConnectOutcome::Shutdown,
                                Some(SessionCommand::Restart) => {
                                    self.retry_ms = 1;
                                    break ConnectOutcome::Restart;
                                }
                                Some(other) => {
                                    if self.apply_command(other, &mut state) {
                                        break ConnectOutcome::Shutdown;
                                    }
                                }
                            }
                        }
                    }
                }
            };

            let mut conn = match connect_outcome {
                ConnectOutcome::Connected(c) => c,
                ConnectOutcome::Restart => {
                    self.set_connection_state(ConnectionState::Connecting);
                    continue;
                }
                ConnectOutcome::Shutdown => {
                    self.set_connection_state(ConnectionState::Disconnected);
                    self.fail_all_waiters(InvocationError::Dropped);
                    return;
                }
                ConnectOutcome::Failed(err) => {
                    debug!(
                        dc = self.shifted_dc_id,
                        error = %err,
                        retry_ms = self.retry_ms,
                        "transport connect failed"
                    );
                    let wait = Duration::from_millis(self.retry_ms);
                    self.set_connection_state(ConnectionState::Waiting {
                        retry_after_ms: self.retry_ms,
                    });
                    self.retry_ms = next_retry_backoff_ms(self.retry_ms);
                    if self.sleep_or_commands(wait, &mut state).await {
                        self.set_connection_state(ConnectionState::Disconnected);
                        self.fail_all_waiters(InvocationError::Dropped);
                        return;
                    }
                    continue;
                }
            };

            state.set_using_http_wait(conn.using_http_wait());

            // Ensure auth key (persistent + optional PFS temporary key) exists for this DC.
            let active_key = match self
                .ensure_auth_key(conn.as_mut(), dc_type, proto_dc, &mut state)
                .await
            {
                Ok(key) => key,
                Err(err) => {
                    warn!(dc = self.shifted_dc_id, error = %err, "auth key setup failed");
                    conn.close().await;
                    let wait = Duration::from_millis(self.retry_ms);
                    self.set_connection_state(ConnectionState::Waiting {
                        retry_after_ms: self.retry_ms,
                    });
                    self.retry_ms = next_retry_backoff_ms(self.retry_ms);
                    if self.sleep_or_commands(wait, &mut state).await {
                        self.set_connection_state(ConnectionState::Disconnected);
                        self.fail_all_waiters(InvocationError::Dropped);
                        return;
                    }
                    continue;
                }
            };

            // Match `SessionPrivate::setCurrentKeyId` (`mtproto/session_private.cpp:2482`):
            // switching to a different auth key (e.g. after `-404`, `AUTH_KEY_PERM_EMPTY`, or
            // `AUTH_KEY_DUPLICATED`) must rotate `session_id` and reset `messages_counter`.
            let current_key_id = active_key.key_id_u64();
            if last_key_id.is_some_and(|prev| prev != current_key_id) {
                state.reset_session();
            }
            last_key_id = Some(current_key_id);

            state.on_transport_connected();
            self.set_connection_state(ConnectionState::Connected);
            if state.salts_mut().needs_refresh(self.clock.now_secs())
                && !state.is_binding_temp_key()
            {
                state.schedule_future_salts();
            }

            let exit = self
                .drive_connection(conn.as_mut(), &active_key, &mut state)
                .await;
            conn.close().await;
            match exit {
                DriveExit::Shutdown => {
                    self.set_connection_state(ConnectionState::Disconnected);
                    self.fail_all_waiters(InvocationError::Dropped);
                    return;
                }
                DriveExit::Restart => {
                    self.retry_ms = 1;
                    self.set_connection_state(ConnectionState::Connecting);
                    continue;
                }
                DriveExit::Disconnected => {}
            }

            let wait = Duration::from_millis(self.retry_ms);
            self.set_connection_state(ConnectionState::Waiting {
                retry_after_ms: self.retry_ms,
            });
            self.retry_ms = next_retry_backoff_ms(self.retry_ms);
            if self.sleep_or_commands(wait, &mut state).await {
                self.set_connection_state(ConnectionState::Disconnected);
                self.fail_all_waiters(InvocationError::Dropped);
                return;
            }
        }
    }

    async fn ensure_auth_key(
        &mut self,
        conn: &mut dyn PacketConnection,
        dc_type: DcType,
        proto_dc: i16,
        state: &mut SessionState,
    ) -> Result<AuthKey, InvocationError> {
        let bare = bare_dc_id(self.shifted_dc_id);
        let temp_slot = TemporaryKeyType::from_dc_type(dc_type);

        // KeysDestroyer always operates directly on the persistent key without PFS.
        if self.keys_destroyer_mode {
            return self.dcenter.persistent_key().map(|k| k.key).ok_or_else(|| {
                InvocationError::Protocol("missing persistent key in KeysDestroyer".to_string())
            });
        }

        // CDN DCs always use unbound temporary auth keys (never persistent keys or
        // `auth.bindTempAuthKey`), even when PFS is disabled on regular DCs
        // (`Dcenter::acquireKeyCreation` in `mtproto/details/mtproto_dcenter.cpp:126` and
        // `SessionPrivate::tryAcquireKeyCreation` in `mtproto/session_private.cpp:2608-2627`).
        if !self.config.use_pfs && dc_type != DcType::Cdn {
            if let Some(perm) = self.dcenter.persistent_key() {
                return Ok(perm.key);
            }
            // Persistent keys always belong to the Regular DC ID, never a negative MediaCluster ID
            // (`SessionPrivate::tryAcquireKeyCreation` in `mtproto/session_private.cpp:2642-2644`).
            let perm_proto_dc = if dc_type == DcType::MediaCluster {
                protocol_dc_id(
                    self.shifted_dc_id,
                    DcType::Regular,
                    self.dc_options.is_test_mode(),
                )
            } else {
                proto_dc
            };
            let res = create_dc_keys(
                conn,
                bare,
                perm_proto_dc,
                &self.dc_options,
                &self.clock,
                DcKeyRequest {
                    persistent_needed: true,
                    temporary_expires_in: 0,
                },
            )
            .await?;
            let perm = res.persistent_key.ok_or_else(|| {
                InvocationError::Protocol("missing persistent key after DH".to_string())
            })?;
            state.salts_mut().set_salt(res.persistent_server_salt);
            let raw_key = perm.key.clone();
            self.dcenter.set_persistent_key(Some(perm));
            return Ok(raw_key);
        }

        // PFS / CDN mode: use existing temporary key if present.
        if let Some(temp) = self.dcenter.temporary_key(temp_slot) {
            return Ok(temp.key);
        }

        // This session already created a temporary key and holds the creation lock while the
        // bind is pending; after a reconnect, bind that key again instead of waiting on itself.
        if let Some(temp) = state.resume_pending_bind() {
            return Ok(temp);
        }

        let creating = self.dcenter.acquire_key_creation(dc_type);
        if creating == CreatingKeyType::None {
            // Another session on the same DC is creating the key; wait briefly and retry.
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(temp) = self.dcenter.temporary_key(temp_slot) {
                return Ok(temp.key);
            }
            return Err(InvocationError::Protocol(
                "waiting for concurrent key creation".to_string(),
            ));
        }

        // Match `SessionPrivate::tryAcquireKeyCreation` (`mtproto/session_private.cpp:2642-2644`):
        // if a MediaCluster session had to acquire Persistent or TemporaryRegular creation
        // (because no Regular key exists yet), negotiate DH with the Regular protocol DC ID
        // rather than the negative MediaCluster protocol DC ID.
        let key_dc_type = if dc_type == DcType::MediaCluster
            && creating != CreatingKeyType::TemporaryMediaCluster
        {
            DcType::Regular
        } else {
            dc_type
        };
        let key_proto_dc = protocol_dc_id(
            self.shifted_dc_id,
            key_dc_type,
            self.dc_options.is_test_mode(),
        );

        let req = DcKeyRequest {
            persistent_needed: creating == CreatingKeyType::Persistent,
            temporary_expires_in: TEMPORARY_EXPIRES_IN_SECS,
        };
        let res = match create_dc_keys(conn, bare, key_proto_dc, &self.dc_options, &self.clock, req)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                self.dcenter.release_key_creation_on_fail(creating);
                return Err(e);
            }
        };

        let temp_key = match res.temporary_key {
            Some(k) => k,
            None => {
                self.dcenter.release_key_creation_on_fail(creating);
                return Err(InvocationError::Protocol(
                    "missing temporary key after PFS DH".to_string(),
                ));
            }
        };
        state.salts_mut().set_salt(res.temporary_server_salt);
        let raw_key = temp_key.key.clone();

        if dc_type == DcType::Cdn {
            if !self
                .dcenter
                .release_key_creation_on_done(creating, temp_key, None)
            {
                return Err(InvocationError::Protocol(
                    "failed to store CDN temporary key".to_string(),
                ));
            }
            return Ok(raw_key);
        }

        let perm_key = match res.persistent_key.or_else(|| self.dcenter.persistent_key()) {
            Some(k) => k,
            None => {
                self.dcenter.release_key_creation_on_fail(creating);
                return Err(InvocationError::Protocol(
                    "missing persistent key for PFS bind".to_string(),
                ));
            }
        };
        state.set_pending_temp_key_bind(perm_key, temp_key, creating);
        Ok(raw_key)
    }

    /// Drives an established encrypted connection until disconnect, restart, or shutdown.
    async fn drive_connection(
        &mut self,
        conn: &mut dyn PacketConnection,
        auth_key: &AuthKey,
        state: &mut SessionState,
    ) -> DriveExit {
        if self.flush_outbound(conn, auth_key, state).await.is_err() {
            return DriveExit::Disconnected;
        }
        if conn.need_http_wait() && state.is_connected_state() {
            state.request_http_wait();
            if self.flush_outbound(conn, auth_key, state).await.is_err() {
                return DriveExit::Disconnected;
            }
        }

        let mut ping_interval = tokio::time::interval(self.config.ping_interval);
        ping_interval.tick().await;
        let mut ack_interval = tokio::time::interval(ACK_SEND_WAITING);
        ack_interval.tick().await;
        let mut check_sent_interval = tokio::time::interval(CHECK_SENT_REQUEST_TIMEOUT);
        check_sent_interval.tick().await;
        let mut container_interval = tokio::time::interval(SENT_CONTAINER_LIVES);
        container_interval.tick().await;

        loop {
            tokio::select! {
                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        None | Some(SessionCommand::Shutdown) => return DriveExit::Shutdown,
                        Some(SessionCommand::Restart) => {
                            self.retry_ms = 1;
                            return DriveExit::Restart;
                        }
                        Some(other) => {
                            let _ = self.apply_command(other, state);
                            if self.flush_outbound(conn, auth_key, state).await.is_err() {
                                return DriveExit::Disconnected;
                            }
                        }
                    }
                }
                pkt = conn.recv() => {
                    match pkt {
                        Ok(raw) => {
                            if raw.is_empty() {
                                // Empty HTTP 200 OK body after http_wait timeout; re-arm long-poll.
                                if conn.need_http_wait() && state.is_connected_state() {
                                    state.request_http_wait();
                                    if self.flush_outbound(conn, auth_key, state).await.is_err() {
                                        return DriveExit::Disconnected;
                                    }
                                }
                                continue;
                            }
                            let envelope = match parse_encrypted_packet(&raw, auth_key, Side::Server) {
                                Ok(env) => env,
                                Err(err) => {
                                    warn!(dc = self.shifted_dc_id, error = %err, "failed to decrypt server packet");
                                    return DriveExit::Disconnected;
                                }
                            };
                            self.retry_ms = 1;
                            let outcome = state.handle_decrypted(envelope);
                            self.dispatch_events(outcome.events);
                            match outcome.action {
                                Some(SessionAction::RestartConnection) => return DriveExit::Disconnected,
                                Some(SessionAction::ResetSession) => {
                                    state.reset_session();
                                    return DriveExit::Disconnected;
                                }
                                Some(SessionAction::DestroyTemporaryKey) => {
                                    // Match `SessionPrivate::destroyTemporaryKey`
                                    // (`mtproto/session_private.cpp:2703`): release any pending
                                    // key-creation lock, clear the key, and rotate session state.
                                    if let Some(creating) = state.abort_pending_bind() {
                                        self.dcenter.release_key_creation_on_fail(creating);
                                    }
                                    self.dcenter.destroy_temporary_key(auth_key.key_id_u64());
                                    state.reset_session();
                                    return DriveExit::Disconnected;
                                }
                                Some(SessionAction::FlushOutbound) => {
                                    if self.flush_outbound(conn, auth_key, state).await.is_err() {
                                        return DriveExit::Disconnected;
                                    }
                                }
                                Some(SessionAction::Continue) | None => {}
                            }
                            if conn.need_http_wait() && state.is_connected_state() {
                                state.request_http_wait();
                                if self.flush_outbound(conn, auth_key, state).await.is_err() {
                                    return DriveExit::Disconnected;
                                }
                            }
                        }
                        Err(TransportError::ServerError(TransportErrorCode::AuthKeyNotFound)) => {
                            if self.keys_destroyer_mode {
                                debug!(dc = self.shifted_dc_id, "received -404 in KeysDestroyer mode: key already gone");
                                for (_, tx) in self.waiters.drain() {
                                    let _ = tx.send(Err(InvocationError::Transport(
                                        TransportError::ServerError(TransportErrorCode::AuthKeyNotFound),
                                    )));
                                }
                                return DriveExit::Shutdown;
                            }
                            warn!(dc = self.shifted_dc_id, "received -404 transport error, destroying rejected key");
                            // Match `SessionPrivate::destroyTemporaryKey` & `setCurrentKeyId`
                            // (`mtproto/session_private.cpp:2482,2703`):
                            // 1. Release any in-flight bind creation lock so reconnect isn't stuck.
                            if let Some(creating) = state.abort_pending_bind() {
                                self.dcenter.release_key_creation_on_fail(creating);
                            }
                            // 2. Destroy the rejected key (temporary key under PFS, or persistent
                            //    key when PFS is disabled so DH runs instead of looping on -404).
                            let key_id = auth_key.key_id_u64();
                            if !self.dcenter.destroy_temporary_key(key_id) && !self.config.use_pfs {
                                self.dcenter.destroy_confirmed_forgotten_key(key_id);
                            }
                            // 3. Reset session_id / seq_no / have_sent for the replacement key.
                            state.reset_session();
                            return DriveExit::Disconnected;
                        }
                        Err(err) => {
                            debug!(dc = self.shifted_dc_id, error = %err, "connection receive error");
                            return DriveExit::Disconnected;
                        }
                    }
                }
                _ = ping_interval.tick() => {
                    let mut pid = rand::thread_rng().next_u64();
                    if pid == 0 { pid = 1; }
                    state.schedule_ping(pid);
                    if self.flush_outbound(conn, auth_key, state).await.is_err() {
                        return DriveExit::Disconnected;
                    }
                }
                _ = ack_interval.tick() => {
                    if state.pending_ack_len() > 0
                        && self.flush_outbound(conn, auth_key, state).await.is_err()
                    {
                        return DriveExit::Disconnected;
                    }
                }
                _ = check_sent_interval.tick() => {
                    match state.check_sent_requests(Instant::now()) {
                        CheckSentOutcome::QueuedStateRequests => {
                            if self.flush_outbound(conn, auth_key, state).await.is_err() {
                                return DriveExit::Disconnected;
                            }
                        }
                        CheckSentOutcome::NothingDue => {}
                        CheckSentOutcome::BindTimedOut => return DriveExit::Disconnected,
                    }
                }
                _ = container_interval.tick() => {
                    if state.clear_old_containers(Instant::now())
                        && self.flush_outbound(conn, auth_key, state).await.is_err()
                    {
                        return DriveExit::Disconnected;
                    }
                }
            }
        }
    }

    async fn flush_outbound(
        &mut self,
        conn: &mut dyn PacketConnection,
        auth_key: &AuthKey,
        state: &mut SessionState,
    ) -> Result<(), TransportError> {
        while let Some(out) = state.build_next_outbound() {
            let wire = build_encrypted_packet(
                out.salt,
                out.session_id,
                out.msg_id,
                out.seq_no,
                &out.body,
                auth_key,
                Side::Client,
            );
            conn.send(&wire).await?;
            if !out.has_more {
                break;
            }
        }
        Ok(())
    }

    fn apply_command(&mut self, cmd: SessionCommand, state: &mut SessionState) -> bool {
        match cmd {
            SessionCommand::Send { request, reply_tx } => {
                self.waiters.insert(request.request_id, reply_tx);
                state.enqueue_request(request);
                false
            }
            SessionCommand::Cancel(req_id) => {
                state.cancel_request(req_id);
                if let Some(tx) = self.waiters.remove(&req_id) {
                    let _ = tx.send(Err(InvocationError::Cancelled));
                }
                false
            }
            SessionCommand::Ping { ping_id, reply_tx } => {
                self.ping_waiters.insert(ping_id, reply_tx);
                state.schedule_ping(ping_id);
                false
            }
            SessionCommand::Restart => false,
            SessionCommand::Shutdown => true,
        }
    }

    fn dispatch_events(&mut self, events: Vec<SessionEvent>) {
        for event in events {
            match event {
                SessionEvent::RpcResult {
                    request_id, body, ..
                } => {
                    if let Some(tx) = self.waiters.remove(&request_id) {
                        let _ = tx.send(Ok(body));
                    }
                }
                SessionEvent::Update(msg) => {
                    if let Some(btx) = &self.updates_broadcast_tx {
                        let _ = btx.send(msg.clone());
                    }
                    let _ = self.updates_tx.send(msg);
                }
                SessionEvent::Pong(pong) => {
                    if let Some(tx) = self.ping_waiters.remove(&(pong.ping_id as u64)) {
                        let _ = tx.send(Ok(pong));
                    }
                }
                SessionEvent::TempKeyBound => {
                    debug!(dc = self.shifted_dc_id, "PFS temporary auth key bound");
                }
                SessionEvent::PersistentKeyForgotten(key_id) => {
                    warn!(
                        dc = self.shifted_dc_id,
                        key_id = format_args!("0x{key_id:016x}"),
                        "persistent key forgotten by server, clearing"
                    );
                    self.dcenter.destroy_confirmed_forgotten_key(key_id);
                }
            }
        }
    }

    async fn sleep_or_commands(&mut self, dur: Duration, state: &mut SessionState) -> bool {
        let sleep = tokio::time::sleep(dur);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => return false,
                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        None | Some(SessionCommand::Shutdown) => return true,
                        Some(SessionCommand::Restart) => {
                            self.retry_ms = 1;
                            return false;
                        }
                        Some(other) => {
                            if self.apply_command(other, state) {
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }

    fn fail_all_waiters(&mut self, err: InvocationError) {
        let msg = err.to_string();
        for (_, tx) in self.waiters.drain() {
            let _ = tx.send(Err(InvocationError::Dropped));
        }
        for (_, tx) in self.ping_waiters.drain() {
            let _ = tx.send(Err(InvocationError::Protocol(msg.clone())));
        }
    }
}
