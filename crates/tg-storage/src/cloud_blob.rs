//! Cloud blob staged downloader, SHA-256 verifier, and safe atomic ZIP unpacker.
//!
//! Port of `Telegram/SourceFiles/storage/storage_cloud_blob.h` and `storage_cloud_blob.cpp`,
//! hardened with:
//! - Staged download to a sibling temporary `.part` file with streaming byte limits.
//! - Mandatory SHA-256 digest verification (constant-time) before any archive parsing or
//!   decompression when an expected digest is configured.
//! - Strict ZIP extraction rejecting zip-slip traversal (`..`), absolute paths, backslashes,
//!   Windows drive letters / reserved device names, symlinks (`S_IFLNK`), case-insensitive
//!   path collisions, and zip-bomb decompressed byte limits.
//! - Atomic staging-directory swap so any error leaves an existing target directory untouched.

use std::collections::HashSet;
use std::fs::{self, File};
use std::future::Future;
use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::net::TcpStream;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tg_crypto::{Sha256Hasher, const_time_eq, random_u64, sha256_bytes};
use tokio::sync::watch;
use zip::ZipArchive;

use crate::error::StorageError;

/// Telegram channel username used for official cloud blob bundles
/// (`kCloudLocationUsername` in `storage_cloud_blob.h`).
pub const CLOUD_LOCATION_USERNAME: &str = "tdhbcfiles";

/// Maximum compressed cloud blob archive size (10 MiB, matching `kMaxZipSize` in `ReadFinalFile`).
pub const MAX_CLOUD_BLOB_ZIP_SIZE: u64 = 10 * 1024 * 1024;

/// Maximum uncompressed size of any single extracted file (25 MiB, matching `kMaxSize` in
/// `ExtractZipFile`).
pub const MAX_EXTRACTED_FILE_SIZE: u64 = 25 * 1024 * 1024;

/// Default cap on total uncompressed bytes extracted from a cloud blob archive (128 MiB).
pub const DEFAULT_MAX_TOTAL_UNCOMPRESSED_SIZE: u64 = 128 * 1024 * 1024;

/// Default cap on total file entries allowed in a single cloud blob archive.
pub const DEFAULT_MAX_ARCHIVE_FILES: usize = 4096;

/// POSIX `S_IFMT` file-type mask in ZIP `external_attributes >> 16`.
const UNIX_FILE_TYPE_MASK: u32 = 0o170_000;

/// POSIX `S_IFLNK` symlink mode bits in ZIP `external_attributes >> 16`.
const UNIX_SYMLINK_MODE: u32 = 0o120_000;

/// Errors produced by cloud blob download, verification, and unpacking operations.
#[derive(Debug, thiserror::Error)]
pub enum CloudBlobError {
    /// Underlying filesystem or network I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// ZIP container parsing or decompression error.
    #[error("archive error: {0}")]
    Archive(String),

    /// Compressed archive exceeds configured size limit.
    #[error("cloud blob archive size {size} exceeds limit {limit}")]
    ArchiveTooLarge {
        /// Actual or declared archive size in bytes.
        size: u64,
        /// Configured upper bound in bytes.
        limit: u64,
    },

    /// Archive contains no extractable entries.
    #[error("cloud blob archive is empty")]
    EmptyArchive,

    /// SHA-256 verification failed before extraction.
    #[error("SHA-256 mismatch: expected {expected}, got {actual}")]
    Sha256Mismatch {
        /// Lowercase hex expected digest.
        expected: String,
        /// Lowercase hex actual digest.
        actual: String,
    },

    /// Entry path failed traversal, absolute-path, or filename safety validation.
    #[error("unsafe archive entry path: {0}")]
    UnsafePath(String),

    /// Archive contained a symlink entry (rejected unconditionally).
    #[error("symlink entry rejected in archive: {0}")]
    SymlinkRejected(String),

    /// Archive contained duplicate or conflicting case-insensitive paths.
    #[error("duplicate or conflicting archive path: {0}")]
    DuplicatePath(String),

    /// Archive exceeded maximum file entry count.
    #[error("archive entry count {count} exceeds limit {limit}")]
    FileCountLimitExceeded {
        /// Observed entry count.
        count: usize,
        /// Configured maximum entries.
        limit: usize,
    },

    /// A single decompressed entry exceeded the single-file byte limit.
    #[error("entry '{path}' exceeds single-file limit of {limit} bytes")]
    FileSizeLimitExceeded {
        /// Archive entry path.
        path: String,
        /// Configured single-file byte limit.
        limit: u64,
    },

    /// Cumulative decompressed bytes exceeded the archive total byte budget.
    #[error("total uncompressed bytes exceed limit of {limit} bytes")]
    TotalSizeLimitExceeded {
        /// Configured total uncompressed byte limit.
        limit: u64,
    },

    /// Target folder path cannot be safely swapped (e.g., filesystem root, `.`, or `..`).
    #[error("invalid target folder for atomic swap: {0}")]
    InvalidTargetFolder(PathBuf),

    /// Download transport or source failure.
    #[error("cloud blob download failed: {0}")]
    DownloadFailed(String),

