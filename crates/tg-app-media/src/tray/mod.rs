//! System tray icon controller, taskbar unread overlay badge, and Windows autostart flags.
//!
//! Port of `Telegram/SourceFiles/tray.h`, `Telegram/SourceFiles/tray.cpp`,
//! `Telegram/SourceFiles/platform/win/tray_win.cpp` wiring,
//! `Telegram/SourceFiles/window/main_window.cpp` taskbar badge sync, and
//! `Telegram/SourceFiles/platform/win/specific_win.cpp` (`AutostartToggle`, `psSendToMenu`)
//! wired to `tg-platform`.

pub mod accounts_menu;
pub mod strings;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tg_platform::registration::{
    AppLinkConfig, is_autostart_enabled_in, is_send_to_enabled_in, send_to_folder_dir,
    set_autostart_in, set_send_to_in, startup_folder_dir,
};
use tg_platform::tray::{
    TaskbarOverlay, TrayConfig, TrayCounterColors, TrayEvent, TrayIcon, TrayMenuItem,
    format_tray_tooltip,
};

use crate::notifications::settings::{
    AccountUnreadEntry, MUTE_ONE_HOUR_SECS, NotificationPreferences, UnreadBadgeResult,
    aggregate_account_badges_per_account,
};
pub use crate::tray::accounts_menu::TrayAccountEntry;

/// Minimum interval (ms) between left-clicks on the tray icon to debounce double-clicks
/// (`tray.cpp:69-76`).
pub const TRAY_CLICK_DEBOUNCE_MS: i64 = 500;

/// Menu command IDs for the system tray context menu.
pub mod menu_id {
    /// Open or minimize the main Rustgram window.
    pub const OPEN_OR_MINIMIZE: u32 = 1;
    /// Mute notifications for 1 hour (or unmute immediately if muted).
    pub const MUTE_ONE_HOUR: u32 = 2;
    /// Toggle desktop notifications on/off (remembering sound & flash state).
    pub const TOGGLE_NOTIFICATIONS: u32 = 3;
    /// Debug action: simulate an incoming message notification.
    pub const SIMULATE_MESSAGE: u32 = 4;
    /// Quit Rustgram completely.
    pub const QUIT: u32 = 5;
}

/// Application tray/window work mode (`Core::Settings::WorkMode` in `core_settings.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WorkMode {
    /// Show both taskbar window and system tray icon (default on Windows).
    #[default]
    WindowAndTray,
    /// Hide taskbar button when minimized and keep only the tray icon.
    TrayOnly,
    /// Show only the taskbar window and destroy the tray icon.
    WindowOnly,
}

impl WorkMode {
    /// Returns `true` when the system tray icon should exist.
    #[must_use]
    pub const fn has_tray_icon(self) -> bool {
        !matches!(self, Self::WindowOnly)
    }
}

/// High-level action emitted by [`TrayController`] to the application shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    /// Restore / bring main window to foreground.
    ShowWindow,
    /// Hide / minimize main window to tray.
    HideToTray,
    /// Mute notifications globally for 1 hour.
    MuteForOneHour,
    /// Cancel active 1-hour global mute.
    UnmuteGlobal,
    /// Toggle desktop notifications on/off (with sound/flash memory).
    ToggleNotifications,
    /// Trigger `"simulate incoming message"` debug action.
    SimulateIncomingMessage,
    /// Switch the main window to the selected account slot (`tray_accounts_menu.cpp`).
    SwitchAccount(i32),
    /// Quit the application.
    Quit,
}

/// Platform backend trait abstracting `tg_platform::tray::{TrayIcon, TaskbarOverlay}` so
/// unit tests can exercise all tray/badge logic without creating Win32 icons or windows.
pub trait TrayPlatform {
    /// Creates the OS tray icon if not already present.
    fn create_icon(&mut self, config: TrayConfig) -> tg_platform::Result<()>;
    /// Destroys the OS tray icon if present.
    fn destroy_icon(&mut self);
    /// Returns `true` if a tray icon currently exists.
    fn has_icon(&self) -> bool;
    /// Updates tray icon unread counter, muted badge color, and tooltip.
    fn set_tray_unread(&mut self, count: u32, muted: bool, tooltip: &str);
    /// Updates tray monochrome mode.
    fn set_monochrome(&mut self, monochrome: bool);
    /// Replaces the tray context menu.
    fn set_menu(&mut self, items: Vec<TrayMenuItem>);
    /// Shows a Windows tray balloon message (e.g. when minimizing to tray).
    fn show_balloon(&mut self, title: &str, body: &str);
    /// Updates the Windows `ITaskbarList3` taskbar overlay badge on `hwnd`.
    fn set_taskbar_badge(&mut self, hwnd: Option<isize>, count: u32, muted: bool);
    /// Drains pending [`TrayEvent`]s emitted by the native tray thread.
    fn drain_events(&mut self) -> Vec<TrayEvent>;
}

