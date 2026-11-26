//! Full Telegram Desktop Settings subsystem for `tg-app`.
//!
//! Port origin: `Telegram/SourceFiles/settings/` (`settings_main.cpp`,
//! `settings_information.cpp`, `settings_notifications.cpp`,
//! `settings_privacy_security.cpp`, `settings_active_sessions.cpp`,
//! `settings_local_passcode.cpp`, `settings_cloud_password_*`, `settings_chat.cpp`,
//! `settings_folders.cpp`, `settings_advanced.cpp`, `settings_calls.cpp`,
//! `settings_power_saving.cpp`, `settings_scale_preview.cpp`, `settings_codes.cpp`,
//! `settings_search.cpp`, `settings_premium.cpp`, and `boxes/language_box.cpp`).

pub mod advanced;
pub use tg_app_core::settings::backend;
pub mod calls;
pub mod chat;
pub mod cloud_password;
pub mod codes;
pub mod dictionaries;
pub mod experimental;
pub mod faq_suggestions;
pub mod folder_links;
pub mod folders;
pub mod information;
pub mod language;
pub mod local_passcode;
pub mod main_page;
pub mod mock;
pub mod notifications;
pub mod notifications_reactions;
pub mod passkeys;
pub mod privacy;
pub mod recent_searches;
pub mod scale;
pub mod sessions;
pub mod shortcuts;
pub mod strings;
pub mod theme_bridge;
pub mod websites;
pub mod widgets;

use std::sync::{Arc, Mutex};

use egui::{Event, Id, ScrollArea, Ui, UiBuilder};
use tg_core::{CoreSettings, GlobalConfig};
use tg_lang::{Instance as LangInstance, tr};
use tg_theme::EmbeddedType;
use tg_ui::icons::invalidate_context_icon_cache;
use tg_ui::stand_in::{Colors, ToastStyle};
use tg_ui::toast::{ToastAttach, ToastConfig, ToastManager};
use tg_ui::widgets::buttons::IconKind;

use tg_app_core::core_types::startup_types::{UpdateSnapshot, UpdateState, UpdateUiAction};
use tg_app_core::core_types::window_types::ColumnLayout;
use tg_app_core::core_types::window_types::SettingsPage;
use tg_app_core::stand_in::strings::Strings;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{AuthUser, ConnectionStatus};

use advanced::{
    AdvancedState, AdvancedSubPanel, propagate_update_preferences, render_advanced_section,
    render_power_saving_panel,
};
use backend::SettingsBackend;
use calls::{CallsSettingsState, render_calls_section};
use chat::{ChatSettingsState, render_chat_settings_section};
use cloud_password::{CloudPasswordWizard, render_cloud_password_section};
use codes::{DebugOptions, SecretCodeAction, SecretCodeTracker};
use experimental::{ExperimentalSettingsState, render_experimental_section};
use faq_suggestions::FaqSuggestionsState;
use folders::{FoldersState, render_folders_section};
use information::{InformationState, render_information_section};
use language::{LanguageState, render_language_section};
use local_passcode::{
    LocalPasscodeCommand, LocalPasscodeState, PasscodeVerifierFn, render_local_passcode_section,
};
use main_page::render_main_settings_page_with_extras;
use mock::MockSettingsBackend;
use notifications::{NotificationsState, render_notifications_section};
use notifications_reactions::{ReactionsNotifyState, render_notifications_reactions_section};
use passkeys::{PasskeysState, render_passkeys_section};
use privacy::{PrivacyState, render_privacy_section};
use recent_searches::RecentSettingsSearches;
use sessions::{SessionsState, render_sessions_section};
use shortcuts::{ShortcutsSettingsState, render_shortcuts_section};
use theme_bridge::LiveThemeColors;
use websites::{WebsitesState, render_websites_section};
use widgets::{settings_top_bar, settings_top_bar_height};

/// Actions emitted by [`SettingsWidget`] to the outer window shell (`main_widget.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsAction {
    /// Navigate back from Settings.
    Back,
    /// Switch to another Settings sub-page.
    NavigatePage(SettingsPage),
    /// Toggle Day/Night mode.
    ToggleTheme,
    /// Toggle the left vertical folders sidebar.
    ToggleFoldersSidebar,
    /// Open local passcode setup modal.
    OpenPasscodeSetup,
    /// Immediately lock the application (when passcode is configured).
    LockNow,
    /// Update simulated connection status.
    SetConnectionStatus(ConnectionStatus),
    /// Toggle simulated proxy indicator.
    SetProxyEnabled(bool),
    /// Open the Payments / Stars / Gifts / Premium / Affiliate overlay on a specific screen.
    OpenPayments(tg_app_core::core_types::window_types::PaymentsScreen),
    /// Open the Telegram Business overlay.
    OpenBusiness,
    /// Open `EditPeerColorBox` for the currently signed-in user.
    OpenEditSelfColor,
    /// Toggle `"Suggest frequent contacts"` (`contacts.toggleTopPeers` via `DialogsBackend`).
    ToggleTopPeers(bool),
    /// Request Log Out confirmation box.
    RequestLogout,
}

/// Complete set of Rustgram Settings sections matching `Telegram/SourceFiles/settings/sections/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SettingsSection {
    /// Root Settings landing page (`settings_main.cpp`).
    #[default]
    Main,
    /// My Account / Profile editor (`settings_information.cpp`).
    Information,
    /// Notifications and Sounds (`settings_notifications.cpp`).
    Notifications,
    /// Notifications for Reactions (`settings_notifications_reactions.cpp`).
    NotificationsReactions,
    /// Privacy and Security (`settings_privacy_security.cpp`).
    PrivacySecurity,
    /// Active Sessions / Devices (`settings_active_sessions.cpp`).
    ActiveSessions,
    /// Connected Websites (`settings_websites.cpp`).
    ConnectedWebsites,
    /// Local Passcode & Auto-Lock (`settings_local_passcode.cpp`).
    LocalPasscode,
    /// Two-Step Verification / Cloud Password (`settings/cloud_password/*`).
    CloudPassword,
    /// Passkeys / WebAuthn (`settings/sections/settings_passkeys.cpp`).
    Passkeys,
    /// Chat Settings: themes, accents, backgrounds, emoji/stickers (`settings_chat.cpp`).
    Chat,
    /// Chat Folders & filters (`settings_folders.cpp`).
    Folders,
    /// Advanced: connection/proxy, storage, auto-download, export, system integration (`settings_advanced.cpp`).
    Advanced,
    /// Battery & Animations / Power Saving (`settings_power_saving.cpp`).
    PowerSaving,
    /// Speakers, Microphone, Camera & Calls (`settings_calls.cpp`).
    Calls,
    /// Interface Language & Translation (`boxes/language_box.cpp`).
    Language,
    /// Custom Keyboard Shortcuts editor (`settings_shortcuts.cpp`).
    Shortcuts,
    /// Experimental Settings (`settings_experimental.cpp`).
    Experimental,
}

impl SettingsSection {
    /// Display title shown in the settings top navigation bar.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Main => "Settings",
            Self::Information => "My Account",
            Self::Notifications => "Notifications and Sounds",
            Self::NotificationsReactions => "Notifications for reactions",
            Self::PrivacySecurity => "Privacy and Security",
            Self::ActiveSessions => "Devices",
            Self::ConnectedWebsites => "Connected Websites",
            Self::LocalPasscode => "Local Passcode",
            Self::CloudPassword => "Two-Step Verification",
            Self::Passkeys => "Passkeys",
            Self::Chat => "Chat Settings",
            Self::Folders => "Chat Folders",
            Self::Advanced => "Advanced",
            Self::PowerSaving => "Battery and Animations",
            Self::Calls => "Speakers and Camera",
            Self::Language => "Language",
            Self::Shortcuts => "Keyboard Shortcuts",
            Self::Experimental => "Experimental settings",
        }
    }

    /// Localized display title from `tg_lang::tr::*`.
    #[must_use]
    pub fn localized_title(self) -> String {
        match self {
            Self::Main => tr::lng_menu_settings(),
            Self::Information => tr::lng_settings_my_account(),
            Self::Notifications => tr::lng_settings_section_notify(),
            Self::NotificationsReactions => tr::lng_notification_reactions_title(),
            Self::PrivacySecurity => tr::lng_settings_section_privacy(),
            Self::ActiveSessions => tr::lng_settings_section_devices(),
            Self::ConnectedWebsites => tr::lng_settings_connected_title(),
            Self::LocalPasscode => tr::lng_settings_passcode_title(),
            Self::CloudPassword => tr::lng_settings_cloud_password_start_title(),
            Self::Passkeys => tr::lng_settings_passkeys_title(),
            Self::Chat => tr::lng_settings_section_chat_settings(),
            Self::Folders => tr::lng_settings_section_filters(),
            Self::Advanced => tr::lng_settings_advanced(),
            Self::PowerSaving => tr::lng_settings_power_menu(),
            Self::Calls => tr::lng_settings_section_call_settings(),
            Self::Language => tr::lng_settings_language(),
            Self::Shortcuts => "Keyboard Shortcuts".to_owned(),
            Self::Experimental => tr::lng_settings_experimental(),
        }
    }

    /// Maps the shell's coarse [`SettingsPage`] onto our detailed [`SettingsSection`].
    #[must_use]
    pub const fn from_shell_page(page: SettingsPage) -> Self {
        match page {
            SettingsPage::Main => Self::Main,
            SettingsPage::Account => Self::Information,
            SettingsPage::Notifications => Self::Notifications,
            SettingsPage::PrivacySecurity => Self::PrivacySecurity,
            SettingsPage::ChatSettings => Self::Chat,
            SettingsPage::Connection | SettingsPage::Advanced => Self::Advanced,
        }
    }
}

