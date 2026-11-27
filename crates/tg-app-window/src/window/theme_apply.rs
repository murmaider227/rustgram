//! Theme apply path of the window: choosing a built-in theme in Chat settings, the night-mode
//! toggle, and following the system dark mode.
//!
//! Port of `settings/sections/settings_chat.cpp` (`SetupDefaultThemes`, its `apply` lambda),
//! `window/themes/window_theme.cpp` (`ToggleNightMode`, `ToggleNightModeWithConfirmation`,
//! `ChatBackground::nightModeChangeAllowed`), `core/application.cpp` (`checkSystemDarkMode`)
//! and the night toggle of `window/window_main_menu.cpp`.
//!
//! The rules are [`tg_theme::NightModeState`], owned by the window's `ThemeManager`. This
//! module feeds it the follow-system setting and the OS value and applies what it decides.
//! Unlike tdesktop's reactive streams, the two inputs are compared once per frame (two
//! equality checks) and the rules run only when one of them changed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tg_theme::{Color as ThemeColor, EmbeddedType, ToggleDecision};

use tg_app_core::stand_in::settings::ShellSettings;

use super::WindowController;
use crate::window::layer_stack::ModalBox;

/// How often the OS dark-mode value is re-read while it comes from the platform.
const SYSTEM_DARK_POLL: Duration = Duration::from_secs(2);

/// `Core::Settings::_systemDarkModeEnabled` starts as `true`; used until the settings
/// controller (which owns the stored value) exists.
const FOLLOW_SYSTEM_DEFAULT: bool = true;

/// Whether this process is the real app, whose live windows read the OS dark mode.
static PLATFORM_DARK_MODE: AtomicBool = AtomicBool::new(false);

/// Lets the windows of live accounts read the system dark mode from the platform. The app
/// calls it once at start-up. Without it (tests, tools) no window reads the OS, so nothing
/// depends on the machine's theme.
pub fn follow_platform_dark_mode() {
    PLATFORM_DARK_MODE.store(true, Ordering::Relaxed);
}

/// Where the OS dark-mode value (`Core::Settings::systemDarkMode`) comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SystemDarkSource {
    /// A live window of the real app (see [`follow_platform_dark_mode`]) reads
    /// `tg_platform::system_info::is_dark_mode()` every [`SYSTEM_DARK_POLL`]. Any other
    /// window (the mock demo, tests) has no value, so nothing follows the OS.
    Auto,
    /// A given value; `None` means the OS gives no signal.
    Fixed(Option<bool>),
}

/// A night-mode change waiting for the "disable auto-night mode" confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingNightSwitch {
    /// The night-mode toggle: switch to the theme saved for the other mode.
    Toggle,
    /// A theme card of the other mode: switch to exactly this theme.
    Theme {
        kind: EmbeddedType,
        accent: Option<ThemeColor>,
    },
}

/// Follow-system state of one window.
#[derive(Debug)]
pub(super) struct ThemeFollow {
    /// The theme before a Theme Studio theme went on trial ("Keep this theme?", TGB-2984).
    trial_previous: Option<Box<tg_theme::Palette>>,
    source: SystemDarkSource,
    /// Last value read from the platform.
    platform_dark: Option<bool>,
    platform_read_at: Option<Instant>,
    pending: Option<PendingNightSwitch>,
    /// Theme generation a repaint was last requested for.
    repaint_generation: u64,
}

impl ThemeFollow {
    pub(super) const fn new() -> Self {
        Self {
            trial_previous: None,
            source: SystemDarkSource::Auto,
            platform_dark: None,
            platform_read_at: None,
            pending: None,
            repaint_generation: 0,
        }
    }

    fn system_dark(&mut self, live_window: bool) -> Option<bool> {
        match self.source {
            SystemDarkSource::Fixed(value) => value,
            SystemDarkSource::Auto => {
                if !(live_window && PLATFORM_DARK_MODE.load(Ordering::Relaxed)) {
                    return None;
                }
                let now = tg_app_core::clock::now();
                let stale = self
                    .platform_read_at
                    .is_none_or(|at| now.saturating_duration_since(at) >= SYSTEM_DARK_POLL);
                if stale {
                    self.platform_dark = tg_platform::system_info::is_dark_mode();
                    self.platform_read_at = Some(now);
                }
                self.platform_dark
            }
        }
    }
}

