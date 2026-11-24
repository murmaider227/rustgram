//! Local passcode lock screen flow and flood-protection logic.
//!
//! Port origin: `Telegram/SourceFiles/window/window_lock_widgets.h`
//! and `Telegram/SourceFiles/window/window_lock_widgets.cpp`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Align, CornerRadius, Frame, Key, Layout, Margin, RichText, Stroke, TextEdit, Ui, Vec2};
use tg_platform::windows_hello::{WindowsHelloResult, WindowsHelloVerifier};

use crate::window::platform_call::{
    PLATFORM_CALL_REPAINT_INTERVAL, PlatformCall, PlatformCallState,
};
use tg_app_core::stand_in::strings::Strings;
use tg_app_core::stand_in::theme::Palette;

/// Result of one Windows Hello prompt as delivered by the worker thread.
type HelloOutcome = Result<WindowsHelloResult, String>;

/// Immediate verdict when submitting a passcode (`Window::PasscodeAttempt`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasscodeAttempt {
    /// Input was empty.
    Empty,
    /// Too many failed tries recently; caller must wait.
    Flood,
    /// Another verification attempt is currently in progress.
    Busy,
    /// Verification ran and succeeded.
    Unlocked,
    /// Verification ran and failed.
    Wrong,
}

/// Actions emitted by the passcode lock screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasscodeLockAction {
    /// Passcode verified; unlock the window.
    Unlock,
    /// User clicked Log Out from the lock screen.
    RequestLogout,
}

/// Stateful local passcode lock screen (`Window::PasscodeLockWidget`).
#[derive(Default)]
pub struct PasscodeLockWidget {
    input: String,
    error: String,
    bad_tries: u32,
    last_bad_try: Option<Instant>,
    deriving: bool,
    request_focus: bool,
    system_unlock_enabled: bool,
    window_hwnd: isize,
    windows_hello_verifier: Option<Arc<dyn WindowsHelloVerifier>>,
    /// Windows Hello prompt in flight on a worker thread (`suggestSystemUnlock`).
    hello_pending: Option<PlatformCall<HelloOutcome>>,
}

impl std::fmt::Debug for PasscodeLockWidget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PasscodeLockWidget")
            .field("error", &self.error)
            .field("bad_tries", &self.bad_tries)
            .field("deriving", &self.deriving)
            .field("system_unlock_enabled", &self.system_unlock_enabled)
            .field("has_windows_hello", &self.windows_hello_verifier.is_some())
            .field("hello_pending", &self.hello_pending.is_some())
            .finish()
    }
}

impl PasscodeLockWidget {
    /// Creates a fresh lock widget ready to receive keyboard focus.
    #[must_use]
    pub fn new() -> Self {
        Self {
            request_focus: true,
            ..Self::default()
        }
    }

    /// Resets transient input/error when locking again.
    pub fn reset_on_lock(&mut self) {
        self.input.clear();
        self.error.clear();
        self.deriving = false;
        self.request_focus = true;
    }

    /// Configures whether Windows Hello biometric/PIN unlock is enabled for this lock screen.
    pub fn set_system_unlock_enabled(&mut self, enabled: bool) {
        self.system_unlock_enabled = enabled;
    }

    /// Sets the native Win32 `HWND` anchoring Windows Hello consent dialogs.
    pub fn set_window_hwnd(&mut self, hwnd: isize) {
        self.window_hwnd = hwnd;
    }

    /// Injects the [`WindowsHelloVerifier`] used when the user triggers Windows Hello unlock.
    pub fn set_windows_hello_verifier(&mut self, verifier: Option<Arc<dyn WindowsHelloVerifier>>) {
        self.windows_hello_verifier = verifier;
    }

