//! Command-line argument parsing and pure startup-plan computation.
//!
//! Port origin: `Telegram/SourceFiles/core/launcher.cpp`,
//! `Telegram/SourceFiles/core/sandbox.cpp`, and
//! `Telegram/SourceFiles/core/application.cpp` (startup mode & settings decisions).

use std::path::{Path, PathBuf};

use tg_core::launcher::{
    ParsedArgs, ResolvedWorkingDir, WorkDirEnv, apply_to_config, instance_hash, parse_arguments,
    resolve_working_dir, working_dir_hash,
};
use tg_core::{GlobalConfig, LaunchMode};
use tg_platform::app_id::{DEFAULT_APP_USER_MODEL_ID, build_app_user_model_id};
use tg_platform::single_instance::{InstanceActivation, single_instance_name};
use tg_platform::widget_sandbox::resolve_widget_helper_path_with_fallback;

use super::cli_guard::{CliRequest, classify_cli};
use super::ipc::build_activation_from_args;

/// High-level action chosen from parsed CLI arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupAction {
    /// Print CLI help and exit without starting the GUI or touching working dir.
    ShowHelp,
    /// Print the version (`--version`, `-v`) and exit without touching the working dir.
    ShowVersion,
    /// An unknown option was passed: print an error and exit without touching the working dir.
    RejectArguments,
    /// Stop running instance, remove OS integration (`-cleanup`), and exit.
    Cleanup,
    /// Proceed with single-instance handshake, crash check, and GUI launch.
    RunApp,
}

pub use crate::core_types::startup_types::StartupPrefs;

/// Environment variable enabling live MTProto session mode (`RUSTGRAM_LIVE=1`).
pub const ENV_LIVE_MODE: &str = "RUSTGRAM_LIVE";

/// Legacy fallback environment variable enabling live MTProto session mode (`TGFORK_LIVE=1`).
pub const LEGACY_ENV_LIVE_MODE: &str = "TGFORK_LIVE";

/// Returns `true` when an environment variable value enables live mode (`1`, `true`, `yes`, `on`).
#[must_use]
pub fn parse_live_mode_env(raw: Option<&str>) -> bool {
    let Some(val) = raw else {
        return false;
    };
    matches!(
        val.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Returns `true` if `RUSTGRAM_LIVE` (or legacy `TGFORK_LIVE`) is enabled in the current process environment.
#[must_use]
pub fn is_live_mode_env_enabled() -> bool {
    let val = std::env::var(ENV_LIVE_MODE)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| std::env::var(LEGACY_ENV_LIVE_MODE).ok());
    parse_live_mode_env(val.as_deref())
}

/// Chooses live MTProto mode or the offline demo (`MockSession`).
///
/// Live mode is the default whenever API credentials are available (runtime environment or
/// compiled into the build, like tdesktop's official builds). `--demo` always selects the
/// offline demo; `--live` / `RUSTGRAM_LIVE=1` force live mode even without credentials (the
/// app then shows how to provide them).
#[must_use]
pub const fn select_live_mode(
    explicit_live: bool,
    explicit_demo: bool,
    live_env: bool,
    credentials_available: bool,
) -> bool {
    !explicit_demo && (explicit_live || live_env || credentials_available)
}

/// Extra `tg-app` developer switches extracted before handing argv to `tg-core`'s parser.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DevSwitches {
    start_signed_in: bool,
    live_mode: bool,
    demo_mode: bool,
    settings_path: Option<PathBuf>,
}

