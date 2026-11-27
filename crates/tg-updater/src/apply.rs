//! Staged update application, process handoff, file replacement, and relaunch logic.
//!
//! Port of `Telegram/SourceFiles/_other/updater_win.cpp` for `rustgram-updater`.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use crate::package::read_version_file;
use crate::{DEFAULT_EXE_NAME, TUPDATES_DIR_NAME, UPDATER_EXE_NAME, UpdateError};

/// Maximum time to wait for the parent app to exit gracefully before terminating it (5s),
/// matching `kProcessExitTimeout` in `updater_win.cpp`.
pub const PROCESS_EXIT_TIMEOUT_MS: u32 = 5_000;

/// Number of copy retries when a destination file is temporarily locked by another process.
const DEFAULT_COPY_RETRIES: u32 = 50;

/// Delay between file copy retries.
const DEFAULT_COPY_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Retry policy used when replacing destination files during [`apply_staged_update_with_policy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyRetryPolicy {
    /// Maximum copy attempts per file.
    pub max_retries: u32,
    /// Delay between consecutive retry attempts.
    pub retry_delay: Duration,
}

impl Default for ApplyRetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_COPY_RETRIES,
            retry_delay: DEFAULT_COPY_RETRY_DELAY,
        }
    }
}

impl ApplyRetryPolicy {
    /// Short retry budget suitable for deterministic offline tests.
    #[must_use]
    pub const fn fast() -> Self {
        Self {
            max_retries: 2,
            retry_delay: Duration::from_millis(5),
        }
    }
}

/// Parsed command-line arguments for `rustgram-updater`.
///
/// Mirrors the flag set in `Telegram/SourceFiles/_other/updater_win.cpp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdaterArgs {
    /// `-update`: perform staged file replacement from `tupdates`.
    pub need_update: bool,
    /// `-finishprocess <pid> <creation_time>`: wait for parent process before replacing files.
    pub finish_process: Option<(u32, u64)>,
    /// `-autostart`: forward `-autostart` on relaunch.
    pub autostart: bool,
    /// `-justupdate`: apply update without relaunching the app.
    pub just_update: bool,
    /// `-debug`: enable verbose updater logging and forward `-debug` on relaunch.
    pub debug: bool,
    /// `-startintray`: forward `-startintray` on relaunch.
    pub start_in_tray: bool,
    /// `-writeprotected <dir>`: explicit target installation directory.
    pub write_protected_dir: Option<PathBuf>,
    /// `-workdir <dir>`: custom working directory forwarded to the relaunched app.
    pub workdir: Option<PathBuf>,
    /// `-key <name>`: custom storage key forwarded to the relaunched app.
    pub key: Option<String>,
    /// `-exename <file>`: target executable filename (validated against path separators).
    pub exe_name: String,
}

impl Default for UpdaterArgs {
    fn default() -> Self {
        Self {
            need_update: false,
            finish_process: None,
            autostart: false,
            just_update: false,
            debug: false,
            start_in_tray: false,
            write_protected_dir: None,
            workdir: None,
            key: None,
            exe_name: DEFAULT_EXE_NAME.to_string(),
        }
    }
}

/// Parses `rustgram-updater` command-line arguments (excluding `argv[0]`),
/// matching `wWinMain` argument handling in `_other/updater_win.cpp`.
#[must_use]
pub fn parse_args<I, S>(args: I) -> UpdaterArgs
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let raw: Vec<String> = args.into_iter().map(|s| s.as_ref().to_string()).collect();
    let mut parsed = UpdaterArgs::default();
    let mut process_args_valid = true;

    let mut i = 0usize;
    while i < raw.len() {
        let arg = raw[i].as_str();
        if arg.eq_ignore_ascii_case("-update") {
            parsed.need_update = true;
        } else if arg.eq_ignore_ascii_case("-finishprocess") {
            if parsed.finish_process.is_some() || i + 2 >= raw.len() {
                process_args_valid = false;
            } else {
                let pid = parse_positive_u64(&raw[i + 1]).and_then(|v| u32::try_from(v).ok());
                let created = parse_positive_u64(&raw[i + 2]);
                match (pid, created) {
                    (Some(pid), Some(created)) => {
                        parsed.finish_process = Some((pid, created));
                        i += 2;
                    }
                    _ => {
                        process_args_valid = false;
                    }
                }
            }
        } else if arg.eq_ignore_ascii_case("-autostart") {
            parsed.autostart = true;
        } else if arg.eq_ignore_ascii_case("-justupdate") {
            parsed.just_update = true;
        } else if arg.eq_ignore_ascii_case("-debug") {
            parsed.debug = true;
        } else if arg.eq_ignore_ascii_case("-startintray") {
            parsed.start_in_tray = true;
        } else if arg.eq_ignore_ascii_case("-writeprotected") && i + 1 < raw.len() {
            i += 1;
            parsed.write_protected_dir = Some(PathBuf::from(&raw[i]));
        } else if arg.eq_ignore_ascii_case("-workdir") && i + 1 < raw.len() {
            i += 1;
            parsed.workdir = Some(PathBuf::from(&raw[i]));
        } else if arg.eq_ignore_ascii_case("-key") && i + 1 < raw.len() {
            i += 1;
            parsed.key = Some(raw[i].clone());
        } else if arg.eq_ignore_ascii_case("-exename") && i + 1 < raw.len() {
            i += 1;
            let candidate = &raw[i];
            if !candidate.is_empty() && !candidate.contains('/') && !candidate.contains('\\') {
                parsed.exe_name = candidate.clone();
            } else {
                parsed.exe_name = DEFAULT_EXE_NAME.to_string();
            }
        }
        i += 1;
    }

    if !process_args_valid {
        parsed.finish_process = None;
    }

    parsed
}

