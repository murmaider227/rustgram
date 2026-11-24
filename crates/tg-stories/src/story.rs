//! Port of `data/data_story.h` and `data/data_story.cpp`.
//!
//! Defines the domain model for Telegram Stories: media (photo, video, live video
//! stream, unsupported placeholder), captions with entities and non-premium link
//! stripping, privacy rules, interactive media areas (location/venue, suggested
//! reaction, channel post, URL / Star Gift, weather), views/reactions tracking,
//! repost/forward attribution, albums, and permission checks.

use std::collections::{BTreeMap, BTreeSet};

use bitflags::bitflags;
use tg_data::peer::GeoPoint;
use tg_data::{
    CallId, ChatBannedRights, DocumentData, EntityKind, FullMsgId, FullStoryId, MsgId, Peer,
    PeerId, PhotoData, ReactionId, SendCheckContext, StoryId, TextWithEntities, TimeId,
};

/// Maximum number of recent viewer userpics retained per story (`Story::kRecentViewersMax`).
pub const RECENT_VIEWERS_MAX: usize = 3;

/// Default maximum number of stories pinned to top of a profile (`stories_pinned_to_top_count_max`).
pub const DEFAULT_MAX_PINNED_COUNT: usize = 3;

/// Default photo story display duration in milliseconds (`kPhotoDuration` = 5 s).
pub const PHOTO_STORY_DURATION_MS: i64 = 5_000;

/// Special album ID for stories saved/pinned to a peer's profile (`kStoriesAlbumIdSaved`).
pub const STORIES_ALBUM_ID_SAVED: i32 = 0;

/// Special album ID for a peer's stories archive (`kStoriesAlbumIdArchive`).
pub const STORIES_ALBUM_ID_ARCHIVE: i32 = -1;

/// Story visibility privacy category (`Data::StoryPrivacy`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum StoryPrivacy {
    /// Visible to everyone (`public`).
    Public,
    /// Visible only to close friends (`close_friends`).
    CloseFriends,
    /// Visible to contacts (`contacts`).
    Contacts,
    /// Visible to selected contacts (`selected_contacts`).
    SelectedContacts,
    /// Other / custom privacy configuration (`Other`).
    #[default]
    Other,
}

/// Compact story identifier paired with creation/expiry timestamps (`Data::StoryIdDates`).
///
/// Ordered lexicographically by `(id, date, expires, video_stream)` matching C++ `operator<=>`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct StoryIdDates {
    /// Server story ID within the peer.
    pub id: StoryId,
    /// Unix creation timestamp (seconds).
    pub date: TimeId,
    /// Unix expiration timestamp (seconds).
    pub expires: TimeId,
    /// Whether this story represents an active live video stream.
    pub video_stream: bool,
}

impl StoryIdDates {
    /// Creates a new `StoryIdDates`.
    #[inline]
    pub const fn new(id: StoryId, date: TimeId, expires: TimeId, video_stream: bool) -> Self {
        Self {
            id,
            date,
            expires,
            video_stream,
        }
    }

    /// Returns `true` if `id != 0` (`StoryIdDates::valid`).
    #[inline]
    pub const fn is_valid(self) -> bool {
        self.id != 0
    }
}

/// Live video stream reference attached to a story (`messageMediaVideoStream`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct StoryVideoStream {
    /// Owning peer ID.
    pub peer_id: PeerId,
    /// Group call ID (`inputGroupCall.id`).
    pub call_id: CallId,
    /// Group call access hash (`inputGroupCall.access_hash`).
    pub access_hash: i64,
    /// Whether this is an RTMP stream (`rtmp_stream` flag).
    pub rtmp_stream: bool,
    /// Live viewer count (if known).
    pub viewers_count: i32,
    /// Whether viewer comments are enabled in the live stream.
    pub messages_enabled: bool,
    /// Minimum Telegram Stars required per comment (`0` = free).
    pub messages_min_price: i32,
    /// Whether the live stream metadata has finished loading.
    pub loaded: bool,
}

/// Media payload attached to a `Story` (`Data::StoryMedia`).
// Few stories are alive at once; boxing `Video` would only churn the API the viewer uses.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Default, PartialEq)]
pub enum StoryMedia {
    /// Unsupported media constructor from a newer API layer (`v::null_t`).
    #[default]
    Unsupported,
    /// Static photo story (`PhotoData`).
    Photo(PhotoData),
    /// Video or silent animation document story (`DocumentData`).
    Video {
        /// Underlying video/GIF document metadata.
        document: DocumentData,
        /// Whether the video has no audio track (`DocumentAttributeVideo::nosound`).
        silent: bool,
    },
    /// Live video stream story (`messageMediaVideoStream`).
    VideoStream(StoryVideoStream),
}

