//! Port of `Instance::Mode::KeysDestroyer` from `mtproto/mtp_instance.cpp`
//! (`addKeysForDestroy`, `scheduleKeyDestroy`, `performKeyDestroy`, `keyWasPossiblyDestroyed`,
//! `completedKeyDestroy`).
//!
//! Destroys stale persistent auth keys on Telegram servers by opening dedicated non-PFS
//! sessions on unique `destroy_key_next_dc_id` slots, calling `auth.logOut` (for non-CDN DCs)
//! followed by `destroy_auth_key`, and treating `-404`, `destroy_auth_key_ok`,
//! `destroy_auth_key_none`, or missing DC options as destroyed.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use tg_crypto::AuthKey;
use tg_tl::functions::auth::LogOut;
use tg_tl::mtproto::{enums as mtp_enums, functions as mtp_functions, types as mtp_types};
use tg_tl::{Deserializable, Identifiable, Serializable};
use tg_transport::{Error as TransportError, ProxyData, TransportErrorCode};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use crate::auth_key::{AuthKeyType, Dcenter, ManagedAuthKey};
use crate::config::ClientConfig;
use crate::dc_options::{DcId, DcOptions, DcType, ShiftedDcId, bare_dc_id, destroy_key_next_dc_id};
use crate::error::{InvocationError, RpcError};
use crate::message::next_request_id;
use crate::session::{QueuedRequest, SessionHandle};
use crate::time::MtprotoClock;
use crate::transport_adapter::{TgTransportFactory, TransportFactory};

/// Default per-key destruction timeout so unreachable DCs do not hang shutdown indefinitely.
pub const DEFAULT_DESTROY_KEY_TIMEOUT: Duration = Duration::from_secs(30);

/// Outcome for a single stale auth key handled by [`KeysDestroyer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyDestroyOutcome {
    /// `destroy_auth_key_ok` received from the server.
    Destroyed,
    /// `destroy_auth_key_none` or `-404` received, or DC options missing (key already gone).
    AlreadyDestroyed,
    /// Server explicitly returned `destroy_auth_key_fail`; key remains tracked.
    Failed,
    /// Destruction timed out or transport failed before completion; key remains tracked.
    TimedOut,
}

/// Summary returned by [`KeysDestroyer::destroy_all`].
#[derive(Debug, Clone)]
pub struct KeysDestroyResult {
    /// Per-key `(dc_id, key_id, outcome)` records.
    pub outcomes: Vec<(DcId, u64, KeyDestroyOutcome)>,
    /// Keys that were NOT confirmed destroyed and should still be persisted for later retry.
    pub remaining: Vec<(DcId, AuthKey)>,
}

impl KeysDestroyResult {
    /// Returns `true` if all queued keys were destroyed (matches `allKeysDestroyed`).
    pub fn all_destroyed(&self) -> bool {
        self.remaining.is_empty()
    }
}

/// Coordinates destruction of stale persistent auth keys (`MTP::Instance::Mode::KeysDestroyer`).
pub struct KeysDestroyer {
    config: ClientConfig,
    dc_options: DcOptions,
    clock: MtprotoClock,
    proxy: Arc<RwLock<ProxyData>>,
    transport_factory: Arc<dyn TransportFactory>,
    keys: RwLock<BTreeMap<ShiftedDcId, (DcId, AuthKey)>>,
    per_key_timeout: Duration,
}

impl KeysDestroyer {
    /// Creates a `KeysDestroyer` using the default `tg-transport` factory.
    pub fn new(config: ClientConfig, dc_options: DcOptions, keys: Vec<(DcId, AuthKey)>) -> Self {
        let factory = Arc::new(TgTransportFactory::from_config(&config));
        Self::with_transport(config, dc_options, factory, keys)
    }

    /// Creates a `KeysDestroyer` with an explicit [`TransportFactory`] (for tests or custom stacks).
    pub fn with_transport(
        config: ClientConfig,
        dc_options: DcOptions,
        transport_factory: Arc<dyn TransportFactory>,
        keys: Vec<(DcId, AuthKey)>,
    ) -> Self {
        let proxy = Arc::new(RwLock::new(config.proxy.clone()));
        let destroyer = Self {
            config,
            dc_options,
            clock: MtprotoClock::new(),
            proxy,
            transport_factory,
            keys: RwLock::new(BTreeMap::new()),
            per_key_timeout: DEFAULT_DESTROY_KEY_TIMEOUT,
        };
        destroyer.add_keys(keys);
        destroyer
    }

