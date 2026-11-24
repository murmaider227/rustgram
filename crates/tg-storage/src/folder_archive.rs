//! Deterministic folder archival and safe extraction with size/count caps and progress callbacks.
//!
//! Port of `Telegram/SourceFiles/storage/storage_folder_archive.h` and
//! `storage_folder_archive.cpp`, extended with deterministic entry ordering/timestamps,
//! maximum file-count and uncompressed/compressed byte budgets, and safe symmetric extraction.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tg_crypto::random_u64;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime as ZipDateTime, ZipArchive, ZipWriter};

use crate::cloud_blob::{
    is_zip_entry_symlink, register_unique_archive_path, validate_archive_entry_path,
};
use crate::error::StorageError;

/// Default read chunk size when streaming files into an archive (`kArchiveChunkSize` = 1 MiB).
pub const ARCHIVE_CHUNK_SIZE: usize = 1024 * 1024;

/// Streaming chunk size used during archive writes for responsive progress/limit checks (64 KiB).
const STREAM_BUFFER_SIZE: usize = 64 * 1024;

/// Per-entry ZIP metadata overhead estimate in bytes (`kArchiveEntryOverhead` = 128).
pub const ARCHIVE_ENTRY_OVERHEAD: u64 = 128;

/// Age threshold after which temporary archive files are considered stale
/// (`kStaleArchiveTimeout` = 24 hours).
pub const STALE_ARCHIVE_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// Default maximum number of file/directory entries permitted in a folder archive.
pub const DEFAULT_MAX_ARCHIVE_FILES: usize = 10_000;

/// Default maximum total uncompressed bytes (2 GiB).
pub const DEFAULT_MAX_TOTAL_UNCOMPRESSED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Default maximum output archive file size in bytes (2 GiB).
pub const DEFAULT_MAX_ARCHIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Extensions stored uncompressed (`CompressionMethod::Stored`) because they are already
/// compressed, matching `kStoredExtensions` in `AlreadyCompressedName`.
pub const STORED_EXTENSIONS: &[&str] = &[
    "3gp", "7z", "aac", "apk", "avi", "avif", "bz2", "cab", "docx", "epub", "flac", "flv", "gif",
    "gz", "heic", "heif", "jar", "jpeg", "jpg", "jxl", "m4a", "m4v", "mkv", "mov", "mp3", "mp4",
    "odp", "ods", "odt", "oga", "ogg", "opus", "png", "pptx", "rar", "tgz", "webm", "webp", "wma",
    "wmv", "xlsx", "xz", "zip", "zst",
];

/// Errors produced by folder archival and extraction operations.
#[derive(Debug, thiserror::Error)]
pub enum FolderArchiveError {
    /// Underlying filesystem I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// ZIP encoding or decoding error.
    #[error("archive error: {0}")]
    Archive(String),

    /// Source folder or file list yielded no archivable entries.
    #[error("no archivable entries found")]
    NoEntries,

    /// Entry count exceeded [`ArchiveLimits::max_files`].
    #[error("file count {count} exceeds limit {limit}")]
    FileCountLimitExceeded {
        /// Observed entry count.
        count: usize,
        /// Configured limit.
        limit: usize,
    },

    /// Cumulative uncompressed size exceeded [`ArchiveLimits::max_total_uncompressed_bytes`].
    #[error("uncompressed byte total {bytes} exceeds budget {limit}")]
    ByteBudgetExceeded {
        /// Observed uncompressed bytes.
        bytes: u64,
        /// Configured budget.
        limit: u64,
    },

    /// Single file size exceeded [`ArchiveLimits::max_single_file_bytes`].
    #[error("file '{path}' size {size} exceeds single-file limit {limit}")]
    SingleFileTooLarge {
        /// Relative archive entry path.
        path: String,
        /// Observed size in bytes.
        size: u64,
        /// Configured single-file limit.
        limit: u64,
    },

    /// Output archive size exceeded [`ArchiveLimits::max_archive_bytes`].
    #[error("archive size {size} exceeds limit {limit}")]
    ArchiveTooLarge {
        /// Observed archive size in bytes.
        size: u64,
        /// Configured archive size limit.
        limit: u64,
    },

    /// Archive entry failed safety validation (zip-slip, absolute path, etc.).
    #[error("unsafe archive path: {0}")]
    UnsafePath(String),