impl StoryMedia {
    /// Returns `true` if this media is unsupported (`Story::unsupported`).
    #[inline]
    pub const fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported)
    }

    /// Returns the photo payload if this is a photo story.
    #[inline]
    pub fn as_photo(&self) -> Option<&PhotoData> {
        match self {
            Self::Photo(p) => Some(p),
            _ => None,
        }
    }

    /// Returns the video document payload if this is a video story.
    #[inline]
    pub fn as_video(&self) -> Option<&DocumentData> {
        match self {
            Self::Video { document, .. } => Some(document),
            _ => None,
        }
    }

    /// Returns `true` if this is a silent video story.
    #[inline]
    pub const fn is_silent_video(&self) -> bool {
        matches!(self, Self::Video { silent: true, .. })
    }

    /// Returns the live video stream metadata if this is a video stream story.
    #[inline]
    pub fn as_video_stream(&self) -> Option<&StoryVideoStream> {
        match self {
            Self::VideoStream(stream) => Some(stream),
            _ => None,
        }
    }

    /// Returns the expected duration in milliseconds for playback/progress calculation.
    ///
    /// - Photo / Unsupported: `PHOTO_STORY_DURATION_MS` (5 000 ms).
    /// - Video: document duration in milliseconds (clamped to `>= 1000` if present, else `5000`).
    /// - VideoStream: `None` (live streams do not auto-advance).
    pub fn duration_ms(&self) -> Option<i64> {
        match self {
            Self::Photo(_) | Self::Unsupported => Some(PHOTO_STORY_DURATION_MS),
            Self::Video { document, .. } => {
                let secs = document.duration.unwrap_or(5.0);
                if secs.is_finite() && secs > 0.0 {
                    Some((secs * 1000.0).round() as i64)
                } else {
                    Some(PHOTO_STORY_DURATION_MS)
                }
            }
            Self::VideoStream(_) => None,
        }
    }
}

/// Normalized rectangle in `[0.0, 1.0]` story-frame coordinates (`QRectF`).
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct RectF {
    /// Left X coordinate in `[0.0, 1.0]`.
    pub x: f64,
    /// Top Y coordinate in `[0.0, 1.0]`.
    pub y: f64,
    /// Width in `[0.0, 1.0]`.
    pub width: f64,
    /// Height in `[0.0, 1.0]`.
    pub height: f64,
}

impl RectF {
    /// Creates a new `RectF`.
    #[inline]
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Returns the center `(cx, cy)` of this rectangle.
    #[inline]
    pub fn center(self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    /// Returns `true` if `(px, py)` lies within `[x, x + width] x [y, y + height]`.
    #[inline]
    pub fn contains(self, px: f64, py: f64) -> bool {
        self.width > 0.0
            && self.height > 0.0
            && px >= self.x
            && px <= self.x + self.width
            && py >= self.y
            && py <= self.y + self.height
    }

    /// Returns `true` if width or height is non-positive.
    #[inline]
    pub fn is_empty(self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }
}

/// Normalized position, rotation, and corner radius of an interactive media area
/// (`Data::StoryArea`).
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct StoryArea {
    /// Normalized bounding box (`x, y, width, height` in `0.0..=1.0`, top-left origin).
    pub geometry: RectF,
    /// Clockwise rotation angle in degrees (`(-360, 360)`).
    pub rotation: f64,
    /// Optional corner radius percentage (`0.0..=100.0`).
    pub radius: f64,
}

impl StoryArea {
    /// Parses percentage-based center/size coordinates (`0..=100`) into a `StoryArea`,
    /// matching `Data::ParseArea` in `data_story.cpp`.
    pub fn from_tl_coords(
        x_pct: f64,
        y_pct: f64,
        w_pct: f64,
        h_pct: f64,
        rotation: f64,
        radius: Option<f64>,
    ) -> Self {
        let valid = |v: f64| v.is_finite() && (0.0..=100.0).contains(&v);
        if !valid(x_pct) || !valid(y_pct) || !valid(w_pct) || !valid(h_pct) {
            return Self::default();
        }
        let left = (x_pct - w_pct / 2.0) / 100.0;
        let top = (y_pct - h_pct / 2.0) / 100.0;
        let rot = if rotation.is_finite() {
            rotation % 360.0
        } else {
            0.0
        };
        let rad = radius.filter(|&r| valid(r)).unwrap_or(0.0);
        Self {
            geometry: RectF::new(left, top, w_pct / 100.0, h_pct / 100.0),
            rotation: rot,
            radius: rad,
        }
    }

    /// Converts back to TL percentage-based center/size coordinates (`Data::SerializeArea`).
    ///
    /// Returns `(x_pct, y_pct, w_pct, h_pct, rotation, radius_opt)`.
    pub fn to_tl_coords(self) -> (f64, f64, f64, f64, f64, Option<f64>) {
        let (cx, cy) = self.geometry.center();
        let radius = (self.radius > 0.0).then_some(self.radius);
        (
            cx * 100.0,
            cy * 100.0,
            self.geometry.width * 100.0,
            self.geometry.height * 100.0,
            self.rotation,
            radius,
        )
    }

