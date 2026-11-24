//! Sparse range streamed file downloader with priority seeks, in-flight deduplication, and
//! resumable partial-file metadata.
//!
//! Port of `Telegram/SourceFiles/storage/streamed_file_downloader.h`,
//! `streamed_file_downloader.cpp`, and `PriorityQueue` from
//! `Telegram/SourceFiles/media/streaming/media_streaming_loader.{h,cpp}`.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::future::Future;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tg_crypto::{const_time_eq, random_u64, sha256_bytes};
use tokio::sync::{oneshot, watch};

use crate::cache::binlog::CacheKey;
use crate::cloud_blob::{sha256_file, sha256_hex};
use crate::error::StorageError;

/// Default streaming part size (128 KiB, matching `Media::Streaming::Loader::kPartSize`).
pub const STREAMED_PART_SIZE: usize = 128 * 1024;

/// Default maximum number of concurrent part requests in flight
/// (`kRequestPartsCount = 32` in `streamed_file_downloader.cpp`).
pub const DEFAULT_MAX_IN_FLIGHT_PARTS: usize = 32;

/// Binary magic header for resumable partial-file metadata (`TGSP`).
pub const PARTIAL_META_MAGIC: [u8; 4] = *b"TGSP";

/// Current binary schema version for [`PartialFileMetadata`].
pub const PARTIAL_META_VERSION: u32 = 1;

/// Minimum encoded metadata size (header without bitset + trailing 32-byte SHA-256).
const PARTIAL_META_HEADER_LEN: usize = 4 + 4 + 8 + 8 + 8 + 8 + 4 + 4 + 1 + 32 + 4;
const PARTIAL_META_CHECKSUM_LEN: usize = 32;

/// Errors returned by streamed file download operations.
#[derive(Debug, thiserror::Error)]
pub enum StreamedDownloadError {
    /// Underlying filesystem I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Invalid configuration parameter (e.g., zero part size or concurrency limit).
    #[error("invalid streamed downloader configuration: {0}")]
    InvalidConfig(String),

    /// Requested byte offset or range is misaligned or out of file bounds.
    #[error("invalid range or offset: {0}")]
    InvalidRange(String),

    /// Chunk provider returned an error for a requested offset.
    #[error("chunk provider failed at offset {offset}: {message}")]
    ProviderFailed {
        /// Byte offset that failed.
        offset: u64,
        /// Provider error description.
        message: String,
    },

    /// Chunk provider returned a chunk whose length did not match the expected part length.
    #[error(
        "invalid chunk length at offset {offset}: expected {expected} bytes, got {actual} bytes"
    )]
    InvalidChunkLength {
        /// Byte offset of the part.
        offset: u64,
        /// Expected byte count.
        expected: usize,
        /// Actual byte count returned by provider.
        actual: usize,
    },

    /// Resumable partial-file metadata file was corrupt or mismatched.
    #[error("corrupt or mismatched partial metadata: {0}")]
    CorruptMetadata(String),

    /// Final assembled file failed SHA-256 verification.
    #[error("final file SHA-256 mismatch: expected {expected}, got {actual}")]
    Sha256Mismatch {
        /// Lowercase hex expected digest.
        expected: String,
        /// Lowercase hex actual digest.
        actual: String,
    },

    /// Download was cancelled.
    #[error("streamed download cancelled")]
    Cancelled,
}

impl From<StreamedDownloadError> for StorageError {
    fn from(err: StreamedDownloadError) -> Self {
        match err {
            StreamedDownloadError::Io(io) => Self::Io(io),
            StreamedDownloadError::Sha256Mismatch { .. } => Self::ChecksumMismatch,
            other => Self::CorruptData(other.to_string()),
        }
    }
}

/// Result alias for streamed file downloader operations.
pub type StreamedDownloadResult<T> = std::result::Result<T, StreamedDownloadError>;

/// Priority queue for part offsets, ported from `Media::Streaming::PriorityQueue`.
///
/// Entries with a higher `priority` epoch sort first; within the same epoch, smaller byte offsets
/// sort first. Calling [`Self::reset_priorities`] bumps the current epoch so newly queued seek
/// ranges preempt older sequential background requests.
#[derive(Debug, Clone, Default)]
pub struct PartPriorityQueue {
    entries: BTreeSet<PriorityEntry>,
    priorities: HashMap<u64, u64>,
    current_priority: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PriorityEntry {
    offset: u64,
    priority: u64,
}

impl Ord for PriorityEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .priority
            .cmp(&self.priority)
            .then_with(|| self.offset.cmp(&other.offset))
    }
}

impl PartialOrd for PriorityEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartPriorityQueue {
    /// Create an empty priority queue starting at priority epoch `0`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Current priority epoch.
    #[must_use]
    pub const fn current_priority(&self) -> u64 {
        self.current_priority
    }

    /// Increment the priority epoch so subsequent [`Self::add`] calls jump ahead of previously
    /// queued offsets (`PriorityQueue::resetPriorities`).
    pub fn reset_priorities(&mut self) -> u64 {
        self.current_priority = self.current_priority.saturating_add(1);
        self.current_priority
    }

    /// Insert `offset` at the current priority epoch, or elevate its priority if already present
    /// at a different epoch (`PriorityQueue::add`).
    pub fn add(&mut self, offset: u64) -> bool {
        self.add_with_priority(offset, self.current_priority)
    }

    /// Insert or update `offset` at an explicit `priority` epoch.
    pub fn add_with_priority(&mut self, offset: u64, priority: u64) -> bool {
        if let Some(&existing_priority) = self.priorities.get(&offset) {
            if existing_priority == priority {
                return false;
            }
            self.entries.remove(&PriorityEntry {
                offset,
                priority: existing_priority,
            });
        }
        self.priorities.insert(offset, priority);
        self.entries.insert(PriorityEntry { offset, priority });
        true
    }

    /// Remove `offset` from the queue (`PriorityQueue::remove`).
    pub fn remove(&mut self, offset: u64) -> bool {
        let Some(priority) = self.priorities.remove(&offset) else {
            return false;
        };
        self.entries.remove(&PriorityEntry { offset, priority });
        true
    }

    /// Whether the queue has no pending offsets.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of queued offsets.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Peek at the highest-priority offset without removing it (`PriorityQueue::front`).
    #[must_use]
    pub fn front(&self) -> Option<u64> {
        self.entries.first().map(|e| e.offset)
    }

