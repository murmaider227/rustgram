//! TGB-3117: an update staged by an older install is never offered or installed over a newer app.
//!
//! The app side ([`check_ready_update`]) ports the "check ready version" step of tdesktop's
//! `checkReadyUpdate` (`core/update_checker.cpp`); the updater side
//! ([`refuse_stale_staged_update`] in [`run_updater`]) is defence in depth.
//!
//! No case here installs an update with a version file present, so the Windows uninstall registry
//! key of a real installation is never written.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tempfile::TempDir;

use tg_updater::apply::{
    AppLauncher, ProcessWaiter, build_relaunch_args, installed_app_version, parse_args,
    refuse_stale_staged_update, run_updater,
};
use tg_updater::checker::{
    ReadyUpdateCheck, UpdaterLaunchOptions, build_updater_launch_args, check_ready_update,
    has_ready_update,
};
use tg_updater::package::write_version_file;
use tg_updater::version::Version;
use tg_updater::{DEFAULT_EXE_NAME, TUPDATES_DIR_NAME, UpdateError};

fn v(text: &str) -> Version {
    Version::parse(text).expect("version")
}

/// Stages `<working_dir>/tupdates/temp` like `unpack_verified_package`: files, then
/// `tdata/version` (when `version` is given), then the `ready` marker.
fn stage(working_dir: &Path, version: Option<&str>) -> PathBuf {
    let temp = working_dir.join(TUPDATES_DIR_NAME).join("temp");
    fs::create_dir_all(&temp).expect("temp");
    fs::write(temp.join(DEFAULT_EXE_NAME), b"staged app").expect("staged app");
    if let Some(version) = version {
        write_version_file(&temp, &v(version)).expect("version file");
    }
    fs::write(temp.join("ready"), b"1").expect("ready");
    temp
}

#[test]
fn startup_keeps_a_newer_staged_update_ready() {
    let tmp = TempDir::new().expect("tmp");
    let temp = stage(tmp.path(), Some("0.2.0-alpha.3"));

    match check_ready_update(tmp.path(), &v("0.2.0-alpha.2")) {
        ReadyUpdateCheck::Ready {
            version,
            staging_dir,
        } => {
            assert_eq!(version, v("0.2.0-alpha.3"));
            assert_eq!(staging_dir, temp);
        }
        other => panic!("expected Ready, got {other:?}"),
    }
    assert!(has_ready_update(tmp.path()), "a newer update stays staged");
}

/// The owner's case: 0.2.0-alpha.2 installed by hand over a profile holding a staged alpha.1.
#[test]
fn startup_deletes_an_older_staged_update() {
    let tmp = TempDir::new().expect("tmp");
    stage(tmp.path(), Some("0.2.0-alpha.1"));

    match check_ready_update(tmp.path(), &v("0.2.0-alpha.2")) {
        ReadyUpdateCheck::Rejected(UpdateError::NotNewer { current, package }) => {
            assert_eq!(current, "0.2.0-alpha.2");
            assert_eq!(package, "0.2.0-alpha.1");
        }
        other => panic!("expected Rejected(NotNewer), got {other:?}"),
    }
    assert!(!tmp.path().join(TUPDATES_DIR_NAME).exists());
}

#[test]
fn startup_deletes_a_staged_update_of_the_running_version() {
    let tmp = TempDir::new().expect("tmp");
    stage(tmp.path(), Some("0.2.0-alpha.2"));

    assert!(matches!(
        check_ready_update(tmp.path(), &v("0.2.0-alpha.2")),
        ReadyUpdateCheck::Rejected(UpdateError::NotNewer { .. })
    ));
    assert!(!tmp.path().join(TUPDATES_DIR_NAME).exists());
}

#[test]
fn startup_deletes_a_staged_update_without_a_readable_version() {
    let tmp = TempDir::new().expect("tmp");
    stage(tmp.path(), None);
    assert!(matches!(
        check_ready_update(tmp.path(), &v("0.2.0-alpha.2")),
        ReadyUpdateCheck::Rejected(_)
    ));
    assert!(!tmp.path().join(TUPDATES_DIR_NAME).exists());

    let temp = stage(tmp.path(), Some("0.2.0-alpha.3"));
    fs::write(temp.join("tdata").join("version"), b"junk").expect("corrupt version file");
    assert!(matches!(
        check_ready_update(tmp.path(), &v("0.2.0-alpha.2")),
        ReadyUpdateCheck::Rejected(UpdateError::MalformedPackage(_))
    ));
    assert!(!tmp.path().join(TUPDATES_DIR_NAME).exists());
}

#[test]
fn startup_without_a_staged_update_touches_nothing() {
    let tmp = TempDir::new().expect("tmp");
    // A download still in progress has no `ready` marker yet.
    let temp = tmp.path().join(TUPDATES_DIR_NAME).join("temp");
    fs::create_dir_all(&temp).expect("temp");
    assert!(matches!(
        check_ready_update(tmp.path(), &v("0.2.0-alpha.2")),
        ReadyUpdateCheck::NotStaged
    ));
    assert!(temp.is_dir());
}

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

