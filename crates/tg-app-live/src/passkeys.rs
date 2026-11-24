//! Live passkeys controller (`LivePasskeysHandle`) and intro passkey login RPC helpers.
//!
//! Port origin: `Telegram/SourceFiles/data/components/passkeys.h`,
//! `Telegram/SourceFiles/data/components/passkeys.cpp`, and
//! `Telegram/SourceFiles/intro/intro_qr.cpp` (`setupPasskeyLink`).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use tg_api::rpc::RpcClient;
use tg_app_core::core_types::erased_rpc::ErasedRpcClient;
use tg_platform::cable_scanner::{
    CableBleScanner, MatchedCableEid, WinRtCableBleScanner, start_cable_proximity_match,
};
use tg_platform::cable_tunnel::{
    CableTunnelClient, CableTunnelConnector, CableTunnelEndpoint, TlsCableTunnelConnector,
};
use tg_platform::webauthn::{
    CABLE_EID_KEY_LEN, CABLE_QR_SECRET_LEN, CableCeremonyHandle, LoginData, LoginResult,
    P256_X962_LEN, RegisterData, RegisterResult, WebAuthnProvider, WindowsWebAuthnProvider,
    deserialize_register_data, encode_base64url_padded,
};
use tg_session::SessionRuntime;
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tracing::warn;

use crate::bridge::{LiveBridgeState, LiveRuntimeBridge};
use crate::media::{block_on_runtime, with_rpc_timeout};
use tg_app_core::settings::backend::{PasskeyEntry, PasskeyHybridQrStatus};

pub use tg_app_core::core_types::misc::DEFAULT_MAX_PASSKEYS;

/// Extracts the native Win32 `HWND` (or `0` on non-Windows / when unavailable) from any
/// `raw_window_handle::HasWindowHandle` such as `eframe::CreationContext` or `eframe::Frame`.
#[must_use]
pub fn win32_hwnd_from_window_handle(handle: &impl HasWindowHandle) -> usize {
    match handle.window_handle().map(|wh| wh.as_raw()) {
        Ok(RawWindowHandle::Win32(win)) => win.hwnd.get() as usize,
        _ => 0,
    }
}

/// Timeout for synchronous passkey RPC calls initiated from UI actions.
const PASSKEY_RPC_TIMEOUT: Duration = Duration::from_secs(15);

/// Converts a generated `tg_tl::types::Passkey` into a UI [`PasskeyEntry`].
#[must_use]
pub fn passkey_from_tl(raw: &tl_types::Passkey) -> PasskeyEntry {
    PasskeyEntry {
        id: raw.id.clone(),
        name: raw.name.clone(),
        date: raw.date,
        software_emoji_id: raw.software_emoji_id,
        last_usage_date: raw.last_usage_date,
    }
}

/// Converts a generated `tg_tl::enums::Passkey` into a UI [`PasskeyEntry`].
#[must_use]
pub fn passkey_enum_from_tl(raw: &tl_enums::Passkey) -> PasskeyEntry {
    match raw {
        tl_enums::Passkey::Passkey(pk) => passkey_from_tl(pk),
    }
}

/// Builds the TL `InputPasskeyCredential` for `account.registerPasskey` (`passkeys.cpp:65-73`).
#[must_use]
pub fn build_register_credential_tl(result: &RegisterResult) -> tl_enums::InputPasskeyCredential {
    let credential_id_b64 = encode_base64url_padded(&result.credential_id);
    let client_data_str = String::from_utf8_lossy(&result.client_data_json).into_owned();
    tl_enums::InputPasskeyCredential::PublicKey(tl_types::InputPasskeyCredentialPublicKey {
        id: credential_id_b64.clone(),
        raw_id: credential_id_b64,
        response: tl_enums::InputPasskeyResponse::Register(
            tl_types::InputPasskeyResponseRegister {
                client_data: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                    data: client_data_str,
                }),
                attestation_data: result.attestation_object.clone(),
            },
        ),
    })
}

