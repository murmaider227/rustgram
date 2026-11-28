//! TGB-3187 end-to-end: the built app installs a ready update at startup, like tdesktop
//! (`Core::checkReadyUpdate` from `Sandbox::setupApplication`, `core/sandbox.cpp:470`).
//!
//! Each case copies the built `rustgram.exe` and a freshly built `rustgram-updater.exe` into a
//! scratch install folder, stages an update in a scratch `-workdir` the way the app stages it
//! (`tupdates/temp`, `tdata/version`, `ready`), and starts the copied app with that `-workdir`:
//! - a newer update: the app starts the updater and exits; the updater installs the package and
//!   starts the new version with `-noupdate`;
//! - a stale update: the app deletes it and starts normally (TGB-3117);
//! - a broken update (a file that cannot be copied): one attempt, then the updater starts the old
//!   app with `-noupdate`, which starts normally and still offers the update; the next start does
//!   not try again.
//!
//! `harness = false`: this test binary is also the "new version" inside the staged package. Started
//! as `rustgram.exe`, it writes its current dir and arguments next to itself and exits.
//!
//! The real app never opens a window here: with `RUSTGRAM_E2E_STARTUP_UPDATE_RECORD` set and a
//! scratch `-workdir`, it appends what its startup update step decided to the record file and exits
//! where it would open the window. `APPDATA` and the other data-dir variables point into the
//! sandbox, the staged versions are far from any real one, and the install folder is a scratch
//! folder, so no real profile, handler or uninstall registry key is touched.

use std::any::Any;
use std::env;
use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use tg_updater::package::write_version_file;
use tg_updater::startup_attempt::startup_attempt_path;
use tg_updater::version::Version;
use tg_updater::{DEFAULT_EXE_NAME, TUPDATES_DIR_NAME, UPDATER_EXE_NAME};

/// File the fake new version writes next to itself: its current dir, then one argument per line.
const RELAUNCH_RECORD: &str = "relaunch_record.txt";
/// `tg_app::startup::startup_update::STARTUP_UPDATE_RECORD_ENV` (not linked: this binary stays
/// small because it is copied into every staged package).
const RECORD_ENV: &str = "RUSTGRAM_E2E_STARTUP_UPDATE_RECORD";
const NEWER: &str = "999.0.0";
const STALE: &str = "0.0.1";
const TIMEOUT: Duration = Duration::from_secs(90);

fn main() -> ExitCode {
    if is_fake_new_version() {
        record_relaunch();
        return ExitCode::SUCCESS;
    }
    if !cfg!(windows) {
        println!("e2e_tgb3187_startup_update: skipped (Windows updater flow)");
        return ExitCode::SUCCESS;
    }
    let filter: Vec<String> = env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();
    let cases: [(&str, fn()); 3] = [
        (
            "newer_staged_update_is_installed_instead_of_starting",
            newer_staged_update_is_installed_instead_of_starting,
        ),
        (
            "stale_staged_update_is_deleted_and_the_app_starts",
            stale_staged_update_is_deleted_and_the_app_starts,
        ),
        (
            "broken_staged_update_is_tried_once_then_the_app_starts",
            broken_staged_update_is_tried_once_then_the_app_starts,
        ),
    ];
    let mut ran = 0;
    let mut failed = Vec::new();
    for (name, case) in cases {
        if !filter.is_empty() && !filter.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        ran += 1;
        match panic::catch_unwind(AssertUnwindSafe(case)) {
            Ok(()) => println!("test {name} ... ok"),
            Err(err) => {
                println!("test {name} ... FAILED: {}", panic_message(err.as_ref()));
                failed.push(name);
            }
        }
    }
    if failed.is_empty() {
        println!("\ntest result: ok. {ran} passed; 0 failed");
        ExitCode::SUCCESS
    } else {
        println!(
            "\ntest result: FAILED. {} passed; {} failed ({})",
            ran - failed.len(),
            failed.len(),
            failed.join(", ")
        );
        ExitCode::FAILURE
    }
}

// ---------------------------------------------------------------------------------------------
// Cases

