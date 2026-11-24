//! Glue implementing [`tg_api::RpcClient`] over [`tg_mtproto::Client`].
//!
//! Port origin: `main/main_account.cpp` (`checkForUpdates`, `checkForNewSession`,
//! `_mtpUpdates`, `_mtpNewSessionCreated`) and `mtproto/sender.h`.
//!
//! Uses a [`MtprotoRpc`] newtype so `tg-session` can bridge `tg-mtproto` into `tg-api`
//! without violating Rust's orphan rules.

use std::sync::Arc;

use parking_lot::Mutex;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_crypto::AuthKey;
use tg_mtproto::{
    Client as MtprotoClient, ConnectionState, InvocationError as MtprotoInvocationError,
    InvokeOptions, RpcError as MtprotoRpcError, UpdatesMsg,
};
use tg_tl::mtproto::types as mtp_types;
use tg_tl::{Deserializable, Identifiable, RemoteCall, enums as tl_enums};
use tg_transport::ProxyData as TransportProxyData;
use tokio::runtime::Handle;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

/// Capacity of the parsed `tg_tl::enums::Updates` broadcast channel.
const UPDATES_CHANNEL_CAPACITY: usize = 1024;

/// Capacity of the `new_session_created` DC notification broadcast channel.
const NEW_SESSION_CHANNEL_CAPACITY: usize = 64;

/// Decoded push payload received from `tg-mtproto`'s [`UpdatesMsg`] stream.
#[derive(Debug, Clone, PartialEq)]
pub enum DecodedMtpPush {
    /// A Telegram `Updates` object (`Account::checkForUpdates`).
    Updates(Box<tl_enums::Updates>),
    /// An MTProto `new_session_created` server notification (`Account::checkForNewSession`).
    NewSessionCreated {
        /// Bare DC ID whose session was recreated.
        dc_id: DcId,
    },
    /// Unrecognized or unparseable push payload.
    Unknown,
}

/// Converts a [`tg_mtproto::RpcError`] into a [`tg_api::RpcError`].
#[must_use]
pub fn map_rpc_error(err: MtprotoRpcError) -> RpcError {
    RpcError {
        code: err.code,
        name: err.name,
        value: err.value,
    }
}

/// Converts a [`tg_mtproto::InvocationError`] into a [`tg_api::InvocationError`].
#[must_use]
pub fn map_invocation_error(err: MtprotoInvocationError) -> InvocationError {
    match err {
        MtprotoInvocationError::Rpc(rpc) => InvocationError::Rpc(map_rpc_error(rpc)),
        MtprotoInvocationError::Dropped => InvocationError::Dropped,
        MtprotoInvocationError::Transport(e) => InvocationError::Transport(e.to_string()),
        MtprotoInvocationError::Deserialize(e) => InvocationError::Deserialize(e.to_string()),
        MtprotoInvocationError::Cancelled => InvocationError::Cancelled,
        MtprotoInvocationError::Crypto(e) => InvocationError::Transport(format!("crypto: {e}")),
        MtprotoInvocationError::InvalidDc(dc) => {
            InvocationError::Transport(format!("invalid DC {dc}"))
        }
        MtprotoInvocationError::Protocol(msg) => {
            InvocationError::Transport(format!("protocol: {msg}"))
        }
        MtprotoInvocationError::Timeout => InvocationError::Transport("request timed out".into()),
    }
}

/// Parses a raw [`UpdatesMsg`] from `tg-mtproto` into either a Telegram `Updates` object or
/// a `new_session_created` notification (port of `Account::checkForUpdates` and
/// `Account::checkForNewSession` in `main/main_account.cpp:511-529`).
#[must_use]
pub fn decode_updates_msg(msg: &UpdatesMsg) -> DecodedMtpPush {
    if let Some(updates) = &msg.updates {
        return DecodedMtpPush::Updates(updates.clone());
    }
    if let Ok(parsed) = tl_enums::Updates::from_bytes(&msg.body) {
        return DecodedMtpPush::Updates(Box::new(parsed));
    }
    if msg.body.len() >= 4 {
        let ctor = u32::from_le_bytes([msg.body[0], msg.body[1], msg.body[2], msg.body[3]]);
        if ctor == mtp_types::NewSessionCreated::CONSTRUCTOR_ID {
            return DecodedMtpPush::NewSessionCreated { dc_id: msg.dc_id };
        }
    }
    DecodedMtpPush::Unknown
}

