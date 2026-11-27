//! Local Passcode setup, verification, strength meter, and Auto-Lock timeout settings.
//!
//! Port origin: `Telegram/SourceFiles/settings/sections/settings_local_passcode.cpp`,
//! `Telegram/SourceFiles/boxes/auto_lock_box.cpp`, and
//! `Telegram/SourceFiles/ui/widgets/passcode_strength_meter.cpp`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Color32, CornerRadius, Rect, Sense, Ui, Vec2};
use tg_core::{CoreSettings, GlobalConfig};
use tg_crypto::{LOCAL_ENCRYPT_SALT_SIZE, const_time_eq, create_local_key, random_bytes};
use tg_lang::tr;
use tg_platform::windows_hello::{
    WinRtWindowsHelloVerifier, WindowsHelloAvailability, WindowsHelloResult, WindowsHelloVerifier,
};
use tg_ui::fonts::font_id_from_style;
use tg_ui::stand_in::{Colors, InputFieldStyle, RoundButtonStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::buttons::RoundButton;
use tg_ui::widgets::input_field::InputField;

use tg_app_core::core_types::platform_call::{
    PLATFORM_CALL_REPAINT_INTERVAL, PlatformCall, PlatformCallState,
};

use super::backend::AUTO_LOCK_PRESETS_SECS;
use super::strings;
use super::widgets::{
    SettingsIconKind, settings_card, settings_divider, settings_radio_row, settings_row,
    settings_toggle_row, subsection_title,
};

/// Grace period after scheduled lock deadline to catch system sleep/wake (`kAutoLockTimeoutLateMs`).
pub const AUTO_LOCK_TIMEOUT_LATE_MS: u64 = 3_000;

/// Callback type verifying a candidate passcode against `tdata` (`AccountManager::check_local_passcode`).
pub type PasscodeVerifierFn = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Passcode mutation emitted by the Local Passcode Settings UI to be persisted in `tdata`
/// through `AccountManager`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalPasscodeCommand {
    /// Create a new passcode (`current: None`) or change an existing one (`current: Some(...)`).
    Set {
        /// Verified current passcode when changing an existing passcode.
        current: Option<String>,
        /// New passcode to wrap the local storage key with.
        new_passcode: String,
    },
    /// Remove the configured passcode after verifying `current`.
    Remove {
        /// Verified current passcode authorizing removal.
        current: String,
    },
}

/// Idle auto-lock controller porting `Core::Application::checkAutoLock`
/// (`Telegram/SourceFiles/core/application.cpp:1515-1555`).
#[derive(Debug, Default, Clone)]
pub struct AutoLockController {
    should_lock_at: Option<Instant>,
    last_timeout_secs: i32,
}

impl AutoLockController {
    /// Creates a fresh auto-lock controller.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            should_lock_at: None,
            last_timeout_secs: 0,
        }
    }

    /// Resets any pending lock deadline (called on unlock, passcode change, or timeout edit).
    pub fn reset(&mut self) {
        self.should_lock_at = None;
    }

    /// Evaluates whether the application should auto-lock now using the OS idle time
    /// from [`tg_platform::system_info::idle_time`].
    pub fn check_system_idle(
        &mut self,
        has_passcode: bool,
        is_locked: bool,
        auto_lock_secs: i32,
        now: Instant,
    ) -> bool {
        let idle = tg_platform::system_info::idle_time();
        self.check_with_idle(has_passcode, is_locked, auto_lock_secs, idle, now)
    }

    /// Evaluates whether the application should auto-lock now given an explicit `idle` duration
    /// (deterministic entry point for unit tests and live runtime).
    pub fn check_with_idle(
        &mut self,
        has_passcode: bool,
        is_locked: bool,
        auto_lock_secs: i32,
        idle: Option<Duration>,
        now: Instant,
    ) -> bool {
        if !has_passcode || is_locked || auto_lock_secs <= 0 {
            self.should_lock_at = None;
            self.last_timeout_secs = auto_lock_secs;
            return false;
        }
        if self.last_timeout_secs != auto_lock_secs {
            self.last_timeout_secs = auto_lock_secs;
            self.should_lock_at = None;
        }

        let limit = Duration::from_secs(auto_lock_secs as u64);
        let late_grace = Duration::from_millis(AUTO_LOCK_TIMEOUT_LATE_MS);

        if let Some(deadline) = self.should_lock_at
            && now > deadline + late_grace
        {
            self.should_lock_at = None;
            return true;
        }

        let Some(idle_dur) = idle else {
            return false;
        };
        if idle_dur >= limit {
            self.should_lock_at = None;
            return true;
        }

        let remaining = limit.saturating_sub(idle_dur);
        self.should_lock_at = Some(now + remaining);
        false
    }
}