// ============================================================================
// Live Account Passkeys Controller (`Data::Passkeys`)
// ============================================================================

struct LivePasskeysState {
    entries: Vec<PasskeyEntry>,
    list_known: bool,
    max_passkeys: usize,
    pending_registration: Option<(RegisterData, Instant)>,
    provider: Arc<dyn WebAuthnProvider>,
    cable_scanner: Arc<dyn CableBleScanner>,
    cable_tunnel: Arc<dyn CableTunnelConnector>,
    active_cable: Option<CableCeremonyHandle>,
    cable_status: PasskeyHybridQrStatus,
    matched_cable_eid: Option<MatchedCableEid>,
    last_hybrid_registered: Option<PasskeyEntry>,
    cable_generation: u64,
}

struct LivePasskeysInner {
    runtime: Arc<SessionRuntime<ErasedRpcClient>>,
    bridge: Arc<LiveBridgeState>,
    window_hwnd: AtomicUsize,
    state: Mutex<LivePasskeysState>,
}

/// Shared live controller for account passkeys (`account.getPasskeys`,
/// `account.initPasskeyRegistration`, `account.registerPasskey`, `account.deletePasskey`).
pub struct LivePasskeysHandle {
    inner: Arc<LivePasskeysInner>,
}

impl Clone for LivePasskeysHandle {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl LivePasskeysHandle {
    /// Creates a new [`LivePasskeysHandle`] using [`WindowsWebAuthnProvider`] by default.
    #[must_use]
    pub fn new(runtime: Arc<SessionRuntime<ErasedRpcClient>>, bridge: &LiveRuntimeBridge) -> Self {
        Self::with_provider(runtime, bridge, Arc::new(WindowsWebAuthnProvider::new()))
    }

    /// Creates a new [`LivePasskeysHandle`] with an explicit [`WebAuthnProvider`] (e.g. a fake in tests).
    #[must_use]
    pub fn with_provider(
        runtime: Arc<SessionRuntime<ErasedRpcClient>>,
        bridge: &LiveRuntimeBridge,
        provider: Arc<dyn WebAuthnProvider>,
    ) -> Self {
        Self {
            inner: Arc::new(LivePasskeysInner {
                runtime,
                bridge: Arc::clone(bridge.state()),
                window_hwnd: AtomicUsize::new(0),
                state: Mutex::new(LivePasskeysState {
                    entries: Vec::new(),
                    list_known: false,
                    max_passkeys: DEFAULT_MAX_PASSKEYS,
                    pending_registration: None,
                    provider,
                    cable_scanner: Arc::new(WinRtCableBleScanner::new()),
                    cable_tunnel: Arc::new(TlsCableTunnelConnector::new()),
                    active_cable: None,
                    cable_status: PasskeyHybridQrStatus::Idle,
                    matched_cable_eid: None,
                    last_hybrid_registered: None,
                    cable_generation: 0,
                }),
            }),
        }
    }

    /// Replaces the active [`WebAuthnProvider`] (primarily for tests using `FakeWebAuthnProvider`).
    pub fn set_provider(&self, provider: Arc<dyn WebAuthnProvider>) {
        if let Ok(mut st) = self.inner.state.lock() {
            st.provider = provider;
        }
    }

    /// Replaces the active [`CableBleScanner`] (primarily for tests using `FakeCableBleScanner`).
    pub fn set_cable_scanner(&self, scanner: Arc<dyn CableBleScanner>) {
        if let Ok(mut st) = self.inner.state.lock() {
            st.cable_scanner.stop();
            st.cable_scanner = scanner;
        }
    }

    /// Replaces the active [`CableTunnelConnector`] (primarily for tests using
    /// `FakeCableTunnelConnector` so no real tunnel server is contacted).
    pub fn set_cable_tunnel(&self, tunnel: Arc<dyn CableTunnelConnector>) {
        if let Ok(mut st) = self.inner.state.lock() {
            st.cable_tunnel = tunnel;
        }
    }