    /// Starts a Windows Hello prompt (`UserConsentVerifier`) on a worker thread.
    ///
    /// Mirrors `PasscodeLockWidget::suggestSystemUnlock` in `window_lock_widgets.cpp:227-255`:
    /// the window keeps painting while the system dialog is open, and
    /// [`Self::poll_windows_hello_unlock`] delivers the verdict (`systemUnlockDone`).
    /// Returns [`PasscodeAttempt::Busy`] while a prompt is already open and
    /// [`PasscodeAttempt::Empty`] when Windows Hello is disabled or not wired.
    pub fn start_windows_hello_unlock(
        &mut self,
        repaint: Option<egui::Context>,
    ) -> PasscodeAttempt {
        if !self.system_unlock_enabled {
            return PasscodeAttempt::Empty;
        }
        if self.hello_pending.is_some() {
            return PasscodeAttempt::Busy;
        }
        let Some(verifier) = self.windows_hello_verifier.clone() else {
            return PasscodeAttempt::Empty;
        };
        let prompt = tg_lang::tr::lng_passcode_winhello_unlock();
        let hwnd = self.window_hwnd;
        self.error.clear();
        self.hello_pending = Some(PlatformCall::spawn("winhello-unlock", repaint, move || {
            verifier
                .request_verification(hwnd, &prompt)
                .map_err(|e| e.to_string())
        }));
        PasscodeAttempt::Busy
    }

    /// Returns `true` while a Windows Hello prompt is open.
    #[must_use]
    pub const fn is_windows_hello_pending(&self) -> bool {
        self.hello_pending.is_some()
    }

    /// Collects the verdict of the Windows Hello prompt started by
    /// [`Self::start_windows_hello_unlock`] (`systemUnlockDone`), or `None` while it is open.
    pub fn poll_windows_hello_unlock(&mut self, strings: &dyn Strings) -> Option<PasscodeAttempt> {
        let outcome = match self.hello_pending.as_ref()?.poll() {
            PlatformCallState::Pending => return None,
            PlatformCallState::Done(outcome) => outcome,
            PlatformCallState::Lost => Err("the prompt closed unexpectedly".to_owned()),
        };
        self.hello_pending = None;
        Some(self.apply_windows_hello_outcome(outcome, strings))
    }

    /// Blocks up to `timeout` for the open Windows Hello prompt (tests only, never a UI frame).
    #[doc(hidden)]
    pub fn wait_windows_hello_unlock(
        &mut self,
        timeout: Duration,
        strings: &dyn Strings,
    ) -> Option<PasscodeAttempt> {
        let outcome = match self.hello_pending.as_ref()?.wait(timeout) {
            PlatformCallState::Pending => return None,
            PlatformCallState::Done(outcome) => outcome,
            PlatformCallState::Lost => Err("the prompt closed unexpectedly".to_owned()),
        };
        self.hello_pending = None;
        Some(self.apply_windows_hello_outcome(outcome, strings))
    }

    fn apply_windows_hello_outcome(
        &mut self,
        outcome: HelloOutcome,
        strings: &dyn Strings,
    ) -> PasscodeAttempt {
        match outcome {
            Ok(WindowsHelloResult::Verified) => {
                self.input.clear();
                self.error.clear();
                self.bad_tries = 0;
                self.last_bad_try = None;
                PasscodeAttempt::Unlocked
            }
            Ok(WindowsHelloResult::RetriesExhausted) => {
                self.error = strings.passcode_flood().to_owned();
                PasscodeAttempt::Flood
            }
            Ok(WindowsHelloResult::Canceled) => PasscodeAttempt::Wrong,
            Ok(_) => {
                self.error = "Windows Hello is unavailable.".to_owned();
                PasscodeAttempt::Wrong
            }
            Err(err) => {
                self.error = format!("Windows Hello failed: {err}");
                PasscodeAttempt::Wrong
            }
        }
    }

    /// Computes required flood cooldown after `bad_tries` failed attempts.
    ///
    /// Matches `tdesktop`'s `passcodeCanTry()` escalation (first 3 tries free,
    /// then 5s / 10s / 15s / 20s / 25s / 30s).
    #[must_use]
    pub const fn flood_cooldown_for_tries(bad_tries: u32) -> Duration {
        if bad_tries < 3 {
            Duration::ZERO
        } else {
            let step = bad_tries - 2;
            let secs = if step >= 6 { 30 } else { (step as u64) * 5 };
            Duration::from_secs(secs)
        }
    }

