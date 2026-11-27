//! Main authenticated shell widget: vertical folders sidebar, dialogs | main | third column,
//! resizable splitters, floating connection indicator, and unified screen composition.
//!
//! Port origin: `Telegram/SourceFiles/mainwidget.h` and `Telegram/SourceFiles/mainwidget.cpp`.

use egui::{
    Area, Color32, CornerRadius, CursorIcon, Frame, Id, Order, Pos2, Rect, Sense, Stroke, Ui,
    UiBuilder, Vec2,
};
use tg_audio::AudioMsgId;
use tg_core::settings::{AutoplaySettings, CoreSettings, PowerSavingFlags};
use tg_emoji::EmojiTextureCache;
use tg_text::TextWithEntities;
use tg_theme::{Color as ThemeColor, EmbeddedType};
use tg_ui::fonts::font_id_from_style;
use tg_ui::icons::paint_style_icon;
use tg_ui::tg_style::{DefaultPaletteResolver, Icon, st};
use tg_ui::theme::Colors;

use crate::window::adaptive::{
    COLUMN_MIN_WIDTH_MAIN, COLUMN_MIN_WIDTH_THIRD, HISTORY_RESIZE_WIDTH, WindowLayout,
    dialogs_drag_to_ratio, minimal_three_column_width,
};
use crate::window::connecting_widget::{ConnectionIndicator, ConnectionWidgetAction};
use crate::window::section::{SectionMemento, SectionShow, SettingsPage};
use crate::window::session_controller::SessionController;
use tg_app_core::session_api::AppSession;
use tg_app_core::stand_in::backend::Backend;
use tg_app_core::stand_in::strings::Strings;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_app_history::history::backend::{HistoryBackend, SendMessageRequest};
use tg_app_history::history::compose::ComposeMode;
use tg_app_history::history::inline_media::default_hardware_audio_enabled;
use tg_app_history::history::sections_ui::HistorySectionKind;
use tg_app_history::history::{HistoryAction, HistoryWidget};
use tg_app_info::dialogs::{DialogsPanel, DialogsPanelAction, DialogsShortcut};
use tg_app_info::info::{InfoWrapEvent, InfoWrapWidget};
use tg_app_info::statistics::{StatisticsScreen, StatisticsScreenHost};
use tg_app_media::media::style::MEDIA_PLAYER_HEIGHT;
use tg_app_media::media::{AudioPlayerBar, AudioPlayerBarEvent, MediaBackend};
use tg_app_media::stories::{AppStoriesHost, StoriesSearchSlot};
use tg_app_settings::chat_helpers::{
    AutocompleteAction, ChatHelpersEvent, FieldAutocompleteView, TabbedPanel,
};
use tg_app_settings::settings::advanced::AdvancedSubPanel;
use tg_app_settings::settings::backend::SettingsBackend;
use tg_app_settings::settings::main_page::{set_settings_search_open, settings_search_open};
use tg_app_settings::settings::{SettingsAction, SettingsController, SettingsSection};
use tg_app_settings::themes::chat_background::{BackgroundConfig, ChatBackground};

/// High-level actions forwarded from [`MainWidget`] to [`crate::window::controller::WindowController`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MainWidgetAction {
    /// Open the hamburger main menu drawer.
    OpenMainMenu,
    /// Open a chat in a separate window (`SessionController::showInNewWindow`).
    OpenInNewWindow(PeerId),
    /// Open proxy/connection modal box.
    OpenProxySettings,
    /// Retry connection immediately.
    RetryConnection,
    /// Toggle Day/Night theme mode.
    ToggleTheme,
    /// Apply a specific embedded theme and optional accent color selected inside Settings.
    ApplyTheme {
        /// Selected embedded theme kind.
        kind: EmbeddedType,
        /// Optional accent override.
        accent: Option<ThemeColor>,
    },
    /// Apply a new UI language code chosen in Settings.
    ApplyLanguage(String),
    /// Toggle vertical folders sidebar.
    ToggleFoldersSidebar,
    /// Open passcode setup modal.
    OpenPasscodeSetup,
    /// Immediately lock the app with passcode.
    LockNow,
    /// Open Log Out confirmation modal.
    RequestLogout,
    /// Open media viewer (or audio player bar) for a history message attachment.
    OpenHistoryMedia {
        /// Owning peer id.
        peer_id: PeerId,
        /// Clicked message id.
        msg_id: MsgId,
        /// Optional album item index.
        album_index: Option<usize>,
    },
    /// Route a clicked link (internal `tg://`/`t.me/`, Instant View, or system browser).
    OpenLink(String),
    /// Route a clicked hidden link (`messageEntityTextUrl`) or widget `ConfirmExternal` link
    /// through scheme validation and confirmation before opening externally.
    OpenHiddenLink {
        /// Target URL.
        url: String,
        /// If `true`, always require confirmation even on whitelisted domains (e.g. widget bridge).
        force_confirm: bool,
    },
    /// Open a Bot Mini App (or embedded HTML5 Game WebView).
    OpenMiniApp {
        /// Target bot peer id.
        bot_id: PeerId,
        /// Optional explicit button or game WebView URL.
        url: Option<String>,
        /// Optional deep-link `start_param` payload.
        start_param: Option<String>,
        /// Show the `confirmOpen` box first unless the bot is verified or trusted.
        confirm_open: bool,
    },
    /// Open the peer picker filtered by `peer_types` for a `switch_inline` button (`api_bot.cpp`).
    SwitchInlineChooseChat {
        /// Target bot peer id.
        bot_id: PeerId,
        /// Target bot username without leading `@`.
        bot_username: String,
        /// Pre-filled inline query text.
        query: String,
        /// Allowed destination chat types (`PeerTypes::empty()` allows all sendable chats).
        peer_types: tg_bots::markup::PeerTypes,
    },
    /// Switch directly to `target_peer` and prefill `@bot_username query` in its compose field.
    SwitchInlineToPeer {
        /// Destination chat peer id.
        target_peer: PeerId,
        /// Target bot peer id.
        bot_id: PeerId,
        /// Target bot username without leading `@`.
        bot_username: String,
        /// Pre-filled inline query text.
        query: String,
    },
    /// Open `ModerateMessagesBox` from a group/channel message context menu.
    OpenModerateMessages {
        /// Group / supergroup / channel peer id.
        peer_id: PeerId,
        /// Chat display title.
        peer_title: String,
        /// Selected message IDs.
        message_ids: Vec<i32>,
        /// Distinct participants authoring the selected messages.
        participants: Vec<tg_app_info::boxes::moderate_messages::ModerateParticipant>,
    },
    /// Open `MusicAttachBox` from the compose attach menu (`boxes/music_attach_box.cpp`).
    OpenMusicAttach(PeerId),
    /// Forward an Info/Profile action (modal boxes, shared media, statistics, boosts, mini app).
    InfoEvent(InfoWrapEvent),
    /// Open the Payments / Stars / Gifts / Premium / Affiliate / Checkout overlay.
    OpenPayments {
        /// Target payments screen.
        screen: tg_app_info::payments::PaymentsScreen,
        /// Optional invoice slug, message invoice reference, passport URI, unique gift slug, or paid media payload.
        slug: Option<String>,
    },
    /// Open the Telegram Business overlay.
    OpenBusiness,
    /// Open `EditPeerColorBox` for the currently signed-in user.
    OpenEditSelfColor,
    /// Open a specific peer's story viewer from an interactive Story bubble.
    OpenStory {
        /// Story owner peer ID.
        peer_id: PeerId,
        /// Story identifier.
        story_id: i32,
    },
    /// Open the Giveaway Info / Boosts screen from an interactive Giveaway bubble.
    OpenGiveawayInfo {
        /// Owning chat peer ID.
        peer_id: PeerId,
        /// Giveaway message ID.
        msg_id: MsgId,
        /// Host channel peer ID, if known.
        channel_id: Option<PeerId>,
        /// Launch message ID, if known.
        launch_msg_id: Option<MsgId>,
    },
    /// Open the RTMP/HLS live video stream viewer from an interactive VideoStream bubble.
    OpenVideoStream {
        /// Owning chat peer ID.
        peer_id: PeerId,
        /// Message ID.
        msg_id: MsgId,
        /// Group call ID.
        call_id: i64,
        /// Group call access hash.
        access_hash: u64,
        /// True if RTMP/HLS broadcast.
        rtmp_stream: bool,
        /// Stream title.
        title: String,
    },
    /// Open the "Forward to..." `ShareBox` layer for messages chosen in the message menu or the
    /// selection bar (`Window::ShowForwardMessagesBox`).
    OpenForwardBox {
        /// Chat the messages are forwarded from.
        from_peer: PeerId,
        /// Messages to forward.
        msg_ids: Vec<MsgId>,
    },
    /// Show an informational toast.
    Toast(String),
    /// Show a failed request as tdesktop does there (a box or the Premium screen).
    UserError(tg_app_core::user_error::UserFacingError),
    /// Column widths changed and should be persisted.
    PersistSettings,
}

/// Smallest window width that shows Settings as a layer (`Info::LayerWidget::MinimalSupportedWidth`
/// = `st::infoMinimalWidth` 324 + 2 * `st::infoMinimalLayerMargin` 48).
pub const SETTINGS_LAYER_MIN_WIDTH: f32 = 324.0 + 2.0 * 48.0;
/// Settings layer width (`st::infoDesiredWidth`).
pub const SETTINGS_LAYER_WIDTH: f32 = 392.0;
/// Settings layer header height (`st::infoLayerTopBarHeight`).
pub const SETTINGS_LAYER_HEADER_HEIGHT: f32 = 56.0;
/// Width of the docked Emoji / Stickers / GIFs column (`st::columnMinimalWidthThird`).
pub const TABBED_DOCK_WIDTH: f32 = COLUMN_MIN_WIDTH_THIRD as f32;

/// Whether the chat header shows its Back arrow (`TopBarWidget::updateControlsVisibility`:
/// one-column mode or a non-empty section stack). Settings drawn as a layer over the chat is not
/// part of the chat's navigation: tdesktop shows it as a layer without touching the stack, so the
/// entry `show_settings` pushed for the covered chat and the Settings main section are ignored.
#[must_use]
pub fn history_can_go_back(session: &SessionController, settings_as_layer: bool) -> bool {
    if !settings_as_layer {
        return session.can_go_back();
    }
    session.stack_len() > 1
        || (session.adaptive().is_one_column() && !session.active_peer().is_zero())
}

/// Geometry of the centered Settings layer (`Info::LayerWidget::countGeometry`) inside the
/// window body `body`: `st::infoDesiredWidth` wide (at least 48 px side margins), starting
/// `windowHeight / 24` (clamped to 20..40 px) from the top; long content such as Settings runs
/// to the bottom edge of the window, as in 7.4.6.
#[must_use]
pub fn settings_layer_rect(body: Rect) -> Rect {
    let width = (body.width() - 2.0 * 48.0).clamp(0.0, SETTINGS_LAYER_WIDTH);
    let top = (body.height() / 24.0).floor().clamp(20.0, 40.0);
    let height = (body.height() - top).max(0.0);
    Rect::from_min_size(
        Pos2::new((body.center().x - width * 0.5).round(), body.top() + top),
        Vec2::new(width, height),
    )
}