/// Salted local passcode digest derived via `tg-crypto::create_local_key` (PBKDF2-HMAC-SHA512).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPasscodeHash {
    /// 32-byte random salt (`LOCAL_ENCRYPT_SALT_SIZE`).
    pub salt: [u8; LOCAL_ENCRYPT_SALT_SIZE],
    /// Derived 256-byte local storage key bytes (first 32 bytes compared in constant time).
    pub digest: [u8; 32],
}

impl LocalPasscodeHash {
    /// Derives a salted passcode hash using `tg_crypto::create_local_key`.
    #[must_use]
    pub fn derive(passcode: &str) -> Self {
        let mut salt = [0u8; LOCAL_ENCRYPT_SALT_SIZE];
        random_bytes(&mut salt);
        Self::derive_with_salt(passcode, salt)
    }

    /// Deterministically derives a hash from `passcode` and an explicit 32-byte `salt`.
    #[must_use]
    pub fn derive_with_salt(passcode: &str, salt: [u8; LOCAL_ENCRYPT_SALT_SIZE]) -> Self {
        let key = create_local_key(passcode.as_bytes(), &salt);
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&key.data()[..32]);
        Self { salt, digest }
    }

    /// Verifies `candidate` in constant time using [`tg_crypto::const_time_eq`].
    #[must_use]
    pub fn verify(&self, candidate: &str) -> bool {
        let derived = Self::derive_with_salt(candidate, self.salt);
        const_time_eq(&self.digest, &derived.digest)
    }
}

/// Passcode strength score matching `Ui::PasscodeStrengthMeter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasscodeStrength {
    Empty,
    Weak,
    Fair,
    Good,
    Strong,
}

impl PasscodeStrength {
    /// Evaluates passcode strength from length and character classes.
    #[must_use]
    pub fn evaluate(passcode: &str) -> Self {
        let len = passcode.chars().count();
        if len == 0 {
            return Self::Empty;
        }
        let mut classes = 0u8;
        if passcode.chars().any(|c| c.is_ascii_lowercase()) {
            classes += 1;
        }
        if passcode.chars().any(|c| c.is_ascii_uppercase()) {
            classes += 1;
        }
        if passcode.chars().any(|c| c.is_ascii_digit()) {
            classes += 1;
        }
        if passcode.chars().any(|c| !c.is_alphanumeric()) {
            classes += 1;
        }
        let score = len + usize::from(classes) * 2;
        if len < 4 || score < 7 {
            Self::Weak
        } else if score < 11 {
            Self::Fair
        } else if score < 15 {
            Self::Good
        } else {
            Self::Strong
        }
    }

    /// Label shown beside the strength meter.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Empty => "",
            Self::Weak => strings::PASSCODE_STRENGTH_WEAK,
            Self::Fair => strings::PASSCODE_STRENGTH_FAIR,
            Self::Good => strings::PASSCODE_STRENGTH_GOOD,
            Self::Strong => strings::PASSCODE_STRENGTH_STRONG,
        }
    }

    /// Fill fraction `0.0..=1.0`.
    #[must_use]
    pub const fn ratio(self) -> f32 {
        match self {
            Self::Empty => 0.0,
            Self::Weak => 0.25,
            Self::Fair => 0.50,
            Self::Good => 0.75,
            Self::Strong => 1.0,
        }
    }

    /// Meter bar color.
    #[must_use]
    pub fn color(self, colors: &dyn Colors) -> Color32 {
        match self {
            Self::Empty => Color32::TRANSPARENT,
            Self::Weak => colors.color("boxTextFgError"),
            Self::Fair => colors.color("settingsIconBg3"),
            Self::Good => colors.color("windowBgActive"),
            Self::Strong => colors.color("boxTextFgGood"),
        }
    }
}

/// Active screen mode inside Local Passcode settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LocalPasscodeStep {
    /// Overview / Start when passcode is disabled or Manage when unlocked.
    #[default]
    Overview,
    /// Creating a new passcode (first time).
    Create,
    /// Verifying existing passcode before managing/changing/disabling.
    VerifyCurrent,
    /// Changing passcode after verification.
    Change,
    /// Confirming passcode removal.
    DisableConfirm,
}

