//! Signed update envelope packing, Ed25519 verification, and safe unpacking into `tupdates`.
//!
//! Port of `Telegram/SourceFiles/_other/packer.cpp`,
//! `Telegram/SourceFiles/core/update_verify.cpp`, and
//! `Telegram/SourceFiles/core/update_unpack.cpp`.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::github::MAX_PACKAGE_SIZE;
use crate::version::{
    PlatformArch, PlatformOs, Target, UpdateChannel, Version, channel_allows_update,
};
use crate::{TUPDATES_DIR_NAME, UPDATER_EXE_NAME, UpdateError};

/// Four-byte magic prefix for Rustgram signed update envelopes (inspired by tdesktop's `TDUP`).
pub const ENVELOPE_MAGIC: [u8; 4] = *b"TFUP";

/// Current envelope binary format version.
pub const ENVELOPE_FORMAT_VERSION: u32 = 1;

/// Domain-separation prefix included in Ed25519 signing input.
pub const SIGNING_DOMAIN: &[u8] = b"tgfork-update-v1\0";

/// Maximum permitted uncompressed bytes when extracting a package (1 GiB),
/// matching `kMaxUncompressedSize` in `core/update_unpack.cpp`.
pub const MAX_UNCOMPRESSED_SIZE: u64 = 1024 * 1024 * 1024;

/// Maximum number of files allowed in a single update archive.
pub const MAX_ARCHIVE_FILES: usize = 4096;

/// Maximum version string length in the binary envelope.
const MAX_VERSION_LEN: usize = 128;

/// Parsed (but not yet cryptographically verified) update envelope.
#[derive(Debug, Clone)]
pub struct PackageEnvelope {
    /// Declared update channel.
    pub channel: UpdateChannel,
    /// Declared platform target.
    pub target: Target,
    /// Declared semantic version.
    pub version: Version,
    /// Creation timestamp (Unix seconds).
    pub created_unix: i64,
    /// Raw 64-byte Ed25519 signature.
    pub signature: [u8; 64],
    /// ZIP archive payload bytes.
    pub payload: Vec<u8>,
    /// Exact header bytes covered by the signature.
    signed_header: Vec<u8>,
}

/// Cryptographically authenticated update package ready for decompression and staging.
#[derive(Debug, Clone)]
pub struct VerifiedPackage {
    /// Verified envelope.
    pub envelope: PackageEnvelope,
}