/// The maintainer's case: a copy with a downloaded update kept starting the old version.
fn newer_staged_update_is_installed_instead_of_starting() {
    let s = Sandbox::new("newer");
    s.stage(NEWER, &[(DEFAULT_EXE_NAME, &fake_new_version_bytes())]);

    let started = Instant::now();
    s.run_app(&[]);
    let record = s.record_lines(1);
    assert!(
        record[0].starts_with(&format!("decision=install {NEWER}\t")),
        "the app must install instead of starting: {record:?}"
    );

    let relaunch = s.wait_for_relaunch();
    assert_eq!(
        relaunch,
        vec![
            normalize(&s.install),
            "-noupdate".to_owned(),
            "-workdir".to_owned(),
            normalize(&s.profile),
        ],
        "the updater starts the new version from the install folder with -noupdate"
    );
    assert_eq!(
        fs::read(s.install.join(DEFAULT_EXE_NAME)).expect("installed app"),
        fake_new_version_bytes(),
        "the new version is installed"
    );
    assert!(
        !s.profile.join(TUPDATES_DIR_NAME).exists(),
        "tupdates (with the startup-attempt record) is cleaned after the install"
    );
    assert_eq!(s.record_lines(1).len(), 1, "the old app started only once");
    println!("  installed and relaunched in {:?}", started.elapsed());
}

fn stale_staged_update_is_deleted_and_the_app_starts() {
    let s = Sandbox::new("stale");
    s.stage(STALE, &[(DEFAULT_EXE_NAME, &fake_new_version_bytes())]);

    s.run_app(&[]);
    let record = s.record_lines(1);
    assert!(
        record[0].starts_with("decision=start-normally rejected\t")
            && record[0].contains("offered=none"),
        "{record:?}"
    );
    assert!(!s.profile.join(TUPDATES_DIR_NAME).exists());
    s.assert_old_app_installed();
    thread::sleep(Duration::from_secs(2));
    assert!(!s.install.join(RELAUNCH_RECORD).exists(), "nothing ran");
    assert_eq!(s.record_lines(1).len(), 1);
}

fn broken_staged_update_is_tried_once_then_the_app_starts() {
    let s = Sandbox::new("broken");
    // A folder where the package has a file: the copy fails and is rolled back.
    fs::create_dir_all(s.install.join("blocked.dll")).expect("blocking folder");
    s.stage(
        NEWER,
        &[
            (DEFAULT_EXE_NAME, &fake_new_version_bytes()),
            ("blocked.dll", b"new dll"),
        ],
    );

    s.run_app(&[]);
    // 1. The app tries the install; 2. the updater fails and starts the old app with -noupdate,
    // which opens normally and still offers the update.
    let record = s.record_lines(2);
    assert!(
        record[0].starts_with(&format!("decision=install {NEWER}\t")),
        "{record:?}"
    );
    assert!(
        record[1].starts_with("decision=start-normally noupdate\t")
            && record[1].contains(&format!("offered={NEWER}"))
            && record[1].contains("args=-noupdate -workdir"),
        "the old app must start after the failed install: {record:?}"
    );
    s.assert_old_app_installed();
    assert!(!s.install.join(RELAUNCH_RECORD).exists());
    assert!(
        s.profile
            .join(TUPDATES_DIR_NAME)
            .join("temp")
            .join("ready")
            .is_file(),
        "the update stays staged for \"Restart to update\""
    );
    assert!(startup_attempt_path(&s.profile).is_file());

    // 3. The next start (no -noupdate) does not try again.
    s.run_app(&[]);
    let record = s.record_lines(3);
    assert!(
        record[2].starts_with(&format!("decision=start-normally already-tried {NEWER}\t"))
            && record[2].contains(&format!("offered={NEWER}")),
        "{record:?}"
    );
    thread::sleep(Duration::from_secs(2));
    assert_eq!(s.record_lines(3).len(), 3, "no updater was started");
    s.assert_old_app_installed();
}

// ---------------------------------------------------------------------------------------------
// Sandbox

