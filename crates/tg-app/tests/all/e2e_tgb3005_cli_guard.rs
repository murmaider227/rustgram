//! TGB-3005: `rustgram --version` (and other info flags or unknown options) must print and exit
//! before any profile, lock or window is touched.
//!
//! The process tests always pass a scratch `-workdir` plus `--demo` and point every data-dir
//! variable at a temp dir, so even a regression that starts the app cannot reach a real profile
//! or take the `tg://` handler; it times out, is killed by PID, and the test fails.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tg_app::startup::{StartupAction, StartupPlan, StartupPrefs};
use tg_core::launcher::WorkDirEnv;

fn test_env(base: &Path) -> WorkDirEnv {
    WorkDirEnv {
        exe_dir: base.join("bin"),
        app_data_dir: Some(base.join("AppData")),
        current_dir: base.join("cwd"),
    }
}

#[test]
fn info_flags_and_unknown_options_never_plan_an_app_run() {
    let dir = TempDir::new().unwrap();
    let env = test_env(dir.path());
    let prefs = StartupPrefs::default();
    for (argv, expected) in [
        (
            &["rustgram.exe", "--version"][..],
            StartupAction::ShowVersion,
        ),
        (&["rustgram.exe", "-v"], StartupAction::ShowVersion),
        (&["rustgram.exe", "--help"], StartupAction::ShowHelp),
        (
            &["rustgram.exe", "--verison"],
            StartupAction::RejectArguments,
        ),
        (
            &["rustgram.exe", "-cleanup", "--foo"],
            StartupAction::RejectArguments,
        ),
    ] {
        let plan = StartupPlan::build(argv, &env, Some(Path::new("rustgram.exe")), &prefs);
        assert_eq!(plan.action, expected, "{argv:?}");
        assert!(!plan.register_url_schemes_now, "{argv:?}");
        assert!(!plan.check_updates_enabled, "{argv:?}");
    }
    let plan = StartupPlan::build(
        ["rustgram.exe", "--", "--version"],
        &env,
        Some(Path::new("rustgram.exe")),
        &prefs,
    );
    assert_eq!(plan.action, StartupAction::RunApp);
}

/// Runs the real binary with data-dir variables in `sandbox` and a hard timeout.
fn run_exe(args: &[&str], sandbox: &Path) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rustgram"))
        .args(args)
        .current_dir(sandbox)
        .env("APPDATA", sandbox.join("AppData"))
        .env("LOCALAPPDATA", sandbox.join("LocalAppData"))
        .env("XDG_DATA_HOME", sandbox.join("xdg"))
        .env("HOME", sandbox.join("home"))
        .env("RUSTGRAM_AGENT", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rustgram");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if child.try_wait().expect("poll rustgram").is_some() {
            return child.wait_with_output().expect("collect rustgram output");
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("rustgram {args:?} did not exit: it started the app instead of printing");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn version_and_unknown_option_exit_without_touching_the_workdir() {
    let sandbox = TempDir::new().unwrap();
    let workdir = sandbox.path().join("wd");
    let wd = workdir.to_string_lossy().into_owned();

    let out = run_exe(&["--version", "--demo", "-workdir", &wd], sandbox.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.starts_with("Rustgram Desktop "), "stdout: {stdout}");

    let out = run_exe(&["--frobnicate", "--demo", "-workdir", &wd], sandbox.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains("--frobnicate"), "stderr: {stderr}");

    assert!(!workdir.exists(), "the working dir was created");
    assert!(
        !sandbox.path().join("AppData").exists(),
        "the default profile dir was created"
    );
}
