//! History view domain types and `HistoryBackend` trait.
//!
//! Port origin: narrow history facade over `Telegram/SourceFiles/apiwrap.h`,
//! `Telegram/SourceFiles/history/history.h`, `Telegram/SourceFiles/history/history_item.h`,
//! `Telegram/SourceFiles/data/data_drafts.h`, and `Telegram/SourceFiles/api/api_send_progress.h`.

use tg_text::TextWithEntities;

pub use crate::core_types::dto::engagement::{
    ChatMigrationResult, DEFAULT_CHAT_READ_MARK_SIZE_THRESHOLD,
    DEFAULT_READ_DATE_EXPIRE_PERIOD_SECS, EphemeralReportOption, EphemeralReportOutcome,
    ExportedMessageLinkInfo, ExtendedMediaRefreshEntry, FutureChatCreatorInfo,
    MAX_EXTENDED_MEDIA_POLL_PER_REQUEST, MessageAuthorInfo, MessageReadParticipant,
    MessageViewsInfo, OutboxReadDateStatus, POLL_EXTENDED_MEDIA_PERIOD_MS,
    REPORT_DELIVERIES_PER_REQUEST, UNREAD_THINGS_FIRST_LIMIT, UNREAD_THINGS_NEXT_LIMIT,
    UNREAD_THINGS_PRELOAD_IF_LESS, UnreadThingsCounts, UnreadThingsKind, UnreadThingsReadResult,
    UnreadThingsScope, VIEWS_SEND_TIMEOUT_MS, unread_things_pagination_params,
    who_read_exists_for_dm, who_read_exists_for_group,
};
use crate::core_types::dto::factcheck::FactCheckData;
use crate::core_types::dto::inline_media::InlineMediaDescriptor;
use crate::core_types::dto::monoforum::{
    MonoforumSublist, SuggestApprovalDecision, SuggestOptions, SuggestedPostData,
    format_suggested_post_approval_service, format_suggested_post_refund_service,
    format_suggested_post_success_service,
};
use crate::core_types::dto::rich_compose::{MentionCandidate, RichThreadDraft};
use crate::core_types::dto::sponsored::{SponsoredMessageItem, SponsoredReportOutcome};
use crate::core_types::dto::todo::{
    CreateTodoListRequest, TodoItemInput, TodoListAttachment, format_todo_append_service,
    format_todo_completions_service,
};

use crate::history::sections_model::{
    AddPollOptionRequest, AdminLogAdmin, AdminLogEntry, AdminLogFilter, ContactAttachment,
    CreatePollRequest, ForumTopic, ForwardOptions, InlineBotResult, LinkPreviewData,
    LocationAttachment, MessageReportReason, PollMessageExtras, ReplyQuote, RoundVideoAttachment,
    SavedSublist, ScheduleTarget, SendAsPeer, SlowModeState, ThreadKey, VenueItem,
};
use crate::stand_in::types::{FullMsgId, MsgId, PeerId, PeerKind, PeerSummary};

/// Converts a `tg-app` [`PeerId`] into a `tg-history` [`tg_history::PeerId`].
#[inline]
#[must_use]
pub const fn to_hist_peer(peer: PeerId) -> tg_history::PeerId {
    tg_history::PeerId(peer.0)
}

/// Converts a `tg-app` [`MsgId`] into a `tg-history` [`tg_history::MsgId`].
#[inline]
#[must_use]
pub const fn to_hist_msg(msg: MsgId) -> tg_history::MsgId {
    tg_history::MsgId(msg.0 as i64)
}

/// Converts a `tg-app` [`FullMsgId`] into a `tg-history` [`tg_history::FullMsgId`].
#[inline]
#[must_use]
pub const fn to_hist_full(id: FullMsgId) -> tg_history::FullMsgId {
    tg_history::FullMsgId {
        peer: to_hist_peer(id.peer),
        msg: to_hist_msg(id.msg),
    }
}

/// Converts a `tg-history` [`tg_history::FullMsgId`] back to `tg-app`'s [`FullMsgId`].
#[inline]
#[must_use]
pub const fn from_hist_full(id: tg_history::FullMsgId) -> FullMsgId {
    FullMsgId {
        peer: PeerId(id.peer.0),
        msg: MsgId(id.msg.0 as i32),
    }
}

/// Converts a `tg-app` [`PeerId`] into a `tg-bots` [`tg_bots::stand_in::PeerId`].
#[inline]
#[must_use]
pub const fn to_bot_peer(peer: PeerId) -> tg_bots::stand_in::PeerId {
    tg_bots::stand_in::PeerId(peer.0)
}

/// Converts a `tg-bots` [`tg_bots::stand_in::PeerId`] back to `tg-app`'s [`PeerId`].
#[inline]
#[must_use]
pub const fn from_bot_peer(peer: tg_bots::stand_in::PeerId) -> PeerId {
    PeerId(peer.0)
}

/// Converts a `tg-app` [`MsgId`] into a `tg-bots` [`tg_bots::stand_in::MsgId`].
#[inline]
#[must_use]
pub const fn to_bot_msg(msg: MsgId) -> tg_bots::stand_in::MsgId {
    tg_bots::stand_in::MsgId(msg.0)
}

/// Converts a `tg-app` [`FullMsgId`] into a `tg-bots` [`tg_bots::stand_in::FullMsgId`].
#[inline]
#[must_use]
pub const fn to_bot_full(id: FullMsgId) -> tg_bots::stand_in::FullMsgId {
    tg_bots::stand_in::FullMsgId::new(to_bot_peer(id.peer), to_bot_msg(id.msg))
}

/// Converts a `tg-app` [`PeerKind`] (plus `is_bot` flag) into a `tg-bots` [`tg_bots::stand_in::PeerKind`].
#[inline]
#[must_use]
pub const fn to_bot_peer_kind(kind: PeerKind, is_bot: bool) -> tg_bots::stand_in::PeerKind {
    match kind {
        PeerKind::User => {
            if is_bot {
                tg_bots::stand_in::PeerKind::Bot
            } else {
                tg_bots::stand_in::PeerKind::User
            }
        }
        PeerKind::Group => tg_bots::stand_in::PeerKind::Group,
        PeerKind::Channel => tg_bots::stand_in::PeerKind::Broadcast,
    }
}

/// Typing / activity action sent to a peer (`SendProgressManager`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypingAction {
    /// Standard text typing indicator.
    Typing,
    /// Recording a voice message.
    RecordingVoice,
    /// Uploading a photo/file.
    UploadingMedia,
    /// Cancel any active send progress.
    Cancel,
}

/// Media thumbnail of the replied-to message, drawn 32x32 at the left of the reply header
/// (`Data::ReplyPreview`, `Reply::_hasPreview`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReplyThumb {
    /// Inline (`photoStrippedSize`) thumbnail bytes, drawn blurred until the real thumbnail
    /// arrives through [`HistoryBackend::reply_preview_thumb`].
    pub stripped: Option<Vec<u8>>,
    /// Round video message: the thumbnail is a circle (`Images::Option::RoundCircle`).
    pub round: bool,
    /// Media under a spoiler (`Media::hasSpoilerForPreview`): the thumbnail stays blurred,
    /// whether it comes from the inline bytes or from the downloaded file.
    pub spoiler: bool,
    /// The animated spoiler particles are drawn over the blurred thumbnail (`Reply::_spoiler`,
    /// only for the media of a resolved message).
    pub spoiler_animation: bool,
    /// Identity of the image (the photo, document or web file and its spoiler flag): the
    /// downloaded thumbnail is cached per message and this key, so an edit of the replied-to
    /// media loads the new one.
    pub key: u64,
}

/// Reply reference header displayed inside a message bubble (`HistoryView::Reply`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyHeader {
    /// Target message id being replied to.
    pub reply_to_msg_id: MsgId,
    /// Display name of the replied-to message author. Empty while the target is loading or
    /// deleted: `preview_text` then holds that one-line state (`Reply::_stateText`).
    pub author_name: String,
    /// Bare sender id for deterministic peer accent color.
    pub author_color_id: u64,
    /// The author's accent (`Reply::_colorPeer`: `colorIndex` and `colorCollectible`). `None`:
    /// the id-based default of `author_color_id`.
    pub author_accent: Option<tg_ui::peer_color::PeerAccent>,
    /// Short preview text of the replied-to message.
    pub preview_text: String,
    /// `Some` when the header reserves the 32x32 media preview (`Reply::_hasPreview`); the RGB
    /// fills it while no thumbnail image is available.
    pub preview_thumb_rgb: Option<[u8; 3]>,
    /// Thumbnail of the replied-to message's media. Set together with `preview_thumb_rgb`.
    pub preview_thumb: Option<ReplyThumb>,
    /// Leading bytes of `preview_text` drawn in the reply's name colour instead of the text
    /// colour (`Ui::Text::Colorized`: a media type such as "Photo", a service action).
    pub preview_colored_len: usize,
    /// Byte ranges of `preview_text` under a spoiler, drawn hidden under the particle cover
    /// (`Reply::paint` draws `_text` with `.spoiler`).
    pub preview_spoilers: Vec<std::ops::Range<usize>>,
    /// Optional quoted excerpt if replying to a specific quote.
    pub quote_text: Option<String>,
}

/// Forwarded-from header displayed inside a message bubble.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardHeader {
    /// Original sender display name.
    pub from_name: String,
    /// Optional original peer id for jump/profile navigation.
    pub from_peer: Option<PeerId>,
    /// Formatted original date string (e.g. `"Nov 20 at 18:10"`).
    pub original_date: String,
}

/// Single reaction counter pill attached to a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionCounter {
    /// Reaction emoji string (e.g. `"👍"`, `"🔥"`, `"❤️"`).
    pub emoji: String,
    /// Total reaction count.
    pub count: u32,
    /// Whether the current user has selected this reaction.
    pub chosen: bool,
}

/// Entry from `messages.getAvailableReactions` catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableReactionItem {
    /// Unicode emoji string (e.g. `"👍"`, `"❤️"`).
    pub emoji: String,
    /// Human-readable reaction title (e.g. `"Thumbs Up"`).
    pub title: String,
    /// True if selecting this reaction requires Telegram Premium.
    pub is_premium: bool,
    /// True if the server marked this reaction inactive.
    pub inactive: bool,
}

/// Entry from `messages.getAvailableEffects` catalog (`availableEffect#93c3e27e`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableMessageEffect {
    /// Stable Telegram effect ID (`availableEffect.id`).
    pub id: u64,
    /// Representative emoji (`availableEffect.emoticon`).
    pub emoji: String,
    /// Human-readable display title.
    pub name: String,
    /// True when sending this effect requires Telegram Premium (`availableEffect.premium_required`).
    pub premium_required: bool,
    /// Optional static icon document ID (`availableEffect.static_icon_id`).
    pub static_icon_id: Option<i64>,
    /// Sticker document ID (`availableEffect.effect_sticker_id`).
    pub effect_sticker_id: i64,
    /// Optional Lottie animation document ID (`availableEffect.effect_animation_id`).
    pub effect_animation_id: Option<i64>,
    /// Optional decompressed Lottie JSON payload for effect playback.
    pub lottie_json: Option<String>,
    /// Accent badge RGB color.
    pub accent_rgb: [u8; 3],
}

/// Single peer reaction row returned by `messages.getMessageReactionsList`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionPeerEntry {
    /// Peer who reacted.
    pub peer_id: PeerId,
    /// Display name of the reacting peer.
    pub name: String,
    /// Optional `@username` (without `@`).
    pub username: Option<String>,
    /// Reaction emoji (or `"custom:<doc_id>"` / `"⭐"`).
    pub emoji: String,
    /// Unix timestamp (seconds) when the reaction was added.
    pub date_unix: i32,
    /// True if this reaction belongs to the current user.
    pub is_my: bool,
    /// Avatar palette color index (`0..7`).
    pub color_index: u8,
}

/// Paginated response for `messages.getMessageReactionsList`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MessageReactionsPage {
    /// Total number of reactions matching the active filter.
    pub total_count: u32,
    /// Peer reaction entries in this page.
    pub entries: Vec<ReactionPeerEntry>,
    /// Pagination cursor for the next page, if any.
    pub next_offset: Option<String>,
}

/// Saved Messages reaction tag from `messages.getSavedReactionTags`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedReactionTagItem {
    /// Reaction emoji (or `"custom:<doc_id>"`).
    pub emoji: String,
    /// Optional custom tag label set by the user.
    pub title: Option<String>,
    /// Number of saved messages tagged with this reaction.
    pub count: u32,
}

/// Single voter entry returned by `messages.getPollVotes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollVoterEntry {
    /// Voter peer identifier.
    pub peer_id: PeerId,
    /// Voter display name.
    pub name: String,
    /// Optional `@username` (without `@`).
    pub username: Option<String>,
    /// Zero-based poll option indices chosen by this voter.
    pub option_indices: Vec<usize>,
    /// Unix timestamp (seconds) when the vote was cast.
    pub date_unix: i32,
    /// Avatar palette color index (`0..7`).
    pub color_index: u8,
}

/// Paginated response for `messages.getPollVotes`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PollVotesPage {
    /// Total number of voters matching the active option filter.
    pub total_count: u32,
    /// Voter entries in this page.
    pub voters: Vec<PollVoterEntry>,
    /// Pagination cursor for the next page, if any.
    pub next_offset: Option<String>,
}

/// Single photo or video tile inside a grouped album.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumItem {
    /// Message id of this album part.
    pub msg_id: MsgId,
    /// Natural width in pixels.
    pub width: i32,
    /// Natural height in pixels.
    pub height: i32,
    /// Optional Telegram stripped JPEG thumbnail (`photoStrippedSize`, starts with `0x01`).
    pub stripped_thumb: Option<Vec<u8>>,
    /// Fallback procedural gradient top/bottom colors if no thumb bytes are present.
    pub fallback_rgb: ([u8; 3], [u8; 3]),
    /// Optional video duration in seconds (`None` for still photos).
    pub video_duration_secs: Option<u32>,
    /// Short descriptive label (used for accessibility / Save As).
    pub label: String,
}

/// Photo attachment metadata (`HistoryView::Photo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhotoAttachment {
    /// Natural width in pixels.
    pub width: i32,
    /// Natural height in pixels.
    pub height: i32,
    /// Optional Telegram stripped JPEG thumbnail (`photoStrippedSize`).
    pub stripped_thumb: Option<Vec<u8>>,
    /// Fallback gradient colors.
    pub fallback_rgb: ([u8; 3], [u8; 3]),
    /// Suggested file name for Save As.
    pub file_name: String,
    /// Approximate size in bytes.
    pub size_bytes: u64,
}

/// Video or GIF attachment metadata (`HistoryView::Gif`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoAttachment {
    /// Natural width in pixels.
    pub width: i32,
    /// Natural height in pixels.
    pub height: i32,
    /// Duration in seconds.
    pub duration_secs: u32,
    /// File size in bytes.
    pub size_bytes: u64,
    /// True if looping silent GIF animation.
    pub is_gif: bool,
    /// Optional Telegram stripped thumbnail bytes.
    pub stripped_thumb: Option<Vec<u8>>,
    /// Fallback gradient colors.
    pub fallback_rgb: ([u8; 3], [u8; 3]),
    /// Suggested file name.
    pub file_name: String,
}