/// Computes the SHA-256 digest of `data`.
#[must_use]
pub fn sha256_digest(data: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// Verifies that `SHA-256(data)` matches `expected`.
pub fn verify_sha256(data: &[u8], expected: &[u8; 32]) -> Result<(), UpdateError> {
    let actual = sha256_digest(data);
    if actual != *expected {
        return Err(UpdateError::HashMismatch {
            expected: hex::encode(expected),
            actual: hex::encode(actual),
        });
    }
    Ok(())
}

impl PackageEnvelope {
    /// Returns the SHA-256 digest of the compressed archive payload.
    #[must_use]
    pub fn payload_sha256(&self) -> [u8; 32] {
        sha256_digest(&self.payload)
    }
}

/// Computes the canonical signing input: `SIGNING_DOMAIN || signed_header || SHA256(payload)`.
#[must_use]
pub fn compute_signing_input(signed_header: &[u8], payload: &[u8]) -> Vec<u8> {
    let payload_hash = Sha256::digest(payload);
    let mut out = Vec::with_capacity(SIGNING_DOMAIN.len() + signed_header.len() + 32);
    out.extend_from_slice(SIGNING_DOMAIN);
    out.extend_from_slice(signed_header);
    out.extend_from_slice(&payload_hash);
    out
}

/// Builds the signed header bytes for a `TFUP` v1 envelope.
fn build_signed_header(
    channel: UpdateChannel,
    target: Target,
    version: &Version,
    created_unix: i64,
) -> Result<Vec<u8>, UpdateError> {
    let ver_str = version.to_string();
    let ver_bytes = ver_str.as_bytes();
    if ver_bytes.is_empty() || ver_bytes.len() > MAX_VERSION_LEN {
        return Err(UpdateError::MalformedPackage(
            "version string length out of bounds".into(),
        ));
    }

    let mut header = Vec::with_capacity(4 + 4 + 4 + 4 + ver_bytes.len() + 8);
    header.extend_from_slice(&ENVELOPE_MAGIC);
    header.extend_from_slice(&ENVELOPE_FORMAT_VERSION.to_le_bytes());
    header.push(channel.to_u8());
    header.push(target.os as u8);
    header.push(target.arch as u8);
    header.push(0u8); // reserved
    header.extend_from_slice(&(ver_bytes.len() as u32).to_le_bytes());
    header.extend_from_slice(ver_bytes);
    header.extend_from_slice(&created_unix.to_le_bytes());
    Ok(header)
}

/// Packs `(relative_path, content)` entries into a compressed ZIP archive and wraps them
/// in an Ed25519-signed `TFUP` envelope.
///
/// Mirrors `WriteV2Update` + `VerifyPackedV2` in `_other/packer.cpp`: validates every path
/// and immediately verifies the produced package with the corresponding public key.
pub fn pack_update<'a, I>(
    channel: UpdateChannel,
    target: Target,
    version: &Version,
    created_unix: i64,
    files: I,
    signing_key: &SigningKey,
) -> Result<Vec<u8>, UpdateError>
where
    I: IntoIterator<Item = (&'a str, &'a [u8])>,
{
    let mut seen_files = HashSet::new();
    let mut seen_dirs = HashSet::new();
    let mut total_uncompressed: u64 = 0;

    let mut zip_buf = Cursor::new(Vec::new());
    {
        let mut writer = ZipWriter::new(&mut zip_buf);
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(0o755);

        let mut count = 0usize;
        for (rel_path, content) in files {
            count += 1;
            if count > MAX_ARCHIVE_FILES {
                return Err(UpdateError::ArchiveLimitExceeded(format!(
                    "too many files (>{MAX_ARCHIVE_FILES})"
                )));
            }
            if !is_safe_archive_path(rel_path)
                || !register_unique_path(rel_path, &mut seen_files, &mut seen_dirs)
            {
                return Err(UpdateError::UnsafePath(rel_path.to_string()));
            }
            total_uncompressed = total_uncompressed
                .checked_add(content.len() as u64)
                .ok_or_else(|| UpdateError::ArchiveLimitExceeded("size overflow".into()))?;
            if total_uncompressed > MAX_UNCOMPRESSED_SIZE {
                return Err(UpdateError::ArchiveLimitExceeded(format!(
                    "uncompressed size exceeds {MAX_UNCOMPRESSED_SIZE} bytes"
                )));
            }
            writer
                .start_file(rel_path, options)
                .map_err(|e| UpdateError::Archive(e.to_string()))?;
            writer
                .write_all(content)
                .map_err(|e| UpdateError::Archive(e.to_string()))?;
        }
        if count == 0 {
            return Err(UpdateError::MalformedPackage(
                "update archive must contain at least one file".into(),
            ));
        }
        writer
            .finish()
            .map_err(|e| UpdateError::Archive(e.to_string()))?;
    }

    let payload = zip_buf.into_inner();
    if payload.len() > MAX_PACKAGE_SIZE {
        return Err(UpdateError::ArchiveLimitExceeded(format!(
            "compressed payload {} exceeds {MAX_PACKAGE_SIZE} bytes",
            payload.len()
        )));
    }

    let signed_header = build_signed_header(channel, target, version, created_unix)?;
    let signing_input = compute_signing_input(&signed_header, &payload);
    let signature: Signature = signing_key.sign(&signing_input);

    let mut envelope = Vec::with_capacity(signed_header.len() + 64 + 4 + payload.len());
    envelope.extend_from_slice(&signed_header);
    envelope.extend_from_slice(&signature.to_bytes());
    envelope.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    envelope.extend_from_slice(&payload);

    // Build-time tripwire (matching VerifyPackedV2 in packer.cpp): verify before returning.
    let verifying_key = signing_key.verifying_key();
    let parsed = parse_envelope(&envelope)?;
    verify_envelope_signature(&parsed, &verifying_key)?;

    Ok(envelope)
}

