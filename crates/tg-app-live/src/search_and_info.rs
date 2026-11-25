//! Live global & in-chat search, top peers, channel/chat members, common groups,
//! shared media pagination, message reporting, and unpin-all for [`LiveSession`].
//!
//! Port origin:
//! - `Telegram/SourceFiles/dialogs/dialogs_widget.cpp` (global & in-chat search pagination)
//! - `Telegram/SourceFiles/api/api_messages_search.cpp` (in-chat search)
//! - `Telegram/SourceFiles/api/api_peer_search.cpp` (`contacts.search`)
//! - `Telegram/SourceFiles/data/components/top_peers.cpp` (`contacts.getTopPeers`, `contacts.resetTopPeerRating`)
//! - `Telegram/SourceFiles/data/data_search_controller.cpp` (bidirectional `messages.search` & filters)
//! - `Telegram/SourceFiles/info/media/info_media_provider.cpp` (shared media tabs & pagination)
//! - `Telegram/SourceFiles/info/profile/info_profile_members.cpp` & `api/api_chat_participants.cpp`
//! - `Telegram/SourceFiles/info/common_groups/info_common_groups_inner_widget.cpp`
//! - `Telegram/SourceFiles/api/api_report.cpp` & `boxes/report_messages_box.cpp`
//! - `Telegram/SourceFiles/window/window_peer_menu.cpp` (`messages.unpinAllMessages`)

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::convert::{
    admin_rights_from_tl, apply_tl_chat_full, apply_tl_message, apply_tl_users_and_chats,
    banned_rights_from_tl, input_channel_for, input_peer_for, input_user_for, peer_id_from_tl,
};
use tg_api::{InvocationError, LoadDirection, RpcClient};
use tg_data::{
    ChatAdminRights, ChatFlags, DialogKey, FullMsgId as DataFullMsgId, MessageContent,
    MessageFlags, MessageMedia, MsgId as DataMsgId, Peer, PeerId as DataPeerId, SERVER_MAX_MSG_ID,
    Session, UserId as DataUserId,
};
use tg_media::transfer::download::CancelToken;
use tg_tl::{RemoteCall, enums as tl_enums, functions as tl_fns, types as tl_types};
use tracing::{debug, warn};

use crate::bridge::LiveBridgeState;
use crate::convert::{
    dialog_item_from_session, from_data_msg, from_data_peer, history_messages_ordered,
    message_to_history_message, message_to_info_media_item, now_unix_i32, peer_dialog_kind,
    peer_display_title, peer_primary_username, peer_status_text, to_data_msg, to_data_peer,
    ui_text_to_data_text, ui_text_to_tl_entities, unix_to_local_datetime,
};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use crate::upload::{LiveUploadConfig, upload_file_bytes_async};
use tg_app_core::core_types::dto::calendar::{
    CalendarDayInfo, month_end_unix_utc, month_start_unix_utc,
};
use tg_app_core::core_types::dto::edit_caption::EditMediaCaptionRequest;
use tg_app_core::core_types::dto::global_media::{GlobalMediaEntry, GlobalMediaPage};
use tg_app_core::dialogs::backend::{
    ChatTypeFilter, DialogListQuery, GlobalPeerResult, HashtagSuggestion, MessageSearchResult,
    PostsSearchFloodInfo, SearchRequest, SearchResults, SearchTab, SpecialRowKind, TopPeerItem,
};
use tg_app_core::history::backend::{HistoryMessage, PinnedMessageSummary};
use tg_app_core::history::sections_model::{ContactAttachment, MessageReportReason, VenueItem};
use tg_app_core::info::backend::{
    CommonGroupEntry, MediaItem as InfoMediaItem, MemberEntry, MemberRole, SharedMediaTab,
};
use tg_app_core::stand_in::types::{FilterId, MsgId, PeerId};

/// Default timeout for synchronous search & info RPC invocations.
const SEARCH_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Default messages per page for global and in-chat text search (`kSearchPerPage = 50`).
pub const SEARCH_PER_PAGE: i32 = 50;

/// Default shared media page size (`kSharedMediaLimit = 100` in `data_search_controller.cpp`).
pub const SHARED_MEDIA_PER_PAGE: i32 = 100;

/// Default top peers request limit (`kLimit = 64` in `data/components/top_peers.cpp`).
pub const TOP_PEERS_LIMIT: i32 = 64;

/// Default channel participants page size (`kParticipantsPerPage = 200` in `edit_participants_box.cpp`).
pub const PARTICIPANTS_PER_PAGE: i32 = 200;

/// Default common groups page size (`kCommonGroupsPerPage = 40` in `info_common_groups_inner_widget.cpp`).
pub const COMMON_GROUPS_PER_PAGE: i32 = 40;

/// Default typed peer search limit (`kTypedLimit = 50` in `api_peer_search.cpp`).
const PEER_SEARCH_TYPED_LIMIT: i32 = 50;

/// Default general peer search limit (`SearchPeopleLimit = 10` in `api_peer_search.cpp`).
const PEER_SEARCH_DEFAULT_LIMIT: i32 = 10;

/// Maximum 32-bit `offset_id` accepted by `messages.search` (`0x3FFFFFFF`).
const MAX_OFFSET_ID: i64 = 0x3FFF_FFFF;

/// Safety bound for iterative `messages.unpinAllMessages` batches.
const MAX_UNPIN_BATCHES: usize = 64;

/// Safety bound for multi-step option-based `messages.report` traversal.
const MAX_REPORT_STEPS: usize = 8;

// =============================================================================
// Public domain types for Search, Top Peers, Participants, Common Groups, Report
// =============================================================================

/// Filter passed to `messages.search` and `messages.searchGlobal`
/// (`Storage::SharedMediaType` / `Api::PrepareSearchFilter` in `data_search_controller.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SharedMediaFilter {
    /// No media filter (`inputMessagesFilterEmpty`).
    #[default]
    Empty,
    /// Photos only (`inputMessagesFilterPhotos`).
    Photos,
    /// Videos only (`inputMessagesFilterVideo`).
    Videos,
    /// Photos and videos (`inputMessagesFilterPhotoVideo`).
    PhotoVideo,
    /// Documents / files (`inputMessagesFilterDocument`).
    Files,
    /// Shared URLs / web links (`inputMessagesFilterUrl`).
    Links,
    /// Shared music tracks (`inputMessagesFilterMusic`).
    Music,
    /// Voice messages only (`inputMessagesFilterVoice`).
    Voice,
    /// Voice and round video messages (`inputMessagesFilterRoundVoice`).
    RoundVoice,
    /// Round video messages only (`inputMessagesFilterRoundVideo`).
    RoundVideo,
    /// GIF animations (`inputMessagesFilterGif`).
    Gifs,
    /// Chat profile photos (`inputMessagesFilterChatPhotos`).
    ChatPhotos,
    /// Pinned messages (`inputMessagesFilterPinned`).
    Pinned,
    /// Polls (`inputMessagesFilterPoll`).
    Poll,
}

impl SharedMediaFilter {
    /// Converts a UI [`SharedMediaTab`] to its corresponding Telegram `MessagesFilter`.
    #[must_use]
    pub const fn from_tab(tab: SharedMediaTab) -> Self {
        match tab {
            SharedMediaTab::PhotosVideos => Self::PhotoVideo,
            SharedMediaTab::Files => Self::Files,
            SharedMediaTab::Links => Self::Links,
            SharedMediaTab::Voice => Self::RoundVoice,
            SharedMediaTab::Music => Self::Music,
            SharedMediaTab::Gifs => Self::Gifs,
        }
    }

    /// Converts this filter to a Layer-233 TL `MessagesFilter`.
    #[must_use]
    pub const fn to_tl(self) -> tl_enums::MessagesFilter {
        match self {
            Self::Empty => tl_enums::MessagesFilter::InputMessagesFilterEmpty,
            Self::Photos => tl_enums::MessagesFilter::InputMessagesFilterPhotos,
            Self::Videos => tl_enums::MessagesFilter::InputMessagesFilterVideo,
            Self::PhotoVideo => tl_enums::MessagesFilter::InputMessagesFilterPhotoVideo,
            Self::Files => tl_enums::MessagesFilter::InputMessagesFilterDocument,
            Self::Links => tl_enums::MessagesFilter::InputMessagesFilterUrl,
            Self::Music => tl_enums::MessagesFilter::InputMessagesFilterMusic,
            Self::Voice => tl_enums::MessagesFilter::InputMessagesFilterVoice,
            Self::RoundVoice => tl_enums::MessagesFilter::InputMessagesFilterRoundVoice,
            Self::RoundVideo => tl_enums::MessagesFilter::InputMessagesFilterRoundVideo,
            Self::Gifs => tl_enums::MessagesFilter::InputMessagesFilterGif,
            Self::ChatPhotos => tl_enums::MessagesFilter::InputMessagesFilterChatPhotos,
            Self::Pinned => tl_enums::MessagesFilter::InputMessagesFilterPinned,
            Self::Poll => tl_enums::MessagesFilter::InputMessagesFilterPoll,
        }
    }
}

/// Pagination cursor for `messages.searchGlobal` (`offset_rate`, `offset_peer`, `offset_id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GlobalSearchCursor {
    /// Server-provided `next_rate` from the previous `messages.messagesSlice` response (`0` on page 1).
    pub offset_rate: i32,
    /// Peer of the last returned message (`None` -> `inputPeerEmpty` on page 1).
    pub offset_peer: Option<PeerId>,
    /// Message ID of the last returned message (`MsgId(0)` on page 1).
    pub offset_id: MsgId,
}

impl GlobalSearchCursor {
    /// Returns `true` if this cursor represents the start of a search (first page).
    #[must_use]
    pub const fn is_start(&self) -> bool {
        self.offset_id.0 == 0 || self.offset_peer.is_none()
    }
}

/// Parameters for `messages.searchGlobal` (`dialogs_widget.cpp:3529-3585`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalSearchParams {
    /// Search query text.
    pub query: String,
    /// Chat type restriction (`users_only`, `groups_only`, `broadcasts_only`).
    pub chat_type_filter: ChatTypeFilter,
    /// Optional folder filter (`None` = all folders, `Some(0)` = main list, `Some(1)` = archive).
    pub folder_id: Option<i32>,
    /// Optional media filter (defaults to `Empty`).
    pub filter: SharedMediaFilter,
    /// Minimum Unix timestamp (`0` = unbounded).
    pub min_date: i32,
    /// Maximum Unix timestamp (`0` = unbounded).
    pub max_date: i32,
    /// Maximum results per page (`1..=100`, defaults to `SEARCH_PER_PAGE`).
    pub limit: i32,
}

impl Default for GlobalSearchParams {
    fn default() -> Self {
        Self {
            query: String::new(),
            chat_type_filter: ChatTypeFilter::All,
            folder_id: None,
            filter: SharedMediaFilter::Empty,
            min_date: 0,
            max_date: 0,
            limit: SEARCH_PER_PAGE,
        }
    }
}

/// Single page returned by [`LiveSession::search_global_page`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GlobalSearchPage {
    /// Messages returned in this page (server order, newest-first).
    pub messages: Vec<MessageSearchResult>,
    /// Total count reported by the server.
    pub total_count: usize,
    /// Cursor to pass to the next page request.
    pub next_cursor: GlobalSearchCursor,
    /// `true` when additional pages can be requested.
    pub has_more: bool,
}

/// Parameters for in-chat `messages.search` (`api_messages_search.cpp` + `data_search_controller.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InChatSearchParams {
    /// Chat/channel peer to search inside.
    pub peer_id: PeerId,
    /// Query text (may be empty when `filter != SharedMediaFilter::Empty`).
    pub query: String,
    /// Media or pinned filter.
    pub filter: SharedMediaFilter,
    /// Optional sender filter (`from_id`, ignored for private 1:1 chats).
    pub from_peer: Option<PeerId>,
    /// Optional Saved Messages sublist peer (`saved_peer_id`).
    pub saved_peer: Option<PeerId>,
    /// Optional Saved Messages reaction tag filter (`saved_reaction`, `api_messages_search.cpp:89-95`).
    pub saved_reaction: Vec<tg_data::ReactionId>,
    /// Optional forum topic root message id (`top_msg_id`).
    pub top_msg_id: Option<MsgId>,
    /// Minimum Unix timestamp (`0` = unbounded).
    pub min_date: i32,
    /// Maximum Unix timestamp (`0` = unbounded).
    pub max_date: i32,
    /// Anchor message id (`MsgId(0)` = start from newest).
    pub around_id: MsgId,
    /// Pagination direction (`Before` = older, `Around` = centered, `After` = newer).
    pub direction: LoadDirection,
    /// Maximum items per page (`1..=100`).
    pub limit: i32,
}

impl InChatSearchParams {
    /// Creates default parameters for searching inside `peer_id` starting from newest messages.
    #[must_use]
    pub fn new(peer_id: PeerId, query: impl Into<String>, filter: SharedMediaFilter) -> Self {
        Self {
            peer_id,
            query: query.into(),
            filter,
            from_peer: None,
            saved_peer: None,
            saved_reaction: Vec::new(),
            top_msg_id: None,
            min_date: 0,
            max_date: 0,
            around_id: MsgId(0),
            direction: LoadDirection::Before,
            limit: SEARCH_PER_PAGE,
        }
    }
}

/// Single page returned by [`LiveSession::search_in_chat`] or [`LiveSession::load_pinned_messages_page`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InChatSearchPage {
    /// Matching messages ordered newest-first (descending `MsgId`).
    pub messages: Vec<HistoryMessage>,
    /// Total matching message count reported by the server.
    pub total_count: usize,
    /// Smallest message id in this page (use with `LoadDirection::Before` to load older).
    pub oldest_id: Option<MsgId>,
    /// Largest message id in this page (use with `LoadDirection::After` to load newer).
    pub newest_id: Option<MsgId>,
    /// `true` if older messages may exist before `oldest_id`.
    pub has_more_older: bool,
    /// `true` if newer messages may exist after `newest_id`.
    pub has_more_newer: bool,
    /// Contiguous `noSkipRange` lower bound (`data_search_controller.cpp`).
    pub no_skip_from: MsgId,
    /// Contiguous `noSkipRange` upper bound (`data_search_controller.cpp`).
    pub no_skip_till: MsgId,
}

/// Paginated shared media page returned by [`LiveSession::load_shared_media_page`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SharedMediaPage {
    /// Converted shared media items ordered newest-first.
    pub items: Vec<InfoMediaItem>,
    /// Total count reported by the server for this filter.
    pub total_count: usize,
    /// Smallest message id in this page (use with `LoadDirection::Before` to paginate older).
    pub oldest_id: Option<MsgId>,
    /// Largest message id in this page (use with `LoadDirection::After` to paginate newer).
    pub newest_id: Option<MsgId>,
    /// `true` if older items may exist.
    pub has_more_older: bool,
    /// `true` if newer items may exist.
    pub has_more_newer: bool,
}

/// Result of `contacts.search` (`api_peer_search.cpp`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeerSearchResults {
    /// Matching peers from the user's own contacts / chats (`my_results`).
    pub my_results: Vec<PeerId>,
    /// Matching global public peers (`results`).
    pub global_results: Vec<GlobalPeerResult>,
}

/// Top peer category selector (`TopPeerCategory` in `schema/api.tl` & `top_peers.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TopPeerCategoryKind {
    /// Frequently messaged users (`topPeerCategoryCorrespondents`).
    #[default]
    Correspondents,
    /// Frequently used bots in PM (`topPeerCategoryBotsPM`).
    BotsPm,
    /// Frequently used inline bots (`topPeerCategoryBotsInline`).
    BotsInline,
    /// Frequently called users (`topPeerCategoryPhoneCalls`).
    PhoneCalls,
    /// Users frequently forwarded to (`topPeerCategoryForwardUsers`).
    ForwardUsers,
    /// Chats frequently forwarded to (`topPeerCategoryForwardChats`).
    ForwardChats,
    /// Frequently visited groups (`topPeerCategoryGroups`).
    Groups,
    /// Frequently visited channels (`topPeerCategoryChannels`).
    Channels,
    /// Frequently opened bot mini apps (`topPeerCategoryBotsApp`).
    BotsApp,
    /// Guest chat bots (`topPeerCategoryBotsGuestChat`).
    BotsGuestChat,
}

impl TopPeerCategoryKind {
    /// Converts this category into its TL `TopPeerCategory` enum value.
    #[must_use]
    pub const fn to_tl(self) -> tl_enums::TopPeerCategory {
        match self {
            Self::Correspondents => tl_enums::TopPeerCategory::Correspondents,
            Self::BotsPm => tl_enums::TopPeerCategory::BotsPM,
            Self::BotsInline => tl_enums::TopPeerCategory::BotsInline,
            Self::PhoneCalls => tl_enums::TopPeerCategory::PhoneCalls,
            Self::ForwardUsers => tl_enums::TopPeerCategory::ForwardUsers,
            Self::ForwardChats => tl_enums::TopPeerCategory::ForwardChats,
            Self::Groups => tl_enums::TopPeerCategory::Groups,
            Self::Channels => tl_enums::TopPeerCategory::Channels,
            Self::BotsApp => tl_enums::TopPeerCategory::BotsApp,
            Self::BotsGuestChat => tl_enums::TopPeerCategory::BotsGuestChat,
        }
    }

