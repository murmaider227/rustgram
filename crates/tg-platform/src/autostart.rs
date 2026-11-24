//! Windows StartupTask (WinRT) and `HKCU\...\Run` autostart registration.
//!
//! Port of:
//! - `Telegram/SourceFiles/platform/win/windows_autostart_task.h`
//! - `Telegram/SourceFiles/platform/win/windows_autostart_task.cpp`
//! - `Telegram/SourceFiles/platform/win/specific_win.cpp` (`AutostartToggle`, `AutostartRequestStateFromSystem`)
//!
//! Provides:
//! - [`StartupTaskState`] mirroring `Windows.ApplicationModel.StartupTaskState`;
//! - [`AutostartEntry`] and safe Windows `CommandLineToArgvW` executable/argument quoting helpers;
//! - [`AutostartProvider`] trait abstracting both packaged WinRT `StartupTask` and desktop
//!   `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` registration;
//! - [`WinRtStartupTaskProvider`] and [`WindowsRunKeyAutostartProvider`] under `#[cfg(windows)]`;
//! - [`FakeAutostartProvider`] for deterministic tests without touching real startup tasks or
//!   Windows registry keys.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::{PlatformError, Result};
use crate::registration::RegistryRoot;

/// Default MSIX/WinRT `StartupTask` ID for Rustgram.
pub const DEFAULT_STARTUP_TASK_ID: &str = "RustgramStartupTask";

/// Upstream Telegram Desktop MSIX `StartupTask` ID (`windows_autostart_task.cpp:74`).
pub const TDESKTOP_STARTUP_TASK_ID: &str = "TelegramStartupTask";

/// Per-user Windows Registry `Run` key subkey path under `HKEY_CURRENT_USER`.
pub const WINDOWS_RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Windows Settings deep-link URI opened when the user disabled startup externally
/// (`windows_autostart_task.cpp:108`).
pub const STARTUP_APPS_SETTINGS_URI: &str = "ms-settings:startupapps";

/// Default command-line flag passed to launch minimized/in background on Windows sign-in.
pub const DEFAULT_AUTOSTART_ARG: &str = "-autostart";

/// Startup task enablement state mirroring `Windows.ApplicationModel.StartupTaskState`
/// (`windows_autostart_task.cpp:23-34`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StartupTaskState {
    /// Autostart is disabled and can be enabled programmatically.
    #[default]
    Disabled,
    /// Autostart was explicitly disabled by the user in Task Manager / Windows Settings.
    ///
    /// Applications cannot override this state; they must guide the user to
    /// [`STARTUP_APPS_SETTINGS_URI`].
    DisabledByUser,
    /// Autostart is currently enabled.
    Enabled,
    /// Autostart is blocked by enterprise / Group Policy.
    DisabledByPolicy,
    /// Autostart is forced on by enterprise / Group Policy.
    EnabledByPolicy,
}

impl StartupTaskState {
    /// Returns `true` when autostart is effectively enabled (`IsEnabled` in
    /// `windows_autostart_task.cpp:23-34`).
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled | Self::EnabledByPolicy)
    }

    /// Returns `true` when enabling requires user action in Windows Settings or is blocked by
    /// Group Policy.
    #[must_use]
    pub const fn is_blocked_by_user_or_policy(self) -> bool {
        matches!(self, Self::DisabledByUser | Self::DisabledByPolicy)
    }

    /// Returns `true` if Group Policy does not lock the current state.
    #[must_use]
    pub const fn can_toggle(self) -> bool {
        !matches!(self, Self::DisabledByPolicy | Self::EnabledByPolicy)
    }
}

/// Configuration describing an application's autostart registration entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutostartEntry {
    /// Registry value name under `HKCU\...\Run` (e.g. `"Rustgram"`).
    pub app_name: String,
    /// Packaged WinRT `StartupTask` identifier (e.g. [`DEFAULT_STARTUP_TASK_ID`]).
    pub task_id: String,
    /// Full path to the application executable.
    pub executable: PathBuf,
    /// Command-line arguments passed on login (default `["-autostart"]`).
    pub arguments: Vec<String>,
}