    /// Returns `true` if a new passcode attempt is allowed at `now`.
    #[must_use]
    pub fn can_try(&self, now: Instant) -> bool {
        let Some(last) = self.last_bad_try else {
            return true;
        };
        let cooldown = Self::flood_cooldown_for_tries(self.bad_tries);
        now.saturating_duration_since(last) >= cooldown
    }

    /// Evaluates a passcode attempt against `expected_passcode`.
    ///
    /// Mirrors `Window::TryPasscode()` in `window_lock_widgets.cpp`.
    pub fn try_passcode(
        &mut self,
        expected_passcode: &str,
        now: Instant,
        strings: &dyn Strings,
    ) -> PasscodeAttempt {
        self.try_passcode_with(|candidate| candidate == expected_passcode, now, strings)
    }

    /// Evaluates a passcode attempt using an arbitrary verification predicate (e.g.
    /// `AccountManager::check_local_passcode` in live mode).
    pub fn try_passcode_with(
        &mut self,
        verify: impl FnOnce(&str) -> bool,
        now: Instant,
        strings: &dyn Strings,
    ) -> PasscodeAttempt {
        if self.deriving {
            return PasscodeAttempt::Busy;
        }
        if self.input.is_empty() {
            return PasscodeAttempt::Empty;
        }
        if !self.can_try(now) {
            self.error = strings.passcode_flood().to_owned();
            return PasscodeAttempt::Flood;
        }
        if verify(&self.input) {
            self.input.clear();
            self.error.clear();
            self.bad_tries = 0;
            self.last_bad_try = None;
            PasscodeAttempt::Unlocked
        } else {
            self.bad_tries = self.bad_tries.saturating_add(1);
            self.last_bad_try = Some(now);
            self.error = strings.passcode_wrong().to_owned();
            PasscodeAttempt::Wrong
        }
    }

