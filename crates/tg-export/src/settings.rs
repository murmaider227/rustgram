//! Port of `export/export_settings.h`, `export/export_settings.cpp`,
//! and `storage/storage_account.cpp` (`writeExportSettings` / `readExportSettings`).

use bitflags::bitflags;

use crate::output::OutputFormat;
use crate::stand_in::{InputPeer, TimeId};
use tg_tl::types::{InputPeerChannel, InputPeerChat, InputPeerUser};

/// Maximum allowed media file size limit (4000 MiB, matching `kMaxFileSize` in `export_settings.cpp`).
pub const MAX_FILE_SIZE: i64 = 4000 * 1024 * 1024;

/// Default media file size limit (8 MiB).
pub const DEFAULT_MEDIA_SIZE_LIMIT: i64 = 8 * 1024 * 1024;

bitflags! {
    /// Media categories to download during export (port of `Export::MediaSettings::Type`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct MediaTypes: u32 {
        /// Photos.
        const PHOTO = 0x01;
        /// Video files.
        const VIDEO = 0x02;
        /// Voice messages.
        const VOICE_MESSAGE = 0x04;
        /// Round video messages.
        const VIDEO_MESSAGE = 0x08;
        /// Stickers.
        const STICKER = 0x10;
        /// Animated GIFs / animations.
        const GIF = 0x20;
        /// Generic files and audio files.
        const FILE = 0x40;

        /// Core media mask (`PHOTO | VIDEO | VOICE_MESSAGE | VIDEO_MESSAGE`).
        const MEDIA_MASK = Self::PHOTO.bits()
            | Self::VIDEO.bits()
            | Self::VOICE_MESSAGE.bits()
            | Self::VIDEO_MESSAGE.bits();
        /// All valid media type flags.
        const ALL_MASK = Self::MEDIA_MASK.bits()
            | Self::STICKER.bits()
            | Self::GIF.bits()
            | Self::FILE.bits();
    }
}

impl Default for MediaTypes {
    fn default() -> Self {
        Self::PHOTO
    }
}

/// Media download filter settings (port of `Export::MediaSettings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaSettings {
    /// Enabled media categories.
    pub types: MediaTypes,
    /// Maximum single file size in bytes.
    pub size_limit: i64,
}

impl Default for MediaSettings {
    fn default() -> Self {
        Self {
            types: MediaTypes::default(),
            size_limit: DEFAULT_MEDIA_SIZE_LIMIT,
        }
    }
}

impl MediaSettings {
    /// Validates flags and size limit (port of `MediaSettings::validate`).
    pub fn validate(&self) -> bool {
        if (self.types.bits() | MediaTypes::ALL_MASK.bits()) != MediaTypes::ALL_MASK.bits() {
            return false;
        }
        if self.size_limit < 0 || self.size_limit > MAX_FILE_SIZE {
            return false;
        }
        true
    }
}

bitflags! {
    /// Account / chat export sections (port of `Export::Settings::Type`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ExportTypes: u32 {
        /// Account personal information (name, phone, username, bio).
        const PERSONAL_INFO = 0x001;
        /// Profile pictures archive.
        const USERPICS = 0x002;
        /// Contacts and frequent contacts lists.
        const CONTACTS = 0x004;
        /// Active sessions and web sessions.
        const SESSIONS = 0x008;
        /// Server-supplied `other_data.json`.
        const OTHER_DATA = 0x010;
        /// Personal (1-on-1) chats.
        const PERSONAL_CHATS = 0x020;
        /// Bot chats.
        const BOT_CHATS = 0x040;
        /// Private groups and private supergroups.
        const PRIVATE_GROUPS = 0x080;
        /// Public supergroups.
        const PUBLIC_GROUPS = 0x100;
        /// Private channels.
        const PRIVATE_CHANNELS = 0x200;
        /// Public channels.
        const PUBLIC_CHANNELS = 0x400;
        /// Stories archive.
        const STORIES = 0x800;
        /// Profile music archive.
        const PROFILE_MUSIC = 0x1000;

        /// Both private and public groups.
        const GROUPS_MASK = Self::PRIVATE_GROUPS.bits() | Self::PUBLIC_GROUPS.bits();
        /// Both private and public channels.
        const CHANNELS_MASK = Self::PRIVATE_CHANNELS.bits() | Self::PUBLIC_CHANNELS.bits();
        /// All groups and channels.
        const GROUPS_CHANNELS_MASK = Self::GROUPS_MASK.bits() | Self::CHANNELS_MASK.bits();
        /// Non-channel chats (`PERSONAL_CHATS | BOT_CHATS | PRIVATE_GROUPS`).
        const NON_CHANNEL_CHATS_MASK = Self::PERSONAL_CHATS.bits()
            | Self::BOT_CHATS.bits()
            | Self::PRIVATE_GROUPS.bits();
        /// Any chat category.
        const ANY_CHATS_MASK = Self::PERSONAL_CHATS.bits()
            | Self::BOT_CHATS.bits()
            | Self::GROUPS_CHANNELS_MASK.bits();
        /// Non-chat account sections.
        const NON_CHATS_MASK = Self::PERSONAL_INFO.bits()
            | Self::USERPICS.bits()
            | Self::CONTACTS.bits()
            | Self::STORIES.bits()
            | Self::PROFILE_MUSIC.bits()
            | Self::SESSIONS.bits();
        /// All valid export type bits.
        const ALL_MASK = Self::NON_CHATS_MASK.bits()
            | Self::OTHER_DATA.bits()
            | Self::ANY_CHATS_MASK.bits();
    }
}

