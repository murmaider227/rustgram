//! Build script for `tg-updater`.
//!
//! 1. Reads the optional build-time environment variable `RUSTGRAM_UPDATE_PUBKEY`
//!    (with legacy fallback `TGFORK_UPDATE_PUBKEY`, a 64-character hex-encoded Ed25519
//!    verifying key produced by `cargo run -p tg-xtask -- keygen`) and emits `release_pubkey.rs`
//!    into `OUT_DIR`. When unset or empty, an all-zero placeholder key is emitted so the updater
//!    fails closed at runtime. Invalid hex or wrong-length values fail the build immediately.
//! 2. On Windows targets, embeds a `VERSIONINFO` resource into `rustgram-updater.exe` using
//!    `RUSTGRAM_VERSION` (with `TGFORK_VERSION` / `CARGO_PKG_VERSION` fallback).

use std::env;
use std::fs;
use std::path::PathBuf;

const ENV_VAR: &str = "RUSTGRAM_UPDATE_PUBKEY";
const LEGACY_ENV_VAR: &str = "TGFORK_UPDATE_PUBKEY";

fn main() {
    println!("cargo:rerun-if-env-changed={ENV_VAR}");
    println!("cargo:rerun-if-env-changed={LEGACY_ENV_VAR}");
    println!("cargo:rerun-if-env-changed=RUSTGRAM_VERSION");
    println!("cargo:rerun-if-env-changed=TGFORK_VERSION");

    let raw = env::var(ENV_VAR)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| env::var(LEGACY_ENV_VAR).ok())
        .unwrap_or_default();
    let trimmed = raw.trim();

    let (bytes, configured) = if trimmed.is_empty() {
        ([0u8; 32], false)
    } else {
        let decoded = decode_hex_32(trimmed).unwrap_or_else(|err| {
            panic!(
                "invalid {ENV_VAR}: {err}. \
                 Generate a keypair with `cargo run -p tg-xtask -- keygen` \
                 and pass the 64-character hex public key in {ENV_VAR}."
            );
        });
        let all_zero = decoded.iter().all(|&b| b == 0);
        (decoded, !all_zero)
    };

    let mut array_elems = String::with_capacity(32 * 6);
    for (idx, b) in bytes.iter().enumerate() {
        if idx > 0 {
            array_elems.push_str(", ");
        }
        array_elems.push_str(&format!("0x{b:02X}"));
    }

    let generated = format!(
        "/// Compiled-in Ed25519 release verifying key (32 raw bytes).\n\
         ///\n\
         /// Configured at build time via the `{ENV_VAR}` environment variable (64 hex chars).\n\
         /// Generate a release keypair with:\n\
         /// ```text\n\
         /// cargo run -p tg-xtask -- keygen\n\
         /// ```\n\
         /// Pass the printed public key hex via `{ENV_VAR}` when building release binaries,\n\
         /// and store the private seed ONLY in CI secrets (`RUSTGRAM_UPDATER_SIGNING_KEY`).\n\
         /// Never commit a private signing key.\n\
         ///\n\
         /// When `{ENV_VAR}` is unset at build time, this defaults to the all-zero placeholder\n\
         /// and `tg-updater` fails closed (refusing to verify or apply any updates).\n\
         pub const RELEASE_SIGNING_PUBLIC_KEY: [u8; 32] = [{array_elems}];\n\
         \n\
         /// `true` when `{ENV_VAR}` was configured with a non-zero Ed25519 key at build time.\n\
         pub const RELEASE_SIGNING_PUBLIC_KEY_CONFIGURED: bool = {configured};\n"
    );

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set by cargo"));
    fs::write(out_dir.join("release_pubkey.rs"), generated)
        .expect("failed to write release_pubkey.rs");

    #[cfg(windows)]
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        win_resources::embed_version_resource(&out_dir);
    }
}

fn decode_hex_32(s: &str) -> Result<[u8; 32], String> {
    if s.len() != 64 {
        return Err(format!(
            "expected 64 hex characters (32 bytes), got {} characters",
            s.len()
        ));
    }
    let bytes = s.as_bytes();
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = hex_nibble(bytes[i * 2])?;
        let lo = hex_nibble(bytes[i * 2 + 1])?;
        *slot = (hi << 4) | lo;
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Result<u8, String> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        other => Err(format!("non-hex byte 0x{other:02X}")),
    }
}

#[cfg(windows)]
mod win_resources {
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const PRODUCT_NAME: &str = "Rustgram";
    const COMPANY_NAME: &str = "Rustgram Authors";
    const FILE_DESCRIPTION: &str = "Rustgram Updater";
    const ORIGINAL_FILENAME: &str = "rustgram-updater.exe";
    const INTERNAL_NAME: &str = "rustgram-updater";
    const LEGAL_COPYRIGHT: &str = "Copyright (c) 2026 Rustgram Authors (GPL-3.0)";

