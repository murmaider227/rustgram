//! Port of `core/launcher.h`, `core/launcher.cpp`, and `platform/win/launcher_win.cpp`.
//!
//! Handles command-line switch parsing, launch mode selection, working directory resolution
//! (standard `%APPDATA%\Rustgram`, portable mode via `RustgramForcePortable` or adjacent `tdata`,
//! `-workdir` override, and `-key` profile suffix), auxiliary `tdata` state flags, instance
//! hashing, and self-relaunch argument building.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use md5::{Digest, Md5};
use rand::Rng;
use thiserror::Error;

use crate::globals::{GlobalConfig, LaunchMode, SCALE_AUTO, SCALE_MAX, SCALE_MIN};
use crate::version::{APP_NAME, PORTABLE_FOLDER_NAME};

/// Maximum character length accepted for any single CLI value (`launcher.cpp:623`).
pub const MAX_ARG_CHARS: usize = 8192;

/// Errors returned by launcher operations.
#[derive(Debug, Error)]
pub enum LauncherError {
    /// Filesystem I/O error while creating or reading working-dir files.
    #[error("launcher I/O error: {0}")]
    Io(#[from] io::Error),
    /// Could not determine current executable path for relaunch.
    #[error("executable path is empty; cannot relaunch")]
    MissingExecutablePath,
}

/// Action requested when relaunching the process (`Core::Launcher::UpdaterLaunch`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelaunchAction {
    /// Relaunch the application normally (passes `-noupdate` unless retrying after update).
    JustRelaunch,
    /// Perform an update launch.
    PerformUpdate,
}

/// Parsed command-line arguments matching tdesktop's `Launcher::processArguments()`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedArgs {
    /// `-debug` (or implied by `-testagent`).
    pub debug: bool,
    /// `-testmode` (connect to Telegram test datacenters).
    pub test_mode: bool,
    /// `-testagent`.
    pub test_agent: bool,
    /// `-key <name>` sanitized to lowercase `[a-z0-9\-_]`.
    pub key: String,
    /// Selected launch mode (`-autostart` > `-fixprevious` > `-cleanup` > `Normal`).
    pub launch_mode: LaunchMode,
    /// `-noupdate`.
    pub no_update: bool,
    /// `-tosettings`.
    pub to_settings: bool,
    /// `-startintray`.
    pub start_in_tray: bool,
    /// `-workdir <dir>`.
    pub workdir: Option<PathBuf>,
    /// `-many` (allow multiple instances).
    pub many: bool,
    /// `-externalinterface`.
    pub external_interface: bool,
    /// `-scale <N>` (None if flag absent; `Some(SCALE_AUTO)` if out of `50..=300` or invalid).
    pub scale: Option<i32>,
    /// `-quit`.
    pub quit: bool,
    /// Trailing `tg://` / `tonsite://` (or other URI-scheme) startup URLs.
    pub start_urls: Vec<String>,
    /// Local file paths passed via `-sendpath` or as trailing arguments.
    pub send_paths: Vec<PathBuf>,
    /// All trailing values in order (both URLs and paths).
    pub raw_trailing: Vec<String>,
}

/// How the application's working directory was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkingDirOrigin {
    /// Explicitly overridden via `-workdir <dir>`.
    CustomFlag,
    /// Portable folder `<exe_dir>/RustgramForcePortable` exists.
    PortableMarker,
    /// Adjacent `<exe_dir>/tdata` folder exists next to the executable.
    PortableAdjacentTdata,
    /// Standard OS per-user app data folder (`%APPDATA%\Rustgram` on Windows).
    AppData,
    /// Fallback to executable directory when no app data directory is available.
    ExecutableDirFallback,
}

/// Result of resolving the application's working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWorkingDir {
    /// Resolved absolute (when possible) working directory path.
    pub path: PathBuf,
    /// How this working directory was chosen.
    pub origin: WorkingDirOrigin,
    /// Whether this counts as a custom working directory (`-workdir`).
    pub is_custom: bool,
    /// Whether portable mode is active (`RustgramForcePortable` or adjacent `tdata`).
    pub is_portable: bool,
}

