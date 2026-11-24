//! Port of `media/stories/media_stories_controller.h`, `media_stories_controller.cpp`,
//! `media_stories_reply.cpp`, `media_stories_recent_views.cpp`, `media_stories_sibling.cpp`,
//! `media_stories_slider.cpp`, `media_stories_share.cpp`, and `media_stories_stealth.cpp`.
//!
//! Pure UI-independent state machine for the story viewer:
//! - Current source / album list and story index tracking
//! - Auto-advance playback timer (5 000 ms photos, video `TrackState` tracking, live stream no-advance)
//! - Pause state machine (`Playing`, `Paused`, `Inactive`, `InactivePaused`) responding to
//!   hold/press, hover/tooltips, menus, layers, reply focus, expanded caption, and window focus
//! - Next/previous story and source navigation with cached sources list (`showing_unread_sources`),
//!   waiting-for-resolve / album pagination, and edge cases
//! - Sibling left/right preview descriptors
//! - Same-day slider range and active segment progress
//! - Reply area visibility, write-restriction, and placeholder rules
//! - Recent views summary and paginated views/reactions list
//! - Share / forward / report / delete / save-to-profile / pin-to-top actions
//! - Caption expand / collapse and unsupported story placeholder
//! - Stealth mode eligibility, cooldown, and activation rules
//! - Interactive media area hit-testing (location, suggested reaction, channel post, URL, weather)

use tg_data::peer::GeoPoint;
use tg_data::{FullMsgId, FullStoryId, PeerId, ReactionId, StoryId, TextWithEntities, TimeId};

use crate::ids::{StoriesIds, respecting_pinned};
use crate::store::{
    NoStory, PollingKind, StoriesContext, StoriesSource, StoriesSourceInfo, StoriesStore,
};
use crate::story::{
    PHOTO_STORY_DURATION_MS, STORIES_ALBUM_ID_ARCHIVE, StealthMode, Story, StoryMedia,
    StoryPeerContext, StoryPrivacy, StoryViews,
};

// ---------------------------------------------------------------------------
// Constants matching media/stories/media_stories_controller.cpp
// ---------------------------------------------------------------------------

/// Photo progress tick interval in ms (`kPhotoProgressInterval`).
pub const PHOTO_PROGRESS_INTERVAL_MS: i64 = 100;

/// Trigger `loadMore` when fewer than this many sources remain (`kPreloadPeersCount`).
pub const PRELOAD_PEERS_COUNT: usize = 3;

/// Trigger album `loadMore` when fewer than this many stories remain (`kPreloadStoriesCount`).
pub const PRELOAD_STORIES_COUNT: usize = 5;

/// Number of following stories preloaded in viewer (`kPreloadNextMediaCount`).
pub const PRELOAD_NEXT_MEDIA_COUNT: usize = 3;

/// Number of preceding stories preloaded in viewer (`kPreloadPreviousMediaCount`).
pub const PRELOAD_PREVIOUS_MEDIA_COUNT: usize = 1;

/// Minimum playback seconds elapsed before marking a story read (`kMarkAsReadAfterSeconds`).
pub const MARK_AS_READ_AFTER_SECONDS: f64 = 0.2;

/// Minimum playback progress ratio before marking read (`kMarkAsReadAfterProgress`).
pub const MARK_AS_READ_AFTER_PROGRESS: f64 = 0.0;

/// Views menu page increment (`kAddPerPage` in `media_stories_recent_views.cpp`).
pub const VIEWS_MENU_ADD_PER_PAGE: usize = 50;

// ---------------------------------------------------------------------------
// Playback & Pause State
// ---------------------------------------------------------------------------

/// Playback status fed into the viewer (`Media::Player::State` subset).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum PlaybackStatus {
    /// Actively advancing.
    Playing,
    /// Temporarily paused.
    Paused,
    /// Stopped / not yet started.
    #[default]
    Stopped,
    /// Reached the end of the story duration (`StoppedAtEnd`).
    StoppedAtEnd,
}

impl PlaybackStatus {
    /// Returns `true` if `self == StoppedAtEnd`.
    #[inline]
    pub const fn is_stopped_at_end(self) -> bool {
        matches!(self, Self::StoppedAtEnd)
    }

    /// Returns `true` if `self == Stopped`.
    #[inline]
    pub const fn is_stopped(self) -> bool {
        matches!(self, Self::Stopped)
    }
}

/// Video or photo playback progress snapshot (`Media::Player::TrackState` subset).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct TrackState {
    /// Current playback status.
    pub status: PlaybackStatus,
    /// Current position in ticks (at `frequency` ticks/second).
    pub position: i64,
    /// Total length in ticks.
    pub length: i64,
    /// Ticks per second (e.g. `1000` for millisecond precision).
    pub frequency: i64,
}

impl TrackState {
    /// Normalized playback progress in `[0.0, 1.0]`.
    pub fn progress(self) -> f64 {
        if self.status.is_stopped_at_end() {
            return 1.0;
        }
        if self.length <= 0 {
            return 0.0;
        }
        (self.position as f64 / self.length as f64).clamp(0.0, 1.0)
    }
}

/// High-level pause state for header controls (`Media::Stories::PauseState`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum PauseState {
    /// Playing normally.
    Playing,
    /// Explicitly paused by user (or hold/hover) while viewer is active.
    Paused,
    /// Implicitly paused because viewer is inactive (window unfocused, reply focused, menu/layer shown).
    Inactive,
    /// Both explicitly paused and inactive.
    InactivePaused,
}

/// Photo auto-advance timer (`Controller::PhotoPlayback`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PhotoPlayback {
    duration_ms: i64,
    started_ms: i64,
    paused_at_ms: Option<i64>,
    finished: bool,
}

impl PhotoPlayback {
    /// Creates a new `PhotoPlayback` starting in the paused state at `now_ms`,
    /// matching C++ `PhotoPlayback::PhotoPlayback` (`_paused(_started)`).
    pub const fn new(now_ms: i64) -> Self {
        Self::with_duration(now_ms, PHOTO_STORY_DURATION_MS)
    }

    /// Creates a `PhotoPlayback` with a custom duration in milliseconds.
    pub const fn with_duration(now_ms: i64, duration_ms: i64) -> Self {
        Self {
            duration_ms: if duration_ms > 0 {
                duration_ms
            } else {
                PHOTO_STORY_DURATION_MS
            },
            started_ms: now_ms,
            paused_at_ms: Some(now_ms),
            finished: false,
        }
    }

    /// Returns `true` if currently paused.
    #[inline]
    pub const fn is_paused(&self) -> bool {
        self.paused_at_ms.is_some()
    }

    /// Returns `true` if the photo duration has completed.
    #[inline]
    pub const fn is_finished(&self) -> bool {
        self.finished
    }

    /// Configured duration in milliseconds.
    #[inline]
    pub const fn duration_ms(&self) -> i64 {
        self.duration_ms
    }

    /// Toggles pause state at `now_ms`, preserving elapsed time (`PhotoPlayback::togglePaused`).
    pub fn set_paused(&mut self, paused: bool, now_ms: i64) -> Option<TrackState> {
        if self.paused_at_ms.is_some() == paused {
            return None;
        }
        if paused {
            if now_ms - self.started_ms >= self.duration_ms {
                return None;
            }
            self.paused_at_ms = Some(now_ms);
        } else if let Some(paused_at) = self.paused_at_ms.take() {
            self.started_ms += now_ms - paused_at;
        }
        Some(self.state_at(now_ms))
    }

    /// Evaluates current `TrackState` at `now_ms` (`PhotoPlayback::callback`).
    pub fn state_at(&mut self, now_ms: i64) -> TrackState {
        let effective_now = self.paused_at_ms.unwrap_or(now_ms);
        let elapsed = (effective_now - self.started_ms).max(0);
        if elapsed >= self.duration_ms {
            self.finished = true;
            return TrackState {
                status: PlaybackStatus::StoppedAtEnd,
                position: self.duration_ms,
                length: self.duration_ms,
                frequency: 1_000,
            };
        }
        let status = if self.paused_at_ms.is_some() {
            PlaybackStatus::Paused
        } else {
            PlaybackStatus::Playing
        };
        TrackState {
            status,
            position: elapsed,
            length: self.duration_ms,
            frequency: 1_000,
        }
    }

    /// Returns remaining milliseconds until finish when playing, or `None` if paused/finished.
    pub fn remaining_ms(&self, now_ms: i64) -> Option<i64> {
        if self.finished || self.paused_at_ms.is_some() {
            return None;
        }
        Some((self.started_ms + self.duration_ms - now_ms).max(0))
    }
}

