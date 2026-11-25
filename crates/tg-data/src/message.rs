//! Port of `history/history_item.h`, `data/data_types.h` (`MessageFlag`, `DocumentType`),
//! `data/data_media_types.h`, and Telegram layer 233 `MessageMedia` / `MessageAction`
//! constructors from `schema/api.tl`.
//!
//! Defines the complete TL-independent message domain model: message flags,
//! content & media enums, all 72 `messageAction*` service actions, replies,
//! forwards, reactions summary, albums, and metadata.

use bitflags::bitflags;

use crate::id::{
    BotAppId, BusinessShortcutId, CallId, ChannelId, ChatId, DocumentId, EffectId, FullMsgId,
    FullStoryId, GameId, MessageGroupId, MsgId, PeerId, PhotoId, PollId, StoryId, TimeId, UserId,
    WallPaperId, WebPageId,
};
use crate::peer::{Birthday, GeoPoint, ReactionId};
use crate::rights::ChatAdminRights;
use crate::text::TextWithEntities;

bitflags! {
    /// 64-bit message flags (`MessageFlag` in `data/data_types.h`).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct MessageFlags: u64 {
        /// Hide the "edited" badge even when `edit_date` is set.
        const HIDE_EDITED               = 1 << 0;
        /// Legacy channel post / message.
        const LEGACY                    = 1 << 1;
        /// Has a reply markup (inline or reply keyboard).
        const HAS_REPLY_MARKUP          = 1 << 2;
        /// Has an explicit `from_id`.
        const HAS_FROM_ID               = 1 << 3;
        /// Has a `post_author` signature.
        const HAS_POST_AUTHOR           = 1 << 4;
        /// Has a view counter.
        const HAS_VIEWS                 = 1 << 5;
        /// Has reply/comments thread info.
        const HAS_REPLY_INFO            = 1 << 6;
        /// Viewer can see the list of reactors.
        const CAN_VIEW_REACTIONS        = 1 << 7;
        /// Synthetic admin-log entry.
        const ADMIN_LOG_ENTRY           = 1 << 8;
        /// Channel broadcast post.
        const POST                      = 1 << 9;
        /// Sent silently without notification sound.
        const SILENT                    = 1 << 10;
        /// Outgoing message sent by the current account.
        const OUTGOING                  = 1 << 11;
        /// Pinned in chat/topic.
        const PINNED                    = 1 << 12;
        /// Unplayed voice/round-video or unread media content.
        const MEDIA_IS_UNREAD           = 1 << 13;
        /// Has unread reactions.
        const HAS_UNREAD_REACTION       = 1 << 14;
        /// Mentions the current account.
        const MENTIONS_ME               = 1 << 15;
        /// Is scheduled or was delivered from a scheduled message.
        const IS_OR_WAS_SCHEDULED       = 1 << 16;
        /// Forwarding/saving this message is forbidden.
        const NO_FORWARDS               = 1 << 17;
        /// Display media above text caption.
        const INVERT_MEDIA              = 1 << 18;
        /// Contains a switch-inline button.
        const HAS_SWITCH_INLINE_BUTTON  = 1 << 19;
        /// Contains hyperlinks for shared-links indexing.
        const HAS_TEXT_LINKS            = 1 << 20;
        /// Group/channel create or migrate service message.
        const IS_GROUP_ESSENTIAL        = 1 << 21;
        /// Client-side edited media pending server confirmation.
        const IS_LOCAL_UPDATE_MEDIA     = 1 << 22;
        /// Sent via inline bot.
        const FROM_INLINE_BOT           = 1 << 23;
        /// Client-side synthetic unread message.
        const CLIENT_SIDE_UNREAD        = 1 << 24;
        /// Sender has admin badge in supergroup.
        const HAS_ADMIN_BADGE           = 1 << 25;
        /// Outgoing message currently being uploaded/sent.
        const BEING_SENT                = 1 << 26;
        /// Outgoing message failed to send.
        const SENDING_FAILED            = 1 << 27;
        /// Emoji interaction animation watched.
        const EMOJI_INTERACTION_WATCHED = 1 << 28;
        /// Exists in main message history.
        const HISTORY_ENTRY             = 1 << 29;
        /// Local message not on server.
        const LOCAL                     = 1 << 30;
        /// Fake message for UI preview.
        const FAKE_HISTORY_ITEM         = 1 << 31;
        /// Contact sign-up service message.
        const IS_CONTACT_SIGN_UP        = 1 << 32;
        /// Custom emoji repainting hint.
        const CUSTOM_EMOJI_REPAINTING   = 1 << 33;
        /// Profile photo suggestion service message.
        const IS_USERPIC_SUGGESTION     = 1 << 34;
        /// Text consists solely of emoji and whitespace.
        const ONLY_EMOJI_AND_SPACES     = 1 << 35;
        /// Cached flag indicating `ONLY_EMOJI_AND_SPACES` was computed.
        const ONLY_EMOJI_AND_SPACES_SET = 1 << 36;
        /// Fake bot about-view message.
        const FAKE_ABOUT_VIEW           = 1 << 37;
        /// Story wrapper message.
        const STORY_ITEM                = 1 << 38;
        /// Currently highlighted by jump-to-message animation.
        const IN_HIGHLIGHT_PROCESS      = 1 << 39;
        /// Display-from cache checked.
        const DISPLAY_FROM_CHECKED      = 1 << 40;
        /// Display-from profile mode.
        const DISPLAY_FROM_PROFILES     = 1 << 41;
        /// Show similar channels below message.
        const SHOW_SIMILAR_CHANNELS     = 1 << 42;
        /// Sponsored ad message.
        const SPONSORED                 = 1 << 43;
        /// Reactions act as personal Saved Messages tags.
        const REACTIONS_ARE_TAGS        = 1 << 44;
        /// Business quick-reply shortcut message.
        const SHORTCUT_MESSAGE          = 1 << 45;
        /// Message effect animation watched.
        const EFFECT_WATCHED            = 1 << 46;
        /// Contains sensitive content.
        const SENSITIVE_CONTENT         = 1 << 47;
        /// Has server restriction reasons.
        const HAS_RESTRICTIONS          = 1 << 48;
        /// Date is estimated rather than authoritative.
        const ESTIMATED_DATE            = 1 << 49;
        /// Reactions are allowed on this message.
        const REACTIONS_ALLOWED         = 1 << 50;
        /// Hide date divider for this item.
        const HIDE_DISPLAY_DATE         = 1 << 51;
        /// Suggested post priced in Telegram Stars.
        const STARS_PAID_SUGGESTED      = 1 << 52;
        /// Suggested post priced in TON.
        const TON_PAID_SUGGESTED        = 1 << 53;
        /// Story displayed in profile grid.
        const STORY_IN_PROFILE          = 1 << 54;
        /// Saved profile music item.
        const SAVED_MUSIC_ITEM          = 1 << 55;
        /// Contains hidden links.
        const HAS_HIDDEN_LINKS          = 1 << 56;
        /// Has AI summary entry.
        const HAS_SUMMARY_ENTRY         = 1 << 57;
        /// Can be summarized.
        const CAN_BE_SUMMARIZED         = 1 << 58;
        /// Has unread poll votes.
        const HAS_UNREAD_POLL_VOTE      = 1 << 59;
        /// Streaming draft text appearing.
        const TEXT_APPEARING            = 1 << 60;
        /// Streaming draft text appearing started.
        const TEXT_APPEARING_STARTED    = 1 << 61;
        /// Guest chat via-from flag.
        const GUEST_CHAT_VIA_FROM       = 1 << 62;
        /// Ephemeral / self-destructing message.
        const EPHEMERAL                 = 1 << 63;
    }
}

bitflags! {
    /// Webpage preview display flags (`MediaWebPageFlag` in `data/data_types.h`).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct MediaWebPageFlags: u8 {
        /// Force large preview image.
        const FORCE_LARGE_MEDIA = 1 << 0;
        /// Force small preview thumbnail.
        const FORCE_SMALL_MEDIA = 1 << 1;
        /// Link preview was chosen manually.
        const MANUAL            = 1 << 2;
        /// Safe / verified preview.
        const SAFE              = 1 << 3;
        /// Sponsored preview.
        const SPONSORED         = 1 << 4;
    }
}

/// Stable serialization discriminant matching tdesktop's `enum DocumentType` (`data_types.h`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
#[repr(u8)]
pub enum DocumentType {
    /// Generic file attachment (`0`).
    #[default]
    File = 0,
    /// Video file (`1`).
    Video = 1,
    /// Audio music track (`2`).
    Song = 2,
    /// Sticker (`3`).
    Sticker = 3,
    /// Looping GIF / silent MP4 animation (`4`).
    Animated = 4,
    /// Voice note (`5`).
    Voice = 5,
    /// Circular video note (`6`).
    RoundVideo = 6,
    /// Chat wallpaper document (`7`).
    WallPaper = 7,
}

