//! Per-account `Main::SessionSettings` and `Data::AutoDownload::Full` `QDataStream`
//! serialization stored in `dbiSessionSettings` (`0x4d`).
//!
//! Port of `Telegram/SourceFiles/main/main_session_settings.cpp` and
//! `Telegram/SourceFiles/data/data_auto_download.cpp`.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Result, StorageError};
use crate::peer_id::{PeerId, deserialize_peer_id, serialize_peer_id};
use crate::qdatastream::{QDataStreamReader, QDataStreamWriter};

/// Magic version tag prefixed to `Main::SessionSettings` (`kVersionTag = -1`).
pub const SESSION_SETTINGS_VERSION_TAG: i32 = -1;
/// Current `Main::SessionSettings` format version (`kVersion = 2`).
pub const SESSION_SETTINGS_VERSION: i32 = 2;

/// Maximum auto-download byte limit (`Data::AutoDownload::kMaxBytesLimit` = 4,000 MiB).
pub const AUTO_DOWNLOAD_MAX_BYTES: i64 = 8000 * 512 * 1024;
/// Default max size for photos, voice, files, and music (`kDefaultMaxSize` = 8 MiB).
pub const AUTO_DOWNLOAD_DEFAULT_MAX_SIZE: i64 = 8 * 1024 * 1024;
/// Default max size for auto-play videos, round videos, and GIFs (`kDefaultAutoPlaySize` = 50 MiB).
pub const AUTO_DOWNLOAD_DEFAULT_AUTOPLAY_SIZE: i64 = 50 * 1024 * 1024;

const AUTO_DOWNLOAD_V1: i8 = 1;
const AUTO_DOWNLOAD_V2: i8 = 2;
const AUTO_DOWNLOAD_V3: i8 = 3;

const SOURCES_COUNT: usize = 3;
const TYPES_COUNT: usize = 7;

/// Auto-download source category (`Data::AutoDownload::Source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(usize)]
pub enum AutoDownloadSource {
    /// Private 1:1 chats (`0`).
    User = 0,
    /// Group chats & supergroups (`1`).
    Group = 1,
    /// Broadcast channels (`2`).
    Channel = 2,
}

impl AutoDownloadSource {
    /// All three sources in tdesktop wire order.
    pub const ALL: [Self; SOURCES_COUNT] = [Self::User, Self::Group, Self::Channel];
}

/// Auto-download media type (`Data::AutoDownload::Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(usize)]
pub enum AutoDownloadType {
    /// Photos (`0`).
    Photo = 0,
    /// Videos (`1`, auto-play).
    AutoPlayVideo = 1,
    /// Voice messages (`2`).
    VoiceMessage = 2,
    /// Round video messages (`3`, auto-play).
    AutoPlayVideoMessage = 3,
    /// Music / audio files (`4`).
    Music = 4,
    /// GIF animations (`5`, auto-play).
    AutoPlayGif = 5,
    /// Generic files/documents (`6`).
    File = 6,
}

impl AutoDownloadType {
    /// All 7 media types in tdesktop wire order.
    pub const ALL: [Self; TYPES_COUNT] = [
        Self::Photo,
        Self::AutoPlayVideo,
        Self::VoiceMessage,
        Self::AutoPlayVideoMessage,
        Self::Music,
        Self::AutoPlayGif,
        Self::File,
    ];

    const AUTOPLAY_TYPES: [Self; 3] = [
        Self::AutoPlayVideo,
        Self::AutoPlayVideoMessage,
        Self::AutoPlayGif,
    ];
}

/// Per-peer auto-download override (`Data::AutoDownload::Override`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i8)]
pub enum AutoDownloadOverride {
    /// Inherit global/source rules (`0`).
    #[default]
    Default = 0,
    /// Force-allow auto-download for this peer (`1`).
    ForceAllow = 1,
    /// Force-deny auto-download for this peer (`2`).
    ForceDeny = 2,
}

/// Serialized `Data::AutoDownload::Full` settings (`data/data_auto_download.cpp`).
///
/// Each entry stores `-1` (unset / inherit tdesktop default) or a non-negative byte limit
/// up to [`AUTO_DOWNLOAD_MAX_BYTES`] (`0` = disabled), matching `Single::_limit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAutoDownload {
    /// Raw limits `[source][type]`: `-1` means default, `0` means disabled, `>0` is byte cap
    /// encoded as `i32` (`u32` bit pattern, matching `Single::serialize()`).
    pub raw_limits: [[i32; TYPES_COUNT]; SOURCES_COUNT],
    /// Per-peer force allow / force deny overrides (v3 trailer).
    pub peer_overrides: BTreeMap<PeerId, AutoDownloadOverride>,
}

impl Default for StoredAutoDownload {
    fn default() -> Self {
        Self {
            raw_limits: [[-1; TYPES_COUNT]; SOURCES_COUNT],
            peer_overrides: BTreeMap::new(),
        }
    }
}