    const RT_VERSION: u16 = 16;
    const LANG_EN_US: u16 = 0x0409;

    pub fn embed_version_resource(out_dir: &Path) {
        let version_str = env::var("RUSTGRAM_VERSION")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| {
                env::var("TGFORK_VERSION")
                    .ok()
                    .filter(|v| !v.trim().is_empty())
            })
            .unwrap_or_else(|| env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".into()));
        let version_quad = parse_version_quad(&version_str);

        let res_path = out_dir.join("rustgram_updater_res.res");
        let obj_path = out_dir.join("rustgram_updater_res.obj");
        let res_bytes = build_version_only_res_file(&version_str, version_quad);
        fs::write(&res_path, &res_bytes).expect("failed to write rustgram_updater_res.res");

        let cvtres = find_cvtres()
            .expect("cvtres.exe (from MSVC) was not found to embed rustgram-updater resources");
        let machine = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
            Ok("x86") => "X86",
            Ok("aarch64") => "ARM64",
            _ => "X64",
        };
        let status = Command::new(&cvtres)
            .arg("/NOLOGO")
            .arg(format!("/MACHINE:{machine}"))
            .arg(format!("/OUT:{}", obj_path.display()))
            .arg(&res_path)
            .status()
            .expect("failed to execute cvtres.exe");
        assert!(
            status.success(),
            "cvtres.exe failed with status {status} while compiling {}",
            res_path.display()
        );

