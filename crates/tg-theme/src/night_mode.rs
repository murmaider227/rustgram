//! Night mode and system dark mode following state machine.
//!
//! Pure logic port of night mode / `systemDarkMode` / `systemDarkModeEnabled`
//! from `SourceFiles/window/themes/window_theme.cpp` and `SourceFiles/core/application.cpp`.
//! The OS dark mode query itself lives in `tg-platform`.

use crate::embedded::NIGHT_THEME_PATH;

/// Outcome of requesting a manual night mode toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToggleDecision {
    /// Switching is allowed; caller should transition to `new_night_mode`.
    Allowed {
        /// The night mode value after toggling.
        new_night_mode: bool,
    },
    /// System dark mode following is active and disagrees with the requested toggle.
    /// UI should ask the user to disable automatic system dark mode following
    /// (mirrors `ToggleNightModeWithConfirmation`).
    NeedsDisableSystemDarkModeConfirmation,
}

/// Action recommended when system dark mode or the follow setting changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NightModeSwitch {
    /// Target night mode value (`true` = dark, `false` = day).
    pub target_night_mode: bool,
    /// Default built-in theme path if no saved theme exists for that mode.
    pub default_theme_path: &'static str,
}

/// Pure state machine tracking night mode, system dark mode following,
/// theme editing lock, and separate day/night background tile flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NightModeState {
    night_mode: bool,
    system_dark_mode_enabled: bool,
    system_dark_mode: Option<bool>,
    editing_theme: bool,
    tile_day: bool,
    tile_night: bool,
}

impl Default for NightModeState {
    fn default() -> Self {
        Self {
            night_mode: false,
            system_dark_mode_enabled: false,
            system_dark_mode: None,
            editing_theme: false,
            // Mirrors ChatBackground defaults: _tileDayValue = false, _tileNightValue = true.
            tile_day: false,
            tile_night: true,
        }
    }
}

impl NightModeState {
    /// Creates a new state with explicit initial night mode and system dark mode follow flag.
    #[must_use]
    pub fn new(night_mode: bool, system_dark_mode_enabled: bool) -> Self {
        Self {
            night_mode,
            system_dark_mode_enabled,
            ..Self::default()
        }
    }

    /// Returns whether night mode is currently active (`IsNightMode`).
    #[must_use]
    pub const fn is_night_mode(&self) -> bool {
        self.night_mode
    }

    /// Sets the stored night mode value directly (`SetNightModeValue`).
    pub fn set_night_mode(&mut self, night_mode: bool) {
        self.night_mode = night_mode;
    }

    /// Returns whether following OS dark mode is enabled (`settings.systemDarkModeEnabled()`).
    #[must_use]
    pub const fn system_dark_mode_enabled(&self) -> bool {
        self.system_dark_mode_enabled
    }

    /// Returns the last reported OS dark mode state (`settings.systemDarkMode()`).
    #[must_use]
    pub const fn system_dark_mode(&self) -> Option<bool> {
        self.system_dark_mode
    }

    /// Returns whether the theme editor is currently open.
    #[must_use]
    pub const fn is_editing_theme(&self) -> bool {
        self.editing_theme
    }

    /// Updates theme editor state. Mirrors `Application::startSystemDarkModeViewer`:
    /// entering theme editing disables automatic system dark mode following.
    pub fn set_editing_theme(&mut self, editing: bool) {
        self.editing_theme = editing;
        if editing {
            self.system_dark_mode_enabled = false;
        }
    }

    /// Returns the active wallpaper tile flag for the current day/night mode.
    #[must_use]
    pub const fn tile(&self) -> bool {
        if self.night_mode {
            self.tile_night
        } else {
            self.tile_day
        }
    }

    /// Sets the wallpaper tile flag for the current day/night mode.
    pub fn set_tile(&mut self, tile: bool) {
        if self.night_mode {
            self.tile_night = tile;
        } else {
            self.tile_day = tile;
        }
    }

    /// Returns the stored day-mode tile flag (`tileDay`).
    #[must_use]
    pub const fn tile_day(&self) -> bool {
        self.tile_day
    }

    /// Sets the day-mode tile flag (`setTileDayValue`).
    pub fn set_tile_day(&mut self, tile: bool) {
        self.tile_day = tile;
    }

    /// Returns the stored night-mode tile flag (`tileNight`).
    #[must_use]
    pub const fn tile_night(&self) -> bool {
        self.tile_night
    }

    /// Sets the night-mode tile flag (`setTileNightValue`).
    pub fn set_tile_night(&mut self, tile: bool) {
        self.tile_night = tile;
    }

