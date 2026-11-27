//! Virtualized scrollable message list: visible-range layout, sticky userpics,
//! scroll anchoring, preload triggers, jump-to-message highlight, multi-select,
//! drag-select text, and right-click context menu.
//!
//! Port origin: `Telegram/SourceFiles/history/history_inner_widget.cpp` and
//! `Telegram/SourceFiles/history/view/history_view_list_widget.cpp`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use crate::history::delete_collapse::{CollapseScroll, Removal};
use egui::{
    Color32, CornerRadius, FontId, Id, Key, Pos2, Rect, ScrollArea, Sense, Shape, Stroke, Ui, Vec2,
};
use tg_history::consts::{
    FontMetrics, HISTORY_PHOTO_LEFT, HISTORY_UNREAD_BAR_MARGIN, LINE_WIDTH, MSG_MARGIN,
    MSG_MAX_WIDTH, MSG_PADDING, MSG_PHOTO_SIZE, MSG_PHOTO_SKIP,
};
use tg_history::{
    AttachMetadata, BottomInfoInput, CivilDate, DocumentInput, DocumentKind, ElementChatMode,
    ElementHighlighter, FlatSelectionEndpoint, GifInput, GifKind, GroupedMediaInput, GroupedMode,
    HighlightTarget, HistoryList, HistoryListItem, ListSelectionEndpoint, LocalDateTime,
    MediaInput, MessageInput, MessageLayout, MessagePosition, PhotoInput, PollAnswerInput,
    PollInput, ReplyInput, ServiceInput, ServiceLayout, Size, StickerInput, StickerKind,
    WebPageInput, count_service_date_rect, date_badge_height, layout_message,
    layout_service_message, unread_bar_height,
};
use tg_ui::stand_in::PopupMenuStyle;
use tg_ui::userpic::EmptyUserpic;
use tg_ui::widgets::buttons::IconKind;
use tg_ui::widgets::menu::{MenuItem, PopupMenuState};

use crate::bots::inline_keyboard::{InlineKeyboardActivation, InlineKeyboardState};
use crate::history::backend::{
    HistoryBackend, HistoryMedia, HistoryMessage, HistoryPeerInfo, HistorySlice, Loadable,
    MessageSummaryEntry, ServiceAction, to_hist_full, to_hist_peer,
};
use crate::history::bubble::{
    InlineKeyboardRenderCtx, MessagePaintEvent, ReactionUserpics, ServiceCardAction,
    format_compact_views, hist_rect_to_egui, link_preview_card_height, measure_reactions_size,
    paint_date_separator, paint_message_element_full, paint_service_message_interactive,
    paint_unread_bar, parse_paid_media_stars,
};
use crate::history::copy_restriction;
use crate::history::copy_selection::{self, SelectionCopy, UponSelected};
use crate::history::inline_media::HistoryMediaRuntime;
use crate::history::media::{
    MediaAssetCache, MediaInteraction, VoicePlaybackState, format_duration, format_file_size,
};
use crate::history::message_text_state::{MessageTextState, ends_with_quote, measure_message_text};
use crate::history::paint_cache::MessagePaintCache;
use crate::history::read_gate::{BackendReadGate, Gated};
use crate::history::sections_model::{CopiedRichText, ReplyQuote, slice_text_with_entities};
use crate::history::text_bridge::{
    entities_to_rich_text, estimate_text_metrics, message_text_style,
};
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{FullMsgId, MsgId, PeerId, PeerKind};

/// Cached computed layout for a single history item at a specific viewport width.
#[derive(Debug, Clone)]
pub enum CachedItemLayout {
    /// Regular chat message layout (`tg-history::MessageLayout`).
    Message(Box<MessageLayout>),
    /// Service message layout (`tg-history::ServiceLayout`).
    Service(Box<ServiceLayout>),
}

/// One entry of the per-message layout cache: the geometry and what painting keeps with it
/// (the prepared text). Dropping the entry (an edit, a new width) drops both.
#[derive(Debug)]
struct CachedItem {
    layout: CachedItemLayout,
    paint: MessagePaintCache,
}

impl CachedItem {
    fn new(layout: CachedItemLayout) -> Self {
        Self {
            layout,
            paint: MessagePaintCache::default(),
        }
    }
}

/// Loaded messages whose registered media is still arriving, as found by the last scan of the
/// list. The list is scanned again when it or a media descriptor may have changed and at least
/// once per poll interval, instead of twice on every frame.
#[derive(Debug, Default)]
struct MediaWatch {
    /// Photos whose descriptor says `Loading` or `Pending`.
    loading: Vec<MsgId>,
    /// One of `loading` is still being read from the disk cache (`Pending`): polled on every
    /// frame until the read lands.
    cache_reads: bool,
    /// Round videos registered before their video arrived.
    pending_rounds: Vec<MsgId>,
    /// A descriptor or a message changed since the scan.
    dirty: bool,
    /// `(len, first id, last id)` of the list at the scan, `None` before the first scan.
    scanned_list: Option<(usize, Option<MsgId>, Option<MsgId>)>,
    scanned_ms: i64,
}

/// How long one frame may spend asking for the media of messages away from the viewport
/// (`HistoryInnerWidget::request_backlog_media`); at least one message is asked per frame.
const MEDIA_BACKLOG_BUDGET: Duration = Duration::from_millis(2);
/// Most messages away from the viewport asked for on one frame.
const MEDIA_BACKLOG_MAX_PER_FRAME: usize = 16;

/// How many messages keep their prepared text (`MessagePaintCache`) after they were painted.
/// A galley holds every glyph of the text, so keeping one for each message ever scrolled
/// through would grow without bound; a message that left this window is prepared again when
/// it is painted next, which costs what one frame cost before.
const PAINT_CACHES_KEPT: usize = 128;

/// Whether the audio playlists read this message's media when a slice is applied
/// (`HistoryMediaRuntime::sync_peer_audio_tracks`): its descriptor has to be there by then.
fn media_feeds_audio_playlist(msg: &HistoryMessage) -> bool {
    matches!(
        msg.media,
        Some(HistoryMedia::Voice(_) | HistoryMedia::RoundVideo(_) | HistoryMedia::Document(_))
    )
}

/// Events bubbled up from [`HistoryInnerWidget`] to [`crate::history::HistoryWidget`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InnerEvent {
    /// Start replying to `msg_id`.
    Reply(MsgId),
    /// Open Draw-to-Reply photo editor on `msg_id` (`history_view_draw_to_reply.cpp`).
    DrawToReply(MsgId),
    /// Start editing `msg_id`.
    Edit(MsgId),
    /// Forward one or more messages.
    Forward(Vec<MsgId>),
    /// Pin or unpin `msg_id`.
    TogglePin {
        /// Target message id.
        msg_id: MsgId,
        /// `true` to unpin, `false` to pin.
        unpin: bool,
    },
    /// Delete one or more messages.
    Delete(Vec<MsgId>),
    /// Open `ModerateMessagesBox` for one or more incoming group/channel messages.
    Moderate(Vec<MsgId>),
    /// Toggle emoji reaction on `msg_id`.
    ToggleReaction {
        /// Target message id.
        msg_id: MsgId,
        /// Emoji string.
        emoji: String,
    },
    /// Submit poll vote.
    VotePoll {
        /// Poll message id.
        msg_id: MsgId,
        /// Selected option index.
        option_index: usize,
    },
    /// User copied text or link (for toast feedback).
    Copied(String),
    /// User clicked a link / mention.
    OpenLink(String),
    /// User clicked a hashtag, cashtag, bot command or name mention in a message's text
    /// (the element click handlers of `core/click_handler_types.cpp`).
    EntityLink {
        /// Message whose text holds the entity (`ClickHandlerContext::itemId`).
        msg_id: MsgId,
        /// The clicked entity.
        link: tg_ui::entity_link::EntityLink,
    },
    /// User clicked a hidden `messageEntityTextUrl` link requiring scheme check and confirmation.
    OpenHiddenLink(String),
    /// User clicked a sticker that belongs to a set: show the pack (`StickerSetBox`).
    OpenStickerSet(crate::history::backend::StickerSetId),
    /// User clicked a photo/video/sticker media item.
    OpenMedia {
        /// Target message id.
        msg_id: MsgId,
        /// Optional album tile index.
        album_index: Option<usize>,
    },
    /// User triggered Save As on a document or media item.
    SaveAs {
        /// Target message id.
        msg_id: MsgId,
        /// File name suggestion.
        file_name: String,
    },
    /// User chose "Save GIF" on an inline GIF animation message (subject to Saved GIFs limit).
    SaveGif(MsgId),
    /// Reply to `msg_id` quoting a drag-selected text passage with entities.
    ReplyWithQuote {
        /// Target message id.
        msg_id: MsgId,
        /// Selected quote excerpt.
        quote: ReplyQuote,
    },
    /// Translate or restore original text for `msg_id`.
    Translate {
        /// Target message id.
        msg_id: MsgId,
        /// True to clear translation and show original text.
        clear: bool,
    },
    /// Open `TranslateBox` modal for `msg_id` (`boxes/translate_box.cpp`).
    OpenTranslateModal(MsgId),
    /// Open `AddPollOptionBox` for an open-answers poll (`messages.addPollAnswer`).
    OpenAddPollOption(MsgId),
    /// Open `ViewOnceMediaModal` for a self-destructing / view-once media bubble.
    OpenViewOnceMedia {
        /// Target message id.
        msg_id: MsgId,
        /// Media kind.
        kind: tg_app_info::boxes::self_destruct::ViewOnceMediaKind,
        /// Wire `ttl_seconds`.
        ttl_seconds: i32,
    },
    /// Report one or more messages.
    Report(Vec<MsgId>),
    /// Block a sender from the official `@Replies` chat (`contacts.blockFromReplies`).
    BlockFromReplies {
        /// Reply message id in the Replies chat.
        msg_id: MsgId,
        /// Display name of the offending sender shown in the confirmation modal.
        sender_name: String,
    },
    /// Open the full emoji + `tg-lottie` reactions picker for `msg_id`.
    OpenReactionsPicker {
        /// Target message id.
        msg_id: MsgId,
        /// Anchor screen position.
        pos: Pos2,
    },
    /// Open the Who-Reacted peer list for `msg_id` (`messages.getMessageReactionsList`).
    OpenWhoReacted(MsgId),
    /// Toggle paid-reaction leaderboard privacy on `msg_id` (`messages.togglePaidReactionPrivacy`).
    TogglePaidReactionPrivacy(MsgId),
    /// Report a reaction on `msg_id` (`messages.reportReaction`).
    ReportMessageReaction(MsgId),
    /// Remove all reactions by `participant_id` across this chat (`messages.deleteParticipantReactions`).
    RemoveParticipantReactions {
        /// Context message ID.
        msg_id: MsgId,
        /// Participant whose reactions should be removed.
        participant_id: PeerId,
    },
    /// Open the "Seen by" group read-receipts modal (`messages.getMessageReadParticipants`).
    OpenSeenBy {
        /// Target message id.
        msg_id: MsgId,
        /// Loaded group read-receipt participants.
        readers: Vec<tg_app_core::core_types::dto::engagement::MessageReadParticipant>,
    },
    /// Clicked the 1:1 DM `"Read at HH:MM"` / `"read · show when"` context-menu row.
    OpenReadDateStatus(tg_app_core::core_types::dto::engagement::OutboxReadDateStatus),
    /// Export and copy a message/post link via `channels.exportMessageLink` (with local fallback).
    CopyMessageLink(MsgId),
    /// Clicked `"Sent by {user}"` (`channels.getMessageAuthor`) on a channel post.
    OpenMessageAuthor(tg_app_core::core_types::dto::engagement::MessageAuthorInfo),
    /// Open the Poll Voters list for a non-anonymous poll (`messages.getPollVotes`).
    OpenPollVoters(MsgId),
    /// Retract the current user's vote on a poll (`messages.sendVote` with empty options).
    RetractPollVote(MsgId),
    /// Stop/close an open poll (`messages.editMessage` with `Poll::closed = true`).
    StopPoll(MsgId),
    /// Open the discussion/comments thread for channel post `msg_id`.
    OpenComments(MsgId),
    /// A copy was refused: the chat or a selected message restricts saving content
    /// (`HistoryInner::showCopyRestriction`). The toast text.
    CopyRestricted(String),
    /// Copied text with preserved `tg-text` entities.
    CopiedRich {
        /// Toast summary; empty for no toast (tdesktop shows none for text copies).
        summary: String,
        /// Preserved rich text payload.
        rich: CopiedRichText,
    },
    /// User clicked an inline keyboard button beneath a message bubble.
    InlineKeyboard(InlineKeyboardActivation),
    /// User clicked an HTML5 game card to launch the game (`history_view_game.cpp`).
    PlayGame(MsgId),
    /// User toggled a checklist task (`history_view_todo_list.cpp`).
    ToggleTodoItem {
        /// Checklist message id.
        msg_id: MsgId,
        /// Task id inside the checklist.
        item_id: i32,
    },
    /// User clicked "+ Add task" on a checklist bubble.
    AddTodoTasks(MsgId),
    /// User clicked "Edit" on an outgoing checklist bubble.
    EditTodoList(MsgId),
    /// User clicked "Edit" on a fact-check bubble footer (`ui/boxes/edit_factcheck_box.cpp`).
    EditFactCheck(MsgId),
    /// User clicked the `"→A"` voice/round-video transcribe button.
    ToggleTranscribe {
        /// Target message id.
        msg_id: MsgId,
        /// Duration in seconds.
        duration_secs: u32,
        /// True if round video note.
        is_round_video: bool,
    },
    /// User rated a completed transcription (`messages.rateTranscribedAudio`).
    RateTranscribe {
        /// Target message id.
        msg_id: MsgId,
        /// True for thumbs-up, false for thumbs-down.
        good: bool,
    },
    /// User clicked a sandboxed HTML widget card (`history_view_html_widget.cpp`).
    OpenWidget(MsgId),
    /// User clicked an Invoice / Payment Receipt card inside a message bubble.
    OpenInvoice {
        /// Target message id.
        msg_id: MsgId,
        /// Invoice identifier (`tg://invoice?slug=...`, `msg:<peer>:<msg>`, or `receipt:<peer>:<msg>`).
        invoice_id: String,
    },
    /// User clicked a Paid Media unlock card or locked preview inside a message bubble.
    UnlockPaidMedia {
        /// Target message id.
        msg_id: MsgId,
        /// Stars amount required to unlock.
        stars_amount: i64,
        /// Display title for the confirmation box.
        title: String,
    },
    /// User clicked an interactive Story share / mention card (`history_view_story_mention.cpp`).
    OpenStory {
        /// Story owner peer ID.
        peer_id: PeerId,
        /// Story identifier.
        story_id: i32,
    },
    /// User clicked an interactive Giveaway / GiveawayResults card (`history_view_giveaway.cpp`).
    OpenGiveawayInfo {
        /// Chat peer ID.
        peer_id: PeerId,
        /// Giveaway message ID.
        msg_id: MsgId,
        /// Optional host channel ID.
        channel_id: Option<PeerId>,
        /// Optional original giveaway launch message ID.
        launch_msg_id: Option<MsgId>,
    },
    /// User clicked an interactive Live Video Stream card (`media_view_video_stream.cpp`).
    OpenVideoStream {
        /// Message ID.
        msg_id: MsgId,
        /// Group call ID.
        call_id: i64,
        /// Group call access hash.
        access_hash: u64,
        /// Whether RTMP/HLS broadcast.
        rtmp_stream: bool,
        /// Display title.
        title: String,
    },
    /// User tapped an interactive single-emoji message (`history_view_emoji_interactions.cpp`).
    TapInteractiveEmoji {
        /// Target message id.
        msg_id: MsgId,
        /// Emoticon string.
        emoticon: String,
        /// Bubble center anchor in screen coordinates.
        anchor: Pos2,
        /// True if outgoing layout.
        outgoing: bool,
    },
    /// User clicked Allow or Decline on a 1:1 no-forwards sharing request card.
    RespondNoForwardsRequest {
        /// Service message id of the request.
        msg_id: MsgId,
        /// `true` to accept enabling sharing (`enabled = false`), `false` to decline (`enabled = true`).
        accept: bool,
    },
    /// Context menu "Add Sticker to Favorites" (`messages.faveSticker` with file-ref refresh).
    FaveSticker(MsgId),
    /// Context menu "Save for Notifications" (`account.saveRingtone` with file-ref refresh).
    SaveRingtone(MsgId),
    /// User toggled AI message summary (`messages.summarizeText`).
    ToggleSummary(MsgId),
    /// User clicked the AI info icon on an expanded summary header.
    AboutAiSummary(MsgId),
    /// Recipient clicked the CTA on a Birthday Suggestion service card (`account.updateBirthday`).
    ApplySuggestedBirthday {
        /// Service message id.
        msg_id: MsgId,
        /// Suggested day of month (`1..=31`).
        day: u8,
        /// Suggested month (`1..=12`).
        month: u8,
        /// Optional suggested birth year.
        year: Option<u16>,
    },
    /// Recipient clicked `"Set Photo"` on a Userpic Suggestion service card (`photos.updateProfilePhoto`).
    ApplySuggestedUserpic {
        /// Service message id.
        msg_id: MsgId,
        /// Suggested photo id.
        photo_id: i64,
        /// Suggested photo access hash.
        access_hash: i64,
        /// Suggested photo file reference.
        file_reference: Vec<u8>,
        /// True if animated video avatar.
        is_video: bool,
    },
    /// Sender clicked `"View Photo"` on an outgoing Userpic Suggestion service card.
    ViewSuggestedUserpic(MsgId),
}

/// The page [`HistoryInnerWidget::load_peer`] waits for.
#[derive(Debug, Clone, Copy)]
struct PendingOpen {
    around_id: MsgId,
    peer_kind: PeerKind,
    /// The chat was switched to (scroll to the anchor, not keep the position).
    switching: bool,
}

/// Virtualized chat history list widget (`HistoryInner` / `HistoryView::ListWidget`).
#[derive(Debug)]
pub struct HistoryInnerWidget {
    /// Currently loaded peer id.
    peer_id: PeerId,
    /// Ordered loaded messages synchronized 1:1 with `list.items()`.
    messages: Vec<HistoryMessage>,
    /// Pure-logic history list model (`tg-history::HistoryList`).
    list: HistoryList,
    /// Jump-to-message highlight animator (`tg-history::ElementHighlighter`).
    highlighter: ElementHighlighter,
    /// Per-message layout cache keyed by `MsgId` for the current `layout_width`.
    layout_cache: HashMap<MsgId, CachedItem>,
    /// Viewport width (integer logical pixels) for which `layout_cache` is valid.
    layout_width: i32,
    /// Chat layout mode (`Default` vs `Wide`) for which `layout_cache` is valid.
    layout_wide: bool,
    /// Current vertical scroll offset in logical pixels.
    scroll_offset_y: f32,
    /// How far the rows are lifted by a pull past the bottom ([`Self::set_bottom_stretch`]).
    bottom_stretch: f32,
    /// Pending explicit scroll target (`Some(y)` overrides `ScrollArea` on next frame).
    pending_scroll_y: Option<f32>,
    /// A left-button text drag began in a message and the button is still held
    /// (`MouseAction::Selecting`): see `inner_drag_select.rs`.
    text_drag_active: bool,
    /// When the drag auto-scroll last ticked (`Ui::DraggingScrollManager`'s timer), while the
    /// pointer is held past the list's top or bottom.
    drag_scroll_clock: Option<f64>,
    /// True if next paint should jump to the very bottom.
    pending_scroll_to_bottom: bool,
    /// The first page of the chat is still loading (`HistoryWidget::_firstLoadRequest`): the
    /// list stays empty and the anchor waits for the page.
    pending_open: Option<PendingOpen>,
    /// Pending message id to center and highlight once laid out.
    pending_jump_msg: Option<MsgId>,
    /// `ShowAtUnreadMsgId`: the next paint scrolls the "Unread messages" bar to the top of the
    /// view (`HistoryWidget::unreadBarTop`) instead of pinning to the bottom.
    pending_scroll_to_unread_bar: bool,
    /// Last measured viewport height.
    last_viewport_h: f32,
    /// Whether the user is scrolled away from the bottom (controls FAB visibility).
    scrolled_up: bool,
    /// What the scroll bar of the last frame was drawn from.
    last_scroll_metrics: ScrollMetrics,
    /// Unread counter reported by backend slice.
    unread_count: u32,
    /// Cached textures & Lottie compositions.
    assets: MediaAssetCache,
    /// Inline media playback runtime (`tg-video`, `tg-lottie` per-size cache, `tg-audio` + player bar, spoilers, blur-up).
    media_runtime: HistoryMediaRuntime,
    /// Voice playback progress state.
    voice_state: VoicePlaybackState,
    /// Active right-click context menu state.
    context_menu: PopupMenuState,
    /// Message id that the context menu was opened for.
    context_menu_msg: Option<MsgId>,
    /// Screen position where the context menu was opened.
    context_menu_pos: Pos2,
    /// Cached WP-53/WP-61 read-receipt and channel-author metadata for the open context menu.
    context_menu_engagement: Option<crate::history::read_receipts::ContextMenuEngagementInfo>,
    /// Frame on which the context menu opened (its own right-click must not close it).
    context_menu_opened_frame: u64,
    /// Anchor message id for Shift+click range selection.
    selection_anchor: Option<MsgId>,
    /// Count of items laid out during the most recent frame (exposed for performance tests).
    last_frame_laid_out_count: usize,
    /// Count of items painted during the most recent frame (exposed for performance tests).
    last_frame_painted_count: usize,
    /// Shared per-chat inline keyboard loading/pressed state.
    inline_kb_state: InlineKeyboardState,
    /// Message ids whose fact-check footer note is currently expanded.
    expanded_factchecks: HashSet<MsgId>,
    /// Screen-space bubble rects recorded during the most recent paint pass (for layout tests).
    last_painted_bubble_rects: HashMap<MsgId, Rect>,
    /// The message flying from the field this frame (`send_flight`).
    send_flight_row: Option<crate::history::send_flight::SendFlightRow>,
    /// Per-chat viewport read-metrics visibility tracker (`HistoryView::ReadMetricsTracker`).
    read_metrics: Option<tg_app_core::core_types::read_metrics::ReadMetricsTracker>,
    /// Messages painted this frame (filled while painting the visible window).
    painted_msg_ids: Vec<MsgId>,
    /// The painted set last sent to `mark_visible_reactions_read`, so the backend is only asked
    /// again when the visible messages change.
    reactions_read_for: Vec<MsgId>,
    /// Highest server message id already confirmed/sent as inbox-read for the active peer.
    last_inbox_read_sent: MsgId,
    /// Highest unread incoming message id shown this frame (`readTill`), the only id that may
    /// advance the inbox read watermark.
    read_till_shown: Option<MsgId>,
    /// Set by the window: the user can see the chat (`MainWindow::markingAsRead`).
    marking_as_read: bool,
    /// Set by the live host: one of its overlays (stories, Business, Payments, wallet) covers
    /// the chat.
    host_overlay_open: bool,
    /// View-layer interactive emoji burst animation overlays (`HistoryView::EmojiInteractions`).
    emoji_interactions: crate::history::emoji_interactions::HistoryViewEmojiInteractions,
    /// Last painted bubble center & outgoing flag per visible message (for incoming emoji bursts).
    last_bubble_anchors: HashMap<MsgId, (Pos2, bool)>,
    /// Last frame timestamp in ms (used by programmatic highlight triggers).
    last_now_ms: i64,
    /// Last time downloading inline photos were re-polled (`refresh_loading_media`).
    last_media_poll_ms: i64,
    /// Last observed AI summary fingerprint `(shown, loading, text_len)` per message for cache invalidation.
    summary_fingerprints: HashMap<MsgId, (bool, bool, usize)>,
    /// Configured double-click quick action (`CoreSettings::chat_quick_action`).
    chat_quick_action: tg_core::settings::DoubleClickQuickAction,
    /// Messages of only 1-3 emoji drawn large without a bubble (`CoreSettings::large_emoji`).
    large_emoji: bool,
    /// Whether hover corner Quick Reply pill is enabled (`CoreSettings::corner_reply`).
    corner_reply: bool,
    /// Whether hover corner Quick React pill is enabled (`CoreSettings::corner_reaction`).
    corner_reaction: bool,
    /// Quick reaction emoji used by double-click React and corner React pill.
    quick_reaction_emoji: String,
    /// Reduced-motion flag (`PowerSavingFlags::ANIMATIONS`).
    reduced_motion: bool,
    /// Hover fade animation tracker for bubble corner quick actions.
    bubble_hover_quick_actions: crate::history::quick_action::BubbleHoverQuickActions,
    /// Messages of the opened slice whose inline media has not been asked for yet: `load_peer`
    /// asks for the ones near the viewport on the first frame, the rest follow frame by frame.
    media_backlog: HashSet<MsgId>,
    /// Messages whose media is still arriving (polled by `refresh_loading_media`).
    media_watch: MediaWatch,
    /// Version gate for the backend reads repeated on every frame.
    read_gate: BackendReadGate,
    /// Reactors of a private chat (the peer and the own name), kept while the gate allows.
    reaction_userpics: Option<Gated<ReactionUserpics>>,
    /// Messages holding prepared text, oldest first (`trim_paint_caches`).
    paint_cache_order: VecDeque<MsgId>,
    /// The painted messages `paint_cache_order` was last updated for.
    paint_cache_painted: Vec<MsgId>,
    /// Revealed spoilers and expanded quotes of the open chat's messages (TGB-3009).
    text_states: HashMap<MsgId, MessageTextState>,
    /// Holes of deleted messages closing (TGB-3011, `ThanosEffectController` collapse).
    delete_collapse: crate::history::delete_collapse::CollapseAnimator,
}

/// The text state of a message nobody clicked.
static UNTOUCHED_TEXT: MessageTextState = MessageTextState {
    spoilers_revealed_at: None,
    expanded_quotes: Vec::new(),
};

impl Default for HistoryInnerWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryInnerWidget {
    /// Creates an empty virtualized history list.
    #[must_use]
    pub fn new() -> Self {
        Self {
            peer_id: PeerId::ZERO,
            messages: Vec::new(),
            list: HistoryList::new(),
            highlighter: ElementHighlighter::new(),
            layout_cache: HashMap::new(),
            layout_width: 0,
            layout_wide: false,
            scroll_offset_y: 0.0,
            bottom_stretch: 0.0,
            pending_scroll_y: None,
            text_drag_active: false,
            drag_scroll_clock: None,
            pending_scroll_to_bottom: true,
            pending_open: None,
            pending_jump_msg: None,
            pending_scroll_to_unread_bar: false,
            last_viewport_h: 600.0,
            painted_msg_ids: Vec::new(),
            reactions_read_for: Vec::new(),
            last_inbox_read_sent: MsgId(0),
            read_till_shown: None,
            marking_as_read: true,
            host_overlay_open: false,
            scrolled_up: false,
            last_scroll_metrics: ScrollMetrics::default(),
            unread_count: 0,
            assets: MediaAssetCache::new(),
            media_runtime: HistoryMediaRuntime::new(false),
            voice_state: VoicePlaybackState::default(),
            context_menu: PopupMenuState::new(),
            context_menu_msg: None,
            context_menu_pos: Pos2::ZERO,
            context_menu_engagement: None,
            context_menu_opened_frame: 0,
            selection_anchor: None,
            last_frame_laid_out_count: 0,
            last_frame_painted_count: 0,
            inline_kb_state: InlineKeyboardState::new(),
            expanded_factchecks: HashSet::new(),
            last_painted_bubble_rects: HashMap::new(),
            send_flight_row: None,
            read_metrics: None,
            emoji_interactions:
                crate::history::emoji_interactions::HistoryViewEmojiInteractions::new(),
            last_bubble_anchors: HashMap::new(),
            last_now_ms: 0,
            last_media_poll_ms: 0,
            summary_fingerprints: HashMap::new(),
            chat_quick_action: tg_core::settings::DoubleClickQuickAction::Reply,
            large_emoji: true,
            corner_reply: true,
            corner_reaction: true,
            quick_reaction_emoji: crate::history::quick_action::DEFAULT_QUICK_REACTION_EMOJI
                .to_owned(),
            reduced_motion: false,
            bubble_hover_quick_actions:
                crate::history::quick_action::BubbleHoverQuickActions::default(),
            media_backlog: HashSet::new(),
            media_watch: MediaWatch::default(),
            read_gate: BackendReadGate::default(),
            reaction_userpics: None,
            paint_cache_order: VecDeque::new(),
            paint_cache_painted: Vec::new(),
            text_states: HashMap::new(),
            delete_collapse: Default::default(),
        }
    }

    /// Drops the prepared text of messages painted long ago, keeping it for the
    /// [`PAINT_CACHES_KEPT`] most recently shown ones and for everything painted this frame.
    /// The layouts stay; only the galleys and strings kept for painting go.
    fn trim_paint_caches(&mut self) {
        if self.painted_msg_ids == self.paint_cache_painted {
            // The same messages as on the last frame: nothing new holds prepared text.
            return;
        }
        self.paint_cache_painted.clone_from(&self.painted_msg_ids);
        for id in &self.painted_msg_ids {
            if !self.paint_cache_order.contains(id) {
                self.paint_cache_order.push_back(*id);
            }
        }
        let mut checks = self.paint_cache_order.len();
        while self.paint_cache_order.len() > PAINT_CACHES_KEPT && checks > 0 {
            checks -= 1;
            let Some(id) = self.paint_cache_order.pop_front() else {
                break;
            };
            if self.painted_msg_ids.contains(&id) {
                self.paint_cache_order.push_back(id);
            } else if let Some(cached) = self.layout_cache.get_mut(&id) {
                cached.paint = MessagePaintCache::default();
            }
        }
    }

