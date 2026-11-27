//! Per-account local storage (`map`, MTP authorization, session settings, drafts, locations,
//! stickers, prefs, trusted peers, bot storages, wallet engine records, etc.).
//!
//! Port of `Telegram/SourceFiles/storage/storage_account.h` and `storage_account.cpp`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::error::{Result, StorageError};
use crate::mtp_auth::MtpAuthorization;
use crate::peer_id::{DraftKey, PeerId, deserialize_peer_id, serialize_peer_id};
use crate::qdatastream::{QDataStreamReader, QDataStreamWriter, QtDateTime};
use crate::serialize::document::SerializedDocument;
use crate::serialize::image_location::{DownloadLocation, ImageLocation, StorageFileLocationType};
use crate::serialize::peer::SerializedPeer;
use crate::serialize::{TextTag, deserialize_tags, serialize_tags};
use crate::session_settings::StoredSessionSettings;
use crate::settings_scheme::{
    DEFAULT_CACHE_TOTAL_SIZE_LIMIT, DEFAULT_CACHE_TOTAL_TIME_LIMIT, ReadSettingsContext,
    read_all_settings, write_account_session_settings_blocks, write_mtp_authorization_block,
};
use crate::tdf::{
    APP_VERSION, FileKey, FileWriteDescriptor, clear_key, compose_data_string,
    compute_data_name_key, decrypt_local, generate_key, read_encrypted_file,
    read_encrypted_file_by_key, read_file, to_file_part, write_encrypted_file,
    write_encrypted_file_by_key,
};
use tg_crypto::{
    AUTH_KEY_SIZE, AuthKey, LOCAL_ENCRYPT_SALT_SIZE, StorageEncryptionKey, create_legacy_local_key,
};

/// Draft serialization tags (`storage_account.cpp`).
pub const MULTI_DRAFT_TAG_OLD: u64 = 0xFFFF_FFFF_FFFF_FF01;
/// Legacy multi-draft cursors tag.
pub const MULTI_DRAFT_CURSORS_TAG_OLD: u64 = 0xFFFF_FFFF_FFFF_FF02;
/// Multi-draft tag with 64-bit `DraftKey`.
pub const MULTI_DRAFT_TAG: u64 = 0xFFFF_FFFF_FFFF_FF03;
/// Multi-draft cursors tag with 64-bit `DraftKey`.
pub const MULTI_DRAFT_CURSORS_TAG: u64 = 0xFFFF_FFFF_FFFF_FF04;
/// Rich drafts tag (with full `reply` + `WebPageDraft`).
pub const RICH_DRAFTS_TAG: u64 = 0xFFFF_FFFF_FFFF_FF05;
/// Modern drafts tag (with `SuggestOptions` + `WebPageDraft`).
pub const DRAFTS_TAG_2: u64 = 0xFFFF_FFFF_FFFF_FF06;

/// Stickers file version tag (`kStickersVersionTag = quint32(-1)`).
pub const STICKERS_VERSION_TAG: u32 = u32::MAX;
/// Current stickers serialization version (`kStickersSerializeVersion = 4`).
pub const STICKERS_SERIALIZE_VERSION: i32 = 4;
/// Maximum saved sticker sets count (`kMaxSavedStickerSetsCount = 1000`).
pub const MAX_SAVED_STICKER_SETS_COUNT: i32 = 1000;

/// Current wallet engine per-key storage format version (`kWalletEngineFormatVersion = 1`).
pub const WALLET_ENGINE_FORMAT_VERSION: u32 = 1;

/// Special filename marker indicating a file lives in tdesktop's media cache.
pub const IN_MEDIA_CACHE_LOCATION: &str = "*media_cache*";

/// Local Storage Key tags stored inside each account's encrypted `map` (`lsk*` enum).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LskKeyType {
    /// User map (`0x00`).
    UserMap = 0x00,
    /// Per-peer drafts map (`0x01`).
    Draft = 0x01,
    /// Per-peer draft cursor positions (`0x02`).
    DraftPosition = 0x02,
    /// Legacy cached images (`0x03`).
    LegacyImages = 0x03,
    /// File locations map (`0x04`).
    Locations = 0x04,
    /// Legacy cached sticker images (`0x05`).
    LegacyStickerImages = 0x05,
    /// Legacy cached audios (`0x06`).
    LegacyAudios = 0x06,
    /// Legacy recent stickers (`0x07`).
    RecentStickersOld = 0x07,
    /// Very old single background (`0x08`).
    BackgroundOldOld = 0x08,
    /// Per-account encrypted user settings (`0x09`).
    UserSettings = 0x09,
    /// Recent hashtags and inline bots (`0x0a`).
    RecentHashtagsAndBots = 0x0a,
    /// Legacy installed stickers (`0x0b`).
    StickersOld = 0x0b,
    /// Legacy saved peers (`0x0c`).
    SavedPeersOld = 0x0c,
    /// Legacy report spam statuses (`0x0d`).
    ReportSpamStatusesOld = 0x0d,
    /// Legacy saved GIFs (`0x0e`).
    SavedGifsOld = 0x0e,
    /// Saved GIFs (`0x0f`).
    SavedGifs = 0x0f,
    /// Installed/featured/recent/archived stickers keys (`0x10`).
    StickersKeys = 0x10,
    /// Trusted peers and pay-for-message stars (`0x11`).
    TrustedPeers = 0x11,
    /// Faved stickers (`0x12`).
    FavedStickers = 0x12,
    /// Chat export settings (`0x13`).
    ExportSettings = 0x13,
    /// Legacy day/night background keys (`0x14`).
    BackgroundOld = 0x14,
    /// Serialized self user + about (`0x15`).
    SelfSerialized = 0x15,
    /// Installed/recent/archived masks keys (`0x16`).
    MasksKeys = 0x16,
    /// Installed/featured/archived custom emoji keys (`0x17`).
    CustomEmojiKeys = 0x17,
    /// Search suggestions (`0x18`).
    SearchSuggestions = 0x18,
    /// Webview storage tokens (`0x19`).
    WebviewTokens = 0x19,
    /// Round video placeholder JPEG (`0x1a`).
    RoundPlaceholder = 0x1a,
    /// Inline bots downloads (`0x1b`).
    InlineBotsDownloads = 0x1b,
    /// Media last playback positions (`0x1c`).
    MediaLastPlaybackPositions = 0x1c,
    /// Per-bot webview storage files (`0x1d`).
    BotStorages = 0x1d,
    /// Account key-value preferences (`0x1e`).
    Prefs = 0x1e,
    /// Legacy wallet key (`0x1f`).
    WalletKey = 0x1f,
    /// Legacy single-file wallet engine key (`0x20`).
    WalletEngineKey = 0x20,
    /// Per-key wallet engine encrypted records (`0x21`).
    WalletEngineStorages = 0x21,
}

impl LskKeyType {
    /// Convert raw `u32` tag into `LskKeyType`.
    #[must_use]
    pub const fn from_u32(v: u32) -> Option<Self> {
        match v {
            0x00 => Some(Self::UserMap),
            0x01 => Some(Self::Draft),
            0x02 => Some(Self::DraftPosition),
            0x03 => Some(Self::LegacyImages),
            0x04 => Some(Self::Locations),
            0x05 => Some(Self::LegacyStickerImages),
            0x06 => Some(Self::LegacyAudios),
            0x07 => Some(Self::RecentStickersOld),
            0x08 => Some(Self::BackgroundOldOld),
            0x09 => Some(Self::UserSettings),
            0x0a => Some(Self::RecentHashtagsAndBots),
            0x0b => Some(Self::StickersOld),
            0x0c => Some(Self::SavedPeersOld),
            0x0d => Some(Self::ReportSpamStatusesOld),
            0x0e => Some(Self::SavedGifsOld),
            0x0f => Some(Self::SavedGifs),
            0x10 => Some(Self::StickersKeys),
            0x11 => Some(Self::TrustedPeers),
            0x12 => Some(Self::FavedStickers),
            0x13 => Some(Self::ExportSettings),
            0x14 => Some(Self::BackgroundOld),
            0x15 => Some(Self::SelfSerialized),
            0x16 => Some(Self::MasksKeys),
            0x17 => Some(Self::CustomEmojiKeys),
            0x18 => Some(Self::SearchSuggestions),
            0x19 => Some(Self::WebviewTokens),
            0x1a => Some(Self::RoundPlaceholder),
            0x1b => Some(Self::InlineBotsDownloads),
            0x1c => Some(Self::MediaLastPlaybackPositions),
            0x1d => Some(Self::BotStorages),
            0x1e => Some(Self::Prefs),
            0x1f => Some(Self::WalletKey),
            0x20 => Some(Self::WalletEngineKey),
            0x21 => Some(Self::WalletEngineStorages),
            _ => None,
        }
    }
}

/// Complete contents of an account's encrypted `map` file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccountMap {
    /// Serialized self user (`Peer` + `about`), stored under `lskSelfSerialized`.
    pub self_serialized: Vec<u8>,
    /// Per-peer drafts file keys (`lskDraft`).
    pub drafts_map: BTreeMap<PeerId, FileKey>,
    /// Per-peer draft cursor file keys (`lskDraftPosition`).
    pub draft_cursors_map: BTreeMap<PeerId, FileKey>,
    /// Per-bot storage file keys (`lskBotStorages`).
    pub bot_storages_map: BTreeMap<PeerId, FileKey>,
    /// Per-key wallet engine storage file keys (`lskWalletEngineStorages`).
    pub wallet_engine_storages_map: BTreeMap<String, FileKey>,

    /// Key-value prefs file (`lskPrefs`).
    pub prefs_key: FileKey,
    /// Downloaded file locations file (`lskLocations`).
    pub locations_key: FileKey,
    /// Trusted peers file (`lskTrustedPeers`).
    pub trusted_peers_key: FileKey,
    /// Legacy recent stickers file (`lskRecentStickersOld`).
    pub recent_stickers_key_old: FileKey,
    /// Installed sticker sets file (`lskStickersKeys`).
    pub installed_stickers_key: FileKey,
    /// Featured sticker sets file (`lskStickersKeys`).
    pub featured_stickers_key: FileKey,
    /// Recent stickers file (`lskStickersKeys`).
    pub recent_stickers_key: FileKey,
    /// Faved stickers file (`lskFavedStickers`).
    pub faved_stickers_key: FileKey,
    /// Archived sticker sets file (`lskStickersKeys`).
    pub archived_stickers_key: FileKey,
    /// Saved GIFs file (`lskSavedGifs`).
    pub saved_gifs_key: FileKey,
    /// Installed masks file (`lskMasksKeys`).
    pub installed_masks_key: FileKey,
    /// Recent masks file (`lskMasksKeys`).
    pub recent_masks_key: FileKey,
    /// Archived masks file (`lskMasksKeys`).
    pub archived_masks_key: FileKey,
    /// Installed custom emoji sets file (`lskCustomEmojiKeys`).
    pub installed_custom_emoji_key: FileKey,
    /// Featured custom emoji sets file (`lskCustomEmojiKeys`).
    pub featured_custom_emoji_key: FileKey,
    /// Archived custom emoji sets file (`lskCustomEmojiKeys`).
    pub archived_custom_emoji_key: FileKey,
    /// Legacy day background key (`lskBackgroundOld`).
    pub legacy_background_key_day: FileKey,
    /// Legacy night background key (`lskBackgroundOld`).
    pub legacy_background_key_night: FileKey,
    /// Per-account settings file (`lskUserSettings`).
    pub settings_key: FileKey,
    /// Recent hashtags and inline bots file (`lskRecentHashtagsAndBots`).
    pub recent_hashtags_and_bots_key: FileKey,
    /// Export settings file (`lskExportSettings`).
    pub export_settings_key: FileKey,
    /// Search suggestions file (`lskSearchSuggestions`).
    pub search_suggestions_key: FileKey,
    /// Round video placeholder JPEG file (`lskRoundPlaceholder`).
    pub round_placeholder_key: FileKey,
    /// Inline bots downloads file (`lskInlineBotsDownloads`).
    pub inline_bots_downloads_key: FileKey,
    /// Media last playback positions file (`lskMediaLastPlaybackPositions`).
    pub media_last_playback_positions_key: FileKey,
    /// Legacy wallet key (`lskWalletKey`).
    pub wallet_legacy_key: FileKey,
    /// Legacy wallet engine key (`lskWalletEngineKey`).
    pub wallet_engine_legacy_key: FileKey,

    /// Bots webview storage token (`lskWebviewTokens`).
    pub webview_token_bots: Vec<u8>,
    /// Other webview storage token (`lskWebviewTokens`).
    pub webview_token_other: Vec<u8>,
}

impl AccountMap {
    /// Serialize the inner encrypted `map` stream (`Account::writeMap`).
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let mut w = QDataStreamWriter::new();