/// Generic or thumbed document file attachment (`HistoryView::Document`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentAttachment {
    /// Display file name (e.g. `"architecture_notes.pdf"`).
    pub file_name: String,
    /// File size in bytes.
    pub size_bytes: u64,
    /// MIME type string (e.g. `"application/pdf"`).
    pub mime_type: String,
    /// Optional square thumbnail accent color (when present, uses thumbed 72px layout).
    pub thumb_rgb: Option<[u8; 3]>,
}

/// Lightweight per-message voice/round-video transcription snapshot rendered inside chat bubbles
/// (`Api::Transcribes::Entry` view).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscribeUiEntry {
    /// Current recognized text (may be partial while `pending` is true).
    pub text: String,
    /// Whether the expandable transcript panel is currently shown.
    pub shown: bool,
    /// Whether recognition is still in progress (`pending` or RPC in flight).
    pub pending: bool,
    /// Whether transcription failed.
    pub failed: bool,
    /// Whether transcription failed due to `MSG_VOICE_TOO_LONG`.
    pub too_long: bool,
    /// Whether this transcription is for a round video note.
    pub round_view: bool,
    /// True if the user can rate this completed transcription (`thumbs up / down`).
    pub can_rate: bool,
    /// True if the user already rated this transcription.
    pub rated: bool,
}

/// UI snapshot of Premium / free-trial transcription limits (`history_view_transcribe_button.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranscribeLimitsView {
    /// True if the account has Telegram Premium (unlimited transcription).
    pub is_premium: bool,
    /// True if weekly free trials are exhausted during cooldown (`TranscribeButton::hasLock`).
    pub locked: bool,
    /// Remaining free trial transcriptions in the current cooldown window.
    pub trials_remaining: u32,
    /// Unix timestamp when free trials reset (`0` if none).
    pub trials_refresh_at: i32,
    /// Maximum audio duration in seconds for free-trial transcriptions.
    pub trials_max_secs: u32,
}

impl Default for TranscribeLimitsView {
    fn default() -> Self {
        Self {
            is_premium: false,
            locked: false,
            trials_remaining: 2,
            trials_refresh_at: 0,
            trials_max_secs: 300,
        }
    }
}

/// Result of clicking the `"→A"` transcribe button on a voice or round-video bubble.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscribeToggleOutcome {
    /// Toggled visibility of an already-loaded transcription.
    Toggled {
        /// New expanded visibility state.
        shown: bool,
    },
    /// Transcription request is pending; final text will arrive via `updateTranscribedAudio`.
    Pending,
    /// Transcription completed immediately with text and optional trial-quota notice.
    Ready {
        /// Recognized text.
        text: String,
        /// Optional free-trial remaining count banner.
        trial_toast: Option<String>,
    },
    /// Blocked because weekly free trials are exhausted (`ShowPremiumPreviewBox(VoiceToText)`).
    PremiumRequired {
        /// Unix timestamp when free trials reset.
        refresh_at: i32,
    },
    /// Audio exceeded the non-Premium trial duration cap (`MSG_VOICE_TOO_LONG`).
    TooLong {
        /// Maximum allowed duration in seconds.
        max_secs: u32,
    },
    /// Backend error or unavailable.
    Failed(String),
}

/// Voice message attachment with 5-bit packed waveform (`HistoryView::Document` voice mode).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceAttachment {
    /// Duration in seconds.
    pub duration_secs: u32,
    /// Telegram 5-bit packed waveform bytes (decoded via `tg_media::audio::waveform_decode_5bit`).
    pub waveform_5bit: Vec<u8>,
    /// File size in bytes (`DocumentData::size`), `0` when not known.
    pub size_bytes: u64,
    /// Whether this voice note has not yet been listened to (renders unread blue dot).
    pub is_unread: bool,
    /// Optional speech-to-text transcription snippet.
    pub transcription: Option<String>,
}

/// Animated or static sticker attachment (`HistoryView::Sticker`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickerAttachment {
    /// Stable sticker identifier used for frame texture caching.
    pub sticker_id: u64,
    /// Associated emoji (e.g. `"✨"`).
    pub emoji: String,
    /// Sticker pack title.
    pub set_title: String,
    /// Original dimensions (typically 512x512).
    pub width: i32,
    /// Original height.
    pub height: i32,
    /// Uncompressed Lottie JSON source rendered through `tg_lottie`.
    pub lottie_json: String,
    /// The sticker's set (`StickerData::set`), opened in `StickerSetBox` on a click. `None`
    /// for a sticker without a set (a dice face, an uploaded image).
    pub set: Option<StickerSetId>,
}

/// A sticker set by id (`inputStickerSetID`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StickerSetId {
    /// Set id.
    pub id: u64,
    /// Set access hash.
    pub access_hash: u64,
}

/// Single option inside a poll or quiz (`HistoryView::Poll`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollOption {
    /// Option display text.
    pub text: String,
    /// Number of votes for this option.
    pub votes: u32,
    /// Whether the current user voted for this option.
    pub chosen: bool,
    /// Whether this option is the correct answer (for quizzes).
    pub correct: bool,
}

/// Poll or quiz attachment (`HistoryView::Poll`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollAttachment {
    /// Poll question heading.
    pub question: String,
    /// True if quiz mode (reveals correct answer after voting).
    pub is_quiz: bool,
    /// True if anonymous voting.
    pub is_anonymous: bool,
    /// True if multiple choices are allowed.
    pub multiple_choice: bool,
    /// True if poll is closed.
    pub closed: bool,
    /// Answer choices.
    pub options: Vec<PollOption>,
    /// Total voters count.
    pub total_voters: u32,
    /// Optional quiz explanation shown after voting.
    pub explanation: Option<String>,
}

/// HTML5 bot game attachment (`HistoryView::Game`, `history/view/media/history_view_game.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameAttachment {
    /// Core game metadata (`GameData`: id, access_hash, short_name, title, description, photo/doc ids).
    pub game: tg_bots::mini_app::GameData,
    /// Bot username owning this game (for canonical share link `https://t.me/<bot>?game=<short_name>`).
    pub bot_username: String,
    /// Preview photo/animation natural width in pixels.
    pub media_width: i32,
    /// Preview photo/animation natural height in pixels.
    pub media_height: i32,
    /// Whether the game preview is an animated document (`game.document_id.is_some()`).
    pub is_animation: bool,
    /// Optional Telegram stripped thumbnail bytes.
    pub stripped_thumb: Option<Vec<u8>>,
    /// Fallback gradient top/bottom RGB colors for the preview cover art.
    pub fallback_rgb: ([u8; 3], [u8; 3]),
    /// High-score leaderboard displayed in the `ShareGameScore` modal box.
    pub high_scores: Vec<tg_bots::mini_app::GameHighScore>,
}

/// Sandboxed HTML widget attachment (`HistoryView::HtmlWidget`, `tgw_media.h` / `history_view_html_widget.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetAttachment {
    /// Optional shared widget ID across messages (`messageMediaWidget.widget_id`).
    pub widget_id: Option<i64>,
    /// Requested width in CSS pixels (`0` = auto bubble width).
    pub width: i32,
    /// Requested height in CSS pixels (`0` = auto content height).
    pub height: i32,
    /// Raw compressed (zlib or gzip) widget payload bytes.
    pub payload: Vec<u8>,
    /// Unpacked UTF-8 HTML document, if decompression and UTF-8 decoding succeeded.
    pub html: Option<String>,
    /// Extracted `<title>` or fallback label for display in bubble header & previews.
    pub title: String,
    /// Short plain-text excerpt from the unpacked HTML body.
    pub preview_text: String,
    /// Whether this message is a forwarded copy (`flags.is_copy`).
    pub is_copy: bool,
    /// Whether interactive bot queries are allowed (`flags.can_query`).
    pub can_query: bool,
    /// Whether background bot queries are allowed (`flags.background_query`).
    pub background_query: bool,
    /// Optional shared public data JSON/text blob.
    pub public_data: Option<String>,
    /// Optional hash of `public_data` for conditional polling.
    pub public_data_hash: Option<i64>,
    /// Optional per-chat data JSON/text blob.
    pub chat_data: Option<String>,
}

/// Interactive story share / story mention card (`HistoryView::StoryMention`,
/// `history/view/media/history_view_story_mention.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryAttachment {
    /// Story owner peer ID.
    pub peer_id: PeerId,
    /// Story identifier within `peer_id`.
    pub story_id: i32,
    /// Display name of the story owner.
    pub owner_name: String,
    /// True if attached via a story mention.
    pub via_mention: bool,
    /// True if the story is known to have expired.
    pub expired: bool,
    /// Deterministic avatar/ring accent color RGB.
    pub accent_rgb: [u8; 3],
}

/// Interactive Telegram Premium / Stars giveaway card (`HistoryView::GenerateGiveawayStart`,
/// `history/view/media/history_view_giveaway.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GiveawayAttachment {
    /// Participating channel peer IDs.
    pub channels: Vec<PeerId>,
    /// Resolved display names of participating channels.
    pub channel_names: Vec<String>,
    /// Eligible country ISO-2 codes (empty = worldwide).
    pub countries_iso2: Vec<String>,
    /// Optional custom additional prize description.
    pub prize_description: Option<String>,
    /// Number of winners / prizes.
    pub quantity: i32,
    /// Telegram Premium subscription duration in months (if Premium giveaway).
    pub months: Option<i32>,
    /// Total Telegram Stars prize pool (if Stars giveaway).
    pub stars: Option<i64>,
    /// Giveaway end Unix timestamp.
    pub until_date: i32,
    /// Human-readable formatted end date/time string.
    pub until_date_text: String,
    /// True if only new subscribers are eligible.
    pub only_new_subscribers: bool,
    /// True if winners list will be public.
    pub winners_are_visible: bool,
}

/// Interactive Telegram giveaway results card (`HistoryView::GenerateGiveawayResults`,
/// `history/view/media/history_view_giveaway.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GiveawayResultsAttachment {
    /// Host channel peer ID.
    pub channel_id: PeerId,
    /// Host channel display name.
    pub channel_name: String,
    /// Message ID of the original giveaway launch post.
    pub launch_msg_id: MsgId,
    /// Additional participating channel count.
    pub additional_peers_count: i32,
    /// Total number of winners selected.
    pub winners_count: i32,
    /// Number of unclaimed prizes.
    pub unclaimed_count: i32,
    /// Visible winning peers `(peer_id, display_name)`.
    pub winners: Vec<(PeerId, String)>,
    /// Premium months per winner (if Premium giveaway).
    pub months: Option<i32>,
    /// Total Telegram Stars prize pool (if Stars giveaway).
    pub stars: Option<i64>,
    /// Optional custom prize description.
    pub prize_description: Option<String>,
    /// Giveaway end Unix timestamp.
    pub until_date: i32,
    /// True if the giveaway was refunded.
    pub refunded: bool,
}

/// Interactive live video stream card (`MessageMedia::VideoStream`,
/// `media/view/media_view_video_stream.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoStreamAttachment {
    /// Group call ID (`InputGroupCall.id`).
    pub call_id: i64,
    /// Group call access hash (`InputGroupCall.access_hash`).
    pub access_hash: u64,
    /// True if backed by an RTMP/HLS broadcast stream.
    pub rtmp_stream: bool,
    /// Display title for the live stream card.
    pub title: String,
    /// True while stream is currently live.
    pub is_live: bool,
}

/// Media payload attached to a [`HistoryMessage`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryMedia {
    /// Single photo.
    Photo(PhotoAttachment),
    /// Multi-item photo/video mosaic album.
    Album(Vec<AlbumItem>),
    /// Video file or looping GIF.
    Video(VideoAttachment),
    /// Downloadable file / document.
    Document(DocumentAttachment),
    /// Voice note with 5-bit waveform.
    Voice(VoiceAttachment),
    /// Animated `.tgs`/Lottie sticker.
    Sticker(StickerAttachment),
    /// Interactive poll or quiz.
    Poll(PollAttachment),
    /// Static/live map location or venue card.
    Location(LocationAttachment),
    /// Shared contact card.
    Contact(ContactAttachment),
    /// Round video note placeholder.
    RoundVideo(RoundVideoAttachment),
    /// Interactive HTML5 bot game card (`HistoryView::Game`).
    Game(GameAttachment),
    /// Collaborative checklist / to-do list (`MessageMedia::ToDo`, `history_view_todo_list.cpp`).
    TodoList(TodoListAttachment),
    /// Sandboxed interactive HTML widget card (`HistoryView::HtmlWidget`, `tgw`).
    Widget(WidgetAttachment),
    /// Interactive Story share / mention card (`HistoryView::StoryMention`).
    Story(StoryAttachment),
    /// Interactive Telegram Giveaway card (`HistoryView::GenerateGiveawayStart`).
    Giveaway(GiveawayAttachment),
    /// Interactive Telegram Giveaway Results card (`HistoryView::GenerateGiveawayResults`).
    GiveawayResults(GiveawayResultsAttachment),
    /// Interactive Live Video Stream card (`MessageMedia::VideoStream`).
    VideoStream(VideoStreamAttachment),
}

/// Service message classification (`HistoryView::Service`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceAction {
    /// Chat or channel created with title.
    ChatCreated {
        /// New group/channel title.
        title: String,
    },
    /// Group title renamed.
    TitleChanged {
        /// Updated title.
        title: String,
    },
    /// A message was pinned.
    PinnedMessage {
        /// Pinned target message id.
        msg_id: MsgId,
        /// Short preview of the pinned message.
        preview: String,
    },
    /// User joined the group.
    UserJoined {
        /// Member display name.
        name: String,
    },
    /// Checklist tasks completed or uncompleted (`messageActionTodoCompletions`).
    TodoCompletions {
        /// Newly completed task ids.
        completed: Vec<i32>,
        /// Newly uncompleted task ids.
        incompleted: Vec<i32>,
    },
    /// New tasks appended to a checklist (`messageActionTodoAppendTasks`).
    TodoAppendTasks {
        /// Titles of appended tasks.
        titles: Vec<String>,
    },
    /// Suggested post approved or declined (`messageActionSuggestedPostApproval`).
    SuggestedPostApproval {
        /// True if rejected.
        rejected: bool,
        /// True if rejected due to insufficient Stars/TON balance.
        balance_too_low: bool,
        /// Optional rejection reason comment.
        reject_comment: Option<String>,
        /// Optional scheduled publication Unix timestamp.
        schedule_date: Option<i32>,
        /// Optional formatted price string.
        price_label: Option<String>,
    },
    /// Suggested post published and paid (`messageActionSuggestedPostSuccess`).
    SuggestedPostSuccess {
        /// Formatted paid price string.
        price_label: String,
    },
    /// Suggested post payment refunded (`messageActionSuggestedPostRefund`).
    SuggestedPostRefund {
        /// True if refund was initiated by the payer.
        payer_initiated: bool,
    },
    /// 1:1 chat sharing-protection toggle (`messageActionNoForwardsToggle`).
    NoForwardsToggle {
        /// Previous `noforwards` state.
        prev_value: bool,
        /// New `noforwards` state (`true` = sharing disabled, `false` = sharing enabled).
        new_value: bool,
    },
    /// 1:1 request to enable sharing in a protected chat (`messageActionNoForwardsRequest`).
    NoForwardsRequest {
        /// True if the request expired without response.
        expired: bool,
        /// Previous `noforwards` state.
        prev_value: bool,
        /// Requested `noforwards` state.
        new_value: bool,
    },
    /// Birthday suggestion card (`messageActionSuggestBirthday`, `history_view_birthday_suggestion.cpp`).
    SuggestBirthday {
        /// Day of month (`1..=31`).
        day: u8,
        /// Month of year (`1..=12`).
        month: u8,
        /// Optional birth year.
        year: Option<u16>,
    },
    /// Suggested profile photo/video card (`messageActionSuggestProfilePhoto`, `history_view_userpic_suggestion.cpp`).
    SuggestProfilePhoto {
        /// Telegram photo ID.
        photo_id: i64,
        /// Telegram photo access hash.
        access_hash: i64,
        /// Telegram file reference bytes.
        file_reference: Vec<u8>,
        /// True if the suggested avatar has a video variant.
        is_video: bool,
        /// Optional Telegram stripped thumbnail bytes for inline preview.
        stripped_thumb: Option<Vec<u8>>,
        /// Fallback gradient colors when thumbnail bytes are absent.
        fallback_rgb: ([u8; 3], [u8; 3]),
    },
    /// Custom plain service text.
    Custom(String),
}