    /// Archive contained a symlink entry.
    #[error("symlink rejected: {0}")]
    SymlinkRejected(String),

    /// Archive contained duplicate or conflicting case-insensitive paths.
    #[error("duplicate archive path: {0}")]
    DuplicatePath(String),

    /// Operation was cancelled via progress callback returning `false`.
    #[error("archive operation cancelled")]
    Cancelled,
}

impl From<FolderArchiveError> for StorageError {
    fn from(err: FolderArchiveError) -> Self {
        match err {
            FolderArchiveError::Io(io) => Self::Io(io),
            other => Self::CorruptData(other.to_string()),
        }
    }
}

/// Result alias for folder archive operations.
pub type FolderArchiveResult<T> = std::result::Result<T, FolderArchiveError>;

/// Resource limits and behavior flags governing archival and extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveLimits {
    /// Maximum number of entries (files + directories) permitted.
    pub max_files: usize,
    /// Maximum total uncompressed bytes permitted across all files.
    pub max_total_uncompressed_bytes: u64,
    /// Maximum compressed output `.zip` size permitted in bytes.
    pub max_archive_bytes: u64,
    /// Maximum uncompressed size of any individual file.
    pub max_single_file_bytes: u64,
    /// Whether hidden files/directories (leading `.`) are included when walking folders.
    pub include_hidden: bool,
    /// When `true`, normalize entry timestamps to DOS epoch (`1980-01-01 00:00:00`) and fixed
    /// permissions so archiving the same tree produces byte-for-byte deterministic ZIP output.
    pub deterministic_timestamps: bool,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_files: DEFAULT_MAX_ARCHIVE_FILES,
            max_total_uncompressed_bytes: DEFAULT_MAX_TOTAL_UNCOMPRESSED_BYTES,
            max_archive_bytes: DEFAULT_MAX_ARCHIVE_BYTES,
            max_single_file_bytes: DEFAULT_MAX_TOTAL_UNCOMPRESSED_BYTES,
            include_hidden: true,
            deterministic_timestamps: true,
        }
    }
}

/// Single entry gathered for inclusion in an archive (`Storage::ArchiveEntry`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntry {
    /// Source path on disk.
    pub absolute: PathBuf,
    /// Normalized forward-slash archive entry name (directories end with `'/'`).
    pub name: String,
    /// Source modification timestamp (if available).
    pub modified: Option<SystemTime>,
    /// Uncompressed file size in bytes (`0` for directories).
    pub size: u64,
    /// Whether this entry represents a directory.
    pub directory: bool,
}

/// Collection of entries and their total uncompressed byte size (`Storage::ArchiveEntries`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArchiveEntries {
    /// Deterministically sorted list of archive entries.
    pub list: Vec<ArchiveEntry>,
    /// Sum of `entry.size` for all regular files in `list`.
    pub total: u64,
}

/// Prepared archival job metadata (`Ui::PreparedFileArchive`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedFileArchive {
    /// Display filename for the resulting `.zip` (e.g. `"MyFolder.zip"` or `"Archive.zip"`).
    pub display_name: String,
    /// Folder to archive recursively (if folder mode).
    pub folder: Option<PathBuf>,
    /// Root prefix inside the archive (e.g. `"MyFolder"`).
    pub root: String,
    /// Individual file paths to archive (if files mode).
    pub paths: Vec<PathBuf>,
    /// Optional explicit names corresponding to `paths`.
    pub names: Vec<String>,
}

/// Progress snapshot passed to archive write/extract callbacks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArchiveProgress {
    /// Bytes processed so far.
    pub processed_bytes: u64,
    /// Expected total uncompressed bytes.
    pub total_bytes: u64,
    /// Completion fraction clamped to `[0.0, 1.0]`.
    pub fraction: f64,
}

impl ArchiveProgress {
    fn new(processed_bytes: u64, total_bytes: u64) -> Self {
        let fraction = if total_bytes == 0 {
            0.0
        } else {
            (processed_bytes as f64 / total_bytes as f64).clamp(0.0, 1.0)
        };
        Self {
            processed_bytes,
            total_bytes,
            fraction,
        }
    }
}