    /// Operation was cancelled by the caller.
    #[error("cloud blob operation cancelled")]
    Cancelled,
}

impl From<CloudBlobError> for StorageError {
    fn from(err: CloudBlobError) -> Self {
        match err {
            CloudBlobError::Io(io) => Self::Io(io),
            CloudBlobError::Sha256Mismatch { .. } => Self::ChecksumMismatch,
            other => Self::CorruptData(other.to_string()),
        }
    }
}

/// Result alias for cloud blob operations.
pub type CloudBlobResult<T> = std::result::Result<T, CloudBlobError>;

/// Remote cloud blob location (`MTP::DedicatedLoader::Location`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CloudBlobLocation {
    /// Channel username hosting the blob post (defaults to [`CLOUD_LOCATION_USERNAME`]).
    pub username: String,
    /// Post identifier inside the channel.
    pub post_id: i32,
}

impl CloudBlobLocation {
    /// Create a location in the official `tdhbcfiles` channel.
    #[must_use]
    pub fn official(post_id: i32) -> Self {
        Self {
            username: CLOUD_LOCATION_USERNAME.to_string(),
            post_id,
        }
    }

    /// Create a location for an arbitrary channel username and post ID.
    #[must_use]
    pub fn new(username: impl Into<String>, post_id: i32) -> Self {
        Self {
            username: username.into(),
            post_id,
        }
    }
}

/// Metadata describing an available cloud blob (`Storage::CloudBlob::Blob`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudBlobDescriptor {
    /// Logical blob identifier (e.g. emoji pack ID or language pack ID).
    pub id: i32,
    /// Channel post ID for dedicated loading.
    pub post_id: i32,
    /// Declared compressed size in bytes.
    pub size: u64,
    /// Human-readable display name.
    pub name: String,
    /// Optional expected SHA-256 digest of the compressed archive.
    pub expected_sha256: Option<[u8; 32]>,
}

/// Transfer progress for an in-flight cloud blob download (`MTP::DedicatedLoader::Progress`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlobProgress {
    /// Bytes downloaded so far.
    pub already: u64,
    /// Total expected bytes (`0` if unknown).
    pub size: u64,
}

impl BlobProgress {
    /// Completion fraction clamped to `[0.0, 1.0]`.
    #[must_use]
    pub fn fraction(self) -> f64 {
        if self.size == 0 {
            0.0
        } else {
            (self.already as f64 / self.size as f64).clamp(0.0, 1.0)
        }
    }

    /// Rounded completion percentage in `0..=100` (matching `StateDescription` in
    /// `storage_cloud_blob.cpp`).
    #[must_use]
    pub fn percent(self) -> u8 {
        (self.fraction() * 100.0).round().clamp(0.0, 100.0) as u8
    }
}

/// Lifecycle state of a cloud blob (`Storage::CloudBlob::BlobState`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobState {
    /// Available for download from the cloud.
    Available {
        /// Declared compressed size in bytes.
        size: u64,
    },
    /// Downloaded, verified, and unpacked locally.
    Ready,
    /// Currently active / selected.
    Active,
    /// Download, verification, or unpacking failed.
    Failed(String),
    /// Currently downloading.
    Loading(BlobProgress),
}

/// Format a human-readable description of a [`BlobState`], mirroring `StateDescription` in
/// `storage_cloud_blob.cpp`.
#[must_use]
pub fn state_description(state: &BlobState, active_text: &str) -> String {
    match state {
        BlobState::Available { size } => format!("Download ({})", format_byte_size(*size)),
        BlobState::Ready => "Ready".to_string(),
        BlobState::Active => active_text.to_string(),
        BlobState::Loading(progress) => format!(
            "Loading {}% ({} / {})",
            progress.percent(),
            format_byte_size(progress.already),
            format_byte_size(progress.size),
        ),
        BlobState::Failed(reason) => {
            if reason.is_empty() {
                "Failed".to_string()
            } else {
                format!("Failed: {reason}")
            }
        }
    }
}

fn format_byte_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.1} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// Policy and security bounds for unpacking a cloud blob archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnpackOptions {
    /// Optional expected SHA-256 digest of the compressed archive bytes.
    /// Verified in constant time BEFORE parsing or decompressing the ZIP.
    pub expected_sha256: Option<[u8; 32]>,
    /// Maximum compressed archive size in bytes (default: [`MAX_CLOUD_BLOB_ZIP_SIZE`]).
    pub max_archive_size: u64,
    /// Maximum uncompressed size of any single file entry (default: [`MAX_EXTRACTED_FILE_SIZE`]).
    pub max_file_size: u64,
    /// Maximum total uncompressed bytes across the entire archive
    /// (default: [`DEFAULT_MAX_TOTAL_UNCOMPRESSED_SIZE`]).
    pub max_total_uncompressed_size: u64,
    /// Maximum number of entries in the archive (default: [`DEFAULT_MAX_ARCHIVE_FILES`]).
    pub max_file_count: usize,
}