    /// Tests whether a normalized point `(px, py)` hits this rotated area
    /// (matching `Controller::lookupAreaHandler` / `Rotated` in `media_stories_controller.cpp`).
    pub fn hit_test_normalized(&self, px: f64, py: f64) -> bool {
        if self.geometry.is_empty() {
            return false;
        }
        let (cx, cy) = self.geometry.center();
        let (rx, ry) = rotate_point(px, py, cx, cy, -self.rotation);
        self.geometry.contains(rx, ry)
    }
}

/// Rotates `(px, py)` around `(ox, oy)` by `angle_deg` degrees (`Rotated` in `media_stories_controller.cpp`).
pub fn rotate_point(px: f64, py: f64, ox: f64, oy: f64, angle_deg: f64) -> (f64, f64) {
    if angle_deg.abs() < 1.0 {
        return (px, py);
    }
    let alpha = angle_deg.to_radians();
    let (sin_a, cos_a) = alpha.sin_cos();
    let dx = px - ox;
    let dy = py - oy;
    (ox + cos_a * dx - sin_a * dy, oy + sin_a * dx + cos_a * dy)
}

/// Optional structured address attached to a geo-point media area (`GeoPointAddress`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct GeoPointAddress {
    /// Two-letter ISO 3166-1 alpha-2 country code.
    pub country_iso2: String,
    /// Optional state or province name.
    pub state: Option<String>,
    /// Optional city name.
    pub city: Option<String>,
    /// Optional street address.
    pub street: Option<String>,
}

/// Location or venue interactive area (`Data::StoryLocation`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StoryLocation {
    /// Interactive area geometry.
    pub area: StoryArea,
    /// Geographic coordinates.
    pub point: GeoPoint,
    /// Venue title (empty for plain geo-point).
    pub title: String,
    /// Venue street address (empty for plain geo-point).
    pub address: String,
    /// ISO2 country code from `GeoPointAddress` (if present).
    pub country_iso2: String,
    /// State from `GeoPointAddress` (if present).
    pub state: String,
    /// City from `GeoPointAddress` (if present).
    pub city: String,
    /// Street from `GeoPointAddress` (if present).
    pub street: String,
    /// Venue provider (e.g. `"foursquare"`, `"gplaces"`).
    pub provider: String,
    /// Provider venue identifier.
    pub venue_id: String,
    /// Provider venue category/type.
    pub venue_type: String,
}

/// Suggested reaction interactive sticker (`Data::SuggestedReaction`).
#[derive(Clone, Debug, PartialEq)]
pub struct SuggestedReaction {
    /// Interactive area geometry.
    pub area: StoryArea,
    /// Reaction emoji or custom emoji document ID.
    pub reaction: ReactionId,
    /// Aggregated count of this reaction (for channel stories).
    pub count: i32,
    /// Whether the sticker is horizontally flipped.
    pub flipped: bool,
    /// Whether the sticker uses dark styling.
    pub dark: bool,
}

/// Reposted channel post interactive area (`Data::ChannelPost`).
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelPost {
    /// Interactive area geometry.
    pub area: StoryArea,
    /// Referenced channel message ID (`FullMsgId`).
    pub item_id: FullMsgId,
}

/// Clickable URL or Star Gift interactive area (`Data::UrlArea`).
#[derive(Clone, Debug, PartialEq)]
pub struct UrlArea {
    /// Interactive area geometry.
    pub area: StoryArea,
    /// Target URL (for `mediaAreaStarGift`, formatted as `"tg://nft?slug=<slug>"`).
    pub url: String,
}

/// Weather widget interactive area (`Data::WeatherArea`).
#[derive(Clone, Debug, PartialEq)]
pub struct WeatherArea {
    /// Interactive area geometry.
    pub area: StoryArea,
    /// Weather condition emoji.
    pub emoji: String,
    /// ARGB color packed as `u32` (matching `Ui::Color32FromSerialized`).
    pub color: u32,
    /// Temperature in millicelsius (`round(clamp(temp_c, -274.0, 1_000_000.0) * 1000)`).
    pub millicelsius: i32,
}

impl WeatherArea {
    /// Converts `millicelsius` to degrees Celsius.
    #[inline]
    pub fn celsius(&self) -> f64 {
        f64::from(self.millicelsius) / 1000.0
    }

    /// Converts `millicelsius` to degrees Fahrenheit.
    #[inline]
    pub fn fahrenheit(&self) -> f64 {
        self.celsius() * 9.0 / 5.0 + 32.0
    }
}

/// Repost / forward attribution header on a story (`storyFwdHeader`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct StoryRepostInfo {
    /// Original peer ID if visible.
    pub source_peer: Option<PeerId>,
    /// Original author name if peer is hidden.
    pub source_name: String,
    /// Original story ID if visible.
    pub source_id: StoryId,
    /// Whether the reposter modified the story (`fwd_from.modified`).
    pub modified: bool,
}

impl StoryRepostInfo {
    /// Returns `true` if this story is a repost (`Story::repost`).
    #[inline]
    pub fn is_repost(&self) -> bool {
        self.source_peer.is_some() || !self.source_name.is_empty()
    }
}

/// A single viewer / reactor / public forward / public repost entry (`Data::StoryView`).
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct StoryView {
    /// Viewer or reposter peer ID.
    pub peer_id: PeerId,
    /// Optional reaction sent by this peer.
    pub reaction: Option<ReactionId>,
    /// Repost story ID if this entry is a public repost (`0` otherwise).
    pub repost_id: StoryId,
    /// Forwarded message ID if this entry is a public forward (`MsgId::ZERO` otherwise).
    pub forward_id: MsgId,
    /// View/reaction timestamp (Unix seconds).
    pub date: TimeId,
    /// Whether this viewer is blocked by us (`storyView.blocked`).
    pub blocked: bool,
    /// Whether we hid our stories from this viewer (`storyView.blocked_my_stories_from`).
    pub blocked_my_stories_from: bool,
}

