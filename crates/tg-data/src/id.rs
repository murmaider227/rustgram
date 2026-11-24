//! Port of `data/data_peer_id.h`, `data/data_peer_id.cpp`, `data/data_msg_id.h`,
//! and `data/data_types.h`.
//!
//! Defines Telegram Desktop's strongly-typed peer, message, media, and time IDs
//! with the exact bit encodings, ranges, special constants, and serialization
//! helpers from the C++ reference implementation.

use std::fmt;

/// Bare 64-bit unsigned identifier used inside shifted peer IDs.
pub type BareId = u64;

/// Unix timestamp in seconds (32-bit signed, matching Telegram API `int`).
pub type TimeId = i32;

/// Dialog folder identifier (`0` = main list, `1` = archive).
pub type FolderId = i32;

/// Chat filter (custom folder) identifier (`0` = "All Chats").
pub type FilterId = i32;

/// Story identifier within a peer.
pub type StoryId = i32;

/// Business quick-reply shortcut identifier.
pub type BusinessShortcutId = i32;

/// Archive folder constant (`Data::Folder::kId`).
pub const ARCHIVE_FOLDER_ID: FolderId = 1;

/// Number of available peer accent color indices (`Ui::kColorIndexCount`).
pub const COLOR_INDEX_COUNT: u8 = 1 << 6;

/// Number of simple (default) peer color indices (`Ui::kSimpleColorIndexCount`).
pub const SIMPLE_COLOR_INDEX_COUNT: u8 = 7;

/// Sentinel `PhotoId` indicating that a peer's profile photo ID is not yet known.
pub const UNKNOWN_PHOTO_ID: PhotoId = PhotoId(0xFFFF_FFFF_FFFF_FFFF);

/// Sentinel `WebPageId` indicating a cancelled webpage preview.
pub const CANCELLED_WEB_PAGE_ID: WebPageId = WebPageId(0xFFFF_FFFF_FFFF_FFFF);

/// Minimum `TimeId` used in `MessagePosition`.
pub const MIN_DATE: TimeId = 0;

/// Maximum `TimeId` used in `MessagePosition`.
pub const MAX_DATE: TimeId = i32::MAX;

