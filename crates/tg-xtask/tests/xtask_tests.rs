//! Unit and integration tests for `tg-xtask`:
//! - Rendering `assets/icon.svg` to a valid multi-size `.ico` (verifying all icon sizes
//!   and that both background and foreground text pixels are drawn)
//! - Running `dist` with prebuilt dummy executables + FFmpeg DLLs in a temp directory, producing
//!   the ready portable `Rustgram/` folder, portable ZIP, signed `.tupdate` (verified by
//!   `tg-updater`), and `SHA256SUMS.txt` while gracefully skipping missing `ISCC.exe`.
//! - Validating `installer/tgfork.iss` (no Telegram branding, product name "Rustgram",
//!   optional `tg://` registration, FFmpeg DLLs & license files).

use std::fs::{self, File};
use std::io::Read;

use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use tempfile::TempDir;
use zip::ZipArchive;

use tg_updater::package::verify_package;
use tg_updater::version::{Target, UpdateChannel, Version};
use tg_updater::{DEFAULT_EXE_NAME, UPDATER_EXE_NAME};
use tg_xtask::dist::{
    DIST_EXE_NAMES, DistOptions, PORTABLE_DIR_NAME, REQUIRED_FFMPEG_DLL_PREFIXES,
    THIRD_PARTY_NOTICES_NAME, WIDGET_HELPER_EXE_NAME, find_workspace_root,
    generate_inno_setup_script, run_dist,
};
use tg_xtask::icon::{ICON_SIZES, render_svg_to_ico};

#[test]
fn renders_repo_icon_svg_to_multisize_ico() {
    let root = find_workspace_root().unwrap();
    let svg_path = root.join("assets").join("icon.svg");
    let tmp = TempDir::new().unwrap();
    let ico_path = tmp.path().join("rustgram.ico");

    render_svg_to_ico(&svg_path, &ico_path).unwrap();

    let file = File::open(&ico_path).unwrap();
    let icon_dir = ico::IconDir::read(file).unwrap();
    let rendered_sizes: Vec<u32> = icon_dir.entries().iter().map(|e| e.width()).collect();
    assert_eq!(rendered_sizes, ICON_SIZES);

    // Check 64x64 entry has both teal background pixels (#0891b2) and non-teal pixels
    // (transparent rounded corners and white "T" glyph where a sans-serif font is available).
    let entry_64 = icon_dir
        .entries()
        .iter()
        .find(|e| e.width() == 64)
        .expect("64x64 entry");
    let img = entry_64.decode().unwrap();
    assert_eq!(img.width(), 64);
    assert_eq!(img.height(), 64);

    let (pixel_chunks, _) = img.rgba_data().as_chunks::<4>();
    let has_teal = pixel_chunks
        .iter()
        .any(|p| p[0] < 30 && p[1] > 120 && p[2] > 150 && p[3] == 255);
    let has_white = pixel_chunks
        .iter()
        .any(|p| p[0] > 230 && p[1] > 230 && p[2] > 230 && p[3] == 255);
    let has_transparent_corner = pixel_chunks[0][3] == 0;
    assert!(has_teal, "expected teal icon fill (#0891b2)");
    assert!(has_white, "expected white 'T' glyph rendered from icon.svg");
    assert!(
        has_transparent_corner,
        "expected transparent rounded corner"
    );
}

