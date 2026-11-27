//! Decoupled backend trait for Telegram Stories UI.
//!
//! Port origin: `Telegram/SourceFiles/data/data_stories.h`,
//! `Telegram/SourceFiles/media/stories/media_stories_delegate.h`, and
//! `Telegram/SourceFiles/media/stories/media_stories_share.cpp`.

use std::path::PathBuf;
use std::sync::Arc;

use tg_data::{FullStoryId, PeerId, StoryId, TimeId};
use tg_stories::{
    ChannelPost, StoriesAction, StoriesStore, StoryLocation, StoryPrivacy, StoryUploadStatus,
    SuggestedReaction, UrlArea, WeatherArea,
};
use tg_text::TextWithEntities as UiText;
use tg_video::streaming::RangeLoader;

/// Default story lifetime period in seconds (`86400` = 24 hours).
pub const DEFAULT_STORY_PERIOD_SECS: i32 = 86_400;

/// Supported story expiry periods in seconds (`6h`, `12h`, `24h`, `48h`), matching tdesktop.
pub const STORY_PERIOD_OPTIONS: &[i32] = &[21_600, 43_200, 86_400, 172_800];

/// Media payload for uploading and publishing or editing a Telegram Story
/// (`stories.sendStory` / `stories.editStory`).
#[derive(Debug, Clone, PartialEq)]
pub enum StoryUploadMedia {
    /// Static photo story (`inputMediaUploadedPhoto`).
    Photo {
        /// Encoded JPEG/PNG/WebP image bytes.
        bytes: Vec<u8>,
        /// File name (`.jpg` appended if missing).
        file_name: String,
    },
    /// Video story (`inputMediaUploadedDocument` with `documentAttributeVideo`).
    Video {
        /// Encoded MP4 video bytes.
        bytes: Vec<u8>,
        /// File name (defaults to `"story.mp4"` if empty).
        file_name: String,
        /// Video width in pixels.
        width: i32,
        /// Video height in pixels.
        height: i32,
        /// Video duration in seconds.
        duration_secs: f64,
        /// Whether the video has an audio track (`false` sets `nosound_video` and `nosound`).
        has_audio: bool,
        /// Optional JPEG thumbnail bytes.
        thumb_bytes: Option<Vec<u8>>,
    },
}

impl StoryUploadMedia {
    /// Total byte size to upload (including optional video thumbnail).
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        match self {
            Self::Photo { bytes, .. } => bytes.len() as u64,
            Self::Video {
                bytes, thumb_bytes, ..
            } => (bytes.len() + thumb_bytes.as_ref().map_or(0, Vec::len)) as u64,
        }
    }

    /// Returns `true` if this media payload is a video.
    #[must_use]
    pub const fn is_video(&self) -> bool {
        matches!(self, Self::Video { .. })
    }

    /// Returns the display file name for this media payload.
    #[must_use]
    pub fn file_name(&self) -> &str {
        match self {
            Self::Photo { file_name, .. } | Self::Video { file_name, .. } => file_name.as_str(),
        }
    }
}

/// Audience privacy rules for a newly uploaded or edited story
/// (`StoryPrivacyToInput` in `api_stories.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryPrivacyInput {
    /// Base visibility category (`Public`, `Contacts`, `CloseFriends`, `SelectedContacts`).
    pub privacy: StoryPrivacy,
    /// Explicitly allowed peers (used when `privacy == SelectedContacts` / `Other`).
    pub allow_peers: Vec<PeerId>,
    /// Explicitly disallowed peers (used when `privacy == Public` or `Contacts`).
    pub disallow_peers: Vec<PeerId>,
}

impl Default for StoryPrivacyInput {
    fn default() -> Self {
        Self {
            privacy: StoryPrivacy::Public,
            allow_peers: Vec::new(),
            disallow_peers: Vec::new(),
        }
    }
}

/// Collection of interactive media areas attached to a story (`MediaAreasToMTP`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StoryMediaAreasInput {
    /// Interactive location/venue media areas.
    pub locations: Vec<StoryLocation>,
    /// Interactive suggested reaction media areas.
    pub suggested_reactions: Vec<SuggestedReaction>,
    /// Interactive reposted channel post media areas.
    pub channel_posts: Vec<ChannelPost>,
    /// Interactive URL media areas.
    pub url_areas: Vec<UrlArea>,
    /// Interactive weather widget media areas.
    pub weather_areas: Vec<WeatherArea>,
}

