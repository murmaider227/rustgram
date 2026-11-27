//! Unit and integration tests for `tg-xtask`:
//! - Rendering `assets/icon.svg` to a valid multi-size `.ico` (verifying all icon sizes
//!   and that both background and foreground text pixels are drawn)
//! - Running `dist` with prebuilt dummy executables + FFmpeg DLLs in a temp directory, producing
//!   the ready portable `Rustgram/` folder, portable ZIP, signed `.tupdate` (verified by
//!   `tg-updater`), and `SHA256SUMS.txt` while gracefully skipping missing `ISCC.exe`.
//! - Validating `installer/tgfork.iss` (no Telegram branding, product name "Rustgram",
//!   optional `tg://` registration, FFmpeg DLLs & license files).
//! - Checking that every path handed to `ISCC.exe` is absolute when `--out-dir` is relative.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use tempfile::TempDir;
use zip::ZipArchive;

use tg_updater::package::verify_package;
use tg_updater::version::{Target, UpdateChannel, Version};
use tg_updater::{DEFAULT_EXE_NAME, UPDATER_EXE_NAME};
use tg_xtask::dist::{
    DIST_EXE_NAMES, DistOptions, InnoSetupDefines, PORTABLE_DIR_NAME, PRIVACY_NOTICE_NAME,
    REQUIRED_FFMPEG_DLL_PREFIXES, THIRD_PARTY_NOTICES_NAME, WIDGET_HELPER_EXE_NAME,
    find_workspace_root, generate_inno_setup_script, read_workspace_version, run_dist,
};
use tg_xtask::icon::{ICON_SIZES, render_svg_to_ico};
use tg_xtask::winrc::{COMPANY_NAME, DIST_BINARY_RESOURCES, PRODUCT_NAME, generate_version_rc};

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

    // Check 64x64 entry has violet circle pixels (gradient #8A6CFF -> #4B2FC2), the white
    // paper plane, and transparent corners outside the circle.
    let entry_64 = icon_dir
        .entries()
        .iter()
        .find(|e| e.width() == 64)
        .expect("64x64 entry");
    let img = entry_64.decode().unwrap();
    assert_eq!(img.width(), 64);
    assert_eq!(img.height(), 64);

    let (pixel_chunks, _) = img.rgba_data().as_chunks::<4>();
    let has_violet = pixel_chunks.iter().any(|p| {
        (60..=150).contains(&p[0]) && p[1] < 120 && p[2] > 180 && p[2] > p[0] && p[3] == 255
    });
    let has_white = pixel_chunks
        .iter()
        .any(|p| p[0] > 230 && p[1] > 230 && p[2] > 230 && p[3] == 255);
    let has_transparent_corner = pixel_chunks[0][3] == 0;
    assert!(has_violet, "expected violet icon fill (#8A6CFF -> #4B2FC2)");
    assert!(
        has_white,
        "expected white paper plane rendered from icon.svg"
    );
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
        "avcodec-63.dll",
        "avformat-63.dll",
        "avutil-61.dll",
        "swresample-7.dll",
        "swscale-10.dll",
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
        require_api_credentials: false,
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
    // TGB-2969: tdesktop's notification sound ships embedded and is credited.
    assert!(notices.contains("msg_incoming.mp3"));
    // TGB-2979: so do tdesktop's ten call and group-call sounds.
    for key in [
        "call_incoming",
        "call_outgoing",
        "call_connect",
        "call_busy",
        "call_end",
        "group_call_start",
        "group_call_connect",
        "group_call_allowed",
        "group_call_end",
        "group_call_recording_start",
    ] {
        assert!(notices.contains(&format!("{key}.mp3")), "{key} is credited");
    }
    assert!(notices.contains("The Telegram Desktop Authors"));
    let privacy = fs::read_to_string(output.portable_dir.join(PRIVACY_NOTICE_NAME)).unwrap();
    assert!(privacy.contains("# Privacy policy"));
    assert!(
        privacy.contains("Update automatically") || privacy.contains("Update\r\nautomatically")
    );

    // Inspect portable ZIP contents
    let zip_file = File::open(&output.portable_zip).unwrap();
    let mut archive = ZipArchive::new(zip_file).unwrap();
    let mut names = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        let name = entry.name().unwrap().into_owned();
        names.push(name.clone());
        if name == DEFAULT_EXE_NAME {
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
    assert!(names.contains(&PRIVACY_NOTICE_NAME.to_string()));
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
    assert!(norm_disk.contains(PRIVACY_NOTICE_NAME));
    assert!(norm_disk.contains("InfoBeforeFile={#DistDir}\\PRIVACY.txt"));

    // Must declare default-checked `autoupdate` task writing StartupPrefs.auto_update.
    let autoupdate_line = norm_disk
        .lines()
        .find(|l| l.contains("Name: \"autoupdate\""))
        .expect("installer/tgfork.iss missing autoupdate task");
    assert!(
        autoupdate_line.contains("Check for updates automatically"),
        "autoupdate task must carry user-facing description"
    );
    assert!(
        !autoupdate_line.contains("unchecked"),
        "autoupdate task must be checked by default"
    );
    assert!(norm_disk.contains("startup_prefs.json"));
    assert!(norm_disk.contains("\"auto_update\""));

    // Must preserve user tdata on uninstall, matching docs/PRIVACY.md.
    assert!(
        !norm_disk.contains("Name: \"{app}\\tdata\""),
        "uninstaller must keep tdata per docs/PRIVACY.md"
    );
    assert!(
        !norm_disk.contains("Name: \"{userappdata}\\{#MyAppName}\\tdata\""),
        "uninstaller must keep userappdata tdata per docs/PRIVACY.md"
    );

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
        require_api_credentials: false,
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
        .map(|i| archive.by_index(i).unwrap().name().unwrap().into_owned())
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
        .map(|i| inner_zip.by_index(i).unwrap().name().unwrap().into_owned())
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

#[test]
fn generated_windows_version_resource_scripts_cover_all_exes_and_release_version() {
    let root = find_workspace_root().unwrap();
    let workspace_version = read_workspace_version(&root).unwrap();
    let prerelease_version = Version::parse("0.1.0-alpha.1").unwrap();

    assert_eq!(DIST_BINARY_RESOURCES.len(), DIST_EXE_NAMES.len());
    for (spec, &expected_exe) in DIST_BINARY_RESOURCES.iter().zip(DIST_EXE_NAMES.iter()) {
        assert_eq!(spec.exe_name, expected_exe);
    }

    for version in [&prerelease_version, &workspace_version] {
        let ver_str = version.to_string();
        let expected_quad = format!("{},{},{},0", version.major, version.minor, version.patch);

        for spec in DIST_BINARY_RESOURCES {
            let icon = spec.has_icon.then_some("rustgram.ico");
            let rc = generate_version_rc(spec, version, icon);

            assert!(
                rc.contains("1 VERSIONINFO"),
                "{}: missing VERSIONINFO block",
                spec.exe_name
            );
            assert!(
                rc.contains(&format!("FILEVERSION {expected_quad}")),
                "{}: missing FILEVERSION {expected_quad}",
                spec.exe_name
            );
            assert!(
                rc.contains(&format!("PRODUCTVERSION {expected_quad}")),
                "{}: missing PRODUCTVERSION {expected_quad}",
                spec.exe_name
            );
            assert!(
                rc.contains(&format!("VALUE \"ProductName\", \"{PRODUCT_NAME}\"")),
                "{}: ProductName must be {PRODUCT_NAME}",
                spec.exe_name
            );
            assert!(
                rc.contains(&format!("VALUE \"CompanyName\", \"{COMPANY_NAME}\"")),
                "{}: CompanyName must be {COMPANY_NAME}",
                spec.exe_name
            );
            assert!(
                rc.contains(&format!(
                    "VALUE \"FileDescription\", \"{}\"",
                    spec.file_description
                )),
                "{}: missing FileDescription",
                spec.exe_name
            );
            assert!(
                rc.contains(&format!("VALUE \"FileVersion\", \"{ver_str}\"")),
                "{}: FileVersion must equal release version {ver_str}",
                spec.exe_name
            );
            assert!(
                rc.contains(&format!("VALUE \"ProductVersion\", \"{ver_str}\"")),
                "{}: ProductVersion must equal release version {ver_str}",
                spec.exe_name
            );
            assert!(
                rc.contains(&format!(
                    "VALUE \"OriginalFilename\", \"{}\"",
                    spec.exe_name
                )),
                "{}: missing OriginalFilename",
                spec.exe_name
            );
            assert!(
                rc.contains(&format!(
                    "VALUE \"InternalName\", \"{}\"",
                    spec.internal_name
                )),
                "{}: missing InternalName",
                spec.exe_name
            );
            assert!(
                rc.contains("VALUE \"Translation\", 0x0409, 1200"),
                "{}: missing VarFileInfo Translation",
                spec.exe_name
            );
            if spec.has_icon {
                assert!(
                    rc.contains("1 ICON \"rustgram.ico\""),
                    "{}: expected icon directive",
                    spec.exe_name
                );
            } else {
                assert!(
                    !rc.contains("1 ICON"),
                    "{}: helper exe should not require icon directive",
                    spec.exe_name
                );
            }
        }
    }

    // Verify each crate's build.rs embeds matching metadata and reads RUSTGRAM_VERSION.
    for spec in DIST_BINARY_RESOURCES {
        let build_rs = fs::read_to_string(root.join(spec.build_script_rel_path))
            .unwrap_or_else(|e| panic!("read {}: {e}", spec.build_script_rel_path));
        assert!(
            build_rs.contains(&format!("const PRODUCT_NAME: &str = \"{PRODUCT_NAME}\";")),
            "{} must set PRODUCT_NAME = \"{PRODUCT_NAME}\"",
            spec.build_script_rel_path
        );
        assert!(
            build_rs.contains(&format!("const COMPANY_NAME: &str = \"{COMPANY_NAME}\";")),
            "{} must set COMPANY_NAME = \"{COMPANY_NAME}\"",
            spec.build_script_rel_path
        );
        assert!(
            build_rs.contains(&format!(
                "const FILE_DESCRIPTION: &str = \"{}\";",
                spec.file_description
            )),
            "{} must set FILE_DESCRIPTION = \"{}\"",
            spec.build_script_rel_path,
            spec.file_description
        );
        assert!(
            build_rs.contains("RUSTGRAM_VERSION") && build_rs.contains("CARGO_PKG_VERSION"),
            "{} must read RUSTGRAM_VERSION with CARGO_PKG_VERSION fallback",
            spec.build_script_rel_path
        );
        assert!(
            build_rs.contains("(\"FileVersion\", version_str)")
                && build_rs.contains("(\"ProductVersion\", version_str)"),
            "{} must set both FileVersion and ProductVersion to version_str",
            spec.build_script_rel_path
        );
    }

    // Verify both Inno Setup scripts carry full installer VersionInfo metadata, privacy policy, and autoupdate task.
    for rel in ["installer/tgfork.iss", "packaging/windows/tgfork.iss"] {
        let iss = fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        assert!(
            iss.contains(&format!("#define MyAppPublisher \"{COMPANY_NAME}\"")),
            "{rel} must set MyAppPublisher to \"{COMPANY_NAME}\""
        );
        for required in [
            "VersionInfoVersion={#MyAppNumericVersion}",
            "VersionInfoTextVersion={#MyAppVersion}",
            "VersionInfoProductName={#MyAppName}",
            "VersionInfoProductVersion={#MyAppNumericVersion}",
            "VersionInfoProductTextVersion={#MyAppVersion}",
            "VersionInfoCompany={#MyAppPublisher}",
            "VersionInfoDescription={#MyAppName} Setup",
            "InfoBeforeFile=",
            "PRIVACY.txt",
            "Name: \"autoupdate\"",
            "Check for updates automatically",
            "startup_prefs.json",
        ] {
            assert!(iss.contains(required), "{rel} missing '{required}'");
        }
    }
}

#[test]
fn release_workflow_builds_prerelease_draft_with_pubkey_guard_and_all_artifacts() {
    let root = find_workspace_root().unwrap();
    let yml = fs::read_to_string(root.join(".github").join("workflows").join("release.yml"))
        .expect("read .github/workflows/release.yml");

    assert!(yml.contains("\"v*\""));
    assert!(yml.contains("vars.RUSTGRAM_UPDATE_PUBKEY"));
    assert!(yml.contains("draft: true"));
    assert!(yml.contains("prerelease: ${{ steps.meta.outputs.prerelease }}"));
    assert!(
        yml.contains("$channel = if ($isPrerelease -eq \"true\") { \"beta\" } else { \"stable\" }")
    );
    for artifact in [
        "Rustgram-Setup-",
        "-win-x64-portable.zip",
        "-win-x64.tupdate",
        "SHA256SUMS.txt",
    ] {
        assert!(
            yml.contains(artifact),
            "release.yml missing artifact reference '{artifact}'"
        );
    }
}

#[test]
fn inno_setup_paths_are_absolute_for_relative_out_dir() {
    // `tg-xtask dist --out-dir dist`: ISCC resolves relative /D paths against `installer/`, which
    // made the release workflow look for `installer\dist\Rustgram\PRIVACY.txt`.
    let out_dir = Path::new("dist");
    let dist_dir = out_dir.join(PORTABLE_DIR_NAME);
    let privacy_file = dist_dir.join(PRIVACY_NOTICE_NAME);
    let setup_icon = out_dir.join("rustgram.ico");
    let release_dir = Path::new("target").join("release");
    let defines = InnoSetupDefines {
        version: "0.1.0-alpha.1",
        numeric_version: "0.1.0.1",
        dist_dir: &dist_dir,
        release_dir: &release_dir,
        privacy_file: &privacy_file,
        setup_icon: &setup_icon,
        output_dir: out_dir,
        output_base_filename: "Rustgram-Setup-0.1.0-alpha.1-win-x64",
    };
    let args = defines.to_args().unwrap();
    let cwd = std::env::current_dir().unwrap();

    for (key, relative) in [
        ("DistDir", dist_dir.as_path()),
        ("ReleasePath", release_dir.as_path()),
        ("PrivacyFile", privacy_file.as_path()),
        ("SetupIcon", setup_icon.as_path()),
        ("OutputDir", out_dir),
    ] {
        let prefix = format!("/D{key}=");
        let value = args
            .iter()
            .find_map(|arg| arg.strip_prefix(prefix.as_str()))
            .unwrap_or_else(|| panic!("missing /D{key} in {args:?}"));
        let path = Path::new(value);
        assert!(path.is_absolute(), "/D{key} must be absolute, got {value}");
        assert_eq!(path, cwd.join(relative), "/D{key}");
    }
    assert!(args.contains(&"/DMyAppVersion=0.1.0-alpha.1".to_owned()));
    assert!(args.contains(&"/DMyAppNumericVersion=0.1.0.1".to_owned()));
    assert!(
        args.contains(&"/DMyOutputBaseFilename=Rustgram-Setup-0.1.0-alpha.1-win-x64".to_owned())
    );
}

#[test]
fn release_check_requires_the_api_credentials_compiled_in() {
    use tg_xtask::dist::check_embedded_api_credentials;

    // Synthetic placeholder values only.
    let hash = "0123456789abcdef0123456789abcdef";
    let with_hash = [b"MZ..prefix..".as_slice(), hash.as_bytes(), b"..suffix"].concat();
    let without_hash = b"MZ..a build without credentials..".to_vec();

    // No credentials for this build: nothing to check unless required.
    assert!(!check_embedded_api_credentials(&without_hash, None, None, false).unwrap());
    assert!(check_embedded_api_credentials(&without_hash, None, Some(" "), true).is_err());

    // Credentials set: the binary must contain them.
    assert!(check_embedded_api_credentials(&with_hash, Some("99001"), Some(hash), true).unwrap());
    let err = check_embedded_api_credentials(&without_hash, Some("99001"), Some(hash), false)
        .expect_err("missing embedded hash must fail");
    assert!(
        !err.to_string().contains(hash),
        "the error must not print the secret"
    );

    // Half-set secrets fail the release.
    assert!(check_embedded_api_credentials(&with_hash, Some("99001"), None, false).is_err());
}
