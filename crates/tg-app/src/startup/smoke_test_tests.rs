//! Tests for `--smoke-test` over a fresh working dir and the frozen v0.1.0-alpha.2 profile.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tg_storage::cache::media_cache::MediaCacheIndex;

use super::*;

/// The frozen profile written by the v0.1.0-alpha.2 storage code. NEVER regenerate it: a new
/// profile written by today's code would prove nothing about reading old profiles. When the
/// storage format changes, keep this fixture and teach the code to read it; to cover a newer
/// format, add a second fixture next to it.
fn fixture_profile() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/smoke_profile_v0_1_0_alpha_2/profile")
}

/// SHA-256 of every fixture file, as committed. A failure here means the fixture was modified or
/// regenerated: restore it from git instead of updating these values.
const FIXTURE_SHA256: &[(&str, &str)] = &[
    (
        "shell_state.json",
        "f33d3c7a928b2fcf7ccb72c41a2f6b31a07d4e56484e22c551bc5b0510b2135a",
    ),
    (
        "tdata/D877F783D5D3EF8C/7BADE298B3BCEAAEs",
        "55ec90e8d7e6d00f2e4ba2d6a4c53bae44711c60ef572d87853c6847dc27bf9b",
    ),
    (
        "tdata/D877F783D5D3EF8C/maps",
        "073fe9b6d0ffc9b9f68b7cac2f4a66a4f00d2a81c179238b8f8f1748a294bbb1",
    ),
    (
        "tdata/D877F783D5D3EF8Cs",
        "9f65fb3cf6155ce431250903cd66713f26c8c19ef40f9784e79f1cb67e6556ba",
    ),
    (
        "tdata/key_datas",
        "f8021ab12516341d1279501f91403f31d36b3e399528ddc57a0c544525b9bc22",
    ),
    (
        "tdata/media_cache/D877F783D5D3EF8C-a1b2c3d4e5f60718/blobs/26/264bfc8bd694b98d2013b92af1f6686d",
        "54342671d2489857217c9408864be7a1e03b448da61fb729becceb06ae2aeb28",
    ),
    (
        "tdata/media_cache/D877F783D5D3EF8C-a1b2c3d4e5f60718/blobs/61/618795df9a439aa64b1df9c9080d2b0f",
        "680a49eb0a79c1d9b1587a64e4eff41244a47e4dfd3d8f24cd3b88df14ddd75f",
    ),
    (
        "tdata/media_cache/D877F783D5D3EF8C-a1b2c3d4e5f60718/index.sqlite3",
        "d14972b8d8cd860ef196c5f8f87bc265d92ef9c511f9fa0883a10ab7b43b39e7",
    ),
    (
        "tdata/settingss",
        "fa607e7fc1ff32f47417c1ab2de665f4b43b12f94fd1105a9e8473311be225b3",
    ),
    (
        "tdata/startup_prefs.json",
        "a34c9130a48f29dffa7f31379320ca935bbc84c633a8460dd6941d56ccb3474c",
    ),
];