// ---------------------------------------------------------------------------
// Same-day slider range, Siblings, Reply Area, Recent Views, Stealth Mode
// ---------------------------------------------------------------------------

/// Inclusive index range `[from, till]` for stories posted on the same calendar day
/// (`SameDayRange` / `ComputeSameDayRange` in `media_stories_controller.cpp:87-129`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct SameDayRange {
    /// First index in the same-day group (inclusive).
    pub from: usize,
    /// Last index in the same-day group (inclusive).
    pub till: usize,
}

/// Computes the same-day slider segment range around `index` within `sorted` album IDs.
///
/// Pinned-to-top stories form their own group `0..pinned`. Non-pinned stories are grouped by
/// UTC calendar day (`date.div_euclid(86_400)`).
pub fn compute_same_day_range(
    store: &StoriesStore,
    peer_id: PeerId,
    story_date: TimeId,
    ids: &StoriesIds,
    sorted: &[StoryId],
    index: usize,
) -> SameDayRange {
    if sorted.is_empty() || index >= sorted.len() {
        return SameDayRange::default();
    }
    let pinned = ids.pinned_to_top.len();
    if index < pinned {
        return SameDayRange {
            from: 0,
            till: pinned.saturating_sub(1),
        };
    }
    let target_day = story_date.div_euclid(86_400);
    let mut from = index;
    while from > pinned {
        let prev_id = sorted[from - 1];
        if let Ok(prev) = store.lookup(FullStoryId::new(peer_id, prev_id))
            && prev.date.div_euclid(86_400) != target_day
        {
            break;
        }
        from -= 1;
    }
    let mut till = index;
    while till + 1 < sorted.len() {
        let next_id = sorted[till + 1];
        if let Ok(next) = store.lookup(FullStoryId::new(peer_id, next_id))
            && next.date.div_euclid(86_400) != target_day
        {
            break;
        }
        till += 1;
    }
    SameDayRange { from, till }
}

/// Slider progress bar state at the top of the story viewer (`Media::Stories::SliderData`).
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct SliderState {
    /// Active segment index (`0..total`).
    pub index: usize,
    /// Total number of segments (`>= 1`).
    pub total: usize,
    /// Normalized progress of the active segment in `[0.0, 1.0]`.
    pub active_progress: f64,
    /// Whether the story is a live video stream (slider segments are hidden for live streams).
    pub video_stream: bool,
}

/// Left or right sibling peer preview descriptor (`Media::Stories::Sibling`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SiblingPreview {
    /// Sibling peer ID.
    pub peer_id: PeerId,
    /// Story ID that will be opened when jumping to this sibling (`LookupShownId`).
    pub story_id: StoryId,
    /// Whether this sibling is the self user.
    pub is_self: bool,
}

impl SiblingPreview {
    /// Resolves which story ID a sibling should display (`LookupShownId` in `media_stories_sibling.cpp:37-46`).
    pub fn from_source(
        source: &StoriesSource,
        suggested_id: StoryId,
        is_self: bool,
    ) -> Option<Self> {
        let shown = if suggested_id != 0 && source.index_of(suggested_id).is_some() {
            suggested_id
        } else {
            source.to_open()?.id
        };
        if shown == 0 {
            return None;
        }
        Some(Self {
            peer_id: source.peer_id,
            story_id: shown,
            is_self,
        })
    }

    /// Returns the `FullStoryId` of this sibling preview.
    #[inline]
    pub const fn full_id(&self) -> FullStoryId {
        FullStoryId::new(self.peer_id, self.story_id)
    }
}

/// Kind of reply input shown below the story (`Media::Stories::ReplyAreaType`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ReplyAreaKind {
    /// Direct private reply to a user's story.
    Reply,
    /// Comment reply on a megagroup's story.
    Comment,
    /// Live chat comment on a video stream story.
    VideoStreamComment,
}

/// Write restriction state for the reply area (`media_stories_reply.cpp:866-899`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum ReplyWriteRestriction {
    /// Replying is allowed.
    #[default]
    None,
    /// Video stream metadata is still loading (hide input temporarily).
    LoadingHidden,
    /// Video stream comments are disabled by the host.
    VideoStreamCommentsDisabled,
    /// Account is frozen.
    AccountFrozen,
    /// Target user requires Telegram Premium to write messages.
    PremiumRequired,
}

/// Placeholder kind displayed in the reply text field (`PlaceholderText` in `media_stories_reply.cpp:65-110`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplyPlaceholder {
    /// Standard "Reply privately..." placeholder.
    Reply,
    /// Megagroup "Comment..." placeholder.
    Comment,
    /// Live video stream comment placeholder.
    VideoStreamComment,
    /// Paid Stars message/comment placeholder (`stars_per_message > 0`).
    PaidStars {
        /// Stars required per message/comment.
        stars: i32,
        /// Whether this is for a live video stream comment.
        video_stream: bool,
    },
    /// Stealth mode countdown placeholder (`"Stealth Mode active: <left>"`).
    StealthModeCountdown {
        /// Formatted remaining time (`time_left_text`).
        time_left: String,
    },
}

/// Computed reply area visibility and state (`ReplyArea::show` in `media_stories_reply.cpp:824-946`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplyAreaState {
    /// Hidden (self stories, broadcast channel stories, or Telegram service changelog stories).
    Hidden,
    /// Shows the "You can't reply to this story" banner (unsupported story or missing peer).
    CantReply,
    /// Interactive reply / comment composer is visible.
    Visible {
        /// Reply vs comment vs video stream comment.
        kind: ReplyAreaKind,
        /// Active write restriction (if any).
        restriction: ReplyWriteRestriction,
        /// Input field placeholder.
        placeholder: ReplyPlaceholder,
        /// Whether the quick-like button is shown (`!video_stream`).
        likes_enabled: bool,
        /// Whether file/media attachments are enabled (`!video_stream`).
        attachments_enabled: bool,
    },
}

/// Inputs needed to evaluate reply area visibility and restrictions.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ReplyContext {
    /// Whether the current logged-in account has Telegram Premium.
    pub account_premium: bool,
    /// Whether the current logged-in account is frozen.
    pub account_frozen: bool,
    /// Whether the current account can send messages to the story's peer (`CanSendAnything`).
    pub can_send_anything: bool,
    /// Whether the target user requires Premium to initiate messages (`user->requiresPremiumToWrite()`).
    pub peer_requires_premium_to_write: bool,
    /// Stars per message required by the peer (`0` if free).
    pub stars_per_message: i32,
    /// Whether the current user can manage the live stream (exempt from comment stars price).
    pub can_manage_video_stream: bool,
}

/// Computes the `ReplyAreaState` for a story (`media_stories_reply.cpp:860-946`).
pub fn compute_reply_area_state(
    story: &Story,
    peer_ctx: StoryPeerContext,
    reply_ctx: ReplyContext,
    stealth_mode: StealthMode,
    now_secs: TimeId,
) -> ReplyAreaState {
    if story.unsupported() {
        return ReplyAreaState::CantReply;
    }
    let stream = story.media.as_video_stream();
    let hidden =
        (peer_ctx.is_broadcast || peer_ctx.is_self || peer_ctx.is_service_user) && stream.is_none();
    if hidden {
        return ReplyAreaState::Hidden;
    }

    let kind = if stream.is_some() {
        ReplyAreaKind::VideoStreamComment
    } else if peer_ctx.is_megagroup {
        ReplyAreaKind::Comment
    } else {
        ReplyAreaKind::Reply
    };

    let restriction = if let Some(s) = stream {
        if !s.loaded {
            ReplyWriteRestriction::LoadingHidden
        } else if s.messages_enabled {
            ReplyWriteRestriction::None
        } else {
            ReplyWriteRestriction::VideoStreamCommentsDisabled
        }
    } else if reply_ctx.account_frozen {
        ReplyWriteRestriction::AccountFrozen
    } else if !reply_ctx.can_send_anything
        && peer_ctx.is_user
        && reply_ctx.peer_requires_premium_to_write
        && !reply_ctx.account_premium
    {
        ReplyWriteRestriction::PremiumRequired
    } else {
        ReplyWriteRestriction::None
    };

    let stars = if let Some(s) = stream {
        if reply_ctx.can_manage_video_stream {
            0
        } else {
            s.messages_min_price
        }
    } else {
        reply_ctx.stars_per_message
    };

    let stealth_left = stealth_mode.active_seconds_left(now_secs);
    let placeholder = match kind {
        ReplyAreaKind::VideoStreamComment => {
            if stars > 0 {
                ReplyPlaceholder::PaidStars {
                    stars,
                    video_stream: true,
                }
            } else {
                ReplyPlaceholder::VideoStreamComment
            }
        }
        ReplyAreaKind::Comment | ReplyAreaKind::Reply => {
            if stars > 0 {
                ReplyPlaceholder::PaidStars {
                    stars,
                    video_stream: false,
                }
            } else if stealth_left > 0 {
                ReplyPlaceholder::StealthModeCountdown {
                    time_left: time_left_text(stealth_left),
                }
            } else if kind == ReplyAreaKind::Comment {
                ReplyPlaceholder::Comment
            } else {
                ReplyPlaceholder::Reply
            }
        }
    };

    ReplyAreaState::Visible {
        kind,
        restriction,
        placeholder,
        likes_enabled: stream.is_none(),
        attachments_enabled: stream.is_none(),
    }
}