    /// The messages painted on the last frame (tests).
    #[cfg(test)]
    pub(crate) fn painted_message_ids(&self) -> &[MsgId] {
        &self.painted_msg_ids
    }

    /// Voice playback state, to put a message into a playing state (tests).
    #[cfg(test)]
    pub(crate) fn voice_state_mut(&mut self) -> &mut VoicePlaybackState {
        &mut self.voice_state
    }

    /// How many messages hold prepared text (tests).
    #[cfg(test)]
    pub(crate) fn prepared_message_count(&self) -> usize {
        self.layout_cache
            .values()
            .filter(|cached| cached.paint.holds_text())
            .count()
    }

    /// Index of `id` among the loaded messages.
    ///
    /// Server ids are loaded in ascending order with local ids (drafts, messages being sent)
    /// after them, so a binary search finds a loaded server message in `O(log n)`. Whatever it
    /// does not find is looked up by a scan, so the result never depends on that order.
    fn index_of(&self, id: MsgId) -> Option<usize> {
        if id.0 > 0 {
            let guess = self.messages.partition_point(|m| m.id.0 > 0 && m.id < id);
            if self.messages.get(guess).is_some_and(|m| m.id == id) {
                return Some(guess);
            }
        }
        self.messages.iter().position(|m| m.id == id)
    }

    /// Invalidates cached layout for `msg_id` so the next frame re-measures it.
    pub fn invalidate_message_layout(&mut self, msg_id: MsgId) {
        self.layout_cache.remove(&msg_id);
    }

    /// Triggers a brief `ElementHighlighter` pulse on `msg_id`.
    pub fn highlight_message(&mut self, msg_id: MsgId) {
        let full = to_hist_full(FullMsgId {
            peer: self.peer_id,
            msg: msg_id,
        });
        self.highlighter
            .highlight(HighlightTarget::whole_message(full), self.last_now_ms);
    }

    /// Returns a reference to the view-layer interactive emoji overlay manager.
    #[must_use]
    pub fn emoji_interactions(
        &self,
    ) -> &crate::history::emoji_interactions::HistoryViewEmojiInteractions {
        &self.emoji_interactions
    }

    /// Plays an interactive emoji burst overlay anchored to `msg_id` (or explicit `anchor`).
    pub fn play_emoji_interaction(
        &mut self,
        msg_id: MsgId,
        emoticon: String,
        anchor: Option<Pos2>,
        outgoing: bool,
        variant_index: usize,
        now_ms: u64,
    ) {
        self.play_emoji_interaction_with_lottie(
            msg_id,
            emoticon,
            anchor,
            outgoing,
            variant_index,
            None,
            now_ms,
        );
    }

    /// Plays an interactive emoji overlay anchored to `msg_id` (or explicit `anchor`),
    /// using the server Lottie fixture `(doc_id, lottie_json)` from `EmojiStickersPack` when
    /// available and falling back to the procedural burst otherwise.
    #[allow(clippy::too_many_arguments)]
    pub fn play_emoji_interaction_with_lottie(
        &mut self,
        msg_id: MsgId,
        emoticon: String,
        anchor: Option<Pos2>,
        outgoing: bool,
        variant_index: usize,
        lottie: Option<(u64, String)>,
        now_ms: u64,
    ) {
        let (pos, is_out) = match anchor {
            Some(p) => (p, outgoing),
            None => self
                .last_bubble_anchors
                .get(&msg_id)
                .copied()
                .unwrap_or((Pos2::new(240.0, 240.0), outgoing)),
        };
        self.emoji_interactions.play_with_lottie(
            msg_id,
            emoticon,
            pos,
            is_out,
            variant_index,
            lottie,
            now_ms,
        );
    }

    /// Finalizes all active viewport read metrics and submits them to `backend` (e.g. on peer switch).
    pub fn finalize_read_metrics_for_backend(
        &mut self,
        backend: &mut dyn HistoryBackend,
        now_ms: u64,
    ) {
        if let Some(ref mut tracker) = self.read_metrics {
            let pid = tracker.peer_id();
            let finalized = tracker.finalize_all(now_ms);
            if !finalized.is_empty() {
                backend.report_read_metrics(pid, finalized);
            }
        }
    }

    /// Synchronizes active Layer-233 streamed bot draft messages (negative local `MsgId`) at the
    /// bottom of the loaded history list without disrupting server messages.
    pub fn sync_streamed_drafts(&mut self, drafts: Vec<HistoryMessage>, peer_kind: PeerKind) {
        if !self.list.loaded_at_bottom {
            return;
        }
        // Called on every frame: compare in place, without collecting the drafts.
        let mut existing = self.messages.iter().filter(|m| m.id.is_streamed_draft());
        let unchanged = drafts.iter().all(|b| {
            existing.next().is_some_and(|a| {
                a.id == b.id && a.text == b.text && a.extra.rich_message == b.extra.rich_message
            })
        }) && existing.next().is_none();
        if unchanged {
            return;
        }
        self.media_watch.dirty = true;
        self.messages.retain(|m| !m.id.is_streamed_draft());
        for d in drafts {
            self.layout_cache.remove(&d.id);
            self.messages.push(d);
        }
        let is_group = matches!(peer_kind, PeerKind::Group);
        let items: Vec<HistoryListItem> = self
            .messages
            .iter()
            .map(|m| build_list_item(m, is_group, self.large_emoji, self.layout_wide))
            .collect();
        self.list.set_items(items);
        self.update_date_and_unread_heights();
    }

    /// Returns a reference to the inline keyboard loading/pressed state.
    #[must_use]
    pub const fn inline_kb_state(&self) -> &InlineKeyboardState {
        &self.inline_kb_state
    }

    /// Returns mutable access to the inline keyboard loading/pressed state.
    pub fn inline_kb_state_mut(&mut self) -> &mut InlineKeyboardState {
        &mut self.inline_kb_state
    }

    /// Returns the currently loaded peer id.
    #[must_use]
    pub const fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    /// Returns loaded messages in oldest-to-newest order.
    #[must_use]
    pub fn messages(&self) -> &[HistoryMessage] {
        &self.messages
    }

    /// Returns a reference to the underlying `tg-history` [`HistoryList`].
    #[must_use]
    pub const fn list(&self) -> &HistoryList {
        &self.list
    }

    /// Returns a reference to the [`ElementHighlighter`].
    #[must_use]
    pub const fn highlighter(&self) -> &ElementHighlighter {
        &self.highlighter
    }

    /// Returns a reference to the inline media playback runtime.
    #[must_use]
    pub const fn media_runtime(&self) -> &HistoryMediaRuntime {
        &self.media_runtime
    }

    /// Returns mutable access to the inline media playback runtime.
    pub fn media_runtime_mut(&mut self) -> &mut HistoryMediaRuntime {
        // The caller may register or change a descriptor: look at the list again.
        self.media_watch.dirty = true;
        &mut self.media_runtime
    }

    /// Applies [`tg_core::settings::CoreSettings`] autoplay, audio, and quick-action preferences.
    pub fn apply_core_settings(&mut self, settings: &tg_core::settings::CoreSettings) {
        self.media_runtime.apply_core_settings(settings);
        self.chat_quick_action = settings.chat_quick_action;
        self.set_large_emoji(settings.large_emoji);
        self.corner_reply = settings.corner_reply;
        self.corner_reaction = settings.corner_reaction;
        self.reduced_motion =
            settings.is_power_saving_on(tg_core::settings::PowerSavingFlags::ANIMATIONS);
    }

    /// Turns "Large emoji" on or off (`Element::refreshMedia` for every emoji-only message):
    /// their layouts and bubble attachment change.
    pub fn set_large_emoji(&mut self, enabled: bool) {
        if self.large_emoji == enabled {
            return;
        }
        self.large_emoji = enabled;
        let mut changed = false;
        for (msg, item) in self.messages.iter().zip(self.list.items_mut()) {
            if crate::history::large_emoji::large_emoji_of(msg, true).is_some() {
                item.attach_meta.unwrapped = enabled;
                self.layout_cache.remove(&msg.id);
                changed = true;
            }
        }
        if changed {
            self.list.recount_attachments();
            self.list.relayout_positions();
        }
    }

    /// Returns the configured double-click quick action (`HistoryView::CurrentQuickAction`).
    #[must_use]
    pub const fn chat_quick_action(&self) -> tg_core::settings::DoubleClickQuickAction {
        self.chat_quick_action
    }

    /// Overrides the configured double-click quick action (for tests / settings previews).
    pub fn set_chat_quick_action(&mut self, action: tg_core::settings::DoubleClickQuickAction) {
        self.chat_quick_action = action;
    }

    /// Returns the number of currently multi-selected messages.
    #[must_use]
    pub fn selected_count(&self) -> usize {
        self.list.selection.selected_items.len()
    }

    /// Returns the currently multi-selected message ids.
    #[must_use]
    pub fn selected_msg_ids(&self) -> Vec<MsgId> {
        self.list
            .selection
            .selected_items
            .iter()
            .map(|id| MsgId(id.msg.0 as i32))
            .collect()
    }

    /// Clears both multi-item and text drag selections.
    pub fn clear_selection(&mut self) {
        self.list.selection.clear();
        self.selection_anchor = None;
    }

    /// Toggles whole-item multi-selection for `msg_id` and records it as the Shift+click anchor.
    pub fn toggle_select_message(&mut self, msg_id: MsgId) {
        let full = to_hist_full(FullMsgId {
            peer: self.peer_id,
            msg: msg_id,
        });
        self.list.selection.toggle_item(full);
        self.selection_anchor = Some(msg_id);
    }

