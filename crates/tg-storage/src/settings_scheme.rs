//! Setting block IDs (`dbi*`) and stream reader/writer for tdesktop settings blocks.
//!
//! Port of `Telegram/SourceFiles/storage/details/storage_settings_scheme.h` and `.cpp`.

use tg_crypto::{AUTH_KEY_SIZE, AuthKey};

use crate::error::{Result, StorageError};
use crate::mtp_auth::MtpAuthKeyEntry;
use crate::peer_id::{PeerId, deserialize_peer_id};
use crate::qdatastream::{QDataStreamReader, QDataStreamWriter};
use crate::tdf::FileKey;

/// Default maximum cache entry size (`kDataSizeLimit - 1`, ~16 MiB).
pub const CACHE_MAX_DATA_SIZE: i64 = (1 << 24) - 1;

/// Default cache total size limit (1 GiB).
pub const DEFAULT_CACHE_TOTAL_SIZE_LIMIT: i64 = 1024 * 1024 * 1024;

/// Default cache total time limit (31 days in seconds).
pub const DEFAULT_CACHE_TOTAL_TIME_LIMIT: i32 = 31 * 24 * 60 * 60;

/// Setting block identifiers (`dbi*` enum in `storage_settings_scheme.h`).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DbiBlockId {
    /// Legacy single DC auth key (`0x00`).
    Key = 0x00,
    /// Legacy user ID + main DC ID (`0x01`).
    User = 0x01,
    /// Very old single DC option (`0x02`).
    DcOptionOldOld = 0x02,
    /// Legacy chat size max (`0x03`).
    ChatSizeMaxOld = 0x03,
    /// Legacy muted single peer (`0x04`).
    MutePeerOld = 0x04,
    /// Legacy send key (`0x05`).
    SendKeyOld = 0x05,
    /// Auto-start with OS (`0x06`).
    AutoStart = 0x06,
    /// Start minimized (`0x07`).
    StartMinimized = 0x07,
    /// Legacy sound/flash/bounce notify bitmask (`0x08`).
    SoundFlashBounceNotifyOld = 0x08,
    /// Legacy tray/window work mode (`0x09`).
    WorkModeOld = 0x09,
    /// Seen tray tooltip (`0x0a`).
    SeenTrayTooltip = 0x0a,
    /// Legacy desktop notify flag (`0x0b`).
    DesktopNotifyOld = 0x0b,
    /// Auto-update enabled (`0x0c`).
    AutoUpdate = 0x0c,
    /// Last update check timestamp (`0x0d`).
    LastUpdateCheck = 0x0d,
    /// Legacy window position (`0x0e`).
    WindowPositionOld = 0x0e,
    /// Very old connection/proxy type (`0x0f`).
    ConnectionTypeOldOld = 0x0f,
    /// Default attach (`0x11`).
    DefaultAttach = 0x11,
    /// Legacy cats and dogs (`0x12`).
    CatsAndDogsOld = 0x12,
    /// Legacy replace emoji (`0x13`).
    ReplaceEmojiOld = 0x13,
    /// Legacy ask download path (`0x14`).
    AskDownloadPathOld = 0x14,
    /// Very old download path (`0x15`).
    DownloadPathOldOld = 0x15,
    /// Legacy UI scale enum (`0x16`).
    ScaleOld = 0x16,
    /// Legacy emoji tab (`0x17`).
    EmojiTabOld = 0x17,
    /// Oldest recent emoji (`0x18`).
    RecentEmojiOldOldOld = 0x18,
    /// Legacy logged phone number (`0x19`).
    LoggedPhoneNumberOld = 0x19,
    /// Legacy muted peers list (`0x1a`).
    MutedPeersOld = 0x1a,
    /// Legacy notify view (`0x1c`).
    NotifyViewOld = 0x1c,
    /// Windows "Send To" menu integration (`0x1d`).
    SendToMenu = 0x1d,
    /// Legacy compress pasted image (`0x1e`).
    CompressPastedImageOld = 0x1e,
    /// Legacy language ID (`0x1f`).
    LangOld = 0x1f,
    /// Legacy language file (`0x20`).
    LangFileOld = 0x20,
    /// Legacy single tile background (`0x21`).
    TileBackgroundOld = 0x21,
    /// Legacy auto-lock timeout (`0x22`).
    AutoLockOld = 0x22,
    /// Last file dialog path (`0x23`).
    DialogLastPath = 0x23,
    /// Older recent emoji (`0x24`).
    RecentEmojiOldOld = 0x24,
    /// Older emoji variants (`0x25`).
    EmojiVariantsOldOld = 0x25,
    /// Recent stickers preload (`0x26`).
    RecentStickers = 0x26,
    /// Old single DC option with flags (`0x27`).
    DcOptionOld = 0x27,
    /// Legacy try IPv6 (`0x28`).
    TryIPv6Old = 0x28,
    /// Legacy song volume (`0x29`).
    SongVolumeOld = 0x29,
    /// Legacy Windows notifications (`0x30`).
    WindowsNotificationsOld = 0x30,
    /// Legacy include muted counter (`0x31`).
    IncludeMutedOld = 0x31,
    /// Legacy megagroup size max (`0x32`).
    MegagroupSizeMaxOld = 0x32,
    /// Legacy download path + bookmark (`0x33`).
    DownloadPathOld = 0x33,
    /// Legacy auto-download flags (`0x34`).
    AutoDownloadOld = 0x34,
    /// Legacy saved GIFs limit (`0x35`).
    SavedGifsLimitOld = 0x35,
    /// Legacy showing saved GIFs (`0x36`).
    ShowingSavedGifsOld = 0x36,
    /// Legacy auto-play GIF (`0x37`).
    AutoPlayOld = 0x37,
    /// Legacy adaptive for wide (`0x38`).
    AdaptiveForWideOld = 0x38,
    /// Legacy hidden pinned messages map (`0x39`).
    HiddenPinnedMessagesOld = 0x39,
    /// Legacy recent emoji (`0x3a`).
    RecentEmojiOld = 0x3a,
    /// Legacy emoji variants (`0x3b`).
    EmojiVariantsOld = 0x3b,
    /// Legacy dialogs mode (`0x40`).
    DialogsModeOld = 0x40,
    /// Legacy moderate mode (`0x41`).
    ModerateModeOld = 0x41,
    /// Legacy video volume (`0x42`).
    VideoVolumeOld = 0x42,
    /// Legacy stickers recent limit (`0x43`).
    StickersRecentLimitOld = 0x43,
    /// Legacy native notifications (`0x44`).
    NativeNotificationsOld = 0x44,
    /// Legacy notifications count (`0x45`).
    NotificationsCountOld = 0x45,
    /// Legacy notifications corner (`0x46`).
    NotificationsCornerOld = 0x46,
    /// Legacy single theme key (`0x47`).
    ThemeKeyOld = 0x47,
    /// Legacy dialogs width ratio (`0x48`).
    DialogsWidthRatioOld = 0x48,
    /// Legacy use external video player (`0x49`).
    UseExternalVideoPlayerOld = 0x49,
    /// Legacy serialized DC options (`0x4a`).
    DcOptionsOld = 0x4a,
    /// Serialized MTProto authorization (`0x4b`).
    MtpAuthorization = 0x4b,
    /// Legacy last seen warning seen (`0x4c`).
    LastSeenWarningSeenOld = 0x4c,
    /// Per-session serialized settings (`0x4d`).
    SessionSettings = 0x4d,
    /// Global langpack file key (`0x4e`).
    LangPackKey = 0x4e,
    /// Legacy connection/proxy list (`0x4f`).
    ConnectionTypeOld = 0x4f,
    /// Legacy stickers faved limit (`0x50`).
    StickersFavedLimitOld = 0x50,
    /// Legacy suggest stickers by emoji (`0x51`).
    SuggestStickersByEmojiOld = 0x51,
    /// Legacy suggest emoji (`0x52`).
    SuggestEmojiOld = 0x52,
    /// Very old TXT domain string (`0x53`).
    TxtDomainStringOldOld = 0x53,
    /// Day/night theme keys + night mode flag (`0x54`).
    ThemeKey = 0x54,
    /// Day/night tile background flags (`0x55`).
    TileBackground = 0x55,
    /// Legacy single cache settings (`0x56`).
    CacheSettingsOld = 0x56,
    /// Power saving flags (`0x57`).
    PowerSaving = 0x57,
    /// UI scale percentage (`0x58`).
    ScalePercent = 0x58,
    /// Legacy playback speed (`0x59`).
    PlaybackSpeedOld = 0x59,
    /// Recent languages file key (`0x5a`).
    LanguagesKey = 0x5a,
    /// Legacy call settings blob (`0x5b`).
    CallSettingsOld = 0x5b,
    /// Normal + big-file cache limits (`0x5c`).
    CacheSettings = 0x5c,
    /// Legacy TXT domain string (`0x5d`).
    TxtDomainStringOld = 0x5d,
    /// Global `Core::Settings` serialized blob (`0x5e`).
    ApplicationSettings = 0x5e,
    /// Legacy dialogs filters enabled (`0x5f`).
    DialogsFiltersOld = 0x5f,
    /// Serialized fallback production `MTP::Config` (`0x60`).
    FallbackProductionConfig = 0x60,
    /// Day/night global background keys (`0x61`).
    BackgroundKey = 0x61,

    /// Legacy encrypted block with embedded salt (`333`).
    EncryptedWithSalt = 333,
    /// Legacy encrypted block reusing previous salt (`444`).
    Encrypted = 444,
    /// Legacy inline version header (`666`).
    Version = 666,
}