impl WindowController {
    /// Applies a theme change made in the Theme Studio and keeps the studio on the window's
    /// theme (TGB-2984). Called once per frame after the studio was drawn.
    pub fn exchange_studio_theme(&mut self, ctx: &egui::Context) {
        use tg_app_settings::themes::{AppThemesHost, StudioThemeChange};
        match AppThemesHost::take_app_theme_change(ctx) {
            Some(StudioThemeChange::Live(palette)) => {
                self.theme_manager.set_palette(*palette);
                AppThemesHost::note_window_theme(ctx, self.theme_manager.generation());
            }
            Some(StudioThemeChange::Test(palette)) => {
                self.start_theme_trial(*palette);
                AppThemesHost::note_window_theme(ctx, self.theme_manager.generation());
            }
            Some(StudioThemeChange::Embedded(kind)) => self.choose_embedded_theme(kind, None),
            Some(StudioThemeChange::ToggleNight) => self.request_night_mode_toggle(),
            None => {
                let theme = &self.theme_manager;
                AppThemesHost::sync_window_theme(ctx, theme.generation(), || {
                    theme.palette().clone()
                });
            }
        }
    }

    /// Shows `palette` on trial with "Keep this theme?" (`Window::Theme::Apply` of a theme
    /// file, then `WarningWidget`). A trial started during another one keeps the first
    /// theme to revert to.
    pub(super) fn start_theme_trial(&mut self, palette: tg_theme::Palette) {
        if self.theme_follow.trial_previous.is_none() {
            self.theme_follow.trial_previous = Some(Box::new(self.theme_manager.palette().clone()));
        }
        self.theme_manager.set_palette(palette);
        self.layer_stack.show_box(ModalBox::KeepTheme {
            started: tg_app_core::clock::now(),
        });
    }

    /// "Keep changes" (`KeepApplied`) or "Revert" (`Revert`) for a theme on trial.
    pub(super) fn finish_theme_trial(&mut self, keep: bool) {
        if let Some(previous) = self.theme_follow.trial_previous.take()
            && !keep
        {
            self.theme_manager.set_palette(*previous);
        }
    }

    /// A built-in theme or accent colour was picked in Chat settings (`SetupDefaultThemes`,
    /// `apply`): a theme of the current mode is applied at once; a theme of the other mode
    /// is a night-mode switch, which asks first while auto-night mode forbids it.
    pub fn choose_embedded_theme(&mut self, kind: EmbeddedType, accent: Option<ThemeColor>) {
        if self.theme_manager.night_mode_state().is_night_mode() == kind.is_dark() {
            self.apply_theme(kind, accent);
        } else {
            self.switch_night_mode_with_confirmation(PendingNightSwitch::Theme { kind, accent });
        }
    }

    /// The Night Mode switch of the main menu (`ToggleNightModeWithConfirmation`).
    pub fn request_night_mode_toggle(&mut self) {
        self.switch_night_mode_with_confirmation(PendingNightSwitch::Toggle);
    }

    /// Sets the OS dark-mode value directly (`None`: the OS gives no signal).
    pub fn set_system_dark_mode(&mut self, system_dark: Option<bool>) {
        self.theme_follow.source = SystemDarkSource::Fixed(system_dark);
    }

    /// Takes over the theme of another window of the app. The theme is app-wide in tdesktop;
    /// here every account workspace has its own window controller, so the one that becomes
    /// active adopts the theme the previous one had.
    pub fn adopt_theme(&mut self, from: &ShellSettings) {
        let theirs = from.restored_theme_manager();
        self.settings.day_theme.clone_from(&from.day_theme);
        self.settings.night_theme.clone_from(&from.night_theme);
        for night in [false, true] {
            self.theme_manager.remember(theirs.saved_embedded(night));
        }
        let (kind, accent) = (theirs.embedded_type(), theirs.accent());
        if (kind, accent)
            != (
                self.theme_manager.embedded_type(),
                self.theme_manager.accent(),
            )
        {
            self.apply_theme(kind, accent);
        }
    }

    /// Whether auto-night mode (follow the system dark mode) is enabled.
    #[must_use]
    pub fn follows_system_dark_mode(&self) -> bool {
        self.follow_system_setting()
            .unwrap_or(FOLLOW_SYSTEM_DEFAULT)
    }