/// Main multi-column desktop shell (`MainWidget`) hosting Dialogs, History, Info, Settings,
/// Statistics, AudioPlayerBar, and ChatHelpers over one shared [`AppSession`].
pub struct MainWidget {
    dialogs: DialogsPanel,
    history: HistoryWidget,
    /// The chat last passed to `AppSession::prepare_open_chat`.
    prepared_chat: PeerId,
    /// Open chat and `DialogsBackend::dialogs_version` the next-unread-channel hint was found
    /// for: the chat list is searched again only when one of them changes (TGB-2949).
    next_unread_channel_key: Option<(PeerId, u64)>,
    main_info: InfoWrapWidget,
    third_info: InfoWrapWidget,
    settings: Option<SettingsController>,
    last_settings_page: Option<SettingsPage>,
    statistics: Option<StatisticsScreenHost>,
    last_stats_key: Option<(PeerId, StatisticsScreen)>,
    /// Whether the single shared `AudioPlayerBar` enables real `cpal` hardware audio output
    /// (`true` in running desktop app, `false` in `#[cfg(test)]` headless mode).
    hardware_audio_enabled: bool,
    /// Tracks whether the session's `MediaBackend` has been attached to the shared `AudioPlayerBar`.
    shared_audio_attached: bool,
    /// Snapshot of the last `CoreSettings` applied to `HistoryWidget::apply_core_settings`.
    last_applied_core_settings: Option<CoreSettings>,
    tabbed_panel: TabbedPanel,
    autocomplete: FieldAutocompleteView,
    emoji_cache: EmojiTextureCache,
    last_jumped_msg: Option<(PeerId, MsgId)>,
    pending_compose_prefill: Option<(PeerId, String)>,
    shortcuts_tdata_dir: Option<std::path::PathBuf>,
    last_theme: (EmbeddedType, Option<ThemeColor>),
    dragging_first_splitter: bool,
    dragging_third_splitter: bool,
    /// Wallpaper painter for the empty chat pane (`HistoryWidget::paintEvent` without a peer).
    empty_background: ChatBackground,
    empty_background_dark: Option<bool>,
    /// The system dark-mode value the window read this frame, handed to Chat settings.
    pub(super) system_dark_mode: Option<bool>,
    /// Whether this frame renders Settings as a centered layer instead of a main section.
    settings_as_layer: bool,
    /// Whether the Settings layer's top-bar menu (`Settings::Main::fillTopBarMenu`) is open.
    settings_layer_menu_open: bool,
    /// Whether this frame docks the tabbed selector as a third column (`TabbedSection`).
    tabbed_docked: bool,
    /// Screen rect of the docked tabbed selector column in the last frame.
    docked_tabbed_rect: Option<Rect>,
    /// Forward requested from the message menu, turned into [`MainWidgetAction::OpenForwardBox`].
    pending_forward: Option<(PeerId, Vec<MsgId>)>,
}

impl Default for MainWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl MainWidget {
    /// Creates a new `MainWidget` with real `cpal` audio output in the running app and
    /// headless software mixing in tests (`default_hardware_audio_enabled()`).
    #[must_use]
    pub fn new() -> Self {
        Self::with_hardware_audio(default_hardware_audio_enabled())
    }

    /// Creates a new `MainWidget` with an explicit hardware audio (`cpal`) toggle.
    #[must_use]
    pub fn with_hardware_audio(enable_hardware_audio: bool) -> Self {
        let mut history = HistoryWidget::new();
        // MainWidget renders the single unified AudioPlayerBar above the entire main column
        // (shared across History and Media/Info/Settings sections).
        history.set_render_internal_audio_bar(false);
        Self {
            dialogs: DialogsPanel::new(),
            history,
            prepared_chat: PeerId::ZERO,
            next_unread_channel_key: None,
            main_info: InfoWrapWidget::for_peer(PeerId::ZERO),
            third_info: InfoWrapWidget::for_peer(PeerId::ZERO),
            settings: None,
            last_settings_page: None,
            statistics: None,
            last_stats_key: None,
            hardware_audio_enabled: enable_hardware_audio,
            shared_audio_attached: false,
            last_applied_core_settings: None,
            tabbed_panel: TabbedPanel::new(),
            autocomplete: FieldAutocompleteView::new(),
            emoji_cache: EmojiTextureCache::new(),
            last_jumped_msg: None,
            pending_compose_prefill: None,
            shortcuts_tdata_dir: None,
            last_theme: (EmbeddedType::DayBlue, None),
            dragging_first_splitter: false,
            dragging_third_splitter: false,
            empty_background: ChatBackground::new(),
            empty_background_dark: None,
            system_dark_mode: None,
            settings_as_layer: false,
            settings_layer_menu_open: false,
            tabbed_docked: false,
            docked_tabbed_rect: None,
            pending_forward: None,
        }
    }

    /// Requests the "Forward to..." box for `msg_ids` of `from_peer`; the next frame emits
    /// [`MainWidgetAction::OpenForwardBox`].
    pub fn request_forward_box(&mut self, from_peer: PeerId, msg_ids: Vec<MsgId>) {
        if !msg_ids.is_empty() {
            self.pending_forward = Some((from_peer, msg_ids));
        }
    }

    /// Returns whether the last frame showed Settings as a centered layer.
    #[must_use]
    pub const fn settings_shown_as_layer(&self) -> bool {
        self.settings_as_layer
    }

    /// Returns whether the last frame docked the Emoji / Stickers / GIFs panel as a column.
    #[must_use]
    pub const fn tabbed_panel_docked(&self) -> bool {
        self.tabbed_docked
    }

    /// Returns the docked Emoji / Stickers / GIFs column rect from the last frame, if docked.
    #[must_use]
    pub const fn docked_tabbed_rect(&self) -> Option<Rect> {
        self.docked_tabbed_rect
    }

    /// Returns whether real `cpal` hardware audio output is enabled on the shared player bar.
    #[must_use]
    pub const fn hardware_audio_enabled(&self) -> bool {
        self.hardware_audio_enabled
    }

    /// Returns a reference to the single unified [`AudioPlayerBar`] shared across the history
    /// view and media sections.
    #[must_use]
    pub const fn audio_player_bar(&self) -> &AudioPlayerBar {
        self.history.media_runtime().audio_player_bar()
    }

    /// Returns a mutable reference to the single unified [`AudioPlayerBar`].
    pub fn audio_player_bar_mut(&mut self) -> &mut AudioPlayerBar {
        self.history.media_runtime_mut().audio_player_bar_mut()
    }

    /// Returns `true` if any panel transition animation owned by [`MainWidget`] is active.
    #[must_use]
    pub const fn is_animating(&self) -> bool {
        self.tabbed_panel.is_animating()
    }

    /// Returns a shared reference to the main-column [`InfoWrapWidget`] (Profile / Downloads).
    #[must_use]
    pub const fn main_info(&self) -> &InfoWrapWidget {
        &self.main_info
    }

    /// Returns mutable access to the main-column [`InfoWrapWidget`].
    pub fn main_info_mut(&mut self) -> &mut InfoWrapWidget {
        &mut self.main_info
    }

    /// Returns a shared reference to the right-side third-column [`InfoWrapWidget`].
    #[must_use]
    pub const fn third_info(&self) -> &InfoWrapWidget {
        &self.third_info
    }

    /// Returns mutable access to the right-side third-column [`InfoWrapWidget`].
    pub fn third_info_mut(&mut self) -> &mut InfoWrapWidget {
        &mut self.third_info
    }

    /// Returns a shared reference to the embedded [`HistoryWidget`].
    #[must_use]
    pub const fn history(&self) -> &HistoryWidget {
        &self.history
    }

    /// Connects profile data directories and the HTML export folder dialog into [`HistoryWidget`]
    /// so `VideoEditorModalState::draft_path` and `RichEditorWindow::set_export_dir` inherit the
    /// resolved profile directory at startup.
    pub fn set_profile_media_dirs(
        &mut self,
        video_drafts_dir: Option<std::path::PathBuf>,
        rich_export_dir: Option<std::path::PathBuf>,
        rich_export_dialog: Option<
            std::sync::Arc<dyn tg_app_media::webview::rich_editor::ExportFolderDialog>,
        >,
    ) {
        self.history.set_video_drafts_dir(video_drafts_dir);
        self.history.set_rich_export_dir(rich_export_dir);
        self.history.set_rich_export_dialog(rich_export_dialog);
    }

    /// Applies `settings` directly to [`HistoryWidget::apply_core_settings`] and
    /// [`DialogsPanel::apply_core_settings`] and remembers the applied snapshot.
    pub fn apply_core_settings(&mut self, settings: &CoreSettings) {
        self.history.apply_core_settings(settings);
        let pause_effects = settings
            .power_saving
            .contains(PowerSavingFlags::CHAT_EFFECTS)
            || settings.power_saving.contains(PowerSavingFlags::ANIMATIONS);
        self.main_info.chat_effects_paused = pause_effects;
        self.third_info.chat_effects_paused = pause_effects;
        self.dialogs.apply_core_settings(settings);
        self.last_applied_core_settings = Some(settings.clone());
    }

    /// Synchronizes live `CoreSettings` (including Advanced Power Saving flags and Chat Settings
    /// autoplay toggles) from [`SettingsController`] into [`HistoryWidget::apply_core_settings`]
    /// whenever any field changes.
    pub fn sync_live_core_settings(&mut self) {
        let Some(ctrl) = self.settings.as_mut() else {
            return;
        };
        let ps = ctrl.advanced.power_saving;
        let mut flags = PowerSavingFlags::NONE;
        flags.set(PowerSavingFlags::ANIMATIONS, ps.animations);
        flags.set(PowerSavingFlags::STICKERS_PANEL, ps.stickers_panel);
        flags.set(PowerSavingFlags::STICKERS_CHAT, ps.stickers_chat);
        flags.set(PowerSavingFlags::EMOJI_PANEL, ps.emoji_panel);
        flags.set(PowerSavingFlags::EMOJI_REACTIONS, ps.emoji_reactions);
        flags.set(PowerSavingFlags::EMOJI_CHAT, ps.emoji_chat);
        flags.set(PowerSavingFlags::EMOJI_STATUS, ps.emoji_status);
        flags.set(PowerSavingFlags::CHAT_BACKGROUND, ps.chat_background);
        flags.set(PowerSavingFlags::CHAT_SPOILER, ps.chat_spoiler);
        flags.set(PowerSavingFlags::CHAT_EFFECTS, ps.chat_effects);
        flags.set(PowerSavingFlags::CALLS, ps.calls);
        ctrl.core_settings.power_saving = flags;

        let auto_dl = ctrl.backend.auto_download();
        ctrl.core_settings.autoplay = AutoplaySettings {
            gifs: auto_dl.autoplay_gifs,
            videos: auto_dl.autoplay_videos,
            round_videos: auto_dl.autoplay_round_videos,
            video_stickers: ctrl.core_settings.autoplay.video_stickers,
        };

        let effective = ctrl.core_settings.clone();
        if self.last_applied_core_settings.as_ref() != Some(&effective) {
            self.history.apply_core_settings(&effective);
            let pause_effects = effective
                .power_saving
                .contains(PowerSavingFlags::CHAT_EFFECTS)
                || effective
                    .power_saving
                    .contains(PowerSavingFlags::ANIMATIONS);
            self.main_info.chat_effects_paused = pause_effects;
            self.third_info.chat_effects_paused = pause_effects;
            self.dialogs.apply_core_settings(&effective);
            self.last_applied_core_settings = Some(effective);
        }
    }

    fn ensure_shared_audio_attached(&mut self, backend: &dyn MediaBackend) {
        if !self.shared_audio_attached {
            self.history
                .media_runtime_mut()
                .attach_shared_media_backend(backend, self.hardware_audio_enabled);
            self.shared_audio_attached = true;
        }
    }

    /// Returns a mutable reference to the stateful [`DialogsPanel`].
    pub fn dialogs_mut(&mut self) -> &mut DialogsPanel {
        &mut self.dialogs
    }

    /// Returns a mutable reference to the [`HistoryWidget`].
    pub fn history_mut(&mut self) -> &mut HistoryWidget {
        &mut self.history
    }

    /// Forwards the startup-resolved `rustgram-widget-helper.exe` path into [`HistoryWidget`].
    pub fn set_widget_helper_path(&mut self, path: std::path::PathBuf) {
        self.history.set_widget_helper_path(path);
    }

    /// Returns the startup-resolved `rustgram-widget-helper.exe` path configured on [`HistoryWidget`].
    #[must_use]
    pub fn widget_helper_path(&self) -> Option<&std::path::Path> {
        self.history.widget_helper_path()
    }

    /// Returns a mutable reference to the floating [`TabbedPanel`] (Emoji / Stickers / GIFs).
    pub fn tabbed_panel_mut(&mut self) -> &mut TabbedPanel {
        &mut self.tabbed_panel
    }

    /// Returns a mutable reference to the compose [`FieldAutocompleteView`].
    pub fn autocomplete_mut(&mut self) -> &mut FieldAutocompleteView {
        &mut self.autocomplete
    }

    /// Returns a shared reference to the active [`SettingsController`], if initialized.
    #[must_use]
    pub fn settings(&self) -> Option<&SettingsController> {
        self.settings.as_ref()
    }

    /// Returns a mutable reference to the active [`SettingsController`], if initialized.
    pub fn settings_mut(&mut self) -> Option<&mut SettingsController> {
        self.settings.as_mut()
    }

    /// Attaches a `tdata` directory to `SettingsController.shortcuts.manager` (now or when lazily
    /// constructed), immediately loading any saved `shortcuts-custom.json`.
    pub fn attach_shortcuts_tdata_dir(&mut self, tdata_dir: &std::path::Path) {
        self.shortcuts_tdata_dir = Some(tdata_dir.to_path_buf());
        if let Some(ctrl) = self.settings.as_mut() {
            let _ = ctrl.shortcuts.manager.load_from_tdata_dir(tdata_dir);
        }
    }