    /// Pop the highest-priority `(offset, priority)` pair.
    pub fn take_with_priority(&mut self) -> Option<(u64, u64)> {
        let first = self.entries.pop_first()?;
        self.priorities.remove(&first.offset);
        Some((first.offset, first.priority))
    }

    /// Pop the highest-priority offset (`PriorityQueue::take`).
    pub fn take(&mut self) -> Option<u64> {
        self.take_with_priority().map(|(offset, _)| offset)
    }

    /// Remove and return all queued offsets in `[from, till)` (`PriorityQueue::takeInRange`).
    pub fn take_in_range(&mut self, from: u64, till: u64) -> Vec<u64> {
        let matched: Vec<PriorityEntry> = self
            .entries
            .iter()
            .copied()
            .filter(|e| e.offset >= from && e.offset < till)
            .collect();
        let mut out = Vec::with_capacity(matched.len());
        for entry in matched {
            self.entries.remove(&entry);
            self.priorities.remove(&entry.offset);
            out.push(entry.offset);
        }
        out.sort_unstable();
        out
    }

    /// Clear all queued offsets (`PriorityQueue::clear`).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.priorities.clear();
    }
}

/// Resumable on-disk metadata for a sparse partial download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialFileMetadata {
    /// Logical object identifier (`StreamedFileDownloader::objId`).
    pub object_id: u64,
    /// Storage cache key (`StreamedFileDownloader::cacheKey`).
    pub cache_key: CacheKey,
    /// Total file size in bytes.
    pub total_size: u64,
    /// Fixed part size in bytes.
    pub part_size: u32,
    /// Total number of parts (`ceil(total_size / part_size)`).
    pub parts_count: u32,
    /// Optional expected SHA-256 digest of the completed file.
    pub expected_sha256: Option<[u8; 32]>,
    /// Bitset tracking which parts have been persisted to the `.part` file.
    saved_bitset: Vec<u8>,
}

impl PartialFileMetadata {
    /// Create fresh empty partial metadata for a file of `total_size` bytes split into `part_size`
    /// byte parts.
    pub fn new(
        object_id: u64,
        cache_key: CacheKey,
        total_size: u64,
        part_size: usize,
        expected_sha256: Option<[u8; 32]>,
    ) -> StreamedDownloadResult<Self> {
        let part_size_u32 = u32::try_from(part_size).map_err(|_| {
            StreamedDownloadError::InvalidConfig("part_size must fit in u32".into())
        })?;
        if part_size_u32 == 0 {
            return Err(StreamedDownloadError::InvalidConfig(
                "part_size must be > 0".into(),
            ));
        }
        let parts_u64 = total_size.div_ceil(u64::from(part_size_u32));
        let parts_count = u32::try_from(parts_u64).map_err(|_| {
            StreamedDownloadError::InvalidConfig("parts_count exceeds u32::MAX".into())
        })?;
        let bitset_len = (parts_count as usize).div_ceil(8);
        Ok(Self {
            object_id,
            cache_key,
            total_size,
            part_size: part_size_u32,
            parts_count,
            expected_sha256,
            saved_bitset: vec![0u8; bitset_len],
        })
    }

    /// Expected byte length of part `index`.
    #[must_use]
    pub fn part_len(&self, index: usize) -> usize {
        let offset = (index as u64).saturating_mul(u64::from(self.part_size));
        let remaining = self.total_size.saturating_sub(offset);
        remaining.min(u64::from(self.part_size)) as usize
    }

    /// Whether part `index` has been saved to disk.
    #[must_use]
    pub fn is_part_saved(&self, index: usize) -> bool {
        if index >= self.parts_count as usize {
            return false;
        }
        let byte = self.saved_bitset[index / 8];
        (byte & (1u8 << (index % 8))) != 0
    }

    /// Mark part `index` as saved. Returns `true` if the bit transitioned from `0` to `1`.
    pub fn mark_part_saved(&mut self, index: usize) -> bool {
        if index >= self.parts_count as usize {
            return false;
        }
        let mask = 1u8 << (index % 8);
        let slot = &mut self.saved_bitset[index / 8];
        let was_saved = (*slot & mask) != 0;
        *slot |= mask;
        !was_saved
    }

    /// Number of parts currently marked saved.
    #[must_use]
    pub fn saved_parts_count(&self) -> usize {
        self.saved_bitset
            .iter()
            .map(|b| b.count_ones() as usize)
            .sum()
    }

    /// Total bytes persisted so far across all saved parts.
    #[must_use]
    pub fn saved_bytes(&self) -> u64 {
        let mut total = 0u64;
        for idx in 0..(self.parts_count as usize) {
            if self.is_part_saved(idx) {
                total += self.part_len(idx) as u64;
            }
        }
        total
    }