impl StoryMediaAreasInput {
    /// Total number of attached interactive media areas.
    #[must_use]
    pub fn len(&self) -> usize {
        self.locations.len()
            + self.suggested_reactions.len()
            + self.channel_posts.len()
            + self.url_areas.len()
            + self.weather_areas.len()
    }

    /// Returns `true` if no media areas are attached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Complete request for uploading media and invoking `stories.sendStory`.
#[derive(Debug, Clone, PartialEq)]
pub struct SendStoryRequest {
    /// Target peer (`self_peer_id` or a channel the account can post stories to).
    pub peer_id: PeerId,
    /// Photo or video media payload to upload.
    pub media: StoryUploadMedia,
    /// Formatted caption with UTF-16 entities.
    pub caption: UiText,
    /// Audience privacy rules.
    pub privacy: StoryPrivacyInput,
    /// Optional expiry period in seconds (`21600`, `43200`, `86400`, `172800`; defaults to `86400`).
    pub period: Option<i32>,
    /// Whether the story is pinned/saved to the peer's profile (`pinned` flag).
    pub pinned: bool,
    /// Whether forwarding/saving the story is forbidden (`noforwards` flag).
    pub noforwards: bool,
    /// Optional custom story album IDs (`albums`).
    pub albums: Vec<i32>,
    /// Interactive location/venue media areas.
    pub locations: Vec<StoryLocation>,
    /// Interactive suggested reaction media areas.
    pub suggested_reactions: Vec<SuggestedReaction>,
    /// Interactive reposted channel post media areas.
    pub channel_posts: Vec<ChannelPost>,
    /// Interactive URL media areas.
    pub url_areas: Vec<UrlArea>,
    /// Interactive weather widget media areas.
    pub weather_areas: Vec<WeatherArea>,
}

/// Request for editing an existing story via `stories.editStory`.
#[derive(Debug, Clone, PartialEq)]
pub struct EditStoryRequest {
    /// Peer owning the story (`self_peer_id` or managed channel).
    pub peer_id: PeerId,
    /// Identifier of the story to edit.
    pub story_id: StoryId,
    /// Optional replacement photo or video media (`flags.0`).
    pub media: Option<StoryUploadMedia>,
    /// Optional replacement interactive media areas (`flags.3`).
    pub media_areas: Option<StoryMediaAreasInput>,
    /// Optional replacement caption + UTF-16 entities (`flags.1`).
    pub caption: Option<UiText>,
    /// Optional replacement privacy rules (`flags.2`).
    pub privacy: Option<StoryPrivacyInput>,
}

/// Local unsent story draft persisted in memory for [`crate::stories::compose::StoryDraftsBox`].
#[derive(Debug, Clone, PartialEq)]
pub struct StoryDraft {
    /// Unique local draft identifier.
    pub id: u64,
    /// Target peer ID (`self_peer_id` or channel).
    pub peer_id: PeerId,
    /// Unix timestamp (seconds) when the draft was saved.
    pub created_at: TimeId,
    /// Optional attached photo/video media.
    pub media: Option<StoryUploadMedia>,
    /// Draft caption with entities.
    pub caption: UiText,
    /// Draft privacy selection and exceptions.
    pub privacy: StoryPrivacyInput,
    /// Selected story lifetime in seconds.
    pub period: i32,
    /// Whether "Keep on profile" is toggled on.
    pub pinned: bool,
    /// Whether forwarding/screenshots are disabled.
    pub noforwards: bool,
    /// Attached interactive media areas.
    pub areas: StoryMediaAreasInput,
}

/// Visual peer profile metadata rendered in the stories strip, viewer header,
/// sibling previews, and own-story views list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerProfile {
    /// Peer identifier.
    pub peer_id: PeerId,
    /// Full display name (e.g. `"Aurora Studio"`).
    pub name: String,
    /// Short name used beneath strip avatars and in tooltips (e.g. `"Aurora"`).
    pub short_name: String,
    /// Optional public username without `@` (used for `t.me/<username>/s/<id>` links).
    pub username: String,
    /// 1-2 character avatar initials.
    pub initials: String,
    /// Top gradient RGB color for generated avatar circles.
    pub avatar_top_rgb: [u8; 3],
    /// Bottom gradient RGB color for generated avatar circles.
    pub avatar_bottom_rgb: [u8; 3],
    /// Whether this peer is the current logged-in account.
    pub is_self: bool,
    /// Whether this peer is a verified account/channel.
    pub is_verified: bool,
    /// Whether this peer has Telegram Premium.
    pub is_premium: bool,
    /// Whether this peer is a broadcast channel.
    pub is_broadcast: bool,
    /// Whether this peer is a megagroup/supergroup.
    pub is_megagroup: bool,
    /// Whether the current user can inspect channel reactions list.
    pub can_view_channel_reactions: bool,
    /// Whether the target user requires Premium to initiate private replies.
    pub requires_premium_to_write: bool,
    /// Paid Stars required per reply message (`0` if free).
    pub stars_per_message: i32,
}

