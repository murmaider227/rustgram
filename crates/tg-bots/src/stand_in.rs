//! Stand-in domain primitives matching `tg-data` contracts in `docs/ARCHITECTURE.md`.
//!
//! Because `tg-tl` and `tg-data` are implemented in parallel, `tg-bots` defines
//! lightweight plain types here so `tg-api` can convert without GUI or TL coupling.

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

/// Unique peer identifier, matching `tg-data`'s `PeerId(i64)` contract.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct PeerId(pub i64);

/// Unique user identifier.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct UserId(pub u64);

/// Unique basic-group chat identifier.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct ChatId(pub u64);

/// Unique channel/supergroup identifier.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct ChannelId(pub u64);

/// Message identifier within a peer history, matching `tg-data`'s `MsgId(i32)`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct MsgId(pub i32);

/// Full message identifier (peer + message id), matching `tg-data`'s `FullMsgId`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct FullMsgId {
    /// Owning peer.
    pub peer: PeerId,
    /// Message id inside the peer's history.
    pub msg: MsgId,
}

impl FullMsgId {
    /// Creates a new [`FullMsgId`].
    pub const fn new(peer: PeerId, msg: MsgId) -> Self {
        Self { peer, msg }
    }

    /// Returns `true` when both peer and message id are non-zero.
    pub const fn is_valid(self) -> bool {
        self.peer.0 != 0 && self.msg.0 != 0
    }
}

/// Unique document identifier.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct DocumentId(pub u64);

/// Unique photo identifier.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct PhotoId(pub u64);

/// Unique game identifier.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct GameId(pub u64);

/// Unique bot Mini App identifier.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct BotAppId(pub u64);

/// Coarse classification of a Telegram peer used by bot filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PeerKind {
    /// Regular human user chat.
    User,
    /// Bot chat.
    Bot,
    /// Basic group or supergroup (megagroup).
    Group,
    /// Broadcast channel.
    Broadcast,
}

/// Plain RGBA color value (0..=255 per channel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Rgba {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
    /// Alpha channel (`255` = opaque, `0` = transparent).
    pub a: u8,
}

impl Rgba {
    /// Transparent black (`#00000000`).
    pub const TRANSPARENT: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };

    /// Creates an opaque RGB color (`a = 255`).
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Creates an RGBA color.
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Parses a 24-bit serialized RGB integer (`0x00RRGGBB`), matching
    /// tdesktop's `Ui::MaybeColorFromSerialized`.
    pub const fn from_serialized_rgb(value: u32) -> Self {
        Self {
            r: ((value >> 16) & 0xFF) as u8,
            g: ((value >> 8) & 0xFF) as u8,
            b: (value & 0xFF) as u8,
            a: 255,
        }
    }

    /// Formats the RGB channels as lowercase `#rrggbb`, matching
    /// `Window::Theme::WebViewParams()`.
    pub fn to_hex_rgb(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Returns `true` if the color is completely opaque (`a == 255`).
    pub const fn is_opaque(self) -> bool {
        self.a == 255
    }
}

bitflags! {
    /// Admin right flags matching `data/data_chat_participant_status.h`'s `ChatAdminRight`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
    pub struct ChatAdminRights: u32 {
        /// Change chat title, photo, and other settings.
        const CHANGE_INFO = 1 << 0;
        /// Post messages in a channel.
        const POST_MESSAGES = 1 << 1;
        /// Edit messages of others in a channel.
        const EDIT_MESSAGES = 1 << 2;
        /// Delete messages of others.
        const DELETE_MESSAGES = 1 << 3;
        /// Ban / restrict members.
        const BAN_USERS = 1 << 4;
        /// Invite users via link or add members.
        const INVITE_BY_LINK_OR_ADD = 1 << 5;
        /// Pin messages.
        const PIN_MESSAGES = 1 << 7;
        /// Promote new admins.
        const ADD_ADMINS = 1 << 9;
        /// Remain anonymous.
        const ANONYMOUS = 1 << 10;
        /// Manage voice/video chats.
        const MANAGE_CALL = 1 << 11;
        /// General chat management (`manage_chat`).
        const OTHER = 1 << 12;
        /// Manage forum topics.
        const MANAGE_TOPICS = 1 << 13;
        /// Post stories.
        const POST_STORIES = 1 << 14;
        /// Edit stories.
        const EDIT_STORIES = 1 << 15;
        /// Delete stories.
        const DELETE_STORIES = 1 << 16;
        /// Manage channel direct messages.
        const MANAGE_DIRECT = 1 << 17;
        /// Manage custom ranks.
        const MANAGE_RANKS = 1 << 18;
        /// Process join requests.
        const PROCESS_JOIN_REQUESTS = 1 << 19;
        /// Manage linked peers.
        const MANAGE_LINKED_PEERS = 1 << 20;
        /// Manage welcome messages.
        const MANAGE_WELCOME_MESSAGES = 1 << 21;
    }
}