    /// Sets the parent main-window Win32 `HWND` passed to native WebAuthn dialogs.
    pub fn set_window_hwnd(&self, hwnd: usize) {
        self.inner.window_hwnd.store(hwnd, Ordering::Relaxed);
    }

    /// Returns the configured parent main-window Win32 `HWND` (`0` if unset).
    #[must_use]
    pub fn window_hwnd(&self) -> usize {
        self.inner.window_hwnd.load(Ordering::Relaxed)
    }

    /// Overrides the maximum allowed passkeys per account (`passkeysAccountPasskeysMax`).
    pub fn set_max_passkeys(&self, max_passkeys: usize) {
        if let Ok(mut st) = self.inner.state.lock() {
            st.max_passkeys = max_passkeys;
        }
    }

    /// Returns whether the configured platform WebAuthn provider is supported.
    #[must_use]
    pub fn is_supported(&self) -> bool {
        self.inner
            .state
            .lock()
            .map(|st| st.provider.is_supported())
            .unwrap_or(false)
    }

    /// Returns whether a new passkey can be registered (`Data::Passkeys::canRegister`).
    #[must_use]
    pub fn can_register(&self) -> bool {
        self.inner
            .state
            .lock()
            .map(|st| st.provider.is_supported() && st.entries.len() < st.max_passkeys)
            .unwrap_or(false)
    }

    /// Returns the cached list of registered passkeys.
    #[must_use]
    pub fn passkeys(&self) -> Vec<PasskeyEntry> {
        self.inner
            .state
            .lock()
            .map(|st| st.entries.clone())
            .unwrap_or_default()
    }

    /// Returns whether the passkey list has been fetched at least once from the server.
    #[must_use]
    pub fn list_known(&self) -> bool {
        self.inner
            .state
            .lock()
            .map(|st| st.list_known)
            .unwrap_or(false)
    }

    /// Fetches `account.getPasskeys` from the server and updates the local cache.
    pub fn refresh_list(&self) -> Result<Vec<PasskeyEntry>, String> {
        let rpc = self.inner.runtime.rpc().clone();
        let handle = self.inner.runtime.tokio_handle().clone();
        let resp = block_on_runtime(&handle, async move {
            with_rpc_timeout(PASSKEY_RPC_TIMEOUT, async move {
                rpc.invoke(&tl_fns::account::GetPasskeys {}).await
            })
            .await
        })
        .map_err(|e| format!("account.getPasskeys failed: {e}"))?;

        let tl_enums::account::Passkeys::Passkeys(data) = resp;
        let entries: Vec<PasskeyEntry> = data.passkeys.iter().map(passkey_enum_from_tl).collect();
        if let Ok(mut st) = self.inner.state.lock() {
            st.entries = entries.clone();
            st.list_known = true;
        }
        self.inner.bridge.notify_ui();
        Ok(entries)
    }

    /// Calls `account.initPasskeyRegistration` (reusing a cached non-expired registration challenge
    /// just like `Passkeys::initRegistration` in `passkeys.cpp:40-60`).
    pub fn init_registration(&self) -> Result<RegisterData, String> {
        let now = Instant::now();
        if let Ok(st) = self.inner.state.lock()
            && let Some((ref cached, created_at)) = st.pending_registration
            && now.duration_since(created_at) < Duration::from_millis(u64::from(cached.timeout_ms))
        {
            return Ok(cached.clone());
        }

        let rpc = self.inner.runtime.rpc().clone();
        let handle = self.inner.runtime.tokio_handle().clone();
        let resp = block_on_runtime(&handle, async move {
            with_rpc_timeout(PASSKEY_RPC_TIMEOUT, async move {
                rpc.invoke(&tl_fns::account::InitPasskeyRegistration {})
                    .await
            })
            .await
        })
        .map_err(|e| format!("account.initPasskeyRegistration failed: {e}"))?;

        let tl_enums::account::PasskeyRegistrationOptions::PasskeyRegistrationOptions(opts) = resp;
        let tl_enums::DataJSON::DataJSON(json_data) = opts.options;
        let parsed = deserialize_register_data(json_data.data.as_bytes())
            .map_err(|e| format!("Invalid passkey registration options: {e}"))?;

        if let Ok(mut st) = self.inner.state.lock() {
            st.pending_registration = Some((parsed.clone(), now));
        }
        Ok(parsed)
    }

