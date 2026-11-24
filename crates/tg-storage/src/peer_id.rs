//! Peer ID and DraftKey serialization compatible with tdesktop.
//!
//! Port of `Telegram/SourceFiles/data/data_peer_id.h`, `data_peer_id.cpp`,
//! and `DraftKey` from `Telegram/SourceFiles/data/data_drafts.h`.

/// 48-bit bare ID mask inside tdesktop's `PeerId`.
pub const PEER_ID_CHAT_TYPE_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

/// Reserved bit 55 (`0x80 << 48`) distinguishing modern from legacy serialized `PeerId`.
pub const PEER_ID_RESERVED_BIT: u64 = 0x80u64 << 48;

/// Shift value for user peers.
pub const PEER_SHIFT_USER: u8 = 0;
/// Shift value for basic group chat peers.
pub const PEER_SHIFT_CHAT: u8 = 1;
/// Shift value for channel/supergroup peers.
pub const PEER_SHIFT_CHANNEL: u8 = 2;
/// Shift value for fake/synthetic chat peers.
pub const PEER_SHIFT_FAKE: u8 = 0x7F;

/// Maximum server message ID (`ServerMaxMsgId = 1LL << 56`).
pub const SERVER_MAX_MSG_ID: i64 = 1i64 << 56;

/// tdesktop `PeerId` encoding: lower 48 bits hold the bare ID, bits 48..55 hold the peer type shift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct PeerId(pub u64);

impl PeerId {
    /// Zero / empty peer ID.
    pub const ZERO: Self = Self(0);

    /// Construct a user `PeerId` from a bare user ID.
    #[must_use]
    pub const fn from_user(bare: u64) -> Self {
        Self((bare & PEER_ID_CHAT_TYPE_MASK) | ((PEER_SHIFT_USER as u64) << 48))
    }

    /// Construct a chat `PeerId` from a bare chat ID.
    #[must_use]
    pub const fn from_chat(bare: u64) -> Self {
        Self((bare & PEER_ID_CHAT_TYPE_MASK) | ((PEER_SHIFT_CHAT as u64) << 48))
    }

    /// Construct a channel `PeerId` from a bare channel ID.
    #[must_use]
    pub const fn from_channel(bare: u64) -> Self {
        Self((bare & PEER_ID_CHAT_TYPE_MASK) | ((PEER_SHIFT_CHANNEL as u64) << 48))
    }

    /// Construct a fake chat `PeerId` from a bare ID.
    #[must_use]
    pub const fn from_fake(bare: u64) -> Self {
        Self((bare & PEER_ID_CHAT_TYPE_MASK) | ((PEER_SHIFT_FAKE as u64) << 48))
    }

    /// Return the type shift byte (`0` = user, `1` = chat, `2` = channel, `0x7F` = fake).
    #[must_use]
    pub const fn kind_shift(self) -> u8 {
        ((self.0 >> 48) & 0xFF) as u8
    }

    /// Return the lower 48-bit bare ID.
    #[must_use]
    pub const fn bare_id(self) -> u64 {
        self.0 & PEER_ID_CHAT_TYPE_MASK
    }

    /// Whether this peer is a user.
    #[must_use]
    pub const fn is_user(self) -> bool {
        self.kind_shift() == PEER_SHIFT_USER
    }

    /// Whether this peer is a basic chat.
    #[must_use]
    pub const fn is_chat(self) -> bool {
        self.kind_shift() == PEER_SHIFT_CHAT
    }

    /// Whether this peer is a channel/supergroup.
    #[must_use]
    pub const fn is_channel(self) -> bool {
        self.kind_shift() == PEER_SHIFT_CHANNEL
    }

    /// Whether this peer is zero/empty.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }
}

/// Serialize a `PeerId` using tdesktop's modern format (`SerializePeerId`).
///
/// Sets bit 55 (`0x80 << 48`) so readers distinguish modern 64-bit IDs from legacy 32-bit IDs.
#[must_use]
pub const fn serialize_peer_id(id: PeerId) -> u64 {
    id.0 | PEER_ID_RESERVED_BIT
}

/// Deserialize a `PeerId` supporting both modern and legacy encodings (`DeserializePeerId`).
#[must_use]
pub const fn deserialize_peer_id(serialized: u64) -> PeerId {
    if (serialized & PEER_ID_RESERVED_BIT) != 0 {
        return PeerId(serialized & !PEER_ID_RESERVED_BIT);
    }
    const PEER_ID_MASK: u64 = 0xFFFF_FFFF;
    const PEER_ID_TYPE_MASK: u64 = 0x000F_0000_0000;
    const PEER_ID_USER_SHIFT: u64 = 0x0000_0000_0000;
    const PEER_ID_CHAT_SHIFT: u64 = 0x0001_0000_0000;
    const PEER_ID_CHANNEL_SHIFT: u64 = 0x0002_0000_0000;
    const PEER_ID_FAKE_SHIFT: u64 = 0x000F_0000_0000;

    let bare = serialized & PEER_ID_MASK;
    match serialized & PEER_ID_TYPE_MASK {
        PEER_ID_USER_SHIFT => PeerId::from_user(bare),
        PEER_ID_CHAT_SHIFT => PeerId::from_chat(bare),
        PEER_ID_CHANNEL_SHIFT => PeerId::from_channel(bare),
        PEER_ID_FAKE_SHIFT => PeerId::from_fake(bare),
        _ => PeerId::ZERO,
    }
}

