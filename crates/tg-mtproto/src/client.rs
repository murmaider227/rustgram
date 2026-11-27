//! Port of `mtproto/mtp_instance.cpp`, `mtproto/sender.h`, and `mtproto/facade.cpp`.
//!
//! Provides the high-level [`Client`] and [`Sender`] APIs matching `docs/ARCHITECTURE.md`:
//! - `Client::invoke` / `Client::invoke_in_dc`
//! - Updates stream (`mpsc::UnboundedReceiver<UpdatesMsg>`)
//! - Multi-DC session management (main DC, media/download/upload shifted DCs, CDN DCs)
//! - Proxy configuration (SOCKS5, HTTP CONNECT, MTProxy incl. fake-TLS) and connection racing
//! - `help.getConfig` loading & periodic scheduling + `help.getCdnConfig` + `Config` persistence
//! - Stale auth key destruction (`KeysDestroyer`) and multi-DC logout
//! - Automatic handling of `PHONE_MIGRATE_X`, `NETWORK_MIGRATE_X`, `USER_MIGRATE_X`,
//!   `FILE_MIGRATE_X`, `FLOOD_WAIT_X` / `FLOOD_PREMIUM_WAIT_X` / `SLOWMODE_WAIT_X`,
//!   5xx/negative server error retries, `CONNECTION_NOT_INITED` / `CONNECTION_LAYER_INVALID`,
//!   and guest-DC `auth.exportAuthorization` / `auth.importAuthorization` coalescing.

use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use tg_crypto::AuthKey;
use tg_crypto::rsa::PublicKey;
use tg_tl::enums as tl_enums;
use tg_tl::functions as tl_functions;
use tg_tl::mtproto::{enums as mtp_enums, types as mtp_types};
use tg_tl::types as tl_types;
use tg_tl::{Deserializable, Identifiable, RemoteCall, Serializable};
use tg_transport::ProxyData;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use crate::auth_key::{AuthKeyType, Dcenter, ManagedAuthKey, create_single_auth_key};
use crate::config::{ClientConfig, Config, ConfigFields};
use crate::config_loader::{ConfigLoader, ConfigLoaderOptions, ConfigSchedulerHandle};
use crate::dc_options::{
    DC_SHIFT, DcId, DcOptions, DcType, ShiftedDcId, bare_dc_id, download_dc_id, get_dc_id_shift,
    is_temporary_dc_id, logout_dc_id, protocol_dc_id, real_id_from_temporary_dc_id, shift_dc_id,
};
use crate::error::{InvocationError, MigrateKind, RpcError};
use crate::keys_destroyer::{KeysDestroyResult, KeysDestroyer};
use crate::message::{RequestId, next_request_id};
use crate::session::{ConnectionState, QueuedRequest, SessionHandle, UpdatesMsg};
use crate::single_flight::{Joined, SingleFlight};
use crate::special_config::{SpecialConfigError, sync_http_unixtime};
use crate::time::MtprotoClock;
use crate::transport_adapter::{TgTransportFactory, TransportFactory};

/// Broadcast channel buffer capacity for [`Client::updates`] / [`RpcClient::updates`].
const UPDATES_BROADCAST_CAPACITY: usize = 1024;

/// Async RPC client trait implemented by [`Client`], allowing higher-level crates such as
/// `tg-api` to depend on a mockable abstraction instead of a concrete transport-backed client.
pub trait RpcClient: Send + Sync {
    /// Invokes a TL [`RemoteCall`] on the main DC.
    fn invoke<R: RemoteCall + Send + Sync>(
        &self,
        req: &R,
    ) -> impl Future<Output = Result<R::Return, InvocationError>> + Send;

    /// Invokes a TL [`RemoteCall`] on a specific bare or shifted DC.
    fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        dc_id: ShiftedDcId,
        req: &R,
    ) -> impl Future<Output = Result<R::Return, InvocationError>> + Send;

    /// Invokes a TL [`RemoteCall`] with explicit [`InvokeOptions`].
    fn invoke_with_options<R: RemoteCall + Send + Sync>(
        &self,
        req: &R,
        options: InvokeOptions,
    ) -> impl Future<Output = Result<R::Return, InvocationError>> + Send;

    /// Returns a broadcast receiver subscribed to server-pushed updates across sessions.
    fn updates(&self) -> broadcast::Receiver<UpdatesMsg>;

    /// Returns the currently active main DC ID.
    fn main_dc_id(&self) -> DcId;

    /// Switches the main DC ID.
    fn set_main_dc_id(&self, new_main_dc: DcId);

    /// Exports authorization from the main DC for `dc_id` (`auth.exportAuthorization`).
    fn export_authorization(
        &self,
        dc_id: DcId,
    ) -> impl Future<Output = Result<tl_types::auth::ExportedAuthorization, InvocationError>> + Send;

    /// Imports an exported authorization into `dc_id` (`auth.importAuthorization`).
    fn import_authorization(
        &self,
        dc_id: ShiftedDcId,
        id: i64,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<tl_enums::auth::Authorization, InvocationError>> + Send;
}

/// Options controlling how an individual RPC call is executed (`Sender::RequestBuilder`).
#[derive(Debug, Clone, Copy)]
pub struct InvokeOptions {
    /// Target shifted DC ID (`0` = current main DC).
    pub dc_id: ShiftedDcId,
    /// Optional predecessor request ID for `invokeAfterMsg` ordering.
    pub after_request_id: Option<RequestId>,
    /// Whether the request needs `invokeWithLayer(initConnection(...))` on uninitialized DCs.
    pub needs_layer: bool,
    /// If `true`, return `FLOOD_WAIT` errors directly to the caller instead of sleeping & retrying.
    pub handle_flood_errors: bool,
    /// If `true`, return all RPC errors directly without automatic retry/migration/auth-import.
    pub handle_all_errors: bool,
}

