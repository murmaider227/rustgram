//! Virtualized scrollable message list: visible-range layout, sticky userpics,
//! scroll anchoring, preload triggers, jump-to-message highlight, multi-select,
//! drag-select text, and right-click context menu.
//!
//! Port origin: `Telegram/SourceFiles/history/history_inner_widget.cpp` and
//! `Telegram/SourceFiles/history/view/history_view_list_widget.cpp`.

use std::collections::{HashMap, HashSet};

use egui::{
    Color32, CornerRadius, FontId, Id, Key, Pos2, Rect, ScrollArea, Sense, Shape, Stroke, Ui, Vec2,
};
use tg_history::consts::{
    FontMetrics, HISTORY_PHOTO_LEFT, HISTORY_UNREAD_BAR_MARGIN, LINE_WIDTH, MSG_MARGIN,
    MSG_MAX_WIDTH, MSG_PADDING, MSG_PHOTO_SIZE,
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
use tg_ui::stand_in::{PopupMenuStyle, ToggleStyle};
use tg_ui::userpic::EmptyUserpic;
use tg_ui::widgets::buttons::IconKind;
use tg_ui::widgets::menu::{MenuItem, PopupMenuState};

use crate::bots::inline_keyboard::{InlineKeyboardActivation, InlineKeyboardState};
use crate::history::backend::{
    HistoryBackend, HistoryMedia, HistoryMessage, HistoryPeerInfo, HistorySlice,
    MessageSummaryEntry, ServiceAction, to_hist_full, to_hist_peer,
};
use crate::history::bubble::{
    InlineKeyboardRenderCtx, MessagePaintEvent, ServiceCardAction, format_compact_views,
    hist_rect_to_egui, link_preview_card_height, measure_reactions_size, paint_date_separator,
    paint_message_element_full, paint_service_message_interactive, paint_unread_bar,
    parse_paid_media_stars,
};
use crate::history::inline_media::HistoryMediaRuntime;
use crate::history::media::{
    MediaAssetCache, MediaInteraction, VoicePlaybackState, format_duration, format_file_size,
};
use crate::history::sections_model::{CopiedRichText, ReplyQuote, slice_text_with_entities};
use crate::history::text_bridge::{estimate_text_metrics, measure_rich_text, slice_by_char_range};
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
    /// User clicked a link / mention / hashtag.
    OpenLink(String),
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
    /// Open the full emoji + `tg-lottie` reactions picker for `msg_id`.
    OpenReactionsPicker {
        /// Target message id.
        msg_id: MsgId,
        /// Anchor screen position.
        pos: Pos2,
    },
    /// Open the Who-Reacted peer list for `msg_id` (`messages.getMessageReactionsList`).
    OpenWhoReacted(MsgId),
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
    /// Copied text with preserved `tg-text` entities.
    CopiedRich {
        /// Toast summary.
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
    layout_cache: HashMap<MsgId, CachedItemLayout>,
    /// Viewport width (integer logical pixels) for which `layout_cache` is valid.
    layout_width: i32,
    /// Chat layout mode (`Default` vs `Wide`) for which `layout_cache` is valid.
    layout_wide: bool,
    /// Current vertical scroll offset in logical pixels.
    scroll_offset_y: f32,
    /// Pending explicit scroll target (`Some(y)` overrides `ScrollArea` on next frame).
    pending_scroll_y: Option<f32>,
    /// True if next paint should jump to the very bottom.
    pending_scroll_to_bottom: bool,
    /// Pending message id to center and highlight once laid out.
    pending_jump_msg: Option<MsgId>,
    /// Last measured viewport height.
    last_viewport_h: f32,
    /// Whether the user is scrolled away from the bottom (controls FAB visibility).
    scrolled_up: bool,
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
    /// Per-chat viewport read-metrics visibility tracker (`HistoryView::ReadMetricsTracker`).
    read_metrics: Option<tg_app_core::core_types::read_metrics::ReadMetricsTracker>,
    /// View-layer interactive emoji burst animation overlays (`HistoryView::EmojiInteractions`).
    emoji_interactions: crate::history::emoji_interactions::HistoryViewEmojiInteractions,
    /// Last painted bubble center & outgoing flag per visible message (for incoming emoji bursts).
    last_bubble_anchors: HashMap<MsgId, (Pos2, bool)>,
    /// Last frame timestamp in ms (used by programmatic highlight triggers).
    last_now_ms: i64,
    /// Last observed AI summary fingerprint `(shown, loading, text_len)` per message for cache invalidation.
    summary_fingerprints: HashMap<MsgId, (bool, bool, usize)>,
    /// Configured double-click quick action (`CoreSettings::chat_quick_action`).
    chat_quick_action: tg_core::settings::DoubleClickQuickAction,
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
}

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
            pending_scroll_y: None,
            pending_scroll_to_bottom: true,
            pending_jump_msg: None,
            last_viewport_h: 600.0,
            scrolled_up: false,
            unread_count: 0,
            assets: MediaAssetCache::new(),
            media_runtime: HistoryMediaRuntime::new(false),
            voice_state: VoicePlaybackState::default(),
            context_menu: PopupMenuState::new(),
            context_menu_msg: None,
            context_menu_pos: Pos2::ZERO,
            context_menu_engagement: None,
            selection_anchor: None,
            last_frame_laid_out_count: 0,
            last_frame_painted_count: 0,
            inline_kb_state: InlineKeyboardState::new(),
            expanded_factchecks: HashSet::new(),
            last_painted_bubble_rects: HashMap::new(),
            read_metrics: None,
            emoji_interactions:
                crate::history::emoji_interactions::HistoryViewEmojiInteractions::new(),
            last_bubble_anchors: HashMap::new(),
            last_now_ms: 0,
            summary_fingerprints: HashMap::new(),
            chat_quick_action: tg_core::settings::DoubleClickQuickAction::Reply,
            corner_reply: true,
            corner_reaction: true,
            quick_reaction_emoji: crate::history::quick_action::DEFAULT_QUICK_REACTION_EMOJI
                .to_owned(),
            reduced_motion: false,
            bubble_hover_quick_actions:
                crate::history::quick_action::BubbleHoverQuickActions::default(),
        }
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
        let existing_drafts: Vec<&HistoryMessage> =
            self.messages.iter().filter(|m| m.id.0 < 0).collect();
        if existing_drafts.len() == drafts.len()
            && existing_drafts.iter().zip(drafts.iter()).all(|(a, b)| {
                a.id == b.id && a.text == b.text && a.extra.rich_message == b.extra.rich_message
            })
        {
            return;
        }
        self.messages.retain(|m| m.id.0 >= 0);
        for d in drafts {
            self.layout_cache.remove(&d.id);
            self.messages.push(d);
        }
        let is_group = matches!(peer_kind, PeerKind::Group);
        let items: Vec<HistoryListItem> = self
            .messages
            .iter()
            .map(|m| build_list_item(m, is_group))
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
        &mut self.media_runtime
    }

    /// Applies [`tg_core::settings::CoreSettings`] autoplay, audio, and quick-action preferences.
    pub fn apply_core_settings(&mut self, settings: &tg_core::settings::CoreSettings) {
        self.media_runtime.apply_core_settings(settings);
        self.chat_quick_action = settings.chat_quick_action;
        self.corner_reply = settings.corner_reply;
        self.corner_reaction = settings.corner_reaction;
        self.reduced_motion =
            settings.is_power_saving_on(tg_core::settings::PowerSavingFlags::ANIMATIONS);
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

    /// Extracts selected text preserving all `tg-text` UTF-16 entities (for copy with entities).
    #[must_use]
    pub fn extract_selected_rich_text(&self) -> CopiedRichText {
        let mut combined = tg_text::TextWithEntities::default();
        if self.list.selection.has_selected_items() {
            for m in &self.messages {
                let full = to_hist_full(FullMsgId {
                    peer: self.peer_id,
                    msg: m.id,
                });
                if self.list.selection.selected_items.contains(&full) {
                    if !combined.text.is_empty() {
                        combined.text.push('\n');
                    }
                    let base_u16 = combined.utf16_len() as i32;
                    let src = if m.text.is_empty() {
                        tg_text::TextWithEntities::simple(m.preview_snippet())
                    } else {
                        m.text.clone()
                    };
                    combined.text.push_str(&src.text);
                    for e in src.entities {
                        combined.entities.push(tg_text::EntityInText::with_data(
                            e.entity_type(),
                            e.offset() + base_u16,
                            e.length(),
                            e.data(),
                        ));
                    }
                }
            }
            combined.sort_entities();
            return CopiedRichText { content: combined };
        }
        for (idx, m) in self.messages.iter().enumerate() {
            let full = to_hist_full(FullMsgId {
                peer: self.peer_id,
                msg: m.id,
            });
            let sel = self.list.selection.selection_for_item(idx, full);
            if sel.is_empty() {
                continue;
            }
            let piece = if sel.is_full() {
                m.text.clone()
            } else {
                slice_text_with_entities(&m.text, usize::from(sel.from), usize::from(sel.to))
            };
            if piece.text.is_empty() {
                continue;
            }
            if !combined.text.is_empty() {
                combined.text.push('\n');
            }
            let base_u16 = combined.utf16_len() as i32;
            combined.text.push_str(&piece.text);
            for e in piece.entities {
                combined.entities.push(tg_text::EntityInText::with_data(
                    e.entity_type(),
                    e.offset() + base_u16,
                    e.length(),
                    e.data(),
                ));
            }
        }
        combined.sort_entities();
        CopiedRichText { content: combined }
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

    /// Requests scrolling to the newest message at the bottom.
    pub fn request_scroll_to_bottom(&mut self) {
        self.list.scroll_top_state = tg_history::ScrollTopState::default();
        self.pending_scroll_to_bottom = true;
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
        let loaded = self.messages.iter().any(|m| m.id == msg_id);
        if !loaded {
            let slice = backend.load_slice_around(self.peer_id, msg_id, 40, 40);
            self.apply_slice(self.peer_id, slice, peer_kind, false);
        }
        self.pending_jump_msg = Some(msg_id);
        self.pending_scroll_to_bottom = false;
        let full = to_hist_full(FullMsgId {
            peer: self.peer_id,
            msg: msg_id,
        });
        self.highlighter
            .highlight(HighlightTarget::whole_message(full), now_ms);
    }

    /// Loads or reloads a peer's initial slice from `backend`.
    pub fn load_peer(
        &mut self,
        peer_id: PeerId,
        around_id: MsgId,
        backend: &dyn HistoryBackend,
        peer_kind: PeerKind,
    ) {
        let slice = backend.load_slice_around(peer_id, around_id, 60, 40);
        for msg in &slice.messages {
            if let Some(desc) = backend.inline_media_descriptor(peer_id, msg.id) {
                self.media_runtime.register_descriptor(msg.id, desc);
            }
        }
        self.apply_slice(peer_id, slice, peer_kind, around_id == MsgId::SHOW_AT_END);
    }

    /// Replaces loaded messages with `slice` and rebuilds `HistoryList` metadata + estimated heights.
    pub fn apply_slice(
        &mut self,
        peer_id: PeerId,
        slice: HistorySlice,
        peer_kind: PeerKind,
        scroll_to_bottom: bool,
    ) {
        let switching_peer = self.peer_id != peer_id;
        self.peer_id = peer_id;
        self.messages = slice.messages;
        self.unread_count = slice.unread_count;
        self.layout_cache.clear();
        if switching_peer {
            self.list.selection.clear();
            self.highlighter.clear();
            self.context_menu.close();
            self.context_menu_msg = None;
            self.context_menu_engagement = None;
            self.scroll_offset_y = 0.0;
            self.emoji_interactions.clear();
            self.last_bubble_anchors.clear();
            self.read_metrics =
                Some(tg_app_core::core_types::read_metrics::ReadMetricsTracker::new(peer_id));
        }

        let is_group = matches!(peer_kind, PeerKind::Group);
        let items: Vec<HistoryListItem> = self
            .messages
            .iter()
            .map(|m| build_list_item(m, is_group))
            .collect();

        self.list.loaded_at_top = slice.loaded_at_top;
        self.list.loaded_at_bottom = slice.loaded_at_bottom;
        self.list.set_items(items);

        let unread_idx = slice
            .first_unread_id
            .and_then(|uid| self.messages.iter().position(|m| m.id == uid));
        self.list.set_unread_bar(unread_idx);
        self.update_date_and_unread_heights();
        self.media_runtime
            .sync_peer_audio_tracks(peer_id, &self.messages);

        if scroll_to_bottom || switching_peer {
            self.pending_scroll_to_bottom = true;
        }
    }

    /// Appends a newly sent/received message to the bottom if loaded at bottom.
    pub fn append_message(&mut self, msg: HistoryMessage, peer_kind: PeerKind) {
        if !self.list.loaded_at_bottom {
            return;
        }
        if let Some(prev_last) = self.messages.last() {
            self.layout_cache.remove(&prev_last.id);
        }
        let is_group = matches!(peer_kind, PeerKind::Group);
        let item = build_list_item(&msg, is_group);
        self.messages.push(msg);
        self.list.push_back(item);
        self.update_date_and_unread_heights();
        self.media_runtime
            .sync_peer_audio_tracks(self.peer_id, &self.messages);
        self.pending_scroll_to_bottom = true;
    }

    /// Updates an edited or reaction-changed message in place and invalidates its cached layout.
    pub fn update_message_in_place(&mut self, updated: HistoryMessage) {
        if let Some(slot) = self.messages.iter_mut().find(|m| m.id == updated.id) {
            self.layout_cache.remove(&updated.id);
            *slot = updated;
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
        }
        let is_group = matches!(peer_kind, PeerKind::Group);
        let items: Vec<HistoryListItem> = self
            .messages
            .iter()
            .map(|m| build_list_item(m, is_group))
            .collect();
        self.list.set_items(items);
        self.list
            .selection
            .selected_items
            .retain(|id| !msg_ids.contains(&MsgId(id.msg.0 as i32)));
        self.update_date_and_unread_heights();
    }

    /// Prepends older messages while keeping the user's viewport anchored to the exact same message!
    pub fn prepend_older_slice(&mut self, older: HistorySlice, peer_kind: PeerKind) {
        if older.messages.is_empty() {
            self.list.loaded_at_top = true;
            return;
        }
        let vis_top = self.scroll_offset_y.round() as i32;
        let vis_bot = (self.scroll_offset_y + self.last_viewport_h).round() as i32;
        self.list.save_scroll_state(vis_top, vis_bot);

        let is_group = matches!(peer_kind, PeerKind::Group);
        let older_items: Vec<HistoryListItem> = older
            .messages
            .iter()
            .map(|m| build_list_item(m, is_group))
            .collect();

        if let Some(prev_first) = self.messages.first() {
            self.layout_cache.remove(&prev_first.id);
        }
        let mut merged = older.messages;
        merged.append(&mut self.messages);
        self.messages = merged;
        self.list.loaded_at_top = older.loaded_at_top;
        self.list.prepend_older(older_items);
        self.update_date_and_unread_heights();

        if let Some(restored) = self.list.restore_scroll_top() {
            self.scroll_offset_y = restored as f32;
            self.pending_scroll_y = Some(restored as f32);
        }
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
            let item = build_list_item(&msg, is_group);
            self.messages.push(msg);
            self.list.push_back(item);
        }
        self.list.loaded_at_bottom = newer.loaded_at_bottom;
        self.update_date_and_unread_heights();
    }

    /// Extracts concatenated selected text (from either partial drag selection or multi-selected items).
    #[must_use]
    pub fn extract_selected_text(&self) -> String {
        if self.list.selection.has_selected_items() {
            let mut out = Vec::new();
            for m in &self.messages {
                let full = to_hist_full(FullMsgId {
                    peer: self.peer_id,
                    msg: m.id,
                });
                if self.list.selection.selected_items.contains(&full) {
                    let body = if m.text.is_empty() {
                        m.preview_snippet()
                    } else {
                        m.text.text.clone()
                    };
                    out.push(format!("{}: {}", m.sender_name, body));
                }
            }
            return out.join("\n");
        }
        let mut parts = Vec::new();
        for (idx, m) in self.messages.iter().enumerate() {
            let full = to_hist_full(FullMsgId {
                peer: self.peer_id,
                msg: m.id,
            });
            let sel = self.list.selection.selection_for_item(idx, full);
            if sel.is_empty() {
                continue;
            }
            if sel.is_full() {
                parts.push(m.text.text.clone());
            } else {
                parts.push(slice_by_char_range(&m.text.text, sel.from, sel.to));
            }
        }
        parts.join("\n")
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
            self.layout_wide = is_chat_wide;
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
            );
            let new_h = cached_layout_height(&cached);
            if self.list.items()[v.index].height != new_h {
                self.list.items_mut()[v.index].height = new_h;
                heights_changed = true;
            }
            self.layout_cache.insert(msg.id, cached);
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
        let mut events = Vec::new();
        let fonts = FontMetrics::default();
        let viewport_w = list_rect.width().round().max(240.0) as i32;
        let viewport_h = list_rect.height().max(80.0);
        self.last_viewport_h = viewport_h;

        let now_ms = (ui.ctx().input(|i| i.time) * 1000.0).max(0.0) as i64;
        self.last_now_ms = now_ms;
        self.highlighter.tick(now_ms);
        if self.highlighter.is_animating(now_ms) || self.inline_kb_state.loading_count() > 0 {
            ui.ctx().request_repaint();
        }

        // Handle Ctrl+C copy shortcut for text/multi selection, Escape to clear selection.
        if !self.list.selection.is_empty() {
            if ui.ctx().input(|i| i.key_pressed(Key::Escape)) {
                self.list.selection.clear();
            } else if ui
                .ctx()
                .input(|i| i.modifiers.command && i.key_pressed(Key::C))
            {
                let rich = self.extract_selected_rich_text();
                let text = if !rich.content.text.is_empty() {
                    rich.to_markdown()
                } else {
                    self.extract_selected_text()
                };
                if !text.is_empty() {
                    ui.ctx().copy_text(text);
                    events.push(InnerEvent::CopiedRich {
                        summary: "Copied selection (with formatting)".to_owned(),
                        rich,
                    });
                }
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
            self.layout_wide = is_chat_wide;
        }

        // Paint subtle tdesktop-inspired chat wallpaper background.
        paint_chat_wallpaper(ui.painter(), list_rect, colors);

        let content_pad_bottom = 12;
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
            && let Some(idx) = self.messages.iter().position(|m| m.id == target_id)
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

        self.last_painted_bubble_rects.clear();

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
                let vis_top = viewport.min.y.floor() as i32;
                let vis_bottom = viewport.max.y.ceil() as i32;
                let overscan = 160;
                let visible = self.list.visible_items_top_to_bottom(
                    (vis_top - overscan).max(0),
                    vis_bottom + overscan,
                );

                // Sync live voice/round-video transcription state and limits for visible items.
                self.voice_state.transcribe_limits = backend.transcribe_limits(Some(self.peer_id));
                for v in &visible {
                    self.sync_voice_transcription_for(v.index, backend);
                    self.sync_summary_state_for(v.index, backend);
                }

                // Phase 1: Lazily compute exact layouts ONLY for visible items (+ small overscan).
                let mut heights_changed = false;
                let anchor_top = vis_top;
                let anchor_bot = vis_bottom;
                for v in &visible {
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
                if heights_changed {
                    if pinning_to_bottom {
                        self.list.relayout_positions();
                        let items_top = ((viewport_h as i32)
                            - self.list.items_height()
                            - content_pad_bottom)
                            .max(0);
                        self.list.set_items_top(items_top);
                        self.pending_scroll_to_bottom = true;
                        ui.ctx().request_repaint();
                    } else {
                        self.list.save_scroll_state(anchor_top, anchor_bot);
                        self.list.relayout_positions();
                        let items_top = ((viewport_h as i32)
                            - self.list.items_height()
                            - content_pad_bottom)
                            .max(0);
                        self.list.set_items_top(items_top);
                        if let Some(restored) = self.list.restore_scroll_top() {
                            self.pending_scroll_y = Some(restored as f32);
                        }
                    }
                }

                // Allocate the full content area AFTER Phase 1 so `ScrollArea::end` records the
                // up-to-date `total_height()` on this frame rather than the pre-layout estimate.
                let total_h = (self.list.total_height() as f32).max(viewport_h);
                let (content_rect, bg_resp) =
                    ui.allocate_exact_size(Vec2::new(list_rect.width(), total_h), Sense::click());

                if bg_resp.clicked() && !self.list.selection.has_selected_items() {
                    self.list.selection.clear();
                }

                // Re-query exact visible items after any height updates.
                let visible_paint = self.list.visible_items_top_to_bottom(vis_top, vis_bottom);
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

                // Phase 2: Paint visible items only!
                for v in &visible_paint {
                    let msg = &self.messages[v.index];
                    let item = &self.list.items()[v.index];
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
                        Pos2::new(content_rect.min.x, content_rect.min.y + v.top as f32);
                    let row_rect = Rect::from_min_size(
                        row_origin,
                        Vec2::new(content_rect.width(), item.height as f32),
                    );

                    // Optional search filter highlight tint.
                    if !search_q.is_empty() && msg.text.text.to_lowercase().contains(&search_q) {
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
                        ui.painter().rect_filled(
                            row_rect,
                            CornerRadius::ZERO,
                            colors.select_overlay,
                        );
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

                    // Paint cached message or service layout.
                    if let Some(cached) = self.layout_cache.get(&msg.id).cloned() {
                        match cached {
                            CachedItemLayout::Service(svc_layout) => {
                                let action_taken =
                                    backend.is_no_forwards_request_acted(self.peer_id, msg.id);
                                if let Some(svc_act) = paint_service_message_interactive(
                                    ui,
                                    msg,
                                    &svc_layout,
                                    row_origin,
                                    action_taken,
                                    &peer.summary.title,
                                    self.reduced_motion,
                                    colors,
                                ) {
                                    match svc_act {
                                        ServiceCardAction::NoForwards(nfr_act) => match nfr_act {
                                            crate::history::no_forwards_request::NoForwardsRequestAction::Accept(mid) => {
                                                self.layout_cache.remove(&mid);
                                                events.push(InnerEvent::RespondNoForwardsRequest {
                                                    msg_id: mid,
                                                    accept: true,
                                                });
                                            }
                                            crate::history::no_forwards_request::NoForwardsRequestAction::Decline(mid) => {
                                                self.layout_cache.remove(&mid);
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
                                let show_name = show_sender_in_chat
                                    && !msg.outgoing
                                    && !item.attach_state.attached_to_previous;
                                let summary_entry = backend.message_summary(self.peer_id, msg.id);
                                let paused_chat_effects =
                                    !self.media_runtime.policy.allows_chat_effects();
                                let paint_events = paint_message_element_full(
                                    ui,
                                    self.peer_id,
                                    msg,
                                    &msg_layout,
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
                                );
                                for ev in paint_events {
                                    match ev {
                                        MessagePaintEvent::JumpToMessage(tid) => {
                                            jump_to_msg = Some(tid);
                                        }
                                        MessagePaintEvent::OpenLink(url) => {
                                            events.push(InnerEvent::OpenLink(url));
                                        }
                                        MessagePaintEvent::ToggleReaction { msg_id, emoji } => {
                                            events
                                                .push(InnerEvent::ToggleReaction { msg_id, emoji });
                                        }
                                        MessagePaintEvent::Media(mev) => match mev {
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
                                                self.layout_cache.remove(&msg_id);
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
                                            self.list.selection.update_drag(
                                                ListSelectionEndpoint {
                                                    item_id: full_id,
                                                    item_index: v.index,
                                                    text: FlatSelectionEndpoint::new(
                                                        char_index, false,
                                                    ),
                                                },
                                            );
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
                                            self.layout_cache.remove(&mid);
                                            events.push(InnerEvent::ToggleSummary(mid));
                                        }
                                        MessagePaintEvent::AboutAiSummary(mid) => {
                                            events.push(InnerEvent::AboutAiSummary(mid));
                                        }
                                    }
                                }

                                // Fact-check bubble footer (`data/components/factchecks.cpp`)
                                if let Some(ex) = backend.as_layer233_extras()
                                    && let Some(fc) = ex.cached_factcheck(self.peer_id, msg.id)
                                {
                                    let bubble_rect = crate::history::bubble::hist_rect_to_egui(
                                        msg_layout.geometry,
                                        row_origin.x,
                                        row_origin.y,
                                    );
                                    let expanded = self.expanded_factchecks.contains(&msg.id);
                                    let fc_w = (bubble_rect.width() - 20.0).max(100.0);
                                    let fc_h =
                                        crate::history::factcheck::measure_factcheck_footer_height(
                                            &fc, expanded, fc_w,
                                        );
                                    let bottom_reserve = if msg.is_post
                                        && msg.extra.replies_count.is_some()
                                    {
                                        48.0
                                    } else {
                                        24.0
                                    };
                                    let fc_rect = Rect::from_min_size(
                                        Pos2::new(
                                            bubble_rect.left() + 10.0,
                                            (bubble_rect.bottom() - bottom_reserve - fc_h)
                                                .max(bubble_rect.top() + 8.0),
                                        ),
                                        Vec2::new(fc_w, fc_h),
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
                                                self.layout_cache.remove(&mid);
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
                        if self.bubble_hover_quick_actions.is_animating() {
                            ui.ctx().request_repaint();
                        }
                        if qa_opacity > 0.02
                            && let Some(qa_out) =
                                crate::history::quick_action::show_bubble_hover_quick_actions(
                                    ui,
                                    bubble_rect,
                                    row_rect,
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

                // Pause any inline GIFs / round videos / stickers that scrolled offscreen.
                self.media_runtime.finish_frame(now_ms);
                if let Some(ref mut tracker) = self.read_metrics {
                    let finalized = tracker.end_batch();
                    if !finalized.is_empty() {
                        backend.report_read_metrics(self.peer_id, finalized);
                    }
                }

                // Phase 3: Sticky group userpics (`HistoryList::enumerate_userpics`).
                if show_sender_in_chat {
                    let ui_colors = colors.ui_colors();
                    let userpics = self.list.enumerate_userpics(vis_top, vis_bottom);
                    for up in userpics {
                        if let Some(msg) = self.messages.get(up.item_index) {
                            let avatar_rect = Rect::from_min_size(
                                Pos2::new(
                                    content_rect.min.x + HISTORY_PHOTO_LEFT as f32,
                                    content_rect.min.y + up.top as f32,
                                ),
                                Vec2::splat(MSG_PHOTO_SIZE as f32),
                            );
                            EmptyUserpic::for_peer(
                                msg.from_id.bare(),
                                &msg.sender_name,
                                &ui_colors,
                            )
                            .paint_circle(ui.painter(), avatar_rect);
                        }
                    }
                }
            });
            debug_assert_eq!(out.id, expected_scroll_id);
            out
        });

        self.last_frame_laid_out_count = laid_out_this_frame;
        self.last_frame_painted_count = painted_this_frame;

        // Paint interactive emoji tap burst overlays above the message list.
        self.emoji_interactions
            .paint_overlays(ui, list_rect, now_ms.max(0) as u64, colors);

        // Track scroll state & bottom-edge detection.
        self.scroll_offset_y = scroll_out.inner.state.clamped_offset().y;
        let vis_top = self.scroll_offset_y.round() as i32;
        let vis_bot = (self.scroll_offset_y + viewport_h).round() as i32;
        let total_h = self.list.total_height();
        self.scrolled_up = (total_h - vis_bot) > 120;

        // Automatic preload triggers (`HistoryList::check_preload`).
        let preload = self.list.check_preload(vis_top, vis_bot);
        if preload.preload_top
            && let Some(first) = self.messages.first()
        {
            let older = backend.load_slice_before(self.peer_id, first.id, 40);
            self.prepend_older_slice(older, peer.summary.kind);
        } else if preload.preload_bottom
            && let Some(last) = self.messages.last()
        {
            let newer = backend.load_slice_after(self.peer_id, last.id, 40);
            self.append_newer_slice(newer, peer.summary.kind);
        }

        // Handle reply-header jump clicks.
        if let Some(tid) = jump_to_msg {
            self.jump_to_message(tid, backend, peer.summary.kind, now_ms);
        }

        // Right-click context menu (`PopupMenuState` from `tg-ui`).
        let ui_colors = colors.ui_colors();
        let menu_style = PopupMenuStyle::default_popup(&ui_colors);
        let toggle_style = ToggleStyle::default_toggle(&ui_colors);
        let now_unix_secs = (now_ms / 1000).max(0);
        if let Some((mid, pos)) = open_context_menu_for {
            self.context_menu_msg = Some(mid);
            self.context_menu_pos = pos;
            self.context_menu_engagement = self.messages.iter().find(|m| m.id == mid).map(|m| {
                crate::history::read_receipts::ContextMenuEngagementInfo::resolve(
                    peer,
                    m,
                    now_unix_secs,
                    backend,
                )
            });
            self.context_menu.open_at(ui.ctx(), pos, &menu_style);
        }

        if let Some(target_id) = self.context_menu_msg
            && let Some(target_msg) = self.messages.iter().find(|m| m.id == target_id).cloned()
        {
            let has_quote_sel = self.selected_quote_for_msg(target_id).is_some();
            let ref_now_unix = now_unix_secs.max(i64::from(target_msg.date_unix) + 60);
            let mut items = build_context_menu_items(&target_msg, peer, has_quote_sel);
            if let Some(ref eng) = self.context_menu_engagement
                && eng.peer_id == self.peer_id
                && eng.msg_id == target_id
            {
                eng.append_menu_items(&mut items, ref_now_unix);
            }
            if let Some(triggered) = self.context_menu.show(
                ui.ctx(),
                Id::unique(("tg_hist_ctx_menu", self.peer_id.0)),
                &items,
                &menu_style,
                &toggle_style,
            ) {
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
                    "react_thumbsup" => events.push(InnerEvent::ToggleReaction {
                        msg_id: target_id,
                        emoji: "👍".to_owned(),
                    }),
                    "react_fire" => events.push(InnerEvent::ToggleReaction {
                        msg_id: target_id,
                        emoji: "🔥".to_owned(),
                    }),
                    "react_heart" => events.push(InnerEvent::ToggleReaction {
                        msg_id: target_id,
                        emoji: "❤️".to_owned(),
                    }),
                    "react_more" => events.push(InnerEvent::OpenReactionsPicker {
                        msg_id: target_id,
                        pos: self.context_menu_pos,
                    }),
                    "who_reacted" => events.push(InnerEvent::OpenWhoReacted(target_id)),
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
                    "copy_text" => {
                        let rich = if !self.list.selection.is_empty() {
                            self.extract_selected_rich_text()
                        } else if !target_msg.text.is_empty() {
                            CopiedRichText {
                                content: target_msg.text.clone(),
                            }
                        } else {
                            CopiedRichText {
                                content: tg_text::TextWithEntities::simple(
                                    target_msg.preview_snippet(),
                                ),
                            }
                        };
                        let copy_str = rich.to_markdown();
                        ui.ctx().copy_text(copy_str);
                        events.push(InnerEvent::CopiedRich {
                            summary: "Copied text with formatting".to_owned(),
                            rich,
                        });
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
                item.height = cached_layout_height(cached);
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
        let cached = compute_item_layout_egui(
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
        );
        let new_h = cached_layout_height(&cached);
        let changed = self.list.items()[index].height != new_h;
        if changed {
            self.list.items_mut()[index].height = new_h;
        }
        self.layout_cache.insert(msg.id, cached);
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

    fn factcheck_extra_height(&self, msg_id: MsgId, backend: &dyn HistoryBackend) -> i32 {
        backend
            .as_layer233_extras()
            .and_then(|ex| ex.cached_factcheck(self.peer_id, msg_id))
            .map_or(0, |fc| {
                crate::history::factcheck::measure_factcheck_footer_height(
                    &fc,
                    self.expanded_factchecks.contains(&msg_id),
                    320.0,
                )
                .ceil() as i32
                    + 8
            })
    }
}

fn cached_layout_height(cached: &CachedItemLayout) -> i32 {
    match cached {
        CachedItemLayout::Message(m) => m.current_size.height.max(24),
        CachedItemLayout::Service(s) => s.current_size.height.max(24),
    }
}

fn build_list_item(msg: &HistoryMessage, is_group: bool) -> HistoryListItem {
    let full_id = to_hist_full(FullMsgId {
        peer: msg.peer_id,
        msg: msg.id,
    });
    let pos = MessagePosition {
        full_id,
        date: msg.date_unix,
    };
    let unwrapped = matches!(
        msg.media,
        Some(HistoryMedia::Sticker(_) | HistoryMedia::RoundVideo(_))
    ) && msg.text.is_empty();
    let meta = AttachMetadata {
        date: msg.date_unix,
        date_time: LocalDateTime::from_unix_utc(i64::from(msg.date_unix)),
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
    item.has_from_photo = is_group && !msg.outgoing && !msg.is_service();
    item
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
        Some(HistoryMedia::RoundVideo(rv)) => rv.diameter_px.max(180),
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
        let text = if entry.too_long {
            "Audio is too long to transcribe."
        } else if entry.failed {
            "Transcription unavailable."
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

fn build_media_input(
    msg: &HistoryMessage,
    has_header: bool,
    has_text: bool,
    bottom_info_w: i32,
    transcribe_text_height: i32,
) -> Option<MediaInput> {
    let media = msg.media.as_ref()?;
    Some(match media {
        HistoryMedia::Photo(p) => MediaInput::Photo(PhotoInput {
            dimensions: Size::new(p.width, p.height),
            service_width: 0,
            has_bubble: has_header || has_text,
            min_width_for_media: bottom_info_w + 24,
            textual_max_width: if has_text { 320 } else { 0 },
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
            textual_max_width: if has_text { 320 } else { 0 },
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
        HistoryMedia::RoundVideo(rv) => MediaInput::Sticker(StickerInput {
            kind: StickerKind::Regular,
            dimensions: Size::new(rv.diameter_px, rv.diameter_px + transcribe_text_height),
            custom_size: (rv.diameter_px + transcribe_text_height).clamp(160, 380),
            option_sticker_size: None,
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
    let mut time_chars = msg.formatted_time().len() as i32;
    if msg.edited {
        time_chars += 8;
    }
    if let Some(ref bot) = msg.extra.via_bot {
        time_chars += (bot.chars().count() as i32) + 6;
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
        let text = svc.display_text(&msg.sender_name);
        let max_w = (viewport_w - 60).max(120) as f32;
        let single_w = ui
            .painter()
            .layout_no_wrap(text.clone(), FontId::proportional(12.5), colors.service_fg)
            .size()
            .x
            .ceil() as i32;
        let wrap_h = ui
            .painter()
            .layout(text, FontId::proportional(12.5), colors.service_fg, max_w)
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
    let max_text_wrap = ((viewport_w - 130).min(MSG_MAX_WIDTH) - MSG_PADDING.horizontal()).max(120);
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
        } else if active_text.is_empty() {
            (0, 0, 0)
        } else {
            measure_rich_text(
                ui,
                active_text,
                &text_pal,
                colors.text_fg,
                max_text_wrap as f32,
            )
        };
    if summary_h > 0 {
        let min_summary_w = 216.min(max_text_wrap);
        text_max_w = text_max_w.max(min_summary_w);
        text_real_w = text_real_w.max(min_summary_w);
    }

    let show_name = show_sender_in_chat && !msg.outgoing && !item.attach_state.attached_to_previous;
    let from_name_width = show_name.then(|| {
        ui.painter()
            .layout_no_wrap(
                msg.sender_name.clone(),
                FontId::proportional(13.0),
                colors.text_fg,
            )
            .size()
            .x
            .ceil() as i32
    });
    let forwarded_width = msg.forward.as_ref().map(|f| {
        ui.painter()
            .layout_no_wrap(
                format!("Forwarded from {}", f.from_name),
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
    let bottom_info = build_bottom_info_input(msg);
    let bottom_info_w = bottom_info.author_edited_date_width + bottom_info.views_text_width + 24;
    let lp_extra_h = msg
        .extra
        .link_preview
        .as_ref()
        .map_or(0, |lp| link_preview_card_height(lp) + 8);
    let has_lp = lp_extra_h > 0;
    let has_body_text = has_rich_body || !msg.text.is_empty() || has_lp;
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
        bottom_info_w,
        transcribe_text_height,
    );

    // Extra vertical space so bottom-right timestamp never overlaps the final text line or preview card.
    let needs_date_line_room = has_body_text && msg.reactions.is_empty();
    let adjusted_text_h = if needs_date_line_room
        && (has_rich_body
            || has_lp
            || summary_h > 0
            || text_max_w + bottom_info_w + 18 > max_text_wrap)
    {
        text_wrapped_h + summary_h + lp_extra_h + 18 + factcheck_extra_h
    } else {
        text_wrapped_h + summary_h + lp_extra_h + factcheck_extra_h
    };

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
            let kb_max_w = (viewport_w - 130).clamp(200, MSG_MAX_WIDTH);
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
        reactions_size: measure_reactions_size(&msg.reactions),
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

fn compute_item_layout_headless(
    msg: &HistoryMessage,
    item: &HistoryListItem,
    viewport_w: i32,
    is_chat_wide: bool,
    show_sender_in_chat: bool,
    fonts: &FontMetrics,
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
        let text = svc.display_text(&msg.sender_name);
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
    let max_text_wrap = ((viewport_w - 130).min(MSG_MAX_WIDTH) - MSG_PADDING.horizontal()).max(120);
    let has_rich_body = msg.extra.rich_message.is_some();
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
    } else {
        estimate_text_metrics(&msg.text.text, max_text_wrap)
    };
    let show_name = show_sender_in_chat && !msg.outgoing && !item.attach_state.attached_to_previous;
    let from_name_width = show_name.then(|| (msg.sender_name.chars().count() as i32) * 8);
    let forwarded_width = msg
        .forward
        .as_ref()
        .map(|f| ((f.from_name.chars().count() + 15) as i32) * 7);
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
    let has_body_text = has_rich_body || !msg.text.is_empty() || has_lp;
    let tw = if has_lp {
        tw.max(240.min(max_text_wrap))
    } else {
        tw
    };
    let th = th + lp_extra_h + if has_lp { 18 } else { 0 };
    let media = build_media_input(msg, has_header, has_body_text, bottom_info_w, 0);

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
            let kb_max_w = (viewport_w - 130).clamp(200, MSG_MAX_WIDTH);
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
        reactions_size: measure_reactions_size(&msg.reactions),
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

fn build_context_menu_items(
    msg: &HistoryMessage,
    peer: &HistoryPeerInfo,
    has_quote_selection: bool,
) -> Vec<MenuItem> {
    let mut items = vec![
        MenuItem::action("react_thumbsup", "React with Thumbs Up").with_icon(IconKind::Star),
        MenuItem::action("react_fire", "React with Fire").with_icon(IconKind::Star),
        MenuItem::action("react_heart", "React with Heart").with_icon(IconKind::Star),
        MenuItem::action("react_more", "More Reactions...").with_icon(IconKind::Plus),
    ];

    if !msg.reactions.is_empty() {
        let total_reacts: u32 = msg.reactions.iter().map(|r| r.count).sum();
        items.push(
            MenuItem::action("who_reacted", format!("Who Reacted ({total_reacts})..."))
                .with_icon(IconKind::Info),
        );
    }

    items.push(MenuItem::Separator);
    items.push(
        MenuItem::action("reply", "Reply")
            .with_icon(IconKind::Back)
            .with_shortcut("Double-click"),
    );

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

    if has_quote_selection {
        items.push(MenuItem::action("reply_quote", "Reply with Quote").with_icon(IconKind::Back));
    }

    if msg.extra.replies_count.is_some() && msg.is_post {
        items.push(MenuItem::action("open_comments", "Open Comments").with_icon(IconKind::Info));
    }

    if msg.outgoing
        && (!msg.text.is_empty() || msg.extra.rich_message.is_some())
        && peer.summary.can_send
    {
        items.push(MenuItem::action("edit", "Edit").with_icon(IconKind::Edit));
    }

    items.push(
        MenuItem::action("copy_text", "Copy Text")
            .with_icon(IconKind::Bookmark)
            .with_shortcut("Ctrl+C"),
    );
    let copy_link_label = if msg.is_post {
        tg_lang::tr::lng_context_copy_post_link()
    } else {
        tg_lang::tr::lng_context_copy_message_link()
    };
    items.push(MenuItem::action("copy_link", copy_link_label).with_icon(IconKind::Info));

    if !msg.text.is_empty() {
        let tr_label = if msg.extra.translated_text.is_some() {
            "Show Original"
        } else {
            "Translate"
        };
        items.push(MenuItem::action("translate", tr_label).with_icon(IconKind::Info));
    }

    items.push(MenuItem::action("forward", "Forward").with_icon(IconKind::ChevronRight));

    if peer.can_pin {
        items.push(
            MenuItem::action("pin", if msg.is_pinned { "Unpin" } else { "Pin" })
                .with_icon(IconKind::Star),
        );
    }

    items.push(MenuItem::action("select", "Select (Shift+Click range)").with_icon(IconKind::Check));

    if matches!(msg.media, Some(HistoryMedia::Video(ref v)) if v.is_gif) {
        items.push(MenuItem::action("save_gif", "Save GIF").with_icon(IconKind::Star));
    }

    if matches!(msg.media, Some(HistoryMedia::Sticker(_))) {
        items.push(
            MenuItem::action("fave_sticker", "Add Sticker to Favorites").with_icon(IconKind::Star),
        );
    }

    if matches!(msg.media, Some(HistoryMedia::Voice(_)))
        || matches!(msg.media, Some(HistoryMedia::Document(ref d)) if d.mime_type.starts_with("audio/"))
    {
        items.push(
            MenuItem::action("save_ringtone", "Save for Notifications").with_icon(IconKind::Star),
        );
    }

    if matches!(msg.media, Some(HistoryMedia::Photo(_))) && peer.summary.can_send {
        items.push(MenuItem::action("draw_to_reply", "Draw to Reply").with_icon(IconKind::Edit));
    }

    if msg.media.is_some() {
        items.push(MenuItem::action("save_as", "Save As...").with_icon(IconKind::Attach));
    }

    if !msg.outgoing {
        items.push(MenuItem::action("report", "Report").with_icon(IconKind::Info));
    }

    if peer.can_delete {
        items.push(MenuItem::Separator);
        if can_moderate_message(msg, peer) {
            items.push(
                MenuItem::action("moderate", "Moderate...")
                    .with_icon(IconKind::Trash)
                    .attention(),
            );
        }
        items.push(
            MenuItem::action("delete", "Delete")
                .with_icon(IconKind::Trash)
                .attention(),
        );
    }

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

fn paint_chat_wallpaper(painter: &egui::Painter, rect: Rect, colors: &HistoryColors) {
    painter.rect_filled(rect, CornerRadius::ZERO, colors.canvas_bg);
    // Subtle repeating geometric pattern evoking tdesktop's classic wallpaper texture.
    let step = 44.0;
    let start_x = rect.left() + 22.0;
    let start_y = rect.top() + 22.0;
    let mut row = 0usize;
    let mut y = start_y;
    while y < rect.bottom() {
        let mut x = start_x + if row % 2 == 1 { step * 0.5 } else { 0.0 };
        while x < rect.right() {
            painter.circle_filled(Pos2::new(x, y), 1.6, colors.canvas_pattern);
            x += step;
        }
        y += step;
        row += 1;
    }
}

/// Formats a Unix timestamp as a Telegram-style centered date separator (`"November 21"`, `"November 22"`).
#[must_use]
pub fn format_date_separator(unix_secs: i32) -> String {
    let dt = LocalDateTime::from_unix_utc(i64::from(unix_secs));
    format_civil_date(dt.date)
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
