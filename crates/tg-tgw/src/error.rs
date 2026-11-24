//! Error types for `tg-tgw`.
//!
//! Port origin: `Telegram/SourceFiles/tgw/*` and `platform/win/widget_sandbox_win.cpp`.

use thiserror::Error;

/// Result alias used throughout `tg-tgw`.
pub type Result<T> = std::result::Result<T, TgwError>;

/// Errors produced by the sandboxed HTML widget engine, wire protocol, backend, and sandbox.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TgwError {
    /// Widget payload could not be decompressed (empty, corrupt, or exceeds safety cap).
    #[error("failed to unpack widget payload: {0}")]
    InvalidPayload(String),

    /// Hosted binary wire control frame failed validation or decoding.
    #[error("wire protocol error: {0}")]
    Wire(String),

    /// JSON bridge message exceeded size cap or failed parsing/validation.
    #[error("bridge protocol error: {0}")]
    Bridge(String),

    /// Backend resource allowance (fonts, images, canvas memory, pixel work, CPU meter) exceeded.
    #[error("widget resource limit exceeded: {0}")]
    ResourceLimit(String),

    /// Invalid state transition or lifecycle operation on the widget state machine.
    #[error("invalid widget state: {0}")]
    InvalidState(String),

    /// Widget persistent storage quota or binary record format error.
    #[error("widget storage error: {0}")]
    Storage(String),

    /// OS sandbox policy construction, Win32 token/job enforcement, or verification failed.
    #[error("sandbox error: {0}")]
    Sandbox(String),

    /// Embedded WebView2 host failed or is unavailable on this machine.
    #[error("webview sandbox host error: {0}")]
    WebView(String),
}
