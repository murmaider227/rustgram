//! Compose controls: multi-line markdown input field, reply/quote/edit/forward (with hide sender/
//! caption options) preview bars, link preview editor, inline bot results panel hook, spellcheck
//! wavy underlines & suggestions (`tg-platform`), voice recording hold/lock + live waveform
//! (`tg-audio`), round video recording, send-as peer chooser, slow mode timer,
//! silent send, message effects picker, Enter vs Ctrl+Enter shortcut, draft save debouncer
//! (per-thread), and typing action throttle.
//!
//! Port origin: `Telegram/SourceFiles/history/view/controls/history_view_compose_controls.cpp`,
//! `Telegram/SourceFiles/history/history_widget.cpp` (`kSaveDraftTimeout`,
//! `kSaveDraftAnywayTimeout`, `kSaveCloudDraftIdleTimeout`), and
//! `Telegram/SourceFiles/api/api_send_progress.cpp` (`kSendMyTypingInterval`).

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Event, FontId, Id, Key, Pos2, Rect, Role,
    ScrollArea, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, WidgetInfo,
};
use tg_text::TextWithEntities;
use tg_ui::widgets::buttons::{IconKind, paint_icon};
use tg_ui::widgets::rich_input::{
    EditLinkDialogAction, EditLinkDialogState, FormatAction, FormattingContextMenu,
    FormattingMenuEvent, RichEditState, RichInputPalette, detect_format_shortcut,
    show_rich_text_edit,
};

use crate::bots::menus::{BotMenuButtonAction, paint_bot_menu_button};
use crate::bots::reply_keyboard::paint_reply_keyboard_toggle_button;
use crate::history::backend::{
    DraftMessage, HistoryBackend, SendMessageExtended, ThreadDraft, TypingAction, VoiceAttachment,
};
use crate::history::compose_extras::{
    ComposeSpellchecker, InlineBotPanelState, InlineBotQueryHook, LINK_PREVIEW_BAR_HEIGHT,
    LinkPreviewEditorState, MessageEffectsPickerState, RecordButtonMode, RoundVideoRecorderState,
    SendAsChooserState, VoiceBarAction, VoiceRecorderController, paint_slow_mode_badge,
    paint_voice_recording_bar,
};
use crate::history::rich_compose::{
    ComposeHistoryShortcut, FormatStripAction, MentionAutocompleteState,
    QuickReplyAutocompleteState, RichThreadDraft, detect_compose_history_shortcut,
    insert_mention_candidate, paint_active_format_strip,
};
use crate::history::sections_model::{
    CopiedRichText, ForwardOptions, InlineBotResult, ReplyQuote, RoundVideoAttachment, SendAsPeer,
};
use crate::history::text_bridge::parse_compose_markdown;
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind};

/// Local draft debounce delay after typing stops (`kSaveDraftTimeout = 1000ms` in `history_widget.cpp:239`).
pub const SAVE_DRAFT_TIMEOUT_MS: u64 = 1_000;
/// Maximum draft save delay while continuously typing (`kSaveDraftAnywayTimeout = 5000ms`, `history_widget.cpp:240`).
pub const SAVE_DRAFT_ANYWAY_TIMEOUT_MS: u64 = 5_000;
/// Cloud draft idle sync timeout (`kSaveCloudDraftIdleTimeout = 14000ms`, `history_widget.cpp:241`).
pub const SAVE_CLOUD_DRAFT_IDLE_TIMEOUT_MS: u64 = 14_000;
/// Minimum interval between outbound typing actions (`kSendMyTypingInterval = 5000ms`, `api_send_progress.cpp:23`).
pub const SEND_MY_TYPING_INTERVAL_MS: u64 = 5_000;

/// Height of the reply/edit/forward preview strip above the compose field.
pub const PREVIEW_BAR_HEIGHT: f32 = 46.0;
/// Base height of a single-line compose input row.
pub const COMPOSE_MIN_HEIGHT: f32 = 48.0;
/// Maximum height of the auto-growing multi-line compose input row.
pub const COMPOSE_MAX_HEIGHT: f32 = 160.0;

/// Keyboard shortcut setting controlling how Enter submits messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SendShortcut {
    /// `Enter` sends; `Shift+Enter` or `Ctrl+Enter` inserts a newline (tdesktop default).
    #[default]
    Enter,
    /// `Ctrl+Enter` sends; `Enter` inserts a newline.
    CtrlEnter,
}

impl SendShortcut {
    /// Returns `true` when the given key press and modifier flags should submit the message.
    #[must_use]
    pub const fn should_send(self, enter_pressed: bool, shift: bool, ctrl_or_cmd: bool) -> bool {
        if !enter_pressed {
            return false;
        }
        match self {
            Self::Enter => !shift && !ctrl_or_cmd,
            Self::CtrlEnter => ctrl_or_cmd && !shift,
        }
    }
}

/// Active header preview bar above the compose field (`Reply` / `Edit` / `Forward`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeMode {
    /// Normal message composition.
    Normal,
    /// Replying to an existing message (with optional selected quote).
    Reply {
        /// Target message id.
        msg_id: MsgId,
        /// Target author name.
        author: String,
        /// Target preview snippet.
        preview: String,
        /// Optional quoted passage selected from the target message.
        quote: Option<ReplyQuote>,
    },
    /// Editing an existing outgoing message.
    Edit {
        /// Message id being edited.
        msg_id: MsgId,
        /// Original preview snippet.
        original_preview: String,
    },
    /// Forwarding one or more messages into the current chat (with hide sender/captions options).
    Forward {
        /// Source peer id.
        from_peer: PeerId,
        /// Message ids queued for forwarding.
        msg_ids: Vec<MsgId>,
        /// Short summary label (e.g. `"3 messages from Rustgram Core Port"`).
        summary: String,
        /// Hide sender name / hide media captions flags.
        options: ForwardOptions,
    },
}

/// Deterministic draft-save timer matching `history_widget.cpp:239-2425`.
#[derive(Debug, Clone, Default)]
pub struct DraftSaveScheduler {
    dirty: bool,
    first_dirty_ms: Option<u64>,
    last_edit_ms: Option<u64>,
    cloud_synced: bool,
}

/// Decision emitted by [`DraftSaveScheduler::poll`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftSaveDue {
    /// Nothing due yet.
    None,
    /// Save local draft (1s debounce or 5s continuous cap).
    Local,
    /// Save cloud draft (14s idle sync).
    Cloud,
}

impl DraftSaveScheduler {
    /// Marks the draft dirty at timestamp `now_ms`.
    pub fn mark_edited(&mut self, now_ms: u64) {
        self.dirty = true;
        self.cloud_synced = false;
        if self.first_dirty_ms.is_none() {
            self.first_dirty_ms = Some(now_ms);
        }
        self.last_edit_ms = Some(now_ms);
    }

    /// Evaluates whether a local or cloud draft save is due at `now_ms`.
    pub fn poll(&mut self, now_ms: u64) -> DraftSaveDue {
        if self.dirty {
            let since_last = self.last_edit_ms.map_or(0, |t| now_ms.saturating_sub(t));
            let since_first = self.first_dirty_ms.map_or(0, |t| now_ms.saturating_sub(t));
            if since_last >= SAVE_DRAFT_TIMEOUT_MS || since_first >= SAVE_DRAFT_ANYWAY_TIMEOUT_MS {
                self.dirty = false;
                self.first_dirty_ms = None;
                return DraftSaveDue::Local;
            }
            return DraftSaveDue::None;
        }
        if !self.cloud_synced
            && let Some(last) = self.last_edit_ms
            && now_ms.saturating_sub(last) >= SAVE_CLOUD_DRAFT_IDLE_TIMEOUT_MS
        {
            self.cloud_synced = true;
            return DraftSaveDue::Cloud;
        }
        DraftSaveDue::None
    }

    /// Forces immediate save state reset (e.g. on peer switch or send).
    pub fn reset(&mut self) {
        self.dirty = false;
        self.first_dirty_ms = None;
        self.last_edit_ms = None;
        self.cloud_synced = true;
    }

    /// Returns `true` if there are unsaved local edits.
    #[must_use]
    pub const fn is_dirty(&self) -> bool {
        self.dirty
    }
}

/// Throttle for outbound typing actions (`api_send_progress.cpp:23`, `kSendMyTypingInterval = 5000ms`).
#[derive(Debug, Clone, Default)]
pub struct TypingThrottle {
    last_sent_ms: Option<u64>,
    last_peer: Option<PeerId>,
}

impl TypingThrottle {
    /// Returns `true` if a new typing action should be dispatched for `peer_id` at `now_ms`.
    pub fn should_send(&mut self, peer_id: PeerId, now_ms: u64) -> bool {
        let same_peer = self.last_peer == Some(peer_id);
        let due = match self.last_sent_ms {
            Some(prev) if same_peer => now_ms.saturating_sub(prev) >= SEND_MY_TYPING_INTERVAL_MS,
            _ => true,
        };
        if due {
            self.last_sent_ms = Some(now_ms);
            self.last_peer = Some(peer_id);
        }
        due
    }

    /// Clears throttle state (e.g. after sending a message or clearing input).
    pub fn reset(&mut self) {
        self.last_sent_ms = None;
        self.last_peer = None;
    }
}

