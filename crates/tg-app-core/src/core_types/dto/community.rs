//! Pure community DTOs (`boxes/community.rs` model).

use crate::stand_in::types::PeerId;

/// RPC error string returned by `communities.togglePeerLink` when a link request
/// was created instead of immediately linking the peer (`kCommunityRequestCreated`).
pub const COMMUNITY_REQUEST_CREATED: &str = "COMMUNITY_REQUEST_CREATED";

/// RPC error string returned when a community reached its peer limit (`kCommunityPeersTooMuch`).
pub const COMMUNITY_PEERS_TOO_MUCH: &str = "COMMUNITY_PEERS_TOO_MUCH";

/// Default maximum linked groups/channels in a community (`CommunityPeersLimit`).
pub const DEFAULT_COMMUNITY_PEERS_LIMIT: usize = 100;

/// Default maximum linked bots in a community (`CommunityBotPeersLimit`).
pub const DEFAULT_COMMUNITY_BOT_PEERS_LIMIT: usize = 100;

/// Action passed to `communities.togglePeerLink` (`Api::PeerLinkAction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommunityPeerLinkAction {
    /// Link peer visibly to all community members (`visible = true`).
    Visible,
    /// Link peer privately / hidden (`hidden = true`).
    Hidden,
    /// Remove peer link from the community (`deleted = true`).
    Deleted,
}

/// Single linked peer inside a community (`Data::CommunityLinkedPeer` / `communityPeer`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommunityLinkedPeer {
    /// Linked group, channel, or bot peer ID.
    pub peer_id: PeerId,
    /// Display title.
    pub title: String,
    /// Optional public username without `@`.
    pub username: Option<String>,
    /// True if this peer is a broadcast channel.
    pub is_channel: bool,
    /// True if this peer is a bot user.
    pub is_bot: bool,
    /// Member or subscriber count (`0` if unknown or bot).
    pub members_count: u32,
    /// True if the current user is already a member (`Data::CommunityChatJoined`).
    pub joined: bool,
    /// Visibility override from TL `communityPeer.visible`:
    /// `Some(true)` = visible, `Some(false)` = hidden, `None` = default visible.
    pub visible: Option<bool>,
    /// Whether non-members can preview message history (`communityPeer.can_view_history`).
    pub can_view_history: bool,
    /// Unread message count for joined chats.
    pub unread_count: u32,
    /// True if notifications are muted for this chat.
    pub muted: bool,
    /// Optional latest message preview text.
    pub last_message: String,
    /// Optional latest message Unix timestamp.
    pub last_date_unix: i32,
    /// Deterministic avatar palette color index (`0..7`).
    pub color_index: u8,
}

impl CommunityLinkedPeer {
    /// Returns `true` if this linked peer is hidden (`visible == Some(false)`).
    #[must_use]
    pub const fn is_hidden(&self) -> bool {
        matches!(self.visible, Some(false))
    }

    /// Returns `true` if a non-member can view or join this chat (`Data::IsCommunityChatViewable`).
    #[must_use]
    pub const fn is_viewable(&self) -> bool {
        !self.is_hidden() || self.can_view_history
    }
}

/// Pending request to link a chat/bot into a community (`Api::CommunityPeerRequest`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommunityPeerRequest {
    /// Peer requesting to be linked.
    pub peer_id: PeerId,
    /// Display title of the peer.
    pub peer_title: String,
    /// Optional peer username without `@`.
    pub peer_username: Option<String>,
    /// True if the peer is a broadcast channel.
    pub is_channel: bool,
    /// True if the peer is a bot user.
    pub is_bot: bool,
    /// User who submitted the link request.
    pub requested_by: Option<PeerId>,
    /// Display name of the requester.
    pub requested_by_name: String,
    /// Request Unix timestamp.
    pub date: i32,
    /// Whether the requester asked for visible placement (`communityPeerRequest.visible`).
    pub visible: bool,
    /// Deterministic avatar palette index (`0..7`).
    pub color_index: u8,
}

/// Paginated slice of pending peer link requests (`Api::CommunityPeerRequestsSlice`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommunityPeerRequestsSlice {
    /// Requests on this page.
    pub list: Vec<CommunityPeerRequest>,
    /// Pagination token for the next page (empty when complete).
    pub next_offset: String,
    /// Total pending requests reported by `communities.peerLinkRequests.total_count`.
    pub total_count: i32,
}