/// Production [`TrayPlatform`] backed by `tg_platform::tray::{TrayIcon, TaskbarOverlay}`.
pub struct RealTrayPlatform {
    icon: Option<TrayIcon>,
    overlay: Option<TaskbarOverlay>,
    events: Arc<Mutex<VecDeque<TrayEvent>>>,
}

impl std::fmt::Debug for RealTrayPlatform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealTrayPlatform")
            .field("has_icon", &self.icon.is_some())
            .field("has_overlay", &self.overlay.is_some())
            .finish()
    }
}

impl Default for RealTrayPlatform {
    fn default() -> Self {
        Self::new()
    }
}

impl RealTrayPlatform {
    /// Creates a new [`RealTrayPlatform`] initializing `ITaskbarList3` if available.
    #[must_use]
    pub fn new() -> Self {
        let overlay = TaskbarOverlay::new().ok();
        Self {
            icon: None,
            overlay,
            events: Arc::new(Mutex::new(VecDeque::new())),
        }
    }
}

impl TrayPlatform for RealTrayPlatform {
    fn create_icon(&mut self, config: TrayConfig) -> tg_platform::Result<()> {
        if self.icon.is_some() {
            return Ok(());
        }
        let sink = Arc::clone(&self.events);
        let tray = TrayIcon::new(config, move |ev| {
            if let Ok(mut q) = sink.lock() {
                q.push_back(ev);
            }
        })?;
        self.icon = Some(tray);
        Ok(())
    }

    fn destroy_icon(&mut self) {
        self.icon = None;
    }

    fn has_icon(&self) -> bool {
        self.icon.is_some()
    }

    fn set_tray_unread(&mut self, count: u32, muted: bool, tooltip: &str) {
        if let Some(ref icon) = self.icon {
            let _ = icon.set_unread(count, muted);
            let _ = icon.set_tooltip(tooltip);
        }
    }

    fn set_monochrome(&mut self, monochrome: bool) {
        if let Some(ref icon) = self.icon {
            let _ = icon.set_monochrome(monochrome);
        }
    }

    fn set_menu(&mut self, items: Vec<TrayMenuItem>) {
        if let Some(ref icon) = self.icon {
            let _ = icon.set_menu(items);
        }
    }

    fn show_balloon(&mut self, title: &str, body: &str) {
        if let Some(ref icon) = self.icon {
            let _ = icon.show_balloon(title, body);
        }
    }

    fn set_taskbar_badge(&mut self, hwnd: Option<isize>, count: u32, muted: bool) {
        let Some(h) = hwnd else {
            return;
        };
        if h == 0 {
            return;
        }
        if let Some(ref mut overlay) = self.overlay {
            let desc = if count > 0 {
                tg_lang::tr::lng_unread_bar(i64::from(count))
            } else {
                String::new()
            };
            let _ = overlay.set_unread_badge(h, count, muted, &desc, TrayCounterColors::default());
        }
    }

    fn drain_events(&mut self) -> Vec<TrayEvent> {
        self.events
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default()
    }
}

/// Deterministic mock [`TrayPlatform`] for headless unit testing.
#[derive(Debug, Default)]
pub struct MockTrayPlatform {
    /// Whether an icon is currently created.
    pub icon_created: bool,
    /// Last applied tray unread count.
    pub tray_unread_count: u32,
    /// Last applied tray muted flag.
    pub tray_muted: bool,
    /// Last applied tray tooltip.
    pub tooltip: String,
    /// Monochrome mode flag.
    pub monochrome: bool,
    /// Most recently applied menu items.
    pub menu: Vec<TrayMenuItem>,
    /// Last applied taskbar overlay `(hwnd, count, muted)`.
    pub taskbar_badge: Option<(Option<isize>, u32, bool)>,
    /// Recorded tray balloons `(title, body)`.
    pub balloons: Vec<(String, String)>,
    /// Queued synthetic events returned by `drain_events()`.
    pub pending_events: VecDeque<TrayEvent>,
}

