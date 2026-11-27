//! End-to-end auto-update integration tests against a local in-process fake HTTP/1.1 server
//! (never touches the internet):
//! - Check -> download -> verify Ed25519 signature + SHA-256 hash -> unpack into `tupdates/temp`
//!   -> promote updater -> execute the compiled `rustgram-updater` binary to replace files and
//!   relaunch the updated target executable with `-noupdate` + forwarded flags.
//! - Reject bad Ed25519 signature (wrong key & tampered payload) before unpacking.
//! - Reject mismatched SHA-256 digest before unpacking.
//! - Roll back replaced and newly created files when applying fails midway.
//! - Recursive directory packing (`pack_directory`) porting `_other/packer.cpp`.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use tempfile::TempDir;

use tg_updater::apply::{ApplyRetryPolicy, apply_staged_update_with_policy};
use tg_updater::checker::{
    UpdateChecker, UpdateConfig, UpdaterLaunchOptions, build_updater_launch_args, has_ready_update,
};
use tg_updater::github::LocalHttpClient;
use tg_updater::package::{
    pack_directory, pack_update, sha256_digest, unpack_verified_package, verify_package,
};
use tg_updater::version::{Target, UpdateChannel, Version};
use tg_updater::{DEFAULT_EXE_NAME, UPDATER_EXE_NAME, UpdateError};

struct FakeHttpServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    requests: Arc<Mutex<Vec<String>>>,
}

