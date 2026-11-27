//! The saved "Default interface scale" and the restart it needs (TGB-3111).
//!
//! tdesktop reads `dbiScalePercent` from `tdata/settingss` at startup and applies it only when no
//! `-scale` was given (`storage_settings_scheme.cpp`: "If cConfigScale() has value then it was
//! set via command line"). Changing the scale in Settings asks "Restart now?" and
//! `Core::Restart` quits and relaunches the app (`Launcher::exec` → `launchUpdater(JustRelaunch)`
//! with `-noupdate`, plus `-tosettings` because `cRestartingToSettings` is set).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use tg_core::launcher::{RelaunchAction, build_relaunch_args};
use tg_core::{GlobalConfig, SCALE_AUTO, SCALE_MAX, SCALE_MIN};

use super::plan::StartupPlan;

static RELAUNCH_REQUESTED: AtomicBool = AtomicBool::new(false);

/// The scale saved in `<working_dir>/tdata/settingss`, or `None` when there is no readable
/// settings file. Reads only: a missing file is not created.
#[must_use]
pub fn read_saved_scale(working_dir: &Path) -> Option<i32> {
    let tdata = working_dir.join("tdata");
    if !tdata.join("settingss").is_file() {
        return None;
    }
    match tg_storage::LocalStorage::open_or_init(&tdata) {
        Ok(storage) => Some(storage.settings().scale_percent),
        Err(err) => {
            tracing::warn!(%err, "could not read the saved interface scale");
            None
        }
    }
}

/// Applies a saved scale the way `dbiScalePercent` does: only when `-scale` left the config on
/// `SCALE_AUTO`, then `SetScaleChecked`: `style::CheckScale` clamps a set value into
/// `kScaleMin..=kScaleMax` (the window clamps it further for its pixel ratio).
pub fn apply_saved_scale(config: &mut GlobalConfig, saved: Option<i32>) {
    let Some(saved) = saved else {
        return;
    };
    if config.config_scale == SCALE_AUTO {
        config.config_scale = if saved == SCALE_AUTO {
            SCALE_AUTO
        } else {
            saved.clamp(SCALE_MIN, SCALE_MAX)
        };
        config.validate_scale();
    }
}

/// Asks [`relaunch_after_exit`] to start the app again once the window has closed.
pub fn request_relaunch() {
    RELAUNCH_REQUESTED.store(true, Ordering::SeqCst);
}

fn take_relaunch_request() -> bool {
    RELAUNCH_REQUESTED.swap(false, Ordering::SeqCst)
}

/// What a restart runs: `exe` with these arguments in `working_dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelaunchCommand {
    /// The running executable.
    pub exe: PathBuf,
    /// Its arguments (no `argv[0]`).
    pub args: Vec<String>,
    /// The working directory for the new process.
    pub working_dir: PathBuf,
}

/// The command `Core::Restart` runs for this launch: tdesktop's `JustRelaunch` arguments
/// (`-noupdate`, `-tosettings`, and the `-workdir`, `-key`, `-testmode`, `-debug`, `-many`,
/// `-autostart`, `-startintray` this launch had) plus Rustgram's own `--demo` / `--live` /
/// `--start-main` / `--settings`, so a demo or scratch run restarts as the same kind of run.
/// `-scale`, start links and `-sendpath` are not repeated: the saved scale applies and nothing
/// is opened or sent twice.
#[must_use]
pub fn relaunch_command(plan: &StartupPlan, exe: &Path) -> RelaunchCommand {
    let mut config = plan.global_config.clone();
    config.restarting_to_settings = true;
    let mut args = build_relaunch_args(
        &config,
        RelaunchAction::JustRelaunch,
        plan.debug_logging,
        false,
        false,
    );
    if plan.demo_mode {
        args.push("--demo".to_owned());
    }
    if plan.live_mode {
        args.push("--live".to_owned());
    }
    if plan.start_signed_in {
        args.push("--start-main".to_owned());
    }
    if plan.settings_path != plan.resolved_working_dir.path.join("shell_state.json") {
        args.push("--settings".to_owned());
        args.push(plan.settings_path.to_string_lossy().into_owned());
    }
    RelaunchCommand {
        exe: exe.to_path_buf(),
        args,
        working_dir: plan.resolved_working_dir.path.clone(),
    }
}