fn parse_positive_u64(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value = text.parse::<u64>().ok()?;
    if value == 0 { None } else { Some(value) }
}

/// Builds the relaunch argument list for `rustgram.exe`, prepending `-noupdate`
/// and forwarding `-autostart`, `-debug`, `-startintray`, `-workdir`, and `-key`.
///
/// Mirrors `updater_win.cpp` lines 791-831.
#[must_use]
pub fn build_relaunch_args(args: &UpdaterArgs) -> Vec<String> {
    let mut out = vec!["-noupdate".to_string()];
    if args.autostart {
        out.push("-autostart".to_string());
    }
    if args.debug {
        out.push("-debug".to_string());
    }
    if args.start_in_tray {
        out.push("-startintray".to_string());
    }
    if let Some(ref workdir) = args.workdir {
        out.push("-workdir".to_string());
        out.push(workdir.to_string_lossy().into_owned());
    }
    if let Some(ref key) = args.key {
        out.push("-key".to_string());
        out.push(key.clone());
    }
    out
}

/// Strategy for waiting on the parent application process to exit before replacing binaries.
pub trait ProcessWaiter {
    /// Waits for `pid` (verified by `creation_time`) to exit.
    fn wait_for_exit(&self, pid: u32, creation_time: u64) -> Result<(), UpdateError>;
}

/// Real OS process waiter (Win32 `OpenProcess` / `GetProcessTimes` / `WaitForSingleObject` /
/// `TerminateProcess` fallback on Windows).
#[derive(Debug, Default)]
pub struct SystemProcessWaiter;

impl ProcessWaiter for SystemProcessWaiter {
    #[cfg(windows)]
    fn wait_for_exit(&self, pid: u32, creation_time: u64) -> Result<(), UpdateError> {
        win32::finish_update_process(pid, creation_time)
    }

    #[cfg(not(windows))]
    fn wait_for_exit(&self, _pid: u32, _creation_time: u64) -> Result<(), UpdateError> {
        Ok(())
    }
}

/// Strategy for relaunching the updated application executable.
pub trait AppLauncher {
    /// Spawns `exe` with `args` using `cwd` as working directory.
    fn launch(&self, exe: &Path, args: &[String], cwd: &Path) -> Result<(), UpdateError>;
}

/// Real OS launcher using `std::process::Command`.
#[derive(Debug, Default)]
pub struct SystemAppLauncher;

impl AppLauncher for SystemAppLauncher {
    fn launch(&self, exe: &Path, args: &[String], cwd: &Path) -> Result<(), UpdateError> {
        Command::new(exe)
            .args(args)
            .current_dir(cwd)
            .spawn()
            .map(|_| ())
            .map_err(|e| UpdateError::RelaunchFailed(format!("{}: {e}", exe.display())))
    }
}

/// Summary returned by [`apply_staged_update`] and [`run_updater`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyOutcome {
    /// Whether an update was found and installed.
    pub updated: bool,
    /// Installed version string read from `tdata/version`, if present.
    pub installed_version: Option<String>,
    /// Number of files copied into the installation directory.
    pub files_replaced: usize,
    /// Whether the application was relaunched.
    pub relaunched: bool,
}

/// Returns `true` when `working_dir` holds an update the updater can install: the
/// `tupdates/temp/ready` marker, or the legacy `tupdates/ready` folder (`update()` in
/// `updater_win.cpp`).
#[must_use]
pub fn has_staged_update(working_dir: &Path) -> bool {
    let tupdates = working_dir.join(TUPDATES_DIR_NAME);
    tupdates.join("temp").join("ready").is_file() || tupdates.join("ready").is_dir()
}