macro_rules! define_u64_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
        pub struct $name(pub u64);

        impl $name {
            /// Zero / unset identifier.
            pub const ZERO: Self = Self(0);

            /// Returns the underlying raw 64-bit value.
            #[inline]
            pub const fn raw(self) -> u64 {
                self.0
            }

            /// Returns `true` if this identifier is zero (unset).
            #[inline]
            pub const fn is_zero(self) -> bool {
                self.0 == 0
            }

            /// Constructs an identifier from a signed 64-bit TL `long`.
            #[inline]
            pub const fn from_tl(value: i64) -> Self {
                Self(value as u64)
            }

            /// Converts this identifier to a signed 64-bit TL `long`.
            #[inline]
            pub const fn to_tl(self) -> i64 {
                self.0 as i64
            }
        }

        impl From<u64> for $name {
            #[inline]
            fn from(value: u64) -> Self {
                Self(value)
            }
        }

        impl From<$name> for u64 {
            #[inline]
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

define_u64_id!(
    /// Bare user identifier (`UserId` = `ChatIdType<0>`).
    UserId
);
define_u64_id!(
    /// Bare basic group chat identifier (`ChatId` = `ChatIdType<1>`).
    ChatId
);
define_u64_id!(
    /// Bare channel / supergroup identifier (`ChannelId` = `ChatIdType<2>`).
    ChannelId
);
define_u64_id!(
    /// Bare fake/synthetic chat identifier (`FakeChatId` = `ChatIdType<0x7F>`).
    FakeChatId
);
define_u64_id!(
    /// Cloud photo identifier (`PhotoId`).
    PhotoId
);
define_u64_id!(
    /// Cloud document identifier (`DocumentId`).
    DocumentId
);
define_u64_id!(
    /// Webpage preview identifier (`WebPageId`).
    WebPageId
);
define_u64_id!(
    /// Bot game identifier (`GameId`).
    GameId
);
define_u64_id!(
    /// Poll identifier (`PollId`).
    PollId
);
define_u64_id!(
    /// Wallpaper identifier (`WallPaperId`).
    WallPaperId
);
define_u64_id!(
    /// Call / group call identifier (`CallId`).
    CallId
);
define_u64_id!(
    /// Bot mini-app identifier (`BotAppId`).
    BotAppId
);
define_u64_id!(
    /// Message effect identifier (`EffectId`).
    EffectId
);
define_u64_id!(
    /// Collectible color/gift identifier (`CollectibleId`).
    CollectibleId
);

/// Unified peer identifier using Telegram Desktop's exact bit layout.
///
/// Layout (stored in `i64` per `docs/ARCHITECTURE.md`; bit 63 is always 0):
/// - Bits `0..48`: bare ID (`CHAT_TYPE_MASK = 0xFFFF_FFFF_FFFF`)
/// - Bits `48..55`: type tag (`0` = user, `1` = chat, `2` = channel, `0x7F` = fake)
/// - Bit `55`: reserved (`RESERVED_BIT = 0x80`, set only in serialized form)
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct PeerId(pub i64);

impl PeerId {
    /// Zero / unset peer ID.
    pub const ZERO: Self = Self(0);

    /// Low 48-bit mask isolating the bare identifier (`PeerId::kChatTypeMask`).
    pub const CHAT_TYPE_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

    /// Type shift for `UserId` (`ChatIdType<0>::kShift`).
    pub const USER_SHIFT: u8 = 0;
    /// Type shift for `ChatId` (`ChatIdType<1>::kShift`).
    pub const CHAT_SHIFT: u8 = 1;
    /// Type shift for `ChannelId` (`ChatIdType<2>::kShift`).
    pub const CHANNEL_SHIFT: u8 = 2;
    /// Type shift for `FakeChatId` (`ChatIdType<0x7F>::kShift`).
    pub const FAKE_CHAT_SHIFT: u8 = 0x7F;
    /// Reserved bit in the shift byte used by `SerializePeerId` (`kReservedBit`).
    pub const RESERVED_BIT: u8 = 0x80;

    /// Telegram service notifications user (`777000`).
    pub const SERVICE_NOTIFICATIONS: Self = Self::from_user(UserId(777_000));
    /// Legacy Telegram notifications user (`333000`).
    pub const LEGACY_NOTIFICATIONS: Self = Self::from_user(UserId(333_000));
    /// Hidden author placeholder in Saved Messages (`2666000`).
    pub const SAVED_HIDDEN_AUTHOR: Self = Self::from_user(UserId(2_666_000));
    /// Verification codes service user (`489000`).
    pub const VERIFY_CODES: Self = Self::from_user(UserId(489_000));
    /// Production Replies chat user (`1271266957`).
    pub const REPLIES_CHAT_PRODUCTION: Self = Self::from_user(UserId(1_271_266_957));
    /// Test-environment Replies chat user (`708513`).
    pub const REPLIES_CHAT_TEST: Self = Self::from_user(UserId(708_513));

    /// Constructs a `PeerId` from a raw `u64` value matching tdesktop's `PeerId::value`.
    #[inline]
    pub const fn from_raw(value: u64) -> Self {
        Self(value as i64)
    }

    /// Returns the raw `u64` representation matching tdesktop's `PeerId::value`.
    #[inline]
    pub const fn raw(self) -> u64 {
        self.0 as u64
    }

    /// Encodes a `UserId` into a `PeerId`.
    #[inline]
    pub const fn from_user(id: UserId) -> Self {
        Self::from_shifted(Self::USER_SHIFT, id.0)
    }

    /// Encodes a `ChatId` into a `PeerId`.
    #[inline]
    pub const fn from_chat(id: ChatId) -> Self {
        Self::from_shifted(Self::CHAT_SHIFT, id.0)
    }

    /// Encodes a `ChannelId` into a `PeerId`.
    #[inline]
    pub const fn from_channel(id: ChannelId) -> Self {
        Self::from_shifted(Self::CHANNEL_SHIFT, id.0)
    }

    /// Encodes a `FakeChatId` into a `PeerId`.
    #[inline]
    pub const fn from_fake(id: FakeChatId) -> Self {
        Self::from_shifted(Self::FAKE_CHAT_SHIFT, id.0)
    }

    #[inline]
    const fn from_shifted(shift: u8, bare: u64) -> Self {
        Self((bare | ((shift as u64) << 48)) as i64)
    }

    /// Returns the 8-bit type shift field `(value >> 48) & 0xFF`.
    #[inline]
    pub const fn type_shift(self) -> u8 {
        ((self.raw() >> 48) & 0xFF) as u8
    }

    /// Returns the low 48-bit bare ID (`value & CHAT_TYPE_MASK`).
    #[inline]
    pub const fn bare(self) -> BareId {
        self.raw() & Self::CHAT_TYPE_MASK
    }

    /// Returns `true` if `self == PeerId::ZERO`.
    #[inline]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Returns `true` if this peer ID has the `UserId` shift (`0`).
    ///
    /// Note: matches tdesktop's `peerIsUser(PeerId(0)) == true`.
    #[inline]
    pub const fn is_user(self) -> bool {
        self.type_shift() == Self::USER_SHIFT
    }

    /// Returns `true` if this peer ID has the `ChatId` shift (`1`).
    #[inline]
    pub const fn is_chat(self) -> bool {
        self.type_shift() == Self::CHAT_SHIFT
    }

    /// Returns `true` if this peer ID has the `ChannelId` shift (`2`).
    #[inline]
    pub const fn is_channel(self) -> bool {
        self.type_shift() == Self::CHANNEL_SHIFT
    }

    /// Returns `true` if this peer ID has the `FakeChatId` shift (`0x7F`).
    #[inline]
    pub const fn is_fake(self) -> bool {
        self.type_shift() == Self::FAKE_CHAT_SHIFT
    }

    /// Extracts the `UserId` if this is a user peer ID.
    #[inline]
    pub const fn to_user(self) -> Option<UserId> {
        if self.is_user() {
            Some(UserId(self.bare()))
        } else {
            None
        }
    }

    /// Extracts the `UserId`, or `UserId::ZERO` if not a user (matches C++ `peerToUser`).
    #[inline]
    pub const fn to_user_or_zero(self) -> UserId {
        if self.is_user() {
            UserId(self.bare())
        } else {
            UserId::ZERO
        }
    }

    /// Extracts the `ChatId` if this is a basic chat peer ID.
    #[inline]
    pub const fn to_chat(self) -> Option<ChatId> {
        if self.is_chat() {
            Some(ChatId(self.bare()))
        } else {
            None
        }
    }

    /// Extracts the `ChatId`, or `ChatId::ZERO` if not a chat (matches C++ `peerToChat`).
    #[inline]
    pub const fn to_chat_or_zero(self) -> ChatId {
        if self.is_chat() {
            ChatId(self.bare())
        } else {
            ChatId::ZERO
        }
    }

    /// Extracts the `ChannelId` if this is a channel peer ID.
    #[inline]
    pub const fn to_channel(self) -> Option<ChannelId> {
        if self.is_channel() {
            Some(ChannelId(self.bare()))
        } else {
            None
        }
    }

    /// Extracts the `ChannelId`, or `ChannelId::ZERO` if not a channel (matches C++ `peerToChannel`).
    #[inline]
    pub const fn to_channel_or_zero(self) -> ChannelId {
        if self.is_channel() {
            ChannelId(self.bare())
        } else {
            ChannelId::ZERO
        }
    }

    /// Extracts the `FakeChatId` if this is a fake peer ID.
    #[inline]
    pub const fn to_fake(self) -> Option<FakeChatId> {
        if self.is_fake() {
            Some(FakeChatId(self.bare()))
        } else {
            None
        }
    }

    /// Serializes this `PeerId` into tdesktop's modern on-disk `uint64` format
    /// (`SerializePeerId` in `data_peer_id.cpp`), setting bit 55 (`RESERVED_BIT << 48`).
    ///
    /// Returns `None` if the reserved bit is already set.
    #[inline]
    pub const fn serialize(self) -> Option<u64> {
        let flag = (Self::RESERVED_BIT as u64) << 48;
        let raw = self.raw();
        if (raw & flag) != 0 {
            None
        } else {
            Some(raw | flag)
        }
    }

    /// Deserializes a `PeerId` from either tdesktop's modern or legacy `uint64`
    /// storage format (`DeserializePeerId` in `data_peer_id.cpp`).
    #[inline]
    pub const fn deserialize(serialized: u64) -> Self {
        let flag = (Self::RESERVED_BIT as u64) << 48;
        if (serialized & flag) != 0 {
            return Self::from_raw(serialized & !flag);
        }
        const LEGACY_MASK: u64 = 0xFFFF_FFFF;
        const LEGACY_TYPE_MASK: u64 = 0xF_0000_0000;
        const LEGACY_USER_SHIFT: u64 = 0x0_0000_0000;
        const LEGACY_CHAT_SHIFT: u64 = 0x1_0000_0000;
        const LEGACY_CHANNEL_SHIFT: u64 = 0x2_0000_0000;
        const LEGACY_FAKE_SHIFT: u64 = 0xF_0000_0000;

        let bare = serialized & LEGACY_MASK;
        match serialized & LEGACY_TYPE_MASK {
            LEGACY_USER_SHIFT => Self::from_user(UserId(bare)),
            LEGACY_CHAT_SHIFT => Self::from_chat(ChatId(bare)),
            LEGACY_CHANNEL_SHIFT => Self::from_channel(ChannelId(bare)),
            LEGACY_FAKE_SHIFT => Self::from_fake(FakeChatId(bare)),
            _ => Self::ZERO,
        }
    }

    /// Converts this `PeerId` to a Telegram Bot API / TDLib-style signed chat ID.
    ///
    /// - User `u` (non-zero, `<= 0x0000_FFFF_FFFF_FFFF`): `+u`
    /// - Chat `c` (`1..=999_999_999_999`): `-c`
    /// - Channel `ch` (`1..`): `-1_000_000_000_000 - ch`
    /// - Zero or fake peer IDs return `None`.
    pub const fn to_bot_api_id(self) -> Option<i64> {
        const CHANNEL_OFFSET: i64 = 1_000_000_000_000;
        let bare = self.bare();
        if bare == 0 {
            return None;
        }
        if self.is_user() {
            if bare <= i64::MAX as u64 {
                Some(bare as i64)
            } else {
                None
            }
        } else if self.is_chat() {
            if bare < CHANNEL_OFFSET as u64 {
                Some(-(bare as i64))
            } else {
                None
            }
        } else if self.is_channel() {
            match (bare as i64).checked_add(CHANNEL_OFFSET) {
                Some(v) => v.checked_neg(),
                None => None,
            }
        } else {
            None
        }
    }

    /// Parses a Telegram Bot API / TDLib-style signed chat ID into a `PeerId`.
    ///
    /// - `id > 0` -> `UserId(id)`
    /// - `-999_999_999_999 <= id <= -1` -> `ChatId(-id)`
    /// - `id <= -1_000_000_000_001` -> `ChannelId(-1_000_000_000_000 - id)`
    /// - `0` or `-1_000_000_000_000` -> `None`
    pub const fn from_bot_api_id(id: i64) -> Option<Self> {
        const CHANNEL_OFFSET: i64 = 1_000_000_000_000;
        if id > 0 {
            let bare = id as u64;
            if bare <= Self::CHAT_TYPE_MASK {
                Some(Self::from_user(UserId(bare)))
            } else {
                None
            }
        } else if id < 0 && id > -CHANNEL_OFFSET {
            Some(Self::from_chat(ChatId((-id) as u64)))
        } else if id < -CHANNEL_OFFSET {
            let Some(pos) = id.checked_neg() else {
                return None;
            };
            let bare = (pos - CHANNEL_OFFSET) as u64;
            if bare <= Self::CHAT_TYPE_MASK {
                Some(Self::from_channel(ChannelId(bare)))
            } else {
                None
            }
        } else {
            None
        }
    }
}

impl From<UserId> for PeerId {
    #[inline]
    fn from(id: UserId) -> Self {
        Self::from_user(id)
    }
}

impl From<ChatId> for PeerId {
    #[inline]
    fn from(id: ChatId) -> Self {
        Self::from_chat(id)
    }
}

impl From<ChannelId> for PeerId {
    #[inline]
    fn from(id: ChannelId) -> Self {
        Self::from_channel(id)
    }
}

impl From<FakeChatId> for PeerId {
    #[inline]
    fn from(id: FakeChatId) -> Self {
        Self::from_fake(id)
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_zero() {
            write!(f, "peer:0")
        } else if self.is_user() {
            write!(f, "user:{}", self.bare())
        } else if self.is_chat() {
            write!(f, "chat:{}", self.bare())
        } else if self.is_channel() {
            write!(f, "channel:{}", self.bare())
        } else if self.is_fake() {
            write!(f, "fake:{}", self.bare())
        } else {
            write!(f, "peer:{:#x}", self.raw())
        }
    }
}

/// Computes the default peer accent color index (`Data::DecideColorIndex` / `Ui::DecideColorIndex`).
#[inline]
pub const fn decide_color_index(peer_id: PeerId) -> u8 {
    (peer_id.bare() % (SIMPLE_COLOR_INDEX_COUNT as u64)) as u8
}

/// Maps a color index (`0..64`) to its legacy palette slot (`Ui::ColorIndexToPaletteIndex`).
#[inline]
pub const fn color_index_to_palette_index(color_index: u8) -> u8 {
    const MAP: [u8; 7] = [0, 7, 4, 1, 6, 3, 5];
    MAP[(color_index % SIMPLE_COLOR_INDEX_COUNT) as usize]
}

/// Computes a deterministic fake `PeerId` for a hidden-forward sender name
/// (`Data::FakePeerIdForJustName` in `data_peer.cpp`), used only for color selection.
pub fn fake_peer_id_for_just_name(name: &str) -> PeerId {
    const SHIFT: u64 = 0xFE_u64 << 32;
    let base: u64 = if name.is_empty() {
        777
    } else {
        // Qt QString stores UTF-16LE on Windows/x86_64; hash its UTF-16 code units with CRC32.
        let utf16_bytes: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let crc = crc32fast::hash(&utf16_bytes) as i32;
        crc.unsigned_abs() as u64
    };
    PeerId::from_user(UserId(SHIFT + base))
}

/// Message identifier using Telegram Desktop's 64-bit signed space (`data_msg_id.h`).
///
/// Server messages occupy `1..SERVER_MAX_MSG_ID`. Client-generated pending IDs,
/// story wrapper IDs, scheduled/shortcut/welcome IDs, and UI navigation sentinels
/// occupy dedicated non-overlapping ranges.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct MsgId(pub i64);

/// First client-generated local message ID (`StartClientMsgId = 0x01 - (1LL << 58)`).
pub const START_CLIENT_MSG_ID: MsgId = MsgId(0x01 - (1_i64 << 58));

/// Number of client-generated local message IDs (`ClientMsgIds = 1LL << 31`).
pub const CLIENT_MSG_IDS: i64 = 1_i64 << 31;

/// Exclusive upper bound for client-generated local message IDs (`EndClientMsgId`).
pub const END_CLIENT_MSG_ID: MsgId = MsgId(START_CLIENT_MSG_ID.0 + CLIENT_MSG_IDS);

/// First story wrapper message ID (`StartStoryMsgId = EndClientMsgId + 1`).
pub const START_STORY_MSG_ID: MsgId = MsgId(END_CLIENT_MSG_ID.0 + 1);

/// Maximum server story ID (`ServerMaxStoryId = 1 << 30`).
pub const SERVER_MAX_STORY_ID: StoryId = 1_i32 << 30;

/// Number of story wrapper message IDs (`StoryMsgIds`).
pub const STORY_MSG_IDS: i64 = SERVER_MAX_STORY_ID as i64;

/// Exclusive upper bound for story wrapper message IDs (`EndStoryMsgId`).
pub const END_STORY_MSG_ID: MsgId = MsgId(START_STORY_MSG_ID.0 + STORY_MSG_IDS);

/// Exclusive upper bound for regular server message IDs (`ServerMaxMsgId = 1LL << 56`).
pub const SERVER_MAX_MSG_ID: MsgId = MsgId(1_i64 << 56);

/// Exclusive upper bound for scheduled message IDs (`ScheduledMaxMsgId`).
pub const SCHEDULED_MAX_MSG_ID: MsgId = MsgId(SERVER_MAX_MSG_ID.0 + (1_i64 << 32));

/// Exclusive upper bound for business quick-reply shortcut message IDs (`ShortcutMaxMsgId`).
pub const SHORTCUT_MAX_MSG_ID: MsgId = MsgId(SCHEDULED_MAX_MSG_ID.0 + (1_i64 << 32));

/// Exclusive upper bound for welcome template message IDs (`WelcomeMaxMsgId`).
pub const WELCOME_MAX_MSG_ID: MsgId = MsgId(SHORTCUT_MAX_MSG_ID.0 + (1_i64 << 32));

/// Sentinel: open chat at the first unread message (`ShowAtUnreadMsgId = 0`).
pub const SHOW_AT_UNREAD_MSG_ID: MsgId = MsgId(0);

/// Base shift for special navigation message IDs (`SpecialMsgIdShift = EndStoryMsgId`).
pub const SPECIAL_MSG_ID_SHIFT: i64 = END_STORY_MSG_ID.0;

/// Sentinel: open chat scrolled to the very bottom (`ShowAtTheEndMsgId`).
pub const SHOW_AT_THE_END_MSG_ID: MsgId = MsgId(SPECIAL_MSG_ID_SHIFT + 1);

/// Sentinel: switch to chat at top (`SwitchAtTopMsgId`).
pub const SWITCH_AT_TOP_MSG_ID: MsgId = MsgId(SPECIAL_MSG_ID_SHIFT + 2);

/// Sentinel: show chat and start bot (`ShowAndStartBotMsgId`).
pub const SHOW_AND_START_BOT_MSG_ID: MsgId = MsgId(SPECIAL_MSG_ID_SHIFT + 4);

/// Sentinel: show chat and maybe start bot (`ShowAndMaybeStartBotMsgId`).
pub const SHOW_AND_MAYBE_START_BOT_MSG_ID: MsgId = MsgId(SPECIAL_MSG_ID_SHIFT + 5);

/// Sentinel: show chat in message-chooser mode (`ShowForChooseMessagesMsgId`).
pub const SHOW_FOR_CHOOSE_MESSAGES_MSG_ID: MsgId = MsgId(SPECIAL_MSG_ID_SHIFT + 6);

/// Offset hint constant for search queries (`kSearchQueryOffsetHint`).
pub const SEARCH_QUERY_OFFSET_HINT: i32 = -1;

// Compile-time invariants matching `static_assert` in `data_msg_id.h`.
const _: () = assert!(SPECIAL_MSG_ID_SHIFT + 0xFF < 0);
const _: () = assert!(-(SPECIAL_MSG_ID_SHIFT + 0xFF) > SERVER_MAX_MSG_ID.0);

impl MsgId {
    /// Zero / unset message ID.
    pub const ZERO: Self = Self(0);

    /// Returns the underlying `i64` value (`MsgId::bare`).
    #[inline]
    pub const fn bare(self) -> i64 {
        self.0
    }

    /// Returns `true` if `self == MsgId::ZERO`.
    #[inline]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Returns `true` if `self` is a valid regular server message ID (`IsServerMsgId`).
    #[inline]
    pub const fn is_server(self) -> bool {
        self.0 > 0 && self.0 < SERVER_MAX_MSG_ID.0
    }

    /// Returns `true` if `self` is a client-generated local message ID (`IsClientMsgId`).
    #[inline]
    pub const fn is_client(self) -> bool {
        self.0 >= START_CLIENT_MSG_ID.0 && self.0 < END_CLIENT_MSG_ID.0
    }

    /// Extracts the 0-based client message index (`ClientMsgIndex`), or `None` if not a client ID.
    #[inline]
    pub const fn client_index(self) -> Option<i32> {
        if self.is_client() {
            Some((self.0 - START_CLIENT_MSG_ID.0) as i32)
        } else {
            None
        }
    }

    /// Constructs a client message ID from a non-negative index (`ClientMsgByIndex`).
    #[inline]
    pub const fn client_by_index(index: i32) -> Option<Self> {
        if index >= 0 {
            Some(Self(START_CLIENT_MSG_ID.0 + (index as i64)))
        } else {
            None
        }
    }

    /// Returns `true` if `self` is a story wrapper message ID (`IsStoryMsgId`).
    #[inline]
    pub const fn is_story(self) -> bool {
        self.0 >= START_STORY_MSG_ID.0 && self.0 < END_STORY_MSG_ID.0
    }

    /// Extracts the `StoryId` from a story wrapper message ID (`StoryIdFromMsgId`).
    #[inline]
    pub const fn to_story_id(self) -> Option<StoryId> {
        if self.is_story() {
            Some((self.0 - START_STORY_MSG_ID.0) as StoryId)
        } else {
            None
        }
    }

    /// Encodes a non-negative `StoryId` into a story wrapper message ID (`StoryIdToMsgId`).
    #[inline]
    pub const fn from_story_id(story_id: StoryId) -> Option<Self> {
        if story_id >= 0 && story_id < SERVER_MAX_STORY_ID {
            Some(Self(START_STORY_MSG_ID.0 + (story_id as i64)))
        } else {
            None
        }
    }

    /// Returns `true` if `self` falls in the scheduled message ID range (`IsScheduledMsgId`).
    ///
    /// Matches `data/components/scheduled_messages.cpp:117-119`: strictly greater than
    /// `SERVER_MAX_MSG_ID` (which is the `SparseIdsList` bottom sentinel) and less than
    /// `SCHEDULED_MAX_MSG_ID`.
    #[inline]
    pub const fn is_scheduled(self) -> bool {
        self.0 > SERVER_MAX_MSG_ID.0 && self.0 < SCHEDULED_MAX_MSG_ID.0
    }

    /// Returns `true` if `self` falls in the business quick-reply shortcut ID range
    /// (`IsShortcutMsgId`).
    ///
    /// Matches `data/business/data_shortcut_messages.cpp:113-115`: strictly greater than
    /// `SCHEDULED_MAX_MSG_ID` and less than `SHORTCUT_MAX_MSG_ID`.
    #[inline]
    pub const fn is_shortcut(self) -> bool {
        self.0 > SCHEDULED_MAX_MSG_ID.0 && self.0 < SHORTCUT_MAX_MSG_ID.0
    }

    /// Returns `true` if `self` falls in the welcome template message ID range.
    #[inline]
    pub const fn is_welcome(self) -> bool {
        self.0 > SHORTCUT_MAX_MSG_ID.0 && self.0 < WELCOME_MAX_MSG_ID.0
    }

    /// Converts to a 32-bit TL `int` if in `i32` range.
    #[inline]
    pub const fn as_i32(self) -> Option<i32> {
        if self.0 >= i32::MIN as i64 && self.0 <= i32::MAX as i64 {
            Some(self.0 as i32)
        } else {
            None
        }
    }
}

impl From<i32> for MsgId {
    #[inline]
    fn from(value: i32) -> Self {
        Self(value as i64)
    }
}

impl From<i64> for MsgId {
    #[inline]
    fn from(value: i64) -> Self {
        Self(value)
    }
}

impl fmt::Display for MsgId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Pair of `(peer, msg)` uniquely identifying a message within a session (`FullMsgId`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct FullMsgId {
    /// Owning peer ID (`PeerId::ZERO` for global non-channel lookup keys when needed).
    pub peer: PeerId,
    /// Message ID within the peer's history.
    pub msg: MsgId,
}

impl FullMsgId {
    /// Zero / empty `FullMsgId`.
    pub const ZERO: Self = Self {
        peer: PeerId::ZERO,
        msg: MsgId::ZERO,
    };

    /// Creates a new `FullMsgId`.
    #[inline]
    pub const fn new(peer: PeerId, msg: MsgId) -> Self {
        Self { peer, msg }
    }

    /// Returns `true` if `msg != 0` (matching C++ `explicit operator bool()`).
    #[inline]
    pub const fn is_valid(self) -> bool {
        !self.msg.is_zero()
    }
}

/// Pair of `(peer, story)` uniquely identifying a story (`FullStoryId`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct FullStoryId {
    /// Owner peer ID.
    pub peer: PeerId,
    /// Story ID within the peer.
    pub story: StoryId,
}

impl FullStoryId {
    /// Creates a new `FullStoryId`.
    #[inline]
    pub const fn new(peer: PeerId, story: StoryId) -> Self {
        Self { peer, story }
    }

    /// Returns `true` if both `peer` and `story` are non-zero.
    #[inline]
    pub const fn is_valid(self) -> bool {
        !self.peer.is_zero() && self.story != 0
    }
}

/// Globally unique message ID across multiple active sessions (`GlobalMsgId`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct GlobalMsgId {
    /// Message ID within its session.
    pub item_id: FullMsgId,
    /// Unique identifier of the owning account session.
    pub session_unique_id: u64,
}

