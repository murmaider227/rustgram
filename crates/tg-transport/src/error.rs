//! Error types for MTProto transport framing, obfuscation, proxies, and connections.
//!
//! Port of error handling across `mtproto/connection_abstract.h`, `mtproto/connection_tcp.cpp`,
//! `mtproto/connection_http.cpp`, and `mtproto/details/mtproto_tls_socket.cpp`.

use std::fmt;

/// Synthetic error code used by tdesktop when no server transport code was received
/// (`AbstractConnection::kErrorCodeOther`).
pub const ERROR_CODE_OTHER: i32 = -499;

/// Known MTProto 4-byte negative transport error codes returned by Telegram servers.
///
/// See <https://core.telegram.org/mtproto/mtproto-transports#transport-errors>.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportErrorCode {
    /// `-404`: Authorization key not found on the server.
    AuthKeyNotFound,
    /// `-429`: Transport flood / too many requests or connections.
    TransportFlood,
    /// `-444`: Invalid DC ID (for example, in an MTProxy connection).
    InvalidDc,
    /// `-400`: Bad request / malformed transport payload.
    BadRequest,
    /// `-403`: Forbidden (often returned by HTTP transport / proxies).
    Forbidden,
    /// `-500`: Internal server error (or bad HTTP response size in `connection_http.cpp`).
    InternalServerError,
    /// `-503`: Service unavailable.
    ServiceUnavailable,
    /// Any other negative transport error code from the server.
    Other(i32),
}

impl TransportErrorCode {
    /// Convert a signed 32-bit server transport error code (e.g. `-404`) into a typed variant.
    #[must_use]
    pub const fn from_code(code: i32) -> Self {
        match code {
            -400 => Self::BadRequest,
            -403 => Self::Forbidden,
            -404 => Self::AuthKeyNotFound,
            -429 => Self::TransportFlood,
            -444 => Self::InvalidDc,
            -500 => Self::InternalServerError,
            -503 => Self::ServiceUnavailable,
            other => Self::Other(other),
        }
    }

    /// Return the raw signed 32-bit wire code (always negative for protocol errors).
    #[must_use]
    pub const fn code(self) -> i32 {
        match self {
            Self::BadRequest => -400,
            Self::Forbidden => -403,
            Self::AuthKeyNotFound => -404,
            Self::TransportFlood => -429,
            Self::InvalidDc => -444,
            Self::InternalServerError => -500,
            Self::ServiceUnavailable => -503,
            Self::Other(code) => code,
        }
    }
}

impl fmt::Display for TransportErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadRequest => write!(f, "transport bad request (-400)"),
            Self::Forbidden => write!(f, "transport forbidden (-403)"),
            Self::AuthKeyNotFound => write!(f, "auth key not found (-404)"),
            Self::TransportFlood => write!(f, "transport flood (-429)"),
            Self::InvalidDc => write!(f, "invalid DC (-444)"),
            Self::InternalServerError => write!(f, "internal server error (-500)"),
            Self::ServiceUnavailable => write!(f, "service unavailable (-503)"),
            Self::Other(code) => write!(f, "transport error ({code})"),
        }
    }
}

/// SOCKS5 reply codes from RFC 1928 section 6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Socks5Reply {
    /// `0x01`: General SOCKS server failure.
    GeneralFailure,
    /// `0x02`: Connection not allowed by ruleset.
    ConnectionNotAllowed,
    /// `0x03`: Network unreachable.
    NetworkUnreachable,
    /// `0x04`: Host unreachable.
    HostUnreachable,
    /// `0x05`: Connection refused.
    ConnectionRefused,
    /// `0x06`: TTL expired.
    TtlExpired,
    /// `0x07`: Command not supported.
    CommandNotSupported,
    /// `0x08`: Address type not supported.
    AddressTypeNotSupported,
    /// Any other unassigned reply byte.
    Unknown(u8),
}

impl Socks5Reply {
    /// Convert a raw non-zero SOCKS5 reply byte into a typed reply variant.
    #[must_use]
    pub const fn from_byte(code: u8) -> Self {
        match code {
            0x01 => Self::GeneralFailure,
            0x02 => Self::ConnectionNotAllowed,
            0x03 => Self::NetworkUnreachable,
            0x04 => Self::HostUnreachable,
            0x05 => Self::ConnectionRefused,
            0x06 => Self::TtlExpired,
            0x07 => Self::CommandNotSupported,
            0x08 => Self::AddressTypeNotSupported,
            other => Self::Unknown(other),
        }
    }
}