impl StoredAutoDownload {
    /// Returns tdesktop's default byte limit for `(source, kind)` (`SetDefaultsForSource`).
    #[must_use]
    pub const fn default_bytes_limit(source: AutoDownloadSource, kind: AutoDownloadType) -> i64 {
        match kind {
            AutoDownloadType::Photo | AutoDownloadType::VoiceMessage => {
                AUTO_DOWNLOAD_DEFAULT_MAX_SIZE
            }
            AutoDownloadType::AutoPlayVideo
            | AutoDownloadType::AutoPlayVideoMessage
            | AutoDownloadType::AutoPlayGif => AUTO_DOWNLOAD_DEFAULT_AUTOPLAY_SIZE,
            AutoDownloadType::File | AutoDownloadType::Music => match source {
                AutoDownloadSource::Channel => 0,
                AutoDownloadSource::User | AutoDownloadSource::Group => {
                    AUTO_DOWNLOAD_DEFAULT_MAX_SIZE
                }
            },
        }
    }

    /// Returns the effective byte limit for `(source, kind)`, resolving `-1` to defaults.
    #[must_use]
    pub const fn bytes_limit(&self, source: AutoDownloadSource, kind: AutoDownloadType) -> i64 {
        let raw = self.raw_limits[source as usize][kind as usize];
        if raw == -1 {
            Self::default_bytes_limit(source, kind)
        } else {
            (raw as u32) as i64
        }
    }

    /// Sets the byte limit for `(source, kind)` clamped to `0..=AUTO_DOWNLOAD_MAX_BYTES`.
    pub fn set_bytes_limit(
        &mut self,
        source: AutoDownloadSource,
        kind: AutoDownloadType,
        bytes: i64,
    ) {
        let clamped = bytes.clamp(0, AUTO_DOWNLOAD_MAX_BYTES) as u32;
        self.raw_limits[source as usize][kind as usize] = clamped as i32;
    }

    /// Sets or clears a per-peer auto-download override.
    pub fn set_peer_override(&mut self, peer_id: PeerId, value: AutoDownloadOverride) {
        if value == AutoDownloadOverride::Default {
            self.peer_overrides.remove(&peer_id);
        } else {
            self.peer_overrides.insert(peer_id, value);
        }
    }

    /// Serializes in tdesktop's exact `Data::AutoDownload::Full::serialize()` v3 format.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let cap = 1 + SOURCES_COUNT * TYPES_COUNT * 4 + 4 + self.peer_overrides.len() * 9;
        let mut w = QDataStreamWriter::with_capacity(cap);
        w.write_i8(AUTO_DOWNLOAD_V3);
        for src in AutoDownloadSource::ALL {
            for kind in AutoDownloadType::ALL {
                w.write_i32(self.raw_limits[src as usize][kind as usize]);
            }
        }
        w.write_i32(self.peer_overrides.len() as i32);
        for (&peer_id, &ov) in &self.peer_overrides {
            w.write_u64(serialize_peer_id(peer_id));
            w.write_i8(ov as i8);
        }
        w.into_bytes()
    }

    /// Deserializes from tdesktop's `Data::AutoDownload::Full::setFromSerialized()` (v1/v2/v3).
    pub fn deserialize(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() {
            return Ok(Self::default());
        }
        let mut r = QDataStreamReader::new(bytes);
        let version = r.read_i8()?;
        if !matches!(
            version,
            AUTO_DOWNLOAD_V1 | AUTO_DOWNLOAD_V2 | AUTO_DOWNLOAD_V3
        ) {
            return Err(StorageError::UnsupportedVersion(i32::from(version)));
        }

        let mut out = Self::default();
        for src in AutoDownloadSource::ALL {
            for kind in AutoDownloadType::ALL {
                let raw = r.read_i32()?;
                if raw != -1 && i64::from(raw as u32) > AUTO_DOWNLOAD_MAX_BYTES {
                    return Err(StorageError::CorruptData(format!(
                        "auto_download limit exceeds maximum: {}",
                        raw as u32
                    )));
                }
                out.raw_limits[src as usize][kind as usize] = raw;
            }
        }

        if version == AUTO_DOWNLOAD_V1 {
            for src in AutoDownloadSource::ALL {
                for kind in AutoDownloadType::AUTOPLAY_TYPES {
                    let upgraded = out
                        .bytes_limit(src, kind)
                        .max(AUTO_DOWNLOAD_DEFAULT_AUTOPLAY_SIZE);
                    out.set_bytes_limit(src, kind, upgraded);
                }
            }
        }

        if version >= AUTO_DOWNLOAD_V3 && !r.at_end() {
            let count = r.read_i32()?;
            if count < 0 || (count as usize) > r.remaining() / 9 {
                return Err(StorageError::CorruptData(format!(
                    "invalid auto_download peer overrides count: {count}"
                )));
            }
            for _ in 0..count {
                let peer_raw = r.read_u64()?;
                let raw_override = r.read_i8()?;
                let value = match raw_override {
                    1 => AutoDownloadOverride::ForceAllow,
                    2 => AutoDownloadOverride::ForceDeny,
                    _ => AutoDownloadOverride::Default,
                };
                if value != AutoDownloadOverride::Default {
                    out.peer_overrides
                        .insert(deserialize_peer_id(peer_raw), value);
                }
            }
        }

        Ok(out)
    }
}

/// Composite thread identifier stored in `SessionSettings` maps (`Data::ThreadId`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct StoredThreadId {
    /// Owning peer ID.
    pub peer_id: PeerId,
    /// Forum topic root message ID (`0` for non-topic chats).
    pub topic_root_id: i64,
    /// Optional monoforum sub-peer ID (`0` when unused).
    pub monoforum_peer_id: PeerId,
}

