//! Stand-in domain types matching the `tg-data` contract in `docs/ARCHITECTURE.md`.
//!
//! Port origin: `Telegram/SourceFiles/data/data_peer_id.h`,
//! `Telegram/SourceFiles/dialogs/dialogs_key.h`, and `Telegram/SourceFiles/mtproto/facade.h`.
//! Swap these with `tg_data` and `tg_api` once those crates are integrated.

use serde::{Deserialize, Serialize};

/// Bit-shift encoding for peer kinds, matching `tdesktop`'s `data_peer_id.h`.
const CHAT_TYPE_MASK: u64 = 0xFFFF_FFFF_FFFF;
const SHIFT_USER: u64 = 0;
const SHIFT_CHAT: u64 = 1;
const SHIFT_CHANNEL: u64 = 2;

/// Unique peer identifier using `tdesktop` bit-shifted encoding.
///
/// Mirrors the `tg-data` cross-crate contract (`PeerId(i64)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct PeerId(pub i64);

impl PeerId {
    /// Empty / unset peer identifier (`PeerId(0)`).
    pub const ZERO: Self = Self(0);

    /// Encodes a bare user id (`Shift = 0`).
    #[must_use]
    pub const fn from_user(bare_id: u64) -> Self {
        Self(((bare_id & CHAT_TYPE_MASK) | (SHIFT_USER << 48)) as i64)
    }

    /// Encodes a bare basic group chat id (`Shift = 1`).
    #[must_use]
    pub const fn from_chat(bare_id: u64) -> Self {
        Self(((bare_id & CHAT_TYPE_MASK) | (SHIFT_CHAT << 48)) as i64)
    }

    /// Encodes a bare channel / supergroup id (`Shift = 2`).
    #[must_use]
    pub const fn from_channel(bare_id: u64) -> Self {
        Self(((bare_id & CHAT_TYPE_MASK) | (SHIFT_CHANNEL << 48)) as i64)
    }

    /// Returns `true` if this id is unset (`0`).
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Returns the kind of peer encoded in the upper bits.
    #[must_use]
    pub const fn kind(self) -> PeerKind {
        let shift = ((self.0 as u64) >> 48) & 0x7F;
        match shift {
            SHIFT_CHAT => PeerKind::Group,
            SHIFT_CHANNEL => PeerKind::Channel,
            _ => PeerKind::User,
        }
    }

    /// Returns the bare 48-bit numeric identifier.
    #[must_use]
    pub const fn bare(self) -> u64 {
        (self.0 as u64) & CHAT_TYPE_MASK
    }
}

/// Classification of a Telegram peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PeerKind {
    /// Individual user or Saved Messages.
    User,
    /// Legacy chat or supergroup.
    Group,
    /// Broadcast channel.
    Channel,
}

/// Message identifier within a peer history.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct MsgId(pub i32);

impl MsgId {
    /// Unset message id; in tdesktop means show at unread / end.
    pub const SHOW_AT_UNREAD: Self = Self(0);
    /// Sentinel to jump to the very bottom of a history.
    pub const SHOW_AT_END: Self = Self(i32::MAX);
}

/// Globally unique message reference `(peer, msg)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct FullMsgId {
    /// Owning peer.
    pub peer: PeerId,
    /// Message id within that peer's history.
    pub msg: MsgId,
}

/// Folder / chat filter identifier (`0` is "All Chats").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct FilterId(pub i32);

impl FilterId {
    /// Default unfiltered "All Chats" folder.
    pub const ALL: Self = Self(0);
}

/// Chat folder metadata for the left filters sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderFilter {
    /// Filter identifier (`FilterId::ALL` for all chats).
    pub id: FilterId,
    /// Short display title (e.g. "All Chats", "Personal", "Work").
    pub title: String,
    /// Short ASCII/Unicode badge glyph for the sidebar button.
    pub icon_glyph: String,
    /// Total unread messages in this folder.
    pub unread_count: u32,
}

/// Authenticated account profile shown in the main menu and settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthUser {
    /// Self peer id.
    pub id: PeerId,
    /// Full display name.
    pub name: String,
    /// Public username without `@`.
    pub username: String,
    /// Synthetic phone number for display.
    pub phone: String,
    /// Short bio line.
    pub bio: String,
    /// Two-letter avatar initials.
    pub initials: String,
    /// RGB color accent for the fallback avatar circle.
    pub avatar_rgb: [u8; 3],
}

/// Detailed peer info displayed in the history header and third info column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerSummary {
    /// Peer identifier.
    pub id: PeerId,
    /// Peer classification.
    pub kind: PeerKind,
    /// Display title.
    pub title: String,
    /// Subtitle (e.g. "online", "last seen recently", "42 members").
    pub status: String,
    /// Optional public handle without `@`.
    pub username: Option<String>,
    /// Optional synthetic phone or invite info.
    pub phone_or_link: Option<String>,
    /// Bio or channel description.
    pub about: String,
    /// Two-letter avatar initials.
    pub initials: String,
    /// RGB accent color for the avatar circle.
    pub avatar_rgb: [u8; 3],
    /// Whether notifications are muted for this peer.
    pub muted: bool,
    /// Whether the current user can compose messages in this peer.
    pub can_send: bool,
    /// Whether this peer represents Saved Messages.
    pub is_saved_messages: bool,
}

/// Single row in the dialogs (chats) list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogEntry {
    /// Peer summary for this conversation.
    pub peer: PeerSummary,
    /// Preview text of the latest message.
    pub last_message: String,
    /// Optional sender prefix for group chats.
    pub last_sender: Option<String>,
    /// Formatted timestamp string (e.g. "14:22", "Yesterday").
    pub timestamp: String,
    /// Number of unread messages in this chat.
    pub unread_count: u32,
    /// Whether this chat is pinned at the top.
    pub pinned: bool,
    /// Folder filters that include this chat (beyond `FilterId::ALL`).
    pub folders: Vec<FilterId>,
}

/// Single message bubble in a peer's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageItem {
    /// Message identifier.
    pub id: MsgId,
    /// Owning peer identifier.
    pub peer_id: PeerId,
    /// Sender display name.
    pub sender_name: String,
    /// `true` if sent by the current account.
    pub outgoing: bool,
    /// Plain message body.
    pub text: String,
    /// Formatted time label (e.g. "14:22").
    pub timestamp: String,
    /// Whether an outgoing message has been read by the recipient.
    pub is_read: bool,
}

/// Raw MTProto connection state fed into `window::connecting_widget`.
///
/// Mirrors `MTP::dcstate()` semantics from `window_connecting_widget.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// Connected to the active data center.
    Connected,
    /// Actively establishing or re-establishing a transport connection.
    Connecting,
    /// Waiting `remaining_ms` milliseconds before retrying connection.
    Waiting {
        /// Milliseconds remaining until the next reconnect attempt.
        remaining_ms: u32,
    },
}

/// Result returned when submitting a phone number during intro.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeSentInfo {
    /// Normalized display phone string.
    pub phone: String,
    /// Expected verification code length (typically 5).
    pub code_length: usize,
    /// Hint shown in the mock UI so developers know what test code works.
    pub delivery_hint: String,
}

/// Outcome of verifying a login code during intro.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeVerifyOutcome {
    /// Login succeeded without 2FA.
    SignedIn(AuthUser),
    /// Cloud 2FA password is required next.
    PasswordRequired {
        /// Password hint configured on the account.
        hint: String,
    },
}