impl ServiceAction {
    /// Formats human-readable service pill text.
    #[must_use]
    pub fn display_text(&self, actor_name: &str) -> String {
        match self {
            Self::ChatCreated { title } => format!("{actor_name} created the group \"{title}\""),
            Self::TitleChanged { title } => {
                format!("{actor_name} changed group name to \"{title}\"")
            }
            Self::PinnedMessage { preview, .. } => {
                format!("{actor_name} pinned \"{preview}\"")
            }
            Self::UserJoined { name } => format!("{name} joined the group via invite link"),
            Self::TodoCompletions {
                completed,
                incompleted,
            } => format_todo_completions_service(actor_name, completed, incompleted),
            Self::TodoAppendTasks { titles } => format_todo_append_service(actor_name, titles),
            Self::SuggestedPostApproval {
                rejected,
                balance_too_low,
                reject_comment,
                schedule_date,
                price_label,
            } => format_suggested_post_approval_service(
                actor_name,
                *rejected,
                *balance_too_low,
                reject_comment.as_deref(),
                *schedule_date,
                price_label.as_deref(),
            ),
            Self::SuggestedPostSuccess { price_label } => {
                format_suggested_post_success_service(price_label)
            }
            Self::SuggestedPostRefund { payer_initiated } => {
                format_suggested_post_refund_service(*payer_initiated)
            }
            Self::NoForwardsToggle { new_value, .. } => {
                crate::core_types::misc::format_no_forwards_toggle_text(
                    actor_name, false, *new_value,
                )
            }
            Self::NoForwardsRequest { expired, .. } => {
                if *expired {
                    crate::core_types::misc::format_no_forwards_request_expired_text()
                } else {
                    crate::core_types::misc::format_no_forwards_request_title(actor_name, false)
                }
            }
            Self::SuggestBirthday { .. } => tg_lang::tr::lng_action_suggested_birthday(actor_name),
            Self::SuggestProfilePhoto { is_video, .. } => {
                if *is_video {
                    tg_lang::tr::lng_action_suggested_video(actor_name)
                } else {
                    tg_lang::tr::lng_action_suggested_photo(actor_name)
                }
            }
            Self::Custom(text) => text.clone(),
        }
    }
}

/// Cached AI summary state for a single message (`Api::Transcribes::SummaryEntry`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageSummaryEntry {
    /// Summarized rich text returned by `messages.summarizeText`.
    pub text: TextWithEntities,
    /// True while a `messages.summarizeText` RPC is in flight.
    pub loading: bool,
    /// True when the summarized text is currently expanded in place of the original body.
    pub shown: bool,
    /// True when the server rejected the request with `SUMMARY_FLOOD_PREMIUM`.
    pub premium_required: bool,
    /// Target language code used for this summary (empty string = server default).
    pub to_lang: String,
}

/// Optional extended metadata attached to a [`HistoryMessage`] (thread routing, scheduling,
/// silent/effect flags, inline bot attribution, translation, link preview, and comments counter).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MessageExtra {
    /// Optional forum topic root id or discussion thread root msg id.
    pub thread_id: Option<MsgId>,
    /// Optional original peer id for Saved Messages per-chat sublists.
    pub saved_from_peer: Option<PeerId>,
    /// Optional scheduled Unix timestamp (`SCHEDULED_UNTIL_ONLINE_TIMESTAMP` for send-when-online).
    pub scheduled_date: Option<i32>,
    /// True if sent silently without notification sound.
    pub silent: bool,
    /// Optional Telegram message effect id.
    pub effect_id: Option<u64>,
    /// Optional inline bot username (e.g. `"gif"` -> `"via @gif"`).
    pub via_bot: Option<String>,
    /// Optional "Send As" display label if sent as a channel/alias.
    pub send_as_name: Option<String>,
    /// Optional `(lang_code, translated_text)` displayed inline below the original text.
    pub translated_text: Option<(String, String)>,
    /// Optional link preview card rendered inside the bubble.
    pub link_preview: Option<LinkPreviewData>,
    /// Optional comments/replies count on channel posts (opens replies thread section).
    pub replies_count: Option<u32>,
    /// Optional bot reply markup (inline keyboard under bubble or reply keyboard for compose).
    pub reply_markup: Option<tg_bots::markup::ReplyMarkupData>,
    /// Optional Layer-233 rich document message payload (blocks + inline/display formulas).
    pub rich_message: Option<crate::core_types::dto::rich_message::RichMessagePayload>,
    /// Optional self-destruct / view-once media TTL in seconds (`i32::MAX` = view once).
    pub ttl_seconds: Option<i32>,
    /// True once a self-destructing / view-once media bubble has been opened and expired.
    pub ttl_viewed: bool,
    /// Optional Layer-233 poll extensions (open answers, option media/link attachments).
    pub poll_extras: Option<PollMessageExtras>,
    /// Optional local MP4 file path captured for a round video message.
    pub round_video_path: Option<String>,
    /// Optional source language code indicating the server supports `messages.summarizeText`
    /// (`Message::summary_from_language` in Layer 233).
    pub summary_from_language: Option<String>,
    /// True when sending this outgoing message failed (`HistoryItem::hasFailed`). It keeps the
    /// clock like a message still being sent (`BottomInfo` `Flag::Sending`).
    pub send_failed: bool,
    /// Rank or role badge right of the sender name (`HistoryView::RightBadge`).
    pub sender_badge: Option<SenderBadge>,
    /// Premium star or emoji status after the sender name (`Message::FromNameStatus`).
    pub sender_status: Option<SenderNameStatus>,
    /// The sender's accent (`Element::colorIndex` and `colorCollectible`): the server colour
    /// index, or a collectible colour. `None`: the id-based default of `from_id`.
    pub sender_accent: Option<tg_ui::peer_color::PeerAccent>,
    /// The message is protected (`noforwards`, `HistoryItem::forbidsForward`): its text is
    /// not copied and it is not forwarded.
    pub no_forwards: bool,
    /// Who reacted, per reaction, when the reaction buttons show userpics instead of counts in
    /// a group or channel (`InlineListData::recent`). Empty: the buttons show counts.
    pub recent_reactors: Vec<RecentReactors>,
}

/// The recent reactors of one reaction button (`InlineListData::recent` entry).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecentReactors {
    /// Reaction key, as in [`ReactionCounter::emoji`].
    pub reaction: String,
    /// The reactors in the server's order, with display names for the empty userpic.
    pub peers: Vec<(PeerId, String)>,
}

/// Who a sender is in a group, which picks the badge colour (`HistoryView::BadgeRole`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SenderBadgeRole {
    /// A member rank (`fromRank`, a basic-group rank): plain grey text (`rankUserFg`).
    #[default]
    User,
    /// An administrator: a green pill (`rankAdminFg`).
    Admin,
    /// The group owner: a purple pill (`rankOwnerFg`).
    Creator,
}

/// Text shown right of the sender name in a group (`Message::refreshRightBadge`): the custom
/// rank, or "admin" / "owner" for administrators without one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SenderBadge {
    /// Single-line badge text without emoji (`TextUtilities::RemoveEmoji`).
    pub text: String,
    /// Badge role and colour.
    pub role: SenderBadgeRole,
}

/// The icon after a sender's name: the custom emoji status, or the premium star when the
/// sender has Premium but no status (`Message::paintFromName`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SenderNameStatus {
    /// Custom emoji document id of the emoji status, if one is set.
    pub emoji_status_id: Option<u64>,
    /// True when the sender has Telegram Premium.
    pub premium: bool,
}

/// Rich chat history message item owned by the history view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryMessage {
    /// Message id within the peer's history.
    pub id: MsgId,
    /// Owning peer id.
    pub peer_id: PeerId,
    /// Sender peer id.
    pub from_id: PeerId,
    /// Sender display name.
    pub sender_name: String,
    /// Unix timestamp in seconds (`TimeId`).
    pub date_unix: i32,
    /// True if sent by the current account.
    pub outgoing: bool,
    /// True if this is a broadcast channel post.
    pub is_post: bool,
    /// Formatted message text and UTF-16 entities (`tg_text::TextWithEntities`).
    pub text: TextWithEntities,
    /// Optional attached media.
    pub media: Option<HistoryMedia>,
    /// Optional service action (when `Some`, rendered as a centered service message).
    pub service: Option<ServiceAction>,
    /// Optional reply header.
    pub reply: Option<ReplyHeader>,
    /// Optional forward header.
    pub forward: Option<ForwardHeader>,
    /// Reactions row.
    pub reactions: Vec<ReactionCounter>,
    /// Optional channel/post view counter.
    pub views: Option<u32>,
    /// True if the message was edited.
    pub edited: bool,
    /// True if this outgoing message was read by the recipient.
    pub is_read: bool,
    /// True if still sending (clock indicator).
    pub is_sending: bool,
    /// True if pinned in this chat.
    pub is_pinned: bool,
    /// Extended section/compose/action metadata.
    pub extra: MessageExtra,
}

impl HistoryMessage {
    /// Returns `true` if this is a service message.
    #[must_use]
    pub const fn is_service(&self) -> bool {
        self.service.is_some()
    }

    /// Returns a compact single-line preview suitable for pinned/reply/forward bars.
    #[must_use]
    pub fn preview_snippet(&self) -> String {
        if let Some(ref svc) = self.service {
            return svc.display_text(&self.sender_name);
        }
        if let Some(ref rm) = self.extra.rich_message {
            let summary = rm.summary_text();
            if !summary.is_empty() {
                return truncate_chars(&summary.replace('\n', " "), 72);
            }
        }
        let body = self.text.text.trim();
        let prefix = match &self.media {
            Some(HistoryMedia::Photo(_)) => "Photo",
            Some(HistoryMedia::Album(items)) => {
                if body.is_empty() {
                    return format!("Album ({} items)", items.len());
                }
                "Album"
            }
            Some(HistoryMedia::Video(v)) => {
                if v.is_gif {
                    "GIF"
                } else {
                    "Video"
                }
            }
            Some(HistoryMedia::Document(d)) => {
                if body.is_empty() {
                    return d.file_name.clone();
                }
                "File"
            }
            Some(HistoryMedia::Voice(v)) => {
                return format!(
                    "Voice message ({}:{:02})",
                    v.duration_secs / 60,
                    v.duration_secs % 60
                );
            }
            Some(HistoryMedia::Sticker(s)) => {
                return format!("{} Sticker", s.emoji);
            }
            Some(HistoryMedia::Poll(p)) => {
                return format!("Poll: {}", p.question);
            }
            Some(HistoryMedia::Location(loc)) => {
                if let Some(ref title) = loc.venue_title {
                    return format!("📍 {title}");
                }
                return format!("📍 Location ({:.3}, {:.3})", loc.lat, loc.lon);
            }
            Some(HistoryMedia::Contact(c)) => {
                return format!("👤 Contact: {}", c.full_name());
            }
            Some(HistoryMedia::RoundVideo(rv)) => {
                return format!(
                    "Video message ({}:{:02})",
                    rv.duration_secs / 60,
                    rv.duration_secs % 60
                );
            }
            Some(HistoryMedia::Game(g)) => {
                return format!("🎮 {}", g.game.title);
            }
            Some(HistoryMedia::TodoList(todo)) => {
                let title = if todo.title.text.is_empty() {
                    "Checklist"
                } else {
                    todo.title.text.as_str()
                };
                return format!(
                    "☑ {title} ({}/{})",
                    todo.completed_count(),
                    todo.total_count()
                );
            }
            Some(HistoryMedia::Widget(w)) => {
                return if w.title.is_empty() {
                    "Widget".to_owned()
                } else {
                    format!("🧩 {}", w.title)
                };
            }
            Some(HistoryMedia::Story(st)) => {
                return if st.via_mention {
                    format!("Story mention: {}'s Story", st.owner_name)
                } else {
                    format!("{}'s Story", st.owner_name)
                };
            }
            Some(HistoryMedia::Giveaway(g)) => {
                return if let Some(stars) = g.stars {
                    format!("🎁 Giveaway: {stars} ⭐ ({} winners)", g.quantity.max(1))
                } else {
                    format!("🎁 Giveaway ({} prizes)", g.quantity.max(1))
                };
            }
            Some(HistoryMedia::GiveawayResults(r)) => {
                return if r.refunded {
                    "Giveaway refunded".to_owned()
                } else {
                    format!("🏆 Giveaway Results ({} winners)", r.winners_count.max(0))
                };
            }
            Some(HistoryMedia::VideoStream(vs)) => {
                return format!("🔴 Live Stream: {}", vs.title);
            }
            None => "",
        };
        let combined = if prefix.is_empty() {
            body.replace('\n', " ")
        } else if body.is_empty() {
            prefix.to_owned()
        } else {
            format!("{prefix}: {}", body.replace('\n', " "))
        };
        truncate_chars(&combined, 72)
    }

    /// Formats the `HH:MM` timestamp from `date_unix`.
    #[must_use]
    pub fn formatted_time(&self) -> String {
        let dt = tg_history::LocalDateTime::from_unix_utc(i64::from(self.date_unix));
        format!("{:02}:{:02}", dt.hour, dt.minute)
    }
}

/// Truncates a string to at most `max_chars` Unicode scalar values, appending `"..."`.
#[must_use]
pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}...")
    } else {
        head
    }
}

/// Summary of one pinned message displayed in `PinnedBar`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedMessageSummary {
    /// Pinned message id.
    pub msg_id: MsgId,
    /// Sender name of the pinned message.
    pub sender_name: String,
    /// Preview snippet text.
    pub preview: String,
    /// Byte ranges of `preview` under a spoiler, drawn hidden (`MessageBar` draws its text
    /// with `.spoiler`).
    pub preview_spoilers: Vec<std::ops::Range<usize>>,
}

/// Optional contact status action banner below the top bar (`history_view_contact_status.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContactStatusBanner {
    /// Non-contact direct message: offers Add to Contacts, Block User, and Report Spam.
    AddOrBlock {
        /// Peer display name.
        peer_name: String,
    },
    /// User blocked banner: offers Unblock.
    Blocked,
    /// Request to share phone number (`contacts.acceptContact`).
    SharePhoneNumber {
        /// Peer display name.
        peer_name: String,
    },
    /// Unverified user/group/channel spam banner (`messages.reportSpam`).
    ReportSpam {
        /// Peer display name.
        peer_name: String,
        /// `true` for broadcast channels, `false` for groups/users.
        is_channel: bool,
    },
}