    /// Whether every part of the file has been saved.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.saved_parts_count() == self.parts_count as usize
    }

    /// Encode metadata into its checksummed binary representation.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            PARTIAL_META_HEADER_LEN + self.saved_bitset.len() + PARTIAL_META_CHECKSUM_LEN,
        );
        out.extend_from_slice(&PARTIAL_META_MAGIC);
        out.extend_from_slice(&PARTIAL_META_VERSION.to_le_bytes());
        out.extend_from_slice(&self.object_id.to_le_bytes());
        out.extend_from_slice(&self.cache_key.high.to_le_bytes());
        out.extend_from_slice(&self.cache_key.low.to_le_bytes());
        out.extend_from_slice(&self.total_size.to_le_bytes());
        out.extend_from_slice(&self.part_size.to_le_bytes());
        out.extend_from_slice(&self.parts_count.to_le_bytes());
        match &self.expected_sha256 {
            Some(digest) => {
                out.push(1);
                out.extend_from_slice(digest);
            }
            None => {
                out.push(0);
                out.extend_from_slice(&[0u8; 32]);
            }
        }
        out.extend_from_slice(&(self.saved_bitset.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.saved_bitset);
        let checksum = sha256_bytes(&out);
        out.extend_from_slice(&checksum);
        out
    }

    /// Decode and verify metadata bytes.
    pub fn decode(data: &[u8]) -> StreamedDownloadResult<Self> {
        if data.len() < PARTIAL_META_HEADER_LEN + PARTIAL_META_CHECKSUM_LEN {
            return Err(StreamedDownloadError::CorruptMetadata(
                "metadata file shorter than header".into(),
            ));
        }
        let body_len = data.len() - PARTIAL_META_CHECKSUM_LEN;
        let expected_checksum = &data[body_len..];
        let actual_checksum = sha256_bytes(&data[..body_len]);
        if !const_time_eq(&actual_checksum, expected_checksum) {
            return Err(StreamedDownloadError::CorruptMetadata(
                "metadata checksum mismatch".into(),
            ));
        }

        if data[0..4] != PARTIAL_META_MAGIC {
            return Err(StreamedDownloadError::CorruptMetadata(
                "invalid metadata magic".into(),
            ));
        }
        let version = u32::from_le_bytes(data[4..8].try_into().unwrap());
        if version != PARTIAL_META_VERSION {
            return Err(StreamedDownloadError::CorruptMetadata(format!(
                "unsupported metadata version {version}"
            )));
        }
        let object_id = u64::from_le_bytes(data[8..16].try_into().unwrap());
        let cache_high = u64::from_le_bytes(data[16..24].try_into().unwrap());
        let cache_low = u64::from_le_bytes(data[24..32].try_into().unwrap());
        let total_size = u64::from_le_bytes(data[32..40].try_into().unwrap());
        let part_size = u32::from_le_bytes(data[40..44].try_into().unwrap());
        let parts_count = u32::from_le_bytes(data[44..48].try_into().unwrap());
        if part_size == 0 {
            return Err(StreamedDownloadError::CorruptMetadata(
                "metadata part_size is zero".into(),
            ));
        }
        let expected_parts = total_size.div_ceil(u64::from(part_size));
        if u64::from(parts_count) != expected_parts {
            return Err(StreamedDownloadError::CorruptMetadata(
                "metadata parts_count does not match total_size / part_size".into(),
            ));
        }
        let has_sha256 = data[48];
        let expected_sha256 = match has_sha256 {
            0 => None,
            1 => {
                let mut digest = [0u8; 32];
                digest.copy_from_slice(&data[49..81]);
                Some(digest)
            }
            other => {
                return Err(StreamedDownloadError::CorruptMetadata(format!(
                    "invalid sha256 presence flag {other}"
                )));
            }
        };
        let bitset_len = u32::from_le_bytes(data[81..85].try_into().unwrap()) as usize;
        let expected_bitset_len = (parts_count as usize).div_ceil(8);
        if bitset_len != expected_bitset_len || body_len != PARTIAL_META_HEADER_LEN + bitset_len {
            return Err(StreamedDownloadError::CorruptMetadata(
                "metadata bitset length mismatch".into(),
            ));
        }
        let mut saved_bitset = data[85..85 + bitset_len].to_vec();
        // Mask out any unused trailing bits in the final byte so `count_ones()` stays exact.
        let rem = (parts_count as usize) % 8;
        if rem != 0
            && let Some(last) = saved_bitset.last_mut()
        {
            *last &= (1u8 << rem) - 1;
        }

        Ok(Self {
            object_id,
            cache_key: CacheKey::new(cache_high, cache_low),
            total_size,
            part_size,
            parts_count,
            expected_sha256,
            saved_bitset,
        })
    }

    /// Persist metadata atomically via a sibling temporary file + rename.
    pub fn save_atomic(&self, path: &Path) -> StreamedDownloadResult<()> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let encoded = self.encode();

        let temp_path = loop {
            let nonce = random_u64();
            let candidate = parent.join(format!(".meta-{nonce:016x}.tmp"));
            if !candidate.exists() {
                break candidate;
            }
        };

        let mut file = File::options()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        file.write_all(&encoded)?;
        file.flush()?;
        drop(file);

        fs::rename(&temp_path, path)?;
        Ok(())
    }

    /// Load and verify metadata from `path`.
    pub fn load(path: &Path) -> StreamedDownloadResult<Self> {
        let bytes = fs::read(path)?;
        Self::decode(&bytes)
    }
}

/// Future returned by [`ChunkProvider::fetch_chunk`].
pub type ChunkFuture<'a> =
    Pin<Box<dyn Future<Output = StreamedDownloadResult<Vec<u8>>> + Send + 'a>>;

/// Remote chunk transport abstraction for [`StreamedFileDownloader`] (e.g. MTProto `upload.getFile`
/// reader or an in-memory fake provider in tests).
pub trait ChunkProvider: Send + Sync + 'static {
    /// Fetch exactly `length` bytes starting at aligned byte `offset`.
    fn fetch_chunk(&self, offset: u64, length: usize) -> ChunkFuture<'_>;

    /// Optional notification invoked when an in-flight chunk request is cancelled (for example
    /// because a seek preempted lower-priority background prefetching or the download was
    /// cancelled).
    fn cancel_chunk(&self, _offset: u64) {}
}

/// Configuration for [`StreamedFileDownloader`].
#[derive(Debug, Clone)]
pub struct StreamedDownloaderConfig {
    /// Logical object identifier (`StreamedFileDownloader::objId`).
    pub object_id: u64,
    /// Associated storage cache key (`StreamedFileDownloader::cacheKey`).
    pub cache_key: CacheKey,
    /// Total file size in bytes.
    pub total_size: u64,
    /// Aligned part size in bytes (default: [`STREAMED_PART_SIZE`] = 128 KiB).
    pub part_size: usize,
    /// Maximum concurrent in-flight part requests (default: [`DEFAULT_MAX_IN_FLIGHT_PARTS`] = 32).
    pub max_in_flight: usize,
    /// Optional SHA-256 digest verified before finalizing the completed file.
    pub expected_sha256: Option<[u8; 32]>,
    /// Final destination file path once all parts have been downloaded.
    pub target_path: PathBuf,
    /// Optional explicit path for the sparse partial file (defaults to `<target_path>.part`).
    pub part_path: Option<PathBuf>,
    /// Optional explicit path for the partial metadata file (defaults to `<target_path>.part.meta`).
    pub meta_path: Option<PathBuf>,
}