impl MockTrayPlatform {
    /// Pushes a synthetic [`TrayEvent`] to be processed on the next poll.
    pub fn push_event(&mut self, event: TrayEvent) {
        self.pending_events.push_back(event);
    }
}

impl TrayPlatform for MockTrayPlatform {
    fn create_icon(&mut self, config: TrayConfig) -> tg_platform::Result<()> {
        self.icon_created = true;
        self.tray_unread_count = config.unread_count;
        self.tray_muted = config.muted;
        self.monochrome = config.monochrome;
        self.tooltip = config
            .tooltip
            .unwrap_or_else(|| format_tray_tooltip(&config.app_name, config.unread_count));
        self.menu = config.menu;
        Ok(())
    }

    fn destroy_icon(&mut self) {
        self.icon_created = false;
    }

    fn has_icon(&self) -> bool {
        self.icon_created
    }

    fn set_tray_unread(&mut self, count: u32, muted: bool, tooltip: &str) {
        self.tray_unread_count = count;
        self.tray_muted = muted;
        self.tooltip = tooltip.to_owned();
    }

    fn set_monochrome(&mut self, monochrome: bool) {
        self.monochrome = monochrome;
    }

    fn set_menu(&mut self, items: Vec<TrayMenuItem>) {
        self.menu = items;
    }

    fn show_balloon(&mut self, title: &str, body: &str) {
        self.balloons.push((title.to_owned(), body.to_owned()));
    }

    fn set_taskbar_badge(&mut self, hwnd: Option<isize>, count: u32, muted: bool) {
        self.taskbar_badge = Some((hwnd, count, muted));
    }

    fn drain_events(&mut self) -> Vec<TrayEvent> {
        self.pending_events.drain(..).collect()
    }
}

impl TrayPlatform for Arc<Mutex<MockTrayPlatform>> {
    fn create_icon(&mut self, config: TrayConfig) -> tg_platform::Result<()> {
        self.lock()
            .map_err(|_| tg_platform::PlatformError::InvalidArgument("mock tray lock".into()))?
            .create_icon(config)
    }

    fn destroy_icon(&mut self) {
        if let Ok(mut g) = self.lock() {
            g.destroy_icon();
        }
    }

    fn has_icon(&self) -> bool {
        self.lock().is_ok_and(|g| g.has_icon())
    }

    fn set_tray_unread(&mut self, count: u32, muted: bool, tooltip: &str) {
        if let Ok(mut g) = self.lock() {
            g.set_tray_unread(count, muted, tooltip);
        }
    }

    fn set_monochrome(&mut self, monochrome: bool) {
        if let Ok(mut g) = self.lock() {
            g.set_monochrome(monochrome);
        }
    }

    fn set_menu(&mut self, items: Vec<TrayMenuItem>) {
        if let Ok(mut g) = self.lock() {
            g.set_menu(items);
        }
    }

    fn show_balloon(&mut self, title: &str, body: &str) {
        if let Ok(mut g) = self.lock() {
            g.show_balloon(title, body);
        }
    }

    fn set_taskbar_badge(&mut self, hwnd: Option<isize>, count: u32, muted: bool) {
        if let Ok(mut g) = self.lock() {
            g.set_taskbar_badge(hwnd, count, muted);
        }
    }

    fn drain_events(&mut self) -> Vec<TrayEvent> {
        self.lock()
            .map(|mut g| g.drain_events())
            .unwrap_or_default()
    }
}

/// Persisted Windows OS integration flags (autostart + Explorer "Send to").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutostartFlags {
    /// Launch Rustgram automatically when Windows starts (`-autostart` shortcut in Startup folder).
    pub autostart_enabled: bool,
    /// Start minimized to tray when launched via `-autostart`.
    pub start_minimized: bool,
    /// Add Rustgram shortcut to Windows Explorer's "Send to" menu.
    pub send_to_menu_enabled: bool,
}

/// Controller managing Windows Startup and SendTo `.lnk` shortcuts via `tg-platform::registration`.
///
/// Accepts explicit folder overrides (`startup_dir_override`, `send_to_dir_override`) so unit
/// tests NEVER touch the real user Startup folder, Start Menu, or Windows Registry.
#[derive(Debug, Clone)]
pub struct AutostartController {
    link_config: AppLinkConfig,
    startup_dir_override: Option<PathBuf>,
    send_to_dir_override: Option<PathBuf>,
    flags: AutostartFlags,
}

