//! Live wiring helpers for:
//! - Custom keyboard shortcut dispatch (`SettingsController.shortcuts.manager` -> `WindowController`)
//!   and `tdata/shortcuts-custom.json` persistence.
//! - Real top-bar promo suggestion activation (`dialogs/suggestions/*`).
//! - Pre-login Intro Proxy Settings & `AddProxyBox` modal (`IntroAction::OpenSettings` in `live/host.rs`).
//!
//! Port origin:
//! - `Telegram/SourceFiles/core/shortcuts.cpp`
//! - `Telegram/SourceFiles/dialogs/suggestions/suggestion_*.cpp`
//! - `Telegram/SourceFiles/boxes/connection_box.cpp` (`ProxiesBoxController`)

use egui::{
    Align, Area, CornerRadius, Event, Frame, Id, Key, Layout, Order, Pos2, Rect, RichText, Sense,
    Stroke, UiBuilder, Vec2, ViewportCommand,
};
use tg_core::settings_proxy::{
    ProxyData as CoreProxyData, ProxySettingsMode, ProxyType as CoreProxyType, SettingsProxy,
};
use tg_platform::smtc::SmtcCommand;
use tg_theme::{Color as ThemeColor, EmbeddedType};
use tg_ui::stand_in::Colors;

use crate::window::chat_preview::{ChatPreviewData, ChatPreviewMessage};
use crate::window::controller::{RootScreen, WindowController};
use crate::window::layer_stack::ModalBox;
use crate::window::main_widget::{MainWidget, MainWidgetAction};
use crate::window::section::SettingsPage;
use crate::window::session_controller::SessionController;
use tg_app_core::session_api::AppSession;
use tg_app_core::stand_in::backend::Backend;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::{FilterId, PeerId};
use tg_app_history::history::sections_ui::HistorySectionKind;
use tg_app_info::boxes::BoxKind;
use tg_app_info::boxes::add_contact::AddContactBoxState;
use tg_app_info::boxes::add_proxy::AddProxyBoxState;
use tg_app_info::dialogs::DialogsShortcut;
use tg_app_info::dialogs::suggestions::{PromoSuggestion, PromoSuggestionKind};
use tg_app_info::payments::PaymentsScreen;
use tg_app_settings::settings::SettingsSection;
use tg_app_settings::settings::advanced::{ProxyFormDraft, format_proxy_label};
use tg_app_settings::settings::shortcuts::{KeyChord, ShortcutCommand, ShortcutManager};

// =============================================================================
// 1. Global Keyboard Shortcuts Dispatch (`ShortcutManager` -> `WindowController`)
// =============================================================================

/// Collects all normalized [`KeyChord`] key-press events from `ctx` for the current frame.
#[must_use]
pub fn collect_pressed_chords(ctx: &egui::Context) -> Vec<KeyChord> {
    ctx.input(|i| {
        let mut chords = Vec::new();
        for ev in &i.events {
            if let Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = ev
                && let Some(chord) = KeyChord::from_egui_key(*key, *modifiers)
            {
                chords.push(chord);
            }
        }
        chords
    })
}

/// Resolves all [`ShortcutCommand`]s bound in `manager` that match key presses in `ctx`.
#[must_use]
pub fn resolve_triggered_commands(
    manager: &ShortcutManager,
    ctx: &egui::Context,
) -> Vec<ShortcutCommand> {
    let mut commands = Vec::new();
    for chord in collect_pressed_chords(ctx) {
        for cmd in manager.commands_for(&chord) {
            if !commands.contains(&cmd) {
                commands.push(cmd);
            }
        }
    }
    commands
}