/// Stateful controller owning all local settings (`CoreSettings`, `GlobalConfig`),
/// server-side settings (`SettingsBackend`), live `tg-theme` palette, `tg-lang` instance,
/// section navigation stack, and sub-section UI states.
pub struct SettingsController {
    /// Local desktop settings (`tg_core::CoreSettings`).
    pub core_settings: CoreSettings,
    /// Runtime global config (`tg_core::GlobalConfig`, scale, autostart, debug mode).
    pub global_config: GlobalConfig,
    /// Server-side settings backend (implemented for [`MockSettingsBackend`]).
    pub backend: Box<dyn SettingsBackend>,
    /// Live `tg-theme` embedded palette & accent colors.
    pub live_theme: LiveThemeColors,
    /// Live `tg-lang` localization instance.
    pub lang_instance: LangInstance,
    /// Navigation stack of visited settings sections (bottom is always `Main`).
    pub section_stack: Vec<SettingsSection>,
    /// Last synced shell `SettingsPage` so external navigation pushes cleanly.
    last_shell_page: Option<SettingsPage>,
    /// Search bar text on Main settings page.
    pub search_query: String,
    /// The host draws the top bar (the Settings layer's `st::infoLayerTopBar`), so
    /// [`Self::show`] skips its own and the body fills the whole rect.
    pub host_top_bar: bool,
    /// Whether the profile QR preview card is open on Main.
    pub qr_open: bool,
    /// Secret keyboard code tracker (`settings_codes.cpp`).
    pub secret_codes: SecretCodeTracker,
    /// Experimental / debug runtime flags.
    pub debug_options: DebugOptions,
    /// Toast notifications overlay manager (`tg-ui`).
    pub toasts: ToastManager,
    /// Live self-updater snapshot synced from [`crate::window::controller::WindowController`].
    pub update_snapshot: UpdateSnapshot,
    update_snapshot_synced: bool,
    pending_update_action: Option<UpdateUiAction>,
    pending_update_prefs: Option<(bool, bool)>,
    pending_passcode_command: Option<LocalPasscodeCommand>,
    /// Per-section states.
    pub information: InformationState,
    pub notifications: NotificationsState,
    pub notifications_reactions: ReactionsNotifyState,
    pub privacy: PrivacyState,
    pub sessions: SessionsState,
    pub websites: WebsitesState,
    pub local_passcode: LocalPasscodeState,
    pub cloud_password: CloudPasswordWizard,
    pub passkeys: PasskeysState,
    pub chat: ChatSettingsState,
    pub folders: FoldersState,
    pub advanced: AdvancedState,
    pub calls: CallsSettingsState,
    pub language: LanguageState,
    pub shortcuts: ShortcutsSettingsState,
    /// Experimental settings UI state (`settings_experimental.cpp`).
    pub experimental: ExperimentalSettingsState,
    /// Cached FAQ suggestions (`settings_faq_suggestions.cpp`).
    pub faq_suggestions: FaqSuggestionsState,
    /// Recent Settings searches (`settings_recent_searches.cpp`).
    pub recent_searches: RecentSettingsSearches,
}

impl Default for SettingsController {
    fn default() -> Self {
        Self::new(Box::new(MockSettingsBackend::new()))
    }
}

impl SettingsController {
    /// Creates a new [`SettingsController`] backed by `backend`.
    #[must_use]
    pub fn new(backend: Box<dyn SettingsBackend>) -> Self {
        let initial_lang = backend.active_language_code();
        let mut lang_instance = LangInstance::new();
        tg_lang::with_current(|global| {
            if !global.id().is_empty() && global.id() == initial_lang {
                lang_instance.copy_from(global);
            }
        });
        Self {
            core_settings: CoreSettings::new(),
            global_config: GlobalConfig::default(),
            backend,
            live_theme: LiveThemeColors::default(),
            lang_instance,
            section_stack: vec![SettingsSection::Main],
            last_shell_page: None,
            search_query: String::new(),
            host_top_bar: false,
            qr_open: false,
            secret_codes: SecretCodeTracker::new(),
            debug_options: DebugOptions::default(),
            toasts: ToastManager::new(),
            update_snapshot: UpdateSnapshot::default(),
            update_snapshot_synced: false,
            pending_update_action: None,
            pending_update_prefs: None,
            pending_passcode_command: None,
            information: InformationState::default(),
            notifications: NotificationsState::default(),
            notifications_reactions: ReactionsNotifyState::default(),
            privacy: PrivacyState::default(),
            sessions: SessionsState::default(),
            websites: WebsitesState::default(),
            local_passcode: LocalPasscodeState::default(),
            cloud_password: CloudPasswordWizard::default(),
            passkeys: PasskeysState::default(),
            chat: ChatSettingsState::default(),
            folders: FoldersState::default(),
            advanced: AdvancedState::default(),
            calls: CallsSettingsState::default(),
            language: LanguageState::new(initial_lang),
            shortcuts: ShortcutsSettingsState::default(),
            experimental: ExperimentalSettingsState::default(),
            faq_suggestions: FaqSuggestionsState::new(),
            recent_searches: RecentSettingsSearches::new(),
        }
    }

    /// Resolves a `tg://setlanguage?lang=...` or `t.me/setlanguage/...` link and navigates to
    /// [`SettingsSection::Language`] with the resulting confirmation prompt (if any).
    pub fn open_set_language_link(&mut self, url: &str) -> Result<bool, String> {
        let prompt = self.backend.open_set_language_link(url)?;
        self.language.pending_prompt = prompt;
        self.push_section(SettingsSection::Language);
        Ok(self.language.pending_prompt.is_some())
    }

    /// Returns `true` after [`Self::set_update_snapshot`] has synced an initial updater snapshot.
    #[must_use]
    pub const fn has_synced_update_snapshot(&self) -> bool {
        self.update_snapshot_synced
    }

    /// Updates the read-only [`UpdateSnapshot`] displayed in Settings > Advanced.
    pub fn set_update_snapshot(&mut self, snapshot: UpdateSnapshot) {
        if !self.update_snapshot_synced {
            self.global_config.auto_update = snapshot.auto_update_enabled;
            self.global_config.install_beta_version = snapshot.allow_beta;
            self.update_snapshot_synced = true;
        }
        self.update_snapshot = snapshot;
    }

    /// Drains a pending UI-originated updater action (`CheckNow` or `RestartToUpdate`).
    pub fn take_update_action(&mut self) -> Option<UpdateUiAction> {
        self.pending_update_action.take()
    }

    /// Drains pending `(auto_update, install_beta_version)` preference changes from Advanced.
    pub fn take_update_prefs_change(&mut self) -> Option<(bool, bool)> {
        self.pending_update_prefs.take()
    }

    /// Drains a pending local passcode persistence command (`Set` or `Remove`), if any.
    pub fn take_passcode_command(&mut self) -> Option<LocalPasscodeCommand> {
        self.pending_passcode_command.take()
    }

    /// Seeds `CoreSettings`, `GlobalConfig`, Power Saving mirrors, and `LocalPasscodeState`
    /// from values loaded out of `tdata` on startup.
    pub fn seed_from_tdata(
        &mut self,
        core: CoreSettings,
        global: GlobalConfig,
        has_local_passcode: bool,
        verifier: Option<PasscodeVerifierFn>,
    ) {
        self.advanced.power_saving = advanced::PowerSavingFlags::from_core(core.power_saving);
        self.core_settings = core;
        self.global_config = global;
        self.local_passcode
            .sync_from_tdata(has_local_passcode, verifier);
    }