    /// Renders the full-window passcode lock screen comparing against `expected_passcode`.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        expected_passcode: &str,
        palette: &Palette,
        strings: &dyn Strings,
    ) -> Option<PasscodeLockAction> {
        self.show_with(
            ui,
            |candidate| candidate == expected_passcode,
            palette,
            strings,
        )
    }

    /// Renders the full-window passcode lock screen verifying submissions via `verify`.
    pub fn show_with(
        &mut self,
        ui: &mut Ui,
        verify: impl FnOnce(&str) -> bool,
        palette: &Palette,
        strings: &dyn Strings,
    ) -> Option<PasscodeLockAction> {
        let mut action = None;
        if self.hello_pending.is_some() {
            match self.poll_windows_hello_unlock(strings) {
                Some(PasscodeAttempt::Unlocked) => action = Some(PasscodeLockAction::Unlock),
                Some(_) => {}
                None => ui
                    .ctx()
                    .request_repaint_after(PLATFORM_CALL_REPAINT_INTERVAL),
            }
        }
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, palette.window_bg);

        let card_width = 320.0_f32.min(rect.width() - 32.0).max(220.0);
        let top_skip = (rect.height() * 0.28).max(40.0);

        ui.vertical_centered(|ui| {
            ui.add_space(top_skip);

            Frame::NONE
                .fill(palette.window_bg)
                .inner_margin(Margin::symmetric(16, 12))
                .show(ui, |ui| {
                    ui.set_width(card_width);
                    ui.with_layout(Layout::top_down(Align::Center), |ui| {
                        ui.label(
                            RichText::new(strings.passcode_enter())
                                .size(19.0)
                                .strong()
                                .color(palette.window_bold_fg),
                        );
                        ui.add_space(18.0);

                        let prev_len = self.input.len();
                        let edit = ui.add_sized(
                            Vec2::new(card_width, 38.0),
                            TextEdit::singleline(&mut self.input)
                                .password(true)
                                .hint_text(strings.passcode_placeholder()),
                        );
                        if self.request_focus {
                            edit.request_focus();
                            self.request_focus = false;
                        }
                        if self.input.len() != prev_len && !self.error.is_empty() {
                            self.error.clear();
                        }

                        let enter_pressed =
                            edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));

                        ui.add_space(10.0);
                        if !self.error.is_empty() {
                            ui.label(
                                RichText::new(&self.error)
                                    .size(13.0)
                                    .color(palette.attention_fg),
                            );
                        } else {
                            ui.add_space(16.0);
                        }

                        ui.add_space(10.0);
                        let submit_btn = ui.add_sized(
                            Vec2::new(card_width, 40.0),
                            egui::Button::new(
                                RichText::new(strings.passcode_submit())
                                    .size(14.5)
                                    .strong()
                                    .color(palette.window_fg_active),
                            )
                            .fill(palette.window_bg_active)
                            .stroke(Stroke::NONE)
                            .corner_radius(CornerRadius::same(6)),
                        );

                        if submit_btn.clicked() || enter_pressed {
                            if self.try_passcode_with(verify, tg_app_core::clock::now(), strings)
                                == PasscodeAttempt::Unlocked
                            {
                                action = Some(PasscodeLockAction::Unlock);
                            } else {
                                self.request_focus = true;
                            }
                        }

                        if self.system_unlock_enabled && self.windows_hello_verifier.is_some() {
                            ui.add_space(10.0);
                            let waiting = self.hello_pending.is_some();
                            let label = if waiting {
                                "Waiting for Windows Hello\u{2026}"
                            } else {
                                "Unlock with Windows Hello"
                            };
                            let hello_btn = ui.add_enabled(
                                !waiting,
                                egui::Button::new(
                                    RichText::new(label)
                                        .size(13.5)
                                        .color(palette.window_active_text_fg),
                                )
                                .min_size(Vec2::new(card_width, 36.0))
                                .fill(palette.window_bg_over)
                                .stroke(Stroke::NONE)
                                .corner_radius(CornerRadius::same(6)),
                            );
                            if hello_btn.clicked() {
                                let _ = self.start_windows_hello_unlock(Some(ui.ctx().clone()));
                            }
                        }

                        ui.add_space(16.0);
                        let logout_btn = ui.add(
                            egui::Button::new(
                                RichText::new(strings.menu_logout())
                                    .size(13.5)
                                    .color(palette.window_active_text_fg),
                            )
                            .frame(false),
                        );
                        if logout_btn.clicked() {
                            action = Some(PasscodeLockAction::RequestLogout);
                        }
                    });
                });
        });

        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_app_core::stand_in::strings::EnglishStrings;

    #[test]
    fn empty_wrong_flood_and_unlock_flow() {
        let strings = EnglishStrings;
        let mut lock = PasscodeLockWidget::new();
        let t0 = Instant::now();

        assert_eq!(
            lock.try_passcode("4242", t0, &strings),
            PasscodeAttempt::Empty
        );

        lock.input = "0000".to_owned();
        assert_eq!(
            lock.try_passcode("4242", t0, &strings),
            PasscodeAttempt::Wrong
        );
        assert_eq!(
            lock.try_passcode("4242", t0, &strings),
            PasscodeAttempt::Wrong
        );
        assert_eq!(
            lock.try_passcode("4242", t0, &strings),
            PasscodeAttempt::Wrong
        );

        // 4th try immediately after 3 failures triggers flood protection.
        assert_eq!(
            lock.try_passcode("4242", t0, &strings),
            PasscodeAttempt::Flood
        );

        // After cooldown (5s) passes, correct passcode unlocks cleanly.
        lock.input = "4242".to_owned();
        assert_eq!(
            lock.try_passcode("4242", t0 + Duration::from_secs(6), &strings),
            PasscodeAttempt::Unlocked
        );
        assert_eq!(lock.bad_tries, 0);
    }
}