/// Unreviewed authorization/bot login entry (`Data::UnreviewedAuth`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StoredUnreviewedAuth {
    /// Authorization hash (`0` for bot entries).
    pub hash: u64,
    /// Whether the login is unconfirmed.
    pub unconfirmed: bool,
    /// Login unix timestamp.
    pub date: i32,
    /// Device model string.
    pub device: String,
    /// Location string.
    pub location: String,
    /// Bot bare user ID (`0` for regular authorizations).
    pub bot_id: u64,
    /// Bot access hash (when `bot_id != 0`).
    pub bot_access_hash: u64,
    /// Bot display name (when `bot_id != 0`).
    pub bot_name: String,
}

/// Reaction identifier stored in `_extraFavoriteReactions` (`Data::ReactionId`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StoredReactionId {
    /// Custom emoji document ID (`0` for standard Unicode emoji).
    pub custom_document_id: u64,
    /// Unicode emoji text (empty when `custom_document_id != 0`).
    pub emoji: String,
}

/// Per-account `Main::SessionSettings` serialized inside `dbiSessionSettings` (`0x4d`).
///
/// Exact `QDataStream` wire port of `Telegram/SourceFiles/main/main_session_settings.cpp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSessionSettings {
    /// Active tabbed selector tab (`0` = Emoji, `1` = Stickers, `2` = Gifs).
    pub selector_tab: i32,
    /// Peer IDs whose group stickers section is hidden.
    pub group_stickers_section_hidden: BTreeSet<PeerId>,
    /// Support chat switch mode (`0` = None, `1` = Next, `2` = Previous).
    pub support_switch: i32,
    /// Support fix chats order flag.
    pub support_fix_chats_order: bool,
    /// Support templates autocomplete flag.
    pub support_templates_autocomplete: bool,
    /// Support chats time slice.
    pub support_chats_time_slice: i32,
    /// Per-account auto-download configuration (`Data::AutoDownload::Full`).
    pub auto_download: StoredAutoDownload,
    /// Support show all search results flag.
    pub support_all_search_results: bool,
    /// Whether Archived Chats folder row is collapsed.
    pub archive_collapsed: bool,
    /// Whether Archived Chats is placed in Main Menu instead of top of dialogs list.
    pub archive_in_main_menu: bool,
    /// Whether chat folders/filters are enabled for this account.
    pub dialogs_filters_enabled: bool,
    /// Support all-silent flag.
    pub support_all_silent: bool,
    /// Number of times photo editor hint was shown.
    pub photo_editor_hint_shows_count: i32,
    /// Custom mute period presets in seconds.
    pub mute_periods: Vec<u64>,
    /// Peer IDs whose group emoji section is hidden.
    pub group_emoji_section_hidden: BTreeSet<PeerId>,
    /// Last non-premium download limit shown timestamp.
    pub last_non_premium_limit_download: i32,
    /// Last non-premium upload limit shown timestamp.
    pub last_non_premium_limit_upload: i32,
    /// Hidden pinned message IDs keyed by [`StoredThreadId`].
    pub hidden_pinned_messages: BTreeMap<StoredThreadId, i64>,
    /// Default ringtone volumes per `DefaultNotify` scope (`0` = User, `1` = Group, `2` = Broadcast).
    pub ringtone_default_volumes: BTreeMap<u8, u16>,
    /// Per-thread ringtone volumes.
    pub ringtone_volumes: BTreeMap<StoredThreadId, u16>,
    /// Rated voice transcription IDs.
    pub rated_transcriptions: BTreeSet<u64>,
    /// Unreviewed authorizations & bot logins.
    pub unreviewed: Vec<StoredUnreviewedAuth>,
    /// Login email setup state enum value.
    pub setup_email_state: i32,
    /// Filter IDs for moderate common groups.
    pub moderate_common_groups: Vec<i32>,
    /// Number of times disable-sharing box was shown.
    pub disable_sharing_box_shows_count: i32,
    /// Per-peer forum subsection tabs display modes.
    pub subsection_tabs_modes: BTreeMap<PeerId, i32>,
    /// Whether user's phone number is hidden behind a spoiler in Settings.
    pub phone_number_hidden: bool,
    /// Extra favorite reactions.
    pub extra_favorite_reactions: Vec<StoredReactionId>,
    /// Number of times stash hint was shown.
    pub stash_hint_shows_count: i32,
    /// Bot peer IDs (`PeerId.0` encoded) trusted to open Mini Apps without re-prompting
    /// (`Storage::Account::isPeerTrustedOpenWebView`).
    pub trusted_mini_app_bots: BTreeSet<i64>,
}