    /// Refreshes local passcode enabled state and verifier from `tdata`.
    pub fn sync_local_passcode_from_tdata(
        &mut self,
        has_local_passcode: bool,
        verifier: Option<PasscodeVerifierFn>,
    ) {
        self.local_passcode
            .sync_from_tdata(has_local_passcode, verifier);
    }

    /// Returns the currently displayed [`SettingsSection`].
    #[must_use]
    pub fn current_section(&self) -> SettingsSection {
        self.section_stack
            .last()
            .copied()
            .unwrap_or(SettingsSection::Main)
    }

    /// Navigates forward to `section` (deduplicating adjacent identical sections).
    pub fn push_section(&mut self, section: SettingsSection) {
        if self.current_section() != section {
            if section == SettingsSection::Main {
                self.section_stack.clear();
                self.section_stack.push(SettingsSection::Main);
            } else {
                self.section_stack.push(section);
            }
        }
    }

    /// Applies an optional deep-link control highlight (e.g. `experimental/<id>` from
    /// `tg://settings/experimental/<id>`).
    pub fn set_highlight_control(&mut self, section: SettingsSection, control_id: Option<&str>) {
        if section == SettingsSection::Experimental {
            if let Some(id) = control_id {
                self.experimental.highlight_option(id);
            } else {
                self.experimental.highlighted_id = None;
                self.experimental.scroll_to_highlighted = false;
            }
        } else if let Some(id) = control_id
            && id.starts_with("experimental/")
        {
            self.experimental.highlight_option(id);
        }
    }

    /// Pops one level of the settings navigation stack. Returns `true` if popped internally,
    /// or `false` if already at `SettingsSection::Main` (meaning exit Settings).
    pub fn pop_section(&mut self) -> bool {
        if self.current_section() == SettingsSection::Folders {
            if let Some(links_box) = self.folders.links_box.as_mut() {
                if !links_box.handle_escape() {
                    self.folders.links_box = None;
                }
                return true;
            }
            if self.folders.icon_panel_open {
                self.folders.icon_panel_open = false;
                return true;
            }
        }
        if self.section_stack.len() > 1 {
            self.section_stack.pop();
            true
        } else {
            false
        }
    }

    /// Executes a triggered [`SecretCodeAction`] and returns a user-facing toast message.
    pub fn execute_secret_code(&mut self, action: SecretCodeAction) -> String {
        match action {
            SecretCodeAction::ToggleDebugMode => {
                self.global_config.debug_mode = !self.global_config.debug_mode;
                if self.global_config.debug_mode {
                    "DEBUG logs enabled.".to_owned()
                } else {
                    "DEBUG logs disabled.".to_owned()
                }
            }
            SecretCodeAction::ViewLogs => {
                format!(
                    "Log directory: {}",
                    self.global_config.working_dir.display()
                )
            }
            SecretCodeAction::TestUpdate => {
                "Update check simulated: Rustgram is up to date.".to_owned()
            }
            SecretCodeAction::LoadCustomLang => {
                self.push_section(SettingsSection::Language);
                "Opened Language settings for custom pack.".to_owned()
            }
            SecretCodeAction::ToggleModerateMode => {
                self.core_settings.moderate_mode_enabled =
                    !self.core_settings.moderate_mode_enabled;
                if self.core_settings.moderate_mode_enabled {
                    "Moderate mode enabled.".to_owned()
                } else {
                    "Moderate mode disabled.".to_owned()
                }
            }
            SecretCodeAction::GetDifference => "Requested updates.getDifference sync.".to_owned(),
            SecretCodeAction::LoadColors => {
                self.push_section(SettingsSection::Chat);
                "Opened Chat Settings to load palette colors.".to_owned()
            }
            SecretCodeAction::LoadEndpoints => {
                "DC endpoints loader ready (offline mock).".to_owned()
            }
            SecretCodeAction::ToggleTestMode => {
                self.debug_options.test_dc_environment = !self.debug_options.test_dc_environment;
                if self.debug_options.test_dc_environment {
                    "Switched to the test environment.".to_owned()
                } else {
                    "Switched to the production environment.".to_owned()
                }
            }
            SecretCodeAction::OpenFolders => {
                self.push_section(SettingsSection::Folders);
                "Opened Chat Folders.".to_owned()
            }
            SecretCodeAction::RegisterUrlScheme => {
                "Forced tg:// URL scheme registration.".to_owned()
            }
            SecretCodeAction::ToggleFastButtons => {
                self.debug_options.fast_buttons_mode = !self.debug_options.fast_buttons_mode;
                if self.debug_options.fast_buttons_mode {
                    "Fast buttons mode enabled.".to_owned()
                } else {
                    "Fast buttons mode disabled.".to_owned()
                }
            }
            SecretCodeAction::ToggleExternalWeb => {
                self.debug_options.webview_transport_blocked =
                    !self.debug_options.webview_transport_blocked;
                if self.debug_options.webview_transport_blocked {
                    "WebView transport blocked.".to_owned()
                } else {
                    "WebView transport unblocked.".to_owned()
                }
            }
            SecretCodeAction::ResetSoundOverrides => {
                self.core_settings.sound_overrides.clear();
                "All sound overrides were reset.".to_owned()
            }
            SecretCodeAction::ToggleTestChatColors => {
                self.debug_options.testing_chat_colors = !self.debug_options.testing_chat_colors;
                if self.debug_options.testing_chat_colors {
                    "Testing chat theme colors!".to_owned()
                } else {
                    "Stopped testing chat theme colors.".to_owned()
                }
            }
            SecretCodeAction::OverrideSound(key) => {
                format!("Custom sound override slot selected: {key}")
            }
        }
    }

    fn show_toast(&mut self, ctx: &egui::Context, text: impl Into<String>) {
        let Some(text) = tg_app_core::user_error::visible_toast_text(text.into()) else {
            return;
        };
        self.toasts.show_toast(
            ctx,
            ToastConfig::simple(text)
                .with_icon(IconKind::Check)
                .with_attach(ToastAttach::Bottom),
        );
    }

    fn active_language_display_name(&self) -> String {
        let code = &self.language.active_lang_code;
        if code == "#TEST_X" {
            return "Test X".to_owned();
        }
        self.backend
            .languages()
            .into_iter()
            .find(|l| l.lang_code == *code)
            .map_or_else(|| "English".to_owned(), |l| l.name)
    }