impl AutostartController {
    /// Creates a controller targeting the real OS Startup / SendTo folders for `executable`.
    #[must_use]
    pub fn for_executable(executable: PathBuf, working_dir: PathBuf) -> Self {
        Self {
            link_config: AppLinkConfig::for_executable(executable, working_dir),
            startup_dir_override: None,
            send_to_dir_override: None,
            flags: AutostartFlags::default(),
        }
    }

    /// Creates an isolated controller targeting explicit test directories.
    #[must_use]
    pub fn with_dirs(
        executable: PathBuf,
        working_dir: PathBuf,
        startup_dir: PathBuf,
        send_to_dir: PathBuf,
    ) -> Self {
        Self {
            link_config: AppLinkConfig::for_executable(executable, working_dir),
            startup_dir_override: Some(startup_dir),
            send_to_dir_override: Some(send_to_dir),
            flags: AutostartFlags::default(),
        }
    }

    /// Returns the current [`AutostartFlags`].
    #[must_use]
    pub const fn flags(&self) -> AutostartFlags {
        self.flags
    }

    /// Updates `start_minimized` preference (applied on `-autostart` launch).
    pub fn set_start_minimized(&mut self, minimized: bool) {
        self.flags.start_minimized = minimized;
    }

    /// Refreshes `autostart_enabled` and `send_to_menu_enabled` by probing the target folders.
    pub fn refresh_from_disk(&mut self) -> AutostartFlags {
        if let Ok(dir) = self.resolve_startup_dir() {
            self.flags.autostart_enabled =
                is_autostart_enabled_in(&dir, &self.link_config.shortcut_name);
        }
        if let Ok(dir) = self.resolve_send_to_dir() {
            self.flags.send_to_menu_enabled =
                is_send_to_enabled_in(&dir, &self.link_config.shortcut_name);
        }
        self.flags
    }

    /// Enables or disables the `-autostart` shortcut (`AutostartToggle`).
    pub fn set_autostart(&mut self, enabled: bool) -> tg_platform::Result<()> {
        let dir = self.resolve_startup_dir()?;
        set_autostart_in(&dir, &self.link_config, enabled)?;
        self.flags.autostart_enabled =
            is_autostart_enabled_in(&dir, &self.link_config.shortcut_name);
        Ok(())
    }

    /// Enables or disables the Explorer "Send to" shortcut (`psSendToMenu`).
    pub fn set_send_to_menu(&mut self, enabled: bool) -> tg_platform::Result<()> {
        let dir = self.resolve_send_to_dir()?;
        set_send_to_in(&dir, &self.link_config, enabled)?;
        self.flags.send_to_menu_enabled =
            is_send_to_enabled_in(&dir, &self.link_config.shortcut_name);
        Ok(())
    }

    fn resolve_startup_dir(&self) -> tg_platform::Result<PathBuf> {
        if let Some(ref dir) = self.startup_dir_override {
            return Ok(dir.clone());
        }
        startup_folder_dir()
    }

    fn resolve_send_to_dir(&self) -> tg_platform::Result<PathBuf> {
        if let Some(ref dir) = self.send_to_dir_override {
            return Ok(dir.clone());
        }
        send_to_folder_dir()
    }
}

/// Coordinates system tray icon, menu actions, unread counter badge, taskbar overlay, and
/// autostart flags (`Core::Tray`).
pub struct TrayController {
    platform: Box<dyn TrayPlatform>,
    app_name: String,
    work_mode: WorkMode,
    monochrome: bool,
    include_debug_simulate_item: bool,
    window_active_for_tray: bool,
    passcode_locked: bool,
    last_badge: UnreadBadgeResult,
    last_click_ms: Option<i64>,
    last_menu_signature: Option<(bool, bool, bool, bool)>,
    accounts: Vec<TrayAccountEntry>,
    autostart: Option<AutostartController>,
}

impl std::fmt::Debug for TrayController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrayController")
            .field("app_name", &self.app_name)
            .field("work_mode", &self.work_mode)
            .field("monochrome", &self.monochrome)
            .field("last_badge", &self.last_badge)
            .finish()
    }
}

