//! In-memory mock implementation of [`StoriesBackend`].
//!
//! Executes `tg-stories` [`StoriesAction`] values against rich fictional sample data
//! and procedurally generated runtime PNG gradients (plus optional FFmpeg-generated MP4 videos
//! in tests/examples). Contains zero real people's names and never touches the network.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Cursor;
use std::path::PathBuf;

use image::{ImageFormat, Rgba, RgbaImage};
use tg_data::peer::GeoPoint;
use tg_data::{
    DocumentData, DocumentId, DocumentKind, FullMsgId, FullStoryId, MsgId, PeerId, PhotoData,
    PhotoId, ReactionId, StoryId, TextWithEntities, TimeId,
};
use tg_stories::{
    ParsedAllStories, ParsedPeerStories, ParsedStoriesPage, ParsedStoryItem, PeerStoryMeta,
    RECENT_VIEWERS_MAX, STORIES_ALBUM_ID_ARCHIVE, STORIES_ALBUM_ID_SAVED, StealthMode,
    StoriesAction, StoriesStore, Story, StoryArea, StoryLocation, StoryMedia, StoryPeerContext,
    StoryPrivacy, StoryRepostInfo, StorySourcesList, StoryUploadStatus, StoryVideoStream,
    StoryView, StoryViews, StoryViewsCounts, SuggestedReaction, UrlArea, WeatherArea,
};

use crate::stories::backend::{
    DEFAULT_STORY_PERIOD_SECS, EditStoryRequest, PeerProfile, ReportedStoryRecord,
    SendStoryRequest, SentStoryReply, ShareTarget, SharedStoryRecord, StoriesBackend,
    StoryMediaAsset, StoryUploadMedia,
};

/// Well-known fictional peer IDs used in [`MockStoriesBackend`].
pub mod mock_peers {
    use tg_data::{ChannelId, PeerId, UserId};

    /// Current logged-in user (`"You"`).
    pub const SELF: PeerId = PeerId::from_user(UserId(1001));
    /// Unread peer #1 (`"Aurora Studio"`, Close Friends + interactive areas).
    pub const AURORA: PeerId = PeerId::from_user(UserId(2001));
    /// Unread peer #2 (`"Cedar Labs"`, video story + long expandable caption).
    pub const CEDAR: PeerId = PeerId::from_user(UserId(2002));
    /// Unread broadcast channel (`"Design Journal"`, channel views + reactions).
    pub const DESIGN_JOURNAL: PeerId = PeerId::from_channel(ChannelId(3001));
    /// Live video stream peer (`"Harbor Radio"`).
    pub const HARBOR_RADIO: PeerId = PeerId::from_channel(ChannelId(3002));
    /// Unread peer #3 (`"Kestrel Dev"`, repost story).
    pub const KESTREL: PeerId = PeerId::from_user(UserId(2003));
    /// Already-read peer (`"Northwind Notes"`).
    pub const NORTHWIND: PeerId = PeerId::from_user(UserId(2004));
    /// Hidden/archived peer (`"Solstice Archive"`).
    pub const SOLSTICE: PeerId = PeerId::from_user(UserId(2005));
    /// Additional mock viewer peers for own-story stats/views list.
    pub const VIEWER_ORBIT: PeerId = PeerId::from_user(UserId(2010));
    /// Additional mock viewer peer.
    pub const VIEWER_MEADOW: PeerId = PeerId::from_user(UserId(2011));
    /// Additional mock viewer peer.
    pub const VIEWER_SUMMIT: PeerId = PeerId::from_user(UserId(2012));
}

/// Procedural multi-stop gradient preset for generating runtime PNG story media.
#[derive(Clone, Copy, Debug)]
pub struct GradientPreset {
    /// Top gradient RGB color.
    pub top: [u8; 3],
    /// Middle gradient RGB color.
    pub mid: [u8; 3],
    /// Bottom gradient RGB color.
    pub bottom: [u8; 3],
    /// Luminous bokeh accent RGB color.
    pub accent: [u8; 3],
    /// Primary orb normalized X coordinate.
    pub orb_x: f32,
    /// Primary orb normalized Y coordinate.
    pub orb_y: f32,
    /// Whether to add subtle diagonal light rays.
    pub stripes: bool,
}

/// In-memory fake backend executing [`StoriesAction`] against synthetic stories and media.
pub struct MockStoriesBackend {
    self_peer_id: PeerId,
    account_premium: bool,
    base_time: TimeId,
    profiles: HashMap<PeerId, PeerProfile>,
    peer_meta: HashMap<PeerId, PeerStoryMeta>,
    stories: HashMap<PeerId, BTreeMap<StoryId, Story>>,
    read_tills: HashMap<PeerId, StoryId>,
    views_by_story: HashMap<FullStoryId, Vec<StoryView>>,
    media_cache: HashMap<FullStoryId, StoryMediaAsset>,
    gradient_presets: HashMap<FullStoryId, GradientPreset>,
    blocked_viewers: HashSet<PeerId>,
    close_friends: Vec<PeerId>,
    stealth_mode: StealthMode,
    pending_uploads: Vec<StoryUploadStatus>,
    next_story_id: StoryId,
    /// Log of stories published via [`StoriesBackend::send_story`].
    pub sent_stories: Vec<SendStoryRequest>,
    /// Log of stories edited via [`StoriesBackend::edit_story`].
    pub edited_stories: Vec<EditStoryRequest>,
    /// Log of replies sent from the story viewer.
    pub sent_replies: Vec<SentStoryReply>,
    /// Log of share actions performed.
    pub shared_records: Vec<SharedStoryRecord>,
    /// Log of report actions performed.
    pub reported_records: Vec<ReportedStoryRecord>,
    /// Log of bulk stories blocklist updates (`contacts.setBlocked(my_stories_from=true)`).
    pub saved_stories_blocklists: Vec<Vec<PeerId>>,
    /// Count of actions executed (useful for tests).
    pub executed_actions_count: usize,
}

impl MockStoriesBackend {
    /// Creates a populated mock backend anchored at timestamp `now_secs`.
    #[must_use]
    pub fn new(now_secs: TimeId) -> Self {
        let mut backend = Self {
            self_peer_id: mock_peers::SELF,
            account_premium: true,
            base_time: now_secs,
            profiles: HashMap::new(),
            peer_meta: HashMap::new(),
            stories: HashMap::new(),
            read_tills: HashMap::new(),
            views_by_story: HashMap::new(),
            media_cache: HashMap::new(),
            gradient_presets: HashMap::new(),
            blocked_viewers: HashSet::new(),
            close_friends: vec![mock_peers::AURORA, mock_peers::CEDAR],
            stealth_mode: StealthMode::default(),
            pending_uploads: Vec::new(),
            next_story_id: 103,
            sent_stories: Vec::new(),
            edited_stories: Vec::new(),
            sent_replies: Vec::new(),
            shared_records: Vec::new(),
            reported_records: Vec::new(),
            saved_stories_blocklists: Vec::new(),
            executed_actions_count: 0,
        };
        backend.seed_sample_data();
        backend
    }

    /// Attaches a real video file path (e.g., generated via `ffmpeg.exe` in tests/examples)
    /// to a video story so `tg-video::StreamingPlayer` plays it live.
    pub fn set_video_file(&mut self, story_id: FullStoryId, path: PathBuf) {
        self.media_cache
            .insert(story_id, StoryMediaAsset::VideoPath(path));
    }

    /// Attaches raw encoded video bytes to a video story.
    pub fn set_video_bytes(&mut self, story_id: FullStoryId, bytes: Vec<u8>) {
        self.media_cache
            .insert(story_id, StoryMediaAsset::VideoBytes(bytes));
    }

    /// Configures a mock outgoing story upload entry for strip upload-ring testing.
    pub fn set_pending_uploads(&mut self, uploads: Vec<StoryUploadStatus>) {
        self.pending_uploads = uploads;
    }

    /// Registers peer metadata on `store` and triggers initial `GetAllStories` + `GetAllReadPeerStories`.
    pub fn bootstrap_store(&mut self, store: &mut StoriesStore, now_secs: TimeId) {
        for (&peer_id, &meta) in &self.peer_meta {
            store.set_peer_meta(peer_id, meta);
        }
        store.request_read_tills();
        store.load_more(StorySourcesList::NotHidden);
        store.load_more(StorySourcesList::Hidden);
        let actions = store.take_actions();
        self.execute_actions(store, actions, now_secs);
    }

