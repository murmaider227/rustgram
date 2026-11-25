//! `tg-webview`: Embedded Microsoft Edge WebView2 and Telegram Mini App bridge for Rustgram.
//!
//! Port of `Telegram/lib_webview` (`webview_interface.h`, `webview_embed.cpp`,
//! `webview_data_stream*`, `webview_dialog.cpp`, `platform/win/webview_windows_edge_chromium.cpp`,
//! `platform/win/webview_windows_data_stream.cpp`) and the Mini App bridge protocol from
//! `Telegram/SourceFiles/ui/chat/attach/attach_bot_webview.cpp`.
//!
//! # Architecture
//! - [`availability`] / [`is_supported`]: Detects Microsoft Edge WebView2 Runtime availability.
//! - [`WebView`]: Embeds WebView2 as a child HWND inside any `raw-window-handle` parent window
//!   (such as `winit` / `eframe`), supporting custom-scheme resource routing (`rustgram-internal`),
//!   bidirectional JS bridge, navigation policy filtering, dark/light background & scrollbar
//!   theming, DevTools toggle, zoom, and workdir-scoped user-data storage.
//! - [`data_stream`]: Seekable [`DataStream`] / [`MemoryDataStream`], HTTP `Range` parsing, and
//!   platform-independent [`SchemeRouter`].
//! - [`bridge`]: Typed `serde` enums for incoming [`MiniAppEvent`] (`window.Telegram.WebView` /
//!   `window.TelegramWebviewProxy`) and outgoing [`HostEvent`] (`theme_changed`, `viewport_changed`,
//!   `main_button_pressed`, `back_button_pressed`, `popup_closed`, etc.).
//! - [`mini_app`]: Stateful [`MiniAppController`] implementing tdesktop's Mini App panel rules.

pub mod bridge;
pub mod data_stream;
pub mod dialog;
pub mod embed;
pub mod error;
pub mod interface;
pub mod mini_app;
mod platform;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tg_theme::{Color, WebViewThemeParams};

