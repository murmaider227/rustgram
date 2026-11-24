//! Hidden WebView carrier for the MTProto Web Proxy, owned by the live window.
//!
//! Port of `WebviewCarrier` (`Telegram/SourceFiles/mtproto/web_proxy/web_proxy_webview.cpp`):
//! when the selected proxy is a Web proxy, a hidden WebView loads the proxy's bridge page,
//! its `window.TelegramWebProxy` messages feed the local bridge server, and the frames the
//! bridge queues for the relay are evaluated back in that WebView.

use std::sync::Arc;

use tg_session::WebProxyBridgeServer;
use tg_webview::{WebView, WebViewConfig};
use tracing::warn;

use crate::proxy::LiveProxyController;

/// A created carrier WebView (real WebView2 or a test fake).
pub trait WebProxyCarrierView {
    /// Evaluates a bridge script in the carrier page.
    fn eval(&self, script: &str) -> Result<(), String>;
    /// Closes the carrier.
    fn close(&self);
}

/// Creates carrier WebViews; the native one uses a hidden WebView2 instance.
pub trait WebProxyCarrierFactory {
    /// Creates a hidden carrier for `config`, installs the bridge script and loads `url`.
    fn create(
        &self,
        url: &str,
        config: WebViewConfig,
    ) -> Result<Box<dyn WebProxyCarrierView>, String>;
}

/// Hidden WebView2 carrier factory.
pub struct NativeWebProxyCarrierFactory;

struct NativeCarrierView(WebView);

impl WebProxyCarrierView for NativeCarrierView {
    fn eval(&self, script: &str) -> Result<(), String> {
        self.0.eval(script).map_err(|e| e.to_string())
    }

    fn close(&self) {
        self.0.close();
    }
}

impl WebProxyCarrierFactory for NativeWebProxyCarrierFactory {
    fn create(
        &self,
        url: &str,
        config: WebViewConfig,
    ) -> Result<Box<dyn WebProxyCarrierView>, String> {
        if !tg_webview::is_supported() {
            return Err("WebView2 runtime is not available".to_owned());
        }
        let view = WebView::create_from_raw(None, config).map_err(|e| e.to_string())?;
        view.install_web_proxy_bridge().map_err(|e| e.to_string())?;
        view.navigate(url).map_err(|e| e.to_string())?;
        Ok(Box::new(NativeCarrierView(view)))
    }
}

struct ActiveCarrier {
    server: Arc<WebProxyBridgeServer>,
    view: Box<dyn WebProxyCarrierView>,
}

/// Keeps one hidden carrier per active Web proxy bridge and pumps its outbound scripts.
pub struct WebProxyCarrierHost {
    factory: Box<dyn WebProxyCarrierFactory>,
    active: Option<ActiveCarrier>,
    failed_for: Option<Arc<WebProxyBridgeServer>>,
    last_error: Option<String>,
}

impl Default for WebProxyCarrierHost {
    fn default() -> Self {
        Self::new()
    }
}

impl WebProxyCarrierHost {
    /// Creates a host that uses hidden WebView2 carriers.
    #[must_use]
    pub fn new() -> Self {
        Self::with_factory(Box::new(NativeWebProxyCarrierFactory))
    }

    /// Creates a host with a custom carrier factory (tests use a fake).
    #[must_use]
    pub fn with_factory(factory: Box<dyn WebProxyCarrierFactory>) -> Self {
        Self {
            factory,
            active: None,
            failed_for: None,
            last_error: None,
        }
    }

    /// Returns `true` while a carrier is open.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// Returns the last carrier creation or eval error.
    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Opens, replaces or closes the carrier to match `proxy`'s active bridge, then evaluates
    /// every queued outbound bridge script. Returns `true` while a carrier is open.
    pub fn pump(&mut self, proxy: &LiveProxyController) -> bool {
        let Some(server) = proxy.web_proxy_bridge() else {
            self.close();
            self.failed_for = None;
            return false;
        };
        let current = self
            .active
            .as_ref()
            .is_some_and(|a| Arc::ptr_eq(&a.server, &server));
        if !current {
            self.close();
            let failed_before = self
                .failed_for
                .as_ref()
                .is_some_and(|f| Arc::ptr_eq(f, &server));
            if failed_before {
                return false;
            }
            match proxy
                .prepare_web_proxy_webview_carrier(None)
                .and_then(|(url, config)| self.factory.create(&url, config))
            {
                Ok(view) => {
                    self.failed_for = None;
                    self.last_error = None;
                    self.active = Some(ActiveCarrier { server, view });
                }
                Err(err) => {
                    warn!(error = %err, "failed to open the Web proxy carrier");
                    self.last_error = Some(err);
                    self.failed_for = Some(server);
                    return false;
                }
            }
        }
        if let Some(active) = self.active.as_ref() {
            for out in proxy.drain_web_proxy_carrier_outbound() {
                if let Err(err) = active.view.eval(&out.script) {
                    warn!(error = %err, "Web proxy carrier eval failed");
                    self.last_error = Some(err);
                }
            }
        }
        true
    }

    /// Closes the carrier, if one is open.
    pub fn close(&mut self) {
        if let Some(active) = self.active.take() {
            active.view.eval(tg_webview::web_proxy_close_script()).ok();
            active.view.close();
        }
    }
}

impl Drop for WebProxyCarrierHost {
    fn drop(&mut self) {
        self.close();
    }
}
