//! Top-level window controller coordinating Intro, Main, Passcode Lock, Modal Boxes,
//! Media Viewer, ThemeManager (`tg-ui`), Localization (`tg-lang`), and Keyboard Shortcuts.
//!
//! Port origin: `Telegram/SourceFiles/window/window_controller.h`,
//! `Telegram/SourceFiles/window/window_controller.cpp`,
//! `Telegram/SourceFiles/window/window_session_controller.cpp`,
//! `Telegram/SourceFiles/window/main_window.cpp`,
//! and `Telegram/SourceFiles/mainwindow.cpp`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Ui, ViewportCommand};
use tg_theme::{Color as ThemeColor, EmbeddedType};
use tg_ui::theme::ThemeManager;

use crate::mock_session::{MockSession, SharedSessionHandle, session_peers};
use crate::window::chat_preview::WindowShellExtras;
use crate::window::connecting_widget::{ConnectionIndicator, ConnectionWidgetAction};
use crate::window::deep_links::{
    DeepLinkOutcome, DeepLinkOverlayRequest, dispatch_unauthenticated_deep_link, split_fragment,
};
use crate::window::layer_stack::{LayerAction, LayerStack, ModalBox};
use crate::window::lock_widgets::{PasscodeLockAction, PasscodeLockWidget};
use crate::window::main_menu::{MainMenu, MainMenuAccountItem, MainMenuAction, main_menu_bots};
use crate::window::main_widget::{MainWidget, MainWidgetAction};
use crate::window::section::{SectionShow, SettingsPage};
use crate::window::security_policy::{
    ExternalLinkAction, build_hidden_link_confirm_box, build_launch_file_confirm_box,
    classify_launch_warning, evaluate_external_link,
};
use crate::window::session_controller::SessionController;
use tg_app_core::session_api::AppSession;
use tg_app_core::stand_in::backend::Backend;
use tg_app_core::stand_in::settings::{ShellSettings, WindowPosition};
use tg_app_core::stand_in::strings::{Strings, TgLangStrings};
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{AuthUser, FilterId, MsgId, PeerId};
use tg_app_history::history::backend::HistoryMedia;
use tg_app_info::boxes::about::AboutBoxState;
use tg_app_info::boxes::add_contact::AddContactBoxState;
use tg_app_info::boxes::confirm::ConfirmTag;
use tg_app_info::boxes::create_peer::{CreateChannelBoxState, CreateGroupBoxState};
use tg_app_info::boxes::delete_messages::DeleteMessagesBoxState;
use tg_app_info::boxes::edit_peer::{
    AdminsListBoxState, AutoDeleteTimerBoxState, EditAdminRightsBoxState, EditPeerInfoBoxState,
    EditPermissionsBoxState,
};
use tg_app_info::boxes::language::LanguageBoxState;
use tg_app_info::boxes::mini_app_confirm::MiniAppOpenConfirmBoxState;
use tg_app_info::boxes::peer_color::EditPeerColorBoxState;
use tg_app_info::boxes::peer_list::{PeerListBoxState, PeerListPurpose, PeerSelectionMode};
use tg_app_info::boxes::photo_crop::PhotoCropBoxState;
use tg_app_info::boxes::report::ReportBoxState;
use tg_app_info::boxes::share::ShareBoxState;
use tg_app_info::boxes::terms::TermsOfServiceBoxState;
use tg_app_info::boxes::username::UsernameBoxState;
use tg_app_info::boxes::{BoxEvent, BoxHost, BoxKind};
use tg_app_info::dialogs::DialogsShortcut;
use tg_app_info::info::{InfoBackend, InfoPeerKind, InfoWrapEvent, SharedMediaTab};
use tg_app_info::statistics::StatisticsScreen;
use tg_app_media::media::{
    MediaOverlayWidget, OverlayEvent, PipEvent, PipWindow, RoundFloatEvent, RoundVideoFloat,
};
use tg_app_media::stories::AppStoriesHost;
use tg_app_media::webview::AppWebViewHost;
use tg_app_media::webview::backend::{AppWebViewEvent, WebViewBackend, WebViewRequest};
use tg_app_media::webview::theme::build_webview_theme_params;
use tg_app_settings::intro::{IntroAction, IntroWidget};
use tg_app_settings::settings::SettingsSection;

/// Debounce timeout for saving window position (`kSaveWindowPositionTimeout = 1000ms` in `main_window.cpp`).
const SAVE_WINDOW_POSITION_TIMEOUT: Duration = Duration::from_millis(1000);

/// Active root screen hosted by the window (`MainWindow::_intro`, `_main`, `_passcodeLock`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootScreen {
    /// Login / intro flow.
    Intro,
    /// Authenticated multi-column workspace.
    Main,
    /// Local passcode lock screen overlaying the session.
    PasscodeLock,
}

/// Outbound webview/browser request queued by [`WindowController`] for [`tg_app_media::webview::AppWebViewHost`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebViewHostRequest {
    /// Open an Instant View article (falling back to external browser if unsupported).
    OpenInstantView {
        /// Target article URL.
        url: String,
        /// Optional known webpage id.
        webpage_id: Option<i64>,
        /// Optional anchor fragment.
        fragment: Option<String>,
    },
    /// Launch a Bot Mini App window.
    OpenMiniApp {
        /// Bot bare user ID.
        bot_id: i64,
        /// Optional deep-link start parameter.
        start_param: Option<String>,
    },
    /// Open an external URL in the user's system browser.
    OpenExternalUrl(String),
}

/// Top-level window coordinator (`Window::Controller` + `MainWindow`).
///
/// Generic over any [`AppSession`] implementation (defaults to [`MockSession`] for demo mode
/// and existing UI tests; instantiated with [`crate::live::LiveSession`] in live mode).
pub struct WindowController {
    session_backend: Box<dyn AppSession>,
    theme_manager: ThemeManager,
    strings: TgLangStrings,
    settings: ShellSettings,
    settings_path: PathBuf,
    settings_dirty_since: Option<Instant>,
    started_at: Instant,
    root_screen: RootScreen,
    intro: IntroWidget,
    session: Option<SessionController>,
    main_widget: MainWidget,
    passcode_lock: PasscodeLockWidget,
    /// Whether the lazily created Settings panel has received `ShellSettings::native_window_frame`.
    native_frame_seeded: bool,
    layer_stack: LayerStack,
    box_host: BoxHost,
    main_menu: MainMenu,
    connection_indicator: ConnectionIndicator,
    media_overlay: MediaOverlayWidget,
    media_fullscreen: crate::window::media_fullscreen::MediaViewerFullscreen,
    pip_window: Option<PipWindow>,
    round_float: RoundVideoFloat,
    pending_webview_requests: Vec<WebViewHostRequest>,
    pending_overlay_requests: Vec<DeepLinkOverlayRequest>,
    pending_login_code: Option<String>,
    pending_open_recent_calls: bool,
    pending_start_call: Option<(PeerId, bool)>,
    logout_requested: bool,
    menu_accounts: Vec<MainMenuAccountItem>,
    max_accounts: usize,
    pending_account_switch: Option<i32>,
    pending_add_account: bool,
    update_snapshot: tg_app_core::core_types::startup_types::UpdateSnapshot,
    pending_update_action: Option<tg_app_core::core_types::startup_types::UpdateUiAction>,
    pending_update_prefs: Option<(bool, bool)>,
    external_passcode_verifier:
        Option<tg_app_settings::settings::local_passcode::PasscodeVerifierFn>,
    shell_extras: WindowShellExtras,
    native_hwnd: isize,
    fallback_shortcuts: tg_app_settings::settings::shortcuts::ShortcutManager,
    no_warning_extensions: BTreeSet<String>,
    ip_reveal_warning: bool,
    /// Activations (`tg://` links, `-sendpath`, `-tosettings`) received while the passcode lock
    /// is shown; replayed after unlock (`Application::checkStartUrls` waits for unlock).
    queued_activations: Vec<tg_app_core::core_types::startup_types::IncomingActivation>,
    /// `-tosettings` requested while locked.
    queued_open_settings: bool,
    /// Bots whose Mini App the user already confirmed (`isPeerTrustedOpenWebView`).
    trusted_open_webview: std::collections::HashSet<PeerId>,
}

impl WindowController {
    /// Compatibility constructor accepting a legacy `Backend` and `Strings` (used by shell unit tests).
    #[must_use]
    pub fn new(
        backend: Box<dyn Backend>,
        _strings: Box<dyn Strings>,
        settings: ShellSettings,
        settings_path: PathBuf,
    ) -> Self {
        let start_signed_in = backend.current_user().is_some();
        Self::with_session(MockSession::new(start_signed_in), settings, settings_path)
    }

    /// Returns a clonable handle to the shared [`MockSession`] state.
    #[must_use]
    pub fn session_handle(&self) -> SharedSessionHandle {
        self.mock_session().handle()
    }

    /// Returns a shared reference to the underlying [`MockSession`].
    ///
    /// # Panics
    /// Panics if this controller is not backed by [`MockSession`].
    #[must_use]
    pub fn mock_session(&self) -> &MockSession {
        self.session_backend_as::<MockSession>()
            .expect("WindowController is not backed by MockSession")
    }

    /// Returns a mutable reference to the underlying [`MockSession`].
    ///
    /// # Panics
    /// Panics if this controller is not backed by [`MockSession`].
    pub fn mock_session_mut(&mut self) -> &mut MockSession {
        self.session_backend_as_mut::<MockSession>()
            .expect("WindowController is not backed by MockSession")
    }