        if !self.self_serialized.is_empty() {
            w.write_u32(LskKeyType::SelfSerialized as u32);
            w.write_byte_array(&self.self_serialized);
        }
        if !self.drafts_map.is_empty() {
            w.write_u32(LskKeyType::Draft as u32);
            w.write_u32(self.drafts_map.len() as u32);
            for (&peer_id, &file_key) in &self.drafts_map {
                w.write_u64(file_key);
                w.write_u64(serialize_peer_id(peer_id));
            }
        }
        if !self.draft_cursors_map.is_empty() {
            w.write_u32(LskKeyType::DraftPosition as u32);
            w.write_u32(self.draft_cursors_map.len() as u32);
            for (&peer_id, &file_key) in &self.draft_cursors_map {
                w.write_u64(file_key);
                w.write_u64(serialize_peer_id(peer_id));
            }
        }
        if self.prefs_key != 0 {
            w.write_u32(LskKeyType::Prefs as u32);
            w.write_u64(self.prefs_key);
        }
        if self.locations_key != 0 {
            w.write_u32(LskKeyType::Locations as u32);
            w.write_u64(self.locations_key);
        }
        if self.trusted_peers_key != 0 {
            w.write_u32(LskKeyType::TrustedPeers as u32);
            w.write_u64(self.trusted_peers_key);
        }
        if self.recent_stickers_key_old != 0 {
            w.write_u32(LskKeyType::RecentStickersOld as u32);
            w.write_u64(self.recent_stickers_key_old);
        }
        if self.installed_stickers_key != 0
            || self.featured_stickers_key != 0
            || self.recent_stickers_key != 0
            || self.archived_stickers_key != 0
        {
            w.write_u32(LskKeyType::StickersKeys as u32);
            w.write_u64(self.installed_stickers_key);
            w.write_u64(self.featured_stickers_key);
            w.write_u64(self.recent_stickers_key);
            w.write_u64(self.archived_stickers_key);
        }
        if self.faved_stickers_key != 0 {
            w.write_u32(LskKeyType::FavedStickers as u32);
            w.write_u64(self.faved_stickers_key);
        }
        if self.saved_gifs_key != 0 {
            w.write_u32(LskKeyType::SavedGifs as u32);
            w.write_u64(self.saved_gifs_key);
        }
        if self.settings_key != 0 {
            w.write_u32(LskKeyType::UserSettings as u32);
            w.write_u64(self.settings_key);
        }
        if self.recent_hashtags_and_bots_key != 0 {
            w.write_u32(LskKeyType::RecentHashtagsAndBots as u32);
            w.write_u64(self.recent_hashtags_and_bots_key);
        }
        if self.export_settings_key != 0 {
            w.write_u32(LskKeyType::ExportSettings as u32);
            w.write_u64(self.export_settings_key);
        }
        if self.installed_masks_key != 0
            || self.recent_masks_key != 0
            || self.archived_masks_key != 0
        {
            w.write_u32(LskKeyType::MasksKeys as u32);
            w.write_u64(self.installed_masks_key);
            w.write_u64(self.recent_masks_key);
            w.write_u64(self.archived_masks_key);
        }
        if self.installed_custom_emoji_key != 0
            || self.featured_custom_emoji_key != 0
            || self.archived_custom_emoji_key != 0
        {
            w.write_u32(LskKeyType::CustomEmojiKeys as u32);
            w.write_u64(self.installed_custom_emoji_key);
            w.write_u64(self.featured_custom_emoji_key);
            w.write_u64(self.archived_custom_emoji_key);
        }
        if self.search_suggestions_key != 0 {
            w.write_u32(LskKeyType::SearchSuggestions as u32);
            w.write_u64(self.search_suggestions_key);
        }
        if !self.webview_token_bots.is_empty() || !self.webview_token_other.is_empty() {
            w.write_u32(LskKeyType::WebviewTokens as u32);
            w.write_byte_array(&self.webview_token_bots);
            w.write_byte_array(&self.webview_token_other);
        }
        if self.round_placeholder_key != 0 {
            w.write_u32(LskKeyType::RoundPlaceholder as u32);
            w.write_u64(self.round_placeholder_key);
        }
        if self.inline_bots_downloads_key != 0 {
            w.write_u32(LskKeyType::InlineBotsDownloads as u32);
            w.write_u64(self.inline_bots_downloads_key);
        }
        if self.media_last_playback_positions_key != 0 {
            w.write_u32(LskKeyType::MediaLastPlaybackPositions as u32);
            w.write_u64(self.media_last_playback_positions_key);
        }
        if !self.wallet_engine_storages_map.is_empty() {
            w.write_u32(LskKeyType::WalletEngineStorages as u32);
            w.write_u32(self.wallet_engine_storages_map.len() as u32);
            for (entry_key, &file_key) in &self.wallet_engine_storages_map {
                w.write_u64(file_key);
                w.write_qstring(entry_key);
            }
        }
        if !self.bot_storages_map.is_empty() {
            w.write_u32(LskKeyType::BotStorages as u32);
            w.write_u32(self.bot_storages_map.len() as u32);
            for (&peer_id, &file_key) in &self.bot_storages_map {
                w.write_u64(file_key);
                w.write_u64(serialize_peer_id(peer_id));
            }
        }

        w.into_bytes()
    }

    /// Deserialize the inner decrypted `map` stream (`Account::readMapWith`).
    pub fn deserialize(data: &[u8]) -> Result<Self> {
        let mut r = QDataStreamReader::new(data);
        let mut map = Self::default();

        while !r.at_end() {
            let raw_key_type = r.read_u32()?;
            let key_type =
                LskKeyType::from_u32(raw_key_type).ok_or(StorageError::UnknownTag(raw_key_type))?;
            match key_type {
                LskKeyType::Draft => {
                    let count = r.read_u32()?;
                    for _ in 0..count {
                        let key = r.read_u64()?;
                        let peer_raw = r.read_u64()?;
                        map.drafts_map.insert(deserialize_peer_id(peer_raw), key);
                    }
                }
                LskKeyType::SelfSerialized => {
                    map.self_serialized = r.read_byte_array()?;
                }
                LskKeyType::DraftPosition => {
                    let count = r.read_u32()?;
                    for _ in 0..count {
                        let key = r.read_u64()?;
                        let peer_raw = r.read_u64()?;
                        map.draft_cursors_map
                            .insert(deserialize_peer_id(peer_raw), key);
                    }
                }
                LskKeyType::LegacyImages
                | LskKeyType::LegacyStickerImages
                | LskKeyType::LegacyAudios => {
                    let count = r.read_u32()?;
                    for _ in 0..count {
                        let _key = r.read_u64()?;
                        let _first = r.read_u64()?;
                        let _second = r.read_u64()?;
                        let _size = r.read_i32()?;
                    }
                }
                LskKeyType::Prefs => map.prefs_key = r.read_u64()?,
                LskKeyType::Locations => map.locations_key = r.read_u64()?,
                LskKeyType::ReportSpamStatusesOld
                | LskKeyType::SavedGifsOld
                | LskKeyType::SavedPeersOld => {
                    let _ignored = r.read_u64()?;
                }
                LskKeyType::TrustedPeers => map.trusted_peers_key = r.read_u64()?,
                LskKeyType::RecentStickersOld => map.recent_stickers_key_old = r.read_u64()?,
                LskKeyType::BackgroundOldOld => {
                    map.legacy_background_key_day = r.read_u64()?;
                }
                LskKeyType::BackgroundOld => {
                    map.legacy_background_key_day = r.read_u64()?;
                    map.legacy_background_key_night = r.read_u64()?;
                }
                LskKeyType::UserSettings => map.settings_key = r.read_u64()?,
                LskKeyType::RecentHashtagsAndBots => {
                    map.recent_hashtags_and_bots_key = r.read_u64()?;
                }
                LskKeyType::StickersOld => map.installed_stickers_key = r.read_u64()?,
                LskKeyType::StickersKeys => {
                    map.installed_stickers_key = r.read_u64()?;
                    map.featured_stickers_key = r.read_u64()?;
                    map.recent_stickers_key = r.read_u64()?;
                    map.archived_stickers_key = r.read_u64()?;
                }
                LskKeyType::FavedStickers => map.faved_stickers_key = r.read_u64()?,
                LskKeyType::SavedGifs => map.saved_gifs_key = r.read_u64()?,
                LskKeyType::ExportSettings => map.export_settings_key = r.read_u64()?,
                LskKeyType::MasksKeys => {
                    map.installed_masks_key = r.read_u64()?;
                    map.recent_masks_key = r.read_u64()?;
                    map.archived_masks_key = r.read_u64()?;
                }
                LskKeyType::CustomEmojiKeys => {
                    map.installed_custom_emoji_key = r.read_u64()?;
                    map.featured_custom_emoji_key = r.read_u64()?;
                    map.archived_custom_emoji_key = r.read_u64()?;
                }
                LskKeyType::SearchSuggestions => map.search_suggestions_key = r.read_u64()?,
                LskKeyType::RoundPlaceholder => map.round_placeholder_key = r.read_u64()?,
                LskKeyType::InlineBotsDownloads => map.inline_bots_downloads_key = r.read_u64()?,
                LskKeyType::MediaLastPlaybackPositions => {
                    map.media_last_playback_positions_key = r.read_u64()?;
                }
                LskKeyType::WalletKey => map.wallet_legacy_key = r.read_u64()?,
                LskKeyType::WalletEngineKey => map.wallet_engine_legacy_key = r.read_u64()?,
                LskKeyType::WalletEngineStorages => {
                    let count = r.read_u32()?;
                    for _ in 0..count {
                        let key = r.read_u64()?;
                        let entry_key = r.read_qstring()?;
                        map.wallet_engine_storages_map.insert(entry_key, key);
                    }
                }
                LskKeyType::WebviewTokens => {
                    map.webview_token_bots = r.read_byte_array()?;
                    map.webview_token_other = r.read_byte_array()?;
                }
                LskKeyType::BotStorages => {
                    let count = r.read_u32()?;
                    for _ in 0..count {
                        let key = r.read_u64()?;
                        let peer_raw = r.read_u64()?;
                        map.bot_storages_map
                            .insert(deserialize_peer_id(peer_raw), key);
                    }
                }
                LskKeyType::UserMap => {
                    return Err(StorageError::UnknownTag(raw_key_type));
                }
            }
        }

        Ok(map)
    }

    /// Collect all live filenames inside the account directory (`Account::collectGoodNames`).
    #[must_use]
    pub fn collect_good_names(&self) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        for fixed in ["map0", "map1", "maps", "configs"] {
            names.insert(fixed.to_string());
        }
        let mut push_key = |k: FileKey| {
            if k == 0 {
                return;
            }
            let base = to_file_part(k);
            names.insert(format!("{base}0"));
            names.insert(format!("{base}1"));
            names.insert(format!("{base}s"));
        };
        let scalar_keys = [
            self.prefs_key,
            self.locations_key,
            self.settings_key,
            self.installed_stickers_key,
            self.featured_stickers_key,
            self.recent_stickers_key,
            self.faved_stickers_key,
            self.archived_stickers_key,
            self.recent_stickers_key_old,
            self.saved_gifs_key,
            self.legacy_background_key_night,
            self.legacy_background_key_day,
            self.recent_hashtags_and_bots_key,
            self.export_settings_key,
            self.trusted_peers_key,
            self.installed_masks_key,
            self.recent_masks_key,
            self.archived_masks_key,
            self.installed_custom_emoji_key,
            self.featured_custom_emoji_key,
            self.archived_custom_emoji_key,
            self.search_suggestions_key,
            self.round_placeholder_key,
            self.inline_bots_downloads_key,
            self.media_last_playback_positions_key,
        ];
        for k in scalar_keys {
            push_key(k);
        }
        for &k in self.drafts_map.values() {
            push_key(k);
        }
        for &k in self.draft_cursors_map.values() {
            push_key(k);
        }
        for &k in self.bot_storages_map.values() {
            push_key(k);
        }
        for &k in self.wallet_engine_storages_map.values() {
            push_key(k);
        }
        names
    }
}

/// Suggest post options packed in `kDraftsTag2` (`SuggestOptions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SuggestOptions {
    /// Whether suggest options are present.
    pub exists: bool,
    /// Whole currency amount.
    pub price_whole: u32,
    /// Fractional nano amount.
    pub price_nano: u32,
    /// Whether price is in TON (vs Stars).
    pub ton: bool,
    /// Scheduled date unix timestamp.
    pub date: u32,
}

impl SuggestOptions {
    fn serialize(self) -> (u64, u64) {
        let first = (u64::from(u8::from(self.exists)) << 63) | u64::from(self.date);
        let second = (u64::from(u8::from(self.ton)) << 63)
            | (u64::from(self.price_whole & 0x7FFF_FFFF) << 32)
            | u64::from(self.price_nano);
        (first, second)
    }

    fn deserialize(first: u64, second: u64) -> Self {
        Self {
            exists: (first >> 63) != 0,
            date: (first & 0xFFFF_FFFF) as u32,
            ton: (second >> 63) != 0,
            price_whole: ((second >> 32) & 0x7FFF_FFFF) as u32,
            price_nano: (second & 0xFFFF_FFFF) as u32,
        }
    }
}