    /// Returns `true` if a TL `TopPeerCategory` matches this category.
    #[must_use]
    pub const fn matches_tl(self, tl: &tl_enums::TopPeerCategory) -> bool {
        matches!(
            (self, tl),
            (
                Self::Correspondents,
                tl_enums::TopPeerCategory::Correspondents
            ) | (Self::BotsPm, tl_enums::TopPeerCategory::BotsPM)
                | (Self::BotsInline, tl_enums::TopPeerCategory::BotsInline)
                | (Self::PhoneCalls, tl_enums::TopPeerCategory::PhoneCalls)
                | (Self::ForwardUsers, tl_enums::TopPeerCategory::ForwardUsers)
                | (Self::ForwardChats, tl_enums::TopPeerCategory::ForwardChats)
                | (Self::Groups, tl_enums::TopPeerCategory::Groups)
                | (Self::Channels, tl_enums::TopPeerCategory::Channels)
                | (Self::BotsApp, tl_enums::TopPeerCategory::BotsApp)
                | (
                    Self::BotsGuestChat,
                    tl_enums::TopPeerCategory::BotsGuestChat
                )
        )
    }
}

/// Filter for `channels.getParticipants` (`ChannelParticipantsFilter` in `schema/api.tl`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum ParticipantFilter {
    /// Recent participants (`channelParticipantsRecent`).
    #[default]
    Recent,
    /// Channel/supergroup administrators (`channelParticipantsAdmins`).
    Admins,
    /// Bot participants (`channelParticipantsBots`).
    Bots,
    /// Kicked / removed users (`channelParticipantsKicked(q)`).
    Kicked(String),
    /// Restricted / banned users (`channelParticipantsBanned(q)`).
    Banned(String),
    /// Search participants by name/username (`channelParticipantsSearch(q)`).
    Search(String),
    /// Contacts in the channel (`channelParticipantsContacts(q)`).
    Contacts(String),
}

impl ParticipantFilter {
    /// Converts this filter to a Layer-233 TL `ChannelParticipantsFilter`.
    #[must_use]
    pub fn to_tl(&self) -> tl_enums::ChannelParticipantsFilter {
        match self {
            Self::Recent => tl_enums::ChannelParticipantsFilter::ChannelParticipantsRecent,
            Self::Admins => tl_enums::ChannelParticipantsFilter::ChannelParticipantsAdmins,
            Self::Bots => tl_enums::ChannelParticipantsFilter::ChannelParticipantsBots,
            Self::Kicked(q) => tl_enums::ChannelParticipantsFilter::ChannelParticipantsKicked(
                tl_types::ChannelParticipantsKicked { q: q.clone() },
            ),
            Self::Banned(q) => tl_enums::ChannelParticipantsFilter::ChannelParticipantsBanned(
                tl_types::ChannelParticipantsBanned { q: q.clone() },
            ),
            Self::Search(q) => tl_enums::ChannelParticipantsFilter::ChannelParticipantsSearch(
                tl_types::ChannelParticipantsSearch { q: q.clone() },
            ),
            Self::Contacts(q) => tl_enums::ChannelParticipantsFilter::ChannelParticipantsContacts(
                tl_types::ChannelParticipantsContacts { q: q.clone() },
            ),
        }
    }
}

/// Paginated page of channel/supergroup participants from `channels.getParticipants`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParticipantsPage {
    /// Converted member rows.
    pub members: Vec<MemberEntry>,
    /// Total participant count reported by the server for this filter.
    pub total_count: usize,
    /// Offset to pass to the next page request.
    pub next_offset: i32,
    /// `true` if more participants can be fetched.
    pub has_more: bool,
}

/// Paginated page of common groups from `messages.getCommonChats`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommonGroupsPage {
    /// Common groups returned in this page (excluding migrated legacy basic chats).
    pub groups: Vec<CommonGroupEntry>,
    /// Total count reported by `messages.chatsSlice`, if present.
    pub total_count: Option<usize>,
    /// Bare chat/channel id of the last returned item (`max_id` for the next page).
    pub next_max_id: i64,
    /// `true` if more pages may exist.
    pub has_more: bool,
}

/// One server-supplied report choice in the Layer-233 option-based `messages.report` flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportOption {
    /// Opaque option identifier bytes echoed back in the next `messages.report` call.
    pub option: Vec<u8>,
    /// Localized display label returned by the server.
    pub text: String,
}

/// Outcome of one step of the option-based `messages.report` flow (`api/api_report.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportStepResult {
    /// Server asks the user to choose one of the presented options.
    ChooseOption {
        /// Prompt title shown in the report dialog.
        title: String,
        /// Selectable report sub-options.
        options: Vec<ReportOption>,
    },
    /// Server asks for an optional or required free-text comment before finalizing.
    AddComment {
        /// Whether leaving the comment empty is permitted.
        optional: bool,
        /// Opaque option identifier to send alongside the comment.
        option: Vec<u8>,
    },
    /// Report was accepted and completed (`reportResultReported`).
    Reported,
    /// Server returned `MESSAGE_ID_REQUIRED`, requesting explicit message selection.
    MessageIdRequired,
}

// =============================================================================
// Per-session state registry (keyed by LiveBridgeState pointer, auto-pruned)
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct GlobalSearchCacheKey {
    query: String,
    folder_id: Option<i32>,
    chat_type_filter: ChatTypeFilter,
}

#[derive(Default)]
struct GlobalSearchCacheEntry {
    messages: Vec<MessageSearchResult>,
    seen: HashSet<(PeerId, MsgId)>,
    total_count: usize,
    cursor: GlobalSearchCursor,
    exhausted: bool,
    rpc_ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct InChatCacheKey {
    peer_id: PeerId,
    query: String,
    filter: SharedMediaFilter,
    selected_tag: Option<String>,
}

#[derive(Default)]
struct InChatCacheEntry {
    /// Ordered oldest -> newest by message id; UI reads in reverse (newest-first).
    message_ids: BTreeMap<i32, DataFullMsgId>,
    total_count: usize,
    oldest_id: Option<MsgId>,
    exhausted: bool,
    loaded: bool,
    rpc_ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PeerSearchCacheKey {
    query: String,
    tab: SearchTab,
}

#[derive(Default)]
struct MembersCacheEntry {
    members: Vec<MemberEntry>,
    total_count: usize,
    next_offset: i32,
    exhausted: bool,
}

#[derive(Default)]
struct CommonGroupsCacheEntry {
    groups: Vec<CommonGroupEntry>,
    seen_peers: HashSet<PeerId>,
    last_max_id: i64,
    exhausted: bool,
    loaded: bool,
}

#[derive(Default)]
struct PinnedCacheEntry {
    msg_ids: Vec<MsgId>,
    loaded: bool,
    rpc_ok: bool,
}

/// Single page returned by [`LiveSession::search_public_posts_page`] (`channels.searchPosts`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PublicPostsPage {
    /// Matching public channel posts, newest-first.
    pub messages: Vec<MessageSearchResult>,
    /// Total matching count reported by the server.
    pub total_count: usize,
    /// Pagination cursor for the next page.
    pub next_cursor: GlobalSearchCursor,
    /// `true` when additional pages can be requested.
    pub has_more: bool,
    /// Updated daily quota / Stars flood info returned in `messages.messagesSlice.search_flood`.
    pub flood: Option<PostsSearchFloodInfo>,
}

/// Default inline bot username used by tdesktop for nearby venue search (`appConfig.venue_search_username`).
pub const DEFAULT_VENUE_BOT_USERNAME: &str = "foursquare";

#[derive(Default)]
struct PublicPostsCacheEntry {
    messages: Vec<MessageSearchResult>,
    seen: HashSet<(PeerId, MsgId)>,
    total_count: usize,
    cursor: GlobalSearchCursor,
    flood: Option<PostsSearchFloodInfo>,
    exhausted: bool,
    rpc_ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct GlobalMediaCacheKey {
    tab: SharedMediaTab,
    query: String,
}

#[derive(Default)]
struct GlobalMediaCacheEntry {
    entries: Vec<GlobalMediaEntry>,
    seen: HashSet<(PeerId, i32)>,
    total_count: usize,
    cursor: GlobalSearchCursor,
    exhausted: bool,
    rpc_ok: bool,
}

#[derive(Default)]
struct SearchAndInfoState {
    global_search: HashMap<GlobalSearchCacheKey, GlobalSearchCacheEntry>,
    in_chat_search: HashMap<InChatCacheKey, InChatCacheEntry>,
    peer_search: HashMap<PeerSearchCacheKey, PeerSearchResults>,
    top_peers_loaded: bool,
    top_peers_disabled: bool,
    top_peers: Vec<(PeerId, f64)>,
    removed_top_peers: HashSet<PeerId>,
    members: HashMap<(PeerId, ParticipantFilter), MembersCacheEntry>,
    common_groups: HashMap<PeerId, CommonGroupsCacheEntry>,
    pinned: HashMap<(PeerId, Option<MsgId>), PinnedCacheEntry>,
    calendar: HashMap<(PeerId, i32, u32), Vec<CalendarDayInfo>>,
    public_posts: HashMap<String, PublicPostsCacheEntry>,
    public_posts_flood: Option<PostsSearchFloodInfo>,
    global_media: HashMap<GlobalMediaCacheKey, GlobalMediaCacheEntry>,
    contacts_loaded: bool,
    contacts: Vec<ContactAttachment>,
    venues: HashMap<String, Vec<VenueItem>>,
}

struct StateRegistryEntry {
    owner: Weak<LiveBridgeState>,
    state: SearchAndInfoState,
}

fn state_registry() -> &'static Mutex<HashMap<usize, StateRegistryEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, StateRegistryEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_state<R>(live: &LiveSession, f: impl FnOnce(&mut SearchAndInfoState) -> R) -> R
where
{
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let mut map = state_registry().lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    let entry = map.entry(key).or_insert_with(|| StateRegistryEntry {
        owner: Arc::downgrade(live.bridge_state()),
        state: SearchAndInfoState::default(),
    });
    f(&mut entry.state)
}

// =============================================================================
// Internal session & RPC helpers
// =============================================================================

fn read_session<R>(live: &LiveSession, f: impl FnOnce(&Session) -> R + Send) -> R
where
    R: Send,
{
    let lock = live.runtime().session();
    if let Ok(guard) = lock.try_read() {
        return f(&guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let guard = arc.read().await;
        f(&guard)
    })
}

fn write_session<R>(live: &LiveSession, f: impl FnOnce(&mut Session) -> R + Send) -> R
where
    R: Send,
{
    let lock = live.runtime().session();
    if let Ok(mut guard) = lock.try_write() {
        return f(&mut guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let mut guard = arc.write().await;
        f(&mut guard)
    })
}

fn invoke_rpc_raw<R>(live: &LiveSession, req: R) -> Result<R::Return, InvocationError>
where
    R: RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
{
    let rpc = live.runtime().rpc().clone();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        match tokio::time::timeout(SEARCH_RPC_TIMEOUT, rpc.invoke(&req)).await {
            Ok(Ok(val)) => Ok(val),
            Ok(Err(err)) => Err(err),
            Err(_) => Err(InvocationError::Dropped),
        }
    })
}

fn invoke_rpc<R>(live: &LiveSession, req: R) -> Result<R::Return, String>
where
    R: RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
{
    let rpc = live.runtime().rpc().clone();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(SEARCH_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
    })
}

fn input_peer(live: &LiveSession, peer_id: PeerId) -> Option<tl_enums::InputPeer> {
    let data_peer = to_data_peer(peer_id);
    let peer = read_session(live, |s| input_peer_for(s, data_peer));
    if matches!(peer, tl_enums::InputPeer::Empty) {
        None
    } else {
        Some(peer)
    }
}

fn input_channel(live: &LiveSession, peer_id: PeerId) -> Option<tl_enums::InputChannel> {
    let data_peer = to_data_peer(peer_id);
    let ch_id = data_peer.to_channel()?;
    let ch = read_session(live, |s| input_channel_for(s, ch_id));
    if matches!(ch, tl_enums::InputChannel::Empty) {
        None
    } else {
        Some(ch)
    }
}

fn input_user(live: &LiveSession, user_id: PeerId) -> Option<tl_enums::InputUser> {
    let data_peer = to_data_peer(user_id);
    let uid = data_peer.to_user()?;
    let user = read_session(live, |s| input_user_for(s, uid));
    if matches!(user, tl_enums::InputUser::Empty) {
        None
    } else {
        Some(user)
    }
}

fn is_rpc_error_named(err: &InvocationError, name: &str) -> bool {
    matches!(err, InvocationError::Rpc(rpc) if rpc.name == name)
}

/// Computes Telegram's 64-bit xorshift list hash (`Api::HashInit`/`HashUpdate`/`HashFinalize`
/// in `api/api_hash.h` and `TopPeers::countHash` in `data/components/top_peers.cpp`).
#[must_use]
pub fn compute_top_peers_hash(ids: &[u64]) -> i64 {
    let mut hash: u64 = 0;
    for &value in ids.iter().take(TOP_PEERS_LIMIT as usize) {
        hash ^= hash >> 21;
        hash ^= hash << 35;
        hash ^= hash >> 4;
        hash = hash.wrapping_add(value);
    }
    hash as i64
}

/// Normalizes a peer search query by trimming whitespace and stripping a leading `@`
/// (`Api::ConvertPeerSearchQuery` in `api_peer_search.cpp`).
#[must_use]
pub fn convert_peer_search_query(query: &str) -> String {
    let trimmed = query.trim();
    trimmed.strip_prefix('@').unwrap_or(trimmed).to_owned()
}

/// Computes `messages.search` `offset_id` and `add_offset` for bidirectional pagination
/// (`Api::PrepareSearchRequest` in `data/data_search_controller.cpp:159-181`).
#[must_use]
pub fn prepare_search_offsets(
    around_id: MsgId,
    direction: LoadDirection,
    limit: i32,
) -> (i32, i32) {
    let clamped_limit = limit.clamp(1, 100);
    let raw_offset_id: i64 = match direction {
        LoadDirection::Before | LoadDirection::Around => i64::from(around_id.0),
        LoadDirection::After => i64::from(around_id.0).saturating_add(1),
    };
    let offset_id = raw_offset_id.clamp(0, MAX_OFFSET_ID) as i32;
    let add_offset = match direction {
        LoadDirection::Before => 0,
        LoadDirection::Around => -clamped_limit / 2,
        LoadDirection::After => -clamped_limit,
    };
    (offset_id, add_offset)
}

pub(crate) fn message_to_search_result(
    session: &Session,
    msg: &tg_data::Message,
) -> Option<MessageSearchResult> {
    let peer = session.peer(msg.peer_id);
    let peer_title = peer_display_title(session, msg.peer_id);
    let kind = peer.map_or(
        tg_app_core::dialogs::backend::DialogPeerKind::User,
        peer_dialog_kind,
    );
    let special = if msg.peer_id == session.self_peer_id() {
        SpecialRowKind::SavedMessages
    } else {
        SpecialRowKind::None
    };
    let color_index = peer.map_or(0, |p| p.common().color_index % 7);
    let archived = session
        .history(msg.peer_id)
        .is_some_and(|h| h.folder_id() == Some(tg_data::ARCHIVE_FOLDER_ID));
    if matches!(msg.content, MessageContent::Empty) {
        return None;
    }
    let text = crate::preview_text::message_preview_text(session, msg);
    let sender_name = crate::preview_text::preview_sender(session, msg);
    Some(MessageSearchResult {
        peer_id: from_data_peer(msg.peer_id),
        kind,
        special,
        peer_title,
        color_index,
        msg_id: from_data_msg(msg.id),
        sender_name,
        text,
        time: unix_to_local_datetime(msg.date),
        archived,
    })
}

fn tl_message_id_and_peer(msg: &tl_enums::Message) -> (MsgId, Option<DataPeerId>) {
    match msg {
        tl_enums::Message::Message(m) => (MsgId(m.id), Some(peer_id_from_tl(&m.peer_id))),
        tl_enums::Message::Service(s) => (MsgId(s.id), Some(peer_id_from_tl(&s.peer_id))),
        tl_enums::Message::Empty(e) => (MsgId(e.id), e.peer_id.as_ref().map(peer_id_from_tl)),
    }
}

fn build_global_peer_result(
    session: &Session,
    pid: DataPeerId,
    now: i32,
) -> Option<GlobalPeerResult> {
    let peer = session.peer(pid)?;
    let username = peer_primary_username(peer).unwrap_or_default();
    let (status, _) = peer_status_text(peer, now);
    let subtitle = if username.is_empty() {
        status
    } else {
        format!("@{username}, {status}")
    };
    let verified = match peer {
        Peer::User(u) => u.is_verified(),
        Peer::Channel(ch) => ch.is_verified(),
        Peer::Chat(_) => false,
    };
    Some(GlobalPeerResult {
        peer_id: from_data_peer(pid),
        kind: peer_dialog_kind(peer),
        title: peer.name().to_owned(),
        username,
        subtitle,
        color_index: peer.common().color_index % 7,
        verified,
        premium: peer.as_user().is_some_and(|u| u.is_premium()),
    })
}

/// `PeerListRow::refreshStatus` for a member row: `Data::OnlineText`, where the current account is
/// always online while the app is open (`ApiWrap::updateOnline` marks the self user online).
fn member_status_text(session: &Session, peer: &Peer, now: i32) -> (String, bool) {
    if peer.id() == session.self_peer_id() && peer.as_user().is_some() {
        return (tg_lang::tr::lng_status_online(), true);
    }
    peer_status_text(peer, now)
}

