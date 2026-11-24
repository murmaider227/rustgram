//! Self-update HTTP transport (`reqwest` with `rustls-tls`, no OpenSSL), state machine,
//! periodic background checker, and `rustgram-updater` restart launcher.
//!
//! Port origin: `Telegram/SourceFiles/core/update_checker.cpp` and
//! `Telegram/SourceFiles/platform/win/launcher_win.cpp` (`launchUpdater`).
//!
//! ## UI Integration Contract
//!
//! `WindowController` exposes read-only accessors [`crate::window::controller::WindowController::update_state`]
//! and [`crate::window::controller::WindowController::update_snapshot`], plus action triggers
//! `request_update_check()` and `request_restart_to_update()`:
//! - **Main Menu (`window/main_menu.rs`)**: read `update_snapshot.is_ready()` to display a
//!   `"Update Rustgram"` / `"Restart to update"` footer banner or action row, triggering
//!   `request_restart_to_update()`.
//! - **Settings > Advanced (`settings/advanced.rs`, section "6. Version & Updates")**: read
//!   `update_snapshot.status_label()` and `update_snapshot.state` beneath the
//!   `auto_update` / `install_beta_version` toggles to show live status (Checking / Up to date /
//!   Downloading / Ready to install / Error), a `"Check for Updates"` button calling
//!   `request_update_check()`, and a `"Restart to Update"` button when `UpdateState::Ready`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tg_core::launcher::ParsedArgs;
use tg_core::update_checker::UPDATE_DELAY_CONST_SECS;
use tg_updater::checker::{
    StagedUpdate, UpdateChecker, UpdateConfig, UpdaterLaunchOptions, build_updater_launch_args,
    has_ready_update,
};
use tg_updater::github::HttpClient;
use tg_updater::package::read_version_file;
use tg_updater::version::{Target, Version};
use tg_updater::{
    DEFAULT_GITHUB_REPO, TUPDATES_DIR_NAME, UPDATER_EXE_NAME, UpdateError, release_verifying_key,
    verifying_key_from_hex,
};

use super::plan::StartupPrefs;

/// Environment variable allowing a maintainer/test hex Ed25519 verifying key override.
pub const UPDATER_PUBLIC_KEY_ENV: &str = "RUSTGRAM_UPDATER_PUBLIC_KEY";

/// Legacy fallback environment variable for the updater verifying key override.
pub const LEGACY_UPDATER_PUBLIC_KEY_ENV: &str = "TGFORK_UPDATER_PUBLIC_KEY";

/// Environment variable allowing a custom GitHub `owner/repo` slug override.
pub const UPDATER_REPO_ENV: &str = "RUSTGRAM_UPDATE_REPO";

/// Legacy fallback environment variable for the updater repository slug override.
pub const LEGACY_UPDATER_REPO_ENV: &str = "TGFORK_UPDATE_REPO";

/// Default periodic check interval (8 hours, matching `UpdateDelayConstPart` in `update_checker.cpp`).
pub const DEFAULT_CHECK_INTERVAL_SECS: i64 = UPDATE_DELAY_CONST_SECS;

/// Connect timeout for HTTP update requests.
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Overall request timeout for HTTP update requests.
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

pub use crate::core_types::startup_types::{
    UpdatePreferencesSink, UpdateSnapshot, UpdateState, UpdateUiAction, current_app_version,
};

/// Pure-Rust TLS (`reqwest` + `rustls-tls`, zero OpenSSL) implementation of
/// [`tg_updater::github::HttpClient`].
#[derive(Debug, Clone)]
pub struct RustlsHttpClient {
    client: reqwest::blocking::Client,
}

impl RustlsHttpClient {
    /// Constructs a new rustls-backed blocking HTTP client with sensible timeouts.
    pub fn new() -> Result<Self, UpdateError> {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .timeout(HTTP_REQUEST_TIMEOUT)
            .build()
            .map_err(|e| UpdateError::Network(format!("failed to initialize HTTP client: {e}")))?;
        Ok(Self { client })
    }
}

impl HttpClient for RustlsHttpClient {
    fn get_bytes(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        max_bytes: usize,
    ) -> Result<Vec<u8>, UpdateError> {
        let mut req = self.client.get(url);
        for &(k, v) in headers {
            req = req.header(k, v);
        }

        let mut resp = req
            .send()
            .map_err(|e| UpdateError::Network(format!("GET {url} failed: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(UpdateError::Network(format!(
                "GET {url} returned HTTP {}",
                status.as_u16()
            )));
        }

