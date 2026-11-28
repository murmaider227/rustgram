//! Records that the app tried to install a staged update at startup, so a failed install is
//! tried once per staged version and the next start opens the app normally (TGB-3187).
//!
//! tdesktop installs a ready update instead of starting the app (`Sandbox::setupApplication`,
//! `core/sandbox.cpp:470`) and relies on the updater relaunching it with `-noupdate`
//! (`_other/updater_win.cpp:824`). That breaks the loop only when the updater runs to its end; an
//! updater that crashes or hangs would be started again by every launch. The file kept here,
//! `<working dir>/tupdates/startup_attempt`, holds the version that was tried; while it names the
//! staged version the app starts normally and the update waits for "Restart to update".
//!
//! The file sits next to `tupdates/temp`, never inside it, because the updater copies everything in
//! `temp` into the install folder.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::TUPDATES_DIR_NAME;
use crate::version::Version;

/// File name of the startup-attempt record inside `<working dir>/tupdates`.
pub const STARTUP_ATTEMPT_FILE_NAME: &str = "startup_attempt";

/// `<working_dir>/tupdates/startup_attempt`.
#[must_use]
pub fn startup_attempt_path(working_dir: &Path) -> PathBuf {
    working_dir
        .join(TUPDATES_DIR_NAME)
        .join(STARTUP_ATTEMPT_FILE_NAME)
}

/// Whether a startup install of `version` was already tried in `working_dir`.
#[must_use]
pub fn startup_attempt_recorded(working_dir: &Path, version: &Version) -> bool {
    fs::read_to_string(startup_attempt_path(working_dir))
        .ok()
        .and_then(|text| Version::parse(text.trim()).ok())
        .is_some_and(|tried| tried == *version)
}

/// Records that a startup install of `version` is being tried. The caller installs only when this
/// succeeds: without the record a failing install could not be told from a first try.
pub fn record_startup_attempt(working_dir: &Path, version: &Version) -> io::Result<()> {
    let path = startup_attempt_path(working_dir);
    fs::write(&path, version.to_string())?;
    if startup_attempt_recorded(working_dir, version) {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "'{}' does not read back as {version}",
            path.display()
        )))
    }
}

/// Deletes the startup-attempt record (a missing file is fine).
pub fn clear_startup_attempt(working_dir: &Path) {
    let _ = fs::remove_file(startup_attempt_path(working_dir));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).expect("version")
    }

    #[test]
    fn a_recorded_attempt_matches_only_its_own_version() {
        let tmp = tempfile::TempDir::new().expect("tmp");
        fs::create_dir_all(tmp.path().join(TUPDATES_DIR_NAME)).expect("tupdates");
        assert!(!startup_attempt_recorded(tmp.path(), &v("0.2.0-alpha.7")));

        record_startup_attempt(tmp.path(), &v("0.2.0-alpha.7")).expect("record");
        assert!(startup_attempt_recorded(tmp.path(), &v("0.2.0-alpha.7")));
        assert!(!startup_attempt_recorded(tmp.path(), &v("0.2.0-alpha.8")));

        clear_startup_attempt(tmp.path());
        assert!(!startup_attempt_recorded(tmp.path(), &v("0.2.0-alpha.7")));
    }

    #[test]
    fn recording_fails_without_a_tupdates_folder() {
        let tmp = tempfile::TempDir::new().expect("tmp");
        assert!(record_startup_attempt(tmp.path(), &v("0.2.0-alpha.7")).is_err());
    }
}
