//! Live proxy controller, connection-state indicator bridge, reachability probing,
//! tdata persistence, and automatic proxy rotation for [`super::session::LiveSession`].
//!
//! Port origin:
//! - `Telegram/SourceFiles/boxes/connection_box.cpp` (`ProxiesBoxController`)
//! - `Telegram/SourceFiles/core/proxy_rotation_manager.cpp`
//! - `Telegram/SourceFiles/window/window_connecting_widget.cpp` (proxy status text)

use std::any::Any;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_api::RpcClient;
use tg_core::settings_proxy::{ProxyData as CoreProxyData, ProxySettingsMode, SettingsProxy};
use tg_session::{
    BridgeEvent, BridgeOutbound, ConnectionState, DEFAULT_PROXY_CHECK_TIMEOUT, MtprotoRpc,
    ProxyRotationRunner, SessionRuntime, WebProxyBridgeServer, WebProxyCheckTarget, WebProxyState,
    check_web_proxy_endpoint, core_proxy_to_transport, generate_bridge_nonce,
    load_proxy_settings_from_tdata, register_active_web_proxy_bridge, resolve_effective_proxy,
    save_proxy_settings_to_tdata, start_web_proxy_bridge, transport_proxy_to_core,
    unregister_active_web_proxy_bridge, web_proxy_bridge_url,
};
use tg_transport::ProxyType as TransportProxyType;
use tg_webview::{
    WebMessage, WebViewConfig, build_web_proxy_carrier_config, parse_web_proxy_bridge_message,
};
use tokio::runtime::Handle;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::bridge::LiveBridgeState;
use tg_app_core::settings::backend::ProxyCheckStatus;
use tg_app_core::stand_in::strings::Strings;
use tg_app_core::stand_in::types::ConnectionStatus;

pub use tg_app_core::core_types::misc::{
    ProxyIndicatorState, format_proxy_connection_status, proxy_indicator_state,
};

/// Maps `tg_session::ConnectionState` (`tg_mtproto::ConnectionState`) to `tg-app`'s
/// [`ConnectionStatus`].
#[must_use]
pub const fn map_mtproto_connection_state(state: ConnectionState) -> ConnectionStatus {
    match state {
        ConnectionState::Connected => ConnectionStatus::Connected,
        ConnectionState::Connecting | ConnectionState::Disconnected => ConnectionStatus::Connecting,
        ConnectionState::Waiting { retry_after_ms } => ConnectionStatus::Waiting {
            remaining_ms: if retry_after_ms > u32::MAX as u64 {
                u32::MAX
            } else {
                retry_after_ms as u32
            },
        },
    }
}

struct LiveProxyInner {
    settings: Mutex<SettingsProxy>,
    checks: Mutex<Vec<(CoreProxyData, ProxyCheckStatus)>>,
    tdata_path: Mutex<Option<PathBuf>>,
    rpc: Option<MtprotoRpc>,
    tokio_handle: Handle,
    bridge: Arc<LiveBridgeState>,
    rotation: Mutex<Option<ProxyRotationRunner>>,
    conn_watch_task: Mutex<Option<JoinHandle<()>>>,
    web_proxy_bridge: Mutex<Option<Arc<WebProxyBridgeServer>>>,
    web_proxy_check_target: Mutex<Option<WebProxyCheckTarget>>,
    carrier_outbound: Arc<Mutex<VecDeque<BridgeOutbound>>>,
}

impl Drop for LiveProxyInner {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.conn_watch_task.lock()
            && let Some(task) = guard.take()
        {
            task.abort();
        }
        if let Ok(mut rot) = self.rotation.lock() {
            *rot = None;
        }
        if let Ok(mut wp) = self.web_proxy_bridge.lock()
            && let Some(server) = wp.take()
        {
            stop_web_proxy_bridge(&server);
        }
    }
}