    /// `Application::checkSystemDarkMode`, run when the OS value or the auto-night setting
    /// changed since the last frame: switches night mode to match the OS if the setting is on.
    pub(super) fn sync_system_dark_mode(&mut self) {
        let live_window = self.session_backend.is_live_session();
        let system_dark = self.theme_follow.system_dark(live_window);
        // Chat settings shows the "Auto-night mode" row only while the system has a value.
        self.main_widget.system_dark_mode = system_dark;
        let Some(enabled) = self.follow_system_setting().or_else(|| {
            // A live account's setting is loaded from its `tdata`; never guess it.
            (!live_window).then_some(FOLLOW_SYSTEM_DEFAULT)
        }) else {
            return;
        };
        let state = self.theme_manager.night_mode_state_mut();
        if state.system_dark_mode_enabled() == enabled && state.system_dark_mode() == system_dark {
            return;
        }
        let _ = state.set_system_dark_mode_enabled(enabled);
        let switch = state.on_system_dark_mode_changed(system_dark);
        if let Some(switch) = switch {
            self.set_night_mode_and_keep(switch.target_night_mode);
        }
    }

    /// Tells the shared chat wallpaper whether the app's theme is dark. The window owns this
    /// flag: the chat history is not painted while Settings covers it or no chat is open, and
    /// the wallpaper thumbnail in Chat settings must still follow the theme (TGB-2962).
    /// tdesktop's `ChatBackground` learns the theme through its own `ApplyingTheme` update.
    pub(super) fn sync_chat_background_dark_mode(&self, ctx: &egui::Context) {
        let dark = self.theme_manager.is_dark();
        if let Ok(mut background) = tg_app_settings::themes::active_chat_background(ctx).lock() {
            background.set_dark_mode(dark);
        }
    }

    /// Requests one more frame when the theme changed since the last call. The host applies
    /// a new palette to egui at the start of the next frame; without a request that frame
    /// would wait for the next input event and the window would stay in the old colours.
    pub(super) fn repaint_after_theme_change(&mut self, ctx: &egui::Context) {
        let generation = self.theme_manager.generation();
        if self.theme_follow.repaint_generation != generation {
            self.theme_follow.repaint_generation = generation;
            ctx.request_repaint();
        }
    }

    /// Switches to the theme saved for day or night mode and persists it
    /// (`Window::Theme::ToggleNightMode` + `KeepApplied`).
    pub(super) fn set_night_mode_and_keep(&mut self, night: bool) {
        self.theme_manager.set_night_mode(night);
        self.settings.record_theme(
            self.theme_manager.embedded_type(),
            self.theme_manager.accent(),
        );
        self.mark_settings_dirty();
    }

    /// "Disable" in the auto-night confirmation: turns the setting off, then makes the
    /// switch that was waiting (`ToggleNightModeWithConfirmation`, `disableAndToggle`).
    pub(super) fn disable_auto_night_and_switch(&mut self) {
        let Some(switch) = self.theme_follow.pending.take() else {
            return;
        };
        let backend = self.session_backend.create_settings_backend();
        self.main_widget
            .ensure_settings(backend)
            .core_settings
            .system_dark_mode_enabled = false;
        let _ = self
            .theme_manager
            .night_mode_state_mut()
            .set_system_dark_mode_enabled(false);
        self.perform_night_switch(switch);
    }

    fn follow_system_setting(&self) -> Option<bool> {
        self.main_widget
            .settings()
            .map(|settings| settings.core_settings.system_dark_mode_enabled)
    }

    fn switch_night_mode_with_confirmation(&mut self, switch: PendingNightSwitch) {
        // The setting may have been flipped in Chat settings during this frame.
        self.sync_system_dark_mode();
        match self.theme_manager.night_mode_state().evaluate_toggle() {
            ToggleDecision::Allowed { .. } => self.perform_night_switch(switch),
            ToggleDecision::NeedsDisableSystemDarkModeConfirmation => {
                self.theme_follow.pending = Some(switch);
                self.layer_stack.show_box(ModalBox::ConfirmDisableAutoNight);
            }
        }
    }