struct Sandbox {
    root: PathBuf,
    install: PathBuf,
    profile: PathBuf,
    record: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("e2e_tgb3187")
            .join(format!("{name}-{}", std::process::id()));
        remove_dir_with_retry(&root);
        let install = root.join("install");
        let profile = root.join("profile");
        for dir in [&install, &profile, &root.join("AppData"), &root.join("cwd")] {
            fs::create_dir_all(dir).expect("sandbox dir");
        }
        fs::copy(app_exe(), install.join(DEFAULT_EXE_NAME)).expect("copy rustgram.exe");
        fs::copy(updater_exe(), install.join(UPDATER_EXE_NAME)).expect("copy rustgram-updater.exe");
        Self {
            record: root.join("startup_update_record.txt"),
            root,
            install,
            profile,
        }
    }

    /// Stages `files` with `tdata/version` = `version` and the `ready` marker last, like
    /// `unpack_verified_package`.
    fn stage(&self, version: &str, files: &[(&str, &[u8])]) {
        let temp = self.profile.join(TUPDATES_DIR_NAME).join("temp");
        fs::create_dir_all(&temp).expect("temp");
        for (name, bytes) in files {
            fs::write(temp.join(name), bytes).expect("staged file");
        }
        write_version_file(&temp, &Version::parse(version).expect("version")).expect("version");
        fs::write(temp.join("ready"), b"1").expect("ready");
    }

    /// Starts the copied app with this sandbox's `-workdir` and waits for it to exit.
    fn run_app(&self, extra: &[&str]) {
        let mut child = Command::new(self.install.join(DEFAULT_EXE_NAME))
            .arg("-workdir")
            .arg(&self.profile)
            .args(extra)
            .current_dir(self.root.join("cwd"))
            .env("APPDATA", self.root.join("AppData"))
            .env("LOCALAPPDATA", self.root.join("LocalAppData"))
            .env("XDG_DATA_HOME", self.root.join("xdg"))
            .env("HOME", self.root.join("home"))
            .env("RUSTGRAM_AGENT", "1")
            .env(RECORD_ENV, &self.record)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start rustgram.exe");
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = child.try_wait().expect("poll rustgram.exe") {
                assert!(status.success(), "rustgram.exe exited with {status}");
                return;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("rustgram.exe did not exit (it opened the window?)");
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Waits until the record file has at least `count` lines and returns them.
    fn record_lines(&self, count: usize) -> Vec<String> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let lines: Vec<String> = fs::read_to_string(&self.record)
                .unwrap_or_default()
                .lines()
                .map(ToOwned::to_owned)
                .collect();
            if lines.len() >= count {
                return lines;
            }
            assert!(
                Instant::now() < deadline,
                "expected {count} startup records, got {lines:?}"
            );
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn wait_for_relaunch(&self) -> Vec<String> {
        let path = self.install.join(RELAUNCH_RECORD);
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Ok(text) = fs::read_to_string(&path)
                && let Some(body) = text.strip_suffix("END\n")
            {
                return body.lines().map(ToOwned::to_owned).collect();
            }
            assert!(
                Instant::now() < deadline,
                "the new version was not started by the updater"
            );
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn assert_old_app_installed(&self) {
        let installed = fs::metadata(self.install.join(DEFAULT_EXE_NAME))
            .expect("installed app")
            .len();
        let built = fs::metadata(app_exe()).expect("built app").len();
        assert_eq!(installed, built, "the old app stays installed");
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // The updater may still be closing; its exe and folder unlock a moment later.
        remove_dir_with_retry(&self.root);
    }
}

fn remove_dir_with_retry(dir: &Path) {
    for _ in 0..50 {
        if !dir.exists() || fs::remove_dir_all(dir).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(200));
    }
}

// ---------------------------------------------------------------------------------------------
// Binaries

fn app_exe() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_rustgram"))
}

/// Builds `rustgram-updater` from this checkout into the same target folder and profile as the
/// app, so the test never runs a stale updater left by an older build.
fn updater_exe() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let profile_dir = app_exe().parent().expect("profile dir");
        let target_dir = profile_dir.parent().expect("target dir");
        let profile = match profile_dir.file_name().and_then(|n| n.to_str()) {
            Some("debug") | None => "dev".to_owned(),
            Some(other) => other.to_owned(),
        };
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
        let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let status = Command::new(cargo)
            .args([
                "build",
                "--quiet",
                "-p",
                "tg-updater",
                "--bin",
                "rustgram-updater",
            ])
            .arg("--profile")
            .arg(&profile)
            .arg("--manifest-path")
            .arg(&manifest)
            .arg("--target-dir")
            .arg(target_dir)
            .status()
            .expect("run cargo build for rustgram-updater");
        assert!(status.success(), "building rustgram-updater failed");
        let exe = profile_dir.join(UPDATER_EXE_NAME);
        assert!(exe.is_file(), "{} was not built", exe.display());
        exe
    })
}

fn fake_new_version_bytes() -> Vec<u8> {
    fs::read(env::current_exe().expect("current exe")).expect("read this test binary")
}

fn is_fake_new_version() -> bool {
    env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .is_some_and(|name| name.eq_ignore_ascii_case(DEFAULT_EXE_NAME))
}

fn record_relaunch() {
    let Ok(exe) = env::current_exe() else { return };
    let Some(dir) = exe.parent() else { return };
    let mut text = env::current_dir()
        .map(|d| normalize(&d))
        .unwrap_or_default();
    text.push('\n');
    for arg in env::args_os().skip(1) {
        text.push_str(&normalize(Path::new(&arg)));
        text.push('\n');
    }
    text.push_str("END\n");
    let _ = fs::write(dir.join(RELAUNCH_RECORD), text);
}

/// Lower-cased path text without the `\\?\` prefix or a trailing separator.
fn normalize(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\").to_lowercase();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    text.trim_end_matches('\\').to_owned()
}

fn panic_message(err: &(dyn Any + Send)) -> String {
    err.downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "panic".to_owned())
}
