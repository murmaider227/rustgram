//! Installs a ready update at startup instead of starting the app (TGB-3187).
//!
//! Port of the update step of `Sandbox::setupApplication` (`core/sandbox.cpp:470-476`): once the
//! single-instance server is listening, `!UpdaterDisabled() && !cNoStartUpdate() &&
//! Core::checkReadyUpdate()` sets `cSetRestartingUpdate(true)` and quits, and `Launcher::exec`
//! (`core/launcher.cpp:428-432`) then starts the updater, which installs the update and starts the
//! new version with `-noupdate` (`_other/updater_win.cpp:824-829`).
//!
//! Here the staged update must pass the TGB-3117 check first (newer than the running app, readable
//! version; anything else is deleted), and the updater is started with the same arguments as
//! "Restart to update". Two guards keep an update that cannot be installed from blocking the app:
//! the updater relaunches the installed app with `-noupdate` even when the copy fails, and the
//! version tried is recorded in `tupdates/startup_attempt`, so the next start opens the app
//! normally (the menu still offers "Restart to update") even if the updater died without a
//! relaunch. If the updater cannot be started at all, the app starts normally.
//!
//! Unlike tdesktop, `-quit` returns before this step (the installer closes the app with it), and
//! the "Update automatically" switch does not stop the install of an update that was already
//! downloaded (tdesktop checks only `-noupdate` and the build's `UpdaterDisabled()`).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use tg_core::launcher::ParsedArgs;
use tg_updater::checker::{ReadyUpdateCheck, check_ready_update};
use tg_updater::startup_attempt::{record_startup_attempt, startup_attempt_recorded};
use tg_updater::version::Version;

use super::plan::StartupPlan;
use super::updater::{
    UpdateState, build_updater_launch_plan_for_version, current_process_creation_time,
    spawn_updater_process, staged_update_state,
};

/// Test hook for the end-to-end test of this step (`tests/e2e_tgb3187_startup_update.rs`): when it
/// names a file and the run has its own `-workdir`, each start appends one line saying what this
/// step decided, and a start that would open the window exits right after it instead.
pub const STARTUP_UPDATE_RECORD_ENV: &str = "RUSTGRAM_E2E_STARTUP_UPDATE_RECORD";

/// Why the app starts normally instead of installing a staged update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartNormallyReason {
    /// `-noupdate` was given (the updater relaunches the app with it).
    NoUpdateFlag,
    /// Nothing is staged in `tupdates`.
    NothingStaged,
    /// The staged update is not newer than this app, or its version cannot be read; it was deleted
    /// (TGB-3117).
    Rejected,
    /// A startup install of this staged version was already tried and the app is still running the
    /// old version: the install failed, so it waits for "Restart to update".
    AlreadyTried(Version),
    /// The startup attempt could not be recorded, so a failing install could not be told from a
    /// first try.
    AttemptNotRecorded,
    /// `rustgram-updater.exe` could not be started.
    UpdaterNotStarted,
}

/// What the startup update step decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupUpdateDecision {
    /// Start the updater for this staged version and exit.
    Install(Version),
    /// Open the app.
    StartNormally(StartNormallyReason),
}

impl std::fmt::Display for StartupUpdateDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use StartNormallyReason as Reason;
        match self {
            Self::Install(version) => write!(f, "install {version}"),
            Self::StartNormally(reason) => {
                f.write_str("start-normally ")?;
                match reason {
                    Reason::NoUpdateFlag => f.write_str("noupdate"),
                    Reason::NothingStaged => f.write_str("nothing-staged"),
                    Reason::Rejected => f.write_str("rejected"),
                    Reason::AlreadyTried(version) => write!(f, "already-tried {version}"),
                    Reason::AttemptNotRecorded => f.write_str("attempt-not-recorded"),
                    Reason::UpdaterNotStarted => f.write_str("updater-not-started"),
                }
            }
        }
    }
}