    /// Runs the full passkey creation flow: `account.initPasskeyRegistration` ->
    /// `WebAuthnProvider::make_credential` -> `account.registerPasskey`.
    pub fn register_passkey(&self, hwnd: usize) -> Result<PasskeyEntry, String> {
        let data = self.init_registration()?;
        let provider = self
            .inner
            .state
            .lock()
            .map(|st| Arc::clone(&st.provider))
            .map_err(|_| "passkey state lock poisoned".to_owned())?;

        let result = provider
            .make_credential(hwnd, &data)
            .map_err(|e| e.to_string())?;
        self.register_passkey_with_result(&result)
    }

    /// Completes passkey registration with an already-produced [`RegisterResult`]
    /// (from native Windows WebAuthn or a cross-device caBLE ceremony).
    pub fn register_passkey_with_result(
        &self,
        result: &RegisterResult,
    ) -> Result<PasskeyEntry, String> {
        let credential = build_register_credential_tl(result);
        let rpc = self.inner.runtime.rpc().clone();
        let handle = self.inner.runtime.tokio_handle().clone();
        let resp = block_on_runtime(&handle, async move {
            with_rpc_timeout(PASSKEY_RPC_TIMEOUT, async move {
                rpc.invoke(&tl_fns::account::RegisterPasskey { credential })
                    .await
            })
            .await
        });

        match resp {
            Ok(tl_passkey) => {
                let entry = passkey_enum_from_tl(&tl_passkey);
                if let Ok(mut st) = self.inner.state.lock() {
                    st.cable_scanner.stop();
                    st.pending_registration = None;
                    st.active_cable = None;
                    st.cable_status = PasskeyHybridQrStatus::Idle;
                    st.matched_cable_eid = None;
                    if !st.entries.iter().any(|existing| existing.id == entry.id) {
                        st.entries.push(entry.clone());
                    }
                    st.list_known = true;
                }
                self.inner.bridge.notify_ui();
                Ok(entry)
            }
            Err(err) => {
                if let Ok(mut st) = self.inner.state.lock() {
                    st.pending_registration = None;
                }
                Err(format!("account.registerPasskey failed: {err}"))
            }
        }
    }