impl Default for PeerProfile {
    fn default() -> Self {
        Self {
            peer_id: PeerId::ZERO,
            name: "Unknown".to_owned(),
            short_name: "Unknown".to_owned(),
            username: String::new(),
            initials: "?".to_owned(),
            avatar_top_rgb: [0x5B, 0xA4, 0xE6],
            avatar_bottom_rgb: [0x3B, 0x82, 0xC4],
            is_self: false,
            is_verified: false,
            is_premium: false,
            is_broadcast: false,
            is_megagroup: false,
            can_view_channel_reactions: false,
            requires_premium_to_write: false,
            stars_per_message: 0,
        }
    }
}

/// Decodable story media payload returned by [`StoriesBackend::load_story_media`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoryMediaAsset {
    /// Encoded PNG/JPEG/WebP photo bytes decoded through `tg-media`.
    PhotoBytes(Vec<u8>),
    /// Encoded MP4/WebM video bytes played through `tg-video::StreamingPlayer`.
    VideoBytes(Vec<u8>),
    /// Local MP4/WebM video file path played through `tg-video::StreamingPlayer`.
    VideoPath(PathBuf),
    /// A video streamed through a range loader (tdesktop streams story video like any
    /// document): playback starts once the first parts arrive.
    VideoStream(StoryVideoStream),
}

/// Shared range loader of a streamed video story. Two values are equal when they share the
/// same loader.
#[derive(Clone)]
pub struct StoryVideoStream(Arc<dyn RangeLoader>);

impl StoryVideoStream {
    /// Wraps `loader`.
    #[must_use]
    pub fn new(loader: Arc<dyn RangeLoader>) -> Self {
        Self(loader)
    }

    /// The loader the player reads from.
    #[must_use]
    pub fn loader(&self) -> Arc<dyn RangeLoader> {
        Arc::clone(&self.0)
    }
}

impl std::fmt::Debug for StoryVideoStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoryVideoStream")
            .field("size", &self.0.size())
            .finish_non_exhaustive()
    }
}

impl PartialEq for StoryVideoStream {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for StoryVideoStream {}

/// Share destination row rendered in the Share Story dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareTarget {
    /// Destination peer ID.
    pub peer_id: PeerId,
    /// Display name.
    pub name: String,
    /// Short name.
    pub short_name: String,
    /// Avatar initials.
    pub initials: String,
    /// Avatar RGB color.
    pub avatar_rgb: [u8; 3],
    /// Whether sharing a story to this chat is allowed (`CanSend(SendPhotos|SendVideos)`).
    pub can_send: bool,
}

/// Record of a private reply / comment sent from the story viewer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentStoryReply {
    /// Target story ID.
    pub story_id: FullStoryId,
    /// Reply body text.
    pub text: String,
    /// Unix timestamp (seconds) when sent.
    pub sent_at: TimeId,
}

/// Record of a story share/forward action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedStoryRecord {
    /// Shared story ID.
    pub story_id: FullStoryId,
    /// Destination peer IDs.
    pub targets: Vec<PeerId>,
    /// Optional accompanying comment.
    pub comment: String,
}

/// Record of a story report action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportedStoryRecord {
    /// Reported story ID.
    pub story_id: FullStoryId,
    /// Selected reason label.
    pub reason: String,
}

