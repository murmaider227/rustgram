//! TGB-2998: the bytes of a sent document go to the media cache under the key of the document
//! the server returns, so the app does not download its own file again. tdesktop keeps the
//! local file of a sent document (`DocumentData::setLocation` / `setDataAndCache` after
//! `FileLoadTask`), so a just-sent voice message counts as loaded at once.
//!
//! Photos are left out: the server re-encodes their sizes, so the uploaded bytes are not the
//! bytes of any size the client downloads.

use tg_data::{
    DocumentKind, FullMsgId as DataFullMsgId, MessageMedia, MsgId as DataMsgId,
    PeerId as DataPeerId,
};
use tg_storage::cache::media_cache::MediaCacheTag;

use crate::media::document_to_location;
use crate::media_cache::{MAX_INLINE_AUTO_DOWNLOAD_BYTES, document_cache_key};
use crate::session::LiveSession;
use crate::upload::OutgoingMedia;

/// The main file of `media` when it is a document small enough for the media cache (a larger
/// one is streamed, never read from the cache whole). Copied before the upload consumes it.
pub(crate) fn seedable_bytes(media: &OutgoingMedia) -> Option<Vec<u8>> {
    let bytes = match media {
        OutgoingMedia::Document { bytes, .. }
        | OutgoingMedia::Video { bytes, .. }
        | OutgoingMedia::Voice { bytes, .. }
        | OutgoingMedia::Audio { bytes, .. }
        | OutgoingMedia::Gif { bytes, .. } => bytes,
        _ => return None,
    };
    let fits = !bytes.is_empty() && bytes.len() as u64 <= MAX_INLINE_AUTO_DOWNLOAD_BYTES;
    fits.then(|| bytes.clone())
}

/// Stores `bytes` as the file of the document in the sent message `msg_id`, if the server's
/// document has exactly that size (anything else is not the file that was uploaded).
pub(crate) fn seed(live: &LiveSession, peer: DataPeerId, msg_id: DataMsgId, bytes: &[u8]) {
    let doc =
        live.read_session(
            |s| match s.message(DataFullMsgId::new(peer, msg_id))?.media()? {
                MessageMedia::Document(doc) => Some(doc.as_ref().clone()),
                _ => None,
            },
        );
    let Some(doc) = doc else {
        return;
    };
    if usize::try_from(doc.size).ok() != Some(bytes.len()) {
        return;
    }
    let tag = match doc.kind {
        DocumentKind::Voice | DocumentKind::RoundVideo => MediaCacheTag::Voice,
        DocumentKind::Video | DocumentKind::Gif => MediaCacheTag::Video,
        DocumentKind::Audio => MediaCacheTag::Audio,
        DocumentKind::Sticker | DocumentKind::AnimatedSticker | DocumentKind::VideoSticker => {
            return;
        }
        _ => MediaCacheTag::Document,
    };
    let key = document_cache_key(document_to_location(&doc).dc_id, doc.id.0);
    if let Err(err) = live.media_cache().put_blob(&key, tag, bytes) {
        tracing::warn!("sent document not cached: {err}");
    }
}