impl StreamedDownloaderConfig {
    /// Create a configuration with default 128 KiB parts and 32 concurrent slots.
    #[must_use]
    pub fn new(object_id: u64, total_size: u64, target_path: impl Into<PathBuf>) -> Self {
        Self {
            object_id,
            cache_key: CacheKey::default(),
            total_size,
            part_size: STREAMED_PART_SIZE,
            max_in_flight: DEFAULT_MAX_IN_FLIGHT_PARTS,
            expected_sha256: None,
            target_path: target_path.into(),
            part_path: None,
            meta_path: None,
        }
    }

    /// Resolve the sparse `.part` file path.
    #[must_use]
    pub fn resolved_part_path(&self) -> PathBuf {
        self.part_path
            .clone()
            .unwrap_or_else(|| append_extension(&self.target_path, "part"))
    }

    /// Resolve the `.part.meta` metadata file path.
    #[must_use]
    pub fn resolved_meta_path(&self) -> PathBuf {
        self.meta_path
            .clone()
            .unwrap_or_else(|| append_extension(&self.resolved_part_path(), "meta"))
    }
}

fn append_extension(path: &Path, suffix: &str) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".");
    os.push(suffix);
    PathBuf::from(os)
}

/// Snapshot of streamed download progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DownloadProgress {
    /// Bytes persisted on disk so far.
    pub saved_bytes: u64,
    /// Total file size in bytes.
    pub total_bytes: u64,
    /// Number of parts persisted on disk so far.
    pub saved_parts: usize,
    /// Total number of parts in the file.
    pub total_parts: usize,
    /// Number of chunk requests currently in flight.
    pub in_flight_parts: usize,
    /// Whether the file has been finalized at `target_path`.
    pub finished: bool,
    /// Whether the downloader has been cancelled.
    pub cancelled: bool,
}

impl DownloadProgress {
    /// Completion fraction clamped to `[0.0, 1.0]`.
    #[must_use]
    pub fn fraction(self) -> f64 {
        if self.total_bytes == 0 {
            if self.finished { 1.0 } else { 0.0 }
        } else {
            (self.saved_bytes as f64 / self.total_bytes as f64).clamp(0.0, 1.0)
        }
    }
}

struct InFlightPart {
    request_id: u64,
    offset: u64,
    priority: u64,
    abort_handle: tokio::task::AbortHandle,
    waiters: Vec<oneshot::Sender<StreamedDownloadResult<Vec<u8>>>>,
}

struct MutableState {
    meta: PartialFileMetadata,
    queue: PartPriorityQueue,
    in_flight: HashMap<usize, InFlightPart>,
    next_request_id: u64,
    next_sequential_index: usize,
    finished: bool,
    cancelled: bool,
    last_error: Option<(u64, String)>,
}

impl MutableState {
    fn progress_snapshot(&self) -> DownloadProgress {
        DownloadProgress {
            saved_bytes: self.meta.saved_bytes(),
            total_bytes: self.meta.total_size,
            saved_parts: self.meta.saved_parts_count(),
            total_parts: self.meta.parts_count as usize,
            in_flight_parts: self.in_flight.len(),
            finished: self.finished,
            cancelled: self.cancelled,
        }
    }

    fn next_unsaved_sequential(&mut self) -> Option<usize> {
        let total = self.meta.parts_count as usize;
        if total == 0 {
            return None;
        }
        // Scan from `next_sequential_index` to end, then wrap once to pick up any previously
        // evicted low-priority parts.
        for offset_step in 0..total {
            let idx = (self.next_sequential_index + offset_step) % total;
            if !self.meta.is_part_saved(idx) && !self.in_flight.contains_key(&idx) {
                self.next_sequential_index = (idx + 1) % total;
                return Some(idx);
            }
        }
        None
    }
}

struct DownloaderInner<P: ChunkProvider> {
    config: StreamedDownloaderConfig,
    part_path: PathBuf,
    meta_path: PathBuf,
    provider: Arc<P>,
    file: Mutex<Option<File>>,
    state: Mutex<MutableState>,
    progress_tx: watch::Sender<DownloadProgress>,
    progress_rx: watch::Receiver<DownloadProgress>,
    state_gen_tx: watch::Sender<u64>,
    state_gen_rx: watch::Receiver<u64>,
}

/// Sparse range file downloader coordinating chunk requests, seek-priority cancellation,
/// in-flight offset deduplication, and resumable partial-file metadata.
pub struct StreamedFileDownloader<P: ChunkProvider> {
    inner: Arc<DownloaderInner<P>>,
}

impl<P: ChunkProvider> Clone for StreamedFileDownloader<P> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<P: ChunkProvider> StreamedFileDownloader<P> {
    /// Open or resume a streamed file download.
    ///
    /// If valid partial metadata and its companion `.part` file already exist on disk and match
    /// `config` (`object_id`, `total_size`, `part_size`, `expected_sha256`), previously downloaded
    /// parts are restored so they will not be re-fetched.
    pub fn open(
        config: StreamedDownloaderConfig,
        provider: Arc<P>,
    ) -> StreamedDownloadResult<Self> {
        if config.part_size == 0 {
            return Err(StreamedDownloadError::InvalidConfig(
                "part_size must be > 0".into(),
            ));
        }
        if config.max_in_flight == 0 {
            return Err(StreamedDownloadError::InvalidConfig(
                "max_in_flight must be > 0".into(),
            ));
        }

        let part_path = config.resolved_part_path();
        let meta_path = config.resolved_meta_path();

        if let Some(parent) = config.target_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        if let Some(parent) = part_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        if let Some(parent) = meta_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }

        let fresh_meta = PartialFileMetadata::new(
            config.object_id,
            config.cache_key,
            config.total_size,
            config.part_size,
            config.expected_sha256,
        )?;

