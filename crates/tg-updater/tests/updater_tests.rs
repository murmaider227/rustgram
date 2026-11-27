//! Offline unit and integration tests for `tg-updater`:
//! - SemVer parsing & comparison (stable, beta, numeric prerelease, tdesktop numeric)
//! - GitHub Releases JSON selection (beta channel flag, drafts, target filtering)
//! - Ed25519 package packing & verification (valid, tampered payload, tampered header,
//!   wrong key, placeholder key rejection, cross-target rejection, beta-on-stable rejection,
//!   anti-downgrade)
//! - Path safety (traversal, Windows device names, case-insensitive collisions)
//! - End-to-end staging into `tupdates/temp`, updater promotion, file replacement,
//!   self-overwrite blocking, `-finishprocess` handoff, and `-noupdate` relaunch arguments.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ed25519_dalek::{Signer, SigningKey};
use rand::rngs::OsRng;
use tempfile::TempDir;

use tg_updater::apply::{
    AppLauncher, ProcessWaiter, UpdaterArgs, apply_staged_update, build_relaunch_args, parse_args,
    run_updater,
};
use tg_updater::checker::{
    UpdateChecker, UpdateConfig, UpdaterLaunchOptions, build_updater_launch_args, clear_updates,
    has_ready_update,
};
use tg_updater::github::{HttpClient, parse_releases_json, select_release};
use tg_updater::package::{
    is_safe_archive_path, pack_update, promote_ready_updater, read_version_file,
    unpack_verified_package, verify_detached_signature, verify_package,
};
use tg_updater::version::{
    PlatformArch, PlatformOs, Target, UpdateChannel, Version, channel_allows_update,
};
use tg_updater::{
    DEFAULT_EXE_NAME, RELEASE_SIGNING_PUBLIC_KEY, UPDATER_EXE_NAME, UpdateError,
    is_placeholder_public_key, release_verifying_key,
};

fn dev_keypair() -> SigningKey {
    SigningKey::generate(&mut OsRng)
}

#[derive(Default)]
struct MockHttp {
    responses: HashMap<String, Vec<u8>>,
}

impl HttpClient for MockHttp {
    fn get_bytes(
        &self,
        url: &str,
        _headers: &[(&str, &str)],
        max_bytes: usize,
    ) -> Result<Vec<u8>, UpdateError> {
        let data = self
            .responses
            .get(url)
            .ok_or_else(|| UpdateError::Network(format!("unexpected URL {url}")))?;
        if data.len() > max_bytes {
            return Err(UpdateError::ResponseTooLarge { limit: max_bytes });
        }
        Ok(data.clone())
    }
}

#[derive(Default)]
struct RecordingWaiter {
    waited: Mutex<Vec<(u32, u64)>>,
}

impl ProcessWaiter for RecordingWaiter {
    fn wait_for_exit(&self, pid: u32, creation_time: u64) -> Result<(), UpdateError> {
        self.waited
            .lock()
            .expect("mutex poisoned")
            .push((pid, creation_time));
        Ok(())
    }
}

#[derive(Default)]
struct RecordingLauncher {
    launches: Mutex<Vec<(PathBuf, Vec<String>, PathBuf)>>,
}

impl AppLauncher for RecordingLauncher {
    fn launch(&self, exe: &Path, args: &[String], cwd: &Path) -> Result<(), UpdateError> {
        self.launches.lock().expect("mutex poisoned").push((
            exe.to_path_buf(),
            args.to_vec(),
            cwd.to_path_buf(),
        ));
        Ok(())
    }
}

