//! Application startup bootstrap, CLI planning, single-instance IPC, crash recovery,
//! logging, platform integration, and self-updater orchestration.
//!
//! Port origin: `Telegram/SourceFiles/main.cpp`,
//! `Telegram/SourceFiles/core/launcher.cpp`,
//! `Telegram/SourceFiles/core/sandbox.cpp`,
//! `Telegram/SourceFiles/core/crash_reports.cpp`,
//! `Telegram/SourceFiles/core/crash_report_window.cpp`,
//! `Telegram/SourceFiles/core/update_checker.cpp`, and
//! `Telegram/SourceFiles/platform/win/specific_win.cpp`.

pub mod cli_guard;
pub mod crash_window;
pub mod frame_trace;
pub mod icon;
pub mod ipc;
pub mod plan;
pub mod platform;
pub mod profile_paths;
pub mod profiler;
pub mod scale_restart;
pub mod smoke_test;
pub use crate::core_types::restore_windows;
pub mod updater;

use std::path::PathBuf;
use std::sync::Arc;

use tg_core::launcher::{WorkDirEnv, compute_installation_tag};
use tg_core::{
    CrashReportManager, CrashStartResult, LogManager, TdLogLayer, install_crash_handlers,
};
use tg_platform::single_instance::SingleInstanceGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::app::{AppConfig, run_app};

pub use crash_window::{CrashRecoveryAction, CrashRecoveryState, CrashSummary};
pub use icon::{APP_ICON_SVG, DEFAULT_WINDOW_ICON_SIZE, load_window_icon};
pub use ipc::{
    ActivationQueue, IncomingActivation, SingleInstanceOutcome, acquire_or_forward_single_instance,
    build_activation_from_args, decode_pipe_message, encode_pipe_message,
};
pub use plan::{
    ENV_LIVE_MODE, StartupAction, StartupPlan, StartupPrefs, help_text, is_live_mode_env_enabled,
    parse_live_mode_env, select_live_mode,
};
pub use platform::{CleanupReport, PlatformHooks, RealWindowsPlatform, StartupPlatformReport};
pub use profile_paths::{
    ProfileDataPaths, apply_profile_data_paths, apply_profile_data_paths_native,
};
pub use restore_windows::{
    RestoreShell, RestoreShellStatus, RestoreWindowsAction, RestoreWindowsManager,
    RestoreWindowsOffer, RestoreWindowsPolicy, SavedWindowEntry, SavedWindowGeometry,
    SavedWindowsSnapshot, ScreenBounds,
};
pub use updater::{
    RustlsHttpClient, UpdateController, UpdateSnapshot, UpdateState, UpdateStateMachine,
    UpdateUiAction, UpdaterLaunchPlan, build_updater_launch_plan, current_app_version,
};

/// Live startup resources handed from [`run_from_cli`] into [`crate::app::run_app`] and
/// [`crate::app::TgforkApp`].
pub struct StartupContext {
    /// Resolved startup plan for this launch.
    pub plan: StartupPlan,
    /// Current executable path.
    pub exe_path: PathBuf,
    /// Crash manager owning `tdata/working` and `tdata/dumps`.
    pub crash_manager: Arc<CrashReportManager>,
    /// File logger owning `log.txt` / `log_startN.txt` / `DebugLogs/`.
    pub log_manager: Arc<LogManager>,
    /// Named-mutex + named-pipe guard kept alive for the primary instance's lifetime.
    pub single_instance_guard: Option<SingleInstanceGuard>,
    /// Queue receiving activations forwarded from secondary instances over the named pipe.
    pub activation_queue: ActivationQueue,
    /// Background update checker and restart launcher.
    pub update_controller: UpdateController,
    /// Present when the previous run crashed or exited uncleanly (`tdata/working` existed).
    pub crash_recovery: Option<CrashRecoveryState>,
}

impl std::fmt::Debug for StartupContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartupContext")
            .field("working_dir", &self.plan.resolved_working_dir.path)
            .field("has_single_instance", &self.single_instance_guard.is_some())
            .field("has_crash_recovery", &self.crash_recovery.is_some())
            .field(
                "start_minimized_to_tray",
                &self.plan.start_minimized_to_tray,
            )
            .finish()
    }
}