/// Dispatches global keyboard shortcuts for `controller` using its active [`ShortcutManager`]
/// (`SettingsController.shortcuts.manager` when initialized, otherwise the window's loaded
/// `tdata/shortcuts-custom.json` manager).
pub fn dispatch_global_shortcuts(
    controller: &mut WindowController,
    ctx: &egui::Context,
    now_ms: i64,
) {
    // While recording a new key chord inside Settings -> Shortcuts, never execute shortcuts.
    if controller.is_recording_shortcut() {
        return;
    }

    let (esc, alt_left, ctrl_tab, shift) = ctx.input(|i| {
        let ctrl = i.modifiers.ctrl || i.modifiers.command;
        (
            i.key_pressed(Key::Escape),
            i.modifiers.alt && i.key_pressed(Key::ArrowLeft),
            ctrl && !i.modifiers.alt && i.key_pressed(Key::Tab),
            i.modifiers.shift,
        )
    });

    let commands = resolve_triggered_commands(controller.active_shortcut_manager(), ctx);

    // 1. Always-available window commands (any RootScreen).
    if commands.contains(&ShortcutCommand::Quit) {
        ctx.send_viewport_cmd(ViewportCommand::Close);
        return;
    }
    if commands.contains(&ShortcutCommand::Close) {
        controller.execute_close_command(ctx);
        return;
    }
    if commands.contains(&ShortcutCommand::Minimize) {
        ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        return;
    }

    if controller.root_screen() != RootScreen::Main {
        return;
    }

    // 2. Lock app (`Shortcuts::Command::Lock`).
    if commands.contains(&ShortcutCommand::Lock) {
        controller.execute_lock_command();
        return;
    }

    // 3. Escape priority mirrors `MainWidget::eventFilter`:
    //    MediaOverlay -> BoxHost -> LayerStack/MainMenu -> TabbedPanel/Autocomplete/Search/SubStack -> SessionController stack.
    if esc {
        controller.execute_escape_command(ctx);
        return;
    }

    // 4. Media viewer fullscreen shortcut works while MediaOverlay is open.
    if controller.is_media_overlay_open() {
        if commands.contains(&ShortcutCommand::MediaViewerFullscreen) {
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
        }
        return;
    }

    // Block background workspace shortcuts when a modal layer or box is open.
    if controller.has_blocking_modal_open() {
        return;
    }

    // 5. Alt+Left Back navigation.
    if alt_left {
        controller.execute_back_navigation();
        return;
    }

    // 6. Execute matched `ShortcutManager` commands.
    if !commands.is_empty() {
        // When both a folder command (`ShowAllChats`/`ShowFolder1..Last`) and a pinned-chat
        // command (`ChatPinned1..8`) share the same default chord (`Ctrl+1..8`), prefer folder
        // switching when custom folders exist (matching tdesktop & `e2e_dialogs.rs`).
        let has_folder_cmd = commands.iter().any(|c| {
            matches!(
                c,
                ShortcutCommand::ShowAllChats
                    | ShortcutCommand::ShowFolder1
                    | ShortcutCommand::ShowFolder2
                    | ShortcutCommand::ShowFolder3
                    | ShortcutCommand::ShowFolder4
                    | ShortcutCommand::ShowFolder5
                    | ShortcutCommand::ShowFolder6
                    | ShortcutCommand::ShowFolderLast
            )
        });
        let has_multiple_folders = controller.session_backend().chat_folders().len() > 1;

        for cmd in &commands {
            let is_pinned_cmd = matches!(
                cmd,
                ShortcutCommand::ChatPinned1
                    | ShortcutCommand::ChatPinned2
                    | ShortcutCommand::ChatPinned3
                    | ShortcutCommand::ChatPinned4
                    | ShortcutCommand::ChatPinned5
                    | ShortcutCommand::ChatPinned6
                    | ShortcutCommand::ChatPinned7
                    | ShortcutCommand::ChatPinned8
            );
            if is_pinned_cmd && has_folder_cmd && has_multiple_folders {
                continue;
            }
            if execute_workspace_command(controller, *cmd, ctx, now_ms) {
                return;
            }
        }
    }

    // 7. Ctrl+Tab / Ctrl+Shift+Tab recent-chat switcher overlay (with NextChat/PrevChat fallback).
    if ctrl_tab {
        let active_peer = controller
            .session()
            .map_or(PeerId::ZERO, SessionController::active_peer);
        let dialogs_for_extras = controller.session_backend().dialogs(FilterId::ALL, "");
        if controller.handle_chat_switch_tab(ctx, &dialogs_for_extras, active_peer) {
            return;
        }
        let sc = if shift {
            DialogsShortcut::PrevChat
        } else {
            DialogsShortcut::NextChat
        };
        controller.dispatch_dialogs_shortcut_action(sc, ctx, now_ms);
    }
}

