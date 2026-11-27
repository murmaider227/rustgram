//! What the chat's "three dots" menu may offer for a peer.
//!
//! `Window::Filler` (`window/window_peer_menu.cpp`) asks `PeerData` / `ChannelData` /
//! `ChatData` / `UserData` for every row it adds (`EditPeerInfoBox::Available`,
//! `canCreatePolls`, `canExportChatHistory`, `amCreator`, `amIn`, ...). [`PeerMenuCaps`] carries
//! the answers, so the menu can be built without the data layer.

use crate::history::backend::HistoryPeerInfo;
use crate::stand_in::types::{PeerId, PeerKind};

/// Peer type as `Filler` tells them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PeerMenuKind {
    /// A person (`UserData` without `botInfo`).
    #[default]
    User,
    /// A bot (`UserData::isBot`).
    Bot,
    /// The account's own chat, Saved Messages (`PeerData::isSelf`).
    SavedMessages,
    /// A basic group (`ChatData`).
    BasicGroup,
    /// A supergroup (`ChannelData::isMegagroup`).
    Megagroup,
    /// A broadcast channel (`ChannelData::isBroadcast`).
    Broadcast,
}

impl PeerMenuKind {
    /// `PeerData::isUser` (bots and Saved Messages included).
    #[must_use]
    pub const fn is_user(self) -> bool {
        matches!(self, Self::User | Self::Bot | Self::SavedMessages)
    }

    /// `PeerData::isChannel` (supergroups and broadcast channels).
    #[must_use]
    pub const fn is_channel(self) -> bool {
        matches!(self, Self::Megagroup | Self::Broadcast)
    }

    /// `isChat() || isMegagroup()`.
    #[must_use]
    pub const fn is_group(self) -> bool {
        matches!(self, Self::BasicGroup | Self::Megagroup)
    }
}

/// The facts `Filler::fillHistoryActions` reads about a peer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PeerMenuCaps {
    /// The peer.
    pub peer_id: PeerId,
    /// Peer type.
    pub kind: PeerMenuKind,
    /// `isRepliesChat() || isVerifyCodes()`: no info, theme, export or poll rows.
    pub replies_or_verify_codes: bool,
    /// `UserData::isServiceUser` (the 777000 notifications account).
    pub service_user: bool,
    /// `UserData::isInaccessible` (a deleted account).
    pub inaccessible: bool,
    /// `UserData::isSupport`.
    pub support: bool,
    /// `UserData::isBlocked`.
    pub blocked: bool,
    /// `requiresPremiumToWrite() && !session().premium()`.
    pub premium_required_to_write: bool,
    /// A bot whose chat has no messages yet or a pending start token (`isBotStart` in
    /// `Filler::skipCreateActions`).
    pub bot_start_pending: bool,
    /// `PeerData::isForum`.
    pub forum: bool,
    /// `ChannelDataFlag::ForumTabs`.
    pub forum_tabs: bool,
    /// `PeerData::isMonoforum` (channel direct messages chat).
    pub monoforum: bool,
    /// `ChannelData::amIn` / `ChatData::amIn` (always `true` for users).
    pub am_in: bool,
    /// `amCreator()` of a basic group or channel.
    pub am_creator: bool,
    /// `EditPeerInfoBox::Available` (for bots: `botInfo->canEditInformation`).
    pub can_manage: bool,
    /// `canDeleteMessages()` of a channel.
    pub can_delete_messages: bool,
    /// `ChannelData::isPublic`.
    pub public: bool,
    /// `PeerData::canCreateTopics`.
    pub can_create_topics: bool,
    /// `PeerData::canCreatePolls`.
    pub can_create_polls: bool,
    /// `PeerData::canCreateTodoLists` (needs Premium).
    pub can_create_todo_lists: bool,
    /// `PeerData::canExportChatHistory`.
    pub can_export: bool,
    /// `Media::Stories::CanPostStory`.
    pub can_post_stories: bool,
    /// `ChannelData::canEditStories`.
    pub can_edit_stories: bool,
    /// `!UserData::allowsForwarding()` of a 1:1 chat ("Enable sharing" instead of "Disable").
    pub sharing_disabled: bool,
    /// `ChannelData::discussionLink` of a broadcast channel.
    pub discussion: Option<PeerId>,
    /// `broadcastMonoforum()` exists and `amMonoforumAdmin()`.
    pub direct_messages: bool,
    /// The chat offers "Translate" (`Filler::addTranslate`).
    pub translate_offered: bool,
    /// Notifications are muted.
    pub muted: bool,
    /// When the mute ends (`muteUntil`), for the "Unmute\t2h" hint.
    pub mute_until: Option<i32>,
    /// The chat's notification sound is off (`NotifySound::none`).
    pub silent: bool,
}

impl PeerMenuCaps {
    /// Best guess from the top bar's [`HistoryPeerInfo`] for backends that do not read the data
    /// layer: it never offers rows that need rights the summary cannot show (manage, report as
    /// creator, export).
    #[must_use]
    pub fn from_peer_info(info: &HistoryPeerInfo) -> Self {
        let summary = &info.summary;
        let kind = match summary.kind {
            _ if summary.is_saved_messages => PeerMenuKind::SavedMessages,
            PeerKind::User => PeerMenuKind::User,
            PeerKind::Group if summary.id.kind() == PeerKind::Group => PeerMenuKind::BasicGroup,
            PeerKind::Group => PeerMenuKind::Megagroup,
            PeerKind::Channel => PeerMenuKind::Broadcast,
        };
        Self {
            peer_id: summary.id,
            kind,
            am_in: true,
            can_delete_messages: info.can_delete,
            can_create_polls: summary.can_send && kind != PeerMenuKind::User,
            muted: summary.muted,
            ..Self::default()
        }
    }
}