    /// Returns whether switching night mode is allowed without first disabling
    /// system dark mode following (`ChatBackground::nightModeChangeAllowed`).
    #[must_use]
    pub const fn night_mode_change_allowed(&self) -> bool {
        let allowed_after_change = if self.system_dark_mode_enabled {
            match self.system_dark_mode {
                Some(sys) => sys,
                None => !self.night_mode,
            }
        } else {
            !self.night_mode
        };
        self.night_mode != allowed_after_change
    }

    /// Checks whether system dark mode requires switching night mode
    /// (`Application::checkSystemDarkMode`).
    #[must_use]
    pub const fn pending_system_switch(&self) -> Option<NightModeSwitch> {
        if !self.system_dark_mode_enabled {
            return None;
        }
        let Some(sys_dark) = self.system_dark_mode else {
            return None;
        };
        if sys_dark != self.night_mode {
            Some(NightModeSwitch {
                target_night_mode: sys_dark,
                default_theme_path: default_theme_path(sys_dark),
            })
        } else {
            None
        }
    }

    /// Feeds an OS dark mode update from `tg-platform` and returns a switch
    /// action if night mode should change.
    pub fn on_system_dark_mode_changed(
        &mut self,
        is_dark: Option<bool>,
    ) -> Option<NightModeSwitch> {
        self.system_dark_mode = is_dark;
        self.pending_system_switch()
    }

    /// Enables or disables following OS dark mode and returns a switch action
    /// if enabling causes night mode to align with the OS state.
    pub fn set_system_dark_mode_enabled(&mut self, enabled: bool) -> Option<NightModeSwitch> {
        self.system_dark_mode_enabled = enabled;
        self.pending_system_switch()
    }

    /// Applies a pending switch (or confirmed toggle) by updating `night_mode`.
    pub fn apply_night_mode(&mut self, new_night_mode: bool) -> bool {
        if self.night_mode != new_night_mode && !self.night_mode_change_allowed() {
            return false;
        }
        self.night_mode = new_night_mode;
        true
    }

    /// Evaluates a manual toggle request (`ToggleNightModeWithConfirmation`).
    #[must_use]
    pub const fn evaluate_toggle(&self) -> ToggleDecision {
        if self.night_mode_change_allowed() {
            ToggleDecision::Allowed {
                new_night_mode: !self.night_mode,
            }
        } else {
            ToggleDecision::NeedsDisableSystemDarkModeConfirmation
        }
    }

    /// Disables system dark mode following and toggles night mode immediately
    /// (the confirmed branch of `ToggleNightModeWithConfirmation`).
    pub fn disable_system_follow_and_toggle(&mut self) -> NightModeSwitch {
        self.system_dark_mode_enabled = false;
        self.night_mode = !self.night_mode;
        NightModeSwitch {
            target_night_mode: self.night_mode,
            default_theme_path: default_theme_path(self.night_mode),
        }
    }
}

/// Returns the default built-in theme path for a day (`""`) or night (`":/gui/night.tdesktop-theme"`) mode.
#[must_use]
pub const fn default_theme_path(night_mode: bool) -> &'static str {
    if night_mode { NIGHT_THEME_PATH } else { "" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_toggle_without_system_follow() {
        let mut st = NightModeState::default();
        assert!(!st.is_night_mode());
        assert!(!st.tile()); // day default false
        assert!(st.tile_night()); // night default true
        assert_eq!(
            st.evaluate_toggle(),
            ToggleDecision::Allowed {
                new_night_mode: true
            }
        );
        assert!(st.apply_night_mode(true));
        assert!(st.is_night_mode());
        assert!(st.tile()); // now night default true
    }

    #[test]
    fn system_dark_mode_follow_transitions() {
        let mut st = NightModeState::new(false, true);
        // OS reports dark mode -> should switch to night
        let sw = st.on_system_dark_mode_changed(Some(true)).unwrap();
        assert!(sw.target_night_mode);
        assert_eq!(sw.default_theme_path, NIGHT_THEME_PATH);
        assert!(st.night_mode_change_allowed());
        assert!(st.apply_night_mode(sw.target_night_mode));
        assert!(st.is_night_mode());

        // Now synced with OS dark=true; manual toggle needs confirmation!
        assert!(!st.night_mode_change_allowed());
        assert_eq!(
            st.evaluate_toggle(),
            ToggleDecision::NeedsDisableSystemDarkModeConfirmation
        );
        assert!(!st.apply_night_mode(false));

        // Confirmed disable + toggle succeeds
        let sw2 = st.disable_system_follow_and_toggle();
        assert!(!sw2.target_night_mode);
        assert_eq!(sw2.default_theme_path, "");
        assert!(!st.system_dark_mode_enabled());
        assert!(!st.is_night_mode());
    }

    #[test]
    fn editing_theme_disables_system_dark_mode_follow() {
        let mut st = NightModeState::new(false, true);
        st.set_editing_theme(true);
        assert!(!st.system_dark_mode_enabled());
        assert!(st.is_editing_theme());
    }
}