/// Classification of a `StoryView` entry for display (`Ui::WhoReactedType`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum StoryViewKind {
    /// Regular view (possibly with reaction).
    Viewed,
    /// Public story repost.
    Reposted,
    /// Public message forward.
    Forwarded,
}

impl StoryView {
    /// Returns the interaction kind (`Viewed`, `Reposted`, or `Forwarded`).
    #[inline]
    pub const fn kind(&self) -> StoryViewKind {
        if !self.forward_id.is_zero() {
            StoryViewKind::Forwarded
        } else if self.repost_id != 0 {
            StoryViewKind::Reposted
        } else {
            StoryViewKind::Viewed
        }
    }
}

/// Aggregated story views/reactions counters and paginated viewer list (`Data::StoryViews`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StoryViews {
    /// Loaded viewer/reaction entries.
    pub list: Vec<StoryView>,
    /// Server pagination offset for fetching the next slice (empty when exhausted).
    pub next_offset: String,
    /// Total reactions count.
    pub reactions: i32,
    /// Total forwards/reposts count.
    pub forwards: i32,
    /// Total views count.
    pub views: i32,
    /// Total interactions count (`views + forwards` or server `count`).
    pub total: i32,
    /// Whether view counts have been populated from the server.
    pub known: bool,
}

/// Parsed view counts snapshot from `MTPDstoryViews` (`Story::ViewsCounts`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StoryViewsCounts {
    /// Total views.
    pub views: i32,
    /// Total forwards.
    pub forwards: i32,
    /// Total reactions.
    pub reactions: i32,
    /// Per-reaction counts (populated for channels).
    pub reactions_counts: BTreeMap<ReactionId, i32>,
    /// Up to `RECENT_VIEWERS_MAX` recent viewer peer IDs.
    pub recent_viewers: Vec<PeerId>,
}

/// Story album metadata (`Data::StoryAlbum`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StoryAlbum {
    /// Server album ID (`> 0` for user/channel custom albums).
    pub id: i32,
    /// Album title.
    pub title: String,
    /// Optional photo icon.
    pub icon_photo: Option<PhotoData>,
    /// Optional video icon.
    pub icon_video: Option<DocumentData>,
}

/// Story stealth mode state (`Data::StealthMode`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct StealthMode {
    /// Unix timestamp until which stealth mode is active (`0` = inactive).
    pub enabled_till: TimeId,
    /// Unix timestamp until which stealth mode cannot be re-activated (`0` = ready).
    pub cooldown_till: TimeId,
}

impl StealthMode {
    /// Returns `true` if stealth mode is active at `now`.
    #[inline]
    pub const fn is_active(self, now: TimeId) -> bool {
        self.enabled_till > now
    }

    /// Returns remaining active seconds at `now` (`0` if inactive).
    #[inline]
    pub const fn active_seconds_left(self, now: TimeId) -> i32 {
        if self.enabled_till > now {
            self.enabled_till - now
        } else {
            0
        }
    }

    /// Returns `true` if stealth mode is currently in cooldown at `now`.
    #[inline]
    pub const fn is_in_cooldown(self, now: TimeId) -> bool {
        self.cooldown_till > now
    }

    /// Returns remaining cooldown seconds at `now` (`0` if ready).
    #[inline]
    pub const fn cooldown_seconds_left(self, now: TimeId) -> i32 {
        if self.cooldown_till > now {
            self.cooldown_till - now
        } else {
            0
        }
    }
}

bitflags! {
    /// Change flags emitted when a `Story` is mutated (`Data::StoryUpdate::Flag`).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct StoryUpdateFlags: u8 {
        /// Story media, caption, edited flag, or non-reaction areas changed.
        const EDITED        = 1 << 0;
        /// View counts, viewer list, or recent viewers changed.
        const VIEWS_CHANGED = 1 << 1;
        /// Sent reaction or suggested reaction counts changed.
        const REACTION      = 1 << 2;
        /// Story was marked as read.
        const MARK_READ     = 1 << 3;
        /// Story was deleted / destroyed.
        const DESTROYED     = 1 << 4;
    }
}

/// Peer attributes needed when parsing or checking permissions on a `Story`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct StoryPeerContext {
    /// Whether the story's peer is the current logged-in user (`peer->isSelf()`).
    pub is_self: bool,
    /// Whether the story's peer is a channel (broadcast or megagroup).
    pub is_channel: bool,
    /// Whether the story's peer is a broadcast channel.
    pub is_broadcast: bool,
    /// Whether the story's peer is a megagroup (supergroup).
    pub is_megagroup: bool,
    /// Whether the story's peer is a user.
    pub is_user: bool,
    /// Whether the story's peer is verified.
    pub is_verified: bool,
    /// Whether the story's peer is a Premium user.
    pub is_premium: bool,
    /// Whether the story's peer is Telegram service notifications (`777000`).
    pub is_service_user: bool,
    /// Whether the story's peer has a non-empty public username.
    pub has_username: bool,
    /// Whether the current account can post stories to this peer.
    pub can_post_stories: bool,
    /// Whether the current account can edit stories on this peer.
    pub can_edit_stories: bool,
    /// Whether the current account can delete stories on this peer.
    pub can_delete_stories: bool,
}