/// Terminal status of an archive write operation (`Storage::ArchiveWriteResult::Status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveWriteStatus {
    /// Archive was written completely and verified within limits.
    Done,
    /// Output archive exceeded `max_archive_bytes`.
    TooLarge,
    /// An I/O or ZIP error prevented completion.
    Failed,
    /// Caller cancelled via the progress callback.
    Cancelled,
}

/// Outcome of writing an archive (`Storage::ArchiveWriteResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveWriteResult {
    /// Completion status.
    pub status: ArchiveWriteStatus,
    /// Output archive path when `status == ArchiveWriteStatus::Done`.
    pub path: Option<PathBuf>,
    /// Final written archive size in bytes (or size reached before abort).
    pub size: u64,
    /// Number of entries written.
    pub entry_count: usize,
}

/// Outcome of extracting a folder archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveExtractResult {
    /// Destination directory where files were extracted.
    pub dest_dir: PathBuf,
    /// Number of regular files extracted.
    pub files_extracted: usize,
    /// Number of directory entries created.
    pub dirs_created: usize,
    /// Total uncompressed bytes written across all files.
    pub total_bytes: u64,
}

/// Returns `true` if `name` has an extension that is already compressed and should be stored
/// without deflate (`AlreadyCompressedName` in `storage_folder_archive.cpp`).
#[must_use]
pub fn is_already_compressed_name(name: &str) -> bool {
    let base = name.rsplit('/').next().unwrap_or(name);
    let Some((stem, ext)) = base.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() || ext.is_empty() {
        return false;
    }
    let lower = ext.to_ascii_lowercase();
    STORED_EXTENSIONS.contains(&lower.as_str())
}

/// If `paths` contains exactly one path and it points to an existing directory, return it
/// (`SingleFolderPath` in `storage_folder_archive.cpp`).
#[must_use]
pub fn single_folder_path(paths: &[PathBuf]) -> Option<PathBuf> {
    if paths.len() != 1 {
        return None;
    }
    let candidate = &paths[0];
    candidate.is_dir().then(|| candidate.clone())
}

/// Return direct files in `folder` (sorted by name) followed by direct files in each immediate
/// subdirectory (sorted by name), mirroring `FolderFilesForSending`.
pub fn folder_files_for_sending(folder: &Path) -> FolderArchiveResult<Vec<PathBuf>> {
    let mut root_files = Vec::new();
    let mut subdirs = Vec::new();

    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_file() {
            root_files.push(path);
        } else if meta.is_dir() {
            subdirs.push(path);
        }
    }
    root_files.sort();
    subdirs.sort();

    let mut result = root_files;
    for subdir in subdirs {
        let mut inner_files = Vec::new();
        for entry in fs::read_dir(&subdir)? {
            let entry = entry?;
            let path = entry.path();
            let meta = fs::symlink_metadata(&path)?;
            if !meta.file_type().is_symlink() && meta.is_file() {
                inner_files.push(path);
            }
        }
        inner_files.sort();
        result.extend(inner_files);
    }
    Ok(result)
}

fn archive_root_name(folder: &Path) -> String {
    folder
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Archive")
        .to_string()
}

