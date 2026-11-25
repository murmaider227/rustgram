//! Chat history view: top bar, pinned bar, contact status bar, virtualized message list,
//! scroll-to-bottom FAB, multi-select mode, context menu, and compose controls.
//!
//! Port origin: `Telegram/SourceFiles/history/history_widget.h`,
//! `Telegram/SourceFiles/history/history_widget.cpp`,
//! `Telegram/SourceFiles/history/history_inner_widget.cpp`,
//! `Telegram/SourceFiles/history/view/*`, and
//! `Telegram/SourceFiles/history/view/controls/history_view_compose_controls.cpp`.

pub use tg_app_core::history::backend;
pub mod bars;
pub mod birthday_cake;
pub mod bubble;
pub mod compose;
pub mod compose_boxes;
pub mod compose_extras;
pub mod compose_stash;
pub mod edit_caption;
pub mod emoji_interactions;
pub mod factcheck;
pub mod inline_media;
pub mod inner;
pub mod marking_read;
pub mod media;
pub mod mock;
pub mod mock_ext;
pub mod monoforum;
pub mod no_forwards_request;
pub mod pull_to_next_channel;
pub mod quick_action;
pub mod read_receipts;
pub mod rich_compose;
pub use tg_app_core::history::sections_model;
pub mod sections_ui;
pub mod sponsored;
pub mod streamed_drafts;
pub mod suggestion_cards;
pub mod summary_header;
pub mod text_bridge;
pub use tg_app_core::history::theme;
pub mod todo;
pub mod unread_things;
pub mod video_cover;
pub mod video_editor;
pub mod video_message_seek;
pub use tg_app_info::boxes::welcome_messages;

#[cfg(test)]
mod inline_media_tests;
#[cfg(test)]
mod screenshot;
#[cfg(test)]
mod sections_screenshot;
#[cfg(test)]
mod sections_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_scroll_bottom;
#[cfg(test)]
mod tests_sec3a;
#[cfg(test)]
mod tests_video_editor_wire2;
#[cfg(test)]
mod tests_wp75_media_refresh;
#[cfg(test)]
mod tests_wp79_live_history;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{
    Color32, CornerRadius, FontId, Frame, Margin, Pos2, Rect, RichText, Stroke, TextEdit, Ui, Vec2,
};

use tg_bots::callback::{
    AcceptUrlAuthDecision, BotButtonAction, CallbackAnswerAction, CallbackAnswerCache,
    CallbackCacheKey, CallbackQueryRequest, SwitchInlineDecision, UrlAuthAcceptOutcome,
    UrlAuthDecision, UrlAuthSource, build_accept_url_auth, handle_callback_answer,
    handle_url_auth_accept_result, handle_url_auth_request_error, handle_url_auth_request_result,
};
use tg_bots::commands::PrivacyPolicyAction;
use tg_bots::inline::{SwitchPmButton, parse_inline_bot_query as parse_bot_inline_query};
use tg_bots::mini_app::AttachMenuToggleState;

use crate::bots::backend::BotChatSnapshot;
use crate::bots::boxes::{BotModalBox, BotModalOutcome, show_bot_modal_box};
use crate::bots::inline_keyboard::InlineKeyboardActivation;
use crate::bots::inline_results::{InlineResultsEvent, InlineResultsPanel};
use crate::bots::menus::{
    AttachMenuPopupEvent, BotCommandsPopupEvent, BotCommandsPopupParams, BotInfoHeaderEvent,
    BotMenuButtonAction, show_attach_menu_bots_popup, show_bot_commands_popup,
    show_bot_info_header,
};
use crate::bots::reply_keyboard::{ReplyKeyboardActivation, ReplyKeyboardWidget};
use crate::business::AppBusinessHost;
use crate::business::backend::BusinessBackend;
use crate::business::mock::MockBusinessBackend;
use crate::business::model::{
    BusinessOverviewSnapshot, PremiumLimitKind, PremiumLimitsData, QuickReplyAttachment,
    QuickReplyShortcut,
};
use crate::business::premium::{PremiumLimitsView, PremiumSettingsAction};
use crate::history::backend::{
    AlbumItem, DocumentAttachment, HistoryBackend, HistoryMedia, HistoryMessage, HistoryPeerInfo,
    PhotoAttachment, SendMessageExtended, SendMessageRequest, TranscribeToggleOutcome,
    VideoAttachment, VoiceAttachment, to_bot_full, to_bot_peer, to_bot_peer_kind,
};
use crate::history::bars::{
    BOT_START_BAR_HEIGHT, BUSINESS_BOT_BAR_HEIGHT, BusinessBotBarEvent, CONTACT_STATUS_HEIGHT,
    ContactStatusEvent, JOIN_REQUESTS_BAR_HEIGHT, PINNED_BAR_HEIGHT, PinnedBarEvent, TopBarEvent,
    UPLOAD_PROGRESS_ROW_HEIGHT, paint_bot_start_bar, paint_business_bot_bar,
    paint_contact_status_bar_with_confirm, paint_join_requests_bar, paint_pinned_bar,
    paint_scroll_to_bottom_button, paint_selection_top_bar, paint_top_bar,
    paint_upload_progress_bar,
};
use crate::history::compose::{ComposeBotChrome, ComposeControls, ComposeEvent, SendShortcut};
use crate::history::compose_boxes::{
    AddPollOptionAction, AddPollOptionBoxState, AttachFilesAction, AttachFilesBoxState,
    BlockFromRepliesAction, BlockFromRepliesBoxState, CreatePollBoxAction, CreatePollBoxState,
    DeleteConfirmAction, DeleteConfirmBoxState, LocationPickerAction, LocationPickerBoxState,
    PhotoEditorModalState, ReportMessageAction, ReportMessageBoxState, ScheduleBoxAction,
    ScheduleBoxMode, ScheduleBoxState, ShareContactAction, ShareContactBoxState,
};
use crate::history::compose_extras::{
    InlineBotQueryHook, ReactionsPickerAction, ReactionsPickerState, TRANSLATE_BAR_HEIGHT,
    TranslateBarAction, TranslateBarState, detect_chat_language,
};
use crate::history::inner::{HistoryInnerWidget, InnerEvent};
use crate::history::mock::MockHistoryStore;
use crate::history::rich_compose::{find_last_editable_outgoing, step_reply_target};
use crate::history::sections_model::ThreadKey;
use crate::history::sections_ui::{
    AdminLogSectionWidget, DEFAULT_SECTION_NOW_UNIX, ForumTopicHistoryWidget, ForumTopicsWidget,
    HistorySectionAction, HistorySectionKind, PinnedSectionWidget, RepliesSectionWidget,
    SavedSublistsWidget, ScheduledSectionWidget, SelfForwardsTaggerAction, SelfForwardsTaggerState,
    SubsectionTabsWidget,
};
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::strings::Strings;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::{FullMsgId, MessageItem, MsgId, PeerId, PeerKind, PeerSummary};
use tg_app_info::boxes::about_cocoon::{AboutCocoonAction, AboutCocoonBoxState};
use tg_app_info::boxes::calendar::{CalendarBoxAction, CalendarBoxState};
use tg_app_info::boxes::compose_ai::{
    ComposeAiBoxAction, ComposeAiBoxState, TranslateBoxAction, TranslateBoxState,
};
use tg_app_info::boxes::join_requests::{JoinRequestsAction, JoinRequestsBoxState};
use tg_app_info::boxes::self_destruct::{
    SelfDestructionBoxAction, SelfDestructionBoxState, SelfDestructionBoxType,
    ViewOnceMediaModalState, ViewOnceModalAction,
};
use tg_app_info::boxes::sticker_creator::{
    StickerCreatorAction, StickerCreatorBoxState, StickerSetKind,
};
use tg_app_info::info::reactions_list::{
    PEER_LIST_PAGE_LIMIT, PollVotersAction, PollVotersWidget, ReactionsListAction,
    ReactionsListWidget,
};
use tg_app_media::media::player_bar::AudioPlayerBarEvent;
use tg_app_media::media::style::MEDIA_PLAYER_HEIGHT;

/// Simulated async latency for inline callback buttons so the loading spinner is observable.
const CALLBACK_SPINNER_DELAY_MS: u64 = 240;

#[derive(Debug, Clone)]
struct PendingHistoryCallback {
    msg_id: tg_bots::stand_in::FullMsgId,
    row: usize,
    col: usize,
    request: CallbackQueryRequest,
    bot_id: PeerId,
    bot_name: String,
    bot_verified: bool,
    bot_trusted_for_game: bool,
    deliver_at_ms: u64,
}

/// Forward draft to put on the compose bar once the chosen chat is open
/// (`MainWidget::setForwardDraft` after picking a chat in "Forward to...").
#[derive(Debug, Clone)]
struct PendingForwardDraft {
    target_peer: PeerId,
    from_peer: PeerId,
    msg_ids: Vec<MsgId>,
    summary: String,
}

#[derive(Debug, Clone)]
struct PendingInlinePrefill {
    target_peer: PeerId,
    bot_id: PeerId,
    bot_username: String,
    query: String,
}

/// Top bar height matching tdesktop's history top bar (`st::topBarHeight = 54px`).
pub const HISTORY_TOP_BAR_HEIGHT: f32 = 54.0;
/// Base compose bar height (`st::historySendSize.height`).
pub const COMPOSE_BAR_HEIGHT: f32 = 52.0;

/// Callback hook invoked when the user clicks the Emoji / Stickers / GIFs button
/// so `chat_helpers` can open or toggle its tabbed selector panel.
pub type EmojiPanelCallback = Box<dyn FnMut(PeerId) + Send>;

/// Events emitted by [`HistoryWidget`] to the parent shell (`MainWidget`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryAction {
    /// User clicked the top-left Back button.
    Back,
    /// User clicked the peer header or Info toggle button.
    TogglePeerInfo(PeerId),
    /// User submitted a message from the compose bar.
    SendMessage {
        /// Target peer id.
        peer_id: PeerId,
        /// Non-empty trimmed message body.
        text: String,
    },
    /// User clicked a media attachment (photo, video, album tile, or voice note).
    OpenMedia {
        /// Peer owning the message.
        peer_id: PeerId,
        /// Message identifier.
        msg_id: MsgId,
        /// Optional index inside a grouped album.
        album_index: Option<usize>,
    },
    /// User clicked a link or mention/bot command URL in a message.
    OpenLink(String),
    /// User clicked a hidden `messageEntityTextUrl` link or sandboxed widget external link
    /// requiring scheme validation and confirmation before opening externally.
    OpenHiddenLink {
        /// Target URL.
        url: String,
        /// When `true`, always show confirmation even on allowlisted domains (widget `ConfirmExternal`).
        force_confirm: bool,
    },
    /// User launched a Bot Mini App (or embedded HTML5 Game WebView) from the chat.
    ///
    /// Port of `InlineBots::AttachWebView::open` (`inline_bots/bot_attach_web_view.cpp`):
    /// requests a WebView launch URL with active theme params and opens the embedded Mini App panel.
    OpenMiniApp {
        /// Target bot peer id (`PeerId::from_user(...)`).
        bot_id: PeerId,
        /// Optional explicit button or game WebView URL (`None` for attach-menu / main-app launches).
        url: Option<String>,
        /// Optional deep-link `start_param` payload.
        start_param: Option<String>,
        /// Keyboard, bot menu and switch-inline buttons ask first (`WebViewInstance::confirmOpen`
        /// in `bot_attach_web_view.cpp`) unless the bot is verified or already trusted; attach
        /// menu, main app and game launches have their own flow.
        confirm_open: bool,
    },
    /// User clicked a `switch_inline` button without `same_peer`: open a peer picker filtered
    /// by `peer_types`, then switch to the chosen chat with `@bot_username query` prefilled.
    SwitchInlineChooseChat {
        /// Target bot peer id (`PeerId::from_user(...)`).
        bot_id: PeerId,
        /// Bot username without leading `@`.
        bot_username: String,
        /// Pre-filled inline query string.
        query: String,
        /// Allowed destination chat types (`PeerTypes::empty()` allows any sendable chat).
        peer_types: tg_bots::markup::PeerTypes,
    },
    /// Switch directly to `target_peer` and prefill `@bot_username query` in its compose field.
    SwitchInlineToPeer {
        /// Destination chat peer id.
        target_peer: PeerId,
        /// Target bot peer id (`PeerId::from_user(...)`).
        bot_id: PeerId,
        /// Bot username without leading `@`.
        bot_username: String,
        /// Pre-filled inline query string.
        query: String,
    },
    /// User clicked an Invoice or Payment Receipt bubble (or checkout inline button).
    OpenInvoice {
        /// Peer owning the invoice message.
        peer_id: PeerId,
        /// Invoice message id.
        msg_id: MsgId,
        /// Invoice identifier (`tg://invoice?slug=...`, `msg:<peer>:<msg>`, or `receipt:<peer>:<msg>`).
        invoice_id: String,
    },
    /// User clicked a Paid Media unlock bubble or locked preview.
    UnlockPaidMedia {
        /// Peer owning the paid media message.
        peer_id: PeerId,
        /// Paid media message id.
        msg_id: MsgId,
        /// Stars amount required to unlock.
        stars_amount: i64,
        /// Display title for the unlock box.
        title: String,
    },
    /// User clicked the compose Emoji / Stickers / GIFs toggle button.
    ToggleEmojiPanel(PeerId),
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
        /// Primary host channel peer ID, if known.
        channel_id: Option<PeerId>,
        /// Original launch message ID (for GiveawayResults).
        launch_msg_id: Option<MsgId>,
    },
    /// User clicked an interactive Live Video Stream card (`media_view_video_stream.cpp`).
    OpenVideoStream {
        /// Owning chat peer ID.
        peer_id: PeerId,
        /// Message ID.
        msg_id: MsgId,
        /// Group call ID.
        call_id: i64,
        /// Group call access hash.
        access_hash: u64,
        /// Whether backed by an RTMP/HLS stream.
        rtmp_stream: bool,
        /// Display title.
        title: String,
    },
    /// User opened `ModerateMessagesBox` from a group/channel message context menu.
    OpenModerateMessages {
        /// Owning group/supergroup/channel peer ID.
        peer_id: PeerId,
        /// Display title of the chat.
        peer_title: String,
        /// Selected message IDs to moderate.
        message_ids: Vec<i32>,
        /// Distinct non-outgoing authors of those messages.
        participants: Vec<tg_app_info::boxes::moderate_messages::ModerateParticipant>,
    },
    /// User opened `MusicAttachBox` from the compose attach menu (`boxes/music_attach_box.cpp`).
    OpenMusicAttach(PeerId),
    /// User pulled-to-next-channel (or clicked the footer) in a broadcast channel.
    OpenNextUnreadChannel(PeerId),
    /// User clicked a resolved channel post author (`channels.getMessageAuthor`) to view profile.
    OpenPeerProfile(PeerId),
    /// Informational toast message to display in the host shell.
    Toast(String),
}

/// Complete stateful chat history view (`HistoryWidget`).
///
/// Retains `Default`, `Debug`, `request_scroll_to_bottom`, and `show(...)` compatibility with
/// `MainWidget`, while internally running the full `tg-history` + `tg-ui` + `tg-text` +
/// `tg-media` + `tg-lottie` pipeline and exposing [`Self::show_with_backend`] for direct
/// `HistoryBackend` injection.
pub struct HistoryWidget {
    inner: HistoryInnerWidget,
    compose: ComposeControls,
    embedded_store: MockHistoryStore,
    active_peer: PeerId,
    pinned_bar_index: usize,
    search_open: bool,
    search_query: String,
    status_toast: Option<(String, f64)>,
    on_emoji_panel_toggle: Option<EmojiPanelCallback>,
    emoji_panel_open: bool,
    last_compose_rect: Rect,
    // Extended sections & compose modals
    active_section: Option<HistorySectionKind>,
    scheduled_section: ScheduledSectionWidget,
    pinned_section: PinnedSectionWidget,
    replies_section: RepliesSectionWidget,
    forum_topics: ForumTopicsWidget,
    forum_topic_history: ForumTopicHistoryWidget,
    saved_sublists: SavedSublistsWidget,
    admin_log: AdminLogSectionWidget,
    subsection_tabs: SubsectionTabsWidget,
    self_forwards_tagger: SelfForwardsTaggerState,
    schedule_box: Option<ScheduleBoxState>,
    poll_box: Option<CreatePollBoxState>,
    location_box: Option<LocationPickerBoxState>,
    contact_box: Option<ShareContactBoxState>,
    attach_box: Option<AttachFilesBoxState>,
    delete_box: Option<DeleteConfirmBoxState>,
    report_box: Option<ReportMessageBoxState>,
    block_from_replies_box: Option<BlockFromRepliesBoxState>,
    rich_editor: Option<tg_app_media::webview::rich_editor::RichEditorWindow>,
    video_drafts_dir: Option<PathBuf>,
    rich_export_dir: Option<PathBuf>,
    rich_export_dialog: Option<Arc<dyn tg_app_media::webview::rich_editor::ExportFolderDialog>>,
    join_requests_box: Option<JoinRequestsBoxState>,
    reactions_picker: ReactionsPickerState,
    translate_bar: TranslateBarState,
    who_reacted_modal: Option<ReactionsListWidget>,
    seen_by_modal: Option<read_receipts::SeenByModalWidget>,
    unread_things: unread_things::HistoryUnreadThingsWidget,
    pull_next_channel: pull_to_next_channel::PullToNextChannelWidget,
    next_unread_channel: Option<pull_to_next_channel::NextUnreadChannelHint>,
    poll_voters_modal: Option<PollVotersWidget>,
    /// Whether `HistoryWidget` paints its own top `AudioPlayerBar` (true standalone;
    /// false when `MainWidget` renders the single shared top `AudioPlayerBar`).
    render_internal_audio_bar: bool,
    /// Active Telegram Premium tier used for history limits (caption length, saved GIFs).
    is_premium: bool,
    /// Optional explicit override for business quick replies (used by tests; when `None`,
    /// live shortcuts are pulled from `AppBusinessHost`).
    business_quick_replies_override: Option<(bool, Vec<QuickReplyShortcut>)>,
    /// Current saved GIFs count for enforcing `PremiumLimitKind::SavedGifs`.
    saved_gifs_count: usize,
    /// Embedded `business::premium::PremiumLimitsView` modal opened when a history action
    /// exceeds caption length or saved GIFs limits.
    premium_limits_modal: Option<(
        PremiumLimitsView,
        BusinessOverviewSnapshot,
        MockBusinessBackend,
    )>,
    // Bot chat widgets & state (`crates/tg-app/src/bots/`)
    reply_keyboard: ReplyKeyboardWidget,
    inline_results: InlineResultsPanel,
    callback_cache: CallbackAnswerCache,
    pending_callbacks: Vec<PendingHistoryCallback>,
    bot_commands_open: bool,
    bot_commands_opened_by_slash: bool,
    bot_commands_dismissed_for: Option<String>,
    bot_attach_open: bool,
    bot_modal: Option<BotModalBox>,
    last_inline_compose_text: String,
    pending_inline_prefill: Option<PendingInlinePrefill>,
    pending_forward_draft: Option<PendingForwardDraft>,
    /// Forward bars parked with the chat they were set in (`History::forwardDraft`): switching
    /// chats keeps each forward draft with its own chat instead of moving it to the next one.
    forward_drafts: HashMap<PeerId, crate::history::compose::ComposeMode>,
    /// `BotInfo::startToken` per bot chat, set by a `tg://resolve?domain=<bot>&start=<token>`
    /// link and sent with `messages.startBot` when the user presses START.
    bot_start_tokens: HashMap<PeerId, String>,
    /// Whether the last frame showed the bottom START bar instead of the composer.
    bot_start_shown: bool,
    pending_game_url_bot: Option<PeerId>,
    trusted_game_bots: HashSet<PeerId>,
    // Layer-233 history extras (`todo.rs`, `sponsored.rs`, `factcheck.rs`, `monoforum.rs`)
    todo_box: Option<todo::EditTodoListBoxState>,
    add_todo_tasks_box: Option<todo::AddTodoListTasksBoxState>,
    about_sponsored_box: Option<sponsored::AboutSponsoredBoxState>,
    report_sponsored_box: Option<sponsored::ReportSponsoredBoxState>,
    edit_factcheck_box: Option<factcheck::EditFactCheckBoxState>,
    calendar_box: Option<CalendarBoxState>,
    edit_caption_box: Option<edit_caption::EditCaptionBoxState>,
    monoforum_section: monoforum::MonoforumSectionWidget,
    monoforum_active: Option<(PeerId, String, bool)>,
    active_widget_host: Option<tg_app_media::webview::widget_host::LiveWidgetHost>,
    active_widget_title: String,
    active_widget_query_status: Option<String>,
    widget_helper_path: Option<std::path::PathBuf>,
    // Decorative UI effects (`tg-ui::effects::{thanos, fly}`) & Draw-to-Reply (`tg-editor`)
    thanos_effect: tg_ui::effects::ThanosEffect,
    fly_effects: tg_ui::effects::FlyEffectsManager,
    effects_chat_enabled: bool,
    effects_reactions_enabled: bool,
    effects_emoji_enabled: bool,
    effects_animations_enabled: bool,
    last_list_rect: Rect,
    draw_to_reply_editor: Option<(tg_editor::DrawToReplyRequest, PhotoEditorModalState)>,
    // WP-28 modals: TranslateBox, Compose with AI, Sticker Creator, Self-Destruct, Add Poll Option, View-Once Viewer
    translate_box: Option<TranslateBoxState>,
    compose_ai_box: Option<ComposeAiBoxState>,
    about_cocoon_box: Option<AboutCocoonBoxState>,
    sticker_creator_box: Option<StickerCreatorBoxState>,
    self_destruct_box: Option<SelfDestructionBoxState>,
    add_poll_option_box: Option<AddPollOptionBoxState>,
    view_once_modal: Option<ViewOnceMediaModalState>,
    welcome_messages_section: welcome_messages::WelcomeMessagesSectionWidget,
    welcome_messages_active: bool,
    // WP-38: Compose stash hint/menu & interactive emoji coordinator
    stash_hint: compose_stash::StashHintManager,
    stash_menu_open: bool,
    last_compose_text_for_hint: String,
    emoji_coordinator: emoji_interactions::EmojiInteractionCoordinator,
    geo_provider: Option<std::sync::Arc<dyn tg_platform::geolocation::GeoLocationProvider>>,
    /// Device location fix for a bot `RequestLocation` button, in flight on a worker thread.
    bot_location_pending: Option<(
        PeerId,
        tg_app_core::core_types::platform_call::PlatformCall<compose_boxes::LocationFixResult>,
    )>,
    /// Highest message ID already observed per peer. Only messages above it (newly sent or
    /// newly arrived) trigger Lottie message-effect playback; history loaded on open or by
    /// scrolling up never replays effects.
    effect_watermark: HashMap<PeerId, MsgId>,
    /// Peer currently showing inline Report Spam confirmation inside the contact status bar.
    contact_status_report_confirm_peer: Option<PeerId>,
}

/// Returns `(msg_id, effect_id)` for messages above `peer_id`'s watermark (newly sent or newly
/// arrived) and advances the watermark to the newest loaded message. The first call for a peer
/// only seeds the watermark, so history shown on open or loaded by scrolling up never replays
/// its message effects.
#[doc(hidden)]
pub fn take_new_effect_messages(
    watermarks: &mut HashMap<PeerId, MsgId>,
    peer_id: PeerId,
    messages: &[HistoryMessage],
) -> Vec<(MsgId, u64)> {
    let Some(newest) = messages.iter().map(|m| m.id).max() else {
        return Vec::new();
    };
    let Some(watermark) = watermarks.insert(peer_id, newest) else {
        return Vec::new();
    };
    if newest <= watermark {
        watermarks.insert(peer_id, watermark);
        return Vec::new();
    }
    messages
        .iter()
        .filter(|m| m.id > watermark)
        .filter_map(|m| m.extra.effect_id.map(|eff_id| (m.id, eff_id)))
        .collect()
}

impl fmt::Debug for HistoryWidget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HistoryWidget")
            .field("active_peer", &self.active_peer)
            .field("pinned_bar_index", &self.pinned_bar_index)
            .field("search_open", &self.search_open)
            .field("emoji_panel_open", &self.emoji_panel_open)
            .finish()
    }
}