/// Footer views/reactions presentation mode (`Media::Stories::RecentViewsType`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum RecentViewsType {
    /// Own stories: clickable views count + recent viewer avatars.
    SelfViews,
    /// Broadcast channel stories: views counter + like button + optional reactions menu for admins.
    Channel,
    /// Telegram service changelog stories: "You can't reply" + like button.
    Changelog,
    /// Regular peer stories or live video streams.
    Other,
}

/// Computes `RecentViewsType` for a peer (`RecentViewsTypeFor` in `media_stories_recent_views.cpp:140-152`).
pub const fn recent_views_type_for(
    peer_ctx: StoryPeerContext,
    video_stream: bool,
) -> RecentViewsType {
    if video_stream {
        RecentViewsType::Other
    } else if peer_ctx.is_self {
        RecentViewsType::SelfViews
    } else if peer_ctx.is_broadcast {
        RecentViewsType::Channel
    } else if peer_ctx.is_service_user {
        RecentViewsType::Changelog
    } else {
        RecentViewsType::Other
    }
}

/// Recent views footer snapshot (`RecentViewsData`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecentViewsState {
    /// Footer display mode.
    pub kind: Option<RecentViewsType>,
    /// Up to 3 recent viewer peer IDs.
    pub recent_viewers: Vec<PeerId>,
    /// Views counter.
    pub views: i32,
    /// Forwards counter.
    pub forwards: i32,
    /// Reactions counter.
    pub reactions: i32,
    /// Total interactions counter.
    pub total: i32,
    /// Whether the detailed reactions/viewers menu can be opened.
    pub can_view_reactions: bool,
}

/// Stealth mode activation decision (`SetupStealthMode` & `StealthModeBox` in `media_stories_stealth.cpp`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StealthActivationDecision {
    /// Stealth mode is already active (`enabled_till > now`).
    AlreadyActive {
        /// Remaining active seconds.
        seconds_left: i32,
    },
    /// User does not have Telegram Premium (`ShowPremiumPreviewBox`).
    RequiresPremium,
    /// Stealth mode is on cooldown (`cooldown_till > now`).
    InCooldown {
        /// Remaining cooldown seconds.
        seconds_left: i32,
    },
    /// Eligible to activate stealth mode now (`stories.activateStealthMode(past=true, future=true)`).
    ReadyToActivate,
}

/// Evaluates stealth mode activation rules (`media_stories_stealth.cpp:331-380`).
pub const fn evaluate_stealth_mode(
    mode: StealthMode,
    now_secs: TimeId,
    is_premium: bool,
) -> StealthActivationDecision {
    if mode.enabled_till > now_secs {
        StealthActivationDecision::AlreadyActive {
            seconds_left: mode.enabled_till - now_secs,
        }
    } else if !is_premium {
        StealthActivationDecision::RequiresPremium
    } else if mode.cooldown_till > now_secs {
        StealthActivationDecision::InCooldown {
            seconds_left: mode.cooldown_till - now_secs,
        }
    } else {
        StealthActivationDecision::ReadyToActivate
    }
}

/// Formats remaining seconds into `H:MM:SS`, `M:SS`, or `0:SS` (`TimeLeftText` in `media_stories_stealth.cpp:410-426`).
pub fn time_left_text(left: i32) -> String {
    let clamped = left.max(0);
    let hours = clamped / 3600;
    let minutes = (clamped % 3600) / 60;
    let seconds = clamped % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else if minutes > 0 {
        format!("{minutes}:{seconds:02}")
    } else {
        format!("0:{seconds:02}")
    }
}

/// Share / forward capability for the currently shown story (`PrepareShareBox` in `media_stories_share.cpp`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ShareCapability {
    /// Story cannot be shared.
    Unavailable,
    /// Live video stream story: can only be shared via its direct link (`shareJustLink`).
    DirectLinkOnly {
        /// Story ID.
        story_id: FullStoryId,
    },
    /// Regular shareable story (`inputMediaStory`), optionally with copyable direct link.
    ShareStory {
        /// Story ID.
        story_id: FullStoryId,
        /// Whether a public `t.me/<user>/s/<id>` direct link is available.
        can_copy_link: bool,
    },
}

/// Interactive area hit result (`Controller::lookupAreaHandler`).
#[derive(Clone, Debug, PartialEq)]
pub enum StoryAreaHit {
    /// Location or venue area tapped.
    Location {
        /// Geographic coordinates.
        point: GeoPoint,
        /// Venue title (if any).
        title: String,
    },
    /// Suggested reaction sticker tapped.
    SuggestedReaction {
        /// Reaction to send.
        reaction: ReactionId,
    },
    /// Channel post area tapped.
    ChannelPost {
        /// Target channel message ID.
        item_id: FullMsgId,
    },
    /// URL or Star Gift (`tg://nft?slug=...`) area tapped.
    Url {
        /// Target URL.
        url: String,
    },
    /// Weather area tapped (toggles Celsius/Fahrenheit).
    Weather {
        /// Temperature in millicelsius.
        millicelsius: i32,
        /// Weather emoji.
        emoji: String,
    },
}

/// Events emitted by `StoryViewerController` for the host UI / window controller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ViewerEvent {
    /// Displayed story changed to `id`.
    StoryShown {
        /// Newly displayed story ID.
        id: FullStoryId,
        /// Active viewing context.
        context: StoriesContext,
    },
    /// Viewer should close (finished last story, or shown story was destroyed).
    Close,
    /// Unsupported story placeholder should prompt app update.
    UnsupportedStoryShown(FullStoryId),
    /// Playback paused/resumed state changed for external video player.
    VideoPauseChanged(bool),
    /// Share dialog requested for current story.
    ShareRequested(ShareCapability),
    /// Report dialog requested for current story.
    ReportRequested(FullStoryId),
    /// Stealth mode dialog / activation evaluated.
    StealthModeRequested(StealthActivationDecision),
    /// More views entries loaded into the viewer's cached `views_slice`.
    MoreViewsLoaded,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StoriesList {
    peer_id: PeerId,
    ids: StoriesIds,
    sorted: Vec<StoryId>,
    total: usize,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct CachedSource {
    peer_id: PeerId,
    shown_id: StoryId,
}

/// Account-level viewer configuration flags.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ViewerConfig {
    /// Whether the current account has Telegram Premium.
    pub account_premium: bool,
    /// Whether Premium features (like stealth mode) are possible on this platform/account.
    pub premium_possible: bool,
    /// Whether the current user is an admin/creator of the viewed channel (for viewing reactions).
    pub can_view_channel_reactions: bool,
    /// Reply area context for the currently viewed peer.
    pub reply_ctx: ReplyContext,
}

impl Default for ViewerConfig {
    fn default() -> Self {
        Self {
            account_premium: false,
            premium_possible: true,
            can_view_channel_reactions: false,
            reply_ctx: ReplyContext {
                can_send_anything: true,
                ..ReplyContext::default()
            },
        }
    }
}

/// Pure-logic story viewer controller (`Media::Stories::Controller`).
#[derive(Debug)]
pub struct StoryViewerController {
    shown: Option<FullStoryId>,
    context: StoriesContext,
    source: Option<StoriesSource>,
    list: Option<StoriesList>,
    index: usize,
    slider_index: usize,
    slider_count: usize,
    active_progress: f64,

    // Playback & pause state.
    started: bool,
    viewed: bool,
    window_active: bool,
    manual_paused: bool,
    hold_pressed: bool,
    hover_paused: bool,
    reply_active: bool,
    caption_expanded: bool,
    caption_closing: bool,
    layer_shown: bool,
    menu_shown: bool,
    tooltip_shown: bool,
    photo_playback: Option<PhotoPlayback>,
    video_stream: bool,
    unsupported: bool,
    last_playing_allowed: Option<bool>,