impl DbiBlockId {
    /// Convert a raw `u32` tag into a `DbiBlockId`.
    #[must_use]
    pub const fn from_u32(v: u32) -> Option<Self> {
        match v {
            0x00 => Some(Self::Key),
            0x01 => Some(Self::User),
            0x02 => Some(Self::DcOptionOldOld),
            0x03 => Some(Self::ChatSizeMaxOld),
            0x04 => Some(Self::MutePeerOld),
            0x05 => Some(Self::SendKeyOld),
            0x06 => Some(Self::AutoStart),
            0x07 => Some(Self::StartMinimized),
            0x08 => Some(Self::SoundFlashBounceNotifyOld),
            0x09 => Some(Self::WorkModeOld),
            0x0a => Some(Self::SeenTrayTooltip),
            0x0b => Some(Self::DesktopNotifyOld),
            0x0c => Some(Self::AutoUpdate),
            0x0d => Some(Self::LastUpdateCheck),
            0x0e => Some(Self::WindowPositionOld),
            0x0f => Some(Self::ConnectionTypeOldOld),
            0x11 => Some(Self::DefaultAttach),
            0x12 => Some(Self::CatsAndDogsOld),
            0x13 => Some(Self::ReplaceEmojiOld),
            0x14 => Some(Self::AskDownloadPathOld),
            0x15 => Some(Self::DownloadPathOldOld),
            0x16 => Some(Self::ScaleOld),
            0x17 => Some(Self::EmojiTabOld),
            0x18 => Some(Self::RecentEmojiOldOldOld),
            0x19 => Some(Self::LoggedPhoneNumberOld),
            0x1a => Some(Self::MutedPeersOld),
            0x1c => Some(Self::NotifyViewOld),
            0x1d => Some(Self::SendToMenu),
            0x1e => Some(Self::CompressPastedImageOld),
            0x1f => Some(Self::LangOld),
            0x20 => Some(Self::LangFileOld),
            0x21 => Some(Self::TileBackgroundOld),
            0x22 => Some(Self::AutoLockOld),
            0x23 => Some(Self::DialogLastPath),
            0x24 => Some(Self::RecentEmojiOldOld),
            0x25 => Some(Self::EmojiVariantsOldOld),
            0x26 => Some(Self::RecentStickers),
            0x27 => Some(Self::DcOptionOld),
            0x28 => Some(Self::TryIPv6Old),
            0x29 => Some(Self::SongVolumeOld),
            0x30 => Some(Self::WindowsNotificationsOld),
            0x31 => Some(Self::IncludeMutedOld),
            0x32 => Some(Self::MegagroupSizeMaxOld),
            0x33 => Some(Self::DownloadPathOld),
            0x34 => Some(Self::AutoDownloadOld),
            0x35 => Some(Self::SavedGifsLimitOld),
            0x36 => Some(Self::ShowingSavedGifsOld),
            0x37 => Some(Self::AutoPlayOld),
            0x38 => Some(Self::AdaptiveForWideOld),
            0x39 => Some(Self::HiddenPinnedMessagesOld),
            0x3a => Some(Self::RecentEmojiOld),
            0x3b => Some(Self::EmojiVariantsOld),
            0x40 => Some(Self::DialogsModeOld),
            0x41 => Some(Self::ModerateModeOld),
            0x42 => Some(Self::VideoVolumeOld),
            0x43 => Some(Self::StickersRecentLimitOld),
            0x44 => Some(Self::NativeNotificationsOld),
            0x45 => Some(Self::NotificationsCountOld),
            0x46 => Some(Self::NotificationsCornerOld),
            0x47 => Some(Self::ThemeKeyOld),
            0x48 => Some(Self::DialogsWidthRatioOld),
            0x49 => Some(Self::UseExternalVideoPlayerOld),
            0x4a => Some(Self::DcOptionsOld),
            0x4b => Some(Self::MtpAuthorization),
            0x4c => Some(Self::LastSeenWarningSeenOld),
            0x4d => Some(Self::SessionSettings),
            0x4e => Some(Self::LangPackKey),
            0x4f => Some(Self::ConnectionTypeOld),
            0x50 => Some(Self::StickersFavedLimitOld),
            0x51 => Some(Self::SuggestStickersByEmojiOld),
            0x52 => Some(Self::SuggestEmojiOld),
            0x53 => Some(Self::TxtDomainStringOldOld),
            0x54 => Some(Self::ThemeKey),
            0x55 => Some(Self::TileBackground),
            0x56 => Some(Self::CacheSettingsOld),
            0x57 => Some(Self::PowerSaving),
            0x58 => Some(Self::ScalePercent),
            0x59 => Some(Self::PlaybackSpeedOld),
            0x5a => Some(Self::LanguagesKey),
            0x5b => Some(Self::CallSettingsOld),
            0x5c => Some(Self::CacheSettings),
            0x5d => Some(Self::TxtDomainStringOld),
            0x5e => Some(Self::ApplicationSettings),
            0x5f => Some(Self::DialogsFiltersOld),
            0x60 => Some(Self::FallbackProductionConfig),
            0x61 => Some(Self::BackgroundKey),
            333 => Some(Self::EncryptedWithSalt),
            444 => Some(Self::Encrypted),
            666 => Some(Self::Version),
            _ => None,
        }
    }
}