impl Default for HistoryWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryWidget {
    /// Creates a new `HistoryWidget` with an embedded rich [`MockHistoryStore`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: HistoryInnerWidget::new(),
            compose: ComposeControls::new(),
            embedded_store: MockHistoryStore::new(),
            active_peer: PeerId::ZERO,
            pinned_bar_index: 0,
            search_open: false,
            search_query: String::new(),
            status_toast: None,
            on_emoji_panel_toggle: None,
            emoji_panel_open: false,
            last_compose_rect: Rect::NOTHING,
            active_section: None,
            scheduled_section: ScheduledSectionWidget::default(),
            pinned_section: PinnedSectionWidget::default(),
            replies_section: RepliesSectionWidget::default(),
            forum_topics: ForumTopicsWidget::new(),
            forum_topic_history: ForumTopicHistoryWidget::default(),
            saved_sublists: SavedSublistsWidget::default(),
            admin_log: AdminLogSectionWidget::default(),
            subsection_tabs: SubsectionTabsWidget::new(),
            self_forwards_tagger: SelfForwardsTaggerState::default(),
            schedule_box: None,
            poll_box: None,
            location_box: None,
            contact_box: None,
            attach_box: None,
            delete_box: None,
            report_box: None,
            block_from_replies_box: None,
            rich_editor: None,
            video_drafts_dir: None,
            rich_export_dir: None,
            rich_export_dialog: None,
            join_requests_box: None,
            reactions_picker: ReactionsPickerState::new(),
            translate_bar: TranslateBarState::default(),
            who_reacted_modal: None,
            seen_by_modal: None,
            unread_things: unread_things::HistoryUnreadThingsWidget::new(),
            pull_next_channel: pull_to_next_channel::PullToNextChannelWidget::new(),
            next_unread_channel: None,
            poll_voters_modal: None,
            render_internal_audio_bar: true,
            is_premium: false,
            business_quick_replies_override: None,
            saved_gifs_count: 0,
            premium_limits_modal: None,
            reply_keyboard: ReplyKeyboardWidget::new(),
            inline_results: InlineResultsPanel::new(),
            callback_cache: CallbackAnswerCache::new(),
            pending_callbacks: Vec::new(),
            bot_commands_open: false,
            bot_commands_opened_by_slash: false,
            bot_commands_dismissed_for: None,
            bot_attach_open: false,
            bot_modal: None,
            last_inline_compose_text: String::new(),
            pending_inline_prefill: None,
            pending_forward_draft: None,
            forward_drafts: HashMap::new(),
            bot_start_tokens: HashMap::new(),
            bot_start_shown: false,
            pending_game_url_bot: None,
            trusted_game_bots: HashSet::new(),
            todo_box: None,
            add_todo_tasks_box: None,
            about_sponsored_box: None,
            report_sponsored_box: None,
            edit_factcheck_box: None,
            calendar_box: None,
            edit_caption_box: None,
            monoforum_section: monoforum::MonoforumSectionWidget::default(),
            monoforum_active: None,
            active_widget_host: None,
            active_widget_title: String::new(),
            active_widget_query_status: None,
            widget_helper_path: None,
            thanos_effect: tg_ui::effects::ThanosEffect::new(),
            fly_effects: tg_ui::effects::FlyEffectsManager::new(),
            effects_chat_enabled: true,
            effects_reactions_enabled: true,
            effects_emoji_enabled: true,
            effects_animations_enabled: true,
            last_list_rect: Rect::NOTHING,
            draw_to_reply_editor: None,
            translate_box: None,
            compose_ai_box: None,
            about_cocoon_box: None,
            sticker_creator_box: None,
            self_destruct_box: None,
            add_poll_option_box: None,
            view_once_modal: None,
            welcome_messages_section: welcome_messages::WelcomeMessagesSectionWidget::default(),
            welcome_messages_active: false,
            stash_hint: compose_stash::StashHintManager::new(),
            stash_menu_open: false,
            last_compose_text_for_hint: String::new(),
            emoji_coordinator: emoji_interactions::EmojiInteractionCoordinator::new(),
            geo_provider: None,
            bot_location_pending: None,
            effect_watermark: HashMap::new(),
            contact_status_report_confirm_peer: None,
        }
    }

    /// Configures the resolved path to `rustgram-widget-helper.exe` passed to launched
    /// [`tg_app_media::webview::widget_host::LiveWidgetHost`] instances.
    pub fn set_widget_helper_path(&mut self, path: std::path::PathBuf) {
        if let Some(ref mut active) = self.active_widget_host {
            active.set_helper_executable(path.clone());
        }
        self.widget_helper_path = Some(path);
    }

    /// Returns the configured path to `rustgram-widget-helper.exe`, if any.
    #[must_use]
    pub fn widget_helper_path(&self) -> Option<&std::path::Path> {
        self.widget_helper_path.as_deref()
    }

    /// Opens the Replies-chat block confirmation modal for `(msg_id, sender_name)`.
    pub fn open_block_from_replies_modal(&mut self, msg_id: MsgId, sender_name: impl Into<String>) {
        self.block_from_replies_box = Some(BlockFromRepliesBoxState::new(msg_id, sender_name));
    }

    /// Returns `true` while the Replies-chat block confirmation modal is visible.
    #[must_use]
    pub const fn is_block_from_replies_open(&self) -> bool {
        self.block_from_replies_box.is_some()
    }

    /// Returns read-only access to the inline Saved Messages self-forwards tagger.
    #[must_use]
    pub const fn self_forwards_tagger(&self) -> &SelfForwardsTaggerState {
        &self.self_forwards_tagger
    }

    /// Returns mutable access to the inline Saved Messages self-forwards tagger.
    pub fn self_forwards_tagger_mut(&mut self) -> &mut SelfForwardsTaggerState {
        &mut self.self_forwards_tagger
    }

    /// Exchanges the current composer content with the per-chat compose stash (`Ctrl+S` /
    /// corner stash button), persisting via `backend.set_compose_stash` and syncing the cloud draft.
    ///
    /// Returns `true` if a stash or swap occurred.
    pub fn exchange_compose_stash(
        &mut self,
        peer_id: PeerId,
        backend: &mut dyn HistoryBackend,
        now_secs: f64,
    ) -> bool {
        let key = compose_stash::ComposeStashKey::main(peer_id);
        let existing = backend.compose_stash(key);
        let current = self.take_current_compose_for_stash();
        if existing.is_none() && current.is_none() {
            return false;
        }
        self.stash_hint.mark_used();
        self.stash_menu_open = false;
        backend.set_compose_stash(key, current);
        if let Some(restored) = existing {
            self.apply_compose_stash(restored);
            self.show_toast("Restored stashed draft", now_secs);
        } else {
            self.compose.text.clear();
            self.compose.rich_edit.clear();
            self.compose.cancel_mode();
            self.show_toast("Draft stashed (Ctrl+S to restore)", now_secs);
        }
        self.last_compose_text_for_hint = self.compose.text.clone();
        self.stash_hint
            .track_change(&self.compose.text, false, true, (now_secs * 1000.0) as u64);
        backend.save_rich_draft(ThreadKey::Main(peer_id), self.compose.current_rich_draft());
        true
    }

    fn take_current_compose_for_stash(&self) -> Option<compose_stash::ComposeStash> {
        let draft = self.compose.current_rich_draft().unwrap_or_default();
        let (reply_author, reply_preview, forward) = match &self.compose.mode {
            crate::history::compose::ComposeMode::Reply {
                author, preview, ..
            } => (Some(author.clone()), Some(preview.clone()), None),
            crate::history::compose::ComposeMode::Forward {
                from_peer,
                msg_ids,
                summary,
                options,
            } => (
                None,
                None,
                Some(compose_stash::StashedForward {
                    from_peer: *from_peer,
                    msg_ids: msg_ids.clone(),
                    options: *options,
                    summary: summary.clone(),
                }),
            ),
            _ => (None, None, None),
        };
        let stash = compose_stash::ComposeStash {
            draft,
            reply_author,
            reply_preview,
            forward,
            files: Vec::new(),
        };
        stash.has_content().then_some(stash)
    }

    fn apply_compose_stash(&mut self, stash: compose_stash::ComposeStash) {
        let has_files = stash.has_files();
        let stashed_files = stash.files;
        let caption_text = stash.draft.text.text.clone();
        if !has_files {
            self.compose.apply_rich_draft(
                if !stash.draft.text.text.is_empty() || stash.draft.reply_to.is_some() {
                    Some(stash.draft.clone())
                } else {
                    None
                },
            );
        } else {
            self.compose.apply_rich_draft(None);
            let mut attach = AttachFilesBoxState::from_stashed_files(
                &stashed_files,
                &caption_text,
                false,
                self.is_premium,
            );
            attach.video_drafts_dir = self.video_drafts_dir.clone();
            self.attach_box = Some(attach);
        }
        if let Some(reply_id) = stash.draft.reply_to {
            let author = stash.reply_author.unwrap_or_else(|| "Message".to_owned());
            let preview = stash.reply_preview.unwrap_or_default();
            if let Some(quote) = stash.draft.reply_quote {
                self.compose
                    .start_reply_with_quote(reply_id, author, preview, quote);
            } else {
                self.compose.start_reply(reply_id, author, preview);
            }
        }
        if let Some(fwd) = stash.forward
            && !fwd.msg_ids.is_empty()
        {
            self.compose.mode = crate::history::compose::ComposeMode::Forward {
                from_peer: fwd.from_peer,
                msg_ids: fwd.msg_ids,
                summary: fwd.summary,
                options: fwd.options,
            };
        }
        self.compose.focus_requested = true;
    }

    /// Injects a [`tg_platform::geolocation::GeoLocationProvider`] (e.g. `FakeGeoLocationProvider`
    /// in tests) for `LocationPickerBoxState` and bot `RequestLocation` modals.
    pub fn set_geo_provider(
        &mut self,
        provider: std::sync::Arc<dyn tg_platform::geolocation::GeoLocationProvider>,
    ) {
        self.geo_provider = Some(provider);
    }

    /// Starts the device location fix for a bot `RequestLocation` button on a worker thread;
    /// [`Self::poll_bot_location_share`] sends the location to `peer_id` once it resolves.
    ///
    /// Returns `true` when a lookup was started.
    pub fn start_bot_location_share(
        &mut self,
        peer_id: PeerId,
        repaint: Option<egui::Context>,
        now_secs: f64,
    ) -> bool {
        use tg_platform::geolocation::GeolocationStatus;
        if self.bot_location_pending.is_some() {
            return false;
        }
        let provider = self.active_geo_provider();
        if matches!(
            provider.status(),
            GeolocationStatus::Disabled | GeolocationStatus::NotAvailable
        ) {
            self.show_toast(
                "Windows Location Services are disabled or unavailable.",
                now_secs,
            );
            return false;
        }
        let call = tg_app_core::core_types::platform_call::PlatformCall::spawn(
            "geolocation-bot",
            repaint,
            move || {
                let mut modal = crate::bots::boxes::BotModalBox::request_location("Bot");
                modal.resolve_location_with(provider.as_ref())
            },
        );
        self.bot_location_pending = Some((peer_id, call));
        self.show_toast("Getting your location\u{2026}", now_secs);
        true
    }

    /// Sends the bot location once the fix started by [`Self::start_bot_location_share`]
    /// arrives; `None` while it is still running or when nothing is pending.
    pub fn poll_bot_location_share(
        &mut self,
        backend: &mut dyn HistoryBackend,
        now_secs: f64,
    ) -> Option<bool> {
        use tg_app_core::core_types::platform_call::PlatformCallState;
        let outcome = match self.bot_location_pending.as_ref()?.1.poll() {
            PlatformCallState::Pending => return None,
            PlatformCallState::Done(outcome) => outcome,
            PlatformCallState::Lost => Err("the location service stopped responding".to_owned()),
        };
        let (peer_id, _) = self.bot_location_pending.take()?;
        Some(self.finish_bot_location_share(peer_id, outcome, backend, now_secs))
    }

    /// Blocks up to `timeout` for the bot location fix, then sends it (tests only).
    #[doc(hidden)]
    pub fn wait_bot_location_share(
        &mut self,
        timeout: std::time::Duration,
        backend: &mut dyn HistoryBackend,
        now_secs: f64,
    ) -> Option<bool> {
        use tg_app_core::core_types::platform_call::PlatformCallState;
        let outcome = match self.bot_location_pending.as_ref()?.1.wait(timeout) {
            PlatformCallState::Pending => return None,
            PlatformCallState::Done(outcome) => outcome,
            PlatformCallState::Lost => Err("the location service stopped responding".to_owned()),
        };
        let (peer_id, _) = self.bot_location_pending.take()?;
        Some(self.finish_bot_location_share(peer_id, outcome, backend, now_secs))
    }

    fn finish_bot_location_share(
        &mut self,
        peer_id: PeerId,
        outcome: compose_boxes::LocationFixResult,
        backend: &mut dyn HistoryBackend,
        now_secs: f64,
    ) -> bool {
        match outcome {
            Ok(Some(loc)) if loc.point.is_valid() => self.send_bot_location(
                peer_id,
                loc.point.latitude,
                loc.point.longitude,
                backend,
                now_secs,
            ),
            Ok(_) => {
                self.show_toast("Could not determine your current location.", now_secs);
                false
            }
            Err(err) => {
                self.show_toast(format!("Location lookup failed: {err}"), now_secs);
                false
            }
        }
    }

    /// Sends `(lat, lon)` as a location message to the bot chat `peer_id`.
    fn send_bot_location(
        &mut self,
        peer_id: PeerId,
        lat: f64,
        lon: f64,
        backend: &mut dyn HistoryBackend,
        now_secs: f64,
    ) -> bool {
        let req = SendMessageExtended {
            media: Some(HistoryMedia::Location(
                crate::history::sections_model::LocationAttachment {
                    lat,
                    lon,
                    venue_title: None,
                    venue_address: None,
                    live_period_secs: None,
                },
            )),
            silent: self.compose.silent_mode,
            ..SendMessageExtended::default()
        };
        match backend.send_message_extended(peer_id, req) {
            Ok(_) => {
                if peer_id == self.active_peer {
                    self.reload_active_peer(backend);
                }
                self.show_toast("Location shared with bot", now_secs);
                true
            }
            Err(err) => {
                self.show_toast(format!("Could not share location: {err}"), now_secs);
                false
            }
        }
    }

    fn active_geo_provider(
        &self,
    ) -> std::sync::Arc<dyn tg_platform::geolocation::GeoLocationProvider> {
        self.geo_provider
            .clone()
            .unwrap_or_else(tg_app_core::core_types::geo::default_geolocation_provider)
    }

    /// Returns a reference to the currently open [`LocationPickerBoxState`], if any.
    #[must_use]
    pub const fn location_box(&self) -> Option<&LocationPickerBoxState> {
        self.location_box.as_ref()
    }

    /// Returns a mutable reference to the currently open [`LocationPickerBoxState`], if any.
    pub fn location_box_mut(&mut self) -> Option<&mut LocationPickerBoxState> {
        self.location_box.as_mut()
    }

    /// Opens the Attach / Send Files modal (`SendFilesBox` / [`AttachFilesBoxState`]).
    pub fn open_attach_box(&mut self, slowmode: bool) {
        let mut attach = AttachFilesBoxState::new(slowmode, self.is_premium);
        attach.video_drafts_dir = self.video_drafts_dir.clone();
        self.attach_box = Some(attach);
    }

    /// Configures the profile directory backing [`video_editor::VideoEditorModalState::draft_path`].
    pub fn set_video_drafts_dir(&mut self, dir: Option<PathBuf>) {
        self.video_drafts_dir = dir.clone();
        if let Some(ref mut attach) = self.attach_box {
            attach.video_drafts_dir = dir;
        }
    }

    /// Returns the configured video editor drafts directory, if any.
    #[must_use]
    pub fn video_drafts_dir(&self) -> Option<&Path> {
        self.video_drafts_dir.as_deref()
    }

    /// Configures the default / initial HTML export directory for [`tg_app_media::webview::rich_editor::RichEditorWindow`].
    pub fn set_rich_export_dir(&mut self, dir: Option<PathBuf>) {
        self.rich_export_dir = dir.clone();
        if let Some(ref mut editor) = self.rich_editor {
            editor.set_export_dir(dir);
        }
    }

    /// Returns the configured default / initial rich-document HTML export directory, if any.
    #[must_use]
    pub fn rich_export_dir(&self) -> Option<&Path> {
        self.rich_export_dir.as_deref()
    }

    /// Configures the injectable folder dialog used by [`tg_app_media::webview::rich_editor::RichEditorWindow`]
    /// when the user clicks `"Save as HTML"`.
    pub fn set_rich_export_dialog(
        &mut self,
        dialog: Option<Arc<dyn tg_app_media::webview::rich_editor::ExportFolderDialog>>,
    ) {
        self.rich_export_dialog = dialog.clone();
        if let Some(ref mut editor) = self.rich_editor {
            editor.set_export_dialog(dialog);
        }
    }

    /// Opens the rich document editor in compose mode for `peer_id` with the configured
    /// profile export directory and folder chooser dialog applied.
    pub fn open_rich_editor_compose(&mut self, peer_id: PeerId) {
        let mut editor = tg_app_media::webview::rich_editor::RichEditorWindow::new_compose(peer_id);
        editor.set_export_dir(self.rich_export_dir.clone());
        editor.set_export_dialog(self.rich_export_dialog.clone());
        self.rich_editor = Some(editor);
    }

    /// Returns immutable access to the open [`tg_app_media::webview::rich_editor::RichEditorWindow`], if any.
    #[must_use]
    pub fn rich_editor(&self) -> Option<&tg_app_media::webview::rich_editor::RichEditorWindow> {
        self.rich_editor.as_ref()
    }

    /// Returns mutable access to the open [`tg_app_media::webview::rich_editor::RichEditorWindow`], if any.
    pub fn rich_editor_mut(
        &mut self,
    ) -> Option<&mut tg_app_media::webview::rich_editor::RichEditorWindow> {
        self.rich_editor.as_mut()
    }

    /// Returns `true` when the Send Files modal ([`AttachFilesBoxState`]) is currently open.
    #[must_use]
    pub const fn is_attach_box_open(&self) -> bool {
        self.attach_box.is_some()
    }

    /// Returns an immutable reference to the active Send Files modal ([`AttachFilesBoxState`]), if open.
    #[must_use]
    pub fn attach_box(&self) -> Option<&AttachFilesBoxState> {
        self.attach_box.as_ref()
    }

    /// Returns a mutable reference to the active Send Files modal ([`AttachFilesBoxState`]), if open.
    pub fn attach_box_mut(&mut self) -> Option<&mut AttachFilesBoxState> {
        self.attach_box.as_mut()
    }

    /// Closes the active Send Files modal ([`AttachFilesBoxState`]), if open.
    pub fn close_attach_box(&mut self) {
        self.attach_box = None;
    }

    /// Opens the Create Checklist modal (`EditTodoListBox`).
    pub fn open_create_todo_box(&mut self) {
        self.todo_box = Some(todo::EditTodoListBoxState::new_create());
    }

    /// Opens `EditTodoListBox` in Edit mode for an existing checklist message.
    pub fn open_edit_todo_box(&mut self, msg_id: MsgId, existing: &todo::TodoListAttachment) {
        self.todo_box = Some(todo::EditTodoListBoxState::new_edit(msg_id, existing));
    }

    /// Opens `AddTodoListTasksBox` to append tasks to an existing checklist message.
    pub fn open_add_todo_tasks_box(&mut self, msg_id: MsgId, existing: &todo::TodoListAttachment) {
        self.add_todo_tasks_box = Some(todo::AddTodoListTasksBoxState::for_checklist(
            msg_id, existing,
        ));
    }

    /// Opens `AboutSponsoredBox` explaining Telegram Sponsored Messages.
    pub fn open_about_sponsored_box(&mut self) {
        self.about_sponsored_box = Some(sponsored::AboutSponsoredBoxState::new(self.is_premium));
    }

    /// Opens the Jump-to-Date Calendar modal (`CalendarBox`) for `peer_id`.
    pub fn open_calendar_box(
        &mut self,
        peer_id: PeerId,
        peer_title: impl Into<String>,
        now_unix: i32,
    ) {
        self.calendar_box = Some(CalendarBoxState::new(peer_id, peer_title, now_unix));
    }

    /// Opens the Edit-Caption modal (`EditCaptionBox`) for a media message.
    pub fn open_edit_caption_box(&mut self, peer_id: PeerId, msg: &HistoryMessage) {
        self.edit_caption_box = Some(edit_caption::EditCaptionBoxState::from_message(
            peer_id,
            msg,
            self.is_premium,
        ));
    }

    /// Opens `EditFactCheckBox` for `msg_id`.
    pub fn open_edit_factcheck_box(
        &mut self,
        msg_id: MsgId,
        current: Option<&factcheck::FactCheckData>,
    ) {
        self.edit_factcheck_box = Some(factcheck::EditFactCheckBoxState::new(
            msg_id,
            current,
            factcheck::FACTCHECK_DEFAULT_LENGTH_LIMIT,
        ));
    }

    /// Opens the Channel Direct Messages (monoforum) section for `monoforum_peer_id`.
    pub fn open_monoforum_section(
        &mut self,
        monoforum_peer_id: PeerId,
        channel_title: impl Into<String>,
        is_admin: bool,
    ) {
        self.monoforum_section.active_sublist = None;
        self.monoforum_active = Some((monoforum_peer_id, channel_title.into(), is_admin));
    }

    /// Closes the Channel Direct Messages (monoforum) section if open.
    pub fn close_monoforum_section(&mut self) {
        self.monoforum_active = None;
    }

    /// Opens the Group Welcome Messages section (`history_view_welcome_messages_section`).
    pub fn open_welcome_messages_section(&mut self, peer_id: PeerId, title: impl Into<String>) {
        self.welcome_messages_section.open_for(peer_id, title);
        self.welcome_messages_active = true;
    }

    /// Closes the Group Welcome Messages section if open.
    pub fn close_welcome_messages_section(&mut self) {
        self.welcome_messages_active = false;
    }

    /// Returns `true` when the Group Welcome Messages section is currently open.
    #[must_use]
    pub const fn is_welcome_messages_open(&self) -> bool {
        self.welcome_messages_active
    }

    /// Returns a mutable reference to the embedded [`welcome_messages::WelcomeMessagesSectionWidget`].
    pub fn welcome_messages_section_mut(
        &mut self,
    ) -> &mut welcome_messages::WelcomeMessagesSectionWidget {
        &mut self.welcome_messages_section
    }
    /// Returns `true` when the Channel Direct Messages (monoforum) section is currently open.
    #[must_use]
    pub const fn is_monoforum_open(&self) -> bool {
        self.monoforum_active.is_some()
    }

    /// Returns `true` when the checklist (`EditTodoListBox`) modal is currently open.
    #[must_use]
    pub const fn is_todo_box_open(&self) -> bool {
        self.todo_box.is_some()
    }

    /// Queues a `switch_inline` compose prefill (`@bot_username query`) to apply as soon as
    /// `target_peer` becomes active (survives peer-switch draft restoration).
    pub fn set_pending_inline_query(
        &mut self,
        target_peer: PeerId,
        bot_id: PeerId,
        bot_username: impl Into<String>,
        query: impl Into<String>,
    ) {
        self.pending_inline_prefill = Some(PendingInlinePrefill {
            target_peer,
            bot_id,
            bot_username: bot_username.into(),
            query: query.into(),
        });
    }

    /// Queues a forward draft for `target_peer`: once that chat is open, its compose bar shows
    /// the forward preview for `msg_ids` from `from_peer` (`MainWidget::setForwardDraft`).
    pub fn set_pending_forward_draft(
        &mut self,
        target_peer: PeerId,
        from_peer: PeerId,
        msg_ids: Vec<MsgId>,
        summary: impl Into<String>,
    ) {
        self.pending_forward_draft = Some(PendingForwardDraft {
            target_peer,
            from_peer,
            msg_ids,
            summary: summary.into(),
        });
    }

    /// Stores the `start` token of a `tg://resolve?domain=<bot>&start=<token>` link for
    /// `bot_peer`: the chat then shows the bottom START bar until the user presses it
    /// (`HistoryWidget::isBotStart`, `botInfo->startToken`).
    pub fn set_bot_start_token(&mut self, bot_peer: PeerId, token: impl Into<String>) {
        self.bot_start_tokens.insert(bot_peer, token.into());
    }

    /// Returns the pending `start` token for `bot_peer`, if any.
    #[must_use]
    pub fn bot_start_token(&self, bot_peer: PeerId) -> Option<&str> {
        self.bot_start_tokens.get(&bot_peer).map(String::as_str)
    }

    /// Returns `true` when the last frame showed the bottom START bar instead of the composer.
    #[must_use]
    pub const fn is_bot_start_shown(&self) -> bool {
        self.bot_start_shown
    }

    /// Returns a reference to the bot reply keyboard panel state.
    #[must_use]
    pub const fn reply_keyboard(&self) -> &ReplyKeyboardWidget {
        &self.reply_keyboard
    }

    /// Returns mutable access to the bot reply keyboard panel state.
    pub fn reply_keyboard_mut(&mut self) -> &mut ReplyKeyboardWidget {
        &mut self.reply_keyboard
    }

    /// Returns a reference to the `@botname query` inline results panel state.
    #[must_use]
    pub const fn inline_results(&self) -> &InlineResultsPanel {
        &self.inline_results
    }

    /// Returns mutable access to the `@botname query` inline results panel state.
    pub fn inline_results_mut(&mut self) -> &mut InlineResultsPanel {
        &mut self.inline_results
    }

    /// Returns `true` when the bot slash-commands popup is open above the compose bar.
    #[must_use]
    pub const fn is_bot_commands_popup_open(&self) -> bool {
        self.bot_commands_open
    }

    /// Returns `true` when the attach-menu bots popup is open above the compose bar.
    #[must_use]
    pub const fn is_bot_attach_popup_open(&self) -> bool {
        self.bot_attach_open
    }

    /// Returns the active bot modal box (CallbackAlert, UrlConfirm, UrlAuth, Request*, etc.), if any.
    #[must_use]
    pub const fn active_bot_modal(&self) -> Option<&BotModalBox> {
        self.bot_modal.as_ref()
    }

    /// Returns the currently displayed ephemeral toast text inside the chat view, if any.
    #[must_use]
    pub fn active_toast(&self) -> Option<&str> {
        self.status_toast.as_ref().map(|(msg, _)| msg.as_str())
    }

    /// Returns the currently displayed peer id (`PeerId::ZERO` when none).
    #[must_use]
    pub const fn active_peer(&self) -> PeerId {
        self.active_peer
    }

    /// Returns the screen rectangle of the compose bar painted in the most recent frame.
    #[must_use]
    pub const fn last_compose_rect(&self) -> Rect {
        self.last_compose_rect
    }

    /// Opens the inline chat message search bar (`Ctrl+F`).
    pub fn open_chat_search(&mut self) {
        self.search_open = true;
    }

    /// Closes the inline chat message search bar; returns `true` if it was open.
    pub fn close_chat_search(&mut self) -> bool {
        if self.search_open {
            self.search_open = false;
            self.search_query.clear();
            true
        } else {
            false
        }
    }

    /// Returns `true` when inline chat message search is open.
    #[must_use]
    pub const fn is_chat_search_open(&self) -> bool {
        self.search_open
    }

    /// Reloads the active peer's latest history slice from `backend` (e.g. after a sticker, GIF,
    /// or notification quick-reply was added outside the compose bar).
    pub fn reload_active_peer(&mut self, backend: &dyn HistoryBackend) {
        if self.active_peer.is_zero() {
            return;
        }
        if let Some(info) = backend.peer_info(self.active_peer) {
            self.inner.load_peer(
                self.active_peer,
                MsgId::SHOW_AT_END,
                backend,
                info.summary.kind,
            );
        }
    }

    /// Drains incremental live updates from `backend` for the already-open `peer_id` and applies
    /// them to [`HistoryInnerWidget`] without re-opening the chat.
    fn sync_live_history_updates(
        &mut self,
        peer_id: PeerId,
        peer_info: &HistoryPeerInfo,
        backend: &mut dyn HistoryBackend,
        ctx: &egui::Context,
    ) {
        let updates = backend.drain_history_updates(peer_id);
        if updates.is_empty() {
            return;
        }
        let mut reply_markup_dirty = false;
        for upd in updates {
            match upd {
                tg_app_core::history::backend::HistoryLiveUpdate::ReloadRequired => {
                    let was_scrolled_up = self.inner.show_scroll_down_button();
                    let anchor = if was_scrolled_up {
                        self.inner
                            .messages()
                            .last()
                            .map_or(MsgId::SHOW_AT_END, |m| m.id)
                    } else {
                        MsgId::SHOW_AT_END
                    };
                    self.inner
                        .load_peer(peer_id, anchor, backend, peer_info.summary.kind);
                    reply_markup_dirty = true;
                }
                tg_app_core::history::backend::HistoryLiveUpdate::MessagesDeleted(ids) => {
                    self.inner.remove_messages(&ids, peer_info.summary.kind);
                    // Cancel active edit/reply targeting a deleted message (`HistoryWidget::itemRemoved`).
                    let target_deleted = match &self.compose.mode {
                        crate::history::compose::ComposeMode::Edit { msg_id, .. }
                        | crate::history::compose::ComposeMode::Reply { msg_id, .. } => {
                            ids.contains(msg_id)
                        }
                        _ => false,
                    };
                    if target_deleted {
                        self.compose.mode = crate::history::compose::ComposeMode::Normal;
                    }
                    reply_markup_dirty = true;
                }
                tg_app_core::history::backend::HistoryLiveUpdate::MessageAdded(msg) => {
                    if let Some(desc) = backend.inline_media_descriptor(peer_id, msg.id) {
                        self.inner
                            .media_runtime_mut()
                            .register_descriptor(msg.id, desc);
                    }
                    if msg.extra.reply_markup.is_some() {
                        reply_markup_dirty = true;
                    }
                    self.inner
                        .apply_live_added_message(*msg, peer_info.summary.kind);
                }
                tg_app_core::history::backend::HistoryLiveUpdate::MessageEdited(msg) => {
                    if let Some(desc) = backend.inline_media_descriptor(peer_id, msg.id) {
                        self.inner
                            .media_runtime_mut()
                            .register_descriptor(msg.id, desc);
                    }
                    if msg.extra.reply_markup.is_some() {
                        reply_markup_dirty = true;
                    }
                    self.inner.update_message_in_place(*msg);
                }
                tg_app_core::history::backend::HistoryLiveUpdate::OutboxRead { max_id } => {
                    self.inner.mark_outbox_read_up_to(max_id);
                }
                tg_app_core::history::backend::HistoryLiveUpdate::UnreadChanged {
                    unread_count,
                    inbox_read_till,
                } => {
                    self.inner.apply_unread_state(unread_count, inbox_read_till);
                }
            }
        }
        if reply_markup_dirty {
            self.sync_reply_keyboard_from_loaded(peer_id, backend);
        }
        ctx.request_repaint();
    }

    /// Opens a specialized history section (scheduled, pinned list, comments thread, forum topics,
    /// saved sublists, or channel admin log).
    pub fn open_section(&mut self, section: HistorySectionKind) {
        self.active_section = Some(section);
    }

    /// Returns the currently active specialized section, if any.
    #[must_use]
    pub const fn active_section(&self) -> Option<HistorySectionKind> {
        self.active_section
    }

    /// Closes any active specialized section and returns to the main chat history.
    pub fn close_section(&mut self) {
        self.active_section = None;
    }

    /// Opens the Schedule Message modal box directly (also used by screenshots/tests).
    pub fn open_schedule_box(&mut self, state: ScheduleBoxState) {
        self.schedule_box = Some(state);
    }

    /// Opens the Poll / Quiz creation modal box directly (also used by screenshots/tests).
    pub fn open_create_poll_box(&mut self, state: CreatePollBoxState) {
        self.poll_box = Some(state);
    }

    /// Opens the `TranslateBox` modal directly for `state`.
    pub fn open_translate_box(&mut self, state: TranslateBoxState) {
        self.translate_box = Some(state);
    }

    /// Opens the `ComposeAiBox` modal directly for `state`.
    pub fn open_compose_ai_box(&mut self, state: ComposeAiBoxState) {
        self.compose_ai_box = Some(state);
    }

    /// Returns `true` when `ComposeAiBox` or its nested `AboutCocoonBox` is open.
    #[must_use]
    pub fn is_compose_ai_about_cocoon_open(&self) -> bool {
        self.compose_ai_box
            .as_ref()
            .is_some_and(ComposeAiBoxState::is_about_cocoon_open)
    }

    /// Opens the standalone "About Cocoon" modal directly.
    pub fn open_about_cocoon_box(&mut self) {
        self.about_cocoon_box = Some(AboutCocoonBoxState::new());
    }

    /// Returns `true` when the standalone "About Cocoon" modal is currently open.
    #[must_use]
    pub fn is_about_cocoon_open(&self) -> bool {
        self.about_cocoon_box.is_some()
    }

    /// Dismisses the topmost Cocoon / AI compose / translate-bar popup overlay on Escape.
    pub fn close_cocoon_or_ai_overlay(&mut self) -> bool {
        if self.about_cocoon_box.take().is_some() {
            return true;
        }
        if let Some(ref mut ai) = self.compose_ai_box {
            if ai.about_cocoon.take().is_some() {
                return true;
            }
            if ai.tone_editor.take().is_some() || ai.tone_preview.take().is_some() {
                return true;
            }
            self.compose_ai_box = None;
            return true;
        }
        if self.translate_bar.menu_open {
            self.translate_bar.menu_open = false;
            return true;
        }
        false
    }

    /// Opens the `StickerCreatorBox` modal directly.
    pub fn open_sticker_creator_box(&mut self, state: StickerCreatorBoxState) {
        self.sticker_creator_box = Some(state);
    }

    /// Opens the `SelfDestructionBox` modal directly.
    pub fn open_self_destruct_box(&mut self, state: SelfDestructionBoxState) {
        self.self_destruct_box = Some(state);
    }

    /// Installs an external inline bot query hook for `@bot query` compose results.
    pub fn set_inline_bot_hook(&mut self, hook: InlineBotQueryHook) {
        self.compose.set_inline_bot_hook(hook);
    }

    /// Returns mutable access to the full emoji + `tg-lottie` reactions picker.
    pub fn reactions_picker_mut(&mut self) -> &mut ReactionsPickerState {
        &mut self.reactions_picker
    }

    /// Updates the candidate next unread broadcast channel for `PullToNextChannel`.
    pub fn set_next_unread_channel(
        &mut self,
        next: Option<pull_to_next_channel::NextUnreadChannelHint>,
    ) {
        self.next_unread_channel = next;
    }

    /// Returns mutable access to the unread `@`/heart/poll jump FAB controller.
    pub fn unread_things_mut(&mut self) -> &mut unread_things::HistoryUnreadThingsWidget {
        &mut self.unread_things
    }

    /// Returns mutable access to the broadcast channel pull-to-next-channel controller.
    pub fn pull_next_channel_mut(&mut self) -> &mut pull_to_next_channel::PullToNextChannelWidget {
        &mut self.pull_next_channel
    }

    /// Returns the currently open "Seen by" group read-receipts modal, if any.
    #[must_use]
    pub fn seen_by_modal(&self) -> Option<&read_receipts::SeenByModalWidget> {
        self.seen_by_modal.as_ref()
    }

    /// Closes any open WP-61 engagement overlay (Seen-By modal or unread-things context menu) on Escape.
    pub fn close_engagement_overlays(&mut self) -> bool {
        if self.seen_by_modal.take().is_some() {
            return true;
        }
        self.unread_things.close_menu()
    }

    /// Installs the `chat_helpers` callback invoked when the compose Emoji button is clicked.
    pub fn set_emoji_panel_callback(&mut self, callback: EmojiPanelCallback) {
        self.on_emoji_panel_toggle = Some(callback);
    }

    /// Configures whether `Enter` or `Ctrl+Enter` sends messages.
    pub fn set_send_shortcut(&mut self, shortcut: SendShortcut) {
        self.compose.send_shortcut = shortcut;
    }

    /// Returns the active [`SendShortcut`] setting.
    #[must_use]
    pub const fn send_shortcut(&self) -> SendShortcut {
        self.compose.send_shortcut
    }

    /// Inserts an emoji or sticker text snippet from `chat_helpers` into the active compose field.
    pub fn insert_emoji_from_panel(&mut self, emoji: &str, now_ms: u64) {
        self.compose.insert_text(emoji, now_ms);
    }

    /// Returns a reference to the inner virtualized list widget.
    #[must_use]
    pub const fn inner(&self) -> &HistoryInnerWidget {
        &self.inner
    }

    /// Returns a mutable reference to the inner virtualized list widget.
    pub fn inner_mut(&mut self) -> &mut HistoryInnerWidget {
        &mut self.inner
    }

    /// Returns a reference to the inline media & audio playback runtime.
    #[must_use]
    pub const fn media_runtime(&self) -> &crate::history::inline_media::HistoryMediaRuntime {
        self.inner.media_runtime()
    }

    /// Returns a mutable reference to the inline media & audio playback runtime.
    pub fn media_runtime_mut(&mut self) -> &mut crate::history::inline_media::HistoryMediaRuntime {
        self.inner.media_runtime_mut()
    }

    /// Controls whether `HistoryWidget` paints its own docked top `AudioPlayerBar`.
    ///
    /// `MainWidget` disables this (`false`) so ONE shared `AudioPlayerBar` is rendered across
    /// the history view and all other main-column sections.
    pub fn set_render_internal_audio_bar(&mut self, enabled: bool) {
        self.render_internal_audio_bar = enabled;
    }

    /// Returns a mutable reference to the compose controls (for tests / setup).
    pub fn compose_mut(&mut self) -> &mut ComposeControls {
        &mut self.compose
    }

    /// Applies [`tg_core::settings::CoreSettings`] autoplay, power-saving, sticker loop,
    /// audio speed/volume, and send-shortcut preferences.
    pub fn apply_core_settings(&mut self, settings: &tg_core::settings::CoreSettings) {
        self.inner.apply_core_settings(settings);
        let ps = if settings.power_saving_force_all && !settings.ignore_battery_saving {
            tg_core::settings::PowerSavingFlags::ALL
        } else {
            settings.power_saving
        };
        self.effects_animations_enabled =
            !ps.contains(tg_core::settings::PowerSavingFlags::ANIMATIONS);
        self.effects_chat_enabled = !ps.contains(tg_core::settings::PowerSavingFlags::CHAT_EFFECTS);
        self.effects_reactions_enabled =
            !ps.contains(tg_core::settings::PowerSavingFlags::EMOJI_REACTIONS);
        self.effects_emoji_enabled = !ps.contains(tg_core::settings::PowerSavingFlags::EMOJI_CHAT);
        tg_ui::anim::set_disabled(!self.effects_animations_enabled);
        if !self.thanos_policy().allows_effect() {
            self.thanos_effect.finish_all();
        }
        if !self.fly_policy().allows_text_flight()
            && !self.fly_policy().allows_reaction_fly()
            && !self.fly_policy().allows_emoji_fly()
        {
            self.fly_effects.clear();
        }
        self.compose.send_shortcut = match settings.send_submit_way {
            tg_core::settings::InputSubmitSettings::CtrlEnter => SendShortcut::CtrlEnter,
            tg_core::settings::InputSubmitSettings::Enter
            | tg_core::settings::InputSubmitSettings::Both
            | tg_core::settings::InputSubmitSettings::None => SendShortcut::Enter,
        };
    }

    fn thanos_policy(&self) -> tg_ui::effects::ThanosPolicy {
        tg_ui::effects::ThanosPolicy {
            chat_effects_enabled: self.effects_chat_enabled,
            animations_enabled: self.effects_animations_enabled && !tg_ui::anim::disabled(),
        }
    }

    fn fly_policy(&self) -> tg_ui::effects::FlyEffectPolicy {
        tg_ui::effects::FlyEffectPolicy {
            reaction_fly_enabled: self.effects_reactions_enabled,
            emoji_fly_enabled: self.effects_emoji_enabled,
            text_flight_enabled: self.effects_chat_enabled,
            animations_enabled: self.effects_animations_enabled && !tg_ui::anim::disabled(),
        }
    }

    /// Overrides the Telegram Business quick-reply shortcuts used for `/` compose autocomplete
    /// in private chats (also sets [`Self::set_is_premium`] to `enabled`).
    pub fn set_business_quick_replies(
        &mut self,
        enabled: bool,
        shortcuts: Vec<QuickReplyShortcut>,
    ) {
        self.is_premium = enabled;
        self.compose
            .set_business_quick_replies(enabled, shortcuts.clone());
        self.business_quick_replies_override = Some((enabled, shortcuts));
    }

    /// Sets whether Telegram Premium limits are active (`2048` caption chars / `400` saved GIFs).
    pub fn set_is_premium(&mut self, is_premium: bool) {
        self.is_premium = is_premium;
    }

    /// Returns whether Telegram Premium limits are active.
    #[must_use]
    pub const fn is_premium(&self) -> bool {
        self.is_premium
    }

    /// Sets the tracked number of saved GIFs (used to enforce `PremiumLimitKind::SavedGifs`).
    pub fn set_saved_gifs_count(&mut self, count: usize) {
        self.saved_gifs_count = count;
    }

    /// Returns the tracked number of saved GIFs.
    #[must_use]
    pub const fn saved_gifs_count(&self) -> usize {
        self.saved_gifs_count
    }

    /// Opens the embedded [`PremiumLimitsView`] modal focused on `kind`.
    pub fn open_premium_limits(&mut self, kind: PremiumLimitKind) {
        let mut mock_be = MockBusinessBackend::new();
        let _ = mock_be.set_premium_enabled(self.is_premium);
        if let Ok(mut snap) = mock_be.load_overview() {
            snap.is_premium = self.is_premium;
            self.premium_limits_modal = Some((PremiumLimitsView::new(kind), snap, mock_be));
        }
    }

    /// Returns the [`PremiumLimitKind`] of the currently open limits modal, if any.
    #[must_use]
    pub fn active_premium_limit_modal(&self) -> Option<PremiumLimitKind> {
        self.premium_limits_modal
            .as_ref()
            .map(|(view, _, _)| view.active_kind)
    }

    /// Closes the embedded [`PremiumLimitsView`] modal; returns `true` if one was open.
    pub fn close_premium_limits(&mut self) -> bool {
        self.premium_limits_modal.take().is_some()
    }

    /// Checks whether `caption` fits within the active free/Premium caption length limit.
    ///
    /// Opens [`PremiumLimitsView`] focused on [`PremiumLimitKind::CaptionLength`] and returns
    /// `false` if the limit is exceeded.
    pub fn check_caption_length_limit(&mut self, caption: &str) -> bool {
        let (free_limit, premium_limit) = PremiumLimitsData::default().caption_length;
        let limit = (if self.is_premium {
            premium_limit
        } else {
            free_limit
        }) as usize;
        if caption.trim().chars().count() > limit {
            self.open_premium_limits(PremiumLimitKind::CaptionLength);
            false
        } else {
            true
        }
    }

    /// Attempts to save a GIF message to the user's saved GIFs collection.
    ///
    /// Opens [`PremiumLimitsView`] focused on [`PremiumLimitKind::SavedGifs`] and returns `false`
    /// if the free (`200`) or Premium (`400`) limit has been reached.
    pub fn try_save_gif(&mut self, _msg_id: MsgId, now_secs: f64) -> bool {
        let (free_limit, premium_limit) = PremiumLimitsData::default().saved_gifs;
        let limit = (if self.is_premium {
            premium_limit
        } else {
            free_limit
        }) as usize;
        if self.saved_gifs_count >= limit {
            self.open_premium_limits(PremiumLimitKind::SavedGifs);
            false
        } else {
            self.saved_gifs_count += 1;
            self.show_toast(
                format!("GIF saved ({}/{limit})", self.saved_gifs_count),
                now_secs,
            );
            true
        }
    }

    /// Sends every message in a Telegram Business [`QuickReplyShortcut`] sequentially into
    /// `peer_id` via `backend`, returning the number of messages sent.
    pub fn send_quick_reply_shortcut(
        &mut self,
        peer_id: PeerId,
        peer_kind: PeerKind,
        shortcut: &QuickReplyShortcut,
        backend: &mut dyn HistoryBackend,
    ) -> usize {
        let mut sent_count = 0usize;
        for qr_msg in &shortcut.messages {
            let media = match qr_msg.attachment {
                QuickReplyAttachment::None => None,
                QuickReplyAttachment::Photo => Some(HistoryMedia::Photo(PhotoAttachment {
                    width: 800,
                    height: 600,
                    stripped_thumb: None,
                    fallback_rgb: ([0x4E, 0x90, 0xC8], [0x26, 0x4D, 0x73]),
                    file_name: format!("{}_photo.jpg", shortcut.name),
                    size_bytes: 240_000,
                })),
                QuickReplyAttachment::Document => {
                    Some(HistoryMedia::Document(DocumentAttachment {
                        file_name: format!("{}_attachment.pdf", shortcut.name),
                        size_bytes: 128 * 1024,
                        mime_type: "application/pdf".to_owned(),
                        thumb_rgb: None,
                    }))
                }
                QuickReplyAttachment::Voice => Some(HistoryMedia::Voice(VoiceAttachment {
                    duration_secs: 14,
                    waveform_5bit: vec![8, 16, 24, 18, 28, 22, 14, 10, 20, 26, 16, 12],
                    is_unread: false,
                    transcription: None,
                })),
            };
            let req = SendMessageExtended {
                text: crate::history::text_bridge::parse_compose_markdown(&qr_msg.text),
                media,
                silent: self.compose.silent_mode,
                ..SendMessageExtended::default()
            };
            if let Ok(sent) = backend.send_message_extended(peer_id, req) {
                self.inner.append_message(sent, peer_kind);
                sent_count += 1;
            }
        }
        sent_count
    }

    /// Marks `peer_id` so the message list scrolls to the bottom on next paint.
    pub fn request_scroll_to_bottom(&mut self, peer_id: PeerId) {
        if self.active_peer == peer_id || peer_id != PeerId::ZERO {
            self.inner.request_scroll_to_bottom();
        }
    }

    /// Jumps to `msg_id` in the active chat and activates the jump-to-message highlight animation.
    pub fn jump_to_message(&mut self, msg_id: MsgId, backend: &dyn HistoryBackend, now_ms: i64) {
        let kind = backend
            .peer_info(self.active_peer)
            .map_or(tg_app_core::stand_in::types::PeerKind::User, |p| {
                p.summary.kind
            });
        self.inner.jump_to_message(msg_id, backend, kind, now_ms);
    }

    /// Renders either the empty placeholder or the active peer history + compose controls
    /// using the shell `MainWidget` signature (backed by the rich internal [`MockHistoryStore`]).
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        peer: Option<&PeerSummary>,
        messages: &[MessageItem],
        can_go_back: bool,
        info_open: bool,
        is_chat_wide: bool,
        palette: &Palette,
        strings: &dyn Strings,
    ) -> Option<HistoryAction> {
        let Some(peer) = peer else {
            return self.render_empty_placeholder(ui, palette, strings);
        };
        self.embedded_store.sync_shell_peer(peer, messages);

        // Temporarily swap out embedded store to satisfy borrow checker cleanly with zero cloning.
        let mut store = std::mem::take(&mut self.embedded_store);
        let action = self.show_with_backend(
            ui,
            peer.id,
            &mut store,
            can_go_back,
            info_open,
            is_chat_wide,
            palette,
            strings,
        );
        self.embedded_store = store;
        action
    }

    /// Renders the complete chat history view backed by any [`HistoryBackend`] implementation.
    #[allow(clippy::too_many_arguments)]
    pub fn show_with_backend(
        &mut self,
        ui: &mut Ui,
        peer_id: PeerId,
        backend: &mut dyn HistoryBackend,
        can_go_back: bool,
        info_open: bool,
        is_chat_wide: bool,
        palette: &Palette,
        strings: &dyn Strings,
    ) -> Option<HistoryAction> {
        let Some(peer_info) = backend.peer_info(peer_id) else {
            return self.render_empty_placeholder(ui, palette, strings);
        };

        let colors = HistoryColors::from_palette(palette);
        let now_secs = ui.ctx().input(|i| i.time);
        let now_ms = (now_secs * 1000.0).max(0.0) as i64;
        let now_ms_u64 = now_ms.max(0) as u64;
        let rect = ui.max_rect();

        // Dispatch Channel Direct Messages (monoforum) section if active.
        if let Some((mf_peer, ref mf_title, mf_admin)) = self.monoforum_active.clone()
            && let Some(ex) = backend.as_layer233_extras_mut()
        {
            let now_unix = DEFAULT_SECTION_NOW_UNIX + now_secs.floor() as i32;
            if let Some(ev) = self
                .monoforum_section
                .show(ui, rect, mf_peer, mf_title, mf_admin, now_unix, &colors, ex)
            {
                match ev {
                    monoforum::MonoforumSectionAction::Back => self.monoforum_active = None,
                    monoforum::MonoforumSectionAction::Toast(msg) => {
                        self.show_toast(msg, now_secs);
                    }
                }
            }
            self.paint_status_toast(ui, rect, now_secs);
            return None;
        }

        // Dispatch specialized history sections if one is active for the current peer.
        if let Some(section) = self.active_section
            && section.peer_id() != peer_id
        {
            self.active_section = None;
        }
        if let Some(section) = self.active_section {
            let sec_action = match section {
                HistorySectionKind::Main(_) => {
                    self.active_section = None;
                    None
                }
                HistorySectionKind::Scheduled {
                    peer_id: sp,
                    topic_root_id,
                } => self
                    .scheduled_section
                    .show(ui, rect, sp, topic_root_id, backend, &colors),
                HistorySectionKind::Pinned {
                    peer_id: sp,
                    topic_root_id,
                } => self
                    .pinned_section
                    .show(ui, rect, sp, topic_root_id, backend, &colors),
                HistorySectionKind::Replies {
                    peer_id: sp,
                    root_msg_id,
                } => self
                    .replies_section
                    .show(ui, rect, sp, root_msg_id, backend, &colors),
                HistorySectionKind::ForumTopics { peer_id: sp } => {
                    self.forum_topics.show(ui, rect, sp, backend, &colors)
                }
                HistorySectionKind::ForumTopicHistory {
                    peer_id: sp,
                    topic_root_id,
                } => self
                    .forum_topic_history
                    .show(ui, rect, sp, topic_root_id, backend, &colors),
                HistorySectionKind::SavedSublists { peer_id: sp } => self
                    .saved_sublists
                    .show_list(ui, rect, sp, backend, &colors),
                HistorySectionKind::SavedSublistHistory {
                    peer_id: sp,
                    sublist_peer_id,
                } => self.saved_sublists.show_sublist_history(
                    ui,
                    rect,
                    sp,
                    sublist_peer_id,
                    backend,
                    &colors,
                ),
                HistorySectionKind::AdminLog { peer_id: sp } => {
                    self.admin_log.show(ui, rect, sp, backend, &colors)
                }
            };
            if let Some(sa) = sec_action {
                match sa {
                    HistorySectionAction::Back => self.active_section = None,
                    HistorySectionAction::OpenSection(next) => self.active_section = Some(next),
                    HistorySectionAction::JumpToMessage {
                        peer_id: jp,
                        msg_id,
                    } => {
                        self.active_section = None;
                        self.active_peer = jp;
                        let kind = backend
                            .peer_info(jp)
                            .map_or(PeerKind::Group, |p| p.summary.kind);
                        self.inner.jump_to_message(msg_id, backend, kind, now_ms);
                    }
                    HistorySectionAction::Toast(msg) => self.show_toast(msg, now_secs),
                }
            }
            self.paint_status_toast(ui, rect, now_secs);
            return None;
        }

        // Handle peer switch: flush previous thread rich draft (with entities), load new peer's slice & draft.
        // Port of `HistoryWidget::showHistory` + `updateForwarding`: a forward bar belongs to the
        // chat it was set in (`_history->setForwardDraft`), so it stays with that chat and comes
        // back when the chat reopens; it never moves to the chat opened next.
        if self.active_peer != peer_id {
            if !self.active_peer.is_zero() {
                self.inner
                    .finalize_read_metrics_for_backend(backend, now_ms_u64);
                let _ = backend.flush_read_metrics(false);
            }
            let mut pending_forward = match &self.compose.mode {
                crate::history::compose::ComposeMode::Forward { .. } => {
                    Some(self.compose.mode.clone())
                }
                _ => None,
            };
            if !self.active_peer.is_zero()
                && let Some(fwd) = pending_forward.take()
            {
                self.forward_drafts.insert(self.active_peer, fwd);
            }
            if let Some(fwd) = self.forward_drafts.remove(&peer_id) {
                pending_forward = Some(fwd);
            }
            if !self.active_peer.is_zero()
                && !matches!(
                    self.compose.mode,
                    crate::history::compose::ComposeMode::Edit { .. }
                        | crate::history::compose::ComposeMode::Forward { .. }
                )
            {
                backend.save_rich_draft(
                    ThreadKey::Main(self.active_peer),
                    self.compose.current_rich_draft(),
                );
            }
            self.active_peer = peer_id;
            self.pinned_bar_index = 0;
            self.search_open = false;
            self.search_query.clear();
            self.translate_bar.reset_for_peer(peer_id);
            self.who_reacted_modal = None;
            self.seen_by_modal = None;
            self.unread_things.close_menu();
            self.pull_next_channel.reset();
            self.poll_voters_modal = None;
            self.compose.send_as.selected = backend.default_send_as_peer(peer_id);
            self.compose.send_as.open = false;
            self.compose
                .apply_rich_draft(backend.load_rich_draft(ThreadKey::Main(peer_id)));
            if let Some(fwd) = pending_forward {
                self.compose.mode = fwd;
                self.compose.focus_requested = true;
            }
            self.inner
                .load_peer(peer_id, MsgId::SHOW_AT_END, backend, peer_info.summary.kind);
            // Messages already in history on open never replay their effects; when the slice
            // arrives later, the first frame that sees it seeds the watermark instead.
            match self.inner.messages().iter().map(|m| m.id).max() {
                Some(loaded_max) => self.effect_watermark.insert(peer_id, loaded_max),
                None => self.effect_watermark.remove(&peer_id),
            };
            self.reset_bot_state_for_peer(peer_id, backend);
            self.stash_menu_open = false;
            self.last_compose_text_for_hint = self.compose.text.clone();
            self.stash_hint
                .track_change(&self.compose.text, false, true, now_ms_u64);
        } else {
            // Pull live MTProto updates (incoming/outgoing messages, edits, deletions,
            // read receipts) into the already-open history view (`History::addNewMessage` /
            // `HistoryWidget::newItemAdded`, `itemEdited`, `itemRemoved`).
            self.sync_live_history_updates(peer_id, &peer_info, backend, ui.ctx());
        }

        // WP-38: Sync active streamed bot drafts, poll interactive emoji sync, and flush due read metrics.
        let streamed_drafts = backend.streamed_draft_messages(peer_id, None);
        self.inner
            .sync_streamed_drafts(streamed_drafts, peer_info.summary.kind);
        for incoming in backend.drain_incoming_emoji_interactions(peer_id) {
            let server_variants = backend.emoji_interaction_variant_count(&incoming.emoticon);
            let plays = self.emoji_coordinator.start_incoming_with_variants(
                incoming.peer_id,
                incoming.msg_id,
                &incoming.emoticon,
                &incoming.bunch,
                server_variants,
                now_ms_u64,
            );
            for play in plays {
                let lottie =
                    backend.resolve_emoji_interaction_lottie(&play.emoticon, play.variant_index);
                self.inner.play_emoji_interaction_with_lottie(
                    play.msg_id,
                    play.emoticon.clone(),
                    None,
                    false,
                    play.variant_index,
                    lottie,
                    now_ms_u64,
                );
                if play.incoming
                    && self.emoji_coordinator.should_send_play_started(
                        play.peer_id,
                        &play.emoticon,
                        now_ms_u64,
                    )
                {
                    let _ = backend.send_emoji_interaction_seen(play.peer_id, &play.emoticon);
                }
            }
        }
        for scheduled in self.emoji_coordinator.check_animations(now_ms_u64) {
            let lottie = backend
                .resolve_emoji_interaction_lottie(&scheduled.emoticon, scheduled.variant_index);
            self.inner.play_emoji_interaction_with_lottie(
                scheduled.msg_id,
                scheduled.emoticon.clone(),
                None,
                !scheduled.incoming,
                scheduled.variant_index,
                lottie,
                now_ms_u64,
            );
            if scheduled.incoming
                && self.emoji_coordinator.should_send_play_started(
                    scheduled.peer_id,
                    &scheduled.emoticon,
                    now_ms_u64,
                )
            {
                let _ = backend.send_emoji_interaction_seen(scheduled.peer_id, &scheduled.emoticon);
            }
        }
        for batch in self.emoji_coordinator.poll_outgoing_batches(now_ms_u64) {
            let _ = backend.send_emoji_interaction(
                batch.peer_id,
                batch.msg_id,
                &batch.emoticon,
                &batch.bunch,
            );
        }
        for seen_emoticon in backend.drain_emoji_interaction_seen(peer_id) {
            if self
                .emoji_coordinator
                .accept_seen_outgoing(peer_id, &seen_emoticon, now_ms_u64)
            {
                self.show_toast(
                    format!(
                        "{} is watching your {} animation",
                        peer_info.summary.title, seen_emoticon
                    ),
                    now_secs,
                );
            }
        }
        let _ = backend.flush_read_metrics(false);

        // Apply any queued `switch_inline` compose prefill targeting `peer_id` after draft restore.
        if self
            .pending_inline_prefill
            .as_ref()
            .is_some_and(|p| p.target_peer == peer_id)
            && let Some(prefill) = self.pending_inline_prefill.take()
        {
            let composed = format!("@{} {}", prefill.bot_username, prefill.query);
            self.compose.text = composed.clone();
            self.compose
                .rich_edit
                .set_text_with_entities(tg_text::TextWithEntities::simple(composed.clone()));
            self.compose.focus_requested = true;
            self.last_inline_compose_text = composed;
            if let Some(bots_be) = backend.as_bots_backend_mut() {
                let _ = self.inline_results.fetch_immediately(
                    tg_bots::stand_in::UserId(prefill.bot_id.bare()),
                    prefill.bot_username,
                    to_bot_peer(peer_id),
                    prefill.query,
                    now_ms_u64,
                    bots_be,
                );
            }
        }

        if self
            .pending_forward_draft
            .as_ref()
            .is_some_and(|d| d.target_peer == peer_id)
            && let Some(draft) = self.pending_forward_draft.take()
        {
            self.compose
                .start_forward(draft.from_peer, draft.msg_ids, draft.summary);
        }

        let mut action = None;

        // Tick pending simulated async inline-button callbacks so spinners complete cleanly.
        if let Some(cb_act) = self.tick_pending_bot_callbacks(backend, now_secs, now_ms_u64) {
            action = Some(cb_act);
        }
        if !self.pending_callbacks.is_empty() {
            ui.ctx().request_repaint();
        }

        // Snapshot bot metadata for the active peer (`BotsBackend`).
        let bot_peer_id = to_bot_peer(peer_id);
        let bot_snapshot = backend
            .as_bots_backend()
            .and_then(|b| b.load_bot_chat_for_peer(bot_peer_id));
        let bot_commands = backend
            .as_bots_backend()
            .map(|b| b.commands_for_peer(bot_peer_id))
            .unwrap_or_default();
        let has_bots_backend = backend.as_bots_backend().is_some();
        let bot_peer_kind = to_bot_peer_kind(peer_info.summary.kind, bot_snapshot.is_some());

        // 1. Top bar (or multi-select top bar when messages are selected).
        let top_rect =
            Rect::from_min_size(rect.min, Vec2::new(rect.width(), HISTORY_TOP_BAR_HEIGHT));
        let top_event = if self.inner.selected_count() > 0 {
            paint_selection_top_bar(ui, top_rect, self.inner.selected_count(), &colors)
        } else {
            paint_top_bar(
                ui,
                top_rect,
                &peer_info,
                can_go_back,
                info_open,
                self.search_open,
                &colors,
            )
        };
        if let Some(ev) = top_event {
            match ev {
                TopBarEvent::Back => action = Some(HistoryAction::Back),
                TopBarEvent::ToggleInfo => action = Some(HistoryAction::TogglePeerInfo(peer_id)),
                TopBarEvent::ToggleSearch => {
                    self.search_open = !self.search_open;
                    if !self.search_open {
                        self.search_query.clear();
                    }
                }
                TopBarEvent::ToggleMute => {
                    let muted = backend.toggle_mute(peer_id);
                    self.show_toast(
                        if muted {
                            "Notifications muted"
                        } else {
                            "Notifications unmuted"
                        },
                        now_secs,
                    );
                }
                TopBarEvent::OpenDirectMessages => {
                    self.open_monoforum_section(
                        peer_id,
                        peer_info.summary.title.clone(),
                        peer_info.summary.can_send,
                    );
                }
                TopBarEvent::ToggleNoForwards => {
                    let next_enabled = !backend.peer_no_forwards_enabled(peer_id);
                    match backend.toggle_peer_no_forwards(peer_id, next_enabled, None) {
                        Ok(()) => {
                            self.show_toast(
                                if next_enabled {
                                    "Sharing disabled in this chat"
                                } else {
                                    "Requested to enable sharing in this chat"
                                },
                                now_secs,
                            );
                            self.reload_active_peer(backend);
                        }
                        Err(err) => self.show_toast(err, now_secs),
                    }
                }
                TopBarEvent::ForwardSelected => {
                    let ids = self.inner.selected_msg_ids();
                    let count = ids.len();
                    if count > 0 {
                        let summary = if count == 1 {
                            format!("1 message from {}", peer_info.summary.title)
                        } else {
                            format!("{count} messages from {}", peer_info.summary.title)
                        };
                        self.compose.start_forward(peer_id, ids, summary);
                        self.inner.clear_selection();
                    }
                }
                TopBarEvent::CopySelected => {
                    let copied = self.inner.extract_selected_text();
                    if !copied.is_empty() {
                        ui.ctx().copy_text(copied);
                        self.show_toast("Copied selected messages", now_secs);
                    }
                    self.inner.clear_selection();
                }
                TopBarEvent::DeleteSelected => {
                    let ids = self.inner.selected_msg_ids();
                    if !ids.is_empty() {
                        let can_revoke = !peer_info.summary.is_saved_messages;
                        self.delete_box = Some(DeleteConfirmBoxState::new(
                            ids,
                            peer_info.summary.title.clone(),
                            can_revoke,
                        ));
                    }
                }
                TopBarEvent::CancelSelection => {
                    self.inner.clear_selection();
                }
            }
        }

        let mut list_top = top_rect.bottom();

        // 1b. Docked top AudioPlayerBar (standalone mode only; MainWidget renders one shared bar).
        if self.render_internal_audio_bar
            && self.inner.media_runtime().audio_player_bar().is_visible()
        {
            let player_rect = Rect::from_min_size(
                Pos2::new(rect.left(), list_top),
                Vec2::new(rect.width(), MEDIA_PLAYER_HEIGHT),
            );
            let mut bar_ev = None;
            ui.scope_builder(egui::UiBuilder::new().max_rect(player_rect), |ui| {
                bar_ev = self
                    .inner
                    .media_runtime_mut()
                    .show_top_audio_bar(ui, palette, now_ms);
            });
            if let Some(AudioPlayerBarEvent::ShowItem(full_id)) = bar_ev {
                let target_msg = MsgId(full_id.msg.0 as i32);
                self.inner
                    .jump_to_message(target_msg, backend, peer_info.summary.kind, now_ms);
            }
            list_top = player_rect.bottom();
        }

        // 2. Optional inline chat search bar (`Ctrl+F` or Search button) + Calendar jump button.
        if self.search_open {
            let search_rect = Rect::from_min_size(
                Pos2::new(rect.left(), list_top),
                Vec2::new(rect.width(), 40.0),
            );
            ui.painter()
                .rect_filled(search_rect, CornerRadius::ZERO, colors.bar_bg);
            ui.scope_builder(
                egui::UiBuilder::new().max_rect(search_rect.shrink2(Vec2::new(14.0, 6.0))),
                |ui| {
                    ui.horizontal(|ui| {
                        let avail_w = (ui.available_width() - 96.0).max(120.0);
                        ui.add(
                            TextEdit::singleline(&mut self.search_query)
                                .desired_width(avail_w)
                                .hint_text("Search messages in this chat..."),
                        );
                        if ui
                            .button(
                                RichText::new("📅 Date")
                                    .size(12.0)
                                    .color(colors.active_text_fg),
                            )
                            .on_hover_text("Jump to date")
                            .clicked()
                        {
                            let now_unix = backend
                                .current_unix_time()
                                .unwrap_or(i64::from(DEFAULT_SECTION_NOW_UNIX))
                                as i32;
                            self.calendar_box = Some(CalendarBoxState::new(
                                peer_id,
                                peer_info.summary.title.clone(),
                                now_unix,
                            ));
                        }
                    });
                },
            );
            ui.painter().line_segment(
                [
                    Pos2::new(search_rect.left(), search_rect.bottom()),
                    Pos2::new(search_rect.right(), search_rect.bottom()),
                ],
                Stroke::new(1.0, colors.divider),
            );
            list_top = search_rect.bottom();
        }

        // 3. Pinned messages bar (`history_view_pinned_bar.cpp`).
        let pinned = backend.pinned_messages(peer_id);
        if !pinned.is_empty() {
            let pin_rect = Rect::from_min_size(
                Pos2::new(rect.left(), list_top),
                Vec2::new(rect.width(), PINNED_BAR_HEIGHT),
            );
            if let Some(pev) = paint_pinned_bar(
                ui,
                pin_rect,
                &pinned,
                self.pinned_bar_index,
                peer_info.can_pin,
                &colors,
            ) {
                match pev {
                    PinnedBarEvent::JumpAndCycle(target_id) => {
                        self.inner.jump_to_message(
                            target_id,
                            backend,
                            peer_info.summary.kind,
                            now_ms,
                        );
                        self.pinned_bar_index = (self.pinned_bar_index + 1) % pinned.len();
                    }
                    PinnedBarEvent::Unpin(target_id) => {
                        let _ = backend.pin_message(peer_id, target_id, true);
                        if let Some(updated) = self
                            .inner
                            .messages()
                            .iter()
                            .find(|m| m.id == target_id)
                            .cloned()
                        {
                            let mut copy = updated;
                            copy.is_pinned = false;
                            self.inner.update_message_in_place(copy);
                        }
                        self.pinned_bar_index = 0;
                        self.show_toast("Message unpinned", now_secs);
                    }
                }
            }
            list_top = pin_rect.bottom();
        }

        // 4. Optional Contact Status bar (`history_view_contact_status.cpp`, WP-66 / WP-70).
        if let Some(ref banner) = peer_info.contact_status {
            let cs_rect = Rect::from_min_size(
                Pos2::new(rect.left(), list_top),
                Vec2::new(rect.width(), CONTACT_STATUS_HEIGHT),
            );
            let confirming_spam = self.contact_status_report_confirm_peer == Some(peer_id);
            if let Some(cev) =
                paint_contact_status_bar_with_confirm(ui, cs_rect, banner, confirming_spam, &colors)
            {
                match cev {
                    ContactStatusEvent::AddContact => {
                        self.contact_status_report_confirm_peer = None;
                        match backend.accept_contact(peer_id) {
                            Ok(()) => {
                                self.show_toast(
                                    tg_lang::tr::lng_new_contact_add_done(&peer_info.summary.title),
                                    now_secs,
                                );
                            }
                            Err(_) => {
                                backend.add_contact(peer_id);
                                self.show_toast(
                                    tg_lang::tr::lng_new_contact_add_done(&peer_info.summary.title),
                                    now_secs,
                                );
                            }
                        }
                    }
                    ContactStatusEvent::SharePhone => {
                        self.contact_status_report_confirm_peer = None;
                        match backend.accept_contact(peer_id) {
                            Ok(()) => {
                                self.show_toast(
                                    tg_lang::tr::lng_new_contact_share_done(
                                        &peer_info.summary.title,
                                    ),
                                    now_secs,
                                );
                            }
                            Err(err) => self.show_toast(err, now_secs),
                        }
                    }
                    ContactStatusEvent::ToggleBlock => {
                        self.contact_status_report_confirm_peer = None;
                        backend.toggle_block_peer(peer_id);
                        self.show_toast("Updated block status", now_secs);
                    }
                    ContactStatusEvent::ReportSpam => {
                        self.contact_status_report_confirm_peer = Some(peer_id);
                    }
                    ContactStatusEvent::ConfirmReportSpam => {
                        self.contact_status_report_confirm_peer = None;
                        match backend.report_spam(peer_id) {
                            Ok(_) => {
                                backend.dismiss_contact_status(peer_id);
                                self.show_toast(tg_lang::tr::lng_report_spam_done(), now_secs);
                            }
                            Err(err) => self.show_toast(err, now_secs),
                        }
                    }
                    ContactStatusEvent::CancelReportSpam => {
                        self.contact_status_report_confirm_peer = None;
                    }
                    ContactStatusEvent::Dismiss => {
                        self.contact_status_report_confirm_peer = None;
                        let _ = backend.hide_peer_settings_bar(peer_id);
                        backend.dismiss_contact_status(peer_id);
                    }
                }
            }
            list_top = cs_rect.bottom();
        } else if self.contact_status_report_confirm_peer == Some(peer_id) {
            self.contact_status_report_confirm_peer = None;
        }

        // 4b. Whole-chat Translate Bar (`history_view_translate_bar.cpp`).
        let detected_lang = detect_chat_language(self.inner.messages());
        let peer_tr_disabled = backend.peer_translations_disabled(peer_id);
        let do_not_translate = backend.do_not_translate_languages();
        let target_lang = backend.target_translation_language();
        if self
            .translate_bar
            .should_show(detected_lang, peer_tr_disabled, &do_not_translate)
        {
            let tr_rect = Rect::from_min_size(
                Pos2::new(rect.left(), list_top),
                Vec2::new(rect.width(), TRANSLATE_BAR_HEIGHT),
            );
            if let Some(tr_act) = self.translate_bar.show(
                ui,
                tr_rect,
                detected_lang,
                &target_lang,
                &do_not_translate,
                &colors,
            ) {
                match tr_act {
                    TranslateBarAction::TranslateChat(lang) => {
                        let msg_ids: Vec<MsgId> = self
                            .inner
                            .messages()
                            .iter()
                            .filter(|m| !m.is_service() && !m.text.text.trim().is_empty())
                            .map(|m| m.id)
                            .collect();
                        match backend.translate_messages_batch(peer_id, &msg_ids, &lang) {
                            Ok(pairs) => {
                                for (mid, tr_text) in pairs {
                                    if let Some(mut copy) = self.find_loaded_message(mid).cloned() {
                                        copy.extra.translated_text = Some((lang.clone(), tr_text));
                                        self.inner.update_message_in_place(copy);
                                    }
                                }
                            }
                            Err(err) => {
                                self.translate_bar.translated = false;
                                self.show_toast(err, now_secs);
                            }
                        }
                    }
                    TranslateBarAction::ShowOriginal => {
                        let ids: Vec<MsgId> = self
                            .inner
                            .messages()
                            .iter()
                            .filter(|m| m.extra.translated_text.is_some())
                            .map(|m| m.id)
                            .collect();
                        for mid in ids {
                            backend.clear_message_translation(peer_id, mid);
                            if let Some(mut copy) = self.find_loaded_message(mid).cloned() {
                                copy.extra.translated_text = None;
                                self.inner.update_message_in_place(copy);
                            }
                        }
                    }
                    TranslateBarAction::HideForPeer => {
                        if let Err(err) = backend.toggle_peer_translations(peer_id, true) {
                            self.show_toast(err, now_secs);
                        } else {
                            self.show_toast("Translation hidden for this chat", now_secs);
                        }
                    }
                    TranslateBarAction::AddDoNotTranslate(code) => {
                        let mut updated = do_not_translate.clone();
                        if !updated.iter().any(|l| l.eq_ignore_ascii_case(&code)) {
                            updated.push(code.clone());
                        }
                        backend.set_do_not_translate_languages(updated);
                        self.show_toast(
                            format!(
                                "Added {} to Do not translate",
                                crate::history::compose_extras::translation_language_name(&code)
                            ),
                            now_secs,
                        );
                    }
                    TranslateBarAction::RemoveDoNotTranslate(code) => {
                        let updated: Vec<String> = do_not_translate
                            .iter()
                            .filter(|l| !l.eq_ignore_ascii_case(&code))
                            .cloned()
                            .collect();
                        backend.set_do_not_translate_languages(updated);
                    }
                    TranslateBarAction::AboutCocoon => {
                        self.about_cocoon_box = Some(AboutCocoonBoxState::new());
                    }
                    TranslateBarAction::ChangeTargetLanguage(lang) => {
                        backend.set_target_translation_language(lang.clone());
                        if self.translate_bar.translated {
                            let msg_ids: Vec<MsgId> = self
                                .inner
                                .messages()
                                .iter()
                                .filter(|m| !m.is_service() && !m.text.text.trim().is_empty())
                                .map(|m| m.id)
                                .collect();
                            if let Ok(pairs) =
                                backend.translate_messages_batch(peer_id, &msg_ids, &lang)
                            {
                                for (mid, tr_text) in pairs {
                                    if let Some(mut copy) = self.find_loaded_message(mid).cloned() {
                                        copy.extra.translated_text = Some((lang.clone(), tr_text));
                                        self.inner.update_message_in_place(copy);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            list_top = tr_rect.bottom();
        }

        // 4b. Channel/bot Sponsored Message banner (`data/components/sponsored_messages.cpp`).
        if matches!(peer_info.summary.kind, PeerKind::Channel)
            && let Some(ex) = backend.as_layer233_extras_mut()
            && !ex.is_sponsored_hidden()
            && let Ok(ads) = ex.get_sponsored_messages(peer_id, None)
            && let Some(ad) = ads.first().cloned()
        {
            if !ad.viewed {
                let _ = ex.view_sponsored_message(&ad.random_id);
            }
            let ad_rect = Rect::from_min_size(
                Pos2::new(rect.left(), list_top),
                Vec2::new(rect.width(), sponsored::SPONSORED_BANNER_HEIGHT),
            );
            if let Some(sev) =
                sponsored::paint_sponsored_banner(ui, ad_rect, &ad, self.is_premium, &colors)
            {
                match sev {
                    sponsored::SponsoredBannerAction::Click {
                        random_id,
                        url,
                        is_media,
                        is_fullscreen,
                    } => {
                        let _ = ex.click_sponsored_message(&random_id, is_media, is_fullscreen);
                        action = Some(HistoryAction::OpenLink(url));
                    }
                    sponsored::SponsoredBannerAction::OpenAbout => {
                        self.about_sponsored_box =
                            Some(sponsored::AboutSponsoredBoxState::new(self.is_premium));
                    }
                    sponsored::SponsoredBannerAction::OpenReport(random_id) => {
                        if let Ok(outcome) = ex.report_sponsored_message(&random_id, &[]) {
                            self.report_sponsored_box =
                                Some(sponsored::ReportSponsoredBoxState::from_outcome(
                                    random_id, outcome,
                                ));
                        }
                    }
                    sponsored::SponsoredBannerAction::HideAds => {
                        if ex.set_sponsored_hidden(true).is_ok() {
                            self.show_toast("Sponsored messages hidden", now_secs);
                        }
                    }
                    sponsored::SponsoredBannerAction::Dismiss(random_id) => {
                        let _ = ex.report_sponsored_message(&random_id, b"-1");
                    }
                }
            }
            list_top = ad_rect.bottom();
        }
        // 4b. Optional Join Requests bar (`ui/chat/requests_bar.cpp`).
        if let Some(ref jr_summary) = backend.join_requests_bar(peer_id)
            && jr_summary.count > 0
        {
            let jr_rect = Rect::from_min_size(
                Pos2::new(rect.left(), list_top),
                Vec2::new(rect.width(), JOIN_REQUESTS_BAR_HEIGHT),
            );
            if paint_join_requests_bar(ui, jr_rect, jr_summary, &colors) {
                self.join_requests_box = Some(JoinRequestsBoxState::new(
                    peer_id,
                    matches!(peer_info.summary.kind, PeerKind::Group),
                ));
            }
            list_top = jr_rect.bottom();
        }
        // 4c. Optional Telegram Business chatbot pause/resume bar (`BusinessBotStatus`).
        if let Some(ref biz_bot) = backend.business_bot_bar(peer_id) {
            let biz_rect = Rect::from_min_size(
                Pos2::new(rect.left(), list_top),
                Vec2::new(rect.width(), BUSINESS_BOT_BAR_HEIGHT),
            );
            if let Some(ev) = paint_business_bot_bar(ui, biz_rect, biz_bot, &colors) {
                match ev {
                    BusinessBotBarEvent::Pause => {
                        match backend.toggle_business_bot_paused(peer_id, true) {
                            Ok(()) => {
                                self.show_toast(format!("Paused {}", biz_bot.bot_name), now_secs);
                            }
                            Err(err) => self.show_toast(err, now_secs),
                        }
                    }
                    BusinessBotBarEvent::Resume => {
                        match backend.toggle_business_bot_paused(peer_id, false) {
                            Ok(()) => {
                                self.show_toast(format!("Resumed {}", biz_bot.bot_name), now_secs);
                            }
                            Err(err) => self.show_toast(err, now_secs),
                        }
                    }
                    BusinessBotBarEvent::Remove => match backend.disable_business_bot(peer_id) {
                        Ok(()) => {
                            self.show_toast(format!("Disconnected {}", biz_bot.bot_name), now_secs);
                        }
                        Err(err) => self.show_toast(err, now_secs),
                    },
                    BusinessBotBarEvent::Manage(url) => {
                        action = Some(HistoryAction::OpenLink(url));
                    }
                }
            }
            list_top = biz_rect.bottom();
        }

        // 4d. Subsection tabs slider (`history_view_subsection_tabs.cpp`) for forums & Saved Messages.
        let mut content_bounds = Rect::from_min_max(
            Pos2::new(rect.left(), list_top),
            Pos2::new(rect.right(), rect.bottom()),
        );
        // Saved Messages has no subsection tabs: `SubsectionTabs::UsedFor` is only true for
        // monoforum admins and forum channels / bots (`PeerData::displaySubsectionTabs`).
        if matches!(peer_info.summary.kind, PeerKind::Group) {
            let topics = backend.forum_topics(peer_id);
            if !topics.is_empty() {
                let (remaining, tab_act) = self.subsection_tabs.show_for_forum(
                    ui,
                    content_bounds,
                    peer_id,
                    None,
                    &topics,
                    backend,
                    &colors,
                );
                content_bounds = remaining;
                if let Some(sa) = tab_act {
                    match sa {
                        HistorySectionAction::OpenSection(next) => self.active_section = Some(next),
                        HistorySectionAction::Toast(msg) => self.show_toast(msg, now_secs),
                        HistorySectionAction::Back | HistorySectionAction::JumpToMessage { .. } => {
                        }
                    }
                }
            }
        }

        // 5a. Bot chats with a pending `start` token or no history show the START bar in place of
        //     the composer and its keyboard (`HistoryWidget::isBotStart`).
        // Live sessions return a snapshot for every peer; only real bots get the bar.
        let is_bot_chat = bot_snapshot
            .as_ref()
            .is_some_and(|s| s.peer_kind == tg_bots::stand_in::PeerKind::Bot);
        let bot_start = peer_info.summary.can_send
            && is_bot_chat
            && (self.bot_start_tokens.contains_key(&peer_id) || self.inner.messages().is_empty());
        self.bot_start_shown = bot_start;

        // 5. Compute bottom reply keyboard & compose controls heights & rects.
        let reply_kb_h = if peer_info.summary.can_send && !bot_start {
            self.reply_keyboard
                .desired_height(ui, content_bounds.width())
        } else {
            0.0
        };
        let reply_kb_rect = Rect::from_min_size(
            Pos2::new(content_bounds.left(), content_bounds.bottom() - reply_kb_h),
            Vec2::new(content_bounds.width(), reply_kb_h),
        );
        let compose_h = if bot_start {
            BOT_START_BAR_HEIGHT
        } else {
            self.compose
                .desired_height(content_bounds.width(), peer_info.summary.can_send)
        };
        let compose_rect = Rect::from_min_size(
            Pos2::new(content_bounds.left(), reply_kb_rect.top() - compose_h),
            Vec2::new(content_bounds.width(), compose_h),
        );
        self.last_compose_rect = compose_rect;

        // 5b. Active outgoing media uploads progress bar docked directly above the compose controls.
        let pending_uploads: Vec<_> = backend
            .pending_media_uploads(peer_id)
            .into_iter()
            .filter(|u| !u.cancelled)
            .collect();
        let shown_uploads_count = pending_uploads.len().min(3);
        let uploads_h = shown_uploads_count as f32 * UPLOAD_PROGRESS_ROW_HEIGHT;
        let uploads_rect = Rect::from_min_size(
            Pos2::new(content_bounds.left(), compose_rect.top() - uploads_h),
            Vec2::new(content_bounds.width(), uploads_h),
        );
        if shown_uploads_count > 0 {
            if let Some(cancel_id) = paint_upload_progress_bar(
                ui,
                uploads_rect,
                &pending_uploads[..shown_uploads_count],
                &colors,
            ) && backend.cancel_media_upload(cancel_id)
            {
                self.show_toast("Upload cancelled", now_secs);
            }
            if pending_uploads.iter().any(|u| !u.failed) {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(120));
            }
        }

        // 5c. Optional "Stop generating" bar above the uploads bar when a stoppable streamed bot draft is active.
        let has_stoppable_draft = backend.has_stoppable_streamed_draft(peer_id, None);
        let stop_draft_bar_h = if has_stoppable_draft {
            streamed_drafts::STOP_STREAMED_DRAFT_BAR_HEIGHT
        } else {
            0.0
        };
        let stop_draft_rect = Rect::from_min_size(
            Pos2::new(content_bounds.left(), uploads_rect.top() - stop_draft_bar_h),
            Vec2::new(content_bounds.width(), stop_draft_bar_h),
        );
        if has_stoppable_draft
            && streamed_drafts::paint_stop_streamed_draft_bar(ui, stop_draft_rect, &colors)
        {
            match backend.stop_streamed_draft(peer_id, None) {
                Ok(true) => {
                    self.show_toast("Stopped bot response", now_secs);
                }
                Ok(false) => {}
                Err(err) => self.show_toast(err, now_secs),
            }
        }

        // 5d. Broadcast channel Pull-to-Next-Unread-Channel footer strip (`history_view_pull_to_next_channel.cpp`).
        // tdesktop shows it only while the list is over-scrolled past its bottom, drawn over the
        // list; at rest nothing sits between the messages and the bottom bar.
        let is_broadcast_channel = peer_info.summary.kind == PeerKind::Channel;
        let show_pull_footer = is_broadcast_channel
            && !self.inner.show_scroll_down_button()
            && self.pull_next_channel.pull_px() > 0.0;
        let pull_footer_h = if show_pull_footer {
            pull_to_next_channel::PULL_NEXT_FOOTER_HEIGHT
        } else {
            0.0
        };
        let pull_footer_rect = Rect::from_min_size(
            Pos2::new(content_bounds.left(), stop_draft_rect.top() - pull_footer_h),
            Vec2::new(content_bounds.width(), pull_footer_h),
        );

        // 6. Scrollable virtualized message list between header bars and the stop/upload/compose controls.
        let list_rect = Rect::from_min_max(
            content_bounds.min,
            Pos2::new(
                content_bounds.right(),
                stop_draft_rect.top().max(content_bounds.top() + 40.0),
            ),
        );
        let inner_events = self.inner.show_with_bots(
            ui,
            list_rect,
            &peer_info,
            is_chat_wide,
            &self.search_query,
            &colors,
            palette,
            bot_snapshot.as_ref().map(|s| s.bot_id),
            bot_snapshot.as_ref().is_some_and(|s| s.bot_verified),
            backend,
        );
        if let Some(inner_act) = self.handle_inner_events(
            ui.ctx(),
            inner_events,
            peer_id,
            &peer_info,
            bot_snapshot.as_ref(),
            backend,
            now_secs,
            now_ms_u64,
        ) {
            action = Some(inner_act);
        }

        // 6b. Empty bot chat info header ("What can this bot do?" + START button).
        if self.inner.messages().is_empty()
            && is_bot_chat
            && let Some(ref snap) = bot_snapshot
        {
            let info_ev = ui
                .scope_builder(egui::UiBuilder::new().max_rect(list_rect), |ui| {
                    ui.add_space(((list_rect.height() - 360.0) * 0.5).max(16.0));
                    show_bot_info_header(
                        ui,
                        &snap.bot_name,
                        &snap.bot_username,
                        &snap.bot_info,
                        palette,
                    )
                })
                .inner;
            if let Some(ev) = info_ev {
                match ev {
                    BotInfoHeaderEvent::StartBot => {
                        let req = SendMessageRequest {
                            text: tg_text::TextWithEntities::simple("/start"),
                            reply_to: None,
                            silent: false,
                        };
                        if backend.send_message(peer_id, req).is_ok() {
                            self.reload_active_peer(backend);
                            self.sync_reply_keyboard_from_loaded(peer_id, backend);
                            self.show_toast(format!("Sent /start to {}", snap.bot_name), now_secs);
                            action = Some(HistoryAction::SendMessage {
                                peer_id,
                                text: "/start".to_owned(),
                            });
                        }
                    }
                    BotInfoHeaderEvent::OpenPrivacyPolicy(act) => match act {
                        PrivacyPolicyAction::OpenUrl(url) => {
                            self.show_toast(format!("Opening privacy policy: {url}"), now_secs);
                            action = Some(HistoryAction::OpenLink(url));
                        }
                        PrivacyPolicyAction::SendPrivacyCommand(cmd) => {
                            let req = SendMessageRequest {
                                text: tg_text::TextWithEntities::simple(cmd.clone()),
                                reply_to: None,
                                silent: false,
                            };
                            if let Ok(sent) = backend.send_message(peer_id, req) {
                                self.inner.append_message(sent, peer_info.summary.kind);
                            }
                            self.show_toast(format!("Sent {cmd}"), now_secs);
                        }
                        PrivacyPolicyAction::OpenDefaultTelegramPolicy => {
                            self.show_toast("Opening Telegram Bot Privacy Policy", now_secs);
                            action = Some(HistoryAction::OpenLink(
                                "https://telegram.org/privacy-tpa".to_owned(),
                            ));
                        }
                    },
                    BotInfoHeaderEvent::OpenMainApp => {
                        self.show_toast(
                            format!("Launching {} Mini App...", snap.bot_name),
                            now_secs,
                        );
                        action = Some(HistoryAction::OpenMiniApp {
                            bot_id: PeerId::from_user(snap.bot_id.0),
                            url: None,
                            start_param: None,
                            confirm_open: false,
                        });
                    }
                }
            }
        }

        // 7. Floating scroll-to-bottom button with unread badge.
        let show_scroll_btn = self.inner.show_scroll_down_button();
        if show_scroll_btn
            && paint_scroll_to_bottom_button(ui, list_rect, self.inner.unread_count(), &colors)
        {
            if !self.inner.list().loaded_at_bottom {
                self.inner
                    .load_peer(peer_id, MsgId::SHOW_AT_END, backend, peer_info.summary.kind);
            } else {
                self.inner.request_scroll_to_bottom();
            }
            if let Some(last) = self.inner.messages().last() {
                backend.mark_history_read(peer_id, last.id);
            }
        }

        // 7a. Floating unread `@` mention, heart reaction, and poll-vote jump FABs
        //     (`history_unread_things.{h,cpp}`, `history_widget.cpp`).
        let unread_scope =
            tg_app_core::core_types::dto::engagement::UnreadThingsScope::history(peer_id);
        let unread_counts = backend.unread_things_counts(unread_scope);
        if let Some(unread_ev) = self.unread_things.show(
            ui,
            list_rect,
            unread_scope,
            show_scroll_btn,
            backend,
            &colors,
            self.inner.reduced_motion(),
        ) {
            match unread_ev {
                unread_things::UnreadThingsEvent::JumpTo { msg_id, .. } => {
                    self.inner
                        .jump_to_message(msg_id, backend, peer_info.summary.kind, now_ms);
                }
                unread_things::UnreadThingsEvent::MarkAllRead { kind } => {
                    let msg = match kind {
                        tg_app_core::core_types::dto::engagement::UnreadThingsKind::Mentions => {
                            "All mentions marked as read"
                        }
                        tg_app_core::core_types::dto::engagement::UnreadThingsKind::Reactions => {
                            "All reactions marked as read"
                        }
                        tg_app_core::core_types::dto::engagement::UnreadThingsKind::PollVotes => {
                            "All poll votes marked as read"
                        }
                    };
                    self.show_toast(msg, now_secs);
                }
            }
        }

        // 7a2. Broadcast channel Pull-to-Next-Unread-Channel indicator & footer strip.
        if is_broadcast_channel {
            if let Some(next_peer_id) = self.pull_next_channel.show(
                ui,
                list_rect,
                pull_footer_rect,
                !show_scroll_btn,
                self.next_unread_channel.as_ref(),
                &colors,
                self.inner.reduced_motion(),
            ) {
                action = Some(HistoryAction::OpenNextUnreadChannel(next_peer_id));
            }
        } else {
            self.pull_next_channel.reset();
        }

        // 7b. Floating compose-stash corner button & Ctrl+S shortcut (`history_view_compose_stash`).
        let stash_key = compose_stash::ComposeStashKey::main(peer_id);
        let ctrl_s_pressed = peer_info.summary.can_send
            && !self.search_open
            && self.attach_box.is_none()
            && ui.ctx().input_mut(|i| {
                i.consume_shortcut(&egui::KeyboardShortcut::new(
                    egui::Modifiers::COMMAND,
                    egui::Key::S,
                ))
            });
        if ctrl_s_pressed {
            let _ = self.exchange_compose_stash(peer_id, backend, now_secs);
        }
        let stash_bottom_offset = unread_things::HistoryUnreadThingsWidget::stack_top_offset(
            show_scroll_btn,
            unread_counts,
        );
        if peer_info.summary.can_send
            && let Some(stashed) = backend.compose_stash(stash_key)
            && let Some(stash_act) = compose_stash::paint_stash_corner_button_at_offset(
                ui,
                list_rect,
                stash_bottom_offset,
                &stashed,
                &mut self.stash_menu_open,
                &colors,
            )
        {
            match stash_act {
                compose_stash::StashMenuAction::Exchange => {
                    let _ = self.exchange_compose_stash(peer_id, backend, now_secs);
                }
                compose_stash::StashMenuAction::Delete => {
                    backend.set_compose_stash(stash_key, None);
                    self.show_toast("Stashed draft deleted", now_secs);
                }
                compose_stash::StashMenuAction::Send
                | compose_stash::StashMenuAction::SendSilent => {
                    let silent = matches!(stash_act, compose_stash::StashMenuAction::SendSilent);
                    backend.set_compose_stash(stash_key, None);
                    if stashed.has_files() {
                        let mut caption_pending =
                            stashed.has_text().then(|| stashed.draft.text.clone());
                        for file in &stashed.files {
                            let media = if file.mime_type.starts_with("image/") {
                                HistoryMedia::Photo(PhotoAttachment {
                                    width: 1920,
                                    height: 1080,
                                    stripped_thumb: None,
                                    fallback_rgb: ([0x4E, 0x90, 0xC8], [0x26, 0x4D, 0x73]),
                                    file_name: file.name.clone(),
                                    size_bytes: file.size_bytes,
                                })
                            } else if file.mime_type.starts_with("video/") {
                                HistoryMedia::Video(VideoAttachment {
                                    width: 1280,
                                    height: 720,
                                    duration_secs: 12,
                                    size_bytes: file.size_bytes,
                                    is_gif: false,
                                    stripped_thumb: None,
                                    fallback_rgb: ([0x3D, 0x6E, 0x9E], [0x1D, 0x35, 0x52]),
                                    file_name: file.name.clone(),
                                })
                            } else {
                                HistoryMedia::Document(DocumentAttachment {
                                    file_name: file.name.clone(),
                                    size_bytes: file.size_bytes,
                                    mime_type: file.mime_type.clone(),
                                    thumb_rgb: None,
                                })
                            };
                            let req = SendMessageExtended {
                                text: caption_pending.take().unwrap_or_default(),
                                media: Some(media),
                                reply_to: stashed.draft.reply_to,
                                silent,
                                ..SendMessageExtended::default()
                            };
                            if let Ok(sent) = backend.send_message_extended(peer_id, req) {
                                self.inner.append_message(sent, peer_info.summary.kind);
                            }
                        }
                        self.show_toast(
                            if silent {
                                "Sent stashed files silently"
                            } else {
                                "Sent stashed files"
                            },
                            now_secs,
                        );
                    } else if stashed.has_text() {
                        let req = SendMessageRequest {
                            text: stashed.draft.text.clone(),
                            reply_to: stashed.draft.reply_to,
                            silent,
                        };
                        match backend.send_message(peer_id, req) {
                            Ok(sent) => {
                                self.inner.append_message(sent, peer_info.summary.kind);
                                self.show_toast(
                                    if silent {
                                        "Sent stashed message silently"
                                    } else {
                                        "Sent stashed message"
                                    },
                                    now_secs,
                                );
                            }
                            Err(err) => {
                                backend.set_compose_stash(stash_key, Some(stashed));
                                self.show_toast(err, now_secs);
                            }
                        }
                    } else if let Some(ref fwd) = stashed.forward
                        && let Ok(fwds) = backend.forward_messages_with_options(
                            fwd.from_peer,
                            &fwd.msg_ids,
                            peer_id,
                            fwd.options,
                        )
                    {
                        let fwd_ids: Vec<MsgId> = fwds.iter().map(|m| m.id).collect();
                        for m in fwds {
                            self.inner.append_message(m, peer_info.summary.kind);
                        }
                        if peer_info.summary.is_saved_messages && !fwd_ids.is_empty() {
                            self.self_forwards_tagger
                                .show_for_forwarded(peer_id, fwd_ids, now_secs);
                        } else {
                            self.show_toast("Forwarded stashed messages", now_secs);
                        }
                    }
                }
                compose_stash::StashMenuAction::Schedule => {
                    let _ = self.exchange_compose_stash(peer_id, backend, now_secs);
                    let now_u = backend
                        .current_unix_time()
                        .unwrap_or(i64::from(DEFAULT_SECTION_NOW_UNIX))
                        as i32;
                    let can_online = matches!(peer_info.summary.kind, PeerKind::User)
                        && !peer_info.summary.is_saved_messages;
                    let mode = if peer_info.summary.is_saved_messages {
                        ScheduleBoxMode::SetReminder
                    } else {
                        ScheduleBoxMode::ScheduleMessage
                    };
                    self.schedule_box = Some(ScheduleBoxState::new(
                        mode,
                        now_u,
                        can_online,
                        peer_info.summary.title.clone(),
                    ));
                }
            }
        }

        // 8. Compose controls (`history_view_compose_controls.cpp`) with full extras.
        let send_as_peers = backend.send_as_peers(peer_id);
        let slow_mode = backend.slow_mode_state(peer_id);
        let now_unix_i64 = backend
            .current_unix_time()
            .unwrap_or_else(|| i64::from(DEFAULT_SECTION_NOW_UNIX) + now_secs.floor() as i64);
        let slow_remaining = slow_mode.remaining_secs(now_unix_i64);
        let can_send_when_online = matches!(peer_info.summary.kind, PeerKind::User)
            && !peer_info.summary.is_saved_messages;

        // Sync Business quick replies for private-chat `/` autocomplete.
        if let Some((enabled, ref shortcuts)) = self.business_quick_replies_override {
            self.compose
                .set_business_quick_replies(enabled, shortcuts.clone());
        } else {
            let (is_business, shortcuts) = AppBusinessHost::quick_replies_snapshot(ui.ctx());
            self.compose
                .set_business_quick_replies(is_business, shortcuts);
        }

        let compose_placeholder = self
            .reply_keyboard
            .placeholder()
            .unwrap_or_else(|| strings.write_message_placeholder())
            .to_owned();
        let bot_chrome = ComposeBotChrome {
            menu_button: bot_snapshot.as_ref().map(|s| &s.bot_info.menu_button),
            has_bot_commands: !bot_commands.is_empty(),
            commands_popup_open: self.bot_commands_open,
            has_reply_keyboard: self.reply_keyboard.has_markup(),
            reply_keyboard_visible: self.reply_keyboard.is_shown(),
            use_bot_attach_menu: has_bots_backend,
            bot_attach_open: self.bot_attach_open,
            use_rich_inline_results: has_bots_backend,
            palette,
        };
        let compose_events = if bot_start {
            if paint_bot_start_bar(ui, compose_rect, &colors)
                && let Some(text) = self.send_bot_start(peer_id, backend, now_secs)
            {
                action = Some(HistoryAction::SendMessage { peer_id, text });
            }
            Vec::new()
        } else {
            self.compose.show_extended_with_bots(
                ui,
                compose_rect,
                peer_id,
                peer_info.summary.can_send,
                peer_info.summary.muted,
                &compose_placeholder,
                &colors,
                backend,
                slow_remaining,
                &send_as_peers,
                can_send_when_online,
                Some(bot_chrome),
            )
        };
        if self.compose.text != self.last_compose_text_for_hint {
            self.last_compose_text_for_hint = self.compose.text.clone();
            // The field cleared by a send / edit / forward is not a user cut
            // (`trackChange(false)`): re-sending the same text must not show the hint.
            let user_driven = !(self.compose.text.is_empty() && !compose_events.is_empty());
            if let Some(tip) = self.stash_hint.track_change(
                &self.compose.text,
                user_driven,
                peer_info.summary.can_send,
                now_ms_u64,
            ) {
                self.show_toast(tip, now_secs);
            }
        }
        for cev in compose_events {
            match cev {
                ComposeEvent::SendMessage { text, reply_to } => {
                    let plain_copy = text.text.clone();
                    let req = SendMessageRequest {
                        text,
                        reply_to,
                        silent: false,
                    };
                    match backend.send_message(peer_id, req) {
                        Ok(sent) => {
                            backend.record_slow_mode_send(peer_id, now_unix_i64);
                            self.inner.append_message(sent, peer_info.summary.kind);
                            if self.last_compose_rect.is_positive()
                                && self.last_list_rect.is_positive()
                            {
                                let dst_w =
                                    (self.last_list_rect.width() * 0.55).clamp(120.0, 340.0);
                                let target_rect = Rect::from_min_size(
                                    Pos2::new(
                                        self.last_list_rect.right() - dst_w - 16.0,
                                        self.last_list_rect.bottom() - 48.0,
                                    ),
                                    Vec2::new(dst_w, 36.0),
                                );
                                self.fly_effects.push_text_flight(
                                    plain_copy.clone(),
                                    self.last_compose_rect,
                                    target_rect,
                                    colors.msg_out_bg,
                                    colors.text_fg,
                                    now_secs,
                                    self.fly_policy(),
                                );
                            }
                            action = Some(HistoryAction::SendMessage {
                                peer_id,
                                text: plain_copy,
                            });
                        }
                        Err(err) => self.show_toast(err, now_secs),
                    }
                }
                ComposeEvent::SendMessageExtended(req) => {
                    let plain_copy = req.text.text.clone();
                    let requested_effect_id = req.effect_id;
                    match backend.send_message_extended(peer_id, req) {
                        Ok(sent) => {
                            backend.record_slow_mode_send(peer_id, now_unix_i64);
                            let sent_id = sent.id;
                            let effect_to_play = sent.extra.effect_id.or(requested_effect_id);
                            self.inner.append_message(sent, peer_info.summary.kind);
                            if let Some(eff_id) = effect_to_play {
                                let mark = self.effect_watermark.entry(peer_id).or_default();
                                *mark = (*mark).max(sent_id);
                                if self.effects_animations_enabled {
                                    if !self.compose.effects.is_catalog_loaded() {
                                        self.compose
                                            .effects
                                            .sync_catalog(backend.available_effects());
                                    }
                                    let lottie_json = backend.effect_lottie_json(eff_id);
                                    self.compose.effects.trigger_effect(
                                        sent_id,
                                        eff_id,
                                        lottie_json,
                                        now_secs,
                                    );
                                }
                            }
                            if !plain_copy.is_empty()
                                && self.last_compose_rect.is_positive()
                                && self.last_list_rect.is_positive()
                            {
                                let dst_w =
                                    (self.last_list_rect.width() * 0.55).clamp(120.0, 340.0);
                                let target_rect = Rect::from_min_size(
                                    Pos2::new(
                                        self.last_list_rect.right() - dst_w - 16.0,
                                        self.last_list_rect.bottom() - 48.0,
                                    ),
                                    Vec2::new(dst_w, 36.0),
                                );
                                self.fly_effects.push_text_flight(
                                    plain_copy.clone(),
                                    self.last_compose_rect,
                                    target_rect,
                                    colors.msg_out_bg,
                                    colors.text_fg,
                                    now_secs,
                                    self.fly_policy(),
                                );
                            }
                            action = Some(HistoryAction::SendMessage {
                                peer_id,
                                text: plain_copy,
                            });
                        }
                        Err(err) => self.show_toast(err, now_secs),
                    }
                }
                ComposeEvent::SubmitEdit { msg_id, text } => {
                    let is_media_caption = self
                        .find_loaded_message(msg_id)
                        .is_some_and(|m| m.media.is_some());
                    if is_media_caption && !self.check_caption_length_limit(&text.text) {
                        // Opened PremiumLimitsView(CaptionLength); keep user edit intact.
                    } else {
                        match backend.edit_message(peer_id, msg_id, text) {
                            Ok(edited) => {
                                self.inner.update_message_in_place(edited);
                                self.show_toast("Message updated", now_secs);
                            }
                            Err(err) => self.show_toast(err, now_secs),
                        }
                    }
                }
                ComposeEvent::SubmitForward {
                    from_peer,
                    msg_ids,
                    comment,
                } => match backend.forward_messages(from_peer, &msg_ids, peer_id) {
                    Ok(fwds) => {
                        let fwd_ids: Vec<MsgId> = fwds.iter().map(|m| m.id).collect();
                        for m in fwds {
                            self.inner.append_message(m, peer_info.summary.kind);
                        }
                        if let Some(extra) = comment
                            && let Ok(sent) = backend.send_message(
                                peer_id,
                                SendMessageRequest {
                                    text: extra,
                                    reply_to: None,
                                    silent: false,
                                },
                            )
                        {
                            backend.save_rich_draft(ThreadKey::Main(peer_id), None);
                            self.inner.append_message(sent, peer_info.summary.kind);
                        }
                        if peer_info.summary.is_saved_messages && !fwd_ids.is_empty() {
                            self.self_forwards_tagger
                                .show_for_forwarded(peer_id, fwd_ids, now_secs);
                        } else {
                            self.show_toast("Messages forwarded", now_secs);
                        }
                    }
                    Err(err) => self.show_toast(err, now_secs),
                },
                ComposeEvent::SubmitForwardWithOptions {
                    from_peer,
                    msg_ids,
                    options,
                    comment,
                } => match backend
                    .forward_messages_with_options(from_peer, &msg_ids, peer_id, options)
                {
                    Ok(fwds) => {
                        let fwd_ids: Vec<MsgId> = fwds.iter().map(|m| m.id).collect();
                        for m in fwds {
                            self.inner.append_message(m, peer_info.summary.kind);
                        }
                        if let Some(extra) = comment
                            && let Ok(sent) = backend.send_message(
                                peer_id,
                                SendMessageRequest {
                                    text: extra,
                                    reply_to: None,
                                    silent: false,
                                },
                            )
                        {
                            backend.save_rich_draft(ThreadKey::Main(peer_id), None);
                            self.inner.append_message(sent, peer_info.summary.kind);
                        }
                        if peer_info.summary.is_saved_messages && !fwd_ids.is_empty() {
                            self.self_forwards_tagger
                                .show_for_forwarded(peer_id, fwd_ids, now_secs);
                        } else {
                            self.show_toast("Messages forwarded", now_secs);
                        }
                    }
                    Err(err) => self.show_toast(err, now_secs),
                },
                ComposeEvent::AttachClicked => {
                    self.open_attach_box(slow_mode.seconds > 0);
                }
                ComposeEvent::OpenRichEditor => {
                    self.open_rich_editor_compose(peer_id);
                }
                ComposeEvent::OpenCreatePollBox => {
                    self.poll_box = Some(CreatePollBoxState::new());
                }
                ComposeEvent::OpenComposeAi => {
                    let text = if self.compose.text.trim().is_empty() {
                        tg_text::TextWithEntities::simple(
                            "Draft message to polish with Telegram AI",
                        )
                    } else {
                        self.compose.rich_edit.to_text_with_entities()
                    };
                    self.compose_ai_box = Some(ComposeAiBoxState::new(text));
                }
                ComposeEvent::OpenStickerCreator => {
                    self.sticker_creator_box =
                        Some(StickerCreatorBoxState::new(StickerSetKind::Regular));
                }
                ComposeEvent::OpenSelfDestructBox => {
                    self.self_destruct_box = Some(SelfDestructionBoxState::new(
                        SelfDestructionBoxType::Account,
                        180,
                    ));
                }
                ComposeEvent::OpenCreateTodoBox => {
                    self.open_create_todo_box();
                }
                ComposeEvent::OpenMusicAttach => {
                    action = Some(HistoryAction::OpenMusicAttach(peer_id));
                }
                ComposeEvent::OpenLocationPicker => {
                    let provider = self.active_geo_provider();
                    let country_iso2 = backend.default_location_country_iso2();
                    let picker = LocationPickerBoxState::new(backend.nearby_venues())
                        .with_geo_provider(provider)
                        .with_initial_country(country_iso2.as_deref());
                    self.location_box = Some(picker);
                }
                ComposeEvent::OpenShareContact => {
                    self.contact_box = Some(ShareContactBoxState::new(backend.contacts()));
                }
                ComposeEvent::OpenScheduleBox => {
                    let mode = if peer_info.summary.is_saved_messages {
                        ScheduleBoxMode::SetReminder
                    } else {
                        ScheduleBoxMode::ScheduleMessage
                    };
                    self.schedule_box = Some(ScheduleBoxState::new(
                        mode,
                        DEFAULT_SECTION_NOW_UNIX,
                        can_send_when_online,
                        peer_info.summary.title.clone(),
                    ));
                }
                ComposeEvent::SendWhenOnline(req) => {
                    if backend.send_message_extended(peer_id, req).is_ok() {
                        self.show_toast("Scheduled to send when online", now_secs);
                    }
                }
                ComposeEvent::SendInlineResult(res) => {
                    let req = SendMessageExtended {
                        text: tg_text::TextWithEntities::simple(res.send_text),
                        via_bot: Some(res.bot_username),
                        ..SendMessageExtended::default()
                    };
                    if let Ok(sent) = backend.send_message_extended(peer_id, req) {
                        self.inner.append_message(sent, peer_info.summary.kind);
                    }
                }
                ComposeEvent::ToggleEmojiPanel => {
                    self.emoji_panel_open = !self.emoji_panel_open;
                    if let Some(ref mut cb) = self.on_emoji_panel_toggle {
                        cb(peer_id);
                    }
                    action = Some(HistoryAction::ToggleEmojiPanel(peer_id));
                }
                ComposeEvent::SendTyping(act) => {
                    backend.send_typing(peer_id, act);
                }
                ComposeEvent::SaveDraft(draft) => {
                    backend.save_draft(peer_id, draft);
                }
                ComposeEvent::SaveThreadDraft(draft) => {
                    backend.save_thread_draft(ThreadKey::Main(peer_id), draft);
                }
                ComposeEvent::SaveRichDraft(draft) => {
                    backend.save_rich_draft(ThreadKey::Main(peer_id), draft);
                }
                ComposeEvent::EditLastOutgoing => {
                    if let Some(target) =
                        find_last_editable_outgoing(self.inner.messages()).cloned()
                    {
                        self.compose.start_edit_rich(
                            target.id,
                            target.text.clone(),
                            target.preview_snippet(),
                        );
                    }
                }
                ComposeEvent::ReplyNavigate(delta) => {
                    let current_reply = match self.compose.mode {
                        crate::history::compose::ComposeMode::Reply { msg_id, .. } => Some(msg_id),
                        _ => None,
                    };
                    if let Some(next_target) =
                        step_reply_target(self.inner.messages(), current_reply, delta)
                    {
                        match next_target.cloned() {
                            Some(target) => {
                                self.compose.start_reply(
                                    target.id,
                                    target.sender_name.clone(),
                                    target.preview_snippet(),
                                );
                            }
                            None => {
                                self.compose.cancel_mode();
                            }
                        }
                    }
                }
                ComposeEvent::VoiceRecorded { duration_secs } => {
                    let req = SendMessageRequest {
                        text: tg_text::TextWithEntities::simple(format!(
                            "Voice note recorded ({duration_secs}s)"
                        )),
                        reply_to: None,
                        silent: false,
                    };
                    if let Ok(sent) = backend.send_message(peer_id, req) {
                        self.inner.append_message(sent, peer_info.summary.kind);
                    }
                }
                ComposeEvent::VoiceCaptured(voice) => {
                    let req = SendMessageExtended {
                        media: Some(HistoryMedia::Voice(voice)),
                        silent: self.compose.silent_mode,
                        ..SendMessageExtended::default()
                    };
                    if let Ok(sent) = backend.send_message_extended(peer_id, req) {
                        self.inner.append_message(sent, peer_info.summary.kind);
                    }
                }
                ComposeEvent::RoundVideoCaptured(rv) => {
                    let round_video_path =
                        self.compose.round_video_recorder.last_captured_mp4.take();
                    let req = SendMessageExtended {
                        media: Some(HistoryMedia::RoundVideo(rv)),
                        silent: self.compose.silent_mode,
                        round_video_path,
                        ..SendMessageExtended::default()
                    };
                    if let Ok(sent) = backend.send_message_extended(peer_id, req) {
                        self.inner.append_message(sent, peer_info.summary.kind);
                    }
                }
                ComposeEvent::SendQuickReply(shortcut) => {
                    let count = self.send_quick_reply_shortcut(
                        peer_id,
                        peer_info.summary.kind,
                        &shortcut,
                        backend,
                    );
                    if count > 0 {
                        backend.record_slow_mode_send(peer_id, now_unix_i64);
                        action = Some(HistoryAction::SendMessage {
                            peer_id,
                            text: format!("/{}", shortcut.name),
                        });
                    }
                }
                ComposeEvent::BotMenuButton(menu_act) => match menu_act {
                    BotMenuButtonAction::ToggleCommandsPopup => {
                        self.bot_commands_open = !self.bot_commands_open;
                        self.bot_commands_opened_by_slash = false;
                        self.bot_commands_dismissed_for = None;
                        self.bot_attach_open = false;
                        self.inline_results.visible = false;
                    }
                    BotMenuButtonAction::OpenWebApp { text, url } => {
                        self.bot_commands_open = false;
                        self.bot_attach_open = false;
                        let bot_id = bot_snapshot
                            .as_ref()
                            .map(|s| PeerId::from_user(s.bot_id.0))
                            .unwrap_or(peer_id);
                        self.show_toast(format!("Opening {text}..."), now_secs);
                        action = Some(HistoryAction::OpenMiniApp {
                            bot_id,
                            url: Some(url),
                            start_param: None,
                            confirm_open: true,
                        });
                    }
                },
                ComposeEvent::ToggleReplyKeyboard => {
                    self.reply_keyboard.toggle_visible();
                }
                ComposeEvent::ToggleBotAttachMenu => {
                    self.bot_attach_open = !self.bot_attach_open;
                    self.bot_commands_open = false;
                    self.inline_results.visible = false;
                }
                ComposeEvent::SaveDefaultSendAs(chosen_sa) => {
                    if let Err(err) = backend.save_default_send_as(peer_id, chosen_sa) {
                        self.show_toast(err, now_secs);
                    }
                }
                ComposeEvent::SlowModeBlocked(remaining) => {
                    self.show_toast(
                        format!(
                            "Slow mode is enabled. Wait {} before sending another message.",
                            crate::history::sections_model::SlowModeState::format_countdown(
                                remaining
                            )
                        ),
                        now_secs,
                    );
                }
            }
        }

        // 8a. Reply keyboard drawer below the compose bar (`chat_helpers/bot_keyboard.cpp`).
        if peer_info.summary.can_send && self.reply_keyboard.is_shown() && reply_kb_h > 0.0 {
            let reply_act = ui
                .scope_builder(egui::UiBuilder::new().max_rect(reply_kb_rect), |ui| {
                    self.reply_keyboard.show(
                        ui,
                        bot_snapshot.as_ref().map(|s| s.bot_id),
                        None,
                        palette,
                    )
                })
                .inner;
            if let Some(act) = reply_act
                && let Some(r_act) = self.handle_reply_keyboard_activation(
                    act,
                    peer_id,
                    &peer_info,
                    bot_snapshot.as_ref(),
                    backend,
                    now_secs,
                )
            {
                action = Some(r_act);
            }
        }

        // 8a-ii. Floating bot popups above the compose bar: `@botname query` inline results,
        // `/` commands popup, and attach-menu bots popup.
        if peer_info.summary.can_send
            && let Some(pop_act) = self.show_bot_popups(
                ui,
                list_rect,
                compose_rect,
                peer_id,
                &peer_info,
                bot_peer_id,
                bot_peer_kind,
                bot_snapshot.as_ref(),
                &bot_commands,
                slow_mode.seconds > 0,
                backend,
                palette,
                now_secs,
                now_ms_u64,
            )
        {
            action = Some(pop_act);
        }

        // 8c. Inline Self-Forwards Tagger (`history_view_self_forwards_tagger.cpp`).
        if self.self_forwards_tagger.is_visible() {
            if !self.self_forwards_tagger.has_loaded_default_tags()
                && let Some(ch) = backend.as_chat_helpers_backend()
                && let Ok(res) = ch.default_tag_reactions(false)
            {
                self.self_forwards_tagger
                    .seed_default_tag_reactions(&res.reactions);
            }
            let saved_tags = backend.saved_reaction_tags(None);
            if let Some(tag_act) =
                self.self_forwards_tagger
                    .show(ui, list_rect, &saved_tags, now_secs, &colors)
            {
                match tag_act {
                    SelfForwardsTaggerAction::ApplyTag {
                        saved_peer_id,
                        msg_ids,
                        emoji,
                    } => {
                        for mid in msg_ids {
                            if let Ok(updated) = backend.toggle_reaction(saved_peer_id, mid, &emoji)
                                && saved_peer_id == peer_id
                                && let Some(mut copy) = self.find_loaded_message(mid).cloned()
                            {
                                copy.reactions = updated;
                                self.inner.update_message_in_place(copy);
                            }
                        }
                        self.self_forwards_tagger
                            .show_tagged(saved_peer_id, emoji, now_secs);
                    }
                    SelfForwardsTaggerAction::RenameTag { emoji, title } => {
                        match backend.update_saved_reaction_tag(&emoji, title.as_deref()) {
                            Ok(_) => {
                                self.show_toast(format!("Updated tag label for {emoji}"), now_secs);
                            }
                            Err(err) => self.show_toast(err, now_secs),
                        }
                    }
                    SelfForwardsTaggerAction::OpenSavedMessages(saved_peer_id) => {
                        self.active_section = Some(HistorySectionKind::SavedSublists {
                            peer_id: saved_peer_id,
                        });
                    }
                    SelfForwardsTaggerAction::OpenFoldersForPeer(_) => {
                        action = Some(HistoryAction::OpenLink("tg://settings/folders".to_owned()));
                    }
                }
            }
        }

        // 8b. Active `tg-lottie` reaction bursts, message effects, decorative Thanos / Fly effects & reactions picker.
        self.last_list_rect = list_rect;
        // Detect newly received messages with `effect_id` and trigger Lottie playback once per message.
        let new_effects =
            take_new_effect_messages(&mut self.effect_watermark, peer_id, self.inner.messages());
        if self.effects_animations_enabled && !new_effects.is_empty() {
            if !self.compose.effects.is_catalog_loaded() {
                self.compose
                    .effects
                    .sync_catalog(backend.available_effects());
            }
            for (mid, eff_id) in new_effects {
                let lottie_json = backend.effect_lottie_json(eff_id);
                self.compose
                    .effects
                    .trigger_effect(mid, eff_id, lottie_json, now_secs);
            }
        }
        let thanos_pol = self.thanos_policy();
        let fly_pol = self.fly_policy();
        self.thanos_effect.paint(ui, now_secs, thanos_pol);
        self.fly_effects.paint(ui, now_secs, fly_pol);
        self.reactions_picker
            .paint_active_bursts(ui, list_rect, now_secs);
        self.compose
            .effects
            .paint_active_effects(ui, list_rect, now_secs);
        let available_reacts = if self.reactions_picker.target_msg_id.is_some() {
            backend.available_reactions()
        } else {
            Vec::new()
        };
        let default_react = if self.reactions_picker.target_msg_id.is_some() {
            backend.default_reaction()
        } else {
            String::new()
        };
        if let Some(pick_act) = self.reactions_picker.show_with_catalog(
            ui,
            now_secs,
            &available_reacts,
            &default_react,
            &colors,
        ) {
            match pick_act {
                ReactionsPickerAction::Pick(mid, emoji) => {
                    if let Ok(updated) = backend.toggle_reaction(peer_id, mid, &emoji)
                        && let Some(mut copy) = self.find_loaded_message(mid).cloned()
                    {
                        copy.reactions = updated;
                        self.inner.update_message_in_place(copy);
                        if self.last_list_rect.is_positive() {
                            let from = Rect::from_center_size(
                                self.reactions_picker.anchor_pos,
                                Vec2::splat(26.0),
                            );
                            let to = Rect::from_center_size(
                                Pos2::new(
                                    self.last_list_rect.center().x,
                                    (self.reactions_picker.anchor_pos.y + 42.0)
                                        .min(self.last_list_rect.bottom() - 24.0),
                                ),
                                Vec2::splat(20.0),
                            );
                            let mut args = tg_ui::effects::ReactionFlyArgs::new(emoji, from, to);
                            args.tint = colors.accent;
                            self.fly_effects
                                .push_reaction_fly(args, now_secs, self.fly_policy());
                        }
                    }
                }
                ReactionsPickerAction::SetDefault(emoji) => {
                    match backend.set_default_reaction(&emoji) {
                        Ok(()) => {
                            self.show_toast(format!("Default reaction set to {emoji}"), now_secs);
                        }
                        Err(err) => self.show_toast(err, now_secs),
                    }
                }
            }
        }

        // 8c. Compose, message-action, and bot modal boxes.
        if let Some(modal_act) =
            self.show_active_modals(ui, peer_id, &peer_info, backend, now_secs, palette, &colors)
        {
            action = Some(modal_act);
        }
        if self.poll_bot_location_share(backend, now_secs).is_none()
            && self.bot_location_pending.is_some()
        {
            ui.ctx().request_repaint_after(
                tg_app_core::core_types::platform_call::PLATFORM_CALL_REPAINT_INTERVAL,
            );
        }
        if let Some(modal_act) = self.show_bot_modal(ui, rect, peer_id, backend, palette, now_secs)
        {
            action = Some(modal_act);
        }

        // 9. Ephemeral floating status toast inside the chat column.
        self.paint_status_toast(ui, list_rect, now_secs);

        action
    }

    fn reset_bot_state_for_peer(&mut self, peer_id: PeerId, backend: &dyn HistoryBackend) {
        self.reply_keyboard.clear();
        self.inline_results.visible = false;
        self.bot_commands_open = false;
        self.bot_commands_opened_by_slash = false;
        self.bot_commands_dismissed_for = None;
        self.bot_attach_open = false;
        self.bot_modal = None;
        self.join_requests_box = None;
        self.pending_game_url_bot = None;
        self.pending_callbacks.clear();
        self.last_inline_compose_text.clear();
        self.compose.bot_popup_active = false;
        self.sync_reply_keyboard_from_loaded(peer_id, backend);
    }

    /// START bar click (`HistoryWidget::sendBotStartCommand`): a pending `start` token goes out
    /// with `messages.startBot`, otherwise a plain `/start` message is sent.
    fn send_bot_start(
        &mut self,
        peer_id: PeerId,
        backend: &mut dyn HistoryBackend,
        now_secs: f64,
    ) -> Option<String> {
        let sent = match self.bot_start_tokens.remove(&peer_id) {
            Some(token) if !token.is_empty() => match backend.as_bots_backend_mut() {
                Some(bots) => {
                    let params = tg_bots::protocol::StartBotParams::for_private_chat(
                        tg_bots::stand_in::UserId(peer_id.bare()),
                        token,
                        0,
                    );
                    bots.start_bot(&params).map(|_| ())
                }
                None => Err("Bots are not available in this session".to_owned()),
            },
            _ => backend
                .send_message(
                    peer_id,
                    SendMessageRequest {
                        text: tg_text::TextWithEntities::simple("/start"),
                        reply_to: None,
                        silent: false,
                    },
                )
                .map(|_| ()),
        };
        match sent {
            Ok(()) => {
                self.reload_active_peer(backend);
                self.sync_reply_keyboard_from_loaded(peer_id, backend);
                Some("/start".to_owned())
            }
            Err(err) => {
                self.show_toast(err, now_secs);
                None
            }
        }
    }

    fn sync_reply_keyboard_from_loaded(&mut self, peer_id: PeerId, backend: &dyn HistoryBackend) {
        let from_msgs = self.inner.messages().iter().rev().find_map(|m| {
            m.extra
                .reply_markup
                .as_ref()
                .filter(|rm| rm.defines_reply_keyboard() || rm.is_remove_keyboard())
                .map(|rm| {
                    (
                        to_bot_full(FullMsgId {
                            peer: peer_id,
                            msg: m.id,
                        }),
                        rm.clone(),
                    )
                })
        });
        if let Some((mid, markup)) = from_msgs {
            self.reply_keyboard
                .update_markup(Some(mid), Some(&markup), true);
            return;
        }
        if let Some(bots_be) = backend.as_bots_backend()
            && let Some((mid, markup)) =
                bots_be.active_reply_keyboard_for_peer(to_bot_peer(peer_id))
        {
            self.reply_keyboard
                .update_markup(Some(mid), Some(&markup), true);
        }
    }

    fn tick_pending_bot_callbacks(
        &mut self,
        backend: &mut dyn HistoryBackend,
        now_secs: f64,
        now_ms_u64: u64,
    ) -> Option<HistoryAction> {
        if self.pending_callbacks.is_empty() {
            return None;
        }
        let mut remaining = Vec::new();
        let mut ready = Vec::new();
        for item in self.pending_callbacks.drain(..) {
            if now_ms_u64 >= item.deliver_at_ms {
                ready.push(item);
            } else {
                remaining.push(item);
            }
        }
        self.pending_callbacks = remaining;

        let mut out_action = None;
        for item in ready {
            self.inner
                .inline_kb_state_mut()
                .clear_loading(item.msg_id, item.row, item.col);
            if let Some(act) = self.execute_bot_callback_now(
                &item.request,
                item.bot_id,
                &item.bot_name,
                item.bot_verified,
                item.bot_trusted_for_game,
                backend,
                now_secs,
                now_ms_u64,
            ) {
                out_action = Some(act);
            }
        }
        out_action
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_bot_callback_now(
        &mut self,
        request: &CallbackQueryRequest,
        bot_id: PeerId,
        bot_name: &str,
        bot_verified: bool,
        bot_trusted_for_game: bool,
        backend: &mut dyn HistoryBackend,
        now_secs: f64,
        now_ms_u64: u64,
    ) -> Option<HistoryAction> {
        let bots_be = backend.as_bots_backend_mut()?;
        match bots_be.send_callback_query(request) {
            Ok(answer) => {
                let cache_key = CallbackCacheKey {
                    item_id: tg_bots::stand_in::FullMsgId::new(request.peer_id, request.msg_id),
                    is_game: request.is_game,
                    data: request.data.clone().unwrap_or_default(),
                };
                self.callback_cache
                    .insert(cache_key, answer.clone(), now_ms_u64 / 1_000);
                self.apply_bot_callback_answer(
                    request,
                    &answer,
                    bot_id,
                    bot_name,
                    bot_verified,
                    bot_trusted_for_game,
                    now_secs,
                )
            }
            Err(err) => {
                self.show_toast(format!("Callback error: {err}"), now_secs);
                None
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_bot_callback_answer(
        &mut self,
        request: &CallbackQueryRequest,
        answer: &tg_bots::callback::CallbackAnswerInput,
        bot_id: PeerId,
        bot_name: &str,
        bot_verified: bool,
        bot_trusted_for_game: bool,
        now_secs: f64,
    ) -> Option<HistoryAction> {
        let trusted_for_game = bot_trusted_for_game || self.trusted_game_bots.contains(&bot_id);
        match handle_callback_answer(
            request.is_game,
            request.requires_password,
            bot_verified,
            trusted_for_game,
            answer,
        ) {
            CallbackAnswerAction::ShowAlert { message } => {
                self.bot_modal = Some(BotModalBox::CallbackAlert {
                    bot_name: bot_name.to_owned(),
                    message,
                });
                None
            }
            CallbackAnswerAction::ShowToast { message, .. } => {
                self.show_toast(message.clone(), now_secs);
                Some(HistoryAction::Toast(message))
            }
            CallbackAnswerAction::OpenUrl { url } => {
                self.show_toast(format!("Opening {url}"), now_secs);
                Some(HistoryAction::OpenLink(url))
            }
            CallbackAnswerAction::OpenGameUrl {
                url,
                is_local_link,
                require_confirmation,
                ..
            } => {
                if is_local_link {
                    self.show_toast(format!("Opening {url}"), now_secs);
                    Some(HistoryAction::OpenLink(url))
                } else if require_confirmation {
                    self.pending_game_url_bot = Some(bot_id);
                    self.bot_modal = Some(BotModalBox::UrlConfirm {
                        url,
                        bot_name: bot_name.to_owned(),
                    });
                    None
                } else {
                    self.show_toast(format!("Launching game: {url}"), now_secs);
                    Some(HistoryAction::OpenMiniApp {
                        bot_id,
                        url: Some(url),
                        start_param: None,
                        confirm_open: false,
                    })
                }
            }
            CallbackAnswerAction::HidePasswordLayer | CallbackAnswerAction::None => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_inline_keyboard_activation(
        &mut self,
        ctx: &egui::Context,
        act: InlineKeyboardActivation,
        peer_id: PeerId,
        peer_info: &HistoryPeerInfo,
        bot_snapshot: Option<&BotChatSnapshot>,
        backend: &mut dyn HistoryBackend,
        now_secs: f64,
        now_ms_u64: u64,
    ) -> Option<HistoryAction> {
        let bot_id = bot_snapshot
            .map(|s| PeerId::from_user(s.bot_id.0))
            .or_else(|| {
                self.find_loaded_message(MsgId(act.msg_id.msg.0))
                    .map(|m| m.from_id)
            })
            .unwrap_or(peer_id);
        let bot_name = bot_snapshot
            .map(|s| s.bot_name.clone())
            .unwrap_or_else(|| peer_info.summary.title.clone());
        let bot_username = bot_snapshot
            .map(|s| s.bot_username.clone())
            .or_else(|| peer_info.summary.username.clone())
            .unwrap_or_else(|| "bot".to_owned());
        let bot_verified = bot_snapshot.is_some_and(|s| s.bot_verified);
        let bot_trusted_for_game = bot_snapshot.is_some_and(|s| s.bot_trusted_for_game);

        match act.action {
            BotButtonAction::SendCallback(req) => {
                // "Share Score" button on a game message opens the ShareGameScore modal box.
                if req.data.as_deref() == Some(b"share_game_score")
                    && let Some(msg) = self.find_loaded_message(MsgId(act.msg_id.msg.0))
                    && let Some(HistoryMedia::Game(ref game_att)) = msg.media
                {
                    let share_bot_username = if game_att.bot_username.is_empty() {
                        bot_username.clone()
                    } else {
                        game_att.bot_username.clone()
                    };
                    let game_data = game_att.game.clone();
                    let high_scores = game_att.high_scores.clone();
                    let bots_be = backend.as_bots_backend();
                    self.bot_modal = Some(BotModalBox::share_game_score(
                        act.msg_id,
                        share_bot_username,
                        game_data,
                        &high_scores,
                        |uid| {
                            bots_be.map_or_else(
                                || format!("User #{}", uid.0),
                                |b| b.resolve_user_name(uid),
                            )
                        },
                    ));
                    return None;
                }
                if req.requires_password {
                    self.bot_modal = Some(BotModalBox::CallbackPassword {
                        request: req,
                        button_text: act.button.text,
                        password_input: String::new(),
                        error: None,
                    });
                    return None;
                }
                let cache_key = CallbackCacheKey {
                    item_id: act.msg_id,
                    is_game: req.is_game,
                    data: req.data.clone().unwrap_or_default(),
                };
                if let Some(cached) = self
                    .callback_cache
                    .get(&cache_key, now_ms_u64 / 1_000)
                    .cloned()
                {
                    return self.apply_bot_callback_answer(
                        &req,
                        &cached,
                        bot_id,
                        &bot_name,
                        bot_verified,
                        bot_trusted_for_game,
                        now_secs,
                    );
                }
                self.inner
                    .inline_kb_state_mut()
                    .set_loading(act.msg_id, act.row, act.col, now_secs);
                self.pending_callbacks.push(PendingHistoryCallback {
                    msg_id: act.msg_id,
                    row: act.row,
                    col: act.col,
                    request: req,
                    bot_id,
                    bot_name,
                    bot_verified,
                    bot_trusted_for_game,
                    deliver_at_ms: now_ms_u64.saturating_add(CALLBACK_SPINNER_DELAY_MS),
                });
                None
            }
            BotButtonAction::OpenUrl {
                url,
                require_confirmation,
            } => {
                if require_confirmation {
                    self.pending_game_url_bot = None;
                    self.bot_modal = Some(BotModalBox::UrlConfirm { url, bot_name });
                    None
                } else {
                    self.show_toast(format!("Opening {url}"), now_secs);
                    Some(HistoryAction::OpenLink(url))
                }
            }
            BotButtonAction::RequestUrlAuth(source) => {
                self.begin_bot_url_auth(source, &bot_name, bot_snapshot, backend, now_secs)
            }
            BotButtonAction::SwitchInline(decision) => match decision {
                SwitchInlineDecision::SwitchSamePeer { bot_id, query, .. } => {
                    let composed = format!("@{bot_username} {query}");
                    self.compose.text = composed.clone();
                    self.compose.rich_edit.set_text_with_entities(
                        tg_text::TextWithEntities::simple(composed.clone()),
                    );
                    self.compose.focus_requested = true;
                    self.last_inline_compose_text = composed;
                    if let Some(bots_be) = backend.as_bots_backend_mut() {
                        let resolved_bot_id = bot_snapshot
                            .map(|s| s.bot_id)
                            .or_else(|| bots_be.resolve_inline_bot(&bot_username).map(|(id, _)| id))
                            .unwrap_or(bot_id);
                        let _ = self.inline_results.fetch_immediately(
                            resolved_bot_id,
                            bot_username,
                            to_bot_peer(peer_id),
                            query,
                            now_ms_u64,
                            bots_be,
                        );
                    }
                    None
                }
                SwitchInlineDecision::ReturnToSavedPeer {
                    bot_id,
                    query,
                    return_peer,
                } => Some(HistoryAction::SwitchInlineToPeer {
                    target_peer: PeerId(return_peer.0),
                    bot_id: PeerId::from_user(bot_id.0),
                    bot_username,
                    query,
                }),
                SwitchInlineDecision::ChooseRecipient {
                    bot_id,
                    query,
                    peer_types,
                } => Some(HistoryAction::SwitchInlineChooseChat {
                    bot_id: PeerId::from_user(bot_id.0),
                    bot_username,
                    query,
                    peer_types,
                }),
            },
            BotButtonAction::OpenWebView {
                bot_id, text, url, ..
            } => {
                self.show_toast(format!("Opening {text}..."), now_secs);
                Some(HistoryAction::OpenMiniApp {
                    bot_id: PeerId::from_user(bot_id.0),
                    url: Some(url),
                    start_param: None,
                    confirm_open: true,
                })
            }
            BotButtonAction::CopyText { text } => {
                ctx.copy_text(text.clone());
                self.show_toast(format!("Copied: {text}"), now_secs);
                Some(HistoryAction::Toast(format!("Copied: {text}")))
            }
            BotButtonAction::OpenUserProfile { user_id } => {
                let name = backend.as_bots_backend().map_or_else(
                    || format!("User #{}", user_id.0),
                    |b| b.resolve_user_name(user_id),
                );
                self.show_toast(format!("Profile: {name}"), now_secs);
                None
            }
            BotButtonAction::OpenCheckout { item_id } => {
                let msg_id = MsgId(item_id.msg.0);
                let invoice_id = self
                    .find_loaded_message(msg_id)
                    .and_then(|m| m.extra.link_preview.as_ref())
                    .map(|lp| lp.url.trim())
                    .filter(|u| !u.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("msg:{}:{}", peer_id.0, msg_id.0));
                Some(HistoryAction::OpenInvoice {
                    peer_id,
                    msg_id,
                    invoice_id,
                })
            }
            _ => None,
        }
    }

    fn begin_bot_url_auth(
        &mut self,
        source: UrlAuthSource,
        bot_name: &str,
        bot_snapshot: Option<&BotChatSnapshot>,
        backend: &mut dyn HistoryBackend,
        now_secs: f64,
    ) -> Option<HistoryAction> {
        let account_name = bot_snapshot
            .map(|s| s.current_user_name.clone())
            .unwrap_or_else(|| backend.self_peer().1);
        let bots_be = backend.as_bots_backend_mut()?;
        match bots_be.request_url_auth(&source) {
            Ok(result) => match handle_url_auth_request_result(source, result) {
                UrlAuthDecision::OpenAcceptedUrl { url, .. } => {
                    self.show_toast(format!("Authenticated redirect: {url}"), now_secs);
                    Some(HistoryAction::OpenLink(url))
                }
                UrlAuthDecision::OpenDefaultUrl { url, .. } => {
                    self.bot_modal = Some(BotModalBox::UrlConfirm {
                        url,
                        bot_name: bot_name.to_owned(),
                    });
                    None
                }
                UrlAuthDecision::ShowConfirmation(box_data) => {
                    self.bot_modal = Some(BotModalBox::url_auth(*box_data, account_name));
                    None
                }
                UrlAuthDecision::NoOp => None,
            },
            Err(err_type) => match handle_url_auth_request_error(&source, &err_type) {
                tg_bots::callback::UrlAuthErrorAction::ShowExpiredToast => {
                    self.show_toast("Login link has expired.", now_secs);
                    None
                }
                tg_bots::callback::UrlAuthErrorAction::OpenDefaultUrl { url, .. } => {
                    self.bot_modal = Some(BotModalBox::UrlConfirm {
                        url,
                        bot_name: bot_name.to_owned(),
                    });
                    None
                }
            },
        }
    }

    fn handle_reply_keyboard_activation(
        &mut self,
        act: ReplyKeyboardActivation,
        peer_id: PeerId,
        peer_info: &HistoryPeerInfo,
        bot_snapshot: Option<&BotChatSnapshot>,
        backend: &mut dyn HistoryBackend,
        now_secs: f64,
    ) -> Option<HistoryAction> {
        let bot_name = bot_snapshot
            .map(|s| s.bot_name.clone())
            .unwrap_or_else(|| peer_info.summary.title.clone());
        let phone = bot_snapshot
            .map(|s| s.current_user_phone.clone())
            .unwrap_or_else(|| "+999 661 0000".to_owned());

        match act.action {
            BotButtonAction::SendCommand {
                command, reply_to, ..
            } => {
                let req = SendMessageRequest {
                    text: tg_text::TextWithEntities::simple(command.clone()),
                    reply_to: reply_to.map(|f| MsgId(f.msg.0)),
                    silent: self.compose.silent_mode,
                };
                match backend.send_message(peer_id, req) {
                    Ok(sent) => {
                        self.inner.append_message(sent, peer_info.summary.kind);
                        Some(HistoryAction::SendMessage {
                            peer_id,
                            text: command,
                        })
                    }
                    Err(err) => {
                        self.show_toast(err, now_secs);
                        None
                    }
                }
            }
            BotButtonAction::ConfirmSharePhone { item_id, .. } => {
                self.bot_modal = Some(BotModalBox::RequestPhone {
                    item_id,
                    bot_name,
                    phone_number: phone,
                });
                None
            }
            BotButtonAction::ShowLocationUnavailable { .. } => {
                self.bot_modal = Some(BotModalBox::request_location(bot_name));
                None
            }
            BotButtonAction::CreatePoll { restriction, .. } => {
                self.bot_modal = Some(BotModalBox::request_poll(restriction));
                None
            }
            BotButtonAction::ChoosePeer {
                item_id,
                button_id,
                query,
                ..
            } => {
                self.bot_modal = Some(BotModalBox::RequestPeer {
                    item_id,
                    button_id,
                    query,
                    selected: Vec::new(),
                    search: String::new(),
                });
                None
            }
            BotButtonAction::OpenWebView {
                bot_id, text, url, ..
            } => {
                self.show_toast(format!("Opening {text}..."), now_secs);
                Some(HistoryAction::OpenMiniApp {
                    bot_id: PeerId::from_user(bot_id.0),
                    url: Some(url),
                    start_param: None,
                    confirm_open: true,
                })
            }
            _ => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn show_bot_popups(
        &mut self,
        ui: &mut Ui,
        list_rect: Rect,
        compose_rect: Rect,
        peer_id: PeerId,
        peer_info: &HistoryPeerInfo,
        bot_peer_id: tg_bots::stand_in::PeerId,
        bot_peer_kind: tg_bots::stand_in::PeerKind,
        bot_snapshot: Option<&BotChatSnapshot>,
        bot_commands: &[tg_bots::commands::BotCommand],
        slow_mode_active: bool,
        backend: &mut dyn HistoryBackend,
        palette: &Palette,
        now_secs: f64,
        now_ms_u64: u64,
    ) -> Option<HistoryAction> {
        let mut out_action = None;

        // 1. Sync `/` slash-commands popup in chats that expose bot commands.
        if !bot_commands.is_empty() && self.compose.text.starts_with('/') {
            if self.bot_commands_dismissed_for.as_deref() != Some(self.compose.text.as_str()) {
                self.bot_commands_open = true;
                self.bot_commands_opened_by_slash = true;
                self.bot_attach_open = false;
            }
        } else {
            self.bot_commands_dismissed_for = None;
            if self.bot_commands_opened_by_slash {
                self.bot_commands_open = false;
                self.bot_commands_opened_by_slash = false;
            }
        }

        // 2. Sync `@botname query` inline bot results panel (400 ms debounce + pagination + mosaic/list).
        // Only feed `sync_from_compose_text` when the text actually changed so the 400ms debounce
        // deadline is not reset on every frame; always poll the debounce timer each frame.
        let compose_text = self.compose.text.clone();
        if let Some(bots_be) = backend.as_bots_backend_mut() {
            if compose_text != self.last_inline_compose_text {
                self.last_inline_compose_text = compose_text.clone();
                if let Some(parsed) = parse_bot_inline_query(&compose_text, None)
                    && let Some((bot_id, canonical_username)) =
                        bots_be.resolve_inline_bot(&parsed.username)
                {
                    let matched = self.inline_results.sync_from_compose_text(
                        &compose_text,
                        bot_id,
                        &canonical_username,
                        bot_peer_id,
                        now_ms_u64,
                    );
                    if matched {
                        self.bot_commands_open = false;
                        self.bot_commands_opened_by_slash = false;
                        self.bot_attach_open = false;
                    }
                } else {
                    self.inline_results.visible = false;
                }
            }
            if self.inline_results.visible {
                let _ = self.inline_results.poll_debounce(now_ms_u64, bots_be);
                if self
                    .inline_results
                    .controller
                    .debounce_deadline_ms()
                    .is_some()
                {
                    ui.ctx().request_repaint();
                }
            }
        }

        self.compose.bot_popup_active =
            self.inline_results.should_render() || self.bot_commands_open || self.bot_attach_open;

        let escape_pressed = ui.ctx().input(|i| i.key_pressed(egui::Key::Escape));

        // 3. Render active popup inside a foreground Area anchored cleanly above `compose_rect`.
        if self.inline_results.should_render() {
            if escape_pressed {
                self.inline_results.visible = false;
                return None;
            }
            let panel_w = (list_rect.width() - 24.0).clamp(320.0, 560.0);
            let max_h = (list_rect.height() - 16.0).clamp(180.0, 390.0);
            let panel_pos = Pos2::new(
                list_rect.left() + 12.0,
                (compose_rect.top() - max_h - 6.0).max(list_rect.top() + 8.0),
            );
            let panel_size =
                Vec2::new(panel_w, (compose_rect.top() - 6.0 - panel_pos.y).max(160.0));
            let mut inline_ev = None;
            if let Some(bots_be) = backend.as_bots_backend_mut() {
                egui::Area::new(egui::Id::unique(("tg_hist_inline_results", peer_id.0)))
                    .order(egui::Order::Foreground)
                    .fixed_pos(panel_pos)
                    .show(ui.ctx(), |ui| {
                        ui.set_min_size(panel_size);
                        ui.set_max_size(panel_size);
                        inline_ev =
                            self.inline_results
                                .show(ui, panel_w, panel_size.y, palette, bots_be);
                    });
            }
            if let Some(ev) = inline_ev {
                match ev {
                    InlineResultsEvent::SendResult { bot_id, result } => {
                        let sent_ok = backend.as_bots_backend_mut().is_some_and(|b| {
                            b.send_inline_result(bot_id, bot_peer_id, &result).is_ok()
                        });
                        if sent_ok {
                            self.reload_active_peer(backend);
                            self.compose.clear_after_send();
                            self.inline_results.visible = false;
                            let title = if result.title.is_empty() {
                                result.id.clone()
                            } else {
                                result.title.clone()
                            };
                            self.show_toast(format!("Sent inline result: {title}"), now_secs);
                            out_action = Some(HistoryAction::SendMessage {
                                peer_id,
                                text: title,
                            });
                        }
                    }
                    InlineResultsEvent::SwitchPm { bot_id, button } => {
                        self.inline_results.visible = false;
                        match button {
                            SwitchPmButton::StartPm {
                                text, start_param, ..
                            } => {
                                self.show_toast(format!("{text} (/start {start_param})"), now_secs);
                            }
                            SwitchPmButton::WebView { text, url } => {
                                self.show_toast(format!("Opening {text}..."), now_secs);
                                out_action = Some(HistoryAction::OpenMiniApp {
                                    bot_id: PeerId::from_user(bot_id.0),
                                    url: Some(url),
                                    start_param: None,
                                    confirm_open: true,
                                });
                            }
                        }
                    }
                    InlineResultsEvent::Dismissed => {
                        self.inline_results.visible = false;
                    }
                }
            }
        } else if self.bot_commands_open && !bot_commands.is_empty() {
            if escape_pressed {
                self.bot_commands_open = false;
                self.bot_commands_opened_by_slash = false;
                self.bot_commands_dismissed_for = Some(self.compose.text.clone());
                return None;
            }
            let pop_w = (list_rect.width() - 24.0).clamp(280.0, 400.0);
            let filtered_count =
                crate::bots::menus::filter_bot_commands(bot_commands, &self.compose.text)
                    .len()
                    .max(1);
            let pop_h = (34.0 + (filtered_count as f32 * 44.0).min(240.0) + 6.0)
                .min((list_rect.height() - 16.0).max(100.0));
            let pop_pos = Pos2::new(
                list_rect.left() + 12.0,
                (compose_rect.top() - pop_h - 6.0).max(list_rect.top() + 8.0),
            );
            let bot_username = bot_snapshot
                .map(|s| s.bot_username.as_str())
                .or(peer_info.summary.username.as_deref())
                .unwrap_or("bot");
            let peer_bot_status =
                bot_snapshot.map_or(tg_bots::commands::BotStatus::HasBots, |s| s.peer_bot_status);
            let mut cmd_ev = None;
            egui::Area::new(egui::Id::unique(("tg_hist_bot_commands", peer_id.0)))
                .order(egui::Order::Foreground)
                .fixed_pos(pop_pos)
                .show(ui.ctx(), |ui| {
                    cmd_ev = show_bot_commands_popup(
                        ui,
                        BotCommandsPopupParams {
                            commands: bot_commands,
                            filter_prefix: &self.compose.text,
                            peer_kind: bot_peer_kind,
                            peer_bot_status,
                            bot_username,
                            max_width: pop_w,
                        },
                        palette,
                    );
                });
            if let Some(ev) = cmd_ev {
                match ev {
                    BotCommandsPopupEvent::ExecuteCommand(cmd) => {
                        self.bot_commands_open = false;
                        self.bot_commands_opened_by_slash = false;
                        self.bot_commands_dismissed_for = None;
                        self.compose.clear_after_send();
                        let req = SendMessageRequest {
                            text: tg_text::TextWithEntities::simple(cmd.clone()),
                            reply_to: None,
                            silent: self.compose.silent_mode,
                        };
                        if let Ok(sent) = backend.send_message(peer_id, req) {
                            self.inner.append_message(sent, peer_info.summary.kind);
                            self.show_toast(format!("Executed {cmd}"), now_secs);
                            out_action = Some(HistoryAction::SendMessage { peer_id, text: cmd });
                        }
                    }
                    BotCommandsPopupEvent::Dismissed => {
                        self.bot_commands_open = false;
                        self.bot_commands_opened_by_slash = false;
                        self.bot_commands_dismissed_for = Some(self.compose.text.clone());
                    }
                }
            }
        } else if self.bot_attach_open {
            if escape_pressed {
                self.bot_attach_open = false;
                return None;
            }
            if let Some(store) = backend.as_bots_backend().map(|b| b.load_attach_menu()) {
                let pop_h = 310.0_f32.min((list_rect.height() - 16.0).max(160.0));
                let pop_pos = Pos2::new(
                    list_rect.left() + 12.0,
                    (compose_rect.top() - pop_h - 6.0).max(list_rect.top() + 8.0),
                );
                let mut attach_ev = None;
                egui::Area::new(egui::Id::unique(("tg_hist_bot_attach", peer_id.0)))
                    .order(egui::Order::Foreground)
                    .fixed_pos(pop_pos)
                    .show(ui.ctx(), |ui| {
                        attach_ev = show_attach_menu_bots_popup(
                            ui,
                            &store,
                            bot_peer_id,
                            bot_peer_kind,
                            palette,
                        );
                    });
                if let Some(ev) = attach_ev {
                    match ev {
                        AttachMenuPopupEvent::StandardAttach(kind) => {
                            self.bot_attach_open = false;
                            match kind {
                                "Poll" => {
                                    self.poll_box = Some(CreatePollBoxState::new());
                                }
                                "Checklist" => {
                                    self.open_create_todo_box();
                                }
                                "Music" => {
                                    out_action = Some(HistoryAction::OpenMusicAttach(peer_id));
                                }
                                _ => {
                                    self.open_attach_box(slow_mode_active);
                                }
                            }
                        }
                        AttachMenuPopupEvent::LaunchBot(bot) => {
                            self.bot_attach_open = false;
                            if bot.disclaimer_required && !store.is_disclaimer_accepted(bot.bot_id)
                            {
                                let allow_write = bot.request_write_access;
                                self.bot_modal =
                                    Some(BotModalBox::AttachBotDisclaimer { bot, allow_write });
                            } else {
                                self.show_toast(
                                    format!("Launched Mini App: {}", bot.short_name),
                                    now_secs,
                                );
                                out_action = Some(HistoryAction::OpenMiniApp {
                                    bot_id: PeerId::from_user(bot.bot_id.0),
                                    url: None,
                                    start_param: None,
                                    confirm_open: false,
                                });
                            }
                        }
                        AttachMenuPopupEvent::AddBot(bot_id) => {
                            if let Some(bots_be) = backend.as_bots_backend_mut()
                                && bots_be
                                    .toggle_attach_menu_bot(bot_id, AttachMenuToggleState::Added)
                                    .is_ok()
                            {
                                self.show_toast("Added bot to Attach Menu", now_secs);
                            }
                        }
                        AttachMenuPopupEvent::RemoveBot(bot_id) => {
                            if let Some(bots_be) = backend.as_bots_backend_mut()
                                && bots_be
                                    .toggle_attach_menu_bot(bot_id, AttachMenuToggleState::Removed)
                                    .is_ok()
                            {
                                self.show_toast("Removed bot from Attach Menu", now_secs);
                            }
                        }
                        AttachMenuPopupEvent::Dismissed => {
                            self.bot_attach_open = false;
                        }
                    }
                }
            }
        }

        self.compose.bot_popup_active =
            self.inline_results.should_render() || self.bot_commands_open || self.bot_attach_open;

        out_action
    }

    fn show_bot_modal(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        peer_id: PeerId,
        backend: &mut dyn HistoryBackend,
        palette: &Palette,
        now_secs: f64,
    ) -> Option<HistoryAction> {
        let mut modal = self.bot_modal.take()?;
        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            return None;
        }
        let candidates = backend
            .as_bots_backend()
            .map(|b| b.candidate_peers())
            .unwrap_or_default();
        let mut outcome = None;
        egui::Area::new(egui::Id::unique(("tg_hist_bot_modal", peer_id.0)))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .show(ui.ctx(), |ui| {
                ui.set_min_size(rect.size());
                ui.set_max_size(rect.size());
                outcome = show_bot_modal_box(ui, rect, &mut modal, &candidates, palette);
            });

        let Some(outcome) = outcome else {
            self.bot_modal = Some(modal);
            return None;
        };

        let now_ms_u64 = (now_secs * 1000.0).max(0.0) as u64;
        match outcome {
            BotModalOutcome::Closed => {
                self.pending_game_url_bot = None;
                None
            }
            BotModalOutcome::OpenUrlConfirmed(url) => {
                if let Some(game_bot_id) = self.pending_game_url_bot.take() {
                    self.trusted_game_bots.insert(game_bot_id);
                    self.show_toast(format!("Launching game: {url}"), now_secs);
                    Some(HistoryAction::OpenMiniApp {
                        bot_id: game_bot_id,
                        url: Some(url),
                        start_param: None,
                        confirm_open: false,
                    })
                } else {
                    self.show_toast(format!("Opening external link: {url}"), now_secs);
                    Some(HistoryAction::OpenLink(url))
                }
            }
            BotModalOutcome::UrlAuthAnswered { data, choice } => {
                match build_accept_url_auth(data.source.clone(), choice) {
                    AcceptUrlAuthDecision::Decline {
                        url,
                        open_fallback_url,
                    } => {
                        if let Some(bots_be) = backend.as_bots_backend_mut() {
                            let _ = bots_be.decline_url_auth(&url);
                        }
                        if open_fallback_url {
                            self.show_toast(format!("Opening {url}"), now_secs);
                            Some(HistoryAction::OpenLink(url))
                        } else {
                            None
                        }
                    }
                    AcceptUrlAuthDecision::Accept(params) => {
                        let source = params.source.clone();
                        let write_note = if params.write_allowed {
                            " (bot write access granted)"
                        } else {
                            ""
                        };
                        let res = backend
                            .as_bots_backend_mut()
                            .map(|b| b.accept_url_auth(&params));
                        match res {
                            Some(Ok(auth_res)) => {
                                match handle_url_auth_accept_result(&source, auth_res) {
                                    UrlAuthAcceptOutcome::OpenUrl { url, .. } => {
                                        self.show_toast(
                                            format!(
                                                "Logged in to {}{write_note}",
                                                data.display_name
                                            ),
                                            now_secs,
                                        );
                                        Some(HistoryAction::OpenLink(url))
                                    }
                                    UrlAuthAcceptOutcome::ShowSuccessToast => {
                                        self.show_toast(
                                            format!("Authorization succeeded{write_note}"),
                                            now_secs,
                                        );
                                        None
                                    }
                                    UrlAuthAcceptOutcome::DoneWithoutNavigation => {
                                        self.show_toast("Login completed.", now_secs);
                                        None
                                    }
                                }
                            }
                            Some(Err(err)) => {
                                self.show_toast(format!("Login failed: {err}"), now_secs);
                                None
                            }
                            None => None,
                        }
                    }
                }
            }
            BotModalOutcome::CallbackPasswordSubmitted { request, .. } => self
                .execute_bot_callback_now(
                    &request, peer_id, "Bot", false, false, backend, now_secs, now_ms_u64,
                ),
            BotModalOutcome::SharePhoneConfirmed(item_id) => {
                let ok = backend
                    .as_bots_backend_mut()
                    .is_some_and(|b| b.share_phone(item_id.peer, Some(item_id.msg)).is_ok());
                if ok {
                    self.reload_active_peer(backend);
                    self.show_toast("Phone number shared with bot", now_secs);
                }
                None
            }
            BotModalOutcome::ShareLocationRequested => {
                self.start_bot_location_share(peer_id, Some(ui.ctx().clone()), now_secs);
                None
            }
            BotModalOutcome::ShareLocationConfirmed { lat_e7, lon_e7 } => {
                self.send_bot_location(
                    peer_id,
                    (lat_e7 as f64) / 1e7,
                    (lon_e7 as f64) / 1e7,
                    backend,
                    now_secs,
                );
                None
            }
            BotModalOutcome::PollSubmitted(poll) => {
                let ok = backend
                    .as_bots_backend_mut()
                    .is_some_and(|b| b.send_poll(to_bot_peer(peer_id), &poll).is_ok());
                if ok {
                    self.reload_active_peer(backend);
                    self.show_toast("Poll sent to chat", now_secs);
                }
                None
            }
            BotModalOutcome::PeersSelected {
                item_id,
                button_id,
                peers,
            } => {
                let count = peers.len();
                let ok = backend
                    .as_bots_backend_mut()
                    .is_some_and(|b| b.send_requested_peers(item_id, button_id, &peers).is_ok());
                if ok {
                    self.reload_active_peer(backend);
                    self.show_toast(format!("Shared {count} peer(s) with bot"), now_secs);
                }
                None
            }
            BotModalOutcome::GameShared {
                message_id,
                target_peer,
            } => {
                if let Some(bots_be) = backend.as_bots_backend_mut() {
                    match bots_be.share_game_score(message_id, target_peer) {
                        Ok(summary) => self.show_toast(summary, now_secs),
                        Err(err) => self.show_toast(err, now_secs),
                    }
                }
                None
            }
            BotModalOutcome::CopyGameLink(link) => {
                ui.ctx().copy_text(link.clone());
                self.show_toast(format!("Copied game link: {link}"), now_secs);
                None
            }
            BotModalOutcome::AttachDisclaimerAccepted { bot, allow_write } => {
                if let Some(bots_be) = backend.as_bots_backend_mut() {
                    bots_be.accept_attach_disclaimer(bot.bot_id);
                    let state = if allow_write {
                        AttachMenuToggleState::AllowedToWrite
                    } else {
                        AttachMenuToggleState::Added
                    };
                    let _ = bots_be.toggle_attach_menu_bot(bot.bot_id, state);
                }
                self.show_toast(format!("Launched Mini App: {}", bot.short_name), now_secs);
                Some(HistoryAction::OpenMiniApp {
                    bot_id: PeerId::from_user(bot.bot_id.0),
                    url: None,
                    start_param: None,
                    confirm_open: false,
                })
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_inner_events(
        &mut self,
        ctx: &egui::Context,
        events: Vec<InnerEvent>,
        peer_id: PeerId,
        peer_info: &HistoryPeerInfo,
        bot_snapshot: Option<&BotChatSnapshot>,
        backend: &mut dyn HistoryBackend,
        now_secs: f64,
        now_ms_u64: u64,
    ) -> Option<HistoryAction> {
        let mut out_action = None;
        for ev in events {
            match ev {
                InnerEvent::Reply(msg_id) => {
                    if let Some(target) = self.find_loaded_message(msg_id) {
                        self.compose.start_reply(
                            target.id,
                            target.sender_name.clone(),
                            target.preview_snippet(),
                        );
                    }
                }
                InnerEvent::DrawToReply(msg_id) => {
                    if let Some(target) = self.find_loaded_message(msg_id).cloned() {
                        let (w, h, fname, rgb) = match target.media {
                            Some(HistoryMedia::Photo(ref p)) => (
                                p.width.clamp(120, 480) as u32,
                                p.height.clamp(90, 360) as u32,
                                p.file_name.clone(),
                                p.fallback_rgb,
                            ),
                            _ => (
                                240,
                                180,
                                format!("reply_{}.jpg", msg_id.0),
                                ([0x4E, 0x90, 0xC8], [0x26, 0x4D, 0x73]),
                            ),
                        };
                        let mut img = image::RgbaImage::new(w, h);
                        for y in 0..h {
                            let t = y as f32 / h.max(1) as f32;
                            let r =
                                (f32::from(rgb.0[0]) * (1.0 - t) + f32::from(rgb.1[0]) * t) as u8;
                            let g =
                                (f32::from(rgb.0[1]) * (1.0 - t) + f32::from(rgb.1[1]) * t) as u8;
                            let b =
                                (f32::from(rgb.0[2]) * (1.0 - t) + f32::from(rgb.1[2]) * t) as u8;
                            for x in 0..w {
                                img.put_pixel(x, y, image::Rgba([r, g, b, 255]));
                            }
                        }
                        let req = tg_editor::DrawToReplyRequest {
                            peer_id: peer_id.0,
                            reply_to_msg_id: msg_id.0,
                            photo_id: Some(msg_id.0.max(0) as u64),
                            document_id: None,
                            file_name: fname,
                        };
                        self.draw_to_reply_editor = Some((
                            req.clone(),
                            PhotoEditorModalState::for_draw_to_reply(req, img),
                        ));
                    }
                }
                InnerEvent::Edit(msg_id) => {
                    if let Some(target) = self.find_loaded_message(msg_id).cloned() {
                        if let Some(ref rich) = target.extra.rich_message {
                            let page = rich.to_editable_page();
                            let mut editor =
                                tg_app_media::webview::rich_editor::RichEditorWindow::new_edit(
                                    peer_id, target.id, page,
                                );
                            editor.set_export_dir(self.rich_export_dir.clone());
                            editor.set_export_dialog(self.rich_export_dialog.clone());
                            self.rich_editor = Some(editor);
                        } else if target
                            .media
                            .as_ref()
                            .is_some_and(edit_caption::media_supports_edit_caption)
                        {
                            self.edit_caption_box =
                                Some(edit_caption::EditCaptionBoxState::from_message(
                                    peer_id,
                                    &target,
                                    self.is_premium,
                                ));
                        } else {
                            self.compose.start_edit_rich(
                                target.id,
                                target.text.clone(),
                                target.preview_snippet(),
                            );
                        }
                    }
                }
                InnerEvent::Forward(msg_ids) => {
                    let summary = if msg_ids.len() == 1 {
                        format!("1 message from {}", peer_info.summary.title)
                    } else {
                        format!(
                            "{} messages from {}",
                            msg_ids.len(),
                            peer_info.summary.title
                        )
                    };
                    self.compose.start_forward(peer_id, msg_ids, summary);
                }
                InnerEvent::TogglePin { msg_id, unpin } => {
                    if backend.pin_message(peer_id, msg_id, unpin).is_ok()
                        && let Some(mut copy) = self.find_loaded_message(msg_id).cloned()
                    {
                        copy.is_pinned = !unpin;
                        self.inner.update_message_in_place(copy);
                        self.pinned_bar_index = 0;
                        self.show_toast(
                            if unpin {
                                "Message unpinned"
                            } else {
                                "Message pinned"
                            },
                            now_secs,
                        );
                    }
                }
                InnerEvent::Delete(msg_ids) => {
                    let can_revoke = !peer_info.summary.is_saved_messages;
                    self.delete_box = Some(DeleteConfirmBoxState::new(
                        msg_ids,
                        peer_info.summary.title.clone(),
                        can_revoke,
                    ));
                }
                InnerEvent::Moderate(msg_ids) => {
                    let mut raw_ids = Vec::with_capacity(msg_ids.len());
                    let mut participants: Vec<
                        tg_app_info::boxes::moderate_messages::ModerateParticipant,
                    > = Vec::new();
                    for mid in msg_ids {
                        raw_ids.push(mid.0);
                        if let Some(msg) = self.find_loaded_message(mid) {
                            let author_id = msg.from_id;
                            let author_name = if msg.sender_name.trim().is_empty() {
                                format!("User {}", author_id.bare())
                            } else {
                                msg.sender_name.clone()
                            };
                            if let Some(existing) =
                                participants.iter_mut().find(|p| p.peer_id == author_id)
                            {
                                existing.selected_msg_ids.push(mid.0);
                            } else {
                                participants.push(
                                    tg_app_info::boxes::moderate_messages::ModerateParticipant {
                                        peer_id: author_id,
                                        name: author_name,
                                        username: None,
                                        selected_msg_ids: vec![mid.0],
                                        total_messages_count: None,
                                        can_ban: true,
                                        can_delete_all: true,
                                    },
                                );
                            }
                        }
                    }
                    if !raw_ids.is_empty() {
                        out_action = Some(HistoryAction::OpenModerateMessages {
                            peer_id,
                            peer_title: peer_info.summary.title.clone(),
                            message_ids: raw_ids,
                            participants,
                        });
                    }
                }
                InnerEvent::ToggleReaction { msg_id, emoji } => {
                    if let Ok(updated_reactions) = backend.toggle_reaction(peer_id, msg_id, &emoji)
                        && let Some(mut copy) = self.find_loaded_message(msg_id).cloned()
                    {
                        copy.reactions = updated_reactions;
                        self.inner.update_message_in_place(copy);
                        self.reactions_picker
                            .trigger_burst(msg_id, &emoji, now_secs);
                        if self.last_list_rect.is_positive() {
                            let target = Rect::from_center_size(
                                self.last_list_rect.center(),
                                Vec2::splat(20.0),
                            );
                            let from = Rect::from_center_size(
                                Pos2::new(target.center().x - 36.0, target.center().y - 28.0),
                                Vec2::splat(24.0),
                            );
                            self.fly_effects.push_reaction_fly(
                                tg_ui::effects::ReactionFlyArgs::new(emoji, from, target),
                                now_secs,
                                self.fly_policy(),
                            );
                        }
                    }
                }
                InnerEvent::VotePoll {
                    msg_id,
                    option_index,
                } => {
                    if backend.vote_poll(peer_id, msg_id, &[option_index]).is_ok() {
                        let refreshed = backend.load_slice_around(peer_id, msg_id, 0, 0);
                        if let Some(updated) =
                            refreshed.messages.into_iter().find(|m| m.id == msg_id)
                        {
                            self.inner.update_message_in_place(updated);
                        }
                    }
                }
                InnerEvent::Copied(msg) => self.show_toast(msg, now_secs),
                InnerEvent::OpenLink(url) => {
                    out_action = Some(HistoryAction::OpenLink(url));
                }
                InnerEvent::OpenHiddenLink(url) => {
                    out_action = Some(HistoryAction::OpenHiddenLink {
                        url,
                        force_confirm: false,
                    });
                }
                InnerEvent::CopiedRich { summary, rich } => {
                    self.compose.copied_rich_buffer = Some(rich);
                    self.show_toast(summary, now_secs);
                }
                InnerEvent::OpenMedia {
                    msg_id,
                    album_index,
                } => {
                    out_action = Some(HistoryAction::OpenMedia {
                        peer_id,
                        msg_id,
                        album_index,
                    });
                }
                InnerEvent::SaveAs { file_name, .. } => {
                    self.show_toast(format!("Save As: {file_name}"), now_secs);
                }
                InnerEvent::SaveGif(msg_id) => {
                    if self.try_save_gif(msg_id, now_secs)
                        && let Err(err) =
                            backend.toggle_saved_gif_from_message(peer_id, msg_id, true)
                    {
                        self.show_toast(err, now_secs);
                    }
                }
                InnerEvent::FaveSticker(msg_id) => {
                    match backend.toggle_faved_sticker_from_message(peer_id, msg_id, true) {
                        Ok(_) => self.show_toast("Sticker added to favorites", now_secs),
                        Err(err) => self.show_toast(err, now_secs),
                    }
                }
                InnerEvent::SaveRingtone(msg_id) => {
                    match backend.toggle_saved_ringtone_from_message(peer_id, msg_id, true) {
                        Ok(_) => self.show_toast("Saved for notifications", now_secs),
                        Err(err) => self.show_toast(err, now_secs),
                    }
                }
                InnerEvent::TapInteractiveEmoji {
                    msg_id,
                    emoticon,
                    anchor,
                    outgoing,
                } => {
                    let server_variants = backend.emoji_interaction_variant_count(&emoticon);
                    let plays = self.emoji_coordinator.start_outgoing_with_variants(
                        peer_id,
                        msg_id,
                        &emoticon,
                        server_variants,
                        now_ms_u64,
                    );
                    for play in plays {
                        let lottie = backend
                            .resolve_emoji_interaction_lottie(&play.emoticon, play.variant_index);
                        self.inner.play_emoji_interaction_with_lottie(
                            play.msg_id,
                            play.emoticon,
                            Some(anchor),
                            outgoing,
                            play.variant_index,
                            lottie,
                            now_ms_u64,
                        );
                    }
                }
                InnerEvent::RespondNoForwardsRequest { msg_id, accept } => {
                    match backend.toggle_peer_no_forwards(peer_id, !accept, Some(msg_id)) {
                        Ok(()) => {
                            self.show_toast(
                                if accept {
                                    "Sharing enabled in this chat"
                                } else {
                                    "Sharing request declined"
                                },
                                now_secs,
                            );
                            self.reload_active_peer(backend);
                        }
                        Err(err) => self.show_toast(err, now_secs),
                    }
                }
                InnerEvent::ToggleSummary(msg_id) => {
                    match backend.toggle_message_summary(peer_id, msg_id, None) {
                        Ok(entry) => {
                            self.inner.invalidate_message_layout(msg_id);
                            if entry.shown {
                                self.inner.highlight_message(msg_id);
                            }
                        }
                        Err(err) => {
                            self.inner.invalidate_message_layout(msg_id);
                            self.show_toast(err, now_secs);
                        }
                    }
                }
                InnerEvent::AboutAiSummary(_msg_id) => {
                    self.about_cocoon_box = Some(AboutCocoonBoxState::new());
                }
                InnerEvent::ApplySuggestedBirthday {
                    day, month, year, ..
                } => match backend.apply_suggested_birthday(day, month, year) {
                    Ok(()) => {
                        self.show_toast("Birthday updated on your profile", now_secs);
                    }
                    Err(err) => self.show_toast(err, now_secs),
                },
                InnerEvent::ApplySuggestedUserpic {
                    msg_id,
                    photo_id,
                    access_hash,
                    file_reference,
                    is_video,
                } => {
                    match backend.apply_suggested_profile_photo(
                        peer_id,
                        msg_id,
                        photo_id,
                        access_hash,
                        &file_reference,
                        is_video,
                    ) {
                        Ok(()) => {
                            self.show_toast(
                                tg_lang::tr::lng_profile_changed_photo_title(),
                                now_secs,
                            );
                        }
                        Err(err) => self.show_toast(err, now_secs),
                    }
                }
                InnerEvent::ViewSuggestedUserpic(msg_id) => {
                    out_action = Some(HistoryAction::OpenMedia {
                        peer_id,
                        msg_id,
                        album_index: None,
                    });
                }
                InnerEvent::ReplyWithQuote { msg_id, quote } => {
                    if let Some(target) = self.find_loaded_message(msg_id) {
                        self.compose.start_reply_with_quote(
                            target.id,
                            target.sender_name.clone(),
                            target.preview_snippet(),
                            quote,
                        );
                    }
                }
                InnerEvent::Translate { msg_id, clear } => {
                    if clear {
                        backend.clear_message_translation(peer_id, msg_id);
                        if let Some(mut copy) = self.find_loaded_message(msg_id).cloned() {
                            copy.extra.translated_text = None;
                            self.inner.update_message_in_place(copy);
                        }
                    } else {
                        let lang = backend.target_translation_language();
                        match backend.translate_message(peer_id, msg_id, &lang) {
                            Ok(tr) => {
                                if let Some(mut copy) = self.find_loaded_message(msg_id).cloned() {
                                    copy.extra.translated_text = Some((lang, tr));
                                    self.inner.update_message_in_place(copy);
                                }
                            }
                            Err(err) => self.show_toast(err, now_secs),
                        }
                    }
                }
                InnerEvent::OpenTranslateModal(msg_id) => {
                    if let Some(msg) = self.find_loaded_message(msg_id).cloned() {
                        let lang = backend.target_translation_language();
                        self.translate_box = Some(TranslateBoxState::new(
                            Some(peer_id),
                            Some(msg_id),
                            msg.text,
                            lang,
                        ));
                    }
                }
                InnerEvent::OpenAddPollOption(msg_id) => {
                    if let Some(msg) = self.find_loaded_message(msg_id)
                        && let Some(HistoryMedia::Poll(ref poll)) = msg.media
                    {
                        self.add_poll_option_box =
                            Some(AddPollOptionBoxState::new(msg_id, poll.question.clone()));
                    }
                }
                InnerEvent::OpenViewOnceMedia {
                    msg_id,
                    kind,
                    ttl_seconds,
                } => {
                    if let Some(msg) = self.find_loaded_message(msg_id).cloned() {
                        let preview_rgb = match &msg.media {
                            Some(HistoryMedia::Photo(p)) => p.fallback_rgb,
                            Some(HistoryMedia::Video(v)) => v.fallback_rgb,
                            Some(HistoryMedia::RoundVideo(rv)) => rv.accent_rgb,
                            _ => ([0x2A, 0x52, 0x7A], [0x58, 0xA6, 0xE8]),
                        };
                        self.view_once_modal = Some(ViewOnceMediaModalState::open(
                            peer_id,
                            msg_id,
                            msg.sender_name,
                            kind,
                            ttl_seconds,
                            msg.text.text,
                            preview_rgb,
                            now_secs,
                        ));
                    }
                }
                InnerEvent::Report(msg_ids) => {
                    self.report_box = Some(ReportMessageBoxState::new(msg_ids));
                }
                InnerEvent::BlockFromReplies {
                    msg_id,
                    sender_name,
                } => {
                    self.block_from_replies_box =
                        Some(BlockFromRepliesBoxState::new(msg_id, sender_name));
                }
                InnerEvent::OpenReactionsPicker { msg_id, pos } => {
                    self.reactions_picker.open(msg_id, pos);
                }
                InnerEvent::OpenWhoReacted(msg_id) => {
                    let summary = self
                        .find_loaded_message(msg_id)
                        .map(|m| m.reactions.clone())
                        .unwrap_or_default();
                    let can_moderate = peer_info.can_delete
                        && matches!(peer_info.summary.kind, PeerKind::Group | PeerKind::Channel);
                    let has_paid = summary.iter().any(|r| r.emoji == "⭐" || r.emoji == "★");
                    let paid_privacy = if has_paid {
                        Some(
                            backend
                                .as_chat_helpers_backend()
                                .and_then(|ch| ch.paid_reaction_privacy(false).ok())
                                .unwrap_or(
                                    tg_app_core::chat_helpers::backend::PaidReactionPrivacy::Default,
                                ),
                        )
                    } else {
                        None
                    };
                    match backend.message_reactions_list(
                        peer_id,
                        msg_id,
                        None,
                        None,
                        PEER_LIST_PAGE_LIMIT,
                    ) {
                        Ok(page) => {
                            let modal = ReactionsListWidget::new(peer_id, msg_id, summary, page)
                                .with_moderation(can_moderate)
                                .with_paid_privacy(paid_privacy);
                            self.who_reacted_modal = Some(modal);
                        }
                        Err(err) => self.show_toast(err, now_secs),
                    }
                }
                InnerEvent::TogglePaidReactionPrivacy(msg_id) => {
                    if let Some(ch) = backend.as_chat_helpers_backend_mut() {
                        let current = ch.paid_reaction_privacy(false).unwrap_or(
                            tg_app_core::chat_helpers::backend::PaidReactionPrivacy::Default,
                        );
                        let next = match current {
                            tg_app_core::chat_helpers::backend::PaidReactionPrivacy::Anonymous => {
                                tg_app_core::chat_helpers::backend::PaidReactionPrivacy::Default
                            }
                            _ => tg_app_core::chat_helpers::backend::PaidReactionPrivacy::Anonymous,
                        };
                        match ch.toggle_paid_reaction_privacy(peer_id, msg_id, next) {
                            Ok(_) => {
                                let label = if matches!(
                                    next,
                                    tg_app_core::chat_helpers::backend::PaidReactionPrivacy::Anonymous
                                ) {
                                    "Paid reaction set to Anonymous"
                                } else {
                                    "Shown in Paid Top Senders"
                                };
                                self.show_toast(label, now_secs);
                            }
                            Err(err) => self.show_toast(err, now_secs),
                        }
                    }
                }
                InnerEvent::ReportMessageReaction(msg_id) => {
                    let target_peer = backend
                        .message_reactions_list(peer_id, msg_id, None, None, 10)
                        .ok()
                        .and_then(|page| {
                            page.entries
                                .into_iter()
                                .find(|e| !e.is_my)
                                .map(|e| e.peer_id)
                        })
                        .or_else(|| self.find_loaded_message(msg_id).map(|m| m.from_id));
                    if let Some(reaction_peer_id) = target_peer
                        && let Some(ch) = backend.as_chat_helpers_backend()
                    {
                        match ch.report_reaction(peer_id, msg_id, reaction_peer_id) {
                            Ok(_) => self.show_toast(tg_lang::tr::lng_report_spam_done(), now_secs),
                            Err(err) => self.show_toast(err, now_secs),
                        }
                    }
                }
                InnerEvent::RemoveParticipantReactions {
                    msg_id,
                    participant_id,
                } => {
                    let resolved_participant = backend
                        .message_reactions_list(peer_id, msg_id, None, None, 10)
                        .ok()
                        .and_then(|page| {
                            page.entries
                                .into_iter()
                                .find(|e| !e.is_my)
                                .map(|e| e.peer_id)
                        })
                        .unwrap_or(participant_id);
                    if let Some(ch) = backend.as_chat_helpers_backend_mut() {
                        match ch.delete_participant_reactions(peer_id, resolved_participant) {
                            Ok(_) => {
                                self.show_toast("Removed participant reactions", now_secs);
                            }
                            Err(err) => self.show_toast(err, now_secs),
                        }
                    }
                }
                InnerEvent::OpenSeenBy { msg_id, readers } => {
                    let ref_now_unix = self
                        .find_loaded_message(msg_id)
                        .map_or(now_secs.round() as i64, |m| {
                            (now_secs.round() as i64).max(i64::from(m.date_unix) + 60)
                        });
                    self.seen_by_modal = Some(read_receipts::SeenByModalWidget::new(
                        peer_id,
                        msg_id,
                        readers,
                        ref_now_unix,
                    ));
                }
                InnerEvent::OpenReadDateStatus(status) => {
                    let msg = match status {
                        tg_app_core::core_types::dto::engagement::OutboxReadDateStatus::Read {
                            date_unix,
                        } => {
                            read_receipts::format_read_timestamp(
                                date_unix,
                                (now_secs.round() as i64).max(i64::from(date_unix) + 60),
                            )
                        }
                        tg_app_core::core_types::dto::engagement::OutboxReadDateStatus::MyPrivacyHidden => {
                            "Enable Last Seen & Read Time in Privacy Settings to see exact read times."
                                .to_owned()
                        }
                        tg_app_core::core_types::dto::engagement::OutboxReadDateStatus::PeerPrivacyHidden => {
                            "Read time is hidden by this user's privacy settings.".to_owned()
                        }
                        tg_app_core::core_types::dto::engagement::OutboxReadDateStatus::TooOld
                        | tg_app_core::core_types::dto::engagement::OutboxReadDateStatus::Unavailable => {
                            "Read time is no longer available for this message.".to_owned()
                        }
                    };
                    self.show_toast(msg, now_secs);
                }
                InnerEvent::CopyMessageLink(msg_id) => {
                    let is_album = self
                        .find_loaded_message(msg_id)
                        .is_some_and(|m| matches!(m.media, Some(HistoryMedia::Album(_))));
                    let exported =
                        if matches!(peer_info.summary.kind, PeerKind::Channel | PeerKind::Group) {
                            backend
                                .export_message_link(peer_id, msg_id, is_album, false)
                                .ok()
                                .map(|info| info.link)
                                .filter(|s| !s.is_empty())
                        } else {
                            None
                        };
                    let link =
                        exported.unwrap_or_else(|| format!("{}/{}", peer_info.link_base, msg_id.0));
                    ctx.copy_text(link.clone());
                    self.show_toast(format!("Copied link: {link}"), now_secs);
                }
                InnerEvent::OpenMessageAuthor(author) => {
                    if author.is_deleted {
                        self.show_toast("Author account was deleted", now_secs);
                    } else {
                        out_action = Some(HistoryAction::OpenPeerProfile(author.user_id));
                    }
                }
                InnerEvent::OpenPollVoters(msg_id) => {
                    if let Some(msg) = self.find_loaded_message(msg_id)
                        && let Some(HistoryMedia::Poll(ref poll)) = msg.media
                    {
                        let poll_clone = poll.clone();
                        match backend.poll_votes(peer_id, msg_id, None, None, PEER_LIST_PAGE_LIMIT)
                        {
                            Ok(page) => {
                                self.poll_voters_modal =
                                    Some(PollVotersWidget::new(peer_id, msg_id, poll_clone, page));
                            }
                            Err(err) => self.show_toast(err, now_secs),
                        }
                    }
                }
                InnerEvent::RetractPollVote(msg_id) => {
                    match backend.retract_poll_vote(peer_id, msg_id) {
                        Ok(()) => {
                            let refreshed = backend.load_slice_around(peer_id, msg_id, 0, 0);
                            if let Some(updated) =
                                refreshed.messages.into_iter().find(|m| m.id == msg_id)
                            {
                                self.inner.update_message_in_place(updated);
                            }
                            self.show_toast("Vote retracted", now_secs);
                        }
                        Err(err) => self.show_toast(err, now_secs),
                    }
                }
                InnerEvent::StopPoll(msg_id) => match backend.stop_poll(peer_id, msg_id) {
                    Ok(()) => {
                        let refreshed = backend.load_slice_around(peer_id, msg_id, 0, 0);
                        if let Some(updated) =
                            refreshed.messages.into_iter().find(|m| m.id == msg_id)
                        {
                            self.inner.update_message_in_place(updated);
                        }
                        self.show_toast("Poll stopped", now_secs);
                    }
                    Err(err) => self.show_toast(err, now_secs),
                },
                InnerEvent::OpenComments(root_msg_id) => {
                    self.active_section = Some(HistorySectionKind::Replies {
                        peer_id,
                        root_msg_id,
                    });
                }
                InnerEvent::InlineKeyboard(act) => {
                    if let Some(kb_act) = self.handle_inline_keyboard_activation(
                        ctx,
                        act,
                        peer_id,
                        peer_info,
                        bot_snapshot,
                        backend,
                        now_secs,
                        now_ms_u64,
                    ) {
                        out_action = Some(kb_act);
                    }
                }
                InnerEvent::PlayGame(msg_id) => {
                    let bot_full = to_bot_full(FullMsgId {
                        peer: peer_id,
                        msg: msg_id,
                    });
                    let req = CallbackQueryRequest {
                        peer_id: bot_full.peer,
                        msg_id: bot_full.msg,
                        is_game: true,
                        data: None,
                        requires_password: false,
                        ephemeral_id: None,
                    };
                    let bot_id = bot_snapshot
                        .map(|s| PeerId::from_user(s.bot_id.0))
                        .or_else(|| self.find_loaded_message(msg_id).map(|m| m.from_id))
                        .unwrap_or(peer_id);
                    let bot_name = bot_snapshot
                        .map(|s| s.bot_name.clone())
                        .unwrap_or_else(|| peer_info.summary.title.clone());
                    let bot_verified = bot_snapshot.is_some_and(|s| s.bot_verified);
                    let bot_trusted_for_game = bot_snapshot.is_some_and(|s| s.bot_trusted_for_game);
                    self.inner
                        .inline_kb_state_mut()
                        .set_loading(bot_full, 0, 0, now_secs);
                    self.pending_callbacks.push(PendingHistoryCallback {
                        msg_id: bot_full,
                        row: 0,
                        col: 0,
                        request: req,
                        bot_id,
                        bot_name,
                        bot_verified,
                        bot_trusted_for_game,
                        deliver_at_ms: now_ms_u64.saturating_add(CALLBACK_SPINNER_DELAY_MS),
                    });
                }
                InnerEvent::ToggleTodoItem { msg_id, item_id } => {
                    let currently_done = self
                        .find_loaded_message(msg_id)
                        .and_then(|m| match &m.media {
                            Some(HistoryMedia::TodoList(todo)) => todo
                                .items
                                .iter()
                                .find(|i| i.id == item_id)
                                .map(todo::TodoTaskItem::is_completed),
                            _ => None,
                        })
                        .unwrap_or(false);
                    if let Some(ex) = backend.as_layer233_extras_mut() {
                        let (comp, incomp) = if currently_done {
                            (Vec::new(), vec![item_id])
                        } else {
                            (vec![item_id], Vec::new())
                        };
                        match ex.toggle_todo_completed(peer_id, msg_id, &comp, &incomp) {
                            Ok(Some(updated)) => self.inner.update_message_in_place(updated),
                            Ok(None) => {}
                            Err(err) => self.show_toast(err, now_secs),
                        }
                    }
                }
                InnerEvent::AddTodoTasks(msg_id) => {
                    if let Some(HistoryMedia::TodoList(todo)) = self
                        .find_loaded_message(msg_id)
                        .and_then(|m| m.media.as_ref())
                        .cloned()
                    {
                        self.add_todo_tasks_box =
                            Some(todo::AddTodoListTasksBoxState::for_checklist(msg_id, &todo));
                    }
                }
                InnerEvent::EditTodoList(msg_id) => {
                    if let Some(HistoryMedia::TodoList(todo)) = self
                        .find_loaded_message(msg_id)
                        .and_then(|m| m.media.as_ref())
                        .cloned()
                    {
                        self.todo_box = Some(todo::EditTodoListBoxState::new_edit(msg_id, &todo));
                    }
                }
                InnerEvent::EditFactCheck(msg_id) => {
                    let current = backend
                        .as_layer233_extras()
                        .and_then(|ex| ex.cached_factcheck(peer_id, msg_id));
                    self.open_edit_factcheck_box(msg_id, current.as_ref());
                }
                InnerEvent::ToggleTranscribe {
                    msg_id,
                    duration_secs,
                    is_round_video,
                } => {
                    match backend.toggle_transcribe_audio(
                        peer_id,
                        msg_id,
                        duration_secs,
                        is_round_video,
                    ) {
                        TranscribeToggleOutcome::Ready {
                            trial_toast: Some(msg),
                            ..
                        } => self.show_toast(msg, now_secs),
                        TranscribeToggleOutcome::PremiumRequired { .. } => {
                            self.show_toast(
                                "Weekly free transcriptions used up. Subscribe to Telegram Premium for unlimited voice-to-text.",
                                now_secs,
                            );
                        }
                        TranscribeToggleOutcome::TooLong { max_secs } => {
                            self.show_toast(
                                format!(
                                    "Audio is longer than the {max_secs}s free transcription limit. Telegram Premium removes this limit."
                                ),
                                now_secs,
                            );
                        }
                        TranscribeToggleOutcome::Failed(err)
                            if !err.contains("not available for this backend") =>
                        {
                            self.show_toast(format!("Transcription failed: {err}"), now_secs);
                        }
                        _ => {}
                    }
                }
                InnerEvent::RateTranscribe { msg_id, good } => {
                    if backend
                        .rate_transcribed_audio(peer_id, msg_id, good)
                        .is_ok()
                    {
                        self.show_toast(
                            if good {
                                "Thanks for your positive transcription feedback!"
                            } else {
                                "Thanks for your transcription feedback!"
                            },
                            now_secs,
                        );
                    }
                }
                InnerEvent::OpenWidget(msg_id) => {
                    if let Some(msg) = self.find_loaded_message(msg_id).cloned()
                        && let Some(HistoryMedia::Widget(ref w)) = msg.media
                    {
                        let html_bytes = w.html.as_deref().map(str::as_bytes).unwrap_or(&w.payload);
                        let mut host = tg_app_media::webview::widget_host::LiveWidgetHost::new(
                            peer_id, msg_id, html_bytes, false,
                        );
                        if let Some(ref helper_path) = self.widget_helper_path {
                            host.set_helper_executable(helper_path.clone());
                        }
                        if let Some(ref html) = w.html {
                            if let Err(err) = host.launch_webview(html) {
                                self.show_toast(format!("Widget sandbox: {err}"), now_secs);
                            } else {
                                self.show_toast(format!("Launched widget: {}", w.title), now_secs);
                            }
                        } else {
                            host.state_machine
                                .fail("Widget payload could not be unpacked");
                            self.show_toast("Widget payload could not be unpacked", now_secs);
                        }
                        self.active_widget_title = w.title.clone();
                        self.active_widget_query_status = None;
                        self.active_widget_host = Some(host);
                    }
                }
                InnerEvent::OpenInvoice { msg_id, invoice_id } => {
                    out_action = Some(HistoryAction::OpenInvoice {
                        peer_id,
                        msg_id,
                        invoice_id,
                    });
                }
                InnerEvent::UnlockPaidMedia {
                    msg_id,
                    stars_amount,
                    title,
                } => {
                    out_action = Some(HistoryAction::UnlockPaidMedia {
                        peer_id,
                        msg_id,
                        stars_amount,
                        title,
                    });
                }
                InnerEvent::OpenStory { peer_id, story_id } => {
                    out_action = Some(HistoryAction::OpenStory { peer_id, story_id });
                }
                InnerEvent::OpenGiveawayInfo {
                    peer_id,
                    msg_id,
                    channel_id,
                    launch_msg_id,
                } => {
                    out_action = Some(HistoryAction::OpenGiveawayInfo {
                        peer_id,
                        msg_id,
                        channel_id,
                        launch_msg_id,
                    });
                }
                InnerEvent::OpenVideoStream {
                    msg_id,
                    call_id,
                    access_hash,
                    rtmp_stream,
                    title,
                } => {
                    out_action = Some(HistoryAction::OpenVideoStream {
                        peer_id,
                        msg_id,
                        call_id,
                        access_hash,
                        rtmp_stream,
                        title,
                    });
                }
            }
        }
        out_action
    }

    #[allow(clippy::too_many_arguments)]
    fn show_active_modals(
        &mut self,
        ui: &mut Ui,
        peer_id: PeerId,
        peer_info: &HistoryPeerInfo,
        backend: &mut dyn HistoryBackend,
        now_secs: f64,
        palette: &Palette,
        colors: &HistoryColors,
    ) -> Option<HistoryAction> {
        let mut out_action: Option<HistoryAction> = None;
        if let Some(ref mut box_state) = self.schedule_box
            && let Some(ev) = box_state.show(ui, colors, false)
        {
            if let ScheduleBoxAction::Confirmed { target, .. } = ev {
                let mut req = self.compose.build_pending_extended(None);
                req.schedule_date = Some(target.to_time_id());
                if !req.text.text.is_empty() && backend.send_message_extended(peer_id, req).is_ok()
                {
                    self.compose.clear_after_send();
                    self.show_toast(
                        crate::history::sections_model::format_schedule_badge(target.to_time_id()),
                        now_secs,
                    );
                }
            }
            self.schedule_box = None;
        }

        if let Some(ref mut widget_host) = self.active_widget_host {
            let mut close_widget = false;
            let mut run_query = false;
            let mut poll_public = false;
            let mut toast_to_show: Option<String> = None;
            let w_peer = widget_host.peer_id;
            let w_msg = widget_host.msg_id;

            // Drain any inbound bridge messages emitted by the sandboxed WebView2 page.
            let inbound = widget_host.drain_messages();
            for msg in inbound {
                match msg {
                    tg_tgw::RendererToHostMsg::Size { height } => {
                        widget_host.on_resize(height.round() as i32);
                    }
                    tg_tgw::RendererToHostMsg::CopyText { text } => {
                        match tg_tgw::evaluate_copy_text(&text, true) {
                            tg_tgw::CopyTextDecision::Copy { text, note, .. } => {
                                if !text.is_empty() {
                                    ui.ctx().copy_text(text);
                                    toast_to_show = Some(note);
                                }
                            }
                            tg_tgw::CopyTextDecision::Refused(reason) => {
                                toast_to_show = Some(reason);
                            }
                        }
                    }
                    tg_tgw::RendererToHostMsg::OpenLink { url, telegram } => {
                        match route_widget_open_link(&url, telegram) {
                            Ok(action) => out_action = Some(action),
                            Err(reason) => toast_to_show = Some(reason),
                        }
                    }
                    tg_tgw::RendererToHostMsg::BotQuery {
                        request_id,
                        data,
                        wallet,
                    } => {
                        if let Ok(outcome) =
                            backend.widget_query(w_peer, w_msg, data.as_bytes(), wallet)
                        {
                            self.active_widget_query_status = Some(
                                outcome
                                    .answer
                                    .result
                                    .clone()
                                    .unwrap_or_else(|| format!("{:?}", outcome.failure)),
                            );
                            let _ = widget_host.send_host_message(
                                &tg_tgw::HostToRendererMsg::QueryAnswer {
                                    request_id,
                                    outcome,
                                },
                            );
                        }
                    }
                    tg_tgw::RendererToHostMsg::Stopped { reason } => {
                        widget_host.state_machine.fail(reason);
                    }
                    _ => {}
                }
            }

            let w_state = widget_host.state();
            let w_title = self.active_widget_title.clone();
            let w_status = self.active_widget_query_status.clone();
            egui::Window::new(format!("Sandboxed Widget — {w_title}"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ui.ctx(), |ui| {
                    ui.label(format!("Runtime state: {w_state:?}"));
                    ui.label("Sandbox: strict CSP (no network, no storage, isolated origin)");
                    if let Some(reason) = widget_host.state_machine.stop_reason() {
                        ui.label(format!("Notice: {reason}"));
                    }
                    if let Some(ref status) = w_status {
                        ui.label(format!("Last query: {status}"));
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Send Widget Query").clicked() {
                            run_query = true;
                        }
                        if ui.button("Poll Public Data").clicked() {
                            poll_public = true;
                        }
                        if ui.button("Close").clicked() {
                            close_widget = true;
                        }
                    });
                });
            if run_query {
                match backend.widget_query(w_peer, w_msg, b"ping", false) {
                    Ok(outcome) => {
                        self.active_widget_query_status = Some(
                            outcome
                                .answer
                                .result
                                .clone()
                                .unwrap_or_else(|| format!("{:?}", outcome.failure)),
                        );
                        let _ = widget_host.send_host_message(
                            &tg_tgw::HostToRendererMsg::QueryAnswer {
                                request_id: 0,
                                outcome,
                            },
                        );
                    }
                    Err(err) => {
                        self.active_widget_query_status = Some(err);
                    }
                }
            }
            if poll_public {
                match backend.widget_poll_public_data(w_peer, w_msg, 0) {
                    Ok(tg_tgw::PublicDataPollResult::Updated { data, .. }) => {
                        self.active_widget_query_status = Some(format!("Public data: {data}"));
                        let _ = widget_host.send_host_message(&tg_tgw::HostToRendererMsg::Data {
                            chat_data: None,
                            public_data: Some(data),
                        });
                    }
                    Ok(tg_tgw::PublicDataPollResult::NotModified { period_ms }) => {
                        self.active_widget_query_status =
                            Some(format!("Not modified (next poll {}s)", period_ms / 1000));
                    }
                    Err(err) => {
                        self.active_widget_query_status = Some(err);
                    }
                }
            }
            if close_widget {
                widget_host.close();
                self.active_widget_host = None;
            }
            if let Some(toast) = toast_to_show {
                self.show_toast(toast, now_secs);
            }
        }

        if let Some(ref mut box_state) = self.poll_box
            && let Some(ev) = box_state.show(ui, colors, false)
        {
            match ev {
                CreatePollBoxAction::Submit(req) => {
                    match backend.send_poll(peer_id, Some(ThreadKey::Main(peer_id)), req) {
                        Ok(msg) => {
                            self.inner.append_message(msg, peer_info.summary.kind);
                            self.show_toast("Poll created", now_secs);
                        }
                        Err(err) => self.show_toast(err, now_secs),
                    }
                }
                CreatePollBoxAction::SubmitWithExtras(req, extras) => {
                    match backend.send_poll_with_extras(
                        peer_id,
                        Some(ThreadKey::Main(peer_id)),
                        req,
                        extras,
                    ) {
                        Ok(msg) => {
                            self.inner.append_message(msg, peer_info.summary.kind);
                            self.show_toast("Poll created", now_secs);
                        }
                        Err(err) => self.show_toast(err, now_secs),
                    }
                }
                CreatePollBoxAction::Cancelled => {}
            }
            self.poll_box = None;
        }

        if let Some(ref mut box_state) = self.add_poll_option_box
            && let Some(ev) = box_state.show(ui, colors)
        {
            if let AddPollOptionAction::Submit { msg_id, request } = ev {
                match backend.add_poll_option(peer_id, msg_id, request) {
                    Ok(updated) => {
                        self.inner.update_message_in_place(updated);
                        self.show_toast("Poll option added", now_secs);
                    }
                    Err(err) => self.show_toast(err, now_secs),
                }
            }
            self.add_poll_option_box = None;
        }

        if let Some(ref mut editor) = self.rich_editor
            && let Some(ev) = editor.show_window(ui.ctx(), colors.is_night)
        {
            if let tg_app_media::webview::rich_editor::RichEditorAction::Submit {
                peer_id: target_peer,
                edit_msg_id,
                input,
                page,
            } = ev
            {
                let res = if let Some(mid) = edit_msg_id {
                    backend.edit_rich_message(target_peer, mid, input, page)
                } else {
                    let reply_to = match self.compose.mode {
                        crate::history::compose::ComposeMode::Reply { msg_id, .. } => Some(msg_id),
                        _ => None,
                    };
                    backend.send_rich_message(
                        target_peer,
                        input,
                        page,
                        reply_to,
                        self.compose.silent_mode,
                    )
                };
                match res {
                    Ok(msg) => {
                        if edit_msg_id.is_some() {
                            self.inner.update_message_in_place(msg);
                            self.show_toast("Rich message updated", now_secs);
                        } else {
                            self.inner.append_message(msg, peer_info.summary.kind);
                            self.show_toast("Rich message sent", now_secs);
                        }
                    }
                    Err(err) => self.show_toast(err, now_secs),
                }
            }
            self.rich_editor = None;
        }

        if let Some(ref mut box_state) = self.location_box {
            let _ = box_state.poll_static_map_tile();
            if box_state.needs_tile_fetch() {
                let call = backend.load_static_map_tile_async(
                    box_state.lat,
                    box_state.lon,
                    compose_boxes::LOCATION_MAP_TILE_WIDTH,
                    compose_boxes::LOCATION_MAP_TILE_HEIGHT,
                    box_state.zoom,
                    1,
                    Some(ui.ctx().clone()),
                );
                box_state.start_static_map_tile_fetch(call);
                let _ = box_state.poll_static_map_tile();
            }
            if box_state.take_venues_dirty() {
                box_state.venues = backend.nearby_venues_for_coords(
                    box_state.lat,
                    box_state.lon,
                    &box_state.search_query,
                );
            }
            if let Some(ev) = box_state.show(ui, colors) {
                if let LocationPickerAction::Send(loc) = ev {
                    let req = SendMessageExtended {
                        media: Some(HistoryMedia::Location(loc)),
                        silent: self.compose.silent_mode,
                        ..SendMessageExtended::default()
                    };
                    if let Ok(sent) = backend.send_message_extended(peer_id, req) {
                        self.inner.append_message(sent, peer_info.summary.kind);
                    }
                }
                self.location_box = None;
            }
        }

        if let Some(ref mut box_state) = self.contact_box
            && let Some(ev) = box_state.show(ui, colors)
        {
            if let ShareContactAction::Share(contact) = ev {
                let req = SendMessageExtended {
                    media: Some(HistoryMedia::Contact(contact)),
                    silent: self.compose.silent_mode,
                    ..SendMessageExtended::default()
                };
                if let Ok(sent) = backend.send_message_extended(peer_id, req) {
                    self.inner.append_message(sent, peer_info.summary.kind);
                }
            }
            self.contact_box = None;
        }

        if let Some(ref mut box_state) = self.attach_box
            && let Some(ev) = box_state.show(ui, colors)
        {
            match ev {
                AttachFilesAction::Send(sub) => {
                    if !self.check_caption_length_limit(&sub.caption) {
                        // Opened PremiumLimitsView(CaptionLength); keep attach box open.
                    } else {
                        let mut caption_pending = (!sub.caption.is_empty()).then(|| {
                            crate::history::text_bridge::parse_compose_markdown(&sub.caption)
                        });
                        for group in sub.groups {
                            if group.files.len() > 1
                                && group.album_type == tg_chat::media_prepare::AlbumType::PhotoVideo
                            {
                                let items = group
                                    .files
                                    .iter()
                                    .enumerate()
                                    .map(|(i, f)| AlbumItem {
                                        msg_id: MsgId(9800 + i as i32),
                                        width: f.shown_dimensions.width.max(320),
                                        height: f.shown_dimensions.height.max(240),
                                        stripped_thumb: None,
                                        fallback_rgb: ([0x4A, 0x8F, 0xC4], [0x2A, 0x50, 0x7A]),
                                        video_duration_secs: None,
                                        label: f.path.clone(),
                                    })
                                    .collect();
                                let req = SendMessageExtended {
                                    text: caption_pending.take().unwrap_or_default(),
                                    media: Some(HistoryMedia::Album(items)),
                                    silent: self.compose.silent_mode,
                                    ..SendMessageExtended::default()
                                };
                                if let Ok(sent) = backend.send_message_extended(peer_id, req) {
                                    self.inner.append_message(sent, peer_info.summary.kind);
                                }
                            } else {
                                for file in group.files {
                                    let media = if sub.way.send_images_as_photos()
                                        && file.file_type
                                            == tg_chat::media_prepare::PreparedFileType::Photo
                                    {
                                        HistoryMedia::Photo(PhotoAttachment {
                                            width: file.shown_dimensions.width.max(320),
                                            height: file.shown_dimensions.height.max(240),
                                            stripped_thumb: None,
                                            fallback_rgb: ([0x4E, 0x90, 0xC8], [0x26, 0x4D, 0x73]),
                                            file_name: file.path.clone(),
                                            size_bytes: file.size.max(0) as u64,
                                        })
                                    } else if file.file_type
                                        == tg_chat::media_prepare::PreparedFileType::Video
                                        || file.mime.starts_with("video/")
                                    {
                                        let vmods = sub.edited_videos.get(&file.path);
                                        let dur_secs = vmods.map_or(12, |m| {
                                            if m.till_ms > m.from_ms {
                                                ((m.till_ms - m.from_ms) / 1000).max(1) as u32
                                            } else {
                                                12
                                            }
                                        });
                                        HistoryMedia::Video(VideoAttachment {
                                            width: file.shown_dimensions.width.max(320),
                                            height: file.shown_dimensions.height.max(180),
                                            duration_secs: dur_secs,
                                            size_bytes: file.size.max(0) as u64,
                                            is_gif: vmods.is_some_and(|m| m.gif),
                                            stripped_thumb: None,
                                            fallback_rgb: ([0x3D, 0x6E, 0x9E], [0x1D, 0x35, 0x52]),
                                            file_name: file.path.clone(),
                                        })
                                    } else {
                                        HistoryMedia::Document(DocumentAttachment {
                                            file_name: file.path.clone(),
                                            size_bytes: file.size.max(0) as u64,
                                            mime_type: file.mime.clone(),
                                            thumb_rgb: None,
                                        })
                                    };
                                    let video_cover_jpeg =
                                        sub.video_covers.get(&file.path).cloned();
                                    let req = SendMessageExtended {
                                        text: caption_pending.take().unwrap_or_default(),
                                        media: Some(media),
                                        silent: self.compose.silent_mode,
                                        ttl_seconds: sub.ttl_seconds,
                                        video_cover_jpeg,
                                        ..SendMessageExtended::default()
                                    };
                                    if let Ok(sent) = backend.send_message_extended(peer_id, req) {
                                        self.inner.append_message(sent, peer_info.summary.kind);
                                    }
                                }
                            }
                        }
                        self.show_toast("Files sent", now_secs);
                        self.attach_box = None;
                    }
                }
                AttachFilesAction::StashFiles => {
                    let stash_key = compose_stash::ComposeStashKey::main(peer_id);
                    match compose_stash::take_stash_from_box(box_state) {
                        Ok(mut stashed) => {
                            // Match `StashManager::takeFromBox`: carry active composer
                            // reply/forward into the stash, clear composer, store stash,
                            // close the box, and restore any previously stashed draft.
                            if let Some(current) = self.take_current_compose_for_stash() {
                                if stashed.draft.reply_to.is_none() {
                                    stashed.draft.reply_to = current.draft.reply_to;
                                    stashed.draft.reply_quote = current.draft.reply_quote;
                                    stashed.reply_author = current.reply_author;
                                    stashed.reply_preview = current.reply_preview;
                                }
                                if stashed.forward.is_none() {
                                    stashed.forward = current.forward;
                                }
                            }
                            let previous = backend.compose_stash(stash_key);
                            self.stash_hint.mark_used();
                            self.stash_menu_open = false;
                            self.compose.text.clear();
                            self.compose.rich_edit.clear();
                            self.compose.cancel_mode();
                            backend.save_rich_draft(ThreadKey::Main(peer_id), None);
                            backend.set_compose_stash(stash_key, Some(stashed));
                            self.attach_box = None;
                            if let Some(prev) = previous {
                                self.apply_compose_stash(prev);
                                backend.save_rich_draft(
                                    ThreadKey::Main(peer_id),
                                    self.compose.current_rich_draft(),
                                );
                            }
                            self.last_compose_text_for_hint = self.compose.text.clone();
                            self.show_toast("Files stashed (Ctrl+S to restore)", now_secs);
                        }
                        Err(err) => {
                            self.show_toast(err, now_secs);
                        }
                    }
                }
                AttachFilesAction::CaptionLimitExceeded { .. } => {
                    self.open_premium_limits(PremiumLimitKind::CaptionLength);
                }
                AttachFilesAction::Cancelled => {
                    self.attach_box = None;
                }
            }
        }

        if let Some(ref mut box_state) = self.delete_box
            && let Some(ev) = box_state.show(ui, colors)
        {
            if let DeleteConfirmAction::Confirm {
                msg_ids,
                revoke_for_everyone,
            } = ev
                && backend
                    .delete_messages(peer_id, &msg_ids, revoke_for_everyone)
                    .is_ok()
            {
                // Launch Thanos disintegration particle cloud for deleted bubbles (`thanos_effect.cpp`).
                if self.last_list_rect.is_positive() {
                    let policy = self.thanos_policy();
                    let bubble_w = (self.last_list_rect.width() * 0.55).clamp(140.0, 320.0);
                    for (idx, _) in msg_ids.iter().take(4).enumerate() {
                        let y = (self.last_list_rect.center().y + idx as f32 * 52.0)
                            .min(self.last_list_rect.bottom() - 56.0);
                        let rect = Rect::from_min_size(
                            Pos2::new(self.last_list_rect.center().x - bubble_w * 0.5, y),
                            Vec2::new(bubble_w, 44.0),
                        );
                        self.thanos_effect
                            .add_item(rect, colors.msg_out_bg, colors.accent, policy);
                    }
                }
                self.inner.remove_messages(&msg_ids, peer_info.summary.kind);
                self.inner.clear_selection();
                let toast = if revoke_for_everyone {
                    format!("Deleted {} message(s) for everyone", msg_ids.len())
                } else {
                    format!("Deleted {} message(s) for you", msg_ids.len())
                };
                self.show_toast(toast, now_secs);
            }
            self.delete_box = None;
        }

        if let Some((req, mut editor)) = self.draw_to_reply_editor.take() {
            if let Some(result) = editor.show(ui, colors) {
                if let Some(jpeg_bytes) = result {
                    let send_req = SendMessageExtended {
                        text: tg_text::TextWithEntities::default(),
                        reply_to: Some(MsgId(req.reply_to_msg_id)),
                        media: Some(HistoryMedia::Photo(PhotoAttachment {
                            width: editor.width as i32,
                            height: editor.height as i32,
                            stripped_thumb: None,
                            fallback_rgb: ([0x4E, 0x90, 0xC8], [0x26, 0x4D, 0x73]),
                            file_name: req.file_name,
                            size_bytes: jpeg_bytes.len() as u64,
                        })),
                        silent: self.compose.silent_mode,
                        ..SendMessageExtended::default()
                    };
                    if let Ok(sent) = backend.send_message_extended(peer_id, send_req) {
                        self.inner.append_message(sent, peer_info.summary.kind);
                        self.show_toast("Sent drawn reply", now_secs);
                    }
                }
            } else {
                self.draw_to_reply_editor = Some((req, editor));
            }
        }

        if let Some(ref mut box_state) = self.report_box
            && let Some(ev) = box_state.show(ui, colors)
        {
            if let ReportMessageAction::Submit {
                msg_ids,
                reason,
                comment,
            } = ev
                && backend
                    .report_messages(peer_id, &msg_ids, reason, &comment)
                    .is_ok()
            {
                self.show_toast(format!("Reported ({})", reason.label()), now_secs);
            }
            self.report_box = None;
        }

        if let Some(ref mut box_state) = self.block_from_replies_box
            && let Some(ev) = box_state.show(ui, colors)
        {
            let sender_name = box_state.sender_name.clone();
            if let BlockFromRepliesAction::Submit(req) = ev {
                match backend.block_from_replies(req) {
                    Ok(()) => {
                        if req.delete_message || req.delete_history {
                            let target_from =
                                self.find_loaded_message(req.msg_id).map(|m| m.from_id);
                            let to_remove: Vec<MsgId> = if req.delete_history
                                && let Some(from_id) = target_from
                            {
                                self.inner
                                    .messages()
                                    .iter()
                                    .filter(|m| m.from_id == from_id || m.id == req.msg_id)
                                    .map(|m| m.id)
                                    .collect()
                            } else {
                                vec![req.msg_id]
                            };
                            self.inner
                                .remove_messages(&to_remove, peer_info.summary.kind);
                        }
                        self.show_toast(format!("Blocked {sender_name} from Replies"), now_secs);
                    }
                    Err(err) => self.show_toast(err, now_secs),
                }
            }
            self.block_from_replies_box = None;
        }

        // Layer-233 modals: EditTodoListBox, AddTodoListTasksBox, AboutSponsoredBox,
        // ReportSponsoredBox, and EditFactCheckBox.
        if let Some(ref mut box_state) = self.todo_box
            && let Some(ev) = box_state.show(ui, colors)
        {
            match ev {
                todo::EditTodoListBoxAction::Create(req) => {
                    if let Some(ex) = backend.as_layer233_extras_mut() {
                        match ex.create_todo_list(peer_id, req) {
                            Ok(sent) => {
                                self.inner.append_message(sent, peer_info.summary.kind);
                                self.show_toast("Checklist created", now_secs);
                            }
                            Err(err) => self.show_toast(err, now_secs),
                        }
                    }
                }
                todo::EditTodoListBoxAction::SaveEdit { msg_id, request } => {
                    if let Some(ex) = backend.as_layer233_extras_mut() {
                        match ex.edit_todo_list(peer_id, msg_id, request) {
                            Ok(updated) => {
                                self.inner.update_message_in_place(updated);
                                self.show_toast("Checklist updated", now_secs);
                            }
                            Err(err) => self.show_toast(err, now_secs),
                        }
                    }
                }
                todo::EditTodoListBoxAction::Cancelled => {}
            }
            self.todo_box = None;
        }

        if let Some(ref mut box_state) = self.add_todo_tasks_box
            && let Some(ev) = box_state.show(ui, colors)
        {
            if let todo::AddTodoListTasksBoxAction::Append { msg_id, items } = ev
                && let Some(ex) = backend.as_layer233_extras_mut()
            {
                match ex.append_todo_list(peer_id, msg_id, items) {
                    Ok(Some(updated)) => {
                        self.inner.update_message_in_place(updated);
                        self.show_toast("Tasks added to checklist", now_secs);
                    }
                    Ok(None) => {}
                    Err(err) => self.show_toast(err, now_secs),
                }
            }
            self.add_todo_tasks_box = None;
        }

        if let Some(ref mut box_state) = self.about_sponsored_box
            && let Some(ev) = box_state.show(ui, colors)
        {
            if let sponsored::AboutSponsoredBoxAction::OpenAdsPlatform(url) = ev {
                self.show_toast(format!("Opening {url}"), now_secs);
            }
            self.about_sponsored_box = None;
        }

        if let Some(ref mut box_state) = self.report_sponsored_box
            && let Some(ev) = box_state.show(ui, colors)
        {
            match ev {
                sponsored::ReportSponsoredBoxAction::ChooseOption { random_id, option } => {
                    if let Some(ex) = backend.as_layer233_extras_mut()
                        && let Ok(outcome) = ex.report_sponsored_message(&random_id, &option)
                    {
                        box_state.apply_outcome(outcome);
                    }
                }
                sponsored::ReportSponsoredBoxAction::Close => {
                    self.report_sponsored_box = None;
                }
            }
        }

        if let Some(ref mut box_state) = self.calendar_box
            && let Some(ev) = box_state.show_in_history(ui, colors, |pid, y, m| {
                backend.search_results_calendar(pid, y, m)
            })
        {
            if let CalendarBoxAction::JumpToDate { msg_id, .. } = ev {
                let jump_id = if msg_id.0 > 0 {
                    msg_id
                } else {
                    MsgId::SHOW_AT_END
                };
                self.inner.jump_to_message(
                    jump_id,
                    backend,
                    peer_info.summary.kind,
                    (now_secs.max(0.0) * 1000.0) as i64,
                );
            }
            self.calendar_box = None;
        }

        if let Some(ref mut box_state) = self.edit_caption_box
            && let Some(ev) = box_state.show(ui, colors)
        {
            if let edit_caption::EditCaptionAction::Submit {
                peer_id: target_peer,
                msg_id,
                request,
            } = ev
            {
                match backend.edit_message_media(target_peer, msg_id, request) {
                    Ok(updated) => {
                        self.inner.update_message_in_place(updated);
                        self.show_toast("Caption updated", now_secs);
                    }
                    Err(err) => self.show_toast(err, now_secs),
                }
            }
            self.edit_caption_box = None;
        }

        if let Some(ref mut box_state) = self.edit_factcheck_box
            && let Some(ev) = box_state.show(ui, colors)
        {
            match ev {
                factcheck::EditFactCheckBoxAction::Save { msg_id, text } => {
                    if let Some(ex) = backend.as_layer233_extras_mut() {
                        match ex.edit_factcheck(peer_id, msg_id, text) {
                            Ok(_) => self.show_toast("Fact-check saved", now_secs),
                            Err(err) => self.show_toast(err, now_secs),
                        }
                    }
                }
                factcheck::EditFactCheckBoxAction::Remove { msg_id } => {
                    if let Some(ex) = backend.as_layer233_extras_mut() {
                        match ex.edit_factcheck(
                            peer_id,
                            msg_id,
                            tg_text::TextWithEntities::simple(""),
                        ) {
                            Ok(_) => self.show_toast("Fact-check removed", now_secs),
                            Err(err) => self.show_toast(err, now_secs),
                        }
                    }
                }
                factcheck::EditFactCheckBoxAction::Cancelled => {}
            }
            self.edit_factcheck_box = None;
        }
        if let Some(mut jr_box) = self.join_requests_box.take() {
            let mut keep_open = true;
            let ui_colors = colors.ui_colors();
            let modal_w = 400.0_f32.min((ui.ctx().content_rect().width() - 32.0).max(260.0));
            egui::Area::new(egui::Id::unique("tg_hist_join_requests_modal"))
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ui.ctx(), |ui| {
                    egui::Frame::NONE
                        .fill(colors.bar_bg)
                        .stroke(Stroke::new(1.0, colors.divider))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(egui::Margin::same(16))
                        .show(ui, |ui| {
                            ui.set_min_width(modal_w);
                            ui.set_max_width(modal_w);
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("Join Requests")
                                        .font(FontId::proportional(16.0))
                                        .color(colors.bold_fg),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.button("Close").clicked() {
                                            keep_open = false;
                                        }
                                    },
                                );
                            });
                            ui.add_space(8.0);
                            egui::ScrollArea::vertical()
                                .max_height(360.0)
                                .show(ui, |ui| {
                                    if let Some(act) =
                                        jr_box.show_body_history(ui, backend, &ui_colors)
                                    {
                                        match act {
                                            JoinRequestsAction::SingleResolved {
                                                approved, ..
                                            } => {
                                                self.show_toast(
                                                    if approved {
                                                        "Join request approved"
                                                    } else {
                                                        "Join request dismissed"
                                                    },
                                                    now_secs,
                                                );
                                            }
                                            JoinRequestsAction::AllResolved { count, approved } => {
                                                self.show_toast(
                                                    if approved {
                                                        format!("Approved {count} join request(s)")
                                                    } else {
                                                        format!("Dismissed {count} join request(s)")
                                                    },
                                                    now_secs,
                                                );
                                            }
                                        }
                                    }
                                });
                        });
                });
            if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
                keep_open = false;
            }
            if keep_open {
                self.join_requests_box = Some(jr_box);
            }
        }

        // Embedded Telegram Premium Limits modal (`business::premium::PremiumLimitsView`).
        if let Some((ref mut limits_view, ref mut snap, ref mut biz_backend)) =
            self.premium_limits_modal
        {
            let screen = ui.ctx().content_rect();
            let modal_w = (screen.width() - 48.0).clamp(360.0, 520.0);
            let modal_h = (screen.height() - 64.0).clamp(380.0, 560.0);
            let modal_pos = Pos2::new(
                screen.center().x - modal_w * 0.5,
                screen.center().y - modal_h * 0.5,
            );
            let mut close_modal = ui.ctx().input(|i| i.key_pressed(egui::Key::Escape));
            let mut toast_msg = None;

            egui::Area::new(egui::Id::unique("tg_hist_premium_limits_backdrop"))
                .order(egui::Order::Foreground)
                .fixed_pos(screen.min)
                .show(ui.ctx(), |ui| {
                    let (_, resp) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
                    ui.painter()
                        .rect_filled(screen, CornerRadius::ZERO, palette.layer_backdrop);
                    if resp.clicked() {
                        close_modal = true;
                    }
                });

            egui::Area::new(egui::Id::unique("tg_hist_premium_limits_modal"))
                .order(egui::Order::Tooltip)
                .fixed_pos(modal_pos)
                .show(ui.ctx(), |ui| {
                    Frame::NONE
                        .fill(palette.window_bg)
                        .stroke(Stroke::new(1.0, palette.divider))
                        .corner_radius(CornerRadius::same(12))
                        .inner_margin(Margin::same(14))
                        .show(ui, |ui| {
                            ui.set_min_size(Vec2::new(modal_w - 28.0, modal_h - 28.0));
                            ui.set_max_size(Vec2::new(modal_w - 28.0, modal_h - 28.0));
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new("Telegram Premium Limits")
                                        .size(15.0)
                                        .strong()
                                        .color(palette.window_bold_fg),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.button("Close").clicked() {
                                            close_modal = true;
                                        }
                                    },
                                );
                            });
                            ui.add_space(6.0);
                            egui::ScrollArea::vertical()
                                .id_salt("tg_hist_premium_limits_scroll")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    if let Some(PremiumSettingsAction::Toast(msg)) =
                                        limits_view.show(ui, snap, biz_backend, palette)
                                    {
                                        toast_msg = Some(msg);
                                    }
                                });
                        });
                });

            if let Some(msg) = toast_msg {
                self.show_toast(msg, now_secs);
            }
            if close_modal {
                self.premium_limits_modal = None;
            }
        }

        // Seen-By group read receipts modal (`messages.getMessageReadParticipants`).
        if let Some(mut modal) = self.seen_by_modal.take()
            && !modal.show(ui, ui.max_rect(), colors)
        {
            self.seen_by_modal = Some(modal);
        }

        // Who-Reacted peer list modal (`info::reactions_list::ReactionsListWidget`).
        if let Some(mut modal) = self.who_reacted_modal.take() {
            let act = modal.show_modal(ui, colors);
            match act {
                Some(ReactionsListAction::Close) => {}
                Some(ReactionsListAction::SelectFilter(filter)) => {
                    match backend.message_reactions_list(
                        modal.peer_id,
                        modal.msg_id,
                        filter.as_deref(),
                        None,
                        PEER_LIST_PAGE_LIMIT,
                    ) {
                        Ok(page) => modal.apply_filter_page(filter, page),
                        Err(err) => modal.error = Some(err),
                    }
                    self.who_reacted_modal = Some(modal);
                }
                Some(ReactionsListAction::LoadMore(cursor)) => {
                    match backend.message_reactions_list(
                        modal.peer_id,
                        modal.msg_id,
                        modal.active_filter.as_deref(),
                        Some(&cursor),
                        PEER_LIST_PAGE_LIMIT,
                    ) {
                        Ok(page) => modal.append_page(page),
                        Err(err) => modal.error = Some(err),
                    }
                    self.who_reacted_modal = Some(modal);
                }
                Some(ReactionsListAction::OpenPeer(target_peer)) => {
                    self.show_toast(format!("Peer #{}", target_peer.bare()), now_secs);
                }
                Some(ReactionsListAction::TogglePaidPrivacy(privacy)) => {
                    if let Some(ch) = backend.as_chat_helpers_backend_mut() {
                        match ch.toggle_paid_reaction_privacy(modal.peer_id, modal.msg_id, privacy)
                        {
                            Ok(_) => {
                                modal.paid_privacy = Some(privacy);
                                let label = if matches!(
                                    privacy,
                                    tg_app_core::chat_helpers::backend::PaidReactionPrivacy::Anonymous
                                ) {
                                    "Paid reaction set to Anonymous"
                                } else {
                                    "Shown in Paid Top Senders"
                                };
                                self.show_toast(label, now_secs);
                            }
                            Err(err) => modal.error = Some(err),
                        }
                    }
                    self.who_reacted_modal = Some(modal);
                }
                Some(ReactionsListAction::ReportReaction(reaction_peer_id)) => {
                    if let Some(ch) = backend.as_chat_helpers_backend() {
                        match ch.report_reaction(modal.peer_id, modal.msg_id, reaction_peer_id) {
                            Ok(_) => {
                                self.show_toast(tg_lang::tr::lng_report_spam_done(), now_secs);
                            }
                            Err(err) => modal.error = Some(err),
                        }
                    }
                    self.who_reacted_modal = Some(modal);
                }
                Some(ReactionsListAction::DeleteParticipantReaction(participant_id)) => {
                    if let Some(ch) = backend.as_chat_helpers_backend_mut() {
                        match ch.delete_participant_reaction(
                            modal.peer_id,
                            modal.msg_id,
                            participant_id,
                        ) {
                            Ok(()) => {
                                modal.remove_participant_entries(participant_id);
                                self.show_toast("Reaction removed", now_secs);
                            }
                            Err(err) => modal.error = Some(err),
                        }
                    }
                    self.who_reacted_modal = Some(modal);
                }
                Some(ReactionsListAction::DeleteParticipantReactions(participant_id)) => {
                    if let Some(ch) = backend.as_chat_helpers_backend_mut() {
                        match ch.delete_participant_reactions(modal.peer_id, participant_id) {
                            Ok(_) => {
                                modal.remove_participant_entries(participant_id);
                                self.show_toast("Removed participant reactions", now_secs);
                            }
                            Err(err) => modal.error = Some(err),
                        }
                    }
                    self.who_reacted_modal = Some(modal);
                }
                None => {
                    self.who_reacted_modal = Some(modal);
                }
            }
        }

        // WP-28 modals: TranslateBox, Compose with AI, Sticker Creator, Self-Destruct TTL, View-Once Viewer.
        let modal_palette = colors.ui_colors();

        if let Some(mut modal) = self.translate_box.take() {
            let outcome = if let Some(ai_be) = backend.as_compose_ai_backend() {
                modal.show(ui, ai_be, &modal_palette)
            } else {
                let mut fallback = tg_app_info::boxes::compose_ai::MockComposeAiStore::new();
                modal.show(ui, &mut fallback, &modal_palette)
            };
            match outcome {
                Some(TranslateBoxAction::Copy(text)) => {
                    ui.ctx().copy_text(text);
                    self.show_toast("Translation copied", now_secs);
                }
                Some(TranslateBoxAction::Close) => {}
                None => self.translate_box = Some(modal),
            }
        }

        if let Some(mut modal) = self.compose_ai_box.take() {
            let outcome = if let Some(ai_be) = backend.as_compose_ai_backend() {
                modal.show(ui, ai_be, &modal_palette)
            } else {
                let mut fallback = tg_app_info::boxes::compose_ai::MockComposeAiStore::new();
                modal.show(ui, &mut fallback, &modal_palette)
            };
            match outcome {
                Some(ComposeAiBoxAction::Apply(applied)) => {
                    self.compose.text = applied.text.clone();
                    self.compose.rich_edit.set_text_with_entities(applied);
                    self.compose.focus_requested = true;
                    self.show_toast("Applied AI compose result", now_secs);
                }
                Some(ComposeAiBoxAction::Close) => {}
                None => self.compose_ai_box = Some(modal),
            }
        }

        if let Some(mut modal) = self.about_cocoon_box.take() {
            let paused = !self.effects_animations_enabled || !self.effects_chat_enabled;
            match modal.show(ui, &modal_palette, paused) {
                Some(AboutCocoonAction::OpenLink(url)) => {
                    out_action = Some(HistoryAction::OpenLink(url.clone()));
                    self.show_toast(format!("Opening {url}"), now_secs);
                    self.about_cocoon_box = Some(modal);
                }
                Some(AboutCocoonAction::Close) => {}
                None => self.about_cocoon_box = Some(modal),
            }
        }

        if let Some(mut modal) = self.sticker_creator_box.take() {
            let outcome = if let Some(sc_be) = backend.as_sticker_creator_backend() {
                modal.show(ui, sc_be, &modal_palette)
            } else {
                let mut fallback =
                    tg_app_info::boxes::sticker_creator::MockStickerCreatorStore::new();
                modal.show(ui, &mut fallback, &modal_palette)
            };
            match outcome {
                Some(StickerCreatorAction::Saved(summary)) => {
                    self.show_toast(
                        format!("Saved to sticker pack \"{}\"", summary.title),
                        now_secs,
                    );
                }
                Some(StickerCreatorAction::Cancelled) => {}
                None => self.sticker_creator_box = Some(modal),
            }
        }

        if let Some(mut modal) = self.self_destruct_box.take() {
            let outcome = if let Some(sd_be) = backend.as_self_destruct_backend() {
                modal.show(ui, sd_be, &modal_palette)
            } else {
                let mut fallback = tg_app_info::boxes::self_destruct::MockSelfDestructStore::new();
                modal.show(ui, &mut fallback, &modal_palette)
            };
            match outcome {
                Some(SelfDestructionBoxAction::Saved { .. }) => {
                    self.show_toast("Self-destruct TTL updated", now_secs);
                }
                Some(SelfDestructionBoxAction::Cancelled) => {}
                None => self.self_destruct_box = Some(modal),
            }
        }

        if let Some(mut modal) = self.view_once_modal.take() {
            if !modal.read_acknowledged {
                let _ = backend.mark_ttl_media_viewed(modal.peer_id, modal.msg_id);
                modal.read_acknowledged = true;
            }
            match modal.show(ui, now_secs) {
                Some(ViewOnceModalAction::Consumed {
                    peer_id: target_peer,
                    msg_id,
                }) => {
                    let _ = backend.mark_ttl_media_viewed(target_peer, msg_id);
                    if let Some(mut copy) = self.find_loaded_message(msg_id).cloned() {
                        copy.extra.ttl_viewed = true;
                        self.inner.update_message_in_place(copy);
                    }
                    self.show_toast("View-once media expired", now_secs);
                }
                None => self.view_once_modal = Some(modal),
            }
        }

        // Poll Voters list modal (`info::reactions_list::PollVotersWidget`).
        if let Some(mut modal) = self.poll_voters_modal.take() {
            let act = modal.show_modal(ui, colors);
            match act {
                Some(PollVotersAction::Close) => {}
                Some(PollVotersAction::SelectOption(opt_idx)) => {
                    match backend.poll_votes(
                        modal.peer_id,
                        modal.msg_id,
                        opt_idx,
                        None,
                        PEER_LIST_PAGE_LIMIT,
                    ) {
                        Ok(page) => modal.apply_option_page(opt_idx, page),
                        Err(err) => modal.error = Some(err),
                    }
                    self.poll_voters_modal = Some(modal);
                }
                Some(PollVotersAction::LoadMore(cursor)) => {
                    match backend.poll_votes(
                        modal.peer_id,
                        modal.msg_id,
                        modal.active_option,
                        Some(&cursor),
                        PEER_LIST_PAGE_LIMIT,
                    ) {
                        Ok(page) => modal.append_page(page),
                        Err(err) => modal.error = Some(err),
                    }
                    self.poll_voters_modal = Some(modal);
                }
                Some(PollVotersAction::OpenPeer(target_peer)) => {
                    self.show_toast(format!("Voter #{}", target_peer.bare()), now_secs);
                }
                Some(PollVotersAction::ReloadResults) => {
                    match backend.reload_poll_results(modal.peer_id, modal.msg_id) {
                        Ok(Some(updated)) => {
                            if let Some(mut copy) = self.find_loaded_message(modal.msg_id).cloned()
                            {
                                copy.media = Some(HistoryMedia::Poll(updated.clone()));
                                self.inner.update_message_in_place(copy);
                            }
                            modal.apply_poll_results(updated);
                            self.show_toast("Poll results refreshed", now_secs);
                        }
                        Ok(None) => {
                            self.show_toast("Poll results up to date", now_secs);
                        }
                        Err(err) => modal.error = Some(err),
                    }
                    self.poll_voters_modal = Some(modal);
                }
                Some(PollVotersAction::LoadStats { dark }) => {
                    match backend.poll_stats(modal.peer_id, modal.msg_id, dark) {
                        Ok(graph) => modal.apply_poll_stats(graph, dark),
                        Err(err) => {
                            modal.show_stats = true;
                            modal.stats_error = Some(err);
                        }
                    }
                    self.poll_voters_modal = Some(modal);
                }
                None => {
                    self.poll_voters_modal = Some(modal);
                }
            }
        }
        out_action
    }

    fn find_loaded_message(&self, msg_id: MsgId) -> Option<&HistoryMessage> {
        self.inner.messages().iter().find(|m| m.id == msg_id)
    }

    fn show_toast(&mut self, message: impl Into<String>, now_secs: f64) {
        self.status_toast = Some((message.into(), now_secs + 2.6));
    }

    fn paint_status_toast(&mut self, ui: &mut Ui, list_rect: Rect, now_secs: f64) {
        let Some((ref text, expires_at)) = self.status_toast else {
            return;
        };
        if now_secs >= expires_at {
            self.status_toast = None;
            return;
        }
        let galley =
            ui.painter()
                .layout_no_wrap(text.clone(), FontId::proportional(12.5), Color32::WHITE);
        let size = Vec2::new(galley.size().x + 24.0, galley.size().y + 12.0);
        let pill = Rect::from_center_size(
            Pos2::new(list_rect.center().x, list_rect.bottom() - 26.0),
            size,
        );
        ui.painter().rect_filled(
            pill,
            CornerRadius::same(14),
            Color32::from_rgba_unmultiplied(28, 32, 36, 224),
        );
        ui.painter().galley(
            Pos2::new(
                pill.center().x - galley.size().x * 0.5,
                pill.center().y - galley.size().y * 0.5,
            ),
            galley,
            Color32::WHITE,
        );
        ui.ctx().request_repaint();
    }

    fn render_empty_placeholder(
        &self,
        ui: &mut Ui,
        palette: &Palette,
        strings: &dyn Strings,
    ) -> Option<HistoryAction> {
        let rect = ui.max_rect();
        let colors = HistoryColors::from_palette(palette);
        ui.painter().rect_filled(rect, 0.0, colors.canvas_bg);
        ui.centered_and_justified(|ui| {
            Frame::NONE
                .fill(colors.service_bg)
                .corner_radius(CornerRadius::same(16))
                .inner_margin(Margin::symmetric(16, 8))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(strings.select_chat_prompt())
                            .size(13.5)
                            .color(colors.service_fg),
                    );
                });
        });
        None
    }
}