    // Content metadata snapshots.
    caption_text: TextWithEntities,
    repost: bool,
    weather_in_celsius: bool,

    // Navigation waiting state.
    waiting_for_id: Option<FullStoryId>,
    waiting_for_delta: i32,

    // Cached sources list for sibling navigation.
    cached_sources: Vec<CachedSource>,
    cached_source_index: Option<usize>,
    showing_unread_sources: bool,
    sibling_left: Option<SiblingPreview>,
    sibling_right: Option<SiblingPreview>,

    // Views & footer.
    views_slice: StoryViews,
    recent_views: RecentViewsState,
    reply_area: ReplyAreaState,

    // Emitted events drained by host.
    events: Vec<ViewerEvent>,
}

impl Default for StoryViewerController {
    fn default() -> Self {
        Self::new()
    }
}

impl StoryViewerController {
    /// Creates a new idle `StoryViewerController` with `window_active = true`.
    pub fn new() -> Self {
        Self {
            shown: None,
            context: StoriesContext::Single,
            source: None,
            list: None,
            index: 0,
            slider_index: 0,
            slider_count: 0,
            active_progress: 0.0,
            started: false,
            viewed: false,
            window_active: true,
            manual_paused: false,
            hold_pressed: false,
            hover_paused: false,
            reply_active: false,
            caption_expanded: false,
            caption_closing: false,
            layer_shown: false,
            menu_shown: false,
            tooltip_shown: false,
            photo_playback: None,
            video_stream: false,
            unsupported: false,
            last_playing_allowed: None,
            caption_text: TextWithEntities::default(),
            repost: false,
            weather_in_celsius: true,
            waiting_for_id: None,
            waiting_for_delta: 0,
            cached_sources: Vec::new(),
            cached_source_index: None,
            showing_unread_sources: false,
            sibling_left: None,
            sibling_right: None,
            views_slice: StoryViews::default(),
            recent_views: RecentViewsState::default(),
            reply_area: ReplyAreaState::Hidden,
            events: Vec::new(),
        }
    }

    /// Drains all emitted `ViewerEvent`s.
    pub fn take_events(&mut self) -> Vec<ViewerEvent> {
        std::mem::take(&mut self.events)
    }

    /// Returns pending events without draining.
    pub fn pending_events(&self) -> &[ViewerEvent] {
        &self.events
    }

    /// Currently shown `FullStoryId`, if any.
    #[inline]
    pub const fn shown_id(&self) -> Option<FullStoryId> {
        self.shown
    }

    /// Active viewing context.
    #[inline]
    pub const fn context(&self) -> StoriesContext {
        self.context
    }

    /// Current 0-based index within the active source or album list.
    #[inline]
    pub const fn current_index(&self) -> usize {
        self.index
    }

    /// Total story count in the active source or album list.
    pub fn shown_count(&self) -> usize {
        if let Some(src) = &self.source {
            src.ids.len()
        } else if let Some(list) = &self.list {
            list.total
        } else {
            0
        }
    }

    /// Returns the `StoryId` at `index` within the active source or album list, or `0` if not yet loaded.
    pub fn story_id_at(&self, index: usize) -> StoryId {
        if let Some(src) = &self.source {
            src.get_index(index).map_or(0, |s| s.id)
        } else if let Some(list) = &self.list {
            list.sorted.get(index).copied().unwrap_or(0)
        } else {
            0
        }
    }

    /// Returns the current slider bar state.
    pub fn slider_state(&self) -> SliderState {
        let total = if self.slider_count > 0 {
            self.slider_count
        } else {
            self.shown_count().max(1)
        };
        let index = if self.slider_count > 0 {
            self.slider_index
        } else {
            self.index
        }
        .min(total.saturating_sub(1));
        SliderState {
            index,
            total,
            active_progress: self.active_progress,
            video_stream: self.video_stream,
        }
    }

    /// Returns left sibling preview, if available.
    pub fn sibling_left(&self) -> Option<&SiblingPreview> {
        self.sibling_left.as_ref()
    }

    /// Returns right sibling preview, if available.
    pub fn sibling_right(&self) -> Option<&SiblingPreview> {
        self.sibling_right.as_ref()
    }

    /// Returns cached peer navigation order (`PeerId`s).
    pub fn cached_source_peers(&self) -> Vec<PeerId> {
        self.cached_sources.iter().map(|s| s.peer_id).collect()
    }

    /// Returns whether the viewer is filtered to unread sources only.
    #[inline]
    pub const fn showing_unread_sources(&self) -> bool {
        self.showing_unread_sources
    }

    /// Returns whether the currently displayed story is unsupported (`Controller::Unsupported`).
    #[inline]
    pub const fn is_unsupported(&self) -> bool {
        self.unsupported
    }

    /// Returns whether the currently displayed story is a live video stream.
    #[inline]
    pub const fn is_video_stream(&self) -> bool {
        self.video_stream
    }

    /// Returns the caption text for the currently shown story (empty if unsupported).
    pub fn caption_text(&self) -> &TextWithEntities {
        &self.caption_text
    }

    /// Returns `true` if caption overlay should be hidden (`Controller::skipCaption`).
    pub fn skip_caption(&self) -> bool {
        self.video_stream || self.caption_expanded || (self.caption_text.is_empty() && !self.repost)
    }

    /// Returns `true` if the full caption sheet is expanded.
    #[inline]
    pub const fn is_caption_expanded(&self) -> bool {
        self.caption_expanded
    }

    /// Returns `true` if the content background should be dimmed/faded (`_contentFaded`).
    #[inline]
    pub const fn is_content_faded(&self) -> bool {
        self.reply_active || (self.caption_expanded && !self.caption_closing)
    }

    /// Returns the current reply area visibility & state.
    pub fn reply_area(&self) -> &ReplyAreaState {
        &self.reply_area
    }

    /// Returns the current recent views footer state.
    pub fn recent_views(&self) -> &RecentViewsState {
        &self.recent_views
    }

    /// Returns the current weather temperature display unit (`true` = Celsius, `false` = Fahrenheit).
    #[inline]
    pub const fn weather_in_celsius(&self) -> bool {
        self.weather_in_celsius
    }

    /// Returns `true` if the current story has been marked as viewed/read.
    #[inline]
    pub const fn is_viewed(&self) -> bool {
        self.viewed
    }

    /// Returns the `FullStoryId` we are waiting to resolve before jumping, if any.
    #[inline]
    pub const fn waiting_for_id(&self) -> Option<FullStoryId> {
        self.waiting_for_id
    }

    /// Returns the pending album-pagination delta, if any.
    #[inline]
    pub const fn waiting_for_delta(&self) -> i32 {
        self.waiting_for_delta
    }

    // -----------------------------------------------------------------------
    // Opening & Closing Stories (`Controller::show`)
    // -----------------------------------------------------------------------

    /// Opens `story_id` in `context` (`Controller::show` in `media_stories_controller.cpp:847-965`).
    ///
    /// Returns `false` if `story_id` is not currently loaded in `store`.
    pub fn show(
        &mut self,
        store: &mut StoriesStore,
        story_id: FullStoryId,
        context: StoriesContext,
        cfg: ViewerConfig,
        now_secs: TimeId,
        now_ms: i64,
    ) -> bool {
        let Ok(story) = store.lookup(story_id).cloned() else {
            return false;
        };
        let peer_ctx = store.peer_meta(story_id.peer).ctx;

        // If switching away from an existing story, unregister its viewer polling.
        if let Some(prev) = self.shown
            && prev != story_id
        {
            store.unregister_polling(prev, PollingKind::Viewer);
        }

        // Reset cached sources when switching context kind.
        if self.context != context {
            self.cached_sources.clear();
            self.cached_source_index = None;
            self.showing_unread_sources = false;
        }
        self.context = context;
        self.waiting_for_id = None;
        self.waiting_for_delta = 0;
        self.video_stream = story.media.as_video_stream().is_some();
        self.unsupported = story.unsupported();

        self.rebuild_from_context(store, story_id);

        // Reset playback state for newly displayed story.
        self.manual_paused = false;
        self.hold_pressed = false;
        self.started = false;
        self.active_progress = 0.0;
        self.caption_expanded = false;
        self.caption_closing = false;
        self.caption_text = story.display_caption().cloned().unwrap_or_default();
        self.repost = story.is_repost() || !story.channel_posts.is_empty();
        self.photo_playback =
            if matches!(story.media, StoryMedia::Video { .. }) || self.video_stream {
                None
            } else {
                Some(PhotoPlayback::new(now_ms))
            };

        let changed_story = self.shown != Some(story_id);
        self.shown = Some(story_id);
        if changed_story {
            store.register_polling(story_id, PollingKind::Viewer, now_secs, now_ms);
            self.viewed = false;
            self.reply_active = false;
        }

        self.reply_area = compute_reply_area_state(
            &story,
            peer_ctx,
            cfg.reply_ctx,
            store.stealth_mode(),
            now_secs,
        );
        self.refresh_recent_views(&story, peer_ctx, cfg.can_view_channel_reactions);

        store.load_around(story_id, context);
        self.update_playing_allowed(now_ms);

        self.events.push(ViewerEvent::StoryShown {
            id: story_id,
            context,
        });
        if self.unsupported {
            self.events
                .push(ViewerEvent::UnsupportedStoryShown(story_id));
        }
        true
    }