        if let Some(content_len) = resp.content_length()
            && content_len > max_bytes as u64
        {
            return Err(UpdateError::ResponseTooLarge { limit: max_bytes });
        }

        // Read at most `max_bytes + 1` bytes so oversized chunked responses are rejected cleanly.
        let cap = resp
            .content_length()
            .map(|n| (n as usize).min(max_bytes))
            .unwrap_or(0);
        let mut buf = Vec::with_capacity(cap);
        let mut limited = (&mut resp).take((max_bytes as u64).saturating_add(1));
        limited
            .read_to_end(&mut buf)
            .map_err(|e| UpdateError::Network(format!("reading response body failed: {e}")))?;

        if buf.len() > max_bytes {
            return Err(UpdateError::ResponseTooLarge { limit: max_bytes });
        }
        Ok(buf)
    }
}

/// Deterministic, offline-testable update state machine wrapping [`tg_updater::checker::UpdateChecker`].
pub struct UpdateStateMachine<C: HttpClient> {
    client: C,
    config: UpdateConfig,
    state: UpdateState,
    auto_update: bool,
    disabled_by_cli: bool,
    last_check_unix: i64,
    check_interval_secs: i64,
}

impl<C: HttpClient> UpdateStateMachine<C> {
    /// Creates a new state machine, inspecting `<working_dir>/tupdates/temp/ready` for an already
    /// staged update left over from a previous run.
    #[must_use]
    pub fn new(
        client: C,
        config: UpdateConfig,
        auto_update: bool,
        disabled_by_cli: bool,
        last_check_unix: i64,
    ) -> Self {
        let state = if has_ready_update(&config.working_dir) {
            let staging_dir = config.working_dir.join(TUPDATES_DIR_NAME).join("temp");
            let version =
                read_staged_version(&staging_dir).unwrap_or_else(|| config.current_version.clone());
            UpdateState::Ready {
                version,
                staging_dir,
                updater_promoted: false,
            }
        } else if disabled_by_cli || !auto_update {
            UpdateState::Disabled
        } else {
            UpdateState::Idle
        };

        Self {
            client,
            config,
            state,
            auto_update,
            disabled_by_cli,
            last_check_unix,
            check_interval_secs: DEFAULT_CHECK_INTERVAL_SECS,
        }
    }

    /// Overrides the periodic check interval (primarily for deterministic unit tests).
    pub fn set_check_interval_secs(&mut self, secs: i64) {
        self.check_interval_secs = secs.max(1);
    }

    /// Updates user preferences (`auto_update`, `allow_beta`) at runtime.
    pub fn set_preferences(&mut self, auto_update: bool, allow_beta: bool) {
        self.auto_update = auto_update;
        self.config.allow_beta = allow_beta;
        if !self.disabled_by_cli && !matches!(self.state, UpdateState::Ready { .. }) {
            if !auto_update
                && matches!(self.state, UpdateState::Idle | UpdateState::UpToDate { .. })
            {
                self.state = UpdateState::Disabled;
            } else if auto_update && matches!(self.state, UpdateState::Disabled) {
                self.state = UpdateState::Idle;
            }
        }
    }

    /// Returns the current [`UpdateState`].
    #[must_use]
    pub fn state(&self) -> &UpdateState {
        &self.state
    }

    /// Returns a read-only [`UpdateSnapshot`] for UI consumption.
    #[must_use]
    pub fn snapshot(&self) -> UpdateSnapshot {
        let next_check_unix = if self.disabled_by_cli
            || !self.auto_update
            || matches!(self.state, UpdateState::Ready { .. })
        {
            None
        } else if self.last_check_unix <= 0 {
            Some(0)
        } else {
            Some(
                self.last_check_unix
                    .saturating_add(self.check_interval_secs),
            )
        };

        UpdateSnapshot {
            state: self.state.clone(),
            current_version: self.config.current_version.clone(),
            auto_update_enabled: self.auto_update,
            allow_beta: self.config.allow_beta,
            disabled_by_cli: self.disabled_by_cli,
            last_check_unix: self.last_check_unix,
            next_check_unix,
        }
    }