/// Decides whether the update staged in `working_dir` is installed now instead of starting the app
/// (`Core::checkReadyUpdate`), recording the attempt when it is.
#[must_use]
pub fn decide_startup_update(
    working_dir: &Path,
    current: &Version,
    no_update: bool,
) -> StartupUpdateDecision {
    use StartNormallyReason as Reason;
    if no_update {
        return StartupUpdateDecision::StartNormally(Reason::NoUpdateFlag);
    }
    match check_ready_update(working_dir, current) {
        ReadyUpdateCheck::NotStaged => StartupUpdateDecision::StartNormally(Reason::NothingStaged),
        ReadyUpdateCheck::Rejected(err) => {
            tracing::info!(%err, "Deleted a staged update that is not newer than this version");
            StartupUpdateDecision::StartNormally(Reason::Rejected)
        }
        ReadyUpdateCheck::Ready { version, .. } => {
            if startup_attempt_recorded(working_dir, &version) {
                tracing::warn!(
                    %version,
                    "Installing this update at startup failed before; starting the app"
                );
                StartupUpdateDecision::StartNormally(Reason::AlreadyTried(version))
            } else if let Err(err) = record_startup_attempt(working_dir, &version) {
                tracing::warn!(%err, "Could not record the startup update attempt; starting the app");
                StartupUpdateDecision::StartNormally(Reason::AttemptNotRecorded)
            } else {
                StartupUpdateDecision::Install(version)
            }
        }
    }
}

/// Runs the startup update step for this launch: starts `rustgram-updater.exe` for a ready update
/// (same plan as "Restart to update"). The caller exits when it returns
/// [`StartupUpdateDecision::Install`] and opens the app otherwise.
pub fn install_ready_update_at_start(
    exe_path: &Path,
    working_dir: &Path,
    is_custom_workdir: bool,
    parsed_args: &ParsedArgs,
    current: &Version,
) -> StartupUpdateDecision {
    let decision = decide_startup_update(working_dir, current, parsed_args.no_update);
    let StartupUpdateDecision::Install(version) = decision else {
        return decision;
    };
    let plan = build_updater_launch_plan_for_version(
        exe_path,
        working_dir,
        is_custom_workdir,
        parsed_args,
        Some(std::process::id()),
        current_process_creation_time(),
        Some(current),
    );
    match spawn_updater_process(&plan) {
        Ok(()) => {
            tracing::info!(%version, "Installing the ready update instead of starting the app");
            StartupUpdateDecision::Install(version)
        }
        Err(err) => {
            tracing::warn!(%err, "Could not start the updater; starting the app");
            StartupUpdateDecision::StartNormally(StartNormallyReason::UpdaterNotStarted)
        }
    }
}