/// Rich domain classification of a document attachment.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum DocumentKind {
    /// Generic file.
    #[default]
    File,
    /// Regular video.
    Video,
    /// Circular round video message.
    RoundVideo,
    /// Voice note.
    Voice,
    /// Audio track / music.
    Audio,
    /// Static WEBP sticker.
    Sticker,
    /// Lottie `.tgs` animated sticker.
    AnimatedSticker,
    /// WEBM video sticker.
    VideoSticker,
    /// GIF / muted looping animation.
    Gif,
    /// Wallpaper document.
    WallPaper,
}

impl DocumentKind {
    /// Maps this `DocumentKind` to tdesktop's serialized `DocumentType` discriminant.
    #[inline]
    pub const fn document_type(self) -> DocumentType {
        match self {
            Self::File => DocumentType::File,
            Self::Video => DocumentType::Video,
            Self::RoundVideo => DocumentType::RoundVideo,
            Self::Voice => DocumentType::Voice,
            Self::Audio => DocumentType::Song,
            Self::Sticker | Self::AnimatedSticker | Self::VideoSticker => DocumentType::Sticker,
            Self::Gif => DocumentType::Animated,
            Self::WallPaper => DocumentType::WallPaper,
        }
    }
}

/// Photo size descriptor.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct PhotoSize {
    /// Telegram size type tag (e.g. `"s"`, `"m"`, `"x"`, `"y"`, `"i"`).
    pub kind: String,
    /// Pixel width.
    pub width: i32,
    /// Pixel height.
    pub height: i32,
    /// Byte size (if known).
    pub size: i64,
}

/// Cloud photo attachment metadata.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct PhotoData {
    /// Photo ID.
    pub id: PhotoId,
    /// Server access hash.
    pub access_hash: u64,
    /// File reference bytes.
    pub file_reference: Vec<u8>,
    /// Upload timestamp.
    pub date: TimeId,
    /// Hosting DC ID.
    pub dc_id: i32,
    /// Available thumbnail / full sizes.
    pub sizes: Vec<PhotoSize>,
    /// Optional stripped inline JPEG thumbnail bytes (`photoStrippedSize`).
    pub stripped_thumb: Option<Vec<u8>>,
    /// Whether the photo has embedded stickers.
    pub has_stickers: bool,
    /// Whether the photo is hidden behind a spoiler blur.
    pub spoiler: bool,
    /// Optional self-destruct TTL in seconds.
    pub ttl_seconds: Option<i32>,
    /// Optional live-photo companion video document ID.
    pub live_photo_video_id: Option<DocumentId>,
}

/// Sticker metadata attached to sticker documents.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct StickerMeta {
    /// Associated emoji string.
    pub alt: String,
    /// Owning sticker set ID (`0` if none).
    pub set_id: u64,
    /// Owning sticker set access hash.
    pub set_access_hash: u64,
    /// Whether this sticker is a mask.
    pub is_mask: bool,
}

/// Cloud document attachment metadata.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocumentData {
    /// Document ID.
    pub id: DocumentId,
    /// Server access hash.
    pub access_hash: u64,
    /// File reference bytes.
    pub file_reference: Vec<u8>,
    /// Upload timestamp.
    pub date: TimeId,
    /// MIME type string (e.g. `"video/mp4"`, `"audio/ogg"`, `"application/x-tgsticker"`).
    pub mime_type: String,
    /// File size in bytes.
    pub size: i64,
    /// Hosting DC ID.
    pub dc_id: i32,
    /// Available thumbnail sizes.
    pub thumbs: Vec<PhotoSize>,
    /// Optional stripped inline JPEG thumbnail bytes (`photoStrippedSize`).
    pub stripped_thumb: Option<Vec<u8>>,
    /// Classified document kind.
    pub kind: DocumentKind,
    /// Original filename (if present).
    pub file_name: String,
    /// Duration in seconds (for audio, voice, video, round video, GIF).
    pub duration: Option<f64>,
    /// Pixel width (if visual media).
    pub width: Option<i32>,
    /// Pixel height (if visual media).
    pub height: Option<i32>,
    /// Audio track title (for `DocumentKind::Audio`).
    pub audio_title: Option<String>,
    /// Audio performer/artist (for `DocumentKind::Audio`).
    pub audio_performer: Option<String>,
    /// Packed 5-bit voice waveform bytes (for `DocumentKind::Voice`).
    pub waveform: Option<Vec<u8>>,
    /// Sticker metadata (for sticker kinds).
    pub sticker: Option<StickerMeta>,
    /// Whether video supports streaming playback.
    pub supports_streaming: bool,
    /// Whether hidden behind a spoiler blur.
    pub spoiler: bool,
    /// Optional self-destruct TTL in seconds.
    pub ttl_seconds: Option<i32>,
    /// Optional video timestamp offset in seconds.
    pub video_timestamp: Option<i32>,
}

/// Webpage link preview metadata (`WebPageData`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WebPageData {
    /// Webpage ID.
    pub id: WebPageId,
    /// Canonical URL.
    pub url: String,
    /// Short display URL.
    pub display_url: String,
    /// Webpage type (e.g. `"article"`, `"photo"`, `"video"`, `"telegram_channel"`).
    pub kind: String,
    /// Site name.
    pub site_name: Option<String>,
    /// Page title.
    pub title: Option<String>,
    /// Page description.
    pub description: Option<String>,
    /// Preview photo (if any).
    pub photo: Option<PhotoData>,
    /// Preview document (if any).
    pub document: Option<DocumentData>,
    /// Display flags.
    pub flags: MediaWebPageFlags,
}

/// A single answer option in a poll.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct PollAnswer {
    /// Answer text with entities.
    pub text: TextWithEntities,
    /// Server-assigned opaque option bytes.
    pub option: Vec<u8>,
}

/// Per-option vote tally in `PollResults`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct PollAnswerVoters {
    /// Option bytes matching `PollAnswer::option`.
    pub option: Vec<u8>,
    /// Vote count for this option.
    pub voters: i32,
    /// Whether the current user voted for this option.
    pub chosen: bool,
    /// Whether this option is the correct quiz answer.
    pub correct: bool,
}

/// Aggregated poll voting results.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct PollResults {
    /// Per-option tallies.
    pub results: Vec<PollAnswerVoters>,
    /// Total voter count.
    pub total_voters: Option<i32>,
    /// Recent voter peer IDs.
    pub recent_voters: Vec<PeerId>,
    /// Quiz solution explanation text.
    pub solution: Option<TextWithEntities>,
}

/// Poll media payload (`PollData`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct PollData {
    /// Poll ID.
    pub id: PollId,
    /// Poll question with entities.
    pub question: TextWithEntities,
    /// Answer choices.
    pub answers: Vec<PollAnswer>,
    /// Whether voting is closed.
    pub closed: bool,
    /// Whether voter identities are public.
    pub public_voters: bool,
    /// Whether multiple answers may be selected.
    pub multiple_choice: bool,
    /// Whether this poll is a quiz with a correct answer.
    pub quiz: bool,
    /// Optional countdown duration in seconds.
    pub close_period: Option<i32>,
    /// Optional close timestamp.
    pub close_date: Option<TimeId>,
    /// Current results.
    pub results: PollResults,
}

/// Contact card media payload.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct ContactData {
    /// Phone number.
    pub phone_number: String,
    /// First name.
    pub first_name: String,
    /// Last name.
    pub last_name: String,
    /// Optional vCard payload.
    pub vcard: String,
    /// Associated Telegram user ID (`UserId::ZERO` if unregistered).
    pub user_id: UserId,
}

/// Venue (named place) media payload.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VenueData {
    /// Coordinates.
    pub geo: GeoPoint,
    /// Venue title.
    pub title: String,
    /// Street address.
    pub address: String,
    /// Provider name (e.g. `"foursquare"`).
    pub provider: String,
    /// Provider-specific venue ID.
    pub venue_id: String,
    /// Venue category type.
    pub venue_type: String,
}

/// Live-updating location media payload.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveLocationData {
    /// Current coordinates.
    pub geo: GeoPoint,
    /// Optional compass heading in degrees (`1..=360`).
    pub heading: Option<i32>,
    /// Sharing duration in seconds.
    pub period: i32,
    /// Optional proximity alert radius in meters.
    pub proximity_notification_radius: Option<i32>,
}

/// Animated dice / dart / slot / basketball media payload.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct DiceData {
    /// Rolled outcome value.
    pub value: i32,
    /// Dice emoji (e.g. `"🎲"`, `"🎯"`, `"🏀"`, `"⚽"`, `"🎰"`, `"🎳"`).
    pub emoticon: String,
}