/// Starts `command` if a restart was requested. Call after the window has closed and the
/// single-instance guard is released, or the new process would hand its arguments to this one.
pub fn relaunch_after_exit(command: &RelaunchCommand) {
    if !take_relaunch_request() {
        return;
    }
    let mut cmd = Command::new(&command.exe);
    cmd.args(&command.args);
    if command.working_dir.is_dir() {
        cmd.current_dir(&command.working_dir);
    }
    if let Err(err) = cmd.spawn() {
        tracing::error!(%err, "could not restart Rustgram");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_core::launcher::WorkDirEnv;

    use crate::startup::plan::StartupPrefs;

    fn plan(args: &[&str], dir: &Path) -> StartupPlan {
        let env = WorkDirEnv {
            exe_dir: dir.to_path_buf(),
            app_data_dir: Some(dir.join("appdata")),
            current_dir: dir.to_path_buf(),
        };
        let mut argv = vec!["rustgram.exe"];
        argv.extend_from_slice(args);
        StartupPlan::build(
            argv,
            &env,
            Some(&dir.join("rustgram.exe")),
            &StartupPrefs::default(),
        )
    }

    fn write_saved_scale(working_dir: &Path, scale: i32) {
        let tdata = working_dir.join("tdata");
        std::fs::create_dir_all(&tdata).expect("tdata");
        let mut storage = tg_storage::LocalStorage::open_or_init(&tdata).expect("open");
        storage.settings_mut().scale_percent = scale;
        storage.write_settings().expect("write");
    }

    #[test]
    fn saved_scale_is_read_from_settings_and_a_missing_file_stays_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(read_saved_scale(dir.path()), None);
        assert!(
            !dir.path().join("tdata").exists(),
            "reading must not create tdata"
        );
        write_saved_scale(dir.path(), 150);
        assert_eq!(read_saved_scale(dir.path()), Some(150));
    }

    #[test]
    fn corrupt_settings_file_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tdata = dir.path().join("tdata");
        std::fs::create_dir_all(&tdata).expect("tdata");
        std::fs::write(tdata.join("settingss"), b"not a settings file").expect("write");
        assert_eq!(read_saved_scale(dir.path()), None);
    }

    #[test]
    fn saved_scale_applies_only_without_a_scale_option() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut auto = plan(&[], dir.path()).global_config;
        assert_eq!(auto.config_scale, SCALE_AUTO);
        apply_saved_scale(&mut auto, Some(150));
        assert_eq!(auto.config_scale, 150);

        // `-scale 125` wins over the saved 150.
        let mut cli = plan(&["-scale", "125"], dir.path()).global_config;
        apply_saved_scale(&mut cli, Some(150));
        assert_eq!(cli.config_scale, 125);

        // Out-of-range saved values are clamped (`SetScaleChecked`); `None` changes nothing.
        let mut big = plan(&[], dir.path()).global_config;
        apply_saved_scale(&mut big, Some(10));
        assert_eq!(big.config_scale, SCALE_MIN);
        let mut huge = plan(&[], dir.path()).global_config;
        apply_saved_scale(&mut huge, Some(900));
        assert_eq!(huge.config_scale, SCALE_MAX);
        let mut none = plan(&[], dir.path()).global_config;
        apply_saved_scale(&mut none, None);
        assert_eq!(none.config_scale, SCALE_AUTO);
    }

    #[test]
    fn relaunch_keeps_the_workdir_and_demo_and_drops_scale_and_links() {
        let dir = tempfile::tempdir().expect("tempdir");
        let work = dir.path().join("scratch");
        let work_s = work.to_string_lossy().into_owned();
        let p = plan(
            &[
                "-workdir",
                &work_s,
                "--demo",
                "-testmode",
                "-scale",
                "200",
                "-sendpath",
                "file.txt",
                "tg://resolve?domain=example",
            ],
            dir.path(),
        );
        let cmd = relaunch_command(&p, &dir.path().join("rustgram.exe"));
        let workdir_at = cmd
            .args
            .iter()
            .position(|a| a == "-workdir")
            .expect("-workdir kept");
        assert_eq!(
            Path::new(&cmd.args[workdir_at + 1]),
            p.resolved_working_dir.path.as_path()
        );
        for kept in ["--demo", "-testmode", "-noupdate", "-tosettings"] {
            assert!(
                cmd.args.iter().any(|a| a == kept),
                "{kept} in {:?}",
                cmd.args
            );
        }
        for dropped in ["-scale", "200", "-sendpath", "file.txt"] {
            assert!(
                !cmd.args.iter().any(|a| a == dropped),
                "{dropped} in {:?}",
                cmd.args
            );
        }
        assert!(!cmd.args.iter().any(|a| a.starts_with("tg://")));
        assert_eq!(cmd.working_dir, p.resolved_working_dir.path);
    }

    #[test]
    fn relaunch_runs_only_when_requested() {
        // A command that cannot start: the request is consumed and nothing panics.
        let cmd = RelaunchCommand {
            exe: PathBuf::from("does-not-exist-rustgram-test.exe"),
            args: Vec::new(),
            working_dir: PathBuf::new(),
        };
        relaunch_after_exit(&cmd);
        request_relaunch();
        relaunch_after_exit(&cmd);
        assert!(!take_relaunch_request());
    }
}
