//! `rustgram --smoke-test -workdir <scratch>` (the release workflow's smoke test) exits on its own
//! with code 0 on the frozen v0.1.0-alpha.2 profile, and refuses to run without a scratch
//! `-workdir`.
//!
//! Every data-dir variable points into a temp sandbox, so even a regression that started the full
//! app could not reach a real profile; such a run times out, is killed by PID and fails the test.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

fn fixture_profile() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/smoke_profile_v0_1_0_alpha_2/profile")
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create dir");
    for entry in fs::read_dir(from).expect("read dir") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

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
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if child.try_wait().expect("poll rustgram").is_some() {
            return child.wait_with_output().expect("collect rustgram output");
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("rustgram {args:?} did not exit: the smoke test started the app");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn packaged_binary_smoke_test_passes_on_the_alpha2_profile() {
    let sandbox = TempDir::new().unwrap();
    let workdir = sandbox.path().join("wd");
    copy_dir(&fixture_profile(), &workdir);
    let wd = workdir.to_string_lossy().into_owned();

    let out = run_exe(&["--smoke-test", "-workdir", &wd], sandbox.path());
    let report = fs::read_to_string(workdir.join("smoke_test_report.txt")).unwrap_or_default();
    assert_eq!(
        out.status.code(),
        Some(0),
        "report: {report}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(report.contains("media cache 2 entries"), "{report}");
    assert!(report.ends_with("result: PASS\n"), "{report}");

    // No window state, log or lock files of a normal start, and no default profile.
    assert!(!workdir.join("log.txt").exists());
    assert!(!sandbox.path().join("AppData").exists());
    assert!(!sandbox.path().join("LocalAppData").exists());
}

#[test]
fn packaged_binary_smoke_test_passes_on_a_first_start() {
    let sandbox = TempDir::new().unwrap();
    let workdir = sandbox.path().join("fresh");
    let wd = workdir.to_string_lossy().into_owned();
    let out = run_exe(&["--smoke-test", "-workdir", &wd], sandbox.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(workdir.join("tdata").join("key_datas").is_file());
}

#[test]
fn smoke_test_without_a_scratch_workdir_is_refused() {
    let sandbox = TempDir::new().unwrap();
    let out = run_exe(&["--smoke-test"], sandbox.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains("--smoke-test"), "stderr: {stderr}");
    assert!(!sandbox.path().join("AppData").exists());
}