    /// Selects the entire contiguous range of non-service messages between the current anchor
    /// (or first selected item) and `target_msg_id` (`Shift+click` range selection).
    pub fn select_range_to(&mut self, target_msg_id: MsgId) {
        let Some(target_idx) = self.messages.iter().position(|m| m.id == target_msg_id) else {
            return;
        };
        let anchor_idx = self
            .selection_anchor
            .and_then(|aid| self.messages.iter().position(|m| m.id == aid))
            .unwrap_or(target_idx);
        let lo = anchor_idx.min(target_idx);
        let hi = anchor_idx.max(target_idx);
        for msg in &self.messages[lo..=hi] {
            if msg.is_service() {
                continue;
            }
            let full = to_hist_full(FullMsgId {
                peer: self.peer_id,
                msg: msg.id,
            });
            if !self.list.selection.selected_items.contains(&full) {
                self.list.selection.toggle_item(full);
            }
        }
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(target_msg_id);
        }
    }

    /// Selected text with its `tg-text` UTF-16 entities (for a formatted paste).
    #[must_use]
    pub fn extract_selected_rich_text(&self) -> CopiedRichText {
        self.selection_copy()
            .map(|copy| copy.rich)
            .unwrap_or_default()
    }

    /// The messages a copy of the current selection takes text from: the selected items, the
    /// messages a drag crossed, or the one message a text selection is in.
    fn selection_messages(&self) -> Vec<&HistoryMessage> {
        let selection = &self.list.selection;
        if selection.has_selected_items() {
            return self
                .messages
                .iter()
                .filter(|m| {
                    selection.selected_items.contains(&to_hist_full(FullMsgId {
                        peer: self.peer_id,
                        msg: m.id,
                    }))
                })
                .collect();
        }
        let (Some(from), Some(to)) = (selection.from, selection.to) else {
            return Vec::new();
        };
        let lo = from.item_index.min(to.item_index);
        let hi = from.item_index.max(to.item_index);
        self.messages
            .get(lo..=hi.min(self.messages.len().saturating_sub(1)))
            .unwrap_or_default()
            .iter()
            .collect()
    }

    /// The toast shown instead of copying the selection when the chat or a selected message
    /// restricts saving content (`HistoryInner::showCopyRestrictionForSelected`).
    #[must_use]
    pub fn selection_copy_restriction(
        &self,
        chat_allows_forwarding: bool,
        kind: PeerKind,
    ) -> Option<String> {
        let selected = self.selection_messages();
        (!selected.is_empty() && copy_restriction::restricts_any(chat_allows_forwarding, selected))
            .then(|| copy_restriction::toast_text(kind))
    }

    /// What copying the current selection puts on the clipboard (`HistoryInner::
    /// getSelectedTextParts`): the selected piece of one message's text, or whole messages
    /// (selected items, or a drag that crossed into other messages) wrapped with their time
    /// and author when there are several.
    #[must_use]
    pub fn selection_copy(&self) -> Option<SelectionCopy> {
        let selection = &self.list.selection;
        if selection.has_selected_items() {
            let items: Vec<&HistoryMessage> = self
                .messages
                .iter()
                .filter(|m| {
                    selection.selected_items.contains(&to_hist_full(FullMsgId {
                        peer: self.peer_id,
                        msg: m.id,
                    }))
                })
                .collect();
            return copy_selection::items_copy(&items);
        }
        let (Some(from), Some(to)) = (selection.from, selection.to) else {
            return None;
        };
        if from.item_index != to.item_index {
            let lo = from.item_index.min(to.item_index);
            let hi = from.item_index.max(to.item_index);
            let items: Vec<&HistoryMessage> = self
                .messages
                .get(lo..=hi.min(self.messages.len().saturating_sub(1)))
                .unwrap_or_default()
                .iter()
                .collect();
            return copy_selection::items_copy(&items);
        }
        let msg = self.messages.get(from.item_index)?;
        let full = to_hist_full(FullMsgId {
            peer: self.peer_id,
            msg: msg.id,
        });
        let sel = selection.selection_for_item(from.item_index, full);
        if sel.is_empty() {
            return None;
        }
        let piece = if sel.is_full() {
            msg.text.clone()
        } else {
            slice_text_with_entities(&msg.text, usize::from(sel.from), usize::from(sel.to))
        };
        copy_selection::text_selection_copy(piece)
    }

    /// Where the context menu for `msg_id` opened relative to the selection (`isUponSelected`).
    fn upon_selected(&self, msg_id: MsgId) -> UponSelected {
        let Some(index) = self.index_of(msg_id) else {
            return UponSelected::No;
        };
        let selection = &self.list.selection;
        let full = to_hist_full(FullMsgId {
            peer: self.peer_id,
            msg: msg_id,
        });
        let sel = selection.selection_for_item(index, full);
        if sel.is_empty() {
            UponSelected::No
        } else if selection.has_selected_items()
            || selection
                .from
                .zip(selection.to)
                .is_some_and(|(a, b)| a.item_index != b.item_index)
        {
            UponSelected::Items
        } else {
            UponSelected::Text
        }
    }

    /// Returns a [`ReplyQuote`] if `msg_id` currently has a partial drag-selected text range.
    #[must_use]
    pub fn selected_quote_for_msg(&self, msg_id: MsgId) -> Option<ReplyQuote> {
        let idx = self.messages.iter().position(|m| m.id == msg_id)?;
        let msg = &self.messages[idx];
        let full = to_hist_full(FullMsgId {
            peer: self.peer_id,
            msg: msg_id,
        });
        let sel = self.list.selection.selection_for_item(idx, full);
        if sel.is_empty() || sel.is_full() {
            return None;
        }
        let sliced =
            slice_text_with_entities(&msg.text, usize::from(sel.from), usize::from(sel.to));
        if sliced.text.trim().is_empty() {
            return None;
        }
        let offset_utf16: usize = msg
            .text
            .text
            .chars()
            .take(usize::from(sel.from))
            .map(char::len_utf16)
            .sum();
        Some(ReplyQuote {
            text: sliced.text,
            entities: sliced.entities,
            offset_utf16: offset_utf16 as i32,
        })
    }

    /// Returns `true` if scrolled up far enough to show the scroll-to-bottom button.
    #[must_use]
    pub const fn show_scroll_down_button(&self) -> bool {
        self.scrolled_up || !self.list.loaded_at_bottom
    }

    /// Returns `true` when reduced-motion / power-saving animation suppression is active.
    #[must_use]
    pub const fn reduced_motion(&self) -> bool {
        self.reduced_motion
    }

    /// Returns current unread count for the scroll-to-bottom FAB badge.
    #[must_use]
    pub const fn unread_count(&self) -> u32 {
        self.unread_count
    }

    /// Returns count of items laid out during the last visible-range pass (for virtualization tests).
    #[must_use]
    pub const fn last_frame_laid_out_count(&self) -> usize {
        self.last_frame_laid_out_count
    }

    /// Returns count of items painted during the last visible-range pass.
    #[must_use]
    pub const fn last_frame_painted_count(&self) -> usize {
        self.last_frame_painted_count
    }

    /// Returns the screen-space bubble rectangle painted for `msg_id` during the most recent frame.
    #[doc(hidden)]
    #[must_use]
    pub fn last_painted_bubble_rect(&self, msg_id: MsgId) -> Option<Rect> {
        self.last_painted_bubble_rects.get(&msg_id).copied()
    }

    /// Sets the message the next paint draws into the send flight's layer.
    pub(crate) fn set_send_flight_row(
        &mut self,
        row: Option<crate::history::send_flight::SendFlightRow>,
    ) {
        self.send_flight_row = row;
    }

    /// Takes the send flight row back after the paint, with where its text was drawn.
    pub(crate) fn take_send_flight_row(
        &mut self,
    ) -> Option<crate::history::send_flight::SendFlightRow> {
        self.send_flight_row.take()
    }

    /// Returns the cached message layout of `msg_id` (tests).
    #[cfg(test)]
    pub(crate) fn cached_message_layout(&self, msg_id: MsgId) -> Option<&MessageLayout> {
        match &self.layout_cache.get(&msg_id)?.layout {
            CachedItemLayout::Message(layout) => Some(layout),
            CachedItemLayout::Service(_) => None,
        }
    }

    /// What the scroll bar of the last frame was drawn from: its thumb sits at
    /// `offset / (content_height - viewport_height)` of the track.
    #[must_use]
    pub const fn last_scroll_metrics(&self) -> ScrollMetrics {
        self.last_scroll_metrics
    }

    /// Current vertical scroll offset in logical pixels (tests).
    #[cfg(test)]
    pub(crate) const fn scroll_offset_y(&self) -> f32 {
        self.scroll_offset_y
    }

    /// Requests scrolling to the newest message at the bottom.
    pub fn request_scroll_to_bottom(&mut self) {
        self.list.scroll_top_state = tg_history::ScrollTopState::default();
        // Back at the bottom the chat holds what a fresh open holds; the pages read while
        // scrolled up are paged in again if the user scrolls back (TGB-2952).
        if self.list.loaded_at_bottom {
            self.unload_oldest_messages(OPEN_PAGE_BEFORE + 1);
        }
        self.pending_scroll_to_bottom = true;
        self.pending_scroll_to_unread_bar = false;
        self.pending_jump_msg = None;
    }

    /// Sets an explicit vertical scroll offset in logical pixels (used by tests and restoration).
    pub fn set_scroll_offset(&mut self, y: f32) {
        self.list.scroll_top_state = tg_history::ScrollTopState::default();
        self.pending_scroll_y = Some(y.max(0.0));
        self.pending_scroll_to_bottom = false;
    }

    /// Requests jumping to `msg_id`, loading a slice around it if not currently loaded,
    /// centering it in the viewport, and triggering `ElementHighlighter`.
    pub fn jump_to_message(
        &mut self,
        msg_id: MsgId,
        backend: &dyn HistoryBackend,
        peer_kind: PeerKind,
        now_ms: i64,
    ) {
        let loaded = self.index_of(msg_id).is_some();
        if !loaded {
            let slice = backend.load_slice_around(self.peer_id, msg_id, 40, 40);
            self.register_media_descriptors(self.peer_id, &slice.messages, backend);
            self.apply_slice(self.peer_id, slice, peer_kind, false);
        }
        self.pending_jump_msg = Some(msg_id);
        self.pending_scroll_to_bottom = false;
        self.pending_scroll_to_unread_bar = false;
        let full = to_hist_full(FullMsgId {
            peer: self.peer_id,
            msg: msg_id,
        });
        self.highlighter
            .highlight(HighlightTarget::whole_message(full), now_ms);
    }

    /// Finds the loaded messages whose registered media is still arriving.
    fn scan_media_watch(&mut self, now_ms: i64) {
        use tg_app_core::core_types::dto::inline_media::PhotoLoadState;
        // A photo still being read from the disk cache is re-polled on the next frame (the
        // cache repaints when the read lands), not after the download poll interval.
        let mut cache_reads = false;
        let loading: Vec<MsgId> = self
            .messages
            .iter()
            .flat_map(|m| {
                let tiles = match &m.media {
                    Some(HistoryMedia::Album(items)) => items.as_slice(),
                    _ => &[],
                };
                let own = m.id;
                std::iter::once(own).chain(
                    tiles
                        .iter()
                        .map(|item| item.msg_id)
                        .filter(move |&id| id != own),
                )
            })
            .filter(|id| {
                match self
                    .media_runtime
                    .descriptor(*id)
                    .and_then(|d| d.photo_load.as_ref())
                {
                    Some(PhotoLoadState::Loading { .. }) => true,
                    Some(PhotoLoadState::Pending) => {
                        cache_reads = true;
                        true
                    }
                    _ => false,
                }
            })
            .collect();
        // Round videos registered while their bytes were still downloading (the descriptor is
        // otherwise read once per chat load, so the circle never got its video).
        let pending_rounds: Vec<MsgId> = self
            .messages
            .iter()
            .filter(|m| matches!(m.media, Some(HistoryMedia::RoundVideo(_))))
            .map(|m| m.id)
            .filter(|id| {
                self.media_runtime
                    .descriptor(*id)
                    .is_some_and(|d| d.video_data.is_none())
            })
            .collect();
        self.media_watch = MediaWatch {
            loading,
            pending_rounds,
            cache_reads,
            dirty: false,
            scanned_list: Some(self.media_watch_list_key()),
            scanned_ms: now_ms,
        };
    }

    fn media_watch_list_key(&self) -> (usize, Option<MsgId>, Option<MsgId>) {
        (
            self.messages.len(),
            self.messages.first().map(|m| m.id),
            self.messages.last().map(|m| m.id),
        )
    }

    /// Hands the download clicks of the last frame to `backend` (cancel / download on a photo,
    /// the first play of a voice message) and reads the new state back at once, and asks for
    /// the voice message that waits for its file until it is there.
    fn apply_media_downloads(&mut self, ctx: &egui::Context, backend: &dyn HistoryBackend) {
        use crate::history::inline_media::MediaDownloadAction;
        self.media_runtime.guard_unloaded_voice();
        for (id, action) in self.media_runtime.take_download_actions() {
            match action {
                MediaDownloadAction::Start => backend.start_media_download(self.peer_id, id),
                MediaDownloadAction::Cancel => backend.cancel_media_download(self.peer_id, id),
                MediaDownloadAction::Stream => {
                    let loader = backend.open_media_stream(self.peer_id, id);
                    self.media_runtime.provide_voice_stream(id, loader);
                }
            }
            if let Some(desc) = backend.inline_media_descriptor(self.peer_id, id) {
                match desc.photo_load {
                    Some(state) => self.media_runtime.set_photo_load_state(id, state),
                    None => self.media_runtime.register_descriptor(id, desc),
                }
            }
            self.media_watch.dirty = true;
        }
        // TGB-3115: a streamed message whose file the backend kept after a full play.
        let (rechecks, more) = self.media_runtime.take_stream_rechecks();
        for id in rechecks {
            if let Some(desc) = backend.inline_media_descriptor(self.peer_id, id)
                && desc.audio_bytes.is_some()
            {
                self.media_runtime.register_descriptor(id, desc);
                self.media_watch.dirty = true;
            }
        }
        if more {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        if let Some(id) = self.media_runtime.pending_voice_msg() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            if let Some(desc) = backend.inline_media_descriptor(self.peer_id, id)
                && desc.audio_bytes.is_some()
            {
                self.media_runtime.register_descriptor(id, desc);
            }
        }
    }

    /// Re-polls photos that are still downloading so they replace their blurred thumbnail as
    /// soon as the bytes arrive (tdesktop repaints on `Data::PhotoMedia` load progress); the
    /// descriptor was otherwise only read when the chat was (re)loaded.
    fn refresh_loading_media(
        &mut self,
        ctx: &egui::Context,
        backend: &dyn HistoryBackend,
        now_ms: i64,
    ) {
        const POLL_MS: i64 = 250;
        self.apply_media_downloads(ctx, backend);
        // The watched messages only change when the list or a descriptor changes; a scan at
        // least once per poll interval covers a change this widget was not told about.
        let watch = &self.media_watch;
        if watch.dirty
            || watch.scanned_list != Some(self.media_watch_list_key())
            || now_ms - watch.scanned_ms >= POLL_MS
            || now_ms < watch.scanned_ms
        {
            self.scan_media_watch(now_ms);
        }
        if self.media_watch.loading.is_empty() && self.media_watch.pending_rounds.is_empty() {
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(POLL_MS as u64));
        if !self.media_watch.cache_reads
            && now_ms - self.last_media_poll_ms < POLL_MS
            && now_ms >= self.last_media_poll_ms
        {
            return;
        }
        self.last_media_poll_ms = now_ms;
        // The poll changes descriptors, so the next frame scans again.
        self.media_watch.dirty = true;
        let pending_rounds = std::mem::take(&mut self.media_watch.pending_rounds);
        let loading = std::mem::take(&mut self.media_watch.loading);
        for id in pending_rounds {
            let thumb_ready =
                |d: &tg_app_core::core_types::dto::inline_media::InlineMediaDescriptor| {
                    matches!(
                        d.photo_load,
                        Some(
                            tg_app_core::core_types::dto::inline_media::PhotoLoadState::Ready { .. }
                        )
                    )
                };
            let had_thumb = self.media_runtime.descriptor(id).is_some_and(thumb_ready);
            if let Some(desc) = backend.inline_media_descriptor(self.peer_id, id)
                && (desc.video_data.is_some() || (!had_thumb && thumb_ready(&desc)))
            {
                self.media_runtime.register_descriptor(id, desc);
            }
        }
        for id in loading {
            if let Some(state) = backend
                .inline_media_descriptor(self.peer_id, id)
                .and_then(|d| d.photo_load)
            {
                self.media_runtime.set_photo_load_state(id, state);
            }
        }
    }

    /// Loads or reloads a peer's initial slice from `backend`.
    ///
    /// The slice can hold a hundred messages, and asking for a message's inline media reads its
    /// cached file, so only what the audio playlists need is asked for here. The media of the
    /// other messages is asked for from the first frame on, the ones near the viewport first
    /// ([`Self::request_backlog_media`]): every message of the slice still requests (and so
    /// starts) its media, the chat just does not wait for all of them before it paints.
    ///
    /// `around_id` may be [`MsgId::SHOW_AT_END`] (the bottom), [`MsgId::SHOW_AT_UNREAD`] (the
    /// "Unread messages" bar at the top when the chat has unread messages, else the bottom) or
    /// a message. The page is requested without waiting (TGB-2926): until it arrives a new chat
    /// shows an empty list, like tdesktop before its first `messages.getHistory` answers, and
    /// the anchor is applied when the page is there.
    pub fn load_peer(
        &mut self,
        peer_id: PeerId,
        around_id: MsgId,
        backend: &dyn HistoryBackend,
        peer_kind: PeerKind,
    ) {
        let switching = self.peer_id != peer_id;
        match backend.try_load_slice_around(peer_id, around_id, OPEN_PAGE_BEFORE, OPEN_PAGE_AFTER) {
            Loadable::Ready(slice) => {
                self.pending_open = None;
                self.finish_load_peer(peer_id, around_id, slice, backend, peer_kind, switching);
            }
            // A failed first load keeps what is shown, like a page that never arrives.
            Loadable::Failed => {
                if switching {
                    self.apply_slice(peer_id, HistorySlice::default(), peer_kind, false);
                }
                self.pending_open = None;
            }
            Loadable::Loading => {
                if switching {
                    self.apply_slice(peer_id, HistorySlice::default(), peer_kind, false);
                }
                // A reload of the open chat keeps showing what it has until the page is there.
                let switching = switching || self.pending_open.is_some_and(|p| p.switching);
                self.pending_open = Some(PendingOpen {
                    around_id,
                    peer_kind,
                    switching,
                });
            }
        }
    }

    /// `true` while the first page of the open chat is loading (`HistoryWidget::_firstLoadRequest`).
    #[must_use]
    pub const fn is_loading_peer(&self) -> bool {
        self.pending_open.is_some()
    }

    /// Asks again for the page [`Self::load_peer`] is waiting for and applies it once it is
    /// there. Called every frame; the backend answers from its request state without waiting.
    pub fn poll_pending_open(&mut self, backend: &dyn HistoryBackend) {
        let Some(pending) = self.pending_open else {
            return;
        };
        if self.peer_id.is_zero() {
            self.pending_open = None;
            return;
        }
        match backend.try_load_slice_around(
            self.peer_id,
            pending.around_id,
            OPEN_PAGE_BEFORE,
            OPEN_PAGE_AFTER,
        ) {
            Loadable::Ready(slice) => {
                self.pending_open = None;
                self.finish_load_peer(
                    self.peer_id,
                    pending.around_id,
                    slice,
                    backend,
                    pending.peer_kind,
                    pending.switching,
                );
            }
            Loadable::Failed => self.pending_open = None,
            Loadable::Loading => {}
        }
    }

    fn finish_load_peer(
        &mut self,
        peer_id: PeerId,
        around_id: MsgId,
        slice: HistorySlice,
        backend: &dyn HistoryBackend,
        peer_kind: PeerKind,
        switching: bool,
    ) {
        let mut later = HashSet::with_capacity(slice.messages.len());
        for msg in &slice.messages {
            if media_feeds_audio_playlist(msg) {
                self.request_media_descriptor(peer_id, msg.id, backend);
            } else {
                later.insert(msg.id);
            }
        }
        // The first page of a chat opened while it loaded counts as the switch to that chat
        // (its scroll goes to the unread bar or the bottom, `countInitialScrollTop`).
        self.apply_slice_opening(
            peer_id,
            slice,
            peer_kind,
            around_id == MsgId::SHOW_AT_END,
            switching,
        );
        self.media_backlog = later;
    }

    /// Asks `backend` for the inline media of `msg_id` (which also starts its automatic
    /// download, `PhotoMedia::automaticLoad`) and registers what it returns.
    pub fn request_media_descriptor(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        backend: &dyn HistoryBackend,
    ) {
        if let Some(desc) = backend.inline_media_descriptor(peer_id, msg_id) {
            self.media_runtime.register_descriptor(msg_id, desc);
            self.media_watch.dirty = true;
        }
        // Each tile of an album is painted under its own id (`GroupedMedia` parts), so each
        // asks for its own picture.
        let tile_ids: Vec<MsgId> = match self
            .index_of(msg_id)
            .and_then(|i| self.messages[i].media.as_ref())
        {
            Some(HistoryMedia::Album(items)) => items
                .iter()
                .map(|item| item.msg_id)
                .filter(|&id| id != msg_id)
                .collect(),
            _ => return,
        };
        for id in tile_ids {
            if let Some(desc) = backend.inline_media_descriptor(peer_id, id) {
                self.media_runtime.register_descriptor(id, desc);
                self.media_watch.dirty = true;
            }
        }
    }

    /// Asks `backend` for the inline media of `messages`, for the slices that enter the list
    /// after the chat opened: older / newer preloads and jumps (`HistoryView::Photo` creates its
    /// media view when the element is created, whichever slice brought it).
    fn register_media_descriptors(
        &mut self,
        peer_id: PeerId,
        messages: &[HistoryMessage],
        backend: &dyn HistoryBackend,
    ) {
        for msg in messages {
            self.request_media_descriptor(peer_id, msg.id, backend);
        }
    }

    /// Asks for the inline media `load_peer` left for later: every backlog message within one
    /// viewport of the visible range now (before it is painted), then a few more per frame,
    /// walking outward from the viewport, until the whole slice has been asked for.
    fn request_backlog_media(
        &mut self,
        ctx: &egui::Context,
        vis_top: i32,
        vis_bottom: i32,
        backend: &dyn HistoryBackend,
    ) {
        if self.media_backlog.is_empty() {
            return;
        }
        let reach = (vis_bottom - vis_top).max(400);
        let near = self
            .list
            .visible_items_top_to_bottom((vis_top - reach).max(0), vis_bottom + reach);
        let (Some(first), Some(last)) =
            (near.first().map(|v| v.index), near.last().map(|v| v.index))
        else {
            // Nothing is loaded: the backlog is about messages that left the list.
            self.media_backlog.clear();
            return;
        };
        for index in first..=last {
            let Some(id) = self.messages.get(index).map(|m| m.id) else {
                break;
            };
            if self.media_backlog.remove(&id) {
                self.request_media_descriptor(self.peer_id, id, backend);
            }
        }

        let started = Instant::now();
        let mut asked = 0usize;
        let (mut above, mut below) = (first, last + 1);
        let mut take_above = true;
        while !self.media_backlog.is_empty()
            && asked < MEDIA_BACKLOG_MAX_PER_FRAME
            && (asked == 0 || started.elapsed() < MEDIA_BACKLOG_BUDGET)
        {
            let len = self.messages.len();
            let index = if above > 0 && (take_above || below >= len) {
                above -= 1;
                above
            } else if below < len {
                below += 1;
                below - 1
            } else {
                // Walked the whole list: what is left is no longer loaded.
                self.media_backlog.clear();
                break;
            };
            take_above = !take_above;
            let id = self.messages[index].id;
            if self.media_backlog.remove(&id) {
                self.request_media_descriptor(self.peer_id, id, backend);
                asked += 1;
            }
        }
        if !self.media_backlog.is_empty() {
            ctx.request_repaint();
        }
    }

    /// Replaces loaded messages with `slice` and rebuilds `HistoryList` metadata + estimated heights.
    pub fn apply_slice(
        &mut self,
        peer_id: PeerId,
        slice: HistorySlice,
        peer_kind: PeerKind,
        scroll_to_bottom: bool,
    ) {
        let switching = self.peer_id != peer_id;
        self.apply_slice_opening(peer_id, slice, peer_kind, scroll_to_bottom, switching);
    }

    /// [`Self::apply_slice`] that may treat the slice as the switch to `peer_id` even when the
    /// view already shows that (still empty) chat while its first page loaded.
    fn apply_slice_opening(
        &mut self,
        peer_id: PeerId,
        slice: HistorySlice,
        peer_kind: PeerKind,
        scroll_to_bottom: bool,
        switching_peer: bool,
    ) {
        // A new page of the open chat (a reload) keeps the message at the top of the view where
        // it is, like `HistoryWidget` keeps `scrollTopItem` (TGB-2952).
        let reload_anchor = if switching_peer || scroll_to_bottom || self.pending_scroll_to_bottom {
            tg_history::ScrollTopState::default()
        } else {
            self.current_scroll_state()
        };
        self.peer_id = peer_id;
        self.messages = slice.messages;
        self.unread_count = slice.unread_count;
        self.delete_collapse.clear();
        self.layout_cache.clear();
        // A backlog belongs to the slice `load_peer` applied; it sets its own after this call.
        self.media_backlog.clear();
        self.media_watch.dirty = true;
        if switching_peer {
            // `Data::Session::hideShownSpoilers`: another chat hides revealed spoilers again.
            self.text_states.clear();
            self.list.selection.clear();
            self.highlighter.clear();
            self.context_menu.close();
            self.context_menu_msg = None;
            self.context_menu_engagement = None;
            self.scroll_offset_y = 0.0;
            self.emoji_interactions.clear();
            self.last_bubble_anchors.clear();
            self.last_inbox_read_sent = MsgId(0);
            self.read_metrics =
                Some(tg_app_core::core_types::read_metrics::ReadMetricsTracker::new(peer_id));
        }

        if slice.unread_count == 0 {
            if let Some(max_loaded) = self
                .messages
                .iter()
                .map(|m| m.id)
                .filter(|id| id.0 > 0)
                .max()
            {
                self.last_inbox_read_sent = self.last_inbox_read_sent.max(max_loaded);
            }
        } else if let Some(first_unread) = slice.first_unread_id
            && first_unread.0 > 1
        {
            self.last_inbox_read_sent = self.last_inbox_read_sent.max(MsgId(first_unread.0 - 1));
        }

        let is_group = matches!(peer_kind, PeerKind::Group);
        let items: Vec<HistoryListItem> = self
            .messages
            .iter()
            .map(|m| build_list_item(m, is_group, self.large_emoji, self.layout_wide))
            .collect();

        self.list.loaded_at_top = slice.loaded_at_top;
        self.list.loaded_at_bottom = slice.loaded_at_bottom;
        self.list.set_items(items);

        let unread_idx = slice.first_unread_id.and_then(|uid| self.index_of(uid));
        self.list.set_unread_bar(unread_idx);
        self.update_date_and_unread_heights();
        self.media_runtime
            .sync_peer_audio_tracks(peer_id, &self.messages);

        // `HistoryWidget::countInitialScrollTop`: a chat opened at its unread messages starts at
        // the "Unread messages" bar; every other open starts at the bottom.
        self.pending_scroll_to_unread_bar =
            switching_peer && !scroll_to_bottom && unread_idx.is_some();
        if scroll_to_bottom || (switching_peer && !self.pending_scroll_to_unread_bar) {
            self.pending_scroll_to_bottom = true;
        } else if self.pending_scroll_to_unread_bar {
            self.pending_scroll_to_bottom = false;
        } else {
            self.restore_scroll_state(reload_anchor);
        }
    }

    /// The message at the top of the view, for a reload that should keep it in place.
    #[must_use]
    pub fn top_visible_message(&self) -> Option<MsgId> {
        let state = self.current_scroll_state();
        tg_history::is_message_position_valid(state.item)
            .then_some(MsgId(state.item.full_id.msg.0 as i32))
    }

    /// Removes the `"Unread messages"` divider bar and invalidates the cached layout of the item
    /// that carried it, matching tdesktop's `History::destroyUnreadBar`.
    fn clear_unread_bar(&mut self) {
        if let Some(bar_idx) = self.list.unread_bar_index {
            if let Some(msg) = self.messages.get(bar_idx) {
                self.layout_cache.remove(&msg.id);
            }
            self.list.set_unread_bar(None);
            self.update_date_and_unread_heights();
        }
    }

    /// Appends a newly sent/received message to the bottom if loaded at bottom.
    pub fn append_message(&mut self, msg: HistoryMessage, peer_kind: PeerKind) {
        if !self.list.loaded_at_bottom && !self.messages.is_empty() {
            return;
        }
        if self.index_of(msg.id).is_some() {
            self.update_message_in_place(msg);
            return;
        }
        if msg.outgoing {
            // Matches tdesktop `History::addNewMessage` (`history.cpp:1708`).
            self.clear_unread_bar();
        }
        self.list.loaded_at_bottom = true;
        if let Some(prev_last) = self.messages.last() {
            self.layout_cache.remove(&prev_last.id);
        }
        let is_group = matches!(peer_kind, PeerKind::Group);
        let item = build_list_item(&msg, is_group, self.large_emoji, self.layout_wide);
        self.messages.push(msg);
        self.list.push_back(item);
        self.update_date_and_unread_heights();
        self.media_runtime
            .sync_peer_audio_tracks(self.peer_id, &self.messages);
        self.pending_scroll_to_bottom = true;
    }

    /// Applies a live-arrived message (`updateNewMessage` / `updateNewChannelMessage` /
    /// `updateShortMessage` / `updateShortChatMessage`) into the already-open history list,
    /// matching tdesktop's `History::addNewMessage` + `HistoryWidget::newItemAdded`.
    ///
    /// Deduplicates existing IDs in place, keeps the user's scroll position when scrolled up,
    /// and auto-scrolls to bottom only when already at bottom (or for outgoing messages).
    /// Returns `true` when the message is in the list afterwards.
    pub fn apply_live_added_message(&mut self, msg: HistoryMessage, peer_kind: PeerKind) -> bool {
        if self.index_of(msg.id).is_some() {
            self.update_message_in_place(msg);
            return true;
        }
        let is_outgoing = msg.outgoing;
        let is_unread_incoming = (!msg.outgoing || msg.is_post) && !msg.is_read;

        // If viewing an older jumped-to slice not loaded at bottom, do not splice a gap into
        // the middle of the list; only update the unread badge counter on the bottom FAB.
        if !self.list.loaded_at_bottom && !self.messages.is_empty() {
            if is_unread_incoming {
                self.unread_count = self.unread_count.saturating_add(1);
            }
            return false;
        }

        self.list.loaded_at_bottom = true;
        let was_at_bottom = !self.scrolled_up;
        let is_group = matches!(peer_kind, PeerKind::Group);
        // A local message (being sent) always goes to the bottom, like any message newer than
        // the last one; tdesktop keeps local messages after the server ones they follow.
        let goes_last = msg.id.is_local()
            || self
                .messages
                .iter()
                .rev()
                .find(|m| m.id.0 > 0)
                .is_none_or(|last_server| msg.id > last_server.id);
        // The session stores a history page through `Session::insert_message`, which announces
        // each of its messages as a new one. A message older than the loaded range belongs to
        // such a page: the preload hands the page out and prepends it with the scroll anchor
        // kept (`HistoryWidget::addMessagesToFront`). Splicing it in here would push the view
        // down by the page's height and leave a gap in the loaded range (TGB-2952).
        if !goes_last
            && !self.list.loaded_at_top
            && self
                .messages
                .iter()
                .find(|m| m.id.0 > 0)
                .is_some_and(|first_server| msg.id < first_server.id)
        {
            return false;
        }
        // A message that lands above the view keeps what is on screen in place
        // (`ListWidget::rememberScrollAnchor` / `HistoryWidget::updateHistoryGeometry`).
        let keep_anchor = !goes_last && !was_at_bottom && !is_outgoing;
        let anchor = if keep_anchor {
            self.current_scroll_state()
        } else {
            tg_history::ScrollTopState::default()
        };
        let inserted_idx = if goes_last {
            if let Some(prev_last) = self.messages.last() {
                self.layout_cache.remove(&prev_last.id);
            }
            let item = build_list_item(&msg, is_group, self.large_emoji, self.layout_wide);
            let idx = self.messages.len();
            self.messages.push(msg);
            self.list.push_back(item);
            idx
        } else {
            // Before the first newer server message; local messages and drafts at the bottom
            // are not in id order, so this is a scan rather than a binary search.
            let idx = self
                .messages
                .iter()
                .position(|m| m.id.0 > 0 && m.id > msg.id)
                .unwrap_or(self.messages.len());
            if idx > 0 {
                self.layout_cache.remove(&self.messages[idx - 1].id);
            }
            if let Some(next_msg) = self.messages.get(idx) {
                self.layout_cache.remove(&next_msg.id);
            }
            let prev_unread_bar = self
                .list
                .unread_bar_index
                .map(|u| if u >= idx { u + 1 } else { u });
            // The other items keep their measured heights; only the new one is estimated.
            let mut items = self.list.items().to_vec();
            items.insert(
                idx,
                build_list_item(&msg, is_group, self.large_emoji, self.layout_wide),
            );
            self.messages.insert(idx, msg);
            self.list.set_items(items);
            self.list.set_unread_bar(prev_unread_bar);
            idx
        };

        if is_outgoing || (was_at_bottom && is_unread_incoming) {
            // Matches tdesktop `History::addNewMessage` (`history.cpp:1708`) and
            // `HistoryWidget::newItemAdded` (`history_widget.cpp:4726`).
            self.clear_unread_bar();
        } else if !was_at_bottom && is_unread_incoming && self.list.unread_bar_index.is_none() {
            self.list.set_unread_bar(Some(inserted_idx));
        }
        // Each unread arrival counts (`History::addNewMessage`, `history.cpp:1717-1723`); the
        // session's own count replaces this one with the `UnreadChanged` that follows.
        if is_unread_incoming {
            self.unread_count = self.unread_count.saturating_add(1);
        }
        if (was_at_bottom || is_outgoing) && self.messages.len() > MAX_LOADED_AT_BOTTOM {
            self.unload_oldest_messages(KEEP_LOADED_AFTER_UNLOAD);
        }

        self.update_date_and_unread_heights();
        self.media_runtime
            .sync_peer_audio_tracks(self.peer_id, &self.messages);

        if was_at_bottom || is_outgoing {
            self.pending_scroll_to_bottom = true;
        } else if keep_anchor {
            self.restore_scroll_state(anchor);
        }
        true
    }

    /// Where the next frame starts drawing: a scroll target already asked for, else the
    /// offset the last frame ended with.
    fn effective_scroll_top(&self) -> f32 {
        self.pending_scroll_y.unwrap_or(self.scroll_offset_y)
    }

    /// The message at the top of the view and how far into it the view starts
    /// (`ListWidget::countScrollState`); empty at the bottom of a chat loaded to its end.
    fn current_scroll_state(&self) -> tg_history::ScrollTopState {
        let top = self.effective_scroll_top();
        let vis_top = top.round() as i32;
        let vis_bot = (top + self.last_viewport_h).round() as i32;
        self.list.count_scroll_state(vis_top, vis_bot)
    }

    /// The anchor for rows measured this frame: the first row that starts at or below the top
    /// of the view. The row cut by the top edge may itself have just been measured (an
    /// estimate replaced by its real height); anchoring on it would keep its top still and
    /// push everything on screen down by its growth. Uses the positions from before the
    /// measured heights are laid out.
    fn anchor_below_top(&self, vis_top: i32, vis_bottom: i32) -> tg_history::ScrollTopState {
        let items = self.list.items();
        let items_top = self.list.items_top();
        let index = items.partition_point(|item| items_top + item.y < vis_top);
        match items.get(index) {
            Some(item) if items_top + item.y < vis_bottom && !item.attach_meta.is_sponsored => {
                tg_history::ScrollTopState {
                    item: item.position,
                    shift: vis_top - (items_top + item.y),
                }
            }
            _ => self.list.count_scroll_state(vis_top, vis_bottom),
        }
    }

    /// Scrolls so the message of `state` is where it was (`ListWidget::restoreScrollState`).
    /// Returns `false` when `state` names no message or the message is gone.
    fn restore_scroll_state(&mut self, state: tg_history::ScrollTopState) -> bool {
        if !tg_history::is_message_position_valid(state.item) {
            return false;
        }
        self.list.scroll_top_state = state;
        let Some(restored) = self.list.restore_scroll_top() else {
            return false;
        };
        self.scroll_offset_y = restored as f32;
        self.pending_scroll_y = Some(restored as f32);
        true
    }

    /// Drops the oldest loaded messages down to the newest `keep` while the chat sits at the
    /// bottom, so a busy chat left open, or one scrolled far up and brought back down, holds
    /// about what it held when it was opened and the scroll bar is sized from that (owner,
    /// TGB-2952; tdesktop drops a history's blocks with `History::clear(ClearType::Unload)`).
    /// Scrolling up pages them in again from the backend. The kept messages keep their
    /// measured heights.
    fn unload_oldest_messages(&mut self, keep: usize) {
        let Some(drop_count) = self.messages.len().checked_sub(keep) else {
            return;
        };
        if drop_count == 0 {
            return;
        }
        let dropped_height = self.list.items().get(drop_count).map_or(0, |item| item.y);
        self.delete_collapse.shift(-dropped_height);
        let dropped: HashSet<MsgId> = self.messages.drain(..drop_count).map(|m| m.id).collect();
        for id in &dropped {
            self.layout_cache.remove(id);
            self.last_painted_bubble_rects.remove(id);
            self.media_backlog.remove(id);
        }
        // The new first message may now show its date above it.
        if let Some(first) = self.messages.first() {
            self.layout_cache.remove(&first.id);
        }
        let unread_bar = self
            .list
            .unread_bar_index
            .and_then(|u| u.checked_sub(drop_count));
        let items = self.list.items()[drop_count..].to_vec();
        self.list.set_items(items);
        self.list.set_unread_bar(unread_bar);
        self.list
            .selection
            .selected_items
            .retain(|id| !dropped.contains(&MsgId(id.msg.0 as i32)));
        self.list.loaded_at_top = false;
        self.list.scroll_top_state = tg_history::ScrollTopState::default();
        self.update_date_and_unread_heights();
        self.media_runtime
            .sync_peer_audio_tracks(self.peer_id, &self.messages);
    }

    /// Marks loaded outgoing messages up to `max_id` as read (`updateReadHistoryOutbox` /
    /// `updateReadChannelOutbox`), refreshing their checkmark glyphs without re-opening.
    pub fn mark_outbox_read_up_to(&mut self, max_id: MsgId) {
        for msg in &mut self.messages {
            if msg.outgoing && msg.id.0 > 0 && msg.id <= max_id && !msg.is_read {
                msg.is_read = true;
                self.layout_cache.remove(&msg.id);
            }
        }
    }

    /// Sets whether the user can see the chat (`MainWindow::markingAsRead`, computed by the window
    /// with [`crate::history::marking_read::window_marking_as_read`]). While `false`, shown
    /// messages and reactions are not marked read.
    pub fn set_marking_as_read(&mut self, marking: bool) {
        self.marking_as_read = marking;
    }

    /// Tells the widget that a live-host overlay covers the chat, so nothing is marked read.
    pub fn set_host_overlay_open(&mut self, open: bool) {
        self.host_overlay_open = open;
    }

    /// Returns `true` when shown messages may be marked read this frame (focus excluded).
    #[must_use]
    pub const fn is_marking_as_read(&self) -> bool {
        self.marking_as_read && !self.host_overlay_open
    }

    /// Synchronizes unread counter and inbox read watermark from a live `HistoryUpdated` event.
    ///
    /// Matches tdesktop's `History::inboxRead` (`history.cpp:2286-2304`): updating the unread
    /// counter does not destroy an existing `"Unread messages"` divider bar in an open chat.
    pub fn apply_unread_state(&mut self, unread_count: u32, inbox_read_till: MsgId) {
        self.unread_count = unread_count;
        if inbox_read_till.0 > 0 {
            self.last_inbox_read_sent = self.last_inbox_read_sent.max(inbox_read_till);
        }
    }

    /// Puts the sent version of local message `local_id` in its place (the position, height and
    /// unread bar stay), like tdesktop's `HistoryItem::setRealId`. When the sent message is
    /// already listed (a reload brought it), the local one is removed instead. Returns `true`
    /// when the local message was listed.
    pub fn replace_local_message(
        &mut self,
        local_id: MsgId,
        sent: HistoryMessage,
        peer_kind: PeerKind,
    ) -> bool {
        let Some(index) = self.index_of(local_id) else {
            self.update_message_in_place(sent);
            return false;
        };
        if sent.id != local_id && self.index_of(sent.id).is_some() {
            self.remove_messages(&[local_id], peer_kind);
            self.update_message_in_place(sent);
            return true;
        }
        self.layout_cache.remove(&local_id);
        self.media_backlog.remove(&local_id);
        self.media_watch.dirty = true;
        let is_group = matches!(peer_kind, PeerKind::Group);
        let fresh = build_list_item(&sent, is_group, self.large_emoji, self.layout_wide);
        if let Some(slot) = self.list.items_mut().get_mut(index) {
            *slot = HistoryListItem {
                y: slot.y,
                height: slot.height,
                has_unread_bar: slot.has_unread_bar,
                displayed_date_height: slot.displayed_date_height,
                ..fresh
            };
        }
        if let Some(anchor) = self.last_bubble_anchors.remove(&local_id) {
            self.last_bubble_anchors.insert(sent.id, anchor);
        }
        let local_full = to_hist_full(FullMsgId {
            peer: self.peer_id,
            msg: local_id,
        });
        let sent_full = to_hist_full(FullMsgId {
            peer: self.peer_id,
            msg: sent.id,
        });
        for selected in &mut self.list.selection.selected_items {
            if *selected == local_full {
                *selected = sent_full;
            }
        }
        self.messages[index] = sent;
        self.list.recount_attachments();
        self.list.relayout_positions();
        true
    }

    /// Shows local message `local_id` as failed (`HistoryItem::sendFailed`): it keeps the clock
    /// (`BottomInfo`, `Flag::Sending`) and stays in the list.
    pub fn mark_local_message_failed(&mut self, local_id: MsgId) -> bool {
        let Some(index) = self.index_of(local_id) else {
            return false;
        };
        let msg = &mut self.messages[index];
        msg.is_sending = true;
        msg.extra.send_failed = true;
        self.layout_cache.remove(&local_id);
        true
    }

    /// Updates an edited or reaction-changed message in place and invalidates its cached layout.
    pub fn update_message_in_place(&mut self, updated: HistoryMessage) {
        if let Some(index) = self.index_of(updated.id) {
            self.layout_cache.remove(&updated.id);
            self.media_watch.dirty = true;
            self.messages[index] = updated;
        }
    }

    /// Removes deleted messages and recomputes attachment/positions.
    pub fn remove_messages(&mut self, msg_ids: &[MsgId], peer_kind: PeerKind) {
        let before_len = self.messages.len();
        self.messages.retain(|m| !msg_ids.contains(&m.id));
        if self.messages.len() == before_len {
            return;
        }
        for id in msg_ids {
            self.layout_cache.remove(id);
            self.media_backlog.remove(id);
        }
        let is_group = matches!(peer_kind, PeerKind::Group);
        let items: Vec<HistoryListItem> = self
            .messages
            .iter()
            .map(|m| build_list_item(m, is_group, self.large_emoji, self.layout_wide))
            .collect();
        self.list.set_items(items);
        self.list
            .selection
            .selected_items
            .retain(|id| !msg_ids.contains(&MsgId(id.msg.0 as i32)));
        self.update_date_and_unread_heights();
    }

    /// Removes deleted messages leaving holes that close smoothly (TGB-3011,
    /// `ThanosEffectController::captureOnRemoval` and `flushRemovals`). Regular messages not
    /// below the view leave a hole; local, service and lower ones go at once.
    pub fn remove_messages_collapsing(
        &mut self,
        msg_ids: &[MsgId],
        peer_kind: PeerKind,
        now_ms: f64,
    ) {
        let view_top = self.effective_scroll_top();
        let view_bottom = view_top + self.last_viewport_h;
        let removals: Vec<Removal> = self
            .messages
            .iter()
            .enumerate()
            .filter(|(_, m)| msg_ids.contains(&m.id))
            .filter_map(|(index, m)| {
                let item = self.list.items().get(index)?;
                let below = self.list.visual_item_top(index) as f32 >= view_bottom;
                Some(Removal {
                    top: item.y,
                    height: item.height,
                    animated: m.id.0 > 0 && !m.is_service() && !below,
                })
            })
            .collect();
        if removals.is_empty() {
            return;
        }
        if removals.iter().any(|r| r.animated) {
            self.delete_collapse
                .note_scroll_baseline(view_top, self.is_at_history_bottom());
        }
        let before = self.list.items_height();
        self.remove_messages(msg_ids, peer_kind);
        let left = before - self.list.items_height();
        self.delete_collapse.flush(&removals, left, now_ms);
        self.list
            .set_collapse_gaps(self.delete_collapse.render_gaps());
    }

    /// Corner radii of message `msg_id`'s bubble as laid out (square for media without one).
    #[must_use]
    pub fn bubble_corners(&self, msg_id: MsgId) -> CornerRadius {
        match self.layout_cache.get(&msg_id).map(|cached| &cached.layout) {
            Some(CachedItemLayout::Message(layout)) if layout.has_bubble => {
                crate::history::media::bubble_rounding_to_egui(layout.bubble_rounding)
            }
            _ => CornerRadius::ZERO,
        }
    }

    /// Loads the slice before the first loaded message (`HistoryList::check_preload` top) and
    /// prepends it, starting the media of its messages.
    pub fn load_older_slice(&mut self, backend: &dyn HistoryBackend, peer_kind: PeerKind) {
        let Some(first) = self.messages.first() else {
            return;
        };
        // While the page is requested the list stays as it is; the preload check asks again on
        // a later frame (the backend wakes the UI when the page lands).
        let Some(older) = backend
            .try_load_slice_before(self.peer_id, first.id, 40)
            .ready()
        else {
            return;
        };
        self.register_media_descriptors(self.peer_id, &older.messages, backend);
        self.prepend_older_slice(older, peer_kind);
    }

    /// Loads the slice after the last loaded message (`HistoryList::check_preload` bottom) and
    /// appends it, starting the media of its messages.
    pub fn load_newer_slice(&mut self, backend: &dyn HistoryBackend, peer_kind: PeerKind) {
        let Some(last) = self.messages.last() else {
            return;
        };
        let Some(newer) = backend
            .try_load_slice_after(self.peer_id, last.id, 40)
            .ready()
        else {
            return;
        };
        self.register_media_descriptors(self.peer_id, &newer.messages, backend);
        self.append_newer_slice(newer, peer_kind);
    }

    /// Prepends older messages while keeping the user's viewport anchored to the exact same message!
    pub fn prepend_older_slice(&mut self, older: HistorySlice, peer_kind: PeerKind) {
        if older.messages.is_empty() {
            self.list.loaded_at_top = true;
            return;
        }
        let vis_bot = (self.effective_scroll_top() + self.last_viewport_h).round() as i32;
        let was_at_bottom = self.is_at_loaded_bottom(vis_bot);
        let anchor = self.current_scroll_state();

        let is_group = matches!(peer_kind, PeerKind::Group);
        let older_items: Vec<HistoryListItem> = older
            .messages
            .iter()
            .map(|m| build_list_item(m, is_group, self.large_emoji, self.layout_wide))
            .collect();

        if let Some(prev_first) = self.messages.first() {
            self.layout_cache.remove(&prev_first.id);
        }
        let mut merged = older.messages;
        merged.append(&mut self.messages);
        self.messages = merged;
        self.list.loaded_at_top = older.loaded_at_top;
        let added = older_items.len();
        self.list.prepend_older(older_items);
        self.update_date_and_unread_heights();
        self.delete_collapse
            .shift(self.list.items().get(added).map_or(0, |item| item.y));

        // A view at the newest message stays there while older messages fill in above
        // (`HistoryWidget::updateHistoryGeometry` keeps `wasAtBottom` at the new bottom); a chat
        // opened with only its top message loaded otherwise drifts up as each page arrives.
        if was_at_bottom {
            self.pending_scroll_to_bottom = true;
        } else {
            self.restore_scroll_state(anchor);
        }
    }

    /// Lifts the painted rows by `px` above their place: the list stretched past its bottom
    /// (`Ui::ElasticScroll` real overscroll plus `setContentBottomInset`). `0.0` at rest.
    pub fn set_bottom_stretch(&mut self, px: f32) {
        self.bottom_stretch = px.max(0.0);
    }

    /// `true` when the last frame showed the very end of a history loaded to its newest message
    /// (`scrollTop() >= scrollTopMax()` with `History::loadedAtBottom`).
    #[must_use]
    pub fn is_at_history_bottom(&self) -> bool {
        let m = self.last_scroll_metrics;
        self.list.loaded_at_bottom
            && m.viewport_height > 0.0
            && m.offset + m.viewport_height + 2.0 >= m.content_height
    }

    /// `true` when `vis_bottom` reaches the end of a list that ends at the newest message.
    fn is_at_loaded_bottom(&self, vis_bottom: i32) -> bool {
        self.list.loaded_at_bottom && vis_bottom + 2 >= self.list.total_height()
    }

    /// Appends newer messages when scrolling downward in a jumped-to historical slice.
    pub fn append_newer_slice(&mut self, newer: HistorySlice, peer_kind: PeerKind) {
        if newer.messages.is_empty() {
            self.list.loaded_at_bottom = true;
            return;
        }
        if let Some(prev_last) = self.messages.last() {
            self.layout_cache.remove(&prev_last.id);
        }
        let is_group = matches!(peer_kind, PeerKind::Group);
        for msg in newer.messages {
            let item = build_list_item(&msg, is_group, self.large_emoji, self.layout_wide);
            self.messages.push(msg);
            self.list.push_back(item);
        }
        self.list.loaded_at_bottom = newer.loaded_at_bottom;
        self.update_date_and_unread_heights();
    }

    /// The plain clipboard text a copy of the current selection produces (empty when nothing is
    /// selected): see [`Self::selection_copy`].
    #[must_use]
    pub fn extract_selected_text(&self) -> String {
        self.selection_copy()
            .map(|copy| copy.clipboard)
            .unwrap_or_default()
    }

    /// Headless visible-range layout helper (also used by unit tests to verify 5,000+ message
    /// virtualization without requiring an active GPU/egui context).
    pub fn layout_visible_range_headless(
        &mut self,
        viewport_width: i32,
        visible_top: i32,
        visible_bottom: i32,
        is_chat_wide: bool,
        peer_kind: PeerKind,
    ) -> usize {
        if self.layout_width != viewport_width || self.layout_wide != is_chat_wide {
            self.layout_cache.clear();
            self.layout_width = viewport_width;
            if self.layout_wide != is_chat_wide {
                self.layout_wide = is_chat_wide;
                self.refresh_from_photos(matches!(peer_kind, PeerKind::Group));
            }
        }
        let fonts = FontMetrics::default();
        let overscan = ((visible_bottom - visible_top) / 2).max(200);
        let vis = self.list.visible_items_top_to_bottom(
            (visible_top - overscan).max(0),
            visible_bottom + overscan,
        );
        let mut laid_out = 0usize;
        let mut heights_changed = false;
        let show_sender_in_chat = matches!(peer_kind, PeerKind::Group);

        for v in &vis {
            let msg = &self.messages[v.index];
            if self.layout_cache.contains_key(&msg.id) {
                continue;
            }
            let item = &self.list.items()[v.index];
            let cached = compute_item_layout_headless(
                msg,
                item,
                viewport_width,
                is_chat_wide,
                show_sender_in_chat,
                &fonts,
                self.large_emoji,
            );
            let new_h = cached_layout_height(&cached);
            if self.list.items()[v.index].height != new_h {
                self.list.items_mut()[v.index].height = new_h;
                heights_changed = true;
            }
            self.layout_cache.insert(msg.id, CachedItem::new(cached));
            laid_out += 1;
        }
        if heights_changed {
            self.list.relayout_positions();
        }
        self.last_frame_laid_out_count = laid_out;
        laid_out
    }

    /// Renders the scrollable virtualized message list inside `list_rect`.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        list_rect: Rect,
        peer: &HistoryPeerInfo,
        is_chat_wide: bool,
        search_filter: &str,
        colors: &HistoryColors,
        backend: &mut dyn HistoryBackend,
    ) -> Vec<InnerEvent> {
        let fallback_pal = Palette::for_mode(if colors.is_night {
            ThemeMode::Night
        } else {
            ThemeMode::Day
        });
        self.show_with_bots(
            ui,
            list_rect,
            peer,
            is_chat_wide,
            search_filter,
            colors,
            &fallback_pal,
            None,
            false,
            backend,
        )
    }

    /// Renders the scrollable virtualized message list with bot inline-keyboard context.
    #[allow(clippy::too_many_arguments)]
    pub fn show_with_bots(
        &mut self,
        ui: &mut Ui,
        list_rect: Rect,
        peer: &HistoryPeerInfo,
        is_chat_wide: bool,
        search_filter: &str,
        colors: &HistoryColors,
        palette: &Palette,
        bot_id: Option<tg_bots::stand_in::UserId>,
        bot_verified: bool,
        backend: &mut dyn HistoryBackend,
    ) -> Vec<InnerEvent> {
        let wallpaper_colors = colors_for_default_wallpaper(colors);
        let colors = &wallpaper_colors;
        let mut events = Vec::new();
        let fonts = FontMetrics::default();
        let viewport_w = list_rect.width().round().max(240.0) as i32;
        let viewport_h = list_rect.height().max(80.0);
        self.last_viewport_h = viewport_h;

        let now_ms = (ui.ctx().input(|i| i.time) * 1000.0).max(0.0) as i64;
        self.last_now_ms = now_ms;
        self.poll_pending_open(&*backend);
        self.refresh_loading_media(ui.ctx(), &*backend, now_ms);
        self.assets.reply_previews.poll(ui.ctx(), &*backend, now_ms);
        self.highlighter.tick(now_ms);
        if self.highlighter.is_animating(now_ms) || self.inline_kb_state.loading_count() > 0 {
            ui.ctx().request_repaint();
        }

        // `pausedSpoiler`: the spoiler particles stand still while the window is inactive or
        // something covers the chat (TGB-3029).
        if !self.is_marking_as_read() {
            tg_ui::effects::pause_text_spoilers_this_pass(ui.ctx());
        }

        // Copy shortcut for the text/multi selection (the backend sends `Event::Copy`, not a
        // `C` key, TGB-3016), Escape to clear selection.
        if !self.list.selection.is_empty() {
            if ui.ctx().input(|i| i.key_pressed(Key::Escape)) {
                self.list.selection.clear();
            } else if ui.ctx().input(tg_ui::clipboard::copy_requested)
                && let Some(toast) = self.selection_copy_restriction(
                    backend.peer_allows_forwarding(self.peer_id),
                    peer.summary.kind,
                )
            {
                events.push(InnerEvent::CopyRestricted(toast));
            } else if ui.ctx().input(tg_ui::clipboard::copy_requested)
                && let Some(copy) = self.selection_copy()
            {
                ui.ctx().copy_text(copy.clipboard);
                events.push(InnerEvent::CopiedRich {
                    summary: String::new(),
                    rich: copy.rich,
                });
            }
        }

        // Invalidate layout cache if column width or wide-mode changed.
        if self.layout_width != viewport_w || self.layout_wide != is_chat_wide {
            if self.layout_width > 0
                && !self.pending_scroll_to_bottom
                && self.pending_scroll_y.is_none()
            {
                let vis_top = self.scroll_offset_y.round() as i32;
                let vis_bot = (self.scroll_offset_y + viewport_h).round() as i32;
                self.list.scroll_top_state = tg_history::ScrollTopState::default();
                self.list.save_scroll_state(vis_top, vis_bot);
            }
            self.layout_cache.clear();
            self.layout_width = viewport_w;
            if self.layout_wide != is_chat_wide {
                self.layout_wide = is_chat_wide;
                self.refresh_from_photos(matches!(peer.summary.kind, PeerKind::Group));
            }
        }

        // Active chat background (Classic wallpaper by default, flat Night background).
        self.paint_chat_wallpaper(ui, list_rect, colors);

        // Holes of deleted messages closing (TGB-3011). They count in the list's height, so a
        // short chat stays bottom-aligned while they close.
        if let Some(scroll) = self.delete_collapse.tick(now_ms as f64) {
            match scroll {
                CollapseScroll::Bottom => self.pending_scroll_to_bottom = true,
                CollapseScroll::Top(y) => self.pending_scroll_y = Some(y),
            }
            ui.ctx().request_repaint();
        }
        self.list
            .set_collapse_gaps(self.delete_collapse.render_gaps());
        let content_pad_bottom = 12 + self.list.collapse_gap_total();
        let mut laid_out_this_frame = 0usize;

        // When pinning to the bottom (initial load, peer switch, or newly appended messages),
        // eagerly lay out the bottom visible window before entering `ScrollArea` so `items_top`,
        // `total_height()`, and `max_scroll` already reflect exact item heights on the very first
        // frame instead of overshooting `ScrollArea`'s previous-frame `max_offset`.
        if self.pending_scroll_to_bottom && !self.messages.is_empty() {
            self.voice_state.transcribe_limits = backend.transcribe_limits(Some(self.peer_id));
            let overscan = 160;
            for _ in 0..4 {
                let total_items_h = self.list.items_height();
                let items_top = ((viewport_h as i32) - total_items_h - content_pad_bottom).max(0);
                self.list.set_items_top(items_top);
                let vis_bottom = self.list.total_height();
                let vis_top = (vis_bottom - viewport_h.ceil() as i32).max(0);
                let visible = self.list.visible_items_top_to_bottom(
                    (vis_top - overscan).max(0),
                    vis_bottom + overscan,
                );
                let mut changed_any = false;
                for v in &visible {
                    let msg_id = self.messages[v.index].id;
                    self.sync_voice_transcription_for(v.index, backend);
                    self.sync_summary_state_for(v.index, backend);
                    if !self.layout_cache.contains_key(&msg_id) {
                        let fc_extra_h = self.factcheck_extra_height(msg_id, backend);
                        let summary_entry = backend.message_summary(self.peer_id, msg_id);
                        if self.ensure_item_laid_out(
                            ui,
                            v.index,
                            viewport_w,
                            is_chat_wide,
                            peer,
                            colors,
                            &fonts,
                            fc_extra_h,
                            summary_entry.as_ref(),
                        ) {
                            changed_any = true;
                        }
                        laid_out_this_frame += 1;
                    }
                }
                if changed_any {
                    self.list.relayout_positions();
                } else {
                    break;
                }
            }
        }

        // Compute bottom alignment offset when content is shorter than the viewport.
        let total_items_h = self.list.items_height();
        let items_top = ((viewport_h as i32) - total_items_h - content_pad_bottom).max(0);
        self.list.set_items_top(items_top);

        // Resolve pending jump-to-message scroll target.
        if let Some(target_id) = self.pending_jump_msg.take()
            && let Some(idx) = self.index_of(target_id)
        {
            // Ensure target item is laid out so centering is exact.
            let fc_extra_h = self.factcheck_extra_height(target_id, backend);
            let summary_entry = backend.message_summary(self.peer_id, target_id);
            if self.ensure_item_laid_out(
                ui,
                idx,
                viewport_w,
                is_chat_wide,
                peer,
                colors,
                &fonts,
                fc_extra_h,
                summary_entry.as_ref(),
            ) {
                self.list.relayout_positions();
                let items_top =
                    ((viewport_h as i32) - self.list.items_height() - content_pad_bottom).max(0);
                self.list.set_items_top(items_top);
            }
            if let Some(y) = self
                .list
                .scroll_top_for_index(idx, 0, viewport_h.round() as i32)
            {
                self.pending_scroll_y = Some(y as f32);
            }
        }

        // `unreadBarTop`: lay out the bar's item and one screen below it, then put the bar at the
        // top of the view (`ScrollArea` clamps it when the rest is shorter than the viewport).
        if self.pending_scroll_to_unread_bar {
            self.pending_scroll_to_unread_bar = false;
            if let Some(idx) = self
                .list
                .unread_bar_index
                .filter(|&i| i < self.messages.len())
            {
                let mut changed = false;
                let mut filled = 0;
                let mut i = idx;
                while i < self.messages.len() && filled < viewport_h.ceil() as i32 {
                    let id = self.messages[i].id;
                    let fc_extra_h = self.factcheck_extra_height(id, backend);
                    let summary_entry = backend.message_summary(self.peer_id, id);
                    changed |= self.ensure_item_laid_out(
                        ui,
                        i,
                        viewport_w,
                        is_chat_wide,
                        peer,
                        colors,
                        &fonts,
                        fc_extra_h,
                        summary_entry.as_ref(),
                    );
                    filled += self.list.items()[i].height;
                    i += 1;
                }
                if changed {
                    self.list.relayout_positions();
                    let items_top =
                        ((viewport_h as i32) - self.list.items_height() - content_pad_bottom)
                            .max(0);
                    self.list.set_items_top(items_top);
                }
                self.pending_scroll_y = Some(unread_bar_scroll_top(&self.list, idx) as f32);
            }
        }

        // Resolve pending scroll-to-bottom target.
        let mut pinning_to_bottom = false;
        if self.pending_scroll_to_bottom {
            let max_scroll = (self.list.total_height() as f32 - viewport_h).max(0.0);
            self.pending_scroll_y = Some(max_scroll);
            self.pending_scroll_to_bottom = false;
            pinning_to_bottom = true;
        }

        let scroll_salt = ("tg_history_inner_scroll", self.peer_id.0);
        let mut scroll_area = ScrollArea::vertical()
            .id_salt(scroll_salt)
            .auto_shrink([false, false]);
        let explicit_scroll_y = self.pending_scroll_y.take().map(|y| y.max(0.0));
        if let Some(target_y) = explicit_scroll_y {
            scroll_area = scroll_area.vertical_scroll_offset(target_y);
        }

        let mut painted_this_frame = 0usize;
        let mut open_context_menu_for: Option<(MsgId, Pos2)> = None;
        let mut jump_to_msg: Option<MsgId> = None;
        let search_q = search_filter.trim().to_lowercase();
        let show_selection_checks = self.list.selection.has_selected_items();
        let show_sender_in_chat = matches!(peer.summary.kind, PeerKind::Group);
        // The own name comes from the backend: it is read again when the chat's version (or the
        // peer shown) changes, not on every frame. The kept value is moved out of `self` for
        // the frame so the paint loop can borrow it, and put back after the list is painted.
        let mut reaction_userpics_slot = self.reaction_userpics.take();
        let reaction_userpics: Option<&ReactionUserpics> =
            if matches!(peer.summary.kind, PeerKind::User) && !peer.summary.is_saved_messages {
                let generation = self.read_gate.begin_frame(
                    ui.ctx(),
                    self.peer_id,
                    backend.chat_version(self.peer_id),
                );
                if !reaction_userpics_slot.as_ref().is_some_and(|kept| {
                    let (id, title) = &kept.value().peer;
                    *id == peer.summary.id && *title == peer.summary.title
                }) {
                    reaction_userpics_slot = None;
                }
                Some(Gated::get_or_read(
                    &mut reaction_userpics_slot,
                    generation,
                    || {
                        let (self_id, self_name) = backend.self_peer();
                        ReactionUserpics {
                            peer: (peer.summary.id, peer.summary.title.clone()),
                            me: (self_id, self_name),
                        }
                    },
                ))
            } else {
                None
            };

        self.last_painted_bubble_rects.clear();

        // How far this frame moved the view to keep its top message in place.
        let mut anchor_shift = 0i32;
        let scroll_out = ui.scope_builder(egui::UiBuilder::new().max_rect(list_rect), |ui| {
            // Clear stale ScrollArea physics state (`max_offset`, `vel`, `offset_target`) before
            // an explicit programmatic scroll jump. Otherwise egui's `ScrollArea::begin` compares
            // the new `target_y` against the *previous* frame's smaller `max_offset` before this
            // frame's content is allocated, treats the increase as rubber-band overscroll, and
            // injects a negative spring velocity that coasts the list upward on later frames.
            // Note: `ScrollArea::id_salt` wraps `scroll_salt` in `egui::IdSalt::new`, and
            // `ScrollArea::begin` calls `ui.make_persistent_id(id_salt)` on that `IdSalt`.
            let expected_scroll_id = ui.make_persistent_id(egui::IdSalt::new(scroll_salt));
            if explicit_scroll_y.is_some() {
                egui::scroll_area::State::default().store(ui.ctx(), expected_scroll_id);
            }
            let out = scroll_area.show_viewport(ui, |ui, viewport| {
                let mut vis_top = viewport.min.y.floor() as i32;
                let mut vis_bottom = viewport.max.y.ceil() as i32;
                let was_at_bottom = self.is_at_loaded_bottom(vis_bottom);
                let overscan = 160;
                let visible = self.list.visible_items_top_to_bottom(
                    (vis_top - overscan).max(0),
                    vis_bottom + overscan,
                );

                // Media `load_peer` left for the first frames: what is near the viewport is
                // asked for before it is painted.
                self.request_backlog_media(ui.ctx(), vis_top, vis_bottom, &*backend);

                // Sync live voice/round-video transcription state and limits for visible items.
                self.voice_state.transcribe_limits = backend.transcribe_limits(Some(self.peer_id));
                for v in &visible {
                    self.sync_voice_transcription_for(v.index, backend);
                    self.sync_summary_state_for(v.index, backend);
                }

                // Phase 1: Lazily compute exact layouts ONLY for visible items (+ small overscan).
                // A measured height above the view moves what is on screen; the view follows its
                // top message in this same frame (`HistoryWidget::updateHistoryGeometry` keeps
                // `scrollTopItem` in place synchronously), so the frame never shows the content
                // shifted and the next frame does not snap back (TGB-2952).
                let pinned = pinning_to_bottom || was_at_bottom;
                if pinned {
                    // A view held at the bottom has no anchor; one left from a resize would be
                    // applied later, at a place the user has scrolled away from.
                    self.list.scroll_top_state = tg_history::ScrollTopState::default();
                }
                let mut to_measure = visible;
                for _ in 0..3 {
                    let mut heights_changed = false;
                    for v in &to_measure {
                        let msg_id = self.messages[v.index].id;
                        if !self.layout_cache.contains_key(&msg_id) {
                            let fc_extra_h = self.factcheck_extra_height(msg_id, backend);
                            let summary_entry = backend.message_summary(self.peer_id, msg_id);
                            let changed = self.ensure_item_laid_out(
                                ui,
                                v.index,
                                viewport_w,
                                is_chat_wide,
                                peer,
                                colors,
                                &fonts,
                                fc_extra_h,
                                summary_entry.as_ref(),
                            );
                            if changed {
                                heights_changed = true;
                            }
                            laid_out_this_frame += 1;
                        }
                    }
                    if !heights_changed {
                        break;
                    }
                    if pinned {
                        self.list.relayout_positions();
                        let items_top = ((viewport_h as i32)
                            - self.list.items_height()
                            - content_pad_bottom)
                            .max(0);
                        self.list.set_items_top(items_top);
                        self.pending_scroll_to_bottom = true;
                        ui.ctx().request_repaint();
                        break;
                    }
                    // An anchor saved before a resize dropped the layouts wins over this one.
                    if !tg_history::is_message_position_valid(self.list.scroll_top_state.item) {
                        self.list.scroll_top_state = self.anchor_below_top(vis_top, vis_bottom);
                    }
                    self.list.relayout_positions();
                    let items_top =
                        ((viewport_h as i32) - self.list.items_height() - content_pad_bottom)
                            .max(0);
                    self.list.set_items_top(items_top);
                    if let Some(restored) = self.list.restore_scroll_top() {
                        let max_top = (self.list.total_height() - viewport_h as i32).max(0);
                        let shift = restored.min(max_top) - vis_top;
                        vis_top += shift;
                        vis_bottom += shift;
                        anchor_shift += shift;
                    }
                    to_measure = self.list.visible_items_top_to_bottom(
                        (vis_top - overscan).max(0),
                        vis_bottom + overscan,
                    );
                }
                // A resize anchor no height change used is applied now; left in place it would
                // stop the preloads and be applied later, far from where the view is.
                if let Some(restored) = self.list.restore_scroll_top() {
                    let max_top = (self.list.total_height() - viewport_h as i32).max(0);
                    let shift = restored.min(max_top) - vis_top;
                    vis_top += shift;
                    vis_bottom += shift;
                    anchor_shift += shift;
                }

                // Allocate the full content area AFTER Phase 1 so `ScrollArea::end` records the
                // up-to-date `total_height()` on this frame rather than the pre-layout estimate.
                let total_h = (self.list.total_height() as f32).max(viewport_h);
                let (content_rect, bg_resp) =
                    ui.allocate_exact_size(Vec2::new(list_rect.width(), total_h), Sense::click());
                // Rows are placed as if the scroll area already stood at the corrected offset.
                // A pull past the bottom lifts the rows; the wallpaper and the scroll stay.
                let paint_top = content_rect.min.y - anchor_shift as f32 - self.bottom_stretch;
                // Rows are painted in a child that adds nothing to the scrolled content: the
                // scroll bar is sized from the list height alone (`HistoryInner::height` in
                // tdesktop), whatever a row's widgets allocate (TGB-2952).
                let mut rows_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(content_rect)
                        .layout(*ui.layout()),
                );
                let ui = &mut rows_ui;

                if bg_resp.clicked() && !self.list.selection.has_selected_items() {
                    self.list.selection.clear();
                }

                // Re-query exact visible items after any height updates.
                let visible_paint = self.list.visible_items_top_to_bottom(vis_top, vis_bottom);
                self.painted_msg_ids.clear();
                self.painted_msg_ids
                    .extend(visible_paint.iter().map(|v| self.messages[v.index].id));
                let broadcast = matches!(peer.summary.kind, PeerKind::Channel);
                self.read_till_shown = visible_paint
                    .iter()
                    .filter_map(|v| {
                        let msg = self.messages.get(v.index)?;
                        let unread = !msg.outgoing
                            && msg.id.0 > 0
                            && msg.id > self.last_inbox_read_sent;
                        (unread
                            && crate::history::marking_read::unread_item_shown(
                                broadcast, v.top, v.bottom, vis_top, vis_bottom,
                            ))
                        .then_some(msg.id)
                    })
                    .max();
                self.media_runtime.begin_frame(now_ms);
                self.last_bubble_anchors.clear();

                let now_ms_u64 = now_ms.max(0) as u64;
                let (app_focused, user_active) = ui.ctx().input(|i| {
                    let moved = i.pointer.delta().length_sq() > 0.0
                        || i.pointer.any_pressed()
                        || i.smooth_scroll_delta.length_sq() > 0.0
                        || !i.keys_down.is_empty();
                    (i.focused, moved)
                });
                let rm = self.read_metrics.get_or_insert_with(|| {
                    tg_app_core::core_types::read_metrics::ReadMetricsTracker::new(self.peer_id)
                });
                rm.set_app_active(app_focused, now_ms_u64);
                if user_active {
                    rm.register_activity(now_ms_u64);
                }
                rm.start_batch(vis_top, vis_bottom, now_ms_u64);

                // Layouts invalidated by an action taken while painting: dropped after the
                // loop, so the paint can borrow the cache instead of cloning each layout.
                let mut invalidated: Vec<MsgId> = Vec::new();

                // Phase 2: Paint visible items only!
                for v in &visible_paint {
                    let msg = &self.messages[v.index];
                    let item = &self.list.items()[v.index];
                    // The message flying from the field is drawn in the flight's layer, which is
                    // moved after the paint (tdesktop skips it here and draws the view itself).
                    let mut flight_ui = None;
                    if let Some(row) = self
                        .send_flight_row
                        .as_mut()
                        .filter(|row| row.msg_id == msg.id)
                    {
                        let text_bubble = matches!(
                            self.layout_cache.get(&msg.id).map(|cached| &cached.layout),
                            Some(CachedItemLayout::Message(layout))
                                if layout.has_bubble
                                    && !layout.unwrapped
                                    && layout.media_rect.is_empty()
                                    && !layout.text_rect.is_empty()
                        );
                        if text_bubble {
                            let mut child = ui.new_child(
                                egui::UiBuilder::new()
                                    .layer_id(row.layer)
                                    .max_rect(ui.max_rect()),
                            );
                            child.set_opacity(row.opacity);
                            flight_ui = Some(child);
                        } else {
                            row.refused = true;
                        }
                    }
                    let ui: &mut Ui = match flight_ui.as_mut() {
                        Some(child) => child,
                        None => &mut *ui,
                    };
                    if let Some(ref mut tracker) = self.read_metrics {
                        tracker.push(
                            msg.id,
                            !msg.is_service(),
                            msg.views.is_some(),
                            v.top,
                            item.height,
                        );
                    }
                    let row_origin =
                        Pos2::new(content_rect.min.x, paint_top + v.top as f32);
                    let row_rect = Rect::from_min_size(
                        row_origin,
                        Vec2::new(content_rect.width(), item.height as f32),
                    );

                    // Optional search filter highlight tint.
                    let search_hit = !search_q.is_empty()
                        && match self.layout_cache.get_mut(&msg.id) {
                            // The lowercase text is kept with the message, not rebuilt per frame.
                            Some(cached) => cached
                                .paint
                                .contains_lowercase(&msg.text.text, &search_q),
                            None => msg.text.text.to_lowercase().contains(&search_q),
                        };
                    if search_hit {
                        ui.painter().rect_filled(
                            row_rect,
                            CornerRadius::ZERO,
                            Color32::from_rgba_unmultiplied(255, 220, 80, 56),
                        );
                    }

                    // Selection row tint & left checkbox in multi-select mode.
                    let full_id = to_hist_full(FullMsgId {
                        peer: self.peer_id,
                        msg: msg.id,
                    });
                    let sel = self.list.selection.selection_for_item(v.index, full_id);
                    if sel.is_full() {
                        // The date badge and the unread bar above the message stay untinted
                        // (`Message::draw` fills from below them).
                        let skip = item.displayed_date_height
                            + if item.has_unread_bar {
                                unread_bar_height()
                            } else {
                                0
                            };
                        let mut tint = row_rect;
                        tint.min.y = (tint.min.y + skip as f32).min(tint.max.y);
                        ui.painter()
                            .rect_filled(tint, CornerRadius::ZERO, colors.select_overlay);
                    }

                    // Jump-to-message full-row highlight overlay (`ElementHighlighter`).
                    let hl = self.highlighter.state_for(full_id, now_ms);
                    if hl.opacity > 0.01 {
                        let alpha = (hl.opacity * 72.0).round().clamp(0.0, 255.0) as u8;
                        ui.painter().rect_filled(
                            row_rect,
                            CornerRadius::ZERO,
                            Color32::from_rgba_unmultiplied(
                                colors.accent.r(),
                                colors.accent.g(),
                                colors.accent.b(),
                                alpha,
                            ),
                        );
                    }

                    // Date separator badge above item if `attach_state.display_date`.
                    let mut top_decor_y = row_origin.y;
                    if item.attach_state.display_date {
                        let date_label = format_date_separator(msg.date_unix);
                        let date_w = ui
                            .painter()
                            .layout_no_wrap(
                                date_label.clone(),
                                FontId::proportional(12.5),
                                colors.service_fg,
                            )
                            .size()
                            .x
                            .ceil() as i32;
                        let dr =
                            count_service_date_rect(date_w, 0, viewport_w, is_chat_wide, &fonts);
                        let egui_dr = Rect::from_min_size(
                            Pos2::new(row_origin.x + dr.x as f32, top_decor_y + dr.y as f32),
                            Vec2::new(dr.width as f32, dr.height as f32),
                        );
                        paint_date_separator(ui.painter(), egui_dr, &date_label, colors);
                        top_decor_y += date_badge_height(&fonts) as f32;
                    }

                    // Full-width "Unread messages" bar above first unread item.
                    if item.has_unread_bar {
                        paint_unread_bar(
                            ui.painter(),
                            row_origin.x,
                            content_rect.width(),
                            top_decor_y + (LINE_WIDTH + HISTORY_UNREAD_BAR_MARGIN) as f32 * 0.5,
                            colors,
                        );
                    }

                    // Register row-level background hit area BEFORE inner bubble controls so inner
                    // buttons (comments bar, reaction pills, links, media, polls) receive primary clicks.
                    let row_resp = ui.interact(
                        row_rect,
                        Id::unique(("tg_hist_row", msg.id.0)),
                        Sense::click(),
                    );
                    if !msg.is_service() && !msg.text.text.is_empty() {
                        row_resp.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::accesskit::Role::Button,
                                true,
                                &msg.text.text,
                            )
                        });
                    }

                    // Paint cached message or service layout, borrowed from the cache. A
                    // layout invalidated earlier on this frame is treated as already gone.
                    let cached = if invalidated.contains(&msg.id) {
                        None
                    } else {
                        self.layout_cache.get_mut(&msg.id)
                    };
                    if let Some(CachedItem {
                        layout: cached_layout,
                        paint: paint_cache,
                    }) = cached
                    {
                        match cached_layout {
                            CachedItemLayout::Service(svc_layout) => {
                                // Only the no-forwards request card has an "acted" state.
                                let action_taken = matches!(
                                    msg.service,
                                    Some(ServiceAction::NoForwardsRequest { .. })
                                ) && backend
                                    .is_no_forwards_request_acted(self.peer_id, msg.id);
                                if let Some(svc_act) = paint_service_message_interactive(
                                    ui,
                                    msg,
                                    svc_layout,
                                    row_origin,
                                    action_taken,
                                    &peer.summary.title,
                                    self.reduced_motion,
                                    colors,
                                ) {
                                    match svc_act {
                                        ServiceCardAction::NoForwards(nfr_act) => match nfr_act {
                                            crate::history::no_forwards_request::NoForwardsRequestAction::Accept(mid) => {
                                                invalidated.push(mid);
                                                events.push(InnerEvent::RespondNoForwardsRequest {
                                                    msg_id: mid,
                                                    accept: true,
                                                });
                                            }
                                            crate::history::no_forwards_request::NoForwardsRequestAction::Decline(mid) => {
                                                invalidated.push(mid);
                                                events.push(InnerEvent::RespondNoForwardsRequest {
                                                    msg_id: mid,
                                                    accept: false,
                                                });
                                            }
                                        },
                                        ServiceCardAction::Suggestion(sact) => match sact {
                                            crate::history::suggestion_cards::SuggestionCardAction::ApplyBirthday {
                                                msg_id,
                                                day,
                                                month,
                                                year,
                                            } => {
                                                events.push(InnerEvent::ApplySuggestedBirthday {
                                                    msg_id,
                                                    day,
                                                    month,
                                                    year,
                                                });
                                            }
                                            crate::history::suggestion_cards::SuggestionCardAction::ApplyUserpic {
                                                msg_id,
                                                photo_id,
                                                access_hash,
                                                file_reference,
                                                is_video,
                                            } => {
                                                events.push(InnerEvent::ApplySuggestedUserpic {
                                                    msg_id,
                                                    photo_id,
                                                    access_hash,
                                                    file_reference,
                                                    is_video,
                                                });
                                            }
                                            crate::history::suggestion_cards::SuggestionCardAction::ViewUserpic {
                                                msg_id,
                                            } => {
                                                events.push(InnerEvent::ViewSuggestedUserpic(msg_id));
                                            }
                                        },
                                    }
                                }
                            }
                            CachedItemLayout::Message(msg_layout) => {
                                self.last_painted_bubble_rects.insert(
                                    msg.id,
                                    crate::history::bubble::hist_rect_to_egui(
                                        msg_layout.geometry,
                                        row_origin.x,
                                        row_origin.y,
                                    ),
                                );
                                if let Some(row) = self
                                    .send_flight_row
                                    .as_mut()
                                    .filter(|row| row.msg_id == msg.id && !row.refused)
                                {
                                    row.text_origin = Some(
                                        hist_rect_to_egui(
                                            msg_layout.text_rect,
                                            row_origin.x,
                                            row_origin.y,
                                        )
                                        .min,
                                    );
                                }
                                let bubble_rect = hist_rect_to_egui(
                                    msg_layout.geometry,
                                    row_origin.x,
                                    row_origin.y,
                                );
                                self.last_bubble_anchors
                                    .insert(msg.id, (bubble_rect.center(), msg.outgoing));
                                let interactive_emoji =
                                    crate::history::emoji_interactions::can_play_emoji_interaction_message(
                                        msg,
                                        peer.summary.kind,
                                    )
                                    .map(str::to_owned)
                                    .or_else(|| {
                                        if matches!(peer.summary.kind, PeerKind::User)
                                            && !msg.is_service()
                                            && msg.id.0 > 0
                                            && msg.media.is_none()
                                            && msg.extra.rich_message.is_none()
                                            && backend
                                                .emoji_interaction_variant_count(&msg.text.text)
                                                .is_some()
                                        {
                                            Some(msg.text.text.trim().to_owned())
                                        } else {
                                            None
                                        }
                                    });
                                if let Some(interactive_emoji) = interactive_emoji {
                                    let tap_resp = ui.interact(
                                        bubble_rect,
                                        Id::unique(("tg_interactive_emoji_tap", msg.id.0)),
                                        Sense::click(),
                                    );
                                    if tap_resp.hovered() {
                                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                    }
                                    if tap_resp
                                        .on_hover_text("Tap for interactive emoji animation")
                                        .clicked()
                                    {
                                        events.push(InnerEvent::TapInteractiveEmoji {
                                            msg_id: msg.id,
                                            emoticon: interactive_emoji,
                                            anchor: bubble_rect.center(),
                                            outgoing: msg.outgoing,
                                        });
                                    }
                                }
                                let show_name = shows_from_name(
                                    msg,
                                    show_sender_in_chat,
                                    item.attach_state.attached_to_previous,
                                );
                                let summary_entry = backend.message_summary(self.peer_id, msg.id);
                                let paused_chat_effects =
                                    !self.media_runtime.policy.allows_chat_effects();
                                let paint_events = paint_message_element_full(
                                    ui,
                                    self.peer_id,
                                    msg,
                                    msg_layout,
                                    row_origin,
                                    sel,
                                    hl.opacity,
                                    show_name,
                                    colors,
                                    &mut self.assets,
                                    &mut self.voice_state,
                                    &mut self.media_runtime,
                                    Some(InlineKeyboardRenderCtx {
                                        state: &mut self.inline_kb_state,
                                        bot_id: bot_id.or_else(|| {
                                            (!msg.outgoing).then_some(tg_bots::stand_in::UserId(
                                                msg.from_id.bare(),
                                            ))
                                        }),
                                        bot_verified,
                                        palette,
                                    }),
                                    summary_entry.as_ref(),
                                    paused_chat_effects,
                                    reaction_userpics,
                                    paint_cache,
                                    Some(
                                        self.text_states
                                            .get(&msg.id)
                                            .unwrap_or(&UNTOUCHED_TEXT),
                                    ),
                                    // `Element::isLastAndSelfMessage`: an outgoing message that
                                    // is the chat's last, outside Saved Messages.
                                    msg.outgoing
                                        && !msg.is_post
                                        && !peer.summary.is_saved_messages
                                        && self.list.loaded_at_bottom
                                        && v.index + 1 == self.messages.len(),
                                );
                                if msg.is_post
                                    && matches!(peer.summary.kind, PeerKind::Channel)
                                    && msg_layout.has_bubble
                                    && paint_fast_share_button(ui, msg.id, bubble_rect, colors)
                                {
                                    events.push(InnerEvent::Forward(vec![msg.id]));
                                }
                                if msg.is_post
                                    && matches!(peer.summary.kind, PeerKind::Channel)
                                    && !msg_layout.has_bubble
                                    && !msg_layout.media_rect.is_empty()
                                {
                                    let anchor = unwrapped_right_action_anchor(
                                        msg_layout,
                                        row_origin,
                                    );
                                    match msg.extra.replies_count {
                                        Some(count) => {
                                            if paint_side_comments_button(
                                                ui, msg.id, count, anchor, colors,
                                            ) {
                                                events.push(InnerEvent::OpenComments(msg.id));
                                            }
                                        }
                                        None => {
                                            if paint_fast_share_button(ui, msg.id, anchor, colors)
                                            {
                                                events.push(InnerEvent::Forward(vec![msg.id]));
                                            }
                                        }
                                    }
                                }
                                for ev in paint_events {
                                    match ev {
                                        MessagePaintEvent::JumpToMessage(tid) => {
                                            jump_to_msg = Some(tid);
                                        }
                                        MessagePaintEvent::OpenLink(url) => {
                                            events.push(
                                                match tg_ui::entity_link::EntityLink::parse(&url) {
                                                    Some(link) => InnerEvent::EntityLink {
                                                        msg_id: msg.id,
                                                        link,
                                                    },
                                                    None => InnerEvent::OpenLink(url),
                                                },
                                            );
                                        }
                                        MessagePaintEvent::OpenHiddenLink(url) => {
                                            events.push(InnerEvent::OpenHiddenLink(url));
                                        }
                                        MessagePaintEvent::ToggleReaction { msg_id, emoji } => {
                                            events
                                                .push(InnerEvent::ToggleReaction { msg_id, emoji });
                                        }
                                        MessagePaintEvent::Media(mev) => match mev {
                                            MediaInteraction::OpenStickerSet(set) => {
                                                events.push(InnerEvent::OpenStickerSet(set));
                                            }
                                            MediaInteraction::OpenMedia {
                                                msg_id,
                                                album_index,
                                            } => {
                                                if let Some(ref lp) = msg.extra.link_preview
                                                    && let Some(stars_amount) =
                                                        parse_paid_media_stars(lp)
                                                {
                                                    events.push(InnerEvent::UnlockPaidMedia {
                                                        msg_id,
                                                        stars_amount,
                                                        title: lp.title.clone(),
                                                    });
                                                } else {
                                                    events.push(InnerEvent::OpenMedia {
                                                        msg_id,
                                                        album_index,
                                                    });
                                                }
                                            }
                                            MediaInteraction::SaveAs { msg_id, file_name } => {
                                                events
                                                    .push(InnerEvent::SaveAs { msg_id, file_name });
                                            }
                                            MediaInteraction::VotePoll {
                                                msg_id,
                                                option_index,
                                            } => events.push(InnerEvent::VotePoll {
                                                msg_id,
                                                option_index,
                                            }),
                                            MediaInteraction::OpenPollVoters(msg_id) => {
                                                events.push(InnerEvent::OpenPollVoters(msg_id));
                                            }
                                            MediaInteraction::PlayGame(msg_id) => {
                                                events.push(InnerEvent::PlayGame(msg_id));
                                            }
                                            MediaInteraction::ToggleTodoItem {
                                                msg_id,
                                                item_id,
                                            } => {
                                                events.push(InnerEvent::ToggleTodoItem {
                                                    msg_id,
                                                    item_id,
                                                });
                                            }
                                            MediaInteraction::AddTodoTasks(msg_id) => {
                                                events.push(InnerEvent::AddTodoTasks(msg_id));
                                            }
                                            MediaInteraction::EditTodoList(msg_id) => {
                                                events.push(InnerEvent::EditTodoList(msg_id));
                                            }
                                            MediaInteraction::ToggleTranscribe {
                                                msg_id,
                                                duration_secs,
                                                is_round_video,
                                            } => {
                                                invalidated.push(msg_id);
                                                events.push(InnerEvent::ToggleTranscribe {
                                                    msg_id,
                                                    duration_secs,
                                                    is_round_video,
                                                });
                                            }
                                            MediaInteraction::RateTranscribe { msg_id, good } => {
                                                events.push(InnerEvent::RateTranscribe {
                                                    msg_id,
                                                    good,
                                                });
                                            }
                                            MediaInteraction::OpenWidget(msg_id) => {
                                                events.push(InnerEvent::OpenWidget(msg_id));
                                            }
                                            MediaInteraction::OpenAddPollOption(msg_id) => {
                                                events.push(InnerEvent::OpenAddPollOption(msg_id));
                                            }
                                            MediaInteraction::OpenViewOnceMedia {
                                                msg_id,
                                                kind,
                                                ttl_seconds,
                                            } => {
                                                events.push(InnerEvent::OpenViewOnceMedia {
                                                    msg_id,
                                                    kind,
                                                    ttl_seconds,
                                                });
                                            }
                                            MediaInteraction::OpenStory { peer_id, story_id } => {
                                                events.push(InnerEvent::OpenStory {
                                                    peer_id,
                                                    story_id,
                                                });
                                            }
                                            MediaInteraction::OpenGiveawayInfo {
                                                peer_id,
                                                msg_id,
                                                channel_id,
                                                launch_msg_id,
                                            } => {
                                                events.push(InnerEvent::OpenGiveawayInfo {
                                                    peer_id,
                                                    msg_id,
                                                    channel_id,
                                                    launch_msg_id,
                                                });
                                            }
                                            MediaInteraction::OpenVideoStream {
                                                msg_id,
                                                call_id,
                                                access_hash,
                                                rtmp_stream,
                                                title,
                                            } => {
                                                events.push(InnerEvent::OpenVideoStream {
                                                    msg_id,
                                                    call_id,
                                                    access_hash,
                                                    rtmp_stream,
                                                    title,
                                                });
                                            }
                                            MediaInteraction::ToggleVoicePlay { .. } => {}
                                        },
                                        MessagePaintEvent::TextDragStart {
                                            msg_id: _,
                                            char_index,
                                        } => {
                                            self.list.selection.selected_items.clear();
                                            self.text_drag_active = true;
                                            self.list.selection.start_drag(ListSelectionEndpoint {
                                                item_id: full_id,
                                                item_index: v.index,
                                                text: FlatSelectionEndpoint::new(char_index, false),
                                            });
                                        }
                                        MessagePaintEvent::TextDragUpdate {
                                            msg_id: _,
                                            char_index,
                                        } => {
                                            // egui keeps the drag on the message it began in;
                                            // the message under the pointer says whether it
                                            // crossed into others (whole messages then).
                                            let endpoint = ui
                                                .ctx()
                                                .pointer_interact_pos()
                                                .and_then(|p| {
                                                    self.list.find_item_index_by_y(
                                                        (p.y - paint_top).floor() as i32,
                                                    )
                                                })
                                                .filter(|&i| i != v.index)
                                                .and_then(|i| {
                                                    let m = self.messages.get(i)?;
                                                    Some(ListSelectionEndpoint {
                                                        item_id: to_hist_full(FullMsgId {
                                                            peer: self.peer_id,
                                                            msg: m.id,
                                                        }),
                                                        item_index: i,
                                                        text: FlatSelectionEndpoint::new(0, false),
                                                    })
                                                })
                                                .unwrap_or(ListSelectionEndpoint {
                                                    item_id: full_id,
                                                    item_index: v.index,
                                                    text: FlatSelectionEndpoint::new(
                                                        char_index, false,
                                                    ),
                                                });
                                            self.list.selection.update_drag(endpoint);
                                        }
                                        MessagePaintEvent::OpenComments(mid) => {
                                            events.push(InnerEvent::OpenComments(mid));
                                        }
                                        MessagePaintEvent::ClearTranslation(mid) => {
                                            events.push(InnerEvent::Translate {
                                                msg_id: mid,
                                                clear: true,
                                            });
                                        }
                                        MessagePaintEvent::OpenReactionsPicker { msg_id, pos } => {
                                            events.push(InnerEvent::OpenReactionsPicker {
                                                msg_id,
                                                pos,
                                            });
                                        }
                                        MessagePaintEvent::InlineKeyboard(act) => {
                                            events.push(InnerEvent::InlineKeyboard(act));
                                        }
                                        MessagePaintEvent::OpenInvoice { msg_id, invoice_id } => {
                                            events.push(InnerEvent::OpenInvoice {
                                                msg_id,
                                                invoice_id,
                                            });
                                        }
                                        MessagePaintEvent::UnlockPaidMedia {
                                            msg_id,
                                            stars_amount,
                                            title,
                                        } => {
                                            events.push(InnerEvent::UnlockPaidMedia {
                                                msg_id,
                                                stars_amount,
                                                title,
                                            });
                                        }
                                        MessagePaintEvent::TapInteractiveEmoji {
                                            msg_id,
                                            emoticon,
                                            anchor,
                                            outgoing,
                                        } => {
                                            events.push(InnerEvent::TapInteractiveEmoji {
                                                msg_id,
                                                emoticon,
                                                anchor,
                                                outgoing,
                                            });
                                        }
                                        MessagePaintEvent::ToggleSummary(mid) => {
                                            invalidated.push(mid);
                                            events.push(InnerEvent::ToggleSummary(mid));
                                        }
                                        MessagePaintEvent::AboutAiSummary(mid) => {
                                            events.push(InnerEvent::AboutAiSummary(mid));
                                        }
                                        MessagePaintEvent::RevealSpoilers(mid) => {
                                            let now = ui.input(|i| i.time);
                                            self.text_states
                                                .entry(mid)
                                                .or_default()
                                                .spoilers_revealed_at
                                                .get_or_insert(now);
                                        }
                                        MessagePaintEvent::ToggleQuote { msg_id, block } => {
                                            self.text_states
                                                .entry(msg_id)
                                                .or_default()
                                                .toggle_quote(block);
                                            // The bubble is measured with the quote's lines.
                                            invalidated.push(msg_id);
                                        }
                                    }
                                }

                                // Fact-check bubble footer (`data/components/factchecks.cpp`)
                                if let Some(ex) = backend.as_layer233_extras()
                                    && let Some(fc) = ex.cached_factcheck(self.peer_id, msg.id)
                                {
                                    let expanded = self.expanded_factchecks.contains(&msg.id);
                                    // The card ends the text block, above the time line; the
                                    // reactions and the bottom info come below it (TGB-3022).
                                    // A message without text keeps the card at its bubble's
                                    // bottom, inside the bubble's padding.
                                    let text_rect = if msg_layout.text_rect.is_empty() {
                                        crate::history::bubble::hist_rect_to_egui(
                                            msg_layout.geometry,
                                            row_origin.x,
                                            row_origin.y,
                                        )
                                        .shrink2(Vec2::new(10.0, 8.0))
                                    } else {
                                        crate::history::bubble::hist_rect_to_egui(
                                            msg_layout.text_rect,
                                            row_origin.x,
                                            row_origin.y,
                                        )
                                    };
                                    // With reactions in the bubble the time sits by them, so
                                    // no time line is reserved under the text block.
                                    let date_line = if msg.reactions.is_empty() {
                                        msg_layout.bottom_info.current_size.height as f32
                                    } else {
                                        0.0
                                    };
                                    let fc_rect = crate::history::factcheck::factcheck_rect(
                                        text_rect,
                                        crate::history::factcheck::factcheck_reserved_height(
                                            &fc, expanded,
                                        ),
                                        date_line,
                                    );
                                    let can_edit = crate::history::factcheck::can_edit_factcheck(
                                        ex.can_edit_factcheck_config(),
                                        matches!(peer.summary.kind, PeerKind::Channel),
                                        !msg.is_service(),
                                        msg.media.as_ref(),
                                    );
                                    if let Some(fc_ev) =
                                        crate::history::factcheck::paint_factcheck_footer(
                                            ui,
                                            msg.id,
                                            &fc,
                                            expanded,
                                            can_edit,
                                            fc_rect,
                                            msg_layout.has_out_layout,
                                            colors,
                                        )
                                    {
                                        match fc_ev {
                                            crate::history::factcheck::FactCheckFooterAction::ToggleExpand(mid) => {
                                                if !self.expanded_factchecks.remove(&mid) {
                                                    self.expanded_factchecks.insert(mid);
                                                }
                                                invalidated.push(mid);
                                            }
                                            crate::history::factcheck::FactCheckFooterAction::Edit(mid) => {
                                                events.push(InnerEvent::EditFactCheck(mid));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Multi-select checkbox on the left edge of each message row.
                    if show_selection_checks && !msg.is_service() {
                        let check_center = Pos2::new(
                            row_origin.x + 14.0,
                            row_rect.bottom() - MSG_MARGIN.bottom as f32 - 16.0,
                        );
                        paint_selection_checkbox(ui.painter(), check_center, sel.is_full(), colors);
                    }

                    // Bubble hover corner quick actions (Reply / React / Forward).
                    if !show_selection_checks
                        && !msg.is_service()
                        && let Some(&bubble_rect) = self.last_painted_bubble_rects.get(&msg.id)
                    {
                        let row_hovered = row_resp.hovered() || ui.rect_contains_pointer(row_rect);
                        let qa_opacity = self.bubble_hover_quick_actions.opacity_for(
                            msg.id,
                            row_hovered,
                            (now_ms as f64) * 0.001,
                            self.reduced_motion,
                        );
                        // Only the hovered row's fade needs frames: a row covered by the
                        // context menu stops being hovered before its fade finishes.
                        if row_hovered && self.bubble_hover_quick_actions.is_animating() {
                            ui.ctx().request_repaint();
                        }
                        let has_right_layout = !invalidated.contains(&msg.id)
                            && matches!(
                                self.layout_cache.get(&msg.id).map(|cached| &cached.layout),
                                Some(CachedItemLayout::Message(l)) if l.has_right_layout
                            );
                        if qa_opacity > 0.02
                            && let Some(qa_out) =
                                crate::history::quick_action::show_bubble_hover_quick_actions(
                                    ui,
                                    bubble_rect,
                                    row_rect,
                                    has_right_layout,
                                    msg,
                                    self.corner_reply,
                                    self.corner_reaction,
                                    &self.quick_reaction_emoji,
                                    qa_opacity,
                                    colors,
                                )
                        {
                            match qa_out {
                                crate::history::quick_action::HistoryQuickActionOutcome::Reply(
                                    mid,
                                ) => {
                                    events.push(InnerEvent::Reply(mid));
                                }
                                crate::history::quick_action::HistoryQuickActionOutcome::React {
                                    msg_id,
                                    emoji,
                                } => {
                                    events.push(InnerEvent::ToggleReaction { msg_id, emoji });
                                }
                                crate::history::quick_action::HistoryQuickActionOutcome::Forward(
                                    mid,
                                ) => {
                                    events.push(InnerEvent::Forward(vec![mid]));
                                }
                            }
                        }
                    }

                    // Row-level right-click context menu, double-click quick action & multi-select click.
                    let row_secondary_clicked = row_resp.secondary_clicked()
                        || (ui.rect_contains_pointer(row_rect)
                            && ui.ctx().input(|i| i.pointer.secondary_clicked()));
                    if row_secondary_clicked
                        && !msg.is_service()
                        && let Some(pos) = ui.ctx().pointer_interact_pos()
                    {
                        open_context_menu_for = Some((msg.id, pos));
                    } else if row_resp.double_clicked()
                        && !show_selection_checks
                        && !msg.is_service()
                        && let Some(qa_out) =
                            crate::history::quick_action::resolve_double_click_action(
                                self.chat_quick_action,
                                msg,
                                &self.quick_reaction_emoji,
                            )
                    {
                        match qa_out {
                            crate::history::quick_action::HistoryQuickActionOutcome::Reply(mid) => {
                                events.push(InnerEvent::Reply(mid));
                            }
                            crate::history::quick_action::HistoryQuickActionOutcome::React {
                                msg_id,
                                emoji,
                            } => {
                                events.push(InnerEvent::ToggleReaction { msg_id, emoji });
                            }
                            crate::history::quick_action::HistoryQuickActionOutcome::Forward(
                                mid,
                            ) => {
                                events.push(InnerEvent::Forward(vec![mid]));
                            }
                        }
                    } else if row_resp.clicked() && !msg.is_service() {
                        let shift_held = ui.ctx().input(|i| i.modifiers.shift);
                        if shift_held && (show_selection_checks || self.selection_anchor.is_some())
                        {
                            self.select_range_to(msg.id);
                        } else if show_selection_checks || shift_held {
                            self.toggle_select_message(msg.id);
                        }
                    }

                    painted_this_frame += 1;
                }

                for id in invalidated {
                    self.layout_cache.remove(&id);
                }
                self.trim_paint_caches();

                // Pause any inline GIFs / round videos / stickers that scrolled offscreen.
                self.media_runtime.finish_frame(now_ms);
                if let Some(ref mut tracker) = self.read_metrics {
                    let finalized = tracker.end_batch();
                    if !finalized.is_empty() {
                        backend.report_read_metrics(self.peer_id, finalized);
                    }
                }

                // Phase 3: Sticky userpics (`HistoryList::enumerate_userpics`): groups, and every
                // chat in the wide layout (`hasFromPhoto`).
                if show_sender_in_chat || self.layout_wide {
                    let ui_colors = colors.ui_colors();
                    let userpics = self.list.enumerate_userpics(vis_top, vis_bottom);
                    for up in userpics {
                        if let Some(msg) = self.messages.get(up.item_index) {
                            let avatar_rect = Rect::from_min_size(
                                Pos2::new(
                                    content_rect.min.x + HISTORY_PHOTO_LEFT as f32,
                                    paint_top + up.top as f32,
                                ),
                                Vec2::splat(MSG_PHOTO_SIZE as f32),
                            );
                            if !tg_ui::userpic_images::paint_userpic_image(
                                ui.painter(),
                                avatar_rect,
                                msg.from_id.0,
                            ) {
                                // `EmptyUserpic::UserpicColor(peer->colorIndex())`.
                                EmptyUserpic::for_color_index(
                                    crate::history::name_line::sender_accent(msg).index,
                                    &msg.sender_name,
                                    &ui_colors,
                                )
                                .paint_circle(ui.painter(), avatar_rect);
                            }
                        }
                    }
                }
                let spilled = ui.min_rect();
                if spilled.min.y < content_rect.min.y - 1.0
                    || spilled.max.y > content_rect.max.y + 1.0
                {
                    tracing::debug!(
                        ?spilled,
                        ?content_rect,
                        painted = ?self.painted_msg_ids,
                        "history rows placed widgets outside the list"
                    );
                }
            });
            debug_assert_eq!(out.id, expected_scroll_id);
            out
        });

        self.reaction_userpics = reaction_userpics_slot;
        self.last_frame_laid_out_count = laid_out_this_frame;
        self.last_frame_painted_count = painted_this_frame;

        // Track scroll state & bottom-edge detection before read-marking checks so `scrolled_up`
        // reflects this frame's actual scroll position.
        self.scroll_offset_y = scroll_out.inner.state.clamped_offset().y;
        self.drive_text_drag(ui.ctx(), list_rect);
        if anchor_shift != 0 {
            // The scroll area takes the correction on the next frame, on top of whatever the
            // wheel added this frame.
            self.scroll_offset_y = (self.scroll_offset_y + anchor_shift as f32).max(0.0);
            self.pending_scroll_y = Some(self.scroll_offset_y);
        }
        let vis_top = self.scroll_offset_y.round() as i32;
        let vis_bot = (self.scroll_offset_y + viewport_h).round() as i32;
        // Coming back down to the bottom of a chat with many pages loaded drops the oldest of
        // them, so the scroll bar is sized from what is around the bottom again (TGB-2952).
        if self.scrolled_up
            && self.messages.len() > MAX_LOADED_AT_BOTTOM
            && self.is_at_loaded_bottom(vis_bot)
            && !self.pending_scroll_to_bottom
            && self.pending_scroll_y.is_none()
        {
            self.unload_oldest_messages(KEEP_LOADED_AFTER_UNLOAD);
            self.pending_scroll_to_bottom = true;
            ui.ctx().request_repaint();
        }
        let total_h = self.list.total_height();
        self.last_scroll_metrics = ScrollMetrics {
            offset: self.scroll_offset_y,
            content_height: scroll_out.inner.content_size.y,
            viewport_height: scroll_out.inner.inner_rect.height(),
            list_height: total_h,
            loaded_messages: self.messages.len(),
        };
        self.scrolled_up = (total_h - vis_bot) > 120;

        // `markingContentsRead`: the window is focused and the user can see the chat (no box,
        // layer or overlay, not minimized, not idle; set by the window each frame).
        let window_focused = ui.ctx().input(|i| i.focused) && self.is_marking_as_read();

        // `HistoryInner::paintEvent` -> `Reactions::poll(item, now)`: the shown messages'
        // reactions are refreshed every `kPollEach` (the backend skips those not due).
        if !self.painted_msg_ids.is_empty() {
            backend.poll_painted_reactions(self.peer_id, &self.painted_msg_ids);
        }

        // `HistoryInner::paintEvent` marks shown messages with unread reactions as read
        // (`readContents`), which clears the floating "♥ N" button and the chat-row badge.
        // Only while the window is active (`markingAsViewed`), and only when the set changed.
        if self.painted_msg_ids != self.reactions_read_for && window_focused {
            self.reactions_read_for.clone_from(&self.painted_msg_ids);
            if !self.painted_msg_ids.is_empty()
                && let Err(err) =
                    backend.mark_visible_reactions_read(self.peer_id, &self.painted_msg_ids)
            {
                tracing::debug!("mark_visible_reactions_read failed: {err}");
            }
        }

        // Mark visible unread messages as read when the window is focused
        // (`HistoryWidget::newItemAdded` / `HistoryInner::paintEvent` -> `readInboxTill`).
        // Like tdesktop (`history_inner_widget.cpp:1513-1515`), reading visible messages clears
        // the unread counter without destroying the `"Unread messages"` divider bar while the
        // chat remains open (it is cleared on close, send, or new arrival at bottom).
        // Only a shown unread incoming item advances the watermark (`readTill`,
        // `history_inner_widget.cpp:1489-1557`): a visible outgoing message must not mark an
        // off-screen incoming one read.
        if window_focused
            && (self.unread_count > 0 || peer.unread_count > 0)
            && let Some(max_id) = self.read_till_shown
            && max_id > self.last_inbox_read_sent
        {
            self.last_inbox_read_sent = max_id;
            backend.mark_history_read(self.peer_id, max_id);
            if self.list.loaded_at_bottom && !self.scrolled_up {
                self.unread_count = 0;
            }
        }

        // Paint interactive emoji tap burst overlays above the message list.
        self.emoji_interactions
            .paint_overlays(ui, list_rect, now_ms.max(0) as u64, colors);

        // Automatic preload triggers (`HistoryList::check_preload`).
        let preload = self.list.check_preload(vis_top, vis_bot);
        if preload.preload_top {
            self.load_older_slice(backend, peer.summary.kind);
        } else if preload.preload_bottom {
            self.load_newer_slice(backend, peer.summary.kind);
        }

        // Handle reply-header jump clicks.
        if let Some(tid) = jump_to_msg {
            self.jump_to_message(tid, backend, peer.summary.kind, now_ms);
        }

        // Right-click context menu (`PopupMenuState` from `tg-ui`).
        let ui_colors = colors.ui_colors();
        let menu_style = PopupMenuStyle::default_popup(&ui_colors);
        let now_unix_secs = (now_ms / 1000).max(0);
        if let Some((mid, pos)) = open_context_menu_for {
            self.context_menu_msg = Some(mid);
            self.context_menu_pos = pos;
            let menu_msg = self
                .index_of(mid)
                .and_then(|index| self.messages.get(index));
            self.context_menu_engagement = menu_msg.map(|m| {
                crate::history::read_receipts::ContextMenuEngagementInfo::resolve(
                    peer,
                    m,
                    now_unix_secs,
                    backend,
                )
            });
            self.context_menu.open_at(ui.ctx(), pos, &menu_style);
            self.context_menu_opened_frame = ui.ctx().cumulative_frame_nr();
        }

        if let Some(target_id) = self.context_menu_msg
            && let Some(target_msg) = self
                .index_of(target_id)
                .and_then(|index| self.messages.get(index))
                .cloned()
        {
            let has_quote_sel = self.selected_quote_for_msg(target_id).is_some();
            let ref_now_unix = now_unix_secs.max(i64::from(target_msg.date_unix) + 60);
            let mut items = build_context_menu_items(&target_msg, peer, has_quote_sel);
            copy_selection::apply_upon_selected(&mut items, self.upon_selected(target_id));
            let allows_forwarding = backend.peer_allows_forwarding(self.peer_id);
            copy_restriction::drop_restricted_copy_items(
                &mut items,
                copy_restriction::restricts(allows_forwarding, &target_msg),
                copy_restriction::restricts_any(allows_forwarding, self.selection_messages()),
            );
            if let Some(ref eng) = self.context_menu_engagement
                && eng.peer_id == self.peer_id
                && eng.msg_id == target_id
            {
                eng.append_menu_items(&mut items, ref_now_unix);
            }
            // `Data::LookupPossibleReactions`: favourite first, then `messages.getTopReactions`,
            // then the rest of the available reactions allowed in this chat.
            let strip: Vec<String> = backend
                .reaction_strip_emojis(Some(self.peer_id))
                .into_iter()
                .take(CONTEXT_STRIP_MAX_REACTIONS)
                .collect();
            if let Some(triggered) = self.show_message_menu(ui.ctx(), &items, &strip, &menu_style) {
                if let Some(emoji) = triggered.strip_prefix(CONTEXT_STRIP_REACTION_PREFIX) {
                    events.push(InnerEvent::ToggleReaction {
                        msg_id: target_id,
                        emoji: emoji.to_owned(),
                    });
                }
                match triggered.as_str() {
                    crate::history::read_receipts::MENU_ID_SEEN_BY => {
                        let readers = self
                            .context_menu_engagement
                            .as_ref()
                            .and_then(|e| e.group_readers.clone())
                            .unwrap_or_default();
                        events.push(InnerEvent::OpenSeenBy {
                            msg_id: target_id,
                            readers,
                        });
                    }
                    crate::history::read_receipts::MENU_ID_READ_AT => {
                        if let Some(status) = self
                            .context_menu_engagement
                            .as_ref()
                            .and_then(|e| e.dm_read_date)
                        {
                            events.push(InnerEvent::OpenReadDateStatus(status));
                        }
                    }
                    crate::history::read_receipts::MENU_ID_MESSAGE_AUTHOR => {
                        if let Some(author) = self
                            .context_menu_engagement
                            .as_ref()
                            .and_then(|e| e.message_author.clone())
                        {
                            events.push(InnerEvent::OpenMessageAuthor(author));
                        }
                    }
                    "react_more" => events.push(InnerEvent::OpenReactionsPicker {
                        msg_id: target_id,
                        pos: self.context_menu_pos,
                    }),
                    "who_reacted" => events.push(InnerEvent::OpenWhoReacted(target_id)),
                    "toggle_paid_reaction_privacy" => {
                        events.push(InnerEvent::TogglePaidReactionPrivacy(target_id));
                    }
                    "report_msg_reaction" => {
                        events.push(InnerEvent::ReportMessageReaction(target_id));
                    }
                    "remove_participant_reactions" => {
                        events.push(InnerEvent::RemoveParticipantReactions {
                            msg_id: target_id,
                            participant_id: target_msg.from_id,
                        });
                    }
                    "poll_voters" => events.push(InnerEvent::OpenPollVoters(target_id)),
                    "retract_vote" => events.push(InnerEvent::RetractPollVote(target_id)),
                    "stop_poll" => events.push(InnerEvent::StopPoll(target_id)),
                    "reply" => events.push(InnerEvent::Reply(target_id)),
                    "draw_to_reply" => events.push(InnerEvent::DrawToReply(target_id)),
                    "reply_quote" => {
                        if let Some(quote) = self.selected_quote_for_msg(target_id) {
                            events.push(InnerEvent::ReplyWithQuote {
                                msg_id: target_id,
                                quote,
                            });
                        } else {
                            events.push(InnerEvent::Reply(target_id));
                        }
                    }
                    "open_comments" => events.push(InnerEvent::OpenComments(target_id)),
                    "edit" => events.push(InnerEvent::Edit(target_id)),
                    // `copyContextText`: the whole message; `copySelectedText`: the selection,
                    // and a copy of selected messages ends the selection.
                    "copy_text" | copy_selection::MENU_ID_COPY_SELECTED => {
                        let selected = triggered == copy_selection::MENU_ID_COPY_SELECTED;
                        let copy = if selected {
                            self.selection_copy()
                        } else {
                            copy_selection::items_copy(&[&target_msg])
                        };
                        if let Some(copy) = copy {
                            ui.ctx().copy_text(copy.clipboard);
                            events.push(InnerEvent::CopiedRich {
                                summary: String::new(),
                                rich: copy.rich,
                            });
                        }
                        if selected && self.list.selection.has_selected_items() {
                            self.clear_selection();
                        }
                    }
                    "copy_link" => {
                        events.push(InnerEvent::CopyMessageLink(target_id));
                    }
                    "translate" => {
                        if target_msg.extra.translated_text.is_some() {
                            events.push(InnerEvent::Translate {
                                msg_id: target_id,
                                clear: true,
                            });
                        } else {
                            events.push(InnerEvent::OpenTranslateModal(target_id));
                        }
                    }
                    "forward" => events.push(InnerEvent::Forward(vec![target_id])),
                    "pin" => events.push(InnerEvent::TogglePin {
                        msg_id: target_id,
                        unpin: target_msg.is_pinned,
                    }),
                    "select" => {
                        self.toggle_select_message(target_id);
                    }
                    "save_as" => {
                        let fname = suggested_save_name(&target_msg);
                        events.push(InnerEvent::SaveAs {
                            msg_id: target_id,
                            file_name: fname,
                        });
                    }
                    "save_gif" => events.push(InnerEvent::SaveGif(target_id)),
                    "fave_sticker" => events.push(InnerEvent::FaveSticker(target_id)),
                    "save_ringtone" => events.push(InnerEvent::SaveRingtone(target_id)),
                    "report" => events.push(InnerEvent::Report(vec![target_id])),
                    "moderate" => {
                        let target_full = to_hist_full(FullMsgId {
                            peer: self.peer_id,
                            msg: target_id,
                        });
                        let ids = if self.list.selection.selected_items.contains(&target_full) {
                            let mut selected: Vec<MsgId> = self
                                .messages
                                .iter()
                                .filter(|m| {
                                    can_moderate_message(m, peer)
                                        && self.list.selection.selected_items.contains(
                                            &to_hist_full(FullMsgId {
                                                peer: self.peer_id,
                                                msg: m.id,
                                            }),
                                        )
                                })
                                .map(|m| m.id)
                                .collect();
                            if selected.is_empty() {
                                selected.push(target_id);
                            }
                            selected
                        } else {
                            vec![target_id]
                        };
                        events.push(InnerEvent::Moderate(ids));
                    }
                    "block_from_replies" => {
                        let sender_name = if target_msg.sender_name.trim().is_empty() {
                            "User".to_owned()
                        } else {
                            target_msg.sender_name.clone()
                        };
                        events.push(InnerEvent::BlockFromReplies {
                            msg_id: target_id,
                            sender_name,
                        });
                    }
                    "delete" => events.push(InnerEvent::Delete(vec![target_id])),
                    _ => {}
                }
            }
        }

        events
    }

    fn update_date_and_unread_heights(&mut self) {
        let fonts = FontMetrics::default();
        let date_h = date_badge_height(&fonts);
        for (item, msg) in self.list.items_mut().iter_mut().zip(self.messages.iter()) {
            item.displayed_date_height = if item.attach_state.display_date {
                date_h
            } else {
                0
            };
            if let Some(cached) = self.layout_cache.get(&msg.id) {
                item.height = cached_layout_height(&cached.layout);
                continue;
            }
            // Re-estimate height so date badges and unread bars are accounted for before first paint.
            let mut est = estimate_initial_height(msg);
            if item.attach_state.display_date {
                est += date_h;
            }
            if item.has_unread_bar {
                est += unread_bar_height();
            }
            item.height = est;
        }
        self.list.relayout_positions();
    }

    /// Whether the next paint puts the "Unread messages" bar at the top of the view.
    #[must_use]
    pub const fn opens_at_unread_bar(&self) -> bool {
        self.pending_scroll_to_unread_bar
    }

    #[allow(clippy::too_many_arguments)]
    fn ensure_item_laid_out(
        &mut self,
        ui: &Ui,
        index: usize,
        viewport_w: i32,
        is_chat_wide: bool,
        peer: &HistoryPeerInfo,
        colors: &HistoryColors,
        fonts: &FontMetrics,
        factcheck_extra_h: i32,
        summary_entry: Option<&MessageSummaryEntry>,
    ) -> bool {
        let msg = &self.messages[index];
        let item = &self.list.items()[index];
        let show_sender_in_chat = matches!(peer.summary.kind, PeerKind::Group);
        let transcribe_h = expanded_transcribe_height(msg, &self.voice_state);
        let expanded_quotes = self
            .text_states
            .get(&msg.id)
            .map_or(&[][..], |state| &state.expanded_quotes);
        let cached = compute_item_layout_egui_with_quotes(
            ui,
            msg,
            item,
            viewport_w,
            is_chat_wide,
            show_sender_in_chat,
            colors,
            fonts,
            factcheck_extra_h,
            transcribe_h,
            summary_entry,
            expanded_quotes,
            self.large_emoji,
        );
        let new_h = cached_layout_height(&cached);
        let changed = self.list.items()[index].height != new_h;
        if changed {
            self.list.items_mut()[index].height = new_h;
        }
        self.layout_cache.insert(msg.id, CachedItem::new(cached));
        changed
    }

    fn sync_summary_state_for(&mut self, index: usize, backend: &dyn HistoryBackend) {
        let msg = &self.messages[index];
        if msg.extra.summary_from_language.is_none()
            && !self.summary_fingerprints.contains_key(&msg.id)
        {
            return;
        }
        let fp = backend
            .message_summary(self.peer_id, msg.id)
            .map(|e| (e.shown, e.loading, e.text.text.len()));
        if self.summary_fingerprints.get(&msg.id).copied() != fp {
            match fp {
                Some(v) => {
                    self.summary_fingerprints.insert(msg.id, v);
                }
                None => {
                    self.summary_fingerprints.remove(&msg.id);
                }
            }
            self.layout_cache.remove(&msg.id);
        }
    }

    fn sync_voice_transcription_for(&mut self, index: usize, backend: &dyn HistoryBackend) {
        let msg = &self.messages[index];
        if matches!(
            msg.media,
            Some(HistoryMedia::Voice(_) | HistoryMedia::RoundVideo(_))
        ) {
            let fetched = backend.transcribe_audio_state(self.peer_id, msg.id);
            let prev = self.voice_state.transcriptions.get(&msg.id);
            if prev != fetched.as_ref() {
                match fetched {
                    Some(entry) => {
                        self.voice_state.transcriptions.insert(msg.id, entry);
                    }
                    None => {
                        self.voice_state.transcriptions.remove(&msg.id);
                    }
                }
                self.layout_cache.remove(&msg.id);
            }
        }
    }

    /// The list items, in message order (tests).
    #[cfg(test)]
    pub(crate) fn list_items(&self) -> &[HistoryListItem] {
        self.list.items()
    }

    /// Recomputes which messages show a userpic (`hasFromPhoto` depends on the chat being wide)
    /// and the attachments that decide where it is drawn.
    fn refresh_from_photos(&mut self, is_group: bool) {
        let wide = self.layout_wide;
        for (item, msg) in self.list.items_mut().iter_mut().zip(&self.messages) {
            item.has_from_photo = has_from_photo(msg, is_group, wide);
        }
        self.list.recount_attachments();
    }

    fn factcheck_extra_height(&self, msg_id: MsgId, backend: &dyn HistoryBackend) -> i32 {
        backend
            .as_layer233_extras()
            .and_then(|ex| ex.cached_factcheck(self.peer_id, msg_id))
            .map_or(0, |fc| {
                crate::history::factcheck::factcheck_reserved_height(
                    &fc,
                    self.expanded_factchecks.contains(&msg_id),
                )
            })
    }
}

fn cached_layout_height(cached: &CachedItemLayout) -> i32 {
    match cached {
        CachedItemLayout::Message(m) => m.current_size.height.max(24),
        CachedItemLayout::Service(s) => s.current_size.height.max(24),
    }
}

fn build_list_item(
    msg: &HistoryMessage,
    is_group: bool,
    large_emoji: bool,
    chat_wide: bool,
) -> HistoryListItem {
    let full_id = to_hist_full(FullMsgId {
        peer: msg.peer_id,
        msg: msg.id,
    });
    let pos = MessagePosition {
        full_id,
        date: msg.date_unix,
    };
    let unwrapped = (matches!(
        msg.media,
        Some(HistoryMedia::Sticker(_) | HistoryMedia::RoundVideo(_))
    ) && msg.text.is_empty())
        || crate::history::large_emoji::large_emoji_of(msg, large_emoji).is_some();
    let meta = AttachMetadata {
        date: msg.date_unix,
        date_time: LocalDateTime::from_unix_local(i64::from(msg.date_unix)),
        from: to_hist_peer(msg.from_id),
        display_from: Some(to_hist_peer(msg.from_id)),
        has_out_layout: msg.outgoing && !msg.is_post,
        is_service: msg.is_service(),
        is_forwarded: msg.forward.is_some(),
        has_visible_inline_markup: msg
            .extra
            .reply_markup
            .as_ref()
            .is_some_and(|m| m.is_inline()),
        unwrapped,
        ..AttachMetadata::default()
    };
    let mut item = HistoryListItem::new(full_id, pos, meta, estimate_initial_height(msg));
    item.has_from_photo = has_from_photo(msg, is_group, chat_wide);
    item
}

/// `Message::hasFromPhoto` in the history: no userpic for service messages or channel posts; in
/// a wide chat (`ElementChatMode::Wide`, every message on the left) every message has one,
/// outgoing too; otherwise incoming messages of groups.
fn has_from_photo(msg: &HistoryMessage, is_group: bool, chat_wide: bool) -> bool {
    if msg.is_service() || msg.is_post {
        return false;
    }
    chat_wide || (is_group && !msg.outgoing)
}

fn estimate_initial_height(msg: &HistoryMessage) -> i32 {
    if let Some(ref svc) = msg.service {
        return match svc {
            ServiceAction::SuggestBirthday { .. } => {
                crate::history::suggestion_cards::measure_birthday_suggestion_card(msg.outgoing).1
                    + 16
            }
            ServiceAction::SuggestProfilePhoto { .. } => {
                crate::history::suggestion_cards::measure_userpic_suggestion_card().1 + 16
            }
            _ => 36,
        };
    }
    let (_, text_h) = estimate_text_metrics(&msg.text.text, 340);
    let media_h = match &msg.media {
        Some(HistoryMedia::Photo(_)) => 260,
        Some(HistoryMedia::Album(_)) => 320,
        Some(HistoryMedia::Video(_)) => 240,
        Some(HistoryMedia::Document(d)) => {
            if d.thumb_rgb.is_some() {
                86
            } else {
                64
            }
        }
        Some(HistoryMedia::Voice(_)) => 66,
        Some(HistoryMedia::Sticker(_)) => 180,
        Some(HistoryMedia::Poll(p)) => 110 + (p.options.len() as i32) * 38,
        Some(HistoryMedia::Location(_)) => 176,
        Some(HistoryMedia::Contact(_)) => 68,
        Some(HistoryMedia::RoundVideo(rv)) => rv.diameter_px.clamp(180, 240),
        Some(HistoryMedia::Game(_)) => 265,
        Some(HistoryMedia::TodoList(todo)) => {
            crate::history::todo::measure_todo_list_size(todo, 340).1
        }
        Some(HistoryMedia::Widget(w)) => {
            if w.height > 0 {
                w.height
                    .clamp(tg_tgw::TGW_MIN_HEIGHT as i32, tg_tgw::TGW_MAX_HEIGHT as i32)
            } else {
                tg_tgw::TGW_PLACEHOLDER_HEIGHT
            }
        }
        Some(HistoryMedia::Story(_)) => 106,
        Some(HistoryMedia::Giveaway(_)) => 210,
        Some(HistoryMedia::GiveawayResults(_)) => 185,
        Some(HistoryMedia::VideoStream(_)) => 184,
        None => 0,
    };
    let header_h =
        (if msg.reply.is_some() { 42 } else { 0 }) + (if msg.forward.is_some() { 20 } else { 0 });
    let react_h = if msg.reactions.is_empty() { 0 } else { 30 };
    let comm_h = if msg.is_post && msg.extra.replies_count.is_some() {
        40
    } else {
        0
    };
    let kb_h = msg
        .extra
        .reply_markup
        .as_ref()
        .filter(|m| m.is_inline())
        .map_or(0, |m| (m.rows.len() as i32) * 42 + 4);
    let lp_h = msg
        .extra
        .link_preview
        .as_ref()
        .map_or(0, |lp| link_preview_card_height(lp) + 8);
    (text_h + media_h + header_h + react_h + comm_h + kb_h + lp_h + 36).max(48)
}

fn expanded_transcribe_height(msg: &HistoryMessage, voice_state: &VoicePlaybackState) -> i32 {
    if let Some(entry) = voice_state.transcriptions.get(&msg.id) {
        if !entry.shown {
            return 0;
        }
        let failure;
        let text = if entry.failed || entry.too_long {
            failure = tg_app_core::user_error::transcribe_bubble_text(entry.too_long);
            failure.as_str()
        } else if entry.pending && entry.text.is_empty() {
            "Transcribing..."
        } else {
            entry.text.as_str()
        };
        let (_, h) = estimate_text_metrics(text, 240);
        let rate_h = if entry.can_rate { 24 } else { 0 };
        return h.max(20) + rate_h + 12;
    }
    if let Some(HistoryMedia::Voice(v)) = &msg.media
        && voice_state
            .transcription_expanded
            .get(&msg.id)
            .copied()
            .unwrap_or(false)
        && let Some(ref txt) = v.transcription
    {
        let (_, h) = estimate_text_metrics(txt, 240);
        return h.max(20) + 10;
    }
    0
}

/// `Element::textualMaxWidth` (`history_view_element.cpp:2935`): the caption's single-line width
/// with the bubble paddings, `0` without a caption. A caption under displayed media carries no
/// date skip block (`Message::validateTextSkipBlock`), so the time is not counted here.
fn media_textual_max_width(has_text: bool, text_max_width: i32) -> i32 {
    if has_text {
        MSG_PADDING.left + text_max_width.max(0) + MSG_PADDING.right
    } else {
        0
    }
}

/// `UnwrappedMedia` around `LargeEmoji` for an emoji-only message; `info_width` is the bottom
/// info's optimal width (`Element::infoWidth`).
fn large_emoji_media_input(
    emoji: &crate::history::large_emoji::LargeEmoji,
    info_width: i32,
) -> MediaInput {
    emoji.media_input(unwrapped_bottom_info_width(info_width))
}

/// `UnwrappedMedia::bottomInfoWidth` for the bottom info's optimal width.
fn unwrapped_bottom_info_width(info_width: i32) -> i32 {
    tg_history::consts::MSG_REPLY_PADDING.left
        + info_width
        + 2 * tg_history::consts::MSG_DATE_IMG_PADDING.x
}

fn build_media_input(
    msg: &HistoryMessage,
    has_header: bool,
    has_text: bool,
    text_max_width: i32,
    bottom_info_w: i32,
    info_width: i32,
    transcribe_text_height: i32,
) -> Option<MediaInput> {
    let media = msg.media.as_ref()?;
    let caption_w = media_textual_max_width(has_text, text_max_width);
    Some(match media {
        HistoryMedia::Photo(p) => MediaInput::Photo(PhotoInput {
            dimensions: Size::new(p.width, p.height),
            service_width: 0,
            has_bubble: has_header || has_text,
            min_width_for_media: bottom_info_w + 24,
            textual_max_width: caption_w,
            is_extended_video: false,
            hosted_instant_view: false,
        }),
        HistoryMedia::Album(items) => MediaInput::Grouped(GroupedMediaInput {
            mode: GroupedMode::Grid,
            part_sizes: items
                .iter()
                .map(|it| Size::new(it.width, it.height))
                .collect(),
            is_bubble_top: !has_header,
            is_bubble_bottom: !has_text,
            last_item_has_text: has_text,
            column_extra_bottom_info: false,
            hosted_instant_view: false,
        }),
        HistoryMedia::Video(v) => MediaInput::Gif(GifInput {
            kind: if v.is_gif {
                GifKind::Gif
            } else {
                GifKind::VideoFile
            },
            dimensions: Size::new(v.width, v.height),
            has_bubble: has_header || has_text,
            min_width_for_media: bottom_info_w + 24,
            info_width: bottom_info_w,
            hides_bottom_info: false,
            textual_max_width: caption_w,
            max_status_width: 110,
            is_streaming: false,
            unwrapped_additional_width: 0,
            reactions_optimal_width: 0,
            hosted_instant_view: false,
        }),
        HistoryMedia::Document(d) => {
            let status_w = (format_file_size(d.size_bytes).len() as i32) * 7;
            MediaInput::Document(DocumentInput {
                kind: if d.thumb_rgb.is_some() {
                    DocumentKind::Thumbed
                } else {
                    DocumentKind::File
                },
                is_bubble_top: !has_header,
                is_bubble_bottom: !has_text,
                name_width: (d.file_name.chars().count() as i32) * 8,
                max_status_width: status_w.max(60),
                status_width: status_w.max(60),
                thumbed_link_width: if d.thumb_rgb.is_some() { 64 } else { 0 },
                is_unread_media: false,
                has_transcribe_button: false,
                transcribe_text_height: 0,
                skip_block_width: bottom_info_w + 12,
                bottom_info_first_line_width: bottom_info_w,
            })
        }
        HistoryMedia::Voice(v) => {
            let status_w = (format_duration(v.duration_secs).len() as i32) * 7 + 8;
            MediaInput::Document(DocumentInput {
                kind: DocumentKind::Voice,
                is_bubble_top: !has_header,
                is_bubble_bottom: !has_text,
                name_width: 0,
                max_status_width: status_w,
                status_width: status_w,
                thumbed_link_width: 0,
                is_unread_media: v.is_unread,
                has_transcribe_button: true,
                transcribe_text_height,
                skip_block_width: bottom_info_w + 12,
                bottom_info_first_line_width: bottom_info_w,
            })
        }
        HistoryMedia::Sticker(s) => MediaInput::Sticker(StickerInput {
            kind: StickerKind::Regular,
            dimensions: Size::new(s.width, s.height),
            custom_size: 168,
            option_sticker_size: None,
            bottom_info_width: Some(unwrapped_bottom_info_width(info_width)),
        }),
        HistoryMedia::Poll(p) => {
            let (qw, qh) = estimate_text_metrics(&p.question, 300);
            let answers = p
                .options
                .iter()
                .map(|o| {
                    let (ow, oh) = estimate_text_metrics(&o.text, 260);
                    PollAnswerInput {
                        text_width: ow,
                        text_height: oh.max(20),
                    }
                })
                .collect();
            MediaInput::Poll(PollInput {
                question_width: qw,
                question_height: qh.max(20),
                description_width: 0,
                description_height: 0,
                answers,
                any_answer_has_media: false,
                answer_media_width: 0,
                has_bottom_button: false,
            })
        }
        HistoryMedia::Location(_) => MediaInput::Photo(PhotoInput {
            dimensions: Size::new(280, 170),
            service_width: 0,
            has_bubble: true,
            min_width_for_media: 240,
            textual_max_width: 280,
            is_extended_video: false,
            hosted_instant_view: false,
        }),
        HistoryMedia::Contact(c) => MediaInput::Document(DocumentInput {
            kind: DocumentKind::File,
            is_bubble_top: !has_header,
            is_bubble_bottom: !has_text,
            name_width: (c.full_name().chars().count() as i32) * 8 + 40,
            max_status_width: (c.phone_number.len() as i32) * 7 + 40,
            status_width: (c.phone_number.len() as i32) * 7 + 40,
            thumbed_link_width: 0,
            is_unread_media: false,
            has_transcribe_button: false,
            transcribe_text_height: 0,
            skip_block_width: bottom_info_w + 12,
            bottom_info_first_line_width: bottom_info_w,
        }),
        HistoryMedia::RoundVideo(rv) if transcribe_text_height > 0 => {
            let side = rv
                .diameter_px
                .min(tg_history::consts::MAX_VIDEO_MESSAGE_SIZE);
            MediaInput::Sticker(StickerInput {
                kind: StickerKind::Regular,
                dimensions: Size::new(side, side + transcribe_text_height),
                custom_size: side + transcribe_text_height,
                option_sticker_size: None,
                bottom_info_width: None,
            })
        }
        // `HistoryView::Gif` for a video message: a bare circle of `st::maxVideoMessageSize`.
        HistoryMedia::RoundVideo(rv) => MediaInput::Gif(GifInput {
            kind: GifKind::VideoMessage,
            dimensions: Size::new(rv.diameter_px.max(1), rv.diameter_px.max(1)),
            has_bubble: has_text,
            min_width_for_media: 0,
            info_width: bottom_info_w,
            hides_bottom_info: false,
            textual_max_width: 0,
            max_status_width: 0,
            is_streaming: true,
            unwrapped_additional_width: 0,
            reactions_optimal_width: measure_reactions_size(
                &msg.reactions,
                matches!(msg.peer_id.kind(), PeerKind::User),
            )
            .width,
            hosted_instant_view: false,
        }),
        HistoryMedia::Game(g) => {
            let (tw, th) = estimate_text_metrics(&g.game.title, 280);
            let title_lines = if g.game.title.is_empty() {
                0
            } else if th > 22 {
                2
            } else {
                1
            };
            let (dw, dh) = estimate_text_metrics(&g.game.description, 280);
            let aspect_w = g.media_width.max(1);
            let aspect_h = g.media_height.max(1);
            let attach_w = 280;
            let attach_h = ((attach_w * aspect_h) / aspect_w).clamp(110, 175);
            MediaInput::WebPage(WebPageInput {
                pending_or_failed: false,
                as_article: false,
                site_name_width: 0,
                title_width: tw.max(120),
                title_lines,
                description_width: dw,
                description_height: dh,
                article_photo_size: Size::ZERO,
                attach_size: Size::new(attach_w, attach_h),
                open_button_width: 0,
                skip_block_width: bottom_info_w + 12,
                is_bubble_bottom: !has_text,
            })
        }
        HistoryMedia::TodoList(todo) => {
            let (tw, th) = estimate_text_metrics(&todo.title.text, 300);
            let answers = todo
                .items
                .iter()
                .map(|it| {
                    let (iw, ih) = estimate_text_metrics(&it.text.text, 260);
                    PollAnswerInput {
                        text_width: iw.max(120),
                        text_height: ih.max(22),
                    }
                })
                .collect();
            MediaInput::Poll(PollInput {
                question_width: tw.max(180),
                question_height: th.max(22),
                description_width: 0,
                description_height: 0,
                answers,
                any_answer_has_media: false,
                answer_media_width: 0,
                has_bottom_button: true,
            })
        }
        HistoryMedia::Widget(w) => {
            let card_w = if w.width > 0 {
                w.width.clamp(tg_tgw::TGW_MIN_WIDTH as i32, 340)
            } else {
                320
            };
            let card_h = if w.height > 0 {
                w.height
                    .clamp(tg_tgw::TGW_MIN_HEIGHT as i32, tg_tgw::TGW_MAX_HEIGHT as i32)
            } else {
                tg_tgw::TGW_PLACEHOLDER_HEIGHT
            };
            MediaInput::Photo(PhotoInput {
                dimensions: Size::new(card_w, card_h),
                service_width: 0,
                has_bubble: true,
                min_width_for_media: card_w.max(bottom_info_w + 24),
                textual_max_width: if has_text { 320 } else { 0 },
                is_extended_video: false,
                hosted_instant_view: false,
            })
        }
        HistoryMedia::Story(_) => MediaInput::Photo(PhotoInput {
            dimensions: Size::new(280, 106),
            service_width: 0,
            has_bubble: true,
            min_width_for_media: 260_i32.max(bottom_info_w + 24),
            textual_max_width: if has_text { 280 } else { 0 },
            is_extended_video: false,
            hosted_instant_view: false,
        }),
        HistoryMedia::Giveaway(_) => MediaInput::Photo(PhotoInput {
            dimensions: Size::new(310, 210),
            service_width: 0,
            has_bubble: true,
            min_width_for_media: 280_i32.max(bottom_info_w + 24),
            textual_max_width: if has_text { 310 } else { 0 },
            is_extended_video: false,
            hosted_instant_view: false,
        }),
        HistoryMedia::GiveawayResults(_) => MediaInput::Photo(PhotoInput {
            dimensions: Size::new(310, 185),
            service_width: 0,
            has_bubble: true,
            min_width_for_media: 280_i32.max(bottom_info_w + 24),
            textual_max_width: if has_text { 310 } else { 0 },
            is_extended_video: false,
            hosted_instant_view: false,
        }),
        HistoryMedia::VideoStream(_) => MediaInput::Photo(PhotoInput {
            dimensions: Size::new(300, 184),
            service_width: 0,
            has_bubble: true,
            min_width_for_media: 260_i32.max(bottom_info_w + 24),
            textual_max_width: if has_text { 300 } else { 0 },
            is_extended_video: false,
            hosted_instant_view: false,
        }),
    })
}

fn build_bottom_info_input(msg: &HistoryMessage) -> BottomInfoInput {
    let mut time_chars = message_time_text(msg).len() as i32;
    if msg.edited {
        time_chars += 8;
    }
    let views_w = msg
        .views
        .map_or(0, |v| (format_compact_views(v).len() as i32) * 7);
    BottomInfoInput {
        author_edited_date_width: time_chars * 7 + 6,
        author_width: 0,
        after_author_width: 0,
        views_text_width: views_w,
        replies_text_width: 0,
        out_layout: msg.outgoing && !msg.is_post,
        sending: msg.is_sending,
        pinned: msg.is_pinned,
        silent: false,
        ephemeral: false,
        has_effect: false,
        sponsored: false,
        shortcut: false,
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn compute_item_layout_egui(
    ui: &Ui,
    msg: &HistoryMessage,
    item: &HistoryListItem,
    viewport_w: i32,
    is_chat_wide: bool,
    show_sender_in_chat: bool,
    colors: &HistoryColors,
    fonts: &FontMetrics,
    factcheck_extra_h: i32,
    transcribe_text_height: i32,
    summary_entry: Option<&MessageSummaryEntry>,
) -> CachedItemLayout {
    compute_item_layout_egui_with_quotes(
        ui,
        msg,
        item,
        viewport_w,
        is_chat_wide,
        show_sender_in_chat,
        colors,
        fonts,
        factcheck_extra_h,
        transcribe_text_height,
        summary_entry,
        &[],
        true,
    )
}

/// The layout of one item; `expanded_quotes` are the message's expanded collapsible quotes.
#[allow(clippy::too_many_arguments)]
fn compute_item_layout_egui_with_quotes(
    ui: &Ui,
    msg: &HistoryMessage,
    item: &HistoryListItem,
    viewport_w: i32,
    is_chat_wide: bool,
    show_sender_in_chat: bool,
    colors: &HistoryColors,
    fonts: &FontMetrics,
    factcheck_extra_h: i32,
    transcribe_text_height: i32,
    summary_entry: Option<&MessageSummaryEntry>,
    expanded_quotes: &[usize],
    large_emoji: bool,
) -> CachedItemLayout {
    let chat_mode = if is_chat_wide {
        ElementChatMode::Wide
    } else {
        ElementChatMode::Default
    };

    if let Some(ref svc) = msg.service {
        let card_dims = match svc {
            ServiceAction::NoForwardsRequest { expired, .. }
                if crate::history::no_forwards_request::should_render_no_forwards_request_card(
                    *expired, false,
                ) =>
            {
                Some(
                    crate::history::no_forwards_request::measure_no_forwards_request_card(
                        *expired,
                        msg.outgoing,
                        false,
                    ),
                )
            }
            ServiceAction::SuggestBirthday { .. } => Some(
                crate::history::suggestion_cards::measure_birthday_suggestion_card(msg.outgoing),
            ),
            ServiceAction::SuggestProfilePhoto { .. } => {
                Some(crate::history::suggestion_cards::measure_userpic_suggestion_card())
            }
            _ => None,
        };
        if let Some((card_w, card_h)) = card_dims {
            let input = ServiceInput {
                chat_mode,
                display_date: item.attach_state.display_date,
                has_unread_bar: item.has_unread_bar,
                media: Some(MediaInput::Photo(PhotoInput {
                    dimensions: Size::new(card_w, card_h),
                    service_width: card_w,
                    has_bubble: false,
                    min_width_for_media: card_w,
                    textual_max_width: 0,
                    is_extended_video: false,
                    hosted_instant_view: false,
                })),
                media_hides_service_text: true,
                ..ServiceInput::default()
            };
            return CachedItemLayout::Service(Box::new(layout_service_message(
                &input, viewport_w, fonts,
            )));
        }
        let text = crate::history::bubble::service_text(msg, svc);
        let max_w = (viewport_w - 60).max(120) as f32;
        let single_w = ui
            .painter()
            .layout_no_wrap(
                text.clone(),
                crate::history::bubble::service_font(),
                colors.service_fg,
            )
            .size()
            .x
            .ceil() as i32;
        let wrap_h = ui
            .painter()
            .layout(
                text,
                crate::history::bubble::service_font(),
                colors.service_fg,
                max_w,
            )
            .size()
            .y
            .ceil() as i32;
        let input = ServiceInput {
            chat_mode,
            display_date: item.attach_state.display_date,
            has_unread_bar: item.has_unread_bar,
            text_max_width: single_w,
            text_min_height: fonts.semibold.height,
            text_wrapped_height: wrap_h,
            ..ServiceInput::default()
        };
        return CachedItemLayout::Service(Box::new(layout_service_message(
            &input, viewport_w, fonts,
        )));
    }

    let text_pal = colors.text_palette(msg.outgoing && !msg.is_post);
    let max_text_wrap = (bubble_content_max_width(viewport_w, item.has_from_photo)
        - MSG_PADDING.horizontal())
    .max(120);
    let summary_h = crate::history::summary_header::summary_reserved_height(
        msg.extra.summary_from_language.as_deref(),
        summary_entry,
    );
    let summary_loading_fallback = tg_text::TextWithEntities::simple("Summarizing...");
    let active_text: &tg_text::TextWithEntities = if let Some(s) = summary_entry
        && s.shown
    {
        if !s.text.is_empty() {
            &s.text
        } else if s.loading {
            &summary_loading_fallback
        } else {
            &msg.text
        }
    } else {
        &msg.text
    };
    let has_rich_body = msg.extra.rich_message.is_some();
    let summary_shown = summary_entry.is_some_and(|s| s.shown);
    let large = (!summary_shown)
        .then(|| crate::history::large_emoji::large_emoji_of(msg, large_emoji))
        .flatten();
    let (mut text_max_w, text_wrapped_h, mut text_real_w) =
        if let Some(ref rich_payload) = msg.extra.rich_message {
            let view_style =
                tg_app_media::webview::rich_page_view::RichPageViewStyle::from_history_colors(
                    colors,
                    msg.outgoing && !msg.is_post,
                );
            let sz = tg_app_media::webview::formula_widget::with_math_engine(|engine| {
                tg_app_media::webview::rich_page_view::measure_rich_page(
                    ui.painter(),
                    engine,
                    &rich_payload.page,
                    max_text_wrap as f32,
                    &view_style,
                )
            });
            let w = (sz.x.ceil() as i32).clamp(80, max_text_wrap);
            let h = (sz.y.ceil() as i32).max(fonts.normal.height);
            (w, h, w)
        } else if active_text.is_empty() || large.is_some() {
            (0, 0, 0)
        } else {
            measure_message_text(
                ui,
                active_text,
                &text_pal,
                colors.text_fg,
                max_text_wrap as f32,
                expanded_quotes,
            )
        };
    if summary_h > 0 {
        let min_summary_w = 216.min(max_text_wrap);
        text_max_w = text_max_w.max(min_summary_w);
        text_real_w = text_real_w.max(min_summary_w);
    }

    let show_name = shows_from_name(
        msg,
        show_sender_in_chat,
        item.attach_state.attached_to_previous,
    );
    // `performCountOptimalSize`: the name with the status room, and the rank badge.
    let (name_line_w, right_badge_width) = if show_name {
        crate::history::name_line::name_line_widths(ui.painter(), msg)
    } else {
        (0, 0)
    };
    let from_name_width = show_name.then_some(name_line_w);
    let via_bot_width = crate::history::name_line::via_bot_width(ui.painter(), msg, show_name);
    let forwarded_width = msg.forward.as_ref().map(|f| {
        ui.painter()
            .layout_no_wrap(
                crate::history::name_line::forwarded_header_text(msg, f),
                FontId::proportional(12.5),
                colors.text_fg,
            )
            .size()
            .x
            .ceil() as i32
    });
    let reply = msg.reply.as_ref().map(|r| ReplyInput {
        name_width: (r.author_name.chars().count() as i32) * 7,
        text_width: (r.preview_text.chars().count() as i32) * 7,
        text_wrapped_height: fonts.normal.height,
        has_preview: r.preview_thumb_rgb.is_some(),
        multiline: false,
        expanded: false,
        name_icons_skip: 0,
        outline_width: 3,
    });

    let has_header = from_name_width.is_some() || forwarded_width.is_some() || reply.is_some();
    let bottom_info = crate::history::bubble_info::bottom_info_input(ui.painter(), msg);
    let bottom_info_w =
        build_bottom_info_input(msg).author_edited_date_width + bottom_info.views_text_width + 24;
    let info_optimal_w = tg_history::BottomInfoLayout::compute(&bottom_info, i32::MAX, fonts)
        .optimal_size
        .width;
    let info_skip_w = tg_history::skip_block_width(info_optimal_w);
    let lp_extra_h = msg
        .extra
        .link_preview
        .as_ref()
        .map_or(0, |lp| link_preview_card_height(lp) + 8);
    let has_lp = lp_extra_h > 0;
    let has_body_text = has_rich_body || (!msg.text.is_empty() && large.is_none()) || has_lp;
    let text_max_w = if has_lp {
        text_max_w.max(240.min(max_text_wrap))
    } else {
        text_max_w
    };
    let text_real_w = if has_lp {
        text_real_w.max(240.min(max_text_wrap))
    } else {
        text_real_w
    };
    let media = build_media_input(
        msg,
        has_header,
        has_body_text,
        text_max_w,
        bottom_info_w,
        info_optimal_w,
        transcribe_text_height,
    )
    .or_else(|| {
        large
            .as_ref()
            .map(|emoji| large_emoji_media_input(emoji, info_optimal_w))
    });

    // Extra vertical space so bottom-right timestamp never overlaps the final text line or preview card.
    // tdesktop appends a skip block for the date to the last text line (`Ui::Text::String`
    // `updateSkipBlock`): the time drops to its own line only when that line is full.
    let needs_date_line_room = has_body_text && msg.reactions.is_empty();
    let time_fits_last_line = || {
        if ends_with_quote(active_text) {
            return false;
        }
        let line_h = message_text_style().line_height.round() as i32;
        let (last_w, row_w) = if text_wrapped_h <= line_h + 1 {
            (text_max_w, max_text_wrap)
        } else {
            let last = last_line_width(ui, active_text, &text_pal, colors.text_fg, max_text_wrap);
            (last, text_real_w)
        };
        last_w + info_skip_w <= row_w
    };
    let text_block_h = |wrapped_h: i32, time_fits: bool| {
        // A fact-check card ends the text block: the time gets a line of its own below it.
        let date_line = if needs_date_line_room
            && (has_rich_body || has_lp || summary_h > 0 || factcheck_extra_h > 0 || !time_fits)
        {
            fonts.normal.height
        } else {
            0
        };
        wrapped_h + summary_h + lp_extra_h + date_line + factcheck_extra_h
    };
    let adjusted_text_h = text_block_h(
        text_wrapped_h,
        !needs_date_line_room || time_fits_last_line(),
    );

    let inline_keyboard = msg
        .extra
        .reply_markup
        .as_ref()
        .filter(|m| m.is_inline() && !m.rows.is_empty())
        .map(|markup| {
            let kb_font = FontId::proportional(13.0);
            let mut kb_layout = tg_bots::keyboard_layout::ButtonsLayout::from_markup(
                markup,
                tg_bots::keyboard_layout::BotKeyboardMetrics::INLINE,
                |text| {
                    ui.painter()
                        .layout_no_wrap(text.to_owned(), kb_font.clone(), Color32::WHITE)
                        .size()
                        .x
                        .ceil() as i32
                },
            );
            let kb_max_w = bubble_content_max_width(viewport_w, item.has_from_photo).max(200);
            let kb_w = kb_layout.natural_width().clamp(200, kb_max_w);
            let kb_h = kb_layout.natural_height();
            kb_layout.resize(kb_w, kb_h);
            Size::new(kb_w, kb_layout.height().max(kb_h))
        });

    let input = MessageInput {
        is_out: msg.outgoing,
        is_post: msg.is_post,
        has_from_photo: item.has_from_photo,
        chat_mode,
        attach: item.attach_state,
        has_unread_bar: item.has_unread_bar,
        from_name_width,
        right_badge_width,
        via_bot_width,
        forwarded_width,
        reply,
        text_max_width: text_max_w,
        text_min_height: if !has_body_text {
            0
        } else {
            fonts.normal.height
        },
        text_wrapped_height: adjusted_text_h,
        text_real_width: text_real_w,
        media,
        invert_media: matches!(
            msg.media,
            Some(
                HistoryMedia::Photo(_)
                    | HistoryMedia::Album(_)
                    | HistoryMedia::Video(_)
                    | HistoryMedia::Document(_)
            )
        ),
        bottom_info,
        reaction_button_widths: crate::history::bubble_info::reaction_chip_widths(
            ui.painter(),
            &msg.reactions,
            matches!(msg.peer_id.kind(), PeerKind::User).then_some(
                &crate::history::bubble::reaction_userpics_chip_width
                    as &dyn Fn(&crate::history::backend::ReactionCounter) -> i32,
            ),
        ),
        reactions_in_bubble: true,
        has_comments_button: msg.is_post && msg.extra.replies_count.is_some(),
        comments_text_width: msg.extra.replies_count.map_or(0, |c| {
            let label_len = if c == 1 {
                "1 comment".len()
            } else {
                format!("{c} comments").len()
            };
            (label_len as i32) * 7 + 16
        }),
        inline_keyboard,
        ..MessageInput::default()
    };

    let mut layout = layout_message(&input, viewport_w, fonts);
    // tdesktop measures the text height at the width the bubble really got
    // (`Message::resizeGetHeight` -> `Ui::Text::String::countHeight(textWidth)`). The first pass
    // above wrapped at the widest possible bubble, but media (a photo caption) or a reply can
    // make the bubble narrower, so the painted caption wraps into more lines than were measured
    // and spills below the bubble. Re-measure at the painted width and lay out again; the
    // width does not depend on the text height, so the second pass keeps the same geometry.
    let painted_wrap = layout.text_rect.width;
    if !has_rich_body && !active_text.is_empty() && painted_wrap > 0 && painted_wrap < max_text_wrap
    {
        let (_, wrapped_h, _) = measure_message_text(
            ui,
            active_text,
            &text_pal,
            colors.text_fg,
            painted_wrap as f32,
            expanded_quotes,
        );
        let time_fits = !needs_date_line_room
            || !ends_with_quote(active_text)
                && last_line_fits_time(
                    ui,
                    active_text,
                    &text_pal,
                    colors.text_fg,
                    painted_wrap,
                    wrapped_h,
                    text_max_w,
                    info_skip_w,
                );
        let painted_h = text_block_h(wrapped_h, time_fits);
        if painted_h != input.text_wrapped_height {
            let input = MessageInput {
                text_wrapped_height: painted_h,
                ..input
            };
            layout = layout_message(&input, viewport_w, fonts);
        }
    }
    CachedItemLayout::Message(Box::new(layout))
}

/// Whether the bottom info (time, ticks) fits after the last line of `text` wrapped at `wrap`
/// px, like tdesktop's skip block (`Ui::Text::String::updateSkipBlock`): otherwise the time gets
/// a line of its own.
#[allow(clippy::too_many_arguments)]
fn last_line_fits_time(
    ui: &Ui,
    text: &tg_text::TextWithEntities,
    palette: &tg_ui::stand_in::TextPalette,
    base_color: Color32,
    wrap: i32,
    wrapped_h: i32,
    single_line_w: i32,
    info_skip_w: i32,
) -> bool {
    let line_h = message_text_style().line_height.round() as i32;
    let last_w = if wrapped_h <= line_h + 1 {
        single_line_w
    } else {
        last_line_width(ui, text, palette, base_color, wrap)
    };
    last_w + info_skip_w <= wrap
}

fn compute_item_layout_headless(
    msg: &HistoryMessage,
    item: &HistoryListItem,
    viewport_w: i32,
    is_chat_wide: bool,
    show_sender_in_chat: bool,
    fonts: &FontMetrics,
    large_emoji: bool,
) -> CachedItemLayout {
    let chat_mode = if is_chat_wide {
        ElementChatMode::Wide
    } else {
        ElementChatMode::Default
    };
    if let Some(ref svc) = msg.service {
        let card_dims = match svc {
            ServiceAction::NoForwardsRequest { expired, .. }
                if crate::history::no_forwards_request::should_render_no_forwards_request_card(
                    *expired, false,
                ) =>
            {
                Some(
                    crate::history::no_forwards_request::measure_no_forwards_request_card(
                        *expired,
                        msg.outgoing,
                        false,
                    ),
                )
            }
            ServiceAction::SuggestBirthday { .. } => Some(
                crate::history::suggestion_cards::measure_birthday_suggestion_card(msg.outgoing),
            ),
            ServiceAction::SuggestProfilePhoto { .. } => {
                Some(crate::history::suggestion_cards::measure_userpic_suggestion_card())
            }
            _ => None,
        };
        if let Some((card_w, card_h)) = card_dims {
            let input = ServiceInput {
                chat_mode,
                display_date: item.attach_state.display_date,
                has_unread_bar: item.has_unread_bar,
                media: Some(MediaInput::Photo(PhotoInput {
                    dimensions: Size::new(card_w, card_h),
                    service_width: card_w,
                    has_bubble: false,
                    min_width_for_media: card_w,
                    textual_max_width: 0,
                    is_extended_video: false,
                    hosted_instant_view: false,
                })),
                media_hides_service_text: true,
                ..ServiceInput::default()
            };
            return CachedItemLayout::Service(Box::new(layout_service_message(
                &input, viewport_w, fonts,
            )));
        }
        let text = crate::history::bubble::service_text(msg, svc);
        let (w, h) = estimate_text_metrics(&text, (viewport_w - 60).max(120));
        let input = ServiceInput {
            chat_mode,
            display_date: item.attach_state.display_date,
            has_unread_bar: item.has_unread_bar,
            text_max_width: w,
            text_min_height: fonts.semibold.height,
            text_wrapped_height: h,
            ..ServiceInput::default()
        };
        return CachedItemLayout::Service(Box::new(layout_service_message(
            &input, viewport_w, fonts,
        )));
    }
    let max_text_wrap = (bubble_content_max_width(viewport_w, item.has_from_photo)
        - MSG_PADDING.horizontal())
    .max(120);
    let has_rich_body = msg.extra.rich_message.is_some();
    let large = crate::history::large_emoji::large_emoji_of(msg, large_emoji);
    let (tw, th) = if let Some(ref rich_payload) = msg.extra.rich_message {
        let sz = tg_app_media::webview::rich_page_view::estimate_rich_page_size(
            &rich_payload.page,
            max_text_wrap as f32,
            &tg_app_media::webview::rich_page_view::RichPageViewStyle::day(),
        );
        (
            (sz.x.ceil() as i32).clamp(80, max_text_wrap),
            (sz.y.ceil() as i32).max(fonts.normal.height) + 18,
        )
    } else if large.is_some() {
        (0, 0)
    } else {
        estimate_text_metrics(&msg.text.text, max_text_wrap)
    };
    let show_name = shows_from_name(
        msg,
        show_sender_in_chat,
        item.attach_state.attached_to_previous,
    );
    let from_name_width = show_name.then(|| (msg.sender_name.chars().count() as i32) * 8);
    let via_bot_width =
        crate::history::name_line::via_text(msg).map(|t| (t.chars().count() as i32) * 8);
    let forwarded_width = msg.forward.as_ref().map(|f| {
        (crate::history::name_line::forwarded_header_text(msg, f)
            .chars()
            .count() as i32)
            * 7
    });
    let reply = msg.reply.as_ref().map(|r| ReplyInput {
        name_width: (r.author_name.chars().count() as i32) * 7,
        text_width: (r.preview_text.chars().count() as i32) * 7,
        text_wrapped_height: fonts.normal.height,
        has_preview: r.preview_thumb_rgb.is_some(),
        ..ReplyInput::default()
    });
    let has_header = from_name_width.is_some() || forwarded_width.is_some() || reply.is_some();
    let bottom_info = build_bottom_info_input(msg);
    let bottom_info_w = bottom_info.author_edited_date_width + bottom_info.views_text_width + 24;
    let lp_extra_h = msg
        .extra
        .link_preview
        .as_ref()
        .map_or(0, |lp| link_preview_card_height(lp) + 8);
    let has_lp = lp_extra_h > 0;
    let has_body_text = has_rich_body || (!msg.text.is_empty() && large.is_none()) || has_lp;
    let tw = if has_lp {
        tw.max(240.min(max_text_wrap))
    } else {
        tw
    };
    let th = th + lp_extra_h + if has_lp { 18 } else { 0 };
    let info_w = tg_history::BottomInfoLayout::compute(&bottom_info, i32::MAX, fonts)
        .optimal_size
        .width;
    let media = build_media_input(msg, has_header, has_body_text, tw, bottom_info_w, info_w, 0)
        .or_else(|| {
            large
                .as_ref()
                .map(|emoji| large_emoji_media_input(emoji, info_w))
        });

    let inline_keyboard = msg
        .extra
        .reply_markup
        .as_ref()
        .filter(|m| m.is_inline() && !m.rows.is_empty())
        .map(|markup| {
            let mut kb_layout = tg_bots::keyboard_layout::ButtonsLayout::from_markup(
                markup,
                tg_bots::keyboard_layout::BotKeyboardMetrics::INLINE,
                |text| (text.chars().count() as i32) * 8,
            );
            let kb_max_w = bubble_content_max_width(viewport_w, item.has_from_photo).max(200);
            let kb_w = kb_layout.natural_width().clamp(200, kb_max_w);
            let kb_h = kb_layout.natural_height();
            kb_layout.resize(kb_w, kb_h);
            Size::new(kb_w, kb_layout.height().max(kb_h))
        });

    let input = MessageInput {
        is_out: msg.outgoing,
        is_post: msg.is_post,
        has_from_photo: item.has_from_photo,
        chat_mode,
        attach: item.attach_state,
        has_unread_bar: item.has_unread_bar,
        from_name_width,
        via_bot_width,
        forwarded_width,
        reply,
        text_max_width: tw,
        text_min_height: if !has_body_text {
            0
        } else {
            fonts.normal.height
        },
        text_wrapped_height: th,
        text_real_width: tw.min(max_text_wrap),
        media,
        invert_media: matches!(
            msg.media,
            Some(
                HistoryMedia::Photo(_)
                    | HistoryMedia::Album(_)
                    | HistoryMedia::Video(_)
                    | HistoryMedia::Document(_)
            )
        ),
        bottom_info,
        reactions_size: measure_reactions_size(
            &msg.reactions,
            matches!(msg.peer_id.kind(), PeerKind::User),
        ),
        reactions_in_bubble: true,
        has_comments_button: msg.is_post && msg.extra.replies_count.is_some(),
        comments_text_width: msg.extra.replies_count.map_or(0, |c| {
            let label_len = if c == 1 {
                "1 comment".len()
            } else {
                format!("{c} comments").len()
            };
            (label_len as i32) * 7 + 16
        }),
        inline_keyboard,
        ..MessageInput::default()
    };
    CachedItemLayout::Message(Box::new(layout_message(&input, viewport_w, fonts)))
}

/// Mirrors `CalculateModerateOptions` (`boxes/moderate_messages_box.cpp`): only incoming,
/// non-service group messages whose author is a participant (not the chat itself) can be moderated.
#[doc(hidden)]
pub fn can_moderate_message(msg: &HistoryMessage, peer: &HistoryPeerInfo) -> bool {
    peer.can_delete
        && !msg.outgoing
        && !msg.is_service()
        && peer.summary.kind == PeerKind::Group
        && msg.from_id != peer.summary.id
}

/// Official production `@Replies` bot user id (`PeerData::isRepliesChat`, `tg-data/src/peer.rs`).
pub const REPLIES_PEER_ID_PROD: u64 = 1_271_266_957;

/// Official test-DC `@Replies` bot user id (`PeerData::isRepliesChat`, `tg-data/src/peer.rs`).
pub const REPLIES_PEER_ID_TEST: u64 = 708_513;

/// Returns `true` when `peer` is the official `@Replies` chat where incoming comment replies
/// can be blocked via `contacts.blockFromReplies` (`window/window_peer_menu.cpp:2770-2855`).
#[must_use]
pub fn is_replies_chat_peer(peer: &HistoryPeerInfo) -> bool {
    (peer.summary.kind == PeerKind::User
        && matches!(
            peer.summary.id.bare(),
            REPLIES_PEER_ID_PROD | REPLIES_PEER_ID_TEST
        ))
        || peer.summary.title == "Replies"
}

/// `st::reactStripHeight`, `st::reactStripSize`, `st::reactStripImage`, `st::reactStripSkip`.
/// The scroll state one frame of the message list ended with (`Ui::ScrollArea` thumb inputs).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ScrollMetrics {
    /// Scroll offset from the top of the content, logical pixels.
    pub offset: f32,
    /// Height of the scrolled content the thumb is sized against.
    pub content_height: f32,
    /// Height of the visible part of the list.
    pub viewport_height: f32,
    /// Height of the laid-out message list (`HistoryList::total_height`).
    pub list_height: i32,
    /// Messages held by the list.
    pub loaded_messages: usize,
}

impl ScrollMetrics {
    /// Where the thumb sits on the track: `0.0` at the top, `1.0` at the bottom.
    #[must_use]
    pub fn thumb_position(&self) -> f32 {
        let range = self.content_height - self.viewport_height;
        if range <= 0.0 {
            1.0
        } else {
            (self.offset / range).clamp(0.0, 1.0)
        }
    }

    /// Thumb length as a share of the track (`viewport / content`).
    #[must_use]
    pub fn thumb_share(&self) -> f32 {
        if self.content_height <= 0.0 {
            1.0
        } else {
            (self.viewport_height / self.content_height).min(1.0)
        }
    }
}

/// Messages before the anchor in the page a chat opens with (`load_peer`).
pub const OPEN_PAGE_BEFORE: usize = 60;
/// Messages after the anchor in the page a chat opens with (`load_peer`).
pub const OPEN_PAGE_AFTER: usize = 40;

/// More loaded messages than this while the open chat sits at the bottom unloads the oldest page.
pub const MAX_LOADED_AT_BOTTOM: usize = 500;
/// Messages kept after unloading the oldest page (one 100-message page below the cap).
pub const KEEP_LOADED_AFTER_UNLOAD: usize = 400;

const CONTEXT_STRIP_HEIGHT: f32 = 40.0;
const CONTEXT_STRIP_SIZE: f32 = 32.0;
const CONTEXT_STRIP_IMAGE: f32 = 26.0;
const CONTEXT_STRIP_SKIP: f32 = 7.0;
/// Reactions shown in the strip before the expand button (Telegram Desktop 7.4.6 shows six).
const CONTEXT_STRIP_MAX_REACTIONS: usize = 6;
/// Strip offset relative to the menu (measured from Telegram Desktop 7.4.6).
const CONTEXT_STRIP_LEFT_SHIFT: f32 = 37.0;
const CONTEXT_STRIP_GAP: f32 = 13.0;
/// Triggered id prefix for a reaction chosen in the strip.
const CONTEXT_STRIP_REACTION_PREFIX: &str = "react:";
/// `st::menuWithIcons`: icon position and text left padding.
const CONTEXT_MENU_ICON_LEFT: f32 = 15.0;
const CONTEXT_MENU_TEXT_LEFT: f32 = 54.0;
const CONTEXT_MENU_PADDING_RIGHT: f32 = 17.0;
const CONTEXT_MENU_PADDING_V: f32 = 8.0;

/// tdesktop menu icon for a message context menu action (`st::menuIcon*`).
fn context_menu_icon(id: &str, fallback: Option<IconKind>) -> Option<tg_ui::tg_style::Icon> {
    use tg_ui::tg_style::st;
    Some(match id {
        "reply" | "reply_quote" => st::menu_icon_reply,
        "edit" => st::menu_icon_edit,
        "pin" => st::menu_icon_pin,
        "copy_text" | copy_selection::MENU_ID_COPY_SELECTED => st::menu_icon_copy,
        "copy_link" => st::menu_icon_link,
        "translate" => st::menu_icon_translate,
        "forward" => st::menu_icon_forward,
        "delete" | "moderate" | "remove_participant_reactions" | "stop_poll" => {
            st::menu_icon_delete
        }
        "select" => st::menu_icon_select,
        "report" | "report_msg_reaction" => st::menu_icon_report,
        "save_as" => st::menu_icon_download,
        "draw_to_reply" => st::menu_icon_draw,
        "save_gif" => st::menu_icon_gif,
        "fave_sticker" => st::menu_icon_stickers,
        "block_from_replies" => st::menu_icon_block,
        "who_reacted" => st::menu_icon_reactions,
        "open_comments" => st::menu_icon_show_in_chat,
        _ => return fallback.and_then(tg_ui::icons::style_icon_for_kind),
    })
}

/// Geometry of the open message context menu and its reaction strip.
struct ContextMenuGeometry {
    menu: Rect,
    strip: Rect,
    row_h: f32,
    sep_h: f32,
}

fn context_menu_geometry(
    ctx: &egui::Context,
    anchor: Pos2,
    items: &[MenuItem],
    reaction_count: usize,
    style: &PopupMenuStyle,
) -> ContextMenuGeometry {
    let menu_st = &style.menu;
    let font = menu_st.item_style.font.font_id();
    let row_h = CONTEXT_MENU_PADDING_V * 2.0 + menu_st.item_style.font.line_height();
    let sep_h = menu_st.separator.padding.vertical() + menu_st.separator.width;
    let mut width = menu_st.width_min;
    let mut height = style.scroll_padding.vertical();
    for item in items {
        match item {
            MenuItem::Separator => height += sep_h,
            MenuItem::Action { label, .. } | MenuItem::Toggle { label, .. } => {
                height += row_h;
                let w = ctx.fonts_mut(|f| {
                    f.layout_no_wrap(label.clone(), font.clone(), Color32::WHITE)
                        .size()
                        .x
                });
                width = width.max(CONTEXT_MENU_TEXT_LEFT + w + CONTEXT_MENU_PADDING_RIGHT);
            }
        }
    }
    width = width.clamp(menu_st.width_min, menu_st.width_max);
    let (strip_w, strip_room) = if reaction_count == 0 {
        (0.0, 0.0)
    } else {
        (
            CONTEXT_STRIP_SKIP * 2.0 + (reaction_count as f32 + 1.0) * CONTEXT_STRIP_SIZE,
            CONTEXT_STRIP_HEIGHT + CONTEXT_STRIP_GAP,
        )
    };
    let screen = ctx.viewport_rect();
    let x = anchor
        .x
        .min(screen.max.x - width - 8.0)
        .max(screen.min.x + 8.0);
    let y = anchor
        .y
        .min(screen.max.y - height - 8.0)
        .max(screen.min.y + 8.0 + strip_room);
    let menu = Rect::from_min_size(Pos2::new(x, y), Vec2::new(width, height));
    let strip = Rect::from_min_size(
        Pos2::new(
            (x - CONTEXT_STRIP_LEFT_SHIFT)
                .min(screen.max.x - strip_w - 8.0)
                .max(screen.min.x + 8.0),
            y - CONTEXT_STRIP_GAP - CONTEXT_STRIP_HEIGHT,
        ),
        Vec2::new(strip_w, CONTEXT_STRIP_HEIGHT),
    );
    ContextMenuGeometry {
        menu,
        strip,
        row_h,
        sep_h,
    }
}

/// Paints the reaction strip (`HistoryView::Reactions::Selector`); returns the clicked id.
fn paint_reaction_strip(
    ui: &mut Ui,
    base_id: Id,
    strip_rect: Rect,
    reactions: &[String],
    style: &PopupMenuStyle,
    assets: &mut MediaAssetCache,
) -> Option<String> {
    let ctx = ui.ctx().clone();
    let painter = ui.painter().clone();
    let menu_st = &style.menu;
    let mut triggered = None;
    tg_ui::painter::BoxShadow::new(style.shadow).paint_with_opacity(
        &ctx,
        &painter,
        strip_rect,
        CONTEXT_STRIP_HEIGHT * 0.5,
        style.shadow.opacity,
    );
    painter.rect_filled(
        strip_rect,
        CornerRadius::same((CONTEXT_STRIP_HEIGHT * 0.5) as u8),
        menu_st.item_bg,
    );
    let cell_y = strip_rect.center().y - CONTEXT_STRIP_SIZE * 0.5;
    let cell_at = |i: usize| {
        Rect::from_min_size(
            Pos2::new(
                strip_rect.left() + CONTEXT_STRIP_SKIP + i as f32 * CONTEXT_STRIP_SIZE,
                cell_y,
            ),
            Vec2::splat(CONTEXT_STRIP_SIZE),
        )
    };
    for (i, emoji) in reactions.iter().enumerate() {
        let cell = cell_at(i);
        let resp = ui.interact(cell, base_id.with(("react", i)), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, emoji.as_str()));
        if resp.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            painter.circle_filled(
                cell.center(),
                CONTEXT_STRIP_SIZE * 0.5,
                menu_st.item_bg_over,
            );
        }
        let image = Rect::from_center_size(cell.center(), Vec2::splat(CONTEXT_STRIP_IMAGE));
        if !crate::history::reaction_buttons::paint_special_reaction(
            &ctx,
            &painter,
            emoji,
            image,
            Rect::from_center_size(cell.center(), image.size() * 0.5),
            menu_st.item_fg,
        ) {
            let _ = assets
                .emoji()
                .paint_emoji_str(&painter, &ctx, emoji, image, Color32::WHITE);
        }
        if resp.clicked() {
            triggered = Some(format!("{CONTEXT_STRIP_REACTION_PREFIX}{emoji}"));
        }
    }
    let expand = cell_at(reactions.len());
    let resp = ui.interact(expand, base_id.with("react_more"), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "More reactions"));
    if resp.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    painter.circle_filled(expand.center(), 11.0, menu_st.item_bg_over);
    let c = expand.center();
    painter.add(egui::Shape::line(
        vec![
            Pos2::new(c.x - 4.0, c.y - 1.5),
            Pos2::new(c.x, c.y + 2.5),
            Pos2::new(c.x + 4.0, c.y - 1.5),
        ],
        Stroke::new(1.5, menu_st.item_icon_fg),
    ));
    if resp.clicked() {
        triggered = Some("react_more".to_owned());
    }
    triggered
}