/// Persistent state for Local Passcode settings.
#[derive(Clone, Default)]
pub struct LocalPasscodeState {
    /// Stored salted hash when a local passcode is enabled (mock / in-memory fallback).
    pub stored_hash: Option<LocalPasscodeHash>,
    /// Whether `tdata` reports an active local passcode (live mode).
    pub external_enabled: bool,
    /// Optional verifier backed by `AccountManager::check_local_passcode` in live mode.
    external_verifier: Option<PasscodeVerifierFn>,
    /// Verified current passcode cached only while managing/changing/removing in Settings.
    verified_current: Option<String>,
    /// Whether the user has verified the current passcode in this settings visit.
    pub unlocked_for_manage: bool,
    /// Active workflow step.
    pub step: LocalPasscodeStep,
    /// Current / verify input buffer.
    pub current_input: String,
    /// New passcode input buffer.
    pub new_input: String,
    /// Confirm new passcode input buffer.
    pub confirm_input: String,
    /// Validation error message.
    pub error: Option<String>,
    /// Whether the Auto-Lock timeout picker is expanded.
    pub editing_auto_lock: bool,
    /// Custom auto-lock `HH:MM` text input (`AutoLockBox`).
    pub custom_auto_lock_hhmm: String,
    /// Monotonic clock milliseconds for backoff simulation/testing.
    pub last_tick_ms: i64,
    /// Injectable Windows Hello verifier (`WinRtWindowsHelloVerifier` in production, fake in tests).
    windows_hello_verifier: Option<Arc<dyn WindowsHelloVerifier>>,
    /// Cached Windows Hello availability once queried.
    pub windows_hello_availability: Option<WindowsHelloAvailability>,
    /// Native window handle used to anchor Windows Hello prompts (`0` in headless/tests).
    pub window_hwnd: isize,
    /// Windows Hello prompt in flight on a worker thread.
    hello_pending: Option<PlatformCall<Result<WindowsHelloResult, String>>>,
}

impl std::fmt::Debug for LocalPasscodeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalPasscodeState")
            .field("has_stored_hash", &self.stored_hash.is_some())
            .field("external_enabled", &self.external_enabled)
            .field("has_external_verifier", &self.external_verifier.is_some())
            .field("unlocked_for_manage", &self.unlocked_for_manage)
            .field("step", &self.step)
            .field("error", &self.error)
            .field("editing_auto_lock", &self.editing_auto_lock)
            .field("last_tick_ms", &self.last_tick_ms)
            .finish()
    }
}