impl AutostartEntry {
    /// Creates a standard Rustgram autostart entry launching `executable` with `-autostart`.
    #[must_use]
    pub fn for_executable(executable: PathBuf) -> Self {
        Self {
            app_name: "Rustgram".to_owned(),
            task_id: DEFAULT_STARTUP_TASK_ID.to_owned(),
            executable,
            arguments: vec![DEFAULT_AUTOSTART_ARG.to_owned()],
        }
    }

    /// Appends `-workdir <dir>` to the autostart arguments.
    #[must_use]
    pub fn with_workdir(mut self, workdir: &Path) -> Self {
        let normalized = normalize_windows_path(workdir);
        self.arguments.push("-workdir".to_owned());
        self.arguments.push(normalized);
        self
    }

    /// Formats the fully quoted Windows command line stored in the `Run` registry value.
    pub fn command_line(&self) -> Result<String> {
        format_autostart_command(&self.executable, &self.arguments)
    }
}

fn normalize_windows_path(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\")
}

/// Validates and quotes an executable path for Windows `Run` registry registration.
///
/// Rejects empty paths as well as NUL / CR / LF characters to prevent command-line corruption.
/// Always wraps the executable path in double quotes so paths containing spaces (such as
/// `C:\Program Files\Rustgram\rustgram.exe`) are unambiguous to `CreateProcess`.
pub fn quote_executable_path(path: &Path) -> Result<String> {
    let raw = normalize_windows_path(path);
    if raw.trim().is_empty() {
        return Err(PlatformError::InvalidArgument(
            "autostart executable path cannot be empty".to_owned(),
        ));
    }
    if raw.chars().any(|c| matches!(c, '\0' | '\r' | '\n')) {
        return Err(PlatformError::InvalidArgument(
            "autostart executable path contains forbidden control characters".to_owned(),
        ));
    }
    // Always wrap the executable path in quotes, applying Windows backslash-before-quote escaping.
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    let mut slashes = 0usize;
    for ch in raw.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        let repeat = if ch == '"' { slashes * 2 + 1 } else { slashes };
        for _ in 0..repeat {
            out.push('\\');
        }
        slashes = 0;
        out.push(ch);
    }
    for _ in 0..(slashes * 2) {
        out.push('\\');
    }
    out.push('"');
    Ok(out)
}

