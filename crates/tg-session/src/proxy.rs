//! Live proxy resolution, real MTProto proxy connectivity checking, and automatic proxy rotation.
//!
//! Port origin:
//! - `Telegram/SourceFiles/mtproto/proxy_check.h` & `proxy_check.cpp`
//! - `Telegram/SourceFiles/core/proxy_rotation_manager.h` & `proxy_rotation_manager.cpp`
//! - System proxy resolution (`QNetworkProxyFactory::systemProxyForQuery` equivalent).

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tg_core::settings_proxy::{
    ProxyData as CoreProxyData, ProxySettingsMode, ProxyType as CoreProxyType, SettingsProxy,
};
use tg_mtproto::dc_options::protocol_dc_id;
use tg_mtproto::web_proxy::{
    WebProxyBridgeServer, WebProxyTransportFactory, check_web_proxy, probe_web_proxy_http_url,
    probe_web_proxy_ws_addr, web_proxy_bridge_url,
};
use tg_mtproto::{Client as MtprotoClient, ConnectionState, DcId, DcOptions, DcType};
use tg_transport::{
    Connection, ConnectionConfig, FORCE_HTTP_PORT, HttpConnection, HttpConnectionConfig,
    ProxyData as TransportProxyData, ProxyType as TransportProxyType, TransportKind,
};
use tokio::runtime::Handle;
use tokio::sync::Notify;
use tokio::task::{JoinHandle, JoinSet};
use tracing::{debug, info};

/// Interval between consecutive proxy rotation health checks (`kProxyRotationCheckInterval`).
pub const PROXY_ROTATION_CHECK_INTERVAL: Duration = Duration::from_secs(2);

/// Maximum lifetime of an in-flight proxy rotation check (`kProxyRotationCheckLifetime`).
pub const PROXY_ROTATION_CHECK_LIFETIME: Duration = Duration::from_secs(20);

/// Maximum number of concurrent proxy rotation checks (`kProxyRotationMaxActiveChecks`).
pub const PROXY_ROTATION_MAX_ACTIVE_CHECKS: usize = 10;

/// Default timeout for a single proxy ping / availability probe.
pub const DEFAULT_PROXY_CHECK_TIMEOUT: Duration = Duration::from_secs(8);

/// Converts `tg-core`'s [`CoreProxyData`] into `tg-transport`'s [`TransportProxyData`].
#[must_use]
pub fn core_proxy_to_transport(p: &CoreProxyData) -> TransportProxyData {
    let proxy_type = match p.proxy_type {
        CoreProxyType::None => TransportProxyType::None,
        CoreProxyType::Socks5 => TransportProxyType::Socks5,
        CoreProxyType::Http => TransportProxyType::Http,
        CoreProxyType::Mtproto => TransportProxyType::Mtproto,
        CoreProxyType::Web => TransportProxyType::Web,
    };
    TransportProxyData {
        proxy_type,
        host: p.host.clone(),
        port: p.port.min(u32::from(u16::MAX)) as u16,
        user: p.user.clone(),
        password: p.password.clone(),
        resolved_ips: Vec::new(),
        resolved_expire_at: None,
    }
}

/// Converts `tg-transport`'s [`TransportProxyData`] into `tg-core`'s [`CoreProxyData`].
#[must_use]
pub fn transport_proxy_to_core(p: &TransportProxyData) -> CoreProxyData {
    let proxy_type = match p.proxy_type {
        TransportProxyType::None => CoreProxyType::None,
        TransportProxyType::Socks5 => CoreProxyType::Socks5,
        TransportProxyType::Http => CoreProxyType::Http,
        TransportProxyType::Mtproto => CoreProxyType::Mtproto,
        TransportProxyType::Web => CoreProxyType::Web,
    };
    CoreProxyData {
        proxy_type,
        host: p.host.clone(),
        port: u32::from(p.port),
        user: p.user.clone(),
        password: p.password.clone(),
    }
}

/// Resolves the effective [`TransportProxyData`] for the given [`SettingsProxy`].
///
/// - `Disabled` -> direct connection (`ProxyData::none()`).
/// - `System` -> system proxy from environment (`ALL_PROXY` / `HTTPS_PROXY` / `HTTP_PROXY`) or direct.
/// - `Enabled` -> selected proxy if valid, otherwise direct.
#[must_use]
pub fn resolve_effective_proxy(settings: &SettingsProxy) -> TransportProxyData {
    match settings.settings() {
        ProxySettingsMode::Disabled => TransportProxyData::none(),
        ProxySettingsMode::System => resolve_system_proxy(),
        ProxySettingsMode::Enabled => {
            if settings.selected().is_valid() {
                let tp = core_proxy_to_transport(settings.selected());
                if tp.valid() {
                    return tp;
                }
            }
            TransportProxyData::none()
        }
    }
}

/// Resolves system proxy configuration from standard environment variables
/// (`ALL_PROXY`, `HTTPS_PROXY`, `HTTP_PROXY` and lowercase variants).
#[must_use]
pub fn resolve_system_proxy() -> TransportProxyData {
    for var in [
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ] {
        if let Ok(raw) = std::env::var(var)
            && let Some(proxy) = parse_system_proxy_url(&raw)
            && proxy.valid()
        {
            return proxy;
        }
    }
    TransportProxyData::none()
}

