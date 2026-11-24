//! Error types for `tg-platform`.

use thiserror::Error;

/// Unified error type returned by platform integration operations.
#[derive(Debug, Error)]
pub enum PlatformError {
    /// The operation is not supported on the current operating system or version.
    #[error("operation is not supported on this platform")]
    Unsupported,

    /// An invalid argument was provided by the caller.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// A Windows API or COM/WinRT call failed.
    #[error("Windows API error in {context}: {message} (code {code:#010x})")]
    Win32 {
        /// Human-readable API name or operation context.
        context: &'static str,
        /// Win32 error code or HRESULT bits.
        code: u32,
        /// Formatted system error message.
        message: String,
    },

    /// Standard filesystem or pipe I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Windows Registry operation failure.
    #[error("registry error: {0}")]
    Registry(String),

    /// Toast notification failure.
    #[error("notification error: {0}")]
    Notification(String),

    /// Single-instance IPC failure.
    #[error("IPC error: {0}")]
    Ipc(String),

    /// Spellchecker failure.
    #[error("spellchecker error: {0}")]
    Spellcheck(String),

    /// Image or icon encoding failure.
    #[error("image encoding error: {0}")]
    Image(String),

    /// SystemMediaTransportControls failure.
    #[error("SMTC error: {0}")]
    Smtc(String),

    /// Optical character recognition (OCR) failure.
    #[error("OCR error: {0}")]
    Ocr(String),

    /// Geolocation lookup failure.
    #[error("geolocation error: {0}")]
    Geolocation(String),
    /// Windows Hello (`UserConsentVerifier`) verification failure.
    #[error("Windows Hello error: {0}")]
    WindowsHello(String),

    /// Taskbar thumbnail buttons (`ITaskbarList3`) failure.
    #[error("taskbar buttons error: {0}")]
    TaskbarButtons(String),

    /// Wallet key protection (DPAPI / platform keystore) failure.
    #[error("wallet key protection error: {0}")]
    WalletProtection(String),
    /// WebAuthn / FIDO2 passkey operation failure.
    #[error("WebAuthn error: {0}")]
    WebAuthn(String),

    /// caBLE BLE proximity scanner failure.
    #[error("caBLE BLE scanner error: {0}")]
    CableScanner(String),

    /// Sandboxed widget helper launcher or IPC channel failure.
    #[error("widget sandbox error: {0}")]
    WidgetSandbox(String),

    /// Windows StartupTask / Run key autostart registration failure.
    #[error("autostart error: {0}")]
    Autostart(String),
}

/// Convenience result alias for `tg-platform`.
pub type Result<T> = std::result::Result<T, PlatformError>;

#[cfg(windows)]
impl PlatformError {
    /// Convert a `windows::core::Error` into a `PlatformError::Win32`.
    pub(crate) fn from_win(context: &'static str, err: windows::core::Error) -> Self {
        Self::Win32 {
            context,
            code: err.code().0 as u32,
            message: err.message(),
        }
    }

    /// Capture `GetLastError()` as a `PlatformError::Win32`.
    pub(crate) fn last_win32(context: &'static str) -> Self {
        let err = windows::core::Error::from_thread();
        Self::from_win(context, err)
    }
}