/// Paints the menu card (`Ui::PopupMenu` with `st::menuWithIcons`); returns the clicked id.
fn paint_context_menu_card(
    ui: &mut Ui,
    base_id: Id,
    geo: &ContextMenuGeometry,
    items: &[MenuItem],
    style: &PopupMenuStyle,
) -> Option<String> {
    let ctx = ui.ctx().clone();
    let painter = ui.painter().clone();
    let menu_st = &style.menu;
    let font = menu_st.item_style.font.font_id();
    let menu_rect = geo.menu;
    let mut triggered = None;
    tg_ui::painter::BoxShadow::new(style.shadow).paint_with_opacity(
        &ctx,
        &painter,
        menu_rect,
        style.radius,
        style.shadow.opacity,
    );
    painter.rect_filled(
        menu_rect,
        CornerRadius::same(style.radius as u8),
        menu_st.item_bg,
    );
    let clipped = painter.with_clip_rect(menu_rect);
    let mut cursor_y = menu_rect.top() + style.scroll_padding.top;
    for (idx, item) in items.iter().enumerate() {
        let (id, label, icon, attention) = match item {
            MenuItem::Separator => {
                let sep_y = cursor_y + menu_st.separator.padding.top;
                clipped.rect_filled(
                    Rect::from_min_size(
                        Pos2::new(menu_rect.left(), sep_y),
                        Vec2::new(menu_rect.width(), menu_st.separator.width),
                    ),
                    CornerRadius::ZERO,
                    menu_st.separator.fg,
                );
                cursor_y += geo.sep_h;
                continue;
            }
            MenuItem::Action {
                id,
                label,
                icon,
                attention,
                ..
            } => (id, label, *icon, *attention),
            MenuItem::Toggle {
                id, label, icon, ..
            } => (id, label, *icon, false),
        };
        let row = Rect::from_min_size(
            Pos2::new(menu_rect.left(), cursor_y),
            Vec2::new(menu_rect.width(), geo.row_h),
        );
        cursor_y += geo.row_h;
        let resp = ui.interact(row, base_id.with(("item", idx)), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label.as_str()));
        let hovered = resp.hovered();
        if hovered {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            clipped.rect_filled(row, CornerRadius::ZERO, menu_st.item_bg_over);
        }
        let (fg, icon_fg) = if attention {
            (menu_st.item_fg_attention, menu_st.item_fg_attention)
        } else if hovered {
            (menu_st.item_fg_over, menu_st.item_icon_fg_over)
        } else {
            (menu_st.item_fg, menu_st.item_icon_fg)
        };
        if let Some(style_icon) = context_menu_icon(id, icon) {
            let icon_rect = Rect::from_min_size(
                Pos2::new(row.left() + CONTEXT_MENU_ICON_LEFT, row.top() + 5.0),
                Vec2::splat(24.0),
            );
            tg_app_core::ui_icons::paint_style_tinted(&clipped, icon_rect, style_icon, icon_fg);
        }
        clipped.text(
            Pos2::new(row.left() + CONTEXT_MENU_TEXT_LEFT, row.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            font.clone(),
            fg,
        );
        if resp.clicked() {
            triggered = Some(id.clone());
        }
    }
    triggered
}