impl StoryPeerContext {
    /// Derives `StoryPeerContext` from a `tg_data::Peer`.
    pub fn from_peer(peer: &Peer) -> Self {
        let is_self = peer.is_self();
        let is_user = peer.as_user().is_some();
        let is_channel = peer.as_channel().is_some();
        let is_broadcast = peer.is_broadcast();
        let is_megagroup = peer.is_megagroup();
        let is_service_user = peer.id() == PeerId::SERVICE_NOTIFICATIONS;
        let (can_post_stories, can_edit_stories, can_delete_stories) = match peer {
            Peer::User(_) => (is_self, is_self, is_self),
            Peer::Channel(ch) => (
                ch.can_post_stories(),
                ch.can_edit_stories(),
                ch.can_delete_stories(),
            ),
            Peer::Chat(_) => (false, false, false),
        };
        Self {
            is_self,
            is_channel,
            is_broadcast,
            is_megagroup,
            is_user,
            is_verified: peer.is_verified(),
            is_premium: peer.is_premium(),
            is_service_user,
            has_username: !peer.username().is_empty(),
            can_post_stories,
            can_edit_stories,
            can_delete_stories,
        }
    }

    /// Returns `true` if this peer has a stories archive (`Stories::hasArchive`).
    #[inline]
    pub const fn has_archive(self) -> bool {
        self.is_self || (self.is_channel && self.can_edit_stories)
    }
}

/// Complete domain model of a single Telegram Story (`Data::Story`).
#[derive(Clone, Debug, PartialEq)]
pub struct Story {
    /// Story ID within `peer_id`.
    pub id: StoryId,
    /// Owner peer ID.
    pub peer_id: PeerId,
    /// Story creation timestamp (Unix seconds).
    pub date: TimeId,
    /// Story expiration timestamp (Unix seconds).
    pub expires: TimeId,
    /// Last local refresh/update timestamp (Unix seconds).
    pub last_update_time: TimeId,
    /// Media payload (photo, video, live stream, or unsupported).
    pub media: StoryMedia,
    /// Story caption with entities (links stripped for non-verified non-premium users).
    pub caption: TextWithEntities,
    /// Whether this story was posted by the current account (`out`).
    pub out: bool,
    /// Whether this story is saved/pinned to the peer's profile (`pinned` flag in TL).
    pub in_profile: bool,
    /// Whether this story is pinned to the top of the profile stories grid.
    pub pinned_to_top: bool,
    /// Privacy tier.
    pub privacy: StoryPrivacy,
    /// Explicitly allowed peer IDs from `privacy` rules.
    pub privacy_allowed: Vec<PeerId>,
    /// Explicitly disallowed peer IDs from `privacy` rules.
    pub privacy_disallowed: Vec<PeerId>,
    /// Whether forwarding/saving this story is forbidden (`noforwards`).
    pub no_forwards: bool,
    /// Whether this story was edited (`edited`).
    pub edited: bool,
    /// Optional sender peer ID (e.g., author in a channel story `from_id`).
    pub from_peer: Option<PeerId>,
    /// Repost attribution metadata (`fwd_from`).
    pub repost: StoryRepostInfo,
    /// Optional background music document attached to the story (`music`).
    pub music: Option<DocumentData>,
    /// Custom album IDs this story belongs to (`albums`).
    pub album_ids: BTreeSet<i32>,
    /// Reaction sent by the current account (`sent_reaction`).
    pub sent_reaction: Option<ReactionId>,
    /// Up to `RECENT_VIEWERS_MAX` recent viewer peer IDs.
    pub recent_viewers: Vec<PeerId>,
    /// Views & viewers slice (for self stories and view counts).
    pub views: StoryViews,
    /// Reactions list slice (for channel stories).
    pub channel_reactions: StoryViews,
    /// Interactive location/venue areas.
    pub locations: Vec<StoryLocation>,
    /// Interactive suggested reaction areas.
    pub suggested_reactions: Vec<SuggestedReaction>,
    /// Interactive channel post areas.
    pub channel_posts: Vec<ChannelPost>,
    /// Interactive URL / Star Gift areas.
    pub url_areas: Vec<UrlArea>,
    /// Interactive weather widget areas.
    pub weather_areas: Vec<WeatherArea>,
}

/// Strips clickable link entities (`TextUrl`, `Url`, `Mention`, `Hashtag`) from a story
/// caption for non-verified, non-premium user peers (`StripLinks` in `data_story.cpp`).
pub fn strip_story_links(mut caption: TextWithEntities) -> TextWithEntities {
    caption.entities.retain(|e| {
        !matches!(
            e.kind,
            EntityKind::TextUrl { .. }
                | EntityKind::Url
                | EntityKind::Mention
                | EntityKind::Hashtag
        )
    });
    caption
}

impl Story {
    /// Returns the `FullStoryId` (`{ peer, story }`).
    #[inline]
    pub const fn full_id(&self) -> FullStoryId {
        FullStoryId::new(self.peer_id, self.id)
    }

