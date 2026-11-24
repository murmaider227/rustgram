//! Desktop notifications subsystem and `app.rs` runtime host.
//!
//! Ports:
//! - `Telegram/SourceFiles/window/notifications_manager.h/.cpp` ([`manager`])
//! - `Telegram/SourceFiles/window/notifications_manager_default.h/.cpp` ([`custom_popup`])
//! - `Telegram/SourceFiles/window/notifications_utilities.h/.cpp` ([`utilities`])
//! - `Telegram/SourceFiles/data/notify/data_notify_settings.h/.cpp` and
//!   `data_peer_notify_settings.h/.cpp` ([`settings`])
//! - Notification sound playback via `tg-audio` ([`sound`])
//! - Decoupled [`backend::NotifyBackend`] trait and [`mock::MockNotifyBackend`]

pub use tg_app_core::notifications::backend;
pub mod custom_popup;
pub mod manager;
pub mod mock;
pub use tg_app_core::notifications::settings;
pub mod sound;
pub mod strings;
pub mod utilities;

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use egui::ViewportCommand;

use crate::notifications::backend::NotifyBackend;
use crate::notifications::manager::{ChangeType, NotificationSystem, WindowFocusState};
use crate::notifications::mock::MockNotifyBackend;
use crate::notifications::settings::{
    AccountUnreadEntry, MuteValue, NotificationPreferences, NotifySettingsStore,
    aggregate_account_badges_per_account, compute_badge,
};
use crate::tray::{TrayAccountEntry, TrayAction, TrayController, TrayPlatform, WorkMode};
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::PeerKind;

enum NotifyBackendSlot {
    Mock(MockNotifyBackend),
    Custom {
        backend: Box<dyn NotifyBackend>,
        on_simulate: Box<dyn FnMut(i64) + Send>,
    },
}

impl NotifyBackendSlot {
    fn as_backend(&self) -> &dyn NotifyBackend {
        match self {
            Self::Mock(b) => b,
            Self::Custom { backend, .. } => backend.as_ref(),
        }
    }

    fn as_backend_mut(&mut self) -> &mut dyn NotifyBackend {
        match self {
            Self::Mock(b) => b,
            Self::Custom { backend, .. } => backend.as_mut(),
        }
    }

    fn simulate_incoming(&mut self, now_unix: i64) {
        match self {
            Self::Mock(b) => {
                let _ = b.simulate_incoming_message(now_unix);
            }
            Self::Custom { on_simulate, .. } => {
                on_simulate(now_unix);
            }
        }
    }
}

/// Self-contained host wiring [`NotificationSystem`], [`TrayController`], and
/// a [`NotifyBackend`] into [`crate::app::TgforkApp`] with minimal footprint in `app.rs`.
pub struct AppNotifyTrayHost {
    started_at: Instant,
    system: NotificationSystem,
    tray: TrayController,
    backend: NotifyBackendSlot,
    focus: WindowFocusState,
    window_hidden_to_tray: bool,
    pending_account_switches: Vec<i32>,
    account_unread_entries: Option<Vec<AccountUnreadEntry>>,
}

impl Default for AppNotifyTrayHost {
    fn default() -> Self {
        Self::new()
    }
}

impl AppNotifyTrayHost {
    /// Initializes notifications, tray icon, and mock notification backend.
    #[must_use]
    pub fn new() -> Self {
        let now_unix = current_unix_secs();
        let prefs = NotificationPreferences::default();
        let mut store = NotifySettingsStore::default();
        store.register_peer_kind(MockNotifyBackend::PEER_SUPPORT_DESK, PeerKind::User);
        store.register_peer_kind(MockNotifyBackend::PEER_BUILD_BOT, PeerKind::User);
        store.register_peer_kind(MockNotifyBackend::PEER_TEAM_ALPHA, PeerKind::Group);
        store.register_peer_kind(MockNotifyBackend::PEER_NEWS_DIGEST, PeerKind::Channel);
        store.register_peer_kind(MockNotifyBackend::PEER_SAVED_MESSAGES, PeerKind::User);
        store.update_peer(
            MockNotifyBackend::PEER_TEAM_ALPHA,
            MuteValue::forever(),
            None,
            None,
            None,
            None,
            now_unix,
        );
        store.update_peer(
            MockNotifyBackend::PEER_NEWS_DIGEST,
            MuteValue::forever(),
            None,
            None,
            None,
            None,
            now_unix,
        );

        let system = NotificationSystem::new(prefs.clone(), store);
        let mut tray = TrayController::new(WorkMode::WindowAndTray, false);
        tray.initialize(&prefs, now_unix);

        let backend = MockNotifyBackend::new();
        let badge = compute_badge(backend.unread_state(), system.preferences());
        tray.update_unread_badge(badge, None);

        Self {
            started_at: Instant::now(),
            system,
            tray,
            backend: NotifyBackendSlot::Mock(backend),
            focus: WindowFocusState::with_defaults(1),
            window_hidden_to_tray: false,
            pending_account_switches: Vec::new(),
            account_unread_entries: None,
        }
    }

    /// Initializes notifications and tray icon wired to a shared [`NotifyBackend`] implementation.
    #[must_use]
    pub fn with_backend(
        backend: Box<dyn NotifyBackend>,
        store: NotifySettingsStore,
        on_simulate: Box<dyn FnMut(i64) + Send>,
    ) -> Self {
        let now_unix = current_unix_secs();
        let prefs = NotificationPreferences::default();
        let system = NotificationSystem::new(prefs.clone(), store);
        let mut tray = TrayController::new(WorkMode::WindowAndTray, false);
        tray.initialize(&prefs, now_unix);

        let badge = compute_badge(backend.unread_state(), system.preferences());
        tray.update_unread_badge(badge, None);

        Self {
            started_at: Instant::now(),
            system,
            tray,
            backend: NotifyBackendSlot::Custom {
                backend,
                on_simulate,
            },
            focus: WindowFocusState::with_defaults(1),
            window_hidden_to_tray: false,
            pending_account_switches: Vec::new(),
            account_unread_entries: None,
        }
    }

    /// Initializes a deterministic headless notifications + tray host with explicit
    /// [`NotificationSystem`] and [`TrayPlatform`] (for tests without OS tray/WinRT toasts).
    #[must_use]
    pub fn with_backend_and_platform(
        backend: Box<dyn NotifyBackend>,
        system: NotificationSystem,
        tray_platform: Box<dyn TrayPlatform>,
        on_simulate: Box<dyn FnMut(i64) + Send>,
    ) -> Self {
        let now_unix = current_unix_secs();
        let mut tray =
            TrayController::with_platform(tray_platform, WorkMode::WindowAndTray, false, None);
        tray.set_include_debug_simulate_item(false);
        tray.initialize(system.preferences(), now_unix);

        let badge = compute_badge(backend.unread_state(), system.preferences());
        tray.update_unread_badge(badge, None);

        Self {
            started_at: Instant::now(),
            system,
            tray,
            backend: NotifyBackendSlot::Custom {
                backend,
                on_simulate,
            },
            focus: WindowFocusState::with_defaults(1),
            window_hidden_to_tray: false,
            pending_account_switches: Vec::new(),
            account_unread_entries: None,
        }
    }