pub use bridge::{
    AccessRequestStatus, BiometryInfo, BiometryReasonParams, BiometryUpdateTokenParams,
    ClipboardReadParams, CloseParams, ClosingBehaviorParams, ColorChangeParams, ColorSpec,
    DataSendParams, EXTERNAL_MESSAGE_TYPE, FileDownloadParams, FileDownloadStatus,
    HapticFeedbackParams, HapticNotificationType, HomeScreenStatus, HostEvent, IframeReadyParams,
    ImpactStyle, InlineChatType, InvokeCustomMethodParams, MAX_NATIVE_MESSAGE_BYTES,
    MAX_WEB_PROXY_CARRIER_MESSAGE_BYTES, MainButtonParams, MiniAppEvent, MiniAppPopupButton,
    MiniAppPopupButtonType, NamedColorKey, NativeMessage, OpenInvoiceParams, OpenLinkParams,
    OpenPopupParams, OpenScanQrPopupParams, OpenTgLinkParams, OrientationLockParams,
    PhoneRequestStatus, PreparedMessageParams, RequestChatParams, SafeAreaInsets,
    SecondaryButtonParams, SecondaryButtonPosition, SensorStartParams, SetEmojiStatusParams,
    ShareToStoryParams, StorageClearParams, StorageGetKeyParams, StorageSaveKeyParams,
    StoryWidgetLink, SwipeBehaviorParams, SwitchInlineQueryParams, ThemeChangedPayload,
    VerifyAgeParams, ViewportParams, VisibilityParams, WEB_PROXY_STORAGE_TOKEN,
    WebProxyBridgeMessage, build_web_proxy_carrier_config, decode_mini_app_event,
    format_dynamic_viewport_script, format_post_event_script, format_raw_post_event_script,
    is_external_shell_origin, is_shell_namespace_command, mini_app_init_script,
    origin_check_script, origin_from_url, origins_match, parse_hex_color, parse_mini_app_event,
    parse_mini_app_event_with_shell, parse_native_message, parse_native_message_with_shell,
    parse_web_proxy_bridge_message, resolve_ripple_color, web_proxy_close_script,
    web_proxy_heartbeat_probe_script, web_proxy_init_script,
};
pub use data_stream::{
    DEFAULT_CUSTOM_SCHEME, DEFAULT_DATA_URL_PREFIX, DataRequest, DataRequestDone,
    DataRequestHandler, DataResponse, DataResult, DataStream, HttpResponse, LEGACY_DATA_URL_PREFIX,
    MemoryDataStream, PrefixRouteHandler, SchemeRouter, mime_from_path, parse_range_header,
    parse_range_header_for,
};
pub use dialog::{
    POPUPS_QUICKLY_DELAY, POPUPS_QUICKLY_LIMIT, PopupArgs, PopupButton, PopupButtonType,
    PopupFloodGuard, PopupResult, dialog_result_from_popup, dialog_to_popup_args,
};
pub use embed::{
    LEGACY_STORAGE_ID_TOKEN, RestrictedContent, STORAGE_TOKEN_LEN, WEBVIEW_STORAGE_DIR_NAME,
    generate_storage_token, is_allowed_navigation_scheme, is_allowed_restricted_url,
    make_restricted_content, resolve_user_data_dir, scrollbar_theme_scripts,
};
pub use error::{Availability, AvailabilityError, Result, WEBVIEW2_DOWNLOAD_URL, WebViewError};
pub use interface::{
    DialogArgs, DialogHandler, DialogResult, DialogType, ExternalLinkHandler,
    FullscreenChangedHandler, HistoryChangedHandler, InteractionHandler, MessageHandler,
    NavigationDoneHandler, NavigationHistoryState, NavigationPolicyHandler, NavigationStartHandler,
    PermissionHandler, PermissionType, ProcessFailedHandler, StorageId, WebMessage, WebViewBounds,
    WebViewCallbacks, WebViewConfig, WindowMode, WindowStyle, ZoomChangedHandler,
};
pub use mini_app::{
    CLIPBOARD_READ_TIMEOUT, MiniAppAction, MiniAppButtonState, MiniAppController,
    MiniAppPanelConfig, MiniAppPanelState, OPEN_LINK_TIMEOUT, telegram_link_from_path,
};

/// Queries whether the WebView2 runtime is installed and reports supported capabilities.
#[must_use]
pub fn availability() -> Availability {
    platform::availability()
}

/// Returns `true` when the host system has a working WebView2 runtime installed.
#[must_use]
pub fn is_supported() -> bool {
    availability().is_supported()
}

/// High-level embedded WebView handle (`Webview::Window` / `Webview::Interface`).
///
/// Wraps the platform WebView2 controller and provides navigation, JS bridge evaluation,
/// Mini App event delivery, scrollbar & background theming, DevTools toggle, and zoom control.
pub struct WebView {
    inner: platform::PlatformWebView,
}

impl WebView {
    /// Creates an embedded WebView as a child window of `parent` (any `winit`/`eframe` window
    /// implementing [`HasWindowHandle`]).
    pub fn create(parent: &impl HasWindowHandle, config: WebViewConfig) -> Result<Self> {
        let handle = parent
            .window_handle()
            .map_err(|e| WebViewError::InvalidWindowHandle(e.to_string()))?;
        Self::create_from_raw(Some(handle.as_raw()), config)
    }

    /// Creates a WebView from an explicit optional [`RawWindowHandle`] (pass `None` for
    /// [`WindowMode::Hidden`]).
    pub fn create_from_raw(parent: Option<RawWindowHandle>, config: WebViewConfig) -> Result<Self> {
        let inner = platform::PlatformWebView::create(parent, config)?;
        Ok(Self { inner })
    }

    /// Navigates the webview to `url`.
    pub fn navigate(&self, url: &str) -> Result<()> {
        self.inner.navigate(url)
    }

    /// Navigates to a resource `id` served by the configured custom scheme (`rustgram-internal://...`).
    pub fn navigate_to_data(&self, id: &str) -> Result<()> {
        self.inner.navigate_to_data(id)
    }