    /// Polls [`AudioPlayerBar::poll_listen_reports`] and forwards completed song listen reports
    /// to [`MediaBackend::report_music_listen`].
    pub fn poll_music_listen_reports(&mut self, backend: &dyn MediaBackend, now_ms: i64) {
        let reports = self
            .history
            .media_runtime_mut()
            .audio_player_bar_mut()
            .poll_listen_reports(now_ms);
        for report in reports {
            let _ = backend.report_music_listen(report.track_id, report.duration_secs);
        }
    }

    /// Ensures the [`SettingsController`] is initialized with `settings_backend` and returns
    /// mutable access to it (used on live startup to pre-seed `CoreSettings` from `tdata`).
    pub fn ensure_settings(
        &mut self,
        settings_backend: Box<dyn SettingsBackend>,
    ) -> &mut SettingsController {
        let tdata_dir = self.shortcuts_tdata_dir.clone();
        self.settings.get_or_insert_with(|| {
            let mut ctrl = SettingsController::new(settings_backend);
            if let Some(dir) = tdata_dir.as_deref() {
                let _ = ctrl.shortcuts.manager.load_from_tdata_dir(dir);
            }
            ctrl
        })
    }

    /// Returns a mutable reference to the active [`StatisticsScreenHost`], if initialized.
    pub fn statistics_mut(&mut self) -> Option<&mut StatisticsScreenHost> {
        self.statistics.as_mut()
    }

    /// Opens a specific [`SettingsSection`] directly (e.g. clicking "Edit" in the folders sidebar).
    pub fn open_settings_section(
        &mut self,
        section: SettingsSection,
        settings_backend: Box<dyn SettingsBackend>,
        embedded: EmbeddedType,
        accent: Option<ThemeColor>,
    ) {
        self.open_settings_page_and_section(
            SettingsPage::Main,
            section,
            settings_backend,
            embedded,
            accent,
        );
    }

    /// Opens a specific `(page, section)` pair in [`SettingsController`] while keeping
    /// `last_settings_page` synchronized so the next frame does not reset `section_stack`.
    pub fn open_settings_page_and_section(
        &mut self,
        page: SettingsPage,
        section: SettingsSection,
        settings_backend: Box<dyn SettingsBackend>,
        embedded: EmbeddedType,
        accent: Option<ThemeColor>,
    ) {
        let ctrl = self.ensure_settings(settings_backend);
        ctrl.live_theme.update(embedded, accent);
        ctrl.push_section(section);
        if page == SettingsPage::Connection {
            ctrl.advanced.panel = AdvancedSubPanel::ProxyEditor;
        }
        self.last_settings_page = Some(page);
    }

    /// Resolves a `tg://setlanguage` / `t.me/setlanguage/...` link through
    /// [`SettingsController::open_set_language_link`] and opens the Language settings section.
    pub fn open_set_language_link(
        &mut self,
        url: &str,
        settings_backend: Box<dyn SettingsBackend>,
        embedded: EmbeddedType,
        accent: Option<ThemeColor>,
    ) -> Result<bool, String> {
        let ctrl = self.ensure_settings(settings_backend);
        ctrl.live_theme.update(embedded, accent);
        let has_prompt = ctrl.open_set_language_link(url)?;
        self.last_settings_page = Some(SettingsPage::Main);
        Ok(has_prompt)
    }

    /// Prefills the chat compose box for `peer_id` (e.g. from `?text=...` or `?start=...`
    /// deep links), preserving the prefill across `HistoryWidget`'s next-frame draft restore
    /// when `HistoryWidget` has not switched to `peer_id` yet.
    pub fn prefill_compose(&mut self, peer_id: PeerId, text: String) {
        let compose = self.history.compose_mut();
        compose.text = text.clone();
        compose
            .rich_edit
            .set_text_with_entities(tg_text::TextWithEntities::simple(text.clone()));
        compose.focus_requested = true;
        if self.history.active_peer() != peer_id {
            self.pending_compose_prefill = Some((peer_id, text));
        } else {
            self.pending_compose_prefill = None;
        }
    }

    /// Starts playing an audio/voice track in the single shared top [`AudioPlayerBar`].
    pub fn play_audio_track(
        &mut self,
        backend: &dyn MediaBackend,
        audio_id: AudioMsgId,
        enable_hardware_output: bool,
    ) -> anyhow::Result<()> {
        let effective_hw = self.hardware_audio_enabled || enable_hardware_output;
        self.shared_audio_attached = true;
        self.history
            .media_runtime_mut()
            .play_external_audio_track(backend, audio_id, effective_hw)
    }

    /// Reloads the active chat view when an external event (e.g. notification quick-reply)
    /// added a message to `peer_id`.
    pub fn notify_external_message(&mut self, peer_id: PeerId, backend: &dyn HistoryBackend) {
        if self.history.active_peer() == peer_id {
            self.history.reload_active_peer(backend);
        }
    }

    /// Applies a [`DialogsShortcut`] routed from [`crate::window::controller::WindowController`].
    pub fn apply_dialogs_shortcut(
        &mut self,
        shortcut: DialogsShortcut,
        session: &mut SessionController,
        session_backend: &mut dyn AppSession,
        now_ms: i64,
    ) -> Vec<MainWidgetAction> {
        let mut actions = Vec::new();
        self.dialogs.active_filter = session.active_filter();
        if let Some(act) =
            self.dialogs
                .apply_shortcut(shortcut, session_backend, session.active_peer())
        {
            self.handle_dialogs_action(act, session, session_backend, now_ms, &mut actions);
        }
        actions
    }

    /// Dismisses transient in-workspace overlays or pops internal section stacks on `Escape`.
    ///
    /// Returns `true` if Escape was consumed internally (before popping `SessionController`).
    pub fn close_overlays_by_escape(&mut self, session: &SessionController, now_secs: f64) -> bool {
        if self.tabbed_panel.is_open() {
            self.tabbed_panel.hide_animated(now_secs);
            return true;
        }
        if self.autocomplete.is_visible() {
            self.autocomplete.dismiss();
            return true;
        }
        if self.history.close_engagement_overlays() {
            return true;
        }
        if self.history.close_cocoon_or_ai_overlay() {
            return true;
        }
        if self.history.close_premium_limits() {
            return true;
        }
        if self.history.close_chat_search() {
            return true;
        }
        if let Some(sec) = self.history.active_section() {
            match sec {
                HistorySectionKind::ForumTopicHistory { peer_id, .. } => {
                    self.history
                        .open_section(HistorySectionKind::ForumTopics { peer_id });
                }
                _ => self.history.close_section(),
            }
            return true;
        }
        if self.dialogs.inner.is_context_menu_open() {
            self.dialogs.inner.close_context_menu();
            return true;
        }
        if self.dialogs.cancel_search() {
            return true;
        }
        if self.dialogs.opened_archive {
            self.dialogs.opened_archive = false;
            return true;
        }
        match session.main_section() {
            Some(SectionMemento::Settings { .. }) => {
                if let Some(ctrl) = self.settings.as_mut()
                    && ctrl.pop_section()
                {
                    return true;
                }
            }
            Some(SectionMemento::Statistics { .. }) => {
                if let Some(stats) = self.statistics.as_mut()
                    && stats.go_back()
                {
                    return true;
                }
            }
            Some(SectionMemento::PeerInfo { .. }) if self.main_info.go_back() => {
                return true;
            }
            _ => {}
        }
        false
    }

    /// Navigates back inside the currently active sub-section if it has internal depth.
    pub fn go_back_in_active_section(&mut self, session: &SessionController) -> bool {
        match session.main_section() {
            Some(SectionMemento::Settings { .. }) => self
                .settings
                .as_mut()
                .is_some_and(SettingsController::pop_section),
            Some(SectionMemento::Statistics { .. }) => self
                .statistics
                .as_mut()
                .is_some_and(StatisticsScreenHost::go_back),
            Some(SectionMemento::PeerInfo { .. }) => self.main_info.go_back(),
            _ => false,
        }
    }