struct MtprotoRpcInner {
    client: MtprotoClient,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
    new_session_tx: broadcast::Sender<DcId>,
    auth_fail_tx: broadcast::Sender<RpcError>,
    bridge_task: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for MtprotoRpcInner {
    fn drop(&mut self) {
        if let Some(handle) = self.bridge_task.lock().take() {
            handle.abort();
        }
    }
}

/// Newtype adapter wrapping [`tg_mtproto::Client`] to implement [`tg_api::RpcClient`].
///
/// Cheaply cloneable (`Arc`-backed). Bridges `tg-mtproto`'s [`UpdatesMsg`] broadcast stream
/// into parsed `tg_tl::enums::Updates` and `new_session_created` notifications.
#[derive(Clone)]
pub struct MtprotoRpc {
    inner: Arc<MtprotoRpcInner>,
}

impl std::fmt::Debug for MtprotoRpc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MtprotoRpc")
            .field("main_dc_id", &self.inner.client.main_dc_id())
            .finish()
    }
}

impl MtprotoRpc {
    /// Wraps `client` in an `MtprotoRpc` adapter.
    ///
    /// If called within an active Tokio runtime, immediately starts the background updates
    /// forwarding bridge; otherwise the bridge is started lazily on [`Self::subscribe_updates`]
    /// or explicitly via [`Self::ensure_updates_bridge`].
    #[must_use]
    pub fn new(client: MtprotoClient) -> Self {
        let (updates_tx, _) = broadcast::channel(UPDATES_CHANNEL_CAPACITY);
        let (new_session_tx, _) = broadcast::channel(NEW_SESSION_CHANNEL_CAPACITY);
        let (auth_fail_tx, _) = broadcast::channel(NEW_SESSION_CHANNEL_CAPACITY);
        let rpc = Self {
            inner: Arc::new(MtprotoRpcInner {
                client,
                updates_tx,
                new_session_tx,
                auth_fail_tx,
                bridge_task: Mutex::new(None),
            }),
        };
        if let Ok(handle) = Handle::try_current() {
            rpc.ensure_updates_bridge(&handle);
        }
        rpc
    }

    /// Wraps `client` and starts the updates forwarding bridge on `handle`.
    #[must_use]
    pub fn with_runtime(client: MtprotoClient, handle: &Handle) -> Self {
        let rpc = Self::new(client);
        rpc.ensure_updates_bridge(handle);
        rpc
    }