impl Default for UnpackOptions {
    fn default() -> Self {
        Self {
            expected_sha256: None,
            max_archive_size: MAX_CLOUD_BLOB_ZIP_SIZE,
            max_file_size: MAX_EXTRACTED_FILE_SIZE,
            max_total_uncompressed_size: DEFAULT_MAX_TOTAL_UNCOMPRESSED_SIZE,
            max_file_count: DEFAULT_MAX_ARCHIVE_FILES,
        }
    }
}

/// Summary returned upon a successful verified atomic cloud blob unpack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnpackReport {
    /// Active directory where the unpacked files now reside.
    pub target_dir: PathBuf,
    /// Verified SHA-256 digest of the compressed archive bytes.
    pub sha256: [u8; 32],
    /// Relative forward-slash paths of all files extracted into `target_dir`.
    pub extracted_files: Vec<String>,
    /// Total uncompressed bytes written to disk.
    pub total_uncompressed_bytes: u64,
}

/// Format a 32-byte digest as lowercase hex.
#[must_use]
pub fn sha256_hex(digest: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

/// Compute and verify the SHA-256 digest of `bytes` against `expected` using a constant-time
/// comparison.
pub fn verify_sha256(bytes: &[u8], expected: &[u8; 32]) -> CloudBlobResult<[u8; 32]> {
    let actual = sha256_bytes(bytes);
    if !const_time_eq(&actual, expected) {
        return Err(CloudBlobError::Sha256Mismatch {
            expected: sha256_hex(expected),
            actual: sha256_hex(&actual),
        });
    }
    Ok(actual)
}

/// Stream-compute the SHA-256 digest of a file on disk without buffering it in memory.
pub fn sha256_file(path: &Path) -> CloudBlobResult<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256Hasher::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize())
}

/// Validate a ZIP entry path for cross-platform archive safety (POSIX + Windows).
///
/// Rejects:
/// - Empty paths or leading `/` (absolute POSIX paths)
/// - Backslashes `\` (traversal / UNC paths / Windows separator confusion)
/// - Control characters (`< 0x20`), NUL bytes, and Windows-illegal characters (`: < > " | ? *`)
/// - `.` and `..` components, empty segments (`a//b`), trailing dots or spaces in any segment
/// - Windows reserved device names (`CON`, `PRN`, `AUX`, `NUL`, `COM1`..`COM9`, `LPT1`..`LPT9`)
pub(crate) fn validate_archive_entry_path(raw: &str, is_dir: bool) -> CloudBlobResult<String> {
    let trimmed = if is_dir {
        raw.strip_suffix('/').unwrap_or(raw)
    } else {
        raw
    };
    if trimmed.is_empty() {
        return Err(CloudBlobError::UnsafePath(raw.to_string()));
    }
    if trimmed.starts_with('/') {
        return Err(CloudBlobError::UnsafePath(raw.to_string()));
    }
    for ch in trimmed.chars() {
        if (ch as u32) < 32 || matches!(ch, '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*') {
            return Err(CloudBlobError::UnsafePath(raw.to_string()));
        }
    }
    for segment in trimmed.split('/') {
        if segment.is_empty()
            || segment == "."
            || segment == ".."
            || segment.ends_with('.')
            || segment.ends_with(' ')
        {
            return Err(CloudBlobError::UnsafePath(raw.to_string()));
        }
        if is_windows_reserved_name(segment) {
            return Err(CloudBlobError::UnsafePath(raw.to_string()));
        }
    }

    // Double-check with `std::path::Path` component parser to guarantee every component is Normal.
    let rel = Path::new(trimmed);
    if rel.is_absolute()
        || !rel
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(CloudBlobError::UnsafePath(raw.to_string()));
    }

    Ok(trimmed.to_string())
}

/// Returns `true` if `raw` is a safe relative archive file path.
#[must_use]
pub fn is_safe_archive_path(raw: &str) -> bool {
    validate_archive_entry_path(raw, false).is_ok()
}

fn is_windows_reserved_name(segment: &str) -> bool {
    let stem = segment
        .split('.')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let chars: Vec<char> = stem.chars().collect();
    chars.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && matches!(chars[3], '1'..='9' | '\u{00B9}' | '\u{00B2}' | '\u{00B3}')
}

/// Returns `true` if a ZIP entry represents a symbolic link via `zip::read::ZipFile::is_symlink`
/// or POSIX `S_IFLNK` mode bits.
pub(crate) fn is_zip_entry_symlink<R: Read>(entry: &zip::read::ZipFile<'_, R>) -> bool {
    if entry.is_symlink() {
        return true;
    }
    entry
        .unix_mode()
        .is_some_and(|mode| (mode & UNIX_FILE_TYPE_MASK) == UNIX_SYMLINK_MODE)
}