fn stop_web_proxy_bridge(server: &Arc<WebProxyBridgeServer>) {
    unregister_active_web_proxy_bridge(server);
    server.set_outbound_notifier(None);
    server.shutdown();
}

/// Live proxy manager owned by [`super::session::LiveSession`].
///
/// Transparently attaches to [`MtprotoRpc`] when present (via `Any` downcasting so
/// `LiveSession::new`'s signature stays unchanged), keeping `LiveBridgeState::connection_status`,
/// proxy health checks, automatic proxy rotation, and `tdata/settingss` persistence synchronized.
#[derive(Clone)]
pub struct LiveProxyController {
    inner: Arc<LiveProxyInner>,
}

impl LiveProxyController {
    /// Creates a [`LiveProxyController`] attached to `runtime` and `bridge`.
    #[must_use]
    pub fn new<C: RpcClient + Clone + 'static>(
        runtime: &SessionRuntime<C>,
        bridge: &Arc<LiveBridgeState>,
    ) -> Self {
        let mtproto_rpc = (runtime.rpc() as &dyn Any)
            .downcast_ref::<MtprotoRpc>()
            .cloned()
            .or_else(|| {
                (runtime.rpc() as &dyn Any)
                    .downcast_ref::<tg_app_core::core_types::erased_rpc::ErasedRpcClient>()
                    .and_then(|e| e.downcast_ref::<MtprotoRpc>())
                    .cloned()
            });

        let mut initial_settings = SettingsProxy::new();
        if let Some(ref rpc) = mtproto_rpc {
            let active_tp = rpc.proxy();
            if active_tp.valid() {
                let core_proxy = transport_proxy_to_core(&active_tp);
                initial_settings.add_to_list(core_proxy.clone());
                initial_settings.set_selected(core_proxy);
                initial_settings.set_settings(ProxySettingsMode::Enabled);
            }
            initial_settings.set_try_ipv6(rpc.client().try_ipv6());
        }

        let inner = Arc::new(LiveProxyInner {
            settings: Mutex::new(initial_settings.clone()),
            checks: Mutex::new(Vec::new()),
            tdata_path: Mutex::new(None),
            rpc: mtproto_rpc.clone(),
            tokio_handle: runtime.tokio_handle().clone(),
            bridge: Arc::clone(bridge),
            rotation: Mutex::new(None),
            conn_watch_task: Mutex::new(None),
            web_proxy_bridge: Mutex::new(None),
            web_proxy_check_target: Mutex::new(None),
            carrier_outbound: Arc::new(Mutex::new(VecDeque::new())),
        });

        if let Some(rpc) = mtproto_rpc {
            // Sync initial connection state from the live client.
            let initial_conn = rpc.connection_state();
            if initial_conn != ConnectionState::Disconnected {
                bridge.set_connection_status(map_mtproto_connection_state(initial_conn));
            }

            // Watch live MTProto main-DC connection state transitions into LiveBridgeState.
            let mut state_rx = rpc.subscribe_connection_state();
            let bridge_bg = Arc::clone(bridge);
            let watch_task = runtime.tokio_handle().spawn(async move {
                loop {
                    match state_rx.recv().await {
                        Ok(state) => {
                            bridge_bg.set_connection_status(map_mtproto_connection_state(state));
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            bridge_bg.notify_ui();
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
            if let Ok(mut guard) = inner.conn_watch_task.lock() {
                *guard = Some(watch_task);
            }

            // Start ProxyRotationRunner synchronized with our SettingsProxy + tdata persistence.
            let inner_for_rot = Arc::downgrade(&inner);
            let runner = ProxyRotationRunner::spawn_for_rpc_owned(
                runtime.tokio_handle(),
                &rpc,
                initial_settings,
                Some(Arc::new(move |updated_settings, _target| {
                    if let Some(strong) = inner_for_rot.upgrade() {
                        if let Ok(mut s) = strong.settings.lock() {
                            *s = updated_settings.clone();
                        }
                        if let Ok(path_guard) = strong.tdata_path.lock()
                            && let Some(tdata) = path_guard.as_ref()
                            && let Err(err) = save_proxy_settings_to_tdata(tdata, updated_settings)
                        {
                            warn!(error = %err, "failed to persist rotated proxy settings");
                        }
                        strong.bridge.notify_ui();
                    }
                })),
            );
            if let Ok(mut rot) = inner.rotation.lock() {
                *rot = Some(runner);
            }
        }

        let controller = Self { inner };
        let initial_effective = resolve_effective_proxy(&controller.proxy_settings());
        controller.sync_web_proxy_bridge(&initial_effective);
        controller
    }

    /// Returns `true` when backed by a live [`MtprotoRpc`] client.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.inner.rpc.is_some()
    }

    /// Attaches a `tdata` directory path for reading/writing `tdata/settingss`, immediately
    /// loading any previously persisted proxy settings and applying them to the live client.
    pub fn attach_tdata_path(&self, tdata_path: PathBuf) -> Result<SettingsProxy, String> {
        if let Ok(mut guard) = self.inner.tdata_path.lock() {
            *guard = Some(tdata_path.clone());
        }
        match load_proxy_settings_from_tdata(&tdata_path) {
            Ok(loaded) => {
                self.apply_settings_internal(&loaded, false)?;
                Ok(loaded)
            }
            Err(err) => {
                debug!(error = %err, "no existing tdata proxy settings loaded; keeping current");
                Ok(self.proxy_settings())
            }
        }
    }

    /// Returns the attached `tdata` directory path, if [`Self::attach_tdata_path`] was called.
    #[must_use]
    pub fn tdata_path(&self) -> Option<PathBuf> {
        self.inner.tdata_path.lock().ok().and_then(|g| g.clone())
    }

    /// Returns the current [`SettingsProxy`] snapshot.
    #[must_use]
    pub fn proxy_settings(&self) -> SettingsProxy {
        self.inner
            .settings
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Applies updated proxy settings: updates local state, rotation manager, live MTProto
    /// transport (reconnecting all DCs without losing session), and `tdata/settingss`.
    pub fn apply_proxy_settings(&self, next: &SettingsProxy) -> Result<(), String> {
        self.apply_settings_internal(next, true)
    }

    fn apply_settings_internal(&self, next: &SettingsProxy, persist: bool) -> Result<(), String> {
        if let Ok(mut guard) = self.inner.settings.lock() {
            *guard = next.clone();
        }
        if let Ok(rot) = self.inner.rotation.lock()
            && let Some(runner) = rot.as_ref()
        {
            runner.update_settings(next.clone());
        }
        let effective = resolve_effective_proxy(next);
        self.sync_web_proxy_bridge(&effective);

        if let Some(ref rpc) = self.inner.rpc {
            let proxy_changed = rpc.proxy() != effective;
            let ipv6_changed = rpc.client().try_ipv6() != next.try_ipv6();
            if ipv6_changed {
                rpc.set_try_ipv6(next.try_ipv6());
            }
            if proxy_changed {
                rpc.set_proxy(effective);
            }
            if proxy_changed || ipv6_changed {
                self.inner
                    .bridge
                    .set_connection_status(ConnectionStatus::Connecting);
            }
        }
        if persist
            && let Ok(path_guard) = self.inner.tdata_path.lock()
            && let Some(tdata) = path_guard.as_ref()
        {
            save_proxy_settings_to_tdata(tdata, next)?;
        }
        self.inner.bridge.notify_ui();
        Ok(())
    }

    /// Returns `true` when custom or system proxy mode is currently enabled.
    #[must_use]
    pub fn is_proxy_enabled(&self) -> bool {
        let s = self.proxy_settings();
        s.is_enabled() || s.is_system()
    }

    /// Toggles custom proxy usage on/off (used by shell `Backend::set_proxy_enabled`).
    pub fn set_proxy_enabled(&self, enabled: bool) {
        let mut s = self.proxy_settings();
        if enabled {
            if !s.selected().is_valid()
                && let Some(first) = s.list().iter().find(|p| p.is_valid()).cloned()
            {
                s.set_selected(first);
            }
            if s.selected().is_valid() {
                s.set_settings(ProxySettingsMode::Enabled);
            } else {
                s.set_settings(ProxySettingsMode::System);
            }
        } else {
            s.set_settings(ProxySettingsMode::Disabled);
        }
        if let Err(err) = self.apply_proxy_settings(&s) {
            warn!(error = %err, "failed to toggle proxy enabled state");
        }
    }

    /// Triggers an immediate reconnect on all live MTProto sessions ("Try now").
    pub fn retry_connection(&self) {
        if let Some(ref rpc) = self.inner.rpc {
            self.inner
                .bridge
                .set_connection_status(ConnectionStatus::Connecting);
            rpc.restart_all_sessions();
        }
    }

    /// Spawns a real asynchronous reachability/ping check for `proxy` via `tg-session`.
    pub fn check_proxy(&self, proxy: &CoreProxyData) {
        self.check_proxy_with_timeout(proxy, DEFAULT_PROXY_CHECK_TIMEOUT);
    }

    /// Returns the most recent [`ProxyCheckStatus`] recorded for `proxy`.
    #[must_use]
    pub fn proxy_check_status(&self, proxy: &CoreProxyData) -> ProxyCheckStatus {
        self.inner
            .checks
            .lock()
            .ok()
            .and_then(|list| {
                list.iter()
                    .find(|(p, _)| p == proxy)
                    .map(|(_, s)| s.clone())
            })
            .unwrap_or(ProxyCheckStatus::NotTested)
    }

    /// Computes the current [`ProxyIndicatorState`] for this live session.
    #[must_use]
    pub fn indicator_state(&self, strings: &dyn Strings) -> ProxyIndicatorState {
        let status = self.inner.bridge.connection_status();
        let proxy = self.proxy_settings();
        proxy_indicator_state(status, &proxy, strings)
    }

    /// Sets an optional override endpoint target for checking `ProxyType::Web` proxies
    /// (used by tests targeting local fake WebProxy servers without real network access).
    pub fn set_web_proxy_check_target(&self, target: Option<WebProxyCheckTarget>) {
        if let Ok(mut guard) = self.inner.web_proxy_check_target.lock() {
            *guard = target;
        }
    }

    /// Returns the currently active local [`WebProxyBridgeServer`], if a Web Proxy is selected.
    #[must_use]
    pub fn web_proxy_bridge(&self) -> Option<Arc<WebProxyBridgeServer>> {
        self.inner
            .web_proxy_bridge
            .lock()
            .ok()
            .and_then(|g| g.clone())
    }

    /// Returns the current [`WebProxyState`] of the active Web Proxy bridge (`Idle` if none).
    #[must_use]
    pub fn web_proxy_state(&self) -> WebProxyState {
        self.web_proxy_bridge()
            .map_or(WebProxyState::Idle, |s| s.state())
    }

    /// Returns the canonical remote HTTPS bridge URL (`https://<host><path>?bridge=<cap>`) for
    /// the active Web Proxy, if any.
    #[must_use]
    pub fn web_proxy_remote_bridge_url(&self) -> Option<String> {
        let effective = resolve_effective_proxy(&self.proxy_settings());
        if effective.proxy_type != TransportProxyType::Web {
            return None;
        }
        web_proxy_bridge_url(&effective).ok()
    }

    /// Returns the bound local loopback bridge URL (`http://127.0.0.1:<port>/`) when a Web Proxy
    /// bridge server is running.
    #[must_use]
    pub fn web_proxy_local_bridge_url(&self) -> Option<String> {
        self.web_proxy_bridge().map(|s| s.bridge_page_url())
    }

    /// Initializes the hidden WebView carrier for the active Web Proxy bridge and returns
    /// `(bridge_url, WebViewConfig)` wired through `tg-webview` -> `tg-mtproto::web_proxy`.
    ///
    /// Pass `nonce = None` to generate a fresh 32-byte random nonce automatically.
    pub fn prepare_web_proxy_webview_carrier(
        &self,
        nonce: Option<String>,
    ) -> Result<(String, WebViewConfig), String> {
        let server = self
            .web_proxy_bridge()
            .ok_or_else(|| "No active Web Proxy bridge server".to_owned())?;
        let nonce_val = nonce.unwrap_or_else(generate_bridge_nonce);
        let bridge_url = server
            .attach_webview_carrier(nonce_val)
            .map_err(|e| e.to_string())?;
        let host = server.proxy().host.clone();
        let tdata_dir = self.inner.tdata_path.lock().ok().and_then(|g| g.clone());
        if let Ok(mut queue) = self.inner.carrier_outbound.lock() {
            queue.clear();
        }
        let server_for_cb = Arc::clone(&server);
        let bridge_for_cb = Arc::clone(&self.inner.bridge);
        let outbound_for_cb = Arc::clone(&self.inner.carrier_outbound);
        let on_message = Arc::new(move |msg: WebMessage| {
            if let Err(err) = parse_web_proxy_bridge_message(&msg) {
                warn!(error = %err, "rejected malformed tg-webview carrier message");
                return;
            }
            match server_for_cb.feed_webview_message(&msg.text, &msg.source_url) {
                Ok(events) => {
                    let mut ready = false;
                    if let Ok(mut queue) = outbound_for_cb.lock() {
                        for ev in events {
                            match ev {
                                BridgeEvent::Eval(out) => queue.push_back(out),
                                BridgeEvent::Ready => ready = true,
                                _ => {}
                            }
                        }
                    }
                    if ready {
                        bridge_for_cb.set_connection_status(ConnectionStatus::Connected);
                    }
                    bridge_for_cb.notify_ui();
                }
                Err(err) => {
                    warn!(error = %err, "web proxy webview carrier message failed");
                }
            }
        });
        let cfg = build_web_proxy_carrier_config(&host, &bridge_url, tdata_dir, on_message, None);
        Ok((bridge_url, cfg))
    }

    /// Routes a [`tg_webview::WebMessage`] from a hidden WebView carrier (or test harness)
    /// into the active [`WebProxyBridgeServer`] and returns resulting [`BridgeEvent`]s.
    pub fn route_web_proxy_carrier_message(
        &self,
        msg: &WebMessage,
    ) -> Result<Vec<BridgeEvent>, String> {
        let _parsed = parse_web_proxy_bridge_message(msg).map_err(|e| e.to_string())?;
        let server = self
            .web_proxy_bridge()
            .ok_or_else(|| "No active Web Proxy bridge server".to_owned())?;
        let events = server
            .feed_webview_message(&msg.text, &msg.source_url)
            .map_err(|e| e.to_string())?;
        if events.iter().any(|ev| matches!(ev, BridgeEvent::Ready)) {
            self.inner
                .bridge
                .set_connection_status(ConnectionStatus::Connected);
        }
        Ok(events)
    }

    /// Drains any queued outbound WebView eval commands from the active Web Proxy bridge,
    /// including the ones produced while handling carrier messages, in sequence order.
    #[must_use]
    pub fn drain_web_proxy_carrier_outbound(&self) -> Vec<BridgeOutbound> {
        let mut out: Vec<BridgeOutbound> = self
            .inner
            .carrier_outbound
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default();
        if let Some(server) = self.web_proxy_bridge() {
            out.extend(server.drain_webview_outbound());
        }
        out
    }

    fn sync_web_proxy_bridge(&self, effective: &tg_transport::ProxyData) {
        if effective.proxy_type != TransportProxyType::Web || !effective.valid() {
            if let Ok(mut guard) = self.inner.web_proxy_bridge.lock()
                && let Some(prev) = guard.take()
            {
                stop_web_proxy_bridge(&prev);
            }
            return;
        }
        if let Ok(guard) = self.inner.web_proxy_bridge.lock()
            && let Some(existing) = guard.as_ref()
            && existing.proxy() == effective
        {
            return;
        }
        let proxy_clone = effective.clone();
        let bind_res = crate::media::block_on_runtime(&self.inner.tokio_handle, async move {
            start_web_proxy_bridge(&proxy_clone).await
        });
        match bind_res {
            Ok(server) => {
                let server = Arc::new(server);
                let bridge = Arc::downgrade(&self.inner.bridge);
                server.set_outbound_notifier(Some(Arc::new(move || {
                    if let Some(bridge) = bridge.upgrade() {
                        bridge.notify_ui();
                    }
                })));
                register_active_web_proxy_bridge(&server);
                if let Ok(mut guard) = self.inner.web_proxy_bridge.lock()
                    && let Some(prev) = guard.replace(server)
                {
                    stop_web_proxy_bridge(&prev);
                }
            }
            Err(err) => {
                warn!(error = %err, "failed to bind local Web Proxy bridge server");
            }
        }
    }

    /// Convenience helper for tests to override check timeout or run a check directly with custom timeout.
    pub fn check_proxy_with_timeout(&self, proxy: &CoreProxyData, timeout: Duration) {
        self.set_check_status(proxy, ProxyCheckStatus::Checking);
        let try_ipv6 = self.proxy_settings().try_ipv6();
        let proxy_clone = proxy.clone();
        let inner = Arc::clone(&self.inner);
        let tp = core_proxy_to_transport(&proxy_clone);

        if tp.proxy_type == TransportProxyType::Web {
            let target = self
                .inner
                .web_proxy_check_target
                .lock()
                .ok()
                .and_then(|g| g.clone())
                .unwrap_or(WebProxyCheckTarget::DefaultHttps);
            self.inner.tokio_handle.spawn(async move {
                let outcome = check_web_proxy_endpoint(&tp, &target, timeout).await;
                let status = match outcome {
                    Ok(rtt) => ProxyCheckStatus::Available {
                        ping_ms: rtt.as_millis().min(u128::from(u32::MAX)) as u32,
                    },
                    Err(reason) => ProxyCheckStatus::Unavailable { reason },
                };
                set_check_status_on(&inner, &proxy_clone, status);
            });
            return;
        }

        if let Some(rpc) = self.inner.rpc.clone() {
            self.inner.tokio_handle.spawn(async move {
                let outcome = rpc.check_proxy(&tp, try_ipv6, timeout).await;
                let status = match outcome {
                    Ok(rtt) => ProxyCheckStatus::Available {
                        ping_ms: rtt.as_millis().min(u128::from(u32::MAX)) as u32,
                    },
                    Err(reason) => ProxyCheckStatus::Unavailable { reason },
                };
                set_check_status_on(&inner, &proxy_clone, status);
            });
        } else {
            // Non-MtprotoRpc test harness fallback: validate proxy syntax synchronously.
            let status = if proxy.is_valid() {
                ProxyCheckStatus::Available { ping_ms: 1 }
            } else {
                ProxyCheckStatus::Unavailable {
                    reason: "invalid proxy configuration".to_owned(),
                }
            };
            self.set_check_status(proxy, status);
        }
    }

    fn set_check_status(&self, proxy: &CoreProxyData, status: ProxyCheckStatus) {
        set_check_status_on(&self.inner, proxy, status);
    }
}

fn set_check_status_on(inner: &LiveProxyInner, proxy: &CoreProxyData, status: ProxyCheckStatus) {
    if let Ok(mut list) = inner.checks.lock() {
        if let Some((_, slot)) = list.iter_mut().find(|(p, _)| p == proxy) {
            *slot = status;
        } else {
            list.push((proxy.clone(), status));
        }
    }
    inner.bridge.notify_ui();
}
