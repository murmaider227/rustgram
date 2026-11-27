//! TGB-3113 end-to-end: "Restart to update" with the working dir outside the install dir.
//!
//! Runs the real compiled `rustgram-updater` binary. The install dir and the working dir are
//! different temp folders (like a portable copy with its profile in `%APPDATA%\Rustgram`), a
//! signed package (test key) is staged in `<working dir>/tupdates` the way the app stages it, and
//! the updater is started with the plan the app builds (`tg_updater::launch`). The test checks that
//! the files are replaced, `tupdates` is cleaned, the debug log lands in the working dir, and the
//! app is relaunched from the install dir with the right arguments.
//!
//! `harness = false`: this test binary is also the fake app. Copied to `<install>/rustgram.exe`
//! and started by the updater, it writes its current dir and arguments next to itself and exits.
//!
//! `APPDATA` is pointed at a temp folder for the whole run, so the updater's default-working-dir
//! lookup never reaches a real profile. The staged `tdata/version` file is removed before each run
//! that installs, so the updater never writes the Windows uninstall registry key of a real
//! installation (it also writes that key only when its `InstallLocation` is the updated folder).
//! Only the TGB-3117 case keeps the file: the updater must read the stale version to refuse it.
//!
//! Opt-in: `RUSTGRAM_E2E_RELEASED_UPDATER=<path to a released rustgram-updater.exe>` also runs the
//! released (broken) updater: once as the broken app starts it, and once the way the manual steps
//! for stuck users in `docs/RELEASING.md` start it.

use std::any::Any;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use tempfile::TempDir;

use tg_updater::apply::{executable_product_version, installed_app_version};
use tg_updater::checker::UpdaterLaunchOptions;
use tg_updater::launch::{plan_updater_launch, spawn_updater};
use tg_updater::package::{
    pack_update, promote_ready_updater, unpack_verified_package, verify_package,
};
use tg_updater::version::{Target, UpdateChannel, Version};
use tg_updater::{APP_DATA_DIR_NAME, DEFAULT_EXE_NAME, TUPDATES_DIR_NAME, UPDATER_EXE_NAME};

/// File the fake app writes next to itself: line 1 its current dir, then one argument per line.
const RECORD_FILE: &str = "relaunch_record.txt";
const PROCESS_TIMEOUT: Duration = Duration::from_secs(60);

fn main() -> ExitCode {
    if is_fake_app() {
        record_launch();
        return ExitCode::SUCCESS;
    }
    if !cfg!(windows) {
        println!("e2e_tgb3113_workdir: skipped (Windows updater flow)");
        return ExitCode::SUCCESS;
    }

    let mut cases: Vec<(&str, fn())> = vec![
        (
            "fixed_launch_installs_update_staged_in_default_working_dir",
            fixed_launch_installs_update_staged_in_default_working_dir,
        ),
        (
            "fixed_launch_with_custom_working_dir_forwards_workdir",
            fixed_launch_with_custom_working_dir_forwards_workdir,
        ),
        (
            "new_updater_rescues_launch_from_broken_app",
            new_updater_rescues_launch_from_broken_app,
        ),
        (
            "tgb3117_stale_staged_package_is_refused_and_app_relaunched",
            tgb3117_stale_staged_package_is_refused_and_app_relaunched,
        ),
        (
            "tgb3117_installed_version_is_read_from_the_exe",
            tgb3117_installed_version_is_read_from_the_exe,
        ),
    ];
    if released_updater().is_some() {
        cases.push((
            "released_updater_broken_launch_installs_nothing",
            released_updater_broken_launch_installs_nothing,
        ));
        cases.push((
            "released_updater_manual_steps_install_staged_update",
            released_updater_manual_steps_install_staged_update,
        ));
    } else {
        println!("note: RUSTGRAM_E2E_RELEASED_UPDATER not set; released-updater cases skipped");
    }

    let mut failed = Vec::new();
    for (name, case) in &cases {
        match panic::catch_unwind(AssertUnwindSafe(case)) {
            Ok(()) => println!("test {name} ... ok"),
            Err(err) => {
                println!("test {name} ... FAILED: {}", panic_message(err.as_ref()));
                failed.push(*name);
            }
        }
    }
    let passed = cases.len() - failed.len();
    if failed.is_empty() {
        println!("\ntest result: ok. {passed} passed; 0 failed");
        ExitCode::SUCCESS
    } else {
        println!(
            "\ntest result: FAILED. {passed} passed; {} failed ({})",
            failed.len(),
            failed.join(", ")
        );
        ExitCode::FAILURE
    }
}