#[test]
fn version_parsing_and_ordering() {
    let v010 = Version::parse("0.1.0").unwrap();
    let v010_v = Version::parse("v0.1.0").unwrap();
    let v020_alpha1 = Version::parse("v0.2.0-alpha.1").unwrap();
    let v020_beta2 = Version::parse("v0.2.0-beta.2").unwrap();
    let v020_beta10 = Version::parse("v0.2.0-beta.10").unwrap();
    let v020_rc1 = Version::parse("0.2.0-rc.1+build.42").unwrap();
    let v020 = Version::parse("0.2.0").unwrap();
    let v100 = Version::parse("1.0.0").unwrap();

    assert_eq!(v010, v010_v);
    assert!(v010 < v020_alpha1);
    assert!(v020_alpha1 < v020_beta2);
    assert!(v020_beta2 < v020_beta10);
    assert!(v020_beta10 < v020_rc1);
    assert!(v020_rc1 < v020);
    assert!(v020 < v100);

    assert!(!v020.is_prerelease());
    assert!(v020_beta2.is_prerelease());
    assert_eq!(v020.to_windows_quad(), "0.2.0.0");

    // Match tdesktop's AppVersion = 7004005 for 7.4.5
    let v745 = Version::parse("7.4.5").unwrap();
    assert_eq!(v745.to_tdesktop_numeric(), 7_004_005);
    assert_eq!(Version::from_tdesktop_numeric(7_004_005), v745);

    // Invalid versions rejected
    for bad in ["", "1.2", "1.2.3.4", "01.2.3", "1.2.3-", "1.2.3+", "1.2.x"] {
        assert!(Version::parse(bad).is_err(), "expected error for '{bad}'");
    }
}

#[test]
fn channel_policy_respects_beta_flag() {
    let current = Version::parse("0.1.0").unwrap();
    let beta = Version::parse("0.2.0-beta.1").unwrap();
    let stable = Version::parse("0.2.0").unwrap();
    let older = Version::parse("0.0.9").unwrap();

    assert!(!channel_allows_update(
        &current,
        &beta,
        UpdateChannel::Beta,
        false
    ));
    assert!(channel_allows_update(
        &current,
        &beta,
        UpdateChannel::Beta,
        true
    ));
    assert!(channel_allows_update(
        &current,
        &stable,
        UpdateChannel::Stable,
        false
    ));
    assert!(channel_allows_update(
        &current,
        &stable,
        UpdateChannel::Stable,
        true
    ));
    assert!(!channel_allows_update(
        &current,
        &current,
        UpdateChannel::Stable,
        true
    ));
    assert!(!channel_allows_update(
        &current,
        &older,
        UpdateChannel::Stable,
        true
    ));
}

#[test]
fn github_release_selection_handles_channels_drafts_and_targets() {
    let json = br#"[
        {
            "tag_name": "v0.4.0",
            "draft": true,
            "prerelease": false,
            "assets": [
                {"name": "rustgram-0.4.0-win-x64.tupdate", "browser_download_url": "https://example.invalid/0.4.0.tupdate", "size": 100}
            ]
        },
        {
            "tag_name": "v0.3.0-beta.1",
            "name": "Rustgram 0.3.0 Beta 1",
            "draft": false,
            "prerelease": true,
            "assets": [
                {"name": "rustgram-0.3.0-beta.1-win-x64.tupdate", "browser_download_url": "https://example.invalid/0.3.0-beta.1.tupdate", "size": 200}
            ]
        },
        {
            "tag_name": "v0.2.0",
            "name": "Rustgram 0.2.0",
            "draft": false,
            "prerelease": false,
            "assets": [
                {"name": "rustgram-0.2.0-linux-x64.tupdate", "browser_download_url": "https://example.invalid/linux.tupdate", "size": 150},
                {"name": "rustgram-0.2.0-win-x64.tupdate", "browser_download_url": "https://example.invalid/0.2.0.tupdate", "size": 150}
            ]
        }
    ]"#;

    let releases = parse_releases_json(json).unwrap();
    let current = Version::parse("0.1.0").unwrap();

    // Stable channel ignores draft 0.4.0 and beta 0.3.0-beta.1 -> picks 0.2.0
    let stable_pick = select_release(&releases, &current, false, Target::WIN_X64).unwrap();
    assert_eq!(stable_pick.version, Version::parse("0.2.0").unwrap());
    assert_eq!(stable_pick.channel, UpdateChannel::Stable);
    assert_eq!(stable_pick.asset_name, "rustgram-0.2.0-win-x64.tupdate");

    // Beta channel picks 0.3.0-beta.1 over 0.2.0
    let beta_pick = select_release(&releases, &current, true, Target::WIN_X64).unwrap();
    assert_eq!(beta_pick.version, Version::parse("0.3.0-beta.1").unwrap());
    assert_eq!(beta_pick.channel, UpdateChannel::Beta);

    // Non-matching target returns None when no arm64 asset exists
    let arm_target = Target {
        os: PlatformOs::Windows,
        arch: PlatformArch::Arm64,
    };
    assert!(select_release(&releases, &current, true, arm_target).is_none());
}

