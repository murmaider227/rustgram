//! Error types and WebView2 runtime availability detection.
//!
//! Port of `Telegram/lib_webview/webview/webview_interface.h` (`Available`),
//! `Telegram/lib_webview/webview/platform/win/webview_win.cpp` (`Availability`), and
//! `Telegram/SourceFiles/ui/chat/attach/attach_bot_webview.cpp` (`ErrorText`).

use thiserror::Error;

/// Official Microsoft Edge WebView2 Evergreen Bootstrapper download URL used by tdesktop.
pub const WEBVIEW2_DOWNLOAD_URL: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";

/// Reason why the system webview backend is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvailabilityError {
    /// A supported webview backend is installed and ready.
    None,
    /// Microsoft Edge WebView2 Runtime is not installed on Windows.
    NoWebView2,
    /// WebKitGTK is not installed on Linux.
    NoWebKitGtk,
    /// Windows version is older than Windows 8.1.
    OldWindows,
    /// Current operating system has no webview backend implemented in this crate.
    UnsupportedPlatform,
}

/// Capabilities and availability status of the host system's webview runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Availability {
    /// Availability error (`AvailabilityError::None` when supported).
    pub error: AvailabilityError,
    /// Whether custom scheme / local resource interception is supported.
    pub custom_scheme_requests: bool,
    /// Whether HTTP `Range:` partial-content requests are supported on custom resources.
    pub custom_range_requests: bool,
    /// Whether custom `Referer` headers are supported.
    pub custom_referer: bool,
    /// Detected browser runtime version string (e.g. `"131.0.2903.86"` on WebView2).
    pub browser_version: Option<String>,
    /// Human-readable error or diagnostic detail.
    pub details: String,
}

impl Availability {
    /// Returns `true` when the runtime is available without errors.
    #[must_use]
    pub fn is_supported(&self) -> bool {
        self.error == AvailabilityError::None
    }

    /// Returns a user-facing guidance message describing how to resolve an availability error.
    #[must_use]
    pub fn user_error_text(&self) -> Option<String> {
        match self.error {
            AvailabilityError::None => None,
            AvailabilityError::NoWebView2 => Some(format!(
                "Microsoft Edge WebView2 Runtime is required to display this content. \
                 Download it from {WEBVIEW2_DOWNLOAD_URL}"
            )),
            AvailabilityError::NoWebKitGtk => {
                Some("Please install WebKitGTK to display this content.".to_owned())
            }
            AvailabilityError::OldWindows => {
                Some("Please update your system to Windows 8.1 or later.".to_owned())
            }
            AvailabilityError::UnsupportedPlatform => Some(self.details.clone()),
        }
    }
}

/// Errors produced by `tg-webview`.
#[derive(Debug, Error)]
pub enum WebViewError {
    /// Microsoft Edge WebView2 Runtime is missing or not available on this system.
    #[error(
        "Microsoft Edge WebView2 Runtime is not installed ({details}); download: {download_url}"
    )]
    RuntimeNotInstalled {
        /// Diagnostic detail message.
        details: String,
        /// Official download URL for the WebView2 runtime.
        download_url: &'static str,
    },

    /// The operating system version or platform is not supported.
    #[error("webview is not supported on this platform: {0}")]
    UnsupportedPlatform(String),

    /// A `raw-window-handle` did not contain a compatible Win32 `HWND`.
    #[error("invalid or unsupported parent window handle: {0}")]
    InvalidWindowHandle(String),

    /// Restricted-origin configuration was invalid (must be strict `https://host` without port/path).
    #[error("invalid restricted origin: {0}")]
    InvalidRestrictedOrigin(String),

    /// Failed to create or prepare the WebView2 user-data directory.
    #[error("failed to prepare user data directory {path}: {source}")]
    UserDataDir {
        /// Target path that failed.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Failed to create the embedded Win32 child host window.
    #[error("failed to create child host window: {0}")]
    HostWindow(String),

    /// COM or WebView2 runtime returned an error.
    #[error("WebView2 runtime error: {0}")]
    Backend(String),

    /// Operation attempted after the webview instance was closed.
    #[error("webview has already been closed")]
    Closed,

    /// Invalid or rejected Mini App bridge message.
    #[error("invalid Mini App bridge message: {0}")]
    InvalidBridgeMessage(String),

    /// JSON serialization or deserialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Convenience result alias for `tg-webview`.
pub type Result<T> = std::result::Result<T, WebViewError>;