    /// Closes the viewer, unregistering viewer polling and clearing viewer preloads.
    pub fn close(&mut self, store: &mut StoriesStore) {
        if let Some(prev) = self.shown.take() {
            store.unregister_polling(prev, PollingKind::Viewer);
        }
        store.set_preloading_in_viewer(Vec::new());
        self.source = None;
        self.list = None;
        self.photo_playback = None;
        self.sibling_left = None;
        self.sibling_right = None;
        self.cached_sources.clear();
        self.cached_source_index = None;
        self.waiting_for_id = None;
        self.waiting_for_delta = 0;
        self.started = false;
        self.viewed = false;
    }

    /// Signals that the story media has finished loading and is ready to play (`Controller::ready`).
    pub fn ready(&mut self, now_ms: i64) {
        if self.started {
            return;
        }
        self.started = true;
        self.update_playing_allowed(now_ms);
    }

    // -----------------------------------------------------------------------
    // Pause & Playback State Machine
    // -----------------------------------------------------------------------

    /// Returns whether user/hold/hover pause is set.
    #[inline]
    pub const fn is_explicitly_paused(&self) -> bool {
        self.manual_paused || self.hold_pressed || self.hover_paused
    }

    /// Computes `PauseState` (`Controller::pauseState` in `media_stories_controller.cpp:1118-1131`).
    pub const fn pause_state(&self) -> PauseState {
        let inactive = !self.window_active
            || self.reply_active
            || self.layer_shown
            || self.menu_shown
            || (self.caption_expanded && !self.caption_closing);
        let explicitly_paused = self.is_explicitly_paused();
        if !inactive && !explicitly_paused {
            PauseState::Playing
        } else if !inactive {
            PauseState::Paused
        } else if explicitly_paused {
            PauseState::InactivePaused
        } else {
            PauseState::Inactive
        }
    }

    /// Returns `true` if playback is currently allowed to advance (`Controller::updatePlayingAllowed`).
    pub const fn playing_allowed(&self) -> bool {
        self.shown.is_some()
            && self.started
            && self.window_active
            && !self.manual_paused
            && !self.hold_pressed
            && !self.hover_paused
            && !self.reply_active
            && (!self.caption_expanded || self.caption_closing)
            && !self.layer_shown
            && !self.menu_shown
            && !self.tooltip_shown
    }

    /// Toggles explicit play/pause button (`Controller::togglePaused`).
    pub fn toggle_paused(&mut self, paused: bool, now_ms: i64) {
        if self.manual_paused != paused {
            self.manual_paused = paused;
            self.update_playing_allowed(now_ms);
        }
    }

    /// Press-and-hold on story content (`Controller::contentPressed`).
    ///
    /// Pressing pauses playback and collapses any expanded caption view.
    pub fn content_pressed(&mut self, pressed: bool, now_ms: i64) {
        if self.hold_pressed != pressed {
            self.hold_pressed = pressed;
            if pressed && self.caption_expanded {
                self.caption_expanded = false;
                self.caption_closing = false;
            }
            self.update_playing_allowed(now_ms);
        }
    }

    /// Sets hover-pause state (e.g. hovering interactive area/controls).
    pub fn set_hover_paused(&mut self, hovered: bool, now_ms: i64) {
        if self.hover_paused != hovered {
            self.hover_paused = hovered;
            self.update_playing_allowed(now_ms);
        }
    }

    /// Sets popup menu visibility (`Controller::setMenuShown`).
    pub fn set_menu_shown(&mut self, shown: bool, now_ms: i64) {
        if self.menu_shown != shown {
            self.menu_shown = shown;
            self.update_playing_allowed(now_ms);
        }
    }

    /// Sets modal layer visibility (`storiesLayerShown`).
    pub fn set_layer_shown(&mut self, shown: bool, now_ms: i64) {
        if self.layer_shown != shown {
            self.layer_shown = shown;
            self.update_playing_allowed(now_ms);
        }
    }

    /// Sets header tooltip visibility (`_header->tooltipShownValue`).
    pub fn set_tooltip_shown(&mut self, shown: bool, now_ms: i64) {
        if self.tooltip_shown != shown {
            self.tooltip_shown = shown;
            self.update_playing_allowed(now_ms);
        }
    }

    /// Sets reply area / reaction strip active state (`_replyActive`).
    pub fn set_reply_active(&mut self, active: bool, now_ms: i64) {
        if self.reply_active != active {
            self.reply_active = active;
            self.update_playing_allowed(now_ms);
        }
    }

    /// Sets OS window active/focus state (`_windowActive`).
    pub fn set_window_active(&mut self, active: bool, now_ms: i64) {
        if self.window_active != active {
            self.window_active = active;
            self.update_playing_allowed(now_ms);
        }
    }

    /// Expands the full caption sheet (`Controller::showFullCaption`).
    ///
    /// Does nothing if the caption is empty.
    pub fn show_full_caption(&mut self, now_ms: i64) -> bool {
        if self.caption_text.is_empty() {
            return false;
        }
        self.caption_expanded = true;
        self.caption_closing = false;
        self.update_playing_allowed(now_ms);
        true
    }

    /// Begins closing animation for the full caption sheet (`Controller::captionClosing`).
    pub fn caption_closing(&mut self, now_ms: i64) {
        if self.caption_expanded {
            self.caption_closing = true;
            self.update_playing_allowed(now_ms);
        }
    }

    /// Completes closing the full caption sheet (`Controller::captionClosed`).
    pub fn caption_closed(&mut self, now_ms: i64) {
        if self.caption_expanded {
            self.caption_expanded = false;
            self.caption_closing = false;
            self.update_playing_allowed(now_ms);
        }
    }

    fn update_playing_allowed(&mut self, now_ms: i64) {
        if self.shown.is_none() {
            return;
        }
        let allowed = self.playing_allowed();
        if let Some(photo) = &mut self.photo_playback {
            if let Some(state) = photo.set_paused(!allowed, now_ms) {
                self.active_progress = state.progress();
            }
        } else if self.last_playing_allowed != Some(allowed) {
            self.last_playing_allowed = Some(allowed);
            self.events.push(ViewerEvent::VideoPauseChanged(!allowed));
        }
    }

    /// Advances photo playback timer to `(now_secs, now_ms)`, marking read after 200 ms and
    /// auto-advancing when the 5 s photo duration completes.
    pub fn tick(
        &mut self,
        store: &mut StoriesStore,
        cfg: ViewerConfig,
        now_secs: TimeId,
        now_ms: i64,
    ) {
        let Some(state) = self.photo_playback.as_mut().map(|p| p.state_at(now_ms)) else {
            return;
        };
        self.update_playback(store, state, cfg, now_secs, now_ms);
    }

    /// Feeds a video player `TrackState` update into the viewer (`Controller::updateVideoPlayback`).
    pub fn update_video_playback(
        &mut self,
        store: &mut StoriesStore,
        state: TrackState,
        cfg: ViewerConfig,
        now_secs: TimeId,
        now_ms: i64,
    ) {
        self.update_playback(store, state, cfg, now_secs, now_ms);
    }

    fn update_playback(
        &mut self,
        store: &mut StoriesStore,
        state: TrackState,
        cfg: ViewerConfig,
        now_secs: TimeId,
        now_ms: i64,
    ) {
        self.active_progress = state.progress();
        self.maybe_mark_as_read(store, state, now_secs, now_ms);
        if state.status.is_stopped_at_end() && !self.subjump_for(store, 1, cfg, now_secs, now_ms) {
            self.events.push(ViewerEvent::Close);
        }
    }