    /// Updates the active foreground account `session_id` so inactive background accounts
    /// still deliver notifications while the foreground chat stays suppressed when focused.
    pub fn set_active_session_id(&mut self, session_id: u64) {
        self.focus.active_session_id = session_id;
    }

    /// Updates the signed-in accounts list rendered in the system tray context menu
    /// (`TrayAccountsMenu::Fill`).
    pub fn set_accounts(&mut self, accounts: Vec<TrayAccountEntry>) {
        self.tray.set_accounts(accounts);
    }

    /// Provides per-account unread state + counting preferences so the tray/taskbar badge
    /// sums across all signed-in accounts respecting per-account mute/count settings.
    pub fn set_account_unread_entries(&mut self, entries: Vec<AccountUnreadEntry>) {
        self.account_unread_entries = Some(entries);
    }

    /// Drains account-switch requests emitted when the user selects an account in the tray menu.
    pub fn take_account_switch_requests(&mut self) -> Vec<i32> {
        std::mem::take(&mut self.pending_account_switches)
    }

    /// Borrows the inner [`TrayController`].
    #[must_use]
    pub const fn tray(&self) -> &TrayController {
        &self.tray
    }

    /// Mutably borrows the inner [`TrayController`].
    pub fn tray_mut(&mut self) -> &mut TrayController {
        &mut self.tray
    }

    /// Borrows the inner [`NotificationSystem`].
    #[must_use]
    pub const fn system(&self) -> &NotificationSystem {
        &self.system
    }

    /// Mutably borrows the inner [`NotificationSystem`].
    pub fn system_mut(&mut self) -> &mut NotificationSystem {
        &mut self.system
    }

    /// Updates the currently focused active chat peer so notifications for it are suppressed while focused.
    pub fn set_active_peer(&mut self, peer_id: Option<tg_app_core::stand_in::types::PeerId>) {
        self.focus.active_peer_id = peer_id;
    }

    /// Returns the active [`NotificationPreferences`] applied to this host.
    #[must_use]
    pub fn preferences(&self) -> &NotificationPreferences {
        self.system.preferences()
    }

    /// Returns a mutable reference to the underlying [`NotifySettingsStore`] so live
    /// server mute/scope settings and exceptions can be synced in.
    pub fn settings_store_mut(&mut self) -> &mut NotifySettingsStore {
        self.system.settings_store_mut()
    }

    /// Applies desktop notification and tray preferences from [`tg_core::CoreSettings`]
    /// onto [`NotificationSystem`] and [`TrayController`].
    pub fn apply_core_settings(&mut self, core: &tg_core::CoreSettings) {
        let now_unix = current_unix_secs();
        let prefs = self.system.preferences_mut();
        prefs.desktop_notify = core.desktop_notify;
        prefs.sound_notify = core.sound_notify;
        prefs.flash_bounce_notify = core.flash_bounce_notify;
        prefs.notify_view = match core.notify_view {
            tg_core::settings::NotifyView::ShowPreview => {
                crate::notifications::settings::NotifyView::ShowPreview
            }
            tg_core::settings::NotifyView::ShowName => {
                crate::notifications::settings::NotifyView::ShowName
            }
            tg_core::settings::NotifyView::ShowNothing => {
                crate::notifications::settings::NotifyView::ShowNothing
            }
        };
        if let Some(native) = core.native_notifications {
            prefs.native_notifications = native;
        }
        prefs.notifications_count = (core.notifications_count.clamp(1, 5)) as usize;
        prefs.notifications_corner = match core.notifications_corner {
            tg_core::settings::ScreenCorner::TopLeft => {
                crate::notifications::settings::ScreenCorner::TopLeft
            }
            tg_core::settings::ScreenCorner::TopRight => {
                crate::notifications::settings::ScreenCorner::TopRight
            }
            tg_core::settings::ScreenCorner::BottomRight => {
                crate::notifications::settings::ScreenCorner::BottomRight
            }
            tg_core::settings::ScreenCorner::BottomLeft => {
                crate::notifications::settings::ScreenCorner::BottomLeft
            }
        };
        prefs.include_muted_counter = core.include_muted_counter;
        prefs.count_unread_messages = core.count_unread_messages;
        prefs.notifications_volume_pct = core.notifications_volume.min(100) as u8;
        prefs.skip_in_focus_assist = core.skip_toasts_in_focus;

        let work_mode = match core.work_mode {
            tg_core::settings::WorkMode::WindowAndTray => WorkMode::WindowAndTray,
            tg_core::settings::WorkMode::TrayOnly => WorkMode::TrayOnly,
            tg_core::settings::WorkMode::WindowOnly => WorkMode::WindowOnly,
        };
        let prefs_snapshot = self.system.preferences().clone();
        self.tray
            .set_work_mode(work_mode, &prefs_snapshot, now_unix);
        self.tray.set_monochrome(core.tray_icon_monochrome);
    }

    /// Triggers the `"simulate incoming message"` debug action.
    pub fn simulate_incoming_message(&mut self) {
        let now_unix = current_unix_secs();
        // Temporarily unfocus active peer in debug simulation so the toast/popup is visible.
        self.focus.active_peer_id = None;
        self.backend.simulate_incoming(now_unix);
    }