impl ExportTypes {
    /// Default export sections (`Settings::DefaultTypes`).
    pub const fn default_types() -> Self {
        Self::PERSONAL_INFO
            .union(Self::USERPICS)
            .union(Self::CONTACTS)
            .union(Self::STORIES)
            .union(Self::PROFILE_MUSIC)
            .union(Self::PERSONAL_CHATS)
            .union(Self::PRIVATE_GROUPS)
    }

    /// Default full-history chat sections (`Settings::DefaultFullChats`).
    pub const fn default_full_chats() -> Self {
        Self::PERSONAL_CHATS.union(Self::BOT_CHATS)
    }
}

/// Export configuration (port of `Export::Settings`).
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Target output directory path.
    pub path: String,
    /// Always create a `DataExport_YYYY-MM-DD` / `ChatExport_YYYY-MM-DD` subdirectory.
    pub force_sub_path: bool,
    /// Output format (HTML, JSON, or both).
    pub format: OutputFormat,
    /// Enabled export sections.
    pub types: ExportTypes,
    /// Chat types for which full history (not only outgoing messages) is exported.
    pub full_chats: ExportTypes,
    /// Media download filters.
    pub media: MediaSettings,

    /// Single-peer export target (`InputPeer::Empty` for full account export).
    pub single_peer: InputPeer,
    /// Inclusive start timestamp for single-peer date filter (0 = no lower bound).
    pub single_peer_from: TimeId,
    /// Exclusive end timestamp for single-peer date filter (0 = no upper bound).
    pub single_peer_till: TimeId,

    /// Single forum topic root message ID (0 = not a topic export).
    pub single_topic_root_id: i32,
    /// Peer ID of the forum supergroup for single-topic export.
    pub single_topic_peer_id: u64,
    /// Title of the exported topic.
    pub single_topic_title: String,

    /// Timestamp when takeout becomes available after a `TAKEOUT_INIT_DELAY_*` error.
    pub available_at: TimeId,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            path: String::new(),
            force_sub_path: false,
            format: OutputFormat::Html,
            types: ExportTypes::default_types(),
            full_chats: ExportTypes::default_full_chats(),
            media: MediaSettings::default(),
            single_peer: InputPeer::Empty,
            single_peer_from: 0,
            single_peer_till: 0,
            single_topic_root_id: 0,
            single_topic_peer_id: 0,
            single_topic_title: String::new(),
            available_at: 0,
        }
    }
}

const SINGLE_PEER_TYPE_EMPTY: i32 = 0;
const SINGLE_PEER_TYPE_USER_OLD: i32 = 1;
const SINGLE_PEER_TYPE_CHAT_OLD: i32 = 2;
const SINGLE_PEER_TYPE_CHANNEL_OLD: i32 = 3;
const SINGLE_PEER_TYPE_SELF: i32 = 4;
const SINGLE_PEER_TYPE_USER: i32 = 8 + 1;
const SINGLE_PEER_TYPE_CHAT: i32 = 8 + 2;
const SINGLE_PEER_TYPE_CHANNEL: i32 = 8 + 3;