/// Parses a standard proxy URI (`socks5://[user[:pass]@]host:port`, `http://...`, or `host:port`)
/// into a [`TransportProxyData`].
#[must_use]
pub fn parse_system_proxy_url(raw: &str) -> Option<TransportProxyData> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (proxy_type, rest) = if let Some(r) = trimmed
        .strip_prefix("socks5h://")
        .or_else(|| trimmed.strip_prefix("socks5://"))
        .or_else(|| trimmed.strip_prefix("SOCKS5://"))
    {
        (TransportProxyType::Socks5, r)
    } else if let Some(r) = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("HTTP://"))
        .or_else(|| trimmed.strip_prefix("https://"))
        .or_else(|| trimmed.strip_prefix("HTTPS://"))
    {
        (TransportProxyType::Http, r)
    } else if !trimmed.contains("://") {
        (TransportProxyType::Http, trimmed)
    } else {
        return None;
    };

    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() {
        return None;
    }

    let (user, password, host_port) = match authority.rsplit_once('@') {
        Some((creds, hp)) => match creds.split_once(':') {
            Some((u, p)) => (u.to_owned(), p.to_owned(), hp),
            None => (creds.to_owned(), String::new(), hp),
        },
        None => (String::new(), String::new(), authority),
    };

    let (host, port_str) = if let Some(stripped) = host_port.strip_prefix('[') {
        let (ipv6, after) = stripped.split_once(']')?;
        let port_str = after.strip_prefix(':')?;
        (ipv6.to_owned(), port_str)
    } else {
        let (h, p) = host_port.rsplit_once(':')?;
        (h.to_owned(), p)
    };

    let port = port_str.parse::<u16>().ok()?;
    if host.is_empty() || port == 0 {
        return None;
    }

    Some(match proxy_type {
        TransportProxyType::Socks5 => TransportProxyData::socks5(host, port, user, password),
        TransportProxyType::Http => TransportProxyData::http(host, port, user, password),
        _ => return None,
    })
}

/// Endpoint override used when checking a [`TransportProxyType::Web`] proxy
/// (allows deterministic local fake-server tests without touching the real internet).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum WebProxyCheckTarget {
    /// Probe the canonical `https://<host><path>?bridge=<capability>` URL derived from `ProxyData`.
    #[default]
    DefaultHttps,
    /// Probe an explicit HTTP(S) bridge URL (e.g. a local fake loopback HTTP server).
    BridgeUrl(String),
    /// Probe an explicit WebSocket WebProxy relay (`101 Switching Protocols` + `Hello` -> `Welcome`).
    WebSocketRelay {
        /// Loopback or relay socket address.
        addr: SocketAddr,
        /// Request path (e.g. `"/transport"`).
        path: String,
        /// HTTP `Host` header (e.g. `"127.0.0.1:41234"`).
        host_header: String,
        /// HTTP `Origin` header (e.g. `"http://127.0.0.1:41234"`).
        origin: String,
    },
}

/// Performs a reachability / handshake check for a `ProxyType::Web` proxy through
/// `tg-mtproto::web_proxy`, returning the measured round-trip duration on success.
pub async fn check_web_proxy_endpoint(
    proxy: &TransportProxyData,
    target: &WebProxyCheckTarget,
    timeout: Duration,
) -> Result<Duration, String> {
    if proxy.proxy_type != TransportProxyType::Web {
        return Err("Expected a Web proxy configuration".to_owned());
    }
    if !proxy.valid() {
        return Err(format!("Invalid web proxy ({:?})", proxy.status()));
    }
    // Always validate bridge capability & canonical URL derivation first.
    let _canonical_url = web_proxy_bridge_url(proxy).map_err(|e| e.to_string())?;
    match target {
        WebProxyCheckTarget::DefaultHttps => check_web_proxy(proxy, timeout)
            .await
            .map_err(|e| e.to_string()),
        WebProxyCheckTarget::BridgeUrl(url) => probe_web_proxy_http_url(url, timeout)
            .await
            .map_err(|e| e.to_string()),
        WebProxyCheckTarget::WebSocketRelay {
            addr,
            path,
            host_header,
            origin,
        } => probe_web_proxy_ws_addr(*addr, path, host_header, origin, timeout)
            .await
            .map_err(|e| e.to_string()),
    }
}

/// Starts a local loopback [`WebProxyBridgeServer`] for a validated `ProxyType::Web` proxy.
pub async fn start_web_proxy_bridge(
    proxy: &TransportProxyData,
) -> Result<WebProxyBridgeServer, String> {
    WebProxyBridgeServer::bind(proxy.clone())
        .await
        .map_err(|e| e.to_string())
}

/// Creates a [`WebProxyTransportFactory`] backed by an active [`WebProxyBridgeServer`].
#[must_use]
pub fn web_proxy_transport_factory(server: Arc<WebProxyBridgeServer>) -> WebProxyTransportFactory {
    WebProxyTransportFactory::new(server)
}