    fn perform_night_switch(&mut self, switch: PendingNightSwitch) {
        match switch {
            PendingNightSwitch::Toggle => self.toggle_night_mode(),
            PendingNightSwitch::Theme { kind, accent } => self.apply_theme(kind, accent),
        }
    }
}

#[cfg(test)]
mod tests {
    use tg_app_core::stand_in::theme::ThemeMode;

    use super::*;
    use crate::mock_session::MockSession;
    use crate::window::main_widget::MainWidgetAction;

    fn controller() -> (WindowController, tempfile::TempDir) {
        tg_app_core::clock::init_test_clock();
        let dir = tempfile::tempdir().expect("temp dir");
        let ctrl = WindowController::with_session(
            MockSession::new(true),
            ShellSettings::default(),
            dir.path().join("shell_state.json"),
        );
        (ctrl, dir)
    }

    fn restart(ctrl: &mut WindowController, dir: &tempfile::TempDir) -> WindowController {
        ctrl.flush_settings();
        let path = dir.path().join("shell_state.json");
        WindowController::with_session(
            MockSession::new(true),
            ShellSettings::load_from(&path),
            path,
        )
    }

    fn has_confirmation(ctrl: &WindowController) -> bool {
        ctrl.layer_stack.is_layer_shown()
    }

    /// The actions the settings screen emits for one click on a theme card of the other
    /// mode. Before TGB-2955 it emitted `ApplyTheme` and then `ToggleTheme`, and the toggle
    /// flipped the freshly applied dark theme straight back to Classic.
    #[test]
    fn dark_theme_card_switches_to_dark_and_survives_a_restart() {
        let (mut ctrl, dir) = controller();
        let ctx = egui::Context::default();
        assert_eq!(ctrl.theme_mode(), ThemeMode::Day);

        ctrl.handle_main_widget_action(
            MainWidgetAction::ApplyTheme {
                kind: EmbeddedType::NightGreen,
                accent: None,
            },
            &ctx,
        );
        assert!(!has_confirmation(&ctrl));
        assert!(ctrl.theme_manager().is_dark());
        assert_eq!(
            ctrl.theme_manager().embedded_type(),
            EmbeddedType::NightGreen
        );
        assert_eq!(ctrl.theme_mode(), ThemeMode::Night);
        let bg = ctrl.palette().window_bg;
        assert!(u32::from(bg.r()) + u32::from(bg.g()) + u32::from(bg.b()) < 3 * 80);

        let restarted = restart(&mut ctrl, &dir);
        assert_eq!(
            restarted.theme_manager().embedded_type(),
            EmbeddedType::NightGreen
        );
        assert!(restarted.theme_manager().is_dark());
    }

    #[test]
    fn menu_night_toggle_returns_to_the_day_theme_that_was_chosen() {
        let (mut ctrl, dir) = controller();
        ctrl.choose_embedded_theme(EmbeddedType::DayBlue, None);
        ctrl.request_night_mode_toggle();
        assert_eq!(ctrl.theme_manager().embedded_type(), EmbeddedType::Night);
        assert_eq!(ctrl.theme_mode(), ThemeMode::Night);

        // The saved day theme survives a restart in night mode.
        let mut restarted = restart(&mut ctrl, &dir);
        assert_eq!(restarted.theme_mode(), ThemeMode::Night);
        restarted.request_night_mode_toggle();
        assert_eq!(
            restarted.theme_manager().embedded_type(),
            EmbeddedType::DayBlue
        );
        assert_eq!(restarted.theme_mode(), ThemeMode::Day);
    }