/// Tracks case-insensitive path uniqueness and detects file-vs-directory conflicts.
pub(crate) fn register_unique_archive_path(
    normalized: &str,
    is_dir: bool,
    seen_files: &mut HashSet<String>,
    seen_dirs: &mut HashSet<String>,
) -> bool {
    let key = normalized.to_ascii_lowercase();
    if is_dir {
        if seen_files.contains(&key) {
            return false;
        }
        for (idx, ch) in key.char_indices() {
            if ch == '/' {
                let parent = &key[..idx];
                if seen_files.contains(parent) {
                    return false;
                }
                seen_dirs.insert(parent.to_string());
            }
        }
        seen_dirs.insert(key);
        true
    } else {
        if seen_files.contains(&key) || seen_dirs.contains(&key) {
            return false;
        }
        for (idx, ch) in key.char_indices() {
            if ch == '/' {
                let parent = &key[..idx];
                if seen_files.contains(parent) {
                    return false;
                }
                seen_dirs.insert(parent.to_string());
            }
        }
        seen_files.insert(key);
        true
    }
}

/// Validate `target_folder` and derive its parent directory and leaf directory name so staging
/// and backup directories live on the same filesystem for atomic renames.
fn validate_target_folder(target_folder: &Path) -> CloudBlobResult<(PathBuf, String)> {
    let file_name = target_folder
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| CloudBlobError::InvalidTargetFolder(target_folder.to_path_buf()))?;
    if file_name.is_empty()
        || file_name == "."
        || file_name == ".."
        || file_name.contains(':')
        || file_name.contains('/')
        || file_name.contains('\\')
    {
        return Err(CloudBlobError::InvalidTargetFolder(
            target_folder.to_path_buf(),
        ));
    }
    let parent = target_folder
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    Ok((parent, file_name.to_string()))
}

fn create_unique_sibling_dir(parent: &Path, prefix: &str) -> CloudBlobResult<PathBuf> {
    fs::create_dir_all(parent)?;
    for _ in 0..32 {
        let nonce = random_u64();
        let candidate = parent.join(format!(".{prefix}-{nonce:016x}"));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(CloudBlobError::Io(err)),
        }
    }
    Err(CloudBlobError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "failed to allocate unique staging directory",
    )))
}

fn create_unique_sibling_file(parent: &Path, prefix: &str) -> CloudBlobResult<(PathBuf, File)> {
    fs::create_dir_all(parent)?;
    for _ in 0..32 {
        let nonce = random_u64();
        let candidate = parent.join(format!(".{prefix}-{nonce:016x}.part"));
        match File::options()
            .write(true)
            .read(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(CloudBlobError::Io(err)),
        }
    }
    Err(CloudBlobError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "failed to allocate unique staging file",
    )))
}

/// Atomically replace `target_folder` with `staging_dir`, rolling back on failure so an existing
/// `target_folder` is never lost or left half-replaced.
fn atomic_swap_directory(staging_dir: &Path, target_folder: &Path) -> CloudBlobResult<()> {
    let (parent, leaf) = validate_target_folder(target_folder)?;
    if !target_folder.exists() {
        if let Err(err) = fs::rename(staging_dir, target_folder) {
            let _ = fs::remove_dir_all(staging_dir);
            return Err(CloudBlobError::Io(err));
        }
        return Ok(());
    }

    let backup_path = loop {
        let nonce = random_u64();
        let candidate = parent.join(format!(".{leaf}.backup-{nonce:016x}"));
        if !candidate.exists() {
            break candidate;
        }
    };

    // Step 1: Move existing active directory aside.
    if let Err(err) = fs::rename(target_folder, &backup_path) {
        let _ = fs::remove_dir_all(staging_dir);
        return Err(CloudBlobError::Io(err));
    }

    // Step 2: Move fully extracted staging directory into place.
    if let Err(err) = fs::rename(staging_dir, target_folder) {
        // Roll back the active directory before surfacing the error.
        let _ = fs::rename(&backup_path, target_folder);
        let _ = fs::remove_dir_all(staging_dir);
        return Err(CloudBlobError::Io(err));
    }

    // Step 3: Remove backup directory (best-effort cleanup after successful swap).
    let _ = fs::remove_dir_all(&backup_path);
    Ok(())
}

