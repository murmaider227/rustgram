//! Error types for `tg-calls`.
//!
//! Covers Diffie-Hellman parameter validation, call signaling encryption,
//! 1:1 call state machine errors, and group call payload/state errors.

use thiserror::Error;

/// Errors returned during Diffie-Hellman parameter checks or key exchange.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DhError {
    /// The server-supplied DH prime `p` or generator `g` failed safety checks.
    #[error("invalid DH parameters (prime p or generator g failed validation)")]
    InvalidParameters,
    /// `messages.dhConfigNotModified` was received without a cached DH configuration.
    #[error("dhConfigNotModified received with no cached DH config")]
    MissingCachedConfig,
    /// Random seed length did not equal 256 bytes.
    #[error("invalid DH random seed length: expected {expected}, got {actual}")]
    InvalidRandomLength {
        /// Expected byte count (256).
        expected: usize,
        /// Actual byte count received.
        actual: usize,
    },
    /// Remote `g_a` or `g_b` failed safety bounds (`2^(2048-64) <= x <= p - 2^(2048-64)`).
    #[error("invalid DH public value g_a / g_b")]
    InvalidPublicValue,
    /// Computed auth key exceeded 256 bytes.
    #[error("computed auth key too large: {actual} bytes (max 256)")]
    AuthKeyTooLarge {
        /// Actual byte count.
        actual: usize,
    },
    /// `SHA256(g_a)` did not match the previously committed `g_a_hash`.
    #[error("g_a does not match committed g_a_hash")]
    GaHashMismatch,
}

/// Errors returned during signaling packet encryption or decryption.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SignalingError {
    /// Signaling packet was too short to contain the 16-byte `msg_key`, 4-byte `seq`, and body.
    #[error("signaling packet too short: {0} bytes")]
    PacketTooShort(usize),
    /// Signaling packet exceeded the maximum incoming encrypted packet size.
    #[error("signaling packet too large: {0} bytes")]
    PacketTooLarge(usize),
    /// Decrypted packet's `msg_key` did not match the recomputed SHA-256 MAC.
    #[error("signaling packet msg_key verification failed")]
    MsgKeyMismatch,
    /// Outgoing 30-bit sequence counter was exhausted (`>= 2^30 - 1`).
    #[error("signaling sequence counter exhausted")]
    CounterExhausted,
    /// Inner packet header or payload framing was invalid.
    #[error("invalid signaling packet inner framing")]
    InvalidFraming,
    /// Packet sequence number was already seen or too old (replay detected).
    #[error("replayed or stale signaling sequence number: {0}")]
    ReplayDetected(u32),
}

/// Errors returned when parsing or handling group call JSON payloads.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum GroupCallError {
    /// Failed to parse JSON join payload or join response.
    #[error("invalid group call JSON payload: {0}")]
    InvalidJson(String),
    /// Expected a JSON object at the top level.
    #[error("group call JSON payload must be a JSON object")]
    NotJsonObject,
}
