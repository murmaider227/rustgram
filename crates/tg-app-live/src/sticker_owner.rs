//! Live RPC wiring for sticker / custom-emoji pack owner management actions.
//!
//! Port origin:
//! - `Telegram/SourceFiles/boxes/sticker_set_box.cpp`:
//!   - `stickers.renameStickerSet` (`sticker_set_box.cpp:347-363`)
//!   - `stickers.changeStickerPosition` (`sticker_set_box.cpp:1592-1613`)
//!   - `stickers.removeStickerFromSet` (`sticker_set_box.cpp:1939-1957`)
//!   - `stickers.deleteStickerSet` (`sticker_set_box.cpp:922`)
//! - `Telegram/SourceFiles/api/api_stickers_creator.cpp`:
//!   - `Api::DeleteStickerSet` (`api_stickers_creator.cpp:364-382`)

use tg_api::MAX_STICKER_SET_TITLE_LEN;
use tg_chat::DocumentId as ChatDocId;
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::session::LiveSession;
use tg_app_core::session_api::StickerSetPreview;

impl LiveSession {
    fn resolve_owner_input_sticker_set(
        &self,
        set_id: u64,
        short_name: &str,
    ) -> Result<tl_enums::InputStickerSet, String> {
        if let Some(cached) = self.resolve_input_sticker_set_cached(set_id, short_name) {
            return Ok(cached);
        }
        if !short_name.is_empty() {
            return Ok(tl_enums::InputStickerSet::ShortName(
                tl_types::InputStickerSetShortName {
                    short_name: short_name.to_owned(),
                },
            ));
        }
        if set_id != 0 {
            return Ok(tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
                id: set_id as i64,
                access_hash: 0,
            }));
        }
        Err("Sticker set identifier is missing.".to_owned())
    }

    fn resolve_owner_input_document(
        &self,
        set_id: u64,
        short_name: &str,
        document_id: u64,
    ) -> Result<tl_enums::InputDocument, String> {
        if document_id == 0 {
            return Err("Sticker document ID is missing.".to_owned());
        }
        if let Some(doc) = self.resolve_input_document(ChatDocId(document_id)) {
            return Ok(doc);
        }
        // Populate document metadata (`access_hash`, `file_reference`) by loading the set once.
        if let Ok(stickerset) = self.resolve_owner_input_sticker_set(set_id, short_name)
            && let Ok(tl_enums::messages::StickerSet::StickerSet(full)) =
                self.ch_invoke_rpc(tl_fns::messages::GetStickerSet {
                    stickerset,
                    hash: 0,
                })
        {
            let is_emoji = self.is_sticker_set_emoji_cached(set_id, short_name);
            let _ = self.ingest_sticker_set_preview(&full, is_emoji);
        }
        self.resolve_input_document(ChatDocId(document_id))
            .ok_or_else(|| {
                format!("Sticker document {document_id} metadata or file reference is not loaded.")
            })
    }

    fn refresh_sticker_set_from_owner_reply(
        &self,
        resp: tl_enums::messages::StickerSet,
        set_id: u64,
        short_name: &str,
    ) -> Result<Option<StickerSetPreview>, String> {
        let is_emoji_hint = self.is_sticker_set_emoji_cached(set_id, short_name);
        let preview = match resp {
            tl_enums::messages::StickerSet::StickerSet(full) => {
                Some(self.ingest_sticker_set_preview(&full, is_emoji_hint))
            }
            tl_enums::messages::StickerSet::NotModified => {
                let stickerset = self.resolve_owner_input_sticker_set(set_id, short_name)?;
                match self.ch_invoke_rpc(tl_fns::messages::GetStickerSet {
                    stickerset,
                    hash: 0,
                })? {
                    tl_enums::messages::StickerSet::StickerSet(full) => {
                        Some(self.ingest_sticker_set_preview(&full, is_emoji_hint))
                    }
                    tl_enums::messages::StickerSet::NotModified => None,
                }
            }
        };
        self.bridge_state().notify_ui();
        Ok(preview)
    }

    /// Renames an owned sticker or custom-emoji pack via `stickers.renameStickerSet` and refreshes
    /// the local set cache (`boxes/sticker_set_box.cpp:347-358`).
    pub(crate) fn rename_sticker_set_owner(
        &mut self,
        set_id: u64,
        short_name: &str,
        new_title: &str,
    ) -> Result<Option<StickerSetPreview>, String> {
        let trimmed = new_title.trim();
        if trimmed.is_empty() {
            return Err("Enter a pack name before saving.".to_owned());
        }
        if trimmed.chars().count() > MAX_STICKER_SET_TITLE_LEN {
            return Err(format!(
                "Pack name must be at most {MAX_STICKER_SET_TITLE_LEN} characters."
            ));
        }
        let stickerset = self.resolve_owner_input_sticker_set(set_id, short_name)?;
        let resp = self.ch_invoke_rpc(tl_fns::stickers::RenameStickerSet {
            stickerset,
            title: trimmed.to_owned(),
        })?;
        self.refresh_sticker_set_from_owner_reply(resp, set_id, short_name)
    }

    /// Reorders a sticker inside an owned pack via `stickers.changeStickerPosition` and refreshes
    /// the local set cache (`boxes/sticker_set_box.cpp:1592-1602`).
    pub(crate) fn change_sticker_position_owner(
        &mut self,
        set_id: u64,
        short_name: &str,
        document_id: u64,
        new_position: usize,
    ) -> Result<Option<StickerSetPreview>, String> {
        let position = i32::try_from(new_position)
            .map_err(|_| format!("Sticker position {new_position} exceeds protocol range."))?;
        let sticker = self.resolve_owner_input_document(set_id, short_name, document_id)?;
        let resp =
            self.ch_invoke_rpc(tl_fns::stickers::ChangeStickerPosition { sticker, position })?;
        self.refresh_sticker_set_from_owner_reply(resp, set_id, short_name)
    }

    /// Removes a sticker from an owned pack via `stickers.removeStickerFromSet` and refreshes
    /// the local set cache (`boxes/sticker_set_box.cpp:1939-1949`).
    pub(crate) fn remove_sticker_from_set_owner(
        &mut self,
        set_id: u64,
        short_name: &str,
        document_id: u64,
    ) -> Result<Option<StickerSetPreview>, String> {
        let sticker = self.resolve_owner_input_document(set_id, short_name, document_id)?;
        let resp = self.ch_invoke_rpc(tl_fns::stickers::RemoveStickerFromSet { sticker })?;
        self.refresh_sticker_set_from_owner_reply(resp, set_id, short_name)
    }

    /// Permanently deletes an owned sticker or custom-emoji pack via `stickers.deleteStickerSet`
    /// and evicts it from the local sticker store (`boxes/sticker_set_box.cpp:922` /
    /// `api/api_stickers_creator.cpp:369-376`).
    pub(crate) fn delete_sticker_set_owner(
        &mut self,
        set_id: u64,
        short_name: &str,
    ) -> Result<(), String> {
        let stickerset = self.resolve_owner_input_sticker_set(set_id, short_name)?;
        let deleted = self.ch_invoke_rpc(tl_fns::stickers::DeleteStickerSet { stickerset })?;
        if !deleted {
            return Err("Server declined deleting the sticker pack.".to_owned());
        }
        self.evict_deleted_sticker_set_locally(set_id, short_name);
        self.bridge_state().notify_ui();
        Ok(())
    }
}