impl HistoryInnerWidget {
    /// Paints the open message context menu and the reaction strip above it. Returns the
    /// triggered item id; a strip reaction comes back as `"react:<emoji>"`, the expand button as
    /// `"react_more"`.
    fn show_message_menu(
        &mut self,
        ctx: &egui::Context,
        items: &[MenuItem],
        reactions: &[String],
        style: &PopupMenuStyle,
    ) -> Option<String> {
        if !self.context_menu.is_open() {
            return None;
        }
        let geo = context_menu_geometry(ctx, self.context_menu_pos, items, reactions.len(), style);
        let bounds = if reactions.is_empty() {
            geo.menu
        } else {
            geo.menu.union(geo.strip)
        };
        let base_id = Id::unique(("tg_hist_msg_menu", self.peer_id.0));
        let assets = &mut self.assets;
        let area = egui::Area::new(base_id.with("area"))
            .order(egui::Order::Foreground)
            .fixed_pos(bounds.min)
            .show(ctx, |ui| {
                ui.allocate_exact_size(bounds.size(), Sense::hover());
                let from_strip = if reactions.is_empty() {
                    None
                } else {
                    paint_reaction_strip(ui, base_id, geo.strip, reactions, style, assets)
                };
                let from_menu = paint_context_menu_card(ui, base_id, &geo, items, style);
                from_strip.or(from_menu)
            });
        let triggered = area.inner;
        let outside_click = ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| !area.response.rect.contains(p))
        });
        let escape = ctx.input(|i| i.key_pressed(Key::Escape));
        if triggered.is_some()
            || escape
            || (outside_click && ctx.cumulative_frame_nr() > self.context_menu_opened_frame)
        {
            self.context_menu.close();
        }
        triggered
    }
}