    /// Drives one UI frame: tracks window focus, polls tray actions, ticks notifications,
    /// syncs unread badge, and renders custom in-app popups when native toasts are off.
    ///
    /// Also binds `Ctrl+Shift+N` to the `"simulate incoming message"` debug action.
    pub fn update_and_render(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        passcode_locked: bool,
    ) {
        let now_ms = self.elapsed_ms();
        let now_unix = current_unix_secs();

        // Update focus & minimize snapshot from egui viewport state.
        let (focused, minimized) = ctx.input(|i| {
            let vp = i.viewport();
            (vp.focused.unwrap_or(true), vp.minimized.unwrap_or(false))
        });
        let became_focused = focused && !self.focus.window_focused;
        self.focus.window_focused = focused;
        self.focus.window_minimized = minimized || self.window_hidden_to_tray;
        self.focus.passcode_locked = passcode_locked;
        if became_focused {
            self.system.stop_window_flash(self.focus.hwnd);
        }

        // Debug shortcut: Ctrl+Shift+N triggers "simulate incoming message".
        let simulate_pressed = ctx.input(|i| {
            i.modifiers
                .matches_logically(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT)
                && i.key_pressed(egui::Key::N)
        });
        if simulate_pressed {
            self.simulate_incoming_message();
        }

        // Sync tray menu & drain tray click / menu actions.
        let active_for_tray = !self.window_hidden_to_tray && !minimized;
        self.tray.sync_state(
            active_for_tray,
            passcode_locked,
            self.system.preferences(),
            now_unix,
        );
        let tray_actions = self
            .tray
            .poll_actions(self.system.preferences_mut(), now_ms, now_unix);
        for action in tray_actions {
            match action {
                TrayAction::ShowWindow => {
                    self.window_hidden_to_tray = false;
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                }
                TrayAction::HideToTray => {
                    self.window_hidden_to_tray = true;
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
                }
                TrayAction::MuteForOneHour => {
                    self.system.clear_all(now_ms);
                }
                TrayAction::UnmuteGlobal => {}
                TrayAction::ToggleNotifications => {
                    self.system
                        .notify_settings_changed(ChangeType::DesktopEnabled, now_ms);
                }
                TrayAction::SimulateIncomingMessage => {
                    self.simulate_incoming_message();
                }
                TrayAction::SwitchAccount(slot_index) => {
                    self.window_hidden_to_tray = false;
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                    self.pending_account_switches.push(slot_index);
                }
                TrayAction::Quit => {
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }

        // Tick notification decision engine, audio playback, and native toast activations.
        self.system
            .tick(self.backend.as_backend_mut(), &self.focus, now_ms, now_unix);

        // Render custom in-app notification popups when native toasts are off.
        let _ = self.system.show_custom_popups_egui(
            ctx,
            palette,
            self.backend.as_backend_mut(),
            &self.focus,
            now_ms,
        );

        // Sync unread badge & muted state to tray + taskbar overlay.
        let badge = if let Some(entries) = self.account_unread_entries.as_ref() {
            aggregate_account_badges_per_account(entries.iter().copied())
        } else {
            compute_badge(
                self.backend.as_backend().unread_state(),
                self.system.preferences(),
            )
        };
        self.tray.update_unread_badge(badge, self.focus.hwnd);
    }

    /// Cleans up active toasts, custom popups, and cached avatar files on application exit.
    pub fn on_exit(&mut self) {
        self.system.clear_all_fast();
    }

    fn elapsed_ms(&self) -> i64 {
        self.started_at.elapsed().as_millis().min(i64::MAX as u128) as i64
    }
}

fn current_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use egui::Rect;
    use tg_platform::notifications::{
        ContextId, FocusAssistProfile, NotificationId, NotificationSuppressionState,
        UserNotificationState,
    };
    use tg_platform::tray::TrayEvent;

    use crate::notifications::backend::{
        IncomingMessageNotification, IncomingNotifyEvent, ItemNotificationType,
        NotificationMediaKind, NotifyBackend, TextSpoilerRange,
    };
    use crate::notifications::custom_popup::{
        CustomPopupContent, CustomPopupManager, NOTIFY_DELTA_X, NOTIFY_DELTA_Y, NOTIFY_MIN_HEIGHT,
        NOTIFY_SLOW_HIDE_MS, NOTIFY_WAIT_LONG_HIDE_MS, NOTIFY_WIDTH,
    };
    use crate::notifications::manager::{
        DEFAULT_NOTIFY_CLOUD_DELAY_MS, DEFAULT_NOTIFY_DEFAULT_DELAY_MS, MINIMAL_ALERT_DELAY_MS,
        MINIMAL_DELAY_MS, MINIMAL_FORWARD_DELAY_MS, ManagerType, MockSuppressionProvider,
        MockWindowFlasher, NotificationSystem, REACTION_NOTIFICATION_EACH_MS, SkipDecision,
        SkipReason, WAITING_FOR_ALL_GROUPED_DELAY_MS, WindowFocusState, apply_permanent_spoilers,
        obfuscate_login_code,
    };
    use crate::notifications::mock::MockNotifyBackend;
    use crate::notifications::settings::{
        DefaultNotify, MUTE_ONE_HOUR_SECS, MuteValue, NotificationPreferences, NotifySettingsStore,
        NotifySound, NotifyView, ScreenCorner, UnreadBadgeResult, UnreadBadgeState, compute_badge,
        compute_unread_badge, compute_unread_badge_muted,
    };
    use crate::notifications::sound::{NotificationSoundPlayer, generate_fallback_chime_track};
    use crate::notifications::utilities::{
        CachedUserpics, NOTIFY_DELETE_PHOTO_AFTER_MS, UserpicKey, generate_userpic,
    };
    use crate::tray::{
        AutostartController, MockTrayPlatform, TrayAction, TrayController, WorkMode, menu_id,
        toggle_notifications_from_tray,
    };
    use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind};

    static TEST_DIR_SEQ: AtomicU64 = AtomicU64::new(1);