impl GlobalMsgId {
    /// Returns `true` if both `item_id` and `session_unique_id` are valid.
    #[inline]
    pub const fn is_valid(self) -> bool {
        self.item_id.is_valid() && self.session_unique_id != 0
    }
}

/// Half-open or inclusive range `[from, till]` of `MsgId`s used by `SparseIdsList`
/// and difference requests (`MsgRange`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct MsgRange {
    /// Lower bound message ID.
    pub from: MsgId,
    /// Upper bound message ID.
    pub till: MsgId,
}

impl MsgRange {
    /// Creates a new `MsgRange`.
    #[inline]
    pub const fn new(from: MsgId, till: MsgId) -> Self {
        Self { from, till }
    }

    /// Returns `true` if `from >= till`.
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.from.0 >= self.till.0
    }
}

/// Album / media group identifier scoped to a peer and scheduled flag (`MessageGroupId`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct MessageGroupId {
    /// Peer ID raw bits OR-ed with `(1 << 55)` when scheduled.
    pub peer_and_scheduled_flag: u64,
    /// Raw server `grouped_id`.
    pub value: u64,
}

impl MessageGroupId {
    /// Bit 55 flag marking scheduled albums (`1ULL << 55`).
    pub const SCHEDULED_FLAG: u64 = 1_u64 << 55;