/// Backend contract decoupling the stories UI from MTProto / `tg-api` transport.
///
/// The UI drives [`StoriesStore`] and [`tg_stories::StoryViewerController`], then drains
/// [`StoriesAction`] values and hands them to [`StoriesBackend::execute_actions`].
pub trait StoriesBackend {
    /// Returns the signed-in account's `PeerId`.
    fn self_peer_id(&self) -> PeerId;

    /// Returns whether the signed-in account currently has Telegram Premium.
    fn account_premium(&self) -> bool;

    /// Updates Premium status (useful in tests / mock toggles for Stealth Mode).
    fn set_account_premium(&mut self, premium: bool);

    /// Returns display metadata for `peer_id`.
    fn peer_profile(&self, peer_id: PeerId) -> PeerProfile;

    /// Returns available share targets for the Share Story dialog.
    fn share_targets(&self) -> Vec<ShareTarget>;

    /// Executes drained [`StoriesAction`] items from `store`, feeding responses back into `store`.
    fn execute_actions(
        &mut self,
        store: &mut StoriesStore,
        actions: Vec<StoriesAction>,
        now_secs: TimeId,
    );

    /// Loads or generates the decodable media asset for `story_id`.
    fn load_story_media(&mut self, story_id: FullStoryId) -> Option<StoryMediaAsset>;

    /// Sends a private reply or comment on `story_id`.
    fn send_story_reply(
        &mut self,
        story_id: FullStoryId,
        text: &str,
        now_secs: TimeId,
    ) -> Result<(), String>;

    /// Shares `story_id` to `targets` with an optional `comment`.
    fn share_story(
        &mut self,
        story_id: FullStoryId,
        targets: &[PeerId],
        comment: &str,
    ) -> Result<(), String>;

    /// Exports the public `t.me/<username>/s/<id>` link if available.
    fn export_story_link(&self, story_id: FullStoryId) -> Option<String>;

    /// Reports `story_id` with `reason`.
    fn report_story(&mut self, story_id: FullStoryId, reason: &str) -> Result<(), String>;

    /// Toggles whether `peer_id` is blocked from viewing our own stories.
    fn set_viewer_stories_blocked(&mut self, peer_id: PeerId, blocked: bool);

    /// Returns whether `peer_id` is currently blocked from viewing our own stories.
    fn is_viewer_stories_blocked(&self, peer_id: PeerId) -> bool;

    /// Returns any active outgoing story uploads to merge into the top strip.
    fn pending_uploads(&self) -> Vec<StoryUploadStatus>;

    /// Returns `true` when offline mock Premium toggling is permitted inside `StealthModeBox`.
    /// Live sessions return `false` so mock Premium controls stay hidden in live mode.
    fn supports_mock_premium_toggle(&self) -> bool {
        true
    }

    /// Uploads media and publishes a new story (`stories.sendStory`).
    fn send_story(
        &mut self,
        store: Option<&mut StoriesStore>,
        request: SendStoryRequest,
        now_secs: TimeId,
    ) -> Result<StoryId, String>;

    /// Edits an existing story (`stories.editStory`).
    fn edit_story(
        &mut self,
        store: Option<&mut StoriesStore>,
        request: EditStoryRequest,
        now_secs: TimeId,
    ) -> Result<(), String>;

    /// Dismisses any failed outgoing story upload indicators for `peer_id`.
    fn dismiss_failed_uploads(&mut self, _peer_id: PeerId) {}

    /// Returns the list of user peer IDs currently marked as Close Friends.
    fn close_friends(&self) -> Vec<PeerId> {
        Vec::new()
    }

    /// Updates the Close Friends list (`contacts.editCloseFriends`).
    fn set_close_friends(&mut self, _peers: Vec<PeerId>) -> Result<(), String> {
        Ok(())
    }

    /// Returns selectable contact/peer profiles for story privacy exception pickers.
    fn contacts_for_privacy(&self) -> Vec<PeerProfile> {
        let self_id = self.self_peer_id();
        self.share_targets()
            .into_iter()
            .filter(|t| t.peer_id != self_id)
            .map(|t| self.peer_profile(t.peer_id))
            .collect()
    }

    /// Bulk-replaces the hidden-stories blocklist (`contacts.setBlocked` with
    /// `my_stories_from = true`, `api/api_stories.cpp:595-621`).
    fn save_stories_blocklist(&mut self, _peers: &[PeerId]) -> Result<bool, String> {
        Err("Saving stories blocklist is not supported by this backend.".to_owned())
    }
}