impl Default for InvokeOptions {
    fn default() -> Self {
        Self {
            dc_id: 0,
            after_request_id: None,
            needs_layer: true,
            handle_flood_errors: false,
            handle_all_errors: false,
        }
    }
}

struct ClientInner {
    config: ClientConfig,
    dc_options: DcOptions,
    config_fields: RwLock<ConfigFields>,
    last_config_loaded: RwLock<Option<Instant>>,
    clock: MtprotoClock,
    proxy: Arc<RwLock<ProxyData>>,
    try_ipv6: Arc<AtomicBool>,
    connection_state: Arc<RwLock<ConnectionState>>,
    connection_state_tx: broadcast::Sender<ConnectionState>,
    transport_factory: Arc<dyn TransportFactory>,
    updates_tx: mpsc::UnboundedSender<UpdatesMsg>,
    updates_broadcast_tx: broadcast::Sender<UpdatesMsg>,
    auth_fail_tx: broadcast::Sender<RpcError>,
    non_premium_delay_tx: broadcast::Sender<u32>,

    main_dc_id: RwLock<DcId>,
    dcenters: RwLock<BTreeMap<DcId, Dcenter>>,
    sessions: RwLock<BTreeMap<ShiftedDcId, SessionHandle>>,
    auth_transfers: SingleFlight<DcId, Result<(), String>>,
    authorized_dcs: RwLock<HashSet<DcId>>,
}

/// High-level MTProto client managing sessions across Telegram DCs (`MTP::Instance`).
///
/// Cloneable handle backed by `Arc`; create via [`Client::new`] (real `tg-transport` with proxy
/// support & connection racing) or [`Client::with_transport`] (custom/fake transport).
#[derive(Clone)]
pub struct Client {
    inner: Arc<ClientInner>,
}

impl Client {
    /// Creates a new `Client` using `tg-transport` (obfuscated2 TCP/HTTP + proxies + connection
    /// racing), returning `(client, updates_receiver)`.
    pub fn new(config: ClientConfig) -> (Self, mpsc::UnboundedReceiver<UpdatesMsg>) {
        let factory = Arc::new(TgTransportFactory::from_config(&config));
        Self::with_transport(config, factory)
    }

    /// Creates a new `Client` using a custom [`TransportFactory`] (e.g. [`crate::FakeTransportFactory`]).
    pub fn with_transport(
        config: ClientConfig,
        transport_factory: Arc<dyn TransportFactory>,
    ) -> (Self, mpsc::UnboundedReceiver<UpdatesMsg>) {
        let dc_options = DcOptions::new(config.environment);
        let config_fields = ConfigFields::for_environment(config.environment);
        let main_dc_id = config.main_dc_id;
        let proxy = Arc::new(RwLock::new(config.proxy.clone()));
        let try_ipv6 = Arc::new(AtomicBool::new(config.use_ipv6));
        let connection_state = Arc::new(RwLock::new(ConnectionState::Disconnected));
        let (connection_state_tx, _) = broadcast::channel(64);
        let (updates_tx, updates_rx) = mpsc::unbounded_channel();
        let (updates_broadcast_tx, _) = broadcast::channel(UPDATES_BROADCAST_CAPACITY);
        let (auth_fail_tx, _) = broadcast::channel(64);
        let (non_premium_delay_tx, _) = broadcast::channel(64);

        let inner = Arc::new(ClientInner {
            config,
            dc_options,
            config_fields: RwLock::new(config_fields),
            last_config_loaded: RwLock::new(None),
            clock: MtprotoClock::new(),
            proxy,
            try_ipv6,
            connection_state,
            connection_state_tx,
            transport_factory,
            updates_tx,
            updates_broadcast_tx,
            auth_fail_tx,
            non_premium_delay_tx,
            main_dc_id: RwLock::new(main_dc_id),
            dcenters: RwLock::new(BTreeMap::new()),
            sessions: RwLock::new(BTreeMap::new()),
            auth_transfers: SingleFlight::new(),
            authorized_dcs: RwLock::new(HashSet::new()),
        });
        (Self { inner }, updates_rx)
    }

    /// Subscribes to server-pushed updates across all active sessions.
    pub fn updates(&self) -> broadcast::Receiver<UpdatesMsg> {
        self.inner.updates_broadcast_tx.subscribe()
    }

    /// Subscribes to fatal main-DC `401` authorization failures (`AUTH_KEY_UNREGISTERED`,
    /// `SESSION_REVOKED`, etc., excluding `AUTH_KEY_PERM_EMPTY`), porting
    /// `MTP::Instance::setGlobalFailHandler` (`mtproto/mtp_instance.cpp`) and
    /// `Main::Account::startMtp` (`main/main_account.cpp`).
    pub fn subscribe_auth_failures(&self) -> broadcast::Receiver<RpcError> {
        self.inner.auth_fail_tx.subscribe()
    }

    /// Subscribes to `FLOOD_PREMIUM_WAIT_<secs>` notifications (`_nonPremiumDelayedRequests`
    /// in `mtproto/mtp_instance.cpp`).
    pub fn subscribe_non_premium_delays(&self) -> broadcast::Receiver<u32> {
        self.inner.non_premium_delay_tx.subscribe()
    }

    /// Marks `dc_id` as requiring `invokeWithLayer(initConnection(...))` on the next request
    /// (`MTP::Instance::reInitConnection` in `mtproto/mtp_instance.cpp`).
    pub fn re_init_connection(&self, dc_id: DcId) {
        let bare = bare_dc_id(dc_id);
        if let Some(dc) = self.inner.dcenters.read().get(&bare) {
            dc.set_connection_inited(false);
        }
    }

    /// Returns a [`Sender`] scoped to this client.
    pub fn sender(&self) -> Sender {
        Sender::new(self.clone())
    }