    /// Returns `true` if a periodic background check is due at `now_unix`.
    #[must_use]
    pub fn is_periodic_check_due(&self, now_unix: i64) -> bool {
        if self.disabled_by_cli || !self.auto_update {
            return false;
        }
        if matches!(
            self.state,
            UpdateState::Checking | UpdateState::Downloading { .. } | UpdateState::Ready { .. }
        ) {
            return false;
        }
        if self.last_check_unix <= 0 {
            return true;
        }
        now_unix.saturating_sub(self.last_check_unix) >= self.check_interval_secs
    }

    /// Runs a periodic check if due at `now_unix`, notifying `on_state` on every state transition.
    ///
    /// Returns `true` if a network check was actually performed.
    pub fn poll_periodic<F>(&mut self, now_unix: i64, on_state: F) -> bool
    where
        F: FnMut(&UpdateSnapshot),
    {
        if !self.is_periodic_check_due(now_unix) {
            return false;
        }
        self.run_check_and_stage(now_unix, on_state);
        true
    }

    /// Runs an immediate check (manual trigger), notifying `on_state` on every transition.
    ///
    /// Skipped only when `-noupdate` was passed on the command line or an update is already `Ready`.
    pub fn check_now<F>(&mut self, now_unix: i64, mut on_state: F) -> &UpdateState
    where
        F: FnMut(&UpdateSnapshot),
    {
        if self.disabled_by_cli {
            self.state = UpdateState::Disabled;
            on_state(&self.snapshot());
            return &self.state;
        }
        if matches!(self.state, UpdateState::Ready { .. }) {
            on_state(&self.snapshot());
            return &self.state;
        }
        self.run_check_and_stage(now_unix, on_state);
        &self.state
    }

    fn run_check_and_stage<F>(&mut self, now_unix: i64, mut on_state: F)
    where
        F: FnMut(&UpdateSnapshot),
    {
        self.state = UpdateState::Checking;
        on_state(&self.snapshot());

        let checker = UpdateChecker::new(RefHttpClient(&self.client), self.config.clone());
        match checker.check() {
            Ok(None) => {
                self.last_check_unix = now_unix;
                self.state = UpdateState::UpToDate {
                    checked_at_unix: now_unix,
                };
                on_state(&self.snapshot());
            }
            Ok(Some(available)) => {
                self.state = UpdateState::Available(available.clone());
                on_state(&self.snapshot());

                self.state = UpdateState::Downloading {
                    version: available.version.clone(),
                    downloaded_bytes: 0,
                    total_bytes: available.size,
                };
                on_state(&self.snapshot());

                match checker.download_and_stage(&available) {
                    Ok(StagedUpdate {
                        version,
                        staging_dir,
                        updater_promoted,
                    }) => {
                        self.last_check_unix = now_unix;
                        self.state = UpdateState::Ready {
                            version,
                            staging_dir,
                            updater_promoted,
                        };
                        on_state(&self.snapshot());
                    }
                    Err(err) => {
                        self.last_check_unix = now_unix;
                        self.state = UpdateState::Failed {
                            message: err.to_string(),
                            checked_at_unix: now_unix,
                        };
                        on_state(&self.snapshot());
                    }
                }
            }
            Err(err) => {
                self.last_check_unix = now_unix;
                self.state = UpdateState::Failed {
                    message: err.to_string(),
                    checked_at_unix: now_unix,
                };
                on_state(&self.snapshot());
            }
        }
    }
}

/// Thin borrowing adapter so `UpdateChecker` can borrow `&C` without moving `self.client`.
struct RefHttpClient<'a, C: HttpClient>(&'a C);

impl<C: HttpClient> HttpClient for RefHttpClient<'_, C> {
    fn get_bytes(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        max_bytes: usize,
    ) -> Result<Vec<u8>, UpdateError> {
        self.0.get_bytes(url, headers, max_bytes)
    }
}

fn read_staged_version(staging_dir: &Path) -> Option<Version> {
    let version_file = staging_dir.join("tdata").join("version");
    let (_, ver_str) = read_version_file(&version_file).ok()?;
    Version::parse(&ver_str).ok()
}

/// Command sent from the UI thread to the background updater worker thread.
enum WorkerCommand {
    CheckNow,
    UpdatePrefs { auto_update: bool, allow_beta: bool },
    Stop,
}

