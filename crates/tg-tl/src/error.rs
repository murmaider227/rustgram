//! Error types for TL deserialization.
//!
//! Port of error reporting concepts from `lib_tl/tl/tl_basic_types.h`.

use thiserror::Error;

/// Error returned when deserializing a TL value fails.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DeserializeError {
    /// Input buffer ended before the required number of bytes could be read.
    #[error("unexpected end of buffer: needed {needed} bytes, only {remaining} remaining")]
    UnexpectedEof {
        /// Number of bytes required.
        needed: usize,
        /// Number of bytes still available in the cursor.
        remaining: usize,
    },
    /// Encountered an unknown or mismatched 32-bit TL constructor ID.
    #[error("unexpected constructor id: 0x{id:08x}")]
    UnexpectedConstructor {
        /// The 32-bit constructor ID read from the stream.
        id: u32,
    },
    /// Encountered an invalid TL string/bytes length prefix byte (such as `0xff`).
    #[error("invalid TL string length prefix: 0x{prefix:02x}")]
    InvalidStringLength {
        /// The invalid leading length byte.
        prefix: u8,
    },
    /// Encountered a negative element or byte count in the TL stream.
    #[error("negative length in TL stream: {0}")]
    NegativeLength(i32),
    /// Gzip decompression failed or exceeded the configured output limit.
    #[error("gzip error: {0}")]
    GzipError(String),
}