/// Actions emitted by [`ComposeControls::show`] and [`ComposeControls::show_extended`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeEvent {
    /// Submit a new message (simple path).
    SendMessage {
        /// Parsed text + entities (`tg_text`).
        text: TextWithEntities,
        /// Optional reply target.
        reply_to: Option<MsgId>,
    },
    /// Submit a new message with full extended compose metadata (silent, quote, send-as,
    /// link preview, message effect, etc.).
    SendMessageExtended(SendMessageExtended),
    /// Submit an edit for an existing message.
    SubmitEdit {
        /// Message id being edited.
        msg_id: MsgId,
        /// Updated parsed text + entities.
        text: TextWithEntities,
    },
    /// Submit pending forwarded messages (plus optional comment text & hide-sender/captions options).
    SubmitForward {
        /// Source peer.
        from_peer: PeerId,
        /// Message ids to forward.
        msg_ids: Vec<MsgId>,
        /// Optional extra comment message.
        comment: Option<TextWithEntities>,
    },
    /// Submit pending forwarded messages with explicit [`ForwardOptions`].
    SubmitForwardWithOptions {
        /// Source peer.
        from_peer: PeerId,
        /// Message ids to forward.
        msg_ids: Vec<MsgId>,
        /// Hide sender / hide media captions flags.
        options: ForwardOptions,
        /// Optional extra comment message.
        comment: Option<TextWithEntities>,
    },
    /// Attach file button clicked (opens Attach Files box).
    AttachClicked,
    /// Open the Rich Document Editor window (`iv/editor/*`).
    OpenRichEditor,
    /// Open Create Poll / Quiz modal box.
    OpenCreatePollBox,
    /// Open Compose with AI modal box (`boxes/compose_ai_box.cpp`).
    OpenComposeAi,
    /// Open Sticker Creator modal box (`boxes/sticker_creator_box.cpp`).
    OpenStickerCreator,
    /// Open Self-Destruct TTL settings modal box (`boxes/self_destruction_box.cpp`).
    OpenSelfDestructBox,
    /// Open Create Checklist (`EditTodoListBox`) modal box.
    OpenCreateTodoBox,
    /// Open Saved Music attachment picker (`MusicAttachBox`).
    OpenMusicAttach,
    /// Open Location & Venues picker modal box.
    OpenLocationPicker,
    /// Open Share Contact picker modal box.
    OpenShareContact,
    /// Open Schedule Message / Set Reminder date-time picker box.
    OpenScheduleBox,
    /// Schedule current draft to send when the recipient comes online (`0x7FFFFFFE`).
    SendWhenOnline(SendMessageExtended),
    /// User picked an inline bot query result (`@bot query`).
    SendInlineResult(InlineBotResult),
    /// Emoji panel toggle button clicked (delegated to `chat_helpers` hook).
    ToggleEmojiPanel,
    /// Typing or recording action due (already throttled to 5s).
    SendTyping(TypingAction),
    /// Debounced draft save triggered (legacy per-peer).
    SaveDraft(Option<DraftMessage>),
    /// Debounced per-thread draft save triggered.
    SaveThreadDraft(Option<ThreadDraft>),
    /// Debounced per-thread rich draft save triggered (preserves `tg-text` entities).
    SaveRichDraft(Option<RichThreadDraft>),
    /// `Up` arrow pressed in empty compose field -> edit last outgoing message.
    EditLastOutgoing,
    /// `Ctrl+Up` (`-1`) or `Ctrl+Down` (`+1`) pressed -> navigate reply target message.
    ReplyNavigate(i32),
    /// Voice recording finished (legacy duration-only event).
    VoiceRecorded {
        /// Duration in seconds.
        duration_secs: u32,
    },
    /// Voice recording finalized via `tg-audio` with real 5-bit waveform.
    VoiceCaptured(VoiceAttachment),
    /// Round video recording placeholder finalized.
    RoundVideoCaptured(RoundVideoAttachment),
    /// User picked a Telegram Business quick-reply shortcut via `/` autocomplete in a private chat.
    SendQuickReply(crate::business::model::QuickReplyShortcut),
    /// User clicked the bot menu button (commands menu or Mini App web view) at the left edge.
    BotMenuButton(BotMenuButtonAction),
    /// User clicked the reply keyboard toggle button in the compose bar.
    ToggleReplyKeyboard,
    /// User clicked the Attach button when bot attach-menu integration is active.
    ToggleBotAttachMenu,
    /// User picked a new Send-As peer identity (`messages.saveDefaultSendAs`).
    SaveDefaultSendAs(PeerId),
    /// User attempted to send while slow mode cooldown still had `remaining_secs` left.
    SlowModeBlocked(u32),
}

/// Optional bot chrome configuration painted inside the compose bar (`history_view_bot_menu_button.cpp`
/// and `history_widget.cpp` reply keyboard toggle).
pub struct ComposeBotChrome<'a> {
    /// Bot menu button configuration (Commands vs WebApp) when active peer is a bot chat.
    pub menu_button: Option<&'a tg_bots::commands::BotMenuButton>,
    /// True if the active chat has bot commands available.
    pub has_bot_commands: bool,
    /// True if the bot commands popup is currently open.
    pub commands_popup_open: bool,
    /// True if the active chat has a reply keyboard attached.
    pub has_reply_keyboard: bool,
    /// True if the reply keyboard panel is currently expanded.
    pub reply_keyboard_visible: bool,
    /// True when the host renders the attach-menu bots popup instead of the legacy attach menu.
    pub use_bot_attach_menu: bool,
    /// True while the attach-menu bots popup is open.
    pub bot_attach_open: bool,
    /// True when the host drives `bots::inline_results::InlineResultsPanel` directly.
    pub use_rich_inline_results: bool,
    /// Active palette for `tg-app::bots` chrome buttons.
    pub palette: &'a tg_app_core::stand_in::theme::Palette,
}

/// Stateful compose area widget (`HistoryView::ComposeControls`).
#[derive(Debug)]
pub struct ComposeControls {
    /// Current draft text in the multi-line input field (kept synchronized with [`Self::rich_edit`]).
    pub text: String,
    /// Entity-aware live rich text editor state (`tg-ui::widgets::rich_input::RichEditState`).
    pub rich_edit: RichEditState,
    /// Right-click Formatting context menu (`Ctrl+B/I/U/Shift+X/M/P/N/K`).
    pub format_menu: FormattingContextMenu,
    /// Active `Ctrl+K` Create/Edit Link modal dialog, if open.
    pub link_dialog: Option<EditLinkDialogState>,
    /// `@mention` autocomplete popup creating `EntityType::MentionName` entities.
    pub mention_autocomplete: MentionAutocompleteState,
    /// Business `/` quick-reply autocomplete popup in private chats.
    pub quick_reply_autocomplete: QuickReplyAutocompleteState,
    /// Whether the active account has Telegram Business quick replies enabled.
    pub business_quick_replies_enabled: bool,
    /// Available Telegram Business quick-reply shortcuts for `/` autocomplete.
    pub business_quick_replies: Vec<crate::business::model::QuickReplyShortcut>,
    /// Whether the compact inline formatting & custom-emoji strip is shown above the field.
    pub show_format_strip: bool,
    /// Active reply / edit / forward preview mode.
    pub mode: ComposeMode,
    /// Send shortcut mode (`Enter` vs `Ctrl+Enter`).
    pub send_shortcut: SendShortcut,
    /// Draft debounce timer.
    pub draft_scheduler: DraftSaveScheduler,
    /// Typing action throttle.
    pub typing_throttle: TypingThrottle,
    /// Live voice recorder controller (`tg-audio`).
    pub voice_recorder: VoiceRecorderController,
    /// Round video recording placeholder state.
    pub round_video_recorder: RoundVideoRecorderState,
    /// Active right-button recording mode (Voice Mic vs Round Video Camera).
    pub record_mode: RecordButtonMode,
    /// Native spellchecker (`tg-platform` `ISpellChecker`).
    pub spellchecker: ComposeSpellchecker,
    /// Link preview detector & editor above the compose field.
    pub link_preview: LinkPreviewEditorState,
    /// Inline bot `@bot query` results panel state & hook.
    pub inline_bot: InlineBotPanelState,
    /// Send-As peer chooser popup state.
    pub send_as: SendAsChooserState,
    /// Message effects picker popup state.
    pub effects: MessageEffectsPickerState,
    /// Silent send toggle ("Send Without Sound").
    pub silent_mode: bool,
    /// True while right-click Send menu is open.
    pub send_menu_open: bool,
    /// True while Attach menu (Files / Poll / Location / Contact) is open.
    pub attach_menu_open: bool,
    /// Internal clipboard buffer preserving `tg-text` entities on rich copy/paste.
    pub copied_rich_buffer: Option<CopiedRichText>,
    /// Legacy flag kept synced with `voice_recorder.is_active()`.
    pub recording_voice: bool,
    /// Legacy timestamp kept synced with `voice_recorder`.
    pub recording_started_at: f64,
    /// Request keyboard focus on next paint.
    pub focus_requested: bool,
    /// True while a `HistoryWidget`-managed bot popup (commands, inline results, attach bots)
    /// is visible above the compose bar, so `MainWidget`'s `FieldAutocompleteView` defers to it.
    pub bot_popup_active: bool,
}

impl Default for ComposeControls {
    fn default() -> Self {
        Self {
            text: String::new(),
            rich_edit: RichEditState::new(),
            format_menu: FormattingContextMenu::default(),
            link_dialog: None,
            mention_autocomplete: MentionAutocompleteState::default(),
            quick_reply_autocomplete: QuickReplyAutocompleteState::default(),
            business_quick_replies_enabled: false,
            business_quick_replies: Vec::new(),
            show_format_strip: false,
            mode: ComposeMode::Normal,
            send_shortcut: SendShortcut::Enter,
            draft_scheduler: DraftSaveScheduler::default(),
            typing_throttle: TypingThrottle::default(),
            voice_recorder: VoiceRecorderController::new(),
            round_video_recorder: RoundVideoRecorderState::default(),
            record_mode: RecordButtonMode::Voice,
            spellchecker: ComposeSpellchecker::new(),
            link_preview: LinkPreviewEditorState::default(),
            inline_bot: InlineBotPanelState::new(),
            send_as: SendAsChooserState::default(),
            effects: MessageEffectsPickerState::default(),
            silent_mode: false,
            send_menu_open: false,
            attach_menu_open: false,
            copied_rich_buffer: None,
            recording_voice: false,
            recording_started_at: 0.0,
            focus_requested: false,
            bot_popup_active: false,
        }
    }
}

