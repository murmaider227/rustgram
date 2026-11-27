//! Builds and spawns the `rustgram-updater` invocation the app uses to install a staged update.
//!
//! Port of `Launcher::launchUpdater` and `Launcher::launch` in
//! `Telegram/SourceFiles/platform/win/launcher_win.cpp`. tdesktop starts `Updater.exe` with the
//! app's working directory (`cWorkingDir()`, `nativeWorkingDir`) as the process current directory,
//! because the updater finds `tupdates` relative to its current directory. `-workdir` is passed only
//! for a custom working directory, and only so the updater can forward it to the relaunched app.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use crate::UPDATER_EXE_NAME;
use crate::checker::{UpdaterLaunchOptions, build_updater_launch_args};

/// Everything needed to spawn `rustgram-updater.exe` for "Restart to update".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdaterLaunchPlan {
    /// `<install_dir>/rustgram-updater.exe`.
    pub updater_exe: PathBuf,
    /// Directory holding the running app executable; the updater replaces files here.
    pub install_dir: PathBuf,
    /// Current directory for the updater process: the app's working directory, where the update
    /// is staged in `tupdates` (tdesktop: `ShellExecute(..., nativeWorkingDir)`).
    pub current_dir: PathBuf,
    /// Command-line arguments produced by [`build_updater_launch_args`].
    pub args: Vec<String>,
}

/// Builds the updater invocation for the app at `exe_path` whose working directory is
/// `working_dir`.
///
/// `opts.workdir` and `opts.exe_name` are filled here: `-workdir` only for a custom working
/// directory (`is_custom_workdir`), and the executable name from `exe_path`.
#[must_use]
pub fn plan_updater_launch(
    exe_path: &Path,
    working_dir: &Path,
    is_custom_workdir: bool,
    mut opts: UpdaterLaunchOptions,
) -> UpdaterLaunchPlan {
    let install_dir = exe_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    opts.workdir = is_custom_workdir.then(|| working_dir.to_path_buf());
    opts.exe_name = exe_path
        .file_name()
        .and_then(|s| s.to_str())
        .map(ToOwned::to_owned);
    UpdaterLaunchPlan {
        updater_exe: install_dir.join(UPDATER_EXE_NAME),
        install_dir,
        current_dir: working_dir.to_path_buf(),
        args: build_updater_launch_args(&opts),
    }
}

/// Spawns the updater described by `plan` and returns its child handle.
pub fn spawn_updater(plan: &UpdaterLaunchPlan) -> Result<Child, String> {
    if !plan.updater_exe.is_file() {
        return Err(format!(
            "updater executable not found at '{}'",
            plan.updater_exe.display()
        ));
    }
    if !plan.current_dir.is_dir() {
        return Err(format!(
            "working directory '{}' does not exist",
            plan.current_dir.display()
        ));
    }
    Command::new(&plan.updater_exe)
        .args(&plan.args)
        .current_dir(&plan.current_dir)
        .spawn()
        .map_err(|e| format!("failed to launch {}: {e}", plan.updater_exe.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn default_working_dir_is_current_dir_without_workdir_flag() {
        let plan = plan_updater_launch(
            Path::new(r"C:\Apps\Rustgram\rustgram.exe"),
            Path::new(r"C:\Users\u\AppData\Roaming\Rustgram"),
            false,
            UpdaterLaunchOptions::default(),
        );
        assert_eq!(plan.install_dir, PathBuf::from(r"C:\Apps\Rustgram"));
        assert_eq!(
            plan.updater_exe,
            PathBuf::from(r"C:\Apps\Rustgram\rustgram-updater.exe")
        );
        assert_eq!(
            plan.current_dir,
            PathBuf::from(r"C:\Users\u\AppData\Roaming\Rustgram")
        );
        assert_eq!(plan.args, vec!["-update"]);
    }

    #[test]
    #[cfg(windows)]
    fn custom_working_dir_is_current_dir_and_forwarded() {
        let plan = plan_updater_launch(
            Path::new(r"C:\Apps\Rustgram\rustgram-beta.exe"),
            Path::new(r"D:\Profiles\work"),
            true,
            UpdaterLaunchOptions::default(),
        );
        assert_eq!(plan.current_dir, PathBuf::from(r"D:\Profiles\work"));
        assert_eq!(
            plan.args,
            vec![
                "-update",
                "-workdir",
                r"D:\Profiles\work",
                "-exename",
                "rustgram-beta.exe"
            ]
        );
    }
}