/// Parameters required to spawn `rustgram-updater.exe` and exit cleanly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdaterLaunchPlan {
    /// Absolute path to `rustgram-updater.exe`.
    pub updater_exe: PathBuf,
    /// Working directory (`install_dir`) for the updater process.
    pub install_dir: PathBuf,
    /// Command-line arguments produced by [`build_updater_launch_args`].
    pub args: Vec<String>,
}

/// Builds the exact `rustgram-updater.exe` invocation plan for restarting and installing a staged update.
#[must_use]
pub fn build_updater_launch_plan(
    exe_path: &Path,
    working_dir: &Path,
    is_custom_workdir: bool,
    parsed_args: &ParsedArgs,
    parent_pid: Option<u32>,
    parent_created: Option<u64>,
) -> UpdaterLaunchPlan {
    let install_dir = exe_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let exe_name = exe_path
        .file_name()
        .and_then(|s| s.to_str())
        .map(ToOwned::to_owned);
    let updater_exe = install_dir.join(UPDATER_EXE_NAME);

    let opts = UpdaterLaunchOptions {
        parent_pid,
        parent_created,
        workdir: is_custom_workdir.then(|| working_dir.to_path_buf()),
        key: (!parsed_args.key.is_empty()).then(|| parsed_args.key.clone()),
        exe_name,
        autostart: parsed_args.launch_mode == tg_core::LaunchMode::AutoStart,
        debug: parsed_args.debug,
        start_in_tray: parsed_args.start_in_tray,
        just_update: false,
    };

    UpdaterLaunchPlan {
        updater_exe,
        install_dir,
        args: build_updater_launch_args(&opts),
    }
}

/// Queries the current process creation timestamp (Windows `FILETIME` 100ns ticks) so
/// `rustgram-updater.exe -finishprocess <pid> <created>` can safely identify our process.
#[must_use]
pub fn current_process_creation_time() -> Option<u64> {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: `GetCurrentProcess` returns the pseudo-handle for this process and all four
        // `FILETIME` pointers point to valid stack allocations.
        let ok = unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        };
        if ok.is_ok() {
            let ticks =
                (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
            return (ticks != 0).then_some(ticks);
        }
        None
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Spawns `rustgram-updater.exe` according to `plan`.
pub fn spawn_updater_process(plan: &UpdaterLaunchPlan) -> Result<(), String> {
    if !plan.updater_exe.is_file() {
        return Err(format!(
            "updater executable not found at '{}'",
            plan.updater_exe.display()
        ));
    }
    Command::new(&plan.updater_exe)
        .args(&plan.args)
        .current_dir(&plan.install_dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("failed to launch {}: {e}", plan.updater_exe.display()))
}

/// Thread-safe handle driving periodic background update checks and exposing a live
/// [`UpdateSnapshot`] to [`crate::window::controller::WindowController`].
pub struct UpdateController {
    snapshot: Arc<Mutex<UpdateSnapshot>>,
    tx: Option<Sender<WorkerCommand>>,
    worker: Option<JoinHandle<()>>,
    launch_context: Option<UpdateLaunchContext>,
}

#[derive(Debug, Clone)]
struct UpdateLaunchContext {
    exe_path: PathBuf,
    working_dir: PathBuf,
    is_custom_workdir: bool,
    parsed_args: ParsedArgs,
}

impl std::fmt::Debug for UpdateController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateController")
            .field("snapshot", &self.snapshot())
            .field("has_worker", &self.tx.is_some())
            .finish()
    }
}

impl UpdateController {
    /// Creates an inert controller (no background thread) for tests or disabled builds.
    #[must_use]
    pub fn inactive(
        current_version: Version,
        disabled_by_cli: bool,
        auto_update_enabled: bool,
        allow_beta: bool,
    ) -> Self {
        let state = if disabled_by_cli || !auto_update_enabled {
            UpdateState::Disabled
        } else {
            UpdateState::Idle
        };
        Self {
            snapshot: Arc::new(Mutex::new(UpdateSnapshot {
                state,
                current_version,
                auto_update_enabled,
                allow_beta,
                disabled_by_cli,
                last_check_unix: 0,
                next_check_unix: None,
            })),
            tx: None,
            worker: None,
            launch_context: None,
        }
    }

