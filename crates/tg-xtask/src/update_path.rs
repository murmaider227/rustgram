//! `tg-xtask update-path-check`: applies a new `.tupdate` to a copy of an older published release
//! the way that release's client does, then smoke-tests the result (TGB-3180).
//!
//! The old app stages a package in `CheckerFlow::download_and_stage`
//! (`crates/tg-updater/src/checker.rs`): `verify_package_with_hash`, `unpack_verified_package` into
//! `<working dir>/tupdates/temp`, then `promote_ready_updater` copies a bundled
//! `rustgram-updater.exe` into the install folder. That code is unchanged in what it stages since
//! v0.1.0-alpha.2 (only a symlink check was added), so this check calls the same functions instead
//! of driving the old app's network download. Then it runs the OLD release's own
//! `rustgram-updater.exe` with the arguments the old app passes on "Restart to update" (`-update`,
//! current directory = the working dir, `-workdir` for a custom working dir), plus `-justupdate` so
//! it does not start the app window, and finally `rustgram.exe --smoke-test` on the updated copy.
//!
//! Every path the child processes see is inside `scratch`; `APPDATA` and `LOCALAPPDATA` point
//! into it too.

use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

use zip::ZipArchive;

use tg_updater::install_files::OBSOLETE_INSTALL_FILES;
use tg_updater::package::{
    parse_envelope, promote_ready_updater, unpack_verified_package, verify_package,
};
use tg_updater::version::{Target, Version};
use tg_updater::{DEFAULT_EXE_NAME, TUPDATES_DIR_NAME, UPDATER_EXE_NAME, verifying_key_from_hex};

use crate::XtaskError;

/// Name of the report the smoke test writes into its working dir.
pub const SMOKE_REPORT_NAME: &str = "smoke_test_report.txt";

/// Inputs of [`run_update_path_check`].
#[derive(Debug, Clone)]
pub struct UpdatePathCheck {
    /// Portable zip of the older, published release.
    pub old_portable_zip: PathBuf,
    /// Version of that release (the version its client compares the package with).
    pub old_version: Version,
    /// The new signed `.tupdate`.
    pub package: PathBuf,
    /// 64-character hex Ed25519 public key the old client trusts.
    pub public_key_hex: String,
    /// Profile copied into the working dir before the update (an old release's profile fixture).
    pub profile: Option<PathBuf>,
    /// Scratch directory that receives the copy (`app`), the working dir (`work`) and the per-user
    /// data dirs (`env`); those three must not exist yet.
    pub scratch: PathBuf,
    /// How long each child process may run.
    pub timeout: Duration,
}

/// What [`run_update_path_check`] observed.
#[derive(Debug, Clone)]
pub struct UpdatePathReport {
    /// Version the package declares (the version the updated copy must report).
    pub new_version: Version,
    /// Files the old client staged from the package.
    pub staged_files: Vec<String>,
    /// Exit code of the old `rustgram-updater.exe`.
    pub updater_exit: Option<i32>,
    /// Exit code of `rustgram.exe --smoke-test` (`None` if it was killed after the timeout).
    pub smoke_exit: Option<i32>,
    /// Text of `smoke_test_report.txt`, if written.
    pub smoke_report: Option<String>,
    /// Obsolete files still in the install folder (the app deletes them at its next normal start).
    pub obsolete_left: Vec<String>,
}

/// Picks the release clients update from: the highest published version below `new_version`.
///
/// `listing` holds one `<tag>` per line (`gh release list --exclude-drafts --json tagName --jq
/// '.[].tagName'`); tags that are not `v<semver>` are ignored.
#[must_use]
pub fn pick_previous_release(listing: &str, new_version: &Version) -> Option<(String, Version)> {
    listing
        .lines()
        .map(str::trim)
        .filter_map(|tag| {
            let version = Version::parse(tag.strip_prefix('v')?).ok()?;
            Some((tag.to_owned(), version))
        })
        .filter(|(_, version)| version < new_version)
        .max_by(|a, b| a.1.cmp(&b.1))
}