/// Whether a message bubble shows the sender name header: group messages from others that
/// start a new block, and broadcast channel posts (Telegram Desktop 7.4.6 titles each post).
fn shows_from_name(msg: &HistoryMessage, show_sender_in_chat: bool, attached: bool) -> bool {
    if msg.is_service() {
        return false;
    }
    let channel_post = msg.is_post && matches!(msg.peer_id.kind(), PeerKind::Channel);
    channel_post || (show_sender_in_chat && !msg.outgoing && !attached)
}

/// Paints the round fast-share button right of a channel post (`HistoryView::Message`
/// `drawRightAction`, `st::historyFastShareSize` / `Left` / `Bottom`). Returns `true` on click.
fn paint_fast_share_button(
    ui: &mut Ui,
    msg_id: MsgId,
    bubble_rect: Rect,
    colors: &HistoryColors,
) -> bool {
    use tg_ui::tg_style::st;
    let size = 31.0;
    let rect = Rect::from_min_size(
        Pos2::new(
            bubble_rect.right() + 13.0,
            bubble_rect.bottom() - 5.0 - size,
        ),
        Vec2::splat(size),
    );
    let resp = ui.interact(
        rect,
        Id::unique(("tg_fast_share", msg_id.0)),
        Sense::click(),
    );
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::Role::Button,
            true,
            tg_lang::tr::lng_context_forward_msg(),
        )
    });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    ui.painter()
        .circle_filled(rect.center(), size * 0.5, colors.service_bg);
    tg_app_core::ui_icons::paint_style_tinted(
        ui.painter(),
        rect,
        st::history_fast_share_icon,
        colors.service_fg,
    );
    resp.clicked()
}

