//! TGB-3180: the `.tupdate` must carry every runtime file of the portable folder.
//!
//! v0.2.0-alpha.4 packed only the three executables while its FFmpeg DLLs had new names, so a copy
//! that updated to it had the new `rustgram.exe` next to the old DLLs and did not start.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use tempfile::TempDir;
use zip::ZipArchive;

use tg_updater::install_files::OBSOLETE_INSTALL_FILES;
use tg_updater::package::{pack_update, verify_package};
use tg_updater::version::{Target, UpdateChannel, Version};
use tg_updater::{DEFAULT_EXE_NAME, UPDATER_EXE_NAME};
use tg_xtask::XtaskError;
use tg_xtask::dist::{
    DOC_FILE_NAMES, DistOptions, WIDGET_HELPER_EXE_NAME, check_no_obsolete_names,
    check_update_package_covers_portable, collect_required_ffmpeg_dlls, find_workspace_root,
    generate_inno_setup_script, run_dist, update_package_files,
};

const NEW_DLLS: [&str; 5] = [
    "avcodec-63.dll",
    "avformat-63.dll",
    "avutil-61.dll",
    "swresample-7.dll",
    "swscale-10.dll",
];

fn platform_name(windows: &str, unix: &str) -> String {
    if cfg!(windows) { windows } else { unix }.to_owned()
}

