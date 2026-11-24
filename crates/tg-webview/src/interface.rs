//! Core webview interfaces, configuration structs, and callback types.
//!
//! Port of `Telegram/lib_webview/webview/webview_interface.h` and
//! `Telegram/lib_webview/webview/webview_common.h`.

use std::path::PathBuf;
use std::sync::Arc;

use tg_theme::Color;

use crate::data_stream::{DEFAULT_CUSTOM_SCHEME, DataRequestHandler, SchemeRouter};

/// Presentation mode of a webview window (`Webview::WindowMode`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WindowMode {
    /// Embedded as a child window inside a parent HWND (default for Mini Apps & IV).
    #[default]
    Embedded,
    /// Standalone external top-level window.
    External,
    /// Hidden off-screen instance (e.g. background restricted script execution).
    Hidden,
}

/// Window frame style (`Webview::WindowStyle`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WindowStyle {
    /// Standard OS window chrome.
    #[default]
    Default,
    /// Frameless window with custom chrome.
    Frameless,
}

/// Persistent user-data profile identifier (`Webview::StorageId`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageId {
    /// Filesystem directory storing the WebView2 user-data folder.
    pub path: PathBuf,
    /// Account/profile isolation token (16 random bytes or [`crate::LEGACY_STORAGE_ID_TOKEN`]).
    pub token: Vec<u8>,
}

impl StorageId {
    /// Returns `true` when both `path` and `token` are non-empty.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self.path.as_os_str().is_empty() && !self.token.is_empty()
    }
}

/// Rectangle in parent client coordinates (physical pixels) for an embedded child webview.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WebViewBounds {
    /// Left edge relative to parent client area.
    pub x: i32,
    /// Top edge relative to parent client area.
    pub y: i32,
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
}

impl WebViewBounds {
    /// Creates a new bounds rectangle.
    #[must_use]
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// Snapshot of navigation history and current document metadata (`Webview::NavigationHistoryState`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NavigationHistoryState {
    /// Current document URL.
    pub url: String,
    /// Current document `<title>`.
    pub title: String,
    /// Whether `history.back()` is available.
    pub can_go_back: bool,
    /// Whether `history.forward()` is available.
    pub can_go_forward: bool,
}

/// Kind of JavaScript modal dialog (`Webview::DialogType`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DialogType {
    /// `window.alert(text)`
    #[default]
    Alert,
    /// `window.confirm(text)`
    Confirm,
    /// `window.prompt(text, defaultValue)`
    Prompt,
}

/// Arguments passed to a JavaScript dialog handler (`Webview::DialogArgs`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DialogArgs {
    /// Kind of dialog requested by the page.
    pub dialog_type: DialogType,
    /// Default input text for [`DialogType::Prompt`].
    pub value: String,
    /// Message text passed by the page.
    pub text: String,
    /// Originating page URL.
    pub url: String,
}

/// Result returned to the browser from a JavaScript dialog handler (`Webview::DialogResult`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DialogResult {
    /// Entered text for [`DialogType::Prompt`].
    pub text: String,
    /// Whether the user accepted (clicked OK) rather than dismissed/cancelled.
    pub accepted: bool,
}

/// Device permission kind requested by web content (`Webview::PermissionType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionType {
    /// Audio capture / microphone access.
    Microphone,
    /// Video capture / camera access.
    Camera,
    /// Combined camera and microphone access.
    CameraAndMicrophone,
    /// Geolocation API access.
    Geolocation,
}

/// Web message sent from JavaScript via `window.chrome.webview.postMessage` (`Webview::Message`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebMessage {
    /// UTF-8 message string sent by the page.
    pub text: String,
    /// Source document URL reported by WebView2.
    pub source_url: String,
}

/// Callback invoked when JavaScript posts a message to the host.
pub type MessageHandler = Arc<dyn Fn(WebMessage) + Send + Sync>;

/// Callback deciding whether navigation to `(uri, is_new_window)` is permitted.
pub type NavigationPolicyHandler = Arc<dyn Fn(&str, bool) -> bool + Send + Sync>;

/// Callback invoked when navigation starts.
pub type NavigationStartHandler = Arc<dyn Fn() + Send + Sync>;

/// Callback invoked when navigation finishes (`success`).
pub type NavigationDoneHandler = Arc<dyn Fn(bool) + Send + Sync>;

/// Callback invoked when a user-initiated popup link (`window.open` / `target=_blank`) should be
/// opened externally after passing navigation policy.
pub type ExternalLinkHandler = Arc<dyn Fn(&str) + Send + Sync>;

/// Callback handling JavaScript `alert`/`confirm`/`prompt` dialogs.
pub type DialogHandler = Arc<dyn Fn(DialogArgs) -> DialogResult + Send + Sync>;

/// Async callback handling media/device permission requests (`done(allowed)`).
pub type PermissionHandler =
    Arc<dyn Fn(PermissionType, Box<dyn FnOnce(bool) + Send>) + Send + Sync>;

/// Callback invoked when the browser zoom percentage changes (e.g. `100` for 100%).
pub type ZoomChangedHandler = Arc<dyn Fn(u32) + Send + Sync>;

/// Callback invoked when URL, title, or back/forward availability changes.
pub type HistoryChangedHandler = Arc<dyn Fn(NavigationHistoryState) + Send + Sync>;