/// Saved per-peer compose draft (`Data::Draft`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DraftMessage {
    /// Draft text with entities (or raw markdown text).
    pub text: String,
    /// Optional reply target message id.
    pub reply_to: Option<MsgId>,
}

/// Saved per-thread compose draft (supports forum topics, replies threads, saved sublists,
/// quoted replies, and disabled link preview flags).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ThreadDraft {
    /// Raw compose draft text.
    pub text: String,
    /// Optional reply target message id.
    pub reply_to: Option<MsgId>,
    /// Optional quoted passage within `reply_to`.
    pub reply_quote: Option<ReplyQuote>,
    /// True if the user disabled link previews for this draft.
    pub no_webpage: bool,
}

/// Request payload for sending a new message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendMessageRequest {
    /// Parsed message text with entities.
    pub text: TextWithEntities,
    /// Optional message id being replied to.
    pub reply_to: Option<MsgId>,
    /// True if sent silently (no notification sound).
    pub silent: bool,
}

/// Extended outgoing message payload covering scheduling, reply quotes, send-as peer,
/// message effects, link preview editing, inline bot attribution, media, and thread routing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SendMessageExtended {
    /// Parsed message text with entities.
    pub text: TextWithEntities,
    /// Optional message id being replied to.
    pub reply_to: Option<MsgId>,
    /// Optional quoted excerpt within `reply_to`.
    pub reply_quote: Option<ReplyQuote>,
    /// True if sent silently without notification sound.
    pub silent: bool,
    /// Optional schedule Unix timestamp (or `SCHEDULED_UNTIL_ONLINE_TIMESTAMP`).
    pub schedule_date: Option<i32>,
    /// Optional peer id to send as (channel/alias).
    pub send_as: Option<PeerId>,
    /// Optional message effect id.
    pub effect_id: Option<u64>,
    /// True if link preview was disabled by the user.
    pub no_webpage: bool,
    /// Optional resolved link preview to attach when `!no_webpage`.
    pub link_preview: Option<LinkPreviewData>,
    /// Optional inline bot username (`via @bot`).
    pub via_bot: Option<String>,
    /// Optional attached media payload.
    pub media: Option<HistoryMedia>,
    /// Optional target thread key (topic, comments thread, saved sublist).
    pub thread_key: Option<ThreadKey>,
    /// Optional self-destruct / view-once TTL in seconds (`i32::MAX` = view once).
    pub ttl_seconds: Option<i32>,
    /// Optional local captured MP4 path for a round video message.
    pub round_video_path: Option<String>,
    /// Optional custom JPEG cover image bytes for a video attachment
    /// (`InputMediaUploadedDocument::video_cover`).
    pub video_cover_jpeg: Option<Vec<u8>>,
    /// Encoded OGG/Opus bytes of a just-recorded voice message (`HistoryMedia::Voice`),
    /// the `result.bytes` tdesktop hands to `ApiWrap::sendVoiceMessage`.
    pub voice_ogg: Option<Vec<u8>>,
}

/// Windowed slice returned by [`HistoryBackend::load_slice_around`], `before`, or `after`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HistorySlice {
    /// Ordered oldest-to-newest messages in this slice.
    pub messages: Vec<HistoryMessage>,
    /// True if the oldest message in the chat is included (no more older items).
    pub loaded_at_top: bool,
    /// True if the newest message in the chat is included (no more newer items).
    pub loaded_at_bottom: bool,
    /// Total message count in the peer's history (for diagnostics/pagination).
    pub total_count: usize,
    /// First unread message id (if an unread bar should be rendered above it).
    pub first_unread_id: Option<MsgId>,
    /// Current unread message counter (for the scroll-to-bottom FAB badge).
    pub unread_count: u32,
}

/// Result of a read that may need the network first: either the data is known now, or it is
/// being fetched in the background and the backend wakes the UI when it arrives (the caller asks
/// again then; the version methods change at the same time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loadable<T> {
    /// The request is in flight; show what the view shows while loading and ask again later.
    Loading,
    /// The data is known.
    Ready(T),
    /// The request failed; the caller keeps what it shows.
    Failed,
}

impl<T> Loadable<T> {
    /// The data, if it is known.
    pub fn ready(self) -> Option<T> {
        match self {
            Self::Ready(value) => Some(value),
            Self::Loading | Self::Failed => None,
        }
    }

    /// `true` while the request is in flight.
    #[must_use]
    pub const fn is_loading(&self) -> bool {
        matches!(self, Self::Loading)
    }
}

static UNVERSIONED_BACKEND_VERSION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

/// Value returned by the default `dialogs_version` / `chat_version` of backends that do not track
/// changes: a new number on every call, so a version-keyed cache never reuses stale data.
#[must_use]
pub fn unversioned_backend_version() -> u64 {
    UNVERSIONED_BACKEND_VERSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Incremental live change event pushed from a session backend to an already-open history view.
///
/// Port origin: `History::addNewMessage` / `HistoryWidget::newItemAdded`,
/// `HistoryWidget::itemEdited`, `HistoryWidget::itemRemoved`, and `Data::HistoryUpdate`
/// (`OUTBOX_READ` / `UNREAD_VIEW`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryLiveUpdate {
    /// A new message arrived in this peer's history.
    MessageAdded(Box<HistoryMessage>),
    /// An existing message in this peer's history was edited or updated in place
    /// (text/caption edit, reactions, views, pinned flag, or sent-message promotion).
    MessageEdited(Box<HistoryMessage>),
    /// One or more messages were deleted from this peer's history.
    MessagesDeleted(Vec<MsgId>),
    /// Outgoing messages up to `max_id` were read by the recipient (double checkmarks).
    OutboxRead {
        /// Highest outgoing message id read by the recipient.
        max_id: MsgId,
    },
    /// Unread counter or inbox read watermark changed for this peer.
    UnreadChanged {
        /// Updated unread message count.
        unread_count: u32,
        /// Updated inbox read watermark (`inboxReadTillId`).
        inbox_read_till: MsgId,
    },
    /// The underlying live change stream lagged or overflowed; reload the open slice.
    ReloadRequired,
    /// A local message (`MsgId::is_local`) was sent: `message` (with its server id) takes its
    /// place in the list (`Data::Session::itemIdChanged`).
    LocalMessageSent {
        /// Id of the local message.
        local_id: MsgId,
        /// The sent message.
        message: Box<HistoryMessage>,
    },
    /// Sending a local message failed (`HistoryItem::sendFailed`); it stays in the list.
    LocalMessageFailed {
        /// Id of the local message.
        local_id: MsgId,
        /// Error to show, once per failed send.
        error: Option<String>,
    },
}

/// Extended peer metadata consumed by the history top bar and status banners.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPeerInfo {
    /// Base peer summary.
    pub summary: PeerSummary,
    /// Optional live typing status overriding `summary.status` (e.g. `"Sam Chen is typing..."`).
    pub typing_text: Option<String>,
    /// Optional contact status banner (`history_view_contact_status.cpp`).
    pub contact_status: Option<ContactStatusBanner>,
    /// Unread message count for this peer.
    pub unread_count: u32,
    /// True if the user can pin messages in this chat.
    pub can_pin: bool,
    /// True if the user can delete messages in this chat.
    pub can_delete: bool,
    /// Public link prefix for "Copy Message Link" (e.g. `"https://t.me/rustgram_core"`).
    pub link_base: String,
}

impl HistoryPeerInfo {
    /// Builds a reasonable fallback [`HistoryPeerInfo`] from a shell [`PeerSummary`].
    #[must_use]
    pub fn from_summary(summary: PeerSummary) -> Self {
        let link_base = summary
            .username
            .as_deref()
            .map(|u| format!("https://t.me/{u}"))
            .unwrap_or_else(|| format!("https://t.me/c/{}", summary.id.bare()));
        let can_pin = !matches!(summary.kind, PeerKind::Channel) || summary.can_send;
        Self {
            summary,
            typing_text: None,
            contact_status: None,
            unread_count: 0,
            can_pin,
            can_delete: true,
            link_base,
        }
    }
}

/// Active outgoing media upload progress for a chat (`Storage::Uploader`).
#[derive(Debug, Clone, PartialEq)]
pub struct MediaUploadStatus {
    /// Stable upload identifier within the session.
    pub upload_id: u64,
    /// Destination chat peer id.
    pub peer_id: PeerId,
    /// Display file name (or album label).
    pub file_name: String,
    /// Acknowledged uploaded bytes so far.
    pub uploaded_bytes: u64,
    /// Total bytes to upload.
    pub total_bytes: u64,
    /// True if the upload failed.
    pub failed: bool,
    /// True if the upload was cancelled.
    pub cancelled: bool,
}

impl MediaUploadStatus {
    /// Returns normalized upload progress in `0.0..=1.0`.
    #[must_use]
    pub fn progress(&self) -> f64 {
        if self.total_bytes == 0 {
            0.0
        } else {
            (self.uploaded_bytes as f64 / self.total_bytes as f64).clamp(0.0, 1.0)
        }
    }
}

/// Dedicated backend trait for the chat history view.
///
/// Kept independent from `crate::stand_in::backend::Backend` so parallel agents editing
/// other `tg-app` modules never conflict. Implemented for `MockBackend` and `MockHistoryStore`
/// in [`crate::history::mock`].
/// One page of in-chat search results ([`HistoryBackend::search_chat_messages`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatSearchPage {
    /// Matching message ids, newest first.
    pub ids: Vec<MsgId>,
    /// Total number of matches in the chat (`messages.messagesSlice.count`).
    pub total: usize,
    /// `offset_id` for the next (older) page, `None` once every match is loaded.
    pub next_offset: Option<MsgId>,
}

pub trait HistoryBackend: Send {
    /// Version of what the read methods return for `peer`'s chat view (`peer_info`, the slices,
    /// pinned bar, typing, drafts, ...): it changes whenever their result may have changed, so a
    /// caller may keep the last result while the version stays the same. Read it *before* the
    /// data it guards. Drain-style calls (`drain_history_updates`) must still be drained, and
    /// text derived from the clock is not covered.
    ///
    /// The default never repeats a value, so callers that cache by it always reload.
    fn chat_version(&self, peer: PeerId) -> u64 {
        let _ = peer;
        unversioned_backend_version()
    }

    /// Tells the backend whether `peer`'s history is shown in a chat view, so the messages of
    /// chats nobody looks at may be unloaded (`History::unloadBlocks`). Never waits.
    fn set_history_open(&self, peer: PeerId, open: bool) {
        let _ = (peer, open);
    }

    /// Non-blocking [`Self::load_slice_around`]: [`Loadable::Loading`] while the messages are
    /// requested in the background (the chat version changes and the UI is woken when they
    /// arrive). The default calls the blocking method.
    fn try_load_slice_around(
        &self,
        peer_id: PeerId,
        around_id: MsgId,
        limit_before: usize,
        limit_after: usize,
    ) -> Loadable<HistorySlice> {
        Loadable::Ready(self.load_slice_around(peer_id, around_id, limit_before, limit_after))
    }

    /// Non-blocking [`Self::load_slice_before`] (see [`Self::try_load_slice_around`]).
    fn try_load_slice_before(
        &self,
        peer_id: PeerId,
        before_id: MsgId,
        limit: usize,
    ) -> Loadable<HistorySlice> {
        Loadable::Ready(self.load_slice_before(peer_id, before_id, limit))
    }

    /// Non-blocking [`Self::load_slice_after`] (see [`Self::try_load_slice_around`]).
    fn try_load_slice_after(
        &self,
        peer_id: PeerId,
        after_id: MsgId,
        limit: usize,
    ) -> Loadable<HistorySlice> {
        Loadable::Ready(self.load_slice_after(peer_id, after_id, limit))
    }

    /// Returns the authenticated user's peer id and display name.
    fn self_peer(&self) -> (PeerId, String);

    /// Returns extended peer metadata for `peer_id`.
    fn peer_info(&self, peer_id: PeerId) -> Option<HistoryPeerInfo>;

    /// Loads a slice centered around `around_id` (or newest messages if `around_id == MsgId::SHOW_AT_END` / `SHOW_AT_UNREAD`).
    fn load_slice_around(
        &self,
        peer_id: PeerId,
        around_id: MsgId,
        limit_before: usize,
        limit_after: usize,
    ) -> HistorySlice;

    /// Loads up to `limit` messages strictly older than `before_id`.
    fn load_slice_before(&self, peer_id: PeerId, before_id: MsgId, limit: usize) -> HistorySlice;

    /// Loads up to `limit` messages strictly newer than `after_id`.
    fn load_slice_after(&self, peer_id: PeerId, after_id: MsgId, limit: usize) -> HistorySlice;

    /// Drains any pending live history updates for `peer_id` since the previous frame
    /// (incoming/outgoing messages, edits, deletions, outbox/inbox read updates).
    ///
    /// Default implementation returns an empty list for static/mock backends.
    fn drain_history_updates(&mut self, _peer_id: PeerId) -> Vec<HistoryLiveUpdate> {
        Vec::new()
    }

    /// Sends a new outgoing message to `peer_id`.
    fn send_message(
        &mut self,
        peer_id: PeerId,
        request: SendMessageRequest,
    ) -> Result<HistoryMessage, String>;