    /// Starts the background periodic updater thread (unless `-noupdate` is set or no verifying key
    /// is configured, in which case it still checks for pre-staged updates and serves snapshots).
    #[must_use]
    pub fn start(
        exe_path: PathBuf,
        working_dir: PathBuf,
        is_custom_workdir: bool,
        parsed_args: ParsedArgs,
        prefs: &StartupPrefs,
    ) -> Self {
        let current_version = current_app_version();
        let disabled_by_cli = parsed_args.no_update;
        let auto_update = prefs.auto_update;
        let allow_beta = prefs.install_beta;
        let last_check_unix = prefs.last_update_check_unix;

        let launch_context = Some(UpdateLaunchContext {
            exe_path: exe_path.clone(),
            working_dir: working_dir.clone(),
            is_custom_workdir,
            parsed_args,
        });

        let install_dir = exe_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        // If a ready update is already staged on disk, surface it immediately even if verifying key
        // or network is unavailable.
        let pre_staged = has_ready_update(&working_dir).then(|| {
            let staging_dir = working_dir.join(TUPDATES_DIR_NAME).join("temp");
            let version =
                read_staged_version(&staging_dir).unwrap_or_else(|| current_version.clone());
            UpdateState::Ready {
                version,
                staging_dir,
                updater_promoted: false,
            }
        });

        let initial_state = if let Some(ready) = pre_staged {
            ready
        } else if disabled_by_cli || !auto_update {
            UpdateState::Disabled
        } else {
            UpdateState::Idle
        };

        let snapshot = Arc::new(Mutex::new(UpdateSnapshot {
            state: initial_state,
            current_version: current_version.clone(),
            auto_update_enabled: auto_update,
            allow_beta,
            disabled_by_cli,
            last_check_unix,
            next_check_unix: (!disabled_by_cli && auto_update)
                .then(|| last_check_unix.saturating_add(DEFAULT_CHECK_INTERVAL_SECS)),
        }));

        if disabled_by_cli {
            return Self {
                snapshot,
                tx: None,
                worker: None,
                launch_context,
            };
        }

        let verifying_key_res = match std::env::var(UPDATER_PUBLIC_KEY_ENV)
            .ok()
            .filter(|k| !k.trim().is_empty())
            .or_else(|| {
                std::env::var(LEGACY_UPDATER_PUBLIC_KEY_ENV)
                    .ok()
                    .filter(|k| !k.trim().is_empty())
            }) {
            Some(hex_key) => verifying_key_from_hex(&hex_key),
            None => release_verifying_key(),
        };
        let verifying_key = match verifying_key_res {
            Ok(k) => k,
            Err(err) => {
                tracing::info!(
                    %err,
                    "Background updater inactive until a release verifying key is configured"
                );
                return Self {
                    snapshot,
                    tx: None,
                    worker: None,
                    launch_context,
                };
            }
        };

        let http_client = match RustlsHttpClient::new() {
            Ok(c) => c,
            Err(err) => {
                tracing::warn!(%err, "Could not create rustls HTTP client for updater");
                return Self {
                    snapshot,
                    tx: None,
                    worker: None,
                    launch_context,
                };
            }
        };

        let github_repo = std::env::var(UPDATER_REPO_ENV)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                std::env::var(LEGACY_UPDATER_REPO_ENV)
                    .ok()
                    .filter(|s| !s.trim().is_empty())
            })
            .unwrap_or_else(|| DEFAULT_GITHUB_REPO.to_owned());

        let config = UpdateConfig {
            github_repo,
            api_base_url: None,
            current_version,
            allow_beta,
            target: Target::current(),
            working_dir: working_dir.clone(),
            install_dir,
            verifying_key,
        };

        let (tx, rx) = mpsc::channel::<WorkerCommand>();
        let snap_shared = Arc::clone(&snapshot);
        let wd_for_prefs = working_dir;

        let worker = thread::Builder::new()
            .name("rustgram-updater-check".to_owned())
            .spawn(move || {
                let mut machine = UpdateStateMachine::new(
                    http_client,
                    config,
                    auto_update,
                    disabled_by_cli,
                    last_check_unix,
                );
                publish_snapshot(&snap_shared, &machine.snapshot());

                // Initial startup check if periodic schedule says it is due.
                let now = current_unix_secs();
                if machine.poll_periodic(now, |s| publish_snapshot(&snap_shared, s)) {
                    persist_last_check(&wd_for_prefs, machine.snapshot().last_check_unix);
                }

                // Periodic tick every 60s to see whether the 8h window has elapsed or a command arrived.
                let tick = Duration::from_secs(60);
                loop {
                    match rx.recv_timeout(tick) {
                        Ok(WorkerCommand::CheckNow) => {
                            let now = current_unix_secs();
                            machine.check_now(now, |s| publish_snapshot(&snap_shared, s));
                            persist_last_check(&wd_for_prefs, machine.snapshot().last_check_unix);
                        }
                        Ok(WorkerCommand::UpdatePrefs {
                            auto_update,
                            allow_beta,
                        }) => {
                            machine.set_preferences(auto_update, allow_beta);
                            publish_snapshot(&snap_shared, &machine.snapshot());
                        }
                        Ok(WorkerCommand::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                        Err(RecvTimeoutError::Timeout) => {
                            let now = current_unix_secs();
                            if machine.poll_periodic(now, |s| publish_snapshot(&snap_shared, s)) {
                                persist_last_check(
                                    &wd_for_prefs,
                                    machine.snapshot().last_check_unix,
                                );
                            }
                        }
                    }
                }
            })
            .ok();

        Self {
            snapshot,
            tx: worker.as_ref().map(|_| tx),
            worker,
            launch_context,
        }
    }

    /// Returns a cloned read-only [`UpdateSnapshot`].
    #[must_use]
    pub fn snapshot(&self) -> UpdateSnapshot {
        self.snapshot.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Requests an immediate background update check.
    pub fn request_check_now(&self) {
        if let Some(ref tx) = self.tx {
            let _ = tx.send(WorkerCommand::CheckNow);
        } else if let Ok(mut snap) = self.snapshot.lock()
            && !snap.disabled_by_cli
            && !snap.is_ready()
        {
            snap.state = UpdateState::Failed {
                message: "Release verifying key is not configured for this build".to_owned(),
                checked_at_unix: current_unix_secs(),
            };
        }
    }

    /// Propagates updated auto-update / beta preferences to the background worker.
    pub fn update_preferences(&self, auto_update: bool, allow_beta: bool) {
        if let Some(ref tx) = self.tx {
            let _ = tx.send(WorkerCommand::UpdatePrefs {
                auto_update,
                allow_beta,
            });
        } else if let Ok(mut snap) = self.snapshot.lock() {
            snap.auto_update_enabled = auto_update;
            snap.allow_beta = allow_beta;
        }
    }

    /// Builds and spawns `rustgram-updater.exe` to apply a ready update and relaunch `rustgram.exe`.
    ///
    /// Returns the spawned [`UpdaterLaunchPlan`] on success so the caller can close logs,
    /// finish crash reporting cleanly, and exit the process.
    pub fn launch_updater_for_restart(&self) -> Result<UpdaterLaunchPlan, String> {
        let snap = self.snapshot();
        if !snap.is_ready() {
            return Err("no staged update is ready to install".to_owned());
        }
        let ctx = self
            .launch_context
            .as_ref()
            .ok_or_else(|| "updater launch context is unavailable".to_owned())?;
        let plan = build_updater_launch_plan(
            &ctx.exe_path,
            &ctx.working_dir,
            ctx.is_custom_workdir,
            &ctx.parsed_args,
            Some(std::process::id()),
            current_process_creation_time(),
        );
        spawn_updater_process(&plan)?;
        Ok(plan)
    }

    /// Signals the background updater worker to stop.
    pub fn shutdown(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(WorkerCommand::Stop);
        }
        if let Some(handle) = self.worker.take() {
            // Detach if mid-HTTP request so app shutdown is never blocked on network I/O.
            drop(handle);
        }
    }
}