/// Where [`resolve_working_dir`] found the working directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkingDirSource {
    /// `-workdir <dir>` on the command line.
    Flag,
    /// The updater's current directory (tdesktop's only source: `update()` opens `tupdates\temp`
    /// relative to it, and the app starts the updater with its working dir as current dir).
    CurrentDir,
    /// The app's default working directory, found because the current directory holds no staged
    /// update. Rescues releases up to 0.2.0-alpha.1, which started the updater with the install
    /// directory as its current directory and without `-workdir`.
    AppDefault,
}

/// The app's default working directory for an executable in `exe_dir`, with the same precedence as
/// `tg_core::launcher::resolve_working_dir` without `-workdir`: `<exe_dir>/RustgramForcePortable`,
/// then `<exe_dir>` when `<exe_dir>/tdata` exists, then `<app_data>/Rustgram`, then `<exe_dir>`.
#[must_use]
pub fn app_default_working_dir(exe_dir: &Path, app_data: Option<&Path>) -> PathBuf {
    let portable = exe_dir.join(crate::PORTABLE_FOLDER_NAME);
    if portable.is_dir() {
        return portable;
    }
    if exe_dir.join("tdata").is_dir() {
        return exe_dir.to_path_buf();
    }
    match app_data {
        Some(base) => base.join(crate::APP_DATA_DIR_NAME),
        None => exe_dir.to_path_buf(),
    }
}

/// Picks the working directory whose `tupdates` the updater installs from.
///
/// `-workdir` wins; otherwise the current directory, like tdesktop. When the current directory
/// holds no staged update, the app's default working directory is used if it holds one (see
/// [`WorkingDirSource::AppDefault`]); the app only ever stages verified packages there.
#[must_use]
pub fn resolve_working_dir(
    workdir_flag: Option<&Path>,
    current_dir: &Path,
    exe_dir: &Path,
    app_data: Option<&Path>,
) -> (PathBuf, WorkingDirSource) {
    if let Some(dir) = workdir_flag {
        return (dir.to_path_buf(), WorkingDirSource::Flag);
    }
    if !has_staged_update(current_dir) {
        let default_dir = app_default_working_dir(exe_dir, app_data);
        if has_staged_update(&default_dir) {
            return (default_dir, WorkingDirSource::AppDefault);
        }
    }
    (current_dir.to_path_buf(), WorkingDirSource::CurrentDir)
}

/// Removes `tupdates/ready`, `tupdates/temp`, and `tupdates` (if empty).
pub fn clear_tupdates(working_dir: &Path) {
    let tupdates = working_dir.join(TUPDATES_DIR_NAME);
    let _ = fs::remove_dir_all(tupdates.join("ready"));
    let _ = fs::remove_dir_all(tupdates.join("temp"));
    let _ = fs::remove_dir(&tupdates);
}

/// Applies a staged update from `<working_dir>/tupdates/temp` (or legacy `tupdates/ready`)
/// into `install_dir`, backing up overwritten files and rolling back on any copy failure.
///
/// Direct port of `update()` in `Telegram/SourceFiles/_other/updater_win.cpp` extended with
/// transactional rollback when file replacement fails midway.
pub fn apply_staged_update(
    working_dir: &Path,
    install_dir: &Path,
    exe_name: &str,
    updater_name: &str,
    write_protected: bool,
) -> Result<ApplyOutcome, UpdateError> {
    apply_staged_update_with_policy(
        working_dir,
        install_dir,
        exe_name,
        updater_name,
        write_protected,
        ApplyRetryPolicy::default(),
    )
}

#[derive(Debug)]
enum RollbackEntry {
    CreatedDir(PathBuf),
    CreatedFile(PathBuf),
    ReplacedFile { target: PathBuf, backup: PathBuf },
}

fn perform_rollback(entries: &[RollbackEntry]) -> bool {
    let mut all_ok = true;
    for entry in entries.iter().rev() {
        match entry {
            RollbackEntry::ReplacedFile { target, backup } => {
                if fs::copy(backup, target).is_err() {
                    all_ok = false;
                } else {
                    let _ = fs::remove_file(backup);
                }
            }
            RollbackEntry::CreatedFile(target) => {
                if target.exists() && fs::remove_file(target).is_err() {
                    all_ok = false;
                }
            }
            RollbackEntry::CreatedDir(dir) => {
                if dir.is_dir() && fs::remove_dir(dir).is_err() {
                    all_ok = false;
                }
            }
        }
    }
    all_ok
}