    fn maybe_mark_as_read(
        &mut self,
        store: &mut StoriesStore,
        state: TrackState,
        now_secs: TimeId,
        now_ms: i64,
    ) {
        let position = if state.status.is_stopped_at_end() {
            state.length
        } else if state.status.is_stopped() {
            0
        } else {
            state.position
        };
        if state.frequency > 0
            && (position as f64) > (state.frequency as f64) * MARK_AS_READ_AFTER_SECONDS
            && (position as f64) > MARK_AS_READ_AFTER_PROGRESS * (state.length as f64)
        {
            self.mark_as_read(store, now_secs, now_ms);
        }
    }

    fn mark_as_read(&mut self, store: &mut StoriesStore, now_secs: TimeId, now_ms: i64) {
        if self.viewed {
            return;
        }
        let Some(id) = self.shown else {
            return;
        };
        self.viewed = true;
        store.mark_as_read(id, now_secs, now_ms);
    }

    /// Returns milliseconds until the active photo story completes (if playing), for scheduling wakeups.
    pub fn next_photo_tick_ms(&self, now_ms: i64) -> Option<i64> {
        let photo = self.photo_playback.as_ref()?;
        let rem = photo.remaining_ms(now_ms)?;
        Some(rem.min(PHOTO_PROGRESS_INTERVAL_MS))
    }

    // -----------------------------------------------------------------------
    // Navigation (`subjumpAvailable`, `subjumpFor`, `jumpFor`, `moveFromShown`)
    // -----------------------------------------------------------------------

    /// Returns `true` if stepping by `delta` stories (including into sibling sources) is possible
    /// (`Controller::subjumpAvailable`).
    pub fn subjump_available(&self, delta: i32) -> bool {
        let target = self.index as isize + delta as isize;
        let count = self.shown_count() as isize;
        if target < 0 {
            self.sibling_left.is_some()
        } else if target >= count {
            self.sibling_right.is_some()
        } else {
            true
        }
    }

    /// Navigates by `delta` stories within the current source or into adjacent sibling sources
    /// (`Controller::subjumpFor` in `media_stories_controller.cpp:1373-1394`).
    ///
    /// Edge cases faithfully ported from C++:
    /// - `delta > 0` marks the current story as read immediately.
    /// - Moving before index 0 jumps to `sibling_left` if present; otherwise restarts at index 0
    ///   (returning `true` so pressing Prev on the first story replays it!).
    /// - Moving past the last story jumps to `sibling_right` if present; otherwise returns `false`.
    pub fn subjump_for(
        &mut self,
        store: &mut StoriesStore,
        delta: i32,
        cfg: ViewerConfig,
        now_secs: TimeId,
        now_ms: i64,
    ) -> bool {
        if delta > 0 {
            self.mark_as_read(store, now_secs, now_ms);
        }
        let count = self.shown_count();
        let target = self.index as isize + delta as isize;
        if target < 0 {
            if self.sibling_left.is_some() {
                return self.jump_for(store, -1, cfg, now_secs, now_ms);
            } else if self.shown.is_none() || count == 0 {
                return false;
            }
            self.subjump_to(store, 0, cfg, now_secs, now_ms);
            return true;
        } else if target >= count as isize {
            return self.sibling_right.is_some() && self.jump_for(store, 1, cfg, now_secs, now_ms);
        } else {
            self.subjump_to(store, target as usize, cfg, now_secs, now_ms);
        }
        true
    }

    fn subjump_to(
        &mut self,
        store: &mut StoriesStore,
        target_index: usize,
        cfg: ViewerConfig,
        now_secs: TimeId,
        now_ms: i64,
    ) {
        let Some(shown) = self.shown else {
            return;
        };
        let story_id = self.story_id_at(target_index);
        if story_id == 0 {
            let delta = target_index as i32 - self.index as i32;
            if self.waiting_for_delta != delta {
                self.waiting_for_delta = delta;
                self.waiting_for_id = None;
                if let StoriesContext::Album(album_id) = self.context {
                    store.album_ids_load_more(shown.peer, album_id);
                }
            }
            return;
        }
        let target_full = FullStoryId::new(shown.peer, story_id);
        if store.lookup(target_full).is_ok() {
            self.show(store, target_full, self.context, cfg, now_secs, now_ms);
        } else if self.waiting_for_id != Some(target_full) {
            self.waiting_for_id = Some(target_full);
            self.waiting_for_delta = 0;
            store.load_around(target_full, self.context);
        }
    }

    /// Jumps directly to the left (`delta == -1`) or right (`delta == 1`) sibling source
    /// (`Controller::jumpFor` in `media_stories_controller.cpp:1441-1463`).
    pub fn jump_for(
        &mut self,
        store: &mut StoriesStore,
        delta: i32,
        cfg: ViewerConfig,
        now_secs: TimeId,
        now_ms: i64,
    ) -> bool {
        if delta == -1 {
            if let Some(left) = self.sibling_left.clone() {
                let target = left.full_id();
                if store.lookup(target).is_ok() {
                    return self.show(store, target, self.context, cfg, now_secs, now_ms);
                }
                self.waiting_for_id = Some(target);
                store.resolve(target, false);
                store.flush_resolve_requests();
                return true;
            }
        } else if delta == 1 {
            if self.shown.is_some() && self.index + 1 >= self.shown_count() {
                self.mark_as_read(store, now_secs, now_ms);
            }
            if let Some(right) = self.sibling_right.clone() {
                let target = right.full_id();
                if store.lookup(target).is_ok() {
                    return self.show(store, target, self.context, cfg, now_secs, now_ms);
                }
                self.waiting_for_id = Some(target);
                store.resolve(target, false);
                store.flush_resolve_requests();
                return true;
            }
        }
        false
    }

    /// Moves away from the currently shown story (tries next, then prev) (`Controller::moveFromShown`).
    pub fn move_from_shown(
        &mut self,
        store: &mut StoriesStore,
        cfg: ViewerConfig,
        now_secs: TimeId,
        now_ms: i64,
    ) -> bool {
        if !self.subjump_for(store, 1, cfg, now_secs, now_ms) {
            self.subjump_for(store, -1, cfg, now_secs, now_ms)
        } else {
            true
        }
    }

    /// Call when `StoriesEvent::ItemsChanged(peer_id)` arrives to complete pending jumps.
    pub fn on_items_changed(
        &mut self,
        store: &mut StoriesStore,
        peer_id: PeerId,
        cfg: ViewerConfig,
        now_secs: TimeId,
        now_ms: i64,
    ) {
        let Some(waiting) = self.waiting_for_id else {
            return;
        };
        if waiting.peer != peer_id {
            return;
        }
        match store.lookup(waiting) {
            Ok(_) => {
                self.waiting_for_id = None;
                self.show(store, waiting, self.context, cfg, now_secs, now_ms);
            }
            Err(NoStory::Deleted) => {
                self.waiting_for_id = None;
            }
            Err(NoStory::Unknown) => {}
        }
    }

    /// Call when `StoriesEvent::SourceChanged(peer_id)` arrives.
    pub fn on_source_changed(&mut self, store: &mut StoriesStore, peer_id: PeerId) {
        if let Some(shown) = self.shown
            && shown.peer == peer_id
        {
            self.rebuild_from_context(store, shown);
        }
    }

    /// Call when `StoriesEvent::AlbumIdsChanged(key)` arrives.
    pub fn on_album_ids_changed(
        &mut self,
        store: &mut StoriesStore,
        peer_id: PeerId,
        album_id: i32,
        cfg: ViewerConfig,
        now_secs: TimeId,
        now_ms: i64,
    ) {
        if let Some(shown) = self.shown
            && shown.peer == peer_id
            && self.context == StoriesContext::Album(album_id)
        {
            self.rebuild_from_context(store, shown);
            if self.waiting_for_delta != 0 {
                let target = self.index as isize + self.waiting_for_delta as isize;
                if target >= 0 && (target as usize) < self.shown_count() {
                    self.waiting_for_delta = 0;
                    self.subjump_to(store, target as usize, cfg, now_secs, now_ms);
                }
            }
        }
    }

    /// Call when `StoriesEvent::StoryUpdated` with `DESTROYED` arrives.
    pub fn on_story_destroyed(&mut self, id: FullStoryId) {
        if self.shown == Some(id) {
            self.events.push(ViewerEvent::Close);
        }
    }