/// Recursively packs all regular files under `root_dir` into a deterministically ordered,
/// Ed25519-signed `TFUP` update package (port of directory traversal in `_other/packer.cpp`).
///
/// Control entries (`ready`, `tdata/**`) are skipped automatically.
pub fn pack_directory(
    root_dir: &Path,
    channel: UpdateChannel,
    target: Target,
    version: &Version,
    created_unix: i64,
    signing_key: &SigningKey,
) -> Result<Vec<u8>, UpdateError> {
    let mut collected: Vec<(String, Vec<u8>)> = Vec::new();
    collect_directory_files(root_dir, root_dir, &mut collected)?;
    collected.sort_by(|a, b| a.0.cmp(&b.0));

    let refs: Vec<(&str, &[u8])> = collected
        .iter()
        .map(|(rel, bytes)| (rel.as_str(), bytes.as_slice()))
        .collect();
    pack_update(channel, target, version, created_unix, refs, signing_key)
}

fn collect_directory_files(
    root: &Path,
    current: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), UpdateError> {
    let mut entries: Vec<PathBuf> = fs::read_dir(current)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|e| e.path())
        .collect();
    entries.sort();

    for path in entries {
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            return Err(UpdateError::UnsafePath(path.display().to_string()));
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|_| UpdateError::UnsafePath(path.display().to_string()))?;
        let rel_str = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let lower = rel_str.to_ascii_lowercase();
        if lower == "ready" || lower == "tdata" || lower.starts_with("tdata/") {
            continue;
        }
        if meta.is_dir() {
            collect_directory_files(root, &path, out)?;
        } else if meta.is_file() {
            out.push((rel_str, fs::read(&path)?));
        }
    }
    Ok(())
}

/// Parses a `TFUP` binary envelope without verifying its signature.
pub fn parse_envelope(data: &[u8]) -> Result<PackageEnvelope, UpdateError> {
    if data.len() < 16 {
        return Err(UpdateError::MalformedPackage(
            "envelope shorter than header".into(),
        ));
    }
    if data[0..4] != ENVELOPE_MAGIC {
        return Err(UpdateError::MalformedPackage("bad envelope magic".into()));
    }
    let format = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    if format != ENVELOPE_FORMAT_VERSION {
        return Err(UpdateError::MalformedPackage(format!(
            "unsupported envelope format version {format}"
        )));
    }

    let channel = UpdateChannel::from_u8(data[8])
        .ok_or_else(|| UpdateError::MalformedPackage(format!("unknown channel {}", data[8])))?;
    let os = PlatformOs::from_u8(data[9])
        .ok_or_else(|| UpdateError::MalformedPackage(format!("unknown os {}", data[9])))?;
    let arch = PlatformArch::from_u8(data[10])
        .ok_or_else(|| UpdateError::MalformedPackage(format!("unknown arch {}", data[10])))?;
    if data[11] != 0 {
        return Err(UpdateError::MalformedPackage(
            "non-zero reserved header byte".into(),
        ));
    }

    let ver_len = u32::from_le_bytes([data[12], data[13], data[14], data[15]]) as usize;
    if ver_len == 0 || ver_len > MAX_VERSION_LEN {
        return Err(UpdateError::MalformedPackage(
            "invalid version length".into(),
        ));
    }

    let ver_end = 16usize
        .checked_add(ver_len)
        .ok_or_else(|| UpdateError::MalformedPackage("version length overflow".into()))?;
    let signed_header_end = ver_end
        .checked_add(8)
        .ok_or_else(|| UpdateError::MalformedPackage("header overflow".into()))?;
    let sig_end = signed_header_end
        .checked_add(64)
        .ok_or_else(|| UpdateError::MalformedPackage("signature offset overflow".into()))?;
    let payload_len_end = sig_end
        .checked_add(4)
        .ok_or_else(|| UpdateError::MalformedPackage("payload len offset overflow".into()))?;

    if data.len() < payload_len_end {
        return Err(UpdateError::MalformedPackage("truncated envelope".into()));
    }

    let ver_str = std::str::from_utf8(&data[16..ver_end])
        .map_err(|_| UpdateError::MalformedPackage("non-UTF-8 version string".into()))?;
    let version = Version::parse(ver_str)?;

    let created_bytes: [u8; 8] = data[ver_end..signed_header_end]
        .try_into()
        .map_err(|_| UpdateError::MalformedPackage("truncated timestamp".into()))?;
    let created_unix = i64::from_le_bytes(created_bytes);

    let signature: [u8; 64] = data[signed_header_end..sig_end]
        .try_into()
        .map_err(|_| UpdateError::MalformedPackage("truncated signature".into()))?;

    let payload_len_bytes: [u8; 4] = data[sig_end..payload_len_end]
        .try_into()
        .map_err(|_| UpdateError::MalformedPackage("truncated payload length".into()))?;
    let payload_len = u32::from_le_bytes(payload_len_bytes) as usize;

    if payload_len == 0 || payload_len > MAX_PACKAGE_SIZE {
        return Err(UpdateError::MalformedPackage(format!(
            "invalid payload size {payload_len}"
        )));
    }
    if data.len() != payload_len_end + payload_len {
        return Err(UpdateError::MalformedPackage(
            "envelope size mismatch or trailing bytes".into(),
        ));
    }

    Ok(PackageEnvelope {
        channel,
        target: Target { os, arch },
        version,
        created_unix,
        signature,
        payload: data[payload_len_end..].to_vec(),
        signed_header: data[..signed_header_end].to_vec(),
    })
}