impl FakeHttpServer {
    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn recorded_requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for FakeHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking `listener.incoming()` loop.
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn dev_signing_key() -> SigningKey {
    SigningKey::generate(&mut OsRng)
}

#[test]
#[cfg_attr(not(windows), ignore = "spawns the Windows updater executable")]
fn e2e_fake_http_check_download_verify_unpack_apply_and_relaunch() {
    let signing_key = dev_signing_key();
    let verifying_key = signing_key.verifying_key();

    let tmp = TempDir::new().unwrap();
    let workdir = tmp.path().join("workdir");
    let install_dir = tmp.path().join("install");
    fs::create_dir_all(&workdir).unwrap();
    fs::create_dir_all(&install_dir).unwrap();

    // Real compiled `rustgram-updater` binary built by cargo for this test crate.
    let real_updater_bin = Path::new(env!("CARGO_BIN_EXE_rustgram-updater"));
    let real_updater_bytes = fs::read(real_updater_bin).expect("read compiled rustgram-updater");

    // Seed v0.1.0 installation with an old app script, old asset, and old updater.
    let target_exe_name = if cfg!(windows) {
        "rustgram_target.cmd"
    } else {
        "rustgram_target.sh"
    };
    let sentinel_path = workdir.join("relaunched_args.txt");
    fs::write(install_dir.join(target_exe_name), b"@echo old-v0.1.0\r\n").unwrap();
    fs::write(install_dir.join("resources.dat"), b"old-resources-0.1.0").unwrap();
    fs::write(install_dir.join(UPDATER_EXE_NAME), b"old-updater-stub").unwrap();

    // New v0.2.0 app executable script: writes its command-line args to `sentinel_path`.
    let new_app_script = if cfg!(windows) {
        format!("@echo off\r\necho %* > \"{}\"\r\n", sentinel_path.display())
    } else {
        format!("#!/bin/sh\necho \"$@\" > \"{}\"\n", sentinel_path.display())
    };

    let target = Target::WIN_X64;
    let next_ver = Version::parse("0.2.0").unwrap();
    let asset_name = format!("rustgram-0.2.0-{}.tupdate", target.asset_suffix());
    let package_bytes = pack_update(
        UpdateChannel::Stable,
        target,
        &next_ver,
        1_760_000_000,
        [
            (DEFAULT_EXE_NAME, new_app_script.as_bytes()),
            (UPDATER_EXE_NAME, real_updater_bytes.as_slice()),
            ("resources.dat", b"new-resources-0.2.0".as_slice()),
            ("modules/extra.txt", b"module-v0.2.0".as_slice()),
        ],
        &signing_key,
    )
    .unwrap();
    let pkg_sha256_hex = hex::encode(sha256_digest(&package_bytes));
    let srv = start_release_server(&asset_name, &package_bytes, Some(&pkg_sha256_hex));

    // 1. Check -> Download -> Verify SHA-256 + Ed25519 -> Unpack into `tupdates/temp` -> Promote updater.
    let checker = UpdateChecker::new(
        LocalHttpClient::default(),
        UpdateConfig {
            github_repo: "rustgram/rustgram".into(),
            api_base_url: Some(srv.base_url()),
            current_version: Version::parse("0.1.0").unwrap(),
            allow_beta: false,
            target,
            working_dir: workdir.clone(),
            install_dir: install_dir.clone(),
            verifying_key,
        },
    );

    let staged = checker
        .check_and_stage()
        .expect("check_and_stage succeeds")
        .expect("update v0.2.0 available");
    assert_eq!(staged.version, next_ver);
    assert!(staged.updater_promoted);
    assert!(has_ready_update(&workdir));

    // Confirm real HTTP requests hit the local fake server (releases, SHA256SUMS, package).
    let reqs = srv.recorded_requests();
    assert!(
        reqs.iter()
            .any(|p| p.contains("/repos/rustgram/rustgram/releases"))
    );
    assert!(reqs.iter().any(|p| p.ends_with("SHA256SUMS.txt")));
    assert!(reqs.iter().any(|p| p.ends_with(&asset_name)));

    // 2. Run the promoted compiled `rustgram-updater.exe` binary to replace files and relaunch.
    let promoted_updater = install_dir.join(UPDATER_EXE_NAME);
    assert!(promoted_updater.is_file());

    let args = build_updater_launch_args(&UpdaterLaunchOptions {
        parent_pid: None,
        parent_created: None,
        workdir: Some(workdir.clone()),
        key: Some("e2e_profile".into()),
        exe_name: Some(target_exe_name.to_owned()),
        autostart: false,
        debug: true,
        start_in_tray: false,
        just_update: false,
        app_version: None,
    });

    let output = Command::new(&promoted_updater)
        .args(&args)
        .current_dir(&install_dir)
        .output()
        .expect("spawn promoted rustgram-updater.exe");
    assert!(
        output.status.success(),
        "rustgram-updater failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // 3. Verify files were replaced and `tupdates` was cleaned up.
    assert_eq!(
        fs::read(install_dir.join("resources.dat")).unwrap(),
        b"new-resources-0.2.0"
    );
    assert_eq!(
        fs::read(install_dir.join("modules/extra.txt")).unwrap(),
        b"module-v0.2.0"
    );
    assert_eq!(
        fs::read_to_string(install_dir.join(target_exe_name)).unwrap(),
        new_app_script
    );
    assert!(!has_ready_update(&workdir));
    assert!(!workdir.join("tupdates").exists());

    // Debug log created by `rustgram-updater -debug`.
    let debug_logs: Vec<_> = fs::read_dir(workdir.join("DebugLogs"))
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(!debug_logs.is_empty(), "expected updater debug log file");

    // 4. Verify the updated executable was actually relaunched with `-noupdate` and forwarded args.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !sentinel_path.is_file() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        sentinel_path.is_file(),
        "expected relaunched app to create {}",
        sentinel_path.display()
    );
    let relaunched_args = fs::read_to_string(&sentinel_path).unwrap();
    assert!(
        relaunched_args.contains("-noupdate"),
        "expected -noupdate in '{relaunched_args}'"
    );
    assert!(
        relaunched_args.contains("-debug"),
        "expected -debug in '{relaunched_args}'"
    );
    assert!(
        relaunched_args.contains("-key") && relaunched_args.contains("e2e_profile"),
        "expected -key e2e_profile in '{relaunched_args}'"
    );
}

#[test]
fn e2e_fake_http_rejects_bad_signature_without_unpacking() {
    let trusted_key = dev_signing_key();
    let attacker_key = dev_signing_key();

    let tmp = TempDir::new().unwrap();
    let workdir = tmp.path().join("workdir");
    let install_dir = tmp.path().join("install");
    fs::create_dir_all(&workdir).unwrap();
    fs::create_dir_all(&install_dir).unwrap();
    fs::write(install_dir.join(DEFAULT_EXE_NAME), b"original-v0.1.0").unwrap();

    let target = Target::WIN_X64;
    let asset_name = format!("rustgram-0.2.0-{}.tupdate", target.asset_suffix());
    let wrong_key_pkg = pack_update(
        UpdateChannel::Stable,
        target,
        &Version::parse("0.2.0").unwrap(),
        1_760_000_000,
        [(DEFAULT_EXE_NAME, b"malicious-binary".as_slice())],
        &attacker_key,
    )
    .unwrap();
    let sha_hex = hex::encode(sha256_digest(&wrong_key_pkg));

    let srv = start_release_server(&asset_name, &wrong_key_pkg, Some(&sha_hex));
    let checker = UpdateChecker::new(
        LocalHttpClient::default(),
        UpdateConfig {
            github_repo: "rustgram/rustgram".into(),
            api_base_url: Some(srv.base_url()),
            current_version: Version::parse("0.1.0").unwrap(),
            allow_beta: false,
            target,
            working_dir: workdir.clone(),
            install_dir: install_dir.clone(),
            verifying_key: trusted_key.verifying_key(),
        },
    );

    let err = checker.check_and_stage().unwrap_err();
    assert!(
        matches!(err, UpdateError::InvalidSignature),
        "expected InvalidSignature, got {err:?}"
    );
    assert!(!has_ready_update(&workdir));
    assert!(!workdir.join("tupdates").exists());
    assert_eq!(
        fs::read(install_dir.join(DEFAULT_EXE_NAME)).unwrap(),
        b"original-v0.1.0"
    );

    // Also test a tampered payload signed with the trusted key.
    let mut tampered = pack_update(
        UpdateChannel::Stable,
        target,
        &Version::parse("0.2.0").unwrap(),
        1_760_000_000,
        [(DEFAULT_EXE_NAME, b"legit-binary".as_slice())],
        &trusted_key,
    )
    .unwrap();
    let last = tampered.len() - 1;
    tampered[last] ^= 0x5A;
    let srv_tampered = start_release_server(&asset_name, &tampered, None);
    let checker_tampered = UpdateChecker::new(
        LocalHttpClient::default(),
        UpdateConfig {
            github_repo: "rustgram/rustgram".into(),
            api_base_url: Some(srv_tampered.base_url()),
            current_version: Version::parse("0.1.0").unwrap(),
            allow_beta: false,
            target,
            working_dir: workdir.clone(),
            install_dir,
            verifying_key: trusted_key.verifying_key(),
        },
    );
    assert!(matches!(
        checker_tampered.check_and_stage(),
        Err(UpdateError::InvalidSignature)
    ));
    assert!(!has_ready_update(&workdir));
}

#[test]
fn e2e_fake_http_rejects_bad_sha256_hash() {
    let signing_key = dev_signing_key();
    let tmp = TempDir::new().unwrap();
    let workdir = tmp.path().join("workdir");
    let install_dir = tmp.path().join("install");
    fs::create_dir_all(&workdir).unwrap();
    fs::create_dir_all(&install_dir).unwrap();

    let target = Target::WIN_X64;
    let asset_name = format!("rustgram-0.2.0-{}.tupdate", target.asset_suffix());
    let pkg = pack_update(
        UpdateChannel::Stable,
        target,
        &Version::parse("0.2.0").unwrap(),
        1_760_000_000,
        [(DEFAULT_EXE_NAME, b"valid-signed-binary".as_slice())],
        &signing_key,
    )
    .unwrap();

    let wrong_sha = "00".repeat(32);
    let srv = start_release_server(&asset_name, &pkg, Some(&wrong_sha));
    let checker = UpdateChecker::new(
        LocalHttpClient::default(),
        UpdateConfig {
            github_repo: "rustgram/rustgram".into(),
            api_base_url: Some(srv.base_url()),
            current_version: Version::parse("0.1.0").unwrap(),
            allow_beta: false,
            target,
            working_dir: workdir.clone(),
            install_dir,
            verifying_key: signing_key.verifying_key(),
        },
    );

    let err = checker.check_and_stage().unwrap_err();
    assert!(
        matches!(err, UpdateError::HashMismatch { .. }),
        "expected HashMismatch, got {err:?}"
    );
    assert!(!has_ready_update(&workdir));
}

#[test]
fn apply_rolls_back_replaced_and_created_files_when_copy_fails_midway() {
    let signing_key = dev_signing_key();
    let tmp = TempDir::new().unwrap();
    let workdir = tmp.path().join("workdir");
    let install_dir = tmp.path().join("install");
    fs::create_dir_all(&workdir).unwrap();
    fs::create_dir_all(&install_dir).unwrap();

    // Seed v0.1.0 installation files.
    fs::write(install_dir.join("01_core.dll"), b"v0.1.0-core-dll").unwrap();
    fs::write(install_dir.join("02_app.exe"), b"v0.1.0-app-exe").unwrap();

    // Place a non-empty directory at `03_blocked.dat` so copying a regular file there fails
    // deterministically AFTER `01_core.dll`, `02_app.exe`, and `02_new_dir/added.txt` succeed.
    fs::create_dir_all(install_dir.join("03_blocked.dat")).unwrap();
    fs::write(install_dir.join("03_blocked.dat/keep.txt"), b"blocker").unwrap();

    let pkg = pack_update(
        UpdateChannel::Stable,
        Target::WIN_X64,
        &Version::parse("0.2.0").unwrap(),
        1_760_000_000,
        [
            ("01_core.dll", b"v0.2.0-core-dll".as_slice()),
            ("02_app.exe", b"v0.2.0-app-exe".as_slice()),
            ("02_new_dir/added.txt", b"brand-new-file".as_slice()),
            ("03_blocked.dat", b"will-fail-to-copy".as_slice()),
            ("04_after.txt", b"never-copied".as_slice()),
        ],
        &signing_key,
    )
    .unwrap();

    let verified = verify_package(
        &pkg,
        &signing_key.verifying_key(),
        &Version::parse("0.1.0").unwrap(),
        false,
        Target::WIN_X64,
    )
    .unwrap();
    unpack_verified_package(&verified, &workdir).unwrap();

    let err = apply_staged_update_with_policy(
        &workdir,
        &install_dir,
        DEFAULT_EXE_NAME,
        UPDATER_EXE_NAME,
        false,
        ApplyRetryPolicy::fast(),
    )
    .unwrap_err();

    assert!(
        matches!(
            err,
            UpdateError::ApplyFailed {
                rolled_back: true,
                ..
            }
        ),
        "expected ApplyFailed with rolled_back=true, got {err:?}"
    );

    // Previously overwritten files were restored to their exact v0.1.0 contents.
    assert_eq!(
        fs::read(install_dir.join("01_core.dll")).unwrap(),
        b"v0.1.0-core-dll"
    );
    assert_eq!(
        fs::read(install_dir.join("02_app.exe")).unwrap(),
        b"v0.1.0-app-exe"
    );
    // Newly created files and directories from the failed update were removed.
    assert!(!install_dir.join("02_new_dir/added.txt").exists());
    assert!(!install_dir.join("02_new_dir").exists());
    assert!(!install_dir.join("04_after.txt").exists());
    // Blocker directory remains untouched.
    assert_eq!(
        fs::read(install_dir.join("03_blocked.dat/keep.txt")).unwrap(),
        b"blocker"
    );
}

#[test]
fn pack_directory_skips_control_files_and_verifies_cleanly() {
    let signing_key = dev_signing_key();
    let tmp = TempDir::new().unwrap();
    let src_dir = tmp.path().join("dist_input");
    let workdir = tmp.path().join("workdir");
    fs::create_dir_all(src_dir.join("sub")).unwrap();
    fs::create_dir_all(src_dir.join("tdata")).unwrap();

    fs::write(src_dir.join(DEFAULT_EXE_NAME), b"packed-exe").unwrap();
    fs::write(src_dir.join("sub/lib.dll"), b"packed-dll").unwrap();
    // Control files that must be skipped by `pack_directory`.
    fs::write(src_dir.join("ready"), b"ignored").unwrap();
    fs::write(src_dir.join("tdata/version"), b"ignored").unwrap();

    let packed = pack_directory(
        &src_dir,
        UpdateChannel::Stable,
        Target::WIN_X64,
        &Version::parse("0.2.0").unwrap(),
        1_760_000_000,
        &signing_key,
    )
    .unwrap();

    let verified = verify_package(
        &packed,
        &signing_key.verifying_key(),
        &Version::parse("0.1.0").unwrap(),
        false,
        Target::WIN_X64,
    )
    .unwrap();
    let staging = unpack_verified_package(&verified, &workdir).unwrap();
    assert_eq!(
        fs::read(staging.join(DEFAULT_EXE_NAME)).unwrap(),
        b"packed-exe"
    );
    assert_eq!(
        fs::read(staging.join("sub/lib.dll")).unwrap(),
        b"packed-dll"
    );
    assert!(staging.join("ready").is_file());
    assert!(staging.join("tdata/version").is_file());
}

/// Starts a loopback HTTP/1.1 server serving a GitHub-style `/repos/rustgram/rustgram/releases`
/// endpoint pointing to `/downloads/<asset_name>` on the same ephemeral port.
fn start_release_server(
    asset_name: &str,
    package_bytes: &[u8],
    sha256_hex: Option<&str>,
) -> FakeHttpServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local_addr");
    let base = format!("http://{addr}");
    let pkg_path = format!("/downloads/{asset_name}");
    let sums_path = "/downloads/SHA256SUMS.txt".to_owned();

