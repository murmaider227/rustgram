//! Port of `export/output/export_output_result.h`.

use thiserror::Error;

/// Classification of an output write result (port of `Export::Output::Result::Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputResultType {
    /// Operation succeeded.
    Success,
    /// Recoverable I/O error.
    Error,
    /// Unrecoverable fatal I/O or format error.
    FatalError,
}

/// Result returned by export output operations (port of `Export::Output::Result`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputResult {
    /// Path associated with an error (empty on success).
    pub path: String,
    /// Result status classification.
    pub kind: OutputResultType,
}

impl OutputResult {
    /// Creates a new `OutputResult`.
    pub fn new(kind: OutputResultType, path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            kind,
        }
    }

    /// Creates a successful result.
    pub fn success() -> Self {
        Self {
            path: String::new(),
            kind: OutputResultType::Success,
        }
    }

    /// Creates a recoverable error result for `path`.
    pub fn error(path: impl Into<String>) -> Self {
        Self::new(OutputResultType::Error, path)
    }

    /// Creates a fatal error result for `path`.
    pub fn fatal_error(path: impl Into<String>) -> Self {
        Self::new(OutputResultType::FatalError, path)
    }

    /// Returns true if the operation succeeded.
    pub const fn is_success(&self) -> bool {
        matches!(self.kind, OutputResultType::Success)
    }

    /// Returns true if the operation failed (either recoverable or fatal).
    pub const fn is_error(&self) -> bool {
        matches!(
            self.kind,
            OutputResultType::Error | OutputResultType::FatalError
        )
    }

    /// Returns true if the operation failed with a fatal error.
    pub const fn is_fatal_error(&self) -> bool {
        matches!(self.kind, OutputResultType::FatalError)
    }
}

/// Error type for high-level export operations.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ExportError {
    /// Output filesystem or serialization error.
    #[error("export output error at '{path}' (fatal={fatal})")]
    Output {
        /// File path where the error occurred.
        path: String,
        /// Whether the error is fatal.
        fatal: bool,
    },
    /// Telegram API error returned by the `ExportSource`.
    #[error("export API error ({code}): {message}")]
    Api {
        /// RPC error code.
        code: i32,
        /// RPC error type/message (e.g. `"TAKEOUT_INVALID"`, `"TAKEOUT_INIT_DELAY_86400"`).
        message: String,
    },
    /// Export was cancelled.
    #[error("export cancelled")]
    Cancelled,
}

impl From<OutputResult> for ExportError {
    fn from(res: OutputResult) -> Self {
        let fatal = res.is_fatal_error();
        Self::Output {
            path: res.path,
            fatal,
        }
    }
}