    /// Renders the complete interactive Settings panel inside `ui`.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        connection_status: ConnectionStatus,
        layout: ColumnLayout,
    ) -> Option<SettingsAction> {
        let mut action: Option<SettingsAction> = None;
        let ctx = ui.ctx().clone();

        // Feed typed text events to secret code detector when no input widget has focus.
        if !ctx.egui_wants_keyboard_input() {
            let typed_chunks: Vec<String> = ctx.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        Event::Text(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect()
            });
            for chunk in typed_chunks {
                if let Some(code_act) = self.secret_codes.feed(&chunk) {
                    let msg = self.execute_secret_code(code_act);
                    self.show_toast(&ctx, msg);
                }
            }
        }

        let colors = self.live_theme.clone();
        let rect = ui.max_rect();
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::ZERO, colors.color("windowBg"));

        let section = self.current_section();
        let section_title = section.localized_title();
        let right_label = match section {
            SettingsSection::Main => Some(tr::lng_settings_information()),
            _ => None,
        };
        let mut body_rect = rect;
        if !self.host_top_bar {
            let (back_clicked, right_clicked) =
                settings_top_bar(ui, &section_title, right_label.as_deref(), &colors);
            if back_clicked && !self.pop_section() {
                action = Some(SettingsAction::Back);
            }
            if right_clicked && section == SettingsSection::Main {
                self.push_section(SettingsSection::Information);
            }
            body_rect.min.y += settings_top_bar_height();
        }

        let mut pending_toast: Option<String> = None;
        let mut pending_nav: Option<SettingsSection> = None;
        let mut pending_code: Option<SecretCodeAction> = None;

        ui.scope_builder(UiBuilder::new().max_rect(body_rect), |ui| {
            ScrollArea::vertical()
                .id_salt(("tg_settings_body_scroll", section as u8))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let max_w = 560.0_f32.min(ui.available_width()).max(260.0);
                    let side_pad = ((ui.available_width() - max_w) * 0.5).max(0.0);
                    egui::Frame::NONE
                        .inner_margin(egui::Margin {
                            left: side_pad as i8,
                            right: side_pad as i8,
                            top: 0,
                            bottom: 0,
                        })
                        .show(ui, |ui| {
                            ui.set_max_width(max_w);
                            ui.spacing_mut().item_spacing.y = 2.0;

                            match section {
                                SettingsSection::Main => {
                                    let lang_name = self.active_language_display_name();
                                    let out = render_main_settings_page_with_extras(
                                        ui,
                                        &mut self.search_query,
                                        &mut self.qr_open,
                                        &mut self.global_config,
                                        &mut self.core_settings,
                                        &mut self.faq_suggestions,
                                        &mut self.recent_searches,
                                        &lang_name,
                                        self.backend.as_ref(),
                                        &colors,
                                    );
                                    pending_nav = out.navigate;
                                    pending_toast = out.toast;
                                    if let Some(screen) = out.open_payments {
                                        action = Some(SettingsAction::OpenPayments(screen));
                                    } else if out.open_business {
                                        action = Some(SettingsAction::OpenBusiness);
                                    }
                                }
                                SettingsSection::Information => {
                                    let out = render_information_section(
                                        ui,
                                        &mut self.information,
                                        self.backend.as_mut(),
                                        &colors,
                                    );
                                    pending_toast = out.toast;
                                    if out.open_peer_color {
                                        action = Some(SettingsAction::OpenEditSelfColor);
                                    } else if out.request_logout {
                                        action = Some(SettingsAction::RequestLogout);
                                    }
                                }
                                SettingsSection::Notifications => {
                                    let out = render_notifications_section(
                                        ui,
                                        &mut self.notifications,
                                        &mut self.core_settings,
                                        self.backend.as_mut(),
                                        &colors,
                                    );
                                    if out.open_reactions {
                                        pending_nav = Some(SettingsSection::NotificationsReactions);
                                    }
                                    pending_toast = out.toast;
                                }
                                SettingsSection::NotificationsReactions => {
                                    pending_toast = render_notifications_reactions_section(
                                        ui,
                                        &mut self.notifications_reactions,
                                        self.backend.as_mut(),
                                        &colors,
                                    );
                                }
                                SettingsSection::PrivacySecurity => {
                                    let out = render_privacy_section(
                                        ui,
                                        &mut self.privacy,
                                        self.local_passcode.is_enabled(),
                                        self.backend.as_mut(),
                                        &colors,
                                    );
                                    if out.open_local_passcode {
                                        pending_nav = Some(SettingsSection::LocalPasscode);
                                    } else if out.open_cloud_password {
                                        pending_nav = Some(SettingsSection::CloudPassword);
                                    } else if out.open_passkeys {
                                        pending_nav = Some(SettingsSection::Passkeys);
                                    } else if out.open_active_sessions {
                                        pending_nav = Some(SettingsSection::ActiveSessions);
                                    } else if out.open_connected_websites {
                                        pending_nav = Some(SettingsSection::ConnectedWebsites);
                                    }
                                    if let Some(enabled) = out.toggled_top_peers {
                                        action = Some(SettingsAction::ToggleTopPeers(enabled));
                                    }
                                    pending_toast = out.toast;
                                }
                                SettingsSection::ActiveSessions => {
                                    pending_toast = render_sessions_section(
                                        ui,
                                        &mut self.sessions,
                                        &mut self.core_settings,
                                        self.backend.as_mut(),
                                        &colors,
                                    );
                                }
                                SettingsSection::ConnectedWebsites => {
                                    pending_toast = render_websites_section(
                                        ui,
                                        &mut self.websites,
                                        self.backend.as_mut(),
                                        &colors,
                                    );
                                }
                                SettingsSection::LocalPasscode => {
                                    let out = render_local_passcode_section(
                                        ui,
                                        &mut self.local_passcode,
                                        &mut self.core_settings,
                                        &mut self.global_config,
                                        &colors,
                                    );
                                    if out.lock_now {
                                        action = Some(SettingsAction::LockNow);
                                    }
                                    if let Some(cmd) = out.command {
                                        self.pending_passcode_command = Some(cmd);
                                    }
                                    pending_toast = out.toast;
                                }
                                SettingsSection::CloudPassword => {
                                    pending_toast = render_cloud_password_section(
                                        ui,
                                        &mut self.cloud_password,
                                        self.backend.as_mut(),
                                        &colors,
                                    );
                                }
                                SettingsSection::Passkeys => {
                                    pending_toast = render_passkeys_section(
                                        ui,
                                        &mut self.passkeys,
                                        self.backend.as_mut(),
                                        &colors,
                                    );
                                }
                                SettingsSection::Chat => {
                                    let out = render_chat_settings_section(
                                        ui,
                                        &mut self.chat,
                                        &mut self.core_settings,
                                        &mut self.global_config,
                                        &mut self.live_theme,
                                        self.backend.as_mut(),
                                        &colors,
                                    );
                                    // A theme card changes `live_theme`, and the window applies
                                    // that change. It must not also be reported as a night-mode
                                    // toggle: the toggle ran after the apply and flipped the
                                    // chosen dark theme straight back (TGB-2955).
                                    if out.toggled_folders_sidebar {
                                        action = Some(SettingsAction::ToggleFoldersSidebar);
                                    }
                                    if out.open_shortcuts {
                                        pending_nav = Some(SettingsSection::Shortcuts);
                                    }
                                    pending_toast = out.toast;
                                }
                                SettingsSection::Folders => {
                                    let out = render_folders_section(
                                        ui,
                                        &mut self.folders,
                                        &mut self.core_settings,
                                        self.backend.as_mut(),
                                        &colors,
                                    );
                                    if out.toggled_folders_sidebar {
                                        action = Some(SettingsAction::ToggleFoldersSidebar);
                                    }
                                    pending_toast = out.toast;
                                }
                                SettingsSection::Advanced => {
                                    let out = render_advanced_section(
                                        ui,
                                        &mut self.advanced,
                                        &mut self.core_settings,
                                        &mut self.global_config,
                                        &mut self.debug_options,
                                        &self.update_snapshot,
                                        connection_status,
                                        layout,
                                        self.backend.as_mut(),
                                        &colors,
                                    );
                                    if let Some(cs) = out.connection_status {
                                        action = Some(SettingsAction::SetConnectionStatus(cs));
                                    } else if let Some(pe) = out.proxy_enabled {
                                        action = Some(SettingsAction::SetProxyEnabled(pe));
                                    }
                                    if out.check_for_updates {
                                        self.pending_update_action = Some(UpdateUiAction::CheckNow);
                                    } else if out.restart_to_update {
                                        self.pending_update_action =
                                            Some(UpdateUiAction::RestartToUpdate);
                                    }
                                    if let Some((auto_update, install_beta)) =
                                        out.update_prefs_changed
                                    {
                                        self.pending_update_prefs =
                                            Some((auto_update, install_beta));
                                        self.update_snapshot.auto_update_enabled = auto_update;
                                        self.update_snapshot.allow_beta = install_beta;
                                        if !self.update_snapshot.disabled_by_cli
                                            && !self.update_snapshot.is_ready()
                                        {
                                            if !auto_update
                                                && matches!(
                                                    self.update_snapshot.state,
                                                    UpdateState::Idle
                                                        | UpdateState::UpToDate { .. }
                                                )
                                            {
                                                self.update_snapshot.state = UpdateState::Disabled;
                                            } else if auto_update
                                                && matches!(
                                                    self.update_snapshot.state,
                                                    UpdateState::Disabled
                                                )
                                            {
                                                self.update_snapshot.state = UpdateState::Idle;
                                            }
                                        }
                                        if !self.global_config.working_dir.as_os_str().is_empty() {
                                            let _ = propagate_update_preferences(
                                                Some(&self.global_config.working_dir),
                                                auto_update,
                                                install_beta,
                                                None,
                                            );
                                        }
                                    }
                                    if out.open_experimental {
                                        pending_nav = Some(SettingsSection::Experimental);
                                    }
                                    pending_code = out.secret_code;
                                    pending_toast = out.toast;
                                }
                                SettingsSection::PowerSaving => {
                                    ui.add_space(10.0);
                                    render_power_saving_panel(
                                        ui,
                                        &mut self.advanced.power_saving,
                                        &mut self.advanced.power_saving_expanded,
                                        &mut self.core_settings,
                                        &colors,
                                    );
                                    ui.add_space(16.0);
                                }
                                SettingsSection::Calls => {
                                    pending_toast = render_calls_section(
                                        ui,
                                        &mut self.calls,
                                        &mut self.core_settings,
                                        &colors,
                                    );
                                }
                                SettingsSection::Language => {
                                    pending_toast = render_language_section(
                                        ui,
                                        &mut self.language,
                                        &mut self.core_settings,
                                        &mut self.lang_instance,
                                        self.backend.as_mut(),
                                        &colors,
                                    );
                                }
                                SettingsSection::Shortcuts => {
                                    pending_toast =
                                        render_shortcuts_section(ui, &mut self.shortcuts, &colors);
                                }
                                SettingsSection::Experimental => {
                                    pending_toast = render_experimental_section(
                                        ui,
                                        &mut self.experimental,
                                        &mut self.core_settings,
                                        &colors,
                                    );
                                }
                            }
                        });
                });
        });

        if let Some(next) = pending_nav {
            self.push_section(next);
        }
        if let Some(code_act) = pending_code {
            let msg = self.execute_secret_code(code_act);
            pending_toast = Some(msg);
        }
        if let Some(msg) = pending_toast {
            self.show_toast(&ctx, msg);
        }

        // Paint active toasts inside this panel bounds.
        self.toasts
            .paint(&ctx, rect, &ToastStyle::default_toast(&colors));

        action
    }
}