    let mut assets_json = format!(
        r#"{{"name": "{asset_name}", "browser_download_url": "{base}{pkg_path}", "size": {}}}"#,
        package_bytes.len()
    );
    let mut routes = HashMap::new();
    if let Some(hex_digest) = sha256_hex {
        let sums_body = format!("{hex_digest}  {asset_name}\n").into_bytes();
        assets_json.push_str(&format!(
            r#", {{"name": "SHA256SUMS.txt", "browser_download_url": "{base}{sums_path}", "size": {}}}"#,
            sums_body.len()
        ));
        routes.insert(sums_path, (200, "text/plain".to_owned(), sums_body));
    }

    let releases_json = format!(
        r#"[{{
            "tag_name": "v0.2.0",
            "name": "Rustgram 0.2.0",
            "draft": false,
            "prerelease": false,
            "body": "Automated test release",
            "assets": [{assets_json}]
        }}]"#
    );
    routes.insert(
        "/repos/rustgram/rustgram/releases?per_page=20".to_owned(),
        (
            200,
            "application/json".to_owned(),
            releases_json.into_bytes(),
        ),
    );
    routes.insert(
        pkg_path,
        (
            200,
            "application/octet-stream".to_owned(),
            package_bytes.to_vec(),
        ),
    );

    let stop = Arc::new(AtomicBool::new(false));
    let stop_bg = Arc::clone(&stop);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let req_bg = Arc::clone(&requests);

    let handle = thread::spawn(move || {
        for stream_res in listener.incoming() {
            if stop_bg.load(Ordering::SeqCst) {
                break;
            }
            let Ok(mut stream) = stream_res else {
                continue;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let path = {
                let mut reader = BufReader::new(&stream);
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() {
                    continue;
                }
                loop {
                    let mut h = String::new();
                    match reader.read_line(&mut h) {
                        Ok(0) | Err(_) => break,
                        Ok(_) if h == "\r\n" || h == "\n" => break,
                        Ok(_) => {}
                    }
                }
                request_line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .to_owned()
            };

            if let Ok(mut log) = req_bg.lock() {
                log.push(path.clone());
            }

            let (status, content_type, body) = routes.get(&path).cloned().unwrap_or_else(|| {
                (
                    404,
                    "text/plain".to_owned(),
                    format!("not found: {path}").into_bytes(),
                )
            });
            let reason = if status == 200 { "OK" } else { "Not Found" };
            let header = format!(
                "HTTP/1.1 {status} {reason}\r\n\
                 Content-Type: {content_type}\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }
    });

    FakeHttpServer {
        addr,
        stop,
        handle: Some(handle),
        requests,
    }
}