#[test]
fn placeholder_public_key_is_rejected() {
    assert!(is_placeholder_public_key(&RELEASE_SIGNING_PUBLIC_KEY));
    assert!(matches!(
        release_verifying_key(),
        Err(UpdateError::UnconfiguredPublicKey)
    ));
}

#[test]
fn ed25519_package_signature_and_policy_verification() {
    let signing_key = dev_keypair();
    let verifying_key = signing_key.verifying_key();
    let other_key = dev_keypair().verifying_key();

    let current = Version::parse("0.1.0").unwrap();
    let next = Version::parse("0.2.0").unwrap();

    let pkg = pack_update(
        UpdateChannel::Stable,
        Target::WIN_X64,
        &next,
        1_750_000_000,
        [("rustgram.exe", b"new-binary-bytes".as_slice())],
        &signing_key,
    )
    .unwrap();

    // Valid verification succeeds
    let verified = verify_package(&pkg, &verifying_key, &current, false, Target::WIN_X64).unwrap();
    assert_eq!(verified.envelope.version, next);

    // Wrong key fails
    assert!(matches!(
        verify_package(&pkg, &other_key, &current, false, Target::WIN_X64),
        Err(UpdateError::InvalidSignature)
    ));

    // Tampered payload byte fails BEFORE decompression
    let mut tampered_payload = pkg.clone();
    let last = tampered_payload.len() - 1;
    tampered_payload[last] ^= 0xFF;
    assert!(matches!(
        verify_package(
            &tampered_payload,
            &verifying_key,
            &current,
            false,
            Target::WIN_X64
        ),
        Err(UpdateError::InvalidSignature)
    ));

    // Tampered header byte (flipping channel) fails signature
    let mut tampered_header = pkg.clone();
    tampered_header[8] = UpdateChannel::Beta.to_u8();
    assert!(matches!(
        verify_package(
            &tampered_header,
            &verifying_key,
            &current,
            true,
            Target::WIN_X64
        ),
        Err(UpdateError::InvalidSignature)
    ));

    // Target mismatch fails even with valid signature
    let linux_target = Target {
        os: PlatformOs::Linux,
        arch: PlatformArch::X64,
    };
    assert!(matches!(
        verify_package(&pkg, &verifying_key, &current, false, linux_target),
        Err(UpdateError::TargetMismatch { .. })
    ));

    // Anti-downgrade check rejects same or older version
    assert!(matches!(
        verify_package(&pkg, &verifying_key, &next, false, Target::WIN_X64),
        Err(UpdateError::NotNewer { .. })
    ));

    // Beta package rejected when allow_beta is false
    let beta_ver = Version::parse("0.3.0-beta.1").unwrap();
    let beta_pkg = pack_update(
        UpdateChannel::Beta,
        Target::WIN_X64,
        &beta_ver,
        1_750_000_000,
        [("rustgram.exe", b"beta-binary".as_slice())],
        &signing_key,
    )
    .unwrap();
    assert!(matches!(
        verify_package(&beta_pkg, &verifying_key, &current, false, Target::WIN_X64),
        Err(UpdateError::ChannelNotAllowed)
    ));
    assert!(verify_package(&beta_pkg, &verifying_key, &current, true, Target::WIN_X64).is_ok());

    // Detached signature helper
    let msg = b"portable-zip-bytes";
    let sig = signing_key.sign(msg).to_bytes();
    assert!(verify_detached_signature(&verifying_key, msg, &sig).is_ok());
    assert!(matches!(
        verify_detached_signature(&verifying_key, b"modified", &sig),
        Err(UpdateError::InvalidSignature)
    ));
}

#[test]
fn archive_path_safety_ports_tdesktop_checks() {
    assert!(is_safe_archive_path("rustgram.exe"));
    assert!(is_safe_archive_path("modules/x64/helper.dll"));

    for unsafe_path in [
        "",
        "../rustgram.exe",
        "a/../b.dll",
        "/etc/passwd",
        "dir\\file.exe",
        "C:evil.exe",
        "CON",
        "sub/nul.txt",
        "COM1.dll",
        "LPT9",
        "trailing.",
        "trailing ",
        "bad\x01name",
        "a//b",
    ] {
        assert!(
            !is_safe_archive_path(unsafe_path),
            "expected unsafe path '{unsafe_path}'"
        );
    }
}

