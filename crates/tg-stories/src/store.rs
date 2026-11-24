//! Port of `data/data_stories.h` and `data/data_stories.cpp`.
//!
//! Manages the account-wide stories store (`StoriesStore`):
//! - Per-peer story items, deletion tombstones, and expiration queue
//! - Per-peer active story sources (`NotHidden` and `Hidden`) with tdesktop's exact sort order
//!   (self first, unread before read, premium & service-notifications boost, latest story date)
//! - `stories.getAllStories` pagination (`state` / `next` / `has_more`)
//! - Read-state tracking (`read_till` per peer, `viewed_self_till`), 3 s `readStories` batching,
//!   5 s `incrementStoryViews` batching, `getPeerMaxIDs` polling/batching, and
//!   `getAllReadPeerStories` synchronization
//! - Story resolution (`getStoriesByID`, max 100 per batch) & `load_around` windowing
//! - Story polling (Chat: 5 min, Viewer: 1 min) and views polling (10 s)
//! - Next-story media preload queue (viewer > hidden sources > main sources)
//! - Saved / Archive / Custom Albums and pinned-to-top management
//! - Stealth mode state

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use tg_data::peer::StoriesState;
use tg_data::{FullStoryId, PeerId, ReactionId, StoryId, TimeId};
use tg_tl::enums as tl_enums;

use crate::ids::{
    StoriesIds, StoryAlbumIdsKey, insert_sorted_desc, remove_sorted_desc, remove_unsorted,
    reorder_story_ids, respecting_pinned,
};
use crate::story::{
    DEFAULT_MAX_PINNED_COUNT, RECENT_VIEWERS_MAX, STORIES_ALBUM_ID_ARCHIVE, STORIES_ALBUM_ID_SAVED,
    StealthMode, Story, StoryAlbum, StoryIdDates, StoryPeerContext, StoryUpdateFlags, StoryViews,
    StoryViewsCounts,
};
use crate::tl::{
    ParsedAlbums, ParsedAllStories, ParsedPeerStories, ParsedStoriesPage, ParsedStoryItem,
    ParsedStoryUpdate, RecentStoryInfo, albums_from_tl, all_stories_from_tl_with_existing,
    peer_stories_from_tl_with_existing, stories_page_from_tl_with_existing, story_item_from_tl,
    story_update_from_tl_with_existing,
};

// ---------------------------------------------------------------------------
// Constants matching data/data_stories.cpp
// ---------------------------------------------------------------------------

/// Maximum stories resolved in a single `stories.getStoriesByID` batch (`kMaxResolveTogether`).
pub const MAX_RESOLVE_TOGETHER: usize = 100;

/// Skip `loadAround` if all stories within +/- this radius are already loaded (`kIgnorePreloadAroundIfLoaded`).
pub const IGNORE_PRELOAD_AROUND_IF_LOADED: usize = 15;

/// Radius of stories resolved by `loadAround` (`kPreloadAroundCount`).
pub const PRELOAD_AROUND_COUNT: usize = 30;

/// Debounce delay in ms before flushing `stories.readStories` (`kMarkAsReadDelay` = 3 s).
pub const MARK_AS_READ_DELAY_MS: i64 = 3_000;

/// Batch delay in ms before flushing `stories.incrementStoryViews` (`kIncrementViewsDelay` = 5 s).
pub const INCREMENT_VIEWS_DELAY_MS: i64 = 5_000;

/// Batching delay in ms for `stories.getPeerMaxIDs` polling.
pub const PEER_MAX_IDS_POLL_DELAY_MS: i64 = 500;

/// Maximum peers queried in one `stories.getPeerMaxIDs` call.
pub const MAX_PEER_MAX_IDS_BATCH: usize = 100;

/// First page size for archive stories (`kArchiveFirstPerPage`).
pub const ARCHIVE_FIRST_PER_PAGE: i32 = 30;

/// Subsequent page size for archive stories (`kArchivePerPage`).
pub const ARCHIVE_PER_PAGE: i32 = 100;

/// First page size for saved/pinned stories (`kSavedFirstPerPage`).
pub const SAVED_FIRST_PER_PAGE: i32 = 30;

/// Subsequent page size for saved/pinned stories (`kSavedPerPage`).
pub const SAVED_PER_PAGE: i32 = 100;

/// Number of leading sources preloaded in each strip (`kMaxPreloadSources`).
pub const MAX_PRELOAD_SOURCES: usize = 10;

/// Keep current preload running if it remains within this prefix of the queue (`kStillPreloadFromFirst`).
pub const STILL_PRELOAD_FROM_FIRST: usize = 3;

/// Maximum ring segments shown per source (`kMaxSegmentsCount`).
pub const MAX_SEGMENTS_COUNT: u32 = 180;

/// Story polling interval while visible in chat (`kPollingIntervalChat` = 5 min).
pub const POLLING_INTERVAL_CHAT_SECS: TimeId = 5 * 60;

/// Story polling interval while open in viewer (`kPollingIntervalViewer` = 1 min).
pub const POLLING_INTERVAL_VIEWER_SECS: TimeId = 60;

/// Views polling interval in viewer (`kPollViewsInterval` = 10 s).
pub const POLL_VIEWS_INTERVAL_MS: i64 = 10_000;

/// Page size for `stories.getStoryViewsList` / `getStoryReactionsList` (`kViewsPerPage`).
pub const VIEWS_PER_PAGE: i32 = 50;

/// Page size when polling views in viewer (`kPollingViewsPerPage`).
pub const POLLING_VIEWS_PER_PAGE: i32 = RECENT_VIEWERS_MAX as i32;

/// Maximum delay (seconds) scheduled on the expiry timer (matches `std::min(..., 86'400)`).
pub const MAX_EXPIRE_TIMER_SECS: TimeId = 86_400;

// ---------------------------------------------------------------------------
// Core types matching data/data_stories.h
// ---------------------------------------------------------------------------

/// Reason a `Story` lookup failed (`Data::NoStory`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum NoStory {
    /// Not yet fetched from the server.
    Unknown,
    /// Confirmed deleted or expired without archive/profile copy.
    Deleted,
}

/// Which top-level stories strip a peer belongs to (`Data::StorySourcesList`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum StorySourcesList {
    /// Main chats-list stories bar (`NotHidden` = 0).
    NotHidden = 0,
    /// Archived/hidden stories bar (`Hidden` = 1).
    Hidden = 1,
}

impl StorySourcesList {
    /// Array index (`0` for `NotHidden`, `1` for `Hidden`).
    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Returns the opposite list.
    #[inline]
    pub const fn other(self) -> Self {
        match self {
            Self::NotHidden => Self::Hidden,
            Self::Hidden => Self::NotHidden,
        }
    }
}

/// Context in which a story is being viewed (`Data::StoriesContext`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum StoriesContext {
    /// Standalone single story (e.g. forwarded message or link).
    #[default]
    Single,
    /// All active stories of a single peer (without siblings).
    Peer,
    /// A peer's profile saved stories (`0`), archive (`-1`), or custom album (`> 0`).
    Album(i32),
    /// Main or Hidden stories strip (enables left/right sibling peer navigation).
    SourcesList(StorySourcesList),
}

/// Summary metadata of a peer's story source used for sorting and strip display
/// (`Data::StoriesSourceInfo`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct StoriesSourceInfo {
    /// Source peer ID.
    pub id: PeerId,
    /// Creation timestamp of the newest active story (`0` if empty).
    pub last: TimeId,
    /// Total active story count (clamped to `MAX_SEGMENTS_COUNT`).
    pub count: u32,
    /// Unread active story count (clamped to `MAX_SEGMENTS_COUNT`).
    pub unread_count: u32,
    /// Whether the peer is a Telegram Premium user.
    pub premium: bool,
    /// Whether the peer has an active live video stream story.
    pub has_video_stream: bool,
}

/// Computes tdesktop's exact descending sort key for `StoriesSourceInfo`
/// (`Stories::sort` in `data_stories.cpp:1066-1082`).
///
/// Priority order (highest first):
/// 1. Self (`1 << 50`)
/// 2. Unread stories (`1 << 49`)
/// 3. Premium user (`1 << 47`) / Telegram service notifications (`777000`, `1 << 47`)
/// 4. Newest story timestamp (`last`)
/// 5. `PeerId` tie-breaker
#[inline]
pub fn source_sort_key(info: StoriesSourceInfo, self_peer_id: PeerId) -> (i64, PeerId) {
    let mut key = i64::from(info.last);
    if info.premium {
        key += 1_i64 << 47;
    }
    if info.id == PeerId::SERVICE_NOTIFICATIONS {
        key += 1_i64 << 47;
    }
    if info.unread_count > 0 {
        key += 1_i64 << 49;
    }
    if info.id == self_peer_id {
        key += 1_i64 << 50;
    }
    (key, info.id)
}

/// Active stories source for one peer (`Data::StoriesSource`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoriesSource {
    /// Owner peer ID.
    pub peer_id: PeerId,
    /// Ordered set of active stories (ascending by `StoryIdDates`).
    pub ids: BTreeSet<StoryIdDates>,
    /// Maximum read story ID for this peer.
    pub read_till: StoryId,
    /// Whether this peer's stories are hidden/archived.
    pub hidden: bool,
    /// Whether this peer is a Telegram Premium user.
    pub premium: bool,
    /// Whether this peer has a live video stream story.
    pub has_video_stream: bool,
}

impl StoriesSource {
    /// Builds the compact `StoriesSourceInfo` (`StoriesSource::info`).
    pub fn info(&self) -> StoriesSourceInfo {
        let last = self.ids.iter().next_back().map_or(0, |s| s.date);
        let count = (self.ids.len() as u32).min(MAX_SEGMENTS_COUNT);
        let unread_count = (self.unread_count() as u32).min(MAX_SEGMENTS_COUNT);
        StoriesSourceInfo {
            id: self.peer_id,
            last,
            count,
            unread_count,
            premium: self.premium,
            has_video_stream: self.has_video_stream,
        }
    }

    /// Number of active stories with `id > read_till` (`StoriesSource::unreadCount`).
    pub fn unread_count(&self) -> usize {
        let bound = StoryIdDates {
            id: self.read_till.saturating_add(1),
            date: TimeId::MIN,
            expires: TimeId::MIN,
            video_stream: false,
        };
        self.ids.range(bound..).count()
    }

    /// First unread story if any, else the oldest active story (`StoriesSource::toOpen`).
    pub fn to_open(&self) -> Option<StoryIdDates> {
        let bound = StoryIdDates {
            id: self.read_till.saturating_add(1),
            date: TimeId::MIN,
            expires: TimeId::MIN,
            video_stream: false,
        };
        self.ids
            .range(bound..)
            .next()
            .or_else(|| self.ids.iter().next())
            .copied()
    }

    /// Returns the story at 0-based `index` within ascending `ids`.
    pub fn get_index(&self, index: usize) -> Option<StoryIdDates> {
        self.ids.iter().nth(index).copied()
    }

    /// Returns the 0-based index of `story_id` within `ids`.
    pub fn index_of(&self, story_id: StoryId) -> Option<usize> {
        self.ids.iter().position(|s| s.id == story_id)
    }

    /// Returns the newest story ID, or `0` if empty.
    pub fn max_id(&self) -> StoryId {
        self.ids.iter().next_back().map_or(0, |s| s.id)
    }
}

/// Source origin for `PeerStories` ingestion (`Stories::ParseSource`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ParseSource {
    /// Came from `stories.getAllStories` (my stories strip).
    MyStrip,
    /// Came from a direct `stories.getPeerStories` or peer-full fetch.
    DirectRequest,
}

/// Additional peer traits needed by `StoriesStore` to place peers in strips and check permissions.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct PeerStoryMeta {
    /// Core story peer context (self/channel/user/verified/premium/rights).
    pub ctx: StoryPeerContext,
    /// Whether peer's stories are hidden/archived (`peer->hasStoriesHidden()`).
    pub stories_hidden: bool,
    /// Whether current account is a member of this channel (`channel->amIn()`).
    pub channel_am_in: bool,
    /// Whether user is a contact (`user->isContact()`).
    pub is_contact: bool,
    /// Whether user is a bot (`user->isBot()`).
    pub is_bot: bool,
    /// Whether user is a stories correspondent (`user->storiesCorrespondent()`).
    pub stories_correspondent: bool,
}

impl PeerStoryMeta {
    /// Derives `PeerStoryMeta` from a `tg_data::Peer`.
    pub fn from_peer(peer: &tg_data::Peer) -> Self {
        let ctx = StoryPeerContext::from_peer(peer);
        let (stories_hidden, channel_am_in, is_contact, is_bot, stories_correspondent) = match peer
        {
            tg_data::Peer::User(u) => (
                u.flags.contains(tg_data::UserFlags::STORIES_HIDDEN),
                false,
                u.flags.contains(tg_data::UserFlags::CONTACT),
                u.is_bot(),
                u.flags.contains(tg_data::UserFlags::STORIES_CORRESPONDENT),
            ),
            tg_data::Peer::Channel(ch) => (
                ch.flags.contains(tg_data::ChannelFlags::STORIES_HIDDEN),
                ch.am_in(),
                false,
                false,
                false,
            ),
            tg_data::Peer::Chat(_) => (false, false, false, false, false),
        };
        Self {
            ctx,
            stories_hidden,
            channel_am_in,
            is_contact,
            is_bot,
            stories_correspondent,
        }
    }