/// Full production entry point called by `crates/tg-app/src/main.rs`.
///
/// Executes tdesktop's startup pipeline in order:
/// 1. Parse `LaunchArgs` (`-workdir`, `-many`, `-key`, `-autostart`, `-startintray`, `-noupdate`,
///    `-cleanup`, `-debug`, `-sendpath`, `-tosettings`, `-quit`, `tg://`, `tonsite://`, etc.).
/// 2. Handle `--version` / `--help` / unknown options and refuse agent runs without their own
///    `-workdir` ([`cli_guard`]) before the working dir is touched, then `-cleanup` (uninstaller
///    integration) before GUI creation.
/// 3. Initialize `tg-core` file logger in the resolved working dir + tracing console layer.
/// 4. Set Windows `AppUserModelID` (`"Rustgram"` or `"Rustgram.<hash>"`).
/// 5. Acquire single-instance mutex + named pipe via `tg-platform`, forwarding arguments and
///    exiting immediately if another instance is already running.
/// 6. Promote `log_startN.txt` -> `log.txt`, validate Start-menu shortcut, and register
///    `tg://` / `tonsite://` on first run if allowed.
/// 7. Initialize `tg-core` crash handler + panic hook and detect previous unclean shutdowns.
/// 8. Start periodic `tg-updater` background checker (unless `-noupdate` or disabled).
/// 9. Launch the `eframe` window (showing the crash recovery screen first if a crash occurred,
///    or starting minimized to tray on `-startintray` / `-autostart`).
pub fn run_from_cli() -> anyhow::Result<()> {
    let raw_args: Vec<String> = std::env::args().collect();
    let work_env = WorkDirEnv::from_process();
    let exe_path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("rustgram.exe"));

    // First pass finds the working dir so persisted `StartupPrefs` can inform the final plan.
    let prelim = StartupPlan::build(
        &raw_args,
        &work_env,
        Some(&exe_path),
        &StartupPrefs::default(),
    );
    // `--version` / `--help` / unknown options and agent runs without their own `-workdir` end
    // here, before the working dir is read or written (TGB-3005).
    let agent = cli_guard::detect_agent(std::env::vars_os());
    if let Some(exit) = cli_guard::early_exit(
        &raw_args,
        &prelim.resolved_working_dir,
        &work_env,
        agent.as_ref(),
    ) {
        exit.emit();
        if exit.code != 0 {
            std::process::exit(exit.code);
        }
        return Ok(());
    }
    // `--smoke-test -workdir <scratch>` (release workflow): storage only, no window, network,
    // lock, updater or system integration.
    if prelim.action == StartupAction::SmokeTest {
        let data_name = if prelim.parsed_args.key.is_empty() {
            "data"
        } else {
            prelim.parsed_args.key.as_str()
        };
        let code = smoke_test::run_and_report(
            &prelim.resolved_working_dir.path,
            data_name,
            &prelim.settings_path,
        );
        std::process::exit(code);
    }
    let prefs = StartupPrefs::load_from(&prelim.resolved_working_dir.path);
    let mut plan = StartupPlan::build(&raw_args, &work_env, Some(&exe_path), &prefs);
    profiler::mark("cli_and_plan");

    match plan.action {
        // Handled by `cli_guard::early_exit` and the smoke test above.
        StartupAction::ShowHelp
        | StartupAction::ShowVersion
        | StartupAction::RejectArguments
        | StartupAction::SmokeTest => {
            return Ok(());
        }
        StartupAction::Cleanup => {
            init_console_only_tracing(plan.debug_logging);
            let mut platform_hooks = RealWindowsPlatform;
            let report = platform::run_cleanup(&plan, &exe_path, &mut platform_hooks);
            tracing::info!(?report, "Completed -cleanup uninstall integration");
            return Ok(());
        }
        StartupAction::RunApp => {}
    }

    let working_dir = plan.resolved_working_dir.path.clone();
    // The saved "Default interface scale" unless `-scale` was given (TGB-3111).
    scale_restart::apply_saved_scale(
        &mut plan.global_config,
        scale_restart::read_saved_scale(&working_dir),
    );
    let relaunch = scale_restart::relaunch_command(&plan, &exe_path);

    // Start `tg-core` working-directory file logger (`log_startN.txt`) + console tracing layer.
    let log_manager = LogManager::new();
    log_manager.set_debug_enabled(plan.debug_logging);
    let _ = LogManager::install_global(Arc::clone(&log_manager));
    if let Err(err) = log_manager.start(&working_dir) {
        eprintln!(
            "Warning: could not initialize log file in '{}': {err}",
            working_dir.display()
        );
    }
    init_tracing_with_file_logger(Arc::clone(&log_manager), plan.debug_logging);
    profiler::mark("logger_init");

    // Apply explicit Windows AppUserModelID early (before creating windows or toast notifications).
    let mut platform_hooks = RealWindowsPlatform;
    platform::apply_process_app_id(&plan, &mut platform_hooks);
    profiler::mark("app_id");

    // Single-instance check & argument forwarding via `tg-platform` named mutex + named pipe.
    let (single_instance_guard, activation_queue) = if plan.use_single_instance {
        match acquire_or_forward_single_instance(&plan.single_instance_name, &plan.pipe_activation)
        {
            Ok(SingleInstanceOutcome::Primary { guard, queue }) => {
                if plan.parsed_args.quit {
                    // `-quit` was requested, but no existing instance was running.
                    drop(guard);
                    log_manager.finish();
                    return Ok(());
                }
                if let Err(err) = log_manager.instance_checked() {
                    tracing::warn!(%err, "Could not promote startup log to log.txt");
                }
                (Some(guard), queue)
            }
            Ok(SingleInstanceOutcome::ForwardedToExisting { response }) => {
                tracing::info!(
                    ?response,
                    instance = %plan.single_instance_name,
                    "Forwarded launch arguments to running Rustgram instance; exiting"
                );
                log_manager.finish();
                return Ok(());
            }
            Err(err) => {
                tracing::error!(
                    %err,
                    "Single-instance coordination failed; not starting a second instance"
                );
                let refused = refuse_without_single_instance(&err, plan.parsed_args.quit);
                if let Err(refusal) = &refused {
                    tg_platform::single_instance::show_startup_error(
                        "Rustgram",
                        &refusal.to_string(),
                    );
                }
                log_manager.finish();
                return refused;
            }
        }
    } else {
        if plan.parsed_args.quit {
            log_manager.finish();
            return Ok(());
        }
        (None, ActivationQueue::new())
    };

    // Start-menu shortcut validation + first-run `tg://` / `tonsite://` registration.
    profiler::mark("single_instance");
    // The Start-menu shortcut (COM) and registry writes take ~40 ms and nothing on the way to the
    // first frame depends on them, so they run on a background thread.
    spawn_startup_integration(plan.clone(), exe_path.clone(), prefs.clone());
    profiler::mark("platform_integration");

    // Initialize `tg-core` crash handler (panic hook + Windows SEH filter) and inspect `tdata/working`.
    let crash_manager = CrashReportManager::new(&working_dir);
    let installation_tag = compute_installation_tag(&working_dir).unwrap_or(0);
    crash_manager.init_default_annotations(&plan.global_config, installation_tag);
    crash_manager.attach_logger(Arc::clone(&log_manager));
    install_crash_handlers(Arc::clone(&crash_manager));

    let crash_recovery = match crash_manager.start() {
        Ok(CrashStartResult::Started) => None,
        Ok(CrashStartResult::PreviousCrash(bytes)) => {
            tracing::warn!(
                dump_bytes = bytes.len(),
                "Detected unclean previous exit (`tdata/working`); showing crash recovery window"
            );
            Some(CrashRecoveryState::from_dump_with_upload_config(
                &bytes,
                crash_manager.dumps_dir().to_path_buf(),
                crash_manager.upload_config(),
            ))
        }
        Err(err) => {
            tracing::warn!(%err, "Could not initialize crash report state file");
            None
        }
    };
    profiler::mark("crash_manager");

    // Start periodic self-update checker unless disabled via `-noupdate` or prefs.
    let update_controller = UpdateController::start(
        exe_path.clone(),
        working_dir,
        plan.resolved_working_dir.is_custom,
        plan.parsed_args.clone(),
        &prefs,
    );
    profiler::mark("update_controller_start");

    let live_mode = select_live_mode(
        plan.live_mode,
        plan.demo_mode,
        is_live_mode_env_enabled(),
        tg_session::api_credentials_available(),
    );
    let native_window_frame = std::env::var("RUSTGRAM_NATIVE_FRAME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| std::env::var("TGFORK_NATIVE_FRAME").ok())
        .map(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "yes"));
    let config = AppConfig {
        start_signed_in: plan.start_signed_in,
        live_mode,
        native_window_frame,
        settings_path: Some(plan.settings_path.clone()),
        startup: Some(StartupContext {
            plan,
            exe_path,
            crash_manager,
            log_manager,
            single_instance_guard,
            activation_queue,
            update_controller,
            crash_recovery,
        }),
    };

    let result = run_app(config);
    // `Core::Restart`: start again once the window, the single-instance guard and the logs are
    // gone.
    scale_restart::relaunch_after_exit(&relaunch);
    result
}