/// Serialized draft key identifying local/edit/cloud/topic/monoforum/scheduled drafts (`Data::DraftKey`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct DraftKey(pub i64);

impl DraftKey {
    const LOCAL_DRAFT_INDEX: i64 = -1;
    const CLOUD_DRAFT_INDEX: i64 = -2;
    const SCHEDULED_DRAFT_INDEX: i64 = -3;
    const WELCOME_MESSAGES_DRAFT_INDEX: i64 = -4;
    const MONOFORUM_DRAFT_BIT: i64 = 1i64 << 60;
    const MONOFORUM_DRAFT_MASK: i64 = Self::MONOFORUM_DRAFT_BIT - 1;
    const EDIT_DRAFT_SHIFT: i64 = SERVER_MAX_MSG_ID;
    const CLOUD_DRAFT_SHIFT: i64 = 2 * SERVER_MAX_MSG_ID;
    const SHORTCUT_DRAFT_SHIFT: i64 = 3 * SERVER_MAX_MSG_ID;
    const EDIT_DRAFT_SHIFT_OLD: i32 = 0x3FFF_FFFF;

    /// Empty / invalid draft key.
    #[must_use]
    pub const fn none() -> Self {
        Self(0)
    }

    /// Local draft key for a chat, forum topic, or monoforum peer.
    #[must_use]
    pub const fn local(topic_root_id: i64, monoforum_peer_id: PeerId) -> Self {
        if Self::invalid(topic_root_id, monoforum_peer_id) {
            Self::none()
        } else if topic_root_id != 0 {
            Self(topic_root_id)
        } else if !monoforum_peer_id.is_zero() {
            Self((monoforum_peer_id.0 as i64) + Self::MONOFORUM_DRAFT_BIT)
        } else {
            Self(Self::LOCAL_DRAFT_INDEX)
        }
    }

    /// Local edit draft key.
    #[must_use]
    pub const fn local_edit(topic_root_id: i64, monoforum_peer_id: PeerId) -> Self {
        if Self::invalid(topic_root_id, monoforum_peer_id) {
            Self::none()
        } else {
            let base = if topic_root_id != 0 {
                topic_root_id
            } else if !monoforum_peer_id.is_zero() {
                (monoforum_peer_id.0 as i64) + Self::MONOFORUM_DRAFT_BIT
            } else {
                Self::LOCAL_DRAFT_INDEX
            };
            Self(Self::EDIT_DRAFT_SHIFT + base)
        }
    }

    /// Cloud draft key.
    #[must_use]
    pub const fn cloud(topic_root_id: i64, monoforum_peer_id: PeerId) -> Self {
        if Self::invalid(topic_root_id, monoforum_peer_id) {
            Self::none()
        } else if topic_root_id != 0 {
            Self(Self::CLOUD_DRAFT_SHIFT + topic_root_id)
        } else if !monoforum_peer_id.is_zero() {
            Self(Self::CLOUD_DRAFT_SHIFT + (monoforum_peer_id.0 as i64) + Self::MONOFORUM_DRAFT_BIT)
        } else {
            Self(Self::CLOUD_DRAFT_INDEX)
        }
    }

    /// Scheduled messages draft key.
    #[must_use]
    pub const fn scheduled() -> Self {
        Self(Self::SCHEDULED_DRAFT_INDEX)
    }

    /// Scheduled messages edit draft key.
    #[must_use]
    pub const fn scheduled_edit() -> Self {
        Self(Self::SCHEDULED_DRAFT_INDEX + Self::EDIT_DRAFT_SHIFT)
    }

    /// Business welcome messages draft key.
    #[must_use]
    pub const fn welcome_messages() -> Self {
        Self(Self::WELCOME_MESSAGES_DRAFT_INDEX)
    }

    /// Business quick-reply shortcut draft key.
    #[must_use]
    pub const fn shortcut(shortcut_id: i64) -> Self {
        if shortcut_id < 0 || shortcut_id >= SERVER_MAX_MSG_ID {
            Self::none()
        } else {
            Self(Self::SHORTCUT_DRAFT_SHIFT + shortcut_id)
        }
    }