impl Settings {
    /// Returns true if exporting only a single peer (or single topic).
    pub fn only_single_peer(&self) -> bool {
        !matches!(self.single_peer, InputPeer::Empty)
    }

    /// Returns true if exporting only a single forum topic.
    pub fn only_single_topic(&self) -> bool {
        self.only_single_peer() && self.single_topic_root_id != 0
    }

    /// Validates all fields (port of `Settings::validate` in `export_settings.cpp`).
    ///
    /// Note: allows `OutputFormat::HtmlAndJson` in addition to `Html` and `Json` so that
    /// combined HTML+JSON exports selected in the UI also pass validation and round-trip.
    pub fn validate(&self) -> bool {
        let must_be_full = ExportTypes::PERSONAL_CHATS | ExportTypes::BOT_CHATS;
        let must_not_be_full = ExportTypes::PUBLIC_GROUPS | ExportTypes::PUBLIC_CHANNELS;
        if (self.types.bits() | ExportTypes::ALL_MASK.bits()) != ExportTypes::ALL_MASK.bits() {
            return false;
        }
        if (self.full_chats.bits() | ExportTypes::ALL_MASK.bits()) != ExportTypes::ALL_MASK.bits() {
            return false;
        }
        if (self.full_chats & must_be_full) != must_be_full {
            return false;
        }
        if !(self.full_chats & must_not_be_full).is_empty() {
            return false;
        }
        if !self.media.validate() {
            return false;
        }
        if self.single_peer_till > 0 && self.single_peer_till <= self.single_peer_from {
            return false;
        }
        true
    }

    /// Returns true if persisted fields match default `Settings` (matches `writeExportSettings` skip check).
    pub fn is_persisted_default(&self) -> bool {
        let check = Self::default();
        self.types == check.types
            && self.full_chats == check.full_chats
            && self.media.types == check.media.types
            && self.media.size_limit == check.media.size_limit
            && self.path == check.path
            && self.format == check.format
            && self.available_at == check.available_at
            && !self.only_single_peer()
    }