/// Runs [`platform::apply_startup_integration`] (Start-menu shortcut validation and first-run
/// `tg://` / `tonsite://` registration) on a background thread, saving `startup_prefs.json` when
/// the handlers were registered. If the thread cannot start, the integration is retried on the
/// next launch (the prefs flag stays unset).
fn spawn_startup_integration(plan: StartupPlan, exe_path: PathBuf, mut prefs: StartupPrefs) {
    let spawned = std::thread::Builder::new()
        .name("startup-integration".to_owned())
        .spawn(move || {
            updater::remove_obsolete_install_files(&exe_path);
            let mut hooks = RealWindowsPlatform;
            let report =
                platform::apply_startup_integration(&plan, &exe_path, &mut prefs, &mut hooks);
            if report.url_schemes_registered
                && let Err(err) = prefs.save_to(&plan.resolved_working_dir.path)
            {
                tracing::warn!(%err, "Could not save updated startup_prefs.json");
            }
        });
    if let Err(err) = spawned {
        tracing::warn!(%err, "Could not start the startup integration thread");
    }
}

/// Release builds are GUI-subsystem binaries without a console of their own, so `--help` /
/// `--version` attach to the console of the shell that started it (if any) before printing.
fn attach_parent_console() {
    #[cfg(windows)]
    {
        use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        // SAFETY: `AttachConsole` takes no pointers; failure (no parent console, or a console
        // is already attached as in debug builds) only means the help text is not shown.
        let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }
}