/// Applies a staged update with a custom [`ApplyRetryPolicy`] and transactional rollback.
pub fn apply_staged_update_with_policy(
    working_dir: &Path,
    install_dir: &Path,
    exe_name: &str,
    updater_name: &str,
    write_protected: bool,
    policy: ApplyRetryPolicy,
) -> Result<ApplyOutcome, UpdateError> {
    let temp_dir = working_dir.join(TUPDATES_DIR_NAME).join("temp");
    let ready_marker = temp_dir.join("ready");
    let legacy_dir = working_dir.join(TUPDATES_DIR_NAME).join("ready");

    let upd_dir = if ready_marker.is_file() {
        temp_dir.clone()
    } else if legacy_dir.is_dir() {
        legacy_dir
    } else {
        return Ok(ApplyOutcome {
            updated: false,
            installed_version: None,
            files_replaced: 0,
            relaunched: false,
        });
    };

    let tdata_dir = upd_dir.join("tdata");
    let installed_version = read_version_file(&tdata_dir.join("version"))
        .ok()
        .map(|(_, s)| s);

    let mut dirs = VecDeque::new();
    dirs.push_back(upd_dir.clone());
    let mut force_dirs: Vec<PathBuf> = Vec::new();
    let mut copy_plan: Vec<(PathBuf, PathBuf)> = Vec::new();

    while let Some(dir) = dirs.pop_front() {
        let rel_dir = dir.strip_prefix(&upd_dir).unwrap_or(Path::new(""));
        if !rel_dir.as_os_str().is_empty() {
            force_dirs.push(install_dir.join(rel_dir));
        }

        let entries = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ApplyOutcome {
                    updated: false,
                    installed_version: None,
                    files_replaced: 0,
                    relaunched: false,
                });
            }
            Err(e) => {
                clear_tupdates(working_dir);
                return Err(UpdateError::Io(e));
            }
        };

        for entry_res in entries {
            let entry = entry_res?;
            let path = entry.path();
            let file_type = entry.file_type()?;

            // Skip `tdata` subtree (metadata only), matching updater_win.cpp line 338.
            if path == tdata_dir || path.starts_with(&tdata_dir) {
                continue;
            }

            if file_type.is_dir() {
                dirs.push_back(path);
                continue;
            }

            // Skip the `ready` marker file itself.
            if path == ready_marker {
                continue;
            }

            let rel = path
                .strip_prefix(&upd_dir)
                .map_err(|_| UpdateError::UnsafePath(path.display().to_string()))?;

            // Guard against a package trying to overwrite the running updater binary.
            let is_top_level = rel.components().count() == 1;
            let file_name = rel.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if is_top_level && file_name.eq_ignore_ascii_case(updater_name) {
                clear_tupdates(working_dir);
                return Err(UpdateError::SelfOverwriteBlocked(file_name.to_string()));
            }

            let dest = if is_top_level
                && file_name.eq_ignore_ascii_case(DEFAULT_EXE_NAME)
                && !exe_name.eq_ignore_ascii_case(DEFAULT_EXE_NAME)
            {
                install_dir.join(exe_name)
            } else {
                install_dir.join(rel)
            };

            copy_plan.push((path, dest));
        }
    }

    // Sort deterministically so file replacement and rollback order are reproducible.
    force_dirs.sort();
    copy_plan.sort_by(|a, b| a.1.cmp(&b.1));

    fs::create_dir_all(install_dir)?;

    let rollback_dir = working_dir.join(TUPDATES_DIR_NAME).join(".rollback");
    if rollback_dir.exists() {
        let _ = fs::remove_dir_all(&rollback_dir);
    }
    fs::create_dir_all(&rollback_dir)?;

    let mut rollback_log: Vec<RollbackEntry> = Vec::new();
    for dir in &force_dirs {
        if !dir.exists() {
            if let Err(err) = fs::create_dir_all(dir) {
                let rolled_back = perform_rollback(&rollback_log);
                let _ = fs::remove_dir_all(&rollback_dir);
                return Err(UpdateError::ApplyFailed {
                    reason: format!("failed to create directory '{}': {err}", dir.display()),
                    rolled_back,
                });
            }
            rollback_log.push(RollbackEntry::CreatedDir(dir.clone()));
        }
    }

    for (idx, (from, to)) in copy_plan.iter().enumerate() {
        if let Some(parent) = to.parent()
            && !parent.exists()
        {
            if let Err(err) = fs::create_dir_all(parent) {
                let rolled_back = perform_rollback(&rollback_log);
                let _ = fs::remove_dir_all(&rollback_dir);
                return Err(UpdateError::ApplyFailed {
                    reason: format!("failed to create parent '{}': {err}", parent.display()),
                    rolled_back,
                });
            }
            rollback_log.push(RollbackEntry::CreatedDir(parent.to_path_buf()));
        }

        if to.is_file() {
            let backup_path = rollback_dir.join(format!("{idx:06}.bak"));
            if let Err(err) = fs::copy(to, &backup_path) {
                let rolled_back = perform_rollback(&rollback_log);
                let _ = fs::remove_dir_all(&rollback_dir);
                return Err(UpdateError::ApplyFailed {
                    reason: format!("failed to back up '{}': {err}", to.display()),
                    rolled_back,
                });
            }
            rollback_log.push(RollbackEntry::ReplacedFile {
                target: to.clone(),
                backup: backup_path,
            });
        } else if !to.exists() {
            rollback_log.push(RollbackEntry::CreatedFile(to.clone()));
        }

        if let Err(err) = copy_with_retry(from, to, policy.max_retries, policy.retry_delay) {
            tracing::error!(%err, target = %to.display(), "Update copy failed; rolling back");
            let rolled_back = perform_rollback(&rollback_log);
            let _ = fs::remove_dir_all(&rollback_dir);
            return Err(UpdateError::ApplyFailed {
                reason: format!(
                    "failed to copy '{}' -> '{}': {err}",
                    from.display(),
                    to.display()
                ),
                rolled_back,
            });
        }
    }

    let _ = fs::remove_dir_all(&rollback_dir);

    if write_protected {
        let _ = fs::remove_file(temp_dir.join("tdata").join("version"));
        let _ = fs::remove_file(
            working_dir
                .join(TUPDATES_DIR_NAME)
                .join("ready")
                .join("tdata")
                .join("version"),
        );
    } else {
        clear_tupdates(working_dir);
    }

    Ok(ApplyOutcome {
        updated: true,
        installed_version,
        files_replaced: copy_plan.len(),
        relaunched: false,
    })
}