// ---------------------------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------------------------

/// The bug: the app started the updater with the install dir as current dir, so it never found
/// `%APPDATA%\Rustgram\tupdates`. The fixed plan runs it in the working dir, without `-workdir`.
fn fixed_launch_installs_update_staged_in_default_working_dir() {
    let fx = Fixture::new(&built_updater(), true);
    let working_dir = fx.default_working_dir();
    fx.stage(&working_dir);

    let plan = plan_updater_launch(
        &fx.install.join(DEFAULT_EXE_NAME),
        &working_dir,
        false,
        UpdaterLaunchOptions {
            debug: true,
            ..UpdaterLaunchOptions::default()
        },
    );
    assert_eq!(plan.current_dir, working_dir);
    assert_eq!(plan.args, vec!["-update", "-debug"]);
    let status = wait(spawn_updater(&plan).expect("spawn updater"));
    assert!(status.success(), "updater exit status {status}");

    fx.assert_installed(&working_dir);
    let log = fx.updater_log(&working_dir);
    assert!(
        log.contains("(CurrentDir)"),
        "updater should find tupdates in its current dir: {log}"
    );
    assert!(
        !fx.install.join("DebugLogs").exists(),
        "updater log belongs in the working dir"
    );
    assert_eq!(
        fx.relaunch_record(),
        launch_record(&fx.install, &["-noupdate", "-debug"])
    );
}

/// A custom working dir (`-workdir`) is the updater's current dir and is forwarded to the app.
fn fixed_launch_with_custom_working_dir_forwards_workdir() {
    let fx = Fixture::new(&built_updater(), true);
    let working_dir = fx.root.path().join("custom profile");
    fx.stage(&working_dir);

    let plan = plan_updater_launch(
        &fx.install.join(DEFAULT_EXE_NAME),
        &working_dir,
        true,
        UpdaterLaunchOptions {
            key: Some("second".to_owned()),
            ..UpdaterLaunchOptions::default()
        },
    );
    assert_eq!(plan.current_dir, working_dir);
    let status = wait(spawn_updater(&plan).expect("spawn updater"));
    assert!(status.success(), "updater exit status {status}");

    fx.assert_installed(&working_dir);
    let workdir_arg = working_dir.to_string_lossy().into_owned();
    assert_eq!(
        fx.relaunch_record(),
        launch_record(
            &fx.install,
            &["-noupdate", "-workdir", &workdir_arg, "-key", "second"]
        )
    );
}

/// Releases up to 0.2.0-alpha.1 start the updater in the install dir with only `-update`. When
/// such a build downloads a fixed release, it first copies the release's updater into the install
/// dir (`promote_ready_updater`), so the new updater must find the default working dir itself.
fn new_updater_rescues_launch_from_broken_app() {
    let fx = Fixture::new(b"updater from the broken release", true);
    let working_dir = fx.default_working_dir();
    fx.stage(&working_dir);
    assert_eq!(
        fs::read(fx.install.join(UPDATER_EXE_NAME)).expect("promoted updater"),
        built_updater(),
        "staging promotes the bundled updater"
    );

    let status = fx.run_like_broken_app(&["-update", "-debug"]);
    assert!(status.success(), "updater exit status {status}");

    fx.assert_installed(&working_dir);
    let log = fx.updater_log(&working_dir);
    assert!(
        log.contains("(AppDefault)"),
        "updater should fall back to the app's default working dir: {log}"
    );
    assert_eq!(
        fx.relaunch_record(),
        launch_record(&fx.install, &["-noupdate", "-debug"])
    );
}