    /// Returns the shared [`DcOptions`] registry.
    pub fn dc_options(&self) -> &DcOptions {
        &self.inner.dc_options
    }

    /// Returns the shared [`MtprotoClock`].
    pub fn clock(&self) -> &MtprotoClock {
        &self.inner.clock
    }

    /// Returns the currently active [`ProxyData`].
    pub fn proxy(&self) -> ProxyData {
        self.inner.proxy.read().clone()
    }

    /// Updates the active [`ProxyData`] and restarts all open sessions so they reconnect
    /// through the new proxy (or directly if [`ProxyData::none`]) without losing auth keys
    /// or session state.
    pub fn set_proxy(&self, proxy: ProxyData) {
        *self.inner.proxy.write() = proxy;
        // Match `Main::Account::startMtp` (`main/main_account.cpp:108-110`) and
        // `MTP::Instance::reInitConnection` (`mtproto/mtp_instance.cpp`): re-send
        // `initConnection` on reconnect so the updated proxy metadata is reported.
        for dc in self.inner.dcenters.read().values() {
            dc.set_connection_inited(false);
        }
        self.restart_all_sessions();
    }

    /// Returns whether IPv6 candidates are currently enabled for connection racing.
    pub fn try_ipv6(&self) -> bool {
        self.inner.try_ipv6.load(Ordering::Acquire)
    }

    /// Updates whether IPv6 connection candidates are enabled and restarts active sessions
    /// if the setting changed.
    pub fn set_try_ipv6(&self, enabled: bool) {
        let prev = self.inner.try_ipv6.swap(enabled, Ordering::AcqRel);
        if prev != enabled {
            self.restart_all_sessions();
        }
    }

    /// Returns the current main-DC [`ConnectionState`].
    pub fn connection_state(&self) -> ConnectionState {
        *self.inner.connection_state.read()
    }

    /// Subscribes to main-DC [`ConnectionState`] transitions.
    pub fn subscribe_connection_state(&self) -> broadcast::Receiver<ConnectionState> {
        self.inner.connection_state_tx.subscribe()
    }

    /// Returns the base [`ClientConfig`] used to construct this client.
    pub fn client_config(&self) -> &ClientConfig {
        &self.inner.config
    }

    /// Returns the configured [`TransportFactory`].
    pub fn transport_factory(&self) -> &Arc<dyn TransportFactory> {
        &self.inner.transport_factory
    }

    /// Returns the configured phone prefix hint used for `help.configSimple` rule filtering.
    pub fn phone_prefix_hint(&self) -> String {
        self.inner.config.phone_prefix_hint.clone()
    }

    /// Returns a snapshot of the server [`ConfigFields`].
    pub fn config_fields(&self) -> ConfigFields {
        self.inner.config_fields.read().clone()
    }

    /// Updates the stored [`ConfigFields`].
    pub fn set_config_fields(&self, fields: ConfigFields) {
        *self.inner.config_fields.write() = fields;
    }

    /// Returns a full [`Config`] snapshot (both `DcOptions` and `ConfigFields`).
    pub fn config_snapshot(&self) -> Config {
        Config::from_parts(self.inner.dc_options.deep_clone(), self.config_fields())
    }

    /// Serializes this client's `Config` (`DcOptions` + `ConfigFields`) in tdesktop's
    /// `QDataStream::Qt_5_1` byte format for `tg-storage` persistence.
    pub fn serialize_config(&self) -> Vec<u8> {
        self.config_snapshot().serialize()
    }

    /// Loads `DcOptions` and `ConfigFields` from tdesktop's serialized `Config` bytes.
    ///
    /// Returns `false` if the serialized payload is corrupt or belongs to a different environment.
    pub fn load_serialized_config(&self, serialized: &[u8]) -> bool {
        let Some(loaded) = Config::from_serialized(serialized) else {
            return false;
        };
        if loaded.environment() != self.inner.dc_options.environment() {
            return false;
        }
        let dc_bytes = loaded.dc_options().serialize();
        if !self.inner.dc_options.construct_from_serialized(&dc_bytes) {
            return false;
        }
        *self.inner.config_fields.write() = loaded.values().clone();
        true
    }

    /// Applies a `help.getConfig` [`tg_tl::types::Config`] response to both `ConfigFields` and
    /// `DcOptions` (`Instance::Private::configLoadDone`), returning `false` on `test_mode` mismatch.
    pub fn apply_tl_config(&self, tl: &tl_types::Config) -> bool {
        if tl.test_mode != self.inner.dc_options.is_test_mode() {
            return false;
        }
        let env = self.inner.dc_options.environment();
        self.inner.config_fields.write().apply_tl(tl, env);
        if !tl.dc_options.is_empty() {
            let changed = self.inner.dc_options.set_from_list(&tl.dc_options);
            for dc_id in changed {
                self.restart_dc(dc_id);
            }
        }
        *self.inner.last_config_loaded.write() = Some(Instant::now());
        true
    }

    /// Returns when `help.getConfig` last succeeded, if ever.
    pub fn last_config_loaded(&self) -> Option<Instant> {
        *self.inner.last_config_loaded.read()
    }

    /// Fetches `help.getConfig` once via [`ConfigLoader`] (main DC -> DC enum + DoH fallback)
    /// and applies the result to this client.
    pub async fn load_config_once(
        &self,
        options: ConfigLoaderOptions,
    ) -> Result<tl_types::Config, InvocationError> {
        ConfigLoader::new(self.clone(), options).load().await
    }

    /// Spawns a background periodic `help.getConfig` refresh scheduler respecting server expiry.
    pub fn start_config_scheduler(&self, options: ConfigLoaderOptions) -> ConfigSchedulerHandle {
        ConfigSchedulerHandle::spawn(self.clone(), options)
    }