    /// Returns the compact `StoryIdDates` descriptor.
    #[inline]
    pub const fn id_dates(&self) -> StoryIdDates {
        StoryIdDates {
            id: self.id,
            date: self.date,
            expires: self.expires,
            video_stream: matches!(self.media, StoryMedia::VideoStream(_)),
        }
    }

    /// Returns `true` if this story's media is unsupported (`Story::unsupported`).
    #[inline]
    pub const fn unsupported(&self) -> bool {
        self.media.is_unsupported()
    }

    /// Returns `true` if this story has expired at timestamp `now` (`Story::expired`).
    #[inline]
    pub const fn is_expired(&self, now: TimeId) -> bool {
        self.expires <= now
    }

    /// Returns the caption to display (empty if `unsupported()`, matching `Story::caption`).
    pub fn display_caption(&self) -> Option<&TextWithEntities> {
        if self.unsupported() {
            None
        } else {
            Some(&self.caption)
        }
    }

    /// Returns `true` if this story is a repost (`Story::repost`).
    #[inline]
    pub fn is_repost(&self) -> bool {
        self.repost.is_repost()
    }

    /// Returns `true` if this story can be downloaded if the user has Telegram Premium
    /// (`Story::canDownloadIfPremium`).
    #[inline]
    pub const fn can_download_if_premium(&self, peer_ctx: StoryPeerContext) -> bool {
        !self.no_forwards || peer_ctx.is_self
    }

    /// Returns `true` if the current account can download this story (`Story::canDownloadChecked`).
    #[inline]
    pub const fn can_download_checked(
        &self,
        peer_ctx: StoryPeerContext,
        account_premium: bool,
    ) -> bool {
        peer_ctx.is_self || (self.can_download_if_premium(peer_ctx) && account_premium)
    }

    /// Returns `true` if this story can be shared (`Story::canShare`).
    #[inline]
    pub const fn can_share(&self, now: TimeId) -> bool {
        matches!(self.privacy, StoryPrivacy::Public)
            && !self.no_forwards
            && (self.in_profile || !self.is_expired(now))
    }

    /// Returns `true` if the current account can delete this story (`Story::canDelete`).
    #[inline]
    pub const fn can_delete(&self, peer_ctx: StoryPeerContext) -> bool {
        peer_ctx.can_delete_stories || (self.out && peer_ctx.can_post_stories)
    }

    /// Returns `true` if the current account can report this story (`Story::canReport`).
    #[inline]
    pub const fn can_report(&self, peer_ctx: StoryPeerContext) -> bool {
        !peer_ctx.is_self
    }

    /// Returns `true` if this story has a public direct `t.me/<user>/s/<id>` link
    /// (`Story::hasDirectLink`).
    #[inline]
    pub const fn has_direct_link(&self, peer_ctx: StoryPeerContext, now: TimeId) -> bool {
        if !matches!(self.privacy, StoryPrivacy::Public)
            || (!self.in_profile && self.is_expired(now))
        {
            return false;
        }
        peer_ctx.has_username
    }

    /// Checks whether forwarding this story to `target` peer is permitted
    /// (`Story::errorTextForForward` in `data_story.cpp`).
    ///
    /// Returns `Ok(())` if allowed, or the blocking `ChatBannedRights` (photo/video) if forbidden.
    pub fn can_forward_to(
        &self,
        target: &Peer,
        ctx: SendCheckContext,
    ) -> Result<(), ChatBannedRights> {
        let holds_photo = matches!(self.media, StoryMedia::Photo(_));
        let (first, second) = if holds_photo {
            (ChatBannedRights::SEND_PHOTOS, ChatBannedRights::SEND_VIDEOS)
        } else {
            (ChatBannedRights::SEND_VIDEOS, ChatBannedRights::SEND_PHOTOS)
        };
        if !target.can_send(first, false, ctx) {
            return Err(first);
        }
        if !target.can_send(second, false, ctx) {
            return Err(second);
        }
        Ok(())
    }

    /// Updates our sent reaction locally and adjusts suggested reaction + total counts
    /// (`Story::setReactionId`).
    ///
    /// Returns `true` if the reaction changed.
    pub fn set_reaction(&mut self, reaction: Option<ReactionId>, is_channel: bool) -> bool {
        if self.sent_reaction == reaction {
            return false;
        }
        let was_empty = self.sent_reaction.is_none();
        if let Some(prev) = self.sent_reaction.clone() {
            self.change_suggested_reaction_count(&prev, -1, is_channel);
        }
        self.sent_reaction = reaction;
        if let Some(next) = self.sent_reaction.clone() {
            self.change_suggested_reaction_count(&next, 1, is_channel);
        }
        if self.views.known && self.sent_reaction.is_none() != was_empty {
            let delta = if was_empty { 1 } else { -1 };
            if self.views.reactions + delta >= 0 {
                self.views.reactions += delta;
            }
        }
        true
    }

    fn change_suggested_reaction_count(&mut self, id: &ReactionId, delta: i32, is_channel: bool) {
        if id.is_empty() || !is_channel {
            return;
        }
        for suggested in &mut self.suggested_reactions {
            if &suggested.reaction == id && suggested.count + delta >= 0 {
                suggested.count += delta;
            }
        }
    }

