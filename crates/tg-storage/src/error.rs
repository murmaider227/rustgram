//! Storage error types for `tg-storage`.

use std::path::PathBuf;

/// Result type returned by storage operations.
pub type Result<T> = std::result::Result<T, StorageError>;

/// Errors produced when reading, writing, or decrypting tdesktop storage files.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// Underlying filesystem I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Expected file was not found on disk.
    #[error("file not found: {0}")]
    NotFound(PathBuf),

    /// TDF$ or TDEF magic header mismatch.
    #[error("invalid file magic")]
    InvalidMagic,

    /// File version is newer than supported by this build.
    #[error("unsupported version: {0}")]
    UnsupportedVersion(i32),

    /// MD5 or SHA-256 integrity checksum mismatch.
    #[error("checksum mismatch")]
    ChecksumMismatch,

    /// AES-IGE decryption failed (SHA-1 verification mismatch or invalid padded length).
    #[error("decryption failed (wrong key or corrupt data)")]
    DecryptionFailed,

    /// Data stream ended unexpectedly while decoding.
    #[error("unexpected end of stream")]
    UnexpectedEof,

    /// Stream contained corrupt or out-of-range data.
    #[error("corrupt data: {0}")]
    CorruptData(String),

    /// Unknown block ID or map key type encountered while parsing.
    #[error("unknown tag/block id: 0x{0:x}")]
    UnknownTag(u32),

    /// Invalid or unsupported passcode KDF parameters.
    #[error("unsupported passcode KDF: {0}")]
    UnsupportedKdf(String),

    /// Passcode verification failed or passcode required.
    #[error("incorrect passcode")]
    IncorrectPasscode,

    /// SQLite media cache index error.
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
}

impl From<crate::qdatastream::DataStreamError> for StorageError {
    fn from(err: crate::qdatastream::DataStreamError) -> Self {
        match err {
            crate::qdatastream::DataStreamError::UnexpectedEof { .. } => Self::UnexpectedEof,
            other => Self::CorruptData(other.to_string()),
        }
    }
}