fn copy_with_retry(
    from: &Path,
    to: &Path,
    max_retries: u32,
    delay: Duration,
) -> Result<(), UpdateError> {
    let mut last_err = None;
    for _ in 0..max_retries.max(1) {
        match fs::copy(from, to) {
            Ok(_) => return Ok(()),
            Err(err) => {
                #[cfg(windows)]
                if err.raw_os_error() == Some(win32::ERROR_SHARING_VIOLATION_CODE)
                    && win32::replace_shared_file(from, to)
                {
                    return Ok(());
                }
                last_err = Some(err);
                thread::sleep(delay);
            }
        }
    }
    Err(UpdateError::Io(last_err.unwrap_or_else(|| {
        std::io::Error::other("copy failed without OS error")
    })))
}

/// Orchestrates the full `rustgram-updater` lifecycle:
/// 1. Waits for `-finishprocess` if given.
/// 2. Applies staged update if `-update` is set.
/// 3. Updates the Windows Uninstall registry version if an update succeeded.
/// 4. Relaunches `rustgram.exe` with `-noupdate` + forwarded flags unless `-justupdate` is set.
pub fn run_updater(
    args: &UpdaterArgs,
    working_dir: &Path,
    default_install_dir: &Path,
    waiter: &dyn ProcessWaiter,
    launcher: &dyn AppLauncher,
) -> Result<ApplyOutcome, UpdateError> {
    run_updater_with_policy(
        args,
        working_dir,
        default_install_dir,
        waiter,
        launcher,
        ApplyRetryPolicy::default(),
    )
}

/// Same as [`run_updater`] with an explicit [`ApplyRetryPolicy`].
pub fn run_updater_with_policy(
    args: &UpdaterArgs,
    working_dir: &Path,
    default_install_dir: &Path,
    waiter: &dyn ProcessWaiter,
    launcher: &dyn AppLauncher,
    policy: ApplyRetryPolicy,
) -> Result<ApplyOutcome, UpdateError> {
    let install_dir = args
        .write_protected_dir
        .as_deref()
        .unwrap_or(default_install_dir);

    if args.need_update
        && let Some((pid, created)) = args.finish_process
        && let Err(err) = waiter.wait_for_exit(pid, created)
    {
        tracing::warn!(%err, pid, "Could not finish waiting for parent process; continuing with copy retries");
    }

    let mut outcome = if args.need_update {
        apply_staged_update_with_policy(
            working_dir,
            install_dir,
            &args.exe_name,
            UPDATER_EXE_NAME,
            args.write_protected_dir.is_some(),
            policy,
        )?
    } else {
        ApplyOutcome {
            updated: false,
            installed_version: None,
            files_replaced: 0,
            relaunched: false,
        }
    };

    if outcome.updated
        && let Some(ref version_str) = outcome.installed_version
    {
        update_uninstall_registry(version_str);
    }

    if !args.just_update {
        let target_exe = install_dir.join(&args.exe_name);
        let relaunch_args = build_relaunch_args(args);
        launcher.launch(&target_exe, &relaunch_args, install_dir)?;
        outcome.relaunched = true;
    }

    Ok(outcome)
}