/// Environment inputs for working directory resolution (injectable for tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkDirEnv {
    /// Directory containing the running executable.
    pub exe_dir: PathBuf,
    /// Base OS user data directory (e.g. `%APPDATA%` on Windows).
    pub app_data_dir: Option<PathBuf>,
    /// Initial current working directory when the process launched.
    pub current_dir: PathBuf,
}

impl WorkDirEnv {
    /// Captures the live process environment (`current_exe`, `APPDATA`, `current_dir`).
    #[must_use]
    pub fn from_process() -> Self {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| current_dir.clone());
        let app_data_dir = default_app_data_base_dir();
        Self {
            exe_dir,
            app_data_dir,
            current_dir,
        }
    }
}

fn default_app_data_base_dir() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .map(|h| PathBuf::from(h).join("Library").join("Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|v| !v.is_empty())
                    .map(|h| PathBuf::from(h).join(".local").join("share"))
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyFormat {
    NoValues,
    OneValue,
    AllLeftValues,
}

/// Truncates a string to at most `MAX_ARG_CHARS` Unicode scalar values.
fn truncate_arg(arg: &str) -> String {
    arg.chars().take(MAX_ARG_CHARS).collect()
}

/// Sanitizes `-key` value identically to `launcher.cpp:642-649` (`toLower().replace(/[^a-z0-9\-_]/, "")`).
#[must_use]
pub fn sanitize_key_name(raw: &str) -> String {
    raw.to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-' || *c == '_')
        .collect()
}

/// Returns `true` if `s` looks like a URI with an explicit scheme (`tg://`, `tonsite://`, `http://`, `https://`, `file://`).
#[must_use]
pub fn is_url_argument(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.starts_with("tg://")
        || lower.starts_with("tonsite://")
        || lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("file://")
}

/// Parses command-line arguments (with `argv[0]` as the executable path, skipped just like
/// `Launcher::processArguments()` in `core/launcher.cpp`).
#[must_use]
pub fn parse_arguments<I, S>(args: I) -> ParsedArgs
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut parsed = ParsedArgs::default();
    let mut format = KeyFormat::NoValues;
    let mut current_key = String::new();
    let mut has_autostart = false;
    let mut has_fixprevious = false;
    let mut has_cleanup = false;

    for (idx, raw_arg) in args.into_iter().enumerate() {
        if idx == 0 {
            continue;
        }
        let arg = raw_arg.as_ref();
        match format {
            KeyFormat::OneValue => {
                let value = truncate_arg(arg);
                match current_key.as_str() {
                    "-key" => parsed.key = sanitize_key_name(&value),
                    "-workdir" if !value.is_empty() => {
                        parsed.workdir = Some(PathBuf::from(value));
                    }
                    "-scale" => {
                        let scale = value
                            .trim()
                            .parse::<i32>()
                            .ok()
                            .filter(|&v| (SCALE_MIN..=SCALE_MAX).contains(&v))
                            .unwrap_or(SCALE_AUTO);
                        parsed.scale = Some(scale);
                    }
                    "-sendpath" if !value.is_empty() => {
                        parsed.send_paths.push(PathBuf::from(&value));
                        parsed.raw_trailing.push(value);
                    }
                    _ => {}
                }
                format = KeyFormat::NoValues;
            }
            KeyFormat::AllLeftValues => {
                push_trailing_arg(&mut parsed, &truncate_arg(arg));
            }
            KeyFormat::NoValues => match arg {
                "-debug" => parsed.debug = true,
                "-testmode" => parsed.test_mode = true,
                "-testagent" => {
                    parsed.test_agent = true;
                    parsed.debug = true;
                }
                "-autostart" => has_autostart = true,
                "-fixprevious" => has_fixprevious = true,
                "-cleanup" => has_cleanup = true,
                "-noupdate" => parsed.no_update = true,
                "-tosettings" => parsed.to_settings = true,
                "-startintray" => parsed.start_in_tray = true,
                "-many" => parsed.many = true,
                "-externalinterface" => parsed.external_interface = true,
                "-quit" => parsed.quit = true,
                "-key" | "-workdir" | "-scale" | "-sendpath" => {
                    current_key.clear();
                    current_key.push_str(arg);
                    format = KeyFormat::OneValue;
                }
                "--" => {
                    format = KeyFormat::AllLeftValues;
                }
                _ => {
                    push_trailing_arg(&mut parsed, &truncate_arg(arg));
                }
            },
        }
    }

    parsed.launch_mode = if has_autostart {
        LaunchMode::AutoStart
    } else if has_fixprevious {
        LaunchMode::FixPrevious
    } else if has_cleanup {
        LaunchMode::Cleanup
    } else {
        LaunchMode::Normal
    };

    parsed
}

fn push_trailing_arg(parsed: &mut ParsedArgs, value: &str) {
    if value.is_empty() {
        return;
    }
    parsed.raw_trailing.push(value.to_owned());
    if is_url_argument(value) {
        parsed.start_urls.push(value.to_owned());
    } else {
        parsed.send_paths.push(PathBuf::from(value));
    }
}

/// Resolves the Rustgram working directory according to `-workdir`, portable markers, and `%APPDATA%`.
///
/// Precedence:
/// 1. `-workdir <dir>` override (relative paths resolved against `env.current_dir`).
/// 2. `<exe_dir>/RustgramForcePortable` if that directory exists (portable mode).
/// 3. `<exe_dir>/tdata` if that directory exists next to the exe (portable mode).
/// 4. `<app_data_dir>/Rustgram` (e.g. `%APPDATA%\Rustgram`).
/// 5. Fallback to `<exe_dir>` when no app-data directory is set in the environment.
#[must_use]
pub fn resolve_working_dir(args: &ParsedArgs, env: &WorkDirEnv) -> ResolvedWorkingDir {
    if let Some(custom) = &args.workdir {
        let resolved = if custom.is_absolute() {
            custom.clone()
        } else {
            env.current_dir.join(custom)
        };
        return ResolvedWorkingDir {
            path: normalize_path(resolved),
            origin: WorkingDirOrigin::CustomFlag,
            is_custom: true,
            is_portable: false,
        };
    }

    let portable_dir = env.exe_dir.join(PORTABLE_FOLDER_NAME);
    if portable_dir.is_dir() {
        return ResolvedWorkingDir {
            path: normalize_path(portable_dir),
            origin: WorkingDirOrigin::PortableMarker,
            is_custom: false,
            is_portable: true,
        };
    }

    let adjacent_tdata = env.exe_dir.join("tdata");
    if adjacent_tdata.is_dir() {
        return ResolvedWorkingDir {
            path: normalize_path(env.exe_dir.clone()),
            origin: WorkingDirOrigin::PortableAdjacentTdata,
            is_custom: false,
            is_portable: true,
        };
    }

    if let Some(app_data) = &env.app_data_dir {
        return ResolvedWorkingDir {
            path: normalize_path(app_data.join(APP_NAME)),
            origin: WorkingDirOrigin::AppData,
            is_custom: false,
            is_portable: false,
        };
    }

    ResolvedWorkingDir {
        path: normalize_path(env.exe_dir.clone()),
        origin: WorkingDirOrigin::ExecutableDirFallback,
        is_custom: false,
        is_portable: false,
    }
}

/// Normalizes lexical `.` and `..` components without requiring the path to exist on disk.
fn normalize_path(path: PathBuf) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(comp.as_os_str());
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Applies parsed CLI arguments and resolved working directory to a [`GlobalConfig`].
pub fn apply_to_config(
    args: &ParsedArgs,
    resolved: &ResolvedWorkingDir,
    exe_path: Option<&Path>,
    config: &mut GlobalConfig,
) {
    if let Some(exe) = exe_path {
        if let Some(parent) = exe.parent() {
            config.exe_dir = parent.to_path_buf();
        }
        if let Some(name) = exe.file_name().and_then(|n| n.to_str()) {
            config.exe_name = name.to_owned();
        }
    }
    config.debug_mode = args.debug;
    config.test_mode = args.test_mode;
    config.test_agent = args.test_agent;
    config.key_file = args.key.clone();
    config.launch_mode = args.launch_mode;
    config.no_start_update = args.no_update;
    config.start_to_settings = args.to_settings;
    config.start_in_tray = args.start_in_tray;
    config.many_instance = args.many;
    config.external_interface = args.external_interface;
    config.quit = args.quit;
    config.working_dir = resolved.path.clone();
    config.custom_working_dir = resolved.is_custom;
    config.start_urls = args.start_urls.clone();
    config.send_paths = args.send_paths.clone();
    if let Some(scale) = args.scale {
        config.config_scale = scale;
        config.validate_scale();
    }
}

/// Computes the 32-character lowercase MD5 hex instance hash (`Core::Launcher::instanceHash`).
///
/// Uses the working directory path when `-workdir` is set (and appends `:<key>` when a `-key`
/// multi-instance suffix is active), otherwise hashes the executable path.
#[must_use]
pub fn instance_hash(config: &GlobalConfig) -> String {
    let mut hasher = Md5::new();
    if config.custom_working_dir {
        hasher.update(config.working_dir.to_string_lossy().as_bytes());
    } else {
        let exe_full = config.exe_dir.join(&config.exe_name);
        hasher.update(exe_full.to_string_lossy().as_bytes());
    }
    if !config.key_file.is_empty() {
        hasher.update(b":");
        hasher.update(config.key_file.as_bytes());
    }
    let digest = hasher.finalize();
    bytes_to_lower_hex(&digest)
}

/// Computes the 32-character lowercase MD5 hex hash of the working directory (and `-key` suffix
/// if specified), used to derive the single-instance IPC server name (`Sandbox::start`).
#[must_use]
pub fn working_dir_hash(working_dir: &Path, key_file: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(working_dir.to_string_lossy().as_bytes());
    if !key_file.is_empty() {
        hasher.update(b":");
        hasher.update(key_file.as_bytes());
    }
    let digest = hasher.finalize();
    bytes_to_lower_hex(&digest)
}

fn bytes_to_lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

/// Ensures `<working_dir>/tdata` exists on disk.
pub fn ensure_working_dir(working_dir: &Path) -> Result<(), LauncherError> {
    fs::create_dir_all(working_dir.join("tdata"))?;
    Ok(())
}

/// Path to the debug-mode flag file (`tdata/withdebug`).
#[must_use]
pub fn debug_mode_setting_path(working_dir: &Path) -> PathBuf {
    working_dir.join("tdata").join("withdebug")
}

/// Persists debug logging preference (`Launcher::writeDebugModeSetting`).
pub fn write_debug_mode_setting(working_dir: &Path, enabled: bool) -> Result<(), LauncherError> {
    ensure_working_dir(working_dir)?;
    fs::write(
        debug_mode_setting_path(working_dir),
        if enabled { b"1" } else { b"0" },
    )?;
    Ok(())
}

/// Computes effective debug mode from `tdata/withdebug`, CLI `-debug`, and alpha build status.
#[must_use]
pub fn compute_debug_mode(working_dir: &Path, config: &GlobalConfig) -> bool {
    let mut enabled = config.alpha_version != 0;
    if let Ok(bytes) = fs::read(debug_mode_setting_path(working_dir)) {
        enabled = bytes.first().copied() != Some(b'0');
    }
    if config.debug_mode {
        enabled = true;
    }
    enabled
}

/// Path to the install-beta preference file (`tdata/devversion`).
#[must_use]
pub fn install_beta_setting_path(working_dir: &Path) -> PathBuf {
    working_dir.join("tdata").join("devversion")
}

/// Persists beta-update preference (`Launcher::writeInstallBetaVersionsSetting`).
pub fn write_install_beta_setting(working_dir: &Path, enabled: bool) -> Result<(), LauncherError> {
    ensure_working_dir(working_dir)?;
    fs::write(
        install_beta_setting_path(working_dir),
        if enabled { b"1" } else { b"0" },
    )?;
    Ok(())
}

/// Computes beta-update opt-in from `tdata/devversion` and build defaults.
#[must_use]
pub fn compute_install_beta(working_dir: &Path, config: &GlobalConfig) -> bool {
    if config.alpha_version != 0 {
        return false;
    }
    if let Ok(bytes) = fs::read(install_beta_setting_path(working_dir)) {
        return bytes.first().copied() != Some(b'0');
    }
    config.install_beta_version
}

/// Reads or generates the persistent 64-bit random installation tag (`tdata/usertag`).
pub fn compute_installation_tag(working_dir: &Path) -> Result<u64, LauncherError> {
    let path = working_dir.join("tdata").join("usertag");
    if let Ok(bytes) = fs::read(&path)
        && bytes.len() == 8
    {
        let tag = u64::from_ne_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]);
        if tag != 0 {
            return Ok(tag);
        }
    }
    let mut rng = rand::thread_rng();
    let mut tag = 0u64;
    while tag == 0 {
        tag = rng.r#gen();
    }
    ensure_working_dir(working_dir)?;
    fs::write(&path, tag.to_ne_bytes())?;
    Ok(tag)
}