impl Default for StoredSessionSettings {
    fn default() -> Self {
        Self {
            selector_tab: 0,
            group_stickers_section_hidden: BTreeSet::new(),
            support_switch: 1, // Support::SwitchSettings::Next
            support_fix_chats_order: true,
            support_templates_autocomplete: true,
            support_chats_time_slice: 7 * 24 * 60 * 60,
            auto_download: StoredAutoDownload::default(),
            support_all_search_results: false,
            archive_collapsed: false,
            archive_in_main_menu: false,
            dialogs_filters_enabled: false,
            support_all_silent: false,
            photo_editor_hint_shows_count: 0,
            mute_periods: Vec::new(),
            group_emoji_section_hidden: BTreeSet::new(),
            last_non_premium_limit_download: 0,
            last_non_premium_limit_upload: 0,
            hidden_pinned_messages: BTreeMap::new(),
            ringtone_default_volumes: BTreeMap::new(),
            ringtone_volumes: BTreeMap::new(),
            rated_transcriptions: BTreeSet::new(),
            unreviewed: Vec::new(),
            setup_email_state: 0,
            moderate_common_groups: Vec::new(),
            disable_sharing_box_shows_count: 0,
            subsection_tabs_modes: BTreeMap::new(),
            phone_number_hidden: false,
            extra_favorite_reactions: Vec::new(),
            stash_hint_shows_count: 0,
            trusted_mini_app_bots: BTreeSet::new(),
        }
    }
}

impl StoredSessionSettings {
    /// Serializes in tdesktop's exact `Main::SessionSettings::serialize()` v2 `QDataStream` layout.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let auto_download_blob = self.auto_download.serialize();
        let mut w = QDataStreamWriter::with_capacity(256 + auto_download_blob.len());

        w.write_i32(SESSION_SETTINGS_VERSION_TAG);
        w.write_i32(SESSION_SETTINGS_VERSION);
        w.write_i32(self.selector_tab);

        w.write_i32(self.group_stickers_section_hidden.len() as i32);
        for &peer_id in &self.group_stickers_section_hidden {
            w.write_u64(serialize_peer_id(peer_id));
        }

        w.write_i32(self.support_switch);
        w.write_bool_i32(self.support_fix_chats_order);
        w.write_bool_i32(self.support_templates_autocomplete);
        w.write_i32(self.support_chats_time_slice);
        w.write_byte_array(&auto_download_blob);
        w.write_bool_i32(self.support_all_search_results);
        w.write_bool_i32(self.archive_collapsed);
        w.write_bool_i32(self.archive_in_main_menu);
        w.write_i32(0); // old _skipArchiveInSearch
        w.write_i32(0); // old _mediaLastPlaybackPosition size
        w.write_i32(0); // very very old _hiddenPinnedMessages size
        w.write_bool_i32(self.dialogs_filters_enabled);
        w.write_bool_i32(self.support_all_silent);
        w.write_i32(self.photo_editor_hint_shows_count);
        w.write_i32(0); // very old _hiddenPinnedMessages size

        w.write_i32(self.mute_periods.len() as i32);
        for &period in &self.mute_periods {
            w.write_u64(period);
        }

        w.write_i32(0); // old _skipPremiumStickersSet
        w.write_i32(0); // old _hiddenPinnedMessages size

        w.write_i32(self.group_emoji_section_hidden.len() as i32);
        for &peer_id in &self.group_emoji_section_hidden {
            w.write_u64(serialize_peer_id(peer_id));
        }

        w.write_i32(self.last_non_premium_limit_download);
        w.write_i32(self.last_non_premium_limit_upload);

        w.write_i32(self.hidden_pinned_messages.len() as i32);
        for (&key, &msg_id) in &self.hidden_pinned_messages {
            w.write_u64(serialize_peer_id(key.peer_id));
            w.write_i64(key.topic_root_id);
            w.write_u64(serialize_peer_id(key.monoforum_peer_id));
            w.write_i64(msg_id);
        }

        w.write_i32(0); // legacy subsection tabs modes count

        w.write_i32(self.ringtone_default_volumes.len() as i32);
        for (&scope, &vol) in &self.ringtone_default_volumes {
            w.write_u8(scope);
            w.write_u16(vol);
        }

        w.write_i32(self.ringtone_volumes.len() as i32);
        for (&key, &vol) in &self.ringtone_volumes {
            w.write_u64(serialize_peer_id(key.peer_id));
            w.write_i64(key.topic_root_id);
            w.write_u64(serialize_peer_id(key.monoforum_peer_id));
            w.write_u16(vol);
        }

        w.write_i32(self.rated_transcriptions.len() as i32);
        for &tid in &self.rated_transcriptions {
            w.write_u64(tid);
        }

        let bot_count = self.unreviewed.iter().filter(|a| a.bot_id != 0).count();
        w.write_i32((self.unreviewed.len() - bot_count) as i32);
        for auth in &self.unreviewed {
            if auth.bot_id != 0 {
                continue;
            }
            w.write_u64(auth.hash);
            w.write_bool_i32(auth.unconfirmed);
            w.write_i32(auth.date);
            w.write_qstring(&auth.device);
            w.write_qstring(&auth.location);
        }

        w.write_i32(self.setup_email_state);

        w.write_i32(self.moderate_common_groups.len() as i32);
        for &filter_id in &self.moderate_common_groups {
            w.write_i32(filter_id);
        }

        w.write_i32(self.disable_sharing_box_shows_count);

        w.write_i32(self.subsection_tabs_modes.len() as i32);
        for (&peer_id, &mode) in &self.subsection_tabs_modes {
            w.write_u64(serialize_peer_id(peer_id));
            w.write_i32(mode);
        }

        w.write_bool_i32(self.phone_number_hidden);