/// Updates `DisplayVersion` and `InstallDate` in the per-user Inno Setup Uninstall registry key
/// on Windows (no-op on other platforms or if Rustgram was not installed via the Inno Setup installer).
///
/// Port of `updateRegistry()` in `Telegram/SourceFiles/_other/updater_win.cpp`.
pub fn update_uninstall_registry(version_str: &str) {
    #[cfg(windows)]
    win32::update_uninstall_registry_win(version_str);
    #[cfg(not(windows))]
    let _ = version_str;
}

#[cfg(windows)]
mod win32 {
    use std::ffi::OsStr;
    use std::fs;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, ERROR_PROCESS_ABORTED, ERROR_SHARING_VIOLATION,
        ERROR_SUCCESS, FILETIME, GetLastError, HANDLE, SYSTEMTIME, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_DELAY_UNTIL_REBOOT, MOVEFILE_REPLACE_EXISTING, MoveFileExW,
    };
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RegCloseKey,
        RegOpenKeyExW, RegSetValueExW,
    };
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
    };

    use super::PROCESS_EXIT_TIMEOUT_MS;
    use crate::{APP_ID_GUID, UpdateError};

    pub(super) const ERROR_SHARING_VIOLATION_CODE: i32 = ERROR_SHARING_VIOLATION as i32;

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if self.0 != 0 {
                // SAFETY: `self.0` is a valid Win32 handle returned by `OpenProcess`.
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    fn to_wide_null(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn filetime_to_u64(ft: FILETIME) -> u64 {
        ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64)
    }

    fn query_creation_time(handle: HANDLE) -> Result<u64, u32> {
        let mut created = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut exited = created;
        let mut kernel = created;
        let mut user = created;
        // SAFETY: `handle` is an open process handle with `PROCESS_QUERY_LIMITED_INFORMATION`
        // and all four `FILETIME` pointers point to valid stack allocations.
        let ok =
            unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) };
        if ok == 0 {
            // SAFETY: Calling `GetLastError` immediately after a failing Win32 call.
            return Err(unsafe { GetLastError() });
        }
        Ok(filetime_to_u64(created))
    }

    /// Port of `FinishUpdateProcess` in `Telegram/SourceFiles/_other/updater_win.cpp`.
    pub(super) fn finish_update_process(
        pid: u32,
        expected_created: u64,
    ) -> Result<(), UpdateError> {
        // SAFETY: `GetCurrentProcessId` takes no parameters and always succeeds.
        let current_pid = unsafe { GetCurrentProcessId() };
        if pid == 0 || expected_created == 0 || pid == current_pid {
            return Err(UpdateError::ProcessWait {
                pid,
                reason: "invalid process id or creation timestamp".into(),
            });
        }

        // SAFETY: Opening the parent process handle with read/synchronize rights.
        let raw = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        if raw == 0 {
            // SAFETY: Calling `GetLastError` immediately after `OpenProcess` returned null.
            let err = unsafe { GetLastError() };
            if err == ERROR_INVALID_PARAMETER {
                // Process already exited.
                return Ok(());
            }
            return Err(UpdateError::ProcessWait {
                pid,
                reason: format!("OpenProcess failed with error {err}"),
            });
        }
        let process = OwnedHandle(raw);

        let actual_created =
            query_creation_time(process.0).map_err(|err| UpdateError::ProcessWait {
                pid,
                reason: format!("GetProcessTimes failed with error {err}"),
            })?;
        if actual_created != expected_created {
            // PID was already reused by a different process.
            return Ok(());
        }

        // SAFETY: `process.0` is a valid process handle with `PROCESS_SYNCHRONIZE`.
        let mut wait = unsafe { WaitForSingleObject(process.0, PROCESS_EXIT_TIMEOUT_MS) };
        if wait == WAIT_TIMEOUT {
            // SAFETY: Requesting terminate permission on the verified parent PID.
            let term_raw = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                    0,
                    pid,
                )
            };
            if term_raw == 0 {
                // SAFETY: Calling `GetLastError` immediately after `OpenProcess` failed.
                let err = unsafe { GetLastError() };
                return Err(UpdateError::ProcessWait {
                    pid,
                    reason: format!("OpenProcess(TERMINATE) failed with error {err}"),
                });
            }
            let term_handle = OwnedHandle(term_raw);
            let verified_created =
                query_creation_time(term_handle.0).map_err(|err| UpdateError::ProcessWait {
                    pid,
                    reason: format!("GetProcessTimes(terminate) failed with error {err}"),
                })?;
            if verified_created != expected_created {
                return Ok(());
            }
            // SAFETY: `term_handle.0` was verified to match `expected_created` and has `PROCESS_TERMINATE`.
            let _ = unsafe { TerminateProcess(term_handle.0, ERROR_PROCESS_ABORTED) };
            // SAFETY: Waiting on the original synchronized handle after termination.
            wait = unsafe { WaitForSingleObject(process.0, PROCESS_EXIT_TIMEOUT_MS) };
        }

        if wait == WAIT_OBJECT_0 {
            Ok(())
        } else {
            Err(UpdateError::ProcessWait {
                pid,
                reason: format!("WaitForSingleObject returned {wait}"),
            })
        }
    }

    /// Port of `ReplaceSharedFile` in `Telegram/SourceFiles/_other/updater_win.cpp`:
    /// copies to `<to>.new`, moves locked `<to>` aside to `<to>.old`, moves `<to>.new` into place,
    /// and schedules `<to>.old` for deletion on next reboot.
    pub(super) fn replace_shared_file(from: &Path, to: &Path) -> bool {
        let mut fresh = to.as_os_str().to_os_string();
        fresh.push(".new");
        let mut aside = to.as_os_str().to_os_string();
        aside.push(".old");

        let fresh_path = Path::new(&fresh);
        let aside_path = Path::new(&aside);

        let _ = fs::remove_file(fresh_path);
        if fs::copy(from, fresh_path).is_err() {
            return false;
        }
        let _ = fs::remove_file(aside_path);

        let to_w = to_wide_null(to.as_os_str());
        let fresh_w = to_wide_null(fresh_path.as_os_str());
        let aside_w = to_wide_null(aside_path.as_os_str());

        // SAFETY: All wide string buffers are null-terminated and live across the FFI calls.
        unsafe {
            if MoveFileExW(to_w.as_ptr(), aside_w.as_ptr(), MOVEFILE_REPLACE_EXISTING) == 0 {
                let _ = fs::remove_file(fresh_path);
                return false;
            }
            if MoveFileExW(fresh_w.as_ptr(), to_w.as_ptr(), 0) == 0 {
                let _ = MoveFileExW(aside_w.as_ptr(), to_w.as_ptr(), 0);
                let _ = fs::remove_file(fresh_path);
                return false;
            }
            let _ = MoveFileExW(
                aside_w.as_ptr(),
                std::ptr::null(),
                MOVEFILE_DELAY_UNTIL_REBOOT,
            );
        }
        true
    }

    /// Updates `DisplayVersion` and `InstallDate` in the Inno Setup Uninstall key if present.
    pub(super) fn update_uninstall_registry_win(version_str: &str) {
        let subkey =
            format!("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{APP_ID_GUID}_is1");
        let subkey_w = to_wide_null(OsStr::new(&subkey));
        let mut hkey: HKEY = 0;

        // SAFETY: `subkey_w` is null-terminated and `hkey` is a valid out-pointer.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                subkey_w.as_ptr(),
                0,
                KEY_QUERY_VALUE | KEY_SET_VALUE,
                &mut hkey,
            )
        };
        if status != ERROR_SUCCESS || hkey == 0 {
            return;
        }

        let set_sz = |name: &str, value: &str| {
            let name_w = to_wide_null(OsStr::new(name));
            let val_w = to_wide_null(OsStr::new(value));
            let byte_len = (val_w.len() * 2) as u32;
            // SAFETY: `hkey` is open with `KEY_SET_VALUE`; both wide buffers are null-terminated.
            unsafe {
                let _ = RegSetValueExW(
                    hkey,
                    name_w.as_ptr(),
                    0,
                    REG_SZ,
                    val_w.as_ptr().cast::<u8>(),
                    byte_len,
                );
            }
        };

        let mut st = SYSTEMTIME {
            wYear: 0,
            wMonth: 0,
            wDayOfWeek: 0,
            wDay: 0,
            wHour: 0,
            wMinute: 0,
            wSecond: 0,
            wMilliseconds: 0,
        };
        // SAFETY: `st` is a valid stack `SYSTEMTIME`.
        unsafe {
            GetLocalTime(&mut st);
        }
        let date_str = format!("{:04}{:02}{:02}", st.wYear, st.wMonth, st.wDay);

        set_sz("DisplayVersion", version_str);
        set_sz("InstallDate", &date_str);

        // SAFETY: `hkey` was opened by `RegOpenKeyExW`.
        unsafe {
            let _ = RegCloseKey(hkey);
        }
    }
}