/// Stateless shell entry point preserving compatibility with `window::main_widget::MainWidget`.
///
/// Hosts a persistent [`SettingsController`] in `egui::Context` memory so the desktop shell
/// renders the full Telegram Desktop settings suite without requiring edits to `window/`.
pub struct SettingsWidget;

impl SettingsWidget {
    /// Renders the Settings section inside `ui` (called by `MainWidget`).
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        ui: &mut Ui,
        page: SettingsPage,
        _user: &AuthUser,
        theme_mode: ThemeMode,
        folders_sidebar: bool,
        _has_passcode: bool,
        connection_status: ConnectionStatus,
        _proxy_enabled: bool,
        layout: ColumnLayout,
        _palette: &Palette,
        _strings: &dyn Strings,
    ) -> Option<SettingsAction> {
        let store_id = Id::unique("tg_app_settings_controller_v1");
        let handle = ui.ctx().data_mut(|d| {
            d.get_temp_mut_or_insert_with::<Arc<Mutex<SettingsController>>>(store_id, || {
                Arc::new(Mutex::new(SettingsController::default()))
            })
            .clone()
        });

        let Ok(mut ctrl) = handle.lock() else {
            return None;
        };

        // Sync shell Day/Night mode if toggled from outside Settings (e.g. Main Menu drawer).
        if theme_mode.is_night() != ctrl.live_theme.is_dark() {
            let next_kind = if theme_mode.is_night() {
                EmbeddedType::Night
            } else {
                EmbeddedType::Default
            };
            ctrl.live_theme.update(next_kind, None);
            invalidate_context_icon_cache(ui.ctx(), if theme_mode.is_night() { 2 } else { 1 });
        }
        ctrl.core_settings.chat_filters_horizontal = !folders_sidebar;

        // Sync shell-initiated sub-page jumps (e.g. clicking Connection Pill -> Connection page).
        if ctrl.last_shell_page != Some(page) {
            ctrl.last_shell_page = Some(page);
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

        ctrl.show(ui, connection_status, layout)
    }
}

#[cfg(test)]
mod tests {
    use tg_core::settings_proxy::{ProxySettingsMode, ProxyType as CoreProxyType};
    use tg_core::{CoreSettings, GlobalConfig, SCALE_AUTO};
    use tg_lang::{Instance as LangInstance, tr};
    use tg_transport::{ProxyData as TransportProxyData, ProxyStatus};

    use super::advanced::{
        PowerSavingFlags, ProxyFormDraft, core_to_transport_proxy, transport_to_core_proxy,
    };
    use super::backend::{
        AutoDownloadSettings, AutoDownloadSource, AutoDownloadType, Birthday, ChatFolder,
        CloudPasswordError, FolderIcon, MAX_AUTO_DOWNLOAD_BYTES, MAX_BIO_LEN, NotifyScope,
        PrivacyKey, PrivacyOption, PrivacyRule, SettingsBackend, UsernameStatus, format_bytes,
        validate_username_syntax,
    };
    use super::cloud_password::{CloudPasswordStep, CloudPasswordWizard};
    use super::codes::{SecretCodeAction, SecretCodeTracker};
    use super::language::{LanguageState, apply_language_switch, filter_languages};
    use super::local_passcode::{
        LocalPasscodeHash, LocalPasscodeState, PasscodeStrength, format_auto_lock,
        parse_auto_lock_hhmm,
    };
    use super::main_page::search_settings;
    use super::mock::{MOCK_EMAIL_CONFIRM_CODE, MockSettingsBackend};
    use super::privacy::{format_global_ttl, format_ttl_days};
    use super::scale::{scale_to_slider, scale_values, slider_to_scale, snap_scale_stop};
    use super::{SettingsController, SettingsSection};

    #[test]
    fn username_bio_name_and_birthday_validation() {
        assert_eq!(validate_username_syntax(""), UsernameStatus::Empty);
        assert_eq!(validate_username_syntax("abc"), UsernameStatus::TooShort);
        assert_eq!(
            validate_username_syntax(&"a".repeat(33)),
            UsernameStatus::TooLong
        );
        assert_eq!(
            validate_username_syntax("1starts_num"),
            UsernameStatus::InvalidChars
        );
        assert_eq!(
            validate_username_syntax("ends_under_"),
            UsernameStatus::InvalidChars
        );
        assert_eq!(
            validate_username_syntax("double__under"),
            UsernameStatus::InvalidChars
        );
        assert_eq!(
            validate_username_syntax("valid_user_99"),
            UsernameStatus::Available
        );

        let mut backend = MockSettingsBackend::new();
        assert_eq!(backend.check_username("admin"), UsernameStatus::Taken);
        assert_eq!(
            backend.check_username("taylor_kim"),
            UsernameStatus::Current
        );
        assert!(backend.update_username("admin").is_err());
        assert!(backend.update_username("@new_handle_42").is_ok());
        assert_eq!(backend.profile().username, "new_handle_42");

        assert!(backend.update_name("   ", "Last").is_err());
        assert!(backend.update_name("Jordan", "Lee").is_ok());
        assert_eq!(backend.profile().full_name(), "Jordan Lee");

        let too_long_bio = "x".repeat(MAX_BIO_LEN + 1);
        assert!(backend.update_bio(&too_long_bio).is_err());
        assert!(backend.update_bio("Rust developer").is_ok());

        assert!(
            backend
                .update_birthday(Some(Birthday {
                    day: 31,
                    month: 2,
                    year: Some(1999)
                }))
                .is_err()
        );
        assert!(
            backend
                .update_birthday(Some(Birthday {
                    day: 29,
                    month: 2,
                    year: Some(2000)
                }))
                .is_ok()
        );
        assert_eq!(
            backend.profile().birthday.unwrap().format_display(),
            "Feb 29, 2000"
        );
    }

    #[test]
    fn privacy_rules_all_keys_summary_and_normalization() {
        let mut backend = MockSettingsBackend::new();
        assert_eq!(backend.privacy_rules().len(), PrivacyKey::ALL.len());

        // Overlapping always/never entries normalize cleanly.
        let rule = PrivacyRule {
            option: PrivacyOption::Contacts,
            always_allow: vec![1001, 1002, 1002],
            never_allow: vec![1002, 1003],
            ..PrivacyRule::default()
        };
        backend.set_privacy_rule(PrivacyKey::LastSeen, rule);
        let stored = backend.privacy_rule(PrivacyKey::LastSeen);
        assert_eq!(stored.always_allow, vec![1001]);
        assert_eq!(stored.never_allow, vec![1002, 1003]);
        assert_eq!(stored.summary(PrivacyKey::LastSeen), "My Contacts (-2, +1)");

        // AddedByPhone cannot be set to Nobody; backend clamps to Contacts.
        backend.set_privacy_rule(
            PrivacyKey::AddedByPhone,
            PrivacyRule {
                option: PrivacyOption::Nobody,
                ..PrivacyRule::default()
            },
        );
        assert_eq!(
            backend.privacy_rule(PrivacyKey::AddedByPhone).option,
            PrivacyOption::Contacts
        );

        // CallsPeer2Peer uses Always / My Contacts / Never wording.
        assert_eq!(
            PrivacyRule {
                option: PrivacyOption::Everybody,
                ..PrivacyRule::default()
            }
            .summary(PrivacyKey::CallsPeer2Peer),
            "Always"
        );
        assert_eq!(format_ttl_days(180), "6 months");
        assert_eq!(format_global_ttl(604_800), "1 week");
    }

