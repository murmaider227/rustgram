//! Error types for `tg-voip`.
//!
//! Covers cryptographic framing, reflector/STUN/TURN transport, RTP packetization,
//! Opus codec, DSP processing, audio I/O devices, and engine lifecycle errors.

use thiserror::Error;

/// Result alias for `tg-voip` operations.
pub type VoipResult<T> = Result<T, VoipError>;

/// Top-level error enum for the `tg-voip` media engine.
#[derive(Debug, Error)]
pub enum VoipError {
    /// Packet too short to parse header or authentication tag.
    #[error("packet too short: {0} bytes")]
    PacketTooShort(usize),

    /// Packet exceeds maximum allowed transport or signaling size.
    #[error("packet too large: {0} bytes")]
    PacketTooLarge(usize),

    /// MTProto 2.0 `msg_key` authentication tag mismatch.
    #[error("msg_key verification failed")]
    MsgKeyMismatch,

    /// Sequence counter was already processed or fell outside the replay window.
    #[error("duplicate or out-of-window sequence counter: {0}")]
    ReplayDetected(u32),

    /// Outgoing packet counter exhausted (`>= 2^30 - 1`).
    #[error("outgoing sequence counter exhausted")]
    CounterExhausted,

    /// Malformed inner message or protocol framing.
    #[error("invalid framing: {0}")]
    InvalidFraming(String),

    /// Invalid Telegram reflector packet or mismatched `peer_tag`.
    #[error("reflector protocol error: {0}")]
    Reflector(String),

    /// Invalid STUN/TURN message.
    #[error("STUN/TURN protocol error: {0}")]
    Stun(String),

    /// Invalid signaling JSON payload.
    #[error("signaling error: {0}")]
    Signaling(String),

    /// Opus encoder or decoder error.
    #[error("opus codec error: {0}")]
    Opus(#[from] opus_pure::Error),

    /// Voice DSP (AEC3 / NS / AGC2) error.
    #[error("DSP pipeline error: {0}")]
    Dsp(String),

    /// Audio hardware I/O error (`cpal` or `tg-audio` device manager).
    #[error("audio I/O error: {0}")]
    AudioIo(String),

    /// Video capture, pixel conversion, or codec (Media Foundation / DXGI) error.
    #[error("video pipeline error: {0}")]
    Video(String),

    /// DTLS handshake or certificate error.
    #[error("DTLS error: {0}")]
    Dtls(String),

    /// SRTP/SRTCP encryption, decryption, or replay error.
    #[error("SRTP error: {0}")]
    Srtp(String),

    /// ICE-lite candidate or connectivity check error.
    #[error("ICE error: {0}")]
    Ice(String),

    /// Group call or broadcast stream error.
    #[error("group call error: {0}")]
    GroupCall(String),

    /// Network socket I/O error.
    #[error("network I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Invalid engine state transition or configuration.
    #[error("invalid engine state: {0}")]
    InvalidState(String),
}