    /// Renders the complete unified multi-column workspace inside `ui`.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        session: &mut SessionController,
        session_backend: &mut dyn AppSession,
        connection_indicator: &mut ConnectionIndicator,
        theme_mode: ThemeMode,
        embedded_theme: EmbeddedType,
        accent_color: Option<ThemeColor>,
        theme_colors: &dyn Colors,
        palette: &Palette,
        strings: &dyn Strings,
        now_ms: i64,
    ) -> Vec<MainWidgetAction> {
        self.show_with_stories(
            ui,
            session,
            session_backend,
            connection_indicator,
            theme_mode,
            embedded_theme,
            accent_color,
            theme_colors,
            palette,
            strings,
            now_ms,
            None,
        )
    }

    /// Renders the complete unified multi-column workspace inside `ui`, optionally embedding
    /// an [`AppStoriesHost`]'s collapsible stories strip at the top of the dialogs column.
    #[allow(clippy::too_many_arguments)]
    pub fn show_with_stories(
        &mut self,
        ui: &mut Ui,
        session: &mut SessionController,
        session_backend: &mut dyn AppSession,
        connection_indicator: &mut ConnectionIndicator,
        theme_mode: ThemeMode,
        embedded_theme: EmbeddedType,
        accent_color: Option<ThemeColor>,
        theme_colors: &dyn Colors,
        palette: &Palette,
        strings: &dyn Strings,
        now_ms: i64,
        stories: Option<&mut AppStoriesHost>,
    ) -> Vec<MainWidgetAction> {
        let mut actions = Vec::new();
        self.last_theme = (embedded_theme, accent_color);
        let full_rect = ui.max_rect();
        let window_w = full_rect.width().round() as i32;

        // Sync dialogs search query into SessionController so narrow columns widen while searching.
        let search_text = if self.dialogs.search_active && self.dialogs.search_query.is_empty() {
            " "
        } else {
            self.dialogs.search_query.as_str()
        };
        if session.search_query() != search_text {
            *session.search_query_mut() = search_text.to_owned();
        }
        let layout = session.update_layout(window_w);
        self.settings_as_layer = matches!(
            session.main_section(),
            Some(SectionMemento::Settings { .. })
        ) && full_rect.width() >= SETTINGS_LAYER_MIN_WIDTH;
        if !self.settings_as_layer {
            self.settings_layer_menu_open = false;
        }

        let show_vertical_sidebar = layout.filters_width > 0;
        let body_left = full_rect.left() + layout.filters_width as f32;

        // 1. Optional vertical folders sidebar (`dialogs::FiltersMenuWidget`, width = 72px).
        if show_vertical_sidebar {
            let sidebar_rect = Rect::from_min_size(
                full_rect.min,
                Vec2::new(layout.filters_width as f32, full_rect.height()),
            );
            self.dialogs.active_filter = session.active_filter();
            ui.scope_builder(UiBuilder::new().max_rect(sidebar_rect), |ui| {
                if let Some(act) = self
                    .dialogs
                    .show_vertical_sidebar(ui, session_backend, palette)
                {
                    self.handle_dialogs_action(act, session, session_backend, now_ms, &mut actions);
                }
            });
        }

        let body_rect = Rect::from_min_max(Pos2::new(body_left, full_rect.top()), full_rect.max);

        // 2. Column layout dispatch matching `MainWidget::updateControlsGeometry`.
        match layout.window_layout {
            WindowLayout::OneColumn => {
                self.tabbed_docked = false;
                let layer_over_dialogs = self.settings_as_layer && session.active_peer().is_zero();
                if session.is_main_section_shown() && !layer_over_dialogs {
                    self.render_main_column(
                        ui,
                        body_rect,
                        session,
                        session_backend,
                        theme_mode,
                        embedded_theme,
                        accent_color,
                        theme_colors,
                        palette,
                        strings,
                        now_ms,
                        &mut actions,
                    );
                } else {
                    self.render_dialogs_column(
                        ui,
                        body_rect,
                        !show_vertical_sidebar,
                        false,
                        !show_vertical_sidebar,
                        session,
                        session_backend,
                        palette,
                        strings,
                        now_ms,
                        stories,
                        &mut actions,
                    );
                }
            }
            WindowLayout::Normal | WindowLayout::ThreeColumn => {
                let dialogs_w = layout.dialogs_width as f32;
                let third_w = if session.third_section().is_some() {
                    layout.third_width as f32
                } else {
                    0.0
                };
                let dock_w =
                    self.update_tabbed_dock(session, body_rect.width(), dialogs_w, third_w);
                let main_w = (body_rect.width() - dialogs_w - third_w - dock_w).max(0.0);

                let dialogs_rect =
                    Rect::from_min_size(body_rect.min, Vec2::new(dialogs_w, body_rect.height()));
                let main_rect = Rect::from_min_size(
                    Pos2::new(dialogs_rect.right(), body_rect.top()),
                    Vec2::new(main_w, body_rect.height()),
                );
                let third_rect = Rect::from_min_size(
                    Pos2::new(main_rect.right(), body_rect.top()),
                    Vec2::new(third_w, body_rect.height()),
                );
                let dock_rect = Rect::from_min_size(
                    Pos2::new(main_rect.right(), body_rect.top()),
                    Vec2::new(dock_w, body_rect.height()),
                );

                self.render_dialogs_column(
                    ui,
                    dialogs_rect,
                    !show_vertical_sidebar,
                    layout.dialogs_small_column,
                    !show_vertical_sidebar,
                    session,
                    session_backend,
                    palette,
                    strings,
                    now_ms,
                    stories,
                    &mut actions,
                );

                self.render_main_column(
                    ui,
                    main_rect,
                    session,
                    session_backend,
                    theme_mode,
                    embedded_theme,
                    accent_color,
                    theme_colors,
                    palette,
                    strings,
                    now_ms,
                    &mut actions,
                );

                if third_w > 0.0 {
                    self.render_third_column(
                        ui,
                        third_rect,
                        session,
                        session_backend,
                        theme_colors,
                        &mut actions,
                    );
                }
                if dock_w > 0.0 {
                    let peer_id = session.active_peer();
                    self.render_docked_tabbed_panel(
                        ui,
                        dock_rect,
                        peer_id,
                        session_backend,
                        palette,
                        &mut actions,
                    );
                }

                // 1px column divider shadows (`_sideShadow`, `_thirdShadow`).
                ui.painter().line_segment(
                    [
                        Pos2::new(dialogs_rect.right(), body_rect.top()),
                        Pos2::new(dialogs_rect.right(), body_rect.bottom()),
                    ],
                    Stroke::new(1.0, palette.divider),
                );
                if third_w > 0.0 {
                    ui.painter().line_segment(
                        [
                            Pos2::new(third_rect.left(), body_rect.top()),
                            Pos2::new(third_rect.left(), body_rect.bottom()),
                        ],
                        Stroke::new(1.0, palette.divider),
                    );
                }

                // Resizable splitter 1 (dialogs | main), `st::historyResizeWidth = 6px`.
                self.handle_first_splitter(
                    ui,
                    body_rect,
                    dialogs_rect.right(),
                    session,
                    &mut actions,
                );

                // Resizable splitter 2 (main | third) when ThreeColumn third section is open.
                if third_w > 0.0 {
                    self.handle_third_splitter(
                        ui,
                        body_rect,
                        third_rect.left(),
                        session,
                        &mut actions,
                    );
                }
            }
        }

        // Poll AudioPlayerBar's MusicListenTracker every frame across all layout modes
        // (including OneColumn dialogs view) so track end and 60s pause reports always flush.
        self.poll_music_listen_reports(session_backend, now_ms);

        // 3. Floating bottom-left connection status pill (`_connecting`, hidden in OneColumn).
        let pill_layout = connection_indicator.update(
            tg_app_core::clock::now(),
            session_backend.connection_status(),
            session_backend.proxy_enabled(),
            strings,
        );
        if pill_layout.visible && !session.adaptive().is_one_column() {
            let pill_pos = Pos2::new(body_rect.left() + 10.0, body_rect.bottom() - 44.0);
            egui::Area::new(Id::unique("tg_connecting_pill"))
                .order(egui::Order::Foreground)
                .fixed_pos(pill_pos)
                .show(ui.ctx(), |ui| {
                    if let Some(act) = connection_indicator.show(ui, palette, strings, &pill_layout)
                    {
                        match act {
                            ConnectionWidgetAction::OpenProxySettings => {
                                actions.push(MainWidgetAction::OpenProxySettings);
                            }
                            ConnectionWidgetAction::RetryNow => {
                                actions.push(MainWidgetAction::RetryConnection);
                            }
                        }
                    }
                });
        }

        if self.settings_as_layer
            && let Some(SectionMemento::Settings { page }) = session.main_section().cloned()
        {
            self.render_settings_layer(
                ui,
                full_rect,
                page,
                session,
                session_backend,
                embedded_theme,
                accent_color,
                palette,
                &mut actions,
            );
        }

        if let Some((from_peer, msg_ids)) = self.pending_forward.take() {
            actions.push(MainWidgetAction::OpenForwardBox { from_peer, msg_ids });
        }

        if session.columns_mut().take_dirty() {
            actions.push(MainWidgetAction::PersistSettings);
        }

        actions
    }

    #[allow(clippy::too_many_arguments)]
    fn render_dialogs_column(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        show_horizontal_folders: bool,
        compact_small_column: bool,
        show_hamburger: bool,
        session: &mut SessionController,
        session_backend: &mut dyn AppSession,
        palette: &Palette,
        strings: &dyn Strings,
        now_ms: i64,
        stories: Option<&mut AppStoriesHost>,
        actions: &mut Vec<MainWidgetAction>,
    ) {
        self.dialogs.active_filter = session.active_filter();
        let active_peer = session.active_peer();
        tg_app_info::dialogs::search::sync_global_media(
            &mut self.dialogs.search_state,
            &self.dialogs.search_query,
            &*session_backend,
        );

        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
            // tdesktop puts the collapsed stories at the right end of the search field and the
            // expanded ones right under the search bar (`Dialogs::Widget::updateControlsGeometry`).
            let mut stories = stories;
            let mut strip_shown = false;
            let shown_action = self.dialogs.show_column_with_search_row(
                ui,
                session_backend,
                active_peer,
                show_horizontal_folders,
                compact_small_column,
                show_hamburger,
                false,
                palette,
                strings,
                &mut |ui, row| {
                    if let Some(host) = stories.as_deref_mut() {
                        host.show_strip(
                            ui,
                            StoriesSearchSlot {
                                field_rect: row.field_rect,
                                field_bg: row.field_bg,
                                column_rect: row.column_rect,
                                list_scroll_y: row.list_scroll_y,
                                list_scroll_max: row.list_scroll_max,
                            },
                            palette,
                        );
                        strip_shown = true;
                    }
                },
            );
            if !strip_shown && let Some(host) = stories {
                host.hide_strip();
            }
            if let Some(act) = shown_action {
                self.handle_dialogs_action(act, session, session_backend, now_ms, actions);
                // The list above was painted with the chat open before this click; the history
                // below paints the chat it opened. Run the pass again (without the input) so the
                // shown frame highlights the same chat the history shows, as tdesktop repaints
                // both together.
                if session.active_peer() != active_peer {
                    ui.ctx()
                        .request_discard("the chat list painted the chat open before the click");
                }
            }
        });
    }

    fn handle_dialogs_action(
        &mut self,
        act: DialogsPanelAction,
        session: &mut SessionController,
        session_backend: &mut dyn AppSession,
        now_ms: i64,
        actions: &mut Vec<MainWidgetAction>,
    ) {
        match act {
            DialogsPanelAction::OpenMainMenu => actions.push(MainWidgetAction::OpenMainMenu),
            DialogsPanelAction::SelectPeer(peer_id) => {
                // Not `mark_read`: like `HistoryInner::readTill`, only the messages shown on
                // screen are read, so the unread bar and counter survive until they are seen.
                let reselected = self.history.active_peer() == peer_id;
                session.show_peer_history(
                    peer_id,
                    SectionShow::clear_stack(),
                    MsgId::SHOW_AT_UNREAD,
                );
                if session_backend.is_forum_peer(peer_id) {
                    self.history
                        .open_section(HistorySectionKind::ForumTopics { peer_id });
                } else {
                    self.history.close_section();
                }
                self.history.close_monoforum_section();
                // A click on the open chat jumps to its end; a newly opened chat is positioned
                // by `ShowAtUnreadMsgId` (first unread message or the bottom).
                if reselected {
                    self.history.request_scroll_to_bottom(peer_id);
                }
                self.tabbed_panel.hide_fast();
                self.autocomplete.dismiss();
                self.last_jumped_msg = None;
            }
            DialogsPanelAction::SelectMessage { peer_id, msg_id } => {
                session.show_peer_history(peer_id, SectionShow::clear_stack(), msg_id);
                self.tabbed_panel.hide_fast();
                self.autocomplete.dismiss();
                self.last_jumped_msg = None;
                if self.history.active_peer() == peer_id {
                    self.history
                        .jump_to_message(msg_id, session_backend, now_ms);
                    self.last_jumped_msg = Some((peer_id, msg_id));
                }
            }
            DialogsPanelAction::SelectFolder(filter_id) => {
                session.set_active_filter(filter_id);
                self.dialogs.active_filter = filter_id;
            }
            DialogsPanelAction::OpenFoldersSettings => {
                let settings_backend = session_backend.create_settings_backend();
                let ctrl = self.ensure_settings(settings_backend);
                ctrl.push_section(SettingsSection::Folders);
                self.last_settings_page = Some(SettingsPage::Main);
                session.show_settings(SettingsPage::Main);
            }
            DialogsPanelAction::OpenBotApp(peer_id) => {
                actions.push(MainWidgetAction::OpenMiniApp {
                    bot_id: peer_id,
                    url: None,
                    start_param: None,
                    confirm_open: false,
                });
            }
            DialogsPanelAction::ActivateSuggestion(suggestion) => {
                let (embedded, accent) = self.last_theme;
                crate::window::shortcuts_wiring::activate_promo_suggestion(
                    self,
                    suggestion,
                    session,
                    session_backend,
                    embedded,
                    accent,
                    actions,
                );
            }
            DialogsPanelAction::Toast(msg) => actions.push(MainWidgetAction::Toast(msg)),
            DialogsPanelAction::OpenInNewWindow(peer_id) => {
                actions.push(MainWidgetAction::OpenInNewWindow(peer_id));
            }
            DialogsPanelAction::OpenMuteFor(peer_id) => {
                actions.push(MainWidgetAction::InfoEvent(InfoWrapEvent::OpenMuteFor(
                    peer_id,
                )));
            }
            DialogsPanelAction::OpenSelectTone(peer_id) => {
                actions.push(MainWidgetAction::InfoEvent(InfoWrapEvent::OpenSelectTone(
                    peer_id,
                )));
            }
            DialogsPanelAction::ConfirmDelete {
                peer_id,
                just_clear,
            } => {
                actions.push(MainWidgetAction::InfoEvent(if just_clear {
                    InfoWrapEvent::OpenClearHistory(peer_id)
                } else {
                    InfoWrapEvent::OpenDelete {
                        peer_id,
                        message_ids: Vec::new(),
                    }
                }));
            }
            // `Filler::addBlockUser`: a blocked bot restarts, a user gets `PeerMenuBlockUserBox`,
            // unblocking and blocking a bot happen directly.
            DialogsPanelAction::ToggleBlock(peer_id) => {
                let profile =
                    tg_app_core::info::backend::InfoBackend::peer_profile(session_backend, peer_id);
                let kind = profile.as_ref().map(|p| p.kind);
                let blocked = profile.as_ref().is_some_and(|p| p.is_blocked);
                if blocked && kind == Some(tg_app_info::info::InfoPeerKind::Bot) {
                    actions.push(MainWidgetAction::InfoEvent(InfoWrapEvent::RestartBot(
                        peer_id,
                    )));
                } else if !blocked
                    && let Some(p) =
                        profile.filter(|p| p.kind == tg_app_info::info::InfoPeerKind::User)
                {
                    let name = p
                        .title
                        .split_whitespace()
                        .next()
                        .unwrap_or(&p.title)
                        .to_owned();
                    actions.push(MainWidgetAction::InfoEvent(InfoWrapEvent::OpenBlockUser {
                        peer_id,
                        name,
                    }));
                } else if let Some(toast) = tg_app_info::dialogs::inner::toggle_block_from_chat_list(
                    session_backend,
                    peer_id,
                ) {
                    actions.push(MainWidgetAction::Toast(toast));
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_main_column(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        session: &mut SessionController,
        session_backend: &mut dyn AppSession,
        theme_mode: ThemeMode,
        embedded_theme: EmbeddedType,
        accent_color: Option<ThemeColor>,
        theme_colors: &dyn Colors,
        palette: &Palette,
        strings: &dyn Strings,
        now_ms: i64,
        actions: &mut Vec<MainWidgetAction>,
    ) {
        let slide_offset = session
            .active_slide_offset(tg_app_core::clock::now(), rect.width().min(140.0))
            .unwrap_or(0.0);
        if session.is_animating() {
            ui.ctx().request_repaint();
        }
        let shifted_rect = rect.translate(Vec2::new(slide_offset, 0.0));

        // Ensure the single shared AudioPlayerBar is wired to the session's MediaBackend (with
        // real cpal in app, headless in tests) and propagate live CoreSettings into HistoryWidget.
        self.ensure_shared_audio_attached(session_backend);
        self.sync_live_core_settings();

        // Single unified top AudioPlayerBar docked above the main column (shared across History
        // and Media/Info/Settings sections).
        let mut content_top = shifted_rect.top();
        if self.history.media_runtime().audio_player_bar().is_visible() {
            let bar_rect = Rect::from_min_size(
                shifted_rect.min,
                Vec2::new(shifted_rect.width(), MEDIA_PLAYER_HEIGHT),
            );
            let mut bar_ev = None;
            ui.scope_builder(UiBuilder::new().max_rect(bar_rect), |ui| {
                ui.set_clip_rect(rect);
                bar_ev = self
                    .history
                    .media_runtime_mut()
                    .show_top_audio_bar_with_base(
                        ui,
                        Some(session_backend as &dyn MediaBackend),
                        palette,
                        now_ms,
                    );
            });
            if let Some(ev) = bar_ev {
                match ev {
                    AudioPlayerBarEvent::Closed => {
                        self.history.media_runtime_mut().clear_unmuted_round_video();
                    }
                    AudioPlayerBarEvent::ShowItem(full_id) => {
                        let pid = PeerId(full_id.peer.0);
                        let mid = MsgId(
                            full_id
                                .msg
                                .0
                                .clamp(i64::from(i32::MIN), i64::from(i32::MAX))
                                as i32,
                        );
                        session.show_peer_history(pid, SectionShow::clear_stack(), mid);
                        self.last_jumped_msg = None;
                    }
                    AudioPlayerBarEvent::TogglePlaylist => {
                        actions.push(MainWidgetAction::Toast(
                            "Audio playlist active in top player bar".to_owned(),
                        ));
                    }
                }
            }
            content_top = bar_rect.bottom();
        }

        let content_rect = Rect::from_min_max(
            Pos2::new(shifted_rect.left(), content_top),
            shifted_rect.max,
        );

        ui.scope_builder(UiBuilder::new().max_rect(content_rect), |ui| {
            ui.set_clip_rect(rect);
            match session.main_section().cloned() {
                Some(SectionMemento::Settings { .. }) if self.settings_as_layer => {
                    self.render_history_section(
                        ui,
                        session,
                        session_backend,
                        palette,
                        strings,
                        now_ms,
                        actions,
                    );
                }
                Some(SectionMemento::Settings { page }) => {
                    self.render_settings_section(
                        ui,
                        page,
                        session,
                        session_backend,
                        embedded_theme,
                        accent_color,
                        actions,
                    );
                }
                Some(SectionMemento::Statistics { peer_id, screen }) => {
                    self.render_statistics_section(
                        ui,
                        peer_id,
                        screen,
                        session,
                        session_backend,
                        theme_mode,
                        actions,
                    );
                }
                Some(SectionMemento::PeerInfo { peer_id, tab }) => {
                    if tab == crate::window::section::InfoTab::Downloads {
                        self.main_info.show_downloads_root(peer_id);
                    } else {
                        self.main_info.set_peer(peer_id);
                    }
                    let events = self
                        .main_info
                        .show(ui, session_backend, false, theme_colors);
                    for ev in events {
                        match ev {
                            InfoWrapEvent::CloseOrBack => {
                                session.show_back_from_stack(SectionShow::forward());
                            }
                            InfoWrapEvent::OpenHistory(pid) => {
                                session.show_peer_history(
                                    pid,
                                    SectionShow::forward(),
                                    MsgId::SHOW_AT_UNREAD,
                                );
                            }
                            other => actions.push(MainWidgetAction::InfoEvent(other)),
                        }
                    }
                }
                Some(SectionMemento::PeerHistory { .. }) | None => {
                    self.render_history_section(
                        ui,
                        session,
                        session_backend,
                        palette,
                        strings,
                        now_ms,
                        actions,
                    );
                }
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn render_settings_section(
        &mut self,
        ui: &mut Ui,
        page: SettingsPage,
        session: &mut SessionController,
        session_backend: &mut dyn AppSession,
        embedded_theme: EmbeddedType,
        accent_color: Option<ThemeColor>,
        actions: &mut Vec<MainWidgetAction>,
    ) {
        if self.settings.is_none() {
            self.ensure_settings(session_backend.create_settings_backend());
        }
        let Some(ctrl) = self.settings.as_mut() else {
            return;
        };

        ctrl.host_top_bar = self.settings_as_layer;
        ctrl.core_settings.chat_filters_horizontal = !session.filters_enabled();
        ctrl.live_theme.update(embedded_theme, accent_color);
        ctrl.chat.system_dark_mode = self.system_dark_mode;
        ctrl.privacy.top_peers_enabled =
            tg_app_info::dialogs::backend::DialogsBackend::top_peers_enabled(session_backend);

        if self.last_settings_page != Some(page) {
            self.last_settings_page = Some(page);
            let mapped = SettingsSection::from_shell_page(page);
            ctrl.section_stack.clear();
            ctrl.section_stack.push(SettingsSection::Main);
            if mapped != SettingsSection::Main {
                ctrl.section_stack.push(mapped);
            }
            if page == SettingsPage::Connection {
                ctrl.advanced.panel = AdvancedSubPanel::ProxyEditor;
            }
        }

        let before_kind = ctrl.live_theme.kind;
        let before_accent = ctrl.live_theme.accent;
        let before_lang = ctrl.language.active_lang_code.clone();

        let emitted = ctrl.show(ui, session_backend.connection_status(), session.layout());

        if ctrl.live_theme.kind != before_kind || ctrl.live_theme.accent != before_accent {
            actions.push(MainWidgetAction::ApplyTheme {
                kind: ctrl.live_theme.kind,
                accent: ctrl.live_theme.accent,
            });
        }
        if ctrl.language.active_lang_code != before_lang {
            actions.push(MainWidgetAction::ApplyLanguage(
                ctrl.language.active_lang_code.clone(),
            ));
        }

        if let Some(act) = emitted {
            match act {
                SettingsAction::Back => {
                    self.last_settings_page = None;
                    session.show_back_from_stack(SectionShow::forward());
                }
                SettingsAction::NavigatePage(next_page) => {
                    session.show_settings(next_page);
                }
                SettingsAction::ToggleTheme => {
                    actions.push(MainWidgetAction::ToggleTheme);
                }
                SettingsAction::ToggleFoldersSidebar => {
                    actions.push(MainWidgetAction::ToggleFoldersSidebar);
                }
                SettingsAction::OpenPasscodeSetup => {
                    actions.push(MainWidgetAction::OpenPasscodeSetup);
                }
                SettingsAction::LockNow => {
                    actions.push(MainWidgetAction::LockNow);
                }
                SettingsAction::SetConnectionStatus(status) => {
                    session_backend.set_connection_status(status);
                }
                SettingsAction::SetProxyEnabled(enabled) => {
                    session_backend.set_proxy_enabled(enabled);
                }
                SettingsAction::OpenPayments(screen) => {
                    actions.push(MainWidgetAction::OpenPayments { screen, slug: None });
                }
                SettingsAction::OpenBusiness => {
                    actions.push(MainWidgetAction::OpenBusiness);
                }
                SettingsAction::OpenEditSelfColor => {
                    actions.push(MainWidgetAction::OpenEditSelfColor);
                }
                SettingsAction::ToggleTopPeers(enabled) => {
                    let _ = tg_app_info::dialogs::backend::DialogsBackend::toggle_top_peers(
                        session_backend,
                        enabled,
                    );
                    ctrl.privacy.top_peers_enabled =
                        tg_app_info::dialogs::backend::DialogsBackend::top_peers_enabled(
                            session_backend,
                        );
                }
                SettingsAction::RequestLogout => {
                    actions.push(MainWidgetAction::RequestLogout);
                }
            }
        }

        // Push any settings modified in this frame directly into HistoryWidget::apply_core_settings.
        self.sync_live_core_settings();
    }

    #[allow(clippy::too_many_arguments)]
    fn render_statistics_section(
        &mut self,
        ui: &mut Ui,
        peer_id: PeerId,
        screen: StatisticsScreen,
        session: &mut SessionController,
        session_backend: &mut dyn AppSession,
        theme_mode: ThemeMode,
        actions: &mut Vec<MainWidgetAction>,
    ) {
        session_backend.set_stats_target_peer(peer_id);
        let stats = self.statistics.get_or_insert_with(|| {
            StatisticsScreenHost::new(session_backend.create_stats_backend(), theme_mode)
        });
        if self.last_stats_key != Some((peer_id, screen)) {
            stats.open_screen_fresh(screen);
            self.last_stats_key = Some((peer_id, screen));
        }
        stats.sync_theme(theme_mode);
        let closed = stats.show(ui, true);
        if stats.take_open_affiliate_peer().is_some() {
            actions.push(bot_earn_affiliate_action(
                session_backend.peer_summary(peer_id).as_ref(),
            ));
        }
        if closed {
            self.last_stats_key = None;
            session.show_back_from_stack(SectionShow::forward());
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_history_section(
        &mut self,
        ui: &mut Ui,
        session: &mut SessionController,
        session_backend: &mut dyn AppSession,
        palette: &Palette,
        strings: &dyn Strings,
        now_ms: i64,
        actions: &mut Vec<MainWidgetAction>,
    ) {
        let peer_id = session.active_peer();
        let can_go_back = history_can_go_back(session, self.settings_as_layer);
        let info_open = session.third_section().is_some();
        let is_chat_wide = session.adaptive().is_chat_wide();

        if peer_id.is_zero() {
            self.render_empty_chat_pane(ui, palette, strings);
            return;
        }
        if self.prepared_chat != peer_id {
            self.prepared_chat = peer_id;
            session_backend.prepare_open_chat(peer_id);
        }

        // Populate next unread broadcast channel hint for `PullToNextChannel`. Searching it
        // converts the whole chat list, so it runs when the open chat or the list's data changed,
        // not on every frame.
        let list_version =
            tg_app_info::dialogs::backend::DialogsBackend::dialogs_version(&*session_backend);
        if self.next_unread_channel_key != Some((peer_id, list_version)) {
            self.next_unread_channel_key = Some((peer_id, list_version));
            let next_unread_channel = Backend::dialogs(
                session_backend,
                tg_app_core::stand_in::types::FilterId::ALL,
                "",
            )
            .into_iter()
            .find(|d| {
                d.peer.id != peer_id
                    && d.peer.kind == tg_app_core::stand_in::types::PeerKind::Channel
                    && d.unread_count > 0
            })
            .map(|d| {
                tg_app_history::history::pull_to_next_channel::NextUnreadChannelHint {
                    peer_id: d.peer.id,
                    title: d.peer.title,
                    unread_count: d.unread_count,
                    color_index: (d.peer.id.bare() % 8) as u8,
                }
            });
            self.history.set_next_unread_channel(next_unread_channel);
        }

        let mode_before = self.history.compose_mut().mode.clone();
        let emitted = self.history.show_with_backend(
            ui,
            peer_id,
            session_backend,
            can_go_back,
            info_open,
            is_chat_wide,
            palette,
            strings,
        );
        self.intercept_message_menu_forward(peer_id, mode_before);

        let target_msg = session.active_msg_id();
        if target_msg.0 > 0 && self.last_jumped_msg != Some((peer_id, target_msg)) {
            self.history
                .jump_to_message(target_msg, session_backend, now_ms);
            self.last_jumped_msg = Some((peer_id, target_msg));
        }

        if self
            .pending_compose_prefill
            .as_ref()
            .is_some_and(|(target_peer, _)| *target_peer == peer_id)
            && let Some((_, text)) = self.pending_compose_prefill.take()
        {
            let compose = self.history.compose_mut();
            compose.text = text.clone();
            compose
                .rich_edit
                .set_text_with_entities(tg_text::TextWithEntities::simple(text));
            compose.focus_requested = true;
        }

        let now_secs = ui.ctx().input(|i| i.time);
        if let Some(act) = emitted {
            match act {
                HistoryAction::Back => {
                    self.tabbed_panel.hide_fast();
                    session.show_back_from_stack(SectionShow::forward());
                }
                HistoryAction::TogglePeerInfo(pid) => {
                    session.toggle_peer_info(pid);
                }
                HistoryAction::SendMessage { peer_id, .. } => {
                    Backend::mark_read(session_backend, peer_id);
                }
                HistoryAction::OpenMedia {
                    peer_id,
                    msg_id,
                    album_index,
                } => {
                    actions.push(MainWidgetAction::OpenHistoryMedia {
                        peer_id,
                        msg_id,
                        album_index,
                    });
                }
                HistoryAction::OpenLink(url) => {
                    actions.push(MainWidgetAction::OpenLink(url));
                }
                HistoryAction::OpenHiddenLink { url, force_confirm } => {
                    actions.push(MainWidgetAction::OpenHiddenLink { url, force_confirm });
                }
                HistoryAction::OpenInvoice { invoice_id, .. } => {
                    actions.push(MainWidgetAction::OpenPayments {
                        screen: tg_app_info::payments::PaymentsScreen::Checkout,
                        slug: Some(invoice_id),
                    });
                }
                HistoryAction::UnlockPaidMedia {
                    peer_id,
                    msg_id,
                    stars_amount,
                    title,
                } => {
                    actions.push(MainWidgetAction::OpenPayments {
                        screen: tg_app_info::payments::PaymentsScreen::Stars,
                        slug: Some(format!(
                            "paid_media:{}:{}:{}:{}",
                            peer_id.0, msg_id.0, stars_amount, title
                        )),
                    });
                }
                HistoryAction::OpenMiniApp {
                    bot_id,
                    url,
                    start_param,
                    confirm_open,
                } => {
                    actions.push(MainWidgetAction::OpenMiniApp {
                        bot_id,
                        url,
                        start_param,
                        confirm_open,
                    });
                }
                HistoryAction::SwitchInlineChooseChat {
                    bot_id,
                    bot_username,
                    query,
                    peer_types,
                } => {
                    actions.push(MainWidgetAction::SwitchInlineChooseChat {
                        bot_id,
                        bot_username,
                        query,
                        peer_types,
                    });
                }
                HistoryAction::SwitchInlineToPeer {
                    target_peer,
                    bot_id,
                    bot_username,
                    query,
                } => {
                    actions.push(MainWidgetAction::SwitchInlineToPeer {
                        target_peer,
                        bot_id,
                        bot_username,
                        query,
                    });
                }
                HistoryAction::ToggleEmojiPanel(_) => {
                    self.tabbed_panel.toggle_animated(now_secs);
                }
                HistoryAction::OpenStory { peer_id, story_id } => {
                    actions.push(MainWidgetAction::OpenStory { peer_id, story_id });
                }
                HistoryAction::OpenGiveawayInfo {
                    peer_id,
                    msg_id,
                    channel_id,
                    launch_msg_id,
                } => {
                    actions.push(MainWidgetAction::OpenGiveawayInfo {
                        peer_id,
                        msg_id,
                        channel_id,
                        launch_msg_id,
                    });
                }
                HistoryAction::OpenVideoStream {
                    peer_id,
                    msg_id,
                    call_id,
                    access_hash,
                    rtmp_stream,
                    title,
                } => {
                    actions.push(MainWidgetAction::OpenVideoStream {
                        peer_id,
                        msg_id,
                        call_id,
                        access_hash,
                        rtmp_stream,
                        title,
                    });
                }
                HistoryAction::OpenModerateMessages {
                    peer_id,
                    peer_title,
                    message_ids,
                    participants,
                } => {
                    actions.push(MainWidgetAction::OpenModerateMessages {
                        peer_id,
                        peer_title,
                        message_ids,
                        participants,
                    });
                }
                HistoryAction::OpenMusicAttach(pid) => {
                    actions.push(MainWidgetAction::OpenMusicAttach(pid));
                }
                HistoryAction::OpenNextUnreadChannel(next_peer_id) => {
                    session.show_peer_history(
                        next_peer_id,
                        SectionShow::clear_stack(),
                        MsgId::SHOW_AT_UNREAD,
                    );
                }
                HistoryAction::OpenPeerProfile(uid) => {
                    session.show_peer_info(uid, SectionShow::forward());
                }
                HistoryAction::Toast(msg) => actions.push(MainWidgetAction::Toast(msg)),
            }
        }
        for error in self.history.take_user_errors() {
            actions.push(MainWidgetAction::UserError(error));
        }

        // Wire chat_helpers FieldAutocompleteView & TabbedPanel directly to the compose bar.
        let compose_rect = self.history.last_compose_rect();
        if compose_rect.is_positive() {
            self.render_chat_helpers(
                ui.ctx(),
                compose_rect,
                peer_id,
                session_backend,
                palette,
                actions,
            );
        }
    }

    fn render_chat_helpers(
        &mut self,
        ctx: &egui::Context,
        compose_rect: Rect,
        peer_id: PeerId,
        session_backend: &mut dyn AppSession,
        palette: &Palette,
        actions: &mut Vec<MainWidgetAction>,
    ) {
        let now_secs = ctx.input(|i| i.time);
        let now_ms_u64 = (now_secs.max(0.0) * 1000.0) as u64;

        // 1. Field autocomplete (`@mention`, `#hashtag`, `/command`, `:emoji`, single-emoji stickers).
        // When Business `/` quick-reply autocomplete is active in a private chat, defer to it
        // rather than colliding with bot-command autocomplete.
        let text_snap = self.history.compose_mut().text.clone();
        let cursor_byte = text_snap.len();
        if self.history.compose_mut().is_quick_reply_popup_visible() {
            self.autocomplete.dismiss();
        } else {
            self.autocomplete
                .update_from_input(&text_snap, cursor_byte, false, session_backend);
        }
        let ac_action = if self.history.compose_mut().is_quick_reply_popup_visible() {
            None
        } else {
            self.autocomplete
                .handle_keyboard(ctx, &text_snap, cursor_byte, session_backend)
                .or_else(|| {
                    self.autocomplete.show(
                        ctx,
                        compose_rect,
                        &text_snap,
                        cursor_byte,
                        session_backend,
                        &mut self.emoji_cache,
                        palette,
                    )
                })
        };
        if let Some(act) = ac_action {
            match act {
                AutocompleteAction::ReplaceText(ins) => {
                    self.history.compose_mut().text = ins.text;
                    self.history
                        .compose_mut()
                        .draft_scheduler
                        .mark_edited(now_ms_u64);
                    self.history.compose_mut().focus_requested = true;
                }
                AutocompleteAction::SendBotCommand { command, .. } => {
                    let req = SendMessageRequest {
                        text: TextWithEntities::simple(command),
                        reply_to: None,
                        silent: false,
                    };
                    if HistoryBackend::send_message(session_backend, peer_id, req).is_ok() {
                        self.history.compose_mut().text.clear();
                        self.history.reload_active_peer(session_backend);
                    }
                }
                AutocompleteAction::SendSticker(doc_id) => {
                    let (reply_to, silent) = {
                        let c = self.history.compose_mut();
                        let reply = match &c.mode {
                            tg_app_history::history::compose::ComposeMode::Reply {
                                msg_id, ..
                            } => Some(*msg_id),
                            _ => None,
                        };
                        (reply, c.silent_mode)
                    };
                    match session_backend.send_sticker(peer_id, doc_id, reply_to, silent) {
                        Ok(_) => {
                            self.history.compose_mut().text.clear();
                            if reply_to.is_some() {
                                self.history.compose_mut().mode =
                                    tg_app_history::history::compose::ComposeMode::Normal;
                            }
                            self.history.reload_active_peer(session_backend);
                        }
                        Err(err) => {
                            if let Some(text) =
                                tg_app_core::user_error::send_message(&err, send_target(peer_id))
                                    .into_text()
                            {
                                actions.push(MainWidgetAction::Toast(text));
                            }
                        }
                    }
                }
                AutocompleteAction::PremiumRequired(_) => {
                    actions.push(MainWidgetAction::Toast(
                        "Telegram Premium is required for this sticker".to_owned(),
                    ));
                }
            }
        }

        // 2. Floating Emoji / Stickers / GIFs tabbed panel anchored above compose bar (the docked
        //    third-column mode renders in `render_docked_tabbed_panel`).
        if self.tabbed_panel.is_rendered() && !self.tabbed_docked {
            let mut panel_events = Vec::new();
            let _ = self.tabbed_panel.show(
                ctx,
                compose_rect,
                session_backend,
                &mut self.emoji_cache,
                palette,
                |ev| panel_events.push(ev),
            );
            self.handle_tabbed_panel_events(
                panel_events,
                peer_id,
                session_backend,
                now_secs,
                actions,
            );
        }
    }

    fn handle_tabbed_panel_events(
        &mut self,
        panel_events: Vec<ChatHelpersEvent>,
        peer_id: PeerId,
        session_backend: &mut dyn AppSession,
        now_secs: f64,
        actions: &mut Vec<MainWidgetAction>,
    ) {
        let now_ms_u64 = (now_secs.max(0.0) * 1000.0) as u64;
        let docked = self.tabbed_docked;
        {
            for ev in panel_events {
                match ev {
                    ChatHelpersEvent::EmojiChosen { emoji } => {
                        self.history.insert_emoji_from_panel(&emoji, now_ms_u64);
                    }
                    ChatHelpersEvent::StickerChosen { document_id, .. } => {
                        let (reply_to, silent) = {
                            let c = self.history.compose_mut();
                            let reply = match &c.mode {
                                tg_app_history::history::compose::ComposeMode::Reply {
                                    msg_id,
                                    ..
                                } => Some(*msg_id),
                                _ => None,
                            };
                            (reply, c.silent_mode)
                        };
                        match session_backend.send_sticker(peer_id, document_id, reply_to, silent) {
                            Ok(_) => {
                                if reply_to.is_some() {
                                    self.history.compose_mut().mode =
                                        tg_app_history::history::compose::ComposeMode::Normal;
                                }
                                self.history.reload_active_peer(session_backend);
                                if !docked {
                                    self.tabbed_panel.hide_animated(now_secs);
                                }
                            }
                            Err(err) => {
                                if let Some(text) = tg_app_core::user_error::send_message(
                                    &err,
                                    send_target(peer_id),
                                )
                                .into_text()
                                {
                                    actions.push(MainWidgetAction::Toast(text));
                                }
                            }
                        }
                    }
                    ChatHelpersEvent::GifChosen { gif_id, title } => {
                        let (reply_to, silent) = {
                            let c = self.history.compose_mut();
                            let reply = match &c.mode {
                                tg_app_history::history::compose::ComposeMode::Reply {
                                    msg_id,
                                    ..
                                } => Some(*msg_id),
                                _ => None,
                            };
                            (reply, c.silent_mode)
                        };
                        match session_backend.send_gif(peer_id, gif_id, &title, reply_to, silent) {
                            Ok(_) => {
                                if reply_to.is_some() {
                                    self.history.compose_mut().mode =
                                        tg_app_history::history::compose::ComposeMode::Normal;
                                }
                                self.history.reload_active_peer(session_backend);
                                if !docked {
                                    self.tabbed_panel.hide_animated(now_secs);
                                }
                            }
                            Err(err) => {
                                if let Some(text) = tg_app_core::user_error::send_message(
                                    &err,
                                    send_target(peer_id),
                                )
                                .into_text()
                                {
                                    actions.push(MainWidgetAction::Toast(text));
                                }
                            }
                        }
                    }
                    ChatHelpersEvent::PremiumRequired { reason, .. } => {
                        actions.push(MainWidgetAction::Toast(reason));
                    }
                    ChatHelpersEvent::StickerSetInstalled(_) => {
                        actions.push(MainWidgetAction::Toast("Sticker pack installed".to_owned()));
                    }
                    ChatHelpersEvent::StickerSetRemoved(_) => {
                        actions.push(MainWidgetAction::Toast("Sticker pack removed".to_owned()));
                    }
                    ChatHelpersEvent::RecentStickersCleared => {
                        actions.push(MainWidgetAction::Toast(
                            "Recent stickers cleared".to_owned(),
                        ));
                    }
                    ChatHelpersEvent::CloseRequested => {
                        self.tabbed_panel.hide_animated(now_secs);
                    }
                }
            }
        }
    }

    /// Decides whether the tabbed selector docks as a third column this frame
    /// (`HistoryWidget::toggleTabbedSelectorMode` with `canShowThirdSection`) and returns the
    /// column width to reserve (`0.0` keeps the floating panel).
    fn update_tabbed_dock(
        &mut self,
        session: &SessionController,
        body_w: f32,
        dialogs_w: f32,
        third_w: f32,
    ) -> f32 {
        let history_shown = !session.active_peer().is_zero()
            && match session.main_section() {
                Some(SectionMemento::PeerHistory { .. }) | None => true,
                Some(SectionMemento::Settings { .. }) => self.settings_as_layer,
                Some(_) => false,
            };
        let eligible = history_shown
            && third_w <= 0.0
            && body_w >= minimal_three_column_width(dialogs_w > 0.0) as f32
            && body_w - dialogs_w - TABBED_DOCK_WIDTH >= COLUMN_MIN_WIDTH_MAIN as f32;
        if eligible {
            // A docked section has no show/hide animation.
            if self.tabbed_panel.is_rendered() && !self.tabbed_panel.is_open() {
                self.tabbed_panel.hide_fast();
            } else if self.tabbed_panel.is_open() && self.tabbed_panel.is_animating() {
                self.tabbed_panel.show_immediate();
            }
        }
        self.tabbed_docked = eligible && self.tabbed_panel.is_open();
        self.docked_tabbed_rect = None;
        if self.tabbed_docked {
            TABBED_DOCK_WIDTH
        } else {
            0.0
        }
    }

    /// Renders the docked full-height Emoji / Stickers / GIFs column (`ChatHelpers::TabbedSection`)
    /// through [`TabbedPanel::show_docked`] in the slot reserved by [`Self::update_tabbed_dock`].
    fn render_docked_tabbed_panel(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        peer_id: PeerId,
        session_backend: &mut dyn AppSession,
        palette: &Palette,
        actions: &mut Vec<MainWidgetAction>,
    ) {
        self.docked_tabbed_rect = Some(rect);
        let now_secs = ui.ctx().input(|i| i.time);
        let mut events = Vec::new();
        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
            ui.set_clip_rect(rect);
            let _ = self.tabbed_panel.show_docked(
                ui,
                rect,
                session_backend,
                &mut self.emoji_cache,
                palette,
                |ev| events.push(ev),
            );
        });
        self.handle_tabbed_panel_events(events, peer_id, session_backend, now_secs, actions);
    }

    /// Turns a Forward chosen in the message menu / selection bar this frame (which
    /// `HistoryWidget` starts as an in-place `ComposeMode::Forward`) into the "Forward to..."
    /// box, restoring the composer.
    fn intercept_message_menu_forward(&mut self, peer_id: PeerId, mode_before: ComposeMode) {
        if matches!(mode_before, ComposeMode::Forward { .. }) {
            return;
        }
        let compose = self.history.compose_mut();
        let started = match &compose.mode {
            ComposeMode::Forward {
                from_peer, msg_ids, ..
            } if *from_peer == peer_id => Some((*from_peer, msg_ids.clone())),
            _ => None,
        };
        if let Some((from_peer, msg_ids)) = started {
            compose.mode = mode_before;
            self.request_forward_box(from_peer, msg_ids);
        }
    }

    /// Empty chat pane (`HistoryWidget::paintEvent` without a peer): the chat wallpaper with the
    /// centered `lng_willbe_history` service pill (`msgServiceFont`, `msgServicePadding`,
    /// `msgPadding`), raised by the hidden composer height.
    fn render_empty_chat_pane(&mut self, ui: &mut Ui, palette: &Palette, strings: &dyn Strings) {
        let rect = ui.max_rect();
        let dark = is_dark_palette(palette);
        if self.empty_background_dark != Some(dark) {
            self.empty_background.set_dark_mode(dark);
            self.empty_background.set_global(if dark {
                BackgroundConfig::solid(ThemeColor::rgb(0x0E, 0x16, 0x21))
            } else {
                BackgroundConfig::default()
            });
            self.empty_background_dark = Some(dark);
        }
        self.empty_background.paint(ui, rect, None);

        let fg = Color32::WHITE;
        let galley = ui.painter().layout_no_wrap(
            strings.select_chat_prompt().to_owned(),
            font_id_from_style(st::semibold_font),
            fg,
        );
        let w = galley.size().x + 11.0 + 11.0;
        let h = galley.size().y + 3.0 + 4.0;
        let composer_h = 40.0;
        let top = 7.0 + ((rect.height() - composer_h - h - 7.0 - 7.0) / 2.0).max(0.0);
        let pill = Rect::from_min_size(
            Pos2::new(
                (rect.center().x - w * 0.5).round(),
                (rect.top() + top).round(),
            ),
            Vec2::new(w, h),
        );
        let bg = if dark {
            Color32::from_rgb(0x1C, 0x29, 0x36)
        } else {
            Color32::from_black_alpha(0x48)
        };
        ui.painter()
            .rect_filled(pill, CornerRadius::same((h * 0.5).round() as u8), bg);
        ui.painter()
            .galley(Pos2::new(pill.left() + 11.0, pill.top() + 3.0), galley, fg);
    }

    /// Renders Settings as tdesktop's centered `Info::LayerWidget` over a dimmed scrim, with the
    /// layer top bar (`st::infoLayerTopBar`: title, search, menu and close buttons).
    #[allow(clippy::too_many_arguments)]
    fn render_settings_layer(
        &mut self,
        ui: &mut Ui,
        body: Rect,
        page: SettingsPage,
        session: &mut SessionController,
        session_backend: &mut dyn AppSession,
        embedded_theme: EmbeddedType,
        accent_color: Option<ThemeColor>,
        palette: &Palette,
        actions: &mut Vec<MainWidgetAction>,
    ) {
        let ctx = ui.ctx().clone();
        let layer = settings_layer_rect(body);
        let radius = st::box_radius.0.clamp(0, 32) as u8;
        let area_id = ui.make_persistent_id("tg_settings_layer");
        let mut header_event = None;
        Area::new(area_id)
            .order(Order::Middle)
            .fixed_pos(body.min)
            .constrain(false)
            .show(&ctx, |ui| {
                ui.set_clip_rect(body);
                let scrim = ui.interact(body, area_id.with("scrim"), Sense::click());
                ui.painter()
                    .rect_filled(body, CornerRadius::ZERO, palette.layer_backdrop);
                ui.painter()
                    .rect_filled(layer, CornerRadius::same(radius), palette.window_bg);

                // Settings body below the layer header, which replaces the controller's own
                // top bar (one search control, as in `Info::LayerWidget`).
                let header = Rect::from_min_size(
                    layer.min,
                    Vec2::new(layer.width(), SETTINGS_LAYER_HEADER_HEIGHT),
                );
                let content =
                    Rect::from_min_max(Pos2::new(layer.left(), header.bottom()), layer.max);
                ui.scope_builder(UiBuilder::new().max_rect(content), |ui| {
                    ui.set_clip_rect(content);
                    self.render_settings_section(
                        ui,
                        page,
                        session,
                        session_backend,
                        embedded_theme,
                        accent_color,
                        actions,
                    );
                });

                header_event = self.settings_layer_header(ui, header, radius, palette);

                if scrim.clicked()
                    && ctx
                        .input(|i| i.pointer.interact_pos())
                        .is_some_and(|p| !layer.contains(p))
                {
                    header_event = Some(SettingsLayerEvent::Close);
                }
            });

        if self.settings_layer_menu_open {
            self.settings_layer_menu(&ctx, layer, palette, &mut header_event);
        }

        let Some(event) = header_event else {
            return;
        };
        match event {
            SettingsLayerEvent::Close => {
                self.settings_layer_menu_open = false;
                self.last_settings_page = None;
                set_settings_search_open(&ctx, false);
                session.show_back_from_stack(SectionShow::forward());
            }
            SettingsLayerEvent::Back => {
                if let Some(ctrl) = self.settings.as_mut() {
                    ctrl.pop_section();
                }
            }
            SettingsLayerEvent::ToggleMenu => {
                self.settings_layer_menu_open = !self.settings_layer_menu_open;
            }
            SettingsLayerEvent::Search => {
                if let Some(ctrl) = self.settings.as_mut() {
                    ctrl.section_stack.truncate(1);
                }
                set_settings_search_open(&ctx, true);
            }
            SettingsLayerEvent::EditProfile => {
                self.settings_layer_menu_open = false;
                if let Some(ctrl) = self.settings.as_mut() {
                    ctrl.push_section(SettingsSection::Information);
                }
            }
            SettingsLayerEvent::Logout => {
                self.settings_layer_menu_open = false;
                actions.push(MainWidgetAction::RequestLogout);
            }
        }
    }

    /// Paints the 56 px layer header (`st::infoLayerTopBar`) and returns the clicked control.
    fn settings_layer_header(
        &mut self,
        ui: &mut Ui,
        header: Rect,
        radius: u8,
        palette: &Palette,
    ) -> Option<SettingsLayerEvent> {
        let mut event = None;
        // Swallow clicks on the header background so the hidden settings top bar never reacts.
        let _ = ui.interact(
            header,
            ui.make_persistent_id("tg_settings_layer_header"),
            Sense::click(),
        );
        ui.painter().rect_filled(
            header,
            CornerRadius {
                nw: radius,
                ne: radius,
                sw: 0,
                se: 0,
            },
            palette.window_bg,
        );
        let (depth, title) = self.settings.as_ref().map_or_else(
            || (1, tg_lang::tr::lng_menu_settings()),
            |ctrl| {
                let depth = ctrl.section_stack.len().max(1);
                let title = if depth > 1 {
                    ctrl.current_section().localized_title()
                } else {
                    tg_lang::tr::lng_menu_settings()
                };
                (depth, title)
            },
        );
        let center_y = header.top() + 28.0;
        let mut title_left = header.left() + 24.0;
        if depth > 1 {
            if layer_icon_button(
                ui,
                Pos2::new(header.left() + 28.0, center_y),
                st::info_layer_top_bar_back.icon,
                &tg_lang::tr::lng_go_back(),
                palette,
            ) {
                event = Some(SettingsLayerEvent::Back);
            }
            title_left = header.left() + 56.0;
        }

        // The search field itself is the Settings body's (`settings_search.cpp` pins it under
        // the top bar); the header only opens it and its × closes it first.
        let searching = depth == 1
            && (settings_search_open(ui.ctx())
                || self
                    .settings
                    .as_ref()
                    .is_some_and(|ctrl| !ctrl.search_query.is_empty()));
        let galley = ui.painter().layout(
            title.clone(),
            font_id_from_style(st::box_title_font),
            palette.window_bold_fg,
            (header.right() - 140.0 - title_left).max(40.0),
        );
        let title_pos = Pos2::new(title_left, header.top() + 17.0);
        // The title is a `FlatLabel` in `Info::TopBar`, so screen readers see it.
        ui.interact(
            Rect::from_min_size(title_pos, galley.size()),
            ui.make_persistent_id("tg_settings_layer_title"),
            Sense::hover(),
        )
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &title));
        ui.painter()
            .galley(title_pos, galley, palette.window_bold_fg);

        let close_c = Pos2::new(header.right() - 27.0, center_y);
        if depth == 1 {
            if layer_icon_button(
                ui,
                Pos2::new(header.right() - 112.0, center_y),
                st::info_top_bar_search.icon,
                &tg_lang::tr::lng_dlg_filter(),
                palette,
            ) {
                event = Some(SettingsLayerEvent::Search);
            }
            if layer_icon_button(
                ui,
                Pos2::new(header.right() - 67.0, center_y),
                st::info_layer_top_bar_menu.icon,
                "More",
                palette,
            ) {
                event = Some(SettingsLayerEvent::ToggleMenu);
            }
        }
        if layer_icon_button(
            ui,
            close_c,
            st::info_layer_top_bar_close.icon,
            &tg_lang::tr::lng_close(),
            palette,
        ) {
            if searching {
                if let Some(ctrl) = self.settings.as_mut() {
                    ctrl.search_query.clear();
                }
                set_settings_search_open(ui.ctx(), false);
            } else {
                event = Some(SettingsLayerEvent::Close);
            }
        }
        event
    }

    /// `Settings::Main::fillTopBarMenu`: "Edit profile" and the attention "Log out" item.
    fn settings_layer_menu(
        &mut self,
        ctx: &egui::Context,
        layer: Rect,
        palette: &Palette,
        event: &mut Option<SettingsLayerEvent>,
    ) {
        let pos = Pos2::new(layer.right() - 8.0 - 200.0, layer.top() + 44.0);
        let response = Area::new(Id::unique("tg_settings_layer_menu"))
            .order(Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                Frame::NONE
                    .fill(palette.window_bg)
                    .corner_radius(CornerRadius::same(8))
                    .shadow(egui::Shadow {
                        offset: [0, 2],
                        blur: 10,
                        spread: 0,
                        color: Color32::from_black_alpha(48),
                    })
                    .inner_margin(egui::Margin::symmetric(0, 6))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let items = [
                            (
                                st::menu_icon_edit,
                                tg_lang::tr::lng_settings_information(),
                                palette.window_bold_fg,
                                SettingsLayerEvent::EditProfile,
                            ),
                            (
                                st::menu_icon_leave_attention,
                                tg_lang::tr::lng_settings_logout(),
                                palette.attention_fg,
                                SettingsLayerEvent::Logout,
                            ),
                        ];
                        for (icon, label, color, item_event) in items {
                            let (rect, resp) =
                                ui.allocate_exact_size(Vec2::new(200.0, 36.0), Sense::click());
                            resp.widget_info(|| {
                                egui::WidgetInfo::labeled(egui::Role::MenuItem, true, &label)
                            });
                            if resp.hovered() {
                                ui.painter().rect_filled(rect, 0.0, palette.window_bg_over);
                            }
                            let icon_color = if item_event == SettingsLayerEvent::Logout {
                                palette.attention_fg
                            } else {
                                palette.window_sub_text_fg
                            };
                            paint_style_icon(
                                ui.painter(),
                                Rect::from_center_size(
                                    Pos2::new(rect.left() + 26.0, rect.center().y),
                                    Vec2::splat(24.0),
                                ),
                                icon,
                                &DefaultPaletteResolver,
                                Some(icon_color),
                            );
                            ui.painter().text(
                                Pos2::new(rect.left() + 56.0, rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                &label,
                                font_id_from_style(st::normal_font),
                                color,
                            );
                            if resp.clicked() {
                                *event = Some(item_event);
                            }
                        }
                    });
            })
            .response;
        if event.is_none()
            && ctx.input(|i| i.pointer.any_click())
            && ctx
                .input(|i| i.pointer.interact_pos())
                .is_some_and(|p| !response.rect.contains(p))
        {
            self.settings_layer_menu_open = false;
        }
    }

    fn render_third_column(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        session: &mut SessionController,
        session_backend: &mut dyn AppSession,
        theme_colors: &dyn Colors,
        actions: &mut Vec<MainWidgetAction>,
    ) {
        let Some(SectionMemento::PeerInfo { peer_id, .. }) = session.third_section().cloned()
        else {
            return;
        };
        self.third_info.set_peer(peer_id);
        let mut events = Vec::new();
        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
            events = self
                .third_info
                .show(ui, session_backend, true, theme_colors);
        });
        for ev in events {
            match ev {
                InfoWrapEvent::CloseOrBack => session.close_third_section(),
                InfoWrapEvent::OpenHistory(pid) => {
                    session.show_peer_history(pid, SectionShow::forward(), MsgId::SHOW_AT_UNREAD);
                }
                other => actions.push(MainWidgetAction::InfoEvent(other)),
            }
        }
    }

    fn handle_first_splitter(
        &mut self,
        ui: &mut Ui,
        body_rect: Rect,
        splitter_x: f32,
        session: &mut SessionController,
        actions: &mut Vec<MainWidgetAction>,
    ) {
        let half = HISTORY_RESIZE_WIDTH as f32 * 0.5;
        let hit_rect = Rect::from_min_max(
            Pos2::new(splitter_x - half, body_rect.top()),
            Pos2::new(splitter_x + half, body_rect.bottom()),
        );
        let resp = ui.interact(hit_rect, Id::unique("tg_first_col_splitter"), Sense::drag());
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
        if resp.dragged() {
            self.dragging_first_splitter = true;
            if let Some(pointer) = resp.interact_pointer_pos() {
                let new_w = (pointer.x - body_rect.left()).round() as i32;
                let body_w = body_rect.width().round() as i32;
                let ratio = dialogs_drag_to_ratio(new_w, body_w);
                let nochat = !session.is_main_section_shown();
                session
                    .columns_mut()
                    .update_dialogs_width_ratio(ratio, nochat);
            }
        } else if self.dragging_first_splitter {
            self.dragging_first_splitter = false;
            actions.push(MainWidgetAction::PersistSettings);
        }
    }

    fn handle_third_splitter(
        &mut self,
        ui: &mut Ui,
        body_rect: Rect,
        splitter_x: f32,
        session: &mut SessionController,
        actions: &mut Vec<MainWidgetAction>,
    ) {
        let half = HISTORY_RESIZE_WIDTH as f32 * 0.5;
        let hit_rect = Rect::from_min_max(
            Pos2::new(splitter_x - half, body_rect.top()),
            Pos2::new(splitter_x + half, body_rect.bottom()),
        );
        let resp = ui.interact(hit_rect, Id::unique("tg_third_col_splitter"), Sense::drag());
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
        if resp.dragged() {
            self.dragging_third_splitter = true;
            if let Some(pointer) = resp.interact_pointer_pos() {
                let new_w = (body_rect.right() - pointer.x).round() as i32;
                session.columns_mut().set_third_column_width(new_w);
            }
        } else if self.dragging_third_splitter {
            self.dragging_third_splitter = false;
            session.columns_mut().finish_third_column_resize();
            actions.push(MainWidgetAction::PersistSettings);
        }
    }
}

/// Header controls of the Settings layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsLayerEvent {
    Close,
    Back,
    Search,
    ToggleMenu,
    EditProfile,
    Logout,
}