    /// Fetches CDN RSA public keys via `help.getCdnConfig` and installs them in [`DcOptions`]
    /// (`Instance::Private::requestCDNConfig`).
    pub async fn request_cdn_config(&self) -> Result<usize, InvocationError> {
        let tl_enums::CdnConfig::CdnConfig(cdn_cfg) =
            self.invoke(&tl_functions::help::GetCdnConfig).await?;
        let mut parsed = Vec::with_capacity(cdn_cfg.public_keys.len());
        for entry_enum in cdn_cfg.public_keys {
            let tl_enums::CdnPublicKey::CdnPublicKey(entry) = entry_enum;
            match PublicKey::from_pem(&entry.public_key) {
                Ok(key) => parsed.push((entry.dc_id, key)),
                Err(err) => {
                    warn!(dc_id = entry.dc_id, error = %err, "failed to parse CDN RSA public key");
                }
            }
        }
        let count = parsed.len();
        self.inner.dc_options.set_cdn_keys(&parsed);
        Ok(count)
    }

    /// Synchronizes HTTP unixtime from Google/Mozilla DoH `Date` headers
    /// (`Instance::Private::syncHttpUnixtime`).
    pub async fn sync_http_unixtime(&self) -> Result<i64, SpecialConfigError> {
        let domain = self.config_fields().txt_domain_string;
        sync_http_unixtime(&domain, &self.inner.clock).await
    }

    /// Returns the current main DC ID.
    pub fn main_dc_id(&self) -> DcId {
        *self.inner.main_dc_id.read()
    }

    /// Switches the main DC ID (`Instance::setMainDcId`), shutting down the old main session.
    pub fn set_main_dc_id(&self, new_main_dc: DcId) {
        let old_main = {
            let mut guard = self.inner.main_dc_id.write();
            let prev = *guard;
            *guard = new_main_dc;
            prev
        };
        if old_main != new_main_dc {
            info!(old_main, new_main_dc, "switching main DC");
            self.kill_session(old_main);
        }
    }

    /// Imports pre-existing persistent auth keys (e.g. loaded from `tg-storage` / `tdata`).
    pub fn import_persistent_keys(&self, keys: Vec<(DcId, AuthKey)>) {
        for (dc_id, key) in keys {
            let dc = self.get_or_create_dcenter(dc_id);
            dc.set_persistent_key(Some(ManagedAuthKey::new(
                key,
                AuthKeyType::ReadFromFile,
                dc_id,
            )));
        }
    }

    /// Exports all currently known persistent auth keys `(dc_id, AuthKey)` for persistence.
    pub fn export_persistent_keys(&self) -> Vec<(DcId, AuthKey)> {
        let guard = self.inner.dcenters.read();
        guard
            .iter()
            .filter_map(|(&dc_id, dc)| dc.persistent_key().map(|k| (dc_id, k.key)))
            .collect()
    }

    /// Destroys a batch of stale persistent auth keys via [`KeysDestroyer`] (`auth.logOut` +
    /// `destroy_auth_key` on dedicated non-PFS sessions).
    pub async fn destroy_stale_keys(&self, keys: Vec<(DcId, AuthKey)>) -> KeysDestroyResult {
        let mut cfg = self.inner.config.clone();
        cfg.proxy = self.proxy();
        let destroyer = KeysDestroyer::with_transport(
            cfg,
            self.inner.dc_options.clone(),
            self.inner.transport_factory.clone(),
            keys,
        );
        destroyer.destroy_all().await
    }

    /// Logs out the main DC and all non-CDN guest DCs that hold persistent keys
    /// (`Instance::Private::logout` + `logoutGuestDcs`).
    ///
    /// Matches `mtproto/mtp_instance.cpp:745-779`: only sends `auth.logOut` to DCs that already
    /// hold a persistent auth key so logging out an unauthenticated or never-connected client
    /// never dials a DC to mint a brand-new auth key just to log it out.
    pub async fn logout(&self) {
        let main_dc = self.main_dc_id();
        let existing_key_dcs: HashSet<DcId> = self
            .export_persistent_keys()
            .into_iter()
            .map(|(dc_id, _)| dc_id)
            .collect();

        if existing_key_dcs.contains(&main_dc) {
            let _ = self
                .invoke_with_options(
                    &tl_functions::auth::LogOut,
                    InvokeOptions {
                        dc_id: 0,
                        handle_all_errors: true,
                        ..InvokeOptions::default()
                    },
                )
                .await;
        }

        let guest_dcs: Vec<DcId> = existing_key_dcs
            .into_iter()
            .filter(|&dc_id| {
                dc_id != main_dc && self.inner.dc_options.dc_type(dc_id) != DcType::Cdn
            })
            .collect();

        for dc_id in guest_dcs {
            let shifted = logout_dc_id(dc_id);
            let _ = self
                .invoke_with_options(
                    &tl_functions::auth::LogOut,
                    InvokeOptions {
                        dc_id: shifted,
                        handle_all_errors: true,
                        ..InvokeOptions::default()
                    },
                )
                .await;
            self.kill_session(shifted);
        }
    }

    /// Restarts all currently running sessions (`Instance::restart`).
    pub fn restart_all_sessions(&self) {
        let guard = self.inner.sessions.read();
        for session in guard.values() {
            session.restart();
        }
    }

    /// Restarts all sessions targeting bare `dc_id` (`Instance::restart(shiftedDcId)`).
    pub fn restart_dc(&self, shifted_dc_id: ShiftedDcId) {
        let bare = bare_dc_id(shifted_dc_id);
        let guard = self.inner.sessions.read();
        for (&id, session) in guard.iter() {
            if bare_dc_id(id) == bare {
                session.restart();
            }
        }
    }

    /// Stops and removes a specific shifted DC session (`Instance::killSession`).
    pub fn kill_session(&self, shifted_dc_id: ShiftedDcId) {
        let removed = self.inner.sessions.write().remove(&shifted_dc_id);
        if let Some(session) = removed {
            session.shutdown();
        }
    }

