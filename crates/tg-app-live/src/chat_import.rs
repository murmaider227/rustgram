//! Live Telegram chat history import RPC workflow (`messages.checkHistoryImport`,
//! `messages.checkHistoryImportPeer`, `messages.initHistoryImport`,
//! `messages.uploadImportedMedia`, and `messages.startHistoryImport`).
//!
//! Port origin: Telegram API Layer 233 `messages.*HistoryImport` protocol &
//! `crates/tg-app/src/live/upload.rs` chunked file uploader.

use tg_media::transfer::download::CancelToken;
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tracing::warn;

use crate::session::LiveSession;
use crate::upload::LiveUploadConfig;
use tg_app_core::core_types::dto::chat_import::{
    ChatImportSummary, HistoryImportParsedInfo, ImportedMediaFile, ImportedMediaKind,
    ParsedImportArchive,
};
use tg_app_core::stand_in::types::PeerId;

/// Checks an external chat transcript head via `messages.checkHistoryImport`.
pub fn check_history_import(
    session: &LiveSession,
    import_head: &str,
) -> Result<HistoryImportParsedInfo, String> {
    if import_head.trim().is_empty() {
        return Err("Chat history header is empty.".to_owned());
    }
    let req = tl_fns::messages::CheckHistoryImport {
        import_head: import_head.to_owned(),
    };
    let tl_enums::messages::HistoryImportParsed::HistoryImportParsed(parsed) =
        session.invoke_rpc(req)?;
    Ok(HistoryImportParsedInfo {
        pm: parsed.pm,
        group: parsed.group,
        title: parsed.title,
    })
}

/// Validates whether `peer_id` can receive an imported chat history via
/// `messages.checkHistoryImportPeer` and returns the server confirmation prompt.
pub fn check_history_import_peer(session: &LiveSession, peer_id: PeerId) -> Result<String, String> {
    let peer = session
        .input_peer(peer_id)
        .ok_or_else(|| format!("Destination peer {} not found.", peer_id.0))?;
    let req = tl_fns::messages::CheckHistoryImportPeer { peer };
    let tl_enums::messages::CheckedHistoryImportPeer::CheckedHistoryImportPeer(checked) =
        session.invoke_rpc(req)?;
    Ok(checked.confirm_text)
}

/// Uploads the chat transcript file (`upload.saveFilePart` / `saveBigFilePart`) and calls
/// `messages.initHistoryImport`, returning the server-assigned `import_id`.
pub fn init_history_import(
    session: &LiveSession,
    peer_id: PeerId,
    history_file_name: &str,
    history_bytes: &[u8],
    media_count: i32,
) -> Result<i64, String> {
    let peer = session
        .input_peer(peer_id)
        .ok_or_else(|| format!("Destination peer {} not found.", peer_id.0))?;
    if history_bytes.is_empty() {
        return Err("Chat history transcript is empty.".to_owned());
    }

    let cancel = CancelToken::new();
    let cfg = LiveUploadConfig {
        is_photo: false,
        ..LiveUploadConfig::default()
    };
    let uploaded = session
        .upload_file_bytes(
            history_file_name,
            history_bytes.to_vec(),
            cfg,
            &cancel,
            |_| {},
        )
        .map_err(|e| format!("Failed uploading chat transcript: {e}"))?;

    let req = tl_fns::messages::InitHistoryImport {
        peer,
        file: uploaded.input_file,
        media_count: media_count.max(0),
    };
    let tl_enums::messages::HistoryImport::HistoryImport(import) = session.invoke_rpc(req)?;
    Ok(import.id)
}