/// Verify and unpack in-memory ZIP `archive_bytes` into a sibling staging directory and atomically
/// swap it into `target_folder`.
///
/// Mirrors `Storage::CloudBlob::UnpackBlob` in `storage_cloud_blob.cpp` while failing closed on:
/// - archive size exceeding `options.max_archive_size`,
/// - SHA-256 mismatch against `options.expected_sha256`,
/// - zip-slip (`..`), absolute paths, backslashes, or Windows reserved names (checked for EVERY
///   entry, regardless of `check_name`),
/// - symlink entries (`S_IFLNK`),
/// - duplicate/conflicting case-insensitive paths,
/// - single-file or total uncompressed byte budget overruns.
///
/// If any check or write fails, the staging directory is removed and any pre-existing
/// `target_folder` remains completely untouched.
pub fn unpack_blob_bytes<F>(
    archive_bytes: &[u8],
    target_folder: &Path,
    options: &UnpackOptions,
    mut check_name: F,
) -> CloudBlobResult<UnpackReport>
where
    F: FnMut(&str) -> bool,
{
    let archive_len = archive_bytes.len() as u64;
    if archive_len == 0 {
        return Err(CloudBlobError::EmptyArchive);
    }
    if archive_len > options.max_archive_size {
        return Err(CloudBlobError::ArchiveTooLarge {
            size: archive_len,
            limit: options.max_archive_size,
        });
    }

    // Verify SHA-256 BEFORE touching the ZIP parser or filesystem.
    let actual_sha256 = if let Some(expected) = &options.expected_sha256 {
        verify_sha256(archive_bytes, expected)?
    } else {
        sha256_bytes(archive_bytes)
    };

    let (parent, leaf) = validate_target_folder(target_folder)?;
    let cursor = Cursor::new(archive_bytes);
    let mut archive =
        ZipArchive::new(cursor).map_err(|err| CloudBlobError::Archive(err.to_string()))?;

    if archive.is_empty() {
        return Err(CloudBlobError::EmptyArchive);
    }
    if archive.len() > options.max_file_count {
        return Err(CloudBlobError::FileCountLimitExceeded {
            count: archive.len(),
            limit: options.max_file_count,
        });
    }

    let staging_dir = create_unique_sibling_dir(&parent, &format!("{leaf}.staging"))?;
    let extract_result =
        extract_archive_into_staging(&mut archive, &staging_dir, options, &mut check_name);

    let (extracted_files, total_uncompressed_bytes) = match extract_result {
        Ok((files, _)) if files.is_empty() => {
            let _ = fs::remove_dir_all(&staging_dir);
            return Err(CloudBlobError::EmptyArchive);
        }
        Ok(stats) => stats,
        Err(err) => {
            let _ = fs::remove_dir_all(&staging_dir);
            return Err(err);
        }
    };

    atomic_swap_directory(&staging_dir, target_folder)?;

    Ok(UnpackReport {
        target_dir: target_folder.to_path_buf(),
        sha256: actual_sha256,
        extracted_files,
        total_uncompressed_bytes,
    })
}

fn extract_archive_into_staging<R, F>(
    archive: &mut ZipArchive<R>,
    staging_dir: &Path,
    options: &UnpackOptions,
    check_name: &mut F,
) -> CloudBlobResult<(Vec<String>, u64)>
where
    R: Read + std::io::Seek,
    F: FnMut(&str) -> bool,
{
    let mut seen_files = HashSet::new();
    let mut seen_dirs = HashSet::new();
    let mut extracted_files = Vec::new();
    let mut total_uncompressed: u64 = 0;

    for idx in 0..archive.len() {
        let mut entry = archive
            .by_index(idx)
            .map_err(|err| CloudBlobError::Archive(err.to_string()))?;
        let raw_name = entry
            .name()
            .map_err(|err| CloudBlobError::Archive(err.to_string()))?
            .into_owned();

        // Reject symlinks unconditionally, even before checking name filters.
        if is_zip_entry_symlink(&entry) {
            return Err(CloudBlobError::SymlinkRejected(raw_name));
        }

        let is_dir = entry.is_dir() || raw_name.ends_with('/');
        // Validate every path unconditionally so zip-slip entries always fail closed.
        let normalized = validate_archive_entry_path(&raw_name, is_dir)?;
        if !register_unique_archive_path(&normalized, is_dir, &mut seen_files, &mut seen_dirs) {
            return Err(CloudBlobError::DuplicatePath(raw_name));
        }

        if is_dir {
            if check_name(&raw_name) {
                fs::create_dir_all(staging_dir.join(&normalized))?;
            }
            continue;
        }

        if !check_name(&raw_name) {
            continue;
        }

        let remaining_total = options
            .max_total_uncompressed_size
            .saturating_sub(total_uncompressed);
        let entry_cap = options.max_file_size.min(remaining_total);

        let out_path = staging_dir.join(&normalized);
        if let Some(parent_dir) = out_path.parent() {
            fs::create_dir_all(parent_dir)?;
        }

        let mut out_file = File::options()
            .write(true)
            .create_new(true)
            .open(&out_path)?;

        let mut entry_written: u64 = 0;
        let mut buf = [0u8; 32 * 1024];
        loop {
            let n = entry
                .read(&mut buf)
                .map_err(|err| CloudBlobError::Archive(err.to_string()))?;
            if n == 0 {
                break;
            }
            entry_written = entry_written.checked_add(n as u64).ok_or(
                CloudBlobError::TotalSizeLimitExceeded {
                    limit: options.max_total_uncompressed_size,
                },
            )?;
            if entry_written > entry_cap {
                if entry_written > options.max_file_size {
                    return Err(CloudBlobError::FileSizeLimitExceeded {
                        path: normalized,
                        limit: options.max_file_size,
                    });
                }
                return Err(CloudBlobError::TotalSizeLimitExceeded {
                    limit: options.max_total_uncompressed_size,
                });
            }
            out_file.write_all(&buf[..n])?;
        }
        out_file.flush()?;

        total_uncompressed += entry_written;
        extracted_files.push(normalized);
    }

    Ok((extracted_files, total_uncompressed))
}