    /// Stops and removes all sessions matching `shift` (`Instance::killSessionsByShift`).
    pub fn kill_sessions_by_shift(&self, shift: i32) {
        let mut guard = self.inner.sessions.write();
        let to_kill: Vec<ShiftedDcId> = guard
            .keys()
            .copied()
            .filter(|&id| get_dc_id_shift(id) == shift)
            .collect();
        for id in to_kill {
            if let Some(session) = guard.remove(&id) {
                session.shutdown();
            }
        }
    }

    /// Shuts down all active sessions.
    pub fn shutdown(&self) {
        let mut guard = self.inner.sessions.write();
        for (_, session) in std::mem::take(&mut *guard) {
            session.shutdown();
        }
    }

    /// Explicitly creates a permanent auth key on `dc_id` over a fresh raced connection and
    /// stores it in the DC registry, returning `(ManagedAuthKey, initial_server_salt)`.
    pub async fn create_permanent_auth_key(
        &self,
        dc_id: DcId,
    ) -> Result<(ManagedAuthKey, u64), InvocationError> {
        let dc_type = self.inner.dc_options.dc_type(dc_id);
        let proto_dc = protocol_dc_id(dc_id, dc_type, self.inner.dc_options.is_test_mode());
        let proxy = self.proxy();
        let mut conn = self
            .inner
            .transport_factory
            .connect_dc(
                dc_id,
                dc_type,
                &self.inner.dc_options,
                &self.inner.config,
                &proxy,
            )
            .await?;
        let result = create_single_auth_key(
            conn.as_mut(),
            dc_id,
            proto_dc,
            &self.inner.dc_options,
            &self.inner.clock,
            0,
        )
        .await;
        conn.close().await;
        let (key, salt) = result?;
        let dc = self.get_or_create_dcenter(dc_id);
        dc.set_persistent_key(Some(key.clone()));
        Ok((key, salt))
    }

    /// Sends an encrypted MTProto `ping` on `dc_id` (or main DC when `None`) and awaits `pong`.
    pub async fn ping(
        &self,
        dc_id: Option<ShiftedDcId>,
    ) -> Result<mtp_types::Pong, InvocationError> {
        let target = self.resolve_shifted_dc(dc_id.unwrap_or(0));
        let session = self.get_or_start_session(target);
        session.ping().await
    }

    /// Exports authorization from the main DC for `dc_id` (`auth.exportAuthorization`).
    pub async fn export_authorization(
        &self,
        dc_id: DcId,
    ) -> Result<tl_types::auth::ExportedAuthorization, InvocationError> {
        // Match `Instance::Private::exportFail` (`mtproto/mtp_instance.cpp:1332`):
        // allow default handling of `CONNECTION_NOT_INITED`, 5xx, and `FLOOD_WAIT`, while
        // preventing recursive guest auth transfers.
        let raw = self
            .invoke_raw_internal(
                tl_functions::auth::ExportAuthorization { dc_id }.to_bytes(),
                InvokeOptions {
                    dc_id: 0,
                    ..InvokeOptions::default()
                },
                false,
            )
            .await?;
        let tl_enums::auth::ExportedAuthorization::ExportedAuthorization(exported) =
            tl_enums::auth::ExportedAuthorization::from_bytes(&raw)?;
        Ok(exported)
    }

    /// Imports an exported authorization into `dc_id` (`auth.importAuthorization`).
    pub async fn import_authorization(
        &self,
        dc_id: ShiftedDcId,
        id: i64,
        bytes: Vec<u8>,
    ) -> Result<tl_enums::auth::Authorization, InvocationError> {
        // Match `Instance::Private::importFail` (`mtproto/mtp_instance.cpp:1409`):
        // allow default handling of `CONNECTION_NOT_INITED`, 5xx, and `FLOOD_WAIT` on a freshly
        // opened guest DC connection while preventing recursive guest auth transfer loops.
        let raw = self
            .invoke_raw_internal(
                tl_functions::auth::ImportAuthorization { id, bytes }.to_bytes(),
                InvokeOptions {
                    dc_id,
                    ..InvokeOptions::default()
                },
                false,
            )
            .await?;
        let auth = tl_enums::auth::Authorization::from_bytes(&raw)?;
        self.inner.authorized_dcs.write().insert(bare_dc_id(dc_id));
        Ok(auth)
    }

    /// Ensures authorization has been copied from the main DC to `target_dc` via
    /// `auth.exportAuthorization` + `auth.importAuthorization` before issuing file requests there.
    pub async fn ensure_dc_authorized(
        &self,
        target_dc: ShiftedDcId,
    ) -> Result<(), InvocationError> {
        let resolved = self.resolve_shifted_dc(target_dc);
        let bare = bare_dc_id(resolved);
        if bare == self.main_dc_id() || self.inner.dc_options.dc_type(resolved) == DcType::Cdn {
            return Ok(());
        }
        if self.inner.authorized_dcs.read().contains(&bare) {
            return Ok(());
        }
        self.transfer_auth_to_dc(bare, resolved).await
    }

    /// Invokes a TL [`RemoteCall`] on the main DC (matches `docs/ARCHITECTURE.md` contract).
    pub async fn invoke<R: RemoteCall>(&self, req: &R) -> Result<R::Return, InvocationError> {
        self.invoke_with_options(req, InvokeOptions::default())
            .await
    }

    /// Invokes a TL [`RemoteCall`] on a specific DC / shifted DC (matches `docs/ARCHITECTURE.md`).
    pub async fn invoke_in_dc<R: RemoteCall>(
        &self,
        dc_id: ShiftedDcId,
        req: &R,
    ) -> Result<R::Return, InvocationError> {
        self.invoke_with_options(
            req,
            InvokeOptions {
                dc_id,
                ..InvokeOptions::default()
            },
        )
        .await
    }