/// Callback invoked when the user clicks or presses a key inside the webview.
pub type InteractionHandler = Arc<dyn Fn() + Send + Sync>;

/// Callback invoked when a webview element enters or exits fullscreen.
pub type FullscreenChangedHandler = Arc<dyn Fn(bool) + Send + Sync>;

/// Callback invoked when the browser renderer process crashes or fails.
pub type ProcessFailedHandler = Arc<dyn Fn() + Send + Sync>;

/// Event callbacks wired into a [`WebViewConfig`].
#[derive(Default, Clone)]
pub struct WebViewCallbacks {
    /// Invoked on `window.chrome.webview.postMessage` from the main frame.
    pub on_message: Option<MessageHandler>,
    /// Invoked before navigation (`is_new_window == false`) or popup (`is_new_window == true`).
    pub on_navigation_policy: Option<NavigationPolicyHandler>,
    /// Invoked when a navigation begins.
    pub on_navigation_start: Option<NavigationStartHandler>,
    /// Invoked when a navigation completes (`true` on success).
    pub on_navigation_done: Option<NavigationDoneHandler>,
    /// Invoked to open an approved `target=_blank` / `window.open` URL in the OS browser.
    pub on_external_link: Option<ExternalLinkHandler>,
    /// Invoked on JS `alert`/`confirm`/`prompt`.
    pub on_dialog: Option<DialogHandler>,
    /// Invoked on camera/microphone permission requests.
    pub on_permission: Option<PermissionHandler>,
    /// Fallback handler for custom-scheme data requests not matched by [`WebViewConfig::scheme_router`].
    pub on_data_request: Option<DataRequestHandler>,
    /// Invoked when the zoom factor changes (percentage, e.g. `100`).
    pub on_zoom_changed: Option<ZoomChangedHandler>,
    /// Invoked when navigation history/title/URL updates.
    pub on_history_changed: Option<HistoryChangedHandler>,
    /// Invoked on user mouse/keyboard interaction inside the webview.
    pub on_interaction: Option<InteractionHandler>,
    /// Invoked when DOM fullscreen state changes.
    pub on_fullscreen_changed: Option<FullscreenChangedHandler>,
    /// Invoked when the WebView2 browser/renderer process fails.
    pub on_process_failed: Option<ProcessFailedHandler>,
}

impl std::fmt::Debug for WebViewCallbacks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebViewCallbacks")
            .field("on_message", &self.on_message.is_some())
            .field("on_navigation_policy", &self.on_navigation_policy.is_some())
            .field("on_navigation_start", &self.on_navigation_start.is_some())
            .field("on_navigation_done", &self.on_navigation_done.is_some())
            .field("on_external_link", &self.on_external_link.is_some())
            .field("on_dialog", &self.on_dialog.is_some())
            .field("on_permission", &self.on_permission.is_some())
            .field("on_data_request", &self.on_data_request.is_some())
            .field("on_zoom_changed", &self.on_zoom_changed.is_some())
            .field("on_history_changed", &self.on_history_changed.is_some())
            .field("on_interaction", &self.on_interaction.is_some())
            .field(
                "on_fullscreen_changed",
                &self.on_fullscreen_changed.is_some(),
            )
            .field("on_process_failed", &self.on_process_failed.is_some())
            .finish()
    }
}

/// Configuration for creating a WebView instance (`Webview::Config` / `Webview::WindowConfig`).
#[derive(Debug, Clone)]
pub struct WebViewConfig {
    /// Background color painted before page content renders (supports dark/light/transparent).
    pub opaque_bg: Color,
    /// Persistent user-data directory (should reside under the Rustgram working directory).
    pub user_data_path: Option<PathBuf>,
    /// Optional storage isolation token.
    pub user_data_token: Vec<u8>,
    /// Enables DevTools and default browser context menu when `true` (and not restricted).
    pub debug: bool,
    /// Safe browsing mode flag (e.g. for Instant View / TON Sites).
    pub safe: bool,
    /// Whether third-party cookies are permitted.
    pub allow_third_party_cookies: bool,
    /// Embedded, external, or hidden window presentation mode.
    pub mode: WindowMode,
    /// Window chrome style.
    pub window_style: WindowStyle,
    /// Initial bounds inside the parent window (`None` fills the parent client area).
    pub initial_bounds: Option<WebViewBounds>,
    /// Custom internal URL scheme name (default: `"rustgram-internal"`).
    pub custom_scheme: String,
    /// Optional router serving local resources for [`Self::custom_scheme`].
    pub scheme_router: Option<Arc<SchemeRouter>>,
    /// Optional strict `https://host` origin restriction (enables InPrivate mode, CSP lockdown,
    /// request blocking, muted audio, and disabled SmartScreen).
    pub restricted_origin: Option<String>,
    /// Event callbacks.
    pub callbacks: WebViewCallbacks,
}

impl Default for WebViewConfig {
    fn default() -> Self {
        Self {
            opaque_bg: Color::WHITE,
            user_data_path: None,
            user_data_token: Vec::new(),
            debug: false,
            safe: false,
            allow_third_party_cookies: false,
            mode: WindowMode::Embedded,
            window_style: WindowStyle::Default,
            initial_bounds: None,
            custom_scheme: DEFAULT_CUSTOM_SCHEME.to_owned(),
            scheme_router: None,
            restricted_origin: None,
            callbacks: WebViewCallbacks::default(),
        }
    }
}