/// Verify and atomically unpack a cloud blob ZIP archive already located on disk at `archive_path`.
///
/// Mirrors `Storage::CloudBlob::UnpackBlob` in `storage_cloud_blob.cpp`.
pub fn unpack_blob<F>(
    archive_path: &Path,
    target_folder: &Path,
    options: &UnpackOptions,
    check_name: F,
) -> CloudBlobResult<UnpackReport>
where
    F: FnMut(&str) -> bool,
{
    let meta = fs::metadata(archive_path)?;
    if meta.len() == 0 {
        return Err(CloudBlobError::EmptyArchive);
    }
    if meta.len() > options.max_archive_size {
        return Err(CloudBlobError::ArchiveTooLarge {
            size: meta.len(),
            limit: options.max_archive_size,
        });
    }

    // Read with an explicit upper bound so a concurrently growing file cannot bypass the cap.
    let file = File::open(archive_path)?;
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(options.max_archive_size.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > options.max_archive_size {
        return Err(CloudBlobError::ArchiveTooLarge {
            size: bytes.len() as u64,
            limit: options.max_archive_size,
        });
    }

    unpack_blob_bytes(&bytes, target_folder, options, check_name)
}

/// Synchronous transport abstraction for fetching a cloud blob's raw archive bytes into a staging
/// writer.
pub trait CloudBlobSource: Send + Sync {
    /// Stream archive bytes for `location` into `writer`, calling `on_progress` after chunks.
    ///
    /// Returning `false` from `on_progress` signals cancellation.
    fn download_to(
        &self,
        location: &CloudBlobLocation,
        writer: &mut dyn Write,
        on_progress: &mut dyn FnMut(BlobProgress) -> bool,
    ) -> CloudBlobResult<u64>;
}

/// Asynchronous future returned by [`AsyncCloudBlobSource::download_chunk`].
pub type BlobChunkFuture<'a> = Pin<Box<dyn Future<Output = CloudBlobResult<Vec<u8>>> + Send + 'a>>;

/// Asynchronous chunked transport abstraction (e.g., MTProto dedicated loader or fake RPC).
pub trait AsyncCloudBlobSource: Send + Sync {
    /// Fetch up to `limit` bytes starting at `offset` for `location`.
    ///
    /// Returning an empty `Vec` signals end-of-stream.
    fn download_chunk(
        &self,
        location: &CloudBlobLocation,
        offset: u64,
        limit: usize,
    ) -> BlobChunkFuture<'_>;
}

/// Minimal dependency-free HTTP/1.1 `GET` cloud blob source for plain HTTP endpoints (including
/// local test servers on `127.0.0.1`).
#[derive(Debug, Clone)]
pub struct HttpCloudBlobSource {
    /// Base URL in the form `http://host:port` (optional path prefix).
    base_url: String,
    /// Socket read/write timeout.
    timeout: Duration,
}

impl HttpCloudBlobSource {
    /// Create a new HTTP cloud blob source targeting `base_url` (e.g. `"http://127.0.0.1:8080"`).
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            timeout: Duration::from_secs(15),
        }
    }

    /// Override the socket read/write timeout.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn parse_host_and_prefix(&self) -> CloudBlobResult<(String, String)> {
        let stripped = self.base_url.strip_prefix("http://").ok_or_else(|| {
            CloudBlobError::DownloadFailed("only http:// URLs are supported".into())
        })?;
        let (host_port, prefix) = match stripped.find('/') {
            Some(idx) => (&stripped[..idx], stripped[idx..].trim_end_matches('/')),
            None => (stripped, ""),
        };
        if host_port.is_empty() {
            return Err(CloudBlobError::DownloadFailed("empty HTTP host".into()));
        }
        Ok((host_port.to_string(), prefix.to_string()))
    }
}

impl CloudBlobSource for HttpCloudBlobSource {
    fn download_to(
        &self,
        location: &CloudBlobLocation,
        writer: &mut dyn Write,
        on_progress: &mut dyn FnMut(BlobProgress) -> bool,
    ) -> CloudBlobResult<u64> {
        let (host_port, prefix) = self.parse_host_and_prefix()?;
        let path = format!("{prefix}/{}/{}", location.username, location.post_id);

        let mut stream = TcpStream::connect(&host_port).map_err(|err| {
            CloudBlobError::DownloadFailed(format!("connect to {host_port} failed: {err}"))
        })?;
        let _ = stream.set_read_timeout(Some(self.timeout));
        let _ = stream.set_write_timeout(Some(self.timeout));

        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\nAccept-Encoding: identity\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).map_err(|err| {
            CloudBlobError::DownloadFailed(format!("failed sending HTTP request: {err}"))
        })?;
        stream.flush()?;

        let mut reader = BufReader::new(stream);
        let mut status_line = String::new();
        reader.read_line(&mut status_line).map_err(|err| {
            CloudBlobError::DownloadFailed(format!("failed reading HTTP status: {err}"))
        })?;
        let status_code = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or_else(|| {
                CloudBlobError::DownloadFailed(format!("malformed HTTP status line: {status_line}"))
            })?;
        if status_code != 200 {
            return Err(CloudBlobError::DownloadFailed(format!(
                "HTTP status {status_code} for {path}"
            )));
        }