    /// Returns the first unread story ID of `mock_peers::AURORA` (convenient for viewer demos).
    #[must_use]
    pub const fn featured_interactive_story_id() -> FullStoryId {
        FullStoryId::new(mock_peers::AURORA, 201)
    }

    /// Returns the mock video story ID (`mock_peers::CEDAR`, story 301).
    #[must_use]
    pub const fn featured_video_story_id() -> FullStoryId {
        FullStoryId::new(mock_peers::CEDAR, 301)
    }

    /// Returns the mock own story ID with rich viewer stats (`mock_peers::SELF`, story 101).
    #[must_use]
    pub const fn featured_self_story_id() -> FullStoryId {
        FullStoryId::new(mock_peers::SELF, 101)
    }

    fn seed_sample_data(&mut self) {
        let now = self.base_time;
        let expires = now + 22 * 3600;

        // 1. Register fictional peer profiles (no real people's names).
        self.register_peer(
            PeerProfile {
                peer_id: mock_peers::SELF,
                name: "My Story".to_owned(),
                short_name: "My Story".to_owned(),
                username: "rustgram_demo".to_owned(),
                initials: "ME".to_owned(),
                avatar_top_rgb: [0x54, 0xC3, 0xF3],
                avatar_bottom_rgb: [0x2A, 0x8A, 0xD4],
                is_self: true,
                is_premium: true,
                ..PeerProfile::default()
            },
            false,
            true,
        );
        self.register_peer(
            PeerProfile {
                peer_id: mock_peers::AURORA,
                name: "Aurora Studio".to_owned(),
                short_name: "Aurora".to_owned(),
                username: "aurora_studio".to_owned(),
                initials: "AS".to_owned(),
                avatar_top_rgb: [0xFF, 0x8A, 0x65],
                avatar_bottom_rgb: [0xD8, 0x43, 0x15],
                is_verified: true,
                is_premium: true,
                ..PeerProfile::default()
            },
            false,
            true,
        );
        self.register_peer(
            PeerProfile {
                peer_id: mock_peers::CEDAR,
                name: "Cedar Labs".to_owned(),
                short_name: "Cedar".to_owned(),
                username: "cedar_labs".to_owned(),
                initials: "CL".to_owned(),
                avatar_top_rgb: [0x66, 0xBB, 0x6A],
                avatar_bottom_rgb: [0x2E, 0x7D, 0x32],
                is_premium: true,
                ..PeerProfile::default()
            },
            false,
            true,
        );
        self.register_peer(
            PeerProfile {
                peer_id: mock_peers::DESIGN_JOURNAL,
                name: "Design Journal".to_owned(),
                short_name: "Design".to_owned(),
                username: "design_journal".to_owned(),
                initials: "DJ".to_owned(),
                avatar_top_rgb: [0xAB, 0x47, 0xBC],
                avatar_bottom_rgb: [0x6A, 0x1B, 0x9A],
                is_verified: true,
                is_broadcast: true,
                can_view_channel_reactions: true,
                ..PeerProfile::default()
            },
            false,
            false,
        );
        self.register_peer(
            PeerProfile {
                peer_id: mock_peers::HARBOR_RADIO,
                name: "Harbor Radio".to_owned(),
                short_name: "Harbor".to_owned(),
                username: "harbor_radio".to_owned(),
                initials: "HR".to_owned(),
                avatar_top_rgb: [0xEF, 0x53, 0x50],
                avatar_bottom_rgb: [0xB7, 0x1C, 0x1C],
                is_broadcast: true,
                ..PeerProfile::default()
            },
            false,
            false,
        );
        self.register_peer(
            PeerProfile {
                peer_id: mock_peers::KESTREL,
                name: "Kestrel Dev".to_owned(),
                short_name: "Kestrel".to_owned(),
                username: "kestrel_dev".to_owned(),
                initials: "KD".to_owned(),
                avatar_top_rgb: [0x26, 0xC6, 0xDA],
                avatar_bottom_rgb: [0x00, 0x83, 0x8F],
                is_premium: true,
                ..PeerProfile::default()
            },
            false,
            true,
        );
        self.register_peer(
            PeerProfile {
                peer_id: mock_peers::NORTHWIND,
                name: "Northwind Notes".to_owned(),
                short_name: "Northwind".to_owned(),
                username: "northwind_notes".to_owned(),
                initials: "NN".to_owned(),
                avatar_top_rgb: [0x78, 0x90, 0x9C],
                avatar_bottom_rgb: [0x45, 0x5A, 0x64],
                ..PeerProfile::default()
            },
            false,
            true,
        );
        self.register_peer(
            PeerProfile {
                peer_id: mock_peers::SOLSTICE,
                name: "Solstice Archive".to_owned(),
                short_name: "Solstice".to_owned(),
                username: "solstice_arc".to_owned(),
                initials: "SA".to_owned(),
                avatar_top_rgb: [0xFF, 0xCA, 0x28],
                avatar_bottom_rgb: [0xF5, 0x7F, 0x17],
                ..PeerProfile::default()
            },
            true,
            true,
        );
        for (pid, name, short, init, top, bot) in [
            (
                mock_peers::VIEWER_ORBIT,
                "Orbit Collective",
                "Orbit",
                "OC",
                [0x5C, 0x6B, 0xC0],
                [0x28, 0x35, 0x93],
            ),
            (
                mock_peers::VIEWER_MEADOW,
                "Meadow Works",
                "Meadow",
                "MW",
                [0x4C, 0xAF, 0x50],
                [0x1B, 0x5E, 0x20],
            ),
            (
                mock_peers::VIEWER_SUMMIT,
                "Summit Systems",
                "Summit",
                "SS",
                [0xEC, 0x40, 0x7A],
                [0xAD, 0x14, 0x57],
            ),
        ] {
            self.register_peer(
                PeerProfile {
                    peer_id: pid,
                    name: name.to_owned(),
                    short_name: short.to_owned(),
                    username: short.to_lowercase(),
                    initials: init.to_owned(),
                    avatar_top_rgb: top,
                    avatar_bottom_rgb: bot,
                    ..PeerProfile::default()
                },
                false,
                true,
            );
        }

        // 2. Own stories (Self) with rich viewer statistics.
        let self_viewers = vec![
            StoryView {
                peer_id: mock_peers::AURORA,
                reaction: Some(ReactionId::Emoji("\u{2764}".to_owned())),
                repost_id: 0,
                forward_id: MsgId::ZERO,
                date: now - 180,
                blocked: false,
                blocked_my_stories_from: false,
            },
            StoryView {
                peer_id: mock_peers::CEDAR,
                reaction: Some(ReactionId::Emoji("\u{1F525}".to_owned())),
                repost_id: 309,
                forward_id: MsgId::ZERO,
                date: now - 420,
                blocked: false,
                blocked_my_stories_from: false,
            },
            StoryView {
                peer_id: mock_peers::KESTREL,
                reaction: Some(ReactionId::Emoji("\u{1F44F}".to_owned())),
                repost_id: 0,
                forward_id: MsgId(88),
                date: now - 900,
                blocked: false,
                blocked_my_stories_from: false,
            },
            StoryView {
                peer_id: mock_peers::VIEWER_ORBIT,
                reaction: Some(ReactionId::Emoji("\u{1F60D}".to_owned())),
                repost_id: 0,
                forward_id: MsgId::ZERO,
                date: now - 1500,
                blocked: false,
                blocked_my_stories_from: false,
            },
            StoryView {
                peer_id: mock_peers::VIEWER_MEADOW,
                reaction: None,
                repost_id: 0,
                forward_id: MsgId::ZERO,
                date: now - 2400,
                blocked: false,
                blocked_my_stories_from: false,
            },
            StoryView {
                peer_id: mock_peers::VIEWER_SUMMIT,
                reaction: None,
                repost_id: 0,
                forward_id: MsgId::ZERO,
                date: now - 3100,
                blocked: false,
                blocked_my_stories_from: false,
            },
        ];
        self.views_by_story.insert(
            FullStoryId::new(mock_peers::SELF, 101),
            self_viewers.clone(),
        );

        self.add_story(
            Story {
                id: 101,
                peer_id: mock_peers::SELF,
                date: now - 3600,
                expires,
                last_update_time: now,
                media: StoryMedia::Photo(mock_photo(101)),
                caption: TextWithEntities::plain(
                    "Morning desk setup testing the Rust story viewer pipeline!",
                ),
                out: true,
                in_profile: true,
                pinned_to_top: true,
                privacy: StoryPrivacy::Public,
                privacy_allowed: Vec::new(),
                privacy_disallowed: Vec::new(),
                no_forwards: false,
                edited: false,
                from_peer: None,
                repost: StoryRepostInfo::default(),
                music: None,
                album_ids: BTreeSet::new(),
                sent_reaction: None,
                recent_viewers: self_viewers
                    .iter()
                    .take(RECENT_VIEWERS_MAX)
                    .map(|v| v.peer_id)
                    .collect(),
                views: StoryViews {
                    list: self_viewers,
                    next_offset: String::new(),
                    reactions: 4,
                    forwards: 2,
                    views: 42,
                    total: 42,
                    known: true,
                },
                channel_reactions: StoryViews::default(),
                locations: Vec::new(),
                suggested_reactions: Vec::new(),
                channel_posts: Vec::new(),
                url_areas: Vec::new(),
                weather_areas: vec![WeatherArea {
                    area: StoryArea::from_tl_coords(72.0, 22.0, 34.0, 7.5, -5.0, Some(14.0)),
                    emoji: "\u{2600}".to_owned(),
                    color: 0xE6_1D_6F_A5,
                    millicelsius: 21_000,
                }],
            },
            GradientPreset {
                top: [0x14, 0x28, 0x50],
                mid: [0x27, 0x49, 0x6D],
                bottom: [0x0C, 0x7B, 0x93],
                accent: [0x00, 0xA8, 0xCC],
                orb_x: 0.72,
                orb_y: 0.28,
                stripes: false,
            },
        );

        self.add_story(
            Story {
                id: 102,
                peer_id: mock_peers::SELF,
                date: now - 900,
                expires,
                last_update_time: now,
                media: StoryMedia::Photo(mock_photo(102)),
                caption: TextWithEntities::plain("Evening prototype build passed all checks."),
                out: true,
                in_profile: false,
                pinned_to_top: false,
                privacy: StoryPrivacy::CloseFriends,
                privacy_allowed: Vec::new(),
                privacy_disallowed: Vec::new(),
                no_forwards: false,
                edited: true,
                from_peer: None,
                repost: StoryRepostInfo::default(),
                music: None,
                album_ids: BTreeSet::new(),
                sent_reaction: None,
                recent_viewers: vec![mock_peers::AURORA, mock_peers::CEDAR],
                views: StoryViews {
                    list: vec![
                        StoryView {
                            peer_id: mock_peers::AURORA,
                            reaction: Some(ReactionId::Emoji("\u{1F525}".to_owned())),
                            repost_id: 0,
                            forward_id: MsgId::ZERO,
                            date: now - 240,
                            blocked: false,
                            blocked_my_stories_from: false,
                        },
                        StoryView {
                            peer_id: mock_peers::CEDAR,
                            reaction: None,
                            repost_id: 0,
                            forward_id: MsgId::ZERO,
                            date: now - 500,
                            blocked: false,
                            blocked_my_stories_from: false,
                        },
                    ],
                    next_offset: String::new(),
                    reactions: 1,
                    forwards: 0,
                    views: 11,
                    total: 11,
                    known: true,
                },
                channel_reactions: StoryViews::default(),
                locations: Vec::new(),
                suggested_reactions: Vec::new(),
                channel_posts: Vec::new(),
                url_areas: Vec::new(),
                weather_areas: Vec::new(),
            },
            GradientPreset {
                top: [0x2C, 0x16, 0x54],
                mid: [0x61, 0x28, 0x97],
                bottom: [0xD2, 0x64, 0xB6],
                accent: [0xF6, 0xC9, 0x0E],
                orb_x: 0.30,
                orb_y: 0.34,
                stripes: true,
            },
        );

        // 3. Aurora Studio: 3 stories (1 read, 2 unread) — Story 201 showcases all 4 interactive areas!
        self.read_tills.insert(mock_peers::AURORA, 200);
        self.add_story(
            Story {
                id: 200,
                peer_id: mock_peers::AURORA,
                date: now - 5 * 3600,
                expires,
                last_update_time: now,
                media: StoryMedia::Photo(mock_photo(200)),
                caption: TextWithEntities::plain("Warm-up sketch before the rooftop session."),
                out: false,
                in_profile: true,
                pinned_to_top: false,
                privacy: StoryPrivacy::Public,
                privacy_allowed: Vec::new(),
                privacy_disallowed: Vec::new(),
                no_forwards: false,
                edited: false,
                from_peer: None,
                repost: StoryRepostInfo::default(),
                music: None,
                album_ids: BTreeSet::new(),
                sent_reaction: None,
                recent_viewers: Vec::new(),
                views: StoryViews::default(),
                channel_reactions: StoryViews::default(),
                locations: Vec::new(),
                suggested_reactions: Vec::new(),
                channel_posts: Vec::new(),
                url_areas: Vec::new(),
                weather_areas: Vec::new(),
            },
            GradientPreset {
                top: [0x1F, 0x40, 0x37],
                mid: [0x4B, 0x86, 0x73],
                bottom: [0x99, 0xF2, 0xC8],
                accent: [0xFF, 0xE6, 0x99],
                orb_x: 0.65,
                orb_y: 0.25,
                stripes: false,
            },
        );

        self.add_story(
            Story {
                id: 201,
                peer_id: mock_peers::AURORA,
                date: now - 25 * 60,
                expires,
                last_update_time: now,
                media: StoryMedia::Photo(mock_photo(201)),
                caption: TextWithEntities::plain(
                    "Golden hour from Harbor Observatory! Tap the weather, venue, link, or reaction sticker below to interact — and tap more to expand the full story notes.\n\nWe tested the new optical lens kit along the waterfront promenade and captured clean color transitions all evening. Let us know which frame works best for the autumn issue!",
                ),
                out: false,
                in_profile: true,
                pinned_to_top: false,
                privacy: StoryPrivacy::CloseFriends,
                privacy_allowed: Vec::new(),
                privacy_disallowed: Vec::new(),
                no_forwards: false,
                edited: true,
                from_peer: None,
                repost: StoryRepostInfo::default(),
                music: None,
                album_ids: BTreeSet::new(),
                sent_reaction: None,
                recent_viewers: Vec::new(),
                views: StoryViews::default(),
                channel_reactions: StoryViews::default(),
                locations: vec![StoryLocation {
                    area: StoryArea::from_tl_coords(36.0, 29.0, 56.0, 8.5, -6.5, Some(14.0)),
                    point: GeoPoint {
                        lat: 47.6062,
                        lon: -122.3321,
                        accuracy_radius: Some(50),
                    },
                    title: "Harbor Observatory".to_owned(),
                    address: "140 Waterfront Promenade".to_owned(),
                    city: "Port Harbor".to_owned(),
                    ..StoryLocation::default()
                }],
                weather_areas: vec![WeatherArea {
                    area: StoryArea::from_tl_coords(73.0, 42.0, 38.0, 8.0, 5.5, Some(14.0)),
                    emoji: "\u{26C5}".to_owned(),
                    color: 0xEB_FF_A7_26,
                    millicelsius: 22_400,
                }],
                url_areas: vec![UrlArea {
                    area: StoryArea::from_tl_coords(42.0, 55.0, 62.0, 7.5, -3.5, Some(12.0)),
                    url: "https://aurora-studio.example/autumn-lookbook".to_owned(),
                }],
                suggested_reactions: vec![SuggestedReaction {
                    area: StoryArea::from_tl_coords(74.0, 68.0, 28.0, 14.0, 9.0, Some(18.0)),
                    reaction: ReactionId::Emoji("\u{1F525}".to_owned()),
                    count: 142,
                    flipped: false,
                    dark: false,
                }],
                channel_posts: Vec::new(),
            },
            GradientPreset {
                top: [0x24, 0x14, 0x42],
                mid: [0x90, 0x37, 0x49],
                bottom: [0xE8, 0x7A, 0x5D],
                accent: [0xFF, 0xD1, 0x66],
                orb_x: 0.78,
                orb_y: 0.22,
                stripes: true,
            },
        );

        self.add_story(
            Story {
                id: 202,
                peer_id: mock_peers::AURORA,
                date: now - 8 * 60,
                expires,
                last_update_time: now,
                media: StoryMedia::Photo(mock_photo(202)),
                caption: TextWithEntities::plain("Twilight skyline over the bay."),
                out: false,
                in_profile: false,
                pinned_to_top: false,
                privacy: StoryPrivacy::Public,
                privacy_allowed: Vec::new(),
                privacy_disallowed: Vec::new(),
                no_forwards: false,
                edited: false,
                from_peer: None,
                repost: StoryRepostInfo::default(),
                music: None,
                album_ids: BTreeSet::new(),
                sent_reaction: None,
                recent_viewers: Vec::new(),
                views: StoryViews::default(),
                channel_reactions: StoryViews::default(),
                locations: Vec::new(),
                suggested_reactions: Vec::new(),
                channel_posts: Vec::new(),
                url_areas: Vec::new(),
                weather_areas: Vec::new(),
            },
            GradientPreset {
                top: [0x0F, 0x20, 0x27],
                mid: [0x20, 0x3A, 0x43],
                bottom: [0x2C, 0x53, 0x64],
                accent: [0x64, 0xDF, 0xDF],
                orb_x: 0.50,
                orb_y: 0.30,
                stripes: false,
            },
        );

        // 4. Cedar Labs: Video story + Photo story.
        self.add_story(
            Story {
                id: 301,
                peer_id: mock_peers::CEDAR,
                date: now - 40 * 60,
                expires,
                last_update_time: now,
                media: StoryMedia::Video {
                    document: DocumentData {
                        id: DocumentId(30101),
                        mime_type: "video/mp4".to_owned(),
                        kind: DocumentKind::Video,
                        file_name: "cedar_demo_story.mp4".to_owned(),
                        duration: Some(4.0),
                        width: Some(360),
                        height: Some(640),
                        supports_streaming: true,
                        ..DocumentData::default()
                    },
                    silent: false,
                },
                caption: TextWithEntities::plain(
                    "Streaming video clip decoded through tg-video::StreamingPlayer!",
                ),
                out: false,
                in_profile: true,
                pinned_to_top: false,
                privacy: StoryPrivacy::Contacts,
                privacy_allowed: Vec::new(),
                privacy_disallowed: Vec::new(),
                no_forwards: false,
                edited: false,
                from_peer: None,
                repost: StoryRepostInfo::default(),
                music: None,
                album_ids: BTreeSet::new(),
                sent_reaction: None,
                recent_viewers: Vec::new(),
                views: StoryViews::default(),
                channel_reactions: StoryViews::default(),
                locations: Vec::new(),
                suggested_reactions: Vec::new(),
                channel_posts: Vec::new(),
                url_areas: Vec::new(),
                weather_areas: Vec::new(),
            },
            GradientPreset {
                top: [0x13, 0x4E, 0x5E],
                mid: [0x2C, 0x77, 0x44],
                bottom: [0x71, 0xB2, 0x80],
                accent: [0xD4, 0xFC, 0x79],
                orb_x: 0.42,
                orb_y: 0.36,
                stripes: true,
            },
        );

        // 5. Design Journal (Broadcast Channel story with channel views + reactions).
        let channel_reactors = vec![
            StoryView {
                peer_id: mock_peers::AURORA,
                reaction: Some(ReactionId::Emoji("\u{2764}".to_owned())),
                repost_id: 0,
                forward_id: MsgId::ZERO,
                date: now - 600,
                blocked: false,
                blocked_my_stories_from: false,
            },
            StoryView {
                peer_id: mock_peers::KESTREL,
                reaction: Some(ReactionId::Emoji("\u{1F525}".to_owned())),
                repost_id: 0,
                forward_id: MsgId::ZERO,
                date: now - 1200,
                blocked: false,
                blocked_my_stories_from: false,
            },
        ];
        self.add_story(
            Story {
                id: 401,
                peer_id: mock_peers::DESIGN_JOURNAL,
                date: now - 2 * 3600,
                expires,
                last_update_time: now,
                media: StoryMedia::Photo(mock_photo(401)),
                caption: TextWithEntities::plain(
                    "Issue #18 — Typography grids, micro-motion curves, and desktop ergonomics.",
                ),
                out: false,
                in_profile: true,
                pinned_to_top: false,
                privacy: StoryPrivacy::Public,
                privacy_allowed: Vec::new(),
                privacy_disallowed: Vec::new(),
                no_forwards: false,
                edited: false,
                from_peer: None,
                repost: StoryRepostInfo::default(),
                music: None,
                album_ids: BTreeSet::new(),
                sent_reaction: None,
                recent_viewers: vec![mock_peers::AURORA, mock_peers::KESTREL],
                views: StoryViews {
                    list: Vec::new(),
                    next_offset: String::new(),
                    reactions: 86,
                    forwards: 19,
                    views: 1_420,
                    total: 1_439,
                    known: true,
                },
                channel_reactions: StoryViews {
                    list: channel_reactors,
                    next_offset: String::new(),
                    reactions: 86,
                    forwards: 19,
                    views: 1_420,
                    total: 105,
                    known: true,
                },
                locations: Vec::new(),
                suggested_reactions: vec![SuggestedReaction {
                    area: StoryArea::from_tl_coords(50.0, 62.0, 30.0, 14.0, -4.0, Some(18.0)),
                    reaction: ReactionId::Emoji("\u{2764}".to_owned()),
                    count: 86,
                    flipped: true,
                    dark: true,
                }],
                channel_posts: vec![tg_stories::ChannelPost {
                    area: StoryArea::from_tl_coords(50.0, 38.0, 68.0, 12.0, 2.0, Some(12.0)),
                    item_id: FullMsgId::new(mock_peers::DESIGN_JOURNAL, MsgId(418)),
                }],
                url_areas: Vec::new(),
                weather_areas: Vec::new(),
            },
            GradientPreset {
                top: [0x2E, 0x1A, 0x47],
                mid: [0x59, 0x2E, 0x83],
                bottom: [0xB2, 0x7C, 0x66],
                accent: [0xFF, 0xB4, 0xA2],
                orb_x: 0.25,
                orb_y: 0.22,
                stripes: false,
            },
        );

        // 6. Harbor Radio (Live Video Stream story).
        self.add_story(
            Story {
                id: 501,
                peer_id: mock_peers::HARBOR_RADIO,
                date: now - 300,
                expires,
                last_update_time: now,
                media: StoryMedia::VideoStream(StoryVideoStream {
                    peer_id: mock_peers::HARBOR_RADIO,
                    call_id: tg_data::CallId(9901),
                    access_hash: 4242,
                    rtmp_stream: true,
                    viewers_count: 318,
                    messages_enabled: true,
                    messages_min_price: 0,
                    loaded: true,
                }),
                caption: TextWithEntities::plain("Live synth session from Studio B."),
                out: false,
                in_profile: false,
                pinned_to_top: false,
                privacy: StoryPrivacy::Public,
                privacy_allowed: Vec::new(),
                privacy_disallowed: Vec::new(),
                no_forwards: false,
                edited: false,
                from_peer: None,
                repost: StoryRepostInfo::default(),
                music: None,
                album_ids: BTreeSet::new(),
                sent_reaction: None,
                recent_viewers: Vec::new(),
                views: StoryViews::default(),
                channel_reactions: StoryViews::default(),
                locations: Vec::new(),
                suggested_reactions: Vec::new(),
                channel_posts: Vec::new(),
                url_areas: Vec::new(),
                weather_areas: Vec::new(),
            },
            GradientPreset {
                top: [0x42, 0x0C, 0x14],
                mid: [0x8E, 0x1B, 0x2C],
                bottom: [0xC9, 0x3B, 0x48],
                accent: [0xFF, 0x8A, 0x80],
                orb_x: 0.52,
                orb_y: 0.44,
                stripes: true,
            },
        );

        // 7. Kestrel Dev (Repost story — newest unread premium peer so Aurora sits between Kestrel & Cedar).
        self.add_story(
            Story {
                id: 601,
                peer_id: mock_peers::KESTREL,
                date: now - 5 * 60,
                expires,
                last_update_time: now,
                media: StoryMedia::Photo(mock_photo(601)),
                caption: TextWithEntities::plain("Clean modular layout architecture!"),
                out: false,
                in_profile: false,
                pinned_to_top: false,
                privacy: StoryPrivacy::SelectedContacts,
                privacy_allowed: Vec::new(),
                privacy_disallowed: Vec::new(),
                no_forwards: false,
                edited: false,
                from_peer: None,
                repost: StoryRepostInfo {
                    source_peer: Some(mock_peers::DESIGN_JOURNAL),
                    source_name: "Design Journal".to_owned(),
                    source_id: 401,
                    modified: false,
                },
                music: None,
                album_ids: BTreeSet::new(),
                sent_reaction: None,
                recent_viewers: Vec::new(),
                views: StoryViews::default(),
                channel_reactions: StoryViews::default(),
                locations: Vec::new(),
                suggested_reactions: Vec::new(),
                channel_posts: Vec::new(),
                url_areas: Vec::new(),
                weather_areas: Vec::new(),
            },
            GradientPreset {
                top: [0x0D, 0x3B, 0x66],
                mid: [0x1D, 0x6A, 0x96],
                bottom: [0x85, 0xC1, 0xE9],
                accent: [0xFA, 0xF0, 0xCA],
                orb_x: 0.68,
                orb_y: 0.30,
                stripes: false,
            },
        );

        // 8. Northwind Notes (Already-read peer so strip shows muted read segments).
        self.read_tills.insert(mock_peers::NORTHWIND, 702);
        for (sid, cap, top, bot) in [
            (
                701,
                "Field notes part 1.",
                [0x26, 0x32, 0x38],
                [0x54, 0x6E, 0x7A],
            ),
            (
                702,
                "Field notes part 2.",
                [0x37, 0x47, 0x4F],
                [0x78, 0x90, 0x9C],
            ),
        ] {
            self.add_story(
                Story {
                    id: sid,
                    peer_id: mock_peers::NORTHWIND,
                    date: now - 4 * 3600 + sid * 60,
                    expires,
                    last_update_time: now,
                    media: StoryMedia::Photo(mock_photo(sid as u64)),
                    caption: TextWithEntities::plain(cap),
                    out: false,
                    in_profile: false,
                    pinned_to_top: false,
                    privacy: StoryPrivacy::Public,
                    privacy_allowed: Vec::new(),
                    privacy_disallowed: Vec::new(),
                    no_forwards: false,
                    edited: false,
                    from_peer: None,
                    repost: StoryRepostInfo::default(),
                    music: None,
                    album_ids: BTreeSet::new(),
                    sent_reaction: None,
                    recent_viewers: Vec::new(),
                    views: StoryViews::default(),
                    channel_reactions: StoryViews::default(),
                    locations: Vec::new(),
                    suggested_reactions: Vec::new(),
                    channel_posts: Vec::new(),
                    url_areas: Vec::new(),
                    weather_areas: Vec::new(),
                },
                GradientPreset {
                    top,
                    mid: [0x45, 0x5A, 0x64],
                    bottom: bot,
                    accent: [0xCF, 0xD8, 0xDC],
                    orb_x: 0.45,
                    orb_y: 0.35,
                    stripes: false,
                },
            );
        }

        // 9. Solstice Archive (Hidden/Archived peer).
        self.add_story(
            Story {
                id: 801,
                peer_id: mock_peers::SOLSTICE,
                date: now - 6 * 3600,
                expires,
                last_update_time: now,
                media: StoryMedia::Photo(mock_photo(801)),
                caption: TextWithEntities::plain("Archived stories channel snapshot."),
                out: false,
                in_profile: false,
                pinned_to_top: false,
                privacy: StoryPrivacy::Public,
                privacy_allowed: Vec::new(),
                privacy_disallowed: Vec::new(),
                no_forwards: false,
                edited: false,
                from_peer: None,
                repost: StoryRepostInfo::default(),
                music: None,
                album_ids: BTreeSet::new(),
                sent_reaction: None,
                recent_viewers: Vec::new(),
                views: StoryViews::default(),
                channel_reactions: StoryViews::default(),
                locations: Vec::new(),
                suggested_reactions: Vec::new(),
                channel_posts: Vec::new(),
                url_areas: Vec::new(),
                weather_areas: Vec::new(),
            },
            GradientPreset {
                top: [0x5D, 0x40, 0x37],
                mid: [0x8D, 0x6E, 0x63],
                bottom: [0xFF, 0xCA, 0x28],
                accent: [0xFF, 0xF1, 0x76],
                orb_x: 0.60,
                orb_y: 0.30,
                stripes: false,
            },
        );
    }