/// Executes a single workspace [`ShortcutCommand`] against `controller`, returning `true` if handled.
pub fn execute_workspace_command(
    controller: &mut WindowController,
    cmd: ShortcutCommand,
    ctx: &egui::Context,
    now_ms: i64,
) -> bool {
    match cmd {
        ShortcutCommand::Search => {
            let viewing_chat = controller
                .session()
                .is_some_and(|s| s.main_section().is_none() && !s.active_peer().is_zero());
            if viewing_chat
                && !controller
                    .main_widget_mut()
                    .history_mut()
                    .is_chat_search_open()
            {
                controller
                    .main_widget_mut()
                    .history_mut()
                    .open_chat_search();
            } else {
                controller.dispatch_dialogs_shortcut_action(
                    DialogsShortcut::FocusSearch,
                    ctx,
                    now_ms,
                );
            }
            true
        }
        ShortcutCommand::ShowDownloads => {
            controller.open_downloads();
            true
        }
        ShortcutCommand::ShowContacts => {
            controller.push_box_kind(ctx, BoxKind::AddContact(AddContactBoxState::new()));
            true
        }
        ShortcutCommand::ChatSelf => {
            controller.dispatch_dialogs_shortcut_action(
                DialogsShortcut::OpenSavedMessages,
                ctx,
                now_ms,
            );
            true
        }
        ShortcutCommand::ShowArchive => {
            let dialogs = controller.main_widget_mut().dialogs_mut();
            dialogs.cancel_search();
            dialogs.opened_archive = !dialogs.opened_archive;
            dialogs.inner.scroll_to_top();
            true
        }
        ShortcutCommand::ShowAllChats => {
            controller.dispatch_dialogs_shortcut_action(
                DialogsShortcut::SelectFolderSlot(0),
                ctx,
                now_ms,
            );
            true
        }
        ShortcutCommand::ShowFolder1 => {
            controller.dispatch_dialogs_shortcut_action(
                DialogsShortcut::SelectFolderSlot(1),
                ctx,
                now_ms,
            );
            true
        }
        ShortcutCommand::ShowFolder2 => {
            controller.dispatch_dialogs_shortcut_action(
                DialogsShortcut::SelectFolderSlot(2),
                ctx,
                now_ms,
            );
            true
        }
        ShortcutCommand::ShowFolder3 => {
            controller.dispatch_dialogs_shortcut_action(
                DialogsShortcut::SelectFolderSlot(3),
                ctx,
                now_ms,
            );
            true
        }
        ShortcutCommand::ShowFolder4 => {
            controller.dispatch_dialogs_shortcut_action(
                DialogsShortcut::SelectFolderSlot(4),
                ctx,
                now_ms,
            );
            true
        }
        ShortcutCommand::ShowFolder5 => {
            controller.dispatch_dialogs_shortcut_action(
                DialogsShortcut::SelectFolderSlot(5),
                ctx,
                now_ms,
            );
            true
        }
        ShortcutCommand::ShowFolder6 => {
            controller.dispatch_dialogs_shortcut_action(
                DialogsShortcut::SelectFolderSlot(6),
                ctx,
                now_ms,
            );
            true
        }
        ShortcutCommand::ShowFolderLast => {
            controller.dispatch_dialogs_shortcut_action(
                DialogsShortcut::SelectFolderSlot(8),
                ctx,
                now_ms,
            );
            true
        }
        ShortcutCommand::FolderNext | ShortcutCommand::FolderPrevious => {
            let folders = controller.session_backend().chat_folders();
            if folders.len() <= 1 {
                return false;
            }
            let cur = controller
                .session()
                .map_or(FilterId::ALL, SessionController::active_filter);
            let idx = folders.iter().position(|f| f.id == cur).unwrap_or(0);
            let next_idx = if cmd == ShortcutCommand::FolderNext {
                (idx + 1) % folders.len()
            } else {
                (idx + folders.len() - 1) % folders.len()
            };
            controller.dispatch_dialogs_shortcut_action(
                DialogsShortcut::SelectFolderSlot(next_idx),
                ctx,
                now_ms,
            );
            true
        }
        ShortcutCommand::ChatNext => {
            controller.dispatch_dialogs_shortcut_action(DialogsShortcut::NextChat, ctx, now_ms);
            true
        }
        ShortcutCommand::ChatPrevious => {
            controller.dispatch_dialogs_shortcut_action(DialogsShortcut::PrevChat, ctx, now_ms);
            true
        }
        ShortcutCommand::ChatFirst => {
            controller.dispatch_dialogs_shortcut_action(DialogsShortcut::FirstChat, ctx, now_ms);
            true
        }
        ShortcutCommand::ChatLast => {
            controller.dispatch_dialogs_shortcut_action(DialogsShortcut::LastChat, ctx, now_ms);
            true
        }
        ShortcutCommand::ChatPinned1
        | ShortcutCommand::ChatPinned2
        | ShortcutCommand::ChatPinned3
        | ShortcutCommand::ChatPinned4
        | ShortcutCommand::ChatPinned5
        | ShortcutCommand::ChatPinned6
        | ShortcutCommand::ChatPinned7
        | ShortcutCommand::ChatPinned8 => {
            let idx = match cmd {
                ShortcutCommand::ChatPinned1 => 0,
                ShortcutCommand::ChatPinned2 => 1,
                ShortcutCommand::ChatPinned3 => 2,
                ShortcutCommand::ChatPinned4 => 3,
                ShortcutCommand::ChatPinned5 => 4,
                ShortcutCommand::ChatPinned6 => 5,
                ShortcutCommand::ChatPinned7 => 6,
                _ => 7,
            };
            let list = controller.main_widget_mut().dialogs_mut().current_list();
            let slice = controller.session_backend().dialog_slice(list, 0, 8);
            if idx < slice.pinned_count
                && let Some(item) = slice.entries.get(idx)
            {
                let pid = item.peer_id;
                controller.open_peer_history_from_shortcut(pid);
                return true;
            }
            false
        }
        ShortcutCommand::ShowAccount1
        | ShortcutCommand::ShowAccount2
        | ShortcutCommand::ShowAccount3
        | ShortcutCommand::ShowAccount4
        | ShortcutCommand::ShowAccount5
        | ShortcutCommand::ShowAccount6 => {
            let slot = match cmd {
                ShortcutCommand::ShowAccount1 => 0,
                ShortcutCommand::ShowAccount2 => 1,
                ShortcutCommand::ShowAccount3 => 2,
                ShortcutCommand::ShowAccount4 => 3,
                ShortcutCommand::ShowAccount5 => 4,
                _ => 5,
            };
            controller.request_switch_account_by_index(slot)
        }
        ShortcutCommand::ReadChat => {
            if let Some(pid) = controller.active_focused_peer() {
                Backend::mark_read(controller.session_backend_mut(), pid);
                true
            } else {
                false
            }
        }
        ShortcutCommand::ArchiveChat => {
            if let Some(pid) = controller.active_focused_peer() {
                let archived = controller.session_backend_mut().toggle_archive(pid);
                controller.show_toast(if archived {
                    "Chat archived."
                } else {
                    "Chat unarchived."
                });
                true
            } else {
                false
            }
        }
        ShortcutCommand::ShowScheduled => {
            if let Some(pid) = controller.active_focused_peer() {
                controller.main_widget_mut().history_mut().open_section(
                    HistorySectionKind::Scheduled {
                        peer_id: pid,
                        topic_root_id: None,
                    },
                );
                true
            } else {
                false
            }
        }
        ShortcutCommand::ShowAdminLog => {
            if let Some(pid) = controller.active_focused_peer() {
                controller
                    .main_widget_mut()
                    .history_mut()
                    .open_section(HistorySectionKind::AdminLog { peer_id: pid });
                true
            } else {
                false
            }
        }
        ShortcutCommand::ShowChatMenu => {
            if let Some(pid) = controller.active_focused_peer() {
                if let Some(session) = controller.session_mut() {
                    session.toggle_peer_info(pid);
                }
                true
            } else {
                false
            }
        }
        ShortcutCommand::ShowChatPreview => {
            let active_peer = controller
                .session()
                .map_or(PeerId::ZERO, SessionController::active_peer);
            if active_peer.is_zero() {
                return false;
            }
            if controller.shell_extras_mut().chat_preview.is_open() {
                controller.shell_extras_mut().chat_preview.close();
                return true;
            }
            let dialogs = controller.session_backend().dialogs(FilterId::ALL, "");
            if let Some(d) = dialogs.iter().find(|d| d.peer.id == active_peer) {
                let data = ChatPreviewData {
                    peer_id: d.peer.id,
                    title: d.peer.title.clone(),
                    subtitle: d.peer.status.clone(),
                    initials: d.peer.initials.clone(),
                    color_index: tg_ui::userpic::decide_color_index(d.peer.id.bare()),
                    unread_count: d.unread_count,
                    muted: d.peer.muted,
                    messages: vec![ChatPreviewMessage {
                        sender: d.last_sender.clone().unwrap_or_default(),
                        text: d.last_message.clone(),
                        time_text: String::new(),
                        outgoing: false,
                    }],
                };
                controller
                    .shell_extras_mut()
                    .chat_preview
                    .show_data(data, Pos2::new(340.0, 96.0));
                return true;
            }
            false
        }
        ShortcutCommand::ReopenClosedWindow => controller
            .shell_extras_mut()
            .saved_windows
            .reopen_last_closed()
            .is_some(),
        ShortcutCommand::CloseOtherWindows => {
            let peers: Vec<PeerId> = controller
                .shell_extras_mut()
                .saved_windows
                .open_windows()
                .iter()
                .map(|w| PeerId(w.peer_id))
                .collect();
            if peers.is_empty() {
                return false;
            }
            for peer in peers {
                let _ = controller
                    .shell_extras_mut()
                    .saved_windows
                    .close_window(peer);
            }
            true
        }
        ShortcutCommand::MediaPlay => controller
            .main_widget_mut()
            .audio_player_bar_mut()
            .apply_smtc_command(SmtcCommand::Play),
        ShortcutCommand::MediaPause => controller
            .main_widget_mut()
            .audio_player_bar_mut()
            .apply_smtc_command(SmtcCommand::Pause),
        ShortcutCommand::MediaPlayPause => controller
            .main_widget_mut()
            .audio_player_bar_mut()
            .apply_smtc_command(SmtcCommand::PlayPause),
        ShortcutCommand::MediaStop => controller
            .main_widget_mut()
            .audio_player_bar_mut()
            .apply_smtc_command(SmtcCommand::Stop),
        ShortcutCommand::MediaPrevious => controller
            .main_widget_mut()
            .audio_player_bar_mut()
            .apply_smtc_command(SmtcCommand::Previous),
        ShortcutCommand::MediaNext => controller
            .main_widget_mut()
            .audio_player_bar_mut()
            .apply_smtc_command(SmtcCommand::Next),
        _ => false,
    }
}