        let mut content_length: Option<u64> = None;
        loop {
            let mut header_line = String::new();
            let n = reader.read_line(&mut header_line)?;
            if n == 0 || header_line == "\r\n" || header_line == "\n" {
                break;
            }
            if let Some((name, value)) = header_line.split_once(':')
                && name.trim().eq_ignore_ascii_case("content-length")
            {
                content_length = value.trim().parse::<u64>().ok();
            }
        }

        let declared_size = content_length.unwrap_or(0);
        let mut total_written: u64 = 0;
        let mut buf = [0u8; 32 * 1024];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            writer.write_all(&buf[..n])?;
            total_written += n as u64;
            if !on_progress(BlobProgress {
                already: total_written,
                size: declared_size.max(total_written),
            }) {
                return Err(CloudBlobError::Cancelled);
            }
        }

        if let Some(expected_len) = content_length
            && total_written != expected_len
        {
            return Err(CloudBlobError::DownloadFailed(format!(
                "truncated HTTP body: expected {expected_len} bytes, got {total_written}"
            )));
        }

        Ok(total_written)
    }
}

/// Bounded writer that computes a streaming SHA-256 while writing to a staging file and enforces
/// `max_archive_size` in real time.
struct BoundedHashingWriter {
    file: File,
    hasher: Sha256Hasher,
    written: u64,
    limit: u64,
}

impl BoundedHashingWriter {
    fn new(file: File, limit: u64) -> Self {
        Self {
            file,
            hasher: Sha256Hasher::new(),
            written: 0,
            limit,
        }
    }

    fn finish(mut self) -> CloudBlobResult<(u64, [u8; 32])> {
        self.file.flush()?;
        Ok((self.written, self.hasher.finalize()))
    }
}

impl Write for BoundedHashingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let next = self.written.saturating_add(buf.len() as u64);
        if next > self.limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                format!("cloud blob download exceeded limit of {} bytes", self.limit),
            ));
        }
        self.file.write_all(buf)?;
        self.hasher.update(buf);
        self.written = next;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// Staged downloader + verifier + atomic unpacker matching `Storage::CloudBlob::BlobLoader`.
///
/// Downloads archive bytes to a sibling temporary `.part` file, verifies the SHA-256 digest before
/// unpacking, extracts into a sibling staging directory, and atomically swaps into `target_folder`.
pub struct CloudBlobLoader {
    id: i32,
    location: CloudBlobLocation,
    target_folder: PathBuf,
    declared_size: u64,
    options: UnpackOptions,
    state_tx: watch::Sender<BlobState>,
    state_rx: watch::Receiver<BlobState>,
}

impl CloudBlobLoader {
    /// Create a new loader for blob `id` targeting `target_folder`.
    #[must_use]
    pub fn new(
        id: i32,
        location: CloudBlobLocation,
        target_folder: impl Into<PathBuf>,
        declared_size: u64,
        options: UnpackOptions,
    ) -> Self {
        let initial = BlobState::Loading(BlobProgress {
            already: 0,
            size: declared_size,
        });
        let (state_tx, state_rx) = watch::channel(initial);
        Self {
            id,
            location,
            target_folder: target_folder.into(),
            declared_size,
            options,
            state_tx,
            state_rx,
        }
    }

    /// Logical blob identifier (`BlobLoader::id()`).
    #[must_use]
    pub const fn id(&self) -> i32 {
        self.id
    }

    /// Target directory where the unpacked blob will live.
    #[must_use]
    pub fn folder(&self) -> &Path {
        &self.target_folder
    }

    /// Observe state transitions (`BlobLoader::state()`).
    #[must_use]
    pub fn state(&self) -> watch::Receiver<BlobState> {
        self.state_rx.clone()
    }

    /// Current snapshot of [`BlobState`].
    #[must_use]
    pub fn current_state(&self) -> BlobState {
        self.state_rx.borrow().clone()
    }

    /// Execute the staged download, SHA-256 verification, safe extraction, and atomic directory
    /// swap using a synchronous [`CloudBlobSource`].
    pub fn download_and_unpack_sync<S, F>(
        &self,
        source: &S,
        check_name: F,
    ) -> CloudBlobResult<UnpackReport>
    where
        S: CloudBlobSource + ?Sized,
        F: FnMut(&str) -> bool,
    {
        if self.declared_size > self.options.max_archive_size {
            let err = CloudBlobError::ArchiveTooLarge {
                size: self.declared_size,
                limit: self.options.max_archive_size,
            };
            self.set_failed(&err);
            return Err(err);
        }

        let (parent, leaf) = match validate_target_folder(&self.target_folder) {
            Ok(v) => v,
            Err(err) => {
                self.set_failed(&err);
                return Err(err);
            }
        };

        let (part_path, part_file) =
            match create_unique_sibling_file(&parent, &format!("{leaf}.download")) {
                Ok(v) => v,
                Err(err) => {
                    self.set_failed(&err);
                    return Err(err);
                }
            };

        let mut writer = BoundedHashingWriter::new(part_file, self.options.max_archive_size);
        let mut on_progress = |progress: BlobProgress| {
            let _ = self.state_tx.send(BlobState::Loading(progress));
            true
        };

        let download_res = source.download_to(&self.location, &mut writer, &mut on_progress);
        if let Err(err) = download_res {
            drop(writer);
            let _ = fs::remove_file(&part_path);
            let mapped = if matches!(
                &err,
                CloudBlobError::Io(io_err) if io_err.kind() == std::io::ErrorKind::FileTooLarge
            ) {
                CloudBlobError::ArchiveTooLarge {
                    size: self.options.max_archive_size.saturating_add(1),
                    limit: self.options.max_archive_size,
                }
            } else {
                err
            };
            self.set_failed(&mapped);
            return Err(mapped);
        }

        let (downloaded_len, actual_sha256) = match writer.finish() {
            Ok(v) => v,
            Err(err) => {
                let _ = fs::remove_file(&part_path);
                self.set_failed(&err);
                return Err(err);
            }
        };

        let result = self.verify_and_unpack_staged_file(
            &part_path,
            downloaded_len,
            actual_sha256,
            check_name,
        );
        let _ = fs::remove_file(&part_path);

        match result {
            Ok(report) => {
                let _ = self.state_tx.send(BlobState::Ready);
                Ok(report)
            }
            Err(err) => {
                self.set_failed(&err);
                Err(err)
            }
        }
    }

