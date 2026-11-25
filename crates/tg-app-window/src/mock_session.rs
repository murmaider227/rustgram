//! Unified in-memory [`MockSession`] implementing every screen's backend trait over ONE
//! consistent fictional dataset.
//!
//! Port origin: single-session data hub standing in for `Main::Session` (`main/main_session.h`)
//! and `ApiWrap` (`apiwrap.h`) until live `tg-api` + `tg-data` integration replaces each trait.
//!
//! All names, handles, phone numbers (`+999 661 ...`), messages, media, and URLs are strictly
//! synthetic placeholders (no real people's names or real Telegram accounts).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tg_audio::{AudioDataSource, AudioMsgId, PlaylistProvider};
use tg_chat::autocomplete::RecentHashtags;
use tg_chat::emoji::{EmojiKeywordResult, EmojiVariants, SkinTone};
use tg_chat::stickers::{StickerDocument, StickerPanelSection, StickerTabIcon};
use tg_chat::{BotCommand, DocumentId, LastseenStatus, StickerSetId, UserId, UserInfo};
use tg_data::rights::{ChatAdminRights, ChatBannedRights};
use tg_history::{DialogBadgesState, DialogSendState, LocalDateTime};
use tg_lang::LangPackLanguageInfo;
use tg_stats::{
    BoostsListSlice, BoostsOffsetToken, EarnHistorySlice, MessageInteractionInfo, StatisticalGraph,
};
use tg_text::{EntityInText, EntityType, TextWithEntities, utf8_to_utf16_offset};
use tg_video::RangeLoader;

use tg_app_core::stand_in::backend::Backend;
use tg_app_core::stand_in::types::{
    AuthUser, CodeSentInfo, CodeVerifyOutcome, ConnectionStatus, DialogEntry, FilterId,
    FolderFilter, MessageItem, MsgId, PeerId, PeerKind, PeerSummary,
};
use tg_app_history::bots::backend::{
    BotChatMessage, BotChatSnapshot, BotMessageAttachment, BotPeerCandidate, BotsBackend,
    SubmittedPoll,
};
use tg_app_history::bots::mock::{
    build_demo_inline_keyboard, build_demo_reply_keyboard, build_gallery_inline_page,
    build_list_inline_page,
};
use tg_app_history::business::backend::BusinessBackend;
use tg_app_history::business::mock::MockBusinessBackend;
use tg_app_history::business::model::{
    ActiveEmojiStatus, AwaySettings, BotSearchResult, BusinessChatLink, BusinessLocation,
    BusinessOverviewSnapshot, BusinessShortcutId, ChatIntro, ChatbotsSettings, GreetingSettings,
    QuickReplyAttachment, QuickReplyMessage, QuickReplyShortcut, WorkingHours,
};
use tg_app_history::history::backend::{
    AlbumItem, ContactStatusBanner, DocumentAttachment, DraftMessage, ForwardHeader,
    HistoryBackend, HistoryMedia, HistoryMessage, HistoryPeerInfo, HistorySlice, MessageExtra,
    PhotoAttachment, PinnedMessageSummary, PollAttachment, PollOption, ReactionCounter,
    ReplyHeader, SendMessageExtended, SendMessageRequest, ServiceAction, StickerAttachment,
    ThreadDraft, TypingAction as HistoryTypingAction, VoiceAttachment,
};
use tg_app_history::history::mock::{
    sample_lottie_star_json, sample_voice_waveform_5bit, synthetic_stripped_thumb,
};
use tg_app_history::history::sections_model::{
    FORUM_GENERAL_TOPIC_ID, FORUM_TOPIC_ICON_COLORS, ForumTopic, ForwardOptions, ThreadKey,
};
use tg_app_info::dialogs::backend::{
    ArchiveSummary, ChatFolder as DialogChatFolder, DialogDraft, DialogItem, DialogListQuery,
    DialogMediaPreview, DialogMediaPreviewKind, DialogPeerKind, DialogSlice, DialogsBackend,
    FolderIconKind, FolderTagBadge, FolderUnreadState, GlobalPeerResult, HashtagSuggestion,
    MessageSearchResult, RecentPeerItem, SearchRequest, SearchResults, SearchTab, SearchTagChip,
    SpecialRowKind, TopPeerItem, TypingState,
};
use tg_app_info::info::backend::{
    CommonGroupEntry, CreateChannelRequest, CreateGroupRequest, DownloadEntry, EmojiUserpicRequest,
    InfoBackend, InfoPeerKind, InviteImporterInfo, InviteImportersList, InviteLinkInfo,
    InviteLinkRequest, InviteLinksList, JoinRequestsSummary, LanguageEntry,
    MediaItem as InfoMediaItem, MemberEntry, MemberRole, OwnershipTransferCheck,
    OwnershipTransferError, PeerColorSettings, PeerProfile, PeerReactionsConfig, PeerUsername,
    PeerVerificationStatus, ProfilePeerGiftsPage, ProfileStoriesPage, ProfileStoryAlbum,
    ReportReason, SavedMusicTrack, SharedMediaCounts, SharedMediaTab, SimilarChannelsResult,
    TermsOfServiceData, UpdatePeerColorRequest, UpdatePeerInfoRequest,
};
use tg_app_info::info::communities_mock::CommunitiesMockStore;
use tg_app_info::info::peer_mgmt_mock::PeerManagementMockStore;
use tg_app_info::info::profile_extras_mock::ProfileExtrasMockStore;
use tg_app_info::info::values::{
    Birthday as InfoBirthday, UsernameAvailability, format_members_status, format_phone,
    format_subscribers_status, is_valid_phone, normalize_username,
    validate_username_syntax as validate_info_username,
};
use tg_app_info::payments::backend::{
    BoostsGiveawayBundle, CheckoutBundle, CreditsOverviewBundle, PassportBackend,
    PassportSessionBundle, PassportUnlockResult, PaymentSubmitOutcome, PaymentsBackend,
    PremiumPromoBundle, ReceivedGiftAction, StarGiftsBundle, ValidatedInfoResult,
};
use tg_app_info::payments::mock::{MockPassportBackend, MockPaymentsBackend};
use tg_app_info::statistics::backend::{
    BoostStatusBundle, BotEarnBundle, ChannelEarnBundle, ChannelStatsBundle, MessageStatsBundle,
    PeerDisplayInfo, RecentPostPreview, StatsBackend, SupergroupStatsBundle,
};
use tg_app_info::statistics::mock::MockStatsBackend;
use tg_app_media::calls::backend::{
    ActiveCallSnapshot, AudioDeviceOption, BackendSoundEvent, CallAudioSettings, CallPeerInfo,
    CallsBackend, GroupCallSnapshot, JoinAsCandidate, RecentCallEntry,
};
use tg_app_media::calls::mock::MockCallsBackend;
use tg_app_media::media::backend::{
    AudioTrackMetadata, MediaBackend, MediaItem as ViewerMediaItem, MediaItemId,
};
use tg_app_media::media::mock::{
    MOCK_PHOTO_AURORA_ID, MOCK_PHOTO_LANDSCAPE_ID, MOCK_PHOTO_PORTRAIT_ID, MockMediaBackend,
};
use tg_app_media::notifications::backend::{
    IncomingMessageNotification, IncomingNotifyEvent, ItemNotificationType, NotificationMediaKind,
    NotifyBackend, NotifyPeerInfo,
};
use tg_app_media::notifications::settings::UnreadBadgeState;
use tg_app_media::stories::backend::{
    PeerProfile as StoryPeerProfile, ShareTarget, StoriesBackend, StoryMediaAsset,
};
use tg_app_media::stories::mock::MockStoriesBackend;
use tg_app_media::webview::backend::{
    IvPageBundle, WebViewBackend, WebViewBackendError, WebViewLaunchInfo, WebViewRequest,
};
use tg_app_media::webview::mock::{MockWebViewBackend, SAMPLE_IV_URL};
use tg_app_settings::chat_helpers::backend::{
    ChatHelpersBackend, GifCategory, GifItem, StickerMedia,
};
use tg_app_settings::chat_helpers::mock::MockChatHelpersBackend;
use tg_app_settings::settings::backend::{
    ActiveSession, AutoDownloadSettings, Birthday as SettingsBirthday,
    ChatFolder as SettingsChatFolder, CloudPasswordError, CloudPasswordState, ExportRequest,
    FolderIcon, GlobalPrivacySettings, LocalStorageStats, NotificationEventsSettings, NotifyScope,
    PeerRef, PrivacyKey, PrivacyRule, ProfileInfo, ProxyCheckStatus, SavedRingtone,
    ScopeNotifySettings, SettingsBackend, StickerSetInfo, SuggestedFolder, UsernameStatus,
    WebAuthorization,
};
use tg_app_settings::settings::mock::MockSettingsBackend;

pub use tg_app_core::core_types::session_ids::{PendingChatOpen, session_folders, session_peers};

/// Reference local timestamp (`2026-11-22 14:30:00`) for deterministic dialog timestamps.
const SESSION_NOW: LocalDateTime = LocalDateTime::new(2026, 11, 22, 14, 30, 0);
/// Base unix timestamp for yesterday's messages (`2026-11-21 10:00:00 UTC`).
const BASE_UNIX_YESTERDAY: i32 = 1_795_255_200;
/// Base unix timestamp for today's messages (`2026-11-22 10:00:00 UTC`).
const BASE_UNIX_TODAY: i32 = BASE_UNIX_YESTERDAY + 86_400;

/// Chat-list preview metadata of a mock message, the same shape `tg-app-live` builds from
/// `HistoryItem::toPreview` (16 px thumbnail, type icon, blue media / service text).
fn dialog_media_preview(msg: Option<&HistoryMessage>) -> Option<DialogMediaPreview> {
    let msg = msg?;
    let has_caption = !msg.text.text.trim().is_empty();
    let preview = |kind, stripped_thumb: Option<Vec<u8>>, has_caption: bool| DialogMediaPreview {
        kind,
        stripped_thumb,
        use_service_color: !has_caption,
        has_caption,
    };
    if msg.is_service() {
        return Some(preview(DialogMediaPreviewKind::Service, None, false));
    }
    let media = msg.media.as_ref()?;
    Some(match media {
        HistoryMedia::Photo(p) => preview(
            DialogMediaPreviewKind::Photo,
            p.stripped_thumb.clone(),
            has_caption,
        ),
        HistoryMedia::Album(items) => preview(
            DialogMediaPreviewKind::Photo,
            items.first().and_then(|i| i.stripped_thumb.clone()),
            has_caption,
        ),
        HistoryMedia::Video(v) => preview(
            if v.is_gif {
                DialogMediaPreviewKind::Gif
            } else {
                DialogMediaPreviewKind::Video
            },
            v.stripped_thumb.clone(),
            has_caption,
        ),
        HistoryMedia::Document(_) => preview(DialogMediaPreviewKind::File, None, has_caption),
        HistoryMedia::Voice(_) => preview(DialogMediaPreviewKind::Voice, None, has_caption),
        HistoryMedia::Sticker(_) => preview(DialogMediaPreviewKind::Sticker, None, false),
        HistoryMedia::Poll(_) => preview(DialogMediaPreviewKind::Poll, None, false),
        HistoryMedia::Location(_) => preview(DialogMediaPreviewKind::Location, None, false),
        HistoryMedia::Contact(_) => preview(DialogMediaPreviewKind::Contact, None, false),
        HistoryMedia::RoundVideo(_) => preview(DialogMediaPreviewKind::RoundVideo, None, false),
        HistoryMedia::Game(_) => preview(DialogMediaPreviewKind::Game, None, false),
        HistoryMedia::TodoList(_) => preview(DialogMediaPreviewKind::TodoList, None, false),
        HistoryMedia::Story(_) => preview(DialogMediaPreviewKind::Story, None, false),
        HistoryMedia::Widget(_)
        | HistoryMedia::Giveaway(_)
        | HistoryMedia::GiveawayResults(_)
        | HistoryMedia::VideoStream(_) => return None,
    })
}

/// Unified peer record shared across Dialogs, History, Info, Settings, Notifications, and Stats.
#[derive(Debug, Clone)]
struct PeerRecord {
    id: PeerId,
    kind: InfoPeerKind,
    title: String,
    status: String,
    is_online: bool,
    verified: bool,
    premium: bool,
    color_index: u8,
    avatar_rgb: [u8; 3],
    initials: String,
    phone: Option<String>,
    usernames: Vec<PeerUsername>,
    bio: String,
    birthday: Option<InfoBirthday>,
    invite_link: Option<String>,
    muted: bool,
    pinned: bool,
    archived: bool,
    unread_count: u32,
    unread_mark: bool,
    unread_mentions: u32,
    is_contact: bool,
    is_blocked: bool,
    can_send: bool,
    can_edit: bool,
    can_add_members: bool,
    can_manage_admins: bool,
    can_manage_permissions: bool,
    is_public: bool,
    no_forwards: bool,
    is_forum: bool,
    members_count: u32,
    online_count: u32,
    admins_count: u32,
    default_banned_rights: ChatBannedRights,
    slowmode_seconds: u32,
    auto_delete_ttl_seconds: u32,
    linked_discussion_peer: Option<PeerId>,
    bot_commands_count: usize,
    bot_reads_all_messages: bool,
    has_mini_app: bool,
    folders: Vec<FilterId>,
    contact_status: Option<ContactStatusBanner>,
    typing: Option<TypingState>,
    last_message_time: LocalDateTime,
}

impl PeerRecord {
    fn primary_username(&self) -> Option<&str> {
        self.usernames
            .iter()
            .find(|u| u.is_active)
            .map(|u| u.username.as_str())
    }

    fn coarse_kind(&self) -> PeerKind {
        match self.kind {
            InfoPeerKind::User | InfoPeerKind::Bot | InfoPeerKind::SavedMessages => PeerKind::User,
            InfoPeerKind::Group | InfoPeerKind::Supergroup => PeerKind::Group,
            InfoPeerKind::Channel => PeerKind::Channel,
        }
    }

    fn dialog_kind(&self) -> DialogPeerKind {
        match self.kind {
            InfoPeerKind::User | InfoPeerKind::SavedMessages => DialogPeerKind::User,
            InfoPeerKind::Bot => DialogPeerKind::Bot,
            InfoPeerKind::Group | InfoPeerKind::Supergroup => {
                if self.is_forum {
                    DialogPeerKind::Forum
                } else {
                    DialogPeerKind::Group
                }
            }
            InfoPeerKind::Channel => DialogPeerKind::Channel,
        }
    }

    fn to_peer_summary(&self) -> PeerSummary {
        let phone_or_link = if let Some(ref phone) = self.phone {
            Some(phone.clone())
        } else if let Some(u) = self.primary_username() {
            Some(format!("t.me/{u}"))
        } else {
            self.invite_link.clone()
        };
        PeerSummary {
            id: self.id,
            kind: self.coarse_kind(),
            title: self.title.clone(),
            status: self.status.clone(),
            username: self.primary_username().map(str::to_owned),
            phone_or_link,
            about: self.bio.clone(),
            initials: self.initials.clone(),
            avatar_rgb: self.avatar_rgb,
            muted: self.muted,
            can_send: self.can_send,
            is_saved_messages: self.kind == InfoPeerKind::SavedMessages,
        }
    }

    fn to_peer_ref(&self) -> PeerRef {
        PeerRef {
            id: self.id.0,
            name: self.title.clone(),
            username: self.primary_username().unwrap_or_default().to_owned(),
            subtitle: self.status.clone(),
            is_group_or_channel: self.kind.is_group() || self.kind.is_channel(),
        }
    }

    fn to_display_info(&self) -> PeerDisplayInfo {
        let subtitle = self
            .primary_username()
            .map(|u| format!("@{u}"))
            .unwrap_or_else(|| self.status.clone());
        PeerDisplayInfo {
            id: self.id.bare(),
            title: self.title.clone(),
            subtitle,
            initials: self.initials.clone(),
            avatar_rgb: self.avatar_rgb,
        }
    }
}

/// Shared mutable state locked behind `Arc<Mutex<...>>` so every screen and backend handle sees
/// identical live state.
struct MockSessionState {
    signed_in: bool,
    user: AuthUser,
    expected_code: String,
    expected_password: String,
    require_2fa: bool,
    peers: Vec<PeerRecord>,
    histories: HashMap<PeerId, Vec<HistoryMessage>>,
    pinned_messages: HashMap<PeerId, Vec<MsgId>>,
    first_unread: HashMap<PeerId, Option<MsgId>>,
    drafts: HashMap<PeerId, DraftMessage>,
    thread_drafts: HashMap<ThreadKey, ThreadDraft>,
    forum_topics: HashMap<PeerId, Vec<ForumTopic>>,
    topic_messages: HashMap<(PeerId, MsgId), Vec<HistoryMessage>>,
    replies: HashMap<(PeerId, MsgId), Vec<HistoryMessage>>,
    members: HashMap<PeerId, Vec<MemberEntry>>,
    folders: Vec<SettingsChatFolder>,
    pinned_dialog_orders: HashMap<DialogListQuery, Vec<PeerId>>,
    archive_collapsed: bool,
    top_peers: Vec<PeerId>,
    recent_search_peers: Vec<PeerId>,
    recent_hashtags: Vec<String>,
    search_tags: Vec<SearchTagChip>,
    global_peers: Vec<GlobalPeerResult>,
    next_msg_id: i32,
    next_user_bare: u64,
    next_chat_bare: u64,
    next_channel_bare: u64,
    connection_status: ConnectionStatus,
    proxy_enabled: bool,
    // Media viewer mapping: (peer, msg_id) -> MediaItemId + rich media catalog
    msg_media_map: HashMap<(PeerId, MsgId), MediaItemId>,
    media_items: Vec<ViewerMediaItem>,
    // Notifications queue + pending chat activations
    notify_events: VecDeque<IncomingNotifyEvent>,
    pending_chat_opens: Vec<PendingChatOpen>,
    notify_sim_step: usize,
    // Settings / Info auxiliary state
    settings_store: MockSettingsBackend,
    active_language: String,
    languages: Vec<LanguageEntry>,
    tos: TermsOfServiceData,
    // WebView + Statistics state
    webview: MockWebViewBackend,
    pending_mini_app_urls: HashMap<u64, String>,
    /// Bots confirmed for Mini Apps; stands in for the account's stored session settings.
    trusted_mini_app_bots: std::collections::HashSet<PeerId>,
    webview_requests_log: Vec<WebViewRequest>,
    stats_target_peer: PeerId,
    // Bot chat metadata & attach menu store (`crates/tg-app/src/bots/`)
    bot_info_map: HashMap<PeerId, tg_bots::commands::BotInfo>,
    bot_attach_menu: tg_bots::mini_app::AttachMenuStore,
    // Layer-233 extras demo state (`history/{todo,sponsored,factcheck,monoforum}.rs`)
    layer233: MockLayer233State,

    // WP-10 peer management demo store (invite links, join requests, colors, transfers, similar channels, downloads)
    peer_mgmt: PeerManagementMockStore,
    // WP-26 profile extras demo store (stories/albums, peer gifts, saved music, emoji userpic, reactions, verification, business bot)
    profile_extras: ProfileExtrasMockStore,
    // WP-27 communities, managed bots, moderation, welcome messages, and anti-spam demo store
    communities: CommunitiesMockStore,
    // Demo outgoing media uploads (rendered by HistoryWidget progress bar)
    pending_uploads: Vec<tg_app_history::history::backend::MediaUploadStatus>,
    // WP-51 cached AI message summaries & applied suggestion cards
    summaries: HashMap<(PeerId, MsgId), tg_app_history::history::backend::MessageSummaryEntry>,
    applied_userpic_photo_id: Option<i64>,
    // WP-53/54/61 engagement, read receipts, bot start, and phone confirmation test state
    unread_mentions: HashMap<PeerId, Vec<MsgId>>,
    unread_reactions: HashMap<PeerId, Vec<MsgId>>,
    unread_poll_votes: HashMap<PeerId, Vec<MsgId>>,
    read_participants: HashMap<
        (PeerId, MsgId),
        Vec<tg_app_core::core_types::dto::engagement::MessageReadParticipant>,
    >,
    outbox_read_dates:
        HashMap<(PeerId, MsgId), tg_app_core::core_types::dto::engagement::OutboxReadDateStatus>,
    message_authors:
        HashMap<(PeerId, MsgId), tg_app_core::core_types::dto::engagement::MessageAuthorInfo>,
    future_creators:
        HashMap<PeerId, tg_app_core::core_types::dto::engagement::FutureChatCreatorInfo>,
    started_bots: Vec<tg_bots::protocol::StartBotParams>,
    // WP-78 Mute submenu state: per-peer mute expiry and notification tone.
    mute_until: HashMap<PeerId, i32>,
    notify_sounds: HashMap<PeerId, tg_app_core::notifications::settings::NotifySound>,
    confirmed_phone_codes: Vec<(String, String)>,
    // WP-73 remaining UI triggers state (default empty to preserve baseline snapshots)
    top_peers_enabled: bool,
    contact_birthdays: Vec<tg_app_core::core_types::dto::contacts_extras::ContactBirthdayEntry>,
    sponsored_peers: Vec<tg_app_core::core_types::dto::contacts_extras::SponsoredPeerInfo>,
    promo_data: Option<tg_app_core::core_types::misc::PromoDataSnapshot>,
    hidden_promo_peers: Vec<PeerId>,
    blocked_from_replies:
        Vec<tg_app_core::core_types::dto::contacts_extras::BlockFromRepliesRequest>,
}

/// In-memory Layer-233 extras state owned by [`MockSessionState`].
struct MockLayer233State {
    sponsored: tg_app_history::history::sponsored::SponsoredMessagesState,
    factchecks: HashMap<(PeerId, MsgId), tg_app_history::history::factcheck::FactCheckData>,
    can_edit_factcheck: bool,
    monoforum_sublists: HashMap<PeerId, Vec<tg_app_history::history::monoforum::MonoforumSublist>>,
    monoforum_messages: HashMap<(PeerId, PeerId), Vec<HistoryMessage>>,
    suggested_posts:
        HashMap<(PeerId, MsgId), tg_app_history::history::monoforum::SuggestedPostData>,
    viewed_ads: Vec<Vec<u8>>,
    clicked_ads: Vec<(Vec<u8>, bool, bool)>,
}

impl MockLayer233State {
    fn demo() -> Self {
        use tg_app_history::history::factcheck::FactCheckData;
        use tg_app_history::history::monoforum::{
            MonoforumSublist, SuggestedPostData, SuggestedPrice,
        };
        use tg_app_history::history::sponsored::{
            SponsoredMessageItem, SponsoredMessagesState, SponsoredPeerList, SponsoredPlacementMode,
        };

        let mut sponsored = SponsoredMessagesState::new(true, false);
        let devlog = session_peers::CHANNEL_DEV_LOG;
        sponsored.store_list(
            devlog,
            SponsoredPeerList {
                posts_between: None,
                start_delay_ms: 0,
                between_delay_ms: 0,
                mode: SponsoredPlacementMode::AppendToEnd,
                entries: vec![SponsoredMessageItem {
                    random_id: b"demo_ad_devlog_1".to_vec(),
                    url: "https://t.me/cloud_rust_ci_demo".to_owned(),
                    title: "Rust Cloud CI".to_owned(),
                    text: TextWithEntities::simple(
                        "Fast deterministic CI runners for cross-platform Rust desktop workspaces.",
                    ),
                    button_text: "Open Channel".to_owned(),
                    sponsor_info: Some("Sponsored by Demo Cloud Tools".to_owned()),
                    additional_info: None,
                    recommended: false,
                    can_report: true,
                    color_index: 2,
                    min_display_duration_ms: 0,
                    max_display_duration_ms: 0,
                    is_link_internal: true,
                    viewed: false,
                }],
                received_ms: 1_000,
                injected_count: 0,
                showed_all: false,
            },
            1_000,
        );

        let mut factchecks = HashMap::new();
        factchecks.insert(
            (devlog, MsgId(301)),
            FactCheckData {
                need_check: false,
                country: Some("US".to_owned()),
                text: Some(TextWithEntities::simple(
                    "Independent verification note: the 60fps virtualized layout benchmarks were \
                     measured on synthetic 5,000-message histories with software rendering enabled \
                     and reproduce across Day, Night, and Tinted palettes.",
                )),
                hash: 233_001,
            },
        );

        let mira = session_peers::USER_MIRA;
        let sam = session_peers::USER_SAM;
        let mut monoforum_sublists = HashMap::new();
        monoforum_sublists.insert(
            devlog,
            vec![
                MonoforumSublist {
                    parent_peer_id: devlog,
                    sublist_peer_id: mira,
                    title: "Mira Vance".to_owned(),
                    avatar_rgb: [0x5B, 0xA8, 0x6C],
                    top_message_id: MsgId(480),
                    last_preview: "Suggested post: Zero-copy video decoding notes".to_owned(),
                    last_date_unix: BASE_UNIX_TODAY + 11_100,
                    read_inbox_max_id: MsgId(479),
                    read_outbox_max_id: MsgId(479),
                    unread_count: 1,
                    unread_reactions_count: 0,
                    unread_mark: false,
                    nopaid_messages_exception: false,
                    draft_text: None,
                },
                MonoforumSublist {
                    parent_peer_id: devlog,
                    sublist_peer_id: sam,
                    title: "Sam Chen".to_owned(),
                    avatar_rgb: [0xE0, 0x8A, 0x4B],
                    top_message_id: MsgId(481),
                    last_preview: "Question about the upcoming Layer-233 release".to_owned(),
                    last_date_unix: BASE_UNIX_TODAY + 10_800,
                    read_inbox_max_id: MsgId(481),
                    read_outbox_max_id: MsgId(481),
                    unread_count: 0,
                    unread_reactions_count: 0,
                    unread_mark: false,
                    nopaid_messages_exception: true,
                    draft_text: None,
                },
            ],
        );

        let mut monoforum_messages = HashMap::new();
        monoforum_messages.insert(
            (devlog, mira),
            vec![HistoryMessage {
                extra: MessageExtra::default(),
                id: MsgId(480),
                peer_id: devlog,
                from_id: mira,
                sender_name: "Mira Vance".to_owned(),
                date_unix: BASE_UNIX_TODAY + 11_100,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::simple(
                    "Suggested post: Zero-copy video decoding notes for Rustgram Dev Log.",
                ),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: false,
                is_sending: false,
                is_pinned: false,
            }],
        );
        monoforum_messages.insert(
            (devlog, sam),
            vec![HistoryMessage {
                extra: MessageExtra::default(),
                id: MsgId(481),
                peer_id: devlog,
                from_id: sam,
                sender_name: "Sam Chen".to_owned(),
                date_unix: BASE_UNIX_TODAY + 10_800,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::simple("Question about the upcoming Layer-233 release."),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
            }],
        );

        let mut suggested_posts = HashMap::new();
        suggested_posts.insert(
            (devlog, MsgId(480)),
            SuggestedPostData {
                price: Some(SuggestedPrice::stars(250)),
                schedule_date: Some(BASE_UNIX_TODAY + 86_400),
                accepted: false,
                rejected: false,
                paid_stars: false,
                paid_ton: false,
            },
        );

        Self {
            sponsored,
            factchecks,
            can_edit_factcheck: true,
            monoforum_sublists,
            monoforum_messages,
            suggested_posts,
            viewed_ads: Vec::new(),
            clicked_ads: Vec::new(),
        }
    }
}

impl MockSessionState {
    fn new(start_signed_in: bool) -> Self {
        let user = AuthUser {
            id: session_peers::SELF,
            name: "Taylor Kim".to_owned(),
            username: "taylor_kim".to_owned(),
            phone: "+999 661 0000".to_owned(),
            bio: "Building Rustgram in pure Rust.".to_owned(),
            initials: "TK".to_owned(),
            avatar_rgb: [0x40, 0xA7, 0xE3],
        };

        let peers = build_initial_peers(&user);
        let (histories, pinned_messages, first_unread, msg_media_map) = build_initial_histories();
        let (forum_topics, topic_messages, replies) = build_initial_forum_and_replies();
        let members = build_initial_members();
        let media_items = build_initial_media_items();

        let folders = vec![
            SettingsChatFolder {
                id: session_folders::PERSONAL.0,
                title: "Personal".to_owned(),
                icon: FolderIcon::Private,
                color_index: Some(3),
                include_contacts: true,
                include_non_contacts: true,
                included_peers: vec![
                    session_peers::SELF.0,
                    session_peers::USER_MIRA.0,
                    session_peers::USER_THEO.0,
                    session_peers::USER_SAM.0,
                    session_peers::USER_RILEY.0,
                ],
                ..SettingsChatFolder::default()
            },
            SettingsChatFolder {
                id: session_folders::WORK.0,
                title: "Work".to_owned(),
                icon: FolderIcon::Work,
                color_index: Some(5),
                include_groups: true,
                included_peers: vec![
                    session_peers::GROUP_CORE.0,
                    session_peers::GROUP_UI_LAB.0,
                    session_peers::USER_MIRA.0,
                    session_peers::USER_SAM.0,
                    session_peers::CHANNEL_DEV_LOG.0,
                ],
                shareable: true,
                ..SettingsChatFolder::default()
            },
            SettingsChatFolder {
                id: session_folders::CHANNELS.0,
                title: "Channels".to_owned(),
                icon: FolderIcon::Channels,
                color_index: Some(4),
                include_channels: true,
                included_peers: vec![
                    session_peers::CHANNEL_DEV_LOG.0,
                    session_peers::CHANNEL_RELEASES.0,
                ],
                ..SettingsChatFolder::default()
            },
            SettingsChatFolder {
                id: session_folders::BOTS.0,
                title: "Bots".to_owned(),
                icon: FolderIcon::Bots,
                color_index: Some(1),
                include_bots: true,
                included_peers: vec![session_peers::BOT_CATALOG.0],
                ..SettingsChatFolder::default()
            },
            SettingsChatFolder {
                id: session_folders::UNREAD.0,
                title: "Unread".to_owned(),
                icon: FolderIcon::Unread,
                color_index: Some(0),
                include_contacts: true,
                include_non_contacts: true,
                include_groups: true,
                include_channels: true,
                include_bots: true,
                exclude_read: true,
                exclude_archived: true,
                ..SettingsChatFolder::default()
            },
        ];

        let mut pinned_dialog_orders = HashMap::new();
        pinned_dialog_orders.insert(
            DialogListQuery::Filter(FilterId::ALL),
            vec![session_peers::SELF, session_peers::GROUP_CORE],
        );

        let top_peers = vec![
            session_peers::USER_MIRA,
            session_peers::USER_SAM,
            session_peers::USER_THEO,
            session_peers::BOT_CATALOG,
            session_peers::USER_RILEY,
        ];
        let recent_search_peers = vec![
            session_peers::GROUP_CORE,
            session_peers::USER_MIRA,
            session_peers::CHANNEL_DEV_LOG,
        ];
        let recent_hashtags = vec![
            "rustgram".to_owned(),
            "rust".to_owned(),
            "design".to_owned(),
            "release".to_owned(),
        ];
        let search_tags = vec![
            SearchTagChip {
                id: "todo".to_owned(),
                label: "#todo".to_owned(),
                count: 2,
            },
            SearchTagChip {
                id: "notes".to_owned(),
                label: "#notes".to_owned(),
                count: 3,
            },
        ];
        let global_peers = vec![
            GlobalPeerResult {
                peer_id: session_peers::CHANNEL_DEV_LOG,
                kind: DialogPeerKind::Channel,
                title: "Rustgram Dev Log".to_owned(),
                username: "rustgram_log".to_owned(),
                subtitle: "@rustgram_log, 14.7K subscribers".to_owned(),
                color_index: 4,
                verified: true,
                premium: false,
            },
            GlobalPeerResult {
                peer_id: session_peers::BOT_CATALOG,
                kind: DialogPeerKind::Bot,
                title: "Catalog Bot".to_owned(),
                username: "catalog_demo_bot".to_owned(),
                subtitle: "@catalog_demo_bot, bot".to_owned(),
                color_index: 5,
                verified: true,
                premium: false,
            },
            GlobalPeerResult {
                peer_id: session_peers::GROUP_CORE,
                kind: DialogPeerKind::Group,
                title: "Rustgram Core Port".to_owned(),
                username: "rustgram_core".to_owned(),
                subtitle: "@rustgram_core, 20 members".to_owned(),
                color_index: 0,
                verified: false,
                premium: false,
            },
        ];

        let mut settings_store = MockSettingsBackend::new();
        let _ = settings_store.update_name("Taylor", "Kim");
        let _ = settings_store.update_username("taylor_kim");
        let _ = settings_store.update_bio("Building Rustgram in pure Rust.");

        let languages = vec![
            LanguageEntry {
                id: "en".to_owned(),
                name: "English".to_owned(),
                native_name: "English".to_owned(),
                official: true,
                progress_percent: 100,
            },
            LanguageEntry {
                id: "de".to_owned(),
                name: "German".to_owned(),
                native_name: "Deutsch".to_owned(),
                official: true,
                progress_percent: 98,
            },
            LanguageEntry {
                id: "es".to_owned(),
                name: "Spanish".to_owned(),
                native_name: "Español".to_owned(),
                official: true,
                progress_percent: 99,
            },
            LanguageEntry {
                id: "fr".to_owned(),
                name: "French".to_owned(),
                native_name: "Français".to_owned(),
                official: true,
                progress_percent: 97,
            },
            LanguageEntry {
                id: "uk".to_owned(),
                name: "Ukrainian".to_owned(),
                native_name: "Українська".to_owned(),
                official: true,
                progress_percent: 99,
            },
        ];

        let tos = TermsOfServiceData {
            id: "rustgram_mock_tos_2026".to_owned(),
            title: "Terms of Service".to_owned(),
            body:
                "Rustgram is a GPL-3.0 Rust port of Telegram Desktop running in local mock mode. \
                   No real network requests or real accounts are used in this preview."
                    .to_owned(),
            min_age_confirm: Some(16),
            accepted: false,
        };

        Self {
            signed_in: start_signed_in,
            user,
            expected_code: "12345".to_owned(),
            expected_password: "rust".to_owned(),
            require_2fa: true,
            peers,
            histories,
            pinned_messages,
            first_unread,
            drafts: HashMap::new(),
            thread_drafts: HashMap::new(),
            forum_topics,
            topic_messages,
            replies,
            members,
            folders,
            pinned_dialog_orders,
            archive_collapsed: false,
            top_peers,
            recent_search_peers,
            recent_hashtags,
            search_tags,
            global_peers,
            next_msg_id: 500,
            next_user_bare: 1100,
            next_chat_bare: 2100,
            next_channel_bare: 3100,
            connection_status: ConnectionStatus::Connected,
            proxy_enabled: false,
            msg_media_map,
            media_items,
            notify_events: VecDeque::new(),
            pending_chat_opens: Vec::new(),
            notify_sim_step: 0,
            settings_store,
            active_language: "en".to_owned(),
            languages,
            tos,
            webview: MockWebViewBackend::new(),
            pending_mini_app_urls: HashMap::new(),
            trusted_mini_app_bots: std::collections::HashSet::new(),
            webview_requests_log: Vec::new(),
            stats_target_peer: session_peers::CHANNEL_DEV_LOG,
            bot_info_map: build_initial_bot_infos(),
            bot_attach_menu: build_initial_bot_attach_menu(),
            layer233: MockLayer233State::demo(),
            peer_mgmt: PeerManagementMockStore::new_for_peers(
                session_peers::SELF,
                session_peers::GROUP_CORE,
                session_peers::CHANNEL_DEV_LOG,
                session_peers::CHANNEL_RELEASES,
            ),
            profile_extras: ProfileExtrasMockStore::new_for_peers(
                session_peers::SELF,
                session_peers::USER_MIRA,
                session_peers::GROUP_CORE,
                session_peers::CHANNEL_DEV_LOG,
                session_peers::BOT_CATALOG,
            ),
            communities: CommunitiesMockStore::new_for_peers(
                session_peers::SELF,
                session_peers::GROUP_CORE,
                session_peers::CHANNEL_DEV_LOG,
                session_peers::BOT_CATALOG,
            ),
            pending_uploads: Vec::new(),
            summaries: HashMap::new(),
            applied_userpic_photo_id: None,
            unread_mentions: HashMap::new(),
            unread_reactions: HashMap::new(),
            unread_poll_votes: HashMap::new(),
            read_participants: HashMap::new(),
            outbox_read_dates: HashMap::new(),
            message_authors: HashMap::new(),
            future_creators: HashMap::from([(
                session_peers::GROUP_CORE,
                tg_app_core::core_types::dto::engagement::FutureChatCreatorInfo {
                    user_id: session_peers::USER_MIRA,
                    name: "Mira Vance".to_owned(),
                    username: Some("mira_v".to_owned()),
                },
            )]),
            started_bots: Vec::new(),
            mute_until: HashMap::new(),
            notify_sounds: HashMap::new(),
            confirmed_phone_codes: Vec::new(),
            top_peers_enabled: true,
            contact_birthdays: Vec::new(),
            sponsored_peers: Vec::new(),
            promo_data: None,
            hidden_promo_peers: Vec::new(),
            blocked_from_replies: Vec::new(),
        }
    }

    fn peer_index(&self, peer_id: PeerId) -> Option<usize> {
        self.peers.iter().position(|p| p.id == peer_id)
    }

    fn peer(&self, peer_id: PeerId) -> Option<&PeerRecord> {
        self.peers.iter().find(|p| p.id == peer_id)
    }

    fn peer_mut(&mut self, peer_id: PeerId) -> Option<&mut PeerRecord> {
        self.peers.iter_mut().find(|p| p.id == peer_id)
    }

    fn peer_matches_folder(&self, peer: &PeerRecord, filter_id: FilterId) -> bool {
        if filter_id == FilterId::ALL {
            return !peer.archived;
        }
        let Some(folder) = self.folders.iter().find(|f| f.id == filter_id.0) else {
            return !peer.archived && peer.folders.contains(&filter_id);
        };
        if folder.exclude_archived && peer.archived {
            return false;
        }
        if folder.exclude_muted && peer.muted {
            return false;
        }
        if folder.exclude_read && peer.unread_count == 0 && !peer.unread_mark {
            return false;
        }
        if folder.excluded_peers.contains(&peer.id.0) {
            return false;
        }
        if folder.included_peers.contains(&peer.id.0) || peer.folders.contains(&filter_id) {
            return true;
        }
        match peer.kind {
            InfoPeerKind::User | InfoPeerKind::SavedMessages => {
                (peer.is_contact && folder.include_contacts)
                    || (!peer.is_contact && folder.include_non_contacts)
            }
            InfoPeerKind::Bot => folder.include_bots,
            InfoPeerKind::Group | InfoPeerKind::Supergroup => folder.include_groups,
            InfoPeerKind::Channel => folder.include_channels,
        }
    }

    fn build_dialog_item(&self, peer: &PeerRecord, list: DialogListQuery) -> DialogItem {
        let last_msg = self.histories.get(&peer.id).and_then(|msgs| msgs.last());
        let (last_message_id, last_message, last_sender, send_state) = match last_msg {
            Some(m) => {
                let sender = if peer.kind.is_group() && !m.outgoing && !m.is_service() {
                    Some(
                        m.sender_name
                            .split_whitespace()
                            .next()
                            .unwrap_or(&m.sender_name)
                            .to_owned(),
                    )
                } else {
                    None
                };
                let state = if m.outgoing {
                    if m.is_sending {
                        DialogSendState::Sending
                    } else if m.is_read {
                        DialogSendState::Received
                    } else {
                        DialogSendState::Sent
                    }
                } else {
                    DialogSendState::None
                };
                (m.id, m.preview_snippet(), sender, state)
            }
            None => (MsgId(0), String::new(), None, DialogSendState::None),
        };

        let draft = self.drafts.get(&peer.id).and_then(|d| {
            if d.text.trim().is_empty() && d.reply_to.is_none() {
                None
            } else {
                Some(DialogDraft {
                    text: d.text.clone(),
                    time: SESSION_NOW,
                    is_reply: d.reply_to.is_some(),
                    saving: false,
                })
            }
        });

        let pinned_order = self
            .pinned_dialog_orders
            .get(&list)
            .and_then(|ord| ord.iter().position(|&id| id == peer.id));
        let pinned = pinned_order.is_some()
            || (list == DialogListQuery::Filter(FilterId::ALL) && peer.pinned);

        let folder_tags: Vec<FolderTagBadge> = self
            .folders
            .iter()
            .filter(|f| self.peer_matches_folder(peer, FilterId(f.id)))
            .take(3)
            .map(|f| FolderTagBadge {
                filter_id: FilterId(f.id),
                title: f.title.clone(),
                color_index: f.color_index.unwrap_or(5),
            })
            .collect();

        DialogItem {
            peer_id: peer.id,
            kind: peer.dialog_kind(),
            special: if peer.kind == InfoPeerKind::SavedMessages {
                SpecialRowKind::SavedMessages
            } else {
                SpecialRowKind::None
            },
            title: peer.title.clone(),
            username: peer.primary_username().map(str::to_owned),
            color_index: peer.color_index,
            verified: peer.verified,
            premium: peer.premium,
            online: peer.is_online,
            active_call: false,
            muted: peer.muted,
            pinned,
            pinned_order,
            archived: peer.archived,
            folders: peer.folders.clone(),
            folder_tags,
            last_message_id,
            last_message,
            last_sender,
            last_message_time: peer.last_message_time,
            send_state,
            draft,
            typing: peer.typing.clone(),
            badges: DialogBadgesState {
                unread_counter: peer.unread_count as i32,
                unread: peer.unread_count > 0 || peer.unread_mark,
                unread_muted: peer.muted,
                mention: peer.unread_mentions > 0,
                ..Default::default()
            },
            is_contact: peer.kind == InfoPeerKind::User && peer.is_contact,
            media_preview: dialog_media_preview(last_msg),
            has_bot_app_button: peer.kind == InfoPeerKind::Bot && peer.has_mini_app,
        }
    }

    fn build_peer_profile(&self, peer: &PeerRecord) -> PeerProfile {
        let linked = peer
            .linked_discussion_peer
            .and_then(|lid| self.peer(lid).map(|lp| (lid, lp.title.clone())));
        let counts = self.compute_shared_media_counts(peer.id);
        PeerProfile {
            id: peer.id,
            kind: peer.kind,
            title: peer.title.clone(),
            status: peer.status.clone(),
            is_online: peer.is_online,
            verified: peer.verified,
            premium: peer.premium,
            color_index: peer.color_index,
            phone: peer.phone.clone(),
            usernames: peer.usernames.clone(),
            bio: peer.bio.clone(),
            birthday: peer.birthday,
            invite_link: peer.invite_link.clone(),
            muted: peer.muted,
            is_contact: peer.is_contact,
            is_blocked: peer.is_blocked,
            can_edit: peer.can_edit,
            can_add_members: peer.can_add_members,
            can_manage_admins: peer.can_manage_admins,
            can_manage_permissions: peer.can_manage_permissions,
            is_public: peer.is_public,
            no_forwards: peer.no_forwards,
            is_forum: peer.is_forum,
            members_count: peer.members_count,
            online_count: peer.online_count,
            admins_count: peer.admins_count,
            default_banned_rights: peer.default_banned_rights,
            slowmode_seconds: peer.slowmode_seconds,
            auto_delete_ttl_seconds: peer.auto_delete_ttl_seconds,
            linked_discussion_peer: linked,
            bot_commands_count: peer.bot_commands_count,
            bot_reads_all_messages: peer.bot_reads_all_messages,
            has_mini_app: peer.has_mini_app,
            media_counts: counts,
            group_call: Default::default(),
        }
    }

    fn compute_shared_media_counts(&self, peer_id: PeerId) -> SharedMediaCounts {
        let mut counts = SharedMediaCounts::default();
        if let Some(msgs) = self.histories.get(&peer_id) {
            for m in msgs {
                match &m.media {
                    Some(HistoryMedia::Photo(_)) => counts.photos_videos += 1,
                    Some(HistoryMedia::Album(items)) => {
                        counts.photos_videos += items.len() as u32;
                    }
                    Some(HistoryMedia::Video(v)) => {
                        if v.is_gif {
                            counts.gifs += 1;
                        } else {
                            counts.photos_videos += 1;
                        }
                    }
                    Some(HistoryMedia::Document(_)) => counts.files += 1,
                    Some(HistoryMedia::Voice(_)) => counts.voice += 1,
                    _ => {}
                }
                if m.text
                    .entities
                    .iter()
                    .any(|e| matches!(e.entity_type(), EntityType::Url | EntityType::CustomUrl))
                {
                    counts.links += 1;
                }
            }
        }
        // Count common groups for user peers.
        counts.common_groups = self
            .members
            .iter()
            .filter(|(_, list)| list.iter().any(|m| m.peer_id == peer_id))
            .count() as u32;
        counts
    }

    fn extract_shared_media(
        &self,
        peer_id: PeerId,
        tab: SharedMediaTab,
        query: &str,
    ) -> Vec<InfoMediaItem> {
        let q = query.trim().to_lowercase();
        let mut out = Vec::new();
        let Some(msgs) = self.histories.get(&peer_id) else {
            return out;
        };
        for m in msgs.iter().rev() {
            match tab {
                SharedMediaTab::PhotosVideos => match &m.media {
                    Some(HistoryMedia::Photo(p)) => {
                        out.push(InfoMediaItem::Photo {
                            id: m.id.0,
                            date_group: "November 2026".to_owned(),
                            width: p.width.max(1) as u32,
                            height: p.height.max(1) as u32,
                            caption: m.text.text.clone(),
                            preview_rgb: p.fallback_rgb.0,
                        });
                    }
                    Some(HistoryMedia::Album(items)) => {
                        for item in items {
                            out.push(InfoMediaItem::Photo {
                                id: item.msg_id.0,
                                date_group: "November 2026".to_owned(),
                                width: item.width.max(1) as u32,
                                height: item.height.max(1) as u32,
                                caption: item.label.clone(),
                                preview_rgb: item.fallback_rgb.0,
                            });
                        }
                    }
                    Some(HistoryMedia::Video(v)) if !v.is_gif => {
                        out.push(InfoMediaItem::Video {
                            id: m.id.0,
                            date_group: "November 2026".to_owned(),
                            duration_secs: v.duration_secs,
                            width: v.width.max(1) as u32,
                            height: v.height.max(1) as u32,
                            caption: m.text.text.clone(),
                            preview_rgb: v.fallback_rgb.0,
                        });
                    }
                    _ => {}
                },
                SharedMediaTab::Files => {
                    if let Some(HistoryMedia::Document(d)) = &m.media {
                        let ext = d
                            .file_name
                            .rsplit('.')
                            .next()
                            .unwrap_or("FILE")
                            .to_uppercase();
                        out.push(InfoMediaItem::File {
                            id: m.id.0,
                            date_group: "November 2026".to_owned(),
                            filename: d.file_name.clone(),
                            size_bytes: d.size_bytes,
                            ext,
                            date_label: m.formatted_time(),
                        });
                    }
                }
                SharedMediaTab::Links => {
                    for ent in &m.text.entities {
                        if matches!(ent.entity_type(), EntityType::Url | EntityType::CustomUrl) {
                            let url = if !ent.data().is_empty() {
                                ent.data().to_owned()
                            } else {
                                extract_utf16_slice(&m.text.text, ent.offset(), ent.length())
                            };
                            let domain = url
                                .strip_prefix("https://")
                                .or_else(|| url.strip_prefix("http://"))
                                .unwrap_or(&url)
                                .split('/')
                                .next()
                                .unwrap_or("link")
                                .to_owned();
                            out.push(InfoMediaItem::Link {
                                id: m.id.0,
                                date_group: "November 2026".to_owned(),
                                url,
                                title: domain.clone(),
                                description: m.preview_snippet(),
                                domain,
                            });
                        }
                    }
                }
                SharedMediaTab::Voice => {
                    if let Some(HistoryMedia::Voice(v)) = &m.media {
                        let bars = tg_media::audio::waveform_decode_5bit(&v.waveform_5bit);
                        out.push(InfoMediaItem::Voice {
                            id: m.id.0,
                            date_group: "November 2026".to_owned(),
                            sender: m.sender_name.clone(),
                            duration_secs: v.duration_secs,
                            waveform: bars,
                            listened: !v.is_unread,
                            date_label: m.formatted_time(),
                        });
                    }
                }
                SharedMediaTab::Music => {}
                SharedMediaTab::Gifs => {
                    if let Some(HistoryMedia::Video(v)) = &m.media
                        && v.is_gif
                    {
                        out.push(InfoMediaItem::Gif {
                            id: m.id.0,
                            date_group: "November 2026".to_owned(),
                            label: v.file_name.clone(),
                            duration_secs: v.duration_secs,
                            preview_rgb: v.fallback_rgb.0,
                        });
                    }
                }
            }
        }
        if !q.is_empty() {
            out.retain(|item| item.matches_query(&q));
        }
        out
    }

    fn resolve_peer_by_username(&self, username: &str) -> Option<PeerId> {
        let norm = username.trim().trim_start_matches('@').to_lowercase();
        if norm.is_empty() {
            return None;
        }
        self.peers
            .iter()
            .find(|p| {
                p.usernames
                    .iter()
                    .any(|u| u.username.to_lowercase() == norm)
            })
            .map(|p| p.id)
    }

    fn append_outgoing_message(
        &mut self,
        peer_id: PeerId,
        request: SendMessageRequest,
    ) -> Result<HistoryMessage, String> {
        let trimmed = request.text.text.trim();
        if trimmed.is_empty() {
            return Err("Cannot send an empty message.".to_owned());
        }
        let Some(idx) = self.peer_index(peer_id) else {
            return Err("Chat not found.".to_owned());
        };
        if !self.peers[idx].can_send {
            return Err("You cannot send messages in this read-only channel.".to_owned());
        }

        let reply = request.reply_to.and_then(|rid| {
            self.histories
                .get(&peer_id)
                .and_then(|list| list.iter().find(|m| m.id == rid))
                .map(|target| ReplyHeader {
                    reply_to_msg_id: rid,
                    author_name: target.sender_name.clone(),
                    author_color_id: target.from_id.bare(),
                    preview_text: target.preview_snippet(),
                    preview_thumb_rgb: None,
                    quote_text: None,
                })
        });

        let msg_id = MsgId(self.next_msg_id);
        self.next_msg_id += 1;
        let msg = HistoryMessage {
            extra: Default::default(),
            id: msg_id,
            peer_id,
            from_id: self.user.id,
            sender_name: self.user.name.clone(),
            date_unix: BASE_UNIX_TODAY + 16_200 + (msg_id.0 % 300),
            outgoing: true,
            is_post: false,
            text: request.text,
            media: None,
            service: None,
            reply,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: false,
            is_sending: false,
            is_pinned: false,
        };
        self.histories.entry(peer_id).or_default().push(msg.clone());
        self.drafts.remove(&peer_id);
        self.peers[idx].unread_count = 0;
        self.peers[idx].unread_mark = false;
        self.peers[idx].unread_mentions = 0;
        self.peers[idx].last_message_time = SESSION_NOW;
        self.first_unread.insert(peer_id, None);
        Ok(msg)
    }

    fn mark_peer_read(&mut self, peer_id: PeerId) {
        if let Some(peer) = self.peer_mut(peer_id) {
            peer.unread_count = 0;
            peer.unread_mark = false;
            peer.unread_mentions = 0;
        }
        if let Some(msgs) = self.histories.get_mut(&peer_id) {
            for m in msgs {
                // Reading our own inbox only marks incoming messages as read; outgoing
                // read checkmarks advance only on outbox read updates from the recipient.
                if !m.outgoing {
                    m.is_read = true;
                }
            }
        }
        self.first_unread.insert(peer_id, None);
        self.notify_events
            .push_back(IncomingNotifyEvent::HistoryRead {
                session_id: 1,
                peer_id,
                read_till_id: MsgId::SHOW_AT_END,
            });
    }

    fn sync_auth_user_from_settings(&mut self) {
        let prof = self.settings_store.profile();
        let full = prof.full_name();
        let initials = initials_for(&full);
        self.user.name = full;
        self.user.username = prof.username.clone();
        self.user.bio = prof.bio.clone();
        self.user.initials = initials;
        if let Some(self_peer) = self.peer_mut(session_peers::SELF) {
            self_peer.bio = prof.bio;
            if prof.username.is_empty() {
                self_peer.usernames.clear();
            } else {
                self_peer.usernames = vec![PeerUsername {
                    username: prof.username,
                    is_editable: true,
                    is_active: true,
                }];
            }
        }
    }

    fn simulate_incoming_message(&mut self, now_unix: i64) -> IncomingMessageNotification {
        let step = self.notify_sim_step;
        self.notify_sim_step = self.notify_sim_step.wrapping_add(1);
        let samples: [(PeerId, PeerId, &str, bool); 3] = [
            (
                session_peers::USER_MIRA,
                session_peers::USER_MIRA,
                "Just pushed the unified navigation wiring — check out the third column!",
                false,
            ),
            (
                session_peers::GROUP_CORE,
                session_peers::USER_SAM,
                "@taylor_kim All navigation unit tests are green on Windows x64!",
                true,
            ),
            (
                session_peers::USER_THEO,
                session_peers::USER_THEO,
                "Did you try clicking the Instant View link in Rustgram Dev Log?",
                false,
            ),
        ];
        let (peer_id, sender_id, text, mentions_me) = samples[step % samples.len()];
        let msg_id = MsgId(self.next_msg_id);
        self.next_msg_id += 1;

        let sender_name = self
            .peer(sender_id)
            .map_or_else(|| "Contact".to_owned(), |p| p.title.clone());
        let (chat_title, peer_kind) = self.peer(peer_id).map_or_else(
            || ("Chat".to_owned(), PeerKind::User),
            |p| (p.title.clone(), p.coarse_kind()),
        );

        let msg = HistoryMessage {
            extra: Default::default(),
            id: msg_id,
            peer_id,
            from_id: sender_id,
            sender_name: sender_name.clone(),
            date_unix: now_unix.clamp(0, i64::from(i32::MAX)) as i32,
            outgoing: false,
            is_post: false,
            text: TextWithEntities::simple(text),
            media: None,
            service: None,
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: false,
            is_sending: false,
            is_pinned: false,
        };
        self.histories.entry(peer_id).or_default().push(msg);
        if let Some(peer) = self.peer_mut(peer_id) {
            peer.unread_count = peer.unread_count.saturating_add(1);
            if mentions_me {
                peer.unread_mentions = peer.unread_mentions.saturating_add(1);
            }
            peer.last_message_time = SESSION_NOW;
        }
        self.first_unread.entry(peer_id).or_insert(Some(msg_id));

        let notif = IncomingMessageNotification {
            session_id: 1,
            peer_id,
            peer_kind,
            topic_root_id: MsgId(0),
            monoforum_peer_id: PeerId::ZERO,
            msg_id,
            notification_type: ItemNotificationType::Message,
            date_unix: now_unix,
            sender_id,
            sender_name,
            chat_title,
            topic_title: None,
            text: text.to_owned(),
            spoiler_ranges: Vec::new(),
            media_kind: NotificationMediaKind::Text,
            outgoing: false,
            silent: false,
            mentions_me,
            is_reply_to_me: false,
            is_pinned: false,
            is_from_scheduled: false,
            is_contact_signup: false,
            is_imported_forward: false,
            is_forwarded: false,
            group_id: 0,
            is_self_chat: false,
            is_broadcast: peer_kind == PeerKind::Channel,
            is_notifications_service: false,
            can_send_texts: true,
            slowmode_seconds_left: 0,
            stars_per_message: 0,
            reaction_emoji: None,
            poll_vote_option: None,
            sender_blocked: false,
            am_in_chat: true,
        };
        self.notify_events
            .push_back(IncomingNotifyEvent::Notification(Box::new(notif.clone())));
        notif
    }
}

/// Lightweight clonable handle into a [`MockSession`]'s shared mutable state.
///
/// Implements every owned-return screen backend trait (`DialogsBackend`, `HistoryBackend`,
/// `InfoBackend`, `SettingsBackend`, `MediaBackend`, `NotifyBackend`, `WebViewBackend`,
/// `StatsBackend`) so widgets that take `Box<dyn ...Backend>` share the exact same live
/// data with [`MockSession`].
#[derive(Clone)]
pub struct SharedSessionHandle {
    state: Arc<Mutex<MockSessionState>>,
    media_base: Arc<MockMediaBackend>,
}

impl SharedSessionHandle {
    /// Sets the active peer target used when [`StatsBackend`] screens load.
    pub fn set_stats_target_peer(&self, peer_id: PeerId) {
        if let Ok(mut st) = self.state.lock() {
            st.stats_target_peer = peer_id;
        }
    }

    /// Resolves a username (`@rustgram_log` or `rustgram_log`) to its [`PeerId`].
    #[must_use]
    pub fn resolve_username(&self, username: &str) -> Option<PeerId> {
        self.state
            .lock()
            .ok()
            .and_then(|st| st.resolve_peer_by_username(username))
    }

    /// Resolves a message `(peer_id, msg_id)` or shared media item id to a [`MediaItemId`].
    #[must_use]
    pub fn resolve_media_item_id(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        album_index: Option<usize>,
    ) -> MediaItemId {
        if let Ok(st) = self.state.lock() {
            if let Some(idx) = album_index {
                let ordered = [
                    MOCK_PHOTO_LANDSCAPE_ID,
                    MOCK_PHOTO_PORTRAIT_ID,
                    MOCK_PHOTO_AURORA_ID,
                ];
                return ordered[idx % ordered.len()];
            }
            if let Some(&id) = st.msg_media_map.get(&(peer_id, msg_id)) {
                return id;
            }
            if let Some(item) = st.media_items.iter().find(|it| it.msg_id == msg_id.0) {
                return item.id;
            }
        }
        MOCK_PHOTO_LANDSCAPE_ID
    }

    /// Drains any pending notification click navigations (`open_chat` calls).
    pub fn take_pending_chat_opens(&self) -> Vec<PendingChatOpen> {
        self.state
            .lock()
            .map(|mut st| std::mem::take(&mut st.pending_chat_opens))
            .unwrap_or_default()
    }

    /// Triggers a simulated incoming message into the shared session (appears in History, Dialogs,
    /// and Notifications simultaneously).
    pub fn simulate_incoming_message(&self, now_unix: i64) -> Option<IncomingMessageNotification> {
        self.state
            .lock()
            .ok()
            .map(|mut st| st.simulate_incoming_message(now_unix))
    }

    /// Returns a sample playable audio track ID from the shared media store.
    #[must_use]
    pub fn sample_audio_id(&self) -> Option<AudioMsgId> {
        self.media_base.sample_song_ids().first().copied()
    }

    /// Returns a sample playable voice note ID from the shared media store.
    #[must_use]
    pub fn sample_voice_id(&self) -> Option<AudioMsgId> {
        self.media_base.sample_voice_ids().first().copied()
    }

    /// Appends a sticker or GIF message from `chat_helpers` into `peer_id`.
    pub fn send_helper_message(
        &self,
        peer_id: PeerId,
        text: String,
    ) -> Result<HistoryMessage, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.append_outgoing_message(
            peer_id,
            SendMessageRequest {
                text: TextWithEntities::simple(text),
                reply_to: None,
                silent: false,
            },
        )
    }

    /// Enqueues a mock outgoing media upload for `peer_id` (used by demo-mode progress bar tests).
    pub fn push_pending_upload(&self, upload: tg_app_history::history::backend::MediaUploadStatus) {
        if let Ok(mut st) = self.state.lock() {
            st.pending_uploads.push(upload);
        }
    }

    /// Inserts a custom [`HistoryMessage`] directly into `peer_id`'s demo history.
    pub fn push_history_message(&self, peer_id: PeerId, msg: HistoryMessage) {
        if let Ok(mut st) = self.state.lock() {
            st.histories.entry(peer_id).or_default().push(msg);
        }
    }

    /// Returns the self birthday stored in `settings_store` (used by WP-51 suggestion card tests).
    #[must_use]
    pub fn self_birthday(&self) -> Option<(u8, u8, Option<u16>)> {
        self.state.lock().ok().and_then(|st| {
            SettingsBackend::profile(&st.settings_store)
                .birthday
                .map(|b| (b.day, b.month, b.year))
        })
    }

    /// Returns the last suggested profile photo id applied via `apply_suggested_profile_photo`.
    #[must_use]
    pub fn applied_userpic_photo_id(&self) -> Option<i64> {
        self.state
            .lock()
            .ok()
            .and_then(|st| st.applied_userpic_photo_id)
    }

    /// Configures unread mentions, reactions, or poll-vote message IDs for `peer_id` (WP-61).
    pub fn set_unread_things(
        &self,
        peer_id: PeerId,
        kind: tg_app_core::core_types::dto::engagement::UnreadThingsKind,
        msg_ids: Vec<MsgId>,
    ) {
        if let Ok(mut st) = self.state.lock() {
            let map = match kind {
                tg_app_core::core_types::dto::engagement::UnreadThingsKind::Mentions => {
                    &mut st.unread_mentions
                }
                tg_app_core::core_types::dto::engagement::UnreadThingsKind::Reactions => {
                    &mut st.unread_reactions
                }
                tg_app_core::core_types::dto::engagement::UnreadThingsKind::PollVotes => {
                    &mut st.unread_poll_votes
                }
            };
            if msg_ids.is_empty() {
                map.remove(&peer_id);
            } else {
                map.insert(peer_id, msg_ids);
            }
        }
    }

    /// Overrides the candidate future owner returned by `future_chat_creator_after_leave`.
    pub fn set_future_chat_creator(
        &self,
        peer_id: PeerId,
        creator: Option<tg_app_core::core_types::dto::engagement::FutureChatCreatorInfo>,
    ) {
        if let Ok(mut st) = self.state.lock() {
            match creator {
                Some(c) => {
                    st.future_creators.insert(peer_id, c);
                }
                None => {
                    st.future_creators.remove(&peer_id);
                }
            }
        }
    }

    /// Returns all `messages.startBot` calls recorded by `BotsBackend::start_bot`.
    #[must_use]
    pub fn started_bots(&self) -> Vec<tg_bots::protocol::StartBotParams> {
        self.state
            .lock()
            .map(|st| st.started_bots.clone())
            .unwrap_or_default()
    }

    /// Returns all `(phone_code_hash, phone_code)` pairs submitted via `confirm_phone_code`.
    #[must_use]
    pub fn confirmed_phone_codes(&self) -> Vec<(String, String)> {
        self.state
            .lock()
            .map(|st| st.confirmed_phone_codes.clone())
            .unwrap_or_default()
    }

    /// Configures contact birthday entries returned by `DialogsBackend::contact_birthdays`.
    pub fn set_contact_birthdays(
        &self,
        entries: Vec<tg_app_core::core_types::dto::contacts_extras::ContactBirthdayEntry>,
    ) {
        if let Ok(mut st) = self.state.lock() {
            st.contact_birthdays = entries;
        }
    }

    /// Configures sponsored peer search results returned by `DialogsBackend::sponsored_peers`.
    pub fn set_sponsored_peers(
        &self,
        peers: Vec<tg_app_core::core_types::dto::contacts_extras::SponsoredPeerInfo>,
    ) {
        if let Ok(mut st) = self.state.lock() {
            st.sponsored_peers = peers;
        }
    }

    /// Configures active promoted dialog metadata returned by `DialogsBackend::promo_data`.
    pub fn set_promo_data(&self, promo: Option<tg_app_core::core_types::misc::PromoDataSnapshot>) {
        if let Ok(mut st) = self.state.lock() {
            st.promo_data = promo;
        }
    }

    /// Returns peer IDs passed to `DialogsBackend::hide_promo_data`.
    #[must_use]
    pub fn hidden_promo_peers(&self) -> Vec<PeerId> {
        self.state
            .lock()
            .map(|st| st.hidden_promo_peers.clone())
            .unwrap_or_default()
    }

    /// Returns requests recorded by `HistoryBackend::block_from_replies`.
    #[must_use]
    pub fn blocked_from_replies(
        &self,
    ) -> Vec<tg_app_core::core_types::dto::contacts_extras::BlockFromRepliesRequest> {
        self.state
            .lock()
            .map(|st| st.blocked_from_replies.clone())
            .unwrap_or_default()
    }

    /// Returns the list of visible peer IDs in `list` order (for keyboard chat cycling).
    #[must_use]
    pub fn ordered_peer_ids(&self, list: DialogListQuery) -> Vec<PeerId> {
        self.dialog_slice(list, 0, 200)
            .entries
            .into_iter()
            .map(|d| d.peer_id)
            .collect()
    }

    /// Returns `true` when `peer_id` is a forum supergroup with topics enabled.
    #[must_use]
    pub fn is_forum_peer(&self, peer_id: PeerId) -> bool {
        self.state
            .lock()
            .ok()
            .and_then(|st| st.peer(peer_id).map(|p| p.is_forum))
            .unwrap_or(false)
    }

    /// Queues an explicit Mini App / Game launch URL for `bot_id` so the next `request_web_view`
    /// call for that bot resolves to it even when the caller omits `url_override`.
    pub fn set_pending_mini_app_url(&self, bot_id: PeerId, url: Option<String>) {
        if let Ok(mut st) = self.state.lock() {
            let bare = bot_id.bare();
            if let Some(u) = url {
                st.pending_mini_app_urls.insert(bare, u);
            } else {
                st.pending_mini_app_urls.remove(&bare);
            }
        }
    }

    /// Takes any queued Mini App / Game launch URL for `bot_id`.
    #[must_use]
    pub fn take_pending_mini_app_url(&self, bot_id: PeerId) -> Option<String> {
        self.state
            .lock()
            .ok()
            .and_then(|mut st| st.pending_mini_app_urls.remove(&bot_id.bare()))
    }

    /// Whether `bot_id` was confirmed for Mini Apps (`isPeerTrustedOpenWebView`).
    #[must_use]
    pub fn is_mini_app_bot_trusted(&self, bot_id: PeerId) -> bool {
        self.state
            .lock()
            .is_ok_and(|st| st.trusted_mini_app_bots.contains(&bot_id))
    }

    /// Records `bot_id` as confirmed for Mini Apps (`markPeerTrustedOpenWebView`).
    pub fn trust_mini_app_bot(&self, bot_id: PeerId) {
        if let Ok(mut st) = self.state.lock() {
            st.trusted_mini_app_bots.insert(bot_id);
        }
    }

    /// Returns a snapshot of every [`WebViewRequest`] received by `request_web_view`.
    #[must_use]
    pub fn webview_requests_log(&self) -> Vec<WebViewRequest> {
        self.state
            .lock()
            .map(|st| st.webview_requests_log.clone())
            .unwrap_or_default()
    }

    /// Adds an active collectible username to `peer_id` (used by tests for
    /// `channels.deactivateAllUsernames`).
    pub fn seed_collectible_username(&self, peer_id: PeerId, username: impl Into<String>) {
        if let Ok(mut st) = self.state.lock()
            && let Some(peer) = st.peer_mut(peer_id)
        {
            peer.usernames.push(PeerUsername {
                username: username.into(),
                is_editable: false,
                is_active: true,
            });
        }
    }
}

/// Unified mock session implementing EVERY screen backend trait over ONE consistent dataset:
///
/// - [`Backend`] (shell & intro)
/// - [`DialogsBackend`] (`dialogs/`)
/// - [`HistoryBackend`] (`history/`)
/// - [`InfoBackend`] (`info/` & `boxes/`)
/// - [`SettingsBackend`] (`settings/`)
/// - [`MediaBackend`] (`media/`)
/// - [`ChatHelpersBackend`] (`chat_helpers/`)
/// - [`NotifyBackend`] (`notifications/` & `tray/`)
/// - [`WebViewBackend`] (`webview/`)
/// - [`StatsBackend`] (`statistics/`)
pub struct MockSession {
    handle: SharedSessionHandle,
    cached_user: AuthUser,
    cached_signed_in: bool,
    chat_helpers: MockChatHelpersBackend,
    calls: MockCallsBackend,
    stories: MockStoriesBackend,
    business: MockBusinessBackend,
    payments: MockPaymentsBackend,
    passport: MockPassportBackend,
    compose_ai: tg_app_info::boxes::compose_ai::MockComposeAiStore,
    sticker_creator: tg_app_info::boxes::sticker_creator::MockStickerCreatorStore,
    self_destruct: tg_app_info::boxes::self_destruct::MockSelfDestructStore,
    autocomplete_users: Vec<UserInfo>,
    autocomplete_last_authors: Vec<UserId>,
    autocomplete_bot_commands: HashMap<UserId, Vec<BotCommand>>,
    recent_inline_bots: Vec<UserId>,
}

impl Default for MockSession {
    fn default() -> Self {
        Self::new(false)
    }
}

impl MockSession {
    /// Creates a new unified [`MockSession`]. Pass `start_signed_in = true` for `--start-main`.
    #[must_use]
    pub fn new(start_signed_in: bool) -> Self {
        let state = MockSessionState::new(start_signed_in);
        let cached_user = state.user.clone();
        let make_ac_user = |peer: PeerId,
                            name: &str,
                            username: &str,
                            lastseen: LastseenStatus,
                            is_bot: bool,
                            inline_placeholder: &str|
         -> UserInfo {
            let id = peer.bare();
            UserInfo {
                id: UserId(id),
                access_hash: id * 1_009 + 17,
                first_name: name.to_owned(),
                display_name: name.to_owned(),
                username: username.to_owned(),
                usernames: if username.is_empty() {
                    Vec::new()
                } else {
                    vec![username.to_owned()]
                },
                name_words: name.split_whitespace().map(str::to_lowercase).collect(),
                is_bot,
                lastseen,
                inline_placeholder: inline_placeholder.to_owned(),
                ..UserInfo::default()
            }
        };
        let autocomplete_users = vec![
            make_ac_user(
                session_peers::USER_MIRA,
                "Mira Vance",
                "mira_vance",
                LastseenStatus::online_till_status(BASE_UNIX_TODAY + 3600, false, false),
                false,
                "",
            ),
            make_ac_user(
                session_peers::USER_SAM,
                "Sam Chen",
                "sam_chen",
                LastseenStatus::online_till_status(BASE_UNIX_TODAY + 3600, false, false),
                false,
                "",
            ),
            make_ac_user(
                session_peers::USER_THEO,
                "Theo Park",
                "theo_park",
                LastseenStatus::recently(false),
                false,
                "",
            ),
            make_ac_user(
                session_peers::USER_RILEY,
                "Riley Brooks",
                "riley_brooks",
                LastseenStatus::recently(false),
                false,
                "",
            ),
            make_ac_user(
                session_peers::BOT_CATALOG,
                "Catalog Bot",
                "catalog_demo_bot",
                LastseenStatus::long_ago(false),
                true,
                "Search catalog items...",
            ),
        ];
        let autocomplete_last_authors = vec![
            UserId(session_peers::USER_MIRA.bare()),
            UserId(session_peers::USER_SAM.bare()),
            UserId(session_peers::USER_THEO.bare()),
        ];
        let mut autocomplete_bot_commands = HashMap::new();
        autocomplete_bot_commands.insert(
            UserId(session_peers::BOT_CATALOG.bare()),
            vec![
                BotCommand {
                    command: "start".to_owned(),
                    description: "Open the Catalog Bot welcome menu".to_owned(),
                },
                BotCommand {
                    command: "catalog".to_owned(),
                    description: "Launch the Catalog Mini App".to_owned(),
                },
                BotCommand {
                    command: "help".to_owned(),
                    description: "Show available Catalog Bot commands".to_owned(),
                },
            ],
        );
        let recent_inline_bots = vec![UserId(session_peers::BOT_CATALOG.bare())];

        Self {
            handle: SharedSessionHandle {
                state: Arc::new(Mutex::new(state)),
                media_base: Arc::new(MockMediaBackend::new()),
            },
            cached_user,
            cached_signed_in: start_signed_in,
            chat_helpers: MockChatHelpersBackend::new(),
            calls: MockCallsBackend::new(),
            stories: MockStoriesBackend::new(BASE_UNIX_TODAY),
            business: MockBusinessBackend::new(),
            payments: MockPaymentsBackend::new(),
            passport: MockPassportBackend::new(),
            compose_ai: tg_app_info::boxes::compose_ai::MockComposeAiStore::new(),
            sticker_creator: tg_app_info::boxes::sticker_creator::MockStickerCreatorStore::new(),
            self_destruct: tg_app_info::boxes::self_destruct::MockSelfDestructStore::new(),
            autocomplete_users,
            autocomplete_last_authors,
            autocomplete_bot_commands,
            recent_inline_bots,
        }
    }

    /// Returns a clonable [`SharedSessionHandle`] sharing this session's live state.
    #[must_use]
    pub fn handle(&self) -> SharedSessionHandle {
        self.handle.clone()
    }

    /// Returns mutable access to the embedded [`MockChatHelpersBackend`] (for tests).
    pub fn chat_helpers_mut(&mut self) -> &mut MockChatHelpersBackend {
        &mut self.chat_helpers
    }

    /// Returns `true` when `peer_id` is a forum supergroup with topics enabled.
    #[must_use]
    pub fn is_forum_peer(&self, peer_id: PeerId) -> bool {
        self.handle.is_forum_peer(peer_id)
    }

    /// Synchronizes cached `AuthUser` & sign-in flag after edits made through a shared handle.
    pub fn sync_cached_user(&mut self) {
        if let Ok(st) = self.handle.state.lock() {
            self.cached_signed_in = st.signed_in;
            self.cached_user = st.user.clone();
        }
    }

    /// Signs in immediately using an authorized [`AuthUser`] produced by [`tg_app_settings::intro::IntroWidget`].
    pub fn sign_in_as(&mut self, user: AuthUser) {
        if let Ok(mut st) = self.handle.state.lock() {
            st.signed_in = true;
            if !user.name.trim().is_empty() {
                st.user.name = user.name;
                st.user.initials = initials_for(&st.user.name);
            }
            if !user.phone.trim().is_empty() {
                st.user.phone = user.phone;
            }
            self.cached_user = st.user.clone();
            self.cached_signed_in = true;
        }
    }

    /// Clears all messages in `peer_id`'s history (used by tests to exercise empty bot intro).
    pub fn clear_peer_history(&mut self, peer_id: PeerId) {
        if let Ok(mut st) = self.handle.state.lock() {
            st.histories.entry(peer_id).or_default().clear();
            st.pinned_messages.remove(&peer_id);
        }
    }

    /// Updates a bot peer's compose menu button (`Commands` vs `WebApp`).
    pub fn set_bot_menu_button(
        &mut self,
        peer_id: PeerId,
        button: tg_bots::commands::BotMenuButton,
    ) {
        if let Ok(mut st) = self.handle.state.lock()
            && let Some(info) = st.bot_info_map.get_mut(&peer_id)
        {
            info.apply_menu_button(button);
        }
    }

    /// Replaces the reply keyboard markup on `peer_id`'s latest bot message (useful for testing
    /// `single_use`, `resize`, `persistent`, and `placeholder` modes).
    pub fn set_bot_reply_keyboard(
        &mut self,
        peer_id: PeerId,
        markup: tg_bots::markup::ReplyMarkupData,
    ) {
        if let Ok(mut st) = self.handle.state.lock()
            && let Some(msgs) = st.histories.get_mut(&peer_id)
            && let Some(msg) = msgs.iter_mut().rev().find(|m| !m.outgoing)
        {
            msg.extra.reply_markup = Some(markup);
        }
    }

    /// Queues an explicit Mini App / Game launch URL for `bot_id` on the shared session state.
    pub fn set_pending_mini_app_url(&self, bot_id: PeerId, url: Option<String>) {
        self.handle.set_pending_mini_app_url(bot_id, url);
    }

    /// Takes any queued Mini App / Game launch URL for `bot_id`.
    #[must_use]
    pub fn take_pending_mini_app_url(&self, bot_id: PeerId) -> Option<String> {
        self.handle.take_pending_mini_app_url(bot_id)
    }

    /// Returns all [`WebViewRequest`]s processed via [`WebViewBackend::request_web_view`].
    #[must_use]
    pub fn webview_requests_log(&self) -> Vec<WebViewRequest> {
        self.handle.webview_requests_log()
    }
}

// =============================================================================
// 1. stand_in::backend::Backend
// =============================================================================

impl Backend for MockSession {
    fn current_user(&self) -> Option<&AuthUser> {
        self.cached_signed_in.then_some(&self.cached_user)
    }

    fn send_login_code(&mut self, phone: &str) -> Result<CodeSentInfo, String> {
        let digits: String = phone.chars().filter(char::is_ascii_digit).collect();
        if digits.len() < 4 {
            return Err("Please enter at least 4 test digits (e.g. +999 661 0000).".to_owned());
        }
        let normalized = phone.trim().to_owned();
        let mut st = self
            .handle
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.user.phone = normalized.clone();
        self.cached_user.phone = normalized.clone();
        Ok(CodeSentInfo {
            phone: normalized,
            code_length: st.expected_code.len(),
            delivery_hint: format!(
                "Mock mode: enter code {} (2FA password: \"{}\").",
                st.expected_code, st.expected_password
            ),
        })
    }

    fn verify_login_code(&mut self, code: &str) -> Result<CodeVerifyOutcome, String> {
        let trimmed = code.trim();
        let mut st = self
            .handle
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        if trimmed != st.expected_code {
            return Err(format!("Invalid code. Enter {}.", st.expected_code));
        }
        if st.require_2fa {
            Ok(CodeVerifyOutcome::PasswordRequired {
                hint: format!("Mock hint: type \"{}\"", st.expected_password),
            })
        } else {
            st.signed_in = true;
            self.cached_signed_in = true;
            self.cached_user = st.user.clone();
            Ok(CodeVerifyOutcome::SignedIn(st.user.clone()))
        }
    }

    fn verify_cloud_password(&mut self, password: &str) -> Result<AuthUser, String> {
        let mut st = self
            .handle
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        if password != st.expected_password {
            return Err(format!(
                "Incorrect password. Enter \"{}\".",
                st.expected_password
            ));
        }
        st.signed_in = true;
        self.cached_signed_in = true;
        self.cached_user = st.user.clone();
        Ok(st.user.clone())
    }

    fn logout(&mut self) {
        if let Ok(mut st) = self.handle.state.lock() {
            st.signed_in = false;
        }
        self.cached_signed_in = false;
    }

    fn folders(&self) -> Vec<FolderFilter> {
        self.chat_folders()
            .into_iter()
            .map(|f| FolderFilter {
                id: f.id,
                title: f.title,
                icon_glyph: f.icon.glyph().to_owned(),
                unread_count: f.unread.unread_chats,
            })
            .collect()
    }

    fn dialogs(&self, folder: FilterId, query: &str) -> Vec<DialogEntry> {
        let q = query.trim().to_lowercase();
        let slice = self.dialog_slice(DialogListQuery::Filter(folder), 0, 200);
        let Ok(st) = self.handle.state.lock() else {
            return Vec::new();
        };
        slice
            .entries
            .into_iter()
            .filter_map(|item| {
                let rec = st.peer(item.peer_id)?;
                if !q.is_empty() {
                    let hit = rec.title.to_lowercase().contains(&q)
                        || item.last_message.to_lowercase().contains(&q)
                        || rec
                            .primary_username()
                            .is_some_and(|u| u.to_lowercase().contains(&q));
                    if !hit {
                        return None;
                    }
                }
                Some(DialogEntry {
                    peer: rec.to_peer_summary(),
                    last_message: item.last_message,
                    last_sender: item.last_sender,
                    timestamp: format!(
                        "{:02}:{:02}",
                        item.last_message_time.hour, item.last_message_time.minute
                    ),
                    unread_count: rec.unread_count,
                    pinned: item.pinned,
                    folders: rec.folders.clone(),
                })
            })
            .collect()
    }

    fn peer_summary(&self, peer_id: PeerId) -> Option<PeerSummary> {
        self.handle
            .state
            .lock()
            .ok()
            .and_then(|st| st.peer(peer_id).map(PeerRecord::to_peer_summary))
    }

    fn history_slice(&self, peer_id: PeerId) -> Vec<MessageItem> {
        let Ok(st) = self.handle.state.lock() else {
            return Vec::new();
        };
        st.histories
            .get(&peer_id)
            .map(|list| {
                list.iter()
                    .map(|m| MessageItem {
                        id: m.id,
                        peer_id: m.peer_id,
                        sender_name: m.sender_name.clone(),
                        outgoing: m.outgoing,
                        text: m.text.text.clone(),
                        timestamp: m.formatted_time(),
                        is_read: m.is_read,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn send_message(&mut self, peer_id: PeerId, text: &str) -> Result<MessageItem, String> {
        let msg = HistoryBackend::send_message(
            self,
            peer_id,
            SendMessageRequest {
                text: TextWithEntities::simple(text),
                reply_to: None,
                silent: false,
            },
        )?;
        Ok(MessageItem {
            id: msg.id,
            peer_id: msg.peer_id,
            sender_name: msg.sender_name.clone(),
            outgoing: msg.outgoing,
            text: msg.text.text.clone(),
            timestamp: msg.formatted_time(),
            is_read: msg.is_read,
        })
    }

    fn mark_read(&mut self, peer_id: PeerId) {
        if let Ok(mut st) = self.handle.state.lock() {
            st.mark_peer_read(peer_id);
        }
    }

    fn toggle_mute(&mut self, peer_id: PeerId) -> bool {
        self.handle.toggle_mute_dialog(peer_id)
    }

    fn connection_status(&self) -> ConnectionStatus {
        self.handle
            .state
            .lock()
            .map_or(ConnectionStatus::Connected, |st| st.connection_status)
    }

    fn proxy_enabled(&self) -> bool {
        self.handle.state.lock().is_ok_and(|st| st.proxy_enabled)
    }

    fn set_connection_status(&mut self, status: ConnectionStatus) {
        if let Ok(mut st) = self.handle.state.lock() {
            st.connection_status = status;
        }
    }

    fn set_proxy_enabled(&mut self, enabled: bool) {
        if let Ok(mut st) = self.handle.state.lock() {
            st.proxy_enabled = enabled;
        }
    }

    fn retry_connection(&mut self) {
        if let Ok(mut st) = self.handle.state.lock() {
            st.connection_status = ConnectionStatus::Connected;
        }
    }
}

// =============================================================================
// 2. dialogs::backend::DialogsBackend
// =============================================================================

impl DialogsBackend for SharedSessionHandle {
    fn current_time(&self) -> LocalDateTime {
        SESSION_NOW
    }

    fn dialog_slice(&self, list: DialogListQuery, offset: usize, limit: usize) -> DialogSlice {
        let Ok(st) = self.state.lock() else {
            return DialogSlice {
                entries: Vec::new(),
                total: 0,
                pinned_count: 0,
                has_more: false,
            };
        };

        let mut matching: Vec<DialogItem> = st
            .peers
            .iter()
            .filter(|p| match list {
                DialogListQuery::Archive => p.archived,
                DialogListQuery::Filter(fid) => st.peer_matches_folder(p, fid),
            })
            .map(|p| st.build_dialog_item(p, list))
            .collect();

        // Sort pinned first (preserving explicit pin order), then newest last message time.
        matching.sort_by(|a, b| match (a.pinned, b.pinned) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            (true, true) => a
                .pinned_order
                .unwrap_or(usize::MAX)
                .cmp(&b.pinned_order.unwrap_or(usize::MAX)),
            (false, false) => b
                .display_time()
                .to_local_epoch_secs()
                .cmp(&a.display_time().to_local_epoch_secs()),
        });

        let pinned_count = matching.iter().filter(|d| d.pinned).count();
        let total = matching.len();
        let start = offset.min(total);
        let end = start.saturating_add(limit).min(total);
        let entries = matching[start..end].to_vec();
        DialogSlice {
            entries,
            total,
            pinned_count,
            has_more: end < total,
        }
    }

    fn is_peer_blocked(&self, peer_id: PeerId) -> bool {
        self.state
            .lock()
            .ok()
            .and_then(|st| st.peer(peer_id).map(|p| p.is_blocked))
            .unwrap_or(false)
    }

    fn dialog_by_peer(&self, peer_id: PeerId) -> Option<DialogItem> {
        let st = self.state.lock().ok()?;
        let peer = st.peer(peer_id)?;
        let list = if peer.archived {
            DialogListQuery::Archive
        } else {
            DialogListQuery::Filter(FilterId::ALL)
        };
        Some(st.build_dialog_item(peer, list))
    }

    fn chat_folders(&self) -> Vec<DialogChatFolder> {
        let Ok(st) = self.state.lock() else {
            return Vec::new();
        };

        let compute_unread = |fid: FilterId| -> FolderUnreadState {
            let mut unread_chats = 0u32;
            let mut muted_chats = 0u32;
            for p in &st.peers {
                if st.peer_matches_folder(p, fid) && (p.unread_count > 0 || p.unread_mark) {
                    unread_chats += 1;
                    if p.muted {
                        muted_chats += 1;
                    }
                }
            }
            FolderUnreadState {
                unread_chats,
                muted_chats,
            }
        };

        let mut out = vec![DialogChatFolder {
            id: FilterId::ALL,
            title: "All Chats".to_owned(),
            icon: FolderIconKind::All,
            color_index: 5,
            unread: compute_unread(FilterId::ALL),
            locked: false,
        }];

        for f in &st.folders {
            let fid = FilterId(f.id);
            out.push(DialogChatFolder {
                id: fid,
                title: f.title.clone(),
                icon: map_folder_icon(f.icon),
                color_index: f.color_index.unwrap_or(5),
                unread: compute_unread(fid),
                locked: false,
            });
        }
        out
    }

    fn reorder_folders(&mut self, order: &[FilterId]) {
        let Ok(mut st) = self.state.lock() else {
            return;
        };
        let mut reordered = Vec::with_capacity(st.folders.len());
        for &fid in order {
            if fid == FilterId::ALL {
                continue;
            }
            if let Some(pos) = st.folders.iter().position(|f| f.id == fid.0) {
                reordered.push(st.folders.remove(pos));
            }
        }
        reordered.append(&mut st.folders);
        st.folders = reordered;
    }

    fn add_peer_to_folder(&mut self, peer_id: PeerId, filter_id: FilterId) {
        if let Ok(mut st) = self.state.lock() {
            if let Some(p) = st.peer_mut(peer_id)
                && !p.folders.contains(&filter_id)
            {
                p.folders.push(filter_id);
            }
            if let Some(f) = st.folders.iter_mut().find(|f| f.id == filter_id.0)
                && !f.included_peers.contains(&peer_id.0)
            {
                f.included_peers.push(peer_id.0);
            }
        }
    }

    fn remove_peer_from_folder(&mut self, peer_id: PeerId, filter_id: FilterId) {
        if let Ok(mut st) = self.state.lock() {
            if let Some(p) = st.peer_mut(peer_id) {
                p.folders.retain(|&id| id != filter_id);
            }
            if let Some(f) = st.folders.iter_mut().find(|f| f.id == filter_id.0) {
                f.included_peers.retain(|&id| id != peer_id.0);
            }
        }
    }

    fn mark_folder_read(&mut self, filter_id: FilterId) {
        if let Ok(mut st) = self.state.lock() {
            let ids: Vec<PeerId> = st
                .peers
                .iter()
                .filter(|p| st.peer_matches_folder(p, filter_id))
                .map(|p| p.id)
                .collect();
            for pid in ids {
                st.mark_peer_read(pid);
            }
        }
    }

    fn archive_summary(&self) -> Option<ArchiveSummary> {
        let st = self.state.lock().ok()?;
        let archived: Vec<&PeerRecord> = st.peers.iter().filter(|p| p.archived).collect();
        if archived.is_empty() {
            return None;
        }
        let unread_count: u32 = archived
            .iter()
            .filter(|p| p.unread_count > 0 || p.unread_mark)
            .count() as u32;
        let all_muted = archived
            .iter()
            .filter(|p| p.unread_count > 0 || p.unread_mark)
            .all(|p| p.muted);
        let preview_titles = archived.iter().take(3).map(|p| p.title.clone()).collect();
        Some(ArchiveSummary {
            chat_count: archived.len(),
            unread_count,
            all_muted,
            preview_titles,
            collapsed: st.archive_collapsed,
        })
    }

    fn archive_collapsed(&self) -> bool {
        self.state.lock().is_ok_and(|st| st.archive_collapsed)
    }

    fn set_archive_collapsed(&mut self, collapsed: bool) {
        if let Ok(mut st) = self.state.lock() {
            st.archive_collapsed = collapsed;
        }
    }

    fn toggle_archive(&mut self, peer_id: PeerId) -> bool {
        let Ok(mut st) = self.state.lock() else {
            return false;
        };
        if let Some(p) = st.peer_mut(peer_id) {
            p.archived = !p.archived;
            if p.archived {
                p.pinned = false;
            }
            return p.archived;
        }
        false
    }

    fn mark_archive_read(&mut self) {
        if let Ok(mut st) = self.state.lock() {
            let ids: Vec<PeerId> = st
                .peers
                .iter()
                .filter(|p| p.archived)
                .map(|p| p.id)
                .collect();
            for pid in ids {
                st.mark_peer_read(pid);
            }
        }
    }

    fn toggle_pin(&mut self, peer_id: PeerId, list: DialogListQuery) -> bool {
        let Ok(mut st) = self.state.lock() else {
            return false;
        };
        let order = st.pinned_dialog_orders.entry(list).or_default();
        if let Some(pos) = order.iter().position(|&id| id == peer_id) {
            order.remove(pos);
            if list == DialogListQuery::Filter(FilterId::ALL)
                && let Some(p) = st.peer_mut(peer_id)
            {
                p.pinned = false;
            }
            false
        } else {
            order.insert(0, peer_id);
            if list == DialogListQuery::Filter(FilterId::ALL)
                && let Some(p) = st.peer_mut(peer_id)
            {
                p.pinned = true;
            }
            true
        }
    }

    fn reorder_pinned(
        &mut self,
        list: DialogListQuery,
        from_index: usize,
        to_index: usize,
    ) -> bool {
        let Ok(mut st) = self.state.lock() else {
            return false;
        };
        let Some(order) = st.pinned_dialog_orders.get_mut(&list) else {
            return false;
        };
        if from_index >= order.len() || to_index >= order.len() || from_index == to_index {
            return false;
        }
        let item = order.remove(from_index);
        order.insert(to_index, item);
        true
    }

    fn toggle_mute_dialog(&mut self, peer_id: PeerId) -> bool {
        let Ok(mut st) = self.state.lock() else {
            return false;
        };
        if let Some(p) = st.peer_mut(peer_id) {
            p.muted = !p.muted;
            return p.muted;
        }
        false
    }

    fn mark_dialog_read(&mut self, peer_id: PeerId) {
        if let Ok(mut st) = self.state.lock() {
            st.mark_peer_read(peer_id);
        }
    }

    fn mark_dialog_unread(&mut self, peer_id: PeerId) {
        if let Ok(mut st) = self.state.lock()
            && let Some(p) = st.peer_mut(peer_id)
            && p.unread_count == 0
        {
            p.unread_mark = true;
        }
    }

    fn clear_history(&mut self, peer_id: PeerId) {
        if let Ok(mut st) = self.state.lock() {
            if let Some(list) = st.histories.get_mut(&peer_id) {
                list.clear();
            }
            st.pinned_messages.remove(&peer_id);
        }
    }

    fn delete_dialog(&mut self, peer_id: PeerId) {
        if let Ok(mut st) = self.state.lock() {
            if peer_id == session_peers::SELF {
                return;
            }
            st.peers.retain(|p| p.id != peer_id);
            st.histories.remove(&peer_id);
            st.pinned_messages.remove(&peer_id);
        }
    }

    fn mute_dialog_for(&mut self, peer_id: PeerId, seconds: i32) -> Result<bool, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let until = if seconds <= 0 {
            0
        } else if seconds == i32::MAX {
            i32::MAX
        } else {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i32::try_from(d.as_secs()).unwrap_or(i32::MAX));
            now.saturating_add(seconds)
        };
        let peer = st
            .peer_mut(peer_id)
            .ok_or_else(|| "Chat not found".to_owned())?;
        peer.muted = until > 0;
        let muted = peer.muted;
        if muted {
            st.mute_until.insert(peer_id, until);
        } else {
            st.mute_until.remove(&peer_id);
        }
        Ok(muted)
    }

    fn dialog_mute_until(&self, peer_id: PeerId) -> Option<i32> {
        let st = self.state.lock().ok()?;
        let muted = st.peers.iter().any(|p| p.id == peer_id && p.muted);
        muted.then(|| st.mute_until.get(&peer_id).copied().unwrap_or(i32::MAX))
    }

    fn dialog_notify_sound(
        &self,
        peer_id: PeerId,
    ) -> tg_app_core::notifications::settings::NotifySound {
        self.state
            .lock()
            .ok()
            .and_then(|st| st.notify_sounds.get(&peer_id).cloned())
            .unwrap_or_default()
    }

    fn set_dialog_sound_enabled(&mut self, peer_id: PeerId, enabled: bool) -> Result<bool, String> {
        let sound = if enabled {
            tg_app_core::notifications::settings::NotifySound::default_sound()
        } else {
            tg_app_core::notifications::settings::NotifySound::silent()
        };
        self.set_dialog_notify_sound(peer_id, sound)?;
        Ok(enabled)
    }

    fn set_dialog_notify_sound(
        &mut self,
        peer_id: PeerId,
        sound: tg_app_core::notifications::settings::NotifySound,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.notify_sounds.insert(peer_id, sound);
        Ok(())
    }

    fn report_spam_and_block(
        &mut self,
        peer_id: PeerId,
        report_spam: bool,
        delete_chat: bool,
    ) -> Result<(), String> {
        {
            let mut st = self
                .state
                .lock()
                .map_err(|_| "Session state lock poisoned".to_owned())?;
            let peer = st
                .peer_mut(peer_id)
                .ok_or_else(|| "Chat not found".to_owned())?;
            peer.is_blocked = true;
        }
        if report_spam {
            HistoryBackend::report_spam(self, peer_id)?;
        }
        if delete_chat {
            DialogsBackend::delete_dialog(self, peer_id);
        }
        Ok(())
    }

    fn delete_chat(&mut self, peer_id: PeerId, _revoke: bool) -> Result<(), String> {
        DialogsBackend::delete_dialog(self, peer_id);
        Ok(())
    }

    fn can_revoke_full_history(&self, peer_id: PeerId) -> bool {
        self.state.lock().is_ok_and(|st| {
            st.peer(peer_id).is_some_and(|p| match p.kind {
                InfoPeerKind::User => true,
                InfoPeerKind::Group | InfoPeerKind::Supergroup => p.can_manage_admins,
                _ => false,
            })
        })
    }

    fn peer_first_name(&self, peer_id: PeerId) -> Option<String> {
        let st = self.state.lock().ok()?;
        let peer = st.peer(peer_id).filter(|p| p.kind == InfoPeerKind::User)?;
        peer.title.split_whitespace().next().map(str::to_owned)
    }

    fn unblock_and_restart_bot(&mut self, peer_id: PeerId) -> Result<(), String> {
        let is_bot = {
            let mut st = self
                .state
                .lock()
                .map_err(|_| "Session state lock poisoned".to_owned())?;
            let peer = st
                .peer_mut(peer_id)
                .ok_or_else(|| "Chat not found".to_owned())?;
            peer.is_blocked = false;
            peer.kind == InfoPeerKind::Bot
        };
        if is_bot {
            let bot = tg_bots::stand_in::UserId(peer_id.bare());
            let params = tg_bots::protocol::StartBotParams::for_private_chat(bot, String::new(), 0);
            tg_app_core::bots::backend::BotsBackend::start_bot(self, &params)?;
        }
        Ok(())
    }

    fn search_dialogs(&self, request: &SearchRequest) -> SearchResults {
        let Ok(st) = self.state.lock() else {
            return SearchResults::default();
        };
        let q = request.query.trim().to_lowercase();
        let mut results = SearchResults::default();
        if q.is_empty() && request.selected_tag.is_none() {
            return results;
        }

        // 1. Hashtags
        if q.starts_with('#') || q.starts_with('$') {
            let sigil = q.chars().next().unwrap_or('#');
            let body = &q[1..];
            for tag in &st.recent_hashtags {
                if body.is_empty() || tag.to_lowercase().contains(body) {
                    results.hashtags.push(HashtagSuggestion {
                        tag: tag.clone(),
                        sigil,
                        usage_count: 3,
                    });
                }
            }
        }

        // 2. Local chats
        for peer in &st.peers {
            if request.in_archive && !peer.archived {
                continue;
            }
            if let Some(in_peer) = request.in_peer
                && peer.id != in_peer
            {
                continue;
            }
            let kind_ok = match request.tab {
                SearchTab::Chats => true,
                SearchTab::Channels | SearchTab::PublicPosts => peer.kind.is_channel(),
                SearchTab::Apps => peer.kind == InfoPeerKind::Bot,
            };
            if !kind_ok {
                continue;
            }
            let matched = peer.title.to_lowercase().contains(&q)
                || peer
                    .primary_username()
                    .is_some_and(|u| u.to_lowercase().contains(&q));
            if matched {
                results
                    .local_chats
                    .push(st.build_dialog_item(peer, DialogListQuery::Filter(FilterId::ALL)));
            }
        }

        // 3. Global peers
        for gp in &st.global_peers {
            if gp.title.to_lowercase().contains(&q) || gp.username.to_lowercase().contains(&q) {
                results.global_peers.push(gp.clone());
            }
        }

        // 4. Messages across all histories
        let mut all_matches = Vec::new();
        for peer in &st.peers {
            if request.in_archive && !peer.archived {
                continue;
            }
            if let Some(in_peer) = request.in_peer
                && peer.id != in_peer
            {
                continue;
            }
            if !request.chat_type_filter.matches(peer.dialog_kind()) {
                continue;
            }
            if let Some(msgs) = st.histories.get(&peer.id) {
                for m in msgs.iter().rev() {
                    let hay = m.text.text.to_lowercase();
                    let matches_q = q.is_empty() || hay.contains(&q);
                    let matches_tag = request
                        .selected_tag
                        .as_deref()
                        .is_none_or(|t| hay.contains(&t.to_lowercase()));
                    if matches_q && matches_tag {
                        all_matches.push(MessageSearchResult {
                            peer_id: peer.id,
                            kind: peer.dialog_kind(),
                            special: if peer.kind == InfoPeerKind::SavedMessages {
                                SpecialRowKind::SavedMessages
                            } else {
                                SpecialRowKind::None
                            },
                            peer_title: peer.title.clone(),
                            color_index: peer.color_index,
                            msg_id: m.id,
                            sender_name: peer.kind.is_group().then(|| m.sender_name.clone()),
                            text: m.preview_snippet(),
                            time: LocalDateTime::from_unix_utc(i64::from(m.date_unix)),
                            send_state: match (
                                m.outgoing && peer.kind != InfoPeerKind::SavedMessages,
                                m.is_sending,
                                m.is_read,
                            ) {
                                (false, _, _) => DialogSendState::None,
                                (true, true, _) => DialogSendState::Sending,
                                (true, false, true) => DialogSendState::Received,
                                (true, false, false) => DialogSendState::Sent,
                            },
                            archived: peer.archived,
                        });
                    }
                }
            }
        }
        results.total_messages = all_matches.len();
        let limit = if request.messages_limit == 0 {
            20
        } else {
            request.messages_limit
        };
        let start = request.messages_offset.min(all_matches.len());
        let end = start.saturating_add(limit).min(all_matches.len());
        results.has_more_messages = end < all_matches.len();
        results.messages = all_matches[start..end].to_vec();
        results
    }

    fn top_peers(&self) -> Vec<TopPeerItem> {
        let Ok(st) = self.state.lock() else {
            return Vec::new();
        };
        st.top_peers
            .iter()
            .filter_map(|&pid| {
                let p = st.peer(pid)?;
                Some(TopPeerItem {
                    peer_id: p.id,
                    short_name: p
                        .title
                        .split_whitespace()
                        .next()
                        .unwrap_or(&p.title)
                        .to_owned(),
                    full_title: p.title.clone(),
                    color_index: p.color_index,
                    online: p.is_online,
                    unread_count: p.unread_count,
                    muted: p.muted,
                })
            })
            .collect()
    }

    fn remove_top_peer(&mut self, peer_id: PeerId) {
        if let Ok(mut st) = self.state.lock() {
            st.top_peers.retain(|&id| id != peer_id);
        }
    }

    fn recent_search_peers(&self) -> Vec<RecentPeerItem> {
        let Ok(st) = self.state.lock() else {
            return Vec::new();
        };
        st.recent_search_peers
            .iter()
            .filter_map(|&pid| {
                let p = st.peer(pid)?;
                Some(RecentPeerItem {
                    peer_id: p.id,
                    kind: p.dialog_kind(),
                    special: if p.kind == InfoPeerKind::SavedMessages {
                        SpecialRowKind::SavedMessages
                    } else {
                        SpecialRowKind::None
                    },
                    title: p.title.clone(),
                    status: p.status.clone(),
                    color_index: p.color_index,
                    unread_count: p.unread_count,
                    muted: p.muted,
                    verified: p.verified,
                })
            })
            .collect()
    }

    fn add_recent_search_peer(&mut self, peer_id: PeerId) {
        if let Ok(mut st) = self.state.lock() {
            st.recent_search_peers.retain(|&id| id != peer_id);
            st.recent_search_peers.insert(0, peer_id);
            st.recent_search_peers.truncate(12);
        }
    }

    fn remove_recent_search_peer(&mut self, peer_id: PeerId) {
        if let Ok(mut st) = self.state.lock() {
            st.recent_search_peers.retain(|&id| id != peer_id);
        }
    }

    fn clear_recent_search_peers(&mut self) {
        if let Ok(mut st) = self.state.lock() {
            st.recent_search_peers.clear();
        }
    }

    fn recent_hashtags(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|st| st.recent_hashtags.clone())
            .unwrap_or_default()
    }

    fn remove_recent_hashtag(&mut self, tag: &str) {
        if let Ok(mut st) = self.state.lock() {
            st.recent_hashtags.retain(|t| !t.eq_ignore_ascii_case(tag));
        }
    }

    fn search_tags(&self) -> Vec<SearchTagChip> {
        self.state
            .lock()
            .map(|st| st.search_tags.clone())
            .unwrap_or_default()
    }

    fn saved_messages_peer_id(&self) -> PeerId {
        session_peers::SELF
    }

    fn community_groups(&self) -> Vec<tg_app_info::dialogs::community::CommunityDialogGroup> {
        self.state
            .lock()
            .map(|st| st.communities.community_dialog_groups())
            .unwrap_or_default()
    }

    fn toggle_community_collapsed(&mut self, community_id: PeerId, collapsed: bool) {
        if let Ok(mut st) = self.state.lock() {
            let _ = st
                .communities
                .toggle_collapsed_in_dialogs(community_id, collapsed);
        }
    }

    fn contact_birthdays(
        &self,
        _force: bool,
    ) -> Result<Vec<tg_app_core::core_types::dto::contacts_extras::ContactBirthdayEntry>, String>
    {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.contact_birthdays.clone())
    }

    fn sponsored_peers(
        &self,
        query: &str,
    ) -> Result<Vec<tg_app_core::core_types::dto::contacts_extras::SponsoredPeerInfo>, String> {
        if query.trim().chars().count() < 4 {
            return Ok(Vec::new());
        }
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.sponsored_peers.clone())
    }

    fn promo_data(&self) -> Option<tg_app_core::core_types::misc::PromoDataSnapshot> {
        self.state.lock().ok().and_then(|st| st.promo_data.clone())
    }

    fn hide_promo_data(&mut self, peer_id: PeerId) -> Result<bool, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.hidden_promo_peers.push(peer_id);
        if st
            .promo_data
            .as_ref()
            .is_some_and(|p| p.promo_peer == Some(peer_id))
        {
            st.promo_data = None;
        }
        Ok(true)
    }

    fn top_peers_enabled(&self) -> bool {
        self.state
            .lock()
            .map(|st| st.top_peers_enabled)
            .unwrap_or(true)
    }

    fn toggle_top_peers(&mut self, enabled: bool) -> Result<bool, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.top_peers_enabled = enabled;
        Ok(true)
    }
}

impl DialogsBackend for MockSession {
    fn current_time(&self) -> LocalDateTime {
        self.handle.current_time()
    }
    fn dialog_slice(&self, list: DialogListQuery, offset: usize, limit: usize) -> DialogSlice {
        self.handle.dialog_slice(list, offset, limit)
    }
    fn dialog_by_peer(&self, peer_id: PeerId) -> Option<DialogItem> {
        self.handle.dialog_by_peer(peer_id)
    }
    fn is_peer_blocked(&self, peer_id: PeerId) -> bool {
        self.handle.is_peer_blocked(peer_id)
    }
    fn chat_folders(&self) -> Vec<DialogChatFolder> {
        self.handle.chat_folders()
    }
    fn reorder_folders(&mut self, order: &[FilterId]) {
        DialogsBackend::reorder_folders(&mut self.handle, order);
    }
    fn add_peer_to_folder(&mut self, peer_id: PeerId, filter_id: FilterId) {
        self.handle.add_peer_to_folder(peer_id, filter_id);
    }
    fn remove_peer_from_folder(&mut self, peer_id: PeerId, filter_id: FilterId) {
        self.handle.remove_peer_from_folder(peer_id, filter_id);
    }
    fn mark_folder_read(&mut self, filter_id: FilterId) {
        self.handle.mark_folder_read(filter_id);
    }
    fn archive_summary(&self) -> Option<ArchiveSummary> {
        self.handle.archive_summary()
    }
    fn archive_collapsed(&self) -> bool {
        self.handle.archive_collapsed()
    }
    fn set_archive_collapsed(&mut self, collapsed: bool) {
        self.handle.set_archive_collapsed(collapsed);
    }
    fn toggle_archive(&mut self, peer_id: PeerId) -> bool {
        self.handle.toggle_archive(peer_id)
    }
    fn mark_archive_read(&mut self) {
        self.handle.mark_archive_read();
    }
    fn toggle_pin(&mut self, peer_id: PeerId, list: DialogListQuery) -> bool {
        self.handle.toggle_pin(peer_id, list)
    }
    fn reorder_pinned(
        &mut self,
        list: DialogListQuery,
        from_index: usize,
        to_index: usize,
    ) -> bool {
        self.handle.reorder_pinned(list, from_index, to_index)
    }
    fn toggle_mute_dialog(&mut self, peer_id: PeerId) -> bool {
        self.handle.toggle_mute_dialog(peer_id)
    }
    fn mark_dialog_read(&mut self, peer_id: PeerId) {
        self.handle.mark_dialog_read(peer_id);
    }
    fn mark_dialog_unread(&mut self, peer_id: PeerId) {
        self.handle.mark_dialog_unread(peer_id);
    }
    fn clear_history(&mut self, peer_id: PeerId) {
        self.handle.clear_history(peer_id);
    }
    fn delete_dialog(&mut self, peer_id: PeerId) {
        self.handle.delete_dialog(peer_id);
    }
    fn mute_dialog_for(&mut self, peer_id: PeerId, seconds: i32) -> Result<bool, String> {
        self.handle.mute_dialog_for(peer_id, seconds)
    }
    fn dialog_mute_until(&self, peer_id: PeerId) -> Option<i32> {
        self.handle.dialog_mute_until(peer_id)
    }
    fn dialog_notify_sound(
        &self,
        peer_id: PeerId,
    ) -> tg_app_core::notifications::settings::NotifySound {
        self.handle.dialog_notify_sound(peer_id)
    }
    fn set_dialog_sound_enabled(&mut self, peer_id: PeerId, enabled: bool) -> Result<bool, String> {
        self.handle.set_dialog_sound_enabled(peer_id, enabled)
    }
    fn set_dialog_notify_sound(
        &mut self,
        peer_id: PeerId,
        sound: tg_app_core::notifications::settings::NotifySound,
    ) -> Result<(), String> {
        self.handle.set_dialog_notify_sound(peer_id, sound)
    }
    fn report_spam_and_block(
        &mut self,
        peer_id: PeerId,
        report_spam: bool,
        delete_chat: bool,
    ) -> Result<(), String> {
        self.handle
            .report_spam_and_block(peer_id, report_spam, delete_chat)
    }
    fn delete_chat(&mut self, peer_id: PeerId, revoke: bool) -> Result<(), String> {
        self.handle.delete_chat(peer_id, revoke)
    }
    fn can_revoke_full_history(&self, peer_id: PeerId) -> bool {
        self.handle.can_revoke_full_history(peer_id)
    }
    fn peer_first_name(&self, peer_id: PeerId) -> Option<String> {
        self.handle.peer_first_name(peer_id)
    }
    fn unblock_and_restart_bot(&mut self, peer_id: PeerId) -> Result<(), String> {
        self.handle.unblock_and_restart_bot(peer_id)
    }
    fn search_dialogs(&self, request: &SearchRequest) -> SearchResults {
        self.handle.search_dialogs(request)
    }
    fn top_peers(&self) -> Vec<TopPeerItem> {
        self.handle.top_peers()
    }
    fn remove_top_peer(&mut self, peer_id: PeerId) {
        self.handle.remove_top_peer(peer_id);
    }
    fn recent_search_peers(&self) -> Vec<RecentPeerItem> {
        self.handle.recent_search_peers()
    }
    fn add_recent_search_peer(&mut self, peer_id: PeerId) {
        self.handle.add_recent_search_peer(peer_id);
    }
    fn remove_recent_search_peer(&mut self, peer_id: PeerId) {
        self.handle.remove_recent_search_peer(peer_id);
    }
    fn clear_recent_search_peers(&mut self) {
        self.handle.clear_recent_search_peers();
    }
    fn recent_hashtags(&self) -> Vec<String> {
        self.handle.recent_hashtags()
    }
    fn remove_recent_hashtag(&mut self, tag: &str) {
        self.handle.remove_recent_hashtag(tag);
    }
    fn search_tags(&self) -> Vec<SearchTagChip> {
        self.handle.search_tags()
    }
    fn saved_messages_peer_id(&self) -> PeerId {
        self.handle.saved_messages_peer_id()
    }
    fn community_groups(&self) -> Vec<tg_app_info::dialogs::community::CommunityDialogGroup> {
        self.handle.community_groups()
    }
    fn toggle_community_collapsed(&mut self, community_id: PeerId, collapsed: bool) {
        self.handle
            .toggle_community_collapsed(community_id, collapsed);
    }
    fn contact_birthdays(
        &self,
        force: bool,
    ) -> Result<Vec<tg_app_core::core_types::dto::contacts_extras::ContactBirthdayEntry>, String>
    {
        self.handle.contact_birthdays(force)
    }
    fn sponsored_peers(
        &self,
        query: &str,
    ) -> Result<Vec<tg_app_core::core_types::dto::contacts_extras::SponsoredPeerInfo>, String> {
        self.handle.sponsored_peers(query)
    }
    fn promo_data(&self) -> Option<tg_app_core::core_types::misc::PromoDataSnapshot> {
        self.handle.promo_data()
    }
    fn hide_promo_data(&mut self, peer_id: PeerId) -> Result<bool, String> {
        self.handle.hide_promo_data(peer_id)
    }
    fn top_peers_enabled(&self) -> bool {
        self.handle.top_peers_enabled()
    }
    fn toggle_top_peers(&mut self, enabled: bool) -> Result<bool, String> {
        self.handle.toggle_top_peers(enabled)
    }
}

// =============================================================================
// 3. history::backend::HistoryBackend
// =============================================================================

impl HistoryBackend for SharedSessionHandle {
    fn self_peer(&self) -> (PeerId, String) {
        self.state
            .lock()
            .map(|st| (st.user.id, st.user.name.clone()))
            .unwrap_or_else(|_| (session_peers::SELF, "Taylor Kim".to_owned()))
    }

    fn peer_info(&self, peer_id: PeerId) -> Option<HistoryPeerInfo> {
        let st = self.state.lock().ok()?;
        let rec = st.peer(peer_id)?;
        let summary = rec.to_peer_summary();
        let link_base = rec
            .primary_username()
            .map(|u| format!("https://t.me/{u}"))
            .unwrap_or_else(|| format!("https://t.me/c/{}", rec.id.bare()));
        let typing_text = rec.typing.as_ref().map(|t| format!("{}...", t.label()));
        Some(HistoryPeerInfo {
            summary,
            typing_text,
            contact_status: rec.contact_status.clone(),
            unread_count: rec.unread_count,
            can_pin: !rec.kind.is_channel() || rec.can_edit,
            can_delete: true,
            link_base,
        })
    }

    fn load_slice_around(
        &self,
        peer_id: PeerId,
        around_id: MsgId,
        limit_before: usize,
        limit_after: usize,
    ) -> HistorySlice {
        let Ok(st) = self.state.lock() else {
            return HistorySlice::default();
        };
        let Some(all) = st.histories.get(&peer_id) else {
            return HistorySlice {
                loaded_at_top: true,
                loaded_at_bottom: true,
                ..HistorySlice::default()
            };
        };
        let total = all.len();
        if total == 0 {
            return HistorySlice {
                loaded_at_top: true,
                loaded_at_bottom: true,
                ..HistorySlice::default()
            };
        }
        let anchor_idx = if around_id == MsgId::SHOW_AT_END || around_id == MsgId::SHOW_AT_UNREAD {
            total - 1
        } else {
            all.iter()
                .position(|m| m.id >= around_id)
                .unwrap_or(total - 1)
        };
        let start = anchor_idx.saturating_sub(limit_before.max(40));
        let end = anchor_idx
            .saturating_add(limit_after.max(40) + 1)
            .min(total);
        HistorySlice {
            messages: all[start..end].to_vec(),
            loaded_at_top: start == 0,
            loaded_at_bottom: end >= total,
            total_count: total,
            first_unread_id: st.first_unread.get(&peer_id).copied().flatten(),
            unread_count: st.peer(peer_id).map_or(0, |p| p.unread_count),
        }
    }

    fn load_slice_before(&self, peer_id: PeerId, before_id: MsgId, limit: usize) -> HistorySlice {
        let Ok(st) = self.state.lock() else {
            return HistorySlice::default();
        };
        let Some(all) = st.histories.get(&peer_id) else {
            return HistorySlice {
                loaded_at_top: true,
                ..HistorySlice::default()
            };
        };
        let end = all.iter().position(|m| m.id >= before_id).unwrap_or(0);
        let start = end.saturating_sub(limit.max(1));
        HistorySlice {
            messages: all[start..end].to_vec(),
            loaded_at_top: start == 0,
            loaded_at_bottom: end >= all.len(),
            total_count: all.len(),
            first_unread_id: st.first_unread.get(&peer_id).copied().flatten(),
            unread_count: st.peer(peer_id).map_or(0, |p| p.unread_count),
        }
    }

    fn load_slice_after(&self, peer_id: PeerId, after_id: MsgId, limit: usize) -> HistorySlice {
        let Ok(st) = self.state.lock() else {
            return HistorySlice::default();
        };
        let Some(all) = st.histories.get(&peer_id) else {
            return HistorySlice {
                loaded_at_bottom: true,
                ..HistorySlice::default()
            };
        };
        let start = all
            .iter()
            .position(|m| m.id > after_id)
            .unwrap_or(all.len());
        let end = start.saturating_add(limit.max(1)).min(all.len());
        HistorySlice {
            messages: all[start..end].to_vec(),
            loaded_at_top: start == 0,
            loaded_at_bottom: end >= all.len(),
            total_count: all.len(),
            first_unread_id: st.first_unread.get(&peer_id).copied().flatten(),
            unread_count: st.peer(peer_id).map_or(0, |p| p.unread_count),
        }
    }

    fn send_message(
        &mut self,
        peer_id: PeerId,
        request: SendMessageRequest,
    ) -> Result<HistoryMessage, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let was_empty_bot = st
            .peer(peer_id)
            .is_some_and(|p| p.kind == InfoPeerKind::Bot)
            && st
                .histories
                .get(&peer_id)
                .is_none_or(std::vec::Vec::is_empty);
        let is_start_cmd = request.text.text.trim().starts_with("/start");
        let sent = st.append_outgoing_message(peer_id, request)?;
        if was_empty_bot && is_start_cmd {
            let bot_name = st
                .peer(peer_id)
                .map_or_else(|| "Catalog Bot".to_owned(), |p| p.title.clone());
            let welcome_id = MsgId(st.next_msg_id);
            st.next_msg_id += 1;
            let kb_id = MsgId(st.next_msg_id);
            st.next_msg_id += 1;
            let list = st.histories.entry(peer_id).or_default();
            list.push(HistoryMessage {
                id: welcome_id,
                peer_id,
                from_id: peer_id,
                sender_name: bot_name.clone(),
                date_unix: BASE_UNIX_TODAY + 10_201,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::simple(
                    "Catalog Bot started! Use the inline controls or reply keyboard below.",
                ),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra {
                    reply_markup: Some(build_demo_inline_keyboard()),
                    ..MessageExtra::default()
                },
            });
            list.push(HistoryMessage {
                id: kb_id,
                peer_id,
                from_id: peer_id,
                sender_name: bot_name,
                date_unix: BASE_UNIX_TODAY + 10_202,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::simple(
                    "Choose a quick action from the reply keyboard or type / for commands.",
                ),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra {
                    reply_markup: Some(build_demo_reply_keyboard(true, false, true, false)),
                    ..MessageExtra::default()
                },
            });
        }
        Ok(sent)
    }

    fn send_saved_music(
        &mut self,
        peer_id: PeerId,
        tracks: &[tg_app_core::info::backend::SavedMusicTrack],
    ) -> Result<Vec<HistoryMessage>, String> {
        if tracks.is_empty() {
            return Err("Select at least one track to send.".to_owned());
        }
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let self_id = st.user.id;
        let self_name = st.user.name.clone();
        let mut sent = Vec::with_capacity(tracks.len());
        for track in tracks {
            let msg_id = MsgId(st.next_msg_id);
            st.next_msg_id += 1;
            let file_name = if track.performer.trim().is_empty() {
                format!("{}.mp3", track.title.trim())
            } else {
                format!("{} - {}.mp3", track.performer.trim(), track.title.trim())
            };
            let msg = HistoryMessage {
                id: msg_id,
                peer_id,
                from_id: self_id,
                sender_name: self_name.clone(),
                date_unix: BASE_UNIX_TODAY + 10_300 + sent.len() as i32,
                outgoing: true,
                is_post: false,
                text: TextWithEntities::default(),
                media: Some(HistoryMedia::Document(DocumentAttachment {
                    file_name,
                    size_bytes: track.size_bytes,
                    mime_type: "audio/mpeg".to_owned(),
                    thumb_rgb: None,
                })),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: false,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            };
            st.histories.entry(peer_id).or_default().push(msg.clone());
            sent.push(msg);
        }
        Ok(sent)
    }

    fn edit_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        text: TextWithEntities,
    ) -> Result<HistoryMessage, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let Some(list) = st.histories.get_mut(&peer_id) else {
            return Err("Chat history not found.".to_owned());
        };
        let Some(msg) = list.iter_mut().find(|m| m.id == msg_id) else {
            return Err("Message not found.".to_owned());
        };
        msg.text = text;
        msg.edited = true;
        Ok(msg.clone())
    }

    fn send_rich_message(
        &mut self,
        peer_id: PeerId,
        input: tg_tl::types::InputRichMessage,
        page: tg_iv::rich_page::RichPage,
        reply_to: Option<MsgId>,
        silent: bool,
    ) -> Result<HistoryMessage, String> {
        let payload =
            tg_app_media::webview::rich_page_view::RichMessagePayload::from_page_and_input(
                page, &input,
            );
        let summary = payload.summary_text();
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let mut sent = st.append_outgoing_message(
            peer_id,
            SendMessageRequest {
                text: TextWithEntities::simple(summary),
                reply_to,
                silent,
            },
        )?;
        sent.extra.rich_message = Some(payload);
        if let Some(slot) = st
            .histories
            .get_mut(&peer_id)
            .and_then(|v| v.iter_mut().find(|m| m.id == sent.id))
        {
            *slot = sent.clone();
        }
        Ok(sent)
    }

    fn edit_rich_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        input: tg_tl::types::InputRichMessage,
        page: tg_iv::rich_page::RichPage,
    ) -> Result<HistoryMessage, String> {
        let payload =
            tg_app_media::webview::rich_page_view::RichMessagePayload::from_page_and_input(
                page, &input,
            );
        let summary = payload.summary_text();
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let Some(list) = st.histories.get_mut(&peer_id) else {
            return Err("Chat history not found.".to_owned());
        };
        let Some(msg) = list.iter_mut().find(|m| m.id == msg_id) else {
            return Err("Message not found.".to_owned());
        };
        msg.text = TextWithEntities::simple(summary);
        msg.extra.rich_message = Some(payload);
        msg.edited = true;
        Ok(msg.clone())
    }

    fn delete_messages(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
        _revoke_for_all: bool,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        if let Some(list) = st.histories.get_mut(&peer_id) {
            list.retain(|m| !msg_ids.contains(&m.id));
        }
        if let Some(pins) = st.pinned_messages.get_mut(&peer_id) {
            pins.retain(|id| !msg_ids.contains(id));
        }
        for &mid in msg_ids {
            st.notify_events
                .push_back(IncomingNotifyEvent::MessageDeleted {
                    session_id: 1,
                    peer_id,
                    msg_id: mid,
                });
        }
        Ok(())
    }

    fn forward_messages(
        &mut self,
        from_peer: PeerId,
        msg_ids: &[MsgId],
        to_peer: PeerId,
    ) -> Result<Vec<HistoryMessage>, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let sources: Vec<HistoryMessage> = st
            .histories
            .get(&from_peer)
            .map(|list| {
                list.iter()
                    .filter(|m| msg_ids.contains(&m.id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let mut created = Vec::new();
        for src in sources {
            let new_id = MsgId(st.next_msg_id);
            st.next_msg_id += 1;
            let fwd = HistoryMessage {
                extra: Default::default(),
                id: new_id,
                peer_id: to_peer,
                from_id: st.user.id,
                sender_name: st.user.name.clone(),
                date_unix: BASE_UNIX_TODAY + 16_500,
                outgoing: true,
                is_post: false,
                text: src.text,
                media: src.media,
                service: None,
                reply: None,
                forward: Some(ForwardHeader {
                    from_name: src.sender_name,
                    from_peer: Some(from_peer),
                    original_date: "Nov 22".to_owned(),
                }),
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: false,
                is_sending: false,
                is_pinned: false,
            };
            st.histories.entry(to_peer).or_default().push(fwd.clone());
            created.push(fwd);
        }
        if let Some(p) = st.peer_mut(to_peer) {
            p.last_message_time = SESSION_NOW;
        }
        Ok(created)
    }

    fn mark_history_read(&mut self, peer_id: PeerId, _max_id: MsgId) {
        if let Ok(mut st) = self.state.lock() {
            st.mark_peer_read(peer_id);
        }
    }

    fn send_typing(&mut self, _peer_id: PeerId, _action: HistoryTypingAction) {}

    fn toggle_reaction(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        emoji: &str,
    ) -> Result<Vec<ReactionCounter>, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let Some(list) = st.histories.get_mut(&peer_id) else {
            return Err("Chat not found.".to_owned());
        };
        let Some(msg) = list.iter_mut().find(|m| m.id == msg_id) else {
            return Err("Message not found.".to_owned());
        };
        if let Some(pos) = msg.reactions.iter().position(|r| r.emoji == emoji) {
            if msg.reactions[pos].chosen {
                msg.reactions[pos].chosen = false;
                msg.reactions[pos].count = msg.reactions[pos].count.saturating_sub(1);
                if msg.reactions[pos].count == 0 {
                    msg.reactions.remove(pos);
                }
            } else {
                msg.reactions[pos].chosen = true;
                msg.reactions[pos].count += 1;
            }
        } else {
            msg.reactions.push(ReactionCounter {
                emoji: emoji.to_owned(),
                count: 1,
                chosen: true,
            });
        }
        Ok(msg.reactions.clone())
    }

    fn pin_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        unpin: bool,
    ) -> Result<Vec<PinnedMessageSummary>, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        if let Some(list) = st.histories.get_mut(&peer_id)
            && let Some(msg) = list.iter_mut().find(|m| m.id == msg_id)
        {
            msg.is_pinned = !unpin;
        }
        let pins = st.pinned_messages.entry(peer_id).or_default();
        pins.retain(|&id| id != msg_id);
        if !unpin {
            pins.insert(0, msg_id);
        }
        drop(st);
        Ok(self.pinned_messages(peer_id))
    }

    fn pinned_messages(&self, peer_id: PeerId) -> Vec<PinnedMessageSummary> {
        let Ok(st) = self.state.lock() else {
            return Vec::new();
        };
        let Some(pin_ids) = st.pinned_messages.get(&peer_id) else {
            return Vec::new();
        };
        let Some(msgs) = st.histories.get(&peer_id) else {
            return Vec::new();
        };
        pin_ids
            .iter()
            .filter_map(|&pid| {
                let m = msgs.iter().find(|m| m.id == pid)?;
                Some(PinnedMessageSummary {
                    msg_id: m.id,
                    sender_name: m.sender_name.clone(),
                    preview: m.preview_snippet(),
                })
            })
            .collect()
    }

    fn vote_poll(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        option_indices: &[usize],
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let Some(list) = st.histories.get_mut(&peer_id) else {
            return Err("Chat not found.".to_owned());
        };
        let Some(msg) = list.iter_mut().find(|m| m.id == msg_id) else {
            return Err("Message not found.".to_owned());
        };
        if let Some(HistoryMedia::Poll(ref mut poll)) = msg.media {
            for (idx, opt) in poll.options.iter_mut().enumerate() {
                let selected = option_indices.contains(&idx);
                if selected && !opt.chosen {
                    opt.chosen = true;
                    opt.votes += 1;
                    poll.total_voters += 1;
                } else if !selected && opt.chosen && !poll.multiple_choice {
                    opt.chosen = false;
                    opt.votes = opt.votes.saturating_sub(1);
                    poll.total_voters = poll.total_voters.saturating_sub(1);
                }
            }
        }
        Ok(())
    }

    fn reload_poll_results(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Result<Option<tg_app_core::history::backend::PollAttachment>, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let Some(list) = st.histories.get(&peer_id) else {
            return Ok(None);
        };
        let Some(msg) = list.iter().find(|m| m.id == msg_id) else {
            return Ok(None);
        };
        if let Some(HistoryMedia::Poll(ref poll)) = msg.media {
            return Ok(Some(poll.clone()));
        }
        Ok(None)
    }

    fn poll_stats(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        _dark: bool,
    ) -> Result<tg_stats::StatisticalGraph, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let poll_opt = st
            .histories
            .get(&peer_id)
            .and_then(|list| list.iter().find(|m| m.id == msg_id))
            .and_then(|m| match &m.media {
                Some(HistoryMedia::Poll(p)) => Some(p.clone()),
                _ => None,
            });
        let option_series: Vec<(String, u32)> = match poll_opt {
            Some(poll) if !poll.options.is_empty() => poll
                .options
                .into_iter()
                .map(|o| (o.text, o.votes.max(1)))
                .collect(),
            _ => vec![
                ("Option A".to_owned(), 18),
                ("Option B".to_owned(), 11),
                ("Option C".to_owned(), 7),
            ],
        };
        let palette_colors = ["#3C9DF0", "#55B869", "#F59E0B", "#E05263", "#8B5CF6"];
        let base_ts: i64 = 1_731_800_000_000;
        let x_col = serde_json::json!([
            "x",
            base_ts,
            base_ts + 86_400_000,
            base_ts + 2 * 86_400_000,
            base_ts + 3 * 86_400_000,
            base_ts + 4 * 86_400_000
        ]);
        let mut columns = vec![x_col];
        let mut types = serde_json::Map::new();
        let mut names = serde_json::Map::new();
        let mut colors = serde_json::Map::new();
        types.insert("x".to_owned(), serde_json::Value::String("x".to_owned()));
        for (idx, (label, total)) in option_series.iter().take(5).enumerate() {
            let key = format!("y{idx}");
            let t = i64::from(*total);
            let s0 = (t / 6).max(1);
            let s1 = (t / 3).max(s0);
            let s2 = ((t * 2) / 3).max(s1);
            let s3 = ((t * 5) / 6).max(s2);
            columns.push(serde_json::json!([key, s0, s1, s2, s3, t]));
            types.insert(key.clone(), serde_json::Value::String("bar".to_owned()));
            names.insert(key.clone(), serde_json::Value::String(label.clone()));
            colors.insert(
                key,
                serde_json::Value::String(palette_colors[idx % palette_colors.len()].to_owned()),
            );
        }
        let doc = serde_json::json!({
            "columns": columns,
            "types": types,
            "names": names,
            "colors": colors,
            "stacked": true,
        });
        let chart = tg_stats::StatisticalChart::from_json_str(&doc.to_string())
            .map_err(|e| format!("Failed to build poll stats chart: {e}"))?;
        Ok(tg_stats::StatisticalGraph {
            chart,
            zoom_token: String::new(),
            error: String::new(),
        })
    }

    fn save_draft(&mut self, peer_id: PeerId, draft: Option<DraftMessage>) {
        if let Ok(mut st) = self.state.lock() {
            match draft {
                Some(d) if !d.text.trim().is_empty() || d.reply_to.is_some() => {
                    st.drafts.insert(peer_id, d);
                }
                _ => {
                    st.drafts.remove(&peer_id);
                }
            }
        }
    }

    fn load_draft(&self, peer_id: PeerId) -> Option<DraftMessage> {
        self.state
            .lock()
            .ok()
            .and_then(|st| st.drafts.get(&peer_id).cloned())
    }

    fn dismiss_contact_status(&mut self, peer_id: PeerId) {
        if let Ok(mut st) = self.state.lock()
            && let Some(p) = st.peer_mut(peer_id)
        {
            p.contact_status = None;
        }
    }

    fn add_contact(&mut self, peer_id: PeerId) {
        if let Ok(mut st) = self.state.lock()
            && let Some(p) = st.peer_mut(peer_id)
        {
            p.is_contact = true;
            p.contact_status = None;
        }
    }

    fn accept_contact(&mut self, peer_id: PeerId) -> Result<(), String> {
        HistoryBackend::add_contact(self, peer_id);
        Ok(())
    }

    fn report_spam(&mut self, peer_id: PeerId) -> Result<bool, String> {
        self.dismiss_contact_status(peer_id);
        Ok(true)
    }

    fn hide_peer_settings_bar(&mut self, peer_id: PeerId) -> Result<bool, String> {
        self.dismiss_contact_status(peer_id);
        Ok(true)
    }

    fn toggle_block_peer(&mut self, peer_id: PeerId) {
        if let Ok(mut st) = self.state.lock()
            && let Some(p) = st.peer_mut(peer_id)
        {
            p.is_blocked = !p.is_blocked;
            p.contact_status = if p.is_blocked {
                Some(ContactStatusBanner::Blocked)
            } else {
                None
            };
        }
    }

    fn toggle_mute(&mut self, peer_id: PeerId) -> bool {
        self.toggle_mute_dialog(peer_id)
    }

    fn send_message_extended(
        &mut self,
        peer_id: PeerId,
        request: SendMessageExtended,
    ) -> Result<HistoryMessage, String> {
        if request.text.text.trim().is_empty() && request.media.is_none() {
            return Err("Cannot send an empty message.".to_owned());
        }
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let is_replies_thread = matches!(request.thread_key, Some(ThreadKey::Replies { .. }));
        if !is_replies_thread {
            let Some(peer) = st.peer(peer_id) else {
                return Err("Peer not found.".to_owned());
            };
            if !peer.can_send {
                return Err("You cannot send messages in this read-only channel.".to_owned());
            }
        }

        let reply_header = request.reply_to.map(|rid| {
            let found = st
                .histories
                .get(&peer_id)
                .and_then(|msgs| msgs.iter().find(|m| m.id == rid))
                .or_else(|| {
                    if let Some(ThreadKey::Replies { root_msg_id, .. }) = request.thread_key {
                        st.replies
                            .get(&(peer_id, root_msg_id))
                            .and_then(|msgs| msgs.iter().find(|m| m.id == rid))
                    } else if let Some(ThreadKey::Topic { topic_root_id, .. }) = request.thread_key
                    {
                        st.topic_messages
                            .get(&(peer_id, topic_root_id))
                            .and_then(|msgs| msgs.iter().find(|m| m.id == rid))
                    } else {
                        None
                    }
                });
            let (author_name, author_color_id, preview_text) = if let Some(t) = found {
                (t.sender_name.clone(), t.from_id.bare(), t.preview_snippet())
            } else {
                (
                    "Message".to_owned(),
                    rid.0 as u64,
                    format!("Message #{}", rid.0),
                )
            };
            ReplyHeader {
                reply_to_msg_id: rid,
                author_name,
                author_color_id,
                preview_text,
                preview_thumb_rgb: None,
                quote_text: request.reply_quote.as_ref().map(|q| q.text.clone()),
            }
        });

        let new_id = MsgId(st.next_msg_id);
        st.next_msg_id += 1;
        let last_ts = st
            .histories
            .get(&peer_id)
            .and_then(|m| m.last())
            .map_or(BASE_UNIX_TODAY + 16_200, |m| m.date_unix + 25);
        let thread_key = request.thread_key;
        let msg = HistoryMessage {
            id: new_id,
            peer_id,
            from_id: st.user.id,
            sender_name: st.user.name.clone(),
            date_unix: request.schedule_date.unwrap_or(last_ts),
            outgoing: true,
            is_post: false,
            text: request.text,
            media: request.media,
            service: None,
            reply: reply_header,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: false,
            is_sending: false,
            is_pinned: false,
            extra: MessageExtra {
                thread_id: match thread_key {
                    Some(ThreadKey::Topic { topic_root_id, .. }) => Some(topic_root_id),
                    Some(ThreadKey::Replies { root_msg_id, .. }) => Some(root_msg_id),
                    _ => None,
                },
                saved_from_peer: match thread_key {
                    Some(ThreadKey::SavedSublist {
                        sublist_peer_id, ..
                    }) => Some(sublist_peer_id),
                    _ => None,
                },
                scheduled_date: request.schedule_date,
                silent: request.silent,
                effect_id: request.effect_id,
                via_bot: request.via_bot,
                send_as_name: request
                    .send_as
                    .and_then(|sid| st.peer(sid).map(|p| p.title.clone())),
                translated_text: None,
                link_preview: if request.no_webpage {
                    None
                } else {
                    request.link_preview
                },
                replies_count: None,
                reply_markup: None,
                rich_message: None,
                ttl_seconds: request.ttl_seconds,
                ttl_viewed: false,
                poll_extras: None,
                round_video_path: request.round_video_path,
                summary_from_language: None,
            },
        };

        match thread_key {
            Some(ThreadKey::Topic { topic_root_id, .. }) => {
                st.topic_messages
                    .entry((peer_id, topic_root_id))
                    .or_default()
                    .push(msg.clone());
                if let Some(topics) = st.forum_topics.get_mut(&peer_id)
                    && let Some(t) = topics.iter_mut().find(|t| t.root_id == topic_root_id)
                {
                    t.last_sender = msg.sender_name.clone();
                    t.last_preview = msg.preview_snippet();
                    t.last_date_unix = msg.date_unix;
                    t.draft_preview = None;
                }
                st.thread_drafts.remove(&ThreadKey::Topic {
                    peer_id,
                    topic_root_id,
                });
            }
            Some(ThreadKey::Replies { root_msg_id, .. }) => {
                let list = st.replies.entry((peer_id, root_msg_id)).or_default();
                list.push(msg.clone());
                let new_count = list.len() as u32;
                if let Some(hist) = st.histories.get_mut(&peer_id)
                    && let Some(root) = hist.iter_mut().find(|m| m.id == root_msg_id)
                {
                    root.extra.replies_count = Some(new_count);
                }
                st.thread_drafts.remove(&ThreadKey::Replies {
                    peer_id,
                    root_msg_id,
                });
            }
            _ => {
                st.histories.entry(peer_id).or_default().push(msg.clone());
                st.drafts.remove(&peer_id);
                st.thread_drafts.remove(&ThreadKey::Main(peer_id));
                if let Some(p) = st.peer_mut(peer_id) {
                    p.last_message_time = SESSION_NOW;
                }
            }
        }
        Ok(msg)
    }

    fn forward_messages_with_options(
        &mut self,
        from_peer: PeerId,
        msg_ids: &[MsgId],
        to_peer: PeerId,
        options: ForwardOptions,
    ) -> Result<Vec<HistoryMessage>, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let sources: Vec<HistoryMessage> = st
            .histories
            .get(&from_peer)
            .map(|list| {
                list.iter()
                    .filter(|m| msg_ids.contains(&m.id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let mut created = Vec::new();
        for src in sources {
            let new_id = MsgId(st.next_msg_id);
            st.next_msg_id += 1;
            let text = if options.drop_media_captions && src.media.is_some() {
                TextWithEntities::default()
            } else {
                src.text
            };
            let forward = if options.drop_author {
                None
            } else {
                Some(ForwardHeader {
                    from_name: src.sender_name,
                    from_peer: Some(from_peer),
                    original_date: "Nov 22".to_owned(),
                })
            };
            let fwd = HistoryMessage {
                extra: Default::default(),
                id: new_id,
                peer_id: to_peer,
                from_id: st.user.id,
                sender_name: st.user.name.clone(),
                date_unix: BASE_UNIX_TODAY + 16_500,
                outgoing: true,
                is_post: false,
                text,
                media: src.media,
                service: None,
                reply: None,
                forward,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: false,
                is_sending: false,
                is_pinned: false,
            };
            st.histories.entry(to_peer).or_default().push(fwd.clone());
            created.push(fwd);
        }
        if let Some(p) = st.peer_mut(to_peer) {
            p.last_message_time = SESSION_NOW;
        }
        Ok(created)
    }

    fn replies_root_and_comments(
        &self,
        peer_id: PeerId,
        root_msg_id: MsgId,
    ) -> Option<(HistoryMessage, Vec<HistoryMessage>)> {
        let st = self.state.lock().ok()?;
        let root = st
            .histories
            .get(&peer_id)?
            .iter()
            .find(|m| m.id == root_msg_id)?
            .clone();
        let comments = st
            .replies
            .get(&(peer_id, root_msg_id))
            .cloned()
            .unwrap_or_default();
        Some((root, comments))
    }

    fn forum_topics(&self, peer_id: PeerId) -> Vec<ForumTopic> {
        let Ok(st) = self.state.lock() else {
            return Vec::new();
        };
        let mut topics = st.forum_topics.get(&peer_id).cloned().unwrap_or_default();
        for t in &mut topics {
            if let Some(d) = st.thread_drafts.get(&ThreadKey::Topic {
                peer_id,
                topic_root_id: t.root_id,
            }) && !d.text.trim().is_empty()
            {
                t.draft_preview = Some(d.text.clone());
            }
        }
        topics
    }

    fn create_forum_topic(
        &mut self,
        peer_id: PeerId,
        title: String,
        icon_color: u32,
        custom_emoji: Option<String>,
    ) -> Result<ForumTopic, String> {
        if title.trim().is_empty() {
            return Err("Topic title cannot be empty.".to_owned());
        }
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let root_id = MsgId(st.next_msg_id);
        st.next_msg_id += 1;
        let topic = ForumTopic {
            root_id,
            peer_id,
            title: title.trim().to_owned(),
            icon_color,
            custom_emoji,
            is_general: false,
            is_pinned: false,
            is_closed: false,
            muted: false,
            unread_count: 0,
            unread_mentions: 0,
            last_sender: st.user.name.clone(),
            last_preview: "Topic created".to_owned(),
            last_date_unix: BASE_UNIX_TODAY + 16_800,
            draft_preview: None,
        };
        st.forum_topics
            .entry(peer_id)
            .or_default()
            .push(topic.clone());
        Ok(topic)
    }

    fn toggle_forum_topic_pin(
        &mut self,
        peer_id: PeerId,
        topic_root_id: MsgId,
    ) -> Result<bool, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let topics = st
            .forum_topics
            .get_mut(&peer_id)
            .ok_or_else(|| "Forum not found.".to_owned())?;
        let t = topics
            .iter_mut()
            .find(|t| t.root_id == topic_root_id)
            .ok_or_else(|| "Topic not found.".to_owned())?;
        t.is_pinned = !t.is_pinned;
        Ok(t.is_pinned)
    }

    fn toggle_forum_topic_closed(
        &mut self,
        peer_id: PeerId,
        topic_root_id: MsgId,
    ) -> Result<bool, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let topics = st
            .forum_topics
            .get_mut(&peer_id)
            .ok_or_else(|| "Forum not found.".to_owned())?;
        let t = topics
            .iter_mut()
            .find(|t| t.root_id == topic_root_id)
            .ok_or_else(|| "Topic not found.".to_owned())?;
        t.is_closed = !t.is_closed;
        Ok(t.is_closed)
    }

    fn load_topic_messages(&self, peer_id: PeerId, topic_root_id: MsgId) -> Vec<HistoryMessage> {
        self.state
            .lock()
            .ok()
            .and_then(|st| st.topic_messages.get(&(peer_id, topic_root_id)).cloned())
            .unwrap_or_default()
    }

    fn save_thread_draft(&mut self, thread: ThreadKey, draft: Option<ThreadDraft>) {
        if let ThreadKey::Main(peer_id) = thread {
            self.save_draft(
                peer_id,
                draft.as_ref().map(|d| DraftMessage {
                    text: d.text.clone(),
                    reply_to: d.reply_to,
                }),
            );
        }
        if let Ok(mut st) = self.state.lock() {
            match draft {
                Some(d) if !d.text.trim().is_empty() || d.reply_to.is_some() => {
                    st.thread_drafts.insert(thread, d);
                }
                _ => {
                    st.thread_drafts.remove(&thread);
                }
            }
        }
    }

    fn load_thread_draft(&self, thread: ThreadKey) -> Option<ThreadDraft> {
        if let Ok(st) = self.state.lock()
            && let Some(d) = st.thread_drafts.get(&thread).cloned()
        {
            return Some(d);
        }
        if let ThreadKey::Main(peer_id) = thread {
            self.load_draft(peer_id).map(|d| ThreadDraft {
                text: d.text,
                reply_to: d.reply_to,
                reply_quote: None,
                no_webpage: false,
            })
        } else {
            None
        }
    }

    fn as_bots_backend(&self) -> Option<&dyn BotsBackend> {
        Some(self)
    }

    fn as_bots_backend_mut(&mut self) -> Option<&mut dyn BotsBackend> {
        Some(self)
    }

    fn as_layer233_extras(
        &self,
    ) -> Option<&dyn tg_app_history::history::backend::Layer233ExtrasBackend> {
        Some(self)
    }

    fn as_layer233_extras_mut(
        &mut self,
    ) -> Option<&mut dyn tg_app_history::history::backend::Layer233ExtrasBackend> {
        Some(self)
    }

    fn join_requests_bar(&self, peer_id: PeerId) -> Option<JoinRequestsSummary> {
        let summary = InfoBackend::join_requests_summary(self, peer_id);
        (summary.count > 0).then_some(summary)
    }

    fn join_requests_entries(&self, peer_id: PeerId, query: &str) -> Vec<InviteImporterInfo> {
        InfoBackend::chat_invite_importers(self, peer_id, None, true, query)
            .map(|r| r.importers)
            .unwrap_or_default()
    }

    fn decide_join_request(
        &mut self,
        peer_id: PeerId,
        user_id: PeerId,
        approved: bool,
    ) -> Result<(), String> {
        InfoBackend::hide_chat_join_request(self, peer_id, user_id, approved)
    }

    fn decide_all_join_requests(
        &mut self,
        peer_id: PeerId,
        approved: bool,
    ) -> Result<usize, String> {
        InfoBackend::hide_all_chat_join_requests(self, peer_id, None, approved)
    }

    fn widget_query(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        data: &[u8],
        _with_wallet: bool,
    ) -> Result<tg_tgw::WidgetRequestOutcome, String> {
        let req_text = String::from_utf8_lossy(data);
        let st = self.state.lock().map_err(|_| "lock poisoned".to_owned())?;
        let exists = st
            .histories
            .get(&peer_id)
            .is_some_and(|list| list.iter().any(|m| m.id == msg_id));
        if !exists {
            return Ok(tg_tgw::WidgetRequestOutcome::err(
                tg_tgw::WidgetRequestFailure::Failed,
            ));
        }
        Ok(tg_tgw::WidgetRequestOutcome::ok(Some(format!(
            "{{\"ok\":true,\"echo\":\"{req_text}\"}}"
        ))))
    }

    fn widget_poll_public_data(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        hash: i64,
    ) -> Result<tg_tgw::PublicDataPollResult, String> {
        let st = self.state.lock().map_err(|_| "lock poisoned".to_owned())?;
        let msg = st
            .histories
            .get(&peer_id)
            .and_then(|list| list.iter().find(|m| m.id == msg_id));
        let Some(HistoryMedia::Widget(w)) = msg.and_then(|m| m.media.as_ref()) else {
            return Err("Message does not contain a widget.".to_owned());
        };
        let current_hash = w.public_data_hash.unwrap_or(1);
        if hash != 0 && hash == current_hash {
            Ok(tg_tgw::PublicDataPollResult::NotModified {
                period_ms: tg_tgw::DEFAULT_PUBLIC_DATA_POLL_MS,
            })
        } else {
            Ok(tg_tgw::PublicDataPollResult::Updated {
                hash: current_hash,
                data: w
                    .public_data
                    .clone()
                    .unwrap_or_else(|| "{\"status\":\"live\"}".to_owned()),
                period_ms: tg_tgw::DEFAULT_PUBLIC_DATA_POLL_MS,
            })
        }
    }

    fn widget_edit_data(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        public_data: Option<&str>,
        chat_data: Option<&str>,
    ) -> Result<(), String> {
        let mut st = self.state.lock().map_err(|_| "lock poisoned".to_owned())?;
        let Some(msg) = st
            .histories
            .get_mut(&peer_id)
            .and_then(|list| list.iter_mut().find(|m| m.id == msg_id))
        else {
            return Err("Widget message not found.".to_owned());
        };
        if let Some(HistoryMedia::Widget(ref mut w)) = msg.media {
            if let Some(pd) = public_data {
                w.public_data = Some(pd.to_owned());
                w.public_data_hash = Some(w.public_data_hash.unwrap_or(0).wrapping_add(1));
            }
            if let Some(cd) = chat_data {
                w.chat_data = Some(cd.to_owned());
            }
            Ok(())
        } else {
            Err("Message does not contain a widget.".to_owned())
        }
    }
    fn business_bot_bar(
        &self,
        peer_id: PeerId,
    ) -> Option<tg_app_history::history::backend::BusinessBotBarState> {
        self.state
            .lock()
            .ok()
            .and_then(|st| st.profile_extras.business_bot_bar(peer_id))
    }

    fn toggle_business_bot_paused(&mut self, peer_id: PeerId, paused: bool) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.profile_extras
            .toggle_business_bot_paused(peer_id, paused)
    }

    fn disable_business_bot(&mut self, peer_id: PeerId) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.profile_extras.disable_business_bot(peer_id)
    }

    fn pending_media_uploads(
        &self,
        peer_id: PeerId,
    ) -> Vec<tg_app_history::history::backend::MediaUploadStatus> {
        self.state
            .lock()
            .map(|st| {
                st.pending_uploads
                    .iter()
                    .filter(|u| u.peer_id == peer_id && !u.cancelled)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn cancel_media_upload(&mut self, upload_id: u64) -> bool {
        let Ok(mut st) = self.state.lock() else {
            return false;
        };
        if let Some(item) = st
            .pending_uploads
            .iter_mut()
            .find(|u| u.upload_id == upload_id && !u.cancelled)
        {
            item.cancelled = true;
            true
        } else {
            false
        }
    }

    fn message_summary(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Option<tg_app_history::history::backend::MessageSummaryEntry> {
        self.state
            .lock()
            .ok()
            .and_then(|st| st.summaries.get(&(peer_id, msg_id)).cloned())
    }

    fn toggle_message_summary(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        to_lang: Option<&str>,
    ) -> Result<tg_app_history::history::backend::MessageSummaryEntry, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        if let Some(entry) = st.summaries.get_mut(&(peer_id, msg_id)) {
            entry.shown = !entry.shown;
            return Ok(entry.clone());
        }
        let source_text = st
            .histories
            .get(&peer_id)
            .and_then(|list| list.iter().find(|m| m.id == msg_id))
            .map(|m| m.text.text.clone())
            .unwrap_or_default();
        let trimmed = source_text.trim();
        let summary_body = if trimmed.is_empty() {
            "Key points summarized by AI.".to_owned()
        } else {
            let short: String = trimmed.chars().take(96).collect();
            format!("Summary: {short}")
        };
        let entry = tg_app_history::history::backend::MessageSummaryEntry {
            text: TextWithEntities::simple(summary_body),
            shown: true,
            loading: false,
            premium_required: false,
            to_lang: to_lang.unwrap_or("en").to_owned(),
        };
        st.summaries.insert((peer_id, msg_id), entry.clone());
        Ok(entry)
    }

    fn apply_suggested_birthday(
        &mut self,
        day: u8,
        month: u8,
        year: Option<u16>,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let _ = SettingsBackend::update_birthday(
            &mut st.settings_store,
            Some(SettingsBirthday { day, month, year }),
        );
        Ok(())
    }

    fn apply_suggested_profile_photo(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        photo_id: i64,
        _access_hash: i64,
        _file_reference: &[u8],
        _is_video: bool,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.applied_userpic_photo_id = Some(photo_id);
        Ok(())
    }

    fn unread_things_counts(
        &self,
        scope: tg_app_core::core_types::dto::engagement::UnreadThingsScope,
    ) -> tg_app_core::core_types::dto::engagement::UnreadThingsCounts {
        let Ok(st) = self.state.lock() else {
            return tg_app_core::core_types::dto::engagement::UnreadThingsCounts::default();
        };
        tg_app_core::core_types::dto::engagement::UnreadThingsCounts {
            mentions: st
                .unread_mentions
                .get(&scope.peer_id)
                .map_or(0, |v| v.len() as i32),
            reactions: st
                .unread_reactions
                .get(&scope.peer_id)
                .map_or(0, |v| v.len() as i32),
            poll_votes: st
                .unread_poll_votes
                .get(&scope.peer_id)
                .map_or(0, |v| v.len() as i32),
        }
    }

    fn load_unread_things_page(
        &mut self,
        scope: tg_app_core::core_types::dto::engagement::UnreadThingsScope,
        kind: tg_app_core::core_types::dto::engagement::UnreadThingsKind,
    ) -> Result<tg_app_core::history::backend::UnreadThingsSlice, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let ids = match kind {
            tg_app_core::core_types::dto::engagement::UnreadThingsKind::Mentions => {
                st.unread_mentions.get(&scope.peer_id)
            }
            tg_app_core::core_types::dto::engagement::UnreadThingsKind::Reactions => {
                st.unread_reactions.get(&scope.peer_id)
            }
            tg_app_core::core_types::dto::engagement::UnreadThingsKind::PollVotes => {
                st.unread_poll_votes.get(&scope.peer_id)
            }
        }
        .cloned()
        .unwrap_or_default();
        Ok(tg_app_core::history::backend::UnreadThingsSlice {
            kind,
            full_count: ids.len() as i32,
            loaded_ids: ids,
            messages: Vec::new(),
            has_more: false,
        })
    }

    fn read_unread_mentions(
        &mut self,
        peer_id: PeerId,
        _top_msg_id: Option<MsgId>,
    ) -> Result<tg_app_core::core_types::dto::engagement::UnreadThingsReadResult, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let removed = st
            .unread_mentions
            .remove(&peer_id)
            .map_or(0, |v| v.len() as i32);
        Ok(
            tg_app_core::core_types::dto::engagement::UnreadThingsReadResult {
                pages: 1,
                last_pts: removed,
                total_pts_count: removed,
            },
        )
    }

    fn read_unread_reactions(
        &mut self,
        peer_id: PeerId,
        _top_msg_id: Option<MsgId>,
        _saved_peer_id: Option<PeerId>,
    ) -> Result<tg_app_core::core_types::dto::engagement::UnreadThingsReadResult, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let removed = st
            .unread_reactions
            .remove(&peer_id)
            .map_or(0, |v| v.len() as i32);
        Ok(
            tg_app_core::core_types::dto::engagement::UnreadThingsReadResult {
                pages: 1,
                last_pts: removed,
                total_pts_count: removed,
            },
        )
    }

    fn read_unread_poll_votes(
        &mut self,
        peer_id: PeerId,
        _top_msg_id: Option<MsgId>,
    ) -> Result<tg_app_core::core_types::dto::engagement::UnreadThingsReadResult, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let removed = st
            .unread_poll_votes
            .remove(&peer_id)
            .map_or(0, |v| v.len() as i32);
        Ok(
            tg_app_core::core_types::dto::engagement::UnreadThingsReadResult {
                pages: 1,
                last_pts: removed,
                total_pts_count: removed,
            },
        )
    }

    fn get_message_read_participants(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Result<Vec<tg_app_core::core_types::dto::engagement::MessageReadParticipant>, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        if let Some(custom) = st.read_participants.get(&(peer_id, msg_id)) {
            return Ok(custom.clone());
        }
        let base_ts = st
            .histories
            .get(&peer_id)
            .and_then(|msgs| msgs.iter().find(|m| m.id == msg_id))
            .map_or(1_795_341_600, |m| m.date_unix);
        Ok(vec![
            tg_app_core::core_types::dto::engagement::MessageReadParticipant {
                user_id: session_peers::USER_MIRA,
                name: "Mira Vance".to_owned(),
                username: Some("mira_v".to_owned()),
                read_date_unix: base_ts + 45,
                color_index: 1,
            },
            tg_app_core::core_types::dto::engagement::MessageReadParticipant {
                user_id: session_peers::USER_SAM,
                name: "Sam Chen".to_owned(),
                username: Some("sam_c".to_owned()),
                read_date_unix: base_ts + 120,
                color_index: 3,
            },
            tg_app_core::core_types::dto::engagement::MessageReadParticipant {
                user_id: session_peers::USER_RILEY,
                name: "Riley Brooks".to_owned(),
                username: Some("riley_b".to_owned()),
                read_date_unix: base_ts + 215,
                color_index: 5,
            },
        ])
    }

    fn get_outbox_read_date(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Result<tg_app_core::core_types::dto::engagement::OutboxReadDateStatus, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        if let Some(custom) = st.outbox_read_dates.get(&(peer_id, msg_id)) {
            return Ok(*custom);
        }
        let base_ts = st
            .histories
            .get(&peer_id)
            .and_then(|msgs| msgs.iter().find(|m| m.id == msg_id))
            .map_or(1_795_341_600, |m| m.date_unix);
        Ok(
            tg_app_core::core_types::dto::engagement::OutboxReadDateStatus::Read {
                date_unix: base_ts + 95,
            },
        )
    }

    fn export_message_link(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        grouped: bool,
        thread: bool,
    ) -> Result<tg_app_core::core_types::dto::engagement::ExportedMessageLinkInfo, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let base = st
            .peer(peer_id)
            .and_then(|p| {
                p.usernames
                    .first()
                    .map(|u| format!("https://t.me/{}", u.username))
            })
            .unwrap_or_else(|| format!("https://t.me/c/{}", peer_id.bare()));
        let mut link = format!("{base}/{}", msg_id.0);
        if grouped {
            link.push_str("?single");
        } else if thread {
            link.push_str("?thread=1");
        }
        Ok(
            tg_app_core::core_types::dto::engagement::ExportedMessageLinkInfo {
                html: format!(
                    "<script async src=\"https://telegram.org/js/telegram-widget.js\" data-telegram-post=\"{}\"></script>",
                    link
                ),
                link,
            },
        )
    }

    fn get_message_author(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Result<tg_app_core::core_types::dto::engagement::MessageAuthorInfo, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        if let Some(custom) = st.message_authors.get(&(peer_id, msg_id)) {
            return Ok(custom.clone());
        }
        Ok(
            tg_app_core::core_types::dto::engagement::MessageAuthorInfo {
                user_id: session_peers::USER_MIRA,
                name: "Mira Vance".to_owned(),
                username: Some("mira_v".to_owned()),
                is_deleted: false,
            },
        )
    }

    fn future_chat_creator_after_leave(
        &self,
        peer_id: PeerId,
    ) -> Result<Option<tg_app_core::core_types::dto::engagement::FutureChatCreatorInfo>, String>
    {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.future_creators.get(&peer_id).cloned())
    }

    fn block_from_replies(
        &mut self,
        request: tg_app_core::core_types::dto::contacts_extras::BlockFromRepliesRequest,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.blocked_from_replies.push(request);
        if request.delete_message || request.delete_history {
            let mut found = None;
            for (&pid, msgs) in &st.histories {
                if let Some(m) = msgs.iter().find(|m| m.id == request.msg_id) {
                    found = Some((pid, m.from_id));
                    break;
                }
            }
            if let Some((pid, from_id)) = found
                && let Some(msgs) = st.histories.get_mut(&pid)
            {
                if request.delete_history {
                    msgs.retain(|m| m.from_id != from_id && m.id != request.msg_id);
                } else {
                    msgs.retain(|m| m.id != request.msg_id);
                }
            }
        }
        Ok(())
    }
}

impl HistoryBackend for MockSession {
    fn self_peer(&self) -> (PeerId, String) {
        self.handle.self_peer()
    }
    fn peer_info(&self, peer_id: PeerId) -> Option<HistoryPeerInfo> {
        HistoryBackend::peer_info(&self.handle, peer_id)
    }
    fn load_slice_around(
        &self,
        peer_id: PeerId,
        around_id: MsgId,
        limit_before: usize,
        limit_after: usize,
    ) -> HistorySlice {
        self.handle
            .load_slice_around(peer_id, around_id, limit_before, limit_after)
    }
    fn load_slice_before(&self, peer_id: PeerId, before_id: MsgId, limit: usize) -> HistorySlice {
        self.handle.load_slice_before(peer_id, before_id, limit)
    }
    fn load_slice_after(&self, peer_id: PeerId, after_id: MsgId, limit: usize) -> HistorySlice {
        self.handle.load_slice_after(peer_id, after_id, limit)
    }
    fn send_message(
        &mut self,
        peer_id: PeerId,
        request: SendMessageRequest,
    ) -> Result<HistoryMessage, String> {
        self.handle.send_message(peer_id, request)
    }
    fn send_saved_music(
        &mut self,
        peer_id: PeerId,
        tracks: &[tg_app_core::info::backend::SavedMusicTrack],
    ) -> Result<Vec<HistoryMessage>, String> {
        self.handle.send_saved_music(peer_id, tracks)
    }
    fn edit_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        text: TextWithEntities,
    ) -> Result<HistoryMessage, String> {
        self.handle.edit_message(peer_id, msg_id, text)
    }
    fn send_rich_message(
        &mut self,
        peer_id: PeerId,
        input: tg_tl::types::InputRichMessage,
        page: tg_iv::rich_page::RichPage,
        reply_to: Option<MsgId>,
        silent: bool,
    ) -> Result<HistoryMessage, String> {
        self.handle
            .send_rich_message(peer_id, input, page, reply_to, silent)
    }
    fn edit_rich_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        input: tg_tl::types::InputRichMessage,
        page: tg_iv::rich_page::RichPage,
    ) -> Result<HistoryMessage, String> {
        self.handle.edit_rich_message(peer_id, msg_id, input, page)
    }
    fn delete_messages(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
        revoke_for_all: bool,
    ) -> Result<(), String> {
        HistoryBackend::delete_messages(&mut self.handle, peer_id, msg_ids, revoke_for_all)
    }
    fn forward_messages(
        &mut self,
        from_peer: PeerId,
        msg_ids: &[MsgId],
        to_peer: PeerId,
    ) -> Result<Vec<HistoryMessage>, String> {
        HistoryBackend::forward_messages(&mut self.handle, from_peer, msg_ids, to_peer)
    }
    fn mark_history_read(&mut self, peer_id: PeerId, max_id: MsgId) {
        self.handle.mark_history_read(peer_id, max_id);
    }
    fn send_typing(&mut self, peer_id: PeerId, action: HistoryTypingAction) {
        self.handle.send_typing(peer_id, action);
    }
    fn toggle_reaction(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        emoji: &str,
    ) -> Result<Vec<ReactionCounter>, String> {
        self.handle.toggle_reaction(peer_id, msg_id, emoji)
    }
    fn pin_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        unpin: bool,
    ) -> Result<Vec<PinnedMessageSummary>, String> {
        self.handle.pin_message(peer_id, msg_id, unpin)
    }
    fn pinned_messages(&self, peer_id: PeerId) -> Vec<PinnedMessageSummary> {
        self.handle.pinned_messages(peer_id)
    }
    fn vote_poll(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        option_indices: &[usize],
    ) -> Result<(), String> {
        self.handle.vote_poll(peer_id, msg_id, option_indices)
    }
    fn reload_poll_results(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Result<Option<tg_app_core::history::backend::PollAttachment>, String> {
        self.handle.reload_poll_results(peer_id, msg_id)
    }
    fn poll_stats(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        dark: bool,
    ) -> Result<tg_stats::StatisticalGraph, String> {
        self.handle.poll_stats(peer_id, msg_id, dark)
    }
    fn save_draft(&mut self, peer_id: PeerId, draft: Option<DraftMessage>) {
        self.handle.save_draft(peer_id, draft);
    }
    fn load_draft(&self, peer_id: PeerId) -> Option<DraftMessage> {
        self.handle.load_draft(peer_id)
    }
    fn dismiss_contact_status(&mut self, peer_id: PeerId) {
        self.handle.dismiss_contact_status(peer_id);
    }
    fn add_contact(&mut self, peer_id: PeerId) {
        HistoryBackend::add_contact(&mut self.handle, peer_id);
    }
    fn accept_contact(&mut self, peer_id: PeerId) -> Result<(), String> {
        self.handle.accept_contact(peer_id)
    }
    fn report_spam(&mut self, peer_id: PeerId) -> Result<bool, String> {
        self.handle.report_spam(peer_id)
    }
    fn hide_peer_settings_bar(&mut self, peer_id: PeerId) -> Result<bool, String> {
        self.handle.hide_peer_settings_bar(peer_id)
    }
    fn toggle_block_peer(&mut self, peer_id: PeerId) {
        self.handle.toggle_block_peer(peer_id);
    }
    fn toggle_mute(&mut self, peer_id: PeerId) -> bool {
        self.handle.toggle_mute(peer_id)
    }
    fn send_message_extended(
        &mut self,
        peer_id: PeerId,
        request: SendMessageExtended,
    ) -> Result<HistoryMessage, String> {
        self.handle.send_message_extended(peer_id, request)
    }
    fn forward_messages_with_options(
        &mut self,
        from_peer: PeerId,
        msg_ids: &[MsgId],
        to_peer: PeerId,
        options: ForwardOptions,
    ) -> Result<Vec<HistoryMessage>, String> {
        self.handle
            .forward_messages_with_options(from_peer, msg_ids, to_peer, options)
    }
    fn replies_root_and_comments(
        &self,
        peer_id: PeerId,
        root_msg_id: MsgId,
    ) -> Option<(HistoryMessage, Vec<HistoryMessage>)> {
        self.handle.replies_root_and_comments(peer_id, root_msg_id)
    }
    fn forum_topics(&self, peer_id: PeerId) -> Vec<ForumTopic> {
        self.handle.forum_topics(peer_id)
    }
    fn create_forum_topic(
        &mut self,
        peer_id: PeerId,
        title: String,
        icon_color: u32,
        custom_emoji: Option<String>,
    ) -> Result<ForumTopic, String> {
        self.handle
            .create_forum_topic(peer_id, title, icon_color, custom_emoji)
    }
    fn toggle_forum_topic_pin(
        &mut self,
        peer_id: PeerId,
        topic_root_id: MsgId,
    ) -> Result<bool, String> {
        self.handle.toggle_forum_topic_pin(peer_id, topic_root_id)
    }
    fn toggle_forum_topic_closed(
        &mut self,
        peer_id: PeerId,
        topic_root_id: MsgId,
    ) -> Result<bool, String> {
        self.handle
            .toggle_forum_topic_closed(peer_id, topic_root_id)
    }
    fn load_topic_messages(&self, peer_id: PeerId, topic_root_id: MsgId) -> Vec<HistoryMessage> {
        self.handle.load_topic_messages(peer_id, topic_root_id)
    }
    fn save_thread_draft(&mut self, thread: ThreadKey, draft: Option<ThreadDraft>) {
        self.handle.save_thread_draft(thread, draft);
    }
    fn load_thread_draft(&self, thread: ThreadKey) -> Option<ThreadDraft> {
        self.handle.load_thread_draft(thread)
    }
    fn as_bots_backend(&self) -> Option<&dyn BotsBackend> {
        Some(self)
    }
    fn as_bots_backend_mut(&mut self) -> Option<&mut dyn BotsBackend> {
        Some(self)
    }
    fn as_chat_helpers_backend(
        &self,
    ) -> Option<&dyn tg_app_core::chat_helpers::backend::ChatHelpersBackend> {
        Some(self)
    }
    fn as_chat_helpers_backend_mut(
        &mut self,
    ) -> Option<&mut dyn tg_app_core::chat_helpers::backend::ChatHelpersBackend> {
        Some(self)
    }
    fn as_layer233_extras(
        &self,
    ) -> Option<&dyn tg_app_history::history::backend::Layer233ExtrasBackend> {
        Some(self)
    }

    fn as_layer233_extras_mut(
        &mut self,
    ) -> Option<&mut dyn tg_app_history::history::backend::Layer233ExtrasBackend> {
        Some(self)
    }
    fn join_requests_bar(&self, peer_id: PeerId) -> Option<JoinRequestsSummary> {
        HistoryBackend::join_requests_bar(&self.handle, peer_id)
    }
    fn join_requests_entries(&self, peer_id: PeerId, query: &str) -> Vec<InviteImporterInfo> {
        HistoryBackend::join_requests_entries(&self.handle, peer_id, query)
    }
    fn decide_join_request(
        &mut self,
        peer_id: PeerId,
        user_id: PeerId,
        approved: bool,
    ) -> Result<(), String> {
        HistoryBackend::decide_join_request(&mut self.handle, peer_id, user_id, approved)
    }
    fn decide_all_join_requests(
        &mut self,
        peer_id: PeerId,
        approved: bool,
    ) -> Result<usize, String> {
        HistoryBackend::decide_all_join_requests(&mut self.handle, peer_id, approved)
    }
    fn widget_query(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        data: &[u8],
        with_wallet: bool,
    ) -> Result<tg_tgw::WidgetRequestOutcome, String> {
        self.handle.widget_query(peer_id, msg_id, data, with_wallet)
    }
    fn widget_poll_public_data(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        hash: i64,
    ) -> Result<tg_tgw::PublicDataPollResult, String> {
        self.handle.widget_poll_public_data(peer_id, msg_id, hash)
    }
    fn widget_edit_data(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        public_data: Option<&str>,
        chat_data: Option<&str>,
    ) -> Result<(), String> {
        self.handle
            .widget_edit_data(peer_id, msg_id, public_data, chat_data)
    }
    fn business_bot_bar(
        &self,
        peer_id: PeerId,
    ) -> Option<tg_app_history::history::backend::BusinessBotBarState> {
        HistoryBackend::business_bot_bar(&self.handle, peer_id)
    }
    fn toggle_business_bot_paused(&mut self, peer_id: PeerId, paused: bool) -> Result<(), String> {
        HistoryBackend::toggle_business_bot_paused(&mut self.handle, peer_id, paused)
    }
    fn disable_business_bot(&mut self, peer_id: PeerId) -> Result<(), String> {
        HistoryBackend::disable_business_bot(&mut self.handle, peer_id)
    }
    fn send_poll_with_extras(
        &mut self,
        peer_id: PeerId,
        thread_key: Option<ThreadKey>,
        request: tg_app_history::history::sections_model::CreatePollRequest,
        extras: tg_app_history::history::sections_model::PollMessageExtras,
    ) -> Result<HistoryMessage, String> {
        let mut msg = HistoryBackend::send_poll(&mut self.handle, peer_id, thread_key, request)?;
        msg.extra.poll_extras = Some(extras.clone());
        let mut st = self.handle.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(list) = st.histories.get_mut(&peer_id)
            && let Some(stored) = list.iter_mut().find(|m| m.id == msg.id)
        {
            stored.extra.poll_extras = Some(extras);
        }
        Ok(msg)
    }
    fn add_poll_option(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        request: tg_app_history::history::sections_model::AddPollOptionRequest,
    ) -> Result<HistoryMessage, String> {
        let label = request.text.trim().to_owned();
        if label.is_empty() && request.attachment.is_none() {
            return Err("Poll option cannot be empty.".to_owned());
        }
        let mut st = self.handle.state.lock().unwrap_or_else(|e| e.into_inner());
        let self_name = st.user.name.clone();
        let list = st
            .histories
            .get_mut(&peer_id)
            .ok_or_else(|| "Chat history not found.".to_owned())?;
        let msg = list
            .iter_mut()
            .find(|m| m.id == msg_id)
            .ok_or_else(|| "Poll message not found.".to_owned())?;
        let Some(HistoryMedia::Poll(ref mut poll)) = msg.media else {
            return Err("Target message is not a poll.".to_owned());
        };
        if poll.closed {
            return Err("Poll is already closed.".to_owned());
        }
        let option_text = if label.is_empty() {
            request
                .attachment
                .as_ref()
                .map_or_else(String::new, |a| a.display_badge())
        } else {
            label
        };
        poll.options.push(PollOption {
            text: option_text,
            votes: 0,
            chosen: false,
            correct: false,
        });
        let extras = msg.extra.poll_extras.get_or_insert_with(Default::default);
        extras.open_answers = true;
        let next_idx = extras.option_bytes.len() as u8;
        extras.option_bytes.push(vec![next_idx]);
        extras.option_attachments.push(request.attachment);
        extras.option_added_by.push(Some(self_name));
        Ok(msg.clone())
    }
    fn delete_poll_option(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        option_bytes: &[u8],
    ) -> Result<(), String> {
        let mut st = self.handle.state.lock().unwrap_or_else(|e| e.into_inner());
        let list = st
            .histories
            .get_mut(&peer_id)
            .ok_or_else(|| "Chat history not found.".to_owned())?;
        let msg = list
            .iter_mut()
            .find(|m| m.id == msg_id)
            .ok_or_else(|| "Poll message not found.".to_owned())?;
        if let Some(ref mut extras) = msg.extra.poll_extras
            && let Some(pos) = extras
                .option_bytes
                .iter()
                .position(|b| b.as_slice() == option_bytes)
        {
            extras.option_bytes.remove(pos);
            if pos < extras.option_attachments.len() {
                extras.option_attachments.remove(pos);
            }
            if pos < extras.option_added_by.len() {
                extras.option_added_by.remove(pos);
            }
            if let Some(HistoryMedia::Poll(ref mut poll)) = msg.media
                && pos < poll.options.len()
            {
                poll.options.remove(pos);
            }
            return Ok(());
        }
        Err("Poll option not found.".to_owned())
    }
    fn mark_ttl_media_viewed(&mut self, peer_id: PeerId, msg_id: MsgId) -> Result<(), String> {
        tg_app_info::boxes::self_destruct::SelfDestructBackend::mark_ttl_media_viewed(
            &mut self.self_destruct,
            peer_id,
            msg_id,
        )?;
        let mut st = self.handle.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(list) = st.histories.get_mut(&peer_id)
            && let Some(msg) = list.iter_mut().find(|m| m.id == msg_id)
        {
            msg.extra.ttl_viewed = true;
        }
        Ok(())
    }
    fn as_compose_ai_backend(
        &mut self,
    ) -> Option<&mut dyn tg_app_info::boxes::compose_ai::ComposeAiBackend> {
        Some(&mut self.compose_ai)
    }
    fn as_sticker_creator_backend(
        &mut self,
    ) -> Option<&mut dyn tg_app_info::boxes::sticker_creator::StickerCreatorBackend> {
        Some(&mut self.sticker_creator)
    }
    fn as_self_destruct_backend(
        &mut self,
    ) -> Option<&mut dyn tg_app_info::boxes::self_destruct::SelfDestructBackend> {
        Some(&mut self.self_destruct)
    }

    fn pending_media_uploads(
        &self,
        peer_id: PeerId,
    ) -> Vec<tg_app_history::history::backend::MediaUploadStatus> {
        HistoryBackend::pending_media_uploads(&self.handle, peer_id)
    }

    fn cancel_media_upload(&mut self, upload_id: u64) -> bool {
        HistoryBackend::cancel_media_upload(&mut self.handle, upload_id)
    }

    fn message_summary(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Option<tg_app_history::history::backend::MessageSummaryEntry> {
        HistoryBackend::message_summary(&self.handle, peer_id, msg_id)
    }

    fn toggle_message_summary(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        to_lang: Option<&str>,
    ) -> Result<tg_app_history::history::backend::MessageSummaryEntry, String> {
        HistoryBackend::toggle_message_summary(&mut self.handle, peer_id, msg_id, to_lang)
    }

    fn apply_suggested_birthday(
        &mut self,
        day: u8,
        month: u8,
        year: Option<u16>,
    ) -> Result<(), String> {
        HistoryBackend::apply_suggested_birthday(&mut self.handle, day, month, year)
    }

    fn apply_suggested_profile_photo(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        photo_id: i64,
        access_hash: i64,
        file_reference: &[u8],
        is_video: bool,
    ) -> Result<(), String> {
        HistoryBackend::apply_suggested_profile_photo(
            &mut self.handle,
            peer_id,
            msg_id,
            photo_id,
            access_hash,
            file_reference,
            is_video,
        )
    }

    fn unread_things_counts(
        &self,
        scope: tg_app_core::core_types::dto::engagement::UnreadThingsScope,
    ) -> tg_app_core::core_types::dto::engagement::UnreadThingsCounts {
        HistoryBackend::unread_things_counts(&self.handle, scope)
    }

    fn load_unread_things_page(
        &mut self,
        scope: tg_app_core::core_types::dto::engagement::UnreadThingsScope,
        kind: tg_app_core::core_types::dto::engagement::UnreadThingsKind,
    ) -> Result<tg_app_core::history::backend::UnreadThingsSlice, String> {
        HistoryBackend::load_unread_things_page(&mut self.handle, scope, kind)
    }

    fn read_unread_mentions(
        &mut self,
        peer_id: PeerId,
        top_msg_id: Option<MsgId>,
    ) -> Result<tg_app_core::core_types::dto::engagement::UnreadThingsReadResult, String> {
        HistoryBackend::read_unread_mentions(&mut self.handle, peer_id, top_msg_id)
    }

    fn read_unread_reactions(
        &mut self,
        peer_id: PeerId,
        top_msg_id: Option<MsgId>,
        saved_peer_id: Option<PeerId>,
    ) -> Result<tg_app_core::core_types::dto::engagement::UnreadThingsReadResult, String> {
        HistoryBackend::read_unread_reactions(&mut self.handle, peer_id, top_msg_id, saved_peer_id)
    }

    fn read_unread_poll_votes(
        &mut self,
        peer_id: PeerId,
        top_msg_id: Option<MsgId>,
    ) -> Result<tg_app_core::core_types::dto::engagement::UnreadThingsReadResult, String> {
        HistoryBackend::read_unread_poll_votes(&mut self.handle, peer_id, top_msg_id)
    }

    fn get_message_read_participants(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Result<Vec<tg_app_core::core_types::dto::engagement::MessageReadParticipant>, String> {
        HistoryBackend::get_message_read_participants(&self.handle, peer_id, msg_id)
    }

    fn get_outbox_read_date(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Result<tg_app_core::core_types::dto::engagement::OutboxReadDateStatus, String> {
        HistoryBackend::get_outbox_read_date(&self.handle, peer_id, msg_id)
    }

    fn export_message_link(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        grouped: bool,
        thread: bool,
    ) -> Result<tg_app_core::core_types::dto::engagement::ExportedMessageLinkInfo, String> {
        HistoryBackend::export_message_link(&self.handle, peer_id, msg_id, grouped, thread)
    }

    fn get_message_author(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Result<tg_app_core::core_types::dto::engagement::MessageAuthorInfo, String> {
        HistoryBackend::get_message_author(&self.handle, peer_id, msg_id)
    }

    fn future_chat_creator_after_leave(
        &self,
        peer_id: PeerId,
    ) -> Result<Option<tg_app_core::core_types::dto::engagement::FutureChatCreatorInfo>, String>
    {
        HistoryBackend::future_chat_creator_after_leave(&self.handle, peer_id)
    }

    fn block_from_replies(
        &mut self,
        request: tg_app_core::core_types::dto::contacts_extras::BlockFromRepliesRequest,
    ) -> Result<(), String> {
        HistoryBackend::block_from_replies(&mut self.handle, request)
    }
}

// =============================================================================
// 3a. history::backend::Layer233ExtrasBackend (Checklists, Sponsored, FactChecks, Monoforum)
// =============================================================================

impl tg_app_history::history::backend::Layer233ExtrasBackend for SharedSessionHandle {
    fn create_todo_list(
        &mut self,
        peer_id: PeerId,
        request: tg_app_history::history::todo::CreateTodoListRequest,
    ) -> Result<HistoryMessage, String> {
        use tg_app_history::history::todo::{
            TODO_ITEM_TEXT_LIMIT, TODO_MAX_ITEMS, TODO_TITLE_LIMIT, TodoListAttachment,
            TodoTaskItem, validate_todo_list,
        };
        validate_todo_list(
            &request,
            TODO_TITLE_LIMIT,
            TODO_ITEM_TEXT_LIMIT,
            TODO_MAX_ITEMS,
        )
        .map_err(|e| e.message())?;
        let mut st = self.state.lock().map_err(|_| "lock poisoned".to_owned())?;
        let msg_id = MsgId(st.next_msg_id);
        st.next_msg_id += 1;
        let items = request
            .items
            .into_iter()
            .enumerate()
            .map(|(i, it)| TodoTaskItem {
                id: if it.id > 0 { it.id } else { (i + 1) as i32 },
                text: it.text,
                completed_by: None,
                completed_by_name: None,
                completion_date: 0,
            })
            .collect();
        let msg = HistoryMessage {
            extra: MessageExtra::default(),
            id: msg_id,
            peer_id,
            from_id: session_peers::SELF,
            sender_name: st.user.name.clone(),
            date_unix: BASE_UNIX_TODAY + 12_000,
            outgoing: true,
            is_post: false,
            text: TextWithEntities::default(),
            media: Some(HistoryMedia::TodoList(TodoListAttachment {
                title: request.title,
                items,
                others_can_append: request.others_can_append,
                others_can_complete: request.others_can_complete,
            })),
            service: None,
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: true,
            is_sending: false,
            is_pinned: false,
        };
        st.histories.entry(peer_id).or_default().push(msg.clone());
        Ok(msg)
    }

    fn edit_todo_list(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        request: tg_app_history::history::todo::CreateTodoListRequest,
    ) -> Result<HistoryMessage, String> {
        use tg_app_history::history::todo::{
            TODO_ITEM_TEXT_LIMIT, TODO_MAX_ITEMS, TODO_TITLE_LIMIT, TodoTaskItem,
            validate_todo_list,
        };
        validate_todo_list(
            &request,
            TODO_TITLE_LIMIT,
            TODO_ITEM_TEXT_LIMIT,
            TODO_MAX_ITEMS,
        )
        .map_err(|e| e.message())?;
        let mut st = self.state.lock().map_err(|_| "lock poisoned".to_owned())?;
        let msgs = st
            .histories
            .get_mut(&peer_id)
            .ok_or_else(|| "Peer not found.".to_owned())?;
        let target = msgs
            .iter_mut()
            .find(|m| m.id == msg_id)
            .ok_or_else(|| "Checklist message not found.".to_owned())?;
        let Some(HistoryMedia::TodoList(ref mut todo)) = target.media else {
            return Err("Message is not a checklist.".to_owned());
        };
        let old_items = std::mem::take(&mut todo.items);
        todo.title = request.title;
        todo.others_can_append = request.others_can_append;
        todo.others_can_complete = request.others_can_complete;
        todo.items = request
            .items
            .into_iter()
            .enumerate()
            .map(|(i, it)| {
                let id = if it.id > 0 { it.id } else { (i + 1) as i32 };
                let prev = old_items.iter().find(|o| o.id == id);
                TodoTaskItem {
                    id,
                    text: it.text,
                    completed_by: prev.and_then(|o| o.completed_by),
                    completed_by_name: prev.and_then(|o| o.completed_by_name.clone()),
                    completion_date: prev.map_or(0, |o| o.completion_date),
                }
            })
            .collect();
        target.edited = true;
        Ok(target.clone())
    }

    fn toggle_todo_completed(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        completed: &[i32],
        incompleted: &[i32],
    ) -> Result<Option<HistoryMessage>, String> {
        let mut st = self.state.lock().map_err(|_| "lock poisoned".to_owned())?;
        let actor_name = st.user.name.clone();
        let msgs = st
            .histories
            .get_mut(&peer_id)
            .ok_or_else(|| "Peer not found.".to_owned())?;
        let Some(target) = msgs.iter_mut().find(|m| m.id == msg_id) else {
            return Ok(None);
        };
        if let Some(HistoryMedia::TodoList(ref mut todo)) = target.media {
            todo.apply_completions(
                session_peers::SELF,
                &actor_name,
                BASE_UNIX_TODAY + 12_100,
                completed,
                incompleted,
            );
            return Ok(Some(target.clone()));
        }
        Ok(None)
    }

    fn append_todo_list(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        items: Vec<tg_app_history::history::todo::TodoItemInput>,
    ) -> Result<Option<HistoryMessage>, String> {
        let mut st = self.state.lock().map_err(|_| "lock poisoned".to_owned())?;
        let msgs = st
            .histories
            .get_mut(&peer_id)
            .ok_or_else(|| "Peer not found.".to_owned())?;
        let Some(target) = msgs.iter_mut().find(|m| m.id == msg_id) else {
            return Ok(None);
        };
        if let Some(HistoryMedia::TodoList(ref mut todo)) = target.media {
            todo.append_items(&items);
            return Ok(Some(target.clone()));
        }
        Ok(None)
    }

    fn is_sponsored_hidden(&self) -> bool {
        self.state
            .lock()
            .map(|st| st.layer233.sponsored.ads_suppressed())
            .unwrap_or(false)
    }

    fn set_sponsored_hidden(&mut self, hide_ads: bool) -> Result<(), String> {
        let mut st = self.state.lock().map_err(|_| "lock poisoned".to_owned())?;
        st.layer233.sponsored.set_premium_hide_ads(true, hide_ads);
        Ok(())
    }

    fn get_sponsored_messages(
        &mut self,
        peer_id: PeerId,
        _msg_id: Option<MsgId>,
    ) -> Result<Vec<tg_app_history::history::sponsored::SponsoredMessageItem>, String> {
        let st = self.state.lock().map_err(|_| "lock poisoned".to_owned())?;
        Ok(st.layer233.sponsored.list_for_peer(peer_id).to_vec())
    }

    fn view_sponsored_message(&mut self, random_id: &[u8]) -> Result<(), String> {
        let mut st = self.state.lock().map_err(|_| "lock poisoned".to_owned())?;
        if st.layer233.sponsored.record_view(random_id, 2_000) {
            st.layer233.viewed_ads.push(random_id.to_vec());
        }
        Ok(())
    }

    fn click_sponsored_message(
        &mut self,
        random_id: &[u8],
        is_media: bool,
        is_fullscreen: bool,
    ) -> Result<(), String> {
        let mut st = self.state.lock().map_err(|_| "lock poisoned".to_owned())?;
        st.layer233
            .clicked_ads
            .push((random_id.to_vec(), is_media, is_fullscreen));
        Ok(())
    }

    fn report_sponsored_message(
        &mut self,
        random_id: &[u8],
        option: &[u8],
    ) -> Result<tg_app_history::history::sponsored::SponsoredReportOutcome, String> {
        use tg_app_history::history::sponsored::{SponsoredReportOption, SponsoredReportOutcome};
        let mut st = self.state.lock().map_err(|_| "lock poisoned".to_owned())?;
        if option.is_empty() {
            return Ok(SponsoredReportOutcome::ChooseOption {
                title: "Report Sponsored Message".to_owned(),
                options: vec![
                    SponsoredReportOption {
                        option: b"spam".to_vec(),
                        text: "Spam or misleading".to_owned(),
                    },
                    SponsoredReportOption {
                        option: b"1".to_vec(),
                        text: "I don't like it".to_owned(),
                    },
                    SponsoredReportOption {
                        option: b"hide_all".to_vec(),
                        text: "Hide all ads (Premium)".to_owned(),
                    },
                ],
            });
        }
        if option == b"-1" || option == b"1" {
            st.layer233.sponsored.remove_by_random_id(random_id);
            return Ok(SponsoredReportOutcome::Silence);
        }
        if option == b"hide_all" {
            st.layer233.sponsored.set_premium_hide_ads(true, true);
            return Ok(SponsoredReportOutcome::AdsHidden);
        }
        st.layer233.sponsored.remove_by_random_id(random_id);
        Ok(SponsoredReportOutcome::Reported)
    }

    fn cached_factcheck(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Option<tg_app_history::history::factcheck::FactCheckData> {
        self.state
            .lock()
            .ok()?
            .layer233
            .factchecks
            .get(&(peer_id, msg_id))
            .cloned()
            .filter(|fc| !fc.is_empty())
    }

    fn can_edit_factcheck_config(&self) -> bool {
        self.state
            .lock()
            .map(|st| st.layer233.can_edit_factcheck)
            .unwrap_or(false)
    }

    fn get_factchecks(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
    ) -> Result<Vec<(MsgId, tg_app_history::history::factcheck::FactCheckData)>, String> {
        let st = self.state.lock().map_err(|_| "lock poisoned".to_owned())?;
        Ok(msg_ids
            .iter()
            .map(|&mid| {
                let fc = st
                    .layer233
                    .factchecks
                    .get(&(peer_id, mid))
                    .cloned()
                    .unwrap_or_default();
                (mid, fc)
            })
            .collect())
    }

    fn edit_factcheck(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        text: TextWithEntities,
    ) -> Result<Option<tg_app_history::history::factcheck::FactCheckData>, String> {
        let mut st = self.state.lock().map_err(|_| "lock poisoned".to_owned())?;
        if text.text.trim().is_empty() {
            st.layer233.factchecks.remove(&(peer_id, msg_id));
            return Ok(None);
        }
        let fc = tg_app_history::history::factcheck::FactCheckData {
            need_check: false,
            country: Some("US".to_owned()),
            text: Some(text),
            hash: 233_999,
        };
        st.layer233.factchecks.insert((peer_id, msg_id), fc.clone());
        Ok(Some(fc))
    }

    fn monoforum_sublists(
        &mut self,
        monoforum_peer_id: PeerId,
    ) -> Vec<tg_app_history::history::monoforum::MonoforumSublist> {
        self.state
            .lock()
            .ok()
            .and_then(|st| {
                st.layer233
                    .monoforum_sublists
                    .get(&monoforum_peer_id)
                    .cloned()
            })
            .unwrap_or_default()
    }

    fn monoforum_sublist_messages(
        &mut self,
        monoforum_peer_id: PeerId,
        sublist_peer_id: PeerId,
    ) -> Vec<HistoryMessage> {
        self.state
            .lock()
            .ok()
            .and_then(|st| {
                st.layer233
                    .monoforum_messages
                    .get(&(monoforum_peer_id, sublist_peer_id))
                    .cloned()
            })
            .unwrap_or_default()
    }

    fn mark_monoforum_sublist_read(
        &mut self,
        monoforum_peer_id: PeerId,
        sublist_peer_id: PeerId,
        max_id: MsgId,
    ) -> Result<(), String> {
        let mut st = self.state.lock().map_err(|_| "lock poisoned".to_owned())?;
        if let Some(list) = st.layer233.monoforum_sublists.get_mut(&monoforum_peer_id)
            && let Some(sub) = list
                .iter_mut()
                .find(|s| s.sublist_peer_id == sublist_peer_id)
        {
            sub.apply_read_till(max_id, false);
        }
        Ok(())
    }

    fn send_monoforum_message(
        &mut self,
        monoforum_peer_id: PeerId,
        sublist_peer_id: PeerId,
        text: TextWithEntities,
        suggest: Option<tg_app_history::history::monoforum::SuggestOptions>,
    ) -> Result<HistoryMessage, String> {
        let mut st = self.state.lock().map_err(|_| "lock poisoned".to_owned())?;
        let msg_id = MsgId(st.next_msg_id);
        st.next_msg_id += 1;
        let msg = HistoryMessage {
            extra: MessageExtra::default(),
            id: msg_id,
            peer_id: monoforum_peer_id,
            from_id: session_peers::SELF,
            sender_name: st.user.name.clone(),
            date_unix: BASE_UNIX_TODAY + 12_500,
            outgoing: true,
            is_post: false,
            text: text.clone(),
            media: None,
            service: None,
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: true,
            is_sending: false,
            is_pinned: false,
        };
        st.layer233
            .monoforum_messages
            .entry((monoforum_peer_id, sublist_peer_id))
            .or_default()
            .push(msg.clone());
        if let Some(opts) = suggest {
            st.layer233.suggested_posts.insert(
                (monoforum_peer_id, msg_id),
                tg_app_history::history::monoforum::SuggestedPostData {
                    price: opts.price,
                    schedule_date: opts.schedule_date,
                    accepted: false,
                    rejected: false,
                    paid_stars: false,
                    paid_ton: false,
                },
            );
        }
        if let Some(list) = st.layer233.monoforum_sublists.get_mut(&monoforum_peer_id)
            && let Some(sub) = list
                .iter_mut()
                .find(|s| s.sublist_peer_id == sublist_peer_id)
        {
            sub.top_message_id = msg_id;
            sub.last_preview = text.text;
        }
        Ok(msg)
    }

    fn suggested_post_for(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Option<tg_app_history::history::monoforum::SuggestedPostData> {
        self.state
            .lock()
            .ok()?
            .layer233
            .suggested_posts
            .get(&(peer_id, msg_id))
            .cloned()
    }

    fn toggle_suggested_post_approval(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        decision: tg_app_history::history::monoforum::SuggestApprovalDecision,
    ) -> Result<(), String> {
        use tg_app_history::history::monoforum::SuggestApprovalDecision;
        let mut st = self.state.lock().map_err(|_| "lock poisoned".to_owned())?;
        let entry = st
            .layer233
            .suggested_posts
            .entry((peer_id, msg_id))
            .or_default();
        match decision {
            SuggestApprovalDecision::Approve { schedule_date } => {
                entry.accepted = true;
                entry.rejected = false;
                if schedule_date.is_some() {
                    entry.schedule_date = schedule_date;
                }
            }
            SuggestApprovalDecision::Reject { .. } => {
                entry.rejected = true;
                entry.accepted = false;
            }
        }
        Ok(())
    }
}

impl tg_app_history::history::backend::Layer233ExtrasBackend for MockSession {
    fn create_todo_list(
        &mut self,
        peer_id: PeerId,
        request: tg_app_history::history::todo::CreateTodoListRequest,
    ) -> Result<HistoryMessage, String> {
        self.handle.create_todo_list(peer_id, request)
    }
    fn edit_todo_list(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        request: tg_app_history::history::todo::CreateTodoListRequest,
    ) -> Result<HistoryMessage, String> {
        self.handle.edit_todo_list(peer_id, msg_id, request)
    }
    fn toggle_todo_completed(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        completed: &[i32],
        incompleted: &[i32],
    ) -> Result<Option<HistoryMessage>, String> {
        self.handle
            .toggle_todo_completed(peer_id, msg_id, completed, incompleted)
    }
    fn append_todo_list(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        items: Vec<tg_app_history::history::todo::TodoItemInput>,
    ) -> Result<Option<HistoryMessage>, String> {
        self.handle.append_todo_list(peer_id, msg_id, items)
    }
    fn is_sponsored_hidden(&self) -> bool {
        self.handle.is_sponsored_hidden()
    }
    fn set_sponsored_hidden(&mut self, hide_ads: bool) -> Result<(), String> {
        self.handle.set_sponsored_hidden(hide_ads)
    }
    fn get_sponsored_messages(
        &mut self,
        peer_id: PeerId,
        msg_id: Option<MsgId>,
    ) -> Result<Vec<tg_app_history::history::sponsored::SponsoredMessageItem>, String> {
        self.handle.get_sponsored_messages(peer_id, msg_id)
    }
    fn view_sponsored_message(&mut self, random_id: &[u8]) -> Result<(), String> {
        self.handle.view_sponsored_message(random_id)
    }
    fn click_sponsored_message(
        &mut self,
        random_id: &[u8],
        is_media: bool,
        is_fullscreen: bool,
    ) -> Result<(), String> {
        self.handle
            .click_sponsored_message(random_id, is_media, is_fullscreen)
    }
    fn report_sponsored_message(
        &mut self,
        random_id: &[u8],
        option: &[u8],
    ) -> Result<tg_app_history::history::sponsored::SponsoredReportOutcome, String> {
        self.handle.report_sponsored_message(random_id, option)
    }
    fn cached_factcheck(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Option<tg_app_history::history::factcheck::FactCheckData> {
        self.handle.cached_factcheck(peer_id, msg_id)
    }
    fn can_edit_factcheck_config(&self) -> bool {
        self.handle.can_edit_factcheck_config()
    }
    fn get_factchecks(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
    ) -> Result<Vec<(MsgId, tg_app_history::history::factcheck::FactCheckData)>, String> {
        self.handle.get_factchecks(peer_id, msg_ids)
    }
    fn edit_factcheck(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        text: TextWithEntities,
    ) -> Result<Option<tg_app_history::history::factcheck::FactCheckData>, String> {
        self.handle.edit_factcheck(peer_id, msg_id, text)
    }
    fn monoforum_sublists(
        &mut self,
        monoforum_peer_id: PeerId,
    ) -> Vec<tg_app_history::history::monoforum::MonoforumSublist> {
        self.handle.monoforum_sublists(monoforum_peer_id)
    }
    fn monoforum_sublist_messages(
        &mut self,
        monoforum_peer_id: PeerId,
        sublist_peer_id: PeerId,
    ) -> Vec<HistoryMessage> {
        self.handle
            .monoforum_sublist_messages(monoforum_peer_id, sublist_peer_id)
    }
    fn mark_monoforum_sublist_read(
        &mut self,
        monoforum_peer_id: PeerId,
        sublist_peer_id: PeerId,
        max_id: MsgId,
    ) -> Result<(), String> {
        self.handle
            .mark_monoforum_sublist_read(monoforum_peer_id, sublist_peer_id, max_id)
    }
    fn send_monoforum_message(
        &mut self,
        monoforum_peer_id: PeerId,
        sublist_peer_id: PeerId,
        text: TextWithEntities,
        suggest: Option<tg_app_history::history::monoforum::SuggestOptions>,
    ) -> Result<HistoryMessage, String> {
        self.handle
            .send_monoforum_message(monoforum_peer_id, sublist_peer_id, text, suggest)
    }
    fn suggested_post_for(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Option<tg_app_history::history::monoforum::SuggestedPostData> {
        self.handle.suggested_post_for(peer_id, msg_id)
    }
    fn toggle_suggested_post_approval(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        decision: tg_app_history::history::monoforum::SuggestApprovalDecision,
    ) -> Result<(), String> {
        self.handle
            .toggle_suggested_post_approval(peer_id, msg_id, decision)
    }
}

// =============================================================================
// 3b. bots::backend::BotsBackend
// =============================================================================

impl BotsBackend for SharedSessionHandle {
    fn load_bot_chat(&self) -> BotChatSnapshot {
        self.load_bot_chat_for_peer(tg_bots::stand_in::PeerId(session_peers::BOT_CATALOG.0))
            .unwrap_or_else(|| tg_app_history::bots::mock::MockBotsBackend::new().load_bot_chat())
    }

    fn load_bot_chat_for_peer(
        &self,
        peer_id: tg_bots::stand_in::PeerId,
    ) -> Option<BotChatSnapshot> {
        let st = self.state.lock().ok()?;
        let app_peer = PeerId(peer_id.0);
        let rec = st.peer(app_peer)?;
        if rec.kind != InfoPeerKind::Bot {
            return None;
        }
        let bot_info = st
            .bot_info_map
            .get(&app_peer)
            .cloned()
            .unwrap_or_else(default_catalog_bot_info);
        let mut scoped_commands = tg_bots::commands::ScopedBotCommands::new();
        scoped_commands.set(
            tg_bots::commands::BotCommandScope::Default,
            "",
            bot_info.commands.clone(),
        );
        scoped_commands.set(
            tg_bots::commands::BotCommandScope::Users,
            "en",
            bot_info.commands.clone(),
        );

        let raw_msgs = st.histories.get(&app_peer).cloned().unwrap_or_default();
        let active_reply_keyboard = raw_msgs.iter().rev().find_map(|m| {
            m.extra
                .reply_markup
                .as_ref()
                .filter(|rm| rm.defines_reply_keyboard())
                .map(|rm| {
                    (
                        tg_bots::stand_in::FullMsgId::new(
                            peer_id,
                            tg_bots::stand_in::MsgId(m.id.0),
                        ),
                        rm.clone(),
                    )
                })
        });
        let messages = raw_msgs
            .iter()
            .map(|m| {
                let inline_markup = m
                    .extra
                    .reply_markup
                    .as_ref()
                    .filter(|rm| rm.is_inline())
                    .cloned();
                let reply_markup = m
                    .extra
                    .reply_markup
                    .as_ref()
                    .filter(|rm| rm.defines_reply_keyboard())
                    .cloned();
                BotChatMessage {
                    id: tg_bots::stand_in::FullMsgId::new(
                        peer_id,
                        tg_bots::stand_in::MsgId(m.id.0),
                    ),
                    author: m.sender_name.clone(),
                    outgoing: m.outgoing,
                    time_label: m.formatted_time(),
                    text: m.text.text.clone(),
                    attachment: match &m.media {
                        Some(HistoryMedia::Game(g)) => BotMessageAttachment::Game {
                            game: g.game.clone(),
                            high_scores: g.high_scores.clone(),
                        },
                        _ => BotMessageAttachment::None,
                    },
                    inline_markup,
                    reply_markup,
                    via_bot: m.extra.via_bot.as_ref().map(|u| format!("@{u}")),
                }
            })
            .collect();

        Some(BotChatSnapshot {
            peer_id,
            peer_kind: tg_bots::stand_in::PeerKind::Bot,
            peer_bot_status: tg_bots::commands::BotStatus::HasBots,
            bot_id: tg_bots::stand_in::UserId(app_peer.bare()),
            bot_name: rec.title.clone(),
            bot_username: rec
                .primary_username()
                .unwrap_or("catalog_demo_bot")
                .to_owned(),
            bot_verified: false,
            bot_trusted_for_game: rec.verified,
            bot_info,
            scoped_commands,
            current_user_name: st.user.name.clone(),
            current_user_phone: st.user.phone.clone(),
            messages,
            active_reply_keyboard,
        })
    }

    fn resolve_inline_bot(&self, username: &str) -> Option<(tg_bots::stand_in::UserId, String)> {
        let st = self.state.lock().ok()?;
        st.peers.iter().find_map(|p| {
            if p.kind != InfoPeerKind::Bot {
                return None;
            }
            let uname = p.primary_username()?;
            uname
                .eq_ignore_ascii_case(username)
                .then(|| (tg_bots::stand_in::UserId(p.id.bare()), uname.to_owned()))
        })
    }

    fn commands_for_peer(
        &self,
        peer_id: tg_bots::stand_in::PeerId,
    ) -> Vec<tg_bots::commands::BotCommand> {
        let Ok(st) = self.state.lock() else {
            return Vec::new();
        };
        let app_peer = PeerId(peer_id.0);
        if let Some(info) = st.bot_info_map.get(&app_peer) {
            return info.commands.clone();
        }
        Vec::new()
    }

    fn load_attach_menu(&self) -> tg_bots::mini_app::AttachMenuStore {
        self.state
            .lock()
            .map(|st| st.bot_attach_menu.clone())
            .unwrap_or_default()
    }

    fn candidate_peers(&self) -> Vec<BotPeerCandidate> {
        let Ok(st) = self.state.lock() else {
            return Vec::new();
        };
        st.peers
            .iter()
            .filter(|p| p.kind != InfoPeerKind::SavedMessages)
            .map(|p| {
                let (peer_kind, req_type, is_bot) = match p.kind {
                    InfoPeerKind::Bot => (
                        tg_bots::stand_in::PeerKind::Bot,
                        tg_bots::markup::RequestPeerType::User,
                        true,
                    ),
                    InfoPeerKind::Group | InfoPeerKind::Supergroup => (
                        tg_bots::stand_in::PeerKind::Group,
                        tg_bots::markup::RequestPeerType::Group,
                        false,
                    ),
                    InfoPeerKind::Channel => (
                        tg_bots::stand_in::PeerKind::Broadcast,
                        tg_bots::markup::RequestPeerType::Broadcast,
                        false,
                    ),
                    InfoPeerKind::User | InfoPeerKind::SavedMessages => (
                        tg_bots::stand_in::PeerKind::User,
                        tg_bots::markup::RequestPeerType::User,
                        false,
                    ),
                };
                let subtitle = p
                    .primary_username()
                    .map(|u| format!("@{u} • {}", p.status))
                    .unwrap_or_else(|| p.status.clone());
                BotPeerCandidate {
                    peer_id: tg_bots::stand_in::PeerId(p.id.0),
                    title: p.title.clone(),
                    subtitle,
                    peer_kind,
                    is_self: p.id == session_peers::SELF,
                    can_send_games: p.can_send,
                    is_forum: p.is_forum,
                    criteria: tg_bots::markup::RequestPeerCandidate {
                        peer_type: req_type,
                        is_bot,
                        is_premium: p.premium,
                        is_forum: p.is_forum,
                        has_username: p.primary_username().is_some(),
                        am_creator: p.can_manage_admins,
                        is_bot_participant: p.kind.is_group(),
                        my_rights: if p.can_manage_admins {
                            tg_bots::stand_in::ChatAdminRights::CHANGE_INFO
                                | tg_bots::stand_in::ChatAdminRights::INVITE_BY_LINK_OR_ADD
                                | tg_bots::stand_in::ChatAdminRights::PIN_MESSAGES
                                | tg_bots::stand_in::ChatAdminRights::POST_MESSAGES
                        } else {
                            tg_bots::stand_in::ChatAdminRights::INVITE_BY_LINK_OR_ADD
                        },
                        bot_rights: tg_bots::stand_in::ChatAdminRights::empty(),
                    },
                }
            })
            .collect()
    }

    fn send_callback_query(
        &mut self,
        request: &tg_bots::callback::CallbackQueryRequest,
    ) -> Result<tg_bots::callback::CallbackAnswerInput, String> {
        if request.is_game {
            return Ok(tg_bots::callback::CallbackAnswerInput {
                alert: false,
                has_url: true,
                native_ui: false,
                message: None,
                url: Some("https://games.example.org/star-hopper#score=3950".to_owned()),
                cache_time: 0,
            });
        }
        let payload = request.data.as_deref().unwrap_or(&[]);
        match payload {
            b"cb_toast" => Ok(tg_bots::callback::CallbackAnswerInput {
                alert: false,
                has_url: false,
                native_ui: false,
                message: Some("Catalog synced: 12 items available (cached 15s)".to_owned()),
                url: None,
                cache_time: 15,
            }),
            b"cb_alert" => Ok(tg_bots::callback::CallbackAnswerInput {
                alert: true,
                has_url: false,
                native_ui: false,
                message: Some(
                    "Order #4092 is confirmed and ready for pickup.\n\nThis is a modal callback alert (alert = true)."
                        .to_owned(),
                ),
                url: None,
                cache_time: 0,
            }),
            b"cb_2fa" => Ok(tg_bots::callback::CallbackAnswerInput {
                alert: false,
                has_url: false,
                native_ui: false,
                message: Some("2FA-protected payout action approved.".to_owned()),
                url: None,
                cache_time: 0,
            }),
            _ => Ok(tg_bots::callback::CallbackAnswerInput {
                alert: false,
                has_url: false,
                native_ui: false,
                message: Some("Callback acknowledged by @catalog_demo_bot".to_owned()),
                url: None,
                cache_time: 5,
            }),
        }
    }

    fn request_url_auth(
        &mut self,
        source: &tg_bots::callback::UrlAuthSource,
    ) -> Result<tg_bots::callback::UrlAuthResultInput, String> {
        let domain = source
            .url()
            .strip_prefix("https://")
            .and_then(|s| s.split('/').next())
            .unwrap_or("portal.example.org")
            .to_owned();
        let (bot_id, bot_name) = self
            .state
            .lock()
            .ok()
            .and_then(|st| {
                st.peer(session_peers::BOT_CATALOG)
                    .map(|p| (tg_bots::stand_in::UserId(p.id.bare()), p.title.clone()))
            })
            .unwrap_or((
                tg_bots::stand_in::UserId(session_peers::BOT_CATALOG.bare()),
                "Catalog Bot".to_owned(),
            ));
        Ok(tg_bots::callback::UrlAuthResultInput::Request(
            tg_bots::callback::UrlAuthRequestData {
                domain,
                bot_id,
                bot_name,
                request_write_access: true,
                request_phone_number: false,
                match_codes_first: false,
                is_app: false,
                verified_app_name: None,
                user_id_hint: None,
                match_codes: vec!["A7K".to_owned(), "M4Q".to_owned(), "Z9R".to_owned()],
                browser: Some("Rustgram Desktop".to_owned()),
                platform: Some("Windows 11 x64".to_owned()),
                ip: Some("198.51.100.42".to_owned()),
                region: Some("Portland, US".to_owned()),
            },
        ))
    }

    fn accept_url_auth(
        &mut self,
        params: &tg_bots::callback::AcceptUrlAuthParams,
    ) -> Result<tg_bots::callback::UrlAuthResultInput, String> {
        let write_flag = if params.write_allowed { "1" } else { "0" };
        let url = format!(
            "{}?tgAuthResult=demo_token_ok&write_allowed={write_flag}",
            params.source.url()
        );
        Ok(tg_bots::callback::UrlAuthResultInput::Accepted { url: Some(url) })
    }

    fn decline_url_auth(&mut self, _url: &str) -> Result<(), String> {
        Ok(())
    }

    fn query_inline_results(
        &mut self,
        request: &tg_bots::inline::GetInlineBotResultsRequest,
    ) -> Result<tg_bots::inline::InlineResultsPage, String> {
        let q = request.query.trim().to_ascii_lowercase();
        let gallery = q.contains("photo")
            || q.contains("gif")
            || q.contains("sticker")
            || q.contains("gallery");
        if gallery {
            return Ok(build_gallery_inline_page(&request.query, &request.offset));
        }
        Ok(build_list_inline_page(&request.query, &request.offset))
    }

    fn send_inline_result(
        &mut self,
        bot_id: tg_bots::stand_in::UserId,
        peer_id: tg_bots::stand_in::PeerId,
        result: &tg_bots::inline::InlineResult,
    ) -> Result<BotChatMessage, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let app_peer = PeerId(peer_id.0);
        let bot_username = st
            .peers
            .iter()
            .find(|p| p.id.bare() == bot_id.0)
            .and_then(|p| p.primary_username().map(str::to_owned))
            .unwrap_or_else(|| "catalog_demo_bot".to_owned());

        let kind_label = format!("{:?}", result.kind);
        let title = if result.title.is_empty() {
            format!("{kind_label} #{}", result.id)
        } else {
            result.title.clone()
        };
        let subtitle = if result.description.is_empty() {
            result.url.clone()
        } else {
            result.description.clone()
        };
        let body = if subtitle.is_empty() {
            title.clone()
        } else {
            format!("{title}\n{subtitle}")
        };

        let msg_id = MsgId(st.next_msg_id);
        st.next_msg_id += 1;
        let from_id = st.user.id;
        let sender_name = st.user.name.clone();
        let hist_msg = HistoryMessage {
            id: msg_id,
            peer_id: app_peer,
            from_id,
            sender_name: sender_name.clone(),
            date_unix: BASE_UNIX_TODAY + 16_900,
            outgoing: true,
            is_post: false,
            text: TextWithEntities::simple(body.clone()),
            media: None,
            service: None,
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: true,
            is_sending: false,
            is_pinned: false,
            extra: MessageExtra {
                via_bot: Some(bot_username.clone()),
                reply_markup: result.reply_markup.clone(),
                ..MessageExtra::default()
            },
        };
        let time_label = hist_msg.formatted_time();
        st.histories.entry(app_peer).or_default().push(hist_msg);
        if let Some(p) = st.peer_mut(app_peer) {
            p.last_message_time = SESSION_NOW;
        }
        Ok(BotChatMessage {
            id: tg_bots::stand_in::FullMsgId::new(peer_id, tg_bots::stand_in::MsgId(msg_id.0)),
            author: sender_name,
            outgoing: true,
            time_label,
            text: body,
            attachment: BotMessageAttachment::InlineSent {
                kind_label,
                title,
                subtitle,
            },
            inline_markup: result.reply_markup.clone(),
            reply_markup: None,
            via_bot: Some(format!("@{bot_username}")),
        })
    }

    fn send_bot_command(
        &mut self,
        peer_id: tg_bots::stand_in::PeerId,
        text: &str,
        reply_to: Option<tg_bots::stand_in::FullMsgId>,
    ) -> Result<BotChatMessage, String> {
        let sent = HistoryBackend::send_message(
            self,
            PeerId(peer_id.0),
            SendMessageRequest {
                text: TextWithEntities::simple(text),
                reply_to: reply_to.map(|f| MsgId(f.msg.0)),
                silent: false,
            },
        )?;
        Ok(BotChatMessage {
            id: tg_bots::stand_in::FullMsgId::new(peer_id, tg_bots::stand_in::MsgId(sent.id.0)),
            author: sent.sender_name.clone(),
            outgoing: sent.outgoing,
            time_label: sent.formatted_time(),
            text: sent.text.text,
            attachment: BotMessageAttachment::None,
            inline_markup: None,
            reply_markup: None,
            via_bot: None,
        })
    }

    fn share_phone(
        &mut self,
        peer_id: tg_bots::stand_in::PeerId,
        reply_to: Option<tg_bots::stand_in::MsgId>,
    ) -> Result<BotChatMessage, String> {
        let (name, phone) = self
            .state
            .lock()
            .map(|st| (st.user.name.clone(), st.user.phone.clone()))
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let text = format!("Shared contact: {name} ({phone})");
        let sent = HistoryBackend::send_message(
            self,
            PeerId(peer_id.0),
            SendMessageRequest {
                text: TextWithEntities::simple(text.clone()),
                reply_to: reply_to.map(|m| MsgId(m.0)),
                silent: false,
            },
        )?;
        Ok(BotChatMessage {
            id: tg_bots::stand_in::FullMsgId::new(peer_id, tg_bots::stand_in::MsgId(sent.id.0)),
            author: sent.sender_name.clone(),
            outgoing: true,
            time_label: sent.formatted_time(),
            text,
            attachment: BotMessageAttachment::None,
            inline_markup: None,
            reply_markup: None,
            via_bot: None,
        })
    }

    fn send_poll(
        &mut self,
        peer_id: tg_bots::stand_in::PeerId,
        poll: &SubmittedPoll,
    ) -> Result<BotChatMessage, String> {
        if poll.question.trim().is_empty() || poll.options.len() < 2 {
            return Err("Poll requires a question and at least two options.".to_owned());
        }
        let poll_att = PollAttachment {
            question: poll.question.trim().to_owned(),
            is_quiz: poll.is_quiz,
            is_anonymous: true,
            multiple_choice: false,
            closed: false,
            options: poll
                .options
                .iter()
                .enumerate()
                .map(|(i, text)| PollOption {
                    text: text.clone(),
                    votes: 0,
                    chosen: false,
                    correct: poll.correct_option == Some(i),
                })
                .collect(),
            total_voters: 0,
            explanation: None,
        };
        let sent = HistoryBackend::send_message_extended(
            self,
            PeerId(peer_id.0),
            SendMessageExtended {
                media: Some(HistoryMedia::Poll(poll_att)),
                ..SendMessageExtended::default()
            },
        )?;
        Ok(BotChatMessage {
            id: tg_bots::stand_in::FullMsgId::new(peer_id, tg_bots::stand_in::MsgId(sent.id.0)),
            author: sent.sender_name.clone(),
            outgoing: true,
            time_label: sent.formatted_time(),
            text: format!("Poll: {}", poll.question.trim()),
            attachment: BotMessageAttachment::None,
            inline_markup: None,
            reply_markup: None,
            via_bot: None,
        })
    }

    fn send_requested_peers(
        &mut self,
        item_id: tg_bots::stand_in::FullMsgId,
        button_id: i64,
        selected_peers: &[tg_bots::stand_in::PeerId],
    ) -> Result<BotChatMessage, String> {
        if selected_peers.is_empty() {
            return Err("Select at least one peer.".to_owned());
        }
        let names: Vec<String> = {
            let st = self
                .state
                .lock()
                .map_err(|_| "Session state lock poisoned".to_owned())?;
            selected_peers
                .iter()
                .map(|pid| {
                    st.peer(PeerId(pid.0))
                        .map_or_else(|| format!("Peer {}", pid.0), |p| p.title.clone())
                })
                .collect()
        };
        let text = format!(
            "Shared peer(s) for button #{button_id}: {}",
            names.join(", ")
        );
        self.send_bot_command(item_id.peer, &text, Some(item_id))
    }

    fn toggle_attach_menu_bot(
        &mut self,
        bot_id: tg_bots::stand_in::UserId,
        state: tg_bots::mini_app::AttachMenuToggleState,
    ) -> Result<tg_bots::mini_app::AttachMenuStore, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let Some(existing) = st
            .bot_attach_menu
            .bots
            .iter_mut()
            .find(|b| b.bot_id == bot_id)
        else {
            return Err("Bot not found in attach menu.".to_owned());
        };
        match state {
            tg_bots::mini_app::AttachMenuToggleState::Removed => existing.inactive = true,
            tg_bots::mini_app::AttachMenuToggleState::Added
            | tg_bots::mini_app::AttachMenuToggleState::AllowedToWrite => {
                existing.inactive = false;
            }
        }
        Ok(st.bot_attach_menu.clone())
    }

    fn accept_attach_disclaimer(&mut self, bot_id: tg_bots::stand_in::UserId) {
        if let Ok(mut st) = self.state.lock() {
            st.bot_attach_menu.mark_disclaimer_accepted(bot_id);
        }
    }

    fn share_game_score(
        &mut self,
        _message_id: tg_bots::stand_in::FullMsgId,
        target_peer: tg_bots::stand_in::PeerId,
    ) -> Result<String, String> {
        let title = self
            .state
            .lock()
            .ok()
            .and_then(|st| st.peer(PeerId(target_peer.0)).map(|p| p.title.clone()))
            .unwrap_or_else(|| format!("Peer {}", target_peer.0));
        Ok(format!("Shared game score to {title}"))
    }

    fn set_menu_button(&mut self, button: tg_bots::commands::BotMenuButton) {
        if let Ok(mut st) = self.state.lock()
            && let Some(info) = st.bot_info_map.get_mut(&session_peers::BOT_CATALOG)
        {
            info.apply_menu_button(button);
        }
    }

    fn resolve_user_name(&self, user_id: tg_bots::stand_in::UserId) -> String {
        if let Ok(st) = self.state.lock()
            && let Some(p) = st.peers.iter().find(|p| p.id.bare() == user_id.0)
        {
            return p.title.clone();
        }
        format!("User #{}", user_id.0)
    }

    fn commands_for_chat(&self) -> Vec<tg_bots::commands::BotCommand> {
        self.commands_for_peer(tg_bots::stand_in::PeerId(session_peers::BOT_CATALOG.0))
    }

    fn start_bot(
        &mut self,
        params: &tg_bots::protocol::StartBotParams,
    ) -> Result<tg_app_core::bots::backend::BotChatMessage, String> {
        let (target_peer, msg_id, author_name) = {
            let mut st = self
                .state
                .lock()
                .map_err(|_| "Session state lock poisoned".to_owned())?;
            st.started_bots.push(params.clone());
            let target_peer = params
                .peer_id
                .map(|p| PeerId(p.0))
                .unwrap_or_else(|| PeerId::from_user(params.bot_id.0));
            st.next_msg_id += 1;
            (target_peer, MsgId(st.next_msg_id), st.user.name.clone())
        };
        let reply_text = if params.start_param.is_empty() {
            "/start".to_owned()
        } else {
            format!("Started bot with payload '{}'", params.start_param)
        };
        Ok(tg_app_core::bots::backend::BotChatMessage {
            id: tg_bots::stand_in::FullMsgId::new(
                tg_bots::stand_in::PeerId(target_peer.0),
                tg_bots::stand_in::MsgId(msg_id.0),
            ),
            author: author_name,
            outgoing: true,
            time_label: "14:22".to_owned(),
            text: reply_text,
            attachment: tg_app_core::bots::backend::BotMessageAttachment::None,
            inline_markup: None,
            reply_markup: None,
            via_bot: None,
        })
    }
}

impl BotsBackend for MockSession {
    fn load_bot_chat(&self) -> BotChatSnapshot {
        self.handle.load_bot_chat()
    }
    fn load_bot_chat_for_peer(
        &self,
        peer_id: tg_bots::stand_in::PeerId,
    ) -> Option<BotChatSnapshot> {
        self.handle.load_bot_chat_for_peer(peer_id)
    }
    fn resolve_inline_bot(&self, username: &str) -> Option<(tg_bots::stand_in::UserId, String)> {
        self.handle.resolve_inline_bot(username)
    }
    fn commands_for_peer(
        &self,
        peer_id: tg_bots::stand_in::PeerId,
    ) -> Vec<tg_bots::commands::BotCommand> {
        self.handle.commands_for_peer(peer_id)
    }
    fn load_attach_menu(&self) -> tg_bots::mini_app::AttachMenuStore {
        self.handle.load_attach_menu()
    }
    fn candidate_peers(&self) -> Vec<BotPeerCandidate> {
        self.handle.candidate_peers()
    }
    fn send_callback_query(
        &mut self,
        request: &tg_bots::callback::CallbackQueryRequest,
    ) -> Result<tg_bots::callback::CallbackAnswerInput, String> {
        self.handle.send_callback_query(request)
    }
    fn request_url_auth(
        &mut self,
        source: &tg_bots::callback::UrlAuthSource,
    ) -> Result<tg_bots::callback::UrlAuthResultInput, String> {
        self.handle.request_url_auth(source)
    }
    fn accept_url_auth(
        &mut self,
        params: &tg_bots::callback::AcceptUrlAuthParams,
    ) -> Result<tg_bots::callback::UrlAuthResultInput, String> {
        self.handle.accept_url_auth(params)
    }
    fn decline_url_auth(&mut self, url: &str) -> Result<(), String> {
        self.handle.decline_url_auth(url)
    }
    fn query_inline_results(
        &mut self,
        request: &tg_bots::inline::GetInlineBotResultsRequest,
    ) -> Result<tg_bots::inline::InlineResultsPage, String> {
        self.handle.query_inline_results(request)
    }
    fn send_inline_result(
        &mut self,
        bot_id: tg_bots::stand_in::UserId,
        peer_id: tg_bots::stand_in::PeerId,
        result: &tg_bots::inline::InlineResult,
    ) -> Result<BotChatMessage, String> {
        self.handle.send_inline_result(bot_id, peer_id, result)
    }
    fn send_bot_command(
        &mut self,
        peer_id: tg_bots::stand_in::PeerId,
        text: &str,
        reply_to: Option<tg_bots::stand_in::FullMsgId>,
    ) -> Result<BotChatMessage, String> {
        self.handle.send_bot_command(peer_id, text, reply_to)
    }
    fn share_phone(
        &mut self,
        peer_id: tg_bots::stand_in::PeerId,
        reply_to: Option<tg_bots::stand_in::MsgId>,
    ) -> Result<BotChatMessage, String> {
        self.handle.share_phone(peer_id, reply_to)
    }
    fn send_poll(
        &mut self,
        peer_id: tg_bots::stand_in::PeerId,
        poll: &SubmittedPoll,
    ) -> Result<BotChatMessage, String> {
        BotsBackend::send_poll(&mut self.handle, peer_id, poll)
    }
    fn send_requested_peers(
        &mut self,
        item_id: tg_bots::stand_in::FullMsgId,
        button_id: i64,
        selected_peers: &[tg_bots::stand_in::PeerId],
    ) -> Result<BotChatMessage, String> {
        self.handle
            .send_requested_peers(item_id, button_id, selected_peers)
    }
    fn toggle_attach_menu_bot(
        &mut self,
        bot_id: tg_bots::stand_in::UserId,
        state: tg_bots::mini_app::AttachMenuToggleState,
    ) -> Result<tg_bots::mini_app::AttachMenuStore, String> {
        self.handle.toggle_attach_menu_bot(bot_id, state)
    }
    fn accept_attach_disclaimer(&mut self, bot_id: tg_bots::stand_in::UserId) {
        self.handle.accept_attach_disclaimer(bot_id);
    }
    fn share_game_score(
        &mut self,
        message_id: tg_bots::stand_in::FullMsgId,
        target_peer: tg_bots::stand_in::PeerId,
    ) -> Result<String, String> {
        self.handle.share_game_score(message_id, target_peer)
    }
    fn set_menu_button(&mut self, button: tg_bots::commands::BotMenuButton) {
        self.handle.set_menu_button(button);
    }
    fn resolve_user_name(&self, user_id: tg_bots::stand_in::UserId) -> String {
        self.handle.resolve_user_name(user_id)
    }
    fn commands_for_chat(&self) -> Vec<tg_bots::commands::BotCommand> {
        self.handle.commands_for_chat()
    }
    fn start_bot(
        &mut self,
        params: &tg_bots::protocol::StartBotParams,
    ) -> Result<tg_app_core::bots::backend::BotChatMessage, String> {
        self.handle.start_bot(params)
    }
}

// =============================================================================
// 4. info::backend::InfoBackend
// =============================================================================

impl InfoBackend for SharedSessionHandle {
    fn peer_profile(&self, peer_id: PeerId) -> Option<PeerProfile> {
        let st = self.state.lock().ok()?;
        st.peer(peer_id).map(|p| st.build_peer_profile(p))
    }

    fn all_peers(&self) -> Vec<PeerProfile> {
        let Ok(st) = self.state.lock() else {
            return Vec::new();
        };
        st.peers.iter().map(|p| st.build_peer_profile(p)).collect()
    }

    fn contacts(&self) -> Vec<PeerProfile> {
        let Ok(st) = self.state.lock() else {
            return Vec::new();
        };
        st.peers
            .iter()
            .filter(|p| p.kind == InfoPeerKind::User && p.is_contact)
            .map(|p| st.build_peer_profile(p))
            .collect()
    }

    fn peer_members(&self, peer_id: PeerId, query: &str) -> Vec<MemberEntry> {
        let Ok(st) = self.state.lock() else {
            return Vec::new();
        };
        let q = query.trim().to_lowercase();
        st.members
            .get(&peer_id)
            .map(|list| {
                list.iter()
                    .filter(|m| {
                        q.is_empty()
                            || m.name.to_lowercase().contains(&q)
                            || m.username
                                .as_deref()
                                .is_some_and(|u| u.to_lowercase().contains(&q))
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn common_groups(&self, user_id: PeerId) -> Vec<CommonGroupEntry> {
        let Ok(st) = self.state.lock() else {
            return Vec::new();
        };
        st.members
            .iter()
            .filter(|(_, list)| list.iter().any(|m| m.peer_id == user_id))
            .filter_map(|(&gid, _)| {
                let g = st.peer(gid)?;
                Some(CommonGroupEntry {
                    peer_id: g.id,
                    title: g.title.clone(),
                    members_count: g.members_count,
                    color_index: g.color_index,
                })
            })
            .collect()
    }

    fn shared_media(
        &self,
        peer_id: PeerId,
        tab: SharedMediaTab,
        query: &str,
    ) -> Vec<InfoMediaItem> {
        self.state
            .lock()
            .map(|st| st.extract_shared_media(peer_id, tab, query))
            .unwrap_or_default()
    }

    /// Demo `messages.searchGlobal`: the shared media of every chat, newest chats first.
    fn global_media(
        &self,
        tab: SharedMediaTab,
        query: &str,
        limit: usize,
    ) -> tg_app_core::core_types::dto::global_media::GlobalMediaPage {
        use tg_app_core::core_types::dto::global_media::{GlobalMediaEntry, GlobalMediaPage};
        let mut entries = Vec::new();
        for peer in self.all_peers() {
            for item in self.shared_media(peer.id, tab, query) {
                entries.push(GlobalMediaEntry {
                    peer_id: peer.id,
                    peer_title: peer.title.clone(),
                    item,
                });
            }
        }
        let total_count = entries.len();
        let limit = limit.max(1);
        entries.truncate(limit);
        GlobalMediaPage {
            entries,
            total_count,
            next_cursor: None,
            has_more: total_count > limit,
        }
    }

    fn toggle_notifications(&mut self, peer_id: PeerId) -> bool {
        self.toggle_mute_dialog(peer_id)
    }

    fn toggle_block_user(&mut self, peer_id: PeerId) -> bool {
        let Ok(mut st) = self.state.lock() else {
            return false;
        };
        if let Some(p) = st.peer_mut(peer_id) {
            p.is_blocked = !p.is_blocked;
            return p.is_blocked;
        }
        false
    }

    fn add_contact(
        &mut self,
        first_name: &str,
        last_name: &str,
        phone: &str,
    ) -> Result<PeerId, String> {
        let first = first_name.trim();
        if first.is_empty() {
            return Err("First name is required.".to_owned());
        }
        if !is_valid_phone(phone) {
            return Err("Enter a valid synthetic phone number (e.g. +999 661 0199).".to_owned());
        }
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let id = PeerId::from_user(st.next_user_bare);
        st.next_user_bare += 1;
        let full = if last_name.trim().is_empty() {
            first.to_owned()
        } else {
            format!("{first} {}", last_name.trim())
        };
        let initials = initials_for(&full);
        let color_index = (id.bare() % 7) as u8;
        st.peers.push(PeerRecord {
            id,
            kind: InfoPeerKind::User,
            title: full.clone(),
            status: "last seen recently".to_owned(),
            is_online: false,
            verified: false,
            premium: false,
            color_index,
            avatar_rgb: avatar_rgb_for_index(color_index),
            initials,
            phone: Some(format_phone(phone)),
            usernames: Vec::new(),
            bio: String::new(),
            birthday: None,
            invite_link: None,
            muted: false,
            pinned: false,
            archived: false,
            unread_count: 0,
            unread_mark: false,
            unread_mentions: 0,
            is_contact: true,
            is_blocked: false,
            can_send: true,
            can_edit: true,
            can_add_members: false,
            can_manage_admins: false,
            can_manage_permissions: false,
            is_public: false,
            no_forwards: false,
            is_forum: false,
            members_count: 0,
            online_count: 0,
            admins_count: 0,
            default_banned_rights: ChatBannedRights::empty(),
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: 0,
            linked_discussion_peer: None,
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            folders: vec![session_folders::PERSONAL],
            contact_status: None,
            typing: None,
            last_message_time: SESSION_NOW,
        });
        st.histories.insert(id, Vec::new());
        Ok(id)
    }

    fn create_group(&mut self, req: CreateGroupRequest) -> Result<PeerId, String> {
        let title = req.title.trim();
        if title.is_empty() {
            return Err("Group name cannot be empty.".to_owned());
        }
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let id = PeerId::from_chat(st.next_chat_bare);
        st.next_chat_bare += 1;
        let members_count = (req.member_ids.len() + 1) as u32;
        let color_index = (id.bare() % 7) as u8;
        let mut members_list = vec![MemberEntry {
            peer_id: st.user.id,
            name: st.user.name.clone(),
            username: Some(st.user.username.clone()),
            status_text: "online".to_owned(),
            is_online: true,
            is_bot: false,
            role: MemberRole::Owner { custom_title: None },
            color_index: 5,
        }];
        for &mid in &req.member_ids {
            if let Some(mp) = st.peer(mid) {
                members_list.push(MemberEntry {
                    peer_id: mp.id,
                    name: mp.title.clone(),
                    username: mp.primary_username().map(str::to_owned),
                    status_text: mp.status.clone(),
                    is_online: mp.is_online,
                    is_bot: mp.kind == InfoPeerKind::Bot,
                    role: MemberRole::Member,
                    color_index: mp.color_index,
                });
            }
        }
        st.peers.push(PeerRecord {
            id,
            kind: InfoPeerKind::Supergroup,
            title: title.to_owned(),
            status: format_members_status(members_count, 1),
            is_online: false,
            verified: false,
            premium: false,
            color_index,
            avatar_rgb: avatar_rgb_for_index(color_index),
            initials: initials_for(title),
            phone: None,
            usernames: Vec::new(),
            bio: String::new(),
            birthday: None,
            invite_link: Some(format!("https://t.me/+grp{}", id.bare())),
            muted: false,
            pinned: false,
            archived: false,
            unread_count: 0,
            unread_mark: false,
            unread_mentions: 0,
            is_contact: false,
            is_blocked: false,
            can_send: true,
            can_edit: true,
            can_add_members: true,
            can_manage_admins: true,
            can_manage_permissions: true,
            is_public: false,
            no_forwards: false,
            is_forum: false,
            members_count,
            online_count: 1,
            admins_count: 1,
            default_banned_rights: ChatBannedRights::empty(),
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: req.auto_delete_ttl_seconds,
            linked_discussion_peer: None,
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            folders: vec![session_folders::WORK],
            contact_status: None,
            typing: None,
            last_message_time: SESSION_NOW,
        });
        st.members.insert(id, members_list);
        let msg_id = MsgId(st.next_msg_id);
        st.next_msg_id += 1;
        let actor = st.user.name.clone();
        let self_id = st.user.id;
        st.histories.insert(
            id,
            vec![HistoryMessage {
                extra: Default::default(),
                id: msg_id,
                peer_id: id,
                from_id: self_id,
                sender_name: actor,
                date_unix: BASE_UNIX_TODAY + 16_400,
                outgoing: true,
                is_post: false,
                text: TextWithEntities::default(),
                media: None,
                service: Some(ServiceAction::ChatCreated {
                    title: title.to_owned(),
                }),
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
            }],
        );
        Ok(id)
    }

    fn create_channel(&mut self, req: CreateChannelRequest) -> Result<PeerId, String> {
        let title = req.title.trim();
        if title.is_empty() {
            return Err("Channel name cannot be empty.".to_owned());
        }
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let id = PeerId::from_channel(st.next_channel_bare);
        st.next_channel_bare += 1;
        let color_index = (id.bare() % 7) as u8;
        let usernames = req
            .username
            .as_deref()
            .map(normalize_username)
            .filter(|u| !u.is_empty())
            .map(|u| {
                vec![PeerUsername {
                    username: u.to_owned(),
                    is_editable: true,
                    is_active: true,
                }]
            })
            .unwrap_or_default();
        st.peers.push(PeerRecord {
            id,
            kind: InfoPeerKind::Channel,
            title: title.to_owned(),
            status: format_subscribers_status(1),
            is_online: false,
            verified: false,
            premium: false,
            color_index,
            avatar_rgb: avatar_rgb_for_index(color_index),
            initials: initials_for(title),
            phone: None,
            usernames,
            bio: req.description,
            birthday: None,
            invite_link: Some(format!("https://t.me/+chn{}", id.bare())),
            muted: false,
            pinned: false,
            archived: false,
            unread_count: 0,
            unread_mark: false,
            unread_mentions: 0,
            is_contact: false,
            is_blocked: false,
            can_send: true,
            can_edit: true,
            can_add_members: true,
            can_manage_admins: true,
            can_manage_permissions: false,
            is_public: req.is_public,
            no_forwards: req.no_forwards,
            is_forum: false,
            members_count: 1,
            online_count: 0,
            admins_count: 1,
            default_banned_rights: ChatBannedRights::empty(),
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: 0,
            linked_discussion_peer: None,
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            folders: vec![session_folders::CHANNELS],
            contact_status: None,
            typing: None,
            last_message_time: SESSION_NOW,
        });
        st.histories.insert(id, Vec::new());
        Ok(id)
    }

    fn update_peer_info(
        &mut self,
        peer_id: PeerId,
        req: UpdatePeerInfoRequest,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let Some(p) = st.peer_mut(peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        if !req.title.trim().is_empty() {
            p.title = req.title.trim().to_owned();
            p.initials = initials_for(&p.title);
        }
        p.bio = req.description;
        p.is_public = req.is_public;
        p.no_forwards = req.no_forwards;
        if let Some(u) = req.username {
            let norm = normalize_username(&u);
            if norm.is_empty() {
                p.usernames.clear();
            } else {
                p.usernames = vec![PeerUsername {
                    username: norm.to_owned(),
                    is_editable: true,
                    is_active: true,
                }];
            }
        }
        Ok(())
    }

    fn update_default_permissions(
        &mut self,
        peer_id: PeerId,
        rights: ChatBannedRights,
        slowmode_seconds: u32,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let Some(p) = st.peer_mut(peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        p.default_banned_rights = rights;
        p.slowmode_seconds = slowmode_seconds;
        Ok(())
    }

    fn update_admin_rights(
        &mut self,
        peer_id: PeerId,
        user_id: PeerId,
        rights: ChatAdminRights,
        custom_title: String,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        if let Some(list) = st.members.get_mut(&peer_id)
            && let Some(m) = list.iter_mut().find(|m| m.peer_id == user_id)
        {
            if rights.is_empty() {
                m.role = MemberRole::Member;
            } else {
                m.role = MemberRole::Admin {
                    custom_title: (!custom_title.trim().is_empty())
                        .then(|| custom_title.trim().to_owned()),
                    rights,
                };
            }
        }
        let admin_count = st.members.get(&peer_id).map_or(1, |list| {
            list.iter().filter(|m| m.role.is_admin_or_owner()).count() as u32
        });
        if let Some(p) = st.peer_mut(peer_id) {
            p.admins_count = admin_count;
        }
        Ok(())
    }

    fn add_members(&mut self, peer_id: PeerId, member_ids: &[PeerId]) -> Result<usize, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let mut added = 0usize;
        for &uid in member_ids {
            let already = st
                .members
                .get(&peer_id)
                .is_some_and(|list| list.iter().any(|m| m.peer_id == uid));
            if already {
                continue;
            }
            if let Some(up) = st.peer(uid).cloned() {
                let username = up.primary_username().map(str::to_owned);
                st.members.entry(peer_id).or_default().push(MemberEntry {
                    peer_id: up.id,
                    name: up.title,
                    username,
                    status_text: up.status,
                    is_online: up.is_online,
                    is_bot: up.kind == InfoPeerKind::Bot,
                    role: MemberRole::Member,
                    color_index: up.color_index,
                });
                added += 1;
            }
        }
        if added > 0
            && let Some(p) = st.peer_mut(peer_id)
        {
            p.members_count = p.members_count.saturating_add(added as u32);
            p.status = format_members_status(p.members_count, p.online_count);
        }
        Ok(added)
    }

    fn remove_member(&mut self, peer_id: PeerId, user_id: PeerId) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        if let Some(list) = st.members.get_mut(&peer_id) {
            list.retain(|m| m.peer_id != user_id);
        }
        if let Some(p) = st.peer_mut(peer_id) {
            p.members_count = p.members_count.saturating_sub(1).max(1);
            p.status = format_members_status(p.members_count, p.online_count);
        }
        Ok(())
    }

    fn set_auto_delete_ttl(&mut self, peer_id: PeerId, ttl_seconds: u32) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let Some(p) = st.peer_mut(peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        p.auto_delete_ttl_seconds = ttl_seconds;
        Ok(())
    }

    fn set_linked_discussion(
        &mut self,
        channel_id: PeerId,
        group_id: Option<PeerId>,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let Some(p) = st.peer_mut(channel_id) else {
            return Err("Channel not found.".to_owned());
        };
        p.linked_discussion_peer = group_id;
        Ok(())
    }

    fn check_username(&self, username: &str, exclude_peer: Option<PeerId>) -> UsernameAvailability {
        let norm = normalize_username(username);
        if norm.is_empty() {
            return UsernameAvailability::Empty;
        }
        let syntax = validate_info_username(norm);
        if !matches!(syntax, UsernameAvailability::Available) {
            return syntax;
        }
        let Ok(st) = self.state.lock() else {
            return UsernameAvailability::Available;
        };
        for p in &st.peers {
            if Some(p.id) == exclude_peer {
                continue;
            }
            if p.usernames
                .iter()
                .any(|u| u.username.eq_ignore_ascii_case(norm))
            {
                return UsernameAvailability::Occupied;
            }
        }
        UsernameAvailability::Available
    }

    fn update_username(&mut self, peer_id: PeerId, username: &str) -> Result<(), String> {
        let norm = normalize_username(username);
        if !norm.is_empty() {
            match InfoBackend::check_username(self, norm, Some(peer_id)) {
                UsernameAvailability::Available | UsernameAvailability::Empty => {}
                UsernameAvailability::Occupied => {
                    return Err("Username is already taken.".to_owned());
                }
                other => return Err(format!("Invalid username: {other:?}")),
            }
        }
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let Some(p) = st.peer_mut(peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        if norm.is_empty() {
            p.usernames.clear();
        } else {
            p.usernames = vec![PeerUsername {
                username: norm.to_owned(),
                is_editable: true,
                is_active: true,
            }];
        }
        if peer_id == session_peers::SELF {
            st.user.username = norm.to_owned();
            let _ = st.settings_store.update_username(norm);
        }
        Ok(())
    }

    fn forward_messages(
        &mut self,
        target_peers: &[PeerId],
        message_ids: &[i32],
        comment: &str,
    ) -> Result<usize, String> {
        let mut delivered = 0usize;
        for &target in target_peers {
            let text = if !comment.trim().is_empty() {
                comment.trim().to_owned()
            } else if message_ids.is_empty() {
                "Shared link".to_owned()
            } else {
                format!("Forwarded {} message(s)", message_ids.len())
            };
            if HistoryBackend::send_message(
                self,
                target,
                SendMessageRequest {
                    text: TextWithEntities::simple(text),
                    reply_to: None,
                    silent: false,
                },
            )
            .is_ok()
            {
                delivered += 1;
            }
        }
        Ok(delivered)
    }

    fn delete_messages(
        &mut self,
        peer_id: PeerId,
        message_ids: &[i32],
        revoke_for_everyone: bool,
    ) -> Result<usize, String> {
        if message_ids.is_empty() {
            self.delete_dialog(peer_id);
            return Ok(1);
        }
        let ids: Vec<MsgId> = message_ids.iter().copied().map(MsgId).collect();
        HistoryBackend::delete_messages(self, peer_id, &ids, revoke_for_everyone)?;
        Ok(ids.len())
    }

    fn submit_report(
        &mut self,
        _peer_id: PeerId,
        _reason: ReportReason,
        _comment: &str,
    ) -> Result<(), String> {
        Ok(())
    }

    fn languages(&self) -> Vec<LanguageEntry> {
        self.state
            .lock()
            .map(|st| st.languages.clone())
            .unwrap_or_default()
    }

    fn active_language(&self) -> String {
        self.state
            .lock()
            .map(|st| st.active_language.clone())
            .unwrap_or_else(|_| "en".to_owned())
    }

    fn set_language(&mut self, lang_code: &str) -> Result<(), String> {
        if let Ok(mut st) = self.state.lock() {
            st.active_language = lang_code.to_owned();
        }
        Ok(())
    }

    fn terms_of_service(&self) -> TermsOfServiceData {
        self.state
            .lock()
            .map(|st| st.tos.clone())
            .unwrap_or_else(|_| TermsOfServiceData {
                id: String::new(),
                title: "Terms of Service".to_owned(),
                body: String::new(),
                min_age_confirm: None,
                accepted: false,
            })
    }

    fn accept_terms(&mut self, _tos_id: &str) -> Result<(), String> {
        if let Ok(mut st) = self.state.lock() {
            st.tos.accepted = true;
        }
        Ok(())
    }

    fn exported_chat_invites(
        &self,
        peer_id: PeerId,
        revoked: bool,
    ) -> Result<InviteLinksList, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.peer_mgmt.exported_chat_invites(peer_id, revoked))
    }

    fn export_chat_invite(
        &mut self,
        peer_id: PeerId,
        req: InviteLinkRequest,
    ) -> Result<InviteLinkInfo, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let self_id = st.user.id;
        let self_name = st.user.name.clone();
        let entry = st
            .peer_mgmt
            .export_chat_invite(peer_id, self_id, &self_name, req);
        if entry.permanent
            && let Some(p) = st.peer_mut(peer_id)
        {
            p.invite_link = Some(entry.link.clone());
        }
        Ok(entry)
    }

    fn edit_exported_chat_invite(
        &mut self,
        peer_id: PeerId,
        link: &str,
        req: InviteLinkRequest,
    ) -> Result<InviteLinkInfo, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.peer_mgmt.edit_exported_chat_invite(peer_id, link, req)
    }

    fn revoke_exported_chat_invite(
        &mut self,
        peer_id: PeerId,
        link: &str,
    ) -> Result<(InviteLinkInfo, Option<InviteLinkInfo>), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let self_id = st.user.id;
        let self_name = st.user.name.clone();
        let (revoked, replacement) = st
            .peer_mgmt
            .revoke_exported_chat_invite(peer_id, link, self_id, &self_name)?;
        if let Some(ref fresh) = replacement
            && let Some(p) = st.peer_mut(peer_id)
        {
            p.invite_link = Some(fresh.link.clone());
        }
        Ok((revoked, replacement))
    }

    fn delete_exported_chat_invite(&mut self, peer_id: PeerId, link: &str) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.peer_mgmt.delete_exported_chat_invite(peer_id, link)
    }

    fn delete_revoked_exported_chat_invites(&mut self, peer_id: PeerId) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.peer_mgmt.delete_revoked_exported_chat_invites(peer_id);
        Ok(())
    }

    fn chat_invite_importers(
        &self,
        peer_id: PeerId,
        link: Option<&str>,
        requested: bool,
        query: &str,
    ) -> Result<InviteImportersList, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st
            .peer_mgmt
            .chat_invite_importers(peer_id, link, requested, query))
    }

    fn hide_chat_join_request(
        &mut self,
        peer_id: PeerId,
        user_id: PeerId,
        approved: bool,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let entry = st
            .peer_mgmt
            .hide_chat_join_request(peer_id, user_id, approved)?;
        if approved {
            let list = st.members.entry(peer_id).or_default();
            if !list.iter().any(|m| m.peer_id == entry.user_id) {
                list.push(MemberEntry {
                    peer_id: entry.user_id,
                    name: entry.name.clone(),
                    username: entry.username.clone(),
                    status_text: "recently".to_owned(),
                    is_online: false,
                    is_bot: false,
                    role: MemberRole::Member,
                    color_index: entry.color_index,
                });
                if let Some(p) = st.peer_mut(peer_id) {
                    p.members_count += 1;
                }
            }
        }
        Ok(())
    }

    fn hide_all_chat_join_requests(
        &mut self,
        peer_id: PeerId,
        link: Option<&str>,
        approved: bool,
    ) -> Result<usize, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let resolved = st
            .peer_mgmt
            .hide_all_chat_join_requests(peer_id, link, approved);
        let count = resolved.len();
        if approved {
            let mut added = 0u32;
            let list = st.members.entry(peer_id).or_default();
            for entry in &resolved {
                if !list.iter().any(|m| m.peer_id == entry.user_id) {
                    list.push(MemberEntry {
                        peer_id: entry.user_id,
                        name: entry.name.clone(),
                        username: entry.username.clone(),
                        status_text: "recently".to_owned(),
                        is_online: false,
                        is_bot: false,
                        role: MemberRole::Member,
                        color_index: entry.color_index,
                    });
                    added += 1;
                }
            }
            if added > 0
                && let Some(p) = st.peer_mut(peer_id)
            {
                p.members_count += added;
            }
        }
        Ok(count)
    }

    fn join_requests_summary(&self, peer_id: PeerId) -> JoinRequestsSummary {
        self.state
            .lock()
            .map(|st| st.peer_mgmt.join_requests_summary(peer_id))
            .unwrap_or_default()
    }

    fn peer_color_settings(&self, peer_id: PeerId) -> PeerColorSettings {
        self.state
            .lock()
            .map(|st| {
                let fallback = st.peer(peer_id).map_or(0, |p| p.color_index);
                st.peer_mgmt.peer_color_settings(peer_id, fallback)
            })
            .unwrap_or_default()
    }

    fn update_peer_color(
        &mut self,
        peer_id: PeerId,
        req: UpdatePeerColorRequest,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        if !req.for_profile
            && let Some(idx) = req.color_index
            && let Some(p) = st.peer_mut(peer_id)
        {
            p.color_index = idx % 7;
            p.avatar_rgb = avatar_rgb_for_index(idx % 7);
        }
        st.peer_mgmt.update_peer_color(peer_id, req);
        Ok(())
    }

    fn check_ownership_transfer(&self, _peer_id: PeerId) -> Result<OwnershipTransferCheck, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.peer_mgmt.check_ownership_transfer())
    }

    fn transfer_ownership(
        &mut self,
        peer_id: PeerId,
        new_owner_id: PeerId,
        password: &str,
    ) -> Result<(), OwnershipTransferError> {
        let mut st = self.state.lock().map_err(|_| {
            OwnershipTransferError::ServerError("Session state lock poisoned".to_owned())
        })?;
        st.peer_mgmt
            .transfer_ownership(peer_id, new_owner_id, password)?;
        if let Some(list) = st.members.get_mut(&peer_id) {
            for m in list.iter_mut() {
                if matches!(m.role, MemberRole::Owner { .. }) {
                    m.role = MemberRole::Admin {
                        custom_title: None,
                        rights: ChatAdminRights::all(),
                    };
                }
                if m.peer_id == new_owner_id {
                    m.role = MemberRole::Owner { custom_title: None };
                }
            }
        }
        Ok(())
    }

    fn similar_channels(&self, peer_id: PeerId) -> Result<SimilarChannelsResult, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.peer_mgmt.similar_channels(peer_id))
    }

    fn downloads_list(&self, query: &str) -> Vec<DownloadEntry> {
        self.state
            .lock()
            .map(|st| st.peer_mgmt.downloads_list(query))
            .unwrap_or_default()
    }

    fn toggle_download_pause(&mut self, download_id: u64) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.peer_mgmt.toggle_download_pause(download_id)
    }

    fn cancel_or_delete_download(
        &mut self,
        download_id: u64,
        _delete_file: bool,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.peer_mgmt.cancel_or_delete_download(download_id)
    }

    fn clear_completed_downloads(&mut self) -> Result<usize, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.peer_mgmt.clear_completed_downloads())
    }

    fn delete_all_downloads(&mut self) -> Result<usize, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.peer_mgmt.delete_all_downloads())
    }

    fn peer_pinned_stories(
        &self,
        peer_id: PeerId,
        offset_id: i32,
        limit: i32,
    ) -> Result<ProfileStoriesPage, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.profile_extras.pinned_stories(peer_id, offset_id, limit))
    }

    fn peer_stories_archive(
        &self,
        peer_id: PeerId,
        offset_id: i32,
        limit: i32,
    ) -> Result<ProfileStoriesPage, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.profile_extras.stories_archive(peer_id, offset_id, limit))
    }

    fn peer_story_albums(&self, peer_id: PeerId) -> Result<Vec<ProfileStoryAlbum>, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.profile_extras.story_albums(peer_id))
    }

    fn peer_album_stories(
        &self,
        peer_id: PeerId,
        album_id: i32,
        offset_id: i32,
        limit: i32,
    ) -> Result<ProfileStoriesPage, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st
            .profile_extras
            .album_stories(peer_id, album_id, offset_id, limit))
    }

    fn peer_saved_gifts(
        &self,
        peer_id: PeerId,
        filter: &tg_payments::star_gifts::PeerGiftsFilter,
    ) -> Result<ProfilePeerGiftsPage, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.profile_extras.peer_saved_gifts(peer_id, filter))
    }

    fn toggle_peer_gift_saved(
        &mut self,
        peer_id: PeerId,
        msg_id: i32,
        saved_id: u64,
        unsave: bool,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.profile_extras
            .toggle_peer_gift_saved(peer_id, msg_id, saved_id, unsave)
    }

    fn peer_saved_music(
        &self,
        peer_id: PeerId,
        offset: i32,
        limit: i32,
    ) -> Result<Vec<SavedMusicTrack>, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.profile_extras.saved_music(peer_id, offset, limit))
    }

    fn save_profile_music(&mut self, track: &SavedMusicTrack, unsave: bool) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let self_id = st.user.id;
        st.profile_extras.save_profile_music(self_id, track, unsave)
    }

    fn set_main_profile_tab(
        &mut self,
        peer_id: PeerId,
        tab: tg_app_core::info::backend::ProfileMainTab,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.profile_extras.set_main_profile_tab(peer_id, tab)
    }

    fn main_profile_tab(
        &self,
        peer_id: PeerId,
    ) -> Option<tg_app_core::info::backend::ProfileMainTab> {
        let st = self.state.lock().ok()?;
        st.profile_extras.main_profile_tab(peer_id)
    }

    fn suggest_user_birthday(
        &mut self,
        user_id: PeerId,
        birthday: tg_app_core::info::values::Birthday,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.profile_extras.suggest_user_birthday(user_id, birthday)
    }

    fn report_profile_photo(
        &mut self,
        peer_id: PeerId,
        photo: tg_app_core::info::backend::ProfilePhotoRef,
        reason: ReportReason,
        comment: &str,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.profile_extras
            .report_profile_photo(peer_id, photo, reason, comment)
    }

    fn personal_channel(
        &self,
        peer_id: PeerId,
    ) -> Option<tg_app_core::info::backend::PersonalChannelOption> {
        let st = self.state.lock().ok()?;
        st.profile_extras.personal_channel(peer_id)
    }

    fn personal_channel_candidates(
        &self,
    ) -> Vec<tg_app_core::info::backend::PersonalChannelOption> {
        let Ok(st) = self.state.lock() else {
            return Vec::new();
        };
        st.peers
            .iter()
            .filter(|p| p.kind == InfoPeerKind::Channel)
            .map(|p| tg_app_core::info::backend::PersonalChannelOption {
                peer_id: p.id,
                title: p.title.clone(),
                username: p
                    .usernames
                    .iter()
                    .find(|u| u.is_active)
                    .map(|u| u.username.clone()),
                subscribers_count: p.members_count,
            })
            .collect()
    }

    fn update_personal_channel(&mut self, channel_id: Option<PeerId>) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let self_id = st.user.id;
        let opt = match channel_id {
            Some(cid) => {
                let p = st
                    .peers
                    .iter()
                    .find(|p| p.id == cid && p.kind == InfoPeerKind::Channel)
                    .ok_or_else(|| "Selected channel not found.".to_owned())?;
                Some(tg_app_core::info::backend::PersonalChannelOption {
                    peer_id: p.id,
                    title: p.title.clone(),
                    username: p
                        .usernames
                        .iter()
                        .find(|u| u.is_active)
                        .map(|u| u.username.clone()),
                    subscribers_count: p.members_count,
                })
            }
            None => None,
        };
        st.profile_extras.set_personal_channel(self_id, opt);
        Ok(())
    }

    fn peer_profile_balance(
        &self,
        peer_id: PeerId,
    ) -> Option<tg_app_core::info::backend::ProfileBalanceInfo> {
        let st = self.state.lock().ok()?;
        st.profile_extras.peer_profile_balance(peer_id)
    }

    fn chat_star_gift_notifications_enabled(&self, peer_id: PeerId) -> Option<bool> {
        let st = self.state.lock().ok()?;
        st.profile_extras
            .chat_star_gift_notifications_enabled(peer_id)
    }

    fn toggle_chat_star_gift_notifications(
        &mut self,
        peer_id: PeerId,
        enabled: bool,
    ) -> Result<bool, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.profile_extras
            .toggle_chat_star_gift_notifications(peer_id, enabled)
    }

    fn upload_emoji_userpic(
        &mut self,
        peer_id: PeerId,
        req: EmojiUserpicRequest,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.profile_extras.upload_emoji_userpic(peer_id, req)
    }

    fn peer_available_reactions(&self, peer_id: PeerId) -> PeerReactionsConfig {
        self.state
            .lock()
            .map(|st| st.profile_extras.peer_available_reactions(peer_id))
            .unwrap_or_default()
    }

    fn set_peer_available_reactions(
        &mut self,
        peer_id: PeerId,
        config: PeerReactionsConfig,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.profile_extras
            .set_peer_available_reactions(peer_id, config)
    }

    fn toggle_peer_username(
        &mut self,
        peer_id: PeerId,
        username: &str,
        active: bool,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let Some(p) = st.peer_mut(peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        ProfileExtrasMockStore::toggle_username_in_list(&mut p.usernames, username, active)
    }

    fn reorder_peer_usernames(&mut self, peer_id: PeerId, order: &[String]) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let Some(p) = st.peer_mut(peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        ProfileExtrasMockStore::reorder_usernames_in_list(&mut p.usernames, order)
    }

    fn peer_verification_status(&self, bot_id: PeerId, peer_id: PeerId) -> PeerVerificationStatus {
        self.state
            .lock()
            .map(|st| st.profile_extras.verification_status(bot_id, peer_id))
            .unwrap_or(PeerVerificationStatus {
                bot_id,
                peer_id,
                verified: false,
                custom_description: String::new(),
            })
    }

    fn set_peer_custom_verification(
        &mut self,
        bot_id: PeerId,
        peer_id: PeerId,
        enabled: bool,
        custom_description: Option<&str>,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.profile_extras
            .set_custom_verification(bot_id, peer_id, enabled, custom_description)
    }
    fn joined_communities(
        &self,
    ) -> Result<Vec<tg_app_info::boxes::community::CommunityDetails>, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.communities.joined_communities())
    }

    fn community_details(
        &self,
        community_id: PeerId,
    ) -> Option<tg_app_info::boxes::community::CommunityDetails> {
        self.state
            .lock()
            .ok()
            .and_then(|st| st.communities.community_details(community_id))
    }

    fn community_for_peer(
        &self,
        peer_id: PeerId,
    ) -> Option<tg_app_info::boxes::community::CommunityDetails> {
        self.state
            .lock()
            .ok()
            .and_then(|st| st.communities.community_for_peer(peer_id))
    }

    fn create_community(
        &mut self,
        req: tg_app_info::boxes::community::CreateCommunityRequest,
    ) -> Result<PeerId, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let (title, is_ch) = st.peer(req.initial_peer_id).map_or_else(
            || ("Chat".to_owned(), false),
            |p| (p.title.clone(), p.kind.is_channel()),
        );
        st.communities.create_community(req, &title, is_ch)
    }

    fn toggle_community_peer_link(
        &mut self,
        community_id: PeerId,
        peer_id: PeerId,
        action: tg_app_info::boxes::community::CommunityPeerLinkAction,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        let (title, is_ch, is_bot) = st.peer(peer_id).map_or_else(
            || (format!("Peer {}", peer_id.bare()), false, false),
            |p| {
                (
                    p.title.clone(),
                    p.kind.is_channel(),
                    p.kind == InfoPeerKind::Bot,
                )
            },
        );
        st.communities
            .toggle_peer_link(community_id, peer_id, &title, is_ch, is_bot, action)
    }

    fn toggle_community_collapsed_in_dialogs(
        &mut self,
        community_id: PeerId,
        collapsed: bool,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.communities
            .toggle_collapsed_in_dialogs(community_id, collapsed)
    }

    fn community_peer_link_requests(
        &self,
        community_id: PeerId,
        _offset: &str,
        _limit: i32,
    ) -> Result<tg_app_info::boxes::community::CommunityPeerRequestsSlice, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.communities.peer_link_requests(community_id))
    }

    fn toggle_community_peer_link_request_approval(
        &mut self,
        community_id: PeerId,
        peer_id: PeerId,
        reject: bool,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.communities
            .toggle_peer_link_request_approval(community_id, peer_id, reject)
    }

    fn toggle_all_community_peer_link_request_approval(
        &mut self,
        community_id: PeerId,
        reject: bool,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.communities
            .toggle_all_peer_link_request_approval(community_id, reject)
    }

    fn toggle_community_participant_banned(
        &mut self,
        community_id: PeerId,
        participant_id: PeerId,
        unban: bool,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.communities
            .toggle_participant_banned(community_id, participant_id, unban)
    }

    fn community_participant_joined_chats(
        &self,
        community_id: PeerId,
        participant_id: PeerId,
    ) -> Result<tg_app_info::boxes::community::CommunityParticipantJoinedChats, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st
            .communities
            .participant_joined_chats(community_id, participant_id))
    }

    fn update_community_settings(
        &mut self,
        community_id: PeerId,
        title: &str,
        only_admins_can_add: bool,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.communities
            .update_community_settings(community_id, title, only_admins_can_add)
    }

    fn delete_community(&mut self, community_id: PeerId) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.communities.delete_community(community_id)
    }

    fn check_bot_username(&self, username: &str) -> Result<bool, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.communities.check_bot_username(username))
    }

    fn create_managed_bot(
        &mut self,
        manager_id: PeerId,
        name: &str,
        username: &str,
        via_deeplink: bool,
    ) -> Result<PeerId, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.communities
            .create_managed_bot(manager_id, name, username, via_deeplink)
    }

    fn get_bot_info(
        &self,
        bot_id: PeerId,
        lang_code: &str,
    ) -> Result<tg_app_info::boxes::managed_bot::BotInfoTexts, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.communities.get_bot_info(bot_id, lang_code))
    }

    fn set_bot_info(
        &mut self,
        bot_id: PeerId,
        lang_code: &str,
        change: &tg_app_info::boxes::managed_bot::BotInfoChange,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.communities.set_bot_info(bot_id, lang_code, change)
    }

    fn translate_bot_info_texts(
        &self,
        texts: &[String],
        lang_code: &str,
    ) -> Result<Vec<String>, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.communities.translate_bot_info_texts(texts, lang_code))
    }

    fn moderate_messages(
        &mut self,
        req: tg_app_info::boxes::moderate_messages::ModerateMessagesRequest,
    ) -> Result<tg_app_info::boxes::moderate_messages::ModerateMessagesResult, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.communities.moderate_messages(req)
    }

    fn welcome_messages(
        &self,
        peer_id: PeerId,
    ) -> Result<Vec<tg_app_history::history::welcome_messages::WelcomeMessageItem>, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.communities.welcome_messages(peer_id))
    }

    fn send_welcome_message(
        &mut self,
        peer_id: PeerId,
        text: &str,
    ) -> Result<tg_app_history::history::welcome_messages::WelcomeMessageItem, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.communities
            .send_welcome_message(peer_id, session_peers::SELF, text)
    }

    fn edit_welcome_message(
        &mut self,
        peer_id: PeerId,
        id: i32,
        text: &str,
    ) -> Result<tg_app_history::history::welcome_messages::WelcomeMessageItem, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.communities.edit_welcome_message(peer_id, id, text)
    }

    fn delete_welcome_message(&mut self, peer_id: PeerId, id: i32) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.communities.delete_welcome_message(peer_id, id)
    }

    fn delete_all_welcome_messages(&mut self, peer_id: PeerId) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.communities.delete_all_welcome_messages(peer_id)
    }

    fn channel_anti_spam_enabled(&self, peer_id: PeerId) -> bool {
        self.state
            .lock()
            .map(|st| st.communities.channel_anti_spam_enabled(peer_id))
            .unwrap_or(false)
    }

    fn toggle_channel_anti_spam(&mut self, peer_id: PeerId, enabled: bool) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.communities.toggle_channel_anti_spam(peer_id, enabled)
    }

    fn report_antispam_false_positive(
        &mut self,
        peer_id: PeerId,
        msg_id: i32,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.communities
            .report_antispam_false_positive(peer_id, msg_id)
    }

    fn get_collectible_info(
        &self,
        peer_id: PeerId,
        entity: &str,
    ) -> Result<tg_app_info::boxes::collectible_info::CollectibleInfoData, String> {
        let (owner_name, color_index) = self.peer_profile(peer_id).map_or_else(
            || ("Demo Owner".to_owned(), 0),
            |p| (p.title, p.color_index),
        );
        let clean = entity.trim().trim_start_matches('@');
        let slug: String = clean
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        let url = if entity.trim().starts_with('+') {
            format!("https://fragment.com/number/{slug}")
        } else {
            format!("https://fragment.com/username/{slug}")
        };
        Ok(
            tg_app_info::boxes::collectible_info::CollectibleInfoData::from_fragment_fields(
                entity.trim(),
                peer_id,
                owner_name,
                color_index,
                1_700_000_000,
                "USD",
                45_000,
                "TON",
                85_000_000_000,
                url,
            ),
        )
    }

    fn channel_management_state(
        &self,
        peer_id: PeerId,
    ) -> Option<tg_app_core::info::backend::ChannelManagementState> {
        self.state
            .lock()
            .ok()?
            .peer_mgmt
            .channel_management_state(peer_id)
    }

    fn toggle_pre_history_hidden(&mut self, peer_id: PeerId, hidden: bool) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.peer_mgmt.toggle_pre_history_hidden(peer_id, hidden);
        Ok(())
    }

    fn groups_for_discussion(
        &self,
    ) -> Result<Vec<tg_app_core::info::backend::DiscussionGroupCandidate>, String> {
        Ok(self
            .all_peers()
            .into_iter()
            .filter(|p| p.kind.is_group())
            .map(|p| tg_app_core::info::backend::DiscussionGroupCandidate {
                peer_id: p.id,
                title: p.title.clone(),
                username: p.primary_username().map(ToOwned::to_owned),
                members_count: p.members_count,
                is_megagroup: p.kind == InfoPeerKind::Supergroup,
                is_public: p.is_public,
            })
            .collect())
    }

    fn convert_to_gigagroup(&mut self, peer_id: PeerId) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.peer_mgmt.convert_to_gigagroup(peer_id);
        Ok(())
    }

    fn deactivate_all_channel_usernames(&mut self, peer_id: PeerId) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        if let Some(p) = st.peers.iter_mut().find(|p| p.id == peer_id) {
            for u in &mut p.usernames {
                if !u.is_editable {
                    u.is_active = false;
                }
            }
        }
        st.peer_mgmt.deactivate_all_channel_usernames(peer_id);
        Ok(())
    }

    fn toggle_channel_forum(
        &mut self,
        peer_id: PeerId,
        enabled: bool,
        tabs: bool,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        if let Some(p) = st.peers.iter_mut().find(|p| p.id == peer_id) {
            p.is_forum = enabled;
        }
        st.peer_mgmt.toggle_channel_forum(peer_id, enabled, tabs);
        Ok(())
    }

    fn toggle_participants_hidden(&mut self, peer_id: PeerId, hidden: bool) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.peer_mgmt.toggle_participants_hidden(peer_id, hidden);
        Ok(())
    }

    fn toggle_view_forum_as_messages(
        &mut self,
        peer_id: PeerId,
        as_messages: bool,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.peer_mgmt
            .toggle_view_forum_as_messages(peer_id, as_messages);
        Ok(())
    }

    fn set_boosts_to_unblock_restrictions(
        &mut self,
        peer_id: PeerId,
        boosts: u32,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.peer_mgmt
            .set_boosts_to_unblock_restrictions(peer_id, boosts);
        Ok(())
    }

    fn set_channel_sticker_set(
        &mut self,
        peer_id: PeerId,
        set: tg_app_core::info::backend::ChannelStickerSetRef,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.peer_mgmt.set_channel_sticker_set(peer_id, set);
        Ok(())
    }

    fn set_channel_emoji_sticker_set(
        &mut self,
        peer_id: PeerId,
        set: tg_app_core::info::backend::ChannelStickerSetRef,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.peer_mgmt.set_channel_emoji_sticker_set(peer_id, set);
        Ok(())
    }

    fn update_channel_paid_messages_price(
        &mut self,
        peer_id: PeerId,
        broadcast_messages_allowed: bool,
        stars_per_message: i64,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.peer_mgmt.update_channel_paid_messages_price(
            peer_id,
            broadcast_messages_allowed,
            stars_per_message,
        );
        Ok(())
    }

    fn toggle_channel_autotranslation(
        &mut self,
        peer_id: PeerId,
        enabled: bool,
    ) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.peer_mgmt
            .toggle_channel_autotranslation(peer_id, enabled);
        Ok(())
    }

    fn delete_basic_chat(&mut self, peer_id: PeerId) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.peer_mgmt.delete_basic_chat(peer_id);
        st.peers.retain(|p| p.id != peer_id);
        Ok(())
    }

    fn admins_with_invites(
        &self,
        peer_id: PeerId,
    ) -> Result<Vec<tg_app_core::info::backend::AdminWithInvitesEntry>, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.peer_mgmt.admins_with_invites(peer_id))
    }

    fn delete_contacts(&mut self, peer_ids: &[PeerId]) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        for &pid in peer_ids {
            if let Some(p) = st.peers.iter_mut().find(|p| p.id == pid) {
                p.is_contact = false;
            }
        }
        st.peer_mgmt.record_deleted_contacts(peer_ids);
        Ok(())
    }

    fn export_contact_token(
        &self,
    ) -> Result<tg_app_core::core_types::dto::contacts_extras::ExportedContactTokenInfo, String>
    {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.peer_mgmt.export_contact_token())
    }

    fn import_contact_token(&mut self, token: &str) -> Result<PeerId, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.peer_mgmt
            .import_contact_token(token, session_peers::USER_MIRA)
    }

    fn export_no_paid_messages_token(
        &self,
        revoke: bool,
        temporary: bool,
    ) -> Result<
        tg_app_core::core_types::dto::contacts_extras::ExportedNoPaidMessagesTokenInfo,
        String,
    > {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st
            .peer_mgmt
            .export_no_paid_messages_token(revoke, temporary))
    }

    fn import_no_paid_messages_token(
        &mut self,
        peer_id: PeerId,
        token: &str,
    ) -> Result<
        tg_app_core::core_types::dto::contacts_extras::NoPaidMessagesTokenImportResult,
        String,
    > {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.peer_mgmt.import_no_paid_messages_token(peer_id, token)
    }

    fn contact_note(&self, peer_id: PeerId) -> Option<TextWithEntities> {
        self.state.lock().ok()?.peer_mgmt.contact_note(peer_id)
    }

    fn update_contact_note(
        &mut self,
        peer_id: PeerId,
        note: TextWithEntities,
    ) -> Result<bool, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.peer_mgmt.update_contact_note(peer_id, note))
    }

    fn delete_profile_photos(
        &mut self,
        photos: &[tg_app_core::info::backend::ProfilePhotoRef],
    ) -> Result<Vec<i64>, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.peer_mgmt.delete_profile_photos(photos))
    }

    fn upload_contact_profile_photo(
        &mut self,
        peer_id: PeerId,
        request: tg_app_core::core_types::dto::contacts_extras::ContactProfilePhotoUpload,
    ) -> Result<i64, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        Ok(st.peer_mgmt.upload_contact_profile_photo(peer_id, request))
    }
}

impl MockSession {
    /// Sets the verified badge of `peer_id` (tests: an unverified bot must confirm Mini Apps).
    pub fn set_peer_verified(&mut self, peer_id: PeerId, verified: bool) {
        if let Ok(mut st) = self.handle.state.lock()
            && let Some(rec) = st.peers.iter_mut().find(|p| p.id == peer_id)
        {
            rec.verified = verified;
        }
    }
}

impl InfoBackend for MockSession {
    fn peer_profile(&self, peer_id: PeerId) -> Option<PeerProfile> {
        self.handle.peer_profile(peer_id)
    }
    fn all_peers(&self) -> Vec<PeerProfile> {
        self.handle.all_peers()
    }
    fn contacts(&self) -> Vec<PeerProfile> {
        InfoBackend::contacts(&self.handle)
    }
    fn peer_members(&self, peer_id: PeerId, query: &str) -> Vec<MemberEntry> {
        self.handle.peer_members(peer_id, query)
    }
    fn common_groups(&self, user_id: PeerId) -> Vec<CommonGroupEntry> {
        self.handle.common_groups(user_id)
    }
    fn shared_media(
        &self,
        peer_id: PeerId,
        tab: SharedMediaTab,
        query: &str,
    ) -> Vec<InfoMediaItem> {
        self.handle.shared_media(peer_id, tab, query)
    }
    fn global_media(
        &self,
        tab: SharedMediaTab,
        query: &str,
        limit: usize,
    ) -> tg_app_core::core_types::dto::global_media::GlobalMediaPage {
        self.handle.global_media(tab, query, limit)
    }
    fn toggle_notifications(&mut self, peer_id: PeerId) -> bool {
        self.handle.toggle_notifications(peer_id)
    }
    fn toggle_block_user(&mut self, peer_id: PeerId) -> bool {
        self.handle.toggle_block_user(peer_id)
    }
    fn add_contact(
        &mut self,
        first_name: &str,
        last_name: &str,
        phone: &str,
    ) -> Result<PeerId, String> {
        InfoBackend::add_contact(&mut self.handle, first_name, last_name, phone)
    }
    fn create_group(&mut self, req: CreateGroupRequest) -> Result<PeerId, String> {
        self.handle.create_group(req)
    }
    fn create_channel(&mut self, req: CreateChannelRequest) -> Result<PeerId, String> {
        self.handle.create_channel(req)
    }
    fn update_peer_info(
        &mut self,
        peer_id: PeerId,
        req: UpdatePeerInfoRequest,
    ) -> Result<(), String> {
        self.handle.update_peer_info(peer_id, req)
    }
    fn update_default_permissions(
        &mut self,
        peer_id: PeerId,
        rights: ChatBannedRights,
        slowmode_seconds: u32,
    ) -> Result<(), String> {
        self.handle
            .update_default_permissions(peer_id, rights, slowmode_seconds)
    }
    fn update_admin_rights(
        &mut self,
        peer_id: PeerId,
        user_id: PeerId,
        rights: ChatAdminRights,
        custom_title: String,
    ) -> Result<(), String> {
        self.handle
            .update_admin_rights(peer_id, user_id, rights, custom_title)
    }
    fn add_members(&mut self, peer_id: PeerId, member_ids: &[PeerId]) -> Result<usize, String> {
        self.handle.add_members(peer_id, member_ids)
    }
    fn remove_member(&mut self, peer_id: PeerId, user_id: PeerId) -> Result<(), String> {
        self.handle.remove_member(peer_id, user_id)
    }
    fn set_auto_delete_ttl(&mut self, peer_id: PeerId, ttl_seconds: u32) -> Result<(), String> {
        self.handle.set_auto_delete_ttl(peer_id, ttl_seconds)
    }
    fn set_linked_discussion(
        &mut self,
        channel_id: PeerId,
        group_id: Option<PeerId>,
    ) -> Result<(), String> {
        self.handle.set_linked_discussion(channel_id, group_id)
    }
    fn check_username(&self, username: &str, exclude_peer: Option<PeerId>) -> UsernameAvailability {
        InfoBackend::check_username(&self.handle, username, exclude_peer)
    }
    fn update_username(&mut self, peer_id: PeerId, username: &str) -> Result<(), String> {
        let res = InfoBackend::update_username(&mut self.handle, peer_id, username);
        self.sync_cached_user();
        res
    }
    fn forward_messages(
        &mut self,
        target_peers: &[PeerId],
        message_ids: &[i32],
        comment: &str,
    ) -> Result<usize, String> {
        InfoBackend::forward_messages(&mut self.handle, target_peers, message_ids, comment)
    }
    fn delete_messages(
        &mut self,
        peer_id: PeerId,
        message_ids: &[i32],
        revoke_for_everyone: bool,
    ) -> Result<usize, String> {
        InfoBackend::delete_messages(&mut self.handle, peer_id, message_ids, revoke_for_everyone)
    }
    fn submit_report(
        &mut self,
        peer_id: PeerId,
        reason: ReportReason,
        comment: &str,
    ) -> Result<(), String> {
        self.handle.submit_report(peer_id, reason, comment)
    }
    fn languages(&self) -> Vec<LanguageEntry> {
        InfoBackend::languages(&self.handle)
    }
    fn active_language(&self) -> String {
        self.handle.active_language()
    }
    fn set_language(&mut self, lang_code: &str) -> Result<(), String> {
        self.handle.set_language(lang_code)
    }
    fn terms_of_service(&self) -> TermsOfServiceData {
        self.handle.terms_of_service()
    }
    fn accept_terms(&mut self, tos_id: &str) -> Result<(), String> {
        self.handle.accept_terms(tos_id)
    }
    fn exported_chat_invites(
        &self,
        peer_id: PeerId,
        revoked: bool,
    ) -> Result<InviteLinksList, String> {
        self.handle.exported_chat_invites(peer_id, revoked)
    }
    fn export_chat_invite(
        &mut self,
        peer_id: PeerId,
        req: InviteLinkRequest,
    ) -> Result<InviteLinkInfo, String> {
        self.handle.export_chat_invite(peer_id, req)
    }
    fn edit_exported_chat_invite(
        &mut self,
        peer_id: PeerId,
        link: &str,
        req: InviteLinkRequest,
    ) -> Result<InviteLinkInfo, String> {
        self.handle.edit_exported_chat_invite(peer_id, link, req)
    }
    fn revoke_exported_chat_invite(
        &mut self,
        peer_id: PeerId,
        link: &str,
    ) -> Result<(InviteLinkInfo, Option<InviteLinkInfo>), String> {
        self.handle.revoke_exported_chat_invite(peer_id, link)
    }
    fn delete_exported_chat_invite(&mut self, peer_id: PeerId, link: &str) -> Result<(), String> {
        self.handle.delete_exported_chat_invite(peer_id, link)
    }
    fn delete_revoked_exported_chat_invites(&mut self, peer_id: PeerId) -> Result<(), String> {
        self.handle.delete_revoked_exported_chat_invites(peer_id)
    }
    fn chat_invite_importers(
        &self,
        peer_id: PeerId,
        link: Option<&str>,
        requested: bool,
        query: &str,
    ) -> Result<InviteImportersList, String> {
        self.handle
            .chat_invite_importers(peer_id, link, requested, query)
    }
    fn hide_chat_join_request(
        &mut self,
        peer_id: PeerId,
        user_id: PeerId,
        approved: bool,
    ) -> Result<(), String> {
        self.handle
            .hide_chat_join_request(peer_id, user_id, approved)
    }
    fn hide_all_chat_join_requests(
        &mut self,
        peer_id: PeerId,
        link: Option<&str>,
        approved: bool,
    ) -> Result<usize, String> {
        self.handle
            .hide_all_chat_join_requests(peer_id, link, approved)
    }
    fn join_requests_summary(&self, peer_id: PeerId) -> JoinRequestsSummary {
        self.handle.join_requests_summary(peer_id)
    }
    fn peer_color_settings(&self, peer_id: PeerId) -> PeerColorSettings {
        self.handle.peer_color_settings(peer_id)
    }
    fn update_peer_color(
        &mut self,
        peer_id: PeerId,
        req: UpdatePeerColorRequest,
    ) -> Result<(), String> {
        self.handle.update_peer_color(peer_id, req)
    }
    fn check_ownership_transfer(&self, peer_id: PeerId) -> Result<OwnershipTransferCheck, String> {
        self.handle.check_ownership_transfer(peer_id)
    }
    fn transfer_ownership(
        &mut self,
        peer_id: PeerId,
        new_owner_id: PeerId,
        password: &str,
    ) -> Result<(), OwnershipTransferError> {
        self.handle
            .transfer_ownership(peer_id, new_owner_id, password)
    }
    fn similar_channels(&self, peer_id: PeerId) -> Result<SimilarChannelsResult, String> {
        self.handle.similar_channels(peer_id)
    }
    fn downloads_list(&self, query: &str) -> Vec<DownloadEntry> {
        self.handle.downloads_list(query)
    }
    fn toggle_download_pause(&mut self, download_id: u64) -> Result<(), String> {
        self.handle.toggle_download_pause(download_id)
    }
    fn cancel_or_delete_download(
        &mut self,
        download_id: u64,
        delete_file: bool,
    ) -> Result<(), String> {
        self.handle
            .cancel_or_delete_download(download_id, delete_file)
    }
    fn clear_completed_downloads(&mut self) -> Result<usize, String> {
        self.handle.clear_completed_downloads()
    }
    fn delete_all_downloads(&mut self) -> Result<usize, String> {
        self.handle.delete_all_downloads()
    }
    fn peer_pinned_stories(
        &self,
        peer_id: PeerId,
        offset_id: i32,
        limit: i32,
    ) -> Result<ProfileStoriesPage, String> {
        self.handle.peer_pinned_stories(peer_id, offset_id, limit)
    }
    fn peer_stories_archive(
        &self,
        peer_id: PeerId,
        offset_id: i32,
        limit: i32,
    ) -> Result<ProfileStoriesPage, String> {
        self.handle.peer_stories_archive(peer_id, offset_id, limit)
    }
    fn peer_story_albums(&self, peer_id: PeerId) -> Result<Vec<ProfileStoryAlbum>, String> {
        self.handle.peer_story_albums(peer_id)
    }
    fn peer_album_stories(
        &self,
        peer_id: PeerId,
        album_id: i32,
        offset_id: i32,
        limit: i32,
    ) -> Result<ProfileStoriesPage, String> {
        self.handle
            .peer_album_stories(peer_id, album_id, offset_id, limit)
    }
    fn peer_saved_gifts(
        &self,
        peer_id: PeerId,
        filter: &tg_payments::star_gifts::PeerGiftsFilter,
    ) -> Result<ProfilePeerGiftsPage, String> {
        self.handle.peer_saved_gifts(peer_id, filter)
    }
    fn toggle_peer_gift_saved(
        &mut self,
        peer_id: PeerId,
        msg_id: i32,
        saved_id: u64,
        unsave: bool,
    ) -> Result<(), String> {
        self.handle
            .toggle_peer_gift_saved(peer_id, msg_id, saved_id, unsave)
    }
    fn peer_saved_music(
        &self,
        peer_id: PeerId,
        offset: i32,
        limit: i32,
    ) -> Result<Vec<SavedMusicTrack>, String> {
        self.handle.peer_saved_music(peer_id, offset, limit)
    }
    fn save_profile_music(&mut self, track: &SavedMusicTrack, unsave: bool) -> Result<(), String> {
        self.handle.save_profile_music(track, unsave)
    }
    fn set_main_profile_tab(
        &mut self,
        peer_id: PeerId,
        tab: tg_app_core::info::backend::ProfileMainTab,
    ) -> Result<(), String> {
        self.handle.set_main_profile_tab(peer_id, tab)
    }
    fn main_profile_tab(
        &self,
        peer_id: PeerId,
    ) -> Option<tg_app_core::info::backend::ProfileMainTab> {
        self.handle.main_profile_tab(peer_id)
    }
    fn suggest_user_birthday(
        &mut self,
        user_id: PeerId,
        birthday: tg_app_core::info::values::Birthday,
    ) -> Result<(), String> {
        self.handle.suggest_user_birthday(user_id, birthday)
    }
    fn report_profile_photo(
        &mut self,
        peer_id: PeerId,
        photo: tg_app_core::info::backend::ProfilePhotoRef,
        reason: ReportReason,
        comment: &str,
    ) -> Result<(), String> {
        self.handle
            .report_profile_photo(peer_id, photo, reason, comment)
    }
    fn personal_channel(
        &self,
        peer_id: PeerId,
    ) -> Option<tg_app_core::info::backend::PersonalChannelOption> {
        self.handle.personal_channel(peer_id)
    }
    fn personal_channel_candidates(
        &self,
    ) -> Vec<tg_app_core::info::backend::PersonalChannelOption> {
        self.handle.personal_channel_candidates()
    }
    fn update_personal_channel(&mut self, channel_id: Option<PeerId>) -> Result<(), String> {
        InfoBackend::update_personal_channel(&mut self.handle, channel_id)
    }
    fn peer_profile_balance(
        &self,
        peer_id: PeerId,
    ) -> Option<tg_app_core::info::backend::ProfileBalanceInfo> {
        self.handle.peer_profile_balance(peer_id)
    }
    fn chat_star_gift_notifications_enabled(&self, peer_id: PeerId) -> Option<bool> {
        self.handle.chat_star_gift_notifications_enabled(peer_id)
    }
    fn toggle_chat_star_gift_notifications(
        &mut self,
        peer_id: PeerId,
        enabled: bool,
    ) -> Result<bool, String> {
        self.handle
            .toggle_chat_star_gift_notifications(peer_id, enabled)
    }
    fn upload_emoji_userpic(
        &mut self,
        peer_id: PeerId,
        req: EmojiUserpicRequest,
    ) -> Result<(), String> {
        self.handle.upload_emoji_userpic(peer_id, req)
    }
    fn peer_available_reactions(&self, peer_id: PeerId) -> PeerReactionsConfig {
        self.handle.peer_available_reactions(peer_id)
    }
    fn set_peer_available_reactions(
        &mut self,
        peer_id: PeerId,
        config: PeerReactionsConfig,
    ) -> Result<(), String> {
        self.handle.set_peer_available_reactions(peer_id, config)
    }
    fn toggle_peer_username(
        &mut self,
        peer_id: PeerId,
        username: &str,
        active: bool,
    ) -> Result<(), String> {
        self.handle.toggle_peer_username(peer_id, username, active)
    }
    fn reorder_peer_usernames(&mut self, peer_id: PeerId, order: &[String]) -> Result<(), String> {
        self.handle.reorder_peer_usernames(peer_id, order)
    }
    fn peer_verification_status(&self, bot_id: PeerId, peer_id: PeerId) -> PeerVerificationStatus {
        self.handle.peer_verification_status(bot_id, peer_id)
    }
    fn set_peer_custom_verification(
        &mut self,
        bot_id: PeerId,
        peer_id: PeerId,
        enabled: bool,
        custom_description: Option<&str>,
    ) -> Result<(), String> {
        self.handle
            .set_peer_custom_verification(bot_id, peer_id, enabled, custom_description)
    }
    fn joined_communities(
        &self,
    ) -> Result<Vec<tg_app_info::boxes::community::CommunityDetails>, String> {
        self.handle.joined_communities()
    }
    fn community_details(
        &self,
        community_id: PeerId,
    ) -> Option<tg_app_info::boxes::community::CommunityDetails> {
        self.handle.community_details(community_id)
    }
    fn community_for_peer(
        &self,
        peer_id: PeerId,
    ) -> Option<tg_app_info::boxes::community::CommunityDetails> {
        self.handle.community_for_peer(peer_id)
    }
    fn create_community(
        &mut self,
        req: tg_app_info::boxes::community::CreateCommunityRequest,
    ) -> Result<PeerId, String> {
        self.handle.create_community(req)
    }
    fn toggle_community_peer_link(
        &mut self,
        community_id: PeerId,
        peer_id: PeerId,
        action: tg_app_info::boxes::community::CommunityPeerLinkAction,
    ) -> Result<(), String> {
        self.handle
            .toggle_community_peer_link(community_id, peer_id, action)
    }
    fn toggle_community_collapsed_in_dialogs(
        &mut self,
        community_id: PeerId,
        collapsed: bool,
    ) -> Result<(), String> {
        self.handle
            .toggle_community_collapsed_in_dialogs(community_id, collapsed)
    }
    fn community_peer_link_requests(
        &self,
        community_id: PeerId,
        offset: &str,
        limit: i32,
    ) -> Result<tg_app_info::boxes::community::CommunityPeerRequestsSlice, String> {
        self.handle
            .community_peer_link_requests(community_id, offset, limit)
    }
    fn toggle_community_peer_link_request_approval(
        &mut self,
        community_id: PeerId,
        peer_id: PeerId,
        reject: bool,
    ) -> Result<(), String> {
        self.handle
            .toggle_community_peer_link_request_approval(community_id, peer_id, reject)
    }
    fn toggle_all_community_peer_link_request_approval(
        &mut self,
        community_id: PeerId,
        reject: bool,
    ) -> Result<(), String> {
        self.handle
            .toggle_all_community_peer_link_request_approval(community_id, reject)
    }
    fn toggle_community_participant_banned(
        &mut self,
        community_id: PeerId,
        participant_id: PeerId,
        unban: bool,
    ) -> Result<(), String> {
        self.handle
            .toggle_community_participant_banned(community_id, participant_id, unban)
    }
    fn community_participant_joined_chats(
        &self,
        community_id: PeerId,
        participant_id: PeerId,
    ) -> Result<tg_app_info::boxes::community::CommunityParticipantJoinedChats, String> {
        self.handle
            .community_participant_joined_chats(community_id, participant_id)
    }
    fn update_community_settings(
        &mut self,
        community_id: PeerId,
        title: &str,
        only_admins_can_add: bool,
    ) -> Result<(), String> {
        self.handle
            .update_community_settings(community_id, title, only_admins_can_add)
    }
    fn delete_community(&mut self, community_id: PeerId) -> Result<(), String> {
        self.handle.delete_community(community_id)
    }
    fn check_bot_username(&self, username: &str) -> Result<bool, String> {
        self.handle.check_bot_username(username)
    }
    fn create_managed_bot(
        &mut self,
        manager_id: PeerId,
        name: &str,
        username: &str,
        via_deeplink: bool,
    ) -> Result<PeerId, String> {
        self.handle
            .create_managed_bot(manager_id, name, username, via_deeplink)
    }
    fn get_bot_info(
        &self,
        bot_id: PeerId,
        lang_code: &str,
    ) -> Result<tg_app_info::boxes::managed_bot::BotInfoTexts, String> {
        self.handle.get_bot_info(bot_id, lang_code)
    }
    fn set_bot_info(
        &mut self,
        bot_id: PeerId,
        lang_code: &str,
        change: &tg_app_info::boxes::managed_bot::BotInfoChange,
    ) -> Result<(), String> {
        self.handle.set_bot_info(bot_id, lang_code, change)
    }
    fn translate_bot_info_texts(
        &self,
        texts: &[String],
        lang_code: &str,
    ) -> Result<Vec<String>, String> {
        self.handle.translate_bot_info_texts(texts, lang_code)
    }
    fn moderate_messages(
        &mut self,
        req: tg_app_info::boxes::moderate_messages::ModerateMessagesRequest,
    ) -> Result<tg_app_info::boxes::moderate_messages::ModerateMessagesResult, String> {
        self.handle.moderate_messages(req)
    }
    fn welcome_messages(
        &self,
        peer_id: PeerId,
    ) -> Result<Vec<tg_app_history::history::welcome_messages::WelcomeMessageItem>, String> {
        self.handle.welcome_messages(peer_id)
    }
    fn send_welcome_message(
        &mut self,
        peer_id: PeerId,
        text: &str,
    ) -> Result<tg_app_history::history::welcome_messages::WelcomeMessageItem, String> {
        self.handle.send_welcome_message(peer_id, text)
    }
    fn edit_welcome_message(
        &mut self,
        peer_id: PeerId,
        id: i32,
        text: &str,
    ) -> Result<tg_app_history::history::welcome_messages::WelcomeMessageItem, String> {
        self.handle.edit_welcome_message(peer_id, id, text)
    }
    fn delete_welcome_message(&mut self, peer_id: PeerId, id: i32) -> Result<(), String> {
        self.handle.delete_welcome_message(peer_id, id)
    }
    fn delete_all_welcome_messages(&mut self, peer_id: PeerId) -> Result<(), String> {
        self.handle.delete_all_welcome_messages(peer_id)
    }
    fn channel_anti_spam_enabled(&self, peer_id: PeerId) -> bool {
        self.handle.channel_anti_spam_enabled(peer_id)
    }
    fn toggle_channel_anti_spam(&mut self, peer_id: PeerId, enabled: bool) -> Result<(), String> {
        self.handle.toggle_channel_anti_spam(peer_id, enabled)
    }
    fn report_antispam_false_positive(
        &mut self,
        peer_id: PeerId,
        msg_id: i32,
    ) -> Result<(), String> {
        self.handle.report_antispam_false_positive(peer_id, msg_id)
    }

    fn get_collectible_info(
        &self,
        peer_id: PeerId,
        entity: &str,
    ) -> Result<tg_app_info::boxes::collectible_info::CollectibleInfoData, String> {
        self.handle.get_collectible_info(peer_id, entity)
    }

    fn channel_management_state(
        &self,
        peer_id: PeerId,
    ) -> Option<tg_app_core::info::backend::ChannelManagementState> {
        self.handle.channel_management_state(peer_id)
    }
    fn toggle_pre_history_hidden(&mut self, peer_id: PeerId, hidden: bool) -> Result<(), String> {
        self.handle.toggle_pre_history_hidden(peer_id, hidden)
    }
    fn groups_for_discussion(
        &self,
    ) -> Result<Vec<tg_app_core::info::backend::DiscussionGroupCandidate>, String> {
        self.handle.groups_for_discussion()
    }
    fn convert_to_gigagroup(&mut self, peer_id: PeerId) -> Result<(), String> {
        self.handle.convert_to_gigagroup(peer_id)
    }
    fn deactivate_all_channel_usernames(&mut self, peer_id: PeerId) -> Result<(), String> {
        self.handle.deactivate_all_channel_usernames(peer_id)
    }
    fn toggle_channel_forum(
        &mut self,
        peer_id: PeerId,
        enabled: bool,
        tabs: bool,
    ) -> Result<(), String> {
        self.handle.toggle_channel_forum(peer_id, enabled, tabs)
    }
    fn toggle_participants_hidden(&mut self, peer_id: PeerId, hidden: bool) -> Result<(), String> {
        self.handle.toggle_participants_hidden(peer_id, hidden)
    }
    fn toggle_view_forum_as_messages(
        &mut self,
        peer_id: PeerId,
        as_messages: bool,
    ) -> Result<(), String> {
        self.handle
            .toggle_view_forum_as_messages(peer_id, as_messages)
    }
    fn set_boosts_to_unblock_restrictions(
        &mut self,
        peer_id: PeerId,
        boosts: u32,
    ) -> Result<(), String> {
        self.handle
            .set_boosts_to_unblock_restrictions(peer_id, boosts)
    }
    fn set_channel_sticker_set(
        &mut self,
        peer_id: PeerId,
        set: tg_app_core::info::backend::ChannelStickerSetRef,
    ) -> Result<(), String> {
        self.handle.set_channel_sticker_set(peer_id, set)
    }
    fn set_channel_emoji_sticker_set(
        &mut self,
        peer_id: PeerId,
        set: tg_app_core::info::backend::ChannelStickerSetRef,
    ) -> Result<(), String> {
        self.handle.set_channel_emoji_sticker_set(peer_id, set)
    }
    fn update_channel_paid_messages_price(
        &mut self,
        peer_id: PeerId,
        broadcast_messages_allowed: bool,
        stars_per_message: i64,
    ) -> Result<(), String> {
        self.handle.update_channel_paid_messages_price(
            peer_id,
            broadcast_messages_allowed,
            stars_per_message,
        )
    }
    fn toggle_channel_autotranslation(
        &mut self,
        peer_id: PeerId,
        enabled: bool,
    ) -> Result<(), String> {
        self.handle.toggle_channel_autotranslation(peer_id, enabled)
    }
    fn delete_basic_chat(&mut self, peer_id: PeerId) -> Result<(), String> {
        self.handle.delete_basic_chat(peer_id)
    }
    fn admins_with_invites(
        &self,
        peer_id: PeerId,
    ) -> Result<Vec<tg_app_core::info::backend::AdminWithInvitesEntry>, String> {
        self.handle.admins_with_invites(peer_id)
    }
    fn delete_contacts(&mut self, peer_ids: &[PeerId]) -> Result<(), String> {
        self.handle.delete_contacts(peer_ids)
    }
    fn export_contact_token(
        &self,
    ) -> Result<tg_app_core::core_types::dto::contacts_extras::ExportedContactTokenInfo, String>
    {
        self.handle.export_contact_token()
    }
    fn import_contact_token(&mut self, token: &str) -> Result<PeerId, String> {
        self.handle.import_contact_token(token)
    }
    fn export_no_paid_messages_token(
        &self,
        revoke: bool,
        temporary: bool,
    ) -> Result<
        tg_app_core::core_types::dto::contacts_extras::ExportedNoPaidMessagesTokenInfo,
        String,
    > {
        self.handle.export_no_paid_messages_token(revoke, temporary)
    }
    fn import_no_paid_messages_token(
        &mut self,
        peer_id: PeerId,
        token: &str,
    ) -> Result<
        tg_app_core::core_types::dto::contacts_extras::NoPaidMessagesTokenImportResult,
        String,
    > {
        self.handle.import_no_paid_messages_token(peer_id, token)
    }
    fn contact_note(&self, peer_id: PeerId) -> Option<TextWithEntities> {
        self.handle.contact_note(peer_id)
    }
    fn update_contact_note(
        &mut self,
        peer_id: PeerId,
        note: TextWithEntities,
    ) -> Result<bool, String> {
        self.handle.update_contact_note(peer_id, note)
    }
    fn delete_profile_photos(
        &mut self,
        photos: &[tg_app_core::info::backend::ProfilePhotoRef],
    ) -> Result<Vec<i64>, String> {
        self.handle.delete_profile_photos(photos)
    }
    fn upload_contact_profile_photo(
        &mut self,
        peer_id: PeerId,
        request: tg_app_core::core_types::dto::contacts_extras::ContactProfilePhotoUpload,
    ) -> Result<i64, String> {
        self.handle.upload_contact_profile_photo(peer_id, request)
    }
}

// =============================================================================
// 5. settings::backend::SettingsBackend
// =============================================================================

impl SettingsBackend for SharedSessionHandle {
    fn profile(&self) -> ProfileInfo {
        self.state
            .lock()
            .map(|st| {
                let mut p = st.settings_store.profile();
                p.phone = st.user.phone.clone();
                p
            })
            .unwrap_or_else(|_| MockSettingsBackend::new().profile())
    }

    fn update_name(&mut self, first_name: &str, last_name: &str) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.settings_store.update_name(first_name, last_name)?;
        st.sync_auth_user_from_settings();
        Ok(())
    }

    fn update_bio(&mut self, bio: &str) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.settings_store.update_bio(bio)?;
        st.sync_auth_user_from_settings();
        Ok(())
    }

    fn check_username(&self, username: &str) -> UsernameStatus {
        self.state.lock().map_or(UsernameStatus::Empty, |st| {
            st.settings_store.check_username(username)
        })
    }

    fn update_username(&mut self, username: &str) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.settings_store.update_username(username)?;
        st.sync_auth_user_from_settings();
        Ok(())
    }

    fn update_birthday(&mut self, birthday: Option<SettingsBirthday>) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.settings_store.update_birthday(birthday)
    }

    fn update_profile_photo(&mut self, color_index: u8, photo_label: Option<String>) {
        if let Ok(mut st) = self.state.lock() {
            st.settings_store
                .update_profile_photo(color_index, photo_label);
            st.user.avatar_rgb = avatar_rgb_for_index(color_index);
        }
    }

    fn set_phone_hidden(&mut self, hidden: bool) {
        if let Ok(mut st) = self.state.lock() {
            st.settings_store.set_phone_hidden(hidden);
        }
    }

    fn contacts(&self) -> Vec<PeerRef> {
        self.state
            .lock()
            .map(|st| {
                st.peers
                    .iter()
                    .filter(|p| p.id != session_peers::SELF)
                    .map(PeerRecord::to_peer_ref)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn privacy_rules(&self) -> BTreeMap<PrivacyKey, PrivacyRule> {
        self.state
            .lock()
            .map(|st| st.settings_store.privacy_rules())
            .unwrap_or_default()
    }

    fn privacy_rule(&self, key: PrivacyKey) -> PrivacyRule {
        self.state
            .lock()
            .map(|st| st.settings_store.privacy_rule(key))
            .unwrap_or_default()
    }

    fn set_privacy_rule(&mut self, key: PrivacyKey, rule: PrivacyRule) {
        if let Ok(mut st) = self.state.lock() {
            st.settings_store.set_privacy_rule(key, rule);
        }
    }

    fn global_privacy(&self) -> GlobalPrivacySettings {
        self.state
            .lock()
            .map(|st| st.settings_store.global_privacy())
            .unwrap_or_default()
    }

    fn set_global_privacy(&mut self, settings: GlobalPrivacySettings) {
        if let Ok(mut st) = self.state.lock() {
            st.settings_store.set_global_privacy(settings);
        }
    }

    fn account_ttl_days(&self) -> i32 {
        self.state
            .lock()
            .map_or(180, |st| st.settings_store.account_ttl_days())
    }

    fn set_account_ttl_days(&mut self, days: i32) {
        if let Ok(mut st) = self.state.lock() {
            st.settings_store.set_account_ttl_days(days);
        }
    }

    fn clear_payment_info(&mut self, clear_shipping: bool, clear_credentials: bool) {
        if let Ok(mut st) = self.state.lock() {
            st.settings_store
                .clear_payment_info(clear_shipping, clear_credentials);
        }
    }

    fn active_sessions(&self) -> Vec<ActiveSession> {
        self.state
            .lock()
            .map(|st| st.settings_store.active_sessions())
            .unwrap_or_default()
    }

    fn terminate_session(&mut self, hash: u64) -> bool {
        self.state
            .lock()
            .is_ok_and(|mut st| st.settings_store.terminate_session(hash))
    }

    fn terminate_all_other_sessions(&mut self) -> usize {
        self.state
            .lock()
            .map_or(0, |mut st| st.settings_store.terminate_all_other_sessions())
    }

    fn update_session_permissions(
        &mut self,
        hash: u64,
        accept_calls: bool,
        accept_secret_chats: bool,
    ) {
        if let Ok(mut st) = self.state.lock() {
            st.settings_store
                .update_session_permissions(hash, accept_calls, accept_secret_chats);
        }
    }

    fn session_ttl_days(&self) -> i32 {
        self.state
            .lock()
            .map_or(180, |st| st.settings_store.session_ttl_days())
    }

    fn set_session_ttl_days(&mut self, days: i32) {
        if let Ok(mut st) = self.state.lock() {
            st.settings_store.set_session_ttl_days(days);
        }
    }

    fn blocked_users(&self) -> Vec<PeerRef> {
        self.state
            .lock()
            .map(|st| {
                st.peers
                    .iter()
                    .filter(|p| p.is_blocked)
                    .map(PeerRecord::to_peer_ref)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn block_peer(&mut self, peer_id: i64) {
        if let Ok(mut st) = self.state.lock()
            && let Some(p) = st.peer_mut(PeerId(peer_id))
        {
            p.is_blocked = true;
        }
    }

    fn unblock_peer(&mut self, peer_id: i64) {
        if let Ok(mut st) = self.state.lock()
            && let Some(p) = st.peer_mut(PeerId(peer_id))
        {
            p.is_blocked = false;
        }
    }

    fn cloud_password_state(&self) -> CloudPasswordState {
        self.state
            .lock()
            .map(|st| st.settings_store.cloud_password_state())
            .unwrap_or_default()
    }

    fn verify_cloud_password(&self, password: &str) -> Result<(), CloudPasswordError> {
        let st = self
            .state
            .lock()
            .map_err(|_| CloudPasswordError::WrongPassword)?;
        st.settings_store.verify_cloud_password(password)
    }

    fn set_cloud_password(
        &mut self,
        current_password: Option<&str>,
        new_password: &str,
        hint: &str,
        recovery_email: Option<&str>,
    ) -> Result<CloudPasswordState, CloudPasswordError> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| CloudPasswordError::WrongPassword)?;
        st.settings_store
            .set_cloud_password(current_password, new_password, hint, recovery_email)
    }

    fn confirm_recovery_email(
        &mut self,
        code: &str,
    ) -> Result<CloudPasswordState, CloudPasswordError> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| CloudPasswordError::InvalidCode)?;
        st.settings_store.confirm_recovery_email(code)
    }

    fn cancel_recovery_email(&mut self) -> CloudPasswordState {
        self.state
            .lock()
            .map(|mut st| st.settings_store.cancel_recovery_email())
            .unwrap_or_default()
    }

    fn disable_cloud_password(
        &mut self,
        current_password: &str,
    ) -> Result<CloudPasswordState, CloudPasswordError> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| CloudPasswordError::WrongPassword)?;
        st.settings_store.disable_cloud_password(current_password)
    }

    fn scope_notifications(&self, scope: NotifyScope) -> ScopeNotifySettings {
        self.state
            .lock()
            .map(|st| st.settings_store.scope_notifications(scope))
            .unwrap_or_default()
    }

    fn set_scope_notifications(&mut self, scope: NotifyScope, settings: ScopeNotifySettings) {
        if let Ok(mut st) = self.state.lock() {
            st.settings_store.set_scope_notifications(scope, settings);
        }
    }

    fn notification_events(&self) -> NotificationEventsSettings {
        self.state
            .lock()
            .map(|st| st.settings_store.notification_events())
            .unwrap_or_default()
    }

    fn set_notification_events(&mut self, events: NotificationEventsSettings) {
        if let Ok(mut st) = self.state.lock() {
            st.settings_store.set_notification_events(events);
        }
    }

    fn reset_all_notifications(&mut self) {
        if let Ok(mut st) = self.state.lock() {
            st.settings_store.reset_all_notifications();
            for p in &mut st.peers {
                p.muted = false;
            }
        }
    }

    fn auto_download(&self) -> AutoDownloadSettings {
        self.state
            .lock()
            .map(|st| st.settings_store.auto_download())
            .unwrap_or_default()
    }

    fn set_auto_download(&mut self, settings: AutoDownloadSettings) {
        if let Ok(mut st) = self.state.lock() {
            st.settings_store.set_auto_download(settings);
        }
    }

    fn sticker_sets(&self) -> Vec<StickerSetInfo> {
        self.state
            .lock()
            .map(|st| st.settings_store.sticker_sets())
            .unwrap_or_default()
    }

    fn set_sticker_set_archived(&mut self, id: u64, archived: bool) {
        if let Ok(mut st) = self.state.lock() {
            st.settings_store.set_sticker_set_archived(id, archived);
        }
    }

    fn remove_sticker_set(&mut self, id: u64) {
        if let Ok(mut st) = self.state.lock() {
            st.settings_store.remove_sticker_set(id);
        }
    }

    fn folders(&self) -> Vec<SettingsChatFolder> {
        self.state
            .lock()
            .map(|st| st.folders.clone())
            .unwrap_or_default()
    }

    fn suggested_folders(&self) -> Vec<SuggestedFolder> {
        self.state
            .lock()
            .map(|st| st.settings_store.suggested_folders())
            .unwrap_or_default()
    }

    fn save_folder(
        &mut self,
        mut folder: SettingsChatFolder,
    ) -> Result<SettingsChatFolder, String> {
        if !folder.is_valid() {
            return Err(
                "Folder requires a title (<= 12 chars) and at least one chat rule.".to_owned(),
            );
        }
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        if folder.id == 0 {
            let max_id = st.folders.iter().map(|f| f.id).max().unwrap_or(5);
            folder.id = max_id + 1;
            st.folders.push(folder.clone());
        } else if let Some(existing) = st.folders.iter_mut().find(|f| f.id == folder.id) {
            *existing = folder.clone();
        } else {
            st.folders.push(folder.clone());
        }
        Ok(folder)
    }

    fn delete_folder(&mut self, id: i32) -> bool {
        let Ok(mut st) = self.state.lock() else {
            return false;
        };
        let before = st.folders.len();
        st.folders.retain(|f| f.id != id);
        st.folders.len() < before
    }

    fn reorder_folders(&mut self, ids: &[i32]) {
        let filter_ids: Vec<FilterId> = ids.iter().copied().map(FilterId).collect();
        DialogsBackend::reorder_folders(self, &filter_ids);
    }

    fn accept_suggested_folder(&mut self, index: usize) -> Option<SettingsChatFolder> {
        let mut st = self.state.lock().ok()?;
        let mut folder = st.settings_store.accept_suggested_folder(index)?;
        let max_id = st.folders.iter().map(|f| f.id).max().unwrap_or(5);
        folder.id = max_id + 1;
        st.folders.push(folder.clone());
        Some(folder)
    }

    fn export_folder_invite_link(
        &mut self,
        folder_id: i32,
        title: &str,
        peers: &[i64],
    ) -> Result<String, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state unavailable".to_owned())?;
        st.settings_store
            .export_folder_invite_link(folder_id, title, peers)
    }

    fn languages(&self) -> Vec<LangPackLanguageInfo> {
        self.state
            .lock()
            .map(|st| st.settings_store.languages())
            .unwrap_or_default()
    }

    fn active_language_code(&self) -> String {
        self.state
            .lock()
            .map(|st| st.settings_store.active_language_code())
            .unwrap_or_else(|_| tg_lang::default_language_id().to_owned())
    }

    fn recent_languages(&self) -> Vec<tg_lang::Language> {
        self.state
            .lock()
            .map(|st| st.settings_store.recent_languages())
            .unwrap_or_default()
    }

    fn apply_language(&mut self, lang_code: &str) -> Result<String, String> {
        self.state
            .lock()
            .map_err(|_| "Session state poisoned".to_owned())?
            .settings_store
            .apply_language(lang_code)
    }

    fn open_set_language_link(
        &mut self,
        url: &str,
    ) -> Result<Option<tg_app_settings::settings::backend::LanguagePrompt>, String> {
        self.state
            .lock()
            .map_err(|_| "Session state poisoned".to_owned())?
            .settings_store
            .open_set_language_link(url)
    }

    fn confirm_language_prompt(&mut self) -> Result<Option<String>, String> {
        self.state
            .lock()
            .map_err(|_| "Session state poisoned".to_owned())?
            .settings_store
            .confirm_language_prompt()
    }

    fn dismiss_language_prompt(&mut self) {
        if let Ok(mut st) = self.state.lock() {
            st.settings_store.dismiss_language_prompt();
        }
    }

    fn pending_language_prompt(
        &self,
    ) -> Option<tg_app_settings::settings::backend::LanguagePrompt> {
        self.state
            .lock()
            .ok()
            .and_then(|st| st.settings_store.pending_language_prompt())
    }

    fn remove_recent_language(&mut self, lang_code: &str) {
        if let Ok(mut st) = self.state.lock() {
            st.settings_store.remove_recent_language(lang_code);
        }
    }

    fn local_storage_stats(&self) -> LocalStorageStats {
        self.state
            .lock()
            .map(|st| st.settings_store.local_storage_stats())
            .unwrap_or_else(|_| MockSettingsBackend::new().local_storage_stats())
    }

    fn clear_local_storage(&mut self) -> i64 {
        self.state
            .lock()
            .map_or(0, |mut st| st.settings_store.clear_local_storage())
    }

    fn set_local_storage_limits(&mut self, size_limit_bytes: i64, time_limit_days: i32) {
        if let Ok(mut st) = self.state.lock() {
            st.settings_store
                .set_local_storage_limits(size_limit_bytes, time_limit_days);
        }
    }

    fn request_export(&mut self, request: &ExportRequest) -> String {
        self.state
            .lock()
            .map(|mut st| st.settings_store.request_export(request))
            .unwrap_or_else(|_| "Export scheduled".to_owned())
    }

    fn proxy_settings(&self) -> Option<tg_core::SettingsProxy> {
        self.state
            .lock()
            .ok()
            .and_then(|st| st.settings_store.proxy_settings())
    }

    fn apply_proxy_settings(&mut self, settings: &tg_core::SettingsProxy) -> Result<(), String> {
        if let Ok(mut st) = self.state.lock() {
            st.proxy_enabled = settings.is_enabled();
            st.settings_store.apply_proxy_settings(settings)
        } else {
            Ok(())
        }
    }

    fn check_proxy(&mut self, proxy: &tg_core::ProxyData) {
        if let Ok(mut st) = self.state.lock() {
            st.settings_store.check_proxy(proxy);
        }
    }

    fn proxy_check_status(&self, proxy: &tg_core::ProxyData) -> ProxyCheckStatus {
        self.state
            .lock()
            .map(|st| st.settings_store.proxy_check_status(proxy))
            .unwrap_or_default()
    }
    fn web_authorizations(&self) -> Vec<WebAuthorization> {
        self.state
            .lock()
            .map(|st| st.settings_store.web_authorizations())
            .unwrap_or_default()
    }

    fn disconnect_website(&mut self, hash: u64, block_bot: bool) -> Result<bool, String> {
        self.state
            .lock()
            .map_err(|_| "Session state lock poisoned.".to_owned())?
            .settings_store
            .disconnect_website(hash, block_bot)
    }

    fn disconnect_all_websites(&mut self) -> Result<usize, String> {
        self.state
            .lock()
            .map_err(|_| "Session state lock poisoned.".to_owned())?
            .settings_store
            .disconnect_all_websites()
    }

    fn saved_ringtones(&self) -> Vec<SavedRingtone> {
        self.state
            .lock()
            .map(|st| st.settings_store.saved_ringtones())
            .unwrap_or_default()
    }

    fn save_ringtone(&mut self, id: i64, unsave: bool) -> Result<(), String> {
        self.state
            .lock()
            .map_err(|_| "Session state lock poisoned.".to_owned())?
            .settings_store
            .save_ringtone(id, unsave)
    }

    fn upload_ringtone(
        &mut self,
        file_name: &str,
        mime_type: &str,
        content: &[u8],
    ) -> Result<SavedRingtone, String> {
        self.state
            .lock()
            .map_err(|_| "Session state lock poisoned.".to_owned())?
            .settings_store
            .upload_ringtone(file_name, mime_type, content)
    }

    fn passkeys(&self) -> Vec<tg_app_settings::settings::backend::PasskeyEntry> {
        self.state
            .lock()
            .map(|st| st.settings_store.passkeys())
            .unwrap_or_default()
    }

    fn is_passkey_supported(&self) -> bool {
        self.state
            .lock()
            .map(|st| st.settings_store.is_passkey_supported())
            .unwrap_or(false)
    }

    fn can_register_passkey(&self) -> bool {
        self.state
            .lock()
            .map(|st| st.settings_store.can_register_passkey())
            .unwrap_or(false)
    }

    fn register_passkey(
        &mut self,
    ) -> Result<tg_app_settings::settings::backend::PasskeyEntry, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.settings_store.register_passkey()
    }

    fn start_passkey_hybrid_qr(&mut self) -> Result<String, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.settings_store.start_passkey_hybrid_qr()
    }

    fn passkey_hybrid_qr_status(
        &self,
    ) -> tg_app_settings::settings::backend::PasskeyHybridQrStatus {
        self.state
            .lock()
            .map(|st| st.settings_store.passkey_hybrid_qr_status())
            .unwrap_or_default()
    }

    fn cancel_passkey_hybrid_qr(&mut self) {
        if let Ok(mut st) = self.state.lock() {
            st.settings_store.cancel_passkey_hybrid_qr();
        }
    }

    fn delete_passkey(&mut self, id: &str) -> Result<(), String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.settings_store.delete_passkey(id)
    }

    fn fetch_faq_suggestions(
        &self,
        faq_url: &str,
    ) -> Result<Vec<tg_app_core::settings::backend::FaqEntry>, String> {
        let st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.settings_store.fetch_faq_suggestions(faq_url)
    }
}

impl SettingsBackend for MockSession {
    fn profile(&self) -> ProfileInfo {
        self.handle.profile()
    }
    fn update_name(&mut self, first_name: &str, last_name: &str) -> Result<(), String> {
        let r = self.handle.update_name(first_name, last_name);
        self.sync_cached_user();
        r
    }
    fn update_bio(&mut self, bio: &str) -> Result<(), String> {
        let r = self.handle.update_bio(bio);
        self.sync_cached_user();
        r
    }
    fn check_username(&self, username: &str) -> UsernameStatus {
        SettingsBackend::check_username(&self.handle, username)
    }
    fn update_username(&mut self, username: &str) -> Result<(), String> {
        let r = SettingsBackend::update_username(&mut self.handle, username);
        self.sync_cached_user();
        r
    }
    fn update_birthday(&mut self, birthday: Option<SettingsBirthday>) -> Result<(), String> {
        self.handle.update_birthday(birthday)
    }
    fn update_profile_photo(&mut self, color_index: u8, photo_label: Option<String>) {
        self.handle.update_profile_photo(color_index, photo_label);
        self.sync_cached_user();
    }
    fn set_phone_hidden(&mut self, hidden: bool) {
        self.handle.set_phone_hidden(hidden);
    }
    fn contacts(&self) -> Vec<PeerRef> {
        SettingsBackend::contacts(&self.handle)
    }
    fn privacy_rules(&self) -> BTreeMap<PrivacyKey, PrivacyRule> {
        self.handle.privacy_rules()
    }
    fn privacy_rule(&self, key: PrivacyKey) -> PrivacyRule {
        self.handle.privacy_rule(key)
    }
    fn set_privacy_rule(&mut self, key: PrivacyKey, rule: PrivacyRule) {
        self.handle.set_privacy_rule(key, rule);
    }
    fn global_privacy(&self) -> GlobalPrivacySettings {
        self.handle.global_privacy()
    }
    fn set_global_privacy(&mut self, settings: GlobalPrivacySettings) {
        self.handle.set_global_privacy(settings);
    }
    fn account_ttl_days(&self) -> i32 {
        self.handle.account_ttl_days()
    }
    fn set_account_ttl_days(&mut self, days: i32) {
        self.handle.set_account_ttl_days(days);
    }
    fn clear_payment_info(&mut self, clear_shipping: bool, clear_credentials: bool) {
        self.handle
            .clear_payment_info(clear_shipping, clear_credentials);
    }
    fn active_sessions(&self) -> Vec<ActiveSession> {
        self.handle.active_sessions()
    }
    fn terminate_session(&mut self, hash: u64) -> bool {
        self.handle.terminate_session(hash)
    }
    fn terminate_all_other_sessions(&mut self) -> usize {
        self.handle.terminate_all_other_sessions()
    }
    fn update_session_permissions(
        &mut self,
        hash: u64,
        accept_calls: bool,
        accept_secret_chats: bool,
    ) {
        self.handle
            .update_session_permissions(hash, accept_calls, accept_secret_chats);
    }
    fn session_ttl_days(&self) -> i32 {
        self.handle.session_ttl_days()
    }
    fn set_session_ttl_days(&mut self, days: i32) {
        self.handle.set_session_ttl_days(days);
    }
    fn blocked_users(&self) -> Vec<PeerRef> {
        self.handle.blocked_users()
    }
    fn block_peer(&mut self, peer_id: i64) {
        self.handle.block_peer(peer_id);
    }
    fn unblock_peer(&mut self, peer_id: i64) {
        self.handle.unblock_peer(peer_id);
    }
    fn cloud_password_state(&self) -> CloudPasswordState {
        self.handle.cloud_password_state()
    }
    fn verify_cloud_password(&self, password: &str) -> Result<(), CloudPasswordError> {
        SettingsBackend::verify_cloud_password(&self.handle, password)
    }
    fn set_cloud_password(
        &mut self,
        current_password: Option<&str>,
        new_password: &str,
        hint: &str,
        recovery_email: Option<&str>,
    ) -> Result<CloudPasswordState, CloudPasswordError> {
        self.handle
            .set_cloud_password(current_password, new_password, hint, recovery_email)
    }
    fn confirm_recovery_email(
        &mut self,
        code: &str,
    ) -> Result<CloudPasswordState, CloudPasswordError> {
        self.handle.confirm_recovery_email(code)
    }
    fn cancel_recovery_email(&mut self) -> CloudPasswordState {
        self.handle.cancel_recovery_email()
    }
    fn disable_cloud_password(
        &mut self,
        current_password: &str,
    ) -> Result<CloudPasswordState, CloudPasswordError> {
        self.handle.disable_cloud_password(current_password)
    }
    fn scope_notifications(&self, scope: NotifyScope) -> ScopeNotifySettings {
        self.handle.scope_notifications(scope)
    }
    fn set_scope_notifications(&mut self, scope: NotifyScope, settings: ScopeNotifySettings) {
        self.handle.set_scope_notifications(scope, settings);
    }
    fn notification_events(&self) -> NotificationEventsSettings {
        self.handle.notification_events()
    }
    fn set_notification_events(&mut self, events: NotificationEventsSettings) {
        self.handle.set_notification_events(events);
    }
    fn reset_all_notifications(&mut self) {
        self.handle.reset_all_notifications();
    }
    fn auto_download(&self) -> AutoDownloadSettings {
        self.handle.auto_download()
    }
    fn set_auto_download(&mut self, settings: AutoDownloadSettings) {
        self.handle.set_auto_download(settings);
    }
    fn sticker_sets(&self) -> Vec<StickerSetInfo> {
        self.handle.sticker_sets()
    }
    fn set_sticker_set_archived(&mut self, id: u64, archived: bool) {
        self.handle.set_sticker_set_archived(id, archived);
    }
    fn remove_sticker_set(&mut self, id: u64) {
        self.handle.remove_sticker_set(id);
    }
    fn folders(&self) -> Vec<SettingsChatFolder> {
        SettingsBackend::folders(&self.handle)
    }
    fn suggested_folders(&self) -> Vec<SuggestedFolder> {
        self.handle.suggested_folders()
    }
    fn save_folder(&mut self, folder: SettingsChatFolder) -> Result<SettingsChatFolder, String> {
        self.handle.save_folder(folder)
    }
    fn delete_folder(&mut self, id: i32) -> bool {
        self.handle.delete_folder(id)
    }
    fn reorder_folders(&mut self, ids: &[i32]) {
        SettingsBackend::reorder_folders(&mut self.handle, ids);
    }
    fn accept_suggested_folder(&mut self, index: usize) -> Option<SettingsChatFolder> {
        self.handle.accept_suggested_folder(index)
    }
    fn export_folder_invite_link(
        &mut self,
        folder_id: i32,
        title: &str,
        peers: &[i64],
    ) -> Result<String, String> {
        SettingsBackend::export_folder_invite_link(&mut self.handle, folder_id, title, peers)
    }
    fn languages(&self) -> Vec<LangPackLanguageInfo> {
        SettingsBackend::languages(&self.handle)
    }
    fn active_language_code(&self) -> String {
        SettingsBackend::active_language_code(&self.handle)
    }
    fn recent_languages(&self) -> Vec<tg_lang::Language> {
        SettingsBackend::recent_languages(&self.handle)
    }
    fn apply_language(&mut self, lang_code: &str) -> Result<String, String> {
        SettingsBackend::apply_language(&mut self.handle, lang_code)
    }
    fn open_set_language_link(
        &mut self,
        url: &str,
    ) -> Result<Option<tg_app_settings::settings::backend::LanguagePrompt>, String> {
        SettingsBackend::open_set_language_link(&mut self.handle, url)
    }
    fn confirm_language_prompt(&mut self) -> Result<Option<String>, String> {
        SettingsBackend::confirm_language_prompt(&mut self.handle)
    }
    fn dismiss_language_prompt(&mut self) {
        SettingsBackend::dismiss_language_prompt(&mut self.handle);
    }
    fn pending_language_prompt(
        &self,
    ) -> Option<tg_app_settings::settings::backend::LanguagePrompt> {
        SettingsBackend::pending_language_prompt(&self.handle)
    }
    fn remove_recent_language(&mut self, lang_code: &str) {
        SettingsBackend::remove_recent_language(&mut self.handle, lang_code);
    }
    fn local_storage_stats(&self) -> LocalStorageStats {
        self.handle.local_storage_stats()
    }
    fn clear_local_storage(&mut self) -> i64 {
        self.handle.clear_local_storage()
    }
    fn set_local_storage_limits(&mut self, size_limit_bytes: i64, time_limit_days: i32) {
        self.handle
            .set_local_storage_limits(size_limit_bytes, time_limit_days);
    }
    fn request_export(&mut self, request: &ExportRequest) -> String {
        self.handle.request_export(request)
    }
    fn proxy_settings(&self) -> Option<tg_core::SettingsProxy> {
        self.handle.proxy_settings()
    }
    fn apply_proxy_settings(&mut self, settings: &tg_core::SettingsProxy) -> Result<(), String> {
        self.handle.apply_proxy_settings(settings)
    }
    fn check_proxy(&mut self, proxy: &tg_core::ProxyData) {
        self.handle.check_proxy(proxy);
    }
    fn proxy_check_status(&self, proxy: &tg_core::ProxyData) -> ProxyCheckStatus {
        self.handle.proxy_check_status(proxy)
    }
    fn web_authorizations(&self) -> Vec<WebAuthorization> {
        self.handle.web_authorizations()
    }
    fn disconnect_website(&mut self, hash: u64, block_bot: bool) -> Result<bool, String> {
        self.handle.disconnect_website(hash, block_bot)
    }
    fn disconnect_all_websites(&mut self) -> Result<usize, String> {
        self.handle.disconnect_all_websites()
    }
    fn saved_ringtones(&self) -> Vec<SavedRingtone> {
        self.handle.saved_ringtones()
    }
    fn save_ringtone(&mut self, id: i64, unsave: bool) -> Result<(), String> {
        self.handle.save_ringtone(id, unsave)
    }
    fn upload_ringtone(
        &mut self,
        file_name: &str,
        mime_type: &str,
        content: &[u8],
    ) -> Result<SavedRingtone, String> {
        self.handle.upload_ringtone(file_name, mime_type, content)
    }
    fn passkeys(&self) -> Vec<tg_app_settings::settings::backend::PasskeyEntry> {
        self.handle.passkeys()
    }
    fn is_passkey_supported(&self) -> bool {
        self.handle.is_passkey_supported()
    }
    fn can_register_passkey(&self) -> bool {
        self.handle.can_register_passkey()
    }
    fn register_passkey(
        &mut self,
    ) -> Result<tg_app_settings::settings::backend::PasskeyEntry, String> {
        self.handle.register_passkey()
    }
    fn start_passkey_hybrid_qr(&mut self) -> Result<String, String> {
        self.handle.start_passkey_hybrid_qr()
    }
    fn passkey_hybrid_qr_status(
        &self,
    ) -> tg_app_settings::settings::backend::PasskeyHybridQrStatus {
        self.handle.passkey_hybrid_qr_status()
    }
    fn cancel_passkey_hybrid_qr(&mut self) {
        self.handle.cancel_passkey_hybrid_qr();
    }
    fn delete_passkey(&mut self, id: &str) -> Result<(), String> {
        self.handle.delete_passkey(id)
    }
    fn fetch_faq_suggestions(
        &self,
        faq_url: &str,
    ) -> Result<Vec<tg_app_core::settings::backend::FaqEntry>, String> {
        self.handle.fetch_faq_suggestions(faq_url)
    }
}

// =============================================================================
// 6. media::backend::MediaBackend
// =============================================================================

impl MediaBackend for SharedSessionHandle {
    fn get_item(&self, id: MediaItemId) -> Option<ViewerMediaItem> {
        if let Ok(st) = self.state.lock()
            && let Some(item) = st.media_items.iter().find(|it| it.id == id)
        {
            return Some(item.clone());
        }
        self.media_base.get_item(id)
    }

    fn siblings_in_chat(&self, peer_id: i64, item_id: MediaItemId) -> Vec<ViewerMediaItem> {
        if let Ok(st) = self.state.lock() {
            let matching: Vec<ViewerMediaItem> = st
                .media_items
                .iter()
                .filter(|it| it.peer_id == peer_id)
                .cloned()
                .collect();
            if !matching.is_empty() && matching.iter().any(|it| it.id == item_id) {
                return matching;
            }
            if !st.media_items.is_empty() {
                return st.media_items.clone();
            }
        }
        self.media_base.siblings_in_chat(peer_id, item_id)
    }

    fn load_bytes(&self, id: MediaItemId) -> anyhow::Result<Vec<u8>> {
        self.media_base.load_bytes(id)
    }

    fn open_range_loader(&self, id: MediaItemId) -> anyhow::Result<Arc<dyn RangeLoader>> {
        self.media_base.open_range_loader(id)
    }

    fn save_to_disk(&self, id: MediaItemId, destination: &Path) -> anyhow::Result<PathBuf> {
        self.media_base.save_to_disk(id, destination)
    }

    fn copy_item(&self, id: MediaItemId) -> anyhow::Result<()> {
        self.media_base.copy_item(id)
    }

    fn forward_item(&self, id: MediaItemId) -> anyhow::Result<()> {
        self.media_base.forward_item(id)
    }

    fn delete_item(&self, id: MediaItemId) -> anyhow::Result<()> {
        if let Ok(mut st) = self.state.lock() {
            st.media_items.retain(|it| it.id != id);
        }
        self.media_base.delete_item(id)
    }

    fn show_in_chat(&self, peer_id: i64, msg_id: i32) {
        if let Ok(mut st) = self.state.lock() {
            st.pending_chat_opens.push(PendingChatOpen {
                peer_id: PeerId(peer_id),
                msg_id: MsgId(msg_id),
                draft: None,
            });
        }
    }

    fn open_in_external_app(&self, id: MediaItemId) -> anyhow::Result<PathBuf> {
        self.media_base.open_in_external_app(id)
    }

    fn audio_track_metadata(&self, id: AudioMsgId) -> Option<AudioTrackMetadata> {
        self.media_base.audio_track_metadata(id)
    }

    fn audio_data_source(&self) -> Arc<dyn AudioDataSource> {
        self.media_base.audio_data_source()
    }

    fn playlist_provider(&self) -> Arc<dyn PlaylistProvider> {
        self.media_base.playlist_provider()
    }
}

impl MediaBackend for MockSession {
    fn get_item(&self, id: MediaItemId) -> Option<ViewerMediaItem> {
        self.handle.get_item(id)
    }
    fn siblings_in_chat(&self, peer_id: i64, item_id: MediaItemId) -> Vec<ViewerMediaItem> {
        self.handle.siblings_in_chat(peer_id, item_id)
    }
    fn load_bytes(&self, id: MediaItemId) -> anyhow::Result<Vec<u8>> {
        self.handle.load_bytes(id)
    }
    fn open_range_loader(&self, id: MediaItemId) -> anyhow::Result<Arc<dyn RangeLoader>> {
        self.handle.open_range_loader(id)
    }
    fn save_to_disk(&self, id: MediaItemId, destination: &Path) -> anyhow::Result<PathBuf> {
        self.handle.save_to_disk(id, destination)
    }
    fn copy_item(&self, id: MediaItemId) -> anyhow::Result<()> {
        self.handle.copy_item(id)
    }
    fn forward_item(&self, id: MediaItemId) -> anyhow::Result<()> {
        self.handle.forward_item(id)
    }
    fn delete_item(&self, id: MediaItemId) -> anyhow::Result<()> {
        self.handle.delete_item(id)
    }
    fn show_in_chat(&self, peer_id: i64, msg_id: i32) {
        self.handle.show_in_chat(peer_id, msg_id);
    }
    fn open_in_external_app(&self, id: MediaItemId) -> anyhow::Result<PathBuf> {
        self.handle.open_in_external_app(id)
    }
    fn audio_track_metadata(&self, id: AudioMsgId) -> Option<AudioTrackMetadata> {
        self.handle.audio_track_metadata(id)
    }
    fn audio_data_source(&self) -> Arc<dyn AudioDataSource> {
        self.handle.audio_data_source()
    }
    fn playlist_provider(&self) -> Arc<dyn PlaylistProvider> {
        self.handle.playlist_provider()
    }
}

// =============================================================================
// 7. chat_helpers::backend::ChatHelpersBackend
// =============================================================================

impl ChatHelpersBackend for MockSession {
    fn recent_emoji(&mut self) -> Vec<String> {
        self.chat_helpers.recent_emoji()
    }
    fn record_emoji_used(&mut self, emoji: &str) {
        self.chat_helpers.record_emoji_used(emoji);
    }
    fn hide_recent_emoji(&mut self, emoji: &str) {
        self.chat_helpers.hide_recent_emoji(emoji);
    }
    fn emoji_variants(&self) -> &EmojiVariants {
        self.chat_helpers.emoji_variants()
    }
    fn save_emoji_variant(&mut self, base_emoji: &str, tone: SkinTone) {
        self.chat_helpers.save_emoji_variant(base_emoji, tone);
    }
    fn save_all_emoji_variants(&mut self, tone: SkinTone) {
        self.chat_helpers.save_all_emoji_variants(tone);
    }
    fn search_emoji(&mut self, query: &str) -> Vec<EmojiKeywordResult> {
        self.chat_helpers.search_emoji(query)
    }
    fn is_premium(&self) -> bool {
        self.chat_helpers.is_premium()
    }
    fn set_premium(&mut self, premium: bool) {
        self.chat_helpers.set_premium(premium);
    }
    fn sticker_sections(&self) -> Vec<StickerPanelSection> {
        self.chat_helpers.sticker_sections()
    }
    fn featured_sticker_sections(&self) -> Vec<StickerPanelSection> {
        self.chat_helpers.featured_sticker_sections()
    }
    fn sticker_tab_icons(
        &self,
        sections: &[StickerPanelSection],
        icon_side: i32,
    ) -> Vec<StickerTabIcon> {
        self.chat_helpers.sticker_tab_icons(sections, icon_side)
    }
    fn sticker_document(&self, id: DocumentId) -> Option<&StickerDocument> {
        self.chat_helpers.sticker_document(id)
    }
    fn sticker_media(&self, id: DocumentId) -> Option<StickerMedia> {
        self.chat_helpers.sticker_media(id)
    }
    fn search_stickers(&self, query: &str) -> Vec<DocumentId> {
        self.chat_helpers.search_stickers(query)
    }
    fn record_sticker_used(&mut self, id: DocumentId) {
        self.chat_helpers.record_sticker_used(id);
    }
    fn is_sticker_faved(&self, id: DocumentId) -> bool {
        self.chat_helpers.is_sticker_faved(id)
    }
    fn toggle_faved_sticker(&mut self, id: DocumentId) -> bool {
        self.chat_helpers.toggle_faved_sticker(id)
    }
    fn install_sticker_set(&mut self, set_id: StickerSetId) -> bool {
        self.chat_helpers.install_sticker_set(set_id)
    }
    fn remove_sticker_set(&mut self, set_id: StickerSetId) -> bool {
        self.chat_helpers.remove_sticker_set(set_id)
    }
    fn is_set_installed(&self, set_id: StickerSetId) -> bool {
        self.chat_helpers.is_set_installed(set_id)
    }
    fn sticker_set_section(&self, set_id: StickerSetId) -> Option<StickerPanelSection> {
        self.chat_helpers.sticker_set_section(set_id)
    }
    fn saved_gifs(&self) -> Vec<GifItem> {
        self.chat_helpers.saved_gifs()
    }
    fn gif_categories(&self) -> Vec<GifCategory> {
        self.chat_helpers.gif_categories()
    }
    fn search_gifs(&self, query: &str) -> Vec<GifItem> {
        self.chat_helpers.search_gifs(query)
    }
    fn toggle_saved_gif(&mut self, id: DocumentId) -> bool {
        self.chat_helpers.toggle_saved_gif(id)
    }
    fn gif_frame_rgba(&self, id: DocumentId, frame_index: usize) -> Option<(u32, u32, Vec<u8>)> {
        self.chat_helpers.gif_frame_rgba(id, frame_index)
    }
    fn self_user_id(&self) -> UserId {
        UserId(session_peers::SELF.bare())
    }
    fn autocomplete_users(&self) -> &[UserInfo] {
        &self.autocomplete_users
    }
    fn autocomplete_last_authors(&self) -> &[UserId] {
        &self.autocomplete_last_authors
    }
    fn autocomplete_bot_commands(&self) -> &HashMap<UserId, Vec<BotCommand>> {
        &self.autocomplete_bot_commands
    }
    fn recent_inline_bot_ids(&self) -> &[UserId] {
        &self.recent_inline_bots
    }
    fn remove_recent_inline_bot(&mut self, id: UserId) -> bool {
        let before = self.recent_inline_bots.len();
        self.recent_inline_bots.retain(|&b| b != id);
        self.recent_inline_bots.len() < before
    }
    fn recent_hashtags(&self) -> &RecentHashtags {
        self.chat_helpers.recent_hashtags()
    }
    fn record_hashtag(&mut self, tag: &str) {
        self.chat_helpers.record_hashtag(tag);
        if let Ok(mut st) = self.handle.state.lock() {
            let clean = tag.trim().trim_start_matches('#').to_owned();
            if !clean.is_empty() && !st.recent_hashtags.iter().any(|t| t == &clean) {
                st.recent_hashtags.insert(0, clean);
            }
        }
    }
    fn remove_hashtag(&mut self, tag: &str) -> bool {
        self.handle.remove_recent_hashtag(tag);
        self.chat_helpers.remove_hashtag(tag)
    }
    fn search_emoji_suggestions(&mut self, keyword: &str, exact: bool) -> Vec<EmojiKeywordResult> {
        self.chat_helpers.search_emoji_suggestions(keyword, exact)
    }
    fn stickers_for_emoji(&self, emoji: &str) -> Vec<DocumentId> {
        self.chat_helpers.stickers_for_emoji(emoji)
    }
    fn get_attached_stickers(
        &self,
        media: &tg_app_core::chat_helpers::backend::AttachedStickersMedia,
    ) -> Result<Vec<tg_app_core::chat_helpers::backend::AttachedStickerSetSummary>, String> {
        use tg_app_core::chat_helpers::backend::{
            AttachedStickerSetSummary, AttachedStickersMedia,
        };
        let is_empty = matches!(
            media,
            AttachedStickersMedia::Photo { id: 0, .. }
                | AttachedStickersMedia::Document { id: 0, .. }
        );
        if is_empty {
            return Ok(Vec::new());
        }
        Ok(vec![
            AttachedStickerSetSummary {
                set_id: StickerSetId(9201),
                access_hash: 77,
                title: "Aurora Masks".to_owned(),
                short_name: "aurora_masks".to_owned(),
                count: 18,
                is_emoji: false,
                is_masks: true,
                cover_doc_ids: vec![DocumentId(501)],
            },
            AttachedStickerSetSummary {
                set_id: StickerSetId(9202),
                access_hash: 88,
                title: "Signal Reactions".to_owned(),
                short_name: "signal_reactions".to_owned(),
                count: 24,
                is_emoji: true,
                is_masks: false,
                cover_doc_ids: vec![DocumentId(502)],
            },
        ])
    }
    fn search_stickers_remote(
        &self,
        req: &tg_app_core::chat_helpers::backend::SearchStickersRequest,
    ) -> Result<tg_app_core::chat_helpers::backend::SearchStickersPage, String> {
        self.chat_helpers.search_stickers_remote(req)
    }
    fn search_emoji_sticker_sets(
        &self,
        query: &str,
        exclude_featured: bool,
    ) -> Result<tg_app_core::chat_helpers::backend::SearchStickerSetsResult, String> {
        self.chat_helpers
            .search_emoji_sticker_sets(query, exclude_featured)
    }
    fn load_old_featured_stickers(
        &self,
        offset: i32,
        limit: i32,
    ) -> Result<tg_app_core::chat_helpers::backend::OldFeaturedStickersPage, String> {
        self.chat_helpers.load_old_featured_stickers(offset, limit)
    }
    fn clear_recent_stickers(&mut self, attached: bool) -> Result<bool, String> {
        self.chat_helpers.clear_recent_stickers(attached)
    }
    fn paid_reaction_privacy(
        &self,
        force: bool,
    ) -> Result<tg_app_core::chat_helpers::backend::PaidReactionPrivacy, String> {
        self.chat_helpers.paid_reaction_privacy(force)
    }
    fn toggle_paid_reaction_privacy(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        privacy: tg_app_core::chat_helpers::backend::PaidReactionPrivacy,
    ) -> Result<bool, String> {
        self.chat_helpers
            .toggle_paid_reaction_privacy(peer_id, msg_id, privacy)
    }
    fn report_reaction(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        reaction_peer_id: PeerId,
    ) -> Result<bool, String> {
        self.chat_helpers
            .report_reaction(peer_id, msg_id, reaction_peer_id)
    }
    fn delete_participant_reaction(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        participant_id: PeerId,
    ) -> Result<(), String> {
        self.chat_helpers
            .delete_participant_reaction(peer_id, msg_id, participant_id)
    }
    fn delete_participant_reactions(
        &mut self,
        peer_id: PeerId,
        participant_id: PeerId,
    ) -> Result<bool, String> {
        self.chat_helpers
            .delete_participant_reactions(peer_id, participant_id)
    }
    fn default_emoji_catalog(
        &self,
        kind: tg_app_core::chat_helpers::backend::DefaultEmojiCatalogKind,
        force: bool,
    ) -> Result<tg_app_core::chat_helpers::backend::DefaultEmojiCatalog, String> {
        self.chat_helpers.default_emoji_catalog(kind, force)
    }
    fn default_tag_reactions(
        &self,
        force: bool,
    ) -> Result<tg_app_core::chat_helpers::backend::DefaultTagReactionsResult, String> {
        self.chat_helpers.default_tag_reactions(force)
    }
}

// =============================================================================
// 8. notifications::backend::NotifyBackend
// =============================================================================

impl NotifyBackend for SharedSessionHandle {
    fn poll_events(&mut self) -> Vec<IncomingNotifyEvent> {
        self.state
            .lock()
            .map(|mut st| st.notify_events.drain(..).collect())
            .unwrap_or_default()
    }

    fn mark_read(
        &mut self,
        _session_id: u64,
        peer_id: PeerId,
        _topic_root_id: MsgId,
        _max_msg_id: MsgId,
    ) {
        if let Ok(mut st) = self.state.lock() {
            st.mark_peer_read(peer_id);
        }
    }

    fn send_quick_reply(
        &mut self,
        _session_id: u64,
        peer_id: PeerId,
        _topic_root_id: MsgId,
        reply_to_msg_id: MsgId,
        text: String,
    ) {
        let _ = HistoryBackend::send_message(
            self,
            peer_id,
            SendMessageRequest {
                text: TextWithEntities::simple(text),
                reply_to: (reply_to_msg_id.0 > 0).then_some(reply_to_msg_id),
                silent: false,
            },
        );
    }

    fn open_chat(
        &mut self,
        _session_id: u64,
        peer_id: PeerId,
        _topic_root_id: MsgId,
        msg_id: MsgId,
        draft: Option<String>,
    ) {
        if let Ok(mut st) = self.state.lock() {
            if let Some(ref d) = draft
                && !d.is_empty()
            {
                st.drafts.insert(
                    peer_id,
                    DraftMessage {
                        text: d.clone(),
                        reply_to: None,
                    },
                );
            }
            st.pending_chat_opens.push(PendingChatOpen {
                peer_id,
                msg_id,
                draft,
            });
        }
    }

    fn unread_state(&self) -> UnreadBadgeState {
        let Ok(st) = self.state.lock() else {
            return UnreadBadgeState::default();
        };
        let mut badge = UnreadBadgeState::default();
        for p in &st.peers {
            if p.archived {
                continue;
            }
            if p.unread_count > 0 {
                badge.messages += p.unread_count;
                badge.chats += 1;
                if p.muted {
                    badge.messages_muted += p.unread_count;
                    badge.chats_muted += 1;
                }
            } else if p.unread_mark {
                badge.marks += 1;
                if p.muted {
                    badge.marks_muted += 1;
                }
            }
            badge.mentions += p.unread_mentions;
        }
        badge
    }

    fn peer_info(&self, peer_id: PeerId) -> Option<NotifyPeerInfo> {
        let st = self.state.lock().ok()?;
        let p = st.peer(peer_id)?;
        Some(NotifyPeerInfo {
            id: p.id,
            kind: p.coarse_kind(),
            title: p.title.clone(),
            initials: p.initials.clone(),
            avatar_rgb: p.avatar_rgb,
            is_self: p.kind == InfoPeerKind::SavedMessages,
            is_replies_chat: false,
            is_blocked: p.is_blocked,
            community_id: p.linked_discussion_peer,
        })
    }
}

impl NotifyBackend for MockSession {
    fn poll_events(&mut self) -> Vec<IncomingNotifyEvent> {
        self.handle.poll_events()
    }
    fn mark_read(
        &mut self,
        session_id: u64,
        peer_id: PeerId,
        topic_root_id: MsgId,
        max_msg_id: MsgId,
    ) {
        NotifyBackend::mark_read(
            &mut self.handle,
            session_id,
            peer_id,
            topic_root_id,
            max_msg_id,
        );
    }
    fn send_quick_reply(
        &mut self,
        session_id: u64,
        peer_id: PeerId,
        topic_root_id: MsgId,
        reply_to_msg_id: MsgId,
        text: String,
    ) {
        self.handle
            .send_quick_reply(session_id, peer_id, topic_root_id, reply_to_msg_id, text);
    }
    fn open_chat(
        &mut self,
        session_id: u64,
        peer_id: PeerId,
        topic_root_id: MsgId,
        msg_id: MsgId,
        draft: Option<String>,
    ) {
        self.handle
            .open_chat(session_id, peer_id, topic_root_id, msg_id, draft);
    }
    fn unread_state(&self) -> UnreadBadgeState {
        self.handle.unread_state()
    }
    fn peer_info(&self, peer_id: PeerId) -> Option<NotifyPeerInfo> {
        NotifyBackend::peer_info(&self.handle, peer_id)
    }
}

// =============================================================================
// 9. webview::backend::WebViewBackend
// =============================================================================

impl WebViewBackend for SharedSessionHandle {
    fn fetch_iv_page(
        &self,
        url: &str,
        webpage_id: Option<i64>,
    ) -> Result<Option<IvPageBundle>, WebViewBackendError> {
        let st = self
            .state
            .lock()
            .map_err(|_| WebViewBackendError::new("LOCK", "Session state lock poisoned"))?;
        st.webview.fetch_iv_page(url, webpage_id)
    }

    fn request_web_view(
        &mut self,
        mut request: WebViewRequest,
    ) -> Result<WebViewLaunchInfo, WebViewBackendError> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| WebViewBackendError::new("LOCK", "Session state lock poisoned"))?;
        if request.url_override.is_none() {
            request.url_override = st.pending_mini_app_urls.remove(&request.bot_id);
        } else {
            st.pending_mini_app_urls.remove(&request.bot_id);
        }
        st.webview_requests_log.push(request.clone());
        let mut launch = st.webview.request_web_view(request)?;
        if let Some(bot_peer) = st.peer(PeerId::from_user(launch.bot.id)) {
            launch.bot.title = bot_peer.title.clone();
            if let Some(u) = bot_peer.primary_username() {
                launch.bot.username = u.to_owned();
            }
            launch.bot.verified = bot_peer.verified;
        } else if let Some(attach_bot) = st
            .bot_attach_menu
            .find(tg_bots::stand_in::UserId(launch.bot.id))
        {
            launch.bot.title = attach_bot.short_name.clone();
            launch.bot.in_attach_menu = attach_bot.in_attach_menu;
            launch.bot.in_main_menu = attach_bot.in_side_menu;
        }
        if launch.url.contains("games.") || launch.url.contains("/game") {
            launch.bot.is_game = true;
        }
        Ok(launch)
    }

    fn send_web_app_data(
        &mut self,
        bot_id: u64,
        button_text: &str,
        data: &str,
    ) -> Result<(), WebViewBackendError> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| WebViewBackendError::new("LOCK", "Session state lock poisoned"))?;
        st.webview.send_web_app_data(bot_id, button_text, data)?;
        let peer_id = PeerId::from_user(bot_id);
        let _ = st.append_outgoing_message(
            peer_id,
            SendMessageRequest {
                text: TextWithEntities::simple(format!("[Mini App · {button_text}] {data}")),
                reply_to: None,
                silent: false,
            },
        );
        Ok(())
    }

    fn invoke_custom_method(
        &mut self,
        bot_id: u64,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| "Session state lock poisoned".to_owned())?;
        st.webview.invoke_custom_method(bot_id, method, params)
    }

    fn join_channel(&mut self, channel_id: i64) -> Result<(), WebViewBackendError> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| WebViewBackendError::new("LOCK", "Session state lock poisoned"))?;
        st.webview.join_channel(channel_id)
    }

    fn remove_bot_from_menu(&mut self, bot_id: u64) -> Result<(), WebViewBackendError> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| WebViewBackendError::new("LOCK", "Session state lock poisoned"))?;
        st.webview.remove_bot_from_menu(bot_id)
    }

    fn get_prepared_inline_message(
        &mut self,
        bot_id: u64,
        prepared_id: &str,
    ) -> Result<tg_app_media::webview::backend::PreparedInlineMessageInfo, WebViewBackendError>
    {
        let mut st = self
            .state
            .lock()
            .map_err(|_| WebViewBackendError::new("LOCK", "Session state lock poisoned"))?;
        st.webview.get_prepared_inline_message(bot_id, prepared_id)
    }

    fn send_prepared_inline_message(
        &mut self,
        bot_id: u64,
        peer_id: PeerId,
        query_id: i64,
        result_id: &str,
    ) -> Result<(), WebViewBackendError> {
        let mut st = self
            .state
            .lock()
            .map_err(|_| WebViewBackendError::new("LOCK", "Session state lock poisoned"))?;
        st.webview
            .send_prepared_inline_message(bot_id, peer_id, query_id, result_id)
    }

    fn prepared_message_recipients(
        &self,
        bot_id: u64,
        allowed: &[tg_webview::InlineChatType],
    ) -> Vec<(PeerId, String)> {
        self.state
            .lock()
            .map(|st| st.webview.prepared_message_recipients(bot_id, allowed))
            .unwrap_or_default()
    }

    fn bot_storage_dir(&self) -> Option<std::path::PathBuf> {
        self.state
            .lock()
            .ok()
            .and_then(|st| st.webview.bot_storage_dir())
    }
}

impl WebViewBackend for MockSession {
    fn fetch_iv_page(
        &self,
        url: &str,
        webpage_id: Option<i64>,
    ) -> Result<Option<IvPageBundle>, WebViewBackendError> {
        self.handle.fetch_iv_page(url, webpage_id)
    }
    fn request_web_view(
        &mut self,
        request: WebViewRequest,
    ) -> Result<WebViewLaunchInfo, WebViewBackendError> {
        self.handle.request_web_view(request)
    }
    fn send_web_app_data(
        &mut self,
        bot_id: u64,
        button_text: &str,
        data: &str,
    ) -> Result<(), WebViewBackendError> {
        self.handle.send_web_app_data(bot_id, button_text, data)
    }
    fn invoke_custom_method(
        &mut self,
        bot_id: u64,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        self.handle.invoke_custom_method(bot_id, method, params)
    }
    fn join_channel(&mut self, channel_id: i64) -> Result<(), WebViewBackendError> {
        self.handle.join_channel(channel_id)
    }
    fn remove_bot_from_menu(&mut self, bot_id: u64) -> Result<(), WebViewBackendError> {
        self.handle.remove_bot_from_menu(bot_id)
    }
    fn get_prepared_inline_message(
        &mut self,
        bot_id: u64,
        prepared_id: &str,
    ) -> Result<tg_app_media::webview::backend::PreparedInlineMessageInfo, WebViewBackendError>
    {
        self.handle.get_prepared_inline_message(bot_id, prepared_id)
    }
    fn send_prepared_inline_message(
        &mut self,
        bot_id: u64,
        peer_id: PeerId,
        query_id: i64,
        result_id: &str,
    ) -> Result<(), WebViewBackendError> {
        self.handle
            .send_prepared_inline_message(bot_id, peer_id, query_id, result_id)
    }
    fn prepared_message_recipients(
        &self,
        bot_id: u64,
        allowed: &[tg_webview::InlineChatType],
    ) -> Vec<(PeerId, String)> {
        self.handle.prepared_message_recipients(bot_id, allowed)
    }
    fn bot_storage_dir(&self) -> Option<std::path::PathBuf> {
        self.handle.bot_storage_dir()
    }
}

// =============================================================================
// 10. statistics::backend::StatsBackend
// =============================================================================

impl StatsBackend for SharedSessionHandle {
    fn channel_statistics(&self) -> Result<ChannelStatsBundle, String> {
        let mut bundle = MockStatsBackend::new().channel_statistics()?;
        if let Ok(st) = self.state.lock() {
            let target = st
                .peer(st.stats_target_peer)
                .filter(|p| p.kind.is_channel())
                .or_else(|| st.peer(session_peers::CHANNEL_DEV_LOG));
            if let Some(p) = target {
                bundle.peer = p.to_display_info();
                if let Some(msgs) = st.histories.get(&p.id)
                    && !msgs.is_empty()
                {
                    bundle.recent_posts = msgs
                        .iter()
                        .rev()
                        .map(|m| RecentPostPreview {
                            interaction: MessageInteractionInfo {
                                message_id: m.id.0,
                                story_id: 0,
                                views_count: m.views.unwrap_or(1_240) as i32,
                                forwards_count: 42,
                                reactions_count: m.reactions.iter().map(|r| r.count as i32).sum(),
                            },
                            is_story: false,
                            snippet: m.preview_snippet(),
                            date_unix: i64::from(m.date_unix),
                            author_title: p.title.clone(),
                            initials: p.initials.clone(),
                            avatar_rgb: p.avatar_rgb,
                        })
                        .collect();
                }
            }
        }
        Ok(bundle)
    }

    fn supergroup_statistics(&self) -> Result<SupergroupStatsBundle, String> {
        let mut bundle = MockStatsBackend::new().supergroup_statistics()?;
        if let Ok(st) = self.state.lock() {
            let target = st
                .peer(st.stats_target_peer)
                .filter(|p| p.kind.is_group())
                .or_else(|| st.peer(session_peers::GROUP_CORE));
            if let Some(p) = target {
                bundle.peer = p.to_display_info();
            }
        }
        Ok(bundle)
    }

    fn message_statistics(&self, message_id: i32) -> Result<MessageStatsBundle, String> {
        let mut bundle = MockStatsBackend::new().message_statistics(message_id)?;
        if let Ok(st) = self.state.lock()
            && let Some(p) = st.peer(session_peers::CHANNEL_DEV_LOG)
        {
            bundle.peer = p.to_display_info();
        }
        Ok(bundle)
    }

    fn story_statistics(&self, story_id: i32) -> Result<MessageStatsBundle, String> {
        MockStatsBackend::new().story_statistics(story_id)
    }

    fn load_zoom_graph(
        &self,
        zoom_token: &str,
        x_timestamp: f64,
    ) -> Result<StatisticalGraph, String> {
        MockStatsBackend::new().load_zoom_graph(zoom_token, x_timestamp)
    }

    fn boost_status(&self) -> Result<BoostStatusBundle, String> {
        let mut bundle = MockStatsBackend::new().boost_status()?;
        if let Ok(st) = self.state.lock()
            && let Some(p) = st
                .peer(st.stats_target_peer)
                .or_else(|| st.peer(session_peers::CHANNEL_DEV_LOG))
        {
            bundle.peer = p.to_display_info();
            if let Some(u) = p.primary_username() {
                bundle.status.link = format!("https://t.me/boost/{u}");
            }
        }
        Ok(bundle)
    }

    fn load_more_boosts(&self, token: &BoostsOffsetToken) -> Result<BoostsListSlice, String> {
        MockStatsBackend::new().load_more_boosts(token)
    }

    fn channel_earn(&self) -> Result<ChannelEarnBundle, String> {
        let mut bundle = MockStatsBackend::new().channel_earn()?;
        if let Ok(st) = self.state.lock()
            && let Some(p) = st.peer(session_peers::CHANNEL_DEV_LOG)
        {
            bundle.peer = p.to_display_info();
        }
        Ok(bundle)
    }

    fn load_more_channel_earn_history(&self, token: &str) -> Result<EarnHistorySlice, String> {
        MockStatsBackend::new().load_more_channel_earn_history(token)
    }

    fn bot_earn(&self) -> Result<BotEarnBundle, String> {
        let mut bundle = MockStatsBackend::new().bot_earn()?;
        if let Ok(st) = self.state.lock()
            && let Some(p) = st.peer(session_peers::BOT_CATALOG)
        {
            bundle.peer = p.to_display_info();
        }
        Ok(bundle)
    }
}

impl StatsBackend for MockSession {
    fn channel_statistics(&self) -> Result<ChannelStatsBundle, String> {
        self.handle.channel_statistics()
    }
    fn supergroup_statistics(&self) -> Result<SupergroupStatsBundle, String> {
        self.handle.supergroup_statistics()
    }
    fn message_statistics(&self, message_id: i32) -> Result<MessageStatsBundle, String> {
        self.handle.message_statistics(message_id)
    }
    fn story_statistics(&self, story_id: i32) -> Result<MessageStatsBundle, String> {
        self.handle.story_statistics(story_id)
    }
    fn load_zoom_graph(
        &self,
        zoom_token: &str,
        x_timestamp: f64,
    ) -> Result<StatisticalGraph, String> {
        self.handle.load_zoom_graph(zoom_token, x_timestamp)
    }
    fn boost_status(&self) -> Result<BoostStatusBundle, String> {
        self.handle.boost_status()
    }
    fn load_more_boosts(&self, token: &BoostsOffsetToken) -> Result<BoostsListSlice, String> {
        self.handle.load_more_boosts(token)
    }
    fn channel_earn(&self) -> Result<ChannelEarnBundle, String> {
        self.handle.channel_earn()
    }
    fn load_more_channel_earn_history(&self, token: &str) -> Result<EarnHistorySlice, String> {
        self.handle.load_more_channel_earn_history(token)
    }
    fn bot_earn(&self) -> Result<BotEarnBundle, String> {
        self.handle.bot_earn()
    }
}

// =============================================================================
// 11. calls::backend::CallsBackend
// =============================================================================

impl CallsBackend for MockSession {
    fn tick(&mut self, now_ms: u64) {
        self.calls.tick(now_ms);
    }
    fn active_call(&self) -> Option<&ActiveCallSnapshot> {
        self.calls.active_call()
    }
    fn start_outgoing_call(&mut self, peer_id: PeerId, video: bool, now_ms: u64) {
        self.calls.start_outgoing_call(peer_id, video, now_ms);
    }
    fn confirm_outgoing_call(&mut self, video: bool, now_ms: u64) {
        self.calls.confirm_outgoing_call(video, now_ms);
    }
    fn simulate_incoming_call(&mut self, peer_id: PeerId, video: bool, now_ms: u64) {
        self.calls.simulate_incoming_call(peer_id, video, now_ms);
    }
    fn answer_call(&mut self, now_ms: u64) {
        self.calls.answer_call(now_ms);
    }
    fn hangup_call(&mut self, now_ms: u64) {
        self.calls.hangup_call(now_ms);
    }
    fn redial_call(&mut self, now_ms: u64) {
        self.calls.redial_call(now_ms);
    }
    fn set_call_muted(&mut self, muted: bool) {
        self.calls.set_call_muted(muted);
    }
    fn toggle_call_camera(&mut self) {
        self.calls.toggle_call_camera();
    }
    fn toggle_call_screen_share(&mut self) {
        self.calls.toggle_call_screen_share();
    }
    fn submit_call_rating(&mut self, rating: u8, comment: String) {
        self.calls.submit_call_rating(rating, comment);
    }
    fn finish_call_rating(&mut self) {
        self.calls.finish_call_rating();
    }
    fn active_group_call(&self) -> Option<&GroupCallSnapshot> {
        self.calls.active_group_call()
    }
    fn join_group_call(&mut self, group_peer_id: PeerId, join_as: PeerId, now_ms: u64) {
        self.calls.join_group_call(group_peer_id, join_as, now_ms);
    }
    fn switch_group_join_as(&mut self, join_as: PeerId, now_ms: u64) {
        self.calls.switch_group_join_as(join_as, now_ms);
    }
    fn leave_group_call(&mut self, discard: bool, now_ms: u64) {
        self.calls.leave_group_call(discard, now_ms);
    }
    fn set_group_mute_state(&mut self, state: tg_calls::MuteState, now_ms: u64) {
        self.calls.set_group_mute_state(state, now_ms);
    }
    fn set_group_push_to_talk(&mut self, pressed: bool, now_ms: u64) {
        self.calls.set_group_push_to_talk(pressed, now_ms);
    }
    fn toggle_group_camera(&mut self, now_ms: u64) {
        self.calls.toggle_group_camera(now_ms);
    }
    fn toggle_group_screen_share(&mut self, now_ms: u64) {
        self.calls.toggle_group_screen_share(now_ms);
    }
    fn set_participant_volume(&mut self, peer_id: PeerId, volume: i32) {
        self.calls.set_participant_volume(peer_id, volume);
    }
    fn set_participant_muted(&mut self, peer_id: PeerId, muted: bool) {
        self.calls.set_participant_muted(peer_id, muted);
    }
    fn allow_participant_to_speak(&mut self, peer_id: PeerId) {
        self.calls.allow_participant_to_speak(peer_id);
    }
    fn select_large_video(&mut self, endpoint: Option<tg_calls::VideoEndpoint>, now_ms: u64) {
        self.calls.select_large_video(endpoint, now_ms);
    }
    fn toggle_pin_large_video(&mut self) {
        self.calls.toggle_pin_large_video();
    }
    fn set_group_join_muted(&mut self, join_muted: bool) {
        self.calls.set_group_join_muted(join_muted);
    }
    fn set_group_messages_enabled(&mut self, enabled: bool) {
        self.calls.set_group_messages_enabled(enabled);
    }
    fn set_noise_suppression(&mut self, enabled: bool) {
        self.calls.set_noise_suppression(enabled);
    }
    fn set_audio_devices(&mut self, input_id: String, output_id: String) {
        self.calls.set_audio_devices(input_id, output_id);
    }
    fn set_camera_device(&mut self, camera_id: String) {
        self.calls.set_camera_device(camera_id);
    }
    fn set_push_to_talk_settings(&mut self, enabled: bool, shortcut: String, delay_ms: u64) {
        self.calls
            .set_push_to_talk_settings(enabled, shortcut, delay_ms);
    }
    fn audio_settings(&self) -> &CallAudioSettings {
        self.calls.audio_settings()
    }
    fn input_devices(&self) -> &[AudioDeviceOption] {
        self.calls.input_devices()
    }
    fn output_devices(&self) -> &[AudioDeviceOption] {
        self.calls.output_devices()
    }
    fn camera_devices(&self) -> &[tg_voip::CameraDeviceInfo] {
        self.calls.camera_devices()
    }
    fn recent_calls(&self) -> &[RecentCallEntry] {
        self.calls.recent_calls()
    }
    fn delete_recent_calls(&mut self, msg_ids: &[i32]) {
        self.calls.delete_recent_calls(msg_ids);
    }
    fn active_chat_group_calls(&self) -> &[CallPeerInfo] {
        self.calls.active_chat_group_calls()
    }
    fn join_as_candidates(&self, group_peer_id: PeerId) -> Vec<JoinAsCandidate> {
        self.calls.join_as_candidates(group_peer_id)
    }
    fn drain_sound_events(&mut self) -> Vec<BackendSoundEvent> {
        self.calls.drain_sound_events()
    }
    fn drain_toasts(&mut self) -> Vec<String> {
        self.calls.drain_toasts()
    }
}

// =============================================================================
// 12. stories::backend::StoriesBackend
// =============================================================================

impl StoriesBackend for MockSession {
    fn self_peer_id(&self) -> tg_data::PeerId {
        self.stories.self_peer_id()
    }
    fn account_premium(&self) -> bool {
        self.stories.account_premium()
    }
    fn set_account_premium(&mut self, premium: bool) {
        self.stories.set_account_premium(premium);
    }
    fn peer_profile(&self, peer_id: tg_data::PeerId) -> StoryPeerProfile {
        self.stories.peer_profile(peer_id)
    }
    fn share_targets(&self) -> Vec<ShareTarget> {
        self.stories.share_targets()
    }
    fn execute_actions(
        &mut self,
        store: &mut tg_stories::StoriesStore,
        actions: Vec<tg_stories::StoriesAction>,
        now_secs: tg_data::TimeId,
    ) {
        self.stories.execute_actions(store, actions, now_secs);
    }
    fn load_story_media(&mut self, story_id: tg_data::FullStoryId) -> Option<StoryMediaAsset> {
        self.stories.load_story_media(story_id)
    }
    fn send_story_reply(
        &mut self,
        story_id: tg_data::FullStoryId,
        text: &str,
        now_secs: tg_data::TimeId,
    ) -> Result<(), String> {
        self.stories.send_story_reply(story_id, text, now_secs)
    }
    fn share_story(
        &mut self,
        story_id: tg_data::FullStoryId,
        targets: &[tg_data::PeerId],
        comment: &str,
    ) -> Result<(), String> {
        self.stories.share_story(story_id, targets, comment)
    }
    fn export_story_link(&self, story_id: tg_data::FullStoryId) -> Option<String> {
        self.stories.export_story_link(story_id)
    }
    fn report_story(&mut self, story_id: tg_data::FullStoryId, reason: &str) -> Result<(), String> {
        self.stories.report_story(story_id, reason)
    }
    fn set_viewer_stories_blocked(&mut self, peer_id: tg_data::PeerId, blocked: bool) {
        self.stories.set_viewer_stories_blocked(peer_id, blocked);
    }
    fn is_viewer_stories_blocked(&self, peer_id: tg_data::PeerId) -> bool {
        self.stories.is_viewer_stories_blocked(peer_id)
    }
    fn pending_uploads(&self) -> Vec<tg_stories::StoryUploadStatus> {
        self.stories.pending_uploads()
    }
    fn send_story(
        &mut self,
        store: Option<&mut tg_stories::StoriesStore>,
        request: tg_app_media::stories::backend::SendStoryRequest,
        now_secs: tg_data::TimeId,
    ) -> Result<tg_data::StoryId, String> {
        self.stories.send_story(store, request, now_secs)
    }
    fn edit_story(
        &mut self,
        store: Option<&mut tg_stories::StoriesStore>,
        request: tg_app_media::stories::backend::EditStoryRequest,
        now_secs: tg_data::TimeId,
    ) -> Result<(), String> {
        self.stories.edit_story(store, request, now_secs)
    }
    fn dismiss_failed_uploads(&mut self, peer_id: tg_data::PeerId) {
        self.stories.dismiss_failed_uploads(peer_id);
    }
    fn close_friends(&self) -> Vec<tg_data::PeerId> {
        self.stories.close_friends()
    }
    fn set_close_friends(&mut self, peers: Vec<tg_data::PeerId>) -> Result<(), String> {
        self.stories.set_close_friends(peers)
    }
    fn contacts_for_privacy(&self) -> Vec<StoryPeerProfile> {
        self.stories.contacts_for_privacy()
    }
    fn save_stories_blocklist(&mut self, peers: &[tg_data::PeerId]) -> Result<bool, String> {
        self.stories.save_stories_blocklist(peers)
    }
}

// =============================================================================
// 13. business::backend::BusinessBackend
// =============================================================================

impl BusinessBackend for MockSession {
    fn load_overview(&self) -> Result<BusinessOverviewSnapshot, String> {
        self.business.load_overview()
    }
    fn set_premium_enabled(&mut self, enabled: bool) -> Result<(), String> {
        self.business.set_premium_enabled(enabled)
    }
    fn set_sponsored_disabled(&mut self, disabled: bool) -> Result<(), String> {
        self.business.set_sponsored_disabled(disabled)
    }
    fn save_working_hours(&mut self, hours: WorkingHours) -> Result<(), String> {
        self.business.save_working_hours(hours)
    }
    fn save_location(&mut self, location: BusinessLocation) -> Result<(), String> {
        self.business.save_location(location)
    }
    fn save_chat_intro(&mut self, intro: ChatIntro) -> Result<(), String> {
        self.business.save_chat_intro(intro)
    }
    fn save_greeting_settings(&mut self, settings: GreetingSettings) -> Result<(), String> {
        self.business.save_greeting_settings(settings)
    }
    fn save_away_settings(&mut self, settings: AwaySettings) -> Result<(), String> {
        self.business.save_away_settings(settings)
    }
    fn search_business_bots(&self, query: &str) -> BotSearchResult {
        self.business.search_business_bots(query)
    }
    fn save_chatbots(&mut self, settings: ChatbotsSettings) -> Result<(), String> {
        self.business.save_chatbots(settings)
    }
    fn emplace_shortcut(&mut self, name: &str) -> Result<QuickReplyShortcut, String> {
        self.business.emplace_shortcut(name)
    }
    fn rename_shortcut(
        &mut self,
        shortcut_id: BusinessShortcutId,
        new_name: &str,
    ) -> Result<QuickReplyShortcut, String> {
        self.business.rename_shortcut(shortcut_id, new_name)
    }
    fn remove_shortcut(&mut self, shortcut_id: BusinessShortcutId) -> Result<(), String> {
        self.business.remove_shortcut(shortcut_id)
    }
    fn add_shortcut_message(
        &mut self,
        shortcut_id: BusinessShortcutId,
        text: &str,
        attachment: QuickReplyAttachment,
    ) -> Result<QuickReplyMessage, String> {
        self.business
            .add_shortcut_message(shortcut_id, text, attachment)
    }
    fn edit_shortcut_message(
        &mut self,
        shortcut_id: BusinessShortcutId,
        message_id: i32,
        new_text: &str,
    ) -> Result<QuickReplyMessage, String> {
        self.business
            .edit_shortcut_message(shortcut_id, message_id, new_text)
    }
    fn delete_shortcut_message(
        &mut self,
        shortcut_id: BusinessShortcutId,
        message_id: i32,
    ) -> Result<(), String> {
        self.business
            .delete_shortcut_message(shortcut_id, message_id)
    }
    fn create_chat_link(&mut self, title: &str, message: &str) -> Result<BusinessChatLink, String> {
        self.business.create_chat_link(title, message)
    }
    fn edit_chat_link(
        &mut self,
        link: &str,
        title: &str,
        message: &str,
    ) -> Result<BusinessChatLink, String> {
        self.business.edit_chat_link(link, title, message)
    }
    fn delete_chat_link(&mut self, link: &str) -> Result<(), String> {
        self.business.delete_chat_link(link)
    }
    fn set_emoji_status(&mut self, status: ActiveEmojiStatus) -> Result<(), String> {
        self.business.set_emoji_status(status)
    }
    fn leave_inactive_channels(&mut self, channel_ids: &[i64]) -> Result<usize, String> {
        self.business.leave_inactive_channels(channel_ids)
    }
    fn revoke_public_link(&mut self, channel_id: i64) -> Result<(), String> {
        self.business.revoke_public_link(channel_id)
    }
}

// =============================================================================
// 14. payments::backend::{PaymentsBackend, PassportBackend}
// =============================================================================

impl PaymentsBackend for MockSession {
    fn load_checkout_form(&self, invoice_id: &str) -> Result<CheckoutBundle, String> {
        self.payments.load_checkout_form(invoice_id)
    }
    fn validate_requested_info(
        &mut self,
        invoice_id: &str,
        info: &tg_payments::form::RequestedInformation,
    ) -> Result<ValidatedInfoResult, String> {
        self.payments.validate_requested_info(invoice_id, info)
    }
    fn tokenize_test_card(
        &self,
        details: &tg_payments::card::UncheckedCardDetails,
        save_on_server: bool,
    ) -> Result<tg_payments::form::NewCredentials, String> {
        self.payments.tokenize_test_card(details, save_on_server)
    }
    fn verify_tmp_password(&self, password: &str) -> Result<Vec<u8>, String> {
        self.payments.verify_tmp_password(password)
    }
    fn submit_payment_form(
        &mut self,
        invoice_id: &str,
        form_id: i64,
        shipping_option_id: Option<&str>,
        tip_amount: i64,
        credentials_title: &str,
        require_3ds: bool,
    ) -> Result<PaymentSubmitOutcome, String> {
        self.payments.submit_payment_form(
            invoice_id,
            form_id,
            shipping_option_id,
            tip_amount,
            credentials_title,
            require_3ds,
        )
    }
    fn load_credits_overview(&self) -> Result<CreditsOverviewBundle, String> {
        self.payments.load_credits_overview()
    }
    fn topup_stars(
        &mut self,
        option: &tg_payments::credits::CreditTopupOption,
    ) -> Result<String, String> {
        self.payments.topup_stars(option)
    }
    fn change_stars_subscription(
        &mut self,
        subscription_id: &str,
        canceled: bool,
    ) -> Result<String, String> {
        self.payments
            .change_stars_subscription(subscription_id, canceled)
    }
    fn confirm_paid_media(
        &mut self,
        media_id: u64,
        stars: i64,
    ) -> Result<tg_payments::credits::CreditsAmount, String> {
        self.payments.confirm_paid_media(media_id, stars)
    }
    fn confirm_paid_reaction(
        &mut self,
        post_id: i32,
        stars: i32,
        anonymous: bool,
    ) -> Result<tg_payments::credits::CreditsAmount, String> {
        self.payments
            .confirm_paid_reaction(post_id, stars, anonymous)
    }
    fn load_star_gifts(&self) -> Result<StarGiftsBundle, String> {
        self.payments.load_star_gifts()
    }
    fn send_star_gift(
        &mut self,
        gift_id: u64,
        recipient: &str,
        message: &str,
        hide_name: bool,
        include_upgrade: bool,
    ) -> Result<String, String> {
        self.payments
            .send_star_gift(gift_id, recipient, message, hide_name, include_upgrade)
    }
    fn apply_received_gift_action(
        &mut self,
        gift_id: u64,
        action: ReceivedGiftAction,
    ) -> Result<String, String> {
        self.payments.apply_received_gift_action(gift_id, action)
    }
    fn load_premium_promo(&self) -> Result<PremiumPromoBundle, String> {
        self.payments.load_premium_promo()
    }
    fn preview_premium_invoice(
        &self,
        option: &tg_payments::premium::PremiumSubscriptionOption,
    ) -> Result<tg_app_core::payments::backend::PremiumInvoicePreview, String> {
        self.payments.preview_premium_invoice(option)
    }
    fn load_boosts_giveaway(&self) -> Result<BoostsGiveawayBundle, String> {
        self.payments.load_boosts_giveaway()
    }
    fn apply_channel_boost(
        &mut self,
        channel_title: &str,
        slots: &[i32],
    ) -> Result<BoostsGiveawayBundle, String> {
        self.payments.apply_channel_boost(channel_title, slots)
    }
    fn as_marketplace_backend(
        &mut self,
    ) -> Option<&mut dyn tg_app_core::payments::marketplace_backend::StarGiftsMarketplaceBackend>
    {
        self.payments.as_marketplace_backend()
    }
}

impl PassportBackend for MockSession {
    fn load_authorization_form(&self, bot_username: &str) -> Result<PassportSessionBundle, String> {
        self.passport.load_authorization_form(bot_username)
    }
    fn verify_cloud_password(&mut self, password: &str) -> Result<PassportUnlockResult, String> {
        self.passport.verify_cloud_password(password)
    }
    fn setup_cloud_password(
        &mut self,
        password: &str,
        hint: &str,
    ) -> Result<PassportUnlockResult, String> {
        self.passport.setup_cloud_password(password, hint)
    }
    fn save_passport_value(
        &mut self,
        value_type: tg_passport::types::ValueType,
        fields: tg_passport::types::ValueMap,
        scan_labels: Vec<String>,
    ) -> Result<tg_passport::types::PassportForm, String> {
        self.passport
            .save_passport_value(value_type, fields, scan_labels)
    }
    fn send_contact_verification_code(
        &mut self,
        value_type: tg_passport::types::ValueType,
        value: &str,
    ) -> Result<tg_app_info::payments::backend::PassportVerificationChallenge, String> {
        self.passport
            .send_contact_verification_code(value_type, value)
    }
    fn resend_phone_verification_code(
        &mut self,
        phone: &str,
        phone_code_hash: &str,
    ) -> Result<tg_app_info::payments::backend::PassportVerificationChallenge, String> {
        self.passport
            .resend_phone_verification_code(phone, phone_code_hash)
    }
    fn confirm_contact_verification_code(
        &mut self,
        value_type: tg_passport::types::ValueType,
        value: &str,
        phone_code_hash: Option<&str>,
        code: &str,
    ) -> Result<tg_passport::types::PassportForm, String> {
        self.passport
            .confirm_contact_verification_code(value_type, value, phone_code_hash, code)
    }
    fn delete_passport_values(
        &mut self,
        types: &[tg_passport::types::ValueType],
    ) -> Result<tg_passport::types::PassportForm, String> {
        self.passport.delete_passport_values(types)
    }
    fn reset_passport(&mut self, password: &str) -> Result<PassportUnlockResult, String> {
        self.passport.reset_passport(password)
    }
    fn accept_authorization(&mut self) -> Result<String, String> {
        self.passport.accept_authorization()
    }
    fn decline_authorization(&mut self) -> Result<String, String> {
        self.passport.decline_authorization()
    }
}

// =============================================================================
// Synthetic coherent dataset builders
// =============================================================================

fn initials_for(name: &str) -> String {
    let mut parts = name.split_whitespace();
    let first = parts
        .next()
        .and_then(|s| s.chars().next())
        .map(|c| c.to_ascii_uppercase());
    let second = parts
        .next()
        .and_then(|s| s.chars().next())
        .map(|c| c.to_ascii_uppercase());
    match (first, second) {
        (Some(a), Some(b)) => format!("{a}{b}"),
        (Some(a), None) => a.to_string(),
        _ => "?".to_owned(),
    }
}

fn avatar_rgb_for_index(idx: u8) -> [u8; 3] {
    match idx % 7 {
        0 => [0xE1, 0x70, 0x76],
        1 => [0xED, 0xA8, 0x6C],
        2 => [0xA6, 0x95, 0xE7],
        3 => [0x7B, 0xC8, 0x62],
        4 => [0x6E, 0xC9, 0xCB],
        5 => [0x4D, 0xA6, 0xE8],
        _ => [0xEE, 0x7A, 0xAE],
    }
}

fn map_folder_icon(icon: FolderIcon) -> FolderIconKind {
    FolderIconKind::from(icon)
}

fn extract_utf16_slice(text: &str, offset_u16: i32, len_u16: i32) -> String {
    let start = offset_u16.max(0) as usize;
    let len = len_u16.max(0) as usize;
    let units: Vec<u16> = text.encode_utf16().collect();
    let end = start.saturating_add(len).min(units.len());
    if start >= end {
        return String::new();
    }
    String::from_utf16_lossy(&units[start..end])
}

fn rich_text_with_entities(text: &str, spans: &[(&str, EntityType)]) -> TextWithEntities {
    let mut entities = Vec::new();
    for &(needle, kind) in spans {
        if let Some(byte_idx) = text.find(needle) {
            let off = utf8_to_utf16_offset(text, byte_idx).unwrap_or(0) as i32;
            let len = needle.encode_utf16().count() as i32;
            entities.push(EntityInText::new(kind, off, len));
        }
    }
    TextWithEntities::with_entities(text.to_owned(), entities)
}

fn default_catalog_bot_info() -> tg_bots::commands::BotInfo {
    tg_bots::commands::BotInfo {
        description: "Explore interactive inline keyboards, custom reply keyboards, rich inline search (gallery & list), Mini Apps, and OAuth login confirmation — all offline in Rustgram.".to_owned(),
        inline_placeholder: "Search photos, docs, venues, or music...".to_owned(),
        commands: vec![
            tg_bots::commands::BotCommand::new("start", "Start the bot and show welcome card"),
            tg_bots::commands::BotCommand::new("catalog", "Browse featured items and inline controls"),
            tg_bots::commands::BotCommand::new("keyboard", "Show custom reply keyboard with request buttons"),
            tg_bots::commands::BotCommand::new("order", "Check order status and checkout actions"),
            tg_bots::commands::BotCommand::new("settings", "Configure notifications and language"),
            tg_bots::commands::BotCommand::new("privacy", "Read how this bot handles your data"),
        ],
        description_photo_id: Some(tg_bots::stand_in::PhotoId(90_001)),
        description_document_id: None,
        menu_button: tg_bots::commands::BotMenuButton::Commands,
        privacy_policy_url: "https://example.org/catalog-bot/privacy".to_owned(),
        active_users: 48_250,
        inited: true,
        supports_attach_menu: true,
        has_main_app: true,
        ..tg_bots::commands::BotInfo::default()
    }
}

fn build_initial_bot_infos() -> HashMap<PeerId, tg_bots::commands::BotInfo> {
    let mut map = HashMap::new();
    map.insert(session_peers::BOT_CATALOG, default_catalog_bot_info());
    map
}

fn build_initial_bot_attach_menu() -> tg_bots::mini_app::AttachMenuStore {
    let mut store = tg_bots::mini_app::AttachMenuStore::new();
    let catalog_bot_id = tg_bots::stand_in::UserId(session_peers::BOT_CATALOG.bare());
    let catalog_peer_id = tg_bots::stand_in::PeerId(session_peers::BOT_CATALOG.0);
    store.update_bots(
        42,
        vec![
            tg_bots::mini_app::AttachMenuBot {
                bot_id: catalog_bot_id,
                bot_peer_id: catalog_peer_id,
                icon_document_id: Some(tg_bots::stand_in::DocumentId(88_001)),
                short_name: "Catalog Shop".to_owned(),
                peer_types: tg_bots::markup::PeerTypes::SAME_BOT
                    | tg_bots::markup::PeerTypes::BOT
                    | tg_bots::markup::PeerTypes::USER
                    | tg_bots::markup::PeerTypes::GROUP
                    | tg_bots::markup::PeerTypes::BROADCAST,
                inactive: false,
                in_side_menu: true,
                in_attach_menu: true,
                disclaimer_required: false,
                request_write_access: false,
            },
            tg_bots::mini_app::AttachMenuBot {
                bot_id: tg_bots::stand_in::UserId(777_002),
                bot_peer_id: tg_bots::stand_in::PeerId(777_002),
                icon_document_id: Some(tg_bots::stand_in::DocumentId(88_002)),
                short_name: "Quick Wallet".to_owned(),
                peer_types: tg_bots::markup::PeerTypes::SAME_BOT
                    | tg_bots::markup::PeerTypes::BOT
                    | tg_bots::markup::PeerTypes::USER
                    | tg_bots::markup::PeerTypes::GROUP,
                inactive: false,
                in_side_menu: true,
                in_attach_menu: true,
                disclaimer_required: true,
                request_write_access: true,
            },
            tg_bots::mini_app::AttachMenuBot {
                bot_id: tg_bots::stand_in::UserId(777_003),
                bot_peer_id: tg_bots::stand_in::PeerId(777_003),
                icon_document_id: Some(tg_bots::stand_in::DocumentId(88_003)),
                short_name: "Poll Studio".to_owned(),
                peer_types: tg_bots::markup::PeerTypes::SAME_BOT
                    | tg_bots::markup::PeerTypes::BOT
                    | tg_bots::markup::PeerTypes::GROUP
                    | tg_bots::markup::PeerTypes::BROADCAST,
                inactive: false,
                in_side_menu: false,
                in_attach_menu: true,
                disclaimer_required: false,
                request_write_access: false,
            },
            tg_bots::mini_app::AttachMenuBot {
                bot_id: tg_bots::stand_in::UserId(777_004),
                bot_peer_id: tg_bots::stand_in::PeerId(777_004),
                icon_document_id: Some(tg_bots::stand_in::DocumentId(88_004)),
                short_name: "Route Planner".to_owned(),
                peer_types: tg_bots::markup::PeerTypes::SAME_BOT
                    | tg_bots::markup::PeerTypes::BOT
                    | tg_bots::markup::PeerTypes::USER
                    | tg_bots::markup::PeerTypes::GROUP,
                inactive: true,
                in_side_menu: false,
                in_attach_menu: true,
                disclaimer_required: true,
                request_write_access: true,
            },
        ],
    );
    store
}

fn build_initial_peers(user: &AuthUser) -> Vec<PeerRecord> {
    vec![
        PeerRecord {
            id: session_peers::SELF,
            kind: InfoPeerKind::SavedMessages,
            title: "Saved Messages".to_owned(),
            status: "cloud storage".to_owned(),
            is_online: false,
            verified: false,
            premium: false,
            color_index: 5,
            avatar_rgb: [0x4D, 0xA6, 0xE8],
            initials: "SM".to_owned(),
            phone: Some(user.phone.clone()),
            usernames: vec![PeerUsername {
                username: user.username.clone(),
                is_editable: true,
                is_active: true,
            }],
            bio: user.bio.clone(),
            birthday: None,
            invite_link: None,
            muted: false,
            pinned: true,
            archived: false,
            unread_count: 0,
            unread_mark: false,
            unread_mentions: 0,
            is_contact: false,
            is_blocked: false,
            can_send: true,
            can_edit: true,
            can_add_members: false,
            can_manage_admins: false,
            can_manage_permissions: false,
            is_public: false,
            no_forwards: false,
            is_forum: false,
            members_count: 0,
            online_count: 0,
            admins_count: 0,
            default_banned_rights: ChatBannedRights::empty(),
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: 0,
            linked_discussion_peer: None,
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            folders: vec![session_folders::PERSONAL],
            contact_status: None,
            typing: None,
            last_message_time: LocalDateTime::new(2026, 11, 22, 14, 22, 0),
        },
        PeerRecord {
            id: session_peers::GROUP_CORE,
            kind: InfoPeerKind::Supergroup,
            title: "Rustgram Core Port".to_owned(),
            status: "20 members, 7 online".to_owned(),
            is_online: false,
            verified: false,
            premium: false,
            color_index: 0,
            avatar_rgb: [0xE1, 0x70, 0x76],
            initials: "TC".to_owned(),
            phone: None,
            usernames: vec![PeerUsername {
                username: "rustgram_core".to_owned(),
                is_editable: true,
                is_active: true,
            }],
            bio: "Coordination supergroup for the pure-Rust Telegram Desktop port.".to_owned(),
            birthday: None,
            invite_link: Some("https://t.me/rustgram_core".to_owned()),
            muted: false,
            pinned: true,
            archived: false,
            unread_count: 2,
            unread_mark: false,
            unread_mentions: 1,
            is_contact: false,
            is_blocked: false,
            can_send: true,
            can_edit: true,
            can_add_members: true,
            can_manage_admins: true,
            can_manage_permissions: true,
            is_public: true,
            no_forwards: false,
            is_forum: true,
            members_count: 20,
            online_count: 7,
            admins_count: 3,
            default_banned_rights: ChatBannedRights::empty(),
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: 0,
            linked_discussion_peer: Some(session_peers::CHANNEL_DEV_LOG),
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            folders: vec![session_folders::WORK],
            contact_status: None,
            typing: None,
            last_message_time: LocalDateTime::new(2026, 11, 22, 14, 18, 0),
        },
        PeerRecord {
            id: session_peers::USER_MIRA,
            kind: InfoPeerKind::User,
            title: "Mira Vance".to_owned(),
            status: "online".to_owned(),
            is_online: true,
            verified: false,
            premium: true,
            color_index: 3,
            avatar_rgb: [0x7B, 0xC8, 0x62],
            initials: "MV".to_owned(),
            phone: Some("+999 661 0142".to_owned()),
            usernames: vec![
                PeerUsername {
                    username: "mira_vance".to_owned(),
                    is_editable: true,
                    is_active: true,
                },
                PeerUsername {
                    username: "mv_graphics".to_owned(),
                    is_editable: false,
                    is_active: true,
                },
            ],
            bio: "Systems & graphics engineer working on pure-Rust UI toolkits and shaders."
                .to_owned(),
            birthday: Some(InfoBirthday {
                day: 14,
                month: 5,
                year: Some(1994),
            }),
            invite_link: None,
            muted: false,
            pinned: false,
            archived: false,
            unread_count: 1,
            unread_mark: false,
            unread_mentions: 0,
            is_contact: true,
            is_blocked: false,
            can_send: true,
            can_edit: true,
            can_add_members: false,
            can_manage_admins: false,
            can_manage_permissions: false,
            is_public: true,
            no_forwards: false,
            is_forum: false,
            members_count: 0,
            online_count: 0,
            admins_count: 0,
            default_banned_rights: ChatBannedRights::empty(),
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: 0,
            linked_discussion_peer: None,
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            folders: vec![session_folders::PERSONAL, session_folders::WORK],
            contact_status: None,
            typing: None,
            last_message_time: LocalDateTime::new(2026, 11, 22, 13, 55, 0),
        },
        PeerRecord {
            id: session_peers::CHANNEL_DEV_LOG,
            kind: InfoPeerKind::Channel,
            title: "Rustgram Dev Log".to_owned(),
            status: "14,720 subscribers".to_owned(),
            is_online: false,
            verified: true,
            premium: false,
            color_index: 2,
            avatar_rgb: [0xA6, 0x95, 0xE7],
            initials: "DL".to_owned(),
            phone: None,
            usernames: vec![PeerUsername {
                username: "rustgram_log".to_owned(),
                is_editable: true,
                is_active: true,
            }],
            bio: "Architecture updates, Instant View engineering notes, and channel statistics."
                .to_owned(),
            birthday: None,
            invite_link: Some("https://t.me/rustgram_log".to_owned()),
            muted: false,
            pinned: false,
            archived: false,
            unread_count: 3,
            unread_mark: false,
            unread_mentions: 0,
            is_contact: false,
            is_blocked: false,
            can_send: false,
            can_edit: true,
            can_add_members: true,
            can_manage_admins: true,
            can_manage_permissions: false,
            is_public: true,
            no_forwards: false,
            is_forum: false,
            members_count: 14_720,
            online_count: 0,
            admins_count: 2,
            default_banned_rights: ChatBannedRights::empty(),
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: 0,
            linked_discussion_peer: Some(session_peers::GROUP_CORE),
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            folders: vec![session_folders::CHANNELS, session_folders::WORK],
            contact_status: None,
            typing: None,
            last_message_time: LocalDateTime::new(2026, 11, 22, 13, 20, 0),
        },
        PeerRecord {
            id: session_peers::BOT_CATALOG,
            kind: InfoPeerKind::Bot,
            title: "Catalog Bot".to_owned(),
            status: "bot".to_owned(),
            is_online: false,
            verified: true,
            premium: false,
            color_index: 5,
            avatar_rgb: [0x40, 0xA7, 0xE3],
            initials: "CB".to_owned(),
            phone: None,
            usernames: vec![PeerUsername {
                username: "catalog_demo_bot".to_owned(),
                is_editable: false,
                is_active: true,
            }],
            bio: "Interactive WebView2 Mini App showcase bot. Tap Open App to launch the Mini App."
                .to_owned(),
            birthday: None,
            invite_link: None,
            muted: false,
            pinned: false,
            archived: false,
            unread_count: 0,
            unread_mark: false,
            unread_mentions: 0,
            is_contact: false,
            is_blocked: false,
            can_send: true,
            can_edit: false,
            can_add_members: false,
            can_manage_admins: false,
            can_manage_permissions: false,
            is_public: true,
            no_forwards: false,
            is_forum: false,
            members_count: 0,
            online_count: 0,
            admins_count: 0,
            default_banned_rights: ChatBannedRights::empty(),
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: 0,
            linked_discussion_peer: None,
            bot_commands_count: 3,
            bot_reads_all_messages: false,
            has_mini_app: true,
            folders: vec![session_folders::BOTS],
            contact_status: None,
            typing: None,
            last_message_time: LocalDateTime::new(2026, 11, 22, 12, 50, 0),
        },
        PeerRecord {
            id: session_peers::USER_THEO,
            kind: InfoPeerKind::User,
            title: "Theo Park".to_owned(),
            status: "last seen 12 minutes ago".to_owned(),
            is_online: false,
            verified: false,
            premium: false,
            color_index: 1,
            avatar_rgb: [0xED, 0xA8, 0x6C],
            initials: "TP".to_owned(),
            phone: Some("+999 661 0101".to_owned()),
            usernames: vec![PeerUsername {
                username: "theo_park".to_owned(),
                is_editable: true,
                is_active: true,
            }],
            bio: "MTProto crypto primitives, AES-IGE vectors, and SRP verification.".to_owned(),
            birthday: None,
            invite_link: None,
            muted: false,
            pinned: false,
            archived: false,
            unread_count: 0,
            unread_mark: false,
            unread_mentions: 0,
            is_contact: false,
            is_blocked: false,
            can_send: true,
            can_edit: false,
            can_add_members: false,
            can_manage_admins: false,
            can_manage_permissions: false,
            is_public: true,
            no_forwards: false,
            is_forum: false,
            members_count: 0,
            online_count: 0,
            admins_count: 0,
            default_banned_rights: ChatBannedRights::empty(),
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: 0,
            linked_discussion_peer: None,
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            folders: vec![session_folders::PERSONAL],
            contact_status: Some(ContactStatusBanner::AddOrBlock {
                peer_name: "Theo Park".to_owned(),
            }),
            typing: None,
            last_message_time: LocalDateTime::new(2026, 11, 22, 12, 40, 0),
        },
        PeerRecord {
            id: session_peers::USER_SAM,
            kind: InfoPeerKind::User,
            title: "Sam Chen".to_owned(),
            status: "online".to_owned(),
            is_online: true,
            verified: false,
            premium: false,
            color_index: 4,
            avatar_rgb: [0x6E, 0xC9, 0xCB],
            initials: "SC".to_owned(),
            phone: Some("+999 661 0203".to_owned()),
            usernames: vec![PeerUsername {
                username: "sam_chen".to_owned(),
                is_editable: true,
                is_active: true,
            }],
            bio: "Virtualized scroll lists, keyboard shortcuts, and layout benchmarks.".to_owned(),
            birthday: None,
            invite_link: None,
            muted: false,
            pinned: false,
            archived: false,
            unread_count: 0,
            unread_mark: false,
            unread_mentions: 0,
            is_contact: true,
            is_blocked: false,
            can_send: true,
            can_edit: false,
            can_add_members: false,
            can_manage_admins: false,
            can_manage_permissions: false,
            is_public: true,
            no_forwards: false,
            is_forum: false,
            members_count: 0,
            online_count: 0,
            admins_count: 0,
            default_banned_rights: ChatBannedRights::empty(),
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: 0,
            linked_discussion_peer: None,
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            folders: vec![session_folders::PERSONAL, session_folders::WORK],
            contact_status: None,
            typing: None,
            last_message_time: LocalDateTime::new(2026, 11, 22, 11, 45, 0),
        },
        PeerRecord {
            id: session_peers::GROUP_UI_LAB,
            kind: InfoPeerKind::Supergroup,
            title: "UI & Theme Lab".to_owned(),
            status: "12 members, 4 online".to_owned(),
            is_online: false,
            verified: false,
            premium: false,
            color_index: 4,
            avatar_rgb: [0x6E, 0xC9, 0xCB],
            initials: "UT".to_owned(),
            phone: None,
            usernames: vec![PeerUsername {
                username: "rustgram_ui".to_owned(),
                is_editable: true,
                is_active: true,
            }],
            bio: "Palette colorizer, Open Sans typography, and adaptive layout QA.".to_owned(),
            birthday: None,
            invite_link: Some("https://t.me/rustgram_ui".to_owned()),
            muted: false,
            pinned: false,
            archived: false,
            unread_count: 0,
            unread_mark: false,
            unread_mentions: 0,
            is_contact: false,
            is_blocked: false,
            can_send: true,
            can_edit: true,
            can_add_members: true,
            can_manage_admins: true,
            can_manage_permissions: true,
            is_public: true,
            no_forwards: false,
            is_forum: false,
            members_count: 12,
            online_count: 4,
            admins_count: 2,
            default_banned_rights: ChatBannedRights::empty(),
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: 0,
            linked_discussion_peer: None,
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            folders: vec![session_folders::WORK],
            contact_status: None,
            typing: None,
            last_message_time: LocalDateTime::new(2026, 11, 21, 19, 10, 0),
        },
        PeerRecord {
            id: session_peers::USER_RILEY,
            kind: InfoPeerKind::User,
            title: "Riley Brooks".to_owned(),
            status: "last seen recently".to_owned(),
            is_online: false,
            verified: false,
            premium: false,
            color_index: 6,
            avatar_rgb: [0xEE, 0x7A, 0xAE],
            initials: "RB".to_owned(),
            phone: Some("+999 661 0204".to_owned()),
            usernames: vec![PeerUsername {
                username: "riley_brooks".to_owned(),
                is_editable: true,
                is_active: true,
            }],
            bio: "Audio DSP, Opus voice waveforms, and media pipeline testing.".to_owned(),
            birthday: None,
            invite_link: None,
            muted: false,
            pinned: false,
            archived: false,
            unread_count: 0,
            unread_mark: false,
            unread_mentions: 0,
            is_contact: true,
            is_blocked: false,
            can_send: true,
            can_edit: false,
            can_add_members: false,
            can_manage_admins: false,
            can_manage_permissions: false,
            is_public: true,
            no_forwards: false,
            is_forum: false,
            members_count: 0,
            online_count: 0,
            admins_count: 0,
            default_banned_rights: ChatBannedRights::empty(),
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: 0,
            linked_discussion_peer: None,
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            folders: vec![session_folders::PERSONAL],
            contact_status: None,
            typing: None,
            last_message_time: LocalDateTime::new(2026, 11, 21, 16, 30, 0),
        },
        PeerRecord {
            id: session_peers::CHANNEL_RELEASES,
            kind: InfoPeerKind::Channel,
            title: "Nightly Builds".to_owned(),
            status: "1,420 subscribers".to_owned(),
            is_online: false,
            verified: false,
            premium: false,
            color_index: 6,
            avatar_rgb: [0xEE, 0x7A, 0xAE],
            initials: "NB".to_owned(),
            phone: None,
            usernames: vec![PeerUsername {
                username: "rustgram_builds".to_owned(),
                is_editable: true,
                is_active: true,
            }],
            bio: "Automated Windows x64 MSVC build status announcements.".to_owned(),
            birthday: None,
            invite_link: Some("https://t.me/rustgram_builds".to_owned()),
            muted: true,
            pinned: false,
            archived: false,
            unread_count: 4,
            unread_mark: false,
            unread_mentions: 0,
            is_contact: false,
            is_blocked: false,
            can_send: false,
            can_edit: true,
            can_add_members: false,
            can_manage_admins: true,
            can_manage_permissions: false,
            is_public: true,
            no_forwards: false,
            is_forum: false,
            members_count: 1_420,
            online_count: 0,
            admins_count: 1,
            default_banned_rights: ChatBannedRights::empty(),
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: 0,
            linked_discussion_peer: None,
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            folders: vec![session_folders::CHANNELS],
            contact_status: None,
            typing: None,
            last_message_time: LocalDateTime::new(2026, 11, 20, 9, 15, 0),
        },
        PeerRecord {
            id: session_peers::CHANNEL_ARCHIVE,
            kind: InfoPeerKind::Channel,
            title: "Archive Digest".to_owned(),
            status: "820 subscribers".to_owned(),
            is_online: false,
            verified: false,
            premium: false,
            color_index: 1,
            avatar_rgb: [0xED, 0xA8, 0x6C],
            initials: "AD".to_owned(),
            phone: None,
            usernames: vec![PeerUsername {
                username: "archive_digest".to_owned(),
                is_editable: true,
                is_active: true,
            }],
            bio: "Archived weekly engineering summaries.".to_owned(),
            birthday: None,
            invite_link: None,
            muted: true,
            pinned: false,
            archived: true,
            unread_count: 2,
            unread_mark: false,
            unread_mentions: 0,
            is_contact: false,
            is_blocked: false,
            can_send: false,
            can_edit: false,
            can_add_members: false,
            can_manage_admins: false,
            can_manage_permissions: false,
            is_public: true,
            no_forwards: false,
            is_forum: false,
            members_count: 820,
            online_count: 0,
            admins_count: 1,
            default_banned_rights: ChatBannedRights::empty(),
            slowmode_seconds: 0,
            auto_delete_ttl_seconds: 0,
            linked_discussion_peer: None,
            bot_commands_count: 0,
            bot_reads_all_messages: false,
            has_mini_app: false,
            folders: Vec::new(),
            contact_status: None,
            typing: None,
            last_message_time: LocalDateTime::new(2026, 11, 18, 17, 0, 0),
        },
    ]
}

#[allow(clippy::type_complexity)]
fn build_initial_histories() -> (
    HashMap<PeerId, Vec<HistoryMessage>>,
    HashMap<PeerId, Vec<MsgId>>,
    HashMap<PeerId, Option<MsgId>>,
    HashMap<(PeerId, MsgId), MediaItemId>,
) {
    let mut histories: HashMap<PeerId, Vec<HistoryMessage>> = HashMap::new();
    let mut pinned: HashMap<PeerId, Vec<MsgId>> = HashMap::new();
    let mut first_unread: HashMap<PeerId, Option<MsgId>> = HashMap::new();
    let mut msg_media_map: HashMap<(PeerId, MsgId), MediaItemId> = HashMap::new();

    // 1. Saved Messages
    histories.insert(
        session_peers::SELF,
        vec![
            HistoryMessage {
                extra: Default::default(),
                id: MsgId(1),
                peer_id: session_peers::SELF,
                from_id: session_peers::SELF,
                sender_name: "Taylor Kim".to_owned(),
                date_unix: BASE_UNIX_YESTERDAY + 3_600,
                outgoing: true,
                is_post: false,
                text: rich_text_with_entities(
                    "Build artifacts always live under target/rustgram #notes",
                    &[("target/rustgram", EntityType::Code), ("#notes", EntityType::Hashtag)],
                ),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
            },
            HistoryMessage {
                extra: Default::default(),
                id: MsgId(2),
                peer_id: session_peers::SELF,
                from_id: session_peers::SELF,
                sender_name: "Taylor Kim".to_owned(),
                date_unix: BASE_UNIX_TODAY + 15_720,
                outgoing: true,
                is_post: false,
                text: rich_text_with_entities(
                    "Column thresholds: 260 / 380 / 292 px — read Instant View notes at https://telegra.ph/Rustgram-Instant-View-Sample-11-22 #todo",
                    &[
                        ("https://telegra.ph/Rustgram-Instant-View-Sample-11-22", EntityType::Url),
                        ("#todo", EntityType::Hashtag),
                    ],
                ),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: vec![ReactionCounter {
                    emoji: "📌".to_owned(),
                    count: 1,
                    chosen: true,
                }],
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: true,
            },
        ],
    );
    pinned.insert(session_peers::SELF, vec![MsgId(2)]);

    // 2. Rustgram Core Port (rich showcase chat opened by default!)
    let core = session_peers::GROUP_CORE;
    histories.insert(
        core,
        vec![
            HistoryMessage {
                extra: Default::default(),
                id: MsgId(100),
                peer_id: core,
                from_id: session_peers::USER_MIRA,
                sender_name: "Mira Vance".to_owned(),
                date_unix: BASE_UNIX_YESTERDAY + 1_200,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::default(),
                media: None,
                service: Some(ServiceAction::ChatCreated {
                    title: "Rustgram Core Port".to_owned(),
                }),
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
            },
            HistoryMessage {
                extra: Default::default(),
                id: MsgId(101),
                peer_id: core,
                from_id: session_peers::USER_SAM,
                sender_name: "Sam Chen".to_owned(),
                date_unix: BASE_UNIX_YESTERDAY + 5_400,
                outgoing: false,
                is_post: false,
                text: rich_text_with_entities(
                    "Unified App Checklist: dialogs, history, info/boxes, settings, media viewer, chat_helpers, notifications, webview, and statistics all wired to one MockSession! Read https://telegra.ph/Rustgram-Instant-View-Sample-11-22 #rustgram",
                    &[
                        ("Unified App Checklist:", EntityType::Bold),
                        ("MockSession", EntityType::Code),
                        (SAMPLE_IV_URL, EntityType::Url),
                        ("#rustgram", EntityType::Hashtag),
                    ],
                ),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: vec![
                    ReactionCounter {
                        emoji: "🔥".to_owned(),
                        count: 5,
                        chosen: true,
                    },
                    ReactionCounter {
                        emoji: "👍".to_owned(),
                        count: 3,
                        chosen: false,
                    },
                ],
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: true,
            },
            HistoryMessage {
                extra: Default::default(),
                id: MsgId(102),
                peer_id: core,
                from_id: session_peers::USER_MIRA,
                sender_name: "Mira Vance".to_owned(),
                date_unix: BASE_UNIX_TODAY + 7_200,
                outgoing: false,
                is_post: false,
                text: rich_text_with_entities(
                    "Alpine Ridge test photo — click to open the fullscreen Media Viewer (zoom, rotate, gallery siblings, PiP)!",
                    &[("Media Viewer", EntityType::Bold)],
                ),
                media: Some(HistoryMedia::Photo(PhotoAttachment {
                    width: 1280,
                    height: 800,
                    stripped_thumb: Some(synthetic_stripped_thumb(0x31)),
                    fallback_rgb: ([0xE0, 0x85, 0x52], [0x26, 0x3D, 0x6B]),
                    file_name: "alpine_ridge_sunset.png".to_owned(),
                    size_bytes: 284_100,
                })),
                service: None,
                reply: None,
                forward: None,
                reactions: vec![ReactionCounter {
                    emoji: "❤️".to_owned(),
                    count: 4,
                    chosen: false,
                }],
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
            },
            HistoryMessage {
                extra: Default::default(),
                id: MsgId(103),
                peer_id: core,
                from_id: session_peers::USER_RILEY,
                sender_name: "Riley Brooks".to_owned(),
                date_unix: BASE_UNIX_TODAY + 9_000,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::simple(
                    "Attached the architecture deck and a quick voice summary of the audio pipeline.",
                ),
                media: Some(HistoryMedia::Document(DocumentAttachment {
                    file_name: "rustgram_navigation_spec.pdf".to_owned(),
                    size_bytes: 1_480_000,
                    mime_type: "application/pdf".to_owned(),
                    thumb_rgb: Some([0x40, 0xA7, 0xE3]),
                })),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
            },
            HistoryMessage {
                extra: Default::default(),
                id: MsgId(104),
                peer_id: core,
                from_id: session_peers::USER_RILEY,
                sender_name: "Riley Brooks".to_owned(),
                date_unix: BASE_UNIX_TODAY + 9_120,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::default(),
                media: Some(HistoryMedia::Voice(VoiceAttachment {
                    duration_secs: 18,
                    waveform_5bit: sample_voice_waveform_5bit(7),
                    is_unread: true,
                    transcription: Some(
                        "AudioPlayerBar plays real synthesized WAV tracks via tg-audio.".to_owned(),
                    ),
                })),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
            },
            HistoryMessage {
                extra: Default::default(),
                id: MsgId(105),
                peer_id: core,
                from_id: session_peers::SELF,
                sender_name: "Taylor Kim".to_owned(),
                date_unix: BASE_UNIX_TODAY + 12_000,
                outgoing: true,
                is_post: false,
                text: rich_text_with_entities(
                    "Wired ThemeManager + bundled Open Sans & Hack fonts + tg-lang phrases! Also check @rustgram_log for Channel Statistics & Boosts.",
                    &[
                        ("ThemeManager", EntityType::Code),
                        ("@rustgram_log", EntityType::Mention),
                    ],
                ),
                media: None,
                service: None,
                reply: Some(ReplyHeader {
                    reply_to_msg_id: MsgId(101),
                    author_name: "Sam Chen".to_owned(),
                    author_color_id: session_peers::USER_SAM.bare(),
                    preview_text: "Unified App Checklist: dialogs, history, info/boxes..."
                        .to_owned(),
                    preview_thumb_rgb: None,
                    quote_text: None,
                }),
                forward: None,
                reactions: vec![ReactionCounter {
                    emoji: "🎉".to_owned(),
                    count: 3,
                    chosen: true,
                }],
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
            },
            HistoryMessage {
                extra: Default::default(),
                id: MsgId(106),
                peer_id: core,
                from_id: session_peers::USER_MIRA,
                sender_name: "Mira Vance".to_owned(),
                date_unix: BASE_UNIX_TODAY + 13_500,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::simple("Studio gallery album (3 items) — click any tile!"),
                media: Some(HistoryMedia::Album(vec![
                    AlbumItem {
                        msg_id: MsgId(1061),
                        width: 1280,
                        height: 800,
                        stripped_thumb: Some(synthetic_stripped_thumb(0x21)),
                        fallback_rgb: ([0xE0, 0x85, 0x52], [0x26, 0x3D, 0x6B]),
                        video_duration_secs: None,
                        label: "Alpine Ridge Sunset".to_owned(),
                    },
                    AlbumItem {
                        msg_id: MsgId(1062),
                        width: 840,
                        height: 1050,
                        stripped_thumb: Some(synthetic_stripped_thumb(0x45)),
                        fallback_rgb: ([0x4B, 0x8A, 0xE6], [0x1C, 0x2F, 0x4E]),
                        video_duration_secs: None,
                        label: "Studio Grid Poster".to_owned(),
                    },
                    AlbumItem {
                        msg_id: MsgId(1063),
                        width: 1280,
                        height: 720,
                        stripped_thumb: Some(synthetic_stripped_thumb(0x63)),
                        fallback_rgb: ([0x29, 0xB8, 0x84], [0x0E, 0x1E, 0x36]),
                        video_duration_secs: None,
                        label: "Northern Aurora Panorama".to_owned(),
                    },
                ])),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
            },
            HistoryMessage {
                extra: Default::default(),
                id: MsgId(107),
                peer_id: core,
                from_id: session_peers::USER_SAM,
                sender_name: "Sam Chen".to_owned(),
                date_unix: BASE_UNIX_TODAY + 14_400,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::default(),
                media: Some(HistoryMedia::Sticker(StickerAttachment {
                    sticker_id: 1001,
                    emoji: "⭐".to_owned(),
                    set_title: "Animated Shapes".to_owned(),
                    width: 512,
                    height: 512,
                    lottie_json: sample_lottie_star_json(),
                })),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
            },
            HistoryMessage {
                extra: Default::default(),
                id: MsgId(108),
                peer_id: core,
                from_id: session_peers::USER_MIRA,
                sender_name: "Mira Vance".to_owned(),
                date_unix: BASE_UNIX_TODAY + 15_000,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::default(),
                media: Some(HistoryMedia::Poll(PollAttachment {
                    question: "Which theme should be the default for screenshots?".to_owned(),
                    is_quiz: false,
                    explanation: None,
                    is_anonymous: true,
                    multiple_choice: false,
                    closed: false,
                    options: vec![
                        PollOption {
                            text: "Classic Day (Open Sans)".to_owned(),
                            votes: 14,
                            chosen: false,
                            correct: false,
                        },
                        PollOption {
                            text: "Night Blue".to_owned(),
                            votes: 9,
                            chosen: false,
                            correct: false,
                        },
                        PollOption {
                            text: "Tinted Night Green".to_owned(),
                            votes: 4,
                            chosen: false,
                            correct: false,
                        },
                    ],
                    total_voters: 27,
                })),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: false,
                is_sending: false,
                is_pinned: false,
            },
            HistoryMessage {
                extra: Default::default(),
                id: MsgId(109),
                peer_id: core,
                from_id: session_peers::USER_MIRA,
                sender_name: "Mira Vance".to_owned(),
                date_unix: BASE_UNIX_TODAY + 15_480,
                outgoing: false,
                is_post: false,
                text: rich_text_with_entities(
                    "Adaptive ThreeColumn layout matches window_session_controller.cpp! Click the chat header to toggle the profile third column.",
                    &[("window_session_controller.cpp", EntityType::Code)],
                ),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: vec![ReactionCounter {
                    emoji: "🚀".to_owned(),
                    count: 2,
                    chosen: false,
                }],
                views: None,
                edited: false,
                is_read: false,
                is_sending: false,
                is_pinned: false,
            },
        ],
    );
    pinned.insert(core, vec![MsgId(101)]);
    first_unread.insert(core, Some(MsgId(108)));
    msg_media_map.insert((core, MsgId(102)), MOCK_PHOTO_LANDSCAPE_ID);
    msg_media_map.insert((core, MsgId(106)), MOCK_PHOTO_LANDSCAPE_ID);
    msg_media_map.insert((core, MsgId(1061)), MOCK_PHOTO_LANDSCAPE_ID);
    msg_media_map.insert((core, MsgId(1062)), MOCK_PHOTO_PORTRAIT_ID);
    msg_media_map.insert((core, MsgId(1063)), MOCK_PHOTO_AURORA_ID);

    // 3. Mira Vance DM
    let mira = session_peers::USER_MIRA;
    histories.insert(
        mira,
        vec![
            HistoryMessage {
                extra: Default::default(),
                id: MsgId(201),
                peer_id: mira,
                from_id: mira,
                sender_name: "Mira Vance".to_owned(),
                date_unix: BASE_UNIX_TODAY + 13_800,
                outgoing: false,
                is_post: false,
                text: rich_text_with_entities(
                    "Hey Taylor! Here is the studio poster render — tap it to test zoom & rotate in Media Viewer.",
                    &[("Media Viewer", EntityType::Bold)],
                ),
                media: Some(HistoryMedia::Photo(PhotoAttachment {
                    width: 840,
                    height: 1050,
                    stripped_thumb: Some(synthetic_stripped_thumb(0x52)),
                    fallback_rgb: ([0x4B, 0x8A, 0xE6], [0x1C, 0x2F, 0x4E]),
                    file_name: "studio_grid_poster.png".to_owned(),
                    size_bytes: 192_400,
                })),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
            },
            HistoryMessage {
                extra: MessageExtra {
                    poll_extras: Some(tg_app_history::history::sections_model::PollMessageExtras {
                        open_answers: true,
                        shuffle_answers: false,
                        revoting_disabled: false,
                        hide_results_until_close: false,
                        option_attachments: vec![
                            Some(tg_app_history::history::sections_model::PollOptionAttachment::Link {
                                url: "https://example.test/themes/classic-day".to_owned(),
                                title: Some("Classic Day Preview".to_owned()),
                            }),
                            None,
                        ],
                        option_bytes: vec![vec![0], vec![1]],
                        option_added_by: vec![None, Some("Mira Vance".to_owned())],
                        attached_media: None,
                        solution_media: None,
                    }),
                    ..MessageExtra::default()
                },
                id: MsgId(2011),
                peer_id: mira,
                from_id: mira,
                sender_name: "Mira Vance".to_owned(),
                date_unix: BASE_UNIX_TODAY + 13_920,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::default(),
                media: Some(HistoryMedia::Poll(PollAttachment {
                    question: "Pick a sticker pack accent theme (open answers enabled):".to_owned(),
                    is_quiz: false,
                    explanation: None,
                    is_anonymous: false,
                    multiple_choice: false,
                    closed: false,
                    options: vec![
                        PollOption {
                            text: "Classic Day Blue".to_owned(),
                            votes: 6,
                            chosen: false,
                            correct: false,
                        },
                        PollOption {
                            text: "Warm Sunset Coral".to_owned(),
                            votes: 3,
                            chosen: false,
                            correct: false,
                        },
                    ],
                    total_voters: 9,
                })),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
            },
            HistoryMessage {
                extra: MessageExtra {
                    ttl_seconds: Some(tg_app_info::boxes::self_destruct::VIEW_ONCE_TTL_SECONDS),
                    ttl_viewed: false,
                    ..MessageExtra::default()
                },
                id: MsgId(2012),
                peer_id: mira,
                from_id: mira,
                sender_name: "Mira Vance".to_owned(),
                date_unix: BASE_UNIX_TODAY + 14_010,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::simple(
                    "Quick view-once design preview (tap to open once).",
                ),
                media: Some(HistoryMedia::Photo(PhotoAttachment {
                    width: 960,
                    height: 640,
                    stripped_thumb: None,
                    fallback_rgb: ([0x30, 0x62, 0x98], [0x6A, 0xB4, 0xEB]),
                    file_name: "view_once_design_preview.jpg".to_owned(),
                    size_bytes: 128_000,
                })),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
            },
            HistoryMessage {
                extra: Default::default(),
                id: MsgId(202),
                peer_id: mira,
                from_id: mira,
                sender_name: "Mira Vance".to_owned(),
                date_unix: BASE_UNIX_TODAY + 14_100,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::simple(
                    "Can you verify back navigation from Info -> History and emoji insertion?",
                ),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: false,
                is_sending: false,
                is_pinned: false,
            },
        ],
    );
    first_unread.insert(mira, Some(MsgId(202)));
    msg_media_map.insert((mira, MsgId(201)), MOCK_PHOTO_PORTRAIT_ID);

    // 4. Rustgram Dev Log channel (Instant View links + photo + views + comments -> Statistics & Boosts!)
    let devlog = session_peers::CHANNEL_DEV_LOG;
    histories.insert(
        devlog,
        vec![
            HistoryMessage {
                extra: MessageExtra {
                    replies_count: Some(3),
                    ..MessageExtra::default()
                },
                id: MsgId(301),
                peer_id: devlog,
                from_id: devlog,
                sender_name: "Rustgram Dev Log".to_owned(),
                date_unix: BASE_UNIX_YESTERDAY + 18_000,
                outgoing: false,
                is_post: true,
                text: rich_text_with_entities(
                    "Deep dive into Rustgram Instant View rendering and custom scheme routing: https://telegra.ph/Rustgram-Instant-View-Sample-11-22 #release",
                    &[
                        (SAMPLE_IV_URL, EntityType::Url),
                        ("#release", EntityType::Hashtag),
                    ],
                ),
                media: Some(HistoryMedia::Photo(PhotoAttachment {
                    width: 1280,
                    height: 720,
                    stripped_thumb: Some(synthetic_stripped_thumb(0x74)),
                    fallback_rgb: ([0x29, 0xB8, 0x84], [0x0E, 0x1E, 0x36]),
                    file_name: "northern_aurora.png".to_owned(),
                    size_bytes: 312_000,
                })),
                service: None,
                reply: None,
                forward: None,
                reactions: vec![ReactionCounter {
                    emoji: "🔥".to_owned(),
                    count: 184,
                    chosen: true,
                }],
                views: Some(8_420),
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: true,
            },
            HistoryMessage {
                extra: MessageExtra {
                    replies_count: Some(2),
                    ..MessageExtra::default()
                },
                id: MsgId(302),
                peer_id: devlog,
                from_id: devlog,
                sender_name: "Rustgram Dev Log".to_owned(),
                date_unix: BASE_UNIX_TODAY + 12_000,
                outgoing: false,
                is_post: true,
                text: rich_text_with_entities(
                    "Open Channel Info in the third column to inspect live Channel Statistics, Boosts, and Monetization charts! Architecture notes: https://telegra.ph/Rustgram-Architecture-Notes-11-22",
                    &[
                        ("Channel Statistics", EntityType::Bold),
                        ("https://telegra.ph/Rustgram-Architecture-Notes-11-22", EntityType::Url),
                    ],
                ),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: vec![ReactionCounter {
                    emoji: "⚡".to_owned(),
                    count: 96,
                    chosen: false,
                }],
                views: Some(5_190),
                edited: false,
                is_read: false,
                is_sending: false,
                is_pinned: false,
            },
        ],
    );
    pinned.insert(devlog, vec![MsgId(301)]);
    first_unread.insert(devlog, Some(MsgId(302)));
    msg_media_map.insert((devlog, MsgId(301)), MOCK_PHOTO_AURORA_ID);

    // 5. Catalog Bot (demo bot chat with HTML5 game card, inline keyboard, reply keyboard, commands & inline results)
    let star_hopper_markup = tg_bots::markup::ReplyMarkupData::from_input(Some(
        tg_bots::markup::ReplyMarkupInput::Inline {
            rows: vec![vec![
                tg_bots::markup::MarkupButton::with_visual(
                    "Play Star Hopper",
                    tg_bots::markup::ButtonVisual {
                        icon_id: tg_bots::stand_in::DocumentId(0),
                        color: tg_bots::markup::ButtonColor::Primary,
                    },
                    tg_bots::markup::ButtonKind::Game,
                ),
                tg_bots::markup::MarkupButton::new(
                    "Share Score",
                    tg_bots::markup::ButtonKind::Callback {
                        data: b"share_game_score".to_vec(),
                    },
                ),
            ]],
            force_reply: false,
        },
    ));
    histories.insert(
        session_peers::BOT_CATALOG,
        vec![
            {
                let demo_html = "<title>Status Board Widget</title><div><h3>Cluster Health</h3><p>All 5 regions nominal — latency 18 ms</p></div>";
                let demo_payload = tg_tgw::pack_payload_zlib(demo_html.as_bytes());
                HistoryMessage {
                    extra: Default::default(),
                    id: MsgId(346),
                    peer_id: session_peers::BOT_CATALOG,
                    from_id: session_peers::BOT_CATALOG,
                    sender_name: "Catalog Bot".to_owned(),
                    date_unix: BASE_UNIX_TODAY + 10_000,
                    outgoing: false,
                    is_post: false,
                    text: TextWithEntities::default(),
                    media: Some(HistoryMedia::Widget(
                        tg_app_history::history::backend::WidgetAttachment {
                            widget_id: Some(88_001),
                            width: 320,
                            height: 170,
                            payload: demo_payload,
                            html: Some(demo_html.to_owned()),
                            title: "Status Board Widget".to_owned(),
                            preview_text: "Cluster Health All 5 regions nominal — latency 18 ms".to_owned(),
                            is_copy: false,
                            can_query: true,
                            background_query: false,
                            public_data: Some("{\"regions\":5,\"latency_ms\":18}".to_owned()),
                            public_data_hash: Some(4201),
                            chat_data: Some("{\"pinned\":true}".to_owned()),
                        },
                    )),
                    service: None,
                    reply: None,
                    forward: None,
                    reactions: Vec::new(),
                    views: None,
                    edited: false,
                    is_read: true,
                    is_sending: false,
                    is_pinned: false,
                }
            },
            HistoryMessage {
                extra: MessageExtra {
                    reply_markup: Some(star_hopper_markup),
                    ..MessageExtra::default()
                },
                id: MsgId(347),
                peer_id: session_peers::BOT_CATALOG,
                from_id: session_peers::BOT_CATALOG,
                sender_name: "Catalog Bot".to_owned(),
                date_unix: BASE_UNIX_TODAY + 10_020,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::default(),
                media: Some(HistoryMedia::Game(
                    tg_app_history::history::backend::GameAttachment {
                        game: tg_bots::mini_app::GameData {
                            id: tg_bots::stand_in::GameId(50_001),
                            access_hash: 0x5A17_CAFE,
                            short_name: "star_hopper".to_owned(),
                            title: "Star Hopper".to_owned(),
                            description: "Guide an orbital glider through asteroid rings and beat your group's high score.".to_owned(),
                            photo_id: Some(tg_bots::stand_in::PhotoId(50_100)),
                            document_id: Some(tg_bots::stand_in::DocumentId(50_101)),
                        },
                        bot_username: "catalog_demo_bot".to_owned(),
                        media_width: 640,
                        media_height: 360,
                        is_animation: true,
                        stripped_thumb: None,
                        fallback_rgb: ([0x1D, 0x2B, 0x64], [0x53, 0x35, 0x7A]),
                        high_scores: vec![
                            tg_bots::mini_app::GameHighScore {
                                pos: 1,
                                user_id: tg_bots::stand_in::UserId(session_peers::USER_MIRA.bare()),
                                score: 4_820,
                            },
                            tg_bots::mini_app::GameHighScore {
                                pos: 2,
                                user_id: tg_bots::stand_in::UserId(session_peers::SELF.bare()),
                                score: 3_950,
                            },
                            tg_bots::mini_app::GameHighScore {
                                pos: 3,
                                user_id: tg_bots::stand_in::UserId(session_peers::USER_THEO.bare()),
                                score: 2_740,
                            },
                        ],
                    },
                )),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
            },
            HistoryMessage {
                extra: Default::default(),
                id: MsgId(348),
                peer_id: session_peers::BOT_CATALOG,
                from_id: session_peers::SELF,
                sender_name: "Taylor Kim".to_owned(),
                date_unix: BASE_UNIX_TODAY + 10_080,
                outgoing: true,
                is_post: false,
                text: rich_text_with_entities("/catalog", &[("/catalog", EntityType::BotCommand)]),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
            },
            HistoryMessage {
                extra: MessageExtra {
                    reply_markup: Some(build_demo_inline_keyboard()),
                    ..MessageExtra::default()
                },
                id: MsgId(349),
                peer_id: session_peers::BOT_CATALOG,
                from_id: session_peers::BOT_CATALOG,
                sender_name: "Catalog Bot".to_owned(),
                date_unix: BASE_UNIX_TODAY + 10_140,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::simple(
                    "Interactive catalog controls — try callback buttons (toast, alert, 2FA), external links, Login via Telegram OAuth, Mini App, copy code, and inline search.",
                ),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
            },
            HistoryMessage {
                extra: MessageExtra {
                    reply_markup: Some(build_demo_reply_keyboard(true, false, true, false)),
                    ..MessageExtra::default()
                },
                id: MsgId(350),
                peer_id: session_peers::BOT_CATALOG,
                from_id: session_peers::BOT_CATALOG,
                sender_name: "Catalog Bot".to_owned(),
                date_unix: BASE_UNIX_TODAY + 10_200,
                outgoing: false,
                is_post: false,
                text: rich_text_with_entities(
                    "Welcome to Catalog Bot! Click \"Open\" in the chat list or \"Open App\" in Bot Info to launch the local WebView2 Mini App, or type /catalog.",
                    &[("/catalog", EntityType::BotCommand)],
                ),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
            },
        ],
    );

    // 6. Theo Park
    histories.insert(
        session_peers::USER_THEO,
        vec![HistoryMessage {
            extra: Default::default(),
            id: MsgId(401),
            peer_id: session_peers::USER_THEO,
            from_id: session_peers::USER_THEO,
            sender_name: "Theo Park".to_owned(),
            date_unix: BASE_UNIX_TODAY + 9_600,
            outgoing: false,
            is_post: false,
            text: TextWithEntities::simple(
                "AES-256-IGE and SRP test vectors are all green. Feel free to add me to contacts!",
            ),
            media: None,
            service: None,
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: true,
            is_sending: false,
            is_pinned: false,
        }],
    );

    // 7. Sam Chen
    histories.insert(
        session_peers::USER_SAM,
        vec![HistoryMessage {
            extra: Default::default(),
            id: MsgId(410),
            peer_id: session_peers::USER_SAM,
            from_id: session_peers::USER_SAM,
            sender_name: "Sam Chen".to_owned(),
            date_unix: BASE_UNIX_TODAY + 6_300,
            outgoing: false,
            is_post: false,
            text: TextWithEntities::simple(
                "Keyboard shortcuts Ctrl+F, Alt+Up/Down, Ctrl+Tab, Ctrl+1..9, and Esc all feel snappy.",
            ),
            media: None,
            service: None,
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: true,
            is_sending: false,
            is_pinned: false,
        }],
    );

    // 8. UI & Theme Lab
    histories.insert(
        session_peers::GROUP_UI_LAB,
        vec![
            HistoryMessage {
                extra: Default::default(),
                id: MsgId(420),
                peer_id: session_peers::GROUP_UI_LAB,
                from_id: session_peers::USER_MIRA,
                sender_name: "Mira Vance".to_owned(),
                date_unix: BASE_UNIX_YESTERDAY + 32_000,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::simple(
                    "Open Settings -> Chat Settings to test Day/Night/Tinted themes and live accent swatches #design",
                ),
                media: None,
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
            },
            HistoryMessage {
                extra: Default::default(),
                id: MsgId(421),
                peer_id: session_peers::GROUP_UI_LAB,
                from_id: session_peers::SELF,
                sender_name: "Taylor Kim".to_owned(),
                date_unix: BASE_UNIX_TODAY + 7_200,
                outgoing: true,
                is_post: false,
                text: TextWithEntities::default(),
                media: Some(HistoryMedia::TodoList(
                    tg_app_history::history::todo::TodoListAttachment {
                        title: TextWithEntities::simple("Layer-233 Release Checklist"),
                        items: vec![
                            tg_app_history::history::todo::TodoTaskItem {
                                id: 1,
                                text: TextWithEntities::simple("Audit palette contrast"),
                                completed_by: Some(session_peers::USER_MIRA),
                                completed_by_name: Some("Mira Vance".to_owned()),
                                completion_date: BASE_UNIX_TODAY + 7_500,
                            },
                            tg_app_history::history::todo::TodoTaskItem {
                                id: 2,
                                text: TextWithEntities::simple("Verify checklist toggle batching"),
                                completed_by: None,
                                completed_by_name: None,
                                completion_date: 0,
                            },
                            tg_app_history::history::todo::TodoTaskItem {
                                id: 3,
                                text: TextWithEntities::simple("Run full headless UI suite"),
                                completed_by: None,
                                completed_by_name: None,
                                completion_date: 0,
                            },
                        ],
                        others_can_append: true,
                        others_can_complete: true,
                    },
                )),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
            },
        ],
    );

    // 9. Riley Brooks
    histories.insert(
        session_peers::USER_RILEY,
        vec![HistoryMessage {
            extra: Default::default(),
            id: MsgId(430),
            peer_id: session_peers::USER_RILEY,
            from_id: session_peers::USER_RILEY,
            sender_name: "Riley Brooks".to_owned(),
            date_unix: BASE_UNIX_YESTERDAY + 23_400,
            outgoing: false,
            is_post: false,
            text: TextWithEntities::simple(
                "Waveform 5-bit packing and Ogg Opus mixer tests passed!",
            ),
            media: None,
            service: None,
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: true,
            is_sending: false,
            is_pinned: false,
        }],
    );

    // 10. Nightly Builds
    histories.insert(
        session_peers::CHANNEL_RELEASES,
        vec![HistoryMessage {
            extra: Default::default(),
            id: MsgId(440),
            peer_id: session_peers::CHANNEL_RELEASES,
            from_id: session_peers::CHANNEL_RELEASES,
            sender_name: "Nightly Builds".to_owned(),
            date_unix: BASE_UNIX_YESTERDAY - 3_600,
            outgoing: false,
            is_post: true,
            text: TextWithEntities::simple(
                "clippy -D warnings, cargo fmt --check, and cargo test -p tg-app passed on Windows x64.",
            ),
            media: None,
            service: None,
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: Some(612),
            edited: false,
            is_read: false,
            is_sending: false,
            is_pinned: false,
        }],
    );

    // 11. Archive Digest
    histories.insert(
        session_peers::CHANNEL_ARCHIVE,
        vec![HistoryMessage {
            extra: Default::default(),
            id: MsgId(450),
            peer_id: session_peers::CHANNEL_ARCHIVE,
            from_id: session_peers::CHANNEL_ARCHIVE,
            sender_name: "Archive Digest".to_owned(),
            date_unix: BASE_UNIX_YESTERDAY - 120_000,
            outgoing: false,
            is_post: true,
            text: TextWithEntities::simple("Archived milestone summary from earlier this month."),
            media: None,
            service: None,
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: Some(320),
            edited: false,
            is_read: false,
            is_sending: false,
            is_pinned: false,
        }],
    );

    (histories, pinned, first_unread, msg_media_map)
}

#[allow(clippy::type_complexity)]
fn build_initial_forum_and_replies() -> (
    HashMap<PeerId, Vec<ForumTopic>>,
    HashMap<(PeerId, MsgId), Vec<HistoryMessage>>,
    HashMap<(PeerId, MsgId), Vec<HistoryMessage>>,
) {
    let forum_id = session_peers::GROUP_CORE;
    let devlog = session_peers::CHANNEL_DEV_LOG;
    let self_id = session_peers::SELF;
    let mira = session_peers::USER_MIRA;
    let sam = session_peers::USER_SAM;
    let riley = session_peers::USER_RILEY;
    let theo = session_peers::USER_THEO;

    let mut forum_topics: HashMap<PeerId, Vec<ForumTopic>> = HashMap::new();
    forum_topics.insert(
        forum_id,
        vec![
            ForumTopic {
                root_id: FORUM_GENERAL_TOPIC_ID,
                peer_id: forum_id,
                title: "General".to_owned(),
                icon_color: FORUM_TOPIC_ICON_COLORS[0].0,
                custom_emoji: None,
                is_general: true,
                is_pinned: true,
                is_closed: false,
                muted: false,
                unread_count: 2,
                unread_mentions: 1,
                last_sender: "Mira Vance".to_owned(),
                last_preview: "Adaptive ThreeColumn layout matches window_session_controller.cpp!"
                    .to_owned(),
                last_date_unix: BASE_UNIX_TODAY + 15_480,
                draft_preview: None,
            },
            ForumTopic {
                root_id: MsgId(201),
                peer_id: forum_id,
                title: "UI & Theme Architecture".to_owned(),
                icon_color: FORUM_TOPIC_ICON_COLORS[2].0,
                custom_emoji: Some("🎨".to_owned()),
                is_general: false,
                is_pinned: true,
                is_closed: false,
                muted: false,
                unread_count: 3,
                unread_mentions: 0,
                last_sender: "Mira Vance".to_owned(),
                last_preview: "Updated Day/Night palette tokens and bubble rounding.".to_owned(),
                last_date_unix: BASE_UNIX_TODAY + 14_200,
                draft_preview: None,
            },
            ForumTopic {
                root_id: MsgId(202),
                peer_id: forum_id,
                title: "MTProto & Transport".to_owned(),
                icon_color: FORUM_TOPIC_ICON_COLORS[0].0,
                custom_emoji: None,
                is_general: false,
                is_pinned: false,
                is_closed: false,
                muted: false,
                unread_count: 1,
                unread_mentions: 0,
                last_sender: "Theo Park".to_owned(),
                last_preview: "PFS temporary key rotation test vectors passed.".to_owned(),
                last_date_unix: BASE_UNIX_TODAY + 11_400,
                draft_preview: None,
            },
            ForumTopic {
                root_id: MsgId(203),
                peer_id: forum_id,
                title: "Audio & Voice Pipeline".to_owned(),
                icon_color: FORUM_TOPIC_ICON_COLORS[3].0,
                custom_emoji: None,
                is_general: false,
                is_pinned: false,
                is_closed: false,
                muted: false,
                unread_count: 0,
                unread_mentions: 0,
                last_sender: "Riley Brooks".to_owned(),
                last_preview: "48kHz Opus voice capture + 5-bit waveform ready.".to_owned(),
                last_date_unix: BASE_UNIX_TODAY + 8_600,
                draft_preview: None,
            },
            ForumTopic {
                root_id: MsgId(205),
                peer_id: forum_id,
                title: "Release 0.1 Archive".to_owned(),
                icon_color: FORUM_TOPIC_ICON_COLORS[1].0,
                custom_emoji: None,
                is_general: false,
                is_pinned: false,
                is_closed: true,
                muted: true,
                unread_count: 0,
                unread_mentions: 0,
                last_sender: "Taylor Kim".to_owned(),
                last_preview: "Milestone 1 sign-off complete. Topic closed.".to_owned(),
                last_date_unix: BASE_UNIX_YESTERDAY + 10_000,
                draft_preview: None,
            },
        ],
    );

    let make_thread_msg = |id: MsgId,
                           peer_id: PeerId,
                           from_id: PeerId,
                           sender_name: &str,
                           date_unix: i32,
                           outgoing: bool,
                           text: &str,
                           thread_id: MsgId|
     -> HistoryMessage {
        HistoryMessage {
            id,
            peer_id,
            from_id,
            sender_name: sender_name.to_owned(),
            date_unix,
            outgoing,
            is_post: false,
            text: TextWithEntities::simple(text),
            media: None,
            service: None,
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: true,
            is_sending: false,
            is_pinned: false,
            extra: MessageExtra {
                thread_id: Some(thread_id),
                ..MessageExtra::default()
            },
        }
    };

    let mut topic_messages: HashMap<(PeerId, MsgId), Vec<HistoryMessage>> = HashMap::new();
    topic_messages.insert(
        (forum_id, FORUM_GENERAL_TOPIC_ID),
        vec![
            make_thread_msg(
                MsgId(8001),
                forum_id,
                sam,
                "Sam Chen",
                BASE_UNIX_TODAY + 10_200,
                false,
                "Welcome to the General topic! All core workspace crates compile cleanly.",
                FORUM_GENERAL_TOPIC_ID,
            ),
            make_thread_msg(
                MsgId(8002),
                forum_id,
                mira,
                "Mira Vance",
                BASE_UNIX_TODAY + 15_480,
                false,
                "Adaptive ThreeColumn layout matches window_session_controller.cpp!",
                FORUM_GENERAL_TOPIC_ID,
            ),
        ],
    );
    topic_messages.insert(
        (forum_id, MsgId(201)),
        vec![
            make_thread_msg(
                MsgId(8101),
                forum_id,
                mira,
                "Mira Vance",
                BASE_UNIX_TODAY + 12_000,
                false,
                "Drafting the new forum topic icons using the exact 6 tdesktop hex colors.",
                MsgId(201),
            ),
            make_thread_msg(
                MsgId(8102),
                forum_id,
                self_id,
                "Taylor Kim",
                BASE_UNIX_TODAY + 14_200,
                true,
                "Looks super sharp! Let's keep General pinned with the # badge.",
                MsgId(201),
            ),
        ],
    );
    topic_messages.insert(
        (forum_id, MsgId(202)),
        vec![make_thread_msg(
            MsgId(8201),
            forum_id,
            theo,
            "Theo Park",
            BASE_UNIX_TODAY + 11_400,
            false,
            "PFS temporary key rotation test vectors passed.",
            MsgId(202),
        )],
    );
    topic_messages.insert(
        (forum_id, MsgId(203)),
        vec![make_thread_msg(
            MsgId(8301),
            forum_id,
            riley,
            "Riley Brooks",
            BASE_UNIX_TODAY + 8_600,
            false,
            "48kHz Opus voice capture + 5-bit waveform ready.",
            MsgId(203),
        )],
    );
    topic_messages.insert(
        (forum_id, MsgId(205)),
        vec![make_thread_msg(
            MsgId(8501),
            forum_id,
            self_id,
            "Taylor Kim",
            BASE_UNIX_YESTERDAY + 10_000,
            true,
            "Milestone 1 sign-off complete. Topic closed.",
            MsgId(205),
        )],
    );

    let mut replies: HashMap<(PeerId, MsgId), Vec<HistoryMessage>> = HashMap::new();
    let mut r1 = make_thread_msg(
        MsgId(7101),
        devlog,
        sam,
        "Sam Chen",
        BASE_UNIX_YESTERDAY + 19_200,
        false,
        "Awesome Instant View writeup! Does the layout cache stay bounded on resize?",
        MsgId(301),
    );
    r1.reactions.push(ReactionCounter {
        emoji: "👍".to_owned(),
        count: 4,
        chosen: false,
    });
    let mut r2 = make_thread_msg(
        MsgId(7102),
        devlog,
        mira,
        "Mira Vance",
        BASE_UNIX_YESTERDAY + 20_100,
        false,
        "Verified 60fps scrolling with 5,000 messages — layout cache only touches visible rows.",
        MsgId(301),
    );
    r2.reply = Some(ReplyHeader {
        reply_to_msg_id: MsgId(7101),
        author_name: "Sam Chen".to_owned(),
        author_color_id: sam.bare(),
        preview_text: "Awesome Instant View writeup! Does the layout cache stay bounded on resize?"
            .to_owned(),
        preview_thumb_rgb: None,
        quote_text: None,
    });
    let r3 = make_thread_msg(
        MsgId(7103),
        devlog,
        self_id,
        "Taylor Kim",
        BASE_UNIX_TODAY + 3_600,
        true,
        "Thanks everyone! Forum topics and comments threads are now wired end-to-end.",
        MsgId(301),
    );
    replies.insert((devlog, MsgId(301)), vec![r1, r2, r3]);

    let r4 = make_thread_msg(
        MsgId(7201),
        devlog,
        riley,
        "Riley Brooks",
        BASE_UNIX_TODAY + 12_800,
        false,
        "The live Channel Statistics and Boosts charts look crisp in both Day and Night modes!",
        MsgId(302),
    );
    let r5 = make_thread_msg(
        MsgId(7202),
        devlog,
        theo,
        "Theo Park",
        BASE_UNIX_TODAY + 13_400,
        false,
        "Love having instant comments right under channel posts.",
        MsgId(302),
    );
    replies.insert((devlog, MsgId(302)), vec![r4, r5]);

    (forum_topics, topic_messages, replies)
}

fn build_initial_members() -> HashMap<PeerId, Vec<MemberEntry>> {
    let mut map = HashMap::new();
    let core_members = vec![
        MemberEntry {
            peer_id: session_peers::SELF,
            name: "Taylor Kim".to_owned(),
            username: Some("taylor_kim".to_owned()),
            status_text: "online".to_owned(),
            is_online: true,
            is_bot: false,
            role: MemberRole::Owner {
                custom_title: Some("maintainer".to_owned()),
            },
            color_index: 5,
        },
        MemberEntry {
            peer_id: session_peers::USER_MIRA,
            name: "Mira Vance".to_owned(),
            username: Some("mira_vance".to_owned()),
            status_text: "online".to_owned(),
            is_online: true,
            is_bot: false,
            role: MemberRole::Admin {
                custom_title: Some("UI & Shaders".to_owned()),
                rights: ChatAdminRights::all(),
            },
            color_index: 3,
        },
        MemberEntry {
            peer_id: session_peers::USER_SAM,
            name: "Sam Chen".to_owned(),
            username: Some("sam_chen".to_owned()),
            status_text: "online".to_owned(),
            is_online: true,
            is_bot: false,
            role: MemberRole::Admin {
                custom_title: Some("Navigation".to_owned()),
                rights: ChatAdminRights::all(),
            },
            color_index: 4,
        },
        MemberEntry {
            peer_id: session_peers::USER_THEO,
            name: "Theo Park".to_owned(),
            username: Some("theo_park".to_owned()),
            status_text: "last seen 12 minutes ago".to_owned(),
            is_online: false,
            is_bot: false,
            role: MemberRole::Member,
            color_index: 1,
        },
        MemberEntry {
            peer_id: session_peers::USER_RILEY,
            name: "Riley Brooks".to_owned(),
            username: Some("riley_brooks".to_owned()),
            status_text: "last seen recently".to_owned(),
            is_online: false,
            is_bot: false,
            role: MemberRole::Member,
            color_index: 6,
        },
        MemberEntry {
            peer_id: session_peers::BOT_CATALOG,
            name: "Catalog Bot".to_owned(),
            username: Some("catalog_demo_bot".to_owned()),
            status_text: "bot".to_owned(),
            is_online: false,
            is_bot: true,
            role: MemberRole::Member,
            color_index: 5,
        },
    ];
    map.insert(session_peers::GROUP_CORE, core_members);
    map.insert(
        session_peers::GROUP_UI_LAB,
        vec![
            MemberEntry {
                peer_id: session_peers::SELF,
                name: "Taylor Kim".to_owned(),
                username: Some("taylor_kim".to_owned()),
                status_text: "online".to_owned(),
                is_online: true,
                is_bot: false,
                role: MemberRole::Owner { custom_title: None },
                color_index: 5,
            },
            MemberEntry {
                peer_id: session_peers::USER_MIRA,
                name: "Mira Vance".to_owned(),
                username: Some("mira_vance".to_owned()),
                status_text: "online".to_owned(),
                is_online: true,
                is_bot: false,
                role: MemberRole::Admin {
                    custom_title: Some("Design".to_owned()),
                    rights: ChatAdminRights::all(),
                },
                color_index: 3,
            },
            MemberEntry {
                peer_id: session_peers::USER_SAM,
                name: "Sam Chen".to_owned(),
                username: Some("sam_chen".to_owned()),
                status_text: "online".to_owned(),
                is_online: true,
                is_bot: false,
                role: MemberRole::Member,
                color_index: 4,
            },
        ],
    );
    map
}

fn build_initial_media_items() -> Vec<ViewerMediaItem> {
    let base = MockMediaBackend::new();
    let mut items = Vec::new();
    let specs: [(MediaItemId, PeerId, i32, &str); 3] = [
        (
            MOCK_PHOTO_LANDSCAPE_ID,
            session_peers::GROUP_CORE,
            102,
            "Mira Vance",
        ),
        (
            MOCK_PHOTO_PORTRAIT_ID,
            session_peers::GROUP_CORE,
            1062,
            "Mira Vance",
        ),
        (
            MOCK_PHOTO_AURORA_ID,
            session_peers::GROUP_CORE,
            1063,
            "Rustgram Dev Log",
        ),
    ];
    for (id, peer, msg_id, sender) in specs {
        if let Some(mut item) = base.get_item(id) {
            item.peer_id = peer.0;
            item.msg_id = msg_id;
            item.sender_name = sender.to_owned();
            items.push(item);
        }
    }
    items
}

tg_app_info::delegate_marketplace_via_registry!(MockSession);

impl tg_app_core::session_api::AppSession for MockSession {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn sync_cached_user(&mut self) {
        MockSession::sync_cached_user(self);
    }

    fn sign_in_as(&mut self, user: AuthUser) {
        MockSession::sign_in_as(self, user);
    }

    fn resolve_username(&self, username: &str) -> Option<PeerId> {
        self.handle().resolve_username(username)
    }

    fn is_forum_peer(&self, peer_id: PeerId) -> bool {
        MockSession::is_forum_peer(self, peer_id)
    }

    fn resolve_media_item_id(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        album_index: Option<usize>,
    ) -> Option<tg_app_media::media::backend::MediaItemId> {
        Some(
            self.handle()
                .resolve_media_item_id(peer_id, msg_id, album_index),
        )
    }

    fn sample_voice_id(&self) -> Option<AudioMsgId> {
        self.handle().sample_voice_id()
    }

    fn sample_audio_id(&self) -> Option<AudioMsgId> {
        self.handle().sample_audio_id()
    }

    fn set_pending_mini_app_url(&mut self, bot_id: PeerId, url: Option<String>) {
        MockSession::set_pending_mini_app_url(self, bot_id, url);
    }

    fn take_pending_mini_app_url(&mut self, bot_id: PeerId) -> Option<String> {
        MockSession::take_pending_mini_app_url(self, bot_id)
    }

    fn is_mini_app_bot_trusted(&self, bot_id: PeerId) -> bool {
        self.handle().is_mini_app_bot_trusted(bot_id)
    }

    fn trust_mini_app_bot(&mut self, bot_id: PeerId) {
        self.handle().trust_mini_app_bot(bot_id);
    }

    fn take_pending_chat_opens(&self) -> Vec<PendingChatOpen> {
        self.handle().take_pending_chat_opens()
    }

    fn set_stats_target_peer(&self, peer_id: PeerId) {
        self.handle().set_stats_target_peer(peer_id);
    }

    fn send_helper_message(
        &mut self,
        peer_id: PeerId,
        text: String,
    ) -> Result<HistoryMessage, String> {
        self.handle().send_helper_message(peer_id, text)
    }

    fn create_settings_backend(
        &self,
    ) -> Box<dyn tg_app_settings::settings::backend::SettingsBackend> {
        Box::new(self.handle())
    }

    fn create_stats_backend(&self) -> Box<dyn tg_app_info::statistics::backend::StatsBackend> {
        Box::new(self.handle())
    }

    fn create_notify_backend(
        &self,
    ) -> Box<dyn tg_app_media::notifications::backend::NotifyBackend> {
        Box::new(self.handle())
    }

    fn create_webview_backend(&self) -> Box<dyn tg_app_media::webview::backend::WebViewBackend> {
        Box::new(self.handle())
    }

    fn create_calls_backend(&self) -> Box<dyn tg_app_media::calls::backend::CallsBackend> {
        Box::new(tg_app_media::calls::mock::MockCallsBackend::new())
    }

    fn create_stories_backend(&self) -> Box<dyn tg_app_media::stories::backend::StoriesBackend> {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs().min(i32::MAX as u64) as tg_data::TimeId)
            .unwrap_or(1_760_000_000);
        Box::new(tg_app_media::stories::mock::MockStoriesBackend::new(
            now_secs,
        ))
    }

    fn create_business_backend(
        &self,
    ) -> Box<dyn tg_app_history::business::backend::BusinessBackend> {
        Box::new(tg_app_history::business::mock::MockBusinessBackend::new())
    }

    fn create_payments_backend(&self) -> Box<dyn tg_app_info::payments::backend::PaymentsBackend> {
        Box::new(tg_app_info::payments::mock::MockPaymentsBackend::new())
    }

    fn create_passport_backend(&self) -> Box<dyn tg_app_info::payments::backend::PassportBackend> {
        Box::new(tg_app_info::payments::mock::MockPassportBackend::new())
    }

    fn confirm_phone_code(
        &mut self,
        phone_code_hash: &str,
        phone_code: &str,
    ) -> Result<bool, String> {
        let code = phone_code.trim();
        if code.is_empty() {
            return Err("PHONE_CODE_EMPTY".to_owned());
        }
        if code == "00000" {
            return Err("PHONE_CODE_INVALID".to_owned());
        }
        if let Ok(mut st) = self.handle.state.lock() {
            st.confirmed_phone_codes
                .push((phone_code_hash.to_owned(), code.to_owned()));
        }
        Ok(true)
    }
}

#[cfg(test)]
mod app_session_object_safety_tests {
    use super::*;
    use tg_app_core::session_api::AppSession;

    fn assert_object_safe(_: &mut dyn AppSession) {}

    #[test]
    fn mock_session_implements_object_safe_app_session() {
        let mut mock = MockSession::new(true);
        assert_object_safe(&mut mock);
        assert!(!mock.is_live_session());
        assert!(mock.resolve_username("@rustgram_log").is_some());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_session_shares_one_coherent_dataset_across_all_screen_backends() {
        let mut session = MockSession::new(true);
        let handle = session.handle();

        // 1. Sending a message via HistoryBackend updates DialogsBackend immediately.
        let sent = HistoryBackend::send_message(
            &mut session,
            session_peers::GROUP_CORE,
            SendMessageRequest {
                text: TextWithEntities::simple("Coherent session test message"),
                reply_to: None,
                silent: false,
            },
        )
        .expect("send message in group");
        let dialog = DialogsBackend::dialog_by_peer(&handle, session_peers::GROUP_CORE)
            .expect("group dialog exists");
        assert_eq!(dialog.last_message_id, sent.id);
        assert!(
            dialog
                .last_message
                .contains("Coherent session test message")
        );
        assert_eq!(dialog.badges.unread_counter, 0);

        // 2. Toggling mute via InfoBackend updates DialogsBackend & HistoryBackend & NotifyBackend.
        let muted = InfoBackend::toggle_notifications(&mut session, session_peers::USER_MIRA);
        assert!(muted);
        assert!(
            DialogsBackend::dialog_by_peer(&handle, session_peers::USER_MIRA)
                .unwrap()
                .muted
        );
        assert!(
            HistoryBackend::peer_info(&handle, session_peers::USER_MIRA)
                .unwrap()
                .summary
                .muted
        );

        // 3. Creating a group via InfoBackend appears in DialogsBackend & HistoryBackend.
        let new_group = InfoBackend::create_group(
            &mut session,
            CreateGroupRequest {
                title: "Unified Wiring QA".to_owned(),
                member_ids: vec![session_peers::USER_MIRA, session_peers::USER_SAM],
                auto_delete_ttl_seconds: 0,
                has_custom_photo: false,
            },
        )
        .expect("create group");
        assert!(DialogsBackend::dialog_by_peer(&handle, new_group).is_some());
        assert_eq!(
            HistoryBackend::peer_info(&handle, new_group)
                .unwrap()
                .summary
                .title,
            "Unified Wiring QA"
        );

        // 4. Editing profile name via SettingsBackend updates Backend::current_user().
        SettingsBackend::update_name(&mut session, "Jordan", "Ellis").expect("update name");
        assert_eq!(
            Backend::current_user(&session).unwrap().name,
            "Jordan Ellis"
        );

        // 5. Media viewer resolves siblings from the same chat; StatsBackend targets channel peer.
        let media_id = handle.resolve_media_item_id(session_peers::GROUP_CORE, MsgId(102), None);
        let siblings =
            MediaBackend::siblings_in_chat(&session, session_peers::GROUP_CORE.0, media_id);
        assert_eq!(siblings.len(), 3);
        handle.set_stats_target_peer(session_peers::CHANNEL_DEV_LOG);
        let stats = StatsBackend::channel_statistics(&session).expect("channel stats");
        assert_eq!(stats.peer.title, "Rustgram Dev Log");

        // 6. WebView serves Instant View & Mini App for Catalog Bot.
        assert!(
            WebViewBackend::fetch_iv_page(&session, SAMPLE_IV_URL, None)
                .unwrap()
                .is_some()
        );
        let launch = WebViewBackend::request_web_view(
            &mut session,
            WebViewRequest {
                bot_id: session_peers::BOT_CATALOG.bare(),
                start_param: None,
                url_override: None,
                fullscreen: false,
                theme_params_json: "{}".to_owned(),
            },
        )
        .expect("catalog mini app");
        assert_eq!(launch.bot.title, "Catalog Bot");

        // 7. ChatHelpers autocomplete includes the same session users & Catalog Bot commands.
        assert!(
            ChatHelpersBackend::autocomplete_users(&session)
                .iter()
                .any(|u| u.username == "mira_vance")
        );

        // 8. Notification open_chat records pending navigation and quick reply appends to history.
        let before_len = HistoryBackend::load_slice_around(
            &session,
            session_peers::USER_MIRA,
            MsgId::SHOW_AT_END,
            50,
            0,
        )
        .messages
        .len();
        NotifyBackend::send_quick_reply(
            &mut session,
            1,
            session_peers::USER_MIRA,
            MsgId(0),
            MsgId(202),
            "Quick reply from notification".to_owned(),
        );
        let after_len = HistoryBackend::load_slice_around(
            &session,
            session_peers::USER_MIRA,
            MsgId::SHOW_AT_END,
            50,
            0,
        )
        .messages
        .len();
        assert_eq!(after_len, before_len + 1);
        NotifyBackend::open_chat(
            &mut session,
            1,
            session_peers::USER_MIRA,
            MsgId(0),
            MsgId(202),
            None,
        );
        let opens = handle.take_pending_chat_opens();
        assert_eq!(opens.len(), 1);
        assert_eq!(opens[0].peer_id, session_peers::USER_MIRA);
    }
}
