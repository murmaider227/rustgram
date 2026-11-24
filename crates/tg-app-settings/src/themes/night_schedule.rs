//! Night-mode schedule and automatic OS dark mode switching via `tg-platform`.
//!
//! Port of `Telegram/SourceFiles/core/application.cpp` (`checkSystemDarkMode`,
//! `startSystemDarkModeViewer`) and `Telegram/SourceFiles/window/themes/window_theme.cpp`
//! (`ToggleNightModeWithConfirmation`), plus a local time-of-day schedule option.

use std::time::{Duration, Instant};

use tg_theme::{NightModeState, NightModeSwitch, ToggleDecision};
use tg_ui::theme::ThemeManager;

/// Default polling interval for OS dark mode queries (avoids per-frame registry overhead).
const SYSTEM_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Night mode automation strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NightScheduleMode {
    /// Manual Day / Night selection only.
    Manual,
    /// Automatically follow Windows system dark mode via `tg_platform::is_dark_mode()`.
    #[default]
    System,
    /// Automatically enable Night Mode during a local time window (`start_minutes..end_minutes`
    /// in `0..1440`; wraps across midnight when `start_minutes > end_minutes`).
    Scheduled {
        /// Night start time in minutes since midnight (e.g. `22 * 60 = 1320` for 22:00).
        start_minutes: u16,
        /// Night end time in minutes since midnight (e.g. `7 * 60 = 420` for 07:00).
        end_minutes: u16,
    },
}

impl NightScheduleMode {
    /// Default scheduled window (22:00 – 07:00).
    pub const DEFAULT_SCHEDULE: Self = Self::Scheduled {
        start_minutes: 22 * 60,
        end_minutes: 7 * 60,
    };

    /// Evaluates whether `minute_of_day` (`0..1440`) falls inside a scheduled night window.
    #[must_use]
    pub const fn is_night_at_minute(
        start_minutes: u16,
        end_minutes: u16,
        minute_of_day: u16,
    ) -> bool {
        let m = minute_of_day % 1440;
        let s = start_minutes % 1440;
        let e = end_minutes % 1440;
        if s == e {
            return false;
        }
        if s < e {
            m >= s && m < e
        } else {
            // Spans midnight (e.g. 22:00 .. 07:00).
            m >= s || m < e
        }
    }
}

/// Formats minutes-since-midnight (`0..1440`) as `"HH:MM"`.
#[must_use]
pub fn format_minutes_hhmm(minutes: u16) -> String {
    let clamped = minutes % 1440;
    format!("{:02}:{:02}", clamped / 60, clamped % 60)
}

/// Result returned when polling or updating [`NightModeScheduler`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NightScheduleEvent {
    /// No mode change needed.
    Unchanged,
    /// Night mode transitioned to `night_mode`.
    Switched {
        /// New active night mode value (`true` = dark, `false` = day).
        night_mode: bool,
    },
    /// User attempted a manual toggle while System auto-switch is enabled and disagrees;
    /// UI should prompt to disable system dark mode following.
    NeedsConfirmDisableSystem,
}

/// Coordinates `tg_theme::NightModeState`, `tg_platform::is_dark_mode()`, and optional
/// time-of-day scheduling.
#[derive(Debug, Clone)]
pub struct NightModeScheduler {
    mode: NightScheduleMode,
    state: NightModeState,
    last_poll: Option<Instant>,
    poll_interval: Duration,
    /// Cached last time-of-day schedule window even when switching to Manual/System.
    saved_start_minutes: u16,
    saved_end_minutes: u16,
    /// Whether a manual toggle confirmation dialog is currently pending.
    pub pending_disable_system_confirm: bool,
}

impl Default for NightModeScheduler {
    fn default() -> Self {
        Self::new(NightScheduleMode::System, false)
    }
}

impl NightModeScheduler {
    /// Creates a new scheduler with an explicit mode and initial night-mode state.
    #[must_use]
    pub fn new(mode: NightScheduleMode, initial_night_mode: bool) -> Self {
        let follow_system = matches!(mode, NightScheduleMode::System);
        let (saved_start_minutes, saved_end_minutes) = match mode {
            NightScheduleMode::Scheduled {
                start_minutes,
                end_minutes,
            } => (start_minutes % 1440, end_minutes % 1440),
            _ => (22 * 60, 7 * 60),
        };
        Self {
            mode,
            state: NightModeState::new(initial_night_mode, follow_system),
            last_poll: None,
            poll_interval: SYSTEM_POLL_INTERVAL,
            saved_start_minutes,
            saved_end_minutes,
            pending_disable_system_confirm: false,
        }
    }

    /// Returns the active [`NightScheduleMode`].
    #[must_use]
    pub const fn mode(&self) -> NightScheduleMode {
        self.mode
    }

    /// Returns the underlying `tg_theme::NightModeState` state machine.
    #[must_use]
    pub const fn state(&self) -> &NightModeState {
        &self.state
    }

    /// Returns whether night mode is currently active.
    #[must_use]
    pub const fn is_night_mode(&self) -> bool {
        self.state.is_night_mode()
    }

    /// Returns the last observed Windows system dark mode value from `tg-platform`.
    #[must_use]
    pub const fn last_system_dark_mode(&self) -> Option<bool> {
        self.state.system_dark_mode()
    }