/// Performs a real MTProto connectivity check (`req_pq_multi` -> `resPQ`, or WebProxy bridge
/// handshake for `ProxyType::Web`) through `proxy`, returning the measured round-trip time.
///
/// Port of `MTP::StartProxyCheck` (`Telegram/SourceFiles/mtproto/proxy_check.cpp`) extended with
/// `tg-mtproto::web_proxy` support.
pub async fn check_proxy(
    dc_id: DcId,
    dc_options: &DcOptions,
    proxy: &TransportProxyData,
    try_ipv6: bool,
    timeout: Duration,
) -> Result<Duration, String> {
    if proxy.proxy_type == TransportProxyType::None {
        return Err("No proxy configured".to_owned());
    }
    if !proxy.valid() {
        return Err(format!("Invalid proxy ({:?})", proxy.status()));
    }
    if proxy.proxy_type == TransportProxyType::Web {
        return check_web_proxy_endpoint(proxy, &WebProxyCheckTarget::DefaultHttps, timeout).await;
    }

    let proto_dc = protocol_dc_id(dc_id, DcType::Regular, dc_options.is_test_mode());

    if proxy.proxy_type == TransportProxyType::Mtproto {
        let cfg = ConnectionConfig {
            transport: TransportKind::PaddedIntermediate,
            obfuscated: true,
            dc_id: proto_dc,
            protocol_secret: Vec::new(),
            proxy: proxy.clone(),
            connect_timeout: timeout,
            tcp_nodelay: true,
        };
        let mut conn = Connection::connect(&proxy.host, proxy.port, cfg)
            .await
            .map_err(|e| e.to_string())?;
        return conn.probe_pq(timeout).await.map_err(|e| e.to_string());
    }

    // SOCKS5 / HTTP: look up DC endpoints and race IPv4 (and IPv6 when try_ipv6 is enabled),
    // matching `StartProxyCheck` in `mtproto/proxy_check.cpp:90-110`.
    let variants = dc_options.lookup(dc_id, DcType::Regular, true);
    let mut endpoints = Vec::new();
    if let Some(v4) = variants.ipv4_tcp.first().cloned() {
        endpoints.push(v4);
    }
    if try_ipv6 && let Some(v6) = variants.ipv6_tcp.first().cloned() {
        endpoints.push(v6);
    }
    if endpoints.is_empty() {
        return Err(format!("No DC endpoints available for DC {dc_id}"));
    }

    let mut set = JoinSet::new();
    for ep in endpoints {
        let proxy_clone = proxy.clone();
        set.spawn(async move {
            let cfg = ConnectionConfig {
                transport: TransportKind::Intermediate,
                obfuscated: true,
                dc_id: proto_dc,
                protocol_secret: ep.secret.clone(),
                proxy: proxy_clone.clone(),
                connect_timeout: timeout,
                tcp_nodelay: true,
            };
            match Connection::connect(&ep.ip, ep.port, cfg).await {
                Ok(mut conn) => conn.probe_pq(timeout).await.map_err(|e| e.to_string()),
                Err(tcp_err) if proxy_clone.proxy_type == TransportProxyType::Http => {
                    // Fallback to plain HTTP /api probe for HTTP proxies that do not permit CONNECT.
                    let http_cfg = HttpConnectionConfig {
                        port: FORCE_HTTP_PORT,
                        proxy: proxy_clone,
                        timeout,
                    };
                    let mut http_conn = HttpConnection::new(ep.ip, http_cfg);
                    http_conn
                        .probe_pq()
                        .await
                        .map_err(|http_err| format!("{tcp_err}; HTTP fallback: {http_err}"))
                }
                Err(err) => Err(err.to_string()),
            }
        });
    }

    let mut last_err = "Proxy check failed".to_owned();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Ok(rtt)) => {
                set.abort_all();
                return Ok(rtt);
            }
            Ok(Err(e)) => last_err = e,
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(last_err)
}

/// Per-proxy tracking entry inside [`ProxyRotationState`] (`ProxyRotationManager::Entry`).
#[derive(Debug, Clone)]
struct RotationEntry {
    proxy: CoreProxyData,
    checking: bool,
    started_at: Option<Instant>,
    available_at: Option<Instant>,
    last_ping_ms: Option<u32>,
}

/// Deterministic state machine porting `Core::ProxyRotationManager`
/// (`Telegram/SourceFiles/core/proxy_rotation_manager.h/.cpp`).
#[derive(Debug, Default)]
pub struct ProxyRotationState {
    entries: Vec<RotationEntry>,
    probe_order: Vec<usize>,
    next_check_index: usize,
    checking: bool,
    waiting_to_switch: bool,
    switch_started_at: Option<Instant>,
}

impl ProxyRotationState {
    /// Creates an idle [`ProxyRotationState`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if automatic proxy rotation should observe connection state
    /// (`ProxyRotationManager::shouldObserve`).
    #[must_use]
    pub fn should_observe(settings: &SettingsProxy) -> bool {
        settings.is_enabled()
            && settings.selected().is_valid()
            && settings.proxy_rotation_enabled()
            && settings.list().len() > 1
    }

    /// Whether rotation background probing is currently active.
    #[must_use]
    pub const fn is_checking(&self) -> bool {
        self.checking
    }

    /// Whether the rotation timeout has expired and rotation will switch as soon as any
    /// candidate proxy reports available.
    #[must_use]
    pub const fn is_waiting_to_switch(&self) -> bool {
        self.waiting_to_switch
    }