/// The [`STARTUP_UPDATE_RECORD_ENV`] hook: appends `decision`, the update the menu would offer and
/// this run's arguments to the record file. Returns `true` when the caller must exit instead of
/// opening the window (the hook is set and the run has its own `-workdir`).
pub fn record_for_e2e_and_stop(
    plan: &StartupPlan,
    decision: &StartupUpdateDecision,
    current: &Version,
) -> bool {
    if !plan.resolved_working_dir.is_custom {
        return false;
    }
    let Some(record) = std::env::var_os(STARTUP_UPDATE_RECORD_ENV).filter(|v| !v.is_empty()) else {
        return false;
    };
    let offered = match staged_update_state(&plan.resolved_working_dir.path, current) {
        Some(UpdateState::Ready { version, .. }) => version.to_string(),
        _ => "none".to_owned(),
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let line = format!(
        "decision={decision}\toffered={offered}\targs={}\n",
        args.join(" ")
    );
    let written = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&record)
        .and_then(|mut file| file.write_all(line.as_bytes()));
    if let Err(err) = written {
        tracing::warn!(%err, "Could not write the startup update record");
    }
    true
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tg_updater::TUPDATES_DIR_NAME;
    use tg_updater::checker::has_ready_update;

    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).expect("version")
    }

    fn stage(working_dir: &Path, version: &str) -> PathBuf {
        let temp = working_dir.join(TUPDATES_DIR_NAME).join("temp");
        fs::create_dir_all(&temp).expect("temp");
        fs::write(temp.join("rustgram.exe"), b"staged").expect("staged app");
        tg_updater::package::write_version_file(&temp, &v(version)).expect("version file");
        fs::write(temp.join("ready"), b"1").expect("ready");
        temp
    }

    #[test]
    fn a_newer_staged_update_is_installed_once() {
        let tmp = tempfile::TempDir::new().expect("tmp");
        let running = v("0.2.0-alpha.6");
        stage(tmp.path(), "0.2.0-alpha.7");

        assert_eq!(
            decide_startup_update(tmp.path(), &running, false),
            StartupUpdateDecision::Install(v("0.2.0-alpha.7"))
        );
        // The app is still the old version at the next start: the install failed. Start normally
        // and keep the update for "Restart to update".
        assert_eq!(
            decide_startup_update(tmp.path(), &running, false),
            StartupUpdateDecision::StartNormally(StartNormallyReason::AlreadyTried(v(
                "0.2.0-alpha.7"
            )))
        );
        assert!(has_ready_update(tmp.path()));
        assert!(matches!(
            staged_update_state(tmp.path(), &running),
            Some(UpdateState::Ready { .. })
        ));
    }

    #[test]
    fn a_later_download_is_tried_again() {
        let tmp = tempfile::TempDir::new().expect("tmp");
        let running = v("0.2.0-alpha.6");
        stage(tmp.path(), "0.2.0-alpha.7");
        assert!(matches!(
            decide_startup_update(tmp.path(), &running, false),
            StartupUpdateDecision::Install(_)
        ));
        fs::remove_dir_all(tmp.path().join(TUPDATES_DIR_NAME).join("temp")).expect("clear temp");
        stage(tmp.path(), "0.2.0-alpha.8");
        assert_eq!(
            decide_startup_update(tmp.path(), &running, false),
            StartupUpdateDecision::Install(v("0.2.0-alpha.8"))
        );
    }

    #[test]
    fn noupdate_starts_normally_and_keeps_the_update() {
        let tmp = tempfile::TempDir::new().expect("tmp");
        stage(tmp.path(), "0.2.0-alpha.7");
        assert_eq!(
            decide_startup_update(tmp.path(), &v("0.2.0-alpha.6"), true),
            StartupUpdateDecision::StartNormally(StartNormallyReason::NoUpdateFlag)
        );
        assert!(has_ready_update(tmp.path()));
        assert!(!tg_updater::startup_attempt::startup_attempt_path(tmp.path()).exists());
    }

    #[test]
    fn a_stale_staged_update_is_deleted_and_the_app_starts() {
        let tmp = tempfile::TempDir::new().expect("tmp");
        stage(tmp.path(), "0.2.0-alpha.5");
        assert_eq!(
            decide_startup_update(tmp.path(), &v("0.2.0-alpha.6"), false),
            StartupUpdateDecision::StartNormally(StartNormallyReason::Rejected)
        );
        assert!(!tmp.path().join(TUPDATES_DIR_NAME).exists());

        assert_eq!(
            decide_startup_update(tmp.path(), &v("0.2.0-alpha.6"), false),
            StartupUpdateDecision::StartNormally(StartNormallyReason::NothingStaged)
        );
    }

    #[test]
    fn a_missing_updater_starts_the_app() {
        let tmp = tempfile::TempDir::new().expect("tmp");
        let install = tmp.path().join("install");
        fs::create_dir_all(&install).expect("install");
        let profile = tmp.path().join("profile");
        stage(&profile, "0.2.0-alpha.7");
        let parsed = tg_core::launcher::parse_arguments(&["rustgram.exe".to_owned()]);

        assert_eq!(
            install_ready_update_at_start(
                &install.join("rustgram.exe"),
                &profile,
                true,
                &parsed,
                &v("0.2.0-alpha.6"),
            ),
            StartupUpdateDecision::StartNormally(StartNormallyReason::UpdaterNotStarted)
        );
        assert!(has_ready_update(&profile));
    }
}