/// Bot HTML5 game media payload.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GameData {
    /// Game ID.
    pub id: GameId,
    /// Access hash.
    pub access_hash: u64,
    /// Short name used in deep links.
    pub short_name: String,
    /// Game title.
    pub title: String,
    /// Game description.
    pub description: String,
    /// Preview photo.
    pub photo: Option<PhotoData>,
    /// Optional preview animation document.
    pub document: Option<DocumentData>,
}

/// Extended paid/invoice media entry (`MessageExtendedMedia`).
#[derive(Clone, Debug, PartialEq)]
pub enum ExtendedMedia {
    /// Locked preview before payment.
    Preview {
        /// Preview width.
        width: Option<i32>,
        /// Preview height.
        height: Option<i32>,
        /// Optional stripped JPEG thumbnail bytes.
        thumb: Option<Vec<u8>>,
        /// Optional duration in seconds.
        duration: Option<i32>,
    },
    /// Unlocked full media.
    Media(Box<MessageMedia>),
}

/// Payment invoice media payload.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InvoiceData {
    /// Product title.
    pub title: String,
    /// Product description.
    pub description: String,
    /// Currency ISO-4217 code (or `"XTR"` for Telegram Stars).
    pub currency: String,
    /// Total price in smallest currency units.
    pub total_amount: i64,
    /// Deep-link start parameter.
    pub start_param: String,
    /// Receipt message ID once paid.
    pub receipt_msg_id: Option<MsgId>,
    /// Whether shipping address is requested.
    pub shipping_address_requested: bool,
    /// Whether this is a test invoice.
    pub test: bool,
    /// Optional locked/unlocked extended media.
    pub extended_media: Option<Box<ExtendedMedia>>,
}

/// Forwarded / mentioned story media payload.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct StoryMediaData {
    /// Story owner peer ID.
    pub peer_id: PeerId,
    /// Story ID.
    pub story_id: StoryId,
    /// Whether attached via story mention.
    pub via_mention: bool,
}

/// Telegram Premium / Stars giveaway media payload.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct GiveawayData {
    /// Participating channel IDs.
    pub channels: Vec<ChannelId>,
    /// Eligible country ISO-2 codes (empty = worldwide).
    pub countries_iso2: Vec<String>,
    /// Optional custom prize description.
    pub prize_description: Option<String>,
    /// Number of winners / prizes.
    pub quantity: i32,
    /// Premium duration in months (if Premium giveaway).
    pub months: Option<i32>,
    /// Total Telegram Stars prize pool (if Stars giveaway).
    pub stars: Option<i64>,
    /// Giveaway end timestamp.
    pub until_date: TimeId,
    /// Only new subscribers eligible.
    pub only_new_subscribers: bool,
    /// Winners list will be publicly visible.
    pub winners_are_visible: bool,
}

/// Giveaway results media payload.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct GiveawayResultsData {
    /// Host channel ID.
    pub channel_id: ChannelId,
    /// Additional participating peer count.
    pub additional_peers_count: i32,
    /// Original giveaway launch message ID.
    pub launch_msg_id: MsgId,
    /// Number of winners selected.
    pub winners_count: i32,
    /// Number of unclaimed prizes.
    pub unclaimed_count: i32,
    /// Winning user IDs.
    pub winners: Vec<UserId>,
    /// Premium months per winner.
    pub months: Option<i32>,
    /// Total Stars prize pool.
    pub stars: Option<i64>,
    /// Optional custom prize description.
    pub prize_description: Option<String>,
    /// End timestamp.
    pub until_date: TimeId,
    /// Only new subscribers were eligible.
    pub only_new_subscribers: bool,
    /// Whether the giveaway was refunded.
    pub refunded: bool,
}

/// Paid Stars media album payload (`messageMediaPaidMedia`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaidMediaData {
    /// Price in Telegram Stars to unlock.
    pub stars_amount: i64,
    /// Locked previews or unlocked media items.
    pub extended_media: Vec<ExtendedMedia>,
}

/// Single task entry in a collaborative to-do list.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct TodoItemData {
    /// Task ID within the list.
    pub id: i32,
    /// Task text with entities.
    pub title: TextWithEntities,
}

/// Completion state of a to-do task.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct TodoCompletionData {
    /// Task ID.
    pub id: i32,
    /// User who completed the task.
    pub completed_by: PeerId,
    /// Completion timestamp.
    pub date: TimeId,
}

/// Collaborative to-do list media payload (`messageMediaToDo`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct TodoListData {
    /// List title.
    pub title: TextWithEntities,
    /// Task items.
    pub items: Vec<TodoItemData>,
    /// Completions recorded so far.
    pub completions: Vec<TodoCompletionData>,
    /// Whether others can append tasks.
    pub others_can_append: bool,
    /// Whether others can complete tasks.
    pub others_can_complete: bool,
}

/// Video stream media payload (`messageMediaVideoStream`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct VideoStreamData {
    /// Group call ID.
    pub call_id: CallId,
    /// Group call access hash.
    pub access_hash: u64,
    /// Whether this is an RTMP stream.
    pub rtmp_stream: bool,
}

/// Interactive message widget payload (`messageMediaWidget`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct WidgetData {
    /// Optional widget ID.
    pub widget_id: Option<i64>,
    /// Widget width.
    pub width: i32,
    /// Widget height.
    pub height: i32,
    /// Opaque widget payload bytes.
    pub payload: Vec<u8>,
    /// Whether this is a copy.
    pub is_copy: bool,
}

/// All Telegram layer 233 `MessageMedia` attachment variants.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum MessageMedia {
    /// No media (`messageMediaEmpty`).
    #[default]
    Empty,
    /// Photo (`messageMediaPhoto`).
    Photo(Box<PhotoData>),
    /// Document (`messageMediaDocument`).
    Document(Box<DocumentData>),
    /// Webpage preview (`messageMediaWebPage`).
    WebPage(Box<WebPageData>),
    /// Poll or quiz (`messageMediaPoll`).
    Poll(Box<PollData>),
    /// Contact card (`messageMediaContact`).
    Contact(Box<ContactData>),
    /// Static location (`messageMediaGeo`).
    Location(GeoPoint),
    /// Named venue (`messageMediaVenue`).
    Venue(Box<VenueData>),
    /// Live location (`messageMediaGeoLive`).
    LiveLocation(Box<LiveLocationData>),
    /// Animated dice (`messageMediaDice`).
    Dice(DiceData),
    /// Bot HTML5 game (`messageMediaGame`).
    Game(Box<GameData>),
    /// Payment invoice (`messageMediaInvoice`).
    Invoice(Box<InvoiceData>),
    /// Story share/mention (`messageMediaStory`).
    Story(StoryMediaData),
    /// Giveaway (`messageMediaGiveaway`).
    Giveaway(Box<GiveawayData>),
    /// Giveaway results (`messageMediaGiveawayResults`).
    GiveawayResults(Box<GiveawayResultsData>),
    /// Paid Stars media (`messageMediaPaidMedia`).
    PaidMedia(Box<PaidMediaData>),
    /// Collaborative to-do list (`messageMediaToDo`).
    TodoList(Box<TodoListData>),
    /// Live video stream (`messageMediaVideoStream`).
    VideoStream(VideoStreamData),
    /// Interactive widget (`messageMediaWidget`).
    Widget(Box<WidgetData>),
    /// Unsupported media from a newer API layer (`messageMediaUnsupported`).
    Unsupported,
}

/// Reason a phone call ended (`PhoneCallDiscardReason`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum PhoneCallDiscardReason {
    /// Missed call.
    Missed,
    /// Disconnected due to network error.
    Disconnect,
    /// Normal hangup.
    Hangup,
    /// Callee was busy / declined.
    Busy,
    /// Migrated to conference call.
    MigrateConferenceCall,
}

/// Stars or TON monetary amount (`StarsAmount` in `schema/api.tl`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct StarsAmount {
    /// Whole Stars or TON units.
    pub amount: i64,
    /// Fractional nanos (`0..1_000_000_000`).
    pub nanos: i32,
    /// `true` if denominated in TON (`starsTonAmount`), `false` for Telegram Stars.
    pub is_ton: bool,
}