/// Pure, deterministic startup plan derived from CLI arguments, environment, and saved preferences.
#[derive(Debug, Clone, PartialEq)]
pub struct StartupPlan {
    /// Top-level action to execute.
    pub action: StartupAction,
    /// Parsed tdesktop-compatible CLI arguments from `tg-core`.
    pub parsed_args: ParsedArgs,
    /// Resolved working directory and origin.
    pub resolved_working_dir: ResolvedWorkingDir,
    /// Populated `tg-core` global configuration.
    pub global_config: GlobalConfig,
    /// 32-char lowercase MD5 hash of the working directory (and `-key` suffix if any).
    pub working_dir_hash: String,
    /// 32-char lowercase MD5 instance hash for AUMID / portable identity.
    pub instance_hash: String,
    /// Canonical single-instance name used for the Windows named mutex & named pipe.
    pub single_instance_name: String,
    /// Explicit Windows `AppUserModelID` (`"Rustgram"` or `"Rustgram.<hash>"`).
    pub app_user_model_id: String,
    /// Whether single-instance coordination should run (`false` when `-many` is set).
    pub use_single_instance: bool,
    /// Whether the main window should start minimized to the system tray.
    pub start_minimized_to_tray: bool,
    /// Whether automatic startup/periodic update checking is enabled for this run.
    pub check_updates_enabled: bool,
    /// Whether beta releases are accepted by the updater.
    pub install_beta: bool,
    /// Whether OS Start-menu shortcut validation should run (`RunApp` only).
    pub validate_start_menu_shortcut: bool,
    /// Whether `tg://` and `tonsite://` URL schemes should be registered on startup.
    pub register_url_schemes_now: bool,
    /// Whether to open directly to Settings (`-tosettings`).
    pub open_settings_on_start: bool,
    /// Whether debug file logging is requested via CLI (`-debug` / `-testagent`).
    pub debug_logging: bool,
    /// Skip intro and open straight into the signed-in mock workspace (`--start-main`).
    pub start_signed_in: bool,
    /// `--live` was passed. Live mode is also the default when API credentials are available;
    /// see [`select_live_mode`].
    pub live_mode: bool,
    /// `--demo` was passed: run the offline `MockSession` demo even when credentials exist.
    pub demo_mode: bool,
    /// Resolved path for `ShellSettings` JSON.
    pub settings_path: PathBuf,
    /// Resolved path to the companion `rustgram-widget-helper.exe` binary next to the main executable.
    pub widget_helper_path: PathBuf,
    /// Activation payload to forward over the named pipe if another instance is already running.
    pub pipe_activation: InstanceActivation,
    /// Initial `tg://` / `tonsite://` links to process if we become the primary instance.
    pub initial_urls: Vec<String>,
    /// Initial local files (`-sendpath` or trailing paths) to share if we become primary.
    pub initial_send_paths: Vec<PathBuf>,
}

impl StartupPlan {
    /// Computes a complete [`StartupPlan`] from raw `argv` (including `argv[0]`), injectable
    /// [`WorkDirEnv`], optional executable path, and persisted [`StartupPrefs`].
    ///
    /// Performs no OS side effects (no registry, Start menu, or disk writes) so it can be
    /// thoroughly table-tested.
    #[must_use]
    pub fn build<I, S>(
        argv: I,
        env: &WorkDirEnv,
        exe_path: Option<&Path>,
        prefs: &StartupPrefs,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let raw_args: Vec<String> = argv.into_iter().map(|s| s.as_ref().to_owned()).collect();
        let (dev, filtered_argv) = extract_dev_switches(&raw_args);
        let parsed_args = parse_arguments(&filtered_argv);
        let resolved_working_dir = resolve_working_dir(&parsed_args, env);

        let mut global_config = GlobalConfig::default();
        apply_to_config(
            &parsed_args,
            &resolved_working_dir,
            exe_path,
            &mut global_config,
        );
        global_config.auto_update = prefs.auto_update;
        global_config.install_beta_version = prefs.install_beta;
        global_config.last_update_check = prefs
            .last_update_check_unix
            .clamp(i64::from(i32::MIN), i64::from(i32::MAX))
            as i32;

        let wd_hash = working_dir_hash(&resolved_working_dir.path, &parsed_args.key);
        let inst_hash = instance_hash(&global_config);
        let si_name = single_instance_name(DEFAULT_APP_USER_MODEL_ID, &wd_hash);

        let app_user_model_id = if resolved_working_dir.is_portable
            || resolved_working_dir.is_custom
            || !parsed_args.key.is_empty()
        {
            build_app_user_model_id(DEFAULT_APP_USER_MODEL_ID, Some(&inst_hash))
        } else {
            DEFAULT_APP_USER_MODEL_ID.to_owned()
        };

        let action = match classify_cli(&raw_args) {
            CliRequest::Version => StartupAction::ShowVersion,
            CliRequest::Help => StartupAction::ShowHelp,
            CliRequest::UnknownOption(_) => StartupAction::RejectArguments,
            CliRequest::Run if parsed_args.launch_mode == LaunchMode::Cleanup => {
                StartupAction::Cleanup
            }
            CliRequest::Run => StartupAction::RunApp,
        };

        let is_run = action == StartupAction::RunApp;
        let start_minimized_to_tray = parsed_args.start_in_tray
            || (parsed_args.launch_mode == LaunchMode::AutoStart
                && prefs.start_minimized_on_autostart);

        let check_updates_enabled =
            is_run && !parsed_args.no_update && prefs.auto_update && !parsed_args.quit;

        let register_url_schemes_now = is_run
            && !parsed_args.quit
            && !parsed_args.many
            // Test-server runs (agents, live checks) must never take over the system's tg:// handler.
            && !parsed_args.test_mode
            // Demo runs and custom `-workdir` runs (agents, benchmarks, scratch copies) never take it
            // either: the handler must keep pointing at the user's own Telegram.
            && !dev.demo_mode
            && !resolved_working_dir.is_custom
            && prefs.take_url_schemes
            && !prefs.url_schemes_registered;

        let settings_path = dev
            .settings_path
            .unwrap_or_else(|| resolved_working_dir.path.join("shell_state.json"));
        let widget_helper_path = resolve_widget_helper_path_with_fallback(exe_path, &env.exe_dir);

        let pipe_activation = build_activation_from_args(&parsed_args);
        let initial_urls = parsed_args.start_urls.clone();
        let initial_send_paths = parsed_args.send_paths.clone();

        Self {
            action,
            use_single_instance: !parsed_args.many,
            start_minimized_to_tray,
            check_updates_enabled,
            install_beta: prefs.install_beta,
            validate_start_menu_shortcut: is_run && !parsed_args.quit,
            register_url_schemes_now,
            open_settings_on_start: parsed_args.to_settings,
            debug_logging: parsed_args.debug,
            start_signed_in: dev.start_signed_in,
            live_mode: dev.live_mode,
            demo_mode: dev.demo_mode,
            settings_path,
            widget_helper_path,
            pipe_activation,
            initial_urls,
            initial_send_paths,
            parsed_args,
            resolved_working_dir,
            global_config,
            working_dir_hash: wd_hash,
            instance_hash: inst_hash,
            single_instance_name: si_name,
            app_user_model_id,
        }
    }
}