    /// Constructs a `WindowController` backed by any [`AppSession`], `tg-ui`'s
    /// [`ThemeManager`], and `tg-lang`'s [`TgLangStrings`].
    #[must_use]
    pub fn with_session<S: AppSession + 'static>(
        session_backend: S,
        settings: ShellSettings,
        settings_path: PathBuf,
    ) -> Self {
        Self::with_boxed_session(Box::new(session_backend), settings, settings_path)
    }

    /// Constructs a `WindowController` from an already-boxed [`AppSession`].
    #[must_use]
    pub fn with_boxed_session(
        session_backend: Box<dyn AppSession>,
        settings: ShellSettings,
        settings_path: PathBuf,
    ) -> Self {
        let embedded = settings.resolved_embedded_type();
        let accent = settings.resolved_accent_color();
        let mut theme_manager = ThemeManager::with_embedded(embedded);
        if let Some(c) = accent {
            theme_manager.set_embedded(embedded, Some(c));
        }

        let is_live = session_backend.is_live_session();
        let already_signed_in = session_backend.current_user().is_some() || is_live;
        let mut layer_stack = LayerStack::new();
        layer_stack.set_live_mode(is_live);
        let mut main_menu = MainMenu::default();
        main_menu.set_live_mode(is_live);
        let mut passcode_lock = PasscodeLockWidget::new();
        passcode_lock.set_windows_hello_verifier(Some(std::sync::Arc::new(
            tg_platform::windows_hello::WinRtWindowsHelloVerifier::new(),
        )));
        let mut controller = Self {
            session_backend,
            theme_manager,
            strings: TgLangStrings::new(),
            settings,
            settings_path,
            settings_dirty_since: None,
            started_at: tg_app_core::clock::now(),
            root_screen: RootScreen::Intro,
            intro: IntroWidget::new(),
            session: None,
            main_widget: MainWidget::new(),
            passcode_lock,
            native_frame_seeded: false,
            layer_stack,
            box_host: BoxHost::new(),
            main_menu,
            connection_indicator: ConnectionIndicator::new(tg_app_core::clock::now()),
            media_overlay: MediaOverlayWidget::new(),
            media_fullscreen: crate::window::media_fullscreen::MediaViewerFullscreen::default(),
            pip_window: None,
            round_float: RoundVideoFloat::new(1280, 800),
            pending_webview_requests: Vec::new(),
            pending_overlay_requests: Vec::new(),
            pending_login_code: None,
            pending_open_recent_calls: false,
            pending_start_call: None,
            logout_requested: false,
            menu_accounts: Vec::new(),
            max_accounts: tg_session::MAX_ACCOUNTS,
            pending_account_switch: None,
            pending_add_account: false,
            update_snapshot: tg_app_core::core_types::startup_types::UpdateSnapshot::default(),
            pending_update_action: None,
            pending_update_prefs: None,
            external_passcode_verifier: None,
            shell_extras: WindowShellExtras::new(),
            native_hwnd: 0,
            fallback_shortcuts: tg_app_settings::settings::shortcuts::ShortcutManager::new(),
            no_warning_extensions: BTreeSet::new(),
            ip_reveal_warning: true,
            queued_activations: Vec::new(),
            queued_open_settings: false,
            trusted_open_webview: std::collections::HashSet::new(),
        };

        if already_signed_in {
            controller.setup_main();
        }
        controller
    }

    /// Returns a shared reference to the underlying [`AppSession`].
    #[must_use]
    pub fn session_backend(&self) -> &dyn AppSession {
        &*self.session_backend
    }

    /// Returns a mutable reference to the underlying [`AppSession`].
    pub fn session_backend_mut(&mut self) -> &mut dyn AppSession {
        &mut *self.session_backend
    }

    /// Downcasts the underlying [`AppSession`] to `&T`, if it matches.
    #[must_use]
    pub fn session_backend_as<T: 'static>(&self) -> Option<&T> {
        self.session_backend.as_any().downcast_ref::<T>()
    }

    /// Downcasts the underlying [`AppSession`] to `&mut T`, if it matches.
    pub fn session_backend_as_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.session_backend.as_any_mut().downcast_mut::<T>()
    }

    /// Returns `true` once when the user triggered the Recent Calls entry point (e.g. from Main Menu).
    pub fn take_open_recent_calls_request(&mut self) -> bool {
        std::mem::take(&mut self.pending_open_recent_calls)
    }

    /// Returns the outgoing 1:1 call `(peer, video)` requested from a profile Call button, once.
    pub fn take_start_call_request(&mut self) -> Option<(PeerId, bool)> {
        self.pending_start_call.take()
    }

    /// Returns `true` once when the user confirmed logout on a live session.
    pub fn take_logout_requested(&mut self) -> bool {
        std::mem::take(&mut self.logout_requested)
    }

    /// Updates the multi-account list and dynamic account limit rendered inside [`MainMenu`].
    pub fn set_menu_accounts(&mut self, accounts: Vec<MainMenuAccountItem>, max_accounts: usize) {
        self.menu_accounts = accounts;
        self.max_accounts = max_accounts.max(tg_session::MAX_ACCOUNTS);
    }

    /// Returns the requested target account slot once when the user clicks another account in
    /// [`MainMenu`].
    pub fn take_switch_account_request(&mut self) -> Option<i32> {
        self.pending_account_switch.take()
    }

    /// Returns `true` once when the user clicks `"Add Account"` in [`MainMenu`] while below the
    /// active account limit.
    pub fn take_add_account_request(&mut self) -> bool {
        std::mem::take(&mut self.pending_add_account)
    }

    /// Opens the main menu drawer programmatically (useful for tests & keyboard actions).
    pub fn open_main_menu(&mut self) {
        self.layer_stack.open_main_menu();
    }

    /// Returns `true` when the main menu drawer is currently open.
    #[must_use]
    pub fn is_main_menu_open(&self) -> bool {
        self.layer_stack.is_main_menu_open()
    }

    /// Mutably borrows the [`MainMenu`] drawer state.
    pub fn main_menu_mut(&mut self) -> &mut MainMenu {
        &mut self.main_menu
    }

    /// Connects profile data directories and the HTML export folder dialog into the workspace.
    pub fn set_profile_media_dirs(
        &mut self,
        video_drafts_dir: Option<std::path::PathBuf>,
        rich_export_dir: Option<std::path::PathBuf>,
        rich_export_dialog: Option<
            std::sync::Arc<dyn tg_app_media::webview::rich_editor::ExportFolderDialog>,
        >,
    ) {
        self.main_widget.set_profile_media_dirs(
            video_drafts_dir,
            rich_export_dir,
            rich_export_dialog,
        );
    }

    /// Returns `(include_muted_counter, count_unread_messages)` from the active
    /// [`SettingsController`], if Settings has been opened in this workspace.
    pub fn notification_badge_prefs(&mut self) -> Option<(bool, bool)> {
        self.main_widget.settings_mut().map(|ctrl| {
            (
                ctrl.core_settings.include_muted_counter,
                ctrl.core_settings.count_unread_messages,
            )
        })
    }

    /// Returns the active root screen.
    #[must_use]
    pub const fn root_screen(&self) -> RootScreen {
        self.root_screen
    }

    /// Updates the native window handle (`HWND` on Windows, `0` otherwise) used for
    /// OS-level screenshot protection synchronization on [`MediaOverlayWidget`].
    pub fn set_native_hwnd(&mut self, hwnd: isize) {
        self.native_hwnd = hwnd;
        self.media_overlay.sync_screenshot_protection(hwnd);
    }

    /// Synchronizes screenshot protection (`WDA_EXCLUDEFROMCAPTURE`) for `hwnd` against the
    /// current [`MediaOverlayWidget`] state.
    pub fn sync_screenshot_protection(&mut self, hwnd: isize) {
        self.native_hwnd = hwnd;
        self.media_overlay.sync_screenshot_protection(hwnd);
    }

    /// Opens the Downloads Manager (`InfoSection::Downloads`) on the main section stack.
    pub fn open_downloads(&mut self) {
        let peer_id = self
            .session
            .as_ref()
            .map(SessionController::active_peer)
            .filter(|p| !p.is_zero())
            .or_else(|| self.session_backend.current_user().map(|u| u.id))
            .unwrap_or(PeerId::ZERO);
        if let Some(session) = self.session.as_mut() {
            session.show_downloads(peer_id);
        }
    }

    /// Drains queued [`DeepLinkOverlayRequest`]s into the global demo overlay hosts
    /// (`AppPaymentsHost`, `AppBusinessHost`).
    pub fn dispatch_demo_overlay_requests(&mut self, ctx: &egui::Context) {
        let requests = self.take_deep_link_overlay_requests();
        for req in requests {
            match req {
                DeepLinkOverlayRequest::OpenPayments { screen, slug } => {
                    if let Err(err) =
                        tg_app_info::payments::AppPaymentsHost::dispatch_overlay_request(
                            ctx,
                            screen,
                            slug.as_deref(),
                        )
                    {
                        self.layer_stack.show_toast(err);
                    }
                }
                DeepLinkOverlayRequest::OpenBusiness => {
                    tg_app_history::business::AppBusinessHost::open_overlay(ctx);
                }
                DeepLinkOverlayRequest::OpenStory { .. } => {
                    self.layer_stack
                        .show_toast("Story viewer opened from link.");
                }
                DeepLinkOverlayRequest::JoinCall { .. } => {
                    self.pending_open_recent_calls = true;
                }
                req @ (DeepLinkOverlayRequest::OpenTheme { .. }
                | DeepLinkOverlayRequest::OpenBackground { .. }
                | DeepLinkOverlayRequest::OpenShareTheme { .. }) => {
                    crate::window::deep_links::dispatch_themes_deep_link_overlay(ctx, &req);
                }
                DeepLinkOverlayRequest::OpenWallet { action } => {
                    if let Err(err) =
                        tg_app_settings::wallet::AppWalletHost::handle_deep_link_action(
                            ctx, &action,
                        )
                    {
                        self.layer_stack.show_toast(err);
                    }
                }
            }
        }
    }

    /// Attaches `tdata_dir` for `shortcuts-custom.json` loading & persistence on both this
    /// controller's fallback [`ShortcutManager`] and [`SettingsController::shortcuts`].
    pub fn attach_shortcuts_tdata_dir(&mut self, tdata_dir: &std::path::Path) {
        let _ = self.fallback_shortcuts.load_from_tdata_dir(tdata_dir);
        self.main_widget.attach_shortcuts_tdata_dir(tdata_dir);
    }

    /// Returns the active [`tg_app_settings::settings::shortcuts::ShortcutManager`] used by global shortcut
    /// dispatch (`SettingsController.shortcuts.manager` if Settings is initialized, otherwise
    /// the window's loaded `tdata` shortcut manager).
    #[must_use]
    pub fn active_shortcut_manager(
        &self,
    ) -> &tg_app_settings::settings::shortcuts::ShortcutManager {
        self.main_widget
            .settings()
            .map_or(&self.fallback_shortcuts, |s| &s.shortcuts.manager)
    }

    /// Mutably borrows the active [`tg_app_settings::settings::shortcuts::ShortcutManager`].
    pub fn active_shortcut_manager_mut(
        &mut self,
    ) -> &mut tg_app_settings::settings::shortcuts::ShortcutManager {
        match self.main_widget.settings_mut() {
            Some(settings) => &mut settings.shortcuts.manager,
            None => &mut self.fallback_shortcuts,
        }
    }

    /// Returns a reference to the live `tg-ui` [`ThemeManager`].
    #[must_use]
    pub const fn theme_manager(&self) -> &ThemeManager {
        &self.theme_manager
    }

    /// Returns a mutable reference to the live `tg-ui` [`ThemeManager`].
    pub fn theme_manager_mut(&mut self) -> &mut ThemeManager {
        &mut self.theme_manager
    }

    /// Returns the monotonically increasing theme generation counter from [`ThemeManager`].
    #[must_use]
    pub const fn theme_generation(&self) -> u64 {
        self.theme_manager.generation()
    }

    /// Returns the active color palette derived from `tg-ui`'s live [`ThemeManager`].
    #[must_use]
    pub fn palette(&self) -> Palette {
        Palette::from_theme_manager(&self.theme_manager)
    }

    /// Returns the active Day/Night theme mode.
    #[must_use]
    pub const fn theme_mode(&self) -> ThemeMode {
        self.settings.theme_mode
    }

    /// Returns a reference to persisted shell settings.
    #[must_use]
    pub const fn settings(&self) -> &ShellSettings {
        &self.settings
    }

    /// Returns a reference to the active session controller, if signed in.
    #[must_use]
    pub fn session(&self) -> Option<&SessionController> {
        self.session.as_ref()
    }

    /// Returns a mutable reference to the active session controller, if signed in.
    pub fn session_mut(&mut self) -> Option<&mut SessionController> {
        self.session.as_mut()
    }

    /// Returns a shared reference to [`MainWidget`].
    #[must_use]
    pub const fn main_widget(&self) -> &MainWidget {
        &self.main_widget
    }

    /// Returns a mutable reference to [`MainWidget`] (for tests & screenshot harnesses).
    pub fn main_widget_mut(&mut self) -> &mut MainWidget {
        &mut self.main_widget
    }

    /// Forwards the startup-resolved `rustgram-widget-helper.exe` path into [`MainWidget`] and its
    /// [`HistoryWidget`] for [`tg_app_media::webview::widget_host::LiveWidgetHost`] launches.
    pub fn set_widget_helper_path(&mut self, path: PathBuf) {
        self.main_widget.set_widget_helper_path(path);
    }

    /// Returns the startup-resolved `rustgram-widget-helper.exe` path configured on [`MainWidget`].
    #[must_use]
    pub fn widget_helper_path(&self) -> Option<&std::path::Path> {
        self.main_widget.widget_helper_path()
    }

    /// Returns a read-only reference to [`BoxHost`] (active modal dialog stack).
    #[must_use]
    pub const fn box_host(&self) -> &BoxHost {
        &self.box_host
    }

    /// Returns a mutable reference to [`BoxHost`] (active modal dialog stack).
    pub fn box_host_mut(&mut self) -> &mut BoxHost {
        &mut self.box_host
    }

    /// Returns a read-only reference to [`LayerStack`] (shell modals, main menu, toasts).
    #[must_use]
    pub const fn layer_stack(&self) -> &LayerStack {
        &self.layer_stack
    }

    /// Drains any pending auxiliary overlay requests (Payments, Passport, Business, Stories, Calls)
    /// queued by deep-link routing.
    pub fn take_deep_link_overlay_requests(&mut self) -> Vec<DeepLinkOverlayRequest> {
        let requests = std::mem::take(&mut self.pending_overlay_requests);
        if self.is_passcode_locked() {
            return Vec::new();
        }
        requests
    }

    /// Drains any pending `tg://login?code=<digits>` code captured from a deep link.
    pub fn take_pending_login_code(&mut self) -> Option<String> {
        self.pending_login_code.take()
    }

    /// Returns `true` if any top-level screen, section, modal, or panel transition animation is running.
    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.intro.is_animating()
            || self
                .session
                .as_ref()
                .is_some_and(SessionController::is_animating)
            || self.box_host.is_animating()
            || self.main_widget.is_animating()
    }

    /// Returns a reference to [`MediaOverlayWidget`].
    #[must_use]
    pub const fn media_overlay(&self) -> &MediaOverlayWidget {
        &self.media_overlay
    }

    /// Returns a mutable reference to [`MediaOverlayWidget`].
    pub fn media_overlay_mut(&mut self) -> &mut MediaOverlayWidget {
        &mut self.media_overlay
    }

    /// Returns the currently focused active chat peer, if on `RootScreen::Main` viewing history.
    #[must_use]
    pub fn active_focused_peer(&self) -> Option<PeerId> {
        if self.root_screen != RootScreen::Main {
            return None;
        }
        let session = self.session.as_ref()?;
        if session.main_section().is_some() || session.active_peer().is_zero() {
            return None;
        }
        Some(session.active_peer())
    }

    /// Drains any outbound [`WebViewHostRequest`]s queued during the last frame.
    pub fn take_webview_requests(&mut self) -> Vec<WebViewHostRequest> {
        let requests = std::mem::take(&mut self.pending_webview_requests);
        if self.is_passcode_locked() {
            return Vec::new();
        }
        requests
    }

    /// Drains queued [`WebViewHostRequest`]s and dispatches them into `host` with live theme
    /// parameters and any pending Mini App / Game URL override (`bot_attach_web_view.cpp` flow).
    pub fn dispatch_webview_requests_to(&mut self, host: &mut AppWebViewHost) {
        let requests = self.take_webview_requests();
        if requests.is_empty() {
            return;
        }
        let theme_params_json = build_webview_theme_params(self.theme_manager.palette()).json;
        for req in requests {
            match req {
                WebViewHostRequest::OpenInstantView {
                    url,
                    webpage_id,
                    fragment,
                } => {
                    host.open_instant_view(&url, webpage_id, fragment.as_deref());
                }
                WebViewHostRequest::OpenMiniApp {
                    bot_id,
                    start_param,
                } => {
                    let bot_u64 = bot_id.max(0) as u64;
                    let url_override = self
                        .session_backend
                        .take_pending_mini_app_url(PeerId::from_user(bot_u64));
                    host.open_mini_app(WebViewRequest {
                        bot_id: bot_u64,
                        start_param,
                        url_override,
                        fullscreen: false,
                        theme_params_json: theme_params_json.clone(),
                    });
                }
                WebViewHostRequest::OpenExternalUrl(url) => {
                    host.open_instant_view(&url, None, None);
                }
            }
        }
    }

    /// Read-only accessor for the current self-updater state (consumed by Main Menu and
    /// Settings > Advanced).
    #[must_use]
    pub const fn update_state(&self) -> &tg_app_core::core_types::startup_types::UpdateState {
        &self.update_snapshot.state
    }

    /// Read-only accessor for the full updater snapshot including version, beta channel,
    /// and status label (consumed by Main Menu and Settings > Advanced).
    #[must_use]
    pub const fn update_snapshot(&self) -> &tg_app_core::core_types::startup_types::UpdateSnapshot {
        &self.update_snapshot
    }

    /// Updates the cached updater snapshot synced from [`crate::startup::UpdateController`].
    pub fn set_update_snapshot(
        &mut self,
        snapshot: tg_app_core::core_types::startup_types::UpdateSnapshot,
    ) {
        if let Some(settings) = self.main_widget.settings_mut() {
            settings.set_update_snapshot(snapshot.clone());
        }
        self.update_snapshot = snapshot;
    }

    /// Requests an immediate background update check (e.g. from Settings > Advanced).
    pub fn request_update_check(&mut self) {
        self.pending_update_action =
            Some(tg_app_core::core_types::startup_types::UpdateUiAction::CheckNow);
    }

    /// Requests launching `rustgram-updater.exe` and restarting to install a ready update
    /// (e.g. from Main Menu or Settings > Advanced).
    pub fn request_restart_to_update(&mut self) {
        self.pending_update_action =
            Some(tg_app_core::core_types::startup_types::UpdateUiAction::RestartToUpdate);
    }

    /// Drains a pending UI-originated updater action, if any.
    pub fn take_update_action(
        &mut self,
    ) -> Option<tg_app_core::core_types::startup_types::UpdateUiAction> {
        self.pending_update_action.take()
    }

    /// Drains pending `(auto_update, install_beta)` preference changes emitted from Settings > Advanced.
    pub fn take_update_prefs_change(&mut self) -> Option<(bool, bool)> {
        self.pending_update_prefs.take()
    }

    /// Displays a transient toast notification on the shell layer stack.
    pub fn show_toast(&mut self, message: impl Into<String>) {
        self.layer_stack.show_toast(message);
    }

    /// Opens Settings directly (`-tosettings` CLI flag).
    pub fn open_settings(&mut self) {
        if self.is_passcode_locked() {
            self.queued_open_settings = true;
            return;
        }
        if let Some(session) = self.session.as_mut() {
            session.show_settings(SettingsPage::Main);
        } else {
            self.layer_stack.show_box(ModalBox::ProxySettings);
        }
    }

    /// Processes a single-instance IPC activation (initial CLI args or named-pipe message from a
    /// secondary launch): opens `tg://` / `tonsite://` links and opens ShareBox for `-sendpath`.
    ///
    /// While the passcode lock is shown nothing is opened: the activation is queued and replayed
    /// after unlock (`Application::openLocalUrl` / `checkStartUrls` return early when
    /// `passcodeLocked()`). External `http(s)` URLs from the command line or the pipe always ask
    /// first, and `-sendpath` accepts only absolute paths to existing files.
    pub fn handle_incoming_activation(
        &mut self,
        activation: tg_app_core::core_types::startup_types::IncomingActivation,
        ctx: &egui::Context,
    ) {
        if self.is_passcode_locked() {
            self.queued_activations.push(activation);
            return;
        }
        for url in activation.urls {
            self.open_link_inner(&url, true, true);
        }

        let send_paths: Vec<PathBuf> = activation
            .send_paths
            .into_iter()
            .filter(|p| tg_platform::url_policy::is_local_absolute_path(p) && p.is_file())
            .collect();
        if !send_paths.is_empty() {
            let paths_display: Vec<String> =
                send_paths.iter().map(|p| p.display().to_string()).collect();
            let summary = paths_display.join("\n");
            let mut share = ShareBoxState::new(Vec::new(), Some(paths_display[0].clone()));
            share.comment = if paths_display.len() == 1 {
                format!("File: {summary}")
            } else {
                format!("Files ({}):\n{summary}", paths_display.len())
            };
            self.box_host
                .push(ctx, BoxKind::Share(share), &self.theme_manager);
        }
    }

    /// Applies a new embedded theme + optional accent color to `tg-ui`'s [`ThemeManager`] and persists it.
    pub fn apply_theme(&mut self, kind: EmbeddedType, accent: Option<ThemeColor>) {
        self.theme_manager.set_embedded(kind, accent);
        self.settings.record_theme(kind, accent);
        self.mark_settings_dirty();
    }

    /// Toggles between Day and Night modes via [`ThemeManager`].
    pub fn toggle_night_mode(&mut self) {
        let next_night = !self.theme_manager.is_dark();
        self.theme_manager.set_night_mode(next_night);
        self.settings.record_theme(
            self.theme_manager.embedded_type(),
            self.theme_manager.accent(),
        );
        self.mark_settings_dirty();
    }

    /// Records updated window geometry and schedules a debounced save.
    pub fn note_window_position(&mut self, pos: WindowPosition) {
        let clamped = pos.clamped();
        if self.settings.window_position != clamped {
            self.settings.window_position = clamped;
            self.mark_settings_dirty();
        }
    }

    /// Flushes any pending settings changes to disk immediately (called on exit).
    pub fn flush_settings(&mut self) {
        if let Some(session) = &self.session {
            self.settings.columns = session.columns().saved();
        }
        self.settings.native_window_frame = self.native_window_frame();
        self.settings_dirty_since = None;
        if let Err(err) = self.settings.save_to(&self.settings_path) {
            tracing::warn!(%err, path = %self.settings_path.display(), "Failed to persist shell settings");
        }
    }

    /// Switches into the authenticated main workspace (`Controller::setupMain`).
    pub fn setup_main(&mut self) {
        let mut session =
            SessionController::new(self.settings.columns, self.settings.folders_sidebar_enabled);
        // A live primary window opens with no chat, as tdesktop's `showByInitialId` clears the
        // section stack for `SeparateType::Primary` (`window_session_controller.cpp:1606-1608`),
        // and so it never waits for the chat list. The mock demo still opens a sample chat.
        if !self.session_backend.is_live_session() {
            let initial_dialogs = self.session_backend.dialogs(FilterId::ALL, "");
            if let Some(preferred) = initial_dialogs.get(1).or_else(|| initial_dialogs.first()) {
                session.show_peer_history(
                    preferred.peer.id,
                    SectionShow::instant_clear_stack(),
                    MsgId::SHOW_AT_UNREAD,
                );
            }
        }
        self.session = Some(session);
        self.layer_stack.hide_all();
        self.connection_indicator.set_force_hidden(false);
        self.root_screen = RootScreen::Main;
    }

    /// Installs (or clears) a live `tdata` local passcode verifier (`AccountManager::check_local_passcode`).
    pub fn set_external_passcode_verifier(
        &mut self,
        verifier: Option<tg_app_settings::settings::local_passcode::PasscodeVerifierFn>,
    ) {
        self.external_passcode_verifier = verifier;
    }

    /// Returns `true` when either a live `tdata` passcode or a mock `ShellSettings` passcode is armed.
    #[must_use]
    pub fn has_local_passcode(&self) -> bool {
        self.external_passcode_verifier.is_some() || self.settings.local_passcode.is_some()
    }

    /// Returns `true` while the passcode lock screen is shown (`Core::App().passcodeLocked()`).
    #[must_use]
    pub fn is_passcode_locked(&self) -> bool {
        self.root_screen == RootScreen::PasscodeLock
    }

    /// Returns the number of activations queued while locked (for tests and the host).
    #[must_use]
    pub fn queued_activation_count(&self) -> usize {
        self.queued_activations.len() + usize::from(self.queued_open_settings)
    }

    /// Locks this window even when it has no passcode of its own, because another account's
    /// workspace locked the app (the lock is app-wide in tdesktop, `application.cpp:1433`).
    pub fn lock_for_app_wide_passcode(&mut self) {
        if self.is_passcode_locked() {
            return;
        }
        self.enter_passcode_lock();
    }

    /// Closes every box, layer, viewer and floating window that could show account content over
    /// the lock screen (`Application::closeAdditionalWindows`, `application.cpp:197-203`).
    fn close_additional_windows(&mut self) {
        self.layer_stack.hide_all();
        self.close_content_windows();
    }

    /// Everything [`Self::close_additional_windows`] closes except the shell layer stack, which
    /// hosts the lock screen's own logout confirmation.
    fn close_content_windows(&mut self) {
        while !self.box_host.is_empty() {
            self.box_host.pop_immediate();
        }
        self.media_overlay.close();
        self.pip_window = None;
        self.round_float.detach();
        self.shell_extras.chat_preview.close();
        self.shell_extras.saved_windows.close_all();
        self.pending_webview_requests.clear();
        self.pending_overlay_requests.clear();
    }

    /// Replays activations queued while locked, once the lock is gone.
    fn replay_queued_activations(&mut self, ctx: &egui::Context) {
        if self.is_passcode_locked() {
            return;
        }
        if std::mem::take(&mut self.queued_open_settings) {
            self.open_settings();
        }
        for activation in std::mem::take(&mut self.queued_activations) {
            self.handle_incoming_activation(activation, ctx);
        }
    }

    /// Locks the window behind `PasscodeLockWidget` (`Controller::setupPasscodeLock`).
    pub fn setup_passcode_lock(&mut self) {
        if !self.has_local_passcode() {
            return;
        }
        self.enter_passcode_lock();
    }

    fn enter_passcode_lock(&mut self) {
        if let Some(settings) = self.main_widget.settings_mut() {
            settings.local_passcode.clear_manage_session();
            self.passcode_lock
                .set_system_unlock_enabled(settings.core_settings.system_unlock_enabled);
        }
        self.close_additional_windows();
        self.passcode_lock.reset_on_lock();
        self.connection_indicator.set_force_hidden(true);
        self.root_screen = RootScreen::PasscodeLock;
    }

    /// Injects a [`tg_platform::windows_hello::WindowsHelloVerifier`] into both the lock screen
    /// and Local Passcode settings.
    pub fn set_windows_hello_verifier(
        &mut self,
        verifier: Option<std::sync::Arc<dyn tg_platform::windows_hello::WindowsHelloVerifier>>,
    ) {
        self.passcode_lock
            .set_windows_hello_verifier(verifier.clone());
        if let Some(v) = verifier
            && let Some(settings) = self.main_widget.settings_mut()
        {
            settings.local_passcode.set_windows_hello_verifier(v);
        }
    }

    /// Updates the native Win32 `HWND` used to anchor Windows Hello consent prompts.
    pub fn set_window_hwnd(&mut self, hwnd: isize) {
        self.passcode_lock.set_window_hwnd(hwnd);
        if let Some(settings) = self.main_widget.settings_mut() {
            settings.local_passcode.set_window_hwnd(hwnd);
        }
    }

    /// Returns whether the OS native window frame is enabled (`CoreSettings::native_window_frame`).
    #[must_use]
    pub fn native_window_frame(&self) -> bool {
        self.main_widget
            .settings()
            .map_or(self.settings.native_window_frame, |s| {
                s.core_settings.native_window_frame
            })
    }

    /// Keeps `ShellSettings::native_window_frame` and the Settings panel's
    /// `CoreSettings::native_window_frame` in step and returns the effective value.
    ///
    /// Outside live mode the Settings panel is created lazily with `CoreSettings::new()`, so the
    /// first time it appears it is seeded from the persisted shell value; afterwards a toggle in
    /// Settings > Advanced is copied back and saved. Live mode reads the value from `tdata`.
    pub fn sync_native_window_frame_setting(&mut self) -> bool {
        if self.session_backend.is_live_session() {
            return self.native_window_frame();
        }
        let shell_value = self.settings.native_window_frame;
        let Some(settings) = self.main_widget.settings_mut() else {
            return shell_value;
        };
        if !self.native_frame_seeded {
            settings.core_settings.native_window_frame = shell_value;
            self.native_frame_seeded = true;
            return shell_value;
        }
        let core_value = settings.core_settings.native_window_frame;
        if core_value != shell_value {
            self.settings.native_window_frame = core_value;
            self.mark_settings_dirty();
        }
        core_value
    }

    /// Returns mutable access to the passcode lock screen widget (for tests / Hello wiring).
    pub fn passcode_lock_mut(&mut self) -> &mut PasscodeLockWidget {
        &mut self.passcode_lock
    }

    /// Clears the passcode lock screen (`Controller::clearPasscodeLock`).
    pub fn clear_passcode_lock(&mut self) {
        self.connection_indicator.set_force_hidden(false);
        self.root_screen = if self.session_backend.current_user().is_some()
            || self.session_backend.is_live_session()
        {
            RootScreen::Main
        } else {
            RootScreen::Intro
        };
    }

    /// Logs out and returns to the Intro flow (`Controller::setupIntro`).
    pub fn logout_to_intro(&mut self) {
        self.session_backend.logout();
        self.session = None;
        self.layer_stack.hide_all();
        self.media_overlay.close();
        self.pip_window = None;
        self.round_float.detach();
        self.intro.reset();
        self.connection_indicator.set_force_hidden(false);
        self.root_screen = RootScreen::Intro;
    }

    /// Validates `url` against the shared URL policy (`tg_platform::url_policy`) before queuing
    /// [`WebViewHostRequest::OpenExternalUrl`]. Disallowed schemes, local paths, and UNC shares
    /// show `lng_open_link_unsupported` and are never queued.
    fn queue_external_url(&mut self, url: &str) {
        match tg_platform::url_policy::validate_external_url(url) {
            Ok(validated) => {
                self.pending_webview_requests
                    .push(WebViewHostRequest::OpenExternalUrl(validated));
            }
            Err(_) => {
                self.layer_stack
                    .show_toast(tg_lang::tr::lng_open_link_unsupported());
            }
        }
    }

    /// Routes a clicked or IPC-activated link (from messages, profiles, WebView, or OS `tg://`
    /// activations):
    /// 1. Telegram deep links (`tg://`, `t.me/`, `telegram.me/`, `telegram.dog/`, `<user>.t.me/`,
    ///    `@username`, `internal:`, `tonsite://`) are routed through [`crate::window::deep_links`].
    /// 2. External URLs are validated against [`tg_platform::url_policy::validate_external_url`];
    ///    disallowed schemes, `file:`, UNC `\\host`, and local paths are rejected with a toast.
    /// 3. Instant View-capable URLs (`t.me/iv?...` or URLs returning a cached Instant View page
    ///    from `messages.getWebPage`) queue [`WebViewHostRequest::OpenInstantView`].
    /// 4. Everything else queues [`WebViewHostRequest::OpenExternalUrl`].
    pub fn open_link(&mut self, url: &str) {
        self.open_link_inner(url, false, false);
    }

    /// Routes a clicked hidden link (`messageEntityTextUrl` / `EntityType::CustomUrl`):
    /// enforces the shared URL scheme policy and shows tdesktop's `"Open this link?"`
    /// confirmation box (`HiddenUrlClickHandler::Open`) for non-trusted external destinations.
    pub fn open_hidden_link(&mut self, url: &str) {
        self.open_link_inner(url, true, false);
    }

    /// Routes an external link that unconditionally requires user confirmation (e.g. sandboxed
    /// widget `LinkPolicyDecision::ConfirmExternal`), after enforcing the shared URL scheme policy.
    pub fn open_external_link_requiring_confirmation(&mut self, url: &str) {
        self.open_link_inner(url, true, true);
    }

    fn open_link_inner(&mut self, url: &str, is_hidden: bool, force_confirm: bool) {
        self.open_link_routed(url, is_hidden, force_confirm, false);
    }

    /// Opens a link a Mini App handed back (`web_app_open_tg_link`, local navigation): only
    /// links that route as Telegram links are opened, anything else is dropped
    /// (`WebViewInstance::botHandleLocalUri`).
    pub fn open_bot_local_link(&mut self, url: &str) {
        self.open_link_routed(url, false, false, true);
    }

    fn open_link_routed(
        &mut self,
        url: &str,
        is_hidden: bool,
        force_confirm: bool,
        telegram_only: bool,
    ) {
        if self.is_passcode_locked() {
            return;
        }
        let outcome = if let Some(session) = self.session.as_mut() {
            session.route_deep_link(url, &mut *self.session_backend)
        } else {
            dispatch_unauthenticated_deep_link(url)
        };

        match outcome {
            DeepLinkOutcome::NotTelegram if telegram_only => {}
            DeepLinkOutcome::NotTelegram => {
                match evaluate_external_link(url, is_hidden, force_confirm) {
                    ExternalLinkAction::Rejected => {
                        self.layer_stack
                            .show_toast(tg_lang::tr::lng_open_link_unsupported());
                    }
                    ExternalLinkAction::Confirm {
                        url: validated,
                        host_range,
                        suspicious_ranges,
                    } => {
                        self.media_overlay.close();
                        self.box_host.push_immediate(
                            BoxKind::HiddenLinkConfirm(build_hidden_link_confirm_box(
                                validated,
                                host_range,
                                suspicious_ranges,
                            )),
                            &self.theme_manager,
                        );
                    }
                    ExternalLinkAction::OpenNow(validated) => {
                        let (base_url, fragment) = split_fragment(validated.trim());
                        if matches!(
                            WebViewBackend::fetch_iv_page(&*self.session_backend, base_url, None),
                            Ok(Some(_))
                        ) {
                            self.pending_webview_requests.push(
                                WebViewHostRequest::OpenInstantView {
                                    url: base_url.to_owned(),
                                    webpage_id: None,
                                    fragment: fragment.map(ToOwned::to_owned),
                                },
                            );
                        } else {
                            self.queue_external_url(&validated);
                        }
                    }
                }
            }
            DeepLinkOutcome::Handled => {}
            DeepLinkOutcome::PrefillCompose { peer_id, text } => {
                if self
                    .session
                    .as_ref()
                    .is_some_and(|s| s.active_peer() == peer_id)
                {
                    self.main_widget.prefill_compose(peer_id, text);
                }
            }
            // `tg://resolve?domain=<bot>&start=<token>` stores the token and opens the bot chat,
            // which shows the bottom START bar; nothing is sent until the user presses it
            // (`Window::SessionNavigation::showPeerByLink`, `HistoryWidget::_botStart`).
            DeepLinkOutcome::StartBot {
                bot_id,
                peer_id: None,
                start_param,
            } => {
                self.main_widget
                    .history_mut()
                    .set_bot_start_token(bot_id, start_param);
                if let Some(session) = self.session.as_mut() {
                    session.show_peer_history(
                        bot_id,
                        SectionShow::clear_stack(),
                        MsgId::SHOW_AT_UNREAD,
                    );
                }
            }
            DeepLinkOutcome::StartBot {
                bot_id,
                peer_id,
                start_param,
            } => {
                let bot_uid = tg_bots::stand_in::UserId(bot_id.bare());
                let params = match peer_id {
                    Some(chat_peer) => tg_bots::protocol::StartBotParams::for_group_chat(
                        bot_uid,
                        tg_app_core::history::backend::to_bot_peer(chat_peer),
                        start_param,
                        0,
                    ),
                    None => {
                        tg_bots::protocol::StartBotParams::for_private_chat(bot_uid, start_param, 0)
                    }
                };
                match tg_app_core::bots::backend::BotsBackend::start_bot(
                    &mut *self.session_backend,
                    &params,
                ) {
                    Ok(_) => {
                        let target_peer = peer_id.unwrap_or(bot_id);
                        self.main_widget
                            .notify_external_message(target_peer, &*self.session_backend);
                    }
                    Err(err) => {
                        self.layer_stack.show_toast(err);
                    }
                }
            }
            DeepLinkOutcome::OpenSettingsSection {
                page,
                section,
                highlight,
                toast,
            } => {
                let settings_backend = self.session_backend.create_settings_backend();
                self.main_widget.open_settings_page_and_section(
                    page,
                    section,
                    settings_backend,
                    self.theme_manager.embedded_type(),
                    self.theme_manager.accent(),
                );
                if let Some(ctrl) = self.main_widget.settings_mut() {
                    ctrl.set_highlight_control(section, highlight.as_deref());
                }
                if let Some(session) = self.session.as_mut() {
                    session.show_settings(page);
                }
                if let Some(msg) = toast {
                    self.layer_stack.show_toast(msg);
                }
            }
            DeepLinkOutcome::OpenSetLanguageLink { url } => {
                let settings_backend = self.session_backend.create_settings_backend();
                match self.main_widget.open_set_language_link(
                    &url,
                    settings_backend,
                    self.theme_manager.embedded_type(),
                    self.theme_manager.accent(),
                ) {
                    Ok(_) => {
                        if let Some(session) = self.session.as_mut() {
                            session.show_settings(SettingsPage::Main);
                        }
                    }
                    Err(err) => {
                        self.layer_stack.show_toast(err);
                    }
                }
            }
            DeepLinkOutcome::ShowBox(kind) => {
                self.media_overlay.close();
                self.box_host.push_immediate(*kind, &self.theme_manager);
            }
            DeepLinkOutcome::ShowModal(modal) => {
                self.layer_stack.show_box(modal);
            }
            DeepLinkOutcome::Toast(msg) => {
                self.layer_stack.show_toast(msg);
            }
            DeepLinkOutcome::OpenMainMenu => {
                self.layer_stack.open_main_menu();
            }
            DeepLinkOutcome::FocusSearch => {
                self.trigger_dialogs_shortcut(DialogsShortcut::FocusSearch);
            }
            DeepLinkOutcome::OpenRecentCalls => {
                self.pending_open_recent_calls = true;
            }
            DeepLinkOutcome::OpenMiniApp {
                bot_id,
                start_param,
            } => {
                self.open_mini_app_from_link(bot_id, None, start_param);
            }
            DeepLinkOutcome::OpenInstantView { url, fragment } => {
                self.pending_webview_requests
                    .push(WebViewHostRequest::OpenInstantView {
                        url,
                        webpage_id: None,
                        fragment,
                    });
            }
            DeepLinkOutcome::OpenExternalUrl(target) => {
                if is_hidden || force_confirm {
                    match evaluate_external_link(&target, is_hidden, force_confirm) {
                        ExternalLinkAction::Rejected => {
                            self.layer_stack
                                .show_toast(tg_lang::tr::lng_open_link_unsupported());
                        }
                        ExternalLinkAction::Confirm {
                            url: validated,
                            host_range,
                            suspicious_ranges,
                        } => {
                            self.media_overlay.close();
                            self.box_host.push_immediate(
                                BoxKind::HiddenLinkConfirm(build_hidden_link_confirm_box(
                                    validated,
                                    host_range,
                                    suspicious_ranges,
                                )),
                                &self.theme_manager,
                            );
                        }
                        ExternalLinkAction::OpenNow(validated) => {
                            self.queue_external_url(&validated);
                        }
                    }
                } else {
                    self.queue_external_url(&target);
                }
            }
            DeepLinkOutcome::OpenOverlay(req) => {
                self.pending_overlay_requests.push(req);
            }
            DeepLinkOutcome::OpenChannelDirectMessages { peer_id } => {
                let (title, is_admin) =
                    tg_app_history::history::backend::HistoryBackend::peer_info(
                        &*self.session_backend,
                        peer_id,
                    )
                    .map(|info| (info.summary.title, info.summary.can_send))
                    .or_else(|| {
                        InfoBackend::peer_profile(&*self.session_backend, peer_id)
                            .map(|p| (p.title, p.can_edit))
                    })
                    .unwrap_or_else(|| ("Channel".to_owned(), false));
                self.main_widget
                    .history_mut()
                    .open_monoforum_section(peer_id, title, is_admin);
            }
            DeepLinkOutcome::LoginCode(code) => {
                self.pending_login_code = Some(code.clone());
                self.layer_stack
                    .show_toast(format!("Login code received: {code}"));
            }
        }
    }

    /// Opens the fullscreen [`MediaOverlayWidget`] (or [`tg_app_media::media::AudioPlayerBar`] for voice)
    /// for a message attachment in `peer_id`.
    pub fn open_history_media(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        album_index: Option<usize>,
    ) {
        let slice = self
            .session_backend
            .load_slice_around(peer_id, msg_id, 0, 0);
        if let Some(msg) = slice.messages.into_iter().find(|m| m.id == msg_id)
            && matches!(msg.media, Some(HistoryMedia::Voice(_)))
        {
            if let Some(voice_id) = self
                .session_backend
                .resolve_audio_msg_id(peer_id, msg_id)
                .or_else(|| self.session_backend.sample_voice_id())
            {
                let _ = self
                    .main_widget
                    .play_audio_track(&*self.session_backend, voice_id, false);
            }
            return;
        }

        let Some(item_id) =
            self.session_backend
                .resolve_media_item_id(peer_id, msg_id, album_index)
        else {
            self.layer_stack
                .show_toast("Unable to open media: item not loaded yet");
            return;
        };
        let now_ms = self.elapsed_ms();
        if let Err(err) =
            self.media_overlay
                .open_item(&*self.session_backend, peer_id.0, item_id, now_ms)
        {
            self.layer_stack
                .show_toast(format!("Unable to open media: {err}"));
        } else {
            self.attach_video_sponsored_for_current(peer_id, msg_id, now_ms);
        }
    }

    /// Attaches channel/video sponsored messages (`media_view_playback_sponsored`) to the
    /// currently opened video inside `media_overlay`, if the backend exposes any.
    pub fn attach_video_sponsored_for_current(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        now_ms: i64,
    ) {
        let is_video = self
            .media_overlay
            .gallery()
            .current()
            .is_some_and(|i| i.kind.shows_playback_controls());
        if !is_video {
            return;
        }
        let ads = tg_app_history::history::backend::HistoryBackend::as_layer233_extras_mut(
            &mut *self.session_backend,
        )
        .and_then(|ex| ex.get_sponsored_messages(peer_id, Some(msg_id)).ok())
        .unwrap_or_default();
        if ads.is_empty() {
            return;
        }
        let data = tg_app_media::media::SponsoredForVideo {
            list: ads,
            start_delay_ms: tg_app_media::media::SPONSORED_START_DELAY_MIN_MS,
            between_delay_ms: 60_000,
            state: tg_app_media::media::SponsoredForVideoState::default(),
        };
        let _ = self.media_overlay.attach_video_sponsored(data, now_ms);
    }

    /// Opens a shared media item clicked inside the Info/Profile panel.
    pub fn open_shared_media(&mut self, peer_id: PeerId, item_id: i32, tab: SharedMediaTab) {
        match tab {
            SharedMediaTab::Links => {
                let items = self
                    .session_backend
                    .shared_media(peer_id, SharedMediaTab::Links, "");
                if let Some(tg_app_info::info::MediaItem::Link { url, .. }) =
                    items.into_iter().find(|it| it.id() == item_id)
                {
                    self.open_link(&url);
                }
            }
            SharedMediaTab::Voice => {
                if let Some(voice_id) = self
                    .session_backend
                    .resolve_audio_msg_id(peer_id, MsgId(item_id))
                    .or_else(|| self.session_backend.sample_voice_id())
                {
                    let _ =
                        self.main_widget
                            .play_audio_track(&*self.session_backend, voice_id, false);
                }
            }
            SharedMediaTab::Music => {
                if let Some(song_id) = self
                    .session_backend
                    .resolve_audio_msg_id(peer_id, MsgId(item_id))
                    .or_else(|| self.session_backend.sample_audio_id())
                {
                    let _ =
                        self.main_widget
                            .play_audio_track(&*self.session_backend, song_id, false);
                }
            }
            SharedMediaTab::PhotosVideos | SharedMediaTab::Gifs | SharedMediaTab::Files => {
                let Some(media_id) =
                    self.session_backend
                        .resolve_media_item_id(peer_id, MsgId(item_id), None)
                else {
                    self.layer_stack
                        .show_toast("Unable to open media: item not loaded yet");
                    return;
                };
                let now_ms = self.elapsed_ms();
                if let Err(err) = self.media_overlay.open_item(
                    &*self.session_backend,
                    peer_id.0,
                    media_id,
                    now_ms,
                ) {
                    self.layer_stack
                        .show_toast(format!("Unable to open media: {err}"));
                } else {
                    self.attach_video_sponsored_for_current(peer_id, MsgId(item_id), now_ms);
                }
            }
        }
    }

    /// Opens a Bot Mini App window for `peer_id`.
    pub fn open_bot_mini_app(&mut self, peer_id: PeerId) {
        self.open_bot_mini_app_with_params(peer_id, None, None);
    }

    /// Opens a Bot Mini App (or embedded HTML5 Game WebView) for `bot_id` with optional
    /// explicit launch `url` and `start_param` (`inline_bots/bot_attach_web_view.cpp`).
    pub fn open_bot_mini_app_with_params(
        &mut self,
        bot_id: PeerId,
        url: Option<String>,
        start_param: Option<String>,
    ) {
        self.session_backend.set_pending_mini_app_url(bot_id, url);
        self.pending_webview_requests
            .push(WebViewHostRequest::OpenMiniApp {
                bot_id: bot_id.bare() as i64,
                start_param,
            });
    }

    /// Opens a Mini App requested by a link: unless the user already confirmed this bot, shows
    /// the `confirmOpen` box first (`bot_attach_web_view.cpp:1148-1181`). Like tdesktop's clicked
    /// links (`botAppForceConfirmation`), being verified does not skip the box.
    pub fn open_mini_app_from_link(
        &mut self,
        bot_id: PeerId,
        url: Option<String>,
        start_param: Option<String>,
    ) {
        if self.is_passcode_locked() {
            return;
        }
        if self.is_peer_trusted_open_webview(bot_id) {
            self.open_bot_mini_app_with_params(bot_id, url, start_param);
            return;
        }
        let bot_name = self.peer_display_name(bot_id);
        self.media_overlay.close();
        self.box_host.push_immediate(
            BoxKind::MiniAppOpenConfirm(MiniAppOpenConfirmBoxState::new(
                bot_id,
                bot_name,
                url,
                start_param,
            )),
            &self.theme_manager,
        );
    }

    /// Folder tabs for the "Forward to..." box: "All" plus every chat folder with its chats
    /// (`Ui::AddChatFiltersTabsStrip`, shown only with more than one folder).
    fn forward_folder_tabs(&self) -> Vec<tg_app_info::boxes::share::ShareFolderTab> {
        use tg_app_info::dialogs::backend::{DialogListQuery, DialogsBackend};
        let backend = &*self.session_backend;
        DialogsBackend::chat_folders(backend)
            .into_iter()
            .map(|folder| {
                let all = folder.id == tg_app_core::stand_in::types::FilterId::ALL;
                let peers = if all {
                    Vec::new()
                } else {
                    DialogsBackend::dialog_slice(
                        backend,
                        DialogListQuery::Filter(folder.id),
                        0,
                        usize::MAX / 2,
                    )
                    .entries
                    .into_iter()
                    .map(|d| d.peer_id)
                    .collect()
                };
                tg_app_info::boxes::share::ShareFolderTab {
                    id: folder.id.0,
                    title: if all {
                        tg_lang::tr::lng_filters_all_short()
                    } else {
                        folder.title
                    },
                    peers,
                }
            })
            .collect()
    }

    /// A plain click on a chat in "Forward to..." (`ShowForwardMessagesBox`, `singleChosen`):
    /// Saved Messages gets the messages at once (`ForwardToSelf`); any other chat opens with
    /// the forward bar on its composer (`MainWidget::setForwardDraft`).
    fn apply_forward_single_choice(&mut self, ctx: &egui::Context) {
        let Some(BoxKind::Share(share)) = self.box_host.top_kind_mut() else {
            return;
        };
        let Some(target) = share.take_chosen_single() else {
            return;
        };
        let Some(from_peer) = share.from_peer else {
            return;
        };
        let msg_ids: Vec<MsgId> = share.message_ids.iter().copied().map(MsgId).collect();
        let count = msg_ids.len();
        self.box_host.pop(ctx, &self.theme_manager);
        if msg_ids.is_empty() {
            return;
        }
        let self_peer =
            tg_app_core::history::backend::HistoryBackend::self_peer(&*self.session_backend).0;
        if target == self_peer && from_peer != self_peer {
            let ids: Vec<i32> = msg_ids.iter().map(|id| id.0).collect();
            match InfoBackend::forward_messages(&mut *self.session_backend, &[target], &ids, "") {
                Ok(_) => {
                    let chat = tg_lang::tr::lng_saved_messages();
                    let text = if count == 1 {
                        tg_lang::tr::lng_share_message_to_saved(&chat)
                    } else {
                        tg_lang::tr::lng_share_messages_to_saved(&chat)
                    };
                    self.layer_stack.show_toast(text.replace("**", ""));
                }
                Err(err) => self.layer_stack.show_toast(err),
            }
            return;
        }
        let source = self.peer_display_name(from_peer);
        let summary = if count == 1 {
            format!("1 message from {source}")
        } else {
            format!("{count} messages from {source}")
        };
        self.main_widget
            .history_mut()
            .set_pending_forward_draft(target, from_peer, msg_ids, summary);
        if let Some(session) = self.session.as_mut() {
            session.show_peer_history(target, SectionShow::clear_stack(), MsgId::SHOW_AT_UNREAD);
        }
    }

    /// Opens a Mini App launched from the chat. Keyboard, bot menu and switch-inline buttons
    /// (`confirm_open`) go through `WebViewInstance::confirmOpen` (`bot_attach_web_view.cpp`):
    /// a verified or already trusted bot opens directly, any other bot shows the box first.
    pub fn open_mini_app_from_chat(
        &mut self,
        bot_id: PeerId,
        url: Option<String>,
        start_param: Option<String>,
        confirm_open: bool,
    ) {
        let verified =
            InfoBackend::peer_profile(&*self.session_backend, bot_id).is_some_and(|p| p.verified);
        if !confirm_open || verified {
            self.open_bot_mini_app_with_params(bot_id, url, start_param);
        } else {
            self.open_mini_app_from_link(bot_id, url, start_param);
        }
    }

    /// Returns `true` once the user confirmed opening `bot_id`'s Mini App (this session or a
    /// previous one).
    #[must_use]
    pub fn is_peer_trusted_open_webview(&self, bot_id: PeerId) -> bool {
        // `isPeerTrustedOpenWebView`: persisted in the account's session settings (WP-74).
        self.trusted_open_webview.contains(&bot_id)
            || self.session_backend.is_mini_app_bot_trusted(bot_id)
    }

    fn peer_display_name(&self, peer_id: PeerId) -> String {
        InfoBackend::peer_profile(&*self.session_backend, peer_id)
            .map(|p| p.title)
            .unwrap_or_default()
    }

    /// Sends `/start <token>` after the user pressed START (`HistoryWidget::sendBotStartCommand`).
    /// `PeerMenuUnblockUserWithBotRestart`: unblock the bot, send it /start and open its chat.
    fn restart_bot(&mut self, bot_id: PeerId) {
        match tg_app_core::dialogs::backend::DialogsBackend::unblock_and_restart_bot(
            &mut *self.session_backend,
            bot_id,
        ) {
            Ok(()) => {
                self.main_widget
                    .notify_external_message(bot_id, &*self.session_backend);
                if let Some(session) = self.session.as_mut() {
                    session.show_peer_history(bot_id, SectionShow::default(), MsgId::SHOW_AT_END);
                }
            }
            Err(err) => self.layer_stack.show_toast(err),
        }
    }

    /// The block box's "Block": `report_spam_and_block` (block, then `messages.reportSpam` and
    /// `deleteConversation` as chosen, `PeerMenuBlockUserBox`), going back from a deleted chat,
    /// and the "{user} is now blocked" toast.
    fn block_user_confirmed(&mut self, choice: &tg_app_info::boxes::block_user::BlockUserChoice) {
        let result = tg_app_core::dialogs::backend::DialogsBackend::report_spam_and_block(
            &mut *self.session_backend,
            choice.peer_id,
            choice.report_spam,
            choice.delete_chat,
        );
        if let Err(err) = result {
            self.layer_stack.show_toast(err);
            return;
        }
        if choice.delete_chat
            && let Some(session) = self.session.as_mut()
            && session.active_peer() == choice.peer_id
        {
            session.show_back_from_stack(SectionShow::default());
        }
        self.layer_stack
            .show_toast(tg_lang::tr::lng_new_contact_block_done(&choice.name));
    }

    fn send_bot_start(&mut self, bot_id: PeerId, start_param: String) {
        let bot_uid = tg_bots::stand_in::UserId(bot_id.bare());
        let params = tg_bots::protocol::StartBotParams::for_private_chat(bot_uid, start_param, 0);
        match tg_app_core::bots::backend::BotsBackend::start_bot(
            &mut *self.session_backend,
            &params,
        ) {
            Ok(_) => {
                self.main_widget
                    .notify_external_message(bot_id, &*self.session_backend);
            }
            Err(err) => self.layer_stack.show_toast(err),
        }
    }

    /// Opens `target_peer` and prefills its compose field with `@bot_username query`
    /// (`api/api_bot.cpp` switch-inline destination handoff).
    pub fn open_peer_with_inline_query(
        &mut self,
        target_peer: PeerId,
        bot_id: PeerId,
        bot_username: &str,
        query: &str,
    ) {
        self.main_widget.history_mut().set_pending_inline_query(
            target_peer,
            bot_id,
            bot_username,
            query,
        );
        if let Some(session) = self.session.as_mut() {
            session.show_peer_history(target_peer, SectionShow::clear_stack(), MsgId::SHOW_AT_END);
        }
    }

    /// Opens the appropriate Statistics / Boosts / Monetization screen for `peer_id`.
    pub fn open_statistics_for_peer(&mut self, peer_id: PeerId, boosts: bool) {
        let screen = if boosts {
            StatisticsScreen::Boosts
        } else {
            match InfoBackend::peer_profile(&*self.session_backend, peer_id).map(|p| p.kind) {
                Some(InfoPeerKind::Group | InfoPeerKind::Supergroup) => {
                    StatisticsScreen::Supergroup
                }
                Some(InfoPeerKind::Bot) => StatisticsScreen::BotEarn,
                _ => StatisticsScreen::Channel,
            }
        };
        if let Some(session) = self.session.as_mut() {
            session.show_statistics(peer_id, screen);
        }
    }

    /// Opens a chat directly in response to a clicked notification (including cross-account clicks).
    pub fn open_chat_from_notification(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        draft: Option<String>,
        ctx: Option<&egui::Context>,
    ) {
        if self.root_screen != RootScreen::Main {
            return;
        }
        Backend::mark_read(&mut *self.session_backend, peer_id);
        if let Some(session) = self.session.as_mut() {
            session.show_peer_history(peer_id, SectionShow::clear_stack(), msg_id);
        }
        if let Some(draft_text) = draft {
            self.main_widget.history_mut().compose_mut().text = draft_text;
        }
        self.main_widget
            .notify_external_message(peer_id, &*self.session_backend);
        if let Some(c) = ctx {
            c.send_viewport_cmd(ViewportCommand::Visible(true));
            c.send_viewport_cmd(ViewportCommand::Minimized(false));
            c.send_viewport_cmd(ViewportCommand::Focus);
        }
    }

    /// Processes any pending notification clicks (`NotifyBackend::open_chat`) queued on the shared session.
    pub fn poll_notification_opens(&mut self, ctx: Option<&egui::Context>) {
        let opens = self.session_backend.take_pending_chat_opens();
        for open in opens {
            self.open_chat_from_notification(open.peer_id, open.msg_id, open.draft, ctx);
        }
    }

    /// Routes events emitted by [`tg_app_media::webview::AppWebViewHost`] back into the main app.
    pub fn handle_webview_event(&mut self, event: AppWebViewEvent, ctx: &egui::Context) {
        match event {
            AppWebViewEvent::OpenTelegramLink { uri } => self.open_bot_local_link(&uri),
            AppWebViewEvent::OpenChannel {
                username: Some(u), ..
            } => {
                self.open_link(&format!("https://t.me/{u}"));
            }
            AppWebViewEvent::OpenMediaViewer { .. } => {
                let fallback = if self.session_backend.is_live_session() {
                    PeerId::ZERO
                } else {
                    session_peers::CHANNEL_DEV_LOG
                };
                let peer = self
                    .session
                    .as_ref()
                    .map_or(fallback, SessionController::active_peer);
                if !peer.is_zero() {
                    self.open_history_media(peer, MsgId(0), Some(0));
                }
            }
            AppWebViewEvent::ShareLink { url, .. } => {
                self.box_host.push(
                    ctx,
                    BoxKind::Share(ShareBoxState::new(Vec::new(), Some(url))),
                    &self.theme_manager,
                );
            }
            AppWebViewEvent::OpenInstantView {
                url,
                webpage_id,
                fragment,
            } => {
                self.pending_webview_requests
                    .push(WebViewHostRequest::OpenInstantView {
                        url,
                        webpage_id,
                        fragment,
                    });
            }
            AppWebViewEvent::OpenInvoice { slug } => {
                self.open_link(&format!("tg://invoice?slug={slug}"));
            }
            AppWebViewEvent::OpenTonSite { uri, .. } => {
                self.open_link(&uri);
            }
            AppWebViewEvent::OpenExternalUrl { url }
                if url == tg_platform::url_policy::WINDOWS_LOCATION_SETTINGS_URI => {}
            // The webview host already opened the URL through its policy-checked opener
            // (`UrlClickHandler::Open` runs once in `iv_instance.cpp`); queueing it again here
            // opened every Instant View / Mini App external link twice.
            AppWebViewEvent::OpenExternalUrl { url } => {
                if tg_platform::url_policy::validate_external_url(&url).is_err() {
                    self.layer_stack
                        .show_toast(tg_lang::tr::lng_open_link_unsupported());
                }
            }
            AppWebViewEvent::Toast { message } => self.layer_stack.show_toast(message),
            AppWebViewEvent::QuitApp => ctx.send_viewport_cmd(ViewportCommand::Close),
            _ => {}
        }
    }

    /// Renders the entire window frame and processes shortcuts, overlays, media viewer, and persistence.
    pub fn show(&mut self, ui: &mut Ui) {
        self.show_with_stories(ui, None);
    }

    /// Renders the entire window frame, optionally embedding an [`AppStoriesHost`]'s collapsible
    /// stories strip at the top of the dialogs column.
    pub fn show_with_stories(&mut self, ui: &mut Ui, stories: Option<&mut AppStoriesHost>) {
        let ctx = ui.ctx().clone();
        let body_rect = ui.max_rect();
        self.session_backend.sync_cached_user();
        let palette = self.palette();
        let now_ms = self.elapsed_ms();

        self.handle_global_shortcuts(&ctx, now_ms);

        match self.root_screen {
            RootScreen::Intro => {
                let act = self
                    .intro
                    .show(ui, &mut *self.session_backend, &palette, &self.strings);
                if let Some(act) = act {
                    match act {
                        IntroAction::SignedIn(user) => {
                            self.session_backend.sign_in_as(user);
                            self.setup_main();
                            self.layer_stack
                                .show_toast("Signed in to local mock session");
                        }
                        IntroAction::OpenSettings => {
                            self.layer_stack.show_box(ModalBox::ProxySettings);
                        }
                    }
                }

                // Connection status pill is also hosted in Intro (`Intro::Widget::_connecting`).
                let pill_layout = self.connection_indicator.update(
                    tg_app_core::clock::now(),
                    self.session_backend.connection_status(),
                    self.session_backend.proxy_enabled(),
                    &self.strings,
                );
                if pill_layout.visible {
                    let screen = ui.max_rect();
                    let pill_pos = egui::Pos2::new(screen.left() + 12.0, screen.bottom() - 44.0);
                    egui::Area::new(egui::Id::unique("tg_intro_connecting_pill"))
                        .order(egui::Order::Foreground)
                        .fixed_pos(pill_pos)
                        .show(&ctx, |ui| {
                            if let Some(act) = self.connection_indicator.show(
                                ui,
                                &palette,
                                &self.strings,
                                &pill_layout,
                            ) {
                                match act {
                                    ConnectionWidgetAction::OpenProxySettings => {
                                        self.layer_stack.show_box(ModalBox::ProxySettings);
                                    }
                                    ConnectionWidgetAction::RetryNow => {
                                        self.session_backend.retry_connection();
                                    }
                                }
                            }
                        });
                }
            }
            RootScreen::PasscodeLock => {
                let act = if let Some(verifier) = self.external_passcode_verifier.clone() {
                    self.passcode_lock
                        .show_with(ui, |cand| verifier(cand), &palette, &self.strings)
                } else {
                    let expected = self
                        .settings
                        .local_passcode
                        .as_deref()
                        .unwrap_or_default()
                        .to_owned();
                    self.passcode_lock
                        .show(ui, &expected, &palette, &self.strings)
                };
                if let Some(act) = act {
                    match act {
                        PasscodeLockAction::Unlock => {
                            self.clear_passcode_lock();
                        }
                        PasscodeLockAction::RequestLogout => {
                            self.layer_stack.show_box(ModalBox::ConfirmLogout);
                        }
                    }
                }
            }
            RootScreen::Main => {
                if (self.session_backend.current_user().is_some()
                    || self.session_backend.is_live_session())
                    && let Some(session) = self.session.as_mut()
                {
                    if let Some(settings) = self.main_widget.settings_mut() {
                        settings.set_update_snapshot(self.update_snapshot.clone());
                    }
                    let embedded = self.theme_manager.embedded_type();
                    let accent = self.theme_manager.accent();
                    let emitted = self.main_widget.show_with_stories(
                        ui,
                        session,
                        &mut *self.session_backend,
                        &mut self.connection_indicator,
                        self.settings.theme_mode,
                        embedded,
                        accent,
                        &self.theme_manager,
                        &palette,
                        &self.strings,
                        now_ms,
                        stories,
                    );
                    let mut settings_update_action = None;
                    let mut settings_update_prefs = None;
                    if let Some(settings) = self.main_widget.settings_mut() {
                        if !settings.has_synced_update_snapshot() {
                            settings.set_update_snapshot(self.update_snapshot.clone());
                            ctx.request_repaint();
                        }
                        settings_update_action = settings.take_update_action();
                        settings_update_prefs = settings.take_update_prefs_change();
                    }
                    if let Some(upd_act) = settings_update_action {
                        match upd_act {
                            tg_app_core::core_types::startup_types::UpdateUiAction::CheckNow => {
                                self.request_update_check();
                            }
                            tg_app_core::core_types::startup_types::UpdateUiAction::RestartToUpdate => {
                                self.request_restart_to_update();
                            }
                        }
                    }
                    if let Some((auto_update, install_beta)) = settings_update_prefs {
                        self.pending_update_prefs = Some((auto_update, install_beta));
                        self.update_snapshot.auto_update_enabled = auto_update;
                        self.update_snapshot.allow_beta = install_beta;
                    }
                    for act in emitted {
                        self.handle_main_widget_action(act, &ctx);
                    }
                }
            }
        }
        self.replay_queued_activations(&ctx);
        let locked = self.is_passcode_locked();

        // Main menu overlay drawer (`MainWindow::showMainMenu`).
        if !locked
            && self.layer_stack.is_main_menu_open()
            && let Some(user) = self.session_backend.current_user().cloned().or_else(|| {
                self.session_backend.is_live_session().then(|| AuthUser {
                    id: PeerId::ZERO,
                    name: "Telegram Account".to_owned(),
                    username: String::new(),
                    phone: String::new(),
                    bio: String::new(),
                    initials: "TG".to_owned(),
                    avatar_rgb: [0x4D, 0xA6, 0xE8],
                })
            })
        {
            let fallback_single = [MainMenuAccountItem::from_active_user(&user, 0)];
            let accounts_slice: &[MainMenuAccountItem] = if self.menu_accounts.is_empty() {
                &fallback_single
            } else {
                &self.menu_accounts
            };
            let can_add =
                self.session_backend.is_live_session() && accounts_slice.len() < self.max_accounts;
            self.main_menu.set_body_rect(Some(body_rect));
            let menu_bots = self
                .session_backend
                .as_bots_backend()
                .map(|bots| main_menu_bots(&bots.load_attach_menu()))
                .unwrap_or_default();
            self.main_menu.set_menu_bots(menu_bots);
            if let Some(menu_act) = self.main_menu.show_with_accounts(
                &ctx,
                &user,
                accounts_slice,
                can_add,
                self.max_accounts,
                self.settings.theme_mode,
                self.has_local_passcode(),
                &self.update_snapshot,
                &palette,
                &self.strings,
            ) {
                self.handle_main_menu_action(menu_act, user.id, &ctx);
            }
        }

        // Rich modal boxes (`boxes::BoxHost`).
        if self.root_screen == RootScreen::Main {
            let active_peer = self.session.as_ref().and_then(|s| {
                let pid = s.active_peer();
                (pid != PeerId::ZERO).then(|| {
                    let title = InfoBackend::peer_profile(&*self.session_backend, pid)
                        .map(|p| p.title)
                        .unwrap_or_default();
                    (pid, title)
                })
            });
            tg_app_info::boxes::chat_import::try_handle_dropped_import_files(
                &ctx,
                &mut self.box_host,
                &*self.session_backend,
                &self.theme_manager,
                active_peer,
            );
        }
        if let Some(BoxKind::AddProxy(proxy_box)) = self.box_host.top_kind_mut() {
            proxy_box.sync_check_status(&mut *self.session_backend);
        }
        let screen_rect = ctx.content_rect();
        // Nothing but the lock screen (and its own logout confirmation) renders while locked.
        let box_events = if locked {
            Vec::new()
        } else {
            self.box_host.show(
                &ctx,
                screen_rect,
                &mut *self.session_backend,
                &self.theme_manager,
            )
        };
        for bev in box_events {
            self.handle_box_event(bev, &ctx);
        }
        self.apply_forward_single_choice(&ctx);

        // In mock mode (where LiveActiveWorkspace is absent), route queued theme/background
        // deep-link overlay requests straight into AppThemesHost.
        if !self.session_backend.is_live_session() && !self.pending_overlay_requests.is_empty() {
            self.pending_overlay_requests.retain(|req| {
                !crate::window::deep_links::dispatch_themes_deep_link_overlay(&ctx, req)
            });
        }

        // Shell modal boxes and toasts hosted by the layer stack.
        if let Some(layer_act) = self.layer_stack.show_boxes_and_toasts(
            &ctx,
            &palette,
            &self.strings,
            self.session_backend.connection_status(),
            self.session_backend.proxy_enabled(),
        ) {
            self.handle_layer_action(layer_act);
        }

        // Fullscreen media viewer overlay (`Media::View::OverlayWidget`).
        if locked {
            self.close_content_windows();
        }
        if self.media_overlay.is_open() {
            let ov_events =
                self.media_overlay
                    .show(&ctx, screen_rect, &*self.session_backend, now_ms);
            for ev in ov_events {
                self.handle_overlay_event(ev, &ctx);
            }
        }
        self.media_overlay
            .sync_screenshot_protection(self.native_hwnd);
        self.media_fullscreen
            .sync(&ctx, self.media_overlay.is_open());

        // Floating Picture-in-Picture window (`Media::View::Pip`).
        if let Some(mut pip) = self.pip_window.take() {
            let pip_event = pip.show(&ctx, screen_rect, now_ms);
            let mut keep_pip = true;
            if let Some(ev) = pip_event {
                match ev {
                    PipEvent::Closed => keep_pip = false,
                    PipEvent::EnlargeToViewer { item, resume } => {
                        keep_pip = false;
                        let _ = self.media_overlay.reopen_from_pip(
                            &*self.session_backend,
                            &item,
                            resume,
                            now_ms,
                        );
                    }
                }
            }
            if keep_pip {
                self.pip_window = Some(pip);
            }
        }

        // Floating round-video player (`Media::Player::Float`).
        if self.round_float.is_visible()
            && let Some(ev) = self.round_float.show(&ctx, screen_rect, now_ms)
        {
            match ev {
                RoundFloatEvent::Dismissed(_) => self.round_float.detach(),
                RoundFloatEvent::JumpToMessage(full_id) => {
                    if let Some(session) = self.session.as_mut() {
                        let mid = MsgId(
                            full_id
                                .msg
                                .0
                                .clamp(i64::from(i32::MIN), i64::from(i32::MAX))
                                as i32,
                        );
                        session.show_peer_history(
                            PeerId(full_id.peer.0),
                            SectionShow::clear_stack(),
                            mid,
                        );
                    }
                }
            }
        }

        // Hover chat preview, Ctrl+Tab chat-switch overlay, and separate chat viewports.
        if self.root_screen == RootScreen::Main {
            let dialogs = self.session_backend.dialogs(FilterId::ALL, "");
            if let Some(peer) =
                self.shell_extras
                    .show_overlays(&ctx, screen_rect, &palette, &dialogs)
                && let Some(session) = self.session.as_mut()
            {
                session.show_peer_history(peer, SectionShow::clear_stack(), MsgId::SHOW_AT_END);
            }
            let chat_open = self
                .session
                .as_ref()
                .is_some_and(|s| !s.active_peer().is_zero());
            let extras = &mut self.shell_extras;
            if let Some(toast) = extras
                .restore
                .show(&ctx, &mut extras.saved_windows, chat_open)
            {
                self.show_toast(toast);
            }
        }

        // Flush debounced settings writes (`kSaveWindowPositionTimeout`).
        if let Some(since) = self.settings_dirty_since
            && tg_app_core::clock::now().saturating_duration_since(since)
                >= SAVE_WINDOW_POSITION_TIMEOUT
        {
            self.flush_settings();
        }
    }

    /// Processes Telegram Desktop global keyboard shortcuts (`core/shortcuts.cpp`)
    /// driven by [`tg_app_settings::settings::shortcuts::ShortcutManager`].
    fn handle_global_shortcuts(&mut self, ctx: &egui::Context, now_ms: i64) {
        crate::window::shortcuts_wiring::dispatch_global_shortcuts(self, ctx, now_ms);
    }

    pub(crate) fn is_recording_shortcut(&self) -> bool {
        self.main_widget
            .settings()
            .is_some_and(|s| s.shortcuts.recording.is_some())
    }

    pub(crate) fn execute_close_command(&mut self, ctx: &egui::Context) {
        if self.media_overlay.is_open() {
            self.media_overlay.close();
        } else if !self.box_host.is_empty() {
            self.box_host.pop(ctx, &self.theme_manager);
        } else if self.layer_stack.close_top_by_back() {
            // Closed top layer.
        } else {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }

    pub(crate) fn execute_lock_command(&mut self) {
        if self.has_local_passcode() {
            self.setup_passcode_lock();
        } else if self.session_backend.is_live_session() {
            let settings_backend = self.session_backend.create_settings_backend();
            self.main_widget.open_settings_section(
                SettingsSection::LocalPasscode,
                settings_backend,
                self.theme_manager.embedded_type(),
                self.theme_manager.accent(),
            );
            if let Some(session) = self.session.as_mut() {
                session.show_settings(SettingsPage::Main);
            }
        } else {
            self.layer_stack
                .show_box(crate::window::shortcuts_wiring::build_default_lock_modal());
        }
    }

    pub(crate) fn execute_escape_command(&mut self, ctx: &egui::Context) {
        if self.media_overlay.is_open() || !self.box_host.is_empty() {
            return;
        }
        if self.layer_stack.close_top_by_back() {
            return;
        }
        let now_secs = ctx.input(|i| i.time);
        if let Some(session) = self.session.as_mut() {
            if self.main_widget.close_overlays_by_escape(session, now_secs) {
                return;
            }
            session.show_back_from_stack(SectionShow::forward());
        }
    }

    pub(crate) const fn is_media_overlay_open(&self) -> bool {
        self.media_overlay.is_open()
    }

    pub(crate) fn has_blocking_modal_open(&self) -> bool {
        !self.box_host.is_empty() || self.layer_stack.is_layer_shown()
    }

    pub(crate) fn execute_back_navigation(&mut self) {
        if let Some(session) = self.session.as_mut()
            && !self.main_widget.go_back_in_active_section(session)
        {
            session.show_back_from_stack(SectionShow::forward());
        }
    }

    pub(crate) fn handle_chat_switch_tab(
        &mut self,
        ctx: &egui::Context,
        dialogs: &[tg_app_core::stand_in::types::DialogEntry],
        active_peer: PeerId,
    ) -> bool {
        self.shell_extras
            .handle_shortcut_keys(ctx, dialogs, active_peer)
    }

    pub(crate) fn dispatch_dialogs_shortcut_action(
        &mut self,
        shortcut: DialogsShortcut,
        ctx: &egui::Context,
        now_ms: i64,
    ) {
        self.dispatch_dialogs_shortcut(shortcut, ctx, now_ms);
    }

    pub(crate) fn push_box_kind(&mut self, ctx: &egui::Context, kind: BoxKind) {
        self.box_host.push(ctx, kind, &self.theme_manager);
    }

    pub(crate) fn open_peer_history_from_shortcut(&mut self, peer_id: PeerId) {
        if let Some(session) = self.session.as_mut() {
            session.show_peer_history(peer_id, SectionShow::clear_stack(), MsgId::SHOW_AT_UNREAD);
        }
    }

    pub(crate) fn request_switch_account_by_index(&mut self, idx: usize) -> bool {
        if let Some(item) = self.menu_accounts.get(idx) {
            self.pending_account_switch = Some(item.slot_index);
            true
        } else {
            false
        }
    }

    /// Starts restoring saved separate chat windows from `<working_dir>/tdata` (WP-40).
    pub fn install_restore_windows(&mut self, working_dir: &std::path::Path) {
        let extras = &mut self.shell_extras;
        extras
            .restore
            .install(working_dir, &mut extras.saved_windows);
    }

    /// Mutable access to the separate-window shell extras (chat preview, saved windows, restore).
    pub fn shell_extras_mut(&mut self) -> &mut WindowShellExtras {
        &mut self.shell_extras
    }

    /// Programmatically triggers a [`DialogsShortcut`] (used by keyboard shortcuts and unit tests).
    pub fn trigger_dialogs_shortcut(&mut self, shortcut: DialogsShortcut) {
        let now_ms = self.elapsed_ms();
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let emitted = self.main_widget.apply_dialogs_shortcut(
            shortcut,
            session,
            &mut *self.session_backend,
            now_ms,
        );
        for act in emitted {
            if let MainWidgetAction::Toast(msg) = act {
                self.layer_stack.show_toast(msg);
            } else if let MainWidgetAction::OpenMiniApp {
                bot_id,
                url,
                start_param,
                confirm_open,
            } = act
            {
                self.open_mini_app_from_chat(bot_id, url, start_param, confirm_open);
            }
        }
    }

    fn dispatch_dialogs_shortcut(
        &mut self,
        shortcut: DialogsShortcut,
        ctx: &egui::Context,
        now_ms: i64,
    ) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let emitted = self.main_widget.apply_dialogs_shortcut(
            shortcut,
            session,
            &mut *self.session_backend,
            now_ms,
        );
        for act in emitted {
            self.handle_main_widget_action(act, ctx);
        }
    }

    fn handle_main_widget_action(&mut self, act: MainWidgetAction, ctx: &egui::Context) {
        match act {
            MainWidgetAction::OpenMainMenu => self.layer_stack.open_main_menu(),
            MainWidgetAction::OpenProxySettings => {
                self.layer_stack.show_box(ModalBox::ProxySettings);
            }
            MainWidgetAction::RetryConnection => {
                self.session_backend.retry_connection();
                if self.session_backend.is_live_session() {
                    self.layer_stack.show_toast("Retrying connection...");
                } else {
                    self.layer_stack.show_toast("Reconnected (mock)");
                }
            }
            MainWidgetAction::ToggleTheme => self.toggle_night_mode(),
            MainWidgetAction::ApplyTheme { kind, accent } => self.apply_theme(kind, accent),
            MainWidgetAction::ApplyLanguage(code) => {
                let _ = InfoBackend::set_language(&mut *self.session_backend, &code);
                self.strings.refresh();
                self.layer_stack
                    .show_toast(format!("Language switched to '{code}'"));
            }
            MainWidgetAction::ToggleFoldersSidebar => {
                self.settings.folders_sidebar_enabled = !self.settings.folders_sidebar_enabled;
                if let Some(session) = self.session.as_mut() {
                    session.set_filters_enabled(self.settings.folders_sidebar_enabled);
                }
                self.mark_settings_dirty();
            }
            MainWidgetAction::OpenPasscodeSetup => {
                self.layer_stack.show_box(ModalBox::PasscodeSetup {
                    draft: String::new(),
                    error: String::new(),
                    has_existing: self.settings.local_passcode.is_some(),
                });
            }
            MainWidgetAction::LockNow => {
                self.setup_passcode_lock();
            }
            MainWidgetAction::RequestLogout => {
                self.layer_stack.show_box(ModalBox::ConfirmLogout);
            }
            MainWidgetAction::OpenHistoryMedia {
                peer_id,
                msg_id,
                album_index,
            } => {
                self.open_history_media(peer_id, msg_id, album_index);
            }
            MainWidgetAction::OpenLink(url) => self.open_link(&url),
            MainWidgetAction::OpenHiddenLink { url, force_confirm } => {
                self.open_link_inner(&url, true, force_confirm);
            }
            MainWidgetAction::OpenMiniApp {
                bot_id,
                url,
                start_param,
                confirm_open,
            } => self.open_mini_app_from_chat(bot_id, url, start_param, confirm_open),
            MainWidgetAction::SwitchInlineChooseChat {
                bot_id,
                bot_username,
                query,
                peer_types,
            } => {
                self.box_host.push(
                    ctx,
                    BoxKind::PeerList(PeerListBoxState::new(
                        "Choose Chat",
                        "Select",
                        PeerSelectionMode::Single,
                        PeerListPurpose::SwitchInline {
                            bot_id,
                            bot_username,
                            query,
                            peer_types,
                        },
                    )),
                    &self.theme_manager,
                );
            }
            MainWidgetAction::SwitchInlineToPeer {
                target_peer,
                bot_id,
                bot_username,
                query,
            } => {
                self.open_peer_with_inline_query(target_peer, bot_id, &bot_username, &query);
            }
            MainWidgetAction::OpenModerateMessages {
                peer_id,
                peer_title,
                message_ids,
                participants,
            } => {
                let state = tg_app_info::boxes::click_handlers::build_moderate_box_state(
                    peer_id,
                    peer_title,
                    message_ids,
                    participants,
                    &*self.session_backend,
                );
                self.box_host
                    .push(ctx, BoxKind::ModerateMessages(state), &self.theme_manager);
            }
            MainWidgetAction::OpenMusicAttach(peer_id) => {
                let owner_id = self.session_backend.saved_messages_peer_id();
                let state = tg_app_info::boxes::bot_info_extras::MusicAttachBoxState::new(
                    peer_id, owner_id,
                );
                self.box_host
                    .push(ctx, BoxKind::MusicAttach(state), &self.theme_manager);
            }
            MainWidgetAction::InfoEvent(ev) => self.handle_info_wrap_event(ev, ctx),
            MainWidgetAction::OpenPayments { screen, slug } => {
                self.pending_overlay_requests
                    .push(DeepLinkOverlayRequest::OpenPayments { screen, slug });
            }
            MainWidgetAction::OpenBusiness => {
                self.pending_overlay_requests
                    .push(DeepLinkOverlayRequest::OpenBusiness);
            }
            MainWidgetAction::OpenEditSelfColor => {
                let (self_peer_id, self_name) =
                    tg_app_history::history::backend::HistoryBackend::self_peer(
                        &*self.session_backend,
                    );
                let settings =
                    InfoBackend::peer_color_settings(&*self.session_backend, self_peer_id);
                self.box_host.push(
                    ctx,
                    BoxKind::EditPeerColor(EditPeerColorBoxState::new(
                        self_peer_id,
                        self_name,
                        settings,
                    )),
                    &self.theme_manager,
                );
            }
            MainWidgetAction::OpenStory { peer_id, story_id } => {
                self.pending_overlay_requests
                    .push(DeepLinkOverlayRequest::OpenStory {
                        peer_id,
                        story_id: Some(story_id),
                    });
            }
            MainWidgetAction::OpenGiveawayInfo {
                peer_id,
                msg_id,
                channel_id,
                launch_msg_id,
            } => {
                let target_peer = channel_id.unwrap_or(peer_id);
                let target_msg = launch_msg_id.unwrap_or(msg_id);
                self.pending_overlay_requests
                    .push(DeepLinkOverlayRequest::OpenPayments {
                        screen: tg_app_info::payments::PaymentsScreen::BoostsGiveaways,
                        slug: Some(format!("giveaway:{}:{}", target_peer.0, target_msg.0)),
                    });
            }
            MainWidgetAction::OpenVideoStream {
                peer_id,
                msg_id,
                call_id,
                access_hash,
                rtmp_stream,
                title,
            } => {
                let now_ms = self.elapsed_ms();
                self.media_overlay.open_video_stream(
                    tg_app_media::media::VideoStreamDescriptor {
                        call_id,
                        access_hash,
                        peer_id: peer_id.0,
                        title,
                        call_link_slug: String::new(),
                        join_msg_id: msg_id.0,
                        rtmp_stream,
                        stream_url: None,
                        stream_dc_id: 0,
                        is_live_story: false,
                    },
                    now_ms,
                );
            }
            MainWidgetAction::Toast(msg) => {
                self.layer_stack.show_toast(msg);
            }
            MainWidgetAction::OpenForwardBox { from_peer, msg_ids } => {
                let ids = msg_ids.iter().map(|id| id.0).collect();
                let tabs = self.forward_folder_tabs();
                self.box_host.push(
                    ctx,
                    BoxKind::Share(ShareBoxState::for_forward(from_peer, ids, tabs)),
                    &self.theme_manager,
                );
            }
            MainWidgetAction::OpenInNewWindow(peer_id) => {
                let title = self
                    .session_backend
                    .dialogs(FilterId::ALL, "")
                    .into_iter()
                    .find(|d| d.peer.id == peer_id)
                    .map_or_else(|| format!("Chat {}", peer_id.0), |d| d.peer.title);
                self.shell_extras.saved_windows.open_window(
                    crate::window::chat_preview::SavedChatWindow::new(peer_id, title),
                );
            }
            MainWidgetAction::PersistSettings => {
                if let Some(session) = &self.session {
                    self.settings.columns = session.columns().saved();
                }
                self.mark_settings_dirty();
            }
        }
    }

    fn handle_info_wrap_event(&mut self, ev: InfoWrapEvent, ctx: &egui::Context) {
        match ev {
            InfoWrapEvent::CloseOrBack => {
                if let Some(session) = self.session.as_mut() {
                    session.show_back_from_stack(SectionShow::forward());
                }
            }
            InfoWrapEvent::OpenHistory(pid) => {
                if let Some(session) = self.session.as_mut() {
                    session.show_peer_history(pid, SectionShow::forward(), MsgId::SHOW_AT_UNREAD);
                }
            }
            InfoWrapEvent::Toast(msg) => self.layer_stack.show_toast(msg),
            InfoWrapEvent::OpenStatistics(pid) => self.open_statistics_for_peer(pid, false),
            InfoWrapEvent::OpenBoosts(pid) => self.open_statistics_for_peer(pid, true),
            InfoWrapEvent::OpenBotMiniApp(pid) => self.open_bot_mini_app(pid),
            InfoWrapEvent::StartCall { peer_id, video } => {
                self.pending_start_call = Some((peer_id, video));
            }
            InfoWrapEvent::OpenSendGift(_) => {
                self.pending_overlay_requests
                    .push(DeepLinkOverlayRequest::OpenPayments {
                        screen: tg_app_info::payments::PaymentsScreen::StarGifts,
                        slug: None,
                    });
            }
            InfoWrapEvent::OpenMedia {
                peer_id,
                item_id,
                tab,
            } => self.open_shared_media(peer_id, item_id, tab),
            InfoWrapEvent::OpenEditPeer(pid) => {
                if let Some(peer) = InfoBackend::peer_profile(&*self.session_backend, pid) {
                    self.box_host.push(
                        ctx,
                        BoxKind::EditPeerInfo(EditPeerInfoBoxState::from_profile(&peer)),
                        &self.theme_manager,
                    );
                }
            }
            InfoWrapEvent::OpenEditUsername(pid) => {
                if let Some(peer) = InfoBackend::peer_profile(&*self.session_backend, pid) {
                    self.box_host.push(
                        ctx,
                        BoxKind::Username(UsernameBoxState::from_profile(&peer)),
                        &self.theme_manager,
                    );
                }
            }
            InfoWrapEvent::OpenPermissions(pid) => {
                if let Some(peer) = InfoBackend::peer_profile(&*self.session_backend, pid) {
                    self.box_host.push(
                        ctx,
                        BoxKind::EditPermissions(EditPermissionsBoxState::from_profile(&peer)),
                        &self.theme_manager,
                    );
                }
            }
            InfoWrapEvent::OpenAdminRights { peer_id, user_id } => {
                if let Some(uid) = user_id {
                    if let Some(peer) = InfoBackend::peer_profile(&*self.session_backend, peer_id)
                        && let Some(member) = self
                            .session_backend
                            .peer_members(peer_id, "")
                            .into_iter()
                            .find(|m| m.peer_id == uid)
                    {
                        self.box_host.push(
                            ctx,
                            BoxKind::EditAdminRights(EditAdminRightsBoxState::from_member(
                                &peer, &member,
                            )),
                            &self.theme_manager,
                        );
                    }
                } else {
                    self.box_host.push(
                        ctx,
                        BoxKind::AdminsList(AdminsListBoxState::new(peer_id)),
                        &self.theme_manager,
                    );
                }
            }
            InfoWrapEvent::OpenEditMemberTag { peer_id, user_id } => {
                if let Some(peer) = InfoBackend::peer_profile(&*self.session_backend, peer_id)
                    && let Some(member) = self
                        .session_backend
                        .peer_members(peer_id, "")
                        .into_iter()
                        .find(|m| m.peer_id == user_id)
                {
                    let current_rank = self.session_backend.member_rank(peer_id, user_id);
                    let is_self =
                        tg_app_info::dialogs::backend::DialogsBackend::saved_messages_peer_id(
                            &*self.session_backend,
                        ) == user_id;
                    self.box_host.push(
                        ctx,
                        BoxKind::EditPeerTag(
                            tg_app_info::boxes::peer_tags::EditPeerTagBoxState::from_member(
                                &peer,
                                &member,
                                current_rank.as_deref(),
                                is_self,
                            ),
                        ),
                        &self.theme_manager,
                    );
                }
            }
            InfoWrapEvent::OpenAddMembers(group_id) => {
                let existing: Vec<PeerId> = self
                    .session_backend
                    .peer_members(group_id, "")
                    .into_iter()
                    .map(|m| m.peer_id)
                    .collect();
                self.box_host.push(
                    ctx,
                    BoxKind::PeerList(
                        PeerListBoxState::new(
                            "Add Members",
                            "Add",
                            PeerSelectionMode::Multi,
                            PeerListPurpose::AddMembers { group_id },
                        )
                        .with_excluded(existing),
                    ),
                    &self.theme_manager,
                );
            }
            InfoWrapEvent::OpenAddContact => {
                self.box_host.push(
                    ctx,
                    BoxKind::AddContact(AddContactBoxState::new()),
                    &self.theme_manager,
                );
            }
            InfoWrapEvent::OpenShare {
                message_ids,
                share_link,
            } => {
                self.box_host.push(
                    ctx,
                    BoxKind::Share(ShareBoxState::new(message_ids, share_link)),
                    &self.theme_manager,
                );
            }
            InfoWrapEvent::OpenBlockUser { peer_id, name } => {
                let suggest_report =
                    tg_app_core::dialogs::backend::DialogsBackend::dialog_suggests_report_spam(
                        &*self.session_backend,
                        peer_id,
                    );
                self.box_host.push(
                    ctx,
                    BoxKind::BlockUser(tg_app_info::boxes::block_user::BlockUserBoxState::new(
                        peer_id,
                        name,
                        suggest_report,
                    )),
                    &self.theme_manager,
                );
            }
            InfoWrapEvent::RestartBot(bot_id) => self.restart_bot(bot_id),
            InfoWrapEvent::JoinGroupCall(peer_id) => {
                self.pending_overlay_requests
                    .push(DeepLinkOverlayRequest::JoinCall {
                        peer_id: Some(peer_id),
                        slug_or_hash: None,
                    });
            }
            InfoWrapEvent::OpenMuteFor(peer_id) => {
                self.box_host.push(
                    ctx,
                    BoxKind::MuteFor(tg_app_info::boxes::mute::MuteForBoxState::new(peer_id)),
                    &self.theme_manager,
                );
            }
            InfoWrapEvent::OpenSelectTone(peer_id) => {
                let silent = tg_app_core::dialogs::backend::DialogsBackend::dialog_notify_sound(
                    &*self.session_backend,
                    peer_id,
                )
                .none;
                self.box_host.push(
                    ctx,
                    BoxKind::SelectTone(tg_app_info::boxes::mute::SelectToneBoxState::new(
                        peer_id, silent,
                    )),
                    &self.theme_manager,
                );
            }
            InfoWrapEvent::OpenReport(pid) => {
                let title = InfoBackend::peer_profile(&*self.session_backend, pid)
                    .map_or_else(|| "Chat".to_owned(), |p| p.title);
                self.box_host.push(
                    ctx,
                    BoxKind::Report(ReportBoxState::new(pid, title)),
                    &self.theme_manager,
                );
            }
            InfoWrapEvent::OpenDelete {
                peer_id,
                message_ids,
            } => {
                if let Some(peer) = InfoBackend::peer_profile(&*self.session_backend, peer_id) {
                    let state = if message_ids.is_empty() {
                        let future_creator = if peer.kind.is_group() || peer.kind.is_channel() {
                            tg_app_core::history::backend::HistoryBackend::future_chat_creator_after_leave(
                                &*self.session_backend,
                                peer_id,
                            )
                            .ok()
                            .flatten()
                        } else {
                            None
                        };
                        DeleteMessagesBoxState::for_delete_or_leave(&peer, false)
                            .with_future_creator(future_creator)
                    } else {
                        DeleteMessagesBoxState::for_messages(&peer, message_ids)
                    };
                    self.box_host
                        .push(ctx, BoxKind::DeleteMessages(state), &self.theme_manager);
                }
            }
            InfoWrapEvent::OpenPhotoCrop(pid) => {
                self.box_host.push(
                    ctx,
                    BoxKind::PhotoCrop {
                        state: Box::new(PhotoCropBoxState::sample_profile_photo(
                            "Update Profile Photo",
                            [0x40, 0xA7, 0xE3],
                        )),
                        target_peer: Some(pid),
                    },
                    &self.theme_manager,
                );
            }
            InfoWrapEvent::OpenAutoDelete(pid) => {
                let ttl = InfoBackend::peer_profile(&*self.session_backend, pid)
                    .map_or(0, |p| p.auto_delete_ttl_seconds);
                self.box_host.push(
                    ctx,
                    BoxKind::AutoDeleteTimer(AutoDeleteTimerBoxState::new(pid, ttl)),
                    &self.theme_manager,
                );
            }
            InfoWrapEvent::OpenLinkedDiscussionPicker(channel_id) => {
                let current = InfoBackend::peer_profile(&*self.session_backend, channel_id)
                    .and_then(|p| p.linked_discussion_peer.map(|(id, _)| id))
                    .into_iter()
                    .collect();
                self.box_host.push(
                    ctx,
                    BoxKind::PeerList(
                        PeerListBoxState::new(
                            "Link Discussion Group",
                            "Link",
                            PeerSelectionMode::Single,
                            PeerListPurpose::LinkDiscussion { channel_id },
                        )
                        .with_selected(current),
                    ),
                    &self.theme_manager,
                );
            }
            InfoWrapEvent::OpenCreateGroup => {
                self.box_host.push(
                    ctx,
                    BoxKind::CreateGroup(CreateGroupBoxState::new()),
                    &self.theme_manager,
                );
            }
            InfoWrapEvent::OpenCreateChannel => {
                self.box_host.push(
                    ctx,
                    BoxKind::CreateChannel(CreateChannelBoxState::new()),
                    &self.theme_manager,
                );
            }
            InfoWrapEvent::OpenAbout => {
                self.box_host.push(
                    ctx,
                    BoxKind::About(AboutBoxState::new()),
                    &self.theme_manager,
                );
            }
            InfoWrapEvent::OpenLanguage => {
                let lang = InfoBackend::active_language(&*self.session_backend);
                self.box_host.push(
                    ctx,
                    BoxKind::Language(LanguageBoxState::new(lang)),
                    &self.theme_manager,
                );
            }
            InfoWrapEvent::OpenTerms => {
                let tos = self.session_backend.terms_of_service();
                self.box_host.push(
                    ctx,
                    BoxKind::TermsOfService(TermsOfServiceBoxState::new(tos)),
                    &self.theme_manager,
                );
            }
            InfoWrapEvent::OpenPeerQr(pid) => {
                if let Some(peer) = InfoBackend::peer_profile(&*self.session_backend, pid) {
                    self.box_host.push(
                        ctx,
                        BoxKind::PeerQr(
                            tg_app_info::boxes::peer_qr::PeerQrBoxState::from_profile_with_backend(
                                &peer,
                                &*self.session_backend,
                            ),
                        ),
                        &self.theme_manager,
                    );
                }
            }
            InfoWrapEvent::OpenEmojiUserpic(pid) => {
                let peer = InfoBackend::peer_profile(&*self.session_backend, pid);
                let title = peer
                    .as_ref()
                    .map_or_else(|| "Profile".to_owned(), |p| p.title.clone());
                let catalog_kind = if peer
                    .as_ref()
                    .is_some_and(|p| p.kind.is_group() || p.kind.is_channel())
                {
                    tg_app_core::chat_helpers::backend::DefaultEmojiCatalogKind::GroupPhoto
                } else {
                    tg_app_core::chat_helpers::backend::DefaultEmojiCatalogKind::ProfilePhoto
                };
                self.box_host.push(
                    ctx,
                    BoxKind::EmojiUserpic(
                        tg_app_info::boxes::emoji_userpic::EmojiUserpicBoxState::with_default_catalog(
                            pid,
                            title,
                            catalog_kind,
                            &*self.session_backend,
                        ),
                    ),
                    &self.theme_manager,
                );
            }
            InfoWrapEvent::OpenPeerReactions(pid) => {
                if let Some(peer) = InfoBackend::peer_profile(&*self.session_backend, pid) {
                    let cfg = self.session_backend.peer_available_reactions(pid);
                    self.box_host.push(
                        ctx,
                        BoxKind::EditPeerReactions(
                            tg_app_info::boxes::peer_reactions::EditPeerReactionsBoxState::new(
                                pid,
                                peer.title,
                                peer.kind.is_channel(),
                                cfg,
                            ),
                        ),
                        &self.theme_manager,
                    );
                }
            }
            InfoWrapEvent::OpenVerifyPeers(bot_id) => {
                let title = InfoBackend::peer_profile(&*self.session_backend, bot_id)
                    .map_or_else(|| "Bot".to_owned(), |p| p.title);
                self.box_host.push(
                    ctx,
                    BoxKind::VerifyPeers(
                        tg_app_info::boxes::verify_peers::VerifyPeersBoxState::new(bot_id, title),
                    ),
                    &self.theme_manager,
                );
            }
            InfoWrapEvent::OpenProfileStory { peer_id, story_id } => {
                self.layer_stack.show_toast(format!(
                    "Opening story #{story_id} for peer {}",
                    peer_id.bare()
                ));
            }
            InfoWrapEvent::OpenCollectibleInfo { peer_id, entity } => {
                match InfoBackend::get_collectible_info(&*self.session_backend, peer_id, &entity) {
                    Ok(info) => {
                        self.box_host.push(
                            ctx,
                            BoxKind::CollectibleInfo(
                                tg_app_info::boxes::collectible_info::CollectibleInfoBoxState::new(
                                    info,
                                ),
                            ),
                            &self.theme_manager,
                        );
                    }
                    Err(err) => {
                        self.layer_stack.show_toast(err);
                    }
                }
            }
            InfoWrapEvent::ExportChatHistory(pid) => {
                if let Some((input_peer, title)) = self.session_backend.resolve_export_peer(pid) {
                    tg_app_settings::export::AppExportHost::open_single_chat_export(
                        ctx,
                        self.theme_mode(),
                        input_peer,
                        title,
                    );
                } else {
                    self.layer_stack
                        .show_toast("Cannot export chat history: peer unavailable.");
                }
            }
            InfoWrapEvent::ImportChatHistory(pid) => {
                let state = match InfoBackend::peer_profile(&*self.session_backend, pid) {
                    Some(peer) => tg_app_info::boxes::chat_import::ChatImportBoxState::for_peer(
                        pid, peer.title,
                    ),
                    None => tg_app_info::boxes::chat_import::ChatImportBoxState::new(),
                };
                self.box_host
                    .push(ctx, BoxKind::ChatImport(state), &self.theme_manager);
            }
            InfoWrapEvent::OpenEditBotInfo(bot_id) => {
                let username = InfoBackend::peer_profile(&*self.session_backend, bot_id)
                    .and_then(|p| p.primary_username().map(ToOwned::to_owned))
                    .unwrap_or_default();
                self.box_host.push(
                    ctx,
                    BoxKind::EditBotInfo(
                        tg_app_info::boxes::managed_bot::EditBotInfoBoxState::new(bot_id, username),
                    ),
                    &self.theme_manager,
                );
            }
            InfoWrapEvent::OpenBalanceDetails { peer_id: _, kind } => {
                let label = match kind {
                    tg_app_info::info::balance_pill::BalancePillClick::Ton => "TON profile balance",
                    tg_app_info::info::balance_pill::BalancePillClick::Stars => {
                        "Telegram Stars balance"
                    }
                };
                self.layer_stack.show_toast(format!("Opening {label}"));
            }
            InfoWrapEvent::OpenAttachedStickers { media, media_label } => {
                self.open_attached_stickers_modal(ctx, &media, &media_label);
            }
        }
    }

    /// Queries `messages.getAttachedStickers` via [`ChatHelpersBackend::get_attached_stickers`]
    /// and pushes either `StickerSetBox` (single set), `AttachedStickersBox` (multiple sets),
    /// or shows a `"Sticker set not found."` toast (`api/api_attached_stickers.cpp`).
    pub fn open_attached_stickers_modal(
        &mut self,
        ctx: &egui::Context,
        media: &tg_app_core::chat_helpers::backend::AttachedStickersMedia,
        media_label: &str,
    ) {
        use tg_app_info::boxes::attached_stickers::{
            AttachedStickersOutcome, classify_attached_stickers,
        };
        let sets = match self.session_backend.get_attached_stickers(media) {
            Ok(sets) => sets,
            Err(err) => {
                self.layer_stack
                    .show_toast(format!("Could not load attached stickers: {err}"));
                return;
            }
        };
        let outcome = classify_attached_stickers(sets, media_label, |only| {
            self.session_backend
                .resolve_sticker_set(&only.short_name, only.is_emoji)
                .ok()
        });
        match outcome {
            AttachedStickersOutcome::NotFound => {
                self.layer_stack
                    .show_toast(tg_lang::tr::lng_stickers_not_found());
            }
            AttachedStickersOutcome::SingleSet(st_box) => {
                self.box_host
                    .push(ctx, BoxKind::StickerSet(st_box), &self.theme_manager);
            }
            AttachedStickersOutcome::MultipleSets(list_box) => {
                self.box_host.push(
                    ctx,
                    BoxKind::AttachedStickers(list_box),
                    &self.theme_manager,
                );
            }
        }
    }

    fn handle_box_event(&mut self, ev: BoxEvent, ctx: &egui::Context) {
        match ev {
            BoxEvent::Toast(msg) => self.layer_stack.show_toast(msg),
            BoxEvent::PeerCreated(pid) => {
                if let Some(session) = self.session.as_mut() {
                    session.show_peer_history(
                        pid,
                        SectionShow::clear_stack(),
                        MsgId::SHOW_AT_UNREAD,
                    );
                }
            }
            BoxEvent::PeerUpdated(_)
            | BoxEvent::PhotoCropped { .. }
            | BoxEvent::Forwarded { .. } => {}
            BoxEvent::ChatHistoryImported { peer_id, .. } => {
                self.main_widget
                    .notify_external_message(peer_id, &*self.session_backend);
            }
            BoxEvent::ForwardMediaItem {
                item_id,
                targets,
                comment,
            } => {
                let mut forwarded = 0usize;
                let mut first_err: Option<String> = None;
                let trimmed_comment = comment.trim().to_owned();
                for target in targets {
                    match tg_app_media::media::backend::MediaBackend::forward_item_to(
                        &*self.session_backend,
                        item_id,
                        target.0,
                    ) {
                        Ok(()) => {
                            forwarded += 1;
                            if !trimmed_comment.is_empty() {
                                let _ = tg_app_history::history::backend::HistoryBackend::send_message(
                                    &mut *self.session_backend,
                                    target,
                                    tg_app_history::history::backend::SendMessageRequest {
                                        text: tg_text::TextWithEntities::simple(
                                            trimmed_comment.clone(),
                                        ),
                                        reply_to: None,
                                        silent: false,
                                    },
                                );
                            }
                            self.main_widget
                                .notify_external_message(target, &*self.session_backend);
                        }
                        Err(err) => {
                            if first_err.is_none() {
                                first_err = Some(err.to_string());
                            }
                        }
                    }
                }
                if forwarded > 0 {
                    self.layer_stack.show_toast(
                        tg_lang::tr::lng_share_messages_to_many_chats(forwarded as i64)
                            .replace("**", ""),
                    );
                } else if let Some(err) = first_err {
                    self.layer_stack
                        .show_toast(format!("Could not forward media: {err}"));
                }
            }
            BoxEvent::Deleted { peer_id, count, .. } => {
                if count == 0
                    && let Some(session) = self.session.as_mut()
                    && session.active_peer() == peer_id
                {
                    session.clear_section_stack(SectionShow::clear_stack());
                } else {
                    self.main_widget
                        .notify_external_message(peer_id, &*self.session_backend);
                }
            }
            BoxEvent::BlockUserConfirmed(choice) => self.block_user_confirmed(&choice),
            BoxEvent::MuteDialogFor { peer_id, seconds } => {
                if let Err(err) = tg_app_core::dialogs::backend::DialogsBackend::mute_dialog_for(
                    &mut *self.session_backend,
                    peer_id,
                    seconds,
                ) {
                    self.layer_stack.show_toast(err);
                }
            }
            BoxEvent::DialogToneChosen { peer_id, silent } => {
                if let Err(err) =
                    tg_app_core::dialogs::backend::DialogsBackend::set_dialog_sound_enabled(
                        &mut *self.session_backend,
                        peer_id,
                        !silent,
                    )
                {
                    self.layer_stack.show_toast(err);
                }
            }
            BoxEvent::LanguageChanged(code) => {
                self.strings.refresh();
                self.layer_stack
                    .show_toast(format!("Language switched to '{code}'"));
            }
            BoxEvent::Confirmed(tag, _) => match tag {
                ConfirmTag::ApplyProxy(proxy) => {
                    let result = if let Some(settings) = self.main_widget.settings_mut() {
                        if let Some(existing) =
                            tg_app_settings::settings::backend::SettingsBackend::proxy_settings(
                                &*self.session_backend,
                            )
                        {
                            settings.core_settings.proxy = existing;
                        }
                        tg_app_info::boxes::add_proxy::apply_confirmed_proxy(
                            proxy,
                            &mut settings.core_settings,
                            &mut *self.session_backend,
                        )
                    } else {
                        let mut core = tg_core::CoreSettings::new();
                        if let Some(existing) =
                            tg_app_settings::settings::backend::SettingsBackend::proxy_settings(
                                &*self.session_backend,
                            )
                        {
                            core.proxy = existing;
                        }
                        tg_app_info::boxes::add_proxy::apply_confirmed_proxy(
                            proxy,
                            &mut core,
                            &mut *self.session_backend,
                        )
                    };
                    if let Err(err) = result {
                        self.layer_stack
                            .show_toast(format!("Could not connect proxy: {err}"));
                    }
                }
                ConfirmTag::JoinChatlist { slug, peers } => {
                    match self.session_backend.join_chatlist_invite(&slug, &peers) {
                        Ok(count) => self
                            .layer_stack
                            .show_toast(format!("Added folder with {count} chat(s)")),
                        Err(err) => self
                            .layer_stack
                            .show_toast(format!("Failed to add folder: {err}")),
                    }
                }
                ConfirmTag::ApplyLanguage(code) => {
                    if let Err(err) = InfoBackend::set_language(&mut *self.session_backend, &code) {
                        self.layer_stack
                            .show_toast(format!("Could not switch language: {err}"));
                    } else {
                        self.strings.refresh();
                        self.layer_stack
                            .show_toast(format!("Language switched to '{code}'"));
                    }
                }
                ConfirmTag::ApplyGiftCode { slug, days } => {
                    match self.session_backend.apply_gift_code(&slug) {
                        Ok(()) => self
                            .layer_stack
                            .show_toast(format!("Redeemed {days}-day Telegram Premium gift code!")),
                        Err(err) => self
                            .layer_stack
                            .show_toast(format!("Could not redeem gift code: {err}")),
                    }
                }
                ConfirmTag::Custom(_)
                | ConfirmTag::DeclineTerms
                | ConfirmTag::LeavePeer(_)
                | ConfirmTag::ConvertToGigagroup(_)
                | ConfirmTag::DeactivateAllUsernames(_) => {}
            },
            BoxEvent::JoinInviteConfirmed { hash } => {
                match self.session_backend.import_chat_invite(&hash) {
                    Ok(peer_id) => {
                        if let Some(session) = self.session.as_mut() {
                            session.show_peer_history(
                                peer_id,
                                SectionShow::clear_stack(),
                                MsgId::SHOW_AT_UNREAD,
                            );
                        }
                        self.layer_stack.show_toast("Joined chat");
                    }
                    Err(err) => self
                        .layer_stack
                        .show_toast(format!("Could not join chat: {err}")),
                }
            }
            BoxEvent::TermsResolved { .. } => {}
            BoxEvent::SwitchInlinePeerChosen {
                target_peer,
                bot_id,
                bot_username,
                query,
            } => {
                self.open_peer_with_inline_query(target_peer, bot_id, &bot_username, &query);
            }
            BoxEvent::CalendarJumpToMessage {
                peer_id, msg_id, ..
            } => {
                if let Some(session) = self.session.as_mut() {
                    session.show_peer_history(
                        peer_id,
                        SectionShow::clear_stack(),
                        if msg_id.0 > 0 {
                            msg_id
                        } else {
                            MsgId::SHOW_AT_END
                        },
                    );
                }
            }
            BoxEvent::StickerSetToggleInstall {
                set_id,
                short_name,
                title,
                is_emoji,
                install,
            } => {
                let kind = if is_emoji {
                    "custom emoji pack"
                } else {
                    "sticker pack"
                };
                let res = if install {
                    self.session_backend
                        .install_sticker_set_preview(set_id, &short_name)
                } else {
                    self.session_backend
                        .remove_sticker_set_preview(set_id, &short_name)
                };
                match res {
                    Ok(true) => self.layer_stack.show_toast(if install {
                        format!("Added {kind} \"{title}\"")
                    } else {
                        format!("Removed {kind} \"{title}\"")
                    }),
                    Ok(false) => self
                        .layer_stack
                        .show_toast(format!("No change for {kind} \"{title}\"")),
                    Err(err) => self
                        .layer_stack
                        .show_toast(format!("Could not update {kind} \"{title}\": {err}")),
                }
            }
            BoxEvent::StickerSetOwnerAction(act) => match act {
                tg_app_info::boxes::sticker_set::StickerSetOwnerAction::RenamePack {
                    set_id,
                    short_name,
                    new_title,
                } => match self
                    .session_backend
                    .rename_sticker_set_preview(set_id, &short_name, &new_title)
                {
                    Ok(maybe_preview) => {
                        let shown_title = maybe_preview
                            .as_ref()
                            .map(|p| p.title.clone())
                            .unwrap_or(new_title);
                        if let Some(preview) = maybe_preview
                            && let Some(BoxKind::StickerSet(st)) = self.box_host.top_kind_mut()
                        {
                            st.apply_preview(preview);
                        }
                        self.layer_stack
                            .show_toast(format!("Renamed sticker pack to \"{shown_title}\""));
                    }
                    Err(err) => self
                        .layer_stack
                        .show_toast(format!("Could not rename sticker pack: {err}")),
                },
                tg_app_info::boxes::sticker_set::StickerSetOwnerAction::ReorderSticker {
                    set_id,
                    short_name,
                    document_id,
                    new_position,
                } => match self.session_backend.change_sticker_position_preview(
                    set_id,
                    &short_name,
                    document_id,
                    new_position,
                ) {
                    Ok(maybe_preview) => {
                        if let Some(preview) = maybe_preview
                            && let Some(BoxKind::StickerSet(st)) = self.box_host.top_kind_mut()
                        {
                            st.apply_preview(preview);
                        }
                        self.layer_stack.show_toast(format!(
                            "Moved sticker to position #{}",
                            new_position + 1
                        ));
                    }
                    Err(err) => self
                        .layer_stack
                        .show_toast(format!("Could not reorder sticker: {err}")),
                },
                tg_app_info::boxes::sticker_set::StickerSetOwnerAction::RemoveSticker {
                    set_id,
                    short_name,
                    document_id,
                } => match self.session_backend.remove_sticker_from_set_preview(
                    set_id,
                    &short_name,
                    document_id,
                ) {
                    Ok(maybe_preview) => {
                        if let Some(preview) = maybe_preview
                            && let Some(BoxKind::StickerSet(st)) = self.box_host.top_kind_mut()
                        {
                            st.apply_preview(preview);
                        }
                        self.layer_stack.show_toast("Removed sticker from pack");
                    }
                    Err(err) => self
                        .layer_stack
                        .show_toast(format!("Could not remove sticker: {err}")),
                },
                tg_app_info::boxes::sticker_set::StickerSetOwnerAction::DeletePack {
                    set_id,
                    short_name,
                } => match self
                    .session_backend
                    .delete_sticker_set_preview(set_id, &short_name)
                {
                    Ok(()) => {
                        if let Some(BoxKind::StickerSet(st)) = self.box_host.top_kind_mut()
                            && (st.set_id == set_id
                                || (!short_name.is_empty()
                                    && st.short_name.eq_ignore_ascii_case(&short_name)))
                        {
                            self.box_host.pop_immediate();
                        }
                        self.layer_stack
                            .show_toast(format!("Deleted sticker pack \"{short_name}\""));
                    }
                    Err(err) => self
                        .layer_stack
                        .show_toast(format!("Could not delete sticker pack: {err}")),
                },
            },
            BoxEvent::AiToneToggleSave { tone, unsave } => {
                let res = self
                    .session_backend
                    .as_compose_ai_backend()
                    .ok_or_else(|| "AI compose styles are unavailable.".to_owned())
                    .and_then(|ai| ai.save_ai_tone(&tone, unsave));
                match res {
                    Ok(()) => self.layer_stack.show_toast(if unsave {
                        format!("Removed AI style \"{}\"", tone.title)
                    } else {
                        format!("Added AI style \"{}\"", tone.title)
                    }),
                    Err(err) => self
                        .layer_stack
                        .show_toast(format!("Could not update AI style: {err}")),
                }
            }
            BoxEvent::AiToneNextExample => {
                if let Some(BoxKind::PreviewAiTone(preview)) = self.box_host.top_kind_mut()
                    && let Some(ai) = self.session_backend.as_compose_ai_backend()
                {
                    preview.load_next_example(ai);
                }
            }
            BoxEvent::UrlAuthResolved { data, choice } => {
                match tg_app_info::boxes::click_handlers::execute_url_auth_decision(
                    &mut *self.session_backend,
                    *data,
                    choice,
                ) {
                    tg_app_info::boxes::click_handlers::UrlAuthResolutionEffect::OpenExternalUrl(url) => {
                        self.queue_external_url(&url);
                    }
                    tg_app_info::boxes::click_handlers::UrlAuthResolutionEffect::Toast(msg) => {
                        self.layer_stack.show_toast(msg);
                    }
                    tg_app_info::boxes::click_handlers::UrlAuthResolutionEffect::None => {}
                }
            }
            BoxEvent::OpenPeerProfile(peer_id) => {
                if let Some(session) = self.session.as_mut() {
                    session.show_peer_info(peer_id, SectionShow::forward());
                }
            }
            BoxEvent::OpenExternalUrl(url) => {
                self.queue_external_url(&url);
            }
            BoxEvent::MiniAppOpenConfirmed {
                bot_id,
                url,
                start_param,
            } => {
                self.trusted_open_webview.insert(bot_id);
                self.session_backend.trust_mini_app_bot(bot_id);
                self.open_bot_mini_app_with_params(bot_id, url, start_param);
            }
            BoxEvent::BotStartConfirmed {
                bot_id,
                start_param,
            } => self.send_bot_start(bot_id, start_param),
            BoxEvent::OverlayRequest(req) => {
                self.pending_overlay_requests.push(req);
            }
            BoxEvent::SendMusicTracks { peer_id, tracks } => {
                if let Err(err) = tg_app_core::history::backend::HistoryBackend::send_saved_music(
                    &mut *self.session_backend,
                    peer_id,
                    &tracks,
                ) {
                    self.layer_stack.show_toast(err);
                }
            }
            BoxEvent::ConfirmPhoneSubmitted {
                phone,
                phone_code_hash,
                code,
            } => match self
                .session_backend
                .confirm_phone_code(&phone_code_hash, &code)
            {
                Ok(true) => {
                    self.box_host.pop(ctx, &self.theme_manager);
                    let display = tg_app_info::boxes::confirm::format_display_phone(&phone);
                    self.layer_stack.show_box(ModalBox::Inform {
                        title: tg_lang::tr::lng_confirm_phone_title(),
                        body: tg_lang::tr::lng_confirm_phone_success(&display),
                    });
                }
                Ok(false) => {
                    if let Some(BoxKind::ConfirmPhoneCode(s)) = self.box_host.top_kind_mut() {
                        s.show_server_error("PHONE_CODE_INVALID");
                    }
                }
                Err(err) => {
                    if let Some(BoxKind::ConfirmPhoneCode(s)) = self.box_host.top_kind_mut() {
                        s.show_server_error(&err);
                    } else {
                        self.layer_stack.show_toast(err);
                    }
                }
            },
            BoxEvent::OpenAttachedStickerSet {
                set_id,
                short_name,
                title,
                is_emoji,
            } => {
                let preview = if !short_name.is_empty() {
                    self.session_backend
                        .resolve_sticker_set(&short_name, is_emoji)
                        .ok()
                } else {
                    None
                }
                .unwrap_or_else(|| tg_app_core::session_api::StickerSetPreview {
                    set_id,
                    access_hash: 0,
                    title,
                    short_name,
                    count: 0,
                    is_emoji,
                    installed: false,
                    archived: false,
                    official: false,
                    items: Vec::new(),
                });
                let st_box = tg_app_info::boxes::sticker_set::StickerSetBoxState::from_preview(
                    preview,
                );
                self.box_host
                    .push(ctx, BoxKind::StickerSet(st_box), &self.theme_manager);
            }
            BoxEvent::LoadEmojiUserpicCatalog {
                peer_id,
                catalog_kind,
            } => {
                if let Some(BoxKind::EmojiUserpic(state)) = self.box_host.top_kind_mut()
                    && state.peer_id == peer_id
                {
                    state.catalog_kind = catalog_kind;
                    state.load_default_catalog(&*self.session_backend, false);
                }
            }
            BoxEvent::LaunchExternalFileConfirmed {
                item_id,
                remember_extension,
                disable_ip_reveal_warning,
            } => {
                if let Some(ext) = remember_extension {
                    self.no_warning_extensions.insert(ext.clone());
                    if let Some(settings) = self.main_widget.settings_mut() {
                        settings.core_settings.no_warning_extensions.insert(ext);
                    }
                }
                if disable_ip_reveal_warning {
                    self.ip_reveal_warning = false;
                    if let Some(settings) = self.main_widget.settings_mut() {
                        settings.core_settings.ip_reveal_warning = false;
                    }
                }
                match self.session_backend.open_in_external_app_confirmed(item_id) {
                    Ok(path) => {
                        self.layer_stack
                            .show_toast(format!("Opening {}", path.display()));
                    }
                    Err(err) => {
                        self.layer_stack
                            .show_toast(format!("Failed to open file: {err}"));
                    }
                }
            }
        }
    }

    /// Requests launching `item_id` (`file_name`) in an external OS application, showing
    /// tdesktop's executable / unknown / extensionless / IP-reveal confirmation dialog first
    /// (`Data::LaunchWithWarning` in `data/data_document_resolver.cpp:91`).
    pub fn request_open_external_item(
        &mut self,
        item_id: tg_app_media::media::MediaItemId,
        file_name: &str,
    ) {
        if let Some(settings) = self.main_widget.settings() {
            for ext in &settings.core_settings.no_warning_extensions {
                self.no_warning_extensions.insert(ext.clone());
            }
            if !settings.core_settings.ip_reveal_warning {
                self.ip_reveal_warning = false;
            }
        }
        if let Some(kind) = classify_launch_warning(
            file_name,
            &self.no_warning_extensions,
            self.ip_reveal_warning,
        ) {
            self.media_overlay.close();
            self.box_host.push_immediate(
                BoxKind::LaunchFileConfirm(build_launch_file_confirm_box(item_id, file_name, kind)),
                &self.theme_manager,
            );
            return;
        }
        match self.session_backend.open_in_external_app_confirmed(item_id) {
            Ok(path) => {
                self.layer_stack
                    .show_toast(format!("Opening {}", path.display()));
            }
            Err(err) => {
                self.layer_stack
                    .show_toast(format!("Failed to open file: {err}"));
            }
        }
    }

    fn handle_overlay_event(&mut self, ev: OverlayEvent, ctx: &egui::Context) {
        match ev {
            OverlayEvent::Closed => {}
            OverlayEvent::ShowInChat { peer_id, msg_id } => {
                self.media_overlay.close();
                if let Some(session) = self.session.as_mut() {
                    session.show_peer_history(
                        PeerId(peer_id),
                        SectionShow::clear_stack(),
                        MsgId(msg_id),
                    );
                }
            }
            OverlayEvent::Forward(item_id) => {
                let item = self.session_backend.get_item(item_id);
                if let Some(ref it) = item
                    && !it.allows_forward
                {
                    self.layer_stack
                        .show_toast("Forwarding is restricted for this chat.");
                    return;
                }
                let msg_id = item.map_or(item_id.0 as i32, |it| it.msg_id);
                self.media_overlay.close();
                self.box_host.push(
                    ctx,
                    BoxKind::Share(ShareBoxState::for_media_item(item_id, msg_id)),
                    &self.theme_manager,
                );
            }
            OverlayEvent::Deleted(_) => self.layer_stack.show_toast("Media deleted"),
            OverlayEvent::Saved(path) => {
                self.layer_stack
                    .show_toast(format!("Saved to {}", path.display()));
            }
            OverlayEvent::Copied(_) => self.layer_stack.show_toast("Copied to clipboard"),
            OverlayEvent::OpenExternal(path) => {
                self.layer_stack
                    .show_toast(format!("Opening {}", path.display()));
            }
            OverlayEvent::ConfirmOpenExternal { item_id, file_name } => {
                self.request_open_external_item(item_id, &file_name);
            }
            OverlayEvent::EnterPip(pip) => {
                self.pip_window = Some(*pip);
            }
            OverlayEvent::VideoStream(act) => match act {
                tg_app_media::media::VideoStreamAction::Close => {}
                tg_app_media::media::VideoStreamAction::RequestQuality(q) => {
                    self.layer_stack
                        .show_toast(format!("Stream quality: {}", q.label()));
                }
                tg_app_media::media::VideoStreamAction::SendComment(text) => {
                    if let Some(stream) = self.media_overlay.video_stream_mut() {
                        let next_id = stream.comments().len() as i64 + 1;
                        stream.push_comment(tg_app_media::media::VideoStreamComment {
                            id: next_id,
                            sender_peer_id: 0,
                            sender_name: "You".to_owned(),
                            text,
                            paid_stars: 0,
                        });
                    }
                }
                tg_app_media::media::VideoStreamAction::DeleteComment(id) => {
                    if let Some(stream) = self.media_overlay.video_stream_mut() {
                        stream.remove_comment(id);
                    }
                }
                tg_app_media::media::VideoStreamAction::SetVolume(_)
                | tg_app_media::media::VideoStreamAction::ToggleComments(_) => {}
            },
            OverlayEvent::Sponsored(ev) => match ev {
                tg_app_media::media::PlaybackSponsoredEvent::Clicked { random_id, url } => {
                    if let Some(ex) =
                        tg_app_history::history::backend::HistoryBackend::as_layer233_extras_mut(
                            &mut *self.session_backend,
                        )
                    {
                        let _ = ex.click_sponsored_message(&random_id, false, true);
                    }
                    if !url.is_empty() {
                        self.open_link(&url);
                    }
                }
                tg_app_media::media::PlaybackSponsoredEvent::Viewed { random_id } => {
                    if let Some(ex) =
                        tg_app_history::history::backend::HistoryBackend::as_layer233_extras_mut(
                            &mut *self.session_backend,
                        )
                    {
                        let _ = ex.view_sponsored_message(&random_id);
                    }
                }
                tg_app_media::media::PlaybackSponsoredEvent::PromotePremium => {
                    self.pending_overlay_requests
                        .push(DeepLinkOverlayRequest::OpenPayments {
                            screen: tg_app_info::payments::PaymentsScreen::Premium,
                            slug: Some("no_ads".to_owned()),
                        });
                }
                tg_app_media::media::PlaybackSponsoredEvent::OpenAbout { .. } => {
                    self.open_link(tg_app_history::history::sponsored::ADS_PLATFORM_URL);
                }
                tg_app_media::media::PlaybackSponsoredEvent::Report { random_id } => {
                    if let Some(ex) =
                        tg_app_history::history::backend::HistoryBackend::as_layer233_extras_mut(
                            &mut *self.session_backend,
                        )
                    {
                        let _ = ex.report_sponsored_message(&random_id, &[]);
                    }
                    self.layer_stack.show_toast("Sponsored message reported");
                }
                tg_app_media::media::PlaybackSponsoredEvent::Closed { .. } => {}
            },
            OverlayEvent::ApplyThemePreview {
                theme_name,
                raw_bytes,
                ..
            } => {
                if self
                    .theme_manager
                    .load_custom_theme_bytes(&raw_bytes)
                    .is_ok()
                {
                    self.theme_manager.apply_to_egui(ctx);
                }
                self.media_overlay.close();
                self.layer_stack
                    .show_toast(format!("Applied theme: {theme_name}"));
            }
            OverlayEvent::AttachedStickers { media, media_label } => {
                self.media_overlay.close();
                self.open_attached_stickers_modal(ctx, &media, &media_label);
            }
        }
    }

    fn handle_main_menu_action(
        &mut self,
        act: MainMenuAction,
        self_peer_id: PeerId,
        ctx: &egui::Context,
    ) {
        match act {
            MainMenuAction::SwitchAccount(slot_index) => {
                self.layer_stack.close_main_menu();
                self.pending_account_switch = Some(slot_index);
            }
            MainMenuAction::AddAccount => {
                self.layer_stack.close_main_menu();
                let current_count = self.menu_accounts.len().max(1);
                if !self.session_backend.is_live_session() {
                    self.layer_stack
                        .show_toast("Adding multiple accounts is available in live mode.");
                } else if current_count >= self.max_accounts {
                    self.layer_stack.show_toast(format!(
                        "{}: up to {} accounts ({} with Telegram Premium).",
                        tg_lang::tr::lng_accounts_limit_title(),
                        self.max_accounts,
                        tg_session::PREMIUM_MAX_ACCOUNTS
                    ));
                } else {
                    self.pending_add_account = true;
                }
            }
            MainMenuAction::Close => self.layer_stack.close_main_menu(),
            MainMenuAction::MyProfile => {
                self.layer_stack.close_main_menu();
                if let Some(session) = self.session.as_mut() {
                    session.show_peer_info(self_peer_id, SectionShow::forward());
                }
            }
            MainMenuAction::NewGroup => {
                self.layer_stack.close_main_menu();
                self.box_host.push(
                    ctx,
                    BoxKind::CreateGroup(CreateGroupBoxState::new()),
                    &self.theme_manager,
                );
            }
            MainMenuAction::NewChannel => {
                self.layer_stack.close_main_menu();
                self.box_host.push(
                    ctx,
                    BoxKind::CreateChannel(CreateChannelBoxState::new()),
                    &self.theme_manager,
                );
            }
            MainMenuAction::Contacts => {
                self.layer_stack.close_main_menu();
                self.box_host.push(
                    ctx,
                    BoxKind::AddContact(AddContactBoxState::new()),
                    &self.theme_manager,
                );
            }
            // `window_main_menu.cpp`: the Calls row shows only the Recent Calls box
            // (`Calls::ShowCallsBox`), it does not open Settings.
            MainMenuAction::Calls => {
                self.layer_stack.close_main_menu();
                self.pending_open_recent_calls = true;
            }
            // "Set Emoji Status": Premium users get the emoji status selector
            // (`Info::Profile::EmojiStatusPanel`), others the Premium screen.
            MainMenuAction::SetEmojiStatus => {
                self.layer_stack.close_main_menu();
                if tg_app_core::chat_helpers::backend::ChatHelpersBackend::is_premium(
                    &*self.session_backend,
                ) {
                    tg_app_history::business::AppBusinessHost::open_emoji_status(ctx);
                } else {
                    self.handle_main_widget_action(
                        MainWidgetAction::OpenPayments {
                            screen: tg_app_info::payments::PaymentsScreen::Premium,
                            slug: None,
                        },
                        ctx,
                    );
                }
            }
            MainMenuAction::SavedMessages => {
                self.layer_stack.close_main_menu();
                if let Some(session) = self.session.as_mut() {
                    session.show_peer_history(
                        self_peer_id,
                        SectionShow::clear_stack(),
                        MsgId::SHOW_AT_UNREAD,
                    );
                }
            }
            MainMenuAction::Downloads => {
                self.layer_stack.close_main_menu();
                self.open_downloads();
            }
            MainMenuAction::Settings => {
                self.layer_stack.close_main_menu();
                if let Some(session) = self.session.as_mut() {
                    session.show_settings(SettingsPage::Main);
                }
            }
            MainMenuAction::ToggleNightMode => self.toggle_night_mode(),
            MainMenuAction::LockApp => {
                self.layer_stack.close_main_menu();
                if self.has_local_passcode() {
                    self.setup_passcode_lock();
                } else if self.session_backend.is_live_session() {
                    let settings_backend = self.session_backend.create_settings_backend();
                    self.main_widget.open_settings_section(
                        SettingsSection::LocalPasscode,
                        settings_backend,
                        self.theme_manager.embedded_type(),
                        self.theme_manager.accent(),
                    );
                    if let Some(session) = self.session.as_mut() {
                        session.show_settings(SettingsPage::Main);
                    }
                } else {
                    self.layer_stack.show_box(ModalBox::PasscodeSetup {
                        draft: String::new(),
                        error: String::new(),
                        has_existing: false,
                    });
                }
            }
            MainMenuAction::About => {
                self.layer_stack.close_main_menu();
                self.box_host.push(
                    ctx,
                    BoxKind::About(AboutBoxState::new()),
                    &self.theme_manager,
                );
            }
            MainMenuAction::RestartToUpdate => {
                self.layer_stack.close_main_menu();
                self.request_restart_to_update();
            }
            MainMenuAction::OpenMenuBot(bot_id) => {
                self.layer_stack.close_main_menu();
                self.open_mini_app_from_chat(bot_id, None, None, false);
            }
            MainMenuAction::Logout => {
                self.layer_stack.show_box(ModalBox::ConfirmLogout);
            }
        }
    }

    fn handle_layer_action(&mut self, act: LayerAction) {
        match act {
            LayerAction::ConfirmLogout => {
                if self.session_backend.is_live_session() {
                    self.layer_stack.hide_all();
                    self.media_overlay.close();
                    self.logout_requested = true;
                } else {
                    self.logout_to_intro();
                    self.layer_stack.show_toast("Logged out");
                }
            }
            LayerAction::SavePasscode(code) => {
                self.settings.local_passcode = Some(code);
                self.mark_settings_dirty();
                self.layer_stack
                    .show_toast("Local passcode saved (press Ctrl+L to lock)");
            }
            LayerAction::ClearPasscode => {
                self.settings.local_passcode = None;
                self.mark_settings_dirty();
                self.layer_stack.show_toast("Local passcode disabled");
            }
            LayerAction::SetProxyEnabled(enabled) => {
                self.session_backend.set_proxy_enabled(enabled);
            }
            LayerAction::SetConnectionStatus(status) => {
                self.session_backend.set_connection_status(status);
            }
        }
    }

    /// Returns milliseconds elapsed since controller construction.
    #[must_use]
    pub fn elapsed_ms(&self) -> i64 {
        tg_app_core::clock::now()
            .saturating_duration_since(self.started_at)
            .as_millis()
            .min(i64::MAX as u128) as i64
    }

    fn mark_settings_dirty(&mut self) {
        if self.settings_dirty_since.is_none() {
            self.settings_dirty_since = Some(tg_app_core::clock::now());
        }
    }
}

