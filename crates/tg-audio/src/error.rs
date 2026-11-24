//! Error types for `tg-audio`.
//!
//! Covers decoding, Opus/Ogg encoding, audio device I/O, capture, and cache errors.

use thiserror::Error;

/// Unified error type returned by `tg-audio` operations.
#[derive(Debug, Error)]
pub enum AudioError {
    /// Underlying I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Opus codec or Ogg Opus container error.
    #[error("Opus error: {0}")]
    Opus(String),

    /// Container demuxing or audio decoding error.
    #[error("Decode error: {0}")]
    Decode(String),

    /// Audio format is unsupported or invalid.
    #[error("Unsupported or invalid audio format: {0}")]
    UnsupportedFormat(String),

    /// Audio output or capture device error.
    #[error("Audio device error: {0}")]
    Device(String),

    /// No audio playback or capture device is currently available.
    #[error("No audio device available")]
    NoDevice,

    /// Invalid argument or state transition.
    #[error("Invalid audio operation: {0}")]
    InvalidOperation(String),

    /// Requested audio source could not be resolved.
    #[error("Audio source not found: {0}")]
    SourceNotFound(String),

    /// Input exceeded a configured size or duration limit.
    #[error("Limit exceeded: {0}")]
    LimitExceeded(String),
}

impl From<opus_pure::Error> for AudioError {
    fn from(err: opus_pure::Error) -> Self {
        Self::Opus(err.to_string())
    }
}

/// Convenience result alias for `tg-audio`.
pub type AudioResult<T> = Result<T, AudioError>;