/// TGB-3117: the profile still holds a 0.2.0-alpha.1 package staged by an older install while
/// 0.2.0-alpha.2 runs. The updater, started with the plan the app builds (`-appversion`), installs
/// nothing, deletes the stale `tupdates`, exits non-zero, and relaunches the app unchanged.
fn tgb3117_stale_staged_package_is_refused_and_app_relaunched() {
    let fx = Fixture::new(&built_updater(), false);
    let working_dir = fx.default_working_dir();
    fx.stage_package(&working_dir, "0.2.0-alpha.1", true);

    let plan = plan_updater_launch(
        &fx.install.join(DEFAULT_EXE_NAME),
        &working_dir,
        false,
        UpdaterLaunchOptions {
            debug: true,
            app_version: Some("0.2.0-alpha.2".to_owned()),
            ..UpdaterLaunchOptions::default()
        },
    );
    assert_eq!(
        plan.args,
        vec!["-update", "-debug", "-appversion", "0.2.0-alpha.2"]
    );
    let status = wait(spawn_updater(&plan).expect("spawn updater"));
    assert_eq!(status.code(), Some(1), "a refused update exits non-zero");

    assert!(
        fs::read(fx.install.join(DEFAULT_EXE_NAME)).expect("app") == fx.old_app,
        "rustgram.exe must not be downgraded"
    );
    assert_eq!(
        fs::read(fx.install.join("resources.dat")).expect("resources"),
        b"old resources"
    );
    assert!(!fx.install.join("modules").exists());
    assert!(
        !working_dir.join(TUPDATES_DIR_NAME).exists(),
        "the stale package must be deleted"
    );
    assert!(working_dir.join("tdata").is_dir(), "the profile stays");
    let log = fx.updater_log(&working_dir);
    assert!(
        log.contains("anti-downgrade") && log.contains("0.2.0-alpha.1"),
        "updater log should name the refusal: {log}"
    );
    assert_eq!(
        fx.relaunch_record(),
        launch_record(&fx.install, &["-noupdate", "-debug"])
    );
}

/// Without `-appversion` (an app that does not pass it) the updater reads the installed
/// executable's `ProductVersion`; the built updater carries the same kind of resource.
fn tgb3117_installed_version_is_read_from_the_exe() {
    let expected = [
        option_env!("RUSTGRAM_VERSION"),
        option_env!("TGFORK_VERSION"),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|v| !v.is_empty())
    .unwrap_or(env!("CARGO_PKG_VERSION"));
    let expected = Version::parse(expected).expect("build version");
    let exe = Path::new(env!("CARGO_BIN_EXE_rustgram-updater"));
    assert_eq!(executable_product_version(exe), Some(expected.clone()));
    assert_eq!(installed_app_version(None, exe), Some(expected));
    assert_eq!(
        installed_app_version(Some("0.2.0-alpha.2"), exe),
        Some(Version::parse("0.2.0-alpha.2").expect("version")),
        "the app's own -appversion wins"
    );
}

/// The released updater, started the way the released app starts it, installs nothing and
/// relaunches the old app: the bug the owner saw.
fn released_updater_broken_launch_installs_nothing() {
    let released = fs::read(released_updater().expect("set")).expect("read released updater");
    let fx = Fixture::new(&released, false);
    let working_dir = fx.default_working_dir();
    fx.stage(&working_dir);

    let status = fx.run_like_broken_app(&["-update"]);
    assert_eq!(status.code(), Some(1), "no staged update found -> exit 1");
    assert_eq!(
        fs::read(fx.install.join(DEFAULT_EXE_NAME)).expect("app"),
        fx.old_app
    );
    assert!(
        working_dir
            .join(TUPDATES_DIR_NAME)
            .join("temp")
            .join("ready")
            .is_file()
    );
    assert_eq!(
        fx.relaunch_record(),
        launch_record(&fx.install, &["-noupdate"])
    );
}

/// Manual steps for stuck users: the app closed, run the installed `rustgram-updater.exe -update`
/// with the working dir as the current directory. The released updater then installs the update.
fn released_updater_manual_steps_install_staged_update() {
    let released = fs::read(released_updater().expect("set")).expect("read released updater");
    let fx = Fixture::new(&released, false);
    let working_dir = fx.default_working_dir();
    fx.stage(&working_dir);

    let status = wait(
        Command::new(fx.install.join(UPDATER_EXE_NAME))
            .arg("-update")
            .current_dir(&working_dir)
            .spawn()
            .expect("spawn released updater"),
    );
    assert!(status.success(), "released updater exit status {status}");
    fx.assert_installed(&working_dir);
    assert_eq!(
        fx.relaunch_record(),
        launch_record(&fx.install, &["-noupdate"])
    );
}