    // -----------------------------------------------------------------------
    // Views, Actions, Stealth Mode, Areas
    // -----------------------------------------------------------------------

    /// Loads or returns the views / reactions list for the current story (`Controller::views`).
    pub fn load_views(
        &mut self,
        store: &mut StoriesStore,
        limit: usize,
        initial: bool,
        can_view_channel_reactions: bool,
    ) -> &StoryViews {
        if let Some(shown) = self.shown {
            let peer_ctx = store.peer_meta(shown.peer).ctx;
            if initial {
                if let Ok(story) = store.lookup(shown)
                    && (peer_ctx.is_self || can_view_channel_reactions)
                {
                    self.views_slice = if peer_ctx.is_channel {
                        story.channel_reactions.clone()
                    } else {
                        story.views.clone()
                    };
                } else {
                    self.views_slice = StoryViews::default();
                }
            }
            if (self.views_slice.total as usize) > self.views_slice.list.len()
                && self.views_slice.list.len() < limit
            {
                store.load_views_slice(
                    shown.peer,
                    shown.story,
                    self.views_slice.next_offset.clone(),
                );
            }
        }
        &self.views_slice
    }

    /// Refreshes the cached views slice after a `GetStoryViewsList` / `GetStoryReactionsList` response.
    pub fn on_views_loaded(&mut self, store: &StoriesStore) {
        if let Some(shown) = self.shown
            && let Ok(story) = store.lookup(shown)
        {
            let peer_ctx = store.peer_meta(shown.peer).ctx;
            self.views_slice = if peer_ctx.is_channel {
                story.channel_reactions.clone()
            } else {
                story.views.clone()
            };
            self.events.push(ViewerEvent::MoreViewsLoaded);
        }
    }

    /// Computes share capability and emits `ViewerEvent::ShareRequested` (`Controller::shareRequested`).
    pub fn share_requested(&mut self, store: &StoriesStore, now_secs: TimeId) -> ShareCapability {
        let capability = self
            .shown
            .and_then(|id| store.lookup(id).ok())
            .map(|story| {
                let peer_ctx = store.peer_meta(story.peer_id).ctx;
                let can_copy_link = story.has_direct_link(peer_ctx, now_secs);
                let share_just_link = story.media.as_video_stream().is_some();
                if !story.can_share(now_secs) && !share_just_link {
                    ShareCapability::Unavailable
                } else if share_just_link {
                    if can_copy_link {
                        ShareCapability::DirectLinkOnly {
                            story_id: story.full_id(),
                        }
                    } else {
                        ShareCapability::Unavailable
                    }
                } else {
                    ShareCapability::ShareStory {
                        story_id: story.full_id(),
                        can_copy_link,
                    }
                }
            })
            .unwrap_or(ShareCapability::Unavailable);
        self.events.push(ViewerEvent::ShareRequested(capability));
        capability
    }

    /// Confirms deletion of the current story, moving away first (`Controller::deleteRequested`).
    pub fn delete_confirmed(
        &mut self,
        store: &mut StoriesStore,
        cfg: ViewerConfig,
        now_secs: TimeId,
        now_ms: i64,
    ) -> bool {
        let Some(id) = self.shown else {
            return false;
        };
        let peer_ctx = store.peer_meta(id.peer).ctx;
        let Ok(story) = store.lookup(id) else {
            return false;
        };
        if !story.can_delete(peer_ctx) {
            return false;
        }
        self.move_from_shown(store, cfg, now_secs, now_ms);
        store.delete_list(&[id]);
        true
    }

    /// Requests reporting the current story (`Controller::reportRequested`).
    pub fn report_requested(&mut self, store: &StoriesStore) -> bool {
        let Some(id) = self.shown else {
            return false;
        };
        let peer_ctx = store.peer_meta(id.peer).ctx;
        if let Ok(story) = store.lookup(id)
            && story.can_report(peer_ctx)
        {
            self.events.push(ViewerEvent::ReportRequested(id));
            return true;
        }
        false
    }

    /// Toggles whether the current story is saved in profile (`Controller::toggleInProfileRequested`).
    pub fn toggle_in_profile_requested(
        &mut self,
        store: &mut StoriesStore,
        in_profile: bool,
        cfg: ViewerConfig,
        now_secs: TimeId,
        now_ms: i64,
    ) -> bool {
        let Some(id) = self.shown else {
            return false;
        };
        let peer_ctx = store.peer_meta(id.peer).ctx;
        if !peer_ctx.is_self && !peer_ctx.can_edit_stories {
            return false;
        }
        if !in_profile
            && let StoriesContext::Album(album_id) = self.context
            && album_id != STORIES_ALBUM_ID_ARCHIVE
        {
            self.move_from_shown(store, cfg, now_secs, now_ms);
        }
        store.toggle_in_profile_list(&[id], in_profile);
        true
    }

    /// Toggles whether the current story is pinned to top of the profile grid.
    pub fn toggle_pinned_to_top_requested(&mut self, store: &mut StoriesStore, pin: bool) -> bool {
        let Some(id) = self.shown else {
            return false;
        };
        if !store.can_toggle_pinned_list(&[id], pin) {
            return false;
        }
        store.toggle_pinned_list(&[id], pin);
        true
    }

    /// Sends or clears a reaction on the current story.
    pub fn send_reaction(&mut self, store: &mut StoriesStore, reaction: Option<ReactionId>) {
        if let Some(id) = self.shown {
            store.send_reaction(id, reaction);
        }
    }

    /// Toggles the default heart reaction (`❤️`) on the current story (`Controller::toggleLiked`).
    pub fn toggle_liked(&mut self, store: &mut StoriesStore) {
        let Some(id) = self.shown else {
            return;
        };
        let already_liked = store
            .lookup(id)
            .ok()
            .is_some_and(|s| s.sent_reaction.is_some());
        let next = if already_liked {
            None
        } else {
            Some(ReactionId::Emoji("\u{2764}".to_string()))
        };
        store.send_reaction(id, next);
    }

    /// Returns `true` if the Stealth Mode option should be offered (`Controller::allowStealthMode`).
    pub fn allow_stealth_mode(&self, store: &StoriesStore, premium_possible: bool) -> bool {
        let Some(id) = self.shown else {
            return false;
        };
        store.lookup(id).is_ok() && !store.peer_meta(id.peer).ctx.is_self && premium_possible
    }

    /// Evaluates stealth mode activation for the current viewer (`Controller::setupStealthMode`).
    ///
    /// If `ReadyToActivate`, automatically queues `StoriesAction::ActivateStealthMode` on `store`.
    pub fn setup_stealth_mode(
        &mut self,
        store: &mut StoriesStore,
        now_secs: TimeId,
        is_premium: bool,
    ) -> StealthActivationDecision {
        let decision = evaluate_stealth_mode(store.stealth_mode(), now_secs, is_premium);
        if decision == StealthActivationDecision::ReadyToActivate {
            store.activate_stealth_mode();
        }
        self.events
            .push(ViewerEvent::StealthModeRequested(decision));
        decision
    }

    /// Hit-tests interactive media areas on the currently shown story at normalized `(nx, ny)` in `[0, 1]`
    /// (`Controller::lookupAreaHandler` in `media_stories_controller.cpp:1240-1330`).
    pub fn hit_test_area(&self, store: &StoriesStore, nx: f64, ny: f64) -> Option<StoryAreaHit> {
        let id = self.shown?;
        let story = store.lookup(id).ok()?;
        for loc in &story.locations {
            if loc.area.hit_test_normalized(nx, ny) {
                return Some(StoryAreaHit::Location {
                    point: loc.point,
                    title: loc.title.clone(),
                });
            }
        }
        for sr in &story.suggested_reactions {
            if sr.area.hit_test_normalized(nx, ny) {
                return Some(StoryAreaHit::SuggestedReaction {
                    reaction: sr.reaction.clone(),
                });
            }
        }
        for cp in &story.channel_posts {
            if cp.area.hit_test_normalized(nx, ny) {
                return Some(StoryAreaHit::ChannelPost {
                    item_id: cp.item_id,
                });
            }
        }
        for url in &story.url_areas {
            if url.area.hit_test_normalized(nx, ny) {
                return Some(StoryAreaHit::Url {
                    url: url.url.clone(),
                });
            }
        }
        for weather in &story.weather_areas {
            if weather.area.hit_test_normalized(nx, ny) {
                return Some(StoryAreaHit::Weather {
                    millicelsius: weather.millicelsius,
                    emoji: weather.emoji.clone(),
                });
            }
        }
        None
    }