/// Chats inside a community that a participant owns or belongs to
/// (`Api::CommunityParticipantJoinedChats`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommunityParticipantJoinedChats {
    /// Linked chats created by the participant (`creator_chat_ids`).
    pub creator_chat_ids: Vec<PeerId>,
    /// Linked chats joined by the participant (`joined_chat_ids`).
    pub joined_chat_ids: Vec<PeerId>,
}

/// Full view model for a Layer-233 Community (`community` + `communityFull`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommunityDetails {
    /// Community channel peer ID.
    pub id: PeerId,
    /// Community title.
    pub title: String,
    /// Community description / about text.
    pub about: String,
    /// Whether member chats are grouped into a single row in the dialogs list
    /// (`ChannelDataFlag::CommunityCollapsed`).
    pub collapsed_in_dialogs: bool,
    /// Whether all members (vs only admins) can request/add chats
    /// (`!default_banned_rights.contains(MANAGE_LINKED_PEERS)`).
    pub anyone_can_add_peers: bool,
    /// Whether current user can edit title/settings (`canEditInformation`).
    pub can_edit: bool,
    /// Whether current user can add/remove linked peers & approve requests (`canManageLinkedPeers`).
    pub can_manage_linked_peers: bool,
    /// Whether current user is creator and can delete the community (`canDelete`).
    pub can_delete: bool,
    /// Number of community administrators.
    pub admins_count: u32,
    /// Number of banned / removed participants.
    pub kicked_count: u32,
    /// Number of pending peer link requests.
    pub pending_requests_count: u32,
    /// Linked groups, channels, and bots.
    pub linked_peers: Vec<CommunityLinkedPeer>,
    /// Avatar palette index (`0..7`).
    pub color_index: u8,
}

impl CommunityDetails {
    /// Returns joined member chats sorted by latest message timestamp descending.
    #[must_use]
    pub fn joined_chats(&self) -> Vec<&CommunityLinkedPeer> {
        let mut out: Vec<&CommunityLinkedPeer> = self
            .linked_peers
            .iter()
            .filter(|p| !p.is_bot && p.joined)
            .collect();
        out.sort_by_key(|p| std::cmp::Reverse(p.last_date_unix));
        out
    }

    /// Returns viewable non-joined chats (groups & channels) that the user can discover and join.
    #[must_use]
    pub fn discoverable_chats(&self) -> Vec<&CommunityLinkedPeer> {
        self.linked_peers
            .iter()
            .filter(|p| !p.is_bot && !p.joined && p.is_viewable())
            .collect()
    }

    /// Returns linked bots in the community.
    #[must_use]
    pub fn linked_bots(&self) -> Vec<&CommunityLinkedPeer> {
        self.linked_peers.iter().filter(|p| p.is_bot).collect()
    }

    /// Total aggregated unread messages across joined member chats.
    #[must_use]
    pub fn aggregated_unread_count(&self) -> u32 {
        self.linked_peers
            .iter()
            .filter(|p| p.joined)
            .map(|p| p.unread_count)
            .sum()
    }
}

/// Request payload for `communities.create`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateCommunityRequest {
    /// Community title (1..=128 chars).
    pub title: String,
    /// Optional community about text (0..=255 chars).
    pub about: String,
    /// Initial peer to link into the new community.
    pub initial_peer_id: PeerId,
    /// Whether the initial peer link should be hidden (`flags.1?true`).
    pub hidden: bool,
}

/// Sub-actions emitted from [`CommunityBoxState::show_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommunityBoxAction {
    /// User clicked a joined chat or bot to open its history.
    OpenPeer(PeerId),
    /// User clicked "Manage Community".
    OpenManage(PeerId),
    /// User clicked "Pending Requests".
    OpenPendingRequests(PeerId),
    /// User clicked "Add Chat".
    OpenAddChat(PeerId),
    /// Toggled "Show as one chat" collapsed grouping.
    CollapsedToggled(bool),
    /// Removed a linked peer from the community.
    PeerRemoved(PeerId),
    /// Display a toast message.
    Toast(String),
}