    /// Applies a `StoryViewsCounts` snapshot (`Story::updateViewsCounts` / `Story::applyViewsCounts`).
    ///
    /// Returns emitted `StoryUpdateFlags` (`VIEWS_CHANGED` and/or `REACTION`).
    pub fn apply_views_counts(&mut self, counts: StoryViewsCounts) -> StoryUpdateFlags {
        let mut flags = StoryUpdateFlags::empty();
        for suggested in &mut self.suggested_reactions {
            let v = counts
                .reactions_counts
                .get(&suggested.reaction)
                .copied()
                .unwrap_or(0);
            if suggested.count != v {
                suggested.count = v;
                flags |= StoryUpdateFlags::REACTION;
            }
        }
        if self.update_views_counts_internal(counts, true) {
            flags |= StoryUpdateFlags::VIEWS_CHANGED;
        }
        flags
    }

    pub(crate) fn update_views_counts_internal(
        &mut self,
        counts: StoryViewsCounts,
        known: bool,
    ) -> bool {
        let total = if self.views.total != 0 {
            self.views.total
        } else {
            counts.views.saturating_add(counts.forwards)
        };
        let views_changed = self.views.total != total
            || self.views.forwards != counts.forwards
            || self.views.reactions != counts.reactions
            || self.recent_viewers != counts.recent_viewers;
        if self.views.reactions != counts.reactions
            || self.views.forwards != counts.forwards
            || self.views.total != total
            || self.views.known != known
        {
            self.views = StoryViews {
                list: Vec::new(),
                next_offset: String::new(),
                reactions: counts.reactions,
                forwards: counts.forwards,
                views: counts.views,
                total,
                known,
            };
            if self.channel_reactions.total == 0 {
                self.channel_reactions.total =
                    self.views.reactions.saturating_add(self.views.forwards);
            }
        }
        if views_changed {
            self.recent_viewers = counts.recent_viewers;
        }
        views_changed
    }

    /// Applies a paginated `stories.getStoryViewsList` slice (`Story::applyViewsSlice`).
    ///
    /// Returns `true` if counters or recent viewers changed.
    pub fn apply_views_slice(&mut self, offset: &str, slice: StoryViews) -> bool {
        let mut changed = self.views.reactions != slice.reactions
            || self.views.views != slice.views
            || self.views.forwards != slice.forwards
            || self.views.total != slice.total;

        self.views.reactions = slice.reactions;
        self.views.forwards = slice.forwards;
        self.views.views = slice.views;
        self.views.total = slice.total;
        self.views.known = true;

        if offset.is_empty() {
            self.views = StoryViews {
                known: true,
                ..slice
            };
            if self.channel_reactions.total == 0 {
                self.channel_reactions.total =
                    self.views.reactions.saturating_add(self.views.forwards);
            }
        } else if self.views.next_offset == offset {
            self.views.list.extend(slice.list);
            self.views.next_offset = slice.next_offset;
            if self.views.next_offset.is_empty() {
                self.views.total = self.views.list.len() as i32;
                self.views.reactions = self
                    .views
                    .list
                    .iter()
                    .filter(|v| v.reaction.is_some())
                    .count() as i32;
                self.views.forwards = self
                    .views
                    .list
                    .iter()
                    .filter(|v| v.repost_id != 0 || !v.forward_id.is_zero())
                    .count() as i32;
            }
        }

        let known = self.views.list.len();
        if known >= self.recent_viewers.len() {
            let take = known.min(RECENT_VIEWERS_MAX);
            let viewers: Vec<PeerId> = self
                .views
                .list
                .iter()
                .take(take)
                .map(|v| v.peer_id)
                .collect();
            if self.recent_viewers != viewers {
                self.recent_viewers = viewers;
                changed = true;
            }
        }
        changed
    }

    /// Applies a paginated `stories.getStoryReactionsList` slice (`Story::applyChannelReactionsSlice`).
    ///
    /// Returns `true` if reaction counters changed.
    pub fn apply_channel_reactions_slice(&mut self, offset: &str, slice: StoryViews) -> bool {
        let changed = self.channel_reactions.reactions != slice.reactions
            || self.channel_reactions.total != slice.total;
        self.channel_reactions.reactions = slice.reactions;
        self.channel_reactions.total = slice.total;
        self.channel_reactions.known = true;
        if offset.is_empty() {
            self.channel_reactions = StoryViews {
                known: true,
                ..slice
            };
        } else if self.channel_reactions.next_offset == offset {
            self.channel_reactions.list.extend(slice.list);
            self.channel_reactions.next_offset = slice.next_offset;
            if self.channel_reactions.next_offset.is_empty() {
                self.channel_reactions.total = self.channel_reactions.list.len() as i32;
            }
        }
        changed
    }