/// Legacy single DC option parsed from `dbiDcOptionOldOld` or `dbiDcOptionOld`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyDcOption {
    /// DC ID (with optional shift flags).
    pub dc_id: u32,
    /// Raw DC option flags.
    pub flags: i32,
    /// IP address string.
    pub ip: String,
    /// Port number.
    pub port: u32,
}

/// Legacy proxy record parsed from `dbiConnectionTypeOldOld` or `dbiConnectionTypeOld`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LegacyProxyData {
    /// Raw proxy type code (`0`=None, `1`=Socks5, `2`=Http, `3`=Mtproto, `4`=Web).
    pub proxy_type: i32,
    /// Proxy hostname or IP.
    pub host: String,
    /// Proxy port.
    pub port: u32,
    /// Proxy username or Web base path.
    pub user: String,
    /// Proxy password or MTProto secret.
    pub password: String,
}

impl LegacyProxyData {
    /// Returns `true` if this legacy proxy record has a non-empty type, host, and port.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.proxy_type != 0 && !self.host.is_empty() && self.port > 0
    }
}

/// Aggregated legacy proxy state read from `dbiConnectionTypeOldOld`, `dbiConnectionTypeOld`,
/// and `dbiTryIPv6Old` (`storage/details/storage_settings_scheme.cpp`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LegacyProxySettings {
    /// Serialized `MTP::ProxyData::Settings` (`0` = System, `1` = Enabled, `2` = Disabled).
    pub mode: i32,
    /// Selected proxy entry, if any.
    pub selected: Option<LegacyProxyData>,
    /// Saved proxy list.
    pub list: Vec<LegacyProxyData>,
    /// Whether proxy is used for calls.
    pub use_proxy_for_calls: bool,
    /// Legacy `dbiTryIPv6Old` value, if encountered.
    pub try_ipv6: Option<bool>,
}

/// Legacy window geometry from `dbiWindowPositionOld`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LegacyWindowPosition {
    /// X coordinate.
    pub x: i32,
    /// Y coordinate.
    pub y: i32,
    /// Width.
    pub w: i32,
    /// Height.
    pub h: i32,
    /// Monitor CRC.
    pub moncrc: i32,
    /// Maximized state flag.
    pub maximized: i32,
}

/// Legacy call audio settings from `dbiCallSettingsOld`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LegacyCallSettings {
    /// Output device ID.
    pub output_device_id: String,
    /// Output volume.
    pub output_volume: i32,
    /// Input device ID.
    pub input_device_id: String,
    /// Input volume.
    pub input_volume: i32,
    /// Audio ducking enabled flag.
    pub ducking_enabled: i32,
}

/// Accumulated state produced by reading a sequence of `dbi*` blocks (`ReadSettingsContext`).
#[derive(Debug, Clone, Default)]
pub struct ReadSettingsContext {
    /// Input flag used when interpreting `dbiTileBackgroundOld` for version < 8005.
    pub legacy_has_custom_day_background: bool,

    /// Legacy single DC options.
    pub legacy_dc_options: Vec<LegacyDcOption>,
    /// Legacy serialized DC options blob (`dbiDcOptionsOld`).
    pub legacy_dc_options_serialized: Vec<u8>,
    /// Legacy chat size max.
    pub legacy_chat_size_max: i32,
    /// Legacy saved GIFs limit.
    pub legacy_saved_gifs_limit: i32,
    /// Legacy stickers recent limit.
    pub legacy_stickers_recent_limit: i32,
    /// Legacy stickers faved limit.
    pub legacy_stickers_faved_limit: i32,
    /// Legacy megagroup size max.
    pub legacy_megagroup_size_max: i32,
    /// Legacy TXT domain string.
    pub legacy_txt_domain_string: String,
    /// Serialized fallback production config (`dbiFallbackProductionConfig`).
    pub fallback_config: Vec<u8>,