/// Complete enumeration of all 72 `messageAction*` service message constructors
/// in Telegram API layer 233 (`schema/api.tl`).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum ServiceAction {
    /// `messageActionEmpty`.
    #[default]
    Empty,
    /// `messageActionChatCreate`.
    ChatCreate {
        /// Initial group title.
        title: String,
        /// Initial participant user IDs.
        users: Vec<UserId>,
    },
    /// `messageActionChatEditTitle`.
    ChatEditTitle {
        /// New title.
        title: String,
    },
    /// `messageActionChatEditPhoto`.
    ChatEditPhoto {
        /// New chat photo (optional if empty).
        photo: Option<Box<PhotoData>>,
    },
    /// `messageActionChatDeletePhoto`.
    ChatDeletePhoto,
    /// `messageActionChatAddUser`.
    ChatAddUser {
        /// Added user IDs.
        users: Vec<UserId>,
    },
    /// `messageActionChatDeleteUser`.
    ChatDeleteUser {
        /// Removed / left user ID.
        user_id: UserId,
    },
    /// `messageActionChatJoinedByLink`.
    ChatJoinedByLink {
        /// Invite link creator user ID.
        inviter_id: UserId,
    },
    /// `messageActionChannelCreate`.
    ChannelCreate {
        /// Initial channel title.
        title: String,
    },
    /// `messageActionChatMigrateTo`.
    ChatMigrateTo {
        /// Destination megagroup channel ID.
        channel_id: ChannelId,
    },
    /// `messageActionChannelMigrateFrom`.
    ChannelMigrateFrom {
        /// Former basic group title.
        title: String,
        /// Former basic group chat ID.
        chat_id: ChatId,
    },
    /// `messageActionPinMessage`.
    PinMessage,
    /// `messageActionHistoryClear`.
    HistoryClear,
    /// `messageActionGameScore`.
    GameScore {
        /// Game ID.
        game_id: GameId,
        /// Achieved score.
        score: i32,
    },
    /// `messageActionPaymentSentMe`.
    PaymentSentMe {
        /// Currency code.
        currency: String,
        /// Total amount in smallest currency units.
        total_amount: i64,
        /// Invoice payload.
        payload: Vec<u8>,
        /// Optional shipping option ID.
        shipping_option_id: Option<String>,
        /// Whether initializing a recurring payment.
        recurring_init: bool,
        /// Whether using an existing recurring payment.
        recurring_used: bool,
        /// Optional subscription expiry timestamp.
        subscription_until_date: Option<TimeId>,
    },
    /// `messageActionPaymentSent`.
    PaymentSent {
        /// Currency code.
        currency: String,
        /// Total amount in smallest currency units.
        total_amount: i64,
        /// Optional invoice slug.
        invoice_slug: Option<String>,
        /// Whether initializing a recurring payment.
        recurring_init: bool,
        /// Whether using an existing recurring payment.
        recurring_used: bool,
        /// Optional subscription expiry timestamp.
        subscription_until_date: Option<TimeId>,
    },
    /// `messageActionPhoneCall`.
    PhoneCall {
        /// Call ID.
        call_id: CallId,
        /// Whether this was a video call.
        video: bool,
        /// Optional discard reason.
        reason: Option<PhoneCallDiscardReason>,
        /// Optional duration in seconds.
        duration: Option<i32>,
    },
    /// `messageActionScreenshotTaken`.
    ScreenshotTaken,
    /// `messageActionCustomAction`.
    CustomAction {
        /// Server-supplied display text.
        message: String,
    },
    /// `messageActionBotAllowed`.
    BotAllowed {
        /// Whether allowed via attachment menu.
        attach_menu: bool,
        /// Whether allowed from a request.
        from_request: bool,
        /// Optional domain name.
        domain: Option<String>,
        /// Optional bot app ID.
        app_id: Option<BotAppId>,
    },
    /// `messageActionSecureValuesSentMe`.
    SecureValuesSentMe {
        /// Number of Telegram Passport values received.
        values_count: usize,
    },
    /// `messageActionSecureValuesSent`.
    SecureValuesSent {
        /// Names of Telegram Passport value types sent.
        types: Vec<String>,
    },
    /// `messageActionContactSignUp`.
    ContactSignUp,
    /// `messageActionGeoProximityReached`.
    GeoProximityReached {
        /// Moving user peer ID.
        from_id: PeerId,
        /// Target user peer ID.
        to_id: PeerId,
        /// Distance in meters.
        distance: i32,
    },
    /// `messageActionGroupCall`.
    GroupCall {
        /// Call ID.
        call_id: CallId,
        /// Optional duration in seconds (present when call ended).
        duration: Option<i32>,
    },
    /// `messageActionInviteToGroupCall`.
    InviteToGroupCall {
        /// Call ID.
        call_id: CallId,
        /// Invited user IDs.
        users: Vec<UserId>,
    },
    /// `messageActionSetMessagesTTL`.
    SetMessagesTtl {
        /// Auto-delete period in seconds (`0` = disabled).
        period: TimeId,
        /// Optional user who had global auto-delete enabled.
        auto_setting_from: Option<UserId>,
    },
    /// `messageActionGroupCallScheduled`.
    GroupCallScheduled {
        /// Call ID.
        call_id: CallId,
        /// Scheduled start timestamp.
        schedule_date: TimeId,
    },
    /// `messageActionSetChatTheme`.
    SetChatTheme {
        /// Theme emoticon or token (empty = cleared).
        theme: String,
    },
    /// `messageActionChatJoinedByRequest`.
    ChatJoinedByRequest,
    /// `messageActionWebViewDataSentMe`.
    WebViewDataSentMe {
        /// Button label text.
        text: String,
        /// Payload string sent by mini app.
        data: String,
    },
    /// `messageActionWebViewDataSent`.
    WebViewDataSent {
        /// Button label text.
        text: String,
    },
    /// `messageActionGiftPremium`.
    GiftPremium {
        /// Currency code.
        currency: String,
        /// Price in smallest currency units.
        amount: i64,
        /// Gifted days count.
        days: i32,
        /// Optional crypto currency code.
        crypto_currency: Option<String>,
        /// Optional crypto amount.
        crypto_amount: Option<i64>,
        /// Optional gift message.
        message: Option<TextWithEntities>,
    },
    /// `messageActionTopicCreate`.
    TopicCreate {
        /// Topic title.
        title: String,
        /// RGB icon color.
        icon_color: i32,
        /// Optional custom emoji icon document ID.
        icon_emoji_id: Option<DocumentId>,
        /// Whether title was omitted.
        title_missing: bool,
    },
    /// `messageActionTopicEdit`.
    TopicEdit {
        /// Updated title (if changed).
        title: Option<String>,
        /// Updated custom emoji icon (if changed).
        icon_emoji_id: Option<DocumentId>,
        /// Updated closed state (if changed).
        closed: Option<bool>,
        /// Updated hidden state (if changed).
        hidden: Option<bool>,
    },
    /// `messageActionSuggestProfilePhoto`.
    SuggestProfilePhoto {
        /// Suggested profile photo.
        photo: Option<Box<PhotoData>>,
    },
    /// `messageActionRequestedPeer`.
    RequestedPeer {
        /// Request button ID.
        button_id: i32,
        /// Shared peer IDs.
        peers: Vec<PeerId>,
    },
    /// `messageActionSetChatWallPaper`.
    SetChatWallPaper {
        /// Whether matching the sender's wallpaper.
        same: bool,
        /// Whether applied for both sides.
        for_both: bool,
        /// Wallpaper ID.
        wallpaper_id: WallPaperId,
    },
    /// `messageActionGiftCode`.
    GiftCode {
        /// Whether received via giveaway.
        via_giveaway: bool,
        /// Whether code has not yet been redeemed.
        unclaimed: bool,
        /// Boosted channel peer ID (if any).
        boost_peer: Option<PeerId>,
        /// Premium days count.
        days: i32,
        /// Redemption slug.
        slug: String,
        /// Optional currency.
        currency: Option<String>,
        /// Optional amount.
        amount: Option<i64>,
        /// Optional message.
        message: Option<TextWithEntities>,
    },
    /// `messageActionGiveawayLaunch`.
    GiveawayLaunch {
        /// Optional Stars pool (for Stars giveaways).
        stars: Option<i64>,
    },
    /// `messageActionGiveawayResults`.
    GiveawayResults {
        /// Whether this was a Stars giveaway.
        stars: bool,
        /// Winner count.
        winners_count: i32,
        /// Unclaimed count.
        unclaimed_count: i32,
    },
    /// `messageActionBoostApply`.
    BoostApply {
        /// Number of boosts applied.
        boosts: i32,
    },
    /// `messageActionRequestedPeerSentMe`.
    RequestedPeerSentMe {
        /// Request button ID.
        button_id: i32,
        /// Selected peer IDs.
        peers: Vec<PeerId>,
    },
    /// `messageActionPaymentRefunded`.
    PaymentRefunded {
        /// Refunded peer ID.
        peer: PeerId,
        /// Currency code.
        currency: String,
        /// Refunded amount.
        total_amount: i64,
        /// Optional payload.
        payload: Option<Vec<u8>>,
    },
    /// `messageActionGiftStars`.
    GiftStars {
        /// Currency code.
        currency: String,
        /// Amount paid.
        amount: i64,
        /// Stars gifted.
        stars: i64,
        /// Optional crypto currency.
        crypto_currency: Option<String>,
        /// Optional crypto amount.
        crypto_amount: Option<i64>,
        /// Optional transaction ID.
        transaction_id: Option<String>,
    },
    /// `messageActionPrizeStars`.
    PrizeStars {
        /// Whether prize is unclaimed.
        unclaimed: bool,
        /// Stars won.
        stars: i64,
        /// Transaction ID.
        transaction_id: String,
        /// Host channel peer ID.
        boost_peer: PeerId,
        /// Giveaway message ID.
        giveaway_msg_id: MsgId,
    },
    /// `messageActionStarGift`.
    StarGift {
        /// Gift ID.
        gift_id: u64,
        /// Stars value of the gift.
        stars: i64,
        /// Optional Stars value if converted.
        convert_stars: Option<i64>,
        /// Whether sender name is hidden.
        name_hidden: bool,
        /// Whether displayed on recipient profile.
        saved: bool,
        /// Whether converted to Stars.
        converted: bool,
        /// Whether upgraded to collectible.
        upgraded: bool,
        /// Whether refunded.
        refunded: bool,
        /// Optional gift message.
        message: Option<TextWithEntities>,
        /// Optional sender peer.
        from_id: Option<PeerId>,
        /// Optional recipient peer.
        to_id: Option<PeerId>,
    },
    /// `messageActionStarGiftUnique`.
    StarGiftUnique {
        /// Collectible gift ID.
        gift_id: u64,
        /// Collectible slug.
        slug: String,
        /// Whether newly upgraded.
        upgrade: bool,
        /// Whether transferred.
        transferred: bool,
        /// Whether saved on profile.
        saved: bool,
        /// Whether refunded.
        refunded: bool,
        /// Optional transfer cost in Stars.
        transfer_stars: Option<i64>,
        /// Optional resale price.
        resale_amount: Option<StarsAmount>,
        /// Optional sender peer.
        from_id: Option<PeerId>,
        /// Optional message.
        message: Option<TextWithEntities>,
    },
    /// `messageActionPaidMessagesRefunded`.
    PaidMessagesRefunded {
        /// Number of messages refunded.
        count: i32,
        /// Total Stars refunded.
        stars: i64,
    },
    /// `messageActionPaidMessagesPrice`.
    PaidMessagesPrice {
        /// New Stars price per message (`0` = free).
        stars: i64,
        /// Whether channel broadcast direct messages are allowed.
        broadcast_messages_allowed: bool,
    },
    /// `messageActionConferenceCall`.
    ConferenceCall {
        /// Call ID.
        call_id: CallId,
        /// Whether missed.
        missed: bool,
        /// Whether currently active.
        active: bool,
        /// Whether video call.
        video: bool,
        /// Optional duration in seconds.
        duration: Option<i32>,
        /// Other participant peer IDs.
        other_participants: Vec<PeerId>,
    },
    /// `messageActionTodoCompletions`.
    TodoCompletions {
        /// Newly completed task IDs.
        completed: Vec<i32>,
        /// Newly uncompleted task IDs.
        incompleted: Vec<i32>,
    },
    /// `messageActionTodoAppendTasks`.
    TodoAppendTasks {
        /// Appended task items.
        list: Vec<TodoItemData>,
    },
    /// `messageActionSuggestedPostApproval`.
    SuggestedPostApproval {
        /// Whether rejected.
        rejected: bool,
        /// Whether rejected due to insufficient balance.
        balance_too_low: bool,
        /// Optional rejection comment.
        reject_comment: Option<String>,
        /// Optional scheduled publication timestamp.
        schedule_date: Option<TimeId>,
        /// Optional agreed price.
        price: Option<StarsAmount>,
    },
    /// `messageActionSuggestedPostSuccess`.
    SuggestedPostSuccess {
        /// Paid price.
        price: StarsAmount,
    },
    /// `messageActionSuggestedPostRefund`.
    SuggestedPostRefund {
        /// Whether refund was initiated by payer.
        payer_initiated: bool,
    },
    /// `messageActionGiftTon`.
    GiftTon {
        /// Fiat currency code.
        currency: String,
        /// Fiat amount.
        amount: i64,
        /// Crypto currency code (`"TON"`).
        crypto_currency: String,
        /// Crypto amount in nanotons.
        crypto_amount: i64,
        /// Optional transaction ID.
        transaction_id: Option<String>,
    },
    /// `messageActionSuggestBirthday`.
    SuggestBirthday {
        /// Suggested birthday.
        birthday: Birthday,
    },
    /// `messageActionStarGiftPurchaseOffer`.
    StarGiftPurchaseOffer {
        /// Gift ID.
        gift_id: u64,
        /// Offered price.
        price: StarsAmount,
        /// Offer expiration timestamp.
        expires_at: TimeId,
        /// Whether accepted.
        accepted: bool,
        /// Whether declined.
        declined: bool,
    },
    /// `messageActionStarGiftPurchaseOfferDeclined`.
    StarGiftPurchaseOfferDeclined {
        /// Gift ID.
        gift_id: u64,
        /// Offered price.
        price: StarsAmount,
        /// Whether expired rather than explicitly declined.
        expired: bool,
    },
    /// `messageActionNewCreatorPending`.
    NewCreatorPending {
        /// Pending successor creator user ID.
        new_creator_id: UserId,
    },
    /// `messageActionChangeCreator`.
    ChangeCreator {
        /// New creator user ID.
        new_creator_id: UserId,
    },
    /// `messageActionNoForwardsToggle`.
    NoForwardsToggle {
        /// Previous setting.
        prev_value: bool,
        /// New setting.
        new_value: bool,
    },
    /// `messageActionNoForwardsRequest`.
    NoForwardsRequest {
        /// Whether the request expired.
        expired: bool,
        /// Previous setting.
        prev_value: bool,
        /// Requested new setting.
        new_value: bool,
    },
    /// `messageActionPollAppendAnswer`.
    PollAppendAnswer {
        /// Appended answer option.
        answer: PollAnswer,
    },
    /// `messageActionPollDeleteAnswer`.
    PollDeleteAnswer {
        /// Deleted answer option.
        answer: PollAnswer,
    },
    /// `messageActionManagedBotCreated`.
    ManagedBotCreated {
        /// Created child bot user ID.
        bot_id: UserId,
    },
    /// `messageActionChangeCommunity`.
    ChangeCommunity {
        /// New linked community channel ID (`None` = unlinked).
        community_id: Option<ChannelId>,
    },
    /// `messageActionChatJoinedViaCommunity`.
    ChatJoinedViaCommunity {
        /// Community channel ID.
        community_id: ChannelId,
    },
    /// `messageActionGramTransfer`.
    GramTransfer {
        /// Amount transferred.
        amount: i64,
        /// Peer wallet address.
        peer_address: String,
        /// Transaction ID.
        transaction_id: String,
        /// Optional comment.
        comment: Option<String>,
        /// Whether the comment is encrypted.
        comment_encrypted: bool,
    },
    /// `messageActionWalletTonConnectRequest`.
    WalletTonConnectRequest {
        /// Session ID.
        session_id: i64,
        /// Expiry timestamp.
        expires: TimeId,
        /// Whether accepted.
        accepted: bool,
        /// Whether declined.
        declined: bool,
        /// Optional topic.
        topic: Option<String>,
        /// Optional trace ID.
        trace_id: Option<String>,
        /// Optional dApp name.
        dapp_name: Option<String>,
    },
    /// `messageActionNoPaidMessagesTokenUsed`.
    NoPaidMessagesTokenUsed,
}