    /// Invokes a TL [`RemoteCall`] with full [`InvokeOptions`].
    pub async fn invoke_with_options<R: RemoteCall>(
        &self,
        req: &R,
        options: InvokeOptions,
    ) -> Result<R::Return, InvocationError> {
        let raw = self.invoke_raw(req.to_bytes(), options).await?;
        let decoded = R::Return::from_bytes(&raw)?;
        Ok(decoded)
    }

    /// Invokes a pre-serialized TL request with full error-handling / migration / flood-wait /
    /// guest-DC auth transfer automation (port of `Instance::Private::onErrorDefault`).
    pub async fn invoke_raw(
        &self,
        body: Vec<u8>,
        options: InvokeOptions,
    ) -> Result<Vec<u8>, InvocationError> {
        self.invoke_raw_internal(body, options, true).await
    }

    async fn invoke_raw_internal(
        &self,
        body: Vec<u8>,
        options: InvokeOptions,
        allow_guest_auth_transfer: bool,
    ) -> Result<Vec<u8>, InvocationError> {
        let to_main_dc = options.dc_id == 0;
        let mut target_dc = self.resolve_shifted_dc(options.dc_id);
        let mut needs_layer = options.needs_layer;
        let mut after_request_id = options.after_request_id;
        let mut server_retry_secs: u64 = 1;
        let mut retried_bad_guest = false;
        let mut guest_auth_retries: u32 = 0;
        let mut migration_hops: u32 = 0;
        let mut duplicated_key_retries: u32 = 0;

        loop {
            let request_id = next_request_id();
            let mut queued = QueuedRequest::new(request_id, body.clone());
            queued.needs_layer = needs_layer;
            queued.after_request_id = after_request_id;

            let session = self.get_or_start_session(target_dc);
            let reply = session.send_raw(queued).await?;

            // Check if reply is rpc_error#2144ca19.
            if reply.len() >= 4
                && u32::from_le_bytes([reply[0], reply[1], reply[2], reply[3]])
                    == mtp_types::RpcError::CONSTRUCTOR_ID
            {
                let mtp_enums::RpcError::RpcError(tl_err) =
                    mtp_enums::RpcError::from_bytes(&reply)?;
                let rpc_err = RpcError::parse(tl_err.error_code, &tl_err.error_message);
                debug!(
                    dc = target_dc,
                    code = rpc_err.code,
                    raw_message = %tl_err.error_message,
                    "received RPC error"
                );
                if options.handle_all_errors {
                    return Err(InvocationError::Rpc(rpc_err));
                }

                // 0. 406 AUTH_KEY_DUPLICATED -> destroy keys for this DC, negotiate a new auth
                //    key via DH on reconnect, and repeat the request.
                if rpc_err.is_auth_key_duplicated() && duplicated_key_retries < 2 {
                    duplicated_key_retries += 1;
                    let bare = bare_dc_id(target_dc);
                    warn!(
                        dc = bare,
                        attempt = duplicated_key_retries,
                        "received AUTH_KEY_DUPLICATED; destroying DC keys and reconnecting for fresh DH"
                    );
                    self.get_or_create_dcenter(bare).destroy_all_keys();
                    self.inner.authorized_dcs.write().remove(&bare);
                    self.restart_dc(bare);
                    needs_layer = true;
                    continue;
                }

                // 1. *_MIGRATE_<dc>
                // Match `Instance::Private::onErrorDefault` (`mtproto/mtp_instance.cpp:1612-1627`):
                // PHONE_MIGRATE / NETWORK_MIGRATE / USER_MIGRATE switch `main_dc_id`;
                // FILE_MIGRATE must NEVER change `main_dc_id` and instead uses a per-DC connection.
                if let Some((kind, new_bare_dc)) = rpc_err.migrate_dc()
                    && new_bare_dc > 0
                    && new_bare_dc < DC_SHIFT
                    && migration_hops < 4
                {
                    migration_hops += 1;
                    debug!(
                        ?kind,
                        from = target_dc,
                        to = new_bare_dc,
                        "handling DC migration"
                    );
                    if to_main_dc && kind != MigrateKind::File {
                        self.set_main_dc_id(new_bare_dc);
                        target_dc = new_bare_dc;
                    } else if kind == MigrateKind::File && get_dc_id_shift(target_dc) == 0 {
                        target_dc = download_dc_id(new_bare_dc, 0);
                    } else {
                        target_dc = shift_dc_id(new_bare_dc, get_dc_id_shift(target_dc));
                    }
                    // Match `Instance::Private::onErrorDefault` + `sendAuthExport`/`exportDone`
                    // (`mtproto/mtp_instance.cpp:1378-1441, 1501-1566`): when `FILE_MIGRATE_X`
                    // redirects a file request to another DC, authorize that per-DC download
                    // connection via `auth.exportAuthorization` + `auth.importAuthorization`
                    // before issuing the migrated file request, without ever changing `main_dc_id`.
                    if kind == MigrateKind::File
                        && allow_guest_auth_transfer
                        && new_bare_dc != self.main_dc_id()
                        && self.inner.dc_options.dc_type(target_dc) != DcType::Cdn
                    {
                        let _ = Box::pin(self.ensure_dc_authorized(target_dc)).await;
                    }
                    continue;
                }

                // 2. MSG_WAIT_TIMEOUT / MSG_WAIT_FAILED -> drop stale dependency and resend.
                if rpc_err.is_msg_wait() && after_request_id.is_some() {
                    after_request_id = None;
                    continue;
                }

                // 3. CONNECTION_NOT_INITED / CONNECTION_LAYER_INVALID -> mark uninited and resend.
                if rpc_err.is_connection_not_inited() {
                    let bare = bare_dc_id(target_dc);
                    self.get_or_create_dcenter(bare)
                        .set_connection_inited(false);
                    needs_layer = true;
                    continue;
                }

                // 4. FLOOD_WAIT_X / FLOOD_PREMIUM_WAIT_X / SLOWMODE_WAIT (< 3s).
                if let Some(wait_secs) = rpc_err
                    .flood_wait_secs()
                    .or_else(|| rpc_err.flood_premium_wait_secs())
                    .or_else(|| rpc_err.slowmode_wait_secs().filter(|&s| s < 3))
                {
                    if options.handle_flood_errors {
                        return Err(InvocationError::Rpc(rpc_err));
                    }
                    if let Some(max_wait) = self.inner.config.max_auto_flood_wait_secs
                        && wait_secs > max_wait
                    {
                        return Err(InvocationError::Rpc(rpc_err));
                    }
                    // Match `Instance::Private::onErrorDefault` (`mtproto/mtp_instance.cpp:1662`):
                    // emit non-premium delay notification for `FLOOD_PREMIUM_WAIT_X`.
                    if let Some(premium_secs) = rpc_err.flood_premium_wait_secs() {
                        let _ = self.inner.non_premium_delay_tx.send(premium_secs);
                    }
                    debug!(wait_secs, error = %rpc_err.name, "sleeping for flood/slowmode wait");
                    tokio::time::sleep(
                        Duration::from_secs(wait_secs as u64) + Duration::from_millis(10),
                    )
                    .await;
                    continue;
                }

                // 5. 5xx / negative server error -> exponential backoff retry (1s..64s).
                if rpc_err.code < 0 || rpc_err.code >= 500 {
                    let wait = server_retry_secs;
                    server_retry_secs = if server_retry_secs > 60 {
                        server_retry_secs
                    } else {
                        (server_retry_secs * 2).min(64)
                    };
                    warn!(code = rpc_err.code, name = %rpc_err.name, wait, "retrying 5xx RPC error");
                    tokio::time::sleep(Duration::from_secs(wait) + Duration::from_millis(10)).await;
                    continue;
                }

                // 6. Unauthorized on guest DC (401 != AUTH_KEY_PERM_EMPTY, or first 400 FILE_ID_INVALID)
                //    -> export auth from main DC and import into guest DC (coalesced).
                //    Never export/import user auth to CDN DCs (`Instance::Private::logoutGuestDcs`
                //    in `mtproto/mtp_instance.cpp:765` and `SessionPrivate::tryAcquireKeyCreation`
                //    in `mtproto/session_private.cpp:2608`), and bound guest 401 retries to prevent
                //    an infinite loop if the guest DC rejects the imported authorization.
                //    Unauthorized on main DC (401 != AUTH_KEY_PERM_EMPTY, e.g. AUTH_KEY_UNREGISTERED /
                //    SESSION_REVOKED) -> notify global auth-fail handler so active session logs out
                //    (`Instance::Private::onErrorDefault` in `mtproto/mtp_instance.cpp:1666-1680`).
                let target_bare = bare_dc_id(target_dc);
                let target_is_cdn = self.inner.dc_options.dc_type(target_dc) == DcType::Cdn;
                let is_guest_auth_err = (rpc_err.is_unauthorized() && guest_auth_retries < 2)
                    || (rpc_err.is_bad_guest_dc() && !retried_bad_guest);
                if allow_guest_auth_transfer
                    && !target_is_cdn
                    && is_guest_auth_err
                    && target_bare != self.main_dc_id()
                {
                    if rpc_err.is_bad_guest_dc() {
                        retried_bad_guest = true;
                    } else {
                        guest_auth_retries += 1;
                    }
                    self.inner.authorized_dcs.write().remove(&target_bare);
                    if Box::pin(self.transfer_auth_to_dc(target_bare, target_dc))
                        .await
                        .is_ok()
                    {
                        continue;
                    }
                } else if rpc_err.is_unauthorized() && target_bare == self.main_dc_id() {
                    let _ = self.inner.auth_fail_tx.send(rpc_err.clone());
                }

                return Err(InvocationError::Rpc(rpc_err));
            }

            return Ok(reply);
        }
    }