/// Builds the argument vector used when relaunching the application (`Platform::Launcher::launchUpdater`).
#[must_use]
pub fn build_relaunch_args(
    config: &GlobalConfig,
    action: RelaunchAction,
    debug_enabled: bool,
    updating: bool,
    update_failed: bool,
) -> Vec<String> {
    let mut args = Vec::new();
    if config.launch_mode == LaunchMode::AutoStart {
        args.push("-autostart".to_owned());
    }
    if debug_enabled {
        args.push("-debug".to_owned());
    }
    if config.test_mode {
        args.push("-testmode".to_owned());
    }
    if config.start_in_tray {
        args.push("-startintray".to_owned());
    }
    if config.custom_working_dir {
        args.push("-workdir".to_owned());
        args.push(config.working_dir.to_string_lossy().into_owned());
    }
    if config.data_file() != "data" {
        args.push("-key".to_owned());
        args.push(config.data_file().to_owned());
    }
    if config.many_instance {
        args.push("-many".to_owned());
    }
    match action {
        RelaunchAction::JustRelaunch => {
            if !updating || update_failed {
                args.push("-noupdate".to_owned());
            }
            if config.restarting_to_settings {
                args.push("-tosettings".to_owned());
            }
        }
        RelaunchAction::PerformUpdate => {
            args.push("-update".to_owned());
            if !config.exe_name.is_empty() {
                args.push("-exename".to_owned());
                args.push(config.exe_name.clone());
            }
        }
    }
    args
}