#[test]
fn offline_check_stage_promote_and_apply_end_to_end() {
    let signing_key = dev_keypair();
    let verifying_key = signing_key.verifying_key();

    let tmp = TempDir::new().unwrap();
    let workdir = tmp.path().join("workdir");
    let install_dir = tmp.path().join("install");
    fs::create_dir_all(&workdir).unwrap();
    fs::create_dir_all(&install_dir).unwrap();

    // Seed existing v0.1.0 installation
    fs::write(install_dir.join(DEFAULT_EXE_NAME), b"old-rustgram-0.1.0").unwrap();
    fs::write(install_dir.join(UPDATER_EXE_NAME), b"old-updater-0.1.0").unwrap();

    let next_ver = Version::parse("0.2.0").unwrap();
    let package_bytes = pack_update(
        UpdateChannel::Stable,
        Target::WIN_X64,
        &next_ver,
        1_750_000_000,
        [
            (DEFAULT_EXE_NAME, b"new-rustgram-0.2.0".as_slice()),
            (UPDATER_EXE_NAME, b"new-updater-0.2.0".as_slice()),
            ("resources/extra.dat", b"extra-payload".as_slice()),
        ],
        &signing_key,
    )
    .unwrap();

    let download_url = "https://github.com/rustgram/rustgram/releases/download/v0.2.0/rustgram-0.2.0-win-x64.tupdate";
    let releases_json = format!(
        r#"[{{
            "tag_name": "v0.2.0",
            "name": "Rustgram 0.2.0",
            "draft": false,
            "prerelease": false,
            "assets": [{{
                "name": "rustgram-0.2.0-win-x64.tupdate",
                "browser_download_url": "{download_url}",
                "size": {}
            }}]
        }}]"#,
        package_bytes.len()
    );

    let mut mock = MockHttp::default();
    mock.responses.insert(
        "https://api.github.com/repos/rustgram/rustgram/releases?per_page=20".to_string(),
        releases_json.into_bytes(),
    );
    mock.responses
        .insert(download_url.to_string(), package_bytes);

    let checker = UpdateChecker::new(
        mock,
        UpdateConfig {
            github_repo: "rustgram/rustgram".into(),
            api_base_url: None,
            current_version: Version::parse("0.1.0").unwrap(),
            allow_beta: false,
            target: Target::WIN_X64,
            working_dir: workdir.clone(),
            install_dir: install_dir.clone(),
            verifying_key,
        },
    );

    let staged = checker.check_and_stage().unwrap().expect("expected update");
    assert_eq!(staged.version, next_ver);
    assert!(staged.updater_promoted);
    assert!(has_ready_update(&workdir));

    // Updater binary was promoted into install_dir and removed from tupdates/temp
    assert_eq!(
        fs::read(install_dir.join(UPDATER_EXE_NAME)).unwrap(),
        b"new-updater-0.2.0"
    );
    assert!(!staged.staging_dir.join(UPDATER_EXE_NAME).exists());

    // tdata/version file is readable and matches 0.2.0
    let (num, text) = read_version_file(&staged.staging_dir.join("tdata/version")).unwrap();
    assert_eq!(num, 2_000);
    assert_eq!(text, "0.2.0");

    // Simulate invoking rustgram-updater with -update -finishprocess -workdir -key -debug
    let launch_args = build_updater_launch_args(&UpdaterLaunchOptions {
        parent_pid: Some(4242),
        parent_created: Some(133_000_000_000),
        workdir: Some(workdir.clone()),
        key: Some("custom_key".into()),
        exe_name: None,
        autostart: false,
        debug: true,
        start_in_tray: true,
        just_update: false,
        app_version: None,
    });
    let parsed = parse_args(&launch_args);
    assert!(parsed.need_update);
    assert_eq!(parsed.finish_process, Some((4242, 133_000_000_000)));
    assert_eq!(parsed.workdir.as_deref(), Some(workdir.as_path()));
    assert_eq!(parsed.key.as_deref(), Some("custom_key"));
    assert!(parsed.debug);
    assert!(parsed.start_in_tray);

    let waiter = RecordingWaiter::default();
    let launcher = RecordingLauncher::default();
    let outcome = run_updater(&parsed, &workdir, &install_dir, &waiter, &launcher).unwrap();

    assert!(outcome.updated);
    assert!(outcome.relaunched);
    assert_eq!(outcome.installed_version.as_deref(), Some("0.2.0"));
    assert_eq!(outcome.files_replaced, 2); // rustgram.exe + resources/extra.dat

    // Files were replaced in install_dir and tupdates was cleaned up
    assert_eq!(
        fs::read(install_dir.join(DEFAULT_EXE_NAME)).unwrap(),
        b"new-rustgram-0.2.0"
    );
    assert_eq!(
        fs::read(install_dir.join("resources/extra.dat")).unwrap(),
        b"extra-payload"
    );
    assert!(!has_ready_update(&workdir));
    assert!(!workdir.join("tupdates").exists());

    // Process waiter waited on the parent PID
    assert_eq!(
        *waiter.waited.lock().unwrap(),
        vec![(4242, 133_000_000_000)]
    );

    // Relaunch received -noupdate plus forwarded flags
    let launches = launcher.launches.lock().unwrap();
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0].0, install_dir.join(DEFAULT_EXE_NAME));
    assert_eq!(
        launches[0].1,
        vec![
            "-noupdate",
            "-debug",
            "-startintray",
            "-workdir",
            &workdir.to_string_lossy(),
            "-key",
            "custom_key",
        ]
    );
}