    #[test]
    fn system_dark_mode_is_followed_only_while_auto_night_is_enabled() {
        let (mut ctrl, _dir) = controller();
        assert!(ctrl.follows_system_dark_mode());

        // No OS signal: nothing follows.
        ctrl.sync_system_dark_mode();
        assert_eq!(ctrl.theme_mode(), ThemeMode::Day);

        ctrl.set_system_dark_mode(Some(true));
        ctrl.sync_system_dark_mode();
        assert_eq!(ctrl.theme_mode(), ThemeMode::Night);
        assert_eq!(ctrl.theme_manager().embedded_type(), EmbeddedType::Night);
        ctrl.set_system_dark_mode(Some(false));
        ctrl.sync_system_dark_mode();
        assert_eq!(ctrl.theme_mode(), ThemeMode::Day);

        // Disabled: the OS value no longer changes the theme.
        let backend = ctrl.session_backend.create_settings_backend();
        ctrl.main_widget
            .ensure_settings(backend)
            .core_settings
            .system_dark_mode_enabled = false;
        ctrl.set_system_dark_mode(Some(true));
        ctrl.sync_system_dark_mode();
        assert_eq!(ctrl.theme_mode(), ThemeMode::Day);
        ctrl.request_night_mode_toggle();
        assert!(!has_confirmation(&ctrl));
        assert_eq!(ctrl.theme_mode(), ThemeMode::Night);
        ctrl.sync_system_dark_mode();
        assert_eq!(ctrl.theme_mode(), ThemeMode::Night);

        // Enabling it again switches at once to the OS value.
        ctrl.set_system_dark_mode(Some(false));
        ctrl.main_widget
            .settings_mut()
            .expect("settings")
            .core_settings
            .system_dark_mode_enabled = true;
        ctrl.sync_system_dark_mode();
        assert_eq!(ctrl.theme_mode(), ThemeMode::Day);
    }

    /// Two account workspaces: the one switched to shows the theme chosen in the other.
    #[test]
    fn a_window_adopts_the_theme_of_the_window_it_replaces() {
        let (mut first, _dir_a) = controller();
        let (mut second, _dir_b) = controller();
        first.choose_embedded_theme(EmbeddedType::DayBlue, None);
        first.choose_embedded_theme(EmbeddedType::NightGreen, None);

        let generation = second.theme_generation();
        second.adopt_theme(first.settings());
        assert_eq!(
            second.theme_manager().embedded_type(),
            EmbeddedType::NightGreen
        );
        assert_eq!(second.theme_mode(), ThemeMode::Night);
        assert_ne!(
            second.theme_generation(),
            generation,
            "the host repaints egui"
        );
        second.request_night_mode_toggle();
        assert_eq!(
            second.theme_manager().embedded_type(),
            EmbeddedType::DayBlue
        );

        // Same theme already: nothing is rebuilt.
        let generation = first.theme_generation();
        let same = first.settings().clone();
        first.adopt_theme(&same);
        assert_eq!(first.theme_generation(), generation);
    }

    /// `ToggleNightModeWithConfirmation`: while auto-night mode is on, a switch away from
    /// the OS value asks to disable it first; "Disable" turns it off and switches.
    #[test]
    fn switch_against_the_system_asks_to_disable_auto_night_first() {
        let (mut ctrl, _dir) = controller();
        ctrl.set_system_dark_mode(Some(false));
        ctrl.sync_system_dark_mode();

        ctrl.choose_embedded_theme(EmbeddedType::NightGreen, None);
        assert!(has_confirmation(&ctrl));
        assert_eq!(
            ctrl.theme_mode(),
            ThemeMode::Day,
            "nothing changes before the answer"
        );
        // A theme of the current mode needs no confirmation.
        ctrl.layer_stack.hide_all();
        ctrl.choose_embedded_theme(EmbeddedType::DayBlue, None);
        assert!(!has_confirmation(&ctrl));
        assert_eq!(ctrl.theme_manager().embedded_type(), EmbeddedType::DayBlue);

        ctrl.choose_embedded_theme(EmbeddedType::NightGreen, None);
        assert!(has_confirmation(&ctrl));
        ctrl.handle_layer_action(crate::window::layer_stack::LayerAction::DisableAutoNight);
        assert!(!ctrl.follows_system_dark_mode());
        assert_eq!(
            ctrl.theme_manager().embedded_type(),
            EmbeddedType::NightGreen
        );
        // The OS still says light, and no longer wins.
        ctrl.sync_system_dark_mode();
        assert_eq!(ctrl.theme_mode(), ThemeMode::Night);

        // Towards the OS value the switch is allowed while auto-night is on.
        let (mut ctrl, _dir) = controller();
        ctrl.set_system_dark_mode(Some(false));
        ctrl.sync_system_dark_mode();
        ctrl.toggle_night_mode();
        ctrl.request_night_mode_toggle();
        assert!(!has_confirmation(&ctrl));
        assert_eq!(ctrl.theme_mode(), ThemeMode::Day);
    }
}