/// Verifies the Ed25519 signature of a parsed [`PackageEnvelope`] using strict verification.
pub fn verify_envelope_signature(
    envelope: &PackageEnvelope,
    verifying_key: &VerifyingKey,
) -> Result<(), UpdateError> {
    if crate::is_placeholder_public_key(verifying_key.as_bytes()) {
        return Err(UpdateError::UnconfiguredPublicKey);
    }
    let sig = Signature::from_bytes(&envelope.signature);
    let signing_input = compute_signing_input(&envelope.signed_header, &envelope.payload);
    verifying_key
        .verify_strict(&signing_input, &sig)
        .map_err(|_| UpdateError::InvalidSignature)
}

/// Verifies a standalone detached Ed25519 signature over arbitrary bytes.
pub fn verify_detached_signature(
    verifying_key: &VerifyingKey,
    data: &[u8],
    signature: &[u8; 64],
) -> Result<(), UpdateError> {
    if crate::is_placeholder_public_key(verifying_key.as_bytes()) {
        return Err(UpdateError::UnconfiguredPublicKey);
    }
    let sig = Signature::from_bytes(signature);
    verifying_key
        .verify_strict(data, &sig)
        .map_err(|_| UpdateError::InvalidSignature)
}

/// Parses and fully authenticates an update package BEFORE any decompression occurs.
///
/// Mirrors `Core::Updates::VerifyUpdate` in `core/update_verify.cpp`:
/// 1. Validates binary structure.
/// 2. Verifies Ed25519 signature strictly.
/// 3. Checks target OS and architecture match.
/// 4. Enforces channel policy (rejects beta when `allow_beta == false`).
/// 5. Enforces anti-downgrade (`envelope.version > current_version`).
pub fn verify_package(
    data: &[u8],
    verifying_key: &VerifyingKey,
    current_version: &Version,
    allow_beta: bool,
    expected_target: Target,
) -> Result<VerifiedPackage, UpdateError> {
    let envelope = parse_envelope(data)?;
    verify_envelope_signature(&envelope, verifying_key)?;

    if envelope.target != expected_target {
        return Err(UpdateError::TargetMismatch {
            expected: expected_target.to_string(),
            actual: envelope.target.to_string(),
        });
    }

    if !allow_beta && (envelope.channel == UpdateChannel::Beta || envelope.version.is_prerelease())
    {
        return Err(UpdateError::ChannelNotAllowed);
    }

    if !channel_allows_update(
        current_version,
        &envelope.version,
        envelope.channel,
        allow_beta,
    ) {
        return Err(UpdateError::NotNewer {
            current: current_version.to_string(),
            package: envelope.version.to_string(),
        });
    }

    Ok(VerifiedPackage { envelope })
}

/// Parses and authenticates an update package, additionally verifying an optional expected
/// SHA-256 digest over the raw `.tupdate` bytes before parsing/decompression.
pub fn verify_package_with_hash(
    data: &[u8],
    expected_sha256: Option<&[u8; 32]>,
    verifying_key: &VerifyingKey,
    current_version: &Version,
    allow_beta: bool,
    expected_target: Target,
) -> Result<VerifiedPackage, UpdateError> {
    if let Some(expected) = expected_sha256 {
        verify_sha256(data, expected)?;
    }
    verify_package(
        data,
        verifying_key,
        current_version,
        allow_beta,
        expected_target,
    )
}