    /// Serialized global `Core::Settings` (`dbiApplicationSettings`).
    pub application_settings: Vec<u8>,
    /// Serialized per-account `Main::SessionSettings` (`dbiSessionSettings`).
    pub session_settings: Vec<u8>,

    /// Normal cache size limit (bytes).
    pub cache_total_size_limit: i64,
    /// Normal cache time limit (seconds; `0` = no limit).
    pub cache_total_time_limit: i32,
    /// Big-file cache size limit (bytes).
    pub cache_big_file_total_size_limit: i64,
    /// Big-file cache time limit (seconds; `0` = no limit).
    pub cache_big_file_total_time_limit: i32,

    /// Auto-start with OS (`dbiAutoStart`).
    pub auto_start: Option<bool>,
    /// Start minimized (`dbiStartMinimized`).
    pub start_minimized: Option<bool>,
    /// Send-to menu (`dbiSendToMenu`).
    pub send_to_menu: Option<bool>,
    /// Seen tray tooltip (`dbiSeenTrayTooltip`).
    pub seen_tray_tooltip: Option<bool>,
    /// Auto-update enabled (`dbiAutoUpdate`).
    pub auto_update: Option<bool>,
    /// Last update check unix time (`dbiLastUpdateCheck`).
    pub last_update_check: Option<i32>,
    /// Scale percentage (`dbiScalePercent` / `dbiScaleOld`).
    pub scale_percent: Option<i32>,
    /// Last file dialog path (`dbiDialogLastPath`).
    pub dialog_last_path: Option<String>,
    /// Power saving bitflags (`dbiPowerSaving`).
    pub power_saving: Option<i32>,

    /// Legacy theme key (`dbiThemeKeyOld`).
    pub theme_key_legacy: FileKey,
    /// Day theme file key (`dbiThemeKey`).
    pub theme_key_day: FileKey,
    /// Night theme file key (`dbiThemeKey`).
    pub theme_key_night: FileKey,
    /// Night mode active flag (`dbiThemeKey`).
    pub night_mode: bool,

    /// Global day background key (`dbiBackgroundKey`).
    pub background_key_day: FileKey,
    /// Global night background key (`dbiBackgroundKey`).
    pub background_key_night: FileKey,
    /// Whether `dbiBackgroundKey` was read.
    pub background_keys_read: bool,
    /// Day background tiling flag (`dbiTileBackground`).
    pub tile_day: bool,
    /// Night background tiling flag (`dbiTileBackground`).
    pub tile_night: bool,
    /// Whether tile flags were read.
    pub tile_read: bool,

    /// Langpack file key (`dbiLangPackKey`).
    pub lang_pack_key: FileKey,
    /// Recent languages file key (`dbiLanguagesKey`).
    pub languages_key: FileKey,

    /// Recent stickers preload `(document_id, count)` (`dbiRecentStickers`).
    pub recent_stickers_preload: Vec<(u64, u16)>,

    /// Modern serialized MTProto authorization blob (`dbiMtpAuthorization`).
    pub mtp_authorization: Vec<u8>,
    /// Legacy individual DC keys (`dbiKey`).
    pub mtp_legacy_keys: Vec<MtpAuthKeyEntry>,
    /// Legacy main DC ID (`dbiUser`).
    pub mtp_legacy_main_dc_id: i32,
    /// Legacy 32-bit user ID (`dbiUser`).
    pub mtp_legacy_user_id: i32,

    /// Legacy window position (`dbiWindowPositionOld`).
    pub legacy_window_position: Option<LegacyWindowPosition>,
    /// Legacy proxies (`dbiConnectionTypeOld` / `dbiConnectionTypeOldOld`).
    pub legacy_proxies: Vec<LegacyProxyData>,
    /// Full legacy proxy settings state (`dbiConnectionTypeOld*` + `dbiTryIPv6Old`).
    pub legacy_proxy: Option<LegacyProxySettings>,
    /// Legacy call settings (`dbiCallSettingsOld`).
    pub legacy_call_settings: Option<LegacyCallSettings>,
    /// Legacy hidden pinned messages `(peer_id, msg_id)` (`dbiHiddenPinnedMessagesOld`).
    pub legacy_hidden_pinned_messages: Vec<(PeerId, i32)>,

    /// Set to `true` if any deprecated/legacy block ID was encountered.
    pub legacy_read: bool,
}

fn no_time_limit(stored: i32) -> bool {
    // Workaround for tdesktop issue #5611:
    // !stored || stored == i32::MAX || stored == (i64::MAX as i32 == -1)
    stored == 0 || stored == i32::MAX || stored == -1
}

