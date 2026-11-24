//! Canonical demo session peer/folder IDs and [`PendingChatOpen`] navigation DTO.

use crate::stand_in::types::{MsgId, PeerId};

/// Synthetic demo Mini App bot user ID (`Catalog Bot`).
pub const SAMPLE_BOT_ID: u64 = 777_001;

/// Canonical peer identifiers shared across every screen in `MockSession`.
pub mod session_peers {
    use super::SAMPLE_BOT_ID;
    use crate::stand_in::types::PeerId;

    /// Authenticated self account and Saved Messages (`Taylor Kim`).
    pub const SELF: PeerId = PeerId::from_user(1000);
    /// Direct contact (`Mira Vance`).
    pub const USER_MIRA: PeerId = PeerId::from_user(1001);
    /// Direct non-contact (`Theo Park`).
    pub const USER_THEO: PeerId = PeerId::from_user(1002);
    /// Direct contact (`Sam Chen`).
    pub const USER_SAM: PeerId = PeerId::from_user(1003);
    /// Direct contact (`Riley Brooks`).
    pub const USER_RILEY: PeerId = PeerId::from_user(1004);
    /// Verified Mini App bot (`Catalog Bot`, `@catalog_demo_bot`).
    pub const BOT_CATALOG: PeerId = PeerId::from_user(SAMPLE_BOT_ID);
    /// Pinned supergroup (`Rustgram Core Port`, `@rustgram_core`).
    pub const GROUP_CORE: PeerId = PeerId::from_chat(2001);
    /// Engineering supergroup (`UI & Theme Lab`, `@rustgram_ui`).
    pub const GROUP_UI_LAB: PeerId = PeerId::from_chat(2002);
    /// Broadcast channel (`Rustgram Dev Log`, `@rustgram_log`) with Statistics & Boosts.
    pub const CHANNEL_DEV_LOG: PeerId = PeerId::from_channel(3001);
    /// Muted broadcast channel (`Nightly Builds`, `@rustgram_builds`).
    pub const CHANNEL_RELEASES: PeerId = PeerId::from_channel(3002);
    /// Archived broadcast channel (`Archive Digest`, `@archive_digest`).
    pub const CHANNEL_ARCHIVE: PeerId = PeerId::from_channel(3003);
}

/// Built-in folder filter IDs shared between dialogs sidebar, horizontal tabs, and Settings.
pub mod session_folders {
    use crate::stand_in::types::FilterId;

    /// Personal chats folder (`id = 1`).
    pub const PERSONAL: FilterId = FilterId(1);
    /// Work chats folder (`id = 2`).
    pub const WORK: FilterId = FilterId(2);
    /// Broadcast channels folder (`id = 3`).
    pub const CHANNELS: FilterId = FilterId(3);
    /// Bots folder (`id = 4`).
    pub const BOTS: FilterId = FilterId(4);
    /// Unread chats folder (`id = 5`).
    pub const UNREAD: FilterId = FilterId(5);
}

/// Pending chat navigation emitted when the user clicks a desktop notification toast/popup
/// or media "Show in Chat" action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingChatOpen {
    /// Target chat peer ID.
    pub peer_id: PeerId,
    /// Target message ID to scroll/jump to (`MsgId::SHOW_AT_END` for bottom).
    pub msg_id: MsgId,
    /// Optional inline reply draft text carried from a native toast.
    pub draft: Option<String>,
}