fn common_parent_name(paths: &[PathBuf]) -> Option<String> {
    let mut parent: Option<&Path> = None;
    for path in paths {
        let p = path.parent()?;
        if p.as_os_str().is_empty() {
            return None;
        }
        match parent {
            None => parent = Some(p),
            Some(existing) if existing == p => {}
            Some(_) => return None,
        }
    }
    parent
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

/// Prepare a folder archival job (`PrepareFolderArchive` in `storage_folder_archive.cpp`).
#[must_use]
pub fn prepare_folder_archive(folder: &Path) -> PreparedFileArchive {
    let root = archive_root_name(folder);
    PreparedFileArchive {
        display_name: format!("{root}.zip"),
        folder: Some(folder.to_path_buf()),
        root,
        paths: Vec::new(),
        names: Vec::new(),
    }
}

/// Prepare a multi-file archival job (`PrepareFilesArchive` in `storage_folder_archive.cpp`).
#[must_use]
pub fn prepare_files_archive(paths: &[PathBuf], names: &[String]) -> PreparedFileArchive {
    let base = common_parent_name(paths).unwrap_or_else(|| "Archive".to_string());
    PreparedFileArchive {
        display_name: format!("{base}.zip"),
        folder: None,
        root: String::new(),
        paths: paths.to_vec(),
        names: names.to_vec(),
    }
}

/// Deduplicate archive entry names case-insensitively (`"a.txt"`, `"a (2).txt"`, `"a (3).txt"`),
/// mirroring `DedupedName` in `storage_folder_archive.cpp`.
fn deduped_name(counts: &mut HashMap<String, usize>, name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let count = counts.entry(lower.clone()).or_insert(0);
    *count += 1;
    if *count == 1 {
        return name.to_string();
    }

    let dot = name.rfind('.');
    loop {
        let current_count = *counts.get(&lower).unwrap_or(&2);
        let suffix = format!(" ({current_count})");
        let candidate = match dot {
            Some(pos) if pos > 0 => format!("{}{}{}", &name[..pos], suffix, &name[pos..]),
            _ => format!("{name}{suffix}"),
        };
        let candidate_lower = candidate.to_ascii_lowercase();
        let entry = counts.entry(candidate_lower).or_insert(0);
        *entry += 1;
        if *entry == 1 {
            return candidate;
        }
        *counts.entry(lower.clone()).or_insert(0) += 1;
    }
}

/// Estimate the final ZIP size (`ArchiveSizeEstimate` in `storage_folder_archive.cpp`).
#[must_use]
pub fn archive_size_estimate(entries: &ArchiveEntries) -> u64 {
    let mut result = ARCHIVE_ENTRY_OVERHEAD;
    for entry in &entries.list {
        result = result
            .saturating_add(entry.size)
            .saturating_add(ARCHIVE_ENTRY_OVERHEAD);
    }
    result
}

/// Gather and deterministically sort entries for `job`, enforcing `limits`.
///
/// Mirrors `GatherArchiveEntries` / `WalkFolder` / `GatherFiles` in `storage_folder_archive.cpp`,
/// adding deterministic lexicographic ordering and explicit count/byte budget enforcement.
pub fn gather_archive_entries(
    job: &PreparedFileArchive,
    limits: &ArchiveLimits,
) -> FolderArchiveResult<ArchiveEntries> {
    if let Some(folder) = &job.folder {
        walk_folder(folder, &job.root, limits)
    } else {
        gather_files(&job.paths, &job.names, limits)
    }
}

fn walk_folder(
    folder: &Path,
    root: &str,
    limits: &ArchiveLimits,
) -> FolderArchiveResult<ArchiveEntries> {
    let mut entries = ArchiveEntries::default();
    let mut stack = vec![folder.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let mut children: Vec<PathBuf> = fs::read_dir(&dir)?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|e| e.path())
            .collect();
        children.sort();

        for path in children {
            let meta = fs::symlink_metadata(&path)?;
            if meta.file_type().is_symlink() {
                continue;
            }
            if !limits.include_hidden
                && path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|name| name.starts_with('.'))
            {
                continue;
            }

            let rel = path
                .strip_prefix(folder)
                .map_err(|_| FolderArchiveError::UnsafePath(path.display().to_string()))?;
            let rel_str = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if rel_str.is_empty() {
                continue;
            }

            let archive_name = if root.is_empty() {
                rel_str
            } else {
                format!("{root}/{rel_str}")
            };

            if meta.is_dir() {
                let next_count = entries.list.len() + 1;
                if next_count > limits.max_files {
                    return Err(FolderArchiveError::FileCountLimitExceeded {
                        count: next_count,
                        limit: limits.max_files,
                    });
                }
                entries.list.push(ArchiveEntry {
                    absolute: path.clone(),
                    name: format!("{archive_name}/"),
                    modified: meta.modified().ok(),
                    size: 0,
                    directory: true,
                });
                stack.push(path);
            } else if meta.is_file() {
                // Verify readability early (matching `info.isReadable()` in WalkFolder).
                let _ = File::open(&path)?;

                let next_count = entries.list.len() + 1;
                if next_count > limits.max_files {
                    return Err(FolderArchiveError::FileCountLimitExceeded {
                        count: next_count,
                        limit: limits.max_files,
                    });
                }

                let size = meta.len();
                if size > limits.max_single_file_bytes {
                    return Err(FolderArchiveError::SingleFileTooLarge {
                        path: archive_name,
                        size,
                        limit: limits.max_single_file_bytes,
                    });
                }
                let next_total = entries.total.checked_add(size).ok_or(
                    FolderArchiveError::ByteBudgetExceeded {
                        bytes: u64::MAX,
                        limit: limits.max_total_uncompressed_bytes,
                    },
                )?;
                if next_total > limits.max_total_uncompressed_bytes {
                    return Err(FolderArchiveError::ByteBudgetExceeded {
                        bytes: next_total,
                        limit: limits.max_total_uncompressed_bytes,
                    });
                }
                entries.total = next_total;
                entries.list.push(ArchiveEntry {
                    absolute: path,
                    name: archive_name,
                    modified: meta.modified().ok(),
                    size,
                    directory: false,
                });
            }
        }
    }

    // Sort deterministically by normalized archive path.
    entries.list.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