/// Validates and quotes a single command-line argument according to Windows `CommandLineToArgvW`
/// rules.
///
/// Rejects embedded NUL (`\0`) characters. Leaves simple flag tokens like `-autostart` unquoted,
/// and safely quotes arguments containing spaces, tabs, newlines, or quotes (including trailing
/// backslashes such as `C:\Users\My Data\`).
pub fn quote_command_argument(arg: &str) -> Result<String> {
    if arg.contains('\0') {
        return Err(PlatformError::InvalidArgument(
            "autostart argument contains NUL character".to_owned(),
        ));
    }
    if !arg.is_empty()
        && !arg
            .chars()
            .any(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0B' | '"'))
    {
        return Ok(arg.to_owned());
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut slashes = 0usize;
    for ch in arg.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        let repeat = if ch == '"' { slashes * 2 + 1 } else { slashes };
        for _ in 0..repeat {
            out.push('\\');
        }
        slashes = 0;
        out.push(ch);
    }
    for _ in 0..(slashes * 2) {
        out.push('\\');
    }
    out.push('"');
    Ok(out)
}

/// Formats a complete Windows `Run` command line from `executable` and `arguments`.
pub fn format_autostart_command(executable: &Path, arguments: &[String]) -> Result<String> {
    let mut cmd = quote_executable_path(executable)?;
    for arg in arguments {
        cmd.push(' ');
        cmd.push_str(&quote_command_argument(arg)?);
    }
    Ok(cmd)
}

/// Parses a Windows command line back into argv tokens following `CommandLineToArgvW` rules.
#[must_use]
pub fn parse_command_line_args(cmd: &str) -> Vec<String> {
    let mut args = Vec::new();
    let chars: Vec<char> = cmd.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        while i < chars.len() && matches!(chars[i], ' ' | '\t') {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let mut current = String::new();
        let mut in_quotes = false;
        while i < chars.len() {
            if !in_quotes && matches!(chars[i], ' ' | '\t') {
                break;
            }
            if chars[i] == '\\' {
                let mut slashes = 0usize;
                while i < chars.len() && chars[i] == '\\' {
                    slashes += 1;
                    i += 1;
                }
                if i < chars.len() && chars[i] == '"' {
                    for _ in 0..(slashes / 2) {
                        current.push('\\');
                    }
                    if slashes % 2 == 1 {
                        current.push('"');
                    } else {
                        in_quotes = !in_quotes;
                    }
                    i += 1;
                } else {
                    for _ in 0..slashes {
                        current.push('\\');
                    }
                }
            } else if chars[i] == '"' {
                if in_quotes && i + 1 < chars.len() && chars[i + 1] == '"' {
                    current.push('"');
                    i += 2;
                } else {
                    in_quotes = !in_quotes;
                    i += 1;
                }
            } else {
                current.push(chars[i]);
                i += 1;
            }
        }
        args.push(current);
    }
    args
}

/// Abstraction over Windows autostart registration (WinRT `StartupTask` or `HKCU\...\Run`).
pub trait AutostartProvider: Send + Sync {
    /// Queries the current startup registration state.
    fn query_state(&self) -> Result<StartupTaskState>;

    /// Convenience helper returning `true` when [`Self::query_state`] reports an enabled state.
    fn is_enabled(&self) -> Result<bool> {
        Ok(self.query_state()?.is_enabled())
    }

    /// Attempts to enable or disable autostart, returning the resulting [`StartupTaskState`].
    fn set_enabled(&self, enabled: bool) -> Result<StartupTaskState>;

    /// Opens the OS Startup Apps settings page (`ms-settings:startupapps`).
    fn open_startup_settings(&self) -> Result<()>;

    /// Returns the registered command line if backed by a command-line entry (e.g. `Run` key).
    fn registered_command(&self) -> Result<Option<String>> {
        Ok(None)
    }
}

// ============================================================================
// Deterministic FakeAutostartProvider
// ============================================================================

struct FakeAutostartState {
    entry: AutostartEntry,
    state: StartupTaskState,
    registered_command: Option<String>,
    error_message: Option<String>,
    toggle_history: Vec<(bool, StartupTaskState)>,
}

/// Deterministic in-memory [`AutostartProvider`] for tests and headless runs.
///
/// Accurately models Windows `StartupTask` state transitions (including `DisabledByUser`,
/// `DisabledByPolicy`, and `EnabledByPolicy` immutability) and formats the quoted command line
/// on enable without touching the Windows Registry or StartupTask runtime.
#[derive(Clone)]
pub struct FakeAutostartProvider {
    inner: Arc<Mutex<FakeAutostartState>>,
    query_calls: Arc<AtomicUsize>,
    settings_opened: Arc<AtomicUsize>,
}

impl Default for FakeAutostartProvider {
    fn default() -> Self {
        Self::new(AutostartEntry::for_executable(PathBuf::from(
            r"C:\Program Files\Rustgram\rustgram.exe",
        )))
    }
}

impl FakeAutostartProvider {
    /// Creates a fake provider starting in [`StartupTaskState::Disabled`].
    #[must_use]
    pub fn new(entry: AutostartEntry) -> Self {
        Self::with_state(entry, StartupTaskState::Disabled)
    }

    /// Creates a fake provider initialized to an explicit [`StartupTaskState`].
    #[must_use]
    pub fn with_state(entry: AutostartEntry, state: StartupTaskState) -> Self {
        let registered_command = if state.is_enabled() {
            entry.command_line().ok()
        } else {
            None
        };
        Self {
            inner: Arc::new(Mutex::new(FakeAutostartState {
                entry,
                state,
                registered_command,
                error_message: None,
                toggle_history: Vec::new(),
            })),
            query_calls: Arc::new(AtomicUsize::new(0)),
            settings_opened: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Forces the simulated state (e.g. to simulate a user disabling the app in Task Manager).
    pub fn set_state(&self, state: StartupTaskState) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.state = state;
            if !state.is_enabled() {
                guard.registered_command = None;
            }
        }
    }

    /// Replaces the [`AutostartEntry`] used to generate the quoted command line.
    pub fn set_entry(&self, entry: AutostartEntry) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.entry = entry;
        }
    }

    /// Injects an error to be returned by subsequent provider calls.
    pub fn set_error(&self, message: Option<String>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.error_message = message;
        }
    }

    /// Number of times [`AutostartProvider::query_state`] was called.
    #[must_use]
    pub fn query_call_count(&self) -> usize {
        self.query_calls.load(Ordering::SeqCst)
    }

    /// Number of times [`AutostartProvider::open_startup_settings`] was called.
    #[must_use]
    pub fn settings_opened_count(&self) -> usize {
        self.settings_opened.load(Ordering::SeqCst)
    }

    /// History of `(requested_enabled, resulting_state)` transitions recorded by `set_enabled`.
    #[must_use]
    pub fn toggle_history(&self) -> Vec<(bool, StartupTaskState)> {
        self.inner
            .lock()
            .map(|g| g.toggle_history.clone())
            .unwrap_or_default()
    }
}