    /// Deserialize from modern 64-bit value (`DraftKey::FromSerialized`).
    #[must_use]
    pub const fn from_serialized(value: i64) -> Self {
        Self(value)
    }

    /// Serialize to 64-bit value (`DraftKey::serialize`).
    #[must_use]
    pub const fn serialize(self) -> i64 {
        self.0
    }

    /// Deserialize from legacy 32-bit value (`DraftKey::FromSerializedOld`).
    #[must_use]
    pub const fn from_serialized_old(value: i32) -> Self {
        if value == 0 {
            Self::none()
        } else if value == (Self::LOCAL_DRAFT_INDEX as i32) + Self::EDIT_DRAFT_SHIFT_OLD {
            Self::local_edit(0, PeerId::ZERO)
        } else if value == (Self::SCHEDULED_DRAFT_INDEX as i32) + Self::EDIT_DRAFT_SHIFT_OLD {
            Self::scheduled_edit()
        } else if value > 0 && value < 0x4000_0000 {
            Self::local(value as i64, PeerId::ZERO)
        } else if value > Self::EDIT_DRAFT_SHIFT_OLD
            && value < Self::EDIT_DRAFT_SHIFT_OLD + 0x0400_0000
        {
            Self::local_edit((value - Self::EDIT_DRAFT_SHIFT_OLD) as i64, PeerId::ZERO)
        } else {
            Self::none()
        }
    }

    /// Whether this key is non-zero.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.0 != 0
    }

    /// Whether this draft key represents a cloud draft.
    #[must_use]
    pub const fn is_cloud(self) -> bool {
        let masked = self.0 & Self::MONOFORUM_DRAFT_MASK;
        self.0 == Self::CLOUD_DRAFT_INDEX
            || (masked > Self::CLOUD_DRAFT_SHIFT
                && masked < Self::CLOUD_DRAFT_SHIFT + SERVER_MAX_MSG_ID)
    }

    /// Extract forum topic root message ID if applicable.
    #[must_use]
    pub const fn topic_root_id(self) -> i64 {
        let max = SERVER_MAX_MSG_ID;
        if (self.0 & Self::MONOFORUM_DRAFT_BIT) != 0 {
            0
        } else if self.0 > Self::CLOUD_DRAFT_SHIFT && self.0 < Self::CLOUD_DRAFT_SHIFT + max {
            self.0 - Self::CLOUD_DRAFT_SHIFT
        } else if self.0 > Self::EDIT_DRAFT_SHIFT && self.0 < Self::EDIT_DRAFT_SHIFT + max {
            self.0 - Self::EDIT_DRAFT_SHIFT
        } else if self.0 > 0 && self.0 < max {
            self.0
        } else {
            0
        }
    }

    const fn invalid(topic_root_id: i64, monoforum_peer_id: PeerId) -> bool {
        topic_root_id < 0
            || topic_root_id >= SERVER_MAX_MSG_ID
            || !monoforum_peer_id.is_user()
            || monoforum_peer_id.0 >= (SERVER_MAX_MSG_ID as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_id_modern_and_legacy_roundtrip() {
        let u = PeerId::from_user(123_456_789_012);
        let c = PeerId::from_chat(98_765);
        let ch = PeerId::from_channel(1_234_567_890);

        assert!(u.is_user());
        assert_eq!(u.bare_id(), 123_456_789_012);
        assert!(c.is_chat());
        assert_eq!(c.bare_id(), 98_765);
        assert!(ch.is_channel());
        assert_eq!(ch.bare_id(), 1_234_567_890);

        for peer in [u, c, ch] {
            let s = serialize_peer_id(peer);
            assert_ne!(s & PEER_ID_RESERVED_BIT, 0);
            assert_eq!(deserialize_peer_id(s), peer);
        }

        // Legacy 32-bit encoding (type nibble in bits 32..35)
        assert_eq!(
            deserialize_peer_id(0x0000_0000_0000_002A),
            PeerId::from_user(42)
        );
        assert_eq!(
            deserialize_peer_id(0x0000_0001_0000_002A),
            PeerId::from_chat(42)
        );
        assert_eq!(
            deserialize_peer_id(0x0000_0002_0000_002A),
            PeerId::from_channel(42)
        );
    }

    #[test]
    fn draft_key_conversions() {
        let k = DraftKey::local(0, PeerId::ZERO);
        assert_eq!(k.serialize(), -1);
        assert!(!k.is_cloud());
        assert_eq!(k.topic_root_id(), 0);

        let topic = DraftKey::local(42, PeerId::ZERO);
        assert_eq!(topic.topic_root_id(), 42);
        assert_eq!(DraftKey::from_serialized_old(42), topic);

        let cloud = DraftKey::cloud(0, PeerId::ZERO);
        assert!(cloud.is_cloud());
    }
}