fn package_names(package: &[u8], key: &SigningKey) -> Vec<String> {
    let verified = verify_package(
        package,
        &key.verifying_key(),
        &Version::parse("0.1.0").expect("version"),
        true,
        Target::WIN_X64,
    )
    .expect("verify package");
    let mut zip = ZipArchive::new(Cursor::new(&verified.envelope.payload)).expect("zip");
    let mut names: Vec<String> = (0..zip.len())
        .map(|i| {
            zip.by_index(i)
                .expect("entry")
                .name()
                .expect("name")
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// Runs `dist` on dummy binaries and returns `(tempdir, portable dir, package bytes, key)`.
fn dist_with_dummies() -> (TempDir, PathBuf, Vec<u8>, SigningKey) {
    let tmp = TempDir::new().expect("temp");
    let release_dir = tmp.path().join("release");
    let ffmpeg_bin = tmp.path().join("ffmpeg_bin");
    fs::create_dir_all(&release_dir).expect("release dir");
    fs::create_dir_all(&ffmpeg_bin).expect("ffmpeg dir");
    for (win, unix) in [
        (DEFAULT_EXE_NAME, "rustgram"),
        (UPDATER_EXE_NAME, "rustgram-updater"),
        (WIDGET_HELPER_EXE_NAME, "rustgram-widget-helper"),
    ] {
        fs::write(release_dir.join(platform_name(win, unix)), win.as_bytes()).expect("exe");
    }
    for dll in NEW_DLLS {
        fs::write(ffmpeg_bin.join(dll), dll.as_bytes()).expect("dll");
    }
    let key = SigningKey::generate(&mut OsRng);
    let key_path = tmp.path().join("dev.key");
    fs::write(&key_path, hex::encode(key.to_bytes())).expect("key");
    let output = run_dist(&DistOptions {
        workspace_root: find_workspace_root().expect("workspace"),
        version: Version::parse("0.2.0-alpha.5").expect("version"),
        channel: UpdateChannel::Beta,
        out_dir: tmp.path().join("dist"),
        release_dir: Some(release_dir),
        ffmpeg_bin_dir: Some(ffmpeg_bin),
        skip_build: true,
        signing_key_path: Some(key_path),
        iscc_path: Some(tmp.path().join("missing-iscc.exe")),
        require_iscc: false,
        require_api_credentials: false,
    })
    .expect("dist");
    let package = fs::read(output.update_package.expect("package")).expect("read package");
    (tmp, output.portable_dir, package, key)
}

fn runtime_files(portable_dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(portable_dir)
        .expect("portable dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .filter(|n| {
            let lower = n.to_ascii_lowercase();
            lower.ends_with(".exe") || lower.ends_with(".dll")
        })
        .collect();
    names.sort();
    names
}

#[test]
fn update_package_holds_every_exe_and_dll_of_the_portable_folder() {
    let (_tmp, portable_dir, package, key) = dist_with_dummies();
    let packed = package_names(&package, &key);
    let runtime = runtime_files(&portable_dir);
    assert_eq!(runtime.len(), 8, "3 exes + 5 FFmpeg DLLs: {runtime:?}");
    for name in &runtime {
        assert!(
            packed.contains(name),
            "package lacks {name}; package={packed:?}"
        );
    }
    for doc in DOC_FILE_NAMES {
        assert!(portable_dir.join(doc).is_file() || *doc == "LEGAL");
        assert!(!packed.iter().any(|n| n == doc), "{doc} is a document");
    }
    // Everything that is not a document is packed, not only `.exe` / `.dll`.
    let mut not_docs: Vec<String> = fs::read_dir(&portable_dir)
        .expect("portable dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .filter(|n| !DOC_FILE_NAMES.contains(&n.as_str()))
        .collect();
    not_docs.sort();
    assert_eq!(packed, not_docs);
    check_update_package_covers_portable(&portable_dir, &package).expect("covers");
}

#[test]
fn a_package_with_only_the_executables_is_refused() {
    let (_tmp, portable_dir, _package, key) = dist_with_dummies();
    // The v0.2.0-alpha.4 layout: three executables, no DLLs.
    let alpha4_like = pack_update(
        UpdateChannel::Beta,
        Target::WIN_X64,
        &Version::parse("0.2.0-alpha.4").expect("version"),
        0,
        [
            (DEFAULT_EXE_NAME, b"a".as_slice()),
            (UPDATER_EXE_NAME, b"b".as_slice()),
            (WIDGET_HELPER_EXE_NAME, b"c".as_slice()),
        ],
        &key,
    )
    .expect("pack");
    let err =
        check_update_package_covers_portable(&portable_dir, &alpha4_like).expect_err("must refuse");
    let text = err.to_string();
    for dll in NEW_DLLS {
        assert!(text.contains(dll), "{text}");
    }
}

#[test]
fn update_files_are_the_staged_files_minus_documents() {
    let staged: Vec<(String, PathBuf)> = ["rustgram.exe", "avutil-61.dll", "LICENSE", "new.bin"]
        .iter()
        .map(|n| ((*n).to_owned(), PathBuf::from(n)))
        .collect();
    let names: Vec<String> = update_package_files(&staged)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(names, ["rustgram.exe", "avutil-61.dll", "new.bin"]);
}

#[test]
fn shipping_a_file_listed_as_obsolete_is_refused() {
    let staged = vec![("AVUTIL-60.dll".to_owned(), PathBuf::from("x"))];
    assert!(matches!(
        check_no_obsolete_names(&staged),
        Err(XtaskError::InconsistentRelease(_))
    ));
    let current: Vec<(String, PathBuf)> = NEW_DLLS
        .iter()
        .map(|n| ((*n).to_owned(), PathBuf::from(n)))
        .collect();
    assert!(check_no_obsolete_names(&current).is_ok());
}

#[test]
fn two_ffmpeg_versions_in_one_bin_dir_are_refused() {
    let tmp = TempDir::new().expect("temp");
    for dll in NEW_DLLS.iter().chain(OBSOLETE_INSTALL_FILES) {
        fs::write(tmp.path().join(dll), b"dll").expect("dll");
    }
    let err = collect_required_ffmpeg_dlls(tmp.path()).expect_err("ambiguous");
    assert!(err.to_string().contains("avcodec-62.dll"), "{err}");
    for dll in OBSOLETE_INSTALL_FILES {
        fs::remove_file(tmp.path().join(dll)).expect("remove");
    }
    let picked: Vec<String> = collect_required_ffmpeg_dlls(tmp.path())
        .expect("one version")
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(picked, NEW_DLLS);
}

#[test]
fn installer_deletes_exactly_the_obsolete_files() {
    let script = generate_inno_setup_script();
    let section = script
        .split("[InstallDelete]")
        .nth(1)
        .and_then(|rest| rest.split("\n[").next())
        .expect("[InstallDelete] section");
    let deleted: Vec<&str> = section
        .lines()
        .filter(|l| l.starts_with("Type: files; Name: \"{app}\\"))
        .collect();
    assert_eq!(deleted.len(), OBSOLETE_INSTALL_FILES.len(), "{section}");
    for name in OBSOLETE_INSTALL_FILES {
        assert!(
            deleted.contains(&format!("Type: files; Name: \"{{app}}\\{name}\"").as_str()),
            "{name}"
        );
    }
}

#[test]
fn release_workflow_tests_the_update_path_before_the_draft() {
    let root = find_workspace_root().expect("workspace");
    let yml = fs::read_to_string(root.join(".github/workflows/release.yml")).expect("release.yml");
    let check = yml
        .find("update-path-check @checkArgs")
        .expect("release.yml runs tg-xtask update-path-check");
    let draft = yml
        .find("- name: Create Draft GitHub Release")
        .expect("draft step");
    let attest = yml
        .find("- name: Attest build provenance")
        .expect("attest step");
    assert!(
        check < attest && check < draft,
        "the check runs before the release is staged"
    );
    assert!(yml.contains("\"--from-github\", $env:REPO"));
    // Only test builds may skip it, and only when no release is older than them.
    assert!(
        yml.contains("if ($env:PUBLISH -ne \"true\") { $checkArgs += \"--skip-if-no-previous\" }")
    );
    // The packaged smoke test must not find FFmpeg DLLs on the runner's PATH.
    assert!(yml.contains(
        r#"$env:PATH = "$env:SystemRoot\System32;$env:SystemRoot;$env:SystemRoot\System32\Wbem""#
    ));
}
