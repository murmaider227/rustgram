//! Live Star Gift sticker source: registers `starGift.sticker` documents from
//! `payments.getStarGifts` / `payments.getSavedStarGifts` and downloads them on demand through
//! the shared sticker media path (`GiftButton::setDocument` in
//! `info/peer_gifts/info_peer_gifts_common.cpp`).

use std::sync::{Arc, Weak};

use tg_app_core::chat_helpers::backend::StickerMedia;
use tg_app_core::core_types::erased_rpc::ErasedRpcClient;
use tg_app_core::payments::gift_stickers::{GiftStickerSource, install_gift_sticker_source};
use tg_chat::DocumentId as ChatDocId;
use tg_session::SessionRuntime;
use tg_tl::enums as tl_enums;

use crate::bridge::LiveBridgeState;
use crate::chat_helpers::{
    LiveChatHelpersState, gift_sticker_media, ingest_gift_sticker_documents,
};
use crate::session::LiveSession;

struct LiveGiftStickerSource {
    runtime: Weak<SessionRuntime<ErasedRpcClient>>,
    state: Weak<LiveChatHelpersState>,
    bridge: Weak<LiveBridgeState>,
}

impl GiftStickerSource for LiveGiftStickerSource {
    fn gift_sticker_media(&self, document_id: u64) -> Option<StickerMedia> {
        let runtime = self.runtime.upgrade()?;
        let state = self.state.upgrade()?;
        gift_sticker_media(
            &state,
            runtime.rpc(),
            runtime.tokio_handle(),
            self.bridge.clone(),
            ChatDocId(document_id),
        )
    }
}

/// Collects the sticker document of every gift in `gifts`: `starGift.sticker` for regular gifts
/// and the model sticker (`starGiftAttributeModel.document`) for unique collectibles.
pub(crate) fn sticker_documents<'a>(
    gifts: impl IntoIterator<Item = &'a tl_enums::StarGift>,
) -> Vec<tl_enums::Document> {
    gifts
        .into_iter()
        .filter_map(|gift| match gift {
            tl_enums::StarGift::StarGift(g) => Some(g.sticker.clone()),
            tl_enums::StarGift::Unique(u) => u.attributes.iter().find_map(|attr| match attr {
                tl_enums::StarGiftAttribute::Model(m) => Some(m.document.clone()),
                _ => None,
            }),
        })
        .collect()
}

/// Registers `docs` and installs this session as the process-wide gift sticker source.
pub(crate) fn register_gift_stickers(live: &LiveSession, docs: &[tl_enums::Document]) {
    let state = Arc::clone(&live.chat_helpers.state);
    ingest_gift_sticker_documents(&state, docs);
    install_gift_sticker_source(Arc::new(LiveGiftStickerSource {
        runtime: Arc::downgrade(live.runtime()),
        state: Arc::downgrade(&state),
        bridge: Arc::downgrade(live.bridge_state()),
    }));
}

/// Registers the sticker documents of every gift in a `payments.savedStarGifts` response.
pub(crate) fn register_saved_gift_stickers(live: &LiveSession, saved: &[tl_enums::SavedStarGift]) {
    let docs = sticker_documents(saved.iter().map(|g| {
        let tl_enums::SavedStarGift::SavedStarGift(g) = g;
        &g.gift
    }));
    register_gift_stickers(live, &docs);
}