#[cfg(test)]
mod working_dir_tests {
    use super::*;

    fn stage(dir: &Path) {
        let temp = dir.join(TUPDATES_DIR_NAME).join("temp");
        fs::create_dir_all(&temp).expect("temp");
        fs::write(temp.join("ready"), b"1").expect("ready");
    }

    struct Dirs {
        _root: tempfile::TempDir,
        cwd: PathBuf,
        exe_dir: PathBuf,
        app_data: PathBuf,
    }

    fn dirs() -> Dirs {
        let root = tempfile::TempDir::new().expect("temp");
        let cwd = root.path().join("cwd");
        let exe_dir = root.path().join("install");
        let app_data = root.path().join("appdata");
        for d in [&cwd, &exe_dir, &app_data] {
            fs::create_dir_all(d).expect("dir");
        }
        Dirs {
            _root: root,
            cwd,
            exe_dir,
            app_data,
        }
    }

    #[test]
    fn workdir_flag_wins() {
        let d = dirs();
        stage(&d.app_data.join(crate::APP_DATA_DIR_NAME));
        let flag = d.cwd.join("custom");
        assert_eq!(
            resolve_working_dir(Some(&flag), &d.cwd, &d.exe_dir, Some(&d.app_data)),
            (flag.clone(), WorkingDirSource::Flag)
        );
    }