impl UpdatePreferencesSink for UpdateController {
    fn update_preferences(&self, auto_update: bool, allow_beta: bool) {
        UpdateController::update_preferences(self, auto_update, allow_beta);
    }
}

impl Drop for UpdateController {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn publish_snapshot(target: &Arc<Mutex<UpdateSnapshot>>, snap: &UpdateSnapshot) {
    if let Ok(mut guard) = target.lock() {
        *guard = snap.clone();
    }
}

fn persist_last_check(working_dir: &Path, last_check_unix: i64) {
    if last_check_unix <= 0 {
        return;
    }
    let mut prefs = StartupPrefs::load_from(working_dir);
    if prefs.last_update_check_unix != last_check_unix {
        prefs.last_update_check_unix = last_check_unix;
        let _ = prefs.save_to(working_dir);
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
    use super::*;

    #[test]
    fn update_status_ui_formatting_and_preferences_propagation() {
        use std::path::PathBuf;
        use tempfile::TempDir;
        use tg_lang::tr;
        use tg_updater::version::Version;

        use crate::settings::SettingsController;
        use crate::settings::advanced::{
            format_update_status, localized_update_tgfork, propagate_update_preferences,
        };
        use crate::startup::plan::StartupPrefs;

        // 1. Checking
        let checking = UpdateSnapshot {
            state: UpdateState::Checking,
            current_version: Version::new(0, 1, 0),
            ..UpdateSnapshot::default()
        };
        let d_check = format_update_status(&checking);
        assert_eq!(
            d_check.version_line,
            tr::lng_settings_current_version("0.1.0")
        );
        assert_eq!(d_check.status_line, tr::lng_settings_update_checking());
        assert!(!checking.can_check_now());

        // 2. Up to date
        let up_to_date = UpdateSnapshot {
            state: UpdateState::UpToDate {
                checked_at_unix: 1_700_000_000,
            },
            current_version: Version::new(0, 1, 0),
            ..UpdateSnapshot::default()
        };
        let d_latest = format_update_status(&up_to_date);
        assert_eq!(d_latest.status_line, tr::lng_settings_latest_installed());
        assert!(up_to_date.can_check_now());

        // 3. Downloading with progress
        let downloading = UpdateSnapshot {
            state: UpdateState::Downloading {
                version: Version::new(0, 2, 0),
                downloaded_bytes: 5 * 1024 * 1024,
                total_bytes: 20 * 1024 * 1024,
            },
            current_version: Version::new(0, 1, 0),
            ..UpdateSnapshot::default()
        };
        let d_dl = format_update_status(&downloading);
        assert_eq!(d_dl.status_line, tr::lng_settings_downloading_update("25%"));
        assert!((d_dl.download_fraction.unwrap() - 0.25).abs() < 1e-4);
        assert!(d_dl.detail_line.as_deref().unwrap().contains("0.2.0"));

        // 4. Ready to restart
        let ready = UpdateSnapshot {
            state: UpdateState::Ready {
                version: Version::new(0, 2, 0),
                staging_dir: PathBuf::from("tupdates/temp"),
                updater_promoted: true,
            },
            current_version: Version::new(0, 1, 0),
            allow_beta: true,
            ..UpdateSnapshot::default()
        };
        let d_ready = format_update_status(&ready);
        assert_eq!(d_ready.status_line, tr::lng_settings_update_ready());
        assert!(d_ready.is_ready);
        assert!(d_ready.version_line.contains("0.1.0 beta"));
        assert!(localized_update_tgfork().contains("Rustgram"));

        // 5. Failed
        let failed = UpdateSnapshot {
            state: UpdateState::Failed {
                message: "network timeout".to_owned(),
                checked_at_unix: 1_700_000_100,
            },
            current_version: Version::new(0, 1, 0),
            ..UpdateSnapshot::default()
        };
        let d_err = format_update_status(&failed);
        assert_eq!(d_err.status_line, tr::lng_settings_update_fail());
        assert!(d_err.is_error);
        assert_eq!(d_err.detail_line.as_deref(), Some("network timeout"));

        // 6. Propagating auto_update / install_beta writes StartupPrefs AND updates UpdateController.
        let tmp = TempDir::new().expect("tempdir");
        let updater = UpdateController::inactive(Version::new(0, 1, 0), false, true, false);
        propagate_update_preferences(Some(tmp.path()), false, true, Some(&updater))
            .expect("propagate prefs");

        let saved_prefs = StartupPrefs::load_from(tmp.path());
        assert!(!saved_prefs.auto_update);
        assert!(saved_prefs.install_beta);
        let snap = updater.snapshot();
        assert!(!snap.auto_update_enabled);
        assert!(snap.allow_beta);

        // 7. SettingsController syncs initial preferences from UpdateSnapshot.
        let mut ctrl = SettingsController::default();
        ctrl.set_update_snapshot(UpdateSnapshot {
            auto_update_enabled: false,
            allow_beta: true,
            ..UpdateSnapshot::default()
        });
        assert!(!ctrl.global_config.auto_update);
        assert!(ctrl.global_config.install_beta_version);
        assert!(ctrl.has_synced_update_snapshot());
    }
}