/// Maps a sandboxed widget `OpenLink` bridge message through `tg_tgw::evaluate_link_open` into a
/// [`HistoryAction`], ensuring `LinkPolicyDecision::ConfirmExternal` routes through the hidden-link
/// confirmation flow (`force_confirm: true`) instead of opening immediately.
pub(crate) fn route_widget_open_link(url: &str, telegram: bool) -> Result<HistoryAction, String> {
    match tg_tgw::evaluate_link_open(url, telegram, true, false) {
        tg_tgw::LinkPolicyDecision::OpenTelegram { url: u, .. } => Ok(HistoryAction::OpenLink(u)),
        tg_tgw::LinkPolicyDecision::ConfirmExternal { url: u, .. } => {
            Ok(HistoryAction::OpenHiddenLink {
                url: u,
                force_confirm: true,
            })
        }
        tg_tgw::LinkPolicyDecision::Refused(reason)
        | tg_tgw::LinkPolicyDecision::Dropped(reason) => Err(reason),
    }
}

#[cfg(test)]
mod seca_widget_link_tests {
    use super::*;

    #[test]
    fn seca_a1_widget_confirm_external_routes_to_forced_hidden_link_confirm() {
        assert_eq!(
            route_widget_open_link("https://example.org/docs", false),
            Ok(HistoryAction::OpenHiddenLink {
                url: "https://example.org/docs".to_owned(),
                force_confirm: true,
            })
        );
        assert_eq!(
            route_widget_open_link("https://t.me/telegram", true),
            Ok(HistoryAction::OpenLink("https://t.me/telegram".to_owned()))
        );
        assert!(route_widget_open_link("file:///C:/Windows/calc.exe", false).is_err());
        assert!(route_widget_open_link("javascript:alert(1)", false).is_err());
    }
}