// =============================================================================
// 2. Promo Suggestions Real Activation Routing (`dialogs/suggestions/*`)
// =============================================================================

/// Routes a clicked top-bar [`PromoSuggestion`] to its real Settings section, Payments/Premium/
/// Stars/Gifts screen, or deep-link/external URL handler (no toast-only placeholders).
pub fn activate_promo_suggestion(
    main_widget: &mut MainWidget,
    suggestion: PromoSuggestion,
    session: &mut SessionController,
    session_backend: &mut dyn AppSession,
    embedded: EmbeddedType,
    accent: Option<ThemeColor>,
    actions: &mut Vec<MainWidgetAction>,
) {
    match suggestion.kind {
        PromoSuggestionKind::UserpicSetup | PromoSuggestionKind::BirthdaySetup => {
            let settings_backend = session_backend.create_settings_backend();
            main_widget.open_settings_section(
                SettingsSection::Information,
                settings_backend,
                embedded,
                accent,
            );
            session.show_settings(SettingsPage::Main);
        }
        PromoSuggestionKind::BirthdayContacts => {
            actions.push(MainWidgetAction::OpenPayments {
                screen: PaymentsScreen::StarGifts,
                slug: None,
            });
        }
        PromoSuggestionKind::PremiumOffer | PromoSuggestionKind::PremiumGrace => {
            actions.push(MainWidgetAction::OpenPayments {
                screen: PaymentsScreen::Premium,
                slug: None,
            });
        }
        PromoSuggestionKind::LowCreditsSubs => {
            actions.push(MainWidgetAction::OpenPayments {
                screen: PaymentsScreen::Stars,
                slug: None,
            });
        }
        PromoSuggestionKind::GiftAuctions => {
            actions.push(MainWidgetAction::OpenPayments {
                screen: PaymentsScreen::StarGifts,
                slug: None,
            });
        }
        PromoSuggestionKind::UnreviewedAuth => {
            let section = if suggestion.key == "UNREVIEWED_AUTH" {
                SettingsSection::ActiveSessions
            } else {
                SettingsSection::PrivacySecurity
            };
            let settings_backend = session_backend.create_settings_backend();
            main_widget.open_settings_section(section, settings_backend, embedded, accent);
            session.show_settings(SettingsPage::Main);
        }
        PromoSuggestionKind::CustomPromo { url } => {
            if !url.trim().is_empty() {
                actions.push(MainWidgetAction::OpenLink(url));
            }
        }
    }
}