/// Whether `palette` is a dark (Night) palette, judged by `windowBg` luminance.
fn is_dark_palette(palette: &Palette) -> bool {
    let bg = palette.window_bg;
    u32::from(bg.r()) + u32::from(bg.g()) + u32::from(bg.b()) < 3 * 128
}

/// A 40 px `IconButton` from the info layer top bar tinted `boxTitleCloseFg`
/// (`boxTitleCloseFgOver` on hover).
fn layer_icon_button(
    ui: &mut Ui,
    center: Pos2,
    icon: Icon,
    label: &str,
    palette: &Palette,
) -> bool {
    let rect = Rect::from_center_size(center, Vec2::splat(40.0));
    let resp = ui.interact(
        rect,
        ui.make_persistent_id(("tg_layer_icon", label)),
        Sense::click(),
    );
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    let color = if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        palette.window_fg.gamma_multiply(0.55)
    } else {
        palette.window_sub_text_fg
    };
    paint_style_icon(
        ui.painter(),
        Rect::from_center_size(center, Vec2::splat(24.0)),
        icon,
        &DefaultPaletteResolver,
        Some(color),
    );
    resp.clicked()
}

/// Builds the action that opens the Stars Affiliate join list for the bot shown in
/// [`BotEarnView`] (`showSection(BotStarRef::Join::Make(peer()))` in `info_bot_earn_list.cpp`).
///
/// The bot is identified by its username, falling back to its title.
#[must_use]
pub fn bot_earn_affiliate_action(
    bot: Option<&tg_app_core::stand_in::types::PeerSummary>,
) -> MainWidgetAction {
    let slug = bot
        .and_then(|p| p.username.clone().or_else(|| Some(p.title.clone())))
        .filter(|s| !s.trim().is_empty());
    MainWidgetAction::OpenPayments {
        screen: tg_app_info::payments::PaymentsScreen::Affiliate,
        slug,
    }
}