    #[test]
    fn sessions_device_classification_and_termination() {
        use super::backend::DeviceKind;

        assert_eq!(
            DeviceKind::classify("Windows", "Desktop", "Windows 11", 2040),
            DeviceKind::Windows
        );
        assert_eq!(
            DeviceKind::classify("Android", "Pixel", "Android 15", 21724),
            DeviceKind::Android
        );
        assert_eq!(
            DeviceKind::classify("macOS", "MacBook", "macOS 15", 2834),
            DeviceKind::Mac
        );
        assert_eq!(
            DeviceKind::classify("Web", "Firefox 132", "Linux", 2496),
            DeviceKind::Firefox
        );

        let mut backend = MockSettingsBackend::new();
        let initial = backend.active_sessions();
        assert!(initial.len() >= 3);
        assert!(!backend.terminate_session(0)); // cannot terminate current session
        let other_hash = initial[1].hash;
        assert!(backend.terminate_session(other_hash));
        assert_eq!(backend.active_sessions().len(), initial.len() - 1);

        let terminated = backend.terminate_all_other_sessions();
        assert!(terminated >= 1);
        assert_eq!(backend.active_sessions().len(), 1);
        assert!(backend.active_sessions()[0].is_current());
    }

    #[test]
    fn local_passcode_crypto_backoff_strength_and_autolock() {
        let hash = LocalPasscodeHash::derive("S3cur3!Pass");
        assert!(hash.verify("S3cur3!Pass"));
        assert!(!hash.verify("wrong"));

        assert_eq!(PasscodeStrength::evaluate(""), PasscodeStrength::Empty);
        assert_eq!(PasscodeStrength::evaluate("123"), PasscodeStrength::Weak);
        assert_eq!(PasscodeStrength::evaluate("abcd12"), PasscodeStrength::Fair);
        assert_eq!(
            PasscodeStrength::evaluate("Correct-Horse-99!"),
            PasscodeStrength::Strong
        );

        let mut state = LocalPasscodeState::default();
        let mut core = CoreSettings::new();
        let mut global = GlobalConfig::default();

        assert!(
            state
                .apply_new_passcode("pass1234", "mismatch", &mut global)
                .is_err()
        );
        assert!(
            state
                .apply_new_passcode("pass1234", "pass1234", &mut global)
                .is_ok()
        );
        assert!(state.is_enabled());

        // Lock management & trigger 3 bad tries -> 5s backoff kicks in.
        state.unlocked_for_manage = false;
        assert!(state.try_verify("bad", &mut global, 1_000).is_err());
        assert!(state.try_verify("bad", &mut global, 2_000).is_err());
        assert!(state.try_verify("bad", &mut global, 3_000).is_err());
        assert_eq!(global.passcode_bad_tries, 3);
        // Immediate 4th attempt rejected by backoff even with correct password.
        assert!(state.try_verify("pass1234", &mut global, 4_000).is_err());
        // After 5_000ms (at t=8_000ms), allowed and succeeds.
        assert!(state.try_verify("pass1234", &mut global, 8_000).is_ok());
        assert_eq!(global.passcode_bad_tries, 0);

        state.disable_passcode(&mut core, &mut global);
        assert!(!state.is_enabled());
        assert!(!core.system_unlock_enabled);

        assert_eq!(parse_auto_lock_hhmm("0:15"), Some(900));
        assert_eq!(parse_auto_lock_hhmm("1:30"), Some(5400));
        assert_eq!(parse_auto_lock_hhmm("0:00"), None);
        assert_eq!(parse_auto_lock_hhmm("bad"), None);
        assert_eq!(format_auto_lock(3600), "1 hour");
        assert_eq!(format_auto_lock(5400), "1:30");
    }

    #[test]
    fn cloud_password_two_step_wizard_flow() {
        let mut backend = MockSettingsBackend::new();
        let mut wizard = CloudPasswordWizard {
            new_password: "alpha-password".to_owned(),
            confirm_password: "different".to_owned(),
            ..CloudPasswordWizard::default()
        };
        assert!(wizard.submit_new_password().is_err());

        wizard.confirm_password = "alpha-password".to_owned();
        assert!(wizard.submit_new_password().is_ok());
        assert_eq!(wizard.step, CloudPasswordStep::Hint);

        // Hint equal to password is rejected.
        wizard.hint = "alpha-password".to_owned();
        assert!(wizard.submit_hint().is_err());

        wizard.hint = "greek letter".to_owned();
        assert!(wizard.submit_hint().is_ok());
        assert_eq!(wizard.step, CloudPasswordStep::Email);

        // Invalid email rejected.
        assert_eq!(
            backend.set_cloud_password(
                None,
                "alpha-password",
                "greek letter",
                Some("not-an-email")
            ),
            Err(CloudPasswordError::InvalidEmail)
        );

        let st = backend
            .set_cloud_password(
                None,
                "alpha-password",
                "greek letter",
                Some("user@example.test"),
            )
            .expect("set 2FA");
        assert!(st.has_password);
        assert!(!st.has_recovery);
        assert_eq!(st.unconfirmed_email_pattern, "u***@example.test");

        assert_eq!(
            backend.confirm_recovery_email("000000"),
            Err(CloudPasswordError::InvalidCode)
        );
        let confirmed = backend
            .confirm_recovery_email(MOCK_EMAIL_CONFIRM_CODE)
            .expect("confirm email");
        assert!(confirmed.has_recovery);
        assert_eq!(confirmed.email_pattern, "u***@example.test");
        assert!(confirmed.unconfirmed_email_pattern.is_empty());

        // Disable requires correct current password.
        assert_eq!(
            backend.disable_cloud_password("wrong"),
            Err(CloudPasswordError::WrongPassword)
        );
        let disabled = backend
            .disable_cloud_password("alpha-password")
            .expect("disable 2FA");
        assert!(!disabled.has_password);
    }

    #[test]
    fn folders_crud_reorder_and_suggested() {
        let mut backend = MockSettingsBackend::new();
        let invalid = ChatFolder {
            title: "EmptyRules".to_owned(),
            ..ChatFolder::default()
        };
        assert!(backend.save_folder(invalid).is_err());

        let too_long = ChatFolder {
            title: "ThirteenChars".to_owned(),
            include_contacts: true,
            ..ChatFolder::default()
        };
        assert!(backend.save_folder(too_long).is_err());

        let created = backend
            .save_folder(ChatFolder {
                title: "Alerts".to_owned(),
                icon: FolderIcon::Star,
                include_channels: true,
                exclude_muted: true,
                ..ChatFolder::default()
            })
            .expect("save folder");
        assert!(created.id >= 5);

        let ids: Vec<i32> = backend.folders().iter().map(|f| f.id).rev().collect();
        backend.reorder_folders(&ids);
        assert_eq!(backend.folders()[0].id, created.id);

        assert!(backend.delete_folder(created.id));
        assert!(backend.accept_suggested_folder(0).is_some());
    }

    #[test]
    fn proxy_link_parsing_and_core_settings_integration() {
        let mut draft = ProxyFormDraft {
            link_input:
                "tg://proxy?server=proxy.example.test&port=443&secret=dd000102030405060708090a0b0c0d0e0f"
                    .to_owned(),
            ..ProxyFormDraft::default()
        };
        let parsed = draft.apply_link().expect("parse dd MTProto proxy link");
        assert_eq!(parsed.status(), ProxyStatus::Valid);
        assert_eq!(draft.proxy_type, CoreProxyType::Mtproto);
        assert_eq!(draft.host, "proxy.example.test");
        assert_eq!(draft.port, "443");

        let validated = draft.build_validated().expect("validate form");
        let mut core = CoreSettings::new();
        core.proxy.add_to_list(validated.clone());
        core.proxy.set_selected(validated.clone());
        core.proxy.set_settings(ProxySettingsMode::Enabled);

        // Roundtrip through CoreSettings binary serialization!
        let serialized = core.serialize();
        let mut restored = CoreSettings::new();
        restored
            .add_from_serialized(&serialized, tg_core::TDESKTOP_COMPAT_VERSION)
            .expect("deserialize CoreSettings");
        assert_eq!(restored.proxy.settings(), ProxySettingsMode::Enabled);
        assert_eq!(restored.proxy.list().len(), 1);
        assert_eq!(restored.proxy.selected().host, "proxy.example.test");

        // Back to tg:// link via tg-transport.
        let tp: TransportProxyData = core_to_transport_proxy(restored.proxy.selected());
        let link = tp.to_tg_link().expect("tg link");
        assert!(link.starts_with("tg://proxy?server=proxy.example.test&port=443&secret="));
        let roundtrip = transport_to_core_proxy(&TransportProxyData::from_link(&link).unwrap());
        assert_eq!(roundtrip, validated);
    }