#[test]
fn dist_stages_portable_folder_with_ffmpeg_and_licenses_zip_update_and_checksums() {
    let root = find_workspace_root().unwrap();
    let tmp = TempDir::new().unwrap();
    let release_dir = tmp.path().join("release");
    let ffmpeg_bin = tmp.path().join("ffmpeg_bin");
    let out_dir = tmp.path().join("dist");
    fs::create_dir_all(&release_dir).unwrap();
    fs::create_dir_all(&ffmpeg_bin).unwrap();

    let app_name = if cfg!(windows) {
        DEFAULT_EXE_NAME
    } else {
        "rustgram"
    };
    let updater_name = if cfg!(windows) {
        UPDATER_EXE_NAME
    } else {
        "rustgram-updater"
    };
    let widget_helper_name = if cfg!(windows) {
        WIDGET_HELPER_EXE_NAME
    } else {
        "rustgram-widget-helper"
    };
    fs::write(release_dir.join(app_name), b"dummy-rustgram-exe").unwrap();
    fs::write(release_dir.join(updater_name), b"dummy-updater-exe").unwrap();
    fs::write(
        release_dir.join(widget_helper_name),
        b"dummy-widget-helper-exe",
    )
    .unwrap();

    // Seed dummy required FFmpeg DLLs matching `REQUIRED_FFMPEG_DLL_PREFIXES`.
    let mock_dlls = [
        "avcodec-62.dll",
        "avformat-62.dll",
        "avutil-60.dll",
        "swresample-6.dll",
        "swscale-9.dll",
    ];
    for dll in mock_dlls {
        fs::write(ffmpeg_bin.join(dll), format!("mock-{dll}")).unwrap();
    }
    // Also place an unrelated FFmpeg exe that must NOT be copied into portable dist.
    fs::write(ffmpeg_bin.join("ffmpeg.exe"), b"cli-tool").unwrap();

    // Ephemeral dev signing key in temp dir
    let signing_key = SigningKey::generate(&mut OsRng);
    let key_path = tmp.path().join("dev.key");
    fs::write(&key_path, hex::encode(signing_key.to_bytes())).unwrap();

    let version = Version::parse("0.2.0").unwrap();
    let output = run_dist(&DistOptions {
        workspace_root: root,
        version: version.clone(),
        channel: UpdateChannel::Stable,
        out_dir: out_dir.clone(),
        release_dir: Some(release_dir),
        ffmpeg_bin_dir: Some(ffmpeg_bin),
        skip_build: true,
        signing_key_path: Some(key_path),
        // Point to non-existent ISCC path to verify graceful skip
        iscc_path: Some(tmp.path().join("nonexistent-iscc.exe")),
        require_iscc: false,
    })
    .unwrap();

    assert!(output.icon_path.is_file());
    assert_eq!(output.portable_dir, out_dir.join(PORTABLE_DIR_NAME));
    assert!(output.portable_dir.is_dir());
    assert!(output.portable_zip.is_file());
    assert!(output.installer_exe.is_none());
    assert!(output.checksums_path.is_file());

    // Inspect staged portable folder `<out_dir>/Rustgram/`
    assert_eq!(
        fs::read(output.portable_dir.join(DEFAULT_EXE_NAME)).unwrap(),
        b"dummy-rustgram-exe"
    );
    assert_eq!(
        fs::read(output.portable_dir.join(UPDATER_EXE_NAME)).unwrap(),
        b"dummy-updater-exe"
    );
    assert_eq!(
        fs::read(output.portable_dir.join(WIDGET_HELPER_EXE_NAME)).unwrap(),
        b"dummy-widget-helper-exe"
    );
    for dll in mock_dlls {
        assert!(
            output.portable_dir.join(dll).is_file(),
            "missing staged FFmpeg DLL {dll}"
        );
    }
    assert!(!output.portable_dir.join("ffmpeg.exe").exists());
    assert!(output.portable_dir.join("LICENSE").is_file());
    assert!(output.portable_dir.join("LEGAL").is_file());
    let notices = fs::read_to_string(output.portable_dir.join(THIRD_PARTY_NOTICES_NAME)).unwrap();
    assert!(notices.contains("GPL-3.0"));
    assert!(notices.contains("FFmpeg"));

    // Inspect portable ZIP contents
    let zip_file = File::open(&output.portable_zip).unwrap();
    let mut archive = ZipArchive::new(zip_file).unwrap();
    let mut names = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        names.push(entry.name().to_string());
        if entry.name() == DEFAULT_EXE_NAME {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).unwrap();
            assert_eq!(buf, b"dummy-rustgram-exe");
        }
    }
    assert!(names.contains(&DEFAULT_EXE_NAME.to_string()));
    assert!(names.contains(&UPDATER_EXE_NAME.to_string()));
    assert!(names.contains(&WIDGET_HELPER_EXE_NAME.to_string()));
    assert!(names.contains(&"LICENSE".to_string()));
    assert!(names.contains(&"LEGAL".to_string()));
    assert!(names.contains(&THIRD_PARTY_NOTICES_NAME.to_string()));
    for dll in mock_dlls {
        assert!(
            names.contains(&dll.to_string()),
            "missing FFmpeg DLL {dll} in zip"
        );
    }

    // Verify the generated .tupdate package using tg-updater
    let update_path = output.update_package.expect("signed .tupdate produced");
    let update_bytes = fs::read(&update_path).unwrap();
    let verified = verify_package(
        &update_bytes,
        &signing_key.verifying_key(),
        &Version::parse("0.1.0").unwrap(),
        false,
        Target::WIN_X64,
    )
    .unwrap();
    assert_eq!(verified.envelope.version, version);

    // Check SHA256SUMS.txt lists both artifacts
    let sums = fs::read_to_string(&output.checksums_path).unwrap();
    assert!(sums.contains("rustgram-0.2.0-win-x64-portable.zip"));
    assert!(sums.contains("rustgram-0.2.0-win-x64.tupdate"));
}