    /// Current candidate probe order (indices into `SettingsProxy::list()`).
    #[must_use]
    pub fn probe_order(&self) -> &[usize] {
        &self.probe_order
    }

    /// Returns the deadline at which `on_switch_timer` should fire, if currently checking.
    #[must_use]
    pub fn switch_deadline(&self, settings: &SettingsProxy) -> Option<Instant> {
        if !self.checking || self.waiting_to_switch {
            return None;
        }
        let started = self.switch_started_at?;
        let secs = settings.proxy_rotation_timeout().max(1) as u64;
        Some(started + Duration::from_secs(secs))
    }

    /// Handles proxy settings mutations (`ProxyRotationManager::settingsChanged`).
    pub fn on_settings_changed(
        &mut self,
        settings: &SettingsProxy,
        is_connected: bool,
        now: Instant,
    ) {
        self.stop_checking();
        self.prune_removed_entries(settings);
        self.reevaluate(settings, is_connected, now);
    }

    /// Handles connection state changes (`ProxyRotationManager::handleConnectionStateChanged`).
    pub fn on_connection_state_changed(
        &mut self,
        settings: &SettingsProxy,
        is_connected: bool,
        now: Instant,
    ) {
        self.reevaluate(settings, is_connected, now);
    }

    /// Re-evaluates whether checking should start or stop (`ProxyRotationManager::reevaluate`).
    pub fn reevaluate(&mut self, settings: &SettingsProxy, is_connected: bool, now: Instant) {
        if !Self::should_observe(settings) || is_connected {
            self.stop_checking();
            return;
        }
        self.start_checking(settings, now);
    }

    /// Cancels active rotation probing (`ProxyRotationManager::stopChecking`).
    pub fn stop_checking(&mut self) {
        self.checking = false;
        self.waiting_to_switch = false;
        self.switch_started_at = None;
        self.probe_order.clear();
        self.next_check_index = 0;
        for entry in &mut self.entries {
            entry.checking = false;
            entry.started_at = None;
        }
    }

    /// Expires in-flight checks older than [`PROXY_ROTATION_CHECK_LIFETIME`]
    /// (`ProxyRotationManager::pruneExpiredChecks`).
    pub fn prune_expired_checks(&mut self, now: Instant) {
        for entry in &mut self.entries {
            if entry.checking
                && let Some(started) = entry.started_at
                && now.saturating_duration_since(started) >= PROXY_ROTATION_CHECK_LIFETIME
            {
                entry.checking = false;
                entry.started_at = None;
            }
        }
    }

    /// Selects the next candidate proxy to probe and marks it as in-flight
    /// (`ProxyRotationManager::startNextCheck`).
    pub fn next_proxy_to_check(
        &mut self,
        settings: &SettingsProxy,
        now: Instant,
    ) -> Option<CoreProxyData> {
        if !self.checking || self.probe_order.is_empty() {
            return None;
        }
        self.prune_expired_checks(now);
        let active_checks = self.entries.iter().filter(|e| e.checking).count();
        if active_checks >= PROXY_ROTATION_MAX_ACTIVE_CHECKS {
            return None;
        }

        let list = settings.list();
        let mut attempts_left = self.probe_order.len();
        while attempts_left > 0 {
            attempts_left -= 1;
            if self.next_check_index >= self.probe_order.len() {
                self.next_check_index = 0;
            }
            let list_index = self.probe_order[self.next_check_index];
            self.next_check_index += 1;
            let Some(proxy) = list.get(list_index) else {
                continue;
            };
            let entry = self.ensure_entry(proxy);
            if entry.checking {
                continue;
            }
            entry.checking = true;
            entry.started_at = Some(now);
            return Some(proxy.clone());
        }
        None
    }

    /// Records a successful probe result (`ProxyRotationManager::checkDone`).
    ///
    /// Promotes the responsive proxy in `settings.proxy_rotation_preferred_indices` and,
    /// if the switch timeout has already elapsed while still disconnected, returns the
    /// proxy that should be activated immediately.
    pub fn on_check_done(
        &mut self,
        settings: &mut SettingsProxy,
        proxy: &CoreProxyData,
        ping_ms: u32,
        is_connected: bool,
        now: Instant,
    ) -> Option<CoreProxyData> {
        let entry = self.find_entry_mut(proxy)?;
        if !entry.checking {
            return None;
        }
        entry.checking = false;
        entry.started_at = None;
        entry.available_at = Some(now);
        entry.last_ping_ms = Some(ping_ms);

        if let Some(index) = settings.index_in_list(proxy) {
            settings.promote_proxy_rotation_preferred_index(index as i32);
        }
        self.update_probe_order(settings);

        if self.waiting_to_switch && self.should_switch_to_available(settings, is_connected) {
            let switched = self.switch_to_available(settings);
            self.waiting_to_switch = switched.is_none();
            return switched;
        }
        None
    }

    /// Records a failed probe result (`ProxyRotationManager::checkFailed`).
    pub fn on_check_failed(&mut self, proxy: &CoreProxyData) {
        if let Some(entry) = self.find_entry_mut(proxy)
            && entry.checking
        {
            entry.checking = false;
            entry.started_at = None;
        }
    }

