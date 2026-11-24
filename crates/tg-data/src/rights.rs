//! Port of `data/data_chat_participant_status.h` and `data/data_chat_participant_status.cpp`.
//!
//! Defines `ChatAdminRights` and `ChatBannedRights` bitflags with the exact bit
//! positions from Telegram API layer 233 (`chatAdminRights#5fb224d5` and
//! `chatBannedRights#9f120418`), restriction check results, and `CanSendAnyOf` /
//! `CanSendAnything` / `CanWrite` logic.

use bitflags::bitflags;

use crate::id::TimeId;

bitflags! {
    /// Administrator rights bitflags with exact Telegram API bit positions
    /// (`chatAdminRights#5fb224d5` in `schema/api.tl`).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct ChatAdminRights: u32 {
        /// Bit 0: `change_info`.
        const CHANGE_INFO             = 1 << 0;
        /// Bit 1: `post_messages`.
        const POST_MESSAGES           = 1 << 1;
        /// Bit 2: `edit_messages`.
        const EDIT_MESSAGES           = 1 << 2;
        /// Bit 3: `delete_messages`.
        const DELETE_MESSAGES         = 1 << 3;
        /// Bit 4: `ban_users`.
        const BAN_USERS               = 1 << 4;
        /// Bit 5: `invite_users` (tdesktop `InviteByLinkOrAdd`).
        const INVITE_USERS            = 1 << 5;
        /// Bit 7: `pin_messages`.
        const PIN_MESSAGES            = 1 << 7;
        /// Bit 9: `add_admins`.
        const ADD_ADMINS              = 1 << 9;
        /// Bit 10: `anonymous`.
        const ANONYMOUS               = 1 << 10;
        /// Bit 11: `manage_call`.
        const MANAGE_CALL             = 1 << 11;
        /// Bit 12: `other`.
        const OTHER                   = 1 << 12;
        /// Bit 13: `manage_topics`.
        const MANAGE_TOPICS           = 1 << 13;
        /// Bit 14: `post_stories`.
        const POST_STORIES            = 1 << 14;
        /// Bit 15: `edit_stories`.
        const EDIT_STORIES            = 1 << 15;
        /// Bit 16: `delete_stories`.
        const DELETE_STORIES          = 1 << 16;
        /// Bit 17: `manage_direct_messages` (tdesktop `ManageDirect`).
        const MANAGE_DIRECT_MESSAGES  = 1 << 17;
        /// Bit 18: `manage_ranks`.
        const MANAGE_RANKS            = 1 << 18;
        /// Bit 19: `manage_linked_peers`.
        const MANAGE_LINKED_PEERS     = 1 << 19;
        /// Bit 20: `manage_welcome_messages`.
        const MANAGE_WELCOME_MESSAGES = 1 << 20;
    }
}

bitflags! {
    /// Banned / restricted rights bitflags with exact Telegram API bit positions
    /// (`chatBannedRights#9f120418` in `schema/api.tl`).
    ///
    /// A set bit means the corresponding action is **forbidden** (restricted).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct ChatBannedRights: u32 {
        /// Bit 0: `view_messages` (kicked / cannot view messages).
        const VIEW_MESSAGES       = 1 << 0;
        /// Bit 1: `send_messages` (legacy API bit).
        const SEND_MESSAGES       = 1 << 1;
        /// Bit 2: `send_media` (legacy API bit).
        const SEND_MEDIA          = 1 << 2;
        /// Bit 3: `send_stickers`.
        const SEND_STICKERS       = 1 << 3;
        /// Bit 4: `send_gifs`.
        const SEND_GIFS           = 1 << 4;
        /// Bit 5: `send_games`.
        const SEND_GAMES          = 1 << 5;
        /// Bit 6: `send_inline`.
        const SEND_INLINE         = 1 << 6;
        /// Bit 7: `embed_links`.
        const EMBED_LINKS         = 1 << 7;
        /// Bit 8: `send_polls`.
        const SEND_POLLS          = 1 << 8;
        /// Bit 10: `change_info`.
        const CHANGE_INFO         = 1 << 10;
        /// Bit 15: `invite_users` (tdesktop `AddParticipants`).
        const INVITE_USERS        = 1 << 15;
        /// Bit 17: `pin_messages`.
        const PIN_MESSAGES        = 1 << 17;
        /// Bit 18: `manage_topics` (tdesktop `CreateTopics`).
        const MANAGE_TOPICS       = 1 << 18;
        /// Bit 19: `send_photos`.
        const SEND_PHOTOS         = 1 << 19;
        /// Bit 20: `send_videos`.
        const SEND_VIDEOS         = 1 << 20;
        /// Bit 21: `send_roundvideos` (tdesktop `SendVideoMessages`).
        const SEND_ROUNDVIDEOS    = 1 << 21;
        /// Bit 22: `send_audios` (tdesktop `SendMusic`).
        const SEND_AUDIOS         = 1 << 22;
        /// Bit 23: `send_voices` (tdesktop `SendVoiceMessages`).
        const SEND_VOICES         = 1 << 23;
        /// Bit 24: `send_docs` (tdesktop `SendFiles`).
        const SEND_DOCS           = 1 << 24;
        /// Bit 25: `send_plain` (tdesktop `SendOther`).
        const SEND_PLAIN          = 1 << 25;
        /// Bit 26: `edit_rank`.
        const EDIT_RANK           = 1 << 26;
        /// Bit 27: `send_reactions`.
        const SEND_REACTIONS      = 1 << 27;
        /// Bit 28: `manage_linked_peers`.
        const MANAGE_LINKED_PEERS = 1 << 28;
    }
}