    #[test]
    fn current_dir_with_staged_update_wins_over_app_default() {
        let d = dirs();
        stage(&d.cwd);
        stage(&d.app_data.join(crate::APP_DATA_DIR_NAME));
        assert_eq!(
            resolve_working_dir(None, &d.cwd, &d.exe_dir, Some(&d.app_data)),
            (d.cwd.clone(), WorkingDirSource::CurrentDir)
        );
    }

    #[test]
    fn falls_back_to_app_data_when_current_dir_has_nothing() {
        let d = dirs();
        let default_dir = d.app_data.join(crate::APP_DATA_DIR_NAME);
        stage(&default_dir);
        assert_eq!(
            resolve_working_dir(None, &d.exe_dir, &d.exe_dir, Some(&d.app_data)),
            (default_dir, WorkingDirSource::AppDefault)
        );
    }

    #[test]
    fn falls_back_to_portable_marker_folder() {
        let d = dirs();
        let portable = d.exe_dir.join(crate::PORTABLE_FOLDER_NAME);
        stage(&portable);
        stage(&d.app_data.join(crate::APP_DATA_DIR_NAME));
        assert_eq!(
            resolve_working_dir(None, &d.exe_dir, &d.exe_dir, Some(&d.app_data)),
            (portable, WorkingDirSource::AppDefault)
        );
    }

    #[test]
    fn adjacent_tdata_means_the_exe_dir_is_the_working_dir() {
        let d = dirs();
        fs::create_dir_all(d.exe_dir.join("tdata")).expect("tdata");
        // Another profile's staged update in %APPDATA% is not this install's.
        stage(&d.app_data.join(crate::APP_DATA_DIR_NAME));
        assert_eq!(
            resolve_working_dir(None, &d.cwd, &d.exe_dir, Some(&d.app_data)),
            (d.cwd.clone(), WorkingDirSource::CurrentDir)
        );
    }

    #[test]
    fn nothing_staged_anywhere_keeps_current_dir() {
        let d = dirs();
        assert_eq!(
            resolve_working_dir(None, &d.cwd, &d.exe_dir, Some(&d.app_data)),
            (d.cwd.clone(), WorkingDirSource::CurrentDir)
        );
        assert_eq!(
            resolve_working_dir(None, &d.cwd, &d.exe_dir, None),
            (d.cwd.clone(), WorkingDirSource::CurrentDir)
        );
    }

    #[test]
    fn legacy_ready_folder_counts_as_staged() {
        let d = dirs();
        fs::create_dir_all(d.cwd.join(TUPDATES_DIR_NAME).join("ready")).expect("legacy");
        assert!(has_staged_update(&d.cwd));
        assert!(!has_staged_update(&d.exe_dir));
    }
}