impl AutostartProvider for FakeAutostartProvider {
    fn query_state(&self) -> Result<StartupTaskState> {
        self.query_calls.fetch_add(1, Ordering::SeqCst);
        let guard = self
            .inner
            .lock()
            .map_err(|_| PlatformError::Autostart("fake autostart lock poisoned".into()))?;
        if let Some(ref msg) = guard.error_message {
            return Err(PlatformError::Autostart(msg.clone()));
        }
        Ok(guard.state)
    }

    fn set_enabled(&self, enabled: bool) -> Result<StartupTaskState> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| PlatformError::Autostart("fake autostart lock poisoned".into()))?;
        if let Some(ref msg) = guard.error_message {
            return Err(PlatformError::Autostart(msg.clone()));
        }

        let next_state = match (guard.state, enabled) {
            (StartupTaskState::Disabled, true) => {
                let cmd = guard.entry.command_line()?;
                guard.registered_command = Some(cmd);
                StartupTaskState::Enabled
            }
            (StartupTaskState::Enabled, false) => {
                guard.registered_command = None;
                StartupTaskState::Disabled
            }
            (StartupTaskState::Enabled, true) => {
                // Refresh command line if entry changed.
                let cmd = guard.entry.command_line()?;
                guard.registered_command = Some(cmd);
                StartupTaskState::Enabled
            }
            (StartupTaskState::Disabled, false) => {
                guard.registered_command = None;
                StartupTaskState::Disabled
            }
            // User-disabled and policy-locked states cannot be overridden programmatically.
            (StartupTaskState::DisabledByUser, _) => StartupTaskState::DisabledByUser,
            (StartupTaskState::DisabledByPolicy, _) => StartupTaskState::DisabledByPolicy,
            (StartupTaskState::EnabledByPolicy, _) => StartupTaskState::EnabledByPolicy,
        };
        guard.state = next_state;
        guard.toggle_history.push((enabled, next_state));
        Ok(next_state)
    }

    fn open_startup_settings(&self) -> Result<()> {
        let guard = self
            .inner
            .lock()
            .map_err(|_| PlatformError::Autostart("fake autostart lock poisoned".into()))?;
        if let Some(ref msg) = guard.error_message {
            return Err(PlatformError::Autostart(msg.clone()));
        }
        drop(guard);
        self.settings_opened.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn registered_command(&self) -> Result<Option<String>> {
        let guard = self
            .inner
            .lock()
            .map_err(|_| PlatformError::Autostart("fake autostart lock poisoned".into()))?;
        if let Some(ref msg) = guard.error_message {
            return Err(PlatformError::Autostart(msg.clone()));
        }
        Ok(guard.registered_command.clone())
    }
}

// ============================================================================
// Production Windows WinRT StartupTask Provider
// ============================================================================

/// Production Windows MSIX/Store startup task provider backed by WinRT
/// `Windows.ApplicationModel.StartupTask` (`windows_autostart_task.cpp`).
///
/// Runs WinRT calls on a dedicated MTA worker thread so calling from an STA UI thread never
/// deadlocks while awaiting `IAsyncOperation`.
#[derive(Debug, Clone)]
pub struct WinRtStartupTaskProvider {
    task_id: String,
}

impl Default for WinRtStartupTaskProvider {
    fn default() -> Self {
        Self::new(DEFAULT_STARTUP_TASK_ID)
    }
}