    /// Determines whether this peer should appear in the dialogs stories strip
    /// (`appendToStrip` in `data_stories.cpp:517-530`).
    pub fn should_append_to_strip(&self, source: ParseSource) -> bool {
        if self.ctx.is_self {
            true
        } else if self.ctx.is_channel {
            self.channel_am_in
        } else if self.ctx.is_user {
            source == ParseSource::MyStrip
                || self.stories_correspondent
                || self.ctx.is_service_user
                || self.is_contact
                || self.is_bot
        } else {
            false
        }
    }

    /// Returns `true` if hiding this peer should remove it from hints/strip rather than archiving
    /// (`byHints` in `data_stories.cpp:1361-1365`).
    pub const fn is_by_hints(&self) -> bool {
        self.ctx.is_user && !self.is_bot && !self.is_contact && !self.ctx.is_service_user
    }
}

/// Computed peer story ring state (`Stories::PeerSourceState`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct PeerSourceState {
    /// Highest active story ID.
    pub max_id: StoryId,
    /// Maximum read story ID (`<= max_id`).
    pub read_till: StoryId,
    /// Whether the peer has an active live video stream.
    pub has_video_stream: bool,
}

impl PeerSourceState {
    /// Converts to `tg_data::StoriesState` (`Stories::updatePeerStoriesState`).
    pub const fn to_stories_state(self) -> StoriesState {
        if self.has_video_stream {
            StoriesState::HasVideoStream
        } else if self.max_id == 0 {
            StoriesState::None
        } else if self.max_id <= self.read_till {
            StoriesState::HasRead
        } else {
            StoriesState::HasUnread
        }
    }
}

/// Polling mode for an individual story (`Stories::Polling`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum PollingKind {
    /// Referenced in a chat history message (5 min interval).
    Chat,
    /// Actively open in the story viewer (1 min interval + 10 s views polling).
    Viewer,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct PollingSettings {
    chat: u32,
    viewer: u32,
}

impl PollingSettings {
    fn interval_secs(&self) -> TimeId {
        if self.viewer > 0 {
            POLLING_INTERVAL_VIEWER_SECS
        } else {
            POLLING_INTERVAL_CHAT_SECS
        }
    }
}

/// Outbound RPC / side-effect actions requested by `StoriesStore` for the network layer to execute.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoriesAction {
    /// Call `stories.getAllStories`.
    GetAllStories {
        /// Which strip list to load.
        list: StorySourcesList,
        /// State token (`None` for first page).
        state: Option<String>,
    },
    /// Call `stories.getPeerStories`.
    GetPeerStories {
        /// Peer to fetch.
        peer_id: PeerId,
    },
    /// Call `stories.getStoriesByID` for a batch of story IDs (<= 100).
    GetStoriesById {
        /// Owning peer ID.
        peer_id: PeerId,
        /// Story IDs to resolve.
        ids: Vec<StoryId>,
    },
    /// Call `stories.readStories` to commit max read ID.
    ReadStories {
        /// Peer whose stories were read.
        peer_id: PeerId,
        /// Highest read story ID.
        max_id: StoryId,
    },
    /// Call `stories.incrementStoryViews` for expired in-profile stories.
    IncrementStoryViews {
        /// Owning peer ID.
        peer_id: PeerId,
        /// Story IDs viewed.
        ids: Vec<StoryId>,
    },
    /// Call `stories.getAllReadPeerStories` to hydrate initial read-till map.
    GetAllReadPeerStories,
    /// Call `stories.getPeerMaxIDs` for a batch of peers.
    GetPeerMaxIds {
        /// Peer IDs to query.
        peer_ids: Vec<PeerId>,
    },
    /// Call `stories.getStoryViewsList` (self stories).
    GetStoryViewsList {
        /// Story peer (self).
        peer_id: PeerId,
        /// Story ID.
        story_id: StoryId,
        /// Pagination offset.
        offset: String,
        /// Page limit (`VIEWS_PER_PAGE` or `POLLING_VIEWS_PER_PAGE`).
        limit: i32,
    },
    /// Call `stories.getStoryReactionsList` (channel stories).
    GetStoryReactionsList {
        /// Channel peer ID.
        peer_id: PeerId,
        /// Story ID.
        story_id: StoryId,
        /// Pagination offset.
        offset: String,
        /// Page limit.
        limit: i32,
    },
    /// Call `stories.getStoriesViews` (view counts polling for non-self peer).
    GetStoriesViews {
        /// Peer ID.
        peer_id: PeerId,
        /// Story IDs.
        ids: Vec<StoryId>,
    },
    /// Call `stories.getPinnedStories` / `getStoriesArchive` / `getAlbumStories`.
    LoadAlbumStories {
        /// Owner peer ID.
        peer_id: PeerId,
        /// Album ID (`0` = saved, `-1` = archive, `> 0` = custom).
        album_id: i32,
        /// Pagination offset (`last_id` for saved/archive, count offset for custom albums).
        offset: i32,
        /// Page size.
        limit: i32,
    },
    /// Call `stories.getAlbums`.
    GetAlbums {
        /// Owner peer ID.
        peer_id: PeerId,
        /// Last known albums hash.
        hash: i64,
    },
    /// Call `stories.activateStealthMode(past: true, future: true)`.
    ActivateStealthMode,
    /// Call `stories.sendReaction`.
    SendReaction {
        /// Target story.
        story_id: FullStoryId,
        /// New reaction (`None` clears reaction).
        reaction: Option<ReactionId>,
    },
    /// Call `stories.deleteStories`.
    DeleteStories {
        /// Owner peer ID.
        peer_id: PeerId,
        /// Story IDs to delete.
        ids: Vec<StoryId>,
    },
    /// Call `stories.togglePinned` (profile save / archive).
    ToggleInProfile {
        /// Owner peer ID.
        peer_id: PeerId,
        /// Story IDs.
        ids: Vec<StoryId>,
        /// `true` to save to profile, `false` to move to archive only.
        in_profile: bool,
    },
    /// Call `stories.togglePinnedToTop`.
    TogglePinnedToTop {
        /// Owner peer ID.
        peer_id: PeerId,
        /// Full ordered list of pinned story IDs.
        ids: Vec<StoryId>,
    },
    /// Call `stories.togglePeerStoriesHidden`.
    TogglePeerStoriesHidden {
        /// Peer ID.
        peer_id: PeerId,
        /// `true` to hide/archive, `false` to unhide.
        hidden: bool,
    },
    /// Call `stories.updateAlbum` with reordered story list.
    ReorderAlbumStories {
        /// Owner peer ID.
        peer_id: PeerId,
        /// Custom album ID.
        album_id: i32,
        /// New story ordering.
        order: Vec<StoryId>,
    },
    /// Start preloading media for `story_id`.
    StartPreload {
        /// Target story ID to preload.
        story_id: FullStoryId,
    },
    /// Cancel in-flight media preload for `story_id`.
    CancelPreload {
        /// Cancelled story ID.
        story_id: FullStoryId,
    },
}

/// Notifications emitted by `StoriesStore` for observers (viewer, dialogs bar, session peers).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoriesEvent {
    /// Sources list (`NotHidden` or `Hidden`) changed or finished loading.
    SourcesChanged(StorySourcesList),
    /// Hidden sources total/unread count changed (for archive folder badge).
    HiddenCountsChanged {
        /// Total hidden sources.
        total: usize,
        /// Hidden sources with `unread_count > 0`.
        unread: usize,
    },
    /// A specific peer's `StoriesSource` changed.
    SourceChanged(PeerId),
    /// Stories were resolved/updated for a peer.
    ItemsChanged(PeerId),
    /// A specific story was mutated or destroyed.
    StoryUpdated {
        /// Story ID.
        id: FullStoryId,
        /// Change flags.
        flags: StoryUpdateFlags,
    },
    /// Peer's avatar story ring state changed.
    PeerStoriesStateChanged {
        /// Peer ID.
        peer_id: PeerId,
        /// New ring state.
        state: StoriesState,
    },
    /// Self stories viewed-till watermark changed.
    ViewedSelfTillChanged(StoryId),
    /// Album story ID list changed.
    AlbumIdsChanged(StoryAlbumIdsKey),
    /// Stealth mode timestamps changed.
    StealthModeChanged(StealthMode),
}

#[derive(Clone, Debug)]
struct AlbumSet {
    ids: StoriesIds,
    album_known_in_archive: BTreeSet<StoryId>,
    total: i32,
    last_id: StoryId,
    loaded: bool,
    request_in_flight: bool,
}