fn gather_files(
    paths: &[PathBuf],
    names: &[String],
    limits: &ArchiveLimits,
) -> FolderArchiveResult<ArchiveEntries> {
    let mut entries = ArchiveEntries::default();
    let mut counts = HashMap::new();

    for (idx, path) in paths.iter().enumerate() {
        let meta = fs::symlink_metadata(path)?;
        if meta.is_dir() || meta.file_type().is_symlink() {
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        let _ = File::open(path)?;

        let next_count = entries.list.len() + 1;
        if next_count > limits.max_files {
            return Err(FolderArchiveError::FileCountLimitExceeded {
                count: next_count,
                limit: limits.max_files,
            });
        }

        let size = meta.len();
        let raw_name = names
            .get(idx)
            .filter(|s| !s.is_empty())
            .cloned()
            .or_else(|| {
                path.file_name()
                    .and_then(|s| s.to_str())
                    .map(ToOwned::to_owned)
            })
            .ok_or_else(|| FolderArchiveError::UnsafePath(path.display().to_string()))?;

        if size > limits.max_single_file_bytes {
            return Err(FolderArchiveError::SingleFileTooLarge {
                path: raw_name,
                size,
                limit: limits.max_single_file_bytes,
            });
        }

        let next_total =
            entries
                .total
                .checked_add(size)
                .ok_or(FolderArchiveError::ByteBudgetExceeded {
                    bytes: u64::MAX,
                    limit: limits.max_total_uncompressed_bytes,
                })?;
        if next_total > limits.max_total_uncompressed_bytes {
            return Err(FolderArchiveError::ByteBudgetExceeded {
                bytes: next_total,
                limit: limits.max_total_uncompressed_bytes,
            });
        }

        let final_name = deduped_name(&mut counts, &raw_name);
        entries.total = next_total;
        entries.list.push(ArchiveEntry {
            absolute: path.clone(),
            name: final_name,
            modified: meta.modified().ok(),
            size,
            directory: false,
        });
    }

    if entries.list.is_empty() {
        return Err(FolderArchiveError::NoEntries);
    }

    entries.list.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

/// Convert `SystemTime` into a DOS [`ZipDateTime`] clamped to `[1980-01-01, 2107-12-31]`,
/// matching `FillEntryDate` in `storage_folder_archive.cpp`.
fn system_time_to_zip_datetime(time: Option<SystemTime>) -> ZipDateTime {
    let Some(time) = time else {
        return ZipDateTime::default();
    };
    let secs = match time.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs(),
        Err(_) => return ZipDateTime::default(),
    };
    let days = (secs / 86_400) as i64;
    let secs_of_day = (secs % 86_400) as u32;
    let hour = (secs_of_day / 3600) as u8;
    let minute = ((secs_of_day % 3600) / 60) as u8;
    let second = (secs_of_day % 60) as u8;

    // Howard Hinnant's civil_from_days algorithm (exact Gregorian conversion).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = (y + i64::from(month <= 2)).clamp(1980, 2107) as u16;

    ZipDateTime::from_date_and_time(year, month, day, hour, minute, second).unwrap_or_default()
}

struct TrackedArchiveFile {
    file: File,
    pos: u64,
    max_written: Arc<AtomicU64>,
}

impl TrackedArchiveFile {
    fn new(file: File, max_written: Arc<AtomicU64>) -> Self {
        Self {
            file,
            pos: 0,
            max_written,
        }
    }

    fn into_inner(self) -> File {
        self.file
    }
}

impl Write for TrackedArchiveFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.file.write(buf)?;
        self.pos = self.pos.saturating_add(n as u64);
        self.max_written.fetch_max(self.pos, Ordering::Relaxed);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl Seek for TrackedArchiveFile {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let new_pos = self.file.seek(pos)?;
        self.pos = new_pos;
        Ok(new_pos)
    }
}