    /// Edits an existing message's text and entities.
    fn edit_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        text: TextWithEntities,
    ) -> Result<HistoryMessage, String>;

    /// Deletes one or more messages from `peer_id`.
    fn delete_messages(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
        revoke_for_all: bool,
    ) -> Result<(), String>;

    /// Forwards `msg_ids` from `from_peer` into `to_peer`.
    fn forward_messages(
        &mut self,
        from_peer: PeerId,
        msg_ids: &[MsgId],
        to_peer: PeerId,
    ) -> Result<Vec<HistoryMessage>, String>;

    /// Marks messages up to `max_id` as read.
    fn mark_history_read(&mut self, peer_id: PeerId, max_id: MsgId);

    /// Reports a local typing/recording action for `peer_id`.
    fn send_typing(&mut self, peer_id: PeerId, action: TypingAction);

    /// Toggles `emoji` reaction on `msg_id`, returning the updated reactions list.
    fn toggle_reaction(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        emoji: &str,
    ) -> Result<Vec<ReactionCounter>, String>;

    /// Pins or unpins `msg_id` in `peer_id`, returning the updated pinned list (newest-first).
    fn pin_message(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        unpin: bool,
    ) -> Result<Vec<PinnedMessageSummary>, String>;

    /// Returns all currently pinned messages for `peer_id` (newest-first).
    fn pinned_messages(&self, peer_id: PeerId) -> Vec<PinnedMessageSummary>;

    /// Submits a poll vote for `option_indices` on `msg_id`.
    fn vote_poll(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        option_indices: &[usize],
    ) -> Result<(), String>;

    /// Saves or clears the compose draft for `peer_id`.
    fn save_draft(&mut self, peer_id: PeerId, draft: Option<DraftMessage>);

    /// Loads the saved compose draft for `peer_id`, if any.
    fn load_draft(&self, peer_id: PeerId) -> Option<DraftMessage>;

    /// Dismisses the contact status banner for `peer_id`.
    fn dismiss_contact_status(&mut self, peer_id: PeerId);

    /// Adds `peer_id` to contacts and clears the contact banner.
    fn add_contact(&mut self, peer_id: PeerId);

    /// Toggles block status for `peer_id`.
    fn toggle_block_peer(&mut self, peer_id: PeerId);

    /// Toggles mute status for `peer_id`, returning the new muted flag.
    fn toggle_mute(&mut self, peer_id: PeerId) -> bool;

    // -----------------------------------------------------------------------
    // Extended sections, compose extras, and message actions
    // -----------------------------------------------------------------------

    /// Sends a message with full extended options (schedule, quote, send-as, effect, media, thread).
    fn send_message_extended(
        &mut self,
        peer_id: PeerId,
        request: SendMessageExtended,
    ) -> Result<HistoryMessage, String> {
        self.send_message(
            peer_id,
            SendMessageRequest {
                text: request.text,
                reply_to: request.reply_to,
                silent: request.silent,
            },
        )
    }

    /// Creates and sends a new poll or quiz message in `peer_id` (or `thread_key`).
    fn send_poll(
        &mut self,
        _peer_id: PeerId,
        _thread_key: Option<ThreadKey>,
        _request: CreatePollRequest,
    ) -> Result<HistoryMessage, String> {
        Err("Polls are not supported by this backend.".to_owned())
    }

    /// Forwards `msg_ids` from `from_peer` into `to_peer` with optional hide-sender / hide-captions flags.
    fn forward_messages_with_options(
        &mut self,
        from_peer: PeerId,
        msg_ids: &[MsgId],
        to_peer: PeerId,
        _options: ForwardOptions,
    ) -> Result<Vec<HistoryMessage>, String> {
        self.forward_messages(from_peer, msg_ids, to_peer)
    }

    /// Returns scheduled messages for `peer_id` (optionally scoped to a forum topic).
    fn scheduled_messages(
        &self,
        _peer_id: PeerId,
        _topic_root_id: Option<MsgId>,
    ) -> Vec<HistoryMessage> {
        Vec::new()
    }

    /// Immediately dispatches a scheduled message into the regular history.
    fn send_scheduled_now(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
    ) -> Result<HistoryMessage, String> {
        Err("Scheduled messages are not supported by this backend.".to_owned())
    }

    /// Updates the schedule timestamp (or "send when online") of a scheduled message.
    fn reschedule_message(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _target: ScheduleTarget,
    ) -> Result<HistoryMessage, String> {
        Err("Rescheduling is not supported by this backend.".to_owned())
    }

    /// Deletes one or more scheduled messages.
    fn delete_scheduled_messages(
        &mut self,
        _peer_id: PeerId,
        _msg_ids: &[MsgId],
    ) -> Result<(), String> {
        Ok(())
    }

    /// Returns full [`HistoryMessage`] items for all pinned messages in `peer_id` (or topic).
    fn pinned_full_messages(
        &self,
        _peer_id: PeerId,
        _topic_root_id: Option<MsgId>,
    ) -> Vec<HistoryMessage> {
        Vec::new()
    }

    /// Unpins all messages in `peer_id` (or topic).
    fn unpin_all_messages(
        &mut self,
        _peer_id: PeerId,
        _topic_root_id: Option<MsgId>,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Returns the root channel post and its discussion replies for a comments thread.
    fn replies_root_and_comments(
        &self,
        _peer_id: PeerId,
        _root_msg_id: MsgId,
    ) -> Option<(HistoryMessage, Vec<HistoryMessage>)> {
        None
    }

    /// Returns all forum topics for a forum supergroup peer.
    fn forum_topics(&self, _peer_id: PeerId) -> Vec<ForumTopic> {
        Vec::new()
    }

    /// Creates a new forum topic in `peer_id`.
    fn create_forum_topic(
        &mut self,
        _peer_id: PeerId,
        _title: String,
        _icon_color: u32,
        _custom_emoji: Option<String>,
    ) -> Result<ForumTopic, String> {
        Err("Forum topics are not supported by this backend.".to_owned())
    }

    /// Toggles pin state of a forum topic, returning the new `is_pinned` value.
    fn toggle_forum_topic_pin(
        &mut self,
        _peer_id: PeerId,
        _topic_root_id: MsgId,
    ) -> Result<bool, String> {
        Err("Forum topics are not supported by this backend.".to_owned())
    }

    /// Toggles closed/locked state of a forum topic, returning the new `is_closed` value.
    fn toggle_forum_topic_closed(
        &mut self,
        _peer_id: PeerId,
        _topic_root_id: MsgId,
    ) -> Result<bool, String> {
        Err("Forum topics are not supported by this backend.".to_owned())
    }

    /// Loads messages belonging to a specific forum topic.
    fn load_topic_messages(&self, _peer_id: PeerId, _topic_root_id: MsgId) -> Vec<HistoryMessage> {
        Vec::new()
    }

    /// Returns default selectable custom emoji icons for forum topics
    /// (`Data::ForumIcons` / `messages.getStickerSet(inputStickerSetEmojiDefaultTopicIcons)`).
    fn forum_topic_icons(&self) -> Vec<crate::history::sections_model::ForumTopicIconEntry> {
        crate::history::sections_model::default_forum_topic_icons()
    }

    /// Edits an existing forum topic's title and/or custom emoji icon (`messages.editForumTopic`).
    fn edit_forum_topic(
        &mut self,
        _peer_id: PeerId,
        _topic_root_id: MsgId,
        _title: Option<String>,
        _icon_emoji_id: Option<i64>,
    ) -> Result<ForumTopic, String> {
        Err("Editing forum topics is not supported by this backend.".to_owned())
    }

    /// Reorders pinned forum topics (`messages.reorderPinnedForumTopics`).
    fn reorder_pinned_forum_topics(
        &mut self,
        _peer_id: PeerId,
        _order: &[MsgId],
    ) -> Result<(), String> {
        Ok(())
    }

    /// Returns Saved Messages per-chat sublists.
    fn saved_sublists(&self, _peer_id: PeerId) -> Vec<SavedSublist> {
        Vec::new()
    }

    /// Loads messages belonging to one Saved Messages sublist.
    fn load_saved_sublist_messages(
        &self,
        _peer_id: PeerId,
        _sublist_peer_id: PeerId,
    ) -> Vec<HistoryMessage> {
        Vec::new()
    }

    /// Returns filtered Channel Admin Log / Recent Actions entries.
    fn admin_log_entries(&self, _peer_id: PeerId, _filter: &AdminLogFilter) -> Vec<AdminLogEntry> {
        Vec::new()
    }

    /// Returns administrators selectable in the Admin Log Filter box.
    fn admin_log_admins(&self, _peer_id: PeerId) -> Vec<AdminLogAdmin> {
        Vec::new()
    }

    /// Saves or clears a per-thread compose draft.
    fn save_thread_draft(&mut self, thread: ThreadKey, draft: Option<ThreadDraft>) {
        if let ThreadKey::Main(peer_id) = thread {
            self.save_draft(
                peer_id,
                draft.map(|d| DraftMessage {
                    text: d.text,
                    reply_to: d.reply_to,
                }),
            );
        }
    }

    /// Loads a saved per-thread compose draft.
    fn load_thread_draft(&self, thread: ThreadKey) -> Option<ThreadDraft> {
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

    /// Returns available "Send As" peers for `peer_id`.
    fn send_as_peers(&self, _peer_id: PeerId) -> Vec<SendAsPeer> {
        Vec::new()
    }

    /// Returns the persisted default "Send As" peer for `peer_id`, if any.
    fn default_send_as_peer(&self, _peer_id: PeerId) -> Option<PeerId> {
        None
    }

    /// Saves the default "Send As" peer for `peer_id` (`messages.saveDefaultSendAs`).
    fn save_default_send_as(&mut self, _peer_id: PeerId, _send_as: PeerId) -> Result<(), String> {
        Ok(())
    }

    /// Returns slow mode configuration and active cooldown for `peer_id`.
    fn slow_mode_state(&self, _peer_id: PeerId) -> SlowModeState {
        SlowModeState::default()
    }

    /// Records a sent message timestamp against `peer_id`'s slow mode timer.
    fn record_slow_mode_send(&mut self, _peer_id: PeerId, _now_unix: i64) {}

    /// Returns the backend's authoritative current Unix timestamp in seconds (`None` lets the UI
    /// use its deterministic mock clock for offline/demo tests).
    fn current_unix_time(&self) -> Option<i64> {
        None
    }

    /// Resolves a link preview card for `url`.
    fn resolve_link_preview(&self, _url: &str) -> Option<LinkPreviewData> {
        None
    }

    /// Queries an inline bot (`@bot_username query`) for results.
    fn query_inline_bot(&self, _bot_username: &str, _query: &str) -> Vec<InlineBotResult> {
        Vec::new()
    }

    /// Returns shareable contacts for the Share Contact box.
    fn contacts(&self) -> Vec<ContactAttachment> {
        Vec::new()
    }

    /// Returns nearby placeholder venues for the Location picker box.
    fn nearby_venues(&self) -> Vec<VenueItem> {
        Vec::new()
    }

    /// Queries nearby venues around `(lat, lon)` with an optional search `query`
    /// (`contacts.getLocated` / inline venue bot as in tdesktop).
    fn nearby_venues_for_coords(&self, _lat: f64, _lon: f64, _query: &str) -> Vec<VenueItem> {
        self.nearby_venues()
    }

    /// Downloads a static map tile image for `(lat, lon)` via
    /// `upload.getWebFile(InputWebFileGeoPointLocation)`.
    fn load_static_map_tile(
        &self,
        _lat: f64,
        _lon: f64,
        _width: i32,
        _height: i32,
        _zoom: u8,
        _scale: u8,
    ) -> Result<Vec<u8>, String> {
        Err("Static map tiles are unavailable offline.".to_owned())
    }

    /// Starts downloading a static map tile image for `(lat, lon)` asynchronously via
    /// `upload.getWebFile(InputWebFileGeoPointLocation)` without blocking the UI thread.
    ///
    /// Live backends spawn the RPC on their `tokio` runtime and invoke `repaint.request_repaint()`
    /// once bytes arrive; synchronous mock backends fall back to an immediately ready result.
    #[allow(clippy::too_many_arguments)]
    fn load_static_map_tile_async(
        &self,
        lat: f64,
        lon: f64,
        width: i32,
        height: i32,
        zoom: u8,
        scale: u8,
        repaint: Option<egui::Context>,
    ) -> crate::core_types::platform_call::PlatformCall<Result<Vec<u8>, String>> {
        let _ = repaint;
        crate::core_types::platform_call::PlatformCall::ready(
            self.load_static_map_tile(lat, lon, width, height, zoom, scale),
        )
    }

    /// Returns the user's ISO 3166-1 alpha-2 country code (e.g. `"US"`, `"DE"`, `"JP"`) derived
    /// from the logged-in account's phone number or `help.getNearestDc`, used to center the
    /// location picker without prompting for OS geolocation.
    fn default_location_country_iso2(&self) -> Option<String> {
        None
    }

    /// Returns the initial `(latitude, longitude)` centre for the location picker when no
    /// explicit location was provided, derived from [`Self::default_location_country_iso2`].
    fn default_location_center(&self) -> Option<(f64, f64)> {
        self.default_location_country_iso2()
            .and_then(|iso2| crate::core_types::country_bounds::country_center(&iso2))
    }

    /// Searches messages inside `peer_id` (`messages.search` via `Api::MessagesSearch`, the
    /// in-chat search of `HistoryView::ComposeSearch`): one page of matches newest first,
    /// starting below `offset_id` (`MsgId(0)` = newest), optionally only from `from`.
    ///
    /// The default scans the locally available history, which is what mock backends offer.
    fn search_chat_messages(
        &self,
        peer_id: PeerId,
        query: &str,
        from: Option<PeerId>,
        offset_id: MsgId,
        limit: i32,
    ) -> Result<ChatSearchPage, String> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() && from.is_none() {
            return Ok(ChatSearchPage::default());
        }
        let slice = self.load_slice_around(peer_id, MsgId::SHOW_AT_END, 100_000, 0);
        let mut ids: Vec<MsgId> = slice
            .messages
            .iter()
            .filter(|m| !m.is_service())
            .filter(|m| from.is_none_or(|f| m.from_id == f))
            .filter(|m| needle.is_empty() || m.text.text.to_lowercase().contains(&needle))
            .map(|m| m.id)
            .collect();
        ids.sort_by_key(|id| std::cmp::Reverse(id.0));
        let total = ids.len();
        let start = if offset_id.0 > 0 {
            ids.iter()
                .position(|id| id.0 < offset_id.0)
                .unwrap_or(total)
        } else {
            0
        };
        let end = (start + limit.max(1) as usize).min(total);
        let page: Vec<MsgId> = ids[start..end].to_vec();
        let next_offset = (end < total).then(|| page.last().copied()).flatten();
        Ok(ChatSearchPage {
            ids: page,
            total,
            next_offset,
        })
    }

    /// Loads per-day message counts & anchor message IDs for `(year, month)` inside `peer_id`
    /// (`messages.getSearchResultsCalendar`).
    fn search_results_calendar(
        &self,
        _peer_id: PeerId,
        _year: i32,
        _month: u32,
    ) -> Result<Vec<crate::core_types::dto::calendar::CalendarDayInfo>, String> {
        Ok(Vec::new())
    }

    /// Edits a media message's caption, `invert_media` / `spoiler` flags, and optionally replaces
    /// the attached media (`messages.editMessage` with `media`).
    fn edit_message_media(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        request: crate::core_types::dto::edit_caption::EditMediaCaptionRequest,
    ) -> Result<HistoryMessage, String> {
        self.edit_message(peer_id, msg_id, request.caption)
    }

    /// Translates `msg_id` to `target_lang` (UI hook; backend implementation).
    fn translate_message(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _target_lang: &str,
    ) -> Result<String, String> {
        Err("Translation is not available for this backend.".to_owned())
    }

    /// Batch-translates multiple messages in `peer_id` to `target_lang` (whole-chat translate bar).
    fn translate_messages_batch(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
        target_lang: &str,
    ) -> Result<Vec<(MsgId, String)>, String> {
        let mut out = Vec::with_capacity(msg_ids.len());
        for &mid in msg_ids {
            if let Ok(tr) = self.translate_message(peer_id, mid, target_lang) {
                out.push((mid, tr));
            }
        }
        Ok(out)
    }

    /// Clears any cached translation on `msg_id` ("Show Original").
    fn clear_message_translation(&mut self, _peer_id: PeerId, _msg_id: MsgId) {}

    /// Returns `true` if the user hid/disabled whole-chat translations for `peer_id`.
    fn peer_translations_disabled(&self, _peer_id: PeerId) -> bool {
        false
    }

    /// Enables or disables whole-chat translation bar for `peer_id` (`messages.togglePeerTranslations`).
    fn toggle_peer_translations(
        &mut self,
        _peer_id: PeerId,
        _disabled: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Returns ISO 639-1 language codes excluded from automatic translation suggestions ("Do not translate").
    fn do_not_translate_languages(&self) -> Vec<String> {
        vec!["en".to_owned()]
    }

    /// Updates the user's "Do not translate" ISO 639-1 language codes.
    fn set_do_not_translate_languages(&mut self, _langs: Vec<String>) {}

    /// Returns the preferred target language code for chat/message translation (e.g. `"en"`).
    fn target_translation_language(&self) -> String {
        "en".to_owned()
    }

    /// Sets the preferred target language code for chat/message translation.
    fn set_target_translation_language(&mut self, _lang: String) {}

    /// Returns the available reactions catalog (`messages.getAvailableReactions`).
    fn available_reactions(&self) -> Vec<AvailableReactionItem> {
        [
            ("👍", "Thumbs Up"),
            ("👎", "Thumbs Down"),
            ("❤️", "Red Heart"),
            ("🔥", "Fire"),
            ("🥰", "Smiling Face With Hearts"),
            ("👏", "Clapping Hands"),
            ("😁", "Beaming Face"),
            ("🤔", "Thinking Face"),
            ("🤯", "Exploding Head"),
            ("😱", "Face Screaming in Fear"),
            ("🎉", "Party Popper"),
            ("🤩", "Star-Struck"),
            ("😢", "Crying Face"),
            ("😂", "Face With Tears of Joy"),
            ("😮", "Face With Open Mouth"),
        ]
        .into_iter()
        .map(|(emoji, title)| AvailableReactionItem {
            emoji: emoji.to_owned(),
            title: title.to_owned(),
            is_premium: false,
            inactive: false,
        })
        .collect()
    }

    /// Returns the user's top reaction emojis from `messages.getTopReactions` (cached with 64-bit
    /// hash, matching tdesktop's `Data::Reactions::list(Type::Top)`).
    fn top_reactions(&self) -> Vec<String> {
        self.available_reactions()
            .into_iter()
            .filter(|r| !r.inactive && !r.is_premium)
            .map(|r| r.emoji)
            .collect()
    }

    /// Returns the ordered emoji list for the message context-menu reaction strip, following
    /// tdesktop's `Data::LookupPossibleReactions` (`data_message_reactions.cpp:180-351`):
    /// default/favorite reaction rotated to front, followed by `messages.getTopReactions`,
    /// followed by remaining active `messages.getAvailableReactions`, filtered by `peer_id`'s
    /// allowed reactions policy and account Premium status.
    fn reaction_strip_emojis(&self, _peer_id: Option<PeerId>) -> Vec<String> {
        let mut ordered = Vec::new();
        let fav = self.default_reaction();
        let top = self.top_reactions();
        let avail: Vec<String> = self
            .available_reactions()
            .into_iter()
            .filter(|r| !r.inactive && !r.is_premium)
            .map(|r| r.emoji)
            .collect();
        for emoji in top.into_iter().chain(avail) {
            if !emoji.is_empty() && !ordered.contains(&emoji) {
                ordered.push(emoji);
            }
        }
        if let Some(pos) = ordered.iter().position(|e| e == &fav)
            && pos > 0
        {
            let item = ordered.remove(pos);
            ordered.insert(0, item);
        }
        ordered
    }

    /// Returns the available message effects catalog (`messages.getAvailableEffects`).
    fn available_effects(&self) -> Vec<AvailableMessageEffect> {
        crate::history::sections_model::MESSAGE_EFFECTS
            .iter()
            .map(|e| AvailableMessageEffect {
                id: e.id,
                emoji: e.emoji.to_owned(),
                name: e.name.to_owned(),
                premium_required: false,
                static_icon_id: None,
                effect_sticker_id: 0,
                effect_animation_id: None,
                lottie_json: None,
                accent_rgb: e.accent_rgb,
            })
            .collect()
    }

    /// Returns a Lottie JSON payload for `effect_id` (downloaded from `effect_animation_id` /
    /// `effect_sticker_id` in live mode, or `None` to fall back to procedural Lottie burst).
    fn effect_lottie_json(&self, effect_id: u64) -> Option<String> {
        self.available_effects()
            .into_iter()
            .find(|e| e.id == effect_id)
            .and_then(|e| e.lottie_json)
    }

    /// Loads peers who reacted to `msg_id` (`messages.getMessageReactionsList`).
    fn message_reactions_list(
        &self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _reaction_filter: Option<&str>,
        _offset: Option<&str>,
        _limit: usize,
    ) -> Result<MessageReactionsPage, String> {
        Ok(MessageReactionsPage::default())
    }

    /// Returns the user's default quick-reaction emoji.
    fn default_reaction(&self) -> String {
        "👍".to_owned()
    }

    /// Sets the user's default quick-reaction emoji (`messages.setDefaultReaction`).
    fn set_default_reaction(&mut self, _emoji: &str) -> Result<(), String> {
        Ok(())
    }

    /// Returns Saved Messages reaction tags (`messages.getSavedReactionTags`).
    fn saved_reaction_tags(&self, _peer_id: Option<PeerId>) -> Vec<SavedReactionTagItem> {
        Vec::new()
    }

    /// Updates or clears the custom label for a Saved Messages reaction tag
    /// (`messages.updateSavedReactionTag`).
    fn update_saved_reaction_tag(
        &mut self,
        _emoji: &str,
        _title: Option<&str>,
    ) -> Result<bool, String> {
        Ok(true)
    }

    /// Returns the peer ID of the user's Saved Messages chat, if known.
    fn history_saved_messages_peer_id(&self) -> Option<PeerId> {
        None
    }

    /// Loads voters for a non-anonymous poll (`messages.getPollVotes`).
    fn poll_votes(
        &self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _option_index: Option<usize>,
        _offset: Option<&str>,
        _limit: usize,
    ) -> Result<PollVotesPage, String> {
        Ok(PollVotesPage::default())
    }

    /// Retracts the current user's vote on a non-quiz open poll (`messages.sendVote` with empty options).
    fn retract_poll_vote(&mut self, peer_id: PeerId, msg_id: MsgId) -> Result<(), String> {
        self.vote_poll(peer_id, msg_id, &[])
    }

    /// Closes/stops an active poll (`messages.editMessage` with `Poll::closed = true`).
    fn stop_poll(&mut self, _peer_id: PeerId, _msg_id: MsgId) -> Result<(), String> {
        Err("Stopping polls is not supported by this backend.".to_owned())
    }

    /// Refreshes poll results from the server (`messages.getPollResults`, `api/api_polls.cpp:461-480`),
    /// returning the updated [`PollAttachment`] if present in session history.
    fn reload_poll_results(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
    ) -> Result<Option<PollAttachment>, String> {
        Ok(None)
    }

    /// Loads poll vote statistics (`stats.getPollStats`, automatically hydrating `StatsGraphAsync`
    /// via `stats.loadAsyncGraph`, `api/api_polls.cpp:482-541`).
    fn poll_stats(
        &self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _dark: bool,
    ) -> Result<tg_stats::StatisticalGraph, String> {
        Err("Poll statistics are not supported by this backend.".to_owned())
    }

    /// Reports `msg_ids` in `peer_id` with `reason` and optional `comment`.
    fn report_messages(
        &mut self,
        _peer_id: PeerId,
        _msg_ids: &[MsgId],
        _reason: MessageReportReason,
        _comment: &str,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Saves or clears a per-thread compose draft preserving `tg-text` UTF-16 entities.
    fn save_rich_draft(&mut self, thread: ThreadKey, draft: Option<RichThreadDraft>) {
        self.save_thread_draft(thread, draft.map(|d| d.to_legacy()));
    }

    /// Loads a saved per-thread compose draft preserving `tg-text` UTF-16 entities.
    fn load_rich_draft(&self, thread: ThreadKey) -> Option<RichThreadDraft> {
        self.load_thread_draft(thread)
            .map(RichThreadDraft::from_legacy)
    }

    /// Returns chat participants available for `@mention` autocomplete (`MentionName` insertion).
    fn chat_mention_candidates(&self, _peer_id: PeerId) -> Vec<MentionCandidate> {
        Vec::new()
    }

    /// Thumbnail the reply header of `msg_id` (the message that holds the reply) shows: the
    /// media of the replied-to message, story, poll answer or message from another chat
    /// (`Reply::paint`, `PhotoData::getReplyPreview` / `DocumentData::getReplyPreview`). Never
    /// blocks: a thumbnail that is not cached starts downloading and reports `Loading`.
    fn reply_preview_thumb(
        &self,
        _peer_id: PeerId,
        _msg_id: MsgId,
    ) -> crate::info::backend::SharedMediaThumb {
        crate::info::backend::SharedMediaThumb::Unavailable
    }

    /// Starts the download of `msg_id`'s media file that is not downloaded automatically or
    /// whose download was cancelled (`PhotoSaveClickHandler`, the first play of a voice
    /// message). Never blocks; the progress arrives through [`Self::inline_media_descriptor`].
    fn start_media_download(&self, _peer_id: PeerId, _msg_id: MsgId) {}

    /// A loader that serves `msg_id`'s media file part by part, so a voice message plays while
    /// it downloads (tdesktop streams voice messages, TGB-3000). `None` when the backend cannot
    /// stream it: the first play then downloads the file through
    /// [`Self::start_media_download`] and plays when it is there.
    fn open_media_stream(
        &self,
        _peer_id: PeerId,
        _msg_id: MsgId,
    ) -> Option<std::sync::Arc<dyn tg_video::RangeLoader>> {
        None
    }

    /// Cancels the running download of `msg_id`'s media file (`PhotoCancelClickHandler`). It
    /// stays not downloaded until [`Self::start_media_download`].
    fn cancel_media_download(&self, _peer_id: PeerId, _msg_id: MsgId) {}

    /// Returns optional inline media descriptor (video bytes/path, spoiler flag, photo loading
    /// state, WebM video sticker, or audio stream) for `msg_id`.
    fn inline_media_descriptor(
        &self,
        _peer_id: PeerId,
        _msg_id: MsgId,
    ) -> Option<InlineMediaDescriptor> {
        None
    }

    /// Returns optional read-only access to a [`crate::bots::backend::BotsBackend`] for bot UI
    /// metadata (bot info, commands, inline bot resolution, attach menu bots).
    fn as_bots_backend(&self) -> Option<&dyn crate::bots::backend::BotsBackend> {
        None
    }

    /// Returns optional mutable access to a [`crate::bots::backend::BotsBackend`] for bot
    /// interactions (callback queries, URL auth, inline queries, attach menu toggles).
    fn as_bots_backend_mut(&mut self) -> Option<&mut dyn crate::bots::backend::BotsBackend> {
        None
    }

    /// Returns optional read-only access to [`crate::chat_helpers::backend::ChatHelpersBackend`]
    /// for paid-reaction privacy and reaction moderation RPCs.
    fn as_chat_helpers_backend(
        &self,
    ) -> Option<&dyn crate::chat_helpers::backend::ChatHelpersBackend> {
        None
    }

    /// Returns optional mutable access to [`crate::chat_helpers::backend::ChatHelpersBackend`]
    /// for paid-reaction privacy and reaction moderation RPCs
    /// (`messages.{get,toggle}PaidReactionPrivacy`, `messages.reportReaction`,
    /// `messages.deleteParticipantReaction{,s}`).
    fn as_chat_helpers_backend_mut(
        &mut self,
    ) -> Option<&mut dyn crate::chat_helpers::backend::ChatHelpersBackend> {
        None
    }

    /// Returns active outgoing media uploads for `peer_id` (empty if none or unsupported).
    fn pending_media_uploads(&self, _peer_id: PeerId) -> Vec<MediaUploadStatus> {
        Vec::new()
    }

    /// Cancels an in-flight media upload by `upload_id`, returning `true` if found.
    fn cancel_media_upload(&mut self, _upload_id: u64) -> bool {
        false
    }

    /// Sends a Layer-233 rich document message (`messages.sendMessage` with `rich_message`).
    fn send_rich_message(
        &mut self,
        _peer_id: PeerId,
        _input: tg_tl::types::InputRichMessage,
        _page: tg_iv::rich_page::RichPage,
        _reply_to: Option<MsgId>,
        _silent: bool,
    ) -> Result<HistoryMessage, String> {
        Err("Rich messages are not supported by this backend.".to_owned())
    }

    /// Edits an existing rich document message (`messages.editMessage` with `rich_message`).
    fn edit_rich_message(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _input: tg_tl::types::InputRichMessage,
        _page: tg_iv::rich_page::RichPage,
    ) -> Result<HistoryMessage, String> {
        Err("Rich messages are not supported by this backend.".to_owned())
    }

    /// Returns optional read-only access to [`Layer233ExtrasBackend`] for checklists, sponsored
    /// messages, fact checks, and channel direct messages (monoforum / suggested posts).
    fn as_layer233_extras(&self) -> Option<&dyn Layer233ExtrasBackend> {
        None
    }

    /// Returns optional mutable access to [`Layer233ExtrasBackend`].
    fn as_layer233_extras_mut(&mut self) -> Option<&mut dyn Layer233ExtrasBackend> {
        None
    }

    /// Returns live speech-to-text transcription state for `(peer_id, msg_id)`, if any.
    fn transcribe_audio_state(
        &self,
        _peer_id: PeerId,
        _msg_id: MsgId,
    ) -> Option<TranscribeUiEntry> {
        None
    }

    /// Returns Premium / free-trial transcription limits for `peer_id`.
    fn transcribe_limits(&self, _peer_id: Option<PeerId>) -> TranscribeLimitsView {
        TranscribeLimitsView::default()
    }

    /// Toggles or requests speech-to-text transcription for a voice or round-video message.
    fn toggle_transcribe_audio(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _duration_secs: u32,
        _is_round_video: bool,
    ) -> TranscribeToggleOutcome {
        TranscribeToggleOutcome::Failed(
            "Transcription is not available for this backend.".to_owned(),
        )
    }

    /// Rates a completed transcription via `messages.rateTranscribedAudio`.
    fn rate_transcribed_audio(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _good: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Returns the pending join-requests summary for the in-chat top bar (`ui/chat/requests_bar.cpp`).
    fn join_requests_bar(
        &self,
        _peer_id: PeerId,
    ) -> Option<crate::info::backend::JoinRequestsSummary> {
        None
    }

    /// Returns pending join request entries for `peer_id` filtered by `query`.
    fn join_requests_entries(
        &self,
        _peer_id: PeerId,
        _query: &str,
    ) -> Vec<crate::info::backend::InviteImporterInfo> {
        Vec::new()
    }

    /// Approves (`approved = true`) or dismisses (`approved = false`) a single join request.
    fn decide_join_request(
        &mut self,
        _peer_id: PeerId,
        _user_id: PeerId,
        _approved: bool,
    ) -> Result<(), String> {
        Err("Join request actions are not supported by this backend.".to_owned())
    }

    /// Approves or dismisses all pending join requests for `peer_id`, returning count resolved.
    fn decide_all_join_requests(
        &mut self,
        _peer_id: PeerId,
        _approved: bool,
    ) -> Result<usize, String> {
        Err("Bulk join request actions are not supported by this backend.".to_owned())
    }

    /// Invokes a bot widget query (`messages.getBotWidgetQueryAnswer`) for a sandboxed HTML widget.
    fn widget_query(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _data: &[u8],
        _with_wallet: bool,
    ) -> Result<tg_tgw::WidgetRequestOutcome, String> {
        Err("Widget queries are not supported by this backend.".to_owned())
    }

    /// Polls shared public data (`messages.getWidgetPublicData`) for a widget message.
    fn widget_poll_public_data(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _hash: i64,
    ) -> Result<tg_tgw::PublicDataPollResult, String> {
        Err("Widget public data polling is not supported by this backend.".to_owned())
    }

    /// Edits per-message widget data (`messages.editMessageWidgetData`).
    fn widget_edit_data(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _public_data: Option<&str>,
        _chat_data: Option<&str>,
    ) -> Result<(), String> {
        Err("Editing widget data is not supported by this backend.".to_owned())
    }
    /// Returns the connected Telegram Business chatbot bar state for `peer_id`
    /// (`history_view_contact_status.cpp` `BusinessBotStatus`), if a bot manages this chat.
    fn business_bot_bar(&self, _peer_id: PeerId) -> Option<BusinessBotBarState> {
        None
    }

    /// Pauses (`paused = true`) or resumes (`paused = false`) the connected Business bot
    /// in `peer_id` (`account.toggleConnectedBotPaused`).
    fn toggle_business_bot_paused(
        &mut self,
        _peer_id: PeerId,
        _paused: bool,
    ) -> Result<(), String> {
        Err("Business bot pause/resume is not supported by this backend.".to_owned())
    }

    /// Disconnects the Business bot from `peer_id` (`account.disablePeerConnectedBot`).
    fn disable_business_bot(&mut self, _peer_id: PeerId) -> Result<(), String> {
        Err("Disconnecting a Business bot is not supported by this backend.".to_owned())
    }

    // -----------------------------------------------------------------------
    // WP-28: Layer-233 Poll Extensions, Self-Destruct Media, Compose AI, Sticker Creator
    // -----------------------------------------------------------------------

    /// Creates and sends a poll with Layer-233 extensions (open answers, option media/links).
    fn send_poll_with_extras(
        &mut self,
        peer_id: PeerId,
        thread_key: Option<ThreadKey>,
        request: CreatePollRequest,
        _extras: PollMessageExtras,
    ) -> Result<HistoryMessage, String> {
        self.send_poll(peer_id, thread_key, request)
    }

    /// Appends a new option to an open-answers poll (`messages.addPollAnswer`).
    fn add_poll_option(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _request: AddPollOptionRequest,
    ) -> Result<HistoryMessage, String> {
        Err("Adding poll options is not supported by this backend.".to_owned())
    }

    /// Removes a previously added poll option (`messages.deletePollAnswer`).
    fn delete_poll_option(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _option_bytes: &[u8],
    ) -> Result<(), String> {
        Err("Deleting poll options is not supported by this backend.".to_owned())
    }

    /// Marks a view-once / self-destructing media message as viewed (`messages.readMessageContents`).
    fn mark_ttl_media_viewed(&mut self, _peer_id: PeerId, _msg_id: MsgId) -> Result<(), String> {
        Ok(())
    }

    /// Optional mutable access to [`crate::core_types::dto::compose_ai::ComposeAiBackend`] (TranslateBox + Compose with AI).
    fn as_compose_ai_backend(
        &mut self,
    ) -> Option<&mut dyn crate::core_types::dto::compose_ai::ComposeAiBackend> {
        None
    }

    /// Optional mutable access to [`crate::core_types::dto::sticker_creator::StickerCreatorBackend`].
    fn as_sticker_creator_backend(
        &mut self,
    ) -> Option<&mut dyn crate::core_types::dto::sticker_creator::StickerCreatorBackend> {
        None
    }

    /// Optional mutable access to [`crate::core_types::dto::self_destruct::SelfDestructBackend`].
    fn as_self_destruct_backend(
        &mut self,
    ) -> Option<&mut dyn crate::core_types::dto::self_destruct::SelfDestructBackend> {
        None
    }

    // -----------------------------------------------------------------------
    // WP-38: Streamed Bot Drafts, Compose Stash, Read Metrics, Emoji Interactions,
    //        1:1 No-Forwards Request, and Media Toggling with File Reference Refresh
    // -----------------------------------------------------------------------

    /// Returns active Layer-233 streamed bot draft messages for `(peer_id, root_id)`.
    fn streamed_draft_messages(
        &self,
        _peer_id: PeerId,
        _root_id: Option<MsgId>,
    ) -> Vec<HistoryMessage> {
        Vec::new()
    }

    /// Returns `true` if `peer_id` currently has a stoppable streamed bot draft for `root_id`.
    fn has_stoppable_streamed_draft(&self, _peer_id: PeerId, _root_id: Option<MsgId>) -> bool {
        false
    }

    /// Requests stopping the newest stoppable streamed bot draft (`sendMessageStopDraftAction`).
    fn stop_streamed_draft(
        &mut self,
        _peer_id: PeerId,
        _root_id: Option<MsgId>,
    ) -> Result<bool, String> {
        Ok(false)
    }

    /// Returns the stashed compose state for `key` (`Data::ComposeStash`), if any.
    fn compose_stash(
        &self,
        _key: crate::core_types::dto::compose_stash::ComposeStashKey,
    ) -> Option<crate::core_types::dto::compose_stash::ComposeStash> {
        None
    }

    /// Saves or clears the stashed compose state for `key` (`Data::ComposeStash`).
    fn set_compose_stash(
        &mut self,
        _key: crate::core_types::dto::compose_stash::ComposeStashKey,
        _stash: Option<crate::core_types::dto::compose_stash::ComposeStash>,
    ) {
    }

    /// Enqueues finalized viewport read metrics (`messages.reportReadMetrics`).
    fn report_read_metrics(
        &mut self,
        _peer_id: PeerId,
        _metrics: Vec<crate::core_types::read_metrics::FinalizedReadMetric>,
    ) {
    }

    /// Flushes due (or all when `force == true`) queued read metrics, returning count sent.
    fn flush_read_metrics(&mut self, _force: bool) -> usize {
        0
    }

    /// Sends an outgoing interactive emoji tap bunch (`sendMessageEmojiInteraction`).
    fn send_emoji_interaction(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _emoticon: &str,
        _bunch: &crate::core_types::dto::emoji_interactions::EmojiInteractionsBunch,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Sends an interactive emoji seen acknowledgement (`sendMessageEmojiInteractionSeen`).
    fn send_emoji_interaction_seen(
        &mut self,
        _peer_id: PeerId,
        _emoticon: &str,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Drains incoming remote interactive emoji bunches received for `peer_id`.
    fn drain_incoming_emoji_interactions(
        &mut self,
        _peer_id: PeerId,
    ) -> Vec<crate::core_types::dto::emoji_interactions::IncomingEmojiInteraction> {
        Vec::new()
    }

    /// Drains incoming `"seen"` emoticon notifications received for `peer_id`.
    fn drain_emoji_interaction_seen(&mut self, _peer_id: PeerId) -> Vec<String> {
        Vec::new()
    }

    /// Resolves a server Lottie interaction animation `(document_id, lottie_json)` from
    /// `EmojiStickersPack` (`InputStickerSetAnimatedEmojiAnimations`) for `(emoticon, variant_index)`.
    ///
    /// Returns `None` while the server pack or `.tgs` asset is not yet loaded so callers fall back
    /// to the procedural burst overlay.
    fn resolve_emoji_interaction_lottie(
        &mut self,
        _emoticon: &str,
        _variant_index: usize,
    ) -> Option<(u64, String)> {
        None
    }

    /// Returns the number of server interaction animation variants available for `emoticon`
    /// in `EmojiStickersPack`, if the server pack has been loaded.
    fn emoji_interaction_variant_count(&self, _emoticon: &str) -> Option<usize> {
        None
    }

    /// Whether messages of `peer_id` may be forwarded and copied (`PeerData::allowsForwarding`):
    /// `false` for a chat with "Restrict saving content".
    fn peer_allows_forwarding(&self, _peer_id: PeerId) -> bool {
        true
    }

    /// Returns `true` if 1:1 content-sharing protection (`noforwards`) is enabled by us for `peer_id`.
    fn peer_no_forwards_enabled(&self, _peer_id: PeerId) -> bool {
        false
    }

    /// The rights and flags the chat's "three dots" menu reads (`Window::Filler`). The default
    /// guesses from [`HistoryBackend::peer_info`].
    fn peer_menu_caps(&self, peer_id: PeerId) -> Option<crate::history::peer_menu::PeerMenuCaps> {
        self.peer_info(peer_id)
            .map(|info| crate::history::peer_menu::PeerMenuCaps::from_peer_info(&info))
    }

    /// Returns `true` if a `NoForwardsRequest` service message at `(peer_id, msg_id)` has already
    /// been acted on.
    fn is_no_forwards_request_acted(&self, _peer_id: PeerId, _msg_id: MsgId) -> bool {
        false
    }

    /// Toggles 1:1 sharing protection or responds to a `messageActionNoForwardsRequest`
    /// (`messages.toggleNoForwards`).
    fn toggle_peer_no_forwards(
        &mut self,
        _peer_id: PeerId,
        _enabled: bool,
        _request_msg_id: Option<MsgId>,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Saves or unsaves a GIF message with `FILE_REFERENCE_*` refresh & retry (`messages.saveGif`).
    fn toggle_saved_gif_from_message(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _saved: bool,
    ) -> Result<bool, String> {
        Ok(true)
    }

    /// Favorites or unfavorites a sticker message with `FILE_REFERENCE_*` refresh & retry (`messages.faveSticker`).
    fn toggle_faved_sticker_from_message(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _faved: bool,
    ) -> Result<bool, String> {
        Ok(true)
    }

    /// Saves or removes a recent sticker from a message (`messages.saveRecentSticker`).
    fn toggle_recent_sticker_from_message(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _saved: bool,
    ) -> Result<bool, String> {
        Ok(true)
    }

    /// Saves or removes a custom ringtone from an audio/voice message (`account.saveRingtone`).
    fn toggle_saved_ringtone_from_message(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _saved: bool,
    ) -> Result<bool, String> {
        Ok(true)
    }

    /// Returns the cached AI summary state for `(peer_id, msg_id)`, if any
    /// (`Api::Transcribes::summary`).
    fn message_summary(&self, _peer_id: PeerId, _msg_id: MsgId) -> Option<MessageSummaryEntry> {
        None
    }

    /// Toggles AI message summary visibility, invoking `messages.summarizeText` if not yet cached
    /// (`Api::Transcribes::toggleSummary`).
    fn toggle_message_summary(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _to_lang: Option<&str>,
    ) -> Result<MessageSummaryEntry, String> {
        Err("AI message summary is not available for this backend.".to_owned())
    }

    /// Applies a suggested birthday from a `SuggestBirthday` card (`account.updateBirthday`).
    fn apply_suggested_birthday(
        &mut self,
        _day: u8,
        _month: u8,
        _year: Option<u16>,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Applies a suggested profile photo from a `SuggestProfilePhoto` card
    /// (`photos.updateProfilePhoto`).
    fn apply_suggested_profile_photo(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _photo_id: i64,
        _access_hash: i64,
        _file_reference: &[u8],
        _is_video: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Sends one or more saved profile music tracks (`SavedMusicTrack`) as real audio media via
    /// `messages.sendMedia` (single track) or `messages.sendMultiMedia` (multiple tracks) using
    /// `InputMediaDocument` (`document_id`, `access_hash`, `file_reference`).
    fn send_saved_music(
        &mut self,
        _peer_id: PeerId,
        _tracks: &[crate::info::backend::SavedMusicTrack],
    ) -> Result<Vec<HistoryMessage>, String> {
        Err("Sending saved music is unavailable.".to_owned())
    }

    /// Rewrites, translates, or proofreads a Layer-233 rich message via
    /// `messages.composeRichMessageWithAI`.
    fn compose_rich_message_with_ai(
        &mut self,
        request: crate::core_types::dto::compose_ai::ComposeRichAiRequest,
    ) -> Result<crate::core_types::dto::compose_ai::ComposeRichAiResult, String> {
        if let Some(ai) = self.as_compose_ai_backend() {
            ai.compose_rich_message_with_ai(request)
        } else {
            Err("Rich message AI compose is unavailable.".to_owned())
        }
    }

    /// Translates chat rich messages or inline rich pages via `messages.translateRichMessage`.
    fn translate_rich_message(
        &mut self,
        request: crate::core_types::dto::compose_ai::TranslateRichMessageRequest,
    ) -> Result<crate::core_types::dto::compose_ai::TranslateRichMessageResult, String> {
        if let Some(ai) = self.as_compose_ai_backend() {
            ai.translate_rich_message(request)
        } else {
            Err("Rich message translation is unavailable.".to_owned())
        }
    }

    // -----------------------------------------------------------------------
    // WP-53: Chat Engagement & Read-Receipt Protocol
    // -----------------------------------------------------------------------

    /// Enqueues visible channel/forwarded post IDs for debounced
    /// `messages.getMessagesViews(increment = true)` (`Api::ViewsManager::scheduleIncrement`).
    fn schedule_message_views_increment(&mut self, _peer_id: PeerId, _msg_ids: &[MsgId]) {}

    /// Flushes due (or all when `force == true`) batched view increments for `peer_id` (or all
    /// peers when `None`), returning updated view/forward/reply counts.
    fn flush_message_views(
        &mut self,
        _peer_id: Option<PeerId>,
        _force: bool,
    ) -> Result<Vec<MessageViewsInfo>, String> {
        Ok(Vec::new())
    }

    /// Clears the per-peer already-incremented views deduplication set
    /// (`Api::ViewsManager::removeIncremented`).
    fn remove_incremented_views(&mut self, _peer_id: PeerId) {}

    /// Fetches message view, forward, and reply counts directly via `messages.getMessagesViews`.
    fn get_messages_views(
        &mut self,
        _peer_id: PeerId,
        _msg_ids: &[MsgId],
        _increment: bool,
    ) -> Result<Vec<MessageViewsInfo>, String> {
        Ok(Vec::new())
    }

    /// Queues or immediately polls extended/paid media metadata for `msg_ids` via
    /// `messages.getExtendedMedia` (`Api::ViewsManager::pollExtendedMedia`).
    fn poll_extended_media(
        &mut self,
        _peer_id: PeerId,
        _msg_ids: &[MsgId],
        _force: bool,
    ) -> Result<Vec<ExtendedMediaRefreshEntry>, String> {
        Ok(Vec::new())
    }

    /// Flushes due (or all when `force == true`) queued extended-media polls across all peers.
    fn flush_extended_media_polls(
        &mut self,
        _force: bool,
    ) -> Result<Vec<ExtendedMediaRefreshEntry>, String> {
        Ok(Vec::new())
    }

    /// Polls visible group/channel messages that already carry reactions via
    /// `messages.getMessagesReactions`, honoring tdesktop's 20-second `kPollEach` interval
    /// (`data/data_message_reactions.cpp:1512-1535, 1696-1729`).
    ///
    /// Returns the subset of `msg_ids` that were actually polled on this call.
    fn poll_messages_reactions(
        &self,
        _peer_id: PeerId,
        _msg_ids: &[MsgId],
    ) -> Result<Vec<MsgId>, String> {
        Ok(Vec::new())
    }

    /// `HistoryInner::paintEvent` -> `Data::Reactions::poll` for every painted message: refreshes
    /// the reactions of painted group/channel messages that carry a reactions object, at most
    /// once per `kPollEach` (20 s) each, without waiting (TGB-3013). Called every frame.
    fn poll_painted_reactions(&self, _peer_id: PeerId, _msg_ids: &[MsgId]) {}

    /// Loads the next page of unread mentions, reactions, or poll votes for `scope` via
    /// `messages.getUnread{Mentions,Reactions,PollVotes}` (`Api::UnreadThings`).
    fn load_unread_things_page(
        &mut self,
        _scope: UnreadThingsScope,
        kind: UnreadThingsKind,
    ) -> Result<UnreadThingsSlice, String> {
        Ok(UnreadThingsSlice::empty(kind))
    }

    /// Returns the currently tracked local unread mentions/reactions/poll-votes counts for `scope`.
    fn unread_things_counts(&self, _scope: UnreadThingsScope) -> UnreadThingsCounts {
        UnreadThingsCounts::default()
    }

    /// Marks all unread mentions in `peer_id` (or forum topic `top_msg_id`) as read via
    /// `messages.readMentions`, draining `AffectedHistory` pagination and zeroing local counts.
    fn read_unread_mentions(
        &mut self,
        _peer_id: PeerId,
        _top_msg_id: Option<MsgId>,
    ) -> Result<UnreadThingsReadResult, String> {
        Ok(UnreadThingsReadResult::default())
    }

    /// Marks all unread reactions in `peer_id` (optionally scoped by `top_msg_id` or
    /// `saved_peer_id`) as read via `messages.readReactions`, draining `AffectedHistory` and
    /// zeroing local counts.
    fn read_unread_reactions(
        &mut self,
        _peer_id: PeerId,
        _top_msg_id: Option<MsgId>,
        _saved_peer_id: Option<PeerId>,
    ) -> Result<UnreadThingsReadResult, String> {
        Ok(UnreadThingsReadResult::default())
    }

    /// Marks unread reactions on currently visible `msg_ids` in `peer_id` as read like tdesktop
    /// (`history_inner_widget.cpp:1572`, `apiwrap.cpp:1478-1508`: `messages.readMessageContents`
    /// for DMs/basic groups or `channels.readMessageContents` for supergroups/channels) and
    /// clears local unread-reaction state so the floating "♥ N" button and chat-row badge clear.
    ///
    /// Only regular server messages that currently have unread reactions are sent; returns the
    /// subset of `msg_ids` that were marked read (empty when none had unread reactions).
    fn mark_visible_reactions_read(
        &mut self,
        _peer_id: PeerId,
        _msg_ids: &[MsgId],
    ) -> Result<Vec<MsgId>, String> {
        Ok(Vec::new())
    }

    /// Marks all unread poll votes in `peer_id` (or forum topic `top_msg_id`) as read via
    /// `messages.readPollVotes`, draining `AffectedHistory` and zeroing local counts.
    fn read_unread_poll_votes(
        &mut self,
        _peer_id: PeerId,
        _top_msg_id: Option<MsgId>,
    ) -> Result<UnreadThingsReadResult, String> {
        Ok(UnreadThingsReadResult::default())
    }

    /// Loads group read-receipt participants via `messages.getMessageReadParticipants`
    /// (`Api::WhoReadIds` in `api_who_reacted.cpp:319-342`).
    fn get_message_read_participants(
        &self,
        _peer_id: PeerId,
        _msg_id: MsgId,
    ) -> Result<Vec<MessageReadParticipant>, String> {
        Ok(Vec::new())
    }

    /// Fetches the 1:1 DM outgoing message read timestamp via `messages.getOutboxReadDate`,
    /// mapping `YOUR_PRIVACY_RESTRICTED`, `USER_PRIVACY_RESTRICTED`, and `MESSAGE_TOO_OLD`
    /// (`api_who_reacted.cpp:288-317`).
    fn get_outbox_read_date(
        &self,
        _peer_id: PeerId,
        _msg_id: MsgId,
    ) -> Result<OutboxReadDateStatus, String> {
        Ok(OutboxReadDateStatus::Unavailable)
    }

    /// Syncs inbox read state for a channel comments thread or forum topic via
    /// `messages.readDiscussion` (`RepliesList::sendReadTillRequest` in `data_replies_list.cpp:967`).
    fn read_discussion(
        &mut self,
        _peer_id: PeerId,
        _top_msg_id: MsgId,
        _read_max_id: MsgId,
    ) -> Result<bool, String> {
        Ok(true)
    }

    /// Reports received message delivery to Telegram via `messages.reportMessagesDelivery`,
    /// chunked into batches of 50 (`Histories::reportPendingDeliveries` in `data_histories.cpp:679`).
    fn report_messages_delivery(
        &mut self,
        _peer_id: PeerId,
        _msg_ids: &[MsgId],
        _push: bool,
    ) -> Result<usize, String> {
        Ok(0)
    }

    /// Deletes an ephemeral message via `ephemeral.deleteMessage`
    /// (`EphemeralMessages::deleteMessage` in `ephemeral_messages.cpp:1059-1075`).
    fn ephemeral_delete_message(
        &mut self,
        _peer_id: PeerId,
        _receiver_user_id: PeerId,
        _ephemeral_id: i32,
    ) -> Result<bool, String> {
        Err("Deleting ephemeral messages is not supported by this backend.".to_owned())
    }

    /// Reports an ephemeral message via `ephemeral.reportMessage`
    /// (`api/api_report.cpp:162-184`).
    fn ephemeral_report_message(
        &mut self,
        _peer_id: PeerId,
        _ephemeral_id: i32,
        _option: &[u8],
        _comment: &str,
    ) -> Result<EphemeralReportOutcome, String> {
        Err("Reporting ephemeral messages is not supported by this backend.".to_owned())
    }

    /// Exports a direct message link via `channels.exportMessageLink`
    /// (`ApiWrap::exportDirectMessageLink` in `apiwrap.cpp:878-889`).
    fn export_message_link(
        &self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _grouped: bool,
        _thread: bool,
    ) -> Result<ExportedMessageLinkInfo, String> {
        Err("Exporting message links is not supported by this backend.".to_owned())
    }

    /// Resolves the author of an anonymous/channel message via `channels.getMessageAuthor`
    /// (`LookupMessageAuthor` in `history_view_context_menu.cpp:1556-1616`).
    fn get_message_author(
        &self,
        _peer_id: PeerId,
        _msg_id: MsgId,
    ) -> Result<MessageAuthorInfo, String> {
        Err("Looking up message author is not supported by this backend.".to_owned())
    }

    /// Upgrades a basic group chat to a supergroup via `messages.migrateChat`
    /// (`ApiWrap::migrateChat` in `apiwrap.cpp:1400-1476`).
    fn migrate_chat(&mut self, _chat_peer_id: PeerId) -> Result<ChatMigrationResult, String> {
        Err("Migrating chats is not supported by this backend.".to_owned())
    }

    /// Queries who would become the new creator if the current owner leaves `peer_id` via
    /// `messages.getFutureChatCreatorAfterLeave` (`window_peer_menu.cpp:4367-4389`).
    fn future_chat_creator_after_leave(
        &self,
        _peer_id: PeerId,
    ) -> Result<Option<FutureChatCreatorInfo>, String> {
        Ok(None)
    }

    // -----------------------------------------------------------------------
    // WP-66: Contact Status Bar, Spam Report & Block From Replies
    // -----------------------------------------------------------------------

    /// Shares phone number / accepts a non-contact user via `contacts.acceptContact`
    /// (`history/view/history_view_contact_status.cpp:916-925`) and clears `PeerBarSettings`.
    fn accept_contact(&mut self, _peer_id: PeerId) -> Result<(), String> {
        Err("Accepting contacts is not supported by this backend.".to_owned())
    }

    /// Reports a peer for spam via `messages.reportSpam`
    /// (`history/view/history_view_contact_status.cpp:967-969`) and clears `PeerBarSettings`.
    fn report_spam(&mut self, _peer_id: PeerId) -> Result<bool, String> {
        Err("Reporting spam is not supported by this backend.".to_owned())
    }

    /// Dismisses the peer settings / contact status bar via `messages.hidePeerSettingsBar`
    /// (`history/view/history_view_contact_status.cpp:1014-1017`) and clears `PeerBarSettings`.
    fn hide_peer_settings_bar(&mut self, _peer_id: PeerId) -> Result<bool, String> {
        Err("Hiding peer settings bar is not supported by this backend.".to_owned())
    }

    /// Blocks a sender from the Replies chat via `contacts.blockFromReplies`
    /// (`window/window_peer_menu.cpp:2845-2854`).
    fn block_from_replies(
        &mut self,
        _request: crate::core_types::dto::contacts_extras::BlockFromRepliesRequest,
    ) -> Result<(), String> {
        Err("Blocking from replies is not supported by this backend.".to_owned())
    }
}

/// Paginated slice returned by `messages.getUnread{Mentions,Reactions,PollVotes}`
/// (`HistoryUnreadThings::Proxy::addSlice`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadThingsSlice {
    /// Category of unread items (`Mentions`, `Reactions`, or `PollVotes`).
    pub kind: UnreadThingsKind,
    /// Server-reported total unread count for this scope (clamped to `>= loaded_ids.len()`).
    pub full_count: i32,
    /// All currently loaded unread message IDs for this scope (ascending).
    pub loaded_ids: Vec<MsgId>,
    /// Newly decoded messages from this page.
    pub messages: Vec<HistoryMessage>,
    /// True if more unread items remain on the server (`loaded_ids.len() < full_count`).
    pub has_more: bool,
}

impl UnreadThingsSlice {
    /// Constructs an empty slice for `kind`.
    #[must_use]
    pub const fn empty(kind: UnreadThingsKind) -> Self {
        Self {
            kind,
            full_count: 0,
            loaded_ids: Vec::new(),
            messages: Vec::new(),
            has_more: false,
        }
    }
}

/// Connected Telegram Business chatbot status shown above a private chat
/// (`BusinessBotStatus` in `history_view_contact_status.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusinessBotBarState {
    /// Connected bot's peer identifier.
    pub bot_id: PeerId,
    /// Connected bot's display name.
    pub bot_name: String,
    /// Connected bot's username without leading `@`, if known.
    pub bot_username: Option<String>,
    /// Optional management URL (`PeerSettings::business_bot_manage_url`).
    pub manage_url: Option<String>,
    /// Whether the bot is permitted to reply (`PeerSettings::business_bot_can_reply`).
    pub can_reply: bool,
    /// Whether the bot is currently paused in this chat (`PeerSettings::business_bot_paused`).
    pub paused: bool,
}

/// Backend contract for Layer-233 history extras: Checklists (`MessageMedia::ToDo`),
/// Sponsored Messages, Fact Checks, and Channel Direct Messages (Monoforum / Suggested Posts).
pub trait Layer233ExtrasBackend {
    // --- Checklists / To-Do lists ---

    /// Creates and sends a new checklist (`messages.sendMedia` with `inputMediaTodo`).
    fn create_todo_list(
        &mut self,
        peer_id: PeerId,
        request: CreateTodoListRequest,
    ) -> Result<HistoryMessage, String>;

    /// Edits an existing checklist message (`messages.editMessage` with `inputMediaTodo`).
    fn edit_todo_list(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        request: CreateTodoListRequest,
    ) -> Result<HistoryMessage, String>;

    /// Toggles completion of one or more tasks (`messages.toggleTodoCompleted`).
    fn toggle_todo_completed(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        completed: &[i32],
        incompleted: &[i32],
    ) -> Result<Option<HistoryMessage>, String>;

    /// Appends new tasks to an existing checklist (`messages.appendTodoList`).
    fn append_todo_list(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        items: Vec<TodoItemInput>,
    ) -> Result<Option<HistoryMessage>, String>;

    // --- Sponsored messages ---

    /// Returns whether ads are currently hidden via the Telegram Premium "hide ads" setting.
    fn is_sponsored_hidden(&self) -> bool;

    /// Updates the Telegram Premium "hide ads" setting (`account.toggleSponsoredMessages`).
    fn set_sponsored_hidden(&mut self, hide_ads: bool) -> Result<(), String>;

    /// Fetches or returns cached sponsored messages for a channel/bot (`messages.getSponsoredMessages`).
    fn get_sponsored_messages(
        &mut self,
        peer_id: PeerId,
        msg_id: Option<MsgId>,
    ) -> Result<Vec<SponsoredMessageItem>, String>;

    /// Marks a sponsored message as viewed (`messages.viewSponsoredMessage`).
    fn view_sponsored_message(&mut self, random_id: &[u8]) -> Result<(), String>;

    /// Records a click on a sponsored message (`messages.clickSponsoredMessage`).
    fn click_sponsored_message(
        &mut self,
        random_id: &[u8],
        is_media: bool,
        is_fullscreen: bool,
    ) -> Result<(), String>;

    /// Reports a sponsored message or selects a report sub-option (`messages.reportSponsoredMessage`).
    fn report_sponsored_message(
        &mut self,
        random_id: &[u8],
        option: &[u8],
    ) -> Result<SponsoredReportOutcome, String>;

    // --- Fact checks ---

    /// Returns the cached fact-check for `(peer_id, msg_id)`, if present.
    fn cached_factcheck(&self, peer_id: PeerId, msg_id: MsgId) -> Option<FactCheckData>;

    /// Returns whether the current account may edit fact-checks (`appConfig.can_edit_factcheck`).
    fn can_edit_factcheck_config(&self) -> bool;

    /// Fetches a batch of fact-checks for `msg_ids` in `peer_id` (`messages.getFactCheck`).
    fn get_factchecks(
        &mut self,
        peer_id: PeerId,
        msg_ids: &[MsgId],
    ) -> Result<Vec<(MsgId, FactCheckData)>, String>;

    /// Edits or deletes (when `text.text.trim().is_empty()`) a message's fact-check
    /// (`messages.editFactCheck` / `messages.deleteFactCheck`).
    fn edit_factcheck(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        text: TextWithEntities,
    ) -> Result<Option<FactCheckData>, String>;

    // --- Suggested posts & channel direct messages (monoforum) ---

    /// Returns cached or freshly fetched channel direct-message sublists (`messages.getSavedDialogs`).
    fn monoforum_sublists(&mut self, monoforum_peer_id: PeerId) -> Vec<MonoforumSublist>;

    /// Loads messages belonging to one monoforum sublist (`messages.getSavedHistory`).
    fn monoforum_sublist_messages(
        &mut self,
        monoforum_peer_id: PeerId,
        sublist_peer_id: PeerId,
    ) -> Vec<HistoryMessage>;

    /// Marks a monoforum sublist read up to `max_id` (`messages.readSavedHistory`).
    fn mark_monoforum_sublist_read(
        &mut self,
        monoforum_peer_id: PeerId,
        sublist_peer_id: PeerId,
        max_id: MsgId,
    ) -> Result<(), String>;

    /// Sends a direct message or suggested post in a monoforum channel (`messages.sendMessage`
    /// with `inputReplyToMonoForum` and optional `suggested_post`).
    fn send_monoforum_message(
        &mut self,
        monoforum_peer_id: PeerId,
        sublist_peer_id: PeerId,
        text: TextWithEntities,
        suggest: Option<SuggestOptions>,
    ) -> Result<HistoryMessage, String>;

    /// Returns suggested-post metadata attached to `(peer_id, msg_id)`, if any.
    fn suggested_post_for(&self, peer_id: PeerId, msg_id: MsgId) -> Option<SuggestedPostData>;

    /// Approves or rejects a suggested post (`messages.toggleSuggestedPostApproval`).
    fn toggle_suggested_post_approval(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        decision: SuggestApprovalDecision,
    ) -> Result<(), String>;
}
