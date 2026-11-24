//! Media toggling helpers with automatic `400 FILE_REFERENCE_*` refresh & retry
//! (`messages.faveSticker`, `messages.saveRecentSticker`, `messages.saveGif`,
//! `account.saveRingtone`).
//!
//! Port origin: `Telegram/SourceFiles/api/api_toggling_media.h` and
//! `Telegram/SourceFiles/api/api_toggling_media.cpp`.

use std::sync::Arc;
use std::time::Duration;

use tg_api::RpcClient;
use tg_data::{FullMsgId as DataFullMsgId, MessageMedia};
use tg_media::transfer::{FileOrigin, TransferLocation};
use tg_tl::{RemoteCall, enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::convert::{to_data_msg, to_data_peer};
use crate::media::{is_file_reference_error, refresh_file_reference_async};
use crate::session::LiveSession;
use tg_app_core::stand_in::types::{MsgId, PeerId};

/// RPC timeout for media toggle operations.
const TOGGLE_MEDIA_TIMEOUT: Duration = Duration::from_secs(10);

/// Document descriptor passed to `api_toggling_media` helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TogglingDocument {
    /// Telegram document id.
    pub id: i64,
    /// Telegram document access hash.
    pub access_hash: i64,
    /// Current `file_reference` bytes (may be stale and refreshed automatically).
    pub file_reference: Vec<u8>,
    /// True if this document is a sticker.
    pub is_sticker: bool,
    /// True if this document is an animated GIF (`video/mp4` / `image/gif` gifv).
    pub is_gifv: bool,
    /// Optional origin for `FILE_REFERENCE_*` refresh.
    pub origin: FileOrigin,
}

impl TogglingDocument {
    /// Builds a TL `InputDocument` using `file_reference`.
    #[must_use]
    pub fn to_input_document(&self, file_reference: Vec<u8>) -> tl_enums::InputDocument {
        tl_enums::InputDocument::InputDocument(tl_types::InputDocument {
            id: self.id,
            access_hash: self.access_hash,
            file_reference,
        })
    }
}

/// Executes a document-toggling RPC (`ToggleExistingMedia` in `api_toggling_media.cpp:22-46`).
///
/// If the server rejects the request with `400 FILE_REFERENCE_*`, automatically invokes
/// [`refresh_file_reference_async`] using `doc.origin` and retries the request once if a newer
/// `file_reference` is obtained.
pub fn toggle_existing_media_with_refresh<R, F>(
    live: &LiveSession,
    doc: &TogglingDocument,
    build_req: F,
) -> Result<(R::Return, Vec<u8>), String>
where
    R: RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
    F: Fn(tl_enums::InputDocument) -> R + Send + Sync,
{
    let rpc = live.runtime().rpc().clone();
    let session = Arc::clone(live.runtime().session());
    let handle = live.runtime().tokio_handle().clone();
    let doc_clone = doc.clone();

    let fut = async move {
        let used_ref = doc_clone.file_reference.clone();
        let req = build_req(doc_clone.to_input_document(used_ref.clone()));
        match tokio::time::timeout(TOGGLE_MEDIA_TIMEOUT, rpc.invoke(&req)).await {
            Ok(Ok(resp)) => Ok((resp, used_ref)),
            Ok(Err(raw_err)) => {
                let inv_err: tg_api::InvocationError = raw_err;
                if !is_file_reference_error(&inv_err) {
                    return Err(format!("Media toggle RPC failed: {inv_err}"));
                }
                let loc = TransferLocation::Document {
                    id: doc_clone.id as u64,
                    access_hash: doc_clone.access_hash as u64,
                    file_reference: used_ref.clone(),
                    thumb_size: String::new(),
                };
                let refreshed =
                    refresh_file_reference_async(&rpc, &session, &doc_clone.origin, &loc)
                        .await
                        .map_err(|e| format!("Failed to refresh file reference: {e}"))?;

                if refreshed == used_ref {
                    return Err(format!("File reference unchanged after refresh: {inv_err}"));
                }
                let retry_req = build_req(doc_clone.to_input_document(refreshed.clone()));
                let retry_resp = tokio::time::timeout(TOGGLE_MEDIA_TIMEOUT, rpc.invoke(&retry_req))
                    .await
                    .map_err(|_| "Media toggle retry timed out.".to_owned())?
                    .map_err(|e| {
                        let inv: tg_api::InvocationError = e;
                        format!("Media toggle retry failed: {inv}")
                    })?;
                Ok((retry_resp, refreshed))
            }
            Err(_) => Err("Media toggle RPC timed out.".to_owned()),
        }
    };

    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::block_in_place(|| handle.block_on(fut))
    } else {
        handle.block_on(fut)
    }
}

/// Resolves a [`TogglingDocument`] from a message stored in `tg_data::Session`.
#[must_use]
pub fn document_from_session_message(
    live: &LiveSession,
    peer_id: PeerId,
    msg_id: MsgId,
) -> Option<TogglingDocument> {
    let data_peer = to_data_peer(peer_id);
    let data_msg = to_data_msg(msg_id);
    live.read_session(|session| {
        let msg = session.message(DataFullMsgId::new(data_peer, data_msg))?;
        let MessageMedia::Document(doc) = msg.media()? else {
            return None;
        };
        let is_sticker = doc.sticker.is_some()
            || matches!(
                doc.kind,
                tg_data::DocumentKind::Sticker
                    | tg_data::DocumentKind::AnimatedSticker
                    | tg_data::DocumentKind::VideoSticker
            );
        let is_gifv =
            matches!(doc.kind, tg_data::DocumentKind::Gif) || doc.mime_type == "image/gif";
        Some(TogglingDocument {
            id: doc.id.0 as i64,
            access_hash: doc.access_hash as i64,
            file_reference: doc.file_reference.clone(),
            is_sticker,
            is_gifv,
            origin: FileOrigin {
                peer_id: Some(data_peer.0),
                msg_id: Some(msg_id.0),
                sticker_set_id: doc.sticker.as_ref().map(|s| s.set_id),
            },
        })
    })
}