        let meta = if meta_path.is_file() && part_path.is_file() {
            match PartialFileMetadata::load(&meta_path) {
                Ok(loaded)
                    if loaded.object_id == fresh_meta.object_id
                        && loaded.total_size == fresh_meta.total_size
                        && loaded.part_size == fresh_meta.part_size
                        && loaded.expected_sha256 == fresh_meta.expected_sha256 =>
                {
                    let part_len = fs::metadata(&part_path)?.len();
                    if part_len == config.total_size {
                        loaded
                    } else {
                        fresh_meta
                    }
                }
                _ => fresh_meta,
            }
        } else {
            fresh_meta
        };

        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&part_path)?;
        if file.metadata()?.len() != config.total_size {
            file.set_len(config.total_size)?;
        }
        meta.save_atomic(&meta_path)?;

        let mutable = MutableState {
            meta,
            queue: PartPriorityQueue::new(),
            in_flight: HashMap::new(),
            next_request_id: 1,
            next_sequential_index: 0,
            finished: false,
            cancelled: false,
            last_error: None,
        };
        let initial_progress = mutable.progress_snapshot();
        let (progress_tx, progress_rx) = watch::channel(initial_progress);
        let (state_gen_tx, state_gen_rx) = watch::channel(0u64);

        Ok(Self {
            inner: Arc::new(DownloaderInner {
                config,
                part_path,
                meta_path,
                provider,
                file: Mutex::new(Some(file)),
                state: Mutex::new(mutable),
                progress_tx,
                progress_rx,
                state_gen_tx,
                state_gen_rx,
            }),
        })
    }

    /// Logical object ID (`StreamedFileDownloader::objId`).
    #[must_use]
    pub fn object_id(&self) -> u64 {
        self.inner.config.object_id
    }

    /// Storage cache key (`StreamedFileDownloader::cacheKey`).
    #[must_use]
    pub fn cache_key(&self) -> CacheKey {
        self.inner.config.cache_key
    }

    /// Total expected file size in bytes.
    #[must_use]
    pub fn total_size(&self) -> u64 {
        self.inner.config.total_size
    }

    /// Aligned part size in bytes.
    #[must_use]
    pub fn part_size(&self) -> usize {
        self.inner.config.part_size
    }

    /// Final destination file path.
    #[must_use]
    pub fn target_path(&self) -> &Path {
        &self.inner.config.target_path
    }

    /// Sparse partial file path.
    #[must_use]
    pub fn part_path(&self) -> &Path {
        &self.inner.part_path
    }

    /// Resumable metadata file path.
    #[must_use]
    pub fn meta_path(&self) -> &Path {
        &self.inner.meta_path
    }

    /// Current progress snapshot.
    #[must_use]
    pub fn progress(&self) -> DownloadProgress {
        *self.inner.progress_rx.borrow()
    }

    /// Subscribe to progress updates.
    #[must_use]
    pub fn subscribe_progress(&self) -> watch::Receiver<DownloadProgress> {
        self.inner.progress_rx.clone()
    }

    /// Returns whether part `index` is already saved on disk.
    #[must_use]
    pub fn is_part_saved(&self, index: usize) -> bool {
        self.inner.state.lock().unwrap().meta.is_part_saved(index)
    }

    /// Read a previously loaded aligned part back from disk (`StreamedFileDownloader::readLoadedPart`).
    ///
    /// Returns `Ok(None)` if the part has not been downloaded yet.
    pub fn read_loaded_part(&self, offset: u64) -> StreamedDownloadResult<Option<Vec<u8>>> {
        let part_size = self.inner.config.part_size as u64;
        if offset >= self.inner.config.total_size || !offset.is_multiple_of(part_size) {
            return Err(StreamedDownloadError::InvalidRange(format!(
                "offset {offset} must be < {} and aligned to {part_size}",
                self.inner.config.total_size
            )));
        }
        let index = (offset / part_size) as usize;
        let (is_saved, expected_len, is_finished) = {
            let state = self.inner.state.lock().unwrap();
            (
                state.meta.is_part_saved(index),
                state.meta.part_len(index),
                state.finished,
            )
        };
        if !is_saved {
            return Ok(None);
        }
        self.inner
            .read_bytes_from_disk(offset, expected_len, is_finished)
            .map(Some)
    }

    /// Prioritize byte range `[offset, offset + len)` by advancing the priority epoch, cancelling
    /// lower-priority background in-flight parts outside the range (re-queuing them at their old
    /// priority so background download finishes them later), and returning the cancelled offsets.
    pub fn seek_and_prioritize(&self, offset: u64, len: usize) -> StreamedDownloadResult<Vec<u64>> {
        let (start_part, end_part) = self.inner.part_range(offset, len)?;
        if start_part == end_part {
            return Ok(Vec::new());
        }

        let cancelled_offsets = {
            let mut state = self.inner.state.lock().unwrap();
            if state.cancelled {
                return Err(StreamedDownloadError::Cancelled);
            }
            let new_priority = state.queue.reset_priorities();
            let cancelled = self.inner.cancel_lower_priority_background_locked(
                &mut state,
                new_priority,
                start_part..end_part,
            );

            // Elevate already in-flight parts inside the range, and queue unsaved ones.
            let part_size = self.inner.config.part_size as u64;
            for idx in start_part..end_part {
                if state.meta.is_part_saved(idx) {
                    continue;
                }
                if let Some(in_flight) = state.in_flight.get_mut(&idx) {
                    in_flight.priority = in_flight.priority.max(new_priority);
                } else {
                    state
                        .queue
                        .add_with_priority((idx as u64) * part_size, new_priority);
                }
            }
            self.inner.notify_state_change(&state);
            cancelled
        };

        for cancelled_offset in &cancelled_offsets {
            self.inner.provider.cancel_chunk(*cancelled_offset);
        }
        Ok(cancelled_offsets)
    }

    /// Dispatch background prefetch requests (with no direct waiter) for unsaved parts covering
    /// `[offset, offset + len)` at the current priority epoch, up to `max_in_flight`.
    ///
    /// Because prefetched parts carry no direct waiter, a subsequent high-priority seek via
    /// [`Self::read_range`], [`Self::seek_and_prioritize`], or [`Self::reset_priorities`] can
    /// preempt and cancel them cleanly, while a read targeting the same part deduplicates onto
    /// the in-flight request without issuing a duplicate fetch.
    pub fn prefetch_range(&self, offset: u64, len: usize) -> StreamedDownloadResult<usize> {
        let (start_part, end_part) = self.inner.part_range(offset, len)?;
        if start_part == end_part {
            return Ok(0);
        }

        let mut state = self.inner.state.lock().unwrap();
        if state.cancelled {
            return Err(StreamedDownloadError::Cancelled);
        }
        if state.finished {
            return Ok(0);
        }

        let part_size = self.inner.config.part_size as u64;
        let priority = state.queue.current_priority();
        let mut dispatched = 0usize;

        for idx in start_part..end_part {
            if state.meta.is_part_saved(idx) || state.in_flight.contains_key(&idx) {
                continue;
            }
            if state.in_flight.len() < self.inner.config.max_in_flight {
                self.inner
                    .dispatch_part_locked(&mut state, idx, priority, None);
                dispatched += 1;
            } else {
                state
                    .queue
                    .add_with_priority((idx as u64) * part_size, priority);
            }
        }

        if dispatched > 0 {
            self.inner.notify_state_change(&state);
        }
        Ok(dispatched)
    }

    /// Advance the priority epoch and cancel any lower-priority background in-flight parts
    /// (re-queuing them at their old priority), notifying [`ChunkProvider::cancel_chunk`].
    pub fn reset_priorities(&self) -> Vec<u64> {
        let cancelled = {
            let mut state = self.inner.state.lock().unwrap();
            if state.cancelled || state.finished {
                return Vec::new();
            }
            let new_priority = state.queue.reset_priorities();
            let cancelled =
                self.inner
                    .cancel_lower_priority_background_locked(&mut state, new_priority, 0..0);
            if !cancelled.is_empty() {
                self.inner.notify_state_change(&state);
            }
            cancelled
        };
        for offset in &cancelled {
            self.inner.provider.cancel_chunk(*offset);
        }
        cancelled
    }

    /// Cancel a specific background in-flight part at aligned `offset` if no direct reader is
    /// waiting on it, notifying [`ChunkProvider::cancel_chunk`].
    pub fn cancel_part(&self, offset: u64) -> bool {
        let part_size = self.inner.config.part_size as u64;
        if offset >= self.inner.config.total_size || !offset.is_multiple_of(part_size) {
            return false;
        }
        let idx = (offset / part_size) as usize;
        let cancelled_offset = {
            let mut state = self.inner.state.lock().unwrap();
            let can_cancel = state
                .in_flight
                .get(&idx)
                .is_some_and(|entry| entry.waiters.is_empty());
            if !can_cancel {
                return false;
            }
            if let Some(entry) = state.in_flight.remove(&idx) {
                entry.abort_handle.abort();
                state.queue.add_with_priority(entry.offset, entry.priority);
                self.inner.notify_state_change(&state);
                Some(entry.offset)
            } else {
                None
            }
        };
        if let Some(off) = cancelled_offset {
            self.inner.provider.cancel_chunk(off);
            true
        } else {
            false
        }
    }

    /// Read an arbitrary byte range `[offset, offset + len)`, seeking with high priority,
    /// deduplicating against any already in-flight or saved parts, persisting fetched parts to
    /// disk, and returning the exact requested slice.
    pub async fn read_range(&self, offset: u64, len: usize) -> StreamedDownloadResult<Vec<u8>> {
        if len == 0 {
            if offset > self.inner.config.total_size {
                return Err(StreamedDownloadError::InvalidRange(format!(
                    "offset {offset} exceeds total_size {}",
                    self.inner.config.total_size
                )));
            }
            return Ok(Vec::new());
        }

        let (start_part, end_part) = self.inner.part_range(offset, len)?;
        let part_size = self.inner.config.part_size as u64;

        // Advance priority epoch and preempt background prefetch outside the seek range.
        let seek_priority = {
            let mut state = self.inner.state.lock().unwrap();
            if state.cancelled {
                return Err(StreamedDownloadError::Cancelled);
            }
            let new_priority = state.queue.reset_priorities();
            let cancelled = self.inner.cancel_lower_priority_background_locked(
                &mut state,
                new_priority,
                start_part..end_part,
            );
            for idx in start_part..end_part {
                if let Some(in_flight) = state.in_flight.get_mut(&idx) {
                    in_flight.priority = in_flight.priority.max(new_priority);
                }
            }
            self.inner.notify_state_change(&state);
            drop(state);
            for cancelled_offset in cancelled {
                self.inner.provider.cancel_chunk(cancelled_offset);
            }
            new_priority
        };

        // Ensure every part covering `[offset, offset + len)` is either already saved or awaited.
        let mut pending_receivers: Vec<(
            usize,
            oneshot::Receiver<StreamedDownloadResult<Vec<u8>>>,
        )> = Vec::new();
        let mut remaining_parts: Vec<usize> = (start_part..end_part).collect();

        while !remaining_parts.is_empty() {
            let mut gen_rx = self.inner.state_gen_rx.clone();
            let _ = *gen_rx.borrow_and_update();

            let mut still_waiting_for_slot = Vec::new();
            let mut cancelled_offsets = Vec::new();
            {
                let mut state = self.inner.state.lock().unwrap();
                if state.cancelled {
                    return Err(StreamedDownloadError::Cancelled);
                }

                for idx in remaining_parts.drain(..) {
                    if state.meta.is_part_saved(idx) {
                        continue;
                    }
                    if let Some(in_flight) = state.in_flight.get_mut(&idx) {
                        // Deduplicate: attach a waiter to the existing in-flight request.
                        in_flight.priority = in_flight.priority.max(seek_priority);
                        let (tx, rx) = oneshot::channel();
                        in_flight.waiters.push(tx);
                        pending_receivers.push((idx, rx));
                        continue;
                    }
                    if state.in_flight.len() >= self.inner.config.max_in_flight {
                        // Try to evict one lower-priority background part to make room.
                        if let Some(evicted_offset) = self.inner.evict_one_background_locked(
                            &mut state,
                            seek_priority,
                            start_part..end_part,
                        ) {
                            cancelled_offsets.push(evicted_offset);
                        } else {
                            still_waiting_for_slot.push(idx);
                            continue;
                        }
                    }
                    let (tx, rx) = oneshot::channel();
                    self.inner
                        .dispatch_part_locked(&mut state, idx, seek_priority, Some(tx));
                    pending_receivers.push((idx, rx));
                }

                // Queue any parts still waiting for concurrency slots at seek_priority.
                for &idx in &still_waiting_for_slot {
                    state
                        .queue
                        .add_with_priority((idx as u64) * part_size, seek_priority);
                }
                self.inner.notify_state_change(&state);
            }

            for cancelled_offset in cancelled_offsets {
                self.inner.provider.cancel_chunk(cancelled_offset);
            }

            if !still_waiting_for_slot.is_empty() {
                remaining_parts = still_waiting_for_slot;
                let _ = gen_rx.changed().await;
            }
        }

        // Await all newly or previously in-flight parts needed for this range.
        for (_idx, rx) in pending_receivers {
            match rx.await {
                Ok(Ok(_bytes)) => {}
                Ok(Err(err)) => return Err(err),
                Err(_) => return Err(StreamedDownloadError::Cancelled),
            }
        }

        let is_finished = self.inner.state.lock().unwrap().finished;
        self.inner.read_bytes_from_disk(offset, len, is_finished)
    }

    /// Drive downloading all remaining parts of the file (honoring queued seek priorities and
    /// skipping already-saved parts), verify optional SHA-256, and atomically finalize into
    /// [`Self::target_path`].
    pub async fn download_all(&self) -> StreamedDownloadResult<PathBuf> {
        if self.inner.config.total_size == 0 {
            return self.inner.finalize_file();
        }

        loop {
            let mut gen_rx = self.inner.state_gen_rx.clone();
            let _ = *gen_rx.borrow_and_update();

            let should_finalize = {
                let mut state = self.inner.state.lock().unwrap();
                if state.cancelled {
                    return Err(StreamedDownloadError::Cancelled);
                }
                if state.finished {
                    return Ok(self.inner.config.target_path.clone());
                }
                if let Some((err_offset, msg)) = state.last_error.clone() {
                    return Err(StreamedDownloadError::ProviderFailed {
                        offset: err_offset,
                        message: msg,
                    });
                }
                if state.meta.is_complete() {
                    true
                } else {
                    let part_size = self.inner.config.part_size as u64;
                    let mut dispatched = false;
                    while state.in_flight.len() < self.inner.config.max_in_flight {
                        let next_candidate = loop {
                            if let Some((queued_offset, prio)) = state.queue.take_with_priority() {
                                let idx = (queued_offset / part_size) as usize;
                                if !state.meta.is_part_saved(idx)
                                    && !state.in_flight.contains_key(&idx)
                                {
                                    break Some((idx, prio));
                                }
                            } else {
                                break state.next_unsaved_sequential().map(|idx| (idx, 0));
                            }
                        };
                        let Some((idx, prio)) = next_candidate else {
                            break;
                        };
                        self.inner.dispatch_part_locked(&mut state, idx, prio, None);
                        dispatched = true;
                    }
                    if dispatched {
                        self.inner.notify_state_change(&state);
                    }
                    false
                }
            };

            if should_finalize {
                return self.inner.finalize_file();
            }

            if gen_rx.changed().await.is_err() {
                return Err(StreamedDownloadError::Cancelled);
            }
        }
    }

    /// Cancel all in-flight chunk requests, notify the provider, and flush resumable metadata so
    /// progress persisted so far can be resumed after reopening.
    pub fn cancel(&self) {
        let (cancelled_offsets, waiters) = {
            let mut state = self.inner.state.lock().unwrap();
            if state.finished {
                return;
            }
            state.cancelled = true;
            state.queue.clear();
            state.next_sequential_index = 0;

            let mut offsets = Vec::with_capacity(state.in_flight.len());
            let mut all_waiters = Vec::new();
            for (_, mut entry) in state.in_flight.drain() {
                entry.abort_handle.abort();
                offsets.push(entry.offset);
                all_waiters.append(&mut entry.waiters);
            }
            let _ = state.meta.save_atomic(&self.inner.meta_path);
            self.inner.notify_state_change(&state);
            (offsets, all_waiters)
        };

        for tx in waiters {
            let _ = tx.send(Err(StreamedDownloadError::Cancelled));
        }
        for offset in cancelled_offsets {
            self.inner.provider.cancel_chunk(offset);
        }
    }
}

