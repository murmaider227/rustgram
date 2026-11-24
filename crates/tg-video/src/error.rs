//! Error types for video decoding, streaming, and clip playback.
//!
//! Ports error enums from `media/streaming/media_streaming_common.h` and
//! `ffmpeg/ffmpeg_utility.h`.

use thiserror::Error;

/// Streaming-level error classification matching `Media::Streaming::Error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
pub enum StreamingError {
    /// Failed to open container or initialize required codec streams.
    #[error("failed to open media stream")]
    OpenFailed,
    /// Underlying byte-range loader failed to deliver valid data.
    #[error("failed to load media range")]
    LoadFailed,
    /// Demuxer or decoder encountered corrupted or unsupported data.
    #[error("invalid media data")]
    InvalidData,
    /// Container header exceeds streaming header limits (e.g. moov atom too far).
    #[error("media is not streamable")]
    NotStreamable,
}

/// Top-level error type returned by `tg-video` APIs.
#[derive(Debug, Error)]
pub enum VideoError {
    /// Streaming subsystem error.
    #[error(transparent)]
    Streaming(#[from] StreamingError),
    /// FFmpeg library error with numeric error code and formatted description.
    #[error("ffmpeg error in {context} (code {code}): {message}")]
    Ffmpeg {
        /// FFmpeg call or operation name.
        context: &'static str,
        /// Negative FFmpeg `AVERROR` code.
        code: i32,
        /// Human-readable error text from `av_strerror`.
        message: String,
    },
    /// I/O error reading local files or writing output images.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Invalid caller parameter or out-of-range value.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// Frame dimensions violate safety bounds (`ValidFrameSize` / storage limits).
    #[error("frame size {width}x{height} exceeds allowed area limit {max_area}")]
    FrameSizeExceeded {
        /// Frame width in pixels.
        width: u32,
        /// Frame height in pixels.
        height: u32,
        /// Maximum allowed pixel area.
        max_area: u64,
    },
    /// Image encoding error (e.g. thumbnail serialization).
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
}

/// Convenience result alias for `tg-video`.
pub type Result<T> = std::result::Result<T, VideoError>;