/// Downloads the portable zip of the release before the package's version from GitHub with the
/// `gh` CLI into `download_dir`. Returns `None` when no published release is older than the
/// package (a test build with a low version).
pub fn download_previous_release(
    repo: &str,
    package: &Path,
    download_dir: &Path,
) -> Result<Option<(PathBuf, Version)>, XtaskError> {
    let new_version = parse_envelope(&fs::read(package)?)?.version;
    let listing = run_gh(&[
        "release",
        "list",
        "-R",
        repo,
        "--exclude-drafts",
        "--limit",
        "100",
        "--json",
        "tagName",
        "--jq",
        ".[].tagName",
    ])?;
    let Some((tag, version)) = pick_previous_release(&listing, &new_version) else {
        return Ok(None);
    };
    println!("update-path-check: previous published release of {repo}: {tag}");
    fs::create_dir_all(download_dir)?;
    let dir = download_dir.to_string_lossy().into_owned();
    run_gh(&[
        "release",
        "download",
        &tag,
        "-R",
        repo,
        "-p",
        "*-win-x64-portable.zip",
        "-D",
        &dir,
    ])?;
    let zip = fs::read_dir(download_dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.to_string_lossy().ends_with("-win-x64-portable.zip"))
        .ok_or_else(|| {
            XtaskError::MissingArtifact(format!("{tag} has no *-win-x64-portable.zip asset"))
        })?;
    Ok(Some((zip, version)))
}