    /// Ensures the background task forwarding `tg_mtproto::Client::updates()` into parsed
    /// `tg_tl::enums::Updates` and `new_session_created` broadcasts is running on `handle`.
    pub fn ensure_updates_bridge(&self, handle: &Handle) {
        let mut guard = self.inner.bridge_task.lock();
        if guard.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        let mut raw_rx = self.inner.client.updates();
        let mut raw_auth_fail_rx = self.inner.client.subscribe_auth_failures();
        let updates_tx = self.inner.updates_tx.clone();
        let new_session_tx = self.inner.new_session_tx.clone();
        let auth_fail_tx = self.inner.auth_fail_tx.clone();

        let task = handle.spawn(async move {
            loop {
                tokio::select! {
                    msg_res = raw_rx.recv() => match msg_res {
                        Ok(msg) => match decode_updates_msg(&msg) {
                            DecodedMtpPush::Updates(updates) => {
                                let _ = updates_tx.send(*updates);
                            }
                            DecodedMtpPush::NewSessionCreated { dc_id } => {
                                debug!(dc_id, "MTProto new_session_created received");
                                let _ = new_session_tx.send(dc_id);
                            }
                            DecodedMtpPush::Unknown => {
                                debug!(
                                    dc_id = msg.dc_id,
                                    bytes = msg.body.len(),
                                    "ignoring non-Updates push message from MTProto"
                                );
                            }
                        },
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(
                                skipped,
                                "MTProto raw updates broadcast lagged; emitting Updates::TooLong"
                            );
                            let _ = updates_tx.send(tl_enums::Updates::TooLong);
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                    auth_res = raw_auth_fail_rx.recv() => match auth_res {
                        Ok(err) => {
                            let _ = auth_fail_tx.send(map_rpc_error(err));
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                }
            }
        });
        *guard = Some(task);
    }

    /// Returns a reference to the underlying [`tg_mtproto::Client`].
    #[must_use]
    pub fn client(&self) -> &MtprotoClient {
        &self.inner.client
    }

    /// Subscribes to `new_session_created` notifications (carrying the bare `DcId`).
    #[must_use]
    pub fn subscribe_new_session(&self) -> broadcast::Receiver<DcId> {
        if let Ok(handle) = Handle::try_current() {
            self.ensure_updates_bridge(&handle);
        }
        self.inner.new_session_tx.subscribe()
    }

    /// Subscribes to fatal main-DC `401` authorization failures (`AUTH_KEY_UNREGISTERED`,
    /// `SESSION_REVOKED`, etc.) routed to logout by [`crate::SessionRuntime`]
    /// (`Main::Account::startMtp` in `main/main_account.cpp:469-473`).
    #[must_use]
    pub fn subscribe_auth_failures(&self) -> broadcast::Receiver<RpcError> {
        if let Ok(handle) = Handle::try_current() {
            self.ensure_updates_bridge(&handle);
        }
        self.inner.auth_fail_tx.subscribe()
    }

    /// Exports all persistent `(dc_id, AuthKey)` pairs currently held by the MTProto client.
    #[must_use]
    pub fn export_persistent_keys(&self) -> Vec<(DcId, AuthKey)> {
        self.inner.client.export_persistent_keys()
    }

    /// Seeds persistent `(dc_id, AuthKey)` pairs into the MTProto client.
    pub fn import_persistent_keys(&self, keys: Vec<(DcId, AuthKey)>) {
        self.inner.client.import_persistent_keys(keys);
    }

    /// Returns the currently active transport [`TransportProxyData`].
    #[must_use]
    pub fn proxy(&self) -> TransportProxyData {
        self.inner.client.proxy()
    }

    /// Updates the active proxy and reconnects all DC sessions without losing auth keys.
    pub fn set_proxy(&self, proxy: TransportProxyData) {
        self.inner.client.set_proxy(proxy);
    }

    /// Updates whether IPv6 candidates are attempted when connecting.
    pub fn set_try_ipv6(&self, enabled: bool) {
        self.inner.client.set_try_ipv6(enabled);
    }

    /// Returns the current main-DC [`ConnectionState`].
    #[must_use]
    pub fn connection_state(&self) -> ConnectionState {
        self.inner.client.connection_state()
    }

    /// Subscribes to main-DC [`ConnectionState`] transitions.
    #[must_use]
    pub fn subscribe_connection_state(&self) -> broadcast::Receiver<ConnectionState> {
        self.inner.client.subscribe_connection_state()
    }

    /// Restarts all open DC connections while preserving session state.
    pub fn restart_all_sessions(&self) {
        self.inner.client.restart_all_sessions();
    }

    /// Checks reachability and ping latency for `proxy` against this client's main DC.
    pub async fn check_proxy(
        &self,
        proxy: &TransportProxyData,
        try_ipv6: bool,
        timeout: std::time::Duration,
    ) -> Result<std::time::Duration, String> {
        crate::proxy::check_proxy(
            self.inner.client.main_dc_id(),
            self.inner.client.dc_options(),
            proxy,
            try_ipv6,
            timeout,
        )
        .await
    }
}

impl RpcClient for MtprotoRpc {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        self.inner
            .client
            .invoke(req)
            .await
            .map_err(map_invocation_error)
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        self.inner
            .client
            .invoke_in_dc(dc_id, req)
            .await
            .map_err(map_invocation_error)
    }

    async fn invoke_handle_flood<R: RemoteCall + Send + Sync>(
        &self,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        self.inner
            .client
            .invoke_with_options(
                req,
                InvokeOptions {
                    handle_flood_errors: true,
                    ..InvokeOptions::default()
                },
            )
            .await
            .map_err(map_invocation_error)
    }

    async fn invoke_in_dc_handle_flood<R: RemoteCall + Send + Sync>(
        &self,
        dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        self.inner
            .client
            .invoke_with_options(
                req,
                InvokeOptions {
                    dc_id,
                    handle_flood_errors: true,
                    ..InvokeOptions::default()
                },
            )
            .await
            .map_err(map_invocation_error)
    }

    fn main_dc_id(&self) -> DcId {
        self.inner.client.main_dc_id()
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        self.inner.client.set_main_dc_id(dc_id);
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        if let Ok(handle) = Handle::try_current() {
            self.ensure_updates_bridge(&handle);
        }
        Some(self.inner.updates_tx.subscribe())
    }

    async fn ping(&self) -> Result<(), Self::Error> {
        self.inner
            .client
            .ping(None)
            .await
            .map(|_| ())
            .map_err(map_invocation_error)
    }

    fn restart_sessions(&self) {
        self.inner.client.restart_all_sessions();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_crypto::CryptoError;
    use tg_mtproto::{ClientConfig, FakeTransportFactory};
    use tg_tl::{DeserializeError, Serializable};
    use tg_transport::Error as TransportError;

    #[test]
    fn error_mapping_covers_all_variants() {
        let rpc = MtprotoRpcError::parse(420, "FLOOD_WAIT_37: slow down");
        let mapped = map_invocation_error(MtprotoInvocationError::Rpc(rpc));
        assert_eq!(
            mapped,
            InvocationError::Rpc(RpcError {
                code: 420,
                name: "FLOOD_WAIT_37".into(),
                value: Some(37),
            })
        );
        if let InvocationError::Rpc(r) = mapped {
            assert!(r.is_flood_error());
            assert_eq!(r.flood_wait_seconds(), Some(37));
        }

        let migrate = MtprotoRpcError::parse(303, "PHONE_MIGRATE_4");
        let mapped_mig = map_invocation_error(MtprotoInvocationError::Rpc(migrate));
        if let InvocationError::Rpc(r) = mapped_mig {
            assert_eq!(r.migrate_dc(), Some(4));
        } else {
            panic!("expected Rpc error");
        }

        assert_eq!(
            map_invocation_error(MtprotoInvocationError::Dropped),
            InvocationError::Dropped
        );
        assert_eq!(
            map_invocation_error(MtprotoInvocationError::Cancelled),
            InvocationError::Cancelled
        );
        assert!(matches!(
            map_invocation_error(MtprotoInvocationError::Transport(
                TransportError::Disconnected
            )),
            InvocationError::Transport(_)
        ));
        assert!(matches!(
            map_invocation_error(MtprotoInvocationError::Deserialize(
                DeserializeError::UnexpectedEof {
                    needed: 4,
                    remaining: 0,
                }
            )),
            InvocationError::Deserialize(_)
        ));
        assert!(matches!(
            map_invocation_error(MtprotoInvocationError::Crypto(
                CryptoError::AuthKeyIdMismatch
            )),
            InvocationError::Transport(msg) if msg.contains("crypto:")
        ));
        assert!(matches!(
            map_invocation_error(MtprotoInvocationError::InvalidDc(99)),
            InvocationError::Transport(msg) if msg.contains("99")
        ));
        assert!(matches!(
            map_invocation_error(MtprotoInvocationError::Protocol("bad frame".into())),
            InvocationError::Transport(msg) if msg.contains("bad frame")
        ));
        assert!(matches!(
            map_invocation_error(MtprotoInvocationError::Timeout),
            InvocationError::Transport(msg) if msg.contains("timed out")
        ));
    }

    #[test]
    fn decode_updates_msg_parses_updates_and_new_session() {
        // 1. Pre-parsed Updates
        let too_long = UpdatesMsg {
            dc_id: 2,
            outer_msg_id: 1,
            updates: Some(Box::new(tl_enums::Updates::TooLong)),
            body: tl_enums::Updates::TooLong.to_bytes(),
        };
        assert_eq!(
            decode_updates_msg(&too_long),
            DecodedMtpPush::Updates(Box::new(tl_enums::Updates::TooLong))
        );

        // 2. Raw serialized Updates with `updates: None`
        let raw_only = UpdatesMsg {
            dc_id: 2,
            outer_msg_id: 2,
            updates: None,
            body: tl_enums::Updates::TooLong.to_bytes(),
        };
        assert_eq!(
            decode_updates_msg(&raw_only),
            DecodedMtpPush::Updates(Box::new(tl_enums::Updates::TooLong))
        );

        // 3. MTProto `new_session_created` (boxed on the wire)
        let new_session_body =
            tg_tl::mtproto::enums::NewSession::Created(mtp_types::NewSessionCreated {
                first_msg_id: 100,
                unique_id: 200,
                server_salt: 300,
            })
            .to_bytes();
        let new_session = UpdatesMsg {
            dc_id: 4,
            outer_msg_id: 3,
            updates: None,
            body: new_session_body,
        };
        assert_eq!(
            decode_updates_msg(&new_session),
            DecodedMtpPush::NewSessionCreated { dc_id: 4 }
        );

        // 4. Unknown garbage bytes
        let unknown = UpdatesMsg {
            dc_id: 2,
            outer_msg_id: 4,
            updates: None,
            body: vec![0xDE, 0xAD, 0xBE, 0xEF],
        };
        assert_eq!(decode_updates_msg(&unknown), DecodedMtpPush::Unknown);
    }

    #[tokio::test]
    async fn mtproto_rpc_forwards_dc_id_and_subscribes_updates() {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let factory = Arc::new(FakeTransportFactory::new());
            let cfg = ClientConfig {
                api_id: 12345,
                main_dc_id: 2,
                ..ClientConfig::default()
            };
            let (client, _rx) = MtprotoClient::with_transport(cfg, factory);
            let rpc = MtprotoRpc::new(client);

            assert_eq!(RpcClient::main_dc_id(&rpc), 2);
            RpcClient::set_main_dc_id(&rpc, 4);
            assert_eq!(RpcClient::main_dc_id(&rpc), 4);
            assert!(RpcClient::subscribe_updates(&rpc).is_some());
        })
        .await
        .expect("unit test timed out");
    }
}