    /// Toggles weather area display unit between Celsius and Fahrenheit (`Controller::toggleWeatherMode`).
    pub fn toggle_weather_unit(&mut self) -> bool {
        self.weather_in_celsius = !self.weather_in_celsius;
        self.weather_in_celsius
    }

    // -----------------------------------------------------------------------
    // Internal Context Rebuilding, Siblings, Preloading
    // -----------------------------------------------------------------------

    fn rebuild_from_context(&mut self, store: &mut StoriesStore, story_id: FullStoryId) {
        let peer_id = story_id.peer;
        let id = story_id.story;
        let mut album_list: Option<StoriesList> = None;
        let mut source_opt: Option<StoriesSource> = None;

        match self.context {
            StoriesContext::Single => {
                self.sibling_left = None;
                self.sibling_right = None;
            }
            StoriesContext::Peer => {
                source_opt = store.source(peer_id).cloned();
                self.sibling_left = None;
                self.sibling_right = None;
            }
            StoriesContext::Album(album_id) => {
                if store.album_ids_count_known(peer_id, album_id) {
                    let ids = store.album_ids(peer_id, album_id);
                    let sorted = respecting_pinned(&ids);
                    if let Some(pos) = sorted.iter().position(|&sid| sid == id) {
                        let till_end = sorted.len() - pos;
                        self.index = pos;
                        let total = store.album_ids_count(peer_id, album_id);
                        if ids.list.len() < total && till_end < PRELOAD_STORIES_COUNT {
                            store.album_ids_load_more(peer_id, album_id);
                        }
                        album_list = Some(StoriesList {
                            peer_id,
                            ids,
                            sorted,
                            total,
                        });
                    }
                }
                self.sibling_left = None;
                self.sibling_right = None;
            }
            StoriesContext::SourcesList(list_kind) => {
                source_opt = store.source(peer_id).cloned();
                let sources: Vec<StoriesSourceInfo> = store.sources(list_kind).to_vec();
                if let Some(pos) = sources.iter().position(|s| s.id == peer_id) {
                    if self.cached_sources.is_empty() {
                        self.showing_unread_sources =
                            source_opt.as_ref().is_some_and(|s| s.read_till < id);
                    }
                    self.rebuild_cached_sources_list(&sources, pos);
                    if let Some(ci) = self.cached_source_index
                        && let Some(slot) = self.cached_sources.get_mut(ci)
                    {
                        slot.shown_id = id;
                    }
                    self.refresh_siblings(store);
                    if sources.len() - pos < PRELOAD_PEERS_COUNT {
                        store.load_more(list_kind);
                    }
                }
            }
        }

        self.slider_index = 0;
        self.slider_count = 0;
        if let Some(list) = album_list {
            self.source = None;
            if let Ok(story) = store.lookup(story_id) {
                let range = compute_same_day_range(
                    store,
                    peer_id,
                    story.date,
                    &list.ids,
                    &list.sorted,
                    self.index,
                );
                self.slider_count = range.till.saturating_sub(range.from) + 1;
                self.slider_index = self.index.saturating_sub(range.from);
            }
            self.list = Some(list);
        } else {
            if let Some(src) = &source_opt {
                if let Some(pos) = src.index_of(id) {
                    self.index = pos;
                } else {
                    source_opt = None;
                }
            }
            if let Some(src) = source_opt {
                self.list = None;
                self.source = Some(src);
            } else {
                self.source = None;
                self.list = Some(StoriesList {
                    peer_id,
                    ids: StoriesIds {
                        list: vec![id],
                        pinned_to_top: Vec::new(),
                    },
                    sorted: vec![id],
                    total: 1,
                });
                self.index = 0;
            }
        }

        self.preload_next(store, peer_id);
    }

    fn rebuild_cached_sources_list(&mut self, lists: &[StoriesSourceInfo], index: usize) {
        let current_peer_id = lists[index].id;

        // Remove sources that disappeared.
        self.cached_sources
            .retain(|c| lists.iter().any(|info| info.id == c.peer_id));

        if let Some(pos) = self
            .cached_sources
            .iter()
            .position(|c| c.peer_id == current_peer_id)
        {
            self.cached_source_index = Some(pos);
        } else {
            self.cached_sources.clear();
            self.cached_source_index = None;
        }

        if self.cached_sources.is_empty() {
            self.cached_sources = lists
                .iter()
                .filter(|info| {
                    !self.showing_unread_sources
                        || info.unread_count > 0
                        || info.id == current_peer_id
                })
                .map(|info| CachedSource {
                    peer_id: info.id,
                    shown_id: 0,
                })
                .collect();
            self.cached_source_index = self
                .cached_sources
                .iter()
                .position(|c| c.peer_id == current_peer_id);
        } else {
            let equal = lists.len() == self.cached_sources.len()
                && lists
                    .iter()
                    .zip(self.cached_sources.iter())
                    .all(|(a, b)| a.id == b.peer_id);
            if !equal {
                // Push newly discovered peers before current to front.
                for before in (0..index).rev() {
                    let info = lists[before];
                    if self.showing_unread_sources && info.unread_count == 0 {
                        continue;
                    }
                    if !self.cached_sources.iter().any(|c| c.peer_id == info.id) {
                        self.cached_sources.insert(
                            0,
                            CachedSource {
                                peer_id: info.id,
                                shown_id: 0,
                            },
                        );
                        if let Some(ci) = &mut self.cached_source_index {
                            *ci += 1;
                        }
                    }
                }
                // Push newly discovered peers after current to back.
                for info in &lists[(index + 1)..] {
                    if self.showing_unread_sources && info.unread_count == 0 {
                        continue;
                    }
                    if !self.cached_sources.iter().any(|c| c.peer_id == info.id) {
                        self.cached_sources.push(CachedSource {
                            peer_id: info.id,
                            shown_id: 0,
                        });
                    }
                }
            }
        }
    }

    fn refresh_siblings(&mut self, store: &StoriesStore) {
        let Some(ci) = self.cached_source_index else {
            self.sibling_left = None;
            self.sibling_right = None;
            return;
        };
        let self_id = store.self_peer_id();
        self.sibling_left = if ci > 0 {
            let c = self.cached_sources[ci - 1];
            store
                .source(c.peer_id)
                .and_then(|s| SiblingPreview::from_source(s, c.shown_id, c.peer_id == self_id))
        } else {
            None
        };
        self.sibling_right = if ci + 1 < self.cached_sources.len() {
            let c = self.cached_sources[ci + 1];
            store
                .source(c.peer_id)
                .and_then(|s| SiblingPreview::from_source(s, c.shown_id, c.peer_id == self_id))
        } else {
            None
        };
    }

    fn preload_next(&self, store: &mut StoriesStore, peer_id: PeerId) {
        let count = self.shown_count();
        let mut ids = Vec::with_capacity(PRELOAD_NEXT_MEDIA_COUNT + PRELOAD_PREVIOUS_MEDIA_COUNT);
        let till = (self.index + PRELOAD_NEXT_MEDIA_COUNT).min(count);
        for i in (self.index + 1)..till {
            let sid = self.story_id_at(i);
            if sid != 0 {
                ids.push(FullStoryId::new(peer_id, sid));
            }
        }
        let from = self.index.saturating_sub(PRELOAD_PREVIOUS_MEDIA_COUNT);
        for i in (from..self.index).rev() {
            let sid = self.story_id_at(i);
            if sid != 0 {
                ids.push(FullStoryId::new(peer_id, sid));
            }
        }
        store.set_preloading_in_viewer(ids);
    }

    fn refresh_recent_views(
        &mut self,
        story: &Story,
        peer_ctx: StoryPeerContext,
        can_view_channel_reactions: bool,
    ) {
        let kind = recent_views_type_for(peer_ctx, self.video_stream);
        self.recent_views = RecentViewsState {
            kind: Some(kind),
            recent_viewers: story.recent_viewers.clone(),
            views: story.views.views,
            forwards: story.views.forwards,
            reactions: story.views.reactions,
            total: story.views.total,
            can_view_reactions: !self.video_stream
                && can_view_channel_reactions
                && !peer_ctx.is_megagroup,
        };
    }

    /// Returns a privacy badge classification for the current story header.
    pub fn privacy_badge(privacy: StoryPrivacy) -> Option<StoryPrivacy> {
        match privacy {
            StoryPrivacy::CloseFriends
            | StoryPrivacy::Contacts
            | StoryPrivacy::SelectedContacts => Some(privacy),
            StoryPrivacy::Public | StoryPrivacy::Other => None,
        }
    }
}
