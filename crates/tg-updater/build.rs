//! Build script for `tg-updater`.
//!
//! Reads the optional build-time environment variable `RUSTGRAM_UPDATE_PUBKEY`
//! (with legacy fallback `TGFORK_UPDATE_PUBKEY`, a 64-character hex-encoded Ed25519
//! verifying key produced by `cargo run -p tg-xtask -- keygen`) and emits `release_pubkey.rs`
//! into `OUT_DIR`.
//!
//! When `RUSTGRAM_UPDATE_PUBKEY` is unset or empty, an all-zero placeholder key is
//! emitted so the updater fails closed at runtime (no updates will be verified or
//! applied). Invalid hex or wrong-length values fail the build immediately.

use std::env;
use std::fs;
use std::path::PathBuf;

const ENV_VAR: &str = "RUSTGRAM_UPDATE_PUBKEY";
const LEGACY_ENV_VAR: &str = "TGFORK_UPDATE_PUBKEY";

fn main() {
    println!("cargo:rerun-if-env-changed={ENV_VAR}");
    println!("cargo:rerun-if-env-changed={LEGACY_ENV_VAR}");

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