impl TrayController {
    /// Creates a production [`TrayController`] backed by `tg-platform`'s native tray & taskbar APIs.
    #[must_use]
    pub fn new(work_mode: WorkMode, monochrome: bool) -> Self {
        let autostart = std::env::current_exe().ok().map(|exe| {
            let workdir = exe
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            AutostartController::for_executable(exe, workdir)
        });
        Self::with_platform(
            Box::new(RealTrayPlatform::new()),
            work_mode,
            monochrome,
            autostart,
        )
    }

    /// Creates a [`TrayController`] with an explicit [`TrayPlatform`] (for unit tests).
    #[must_use]
    pub fn with_platform(
        platform: Box<dyn TrayPlatform>,
        work_mode: WorkMode,
        monochrome: bool,
        autostart: Option<AutostartController>,
    ) -> Self {
        Self {
            platform,
            app_name: strings::APP_NAME.to_owned(),
            work_mode,
            monochrome,
            include_debug_simulate_item: true,
            window_active_for_tray: true,
            passcode_locked: false,
            last_badge: UnreadBadgeResult::default(),
            last_click_ms: None,
            last_menu_signature: None,
            accounts: Vec::new(),
            autostart,
        }
    }

    /// Creates the tray icon (when `work_mode != WindowOnly`) and installs the initial menu.
    pub fn initialize(&mut self, prefs: &NotificationPreferences, now_unix: i64) {
        if !self.work_mode.has_tray_icon() {
            return;
        }
        let menu = self.build_menu(prefs, now_unix);
        let config = TrayConfig {
            app_name: self.app_name.clone(),
            tooltip: Some(format_tray_tooltip(&self.app_name, self.last_badge.count)),
            base_icon: None,
            unread_count: self.last_badge.count,
            muted: self.last_badge.muted,
            monochrome: self.monochrome,
            colors: TrayCounterColors::default(),
            menu,
        };
        if let Err(err) = self.platform.create_icon(config) {
            tracing::warn!(%err, "Failed to create Windows system tray icon");
        }
        self.last_menu_signature = Some(self.menu_signature(prefs, now_unix));
    }

    /// Returns `true` if a system tray icon is currently active (`Tray::has`).
    #[must_use]
    pub fn has_icon(&self) -> bool {
        self.platform.has_icon()
    }

    /// Returns the active [`WorkMode`].
    #[must_use]
    pub const fn work_mode(&self) -> WorkMode {
        self.work_mode
    }

    /// Updates [`WorkMode`], creating or destroying the tray icon as needed (`tray.cpp:39-51`).
    pub fn set_work_mode(
        &mut self,
        work_mode: WorkMode,
        prefs: &NotificationPreferences,
        now_unix: i64,
    ) {
        let had_icon = self.work_mode.has_tray_icon();
        let wants_icon = work_mode.has_tray_icon();
        self.work_mode = work_mode;
        if had_icon != wants_icon {
            if wants_icon {
                self.initialize(prefs, now_unix);
            } else {
                self.platform.destroy_icon();
            }
        }
    }

    /// Toggles monochrome tray icon mode (`trayIconMonochromeChanges`).
    pub fn set_monochrome(&mut self, monochrome: bool) {
        if self.monochrome != monochrome {
            self.monochrome = monochrome;
            self.platform.set_monochrome(monochrome);
        }
    }

    /// Controls whether the `"Simulate incoming message"` debug menu entry is included.
    pub fn set_include_debug_simulate_item(&mut self, include: bool) {
        self.include_debug_simulate_item = include;
        self.last_menu_signature = None;
    }

    /// Updates the signed-in accounts list displayed in the tray context menu
    /// (`TrayAccountsMenu::Fill`), forcing a menu rebuild whenever the list changes.
    pub fn set_accounts(&mut self, accounts: Vec<TrayAccountEntry>) {
        if self.accounts != accounts {
            self.accounts = accounts;
            self.last_menu_signature = None;
        }
    }

    /// Returns the currently configured tray account menu entries.
    #[must_use]
    pub fn accounts(&self) -> &[TrayAccountEntry] {
        &self.accounts
    }

    /// Returns mutable access to the [`AutostartController`], if configured.
    pub fn autostart_mut(&mut self) -> Option<&mut AutostartController> {
        self.autostart.as_mut()
    }

    /// Returns immutable access to the [`AutostartController`], if configured.
    #[must_use]
    pub fn autostart(&self) -> Option<&AutostartController> {
        self.autostart.as_ref()
    }