    fn register_peer(&mut self, profile: PeerProfile, hidden: bool, is_contact: bool) {
        let peer_id = profile.peer_id;
        let is_channel = profile.is_broadcast || profile.is_megagroup;
        let meta = PeerStoryMeta {
            ctx: StoryPeerContext {
                is_self: profile.is_self,
                is_channel,
                is_broadcast: profile.is_broadcast,
                is_megagroup: profile.is_megagroup,
                is_user: !is_channel,
                is_verified: profile.is_verified,
                is_premium: profile.is_premium,
                is_service_user: false,
                has_username: !profile.username.is_empty(),
                can_post_stories: profile.is_self,
                can_edit_stories: profile.is_self,
                can_delete_stories: profile.is_self,
            },
            stories_hidden: hidden,
            channel_am_in: is_channel,
            is_contact,
            is_bot: false,
            stories_correspondent: true,
        };
        self.profiles.insert(peer_id, profile);
        self.peer_meta.insert(peer_id, meta);
    }

    fn add_story(&mut self, story: Story, preset: GradientPreset) {
        let full_id = story.full_id();
        self.stories
            .entry(story.peer_id)
            .or_default()
            .insert(story.id, story);
        self.gradient_presets.insert(full_id, preset);
    }

    fn build_peer_stories(&self, peer_id: PeerId) -> Option<ParsedPeerStories> {
        let map = self.stories.get(&peer_id)?;
        if map.is_empty() {
            return None;
        }
        let max_read_id = self.read_tills.get(&peer_id).copied().unwrap_or(0);
        let has_video_stream = map
            .values()
            .any(|s| matches!(s.media, StoryMedia::VideoStream(_)));
        let items = map
            .values()
            .cloned()
            .map(|s| ParsedStoryItem::Full(Box::new(s)))
            .collect();
        Some(ParsedPeerStories {
            peer_id,
            max_read_id,
            items,
            has_video_stream,
        })
    }
}