        println!(
            "cargo:rustc-link-arg-bin=rustgram-updater={}",
            obj_path.display()
        );
    }

    fn parse_version_quad(version: &str) -> (u16, u16, u16, u16) {
        let core = version
            .trim()
            .trim_start_matches(['v', 'V'])
            .split(['-', '+'])
            .next()
            .unwrap_or("0.1.0");
        let mut parts = core.split('.').map(|s| s.parse::<u16>().unwrap_or(0));
        let major = parts.next().unwrap_or(0);
        let minor = parts.next().unwrap_or(1);
        let patch = parts.next().unwrap_or(0);
        let build = parts.next().unwrap_or(0);
        (major, minor, patch, build)
    }

    fn find_cvtres() -> Option<PathBuf> {
        if let Some(p) = env::var_os("CVTRES") {
            let path = PathBuf::from(p);
            if path.is_file() {
                return Some(path);
            }
        }
        for var in [
            "CC_x86_64_pc_windows_msvc",
            "AR_x86_64_pc_windows_msvc",
            "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER",
        ] {
            if let Some(val) = env::var_os(var)
                && let Some(dir) = Path::new(&val).parent()
            {
                let candidate = dir.join("cvtres.exe");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        if let Some(path_var) = env::var_os("PATH") {
            for dir in env::split_paths(&path_var) {
                let candidate = dir.join("cvtres.exe");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        for base in [
            r"C:\Program Files\Microsoft Visual Studio",
            r"C:\Program Files (x86)\Microsoft Visual Studio",
        ] {
            if let Some(found) = find_cvtres_under_vs(Path::new(base)) {
                return Some(found);
            }
        }
        None
    }

    fn find_cvtres_under_vs(root: &Path) -> Option<PathBuf> {
        let years = fs::read_dir(root).ok()?;
        for year in years.flatten() {
            let Ok(editions) = fs::read_dir(year.path()) else {
                continue;
            };
            for edition in editions.flatten() {
                let msvc_dir = edition.path().join("VC").join("Tools").join("MSVC");
                let Ok(versions) = fs::read_dir(&msvc_dir) else {
                    continue;
                };
                for ver in versions.flatten() {
                    let candidate = ver
                        .path()
                        .join("bin")
                        .join("Hostx64")
                        .join("x64")
                        .join("cvtres.exe");
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                }
            }
        }
        None
    }

    fn build_version_only_res_file(version_str: &str, quad: (u16, u16, u16, u16)) -> Vec<u8> {
        let version_data = build_vs_version_info(version_str, quad);
        let mut res = Vec::with_capacity(version_data.len() + 128);
        // Standard 32-byte Win32 `.res` empty marker record.
        write_res_record(&mut res, 0, 0, 0, 0, &[]);
        // Emit RT_VERSION (ID 1).
        write_res_record(&mut res, RT_VERSION, 1, 0x0030, LANG_EN_US, &version_data);
        res
    }

    fn write_res_record(
        out: &mut Vec<u8>,
        res_type: u16,
        res_name: u16,
        memory_flags: u16,
        language_id: u16,
        data: &[u8],
    ) {
        let header_size: u32 = 32;
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&header_size.to_le_bytes());
        out.extend_from_slice(&0xFFFFu16.to_le_bytes());
        out.extend_from_slice(&res_type.to_le_bytes());
        out.extend_from_slice(&0xFFFFu16.to_le_bytes());
        out.extend_from_slice(&res_name.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&memory_flags.to_le_bytes());
        out.extend_from_slice(&language_id.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(data);
        pad_dword(out);
    }

    fn build_vs_version_info(version_str: &str, quad: (u16, u16, u16, u16)) -> Vec<u8> {
        let file_ver_ms = (u32::from(quad.0) << 16) | u32::from(quad.1);
        let file_ver_ls = (u32::from(quad.2) << 16) | u32::from(quad.3);
        let mut fixed = Vec::with_capacity(52);
        fixed.extend_from_slice(&0xFEEF_04BDu32.to_le_bytes());
        fixed.extend_from_slice(&0x0001_0000u32.to_le_bytes());
        fixed.extend_from_slice(&file_ver_ms.to_le_bytes());
        fixed.extend_from_slice(&file_ver_ls.to_le_bytes());
        fixed.extend_from_slice(&file_ver_ms.to_le_bytes());
        fixed.extend_from_slice(&file_ver_ls.to_le_bytes());
        fixed.extend_from_slice(&0x0000_003Fu32.to_le_bytes());
        fixed.extend_from_slice(&0u32.to_le_bytes());
        fixed.extend_from_slice(&0x0004_0004u32.to_le_bytes());
        fixed.extend_from_slice(&0x0000_0001u32.to_le_bytes());
        fixed.extend_from_slice(&0u32.to_le_bytes());
        fixed.extend_from_slice(&0u32.to_le_bytes());
        fixed.extend_from_slice(&0u32.to_le_bytes());

        let entries = [
            ("CompanyName", COMPANY_NAME),
            ("FileDescription", FILE_DESCRIPTION),
            ("FileVersion", version_str),
            ("InternalName", INTERNAL_NAME),
            ("LegalCopyright", LEGAL_COPYRIGHT),
            ("OriginalFilename", ORIGINAL_FILENAME),
            ("ProductName", PRODUCT_NAME),
            ("ProductVersion", version_str),
        ];
        let mut string_children = Vec::new();
        for (k, v) in entries {
            pad_dword(&mut string_children);
            string_children.extend_from_slice(&encode_version_string(k, v));
        }
        let string_table = encode_version_node("040904b0", 1, &[], &string_children);
        let string_file_info = encode_version_node("StringFileInfo", 1, &[], &string_table);

        let mut translation_val = Vec::with_capacity(4);
        translation_val.extend_from_slice(&LANG_EN_US.to_le_bytes());
        translation_val.extend_from_slice(&0x04B0u16.to_le_bytes());
        let var_node = encode_version_node("Translation", 0, &translation_val, &[]);
        let var_file_info = encode_version_node("VarFileInfo", 1, &[], &var_node);

        let mut root_children = Vec::new();
        root_children.extend_from_slice(&string_file_info);
        pad_dword(&mut root_children);
        root_children.extend_from_slice(&var_file_info);

        encode_version_node("VS_VERSION_INFO", 0, &fixed, &root_children)
    }

    fn encode_version_string(key: &str, value: &str) -> Vec<u8> {
        let val_w = to_utf16_nul(value);
        let val_len_words = (val_w.len() / 2) as u16;
        let mut buf = Vec::new();
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&val_len_words.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&to_utf16_nul(key));
        pad_dword(&mut buf);
        buf.extend_from_slice(&val_w);
        let total_len = buf.len() as u16;
        buf[0..2].copy_from_slice(&total_len.to_le_bytes());
        buf
    }

    fn encode_version_node(key: &str, w_type: u16, value: &[u8], children: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&0u16.to_le_bytes());
        let val_len = if w_type == 1 {
            (value.len() / 2) as u16
        } else {
            value.len() as u16
        };
        buf.extend_from_slice(&val_len.to_le_bytes());
        buf.extend_from_slice(&w_type.to_le_bytes());
        buf.extend_from_slice(&to_utf16_nul(key));
        pad_dword(&mut buf);
        if !value.is_empty() {
            buf.extend_from_slice(value);
            if !children.is_empty() {
                pad_dword(&mut buf);
            }
        }
        buf.extend_from_slice(children);
        let total_len = buf.len() as u16;
        buf[0..2].copy_from_slice(&total_len.to_le_bytes());
        buf
    }

    fn to_utf16_nul(s: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity((s.len() + 1) * 2);
        for u in s.encode_utf16().chain(std::iter::once(0)) {
            out.extend_from_slice(&u.to_le_bytes());
        }
        out
    }

    fn pad_dword(buf: &mut Vec<u8>) {
        while !buf.len().is_multiple_of(4) {
            buf.push(0);
        }
    }
}