fn run_gh(args: &[&str]) -> Result<String, XtaskError> {
    let output = Command::new("gh").args(args).output()?;
    if !output.status.success() {
        return Err(XtaskError::CommandFailed {
            program: format!(
                "gh {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            status: output.status.to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Runs the check and returns its observations; `Err` when the updated copy does not start, does
/// not pass the smoke test, or reports another version than the package.
pub fn run_update_path_check(opts: &UpdatePathCheck) -> Result<UpdatePathReport, XtaskError> {
    prepare_scratch(&opts.scratch)?;
    let scratch = std::path::absolute(&opts.scratch)?;
    let app_dir = scratch.join("app");
    let work_dir = scratch.join("work");
    let env_dir = scratch.join("env");
    fs::create_dir_all(&work_dir)?;
    fs::create_dir_all(env_dir.join("AppData"))?;
    fs::create_dir_all(env_dir.join("LocalAppData"))?;

    println!(
        "update-path-check: unpacking {} (v{})",
        opts.old_portable_zip.display(),
        opts.old_version
    );
    unzip_portable(&opts.old_portable_zip, &app_dir)?;
    if !app_dir.join(DEFAULT_EXE_NAME).is_file() || !app_dir.join(UPDATER_EXE_NAME).is_file() {
        return Err(XtaskError::MissingArtifact(format!(
            "{DEFAULT_EXE_NAME} or {UPDATER_EXE_NAME} not at the root of {}",
            opts.old_portable_zip.display()
        )));
    }
    if let Some(profile) = &opts.profile {
        copy_dir(profile, &work_dir)?;
    }

    // 1. Stage the package the way the old app does.
    let package = fs::read(&opts.package)?;
    let key = verifying_key_from_hex(&opts.public_key_hex)?;
    let verified = verify_package(&package, &key, &opts.old_version, true, Target::WIN_X64)?;
    let new_version = verified.envelope.version.clone();
    let payload = package_entries(&package)?;
    let staged_dir = unpack_verified_package(&verified, &work_dir)?;
    let promoted = promote_ready_updater(&work_dir, &app_dir)?;
    let staged_files = list_files(&staged_dir)?;
    println!(
        "update-path-check: staged v{new_version} in {}: {}{}",
        staged_dir.display(),
        staged_files.join(", "),
        if promoted {
            "; rustgram-updater.exe promoted into the install folder first"
        } else {
            ""
        }
    );

    // 2. The installed release's updater, started like "Restart to update" does it.
    let updater = app_dir.join(UPDATER_EXE_NAME);
    let work_arg = work_dir.to_string_lossy().into_owned();
    let updater_status = run_with_timeout(
        Command::new(&updater)
            .args(["-update", "-justupdate", "-debug", "-workdir", &work_arg])
            .current_dir(&work_dir),
        &env_dir,
        opts.timeout,
    )?;
    let updater_exit = updater_status.and_then(|s| s.code());
    println!("update-path-check: rustgram-updater.exe exit code {updater_exit:?}");
    print_debug_logs(&work_dir);
    if updater_exit != Some(0) {
        return Err(XtaskError::InconsistentRelease(format!(
            "the v{} updater did not install the package (exit code {updater_exit:?})",
            opts.old_version
        )));
    }
    if work_dir
        .join(TUPDATES_DIR_NAME)
        .join("temp")
        .join("ready")
        .exists()
    {
        return Err(XtaskError::InconsistentRelease(
            "the update is still staged after the updater exited".to_owned(),
        ));
    }
    let mut differing = Vec::new();
    for (name, bytes) in &payload {
        if fs::read(app_dir.join(name)).ok().as_deref() != Some(bytes.as_slice()) {
            differing.push(name.clone());
        }
    }
    if !differing.is_empty() {
        return Err(XtaskError::InconsistentRelease(format!(
            "after the update these install files differ from the package: {}",
            differing.join(", ")
        )));
    }
    println!(
        "update-path-check: install folder now holds all {} package files",
        payload.len()
    );

    // 3. The updated copy must start and open the profile.
    let smoke_status = run_with_timeout(
        Command::new(app_dir.join(DEFAULT_EXE_NAME))
            .args(["--smoke-test", "-workdir", &work_arg])
            .current_dir(&work_dir),
        &env_dir,
        opts.timeout,
    )?;
    let smoke_exit = smoke_status.and_then(|s| s.code());
    let smoke_report = fs::read_to_string(work_dir.join(SMOKE_REPORT_NAME)).ok();
    let obsolete_left: Vec<String> = OBSOLETE_INSTALL_FILES
        .iter()
        .filter(|name| app_dir.join(name).is_file())
        .map(|name| (*name).to_owned())
        .collect();
    let report = UpdatePathReport {
        new_version,
        staged_files,
        updater_exit,
        smoke_exit,
        smoke_report,
        obsolete_left,
    };
    println!(
        "update-path-check: rustgram.exe --smoke-test exit code {}",
        describe_exit(report.smoke_exit)
    );
    match &report.smoke_report {
        Some(text) => println!("--- {SMOKE_REPORT_NAME}\n{}---", text),
        None => println!("update-path-check: no {SMOKE_REPORT_NAME} written"),
    }
    if !report.obsolete_left.is_empty() {
        println!(
            "update-path-check: files of the old release left for the app's next start: {}",
            report.obsolete_left.join(", ")
        );
    }
    check_smoke_result(&report)?;
    println!(
        "update-path-check: PASS: v{} updated to v{} starts and opens the profile",
        opts.old_version, report.new_version
    );
    Ok(report)
}

/// Fails unless the smoke test exited 0, passed, and reported the package version.
pub fn check_smoke_result(report: &UpdatePathReport) -> Result<(), XtaskError> {
    let fail = |why: String| Err(XtaskError::InconsistentRelease(why));
    if report.smoke_exit != Some(0) {
        return fail(format!(
            "the updated rustgram.exe did not start or failed the smoke test (exit code {})",
            describe_exit(report.smoke_exit)
        ));
    }
    let Some(text) = &report.smoke_report else {
        return fail(format!("the updated app wrote no {SMOKE_REPORT_NAME}"));
    };
    if !text.lines().any(|line| line.trim() == "result: PASS") {
        return fail("the smoke test did not report `result: PASS`".to_owned());
    }
    let expected = format!("Rustgram Desktop {}:", report.new_version);
    if !text
        .lines()
        .next()
        .is_some_and(|l| l.starts_with(&expected))
    {
        return fail(format!(
            "the updated app does not report v{} (first report line: {:?})",
            report.new_version,
            text.lines().next().unwrap_or("")
        ));
    }
    Ok(())
}

/// Exit code as decimal and, for Windows NTSTATUS values, hex
/// (`0xC0000135` = STATUS_DLL_NOT_FOUND: a DLL the exe imports is missing).
fn describe_exit(code: Option<i32>) -> String {
    match code {
        Some(c) if c < 0 => format!("{c} (0x{:08X})", c as u32),
        Some(c) => c.to_string(),
        None => "none (killed after the timeout)".to_owned(),
    }
}

/// The check writes `app`, `work` and `env` under `scratch`; none of them may exist yet.
fn prepare_scratch(scratch: &Path) -> Result<(), XtaskError> {
    for sub in ["app", "work", "env"] {
        if scratch.join(sub).exists() {
            return Err(XtaskError::InvalidArgument(format!(
                "{} already exists; use a fresh scratch dir",
                scratch.join(sub).display()
            )));
        }
    }
    fs::create_dir_all(scratch)?;
    Ok(())
}

/// `(name, bytes)` of every file in the package's archive.
fn package_entries(package: &[u8]) -> Result<Vec<(String, Vec<u8>)>, XtaskError> {
    let envelope = parse_envelope(package)?;
    let mut archive = ZipArchive::new(Cursor::new(&envelope.payload))
        .map_err(|e| XtaskError::Zip(e.to_string()))?;
    let mut out = Vec::new();
    for idx in 0..archive.len() {
        let mut entry = archive
            .by_index(idx)
            .map_err(|e| XtaskError::Zip(e.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry
            .name()
            .map_err(|e| XtaskError::Zip(e.to_string()))?
            .into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        out.push((name, bytes));
    }
    Ok(out)
}

fn unzip_portable(zip_path: &Path, dest: &Path) -> Result<(), XtaskError> {
    let mut archive =
        ZipArchive::new(File::open(zip_path)?).map_err(|e| XtaskError::Zip(e.to_string()))?;
    for idx in 0..archive.len() {
        let mut entry = archive
            .by_index(idx)
            .map_err(|e| XtaskError::Zip(e.to_string()))?;
        let Some(rel) = entry.enclosed_name() else {
            return Err(XtaskError::Zip(format!(
                "unsafe path in {}",
                zip_path.display()
            )));
        };
        let out = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        fs::write(&out, bytes)?;
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), XtaskError> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Relative paths of the files under `dir`, sorted, `/`-separated.
fn list_files(dir: &Path) -> Result<Vec<String>, XtaskError> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(rel) = path.strip_prefix(dir) {
                let parts: Vec<String> = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                out.push(parts.join("/"));
            }
        }
    }
    out.sort();
    Ok(out)
}

fn print_debug_logs(work_dir: &Path) {
    let Ok(entries) = fs::read_dir(work_dir.join("DebugLogs")) else {
        return;
    };
    for entry in entries.flatten() {
        if let Ok(text) = fs::read_to_string(entry.path()) {
            println!("--- {}\n{}---", entry.file_name().to_string_lossy(), text);
        }
    }
}

/// Runs `cmd` with the per-user data dirs inside `env_dir`; kills it after `timeout` and returns
/// `None` then.
fn run_with_timeout(
    cmd: &mut Command,
    env_dir: &Path,
    timeout: Duration,
) -> Result<Option<ExitStatus>, XtaskError> {
    suppress_loader_error_dialogs();
    let mut child = cmd
        .env("APPDATA", env_dir.join("AppData"))
        .env("LOCALAPPDATA", env_dir.join("LocalAppData"))
        .env("PATH", system_only_path())
        .spawn()?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// `PATH` for the child processes: the Windows system folders only. Windows also looks for a DLL
/// in every `PATH` folder, and the build machine has FFmpeg there (the CI's setup-ffmpeg adds its
/// `bin`), so with the inherited `PATH` an install that lacks a DLL still starts. The first local
/// run of this check passed v0.2.0-alpha.4 that way.
#[must_use]
pub fn system_only_path() -> String {
    if cfg!(windows) {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
        format!(r"{root}\System32;{root};{root}\System32\Wbem")
    } else {
        "/usr/bin:/bin".to_owned()
    }
}

/// A missing DLL makes Windows show a "System Error" box and keep the child alive until someone
/// clicks it. Children inherit the error mode, so with `SEM_FAILCRITICALERRORS` the child exits
/// with `STATUS_DLL_NOT_FOUND` instead.
fn suppress_loader_error_dialogs() {
    #[cfg(windows)]
    {
        const SEM_FAILCRITICALERRORS: u32 = 0x0001;
        const SEM_NOGPFAULTERRORBOX: u32 = 0x0002;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn SetErrorMode(mode: u32) -> u32;
        }
        // SAFETY: SetErrorMode only changes this process's error-mode flags.
        unsafe {
            SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(exit: Option<i32>, text: Option<&str>) -> UpdatePathReport {
        UpdatePathReport {
            new_version: Version::parse("0.2.0-alpha.5").expect("version"),
            staged_files: Vec::new(),
            updater_exit: Some(0),
            smoke_exit: exit,
            smoke_report: text.map(str::to_owned),
            obsolete_left: Vec::new(),
        }
    }

    #[test]
    fn smoke_result_needs_exit_zero_pass_and_the_new_version() {
        let pass =
            "Rustgram Desktop 0.2.0-alpha.5: storage smoke test\naccounts: 1\nresult: PASS\n";
        assert!(check_smoke_result(&report(Some(0), Some(pass))).is_ok());
        // v0.2.0-alpha.4's package: the updated exe cannot load avutil-61.dll.
        let dll_missing = 0xC000_0135_u32 as i32;
        let err = check_smoke_result(&report(Some(dll_missing), None)).expect_err("must fail");
        assert!(err.to_string().contains("0xC0000135"), "{err}");
        let old = pass.replace("0.2.0-alpha.5", "0.2.0-alpha.2");
        assert!(check_smoke_result(&report(Some(0), Some(&old))).is_err());
        let failed = pass.replace("result: PASS", "result: FAIL");
        assert!(check_smoke_result(&report(Some(0), Some(&failed))).is_err());
        assert!(check_smoke_result(&report(None, None)).is_err());
    }

    #[test]
    fn scratch_must_not_hold_an_earlier_run() {
        let dir = tempfile::TempDir::new().expect("temp");
        assert!(prepare_scratch(&dir.path().join("new")).is_ok());
        fs::create_dir(dir.path().join("download")).expect("download dir");
        assert!(prepare_scratch(dir.path()).is_ok());
        fs::create_dir(dir.path().join("work")).expect("work dir");
        assert!(prepare_scratch(dir.path()).is_err());
    }

    #[test]
    fn previous_release_is_the_highest_published_version_below_the_package() {
        let listing = "v0.2.0-alpha.2
v0.2.0-alpha.1
v0.1.0-alpha.2
not-a-version
";
        let new = Version::parse("0.2.0-alpha.5").expect("version");
        let (tag, version) = pick_previous_release(listing, &new).expect("previous");
        assert_eq!(tag, "v0.2.0-alpha.2");
        assert_eq!(version, Version::parse("0.2.0-alpha.2").expect("version"));
        // The package's own release (a re-run after publishing) is not "previous".
        let same = Version::parse("0.2.0-alpha.2").expect("version");
        assert_eq!(
            pick_previous_release(listing, &same).map(|(t, _)| t),
            Some("v0.2.0-alpha.1".to_owned())
        );
        // A test build numbered below every release has nothing to update from.
        let test_build = Version::parse("0.0.0-test.1").expect("version");
        assert!(pick_previous_release(listing, &test_build).is_none());
    }
}