    /// Starts a cross-device caBLE / hybrid QR passkey registration ceremony, starts the
    /// [`CableBleScanner`] proximity watcher for FIDO2 (`0xFFF9`) / Google (`0xFDE2`) EID adverts,
    /// and returns the `"FIDO:/..."` QR URI to display to the user.
    ///
    /// Once a matching BLE advert arrives, the controller moves to
    /// [`PasskeyHybridQrStatus::Connecting`], opens the caBLEv2 WebSocket tunnel via the
    /// configured [`CableTunnelConnector`], runs the Noise `KNpsk0` handshake and encrypted CTAP2
    /// `authenticatorMakeCredential` exchange, submits `account.registerPasskey`, and transitions
    /// to [`PasskeyHybridQrStatus::Completed`] or [`PasskeyHybridQrStatus::Error`].
    pub fn start_hybrid_qr_registration(&self) -> Result<String, String> {
        let data = self.init_registration()?;
        let (provider, scanner) = self
            .inner
            .state
            .lock()
            .map(|st| (Arc::clone(&st.provider), Arc::clone(&st.cable_scanner)))
            .map_err(|_| "passkey state lock poisoned".to_owned())?;
        scanner.stop();
        let cable = provider
            .start_cable_register(&data)
            .map_err(|e| e.to_string())?;
        let uri = cable.session.qr_uri().to_owned();
        let eid_key = *cable.session.eid_key();
        let generation = {
            let mut st = self
                .inner
                .state
                .lock()
                .map_err(|_| "passkey state lock poisoned".to_owned())?;
            st.cable_generation = st.cable_generation.wrapping_add(1);
            st.active_cable = Some(cable);
            st.matched_cable_eid = None;
            st.last_hybrid_registered = None;
            st.cable_status = PasskeyHybridQrStatus::Scanning;
            st.cable_generation
        };

        let inner_match = Arc::clone(&self.inner);
        let inner_avail = Arc::clone(&self.inner);
        let register_data = data.clone();
        let on_availability: tg_platform::cable_scanner::AvailabilityCallback =
            Arc::new(move |available| {
                if !available {
                    if let Ok(mut st) = inner_avail.state.lock()
                        && st.cable_generation == generation
                        && matches!(st.cable_status, PasskeyHybridQrStatus::Scanning)
                    {
                        st.cable_status = PasskeyHybridQrStatus::NoBluetooth;
                    }
                    inner_avail.bridge.notify_ui();
                }
            });

        match start_cable_proximity_match(
            &scanner,
            eid_key,
            true,
            move |matched| {
                let tunnel_job = {
                    let Ok(mut st) = inner_match.state.lock() else {
                        return;
                    };
                    if st.cable_generation != generation {
                        return;
                    }
                    let Some(ref mut ceremony) = st.active_cable else {
                        return;
                    };
                    let accepted = ceremony
                        .session
                        .accept_ble_advert(&matched.raw_service_data);
                    let client_data_json = ceremony.client_data_json.clone();
                    let client_data_hash = ceremony.client_data_hash;
                    let tunnel = Arc::clone(&st.cable_tunnel);
                    st.cable_status = PasskeyHybridQrStatus::Connecting {
                        tunnel_domain: matched.tunnel_domain.clone(),
                    };
                    st.matched_cable_eid = Some(matched);
                    accepted.map(|(domain, path, initiator)| {
                        (
                            CableTunnelEndpoint::new(domain, path),
                            initiator,
                            client_data_json,
                            client_data_hash,
                            tunnel,
                        )
                    })
                };
                inner_match.bridge.notify_ui();

                let Some((endpoint, initiator, client_data_json, client_data_hash, tunnel)) =
                    tunnel_job
                else {
                    if let Ok(mut st) = inner_match.state.lock()
                        && st.cable_generation == generation
                    {
                        st.active_cable = None;
                        st.cable_status = PasskeyHybridQrStatus::Error(
                            "BLE advertisement did not match active caBLE QR session".to_owned(),
                        );
                    }
                    inner_match.bridge.notify_ui();
                    return;
                };

                let inner_task = Arc::clone(&inner_match);
                let req_data = register_data.clone();
                let timeout = Duration::from_millis(u64::from(req_data.timeout_ms.max(5_000)));
                let tokio_handle = inner_task.runtime.tokio_handle().clone();
                tokio_handle.spawn(async move {
                    let tunnel_res = tokio::task::spawn_blocking(move || {
                        CableTunnelClient::new(tunnel).run_register(
                            &endpoint,
                            initiator,
                            client_data_json,
                            client_data_hash,
                            &req_data,
                            timeout,
                        )
                    })
                    .await;

                    let outcome = match tunnel_res {
                        Ok(Ok(outcome)) => outcome,
                        Ok(Err(err)) => {
                            if let Ok(mut st) = inner_task.state.lock()
                                && st.cable_generation == generation
                            {
                                st.active_cable = None;
                                st.cable_status = PasskeyHybridQrStatus::Error(err.to_string());
                            }
                            inner_task.bridge.notify_ui();
                            return;
                        }
                        Err(join_err) => {
                            if let Ok(mut st) = inner_task.state.lock()
                                && st.cable_generation == generation
                            {
                                st.active_cable = None;
                                st.cable_status = PasskeyHybridQrStatus::Error(format!(
                                    "caBLE tunnel worker failed: {join_err}"
                                ));
                            }
                            inner_task.bridge.notify_ui();
                            return;
                        }
                    };

                    // Do not submit RPC if the user cancelled while the tunnel was running.
                    let still_current = inner_task
                        .state
                        .lock()
                        .map(|st| st.cable_generation == generation)
                        .unwrap_or(false);
                    if !still_current {
                        return;
                    }

                    let credential = build_register_credential_tl(&outcome.result);
                    let rpc = inner_task.runtime.rpc().clone();
                    let rpc_res = with_rpc_timeout(PASSKEY_RPC_TIMEOUT, async move {
                        rpc.invoke(&tl_fns::account::RegisterPasskey { credential })
                            .await
                    })
                    .await;

                    match rpc_res {
                        Ok(tl_passkey) => {
                            let entry = passkey_enum_from_tl(&tl_passkey);
                            if let Ok(mut st) = inner_task.state.lock()
                                && st.cable_generation == generation
                            {
                                st.cable_scanner.stop();
                                st.pending_registration = None;
                                st.active_cable = None;
                                st.matched_cable_eid = None;
                                if !st.entries.iter().any(|existing| existing.id == entry.id) {
                                    st.entries.push(entry.clone());
                                }
                                st.list_known = true;
                                st.last_hybrid_registered = Some(entry);
                                st.cable_status = PasskeyHybridQrStatus::Completed;
                            }
                            inner_task.bridge.notify_ui();
                        }
                        Err(err) => {
                            if let Ok(mut st) = inner_task.state.lock()
                                && st.cable_generation == generation
                            {
                                st.pending_registration = None;
                                st.active_cable = None;
                                st.cable_status = PasskeyHybridQrStatus::Error(format!(
                                    "account.registerPasskey failed: {err}"
                                ));
                            }
                            inner_task.bridge.notify_ui();
                        }
                    }
                });
            },
            on_availability,
        ) {
            Ok(true) => {}
            Ok(false) => {
                if let Ok(mut st) = self.inner.state.lock()
                    && st.cable_generation == generation
                    && matches!(st.cable_status, PasskeyHybridQrStatus::Scanning)
                {
                    st.cable_status = PasskeyHybridQrStatus::NoBluetooth;
                }
            }
            Err(err) => {
                warn!(error = %err, "caBLE BLE scanner failed to start");
                if let Ok(mut st) = self.inner.state.lock()
                    && st.cable_generation == generation
                {
                    st.cable_status = PasskeyHybridQrStatus::Error(err.to_string());
                }
            }
        }
        self.inner.bridge.notify_ui();
        Ok(uri)
    }