pub(crate) fn build_member_entry(
    session: &Session,
    pid: DataPeerId,
    role: MemberRole,
    now: i32,
) -> MemberEntry {
    if let Some(peer) = session.peer(pid) {
        let (status_text, is_online) = member_status_text(session, peer, now);
        let is_bot = peer.as_user().is_some_and(|u| u.is_bot());
        MemberEntry {
            peer_id: from_data_peer(pid),
            name: crate::preview_text::peer_real_name(session, pid),
            username: peer_primary_username(peer),
            status_text,
            is_online,
            is_bot,
            role,
            color_index: peer.common().color_index % 7,
        }
    } else {
        MemberEntry {
            peer_id: from_data_peer(pid),
            name: format!("User {}", pid.0),
            username: None,
            status_text: String::new(),
            is_online: false,
            is_bot: false,
            role,
            color_index: (pid.0.unsigned_abs() % 7) as u8,
        }
    }
}

fn member_role_sort_key(entry: &MemberEntry) -> (u8, u8, String) {
    let role_rank = match entry.role {
        MemberRole::Owner { .. } => 0,
        MemberRole::Admin { .. } => 1,
        MemberRole::Member => 2,
        MemberRole::Restricted { .. } => 3,
    };
    let online_rank = if entry.is_online { 0 } else { 1 };
    (role_rank, online_rank, entry.name.to_ascii_lowercase())
}

// =============================================================================
// Public API methods on LiveSession
// =============================================================================

impl LiveSession {
    /// Clears all cached search, top peers, participants, common groups, shared media,
    /// and pinned-messages responses for this session.
    pub fn invalidate_search_and_info_caches(&self) {
        with_state(self, |st| {
            st.global_search.clear();
            st.in_chat_search.clear();
            st.peer_search.clear();
            st.top_peers_loaded = false;
            st.members.clear();
            st.common_groups.clear();
            st.pinned.clear();
        });
    }

    /// Invalidates cached pinned messages for `peer_id` (e.g. after pinning or unpinning).
    pub fn invalidate_pinned_cache(&self, peer_id: PeerId) {
        with_state(self, |st| {
            st.pinned.retain(|&(pid, _), _| pid != peer_id);
        });
    }

    /// Invalidates cached members for `peer_id` (e.g. after adding/removing/promoting members).
    pub fn invalidate_members_cache(&self, peer_id: PeerId) {
        with_state(self, |st| {
            st.members.retain(|&(pid, _), _| pid != peer_id);
        });
    }

    // -------------------------------------------------------------------------
    // 1. Global search (`messages.searchGlobal`)
    // -------------------------------------------------------------------------

    /// Executes one paginated `messages.searchGlobal` RPC with `offset_rate` / `offset_peer` /
    /// `offset_id`, folder filter, chat type filter, media filter, and date range filters
    /// (`dialogs/dialogs_widget.cpp:3529-3694`).
    pub fn search_global_page(
        &self,
        params: &GlobalSearchParams,
        cursor: GlobalSearchCursor,
    ) -> Result<GlobalSearchPage, String> {
        let query = params.query.trim();
        if query.is_empty()
            && params.filter == SharedMediaFilter::Empty
            && params.min_date == 0
            && params.max_date == 0
        {
            return Ok(GlobalSearchPage::default());
        }

        let offset_peer = if cursor.is_start() {
            tl_enums::InputPeer::Empty
        } else {
            cursor
                .offset_peer
                .and_then(|pid| input_peer(self, pid))
                .unwrap_or(tl_enums::InputPeer::Empty)
        };
        let offset_rate = if cursor.is_start() {
            0
        } else {
            cursor.offset_rate
        };
        let offset_id = if cursor.is_start() {
            0
        } else {
            cursor.offset_id.0
        };

        let req = tl_fns::messages::SearchGlobal {
            broadcasts_only: matches!(params.chat_type_filter, ChatTypeFilter::Channels),
            groups_only: matches!(params.chat_type_filter, ChatTypeFilter::Groups),
            users_only: matches!(params.chat_type_filter, ChatTypeFilter::Private),
            folder_id: params.folder_id,
            community: None,
            q: query.to_owned(),
            filter: params.filter.to_tl(),
            min_date: params.min_date,
            max_date: params.max_date,
            offset_rate,
            offset_peer,
            offset_id,
            limit: params.limit.clamp(1, 100),
        };

        let resp = match invoke_rpc_raw(self, req) {
            Ok(resp) => resp,
            Err(err) if is_rpc_error_named(&err, "SEARCH_QUERY_EMPTY") => {
                return Ok(GlobalSearchPage::default());
            }
            Err(err) => return Err(err.to_string()),
        };

        let (tl_messages, tl_users, tl_chats, total_count, next_rate, is_complete_variant) =
            match resp {
                tl_enums::messages::Messages::Messages(d) => {
                    let total = d.messages.len();
                    (d.messages, d.users, d.chats, total, None, true)
                }
                tl_enums::messages::Messages::Slice(d) => (
                    d.messages,
                    d.users,
                    d.chats,
                    d.count.max(0) as usize,
                    d.next_rate,
                    false,
                ),
                tl_enums::messages::Messages::ChannelMessages(d) => (
                    d.messages,
                    d.users,
                    d.chats,
                    d.count.max(0) as usize,
                    None,
                    false,
                ),
                tl_enums::messages::Messages::NotModified(_) => {
                    return Ok(GlobalSearchPage::default());
                }
            };

        let mut last_peer: Option<PeerId> = None;
        let mut last_id = MsgId(0);
        let mut inserted_ids: Vec<DataFullMsgId> = Vec::with_capacity(tl_messages.len());

        write_session(self, |session| {
            apply_tl_users_and_chats(session, &tl_users, &tl_chats);
            for tl_msg in &tl_messages {
                let (mid, mpeer) = tl_message_id_and_peer(tl_msg);
                last_id = mid;
                if let Some(dpid) = mpeer {
                    last_peer = Some(from_data_peer(dpid));
                }
                if let Some(full_id) = apply_tl_message(session, tl_msg) {
                    inserted_ids.push(full_id);
                }
            }
        });

        let messages = read_session(self, |session| {
            inserted_ids
                .iter()
                .filter_map(|&fid| session.message(fid))
                .filter_map(|m| message_to_search_result(session, m))
                .collect::<Vec<_>>()
        });

        // Ported from `dialogs_widget.cpp:3668-3679`:
        // For `messages.messagesSlice`, continue only while `next_rate` advances and messages
        // were returned; `messages.messages` always signals the final/complete page.
        let (effective_rate, has_more) = if is_complete_variant || messages.is_empty() {
            (offset_rate, false)
        } else if let Some(nr) = next_rate {
            if nr != offset_rate {
                (nr, last_peer.is_some() && last_id.0 != 0)
            } else {
                (offset_rate, false)
            }
        } else {
            (offset_rate, false)
        };

        let next_cursor = GlobalSearchCursor {
            offset_rate: effective_rate,
            offset_peer: last_peer,
            offset_id: last_id,
        };

        Ok(GlobalSearchPage {
            messages,
            total_count,
            next_cursor,
            has_more,
        })
    }

    // -------------------------------------------------------------------------
    // 2. In-chat search, Shared Media tabs, and Pinned filter (`messages.search`)
    // -------------------------------------------------------------------------