/// Validates an archive entry path for safety across Windows and POSIX filesystems.
///
/// Direct port of `SafeName` in `Telegram/SourceFiles/core/update_unpack.cpp`.
#[must_use]
pub fn is_safe_archive_path(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    for ch in name.chars() {
        if (ch as u32) < 32 || matches!(ch, '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*') {
            return false;
        }
    }
    for part in name.split('/') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with('.')
            || part.ends_with(' ')
        {
            return false;
        }
        let stem = part
            .split('.')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_uppercase();
        if matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) {
            return false;
        }
        let stem_chars: Vec<char> = stem.chars().collect();
        if stem_chars.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(
                stem_chars[3],
                '1'..='9' | '\u{00B9}' | '\u{00B2}' | '\u{00B3}'
            )
        {
            return false;
        }
    }
    true
}

/// Tracks case-insensitive path uniqueness and file/directory conflicts.
///
/// Port of `AddName` in `Telegram/SourceFiles/core/update_unpack.cpp`.
fn register_unique_path(
    name: &str,
    files: &mut HashSet<String>,
    directories: &mut HashSet<String>,
) -> bool {
    let key = name.to_lowercase();
    if files.contains(&key) || directories.contains(&key) {
        return false;
    }
    for (idx, ch) in key.char_indices() {
        if ch == '/' {
            let parent = &key[..idx];
            if files.contains(parent) {
                return false;
            }
            directories.insert(parent.to_string());
        }
    }
    files.insert(key);
    true
}

/// Writes `<temp_dir>/tdata/version` in tdesktop's binary layout so `rustgram-updater`
/// can update Windows Uninstall registry metadata (`DisplayVersion`).
///
/// Port of `WriteUpdateVersionFile` in `core/update_checker.cpp` and reader in `_other/updater_win.cpp`.
pub fn write_version_file(temp_dir: &Path, version: &Version) -> Result<PathBuf, UpdateError> {
    let tdata_dir = temp_dir.join("tdata");
    fs::create_dir_all(&tdata_dir)?;
    let path = tdata_dir.join("version");

    let numeric = version.to_tdesktop_numeric();
    let version_text = version.to_string();
    let utf16_units: Vec<u16> = version_text.encode_utf16().collect();
    let utf16_byte_len = (utf16_units.len() * 2) as u32;

    let mut file = File::create(&path)?;
    file.write_all(&numeric.to_le_bytes())?;
    file.write_all(&utf16_byte_len.to_le_bytes())?;
    for unit in utf16_units {
        file.write_all(&unit.to_le_bytes())?;
    }
    file.flush()?;
    Ok(path)
}

/// Reads `<dir>/tdata/version` written by [`write_version_file`].
pub fn read_version_file(path: &Path) -> Result<(u32, String), UpdateError> {
    let bytes = fs::read(path)?;
    if bytes.len() < 8 {
        return Err(UpdateError::MalformedPackage(
            "version file too short".into(),
        ));
    }
    let numeric = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let str_byte_len = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    if str_byte_len > 256 || !str_byte_len.is_multiple_of(2) || bytes.len() < 8 + str_byte_len {
        return Err(UpdateError::MalformedPackage(
            "invalid version file length".into(),
        ));
    }
    let mut units = Vec::with_capacity(str_byte_len / 2);
    let (chunks, _) = bytes[8..8 + str_byte_len].as_chunks::<2>();
    for &chunk in chunks {
        units.push(u16::from_le_bytes(chunk));
    }
    let text = String::from_utf16(&units)
        .map_err(|_| UpdateError::MalformedPackage("invalid UTF-16 version string".into()))?;
    Ok((numeric, text))
}

/// Reads the version of the update staged in `staged_dir` (`tupdates/temp` or the legacy
/// `tupdates/ready`) from its `tdata/version` file.
pub fn read_staged_version(staged_dir: &Path) -> Result<Version, UpdateError> {
    let (_, text) = read_version_file(&staged_dir.join("tdata").join("version"))?;
    Version::parse(&text)
}