/// Read a single `dbi*` setting block from `stream` into `context` (`Storage::details::ReadSetting`).
pub fn read_setting(
    block_id: u32,
    stream: &mut QDataStreamReader<'_>,
    version: i32,
    context: &mut ReadSettingsContext,
) -> Result<()> {
    let block = DbiBlockId::from_u32(block_id).ok_or(StorageError::UnknownTag(block_id))?;

    match block {
        DbiBlockId::DcOptionOldOld => {
            let dc_id = stream.read_u32()?;
            let _host = stream.read_qstring()?;
            let ip = stream.read_qstring()?;
            let port = stream.read_u32()?;
            context.legacy_dc_options.push(LegacyDcOption {
                dc_id,
                flags: 0,
                ip,
                port,
            });
            context.legacy_read = true;
        }
        DbiBlockId::DcOptionOld => {
            let dc_id = stream.read_u32()?;
            let flags = stream.read_i32()?;
            let ip = stream.read_qstring()?;
            let port = stream.read_u32()?;
            context.legacy_dc_options.push(LegacyDcOption {
                dc_id,
                flags,
                ip,
                port,
            });
            context.legacy_read = true;
        }
        DbiBlockId::DcOptionsOld => {
            context.legacy_dc_options_serialized = stream.read_byte_array()?;
            context.legacy_read = true;
        }
        DbiBlockId::ApplicationSettings => {
            context.application_settings = stream.read_byte_array()?;
        }
        DbiBlockId::ChatSizeMaxOld => {
            context.legacy_chat_size_max = stream.read_i32()?;
            context.legacy_read = true;
        }
        DbiBlockId::SavedGifsLimitOld => {
            context.legacy_saved_gifs_limit = stream.read_i32()?;
            context.legacy_read = true;
        }
        DbiBlockId::StickersRecentLimitOld => {
            context.legacy_stickers_recent_limit = stream.read_i32()?;
            context.legacy_read = true;
        }
        DbiBlockId::StickersFavedLimitOld => {
            context.legacy_stickers_faved_limit = stream.read_i32()?;
            context.legacy_read = true;
        }
        DbiBlockId::MegagroupSizeMaxOld => {
            context.legacy_megagroup_size_max = stream.read_i32()?;
            context.legacy_read = true;
        }
        DbiBlockId::User => {
            let user_id = stream.read_i32()?;
            let dc_id = stream.read_u32()?;
            context.mtp_legacy_user_id = user_id;
            context.mtp_legacy_main_dc_id = dc_id as i32;
        }
        DbiBlockId::Key => {
            let dc_id = stream.read_i32()?;
            let key_data = stream.read_exact_array::<AUTH_KEY_SIZE>()?;
            context.mtp_legacy_keys.push(MtpAuthKeyEntry {
                dc_id,
                key: AuthKey::new(key_data),
            });
        }
        DbiBlockId::MtpAuthorization => {
            context.mtp_authorization = stream.read_byte_array()?;
        }
        DbiBlockId::AutoStart => {
            context.auto_start = Some(stream.read_i32()? == 1);
        }
        DbiBlockId::StartMinimized => {
            context.start_minimized = Some(stream.read_i32()? == 1);
        }
        DbiBlockId::SendToMenu => {
            context.send_to_menu = Some(stream.read_i32()? == 1);
        }
        DbiBlockId::UseExternalVideoPlayerOld => {
            let _v = stream.read_i32()?;
        }
        DbiBlockId::CacheSettingsOld => {
            let size = stream.read_i64()?;
            let time = stream.read_i32()?;
            if size <= CACHE_MAX_DATA_SIZE || (!no_time_limit(time) && time < 0) {
                return Err(StorageError::CorruptData(format!(
                    "invalid legacy cache settings: size={size}, time={time}"
                )));
            }
            let normalized = if no_time_limit(time) { 0 } else { time };
            context.cache_total_size_limit = size;
            context.cache_total_time_limit = normalized;
            context.cache_big_file_total_size_limit = size;
            context.cache_big_file_total_time_limit = normalized;
            context.legacy_read = true;
        }
        DbiBlockId::CacheSettings => {
            let size = stream.read_i64()?;
            let time = stream.read_i32()?;
            let size_big = stream.read_i64()?;
            let time_big = stream.read_i32()?;
            if size <= CACHE_MAX_DATA_SIZE
                || size_big <= CACHE_MAX_DATA_SIZE
                || (!no_time_limit(time) && time < 0)
                || (!no_time_limit(time_big) && time_big < 0)
            {
                return Err(StorageError::CorruptData(format!(
                    "invalid cache settings: size={size}, time={time}, size_big={size_big}, time_big={time_big}"
                )));
            }
            context.cache_total_size_limit = size;
            context.cache_total_time_limit = if no_time_limit(time) { 0 } else { time };
            context.cache_big_file_total_size_limit = size_big;
            context.cache_big_file_total_time_limit =
                if no_time_limit(time_big) { 0 } else { time_big };
        }
        DbiBlockId::PowerSaving => {
            context.power_saving = Some(stream.read_i32()?);
        }
        DbiBlockId::SoundFlashBounceNotifyOld
        | DbiBlockId::AutoPlayOld
        | DbiBlockId::DialogsFiltersOld
        | DbiBlockId::ModerateModeOld
        | DbiBlockId::IncludeMutedOld
        | DbiBlockId::ShowingSavedGifsOld
        | DbiBlockId::DesktopNotifyOld
        | DbiBlockId::WindowsNotificationsOld
        | DbiBlockId::NativeNotificationsOld
        | DbiBlockId::NotificationsCountOld
        | DbiBlockId::NotificationsCornerOld
        | DbiBlockId::DialogsWidthRatioOld
        | DbiBlockId::LastSeenWarningSeenOld
        | DbiBlockId::WorkModeOld
        | DbiBlockId::LangOld
        | DbiBlockId::CatsAndDogsOld
        | DbiBlockId::AdaptiveForWideOld
        | DbiBlockId::AutoLockOld
        | DbiBlockId::ReplaceEmojiOld
        | DbiBlockId::SuggestEmojiOld
        | DbiBlockId::SuggestStickersByEmojiOld
        | DbiBlockId::NotifyViewOld
        | DbiBlockId::AskDownloadPathOld
        | DbiBlockId::CompressPastedImageOld
        | DbiBlockId::EmojiTabOld
        | DbiBlockId::SongVolumeOld
        | DbiBlockId::VideoVolumeOld
        | DbiBlockId::PlaybackSpeedOld => {
            let _v = stream.read_i32()?;
            context.legacy_read = true;
        }
        DbiBlockId::DefaultAttach => {
            let _v = stream.read_i32()?;
        }
        DbiBlockId::AutoDownloadOld => {
            let _photo = stream.read_i32()?;
            let _audio = stream.read_i32()?;
            let _gif = stream.read_i32()?;
            context.legacy_read = true;
        }
        DbiBlockId::DialogsModeOld => {
            let _enabled = stream.read_i32()?;
            let _mode = stream.read_i32()?;
            context.legacy_read = true;
        }
        DbiBlockId::SessionSettings => {
            context.session_settings = stream.read_byte_array()?;
        }
        DbiBlockId::TxtDomainStringOldOld
        | DbiBlockId::LangFileOld
        | DbiBlockId::LoggedPhoneNumberOld
        | DbiBlockId::DownloadPathOldOld => {
            let _v = stream.read_qstring()?;
            context.legacy_read = true;
        }
        DbiBlockId::TxtDomainStringOld => {
            context.legacy_txt_domain_string = stream.read_qstring()?;
            context.legacy_read = true;
        }
        DbiBlockId::DownloadPathOld => {
            let _v = stream.read_qstring()?;
            let _bookmark = stream.read_byte_array()?;
            context.legacy_read = true;
        }
        DbiBlockId::TryIPv6Old => {
            let v = stream.read_i32()?;
            let entry = context.legacy_proxy.get_or_insert_with(Default::default);
            entry.try_ipv6 = Some(v == 1);
            context.legacy_read = true;
        }
        DbiBlockId::ConnectionTypeOldOld => {
            const DBICT_HTTP_PROXY: i32 = 2;
            const DBICT_TCP_PROXY: i32 = 3;
            let v = stream.read_i32()?;
            let mut proxy = LegacyProxyData::default();
            if v == DBICT_HTTP_PROXY || v == DBICT_TCP_PROXY {
                let host = stream.read_qstring()?;
                let port = stream.read_i32()?;
                let user = stream.read_qstring()?;
                let password = stream.read_qstring()?;
                proxy = LegacyProxyData {
                    proxy_type: if v == DBICT_TCP_PROXY { 1 } else { 2 },
                    host,
                    port: port as u32,
                    user,
                    password,
                };
            }
            let entry = context.legacy_proxy.get_or_insert_with(Default::default);
            if proxy.is_valid() {
                context.legacy_proxies.push(proxy.clone());
                entry.selected = Some(proxy.clone());
                entry.mode = 1; // Enabled
                entry.list = vec![proxy];
            } else {
                entry.selected = None;
                entry.mode = 0; // System
                entry.list.clear();
            }
            context.legacy_read = true;
        }
        DbiBlockId::ConnectionTypeOld => {
            const LEGACY_PROXY_TYPE_SHIFT: i32 = 1024;
            const DBICT_HTTP_PROXY: i32 = 2;
            const DBICT_TCP_PROXY: i32 = 3;
            const DBICT_PROXIES_LIST_OLD: i32 = 4;
            const DBICT_PROXIES_LIST: i32 = 5;

            let conn_type = stream.read_i32()?;
            let read_proxy = |s: &mut QDataStreamReader<'_>| -> Result<LegacyProxyData> {
                let raw_type = s.read_i32()?;
                let host = s.read_qstring()?;
                let port = s.read_i32()?;
                let user = s.read_qstring()?;
                let password = s.read_qstring()?;
                let proxy_type = if raw_type == DBICT_TCP_PROXY
                    || raw_type == LEGACY_PROXY_TYPE_SHIFT + 1
                {
                    1 // Socks5
                } else if raw_type == DBICT_HTTP_PROXY || raw_type == LEGACY_PROXY_TYPE_SHIFT + 2 {
                    2 // Http
                } else if raw_type == LEGACY_PROXY_TYPE_SHIFT + 3 {
                    3 // Mtproto
                } else {
                    0 // None
                };
                Ok(LegacyProxyData {
                    proxy_type,
                    host,
                    port: port as u32,
                    user,
                    password,
                })
            };

            let entry = context.legacy_proxy.get_or_insert_with(Default::default);
            if conn_type == DBICT_PROXIES_LIST_OLD || conn_type == DBICT_PROXIES_LIST {
                let count = stream.read_i32()?;
                let mut index = stream.read_i32()?;
                let mut settings = 0i32;
                let mut calls = 0i32;
                if conn_type == DBICT_PROXIES_LIST {
                    settings = stream.read_i32()?;
                    calls = stream.read_i32()?;
                } else if i64::from(index).abs() > i64::from(count) {
                    calls = 1;
                    index -= if index > 0 { count } else { -count };
                }
                if !(0..=10_000).contains(&count) {
                    return Err(StorageError::CorruptData(format!(
                        "invalid legacy proxy count: {count}"
                    )));
                }
                let mut list = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    let proxy = read_proxy(stream)?;
                    if proxy.is_valid() {
                        context.legacy_proxies.push(proxy.clone());
                        list.push(proxy);
                    } else if i64::from(index) < -(list.len() as i64) {
                        index += 1;
                    } else if i64::from(index) > list.len() as i64 {
                        index -= 1;
                    }
                }
                if conn_type == DBICT_PROXIES_LIST_OLD {
                    settings = if index > 0 && (index as usize) <= list.len() {
                        1 // Enabled
                    } else {
                        0 // System
                    };
                    index = index.saturating_abs();
                }
                let selected = if index > 0 && (index as usize) <= list.len() {
                    Some(list[(index - 1) as usize].clone())
                } else {
                    None
                };
                let mode = match settings {
                    1 if selected.is_some() => 1, // Enabled
                    2 => 2,                       // Disabled
                    _ => 0,                       // System
                };
                entry.list = list;
                entry.selected = selected;
                entry.mode = mode;
                entry.use_proxy_for_calls = calls == 1;
            } else {
                let proxy = read_proxy(stream)?;
                if proxy.is_valid() {
                    context.legacy_proxies.push(proxy.clone());
                    entry.list = vec![proxy.clone()];
                    entry.selected = Some(proxy);
                    entry.mode = if conn_type == DBICT_TCP_PROXY || conn_type == DBICT_HTTP_PROXY {
                        1 // Enabled
                    } else {
                        0 // System
                    };
                } else {
                    entry.list.clear();
                    entry.selected = None;
                    entry.mode = 0; // System
                }
            }
            context.legacy_read = true;
        }
        DbiBlockId::ThemeKeyOld => {
            context.theme_key_legacy = stream.read_u64()?;
            context.legacy_read = true;
        }
        DbiBlockId::ThemeKey => {
            context.theme_key_day = stream.read_u64()?;
            context.theme_key_night = stream.read_u64()?;
            context.night_mode = stream.read_u32()? == 1;
        }
        DbiBlockId::BackgroundKey => {
            context.background_key_day = stream.read_u64()?;
            context.background_key_night = stream.read_u64()?;
            context.background_keys_read = true;
        }
        DbiBlockId::LangPackKey => {
            context.lang_pack_key = stream.read_u64()?;
        }
        DbiBlockId::LanguagesKey => {
            context.languages_key = stream.read_u64()?;
        }
        DbiBlockId::SeenTrayTooltip => {
            context.seen_tray_tooltip = Some(stream.read_i32()? == 1);
        }
        DbiBlockId::AutoUpdate => {
            context.auto_update = Some(stream.read_i32()? == 1);
        }
        DbiBlockId::LastUpdateCheck => {
            context.last_update_check = Some(stream.read_i32()?);
        }
        DbiBlockId::ScaleOld => {
            let v = stream.read_i32()?;
            let percent = match v {
                0 => 0,
                1 => 100,
                2 => 125,
                3 => 150,
                4 => 200,
                _ => 0,
            };
            context.scale_percent = Some(percent);
            context.legacy_read = true;
        }
        DbiBlockId::ScalePercent => {
            context.scale_percent = Some(stream.read_i32()?);
        }
        DbiBlockId::WindowPositionOld => {
            let pos = LegacyWindowPosition {
                x: stream.read_i32()?,
                y: stream.read_i32()?,
                w: stream.read_i32()?,
                h: stream.read_i32()?,
                moncrc: stream.read_i32()?,
                maximized: stream.read_i32()?,
            };
            context.legacy_window_position = Some(pos);
            context.legacy_read = true;
        }
        DbiBlockId::MutePeerOld => {
            let _peer_id = stream.read_u64()?;
            context.legacy_read = true;
        }
        DbiBlockId::MutedPeersOld => {
            let _peers = stream.read_qvector(|s| s.read_u64())?;
            context.legacy_read = true;
        }
        DbiBlockId::SendKeyOld => {
            let v = stream.read_i32()?;
            if v != 0 && v != 1 {
                return Err(StorageError::CorruptData(format!(
                    "invalid dbiSendKeyOld value: {v}"
                )));
            }
            context.legacy_read = true;
        }
        DbiBlockId::TileBackgroundOld => {
            let v = stream.read_i32()?;
            let tile = if version < 8005 && !context.legacy_has_custom_day_background {
                false
            } else {
                v == 1
            };
            if context.night_mode {
                context.tile_day = tile;
            } else {
                context.tile_night = tile;
            }
            context.tile_read = true;
            context.legacy_read = true;
        }
        DbiBlockId::TileBackground => {
            context.tile_day = stream.read_i32()? != 0;
            context.tile_night = stream.read_i32()? != 0;
            context.tile_read = true;
        }
        DbiBlockId::RecentEmojiOldOldOld => {
            let _ = stream.read_qvector(|s| Ok((s.read_u32()?, s.read_u16()?)))?;
            context.legacy_read = true;
        }
        DbiBlockId::RecentEmojiOldOld => {
            let _ = stream.read_qvector(|s| Ok((s.read_u64()?, s.read_u16()?)))?;
            context.legacy_read = true;
        }
        DbiBlockId::RecentEmojiOld => {
            let _ = stream.read_qvector(|s| Ok((s.read_qstring()?, s.read_u16()?)))?;
            context.legacy_read = true;
        }
        DbiBlockId::RecentStickers => {
            context.recent_stickers_preload =
                stream.read_qvector(|s| Ok((s.read_u64()?, s.read_u16()?)))?;
        }
        DbiBlockId::EmojiVariantsOldOld => {
            let _ = stream.read_qmap(|s| s.read_u32(), |s| s.read_u64())?;
            context.legacy_read = true;
        }
        DbiBlockId::EmojiVariantsOld => {
            let _ = stream.read_qmap(|s| s.read_qstring(), |s| s.read_i32())?;
            context.legacy_read = true;
        }
        DbiBlockId::HiddenPinnedMessagesOld => {
            let pairs = stream.read_qmap(|s| s.read_u64(), |s| s.read_i32())?;
            for (peer_raw, msg_id) in pairs {
                context
                    .legacy_hidden_pinned_messages
                    .push((deserialize_peer_id(peer_raw), msg_id));
            }
            context.legacy_read = true;
        }
        DbiBlockId::DialogLastPath => {
            context.dialog_last_path = Some(stream.read_qstring()?);
        }
        DbiBlockId::CallSettingsOld => {
            let blob = stream.read_byte_array()?;
            let mut inner = QDataStreamReader::new(&blob);
            if let Ok(output_device_id) = inner.read_qstring()
                && let Ok(output_volume) = inner.read_i32()
                && let Ok(input_device_id) = inner.read_qstring()
                && let Ok(input_volume) = inner.read_i32()
                && let Ok(ducking_enabled) = inner.read_i32()
            {
                context.legacy_call_settings = Some(LegacyCallSettings {
                    output_device_id,
                    output_volume,
                    input_device_id,
                    input_volume,
                    ducking_enabled,
                });
            }
            context.legacy_read = true;
        }
        DbiBlockId::FallbackProductionConfig => {
            context.fallback_config = stream.read_byte_array()?;
        }
        DbiBlockId::EncryptedWithSalt | DbiBlockId::Encrypted | DbiBlockId::Version => {
            return Err(StorageError::UnknownTag(block_id));
        }
    }

    Ok(())
}