// =============================================================================
// 3. Pre-Login Intro Proxy Settings & AddProxyBox Modal (`live/host.rs`)
// =============================================================================

/// Action emitted by [`IntroProxyModal::show`] to [`crate::live::host::LiveAppHost`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntroProxyAction {
    /// Persist and apply updated proxy settings to `AccountManager` / live `MtprotoRpc`.
    Apply(SettingsProxy),
    /// User closed the modal without further changes.
    Closed,
}

/// Pre-login Proxy Settings & `AddProxyBox` modal shown over `IntroWidget` when
/// `IntroAction::OpenSettings` fires before authentication (`live/host.rs`).
#[derive(Debug, Default)]
pub struct IntroProxyModal {
    open: bool,
    settings: SettingsProxy,
    form: ProxyFormDraft,
    add_proxy_box: Option<AddProxyBoxState>,
    status_message: Option<String>,
}

impl IntroProxyModal {
    /// Creates a closed [`IntroProxyModal`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` while the pre-login proxy modal is visible.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Returns the active [`AddProxyBoxState`] confirmation preview, if open.
    #[must_use]
    pub fn add_proxy_box(&self) -> Option<&AddProxyBoxState> {
        self.add_proxy_box.as_ref()
    }

    /// Returns the current edited [`SettingsProxy`] snapshot.
    #[must_use]
    pub const fn settings(&self) -> &SettingsProxy {
        &self.settings
    }

    /// Opens the pre-login proxy settings modal seeded from `current` (e.g. `AccountManager::proxy_settings()`).
    ///
    /// Also initializes an [`AddProxyBoxState`] preview when a valid proxy is already selected so
    /// the user can inspect, add, or switch proxies before signing in.
    pub fn open_with_settings(&mut self, current: SettingsProxy) {
        let selected = current.selected().clone();
        self.add_proxy_box = if selected.is_valid() {
            Some(AddProxyBoxState::from_proxy(selected, "intro://settings"))
        } else {
            Some(AddProxyBoxState::from_proxy(
                CoreProxyData {
                    proxy_type: CoreProxyType::Socks5,
                    host: String::new(),
                    port: 1080,
                    user: String::new(),
                    password: String::new(),
                },
                "intro://settings",
            ))
        };
        self.settings = current;
        self.form = ProxyFormDraft::default();
        self.status_message = None;
        self.open = true;
    }

