//! Error types for `tg-lottie`.
//!
//! Ports error cases from `lottie/lottie_common.h` and `lottie/lottie_animation.cpp`.

use thiserror::Error;

/// Error returned when loading, parsing, caching, or rendering a Lottie animation.
#[derive(Debug, Error)]
pub enum Error {
    /// Input file or payload exceeds the maximum allowed size.
    #[error("file too large: {size} bytes (max {max} bytes)")]
    FileTooLarge {
        /// Actual byte length.
        size: usize,
        /// Configured maximum byte length.
        max: usize,
    },

    /// Gzip decompression failed or exceeded the safety limit.
    #[error("gzip decompression failed: {0}")]
    DecompressionFailed(String),

    /// JSON or Lottie structure parsing failed.
    #[error("lottie parse failed: {0}")]
    ParseFailed(String),

    /// The animation is syntactically valid but violates runtime limits or is unsupported.
    #[error("lottie animation not supported: {0}")]
    NotSupported(String),

    /// Rendering a frame failed (e.g., invalid target dimensions).
    #[error("frame render failed: {0}")]
    RenderFailed(String),

    /// Cached frame blob is corrupt or incompatible.
    #[error("frame cache error: {0}")]
    CacheError(String),
}

/// Coarse error category matching tdesktop's `Lottie::Error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Parsing or content validation failed (`Lottie::Error::ParseFailed`).
    ParseFailed,
    /// Animation metadata or feature is unsupported (`Lottie::Error::NotSupported`).
    NotSupported,
}

impl Error {
    /// Maps a detailed error into tdesktop's `Lottie::Error` kind.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::NotSupported(_) => ErrorKind::NotSupported,
            Self::FileTooLarge { .. }
            | Self::DecompressionFailed(_)
            | Self::ParseFailed(_)
            | Self::RenderFailed(_)
            | Self::CacheError(_) => ErrorKind::ParseFailed,
        }
    }
}