/// The updater refuses an older staged package, deletes it, keeps the installed files, and still
/// relaunches the app (unchanged).
#[test]
fn updater_refuses_an_older_staged_package_and_relaunches_the_app() {
    let tmp = TempDir::new().expect("tmp");
    let working_dir = tmp.path().join("profile");
    let install = tmp.path().join("install");
    fs::create_dir_all(&install).expect("install");
    fs::write(install.join(DEFAULT_EXE_NAME), b"installed app").expect("app");
    stage(&working_dir, Some("0.2.0-alpha.1"));

    let args = parse_args(["-update", "-appversion", "0.2.0-alpha.2"]);
    let launcher = RecordingLauncher::default();
    let result = run_updater(&args, &working_dir, &install, &NoWait, &launcher);

    match result {
        Err(UpdateError::NotNewer { current, package }) => {
            assert_eq!(current, "0.2.0-alpha.2");
            assert_eq!(package, "0.2.0-alpha.1");
        }
        other => panic!("expected NotNewer, got {other:?}"),
    }
    assert_eq!(
        fs::read(install.join(DEFAULT_EXE_NAME)).expect("app"),
        b"installed app"
    );
    assert!(!working_dir.join(TUPDATES_DIR_NAME).exists());
    let launches = launcher.launches.lock().expect("launches");
    assert_eq!(
        *launches,
        vec![(install.join(DEFAULT_EXE_NAME), vec!["-noupdate".to_owned()])]
    );
}

#[test]
fn updater_refuses_the_same_version_and_accepts_a_newer_one() {
    let tmp = TempDir::new().expect("tmp");
    let installed = v("0.2.0-alpha.2");

    stage(tmp.path(), Some("0.2.0-alpha.2"));
    assert!(matches!(
        refuse_stale_staged_update(tmp.path(), Some(&installed)),
        Some(UpdateError::NotNewer { .. })
    ));
    assert!(!tmp.path().join(TUPDATES_DIR_NAME).exists());

    stage(tmp.path(), Some("0.2.0-alpha.3"));
    assert!(refuse_stale_staged_update(tmp.path(), Some(&installed)).is_none());
    assert!(has_ready_update(tmp.path()), "a newer package stays staged");

    // Unknown installed version or no version file: the updater cannot compare and goes ahead,
    // like tdesktop's updater (the app has already checked).
    assert!(refuse_stale_staged_update(tmp.path(), None).is_none());
    let other = TempDir::new().expect("tmp");
    stage(other.path(), None);
    assert!(refuse_stale_staged_update(other.path(), Some(&installed)).is_none());
    assert!(has_ready_update(other.path()));
}

#[test]
fn updater_refuses_an_older_legacy_ready_folder() {
    let tmp = TempDir::new().expect("tmp");
    let legacy = tmp.path().join(TUPDATES_DIR_NAME).join("ready");
    fs::create_dir_all(&legacy).expect("legacy");
    write_version_file(&legacy, &v("0.1.0-alpha.2")).expect("version file");

    assert!(matches!(
        refuse_stale_staged_update(tmp.path(), Some(&v("0.2.0-alpha.2"))),
        Some(UpdateError::NotNewer { .. })
    ));
    assert!(!legacy.exists());
}

#[test]
fn app_version_flag_is_passed_to_the_updater_but_not_back_to_the_app() {
    let launch = build_updater_launch_args(&UpdaterLaunchOptions {
        app_version: Some("0.2.0-alpha.3".to_owned()),
        ..UpdaterLaunchOptions::default()
    });
    assert_eq!(launch, vec!["-update", "-appversion", "0.2.0-alpha.3"]);

    let parsed = parse_args(&launch);
    assert_eq!(parsed.app_version.as_deref(), Some("0.2.0-alpha.3"));
    assert_eq!(build_relaunch_args(&parsed), vec!["-noupdate"]);
}

#[test]
fn installed_version_prefers_the_app_flag() {
    let missing = Path::new("definitely-missing-dir").join(DEFAULT_EXE_NAME);
    assert_eq!(
        installed_app_version(Some("v0.2.0-alpha.2"), &missing),
        Some(v("0.2.0-alpha.2"))
    );
    assert_eq!(installed_app_version(Some("not a version"), &missing), None);
    assert_eq!(installed_app_version(None, &missing), None);
}

/// The uninstall registry key is changed only for the folder it describes, so updating a portable
/// copy or a test install never rewrites the installed copy's version.
#[test]
fn registry_install_location_must_match_the_updated_folder() {
    let tmp = TempDir::new().expect("tmp");
    let install = tmp.path().join("Rustgram");
    fs::create_dir_all(&install).expect("install");
    let other = tmp.path().join("Portable");
    fs::create_dir_all(&other).expect("other");

    let with_slash = PathBuf::from(format!(
        "{}{}",
        install.display(),
        std::path::MAIN_SEPARATOR
    ));
    assert!(tg_updater::apply::same_directory(&with_slash, &install));
    assert!(!tg_updater::apply::same_directory(&other, &install));
    assert!(!tg_updater::apply::same_directory(Path::new(""), &install));
    #[cfg(windows)]
    assert!(tg_updater::apply::same_directory(
        Path::new(&install.to_string_lossy().to_uppercase()),
        &install
    ));
}