/// Spawns a new instance of the current executable with relaunch arguments.
pub fn relaunch_self(
    config: &GlobalConfig,
    action: RelaunchAction,
    debug_enabled: bool,
    updating: bool,
    update_failed: bool,
) -> Result<Child, LauncherError> {
    if config.exe_name.is_empty() {
        return Err(LauncherError::MissingExecutablePath);
    }
    let binary_path = config.exe_dir.join(&config.exe_name);
    let args = build_relaunch_args(config, action, debug_enabled, updating, update_failed);
    let mut cmd = Command::new(&binary_path);
    cmd.args(&args);
    if !config.working_dir.as_os_str().is_empty() {
        cmd.current_dir(&config.working_dir);
    }
    Ok(cmd.spawn()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn argument_parsing_table() {
        struct Case {
            argv: &'static [&'static str],
            check: fn(&ParsedArgs),
        }
        let cases = [
            Case {
                argv: &["rustgram.exe"],
                check: |p| {
                    assert!(!p.debug);
                    assert_eq!(p.launch_mode, LaunchMode::Normal);
                    assert!(p.key.is_empty());
                    assert!(p.workdir.is_none());
                    assert!(p.start_urls.is_empty());
                    assert!(p.send_paths.is_empty());
                },
            },
            Case {
                argv: &[
                    "rustgram.exe",
                    "-debug",
                    "-key",
                    "Work_Profile!@#123",
                    "-autostart",
                    "-noupdate",
                    "-tosettings",
                    "-startintray",
                    "-many",
                    "-externalinterface",
                    "-scale",
                    "150",
                    "-workdir",
                    "D:/tg/custom",
                ],
                check: |p| {
                    assert!(p.debug);
                    assert_eq!(p.key, "work_profile123");
                    assert_eq!(p.launch_mode, LaunchMode::AutoStart);
                    assert!(p.no_update);
                    assert!(p.to_settings);
                    assert!(p.start_in_tray);
                    assert!(p.many);
                    assert!(p.external_interface);
                    assert_eq!(p.scale, Some(150));
                    assert_eq!(p.workdir, Some(PathBuf::from("D:/tg/custom")));
                },
            },
            Case {
                argv: &["rustgram.exe", "-fixprevious", "-cleanup"],
                check: |p| {
                    // Precedence: -autostart > -fixprevious > -cleanup
                    assert_eq!(p.launch_mode, LaunchMode::FixPrevious);
                },
            },
            Case {
                argv: &["rustgram.exe", "-cleanup", "-quit"],
                check: |p| {
                    assert_eq!(p.launch_mode, LaunchMode::Cleanup);
                    assert!(p.quit);
                },
            },
            Case {
                argv: &["rustgram.exe", "-scale", "999"],
                check: |p| {
                    assert_eq!(p.scale, Some(SCALE_AUTO));
                },
            },
            Case {
                argv: &["rustgram.exe", "-scale", "notanumber"],
                check: |p| {
                    assert_eq!(p.scale, Some(SCALE_AUTO));
                },
            },
            Case {
                argv: &[
                    "rustgram.exe",
                    "-sendpath",
                    "C:/photos/cat.png",
                    "tg://resolve?domain=telegram",
                    "tonsite://example.ton/page",
                ],
                check: |p| {
                    assert_eq!(p.send_paths, vec![PathBuf::from("C:/photos/cat.png")]);
                    assert_eq!(
                        p.start_urls,
                        vec![
                            "tg://resolve?domain=telegram".to_owned(),
                            "tonsite://example.ton/page".to_owned(),
                        ]
                    );
                },
            },
            Case {
                argv: &["rustgram.exe", "--", "-debug", "tg://open?x=1", "file.txt"],
                check: |p| {
                    // After `--`, `-debug` is treated as a literal trailing argument, not a switch.
                    assert!(!p.debug);
                    assert_eq!(p.start_urls, vec!["tg://open?x=1".to_owned()]);
                    assert_eq!(
                        p.send_paths,
                        vec![PathBuf::from("-debug"), PathBuf::from("file.txt")]
                    );
                },
            },
            Case {
                argv: &["rustgram.exe", "-testagent"],
                check: |p| {
                    assert!(p.test_agent);
                    assert!(p.debug);
                    assert!(!p.test_mode);
                },
            },
            Case {
                argv: &["rustgram.exe", "-testmode", "-workdir", "D:/tg/test"],
                check: |p| {
                    assert!(p.test_mode);
                    assert!(!p.debug);
                    assert_eq!(p.workdir, Some(PathBuf::from("D:/tg/test")));
                },
            },
        ];

        for case in &cases {
            let parsed = parse_arguments(case.argv);
            (case.check)(&parsed);
        }
    }

    #[test]
    fn working_dir_resolution_precedence_with_temp_dirs() {
        let root = TempDir::new().unwrap();
        let exe_dir = root.path().join("bin");
        let app_data = root.path().join("appdata");
        let current_dir = root.path().join("cwd");
        fs::create_dir_all(&exe_dir).unwrap();
        fs::create_dir_all(&app_data).unwrap();
        fs::create_dir_all(&current_dir).unwrap();

        let env = WorkDirEnv {
            exe_dir: exe_dir.clone(),
            app_data_dir: Some(app_data.clone()),
            current_dir: current_dir.clone(),
        };

        // 1. Default -> %APPDATA%\Rustgram
        let args = parse_arguments(["rustgram.exe"]);
        let res = resolve_working_dir(&args, &env);
        assert_eq!(res.origin, WorkingDirOrigin::AppData);
        assert_eq!(res.path, app_data.join(APP_NAME));
        assert!(!res.is_portable);
        assert!(!res.is_custom);

        // 2. Adjacent tdata next to exe -> portable at exe_dir
        fs::create_dir_all(exe_dir.join("tdata")).unwrap();
        let res = resolve_working_dir(&args, &env);
        assert_eq!(res.origin, WorkingDirOrigin::PortableAdjacentTdata);
        assert_eq!(res.path, exe_dir);
        assert!(res.is_portable);

        // 3. RustgramForcePortable marker takes precedence over adjacent tdata
        let portable_marker = exe_dir.join(PORTABLE_FOLDER_NAME);
        fs::create_dir_all(&portable_marker).unwrap();
        let res = resolve_working_dir(&args, &env);
        assert_eq!(res.origin, WorkingDirOrigin::PortableMarker);
        assert_eq!(res.path, portable_marker);
        assert!(res.is_portable);

        // 4. -workdir flag overrides portable mode
        let custom_args = parse_arguments(["rustgram.exe", "-workdir", "my_profile"]);
        let res = resolve_working_dir(&custom_args, &env);
        assert_eq!(res.origin, WorkingDirOrigin::CustomFlag);
        assert_eq!(res.path, current_dir.join("my_profile"));
        assert!(res.is_custom);
        assert!(!res.is_portable);
    }

    #[test]
    fn auxiliary_tdata_files_and_relaunch_args() {
        let dir = TempDir::new().unwrap();
        let mut cfg = GlobalConfig {
            working_dir: dir.path().to_path_buf(),
            custom_working_dir: true,
            exe_dir: PathBuf::from("C:/Program Files/Rustgram"),
            exe_name: "Rustgram.exe".to_owned(),
            key_file: "alt".to_owned(),
            start_in_tray: true,
            restarting_to_settings: true,
            ..Default::default()
        };

        assert!(!compute_debug_mode(dir.path(), &cfg));
        write_debug_mode_setting(dir.path(), true).unwrap();
        assert!(compute_debug_mode(dir.path(), &cfg));

        write_install_beta_setting(dir.path(), true).unwrap();
        assert!(compute_install_beta(dir.path(), &cfg));

        let tag1 = compute_installation_tag(dir.path()).unwrap();
        assert_ne!(tag1, 0);
        let tag2 = compute_installation_tag(dir.path()).unwrap();
        assert_eq!(tag1, tag2);

        let relaunch = build_relaunch_args(&cfg, RelaunchAction::JustRelaunch, true, false, false);
        assert_eq!(
            relaunch,
            vec![
                "-debug",
                "-startintray",
                "-workdir",
                &dir.path().to_string_lossy(),
                "-key",
                "alt",
                "-noupdate",
                "-tosettings",
            ]
        );

        let h1 = instance_hash(&cfg);
        assert_eq!(h1.len(), 32);
        cfg.key_file = "other".to_owned();
        assert_ne!(h1, instance_hash(&cfg));
    }
}