    /// Opens the pre-login modal directly onto an [`AddProxyBoxState`] parsed from a
    /// `tg://proxy?...` or `tg://socks?...` link. Returns `true` if `url` was a proxy link.
    pub fn open_from_url(&mut self, url: &str, current: SettingsProxy) -> bool {
        let Some(state) = AddProxyBoxState::from_url(url) else {
            return false;
        };
        self.settings = current;
        self.form = ProxyFormDraft {
            proxy_type: state.proxy.proxy_type,
            host: state.proxy.host.clone(),
            port: state.proxy.port.to_string(),
            user: state.proxy.user.clone(),
            password: state.proxy.password.clone(),
            link_input: url.to_owned(),
            error: state.validation_error.clone(),
        };
        self.add_proxy_box = Some(state);
        self.status_message = None;
        self.open = true;
        true
    }

    /// Builds and validates a candidate [`AddProxyBoxState`] from the current form fields.
    pub fn prepare_form_proxy(&mut self) -> Result<AddProxyBoxState, String> {
        let link = self.form.link_input.trim().to_owned();
        if !link.is_empty() {
            let state = AddProxyBoxState::from_url(&link).ok_or_else(|| {
                "Unrecognized proxy link (expected tg://proxy or tg://socks).".to_owned()
            })?;
            if let Some(err) = state.validation_error.clone() {
                self.form.error = Some(err.clone());
                self.add_proxy_box = Some(state);
                return Err(err);
            }
            self.form.proxy_type = state.proxy.proxy_type;
            self.form.host = state.proxy.host.clone();
            self.form.port = state.proxy.port.to_string();
            self.form.user = state.proxy.user.clone();
            self.form.password = state.proxy.password.clone();
            self.form.error = None;
            self.add_proxy_box = Some(state.clone());
            return Ok(state);
        }

        let port = self
            .form
            .port
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|&p| p > 0 && p <= u32::from(u16::MAX))
            .ok_or_else(|| "Port must be between 1 and 65535.".to_owned())?;