    #[test]
    fn auto_download_power_saving_scale_language_and_codes() {
        let mut ad = AutoDownloadSettings::default();
        assert!(ad.has_enabled_types(AutoDownloadSource::Private));
        assert!(ad.should_download(
            AutoDownloadSource::Private,
            AutoDownloadType::Photo,
            5 * 1024 * 1024
        ));
        ad.set_bytes_limit(
            AutoDownloadSource::Private,
            AutoDownloadType::File,
            MAX_AUTO_DOWNLOAD_BYTES + 10_000,
        );
        assert_eq!(
            ad.bytes_limit(AutoDownloadSource::Private, AutoDownloadType::File),
            MAX_AUTO_DOWNLOAD_BYTES
        );
        ad.set_disabled_for_source(AutoDownloadSource::Channels);
        assert!(!ad.has_enabled_types(AutoDownloadSource::Channels));
        assert_eq!(format_bytes(0), "Off");
        assert_eq!(format_bytes(10 * 1024 * 1024), "10 MB");

        let mut pf = PowerSavingFlags::default();
        assert_eq!(pf.summary(), "Off");
        pf.set_all(true);
        assert_eq!(pf.disabled_count(), 11);
        assert_eq!(pf.summary(), "All off");

        assert_eq!(snap_scale_stop(SCALE_AUTO), 100);
        assert_eq!(snap_scale_stop(132), 130);
        assert_eq!(snap_scale_stop(20), 50);
        // `SetupInterfaceScale`: 50..=300 % in 5 % steps, so 100 % sits at 20 % of the track.
        assert_eq!(scale_values(100).len(), 51);
        assert!((scale_to_slider(100) - 0.2).abs() < 1e-9);
        assert!((scale_to_slider(300) - 1.0).abs() < 1e-9);
        for stop in (50..=300).step_by(5) {
            assert_eq!(slider_to_scale(scale_to_slider(stop)), stop);
        }
        // A configured scale between two steps gets its own slider position.
        assert_eq!(scale_values(112).len(), 52);

        // Language search & tg-lang live switch.
        let backend = MockSettingsBackend::new();
        let langs = backend.languages();
        assert_eq!(filter_languages(&langs, "deutsch").len(), 1);
        assert_eq!(filter_languages(&langs, "ja").len(), 1);
        let mut lang_inst = LangInstance::new();
        let mut lang_state = LanguageState::default();
        let de = langs.iter().find(|l| l.lang_code == "de").unwrap();
        apply_language_switch(&mut lang_inst, &mut lang_state, de);
        assert_eq!(lang_inst.id(), "de");
        assert_eq!(lang_state.active_lang_code, "de");

        // Secret codes suffix matching (`CodesFeedString`).
        let mut tracker = SecretCodeTracker::new();
        assert_eq!(tracker.feed("hello_world_"), None);
        assert_eq!(
            tracker.feed("debugmode"),
            Some(SecretCodeAction::ToggleDebugMode)
        );
        assert_eq!(tracker.feed("xxx"), None);
        assert_eq!(tracker.feed("fol"), None);
        assert_eq!(tracker.feed("ders"), Some(SecretCodeAction::OpenFolders));

        // Controller secret code execution & navigation stack.
        let mut ctrl = SettingsController::default();
        assert!(!ctrl.global_config.debug_mode);
        let _ = ctrl.execute_secret_code(SecretCodeAction::ToggleDebugMode);
        assert!(ctrl.global_config.debug_mode);
        let _ = ctrl.execute_secret_code(SecretCodeAction::OpenFolders);
        assert_eq!(ctrl.current_section(), SettingsSection::Folders);
        assert!(ctrl.pop_section());
        assert_eq!(ctrl.current_section(), SettingsSection::Main);

        // Search index queries.
        assert!(!search_settings("proxy").is_empty());
        assert!(!search_settings("corner").is_empty());
        assert!(!search_settings("2fa").is_empty());

        // Notifications reset.
        let mut backend_mut = MockSettingsBackend::new();
        assert!(
            !backend_mut
                .scope_notifications(NotifyScope::PrivateChats)
                .exceptions
                .is_empty()
        );
        backend_mut.reset_all_notifications();
        assert!(
            backend_mut
                .scope_notifications(NotifyScope::PrivateChats)
                .exceptions
                .is_empty()
        );
    }

    #[test]
    fn power_saving_grouped_checkboxes_and_autoplay_write_core_settings_directly() {
        use tg_core::settings::PowerSavingFlags as CorePowerSavingFlags;

        use super::advanced::{
            POWER_SAVING_GROUP_CALLS, POWER_SAVING_GROUP_CHAT, POWER_SAVING_GROUP_EMOJI,
            POWER_SAVING_GROUP_STICKERS, POWER_SAVING_GROUP_UI, PowerSavingFlags,
            is_power_saving_locked_by_battery, power_saving_effective_flags, power_saving_summary,
            set_power_saving_auto_on_low_battery, set_power_saving_flag_enabled,
            set_power_saving_group_enabled, sync_battery_saver_to_core,
        };
        use super::chat::sync_autoplay_to_backend;

        // Exact 5 groups from `settings_power_saving.cpp` partition all 11 PowerSaving::Flags bits.
        let groups = [
            POWER_SAVING_GROUP_STICKERS,
            POWER_SAVING_GROUP_EMOJI,
            POWER_SAVING_GROUP_CHAT,
            POWER_SAVING_GROUP_CALLS,
            POWER_SAVING_GROUP_UI,
        ];
        let mut union = CorePowerSavingFlags::NONE;
        for g in groups {
            assert!(!union.intersects(g), "overlapping power saving group bits");
            union |= g;
        }
        assert_eq!(union, CorePowerSavingFlags::ALL);

        let mut core = CoreSettings::new();
        assert_eq!(
            power_saving_summary(&core),
            tr::lng_settings_cloud_password_off()
        );

        // Unchecking Animated Stickers group disables both sticker flags in CoreSettings::power_saving.
        set_power_saving_group_enabled(&mut core, POWER_SAVING_GROUP_STICKERS, false);
        assert!(
            core.power_saving
                .contains(CorePowerSavingFlags::STICKERS_PANEL)
        );
        assert!(
            core.power_saving
                .contains(CorePowerSavingFlags::STICKERS_CHAT)
        );
        assert_eq!(power_saving_summary(&core), "2/11 reduced");

        // Re-enabling a single child checkbox clears only that bit.
        set_power_saving_flag_enabled(&mut core, CorePowerSavingFlags::STICKERS_PANEL, true);
        assert!(
            !core
                .power_saving
                .contains(CorePowerSavingFlags::STICKERS_PANEL)
        );
        assert!(
            core.power_saving
                .contains(CorePowerSavingFlags::STICKERS_CHAT)
        );

        // UI mirror round-trips losslessly with tg-core PowerSavingFlags.
        let mirror = PowerSavingFlags::from_core(core.power_saving);
        assert_eq!(mirror.to_core(), core.power_saving);

        // Low-battery OS Battery Saver locks editing and forces all flags on unless ignored.
        sync_battery_saver_to_core(&mut core, Some(true));
        assert!(core.power_saving_force_all);
        assert!(is_power_saving_locked_by_battery(&core));
        assert_eq!(
            power_saving_effective_flags(&core),
            CorePowerSavingFlags::ALL
        );
        assert_eq!(power_saving_summary(&core), super::strings::POWER_ALL_OFF);

        // Edits are blocked while locked by OS battery saver.
        set_power_saving_flag_enabled(&mut core, CorePowerSavingFlags::CHAT_SPOILER, false);
        assert!(
            !core
                .power_saving
                .contains(CorePowerSavingFlags::CHAT_SPOILER)
        );

        // Turning off "Save Power on Low Battery" sets ignore_battery_saving and unlocks editing.
        set_power_saving_auto_on_low_battery(&mut core, false);
        assert!(core.ignore_battery_saving);
        assert!(!is_power_saving_locked_by_battery(&core));
        set_power_saving_flag_enabled(&mut core, CorePowerSavingFlags::CHAT_SPOILER, false);
        assert!(
            core.power_saving
                .contains(CorePowerSavingFlags::CHAT_SPOILER)
        );

        // Chat Settings autoplay writes CoreSettings::autoplay directly and mirrors to backend.
        let mut backend = MockSettingsBackend::new();
        core.autoplay.gifs = false;
        core.autoplay.videos = false;
        core.autoplay.round_videos = true;
        core.autoplay.video_stickers = false;
        sync_autoplay_to_backend(&core, &mut backend);
        let ad = backend.auto_download();
        assert!(!ad.autoplay_gifs);
        assert!(!ad.autoplay_videos);
        assert!(ad.autoplay_round_videos);
        assert!(!core.autoplay.video_stickers);
    }

