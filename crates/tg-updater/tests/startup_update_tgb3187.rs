//! TGB-3187: the app installs a ready update at startup, so an update that cannot be installed must
//! never keep it from starting. The updater relaunches the installed app with `-noupdate` even when
//! the copy fails, like tdesktop's `updater_win.cpp` (`ShellExecute` runs whatever `update()`
//! returned), and the staged update stays for "Restart to update".
//!
//! No case installs a package with a version file, so no uninstall registry key is written.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tempfile::TempDir;

use tg_updater::apply::{
    AppLauncher, ApplyRetryPolicy, ProcessWaiter, parse_args, run_updater_with_policy,
};
use tg_updater::checker::has_ready_update;
use tg_updater::startup_attempt::{record_startup_attempt, startup_attempt_path};
use tg_updater::version::Version;
use tg_updater::{DEFAULT_EXE_NAME, TUPDATES_DIR_NAME, UpdateError};

#[derive(Default)]
struct RecordingLauncher {
    launches: Mutex<Vec<(PathBuf, Vec<String>)>>,
}

impl AppLauncher for RecordingLauncher {
    fn launch(&self, exe: &Path, args: &[String], _cwd: &Path) -> Result<(), UpdateError> {
        self.launches
            .lock()
            .expect("launches")
            .push((exe.to_path_buf(), args.to_vec()));
        Ok(())
    }
}

struct NoWait;

impl ProcessWaiter for NoWait {
    fn wait_for_exit(&self, _pid: u32, _creation_time: u64) -> Result<(), UpdateError> {
        Ok(())
    }
}

/// Stages `<working_dir>/tupdates/temp` with `files` and the `ready` marker (no version file).
fn stage(working_dir: &Path, files: &[(&str, &[u8])]) -> PathBuf {
    let temp = working_dir.join(TUPDATES_DIR_NAME).join("temp");
    fs::create_dir_all(&temp).expect("temp");
    for (name, bytes) in files {
        fs::write(temp.join(name), bytes).expect("staged file");
    }
    fs::write(temp.join("ready"), b"1").expect("ready");
    temp
}

/// The old updater returned the copy error before its relaunch step, so a startup install that
/// failed left the user with no app at all.
#[test]
fn a_failed_install_still_relaunches_the_installed_app_with_noupdate() {
    let tmp = TempDir::new().expect("tmp");
    let working_dir = tmp.path().join("profile");
    let install = tmp.path().join("install");
    fs::create_dir_all(&install).expect("install");
    fs::write(install.join(DEFAULT_EXE_NAME), b"installed app").expect("app");
    // A folder where the package has a file: the copy fails on every platform.
    fs::create_dir_all(install.join("blocked.dll")).expect("blocking folder");
    stage(
        &working_dir,
        &[(DEFAULT_EXE_NAME, b"new app"), ("blocked.dll", b"new dll")],
    );

    let args = parse_args([
        "-update",
        "-workdir",
        working_dir.to_str().expect("utf-8 path"),
    ]);
    let launcher = RecordingLauncher::default();
    let result = run_updater_with_policy(
        &args,
        &working_dir,
        &install,
        &NoWait,
        &launcher,
        ApplyRetryPolicy::fast(),
    );

    assert!(
        matches!(
            result,
            Err(UpdateError::ApplyFailed {
                rolled_back: true,
                ..
            })
        ),
        "got {result:?}"
    );
    let launches = launcher.launches.lock().expect("launches");
    assert_eq!(launches.len(), 1, "the installed app must be started again");
    assert_eq!(launches[0].0, install.join(DEFAULT_EXE_NAME));
    assert_eq!(launches[0].1[0], "-noupdate");
    assert!(
        launches[0].1.windows(2).any(|pair| pair[0] == "-workdir"),
        "-workdir is forwarded: {:?}",
        launches[0].1
    );
    assert_eq!(
        fs::read(install.join(DEFAULT_EXE_NAME)).expect("app"),
        b"installed app",
        "the failed install is rolled back"
    );
    assert!(
        has_ready_update(&working_dir),
        "the update stays staged for \"Restart to update\""
    );
}

/// `-justupdate` (the release workflow's update-path check) still gets no relaunch.
#[test]
fn a_failed_install_with_justupdate_relaunches_nothing() {
    let tmp = TempDir::new().expect("tmp");
    let working_dir = tmp.path().join("profile");
    let install = tmp.path().join("install");
    fs::create_dir_all(install.join("blocked.dll")).expect("blocking folder");
    stage(&working_dir, &[("blocked.dll", b"new dll")]);

    let args = parse_args(["-update", "-justupdate"]);
    let launcher = RecordingLauncher::default();
    let result = run_updater_with_policy(
        &args,
        &working_dir,
        &install,
        &NoWait,
        &launcher,
        ApplyRetryPolicy::fast(),
    );
    assert!(matches!(result, Err(UpdateError::ApplyFailed { .. })));
    assert!(launcher.launches.lock().expect("launches").is_empty());
}

/// A successful install removes the startup-attempt record with the rest of `tupdates`.
#[test]
fn a_successful_install_clears_the_startup_attempt_record() {
    let tmp = TempDir::new().expect("tmp");
    let working_dir = tmp.path().join("profile");
    let install = tmp.path().join("install");
    fs::create_dir_all(&install).expect("install");
    fs::write(install.join(DEFAULT_EXE_NAME), b"installed app").expect("app");
    stage(&working_dir, &[(DEFAULT_EXE_NAME, b"new app")]);
    record_startup_attempt(
        &working_dir,
        &Version::parse("0.2.0-alpha.7").expect("version"),
    )
    .expect("record");

    let args = parse_args(["-update"]);
    let launcher = RecordingLauncher::default();
    let outcome = run_updater_with_policy(
        &args,
        &working_dir,
        &install,
        &NoWait,
        &launcher,
        ApplyRetryPolicy::fast(),
    )
    .expect("install");

    assert!(outcome.updated && outcome.relaunched);
    assert_eq!(
        fs::read(install.join(DEFAULT_EXE_NAME)).expect("app"),
        b"new app"
    );
    assert!(!startup_attempt_path(&working_dir).exists());
    assert!(!working_dir.join(TUPDATES_DIR_NAME).exists());
    assert!(
        !install
            .join(tg_updater::startup_attempt::STARTUP_ATTEMPT_FILE_NAME)
            .exists(),
        "the record is never copied into the install folder"
    );
}