/// Primary content payload of a `Message`.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum MessageContent {
    /// Empty / placeholder message (`messageEmpty`).
    #[default]
    Empty,
    /// Regular text and/or media message (`message`).
    Regular {
        /// Message text or media caption with UTF-16 entities.
        text: TextWithEntities,
        /// Optional media attachment (`MessageMedia::Empty` for text-only).
        media: MessageMedia,
    },
    /// Service action message (`messageService`).
    Service(Box<ServiceAction>),
}

/// Forwarding attribution metadata (`MessageFwdHeader` / `HistoryMessageForwarded`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct ForwardInfo {
    /// Original sender peer ID (when not hidden).
    pub from_id: Option<PeerId>,
    /// Original sender display name (when privacy hides `from_id`).
    pub from_name: Option<String>,
    /// Original message timestamp.
    pub date: TimeId,
    /// Original channel post message ID.
    pub channel_post: Option<MsgId>,
    /// Original post author signature.
    pub post_author: Option<String>,
    /// Peer from which the message was saved into Saved Messages.
    pub saved_from_peer: Option<PeerId>,
    /// Original message ID in `saved_from_peer`.
    pub saved_from_msg_id: Option<MsgId>,
    /// Original author peer ID in Saved Messages.
    pub saved_from_id: Option<PeerId>,
    /// Original author name in Saved Messages.
    pub saved_from_name: Option<String>,
    /// Original date when saved.
    pub saved_date: Option<TimeId>,
    /// Public Service Announcement type (if any).
    pub psa_type: Option<String>,
    /// Whether this message was imported from an external messenger.
    pub imported: bool,
}