    /// Serializes settings into tdesktop's `QDataStream` big-endian binary format
    /// (port of `Storage::Account::writeExportSettings` in `storage/storage_account.cpp:3020-3050`).
    pub fn serialize_qt(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.path.len() * 2);
        out.extend_from_slice(&self.types.bits().to_be_bytes());
        out.extend_from_slice(&self.full_chats.bits().to_be_bytes());
        out.extend_from_slice(&self.media.types.bits().to_be_bytes());
        out.extend_from_slice(&(self.media.size_limit as u32).to_be_bytes());
        out.extend_from_slice(&self.format.to_raw().to_be_bytes());
        write_qstring(&mut out, &self.path);
        out.extend_from_slice(&(self.available_at as u32).to_be_bytes());
        match &self.single_peer {
            InputPeer::User(InputPeerUser {
                user_id,
                access_hash,
            }) => {
                out.extend_from_slice(&SINGLE_PEER_TYPE_USER.to_be_bytes());
                out.extend_from_slice(&(*user_id as u64).to_be_bytes());
                out.extend_from_slice(&(*access_hash as u64).to_be_bytes());
            }
            InputPeer::Chat(InputPeerChat { chat_id }) => {
                out.extend_from_slice(&SINGLE_PEER_TYPE_CHAT.to_be_bytes());
                out.extend_from_slice(&(*chat_id as u64).to_be_bytes());
            }
            InputPeer::Channel(InputPeerChannel {
                channel_id,
                access_hash,
            }) => {
                out.extend_from_slice(&SINGLE_PEER_TYPE_CHANNEL.to_be_bytes());
                out.extend_from_slice(&(*channel_id as u64).to_be_bytes());
                out.extend_from_slice(&(*access_hash as u64).to_be_bytes());
            }
            InputPeer::Self_ => {
                out.extend_from_slice(&SINGLE_PEER_TYPE_SELF.to_be_bytes());
            }
            InputPeer::Empty | InputPeer::UserFromMessage(_) | InputPeer::ChannelFromMessage(_) => {
                out.extend_from_slice(&SINGLE_PEER_TYPE_EMPTY.to_be_bytes());
            }
        }
        out.extend_from_slice(&self.single_peer_from.to_be_bytes());
        out.extend_from_slice(&self.single_peer_till.to_be_bytes());
        out
    }

    /// Deserializes settings from tdesktop's `QDataStream` big-endian binary format
    /// (port of `Storage::Account::readExportSettings` in `storage/storage_account.cpp:3069-3150`).
    ///
    /// Returns `None` if the stream is truncated, malformed, or fails validation.
    pub fn deserialize_qt(bytes: &[u8]) -> Option<Self> {
        let mut cursor = bytes;
        let types = read_u32(&mut cursor)?;
        let full_chats = read_u32(&mut cursor)?;
        let media_types = read_u32(&mut cursor)?;
        let media_size_limit = read_u32(&mut cursor)?;
        let format_raw = read_u32(&mut cursor)?;
        let path = read_qstring(&mut cursor)?;
        let available_at = read_u32(&mut cursor)?;

        let mut single_peer = InputPeer::Empty;
        if !cursor.is_empty() {
            let single_peer_type = read_i32(&mut cursor)?;
            single_peer = match single_peer_type {
                SINGLE_PEER_TYPE_USER_OLD => {
                    let bare = read_i32(&mut cursor)?;
                    let access_hash = read_u64(&mut cursor)?;
                    InputPeer::User(InputPeerUser {
                        user_id: i64::from(bare),
                        access_hash: access_hash as i64,
                    })
                }
                SINGLE_PEER_TYPE_CHAT_OLD => {
                    let bare = read_i32(&mut cursor)?;
                    InputPeer::Chat(InputPeerChat {
                        chat_id: i64::from(bare),
                    })
                }
                SINGLE_PEER_TYPE_CHANNEL_OLD => {
                    let bare = read_i32(&mut cursor)?;
                    let access_hash = read_u64(&mut cursor)?;
                    InputPeer::Channel(InputPeerChannel {
                        channel_id: i64::from(bare),
                        access_hash: access_hash as i64,
                    })
                }
                SINGLE_PEER_TYPE_USER => {
                    let bare = read_u64(&mut cursor)?;
                    let access_hash = read_u64(&mut cursor)?;
                    InputPeer::User(InputPeerUser {
                        user_id: bare as i64,
                        access_hash: access_hash as i64,
                    })
                }
                SINGLE_PEER_TYPE_CHAT => {
                    let bare = read_u64(&mut cursor)?;
                    InputPeer::Chat(InputPeerChat {
                        chat_id: bare as i64,
                    })
                }
                SINGLE_PEER_TYPE_CHANNEL => {
                    let bare = read_u64(&mut cursor)?;
                    let access_hash = read_u64(&mut cursor)?;
                    InputPeer::Channel(InputPeerChannel {
                        channel_id: bare as i64,
                        access_hash: access_hash as i64,
                    })
                }
                SINGLE_PEER_TYPE_SELF => InputPeer::Self_,
                SINGLE_PEER_TYPE_EMPTY => InputPeer::Empty,
                _ => return None,
            };
        }

        let (single_peer_from, single_peer_till) = if !cursor.is_empty() {
            (read_i32(&mut cursor)?, read_i32(&mut cursor)?)
        } else {
            (0, 0)
        };

        let format = OutputFormat::from_raw(format_raw)?;
        let types_flags = ExportTypes::from_bits(types)?;
        let full_chats_flags = ExportTypes::from_bits(full_chats)?;
        let media_types_flags = MediaTypes::from_bits(media_types)?;

        let result = Self {
            path,
            force_sub_path: false,
            format,
            types: types_flags,
            full_chats: full_chats_flags,
            media: MediaSettings {
                types: media_types_flags,
                size_limit: i64::from(media_size_limit),
            },
            single_peer,
            single_peer_from,
            single_peer_till,
            single_topic_root_id: 0,
            single_topic_peer_id: 0,
            single_topic_title: String::new(),
            available_at: available_at as TimeId,
        };
        if result.validate() {
            Some(result)
        } else {
            None
        }
    }
}

fn read_u32(cursor: &mut &[u8]) -> Option<u32> {
    if cursor.len() < 4 {
        return None;
    }
    let (head, tail) = cursor.split_at(4);
    *cursor = tail;
    Some(u32::from_be_bytes([head[0], head[1], head[2], head[3]]))
}

fn read_i32(cursor: &mut &[u8]) -> Option<i32> {
    read_u32(cursor).map(|v| v as i32)
}