    /// Updates window active/passcode state and rebuilds the tray menu if labels changed.
    pub fn sync_state(
        &mut self,
        window_active_for_tray: bool,
        passcode_locked: bool,
        prefs: &NotificationPreferences,
        now_unix: i64,
    ) {
        self.window_active_for_tray = window_active_for_tray;
        self.passcode_locked = passcode_locked;
        let sig = self.menu_signature(prefs, now_unix);
        if self.last_menu_signature != Some(sig) {
            let menu = self.build_menu(prefs, now_unix);
            self.platform.set_menu(menu);
            self.last_menu_signature = Some(sig);
        }
    }

    /// Syncs unread badge count and muted color to both the system tray icon and
    /// Windows `ITaskbarList3` taskbar overlay (`updateIconCounters` + `updateTaskbarAndIconCounters`).
    pub fn update_unread_badge(&mut self, badge: UnreadBadgeResult, hwnd: Option<isize>) {
        self.last_badge = badge;
        let tooltip = format_tray_tooltip(&self.app_name, badge.count);
        self.platform
            .set_tray_unread(badge.count, badge.muted, &tooltip);
        self.platform
            .set_taskbar_badge(hwnd, badge.count, badge.muted);
    }

    /// Aggregates unread counts across multiple accounts (respecting each account's
    /// `include_muted_counter` / `count_unread_messages` settings) and updates both the
    /// system tray icon and taskbar overlay badge.
    pub fn update_multi_account_unread_badge(
        &mut self,
        accounts: impl IntoIterator<Item = AccountUnreadEntry>,
        hwnd: Option<isize>,
    ) -> UnreadBadgeResult {
        let badge = aggregate_account_badges_per_account(accounts);
        self.update_unread_badge(badge, hwnd);
        badge
    }

    /// Returns the last applied tray/taskbar [`UnreadBadgeResult`].
    #[must_use]
    pub const fn last_badge(&self) -> UnreadBadgeResult {
        self.last_badge
    }

    /// Builds the tray context menu reflecting current window visibility, 1-hour mute status,
    /// notification toggle, passcode lock, and quit action.
    #[must_use]
    pub fn build_menu(&self, prefs: &NotificationPreferences, now_unix: i64) -> Vec<TrayMenuItem> {
        let mut items = Vec::with_capacity(7);

        let open_label = if self.window_active_for_tray {
            tg_lang::tr::lng_minimize_to_tray()
        } else {
            tg_lang::tr::lng_open_from_tray()
        };
        items.push(TrayMenuItem::action(menu_id::OPEN_OR_MINIMIZE, open_label));

        if !self.passcode_locked {
            let mute_label = if let Some(until) = prefs.global_mute_until_unix
                && until > now_unix
            {
                let mins = ((until - now_unix + 59) / 60).max(1);
                strings::unmute_minutes_left(mins)
            } else {
                strings::MUTE_FOR_ONE_HOUR.to_owned()
            };
            items.push(TrayMenuItem::Action {
                id: menu_id::MUTE_ONE_HOUR,
                label: mute_label,
                enabled: true,
                checked: prefs.is_globally_muted(now_unix),
            });

            let notif_label = if prefs.desktop_notify {
                tg_lang::tr::lng_disable_notifications_from_tray()
            } else {
                tg_lang::tr::lng_enable_notifications_from_tray()
            };
            items.push(TrayMenuItem::action(
                menu_id::TOGGLE_NOTIFICATIONS,
                notif_label,
            ));

            if self.include_debug_simulate_item {
                items.push(TrayMenuItem::Separator);
                items.push(TrayMenuItem::action(
                    menu_id::SIMULATE_MESSAGE,
                    strings::SIMULATE_INCOMING_MESSAGE,
                ));
            }
        }

        items.push(TrayMenuItem::Separator);
        items.push(TrayMenuItem::action(
            menu_id::QUIT,
            tg_lang::tr::lng_quit_from_tray(),
        ));
        accounts_menu::fill_tray_accounts_menu(&mut items, &self.accounts, self.passcode_locked);
        items
    }