/// Reply target, quote, and topic threading metadata (`MessageReplyHeader` / `FullReplyTo`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct ReplyInfo {
    /// Target message ID (in `reply_to_peer_id` if cross-chat, else same chat).
    pub reply_to_msg_id: Option<MsgId>,
    /// Cross-chat target peer ID (when replying to a message in another chat).
    pub reply_to_peer_id: Option<PeerId>,
    /// Target story (when replying to a story).
    pub reply_to_story: Option<FullStoryId>,
    /// Top message ID of the thread / forum topic root (`reply_to_top_id`).
    pub top_msg_id: Option<MsgId>,
    /// Monoforum sublist peer ID (if applicable).
    pub monoforum_peer_id: Option<PeerId>,
    /// Whether this reply belongs to a forum topic.
    pub forum_topic: bool,
    /// Optional quoted text snippet with entities.
    pub quote_text: Option<TextWithEntities>,
    /// Optional UTF-16 offset of the quote in the original message.
    pub quote_offset: Option<i32>,
    /// Whether the quote was explicitly selected by the sender.
    pub quote: bool,
    /// Original forward header for cross-chat replies.
    pub reply_from: Option<Box<ForwardInfo>>,
    /// Specific to-do item ID replied to.
    pub todo_item_id: Option<i32>,
    /// Specific poll option bytes replied to.
    pub poll_option: Option<Vec<u8>>,
}

impl ReplyInfo {
    /// Returns the resolved `FullMsgId` of the replied-to message given the
    /// containing chat's `current_peer`.
    pub fn target_full_msg_id(&self, current_peer: PeerId) -> Option<FullMsgId> {
        self.reply_to_msg_id.map(|msg| {
            let peer = self.reply_to_peer_id.unwrap_or(current_peer);
            FullMsgId::new(peer, msg)
        })
    }
}

/// Per-reaction aggregate count on a message (`ReactionCount`).
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct MessageReactionCount {
    /// Reaction identifier.
    pub id: ReactionId,
    /// Number of users who chose this reaction.
    pub count: i32,
    /// If the current user chose this reaction, the server's chosen order index.
    pub chosen_order: Option<i32>,
}

impl MessageReactionCount {
    /// Returns `true` if the current user reacted with this reaction.
    #[inline]
    pub const fn is_chosen(&self) -> bool {
        self.chosen_order.is_some()
    }
}

/// Recent reactor entry for avatar previews on a message (`MessagePeerReaction`).
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct RecentReactor {
    /// Reactor peer ID.
    pub peer_id: PeerId,
    /// Reaction chosen.
    pub reaction: ReactionId,
    /// Timestamp of reaction.
    pub date: TimeId,
    /// Whether this reaction is still unread by us.
    pub unread: bool,
    /// Whether this is our own reaction.
    pub my: bool,
}

/// Summary of reactions on a message (`MessageReactions`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct MessageReactionsSummary {
    /// Reaction counts.
    pub results: Vec<MessageReactionCount>,
    /// Recent reactors (for small chats / own messages).
    pub recent_reactors: Vec<RecentReactor>,
    /// Whether the current user can open the full reactors list.
    pub can_see_list: bool,
    /// Whether reactions are personal Saved Messages tags.
    pub reactions_as_tags: bool,
    /// Whether this summary came from a `min` object (preserve local chosen state).
    pub min: bool,
}

impl MessageReactionsSummary {
    /// Optimistically strips all reactions by `participant_id` from this summary,
    /// decrementing aggregate counts and removing zeroed entries, matching
    /// `MessageReactions::removeFromParticipant` (`data/data_message_reactions.cpp:2036-2099`).
    ///
    /// When `is_self` is `true`, also clears `chosen_order` for removed self reactions (and
    /// strips chosen reactions even if `recent_reactors` did not list self).
    pub fn remove_from_participant(
        &mut self,
        participant_id: PeerId,
        known_reaction: Option<&ReactionId>,
        is_self: bool,
    ) -> bool {
        let mut changed = false;
        let mut participant_found = false;
        let mut removed_counts: Vec<(ReactionId, i32)> = Vec::new();

        self.recent_reactors.retain(|reactor| {
            if reactor.peer_id == participant_id {
                participant_found = true;
                changed = true;
                if let Some((_, count)) = removed_counts
                    .iter_mut()
                    .find(|(id, _)| *id == reactor.reaction)
                {
                    *count += 1;
                } else {
                    removed_counts.push((reactor.reaction.clone(), 1));
                }
                false
            } else {
                true
            }
        });

        let decrement = |results: &mut Vec<MessageReactionCount>,
                         id: &ReactionId,
                         count: i32,
                         clear_chosen: bool|
         -> bool {
            let Some(pos) = results.iter().position(|entry| &entry.id == id) else {
                return false;
            };
            if results[pos].count <= count {
                results.remove(pos);
            } else {
                results[pos].count -= count;
                if clear_chosen {
                    results[pos].chosen_order = None;
                }
            }
            true
        };

        for (reaction_id, count) in &removed_counts {
            decrement(&mut self.results, reaction_id, *count, is_self);
        }

        if is_self && !participant_found {
            let chosen_ids: Vec<ReactionId> = self
                .results
                .iter()
                .filter(|entry| entry.is_chosen())
                .map(|entry| entry.id.clone())
                .collect();
            for id in &chosen_ids {
                if decrement(&mut self.results, id, 1, true) {
                    changed = true;
                    participant_found = true;
                }
            }
        }

        if let Some(known) = known_reaction
            && !known.is_empty()
            && !participant_found
            && decrement(&mut self.results, known, 1, is_self)
        {
            changed = true;
        }

        changed
    }

    /// Returns `true` when both aggregate reaction counts and recent reactors are empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.results.is_empty() && self.recent_reactors.is_empty()
    }
}

/// Semantic button color style (`KeyboardButtonStyle` in `schema/api.tl`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum ButtonColor {
    /// Standard theme button background.
    #[default]
    Normal,
    /// Primary accent background (`bg_primary`).
    Primary,
    /// Destructive / danger background (`bg_danger`).
    Danger,
    /// Positive / success background (`bg_success`).
    Success,
}

impl ButtonColor {
    /// Resolves color precedence matching `ParseVisual` in `history_item_reply_markup.cpp`:
    /// danger > primary > success > normal.
    #[inline]
    pub const fn from_style_flags(bg_danger: bool, bg_primary: bool, bg_success: bool) -> Self {
        if bg_danger {
            Self::Danger
        } else if bg_primary {
            Self::Primary
        } else if bg_success {
            Self::Success
        } else {
            Self::Normal
        }
    }
}

/// Visual customization attached to a keyboard button (`KeyboardButtonStyle`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct ButtonVisual {
    /// Optional custom emoji icon document ID (`DocumentId::ZERO` if none).
    pub icon_id: DocumentId,
    /// Semantic button color.
    pub color: ButtonColor,
}

bitflags! {
    /// Allowed peer types for `switch_inline` recipient choosers (`InlineQueryPeerType`).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct InlineQueryPeerTypes: u8 {
        /// Same bot's private chat.
        const SAME_BOT  = 1 << 0;
        /// Other bots' private chats.
        const BOT       = 1 << 1;
        /// Regular users' private chats.
        const USER      = 1 << 2;
        /// Basic groups and supergroups.
        const GROUP     = 1 << 3;
        /// Broadcast channels.
        const BROADCAST = 1 << 4;
    }
}

/// Peer category requested by a `buttonTypeRequestPeer` keyboard button.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum RequestPeerType {
    /// Request user(s).
    #[default]
    User,
    /// Request basic group(s) or supergroup(s).
    Group,
    /// Request broadcast channel(s).
    Broadcast,
}

/// Tri-state filter criteria for [`RequestPeerQuery`].
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum RequestPeerRestriction {
    /// No restriction (TL field omitted).
    #[default]
    Any,
    /// Must satisfy predicate (`boolTrue`).
    Yes,
    /// Must NOT satisfy predicate (`boolFalse`).
    No,
}

impl RequestPeerRestriction {
    /// Converts an optional boolean into a tri-state [`RequestPeerRestriction`].
    #[inline]
    pub const fn from_option(value: Option<bool>) -> Self {
        match value {
            None => Self::Any,
            Some(true) => Self::Yes,
            Some(false) => Self::No,
        }
    }
}

