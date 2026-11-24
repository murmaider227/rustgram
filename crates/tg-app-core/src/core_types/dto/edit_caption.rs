//! Pure edit-media-caption DTOs (`history/edit_caption.rs` model).

use tg_text::TextWithEntities;

use crate::history::backend::HistoryMedia;
use crate::stand_in::types::{MsgId, PeerId};

/// Replacement media file chosen in [`EditCaptionBoxState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplacementMediaFile {
    /// Original file name (e.g. `"photo_v2.jpg"` or `"report.pdf"`).
    pub file_name: String,
    /// MIME type string (e.g. `"image/jpeg"`, `"application/pdf"`).
    pub mime_type: String,
    /// Raw file bytes to upload via `upload.saveFilePart` / `saveBigFilePart`.
    pub bytes: Vec<u8>,
    /// `true` to upload as `inputMediaUploadedPhoto`, `false` for `inputMediaUploadedDocument`.
    pub as_photo: bool,
}

impl ReplacementMediaFile {
    /// Infers MIME type and photo/document mode from `file_name` and `bytes`.
    #[must_use]
    pub fn from_bytes(file_name: impl Into<String>, bytes: Vec<u8>) -> Self {
        let name = file_name.into();
        let ext = std::path::Path::new(&name)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        let (mime_type, as_photo) = match ext.as_str() {
            "jpg" | "jpeg" => ("image/jpeg", true),
            "png" => ("image/png", true),
            "webp" => ("image/webp", true),
            "gif" => ("image/gif", false),
            "mp4" => ("video/mp4", false),
            "webm" => ("video/webm", false),
            "pdf" => ("application/pdf", false),
            "mp3" => ("audio/mpeg", false),
            "ogg" => ("audio/ogg", false),
            _ => ("application/octet-stream", false),
        };
        Self {
            file_name: name,
            mime_type: mime_type.to_owned(),
            bytes,
            as_photo,
        }
    }
}

/// Request submitted to [`crate::history::backend::HistoryBackend::edit_message_media`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditMediaCaptionRequest {
    /// Updated caption text with UTF-16 entities (empty clears the caption).
    pub caption: TextWithEntities,
    /// Render caption above media (`messages.editMessage` `invert_media` flag).
    pub invert_media: bool,
    /// Apply media spoiler veil (`inputMediaPhoto` / `inputMediaDocument` / uploaded `spoiler`).
    pub spoiler: bool,
    /// Optional replacement media payload; `None` edits caption/flags on the existing media.
    pub replacement_media: Option<ReplacementMediaFile>,
}

/// Action emitted by [`EditCaptionBoxState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditCaptionAction {
    /// User confirmed saving caption/media changes.
    Submit {
        /// Target peer id.
        peer_id: PeerId,
        /// Target message id.
        msg_id: MsgId,
        /// Edit request payload.
        request: EditMediaCaptionRequest,
    },
    /// User cancelled editing.
    Cancelled,
}

/// Returns `true` if `media` supports caption / media editing via [`EditCaptionBoxState`].
#[must_use]
pub const fn media_supports_edit_caption(media: &HistoryMedia) -> bool {
    matches!(
        media,
        HistoryMedia::Photo(_)
            | HistoryMedia::Video(_)
            | HistoryMedia::Document(_)
            | HistoryMedia::Voice(_)
            | HistoryMedia::Album(_)
    )
}