    /// Sets the per-key destruction timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.per_key_timeout = timeout;
        self
    }

    /// Enqueues additional stale `(DcId, AuthKey)` pairs, assigning each a distinct
    /// `destroy_key_next_dc_id` shifted slot so multiple keys for the same DC never collide
    /// (`Instance::Private::addKeysForDestroy`).
    pub fn add_keys(&self, keys: Vec<(DcId, AuthKey)>) {
        let mut guard = self.keys.write();
        for (dc_id, key) in keys {
            let mut shifted = destroy_key_next_dc_id(dc_id);
            while guard.contains_key(&shifted) {
                shifted = destroy_key_next_dc_id(shifted);
            }
            guard.insert(shifted, (bare_dc_id(dc_id), key));
        }
    }

    /// Returns the assigned `(ShiftedDcId, DcId, key_id)` slots currently pending destruction.
    pub fn pending_slots(&self) -> Vec<(ShiftedDcId, DcId, u64)> {
        self.keys
            .read()
            .iter()
            .map(|(&shifted, (dc_id, key))| (shifted, *dc_id, key.key_id_u64()))
            .collect()
    }

    /// Returns all keys still pending destruction (`getKeysForWrite` in `KeysDestroyer` mode).
    pub fn remaining_keys(&self) -> Vec<(DcId, AuthKey)> {
        self.keys
            .read()
            .values()
            .map(|(dc_id, key)| (*dc_id, key.clone()))
            .collect()
    }

    /// Executes `auth.logOut` (on non-CDN DCs) + `destroy_auth_key` concurrently for all
    /// registered keys, removing confirmed-destroyed keys from the registry.
    pub async fn destroy_all(&self) -> KeysDestroyResult {
        let entries: Vec<(ShiftedDcId, DcId, AuthKey)> = self
            .keys
            .read()
            .iter()
            .map(|(&shifted, (dc_id, key))| (shifted, *dc_id, key.clone()))
            .collect();

        let mut set: JoinSet<(ShiftedDcId, DcId, u64, KeyDestroyOutcome)> = JoinSet::new();
        for (shifted, dc_id, key) in entries {
            let mut cfg = self.config.clone();
            // KeysDestroyer always uses the persistent key directly (no PFS temp key).
            cfg.use_pfs = false;
            let dc_options = self.dc_options.clone();
            let clock = self.clock.clone();
            let proxy = self.proxy.clone();
            let factory = self.transport_factory.clone();
            let per_key_timeout = self.per_key_timeout;
            let key_id = key.key_id_u64();

            set.spawn(async move {
                let outcome = destroy_one_key(
                    shifted,
                    dc_id,
                    key,
                    cfg,
                    dc_options,
                    clock,
                    proxy,
                    factory,
                    per_key_timeout,
                )
                .await;
                (shifted, dc_id, key_id, outcome)
            });
        }

        let mut outcomes = Vec::new();
        while let Some(joined) = set.join_next().await {
            if let Ok((shifted, dc_id, key_id, outcome)) = joined {
                if matches!(
                    outcome,
                    KeyDestroyOutcome::Destroyed | KeyDestroyOutcome::AlreadyDestroyed
                ) {
                    self.keys.write().remove(&shifted);
                }
                outcomes.push((dc_id, key_id, outcome));
            }
        }

        let remaining = self.remaining_keys();
        KeysDestroyResult {
            outcomes,
            remaining,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn destroy_one_key(
    shifted_dc_id: ShiftedDcId,
    dc_id: DcId,
    key: AuthKey,
    config: ClientConfig,
    dc_options: DcOptions,
    clock: MtprotoClock,
    proxy: Arc<RwLock<ProxyData>>,
    transport_factory: Arc<dyn TransportFactory>,
    per_key_timeout: Duration,
) -> KeyDestroyOutcome {
    let dc_type = dc_options.dc_type(shifted_dc_id);
    let variants = dc_options.lookup(dc_id, dc_type, false);
    let has_any_endpoint = !variants.ipv4_tcp.is_empty()
        || !variants.ipv6_tcp.is_empty()
        || !variants.ipv4_http.is_empty()
        || !variants.ipv6_http.is_empty();
    if !has_any_endpoint {
        // Port of `session_private.cpp:1081-1084`: missing DC options for key destruction ->
        // treat key as possibly destroyed.
        warn!(
            shifted_dc_id,
            dc_id, "no DC options for key destruction, assuming destroyed"
        );
        return KeyDestroyOutcome::AlreadyDestroyed;
    }

    let managed = ManagedAuthKey::new(key, AuthKeyType::ReadFromFile, dc_id);
    let dcenter = Dcenter::new(dc_id, Some(managed));
    let (updates_tx, _updates_rx) = mpsc::unbounded_channel();
    let session = SessionHandle::spawn_with_options(
        shifted_dc_id,
        config,
        dc_options,
        clock,
        dcenter,
        transport_factory,
        updates_tx,
        proxy,
        true, // keys_destroyer_mode
    );

    let flow = async {
        // Step 1: On non-CDN DCs, call `auth.logOut` first (`scheduleKeyDestroy`).
        if dc_type != DcType::Cdn {
            let mut logout_req = QueuedRequest::new(next_request_id(), LogOut.to_bytes());
            logout_req.needs_layer = true;
            match session.send_raw(logout_req).await {
                Ok(raw) => {
                    if raw.len() >= 4
                        && u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]])
                            == mtp_types::RpcError::CONSTRUCTOR_ID
                        && let Ok(mtp_enums::RpcError::RpcError(tl_err)) =
                            mtp_enums::RpcError::from_bytes(&raw)
                    {
                        let parsed = RpcError::parse(tl_err.error_code, &tl_err.error_message);
                        debug!(shifted_dc_id, error = %parsed, "auth.logOut in KeysDestroyer returned RPC error, proceeding to destroy_auth_key");
                    }
                }
                Err(InvocationError::Transport(TransportError::ServerError(
                    TransportErrorCode::AuthKeyNotFound,
                ))) => {
                    return KeyDestroyOutcome::AlreadyDestroyed;
                }
                Err(err) => {
                    debug!(shifted_dc_id, error = %err, "auth.logOut transport error in KeysDestroyer");
                    return KeyDestroyOutcome::TimedOut;
                }
            }
        }

        // Step 2: Call `destroy_auth_key#d1435160` (`performKeyDestroy`).
        let mut destroy_req =
            QueuedRequest::new(next_request_id(), mtp_functions::DestroyAuthKey.to_bytes());
        destroy_req.needs_layer = false;
        match session.send_raw(destroy_req).await {
            Ok(raw) => {
                if raw.len() >= 4
                    && u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]])
                        == mtp_types::RpcError::CONSTRUCTOR_ID
                {
                    // Port of `performKeyDestroy` error handler: assumes key possibly destroyed.
                    debug!(
                        shifted_dc_id,
                        "destroy_auth_key returned RPC error, assuming destroyed"
                    );
                    return KeyDestroyOutcome::AlreadyDestroyed;
                }
                match mtp_enums::DestroyAuthKeyRes::from_bytes(&raw) {
                    Ok(mtp_enums::DestroyAuthKeyRes::DestroyAuthKeyOk) => {
                        info!(shifted_dc_id, "auth key destroyed on server");
                        KeyDestroyOutcome::Destroyed
                    }
                    Ok(mtp_enums::DestroyAuthKeyRes::DestroyAuthKeyNone) => {
                        info!(shifted_dc_id, "auth key already destroyed on server");
                        KeyDestroyOutcome::AlreadyDestroyed
                    }
                    Ok(mtp_enums::DestroyAuthKeyRes::DestroyAuthKeyFail) => {
                        warn!(shifted_dc_id, "server returned destroy_auth_key_fail");
                        KeyDestroyOutcome::Failed
                    }
                    Err(err) => {
                        warn!(shifted_dc_id, error = %err, "failed to parse DestroyAuthKeyRes");
                        KeyDestroyOutcome::AlreadyDestroyed
                    }
                }
            }
            Err(InvocationError::Transport(TransportError::ServerError(
                TransportErrorCode::AuthKeyNotFound,
            ))) => KeyDestroyOutcome::AlreadyDestroyed,
            Err(err) => {
                warn!(shifted_dc_id, error = %err, "destroy_auth_key failed");
                KeyDestroyOutcome::TimedOut
            }
        }
    };

    let outcome = match tokio::time::timeout(per_key_timeout, flow).await {
        Ok(res) => res,
        Err(_) => {
            warn!(shifted_dc_id, "key destruction timed out");
            KeyDestroyOutcome::TimedOut
        }
    };
    session.shutdown();
    outcome
}