impl WinRtStartupTaskProvider {
    /// Creates a WinRT `StartupTask` provider for `task_id`.
    #[must_use]
    pub fn new(task_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
        }
    }

    /// Returns the configured WinRT `StartupTask` identifier.
    #[must_use]
    pub fn task_id(&self) -> &str {
        &self.task_id
    }
}

impl AutostartProvider for WinRtStartupTaskProvider {
    fn query_state(&self) -> Result<StartupTaskState> {
        #[cfg(windows)]
        {
            winrt_impl::query_startup_task(&self.task_id)
        }
        #[cfg(not(windows))]
        {
            Err(PlatformError::Unsupported)
        }
    }

    fn set_enabled(&self, enabled: bool) -> Result<StartupTaskState> {
        #[cfg(windows)]
        {
            winrt_impl::toggle_startup_task(&self.task_id, enabled)
        }
        #[cfg(not(windows))]
        {
            let _ = enabled;
            Err(PlatformError::Unsupported)
        }
    }

    fn open_startup_settings(&self) -> Result<()> {
        #[cfg(windows)]
        {
            winrt_impl::open_startup_settings()
        }
        #[cfg(not(windows))]
        {
            Err(PlatformError::Unsupported)
        }
    }
}

// ============================================================================
// Production Windows HKCU Run Key Provider
// ============================================================================

/// Production Windows desktop autostart provider backed by the
/// `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` registry key.
#[derive(Debug, Clone)]
pub struct WindowsRunKeyAutostartProvider {
    root: RegistryRoot,
    run_subkey: String,
    entry: AutostartEntry,
}

impl WindowsRunKeyAutostartProvider {
    /// Creates a provider targeting the real `HKEY_CURRENT_USER` `Run` key for `entry`.
    #[must_use]
    pub fn hkcu(entry: AutostartEntry) -> Self {
        Self {
            root: RegistryRoot::hkcu(),
            run_subkey: WINDOWS_RUN_KEY_PATH.to_owned(),
            entry,
        }
    }

    /// Creates a provider targeting an explicit [`RegistryRoot`] and subkey path.
    #[must_use]
    pub fn with_root(
        root: RegistryRoot,
        run_subkey: impl Into<String>,
        entry: AutostartEntry,
    ) -> Self {
        Self {
            root,
            run_subkey: run_subkey.into(),
            entry,
        }
    }

    /// Returns the configured [`AutostartEntry`].
    #[must_use]
    pub fn entry(&self) -> &AutostartEntry {
        &self.entry
    }
}

impl AutostartProvider for WindowsRunKeyAutostartProvider {
    fn query_state(&self) -> Result<StartupTaskState> {
        #[cfg(windows)]
        {
            let Some(key) =
                crate::registration::win_reg::open_reg_key(&self.root, &self.run_subkey, false)?
            else {
                return Ok(StartupTaskState::Disabled);
            };
            let present = crate::registration::win_reg::read_reg_sz(
                key.0,
                Some(self.entry.app_name.as_str()),
            )
            .is_some_and(|v| !v.trim().is_empty());
            Ok(if present {
                StartupTaskState::Enabled
            } else {
                StartupTaskState::Disabled
            })
        }
        #[cfg(not(windows))]
        {
            Err(PlatformError::Unsupported)
        }
    }

    fn set_enabled(&self, enabled: bool) -> Result<StartupTaskState> {
        #[cfg(windows)]
        {
            if enabled {
                let command = self.entry.command_line()?;
                let key =
                    crate::registration::win_reg::open_reg_key(&self.root, &self.run_subkey, true)?
                        .ok_or_else(|| {
                            PlatformError::Registry(format!("could not open '{}'", self.run_subkey))
                        })?;
                crate::registration::win_reg::set_reg_sz(
                    key.0,
                    Some(self.entry.app_name.as_str()),
                    &command,
                )?;
                Ok(StartupTaskState::Enabled)
            } else {
                if let Some(key) =
                    crate::registration::win_reg::open_reg_key(&self.root, &self.run_subkey, true)?
                {
                    let _ =
                        crate::registration::win_reg::remove_reg_value(key.0, &self.entry.app_name);
                }
                Ok(StartupTaskState::Disabled)
            }
        }
        #[cfg(not(windows))]
        {
            let _ = enabled;
            Err(PlatformError::Unsupported)
        }
    }