        let proxy = CoreProxyData {
            proxy_type: self.form.proxy_type,
            host: self.form.host.trim().to_owned(),
            port,
            user: self.form.user.trim().to_owned(),
            password: self.form.password.trim().to_owned(),
        };
        let state = AddProxyBoxState::from_proxy(proxy, "intro://settings");
        if let Some(err) = state.validation_error.clone() {
            self.form.error = Some(err.clone());
            self.add_proxy_box = Some(state);
            return Err(err);
        }
        self.form.error = None;
        self.add_proxy_box = Some(state.clone());
        Ok(state)
    }

    /// Confirms and enables a validated proxy in `self.settings`, returning [`IntroProxyAction::Apply`].
    pub fn confirm_proxy(&mut self, proxy: CoreProxyData) -> IntroProxyAction {
        if self.settings.index_in_list(&proxy).is_none() {
            self.settings.add_to_list(proxy.clone());
        }
        self.settings.set_selected(proxy.clone());
        self.settings.set_settings(ProxySettingsMode::Enabled);
        self.add_proxy_box = Some(AddProxyBoxState::from_proxy(proxy, "intro://settings"));
        self.status_message = Some("Proxy saved and applied.".to_owned());
        self.open = false;
        IntroProxyAction::Apply(self.settings.clone())
    }

    /// Closes the modal.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Renders the pre-login Proxy Settings & `AddProxyBox` modal over `IntroWidget`.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        colors: &dyn Colors,
    ) -> Option<IntroProxyAction> {
        if !self.open {
            return None;
        }

        let esc = ctx.input(|i| i.key_pressed(Key::Escape));
        if esc {
            self.open = false;
            return Some(IntroProxyAction::Closed);
        }

        let screen = ctx.content_rect();
        let panel_w = (screen.width() - 48.0).clamp(320.0, 440.0);
        let panel_h = (screen.height() - 64.0).clamp(360.0, 560.0);
        let panel_pos = Pos2::new(
            screen.center().x - panel_w * 0.5,
            screen.center().y - panel_h * 0.5,
        );

        let mut action: Option<IntroProxyAction> = None;

        Area::new(Id::unique("tg_intro_proxy_backdrop"))
            .order(Order::Middle)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let (_, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter()
                    .rect_filled(screen, CornerRadius::ZERO, palette.layer_backdrop);
                if resp.clicked() {
                    action = Some(IntroProxyAction::Closed);
                }
            });

        Area::new(Id::unique("tg_intro_proxy_modal"))
            .order(Order::Foreground)
            .fixed_pos(panel_pos)
            .show(ctx, |ui| {
                let panel_rect = Rect::from_min_size(panel_pos, Vec2::new(panel_w, panel_h));
                ui.painter()
                    .rect_filled(panel_rect, CornerRadius::same(10), palette.window_bg);
                ui.painter().rect_stroke(
                    panel_rect,
                    CornerRadius::same(10),
                    Stroke::new(1.0, palette.divider),
                    egui::StrokeKind::Inside,
                );
                let inner = panel_rect.shrink(16.0);
                let footer_h = 36.0;
                let scroll_rect = Rect::from_min_max(
                    inner.min,
                    Pos2::new(inner.max.x, (inner.max.y - footer_h - 8.0).max(inner.min.y)),
                );
                let footer_rect =
                    Rect::from_min_max(Pos2::new(inner.min.x, inner.max.y - footer_h), inner.max);
                ui.scope_builder(UiBuilder::new().max_rect(scroll_rect), |ui| {
                    ui.set_clip_rect(scroll_rect);
                    egui::ScrollArea::vertical()
                        .id_salt("tg_intro_proxy_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new("Connection & Proxy Settings")
                                    .size(17.0)
                                    .strong()
                                    .color(palette.window_bold_fg),
                            );
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new("Configure a proxy before signing in to Telegram.")
                                    .size(12.5)
                                    .color(palette.window_sub_text_fg),
                            );
                            ui.add_space(10.0);

                            // Mode selection
                            let mut mode = self.settings.settings();
                            let prev_mode = mode;
                            ui.horizontal_wrapped(|ui| {
                                ui.radio_value(&mut mode, ProxySettingsMode::Disabled, "Direct");
                                ui.radio_value(
                                    &mut mode,
                                    ProxySettingsMode::System,
                                    "System proxy",
                                );
                                ui.radio_value(
                                    &mut mode,
                                    ProxySettingsMode::Enabled,
                                    "Custom proxy",
                                );
                            });
                            if mode != prev_mode {
                                self.settings.set_settings(mode);
                            }

                            let mut ipv6 = self.settings.try_ipv6();
                            if ui
                                .checkbox(&mut ipv6, "Try connecting through IPv6")
                                .changed()
                            {
                                self.settings.set_try_ipv6(ipv6);
                            }

                            // Saved proxies list
                            let saved: Vec<CoreProxyData> = self.settings.list().to_vec();
                            if !saved.is_empty() {
                                ui.add_space(8.0);
                                ui.separator();
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new("Saved Proxies")
                                        .size(13.0)
                                        .strong()
                                        .color(palette.window_fg),
                                );
                                let selected = self.settings.selected().clone();
                                for proxy in saved {
                                    let is_sel = proxy == selected;
                                    ui.horizontal(|ui| {
                                        if ui
                                            .selectable_label(is_sel, format_proxy_label(&proxy))
                                            .clicked()
                                        {
                                            self.settings.set_selected(proxy.clone());
                                            self.settings.set_settings(ProxySettingsMode::Enabled);
                                            self.add_proxy_box =
                                                Some(AddProxyBoxState::from_proxy(
                                                    proxy.clone(),
                                                    "intro://settings",
                                                ));
                                        }
                                        if ui.small_button("Remove").clicked() {
                                            self.settings.remove_from_list(&proxy);
                                        }
                                    });
                                }
                            }

                            ui.add_space(8.0);
                            ui.separator();
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new("Add Proxy")
                                    .size(13.5)
                                    .strong()
                                    .color(palette.window_fg),
                            );
                            ui.add_space(4.0);

                            // Protocol selector
                            ui.horizontal(|ui| {
                                ui.radio_value(
                                    &mut self.form.proxy_type,
                                    CoreProxyType::Socks5,
                                    "SOCKS5",
                                );
                                ui.radio_value(
                                    &mut self.form.proxy_type,
                                    CoreProxyType::Mtproto,
                                    "MTProto",
                                );
                                ui.radio_value(
                                    &mut self.form.proxy_type,
                                    CoreProxyType::Http,
                                    "HTTP",
                                );
                            });
                            ui.add_space(4.0);

                            ui.horizontal(|ui| {
                                ui.label("Link:");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.form.link_input)
                                        .hint_text("tg://proxy?... or tg://socks?...")
                                        .desired_width(f32::INFINITY),
                                );
                            });
                            ui.horizontal(|ui| {
                                ui.label("Host:");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.form.host)
                                        .hint_text("127.0.0.1")
                                        .desired_width(180.0),
                                );
                                ui.label("Port:");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.form.port)
                                        .hint_text("1080")
                                        .desired_width(64.0),
                                );
                            });
                            match self.form.proxy_type {
                                CoreProxyType::Socks5 | CoreProxyType::Http => {
                                    ui.horizontal(|ui| {
                                        ui.label("User:");
                                        ui.add(
                                            egui::TextEdit::singleline(&mut self.form.user)
                                                .hint_text("optional")
                                                .desired_width(120.0),
                                        );
                                        ui.label("Pass:");
                                        ui.add(
                                            egui::TextEdit::singleline(&mut self.form.password)
                                                .password(true)
                                                .hint_text("optional")
                                                .desired_width(120.0),
                                        );
                                    });
                                }
                                CoreProxyType::Mtproto | CoreProxyType::Web => {
                                    ui.horizontal(|ui| {
                                        ui.label("Secret:");
                                        ui.add(
                                            egui::TextEdit::singleline(&mut self.form.password)
                                                .hint_text("32+ hex chars or base64url")
                                                .desired_width(f32::INFINITY),
                                        );
                                    });
                                }
                                CoreProxyType::None => {}
                            }

                            if let Some(err) = &self.form.error {
                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new(err)
                                        .size(12.0)
                                        .color(colors.color("boxTextFgError")),
                                );
                            } else if let Some(msg) = &self.status_message {
                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new(msg)
                                        .size(12.0)
                                        .color(palette.window_sub_text_fg),
                                );
                            }

                            // Show AddProxyBox preview when a valid proxy is staged.
                            if let Some(ref mut box_state) = self.add_proxy_box
                                && box_state.is_valid()
                            {
                                ui.add_space(8.0);
                                Frame::NONE
                                    .fill(palette.window_bg_over)
                                    .corner_radius(CornerRadius::same(6))
                                    .inner_margin(egui::Margin::same(8))
                                    .show(ui, |ui| {
                                        box_state.show_body(ui, colors);
                                    });
                            }
                        });
                });
                ui.scope_builder(UiBuilder::new().max_rect(footer_rect), |ui| {
                    ui.set_clip_rect(footer_rect);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Connect Proxy").clicked() {
                            match self.prepare_form_proxy() {
                                Ok(valid) => {
                                    action = Some(self.confirm_proxy(valid.proxy));
                                }
                                Err(_) => {
                                    // If form is empty but a valid AddProxyBox or mode was set, apply current settings.
                                    if self.form.host.trim().is_empty()
                                        && self.form.link_input.trim().is_empty()
                                    {
                                        if let Some(staged) = self
                                            .add_proxy_box
                                            .as_ref()
                                            .filter(|b| b.is_valid())
                                            .map(|b| b.proxy.clone())
                                        {
                                            action = Some(self.confirm_proxy(staged));
                                        } else {
                                            self.form.error = None;
                                            self.open = false;
                                            action = Some(IntroProxyAction::Apply(
                                                self.settings.clone(),
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                        ui.add_space(8.0);
                        if ui.button("Save Mode").clicked() {
                            self.open = false;
                            action = Some(IntroProxyAction::Apply(self.settings.clone()));
                        }
                        ui.add_space(8.0);
                        if ui.button("Close").clicked() {
                            self.open = false;
                            action = Some(IntroProxyAction::Closed);
                        }
                    });
                });
            });

        if matches!(action, Some(IntroProxyAction::Closed)) {
            self.open = false;
        }
        action
    }
}