impl StoriesBackend for MockStoriesBackend {
    fn self_peer_id(&self) -> PeerId {
        self.self_peer_id
    }

    fn account_premium(&self) -> bool {
        self.account_premium
    }

    fn set_account_premium(&mut self, premium: bool) {
        self.account_premium = premium;
    }

    fn peer_profile(&self, peer_id: PeerId) -> PeerProfile {
        self.profiles.get(&peer_id).cloned().unwrap_or(PeerProfile {
            peer_id,
            name: format!("Peer {}", peer_id.raw()),
            short_name: format!("Peer {}", peer_id.raw()),
            ..PeerProfile::default()
        })
    }

    fn share_targets(&self) -> Vec<ShareTarget> {
        let mut targets: Vec<ShareTarget> = self
            .profiles
            .values()
            .filter(|p| !p.is_broadcast)
            .map(|p| ShareTarget {
                peer_id: p.peer_id,
                name: if p.is_self {
                    "Saved Messages".to_owned()
                } else {
                    p.name.clone()
                },
                short_name: p.short_name.clone(),
                initials: if p.is_self {
                    "SM".to_owned()
                } else {
                    p.initials.clone()
                },
                avatar_rgb: p.avatar_top_rgb,
                can_send: true,
            })
            .collect();
        targets.sort_by_key(|t| (t.peer_id != self.self_peer_id, t.name.clone()));
        targets
    }