impl<P: ChunkProvider> DownloaderInner<P> {
    fn part_range(&self, offset: u64, len: usize) -> StreamedDownloadResult<(usize, usize)> {
        let end = offset
            .checked_add(len as u64)
            .ok_or_else(|| StreamedDownloadError::InvalidRange("range end overflow".into()))?;
        if end > self.config.total_size {
            return Err(StreamedDownloadError::InvalidRange(format!(
                "range [{offset}, {end}) exceeds total_size {}",
                self.config.total_size
            )));
        }
        if len == 0 {
            return Ok((0, 0));
        }
        let part_size = self.config.part_size as u64;
        let start_part = (offset / part_size) as usize;
        let end_part = end.div_ceil(part_size) as usize;
        Ok((start_part, end_part))
    }

    fn notify_state_change(&self, state: &MutableState) {
        let _ = self.progress_tx.send(state.progress_snapshot());
        let next_gen = self.state_gen_tx.borrow().wrapping_add(1);
        let _ = self.state_gen_tx.send(next_gen);
    }

    fn cancel_lower_priority_background_locked(
        &self,
        state: &mut MutableState,
        min_priority: u64,
        keep_parts: std::ops::Range<usize>,
    ) -> Vec<u64> {
        let to_evict: Vec<usize> = state
            .in_flight
            .iter()
            .filter(|&(&idx, entry)| {
                !keep_parts.contains(&idx)
                    && entry.priority < min_priority
                    && entry.waiters.is_empty()
            })
            .map(|(&idx, _)| idx)
            .collect();

        let mut cancelled_offsets = Vec::with_capacity(to_evict.len());
        for idx in to_evict {
            if let Some(entry) = state.in_flight.remove(&idx) {
                entry.abort_handle.abort();
                state.queue.add_with_priority(entry.offset, entry.priority);
                cancelled_offsets.push(entry.offset);
            }
        }
        cancelled_offsets.sort_unstable();
        cancelled_offsets
    }