/// Extracts `tg-app` developer switches (`--start-main`, `--settings <PATH>`, `-h`/`--help`)
/// while preserving `argv[0]` and every tdesktop switch intact (respecting `--` terminator).
fn extract_dev_switches(raw: &[String]) -> (DevSwitches, Vec<String>) {
    let mut dev = DevSwitches::default();
    let mut out = Vec::with_capacity(raw.len());

    let mut i = 0usize;
    let mut after_double_dash = false;
    while i < raw.len() {
        let arg = &raw[i];
        if i == 0 {
            out.push(arg.clone());
            i += 1;
            continue;
        }
        if after_double_dash {
            out.push(arg.clone());
            i += 1;
            continue;
        }
        match arg.as_str() {
            "--" => {
                after_double_dash = true;
                out.push(arg.clone());
            }
            // Classified by `classify_cli`; never forwarded as trailing file paths.
            "-h" | "--help" => {}
            "--start-main" => {
                dev.start_signed_in = true;
            }
            "--live" => {
                dev.live_mode = true;
            }
            "--demo" => {
                dev.demo_mode = true;
            }
            "--settings" => {
                if let Some(next) = raw.get(i + 1) {
                    dev.settings_path = Some(PathBuf::from(next));
                    i += 1;
                }
            }
            // Preserve tdesktop one-value options together with their argument so a value like
            // `-workdir --settings` is never misread as a dev switch.
            "-key" | "-workdir" | "-scale" | "-sendpath" => {
                out.push(arg.clone());
                if let Some(next) = raw.get(i + 1) {
                    out.push(next.clone());
                    i += 1;
                }
            }
            _ => {
                out.push(arg.clone());
            }
        }
        i += 1;
    }

    (dev, out)
}

/// Formats the CLI usage banner for `-h` / `--help`.
#[must_use]
pub fn help_text() -> &'static str {
    "Rustgram Desktop (Rust port of Telegram Desktop)\n\
     Usage: rustgram [OPTIONS] [tg://... | tonsite://... | FILE...]\n\n\
     Telegram Desktop Options:\n  \
       -workdir <DIR>       Use a custom working directory\n  \
       -many                Allow running multiple instances\n  \
       -key <NAME>          Use an alternate profile key name\n  \
       -autostart           Started automatically at OS login (starts in tray)\n  \
       -startintray         Start minimized to the system tray\n  \
       -noupdate            Disable startup and background update checks\n  \
       -cleanup             Stop running instance and remove OS shortcuts/registration\n  \
       -debug               Enable verbose debug file logging\n  \
       -sendpath <FILE>     Open share dialog to send a local file\n  \
       -tosettings          Open directly into Settings\n  \
       -quit                Request the running instance to quit\n  \
       -scale <50..300>     Override UI scale percentage\n  \
       --                   Treat all remaining arguments as URLs or file paths\n\n\
     Developer Options:\n  \
       --demo               Run the offline demo (no Telegram connection)\n  \
       --live               Force live mode (default when API credentials are available)\n  \
       --start-main         Skip Intro and open straight into the signed-in workspace\n  \
       --settings <PATH>    Use a custom shell settings JSON file\n  \
       -h, --help           Show this help message
         -v, --version        Print the version and exit"
}