    fn execute_actions(
        &mut self,
        store: &mut StoriesStore,
        actions: Vec<StoriesAction>,
        now_secs: TimeId,
    ) {
        for action in actions {
            self.executed_actions_count += 1;
            match action {
                StoriesAction::GetAllStories { list, .. } => {
                    let want_hidden = list == StorySourcesList::Hidden;
                    let mut peer_stories = Vec::new();
                    for (&peer_id, meta) in &self.peer_meta {
                        if meta.stories_hidden == want_hidden
                            && let Some(ps) = self.build_peer_stories(peer_id)
                        {
                            peer_stories.push(ps);
                        }
                    }
                    let count = peer_stories.len() as i32;
                    store.apply_all_stories(
                        list,
                        ParsedAllStories::All {
                            has_more: false,
                            count,
                            state: format!("mock_state_{}", list.index()),
                            peer_stories,
                            stealth_mode: self.stealth_mode,
                        },
                        now_secs,
                    );
                }
                StoriesAction::GetPeerStories { peer_id } => {
                    if let Some(ps) = self.build_peer_stories(peer_id) {
                        store.apply_peer_stories(
                            ps,
                            tg_stories::ParseSource::DirectRequest,
                            now_secs,
                        );
                    } else {
                        store.fail_peer_stories(peer_id);
                    }
                }
                StoriesAction::GetStoriesById { peer_id, ids } => {
                    let items: Vec<ParsedStoryItem> = ids
                        .into_iter()
                        .map(|sid| {
                            if let Some(story) = self
                                .stories
                                .get(&peer_id)
                                .and_then(|m| m.get(&sid))
                                .cloned()
                            {
                                ParsedStoryItem::Full(Box::new(story))
                            } else {
                                ParsedStoryItem::Deleted(sid)
                            }
                        })
                        .collect();
                    store.apply_resolved_stories(peer_id, items, now_secs);
                }
                StoriesAction::ReadStories { peer_id, max_id } => {
                    let entry = self.read_tills.entry(peer_id).or_insert(0);
                    *entry = (*entry).max(max_id);
                    store.finish_mark_as_read(peer_id);
                }
                StoriesAction::IncrementStoryViews { peer_id, .. } => {
                    store.finish_increment_views(peer_id);
                }
                StoriesAction::GetAllReadPeerStories => {
                    for (&peer_id, &max_id) in &self.read_tills {
                        store.apply_read_stories(peer_id, max_id);
                    }
                    store.finish_read_tills();
                }
                StoriesAction::GetPeerMaxIds { peer_ids } => {
                    let results: Vec<(PeerId, tg_stories::RecentStoryInfo)> = peer_ids
                        .into_iter()
                        .map(|pid| {
                            let max_id = self
                                .stories
                                .get(&pid)
                                .and_then(|m| m.keys().next_back().copied())
                                .unwrap_or(0);
                            let live = self.stories.get(&pid).is_some_and(|m| {
                                m.values()
                                    .any(|s| matches!(s.media, StoryMedia::VideoStream(_)))
                            });
                            (pid, tg_stories::RecentStoryInfo { max_id, live })
                        })
                        .collect();
                    store.apply_peer_max_ids(&results);
                }
                StoriesAction::GetStoryViewsList {
                    peer_id,
                    story_id,
                    offset,
                    limit,
                } => {
                    let full_id = FullStoryId::new(peer_id, story_id);
                    let mut all_views = self
                        .views_by_story
                        .get(&full_id)
                        .cloned()
                        .or_else(|| {
                            self.stories
                                .get(&peer_id)
                                .and_then(|m| m.get(&story_id))
                                .map(|s| s.views.list.clone())
                        })
                        .unwrap_or_default();
                    for v in &mut all_views {
                        v.blocked_my_stories_from = self.blocked_viewers.contains(&v.peer_id);
                    }
                    let start = offset.parse::<usize>().unwrap_or(0);
                    let take = (limit.max(1) as usize).min(all_views.len().saturating_sub(start));
                    let page = all_views[start..start + take].to_vec();
                    let next_offset = if start + take < all_views.len() {
                        (start + take).to_string()
                    } else {
                        String::new()
                    };
                    let base_views = self
                        .stories
                        .get(&peer_id)
                        .and_then(|m| m.get(&story_id))
                        .map(|s| (s.views.views, s.views.reactions, s.views.forwards))
                        .unwrap_or((all_views.len() as i32, 0, 0));
                    let slice = StoryViews {
                        list: page,
                        next_offset,
                        reactions: base_views.1,
                        forwards: base_views.2,
                        views: base_views.0,
                        total: all_views.len() as i32,
                        known: true,
                    };
                    store.apply_views_slice(full_id, &offset, slice);
                }
                StoriesAction::GetStoryReactionsList {
                    peer_id,
                    story_id,
                    offset,
                    ..
                } => {
                    let full_id = FullStoryId::new(peer_id, story_id);
                    if let Some(story) = self.stories.get(&peer_id).and_then(|m| m.get(&story_id)) {
                        store.apply_channel_reactions_slice(
                            full_id,
                            &offset,
                            story.channel_reactions.clone(),
                        );
                    }
                }
                StoriesAction::GetStoriesViews { peer_id, ids } => {
                    for sid in ids {
                        if let Some(story) = self.stories.get(&peer_id).and_then(|m| m.get(&sid)) {
                            let counts = StoryViewsCounts {
                                views: story.views.views,
                                forwards: story.views.forwards,
                                reactions: story.views.reactions,
                                reactions_counts: BTreeMap::new(),
                                recent_viewers: story.recent_viewers.clone(),
                            };
                            store.apply_views_counts(FullStoryId::new(peer_id, sid), counts);
                        }
                    }
                }
                StoriesAction::LoadAlbumStories {
                    peer_id, album_id, ..
                } => {
                    let matching: Vec<ParsedStoryItem> = self
                        .stories
                        .get(&peer_id)
                        .into_iter()
                        .flat_map(|m| m.values())
                        .filter(|s| match album_id {
                            STORIES_ALBUM_ID_SAVED => s.in_profile,
                            STORIES_ALBUM_ID_ARCHIVE => true,
                            other => s.album_ids.contains(&other),
                        })
                        .cloned()
                        .map(|s| ParsedStoryItem::Full(Box::new(s)))
                        .collect();
                    let count = matching.len() as i32;
                    let pinned_to_top: Vec<StoryId> = self
                        .stories
                        .get(&peer_id)
                        .into_iter()
                        .flat_map(|m| m.values())
                        .filter(|s| s.pinned_to_top)
                        .map(|s| s.id)
                        .collect();
                    store.apply_album_stories(
                        peer_id,
                        album_id,
                        ParsedStoriesPage {
                            count,
                            items: matching,
                            pinned_to_top,
                        },
                        true,
                        now_secs,
                    );
                }
                StoriesAction::GetAlbums { .. } => {
                    // Mock backend has no custom albums beyond Saved (0) and Archive (-1).
                }
                StoriesAction::ActivateStealthMode => {
                    // Stealth mode lasts 25 minutes active and 1 hour cooldown (matching tdesktop).
                    let mode = StealthMode {
                        enabled_till: now_secs + 25 * 60,
                        cooldown_till: now_secs + 60 * 60,
                    };
                    self.stealth_mode = mode;
                    store.apply_stealth_mode(mode);
                }
                StoriesAction::SendReaction { story_id, reaction } => {
                    let is_channel = self
                        .peer_meta
                        .get(&story_id.peer)
                        .is_some_and(|m| m.ctx.is_channel);
                    if let Some(story) = self
                        .stories
                        .get_mut(&story_id.peer)
                        .and_then(|m| m.get_mut(&story_id.story))
                    {
                        story.set_reaction(reaction, is_channel);
                    }
                }
                StoriesAction::DeleteStories { peer_id, ids } => {
                    for sid in ids {
                        if let Some(map) = self.stories.get_mut(&peer_id) {
                            map.remove(&sid);
                        }
                        store.apply_deleted(peer_id, sid);
                    }
                }
                StoriesAction::ToggleInProfile {
                    peer_id,
                    ids,
                    in_profile,
                } => {
                    for &sid in &ids {
                        if let Some(story) =
                            self.stories.get_mut(&peer_id).and_then(|m| m.get_mut(&sid))
                        {
                            story.in_profile = in_profile;
                        }
                    }
                    store.apply_toggle_in_profile_done(peer_id, &ids, in_profile);
                }
                StoriesAction::TogglePinnedToTop { peer_id, ids } => {
                    if let Some(map) = self.stories.get_mut(&peer_id) {
                        for (&sid, story) in map.iter_mut() {
                            story.pinned_to_top = ids.contains(&sid);
                        }
                    }
                    store.set_pinned_to_top(peer_id, ids);
                }
                StoriesAction::TogglePeerStoriesHidden { peer_id, hidden } => {
                    if let Some(meta) = self.peer_meta.get_mut(&peer_id) {
                        meta.stories_hidden = hidden;
                    }
                }
                StoriesAction::ReorderAlbumStories { .. } => {}
                StoriesAction::StartPreload { story_id } => {
                    let _ = self.load_story_media(story_id);
                    store.finish_preload(story_id, true);
                }
                StoriesAction::CancelPreload { .. } => {}
            }
        }
    }