fn read_u64(cursor: &mut &[u8]) -> Option<u64> {
    if cursor.len() < 8 {
        return None;
    }
    let (head, tail) = cursor.split_at(8);
    *cursor = tail;
    Some(u64::from_be_bytes([
        head[0], head[1], head[2], head[3], head[4], head[5], head[6], head[7],
    ]))
}

/// Writes a `QString` in Qt `QDataStream` format (u32 byte length = 0xFFFF_FFFF for null,
/// or UTF-16BE code units).
fn write_qstring(out: &mut Vec<u8>, s: &str) {
    if s.is_empty() {
        out.extend_from_slice(&0xFFFF_FFFF_u32.to_be_bytes());
        return;
    }
    let units: Vec<u16> = s.encode_utf16().collect();
    let byte_len = (units.len() * 2) as u32;
    out.extend_from_slice(&byte_len.to_be_bytes());
    for u in units {
        out.extend_from_slice(&u.to_be_bytes());
    }
}

fn read_qstring(cursor: &mut &[u8]) -> Option<String> {
    let byte_len = read_u32(cursor)?;
    if byte_len == 0xFFFF_FFFF || byte_len == 0 {
        return Some(String::new());
    }
    if byte_len % 2 != 0 || cursor.len() < byte_len as usize {
        return None;
    }
    let (head, tail) = cursor.split_at(byte_len as usize);
    *cursor = tail;
    let units: Vec<u16> = head
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&units).ok()
}

/// Static text strings and link configuration supplied to writers (port of `Export::Environment`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    /// Base domain for internal Telegram links (e.g. `"https://t.me/"`).
    pub internal_links_domain: String,
    /// Explanatory text shown on the main export page / `result.json` `"about"`.
    pub about_telegram: String,
    /// Explanatory text for the contacts list.
    pub about_contacts: String,
    /// Explanatory text for frequent contacts.
    pub about_frequent: String,
    /// Explanatory text for active sessions.
    pub about_sessions: String,
    /// Explanatory text for web sessions.
    pub about_web_sessions: String,
    /// Explanatory text for the chats list.
    pub about_chats: String,
    /// Explanatory text for left channels/supergroups.
    pub about_left_chats: String,
    /// UTC offset in seconds used for formatting dates/times (defaults to `0` = UTC for
    /// deterministic output unless configured by the host application).
    pub utc_offset_seconds: i32,
}

impl Environment {
    /// Constructs an `Environment` populated with tdesktop's default English `lang.strings` texts.
    pub fn default_english() -> Self {
        Self {
            internal_links_domain: "https://t.me/".to_string(),
            about_telegram: "Here is the data you requested. Remember: Telegram is ad free, it doesn't use your data for ad targeting and doesn't sell it to others. Telegram only keeps the information it needs to function as a secure and feature-rich cloud service.\n\nCheck out Settings > Privacy & Security on Telegram's mobile apps for the relevant settings.".to_string(),
            about_contacts: "If you allow access, your contacts are continuously synced with Telegram. Thanks to this, you can easily switch to Telegram and immediately connect with friends across all your devices. We use data about your contacts to let you know when they join Telegram, and to display them by the name you set for them in your phone.\n\nYou can disable contact syncing or delete your stored contacts in Settings > Privacy & Security on Telegram's mobile apps.".to_string(),
            about_frequent: "This rating shows which people you are likelier to message frequently. Telegram uses this data to populate the 'People' box at the top of the Search section. This rating is also calculated for inline bots so that the app can suggest the bots you are most likely to use in the attachment menu (or when you start a new message with \"@\").\n\nTo delete this data, go to Settings > Privacy & Security and disable 'Suggest Frequent Contacts' (requires Telegram for iOS v.4.8.3 or Telegram for Android v.4.8.10 or higher).".to_string(),
            about_sessions: "We store session info to display your connected devices in Settings > Privacy & Security > Active Sessions.".to_string(),
            about_web_sessions: "We store this to display the websites where you logged in using authentication via Telegram. This information is shown in Settings > Privacy & Security > Active Sessions.".to_string(),
            about_chats: "This page lists all chats from this export.".to_string(),
            about_left_chats: "Below are the supergroups and channels from this export that you've left or where you were banned.\n\nNote that when you leave a channel or supergroup you've created, you have the option to either delete it, or simply leave (in case you want to rejoin later, or keep the community alive despite not being a member).".to_string(),
            utc_offset_seconds: 0,
        }
    }
}