/// Write `entries` deterministically into a ZIP archive at `output_path`, enforcing `limits` and
/// reporting progress via `progress` (return `false` to cancel).
///
/// The archive is written to a sibling temporary file first and atomically renamed into
/// `output_path` on success; on cancellation, size-limit overrun, or error, the temporary file is
/// deleted.
pub fn write_archive_to_path<F>(
    entries: &ArchiveEntries,
    output_path: &Path,
    limits: &ArchiveLimits,
    mut progress: F,
) -> FolderArchiveResult<ArchiveWriteResult>
where
    F: FnMut(ArchiveProgress) -> bool,
{
    if entries.list.len() > limits.max_files {
        return Err(FolderArchiveError::FileCountLimitExceeded {
            count: entries.list.len(),
            limit: limits.max_files,
        });
    }
    if entries.total > limits.max_total_uncompressed_bytes {
        return Err(FolderArchiveError::ByteBudgetExceeded {
            bytes: entries.total,
            limit: limits.max_total_uncompressed_bytes,
        });
    }

    let parent = output_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let temp_path = loop {
        let nonce = random_u64();
        let candidate = parent.join(format!(".archive-{nonce:016x}.tmp"));
        if !candidate.exists() {
            break candidate;
        }
    };

    let temp_file = File::options()
        .write(true)
        .read(true)
        .create_new(true)
        .open(&temp_path)?;

    let written_bytes = Arc::new(AtomicU64::new(0));
    let mut zip = ZipWriter::new(TrackedArchiveFile::new(
        temp_file,
        Arc::clone(&written_bytes),
    ));
    let mut processed: u64 = 0;
    let mut written_entries: usize = 0;
    let mut total_uncompressed_written: u64 = 0;
    let mut status = ArchiveWriteStatus::Done;
    let mut buf = vec![0u8; STREAM_BUFFER_SIZE];

    for entry in &entries.list {
        if !progress(ArchiveProgress::new(processed, entries.total)) {
            status = ArchiveWriteStatus::Cancelled;
            break;
        }

        let mtime = if limits.deterministic_timestamps {
            ZipDateTime::default()
        } else {
            system_time_to_zip_datetime(entry.modified)
        };

        if entry.directory {
            let options = SimpleFileOptions::default()
                .compression_method(CompressionMethod::Stored)
                .unix_permissions(0o755)
                .last_modified_time(mtime);
            if zip.add_directory(&entry.name, options).is_err() {
                status = ArchiveWriteStatus::Failed;
                break;
            }
            written_entries += 1;
            continue;
        }

        if !entry.absolute.exists() {
            processed = processed.saturating_add(entry.size);
            continue;
        }

        let stored = is_already_compressed_name(&entry.name);
        let options = SimpleFileOptions::default()
            .compression_method(if stored {
                CompressionMethod::Stored
            } else {
                CompressionMethod::Deflated
            })
            .unix_permissions(0o644)
            .last_modified_time(mtime)
            .large_file(entry.size >= u64::from(u32::MAX));

        if zip.start_file(&entry.name, options).is_err() {
            status = ArchiveWriteStatus::Failed;
            break;
        }

        let mut src = match File::open(&entry.absolute) {
            Ok(f) => f,
            Err(_) => {
                status = ArchiveWriteStatus::Failed;
                break;
            }
        };

        let mut file_bytes: u64 = 0;
        loop {
            let n = match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => {
                    status = ArchiveWriteStatus::Failed;
                    break;
                }
            };
            file_bytes += n as u64;
            total_uncompressed_written += n as u64;
            if file_bytes > limits.max_single_file_bytes
                || total_uncompressed_written > limits.max_total_uncompressed_bytes
            {
                status = ArchiveWriteStatus::TooLarge;
                break;
            }
            if zip.write_all(&buf[..n]).is_err() {
                status = ArchiveWriteStatus::Failed;
                break;
            }
            processed = processed.saturating_add(n as u64);
            if !progress(ArchiveProgress::new(processed, entries.total)) {
                status = ArchiveWriteStatus::Cancelled;
                break;
            }
            let current_pos = written_bytes.load(Ordering::Relaxed);
            if current_pos > limits.max_archive_bytes {
                status = ArchiveWriteStatus::TooLarge;
                break;
            }
        }

        if status != ArchiveWriteStatus::Done {
            break;
        }
        written_entries += 1;
    }

    if status != ArchiveWriteStatus::Done {
        drop(zip);
        let size = fs::metadata(&temp_path).map(|m| m.len()).unwrap_or(0);
        let _ = fs::remove_file(&temp_path);
        return Ok(ArchiveWriteResult {
            status,
            path: None,
            size,
            entry_count: written_entries,
        });
    }

    match zip.finish() {
        Ok(tracked) => {
            let mut file = tracked.into_inner();
            file.flush()?;
            let final_size = file.metadata()?.len();
            drop(file);
            if final_size > limits.max_archive_bytes {
                let _ = fs::remove_file(&temp_path);
                return Ok(ArchiveWriteResult {
                    status: ArchiveWriteStatus::TooLarge,
                    path: None,
                    size: final_size,
                    entry_count: written_entries,
                });
            }
            // Final 100% progress callback.
            if !progress(ArchiveProgress::new(entries.total, entries.total)) {
                let _ = fs::remove_file(&temp_path);
                return Ok(ArchiveWriteResult {
                    status: ArchiveWriteStatus::Cancelled,
                    path: None,
                    size: final_size,
                    entry_count: written_entries,
                });
            }
            fs::rename(&temp_path, output_path)?;
            Ok(ArchiveWriteResult {
                status: ArchiveWriteStatus::Done,
                path: Some(output_path.to_path_buf()),
                size: final_size,
                entry_count: written_entries,
            })
        }
        Err(_) => {
            let size = fs::metadata(&temp_path).map(|m| m.len()).unwrap_or(0);
            let _ = fs::remove_file(&temp_path);
            Ok(ArchiveWriteResult {
                status: ArchiveWriteStatus::Failed,
                path: None,
                size,
                entry_count: written_entries,
            })
        }
    }
}