impl ComposeControls {
    /// Creates default compose controls.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Installs an external inline bot query hook.
    pub fn set_inline_bot_hook(&mut self, hook: InlineBotQueryHook) {
        self.inline_bot.set_hook(hook);
    }

    /// Configures Telegram Business quick-reply shortcuts for `/` autocomplete in private chats.
    pub fn set_business_quick_replies(
        &mut self,
        enabled: bool,
        shortcuts: Vec<crate::business::model::QuickReplyShortcut>,
    ) {
        self.business_quick_replies_enabled = enabled;
        self.business_quick_replies = shortcuts;
    }

    /// Returns `true` when the Business `/` quick-reply autocomplete popup or a bot compose
    /// popup (commands, inline results, attach bots) is visible above the compose bar.
    #[must_use]
    pub fn is_quick_reply_popup_visible(&self) -> bool {
        self.quick_reply_autocomplete.is_visible() || self.bot_popup_active
    }

    /// Enters Reply mode for the given message.
    pub fn start_reply(&mut self, msg_id: MsgId, author: String, preview: String) {
        self.mode = ComposeMode::Reply {
            msg_id,
            author,
            preview,
            quote: None,
        };
        self.focus_requested = true;
    }

    /// Enters Reply-with-Quote mode for a drag-selected passage of `msg_id`.
    pub fn start_reply_with_quote(
        &mut self,
        msg_id: MsgId,
        author: String,
        preview: String,
        quote: ReplyQuote,
    ) {
        self.mode = ComposeMode::Reply {
            msg_id,
            author,
            preview,
            quote: Some(quote),
        };
        self.focus_requested = true;
    }

    /// Enters Edit mode for an existing outgoing message (plain text compatibility helper).
    pub fn start_edit(&mut self, msg_id: MsgId, current_text: String, preview: String) {
        self.start_edit_rich(msg_id, TextWithEntities::simple(current_text), preview);
    }

    /// Enters Edit mode for an existing outgoing message preserving all `tg-text` entities.
    pub fn start_edit_rich(
        &mut self,
        msg_id: MsgId,
        current_text: TextWithEntities,
        preview: String,
    ) {
        self.text = current_text.text.clone();
        self.rich_edit.set_text_with_entities(current_text);
        self.mode = ComposeMode::Edit {
            msg_id,
            original_preview: preview,
        };
        self.focus_requested = true;
    }

    /// Enters Forward mode for one or more messages with default forward options.
    pub fn start_forward(&mut self, from_peer: PeerId, msg_ids: Vec<MsgId>, summary: String) {
        self.mode = ComposeMode::Forward {
            from_peer,
            msg_ids,
            summary,
            options: ForwardOptions::default(),
        };
        self.focus_requested = true;
    }

    /// Cancels reply / edit / forward mode.
    pub fn cancel_mode(&mut self) {
        if matches!(self.mode, ComposeMode::Edit { .. }) {
            self.text.clear();
            self.rich_edit.clear();
        }
        self.mode = ComposeMode::Normal;
    }

    /// Inserts an emoji or snippet from the `chat_helpers` emoji panel hook.
    pub fn insert_text(&mut self, snippet: &str, now_ms: u64) {
        self.sync_external_text_mutation();
        let end = self.rich_edit.text.chars().count();
        self.rich_edit
            .paste_rich(&TextWithEntities::simple(snippet), Some((end, end)));
        self.text = self.rich_edit.text.clone();
        self.draft_scheduler.mark_edited(now_ms);
        self.focus_requested = true;
    }

    /// Inserts a Telegram custom emoji (`EntityType::CustomEmoji`) at the current cursor/selection.
    pub fn insert_custom_emoji(&mut self, fallback_emoji: &str, document_id: u64, now_ms: u64) {
        self.sync_external_text_mutation();
        self.rich_edit
            .insert_custom_emoji(fallback_emoji, document_id, None);
        self.text = self.rich_edit.text.clone();
        self.draft_scheduler.mark_edited(now_ms);
        self.focus_requested = true;
    }

    /// Loads a stored legacy draft into the compose controls.
    pub fn apply_draft(&mut self, draft: Option<DraftMessage>) {
        self.apply_thread_draft(draft.map(|d| ThreadDraft {
            text: d.text,
            reply_to: d.reply_to,
            reply_quote: None,
            no_webpage: false,
        }));
    }

    /// Loads a stored per-thread draft into the compose controls.
    pub fn apply_thread_draft(&mut self, draft: Option<ThreadDraft>) {
        self.apply_rich_draft(draft.map(RichThreadDraft::from_legacy));
    }

    /// Loads a stored per-thread rich draft preserving all `tg-text` entities.
    pub fn apply_rich_draft(&mut self, draft: Option<RichThreadDraft>) {
        self.draft_scheduler.reset();
        self.typing_throttle.reset();
        self.voice_recorder.cancel();
        self.round_video_recorder.cancel();
        self.recording_voice = false;
        self.link_preview.clear();
        self.inline_bot.clear();
        self.mention_autocomplete.clear();
        self.quick_reply_autocomplete.clear();
        self.format_menu.close();
        self.link_dialog = None;
        self.send_menu_open = false;
        self.attach_menu_open = false;
        match draft {
            Some(d) => {
                self.text = d.text.text.clone();
                self.rich_edit.set_text_with_entities(d.text);
                if d.no_webpage {
                    self.link_preview.disabled_url = Some(String::new());
                }
                self.mode = match d.reply_to {
                    Some(msg_id) => ComposeMode::Reply {
                        msg_id,
                        author: "Reply".to_owned(),
                        preview: format!("Message #{0}", msg_id.0),
                        quote: d.reply_quote,
                    },
                    None => ComposeMode::Normal,
                };
            }
            None => {
                self.text.clear();
                self.rich_edit.clear();
                self.mode = ComposeMode::Normal;
            }
        }
    }

    /// Builds a legacy [`DraftMessage`] snapshot of the current compose state.
    #[must_use]
    pub fn current_draft(&self) -> Option<DraftMessage> {
        self.current_thread_draft().map(|d| DraftMessage {
            text: d.text,
            reply_to: d.reply_to,
        })
    }

    /// Builds a per-thread [`ThreadDraft`] snapshot of the current compose state.
    #[must_use]
    pub fn current_thread_draft(&self) -> Option<ThreadDraft> {
        self.current_rich_draft().map(|d| d.to_legacy())
    }

    /// Builds a per-thread [`RichThreadDraft`] snapshot preserving all `tg-text` entities.
    #[must_use]
    pub fn current_rich_draft(&self) -> Option<RichThreadDraft> {
        if matches!(
            self.mode,
            ComposeMode::Edit { .. } | ComposeMode::Forward { .. }
        ) {
            return None;
        }
        let (reply_to, reply_quote) = match &self.mode {
            ComposeMode::Reply { msg_id, quote, .. } => (Some(*msg_id), quote.clone()),
            _ => (None, None),
        };
        let active_text = if self.text != self.rich_edit.text {
            TextWithEntities::simple(self.text.clone())
        } else {
            self.rich_edit.to_text_with_entities()
        };
        if active_text.text.trim().is_empty() && reply_to.is_none() {
            None
        } else {
            Some(RichThreadDraft {
                text: active_text,
                reply_to,
                reply_quote,
                no_webpage: self.link_preview.is_disabled(),
            })
        }
    }

    /// Builds a [`SendMessageExtended`] payload from the current compose controls (without clearing).
    #[must_use]
    pub fn build_pending_extended(&self, silent_override: Option<bool>) -> SendMessageExtended {
        let parsed = if self.text != self.rich_edit.text {
            parse_compose_markdown(self.text.trim())
        } else {
            self.rich_edit.to_send_text_with_entities()
        };
        let (reply_to, reply_quote) = match &self.mode {
            ComposeMode::Reply { msg_id, quote, .. } => (Some(*msg_id), quote.clone()),
            _ => (None, None),
        };
        SendMessageExtended {
            text: parsed,
            reply_to,
            reply_quote,
            silent: silent_override.unwrap_or(self.silent_mode),
            schedule_date: None,
            send_as: self.send_as.selected,
            effect_id: self.effects.selected_effect_id,
            no_webpage: self.link_preview.is_disabled(),
            link_preview: self.link_preview.preview.clone(),
            via_bot: None,
            media: None,
            thread_key: None,
            ttl_seconds: None,
            round_video_path: None,
            video_cover_jpeg: None,
        }
    }

    /// Clears compose text & one-shot compose state after submitting or scheduling a message.
    pub fn clear_after_send(&mut self) {
        self.text.clear();
        self.rich_edit.clear();
        self.mode = ComposeMode::Normal;
        self.effects.selected_effect_id = None;
        self.link_preview.clear();
        self.inline_bot.clear();
        self.mention_autocomplete.clear();
        self.quick_reply_autocomplete.clear();
        self.format_menu.close();
        self.link_dialog = None;
        self.draft_scheduler.reset();
        self.typing_throttle.reset();
        self.focus_requested = true;
    }

    /// Ensures external callers mutating `self.text` directly stay synced with `self.rich_edit`.
    fn sync_external_text_mutation(&mut self) {
        if self.text != self.rich_edit.text {
            let old = self.rich_edit.text.clone();
            self.rich_edit.text = self.text.clone();
            self.rich_edit.apply_text_diff(&old);
        }
    }

    /// Computes total vertical height needed for the compose area at `width`.
    #[must_use]
    pub fn desired_height(&self, width: f32, can_send: bool) -> f32 {
        if !can_send {
            return COMPOSE_MIN_HEIGHT;
        }
        let preview_h = if matches!(self.mode, ComposeMode::Normal) {
            0.0
        } else {
            PREVIEW_BAR_HEIGHT
        };
        let link_h = if self.link_preview.is_visible() {
            LINK_PREVIEW_BAR_HEIGHT
        } else {
            0.0
        };
        let fmt_h = if self.show_format_strip { 28.0 } else { 0.0 };
        let input_w = (width - 190.0).max(80.0);
        let lines = tg_ui::widgets::input_field::estimate_wrapped_lines(&self.text, input_w, 13.5);
        let field_h = (lines as f32 * 19.0 + 24.0).clamp(COMPOSE_MIN_HEIGHT, COMPOSE_MAX_HEIGHT);
        preview_h + link_h + fmt_h + field_h
    }