    fn open_startup_settings(&self) -> Result<()> {
        #[cfg(windows)]
        {
            winrt_impl::open_startup_settings()
        }
        #[cfg(not(windows))]
        {
            Err(PlatformError::Unsupported)
        }
    }

    fn registered_command(&self) -> Result<Option<String>> {
        #[cfg(windows)]
        {
            let Some(key) =
                crate::registration::win_reg::open_reg_key(&self.root, &self.run_subkey, false)?
            else {
                return Ok(None);
            };
            Ok(crate::registration::win_reg::read_reg_sz(
                key.0,
                Some(self.entry.app_name.as_str()),
            ))
        }
        #[cfg(not(windows))]
        {
            Err(PlatformError::Unsupported)
        }
    }
}

#[cfg(windows)]
mod winrt_impl {
    use windows::ApplicationModel::{StartupTask, StartupTaskState as WinRtStartupTaskState};
    use windows::Foundation::Uri;
    use windows::System::Launcher;
    use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
    use windows_core::HSTRING;

    use super::{STARTUP_APPS_SETTINGS_URI, StartupTaskState};
    use crate::error::{PlatformError, Result};

    struct RoInitGuard(bool);

    impl RoInitGuard {
        fn enter_mta() -> Self {
            // SAFETY: Initializes WinRT MTA on a dedicated worker thread; balanced by
            // `RoUninitialize` in `Drop` only when initialization succeeded.
            let ok = unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_ok();
            Self(ok)
        }
    }