    fn evict_one_background_locked(
        &self,
        state: &mut MutableState,
        min_priority: u64,
        keep_parts: std::ops::Range<usize>,
    ) -> Option<u64> {
        let victim_idx = state
            .in_flight
            .iter()
            .filter(|&(&idx, entry)| {
                !keep_parts.contains(&idx)
                    && entry.priority < min_priority
                    && entry.waiters.is_empty()
            })
            .min_by_key(|&(&idx, entry)| (entry.priority, std::cmp::Reverse(idx)))
            .map(|(&idx, _)| idx)?;

        let entry = state.in_flight.remove(&victim_idx)?;
        entry.abort_handle.abort();
        state.queue.add_with_priority(entry.offset, entry.priority);
        Some(entry.offset)
    }

    fn dispatch_part_locked(
        self: &Arc<Self>,
        state: &mut MutableState,
        part_index: usize,
        priority: u64,
        waiter: Option<oneshot::Sender<StreamedDownloadResult<Vec<u8>>>>,
    ) {
        let request_id = state.next_request_id;
        state.next_request_id = state.next_request_id.wrapping_add(1);
        let offset = (part_index as u64) * (self.config.part_size as u64);
        let expected_len = state.meta.part_len(part_index);
        state.queue.remove(offset);

        let inner = Arc::clone(self);
        let handle = tokio::spawn(async move {
            let fetch_result = inner.provider.fetch_chunk(offset, expected_len).await;
            inner.complete_in_flight(part_index, offset, expected_len, request_id, fetch_result);
        });

        let mut waiters = Vec::new();
        if let Some(tx) = waiter {
            waiters.push(tx);
        }

        state.in_flight.insert(
            part_index,
            InFlightPart {
                request_id,
                offset,
                priority,
                abort_handle: handle.abort_handle(),
                waiters,
            },
        );
    }