    /// Called when the rotation timeout (`proxy_rotation_timeout`) elapses
    /// (`ProxyRotationManager::switchTimerDone`).
    ///
    /// Returns `Some(proxy)` if a checked proxy is already available to switch to;
    /// otherwise sets `waiting_to_switch = true` so the first proxy to pass its check
    /// triggers an immediate switch.
    pub fn on_switch_timer(
        &mut self,
        settings: &SettingsProxy,
        is_connected: bool,
    ) -> Option<CoreProxyData> {
        if !self.should_switch_to_available(settings, is_connected) {
            return None;
        }
        let switched = self.switch_to_available(settings);
        self.waiting_to_switch = switched.is_none();
        switched
    }

    fn start_checking(&mut self, settings: &SettingsProxy, now: Instant) {
        if self.checking {
            return;
        }
        self.checking = true;
        self.waiting_to_switch = false;
        self.switch_started_at = Some(now);
        self.update_probe_order(settings);
    }

    fn prune_removed_entries(&mut self, settings: &SettingsProxy) {
        self.entries
            .retain(|entry| settings.index_in_list(&entry.proxy).is_some());
    }

    fn update_probe_order(&mut self, settings: &SettingsProxy) {
        let current_index = settings.index_in_list(settings.selected());
        let list = settings.list();
        let can_check = |idx: usize| -> bool {
            list.get(idx)
                .is_some_and(|p| p.proxy_type != CoreProxyType::Web && p.is_valid())
        };

        self.probe_order.clear();
        self.probe_order.reserve(list.len());
        for &pref in settings.proxy_rotation_preferred_indices() {
            if pref < 0 {
                continue;
            }
            let idx = pref as usize;
            if Some(idx) == current_index || !can_check(idx) || self.probe_order.contains(&idx) {
                continue;
            }
            self.probe_order.push(idx);
        }
        for idx in 0..list.len() {
            if Some(idx) == current_index || !can_check(idx) || self.probe_order.contains(&idx) {
                continue;
            }
            self.probe_order.push(idx);
        }
        self.next_check_index = 0;
    }

    fn should_switch_to_available(&self, settings: &SettingsProxy, is_connected: bool) -> bool {
        self.checking && Self::should_observe(settings) && !is_connected
    }

    fn switch_to_available(&mut self, settings: &SettingsProxy) -> Option<CoreProxyData> {
        if !self.checking {
            return None;
        }
        let switch_started = self.switch_started_at?;
        let list = settings.list();
        for &idx in &self.probe_order {
            let Some(proxy) = list.get(idx) else {
                continue;
            };
            let Some(entry) = self.find_entry(proxy) else {
                continue;
            };
            if entry.checking {
                continue;
            }
            let Some(avail) = entry.available_at else {
                continue;
            };
            if avail < switch_started {
                continue;
            }
            self.waiting_to_switch = false;
            return Some(proxy.clone());
        }
        None
    }

    fn find_entry(&self, proxy: &CoreProxyData) -> Option<&RotationEntry> {
        self.entries.iter().find(|e| e.proxy == *proxy)
    }

    fn find_entry_mut(&mut self, proxy: &CoreProxyData) -> Option<&mut RotationEntry> {
        self.entries.iter_mut().find(|e| e.proxy == *proxy)
    }

    fn ensure_entry(&mut self, proxy: &CoreProxyData) -> &mut RotationEntry {
        if let Some(pos) = self.entries.iter().position(|e| e.proxy == *proxy) {
            return &mut self.entries[pos];
        }
        self.entries.push(RotationEntry {
            proxy: proxy.clone(),
            checking: false,
            started_at: None,
            available_at: None,
            last_ping_ms: None,
        });
        let last = self.entries.len() - 1;
        &mut self.entries[last]
    }
}

/// Boxed future for injectable proxy health checks in [`ProxyRotationRunner`].
pub type ProxyCheckFuture = Pin<Box<dyn Future<Output = Result<Duration, String>> + Send>>;

/// Injectable proxy check function signature (`(proxy, try_ipv6) -> Future<Result<Duration, String>>`).
pub type ProxyCheckFn = Arc<dyn Fn(CoreProxyData, bool) -> ProxyCheckFuture + Send + Sync>;

/// Callback invoked whenever automatic proxy rotation switches to a new proxy.
pub type ProxyRotatedCallback = Arc<dyn Fn(&SettingsProxy, &CoreProxyData) + Send + Sync>;

/// Background async controller driving [`ProxyRotationState`] against a live [`MtprotoClient`].
pub struct ProxyRotationRunner {
    settings: Arc<Mutex<SettingsProxy>>,
    notify: Arc<Notify>,
    task: JoinHandle<()>,
}

impl ProxyRotationRunner {
    /// Spawns a rotation runner observing `rpc`'s underlying MTProto client with an owned initial
    /// [`SettingsProxy`] snapshot (creates the internal shared lock automatically).
    #[must_use]
    pub fn spawn_for_rpc_owned(
        handle: &Handle,
        rpc: &crate::rpc::MtprotoRpc,
        initial_settings: SettingsProxy,
        on_rotated: Option<ProxyRotatedCallback>,
    ) -> Self {
        Self::spawn(
            handle,
            rpc.client().clone(),
            Arc::new(Mutex::new(initial_settings)),
            on_rotated,
        )
    }

