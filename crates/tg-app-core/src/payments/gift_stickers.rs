//! Process-wide source of Star Gift sticker media (`starGift.sticker`).
//!
//! tdesktop resolves each gift card's sticker through `Delegate::sticker` /
//! `GiftStickerValue` (`info/peer_gifts/info_peer_gifts_common.cpp`) and plays it with the
//! regular Lottie sticker player. The live session installs a [`GiftStickerSource`] that knows
//! the full TL documents and downloads them through the shared sticker path; the UI asks for
//! media by document id and gets `None` until the bytes are ready.

use std::sync::{Arc, RwLock};

use crate::chat_helpers::backend::StickerMedia;

/// Supplies sticker media for Star Gift documents by document id.
pub trait GiftStickerSource: Send + Sync {
    /// Returns the sticker media for `document_id`, starting a download when needed.
    ///
    /// Returns `None` while the document is unknown or still downloading.
    fn gift_sticker_media(&self, document_id: u64) -> Option<StickerMedia>;
}

static SOURCE: RwLock<Option<Arc<dyn GiftStickerSource>>> = RwLock::new(None);

/// Installs `source` as the process-wide gift sticker source, replacing any previous one.
pub fn install_gift_sticker_source(source: Arc<dyn GiftStickerSource>) {
    if let Ok(mut guard) = SOURCE.write() {
        *guard = Some(source);
    }
}

/// Removes the installed gift sticker source (e.g. on logout).
pub fn clear_gift_sticker_source() {
    if let Ok(mut guard) = SOURCE.write() {
        *guard = None;
    }
}

/// Returns sticker media for `document_id` from the installed source, if any.
#[must_use]
pub fn gift_sticker_media(document_id: u64) -> Option<StickerMedia> {
    if document_id == 0 {
        return None;
    }
    let source = SOURCE.read().ok()?.as_ref().map(Arc::clone)?;
    source.gift_sticker_media(document_id)
}