impl LocalPasscodeState {
    /// Returns `true` if a local passcode is currently configured.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.external_enabled || self.stored_hash.is_some()
    }

    /// Synchronizes passcode status and verifier from live `tdata` (`AccountManager`).
    pub fn sync_from_tdata(&mut self, has_passcode: bool, verifier: Option<PasscodeVerifierFn>) {
        self.external_enabled = has_passcode;
        self.external_verifier = verifier;
        if !has_passcode {
            self.stored_hash = None;
            self.verified_current = None;
            self.unlocked_for_manage = false;
        }
    }

    /// Clears sensitive in-memory verified passcode and locks the management screen.
    pub fn clear_manage_session(&mut self) {
        self.verified_current = None;
        self.unlocked_for_manage = false;
        self.current_input.clear();
        self.new_input.clear();
        self.confirm_input.clear();
        self.step = LocalPasscodeStep::Overview;
    }

    /// Records a persistence error returned when `AccountManager` rejects a passcode update.
    pub fn apply_external_error(&mut self, err: String, still_enabled: bool) {
        self.external_enabled = still_enabled;
        if !still_enabled {
            self.stored_hash = None;
        }
        self.error = Some(err);
    }

    /// Verifies a candidate passcode with tdesktop exponential backoff (`GlobalConfig::passcode_can_try`).
    pub fn try_verify(
        &mut self,
        candidate: &str,
        global: &mut GlobalConfig,
        now_ms: i64,
    ) -> Result<(), String> {
        if !global.passcode_can_try(now_ms) {
            return Err(strings::PASSCODE_TOO_MANY_ATTEMPTS.to_owned());
        }
        let ok = if let Some(verifier) = &self.external_verifier {
            verifier(candidate)
        } else if let Some(hash) = &self.stored_hash {
            hash.verify(candidate)
        } else {
            return Ok(());
        };
        if ok {
            global.passcode_bad_tries = 0;
            self.unlocked_for_manage = true;
            self.verified_current = Some(candidate.to_owned());
            Ok(())
        } else {
            global.passcode_bad_tries += 1;
            global.passcode_last_try_ms = now_ms;
            Err(tr::lng_passcode_wrong())
        }
    }

    /// Sets or changes the local passcode after validation.
    pub fn apply_new_passcode(
        &mut self,
        new_pass: &str,
        confirm: &str,
        global: &mut GlobalConfig,
    ) -> Result<(), String> {
        if new_pass.is_empty() {
            return Err(strings::PASSCODE_EMPTY_ERROR.to_owned());
        }
        if new_pass != confirm {
            return Err(tr::lng_passcode_differ());
        }
        self.stored_hash = Some(LocalPasscodeHash::derive(new_pass));
        self.unlocked_for_manage = true;
        global.passcode_bad_tries = 0;
        Ok(())
    }

    /// Removes the local passcode and resets system unlock (`RemovePasscode` in `settings_local_passcode.cpp`).
    pub fn disable_passcode(&mut self, core: &mut CoreSettings, global: &mut GlobalConfig) {
        self.stored_hash = None;
        self.external_enabled = false;
        self.verified_current = None;
        self.unlocked_for_manage = false;
        core.system_unlock_enabled = false;
        global.passcode_bad_tries = 0;
    }

    /// Injects a [`WindowsHelloVerifier`] (production WinRT or fake in tests).
    pub fn set_windows_hello_verifier(&mut self, verifier: Arc<dyn WindowsHelloVerifier>) {
        self.windows_hello_verifier = Some(verifier);
        self.windows_hello_availability = None;
    }

    /// Updates the native Win32 HWND used to parent Windows Hello consent dialogs.
    pub fn set_window_hwnd(&mut self, hwnd: isize) {
        self.window_hwnd = hwnd;
    }

    fn active_windows_hello_verifier(&self) -> Arc<dyn WindowsHelloVerifier> {
        self.windows_hello_verifier
            .clone()
            .unwrap_or_else(|| Arc::new(WinRtWindowsHelloVerifier::new()))
    }

    /// Queries and caches Windows Hello availability through the configured verifier.
    pub fn refresh_windows_hello_availability(&mut self) -> WindowsHelloAvailability {
        let verifier = self.active_windows_hello_verifier();
        let avail = verifier
            .check_availability()
            .unwrap_or(WindowsHelloAvailability::Unavailable);
        self.windows_hello_availability = Some(avail);
        avail
    }

    /// Opens a Windows Hello prompt (`UserConsentVerifier`) on a worker thread to unlock the
    /// Local Passcode management screen; [`Self::poll_windows_hello_unlock`] collects the verdict.
    ///
    /// Returns `Ok(false)` when no passcode is set or a prompt is already open.
    pub fn start_windows_hello_unlock(
        &mut self,
        core: &CoreSettings,
        repaint: Option<egui::Context>,
    ) -> Result<bool, String> {
        if !self.is_enabled() || self.hello_pending.is_some() {
            return Ok(false);
        }
        if !core.system_unlock_enabled {
            return Err("Windows Hello unlock is disabled in Local Passcode settings.".to_owned());
        }
        let verifier = self.active_windows_hello_verifier();
        let hwnd = self.window_hwnd;
        let prompt = tr::lng_passcode_winhello_unlock();
        self.error = None;
        self.hello_pending = Some(PlatformCall::spawn(
            "winhello-settings",
            repaint,
            move || {
                verifier
                    .request_verification(hwnd, &prompt)
                    .map_err(|e| e.to_string())
            },
        ));
        Ok(true)
    }

    /// Returns `true` while a Windows Hello prompt is open.
    #[must_use]
    pub const fn is_windows_hello_pending(&self) -> bool {
        self.hello_pending.is_some()
    }

    /// Collects the verdict of the prompt opened by [`Self::start_windows_hello_unlock`]:
    /// `None` while it is open, `Some(Ok(()))` once the management screen is unlocked.
    pub fn poll_windows_hello_unlock(
        &mut self,
        global: &mut GlobalConfig,
    ) -> Option<Result<(), String>> {
        let outcome = match self.hello_pending.as_ref()?.poll() {
            PlatformCallState::Pending => return None,
            PlatformCallState::Done(outcome) => outcome,
            PlatformCallState::Lost => Err("the prompt closed unexpectedly".to_owned()),
        };
        self.hello_pending = None;
        Some(self.apply_windows_hello_outcome(outcome, global))
    }

    /// Blocks up to `timeout` for the open Windows Hello prompt (tests only, never a UI frame).
    #[doc(hidden)]
    pub fn wait_windows_hello_unlock(
        &mut self,
        timeout: Duration,
        global: &mut GlobalConfig,
    ) -> Option<Result<(), String>> {
        let outcome = match self.hello_pending.as_ref()?.wait(timeout) {
            PlatformCallState::Pending => return None,
            PlatformCallState::Done(outcome) => outcome,
            PlatformCallState::Lost => Err("the prompt closed unexpectedly".to_owned()),
        };
        self.hello_pending = None;
        Some(self.apply_windows_hello_outcome(outcome, global))
    }

    fn apply_windows_hello_outcome(
        &mut self,
        outcome: Result<WindowsHelloResult, String>,
        global: &mut GlobalConfig,
    ) -> Result<(), String> {
        match outcome {
            Ok(WindowsHelloResult::Verified) => {
                global.passcode_bad_tries = 0;
                self.unlocked_for_manage = true;
                self.error = None;
                Ok(())
            }
            Ok(WindowsHelloResult::RetriesExhausted) => {
                let err = strings::PASSCODE_TOO_MANY_ATTEMPTS.to_owned();
                self.error = Some(err.clone());
                Err(err)
            }
            Ok(WindowsHelloResult::Canceled) => {
                let err = "Windows Hello verification was cancelled.".to_owned();
                self.error = Some(err.clone());
                Err(err)
            }
            Ok(other) => {
                let err = format!("Windows Hello unavailable ({other:?}).");
                self.error = Some(err.clone());
                Err(err)
            }
            Err(err) => {
                let msg = format!("Windows Hello error: {err}");
                self.error = Some(msg.clone());
                Err(msg)
            }
        }
    }
}

