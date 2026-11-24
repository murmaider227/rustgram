//! Error types for `tg-session`.
//!
//! Port origin: error paths across `main/main_account.cpp`, `main/main_domain.cpp`,
//! and `main/main_session.cpp`.

use thiserror::Error;

/// Result alias using [`SessionError`].
pub type Result<T> = std::result::Result<T, SessionError>;

/// Top-level error returned by `tg-session` lifecycle and runtime operations.
#[derive(Debug, Error)]
pub enum SessionError {
    /// Required environment variable (`RUSTGRAM_API_ID` or `RUSTGRAM_API_HASH`) was missing or empty.
    #[error("missing required environment variable {0}")]
    MissingEnvVar(&'static str),
    /// `RUSTGRAM_API_ID` could not be parsed as a positive 32-bit integer.
    #[error("invalid RUSTGRAM_API_ID value '{0}' (expected positive integer)")]
    InvalidApiId(String),
    /// Provided local storage passcode did not decrypt `key_datas`.
    #[error("incorrect local passcode")]
    IncorrectPasscode,
    /// `tdata` is locked with a local passcode and requires unlocking.
    #[error("local passcode required to unlock working directory")]
    PasscodeRequired,
    /// Multi-account domain management error from `tg-core`.
    #[error("domain error: {0}")]
    Domain(#[from] tg_core::DomainError),
    /// Persistent `tdata` storage error from `tg-storage`.
    #[error("storage error: {0}")]
    Storage(#[from] tg_storage::StorageError),
    /// Working directory or launcher error from `tg-core`.
    #[error("launcher error: {0}")]
    Launcher(#[from] tg_core::LauncherError),
    /// High-level Telegram API error from `tg-api`.
    #[error("API error: {0}")]
    Api(#[from] tg_api::ApiError),
    /// Low-level RPC invocation error from `tg-api`.
    #[error("RPC invocation error: {0}")]
    Invocation(#[from] tg_api::InvocationError),
    /// Message sending failed with a classified error.
    #[error("send message failed: {0:?}")]
    SendMessage(tg_api::SendMessageErrorKind),
    /// Requested account slot does not exist.
    #[error("account slot {0} not found")]
    AccountNotFound(i32),
    /// Account has no authenticated user session yet.
    #[error("account is not authorized")]
    NotAuthorized,
    /// Background `SessionRuntime` actor has shut down.
    #[error("session runtime is closed")]
    RuntimeClosed,
    /// Failed to spawn or initialize the Tokio runtime thread.
    #[error("failed to start Tokio runtime thread: {0}")]
    RuntimeThread(String),
    /// Target peer could not be resolved to an `InputPeer`.
    #[error("invalid or unknown peer: {0:?}")]
    InvalidPeer(tg_data::PeerId),
    /// Filesystem or standard I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<tg_api::SendMessageErrorKind> for SessionError {
    fn from(kind: tg_api::SendMessageErrorKind) -> Self {
        Self::SendMessage(kind)
    }
}