    /// Execute the staged download, SHA-256 verification, safe extraction, and atomic directory
    /// swap using an asynchronous [`AsyncCloudBlobSource`] (e.g. fake RPC / MTProto loader).
    pub async fn download_and_unpack_async<S, F>(
        &self,
        source: Arc<S>,
        chunk_size: usize,
        check_name: F,
    ) -> CloudBlobResult<UnpackReport>
    where
        S: AsyncCloudBlobSource + ?Sized,
        F: FnMut(&str) -> bool,
    {
        if self.declared_size > self.options.max_archive_size {
            let err = CloudBlobError::ArchiveTooLarge {
                size: self.declared_size,
                limit: self.options.max_archive_size,
            };
            self.set_failed(&err);
            return Err(err);
        }

        let chunk_size = chunk_size.max(1024);
        let (parent, leaf) = match validate_target_folder(&self.target_folder) {
            Ok(v) => v,
            Err(err) => {
                self.set_failed(&err);
                return Err(err);
            }
        };

        let (part_path, part_file) =
            match create_unique_sibling_file(&parent, &format!("{leaf}.download")) {
                Ok(v) => v,
                Err(err) => {
                    self.set_failed(&err);
                    return Err(err);
                }
            };

        let mut writer = BoundedHashingWriter::new(part_file, self.options.max_archive_size);
        let mut offset: u64 = 0;

        loop {
            let chunk = match source
                .download_chunk(&self.location, offset, chunk_size)
                .await
            {
                Ok(bytes) => bytes,
                Err(err) => {
                    drop(writer);
                    let _ = fs::remove_file(&part_path);
                    self.set_failed(&err);
                    return Err(err);
                }
            };
            if chunk.is_empty() {
                break;
            }
            if let Err(io_err) = writer.write_all(&chunk) {
                drop(writer);
                let _ = fs::remove_file(&part_path);
                let err = if io_err.kind() == std::io::ErrorKind::FileTooLarge {
                    CloudBlobError::ArchiveTooLarge {
                        size: offset.saturating_add(chunk.len() as u64),
                        limit: self.options.max_archive_size,
                    }
                } else {
                    CloudBlobError::Io(io_err)
                };
                self.set_failed(&err);
                return Err(err);
            }
            offset += chunk.len() as u64;
            let _ = self.state_tx.send(BlobState::Loading(BlobProgress {
                already: offset,
                size: self.declared_size.max(offset),
            }));
        }

        let (downloaded_len, actual_sha256) = match writer.finish() {
            Ok(v) => v,
            Err(err) => {
                let _ = fs::remove_file(&part_path);
                self.set_failed(&err);
                return Err(err);
            }
        };

        let result = self.verify_and_unpack_staged_file(
            &part_path,
            downloaded_len,
            actual_sha256,
            check_name,
        );
        let _ = fs::remove_file(&part_path);

        match result {
            Ok(report) => {
                let _ = self.state_tx.send(BlobState::Ready);
                Ok(report)
            }
            Err(err) => {
                self.set_failed(&err);
                Err(err)
            }
        }
    }

    fn verify_and_unpack_staged_file<F>(
        &self,
        part_path: &Path,
        downloaded_len: u64,
        actual_sha256: [u8; 32],
        check_name: F,
    ) -> CloudBlobResult<UnpackReport>
    where
        F: FnMut(&str) -> bool,
    {
        if downloaded_len == 0 {
            return Err(CloudBlobError::EmptyArchive);
        }
        if let Some(expected) = &self.options.expected_sha256
            && !const_time_eq(&actual_sha256, expected)
        {
            return Err(CloudBlobError::Sha256Mismatch {
                expected: sha256_hex(expected),
                actual: sha256_hex(&actual_sha256),
            });
        }
        unpack_blob(part_path, &self.target_folder, &self.options, check_name)
    }

    fn set_failed(&self, err: &CloudBlobError) {
        let _ = self.state_tx.send(BlobState::Failed(err.to_string()));
    }
}