/// Criteria for a `buttonTypeRequestPeer` keyboard button (`RequestPeerQuery`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct RequestPeerQuery {
    /// Maximum number of peers the user may select (`max_quantity`).
    pub max_quantity: i32,
    /// Target peer category.
    pub peer_type: RequestPeerType,
    /// For users: whether the user must/must not be a bot.
    pub user_is_bot: RequestPeerRestriction,
    /// For users: whether the user must/must not have Telegram Premium.
    pub user_is_premium: RequestPeerRestriction,
    /// For groups: whether the group must/must not be a forum.
    pub group_is_forum: RequestPeerRestriction,
    /// For groups/channels: whether the peer must/must not have a public username.
    pub has_username: RequestPeerRestriction,
    /// Whether the current user must be the creator of the group/channel.
    pub am_creator: bool,
    /// For groups: whether the requesting bot must already be a participant.
    pub is_bot_participant: bool,
    /// Minimum admin rights required for the current user.
    pub my_rights: ChatAdminRights,
    /// Minimum admin rights required for the bot.
    pub bot_rights: ChatAdminRights,
}

/// Poll type restriction for `buttonTypeRequestPoll` buttons.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum PollRequestRestriction {
    /// User may create either a regular poll or a quiz.
    #[default]
    Any,
    /// Only regular (non-quiz) polls allowed (`quiz = false`).
    RegularOnly,
    /// Only quiz polls allowed (`quiz = true`).
    QuizOnly,
}

impl PollRequestRestriction {
    /// Converts optional TL `quiz: Bool` into [`PollRequestRestriction`].
    #[inline]
    pub const fn from_quiz_option(quiz: Option<bool>) -> Self {
        match quiz {
            None => Self::Any,
            Some(false) => Self::RegularOnly,
            Some(true) => Self::QuizOnly,
        }
    }
}

/// All Telegram layer 233 reply-keyboard (`ButtonType`) and inline-keyboard (`InlineButtonType`)
/// button payloads.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum KeyboardButtonKind {
    /// Plain reply button (`buttonTypeDefault`).
    Default,
    /// Request phone contact (`buttonTypeRequestPhone`).
    RequestPhone,
    /// Request geolocation (`buttonTypeRequestGeoLocation`).
    RequestLocation,
    /// Request poll creation (`buttonTypeRequestPoll`).
    RequestPoll {
        /// Optional quiz/regular restriction.
        restriction: PollRequestRestriction,
    },
    /// Request peer selection (`buttonTypeRequestPeer` for user/chat/broadcast).
    RequestPeer {
        /// Button ID echoed to `messages.sendBotRequestedPeer`.
        button_id: i32,
        /// Peer selection criteria.
        query: RequestPeerQuery,
    },
    /// Create managed bot (`buttonTypeRequestPeer` with `requestPeerTypeCreateBot`).
    CreateBot {
        /// Button ID echoed to `messages.sendBotRequestedPeer`.
        button_id: i32,
        /// Suggested display name.
        suggested_name: String,
        /// Suggested username.
        suggested_username: String,
    },
    /// Simple WebView from reply keyboard (`buttonTypeSimpleWebView`).
    SimpleWebView {
        /// Target WebView URL.
        url: String,
    },
    /// Inline external/Telegram URL (`inlineButtonTypeUrl`).
    Url {
        /// Target URL.
        url: String,
    },
    /// Inline URL OAuth / login button (`inlineButtonTypeUrlAuth`).
    UrlAuth {
        /// Target URL to authenticate against.
        url: String,
        /// Replacement text shown when forwarded.
        forward_text: String,
        /// Server button ID for `messages.requestUrlAuth` / `acceptUrlAuth`.
        button_id: i32,
    },
    /// Inline Mini App WebView button (`inlineButtonTypeWebView`).
    WebView {
        /// Target WebView URL.
        url: String,
    },
    /// Inline callback query button (`inlineButtonTypeCallback` without password).
    Callback {
        /// Opaque callback payload bytes.
        data: Vec<u8>,
    },
    /// Inline 2FA-password-protected callback button (`inlineButtonTypeCallback` with `requires_password`).
    CallbackWithPassword {
        /// Opaque callback payload bytes.
        data: Vec<u8>,
    },
    /// Inline HTML5 bot game button (`inlineButtonTypeGame`).
    Game,
    /// Inline payment/checkout button (`inlineButtonTypeBuy`).
    Buy,
    /// Inline switch-to-inline button in a chosen chat (`inlineButtonTypeSwitchInline`, `same_peer = false`).
    SwitchInline {
        /// Pre-filled inline query string.
        query: String,
        /// Allowed recipient peer types.
        peer_types: InlineQueryPeerTypes,
    },
    /// Inline switch-to-inline button in the current chat (`inlineButtonTypeSwitchInline`, `same_peer = true`).
    SwitchInlineSame {
        /// Pre-filled inline query string.
        query: String,
    },
    /// Inline user profile link button (`inlineButtonTypeUserProfile`).
    UserProfile {
        /// Target user ID.
        user_id: UserId,
    },
    /// Inline copy-to-clipboard button (`inlineButtonTypeCopy`).
    CopyText {
        /// Text copied to clipboard.
        text: String,
    },
    /// Non-interactive disabled inline button (`inlineButtonTypeDisabled`).
    Disabled,
}

/// A single button in a reply or inline keyboard (`KeyboardButton` / `KeyboardInlineButton`).
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct KeyboardButton {
    /// Visible button label.
    pub text: String,
    /// Visual customization (icon + semantic color).
    pub visual: ButtonVisual,
    /// Typed button kind and payload.
    pub kind: KeyboardButtonKind,
}

impl KeyboardButton {
    /// Creates a new [`KeyboardButton`] with default visual styling.
    pub fn new(text: impl Into<String>, kind: KeyboardButtonKind) -> Self {
        Self {
            text: text.into(),
            visual: ButtonVisual::default(),
            kind,
        }
    }
}

/// Reply markup attached to a message (`ReplyMarkup` in `schema/api.tl` layer 233).
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum ReplyMarkup {
    /// Inline keyboard attached beneath a message bubble (`replyInlineMarkup`).
    Inline {
        /// Rows of inline buttons.
        rows: Vec<Vec<KeyboardButton>>,
        /// Optional `force_reply` flag.
        force_reply: bool,
    },
    /// Custom reply keyboard below the compose box (`replyKeyboardMarkup`).
    Keyboard {
        /// Rows of reply buttons.
        rows: Vec<Vec<KeyboardButton>>,
        /// Resize vertically to fit content (`resize`).
        resize: bool,
        /// Hide keyboard after one use (`single_use`).
        single_use: bool,
        /// Apply selectively to mentioned/replied users (`selective`).
        selective: bool,
        /// Keep keyboard visible (`persistent`).
        persistent: bool,
        /// Also force reply (`force_reply`).
        force_reply: bool,
        /// Compose input placeholder (`placeholder`).
        placeholder: String,
    },
    /// Force reply interface (`replyKeyboardForceReply`).
    ForceReply {
        /// Single-use flag.
        single_use: bool,
        /// Selective flag.
        selective: bool,
        /// Compose input placeholder (`placeholder`).
        placeholder: String,
    },
    /// Hide / remove active reply keyboard (`replyKeyboardHide`).
    RemoveKeyboard {
        /// Remove selectively.
        selective: bool,
    },
}

impl ReplyMarkup {
    /// Returns `true` if this markup is an inline keyboard.
    #[inline]
    pub const fn is_inline(&self) -> bool {
        matches!(self, Self::Inline { .. })
    }

    /// Returns `true` if any button in this markup is `KeyboardButtonKind::SwitchInline`.
    pub fn has_switch_inline(&self) -> bool {
        let rows = match self {
            Self::Inline { rows, .. } | Self::Keyboard { rows, .. } => rows.as_slice(),
            Self::ForceReply { .. } | Self::RemoveKeyboard { .. } => &[],
        };
        rows.iter().any(|row| {
            row.iter()
                .any(|btn| matches!(btn.kind, KeyboardButtonKind::SwitchInline { .. }))
        })
    }
}