    /// Returns `(start_minutes, end_minutes)` saved for [`NightScheduleMode::Scheduled`].
    #[must_use]
    pub const fn schedule_window(&self) -> (u16, u16) {
        (self.saved_start_minutes, self.saved_end_minutes)
    }

    /// Updates the schedule mode and returns any immediate transition event.
    pub fn set_mode(
        &mut self,
        mode: NightScheduleMode,
        system_dark: Option<bool>,
        current_minute_of_day: u16,
    ) -> NightScheduleEvent {
        self.mode = mode;
        self.pending_disable_system_confirm = false;
        if let NightScheduleMode::Scheduled {
            start_minutes,
            end_minutes,
        } = mode
        {
            self.saved_start_minutes = start_minutes % 1440;
            self.saved_end_minutes = end_minutes % 1440;
        }
        let follow = matches!(mode, NightScheduleMode::System);
        if let Some(sw) = self.state.set_system_dark_mode_enabled(follow) {
            return self.apply_switch(sw);
        }
        self.evaluate(system_dark, current_minute_of_day)
    }

    /// Updates the theme editor lock. Entering theme editing disables automatic system
    /// dark mode following (`Application::startSystemDarkModeViewer` / `window_theme_editor_box.cpp`).
    pub fn set_editing_theme(&mut self, editing: bool) {
        self.state.set_editing_theme(editing);
        if editing && matches!(self.mode, NightScheduleMode::System) {
            self.mode = NightScheduleMode::Manual;
        }
    }

    /// Requests a manual toggle (`ToggleNightModeWithConfirmation`).
    pub fn request_manual_toggle(&mut self) -> NightScheduleEvent {
        match self.state.evaluate_toggle() {
            ToggleDecision::Allowed { new_night_mode } => {
                if self.state.apply_night_mode(new_night_mode) {
                    if matches!(self.mode, NightScheduleMode::Scheduled { .. }) {
                        self.mode = NightScheduleMode::Manual;
                    }
                    NightScheduleEvent::Switched {
                        night_mode: new_night_mode,
                    }
                } else {
                    NightScheduleEvent::Unchanged
                }
            }
            ToggleDecision::NeedsDisableSystemDarkModeConfirmation => {
                self.pending_disable_system_confirm = true;
                NightScheduleEvent::NeedsConfirmDisableSystem
            }
        }
    }

    /// Confirms disabling system dark mode following and toggles night mode immediately.
    pub fn confirm_disable_system_and_toggle(&mut self) -> NightScheduleEvent {
        self.pending_disable_system_confirm = false;
        self.mode = NightScheduleMode::Manual;
        let sw = self.state.disable_system_follow_and_toggle();
        NightScheduleEvent::Switched {
            night_mode: sw.target_night_mode,
        }
    }

    /// Pure deterministic evaluation using caller-supplied OS dark mode and minute-of-day
    /// (used both by [`Self::poll_platform`] and unit tests).
    pub fn evaluate(
        &mut self,
        system_dark: Option<bool>,
        current_minute_of_day: u16,
    ) -> NightScheduleEvent {
        if self.state.is_editing_theme() {
            return NightScheduleEvent::Unchanged;
        }
        match self.mode {
            NightScheduleMode::Manual => NightScheduleEvent::Unchanged,
            NightScheduleMode::System => {
                if let Some(sw) = self.state.on_system_dark_mode_changed(system_dark) {
                    self.apply_switch(sw)
                } else {
                    NightScheduleEvent::Unchanged
                }
            }
            NightScheduleMode::Scheduled {
                start_minutes,
                end_minutes,
            } => {
                let want_night = NightScheduleMode::is_night_at_minute(
                    start_minutes,
                    end_minutes,
                    current_minute_of_day,
                );
                if want_night != self.state.is_night_mode() {
                    self.state.set_night_mode(want_night);
                    NightScheduleEvent::Switched {
                        night_mode: want_night,
                    }
                } else {
                    NightScheduleEvent::Unchanged
                }
            }
        }
    }

    /// Polls `tg_platform::is_dark_mode()` and local wall-clock time on `poll_interval`,
    /// applying any resulting transition to `theme_manager`.
    pub fn poll_platform(&mut self, theme_manager: &mut ThemeManager) -> NightScheduleEvent {
        let now = Instant::now();
        if let Some(last) = self.last_poll
            && now.duration_since(last) < self.poll_interval
        {
            return NightScheduleEvent::Unchanged;
        }
        self.last_poll = Some(now);
        let sys_dark = tg_platform::system_info::is_dark_mode();
        let minute = current_local_minute_of_day();
        let event = self.evaluate(sys_dark, minute);
        if let NightScheduleEvent::Switched { night_mode } = event {
            theme_manager.set_night_mode(night_mode);
        }
        event
    }

    /// Synchronizes scheduler state when `theme_manager`'s night mode changes externally.
    pub fn sync_from_theme_manager(&mut self, theme_manager: &ThemeManager) {
        self.state.set_night_mode(theme_manager.is_dark());
    }

    fn apply_switch(&mut self, sw: NightModeSwitch) -> NightScheduleEvent {
        if self.state.apply_night_mode(sw.target_night_mode) {
            NightScheduleEvent::Switched {
                night_mode: sw.target_night_mode,
            }
        } else {
            NightScheduleEvent::Unchanged
        }
    }
}

/// Returns the current local time as minutes since midnight (`0..1440`).
#[must_use]
pub fn current_local_minute_of_day() -> u16 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    ((secs / 60) % 1440) as u16
}
