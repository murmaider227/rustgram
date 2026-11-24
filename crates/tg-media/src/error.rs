//! Error types for `tg-media`.

use std::io;
use thiserror::Error;

/// Errors returned by media decoding, caching, and file transfer operations.
#[derive(Debug, Error)]
pub enum MediaError {
    /// Input bytes or file exceeded an enforced size or pixel-area limit.
    #[error("media exceeds limit: {0}")]
    LimitExceeded(String),

    /// Input image or container bytes could not be decoded.
    #[error("invalid media format: {0}")]
    InvalidFormat(String),

    /// Invalid inline stripped thumbnail or vector path payload.
    #[error("invalid inline thumbnail: {0}")]
    InvalidInlineData(String),

    /// Invalid offset, limit, or alignment for MTProto file transfers.
    #[error("invalid transfer parameter: {0}")]
    InvalidTransferParam(String),

    /// File is empty or exceeds Telegram upload size limits.
    #[error("invalid file size for upload: {0}")]
    InvalidFileSize(String),

    /// CDN encryption parameters (AES-256-CTR key/IV) are invalid.
    #[error("invalid CDN encryption parameters: {0}")]
    InvalidCdnParams(String),

    /// SHA-256 verification of a downloaded CDN chunk failed.
    #[error("CDN file hash mismatch at offset {offset}")]
    CdnHashMismatch {
        /// Byte offset of the chunk whose SHA-256 hash failed verification.
        offset: u64,
    },

    /// CDN server did not provide a hash covering the requested offset.
    #[error("missing CDN file hash for offset {offset}")]
    CdnHashMissing {
        /// Byte offset that lacked a hash entry after `upload.getCdnFileHashes`.
        offset: u64,
    },

    /// File transfer was cancelled by caller or due to unrecoverable error.
    #[error("transfer cancelled")]
    Cancelled,

    /// Remote RPC call returned an error.
    #[error("RPC error {code}: {message}")]
    Rpc {
        /// Numeric MTProto error code (e.g., 400, 404, 420).
        code: i32,
        /// Telegram error identifier (e.g., `FILE_REFERENCE_EXPIRED`).
        message: String,
    },

    /// Cache key or entry was invalid.
    #[error("cache error: {0}")]
    Cache(String),

    /// Underlying I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Convenience result alias for `tg-media`.
pub type Result<T> = std::result::Result<T, MediaError>;