/// Extracts an authenticated [`VerifiedPackage`] into `<working_dir>/tupdates/temp`,
/// writes `tdata/version`, and creates the `ready` marker file as the final atomic step.
///
/// Port of `UnpackUpdateV2` and `ExtractUpdateFiles` in `core/update_checker.cpp`
/// and `core/update_unpack.cpp`.
pub fn unpack_verified_package(
    verified: &VerifiedPackage,
    working_dir: &Path,
) -> Result<PathBuf, UpdateError> {
    let tupdates_dir = working_dir.join(TUPDATES_DIR_NAME);
    let temp_dir = tupdates_dir.join("temp");

    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir)?;
    }
    fs::create_dir_all(&temp_dir)?;

    let cursor = Cursor::new(&verified.envelope.payload);
    let mut archive = ZipArchive::new(cursor).map_err(|e| UpdateError::Archive(e.to_string()))?;

    if archive.is_empty() {
        return Err(UpdateError::MalformedPackage("empty update archive".into()));
    }
    if archive.len() > MAX_ARCHIVE_FILES {
        return Err(UpdateError::ArchiveLimitExceeded(format!(
            "archive has {} entries (limit {MAX_ARCHIVE_FILES})",
            archive.len()
        )));
    }

    let mut seen_files = HashSet::new();
    let mut seen_dirs = HashSet::new();
    let mut total_written: u64 = 0;

    for idx in 0..archive.len() {
        let mut entry = archive
            .by_index(idx)
            .map_err(|e| UpdateError::Archive(e.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry
            .name()
            .map_err(|e| UpdateError::Archive(e.to_string()))?
            .into_owned();
        // `pack_update` never writes symlinks; one in a package is malformed or hostile.
        if entry.is_symlink()
            || !is_safe_archive_path(&name)
            || !register_unique_path(&name, &mut seen_files, &mut seen_dirs)
        {
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(UpdateError::UnsafePath(name));
        }

        // Prevent archive entries from spoofing the updater's own control files.
        let lower = name.to_ascii_lowercase();
        if lower == "ready" || lower == "tdata" || lower.starts_with("tdata/") {
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(UpdateError::UnsafePath(name));
        }

        let remaining = MAX_UNCOMPRESSED_SIZE.saturating_sub(total_written);
        let mut data = Vec::new();
        entry
            .by_ref()
            .take(remaining.saturating_add(1))
            .read_to_end(&mut data)
            .map_err(|e| UpdateError::Archive(e.to_string()))?;
        if data.len() as u64 > remaining {
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(UpdateError::ArchiveLimitExceeded(format!(
                "uncompressed content exceeds {MAX_UNCOMPRESSED_SIZE} bytes"
            )));
        }
        total_written += data.len() as u64;

        let out_path = temp_dir.join(&name);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .open(&out_path)?;
        file.write_all(&data)?;
        file.flush()?;
    }

    write_version_file(&temp_dir, &verified.envelope.version)?;

    // Write the `ready` marker strictly last, matching WriteUpdateReadyFile in update_checker.cpp.
    let ready_path = temp_dir.join("ready");
    let mut ready_file = File::options()
        .write(true)
        .create_new(true)
        .open(&ready_path)?;
    ready_file.write_all(b"1")?;
    ready_file.flush()?;

    Ok(temp_dir)
}

/// Promotes a newly unpacked `rustgram-updater.exe` out of `tupdates/temp/` into `install_dir`
/// before launching it, so the running updater never has to overwrite itself.
///
/// Port of `PromoteReadyUpdater` in `Telegram/SourceFiles/core/update_checker.cpp`.
pub fn promote_ready_updater(working_dir: &Path, install_dir: &Path) -> Result<bool, UpdateError> {
    let staged_updater = working_dir
        .join(TUPDATES_DIR_NAME)
        .join("temp")
        .join(UPDATER_EXE_NAME);
    if !staged_updater.is_file() {
        return Ok(false);
    }
    fs::create_dir_all(install_dir)?;
    let target_updater = install_dir.join(UPDATER_EXE_NAME);
    fs::copy(&staged_updater, &target_updater)?;
    fs::remove_file(&staged_updater)?;
    Ok(true)
}