    impl Drop for RoInitGuard {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: Matches the successful `RoInitialize` call on the same thread.
                unsafe { RoUninitialize() };
            }
        }
    }

    fn map_state(state: WinRtStartupTaskState) -> StartupTaskState {
        match state {
            WinRtStartupTaskState::Disabled => StartupTaskState::Disabled,
            WinRtStartupTaskState::DisabledByUser => StartupTaskState::DisabledByUser,
            WinRtStartupTaskState::Enabled => StartupTaskState::Enabled,
            WinRtStartupTaskState::DisabledByPolicy => StartupTaskState::DisabledByPolicy,
            WinRtStartupTaskState::EnabledByPolicy => StartupTaskState::EnabledByPolicy,
            _ => StartupTaskState::Disabled,
        }
    }

    pub(super) fn query_startup_task(task_id: &str) -> Result<StartupTaskState> {
        let task_id_owned = task_id.to_owned();
        let handle = std::thread::Builder::new()
            .name("tg-winrt-startup-query".to_owned())
            .spawn(move || -> Result<StartupTaskState> {
                let _guard = RoInitGuard::enter_mta();
                let h_id = HSTRING::from(&task_id_owned);
                let task = StartupTask::GetAsync(&h_id)
                    .map_err(|e| PlatformError::from_win("StartupTask::GetAsync", e))?
                    .join()
                    .map_err(|e| PlatformError::from_win("StartupTask::GetAsync::join", e))?;
                let state = task
                    .State()
                    .map_err(|e| PlatformError::from_win("StartupTask::State", e))?;
                Ok(map_state(state))
            })
            .map_err(PlatformError::Io)?;
        handle.join().map_err(|_| {
            PlatformError::Autostart("WinRT StartupTask query thread panicked".into())
        })?
    }

    pub(super) fn toggle_startup_task(task_id: &str, enabled: bool) -> Result<StartupTaskState> {
        let task_id_owned = task_id.to_owned();
        let handle = std::thread::Builder::new()
            .name("tg-winrt-startup-toggle".to_owned())
            .spawn(move || -> Result<StartupTaskState> {
                let _guard = RoInitGuard::enter_mta();
                let h_id = HSTRING::from(&task_id_owned);
                let task = StartupTask::GetAsync(&h_id)
                    .map_err(|e| PlatformError::from_win("StartupTask::GetAsync", e))?
                    .join()
                    .map_err(|e| PlatformError::from_win("StartupTask::GetAsync::join", e))?;
                let current = map_state(
                    task.State()
                        .map_err(|e| PlatformError::from_win("StartupTask::State", e))?,
                );
                if current.is_enabled() == enabled {
                    return Ok(current);
                }
                if !enabled {
                    task.Disable()
                        .map_err(|e| PlatformError::from_win("StartupTask::Disable", e))?;
                    let after = task
                        .State()
                        .map_err(|e| PlatformError::from_win("StartupTask::State(after)", e))?;
                    return Ok(map_state(after));
                }
                let updated = task
                    .RequestEnableAsync()
                    .map_err(|e| PlatformError::from_win("StartupTask::RequestEnableAsync", e))?
                    .join()
                    .map_err(|e| {
                        PlatformError::from_win("StartupTask::RequestEnableAsync::join", e)
                    })?;
                Ok(map_state(updated))
            })
            .map_err(PlatformError::Io)?;
        handle.join().map_err(|_| {
            PlatformError::Autostart("WinRT StartupTask toggle thread panicked".into())
        })?
    }

    pub(super) fn open_startup_settings() -> Result<()> {
        let handle = std::thread::Builder::new()
            .name("tg-winrt-startup-settings".to_owned())
            .spawn(|| -> Result<()> {
                let _guard = RoInitGuard::enter_mta();
                let uri = Uri::CreateUri(&HSTRING::from(STARTUP_APPS_SETTINGS_URI))
                    .map_err(|e| PlatformError::from_win("Uri::CreateUri", e))?;
                let _launched = Launcher::LaunchUriAsync(&uri)
                    .map_err(|e| PlatformError::from_win("Launcher::LaunchUriAsync", e))?
                    .join()
                    .map_err(|e| PlatformError::from_win("Launcher::LaunchUriAsync::join", e))?;
                Ok(())
            })
            .map_err(PlatformError::Io)?;
        handle.join().map_err(|_| {
            PlatformError::Autostart("WinRT startup settings thread panicked".into())
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_quoting_round_trips_edge_cases() {
        let entry = AutostartEntry::for_executable(PathBuf::from(
            r"C:\Program Files\Rustgram App\rustgram.exe",
        ))
        .with_workdir(Path::new(r"C:\Users\Test User\Rustgram Data\"));

        let cmd = entry.command_line().unwrap();
        assert_eq!(
            cmd,
            r#""C:\Program Files\Rustgram App\rustgram.exe" -autostart -workdir "C:\Users\Test User\Rustgram Data\\""#
        );

        let parsed = parse_command_line_args(&cmd);
        assert_eq!(
            parsed,
            vec![
                r"C:\Program Files\Rustgram App\rustgram.exe",
                "-autostart",
                "-workdir",
                r"C:\Users\Test User\Rustgram Data\",
            ]
        );
    }

    #[test]
    fn quoting_rejects_empty_and_control_chars() {
        assert!(quote_executable_path(Path::new("")).is_err());
        assert!(quote_executable_path(Path::new("C:\\bad\npath.exe")).is_err());
        assert!(quote_command_argument("bad\0arg").is_err());
    }

    #[test]
    fn fake_autostart_provider_transitions_and_policy_locks() {
        let provider = FakeAutostartProvider::default();
        assert_eq!(provider.query_state().unwrap(), StartupTaskState::Disabled);
        assert!(!provider.is_enabled().unwrap());
        assert_eq!(provider.registered_command().unwrap(), None);

        assert_eq!(
            provider.set_enabled(true).unwrap(),
            StartupTaskState::Enabled
        );
        assert!(provider.is_enabled().unwrap());
        assert_eq!(
            provider.registered_command().unwrap().as_deref(),
            Some(r#""C:\Program Files\Rustgram\rustgram.exe" -autostart"#)
        );

        assert_eq!(
            provider.set_enabled(false).unwrap(),
            StartupTaskState::Disabled
        );
        assert_eq!(provider.registered_command().unwrap(), None);

        // DisabledByUser cannot be enabled programmatically; settings must be opened instead.
        provider.set_state(StartupTaskState::DisabledByUser);
        assert_eq!(
            provider.set_enabled(true).unwrap(),
            StartupTaskState::DisabledByUser
        );
        assert!(
            provider
                .query_state()
                .unwrap()
                .is_blocked_by_user_or_policy()
        );
        provider.open_startup_settings().unwrap();
        assert_eq!(provider.settings_opened_count(), 1);
    }
}
