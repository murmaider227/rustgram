//! Windows platform integration for startup and `-cleanup` uninstall handling.
//!
//! Port origin: `Telegram/SourceFiles/platform/win/specific_win.cpp` (`Platform::start`,
//! `psDoCleanup`, `DeleteMyStartLogs`),
//! `Telegram/SourceFiles/platform/win/windows_app_user_model_id.cpp`, and
//! `Telegram/SourceFiles/core/application.cpp` (`autoRegisterUrlScheme`).

use std::fs;
use std::path::{Path, PathBuf};

use tg_platform::app_id::{
    DEFAULT_SHORTCUT_NAME, ShortcutConfig, cleanup_shortcut_in, set_current_process_app_id,
    start_menu_programs_dir, validate_or_create_shortcut_in,
};
use tg_platform::registration::{
    AppLinkConfig, RegistryRoot, register_default_schemes, send_to_folder_dir, set_autostart_in,
    set_send_to_in, startup_folder_dir, unregister_default_schemes,
};

use super::ipc::stop_running_instance;
use super::plan::{StartupPlan, StartupPrefs};

/// Summary of OS-integration actions performed during startup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartupPlatformReport {
    /// Whether `SetCurrentProcessExplicitAppUserModelID` succeeded.
    pub app_id_set: bool,
    /// Whether the Start-menu `.lnk` shortcut was validated or created.
    pub start_menu_shortcut_ok: bool,
    /// Whether `tg://` and `tonsite://` schemes were registered on this run.
    pub url_schemes_registered: bool,
}

/// Summary of cleanup actions performed for `-cleanup`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanupReport {
    /// Whether a running instance was found and asked to quit.
    pub stopped_running_instance: bool,
    /// Whether `tg://` / `tonsite://` URL scheme unregistration succeeded.
    pub schemes_unregistered: bool,
    /// Whether the Start-menu shortcut was removed.
    pub start_menu_shortcut_removed: bool,
    /// Whether the Startup autostart shortcut was removed.
    pub autostart_shortcut_removed: bool,
    /// Whether the Explorer "Send to" shortcut was removed.
    pub send_to_shortcut_removed: bool,
    /// Number of `log_start[0-9].txt` files removed from the working directory.
    pub start_logs_deleted: usize,
}

/// Abstraction over OS integration side effects so unit tests never touch the real Windows
/// Registry, Start menu, Startup folder, SendTo folder, or `%APPDATA%`.
pub trait PlatformHooks {
    /// Sets the explicit Windows `AppUserModelID` for the current process.
    fn set_app_user_model_id(&mut self, aumid: &str) -> Result<(), String>;

    /// Validates or creates the Start-menu `.lnk` shortcut for toast notifications.
    fn validate_or_create_start_menu_shortcut(
        &mut self,
        exe_path: &Path,
        working_dir: &Path,
        aumid: &str,
    ) -> Result<(), String>;

    /// Registers `tg://` and `tonsite://` URL schemes for this executable.
    fn register_url_schemes(
        &mut self,
        exe_path: &Path,
        custom_workdir: Option<&Path>,
    ) -> Result<(), String>;

    /// Stops any already-running instance for `instance_name` (for `-cleanup`).
    fn stop_running_instance(&mut self, instance_name: &str) -> Result<bool, String>;

    /// Unregisters `tg://` and `tonsite://` URL schemes owned by this executable.
    fn unregister_url_schemes(
        &mut self,
        exe_path: &Path,
        custom_workdir: Option<&Path>,
    ) -> Result<(), String>;

    /// Removes the Start-menu `.lnk` shortcut if it points to `exe_path`.
    fn remove_start_menu_shortcut(&mut self, exe_path: &Path) -> Result<bool, String>;

    /// Removes the Windows Startup autostart `.lnk` shortcut.
    fn remove_autostart_shortcut(
        &mut self,
        exe_path: &Path,
        working_dir: &Path,
    ) -> Result<(), String>;