#[test]
fn inno_setup_script_is_valid_unbranded_and_has_optional_tg_protocol() {
    let root = find_workspace_root().unwrap();
    let iss_path = root.join("installer").join("tgfork.iss");
    let on_disk = fs::read_to_string(&iss_path).expect("installer/tgfork.iss must exist");
    let generated = generate_inno_setup_script();

    // Normalize line endings for comparison across Windows/Unix checkouts.
    let norm_disk = on_disk.replace("\r\n", "\n");
    let norm_gen = generated.replace("\r\n", "\n");
    assert_eq!(
        norm_disk, norm_gen,
        "installer/tgfork.iss should match generate_inno_setup_script()"
    );

    // Must contain ZERO Telegram branding.
    assert!(
        !norm_disk.to_ascii_lowercase().contains("telegram"),
        "installer/tgfork.iss must not contain Telegram branding"
    );

    // Must define product name "Rustgram" and required Inno Setup sections.
    assert!(norm_disk.contains("#define MyAppName \"Rustgram\""));
    for section in [
        "[Setup]",
        "[Languages]",
        "[Tasks]",
        "[Files]",
        "[Icons]",
        "[Registry]",
        "[Run]",
        "[UninstallDelete]",
        "[Code]",
    ] {
        assert!(
            norm_disk.contains(section),
            "missing Inno section {section}"
        );
    }

    // Must include all required FFmpeg DLL wildcards and license files.
    for prefix in REQUIRED_FFMPEG_DLL_PREFIXES {
        let pattern = format!("{prefix}*.dll");
        assert!(
            norm_disk.contains(&pattern),
            "installer/tgfork.iss missing FFmpeg entry '{pattern}'"
        );
    }
    assert!(norm_disk.contains("LICENSE"));
    assert!(norm_disk.contains("LEGAL"));
    assert!(norm_disk.contains(THIRD_PARTY_NOTICES_NAME));

    // Must declare optional `registertg` task and gate EVERY [Registry] line with `Tasks: registertg`.
    assert!(norm_disk.contains("Name: \"registertg\""));
    let mut in_registry = false;
    let mut registry_entry_count = 0usize;
    for line in norm_disk.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_registry = trimmed == "[Registry]";
            continue;
        }
        if in_registry && !trimmed.is_empty() && !trimmed.starts_with(';') {
            registry_entry_count += 1;
            assert!(
                trimmed.contains("Tasks: registertg"),
                "unconditional registry line in [Registry]: {trimmed}"
            );
        }
    }
    assert!(
        registry_entry_count >= 10,
        "expected tg:// registry entries in [Registry]"
    );
}