/// Bottom-right anchor of a bubble-less message's right action (`Gif::draw` / `UnwrappedMedia`
/// `fullRight` / `fullBottom`): past the round video's info pill, or the media's right edge.
pub(crate) fn unwrapped_right_action_anchor(layout: &MessageLayout, origin: Pos2) -> Rect {
    let media = hist_rect_to_egui(layout.media_rect, origin.x, origin.y);
    let right = if layout.bottom_info_rect.is_empty() {
        media.right()
    } else {
        hist_rect_to_egui(layout.bottom_info_rect, origin.x, origin.y)
            .right()
            .max(media.left() + media.width().min(media.height()))
    };
    Rect::from_min_max(media.min, Pos2::new(right, media.bottom()))
}

/// `st::historyFastShareSize`.
pub(crate) const HISTORY_FAST_SHARE_SIZE_F: f32 = 31.0;
/// `st::historyFastShareLeft`.
pub(crate) const HISTORY_FAST_SHARE_LEFT_F: f32 = 13.0;
/// `st::historyFastShareBottom`.
pub(crate) const HISTORY_FAST_SHARE_BOTTOM_F: f32 = 5.0;

/// Short replies count under the side comments icon (`HistoryMessageViews::repliesSmall`).
fn side_comments_count_galley(
    ui: &Ui,
    count: u32,
    colors: &HistoryColors,
) -> Option<std::sync::Arc<egui::Galley>> {
    (count > 0).then(|| {
        ui.painter().layout_no_wrap(
            crate::history::bubble::format_compact_views(count),
            FontId::new(13.0, tg_ui::fonts::family_semibold()),
            colors.service_fg,
        )
    })
}