    /// Spawns a rotation runner observing `rpc`'s underlying MTProto client.
    #[must_use]
    pub fn spawn_for_rpc(
        handle: &Handle,
        rpc: &crate::rpc::MtprotoRpc,
        settings: Arc<Mutex<SettingsProxy>>,
        on_rotated: Option<ProxyRotatedCallback>,
    ) -> Self {
        Self::spawn(handle, rpc.client().clone(), settings, on_rotated)
    }

    /// Spawns a rotation runner observing `client`'s connection state using real `check_proxy`.
    #[must_use]
    pub fn spawn(
        handle: &Handle,
        client: MtprotoClient,
        settings: Arc<Mutex<SettingsProxy>>,
        on_rotated: Option<ProxyRotatedCallback>,
    ) -> Self {
        let client_for_check = client.clone();
        let checker: ProxyCheckFn = Arc::new(move |proxy: CoreProxyData, try_ipv6: bool| {
            let dc_id = client_for_check.main_dc_id();
            let dc_options = client_for_check.dc_options().clone();
            Box::pin(async move {
                let tp = core_proxy_to_transport(&proxy);
                check_proxy(
                    dc_id,
                    &dc_options,
                    &tp,
                    try_ipv6,
                    DEFAULT_PROXY_CHECK_TIMEOUT,
                )
                .await
            })
        });
        Self::spawn_with_checker(
            handle,
            client,
            settings,
            checker,
            PROXY_ROTATION_CHECK_INTERVAL,
            on_rotated,
        )
    }