    /// Captures a PNG screenshot of Settings > Advanced with an update ready to
    /// `crates/tg-app/docs/settings_advanced_update.png`.
    ///
    /// Run manually with:
    /// `cargo test -p tg-app --lib settings::tests::capture_advanced_update_screenshot -- --ignored --nocapture --test-threads=1`
    #[test]
    #[ignore]
    fn capture_advanced_update_screenshot() {
        use std::path::PathBuf;
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        use tg_updater::version::Version;

        use tg_app_core::core_types::startup_types::{UpdateSnapshot, UpdateState};
        use tg_app_core::core_types::window_types::{ChatLayout, ColumnLayout, WindowLayout};
        use tg_app_core::stand_in::types::ConnectionStatus;

        let docs_dir = tg_app_core::core_types::paths::workspace_app_docs_dir();
        let _ = std::fs::create_dir_all(&docs_dir);
        let out_path = docs_dir.join("settings_advanced_update.png");

        struct AdvancedShotApp {
            ctrl: SettingsController,
            out_path: PathBuf,
            frames: u32,
            requested: bool,
            done: Arc<AtomicBool>,
        }

        impl eframe::App for AdvancedShotApp {
            fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
                let ctx = ui.ctx().clone();
                if self.done.load(Ordering::Acquire) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }

                tg_ui::apply_tdesktop_visuals(&ctx, &self.ctrl.live_theme);
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        let _ = self.ctrl.show(
                            ui,
                            ConnectionStatus::Connected,
                            ColumnLayout {
                                filters_width: 0,
                                body_width: 620,
                                dialogs_width: 0,
                                chat_width: 620,
                                third_width: 0,
                                window_layout: WindowLayout::OneColumn,
                                chat_layout: ChatLayout::Normal,
                                dialogs_small_column: false,
                            },
                        );
                    });

                self.frames += 1;
                if !self.requested && self.frames >= 6 {
                    self.requested = true;
                    let path = self.out_path.clone();
                    let done = Arc::clone(&self.done);
                    let ctx_cb = ctx.clone();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                        egui::ScreenshotCallback::new(move |img| {
                            let [w, h] = img.size;
                            let mut raw = Vec::with_capacity(w * h * 4);
                            for px in &img.pixels {
                                let [r, g, b, a] = px.to_srgba_unmultiplied();
                                raw.extend_from_slice(&[r, g, b, a]);
                            }
                            if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, raw) {
                                let _ = buf.save(&path);
                            }
                            done.store(true, Ordering::Release);
                            ctx_cb.request_repaint();
                        }),
                    ));
                } else {
                    ctx.request_repaint();
                }
            }
        }

        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("Rustgram Settings Advanced Update Screenshot")
                .with_inner_size([620.0, 820.0]),
            #[cfg(windows)]
            event_loop_builder: Some(Box::new(|builder| {
                use winit::platform::windows::EventLoopBuilderExtWindows as _;
                builder.with_any_thread(true);
            })),
            ..Default::default()
        };

        let _ = eframe::run_native(
            "Rustgram Settings Advanced Update Screenshot",
            options,
            Box::new(move |cc| {
                tg_ui::fonts::register_tdesktop_fonts(&cc.egui_ctx);
                let mut ctrl = SettingsController::default();
                ctrl.set_update_snapshot(UpdateSnapshot {
                    state: UpdateState::Ready {
                        version: Version::new(0, 2, 0),
                        staging_dir: PathBuf::from("tupdates/temp"),
                        updater_promoted: true,
                    },
                    current_version: Version::new(0, 1, 0),
                    auto_update_enabled: true,
                    allow_beta: false,
                    disabled_by_cli: false,
                    last_check_unix: 1_700_000_000,
                    next_check_unix: None,
                });
                ctrl.push_section(SettingsSection::Advanced);
                Ok(Box::new(AdvancedShotApp {
                    ctrl,
                    out_path,
                    frames: 0,
                    requested: false,
                    done: Arc::new(AtomicBool::new(false)),
                }))
            }),
        );
    }

    /// Captures PNG screenshots of Main Settings, Chat Settings, and Privacy & Security
    /// to `crates/tg-app/docs/settings_*.png`.
    ///
    /// Run manually with:
    /// `cargo test -p tg-app --lib settings::tests::capture_settings_screenshots -- --ignored --nocapture --test-threads=1`
    #[test]
    #[ignore]
    fn capture_settings_screenshots() {
        use std::path::PathBuf;
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        use tg_theme::EmbeddedType;
        use tg_ui::icons::invalidate_context_icon_cache;

        use tg_app_core::core_types::window_types::{ChatLayout, ColumnLayout, WindowLayout};
        use tg_app_core::stand_in::types::ConnectionStatus;

        let docs_dir = tg_app_core::core_types::paths::workspace_app_docs_dir();
        let _ = std::fs::create_dir_all(&docs_dir);

        let targets: Vec<(SettingsSection, EmbeddedType, PathBuf)> = vec![
            (
                SettingsSection::Main,
                EmbeddedType::Default,
                docs_dir.join("settings_main.png"),
            ),
            (
                SettingsSection::Main,
                EmbeddedType::Night,
                docs_dir.join("settings_main_night.png"),
            ),
            (
                SettingsSection::Chat,
                EmbeddedType::Default,
                docs_dir.join("settings_chat.png"),
            ),
            (
                SettingsSection::PrivacySecurity,
                EmbeddedType::Default,
                docs_dir.join("settings_privacy.png"),
            ),
        ];

        struct ShotApp {
            ctrl: SettingsController,
            targets: Vec<(SettingsSection, EmbeddedType, PathBuf)>,
            index: usize,
            frames_on_target: u32,
            requested_for_index: Option<usize>,
            completed_count: Arc<AtomicUsize>,
            theme_gen: u64,
        }

        impl eframe::App for ShotApp {
            fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
                let ctx = ui.ctx().clone();
                let done = self.completed_count.load(Ordering::Acquire);
                if done >= self.targets.len() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
                if done > self.index {
                    self.index = done;
                    self.frames_on_target = 0;
                    self.requested_for_index = None;
                }

                let (section, theme_kind, ref path) = self.targets[self.index];
                if self.ctrl.live_theme.kind != theme_kind {
                    self.ctrl.live_theme.update(theme_kind, None);
                    self.theme_gen += 1;
                    invalidate_context_icon_cache(&ctx, self.theme_gen);
                }
                if self.ctrl.current_section() != section {
                    self.ctrl.section_stack.clear();
                    self.ctrl.section_stack.push(SettingsSection::Main);
                    if section != SettingsSection::Main {
                        self.ctrl.section_stack.push(section);
                    }
                }

                tg_ui::apply_tdesktop_visuals(&ctx, &self.ctrl.live_theme);
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        let _ = self.ctrl.show(
                            ui,
                            ConnectionStatus::Connected,
                            ColumnLayout {
                                filters_width: 0,
                                body_width: 620,
                                dialogs_width: 0,
                                chat_width: 620,
                                third_width: 0,
                                window_layout: WindowLayout::OneColumn,
                                chat_layout: ChatLayout::Normal,
                                dialogs_small_column: false,
                            },
                        );
                    });

                self.frames_on_target += 1;
                if self.requested_for_index != Some(self.index) && self.frames_on_target >= 6 {
                    self.requested_for_index = Some(self.index);
                    let out_path = path.clone();
                    let counter = Arc::clone(&self.completed_count);
                    let ctx_cb = ctx.clone();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                        egui::ScreenshotCallback::new(move |img| {
                            let [w, h] = img.size;
                            let mut raw = Vec::with_capacity(w * h * 4);
                            for px in &img.pixels {
                                let [r, g, b, a] = px.to_srgba_unmultiplied();
                                raw.extend_from_slice(&[r, g, b, a]);
                            }
                            if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, raw) {
                                let _ = buf.save(&out_path);
                            }
                            counter.fetch_add(1, Ordering::Release);
                            ctx_cb.request_repaint();
                        }),
                    ));
                } else {
                    ctx.request_repaint();
                }
            }
        }

        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("Rustgram Settings Screenshot")
                .with_inner_size([620.0, 820.0]),
            #[cfg(windows)]
            event_loop_builder: Some(Box::new(|builder| {
                use winit::platform::windows::EventLoopBuilderExtWindows as _;
                builder.with_any_thread(true);
            })),
            ..Default::default()
        };

        let _ = eframe::run_native(
            "Rustgram Settings Screenshot",
            options,
            Box::new(move |_cc| {
                Ok(Box::new(ShotApp {
                    ctrl: SettingsController::default(),
                    targets,
                    index: 0,
                    frames_on_target: 0,
                    requested_for_index: None,
                    completed_count: Arc::new(AtomicUsize::new(0)),
                    theme_gen: 1,
                }))
            }),
        );
    }
}