#[test]
fn apply_blocks_self_overwrite_when_updater_not_promoted() {
    let signing_key = dev_keypair();
    let tmp = TempDir::new().unwrap();
    let workdir = tmp.path().join("workdir");
    let install_dir = tmp.path().join("install");
    fs::create_dir_all(&workdir).unwrap();
    fs::create_dir_all(&install_dir).unwrap();

    let pkg = pack_update(
        UpdateChannel::Stable,
        Target::WIN_X64,
        &Version::parse("0.2.0").unwrap(),
        1_750_000_000,
        [
            (DEFAULT_EXE_NAME, b"app".as_slice()),
            (UPDATER_EXE_NAME, b"updater".as_slice()),
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

    // Without calling promote_ready_updater, apply_staged_update must refuse & clean up
    let err = apply_staged_update(
        &workdir,
        &install_dir,
        DEFAULT_EXE_NAME,
        UPDATER_EXE_NAME,
        false,
    )
    .unwrap_err();
    assert!(matches!(err, UpdateError::SelfOverwriteBlocked(_)));
    assert!(!workdir.join("tupdates").exists());

    // Unpack again, promote updater, then apply succeeds
    unpack_verified_package(&verified, &workdir).unwrap();
    assert!(promote_ready_updater(&workdir, &install_dir).unwrap());
    let outcome = apply_staged_update(
        &workdir,
        &install_dir,
        DEFAULT_EXE_NAME,
        UPDATER_EXE_NAME,
        false,
    )
    .unwrap();
    assert!(outcome.updated);
    clear_updates(&workdir).unwrap();
}

#[test]
fn updater_args_sanitize_exename_and_handle_relaunch_flags() {
    let parsed = parse_args([
        "-update",
        "-exename",
        "..\\evil.exe",
        "-workdir",
        "D:\\profile",
        "-key",
        "work",
        "-autostart",
        "-justupdate",
    ]);
    assert_eq!(parsed.exe_name, DEFAULT_EXE_NAME);
    assert!(parsed.just_update);
    assert_eq!(
        build_relaunch_args(&parsed),
        vec![
            "-noupdate",
            "-autostart",
            "-workdir",
            "D:\\profile",
            "-key",
            "work"
        ]
    );

    let valid_custom = parse_args(["-exename", "RustgramCustom.exe"]);
    assert_eq!(valid_custom.exe_name, "RustgramCustom.exe");

    // Verify -justupdate suppresses relaunch
    let tmp = TempDir::new().unwrap();
    let launcher = RecordingLauncher::default();
    let waiter = RecordingWaiter::default();
    let args = UpdaterArgs {
        need_update: false,
        just_update: true,
        ..UpdaterArgs::default()
    };
    let outcome = run_updater(&args, tmp.path(), tmp.path(), &waiter, &launcher).unwrap();
    assert!(!outcome.relaunched);
    assert!(launcher.launches.lock().unwrap().is_empty());
}