    /// Executes a caBLEv2 `authenticatorGetAssertion` hybrid login exchange over the configured
    /// [`CableTunnelConnector`] for a matched BLE `service_data` advert.
    pub fn run_hybrid_login_over_tunnel(
        &self,
        login_data: &LoginData,
        ceremony: &mut CableCeremonyHandle,
        service_data: &[u8],
    ) -> Result<LoginResult, String> {
        let tunnel = self
            .inner
            .state
            .lock()
            .map(|st| Arc::clone(&st.cable_tunnel))
            .map_err(|_| "passkey state lock poisoned".to_owned())?;
        let (domain, path, initiator) = ceremony
            .session
            .accept_ble_advert(service_data)
            .ok_or_else(|| "BLE advertisement did not match caBLE login session".to_owned())?;
        let endpoint = CableTunnelEndpoint::new(domain, path);
        let timeout = Duration::from_millis(u64::from(login_data.timeout_ms.max(5_000)));
        let outcome = CableTunnelClient::new(tunnel)
            .run_login(
                &endpoint,
                initiator,
                ceremony.client_data_json.clone(),
                ceremony.client_data_hash,
                login_data,
                timeout,
            )
            .map_err(|e| e.to_string())?;
        Ok(outcome.result)
    }

    /// Returns the derived 64-byte EID key of the active hybrid QR session (for deterministic tests).
    #[must_use]
    pub fn active_cable_eid_key(&self) -> Option<[u8; CABLE_EID_KEY_LEN]> {
        self.inner
            .state
            .lock()
            .ok()
            .and_then(|st| st.active_cable.as_ref().map(|c| *c.session.eid_key()))
    }