impl Default for AlbumSet {
    fn default() -> Self {
        Self {
            ids: StoriesIds::default(),
            album_known_in_archive: BTreeSet::new(),
            total: -1,
            last_id: 0,
            loaded: false,
            request_in_flight: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct PeerAlbums {
    list: Vec<StoryAlbum>,
    sets: BTreeMap<i32, AlbumSet>,
    hash: i64,
    request_in_flight: bool,
}

#[derive(Clone, Debug, Default)]
struct SourcesListState {
    sources: Vec<StoriesSourceInfo>,
    loaded: bool,
    state: String,
    request_in_flight: bool,
    to_preload: Vec<FullStoryId>,
}

/// Account-wide pure-logic stories store (`Data::Stories`).
#[derive(Debug)]
pub struct StoriesStore {
    self_peer_id: PeerId,
    max_pinned_count: usize,

    // Peer metadata cache supplied by caller / session.
    peer_meta: HashMap<PeerId, PeerStoryMeta>,

    // Loaded stories & tombstones.
    stories: HashMap<PeerId, BTreeMap<StoryId, Story>>,
    deleted: HashSet<FullStoryId>,
    expiring: BTreeMap<TimeId, BTreeSet<FullStoryId>>,
    next_expire_check_secs: Option<TimeId>,

    // Active peer story sources and NotHidden / Hidden sorted lists.
    all_sources: HashMap<PeerId, StoriesSource>,
    lists: [SourcesListState; 2],

    // Resolve batching (`stories.getStoriesByID`).
    resolve_pending: BTreeMap<PeerId, BTreeSet<StoryId>>,
    resolve_in_flight: BTreeMap<PeerId, BTreeSet<StoryId>>,
    requesting_peer_stories: HashSet<PeerId>,

    // Read state & timers (`readStories`, `incrementStoryViews`, `getPeerMaxIDs`, `getAllReadPeerStories`).
    read_till: HashMap<PeerId, StoryId>,
    viewed_self_till: StoryId,
    read_till_received: bool,
    read_tills_in_flight: bool,
    pending_peer_recent: HashMap<PeerId, RecentStoryInfo>,

    mark_read_pending: BTreeSet<PeerId>,
    mark_read_in_flight: HashSet<PeerId>,
    mark_read_deadline_ms: Option<i64>,

    increment_views_pending: BTreeMap<PeerId, BTreeSet<StoryId>>,
    increment_views_in_flight: HashSet<PeerId>,
    increment_views_deadline_ms: Option<i64>,

    peer_max_ids_pending: BTreeSet<PeerId>,
    peer_max_ids_in_flight: HashSet<PeerId>,
    peer_max_ids_deadline_ms: Option<i64>,

    // Polling (`Chat`: 5m, `Viewer`: 1m, views: 10s).
    polling_settings: HashMap<FullStoryId, PollingSettings>,
    polling_views: BTreeSet<FullStoryId>,
    polling_deadline_secs: Option<TimeId>,
    polling_views_deadline_ms: Option<i64>,
    views_request_in_flight: bool,

    // Preloading.
    preloaded: HashSet<FullStoryId>,
    to_preload_viewer: Vec<FullStoryId>,
    current_preload: Option<FullStoryId>,
    preloading_main_counter: usize,
    preloading_hidden_counter: usize,

    // Albums: saved (0), archive (-1), custom (>0).
    archive: HashMap<PeerId, AlbumSet>,
    saved: HashMap<PeerId, AlbumSet>,
    albums: HashMap<PeerId, PeerAlbums>,

    // Stealth mode.
    stealth_mode: StealthMode,

    // Emitted actions & events drained by caller.
    actions: Vec<StoriesAction>,
    events: Vec<StoriesEvent>,
}

impl StoriesStore {
    /// Creates a new `StoriesStore` for `self_peer_id`.
    pub fn new(self_peer_id: PeerId) -> Self {
        let mut store = Self {
            self_peer_id,
            max_pinned_count: DEFAULT_MAX_PINNED_COUNT,
            peer_meta: HashMap::new(),
            stories: HashMap::new(),
            deleted: HashSet::new(),
            expiring: BTreeMap::new(),
            next_expire_check_secs: None,
            all_sources: HashMap::new(),
            lists: [SourcesListState::default(), SourcesListState::default()],
            resolve_pending: BTreeMap::new(),
            resolve_in_flight: BTreeMap::new(),
            requesting_peer_stories: HashSet::new(),
            read_till: HashMap::new(),
            viewed_self_till: 0,
            read_till_received: false,
            read_tills_in_flight: false,
            pending_peer_recent: HashMap::new(),
            mark_read_pending: BTreeSet::new(),
            mark_read_in_flight: HashSet::new(),
            mark_read_deadline_ms: None,
            increment_views_pending: BTreeMap::new(),
            increment_views_in_flight: HashSet::new(),
            increment_views_deadline_ms: None,
            peer_max_ids_pending: BTreeSet::new(),
            peer_max_ids_in_flight: HashSet::new(),
            peer_max_ids_deadline_ms: None,
            polling_settings: HashMap::new(),
            polling_views: BTreeSet::new(),
            polling_deadline_secs: None,
            polling_views_deadline_ms: None,
            views_request_in_flight: false,
            preloaded: HashSet::new(),
            to_preload_viewer: Vec::new(),
            current_preload: None,
            preloading_main_counter: 0,
            preloading_hidden_counter: 0,
            archive: HashMap::new(),
            saved: HashMap::new(),
            albums: HashMap::new(),
            stealth_mode: StealthMode::default(),
            actions: Vec::new(),
            events: Vec::new(),
        };
        store.set_peer_meta(
            self_peer_id,
            PeerStoryMeta {
                ctx: StoryPeerContext {
                    is_self: true,
                    is_user: true,
                    can_post_stories: true,
                    can_edit_stories: true,
                    can_delete_stories: true,
                    ..StoryPeerContext::default()
                },
                ..PeerStoryMeta::default()
            },
        );
        store
    }

    /// Returns the owning account's `PeerId`.
    #[inline]
    pub const fn self_peer_id(&self) -> PeerId {
        self.self_peer_id
    }

    /// Sets the configured maximum number of stories pinned to top of a profile.
    #[inline]
    pub fn set_max_pinned_count(&mut self, max: usize) {
        self.max_pinned_count = max;
    }

    /// Returns the configured maximum pinned-to-top count (`Stories::maxPinnedCount`).
    #[inline]
    pub const fn max_pinned_count(&self) -> usize {
        self.max_pinned_count
    }

    /// Registers or updates story-relevant metadata for `peer_id`.
    pub fn set_peer_meta(&mut self, peer_id: PeerId, meta: PeerStoryMeta) {
        self.peer_meta.insert(peer_id, meta);
    }

    /// Registers or updates metadata from a `tg_data::Peer`.
    pub fn update_peer(&mut self, peer: &tg_data::Peer) {
        self.set_peer_meta(peer.id(), PeerStoryMeta::from_peer(peer));
    }

    /// Returns metadata for `peer_id`, falling back to sensible defaults derived from `PeerId`.
    pub fn peer_meta(&self, peer_id: PeerId) -> PeerStoryMeta {
        if let Some(&meta) = self.peer_meta.get(&peer_id) {
            return meta;
        }
        let is_self = peer_id == self.self_peer_id;
        PeerStoryMeta {
            ctx: StoryPeerContext {
                is_self,
                is_user: peer_id.is_user(),
                is_channel: peer_id.is_channel(),
                is_service_user: peer_id == PeerId::SERVICE_NOTIFICATIONS,
                can_post_stories: is_self,
                can_edit_stories: is_self,
                can_delete_stories: is_self,
                ..StoryPeerContext::default()
            },
            ..PeerStoryMeta::default()
        }
    }

    /// Drains all queued outbound `StoriesAction`s.
    pub fn take_actions(&mut self) -> Vec<StoriesAction> {
        std::mem::take(&mut self.actions)
    }

    /// Drains all queued `StoriesEvent` notifications.
    pub fn take_events(&mut self) -> Vec<StoriesEvent> {
        std::mem::take(&mut self.events)
    }

    /// Returns read-only access to pending actions (useful in tests/inspection).
    pub fn pending_actions(&self) -> &[StoriesAction] {
        &self.actions
    }

    /// Returns read-only access to pending events (useful in tests/inspection).
    pub fn pending_events(&self) -> &[StoriesEvent] {
        &self.events
    }

    // -----------------------------------------------------------------------
    // Lookup & Source Accessors
    // -----------------------------------------------------------------------

    /// Looks up a loaded `Story` by `FullStoryId` (`Stories::lookup`).
    pub fn lookup(&self, id: FullStoryId) -> Result<&Story, NoStory> {
        if let Some(map) = self.stories.get(&id.peer)
            && let Some(story) = map.get(&id.story)
        {
            return Ok(story);
        }
        if self.deleted.contains(&id) {
            Err(NoStory::Deleted)
        } else {
            Err(NoStory::Unknown)
        }
    }

    /// Mutable lookup for a loaded `Story`.
    pub fn lookup_mut(&mut self, id: FullStoryId) -> Option<&mut Story> {
        self.stories
            .get_mut(&id.peer)
            .and_then(|map| map.get_mut(&id.story))
    }

    /// Returns the active `StoriesSource` for `peer_id`, if any (`Stories::source`).
    pub fn source(&self, peer_id: PeerId) -> Option<&StoriesSource> {
        self.all_sources.get(&peer_id)
    }

    /// Returns the sorted source list for `NotHidden` or `Hidden` (`Stories::sources`).
    pub fn sources(&self, list: StorySourcesList) -> &[StoriesSourceInfo] {
        &self.lists[list.index()].sources
    }

    /// Returns `true` if the sources list has been fetched at least once (`Stories::sourcesKnown`).
    pub fn sources_known(&self, list: StorySourcesList) -> bool {
        let state = &self.lists[list.index()];
        state.loaded || !state.state.is_empty()
    }

    /// Returns the current `stories.getAllStories` pagination state string for `list`.
    pub fn sources_state(&self, list: StorySourcesList) -> &str {
        &self.lists[list.index()].state
    }

    /// Returns `true` if all pages of `list` have been loaded (`!has_more`).
    pub fn sources_loaded(&self, list: StorySourcesList) -> bool {
        self.lists[list.index()].loaded
    }

    /// Returns the current `StealthMode` (`Stories::stealthMode`).
    #[inline]
    pub const fn stealth_mode(&self) -> StealthMode {
        self.stealth_mode
    }

    /// Returns the highest self story ID viewed by the user (`Stories::viewedSelfTill`).
    #[inline]
    pub const fn viewed_self_till(&self) -> StoryId {
        self.viewed_self_till
    }

    /// Sets `viewed_self_till` monotonically (`Stories::setViewedSelfTill`).
    pub fn set_viewed_self_till(&mut self, till_id: StoryId) {
        if till_id <= self.viewed_self_till {
            return;
        }
        self.viewed_self_till = till_id;
        self.events
            .push(StoriesEvent::ViewedSelfTillChanged(till_id));
    }

    /// Returns the stored max read story ID for `peer_id`.
    pub fn read_till(&self, peer_id: PeerId) -> StoryId {
        self.read_till.get(&peer_id).copied().unwrap_or(0)
    }

    /// Returns `true` if `story` is unread (`Stories::isUnread`).
    ///
    /// If read-tills have not been received yet and `peer_id` is absent, queues
    /// `GetAllReadPeerStories` and returns `false` matching C++.
    pub fn is_unread(&mut self, id: FullStoryId) -> bool {
        match self.read_till.get(&id.peer) {
            None if !self.read_till_received => {
                self.request_read_tills();
                false
            }
            opt => id.story > opt.copied().unwrap_or(0),
        }
    }

    /// Returns `true` if `peer_id` has a stories archive (`Stories::hasArchive`).
    pub fn has_archive(&self, peer_id: PeerId) -> bool {
        self.peer_meta(peer_id).ctx.has_archive()
    }

    // -----------------------------------------------------------------------
    // Pagination & Loading
    // -----------------------------------------------------------------------

    /// Requests the next page of `stories.getAllStories` for `list` (`Stories::loadMore`).
    pub fn load_more(&mut self, list: StorySourcesList) {
        let entry = &mut self.lists[list.index()];
        if entry.request_in_flight || entry.loaded {
            return;
        }
        entry.request_in_flight = true;
        let state = (!entry.state.is_empty()).then(|| entry.state.clone());
        self.actions
            .push(StoriesAction::GetAllStories { list, state });
    }

    /// Requests active stories for a single peer (`Stories::requestPeerStories`).
    pub fn request_peer_stories(&mut self, peer_id: PeerId) {
        if self.requesting_peer_stories.insert(peer_id) {
            self.actions.push(StoriesAction::GetPeerStories { peer_id });
        }
    }

    /// Marks `stories.getPeerStories` as failed, removing `peer_id` from both strips.
    pub fn fail_peer_stories(&mut self, peer_id: PeerId) {
        self.requesting_peer_stories.remove(&peer_id);
        self.apply_deleted_from_sources(peer_id, StorySourcesList::NotHidden);
        self.apply_deleted_from_sources(peer_id, StorySourcesList::Hidden);
    }

    /// Requests `stories.getAllReadPeerStories` if not yet received or in flight.
    pub fn request_read_tills(&mut self) {
        if self.read_till_received || self.read_tills_in_flight {
            return;
        }
        self.read_tills_in_flight = true;
        self.actions.push(StoriesAction::GetAllReadPeerStories);
    }

    /// Marks `stories.getAllReadPeerStories` response received and updates pending peer states.
    pub fn finish_read_tills(&mut self) {
        self.read_tills_in_flight = false;
        self.read_till_received = true;
        let pending = std::mem::take(&mut self.pending_peer_recent);
        for (peer_id, recent) in pending {
            self.update_peer_stories_state(peer_id, Some(recent));
        }
    }

    /// Computes a peer's story ring state from a `RecentStoryInfo` (`Stories::peerSourceState`).
    pub fn peer_source_state(
        &mut self,
        peer_id: PeerId,
        recent: RecentStoryInfo,
    ) -> Option<PeerSourceState> {
        if self.read_till_received || self.read_till.contains_key(&peer_id) {
            let till = self.read_till.get(&peer_id).copied().unwrap_or(0);
            Some(PeerSourceState {
                max_id: recent.max_id,
                read_till: recent.max_id.min(till),
                has_video_stream: recent.live,
            })
        } else {
            self.request_read_tills();
            self.pending_peer_recent.insert(peer_id, recent);
            None
        }
    }

    // -----------------------------------------------------------------------
    // Applying TL Responses & Updates
    // -----------------------------------------------------------------------

    /// Applies a TL `stories.AllStories` response for `list`.
    pub fn apply_tl_all_stories(
        &mut self,
        list: StorySourcesList,
        all: &tl_enums::stories::AllStories,
        now: TimeId,
    ) {
        // Pass existing stories so `min: true` items preserve `sent_reaction`, `out`,
        // privacy exceptions, and live stream metadata (`data/data_story.cpp:787-839`).
        let self_id = self.self_peer_id;
        let peer_meta = &self.peer_meta;
        let stories = &self.stories;
        let parsed = all_stories_from_tl_with_existing(
            all,
            now,
            |pid| {
                peer_meta
                    .get(&pid)
                    .map(|m| m.ctx)
                    .unwrap_or_else(|| default_ctx(pid, self_id))
            },
            |pid, sid| stories.get(&pid).and_then(|m| m.get(&sid)),
        );
        self.apply_all_stories(list, parsed, now);
    }

    /// Applies a parsed `ParsedAllStories` response for `list` (`Stories::loadMore` callback).
    pub fn apply_all_stories(
        &mut self,
        list: StorySourcesList,
        all: ParsedAllStories,
        now: TimeId,
    ) {
        let idx = list.index();
        self.lists[idx].request_in_flight = false;
        let was_known = self.sources_known(list);

        let stealth = match all {
            ParsedAllStories::NotModified {
                state,
                stealth_mode,
            } => {
                if !state.is_empty() {
                    self.lists[idx].state = state;
                }
                stealth_mode
            }
            ParsedAllStories::All {
                has_more,
                state,
                peer_stories,
                stealth_mode,
                ..
            } => {
                self.lists[idx].state = state;
                self.lists[idx].loaded = !has_more;
                for ps in peer_stories {
                    self.apply_peer_stories(ps, ParseSource::MyStrip, now);
                }
                stealth_mode
            }
        };

        self.apply_stealth_mode(stealth);
        if !was_known && self.sources_known(list) {
            self.events.push(StoriesEvent::SourcesChanged(list));
        }
    }

    /// Marks `load_more` for `list` as failed so it can be retried.
    pub fn fail_load_more(&mut self, list: StorySourcesList) {
        self.lists[list.index()].request_in_flight = false;
    }

    /// Applies a TL `PeerStories` payload from a direct request (`stories.getPeerStories`).
    pub fn apply_tl_peer_stories(
        &mut self,
        peer_stories: &tl_enums::PeerStories,
        source: ParseSource,
        now: TimeId,
    ) {
        // Pass existing stories so `min: true` items do not wipe local fields
        // (`Stories::parseAndApply` in `data/data_stories.cpp:557-586`).
        let self_id = self.self_peer_id;
        let peer_meta = &self.peer_meta;
        let stories = &self.stories;
        let parsed = peer_stories_from_tl_with_existing(
            peer_stories,
            now,
            |pid| {
                peer_meta
                    .get(&pid)
                    .map(|m| m.ctx)
                    .unwrap_or_else(|| default_ctx(pid, self_id))
            },
            |pid, sid| stories.get(&pid).and_then(|m| m.get(&sid)),
        );
        self.apply_peer_stories(parsed, source, now);
    }

    /// Applies a `ParsedPeerStories` bundle (`Stories::parseAndApply(MTPPeerStories)`).
    pub fn apply_peer_stories(
        &mut self,
        parsed: ParsedPeerStories,
        source: ParseSource,
        now: TimeId,
    ) {
        let peer_id = parsed.peer_id;
        self.requesting_peer_stories.remove(&peer_id);

        let prev_read = self.read_till.get(&peer_id).copied().unwrap_or(0);
        let mut read_till = parsed.max_read_id.max(prev_read);
        let meta = self.peer_meta(peer_id);

        let mut ids = BTreeSet::new();
        let mut has_video_stream = parsed.has_video_stream;
        for item in parsed.items {
            if let Some(id_dates) = self.ingest_parsed_item(peer_id, item, now) {
                if id_dates.video_stream {
                    has_video_stream = true;
                }
                ids.insert(id_dates);
            }
        }

        if ids.is_empty() {
            self.all_sources.remove(&peer_id);
            self.apply_deleted_from_sources(peer_id, StorySourcesList::NotHidden);
            self.apply_deleted_from_sources(peer_id, StorySourcesList::Hidden);
            self.events.push(StoriesEvent::PeerStoriesStateChanged {
                peer_id,
                state: StoriesState::None,
            });
            return;
        }

        if meta.ctx.is_self
            && let Some(last) = ids.iter().next_back()
        {
            read_till = last.id;
        }
        self.read_till.insert(peer_id, read_till);

        let src = StoriesSource {
            peer_id,
            ids,
            read_till,
            hidden: meta.stories_hidden,
            premium: meta.ctx.is_user && meta.ctx.is_premium,
            has_video_stream,
        };
        let info = src.info();
        self.all_sources.insert(peer_id, src);

        if meta.should_append_to_strip(source) {
            if source == ParseSource::MyStrip
                && !meta.should_append_to_strip(ParseSource::DirectRequest)
                && let Some(m) = self.peer_meta.get_mut(&peer_id)
            {
                m.stories_correspondent = true;
            }
            let target_list = if meta.stories_hidden {
                StorySourcesList::Hidden
            } else {
                StorySourcesList::NotHidden
            };
            self.upsert_source_info(target_list, info);
            self.apply_deleted_from_sources(peer_id, target_list.other());
        } else {
            self.apply_deleted_from_sources(peer_id, StorySourcesList::NotHidden);
            self.apply_deleted_from_sources(peer_id, StorySourcesList::Hidden);
        }

        self.events.push(StoriesEvent::SourceChanged(peer_id));
        self.update_peer_stories_state(peer_id, None);
    }

    /// Removes a peer's stories source completely (equivalent to `Stories::apply(peer, nullptr)`).
    pub fn clear_peer_source(&mut self, peer_id: PeerId) {
        self.apply_deleted_from_sources(peer_id, StorySourcesList::NotHidden);
        self.apply_deleted_from_sources(peer_id, StorySourcesList::Hidden);
        self.all_sources.remove(&peer_id);
        self.events.push(StoriesEvent::SourceChanged(peer_id));
        self.update_peer_stories_state(peer_id, None);
    }

    /// Applies a TL `Update` if it is a story update (`updateStory`, `updateReadStories`,
    /// `updateStoriesStealthMode`, `updateStoryID`).
    pub fn apply_tl_update(&mut self, update: &tl_enums::Update, now: TimeId) -> bool {
        // Pass existing stories so `min: true` `updateStory` payloads preserve local fields
        // (`Stories::apply(MTPDupdateStory)` in `data/data_stories.cpp:588-619`).
        let self_id = self.self_peer_id;
        let peer_meta = &self.peer_meta;
        let stories = &self.stories;
        let Some(parsed) = story_update_from_tl_with_existing(
            update,
            now,
            |pid| {
                peer_meta
                    .get(&pid)
                    .map(|m| m.ctx)
                    .unwrap_or_else(|| default_ctx(pid, self_id))
            },
            |pid, sid| stories.get(&pid).and_then(|m| m.get(&sid)),
        ) else {
            return false;
        };
        match parsed {
            ParsedStoryUpdate::Story { peer_id, item } => {
                self.apply_story_update(peer_id, item, now)
            }
            ParsedStoryUpdate::ReadStories { peer_id, max_id } => {
                self.apply_read_stories(peer_id, max_id);
            }
            ParsedStoryUpdate::StealthMode(mode) => self.apply_stealth_mode(mode),
            ParsedStoryUpdate::StoryId { .. } => {}
        }
        true
    }

    /// Applies an `updateStory` payload (`Stories::apply(MTPDupdateStory)`).
    pub fn apply_story_update(&mut self, peer_id: PeerId, item: ParsedStoryItem, now: TimeId) {
        let Some(id_dates) = self.ingest_parsed_item(peer_id, item, now) else {
            return;
        };
        if id_dates.expires <= now {
            self.apply_expired(FullStoryId::new(peer_id, id_dates.id));
            return;
        }
        let meta = self.peer_meta(peer_id);
        let Some(existing) = self.all_sources.get_mut(&peer_id) else {
            self.request_peer_stories(peer_id);
            return;
        };
        if existing.ids.contains(&id_dates) {
            return;
        }
        let was_info = existing.info();
        existing.ids.insert(id_dates);
        if id_dates.video_stream {
            existing.has_video_stream = true;
        }
        if meta.ctx.is_self && existing.read_till < id_dates.id {
            existing.read_till = id_dates.id;
            self.read_till.insert(peer_id, id_dates.id);
        }
        let now_info = existing.info();
        if was_info == now_info {
            return;
        }
        let list = if meta.stories_hidden {
            StorySourcesList::Hidden
        } else {
            StorySourcesList::NotHidden
        };
        if let Some(slot) = self.lists[list.index()]
            .sources
            .iter_mut()
            .find(|s| s.id == peer_id)
        {
            *slot = now_info;
            self.sort_sources(list);
        }
        self.events.push(StoriesEvent::SourceChanged(peer_id));
        self.update_peer_stories_state(peer_id, None);
    }

    /// Applies an `updateReadStories` update (`Stories::apply(MTPDupdateReadStories)`).
    pub fn apply_read_stories(&mut self, peer_id: PeerId, max_id: StoryId) {
        self.bump_read_till(peer_id, max_id);
    }

    /// Applies a `StealthMode` update (`Stories::apply(MTPStoriesStealthMode)`).
    pub fn apply_stealth_mode(&mut self, mode: StealthMode) {
        if self.stealth_mode != mode {
            self.stealth_mode = mode;
            self.events.push(StoriesEvent::StealthModeChanged(mode));
        }
    }

    /// Inserts or updates a single `Story` directly (convenience helper for tests and local updates).
    pub fn insert_story(&mut self, story: Story, now: TimeId) -> StoryIdDates {
        let peer_id = story.peer_id;
        let id_dates = story.id_dates();
        self.ingest_full_story(peer_id, story, now);
        id_dates
    }

    /// Applies a single TL `StoryItem` (`Stories::applySingle`).
    pub fn apply_single_tl(
        &mut self,
        peer_id: PeerId,
        item: &tl_enums::StoryItem,
        now: TimeId,
    ) -> Option<&Story> {
        let ctx = self.peer_meta(peer_id).ctx;
        let existing = match item {
            tl_enums::StoryItem::StoryItem(d) => self.lookup(FullStoryId::new(peer_id, d.id)).ok(),
            _ => None,
        };
        let parsed = story_item_from_tl(peer_id, ctx, item, now, existing)?;
        let id_dates = self.ingest_parsed_item(peer_id, parsed, now)?;
        self.lookup(FullStoryId::new(peer_id, id_dates.id)).ok()
    }

    fn ingest_parsed_item(
        &mut self,
        peer_id: PeerId,
        item: ParsedStoryItem,
        now: TimeId,
    ) -> Option<StoryIdDates> {
        match item {
            ParsedStoryItem::Full(story) => self.ingest_full_story(peer_id, *story, now),
            ParsedStoryItem::Skipped(id_dates) => {
                let full_id = FullStoryId::new(peer_id, id_dates.id);
                let expired = id_dates.expires <= now;
                if !expired {
                    self.register_expiring(id_dates.expires, full_id);
                } else if !self.has_archive(peer_id) {
                    self.apply_deleted(peer_id, id_dates.id);
                    return None;
                } else {
                    self.remove_expiring(id_dates.expires, full_id);
                    self.apply_expired(full_id);
                }
                Some(id_dates)
            }
            ParsedStoryItem::Deleted(id) => {
                self.apply_deleted(peer_id, id);
                None
            }
        }
    }

    fn ingest_full_story(
        &mut self,
        peer_id: PeerId,
        story: Story,
        now: TimeId,
    ) -> Option<StoryIdDates> {
        let id = story.id;
        let expires = story.expires;
        let expired = expires <= now;
        if expired && !story.in_profile && !self.has_archive(peer_id) {
            return None;
        }
        let full_id = FullStoryId::new(peer_id, id);
        let id_dates = story.id_dates();
        let new_albums = story.album_ids.clone();

        if !new_albums.is_empty() {
            let peer_albums = self.albums.entry(peer_id).or_default();
            for &album_id in &new_albums {
                peer_albums
                    .sets
                    .entry(album_id)
                    .or_default()
                    .album_known_in_archive
                    .insert(id);
            }
        }

        if let Some(existing) = self.stories.get_mut(&peer_id).and_then(|m| m.get_mut(&id)) {
            let prev_albums = existing.album_ids.clone();
            let (flags, media_changed, in_profile_changed) = existing.apply_changes(story);
            let now_in_profile = existing.in_profile;
            if prev_albums != new_albums {
                if let Some(peer_albums) = self.albums.get_mut(&peer_id) {
                    for &was_id in &prev_albums {
                        if !new_albums.contains(&was_id)
                            && let Some(set) = peer_albums.sets.get_mut(&was_id)
                        {
                            set.album_known_in_archive.remove(&id);
                        }
                    }
                }
                if let Some(existing) = self.stories.get_mut(&peer_id).and_then(|m| m.get_mut(&id))
                {
                    existing.album_ids = new_albums;
                }
            }
            if !flags.is_empty() {
                self.events
                    .push(StoriesEvent::StoryUpdated { id: full_id, flags });
            }
            if in_profile_changed {
                self.saved_state_changed(peer_id, id, now_in_profile);
            }
            if let Some(&settings) = self.polling_settings.get(&full_id) {
                self.maybe_schedule_polling(full_id, now, settings, now);
            }
            if media_changed {
                self.preloaded.remove(&full_id);
                if self.current_preload == Some(full_id) {
                    self.current_preload = None;
                    self.actions
                        .push(StoriesAction::CancelPreload { story_id: full_id });
                    self.rebuild_preload_sources(StorySourcesList::NotHidden);
                    self.rebuild_preload_sources(StorySourcesList::Hidden);
                    self.continue_preloading();
                }
            }
            return Some(id_dates);
        }

        self.deleted.remove(&full_id);
        let in_profile = story.in_profile;
        self.stories.entry(peer_id).or_default().insert(id, story);

        if in_profile {
            self.saved_state_changed(peer_id, id, true);
        }

        if self.has_archive(peer_id)
            && let Some(archive) = self.archive.get_mut(&peer_id)
            && insert_sorted_desc(&mut archive.ids.list, id)
        {
            if archive.total >= 0 && id > archive.last_id {
                archive.total += 1;
            }
            self.events
                .push(StoriesEvent::AlbumIdsChanged(StoryAlbumIdsKey::new(
                    peer_id,
                    STORIES_ALBUM_ID_ARCHIVE,
                )));
        }

        if expired {
            self.remove_expiring(expires, full_id);
            self.apply_expired(full_id);
        } else {
            self.register_expiring(expires, full_id);
        }

        Some(id_dates)
    }

    // -----------------------------------------------------------------------
    // Resolution (`stories.getStoriesByID`) & `loadAround`
    // -----------------------------------------------------------------------

    /// Queues resolution of `id` via `stories.getStoriesByID` (`Stories::resolve`).
    pub fn resolve(&mut self, id: FullStoryId, force: bool) {
        if !force {
            match self.lookup(id) {
                Ok(_) | Err(NoStory::Deleted) => return,
                Err(NoStory::Unknown) => {}
            }
        }
        if self
            .resolve_in_flight
            .get(&id.peer)
            .is_some_and(|set| set.contains(&id.story))
        {
            return;
        }
        self.resolve_pending
            .entry(id.peer)
            .or_default()
            .insert(id.story);
    }

    /// Flushes up to `MAX_RESOLVE_TOGETHER` (100) pending resolve IDs into `GetStoriesById` actions
    /// (`Stories::sendResolveRequests`).
    pub fn flush_resolve_requests(&mut self) {
        if !self.resolve_in_flight.is_empty() || self.resolve_pending.is_empty() {
            return;
        }
        let mut left = MAX_RESOLVE_TOGETHER;
        let peers: Vec<PeerId> = self.resolve_pending.keys().copied().collect();
        for peer_id in peers {
            if left == 0 {
                break;
            }
            let Some(pending) = self.resolve_pending.get_mut(&peer_id) else {
                continue;
            };
            let take_ids: Vec<StoryId> = pending.iter().copied().take(left).collect();
            for &sid in &take_ids {
                pending.remove(&sid);
            }
            if pending.is_empty() {
                self.resolve_pending.remove(&peer_id);
            }
            if !take_ids.is_empty() {
                left -= take_ids.len();
                self.resolve_in_flight
                    .entry(peer_id)
                    .or_default()
                    .extend(take_ids.iter().copied());
                self.actions.push(StoriesAction::GetStoriesById {
                    peer_id,
                    ids: take_ids,
                });
            }
        }
    }

    /// Applies a TL `stories.Stories` response from `stories.getStoriesByID` for `peer_id`
    /// while preserving existing story fields on `min: true` items.
    pub fn apply_tl_resolved_stories(
        &mut self,
        peer_id: PeerId,
        response: &tl_enums::stories::Stories,
        now: TimeId,
    ) {
        let ctx = self.peer_meta(peer_id).ctx;
        let stories = &self.stories;
        let page = stories_page_from_tl_with_existing(peer_id, ctx, response, now, |pid, sid| {
            stories.get(&pid).and_then(|m| m.get(&sid))
        });
        self.apply_resolved_stories(peer_id, page.items, now);
    }

    /// Marks an in-flight `stories.getStoriesByID` request for `peer_id` as failed
    /// (`Stories::sendResolveRequests` `.fail` in `data/data_stories.cpp:868-893`).
    ///
    /// Clears `resolve_in_flight` for `peer_id`, marks unresolved requested stories as
    /// deleted, emits `ItemsChanged`, and flushes any remaining pending resolve batches.
    pub fn fail_resolve_stories(&mut self, peer_id: PeerId) {
        if let Some(sent) = self.resolve_in_flight.remove(&peer_id) {
            for story_id in sent {
                let full_id = FullStoryId::new(peer_id, story_id);
                if matches!(self.lookup(full_id), Err(NoStory::Unknown)) {
                    self.apply_deleted(peer_id, story_id);
                }
            }
        }
        self.events.push(StoriesEvent::ItemsChanged(peer_id));
        if self.resolve_in_flight.is_empty() && !self.resolve_pending.is_empty() {
            self.flush_resolve_requests();
        }
    }

    /// Applies the result of `stories.getStoriesByID` for `peer_id`
    /// (`Stories::processResolvedStories` + `finalizeResolve`).
    pub fn apply_resolved_stories(
        &mut self,
        peer_id: PeerId,
        items: Vec<ParsedStoryItem>,
        now: TimeId,
    ) {
        for item in items {
            match item {
                ParsedStoryItem::Full(story) => {
                    let sid = story.id;
                    if self.ingest_full_story(peer_id, *story, now).is_none() {
                        self.apply_deleted(peer_id, sid);
                    }
                }
                ParsedStoryItem::Deleted(id) => self.apply_deleted(peer_id, id),
                ParsedStoryItem::Skipped(_) => {}
            }
        }
        if let Some(sent) = self.resolve_in_flight.remove(&peer_id) {
            for story_id in sent {
                let full_id = FullStoryId::new(peer_id, story_id);
                if matches!(self.lookup(full_id), Err(NoStory::Unknown)) {
                    self.apply_deleted(peer_id, story_id);
                }
            }
        }
        self.events.push(StoriesEvent::ItemsChanged(peer_id));
        if self.resolve_in_flight.is_empty() && !self.resolve_pending.is_empty() {
            self.flush_resolve_requests();
        }
    }

    /// Resolves stories around `id` in `context` (`Stories::loadAround`).
    ///
    /// Skips if all stories within `+/- IGNORE_PRELOAD_AROUND_IF_LOADED` (15) are already known;
    /// otherwise queues up to `+/- PRELOAD_AROUND_COUNT` (30) stories and flushes resolve requests.
    pub fn load_around(&mut self, id: FullStoryId, context: StoriesContext) {
        if matches!(context, StoriesContext::Single | StoriesContext::Album(_)) {
            return;
        }
        let ids_vec: Vec<StoryId> = match self.all_sources.get(&id.peer) {
            Some(src) => src.ids.iter().map(|s| s.id).collect(),
            None => return,
        };
        let Some(pos) = ids_vec.iter().position(|&sid| sid == id.story) else {
            return;
        };

        let check_from = pos.saturating_sub(IGNORE_PRELOAD_AROUND_IF_LOADED);
        let check_till = (pos + IGNORE_PRELOAD_AROUND_IF_LOADED).min(ids_vec.len());
        let all_known = ids_vec[check_from..check_till].iter().all(|&sid| {
            !matches!(
                self.lookup(FullStoryId::new(id.peer, sid)),
                Err(NoStory::Unknown)
            )
        });
        if all_known {
            return;
        }

        let from = pos.saturating_sub(PRELOAD_AROUND_COUNT);
        let till = (pos + PRELOAD_AROUND_COUNT).min(ids_vec.len());
        for &sid in &ids_vec[from..till] {
            self.resolve(FullStoryId::new(id.peer, sid), false);
        }
        self.flush_resolve_requests();
    }

    // -----------------------------------------------------------------------
    // Read State & Batching Timers (`readStories`, `incrementViews`, `getPeerMaxIDs`)
    // -----------------------------------------------------------------------

    /// Marks `id` as read (`Stories::markAsRead` in `data_stories.cpp:1261-1285`).
    ///
    /// - For self stories, bumps `viewed_self_till`.
    /// - For expired in-profile stories, queues `stories.incrementStoryViews` with a 5 s batch timer.
    /// - For active peer stories, bumps local `read_till`; if `id.peer` was not already pending,
    ///   flushes other pending peers immediately, then starts/resets the 3 s debounce timer.
    pub fn mark_as_read(&mut self, id: FullStoryId, now_secs: TimeId, now_ms: i64) {
        if id.peer == self.self_peer_id {
            self.set_viewed_self_till(id.story);
            return;
        }
        let (is_expired, in_profile) = match self.lookup(id) {
            Ok(story) => (story.is_expired(now_secs), story.in_profile),
            Err(_) => return,
        };
        if is_expired && in_profile {
            self.increment_views_pending
                .entry(id.peer)
                .or_default()
                .insert(id.story);
            if self.increment_views_deadline_ms.is_none() {
                self.increment_views_deadline_ms = Some(now_ms + INCREMENT_VIEWS_DELAY_MS);
            }
        }
        if !self.bump_read_till(id.peer, id.story) {
            return;
        }
        if !self.mark_read_pending.contains(&id.peer) {
            self.send_mark_as_read_requests();
        }
        self.mark_read_pending.insert(id.peer);
        self.mark_read_deadline_ms = Some(now_ms + MARK_AS_READ_DELAY_MS);
    }

    /// Monotonically advances `read_till` for `peer_id`, updating story events and strip sort order
    /// (`Stories::bumpReadTill`).
    pub fn bump_read_till(&mut self, peer_id: PeerId, max_read_till: StoryId) -> bool {
        let prev = self.read_till.get(&peer_id).copied().unwrap_or(0);
        if prev < max_read_till {
            self.read_till.insert(peer_id, max_read_till);
            self.update_peer_stories_state(peer_id, None);
            if let Some(map) = self.stories.get(&peer_id) {
                let start = prev.saturating_add(1);
                for (&sid, _) in map.range(start..=max_read_till) {
                    self.events.push(StoriesEvent::StoryUpdated {
                        id: FullStoryId::new(peer_id, sid),
                        flags: StoryUpdateFlags::MARK_READ,
                    });
                }
            }
        }
        let Some(src) = self.all_sources.get_mut(&peer_id) else {
            return false;
        };
        if src.read_till >= max_read_till {
            return false;
        }
        let was_unread = src.unread_count();
        src.read_till = max_read_till;
        let now_unread = src.unread_count();
        if was_unread != now_unread {
            let clamped = (now_unread as u32).min(MAX_SEGMENTS_COUNT);
            for list in [StorySourcesList::NotHidden, StorySourcesList::Hidden] {
                if let Some(slot) = self.lists[list.index()]
                    .sources
                    .iter_mut()
                    .find(|s| s.id == peer_id)
                {
                    slot.unread_count = clamped;
                    self.sort_sources(list);
                }
            }
        }
        true
    }

    /// Flushes all pending `stories.readStories` peers that are not currently in flight
    /// (`Stories::sendMarkAsReadRequests`).
    pub fn send_mark_as_read_requests(&mut self) {
        self.mark_read_deadline_ms = None;
        let peers: Vec<PeerId> = self.mark_read_pending.iter().copied().collect();
        for peer_id in peers {
            if self.mark_read_in_flight.contains(&peer_id) {
                continue;
            }
            if let Some(src) = self.all_sources.get(&peer_id) {
                self.mark_read_in_flight.insert(peer_id);
                self.actions.push(StoriesAction::ReadStories {
                    peer_id,
                    max_id: src.read_till,
                });
            }
            self.mark_read_pending.remove(&peer_id);
        }
    }

    /// Notifies the store that an in-flight `stories.readStories` RPC completed for `peer_id`.
    pub fn finish_mark_as_read(&mut self, peer_id: PeerId) {
        self.mark_read_in_flight.remove(&peer_id);
        if self.mark_read_deadline_ms.is_none() && self.mark_read_pending.contains(&peer_id) {
            self.send_mark_as_read_requests();
        }
    }

    /// Flushes all pending `stories.incrementStoryViews` batches not currently in flight
    /// (`Stories::sendIncrementViewsRequests`).
    pub fn send_increment_views_requests(&mut self) {
        self.increment_views_deadline_ms = None;
        let peers: Vec<PeerId> = self.increment_views_pending.keys().copied().collect();
        for peer_id in peers {
            if self.increment_views_in_flight.contains(&peer_id) {
                continue;
            }
            if let Some(ids) = self.increment_views_pending.remove(&peer_id)
                && !ids.is_empty()
            {
                self.increment_views_in_flight.insert(peer_id);
                self.actions.push(StoriesAction::IncrementStoryViews {
                    peer_id,
                    ids: ids.into_iter().collect(),
                });
            }
        }
    }

    /// Notifies the store that an in-flight `stories.incrementStoryViews` RPC completed for `peer_id`.
    pub fn finish_increment_views(&mut self, peer_id: PeerId) {
        self.increment_views_in_flight.remove(&peer_id);
        if self.increment_views_deadline_ms.is_none()
            && self.increment_views_pending.contains_key(&peer_id)
        {
            self.send_increment_views_requests();
        }
    }

    /// Schedules `peer_id` for a batched `stories.getPeerMaxIDs` poll.
    pub fn schedule_peer_max_id_poll(&mut self, peer_id: PeerId, now_ms: i64) {
        if self.peer_max_ids_in_flight.contains(&peer_id) {
            return;
        }
        self.peer_max_ids_pending.insert(peer_id);
        if self.peer_max_ids_deadline_ms.is_none() {
            self.peer_max_ids_deadline_ms = Some(now_ms + PEER_MAX_IDS_POLL_DELAY_MS);
        }
    }

    /// Flushes queued peers into `GetPeerMaxIds` actions (batches of up to `MAX_PEER_MAX_IDS_BATCH`).
    pub fn flush_peer_max_ids(&mut self) {
        self.peer_max_ids_deadline_ms = None;
        while !self.peer_max_ids_pending.is_empty() {
            let batch: Vec<PeerId> = self
                .peer_max_ids_pending
                .iter()
                .copied()
                .take(MAX_PEER_MAX_IDS_BATCH)
                .collect();
            for &pid in &batch {
                self.peer_max_ids_pending.remove(&pid);
                self.peer_max_ids_in_flight.insert(pid);
            }
            if !batch.is_empty() {
                self.actions
                    .push(StoriesAction::GetPeerMaxIds { peer_ids: batch });
            }
        }
    }

    /// Applies the `Vec<RecentStory>` result of `stories.getPeerMaxIDs` paired with the queried peers.
    pub fn apply_peer_max_ids(&mut self, results: &[(PeerId, RecentStoryInfo)]) {
        for &(peer_id, recent) in results {
            self.peer_max_ids_in_flight.remove(&peer_id);
            if recent.max_id == 0 && !recent.live {
                if self.all_sources.contains_key(&peer_id) {
                    self.clear_peer_source(peer_id);
                } else {
                    self.pending_peer_recent.remove(&peer_id);
                    self.events.push(StoriesEvent::PeerStoriesStateChanged {
                        peer_id,
                        state: StoriesState::None,
                    });
                }
                continue;
            }
            let current_max = self
                .all_sources
                .get(&peer_id)
                .map_or(0, StoriesSource::max_id);
            let current_live = self
                .all_sources
                .get(&peer_id)
                .is_some_and(|s| s.has_video_stream);
            if recent.max_id > current_max || recent.live != current_live {
                self.pending_peer_recent.insert(peer_id, recent);
                self.update_peer_stories_state(peer_id, Some(recent));
                self.request_peer_stories(peer_id);
            } else {
                self.update_peer_stories_state(peer_id, Some(recent));
            }
        }
    }

    /// Flushes pending read/views batches before app shutdown (`Stories::isQuitPrevent`).
    ///
    /// Returns `true` if any `readStories` or `incrementStoryViews` requests are still in flight.
    pub fn is_quit_prevent(&mut self) -> bool {
        if !self.mark_read_pending.is_empty() {
            self.send_mark_as_read_requests();
        }
        if !self.increment_views_pending.is_empty() {
            self.send_increment_views_requests();
        }
        !self.mark_read_in_flight.is_empty() || !self.increment_views_in_flight.is_empty()
    }

    // -----------------------------------------------------------------------
    // Timers: Expiration, Batching, Story Polling, Views Polling
    // -----------------------------------------------------------------------

    /// Advances all internal timers to `(now_secs, now_ms)`, firing any due expirations,
    /// read/views/max-id batches, and polling requests.
    pub fn tick(&mut self, now_secs: TimeId, now_ms: i64) {
        if self.next_expire_check_secs.is_some_and(|t| now_secs >= t) {
            self.process_expired(now_secs);
        }
        if self.mark_read_deadline_ms.is_some_and(|d| now_ms >= d) {
            self.send_mark_as_read_requests();
        }
        if self
            .increment_views_deadline_ms
            .is_some_and(|d| now_ms >= d)
        {
            self.send_increment_views_requests();
        }
        if self.peer_max_ids_deadline_ms.is_some_and(|d| now_ms >= d) {
            self.flush_peer_max_ids();
        }
        if self.polling_deadline_secs.is_some_and(|t| now_secs >= t) {
            self.send_polling_requests(now_secs);
        }
        if self.polling_views_deadline_ms.is_some_and(|d| now_ms >= d) {
            self.send_polling_views_requests(now_ms);
        }
    }

    /// Returns the earliest upcoming timer deadline in milliseconds relative to `(now_secs, now_ms)`,
    /// or `None` if no timers are active.
    pub fn next_timer_delay_ms(&self, now_secs: TimeId, now_ms: i64) -> Option<i64> {
        let mut best: Option<i64> = None;
        let mut consider = |delay: i64| {
            let clamped = delay.max(0);
            best = Some(match best {
                Some(cur) => cur.min(clamped),
                None => clamped,
            });
        };
        if let Some(t) = self.next_expire_check_secs {
            consider(i64::from(t.saturating_sub(now_secs)) * 1_000);
        }
        if let Some(d) = self.mark_read_deadline_ms {
            consider(d - now_ms);
        }
        if let Some(d) = self.increment_views_deadline_ms {
            consider(d - now_ms);
        }
        if let Some(d) = self.peer_max_ids_deadline_ms {
            consider(d - now_ms);
        }
        if let Some(t) = self.polling_deadline_secs {
            consider(i64::from(t.saturating_sub(now_secs)) * 1_000);
        }
        if let Some(d) = self.polling_views_deadline_ms {
            consider(d - now_ms);
        }
        best
    }

    fn register_expiring(&mut self, expires: TimeId, id: FullStoryId) {
        let entry = self.expiring.entry(expires).or_default();
        if !entry.insert(id) {
            return;
        }
        self.reschedule_expire_timer();
    }

    fn remove_expiring(&mut self, expires: TimeId, id: FullStoryId) {
        if let Some(set) = self.expiring.get_mut(&expires) {
            set.remove(&id);
            if set.is_empty() {
                self.expiring.remove(&expires);
            }
        }
        self.reschedule_expire_timer();
    }

    fn reschedule_expire_timer(&mut self) {
        self.next_expire_check_secs = self.expiring.keys().next().copied();
    }

    /// Processes all stories whose expiration timestamp `<= now` (`Stories::processExpired`).
    pub fn process_expired(&mut self, now: TimeId) {
        let due_keys: Vec<TimeId> = self.expiring.range(..=now).map(|(&ts, _)| ts).collect();
        let mut expired_ids = BTreeSet::new();
        for ts in due_keys {
            if let Some(set) = self.expiring.remove(&ts) {
                expired_ids.extend(set);
            }
        }
        for id in expired_ids {
            self.apply_expired(id);
        }
        self.reschedule_expire_timer();
    }

    /// Handles a story reaching its expiry timestamp (`Stories::applyExpired`).
    pub fn apply_expired(&mut self, id: FullStoryId) {
        if let Ok(story) = self.lookup(id) {
            let keep = self.has_archive(id.peer) || story.in_profile;
            if !keep {
                self.apply_deleted(id.peer, id.story);
                return;
            }
        }
        self.apply_removed_from_active(id);
    }

    /// Deletes a story from active sources, story map, albums, and preload queue (`Stories::applyDeleted`).
    pub fn apply_deleted(&mut self, peer_id: PeerId, story_id: StoryId) {
        let full_id = FullStoryId::new(peer_id, story_id);
        self.apply_removed_from_active(full_id);
        self.deleted.insert(full_id);

        let removed_story = self
            .stories
            .get_mut(&peer_id)
            .and_then(|map| map.remove(&story_id));
        if self.stories.get(&peer_id).is_some_and(BTreeMap::is_empty) {
            self.stories.remove(&peer_id);
        }

        if let Some(story) = removed_story {
            self.remove_expiring(story.expires, full_id);
            self.events.push(StoriesEvent::StoryUpdated {
                id: full_id,
                flags: StoryUpdateFlags::DESTROYED,
            });
            if self.has_archive(peer_id) {
                self.remove_from_album_set(peer_id, STORIES_ALBUM_ID_ARCHIVE, story_id);
            }
            if story.in_profile {
                self.remove_from_album_set(peer_id, STORIES_ALBUM_ID_SAVED, story_id);
            }
            for &album_id in &story.album_ids {
                self.remove_from_album_set(peer_id, album_id, story_id);
            }
            if self.current_preload == Some(full_id) {
                self.current_preload = None;
                self.actions
                    .push(StoriesAction::CancelPreload { story_id: full_id });
                self.finish_preload(full_id, false);
            }
        }
    }

    fn apply_removed_from_active(&mut self, id: FullStoryId) {
        let became_empty = if let Some(src) = self.all_sources.get_mut(&id.peer) {
            let matching: Option<StoryIdDates> = src.ids.iter().find(|s| s.id == id.story).copied();
            if let Some(entry) = matching {
                src.ids.remove(&entry);
                src.has_video_stream = src.ids.iter().any(|s| s.video_stream);
                Some(src.ids.is_empty())
            } else {
                None
            }
        } else {
            None
        };

        let Some(empty) = became_empty else {
            return;
        };

        if empty {
            self.all_sources.remove(&id.peer);
            self.apply_deleted_from_sources(id.peer, StorySourcesList::NotHidden);
            self.apply_deleted_from_sources(id.peer, StorySourcesList::Hidden);
        } else if let Some(info) = self.all_sources.get(&id.peer).map(StoriesSource::info) {
            for list in [StorySourcesList::NotHidden, StorySourcesList::Hidden] {
                if let Some(slot) = self.lists[list.index()]
                    .sources
                    .iter_mut()
                    .find(|s| s.id == id.peer)
                {
                    *slot = info;
                    self.sort_sources(list);
                }
            }
        }
        self.events.push(StoriesEvent::SourceChanged(id.peer));
        self.update_peer_stories_state(id.peer, None);
    }

    // -----------------------------------------------------------------------
    // Story & Views Polling
    // -----------------------------------------------------------------------

    /// Registers polling for `id` in chat or viewer mode (`Stories::registerPolling`).
    pub fn register_polling(
        &mut self,
        id: FullStoryId,
        kind: PollingKind,
        now_secs: TimeId,
        now_ms: i64,
    ) -> bool {
        let Some(last_update) = self.lookup(id).ok().map(|s| s.last_update_time) else {
            return false;
        };
        let meta = self.peer_meta(id.peer);
        let settings = self.polling_settings.entry(id).or_default();
        match kind {
            PollingKind::Chat => settings.chat += 1,
            PollingKind::Viewer => {
                settings.viewer += 1;
                if (meta.ctx.is_self || meta.ctx.is_channel) && self.polling_views.insert(id) {
                    self.send_polling_views_requests(now_ms);
                }
            }
        }
        let settings_copy = *self.polling_settings.get(&id).unwrap();
        self.maybe_schedule_polling(id, last_update, settings_copy, now_secs);
        true
    }

    /// Unregisters polling for `id` (`Stories::unregisterPolling`).
    pub fn unregister_polling(&mut self, id: FullStoryId, kind: PollingKind) {
        let Some(settings) = self.polling_settings.get_mut(&id) else {
            return;
        };
        match kind {
            PollingKind::Chat => settings.chat = settings.chat.saturating_sub(1),
            PollingKind::Viewer => {
                settings.viewer = settings.viewer.saturating_sub(1);
                if settings.viewer == 0 {
                    self.polling_views.remove(&id);
                    if self.polling_views.is_empty() {
                        self.polling_views_deadline_ms = None;
                    }
                }
            }
        }
        if settings.chat == 0 && settings.viewer == 0 {
            self.polling_settings.remove(&id);
        }
    }

    fn maybe_schedule_polling(
        &mut self,
        _id: FullStoryId,
        last_update: TimeId,
        settings: PollingSettings,
        now: TimeId,
    ) {
        let next = last_update
            .saturating_add(settings.interval_secs())
            .max(now);
        match self.polling_deadline_secs {
            Some(cur) if cur <= next => {}
            _ => self.polling_deadline_secs = Some(next),
        }
    }

    fn send_polling_requests(&mut self, now: TimeId) {
        self.polling_deadline_secs = None;
        let mut next_earliest: Option<TimeId> = None;
        let entries: Vec<(FullStoryId, PollingSettings)> = self
            .polling_settings
            .iter()
            .map(|(&id, &s)| (id, s))
            .collect();
        let mut to_resolve = Vec::new();
        for (id, settings) in entries {
            let Some(last) = self.lookup(id).ok().map(|s| s.last_update_time) else {
                continue;
            };
            let next = last.saturating_add(settings.interval_secs());
            if now >= next {
                to_resolve.push(id);
            } else {
                next_earliest = Some(match next_earliest {
                    Some(cur) => cur.min(next),
                    None => next,
                });
            }
        }
        for id in to_resolve {
            self.resolve(id, true);
        }
        if !self.resolve_pending.is_empty() {
            self.flush_resolve_requests();
        }
        self.polling_deadline_secs = next_earliest;
    }

    fn send_polling_views_requests(&mut self, now_ms: i64) {
        let Some(&id) = self.polling_views.iter().next() else {
            self.polling_views_deadline_ms = None;
            return;
        };
        if !self.views_request_in_flight {
            self.views_request_in_flight = true;
            if id.peer == self.self_peer_id {
                self.actions.push(StoriesAction::GetStoryViewsList {
                    peer_id: id.peer,
                    story_id: id.story,
                    offset: String::new(),
                    limit: POLLING_VIEWS_PER_PAGE,
                });
            } else {
                self.actions.push(StoriesAction::GetStoriesViews {
                    peer_id: id.peer,
                    ids: vec![id.story],
                });
            }
        }
        self.polling_views_deadline_ms = Some(now_ms + POLL_VIEWS_INTERVAL_MS);
    }

    /// Requests a paginated views slice (`stories.getStoryViewsList`) or channel reactions slice
    /// (`stories.getStoryReactionsList`).
    pub fn load_views_slice(&mut self, peer_id: PeerId, story_id: StoryId, offset: String) {
        self.views_request_in_flight = true;
        if peer_id == self.self_peer_id {
            self.actions.push(StoriesAction::GetStoryViewsList {
                peer_id,
                story_id,
                offset,
                limit: VIEWS_PER_PAGE,
            });
        } else if self.peer_meta(peer_id).ctx.is_channel {
            self.actions.push(StoriesAction::GetStoryReactionsList {
                peer_id,
                story_id,
                offset,
                limit: VIEWS_PER_PAGE,
            });
        } else {
            self.actions.push(StoriesAction::GetStoriesViews {
                peer_id,
                ids: vec![story_id],
            });
        }
    }

    /// Applies a `StoryViews` slice returned by `stories.getStoryViewsList`.
    pub fn apply_views_slice(&mut self, id: FullStoryId, offset: &str, slice: StoryViews) {
        self.views_request_in_flight = false;
        if let Some(story) = self.lookup_mut(id)
            && story.apply_views_slice(offset, slice)
        {
            self.events.push(StoriesEvent::StoryUpdated {
                id,
                flags: StoryUpdateFlags::VIEWS_CHANGED,
            });
        }
    }

    /// Applies a `StoryViews` slice returned by `stories.getStoryReactionsList`.
    pub fn apply_channel_reactions_slice(
        &mut self,
        id: FullStoryId,
        offset: &str,
        slice: StoryViews,
    ) {
        self.views_request_in_flight = false;
        if let Some(story) = self.lookup_mut(id)
            && story.apply_channel_reactions_slice(offset, slice)
        {
            self.events.push(StoriesEvent::StoryUpdated {
                id,
                flags: StoryUpdateFlags::VIEWS_CHANGED,
            });
        }
    }

    /// Applies updated view counts returned by `stories.getStoriesViews`.
    pub fn apply_views_counts(&mut self, id: FullStoryId, counts: StoryViewsCounts) {
        self.views_request_in_flight = false;
        if let Some(story) = self.lookup_mut(id) {
            let flags = story.apply_views_counts(counts);
            if !flags.is_empty() {
                self.events.push(StoriesEvent::StoryUpdated { id, flags });
            }
        }
    }

    /// Marks the active `getStoryViewsList` / `getStoryReactionsList` / `getStoriesViews`
    /// request as failed so subsequent view polling or pagination is not wedged
    /// (`data/data_stories.cpp:1634-1639, 1712-1717, 1743-1745`).
    pub fn fail_views_request(&mut self) {
        self.views_request_in_flight = false;
    }

    // -----------------------------------------------------------------------
    // Hidden / Archive Toggle, Reactions, Stealth, Actions
    // -----------------------------------------------------------------------

    /// Hides or unhides a peer's stories (`Stories::toggleHidden` in `data_stories.cpp:1356-1438`).
    pub fn toggle_hidden(&mut self, peer_id: PeerId, hidden: bool) {
        let meta = self.peer_meta(peer_id);
        let by_hints = meta.is_by_hints();
        let just_remove = (by_hints || meta.ctx.is_service_user) && hidden;
        if meta.stories_hidden != hidden {
            if let Some(m) = self.peer_meta.get_mut(&peer_id) {
                if by_hints && hidden {
                    m.stories_correspondent = false;
                } else if !just_remove {
                    m.stories_hidden = hidden;
                }
            }
            self.actions
                .push(StoriesAction::TogglePeerStoriesHidden { peer_id, hidden });
        }

        if just_remove {
            self.clear_peer_source(peer_id);
            return;
        }

        let Some(src) = self.all_sources.get_mut(&peer_id) else {
            return;
        };
        src.hidden = hidden;
        let info = src.info();
        let (from_list, to_list) = if hidden {
            (StorySourcesList::NotHidden, StorySourcesList::Hidden)
        } else {
            (StorySourcesList::Hidden, StorySourcesList::NotHidden)
        };
        self.apply_deleted_from_sources(peer_id, from_list);
        self.upsert_source_info(to_list, info);
    }

    /// Sends a reaction to `id` (`Stories::sendReaction`), updating local counts optimistically.
    pub fn send_reaction(&mut self, id: FullStoryId, reaction: Option<ReactionId>) {
        let is_channel = self.peer_meta(id.peer).ctx.is_channel;
        if let Some(story) = self.lookup_mut(id) {
            if story.set_reaction(reaction.clone(), is_channel) {
                self.events.push(StoriesEvent::StoryUpdated {
                    id,
                    flags: StoryUpdateFlags::REACTION,
                });
            }
            self.actions.push(StoriesAction::SendReaction {
                story_id: id,
                reaction,
            });
        }
    }

    /// Emits `ActivateStealthMode` RPC (`Stories::activateStealthMode`).
    pub fn activate_stealth_mode(&mut self) {
        self.actions.push(StoriesAction::ActivateStealthMode);
    }

    /// Queues deletion of `ids` belonging to the same peer (`Stories::deleteList`).
    pub fn delete_list(&mut self, ids: &[FullStoryId]) {
        let Some(first) = ids.first() else {
            return;
        };
        let peer_id = first.peer;
        let story_ids: Vec<StoryId> = ids
            .iter()
            .filter(|id| id.peer == peer_id)
            .map(|id| id.story)
            .collect();
        if !story_ids.is_empty() {
            self.actions.push(StoriesAction::DeleteStories {
                peer_id,
                ids: story_ids,
            });
        }
    }

    /// Queues toggling `in_profile` (profile save / archive) (`Stories::toggleInProfileList`).
    pub fn toggle_in_profile_list(&mut self, ids: &[FullStoryId], in_profile: bool) {
        let Some(first) = ids.first() else {
            return;
        };
        let peer_id = first.peer;
        let story_ids: Vec<StoryId> = ids
            .iter()
            .filter(|id| id.peer == peer_id)
            .map(|id| id.story)
            .collect();
        if !story_ids.is_empty() {
            self.actions.push(StoriesAction::ToggleInProfile {
                peer_id,
                ids: story_ids,
                in_profile,
            });
        }
    }

    /// Applies the confirmed result of `stories.togglePinned` (`toggleInProfileList` callback).
    pub fn apply_toggle_in_profile_done(
        &mut self,
        peer_id: PeerId,
        ids: &[StoryId],
        in_profile: bool,
    ) {
        let (loaded, last_id) = {
            let saved = self.saved.entry(peer_id).or_default();
            let last = saved
                .ids
                .list
                .last()
                .copied()
                .or_else(|| (saved.last_id != 0).then_some(saved.last_id))
                .unwrap_or(StoryId::MAX);
            (saved.loaded, last)
        };
        let mut dirty = false;
        for &sid in ids {
            if let Some(story) = self.lookup_mut(FullStoryId::new(peer_id, sid)) {
                story.in_profile = in_profile;
                let saved = self.saved.entry(peer_id).or_default();
                if in_profile {
                    if !loaded && sid < last_id {
                        dirty = true;
                    } else if insert_sorted_desc(&mut saved.ids.list, sid) && saved.total >= 0 {
                        saved.total += 1;
                    }
                } else if remove_sorted_desc(&mut saved.ids.list, sid) {
                    if saved.total > 0 {
                        saved.total -= 1;
                    }
                } else if !loaded {
                    dirty = true;
                }
            } else if !loaded {
                dirty = true;
            }
        }
        if dirty {
            self.album_ids_load_more(peer_id, STORIES_ALBUM_ID_SAVED);
        } else {
            self.events
                .push(StoriesEvent::AlbumIdsChanged(StoryAlbumIdsKey::new(
                    peer_id,
                    STORIES_ALBUM_ID_SAVED,
                )));
        }
    }

    /// Checks whether pinning/unpinning `ids` respects `max_pinned_count` (`Stories::canTogglePinnedList`).
    pub fn can_toggle_pinned_list(&self, ids: &[FullStoryId], pin: bool) -> bool {
        let Some(first) = ids.first() else {
            return false;
        };
        if !pin {
            return true;
        }
        let Some(saved) = self.saved.get(&first.peer) else {
            return false;
        };
        let already = &saved.ids.pinned_to_top;
        let mut count = already.len();
        for id in ids {
            if !already.contains(&id.story) {
                count += 1;
            }
        }
        count <= self.max_pinned_count
    }

    /// Prepares and queues `stories.togglePinnedToTop` (`Stories::togglePinnedList`).
    pub fn toggle_pinned_list(&mut self, ids: &[FullStoryId], pin: bool) {
        let Some(first) = ids.first() else {
            return;
        };
        let peer_id = first.peer;
        let saved = self.saved.entry(peer_id).or_default();
        let mut new_pinned = Vec::with_capacity(self.max_pinned_count);
        if pin {
            let mut copy: Vec<FullStoryId> =
                ids.iter().copied().filter(|i| i.peer == peer_id).collect();
            copy.sort_by(|a, b| b.cmp(a));
            for id in copy {
                if !saved.ids.pinned_to_top.contains(&id.story) {
                    new_pinned.push(id.story);
                }
            }
        }
        for &sid in &saved.ids.pinned_to_top {
            if pin || !ids.contains(&FullStoryId::new(peer_id, sid)) {
                new_pinned.push(sid);
            }
        }
        self.actions.push(StoriesAction::TogglePinnedToTop {
            peer_id,
            ids: new_pinned,
        });
    }

    /// Updates pinned-to-top IDs on `peer_id`'s saved stories (`Stories::setPinnedToTop`).
    pub fn set_pinned_to_top(&mut self, peer_id: PeerId, pinned_to_top: Vec<StoryId>) {
        if !self.saved.contains_key(&peer_id) && pinned_to_top.is_empty() {
            return;
        }
        let prev = self
            .saved
            .entry(peer_id)
            .or_default()
            .ids
            .pinned_to_top
            .clone();
        if prev != pinned_to_top {
            for &old_id in &prev {
                if !pinned_to_top.contains(&old_id)
                    && let Some(s) = self.lookup_mut(FullStoryId::new(peer_id, old_id))
                {
                    s.pinned_to_top = false;
                }
            }
            for &new_id in &pinned_to_top {
                if !prev.contains(&new_id)
                    && let Some(s) = self.lookup_mut(FullStoryId::new(peer_id, new_id))
                {
                    s.pinned_to_top = true;
                }
            }
            self.saved.entry(peer_id).or_default().ids.pinned_to_top = pinned_to_top;
            self.events
                .push(StoriesEvent::AlbumIdsChanged(StoryAlbumIdsKey::new(
                    peer_id,
                    STORIES_ALBUM_ID_SAVED,
                )));
        }
    }

    // -----------------------------------------------------------------------
    // Albums (Saved = 0, Archive = -1, Custom > 0)
    // -----------------------------------------------------------------------

    /// Returns the `StoriesIds` for `(peer_id, album_id)` (`Stories::albumIds`).
    pub fn album_ids(&self, peer_id: PeerId, album_id: i32) -> StoriesIds {
        self.album_set_ref(peer_id, album_id)
            .map(|s| s.ids.clone())
            .unwrap_or_default()
    }

    /// Returns the total known count for `(peer_id, album_id)` (`Stories::albumIdsCount`).
    pub fn album_ids_count(&self, peer_id: PeerId, album_id: i32) -> usize {
        self.album_set_ref(peer_id, album_id)
            .map_or(0, |s| s.total.max(0) as usize)
    }

    /// Returns `true` if total count is known for `(peer_id, album_id)` (`Stories::albumIdsCountKnown`).
    pub fn album_ids_count_known(&self, peer_id: PeerId, album_id: i32) -> bool {
        self.album_set_ref(peer_id, album_id)
            .is_some_and(|s| s.total >= 0)
    }

    /// Returns custom albums list for `peer_id`.
    pub fn albums_list(&self, peer_id: PeerId) -> &[StoryAlbum] {
        self.albums
            .get(&peer_id)
            .map(|a| a.list.as_slice())
            .unwrap_or(&[])
    }

    /// Requests loading the custom albums list for `peer_id`.
    pub fn request_albums(&mut self, peer_id: PeerId) {
        let entry = self.albums.entry(peer_id).or_default();
        if entry.request_in_flight {
            return;
        }
        entry.request_in_flight = true;
        self.actions.push(StoriesAction::GetAlbums {
            peer_id,
            hash: entry.hash,
        });
    }

    /// Applies TL `stories.Albums` response for `peer_id`.
    pub fn apply_tl_albums(&mut self, peer_id: PeerId, response: &tl_enums::stories::Albums) {
        let entry = self.albums.entry(peer_id).or_default();
        entry.request_in_flight = false;
        if let ParsedAlbums::Albums { hash, albums } = albums_from_tl(response) {
            entry.hash = hash;
            entry.list = albums;
        }
    }

    /// Marks an in-flight `stories.getAlbums` request for `peer_id` as failed
    /// (`Stories::albumsLoad` `.fail` in `data/data_stories.cpp:1958-1962`).
    pub fn fail_albums(&mut self, peer_id: PeerId) {
        if let Some(entry) = self.albums.get_mut(&peer_id) {
            entry.request_in_flight = false;
            entry.hash = 0;
        }
    }

    /// Requests the next page of stories for `(peer_id, album_id)` (`Stories::albumIdsLoadMore`).
    pub fn album_ids_load_more(&mut self, peer_id: PeerId, album_id: i32) {
        self.album_ids_load_more_inner(peer_id, album_id, false);
    }

    /// Reloads `(peer_id, album_id)` from offset 0 (`Stories::albumIdsReload` in
    /// `data/data_stories.cpp:1821-1828`).
    pub fn album_ids_reload(&mut self, peer_id: PeerId, album_id: i32) {
        if let Some(set) = self.album_set_mut(peer_id, album_id, false) {
            set.loaded = false;
            set.last_id = 0;
        }
        self.album_ids_load_more_inner(peer_id, album_id, true);
    }

    /// Marks an in-flight album/pinned/archive page request as failed
    /// (`Stories::albumIdsLoadMore` `.fail` in `data/data_stories.cpp:1882-1891`).
    pub fn fail_album_stories(&mut self, peer_id: PeerId, album_id: i32) {
        if let Some(set) = self.album_set_mut(peer_id, album_id, false) {
            set.request_in_flight = false;
            set.loaded = true;
            set.total = set.ids.list.len() as i32;
            self.events
                .push(StoriesEvent::AlbumIdsChanged(StoryAlbumIdsKey::new(
                    peer_id, album_id,
                )));
        }
    }

    fn album_ids_load_more_inner(&mut self, peer_id: PeerId, album_id: i32, reload: bool) {
        if album_id == STORIES_ALBUM_ID_ARCHIVE && !self.has_archive(peer_id) {
            return;
        }
        let Some(set) = self.album_set_mut(peer_id, album_id, false) else {
            return;
        };
        if set.request_in_flight || (!reload && set.loaded) {
            return;
        }
        set.request_in_flight = true;
        let (offset, limit) = if album_id == STORIES_ALBUM_ID_ARCHIVE {
            let lim = if set.last_id != 0 {
                ARCHIVE_PER_PAGE
            } else {
                ARCHIVE_FIRST_PER_PAGE
            };
            (set.last_id, lim)
        } else if album_id == STORIES_ALBUM_ID_SAVED {
            let lim = if set.last_id != 0 {
                SAVED_PER_PAGE
            } else {
                SAVED_FIRST_PER_PAGE
            };
            (set.last_id, lim)
        } else {
            let off = if reload { 0 } else { set.ids.list.len() as i32 };
            let lim = if reload || set.ids.list.is_empty() {
                SAVED_FIRST_PER_PAGE
            } else {
                SAVED_PER_PAGE
            };
            (off, lim)
        };
        self.actions.push(StoriesAction::LoadAlbumStories {
            peer_id,
            album_id,
            offset,
            limit,
        });
    }

    /// Applies TL `stories.Stories` page for `(peer_id, album_id)`.
    pub fn apply_tl_album_stories(
        &mut self,
        peer_id: PeerId,
        album_id: i32,
        response: &tl_enums::stories::Stories,
        reload: bool,
        now: TimeId,
    ) {
        let ctx = self.peer_meta(peer_id).ctx;
        let stories = &self.stories;
        let page = stories_page_from_tl_with_existing(peer_id, ctx, response, now, |pid, sid| {
            stories.get(&pid).and_then(|m| m.get(&sid))
        });
        self.apply_album_stories(peer_id, album_id, page, reload, now);
    }

    /// Applies a `ParsedStoriesPage` for `(peer_id, album_id)` (`Stories::albumIdsLoadMore` callback).
    pub fn apply_album_stories(
        &mut self,
        peer_id: PeerId,
        album_id: i32,
        page: ParsedStoriesPage,
        reload: bool,
        now: TimeId,
    ) {
        if let Some(set) = self.album_set_mut(peer_id, album_id, false) {
            set.request_in_flight = false;
            if reload {
                set.ids.list.clear();
                set.ids.pinned_to_top.clear();
                set.last_id = 0;
            }
            set.total = page.count;
        } else {
            return;
        }

        let ordered = album_id == STORIES_ALBUM_ID_SAVED || album_id == STORIES_ALBUM_ID_ARCHIVE;
        let empty_response = page.items.is_empty();
        for item in page.items {
            let sid = item.story_id();
            if let Some(set) = self.album_set_mut(peer_id, album_id, false) {
                if ordered {
                    insert_sorted_desc(&mut set.ids.list, sid);
                } else {
                    set.ids.list.push(sid);
                }
                set.last_id = sid;
            }
            if self.ingest_parsed_item(peer_id, item, now).is_none()
                && let Some(set) = self.album_set_mut(peer_id, album_id, false)
            {
                if ordered {
                    remove_sorted_desc(&mut set.ids.list, sid);
                } else {
                    set.ids.list.pop();
                }
                if set.total > 0 {
                    set.total -= 1;
                }
            }
        }
        if let Some(set) = self.album_set_mut(peer_id, album_id, false) {
            let count = set.ids.list.len() as i32;
            set.loaded = empty_response;
            set.total = if set.loaded {
                count
            } else {
                set.total.max(count)
            };
        }
        if album_id == STORIES_ALBUM_ID_SAVED {
            self.set_pinned_to_top(peer_id, page.pinned_to_top);
        }
        self.events
            .push(StoriesEvent::AlbumIdsChanged(StoryAlbumIdsKey::new(
                peer_id, album_id,
            )));
    }

    /// Reorders a story within a custom album (`Stories::albumReorderStories`).
    pub fn album_reorder_stories(
        &mut self,
        peer_id: PeerId,
        album_id: i32,
        old_pos: usize,
        new_pos: usize,
    ) -> bool {
        let ids = self.album_ids(peer_id, album_id);
        let mut reordered = respecting_pinned(&ids);
        if !reorder_story_ids(&mut reordered, old_pos, new_pos) {
            return false;
        }
        if let Some(set) = self.album_set_mut(peer_id, album_id, false) {
            set.ids.list = reordered.clone();
        }
        self.actions.push(StoriesAction::ReorderAlbumStories {
            peer_id,
            album_id,
            order: reordered,
        });
        self.events
            .push(StoriesEvent::AlbumIdsChanged(StoryAlbumIdsKey::new(
                peer_id, album_id,
            )));
        true
    }

    fn saved_state_changed(&mut self, peer_id: PeerId, id: StoryId, in_profile: bool) {
        if in_profile {
            let saved = self.saved.entry(peer_id).or_default();
            if insert_sorted_desc(&mut saved.ids.list, id) {
                if saved.total >= 0 && id > saved.last_id {
                    saved.total += 1;
                }
                self.events
                    .push(StoriesEvent::AlbumIdsChanged(StoryAlbumIdsKey::new(
                        peer_id,
                        STORIES_ALBUM_ID_SAVED,
                    )));
            }
        } else if let Some(saved) = self.saved.get_mut(&peer_id)
            && remove_sorted_desc(&mut saved.ids.list, id)
        {
            if saved.total > 0 {
                saved.total -= 1;
            }
            self.events
                .push(StoriesEvent::AlbumIdsChanged(StoryAlbumIdsKey::new(
                    peer_id,
                    STORIES_ALBUM_ID_SAVED,
                )));
        }
    }

    fn remove_from_album_set(&mut self, peer_id: PeerId, album_id: i32, story_id: StoryId) {
        let Some(set) = self.album_set_mut(peer_id, album_id, true) else {
            return;
        };
        set.album_known_in_archive.remove(&story_id);
        remove_unsorted(&mut set.ids.pinned_to_top, story_id);
        let sorted = album_id == STORIES_ALBUM_ID_SAVED || album_id == STORIES_ALBUM_ID_ARCHIVE;
        let removed = if sorted {
            remove_sorted_desc(&mut set.ids.list, story_id)
        } else {
            remove_unsorted(&mut set.ids.list, story_id)
        };
        if removed {
            if set.total > 0 {
                set.total -= 1;
            }
            self.events
                .push(StoriesEvent::AlbumIdsChanged(StoryAlbumIdsKey::new(
                    peer_id, album_id,
                )));
        }
    }

    fn album_set_ref(&self, peer_id: PeerId, album_id: i32) -> Option<&AlbumSet> {
        if album_id == STORIES_ALBUM_ID_ARCHIVE {
            self.archive.get(&peer_id)
        } else if album_id == STORIES_ALBUM_ID_SAVED {
            self.saved.get(&peer_id)
        } else {
            self.albums
                .get(&peer_id)
                .and_then(|a| a.sets.get(&album_id))
        }
    }

    fn album_set_mut(
        &mut self,
        peer_id: PeerId,
        album_id: i32,
        lazy: bool,
    ) -> Option<&mut AlbumSet> {
        if album_id == STORIES_ALBUM_ID_ARCHIVE {
            if !self.has_archive(peer_id) {
                return None;
            }
            if lazy {
                self.archive.get_mut(&peer_id)
            } else {
                Some(self.archive.entry(peer_id).or_default())
            }
        } else if album_id == STORIES_ALBUM_ID_SAVED {
            if lazy {
                self.saved.get_mut(&peer_id)
            } else {
                Some(self.saved.entry(peer_id).or_default())
            }
        } else if lazy {
            self.albums
                .get_mut(&peer_id)
                .and_then(|a| a.sets.get_mut(&album_id))
        } else {
            Some(
                self.albums
                    .entry(peer_id)
                    .or_default()
                    .sets
                    .entry(album_id)
                    .or_default(),
            )
        }
    }

    // -----------------------------------------------------------------------
    // Preloading (`incrementPreloadingMainSources`, `setPreloadingInViewer`, etc.)
    // -----------------------------------------------------------------------

    /// Increments the active consumer count requesting preloads for the main stories strip.
    pub fn increment_preloading_main_sources(&mut self) {
        self.preloading_main_counter += 1;
        if self.preloading_main_counter == 1
            && self.rebuild_preload_sources(StorySourcesList::NotHidden)
        {
            self.continue_preloading();
        }
    }

    /// Decrements the active consumer count requesting preloads for the main stories strip.
    pub fn decrement_preloading_main_sources(&mut self) {
        if self.preloading_main_counter == 0 {
            return;
        }
        self.preloading_main_counter -= 1;
        if self.preloading_main_counter == 0
            && self.rebuild_preload_sources(StorySourcesList::NotHidden)
        {
            self.continue_preloading();
        }
    }

    /// Increments the active consumer count requesting preloads for the hidden stories strip.
    pub fn increment_preloading_hidden_sources(&mut self) {
        self.preloading_hidden_counter += 1;
        if self.preloading_hidden_counter == 1
            && self.rebuild_preload_sources(StorySourcesList::Hidden)
        {
            self.continue_preloading();
        }
    }

    /// Decrements the active consumer count requesting preloads for the hidden stories strip.
    pub fn decrement_preloading_hidden_sources(&mut self) {
        if self.preloading_hidden_counter == 0 {
            return;
        }
        self.preloading_hidden_counter -= 1;
        if self.preloading_hidden_counter == 0
            && self.rebuild_preload_sources(StorySourcesList::Hidden)
        {
            self.continue_preloading();
        }
    }

    /// Sets the priority list of story IDs to preload for the active story viewer
    /// (`Stories::setPreloadingInViewer`).
    pub fn set_preloading_in_viewer(&mut self, mut ids: Vec<FullStoryId>) {
        ids.retain(|id| !self.preloaded.contains(id));
        if self.to_preload_viewer != ids {
            self.to_preload_viewer = ids;
            self.continue_preloading();
        }
    }

    /// Returns the currently active preload target, if any.
    #[inline]
    pub const fn current_preload(&self) -> Option<FullStoryId> {
        self.current_preload
    }

    /// Returns `true` if `id` has already been preloaded.
    #[inline]
    pub fn is_preloaded(&self, id: FullStoryId) -> bool {
        self.preloaded.contains(&id)
    }

    /// Returns the next story ID that should be preloaded (`Stories::nextPreloadId`).
    ///
    /// Priority: viewer preload queue > hidden strip sources > main strip sources.
    pub fn next_preload_id(&self) -> Option<FullStoryId> {
        self.to_preload_viewer
            .first()
            .or_else(|| {
                self.lists[StorySourcesList::Hidden.index()]
                    .to_preload
                    .first()
            })
            .or_else(|| {
                self.lists[StorySourcesList::NotHidden.index()]
                    .to_preload
                    .first()
            })
            .copied()
    }

    /// Returns `true` if an in-flight preload of `id` should continue (`Stories::shouldContinuePreload`).
    pub fn should_continue_preload(&self, id: FullStoryId) -> bool {
        self.to_preload_viewer
            .iter()
            .chain(
                self.lists[StorySourcesList::Hidden.index()]
                    .to_preload
                    .iter(),
            )
            .chain(
                self.lists[StorySourcesList::NotHidden.index()]
                    .to_preload
                    .iter(),
            )
            .take(STILL_PRELOAD_FROM_FIRST)
            .any(|&candidate| candidate == id)
    }

    /// Marks the active preload of `id` as finished and advances to the next queued item
    /// (`Stories::preloadFinished`).
    pub fn finish_preload(&mut self, id: FullStoryId, mark_as_preloaded: bool) {
        if self.current_preload == Some(id) {
            self.current_preload = None;
        }
        for list in &mut self.lists {
            list.to_preload.retain(|&x| x != id);
        }
        self.to_preload_viewer.retain(|&x| x != id);
        if mark_as_preloaded {
            self.preloaded.insert(id);
        }
        self.continue_preloading();
    }

    fn rebuild_preload_sources(&mut self, list: StorySourcesList) -> bool {
        let counter = match list {
            StorySourcesList::Hidden => self.preloading_hidden_counter,
            StorySourcesList::NotHidden => self.preloading_main_counter,
        };
        let idx = list.index();
        if counter == 0 {
            let had_any = !self.lists[idx].to_preload.is_empty();
            self.lists[idx].to_preload.clear();
            return had_any;
        }
        let mut next = Vec::new();
        for info in self.lists[idx].sources.iter().take(MAX_PRELOAD_SOURCES) {
            if let Some(src) = self.all_sources.get(&info.id)
                && let Some(to_open) = src.to_open()
                && to_open.is_valid()
            {
                let full_id = FullStoryId::new(info.id, to_open.id);
                if !self.preloaded.contains(&full_id) {
                    next.push(full_id);
                }
            }
        }
        if self.lists[idx].to_preload != next {
            self.lists[idx].to_preload = next;
            true
        } else {
            false
        }
    }

    fn continue_preloading(&mut self) {
        if let Some(cur) = self.current_preload {
            if self.should_continue_preload(cur) {
                return;
            }
            self.current_preload = None;
            self.actions
                .push(StoriesAction::CancelPreload { story_id: cur });
        }
        if let Some(next_id) = self.next_preload_id()
            && self.lookup(next_id).is_ok()
        {
            self.current_preload = Some(next_id);
            self.actions
                .push(StoriesAction::StartPreload { story_id: next_id });
        }
    }

    // -----------------------------------------------------------------------
    // Internal Source Sorting & State Updates
    // -----------------------------------------------------------------------

    fn upsert_source_info(&mut self, list: StorySourcesList, info: StoriesSourceInfo) {
        let sources = &mut self.lists[list.index()].sources;
        if let Some(slot) = sources.iter_mut().find(|s| s.id == info.id) {
            if *slot == info {
                return;
            }
            *slot = info;
        } else {
            sources.push(info);
        }
        self.sort_sources(list);
    }

    fn apply_deleted_from_sources(&mut self, peer_id: PeerId, list: StorySourcesList) {
        let sources = &mut self.lists[list.index()].sources;
        let before = sources.len();
        sources.retain(|s| s.id != peer_id);
        if sources.len() != before {
            self.notify_sources_changed(list);
            if self.rebuild_preload_sources(list) {
                self.continue_preloading();
            }
        }
    }

    fn sort_sources(&mut self, list: StorySourcesList) {
        let self_id = self.self_peer_id;
        self.lists[list.index()]
            .sources
            .sort_by_key(|a| std::cmp::Reverse(source_sort_key(*a, self_id)));
        self.notify_sources_changed(list);
        if self.rebuild_preload_sources(list) {
            self.continue_preloading();
        }
    }

    fn notify_sources_changed(&mut self, list: StorySourcesList) {
        self.events.push(StoriesEvent::SourcesChanged(list));
        if list == StorySourcesList::Hidden {
            let hidden = &self.lists[StorySourcesList::Hidden.index()].sources;
            let total = hidden.len();
            let unread = hidden.iter().filter(|s| s.unread_count > 0).count();
            self.events
                .push(StoriesEvent::HiddenCountsChanged { total, unread });
        }
    }

    fn update_peer_stories_state(
        &mut self,
        peer_id: PeerId,
        cached_recent: Option<RecentStoryInfo>,
    ) {
        let read_till = self.read_till.get(&peer_id).copied().unwrap_or(0);
        let recent = if let Some(src) = self.all_sources.get(&peer_id) {
            RecentStoryInfo {
                max_id: src.max_id(),
                live: src.ids.iter().next_back().is_some_and(|s| s.video_stream),
            }
        } else if let Some(r) = cached_recent {
            r
        } else {
            self.pending_peer_recent
                .get(&peer_id)
                .copied()
                .unwrap_or_default()
        };
        let state = PeerSourceState {
            max_id: recent.max_id,
            read_till,
            has_video_stream: recent.live,
        }
        .to_stories_state();
        self.events
            .push(StoriesEvent::PeerStoriesStateChanged { peer_id, state });
    }
}

fn default_ctx(peer_id: PeerId, self_peer_id: PeerId) -> StoryPeerContext {
    let is_self = peer_id == self_peer_id;
    StoryPeerContext {
        is_self,
        is_user: peer_id.is_user(),
        is_channel: peer_id.is_channel(),
        is_service_user: peer_id == PeerId::SERVICE_NOTIFICATIONS,
        can_post_stories: is_self,
        can_edit_stories: is_self,
        can_delete_stories: is_self,
        ..StoryPeerContext::default()
    }
}