    /// Executes one bidirectional `messages.search` RPC inside a single chat/channel
    /// (`api/api_messages_search.cpp` + `data/data_search_controller.cpp`).
    ///
    /// Supports text queries, all shared media filters, `InputMessagesFilterPinned`,
    /// sender/sublist/topic scoping, date range bounds, and `Before` / `Around` / `After`
    /// pagination.
    pub fn search_in_chat(&self, params: &InChatSearchParams) -> Result<InChatSearchPage, String> {
        let query = params.query.trim();
        if query.is_empty()
            && params.filter == SharedMediaFilter::Empty
            && params.from_peer.is_none()
            && params.saved_peer.is_none()
            && params.saved_reaction.is_empty()
            && params.min_date == 0
            && params.max_date == 0
        {
            return Ok(InChatSearchPage::default());
        }
        if params.top_msg_id.is_some() && params.filter == SharedMediaFilter::ChatPhotos {
            // `messages.search` rejects `inputMessagesFilterChatPhotos` with `top_msg_id`
            // (`data_search_controller.cpp:152`).
            return Ok(InChatSearchPage::default());
        }

        let Some(peer) = input_peer(self, params.peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        let is_private_user = read_session(self, |s| {
            s.peer(to_data_peer(params.peer_id))
                .and_then(Peer::as_user)
                .is_some()
        });
        let from_id = if is_private_user {
            None
        } else {
            params.from_peer.and_then(|pid| input_peer(self, pid))
        };
        let saved_peer_id = params.saved_peer.and_then(|pid| input_peer(self, pid));
        // Ported from `Api::MessagesSearch::search` (`api/api_messages_search.cpp:89-95`):
        // include `saved_reaction` tags when searching Saved Messages by reaction tag.
        let saved_reaction = (!params.saved_reaction.is_empty()).then(|| {
            params
                .saved_reaction
                .iter()
                .map(tg_api::convert::reaction_to_tl)
                .collect::<Vec<_>>()
        });

        let limit = params.limit.clamp(1, 100);
        let (offset_id, add_offset) =
            prepare_search_offsets(params.around_id, params.direction, limit);

        let req = tl_fns::messages::Search {
            peer,
            q: query.to_owned(),
            from_id,
            saved_peer_id,
            saved_reaction,
            top_msg_id: params.top_msg_id.map(|m| m.0),
            filter: params.filter.to_tl(),
            min_date: params.min_date,
            max_date: params.max_date,
            offset_id,
            add_offset,
            limit,
            max_id: 0,
            min_id: 0,
            hash: 0,
        };

        let resp = match invoke_rpc_raw(self, req) {
            Ok(resp) => resp,
            Err(err) if is_rpc_error_named(&err, "SEARCH_QUERY_EMPTY") => {
                return Ok(InChatSearchPage::default());
            }
            Err(err) => return Err(err.to_string()),
        };

        let (
            tl_messages,
            tl_topics,
            tl_users,
            tl_chats,
            total_count,
            channel_pts,
            is_complete_variant,
        ) = match resp {
            tl_enums::messages::Messages::Messages(d) => {
                let total = d.messages.len();
                (d.messages, Vec::new(), d.users, d.chats, total, None, true)
            }
            tl_enums::messages::Messages::Slice(d) => (
                d.messages,
                Vec::new(),
                d.users,
                d.chats,
                d.count.max(0) as usize,
                None,
                false,
            ),
            tl_enums::messages::Messages::ChannelMessages(d) => (
                d.messages,
                d.topics,
                d.users,
                d.chats,
                d.count.max(0) as usize,
                (d.pts > 0).then_some(d.pts),
                false,
            ),
            tl_enums::messages::Messages::NotModified(_) => {
                return Ok(InChatSearchPage::default());
            }
        };

        let data_peer = to_data_peer(params.peer_id);
        let now = now_unix_i32();
        let mut inserted: Vec<DataFullMsgId> = Vec::with_capacity(tl_messages.len());
        write_session(self, |session| {
            apply_tl_users_and_chats(session, &tl_users, &tl_chats);
            for tl_msg in &tl_messages {
                if let Some(fid) = apply_tl_message(session, tl_msg) {
                    if params.filter == SharedMediaFilter::Pinned {
                        let _ = session.set_message_pinned(fid, true);
                    }
                    inserted.push(fid);
                }
            }
            // Ported from `Api::MessagesSearch::searchReceived` (`api/api_messages_search.cpp:173-180`):
            // apply returned forum `topics` from `messages.channelMessages` into the session forum store.
            for tl_topic in &tl_topics {
                tg_api::forum::apply_tl_forum_topic(session, data_peer, tl_topic, now);
            }
        });
        // Ported from `Api::MessagesSearch::searchReceived` (`api/api_messages_search.cpp:197-199`):
        // feed `messages.channelMessages.pts` (`channel->ptsReceived(pts)`) into `SessionActor`'s
        // single `UpdatesHandler`.
        if let (Some(ch_id), Some(pts)) = (data_peer.to_channel(), channel_pts) {
            let runtime = Arc::clone(self.runtime());
            block_on_runtime(self.runtime().tokio_handle(), async move {
                let _ = runtime.apply_channel_pts(ch_id, pts, 0).await;
            });
        }

        let mut messages: Vec<HistoryMessage> = read_session(self, |session| {
            inserted
                .iter()
                .filter_map(|&fid| session.message(fid))
                .map(|m| message_to_history_message(session, m))
                .collect()
        });
        // Keep newest-first (descending MsgId) deterministically even if server order varies.
        messages.sort_by_key(|m| std::cmp::Reverse(m.id.0));
        messages.dedup_by_key(|m| m.id);

        let newest_id = messages.first().map(|m| m.id);
        let oldest_id = messages.last().map(|m| m.id);

        // Compute `noSkipRange` faithfully (`data_search_controller.cpp:211-291`).
        let mut no_skip_from = params.around_id.0;
        let mut no_skip_till = params.around_id.0;
        for m in &messages {
            if no_skip_from == 0 || m.id.0 < no_skip_from {
                no_skip_from = m.id.0;
            }
            if m.id.0 > no_skip_till {
                no_skip_till = m.id.0;
            }
        }
        let max_server_id = SERVER_MAX_MSG_ID.0.min(i64::from(i32::MAX)) as i32;
        if params.around_id.0 != 0 && messages.is_empty() {
            match params.direction {
                LoadDirection::Before => no_skip_from = 0,
                LoadDirection::Around => {
                    no_skip_from = 0;
                    no_skip_till = max_server_id;
                }
                LoadDirection::After => no_skip_till = max_server_id,
            }
        }

        let got_full_page = tl_messages.len() >= (limit as usize);
        let (has_more_older, has_more_newer) = if is_complete_variant {
            (false, false)
        } else if params.around_id.0 == 0 {
            // Starting from newest (`offset_id = 0`): nothing newer; more older if count/page suggests.
            (
                !messages.is_empty() && (got_full_page || messages.len() < total_count),
                false,
            )
        } else {
            match params.direction {
                LoadDirection::Before => (!messages.is_empty() && got_full_page, true),
                LoadDirection::After => (true, !messages.is_empty() && got_full_page),
                LoadDirection::Around => (!messages.is_empty(), !messages.is_empty()),
            }
        };

        Ok(InChatSearchPage {
            messages,
            total_count,
            oldest_id,
            newest_id,
            has_more_older,
            has_more_newer,
            no_skip_from: MsgId(no_skip_from),
            no_skip_till: MsgId(no_skip_till),
        })
    }

    /// Loads one paginated slice of shared media for `peer_id` and `tab` via `messages.search`
    /// (`info/media/info_media_provider.cpp` + `data/data_search_controller.cpp`).
    ///
    /// Pass `around_id = MsgId(0), direction = LoadDirection::Before` for the newest page,
    /// `oldest_id, LoadDirection::Before` to paginate older, or `newest_id, LoadDirection::After`
    /// to paginate newer.
    pub fn load_shared_media_page(
        &self,
        peer_id: PeerId,
        tab: SharedMediaTab,
        query: &str,
        around_id: MsgId,
        direction: LoadDirection,
        limit: i32,
    ) -> Result<SharedMediaPage, String> {
        let mut params = InChatSearchParams::new(peer_id, query, SharedMediaFilter::from_tab(tab));
        params.around_id = around_id;
        params.direction = direction;
        params.limit = limit.clamp(1, SHARED_MEDIA_PER_PAGE);

        let page = self.search_in_chat(&params)?;
        let data_peer = to_data_peer(peer_id);
        let items: Vec<InfoMediaItem> = read_session(self, |session| {
            page.messages
                .iter()
                .filter_map(|hm| session.message(DataFullMsgId::new(data_peer, to_data_msg(hm.id))))
                .filter_map(|m| message_to_info_media_item(session, m, tab))
                .collect()
        });

        Ok(SharedMediaPage {
            items,
            total_count: page.total_count,
            oldest_id: page.oldest_id,
            newest_id: page.newest_id,
            has_more_older: page.has_more_older,
            has_more_newer: page.has_more_newer,
        })
    }

    /// Loads one paginated slice of pinned messages for `peer_id` (optionally scoped to a
    /// forum topic) via `messages.search` with `InputMessagesFilterPinned`.
    pub fn load_pinned_messages_page(
        &self,
        peer_id: PeerId,
        topic_root_id: Option<MsgId>,
        around_id: MsgId,
        direction: LoadDirection,
        limit: i32,
    ) -> Result<InChatSearchPage, String> {
        let mut params = InChatSearchParams::new(peer_id, "", SharedMediaFilter::Pinned);
        params.top_msg_id = topic_root_id;
        params.around_id = around_id;
        params.direction = direction;
        params.limit = limit.clamp(1, 100);
        self.search_in_chat(&params)
    }

    // -------------------------------------------------------------------------
    // 3. Peer directory search (`contacts.search`) & Top Peers
    // -------------------------------------------------------------------------

    /// Searches Telegram's global peer directory via `contacts.search`
    /// (`api/api_peer_search.cpp`).
    pub fn search_peers(
        &self,
        query: &str,
        tab: SearchTab,
        limit: i32,
    ) -> Result<PeerSearchResults, String> {
        let clean = convert_peer_search_query(query);
        if clean.is_empty() || clean.starts_with('#') || clean.starts_with('$') {
            return Ok(PeerSearchResults::default());
        }
        let broadcasts = matches!(tab, SearchTab::Channels);
        let bots = matches!(tab, SearchTab::Apps);
        let effective_limit = if limit > 0 {
            limit
        } else if broadcasts || bots {
            PEER_SEARCH_TYPED_LIMIT
        } else {
            PEER_SEARCH_DEFAULT_LIMIT
        };

        let req = tl_fns::contacts::Search {
            broadcasts,
            bots,
            q: clean,
            limit: effective_limit.clamp(1, 100),
        };
        let tl_enums::contacts::Found::Found(found) = invoke_rpc(self, req)?;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &found.users, &found.chats);
        });
        let now = now_unix_i32();
        let out = read_session(self, |s| {
            let my_results = found
                .my_results
                .iter()
                .map(|p| from_data_peer(peer_id_from_tl(p)))
                .collect();
            let global_results = found
                .results
                .iter()
                .filter_map(|p| build_global_peer_result(s, peer_id_from_tl(p), now))
                .collect();
            PeerSearchResults {
                my_results,
                global_results,
            }
        });
        Ok(out)
    }

    /// Fetches top peers for `category` via `contacts.getTopPeers`
    /// (`data/components/top_peers.cpp:218-279`).
    pub fn fetch_top_peers(
        &self,
        category: TopPeerCategoryKind,
        offset: i32,
        limit: i32,
    ) -> Result<Vec<TopPeerItem>, String> {
        let existing_ids: Vec<u64> = with_state(self, |st| {
            st.top_peers
                .iter()
                .filter_map(|(pid, _)| to_data_peer(*pid).to_user().map(|u| u.0))
                .collect()
        });
        let hash = if offset == 0 && category == TopPeerCategoryKind::Correspondents {
            compute_top_peers_hash(&existing_ids)
        } else {
            0
        };

        let req = tl_fns::contacts::GetTopPeers {
            correspondents: category == TopPeerCategoryKind::Correspondents,
            bots_pm: category == TopPeerCategoryKind::BotsPm,
            bots_inline: category == TopPeerCategoryKind::BotsInline,
            phone_calls: category == TopPeerCategoryKind::PhoneCalls,
            forward_users: category == TopPeerCategoryKind::ForwardUsers,
            forward_chats: category == TopPeerCategoryKind::ForwardChats,
            groups: category == TopPeerCategoryKind::Groups,
            channels: category == TopPeerCategoryKind::Channels,
            bots_app: category == TopPeerCategoryKind::BotsApp,
            bots_guestchat: category == TopPeerCategoryKind::BotsGuestChat,
            offset: offset.max(0),
            limit: limit.clamp(1, 100),
            hash,
        };

        let resp = invoke_rpc(self, req)?;
        match resp {
            tl_enums::contacts::TopPeers::TopPeers(data) => {
                write_session(self, |s| {
                    apply_tl_users_and_chats(s, &data.users, &data.chats);
                });
                let mut pairs: Vec<(PeerId, f64)> = Vec::new();
                for cat in &data.categories {
                    let tl_enums::TopPeerCategoryPeers::TopPeerCategoryPeers(cat_data) = cat;
                    if !category.matches_tl(&cat_data.category) {
                        continue;
                    }
                    for tp in &cat_data.peers {
                        let tl_enums::TopPeer::TopPeer(entry) = tp;
                        let pid = from_data_peer(peer_id_from_tl(&entry.peer));
                        pairs.push((pid, entry.rating));
                    }
                }
                if category == TopPeerCategoryKind::Correspondents && offset == 0 {
                    with_state(self, |st| {
                        st.top_peers_loaded = true;
                        st.top_peers_disabled = false;
                        st.top_peers = pairs.clone();
                    });
                }
                Ok(build_top_peer_items(self, &pairs))
            }
            tl_enums::contacts::TopPeers::Disabled => {
                if category == TopPeerCategoryKind::Correspondents {
                    with_state(self, |st| {
                        st.top_peers_loaded = true;
                        st.top_peers_disabled = true;
                        st.top_peers.clear();
                    });
                }
                Ok(Vec::new())
            }
            tl_enums::contacts::TopPeers::NotModified => {
                let cached = with_state(self, |st| {
                    st.top_peers_loaded = true;
                    st.top_peers.clone()
                });
                Ok(build_top_peer_items(self, &cached))
            }
        }
    }

    /// Resets a peer's top-peer rating on Telegram via `contacts.resetTopPeerRating`
    /// (`data/components/top_peers.cpp:139-150`).
    pub fn reset_top_peer_rating(
        &self,
        category: TopPeerCategoryKind,
        peer_id: PeerId,
    ) -> Result<(), String> {
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        let req = tl_fns::contacts::ResetTopPeerRating {
            category: category.to_tl(),
            peer,
        };
        let _ = invoke_rpc(self, req)?;
        with_state(self, |st| {
            st.top_peers.retain(|&(pid, _)| pid != peer_id);
            st.removed_top_peers.insert(peer_id);
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    // -------------------------------------------------------------------------
    // 4. Channel participants & Basic group full-chat members
    // -------------------------------------------------------------------------

    /// Fetches a paginated slice of supergroup/channel participants via `channels.getParticipants`
    /// (`api/api_chat_participants.cpp` + `boxes/peers/edit_participants_box.cpp`).
    pub fn load_channel_participants(
        &self,
        peer_id: PeerId,
        filter: ParticipantFilter,
        offset: i32,
        limit: i32,
        hash: i64,
    ) -> Result<ParticipantsPage, String> {
        let Some(channel) = input_channel(self, peer_id) else {
            return Err("Channel not found.".to_owned());
        };
        let req = tl_fns::channels::GetParticipants {
            channel,
            filter: filter.to_tl(),
            offset: offset.max(0),
            limit: limit.clamp(1, PARTICIPANTS_PER_PAGE),
            hash,
        };
        let resp = invoke_rpc(self, req)?;
        match resp {
            tl_enums::channels::ChannelParticipants::ChannelParticipants(data) => {
                let data_peer = to_data_peer(peer_id);
                let mut parsed_rows: Vec<(DataPeerId, MemberRole)> =
                    Vec::with_capacity(data.participants.len());

                write_session(self, |session| {
                    apply_tl_users_and_chats(session, &data.users, &data.chats);
                    let ch_id = data_peer.to_channel();
                    for p in &data.participants {
                        match p {
                            tl_enums::ChannelParticipant::Creator(c) => {
                                let uid = DataUserId::from_tl(c.user_id);
                                let pid = DataPeerId::from_user(uid);
                                let custom_title = c.rank.clone().filter(|r| !r.is_empty());
                                if let Some(cid) = ch_id
                                    && let Some(ch) = session.channel_mut(cid)
                                    && let Some(mg) = ch.mg_info.as_mut()
                                {
                                    mg.creator = Some(uid);
                                    mg.admins.insert(uid);
                                    if let Some(ref rank) = custom_title {
                                        mg.member_ranks.insert(uid, rank.clone());
                                    }
                                }
                                parsed_rows.push((pid, MemberRole::Owner { custom_title }));
                            }
                            tl_enums::ChannelParticipant::Admin(a) => {
                                let uid = DataUserId::from_tl(a.user_id);
                                let pid = DataPeerId::from_user(uid);
                                let custom_title = a.rank.clone().filter(|r| !r.is_empty());
                                let rights = admin_rights_from_tl(&a.admin_rights);
                                if let Some(cid) = ch_id
                                    && let Some(ch) = session.channel_mut(cid)
                                    && let Some(mg) = ch.mg_info.as_mut()
                                {
                                    mg.admins.insert(uid);
                                    if let Some(ref rank) = custom_title {
                                        mg.member_ranks.insert(uid, rank.clone());
                                    }
                                }
                                parsed_rows.push((
                                    pid,
                                    MemberRole::Admin {
                                        custom_title,
                                        rights,
                                    },
                                ));
                            }
                            tl_enums::ChannelParticipant::ChannelParticipant(m) => {
                                let uid = DataUserId::from_tl(m.user_id);
                                let pid = DataPeerId::from_user(uid);
                                if let Some(cid) = ch_id
                                    && let Some(rank) = m.rank.as_ref().filter(|r| !r.is_empty())
                                    && let Some(ch) = session.channel_mut(cid)
                                    && let Some(mg) = ch.mg_info.as_mut()
                                {
                                    mg.member_ranks.insert(uid, rank.clone());
                                }
                                parsed_rows.push((pid, MemberRole::Member));
                            }
                            tl_enums::ChannelParticipant::Self_(m) => {
                                let uid = DataUserId::from_tl(m.user_id);
                                let pid = DataPeerId::from_user(uid);
                                if let Some(cid) = ch_id
                                    && let Some(rank) = m.rank.as_ref().filter(|r| !r.is_empty())
                                    && let Some(ch) = session.channel_mut(cid)
                                    && let Some(mg) = ch.mg_info.as_mut()
                                {
                                    mg.member_ranks.insert(uid, rank.clone());
                                }
                                parsed_rows.push((pid, MemberRole::Member));
                            }
                            tl_enums::ChannelParticipant::Banned(b) => {
                                let pid = peer_id_from_tl(&b.peer);
                                let info = banned_rights_from_tl(&b.banned_rights);
                                parsed_rows.push((
                                    pid,
                                    MemberRole::Restricted {
                                        rights: info.flags,
                                        until: info.until,
                                    },
                                ));
                            }
                            tl_enums::ChannelParticipant::Left(l) => {
                                // Only surface `Left` entries when explicitly querying kicked/banned.
                                if matches!(
                                    filter,
                                    ParticipantFilter::Kicked(_) | ParticipantFilter::Banned(_)
                                ) {
                                    let pid = peer_id_from_tl(&l.peer);
                                    parsed_rows.push((pid, MemberRole::Member));
                                }
                            }
                        }
                    }
                });

                let now = now_unix_i32();
                let members: Vec<MemberEntry> = read_session(self, |session| {
                    parsed_rows
                        .into_iter()
                        .map(|(pid, role)| build_member_entry(session, pid, role, now))
                        .collect()
                });

                let count = data.count.max(0) as usize;
                let received = data.participants.len() as i32;
                let next_offset = offset.max(0).saturating_add(received);
                let has_more = received > 0 && (next_offset as usize) < count;

                Ok(ParticipantsPage {
                    members,
                    total_count: count,
                    next_offset,
                    has_more,
                })
            }
            tl_enums::channels::ChannelParticipants::NotModified => {
                let cached = with_state(self, |st| {
                    st.members
                        .get(&(peer_id, filter))
                        .map(|e| ParticipantsPage {
                            members: e.members.clone(),
                            total_count: e.total_count,
                            next_offset: e.next_offset,
                            has_more: !e.exhausted,
                        })
                });
                Ok(cached.unwrap_or_default())
            }
        }
    }

    /// Loads participants of a legacy basic group via `messages.getFullChat`
    /// (`apiwrap.cpp:1273`, `info/profile/info_profile_members.cpp`).
    pub fn load_basic_chat_members(&self, peer_id: PeerId) -> Result<Vec<MemberEntry>, String> {
        let data_peer = to_data_peer(peer_id);
        let Some(chat_id) = data_peer.to_chat() else {
            return Err("Peer is not a basic group chat.".to_owned());
        };
        let req = tl_fns::messages::GetFullChat {
            chat_id: chat_id.0 as i64,
        };
        let tl_enums::messages::ChatFull::ChatFull(resp) = invoke_rpc(self, req)?;

        let mut parsed_rows: Vec<(DataPeerId, MemberRole)> = Vec::new();
        let mut parsed_ranks: Vec<(DataPeerId, Option<String>)> = Vec::new();
        write_session(self, |session| {
            apply_tl_users_and_chats(session, &resp.users, &resp.chats);
            apply_tl_chat_full(session, &resp.full_chat);

            if let tl_enums::ChatFull::ChatFull(full) = &resp.full_chat
                && let tl_enums::ChatParticipants::ChatParticipants(ps) = &full.participants
            {
                for p in &ps.participants {
                    match p {
                        tl_enums::ChatParticipant::Creator(c) => {
                            let pid = DataPeerId::from_user(DataUserId::from_tl(c.user_id));
                            let custom_title = c.rank.clone().filter(|r| !r.is_empty());
                            parsed_ranks.push((pid, custom_title.clone()));
                            parsed_rows.push((pid, MemberRole::Owner { custom_title }));
                        }
                        tl_enums::ChatParticipant::Admin(a) => {
                            let pid = DataPeerId::from_user(DataUserId::from_tl(a.user_id));
                            let custom_title = a.rank.clone().filter(|r| !r.is_empty());
                            parsed_ranks.push((pid, custom_title.clone()));
                            parsed_rows.push((
                                pid,
                                MemberRole::Admin {
                                    custom_title,
                                    rights: ChatAdminRights::all(),
                                },
                            ));
                        }
                        tl_enums::ChatParticipant::ChatParticipant(m) => {
                            let pid = DataPeerId::from_user(DataUserId::from_tl(m.user_id));
                            parsed_ranks.push((pid, m.rank.clone().filter(|r| !r.is_empty())));
                            parsed_rows.push((pid, MemberRole::Member));
                        }
                    }
                }
            }
        });
        for (pid, rank) in parsed_ranks {
            self.cache_member_rank(peer_id, from_data_peer(pid), rank);
        }

        let now = now_unix_i32();
        let mut members: Vec<MemberEntry> = read_session(self, |session| {
            parsed_rows
                .into_iter()
                .map(|(pid, role)| build_member_entry(session, pid, role, now))
                .collect()
        });
        members.sort_by_key(member_role_sort_key);
        Ok(members)
    }

    // -------------------------------------------------------------------------
    // 5. Common groups (`messages.getCommonChats`)
    // -------------------------------------------------------------------------

    /// Fetches one paginated slice of groups shared in common with `user_id` via
    /// `messages.getCommonChats` (`info/common_groups/info_common_groups_inner_widget.cpp:95-144`).
    pub fn load_common_groups_page(
        &self,
        user_id: PeerId,
        max_id: i64,
        limit: i32,
    ) -> Result<CommonGroupsPage, String> {
        let Some(input_u) = input_user(self, user_id) else {
            return Err("User not found.".to_owned());
        };
        let effective_limit = limit.clamp(1, 100);
        let req = tl_fns::messages::GetCommonChats {
            user_id: input_u,
            max_id: max_id.max(0),
            limit: effective_limit,
        };
        let resp = invoke_rpc(self, req)?;
        let (tl_chats, total_count, is_slice) = match resp {
            tl_enums::messages::Chats::Chats(c) => {
                let len = c.chats.len();
                (c.chats, Some(len), false)
            }
            tl_enums::messages::Chats::Slice(s) => (s.chats, Some(s.count.max(0) as usize), true),
        };

        let mut next_max_id = max_id;
        let mut group_pids: Vec<DataPeerId> = Vec::with_capacity(tl_chats.len());

        write_session(self, |session| {
            apply_tl_users_and_chats(session, &[], &tl_chats);
            for chat in &tl_chats {
                if let Some((pid, bare_id)) = tl_chat_peer_and_bare_id(chat) {
                    next_max_id = bare_id;
                    let migrated = session.peer(pid).and_then(Peer::as_chat).is_some_and(|c| {
                        c.is_migrated() || c.flags.contains(ChatFlags::DEACTIVATED)
                    });
                    if !migrated {
                        group_pids.push(pid);
                    }
                }
            }
            if let Some(total) = total_count
                && let Some(uid) = to_data_peer(user_id).to_user()
                && let Some(user) = session.user_mut(uid)
                && max_id == 0
            {
                user.common_chats_count = total as i32;
            }
        });

        let groups: Vec<CommonGroupEntry> = read_session(self, |session| {
            group_pids
                .into_iter()
                .filter_map(|pid| {
                    let peer = session.peer(pid)?;
                    let members_count = match peer {
                        Peer::Chat(c) => c.count.max(0) as u32,
                        Peer::Channel(ch) => ch.members_count.max(0) as u32,
                        Peer::User(_) => 0,
                    };
                    Some(CommonGroupEntry {
                        peer_id: from_data_peer(pid),
                        title: peer_display_title(session, pid),
                        members_count,
                        color_index: peer.common().color_index % 7,
                    })
                })
                .collect()
        });

        let has_more = if tl_chats.is_empty() || next_max_id == max_id {
            false
        } else if is_slice {
            total_count.is_none_or(|t| tl_chats.len() < t)
        } else {
            tl_chats.len() >= (effective_limit as usize)
        };

        Ok(CommonGroupsPage {
            groups,
            total_count,
            next_max_id,
            has_more,
        })
    }

    /// Loads the next page of common groups for `user_id` into the session cache and returns
    /// all loaded groups so far.
    pub fn load_more_common_groups(
        &self,
        user_id: PeerId,
    ) -> Result<Vec<CommonGroupEntry>, String> {
        let (max_id, exhausted) = with_state(self, |st| {
            let entry = st.common_groups.entry(user_id).or_default();
            (entry.last_max_id, entry.exhausted)
        });
        if exhausted {
            return Ok(with_state(self, |st| {
                st.common_groups
                    .get(&user_id)
                    .map(|e| e.groups.clone())
                    .unwrap_or_default()
            }));
        }
        let page = self.load_common_groups_page(user_id, max_id, COMMON_GROUPS_PER_PAGE)?;
        let all = with_state(self, |st| {
            let entry = st.common_groups.entry(user_id).or_default();
            entry.loaded = true;
            entry.last_max_id = page.next_max_id;
            entry.exhausted = !page.has_more;
            for g in page.groups {
                if entry.seen_peers.insert(g.peer_id) {
                    entry.groups.push(g);
                }
            }
            entry.groups.clone()
        });
        Ok(all)
    }

    // -------------------------------------------------------------------------
    // 6. Option-based `messages.report` & `messages.unpinAllMessages`
    // -------------------------------------------------------------------------

    /// Executes one step of Telegram's Layer-233 option-based `messages.report` flow
    /// (`api/api_report.cpp:87-146`, `boxes/report_messages_box.cpp:71-213`).
    ///
    /// Pass `option = &[]` and `comment = ""` on the initial request to obtain the root
    /// options tree (`ReportStepResult::ChooseOption`), then pass the selected option's bytes
    /// on subsequent steps until `ReportStepResult::AddComment` or `ReportStepResult::Reported`.
    pub fn report_messages_step(
        &self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
        option: &[u8],
        comment: &str,
    ) -> Result<ReportStepResult, String> {
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        let req = tl_fns::messages::Report {
            peer,
            id: msg_ids.iter().map(|m| m.0).collect(),
            option: option.to_vec(),
            message: comment.to_owned(),
        };
        match invoke_rpc_raw(self, req) {
            Ok(tl_enums::ReportResult::ChooseOption(data)) => {
                let options = data
                    .options
                    .into_iter()
                    .map(|o| {
                        let tl_enums::MessageReportOption::MessageReportOption(opt) = o;
                        ReportOption {
                            option: opt.option,
                            text: opt.text,
                        }
                    })
                    .collect();
                Ok(ReportStepResult::ChooseOption {
                    title: data.title,
                    options,
                })
            }
            Ok(tl_enums::ReportResult::AddComment(data)) => Ok(ReportStepResult::AddComment {
                optional: data.optional,
                option: data.option,
            }),
            Ok(tl_enums::ReportResult::Reported) => Ok(ReportStepResult::Reported),
            Err(err) if is_rpc_error_named(&err, "MESSAGE_ID_REQUIRED") => {
                Ok(ReportStepResult::MessageIdRequired)
            }
            Err(err) => Err(err.to_string()),
        }
    }

    /// Unpins all messages in `peer_id` (optionally scoped to `topic_root_id` or
    /// `saved_peer_id`), iterating `messages.unpinAllMessages` while `AffectedHistory.offset > 0`
    /// (`window/window_peer_menu.cpp:4277-4319`).
    pub fn unpin_all_messages_in_thread(
        &self,
        peer_id: PeerId,
        topic_root_id: Option<MsgId>,
        saved_peer_id: Option<PeerId>,
    ) -> Result<(), String> {
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        let saved_input = saved_peer_id.and_then(|pid| input_peer(self, pid));

        for _ in 0..MAX_UNPIN_BATCHES {
            let req = tl_fns::messages::UnpinAllMessages {
                peer: peer.clone(),
                top_msg_id: topic_root_id.map(|m| m.0),
                saved_peer_id: saved_input.clone(),
            };
            let tl_enums::messages::AffectedHistory::AffectedHistory(affected) =
                invoke_rpc(self, req)?;
            // `ApiWrap::applyAffectedHistory` (`apiwrap.cpp:2338-2348`): feed each page's
            // `(pts, pts_count)` into SessionActor's single updates handler.
            let _ = self.apply_affected_history(Some(peer_id), affected.pts, affected.pts_count);
            if affected.offset <= 0 {
                break;
            }
        }

        let data_peer = to_data_peer(peer_id);
        let target_topic = topic_root_id.map(to_data_msg);
        write_session(self, |session| {
            let ids_to_unpin: Vec<DataFullMsgId> = history_messages_ordered(session, data_peer)
                .into_iter()
                .filter(|m| m.flags.contains(MessageFlags::PINNED))
                .filter(|m| match target_topic {
                    None => true,
                    Some(topic_id) => {
                        m.id == topic_id
                            || m.reply_to
                                .as_ref()
                                .and_then(|r| r.top_msg_id)
                                .is_some_and(|t| t == topic_id)
                    }
                })
                .map(|m| m.full_id())
                .collect();
            for fid in ids_to_unpin {
                let _ = session.set_message_pinned(fid, false);
            }
        });

        self.invalidate_pinned_cache(peer_id);
        self.bridge_state().notify_ui();
        Ok(())
    }

    // -------------------------------------------------------------------------
    // 8. Jump-to-Date Calendar (`messages.getSearchResultsCalendar`)
    // -------------------------------------------------------------------------

    /// Fetches per-day message counts and anchor message IDs for `(year, month)` inside `peer_id`
    /// using `messages.getSearchResultsCalendar` (`data/data_search_calendar.cpp`).
    ///
    /// Paginates backwards with `(offset_id, offset_date)` until the entire month is covered or
    /// the server reaches `min_date` / returns no new periods.
    pub fn get_search_results_calendar(
        &self,
        peer_id: PeerId,
        filter: SharedMediaFilter,
        year: i32,
        month: u32,
    ) -> Result<Vec<CalendarDayInfo>, String> {
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        let clamped_month = month.clamp(1, 12);
        let month_start = month_start_unix_utc(year, clamped_month);
        let mut offset_date = month_end_unix_utc(year, clamped_month);
        let mut offset_id: i32 = 0;

        let mut days_by_day: BTreeMap<u32, CalendarDayInfo> = BTreeMap::new();
        const MAX_CALENDAR_PAGES: usize = 8;

        for _ in 0..MAX_CALENDAR_PAGES {
            let req = tl_fns::messages::GetSearchResultsCalendar {
                peer: peer.clone(),
                saved_peer_id: None,
                filter: filter.to_tl(),
                offset_id,
                offset_date,
            };
            let tl_enums::messages::SearchResultsCalendar::SearchResultsCalendar(cal) =
                invoke_rpc(self, req)?;

            write_session(self, |session| {
                apply_tl_users_and_chats(session, &cal.users, &cal.chats);
                for tl_msg in &cal.messages {
                    let _ = apply_tl_message(session, tl_msg);
                }
            });

            if cal.periods.is_empty() {
                break;
            }

            let mut earliest_date = offset_date;
            let mut earliest_msg_id = offset_id;
            let mut progressed = false;

            for period in &cal.periods {
                let tl_enums::SearchResultsCalendarPeriod::SearchResultsCalendarPeriod(p) = period;
                let info =
                    CalendarDayInfo::from_period(p.date, p.min_msg_id, p.max_msg_id, p.count);
                if info.year == year && info.month == clamped_month {
                    days_by_day.entry(info.day).or_insert(info);
                }
                if p.date < earliest_date
                    || (p.date == earliest_date && p.min_msg_id < earliest_msg_id)
                {
                    earliest_date = p.date;
                    earliest_msg_id = p.min_msg_id;
                    progressed = true;
                }
            }

            if !progressed
                || earliest_date <= month_start
                || (cal.min_date > 0 && earliest_date <= cal.min_date)
                || earliest_msg_id <= 1
            {
                break;
            }
            offset_date = earliest_date.saturating_sub(1);
            offset_id = earliest_msg_id;
        }

        let results: Vec<CalendarDayInfo> = days_by_day.into_values().collect();
        with_state(self, |st| {
            st.calendar
                .insert((peer_id, year, clamped_month), results.clone());
        });
        Ok(results)
    }

    // -------------------------------------------------------------------------
    // 9. Public Posts Search (`channels.checkSearchPostsFlood` + `channels.searchPosts`)
    // -------------------------------------------------------------------------

    /// Queries the user's daily free-search quota and optional Stars cost for public posts search
    /// (`channels.checkSearchPostsFlood`, `dialogs/dialogs_search_posts.cpp`).
    pub fn check_search_posts_flood(
        &self,
        query: Option<&str>,
    ) -> Result<PostsSearchFloodInfo, String> {
        let req = tl_fns::channels::CheckSearchPostsFlood {
            query: query
                .map(str::trim)
                .filter(|q| !q.is_empty())
                .map(str::to_owned),
        };
        let tl_enums::SearchPostsFlood::SearchPostsFlood(f) = invoke_rpc(self, req)?;
        let info = posts_flood_from_tl(&f);
        with_state(self, |st| {
            st.public_posts_flood = Some(info.clone());
        });
        Ok(info)
    }

    /// Executes one paginated `channels.searchPosts` RPC for public channel posts
    /// (`dialogs/dialogs_search_posts.cpp`).
    ///
    /// Queries starting with `#` (single token without whitespace) use the `hashtag` parameter;
    /// general text queries use `query`.
    pub fn search_public_posts_page(
        &self,
        raw_query: &str,
        cursor: GlobalSearchCursor,
        limit: i32,
        allow_paid_stars: Option<i64>,
    ) -> Result<PublicPostsPage, String> {
        let trimmed = raw_query.trim();
        if trimmed.is_empty() {
            return Ok(PublicPostsPage::default());
        }

        let (hashtag, query) = if let Some(tag) = trimmed.strip_prefix('#')
            && !tag.is_empty()
            && !tag.chars().any(char::is_whitespace)
        {
            (Some(tag.to_owned()), None)
        } else {
            (None, Some(trimmed.to_owned()))
        };

        let offset_peer = if cursor.is_start() {
            tl_enums::InputPeer::Empty
        } else {
            cursor
                .offset_peer
                .and_then(|pid| input_peer(self, pid))
                .unwrap_or(tl_enums::InputPeer::Empty)
        };
        let offset_rate = if cursor.is_start() {
            0
        } else {
            cursor.offset_rate
        };
        let offset_id = if cursor.is_start() {
            0
        } else {
            cursor.offset_id.0
        };

        let req = tl_fns::channels::SearchPosts {
            hashtag,
            query,
            offset_rate,
            offset_peer,
            offset_id,
            limit: limit.clamp(1, 100),
            allow_paid_stars,
        };

        let resp = match invoke_rpc_raw(self, req) {
            Ok(resp) => resp,
            Err(err) if is_rpc_error_named(&err, "SEARCH_QUERY_EMPTY") => {
                return Ok(PublicPostsPage::default());
            }
            Err(err) => return Err(err.to_string()),
        };

        let (tl_messages, tl_users, tl_chats, total_count, next_rate, flood, is_complete) =
            match resp {
                tl_enums::messages::Messages::Messages(d) => {
                    let total = d.messages.len();
                    (d.messages, d.users, d.chats, total, None, None, true)
                }
                tl_enums::messages::Messages::Slice(d) => {
                    let flood = d.search_flood.as_ref().map(|sf| {
                        let tl_enums::SearchPostsFlood::SearchPostsFlood(inner) = sf;
                        posts_flood_from_tl(inner)
                    });
                    (
                        d.messages,
                        d.users,
                        d.chats,
                        d.count.max(0) as usize,
                        d.next_rate,
                        flood,
                        false,
                    )
                }
                tl_enums::messages::Messages::ChannelMessages(d) => (
                    d.messages,
                    d.users,
                    d.chats,
                    d.count.max(0) as usize,
                    None,
                    None,
                    false,
                ),
                tl_enums::messages::Messages::NotModified(_) => {
                    return Ok(PublicPostsPage::default());
                }
            };

        let mut last_peer: Option<PeerId> = None;
        let mut last_id = MsgId(0);
        let mut inserted_ids: Vec<DataFullMsgId> = Vec::with_capacity(tl_messages.len());

        write_session(self, |session| {
            apply_tl_users_and_chats(session, &tl_users, &tl_chats);
            for tl_msg in &tl_messages {
                let (mid, mpeer) = tl_message_id_and_peer(tl_msg);
                last_id = mid;
                if let Some(dpid) = mpeer {
                    last_peer = Some(from_data_peer(dpid));
                }
                if let Some(full_id) = apply_tl_message(session, tl_msg) {
                    inserted_ids.push(full_id);
                }
            }
        });

        let messages = read_session(self, |session| {
            inserted_ids
                .iter()
                .filter_map(|&fid| session.message(fid))
                .filter_map(|m| message_to_search_result(session, m))
                .collect::<Vec<_>>()
        });

        // Ported from `Dialogs::SearchPosts::loadMore` (`dialogs/dialogs_search_posts.cpp:263-272`):
        // update `offset_rate` when `next_rate` advances, and continue while `messages.messagesSlice`
        // returned non-empty messages with a valid `(offset_peer, offset_id)` cursor even if
        // `next_rate` is `None` or unchanged.
        let effective_rate = match next_rate {
            Some(nr) if nr != offset_rate => nr,
            _ => offset_rate,
        };
        let has_more =
            !is_complete && !messages.is_empty() && last_peer.is_some() && last_id.0 != 0;

        if let Some(ref f) = flood {
            with_state(self, |st| {
                st.public_posts_flood = Some(f.clone());
            });
        }

        Ok(PublicPostsPage {
            messages,
            total_count,
            next_cursor: GlobalSearchCursor {
                offset_rate: effective_rate,
                offset_peer: last_peer,
                offset_id: last_id,
            },
            has_more,
            flood,
        })
    }

    // -------------------------------------------------------------------------
    // 10. Global Media Tab (`Info::GlobalMedia` via `messages.searchGlobal`)
    // -------------------------------------------------------------------------

    /// Loads one paginated slice of global media across all chats for `tab` via
    /// `messages.searchGlobal` (`info/global_media/info_global_media_provider.cpp`).
    pub fn load_global_media_page(
        &self,
        tab: SharedMediaTab,
        query: &str,
        cursor: GlobalSearchCursor,
        limit: i32,
    ) -> Result<GlobalMediaPage, String> {
        let offset_peer = if cursor.is_start() {
            tl_enums::InputPeer::Empty
        } else {
            cursor
                .offset_peer
                .and_then(|pid| input_peer(self, pid))
                .unwrap_or(tl_enums::InputPeer::Empty)
        };
        let offset_rate = if cursor.is_start() {
            0
        } else {
            cursor.offset_rate
        };
        let offset_id = if cursor.is_start() {
            0
        } else {
            cursor.offset_id.0
        };

        // Ported from `Api::SearchController` (`data/data_search_controller.cpp:75-81`):
        // `messages.searchGlobal` for global media always passes `folder_id = Some(0)` (main chat list).
        let req = tl_fns::messages::SearchGlobal {
            broadcasts_only: false,
            groups_only: false,
            users_only: false,
            folder_id: Some(0),
            community: None,
            q: query.trim().to_owned(),
            filter: SharedMediaFilter::from_tab(tab).to_tl(),
            min_date: 0,
            max_date: 0,
            offset_rate,
            offset_peer,
            offset_id,
            limit: limit.clamp(1, 100),
        };

        let resp = match invoke_rpc_raw(self, req) {
            Ok(resp) => resp,
            Err(err) if is_rpc_error_named(&err, "SEARCH_QUERY_EMPTY") => {
                return Ok(GlobalMediaPage::default());
            }
            Err(err) => return Err(err.to_string()),
        };

        let (tl_messages, tl_users, tl_chats, total_count, next_rate, is_complete) = match resp {
            tl_enums::messages::Messages::Messages(d) => {
                let total = d.messages.len();
                (d.messages, d.users, d.chats, total, None, true)
            }
            tl_enums::messages::Messages::Slice(d) => (
                d.messages,
                d.users,
                d.chats,
                d.count.max(0) as usize,
                d.next_rate,
                false,
            ),
            tl_enums::messages::Messages::ChannelMessages(d) => (
                d.messages,
                d.users,
                d.chats,
                d.count.max(0) as usize,
                None,
                false,
            ),
            tl_enums::messages::Messages::NotModified(_) => {
                return Ok(GlobalMediaPage::default());
            }
        };

        let mut last_peer: Option<PeerId> = None;
        let mut last_id = MsgId(0);
        let mut inserted_ids: Vec<DataFullMsgId> = Vec::with_capacity(tl_messages.len());

        write_session(self, |session| {
            apply_tl_users_and_chats(session, &tl_users, &tl_chats);
            for tl_msg in &tl_messages {
                let (mid, mpeer) = tl_message_id_and_peer(tl_msg);
                last_id = mid;
                if let Some(dpid) = mpeer {
                    last_peer = Some(from_data_peer(dpid));
                }
                if let Some(fid) = apply_tl_message(session, tl_msg) {
                    inserted_ids.push(fid);
                }
            }
        });

        let entries: Vec<GlobalMediaEntry> = read_session(self, |session| {
            inserted_ids
                .iter()
                .filter_map(|&fid| session.message(fid))
                .filter_map(|m| {
                    let item = message_to_info_media_item(session, m, tab)?;
                    let peer_title = peer_display_title(session, m.peer_id);
                    Some(GlobalMediaEntry {
                        peer_id: from_data_peer(m.peer_id),
                        peer_title,
                        item,
                    })
                })
                .collect()
        });

        let (effective_rate, has_more) = if is_complete || tl_messages.is_empty() {
            (offset_rate, false)
        } else if let Some(nr) = next_rate {
            if nr != offset_rate {
                (nr, last_peer.is_some() && last_id.0 != 0)
            } else {
                (offset_rate, false)
            }
        } else {
            (offset_rate, false)
        };

        let next_cursor = if has_more {
            last_peer.map(|lp| (effective_rate, lp, last_id))
        } else {
            None
        };

        Ok(GlobalMediaPage {
            entries,
            total_count,
            next_cursor,
            has_more,
        })
    }

    // -------------------------------------------------------------------------
    // 11. History Extras: Contacts, Nearby Venues, Edit Media/Caption
    // -------------------------------------------------------------------------

    /// Fetches the user's Telegram contacts via `contacts.getContacts` and converts them to
    /// [`ContactAttachment`] items for the Share Contact box.
    pub fn load_contacts_live(&self) -> Result<Vec<ContactAttachment>, String> {
        let resp = invoke_rpc(self, tl_fns::contacts::GetContacts { hash: 0 })?;
        if let tl_enums::contacts::Contacts::Contacts(c) = resp {
            write_session(self, |session| {
                apply_tl_users_and_chats(session, &c.users, &[]);
            });
            let mut out = Vec::with_capacity(c.users.len());
            for u in &c.users {
                if let tl_enums::User::User(user) = u {
                    if user.self_ || user.deleted {
                        continue;
                    }
                    out.push(ContactAttachment {
                        user_id: Some(PeerId::from_user(user.id as u64)),
                        first_name: user.first_name.clone().unwrap_or_default(),
                        last_name: user.last_name.clone().unwrap_or_default(),
                        phone_number: user.phone.clone().unwrap_or_default(),
                    });
                }
            }
            with_state(self, |st| {
                st.contacts_loaded = true;
                st.contacts = out.clone();
            });
            return Ok(out);
        }
        // `contactsNotModified`: return cached or session-known contacts.
        Ok(collect_session_contacts(self))
    }

    /// Queries nearby venues via an inline venue bot (`contacts.resolveUsername` +
    /// `messages.getInlineBotResults` with `geo_point`), matching tdesktop's location picker.
    pub fn load_nearby_venues_via_bot(
        &self,
        bot_username: &str,
        lat: f64,
        lon: f64,
        query: &str,
    ) -> Result<Vec<VenueItem>, String> {
        let uname = bot_username.trim().trim_start_matches('@');
        if uname.is_empty() {
            return Ok(Vec::new());
        }
        let tl_enums::contacts::ResolvedPeer::ResolvedPeer(resolved) = invoke_rpc(
            self,
            tl_fns::contacts::ResolveUsername {
                username: uname.to_owned(),
                referer: None,
            },
        )?;
        write_session(self, |session| {
            apply_tl_users_and_chats(session, &resolved.users, &resolved.chats);
        });
        let bot_peer = peer_id_from_tl(&resolved.peer);
        let Some(bot_uid) = bot_peer.to_user() else {
            return Err("Venue bot username did not resolve to a user.".to_owned());
        };
        let bot_input_user = read_session(self, |s| input_user_for(s, bot_uid));
        if matches!(bot_input_user, tl_enums::InputUser::Empty) {
            return Err("Could not build InputUser for venue bot.".to_owned());
        }
        let self_peer = read_session(self, |s| input_peer_for(s, s.self_peer_id()));
        let peer_for_query = if matches!(self_peer, tl_enums::InputPeer::Empty) {
            tl_enums::InputPeer::Self_
        } else {
            self_peer
        };
        let tl_enums::messages::BotResults::BotResults(results) = invoke_rpc(
            self,
            tl_fns::messages::GetInlineBotResults {
                bot: bot_input_user,
                peer: peer_for_query,
                geo_point: Some(tl_enums::InputGeoPoint::InputGeoPoint(
                    tl_types::InputGeoPoint {
                        lat,
                        long: lon,
                        accuracy_radius: None,
                    },
                )),
                query: query.trim().to_owned(),
                offset: String::new(),
            },
        )?;
        write_session(self, |session| {
            apply_tl_users_and_chats(session, &results.users, &[]);
        });

        let mut venues = Vec::new();
        for res in &results.results {
            let (id, fallback_title, fallback_desc, send_msg) = match res {
                tl_enums::BotInlineResult::BotInlineResult(r) => (
                    &r.id,
                    r.title.as_deref().unwrap_or_default(),
                    r.description.as_deref().unwrap_or_default(),
                    &r.send_message,
                ),
                tl_enums::BotInlineResult::BotInlineMediaResult(r) => (
                    &r.id,
                    r.title.as_deref().unwrap_or_default(),
                    r.description.as_deref().unwrap_or_default(),
                    &r.send_message,
                ),
            };
            if let tl_enums::BotInlineMessage::MediaVenue(v) = send_msg {
                let (vlat, vlon) = match &v.geo {
                    tl_enums::GeoPoint::GeoPoint(g) => (g.lat, g.long),
                    tl_enums::GeoPoint::Empty => (lat, lon),
                };
                venues.push(VenueItem {
                    id: if v.venue_id.is_empty() {
                        id.clone()
                    } else {
                        v.venue_id.clone()
                    },
                    title: if v.title.is_empty() {
                        fallback_title.to_owned()
                    } else {
                        v.title.clone()
                    },
                    address: if v.address.is_empty() {
                        fallback_desc.to_owned()
                    } else {
                        v.address.clone()
                    },
                    category: if v.venue_type.is_empty() {
                        if v.provider.is_empty() {
                            "Place".to_owned()
                        } else {
                            v.provider.clone()
                        }
                    } else {
                        v.venue_type.clone()
                    },
                    lat: vlat,
                    lon: vlon,
                });
            }
        }
        Ok(venues)
    }

    /// Queries nearby geochats / located peers via `contacts.getLocated` and maps them into
    /// selectable [`VenueItem`] entries for the Location picker box.
    pub fn load_nearby_venues_located(&self, lat: f64, lon: f64) -> Result<Vec<VenueItem>, String> {
        let updates = invoke_rpc(
            self,
            tl_fns::contacts::GetLocated {
                background: false,
                geo_point: tl_enums::InputGeoPoint::InputGeoPoint(tl_types::InputGeoPoint {
                    lat,
                    long: lon,
                    accuracy_radius: None,
                }),
                self_expires: None,
            },
        )?;

        let _ = self.apply_updates_to_session(&updates);

        let mut located_peers: Vec<(DataPeerId, i32)> = Vec::new();
        let tl_updates: &[tl_enums::Update] = match &updates {
            tl_enums::Updates::Updates(u) => &u.updates,
            tl_enums::Updates::Combined(u) => &u.updates,
            tl_enums::Updates::UpdateShort(s) => std::slice::from_ref(&s.update),
            _ => &[],
        };

        for upd in tl_updates {
            if let tl_enums::Update::PeerLocated(pl) = upd {
                for entry in &pl.peers {
                    if let tl_enums::PeerLocated::PeerLocated(p) = entry {
                        located_peers.push((peer_id_from_tl(&p.peer), p.distance));
                    }
                }
            }
        }

        let venues = read_session(self, |session| {
            located_peers
                .into_iter()
                .filter_map(|(pid, distance)| {
                    let title = peer_display_title(session, pid);
                    if title.is_empty() {
                        return None;
                    }
                    let category = if pid.is_channel() || pid.is_chat() {
                        "Nearby Group".to_owned()
                    } else {
                        "Nearby".to_owned()
                    };
                    Some(VenueItem {
                        id: format!("located_{}", pid.0),
                        title,
                        address: format!("{distance} m away"),
                        category,
                        lat,
                        lon,
                    })
                })
                .collect()
        });
        Ok(venues)
    }

    /// Edits a media message's caption, `invert_media` / `spoiler` flags, and optional replacement
    /// media via `messages.editMessage` (`boxes/edit_caption_box.cpp` + `api/api_editing.cpp`).
    pub fn edit_message_media_live(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        request: EditMediaCaptionRequest,
    ) -> Result<HistoryMessage, String> {
        let Some(peer) = input_peer(self, peer_id) else {
            return Err("Peer not found in session.".to_owned());
        };
        let data_peer = to_data_peer(peer_id);
        let data_msg = to_data_msg(msg_id);

        // 1. Build optional `InputMedia` (either newly uploaded replacement or existing media with spoiler).
        let input_media: Option<tl_enums::InputMedia> = if let Some(ref rep) =
            request.replacement_media
        {
            let rpc = self.runtime().rpc().clone();
            let bytes = Arc::new(rep.bytes.clone());
            let file_name = rep.file_name.clone();
            let cfg = LiveUploadConfig {
                is_photo: rep.as_photo,
                ..LiveUploadConfig::default()
            };
            let cancel = CancelToken::new();
            let uploaded = block_on_runtime(self.runtime().tokio_handle(), async move {
                upload_file_bytes_async(&rpc, file_name, bytes, cfg, &cancel, |_| {}).await
            })
            .map_err(|e| e.to_string())?;

            if rep.as_photo {
                Some(tl_enums::InputMedia::UploadedPhoto(
                    tl_types::InputMediaUploadedPhoto {
                        spoiler: request.spoiler,
                        live_photo: false,
                        file: uploaded.input_file,
                        stickers: None,
                        ttl_seconds: None,
                        video: None,
                    },
                ))
            } else {
                let mime = if rep.mime_type.is_empty() {
                    "application/octet-stream".to_owned()
                } else {
                    rep.mime_type.clone()
                };
                Some(tl_enums::InputMedia::UploadedDocument(
                    tl_types::InputMediaUploadedDocument {
                        nosound_video: false,
                        force_file: false,
                        spoiler: request.spoiler,
                        file: uploaded.input_file,
                        thumb: None,
                        mime_type: mime,
                        attributes: vec![tl_enums::DocumentAttribute::Filename(
                            tl_types::DocumentAttributeFilename {
                                file_name: rep.file_name.clone(),
                            },
                        )],
                        stickers: None,
                        video_cover: None,
                        video_timestamp: None,
                        ttl_seconds: None,
                    },
                ))
            }
        } else if request.spoiler {
            read_session(self, |session| {
                let msg = session.message(DataFullMsgId::new(data_peer, data_msg))?;
                let MessageContent::Regular { media, .. } = &msg.content else {
                    return None;
                };
                match media {
                    MessageMedia::Photo(p) => {
                        Some(tl_enums::InputMedia::Photo(tl_types::InputMediaPhoto {
                            spoiler: true,
                            live_photo: false,
                            id: tl_enums::InputPhoto::InputPhoto(tl_types::InputPhoto {
                                id: p.id.0 as i64,
                                access_hash: p.access_hash as i64,
                                file_reference: p.file_reference.clone(),
                            }),
                            ttl_seconds: None,
                            video: None,
                        }))
                    }
                    MessageMedia::Document(d) => Some(tl_enums::InputMedia::Document(
                        tl_types::InputMediaDocument {
                            spoiler: true,
                            id: tl_enums::InputDocument::InputDocument(tl_types::InputDocument {
                                id: d.id.0 as i64,
                                access_hash: d.access_hash as i64,
                                file_reference: d.file_reference.clone(),
                            }),
                            video_cover: None,
                            video_timestamp: None,
                            ttl_seconds: None,
                            query: None,
                        },
                    )),
                    _ => None,
                }
            })
        } else {
            None
        };

        let entities = read_session(self, |s| ui_text_to_tl_entities(&request.caption, s));
        let req = tl_fns::messages::EditMessage {
            no_webpage: false,
            invert_media: request.invert_media,
            peer,
            id: msg_id.0,
            message: Some(request.caption.text.clone()),
            media: input_media,
            reply_markup: None,
            entities: (!entities.is_empty()).then_some(entities),
            schedule_date: None,
            schedule_repeat_period: None,
            quick_reply_shortcut_id: None,
            rich_message: None,
        };
        let maybe_updates = self.edit_message_via_runtime(req)?;
        if let Some(ref updates) = maybe_updates {
            self.apply_updates_side_caches(updates);
            if !crate::session::updates_contain_edit_for(updates, data_msg) {
                let data_text = ui_text_to_data_text(&request.caption);
                let edit_ts = now_unix_i32();
                write_session(self, |session| {
                    if let Some(m) = session.message_mut(DataFullMsgId::new(data_peer, data_msg)) {
                        if let MessageContent::Regular {
                            text: ref mut t, ..
                        } = m.content
                        {
                            *t = data_text;
                        }
                        m.edit_date = Some(edit_ts);
                    }
                });
            }
        }
        self.bridge_state().notify_ui();

        read_session(self, |session| {
            session
                .message(DataFullMsgId::new(data_peer, data_msg))
                .map(|m| message_to_history_message(session, m))
        })
        .ok_or_else(|| "Edited message not found in session.".to_owned())
    }
}

// =============================================================================
// Trait implementation hooks called from `live/session.rs`
// =============================================================================

/// Hook for [`tg_app_core::dialogs::backend::DialogsBackend::search_dialogs`].
pub(super) fn search_dialogs(
    live: &LiveSession,
    request: &SearchRequest,
    recent_hashtags: &[String],
) -> SearchResults {
    let query = request.query.trim();
    let query_lower = query.to_ascii_lowercase();
    let now = now_unix_i32();
    let mut results = SearchResults::default();

    if request.tab == SearchTab::PublicPosts {
        return search_public_posts_for_dialogs(live, request);
    }

    let selected_tag_norm = request
        .selected_tag
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let selected_reactions: Vec<tg_data::ReactionId> = selected_tag_norm
        .as_deref()
        .map(|tag| {
            if let Some(doc_id) = tag
                .strip_prefix("custom:")
                .and_then(|s| s.parse::<u64>().ok())
            {
                tg_data::ReactionId::Custom(tg_data::DocumentId(doc_id))
            } else {
                tg_data::ReactionId::Emoji(tag.to_owned())
            }
        })
        .into_iter()
        .collect();

    if query.is_empty() && !(request.in_peer.is_some() && !selected_reactions.is_empty()) {
        return results;
    }

    // 1. Hashtag / cashtag suggestions.
    if let Some(tag_prefix) = query.strip_prefix('#').or_else(|| query.strip_prefix('$')) {
        let sigil = query.chars().next().unwrap_or('#');
        let prefix_lower = tag_prefix.to_ascii_lowercase();
        for tag in recent_hashtags {
            if prefix_lower.is_empty() || tag.to_ascii_lowercase().starts_with(&prefix_lower) {
                results.hashtags.push(HashtagSuggestion {
                    tag: tag.clone(),
                    sigil,
                    usage_count: 1,
                });
            }
        }
    }

    let list = if request.in_archive {
        DialogListQuery::Archive
    } else {
        DialogListQuery::Filter(FilterId::ALL)
    };
    let mut local_peer_ids = HashSet::new();

    // 2. Local dialog title/username matching (when not scoped to a single peer).
    if request.in_peer.is_none() {
        read_session(live, |session| {
            let keys = if request.in_archive {
                session.archive_dialogs()
            } else {
                session.main_dialogs()
            };
            for &key in keys {
                let DialogKey::History(pid) = key else {
                    continue;
                };
                if let Some(item) = dialog_item_from_session(session, pid, list, now)
                    && (item.title.to_ascii_lowercase().contains(&query_lower)
                        || item
                            .username
                            .as_deref()
                            .is_some_and(|u| u.to_ascii_lowercase().contains(&query_lower)))
                {
                    local_peer_ids.insert(item.peer_id);
                    results.local_chats.push(item);
                }
            }
        });
    }

    // 3. Global peer directory search (`contacts.search`, cached per `(query, tab)`).
    if request.in_peer.is_none() && query.len() >= 3 {
        let peer_key = PeerSearchCacheKey {
            query: convert_peer_search_query(query).to_ascii_lowercase(),
            tab: request.tab,
        };
        let cached_peers = with_state(live, |st| st.peer_search.get(&peer_key).cloned());
        let peer_res = match cached_peers {
            Some(found) => Some(found),
            None => match live.search_peers(query, request.tab, 0) {
                Ok(found) => {
                    with_state(live, |st| {
                        st.peer_search.insert(peer_key, found.clone());
                    });
                    Some(found)
                }
                Err(err) => {
                    debug!(query, error = %err, "contacts.search failed");
                    None
                }
            },
        };
        if let Some(found) = peer_res {
            // Add any `my_results` not already in `local_chats`.
            read_session(live, |session| {
                for pid in &found.my_results {
                    if local_peer_ids.insert(*pid)
                        && let Some(item) =
                            dialog_item_from_session(session, to_data_peer(*pid), list, now)
                    {
                        results.local_chats.push(item);
                    }
                }
            });
            results.global_peers = found.global_results;
        }
    }

    // 4. Server-side message search: in-chat (`messages.search`) or global (`messages.searchGlobal`).
    let limit = request.messages_limit.max(20);
    let needed = request.messages_offset.saturating_add(limit);

    if let Some(in_peer) = request.in_peer {
        let cache_key = InChatCacheKey {
            peer_id: in_peer,
            query: query.to_owned(),
            filter: SharedMediaFilter::Empty,
            selected_tag: selected_tag_norm.clone(),
        };
        // Fetch pages until we have enough for `offset + limit` or server is exhausted.
        loop {
            let (have_len, exhausted, next_around, rpc_ok, loaded) = with_state(live, |st| {
                let entry = st.in_chat_search.entry(cache_key.clone()).or_default();
                (
                    entry.message_ids.len(),
                    entry.exhausted,
                    entry.oldest_id.unwrap_or(MsgId(0)),
                    entry.rpc_ok,
                    entry.loaded,
                )
            });
            if (loaded && !rpc_ok) || exhausted || have_len >= needed {
                break;
            }
            let mut params = InChatSearchParams::new(in_peer, query, SharedMediaFilter::Empty);
            params.saved_reaction = selected_reactions.clone();
            params.around_id = next_around;
            params.direction = LoadDirection::Before;
            params.limit = SEARCH_PER_PAGE;
            match live.search_in_chat(&params) {
                Ok(page) => {
                    let data_peer = to_data_peer(in_peer);
                    let mut grew = false;
                    with_state(live, |st| {
                        let entry = st.in_chat_search.entry(cache_key.clone()).or_default();
                        entry.loaded = true;
                        entry.rpc_ok = true;
                        entry.total_count = page.total_count.max(entry.message_ids.len());
                        for m in &page.messages {
                            let fid = DataFullMsgId::new(data_peer, to_data_msg(m.id));
                            if entry.message_ids.insert(m.id.0, fid).is_none() {
                                grew = true;
                            }
                        }
                        if let Some(oldest) = page.oldest_id {
                            entry.oldest_id = Some(match entry.oldest_id {
                                Some(prev) => MsgId(prev.0.min(oldest.0)),
                                None => oldest,
                            });
                        }
                        if !page.has_more_older || !grew {
                            entry.exhausted = true;
                        }
                    });
                    if !grew {
                        break;
                    }
                }
                Err(err) => {
                    debug!(peer = in_peer.0, query, error = %err, "messages.search failed");
                    with_state(live, |st| {
                        let entry = st.in_chat_search.entry(cache_key.clone()).or_default();
                        entry.loaded = true;
                        entry.rpc_ok = false;
                        entry.exhausted = true;
                    });
                    break;
                }
            }
        }

        let (rpc_ok, ids_desc, total_count, exhausted) = with_state(live, |st| {
            let entry = st.in_chat_search.get(&cache_key);
            match entry {
                Some(e) => (
                    e.rpc_ok,
                    e.message_ids.values().rev().copied().collect::<Vec<_>>(),
                    e.total_count,
                    e.exhausted,
                ),
                None => (false, Vec::new(), 0, true),
            }
        });

        if rpc_ok {
            let all_msgs: Vec<MessageSearchResult> = read_session(live, |session| {
                ids_desc
                    .iter()
                    .filter_map(|&fid| session.message(fid))
                    .filter_map(|m| message_to_search_result(session, m))
                    .collect()
            });
            let total = total_count.max(all_msgs.len());
            let offset = request.messages_offset.min(all_msgs.len());
            results.total_messages = total;
            results.has_more_messages = !exhausted || offset.saturating_add(limit) < all_msgs.len();
            results.messages = all_msgs.into_iter().skip(offset).take(limit).collect();
            return results;
        }
    } else {
        let folder_id = if request.in_archive {
            Some(tg_data::ARCHIVE_FOLDER_ID)
        } else {
            Some(0)
        };
        let cache_key = GlobalSearchCacheKey {
            query: query.to_owned(),
            folder_id,
            chat_type_filter: request.chat_type_filter,
        };
        loop {
            let (have_len, exhausted, cursor, rpc_attempted, rpc_ok) = with_state(live, |st| {
                let entry = st.global_search.entry(cache_key.clone()).or_default();
                (
                    entry.messages.len(),
                    entry.exhausted,
                    entry.cursor,
                    entry.exhausted || entry.rpc_ok,
                    entry.rpc_ok,
                )
            });
            if (rpc_attempted && !rpc_ok) || exhausted || have_len >= needed {
                break;
            }
            let params = GlobalSearchParams {
                query: query.to_owned(),
                chat_type_filter: request.chat_type_filter,
                folder_id,
                filter: SharedMediaFilter::Empty,
                min_date: 0,
                max_date: 0,
                limit: SEARCH_PER_PAGE,
            };
            match live.search_global_page(&params, cursor) {
                Ok(page) => {
                    let mut grew = false;
                    with_state(live, |st| {
                        let entry = st.global_search.entry(cache_key.clone()).or_default();
                        entry.rpc_ok = true;
                        entry.total_count = page.total_count.max(entry.messages.len());
                        for m in page.messages {
                            if entry.seen.insert((m.peer_id, m.msg_id)) {
                                entry.messages.push(m);
                                grew = true;
                            }
                        }
                        entry.cursor = page.next_cursor;
                        if !page.has_more || !grew {
                            entry.exhausted = true;
                        }
                    });
                    if !grew {
                        break;
                    }
                }
                Err(err) => {
                    debug!(query, error = %err, "messages.searchGlobal failed");
                    with_state(live, |st| {
                        let entry = st.global_search.entry(cache_key.clone()).or_default();
                        entry.rpc_ok = false;
                        entry.exhausted = true;
                    });
                    break;
                }
            }
        }

        let cached_global = with_state(live, |st| {
            st.global_search.get(&cache_key).and_then(|e| {
                e.rpc_ok
                    .then(|| (e.messages.clone(), e.total_count, e.exhausted))
            })
        });
        if let Some((all_msgs, total_count, exhausted)) = cached_global {
            let total = total_count.max(all_msgs.len());
            let offset = request.messages_offset.min(all_msgs.len());
            results.total_messages = total;
            results.has_more_messages = !exhausted || offset.saturating_add(limit) < all_msgs.len();
            results.messages = all_msgs.into_iter().skip(offset).take(limit).collect();
            return results;
        }
    }

    // Fallback: scan locally loaded messages when offline / RPC unavailable.
    read_session(live, |session| {
        let keys = if request.in_archive {
            session.archive_dialogs()
        } else {
            session.main_dialogs()
        };
        for &key in keys {
            let DialogKey::History(pid) = key else {
                continue;
            };
            if let Some(in_peer) = request.in_peer
                && from_data_peer(pid) != in_peer
            {
                continue;
            }
            if let Some(item) = dialog_item_from_session(session, pid, list, now)
                && request.chat_type_filter.matches(item.kind)
            {
                for msg in history_messages_ordered(session, pid).into_iter().rev() {
                    let body = match &msg.content {
                        MessageContent::Regular { text, .. } => text.text.as_str(),
                        MessageContent::Empty | MessageContent::Service(_) => "",
                    };
                    if !body.is_empty() && body.to_ascii_lowercase().contains(&query_lower) {
                        results.messages.push(MessageSearchResult {
                            peer_id: item.peer_id,
                            kind: item.kind,
                            special: item.special,
                            peer_title: item.title.clone(),
                            color_index: item.color_index,
                            msg_id: from_data_msg(msg.id),
                            sender_name: crate::preview_text::preview_sender(session, msg),
                            text: body.to_owned(),
                            time: unix_to_local_datetime(msg.date),
                            archived: item.archived,
                        });
                    }
                }
            }
        }
    });

    let total = results.messages.len();
    let offset = request.messages_offset.min(total);
    results.total_messages = total;
    results.has_more_messages = offset.saturating_add(limit) < total;
    results.messages = results
        .messages
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect();
    results
}

/// Hook for [`tg_app_core::dialogs::backend::DialogsBackend::top_peers`].
pub(super) fn top_peers(live: &LiveSession) -> Vec<TopPeerItem> {
    let (loaded, disabled, cached_pairs) = with_state(live, |st| {
        (
            st.top_peers_loaded,
            st.top_peers_disabled,
            st.top_peers.clone(),
        )
    });
    if !loaded {
        match live.fetch_top_peers(TopPeerCategoryKind::Correspondents, 0, TOP_PEERS_LIMIT) {
            Ok(items) => return items,
            Err(err) => {
                debug!(error = %err, "contacts.getTopPeers failed");
                with_state(live, |st| {
                    st.top_peers_loaded = true;
                });
                return Vec::new();
            }
        }
    }
    if disabled {
        return Vec::new();
    }
    build_top_peer_items(live, &cached_pairs)
}

/// Hook for [`tg_app_core::dialogs::backend::DialogsBackend::remove_top_peer`].
pub(super) fn remove_top_peer(live: &LiveSession, peer_id: PeerId) {
    if let Err(err) = live.reset_top_peer_rating(TopPeerCategoryKind::Correspondents, peer_id) {
        debug!(peer = peer_id.0, error = %err, "contacts.resetTopPeerRating failed");
        with_state(live, |st| {
            st.top_peers.retain(|&(pid, _)| pid != peer_id);
            st.removed_top_peers.insert(peer_id);
        });
        live.bridge_state().notify_ui();
    }
}

/// Updates cached top-peers enabled/disabled state (`data/components/top_peers.cpp:195-216`).
pub(crate) fn set_top_peers_enabled_state(live: &LiveSession, enabled: bool) {
    with_state(live, |st| {
        st.top_peers_disabled = !enabled;
        if !enabled {
            st.top_peers_loaded = true;
            st.top_peers.clear();
        } else {
            st.top_peers_loaded = false;
        }
    });
}

/// Returns whether top-peers suggestions are currently enabled in local state.
pub(crate) fn is_top_peers_enabled(live: &LiveSession) -> bool {
    with_state(live, |st| !st.top_peers_disabled)
}

/// Hook for [`tg_app_core::info::backend::InfoBackend::peer_members`].
pub(super) fn peer_members(live: &LiveSession, peer_id: PeerId, query: &str) -> Vec<MemberEntry> {
    let trimmed = query.trim();
    let data_peer = to_data_peer(peer_id);
    if data_peer.is_channel() {
        let filter = if trimmed.is_empty() {
            ParticipantFilter::Recent
        } else {
            ParticipantFilter::Search(trimmed.to_owned())
        };
        if let Some(cached) = with_state(live, |st| {
            st.members
                .get(&(peer_id, filter.clone()))
                .map(|e| e.members.clone())
        }) {
            return cached;
        }
        match live.load_channel_participants(peer_id, filter.clone(), 0, PARTICIPANTS_PER_PAGE, 0) {
            Ok(page) => {
                with_state(live, |st| {
                    st.members.insert(
                        (peer_id, filter),
                        MembersCacheEntry {
                            members: page.members.clone(),
                            total_count: page.total_count,
                            next_offset: page.next_offset,
                            exhausted: !page.has_more,
                        },
                    );
                });
                return page.members;
            }
            Err(err) => {
                debug!(peer = peer_id.0, error = %err, "channels.getParticipants failed");
                // Fall back to locally filtering cached `Recent` entries if available.
                let q_lower = trimmed.to_ascii_lowercase();
                return with_state(live, |st| {
                    st.members
                        .get(&(peer_id, ParticipantFilter::Recent))
                        .map(|e| {
                            e.members
                                .iter()
                                .filter(|m| member_matches_query(m, &q_lower))
                                .cloned()
                                .collect()
                        })
                        .unwrap_or_default()
                });
            }
        }
    }

    if data_peer.is_chat() {
        let cached = with_state(live, |st| {
            st.members
                .get(&(peer_id, ParticipantFilter::Recent))
                .map(|e| e.members.clone())
        });
        let all = match cached {
            Some(list) => list,
            None => match live.load_basic_chat_members(peer_id) {
                Ok(list) => {
                    with_state(live, |st| {
                        st.members.insert(
                            (peer_id, ParticipantFilter::Recent),
                            MembersCacheEntry {
                                total_count: list.len(),
                                next_offset: list.len() as i32,
                                exhausted: true,
                                members: list.clone(),
                            },
                        );
                    });
                    list
                }
                Err(err) => {
                    debug!(peer = peer_id.0, error = %err, "messages.getFullChat failed");
                    Vec::new()
                }
            },
        };
        let q_lower = trimmed.to_ascii_lowercase();
        return all
            .into_iter()
            .filter(|m| member_matches_query(m, &q_lower))
            .collect();
    }

    Vec::new()
}

/// Hook for [`tg_app_core::info::backend::InfoBackend::common_groups`].
pub(super) fn common_groups(live: &LiveSession, user_id: PeerId) -> Vec<CommonGroupEntry> {
    let loaded = with_state(live, |st| {
        st.common_groups.get(&user_id).is_some_and(|e| e.loaded)
    });
    if loaded {
        return with_state(live, |st| {
            st.common_groups
                .get(&user_id)
                .map(|e| e.groups.clone())
                .unwrap_or_default()
        });
    }
    match live.load_more_common_groups(user_id) {
        Ok(list) => list,
        Err(err) => {
            debug!(user = user_id.0, error = %err, "messages.getCommonChats failed");
            with_state(live, |st| {
                let entry = st.common_groups.entry(user_id).or_default();
                entry.loaded = true;
                entry.exhausted = true;
            });
            Vec::new()
        }
    }
}

/// Hook for [`tg_app_core::info::backend::InfoBackend::shared_media`].
pub(super) fn shared_media(
    live: &LiveSession,
    peer_id: PeerId,
    tab: SharedMediaTab,
    query: &str,
) -> Vec<InfoMediaItem> {
    let trimmed = query.trim();
    let filter = SharedMediaFilter::from_tab(tab);
    let cache_key = InChatCacheKey {
        peer_id,
        query: trimmed.to_owned(),
        filter,
        selected_tag: None,
    };
    let loaded = with_state(live, |st| {
        st.in_chat_search.get(&cache_key).is_some_and(|e| e.loaded)
    });
    if !loaded {
        match live.load_shared_media_page(
            peer_id,
            tab,
            trimmed,
            MsgId(0),
            LoadDirection::Before,
            SHARED_MEDIA_PER_PAGE,
        ) {
            Ok(page) => {
                let data_peer = to_data_peer(peer_id);
                with_state(live, |st| {
                    let entry = st.in_chat_search.entry(cache_key.clone()).or_default();
                    entry.loaded = true;
                    entry.rpc_ok = true;
                    entry.total_count = page.total_count;
                    entry.oldest_id = page.oldest_id;
                    entry.exhausted = !page.has_more_older;
                    for item in &page.items {
                        let mid = item.id();
                        entry.message_ids.insert(
                            mid,
                            DataFullMsgId::new(data_peer, DataMsgId(i64::from(mid))),
                        );
                    }
                });
                return page.items;
            }
            Err(err) => {
                debug!(peer = peer_id.0, ?tab, error = %err, "messages.search (shared media) failed");
                with_state(live, |st| {
                    let entry = st.in_chat_search.entry(cache_key.clone()).or_default();
                    entry.loaded = true;
                    entry.rpc_ok = false;
                    entry.exhausted = true;
                });
            }
        }
    }

    let (rpc_ok, ids_desc) = with_state(live, |st| {
        st.in_chat_search
            .get(&cache_key)
            .map(|e| {
                (
                    e.rpc_ok,
                    e.message_ids.values().rev().copied().collect::<Vec<_>>(),
                )
            })
            .unwrap_or((false, Vec::new()))
    });

    if rpc_ok {
        return read_session(live, |session| {
            ids_desc
                .iter()
                .filter_map(|&fid| session.message(fid))
                .filter_map(|m| message_to_info_media_item(session, m, tab))
                .collect()
        });
    }

    // Fallback to scanning locally loaded history when offline / RPC unavailable.
    let data_peer = to_data_peer(peer_id);
    let q_lower = trimmed.to_ascii_lowercase();
    read_session(live, |session| {
        history_messages_ordered(session, data_peer)
            .into_iter()
            .rev()
            .filter_map(|m| message_to_info_media_item(session, m, tab))
            .filter(|item| item.matches_query(&q_lower))
            .collect()
    })
}

/// Hook for [`tg_app_core::history::backend::HistoryBackend::pinned_messages`].
pub(super) fn pinned_messages(live: &LiveSession, peer_id: PeerId) -> Vec<PinnedMessageSummary> {
    pinned_full_messages(live, peer_id, None)
        .into_iter()
        .map(|hm| PinnedMessageSummary {
            msg_id: hm.id,
            sender_name: hm.sender_name.clone(),
            preview: hm.preview_snippet(),
        })
        .collect()
}

/// Whether the first pinned-messages page of `peer`'s main chat is cached.
pub(crate) fn pinned_loaded(live: &LiveSession, peer: tg_data::PeerId) -> bool {
    let key = (crate::convert::from_data_peer(peer), None);
    with_state(live, |st| st.pinned.get(&key).is_some_and(|e| e.loaded))
}

/// Loads the first pinned-messages page of `peer`'s main chat into the cache (used by
/// [`crate::open_chat_prefetch`]).
pub(crate) fn load_pinned_now(live: &LiveSession, peer: tg_data::PeerId) {
    let _ = load_pinned_into_cache(live, crate::convert::from_data_peer(peer), None);
}

/// Requests the first pinned-messages page and records it (or the failure) in the cache.
fn load_pinned_into_cache(
    live: &LiveSession,
    peer_id: PeerId,
    topic_root_id: Option<MsgId>,
) -> Option<Vec<HistoryMessage>> {
    let key = (peer_id, topic_root_id);
    match live.load_pinned_messages_page(
        peer_id,
        topic_root_id,
        MsgId(0),
        LoadDirection::Before,
        SHARED_MEDIA_PER_PAGE,
    ) {
        Ok(page) => {
            let ids: Vec<MsgId> = page.messages.iter().map(|m| m.id).collect();
            with_state(live, |st| {
                st.pinned.insert(
                    key,
                    PinnedCacheEntry {
                        msg_ids: ids,
                        loaded: true,
                        rpc_ok: true,
                    },
                );
            });
            Some(page.messages)
        }
        Err(err) => {
            debug!(peer = peer_id.0, error = %err, "messages.search (pinned) failed");
            with_state(live, |st| {
                st.pinned.insert(
                    key,
                    PinnedCacheEntry {
                        msg_ids: Vec::new(),
                        loaded: true,
                        rpc_ok: false,
                    },
                );
            });
            None
        }
    }
}

/// Hook for [`tg_app_core::history::backend::HistoryBackend::pinned_full_messages`].
pub(super) fn pinned_full_messages(
    live: &LiveSession,
    peer_id: PeerId,
    topic_root_id: Option<MsgId>,
) -> Vec<HistoryMessage> {
    let key = (peer_id, topic_root_id);
    let is_loaded = || with_state(live, |st| st.pinned.get(&key).is_some_and(|e| e.loaded));
    let mut loaded = is_loaded();
    if !loaded
        && topic_root_id.is_none()
        && crate::open_chat_prefetch::wait_in_flight(
            live,
            to_data_peer(peer_id),
            crate::open_chat_prefetch::OpenChatRequest::Pinned,
        )
    {
        loaded = is_loaded();
    }
    if !loaded && let Some(messages) = load_pinned_into_cache(live, peer_id, topic_root_id) {
        return messages;
    }

    let cached = with_state(live, |st| {
        st.pinned
            .get(&key)
            .and_then(|e| e.rpc_ok.then(|| e.msg_ids.clone()))
    });
    let data_peer = to_data_peer(peer_id);
    if let Some(ids) = cached {
        return read_session(live, |session| {
            ids.into_iter()
                .filter_map(|mid| session.message(DataFullMsgId::new(data_peer, to_data_msg(mid))))
                .filter(|m| m.flags.contains(MessageFlags::PINNED))
                .map(|m| message_to_history_message(session, m))
                .collect()
        });
    }

    // Fallback to locally loaded pinned messages.
    let target_topic = topic_root_id.map(to_data_msg);
    read_session(live, |session| {
        history_messages_ordered(session, data_peer)
            .into_iter()
            .rev()
            .filter(|m| m.flags.contains(MessageFlags::PINNED))
            .filter(|m| match target_topic {
                None => true,
                Some(topic_id) => {
                    m.id == topic_id
                        || m.reply_to
                            .as_ref()
                            .and_then(|r| r.top_msg_id)
                            .is_some_and(|t| t == topic_id)
                }
            })
            .map(|m| message_to_history_message(session, m))
            .collect()
    })
}

/// Hook for [`tg_app_core::history::backend::HistoryBackend::report_messages`].
///
/// Drives Telegram's Layer-233 option-based `messages.report` flow to completion for
/// `reason` and `comment` (`api/api_report.cpp` + `boxes/report_messages_box.cpp`).
pub(super) fn report_messages(
    live: &LiveSession,
    peer_id: PeerId,
    msg_ids: &[MsgId],
    reason: MessageReportReason,
    comment: &str,
) -> Result<(), String> {
    if msg_ids.is_empty() {
        return Err("At least one message ID is required to report.".to_owned());
    }
    let mut current_option: Vec<u8> = Vec::new();
    let mut current_comment = String::new();

    for _ in 0..MAX_REPORT_STEPS {
        match live.report_messages_step(peer_id, msg_ids, &current_option, &current_comment)? {
            ReportStepResult::Reported => return Ok(()),
            ReportStepResult::MessageIdRequired => {
                return Err("Server requires explicit message IDs to complete report.".to_owned());
            }
            ReportStepResult::AddComment { optional, option } => {
                if !optional && comment.trim().is_empty() {
                    return Err("A comment is required for this report option.".to_owned());
                }
                current_option = option;
                current_comment = comment.to_owned();
            }
            ReportStepResult::ChooseOption { options, .. } => {
                let Some(chosen) = select_report_option(&options, reason) else {
                    return Err("Server returned no selectable report options.".to_owned());
                };
                current_option = chosen.option.clone();
                current_comment.clear();
            }
        }
    }

    warn!(
        peer = peer_id.0,
        "messages.report exceeded maximum option steps"
    );
    Err("Report flow exceeded maximum number of option steps.".to_owned())
}

/// Hook for [`tg_app_core::history::backend::HistoryBackend::unpin_all_messages`].
pub(super) fn unpin_all_messages(
    live: &LiveSession,
    peer_id: PeerId,
    topic_root_id: Option<MsgId>,
) -> Result<(), String> {
    live.unpin_all_messages_in_thread(peer_id, topic_root_id, None)
}

/// Hook for [`tg_app_core::history::backend::HistoryBackend::search_results_calendar`].
pub(super) fn search_results_calendar(
    live: &LiveSession,
    peer_id: PeerId,
    year: i32,
    month: u32,
) -> Result<Vec<CalendarDayInfo>, String> {
    let clamped_month = month.clamp(1, 12);
    if let Some(cached) = with_state(live, |st| {
        st.calendar.get(&(peer_id, year, clamped_month)).cloned()
    }) {
        return Ok(cached);
    }
    live.get_search_results_calendar(peer_id, SharedMediaFilter::Empty, year, clamped_month)
}

/// Hook for [`tg_app_core::info::backend::InfoBackend::global_media`].
pub(super) fn global_media(
    live: &LiveSession,
    tab: SharedMediaTab,
    query: &str,
    limit: usize,
) -> GlobalMediaPage {
    let trimmed = query.trim();
    let needed = limit.max(20);
    let cache_key = GlobalMediaCacheKey {
        tab,
        query: trimmed.to_owned(),
    };

    loop {
        let (have_len, exhausted, cursor, rpc_attempted, rpc_ok) = with_state(live, |st| {
            let entry = st.global_media.entry(cache_key.clone()).or_default();
            (
                entry.entries.len(),
                entry.exhausted,
                entry.cursor,
                entry.exhausted || entry.rpc_ok,
                entry.rpc_ok,
            )
        });
        if (rpc_attempted && !rpc_ok) || exhausted || have_len >= needed {
            break;
        }
        match live.load_global_media_page(tab, trimmed, cursor, SEARCH_PER_PAGE) {
            Ok(page) => {
                let mut grew = false;
                with_state(live, |st| {
                    let entry = st.global_media.entry(cache_key.clone()).or_default();
                    entry.rpc_ok = true;
                    entry.total_count = page.total_count.max(entry.entries.len());
                    for item in page.entries {
                        if entry.seen.insert(item.full_key()) {
                            entry.entries.push(item);
                            grew = true;
                        }
                    }
                    if let Some((rate, peer, id)) = page.next_cursor {
                        entry.cursor = GlobalSearchCursor {
                            offset_rate: rate,
                            offset_peer: Some(peer),
                            offset_id: id,
                        };
                    }
                    if !page.has_more || !grew {
                        entry.exhausted = true;
                    }
                });
                if !grew {
                    break;
                }
            }
            Err(err) => {
                debug!(?tab, query = trimmed, error = %err, "messages.searchGlobal (global media) failed");
                with_state(live, |st| {
                    let entry = st.global_media.entry(cache_key.clone()).or_default();
                    entry.rpc_ok = false;
                    entry.exhausted = true;
                });
                break;
            }
        }
    }

    let cached = with_state(live, |st| {
        st.global_media.get(&cache_key).and_then(|e| {
            e.rpc_ok
                .then(|| (e.entries.clone(), e.total_count, e.cursor, e.exhausted))
        })
    });
    if let Some((all, total_count, cursor, exhausted)) = cached {
        let total = total_count.max(all.len());
        let has_more = !exhausted || all.len() > needed;
        let entries: Vec<GlobalMediaEntry> = all.into_iter().take(needed).collect();
        let next_cursor = has_more
            .then(|| {
                cursor
                    .offset_peer
                    .map(|p| (cursor.offset_rate, p, cursor.offset_id))
            })
            .flatten();
        return GlobalMediaPage {
            entries,
            total_count: total,
            next_cursor,
            has_more,
        };
    }

    // Offline fallback: scan locally loaded dialogs in `Session`.
    let q_lower = trimmed.to_ascii_lowercase();
    let entries: Vec<GlobalMediaEntry> = read_session(live, |session| {
        let mut out = Vec::new();
        for &key in session
            .main_dialogs()
            .iter()
            .chain(session.archive_dialogs().iter())
        {
            let DialogKey::History(pid) = key else {
                continue;
            };
            let title = peer_display_title(session, pid);
            for msg in history_messages_ordered(session, pid).into_iter().rev() {
                if let Some(item) = message_to_info_media_item(session, msg, tab)
                    && (q_lower.is_empty()
                        || item.matches_query(&q_lower)
                        || title.to_ascii_lowercase().contains(&q_lower))
                {
                    out.push(GlobalMediaEntry {
                        peer_id: from_data_peer(pid),
                        peer_title: title.clone(),
                        item,
                    });
                }
            }
        }
        out
    });
    let total_count = entries.len();
    let has_more = total_count > needed;
    GlobalMediaPage {
        entries: entries.into_iter().take(needed).collect(),
        total_count,
        next_cursor: None,
        has_more,
    }
}

/// Hook for [`tg_app_core::history::backend::HistoryBackend::contacts`].
pub(super) fn history_contacts(live: &LiveSession) -> Vec<ContactAttachment> {
    let (loaded, cached) = with_state(live, |st| (st.contacts_loaded, st.contacts.clone()));
    if loaded {
        return cached;
    }
    match live.load_contacts_live() {
        Ok(list) => list,
        Err(err) => {
            debug!(error = %err, "contacts.getContacts failed; falling back to session contacts");
            let fallback = collect_session_contacts(live);
            with_state(live, |st| {
                st.contacts_loaded = true;
                st.contacts = fallback.clone();
            });
            fallback
        }
    }
}

/// Hook for [`tg_app_core::history::backend::HistoryBackend::nearby_venues`].
pub(super) fn nearby_venues(live: &LiveSession) -> Vec<VenueItem> {
    nearby_venues_for_coords(live, 0.0, 0.0, "")
}

/// Hook for [`tg_app_core::history::backend::HistoryBackend::nearby_venues_for_coords`].
///
/// Queries the inline venue bot (`DEFAULT_VENUE_BOT_USERNAME`) first as tdesktop does,
/// falling back to `contacts.getLocated` if the venue bot returns empty or fails.
pub(super) fn nearby_venues_for_coords(
    live: &LiveSession,
    lat: f64,
    lon: f64,
    query: &str,
) -> Vec<VenueItem> {
    let key = format!("{lat:.3},{lon:.3}:{}", query.trim().to_ascii_lowercase());
    if let Some(cached) = with_state(live, |st| st.venues.get(&key).cloned()) {
        return cached;
    }

    let mut found =
        match live.load_nearby_venues_via_bot(DEFAULT_VENUE_BOT_USERNAME, lat, lon, query) {
            Ok(list) => list,
            Err(err) => {
                debug!(error = %err, "inline venue bot query failed; trying contacts.getLocated");
                Vec::new()
            }
        };

    if found.is_empty() {
        match live.load_nearby_venues_located(lat, lon) {
            Ok(list) => found = list,
            Err(err) => {
                debug!(error = %err, "contacts.getLocated failed");
            }
        }
    }

    with_state(live, |st| {
        st.venues.insert(key, found.clone());
    });
    found
}

/// Hook for [`tg_app_core::history::backend::HistoryBackend::edit_message_media`].
pub(super) fn edit_message_media(
    live: &LiveSession,
    peer_id: PeerId,
    msg_id: MsgId,
    request: EditMediaCaptionRequest,
) -> Result<HistoryMessage, String> {
    live.edit_message_media_live(peer_id, msg_id, request)
}

fn search_public_posts_for_dialogs(live: &LiveSession, request: &SearchRequest) -> SearchResults {
    let query = request.query.trim();
    let mut results = SearchResults::default();

    if query.is_empty() {
        let cached_flood = with_state(live, |st| st.public_posts_flood.clone());
        results.posts_flood = cached_flood.or_else(|| live.check_search_posts_flood(None).ok());
        return results;
    }

    let limit = request.messages_limit.max(20);
    let needed = request.messages_offset.saturating_add(limit);
    let cache_key = query.to_owned();

    // Ensure we know flood quota at least once.
    let has_flood = with_state(live, |st| st.public_posts_flood.is_some());
    if !has_flood {
        let _ = live.check_search_posts_flood(Some(query));
    }

    loop {
        let (have_len, exhausted, cursor, rpc_attempted, rpc_ok) = with_state(live, |st| {
            let entry = st.public_posts.entry(cache_key.clone()).or_default();
            (
                entry.messages.len(),
                entry.exhausted,
                entry.cursor,
                entry.exhausted || entry.rpc_ok,
                entry.rpc_ok,
            )
        });
        if (rpc_attempted && !rpc_ok) || exhausted || have_len >= needed {
            break;
        }
        match live.search_public_posts_page(query, cursor, SEARCH_PER_PAGE, None) {
            Ok(page) => {
                let mut grew = false;
                with_state(live, |st| {
                    let entry = st.public_posts.entry(cache_key.clone()).or_default();
                    entry.rpc_ok = true;
                    entry.total_count = page.total_count.max(entry.messages.len());
                    if let Some(f) = page.flood {
                        entry.flood = Some(f.clone());
                        st.public_posts_flood = Some(f);
                    }
                    for m in page.messages {
                        if entry.seen.insert((m.peer_id, m.msg_id)) {
                            entry.messages.push(m);
                            grew = true;
                        }
                    }
                    entry.cursor = page.next_cursor;
                    if !page.has_more || !grew {
                        entry.exhausted = true;
                    }
                });
                if !grew {
                    break;
                }
            }
            Err(err) => {
                debug!(query, error = %err, "channels.searchPosts failed");
                with_state(live, |st| {
                    let entry = st.public_posts.entry(cache_key.clone()).or_default();
                    entry.rpc_ok = false;
                    entry.exhausted = true;
                });
                break;
            }
        }
    }

    with_state(live, |st| {
        if let Some(entry) = st.public_posts.get(&cache_key) {
            let total = entry.total_count.max(entry.messages.len());
            let offset = request.messages_offset.min(entry.messages.len());
            results.total_messages = total;
            results.has_more_messages =
                !entry.exhausted || offset.saturating_add(limit) < entry.messages.len();
            results.messages = entry
                .messages
                .iter()
                .skip(offset)
                .take(limit)
                .cloned()
                .collect();
            results.posts_flood = entry
                .flood
                .clone()
                .or_else(|| st.public_posts_flood.clone());
        } else {
            results.posts_flood = st.public_posts_flood.clone();
        }
    });
    results
}

fn posts_flood_from_tl(f: &tl_types::SearchPostsFlood) -> PostsSearchFloodInfo {
    PostsSearchFloodInfo {
        query_is_free: f.query_is_free,
        total_daily: f.total_daily,
        remains: f.remains,
        wait_till: f.wait_till,
        stars_amount: f.stars_amount,
    }
}

fn collect_session_contacts(live: &LiveSession) -> Vec<ContactAttachment> {
    read_session(live, |session| {
        let self_id = session.self_peer_id();
        let mut out = Vec::new();
        for &key in session
            .main_dialogs()
            .iter()
            .chain(session.archive_dialogs().iter())
        {
            let DialogKey::History(pid) = key else {
                continue;
            };
            if pid == self_id {
                continue;
            }
            if let Some(Peer::User(u)) = session.peer(pid)
                && u.is_contact()
            {
                out.push(ContactAttachment {
                    user_id: Some(from_data_peer(pid)),
                    first_name: u.first_name.clone(),
                    last_name: u.last_name.clone(),
                    phone_number: u.phone.clone(),
                });
            }
        }
        out
    })
}

// =============================================================================
// Private helpers
// =============================================================================

fn build_top_peer_items(live: &LiveSession, pairs: &[(PeerId, f64)]) -> Vec<TopPeerItem> {
    let removed = with_state(live, |st| st.removed_top_peers.clone());
    let now = now_unix_i32();
    read_session(live, |session| {
        pairs
            .iter()
            .filter_map(|&(app_pid, _)| {
                if removed.contains(&app_pid) {
                    return None;
                }
                let data_pid = to_data_peer(app_pid);
                if data_pid == session.self_peer_id() {
                    return None;
                }
                let peer = session.peer(data_pid)?;
                let short_name = match peer {
                    Peer::User(u) if !u.first_name.is_empty() => u.first_name.clone(),
                    _ => peer.name().to_owned(),
                };
                let h = session.history(data_pid);
                let unread_count = h.and_then(|h| h.unread_count).unwrap_or(0).max(0) as u32;
                let (_, online) = peer_status_text(peer, now);
                Some(TopPeerItem {
                    peer_id: app_pid,
                    short_name,
                    full_title: peer.name().to_owned(),
                    color_index: peer.common().color_index % 7,
                    online,
                    unread_count,
                    muted: peer.common().notify.is_muted(now),
                })
            })
            .collect()
    })
}

fn member_matches_query(member: &MemberEntry, query_lower: &str) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    member.name.to_ascii_lowercase().contains(query_lower)
        || member
            .username
            .as_deref()
            .is_some_and(|u| u.to_ascii_lowercase().contains(query_lower))
}

fn tl_chat_peer_and_bare_id(chat: &tl_enums::Chat) -> Option<(DataPeerId, i64)> {
    match chat {
        tl_enums::Chat::Empty(c) => {
            Some((DataPeerId::from_chat(tg_data::ChatId::from_tl(c.id)), c.id))
        }
        tl_enums::Chat::Chat(c) => {
            Some((DataPeerId::from_chat(tg_data::ChatId::from_tl(c.id)), c.id))
        }
        tl_enums::Chat::Forbidden(c) => {
            Some((DataPeerId::from_chat(tg_data::ChatId::from_tl(c.id)), c.id))
        }
        tl_enums::Chat::Channel(c) => Some((
            DataPeerId::from_channel(tg_data::ChannelId::from_tl(c.id)),
            c.id,
        )),
        tl_enums::Chat::ChannelForbidden(c) => Some((
            DataPeerId::from_channel(tg_data::ChannelId::from_tl(c.id)),
            c.id,
        )),
        tl_enums::Chat::Community(c) => Some((
            DataPeerId::from_channel(tg_data::ChannelId::from_tl(c.id)),
            c.id,
        )),
        tl_enums::Chat::CommunityForbidden(c) => Some((
            DataPeerId::from_channel(tg_data::ChannelId::from_tl(c.id)),
            c.id,
        )),
    }
}

/// Selects the best server-provided [`ReportOption`] matching a [`MessageReportReason`].
fn select_report_option(
    options: &[ReportOption],
    reason: MessageReportReason,
) -> Option<&ReportOption> {
    if options.is_empty() {
        return None;
    }
    let keywords: &[&str] = match reason {
        MessageReportReason::Spam => &["spam", "unsolicited"],
        MessageReportReason::Violence => &["violence", "threat", "terror"],
        MessageReportReason::ChildAbuse => &["child", "minor"],
        MessageReportReason::Pornography => &["porn", "adult", "sexual", "nude"],
        MessageReportReason::IllegalDrugs => &["drug", "narcotic", "illegal goods"],
        MessageReportReason::PersonalDetails => &["personal", "dox", "private data"],
        MessageReportReason::Copyright => &["copyright", "piracy"],
        MessageReportReason::Fake => &["fake", "scam", "fraud", "impersonat"],
        MessageReportReason::Other => &["other", "dislike"],
    };
    for opt in options {
        let lower = opt.text.to_ascii_lowercase();
        if keywords.iter().any(|kw| lower.contains(kw)) {
            return Some(opt);
        }
    }
    if let Some(other) = options
        .iter()
        .find(|o| o.text.to_ascii_lowercase().contains("other"))
    {
        return Some(other);
    }
    options.first()
}