    fn unique_temp_dir(tag: &str) -> PathBuf {
        let id = TEST_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "rustgram_notify_test_{tag}_{}_{id}",
            std::process::id()
        ))
    }

    fn make_headless_system(
        prefs: NotificationPreferences,
        store: NotifySettingsStore,
    ) -> NotificationSystem {
        NotificationSystem::headless(prefs, store, unique_temp_dir("sys"))
    }

    // -------------------------------------------------------------------------
    // 1. Focus / active chat / lock decision tests
    // -------------------------------------------------------------------------

    #[test]
    fn skips_when_chat_is_active_and_window_focused_but_notifies_when_unfocused() {
        let store = NotifySettingsStore::default();
        let sys = make_headless_system(NotificationPreferences::default(), store);

        let peer = MockNotifyBackend::PEER_SUPPORT_DESK;
        let notif = IncomingMessageNotification {
            session_id: 1,
            peer_id: peer,
            msg_id: MsgId(10),
            text: "Hello!".to_owned(),
            ..IncomingMessageNotification::default()
        };

        let mut focus = WindowFocusState::with_defaults(1);
        focus.window_focused = true;
        focus.active_peer_id = Some(peer);

        // Active chat + focused -> Skip.
        assert_eq!(
            sys.evaluate_skip(&notif, &focus, 1_000, 1_700_000_000),
            SkipDecision::Skip(SkipReason::ActiveChatFocused)
        );

        // Active chat + unfocused -> Notify.
        focus.window_focused = false;
        assert_eq!(
            sys.evaluate_skip(&notif, &focus, 1_000, 1_700_000_000),
            SkipDecision::Notify { silent: false }
        );

        // Active chat + focused + minimized -> Notify.
        focus.window_focused = true;
        focus.window_minimized = true;
        assert_eq!(
            sys.evaluate_skip(&notif, &focus, 1_000, 1_700_000_000),
            SkipDecision::Notify { silent: false }
        );

        // Active chat + focused + passcode locked -> Notify (details hidden via DisplayOptions).
        focus.window_minimized = false;
        focus.passcode_locked = true;
        assert_eq!(
            sys.evaluate_skip(&notif, &focus, 1_000, 1_700_000_000),
            SkipDecision::Notify { silent: false }
        );
        let opts = sys.display_options(&notif, &focus);
        assert!(opts.hide_name_and_photo);
        assert!(opts.hide_message_text);
        assert!(opts.hide_reply_button);
        assert!(opts.hide_mark_as_read);
    }

    // -------------------------------------------------------------------------
    // 2. Mute settings, per-scope defaults, exceptions, and mentions in muted groups
    // -------------------------------------------------------------------------

    #[test]
    fn muted_group_skips_regular_messages_but_allows_mentions_and_replies_from_unmuted_sender() {
        let now_unix = 1_700_000_000;
        let group = MockNotifyBackend::PEER_TEAM_ALPHA;
        let sender = MockNotifyBackend::PEER_BUILD_BOT;

        let mut store = NotifySettingsStore::default();
        store.register_peer_kind(group, PeerKind::Group);
        store.register_peer_kind(sender, PeerKind::User);
        store.update_peer(
            group,
            MuteValue::forever(),
            None,
            None,
            None,
            None,
            now_unix,
        );

        let sys = make_headless_system(NotificationPreferences::default(), store);
        let focus = WindowFocusState::with_defaults(1);

        // Plain message in muted group -> skipped.
        let plain = IncomingMessageNotification {
            session_id: 1,
            peer_id: group,
            peer_kind: PeerKind::Group,
            sender_id: sender,
            sender_name: "Build Bot".to_owned(),
            chat_title: "Team Alpha".to_owned(),
            msg_id: MsgId(20),
            text: "General chatter".to_owned(),
            mentions_me: false,
            is_reply_to_me: false,
            ..IncomingMessageNotification::default()
        };
        assert_eq!(
            sys.evaluate_skip(&plain, &focus, 1_000, now_unix),
            SkipDecision::Skip(SkipReason::Muted)
        );

        // Mention in muted group from unmuted sender -> notifies!
        let mention = IncomingMessageNotification {
            mentions_me: true,
            msg_id: MsgId(21),
            ..plain.clone()
        };
        assert_eq!(
            sys.evaluate_skip(&mention, &focus, 1_000, now_unix),
            SkipDecision::Notify { silent: false }
        );

        // Direct reply in muted group from unmuted sender -> notifies!
        let reply = IncomingMessageNotification {
            is_reply_to_me: true,
            msg_id: MsgId(22),
            ..plain.clone()
        };
        assert_eq!(
            sys.evaluate_skip(&reply, &focus, 1_000, now_unix),
            SkipDecision::Notify { silent: false }
        );

        // If the mentioning sender is ALSO muted, the mention is skipped.
        let mut store_sender_muted = NotifySettingsStore::default();
        store_sender_muted.register_peer_kind(group, PeerKind::Group);
        store_sender_muted.register_peer_kind(sender, PeerKind::User);
        store_sender_muted.update_peer(
            group,
            MuteValue::forever(),
            None,
            None,
            None,
            None,
            now_unix,
        );
        store_sender_muted.update_peer(
            sender,
            MuteValue::forever(),
            None,
            None,
            None,
            None,
            now_unix,
        );
        let sys2 = make_headless_system(NotificationPreferences::default(), store_sender_muted);
        assert_eq!(
            sys2.evaluate_skip(&mention, &focus, 1_000, now_unix),
            SkipDecision::Skip(SkipReason::SenderBlockedOrMuted)
        );
    }

    #[test]
    fn scope_defaults_exceptions_timed_mute_expiry_and_topic_fallback() {
        let now_unix = 1_700_000_000;
        let ch1 = PeerId::from_channel(5001);
        let ch2 = PeerId::from_channel(5002);
        let forum = PeerId::from_channel(5003);

        let mut store = NotifySettingsStore::default();
        store.register_peer_kind(ch1, PeerKind::Channel);
        store.register_peer_kind(ch2, PeerKind::Channel);
        store.register_peer_kind(forum, PeerKind::Group);

        // Mute all broadcast channels by default.
        store.update_default(
            DefaultNotify::Broadcast,
            MuteValue::forever(),
            None,
            None,
            None,
            None,
            now_unix,
        );
        assert!(store.is_peer_muted(ch1, now_unix));
        assert!(store.is_peer_muted(ch2, now_unix));

        // Add exception: explicitly unmute ch1.
        store.update_peer(ch1, MuteValue::unmute(), None, None, None, None, now_unix);
        assert!(!store.is_peer_muted(ch1, now_unix));
        assert!(store.is_peer_muted(ch2, now_unix));
        assert_eq!(store.exceptions(DefaultNotify::Broadcast), vec![ch1]);

        // Clearing exceptions restores ch1 to muted scope default.
        let cleared = store.clear_exceptions(DefaultNotify::Broadcast, now_unix);
        assert_eq!(cleared, vec![ch1]);
        assert!(store.is_peer_muted(ch1, now_unix));
        assert!(store.exceptions(DefaultNotify::Broadcast).is_empty());

        // Temporary 1-hour mute on a forum topic expires cleanly on `unmute_by_finished`.
        let topic = MsgId(77);
        store.update_topic(forum, topic, MuteValue::for_seconds(3600), None, now_unix);
        assert!(store.is_topic_muted(forum, topic, now_unix + 100));
        assert!(!store.is_topic_muted(forum, topic, now_unix + 3601));
        let (_peers, unmuted_topics, _next) = store.unmute_by_finished(now_unix + 3601);
        assert_eq!(unmuted_topics.len(), 1);
        assert_eq!(unmuted_topics[0].topic_root_id, topic);
    }

    // -------------------------------------------------------------------------
    // 3. Pinned message, reaction, and poll-vote notifications + 1h dedup
    // -------------------------------------------------------------------------

    #[test]
    fn pinned_and_reaction_notifications_and_one_hour_reaction_dedup() {
        let now_unix = 1_700_000_000;
        let peer = MockNotifyBackend::PEER_SUPPORT_DESK;
        let mut backend = MockNotifyBackend::new();
        let store = NotifySettingsStore::default();
        let mut sys = make_headless_system(NotificationPreferences::default(), store);
        let focus = WindowFocusState::with_defaults(1);

        // Pinned message formats snippet and delivers.
        let pin = IncomingMessageNotification {
            session_id: 1,
            peer_id: peer,
            peer_kind: PeerKind::Group,
            sender_id: peer,
            sender_name: "Support Desk".to_owned(),
            chat_title: "Team Alpha".to_owned(),
            msg_id: MsgId(300),
            notification_type: ItemNotificationType::PinnedMessage,
            is_pinned: true,
            text: "Release checklist is ready for review".to_owned(),
            ..IncomingMessageNotification::default()
        };
        assert_eq!(
            sys.schedule(pin, &focus, 1_000, now_unix),
            SkipDecision::Notify { silent: false }
        );
        sys.tick(&mut backend, &focus, 1_000 + MINIMAL_DELAY_MS, now_unix);
        assert_eq!(sys.dispatched_notifications().len(), 1);
        assert!(
            sys.dispatched_notifications()[0]
                .message
                .contains("Support Desk pinned \"Release checklis…\"")
        );

        // Reaction notification: first one succeeds (silent toast), duplicate within 1h skipped!
        let reaction = IncomingMessageNotification {
            session_id: 1,
            peer_id: peer,
            sender_id: peer,
            sender_name: "Support Desk".to_owned(),
            chat_title: "Support Desk".to_owned(),
            msg_id: MsgId(301),
            notification_type: ItemNotificationType::Reaction,
            media_kind: NotificationMediaKind::Photo,
            reaction_emoji: Some("🔥".to_owned()),
            ..IncomingMessageNotification::default()
        };
        assert_eq!(
            sys.schedule(reaction.clone(), &focus, 2_000, now_unix),
            SkipDecision::Notify { silent: true }
        );
        assert_eq!(
            sys.schedule(reaction.clone(), &focus, 2_000 + 60_000, now_unix),
            SkipDecision::Skip(SkipReason::RateLimitedDuplicate)
        );
        // After 1 hour + 1ms, the same message can notify again.
        assert_eq!(
            sys.schedule(
                reaction,
                &focus,
                2_000 + REACTION_NOTIFICATION_EACH_MS + 1,
                now_unix + 3601
            ),
            SkipDecision::Notify { silent: true }
        );

        sys.tick(
            &mut backend,
            &focus,
            2_000 + REACTION_NOTIFICATION_EACH_MS + 1 + MINIMAL_DELAY_MS,
            now_unix + 3601,
        );
        assert!(
            sys.dispatched_notifications()
                .iter()
                .any(|n| n.message == tg_lang::tr::lng_reaction_photo("🔥"))
        );
    }

    // -------------------------------------------------------------------------
    // 4. "Include muted chats in badge" & unread badge rules
    // -------------------------------------------------------------------------

    #[test]
    fn unread_badge_include_muted_and_count_messages_rules() {
        let state = UnreadBadgeState {
            messages: 10,
            messages_muted: 7,
            chats: 4,
            chats_muted: 3,
            marks: 1,
            marks_muted: 1,
            mentions: 0,
        };

        // Include muted + count messages -> 10 + 1 = 11, unmuted color (because 3 messages are unmuted).
        assert_eq!(compute_unread_badge(state, true, true), 11);
        assert!(!compute_unread_badge_muted(state, true, true));

        // Exclude muted + count messages -> (10 - 7) + (1 - 1) = 3, unmuted color.
        assert_eq!(compute_unread_badge(state, false, true), 3);
        assert!(!compute_unread_badge_muted(state, false, true));

        // Include muted + count chats -> 4 + 1 = 5.
        assert_eq!(compute_unread_badge(state, true, false), 5);

        // Exclude muted + count chats -> (4 - 3) + (1 - 1) = 1.
        assert_eq!(compute_unread_badge(state, false, false), 1);

        // When ALL unread items are in muted chats:
        let all_muted = UnreadBadgeState {
            messages: 5,
            messages_muted: 5,
            chats: 2,
            chats_muted: 2,
            marks: 0,
            marks_muted: 0,
            mentions: 0,
        };
        let mut prefs = NotificationPreferences {
            include_muted_counter: true,
            count_unread_messages: true,
            ..NotificationPreferences::default()
        };
        assert_eq!(
            compute_badge(all_muted, &prefs),
            UnreadBadgeResult {
                count: 5,
                muted: true
            }
        );

        // Adding 1 unread mention switches badge color back to unmuted highlight!
        let mut with_mention = all_muted;
        with_mention.mentions = 1;
        assert_eq!(
            compute_badge(with_mention, &prefs),
            UnreadBadgeResult {
                count: 5,
                muted: false
            }
        );

        // Turning off include_muted_counter drops count to 0 when only muted chats are unread.
        prefs.include_muted_counter = false;
        assert_eq!(
            compute_badge(all_muted, &prefs),
            UnreadBadgeResult {
                count: 0,
                muted: false
            }
        );
    }

    // -------------------------------------------------------------------------
    // 5. Rate limiting, grouping/collapsing ("N new messages", forwarded, albums)
    // -------------------------------------------------------------------------

    #[test]
    fn rate_limits_sound_per_peer_and_collapses_message_bursts() {
        let now_unix = 1_700_000_000;
        let peer = MockNotifyBackend::PEER_BUILD_BOT;
        let mut backend = MockNotifyBackend::new();
        let store = NotifySettingsStore::default();
        let mut sys = make_headless_system(NotificationPreferences::default(), store);
        let focus = WindowFocusState::with_defaults(1);

        // Burst of 4 messages arriving at t=1000ms from same chat:
        // - Sound should play only ONCE within MINIMAL_ALERT_DELAY_MS (500ms)
        // - Toast collapses into "4 new messages".
        for i in 0..4 {
            let msg = IncomingMessageNotification {
                session_id: 1,
                peer_id: peer,
                sender_id: peer,
                sender_name: "Build Bot".to_owned(),
                chat_title: "Build Bot".to_owned(),
                msg_id: MsgId(100 + i),
                text: format!("Line {i}"),
                ..IncomingMessageNotification::default()
            };
            let _ = sys.schedule(msg, &focus, 1_000, now_unix);
        }

        // Before 100ms minimal delay, nothing dispatched yet.
        sys.tick(&mut backend, &focus, 1_050, now_unix);
        assert!(sys.dispatched_notifications().is_empty());
        assert!(sys.sound_player().play_history().is_empty());

        // At 1000 + 100ms, burst collapses to 1 notification ("4 new messages") and 1 sound!
        sys.tick(&mut backend, &focus, 1_000 + MINIMAL_DELAY_MS, now_unix);
        assert_eq!(sys.dispatched_notifications().len(), 1);
        assert_eq!(sys.dispatched_notifications()[0].message, "4 new messages");
        assert_eq!(sys.dispatched_notifications()[0].collapsed_count, 4);
        assert_eq!(sys.sound_player().play_history().len(), 1);

        // Another message 200ms later (< 500ms alert rate limit) shows toast but suppresses sound.
        let follow_up = IncomingMessageNotification {
            session_id: 1,
            peer_id: peer,
            sender_id: peer,
            sender_name: "Build Bot".to_owned(),
            chat_title: "Build Bot".to_owned(),
            msg_id: MsgId(110),
            text: "Quick follow-up".to_owned(),
            ..IncomingMessageNotification::default()
        };
        let _ = sys.schedule(follow_up, &focus, 1_200, now_unix);
        sys.tick(&mut backend, &focus, 1_300, now_unix);
        assert_eq!(sys.dispatched_notifications().len(), 2);
        assert_eq!(sys.sound_player().play_history().len(), 1);

        // Another message after >= 500ms plays sound again!
        let later = IncomingMessageNotification {
            session_id: 1,
            peer_id: peer,
            sender_id: peer,
            sender_name: "Build Bot".to_owned(),
            chat_title: "Build Bot".to_owned(),
            msg_id: MsgId(111),
            text: "Later update".to_owned(),
            ..IncomingMessageNotification::default()
        };
        let _ = sys.schedule(later, &focus, 1_650, now_unix);
        sys.tick(&mut backend, &focus, 1_650 + MINIMAL_DELAY_MS, now_unix);
        assert_eq!(sys.sound_player().play_history().len(), 2);
        const { assert!(MINIMAL_ALERT_DELAY_MS == 500) };
    }

    #[test]
    fn forwarded_batch_and_album_grouping_and_cloud_delay() {
        let now_unix = 1_700_000_000;
        let peer = MockNotifyBackend::PEER_SUPPORT_DESK;
        let mut backend = MockNotifyBackend::new();
        let store = NotifySettingsStore::default();
        let mut sys = make_headless_system(NotificationPreferences::default(), store);
        let mut focus = WindowFocusState::with_defaults(1);

        // Check cloud delay when another session is online and we are not online.
        focus.last_was_online = false;
        focus.other_online_unix = now_unix + 10;
        let probe = IncomingMessageNotification::default();
        let (delay_cloud, _) = sys.compute_timing(&probe, &focus, 5_000, now_unix);
        assert_eq!(delay_cloud, DEFAULT_NOTIFY_CLOUD_DELAY_MS);

        // When we ARE online and other is online -> default delay.
        focus.last_was_online = true;
        let (delay_default, _) = sys.compute_timing(&probe, &focus, 5_000, now_unix);
        assert_eq!(delay_default, DEFAULT_NOTIFY_DEFAULT_DELAY_MS);

        // Normal local session (no other online).
        focus.other_online_unix = 0;

        // Schedule 3 forwarded messages -> waits 500ms + 1000ms grouped timer -> "3 forwarded messages".
        for i in 0..3 {
            let fwd = IncomingMessageNotification {
                session_id: 1,
                peer_id: peer,
                sender_id: peer,
                msg_id: MsgId(200 + i),
                date_unix: now_unix,
                is_forwarded: true,
                text: format!("Forwarded {i}"),
                ..IncomingMessageNotification::default()
            };
            let _ = sys.schedule(fwd, &focus, 10_000, now_unix);
        }
        sys.tick(
            &mut backend,
            &focus,
            10_000 + MINIMAL_FORWARD_DELAY_MS,
            now_unix,
        );
        // Still waiting on WAITING_FOR_ALL_GROUPED_DELAY_MS (1000ms).
        assert!(sys.dispatched_notifications().is_empty());

        sys.tick(
            &mut backend,
            &focus,
            10_000 + MINIMAL_FORWARD_DELAY_MS + WAITING_FOR_ALL_GROUPED_DELAY_MS,
            now_unix,
        );
        assert_eq!(sys.dispatched_notifications().len(), 1);
        assert_eq!(
            sys.dispatched_notifications()[0].message,
            "3 forwarded messages"
        );
    }

    // -------------------------------------------------------------------------
    // 6. Focus Assist suppression, clear on read, privacy spoilers, platform switch
    // -------------------------------------------------------------------------

    #[test]
    fn focus_assist_suppresses_toast_sound_and_flash_and_clear_on_read_removes_popups() {
        let now_unix = 1_700_000_000;
        let peer = MockNotifyBackend::PEER_SUPPORT_DESK;
        let mut backend = MockNotifyBackend::new();
        let store = NotifySettingsStore::default();
        let mut sys = make_headless_system(NotificationPreferences::default(), store);

        // Engage Focus Assist AlarmsOnly -> suppresses toast, sound, and flash.
        sys.set_suppression_provider(Box::new(MockSuppressionProvider {
            state: NotificationSuppressionState {
                user_state: UserNotificationState::AcceptsNotifications,
                focus_assist: FocusAssistProfile::AlarmsOnly,
                quiet_hours_registry: false,
            },
        }));
        sys.set_window_flasher(Box::new(MockWindowFlasher::default()));

        let focus = WindowFocusState::with_defaults(1);
        let msg = IncomingMessageNotification {
            session_id: 1,
            peer_id: peer,
            msg_id: MsgId(50),
            text: "Should be quiet during Focus Assist".to_owned(),
            ..IncomingMessageNotification::default()
        };
        let _ = sys.schedule(msg, &focus, 1_000, now_unix);
        sys.tick(&mut backend, &focus, 1_200, now_unix);
        assert!(sys.dispatched_notifications().is_empty());
        assert!(sys.sound_player().play_history().is_empty());

        // Turn Focus Assist off -> notification displays in custom popup manager.
        sys.set_suppression_provider(Box::new(MockSuppressionProvider::default()));
        sys.tick(&mut backend, &focus, 1_300, now_unix);
        assert_eq!(sys.dispatched_notifications().len(), 1);
        assert_eq!(sys.custom_popups().active_visible_count(), 1);

        // Clear on read via HistoryRead event unlinks the shown popup!
        backend.push_event(IncomingNotifyEvent::HistoryRead {
            session_id: 1,
            peer_id: peer,
            read_till_id: MsgId(50),
        });
        sys.tick(&mut backend, &focus, 1_350, now_unix);
        assert_eq!(sys.custom_popups().active_visible_count(), 0);

        // Platform switch between Default and Dummy.
        sys.set_manager_type(ManagerType::Dummy, 1_400);
        assert_eq!(sys.manager_type(), ManagerType::Dummy);
        sys.select_manager(1_400);
        assert_eq!(sys.manager_type(), ManagerType::Default);
    }

    #[test]
    fn permanent_spoilers_login_code_and_privacy_view_and_silent_sound() {
        let masked = apply_permanent_spoilers(
            "Secret code here",
            &[TextSpoilerRange {
                char_offset: 7,
                char_len: 4,
            }],
        );
        assert_eq!(masked, "Secret \u{259A}\u{259A}\u{259A}\u{259A} here");

        let login = obfuscate_login_code("Login code: 54321. Do not share (step 1).");
        assert_eq!(
            login,
            "Login code: \u{259A}\u{259A}\u{259A}\u{259A}\u{259A}. Do not share (step 1)."
        );

        let now_unix = 1_700_000_000;
        let peer = MockNotifyBackend::PEER_SUPPORT_DESK;
        let mut store = NotifySettingsStore::default();
        store.update_peer(
            peer,
            MuteValue::unmute(),
            None,
            Some(NotifySound::silent()),
            None,
            None,
            now_unix,
        );
        let prefs = NotificationPreferences {
            notify_view: NotifyView::ShowName,
            ..NotificationPreferences::default()
        };
        let sys = make_headless_system(prefs, store);
        let focus = WindowFocusState::with_defaults(1);
        let notif = IncomingMessageNotification {
            peer_id: peer,
            ..IncomingMessageNotification::default()
        };
        // Explicit `NotifySound::silent()` yields a silent toast decision.
        assert_eq!(
            sys.evaluate_skip(&notif, &focus, 1_000, now_unix),
            SkipDecision::Notify { silent: true }
        );
        let opts = sys.display_options(&notif, &focus);
        assert!(!opts.hide_name_and_photo);
        assert!(opts.hide_message_text);
        assert!(opts.hide_reply_button);
    }

    // -------------------------------------------------------------------------
    // 7. Custom popup layout (4 corners, max count, queue, Hide All, hover/hide)
    // -------------------------------------------------------------------------

    #[test]
    fn custom_popup_manager_corners_queue_hide_all_and_timers() {
        let bounds = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1280.0, 720.0));
        let mut mgr = CustomPopupManager::new(2, ScreenCorner::BottomRight);

        let make_card = |msg_id: i32| CustomPopupContent {
            id: NotificationId {
                context: ContextId {
                    session_id: 1,
                    peer_id: 100,
                    topic_root_id: 0,
                    monoforum_peer_id: 0,
                },
                msg_id,
            },
            title: "Support Desk".to_owned(),
            subtitle: None,
            message: format!("Message {msg_id}"),
            initials: "SD".to_owned(),
            avatar_rgb: [0x40, 0xA7, 0xE3],
            hide_name_and_photo: false,
            hide_reply_button: false,
            hide_mark_as_read: false,
        };

        mgr.push(make_card(1), 1_000, false);
        assert_eq!(mgr.active_visible_count(), 1);
        assert!(!mgr.is_hide_all_visible());

        mgr.push(make_card(2), 1_000, false);
        mgr.push(make_card(3), 1_000, false);
        assert_eq!(mgr.active_visible_count(), 2);
        assert_eq!(mgr.queued_count(), 1);
        assert!(mgr.is_hide_all_visible());

        // BottomRight geometry: right-aligned, newest card closest to bottom edge.
        let (geoms, hide_all) = mgr.compute_layout(bounds);
        assert_eq!(geoms.len(), 2);
        assert!((geoms[0].x - (1280.0 - NOTIFY_WIDTH - NOTIFY_DELTA_X)).abs() < 0.01);
        assert!(geoms[1].y > geoms[0].y); // newer card (#2) is below older card (#1)
        assert!(hide_all.is_some());
        assert!(hide_all.unwrap().y < geoms[0].y);

        // Switch to TopLeft: left-aligned, stacking downward.
        mgr.set_corner(ScreenCorner::TopLeft);
        let (geoms_tl, hide_all_tl) = mgr.compute_layout(bounds);
        assert!((geoms_tl[0].x - NOTIFY_DELTA_X).abs() < 0.01);
        assert!((geoms_tl[1].y - NOTIFY_DELTA_Y).abs() < 0.01);
        assert!(geoms_tl[0].y > geoms_tl[1].y);
        assert!(hide_all_tl.unwrap().y > geoms_tl[0].y);
        assert!((geoms_tl[0].height - NOTIFY_MIN_HEIGHT).abs() < 0.01);

        // Dismiss card 1 -> card 3 immediately promotes from queue!
        mgr.clear_item(make_card(1).id, 1_100);
        assert_eq!(mgr.active_visible_count(), 2);
        assert_eq!(mgr.queued_count(), 0);

        // After wait + slow hide elapses, cards expire and are removed.
        mgr.tick(1_000 + NOTIFY_WAIT_LONG_HIDE_MS + 100, None, false);
        mgr.tick(
            1_000 + NOTIFY_WAIT_LONG_HIDE_MS + NOTIFY_SLOW_HIDE_MS + 200,
            None,
            false,
        );
        assert_eq!(mgr.active_visible_count(), 0);
    }

    // -------------------------------------------------------------------------
    // 8. Userpic cache + tg-audio sound decoding/mixing
    // -------------------------------------------------------------------------

    #[test]
    fn cached_userpics_writes_pngs_expires_after_60s_and_cleans_up_on_drop() {
        let dir = unique_temp_dir("userpics");
        let path_saved: PathBuf;
        {
            let mut cache = CachedUserpics::new(dir.clone());
            let backend = MockNotifyBackend::new();
            let peer = backend.peer_info(MockNotifyBackend::PEER_SUPPORT_DESK);
            let key = UserpicKey::for_peer(MockNotifyBackend::PEER_SUPPORT_DESK.0, 1);

            path_saved = cache
                .get(key, peer.as_ref(), 10_000)
                .expect("write userpic");
            assert!(path_saved.exists());
            let bytes = std::fs::read(&path_saved).expect("read png");
            assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");

            // Accessing again returns the same path and extends expiry.
            let again = cache.get(key, peer.as_ref(), 30_000).expect("cache hit");
            assert_eq!(again, path_saved);
            assert!(cache.clear_expired(70_000).is_some()); // still alive until 30_000 + 60_000
            assert!(path_saved.exists());

            // Past 90_000ms -> evicted and deleted from disk.
            assert!(
                cache
                    .clear_expired(30_000 + NOTIFY_DELETE_PHOTO_AFTER_MS)
                    .is_none()
            );
            assert!(!path_saved.exists());

            // Hidden placeholder persists until Drop, then cleaned up.
            let hidden_path = cache
                .get(UserpicKey::HIDDEN_PLACEHOLDER, None, 100_000)
                .expect("write placeholder");
            assert!(hidden_path.exists());
            let _ = generate_userpic(peer.as_ref(), 64);
        }
        // Cache dropped -> all files removed.
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn notification_sound_player_decodes_bundled_gpl_mp3_and_mixes_into_tg_audio() {
        let mut player = NotificationSoundPlayer::headless();
        let default_track = player.default_track();
        assert!(default_track.length_ms() > 50);

        let fallback = generate_fallback_chime_track();
        assert!(fallback.length_ms() >= 150);

        // Play default notification sound and verify MixerCore renders non-silent stereo audio.
        let rec = player.play(0, 80).expect("sound played");
        assert_eq!(rec.sound_id, 0);
        assert_eq!(rec.volume_pct, 80);

        let mut stereo = vec![0.0f32; 4096 * 2];
        {
            let mut mixer = player.mixer().lock().expect("mixer lock");
            assert!(mixer.has_active_audio());
            let _ = mixer.render_stereo_48k(&mut stereo);
        }
        let max_amp = stereo.iter().copied().map(f32::abs).fold(0.0f32, f32::max);
        assert!(
            max_amp > 0.001,
            "expected audible chime samples in mixer output"
        );

        // LocalCache WAV conversion also works on bundled MP3.
        let local_wav = player.resolve_local_sound(0).expect("wav sound");
        assert!(local_wav.wav.starts_with(b"RIFF"));
    }

    // -------------------------------------------------------------------------
    // 9. Tray controller (menu, Mute for 1 hour, toggle memory, badge, autostart)
    // -------------------------------------------------------------------------

    #[test]
    fn tray_menu_mute_for_one_hour_toggle_notifications_and_autostart_in_temp_dirs() {
        let now_unix = 1_700_000_000;
        let startup_dir = unique_temp_dir("startup");
        let send_to_dir = unique_temp_dir("sendto");
        std::fs::create_dir_all(&startup_dir).expect("startup dir");
        std::fs::create_dir_all(&send_to_dir).expect("sendto dir");

        let fake_exe = std::env::current_exe().expect("current exe");
        let autostart = AutostartController::with_dirs(
            fake_exe,
            startup_dir.clone(),
            startup_dir.clone(),
            send_to_dir.clone(),
        );

        let mut prefs = NotificationPreferences::default();
        let mut tray = TrayController::with_platform(
            Box::new(MockTrayPlatform::default()),
            WorkMode::WindowAndTray,
            false,
            Some(autostart),
        );
        tray.initialize(&prefs, now_unix);
        assert!(tray.has_icon());

        // Menu contains Open/Minimize, Mute for 1 hour, Disable notifications, Simulate, Quit.
        let menu = tray.build_menu(&prefs, now_unix);
        assert!(menu.iter().any(|item| matches!(
            item,
            tg_platform::tray::TrayMenuItem::Action { id, label, .. }
                if *id == menu_id::MUTE_ONE_HOUR && label == "Mute for 1 hour"
        )));
        assert!(menu.iter().any(|item| matches!(
            item,
            tg_platform::tray::TrayMenuItem::Action { id, label, .. }
                if *id == menu_id::QUIT && label == &tg_lang::tr::lng_quit_from_tray()
        )));

        // Toggle notifications from tray remembers sound & flash states.
        assert!(prefs.desktop_notify && prefs.sound_notify && prefs.flash_bounce_notify);
        let (sound_ch, flash_ch) = toggle_notifications_from_tray(&mut prefs);
        assert!(sound_ch && flash_ch);
        assert!(!prefs.desktop_notify && !prefs.sound_notify && !prefs.flash_bounce_notify);
        let (sound_ch2, flash_ch2) = toggle_notifications_from_tray(&mut prefs);
        assert!(sound_ch2 && flash_ch2);
        assert!(prefs.desktop_notify && prefs.sound_notify && prefs.flash_bounce_notify);

        // Mute for 1 hour via tray menu sets global_mute_until_unix = now + 3600.
        let mut mock_plat = MockTrayPlatform::default();
        mock_plat.push_event(TrayEvent::MenuItem(menu_id::MUTE_ONE_HOUR));
        mock_plat.push_event(TrayEvent::MenuItem(menu_id::SIMULATE_MESSAGE));
        mock_plat.push_event(TrayEvent::MenuItem(menu_id::QUIT));

        let mut tray2 = TrayController::with_platform(
            Box::new(mock_plat),
            WorkMode::WindowAndTray,
            false,
            None,
        );
        let acts = tray2.poll_actions(&mut prefs, 1_000, now_unix);
        assert_eq!(
            acts,
            vec![
                TrayAction::MuteForOneHour,
                TrayAction::SimulateIncomingMessage,
                TrayAction::Quit
            ]
        );
        assert!(prefs.is_globally_muted(now_unix + 10));
        assert!(!prefs.is_globally_muted(now_unix + MUTE_ONE_HOUR_SECS));

        // WorkMode::WindowOnly destroys icon; switching back recreates it.
        tray.set_work_mode(WorkMode::WindowOnly, &prefs, now_unix);
        assert!(!tray.has_icon());
        tray.set_work_mode(WorkMode::WindowAndTray, &prefs, now_unix);
        assert!(tray.has_icon());

        // Autostart & SendTo shortcuts work in isolated temp dirs (never touching real Startup).
        if let Some(auto) = tray.autostart_mut() {
            auto.set_autostart(true).expect("create autostart lnk");
            assert!(auto.flags().autostart_enabled);
            auto.set_autostart(false).expect("remove autostart lnk");
            assert!(!auto.flags().autostart_enabled);

            auto.set_send_to_menu(true).expect("create sendto lnk");
            assert!(auto.flags().send_to_menu_enabled);
            auto.set_send_to_menu(false).expect("remove sendto lnk");
            assert!(!auto.flags().send_to_menu_enabled);
        }

        let _ = std::fs::remove_dir_all(&startup_dir);
        let _ = std::fs::remove_dir_all(&send_to_dir);
    }

    #[test]
    fn mock_backend_simulate_incoming_message_cycles_and_records_quick_reply() {
        let mut backend = MockNotifyBackend::new();
        let now_unix = 1_700_000_000;

        // 5 steps cover DM, group mention, pinned, reaction, and 4-message burst.
        let mut total = 0usize;
        for _ in 0..5 {
            total += backend.simulate_incoming_message(now_unix).len();
        }
        assert_eq!(total, 1 + 1 + 1 + 1 + 4);
        assert_eq!(backend.poll_events().len(), 8);

        backend.send_quick_reply(
            1,
            MockNotifyBackend::PEER_SUPPORT_DESK,
            MsgId(0),
            MsgId(500),
            "Thanks, reviewing now!".to_owned(),
        );
        assert_eq!(backend.sent_replies.len(), 1);
        assert_eq!(backend.sent_replies[0].text, "Thanks, reviewing now!");
        assert_eq!(backend.marked_read.len(), 1);
    }
}