/// Parses a custom auto-lock `"HH:MM"` string into seconds (`AutoLockBox::collect`).
#[must_use]
pub fn parse_auto_lock_hhmm(input: &str) -> Option<i32> {
    let (h_str, m_str) = input.trim().split_once(':')?;
    let hours = h_str.trim().parse::<i32>().ok()?;
    let mins = m_str.trim().parse::<i32>().ok()?;
    if hours < 0 || !(0..60).contains(&mins) {
        return None;
    }
    let total = hours.saturating_mul(3600).saturating_add(mins * 60);
    (total > 0).then_some(total)
}

/// Formats an auto-lock timeout in seconds into a localized string (e.g. `"1 hour"`, `"5 minutes"`, `"0:10"`).
#[must_use]
pub fn format_auto_lock(seconds: i32) -> String {
    match seconds {
        60 => tr::lng_minutes(1),
        300 => tr::lng_minutes(5),
        3600 => tr::lng_hours(1),
        18000 => tr::lng_hours(5),
        s if s <= 0 => tr::lng_settings_cloud_password_off(),
        s => {
            let h = s / 3600;
            let m = (s % 3600) / 60;
            if h > 0 && m == 0 {
                tr::lng_hours(i64::from(h))
            } else if h == 0 {
                tr::lng_minutes(i64::from(m))
            } else {
                format!("{h}:{m:02}")
            }
        }
    }
}

/// Output emitted by [`render_local_passcode_section`].
#[derive(Debug, Clone, Default)]
pub struct LocalPasscodeOutput {
    /// Emit `SettingsAction::LockNow` to lock the app immediately.
    pub lock_now: bool,
    /// Emit `SettingsAction::OpenPasscodeSetup` to sync shell-level passcode if desired.
    pub passcode_changed: bool,
    /// Persistence command to re-encrypt `tdata/key_datas` via `AccountManager`.
    pub command: Option<LocalPasscodeCommand>,
    /// Toast feedback message.
    pub toast: Option<String>,
}