    fn load_story_media(&mut self, story_id: FullStoryId) -> Option<StoryMediaAsset> {
        if let Some(asset) = self.media_cache.get(&story_id) {
            return Some(asset.clone());
        }
        let preset = self
            .gradient_presets
            .get(&story_id)
            .copied()
            .unwrap_or(GradientPreset {
                top: [0x1B, 0x28, 0x45],
                mid: [0x27, 0x40, 0x60],
                bottom: [0x33, 0x5C, 0x81],
                accent: [0x65, 0xAF, 0xFF],
                orb_x: 0.5,
                orb_y: 0.3,
                stripes: false,
            });
        let png = generate_gradient_story_png(360, 640, preset);
        let asset = StoryMediaAsset::PhotoBytes(png);
        self.media_cache.insert(story_id, asset.clone());
        Some(asset)
    }

    fn send_story_reply(
        &mut self,
        story_id: FullStoryId,
        text: &str,
        now_secs: TimeId,
    ) -> Result<(), String> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err("Reply text cannot be empty".to_owned());
        }
        self.sent_replies.push(SentStoryReply {
            story_id,
            text: trimmed.to_owned(),
            sent_at: now_secs,
        });
        Ok(())
    }

    fn share_story(
        &mut self,
        story_id: FullStoryId,
        targets: &[PeerId],
        comment: &str,
    ) -> Result<(), String> {
        if targets.is_empty() {
            return Err("Select at least one recipient".to_owned());
        }
        self.shared_records.push(SharedStoryRecord {
            story_id,
            targets: targets.to_vec(),
            comment: comment.trim().to_owned(),
        });
        Ok(())
    }

    fn export_story_link(&self, story_id: FullStoryId) -> Option<String> {
        let profile = self.profiles.get(&story_id.peer)?;
        if profile.username.is_empty() {
            return None;
        }
        Some(format!(
            "https://t.me/{}/s/{}",
            profile.username, story_id.story
        ))
    }

    fn report_story(&mut self, story_id: FullStoryId, reason: &str) -> Result<(), String> {
        self.reported_records.push(ReportedStoryRecord {
            story_id,
            reason: reason.to_owned(),
        });
        Ok(())
    }

    fn set_viewer_stories_blocked(&mut self, peer_id: PeerId, blocked: bool) {
        if blocked {
            self.blocked_viewers.insert(peer_id);
        } else {
            self.blocked_viewers.remove(&peer_id);
        }
    }

    fn is_viewer_stories_blocked(&self, peer_id: PeerId) -> bool {
        self.blocked_viewers.contains(&peer_id)
    }

    fn save_stories_blocklist(&mut self, peers: &[PeerId]) -> Result<bool, String> {
        self.blocked_viewers = peers.iter().copied().collect();
        self.saved_stories_blocklists.push(peers.to_vec());
        Ok(true)
    }

    fn pending_uploads(&self) -> Vec<StoryUploadStatus> {
        self.pending_uploads.clone()
    }

    fn send_story(
        &mut self,
        store: Option<&mut StoriesStore>,
        request: SendStoryRequest,
        now_secs: TimeId,
    ) -> Result<StoryId, String> {
        let (story_media, asset) = match &request.media {
            StoryUploadMedia::Photo { bytes, .. } => {
                if bytes.is_empty() {
                    return Err("Story photo payload cannot be empty".to_owned());
                }
                let id = self.next_story_id as u64;
                (
                    StoryMedia::Photo(mock_photo(id)),
                    StoryMediaAsset::PhotoBytes(bytes.clone()),
                )
            }
            StoryUploadMedia::Video {
                bytes,
                file_name,
                width,
                height,
                duration_secs,
                has_audio,
                ..
            } => {
                if bytes.is_empty() {
                    return Err("Story video payload cannot be empty".to_owned());
                }
                let id = self.next_story_id as u64;
                (
                    StoryMedia::Video {
                        document: DocumentData {
                            id: DocumentId(id * 100),
                            mime_type: "video/mp4".to_owned(),
                            kind: DocumentKind::Video,
                            file_name: if file_name.is_empty() {
                                "story.mp4".to_owned()
                            } else {
                                file_name.clone()
                            },
                            duration: Some((*duration_secs).max(0.0)),
                            width: Some((*width).max(1)),
                            height: Some((*height).max(1)),
                            supports_streaming: true,
                            ..DocumentData::default()
                        },
                        silent: !*has_audio,
                    },
                    StoryMediaAsset::VideoBytes(bytes.clone()),
                )
            }
        };

        let story_id = self.next_story_id;
        self.next_story_id += 1;
        let period = request.period.unwrap_or(DEFAULT_STORY_PERIOD_SECS).max(60);
        let caption = ui_text_to_data_text(&request.caption);
        let full_id = FullStoryId::new(request.peer_id, story_id);

        let story = Story {
            id: story_id,
            peer_id: request.peer_id,
            date: now_secs,
            expires: now_secs.saturating_add(period),
            last_update_time: now_secs,
            media: story_media,
            caption,
            out: true,
            in_profile: request.pinned,
            pinned_to_top: false,
            privacy: request.privacy.privacy,
            privacy_allowed: request.privacy.allow_peers.clone(),
            privacy_disallowed: request.privacy.disallow_peers.clone(),
            no_forwards: request.noforwards,
            edited: false,
            from_peer: None,
            repost: StoryRepostInfo::default(),
            music: None,
            album_ids: request.albums.iter().copied().collect(),
            sent_reaction: None,
            recent_viewers: Vec::new(),
            views: StoryViews {
                known: true,
                ..StoryViews::default()
            },
            channel_reactions: StoryViews::default(),
            locations: request.locations.clone(),
            suggested_reactions: request.suggested_reactions.clone(),
            channel_posts: request.channel_posts.clone(),
            url_areas: request.url_areas.clone(),
            weather_areas: request.weather_areas.clone(),
        };

        self.media_cache.insert(full_id, asset);
        self.stories
            .entry(request.peer_id)
            .or_default()
            .insert(story_id, story.clone());
        self.sent_stories.push(request);

        if let Some(store) = store {
            if store.source(story.peer_id).is_some() {
                store.apply_story_update(
                    story.peer_id,
                    ParsedStoryItem::Full(Box::new(story)),
                    now_secs,
                );
            } else if let Some(ps) = self.build_peer_stories(story.peer_id) {
                store.apply_peer_stories(ps, tg_stories::ParseSource::DirectRequest, now_secs);
            }
        }
        Ok(story_id)
    }

    fn edit_story(
        &mut self,
        store: Option<&mut StoriesStore>,
        request: EditStoryRequest,
        now_secs: TimeId,
    ) -> Result<(), String> {
        let full_id = FullStoryId::new(request.peer_id, request.story_id);
        let new_media = match &request.media {
            Some(StoryUploadMedia::Photo { bytes, .. }) => {
                if bytes.is_empty() {
                    return Err("Replacement story photo cannot be empty".to_owned());
                }
                Some((
                    StoryMedia::Photo(mock_photo(request.story_id as u64 + 10_000)),
                    StoryMediaAsset::PhotoBytes(bytes.clone()),
                ))
            }
            Some(StoryUploadMedia::Video {
                bytes,
                file_name,
                width,
                height,
                duration_secs,
                has_audio,
                ..
            }) => {
                if bytes.is_empty() {
                    return Err("Replacement story video cannot be empty".to_owned());
                }
                Some((
                    StoryMedia::Video {
                        document: DocumentData {
                            id: DocumentId(request.story_id as u64 * 100 + 1),
                            mime_type: "video/mp4".to_owned(),
                            kind: DocumentKind::Video,
                            file_name: if file_name.is_empty() {
                                "story.mp4".to_owned()
                            } else {
                                file_name.clone()
                            },
                            duration: Some((*duration_secs).max(0.0)),
                            width: Some((*width).max(1)),
                            height: Some((*height).max(1)),
                            supports_streaming: true,
                            ..DocumentData::default()
                        },
                        silent: !*has_audio,
                    },
                    StoryMediaAsset::VideoBytes(bytes.clone()),
                ))
            }
            None => None,
        };

        let story = self
            .stories
            .get_mut(&request.peer_id)
            .and_then(|m| m.get_mut(&request.story_id))
            .ok_or_else(|| format!("Story {} not found", request.story_id))?;

        if let Some((media, asset)) = new_media {
            story.media = media;
            self.media_cache.insert(full_id, asset);
        }
        if let Some(caption) = &request.caption {
            story.caption = ui_text_to_data_text(caption);
        }
        if let Some(privacy) = &request.privacy {
            story.privacy = privacy.privacy;
            story.privacy_allowed = privacy.allow_peers.clone();
            story.privacy_disallowed = privacy.disallow_peers.clone();
        }
        if let Some(areas) = &request.media_areas {
            story.locations = areas.locations.clone();
            story.suggested_reactions = areas.suggested_reactions.clone();
            story.channel_posts = areas.channel_posts.clone();
            story.url_areas = areas.url_areas.clone();
            story.weather_areas = areas.weather_areas.clone();
        }
        story.edited = true;
        story.last_update_time = now_secs;
        let updated = story.clone();
        self.edited_stories.push(request);

        if let Some(store) = store {
            store.insert_story(updated, now_secs);
        }
        Ok(())
    }

    fn dismiss_failed_uploads(&mut self, peer_id: PeerId) {
        self.pending_uploads
            .retain(|u| !(u.peer_id == peer_id && u.failed));
    }

    fn close_friends(&self) -> Vec<PeerId> {
        self.close_friends.clone()
    }

    fn set_close_friends(&mut self, peers: Vec<PeerId>) -> Result<(), String> {
        self.close_friends = peers;
        Ok(())
    }

    fn contacts_for_privacy(&self) -> Vec<PeerProfile> {
        let mut out: Vec<PeerProfile> = self
            .profiles
            .values()
            .filter(|p| !p.is_self && !p.is_broadcast && !p.is_megagroup)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

fn ui_text_to_data_text(ui: &tg_text::TextWithEntities) -> TextWithEntities {
    let entities = ui
        .entities
        .iter()
        .filter_map(|e| {
            let kind = match e.entity_type() {
                tg_text::EntityType::Bold => tg_data::EntityKind::Bold,
                tg_text::EntityType::Italic => tg_data::EntityKind::Italic,
                tg_text::EntityType::Underline => tg_data::EntityKind::Underline,
                tg_text::EntityType::StrikeOut => tg_data::EntityKind::StrikeOut,
                tg_text::EntityType::Spoiler => tg_data::EntityKind::Spoiler,
                tg_text::EntityType::Code => tg_data::EntityKind::Code,
                tg_text::EntityType::Pre => tg_data::EntityKind::Pre {
                    language: e.data().to_owned(),
                },
                tg_text::EntityType::CustomUrl => tg_data::EntityKind::TextUrl {
                    url: e.data().to_owned(),
                },
                tg_text::EntityType::Url => tg_data::EntityKind::Url,
                tg_text::EntityType::Mention => tg_data::EntityKind::Mention,
                tg_text::EntityType::Hashtag => tg_data::EntityKind::Hashtag,
                tg_text::EntityType::Blockquote => tg_data::EntityKind::Blockquote {
                    collapsed: !e.data().is_empty(),
                },
                _ => return None,
            };
            Some(tg_data::TextEntity {
                offset: e.offset(),
                length: e.length(),
                kind,
            })
        })
        .collect();
    TextWithEntities {
        text: ui.text.clone(),
        entities,
    }
}

fn mock_photo(id: u64) -> PhotoData {
    PhotoData {
        id: PhotoId(id),
        dc_id: 2,
        ..PhotoData::default()
    }
}

/// Renders a rich procedural multi-stop gradient PNG with luminous bokeh circles and subtle
/// architectural bands at runtime (decoded back through `tg_media::image::read_image`).
#[must_use]
pub fn generate_gradient_story_png(width: u32, height: u32, preset: GradientPreset) -> Vec<u8> {
    let mut img = RgbaImage::new(width, height);
    let wf = width.max(1) as f32;
    let hf = height.max(1) as f32;
    let orb_cx = preset.orb_x * wf;
    let orb_cy = preset.orb_y * hf;
    let orb_r = wf * 0.42;
    let orb2_cx = (1.0 - preset.orb_x) * wf;
    let orb2_cy = (0.72_f32) * hf;
    let orb2_r = wf * 0.34;

    for y in 0..height {
        let v = y as f32 / hf;
        let (c0, c1, local_t) = if v < 0.5 {
            (preset.top, preset.mid, v * 2.0)
        } else {
            (preset.mid, preset.bottom, (v - 0.5) * 2.0)
        };
        let base_r = lerp_u8(c0[0], c1[0], local_t);
        let base_g = lerp_u8(c0[1], c1[1], local_t);
        let base_b = lerp_u8(c0[2], c1[2], local_t);

        for x in 0..width {
            let xf = x as f32;
            let yf = y as f32;
            let mut r = f32::from(base_r);
            let mut g = f32::from(base_g);
            let mut b = f32::from(base_b);

            // Soft luminous orb #1.
            let dx1 = xf - orb_cx;
            let dy1 = yf - orb_cy;
            let dist1 = (dx1 * dx1 + dy1 * dy1).sqrt() / orb_r;
            if dist1 < 1.0 {
                let glow = (1.0 - dist1) * (1.0 - dist1) * 0.55;
                r = r * (1.0 - glow) + f32::from(preset.accent[0]) * glow;
                g = g * (1.0 - glow) + f32::from(preset.accent[1]) * glow;
                b = b * (1.0 - glow) + f32::from(preset.accent[2]) * glow;
            }

            // Soft secondary orb #2.
            let dx2 = xf - orb2_cx;
            let dy2 = yf - orb2_cy;
            let dist2 = (dx2 * dx2 + dy2 * dy2).sqrt() / orb2_r;
            if dist2 < 1.0 {
                let glow = (1.0 - dist2) * (1.0 - dist2) * 0.28;
                r = r * (1.0 - glow) + 255.0 * glow;
                g = g * (1.0 - glow) + 245.0 * glow;
                b = b * (1.0 - glow) + 230.0 * glow;
            }

            // Optional subtle diagonal light rays for texture depth.
            if preset.stripes {
                let wave = ((xf * 0.028 + yf * 0.015).sin() * 0.5 + 0.5) * 10.0;
                r = (r + wave).min(255.0);
                g = (g + wave).min(255.0);
                b = (b + wave).min(255.0);
            }

            img.put_pixel(
                x,
                y,
                Rgba([
                    r.clamp(0.0, 255.0) as u8,
                    g.clamp(0.0, 255.0) as u8,
                    b.clamp(0.0, 255.0) as u8,
                    255,
                ]),
            );
        }
    }

    let mut out = Cursor::new(Vec::new());
    let _ = img.write_to(&mut out, ImageFormat::Png);
    out.into_inner()
}

#[inline]
fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (f32::from(a) + (f32::from(b) - f32::from(a)) * t.clamp(0.0, 1.0)).round() as u8
}