/// Read all `dbi*` blocks until end of stream into `context`.
pub fn read_all_settings(
    data: &[u8],
    version: i32,
    context: &mut ReadSettingsContext,
) -> Result<()> {
    let mut stream = QDataStreamReader::new(data);
    while !stream.at_end() {
        let block_id = stream.read_u32()?;
        read_setting(block_id, &mut stream, version, context)?;
    }
    Ok(())
}

/// Write a `dbiMtpAuthorization` block payload (`Account::writeMtpData`).
#[must_use]
pub fn write_mtp_authorization_block(serialized_mtp_auth: &[u8]) -> Vec<u8> {
    let mut w = QDataStreamWriter::with_capacity(8 + serialized_mtp_auth.len());
    w.write_u32(DbiBlockId::MtpAuthorization as u32);
    w.write_byte_array(serialized_mtp_auth);
    w.into_bytes()
}

/// Write per-account session settings blocks (`Account::writeSessionSettings`).
#[must_use]
pub fn write_account_session_settings_blocks(
    cache_total_size_limit: i64,
    cache_total_time_limit: i32,
    cache_big_file_total_size_limit: i64,
    cache_big_file_total_time_limit: i32,
    session_settings_blob: &[u8],
    recent_stickers: &[(u64, u16)],
) -> Vec<u8> {
    let mut w = QDataStreamWriter::new();
    w.write_u32(DbiBlockId::CacheSettings as u32);
    w.write_i64(cache_total_size_limit);
    w.write_i32(cache_total_time_limit);
    w.write_i64(cache_big_file_total_size_limit);
    w.write_i32(cache_big_file_total_time_limit);

    if !session_settings_blob.is_empty() {
        w.write_u32(DbiBlockId::SessionSettings as u32);
        w.write_byte_array(session_settings_blob);
    }

    w.write_u32(DbiBlockId::RecentStickers as u32);
    w.write_u32(recent_stickers.len() as u32);
    for &(doc_id, count) in recent_stickers {
        w.write_u64(doc_id);
        w.write_u16(count);
    }
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tdf::APP_VERSION;

    #[test]
    fn account_session_settings_blocks_roundtrip() {
        let bytes = write_account_session_settings_blocks(
            DEFAULT_CACHE_TOTAL_SIZE_LIMIT,
            DEFAULT_CACHE_TOTAL_TIME_LIMIT,
            2 * DEFAULT_CACHE_TOTAL_SIZE_LIMIT,
            0,
            b"session_blob",
            &[(1001, 5), (2002, 2)],
        );

        let mut ctx = ReadSettingsContext::default();
        read_all_settings(&bytes, APP_VERSION, &mut ctx).expect("read settings");

        assert_eq!(ctx.cache_total_size_limit, DEFAULT_CACHE_TOTAL_SIZE_LIMIT);
        assert_eq!(ctx.cache_total_time_limit, DEFAULT_CACHE_TOTAL_TIME_LIMIT);
        assert_eq!(
            ctx.cache_big_file_total_size_limit,
            2 * DEFAULT_CACHE_TOTAL_SIZE_LIMIT
        );
        assert_eq!(ctx.cache_big_file_total_time_limit, 0);
        assert_eq!(ctx.session_settings, b"session_blob");
        assert_eq!(ctx.recent_stickers_preload, vec![(1001, 5), (2002, 2)]);
        assert!(!ctx.legacy_read);
    }

    #[test]
    fn legacy_settings_blocks_parse_correctly() {
        let mut w = QDataStreamWriter::new();
        w.write_u32(DbiBlockId::User as u32);
        w.write_i32(777);
        w.write_u32(2);

        w.write_u32(DbiBlockId::Key as u32);
        w.write_i32(2);
        w.write_raw(&[0x55u8; 256]);

        w.write_u32(DbiBlockId::ThemeKey as u32);
        w.write_u64(0x1111);
        w.write_u64(0x2222);
        w.write_u32(1);

        w.write_u32(DbiBlockId::ScalePercent as u32);
        w.write_i32(125);

        let bytes = w.into_bytes();
        let mut ctx = ReadSettingsContext::default();
        read_all_settings(&bytes, APP_VERSION, &mut ctx).expect("read legacy");

        assert_eq!(ctx.mtp_legacy_user_id, 777);
        assert_eq!(ctx.mtp_legacy_main_dc_id, 2);
        assert_eq!(ctx.mtp_legacy_keys.len(), 1);
        assert_eq!(ctx.mtp_legacy_keys[0].dc_id, 2);
        assert_eq!(ctx.mtp_legacy_keys[0].key.data(), &[0x55u8; 256]);
        assert_eq!(ctx.theme_key_day, 0x1111);
        assert_eq!(ctx.theme_key_night, 0x2222);
        assert!(ctx.night_mode);
        assert_eq!(ctx.scale_percent, Some(125));
    }

    #[test]
    fn legacy_connection_type_and_ipv6_blocks_parse_correctly() {
        // Test dbiConnectionTypeOld (dbictProxiesList = 5) with shifted types, calls flag,
        // selected index adjusted past an invalid entry, and dbiTryIPv6Old.
        let mut w = QDataStreamWriter::new();
        w.write_u32(DbiBlockId::ConnectionTypeOld as u32);
        w.write_i32(5); // dbictProxiesList
        w.write_i32(3); // count
        w.write_i32(3); // 1-based selected index (points to 3rd raw entry -> 2nd valid entry)
        w.write_i32(1); // settings = Enabled
        w.write_i32(1); // useProxyForCalls = true

        // Entry 1: valid SOCKS5 (legacy shift 1024 + 1)
        w.write_i32(1024 + 1);
        w.write_qstring("127.0.0.1");
        w.write_i32(1080);
        w.write_qstring("user1");
        w.write_qstring("pass1");

        // Entry 2: invalid (type None / empty) -> should be skipped and decrement index
        w.write_i32(0);
        w.write_qstring("");
        w.write_i32(0);
        w.write_qstring("");
        w.write_qstring("");

        // Entry 3: valid MTProto (legacy shift 1024 + 3)
        w.write_i32(1024 + 3);
        w.write_qstring("mtproxy.example.test");
        w.write_i32(443);
        w.write_qstring("");
        w.write_qstring("dd0123456789abcdef0123456789abcdef");

        w.write_u32(DbiBlockId::TryIPv6Old as u32);
        w.write_i32(1);

        let bytes = w.into_bytes();
        let mut ctx = ReadSettingsContext::default();
        read_all_settings(&bytes, APP_VERSION, &mut ctx).expect("read legacy proxy");

        let legacy = ctx.legacy_proxy.expect("legacy proxy parsed");
        assert_eq!(legacy.mode, 1);
        assert!(legacy.use_proxy_for_calls);
        assert_eq!(legacy.try_ipv6, Some(true));
        assert_eq!(legacy.list.len(), 2);
        assert_eq!(legacy.list[0].proxy_type, 1);
        assert_eq!(legacy.list[0].host, "127.0.0.1");
        assert_eq!(legacy.list[1].proxy_type, 3);
        assert_eq!(legacy.list[1].host, "mtproxy.example.test");
        assert_eq!(legacy.selected.as_ref(), Some(&legacy.list[1]));
    }
}