/// Toggles favorite status of a sticker (`Api::ToggleFavedSticker`, `messages.faveSticker`).
pub fn toggle_faved_sticker(
    live: &LiveSession,
    doc: &TogglingDocument,
    faved: bool,
) -> Result<bool, String> {
    if faved && !doc.is_sticker {
        return Err("Document is not a sticker.".to_owned());
    }
    let (ok, _) =
        toggle_existing_media_with_refresh(live, doc, |input_doc| tl_fns::messages::FaveSticker {
            id: input_doc,
            unfave: !faved,
        })?;
    live.bridge_state().notify_ui();
    Ok(ok)
}

/// Saves or removes a recent sticker (`Api::ToggleRecentSticker`, `messages.saveRecentSticker`).
pub fn toggle_recent_sticker(
    live: &LiveSession,
    doc: &TogglingDocument,
    saved: bool,
) -> Result<bool, String> {
    if !doc.is_sticker {
        return Err("Document is not a sticker.".to_owned());
    }
    let (ok, _) = toggle_existing_media_with_refresh(live, doc, |input_doc| {
        tl_fns::messages::SaveRecentSticker {
            attached: false,
            id: input_doc,
            unsave: !saved,
        }
    })?;
    live.bridge_state().notify_ui();
    Ok(ok)
}

/// Saves or unsaves a GIF (`Api::ToggleSavedGif`, `messages.saveGif`).
pub fn toggle_saved_gif(
    live: &LiveSession,
    doc: &TogglingDocument,
    saved: bool,
) -> Result<bool, String> {
    if saved && !doc.is_gifv {
        return Err("Document is not an animated GIF.".to_owned());
    }
    let (ok, _) =
        toggle_existing_media_with_refresh(live, doc, |input_doc| tl_fns::messages::SaveGif {
            id: input_doc,
            unsave: !saved,
        })?;
    live.bridge_state().notify_ui();
    Ok(ok)
}

/// Saves or unsaves a custom notification ringtone (`Api::ToggleSavedRingtone`, `account.saveRingtone`).
pub fn toggle_saved_ringtone(
    live: &LiveSession,
    doc: &TogglingDocument,
    saved: bool,
) -> Result<tl_enums::account::SavedRingtone, String> {
    let (res, _) =
        toggle_existing_media_with_refresh(live, doc, |input_doc| tl_fns::account::SaveRingtone {
            id: input_doc,
            unsave: !saved,
        })?;
    live.bridge_state().notify_ui();
    Ok(res)
}

/// Resolves the GIF document on `(peer_id, msg_id)` and toggles its saved status with
/// automatic `FILE_REFERENCE_*` refresh & retry.
pub fn toggle_saved_gif_from_message(
    live: &LiveSession,
    peer_id: PeerId,
    msg_id: MsgId,
    saved: bool,
) -> Result<bool, String> {
    let doc = document_from_session_message(live, peer_id, msg_id)
        .ok_or_else(|| "GIF document not found in message.".to_owned())?;
    toggle_saved_gif(live, &doc, saved)
}

/// Resolves the sticker document on `(peer_id, msg_id)` and toggles its favorite status with
/// automatic `FILE_REFERENCE_*` refresh & retry.
pub fn toggle_faved_sticker_from_message(
    live: &LiveSession,
    peer_id: PeerId,
    msg_id: MsgId,
    faved: bool,
) -> Result<bool, String> {
    let doc = document_from_session_message(live, peer_id, msg_id)
        .ok_or_else(|| "Sticker document not found in message.".to_owned())?;
    toggle_faved_sticker(live, &doc, faved)
}

/// Resolves the sticker document on `(peer_id, msg_id)` and toggles its recent-stickers status
/// with automatic `FILE_REFERENCE_*` refresh & retry.
pub fn toggle_recent_sticker_from_message(
    live: &LiveSession,
    peer_id: PeerId,
    msg_id: MsgId,
    saved: bool,
) -> Result<bool, String> {
    let doc = document_from_session_message(live, peer_id, msg_id)
        .ok_or_else(|| "Sticker document not found in message.".to_owned())?;
    toggle_recent_sticker(live, &doc, saved)
}

/// Resolves the audio/voice document on `(peer_id, msg_id)` and toggles its saved-ringtone status
/// with automatic `FILE_REFERENCE_*` refresh & retry.
pub fn toggle_saved_ringtone_from_message(
    live: &LiveSession,
    peer_id: PeerId,
    msg_id: MsgId,
    saved: bool,
) -> Result<bool, String> {
    let doc = document_from_session_message(live, peer_id, msg_id)
        .ok_or_else(|| "Audio document not found in message.".to_owned())?;
    let _ = toggle_saved_ringtone(live, &doc, saved)?;
    Ok(saved)
}