/// Web page preview draft flags (`Data::WebPageDraft`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WebPageDraft {
    /// Manual webpage URL.
    pub url: String,
    /// Force large preview media.
    pub force_large_media: bool,
    /// Force small preview media.
    pub force_small_media: bool,
    /// Invert media position above text.
    pub invert: bool,
    /// Manually chosen preview.
    pub manual: bool,
    /// Preview explicitly removed/cancelled.
    pub removed: bool,
}

/// Cursor and scroll position for a draft input field (`MessageCursor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DraftCursor {
    /// Cursor position in UTF-16 code units.
    pub position: i32,
    /// Selection anchor in UTF-16 code units.
    pub anchor: i32,
    /// Scroll position (`0x3FFF_FFFF` / `kQFixedMax` by default).
    pub scroll: i32,
}

impl Default for DraftCursor {
    fn default() -> Self {
        Self {
            position: 0,
            anchor: 0,
            scroll: 0x3FFF_FFFF,
        }
    }
}

/// Stored draft entry for a single [`DraftKey`] within a peer's draft file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StoredDraft {
    /// Draft key (local, edit, topic, scheduled, etc.).
    pub key: DraftKey,
    /// Draft text.
    pub text: String,
    /// Formatting tags attached to `text`.
    pub tags: Vec<TextTag>,
    /// Peer ID of replied-to message.
    pub reply_peer_id: PeerId,
    /// Message ID of replied-to or edited message.
    pub reply_msg_id: i64,
    /// Post suggestion options (`kDraftsTag2`).
    pub suggest: SuggestOptions,
    /// Web page preview draft state.
    pub webpage: WebPageDraft,
    /// Optional cursor restored from `lskDraftPosition`.
    pub cursor: DraftCursor,
}

/// Local downloaded file location record (`Core::FileLocation`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StoredFileLocation {
    /// Media key `(first, second)`.
    pub media_key: (u64, u64),
    /// Local filesystem path or `IN_MEDIA_CACHE_LOCATION`.
    pub path: String,
    /// Platform security-scoped bookmark bytes.
    pub bookmark: Vec<u8>,
    /// Last modification timestamp.
    pub modified: QtDateTime,
    /// File size in bytes.
    pub size: u32,
}

/// Complete contents of the account's `lskLocations` file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StoredLocations {
    /// File location entries.
    pub locations: Vec<StoredFileLocation>,
    /// Media key aliases `(alias_key, target_key)`.
    pub aliases: Vec<((u64, u64), (u64, u64))>,
    /// Serialized downloads manager blob.
    pub downloads_serialized: Vec<u8>,
}

/// Peer trust flags stored in `lskTrustedPeers` (`Account::PeerTrustFlag`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PeerTrustFlags(pub u8);

impl PeerTrustFlags {
    /// Disallow opening games without prompt (`1 << 0`).
    pub const NO_OPEN_GAME: u8 = 1 << 0;
    /// Trusted for payments (`1 << 1`).
    pub const PAYMENT: u8 = 1 << 1;
    /// Trusted for opening WebViews (`1 << 2`).
    pub const OPEN_WEB_VIEW: u8 = 1 << 2;
}

/// Contents of the `lskTrustedPeers` file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrustedPeersData {
    /// Per-peer trust flag masks.
    pub trusted_peers: BTreeMap<PeerId, PeerTrustFlags>,
    /// Per-peer trusted stars-per-message thresholds.
    pub trusted_pay_per_message: BTreeMap<PeerId, i32>,
}

/// Contents of the `lskRecentHashtagsAndBots` file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RecentHashtagsAndBots {
    /// Recently sent hashtags `(tag, use_count)`.
    pub write_hashtags: Vec<(String, u16)>,
    /// Recently searched hashtags `(tag, use_count)`.
    pub search_hashtags: Vec<(String, u16)>,
    /// Recent inline bots (`SerializedPeer`).
    pub inline_bots: Vec<SerializedPeer>,
}

/// Contents of the `lskSearchSuggestions` file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchSuggestionsBlobs {
    /// Top peers serialized blob.
    pub top_peers: Vec<u8>,
    /// Recent peers serialized blob.
    pub recent_peers: Vec<u8>,
    /// Recent settings searches serialized blob.
    pub settings_searches: Vec<u8>,
    /// Top guest chat bots serialized blob.
    pub guest_chat_bots: Vec<u8>,
    /// Recent money recipients serialized blob.
    pub money_recipients: Vec<u8>,
}

/// Single-peer export target (`Export::Settings::singlePeer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportSinglePeer {
    /// Export all selected chats (`kSinglePeerTypeEmpty = 0`).
    #[default]
    Empty,
    /// Export Saved Messages (`kSinglePeerTypeSelf = 4`).
    SelfPeer,
    /// Export a single user (`kSinglePeerTypeUser = 9`).
    User {
        /// Bare user ID.
        user_id: u64,
        /// Access hash.
        access_hash: u64,
    },
    /// Export a single basic chat (`kSinglePeerTypeChat = 10`).
    Chat {
        /// Bare chat ID.
        chat_id: u64,
    },
    /// Export a single channel/supergroup (`kSinglePeerTypeChannel = 11`).
    Channel {
        /// Bare channel ID.
        channel_id: u64,
        /// Access hash.
        access_hash: u64,
    },
}

/// Contents of the `lskExportSettings` file (`Export::Settings`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StoredExportSettings {
    /// Export types bitmask.
    pub types: u32,
    /// Full chats types bitmask.
    pub full_chats: u32,
    /// Media types bitmask.
    pub media_types: u32,
    /// Media size limit in bytes.
    pub media_size_limit: u32,
    /// Output format code (HTML / JSON / both).
    pub format: u32,
    /// Output directory path.
    pub path: String,
    /// Unix timestamp when takeout is available.
    pub available_at: u32,
    /// Optional single peer target.
    pub single_peer: ExportSinglePeer,
    /// Date range start unix timestamp.
    pub single_peer_from: i32,
    /// Date range end unix timestamp.
    pub single_peer_till: i32,
}

/// Serialized sticker set stored inside stickers/masks/custom-emoji files (`Account::writeStickerSet`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StoredStickerSet {
    /// Sticker set ID.
    pub id: u64,
    /// Access hash.
    pub access_hash: u64,
    /// Set hash.
    pub hash: u64,
    /// Display title.
    pub title: String,
    /// Short name slug.
    pub short_name: String,
    /// Sticker count when `not_loaded` is true, or `stickers.len()` when loaded.
    pub count: i32,
    /// Raw `StickersSetFlags` bitmask.
    pub flags: i32,
    /// Install unix timestamp.
    pub install_date: i32,
    /// Thumbnail document ID (`version > 2`).
    pub thumbnail_document_id: u64,
    /// Thumbnail sticker type (`0` = Webp, `1` = Tgs, `2` = Webm; `version > 3`).
    pub thumbnail_type: i32,
    /// Thumbnail location.
    pub thumbnail_location: ImageLocation,
    /// Loaded sticker documents (empty if `count < 0` / NotLoaded).
    pub stickers: Vec<SerializedDocument>,
    /// Optional per-sticker recent usage timestamps.
    pub dates: Vec<i32>,
    /// Emoji-to-document-IDs index.
    pub emoji: Vec<(String, Vec<u64>)>,
}

/// Complete sticker sets file payload (`sets` + `order`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StoredStickerSetsFile {
    /// Sticker sets stored in the file.
    pub sets: Vec<StoredStickerSet>,
    /// Ordered list of sticker set IDs.
    pub order: Vec<u64>,
}

/// Per-account local storage handle (`Storage::Account`).
#[derive(Debug, Clone)]
pub struct AccountStorage {
    tdata_path: PathBuf,
    data_name: String,
    data_name_key: FileKey,
    base_path: PathBuf,
    temp_path: PathBuf,
    database_path: PathBuf,
    local_key: AuthKey,
    map: AccountMap,
    old_map_version: i32,
}

impl AccountStorage {
    /// Open or prepare an `AccountStorage` for slot `index` (e.g. `0` for `"data"`, `1` for `"data#2"`).
    ///
    /// If `map` already exists on disk, reads and decrypts it; otherwise initializes an empty map.
    pub fn open(
        tdata_path: impl Into<PathBuf>,
        domain_data_name: &str,
        index: i32,
        local_key: AuthKey,
    ) -> Result<Self> {
        let tdata_path = tdata_path.into();
        let data_name = compose_data_string(domain_data_name, index);
        let data_name_key = compute_data_name_key(&data_name);
        let base_path = tdata_path.join(to_file_part(data_name_key));
        let temp_path = tdata_path.join(format!("temp_{data_name}"));
        let database_path = tdata_path.join(format!("user_{data_name}"));

        let mut account = Self {
            tdata_path,
            data_name,
            data_name_key,
            base_path,
            temp_path,
            database_path,
            local_key,
            map: AccountMap::default(),
            old_map_version: 0,
        };

        match account.read_map() {
            Ok(()) => {}
            Err(StorageError::NotFound(_)) => {}
            Err(e) => return Err(e),
        }

        Ok(account)
    }

    /// Open a legacy pre-multi-account `map` where the local key itself is wrapped inside
    /// `map`'s first two blobs using `CreateLegacyLocalKey(passcode, salt)` (`Account::legacyStart`).
    pub fn open_legacy(
        tdata_path: impl Into<PathBuf>,
        domain_data_name: &str,
        passcode: &[u8],
    ) -> Result<Self> {
        let tdata_path = tdata_path.into();
        let data_name = compose_data_string(domain_data_name, 0);
        let data_name_key = compute_data_name_key(&data_name);
        let base_path = tdata_path.join(to_file_part(data_name_key));
        let temp_path = tdata_path.join(format!("temp_{data_name}"));
        let database_path = tdata_path.join(format!("user_{data_name}"));

        let file = read_file("map", &base_path)?;
        let mut r = file.stream();
        let legacy_salt = r.read_byte_array()?;
        let legacy_key_encrypted = r.read_byte_array()?;
        let map_encrypted = r.read_byte_array()?;

        if legacy_salt.len() != LOCAL_ENCRYPT_SALT_SIZE {
            return Err(StorageError::CorruptData(format!(
                "bad legacy salt size in map: {}",
                legacy_salt.len()
            )));
        }

        let passcode_key = create_legacy_local_key(passcode, &legacy_salt);
        let key_inner = decrypt_local(&legacy_key_encrypted, &passcode_key)
            .map_err(|_| StorageError::IncorrectPasscode)?;
        if key_inner.len() != AUTH_KEY_SIZE {
            return Err(StorageError::CorruptData(
                "bad legacy local key size".into(),
            ));
        }
        let mut key_bytes = [0u8; AUTH_KEY_SIZE];
        key_bytes.copy_from_slice(&key_inner);
        let local_key = AuthKey::new(key_bytes);

        let map_inner = decrypt_local(&map_encrypted, &local_key)?;
        let map = AccountMap::deserialize(&map_inner)?;

        Ok(Self {
            tdata_path,
            data_name,
            data_name_key,
            base_path,
            temp_path,
            database_path,
            local_key,
            map,
            old_map_version: file.version,
        })
    }

    /// Account data name (e.g., `"data"`, `"data#2"`).
    #[must_use]
    pub fn data_name(&self) -> &str {
        &self.data_name
    }

    /// 64-bit `FileKey` derived from `MD5(data_name)`.
    #[must_use]
    pub const fn data_name_key(&self) -> FileKey {
        self.data_name_key
    }

    /// Path to `<tdata>/<ToFilePart(dataNameKey)>/`.
    #[must_use]
    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

    /// Path to `<tdata>/temp_<dataName>/`.
    #[must_use]
    pub fn temp_path(&self) -> &Path {
        &self.temp_path
    }

    /// Path to `<tdata>/user_<dataName>/`.
    #[must_use]
    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    /// Path to `<tdata>/user_<dataName>/cache`.
    #[must_use]
    pub fn cache_path(&self) -> PathBuf {
        self.database_path.join("cache")
    }

    /// Path to `<tdata>/user_<dataName>/media_cache`.
    #[must_use]
    pub fn cache_big_file_path(&self) -> PathBuf {
        self.database_path.join("media_cache")
    }

    /// Derive `lib_storage` [`StorageEncryptionKey`] for the binlog caches (`Account::cacheKey`).
    #[must_use]
    pub fn cache_key(&self) -> StorageEncryptionKey {
        match StorageEncryptionKey::new(self.local_key.data().to_vec()) {
            Ok(key) => key,
            Err(_) => unreachable!("256-byte AuthKey is always a valid v1 StorageEncryptionKey"),
        }
    }

    /// The decrypted 256-byte local key.
    #[must_use]
    pub fn local_key(&self) -> &AuthKey {
        &self.local_key
    }

    /// Read-only view of the account's [`AccountMap`].
    #[must_use]
    pub fn map(&self) -> &AccountMap {
        &self.map
    }

    /// Mutable view of the account's [`AccountMap`].
    pub fn map_mut(&mut self) -> &mut AccountMap {
        &mut self.map
    }

    /// TDF app version of the loaded `map` file.
    #[must_use]
    pub const fn old_map_version(&self) -> i32 {
        self.old_map_version
    }