    /// Loads raw HTML directly into the document without a network request.
    pub fn load_html(&self, html: &str) -> Result<()> {
        self.inner.load_html(html)
    }

    /// Reloads the current document.
    pub fn reload(&self) -> Result<()> {
        self.inner.reload()
    }

    /// Registers a JavaScript snippet to execute on every new document creation before page scripts.
    pub fn init(&self, js: &str) -> Result<()> {
        self.inner.init(js)
    }

    /// Installs the Telegram Mini App JS bridge (`window.TelegramWebviewProxy` +
    /// `window.Telegram.WebView`) on document creation.
    pub fn install_mini_app_bridge(&self) -> Result<()> {
        self.init(mini_app_init_script())
    }

    /// Installs the hidden MTProto Web Proxy carrier JS bridge (`window.TelegramWebProxy`)
    /// on document creation.
    pub fn install_web_proxy_bridge(&self) -> Result<()> {
        self.init(web_proxy_init_script())
    }

    /// Evaluates JavaScript asynchronously in the active top-level document.
    pub fn eval(&self, js: &str) -> Result<()> {
        self.inner.eval(js)
    }

    /// Dispatches a typed [`HostEvent`] into the Mini App page.
    pub fn post_host_event(&self, event: &HostEvent, same_origin: Option<&str>) -> Result<()> {
        let script = format_post_event_script(event, same_origin);
        self.eval(&script)
    }

    /// Dispatches a `viewport_changed` event using `window.innerHeight` evaluated in the page.
    pub fn post_dynamic_viewport(&self, same_origin: Option<&str>) -> Result<()> {
        let script = format_dynamic_viewport_script(same_origin);
        self.eval(&script)
    }

    /// Applies theme colors (default background color + custom scrollbar CSS) from `tg-theme`.
    ///
    /// Port of `Webview::Window::updateTheme` (`webview_embed.cpp:205-256`).
    pub fn update_theme(&self, params: &WebViewThemeParams) -> Result<()> {
        let (init_js, eval_js) = scrollbar_theme_scripts(
            params.scroll_bg,
            params.scroll_bg_over,
            params.scroll_bar_bg,
            params.scroll_bar_bg_over,
        );
        self.init(&init_js)?;
        self.eval(&eval_js)?;
        self.set_opaque_bg(params.body_bg)
    }

    /// Sets the WebView2 default background color (e.g. dark/light palette `windowBg`).
    pub fn set_opaque_bg(&self, color: Color) -> Result<()> {
        self.inner.set_opaque_bg(color)
    }

    /// Moves and resizes the embedded child webview within the parent window's client area.
    pub fn set_bounds(&self, bounds: WebViewBounds) -> Result<()> {
        self.inner.set_bounds(bounds)
    }

    /// Shows or hides the embedded webview.
    pub fn set_visible(&self, visible: bool) -> Result<()> {
        self.inner.set_visible(visible)
    }

    /// Enables or disables browser DevTools and default context menus.
    pub fn set_devtools_enabled(&self, enabled: bool) -> Result<()> {
        self.inner.set_devtools_enabled(enabled)
    }

    /// Opens the WebView2 DevTools inspector window.
    pub fn open_devtools(&self) -> Result<()> {
        self.inner.open_devtools()
    }

    /// Sets the zoom factor as a percentage (e.g. `100` for 100%, clamped to `25..=500`).
    pub fn set_zoom(&self, percent: u32) -> Result<()> {
        self.inner.set_zoom(percent)
    }

    /// Returns the current zoom factor as a percentage (e.g. `100`).
    pub fn zoom(&self) -> Result<u32> {
        self.inner.zoom()
    }

    /// Moves keyboard focus into the embedded webview.
    pub fn focus(&self) -> Result<()> {
        self.inner.focus()
    }

    /// Returns the latest navigation history snapshot (URL, title, back/forward state).
    #[must_use]
    pub fn navigation_history_state(&self) -> NavigationHistoryState {
        self.inner.navigation_history_state()
    }

    /// Explicitly closes the underlying WebView2 controller.
    pub fn close(&self) {
        self.inner.close();
    }
}
