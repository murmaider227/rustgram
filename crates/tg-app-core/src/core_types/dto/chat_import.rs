//! Pure chat-history import DTOs (`boxes/chat_import.rs` model).

use crate::info::backend::PeerProfile;
use crate::stand_in::types::PeerId;

/// Maximum UTF-8 bytes sent in `messages.checkHistoryImport(import_head)`.
pub const MAX_IMPORT_HEAD_BYTES: usize = 16 * 1024;

/// Maximum leading lines extracted for `messages.checkHistoryImport(import_head)`.
pub const MAX_IMPORT_HEAD_LINES: usize = 100;

/// Maximum number of archive entries inspected from an imported zip file.
pub const MAX_ARCHIVE_ENTRIES: usize = 5_000;

/// Maximum byte size accepted for a single imported transcript `.txt` file (64 MiB).
pub const MAX_TRANSCRIPT_BYTES: usize = 64 * 1024 * 1024;

/// Maximum byte size accepted for a single bundled imported media file (50 MiB).
pub const MAX_MEDIA_ENTRY_BYTES: usize = 50 * 1024 * 1024;

/// External messaging app format detected from an exported chat transcript.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ImportSourceApp {
    /// WhatsApp export (`_chat.txt`, `[DD/MM/YYYY, HH:MM:SS] ...` or `MM/DD/YY, HH:MM - ...`).
    WhatsApp,
    /// LINE export (`[LINE] Chat history with ...`, `HH:MM\tSender\tBody`).
    Line,
    /// KakaoTalk export (`KakaoTalk Chats`, `[Sender] [HH:MM AM/PM] ...`).
    KakaoTalk,
    /// Unrecognized or generic text export (still forwarded to `checkHistoryImport`).
    #[default]
    Unknown,
}

impl ImportSourceApp {
    /// Human-readable source application label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::WhatsApp => "WhatsApp",
            Self::Line => "LINE",
            Self::KakaoTalk => "KakaoTalk",
            Self::Unknown => "External Chat",
        }
    }
}

/// Classification of a media file extracted from an imported chat archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImportedMediaKind {
    /// Compressed photo (`InputMediaUploadedPhoto`).
    Photo,
    /// Video attachment (`InputMediaUploadedDocument` with video MIME).
    Video,
    /// Audio / voice note (`InputMediaUploadedDocument` with audio MIME).
    Audio,
    /// Generic file / sticker (`InputMediaUploadedDocument`).
    Document,
}

/// Single media attachment extracted from an imported export archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedMediaFile {
    /// Basename referenced by the chat log (passed as `file_name` to `uploadImportedMedia`).
    pub file_name: String,
    /// Inferred MIME type.
    pub mime_type: String,
    /// Media upload category.
    pub kind: ImportedMediaKind,
    /// Raw file bytes ready for `upload.saveFilePart` / `saveBigFilePart`.
    pub bytes: Vec<u8>,
}

/// Parsed chat export archive ready to check and upload via `messages.*HistoryImport`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedImportArchive {
    /// Original archive or transcript filename for display.
    pub archive_name: String,
    /// Detected external messaging app.
    pub source_app: ImportSourceApp,
    /// Primary chat transcript filename (e.g. `"_chat.txt"`).
    pub history_file_name: String,
    /// Full UTF-8 chat transcript bytes.
    pub history_bytes: Vec<u8>,
    /// Leading transcript head snippet (`<= 100` lines / `<= 16 KiB`) for `checkHistoryImport`.
    pub import_head: String,
    /// Total line count in the transcript.
    pub line_count: usize,
    /// Bundled media files found alongside the transcript inside the zip.
    pub media_files: Vec<ImportedMediaFile>,
}

impl ParsedImportArchive {
    /// Number of bundled media attachments (`media_count` in `messages.initHistoryImport`).
    #[must_use]
    pub fn media_count(&self) -> i32 {
        self.media_files.len().min(i32::MAX as usize) as i32
    }
}

/// Server-parsed metadata returned by `messages.checkHistoryImport`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryImportParsedInfo {
    /// Export originated from a 1:1 private chat (`flags.0?true`).
    pub pm: bool,
    /// Export originated from a group chat (`flags.1?true`).
    pub group: bool,
    /// Optional chat title parsed by the server (`flags.2?string`).
    pub title: Option<String>,
}

impl HistoryImportParsedInfo {
    /// Returns `true` if `peer` is a valid destination for this export.
    #[must_use]
    pub fn allows_peer(&self, peer: &PeerProfile) -> bool {
        if !self.pm && !self.group {
            return true;
        }
        let is_group_like = peer.kind.is_group();
        (self.group && is_group_like) || (self.pm && !is_group_like)
    }

    /// Short description of supported destination types.
    #[must_use]
    pub fn target_summary(&self) -> &'static str {
        match (self.pm, self.group) {
            (true, true) => "Private chat or Group",
            (true, false) => "Private chat",
            (false, true) => "Group chat",
            (false, false) => "Any chat",
        }
    }
}

/// Final summary returned when a chat history import finishes (`startHistoryImport`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatImportSummary {
    /// Server-assigned `import_id` from `messages.initHistoryImport`.
    pub import_id: i64,
    /// Destination peer ID.
    pub peer_id: PeerId,
    /// Number of bundled media files uploaded via `messages.uploadImportedMedia`.
    pub uploaded_media_count: usize,
    /// Number of media files skipped due to upload failure.
    pub skipped_media_count: usize,
    /// Whether `messages.startHistoryImport` returned `true`.
    pub started: bool,
}

/// Current wizard stage inside [`ChatImportBoxState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatImportStep {
    /// Choose or inspect an export `.zip` / `.txt` file.
    SelectFile,
    /// Pick a compatible destination Telegram chat.
    SelectPeer,
    /// Review `messages.checkHistoryImportPeer` confirmation text and start import.
    Confirm,
    /// Import completed successfully.
    Completed,
}

/// Outcome emitted by [`ChatImportBoxState`] when an import completes or requests a file picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatImportBoxAction {
    /// Import succeeded for `peer_id`.
    Completed(ChatImportSummary),
    /// Refresh modal footer buttons after step change.
    RefreshModal,
}