    /// Read and decrypt `<base_path>/maps` (`Account::readMapWith`).
    pub fn read_map(&mut self) -> Result<()> {
        let file = read_file("map", &self.base_path)?;
        let mut r = file.stream();
        let _legacy_salt = r.read_byte_array()?;
        let _legacy_key_encrypted = r.read_byte_array()?;
        let map_encrypted = r.read_byte_array()?;

        let map_inner = decrypt_local(&map_encrypted, &self.local_key)?;
        self.map = AccountMap::deserialize(&map_inner)?;
        self.old_map_version = file.version;
        Ok(())
    }

    /// Encrypt and write `<base_path>/maps` (`Account::writeMap`).
    pub fn write_map(&self) -> Result<()> {
        let mut file = self.build_map_descriptor()?;
        file.finish()
    }

    /// Build a [`FileWriteDescriptor`] for `<base_path>/maps` ready for immediate or deferred write.
    pub(crate) fn build_map_descriptor(&self) -> Result<FileWriteDescriptor> {
        let mut file = FileWriteDescriptor::new("map", &self.base_path);
        // Modern maps write two empty/null QByteArrays for legacySalt and legacyKeyEncrypted.
        file.write_data(&[]);
        file.write_data(&[]);
        file.write_encrypted(&self.map.serialize(), &self.local_key)?;
        Ok(file)
    }

    /// Build a [`crate::write_manager::WriteEntry`] for `<base_path>/maps`.
    pub fn prepare_map_write_entry(&self) -> Result<crate::write_manager::WriteEntry> {
        self.build_map_descriptor()?.into_write_entry()
    }

    /// Serialize `locations` into an encrypted [`crate::write_manager::WriteEntry`] (or file keys
    /// to remove if empty), updating `map.locations_key` and returning `(entry, removed_files, map_changed)`.
    pub(crate) fn prepare_locations_write(
        &mut self,
        locations: &StoredLocations,
    ) -> Result<(Option<crate::write_manager::WriteEntry>, Vec<PathBuf>, bool)> {
        if locations.locations.is_empty() && locations.downloads_serialized.is_empty() {
            if self.map.locations_key != 0 {
                let files = key_file_paths(self.map.locations_key, &self.base_path);
                self.map.locations_key = 0;
                return Ok((None, files, true));
            }
            return Ok((None, Vec::new(), false));
        }
        let mut map_changed = false;
        if self.map.locations_key == 0 {
            self.map.locations_key = generate_key(&self.base_path);
            map_changed = true;
        }
        let payload = serialize_locations_payload(locations);
        let mut desc = FileWriteDescriptor::from_key(self.map.locations_key, &self.base_path);
        desc.write_encrypted(&payload, &self.local_key)?;
        Ok((Some(desc.into_write_entry()?), Vec::new(), map_changed))
    }

    /// Serialize `prefs` into an encrypted [`crate::write_manager::WriteEntry`] (or file keys
    /// to remove if empty), updating `map.prefs_key` and returning `(entry, removed_files, map_changed)`.
    pub(crate) fn prepare_prefs_write(
        &mut self,
        prefs: &BTreeMap<Vec<u8>, Vec<u8>>,
    ) -> Result<(Option<crate::write_manager::WriteEntry>, Vec<PathBuf>, bool)> {
        if prefs.is_empty() {
            if self.map.prefs_key != 0 {
                let files = key_file_paths(self.map.prefs_key, &self.base_path);
                self.map.prefs_key = 0;
                return Ok((None, files, true));
            }
            return Ok((None, Vec::new(), false));
        }
        let mut map_changed = false;
        if self.map.prefs_key == 0 {
            self.map.prefs_key = generate_key(&self.base_path);
            map_changed = true;
        }
        let payload = serialize_prefs_payload(prefs);
        let mut desc = FileWriteDescriptor::from_key(self.map.prefs_key, &self.base_path);
        desc.write_encrypted(&payload, &self.local_key)?;
        Ok((Some(desc.into_write_entry()?), Vec::new(), map_changed))
    }

    /// Serialize `search_suggestions` into an encrypted [`crate::write_manager::WriteEntry`] (or
    /// file keys to remove if empty), updating `map.search_suggestions_key` and returning
    /// `(entry, removed_files, map_changed)`.
    pub(crate) fn prepare_search_suggestions_write(
        &mut self,
        data: &SearchSuggestionsBlobs,
    ) -> Result<(Option<crate::write_manager::WriteEntry>, Vec<PathBuf>, bool)> {
        if data.top_peers.is_empty()
            && data.recent_peers.is_empty()
            && data.settings_searches.is_empty()
            && data.guest_chat_bots.is_empty()
            && data.money_recipients.is_empty()
        {
            if self.map.search_suggestions_key != 0 {
                let files = key_file_paths(self.map.search_suggestions_key, &self.base_path);
                self.map.search_suggestions_key = 0;
                return Ok((None, files, true));
            }
            return Ok((None, Vec::new(), false));
        }
        let mut map_changed = false;
        if self.map.search_suggestions_key == 0 {
            self.map.search_suggestions_key = generate_key(&self.base_path);
            map_changed = true;
        }
        let payload = serialize_search_suggestions_payload(data);
        let mut desc =
            FileWriteDescriptor::from_key(self.map.search_suggestions_key, &self.base_path);
        desc.write_encrypted(&payload, &self.local_key)?;
        Ok((Some(desc.into_write_entry()?), Vec::new(), map_changed))
    }