// ---------------------------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------------------------

struct Fixture {
    root: TempDir,
    install: PathBuf,
    old_app: Vec<u8>,
    new_app: Vec<u8>,
    bundles_updater: bool,
}

impl Fixture {
    /// An install dir with the old fake app, an old resource and `updater` as
    /// `rustgram-updater.exe`; `APPDATA` points into this fixture.
    fn new(updater: &[u8], bundles_updater: bool) -> Self {
        let root = TempDir::new().expect("temp dir");
        let install = root.path().join("portable app");
        fs::create_dir_all(&install).expect("install dir");
        let app = fs::read(env::current_exe().expect("current exe")).expect("read test exe");
        let old_app = with_marker(&app, b"OLD-APP-0.2.0-alpha.1");
        let new_app = with_marker(&app, b"NEW-APP-0.2.0-alpha.2");
        fs::write(install.join(DEFAULT_EXE_NAME), &old_app).expect("old app");
        fs::write(install.join("resources.dat"), b"old resources").expect("old resources");
        fs::write(install.join(UPDATER_EXE_NAME), updater).expect("updater");

        let app_data = root.path().join("AppData Roaming");
        fs::create_dir_all(&app_data).expect("appdata");
        // SAFETY: the harness runs one case at a time on the main thread; no other thread reads
        // the environment while it changes.
        unsafe { env::set_var("APPDATA", &app_data) };
        Self {
            root,
            install,
            old_app,
            new_app,
            bundles_updater,
        }
    }

    fn default_working_dir(&self) -> PathBuf {
        let base = env::var_os("APPDATA").map(PathBuf::from).expect("APPDATA");
        assert!(
            base.starts_with(self.root.path()),
            "APPDATA must be sandboxed"
        );
        base.join(APP_DATA_DIR_NAME)
    }

    /// Stages a signed v0.2.0-alpha.2 package in `<working_dir>/tupdates` the way the app does
    /// (`UpdateChecker::download_and_stage`: verify, unpack, promote the bundled updater).
    fn stage(&self, working_dir: &Path) {
        self.stage_package(working_dir, "0.2.0-alpha.2", false);
    }

    /// [`Self::stage`] for any `version`. `keep_version_file` keeps the staged `tdata/version`;
    /// only cases where the updater must refuse the package keep it (see module docs).
    fn stage_package(&self, working_dir: &Path, version: &str, keep_version_file: bool) {
        fs::create_dir_all(working_dir.join("tdata")).expect("profile tdata");
        let key = SigningKey::generate(&mut OsRng);
        let version = Version::parse(version).expect("version");
        let updater = built_updater();
        let mut files: Vec<(&str, &[u8])> = vec![
            (DEFAULT_EXE_NAME, self.new_app.as_slice()),
            ("resources.dat", b"new resources".as_slice()),
            ("modules/extra.txt", b"new module".as_slice()),
        ];
        if self.bundles_updater {
            files.push((UPDATER_EXE_NAME, updater.as_slice()));
        }
        let package = pack_update(
            UpdateChannel::Beta,
            Target::WIN_X64,
            &version,
            1_764_000_000,
            files,
            &key,
        )
        .expect("pack");
        let current = Version::parse("0.1.0").expect("version");
        let verified = verify_package(
            &package,
            &key.verifying_key(),
            &current,
            true,
            Target::WIN_X64,
        )
        .expect("verify");
        let temp = unpack_verified_package(&verified, working_dir).expect("unpack");
        promote_ready_updater(working_dir, &self.install).expect("promote");
        // Without a version file the updater skips the uninstall registry key (see module docs).
        if !keep_version_file {
            fs::remove_file(temp.join("tdata").join("version")).expect("remove version file");
        }
        assert!(temp.join("ready").is_file());
    }