/// Alias matching tdesktop's `ChatRestrictions`.
pub type ChatRestrictions = ChatBannedRights;

/// Sentinel timestamp indicating permanent restriction (`ChannelData::kRestrictUntilForever`).
pub const RESTRICT_UNTIL_FOREVER: TimeId = i32::MAX;

/// Returns `true` if a restriction expiry timestamp means "forever"
/// (`ChannelData::IsRestrictedForever`).
#[inline]
pub const fn is_restricted_forever(until: TimeId) -> bool {
    until == 0 || until == RESTRICT_UNTIL_FOREVER
}

impl ChatBannedRights {
    /// Combined mask of all 12 send-type restrictions (`Data::AllSendRestrictions`).
    pub const ALL_SEND: Self = Self::SEND_PLAIN
        .union(Self::SEND_STICKERS)
        .union(Self::SEND_GIFS)
        .union(Self::SEND_GAMES)
        .union(Self::SEND_INLINE)
        .union(Self::SEND_POLLS)
        .union(Self::SEND_PHOTOS)
        .union(Self::SEND_VIDEOS)
        .union(Self::SEND_ROUNDVIDEOS)
        .union(Self::SEND_AUDIOS)
        .union(Self::SEND_VOICES)
        .union(Self::SEND_DOCS);

    /// Combined mask of file-attachment send restrictions (`Data::FilesSendRestrictions`).
    pub const FILES_SEND: Self = Self::SEND_STICKERS
        .union(Self::SEND_GIFS)
        .union(Self::SEND_PHOTOS)
        .union(Self::SEND_VIDEOS)
        .union(Self::SEND_AUDIOS)
        .union(Self::SEND_DOCS);

    /// Combined mask of tabbed-panel send restrictions (`Data::TabbedPanelSendRestrictions`).
    pub const TABBED_PANEL_SEND: Self = Self::SEND_STICKERS
        .union(Self::SEND_GIFS)
        .union(Self::SEND_PLAIN);

    /// Restriction flags applied when kicking a user (`ChannelData::KickedRestrictedRights`).
    pub const KICKED_USER: Self = Self::VIEW_MESSAGES
        .union(Self::SEND_STICKERS)
        .union(Self::SEND_GIFS)
        .union(Self::SEND_GAMES)
        .union(Self::SEND_INLINE)
        .union(Self::SEND_PHOTOS)
        .union(Self::SEND_VIDEOS)
        .union(Self::SEND_ROUNDVIDEOS)
        .union(Self::SEND_AUDIOS)
        .union(Self::SEND_VOICES)
        .union(Self::SEND_DOCS)
        .union(Self::SEND_PLAIN)
        .union(Self::EMBED_LINKS);
}

/// Pair of banned rights flags and expiration timestamp (`ChatRestrictionsInfo`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct ChatRestrictionsInfo {
    /// Active restriction bitflags.
    pub flags: ChatBannedRights,
    /// Expiration timestamp (`0` or `RESTRICT_UNTIL_FOREVER` for permanent).
    pub until: TimeId,
}

impl ChatRestrictionsInfo {
    /// Creates a new `ChatRestrictionsInfo`.
    #[inline]
    pub const fn new(flags: ChatBannedRights, until: TimeId) -> Self {
        Self { flags, until }
    }

    /// Returns `KickedRestrictedRights` for a user or non-user peer.
    #[inline]
    pub const fn kicked(is_user: bool) -> Self {
        Self {
            flags: if is_user {
                ChatBannedRights::KICKED_USER
            } else {
                ChatBannedRights::VIEW_MESSAGES
            },
            until: RESTRICT_UNTIL_FOREVER,
        }
    }

    /// Returns `true` if any restriction flag is set and not expired at `now`.
    #[inline]
    pub const fn is_active(&self, now: TimeId) -> bool {
        !self.flags.is_empty() && (is_restricted_forever(self.until) || self.until > now)
    }
}

/// Result of checking whether the current user is restricted from an action
/// (`Data::RestrictionCheckResult`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum RestrictionCheckResult {
    /// Action is allowed.
    #[default]
    Allowed,
    /// Action is restricted for all non-admin members (default chat restrictions).
    WithEveryone,
    /// Action is explicitly restricted for this account.
    Explicit,
}

impl RestrictionCheckResult {
    /// Returns `true` if the action is allowed.
    #[inline]
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }

    /// Returns `true` if the action is restricted (either `WithEveryone` or `Explicit`).
    #[inline]
    pub const fn is_restricted(self) -> bool {
        !self.is_allowed()
    }

    /// Returns `true` if restricted via group-wide default restrictions.
    #[inline]
    pub const fn is_with_everyone(self) -> bool {
        matches!(self, Self::WithEveryone)
    }

    /// Returns `true` if explicitly restricted for the current user.
    #[inline]
    pub const fn is_explicit(self) -> bool {
        matches!(self, Self::Explicit)
    }
}

/// Account-level context required by peer permission checks (`CanSendAnyOf`, `amRestricted`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct SendCheckContext {
    /// Whether the current Telegram account is frozen.
    pub account_frozen: bool,
    /// Whether the current Telegram account has Telegram Premium.
    pub account_premium: bool,
    /// Whether the session is connected to the Telegram test DC environment.
    pub is_test_environment: bool,
}