    /// Builds a `MessageGroupId` from peer, raw `grouped_id`, and scheduled status.
    #[inline]
    pub const fn from_raw(peer: PeerId, value: u64, scheduled: bool) -> Self {
        Self {
            peer_and_scheduled_flag: peer.raw() | if scheduled { Self::SCHEDULED_FLAG } else { 0 },
            value,
        }
    }

    /// Returns `true` if `value == 0`.
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.value == 0
    }

    /// Returns the raw `grouped_id`.
    #[inline]
    pub const fn raw(self) -> u64 {
        self.value
    }

    /// Returns the peer encoded in this group ID.
    #[inline]
    pub const fn peer_id(self) -> PeerId {
        PeerId::from_raw(self.peer_and_scheduled_flag & !Self::SCHEDULED_FLAG)
    }

    /// Returns `true` if this group belongs to scheduled messages.
    #[inline]
    pub const fn is_scheduled(self) -> bool {
        (self.peer_and_scheduled_flag & Self::SCHEDULED_FLAG) != 0
    }
}

/// Chronological position of a message (`Data::MessagePosition` in `data_messages.h`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct MessagePosition {
    /// Full message ID.
    pub full_id: FullMsgId,
    /// Message timestamp.
    pub date: TimeId,
}

impl MessagePosition {
    /// Minimum possible message position (`MinMessagePosition`).
    pub const MIN: Self = Self {
        full_id: FullMsgId::new(PeerId::ZERO, MsgId(1)),
        date: MIN_DATE,
    };

    /// Maximum possible message position (`MaxMessagePosition`).
    pub const MAX: Self = Self {
        full_id: FullMsgId::new(PeerId::ZERO, MsgId(SERVER_MAX_MSG_ID.0 - 1)),
        date: MAX_DATE,
    };

    /// Sentinel position for opening at unread (`UnreadMessagePosition`).
    pub const UNREAD: Self = Self {
        full_id: FullMsgId::new(PeerId::ZERO, SHOW_AT_UNREAD_MSG_ID),
        date: MIN_DATE,
    };
}

impl Ord for MessagePosition {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.date
            .cmp(&other.date)
            .then_with(|| self.full_id.cmp(&other.full_id))
    }
}

impl PartialOrd for MessagePosition {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