fn list_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for entry in fs::read_dir(dir).expect("read fixture dir") {
        let path = entry.expect("fixture entry").path();
        if path.is_dir() {
            list_files(root, &path, out);
        } else {
            let rel = path.strip_prefix(root).expect("relative path");
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
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

/// A scratch working dir holding a copy of the fixture.
fn fixture_copy() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let workdir = dir.path().join("wd");
    copy_dir(&fixture_profile(), &workdir);
    (dir, workdir)
}

fn run(workdir: &Path) -> SmokeOutcome {
    run_smoke_test(workdir, "data", &workdir.join("shell_state.json"))
}

#[test]
fn fixture_is_frozen() {
    let root = fixture_profile();
    let mut files = Vec::new();
    list_files(&root, &root, &mut files);
    files.sort();
    let expected: Vec<&str> = FIXTURE_SHA256.iter().map(|(name, _)| *name).collect();
    assert_eq!(files, expected, "fixture file list changed");
    for (name, sha) in FIXTURE_SHA256 {
        let bytes = fs::read(root.join(name)).expect("read fixture file");
        let actual: String = tg_crypto::hash::sha256_bytes(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(
            &actual, sha,
            "{name} changed: the alpha.2 fixture must never be regenerated"
        );
    }
}

#[test]
fn first_start_on_an_empty_workdir_passes() {
    let dir = TempDir::new().expect("tempdir");
    let workdir = dir.path().join("fresh");
    let outcome = run(&workdir);
    assert_eq!(outcome.failures, Vec::<String>::new());
    assert_eq!(outcome.exit_code(), 0);
    // `key_datas` is written on first start; `settingss` only once a setting is saved.
    assert!(workdir.join("tdata").join("key_datas").is_file());
    // A fresh domain has one empty slot and no media cache.
    assert_eq!(outcome.second.accounts.len(), 1);
    assert_eq!(outcome.second.accounts[0].user_id, 0);
    assert_eq!(outcome.second.accounts[0].media_cache_entries, None);
    assert!(outcome.report("Rustgram test").contains("result: PASS"));
}

#[test]
fn alpha2_profile_opens_with_its_account_settings_and_media_cache() {
    let (_dir, workdir) = fixture_copy();
    let outcome = run(&workdir);
    assert_eq!(outcome.failures, Vec::<String>::new());
    assert_eq!(outcome.exit_code(), 0);
    assert!(!outcome.second.passcode_locked);
    assert_eq!(
        outcome.second.accounts,
        vec![AccountCheck {
            index: 0,
            user_id: 1_000_001,
            main_dc_id: 2,
            auth_keys: 2,
            media_cache_entries: Some(2),
        }]
    );
    let report = outcome.report("Rustgram test");
    assert!(
        report.contains("slot 0: signed in true, main DC 2, 2 auth keys, media cache 2 entries")
    );
    assert!(report.contains("result: PASS"), "{report}");

    // The values the alpha.2 build saved are the ones read back.
    let tdata = workdir.join("tdata");
    let local = LocalStorage::open_or_init(&tdata).expect("settingss");
    assert_eq!(local.settings().scale_percent, 125);
    let core = CoreSettings::load_from_local_storage(&local).expect("core settings");
    assert!((core.song_volume - 0.5).abs() < f64::EPSILON);
    assert!(!core.adaptive_for_wide);
    let prefs = StartupPrefs::load_from(&workdir);
    assert!(prefs.install_beta);
    assert_eq!(prefs.last_update_check_unix, 1_764_170_000);
    let shell: ShellSettings = serde_json::from_str(
        &fs::read_to_string(workdir.join("shell_state.json")).expect("shell_state.json"),
    )
    .expect("parse shell_state.json");
    assert_eq!(shell.embedded_theme.as_deref(), Some("Night"));

    // The media cache decrypts with the key the app derives from tdata.
    let cache_dir = tdata
        .join("media_cache")
        .join("D877F783D5D3EF8C-a1b2c3d4e5f60718");
    let key = tg_session::account::resolve_account_cache_key_for_tdata(&tdata, "data", 0)
        .expect("cache key");
    let cache = MediaCacheIndex::open_with_key(&cache_dir, key).expect("open cache");
    assert_eq!(
        cache.get_blob("photo:1001").expect("photo").as_deref(),
        Some(&b"smoke fixture photo"[..])
    );
    assert_eq!(
        cache.get_blob("sticker:2002").expect("sticker"),
        Some(vec![7u8; 4096])
    );
}

#[test]
fn unreadable_shell_settings_fail_instead_of_resetting() {
    let (_dir, workdir) = fixture_copy();
    fs::write(
        workdir.join("shell_state.json"),
        r#"{"window_position": 5}"#,
    )
    .expect("write");
    let outcome = run(&workdir);
    assert_eq!(outcome.exit_code(), EXIT_FAILED);
    assert!(
        outcome
            .failures
            .iter()
            .any(|f| f.contains("shell_state.json no longer parses")),
        "{:?}",
        outcome.failures
    );
}

#[test]
fn unreadable_startup_prefs_fail() {
    let (_dir, workdir) = fixture_copy();
    fs::write(StartupPrefs::path_in(&workdir), "not json").expect("write");
    let outcome = run(&workdir);
    assert_eq!(outcome.exit_code(), EXIT_FAILED);
    assert!(
        outcome
            .failures
            .iter()
            .any(|f| f.contains("startup_prefs.json")),
        "{:?}",
        outcome.failures
    );
}

#[test]
fn damaged_key_datas_fails() {
    let (_dir, workdir) = fixture_copy();
    let path = workdir.join("tdata").join("key_datas");
    let bytes = fs::read(&path).expect("read key_datas");
    fs::write(&path, &bytes[..bytes.len() / 2]).expect("truncate key_datas");
    let outcome = run(&workdir);
    assert_eq!(outcome.exit_code(), EXIT_FAILED, "{outcome:?}");
}

#[test]
fn damaged_media_cache_fails() {
    let (_dir, workdir) = fixture_copy();
    let index = workdir
        .join("tdata/media_cache/D877F783D5D3EF8C-a1b2c3d4e5f60718")
        .join("index.sqlite3");
    fs::write(&index, vec![0x42u8; 8192]).expect("overwrite index");
    let outcome = run(&workdir);
    assert_eq!(outcome.exit_code(), EXIT_FAILED, "{outcome:?}");
    assert!(
        outcome.failures.iter().any(|f| f.contains("media cache")),
        "{:?}",
        outcome.failures
    );
}

#[test]
fn run_and_report_writes_the_report_into_the_workdir() {
    let (_dir, workdir) = fixture_copy();
    let code = run_and_report(&workdir, "data", &workdir.join("shell_state.json"));
    assert_eq!(code, 0);
    let report = fs::read_to_string(workdir.join(REPORT_FILE_NAME)).expect("report");
    assert!(report.contains("storage smoke test"), "{report}");
    assert!(report.ends_with("result: PASS\n"), "{report}");
}