/// Domain model of a Telegram message (`HistoryItem` model layer).
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    /// Message ID within `peer_id`.
    pub id: MsgId,
    /// Owning dialog peer ID.
    pub peer_id: PeerId,
    /// Sender peer ID (absent for anonymous channel posts).
    pub from_id: Option<PeerId>,
    /// Creation or scheduled send timestamp.
    pub date: TimeId,
    /// Last edit timestamp (if edited).
    pub edit_date: Option<TimeId>,
    /// 64-bit message flags.
    pub flags: MessageFlags,
    /// Message payload (text+media, service action, or empty).
    pub content: MessageContent,
    /// Optional reply / topic / quote header.
    pub reply_to: Option<ReplyInfo>,
    /// Optional forward header.
    pub forward_from: Option<ForwardInfo>,
    /// View counter (for channel posts and forwarded posts).
    pub views: Option<i32>,
    /// Forward counter.
    pub forwards: Option<i32>,
    /// Comments / replies count (when `HAS_REPLY_INFO` is set).
    pub replies_count: Option<i32>,
    /// Reactions summary.
    pub reactions: Option<MessageReactionsSummary>,
    /// Album grouping ID (`grouped_id`).
    pub grouped_id: Option<u64>,
    /// Inline bot user ID (`via_bot_id`).
    pub via_bot_id: Option<UserId>,
    /// Business bot user ID (`via_business_bot_id`).
    pub via_business_bot_id: Option<UserId>,
    /// Post author signature (`post_author`).
    pub post_author: Option<String>,
    /// Self-destruct / auto-delete TTL in seconds.
    pub ttl_period: Option<TimeId>,
    /// Scheduled repeat period in seconds.
    pub schedule_repeat_period: Option<TimeId>,
    /// Visual message effect ID.
    pub effect_id: Option<EffectId>,
    /// Business quick-reply shortcut ID.
    pub shortcut_id: Option<BusinessShortcutId>,
    /// Telegram Stars paid to send this message.
    pub paid_message_stars: Option<i64>,
    /// Optional reply markup (inline keyboard, reply keyboard, force reply, or hide).
    pub reply_markup: Option<ReplyMarkup>,
}

impl Message {
    /// Creates a new regular text message with sensible defaults.
    pub fn new_text(
        peer_id: PeerId,
        id: MsgId,
        from_id: Option<PeerId>,
        date: TimeId,
        text: TextWithEntities,
    ) -> Self {
        let mut flags = MessageFlags::HISTORY_ENTRY;
        if from_id.is_some() {
            flags.insert(MessageFlags::HAS_FROM_ID);
        }
        Self {
            id,
            peer_id,
            from_id,
            date,
            edit_date: None,
            flags,
            content: MessageContent::Regular {
                text,
                media: MessageMedia::Empty,
            },
            reply_to: None,
            forward_from: None,
            views: None,
            forwards: None,
            replies_count: None,
            reactions: None,
            grouped_id: None,
            via_bot_id: None,
            via_business_bot_id: None,
            post_author: None,
            ttl_period: None,
            schedule_repeat_period: None,
            effect_id: None,
            shortcut_id: None,
            paid_message_stars: None,
            reply_markup: None,
        }
    }

    /// Creates a new service message.
    pub fn new_service(
        peer_id: PeerId,
        id: MsgId,
        from_id: Option<PeerId>,
        date: TimeId,
        action: ServiceAction,
    ) -> Self {
        let mut flags = MessageFlags::HISTORY_ENTRY;
        if from_id.is_some() {
            flags.insert(MessageFlags::HAS_FROM_ID);
        }
        Self {
            id,
            peer_id,
            from_id,
            date,
            edit_date: None,
            flags,
            content: MessageContent::Service(Box::new(action)),
            reply_to: None,
            forward_from: None,
            views: None,
            forwards: None,
            replies_count: None,
            reactions: None,
            grouped_id: None,
            via_bot_id: None,
            via_business_bot_id: None,
            post_author: None,
            ttl_period: None,
            schedule_repeat_period: None,
            effect_id: None,
            shortcut_id: None,
            paid_message_stars: None,
            reply_markup: None,
        }
    }

    /// Returns the message's `FullMsgId`.
    #[inline]
    pub const fn full_id(&self) -> FullMsgId {
        FullMsgId::new(self.peer_id, self.id)
    }

    /// Returns the message's `(date, full_id)` sort position (`HistoryItem::position`).
    #[inline]
    pub const fn position(&self) -> crate::id::MessagePosition {
        crate::id::MessagePosition {
            full_id: self.full_id(),
            date: self.date,
        }
    }

    /// Returns `true` if this outgoing message is currently being sent (`HistoryItem::isSending`).
    #[inline]
    pub const fn is_sending(&self) -> bool {
        self.flags.contains(MessageFlags::BEING_SENT)
    }

    /// Returns `true` if sending this message failed (`HistoryItem::hasFailed`).
    #[inline]
    pub const fn has_failed(&self) -> bool {
        self.flags.contains(MessageFlags::SENDING_FAILED)
    }

    /// Returns the forum topic root message ID if this message belongs to a forum topic
    /// (`HistoryItem::topicRootId`).
    #[inline]
    pub fn topic_root_id(&self) -> Option<MsgId> {
        let reply = self.reply_to.as_ref()?;
        if !reply.forum_topic {
            return None;
        }
        reply
            .top_msg_id
            .or(reply.reply_to_msg_id)
            .filter(|id| !id.is_zero())
    }

    /// Returns the scoped `MessageGroupId` if this message belongs to an album.
    #[inline]
    pub fn group_id(&self) -> Option<MessageGroupId> {
        self.grouped_id
            .filter(|&v| v != 0)
            .map(|v| MessageGroupId::from_raw(self.peer_id, v, self.is_scheduled()))
    }

    /// Returns `true` if this is an outgoing message.
    #[inline]
    pub const fn is_outgoing(&self) -> bool {
        self.flags.contains(MessageFlags::OUTGOING)
    }

    /// Returns `true` if this message is pinned.
    #[inline]
    pub const fn is_pinned(&self) -> bool {
        self.flags.contains(MessageFlags::PINNED)
    }

    /// Returns `true` if this is a channel broadcast post.
    #[inline]
    pub const fn is_post(&self) -> bool {
        self.flags.contains(MessageFlags::POST)
    }

    /// Returns `true` if sent silently.
    #[inline]
    pub const fn is_silent(&self) -> bool {
        self.flags.contains(MessageFlags::SILENT)
    }

    /// Returns `true` if this is a scheduled message (by ID range or flag).
    #[inline]
    pub const fn is_scheduled(&self) -> bool {
        self.id.is_scheduled()
            || (self.flags.contains(MessageFlags::IS_OR_WAS_SCHEDULED) && !self.id.is_server())
    }

    /// Returns `true` if this message was originally sent via schedule.
    #[inline]
    pub const fn is_from_scheduled(&self) -> bool {
        self.flags.contains(MessageFlags::IS_OR_WAS_SCHEDULED) && self.id.is_server()
    }

    /// Returns `true` if this is a regular server message in main history (`HistoryItem::isRegular`).
    #[inline]
    pub const fn is_regular(&self) -> bool {
        self.id.is_server()
            && !self.flags.contains(MessageFlags::LOCAL)
            && !self.flags.contains(MessageFlags::FAKE_HISTORY_ITEM)
            && !self.flags.contains(MessageFlags::SPONSORED)
    }

    /// Returns `true` if this is a service action message.
    #[inline]
    pub const fn is_service(&self) -> bool {
        matches!(self.content, MessageContent::Service(_))
    }

    /// Returns `true` if this is an empty placeholder message.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        matches!(self.content, MessageContent::Empty)
    }

    /// Returns `true` if this message has been edited.
    #[inline]
    pub const fn is_edited(&self) -> bool {
        self.edit_date.is_some()
    }

    /// Returns `true` if this message mentions the current user.
    #[inline]
    pub const fn mentions_me(&self) -> bool {
        self.flags.contains(MessageFlags::MENTIONS_ME)
    }

    /// Returns `true` if this message has unread reactions.
    #[inline]
    pub const fn has_unread_reaction(&self) -> bool {
        self.flags.contains(MessageFlags::HAS_UNREAD_REACTION)
    }

    /// Returns `true` if forwarding is disabled on this message.
    #[inline]
    pub const fn no_forwards(&self) -> bool {
        self.flags.contains(MessageFlags::NO_FORWARDS)
    }

    /// Returns a reference to the message text/caption if this is a regular message.
    pub fn text(&self) -> Option<&TextWithEntities> {
        match &self.content {
            MessageContent::Regular { text, .. } => Some(text),
            _ => None,
        }
    }

    /// Returns a reference to the message media if this is a regular message.
    pub fn media(&self) -> Option<&MessageMedia> {
        match &self.content {
            MessageContent::Regular { media, .. } => Some(media),
            _ => None,
        }
    }

    /// Returns a reference to the service action if this is a service message.
    pub fn service_action(&self) -> Option<&ServiceAction> {
        match &self.content {
            MessageContent::Service(action) => Some(action),
            _ => None,
        }
    }

    /// Returns a reference to the message's reply markup, if present.
    #[inline]
    pub fn reply_markup(&self) -> Option<&ReplyMarkup> {
        self.reply_markup.as_ref()
    }
}