    /// Spawns a rotation runner with an injectable check function and interval (for deterministic tests).
    #[must_use]
    pub fn spawn_with_checker(
        handle: &Handle,
        client: MtprotoClient,
        settings: Arc<Mutex<SettingsProxy>>,
        checker: ProxyCheckFn,
        check_interval: Duration,
        on_rotated: Option<ProxyRotatedCallback>,
    ) -> Self {
        let notify = Arc::new(Notify::new());
        let notify_bg = Arc::clone(&notify);
        let settings_bg = Arc::clone(&settings);

        let task = handle.spawn(async move {
            let mut state = ProxyRotationState::new();
            let mut state_rx = client.subscribe_connection_state();
            let mut in_flight: JoinSet<(CoreProxyData, Result<Duration, String>)> = JoinSet::new();
            let mut tick = tokio::time::interval(check_interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            // Initial evaluation based on current connection state.
            {
                let is_connected = client.connection_state() == ConnectionState::Connected;
                let s = settings_bg.lock();
                state.on_connection_state_changed(&s, is_connected, Instant::now());
            }

            loop {
                let switch_sleep = {
                    let s = settings_bg.lock();
                    state.switch_deadline(&s)
                };

                tokio::select! {
                    _ = notify_bg.notified() => {
                        in_flight.abort_all();
                        let is_connected = client.connection_state() == ConnectionState::Connected;
                        let s = settings_bg.lock();
                        state.on_settings_changed(&s, is_connected, Instant::now());
                        if let Some(next) = state.next_proxy_to_check(&s, Instant::now()) {
                            let try_ipv6 = s.try_ipv6();
                            let chk = Arc::clone(&checker);
                            in_flight.spawn(async move {
                                let res = chk(next.clone(), try_ipv6).await;
                                (next, res)
                            });
                        }
                    }
                    conn_ev = state_rx.recv() => {
                        match conn_ev {
                            Ok(conn_state) => {
                                let is_connected = conn_state == ConnectionState::Connected;
                                let s = settings_bg.lock();
                                let was_checking = state.is_checking();
                                state.on_connection_state_changed(&s, is_connected, Instant::now());
                                if !state.is_checking() {
                                    in_flight.abort_all();
                                } else if !was_checking
                                    && let Some(next) = state.next_proxy_to_check(&s, Instant::now())
                                {
                                    let try_ipv6 = s.try_ipv6();
                                    let chk = Arc::clone(&checker);
                                    in_flight.spawn(async move {
                                        let res = chk(next.clone(), try_ipv6).await;
                                        (next, res)
                                    });
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                let is_connected = client.connection_state() == ConnectionState::Connected;
                                let s = settings_bg.lock();
                                state.on_connection_state_changed(&s, is_connected, Instant::now());
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                    _ = tick.tick(), if state.is_checking() => {
                        let is_connected = client.connection_state() == ConnectionState::Connected;
                        let s = settings_bg.lock();
                        state.reevaluate(&s, is_connected, Instant::now());
                        if state.is_checking()
                            && let Some(next) = state.next_proxy_to_check(&s, Instant::now())
                        {
                            let try_ipv6 = s.try_ipv6();
                            let chk = Arc::clone(&checker);
                            in_flight.spawn(async move {
                                let res = chk(next.clone(), try_ipv6).await;
                                (next, res)
                            });
                        }
                    }
                    _ = async {
                        if let Some(deadline) = switch_sleep {
                            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
                        } else {
                            std::future::pending::<()>().await;
                        }
                    }, if switch_sleep.is_some() => {
                        let is_connected = client.connection_state() == ConnectionState::Connected;
                        let switched = {
                            let s = settings_bg.lock();
                            state.on_switch_timer(&s, is_connected)
                        };
                        if let Some(target) = switched {
                            apply_rotation_switch(&client, &settings_bg, &mut state, &mut in_flight, target, &on_rotated);
                        }
                    }
                    Some(joined) = in_flight.join_next(), if !in_flight.is_empty() => {
                        if let Ok((proxy, outcome)) = joined {
                            let is_connected = client.connection_state() == ConnectionState::Connected;
                            match outcome {
                                Ok(rtt) => {
                                    let ping_ms = rtt.as_millis().min(u128::from(u32::MAX)) as u32;
                                    debug!(host = %proxy.host, port = proxy.port, ping_ms, "rotation check succeeded");
                                    let switched = {
                                        let mut s = settings_bg.lock();
                                        state.on_check_done(&mut s, &proxy, ping_ms, is_connected, Instant::now())
                                    };
                                    if let Some(target) = switched {
                                        apply_rotation_switch(&client, &settings_bg, &mut state, &mut in_flight, target, &on_rotated);
                                    }
                                }
                                Err(err) => {
                                    debug!(host = %proxy.host, port = proxy.port, error = %err, "rotation check failed");
                                    state.on_check_failed(&proxy);
                                }
                            }
                        }
                    }
                }
            }
        });

        Self {
            settings,
            notify,
            task,
        }
    }

    /// Updates stored proxy settings and notifies the background runner to re-evaluate immediately.
    pub fn update_settings(&self, next: SettingsProxy) {
        *self.settings.lock() = next;
        self.notify.notify_one();
    }

    /// Returns a snapshot of the current proxy settings (including any rotation switches).
    #[must_use]
    pub fn settings(&self) -> SettingsProxy {
        self.settings.lock().clone()
    }
}

impl Drop for ProxyRotationRunner {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn apply_rotation_switch(
    client: &MtprotoClient,
    settings_lock: &Arc<Mutex<SettingsProxy>>,
    state: &mut ProxyRotationState,
    in_flight: &mut JoinSet<(CoreProxyData, Result<Duration, String>)>,
    target: CoreProxyData,
    on_rotated: &Option<ProxyRotatedCallback>,
) {
    info!(
        host = %target.host,
        port = target.port,
        "proxy rotation switching to available proxy"
    );
    let snapshot = {
        let mut s = settings_lock.lock();
        s.set_selected(target.clone());
        s.set_settings(ProxySettingsMode::Enabled);
        s.clone()
    };
    in_flight.abort_all();
    state.on_settings_changed(&snapshot, false, Instant::now());
    let effective = resolve_effective_proxy(&snapshot);
    client.set_proxy(effective);
    if let Some(cb) = on_rotated {
        cb(&snapshot, &target);
    }
}

/// Loads persisted [`SettingsProxy`] from a `tdata` directory (`tdata/settingss`),
/// applying both modern `dbiApplicationSettings` and legacy `dbiConnectionTypeOld*` blocks.
pub fn load_proxy_settings_from_tdata(
    tdata_path: &std::path::Path,
) -> Result<SettingsProxy, String> {
    let local = tg_storage::LocalStorage::open_or_init(tdata_path)
        .map_err(|e| format!("failed to open tdata: {e}"))?;
    let core = tg_core::CoreSettings::load_from_local_storage(&local)
        .map_err(|e| format!("failed to parse core settings: {e}"))?;
    Ok(core.proxy)
}

/// Persists updated [`SettingsProxy`] into a `tdata` directory (`tdata/settingss`) without
/// clobbering unrelated [`tg_core::CoreSettings`] fields.
pub fn save_proxy_settings_to_tdata(
    tdata_path: &std::path::Path,
    proxy: &SettingsProxy,
) -> Result<(), String> {
    let mut local = tg_storage::LocalStorage::open_or_init(tdata_path)
        .map_err(|e| format!("failed to open tdata: {e}"))?;
    let mut core = tg_core::CoreSettings::load_from_local_storage(&local).unwrap_or_default();
    core.proxy = proxy.clone();
    core.save_to_local_storage(&mut local)
        .map_err(|e| format!("failed to write tdata settings: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_socks(host: &str, port: u32) -> CoreProxyData {
        CoreProxyData {
            proxy_type: CoreProxyType::Socks5,
            host: host.to_owned(),
            port,
            user: String::new(),
            password: String::new(),
        }
    }

    #[test]
    fn parses_system_proxy_urls() {
        let s = parse_system_proxy_url("socks5://alice:secret@127.0.0.1:1080").unwrap();
        assert_eq!(s.proxy_type, TransportProxyType::Socks5);
        assert_eq!(s.host, "127.0.0.1");
        assert_eq!(s.port, 1080);
        assert_eq!(s.user, "alice");
        assert_eq!(s.password, "secret");

        let h = parse_system_proxy_url("http://proxy.local:8080/").unwrap();
        assert_eq!(h.proxy_type, TransportProxyType::Http);
        assert_eq!(h.host, "proxy.local");
        assert_eq!(h.port, 8080);

        let v6 = parse_system_proxy_url("socks5://[::1]:1081").unwrap();
        assert_eq!(v6.proxy_type, TransportProxyType::Socks5);
        assert_eq!(v6.host, "::1");
        assert_eq!(v6.port, 1081);

        let bare = parse_system_proxy_url("10.0.0.1:3128").unwrap();
        assert_eq!(bare.proxy_type, TransportProxyType::Http);
        assert_eq!(bare.port, 3128);

        assert!(parse_system_proxy_url("ftp://example.com:21").is_none());
        assert!(parse_system_proxy_url("").is_none());
    }

    #[test]
    fn proxy_rotation_prefers_responsive_proxy_and_switches_on_timeout() {
        let mut settings = SettingsProxy::new();
        let p0 = sample_socks("127.0.0.1", 1080);
        let p1 = sample_socks("127.0.0.1", 1081);
        let p2 = sample_socks("127.0.0.1", 1082);
        settings.set_list(vec![p0.clone(), p1.clone(), p2.clone()]);
        settings.set_selected(p0.clone());
        settings.set_settings(ProxySettingsMode::Enabled);
        settings.set_proxy_rotation_enabled(true);
        settings.set_proxy_rotation_timeout(5);

        let t0 = Instant::now();
        let mut rot = ProxyRotationState::new();
        assert!(ProxyRotationState::should_observe(&settings));

        // Connected -> does not probe.
        rot.on_connection_state_changed(&settings, true, t0);
        assert!(!rot.is_checking());

        // Disconnected -> starts probing p1 then p2 (skipping currently selected p0).
        rot.on_connection_state_changed(&settings, false, t0);
        assert!(rot.is_checking());
        assert_eq!(rot.probe_order(), &[1, 2]);

        let first = rot.next_proxy_to_check(&settings, t0).expect("first probe");
        assert_eq!(first, p1);
        let second = rot
            .next_proxy_to_check(&settings, t0 + Duration::from_secs(2))
            .expect("second probe");
        assert_eq!(second, p2);

        // p1 fails, p2 succeeds before switch timer -> promotes p2 in preferred indices.
        rot.on_check_failed(&p1);
        let premature =
            rot.on_check_done(&mut settings, &p2, 35, false, t0 + Duration::from_secs(3));
        assert!(premature.is_none());
        assert_eq!(settings.proxy_rotation_preferred_indices(), &[2]);

        // Switch timer fires at t0 + 5s -> switches to p2!
        let chosen = rot
            .on_switch_timer(&settings, false)
            .expect("should switch to available p2");
        assert_eq!(chosen, p2);

        // Once connected again, checking stops.
        rot.on_connection_state_changed(&settings, true, t0 + Duration::from_secs(6));
        assert!(!rot.is_checking());
    }

    #[test]
    fn proxy_rotation_waits_for_first_available_after_timeout() {
        let mut settings = SettingsProxy::new();
        let p0 = sample_socks("127.0.0.1", 2080);
        let p1 = sample_socks("127.0.0.1", 2081);
        settings.set_list(vec![p0.clone(), p1.clone()]);
        settings.set_selected(p0);
        settings.set_settings(ProxySettingsMode::Enabled);
        settings.set_proxy_rotation_enabled(true);
        settings.set_proxy_rotation_timeout(5);

        let t0 = Instant::now();
        let mut rot = ProxyRotationState::new();
        rot.on_connection_state_changed(&settings, false, t0);
        assert_eq!(rot.next_proxy_to_check(&settings, t0), Some(p1.clone()));

        // Timeout expires while p1 is still checking -> waiting_to_switch becomes true.
        assert!(rot.on_switch_timer(&settings, false).is_none());
        assert!(rot.is_waiting_to_switch());

        // When p1 finishes checking, rotation switches immediately.
        let switched =
            rot.on_check_done(&mut settings, &p1, 18, false, t0 + Duration::from_secs(6));
        assert_eq!(switched, Some(p1));
        assert!(!rot.is_waiting_to_switch());
    }

    #[tokio::test]
    async fn web_proxy_endpoint_check_validates_and_probes_local_fake_relay() {
        let mut invalid = TransportProxyData::none();
        invalid.proxy_type = TransportProxyType::Web;
        invalid.host = "127.0.0.1".to_owned();
        invalid.port = 443;
        invalid.password = "000102030405060708090a0b0c0d0e0f".to_owned();
        assert!(
            check_web_proxy_endpoint(
                &invalid,
                &WebProxyCheckTarget::DefaultHttps,
                Duration::from_secs(1)
            )
            .await
            .is_err()
        );

        let mut valid = TransportProxyData::none();
        valid.proxy_type = TransportProxyType::Web;
        valid.port = 443;
        valid.set_web_address("https://proxy.example.com/app/");
        valid.password = "000102030405060708090a0b0c0d0e0f".to_owned();

        // Spawn a minimal loopback fake HTTP server answering 200 OK.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let srv = tokio::spawn(async move {
            if let Ok((mut s, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf).await;
                let _ = s
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK",
                    )
                    .await;
            }
        });

        let rtt = check_web_proxy_endpoint(
            &valid,
            &WebProxyCheckTarget::BridgeUrl(format!("http://127.0.0.1:{port}/")),
            Duration::from_secs(3),
        )
        .await
        .expect("local web proxy bridge check should succeed");
        assert!(rtt < Duration::from_secs(3));
        srv.abort();
    }
}