    /// Removes the Explorer "Send to" `.lnk` shortcut.
    fn remove_send_to_shortcut(
        &mut self,
        exe_path: &Path,
        working_dir: &Path,
    ) -> Result<(), String>;
}

/// Real Windows OS integration hooks backed by `tg-platform`.
///
/// Only constructed in real application runs (`main.rs`), never in automated unit tests.
#[derive(Debug, Default)]
pub struct RealWindowsPlatform;

impl PlatformHooks for RealWindowsPlatform {
    fn set_app_user_model_id(&mut self, aumid: &str) -> Result<(), String> {
        set_current_process_app_id(aumid).map_err(|e| e.to_string())
    }

    fn validate_or_create_start_menu_shortcut(
        &mut self,
        exe_path: &Path,
        working_dir: &Path,
        aumid: &str,
    ) -> Result<(), String> {
        let programs_dir = start_menu_programs_dir().map_err(|e| e.to_string())?;
        let mut cfg =
            ShortcutConfig::for_executable(exe_path.to_path_buf(), working_dir.to_path_buf());
        cfg.app_user_model_id = aumid.to_owned();
        validate_or_create_shortcut_in(&programs_dir, &cfg)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn register_url_schemes(
        &mut self,
        exe_path: &Path,
        custom_workdir: Option<&Path>,
    ) -> Result<(), String> {
        register_default_schemes(&RegistryRoot::hkcu(), exe_path, custom_workdir)
            .map_err(|e| e.to_string())
    }

    fn stop_running_instance(&mut self, instance_name: &str) -> Result<bool, String> {
        stop_running_instance(instance_name).map_err(|e| e.to_string())
    }

    fn unregister_url_schemes(
        &mut self,
        exe_path: &Path,
        custom_workdir: Option<&Path>,
    ) -> Result<(), String> {
        unregister_default_schemes(&RegistryRoot::hkcu(), exe_path, custom_workdir)
            .map_err(|e| e.to_string())
    }

    fn remove_start_menu_shortcut(&mut self, exe_path: &Path) -> Result<bool, String> {
        let programs_dir = start_menu_programs_dir().map_err(|e| e.to_string())?;
        cleanup_shortcut_in(&programs_dir, DEFAULT_SHORTCUT_NAME, exe_path)
            .map_err(|e| e.to_string())
    }

    fn remove_autostart_shortcut(
        &mut self,
        exe_path: &Path,
        working_dir: &Path,
    ) -> Result<(), String> {
        let startup_dir = startup_folder_dir().map_err(|e| e.to_string())?;
        let cfg = AppLinkConfig::for_executable(exe_path.to_path_buf(), working_dir.to_path_buf());
        set_autostart_in(&startup_dir, &cfg, false).map_err(|e| e.to_string())
    }

    fn remove_send_to_shortcut(
        &mut self,
        exe_path: &Path,
        working_dir: &Path,
    ) -> Result<(), String> {
        let send_to_dir = send_to_folder_dir().map_err(|e| e.to_string())?;
        let cfg = AppLinkConfig::for_executable(exe_path.to_path_buf(), working_dir.to_path_buf());
        set_send_to_in(&send_to_dir, &cfg, false).map_err(|e| e.to_string())
    }
}

/// Applies early AppUserModelID setup before creating windows or toast notifications
/// (`Platform::start` in `specific_win.cpp:379-381`).
pub fn apply_process_app_id(plan: &StartupPlan, hooks: &mut dyn PlatformHooks) -> bool {
    match hooks.set_app_user_model_id(&plan.app_user_model_id) {
        Ok(()) => {
            tracing::info!(aumid = %plan.app_user_model_id, "Set Windows AppUserModelID");
            true
        }
        Err(err) => {
            tracing::warn!(%err, aumid = %plan.app_user_model_id, "Could not set AppUserModelID");
            false
        }
    }
}

/// Applies post-single-instance OS integration: Start-menu shortcut validation and optional
/// first-run `tg://` / `tonsite://` protocol handler registration.
pub fn apply_startup_integration(
    plan: &StartupPlan,
    exe_path: &Path,
    prefs: &mut StartupPrefs,
    hooks: &mut dyn PlatformHooks,
) -> StartupPlatformReport {
    let mut report = StartupPlatformReport::default();

    if plan.validate_start_menu_shortcut {
        match hooks.validate_or_create_start_menu_shortcut(
            exe_path,
            &plan.resolved_working_dir.path,
            &plan.app_user_model_id,
        ) {
            Ok(()) => report.start_menu_shortcut_ok = true,
            Err(err) => {
                tracing::warn!(%err, "Could not validate/create Start-menu shortcut");
            }
        }
    }

    if plan.register_url_schemes_now {
        let custom_wd = plan
            .resolved_working_dir
            .is_custom
            .then_some(plan.resolved_working_dir.path.as_path());
        match hooks.register_url_schemes(exe_path, custom_wd) {
            Ok(()) => {
                report.url_schemes_registered = true;
                prefs.url_schemes_registered = true;
                tracing::info!("Registered tg:// and tonsite:// URL protocol handlers");
            }
            Err(err) => {
                tracing::warn!(%err, "Could not register tg:// / tonsite:// URL handlers");
            }
        }
    }

    report
}

/// Executes the `-cleanup` uninstall sequence (`Sandbox::start` + `psDoCleanup` in
/// `specific_win.cpp:262-271`):
/// 1. Stops any running instance for this working directory.
/// 2. Unregisters `tg://` and `tonsite://` URL protocol handlers.
/// 3. Removes Start-menu, Startup (autostart), and Explorer "Send to" shortcuts.
/// 4. Removes leftover `log_start[0-9].txt` files from `working_dir`.
pub fn run_cleanup(
    plan: &StartupPlan,
    exe_path: &Path,
    hooks: &mut dyn PlatformHooks,
) -> CleanupReport {
    let mut report = CleanupReport::default();

    match hooks.stop_running_instance(&plan.single_instance_name) {
        Ok(stopped) => report.stopped_running_instance = stopped,
        Err(err) => tracing::warn!(%err, "Cleanup: failed to stop running instance"),
    }

    let custom_wd = plan
        .resolved_working_dir
        .is_custom
        .then_some(plan.resolved_working_dir.path.as_path());
    match hooks.unregister_url_schemes(exe_path, custom_wd) {
        Ok(()) => report.schemes_unregistered = true,
        Err(err) => tracing::warn!(%err, "Cleanup: failed to unregister URL schemes"),
    }

    match hooks.remove_start_menu_shortcut(exe_path) {
        Ok(removed) => report.start_menu_shortcut_removed = removed,
        Err(err) => tracing::warn!(%err, "Cleanup: failed to remove Start-menu shortcut"),
    }

    match hooks.remove_autostart_shortcut(exe_path, &plan.resolved_working_dir.path) {
        Ok(()) => report.autostart_shortcut_removed = true,
        Err(err) => tracing::warn!(%err, "Cleanup: failed to remove autostart shortcut"),
    }

    match hooks.remove_send_to_shortcut(exe_path, &plan.resolved_working_dir.path) {
        Ok(()) => report.send_to_shortcut_removed = true,
        Err(err) => tracing::warn!(%err, "Cleanup: failed to remove SendTo shortcut"),
    }

    report.start_logs_deleted = delete_start_logs(&plan.resolved_working_dir.path);
    report
}

/// Deletes `log_start0.txt..log_start9.txt` inside `working_dir` (`DeleteMyStartLogs`).
pub fn delete_start_logs(working_dir: &Path) -> usize {
    let mut deleted = 0usize;
    for i in 0..10 {
        let path: PathBuf = working_dir.join(format!("log_start{i}.txt"));
        if path.is_file() && fs::remove_file(&path).is_ok() {
            deleted += 1;
        }
    }
    deleted
}