    /// Read MTProto authorization from `<tdata>/<ToFilePart(dataNameKey)>s` (`Account::readMtpData`).
    pub fn read_mtp_data(&self) -> Result<Option<MtpAuthorization>> {
        let name = to_file_part(self.data_name_key);
        let file = match read_encrypted_file(&name, &self.tdata_path, &self.local_key) {
            Ok(f) => f,
            Err(StorageError::NotFound(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut ctx = ReadSettingsContext::default();
        read_all_settings(&file.data, file.version, &mut ctx)?;

        if !ctx.mtp_authorization.is_empty() {
            Ok(Some(MtpAuthorization::deserialize(&ctx.mtp_authorization)?))
        } else if ctx.mtp_legacy_main_dc_id != 0 || !ctx.mtp_legacy_keys.is_empty() {
            Ok(Some(MtpAuthorization {
                user_id: ctx.mtp_legacy_user_id as u64,
                main_dc_id: ctx.mtp_legacy_main_dc_id,
                keys: ctx.mtp_legacy_keys,
                keys_to_destroy: Vec::new(),
            }))
        } else {
            Ok(None)
        }
    }

    /// Write MTProto authorization to `<tdata>/<ToFilePart(dataNameKey)>s` (`Account::writeMtpData`).
    pub fn write_mtp_data(&self, auth: &MtpAuthorization) -> Result<()> {
        let serialized = auth.serialize();
        let payload = write_mtp_authorization_block(&serialized);
        let name = to_file_part(self.data_name_key);
        write_encrypted_file(&name, &self.tdata_path, &payload, &self.local_key)
    }

    /// Read encrypted `MTP::Config` blob from `<base_path>/configs` (`Account::readMtpConfig`).
    pub fn read_mtp_config(&self) -> Result<Option<Vec<u8>>> {
        match read_encrypted_file("config", &self.base_path, &self.local_key) {
            Ok(file) => {
                let mut r = file.stream();
                Ok(Some(r.read_byte_array()?))
            }
            Err(StorageError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Write encrypted `MTP::Config` blob to `<base_path>/configs` (`Account::writeMtpConfig`).
    pub fn write_mtp_config(&self, serialized_config: &[u8]) -> Result<()> {
        let mut w = QDataStreamWriter::with_capacity(4 + serialized_config.len());
        w.write_byte_array(serialized_config);
        write_encrypted_file("config", &self.base_path, w.as_bytes(), &self.local_key)
    }

    /// Read per-account encrypted settings (`lskUserSettings`) into a [`ReadSettingsContext`].
    pub fn read_session_settings(&self) -> Result<Option<ReadSettingsContext>> {
        if self.map.settings_key == 0 {
            return Ok(None);
        }
        let file =
            read_encrypted_file_by_key(self.map.settings_key, &self.base_path, &self.local_key)?;
        let mut ctx = ReadSettingsContext {
            legacy_has_custom_day_background: self.map.legacy_background_key_day != 0,
            cache_total_size_limit: DEFAULT_CACHE_TOTAL_SIZE_LIMIT,
            cache_total_time_limit: DEFAULT_CACHE_TOTAL_TIME_LIMIT,
            cache_big_file_total_size_limit: DEFAULT_CACHE_TOTAL_SIZE_LIMIT,
            cache_big_file_total_time_limit: DEFAULT_CACHE_TOTAL_TIME_LIMIT,
            ..Default::default()
        };
        read_all_settings(&file.data, file.version, &mut ctx)?;
        Ok(Some(ctx))
    }

    /// Write per-account encrypted settings (`Account::writeSessionSettings`), updating `map.settings_key`.
    pub fn write_session_settings(
        &mut self,
        cache_total_size_limit: i64,
        cache_total_time_limit: i32,
        cache_big_file_total_size_limit: i64,
        cache_big_file_total_time_limit: i32,
        session_settings_blob: &[u8],
        recent_stickers: &[(u64, u16)],
    ) -> Result<()> {
        if self.map.settings_key == 0 {
            self.map.settings_key = generate_key(&self.base_path);
            self.write_map()?;
        }
        let payload = write_account_session_settings_blocks(
            cache_total_size_limit,
            cache_total_time_limit,
            cache_big_file_total_size_limit,
            cache_big_file_total_time_limit,
            session_settings_blob,
            recent_stickers,
        );
        write_encrypted_file_by_key(
            self.map.settings_key,
            &self.base_path,
            &payload,
            &self.local_key,
        )
    }

    /// Read and parse per-account [`StoredSessionSettings`] from `dbiSessionSettings` (`0x4d`)
    /// along with its enclosing [`ReadSettingsContext`].
    pub fn read_parsed_session_settings(
        &self,
    ) -> Result<Option<(StoredSessionSettings, ReadSettingsContext)>> {
        let Some(ctx) = self.read_session_settings()? else {
            return Ok(None);
        };
        let parsed = StoredSessionSettings::deserialize(&ctx.session_settings)?;
        Ok(Some((parsed, ctx)))
    }

    /// Serialize and persist per-account [`StoredSessionSettings`] into `lskUserSettings`,
    /// preserving any existing cache limits and recent-stickers preload if already present.
    pub fn write_parsed_session_settings(
        &mut self,
        settings: &StoredSessionSettings,
    ) -> Result<()> {
        let (size_limit, time_limit, big_size_limit, big_time_limit, recent_stickers) =
            match self.read_session_settings()? {
                Some(ctx) => (
                    ctx.cache_total_size_limit,
                    ctx.cache_total_time_limit,
                    ctx.cache_big_file_total_size_limit,
                    ctx.cache_big_file_total_time_limit,
                    ctx.recent_stickers_preload,
                ),
                None => (
                    DEFAULT_CACHE_TOTAL_SIZE_LIMIT,
                    DEFAULT_CACHE_TOTAL_TIME_LIMIT,
                    DEFAULT_CACHE_TOTAL_SIZE_LIMIT,
                    DEFAULT_CACHE_TOTAL_TIME_LIMIT,
                    Vec::new(),
                ),
            };
        let blob = settings.serialize();
        self.write_session_settings(
            size_limit,
            time_limit,
            big_size_limit,
            big_time_limit,
            &blob,
            &recent_stickers,
        )
    }

    /// Write drafts for `peer_id` in `kDraftsTag2` format (`Account::writeDrafts`).
    pub fn write_drafts(&mut self, peer_id: PeerId, drafts: &[StoredDraft]) -> Result<()> {
        if drafts.is_empty() {
            if let Some(key) = self.map.drafts_map.remove(&peer_id) {
                clear_key(key, &self.base_path);
                self.write_map()?;
            }
            return Ok(());
        }
        let file_key = *self
            .map
            .drafts_map
            .entry(peer_id)
            .or_insert_with(|| generate_key(&self.base_path));
        self.write_map()?;

        let mut w = QDataStreamWriter::new();
        w.write_u64(DRAFTS_TAG_2);
        w.write_u64(serialize_peer_id(peer_id));
        w.write_u32(drafts.len() as u32);
        for d in drafts {
            let (s1, s2) = d.suggest.serialize();
            w.write_i64(d.key.serialize());
            w.write_qstring(&d.text);
            w.write_byte_array(&serialize_tags(&d.tags));
            w.write_i64(d.reply_peer_id.0 as i64);
            w.write_i64(d.reply_msg_id);
            w.write_u64(s1);
            w.write_u64(s2);
            w.write_qstring(&d.webpage.url);
            w.write_i32(i32::from(d.webpage.force_large_media));
            w.write_i32(i32::from(d.webpage.force_small_media));
            w.write_i32(i32::from(d.webpage.invert));
            w.write_i32(i32::from(d.webpage.manual));
            w.write_i32(i32::from(d.webpage.removed));
        }
        write_encrypted_file_by_key(file_key, &self.base_path, w.as_bytes(), &self.local_key)
    }

    /// Write draft cursor positions for `peer_id` in `kMultiDraftCursorsTag` format (`Account::writeDraftCursors`).
    pub fn write_draft_cursors(&mut self, peer_id: PeerId, drafts: &[StoredDraft]) -> Result<()> {
        if drafts.is_empty() {
            if let Some(key) = self.map.draft_cursors_map.remove(&peer_id) {
                clear_key(key, &self.base_path);
                self.write_map()?;
            }
            return Ok(());
        }
        let file_key = *self
            .map
            .draft_cursors_map
            .entry(peer_id)
            .or_insert_with(|| generate_key(&self.base_path));
        self.write_map()?;

        let mut w = QDataStreamWriter::new();
        w.write_u64(MULTI_DRAFT_CURSORS_TAG);
        w.write_u64(serialize_peer_id(peer_id));
        w.write_u32(drafts.len() as u32);
        for d in drafts {
            w.write_i64(d.key.serialize());
            w.write_i32(d.cursor.position);
            w.write_i32(d.cursor.anchor);
            w.write_i32(d.cursor.scroll);
        }
        write_encrypted_file_by_key(file_key, &self.base_path, w.as_bytes(), &self.local_key)
    }

    /// Read drafts and their cursors for `peer_id` (`Account::readDraftsWithCursors`).
    pub fn read_drafts_with_cursors(&self, peer_id: PeerId) -> Result<Vec<StoredDraft>> {
        let Some(&file_key) = self.map.drafts_map.get(&peer_id) else {
            return Ok(Vec::new());
        };
        let file = read_encrypted_file_by_key(file_key, &self.base_path, &self.local_key)?;
        let mut r = file.stream();
        let tag = r.read_u64()?;

        let mut drafts = if tag != RICH_DRAFTS_TAG
            && tag != DRAFTS_TAG_2
            && tag != MULTI_DRAFT_TAG
            && tag != MULTI_DRAFT_TAG_OLD
        {
            read_drafts_legacy(peer_id, tag, file.version, &mut r)?
        } else {
            let draft_peer_raw = r.read_u64()?;
            let count = r.read_u32()?;
            if count == 0 || count > 1000 || deserialize_peer_id(draft_peer_raw) != peer_id {
                return Err(StorageError::CorruptData("invalid drafts header".into()));
            }
            let keys_old = tag == MULTI_DRAFT_TAG_OLD;
            let with_suggest = tag == DRAFTS_TAG_2;
            let rich = tag == RICH_DRAFTS_TAG || with_suggest;

            let mut out = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let key = if keys_old {
                    DraftKey::from_serialized_old(r.read_i32()?)
                } else {
                    DraftKey::from_serialized(r.read_i64()?)
                };

                let text;
                let tags_bytes;
                let reply_peer_id;
                let reply_msg_id;
                let mut suggest = SuggestOptions::default();
                let mut webpage = WebPageDraft::default();

                if !rich {
                    text = r.read_qstring()?;
                    tags_bytes = r.read_byte_array()?;
                    reply_msg_id = r.read_i64()?;
                    let preview_state = r.read_i32()?;
                    if preview_state == 1 {
                        webpage.removed = true;
                    }
                    reply_peer_id = peer_id;
                } else {
                    text = r.read_qstring()?;
                    tags_bytes = r.read_byte_array()?;
                    reply_peer_id = PeerId(r.read_i64()? as u64);
                    reply_msg_id = r.read_i64()?;
                    if with_suggest {
                        let s1 = r.read_u64()?;
                        let s2 = r.read_u64()?;
                        suggest = SuggestOptions::deserialize(s1, s2);
                    }
                    webpage.url = r.read_qstring()?;
                    webpage.force_large_media = r.read_i32()? == 1;
                    webpage.force_small_media = r.read_i32()? == 1;
                    webpage.invert = r.read_i32()? == 1;
                    webpage.manual = r.read_i32()? == 1;
                    webpage.removed = r.read_i32()? == 1;
                }

                let tags = deserialize_tags(&tags_bytes, text.encode_utf16().count());
                if key.is_valid() && !key.is_cloud() {
                    out.push(StoredDraft {
                        key,
                        text,
                        tags,
                        reply_peer_id,
                        reply_msg_id,
                        suggest,
                        webpage,
                        cursor: DraftCursor::default(),
                    });
                }
            }
            out
        };

        // Overlay cursors if present
        if let Some(&cursor_key) = self.map.draft_cursors_map.get(&peer_id)
            && let Ok(cfile) =
                read_encrypted_file_by_key(cursor_key, &self.base_path, &self.local_key)
        {
            let _ = apply_draft_cursors(peer_id, &cfile.data, &mut drafts);
        }

        Ok(drafts)
    }

    /// Read downloaded file locations and aliases (`Account::readLocations`).
    pub fn read_locations(&self) -> Result<StoredLocations> {
        if self.map.locations_key == 0 {
            return Ok(StoredLocations::default());
        }
        let file =
            read_encrypted_file_by_key(self.map.locations_key, &self.base_path, &self.local_key)?;
        let mut r = file.stream();
        let mut out = StoredLocations::default();
        let mut end_mark_found = false;

        while !r.at_end() {
            let first = r.read_u64()?;
            let second = r.read_u64()?;
            let legacy_type = r.read_u32()?;
            let path = r.read_qstring()?;
            let bookmark = if file.version > 9013 {
                r.read_byte_array()?
            } else {
                Vec::new()
            };
            let modified = r.read_datetime()?;
            let size = r.read_u32()?;

            if first == 0 && second == 0 && legacy_type == 0 && path.is_empty() && size == 0 {
                end_mark_found = true;
                break;
            }
            out.locations.push(StoredFileLocation {
                media_key: (first, second),
                path,
                bookmark,
                modified,
                size,
            });
        }

        if end_mark_found && !r.at_end() {
            let cnt = r.read_u32()?;
            for _ in 0..cnt {
                let kf = r.read_u64()?;
                let ks = r.read_u64()?;
                let vf = r.read_u64()?;
                let vs = r.read_u64()?;
                out.aliases.push(((kf, ks), (vf, vs)));
            }
            if !r.at_end() {
                let web_cnt = r.read_u32()?;
                for _ in 0..web_cnt {
                    let _url = r.read_qstring()?;
                    let _key = r.read_u64()?;
                    let _size = r.read_i32()?;
                }
                if !r.at_end() {
                    out.downloads_serialized = r.read_byte_array()?;
                }
            }
        }

        Ok(out)
    }

    /// Write downloaded file locations and aliases (`Account::writeLocations`).
    pub fn write_locations(&mut self, locations: &StoredLocations) -> Result<()> {
        if locations.locations.is_empty() && locations.downloads_serialized.is_empty() {
            if self.map.locations_key != 0 {
                clear_key(self.map.locations_key, &self.base_path);
                self.map.locations_key = 0;
                self.write_map()?;
            }
            return Ok(());
        }
        if self.map.locations_key == 0 {
            self.map.locations_key = generate_key(&self.base_path);
            self.write_map()?;
        }

        let payload = serialize_locations_payload(locations);
        write_encrypted_file_by_key(
            self.map.locations_key,
            &self.base_path,
            &payload,
            &self.local_key,
        )
    }

    /// Read key-value account preferences (`Account::readPrefs`).
    pub fn read_prefs(&self) -> Result<BTreeMap<Vec<u8>, Vec<u8>>> {
        if self.map.prefs_key == 0 {
            return Ok(BTreeMap::new());
        }
        let file =
            read_encrypted_file_by_key(self.map.prefs_key, &self.base_path, &self.local_key)?;
        let mut r = file.stream();
        let count = r.read_u32()?;
        let mut map = BTreeMap::new();
        for _ in 0..count {
            let key_size = r.read_u32()? as usize;
            let val_size = r.read_u32()? as usize;
            let key = r.read_raw(key_size)?.to_vec();
            let val = r.read_raw(val_size)?.to_vec();
            map.insert(key, val);
        }
        Ok(map)
    }

    /// Write key-value account preferences (`Account::writePrefs`).
    pub fn write_prefs(&mut self, prefs: &BTreeMap<Vec<u8>, Vec<u8>>) -> Result<()> {
        if prefs.is_empty() {
            if self.map.prefs_key != 0 {
                clear_key(self.map.prefs_key, &self.base_path);
                self.map.prefs_key = 0;
                self.write_map()?;
            }
            return Ok(());
        }
        if self.map.prefs_key == 0 {
            self.map.prefs_key = generate_key(&self.base_path);
            self.write_map()?;
        }

        let payload = serialize_prefs_payload(prefs);
        write_encrypted_file_by_key(
            self.map.prefs_key,
            &self.base_path,
            &payload,
            &self.local_key,
        )
    }

    /// Read trusted peers and pay-per-message stars (`Account::readTrustedPeers`).
    pub fn read_trusted_peers(&self) -> Result<TrustedPeersData> {
        if self.map.trusted_peers_key == 0 {
            return Ok(TrustedPeersData::default());
        }
        let file = read_encrypted_file_by_key(
            self.map.trusted_peers_key,
            &self.base_path,
            &self.local_key,
        )?;
        let mut r = file.stream();
        let mut out = TrustedPeersData::default();
        let trusted_count = r.read_i32()?;
        if !(0..=100_000).contains(&trusted_count) {
            return Err(StorageError::CorruptData("bad trusted peers count".into()));
        }
        for _ in 0..trusted_count {
            let value = r.read_u64()?;
            let mask = PeerTrustFlags((value >> 56) as u8);
            let peer_id = deserialize_peer_id(value & !(0xFFu64 << 56));
            out.trusted_peers.insert(peer_id, mask);
        }
        if !r.at_end() {
            let pay_count = r.read_i32()?;
            if !(0..=100_000).contains(&pay_count) {
                return Err(StorageError::CorruptData(
                    "bad pay-per-message count".into(),
                ));
            }
            for _ in 0..pay_count {
                let peer_raw = r.read_u64()?;
                let stars = r.read_i32()?;
                out.trusted_pay_per_message
                    .insert(deserialize_peer_id(peer_raw), stars);
            }
        }
        Ok(out)
    }

    /// Write trusted peers and pay-per-message stars (`Account::writeTrustedPeers`).
    pub fn write_trusted_peers(&mut self, data: &TrustedPeersData) -> Result<()> {
        if data.trusted_peers.is_empty() && data.trusted_pay_per_message.is_empty() {
            if self.map.trusted_peers_key != 0 {
                clear_key(self.map.trusted_peers_key, &self.base_path);
                self.map.trusted_peers_key = 0;
                self.write_map()?;
            }
            return Ok(());
        }
        if self.map.trusted_peers_key == 0 {
            self.map.trusted_peers_key = generate_key(&self.base_path);
            self.write_map()?;
        }

        let mut w = QDataStreamWriter::new();
        w.write_i32(data.trusted_peers.len() as i32);
        for (&peer_id, &mask) in &data.trusted_peers {
            let value = serialize_peer_id(peer_id) | (u64::from(mask.0) << 56);
            w.write_u64(value);
        }
        w.write_i32(data.trusted_pay_per_message.len() as i32);
        for (&peer_id, &stars) in &data.trusted_pay_per_message {
            w.write_u64(serialize_peer_id(peer_id));
            w.write_i32(stars);
        }
        write_encrypted_file_by_key(
            self.map.trusted_peers_key,
            &self.base_path,
            w.as_bytes(),
            &self.local_key,
        )
    }

    /// Read media last playback positions (`Account::readMediaLastPlaybackPositions`).
    pub fn read_media_last_playback_positions(&self) -> Result<Vec<(u64, i64)>> {
        if self.map.media_last_playback_positions_key == 0 {
            return Ok(Vec::new());
        }
        let file = read_encrypted_file_by_key(
            self.map.media_last_playback_positions_key,
            &self.base_path,
            &self.local_key,
        )?;
        let mut r = file.stream();
        let count = r.read_u32()?;
        let mut out = Vec::with_capacity(count.min(4096) as usize);
        for _ in 0..count {
            out.push((r.read_u64()?, r.read_i64()?));
        }
        Ok(out)
    }

    /// Write media last playback positions (`Account::writeMediaLastPlaybackPositions`).
    pub fn write_media_last_playback_positions(&mut self, positions: &[(u64, i64)]) -> Result<()> {
        if positions.is_empty() {
            if self.map.media_last_playback_positions_key != 0 {
                clear_key(self.map.media_last_playback_positions_key, &self.base_path);
                self.map.media_last_playback_positions_key = 0;
                self.write_map()?;
            }
            return Ok(());
        }
        if self.map.media_last_playback_positions_key == 0 {
            self.map.media_last_playback_positions_key = generate_key(&self.base_path);
            self.write_map()?;
        }

        let mut w = QDataStreamWriter::with_capacity(4 + positions.len() * 16);
        w.write_u32(positions.len() as u32);
        for &(doc_id, time_ms) in positions {
            w.write_u64(doc_id);
            w.write_i64(time_ms);
        }
        write_encrypted_file_by_key(
            self.map.media_last_playback_positions_key,
            &self.base_path,
            w.as_bytes(),
            &self.local_key,
        )
    }

    /// Read recent hashtags and inline bots (`Account::readRecentHashtagsAndBots`).
    pub fn read_recent_hashtags_and_bots(&self) -> Result<RecentHashtagsAndBots> {
        if self.map.recent_hashtags_and_bots_key == 0 {
            return Ok(RecentHashtagsAndBots::default());
        }
        let file = read_encrypted_file_by_key(
            self.map.recent_hashtags_and_bots_key,
            &self.base_path,
            &self.local_key,
        )?;
        let mut r = file.stream();
        let write_count = r.read_u32()?;
        let search_count = r.read_u32()?;
        let mut out = RecentHashtagsAndBots::default();
        for _ in 0..write_count {
            let tag = r.read_qstring()?.trim().to_string();
            let cnt = r.read_u16()?;
            out.write_hashtags.push((tag, cnt));
        }
        for _ in 0..search_count {
            let tag = r.read_qstring()?.trim().to_string();
            let cnt = r.read_u16()?;
            out.search_hashtags.push((tag, cnt));
        }
        if !r.at_end() {
            let bots_count = r.read_u32()?;
            for _ in 0..bots_count {
                out.inline_bots
                    .push(SerializedPeer::read_from_stream(file.version, &mut r)?);
            }
        }
        Ok(out)
    }

    /// Write recent hashtags and inline bots (`Account::writeRecentHashtagsAndBots`).
    pub fn write_recent_hashtags_and_bots(&mut self, data: &RecentHashtagsAndBots) -> Result<()> {
        if data.write_hashtags.is_empty()
            && data.search_hashtags.is_empty()
            && data.inline_bots.is_empty()
        {
            if self.map.recent_hashtags_and_bots_key != 0 {
                clear_key(self.map.recent_hashtags_and_bots_key, &self.base_path);
                self.map.recent_hashtags_and_bots_key = 0;
                self.write_map()?;
            }
            return Ok(());
        }
        if self.map.recent_hashtags_and_bots_key == 0 {
            self.map.recent_hashtags_and_bots_key = generate_key(&self.base_path);
            self.write_map()?;
        }

        let mut w = QDataStreamWriter::new();
        w.write_u32(data.write_hashtags.len() as u32);
        w.write_u32(data.search_hashtags.len() as u32);
        for (tag, cnt) in &data.write_hashtags {
            w.write_qstring(tag);
            w.write_u16(*cnt);
        }
        for (tag, cnt) in &data.search_hashtags {
            w.write_qstring(tag);
            w.write_u16(*cnt);
        }
        w.write_u32(data.inline_bots.len() as u32);
        for bot in &data.inline_bots {
            bot.write_to_stream(&mut w);
        }
        write_encrypted_file_by_key(
            self.map.recent_hashtags_and_bots_key,
            &self.base_path,
            w.as_bytes(),
            &self.local_key,
        )
    }

    /// Read search suggestions blobs (`Account::readSearchSuggestions`).
    pub fn read_search_suggestions(&self) -> Result<SearchSuggestionsBlobs> {
        if self.map.search_suggestions_key == 0 {
            return Ok(SearchSuggestionsBlobs::default());
        }
        let file = read_encrypted_file_by_key(
            self.map.search_suggestions_key,
            &self.base_path,
            &self.local_key,
        )?;
        let mut r = file.stream();
        let top_peers = r.read_byte_array()?;
        let recent_peers = r.read_byte_array()?;
        let settings_searches = if !r.at_end() {
            r.read_byte_array()?
        } else {
            Vec::new()
        };
        let guest_chat_bots = if !r.at_end() {
            r.read_byte_array()?
        } else {
            Vec::new()
        };
        let money_recipients = if !r.at_end() {
            r.read_byte_array()?
        } else {
            Vec::new()
        };
        Ok(SearchSuggestionsBlobs {
            top_peers,
            recent_peers,
            settings_searches,
            guest_chat_bots,
            money_recipients,
        })
    }

    /// Write search suggestions blobs (`Account::writeSearchSuggestions`).
    pub fn write_search_suggestions(&mut self, data: &SearchSuggestionsBlobs) -> Result<()> {
        if data.top_peers.is_empty()
            && data.recent_peers.is_empty()
            && data.settings_searches.is_empty()
            && data.guest_chat_bots.is_empty()
            && data.money_recipients.is_empty()
        {
            if self.map.search_suggestions_key != 0 {
                clear_key(self.map.search_suggestions_key, &self.base_path);
                self.map.search_suggestions_key = 0;
                self.write_map()?;
            }
            return Ok(());
        }
        if self.map.search_suggestions_key == 0 {
            self.map.search_suggestions_key = generate_key(&self.base_path);
            self.write_map()?;
        }

        let payload = serialize_search_suggestions_payload(data);
        write_encrypted_file_by_key(
            self.map.search_suggestions_key,
            &self.base_path,
            &payload,
            &self.local_key,
        )
    }

    /// Read export settings (`Account::readExportSettings`).
    pub fn read_export_settings(&self) -> Result<StoredExportSettings> {
        if self.map.export_settings_key == 0 {
            return Ok(StoredExportSettings::default());
        }
        let file = read_encrypted_file_by_key(
            self.map.export_settings_key,
            &self.base_path,
            &self.local_key,
        )?;
        let mut r = file.stream();
        let types = r.read_u32()?;
        let full_chats = r.read_u32()?;
        let media_types = r.read_u32()?;
        let media_size_limit = r.read_u32()?;
        let format = r.read_u32()?;
        let path = r.read_qstring()?;
        let available_at = r.read_u32()?;

        let single_peer = if !r.at_end() {
            match r.read_i32()? {
                0 => ExportSinglePeer::Empty,
                4 => ExportSinglePeer::SelfPeer,
                1 => ExportSinglePeer::User {
                    user_id: r.read_i32()? as u64,
                    access_hash: r.read_u64()?,
                },
                2 => ExportSinglePeer::Chat {
                    chat_id: r.read_i32()? as u64,
                },
                3 => ExportSinglePeer::Channel {
                    channel_id: r.read_i32()? as u64,
                    access_hash: r.read_u64()?,
                },
                9 => ExportSinglePeer::User {
                    user_id: r.read_u64()?,
                    access_hash: r.read_u64()?,
                },
                10 => ExportSinglePeer::Chat {
                    chat_id: r.read_u64()?,
                },
                11 => ExportSinglePeer::Channel {
                    channel_id: r.read_u64()?,
                    access_hash: r.read_u64()?,
                },
                other => {
                    return Err(StorageError::CorruptData(format!(
                        "unknown single peer export type {other}"
                    )));
                }
            }
        } else {
            ExportSinglePeer::Empty
        };

        let (single_peer_from, single_peer_till) = if !r.at_end() {
            (r.read_i32()?, r.read_i32()?)
        } else {
            (0, 0)
        };

        Ok(StoredExportSettings {
            types,
            full_chats,
            media_types,
            media_size_limit,
            format,
            path,
            available_at,
            single_peer,
            single_peer_from,
            single_peer_till,
        })
    }

    /// Write export settings (`Account::writeExportSettings`).
    pub fn write_export_settings(&mut self, settings: &StoredExportSettings) -> Result<()> {
        if self.map.export_settings_key == 0 {
            self.map.export_settings_key = generate_key(&self.base_path);
            self.write_map()?;
        }

        let mut w = QDataStreamWriter::new();
        w.write_u32(settings.types);
        w.write_u32(settings.full_chats);
        w.write_u32(settings.media_types);
        w.write_u32(settings.media_size_limit);
        w.write_u32(settings.format);
        w.write_qstring(&settings.path);
        w.write_u32(settings.available_at);
        match settings.single_peer {
            ExportSinglePeer::Empty => w.write_i32(0),
            ExportSinglePeer::SelfPeer => w.write_i32(4),
            ExportSinglePeer::User {
                user_id,
                access_hash,
            } => {
                w.write_i32(9);
                w.write_u64(user_id);
                w.write_u64(access_hash);
            }
            ExportSinglePeer::Chat { chat_id } => {
                w.write_i32(10);
                w.write_u64(chat_id);
            }
            ExportSinglePeer::Channel {
                channel_id,
                access_hash,
            } => {
                w.write_i32(11);
                w.write_u64(channel_id);
                w.write_u64(access_hash);
            }
        }
        w.write_i32(settings.single_peer_from);
        w.write_i32(settings.single_peer_till);

        write_encrypted_file_by_key(
            self.map.export_settings_key,
            &self.base_path,
            w.as_bytes(),
            &self.local_key,
        )
    }

    /// Read a sticker/mask/custom-emoji sets file referenced by `stickers_key` (`Account::readStickerSets`).
    pub fn read_sticker_sets(&self, stickers_key: FileKey) -> Result<StoredStickerSetsFile> {
        if stickers_key == 0 {
            return Ok(StoredStickerSetsFile::default());
        }
        let file = read_encrypted_file_by_key(stickers_key, &self.base_path, &self.local_key)?;
        let mut r = file.stream();

        let version_tag = r.read_u32()?;
        let version = r.read_i32()?;
        if version_tag != STICKERS_VERSION_TAG || version < 2 {
            return Err(StorageError::UnsupportedVersion(version));
        }
        let count = r.read_i32()?;
        if !(0..=MAX_SAVED_STICKER_SETS_COUNT).contains(&count) {
            return Err(StorageError::CorruptData(format!(
                "invalid sticker sets count {count}"
            )));
        }

        let mut sets = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let id = r.read_u64()?;
            let access_hash = r.read_u64()?;
            let hash = r.read_u64()?;
            let title = r.read_qstring()?;
            let short_name = r.read_qstring()?;
            let scnt = r.read_i32()?;
            let flags = r.read_i32()?;
            let install_date = r.read_i32()?;
            let thumbnail_document_id = if version > 2 { r.read_u64()? } else { 0 };
            let mut thumbnail_type = if version > 3 { r.read_i32()? } else { 0 };
            if version < 4 && (flags & (1 << 8)) != 0 {
                thumbnail_type = 2; // Webm
            }
            let thumbnail_location = ImageLocation::read_from_stream(file.version, &mut r)?;
            // Match `Storage::Account::readStickerSets` (`storage/storage_account.cpp:2173-2175`):
            // reject valid legacy thumbnail locations because they lack `thumb_version`.
            if let DownloadLocation::Storage(ref s) = thumbnail_location.file
                && s.is_valid()
                && s.location_type == StorageFileLocationType::Legacy
            {
                return Err(StorageError::CorruptData(
                    "legacy sticker set thumbnail location without thumb_version".into(),
                ));
            }

            let mut stickers = Vec::new();
            let mut dates = Vec::new();
            let mut emoji = Vec::new();

            if scnt >= 0 {
                stickers.reserve(r.capacity_for(scnt as usize, 20));
                // Match `Storage::Account::readStickerSets` (`storage/storage_account.cpp:2235-2248`):
                // deduplicate sticker documents by ID so stale duplicates cannot corrupt list hashes.
                let mut seen_docs = BTreeSet::new();
                for _ in 0..scnt {
                    let doc = SerializedDocument::read_from_stream(file.version, &mut r)?;
                    if seen_docs.insert(doc.id) {
                        stickers.push(doc);
                    }
                }
                let dates_count = r.read_i32()?;
                if dates_count > 0 {
                    if dates_count != scnt {
                        return Err(StorageError::CorruptData(
                            "sticker dates_count mismatch".into(),
                        ));
                    }
                    dates.reserve(r.capacity_for(dates_count as usize, 4));
                    for _ in 0..dates_count {
                        dates.push(r.read_i32()?);
                    }
                }
                let emoji_count = r.read_i32()?;
                if emoji_count < 0 {
                    return Err(StorageError::CorruptData("negative emoji count".into()));
                }
                emoji.reserve(r.capacity_for(emoji_count as usize, 8));
                for _ in 0..emoji_count {
                    let emoji_str = r.read_qstring()?;
                    let ids_count = r.read_i32()?;
                    if ids_count < 0 {
                        return Err(StorageError::CorruptData(
                            "negative sticker ids count".into(),
                        ));
                    }
                    let mut ids = Vec::with_capacity(r.capacity_for(ids_count as usize, 8));
                    for _ in 0..ids_count {
                        ids.push(r.read_u64()?);
                    }
                    emoji.push((emoji_str, ids));
                }
            }

            sets.push(StoredStickerSet {
                id,
                access_hash,
                hash,
                title,
                short_name,
                count: scnt,
                flags,
                install_date,
                thumbnail_document_id,
                thumbnail_type,
                thumbnail_location,
                stickers,
                dates,
                emoji,
            });
        }

        let order = if !r.at_end() {
            let order_count = r.read_u32()?;
            if order_count > 1000 {
                return Err(StorageError::CorruptData("order count too large".into()));
            }
            let mut o = Vec::with_capacity(order_count as usize);
            for _ in 0..order_count {
                o.push(r.read_u64()?);
            }
            o
        } else {
            Vec::new()
        };

        Ok(StoredStickerSetsFile { sets, order })
    }

    /// Write a sticker/mask/custom-emoji sets file in v4 format (`Account::writeStickerSets`).
    pub fn write_sticker_sets(
        &self,
        stickers_key: FileKey,
        data: &StoredStickerSetsFile,
    ) -> Result<()> {
        let mut w = QDataStreamWriter::new();
        w.write_u32(STICKERS_VERSION_TAG);
        w.write_i32(STICKERS_SERIALIZE_VERSION);
        w.write_i32(data.sets.len() as i32);

        // Match `Storage::Account::writeStickerSet` (`storage/storage_account.cpp:1958-1960`):
        // a set flagged `NotLoaded` (`1 << 4`) encodes `-set.count` and omits stickers/dates/emoji.
        const STICKER_SET_FLAG_NOT_LOADED: i32 = 1 << 4;
        for set in &data.sets {
            let not_loaded = set.count < 0 || (set.flags & STICKER_SET_FLAG_NOT_LOADED) != 0;
            w.write_u64(set.id);
            w.write_u64(set.access_hash);
            w.write_u64(set.hash);
            w.write_qstring(&set.title);
            w.write_qstring(&set.short_name);
            w.write_i32(if not_loaded {
                if set.count < 0 { set.count } else { -set.count }
            } else {
                set.stickers.len() as i32
            });
            w.write_i32(set.flags);
            w.write_i32(set.install_date);
            w.write_u64(set.thumbnail_document_id);
            w.write_i32(set.thumbnail_type);
            set.thumbnail_location.write_to_stream(&mut w);

            if not_loaded {
                continue;
            }
            for doc in &set.stickers {
                doc.write_to_stream(&mut w);
            }
            w.write_i32(set.dates.len() as i32);
            for &d in &set.dates {
                w.write_i32(d);
            }
            w.write_i32(set.emoji.len() as i32);
            for (emoji_str, ids) in &set.emoji {
                w.write_qstring(emoji_str);
                w.write_i32(ids.len() as i32);
                for &id in ids {
                    w.write_u64(id);
                }
            }
        }

        w.write_u32(data.order.len() as u32);
        for &set_id in &data.order {
            w.write_u64(set_id);
        }

        write_encrypted_file_by_key(stickers_key, &self.base_path, w.as_bytes(), &self.local_key)
    }

    /// Read saved GIFs (`Account::readSavedGifs`).
    pub fn read_saved_gifs(&self) -> Result<Vec<SerializedDocument>> {
        if self.map.saved_gifs_key == 0 {
            return Ok(Vec::new());
        }
        let file =
            read_encrypted_file_by_key(self.map.saved_gifs_key, &self.base_path, &self.local_key)?;
        let mut r = file.stream();
        let count = r.read_u32()?;
        let mut out = Vec::with_capacity(count.min(4096) as usize);
        // Match `Storage::Account::readSavedGifs` (`storage/storage_account.cpp:2803-2818`):
        // deduplicate saved GIF document IDs so duplicates cannot skew `CountSavedGifsHash`.
        let mut seen = BTreeSet::new();
        for _ in 0..count {
            let doc = SerializedDocument::read_from_stream(file.version, &mut r)?;
            if seen.insert(doc.id) {
                out.push(doc);
            }
        }
        Ok(out)
    }

    /// Write saved GIFs (`Account::writeSavedGifs`).
    pub fn write_saved_gifs(&mut self, gifs: &[SerializedDocument]) -> Result<()> {
        if gifs.is_empty() {
            if self.map.saved_gifs_key != 0 {
                clear_key(self.map.saved_gifs_key, &self.base_path);
                self.map.saved_gifs_key = 0;
                self.write_map()?;
            }
            return Ok(());
        }
        if self.map.saved_gifs_key == 0 {
            self.map.saved_gifs_key = generate_key(&self.base_path);
            self.write_map()?;
        }

        let mut w = QDataStreamWriter::new();
        w.write_u32(gifs.len() as u32);
        for gif in gifs {
            gif.write_to_stream(&mut w);
        }
        write_encrypted_file_by_key(
            self.map.saved_gifs_key,
            &self.base_path,
            w.as_bytes(),
            &self.local_key,
        )
    }

    /// Read per-bot storage bytes (`Account::readBotStorage`).
    pub fn read_bot_storage(&self, bot_id: PeerId) -> Result<Option<Vec<u8>>> {
        let Some(&key) = self.map.bot_storages_map.get(&bot_id) else {
            return Ok(None);
        };
        let file = read_encrypted_file_by_key(key, &self.base_path, &self.local_key)?;
        let mut r = file.stream();
        Ok(Some(r.read_byte_array()?))
    }

    /// Write per-bot storage bytes (`Account::writeBotStorage`).
    pub fn write_bot_storage(&mut self, bot_id: PeerId, serialized: &[u8]) -> Result<()> {
        if serialized.is_empty() {
            if let Some(key) = self.map.bot_storages_map.remove(&bot_id) {
                clear_key(key, &self.base_path);
                self.write_map()?;
            }
            return Ok(());
        }
        let file_key = *self
            .map
            .bot_storages_map
            .entry(bot_id)
            .or_insert_with(|| generate_key(&self.base_path));
        self.write_map()?;

        let mut w = QDataStreamWriter::with_capacity(4 + serialized.len());
        w.write_byte_array(serialized);
        write_encrypted_file_by_key(file_key, &self.base_path, w.as_bytes(), &self.local_key)
    }

    /// Read a per-key encrypted wallet engine record (`Account::readWalletEngineValue`).
    pub fn read_wallet_engine_value(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let Some(&file_key) = self.map.wallet_engine_storages_map.get(key) else {
            return Ok(None);
        };
        let file = read_encrypted_file_by_key(file_key, &self.base_path, &self.local_key)?;
        let mut r = file.stream();
        let format_version = r.read_u32()?;
        if format_version > WALLET_ENGINE_FORMAT_VERSION {
            return Err(StorageError::UnsupportedVersion(format_version as i32));
        }
        Ok(Some(r.read_byte_array()?))
    }

    /// Write a per-key encrypted wallet engine record (`Account::writeWalletEngineValue`).
    ///
    /// Commits the encrypted record file first before updating `map` so a crash never leaves
    /// a dangling key pointing to a missing record.
    pub fn write_wallet_engine_value(&mut self, key: &str, bytes: &[u8]) -> Result<()> {
        let existing = self.map.wallet_engine_storages_map.get(key).copied();
        let file_key = existing.unwrap_or_else(|| generate_key(&self.base_path));

        let mut w = QDataStreamWriter::with_capacity(8 + bytes.len());
        w.write_u32(WALLET_ENGINE_FORMAT_VERSION);
        w.write_byte_array(bytes);
        write_encrypted_file_by_key(file_key, &self.base_path, w.as_bytes(), &self.local_key)?;

        if existing.is_none() {
            self.map
                .wallet_engine_storages_map
                .insert(key.to_string(), file_key);
            self.write_map()?;
        }
        Ok(())
    }

    /// Remove a per-key wallet engine record (`Account::removeWalletEngineValue`).
    pub fn remove_wallet_engine_value(&mut self, key: &str) -> Result<bool> {
        let Some(file_key) = self.map.wallet_engine_storages_map.remove(key) else {
            return Ok(false);
        };
        self.write_map()?;
        clear_key(file_key, &self.base_path);
        Ok(true)
    }

    /// Read raw JPEG bytes of the round video placeholder (`Account::readRoundPlaceholder`).
    pub fn read_round_placeholder(&self) -> Result<Option<Vec<u8>>> {
        if self.map.round_placeholder_key == 0 {
            return Ok(None);
        }
        let file = read_encrypted_file_by_key(
            self.map.round_placeholder_key,
            &self.base_path,
            &self.local_key,
        )?;
        let mut r = file.stream();
        Ok(Some(r.read_byte_array()?))
    }

    /// Write raw JPEG bytes of the round video placeholder (`Account::writeRoundPlaceholder`).
    pub fn write_round_placeholder(&mut self, jpeg_bytes: &[u8]) -> Result<()> {
        if self.map.round_placeholder_key == 0 {
            self.map.round_placeholder_key = generate_key(&self.base_path);
            self.write_map()?;
        }
        let mut w = QDataStreamWriter::with_capacity(4 + jpeg_bytes.len());
        w.write_byte_array(jpeg_bytes);
        write_encrypted_file_by_key(
            self.map.round_placeholder_key,
            &self.base_path,
            w.as_bytes(),
            &self.local_key,
        )
    }

    /// Read inline bots downloads blob (`Account::readInlineBotsDownloads`).
    pub fn read_inline_bots_downloads(&self) -> Result<Option<Vec<u8>>> {
        if self.map.inline_bots_downloads_key == 0 {
            return Ok(None);
        }
        let file = read_encrypted_file_by_key(
            self.map.inline_bots_downloads_key,
            &self.base_path,
            &self.local_key,
        )?;
        let mut r = file.stream();
        Ok(Some(r.read_byte_array()?))
    }

    /// Write inline bots downloads blob (`Account::writeInlineBotsDownloads`).
    pub fn write_inline_bots_downloads(&mut self, bytes: &[u8]) -> Result<()> {
        if self.map.inline_bots_downloads_key == 0 {
            self.map.inline_bots_downloads_key = generate_key(&self.base_path);
            self.write_map()?;
        }
        let mut w = QDataStreamWriter::with_capacity(4 + bytes.len());
        w.write_byte_array(bytes);
        write_encrypted_file_by_key(
            self.map.inline_bots_downloads_key,
            &self.base_path,
            w.as_bytes(),
            &self.local_key,
        )
    }

    /// Convenience helper for `APP_VERSION` constant.
    #[must_use]
    pub const fn current_app_version() -> i32 {
        APP_VERSION
    }
}

fn key_file_paths(key: FileKey, base_path: &Path) -> Vec<PathBuf> {
    let name = to_file_part(key);
    ['0', '1', 's']
        .into_iter()
        .map(|suffix| base_path.join(format!("{name}{suffix}")))
        .collect()
}

fn serialize_locations_payload(locations: &StoredLocations) -> Vec<u8> {
    let mut w = QDataStreamWriter::new();
    for loc in &locations.locations {
        w.write_u64(loc.media_key.0);
        w.write_u64(loc.media_key.1);
        w.write_u32(0); // legacyTypeField
        w.write_qstring(&loc.path);
        w.write_byte_array(&loc.bookmark);
        w.write_datetime(loc.modified);
        w.write_u32(loc.size);
    }

    // End marker
    w.write_u64(0);
    w.write_u64(0);
    w.write_u32(0);
    w.write_qstring("");
    w.write_byte_array(&[]);
    w.write_datetime(QtDateTime::default());
    w.write_u32(0);

    // Aliases
    w.write_u32(locations.aliases.len() as u32);
    for &((kf, ks), (vf, vs)) in &locations.aliases {
        w.write_u64(kf);
        w.write_u64(ks);
        w.write_u64(vf);
        w.write_u64(vs);
    }

    // Legacy webLocationsCount = 0 + downloadsSerialized
    w.write_u32(0);
    w.write_byte_array(&locations.downloads_serialized);
    w.into_bytes()
}

fn serialize_prefs_payload(prefs: &BTreeMap<Vec<u8>, Vec<u8>>) -> Vec<u8> {
    let mut w = QDataStreamWriter::new();
    w.write_u32(prefs.len() as u32);
    for (k, v) in prefs {
        w.write_u32(k.len() as u32);
        w.write_u32(v.len() as u32);
        w.write_raw(k);
        w.write_raw(v);
    }
    w.into_bytes()
}

fn serialize_search_suggestions_payload(data: &SearchSuggestionsBlobs) -> Vec<u8> {
    let mut w = QDataStreamWriter::new();
    w.write_byte_array(&data.top_peers);
    w.write_byte_array(&data.recent_peers);
    w.write_byte_array(&data.settings_searches);
    w.write_byte_array(&data.guest_chat_bots);
    w.write_byte_array(&data.money_recipients);
    w.into_bytes()
}

fn read_drafts_legacy(
    peer_id: PeerId,
    draft_peer_raw: u64,
    version: i32,
    r: &mut QDataStreamReader<'_>,
) -> Result<Vec<StoredDraft>> {
    let msg_text = r.read_qstring()?;
    let msg_tags_bytes = if version >= 9048 {
        r.read_byte_array()?
    } else {
        Vec::new()
    };
    let mut msg_reply_to = 0i32;
    let mut msg_preview_cancelled = 0i32;
    let mut edit_text = String::new();
    let mut edit_tags_bytes = Vec::new();
    let mut edit_msg_id = 0i32;
    let mut edit_preview_cancelled = 0i32;

    if version >= 7021 {
        msg_reply_to = r.read_i32()?;
        if version >= 8001 {
            msg_preview_cancelled = r.read_i32()?;
            if !r.at_end() {
                edit_text = r.read_qstring()?;
                if version >= 9048 {
                    edit_tags_bytes = r.read_byte_array()?;
                }
                edit_msg_id = r.read_i32()?;
                edit_preview_cancelled = r.read_i32()?;
            }
        }
    }

    if deserialize_peer_id(draft_peer_raw) != peer_id {
        return Err(StorageError::CorruptData(
            "legacy draft peer mismatch".into(),
        ));
    }

    let mut out = Vec::new();
    if !msg_text.is_empty() || msg_reply_to != 0 {
        let tags = deserialize_tags(&msg_tags_bytes, msg_text.encode_utf16().count());
        out.push(StoredDraft {
            key: DraftKey::local(0, PeerId::ZERO),
            text: msg_text,
            tags,
            reply_peer_id: peer_id,
            reply_msg_id: i64::from(msg_reply_to),
            suggest: SuggestOptions::default(),
            webpage: WebPageDraft {
                removed: msg_preview_cancelled == 1,
                ..Default::default()
            },
            cursor: DraftCursor::default(),
        });
    }
    if edit_msg_id != 0 {
        let tags = deserialize_tags(&edit_tags_bytes, edit_text.encode_utf16().count());
        out.push(StoredDraft {
            key: DraftKey::local_edit(0, PeerId::ZERO),
            text: edit_text,
            tags,
            reply_peer_id: peer_id,
            reply_msg_id: i64::from(edit_msg_id),
            suggest: SuggestOptions::default(),
            webpage: WebPageDraft {
                removed: edit_preview_cancelled == 1,
                ..Default::default()
            },
            cursor: DraftCursor::default(),
        });
    }
    Ok(out)
}

fn apply_draft_cursors(peer_id: PeerId, data: &[u8], drafts: &mut [StoredDraft]) -> Result<()> {
    let mut r = QDataStreamReader::new(data);
    let tag = r.read_u64()?;
    if tag != MULTI_DRAFT_CURSORS_TAG
        && tag != MULTI_DRAFT_CURSORS_TAG_OLD
        && tag != MULTI_DRAFT_TAG_OLD
    {
        // Legacy format: tag was serialized peer id
        let local_pos = r.read_i32()?;
        let local_anc = r.read_i32()?;
        let local_scr = r.read_i32()?;
        let edit_cursor = if !r.at_end() {
            Some(DraftCursor {
                position: r.read_i32()?,
                anchor: r.read_i32()?,
                scroll: r.read_i32()?,
            })
        } else {
            None
        };
        if deserialize_peer_id(tag) != peer_id {
            return Ok(());
        }
        for d in drafts.iter_mut() {
            if d.key == DraftKey::local(0, PeerId::ZERO) {
                d.cursor = DraftCursor {
                    position: local_pos,
                    anchor: local_anc,
                    scroll: local_scr,
                };
            } else if d.key == DraftKey::local_edit(0, PeerId::ZERO)
                && let Some(ec) = edit_cursor
            {
                d.cursor = ec;
            }
        }
        return Ok(());
    }

    let draft_peer_raw = r.read_u64()?;
    let count = r.read_u32()?;
    if count == 0 || count > 1000 || deserialize_peer_id(draft_peer_raw) != peer_id {
        return Ok(());
    }
    let keys_written = tag == MULTI_DRAFT_CURSORS_TAG;
    let keys_old = tag == MULTI_DRAFT_CURSORS_TAG_OLD;
    for _ in 0..count {
        let key = if keys_written {
            DraftKey::from_serialized(r.read_i64()?)
        } else if keys_old {
            DraftKey::from_serialized_old(r.read_i32()?)
        } else {
            DraftKey::local(0, PeerId::ZERO)
        };
        let cursor = DraftCursor {
            position: r.read_i32()?,
            anchor: r.read_i32()?,
            scroll: r.read_i32()?,
        };
        if let Some(d) = drafts.iter_mut().find(|d| d.key == key) {
            d.cursor = cursor;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::DomainStorage;
    use crate::mtp_auth::MtpAuthKeyEntry;
    use crate::serialize::document::{DocumentType, StickerSetType};
    use tempfile::TempDir;

    #[test]
    fn full_account_storage_roundtrip() {
        let dir = TempDir::new().expect("tempdir");
        let domain = DomainStorage::create_fresh(dir.path(), "data").expect("create domain");
        let mut account =
            AccountStorage::open(dir.path(), "data", 0, domain.local_key().clone()).expect("open");

        // Verify expected tdesktop directory name for account 0
        assert_eq!(account.base_path(), dir.path().join("D877F783D5D3EF8C"));

        // 1. MTP Authorization
        let auth = MtpAuthorization {
            user_id: 9_876_543_210,
            main_dc_id: 2,
            keys: vec![MtpAuthKeyEntry {
                dc_id: 2,
                key: AuthKey::new([0x22u8; 256]),
            }],
            keys_to_destroy: vec![],
        };
        account.write_mtp_data(&auth).expect("write mtp");
        assert!(dir.path().join("D877F783D5D3EF8Cs").exists());
        assert_eq!(account.read_mtp_data().expect("read mtp"), Some(auth));

        // 2. MTP Config
        account
            .write_mtp_config(b"mtp_config_payload")
            .expect("write config");
        assert_eq!(
            account.read_mtp_config().expect("read config"),
            Some(b"mtp_config_payload".to_vec())
        );

        // 3. Session Settings
        account
            .write_session_settings(
                DEFAULT_CACHE_TOTAL_SIZE_LIMIT,
                DEFAULT_CACHE_TOTAL_TIME_LIMIT,
                2 * DEFAULT_CACHE_TOTAL_SIZE_LIMIT,
                0,
                b"user_session_settings",
                &[(42, 7)],
            )
            .expect("write session settings");
        let ss = account
            .read_session_settings()
            .expect("read session settings")
            .expect("some");
        assert_eq!(ss.session_settings, b"user_session_settings");
        assert_eq!(ss.recent_stickers_preload, vec![(42, 7)]);

        // 4. Drafts + cursors
        let peer = PeerId::from_user(12345);
        let draft = StoredDraft {
            key: DraftKey::local(0, PeerId::ZERO),
            text: "Hello world".into(),
            tags: vec![TextTag {
                offset: 0,
                length: 5,
                id: "bold".into(),
            }],
            reply_peer_id: peer,
            reply_msg_id: 99,
            suggest: SuggestOptions {
                exists: true,
                price_whole: 10,
                price_nano: 500,
                ton: true,
                date: 1_700_000_000,
            },
            webpage: WebPageDraft {
                url: "https://example.org".into(),
                force_large_media: true,
                ..Default::default()
            },
            cursor: DraftCursor {
                position: 5,
                anchor: 0,
                scroll: 12,
            },
        };
        account
            .write_drafts(peer, std::slice::from_ref(&draft))
            .expect("write drafts");
        account
            .write_draft_cursors(peer, std::slice::from_ref(&draft))
            .expect("write cursors");
        let read_drafts = account.read_drafts_with_cursors(peer).expect("read drafts");
        assert_eq!(read_drafts, vec![draft]);

        // 5. Locations
        let locs = StoredLocations {
            locations: vec![StoredFileLocation {
                media_key: (1, 2),
                path: "C:/downloads/file.bin".into(),
                bookmark: vec![0x01, 0x02],
                modified: QtDateTime::from_unix_millis_utc(1_700_000_000_000),
                size: 4096,
            }],
            aliases: vec![((3, 4), (1, 2))],
            downloads_serialized: b"dl_state".to_vec(),
        };
        account.write_locations(&locs).expect("write locs");
        assert_eq!(account.read_locations().expect("read locs"), locs);

        // 6. Prefs
        let mut prefs = BTreeMap::new();
        prefs.insert(b"pref_a".to_vec(), b"val_1".to_vec());
        prefs.insert(b"pref_b".to_vec(), b"val_2".to_vec());
        account.write_prefs(&prefs).expect("write prefs");
        assert_eq!(account.read_prefs().expect("read prefs"), prefs);

        // 7. Trusted peers
        let mut tp = TrustedPeersData::default();
        tp.trusted_peers.insert(
            peer,
            PeerTrustFlags(PeerTrustFlags::PAYMENT | PeerTrustFlags::OPEN_WEB_VIEW),
        );
        tp.trusted_pay_per_message.insert(peer, 50);
        account.write_trusted_peers(&tp).expect("write trusted");
        assert_eq!(account.read_trusted_peers().expect("read trusted"), tp);

        // 8. Playback positions, bot storage, wallet engine, stickers
        account
            .write_media_last_playback_positions(&[(100, 5_000), (200, 12_345)])
            .expect("write playback");
        assert_eq!(
            account
                .read_media_last_playback_positions()
                .expect("read playback"),
            vec![(100, 5_000), (200, 12_345)]
        );

        let bot = PeerId::from_user(7777);
        account
            .write_bot_storage(bot, b"bot_kv_data")
            .expect("write bot storage");
        assert_eq!(
            account.read_bot_storage(bot).expect("read bot storage"),
            Some(b"bot_kv_data".to_vec())
        );

        account
            .write_wallet_engine_value("journal/tx1", b"pending_tx")
            .expect("write wallet");
        assert_eq!(
            account
                .read_wallet_engine_value("journal/tx1")
                .expect("read wallet"),
            Some(b"pending_tx".to_vec())
        );

        // Stickers set roundtrip
        let skey = generate_key(account.base_path());
        account.map_mut().installed_stickers_key = skey;
        account.write_map().expect("write map");
        let sets_file = StoredStickerSetsFile {
            sets: vec![StoredStickerSet {
                id: 1001,
                access_hash: 2002,
                hash: 3003,
                title: "Pack".into(),
                short_name: "pack_short".into(),
                count: 1,
                flags: 1,
                install_date: 1_700_000_000,
                thumbnail_document_id: 5001,
                thumbnail_type: 0,
                thumbnail_location: ImageLocation::default(),
                stickers: vec![SerializedDocument {
                    id: 5001,
                    access_hash: 6001,
                    date: 1_700_000_000,
                    dc_id: 2,
                    size: 1024,
                    width: 512,
                    height: 512,
                    doc_type: DocumentType::Sticker,
                    sticker_alt: "🙂".into(),
                    sticker_set_type: StickerSetType::Id,
                    duration_ms: -1,
                    ..Default::default()
                }],
                dates: vec![],
                emoji: vec![("🙂".into(), vec![5001])],
            }],
            order: vec![1001],
        };
        account
            .write_sticker_sets(skey, &sets_file)
            .expect("write stickers");
        assert_eq!(
            account.read_sticker_sets(skey).expect("read stickers"),
            sets_file
        );

        // Reopen account from disk and verify map persistence
        let reopened = AccountStorage::open(dir.path(), "data", 0, domain.local_key().clone())
            .expect("reopen");
        assert_eq!(reopened.map(), account.map());
        assert!(reopened.map().collect_good_names().contains("maps"));
    }

    /// Replaces the only occurrence of `needle` in `haystack` with `replacement` (same length).
    fn patch_once(haystack: &mut [u8], needle: &[u8], replacement: &[u8]) {
        let positions: Vec<usize> = haystack
            .windows(needle.len())
            .enumerate()
            .filter_map(|(i, w)| (w == needle).then_some(i))
            .collect();
        assert_eq!(positions.len(), 1, "needle must occur exactly once");
        haystack[positions[0]..positions[0] + needle.len()].copy_from_slice(replacement);
    }

    /// A corrupt emoji count near `i32::MAX` used to size a `Vec` directly: the allocation
    /// failed and aborted the process on startup instead of returning an error.
    #[test]
    fn huge_sticker_emoji_count_is_an_error_not_an_abort() {
        let dir = TempDir::new().expect("tempdir");
        let domain = DomainStorage::create_fresh(dir.path(), "data").expect("create domain");
        let account =
            AccountStorage::open(dir.path(), "data", 0, domain.local_key().clone()).expect("open");
        let skey = generate_key(account.base_path());
        let sets_file = StoredStickerSetsFile {
            sets: vec![StoredStickerSet {
                id: 1001,
                access_hash: 2002,
                title: "Pack".into(),
                short_name: "pack_short".into(),
                count: 0,
                thumbnail_location: ImageLocation::default(),
                emoji: vec![("x".into(), vec![0x0123_4567_89AB_CDEF])],
                ..StoredStickerSet::default()
            }],
            order: vec![1001],
        };
        account
            .write_sticker_sets(skey, &sets_file)
            .expect("write stickers");

        let file = read_encrypted_file_by_key(skey, account.base_path(), domain.local_key())
            .expect("read raw");
        let mut payload = file.data.clone();
        // Emoji count 1, then the QString "x" (byte length 2, UTF-16BE 0x0078).
        let tail = [0, 0, 0, 2, 0, 0x78];
        let mut needle = 1i32.to_be_bytes().to_vec();
        needle.extend_from_slice(&tail);
        let mut replacement = i32::MAX.to_be_bytes().to_vec();
        replacement.extend_from_slice(&tail);
        patch_once(&mut payload, &needle, &replacement);
        write_encrypted_file_by_key(skey, account.base_path(), &payload, domain.local_key())
            .expect("write patched");

        assert!(account.read_sticker_sets(skey).is_err());
    }
}