    /// Drains tray events, applies preference toggles directly where appropriate (including
    /// `"Mute for 1 hour"` and `toggleSoundNotifications` memory), and returns high-level
    /// [`TrayAction`]s for the window shell.
    pub fn poll_actions(
        &mut self,
        prefs: &mut NotificationPreferences,
        now_ms: i64,
        now_unix: i64,
    ) -> Vec<TrayAction> {
        let mut actions = Vec::new();
        let events = self.platform.drain_events();
        for ev in events {
            match ev {
                TrayEvent::LeftClick | TrayEvent::DoubleClick => {
                    let debounced = self
                        .last_click_ms
                        .is_some_and(|prev| now_ms.saturating_sub(prev) < TRAY_CLICK_DEBOUNCE_MS);
                    if !debounced {
                        self.last_click_ms = Some(now_ms);
                        if self.window_active_for_tray {
                            self.platform
                                .show_balloon(&self.app_name, &tg_lang::tr::lng_tray_icon_text());
                            actions.push(TrayAction::HideToTray);
                        } else {
                            actions.push(TrayAction::ShowWindow);
                        }
                    }
                }
                TrayEvent::ContextMenuOpening => {
                    let menu = self.build_menu(prefs, now_unix);
                    self.platform.set_menu(menu);
                    self.last_menu_signature = Some(self.menu_signature(prefs, now_unix));
                }
                TrayEvent::MenuItem(id) => match id {
                    menu_id::OPEN_OR_MINIMIZE => {
                        if self.window_active_for_tray {
                            actions.push(TrayAction::HideToTray);
                        } else {
                            actions.push(TrayAction::ShowWindow);
                        }
                    }
                    menu_id::MUTE_ONE_HOUR => {
                        if prefs.is_globally_muted(now_unix) {
                            prefs.clear_global_mute();
                            actions.push(TrayAction::UnmuteGlobal);
                        } else {
                            prefs.mute_globally_for(now_unix, MUTE_ONE_HOUR_SECS);
                            actions.push(TrayAction::MuteForOneHour);
                        }
                        self.last_menu_signature = None;
                    }
                    menu_id::TOGGLE_NOTIFICATIONS => {
                        toggle_notifications_from_tray(prefs);
                        actions.push(TrayAction::ToggleNotifications);
                        self.last_menu_signature = None;
                    }
                    menu_id::SIMULATE_MESSAGE => {
                        actions.push(TrayAction::SimulateIncomingMessage);
                    }
                    menu_id::QUIT => {
                        actions.push(TrayAction::Quit);
                    }
                    other => {
                        if let Some(slot_index) = accounts_menu::account_slot_from_menu_id(other) {
                            actions.push(TrayAction::SwitchAccount(slot_index));
                        }
                    }
                },
            }
        }
        actions
    }

    /// Returns a reference to the underlying [`TrayPlatform`] (useful for inspecting mocks in tests).
    #[must_use]
    pub fn platform(&self) -> &dyn TrayPlatform {
        self.platform.as_ref()
    }

    fn menu_signature(
        &self,
        prefs: &NotificationPreferences,
        now_unix: i64,
    ) -> (bool, bool, bool, bool) {
        (
            self.window_active_for_tray,
            self.passcode_locked,
            prefs.desktop_notify,
            prefs.is_globally_muted(now_unix),
        )
    }
}

/// Exact port of `Tray::toggleSoundNotifications()` (`tray.cpp:156-200`).
///
/// Toggles `desktop_notify`. When disabling, remembers whether `sound_notify` and
/// `flash_bounce_notify` were enabled and turns them off; when re-enabling, restores
/// those remembered toggles.
pub fn toggle_notifications_from_tray(prefs: &mut NotificationPreferences) -> (bool, bool) {
    let mut sound_changed = false;
    let mut flash_changed = false;

    prefs.desktop_notify = !prefs.desktop_notify;
    if prefs.desktop_notify {
        if prefs.remembered_sound_notify_from_tray && !prefs.sound_notify {
            prefs.sound_notify = true;
            prefs.remembered_sound_notify_from_tray = false;
            sound_changed = true;
        }
        if prefs.remembered_flash_bounce_notify_from_tray && !prefs.flash_bounce_notify {
            prefs.flash_bounce_notify = true;
            prefs.remembered_flash_bounce_notify_from_tray = false;
            flash_changed = true;
        }
    } else {
        if prefs.sound_notify {
            prefs.sound_notify = false;
            prefs.remembered_sound_notify_from_tray = true;
            sound_changed = true;
        } else {
            prefs.remembered_sound_notify_from_tray = false;
        }
        if prefs.flash_bounce_notify {
            prefs.flash_bounce_notify = false;
            prefs.remembered_flash_bounce_notify_from_tray = true;
            flash_changed = true;
        } else {
            prefs.remembered_flash_bounce_notify_from_tray = false;
        }
    }

    (sound_changed, flash_changed)
}