    /// Merges updated fields from `updated` into `self` (`Story::applyFields`).
    ///
    /// Preserves immutable identity fields (`id`, `peer_id`, `date`, `expires`, `repost`, `from_peer`)
    /// and returns `(update_flags, media_changed, in_profile_changed)`.
    pub fn apply_changes(&mut self, updated: Story) -> (StoryUpdateFlags, bool, bool) {
        self.last_update_time = updated.last_update_time;
        let in_profile_changed = self.in_profile != updated.in_profile;
        let edited_changed = self.edited != updated.edited;
        let media_changed = self.media != updated.media;
        let caption_changed = self.caption != updated.caption;
        let locations_changed = self.locations != updated.locations;
        let suggested_reactions_changed = self.suggested_reactions != updated.suggested_reactions;
        let channel_posts_changed = self.channel_posts != updated.channel_posts;
        let url_areas_changed = self.url_areas != updated.url_areas;
        let weather_areas_changed = self.weather_areas != updated.weather_areas;
        let reaction_changed = self.sent_reaction != updated.sent_reaction;

        self.out = updated.out;
        self.privacy = updated.privacy;
        self.privacy_allowed = updated.privacy_allowed;
        self.privacy_disallowed = updated.privacy_disallowed;
        self.edited = updated.edited;
        self.in_profile = updated.in_profile;
        self.no_forwards = updated.no_forwards;
        self.music = updated.music;
        if media_changed {
            self.media = updated.media;
        }
        if caption_changed {
            self.caption = updated.caption;
        }
        if locations_changed {
            self.locations = updated.locations;
        }
        if suggested_reactions_changed {
            self.suggested_reactions = updated.suggested_reactions;
        }
        if channel_posts_changed {
            self.channel_posts = updated.channel_posts;
        }
        if url_areas_changed {
            self.url_areas = updated.url_areas;
        }
        if weather_areas_changed {
            self.weather_areas = updated.weather_areas;
        }
        if reaction_changed {
            self.sent_reaction = updated.sent_reaction;
        }

        let mut flags = StoryUpdateFlags::empty();
        if updated.views.known {
            let counts = StoryViewsCounts {
                views: updated.views.views,
                forwards: updated.views.forwards,
                reactions: updated.views.reactions,
                reactions_counts: BTreeMap::new(),
                recent_viewers: updated.recent_viewers,
            };
            if self.update_views_counts_internal(counts, true) {
                flags |= StoryUpdateFlags::VIEWS_CHANGED;
            }
        }
        if edited_changed
            || caption_changed
            || media_changed
            || locations_changed
            || channel_posts_changed
            || url_areas_changed
            || weather_areas_changed
        {
            flags |= StoryUpdateFlags::EDITED;
        }
        if reaction_changed || suggested_reactions_changed {
            flags |= StoryUpdateFlags::REACTION;
        }
        (flags, media_changed, in_profile_changed)
    }
}

/// Converts a `tg_data::TextWithEntities` caption into `tg_text::TextWithEntities`
/// for rich text processing / formatting in UI layers.
pub fn to_tg_text(caption: &TextWithEntities) -> tg_text::TextWithEntities {
    let entities = caption
        .entities
        .iter()
        .filter_map(|e| {
            let (ty, data) = match &e.kind {
                EntityKind::Url => (tg_text::EntityType::Url, String::new()),
                EntityKind::TextUrl { url } => (tg_text::EntityType::CustomUrl, url.clone()),
                EntityKind::Email => (tg_text::EntityType::Email, String::new()),
                EntityKind::Hashtag => (tg_text::EntityType::Hashtag, String::new()),
                EntityKind::Cashtag => (tg_text::EntityType::Cashtag, String::new()),
                EntityKind::Mention => (tg_text::EntityType::Mention, String::new()),
                EntityKind::MentionName { user_id } => (
                    tg_text::EntityType::MentionName,
                    tg_text::mention_name_data_from_fields(tg_text::MentionNameFields {
                        user_id: user_id.raw(),
                        access_hash: 0,
                        self_id: 0,
                    }),
                ),
                EntityKind::BotCommand => (tg_text::EntityType::BotCommand, String::new()),
                EntityKind::Bold => (tg_text::EntityType::Bold, String::new()),
                EntityKind::Semibold => (tg_text::EntityType::Semibold, String::new()),
                EntityKind::Italic => (tg_text::EntityType::Italic, String::new()),
                EntityKind::Underline => (tg_text::EntityType::Underline, String::new()),
                EntityKind::StrikeOut => (tg_text::EntityType::StrikeOut, String::new()),
                EntityKind::Code => (tg_text::EntityType::Code, String::new()),
                EntityKind::Pre { language } => (tg_text::EntityType::Pre, language.clone()),
                EntityKind::Spoiler => (tg_text::EntityType::Spoiler, String::new()),
                EntityKind::Blockquote { collapsed } => (
                    tg_text::EntityType::Blockquote,
                    if *collapsed {
                        "1".to_string()
                    } else {
                        String::new()
                    },
                ),
                EntityKind::CustomEmoji { document_id, .. } => (
                    tg_text::EntityType::CustomEmoji,
                    document_id.raw().to_string(),
                ),
                EntityKind::Phone => (tg_text::EntityType::Phone, String::new()),
                EntityKind::BankCard => (tg_text::EntityType::BankCard, String::new()),
                _ => return None,
            };
            Some(tg_text::EntityInText::with_data(
                ty, e.offset, e.length, data,
            ))
        })
        .collect();
    tg_text::TextWithEntities {
        text: caption.text.clone(),
        entities,
    }
}