/// The chat kind for the send-failure texts (`ApiWrap::sendMessageFail`).
fn send_target(peer_id: PeerId) -> tg_app_core::user_error::SendTarget {
    use tg_app_core::stand_in::types::PeerKind;
    use tg_app_core::user_error::SendTarget;
    match peer_id.kind() {
        PeerKind::User => SendTarget::User,
        PeerKind::Group => SendTarget::Group,
        PeerKind::Channel => SendTarget::Channel,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock_session::MockSession;
    use tg_app_history::history::compose::SendShortcut;
    use tg_app_media::media::inline::InlinePlaybackKind;
    use tg_app_settings::settings::SettingsSection;
    use tg_audio::{
        AudioType, DocumentId, FullMsgId, MediaKind, MsgId as AudioMsgIdNum, PeerId as AudioPeerId,
    };
    use tg_core::settings::InputSubmitSettings;

    #[test]
    fn main_widget_propagates_live_core_settings_to_history() {
        let mock = MockSession::new(true);
        let mut mw = MainWidget::new();
        mw.open_settings_section(
            SettingsSection::Advanced,
            Box::new(mock.handle()),
            EmbeddedType::DayBlue,
            None,
        );

        // Mutate live settings in SettingsController (power saving + autoplay + submit + volume).
        {
            let ctrl = mw.settings_mut().expect("settings initialized");
            ctrl.advanced.power_saving.stickers_chat = true;
            ctrl.advanced.power_saving.chat_spoiler = true;
            let mut dl = ctrl.backend.auto_download();
            dl.autoplay_gifs = false;
            dl.autoplay_round_videos = false;
            ctrl.backend.set_auto_download(dl);
            ctrl.core_settings.send_submit_way = InputSubmitSettings::CtrlEnter;
            ctrl.core_settings.song_volume = 0.42;
            ctrl.core_settings.loop_animated_stickers = false;
        }

        mw.sync_live_core_settings();

        let policy = &mw.history_mut().media_runtime().policy;
        assert!(!policy.allows_inline(InlinePlaybackKind::Gif));
        assert!(!policy.allows_inline(InlinePlaybackKind::RoundVideo));
        assert!(!policy.allows_inline(InlinePlaybackKind::VideoSticker));
        assert!(!policy.allows_lottie_loop());
        assert!(!policy.allows_spoiler_particles());
        assert_eq!(mw.history_mut().send_shortcut(), SendShortcut::CtrlEnter);
        assert!((mw.audio_player_bar().volume() - 0.42).abs() < 1e-6);

        // Re-enabling settings propagates immediately on next sync.
        {
            let ctrl = mw.settings_mut().expect("settings initialized");
            ctrl.advanced.power_saving.stickers_chat = false;
            ctrl.advanced.power_saving.chat_spoiler = false;
            let mut dl = ctrl.backend.auto_download();
            dl.autoplay_gifs = true;
            dl.autoplay_round_videos = true;
            ctrl.backend.set_auto_download(dl);
            ctrl.core_settings.loop_animated_stickers = true;
            ctrl.core_settings.send_submit_way = InputSubmitSettings::Enter;
        }
        mw.sync_live_core_settings();

        let policy2 = &mw.history_mut().media_runtime().policy;
        assert!(policy2.allows_inline(InlinePlaybackKind::Gif));
        assert!(policy2.allows_inline(InlinePlaybackKind::RoundVideo));
        assert!(policy2.allows_inline(InlinePlaybackKind::VideoSticker));
        assert!(policy2.allows_lottie_loop());
        assert!(policy2.allows_spoiler_particles());
        assert_eq!(mw.history_mut().send_shortcut(), SendShortcut::Enter);
    }

    #[test]
    fn main_widget_uses_one_shared_audio_player_bar_and_stays_headless_in_tests() {
        let mock = MockSession::new(true);
        let mut mw = MainWidget::new();

        // Tests default to headless (software mixer only, no cpal stream).
        assert!(!mw.hardware_audio_enabled());
        assert!(!mw.history_mut().media_runtime().hardware_audio_enabled());
        assert!(!mw.audio_player_bar().is_visible());

        // 1. Playing a voice message from History drives the shared AudioPlayerBar.
        let peer = PeerId::from_chat(2001);
        mw.history_mut().media_runtime_mut().toggle_or_seek_voice(
            peer,
            MsgId(108),
            "Taylor Brooks",
            6,
            None,
        );
        assert!(mw.audio_player_bar().is_visible());
        assert_eq!(mw.audio_player_bar().active_type(), AudioType::Voice);

        // 2. Playing a track from the shared MediaBackend (MockSession) drives the EXACT SAME bar.
        let mock_audio_id = AudioMsgId::new(
            DocumentId(9100),
            FullMsgId::new(AudioPeerId(2001), AudioMsgIdNum(9100)),
            MediaKind::AudioFile,
            5_000,
            0,
        );
        // Register a track into MockSession's audio source via history play_external_audio_track fallback
        // or play one of MockSession's built-in audio tracks.
        let builtin_track = mock
            .siblings_in_chat(2001, tg_app_media::media::backend::MediaItemId(1))
            .into_iter()
            .find_map(|item| {
                let id = AudioMsgId::new(
                    DocumentId(item.id.0),
                    FullMsgId::new(
                        AudioPeerId(item.peer_id),
                        AudioMsgIdNum(i64::from(item.msg_id)),
                    ),
                    MediaKind::AudioFile,
                    item.duration_ms.max(1000),
                    0,
                );
                mock.audio_track_metadata(id).map(|_| id)
            })
            .unwrap_or(mock_audio_id);
        let _ = mw.play_audio_track(&mock, builtin_track, false);
        assert!(mw.audio_player_bar().is_visible());
        // Still headless because cfg!(test) keeps hardware_audio_enabled == false.
        assert!(!mw.history_mut().media_runtime().hardware_audio_enabled());
    }
}