    /// Legacy compose rendering entry point (backwards-compatible with callers without backend refs).
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        peer_id: PeerId,
        can_send: bool,
        muted: bool,
        placeholder: &str,
        colors: &HistoryColors,
    ) -> Vec<ComposeEvent> {
        self.show_internal(
            ui,
            rect,
            peer_id,
            can_send,
            muted,
            placeholder,
            colors,
            None,
            0,
            &[],
            false,
            None,
        )
    }

    /// Full compose rendering entry point wired to `HistoryBackend` for link previews,
    /// inline bot queries, send-as peer selection, slow mode timer, spellchecker, and voice capture.
    #[allow(clippy::too_many_arguments)]
    pub fn show_extended(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        peer_id: PeerId,
        can_send: bool,
        muted: bool,
        placeholder: &str,
        colors: &HistoryColors,
        backend: &dyn HistoryBackend,
        slow_mode_remaining_secs: u32,
        send_as_peers: &[SendAsPeer],
        can_send_when_online: bool,
    ) -> Vec<ComposeEvent> {
        self.show_internal(
            ui,
            rect,
            peer_id,
            can_send,
            muted,
            placeholder,
            colors,
            Some(backend),
            slow_mode_remaining_secs,
            send_as_peers,
            can_send_when_online,
            None,
        )
    }

    /// Full compose rendering entry point with bot menu button, reply keyboard toggle, and
    /// attach-menu bots chrome wired from [`crate::bots`].
    #[allow(clippy::too_many_arguments)]
    pub fn show_extended_with_bots(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        peer_id: PeerId,
        can_send: bool,
        muted: bool,
        placeholder: &str,
        colors: &HistoryColors,
        backend: &dyn HistoryBackend,
        slow_mode_remaining_secs: u32,
        send_as_peers: &[SendAsPeer],
        can_send_when_online: bool,
        bot_chrome: Option<ComposeBotChrome<'_>>,
    ) -> Vec<ComposeEvent> {
        self.show_internal(
            ui,
            rect,
            peer_id,
            can_send,
            muted,
            placeholder,
            colors,
            Some(backend),
            slow_mode_remaining_secs,
            send_as_peers,
            can_send_when_online,
            bot_chrome,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn show_internal(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        peer_id: PeerId,
        can_send: bool,
        muted: bool,
        placeholder: &str,
        colors: &HistoryColors,
        backend: Option<&dyn HistoryBackend>,
        slow_mode_remaining_secs: u32,
        send_as_peers: &[SendAsPeer],
        can_send_when_online: bool,
        bot_chrome: Option<ComposeBotChrome<'_>>,
    ) -> Vec<ComposeEvent> {
        let mut events = Vec::new();
        let now_secs = ui.ctx().input(|i| i.time);
        let now_ms = (now_secs * 1000.0).max(0.0) as u64;

        // Broadcast channels use "Broadcast a message..." / "Silent broadcast..."
        // (`HistoryWidget::updateFieldPlaceholder`).
        let broadcast_placeholder = (placeholder == tg_lang::tr::lng_message_ph()
            && backend
                .and_then(|be| be.peer_info(peer_id))
                .is_some_and(|info| matches!(info.summary.kind, PeerKind::Channel)))
        .then(|| {
            if self.silent_mode {
                tg_lang::tr::lng_broadcast_silent_ph()
            } else {
                tg_lang::tr::lng_broadcast_ph()
            }
        });
        let placeholder = broadcast_placeholder.as_deref().unwrap_or(placeholder);

        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.bar_bg);
        ui.painter().line_segment(
            [
                Pos2::new(rect.left(), rect.top()),
                Pos2::new(rect.right(), rect.top()),
            ],
            Stroke::new(1.0, colors.divider),
        );

        // Read-only broadcast channel bottom bar ("MUTE" / "UNMUTE").
        if !can_send {
            let label = if muted {
                "UNMUTE CHANNEL"
            } else {
                "MUTE CHANNEL"
            };
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(13.0),
                colors.active_text_fg,
            );
            return events;
        }

        // Sync external direct writes to `self.text` into `self.rich_edit`.
        self.sync_external_text_mutation();

        let use_rich_inline = bot_chrome
            .as_ref()
            .is_some_and(|c| c.use_rich_inline_results);
        let has_bot_commands = bot_chrome.as_ref().is_some_and(|c| c.has_bot_commands);

        // Sync link preview (debounced), inline bot results, & `@mention` autocomplete with current text.
        let self_user_id = backend.map_or(1000, |be| be.self_peer().0.bare());
        if let Some(be) = backend {
            if self
                .link_preview
                .sync_from_text_debounced(&self.text, now_ms, be)
            {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(120));
            }
            if !use_rich_inline {
                self.inline_bot.sync_from_text(&self.text, be);
            }
            let mention_cands = be.chat_mention_candidates(peer_id);
            self.mention_autocomplete
                .update(&self.rich_edit, &mention_cands);
        }

        // Floating `@mention` autocomplete popup above compose bar (`MentionName` insertion).
        if let Some(chosen) = self.mention_autocomplete.handle_keys(ui.ctx()) {
            let range = self.mention_autocomplete.replace_char_range;
            insert_mention_candidate(&mut self.rich_edit, &chosen, self_user_id, range);
            self.text = self.rich_edit.text.clone();
            self.draft_scheduler.mark_edited(now_ms);
            self.focus_requested = true;
        }
        if let Some(chosen) = self.mention_autocomplete.show(ui, rect, colors) {
            let range = self.mention_autocomplete.replace_char_range;
            insert_mention_candidate(&mut self.rich_edit, &chosen, self_user_id, range);
            self.text = self.rich_edit.text.clone();
            self.draft_scheduler.mark_edited(now_ms);
            self.focus_requested = true;
        }

        // Floating Business `/` quick-reply autocomplete in private chats (`can_send_when_online`
        // is true strictly for non-Saved-Messages private `PeerKind::User` chats; suppressed in bot
        // chats where `/` opens the bot commands popup).
        self.quick_reply_autocomplete.update(
            &self.text,
            can_send_when_online && !has_bot_commands,
            self.business_quick_replies_enabled,
            &self.business_quick_replies,
        );
        if let Some(chosen_qr) = self
            .quick_reply_autocomplete
            .handle_keys(ui.ctx(), &self.text)
            .or_else(|| self.quick_reply_autocomplete.show(ui, rect, colors))
        {
            self.clear_after_send();
            events.push(ComposeEvent::SendQuickReply(chosen_qr));
        }

        // Floating inline bot results panel above the compose bar (legacy fallback when BotsBackend absent).
        if !use_rich_inline && let Some(res) = self.inline_bot.show(ui, rect, colors) {
            self.text.clear();
            self.rich_edit.clear();
            self.inline_bot.clear();
            events.push(ComposeEvent::SendInlineResult(res));
        }

        // Floating round video recording placeholder overlay.
        if let Some(send_rv) = self
            .round_video_recorder
            .show_overlay(ui, rect, now_secs, colors)
        {
            if send_rv {
                if let Some(rv) = self.round_video_recorder.finish(now_secs) {
                    events.push(ComposeEvent::RoundVideoCaptured(rv));
                }
            } else {
                self.round_video_recorder.cancel();
                events.push(ComposeEvent::SendTyping(TypingAction::Cancel));
            }
        }

        let mut field_top = rect.top();

        // 1. Optional editable Link Preview bar above compose field.
        if self.link_preview.is_visible() {
            let lp_rect = Rect::from_min_size(
                Pos2::new(rect.left(), field_top),
                Vec2::new(rect.width(), LINK_PREVIEW_BAR_HEIGHT),
            );
            self.link_preview.show(ui, lp_rect, colors);
            field_top = lp_rect.bottom();
        }

        // 2. Optional Reply / Reply-with-Quote / Edit / Forward (with options) bar.
        if !matches!(self.mode, ComposeMode::Normal) {
            let preview_rect = Rect::from_min_size(
                Pos2::new(rect.left(), field_top),
                Vec2::new(rect.width(), PREVIEW_BAR_HEIGHT),
            );
            self.paint_preview_bar(ui, preview_rect, colors);
            field_top = preview_rect.bottom();
        }

        // 2b. Optional inline formatting & custom-emoji strip.
        if self.show_format_strip {
            let fmt_rect = Rect::from_min_size(
                Pos2::new(rect.left(), field_top),
                Vec2::new(rect.width(), 28.0),
            );
            if let Some(act) = paint_active_format_strip(ui, fmt_rect, &self.rich_edit, colors) {
                match act {
                    FormatStripAction::Format(f) => {
                        self.handle_format_action(f, None, now_ms);
                    }
                    FormatStripAction::InsertCustomEmoji(preset) => {
                        self.insert_custom_emoji(preset.emoji, preset.document_id, now_ms);
                    }
                }
            }
            field_top = fmt_rect.bottom();
        }

        let input_bar = Rect::from_min_max(Pos2::new(rect.left(), field_top), rect.max);
        let mut left_edge = input_bar.left() + 8.0;

        // 3. Optional Send-As peer chooser button when peer allows posting as channel/alias.
        if send_as_peers.len() > 1 {
            let sa_rect = Rect::from_center_size(
                Pos2::new(left_edge + 16.0, input_bar.bottom() - 24.0),
                Vec2::splat(30.0),
            );
            let sa_resp = ui.interact(sa_rect, Id::unique("tg_compose_send_as"), Sense::click());
            if sa_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            let active_sa = self
                .send_as
                .selected
                .and_then(|id| send_as_peers.iter().find(|p| p.peer_id == id))
                .unwrap_or(&send_as_peers[0]);
            sa_resp.widget_info(|| {
                WidgetInfo::labeled(Role::Button, true, format!("Send as {}", active_sa.title))
            });
            let [r, g, b] = active_sa.avatar_rgb;
            ui.painter()
                .circle_filled(sa_rect.center(), 13.0, Color32::from_rgb(r, g, b));
            let ch = active_sa.title.chars().next().unwrap_or('A').to_string();
            ui.painter().text(
                sa_rect.center(),
                Align2::CENTER_CENTER,
                ch,
                FontId::proportional(11.5),
                Color32::WHITE,
            );
            if sa_resp
                .on_hover_text(format!("Sending as {} (click to switch)", active_sa.title))
                .clicked()
            {
                self.send_as.open = !self.send_as.open;
            }
            if let Some(chosen_sa) =
                self.send_as
                    .show_popup(ui, sa_rect.left_top(), send_as_peers, colors)
            {
                events.push(ComposeEvent::SaveDefaultSendAs(chosen_sa));
            }
            left_edge = sa_rect.right() + 4.0;
        }

        // 3b. Optional Bot Menu Button (Commands or Web App) at the left edge of bot chats.
        if let Some(ref chrome) = bot_chrome
            && let Some(menu_btn) = chrome.menu_button
            && (menu_btn.is_web_app() || chrome.has_bot_commands)
        {
            let menu_max = Rect::from_min_size(
                Pos2::new(left_edge, input_bar.bottom() - 40.0),
                Vec2::new(146.0, 32.0),
            );
            let out = ui.scope_builder(egui::UiBuilder::new().max_rect(menu_max), |ui| {
                paint_bot_menu_button(
                    ui,
                    menu_btn,
                    chrome.has_bot_commands,
                    chrome.commands_popup_open,
                    false,
                    chrome.palette,
                )
            });
            if let Some(act) = out.inner {
                events.push(ComposeEvent::BotMenuButton(act));
            }
            left_edge += out.response.rect.width().max(68.0) + 6.0;
        }

        // 4. Left Attach button (opens Attach Menu: Files / Poll / Location / Contact + Attach Menu Bots)
        let use_bot_attach = bot_chrome.as_ref().is_some_and(|c| c.use_bot_attach_menu);
        let attach_highlighted =
            self.attach_menu_open || bot_chrome.as_ref().is_some_and(|c| c.bot_attach_open);
        let attach_rect = Rect::from_center_size(
            Pos2::new(left_edge + 16.0, input_bar.bottom() - 24.0),
            Vec2::splat(34.0),
        );
        let attach_resp = ui.interact(attach_rect, Id::unique("tg_compose_attach"), Sense::click());
        attach_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Attach"));
        if attach_resp.hovered() || attach_highlighted {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .circle_filled(attach_rect.center(), 17.0, colors.bar_bg_over);
        }
        // `st::historyAttach` (chat/input_attach) at its own size.
        let attach_fg = if attach_highlighted {
            colors.accent
        } else {
            colors.sub_text_fg
        };
        if !tg_app_core::ui_icons::paint_style_tinted(
            ui.painter(),
            Rect::from_center_size(attach_rect.center(), Vec2::splat(40.0)),
            tg_ui::tg_style::st::history_attach.icon,
            attach_fg,
        ) {
            paint_icon(
                ui.painter(),
                attach_rect.shrink(8.0),
                IconKind::Attach,
                attach_fg,
            );
        }
        if attach_resp
            .on_hover_text("Attach media, poll, location, contact, or Mini App bot")
            .clicked()
        {
            if use_bot_attach {
                self.attach_menu_open = false;
                events.push(ComposeEvent::ToggleBotAttachMenu);
            } else {
                self.attach_menu_open = !self.attach_menu_open;
            }
        }
        if !use_bot_attach && self.attach_menu_open {
            self.paint_attach_menu(ui, attach_rect.left_top(), colors, &mut events);
        }

        // 5. Right buttons (`HistoryWidget::updateControlsGeometry`): Send/Mic, Emoji and, for
        // broadcast channels the user can post in, the silent toggle (`hasSilentToggle`).
        // Message effects live in the Send menu ("Message Effect...").
        let is_broadcast = backend
            .and_then(|be| be.peer_info(peer_id))
            .is_some_and(|info| matches!(info.summary.kind, PeerKind::Channel));
        let show_silent_toggle = is_broadcast && can_send;
        let send_rect = Rect::from_center_size(
            Pos2::new(input_bar.right() - 24.0, input_bar.bottom() - 24.0),
            Vec2::new(44.0, 46.0),
        );
        let emoji_rect = Rect::from_center_size(
            Pos2::new(send_rect.left() - 22.0, input_bar.bottom() - 24.0),
            Vec2::new(44.0, 46.0),
        );
        let effects_rect = emoji_rect;
        let silent_rect = Rect::from_center_size(
            Pos2::new(emoji_rect.left() - 22.0, input_bar.bottom() - 24.0),
            Vec2::new(44.0, 46.0),
        );

        let mut right_edge = if show_silent_toggle {
            silent_rect.left()
        } else {
            emoji_rect.left()
        } - 4.0;
        if let Some(ref chrome) = bot_chrome
            && chrome.has_reply_keyboard
        {
            let kb_btn_rect = Rect::from_center_size(
                Pos2::new(right_edge - 17.0, input_bar.bottom() - 24.0),
                Vec2::splat(34.0),
            );
            let clicked = ui
                .scope_builder(egui::UiBuilder::new().max_rect(kb_btn_rect), |ui| {
                    paint_reply_keyboard_toggle_button(
                        ui,
                        chrome.palette,
                        chrome.reply_keyboard_visible,
                        true,
                    )
                    .clicked()
                })
                .inner;
            if clicked {
                events.push(ComposeEvent::ToggleReplyKeyboard);
            }
            right_edge = kb_btn_rect.left() - 4.0;
        }
        if slow_mode_remaining_secs > 0 {
            let sm_rect = Rect::from_center_size(
                Pos2::new(right_edge - 24.0, input_bar.bottom() - 24.0),
                Vec2::new(46.0, 22.0),
            );
            paint_slow_mode_badge(ui.painter(), sm_rect, slow_mode_remaining_secs, colors);
            let _ = ui
                .interact(sm_rect, Id::unique("tg_slow_mode_badge"), Sense::hover())
                .on_hover_text(format!(
                    "Slow mode enabled: wait {} before sending",
                    crate::history::sections_model::SlowModeState::format_countdown(
                        slow_mode_remaining_secs
                    )
                ));
            // Tick countdown smoothly (matching tdesktop `kRefreshSlowmodeLabelTimeout = 200ms`).
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(200));
            right_edge = sm_rect.left() - 4.0;
        }

        // Silent broadcast toggle (`HistoryWidget::_silent`, broadcast channels only).
        if show_silent_toggle {
            let silent_resp =
                ui.interact(silent_rect, Id::unique("tg_compose_silent"), Sense::click());
            silent_resp.widget_info(|| {
                WidgetInfo::selected(Role::Button, true, self.silent_mode, "Silent send")
            });
            if silent_resp.hovered() || self.silent_mode {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter()
                    .circle_filled(silent_rect.center(), 14.0, colors.bar_bg_over);
            }
            let silent_color = if self.silent_mode {
                colors.accent
            } else {
                colors.sub_text_fg
            };
            let silent_icon = if self.silent_mode {
                tg_ui::tg_style::st::menu_icon_mute
            } else {
                tg_ui::tg_style::st::menu_icon_notifications
            };
            tg_app_core::ui_icons::paint_style_tinted(
                ui.painter(),
                Rect::from_center_size(silent_rect.center(), Vec2::splat(16.0)),
                silent_icon,
                silent_color,
            );
            if silent_resp
                .on_hover_text(if self.silent_mode {
                    "Silent send ON (no sound)"
                } else {
                    "Toggle silent send"
                })
                .clicked()
            {
                self.silent_mode = !self.silent_mode;
            }
        }

        // Message effects picker (opened from the Send menu's "Message Effect...").
        if self.effects.open
            && !self.effects.is_catalog_loaded()
            && let Some(be) = backend
        {
            self.effects.sync_catalog(be.available_effects());
        }
        self.effects
            .show_popup(ui, effects_rect.right_top(), colors);

        // Emoji panel button
        let emoji_resp = ui.interact(emoji_rect, Id::unique("tg_compose_emoji"), Sense::click());
        emoji_resp
            .widget_info(|| WidgetInfo::labeled(Role::Button, true, "Emoji, stickers and GIFs"));
        if emoji_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .circle_filled(emoji_rect.center(), 16.0, colors.bar_bg_over);
        }
        paint_emoji_button_icon(ui.painter(), emoji_rect.center(), colors.sub_text_fg);
        if emoji_resp
            .on_hover_text("Emoji, stickers and GIFs")
            .clicked()
        {
            events.push(ComposeEvent::ToggleEmojiPanel);
        }

        let has_sendable_content =
            !self.text.trim().is_empty() || matches!(self.mode, ComposeMode::Forward { .. });

        // 6. Multi-line compose field OR live `tg-audio` voice recording waveform bar
        let field_rect = Rect::from_min_max(
            Pos2::new(attach_rect.right() + 6.0, input_bar.top() + 8.0),
            Pos2::new(
                right_edge.max(attach_rect.right() + 60.0),
                input_bar.bottom() - 8.0,
            ),
        );

        let mut submit_now = false;
        let mut submit_silent_override = None;

        self.voice_recorder.tick(now_secs);
        self.recording_voice = self.voice_recorder.is_active();

        if self.voice_recorder.is_active() {
            if let Some(va) =
                paint_voice_recording_bar(ui, field_rect, &self.voice_recorder, now_secs, colors)
            {
                match va {
                    VoiceBarAction::Cancel => {
                        self.voice_recorder.cancel();
                        self.recording_voice = false;
                        events.push(ComposeEvent::SendTyping(TypingAction::Cancel));
                    }
                    VoiceBarAction::TogglePause => {
                        self.voice_recorder.toggle_pause(now_secs);
                    }
                    VoiceBarAction::Lock => {
                        self.voice_recorder.lock(now_secs);
                    }
                    VoiceBarAction::Send => {
                        if let Some(voice) = self.voice_recorder.finish(now_secs) {
                            self.recording_voice = false;
                            events.push(ComposeEvent::VoiceCaptured(voice));
                        }
                    }
                }
            }
        } else {
            let input_id = Id::unique(("tg_compose_input", peer_id.0));
            let was_focused = ui.ctx().memory(|m| m.has_focus(input_id));

            // History navigation shortcuts: Up arrow (empty field -> edit last outgoing),
            // Ctrl+Up / Ctrl+Down -> reply to previous / next message.
            if (was_focused || self.text.is_empty())
                && self.link_dialog.is_none()
                && let Some(nav) = detect_compose_history_shortcut(
                    ui.ctx(),
                    self.text.is_empty(),
                    matches!(self.mode, ComposeMode::Normal),
                    self.mention_autocomplete.is_visible()
                        || self.quick_reply_autocomplete.is_visible(),
                )
            {
                match nav {
                    ComposeHistoryShortcut::EditLastOutgoing => {
                        events.push(ComposeEvent::EditLastOutgoing);
                    }
                    ComposeHistoryShortcut::ReplyPrevious => {
                        events.push(ComposeEvent::ReplyNavigate(-1));
                    }
                    ComposeHistoryShortcut::ReplyNext => {
                        events.push(ComposeEvent::ReplyNavigate(1));
                    }
                }
            }

            // Formatting shortcuts (`Ctrl+B/I/U/K`, `Ctrl+Shift+X/M/P/N/.`) when focused.
            if was_focused
                && self.link_dialog.is_none()
                && let Some(fmt_act) = detect_format_shortcut(ui.ctx())
            {
                self.handle_format_action(fmt_act, None, now_ms);
            }

            // Intercept rich paste (`Event::Paste`) so entities from `copied_rich_buffer` or
            // Telegram Markdown stay styled live inside `rich_edit`.
            if was_focused && self.link_dialog.is_none() {
                let mut intercepted_paste: Option<TextWithEntities> = None;
                ui.ctx().input_mut(|i| {
                    let mut idx = 0;
                    while idx < i.events.len() {
                        if let Event::Paste(ref pasted_str) = i.events[idx] {
                            if let Some(ref copied) = self.copied_rich_buffer
                                && (copied.to_markdown() == *pasted_str
                                    || copied.content.text == *pasted_str)
                                && !copied.content.entities.is_empty()
                            {
                                intercepted_paste = Some(copied.content.clone());
                                i.events.remove(idx);
                                break;
                            }
                            let parsed_md = parse_compose_markdown(pasted_str);
                            if !parsed_md.entities.is_empty() && parsed_md.text != *pasted_str {
                                intercepted_paste = Some(parsed_md);
                                i.events.remove(idx);
                                break;
                            }
                        }
                        idx += 1;
                    }
                });
                if let Some(rich_paste) = intercepted_paste {
                    self.rich_edit.paste_rich(&rich_paste, None);
                    self.text = self.rich_edit.text.clone();
                    self.draft_scheduler.mark_edited(now_ms);
                }
            }

            let enter_pressed = ui.ctx().input(|i| i.key_pressed(Key::Enter));
            let modifiers = ui.ctx().input(|i| i.modifiers);
            let ctrl_or_cmd = modifiers.ctrl || modifiers.command;
            let shortcut_triggered =
                self.send_shortcut
                    .should_send(enter_pressed, modifiers.shift, ctrl_or_cmd);

            let slowmode_blocks =
                slow_mode_remaining_secs > 0 && !matches!(self.mode, ComposeMode::Edit { .. });
            if was_focused
                && self.link_dialog.is_none()
                && !self.mention_autocomplete.is_visible()
                && !self.quick_reply_autocomplete.is_visible()
                && shortcut_triggered
                && has_sendable_content
            {
                ui.ctx().input_mut(|i| {
                    i.consume_key(modifiers, Key::Enter);
                });
                if slowmode_blocks {
                    events.push(ComposeEvent::SlowModeBlocked(slow_mode_remaining_secs));
                } else {
                    submit_now = true;
                }
            }

            let prev_text = self.rich_edit.text.clone();
            let rich_pal =
                RichInputPalette::from_colors(colors.text_fg, colors.accent, colors.is_night);
            let mut input_secondary_pos = None;
            ui.scope_builder(egui::UiBuilder::new().max_rect(field_rect), |ui| {
                ScrollArea::vertical()
                    .id_salt(("tg_compose_scroll", peer_id.0))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let out = show_rich_text_edit(
                            ui,
                            input_id,
                            &mut self.rich_edit,
                            placeholder,
                            13.0,
                            field_rect.width(),
                            &rich_pal,
                            self.focus_requested,
                        );
                        if self.focus_requested {
                            self.focus_requested = false;
                        }
                        if out.response.secondary_clicked() {
                            input_secondary_pos = ui.ctx().pointer_interact_pos();
                        }
                    });
            });
            self.text = self.rich_edit.text.clone();

            // Right-click opens Formatting Context Menu (and spellcheck suggestions if on error).
            if let Some(pos) = input_secondary_pos {
                self.spellchecker.check(&self.text);
                let sel = self.rich_edit.normalized_selection();
                if !self.spellchecker.errors().is_empty() && sel.0 == sel.1 {
                    self.spellchecker.open_menu_for_text(&self.text, pos, None);
                } else {
                    self.format_menu.open_at(pos, sel);
                }
            }
            if let Some(f_ev) = self.format_menu.show(
                ui,
                self.copied_rich_buffer.is_some(),
                colors.bar_bg,
                colors.divider,
                colors.text_fg,
                colors.sub_text_fg,
            ) {
                let sel = self.format_menu.captured_selection;
                match f_ev {
                    FormattingMenuEvent::Format(act) => {
                        self.handle_format_action(act, Some(sel), now_ms);
                    }
                    FormattingMenuEvent::PasteRich => {
                        if let Some(ref rich) = self.copied_rich_buffer {
                            self.rich_edit.paste_rich(&rich.content, Some(sel));
                            self.text = self.rich_edit.text.clone();
                            self.draft_scheduler.mark_edited(now_ms);
                        }
                    }
                }
            }
            if let Some(ref mut dlg) = self.link_dialog
                && let Some(dlg_ev) = dlg.show(
                    ui,
                    colors.bar_bg,
                    colors.divider,
                    colors.text_fg,
                    colors.sub_text_fg,
                    colors.accent,
                )
            {
                if let EditLinkDialogAction::Confirm { range, text, url } = dlg_ev
                    && self.rich_edit.set_link(Some(range), &text, &url)
                {
                    self.text = self.rich_edit.text.clone();
                    self.draft_scheduler.mark_edited(now_ms);
                }
                self.link_dialog = None;
                self.focus_requested = true;
            }

            if self
                .spellchecker
                .paint_underlines_and_menu(ui, field_rect, &mut self.text, colors)
            {
                self.sync_external_text_mutation();
                self.draft_scheduler.mark_edited(now_ms);
            }

            if self.text != prev_text {
                self.draft_scheduler.mark_edited(now_ms);
                if !self.text.trim().is_empty() && self.typing_throttle.should_send(peer_id, now_ms)
                {
                    events.push(ComposeEvent::SendTyping(TypingAction::Typing));
                }
            }
        }

        // 7. Right-hand Send / Edit-Check / Voice-Mic / Round-Video Camera button
        let send_resp = ui.interact(
            send_rect,
            Id::unique("tg_compose_send"),
            Sense::click_and_drag(),
        );
        let send_a11y_label = if self.voice_recorder.is_active() {
            "Send voice message"
        } else if has_sendable_content {
            if matches!(self.mode, ComposeMode::Edit { .. }) {
                "Save edit"
            } else {
                "Send"
            }
        } else {
            match self.record_mode {
                RecordButtonMode::Voice => "Record voice message",
                RecordButtonMode::RoundVideo => "Record video message",
            }
        };
        send_resp.widget_info(|| {
            WidgetInfo::labeled(Role::Button, slow_mode_remaining_secs == 0, send_a11y_label)
        });
        if send_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .circle_filled(send_rect.center(), 18.0, colors.bar_bg_over);
        }

        if self.voice_recorder.is_active() {
            paint_icon(
                ui.painter(),
                send_rect.shrink(9.0),
                IconKind::Send,
                colors.accent,
            );
            // Handle hold-and-drag gestures while Holding
            if !self.voice_recorder.is_locked() {
                if send_resp.dragged() {
                    let delta = send_resp.drag_delta();
                    if let Some(cancelled) = self.voice_recorder.update_hold_drag(delta, now_secs)
                        && cancelled
                    {
                        self.recording_voice = false;
                        events.push(ComposeEvent::SendTyping(TypingAction::Cancel));
                    }
                } else if send_resp.drag_stopped()
                    && let Some(voice) = self.voice_recorder.finish(now_secs)
                {
                    self.recording_voice = false;
                    events.push(ComposeEvent::VoiceCaptured(voice));
                }
            } else if send_resp.on_hover_text("Send voice message").clicked()
                && let Some(voice) = self.voice_recorder.finish(now_secs)
            {
                self.recording_voice = false;
                events.push(ComposeEvent::VoiceCaptured(voice));
            }
        } else if has_sendable_content {
            let icon = if matches!(self.mode, ComposeMode::Edit { .. }) {
                IconKind::Check
            } else {
                IconKind::Send
            };
            let slowmode_blocks =
                slow_mode_remaining_secs > 0 && !matches!(self.mode, ComposeMode::Edit { .. });
            let send_col = if slowmode_blocks {
                colors.sub_text_fg
            } else {
                colors.accent
            };
            paint_icon(ui.painter(), send_rect.shrink(9.0), icon, send_col);
            if send_resp.secondary_clicked() {
                self.send_menu_open = !self.send_menu_open;
            } else if send_resp
                .on_hover_text(if slowmode_blocks {
                    "Slow mode active — right-click to Schedule"
                } else {
                    "Send message (right-click for Silent / Schedule)"
                })
                .clicked()
            {
                if slowmode_blocks {
                    events.push(ComposeEvent::SlowModeBlocked(slow_mode_remaining_secs));
                } else {
                    submit_now = true;
                }
            }
        } else {
            match self.record_mode {
                RecordButtonMode::Voice => {
                    paint_mic_icon(ui.painter(), send_rect.center(), colors.sub_text_fg);
                    if send_resp.drag_started() {
                        self.voice_recorder.start_hold(now_secs);
                        self.recording_voice = true;
                        self.recording_started_at = now_secs;
                        events.push(ComposeEvent::SendTyping(TypingAction::RecordingVoice));
                    } else if send_resp.secondary_clicked() {
                        self.record_mode = RecordButtonMode::RoundVideo;
                    } else if send_resp
                        .on_hover_text("Hold to record voice · Click to record locked · Right-click for Round Video")
                        .clicked()
                    {
                        self.voice_recorder.start_locked(now_secs);
                        self.recording_voice = true;
                        self.recording_started_at = now_secs;
                        events.push(ComposeEvent::SendTyping(TypingAction::RecordingVoice));
                    }
                }
                RecordButtonMode::RoundVideo => {
                    paint_round_video_icon(ui.painter(), send_rect.center(), colors.accent);
                    if send_resp.secondary_clicked() {
                        self.record_mode = RecordButtonMode::Voice;
                    } else if send_resp
                        .on_hover_text("Record round video · Right-click for Voice")
                        .clicked()
                    {
                        self.round_video_recorder.start(now_secs, true);
                        events.push(ComposeEvent::SendTyping(TypingAction::UploadingMedia));
                    }
                }
            }
        }

        // Right-click Send Context Menu (`menu_send.cpp`)
        if self.send_menu_open
            && let Some(silent_ov) = self.paint_send_menu(
                ui,
                send_rect.right_top(),
                can_send_when_online,
                colors,
                &mut events,
            )
        {
            submit_silent_override = Some(silent_ov);
            submit_now = true;
        }

        // 8. Dispatch submission if triggered by shortcut or Send click.
        let slowmode_blocks_submit =
            slow_mode_remaining_secs > 0 && !matches!(self.mode, ComposeMode::Edit { .. });
        if submit_now && has_sendable_content && !slowmode_blocks_submit {
            let ext_req = self.build_pending_extended(submit_silent_override);
            let mode = std::mem::replace(&mut self.mode, ComposeMode::Normal);
            self.clear_after_send();

            match mode {
                ComposeMode::Normal | ComposeMode::Reply { .. } => {
                    events.push(ComposeEvent::SendMessageExtended(ext_req));
                }
                ComposeMode::Edit { msg_id, .. } => {
                    events.push(ComposeEvent::SubmitEdit {
                        msg_id,
                        text: ext_req.text,
                    });
                }
                ComposeMode::Forward {
                    from_peer,
                    msg_ids,
                    options,
                    ..
                } => {
                    let comment = (!ext_req.text.text.is_empty()).then_some(ext_req.text);
                    events.push(ComposeEvent::SubmitForwardWithOptions {
                        from_peer,
                        msg_ids,
                        options,
                        comment,
                    });
                }
            }
        }

        // 9. Poll debounced draft save timers (1s / 5s / 14s).
        match self.draft_scheduler.poll(now_ms) {
            DraftSaveDue::Local | DraftSaveDue::Cloud => {
                events.push(ComposeEvent::SaveRichDraft(self.current_rich_draft()));
            }
            DraftSaveDue::None => {}
        }

        events
    }

    /// Executes a formatting action or opens the `Ctrl+K` link dialog on `rich_edit`.
    pub fn handle_format_action(
        &mut self,
        action: FormatAction,
        range_override: Option<(usize, usize)>,
        now_ms: u64,
    ) {
        self.sync_external_text_mutation();
        let sel = range_override.unwrap_or_else(|| self.rich_edit.normalized_selection());
        if action == FormatAction::CreateLink {
            let selected_text: String = self
                .rich_edit
                .text
                .chars()
                .skip(sel.0)
                .take(sel.1.saturating_sub(sel.0))
                .collect();
            let existing = self.rich_edit.existing_link_for_range(sel);
            self.link_dialog = Some(EditLinkDialogState::open(selected_text, existing, sel));
            return;
        }
        if self.rich_edit.toggle_format(action, Some(sel)) {
            self.draft_scheduler.mark_edited(now_ms);
        }
    }

    fn paint_attach_menu(
        &mut self,
        ui: &mut Ui,
        anchor: Pos2,
        colors: &HistoryColors,
        events: &mut Vec<ComposeEvent>,
    ) {
        let pos = Pos2::new(anchor.x, (anchor.y - 204.0).max(8.0));
        let mut close = false;
        egui::Area::new(Id::unique("tg_compose_attach_menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE
                    .fill(colors.bar_bg)
                    .stroke(Stroke::new(1.0, colors.divider))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(egui::Margin::same(6))
                    .show(ui, |ui| {
                        use tg_app_core::ui_icons::style_icon_menu_row;
                        use tg_ui::tg_style::st;
                        ui.set_min_width(196.0);
                        let icon_col = colors.sub_text_fg;
                        let text_col = colors.text_fg;
                        let hover_bg = colors.bar_bg_over;
                        if style_icon_menu_row(
                            ui,
                            st::history_attach.icon,
                            icon_col,
                            "Photo, Video or File...",
                            text_col,
                            hover_bg,
                        ) {
                            events.push(ComposeEvent::AttachClicked);
                            close = true;
                        }
                        if style_icon_menu_row(
                            ui,
                            st::menu_icon_article,
                            icon_col,
                            "Rich Document...",
                            text_col,
                            hover_bg,
                        ) {
                            events.push(ComposeEvent::OpenRichEditor);
                            close = true;
                        }
                        if style_icon_menu_row(
                            ui,
                            st::menu_icon_create_poll,
                            icon_col,
                            "Create Poll / Quiz",
                            text_col,
                            hover_bg,
                        ) {
                            events.push(ComposeEvent::OpenCreatePollBox);
                            close = true;
                        }
                        if style_icon_menu_row(
                            ui,
                            st::menu_icon_create_todo_list,
                            icon_col,
                            "Checklist",
                            text_col,
                            hover_bg,
                        ) {
                            events.push(ComposeEvent::OpenCreateTodoBox);
                            close = true;
                        }
                        if style_icon_menu_row(
                            ui,
                            st::menu_icon_sound_select,
                            icon_col,
                            "Attach Music...",
                            text_col,
                            hover_bg,
                        ) {
                            events.push(ComposeEvent::OpenMusicAttach);
                            close = true;
                        }
                        if style_icon_menu_row(
                            ui,
                            st::media_menu_icon_location,
                            icon_col,
                            "Send Location",
                            text_col,
                            hover_bg,
                        ) {
                            events.push(ComposeEvent::OpenLocationPicker);
                            close = true;
                        }
                        if style_icon_menu_row(
                            ui,
                            st::menu_icon_profile,
                            icon_col,
                            "Share Contact",
                            text_col,
                            hover_bg,
                        ) {
                            events.push(ComposeEvent::OpenShareContact);
                            close = true;
                        }
                        ui.separator();
                        if style_icon_menu_row(
                            ui,
                            st::menu_icon_customize,
                            icon_col,
                            "Compose with AI...",
                            text_col,
                            hover_bg,
                        ) {
                            events.push(ComposeEvent::OpenComposeAi);
                            close = true;
                        }
                        if style_icon_menu_row(
                            ui,
                            st::menu_icon_sticker_create,
                            icon_col,
                            "Sticker Creator...",
                            text_col,
                            hover_bg,
                        ) {
                            events.push(ComposeEvent::OpenStickerCreator);
                            close = true;
                        }
                        if style_icon_menu_row(
                            ui,
                            st::menu_icon_ttl,
                            icon_col,
                            "Self-Destruct TTL...",
                            text_col,
                            hover_bg,
                        ) {
                            events.push(ComposeEvent::OpenSelfDestructBox);
                            close = true;
                        }
                        ui.separator();
                        let (toggle_icon, toggle_lbl) = match self.record_mode {
                            RecordButtonMode::Voice => {
                                (st::menu_icon_video_chat, "Switch to Round Video")
                            }
                            RecordButtonMode::RoundVideo => {
                                (st::menu_icon_sound_on, "Switch to Voice Note")
                            }
                        };
                        if style_icon_menu_row(
                            ui,
                            toggle_icon,
                            icon_col,
                            toggle_lbl,
                            text_col,
                            hover_bg,
                        ) {
                            self.record_mode = match self.record_mode {
                                RecordButtonMode::Voice => RecordButtonMode::RoundVideo,
                                RecordButtonMode::RoundVideo => RecordButtonMode::Voice,
                            };
                            close = true;
                        }
                    });
            });
        if close || ui.ctx().input(|i| i.key_pressed(Key::Escape)) {
            self.attach_menu_open = false;
        }
    }

    fn paint_send_menu(
        &mut self,
        ui: &mut Ui,
        anchor: Pos2,
        can_send_when_online: bool,
        colors: &HistoryColors,
        events: &mut Vec<ComposeEvent>,
    ) -> Option<bool> {
        let mut trigger_silent = None;
        let mut close = false;
        let pos = Pos2::new((anchor.x - 195.0).max(8.0), (anchor.y - 135.0).max(8.0));
        egui::Area::new(Id::unique("tg_compose_send_menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE
                    .fill(colors.bar_bg)
                    .stroke(Stroke::new(1.0, colors.divider))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(egui::Margin::same(6))
                    .show(ui, |ui| {
                        use tg_app_core::ui_icons::style_icon_menu_row;
                        use tg_ui::tg_style::st;
                        ui.set_min_width(196.0);
                        let icon_col = colors.sub_text_fg;
                        let text_col = colors.text_fg;
                        let hover_bg = colors.bar_bg_over;
                        if style_icon_menu_row(
                            ui,
                            st::menu_icon_mute,
                            icon_col,
                            "Send Without Sound",
                            text_col,
                            hover_bg,
                        ) {
                            trigger_silent = Some(true);
                            close = true;
                        }
                        if style_icon_menu_row(
                            ui,
                            st::menu_icon_schedule,
                            icon_col,
                            "Schedule Message...",
                            text_col,
                            hover_bg,
                        ) {
                            events.push(ComposeEvent::OpenScheduleBox);
                            close = true;
                        }
                        if can_send_when_online
                            && style_icon_menu_row(
                                ui,
                                st::menu_icon_when_online,
                                icon_col,
                                "Send When Online",
                                text_col,
                                hover_bg,
                            )
                        {
                            let mut req = self.build_pending_extended(None);
                            req.schedule_date =
                                Some(tg_chat::send_options::SCHEDULED_UNTIL_ONLINE_TIMESTAMP);
                            self.clear_after_send();
                            events.push(ComposeEvent::SendWhenOnline(req));
                            close = true;
                        }
                        if style_icon_menu_row(
                            ui,
                            st::menu_icon_customize,
                            icon_col,
                            "Message Effect...",
                            text_col,
                            hover_bg,
                        ) {
                            self.effects.open = true;
                            close = true;
                        }
                    });
            });
        if close || ui.ctx().input(|i| i.key_pressed(Key::Escape)) {
            self.send_menu_open = false;
        }
        trigger_silent
    }

    fn paint_preview_bar(&mut self, ui: &mut Ui, rect: Rect, colors: &HistoryColors) {
        let (title, subtitle, icon, is_forward) = match &self.mode {
            ComposeMode::Reply {
                author,
                preview,
                quote,
                ..
            } => {
                if let Some(q) = quote {
                    (
                        format!("Reply to quote from {author}"),
                        format!("“{}”", q.text),
                        IconKind::Back,
                        false,
                    )
                } else {
                    (
                        format!("Reply to {author}"),
                        preview.clone(),
                        IconKind::Back,
                        false,
                    )
                }
            }
            ComposeMode::Edit {
                original_preview, ..
            } => (
                "Edit Message".to_owned(),
                original_preview.clone(),
                IconKind::Edit,
                false,
            ),
            ComposeMode::Forward { summary, .. } => (
                "Forward Messages".to_owned(),
                summary.clone(),
                IconKind::ChevronRight,
                true,
            ),
            ComposeMode::Normal => return,
        };

        // Left icon
        let icon_rect = Rect::from_center_size(
            Pos2::new(rect.left() + 24.0, rect.center().y),
            Vec2::splat(18.0),
        );
        paint_icon(ui.painter(), icon_rect, icon, colors.accent);

        // Vertical accent bar
        let bar_rect = Rect::from_min_size(
            Pos2::new(rect.left() + 44.0, rect.top() + 8.0),
            Vec2::new(2.5, rect.height() - 16.0),
        );
        ui.painter()
            .rect_filled(bar_rect, CornerRadius::same(1), colors.accent);

        let close_rect = Rect::from_center_size(
            Pos2::new(rect.right() - 22.0, rect.center().y),
            Vec2::splat(26.0),
        );

        // Interactive Forward options pills: "Hide Sender" & "Hide Captions"
        let mut right_limit = close_rect.left() - 6.0;
        if is_forward
            && let ComposeMode::Forward {
                ref mut options, ..
            } = self.mode
        {
            let cap_rect = Rect::from_center_size(
                Pos2::new(right_limit - 46.0, rect.center().y),
                Vec2::new(88.0, 22.0),
            );
            let cap_resp = ui.interact(cap_rect, Id::unique("tg_fwd_hide_cap"), Sense::click());
            cap_resp.widget_info(|| {
                WidgetInfo::selected(
                    Role::Button,
                    true,
                    options.drop_media_captions,
                    "Hide Captions",
                )
            });
            if cap_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            ui.painter().rect_filled(
                cap_rect,
                CornerRadius::same(6),
                if options.drop_media_captions {
                    colors.accent
                } else {
                    colors.bar_bg_over
                },
            );
            ui.painter().text(
                cap_rect.center(),
                Align2::CENTER_CENTER,
                "Hide Captions",
                FontId::proportional(10.5),
                if options.drop_media_captions {
                    Color32::WHITE
                } else {
                    colors.active_text_fg
                },
            );
            if cap_resp.clicked() {
                options.drop_media_captions = !options.drop_media_captions;
            }
            right_limit = cap_rect.left() - 6.0;

            let snd_rect = Rect::from_center_size(
                Pos2::new(right_limit - 42.0, rect.center().y),
                Vec2::new(80.0, 22.0),
            );
            let snd_resp = ui.interact(snd_rect, Id::unique("tg_fwd_hide_snd"), Sense::click());
            snd_resp.widget_info(|| {
                WidgetInfo::selected(Role::Button, true, options.drop_author, "Hide Sender")
            });
            if snd_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            ui.painter().rect_filled(
                snd_rect,
                CornerRadius::same(6),
                if options.drop_author {
                    colors.accent
                } else {
                    colors.bar_bg_over
                },
            );
            ui.painter().text(
                snd_rect.center(),
                Align2::CENTER_CENTER,
                "Hide Sender",
                FontId::proportional(10.5),
                if options.drop_author {
                    Color32::WHITE
                } else {
                    colors.active_text_fg
                },
            );
            if snd_resp.clicked() {
                options.drop_author = !options.drop_author;
            }
            right_limit = snd_rect.left() - 8.0;
        }

        // Title + snippet
        let text_x = bar_rect.right() + 10.0;
        let max_w = (right_limit - text_x).max(60.0);
        ui.painter().text(
            Pos2::new(text_x, rect.top() + 6.0),
            Align2::LEFT_TOP,
            title,
            FontId::proportional(12.5),
            colors.active_text_fg,
        );
        let snippet_galley =
            ui.painter()
                .layout_no_wrap(subtitle, FontId::proportional(12.5), colors.sub_text_fg);
        ui.painter()
            .with_clip_rect(Rect::from_min_size(
                Pos2::new(text_x, rect.top() + 23.0),
                Vec2::new(max_w, 18.0),
            ))
            .galley(
                Pos2::new(text_x, rect.top() + 23.0),
                snippet_galley,
                colors.sub_text_fg,
            );

        // Cancel button (`×`)
        let close_resp = ui.interact(
            close_rect,
            Id::unique("tg_compose_preview_cancel"),
            Sense::click(),
        );
        close_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Cancel compose mode"));
        if close_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .circle_filled(close_rect.center(), 13.0, colors.bar_bg_over);
        }
        paint_icon(
            ui.painter(),
            close_rect.shrink(6.0),
            IconKind::Close,
            colors.sub_text_fg,
        );
        if close_resp.on_hover_text("Cancel").clicked() {
            self.cancel_mode();
        }

        ui.painter().line_segment(
            [
                Pos2::new(rect.left(), rect.bottom()),
                Pos2::new(rect.right(), rect.bottom()),
            ],
            Stroke::new(1.0, colors.divider),
        );
    }
}