    /// Starts the install dir's updater the way releases up to 0.2.0-alpha.1 do: current dir =
    /// install dir, no `-workdir`.
    fn run_like_broken_app(&self, args: &[&str]) -> ExitStatus {
        wait(
            Command::new(self.install.join(UPDATER_EXE_NAME))
                .args(args)
                .current_dir(&self.install)
                .spawn()
                .expect("spawn updater"),
        )
    }

    fn assert_installed(&self, working_dir: &Path) {
        assert!(
            fs::read(self.install.join(DEFAULT_EXE_NAME)).expect("app") == self.new_app,
            "rustgram.exe was not replaced"
        );
        assert_eq!(
            fs::read(self.install.join("resources.dat")).expect("resources"),
            b"new resources"
        );
        assert_eq!(
            fs::read(self.install.join("modules").join("extra.txt")).expect("module"),
            b"new module"
        );
        assert!(
            !working_dir.join(TUPDATES_DIR_NAME).exists(),
            "tupdates must be removed after install"
        );
        assert!(
            working_dir.join("tdata").is_dir(),
            "the profile must stay untouched"
        );
    }

    fn updater_log(&self, working_dir: &Path) -> String {
        let dir = working_dir.join("DebugLogs");
        let mut text = String::new();
        for entry in fs::read_dir(&dir).expect("DebugLogs in the working dir") {
            let path = entry.expect("entry").path();
            text.push_str(&fs::read_to_string(&path).expect("log"));
        }
        assert!(!text.is_empty(), "updater debug log is empty");
        text
    }

    /// Waits for the relaunched fake app to write its record.
    fn relaunch_record(&self) -> Vec<String> {
        let path = self.install.join(RECORD_FILE);
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        loop {
            if let Ok(text) = fs::read_to_string(&path)
                && text.ends_with("\nEND\n")
            {
                return text.lines().map(ToOwned::to_owned).collect();
            }
            assert!(
                Instant::now() < deadline,
                "the app was not relaunched ({} missing)",
                path.display()
            );
            thread::sleep(Duration::from_millis(50));
        }
    }
}

/// The record the fake app writes when started in `cwd` with `args`.
fn launch_record(cwd: &Path, args: &[&str]) -> Vec<String> {
    let mut lines = vec![normalize(cwd)];
    lines.extend(args.iter().map(|a| (*a).to_owned()));
    lines.push("END".to_owned());
    lines
}

// ---------------------------------------------------------------------------------------------
// Fake app and helpers
// ---------------------------------------------------------------------------------------------

fn is_fake_app() -> bool {
    env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .is_some_and(|name| name.eq_ignore_ascii_case(DEFAULT_EXE_NAME))
}

fn record_launch() {
    let Ok(exe) = env::current_exe() else { return };
    let Some(dir) = exe.parent() else { return };
    let cwd = env::current_dir()
        .map(|d| normalize(&d))
        .unwrap_or_default();
    let mut text = cwd;
    text.push('\n');
    for arg in env::args_os().skip(1) {
        text.push_str(&arg.to_string_lossy());
        text.push('\n');
    }
    text.push_str("END\n");
    let _ = fs::write(dir.join(RECORD_FILE), text);
}

/// Case-folded path without a trailing separator, so `C:\x\` and `c:\x` compare equal.
fn normalize(path: &Path) -> String {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = canonical.to_string_lossy().to_lowercase();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    text.trim_end_matches(['\\', '/']).to_owned()
}

/// The PE loader ignores bytes appended to an executable, so this marks two copies of the same
/// binary as "old" and "new" while both still run.
fn with_marker(exe: &[u8], marker: &[u8]) -> Vec<u8> {
    let mut out = exe.to_vec();
    out.extend_from_slice(marker);
    out
}

fn built_updater() -> Vec<u8> {
    fs::read(env!("CARGO_BIN_EXE_rustgram-updater")).expect("read built rustgram-updater")
}

fn released_updater() -> Option<PathBuf> {
    env::var_os("RUSTGRAM_E2E_RELEASED_UPDATER")
        .filter(|v: &OsString| !v.is_empty())
        .map(PathBuf::from)
}

fn wait(mut child: Child) -> ExitStatus {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("process did not exit within {PROCESS_TIMEOUT:?}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn panic_message(err: &(dyn Any + Send)) -> String {
    err.downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "panic".to_owned())
}