#[test]
fn dist_file_list_and_installers_include_all_three_executables() {
    assert_eq!(
        DIST_EXE_NAMES,
        &[
            "rustgram.exe",
            "rustgram-updater.exe",
            "rustgram-widget-helper.exe",
        ],
        "DIST_EXE_NAMES must contain rustgram.exe, rustgram-updater.exe, and rustgram-widget-helper.exe"
    );

    let root = find_workspace_root().unwrap();
    let tmp = TempDir::new().unwrap();
    let release_dir = tmp.path().join("release");
    let ffmpeg_bin = tmp.path().join("ffmpeg_bin");
    let out_dir = tmp.path().join("dist");
    fs::create_dir_all(&release_dir).unwrap();
    fs::create_dir_all(&ffmpeg_bin).unwrap();

    for (win_name, unix_name, body) in [
        (DEFAULT_EXE_NAME, "rustgram", b"exe-main".as_slice()),
        (
            UPDATER_EXE_NAME,
            "rustgram-updater",
            b"exe-updater".as_slice(),
        ),
        (
            WIDGET_HELPER_EXE_NAME,
            "rustgram-widget-helper",
            b"exe-widget-helper".as_slice(),
        ),
    ] {
        let name = if cfg!(windows) { win_name } else { unix_name };
        fs::write(release_dir.join(name), body).unwrap();
    }
    for prefix in REQUIRED_FFMPEG_DLL_PREFIXES {
        fs::write(ffmpeg_bin.join(format!("{prefix}1.dll")), b"dll").unwrap();
    }

    let signing_key = SigningKey::generate(&mut OsRng);
    let key_path = tmp.path().join("dev.key");
    fs::write(&key_path, hex::encode(signing_key.to_bytes())).unwrap();

    let output = run_dist(&DistOptions {
        workspace_root: root.clone(),
        version: Version::parse("0.3.0").unwrap(),
        channel: UpdateChannel::Stable,
        out_dir,
        release_dir: Some(release_dir),
        ffmpeg_bin_dir: Some(ffmpeg_bin),
        skip_build: true,
        signing_key_path: Some(key_path),
        iscc_path: Some(tmp.path().join("missing-iscc.exe")),
        require_iscc: false,
    })
    .unwrap();

    // 1. Staged portable directory contains all three executables side by side.
    for exe in DIST_EXE_NAMES {
        assert!(
            output.portable_dir.join(exe).is_file(),
            "portable dist folder missing {exe}"
        );
    }

    // 2. Portable release zip contains all three executables.
    let zip_file = File::open(&output.portable_zip).unwrap();
    let mut archive = ZipArchive::new(zip_file).unwrap();
    let zip_names: Vec<String> = (0..archive.len())
        .map(|i| archive.by_index(i).unwrap().name().to_owned())
        .collect();
    for exe in DIST_EXE_NAMES {
        assert!(
            zip_names.iter().any(|n| n == exe),
            "release zip missing {exe}; entries={zip_names:?}"
        );
    }

    // 3. Signed .tupdate inner zip contains all three executables.
    let update_path = output.update_package.expect("signed .tupdate");
    let update_bytes = fs::read(&update_path).unwrap();
    let verified = verify_package(
        &update_bytes,
        &signing_key.verifying_key(),
        &Version::parse("0.1.0").unwrap(),
        false,
        Target::WIN_X64,
    )
    .unwrap();
    let mut inner_zip = ZipArchive::new(std::io::Cursor::new(&verified.envelope.payload)).unwrap();
    let update_names: Vec<String> = (0..inner_zip.len())
        .map(|i| inner_zip.by_index(i).unwrap().name().to_owned())
        .collect();
    for exe in DIST_EXE_NAMES {
        assert!(
            update_names.iter().any(|n| n == exe),
            "signed .tupdate missing {exe}; entries={update_names:?}"
        );
    }

    // 4. Both Inno Setup scripts (packaging/windows/tgfork.iss and installer/tgfork.iss) ship all three.
    for rel in ["packaging/windows/tgfork.iss", "installer/tgfork.iss"] {
        let iss = fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        for exe in DIST_EXE_NAMES {
            assert!(iss.contains(exe), "{rel} must reference {exe}");
        }
        assert!(
            iss.contains("{#MyWidgetHelperExeName}"),
            "{rel} [Files] must install {{#MyWidgetHelperExeName}}"
        );
    }
}