/// Write `entries` into a uniquely named `.zip` inside `archive_dir` (`WriteArchive` in
/// `storage_folder_archive.cpp`).
pub fn write_archive<F>(
    entries: &ArchiveEntries,
    archive_dir: &Path,
    limits: &ArchiveLimits,
    progress: F,
) -> FolderArchiveResult<ArchiveWriteResult>
where
    F: FnMut(ArchiveProgress) -> bool,
{
    fs::create_dir_all(archive_dir)?;
    let output_path = loop {
        let nonce = random_u64();
        let candidate = archive_dir.join(format!("{nonce:016x}.zip"));
        if !candidate.exists() {
            break candidate;
        }
    };
    write_archive_to_path(entries, &output_path, limits, progress)
}

/// Convenience helper: gather all entries in `folder` and write them deterministically to
/// `output_path`.
pub fn archive_folder<F>(
    folder: &Path,
    output_path: &Path,
    limits: &ArchiveLimits,
    progress: F,
) -> FolderArchiveResult<ArchiveWriteResult>
where
    F: FnMut(ArchiveProgress) -> bool,
{
    let job = prepare_folder_archive(folder);
    let entries = gather_archive_entries(&job, limits)?;
    write_archive_to_path(&entries, output_path, limits, progress)
}

/// Safely extract a folder archive into `dest_dir`, enforcing path safety, symlink rejection,
/// entry count and uncompressed byte budgets, and progress/cancellation callbacks.
pub fn extract_folder_archive<F>(
    archive_path: &Path,
    dest_dir: &Path,
    limits: &ArchiveLimits,
    mut progress: F,
) -> FolderArchiveResult<ArchiveExtractResult>
where
    F: FnMut(ArchiveProgress) -> bool,
{
    let file = File::open(archive_path)?;
    let mut archive =
        ZipArchive::new(file).map_err(|err| FolderArchiveError::Archive(err.to_string()))?;

    if archive.len() > limits.max_files {
        return Err(FolderArchiveError::FileCountLimitExceeded {
            count: archive.len(),
            limit: limits.max_files,
        });
    }

    // Pre-scan declared total uncompressed size for progress reporting (still enforced strictly
    // against actual decompressed bytes below).
    let mut declared_total: u64 = 0;
    for idx in 0..archive.len() {
        let entry = archive
            .by_index(idx)
            .map_err(|err| FolderArchiveError::Archive(err.to_string()))?;
        declared_total = declared_total.saturating_add(entry.size());
    }

    fs::create_dir_all(dest_dir)?;

    let mut seen_files = HashSet::new();
    let mut seen_dirs = HashSet::new();
    let mut files_extracted: usize = 0;
    let mut dirs_created: usize = 0;
    let mut total_bytes: u64 = 0;
    let mut buf = vec![0u8; STREAM_BUFFER_SIZE];

    for idx in 0..archive.len() {
        if !progress(ArchiveProgress::new(total_bytes, declared_total)) {
            return Err(FolderArchiveError::Cancelled);
        }

        let mut entry = archive
            .by_index(idx)
            .map_err(|err| FolderArchiveError::Archive(err.to_string()))?;
        let raw_name = entry.name().to_string();

        if is_zip_entry_symlink(&entry) {
            return Err(FolderArchiveError::SymlinkRejected(raw_name));
        }

        let is_dir = entry.is_dir() || raw_name.ends_with('/');
        let normalized = validate_archive_entry_path(&raw_name, is_dir)
            .map_err(|_| FolderArchiveError::UnsafePath(raw_name.clone()))?;

        if !register_unique_archive_path(&normalized, is_dir, &mut seen_files, &mut seen_dirs) {
            return Err(FolderArchiveError::DuplicatePath(raw_name));
        }

        let out_path = dest_dir.join(&normalized);
        if is_dir {
            fs::create_dir_all(&out_path)?;
            dirs_created += 1;
            continue;
        }

        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut out_file = File::options()
            .write(true)
            .create_new(true)
            .open(&out_path)?;

        let mut file_bytes: u64 = 0;
        loop {
            let n = entry
                .read(&mut buf)
                .map_err(|err| FolderArchiveError::Archive(err.to_string()))?;
            if n == 0 {
                break;
            }
            file_bytes =
                file_bytes
                    .checked_add(n as u64)
                    .ok_or(FolderArchiveError::SingleFileTooLarge {
                        path: normalized.clone(),
                        size: u64::MAX,
                        limit: limits.max_single_file_bytes,
                    })?;
            if file_bytes > limits.max_single_file_bytes {
                return Err(FolderArchiveError::SingleFileTooLarge {
                    path: normalized,
                    size: file_bytes,
                    limit: limits.max_single_file_bytes,
                });
            }
            total_bytes = total_bytes.checked_add(n as u64).ok_or(
                FolderArchiveError::ByteBudgetExceeded {
                    bytes: u64::MAX,
                    limit: limits.max_total_uncompressed_bytes,
                },
            )?;
            if total_bytes > limits.max_total_uncompressed_bytes {
                return Err(FolderArchiveError::ByteBudgetExceeded {
                    bytes: total_bytes,
                    limit: limits.max_total_uncompressed_bytes,
                });
            }
            out_file.write_all(&buf[..n])?;
            if !progress(ArchiveProgress::new(
                total_bytes,
                declared_total.max(total_bytes),
            )) {
                return Err(FolderArchiveError::Cancelled);
            }
        }
        out_file.flush()?;
        files_extracted += 1;
    }

    let _ = progress(ArchiveProgress::new(total_bytes, total_bytes));

    Ok(ArchiveExtractResult {
        dest_dir: dest_dir.to_path_buf(),
        files_extracted,
        dirs_created,
        total_bytes,
    })
}

/// Remove files in `dir` whose modification time is older than `max_age`, mirroring
/// `ClearStaleArchiveFiles` in `storage_folder_archive.cpp`.
///
/// Returns the number of stale files removed.
pub fn clear_stale_archive_files(dir: &Path, max_age: Duration) -> FolderArchiveResult<usize> {
    if !dir.is_dir() {
        return Ok(0);
    }
    let cutoff = SystemTime::now().checked_sub(max_age).unwrap_or(UNIX_EPOCH);
    let mut removed = 0usize;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() || !meta.is_file() {
            continue;
        }
        if let Ok(modified) = meta.modified()
            && modified < cutoff
            && fs::remove_file(&path).is_ok()
        {
            removed += 1;
        }
    }
    Ok(removed)
}