    /// Exports authorization from main DC and imports it into `target_bare_dc`, coalescing
    /// concurrent waiters (`Instance::Private::sendAuthExport` / `exportDone` / `importDone`).
    async fn transfer_auth_to_dc(
        &self,
        target_bare_dc: DcId,
        target_shifted_dc: ShiftedDcId,
    ) -> Result<(), InvocationError> {
        loop {
            let leader = match self.inner.auth_transfers.join(target_bare_dc) {
                Joined::Leader(leader) => leader,
                Joined::Waiter(waiter) => match waiter.result().await {
                    Some(Ok(())) => return Ok(()),
                    Some(Err(msg)) => return Err(InvocationError::Protocol(msg)),
                    // The leading request was cancelled before the transfer finished: run it here.
                    None => continue,
                },
            };
            let outcome = async {
                let exported = self.export_authorization(target_bare_dc).await?;
                let _ = self
                    .import_authorization(target_shifted_dc, exported.id, exported.bytes)
                    .await?;
                Ok::<(), InvocationError>(())
            }
            .await;
            leader.finish(outcome.as_ref().map(|_| ()).map_err(ToString::to_string));
            return outcome;
        }
    }

    fn resolve_shifted_dc(&self, shifted_dc_id: ShiftedDcId) -> ShiftedDcId {
        if shifted_dc_id == 0 {
            return self.main_dc_id();
        }
        if bare_dc_id(shifted_dc_id) == 0 {
            return shifted_dc_id + bare_dc_id(self.main_dc_id());
        }
        shifted_dc_id
    }

