//! Platform backend dispatch for `tg-webview`.
//!
//! Port of `Telegram/lib_webview/webview/platform/win/webview_win.cpp` (with non-Windows stubs).

use raw_window_handle::RawWindowHandle;
use tg_theme::Color;

use crate::error::{Availability, Result};
use crate::interface::{NavigationHistoryState, WebViewBounds, WebViewConfig};

#[cfg(windows)]
pub(crate) mod win;

#[cfg(windows)]
pub(crate) use win::PlatformWebView;

#[cfg(windows)]
pub(crate) fn availability() -> Availability {
    win::availability()
}

#[cfg(not(windows))]
pub(crate) fn availability() -> Availability {
    use crate::error::AvailabilityError;
    Availability {
        error: AvailabilityError::UnsupportedPlatform,
        custom_scheme_requests: false,
        custom_range_requests: false,
        custom_referer: false,
        browser_version: None,
        details: "WebView2 backend is only supported on Windows.".to_owned(),
    }
}

#[cfg(not(windows))]
pub(crate) struct PlatformWebView;

#[cfg(not(windows))]
impl PlatformWebView {
    pub(crate) fn create(_parent: Option<RawWindowHandle>, _config: WebViewConfig) -> Result<Self> {
        Err(crate::error::WebViewError::UnsupportedPlatform(
            "WebView2 backend is only supported on Windows.".to_owned(),
        ))
    }

    pub(crate) fn navigate(&self, _url: &str) -> Result<()> {
        Err(crate::error::WebViewError::Closed)
    }

    pub(crate) fn navigate_to_data(&self, _id: &str) -> Result<()> {
        Err(crate::error::WebViewError::Closed)
    }

    pub(crate) fn load_html(&self, _html: &str) -> Result<()> {
        Err(crate::error::WebViewError::Closed)
    }

    pub(crate) fn reload(&self) -> Result<()> {
        Err(crate::error::WebViewError::Closed)
    }

    pub(crate) fn init(&self, _js: &str) -> Result<()> {
        Err(crate::error::WebViewError::Closed)
    }

    pub(crate) fn eval(&self, _js: &str) -> Result<()> {
        Err(crate::error::WebViewError::Closed)
    }

    pub(crate) fn set_opaque_bg(&self, _color: Color) -> Result<()> {
        Err(crate::error::WebViewError::Closed)
    }

    pub(crate) fn set_bounds(&self, _bounds: WebViewBounds) -> Result<()> {
        Err(crate::error::WebViewError::Closed)
    }

    pub(crate) fn set_visible(&self, _visible: bool) -> Result<()> {
        Err(crate::error::WebViewError::Closed)
    }

    pub(crate) fn set_devtools_enabled(&self, _enabled: bool) -> Result<()> {
        Err(crate::error::WebViewError::Closed)
    }

    pub(crate) fn open_devtools(&self) -> Result<()> {
        Err(crate::error::WebViewError::Closed)
    }

    pub(crate) fn set_zoom(&self, _percent: u32) -> Result<()> {
        Err(crate::error::WebViewError::Closed)
    }

    pub(crate) fn zoom(&self) -> Result<u32> {
        Err(crate::error::WebViewError::Closed)
    }

    pub(crate) fn focus(&self) -> Result<()> {
        Err(crate::error::WebViewError::Closed)
    }

    pub(crate) fn navigation_history_state(&self) -> NavigationHistoryState {
        NavigationHistoryState::default()
    }

    pub(crate) fn close(&self) {}
}

#[allow(dead_code)]
fn _assert_imports_used(
    _: Option<RawWindowHandle>,
    _: Color,
    _: WebViewBounds,
    _: WebViewConfig,
    _: Result<NavigationHistoryState>,
) {
}