    /// Returns the 16-byte QR secret of the active hybrid QR session (for deterministic tests).
    #[must_use]
    pub fn active_cable_qr_secret(&self) -> Option<[u8; CABLE_QR_SECRET_LEN]> {
        self.inner
            .state
            .lock()
            .ok()
            .and_then(|st| st.active_cable.as_ref().map(|c| *c.session.qr_secret()))
    }

    /// Returns the initiator's uncompressed 65-byte P-256 identity key for the active hybrid QR
    /// session (for deterministic tests).
    #[must_use]
    pub fn active_cable_identity_public_x962(&self) -> Option<[u8; P256_X962_LEN]> {
        self.inner.state.lock().ok().and_then(|st| {
            st.active_cable
                .as_ref()
                .map(|c| c.session.identity_public_x962())
        })
    }

    /// Returns the [`PasskeyEntry`] produced by the most recently completed hybrid QR registration.
    #[must_use]
    pub fn last_hybrid_registered(&self) -> Option<PasskeyEntry> {
        self.inner
            .state
            .lock()
            .ok()
            .and_then(|st| st.last_hybrid_registered.clone())
    }

    /// Returns the live caBLEv2 BLE proximity scanning status for the active hybrid QR ceremony.
    #[must_use]
    pub fn hybrid_qr_status(&self) -> PasskeyHybridQrStatus {
        self.inner
            .state
            .lock()
            .map(|st| st.cable_status.clone())
            .unwrap_or(PasskeyHybridQrStatus::Idle)
    }

    /// Returns the authenticated caBLEv2 proximity match if a phone BLE advert has been received.
    #[must_use]
    pub fn matched_cable_eid(&self) -> Option<MatchedCableEid> {
        self.inner
            .state
            .lock()
            .ok()
            .and_then(|st| st.matched_cable_eid.clone())
    }

    /// Cancels any active hybrid QR passkey ceremony and stops the BLE proximity scanner.
    pub fn cancel_hybrid_qr(&self) {
        if let Ok(mut st) = self.inner.state.lock() {
            st.cable_generation = st.cable_generation.wrapping_add(1);
            st.cable_scanner.stop();
            st.active_cable = None;
            st.matched_cable_eid = None;
            st.cable_status = PasskeyHybridQrStatus::Idle;
        }
        self.inner.bridge.notify_ui();
    }

    /// Deletes a registered passkey via `account.deletePasskey` and reloads the passkey list
    /// (`Passkeys::deletePasskey` in `passkeys.cpp:84-98`).
    pub fn delete_passkey(&self, id: &str) -> Result<(), String> {
        let id_owned = id.to_owned();
        let rpc = self.inner.runtime.rpc().clone();
        let handle = self.inner.runtime.tokio_handle().clone();
        let deleted = block_on_runtime(&handle, async move {
            with_rpc_timeout(PASSKEY_RPC_TIMEOUT, async move {
                rpc.invoke(&tl_fns::account::DeletePasskey { id: id_owned })
                    .await
            })
            .await
        })
        .map_err(|e| format!("account.deletePasskey failed: {e}"))?;

        if !deleted {
            return Err("Server rejected passkey deletion.".to_owned());
        }

        if let Ok(mut st) = self.inner.state.lock() {
            st.entries.retain(|entry| entry.id != id);
            st.list_known = false;
        }
        if let Err(err) = self.refresh_list() {
            warn!(error = %err, "passkeys refresh after delete failed");
        }
        self.inner.bridge.notify_ui();
        Ok(())
    }
}