/// Per-frame Intro-screen proxy glue used by `LiveAppHost`.
///
/// Opens `modal` seeded with `open_with` (the caller passes `Some` when the Intro screen emitted
/// `IntroAction::OpenSettings`), renders it, and hands a confirmed [`SettingsProxy`] to `apply`
/// (live: `AccountManager::set_proxy_settings`, which persists `tdata/settingss` and reconnects).
/// Returns the new "proxy enabled" flag for the Intro connection status when settings were applied.
pub fn drive_intro_proxy_modal(
    modal: &mut IntroProxyModal,
    open_with: Option<SettingsProxy>,
    ctx: &egui::Context,
    palette: &Palette,
    colors: &dyn Colors,
    apply: impl FnOnce(SettingsProxy) -> Result<(), String>,
) -> Option<bool> {
    if let Some(current) = open_with {
        modal.open_with_settings(current);
    }
    let Some(IntroProxyAction::Apply(next)) = modal.show(ctx, palette, colors) else {
        return None;
    };
    let enabled = next.is_enabled() || next.is_system();
    if let Err(err) = apply(next) {
        tracing::warn!(error = %err, "failed to apply pre-login proxy settings");
    }
    Some(enabled)
}

/// Ensures `WindowController`'s local passcode lock/setup flow is invoked cleanly by
/// [`ShortcutCommand::Lock`].
pub fn build_default_lock_modal() -> ModalBox {
    ModalBox::PasscodeSetup {
        draft: String::new(),
        error: String::new(),
        has_existing: false,
    }
}