/// Uploads one bundled media attachment via `upload.saveFilePart`/`saveBigFilePart` and
/// registers it with `messages.uploadImportedMedia`.
pub fn upload_imported_media(
    session: &LiveSession,
    peer_id: PeerId,
    import_id: i64,
    media: &ImportedMediaFile,
) -> Result<tl_enums::MessageMedia, String> {
    let peer = session
        .input_peer(peer_id)
        .ok_or_else(|| format!("Destination peer {} not found.", peer_id.0))?;
    if media.bytes.is_empty() {
        return Err(format!("Imported media '{}' is empty.", media.file_name));
    }

    let is_photo = media.kind == ImportedMediaKind::Photo;
    let cancel = CancelToken::new();
    let cfg = LiveUploadConfig {
        is_photo,
        ..LiveUploadConfig::default()
    };
    let uploaded = session
        .upload_file_bytes(&media.file_name, media.bytes.clone(), cfg, &cancel, |_| {})
        .map_err(|e| format!("Failed uploading media '{}': {e}", media.file_name))?;

    let input_media = if is_photo {
        tl_enums::InputMedia::UploadedPhoto(tl_types::InputMediaUploadedPhoto {
            spoiler: false,
            live_photo: false,
            file: uploaded.input_file,
            stickers: None,
            ttl_seconds: None,
            video: None,
        })
    } else {
        tl_enums::InputMedia::UploadedDocument(tl_types::InputMediaUploadedDocument {
            nosound_video: false,
            force_file: media.kind == ImportedMediaKind::Document,
            spoiler: false,
            file: uploaded.input_file,
            thumb: None,
            mime_type: media.mime_type.clone(),
            attributes: vec![tl_enums::DocumentAttribute::Filename(
                tl_types::DocumentAttributeFilename {
                    file_name: media.file_name.clone(),
                },
            )],
            stickers: None,
            video_cover: None,
            video_timestamp: None,
            ttl_seconds: None,
        })
    };

    let req = tl_fns::messages::UploadImportedMedia {
        peer,
        import_id,
        file_name: media.file_name.clone(),
        media: input_media,
    };
    session.invoke_rpc(req)
}

/// Finalizes and starts server-side ingestion via `messages.startHistoryImport`.
pub fn start_history_import(
    session: &LiveSession,
    peer_id: PeerId,
    import_id: i64,
) -> Result<bool, String> {
    let peer = session
        .input_peer(peer_id)
        .ok_or_else(|| format!("Destination peer {} not found.", peer_id.0))?;
    let req = tl_fns::messages::StartHistoryImport { peer, import_id };
    session.invoke_rpc(req)
}

/// Executes the full chat history import pipeline for `archive` into `peer_id`:
/// 1. Upload transcript & call `messages.initHistoryImport`
/// 2. Upload each bundled media file via `messages.uploadImportedMedia`
/// 3. Call `messages.startHistoryImport`
pub fn run_history_import(
    session: &LiveSession,
    peer_id: PeerId,
    archive: &ParsedImportArchive,
) -> Result<ChatImportSummary, String> {
    run_history_import_with_progress(session, peer_id, archive, |_, _| {})
}

/// Variant of [`run_history_import`] invoking `on_progress(done_steps, total_steps)`.
pub fn run_history_import_with_progress(
    session: &LiveSession,
    peer_id: PeerId,
    archive: &ParsedImportArchive,
    mut on_progress: impl FnMut(usize, usize),
) -> Result<ChatImportSummary, String> {
    let total_steps = 2 + archive.media_files.len();
    on_progress(0, total_steps);

    let import_id = init_history_import(
        session,
        peer_id,
        &archive.history_file_name,
        &archive.history_bytes,
        archive.media_count(),
    )?;
    on_progress(1, total_steps);

    let mut uploaded_media_count = 0usize;
    let mut skipped_media_count = 0usize;
    for (idx, media) in archive.media_files.iter().enumerate() {
        match upload_imported_media(session, peer_id, import_id, media) {
            Ok(_) => uploaded_media_count += 1,
            Err(err) => {
                warn!(
                    target: "tg_app::live::chat_import",
                    file = %media.file_name,
                    %err,
                    "skipping failed imported media attachment"
                );
                skipped_media_count += 1;
            }
        }
        on_progress(2 + idx, total_steps);
    }

    let started = start_history_import(session, peer_id, import_id)?;
    on_progress(total_steps, total_steps);

    Ok(ChatImportSummary {
        import_id,
        peer_id,
        uploaded_media_count,
        skipped_media_count,
        started,
    })
}