        w.write_i32(self.extra_favorite_reactions.len() as i32);
        for reaction in &self.extra_favorite_reactions {
            w.write_u64(reaction.custom_document_id);
            w.write_qstring(&reaction.emoji);
        }

        w.write_i32(self.stash_hint_shows_count);

        w.write_i32(bot_count as i32);
        for auth in &self.unreviewed {
            if auth.bot_id != 0 {
                w.write_u64(auth.bot_id);
                w.write_u64(auth.bot_access_hash);
                w.write_i32(auth.date);
                w.write_qstring(&auth.device);
                w.write_qstring(&auth.location);
                w.write_qstring(&auth.bot_name);
            }
        }

        w.write_i32(self.trusted_mini_app_bots.len() as i32);
        for &bot_peer_id in &self.trusted_mini_app_bots {
            w.write_i64(bot_peer_id);
        }

        w.into_bytes()
    }

    /// Deserializes from tdesktop's `Main::SessionSettings::addFromSerialized()` format
    /// (supports both modern v2 and legacy v0/v1 blobs).
    pub fn deserialize(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() {
            return Ok(Self::default());
        }
        let mut r = QDataStreamReader::new(bytes);
        let mut out = Self::default();

        let version_tag = r.read_i32()?;
        let (version, selector_tab) = if version_tag == SESSION_SETTINGS_VERSION_TAG {
            let v = r.read_i32()?;
            let tab = r.read_i32()?;
            (v, tab)
        } else {
            (0, version_tag)
        };
        if matches!(selector_tab, 0..=2) {
            out.selector_tab = selector_tab;
        }

        if version < 2 {
            let _last_seen_warning = r.read_i32()?;
            if !r.at_end() {
                let _tabbed_selector = r.read_i32()?;
            }
            if !r.at_end() {
                let count = read_checked_count(&mut r, 8)?;
                for _ in 0..count {
                    let _k = r.read_qstring()?;
                    let _v = r.read_qstring()?;
                }
            }
            if !r.at_end() {
                let _tooltip = r.read_i32()?;
            }
            if !r.at_end() {
                let _col = r.read_i32()?;
                let _corner = r.read_i32()?;
            }
        }

        if !r.at_end() {
            let count = read_checked_count(&mut r, 8)?;
            for _ in 0..count {
                out.group_stickers_section_hidden
                    .insert(deserialize_peer_id(r.read_u64()?));
            }
        }

        if version < 2 {
            if !r.at_end() {
                let _third_info = r.read_i32()?;
                let _small_dialogs = r.read_i32()?;
            }
            if !r.at_end() {
                let _ratio = r.read_i32()?;
                let _third_w = r.read_i32()?;
                let _third_ext = r.read_i32()?;
            }
            if !r.at_end() {
                let _send_files_way = r.read_i32()?;
            }
            if !r.at_end() {
                let _legacy_p2p = r.read_i32()?;
            }
        }

        if !r.at_end() {
            if version < 2 {
                let _send_submit_way = r.read_i32()?;
            }
            let sw = r.read_i32()?;
            if matches!(sw, 0..=2) {
                out.support_switch = sw;
            }
            out.support_fix_chats_order = r.read_i32()? == 1;
        }
        if !r.at_end() {
            out.support_templates_autocomplete = r.read_i32()? == 1;
        }
        if !r.at_end() {
            out.support_chats_time_slice = r.read_i32()?;
        }

        if version < 2 {
            if !r.at_end() {
                let _include_muted = r.read_i32()?;
                let _count_unread = r.read_i32()?;
            }
            if !r.at_end() {
                let _exe_warning = r.read_i32()?;
            }
        }

        let mut auto_download_bytes = Vec::new();
        if !r.at_end() {
            auto_download_bytes = r.read_byte_array()?;
        }
        if !r.at_end() {
            out.support_all_search_results = r.read_i32()? == 1;
        }
        if !r.at_end() {
            out.archive_collapsed = r.read_i32()? == 1;
        }
        if version < 2 && !r.at_end() {
            let _notify_about_pinned = r.read_i32()?;
        }
        if !r.at_end() {
            out.archive_in_main_menu = r.read_i32()? == 1;
        }
        if !r.at_end() {
            let _skip_archive_in_search = r.read_i32()?;
        }
        let mut legacy_autoplay_gifs = 1i32;
        if version < 2 {
            if !r.at_end() {
                legacy_autoplay_gifs = r.read_i32()?;
                let _loop_stickers = r.read_i32()?;
                let _large_emoji = r.read_i32()?;
                let _replace_emoji = r.read_i32()?;
                let _suggest_emoji = r.read_i32()?;
                let _suggest_stickers = r.read_i32()?;
            }
            if !r.at_end() {
                let _spellchecker = r.read_i32()?;
            }
        }
        if !r.at_end() {
            let count = read_checked_count(&mut r, 16)?;
            for _ in 0..count {
                let _doc_id = r.read_u64()?;
                let _time = r.read_i64()?;
            }
        }
        if version < 2 {
            if !r.at_end() {
                let _video_speed = r.read_i32()?;
            }
            if !r.at_end() {
                let _pip_geom = r.read_byte_array()?;
            }
            if !r.at_end() {
                let count = read_checked_count(&mut r, 8)?;
                for _ in 0..count {
                    let _lang_id = r.read_i64()?;
                }
            }
            if !r.at_end() {
                let _auto_download_dicts = r.read_i32()?;
            }
        }
        // Very very old hidden pinned messages (u64 peer, i32 msg)
        if !r.at_end() {
            let count = read_checked_count(&mut r, 12)?;
            for _ in 0..count {
                let peer_raw = r.read_u64()?;
                let msg = r.read_i32()?;
                out.hidden_pinned_messages.insert(
                    StoredThreadId {
                        peer_id: deserialize_peer_id(peer_raw),
                        topic_root_id: 0,
                        monoforum_peer_id: PeerId(0),
                    },
                    i64::from(msg),
                );
            }
        }
        if !r.at_end() {
            out.dialogs_filters_enabled = r.read_i32()? == 1;
        }
        if !r.at_end() {
            out.support_all_silent = r.read_i32()? == 1;
        }
        if !r.at_end() {
            out.photo_editor_hint_shows_count = r.read_i32()?;
        }
        // Very old hidden pinned messages (u64 peer, i64 msg)
        if !r.at_end() {
            let count = read_checked_count(&mut r, 16)?;
            for _ in 0..count {
                let peer_raw = r.read_u64()?;
                let msg = r.read_i64()?;
                out.hidden_pinned_messages.insert(
                    StoredThreadId {
                        peer_id: deserialize_peer_id(peer_raw),
                        topic_root_id: 0,
                        monoforum_peer_id: PeerId(0),
                    },
                    msg,
                );
            }
        }
        if !r.at_end() {
            let count = read_checked_count(&mut r, 8)?;
            for _ in 0..count {
                out.mute_periods.push(r.read_u64()?);
            }
        }
        if !r.at_end() {
            let _legacy_skip_premium = r.read_i32()?;
        }
        // Old hidden pinned messages (u64 peer, i64 topic, i64 msg)
        if !r.at_end() {
            let count = read_checked_count(&mut r, 24)?;
            for _ in 0..count {
                let peer_raw = r.read_u64()?;
                let topic = r.read_i64()?;
                let msg = r.read_i64()?;
                out.hidden_pinned_messages.insert(
                    StoredThreadId {
                        peer_id: deserialize_peer_id(peer_raw),
                        topic_root_id: topic,
                        monoforum_peer_id: PeerId(0),
                    },
                    msg,
                );
            }
        }
        if !r.at_end() {
            let count = read_checked_count(&mut r, 8)?;
            for _ in 0..count {
                out.group_emoji_section_hidden
                    .insert(deserialize_peer_id(r.read_u64()?));
            }
        }
        if !r.at_end() {
            out.last_non_premium_limit_download = r.read_i32()?;
            out.last_non_premium_limit_upload = r.read_i32()?;
        }
        // Modern hidden pinned messages (u64 peer, i64 topic, u64 monoforum, i64 msg)
        if !r.at_end() {
            let count = read_checked_count(&mut r, 32)?;
            for _ in 0..count {
                let peer_raw = r.read_u64()?;
                let topic = r.read_i64()?;
                let monoforum_raw = r.read_u64()?;
                let msg = r.read_i64()?;
                out.hidden_pinned_messages.insert(
                    StoredThreadId {
                        peer_id: deserialize_peer_id(peer_raw),
                        topic_root_id: topic,
                        monoforum_peer_id: deserialize_peer_id(monoforum_raw),
                    },
                    msg,
                );
            }
        }
        // Legacy subsection tabs modes (u64 peer -> mode 1)
        if !r.at_end() {
            let count = read_checked_count(&mut r, 8)?;
            for _ in 0..count {
                let peer_raw = r.read_u64()?;
                out.subsection_tabs_modes
                    .insert(deserialize_peer_id(peer_raw), 1);
            }
        }
        if !r.at_end() {
            let count = read_checked_count(&mut r, 3)?;
            for _ in 0..count {
                let scope = r.read_u8()?;
                let vol = r.read_u16()?;
                out.ringtone_default_volumes.insert(scope, vol);
            }
        }
        if !r.at_end() {
            let count = read_checked_count(&mut r, 26)?;
            for _ in 0..count {
                let peer_raw = r.read_u64()?;
                let topic = r.read_i64()?;
                let monoforum_raw = r.read_u64()?;
                let vol = r.read_u16()?;
                out.ringtone_volumes.insert(
                    StoredThreadId {
                        peer_id: deserialize_peer_id(peer_raw),
                        topic_root_id: topic,
                        monoforum_peer_id: deserialize_peer_id(monoforum_raw),
                    },
                    vol,
                );
            }
        }
        if !r.at_end() {
            let count = read_checked_count(&mut r, 8)?;
            for _ in 0..count {
                out.rated_transcriptions.insert(r.read_u64()?);
            }
        }
        if !r.at_end() {
            let count = read_checked_count(&mut r, 24)?;
            for _ in 0..count {
                let hash = r.read_u64()?;
                let unconfirmed = r.read_i32()? == 1;
                let date = r.read_i32()?;
                let device = r.read_qstring()?;
                let location = r.read_qstring()?;
                out.unreviewed.push(StoredUnreviewedAuth {
                    hash,
                    unconfirmed,
                    date,
                    device,
                    location,
                    bot_id: 0,
                    bot_access_hash: 0,
                    bot_name: String::new(),
                });
            }
        }
        if !r.at_end() {
            out.setup_email_state = r.read_i32()?;
        }
        if !r.at_end() {
            let count = read_checked_count(&mut r, 4)?;
            for _ in 0..count {
                out.moderate_common_groups.push(r.read_i32()?);
            }
        }
        if !r.at_end() {
            out.disable_sharing_box_shows_count = r.read_i32()?;
        }
        if !r.at_end() {
            let count = read_checked_count(&mut r, 12)?;
            for _ in 0..count {
                let peer_raw = r.read_u64()?;
                let mode = r.read_i32()?;
                out.subsection_tabs_modes
                    .insert(deserialize_peer_id(peer_raw), mode);
            }
        }
        if !r.at_end() {
            out.phone_number_hidden = r.read_i32()? == 1;
        }
        if !r.at_end() {
            let count = read_checked_count(&mut r, 12)?;
            for _ in 0..count {
                let custom_document_id = r.read_u64()?;
                let emoji = r.read_qstring()?;
                if custom_document_id != 0 || !emoji.is_empty() {
                    out.extra_favorite_reactions.push(StoredReactionId {
                        custom_document_id,
                        emoji,
                    });
                }
            }
        }
        if !r.at_end() {
            out.stash_hint_shows_count = r.read_i32()?;
        }
        if !r.at_end() {
            let count = read_checked_count(&mut r, 32)?;
            for _ in 0..count {
                let bot_id = r.read_u64()?;
                let bot_access_hash = r.read_u64()?;
                let date = r.read_i32()?;
                let device = r.read_qstring()?;
                let location = r.read_qstring()?;
                let bot_name = r.read_qstring()?;
                if bot_id != 0 {
                    out.unreviewed.push(StoredUnreviewedAuth {
                        hash: 0,
                        unconfirmed: true,
                        date,
                        device,
                        location,
                        bot_id,
                        bot_access_hash,
                        bot_name,
                    });
                }
            }
        }
        if !r.at_end() {
            let count = read_checked_count(&mut r, 8)?;
            for _ in 0..count {
                out.trusted_mini_app_bots.insert(r.read_i64()?);
            }
        }

        if !auto_download_bytes.is_empty() {
            out.auto_download = StoredAutoDownload::deserialize(&auto_download_bytes)?;
        }
        if version == 0 && legacy_autoplay_gifs == 0 {
            for src in AutoDownloadSource::ALL {
                for kind in AutoDownloadType::AUTOPLAY_TYPES {
                    out.auto_download.set_bytes_limit(src, kind, 0);
                }
            }
        }

        Ok(out)
    }
}