/// Renders the Local Passcode section (`settings_local_passcode.cpp`).
pub fn render_local_passcode_section(
    ui: &mut Ui,
    state: &mut LocalPasscodeState,
    core: &mut CoreSettings,
    global: &mut GlobalConfig,
    colors: &dyn Colors,
) -> LocalPasscodeOutput {
    let mut out = LocalPasscodeOutput::default();
    let now_ms = (ui.ctx().input(|i| i.time) * 1000.0) as i64;
    state.last_tick_ms = now_ms;

    if !state.is_enabled() {
        match state.step {
            LocalPasscodeStep::Create => {
                subsection_title(ui, &tr::lng_passcode_create_title(), colors);
                settings_card(ui, colors, |ui| {
                    ui.label(
                        egui::RichText::new(tr::lng_passcode_about())
                            .font(font_id_from_style(st::normal_font))
                            .color(colors.color("windowSubTextFg")),
                    );
                    ui.add_space(8.0);
                    InputField::single_line(
                        "passcode_new",
                        &mut state.new_input,
                        &tr::lng_passcode_enter_first(),
                        InputFieldStyle::default_input(colors),
                    )
                    .password(true)
                    .show(ui);
                    render_strength_meter(ui, &state.new_input, colors);
                    ui.add_space(6.0);
                    InputField::single_line(
                        "passcode_confirm",
                        &mut state.confirm_input,
                        &tr::lng_passcode_enter_new(),
                        InputFieldStyle::default_input(colors),
                    )
                    .password(true)
                    .show(ui);
                    if let Some(err) = &state.error {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(err)
                                .font(font_id_from_style(st::normal_font))
                                .color(colors.color("boxTextFgError")),
                        );
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if RoundButton::new(
                            &tr::lng_passcode_create_button(),
                            RoundButtonStyle::default_active(colors),
                        )
                        .show(ui)
                        .clicked()
                        {
                            let new_p = state.new_input.clone();
                            let conf = state.confirm_input.clone();
                            match state.apply_new_passcode(&new_p, &conf, global) {
                                Ok(()) => {
                                    state.verified_current = Some(new_p.clone());
                                    state.new_input.clear();
                                    state.confirm_input.clear();
                                    state.error = None;
                                    state.step = LocalPasscodeStep::Overview;
                                    out.passcode_changed = true;
                                    out.command = Some(LocalPasscodeCommand::Set {
                                        current: None,
                                        new_passcode: new_p,
                                    });
                                    out.toast = Some(strings::PASSCODE_ENABLED_TOAST.to_owned());
                                }
                                Err(e) => state.error = Some(e),
                            }
                        }
                        ui.add_space(8.0);
                        if RoundButton::new(
                            &tr::lng_cancel(),
                            RoundButtonStyle::default_light(colors),
                        )
                        .show(ui)
                        .clicked()
                        {
                            state.new_input.clear();
                            state.confirm_input.clear();
                            state.error = None;
                            state.step = LocalPasscodeStep::Overview;
                        }
                    });
                });
            }
            _ => {
                subsection_title(ui, &tr::lng_settings_passcode_title(), colors);
                settings_card(ui, colors, |ui| {
                    ui.label(
                        egui::RichText::new(strings::PASSCODE_PROTECT_HEADING)
                            .font(font_id_from_style(st::semibold_font))
                            .color(colors.color("windowBoldFg")),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(strings::PASSCODE_PROTECT_DESC)
                            .font(font_id_from_style(st::normal_font))
                            .color(colors.color("windowSubTextFg")),
                    );
                    ui.add_space(12.0);
                    if RoundButton::new(
                        strings::PASSCODE_TURN_ON_BUTTON,
                        RoundButtonStyle::default_active(colors),
                    )
                    .show(ui)
                    .clicked()
                    {
                        state.step = LocalPasscodeStep::Create;
                        state.error = None;
                    }
                });
            }
        }
        ui.add_space(16.0);
        return out;
    }

    // Passcode IS enabled: require verification if not yet unlocked in this visit.
    if !state.unlocked_for_manage {
        match state.poll_windows_hello_unlock(global) {
            Some(Ok(())) => {
                state.current_input.clear();
                state.step = LocalPasscodeStep::Overview;
            }
            Some(Err(_)) => {}
            None if state.is_windows_hello_pending() => {
                ui.ctx()
                    .request_repaint_after(PLATFORM_CALL_REPAINT_INTERVAL);
            }
            None => {}
        }
    }
    if !state.unlocked_for_manage {
        subsection_title(ui, &tr::lng_passcode_enter(), colors);
        settings_card(ui, colors, |ui| {
            InputField::single_line(
                "passcode_verify",
                &mut state.current_input,
                &tr::lng_passcode_enter_old(),
                InputFieldStyle::default_input(colors),
            )
            .password(true)
            .error(state.error.is_some())
            .show(ui);
            if let Some(err) = &state.error {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(err)
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("boxTextFgError")),
                );
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if RoundButton::new(
                    &tr::lng_passcode_submit(),
                    RoundButtonStyle::default_active(colors),
                )
                .show(ui)
                .clicked()
                {
                    let candidate = state.current_input.clone();
                    match state.try_verify(&candidate, global, now_ms) {
                        Ok(()) => {
                            state.current_input.clear();
                            state.error = None;
                            state.step = LocalPasscodeStep::Overview;
                        }
                        Err(e) => state.error = Some(e),
                    }
                }
                if core.system_unlock_enabled {
                    ui.add_space(8.0);
                    let waiting = state.is_windows_hello_pending();
                    let label = if waiting {
                        "Waiting for Windows Hello\u{2026}"
                    } else {
                        strings::PASSCODE_WINHELLO_TITLE
                    };
                    if RoundButton::new(label, RoundButtonStyle::default_light(colors))
                        .show(ui)
                        .clicked()
                        && !waiting
                        && let Err(e) =
                            state.start_windows_hello_unlock(core, Some(ui.ctx().clone()))
                    {
                        state.error = Some(e);
                    }
                }
            });
        });
        ui.add_space(16.0);
        return out;
    }

    // Enabled & unlocked: Manage local passcode.
    subsection_title(ui, &tr::lng_settings_passcode_title(), colors);

    let change_lbl = tr::lng_passcode_change();
    if settings_row(
        ui,
        &change_lbl,
        None,
        None,
        Some(SettingsIconKind::Lock),
        false,
        false,
        colors,
    )
    .clicked()
    {
        state.step = LocalPasscodeStep::Change;
        state.error = None;
    }

    if settings_row(
        ui,
        strings::PASSCODE_LOCK_NOW,
        Some(strings::PASSCODE_LOCK_SHORTCUT),
        None,
        Some(SettingsIconKind::Key),
        false,
        false,
        colors,
    )
    .clicked()
    {
        out.lock_now = true;
    }

    let remove_lbl = tr::lng_passcode_remove();
    if settings_row(
        ui,
        &remove_lbl,
        None,
        None,
        Some(SettingsIconKind::Blocked),
        true,
        false,
        colors,
    )
    .clicked()
    {
        state.step = LocalPasscodeStep::DisableConfirm;
    }

    if state.step == LocalPasscodeStep::Change {
        ui.add_space(6.0);
        settings_card(ui, colors, |ui| {
            ui.label(
                egui::RichText::new(tr::lng_passcode_change())
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.add_space(6.0);
            InputField::single_line(
                "passcode_change_new",
                &mut state.new_input,
                &tr::lng_passcode_enter_new(),
                InputFieldStyle::default_input(colors),
            )
            .password(true)
            .show(ui);
            render_strength_meter(ui, &state.new_input, colors);
            ui.add_space(6.0);
            InputField::single_line(
                "passcode_change_confirm",
                &mut state.confirm_input,
                &tr::lng_passcode_enter_new(),
                InputFieldStyle::default_input(colors),
            )
            .password(true)
            .show(ui);
            if let Some(err) = &state.error {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(err)
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("boxTextFgError")),
                );
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if RoundButton::new(
                    &tr::lng_settings_save(),
                    RoundButtonStyle::default_active(colors),
                )
                .show(ui)
                .clicked()
                {
                    let np = state.new_input.clone();
                    let cp = state.confirm_input.clone();
                    let prev_verified = state.verified_current.clone();
                    match state.apply_new_passcode(&np, &cp, global) {
                        Ok(()) => {
                            state.verified_current = Some(np.clone());
                            state.new_input.clear();
                            state.confirm_input.clear();
                            state.error = None;
                            state.step = LocalPasscodeStep::Overview;
                            out.passcode_changed = true;
                            out.command = Some(LocalPasscodeCommand::Set {
                                current: prev_verified,
                                new_passcode: np,
                            });
                            out.toast = Some(strings::PASSCODE_CHANGED_TOAST.to_owned());
                        }
                        Err(e) => state.error = Some(e),
                    }
                }
                ui.add_space(8.0);
                if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
                    .show(ui)
                    .clicked()
                {
                    state.new_input.clear();
                    state.confirm_input.clear();
                    state.error = None;
                    state.step = LocalPasscodeStep::Overview;
                }
            });
        });
    } else if state.step == LocalPasscodeStep::DisableConfirm {
        ui.add_space(6.0);
        settings_card(ui, colors, |ui| {
            ui.label(
                egui::RichText::new(strings::PASSCODE_DISABLE_CONFIRM)
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if RoundButton::new(
                    strings::PASSCODE_TURN_OFF_BTN,
                    RoundButtonStyle::attention_box(colors),
                )
                .show(ui)
                .clicked()
                {
                    let current = state.verified_current.clone().unwrap_or_default();
                    state.disable_passcode(core, global);
                    state.step = LocalPasscodeStep::Overview;
                    out.passcode_changed = true;
                    out.command = Some(LocalPasscodeCommand::Remove { current });
                    out.toast = Some(strings::PASSCODE_DISABLED_TOAST.to_owned());
                }
                ui.add_space(8.0);
                if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
                    .show(ui)
                    .clicked()
                {
                    state.step = LocalPasscodeStep::Overview;
                }
            });
        });
    }

    settings_divider(ui, None, colors);

    // Auto-lock & System Unlock (`CoreSettings::auto_lock`, `CoreSettings::system_unlock_enabled`).
    subsection_title(ui, strings::PASSCODE_AUTO_LOCK_TITLE, colors);
    let auto_lock_str = format_auto_lock(core.auto_lock);
    if settings_row(
        ui,
        strings::PASSCODE_AUTO_LOCK_IF_AWAY,
        None,
        Some(&auto_lock_str),
        Some(SettingsIconKind::Timer),
        false,
        false,
        colors,
    )
    .clicked()
    {
        state.editing_auto_lock = !state.editing_auto_lock;
        if state.custom_auto_lock_hhmm.is_empty() {
            state.custom_auto_lock_hhmm = "0:10".to_owned();
        }
    }

    if state.editing_auto_lock {
        ui.add_space(4.0);
        settings_card(ui, colors, |ui| {
            for &secs in &AUTO_LOCK_PRESETS_SECS {
                if settings_radio_row(
                    ui,
                    &mut core.auto_lock,
                    secs,
                    &format_auto_lock(secs),
                    true,
                    colors,
                )
                .changed()
                {
                    out.toast = Some(format!(
                        "{}: {}",
                        strings::PASSCODE_AUTO_LOCK_TITLE,
                        format_auto_lock(secs)
                    ));
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                InputField::single_line(
                    "autolock_custom",
                    &mut state.custom_auto_lock_hhmm,
                    strings::PASSCODE_CUSTOM_HHMM_PH,
                    InputFieldStyle::default_input(colors),
                )
                .width(120.0)
                .show(ui);
                ui.add_space(8.0);
                if RoundButton::new(
                    strings::PASSCODE_SET_CUSTOM_BTN,
                    RoundButtonStyle::default_light(colors),
                )
                .show(ui)
                .clicked()
                {
                    if let Some(secs) = parse_auto_lock_hhmm(&state.custom_auto_lock_hhmm) {
                        core.auto_lock = secs;
                        state.editing_auto_lock = false;
                        out.toast = Some(format!(
                            "{}: {}",
                            strings::PASSCODE_AUTO_LOCK_TITLE,
                            format_auto_lock(secs)
                        ));
                    } else {
                        out.toast = Some(strings::PASSCODE_CUSTOM_HINT.to_owned());
                    }
                }
            });
        });
    }

    let prev_hello = core.system_unlock_enabled;
    let hello_subtitle = match state.windows_hello_availability {
        Some(avail) if !avail.is_available() => {
            "Windows Hello is not currently configured on this device."
        }
        _ => strings::PASSCODE_WINHELLO_SUBTITLE,
    };
    if settings_toggle_row(
        ui,
        strings::PASSCODE_WINHELLO_TITLE,
        Some(hello_subtitle),
        None,
        &mut core.system_unlock_enabled,
        true,
        colors,
    ) && core.system_unlock_enabled
        && !prev_hello
    {
        let avail = state.refresh_windows_hello_availability();
        if !avail.is_available() {
            core.system_unlock_enabled = false;
            out.toast = Some("Windows Hello is not available on this device.".to_owned());
        } else {
            out.toast = Some("Windows Hello unlock enabled.".to_owned());
        }
    }

    ui.add_space(16.0);
    out
}

fn render_strength_meter(ui: &mut Ui, passcode: &str, colors: &dyn Colors) {
    let strength = PasscodeStrength::evaluate(passcode);
    if strength == PasscodeStrength::Empty {
        return;
    }
    let meter_color = strength.color(colors);
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(140.0, 6.0), Sense::hover());
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(3),
            colors.color("sliderBgInactive"),
        );
        let fill_rect = Rect::from_min_size(
            rect.min,
            Vec2::new(rect.width() * strength.ratio(), rect.height()),
        );
        ui.painter()
            .rect_filled(fill_rect, CornerRadius::same(3), meter_color);
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(strength.label())
                .font(egui::FontId::new(11.5, tg_ui::fonts::family_semibold()))
                .color(meter_color),
        );
    });
}