fn init_console_only_tracing(debug: bool) {
    let default_filter = if debug { "debug" } else { "info" };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

fn init_tracing_with_file_logger(log_manager: Arc<LogManager>, debug: bool) {
    let default_filter = if debug { "debug" } else { "info" };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(TdLogLayer::new(log_manager))
        .try_init();
}

/// A launch that could neither take nor reach the single-instance lock must not open the same
/// `tdata` a second time (a pre-created pipe name would otherwise disable the lock): with
/// `-quit` there is nothing to stop, otherwise startup is refused.
fn refuse_without_single_instance(
    err: &tg_platform::PlatformError,
    quit: bool,
) -> anyhow::Result<()> {
    if quit {
        return Ok(());
    }
    Err(anyhow::anyhow!(
        "Rustgram could not check whether it is already running for this data folder, so it \
         will not start a second copy. Close other Rustgram windows and try again. ({err})"
    ))
}

#[cfg(test)]
#[path = "sec3_single_instance_tests.rs"]
mod sec3_single_instance_tests;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use ed25519_dalek::SigningKey;
    use tempfile::TempDir;
    use tg_core::LaunchMode;
    use tg_core::launcher::{WorkDirEnv, WorkingDirOrigin};
    use tg_platform::app_id::DEFAULT_APP_USER_MODEL_ID;
    use tg_platform::single_instance::{InstanceActivation, InstanceResponse};
    use tg_updater::UpdateError;
    use tg_updater::checker::UpdateConfig;
    use tg_updater::github::HttpClient;
    use tg_updater::package::pack_update;
    use tg_updater::version::{Target, UpdateChannel, Version};

    use super::crash_window::{CrashRecoveryState, CrashSummary};
    use super::icon::{
        APP_ICON_SVG, DEFAULT_WINDOW_ICON_SIZE, load_window_icon, render_svg_to_icon_data,
    };
    use super::ipc::{
        ActivationQueue, IncomingActivation, build_activation_from_args, decode_pipe_message,
        decode_pipe_response, encode_pipe_message, encode_pipe_response, file_url_to_path,
        path_to_file_url,
    };
    use super::plan::{StartupAction, StartupPlan, StartupPrefs};
    use super::platform::{
        PlatformHooks, apply_process_app_id, apply_startup_integration, run_cleanup,
    };
    use super::updater::{UpdateState, UpdateStateMachine, build_updater_launch_plan};

    fn test_env(base: &Path) -> WorkDirEnv {
        WorkDirEnv {
            exe_dir: base.join("install"),
            app_data_dir: Some(base.join("appdata")),
            current_dir: base.join("cwd"),
        }
    }

    // -------------------------------------------------------------------------
    // 1. Argument -> StartupPlan table tests (no real OS / registry / %APPDATA%)
    // -------------------------------------------------------------------------

    #[test]
    fn startup_plan_argument_table() {
        let dir = TempDir::new().unwrap();
        let env = test_env(dir.path());
        let exe = env.exe_dir.join("rustgram.exe");
        let custom_wd = dir.path().join("custom_wd");
        let custom_wd_str = custom_wd.to_string_lossy().into_owned();
        let custom_settings = dir.path().join("my_shell.json");
        let custom_settings_str = custom_settings.to_string_lossy().into_owned();

        struct Case<'a> {
            name: &'a str,
            argv: Vec<&'a str>,
            prefs: StartupPrefs,
            expected_action: StartupAction,
            expected_single_instance: bool,
            expected_tray: bool,
            expected_updates: bool,
            expected_register_schemes: bool,
            expected_debug: bool,
            expected_to_settings: bool,
            expected_start_signed_in: bool,
            expected_urls: Vec<&'a str>,
            expected_send_paths: Vec<&'a str>,
        }

        let registered_prefs = StartupPrefs {
            url_schemes_registered: true,
            ..StartupPrefs::default()
        };

        let cases = vec![
            Case {
                name: "default_first_run_keeps_the_users_tg_handler",
                argv: vec!["rustgram.exe"],
                prefs: StartupPrefs::default(),
                expected_action: StartupAction::RunApp,
                expected_single_instance: true,
                expected_tray: false,
                expected_updates: true,
                expected_register_schemes: false,
                expected_debug: false,
                expected_to_settings: false,
                expected_start_signed_in: false,
                expected_urls: vec![],
                expected_send_paths: vec![],
            },
            Case {
                name: "opted_in_first_run_takes_the_tg_handler",
                argv: vec!["rustgram.exe"],
                prefs: StartupPrefs {
                    take_url_schemes: true,
                    ..StartupPrefs::default()
                },
                expected_action: StartupAction::RunApp,
                expected_single_instance: true,
                expected_tray: false,
                expected_updates: true,
                expected_register_schemes: true,
                expected_debug: false,
                expected_to_settings: false,
                expected_start_signed_in: false,
                expected_urls: vec![],
                expected_send_paths: vec![],
            },
            Case {
                name: "subsequent_run_skips_scheme_reregistration",
                argv: vec!["rustgram.exe"],
                prefs: registered_prefs.clone(),
                expected_action: StartupAction::RunApp,
                expected_single_instance: true,
                expected_tray: false,
                expected_updates: true,
                expected_register_schemes: false,
                expected_debug: false,
                expected_to_settings: false,
                expected_start_signed_in: false,
                expected_urls: vec![],
                expected_send_paths: vec![],
            },
            Case {
                name: "many_and_noupdate_and_debug",
                argv: vec![
                    "rustgram.exe",
                    "-many",
                    "-noupdate",
                    "-debug",
                    "-key",
                    "Work_2",
                ],
                prefs: StartupPrefs::default(),
                expected_action: StartupAction::RunApp,
                expected_single_instance: false,
                expected_tray: false,
                expected_updates: false,
                expected_register_schemes: false,
                expected_debug: true,
                expected_to_settings: false,
                expected_start_signed_in: false,
                expected_urls: vec![],
                expected_send_paths: vec![],
            },
            Case {
                name: "autostart_starts_in_tray",
                argv: vec!["rustgram.exe", "-autostart"],
                prefs: registered_prefs.clone(),
                expected_action: StartupAction::RunApp,
                expected_single_instance: true,
                expected_tray: true,
                expected_updates: true,
                expected_register_schemes: false,
                expected_debug: false,
                expected_to_settings: false,
                expected_start_signed_in: false,
                expected_urls: vec![],
                expected_send_paths: vec![],
            },
            Case {
                name: "autostart_visible_when_pref_disabled",
                argv: vec!["rustgram.exe", "-autostart"],
                prefs: StartupPrefs {
                    start_minimized_on_autostart: false,
                    url_schemes_registered: true,
                    ..StartupPrefs::default()
                },
                expected_action: StartupAction::RunApp,
                expected_single_instance: true,
                expected_tray: false,
                expected_updates: true,
                expected_register_schemes: false,
                expected_debug: false,
                expected_to_settings: false,
                expected_start_signed_in: false,
                expected_urls: vec![],
                expected_send_paths: vec![],
            },
            Case {
                name: "startintray_overrides_autostart_pref",
                argv: vec!["rustgram.exe", "-startintray"],
                prefs: StartupPrefs {
                    start_minimized_on_autostart: false,
                    url_schemes_registered: true,
                    ..StartupPrefs::default()
                },
                expected_action: StartupAction::RunApp,
                expected_single_instance: true,
                expected_tray: true,
                expected_updates: true,
                expected_register_schemes: false,
                expected_debug: false,
                expected_to_settings: false,
                expected_start_signed_in: false,
                expected_urls: vec![],
                expected_send_paths: vec![],
            },
            Case {
                name: "cleanup_for_uninstaller",
                argv: vec!["rustgram.exe", "-cleanup"],
                prefs: StartupPrefs::default(),
                expected_action: StartupAction::Cleanup,
                expected_single_instance: true,
                expected_tray: false,
                expected_updates: false,
                expected_register_schemes: false,
                expected_debug: false,
                expected_to_settings: false,
                expected_start_signed_in: false,
                expected_urls: vec![],
                expected_send_paths: vec![],
            },
            Case {
                name: "help_flag",
                argv: vec!["rustgram.exe", "--help"],
                prefs: StartupPrefs::default(),
                expected_action: StartupAction::ShowHelp,
                expected_single_instance: true,
                expected_tray: false,
                expected_updates: false,
                expected_register_schemes: false,
                expected_debug: false,
                expected_to_settings: false,
                expected_start_signed_in: false,
                expected_urls: vec![],
                expected_send_paths: vec![],
            },
            Case {
                name: "tg_and_tonsite_urls_plus_sendpath",
                argv: vec![
                    "rustgram.exe",
                    "-workdir",
                    &custom_wd_str,
                    "-sendpath",
                    r"C:\tmp\photo.png",
                    "-tosettings",
                    "--start-main",
                    "--settings",
                    &custom_settings_str,
                    "tg://resolve?domain=rustgram_log",
                    "tonsite://foundation.ton",
                ],
                prefs: registered_prefs.clone(),
                expected_action: StartupAction::RunApp,
                expected_single_instance: true,
                expected_tray: false,
                expected_updates: true,
                expected_register_schemes: false,
                expected_debug: false,
                expected_to_settings: true,
                expected_start_signed_in: true,
                expected_urls: vec![
                    "tg://resolve?domain=rustgram_log",
                    "tonsite://foundation.ton",
                ],
                expected_send_paths: vec![r"C:\tmp\photo.png"],
            },
            Case {
                name: "demo_first_run_never_takes_the_tg_handler",
                argv: vec!["rustgram.exe", "--demo"],
                prefs: StartupPrefs::default(),
                expected_action: StartupAction::RunApp,
                expected_single_instance: true,
                expected_tray: false,
                expected_updates: true,
                expected_register_schemes: false,
                expected_debug: false,
                expected_to_settings: false,
                expected_start_signed_in: false,
                expected_urls: vec![],
                expected_send_paths: vec![],
            },
            Case {
                name: "custom_workdir_first_run_never_takes_the_tg_handler",
                argv: vec!["rustgram.exe", "-workdir", &custom_wd_str],
                prefs: StartupPrefs::default(),
                expected_action: StartupAction::RunApp,
                expected_single_instance: true,
                expected_tray: false,
                expected_updates: true,
                expected_register_schemes: false,
                expected_debug: false,
                expected_to_settings: false,
                expected_start_signed_in: false,
                expected_urls: vec![],
                expected_send_paths: vec![],
            },
            Case {
                name: "auto_update_off_disables_checks",
                argv: vec!["rustgram.exe"],
                prefs: StartupPrefs {
                    auto_update: false,
                    url_schemes_registered: true,
                    ..StartupPrefs::default()
                },
                expected_action: StartupAction::RunApp,
                expected_single_instance: true,
                expected_tray: false,
                expected_updates: false,
                expected_register_schemes: false,
                expected_debug: false,
                expected_to_settings: false,
                expected_start_signed_in: false,
                expected_urls: vec![],
                expected_send_paths: vec![],
            },
        ];

        for case in cases {
            let plan = StartupPlan::build(&case.argv, &env, Some(&exe), &case.prefs);
            assert_eq!(plan.action, case.expected_action, "case {}", case.name);
            assert_eq!(
                plan.use_single_instance, case.expected_single_instance,
                "case {}",
                case.name
            );
            assert_eq!(
                plan.start_minimized_to_tray, case.expected_tray,
                "case {}",
                case.name
            );
            assert_eq!(
                plan.check_updates_enabled, case.expected_updates,
                "case {}",
                case.name
            );
            assert_eq!(
                plan.register_url_schemes_now, case.expected_register_schemes,
                "case {}",
                case.name
            );
            assert_eq!(
                plan.debug_logging, case.expected_debug,
                "case {}",
                case.name
            );
            assert_eq!(
                plan.open_settings_on_start, case.expected_to_settings,
                "case {}",
                case.name
            );
            assert_eq!(
                plan.start_signed_in, case.expected_start_signed_in,
                "case {}",
                case.name
            );
            assert_eq!(plan.initial_urls, case.expected_urls, "case {}", case.name);
            let got_paths: Vec<String> = plan
                .initial_send_paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            assert_eq!(got_paths, case.expected_send_paths, "case {}", case.name);
        }

        // Verify AUMID & workdir origin: default AppData -> "Rustgram", custom -workdir -> "Rustgram.<hash>".
        let default_plan =
            StartupPlan::build(["rustgram.exe"], &env, Some(&exe), &StartupPrefs::default());
        assert_eq!(
            default_plan.resolved_working_dir.origin,
            WorkingDirOrigin::AppData
        );
        assert_eq!(default_plan.app_user_model_id, DEFAULT_APP_USER_MODEL_ID);

        let custom_plan = StartupPlan::build(
            ["rustgram.exe", "-workdir", &custom_wd_str, "-key", "alt"],
            &env,
            Some(&exe),
            &StartupPrefs::default(),
        );
        assert_eq!(
            custom_plan.resolved_working_dir.origin,
            WorkingDirOrigin::CustomFlag
        );
        assert!(custom_plan.app_user_model_id.starts_with("Rustgram."));
        assert_eq!(custom_plan.parsed_args.key, "alt");
        assert_ne!(
            default_plan.single_instance_name,
            custom_plan.single_instance_name
        );
    }

    // -------------------------------------------------------------------------
    // 2. Pipe message encoding / decoding tests
    // -------------------------------------------------------------------------

    #[test]
    fn pipe_message_encoding_and_decoding_roundtrip() {
        // Show-only activation.
        let show_act = InstanceActivation {
            show: true,
            quit: false,
            urls: vec![],
            args: vec![],
        };
        let wire = encode_pipe_message(&show_act);
        assert_eq!(wire, "CMD:show;");
        let decoded = decode_pipe_message(&wire);
        assert!(decoded.show);
        assert!(!decoded.quit);
        assert!(decoded.urls.is_empty());
        assert!(decoded.send_paths.is_empty());

        // Quit activation.
        let quit_act = InstanceActivation {
            show: false,
            quit: true,
            urls: vec![],
            args: vec![],
        };
        let wire = encode_pipe_message(&quit_act);
        assert_eq!(wire, "CMD:quit;");
        let decoded = decode_pipe_message(&wire);
        assert!(decoded.quit);
        assert!(!decoded.show);

        // tg:// + tonsite:// URLs with special characters (`;`, `%`, Unicode).
        let url_act = InstanceActivation {
            show: false,
            quit: false,
            urls: vec![
                "tg://resolve?domain=rustgram_log&post=42".to_owned(),
                "tonsite://example.ton/path?a=1;b=50%".to_owned(),
            ],
            args: vec![],
        };
        let wire = encode_pipe_message(&url_act);
        assert!(wire.contains("OPEN:tg://resolve?domain=rustgram_log&post=42;"));
        assert!(wire.contains("%003b")); // escaped ';'
        assert!(wire.contains("%0025")); // escaped '%'
        let decoded = decode_pipe_message(&wire);
        assert_eq!(decoded.urls, url_act.urls);
        assert!(decoded.send_paths.is_empty());
        assert!(decoded.show);

        // -sendpath local file paths encoded as file:/// and decoded back to PathBuf.
        let parsed = tg_core::launcher::parse_arguments(&[
            "rustgram.exe".to_owned(),
            "-sendpath".to_owned(),
            r"C:\Users\Alice\My Photos\cat;1.png".to_owned(),
        ]);
        let send_act = build_activation_from_args(&parsed);
        let wire = encode_pipe_message(&send_act);
        let decoded = decode_pipe_message(&wire);
        assert!(decoded.urls.is_empty());
        assert_eq!(
            decoded.send_paths,
            vec![PathBuf::from("C:/Users/Alice/My Photos/cat;1.png")]
        );
        assert!(decoded.show);

        // Response encoding/decoding (`RES:<pid>_<window_id>;`).
        let resp = InstanceResponse {
            process_id: 4242,
            window_id: 0x0012_3456,
        };
        let resp_wire = encode_pipe_response(resp);
        assert_eq!(decode_pipe_response(&resp_wire), Some(resp));

        // File URL helpers for Windows drive & UNC paths.
        assert_eq!(
            path_to_file_url(Path::new(r"D:\work\file.txt")),
            "file:///D:/work/file.txt"
        );
        assert_eq!(
            file_url_to_path("file:///D:/work/file.txt"),
            Some(PathBuf::from("D:/work/file.txt"))
        );
        assert_eq!(
            file_url_to_path("file://server/share/doc.pdf"),
            Some(PathBuf::from(r"\\server\share\doc.pdf"))
        );

        // ActivationQueue FIFO ordering.
        let q = ActivationQueue::new();
        q.push(IncomingActivation::default()); // empty ignored
        q.push(IncomingActivation {
            urls: vec!["tg://settings".to_owned()],
            send_paths: vec![],
            show: true,
            quit: false,
        });
        let drained = q.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].urls, vec!["tg://settings"]);
        assert!(q.drain().is_empty());
    }

    // -------------------------------------------------------------------------
    // 3. Update state machine tests with fake HttpClient
    // -------------------------------------------------------------------------

    type FakeResponseMap = HashMap<String, Result<Vec<u8>, String>>;

    #[derive(Clone, Default)]
    struct FakeHttpClient {
        responses: Arc<Mutex<FakeResponseMap>>,
        requested_urls: Arc<Mutex<Vec<String>>>,
    }

    impl FakeHttpClient {
        fn insert_ok(&self, url: &str, body: Vec<u8>) {
            self.responses
                .lock()
                .unwrap()
                .insert(url.to_owned(), Ok(body));
        }

        fn insert_err(&self, url: &str, msg: &str) {
            self.responses
                .lock()
                .unwrap()
                .insert(url.to_owned(), Err(msg.to_owned()));
        }

        fn request_count(&self) -> usize {
            self.requested_urls.lock().unwrap().len()
        }
    }

    impl HttpClient for FakeHttpClient {
        fn get_bytes(
            &self,
            url: &str,
            _headers: &[(&str, &str)],
            max_bytes: usize,
        ) -> Result<Vec<u8>, UpdateError> {
            self.requested_urls.lock().unwrap().push(url.to_owned());
            let entry = self
                .responses
                .lock()
                .unwrap()
                .get(url)
                .cloned()
                .unwrap_or_else(|| Err(format!("unexpected URL: {url}")));
            match entry {
                Ok(bytes) if bytes.len() > max_bytes => {
                    Err(UpdateError::ResponseTooLarge { limit: max_bytes })
                }
                Ok(bytes) => Ok(bytes),
                Err(msg) => Err(UpdateError::Network(msg)),
            }
        }
    }

    fn test_signing_key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn releases_url() -> String {
        tg_updater::github::releases_api_url("rustgram/rustgram")
    }

    #[test]
    fn update_state_machine_happy_path_check_download_verify_ready() {
        let dir = TempDir::new().unwrap();
        let working_dir = dir.path().join("workdir");
        let install_dir = dir.path().join("install");
        std::fs::create_dir_all(&working_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();

        let signing_key = test_signing_key(7);
        let verifying_key = signing_key.verifying_key();
        let target = Target::current();
        let next_ver = Version::new(0, 2, 0);
        let asset_name = format!("rustgram-0.2.0-{}.tupdate", target.asset_suffix());
        let download_url = format!("https://example.invalid/downloads/{asset_name}");

        let signed_pkg = pack_update(
            UpdateChannel::Stable,
            target,
            &next_ver,
            1_700_000_000,
            vec![
                ("rustgram.exe", b"new-binary-bytes".as_slice()),
                ("README.txt", b"hello update".as_slice()),
            ],
            &signing_key,
        )
        .unwrap();

        let releases_json = format!(
            r#"[{{
                "tag_name": "v0.2.0",
                "name": "Rustgram 0.2.0",
                "draft": false,
                "prerelease": false,
                "body": "Bug fixes",
                "assets": [{{
                    "name": "{asset_name}",
                    "browser_download_url": "{download_url}",
                    "size": {}
                }}]
            }}]"#,
            signed_pkg.len()
        );

        let fake = FakeHttpClient::default();
        fake.insert_ok(&releases_url(), releases_json.into_bytes());
        fake.insert_ok(&download_url, signed_pkg);

        let cfg = UpdateConfig {
            github_repo: "rustgram/rustgram".to_owned(),
            api_base_url: None,
            current_version: Version::new(0, 1, 0),
            allow_beta: false,
            target,
            working_dir: working_dir.clone(),
            install_dir: install_dir.clone(),
            verifying_key,
        };

        let mut machine = UpdateStateMachine::new(fake.clone(), cfg, true, false, 0);
        assert_eq!(machine.state(), &UpdateState::Idle);
        assert!(machine.is_periodic_check_due(1_700_000_100));

        let mut transitions = Vec::new();
        let ran = machine.poll_periodic(1_700_000_100, |snap| {
            transitions.push(snap.state.clone());
        });
        assert!(ran);
        assert_eq!(transitions.len(), 4);
        assert!(matches!(transitions[0], UpdateState::Checking));
        assert!(matches!(transitions[1], UpdateState::Available(_)));
        assert!(matches!(transitions[2], UpdateState::Downloading { .. }));
        assert!(matches!(
            transitions[3],
            UpdateState::Ready {
                ref version,
                ..
            } if *version == next_ver
        ));
        assert!(machine.snapshot().is_ready());
        assert!(tg_updater::checker::has_ready_update(&working_dir));

        // Subsequent periodic poll while Ready must NOT make network calls.
        let calls_before = fake.request_count();
        assert!(!machine.poll_periodic(1_800_000_000, |_| {}));
        assert_eq!(fake.request_count(), calls_before);

        // Fresh state machine started on the same working_dir detects the pre-staged update.
        let cfg2 = UpdateConfig {
            github_repo: "rustgram/rustgram".to_owned(),
            api_base_url: None,
            current_version: Version::new(0, 1, 0),
            allow_beta: false,
            target,
            working_dir: working_dir.clone(),
            install_dir,
            verifying_key,
        };
        let restored = UpdateStateMachine::new(fake, cfg2, true, false, 0);
        assert!(restored.snapshot().is_ready());
        assert_eq!(restored.snapshot().ready_version(), Some(&next_ver));
    }

    #[test]
    fn update_state_machine_up_to_date_invalid_sig_and_noupdate() {
        let dir = TempDir::new().unwrap();
        let working_dir = dir.path().join("wd");
        let install_dir = dir.path().join("inst");
        std::fs::create_dir_all(&working_dir).unwrap();
        std::fs::create_dir_all(&install_dir).unwrap();

        let signing_key = test_signing_key(11);
        let wrong_key = test_signing_key(99).verifying_key();
        let target = Target::current();
        let asset_name = format!("rustgram-0.3.0-{}.tupdate", target.asset_suffix());
        let download_url = format!("https://example.invalid/{asset_name}");

        // 1. UpToDate when release is same or older, and periodic interval is respected.
        let fake = FakeHttpClient::default();
        fake.insert_ok(
            &releases_url(),
            br#"[{"tag_name": "v0.1.0", "draft": false, "prerelease": false, "assets": []}]"#
                .to_vec(),
        );
        let cfg = UpdateConfig {
            github_repo: "rustgram/rustgram".to_owned(),
            api_base_url: None,
            current_version: Version::new(0, 1, 0),
            allow_beta: false,
            target,
            working_dir: working_dir.clone(),
            install_dir: install_dir.clone(),
            verifying_key: signing_key.verifying_key(),
        };
        let mut machine = UpdateStateMachine::new(fake.clone(), cfg, true, false, 0);
        machine.set_check_interval_secs(3600);
        assert!(machine.poll_periodic(10_000, |_| {}));
        assert_eq!(
            machine.state(),
            &UpdateState::UpToDate {
                checked_at_unix: 10_000
            }
        );
        // Not due yet at +1800s; due again at +3600s.
        assert!(!machine.is_periodic_check_due(11_800));
        assert!(machine.is_periodic_check_due(13_600));

        // 2. Bad signature transitions cleanly to Failed without leaving a ready marker.
        let bad_pkg = pack_update(
            UpdateChannel::Stable,
            target,
            &Version::new(0, 3, 0),
            1_700_000_000,
            vec![("rustgram.exe", b"tampered".as_slice())],
            &signing_key,
        )
        .unwrap();
        let fake_bad = FakeHttpClient::default();
        fake_bad.insert_ok(
            &releases_url(),
            format!(
                r#"[{{"tag_name":"v0.3.0","draft":false,"prerelease":false,"assets":[{{"name":"{asset_name}","browser_download_url":"{download_url}","size":{}}}]}}]"#,
                bad_pkg.len()
            )
            .into_bytes(),
        );
        fake_bad.insert_ok(&download_url, bad_pkg);
        let cfg_bad = UpdateConfig {
            github_repo: "rustgram/rustgram".to_owned(),
            api_base_url: None,
            current_version: Version::new(0, 1, 0),
            allow_beta: false,
            target,
            working_dir: working_dir.clone(),
            install_dir: install_dir.clone(),
            verifying_key: wrong_key,
        };
        let mut machine_bad = UpdateStateMachine::new(fake_bad, cfg_bad, true, false, 0);
        machine_bad.check_now(20_000, |_| {});
        assert!(matches!(
            machine_bad.state(),
            UpdateState::Failed {
                message,
                checked_at_unix: 20_000
            } if message.contains("signature")
        ));
        assert!(!tg_updater::checker::has_ready_update(&working_dir));

        // 3. `-noupdate` blocks both periodic and manual checks.
        let fake_disabled = FakeHttpClient::default();
        fake_disabled.insert_err(&releases_url(), "should never be called");
        let cfg_disabled = UpdateConfig {
            github_repo: "rustgram/rustgram".to_owned(),
            api_base_url: None,
            current_version: Version::new(0, 1, 0),
            allow_beta: false,
            target,
            working_dir,
            install_dir,
            verifying_key: signing_key.verifying_key(),
        };
        let mut disabled =
            UpdateStateMachine::new(fake_disabled.clone(), cfg_disabled, true, true, 0);
        assert_eq!(disabled.state(), &UpdateState::Disabled);
        assert!(!disabled.poll_periodic(30_000, |_| {}));
        disabled.check_now(30_000, |_| {});
        assert_eq!(disabled.state(), &UpdateState::Disabled);
        assert_eq!(fake_disabled.request_count(), 0);
    }

    #[test]
    fn updater_launch_plan_includes_finishprocess_and_forwarded_flags() {
        let mut parsed = tg_core::launcher::parse_arguments(&[
            "rustgram.exe".to_owned(),
            "-autostart".to_owned(),
            "-startintray".to_owned(),
            "-debug".to_owned(),
            "-key".to_owned(),
            "work".to_owned(),
        ]);
        parsed.launch_mode = LaunchMode::AutoStart;
        let plan = build_updater_launch_plan(
            Path::new(r"C:\Apps\Rustgram\rustgram.exe"),
            Path::new(r"D:\CustomData"),
            true,
            &parsed,
            Some(9876),
            Some(133_000_000_000),
        );
        assert_eq!(
            plan.updater_exe,
            PathBuf::from(r"C:\Apps\Rustgram\rustgram-updater.exe")
        );
        assert_eq!(
            plan.args,
            vec![
                "-update",
                "-finishprocess",
                "9876",
                "133000000000",
                "-autostart",
                "-debug",
                "-startintray",
                "-workdir",
                r"D:\CustomData",
                "-key",
                "work",
            ]
        );
    }

    // -------------------------------------------------------------------------
    // 4. Platform integration & cleanup tests with mock PlatformHooks
    // -------------------------------------------------------------------------

    #[derive(Default)]
    struct MockPlatformHooks {
        aumid_calls: Vec<String>,
        shortcut_calls: Vec<(PathBuf, PathBuf, String)>,
        register_calls: Vec<(PathBuf, Option<PathBuf>)>,
        stop_calls: Vec<String>,
        unregister_calls: Vec<(PathBuf, Option<PathBuf>)>,
        removed_start_menu: bool,
        removed_autostart: bool,
        removed_send_to: bool,
    }

    impl PlatformHooks for MockPlatformHooks {
        fn set_app_user_model_id(&mut self, aumid: &str) -> Result<(), String> {
            self.aumid_calls.push(aumid.to_owned());
            Ok(())
        }

        fn validate_or_create_start_menu_shortcut(
            &mut self,
            exe_path: &Path,
            working_dir: &Path,
            aumid: &str,
        ) -> Result<(), String> {
            self.shortcut_calls.push((
                exe_path.to_path_buf(),
                working_dir.to_path_buf(),
                aumid.to_owned(),
            ));
            Ok(())
        }

        fn register_url_schemes(
            &mut self,
            exe_path: &Path,
            custom_workdir: Option<&Path>,
        ) -> Result<(), String> {
            self.register_calls.push((
                exe_path.to_path_buf(),
                custom_workdir.map(Path::to_path_buf),
            ));
            Ok(())
        }

        fn stop_running_instance(&mut self, instance_name: &str) -> Result<bool, String> {
            self.stop_calls.push(instance_name.to_owned());
            Ok(true)
        }

        fn unregister_url_schemes(
            &mut self,
            exe_path: &Path,
            custom_workdir: Option<&Path>,
        ) -> Result<(), String> {
            self.unregister_calls.push((
                exe_path.to_path_buf(),
                custom_workdir.map(Path::to_path_buf),
            ));
            Ok(())
        }

        fn remove_start_menu_shortcut(&mut self, _exe_path: &Path) -> Result<bool, String> {
            self.removed_start_menu = true;
            Ok(true)
        }

        fn remove_autostart_shortcut(
            &mut self,
            _exe_path: &Path,
            _working_dir: &Path,
        ) -> Result<(), String> {
            self.removed_autostart = true;
            Ok(())
        }

        fn remove_send_to_shortcut(
            &mut self,
            _exe_path: &Path,
            _working_dir: &Path,
        ) -> Result<(), String> {
            self.removed_send_to = true;
            Ok(())
        }
    }

    #[test]
    fn platform_startup_and_cleanup_hooks_isolated() {
        let dir = TempDir::new().unwrap();
        let env = test_env(dir.path());
        let exe = env.exe_dir.join("rustgram.exe");
        // Registration is opt-in; this test exercises the registration hooks themselves.
        let mut prefs = StartupPrefs {
            take_url_schemes: true,
            ..StartupPrefs::default()
        };
        let plan = StartupPlan::build(["rustgram.exe"], &env, Some(&exe), &prefs);
        let mut hooks = MockPlatformHooks::default();

        assert!(apply_process_app_id(&plan, &mut hooks));
        assert_eq!(hooks.aumid_calls, vec![DEFAULT_APP_USER_MODEL_ID]);

        let report = apply_startup_integration(&plan, &exe, &mut prefs, &mut hooks);
        assert!(report.start_menu_shortcut_ok);
        assert!(report.url_schemes_registered);
        assert!(prefs.url_schemes_registered);
        assert_eq!(hooks.shortcut_calls.len(), 1);
        assert_eq!(hooks.register_calls.len(), 1);

        // Create fake log_start files in the temp working dir and run `-cleanup`.
        std::fs::create_dir_all(&plan.resolved_working_dir.path).unwrap();
        std::fs::write(plan.resolved_working_dir.path.join("log_start0.txt"), "a").unwrap();
        std::fs::write(plan.resolved_working_dir.path.join("log_start3.txt"), "b").unwrap();

        let cleanup_plan =
            StartupPlan::build(["rustgram.exe", "-cleanup"], &env, Some(&exe), &prefs);
        let cleanup_report = run_cleanup(&cleanup_plan, &exe, &mut hooks);
        assert!(cleanup_report.stopped_running_instance);
        assert!(cleanup_report.schemes_unregistered);
        assert!(cleanup_report.start_menu_shortcut_removed);
        assert!(cleanup_report.autostart_shortcut_removed);
        assert!(cleanup_report.send_to_shortcut_removed);
        assert_eq!(cleanup_report.start_logs_deleted, 2);
        assert!(
            !plan
                .resolved_working_dir
                .path
                .join("log_start0.txt")
                .exists()
        );
    }

    // -------------------------------------------------------------------------
    // 5. Crash recovery & window icon tests
    // -------------------------------------------------------------------------

    #[test]
    fn crash_recovery_summary_and_window_icon() {
        let dir = TempDir::new().unwrap();
        let dumps_dir = dir.path().join("tdata").join("dumps");
        let dump = b"Binary: rustgram.exe\nVersion: 1000 (0.1.0)\nOS: Windows 10.0.22631 (x86_64)\nThread: main (ThreadId(1), tid=42)\nCrashedAt: 22.11.2026 05:12:34\n\nPanic: index out of bounds\nLocation: crates/tg-app/src/app.rs:42:9\n\nBacktrace:\nframe 0\n";
        let mut state = CrashRecoveryState::from_dump(dump, dumps_dir.clone());
        assert!(state.has_dump_text());
        assert_eq!(state.headline(), "Last time Rustgram crashed");
        assert!(!state.upload_config.is_active());
        assert_eq!(
            state.summary,
            CrashSummary {
                version: Some("1000 (0.1.0)".to_owned()),
                os: Some("Windows 10.0.22631 (x86_64)".to_owned()),
                thread: Some("main (ThreadId(1), tid=42)".to_owned()),
                crashed_at: Some("22.11.2026 05:12:34".to_owned()),
                panic_reason: Some("index out of bounds".to_owned()),
                location: Some("crates/tg-app/src/app.rs:42:9".to_owned()),
            }
        );

        // Open report folder hook creates dumps_dir if missing and invokes opener.
        let mut opened = None;
        state
            .open_report_folder_with(|p| {
                opened = Some(p.to_path_buf());
                Ok(())
            })
            .unwrap();
        assert_eq!(opened.as_deref(), Some(dumps_dir.as_path()));
        assert!(dumps_dir.is_dir());
        assert!(state.status_notice.is_some());

        // Native Windows exception report parses cleanly into CrashSummary.
        let seh_dump = b"Version: 1000 (0.1.0)\nOS: Windows 10.0.22631\nThread: worker (ThreadId(2))\nException: EXCEPTION_ACCESS_VIOLATION (0xC0000005)\n";
        let seh_state = CrashRecoveryState::from_dump(seh_dump, dumps_dir.clone());
        assert_eq!(
            seh_state.summary.panic_reason.as_deref(),
            Some("EXCEPTION_ACCESS_VIOLATION (0xC0000005)")
        );

        let empty = CrashRecoveryState::from_dump(b"", dumps_dir);
        assert!(!empty.has_dump_text());
        assert_eq!(
            empty.headline(),
            "Last time Rustgram was not closed properly."
        );

        // Window icon renders from the shared SVG into valid RGBA data.
        let icon = load_window_icon();
        assert_eq!(icon.width, DEFAULT_WINDOW_ICON_SIZE);
        assert_eq!(icon.height, DEFAULT_WINDOW_ICON_SIZE);
        assert_eq!(
            icon.rgba.len(),
            (DEFAULT_WINDOW_ICON_SIZE * DEFAULT_WINDOW_ICON_SIZE * 4) as usize
        );
        // Ensure the rendered icon is not all-transparent.
        assert!(icon.rgba.as_chunks::<4>().0.iter().any(|px| px[3] > 0));
        assert!(render_svg_to_icon_data(APP_ICON_SVG, 32).is_ok());
    }

    #[test]
    fn controller_updater_accessors_and_activation_hooks() {
        use crate::stand_in::mock_session::{MockSession, session_peers};
        use crate::stand_in::settings::ShellSettings;
        use crate::window::controller::WindowController;
        use crate::window::section::{SectionMemento, SettingsPage};

        use super::updater::{UpdateSnapshot, UpdateUiAction};

        let mut ctrl = WindowController::with_session(
            MockSession::new(true),
            ShellSettings::default(),
            std::env::temp_dir().join("rustgram_startup_ctrl_hook_test.json"),
        );

        assert_eq!(ctrl.update_state(), &UpdateState::Idle);
        let ready_snap = UpdateSnapshot {
            state: UpdateState::Ready {
                version: Version::new(0, 2, 0),
                staging_dir: PathBuf::from(r"D:\tmp\tupdates\temp"),
                updater_promoted: true,
            },
            ..UpdateSnapshot::default()
        };
        ctrl.set_update_snapshot(ready_snap.clone());
        assert_eq!(ctrl.update_snapshot(), &ready_snap);
        assert!(ctrl.update_snapshot().is_ready());
        assert_eq!(
            ctrl.update_snapshot().ready_version(),
            Some(&Version::new(0, 2, 0))
        );

        ctrl.request_update_check();
        assert_eq!(ctrl.take_update_action(), Some(UpdateUiAction::CheckNow));
        ctrl.request_restart_to_update();
        assert_eq!(
            ctrl.take_update_action(),
            Some(UpdateUiAction::RestartToUpdate)
        );
        assert_eq!(ctrl.take_update_action(), None);

        // `-tosettings` opens SettingsPage::Main.
        ctrl.open_settings();
        assert_eq!(
            ctrl.session().unwrap().main_section(),
            Some(&SectionMemento::Settings {
                page: SettingsPage::Main,
            })
        );

        // Incoming activation with `tg://resolve?domain=rustgram_log` and `-sendpath` routes cleanly.
        let egui_ctx = egui::Context::default();
        ctrl.handle_incoming_activation(
            IncomingActivation {
                urls: vec!["tg://resolve?domain=rustgram_log".to_owned()],
                send_paths: vec![PathBuf::from(r"C:\tmp\sample.png")],
                show: true,
                quit: false,
            },
            &egui_ctx,
        );
        assert_eq!(
            ctrl.session().unwrap().active_peer(),
            session_peers::CHANNEL_DEV_LOG
        );
    }
}