#[cfg(test)]
#[path = "controller_tests_sec2a.rs"]
#[cfg(test)]
mod tests_sec2a;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock_session::session_folders;
    use crate::window::section::SectionMemento;
    use tg_app_core::stand_in::backend::MockBackend;
    use tg_app_core::stand_in::strings::EnglishStrings;
    use tg_app_media::notifications::backend::NotifyBackend;
    use tg_app_media::webview::mock::SAMPLE_IV_URL;

    fn make_signed_in_controller() -> WindowController {
        WindowController::with_session(
            MockSession::new(true),
            ShellSettings {
                local_passcode: Some("4242".to_owned()),
                ..ShellSettings::default()
            },
            std::env::temp_dir().join("rustgram_unified_nav_test.json"),
        )
    }

    #[test]
    fn root_screen_transitions_intro_main_lock_logout() {
        let settings = ShellSettings {
            local_passcode: Some("4242".to_owned()),
            ..ShellSettings::default()
        };
        let path = std::env::temp_dir().join("rustgram_controller_test_unused.json");

        let mut ctrl = WindowController::new(
            Box::new(MockBackend::new(false)),
            Box::new(EnglishStrings),
            settings,
            path,
        );
        assert_eq!(ctrl.root_screen(), RootScreen::Intro);

        // Sign in transitions to Main with an initial session.
        ctrl.setup_main();
        assert_eq!(ctrl.root_screen(), RootScreen::Main);
        assert!(ctrl.session().is_some());

        // Lock transitions to PasscodeLock and hides layers.
        ctrl.setup_passcode_lock();
        assert_eq!(ctrl.root_screen(), RootScreen::PasscodeLock);

        // Unlock returns to Intro when backend wasn't signed in.
        ctrl.clear_passcode_lock();
        assert_eq!(ctrl.root_screen(), RootScreen::Intro);

        let mut signed_backend = MockBackend::new(true);
        signed_backend.set_proxy_enabled(false);
        let mut ctrl2 = WindowController::new(
            Box::new(signed_backend),
            Box::new(EnglishStrings),
            ShellSettings {
                local_passcode: Some("9999".to_owned()),
                ..ShellSettings::default()
            },
            std::env::temp_dir().join("rustgram_controller_test_unused2.json"),
        );
        assert_eq!(ctrl2.root_screen(), RootScreen::Main);
        ctrl2.setup_passcode_lock();
        assert_eq!(ctrl2.root_screen(), RootScreen::PasscodeLock);
        ctrl2.clear_passcode_lock();
        assert_eq!(ctrl2.root_screen(), RootScreen::Main);

        ctrl2.logout_to_intro();
        assert_eq!(ctrl2.root_screen(), RootScreen::Intro);
        assert!(ctrl2.session().is_none());
    }

    #[test]
    fn unified_navigation_chat_profile_statistics_boosts_and_back_stack() {
        let mut ctrl = make_signed_in_controller();
        // Wide 1440px layout opens profile in the third column without pushing onto the main stack.
        ctrl.session_mut().unwrap().update_layout(1440);
        ctrl.session_mut().unwrap().show_peer_history(
            session_peers::CHANNEL_DEV_LOG,
            SectionShow::clear_stack(),
            MsgId::SHOW_AT_UNREAD,
        );
        ctrl.session_mut()
            .unwrap()
            .toggle_peer_info(session_peers::CHANNEL_DEV_LOG);
        assert!(ctrl.session().unwrap().third_section().is_some());
        assert!(ctrl.session().unwrap().stack_is_empty());

        // Opening channel Statistics pushes onto the main section stack.
        ctrl.open_statistics_for_peer(session_peers::CHANNEL_DEV_LOG, false);
        assert_eq!(
            ctrl.session().unwrap().main_section(),
            Some(&SectionMemento::Statistics {
                peer_id: session_peers::CHANNEL_DEV_LOG,
                screen: StatisticsScreen::Channel,
            })
        );
        assert_eq!(ctrl.session().unwrap().stack_len(), 1);

        // Switching to Boosts replaces Statistics in-place (drop_same_from_stack).
        ctrl.open_statistics_for_peer(session_peers::CHANNEL_DEV_LOG, true);
        assert_eq!(
            ctrl.session().unwrap().main_section(),
            Some(&SectionMemento::Statistics {
                peer_id: session_peers::CHANNEL_DEV_LOG,
                screen: StatisticsScreen::Boosts,
            })
        );
        assert_eq!(ctrl.session().unwrap().stack_len(), 1);

        // Back unwinds straight to the channel history.
        assert!(
            ctrl.session_mut()
                .unwrap()
                .show_back_from_stack(SectionShow::forward())
        );
        assert!(ctrl.session().unwrap().main_section().is_none());
        assert_eq!(
            ctrl.session().unwrap().active_peer(),
            session_peers::CHANNEL_DEV_LOG
        );

        // Bot profile opens BotEarn stats; supergroup opens Supergroup stats.
        ctrl.open_statistics_for_peer(session_peers::BOT_CATALOG, false);
        assert_eq!(
            ctrl.session().unwrap().main_section(),
            Some(&SectionMemento::Statistics {
                peer_id: session_peers::BOT_CATALOG,
                screen: StatisticsScreen::BotEarn,
            })
        );
        ctrl.open_statistics_for_peer(session_peers::GROUP_CORE, false);
        assert_eq!(
            ctrl.session().unwrap().main_section(),
            Some(&SectionMemento::Statistics {
                peer_id: session_peers::GROUP_CORE,
                screen: StatisticsScreen::Supergroup,
            })
        );
    }

    #[test]
    fn telegram_link_routing_and_instant_view_dispatch() {
        let mut ctrl = make_signed_in_controller();
        ctrl.session_mut().unwrap().update_layout(1200);

        // 1. t.me/rustgram_log/102 navigates directly to channel + post #102.
        ctrl.open_link("https://t.me/rustgram_log/102");
        assert_eq!(
            ctrl.session().unwrap().active_peer(),
            session_peers::CHANNEL_DEV_LOG
        );
        assert_eq!(ctrl.session().unwrap().active_msg_id(), MsgId(102));
        assert!(ctrl.take_webview_requests().is_empty());

        // 2. @mira_vance opens Mira Vance's chat.
        ctrl.open_link("@mira_vance");
        assert_eq!(
            ctrl.session().unwrap().active_peer(),
            session_peers::USER_MIRA
        );

        // 3. tg://settings/privacy opens Settings -> PrivacySecurity.
        ctrl.open_link("tg://settings/privacy");
        assert_eq!(
            ctrl.session().unwrap().main_section(),
            Some(&SectionMemento::Settings {
                page: SettingsPage::PrivacySecurity,
            })
        );

        // 4. Instant View URL queues OpenInstantView.
        ctrl.open_link(SAMPLE_IV_URL);
        let reqs = ctrl.take_webview_requests();
        assert_eq!(
            reqs,
            vec![WebViewHostRequest::OpenInstantView {
                url: SAMPLE_IV_URL.to_owned(),
                webpage_id: None,
                fragment: None,
            }]
        );

        // 5. Ordinary HTTPS URL queues OpenExternalUrl.
        ctrl.open_link("https://rust-lang.org");
        assert_eq!(
            ctrl.take_webview_requests(),
            vec![WebViewHostRequest::OpenExternalUrl(
                "https://rust-lang.org".to_owned()
            )]
        );
    }

    #[test]
    fn keyboard_shortcuts_folders_chat_cycling_saved_messages_and_media_notifications() {
        let mut ctrl = make_signed_in_controller();
        ctrl.session_mut().unwrap().update_layout(1280);

        // Ctrl+0 -> Saved Messages.
        ctrl.trigger_dialogs_shortcut(DialogsShortcut::OpenSavedMessages);
        assert_eq!(ctrl.session().unwrap().active_peer(), session_peers::SELF);

        // Alt+Down / Ctrl+Tab cycles to the next chat in All Chats.
        ctrl.trigger_dialogs_shortcut(DialogsShortcut::NextChat);
        let second = ctrl.session().unwrap().active_peer();
        assert_ne!(second, session_peers::SELF);

        // Alt+Up cycles back to Saved Messages.
        ctrl.trigger_dialogs_shortcut(DialogsShortcut::PrevChat);
        assert_eq!(ctrl.session().unwrap().active_peer(), session_peers::SELF);

        // Ctrl+2 selects the Personal folder (`session_folders::PERSONAL`).
        ctrl.trigger_dialogs_shortcut(DialogsShortcut::SelectFolderSlot(1));
        assert_eq!(
            ctrl.session().unwrap().active_filter(),
            session_folders::PERSONAL
        );

        // Opening media from history activates the fullscreen MediaOverlayWidget.
        ctrl.open_history_media(session_peers::GROUP_CORE, MsgId(205), None);
        assert!(ctrl.media_overlay().is_open());

        // Clicking a desktop notification navigates straight to that chat and message.
        let mut notify_handle = ctrl.session_handle();
        notify_handle.open_chat(1, session_peers::USER_THEO, MsgId(0), MsgId(301), None);
        ctrl.poll_notification_opens(None);
        assert_eq!(
            ctrl.session().unwrap().active_peer(),
            session_peers::USER_THEO
        );
        assert_eq!(ctrl.session().unwrap().active_msg_id(), MsgId(301));

        // Theme switching updates ThemeManager generation and ShellSettings.
        let gen_before = ctrl.theme_generation();
        ctrl.apply_theme(
            EmbeddedType::NightGreen,
            Some(ThemeColor::rgb(0x40, 0xA7, 0xE3)),
        );
        assert!(ctrl.theme_generation() > gen_before);
        assert!(ctrl.theme_mode().is_night());
        assert_eq!(
            ctrl.settings().resolved_embedded_type(),
            EmbeddedType::NightGreen
        );
    }

    #[test]
    fn main_menu_restart_to_update_action_requests_updater_restart() {
        let mut ctrl = make_signed_in_controller();
        let ctx = egui::Context::default();
        ctrl.layer_stack.open_main_menu();
        assert!(ctrl.layer_stack.is_main_menu_open());

        ctrl.handle_main_menu_action(MainMenuAction::RestartToUpdate, session_peers::SELF, &ctx);

        assert!(!ctrl.layer_stack.is_main_menu_open());
        assert_eq!(
            ctrl.take_update_action(),
            Some(tg_app_core::core_types::startup_types::UpdateUiAction::RestartToUpdate)
        );
    }
}