fn read_checked_count(r: &mut QDataStreamReader<'_>, min_entry_bytes: usize) -> Result<usize> {
    let raw = r.read_i32()?;
    if raw < 0 {
        return Err(StorageError::CorruptData(format!(
            "negative count in session settings: {raw}"
        )));
    }
    let count = raw as usize;
    let max_possible = r.remaining() / min_entry_bytes.max(1);
    if count > max_possible {
        return Err(StorageError::CorruptData(format!(
            "session settings count {count} exceeds remaining {} bytes",
            r.remaining()
        )));
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_download_v3_and_v1_roundtrip() {
        let mut ad = StoredAutoDownload::default();
        assert_eq!(
            ad.bytes_limit(AutoDownloadSource::User, AutoDownloadType::Photo),
            AUTO_DOWNLOAD_DEFAULT_MAX_SIZE
        );
        assert_eq!(
            ad.bytes_limit(AutoDownloadSource::Channel, AutoDownloadType::File),
            0
        );

        ad.set_bytes_limit(
            AutoDownloadSource::User,
            AutoDownloadType::Photo,
            12_000_000,
        );
        ad.set_bytes_limit(
            AutoDownloadSource::Group,
            AutoDownloadType::AutoPlayVideo,
            30_000_000,
        );
        ad.set_bytes_limit(
            AutoDownloadSource::Channel,
            AutoDownloadType::File,
            5_000_000,
        );
        ad.set_peer_override(PeerId(1001), AutoDownloadOverride::ForceAllow);
        ad.set_peer_override(PeerId(2002), AutoDownloadOverride::ForceDeny);

        let bytes = ad.serialize();
        assert_eq!(bytes[0], AUTO_DOWNLOAD_V3 as u8);
        let parsed = StoredAutoDownload::deserialize(&bytes).expect("deserialize v3");
        assert_eq!(parsed, ad);

        // V1 compatibility upgrades autoplay limits to at least kDefaultAutoPlaySize.
        let mut v1 = QDataStreamWriter::new();
        v1.write_i8(AUTO_DOWNLOAD_V1);
        for _ in 0..(SOURCES_COUNT * TYPES_COUNT) {
            v1.write_i32(1024);
        }
        let parsed_v1 = StoredAutoDownload::deserialize(v1.as_bytes()).expect("deserialize v1");
        assert_eq!(
            parsed_v1.bytes_limit(AutoDownloadSource::User, AutoDownloadType::Photo),
            1024
        );
        assert_eq!(
            parsed_v1.bytes_limit(AutoDownloadSource::User, AutoDownloadType::AutoPlayVideo),
            AUTO_DOWNLOAD_DEFAULT_AUTOPLAY_SIZE
        );
    }

    #[test]
    fn session_settings_v2_full_roundtrip() {
        let mut s = StoredSessionSettings {
            selector_tab: 2,
            support_switch: 2,
            support_fix_chats_order: false,
            support_templates_autocomplete: false,
            support_chats_time_slice: 3600,
            support_all_search_results: true,
            archive_collapsed: true,
            archive_in_main_menu: true,
            dialogs_filters_enabled: true,
            support_all_silent: true,
            photo_editor_hint_shows_count: 3,
            mute_periods: vec![3600, 28_800],
            last_non_premium_limit_download: 1_700_000_001,
            last_non_premium_limit_upload: 1_700_000_002,
            setup_email_state: 2,
            moderate_common_groups: vec![2, 5],
            disable_sharing_box_shows_count: 1,
            phone_number_hidden: true,
            stash_hint_shows_count: 4,
            ..StoredSessionSettings::default()
        };
        s.group_stickers_section_hidden
            .insert(PeerId::from_chat(42));
        s.group_emoji_section_hidden
            .insert(PeerId::from_channel(99));
        s.auto_download.set_bytes_limit(
            AutoDownloadSource::User,
            AutoDownloadType::File,
            16 * 1024 * 1024,
        );
        s.auto_download
            .set_peer_override(PeerId::from_user(555), AutoDownloadOverride::ForceDeny);
        s.hidden_pinned_messages.insert(
            StoredThreadId {
                peer_id: PeerId::from_channel(100),
                topic_root_id: 7,
                monoforum_peer_id: PeerId(0),
            },
            999,
        );
        s.ringtone_default_volumes.insert(0, 80);
        s.ringtone_volumes.insert(
            StoredThreadId {
                peer_id: PeerId::from_user(123),
                topic_root_id: 0,
                monoforum_peer_id: PeerId(0),
            },
            65,
        );
        s.rated_transcriptions.insert(7777);
        s.subsection_tabs_modes.insert(PeerId::from_channel(200), 2);
        s.extra_favorite_reactions.push(StoredReactionId {
            custom_document_id: 0,
            emoji: "🔥".to_owned(),
        });
        s.extra_favorite_reactions.push(StoredReactionId {
            custom_document_id: 88_888,
            emoji: String::new(),
        });
        s.unreviewed.push(StoredUnreviewedAuth {
            hash: 0xDEAD_BEEF,
            unconfirmed: true,
            date: 1_750_000_000,
            device: "Workstation".to_owned(),
            location: "Zurich".to_owned(),
            bot_id: 0,
            bot_access_hash: 0,
            bot_name: String::new(),
        });
        s.unreviewed.push(StoredUnreviewedAuth {
            hash: 0,
            unconfirmed: true,
            date: 1_750_000_050,
            device: "Web".to_owned(),
            location: "Berlin".to_owned(),
            bot_id: 424_242,
            bot_access_hash: 999_111,
            bot_name: "HelperBot".to_owned(),
        });
        s.trusted_mini_app_bots
            .insert(PeerId::from_user(700_001).0 as i64);
        s.trusted_mini_app_bots
            .insert(PeerId::from_user(700_002).0 as i64);

        let bytes = s.serialize();
        let parsed = StoredSessionSettings::deserialize(&bytes).expect("deserialize v2");
        assert_eq!(parsed, s);
    }

    #[test]
    fn account_storage_parsed_session_settings_tdata_roundtrip() {
        use crate::account::AccountStorage;
        use crate::domain::DomainStorage;
        use tempfile::TempDir;

        let dir = TempDir::new().expect("tempdir");
        let domain = DomainStorage::create_fresh(dir.path(), "data").expect("create domain");
        let mut acc = AccountStorage::open(dir.path(), "data", 0, domain.local_key().clone())
            .expect("open account");

        let mut settings = StoredSessionSettings {
            archive_collapsed: true,
            archive_in_main_menu: true,
            dialogs_filters_enabled: true,
            phone_number_hidden: true,
            ..StoredSessionSettings::default()
        };
        settings
            .trusted_mini_app_bots
            .insert(PeerId::from_user(88_123).0 as i64);
        settings.auto_download.set_bytes_limit(
            AutoDownloadSource::User,
            AutoDownloadType::Photo,
            20 * 1024 * 1024,
        );
        settings.auto_download.set_bytes_limit(
            AutoDownloadSource::Group,
            AutoDownloadType::File,
            4 * 1024 * 1024,
        );

        acc.write_parsed_session_settings(&settings)
            .expect("write parsed session settings");

        let reopened = AccountStorage::open(dir.path(), "data", 0, domain.local_key().clone())
            .expect("reopen account");
        let (loaded, _ctx) = reopened
            .read_parsed_session_settings()
            .expect("read parsed session settings")
            .expect("session settings present");
        assert_eq!(loaded, settings);
    }

    #[test]
    fn session_settings_legacy_blob_without_trusted_bots_defaults_empty() {
        let s = StoredSessionSettings::default();
        let full = s.serialize();
        // Truncate the trailing 4-byte `trusted_mini_app_bots` count (0) to simulate a pre-WP-74 blob.
        let legacy = &full[..full.len() - 4];
        let parsed = StoredSessionSettings::deserialize(legacy).expect("deserialize legacy");
        assert!(parsed.trusted_mini_app_bots.is_empty());
    }
}