/// `Message::rightActionSize` for the comments action (with or without the count line).
fn side_action_size(count_galley: Option<&std::sync::Arc<egui::Galley>>) -> Vec2 {
    count_galley.map_or(Vec2::splat(HISTORY_FAST_SHARE_SIZE_F), |g| {
        Vec2::new(
            HISTORY_FAST_SHARE_SIZE_F.max(2.0 * HISTORY_FAST_SHARE_BOTTOM_F + g.size().x.ceil()),
            HISTORY_FAST_SHARE_SIZE_F + HISTORY_FAST_SHARE_BOTTOM_F + g.size().y.ceil(),
        )
    })
}

/// Height of a bubble-less message's right action (comments or fast share), `0` without one.
pub(crate) fn unwrapped_right_action_height(
    ui: &Ui,
    msg: &HistoryMessage,
    colors: &HistoryColors,
) -> f32 {
    if !(msg.is_post && matches!(msg.peer_id.kind(), PeerKind::Channel)) {
        return 0.0;
    }
    let count = msg.extra.replies_count.unwrap_or(0);
    side_action_size(side_comments_count_galley(ui, count, colors).as_ref()).y
}

/// Paints the comments right action of a bubble-less channel post (`Message::drawRightAction`
/// with `displayRightActionComments`: `historyFastCommentsIcon` plus the short replies count).
/// Returns `true` on click.
fn paint_side_comments_button(
    ui: &mut Ui,
    msg_id: MsgId,
    count: u32,
    anchor: Rect,
    colors: &HistoryColors,
) -> bool {
    use tg_ui::tg_style::st;
    const SIZE: f32 = HISTORY_FAST_SHARE_SIZE_F;
    let count_galley = side_comments_count_galley(ui, count, colors);
    let size = side_action_size(count_galley.as_ref());
    let rect = Rect::from_min_size(
        Pos2::new(
            anchor.right() + HISTORY_FAST_SHARE_LEFT_F,
            anchor.bottom() - HISTORY_FAST_SHARE_BOTTOM_F - size.y,
        ),
        size,
    );
    let resp = ui.interact(
        rect,
        Id::unique(("tg_side_comments", msg_id.0)),
        Sense::click(),
    );
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{count} comments"))
    });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let radius = (SIZE / 2.0).round() as u8;
    ui.painter()
        .rect_filled(rect, CornerRadius::same(radius), colors.service_bg);
    tg_app_core::ui_icons::paint_style_tinted(
        ui.painter(),
        Rect::from_min_size(rect.min, Vec2::new(rect.width(), SIZE)),
        st::history_fast_comments_icon,
        colors.service_fg,
    );
    if let Some(galley) = count_galley {
        let x = rect.center().x - galley.size().x / 2.0;
        ui.painter()
            .galley(Pos2::new(x, rect.top() + SIZE), galley, colors.service_fg);
    }
    resp.clicked()
}

/// Builds the message context menu in Telegram Desktop 7.4.6 order
/// (`HistoryInner::showContextMenu`): Reply, Edit, Pin, Copy Text, Copy Link, media actions,
/// Forward, Delete, Report, Select. Reactions are chosen from the strip above the menu.
fn build_context_menu_items(
    msg: &HistoryMessage,
    peer: &HistoryPeerInfo,
    has_quote_selection: bool,
) -> Vec<MenuItem> {
    let mut items = Vec::new();
    if msg.id.is_local() {
        // `HistoryInner::showContextMenu`: a message being sent (or whose send failed) is not
        // regular, so it can only be copied and deleted.
        if !msg.text.is_empty() || msg.extra.rich_message.is_some() {
            items.push(MenuItem::action(
                "copy_text",
                tg_lang::tr::lng_context_copy_text(),
            ));
        }
        items.push(MenuItem::action(
            "delete",
            tg_lang::tr::lng_context_delete_msg(),
        ));
        return items;
    }
    let is_group_like = matches!(peer.summary.kind, PeerKind::Group | PeerKind::Channel);

    if !msg.reactions.is_empty() && is_group_like {
        let total_reacts: u32 = msg.reactions.iter().map(|r| r.count).sum();
        items.push(
            MenuItem::action("who_reacted", format!("Who Reacted ({total_reacts})..."))
                .with_icon(IconKind::Info),
        );
        if msg
            .reactions
            .iter()
            .any(|r| r.emoji == "⭐" || r.emoji == "★")
        {
            items.push(
                MenuItem::action(
                    "toggle_paid_reaction_privacy",
                    format!(
                        "{} (Toggle Anonymous)",
                        tg_lang::tr::lng_paid_react_show_in_top()
                    ),
                )
                .with_icon(IconKind::Star),
            );
        }
        items.push(
            MenuItem::action("report_msg_reaction", tg_lang::tr::lng_report_reaction())
                .with_icon(IconKind::Info),
        );
        if peer.can_delete && !msg.outgoing && msg.from_id != peer.summary.id {
            items.push(
                MenuItem::action(
                    "remove_participant_reactions",
                    "Remove Participant Reactions",
                )
                .with_icon(IconKind::Trash)
                .attention(),
            );
        }
        items.push(MenuItem::Separator);
    }

    items.push(MenuItem::action(
        "reply",
        tg_lang::tr::lng_context_reply_msg(),
    ));
    if has_quote_selection {
        items.push(MenuItem::action("reply_quote", "Reply with Quote"));
    }

    if let Some(HistoryMedia::Poll(ref poll)) = msg.media {
        if !poll.is_anonymous && poll.total_voters > 0 {
            items.push(
                MenuItem::action("poll_voters", "View Poll Voters...").with_icon(IconKind::Info),
            );
        }
        if !poll.closed && !poll.is_quiz && poll.options.iter().any(|o| o.chosen) {
            items.push(MenuItem::action("retract_vote", "Retract Vote").with_icon(IconKind::Back));
        }
        if !poll.closed && msg.outgoing {
            items.push(
                MenuItem::action("stop_poll", "Stop Poll")
                    .with_icon(IconKind::Trash)
                    .attention(),
            );
        }
    }

    if msg.extra.replies_count.is_some() && msg.is_post {
        items.push(MenuItem::action("open_comments", "Open Comments").with_icon(IconKind::Info));
    }

    if msg.outgoing
        && (!msg.text.is_empty() || msg.extra.rich_message.is_some())
        && peer.summary.can_send
    {
        items.push(MenuItem::action(
            "edit",
            tg_lang::tr::lng_context_edit_msg(),
        ));
    }

    if peer.can_pin {
        items.push(MenuItem::action(
            "pin",
            if msg.is_pinned {
                tg_lang::tr::lng_context_unpin_msg()
            } else {
                tg_lang::tr::lng_context_pin_msg()
            },
        ));
    }

    if !msg.text.is_empty() || msg.extra.rich_message.is_some() {
        items.push(MenuItem::action(
            "copy_text",
            tg_lang::tr::lng_context_copy_text(),
        ));
    }
    if msg.extra.translated_text.is_some() {
        items.push(MenuItem::action("translate", "Show Original"));
    }
    if is_group_like {
        let copy_link_label = if msg.is_post {
            tg_lang::tr::lng_context_copy_post_link()
        } else {
            tg_lang::tr::lng_context_copy_message_link()
        };
        items.push(MenuItem::action("copy_link", copy_link_label));
    }

    if msg.media.is_some() {
        items.push(MenuItem::action("save_as", "Save As..."));
    }
    if matches!(msg.media, Some(HistoryMedia::Photo(_))) && peer.summary.can_send {
        items.push(MenuItem::action("draw_to_reply", "Draw to Reply"));
    }
    if matches!(msg.media, Some(HistoryMedia::Video(ref v)) if v.is_gif) {
        items.push(MenuItem::action("save_gif", "Save GIF"));
    }
    if matches!(msg.media, Some(HistoryMedia::Sticker(_))) {
        items.push(MenuItem::action("fave_sticker", "Add Sticker to Favorites"));
    }
    if matches!(msg.media, Some(HistoryMedia::Voice(_)))
        || matches!(msg.media, Some(HistoryMedia::Document(ref d)) if d.mime_type.starts_with("audio/"))
    {
        items.push(MenuItem::action("save_ringtone", "Save for Notifications"));
    }

    items.push(MenuItem::action(
        "forward",
        tg_lang::tr::lng_context_forward_msg(),
    ));

    if peer.can_delete {
        if can_moderate_message(msg, peer) {
            items.push(
                MenuItem::action("moderate", "Moderate...")
                    .with_icon(IconKind::Trash)
                    .attention(),
            );
        }
        items.push(MenuItem::action(
            "delete",
            tg_lang::tr::lng_context_delete_msg(),
        ));
    }

    if !msg.outgoing && is_group_like {
        items.push(MenuItem::action(
            "report",
            tg_lang::tr::lng_context_report_msg(),
        ));
    }
    if !msg.outgoing && !msg.is_service() && is_replies_chat_peer(peer) {
        items.push(
            MenuItem::action("block_from_replies", "Block User...")
                .with_icon(IconKind::Trash)
                .attention(),
        );
    }

    items.push(MenuItem::action(
        "select",
        tg_lang::tr::lng_context_select_msg(),
    ));
    items
}

fn suggested_save_name(msg: &HistoryMessage) -> String {
    match &msg.media {
        Some(HistoryMedia::Photo(p)) => p.file_name.clone(),
        Some(HistoryMedia::Video(v)) => v.file_name.clone(),
        Some(HistoryMedia::Document(d)) => d.file_name.clone(),
        Some(HistoryMedia::Album(items)) => items
            .first()
            .map_or_else(|| "album.jpg".to_owned(), |it| it.label.clone()),
        Some(HistoryMedia::Voice(_)) => format!("voice_{}.ogg", msg.id.0),
        Some(HistoryMedia::Sticker(s)) => format!("sticker_{}.tgs", s.sticker_id),
        _ => format!("message_{}.txt", msg.id.0),
    }
}

/// Width of the last wrapped line of a message body at `max_wrap` (where the date skip block
/// goes in `Ui::Text::String`).
fn last_line_width(
    ui: &Ui,
    text: &tg_text::TextWithEntities,
    palette: &tg_ui::stand_in::TextPalette,
    base_color: Color32,
    max_wrap: i32,
) -> i32 {
    let rich = entities_to_rich_text(text);
    let job = tg_ui::text::build_layout_job(
        &rich,
        &message_text_style(),
        palette,
        base_color,
        (max_wrap as f32).max(40.0),
        None,
        true,
    );
    let galley = ui.painter().layout_job(job);
    galley.rows.last().map_or(0, |row| {
        row.rect_without_leading_space().width().ceil() as i32
    })
}

fn paint_selection_checkbox(
    painter: &egui::Painter,
    center: Pos2,
    selected: bool,
    colors: &HistoryColors,
) {
    let radius = 9.0;
    if selected {
        painter.circle_filled(center, radius, colors.accent);
        painter.add(Shape::line(
            vec![
                Pos2::new(center.x - 4.0, center.y + 0.5),
                Pos2::new(center.x - 1.2, center.y + 3.2),
                Pos2::new(center.x + 4.2, center.y - 3.0),
            ],
            Stroke::new(1.8, Color32::WHITE),
        ));
    } else {
        painter.circle_filled(center, radius, Color32::from_rgba_unmultiplied(0, 0, 0, 32));
        painter.circle_stroke(center, radius, Stroke::new(1.6, Color32::WHITE));
    }
}

/// Service bubble background over the Classic wallpaper (`msgServiceBg` adjusted to the
/// wallpaper average color, measured from Telegram Desktop 7.4.6).
const CLASSIC_SERVICE_BG: Color32 = Color32::from_rgb(0x82, 0x9F, 0x67);
/// Night service bubble background (`msgServiceBg`, Night theme).
const NIGHT_SERVICE_BG: Color32 = Color32::from_rgb(0x1E, 0x2C, 0x3A);

/// Returns `colors` with service bubbles adjusted to the default chat background
/// (`Window::Theme::Background::adjustPaletteUsingBackground`).
#[must_use]
pub fn colors_for_default_wallpaper(colors: &HistoryColors) -> HistoryColors {
    let mut out = *colors;
    if colors.is_night {
        out.service_bg = NIGHT_SERVICE_BG;
    } else {
        out.service_bg = CLASSIC_SERVICE_BG;
    }
    out.service_fg = Color32::WHITE;
    out
}

impl HistoryInnerWidget {
    /// Paints the active chat background (`tg_app_settings::themes::active_chat_background`):
    /// the Classic pattern wallpaper by default in Day, flat `#0E1621` in Night.
    fn paint_chat_wallpaper(&mut self, ui: &mut Ui, rect: Rect, colors: &HistoryColors) {
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.canvas_bg);
        let background = tg_app_settings::themes::active_chat_background(ui.ctx());
        if let Ok(mut bg) = background.lock() {
            bg.set_dark_mode(colors.is_night);
            bg.paint(ui, rect, Some(self.peer_id.0));
        };
    }
}

/// Formats a Unix timestamp as a Telegram-style centered date separator (`"November 21"`, `"November 22"`).
#[must_use]
pub fn format_date_separator(unix_secs: i32) -> String {
    let dt = LocalDateTime::from_unix_local(i64::from(unix_secs));
    format_civil_date(dt.date)
}

/// Message bottom-info time in the local time zone (`HistoryView::BottomInfo`, `"H:mm"`).
#[must_use]
pub fn message_time_text(msg: &HistoryMessage) -> String {
    tg_history::datetime::format_time_short(LocalDateTime::from_unix_local(i64::from(
        msg.date_unix,
    )))
}

fn format_civil_date(date: CivilDate) -> String {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let month = MONTHS
        .get(usize::from(date.month.saturating_sub(1)))
        .copied()
        .unwrap_or("Unknown");
    format!("{month} {}", date.day)
}

/// Widest bubble content for a `viewport_w` chat (`Message::resizeGetHeight`): the width minus
/// `st::msgMargin` left/right, minus `st::msgPhotoSkip` when the sender userpic is shown, capped
/// at `st::msgMaxWidth`.
fn bubble_content_max_width(viewport_w: i32, has_from_photo: bool) -> i32 {
    let photo_skip = if has_from_photo { MSG_PHOTO_SKIP } else { 0 };
    (viewport_w - MSG_MARGIN.left - MSG_MARGIN.right - photo_skip).min(MSG_MAX_WIDTH)
}

#[path = "inner_drag_select.rs"]
mod drag_select;

#[cfg(test)]
#[path = "inner_factcheck_tests.rs"]
mod factcheck_tests;

#[cfg(test)]
#[path = "inner_caption_tests.rs"]
mod caption_tests;

#[cfg(test)]
#[path = "inner_bubble_layout_tests.rs"]
mod bubble_layout_tests;

#[cfg(test)]
#[path = "inner_font_evidence.rs"]
mod font_evidence;

/// `HistoryWidget::unreadBarTop`: the scroll top that puts the "Unread messages" bar of item
/// `idx` at the top of the view (below the item's date badge, at the offset the bar is painted).
fn unread_bar_scroll_top(list: &tg_history::HistoryList, idx: usize) -> i32 {
    // `countInitialScrollTop`: the whole history is unread and loaded from its start.
    if idx == 0 && list.loaded_at_top {
        return 0;
    }
    let Some(item) = list.items().get(idx) else {
        return 0;
    };
    let bar_offset = ((LINE_WIDTH + HISTORY_UNREAD_BAR_MARGIN) as f32 * 0.5).round() as i32;
    (list.item_top(idx) + item.displayed_date_height + bar_offset).max(0)
}