    fn get_or_create_dcenter(&self, shifted_dc_id: ShiftedDcId) -> Dcenter {
        let raw_bare = bare_dc_id(shifted_dc_id);
        let real_dc = if is_temporary_dc_id(raw_bare) {
            let real = real_id_from_temporary_dc_id(raw_bare);
            if real != 0 { real } else { raw_bare }
        } else {
            raw_bare
        };
        if let Some(dc) = self.inner.dcenters.read().get(&real_dc) {
            return dc.clone();
        }
        let mut guard = self.inner.dcenters.write();
        guard
            .entry(real_dc)
            .or_insert_with(|| Dcenter::new(real_dc, None))
            .clone()
    }

    fn get_or_start_session(&self, shifted_dc_id: ShiftedDcId) -> SessionHandle {
        if let Some(session) = self.inner.sessions.read().get(&shifted_dc_id) {
            return session.clone();
        }
        let dcenter = self.get_or_create_dcenter(shifted_dc_id);
        let is_main_session =
            get_dc_id_shift(shifted_dc_id) == 0 && bare_dc_id(shifted_dc_id) == self.main_dc_id();
        let state_watch = is_main_session.then(|| {
            (
                Arc::clone(&self.inner.connection_state),
                self.inner.connection_state_tx.clone(),
            )
        });
        let mut guard = self.inner.sessions.write();
        guard
            .entry(shifted_dc_id)
            .or_insert_with(|| {
                SessionHandle::spawn_with_state(
                    shifted_dc_id,
                    self.inner.config.clone(),
                    self.inner.dc_options.clone(),
                    self.inner.clock.clone(),
                    dcenter,
                    self.inner.transport_factory.clone(),
                    self.inner.updates_tx.clone(),
                    Some(self.inner.updates_broadcast_tx.clone()),
                    self.inner.proxy.clone(),
                    Arc::clone(&self.inner.try_ipv6),
                    state_watch,
                    false,
                )
            })
            .clone()
    }
}

impl RpcClient for Client {
    async fn invoke<R: RemoteCall + Send + Sync>(
        &self,
        req: &R,
    ) -> Result<R::Return, InvocationError> {
        Client::invoke(self, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        dc_id: ShiftedDcId,
        req: &R,
    ) -> Result<R::Return, InvocationError> {
        Client::invoke_in_dc(self, dc_id, req).await
    }

    async fn invoke_with_options<R: RemoteCall + Send + Sync>(
        &self,
        req: &R,
        options: InvokeOptions,
    ) -> Result<R::Return, InvocationError> {
        Client::invoke_with_options(self, req, options).await
    }

    fn updates(&self) -> broadcast::Receiver<UpdatesMsg> {
        Client::updates(self)
    }

    fn main_dc_id(&self) -> DcId {
        Client::main_dc_id(self)
    }

    fn set_main_dc_id(&self, new_main_dc: DcId) {
        Client::set_main_dc_id(self, new_main_dc)
    }

    async fn export_authorization(
        &self,
        dc_id: DcId,
    ) -> Result<tl_types::auth::ExportedAuthorization, InvocationError> {
        Client::export_authorization(self, dc_id).await
    }

    async fn import_authorization(
        &self,
        dc_id: ShiftedDcId,
        id: i64,
        bytes: Vec<u8>,
    ) -> Result<tl_enums::auth::Authorization, InvocationError> {
        Client::import_authorization(self, dc_id, id, bytes).await
    }
}

/// Ergonomic request sender wrapper porting `MTP::Sender` (`mtproto/sender.h`).
#[derive(Clone)]
pub struct Sender {
    client: Client,
}

impl Sender {
    /// Creates a `Sender` bound to `client`.
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// Underlying [`Client`].
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Starts building a request (`sender.request(req).to_dc(...).send().await`).
    pub fn request<'a, R: RemoteCall>(&self, req: &'a R) -> RequestBuilder<'a, R> {
        RequestBuilder {
            client: self.client.clone(),
            req,
            options: InvokeOptions::default(),
        }
    }

    /// Convenience shortcut matching `Client::invoke`.
    pub async fn invoke<R: RemoteCall>(&self, req: &R) -> Result<R::Return, InvocationError> {
        self.client.invoke(req).await
    }

    /// Convenience shortcut matching `Client::invoke_in_dc`.
    pub async fn invoke_in_dc<R: RemoteCall>(
        &self,
        dc_id: ShiftedDcId,
        req: &R,
    ) -> Result<R::Return, InvocationError> {
        self.client.invoke_in_dc(dc_id, req).await
    }
}

/// Fluent builder for an MTProto RPC invocation (port of `Sender::SpecificRequestBuilder`).
pub struct RequestBuilder<'a, R: RemoteCall> {
    client: Client,
    req: &'a R,
    options: InvokeOptions,
}

impl<'a, R: RemoteCall> RequestBuilder<'a, R> {
    /// Targets a specific bare or shifted DC (`toDC`).
    pub fn to_dc(mut self, dc_id: ShiftedDcId) -> Self {
        self.options.dc_id = dc_id;
        self
    }

    /// Orders this request after `request_id` using `invokeAfterMsg` (`afterRequest`).
    pub fn after_request(mut self, request_id: RequestId) -> Self {
        self.options.after_request_id = Some(request_id);
        self
    }

    /// Disables automatic `FLOOD_WAIT` sleeping/retrying (`handleFloodErrors`).
    pub fn handle_flood_errors(mut self) -> Self {
        self.options.handle_flood_errors = true;
        self
    }

    /// Disables all automatic RPC error handling (`handleAllErrors`).
    pub fn handle_all_errors(mut self) -> Self {
        self.options.handle_all_errors = true;
        self
    }

    /// Controls whether `invokeWithLayer(initConnection(...))` wrapping is applied.
    pub fn needs_layer(mut self, needs: bool) -> Self {
        self.options.needs_layer = needs;
        self
    }

    /// Executes the request and awaits its deserialized response.
    pub async fn send(self) -> Result<R::Return, InvocationError> {
        self.client
            .invoke_with_options(self.req, self.options)
            .await
    }
}