fn paint_emoji_button_icon(painter: &egui::Painter, c: Pos2, color: Color32) {
    let stroke = Stroke::new(1.6, color);
    painter.circle_stroke(c, 9.0, stroke);
    painter.circle_filled(Pos2::new(c.x - 3.2, c.y - 2.0), 1.3, color);
    painter.circle_filled(Pos2::new(c.x + 3.2, c.y - 2.0), 1.3, color);
    painter.add(Shape::line(
        vec![
            Pos2::new(c.x - 4.0, c.y + 2.2),
            Pos2::new(c.x - 1.5, c.y + 4.3),
            Pos2::new(c.x + 1.5, c.y + 4.3),
            Pos2::new(c.x + 4.0, c.y + 2.2),
        ],
        stroke,
    ));
}

fn paint_mic_icon(painter: &egui::Painter, c: Pos2, color: Color32) {
    let stroke = Stroke::new(1.7, color);
    let body = Rect::from_center_size(Pos2::new(c.x, c.y - 2.5), Vec2::new(7.0, 11.0));
    painter.rect_stroke(body, CornerRadius::same(4), stroke, StrokeKind::Middle);
    painter.add(Shape::line(
        vec![
            Pos2::new(c.x - 6.5, c.y - 1.0),
            Pos2::new(c.x - 5.2, c.y + 4.2),
            Pos2::new(c.x, c.y + 6.0),
            Pos2::new(c.x + 5.2, c.y + 4.2),
            Pos2::new(c.x + 6.5, c.y - 1.0),
        ],
        stroke,
    ));
    painter.line_segment(
        [Pos2::new(c.x, c.y + 6.0), Pos2::new(c.x, c.y + 9.5)],
        stroke,
    );
    painter.line_segment(
        [
            Pos2::new(c.x - 3.8, c.y + 9.5),
            Pos2::new(c.x + 3.8, c.y + 9.5),
        ],
        stroke,
    );
}

fn paint_round_video_icon(painter: &egui::Painter, c: Pos2, color: Color32) {
    let stroke = Stroke::new(1.7, color);
    let body = Rect::from_center_size(Pos2::new(c.x - 2.0, c.y), Vec2::new(12.0, 9.0));
    painter.rect_stroke(body, CornerRadius::same(2), stroke, StrokeKind::Middle);
    painter.add(Shape::line(
        vec![
            Pos2::new(c.x + 4.5, c.y - 2.5),
            Pos2::new(c.x + 8.5, c.y - 4.5),
            Pos2::new(c.x + 8.5, c.y + 4.5),
            Pos2::new(c.x + 4.5, c.y + 2.5),
        ],
        stroke,
    ));
}