    fn complete_in_flight(
        &self,
        part_index: usize,
        offset: u64,
        expected_len: usize,
        request_id: u64,
        fetch_result: StreamedDownloadResult<Vec<u8>>,
    ) {
        // Validate chunk length before touching disk.
        let validated = match fetch_result {
            Ok(bytes) if bytes.len() != expected_len => {
                Err(StreamedDownloadError::InvalidChunkLength {
                    offset,
                    expected: expected_len,
                    actual: bytes.len(),
                })
            }
            other => other,
        };

        // Ensure this request is still the active in-flight entry before writing to disk.
        {
            let state = self.state.lock().unwrap();
            let still_active = state
                .in_flight
                .get(&part_index)
                .is_some_and(|entry| entry.request_id == request_id);
            if !still_active || state.cancelled {
                return;
            }
        }

        // Write valid chunk bytes to the sparse `.part` file without holding `state` lock.
        let persisted = match validated {
            Ok(bytes) => match self.write_part_to_disk(offset, &bytes) {
                Ok(()) => Ok(bytes),
                Err(err) => Err(err),
            },
            Err(err) => Err(err),
        };

        // Update metadata & extract waiters under `state` lock.
        let waiters = {
            let mut state = self.state.lock().unwrap();
            let still_active = state
                .in_flight
                .get(&part_index)
                .is_some_and(|entry| entry.request_id == request_id);
            if !still_active {
                return;
            }
            let entry = state.in_flight.remove(&part_index).unwrap();
            match &persisted {
                Ok(_) => {
                    if state.meta.mark_part_saved(part_index)
                        && let Err(meta_err) = state.meta.save_atomic(&self.meta_path)
                    {
                        state.last_error = Some((offset, meta_err.to_string()));
                    }
                }
                Err(err) => {
                    if !matches!(err, StreamedDownloadError::Cancelled) {
                        state.last_error = Some((offset, err.to_string()));
                    }
                }
            }
            self.notify_state_change(&state);
            entry.waiters
        };

        // Notify any attached range readers.
        if !waiters.is_empty() {
            match persisted {
                Ok(bytes) => {
                    let count = waiters.len();
                    for (idx, tx) in waiters.into_iter().enumerate() {
                        if idx + 1 == count {
                            let _ = tx.send(Ok(bytes));
                            break;
                        }
                        let _ = tx.send(Ok(bytes.clone()));
                    }
                }
                Err(err) => {
                    let msg = err.to_string();
                    for tx in waiters {
                        let _ = tx.send(Err(StreamedDownloadError::ProviderFailed {
                            offset,
                            message: msg.clone(),
                        }));
                    }
                }
            }
        }
    }

    fn write_part_to_disk(&self, offset: u64, bytes: &[u8]) -> StreamedDownloadResult<()> {
        let mut guard = self.file.lock().unwrap();
        let file = guard.as_mut().ok_or(StreamedDownloadError::Cancelled)?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(bytes)?;
        file.flush()?;
        Ok(())
    }

    fn read_bytes_from_disk(
        &self,
        offset: u64,
        len: usize,
        is_finished: bool,
    ) -> StreamedDownloadResult<Vec<u8>> {
        let mut buf = vec![0u8; len];
        if is_finished {
            let mut f = File::open(&self.config.target_path)?;
            f.seek(SeekFrom::Start(offset))?;
            f.read_exact(&mut buf)?;
            return Ok(buf);
        }
        let mut guard = self.file.lock().unwrap();
        if let Some(file) = guard.as_mut() {
            file.seek(SeekFrom::Start(offset))?;
            file.read_exact(&mut buf)?;
            Ok(buf)
        } else {
            drop(guard);
            let mut f = File::open(&self.config.target_path)?;
            f.seek(SeekFrom::Start(offset))?;
            f.read_exact(&mut buf)?;
            Ok(buf)
        }
    }

    fn finalize_file(&self) -> StreamedDownloadResult<PathBuf> {
        // Flush and close the `.part` file handle before verification and atomic rename.
        {
            let mut guard = self.file.lock().unwrap();
            if let Some(mut file) = guard.take() {
                file.flush()?;
            }
        }

        if let Some(expected) = &self.config.expected_sha256 {
            let actual = sha256_file(&self.part_path)
                .map_err(|err| StreamedDownloadError::Io(std::io::Error::other(err.to_string())))?;
            if !const_time_eq(&actual, expected) {
                // Re-open the `.part` file handle so state stays inspectable, leave target untouched.
                if let Ok(reopened) = File::options().read(true).write(true).open(&self.part_path) {
                    *self.file.lock().unwrap() = Some(reopened);
                }
                return Err(StreamedDownloadError::Sha256Mismatch {
                    expected: sha256_hex(expected),
                    actual: sha256_hex(&actual),
                });
            }
        }

        fs::rename(&self.part_path, &self.config.target_path)?;
        let _ = fs::remove_file(&self.meta_path);

        let mut state = self.state.lock().unwrap();
        state.finished = true;
        self.notify_state_change(&state);

        Ok(self.config.target_path.clone())
    }
}