impl fmt::Display for Socks5Reply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GeneralFailure => write!(f, "general SOCKS server failure (0x01)"),
            Self::ConnectionNotAllowed => write!(f, "connection not allowed by ruleset (0x02)"),
            Self::NetworkUnreachable => write!(f, "network unreachable (0x03)"),
            Self::HostUnreachable => write!(f, "host unreachable (0x04)"),
            Self::ConnectionRefused => write!(f, "connection refused (0x05)"),
            Self::TtlExpired => write!(f, "TTL expired (0x06)"),
            Self::CommandNotSupported => write!(f, "command not supported (0x07)"),
            Self::AddressTypeNotSupported => write!(f, "address type not supported (0x08)"),
            Self::Unknown(code) => write!(f, "unknown SOCKS5 reply (0x{code:02x})"),
        }
    }
}

/// Unified error type for `tg-transport`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Not enough bytes are currently buffered to decode a full transport frame.
    #[error("incomplete frame: more bytes needed")]
    MissingBytes,

    /// Frame length prefix was malformed, zero, misaligned, or exceeded [`crate::MAX_PACKET_SIZE`].
    #[error("invalid packet length: {0}")]
    InvalidPacketLength(usize),

    /// CRC32 checksum failed when unpacking a `Full` transport frame.
    #[error("CRC32 mismatch: expected 0x{expected:08x}, got 0x{actual:08x}")]
    CrcMismatch {
        /// CRC32 value read from the trailing 4 bytes of the frame.
        expected: u32,
        /// CRC32 value computed over the frame header and payload.
        actual: u32,
    },

    /// Sequence number mismatch when unpacking a `Full` transport frame.
    #[error("sequence number mismatch: expected {expected}, got {actual}")]
    SequenceMismatch {
        /// Next sequence number expected by the transport state.
        expected: u32,
        /// Sequence number received in the frame header.
        actual: u32,
    },

    /// Server returned a 4-byte negative MTProto transport error (e.g. `-404`) or HTTP error status.
    #[error("server transport error: {0}")]
    ServerError(TransportErrorCode),

    /// Transport does not support obfuscated2 initialization (for example, `Full`).
    #[error("transport does not define an obfuscated2 protocol tag")]
    UnsupportedObfuscation,

    /// Invalid MTProxy secret or proxy configuration.
    #[error("invalid proxy configuration: {0}")]
    InvalidProxy(String),

    /// Proxy authentication failed (SOCKS5 auth status != 0 or HTTP 407).
    #[error("proxy authentication failed")]
    ProxyAuthFailed,

    /// SOCKS5 handshake or protocol error.
    #[error("SOCKS5 proxy error: {0}")]
    Socks5(String),

    /// SOCKS5 server returned a non-zero reply code for CONNECT.
    #[error("SOCKS5 CONNECT rejected: {0}")]
    Socks5Reply(Socks5Reply),

    /// HTTP CONNECT proxy returned a non-2xx status code or malformed response.
    #[error("HTTP CONNECT proxy error (status {status}): {reason}")]
    HttpProxy {
        /// HTTP status code returned by the proxy (0 if the status line could not be parsed).
        status: u16,
        /// Human-readable reason or error description.
        reason: String,
    },

    /// Fake-TLS handshake or record framing error.
    #[error("fake-TLS error: {0}")]
    Tls(String),

    /// Plain HTTP transport protocol error.
    #[error("HTTP transport error: {0}")]
    Http(String),

    /// Fake `req_pq` / `resPQ` connectivity probe failed.
    #[error("PQ connectivity probe failed: {0}")]
    ProbeFailed(String),

    /// Connection or handshake timed out.
    #[error("connection timed out")]
    Timeout,

    /// Remote peer closed the connection unexpectedly.
    #[error("connection closed by peer")]
    Disconnected,

    /// No connection candidates were provided or all candidates failed.
    #[error("no candidates succeeded: {0}")]
    AllCandidatesFailed(String),

    /// Underlying I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
