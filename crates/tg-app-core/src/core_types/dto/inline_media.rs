//! Pure inline-media descriptor and progressive photo load state DTOs.

use crate::core_types::misc::InlineDocumentData;

/// Photo download / blur-up state for an inline photo attachment.
#[derive(Debug, Clone)]
pub enum PhotoLoadState {
    /// Photo is still downloading; displays stripped thumbnail + circular progress ring.
    Loading {
        /// Optional determinate progress in `0.0..=1.0` (`None` renders infinite spinner).
        progress: Option<f32>,
        /// Bytes downloaded so far (for badge tooltip/label).
        bytes_loaded: u64,
        /// Total bytes expected.
        total_bytes: u64,
    },
    /// Full-resolution encoded photo bytes (PNG/JPEG/WebP) ready for blur-up crossfade.
    Ready {
        /// Encoded image bytes.
        bytes: Vec<u8>,
    },
}

/// Optional rich media descriptor associated with a chat message `MsgId`.
///
/// Allows supplying real video/GIF/round-video/WebM-sticker payloads, spoiler flags, photo
/// loading progress / full image bytes, and audio bytes/metadata without changing any existing
/// public struct fields in `history/backend.rs`.
#[derive(Debug, Clone, Default)]
pub struct InlineMediaDescriptor {
    /// Encoded video/GIF/round-video/WebM-sticker bytes or local file path for `tg-video`.
    pub video_data: Option<InlineDocumentData>,
    /// True if this media item is hidden behind a shimmering spoiler veil until clicked.
    pub has_spoiler: bool,
    /// Optional photo loading / full-resolution blur-up state.
    pub photo_load: Option<PhotoLoadState>,
    /// Optional audio bytes (WAV/Opus/MP3/FLAC/OGG) for voice, music, or round video sound.
    pub audio_bytes: Option<Vec<u8>>,
    /// Optional audio extension hint (e.g. `"wav"`, `"ogg"`, `"mp3"`).
    pub audio_ext: Option<String>,
    /// Optional music track title (for `DocumentAttachment` music files).
    pub music_title: Option<String>,
    /// Optional music performer / artist name.
    pub music_performer: Option<String>,
    /// Optional music duration in seconds.
    pub music_duration_secs: Option<u32>,
    /// True if this sticker attachment is a WebM VP9 alpha video sticker (decoded via `tg-video`).
    pub is_video_sticker: bool,
}
