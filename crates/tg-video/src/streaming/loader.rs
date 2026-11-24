//! Byte-range loader trait, priority queue, local/memory loaders, and bandwidth estimator.
//!
//! Port of `Telegram/SourceFiles/media/streaming/media_streaming_loader.cpp`,
//! `media_streaming_loader_local.cpp`, and `media_streaming_loader_mtproto.cpp`.

use std::collections::{BTreeSet, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::error::{Result, StreamingError};
use crate::streaming::cache::CacheKey;
use crate::types::SpeedEstimate;

/// Download part size in bytes (`Loader::kPartSize` = 128 KB).
pub const PART_SIZE: u64 = 128 * 1024;

/// Maximum Telegram document size (`8000 * 512 KB` = ~4 GB).
pub const MAX_FILE_SIZE: u64 = 8000 * 512 * 1024;

/// Maximum reported download speed cap (`64 MiB/s`).
const MAX_SPEED_BYTES_PER_SEC: u64 = 64 * 1024 * 1024;

/// Rolling stats window in milliseconds (`kInitialStatsWait` = 5 s).
const STATS_WINDOW_MS: i64 = 5_000;

/// Minimum bytes in window before speed estimate is marked reliable (`3 * 128 KB`).
const RELIABLE_SPEED_MIN_BYTES: u64 = 3 * PART_SIZE;

/// A single fetched part of a media file (mirrors `Media::Streaming::LoadedPart`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedPart {
    /// Byte offset from the start of the file (`None` represents `kFailedOffset`).
    pub offset: Option<u64>,
    /// Raw bytes for this part.
    pub bytes: Vec<u8>,
}

impl LoadedPart {
    /// Creates a valid loaded part.
    #[must_use]
    pub fn new(offset: u64, bytes: Vec<u8>) -> Self {
        Self {
            offset: Some(offset),
            bytes,
        }
    }

    /// Creates a failure sentinel (`kFailedOffset`).
    #[must_use]
    pub const fn failed() -> Self {
        Self {
            offset: None,
            bytes: Vec::new(),
        }
    }

    /// Validates that the part has either `PART_SIZE` bytes or reaches EOF exactly
    /// (mirrors `LoadedPart::valid`).
    #[must_use]
    pub fn is_valid(&self, total_size: u64) -> bool {
        let Some(offset) = self.offset else {
            return false;
        };
        let len = self.bytes.len() as u64;
        len == PART_SIZE || offset.saturating_add(len) == total_size
    }
}

/// Priority queue of requested byte offsets (mirrors `Media::Streaming::PriorityQueue`).
///
/// Higher `priority` generation is served first; within the same generation, smaller
/// byte offsets are served first. Calling [`PriorityQueue::reset_priorities`] bumps the
/// generation so newly added offsets (e.g. after a seek) jump ahead of older prefetches.
#[derive(Debug, Clone, Default)]
pub struct PriorityQueue {
    entries: BTreeSet<PriorityEntry>,
    priority: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PriorityEntry {
    value: u64,
    priority: i32,
}

impl Ord for PriorityEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Higher priority comes first; tie-break by smaller offset first.
        other
            .priority
            .cmp(&self.priority)
            .then_with(|| self.value.cmp(&other.value))
    }
}

impl PartialOrd for PriorityEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PriorityQueue {
    /// Creates an empty priority queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or promotes `value` to the current priority generation.
    /// Returns `true` if inserted or re-prioritized, `false` if already at current priority.
    pub fn add(&mut self, value: u64) -> bool {
        if let Some(&existing) = self.entries.iter().find(|e| e.value == value) {
            if existing.priority != self.priority {
                self.entries.remove(&existing);
                self.entries.insert(PriorityEntry {
                    value,
                    priority: self.priority,
                });
                return true;
            }
            return false;
        }
        self.entries.insert(PriorityEntry {
            value,
            priority: self.priority,
        });
        true
    }

    /// Removes `value` from the queue. Returns `true` if it was present.
    pub fn remove(&mut self, value: u64) -> bool {
        if let Some(&existing) = self.entries.iter().find(|e| e.value == value) {
            self.entries.remove(&existing);
            true
        } else {
            false
        }
    }

    /// Increments the priority generation so subsequent `add` calls preempt existing ones.
    pub fn reset_priorities(&mut self) {
        self.priority = self.priority.saturating_add(1);
    }

    /// Returns `true` when no offsets are queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the number of queued offsets.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Peeks at the highest-priority offset.
    #[must_use]
    pub fn front(&self) -> Option<u64> {
        self.entries.first().map(|e| e.value)
    }

    /// Removes and returns the highest-priority offset.
    pub fn take(&mut self) -> Option<u64> {
        self.entries.pop_first().map(|e| e.value)
    }

    /// Removes and returns all queued offsets in `[from, till)`.
    pub fn take_in_range(&mut self, from: u64, till: u64) -> Vec<u64> {
        let mut taken = Vec::new();
        self.entries.retain(|e| {
            if e.value >= from && e.value < till {
                taken.push(e.value);
                false
            } else {
                true
            }
        });
        taken.sort_unstable();
        taken
    }

    /// Clears all queued offsets.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Byte-range data source trait used by [`crate::streaming::StreamingReader`].
///
/// `tg-api` implements this trait over MTProto `upload.getFile` requests; local files
/// and memory buffers use [`LocalFileLoader`] and [`MemoryRangeLoader`].
pub trait RangeLoader: Send + Sync {
    /// Total file size in bytes.
    fn size(&self) -> u64;

    /// Base cache key for persistent slice caching (`CacheKey::default()` for local loaders).
    fn base_cache_key(&self) -> CacheKey {
        CacheKey::default()
    }

    /// Loads up to `len` bytes starting at `offset` (typically `PART_SIZE` = 128 KB).
    /// Must return `min(len, size - offset)` bytes or fail with [`StreamingError::LoadFailed`].
    fn load_range(&self, offset: u64, len: usize) -> Result<Vec<u8>>;

    /// Optional hint to prefetch a part asynchronously (default no-op).
    fn prefetch(&self, _offset: u64, _len: usize) {}

    /// Optional hint to cancel an in-flight request at `offset` (default no-op).
    fn cancel(&self, _offset: u64) {}

    /// Resets request priority generation on seek (default no-op).
    fn reset_priorities(&self) {}

    /// Updates loader scheduling priority (default no-op).
    fn set_priority(&self, _priority: i32) {}

    /// Stops all pending remote requests (default no-op).
    fn stop(&self) {}
}

/// Validates a local file size against Telegram's `kMaxFileSize`.
#[must_use]
pub const fn validate_local_size(size: u64) -> Option<u64> {
    if size > 0 && size <= MAX_FILE_SIZE {
        Some(size)
    } else {
        None
    }
}

/// Local file [`RangeLoader`] porting `Media::Streaming::LoaderLocal`.
#[derive(Debug)]
pub struct LocalFileLoader {
    file: Mutex<File>,
    size: u64,
}

impl LocalFileLoader {
    /// Opens a local file at `path` for streaming reads.
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        let size = validate_local_size(len).ok_or(StreamingError::OpenFailed)?;
        Ok(Self {
            file: Mutex::new(file),
            size,
        })
    }
}

impl RangeLoader for LocalFileLoader {
    fn size(&self) -> u64 {
        self.size
    }

    fn load_range(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        if offset >= self.size {
            return Err(StreamingError::LoadFailed.into());
        }
        let want = (self.size - offset).min(len as u64) as usize;
        let mut guard = self.file.lock().map_err(|_| StreamingError::LoadFailed)?;
        guard.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; want];
        guard.read_exact(&mut buf)?;
        Ok(buf)
    }
}

/// In-memory byte slice [`RangeLoader`] (mirrors `MakeBytesLoader`).
#[derive(Debug, Clone)]
pub struct MemoryRangeLoader {
    data: Arc<Vec<u8>>,
    cache_key: CacheKey,
}

impl MemoryRangeLoader {
    /// Creates a local (non-cached) memory range loader.
    pub fn new(data: Vec<u8>) -> Result<Self> {
        let _ = validate_local_size(data.len() as u64).ok_or(StreamingError::OpenFailed)?;
        Ok(Self {
            data: Arc::new(data),
            cache_key: CacheKey::default(),
        })
    }

    /// Creates a memory range loader simulating a remote MTProto file with a `CacheKey`.
    pub fn with_cache_key(data: Vec<u8>, cache_key: CacheKey) -> Result<Self> {
        let _ = validate_local_size(data.len() as u64).ok_or(StreamingError::OpenFailed)?;
        Ok(Self {
            data: Arc::new(data),
            cache_key,
        })
    }
}

impl RangeLoader for MemoryRangeLoader {
    fn size(&self) -> u64 {
        self.data.len() as u64
    }

    fn base_cache_key(&self) -> CacheKey {
        self.cache_key
    }

    fn load_range(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let off = usize::try_from(offset).map_err(|_| StreamingError::LoadFailed)?;
        if off >= self.data.len() {
            return Err(StreamingError::LoadFailed.into());
        }
        let end = off.saturating_add(len).min(self.data.len());
        Ok(self.data[off..end].to_vec())
    }
}

#[derive(Debug, Clone, Copy)]
struct RequestStat {
    start_ms: i64,
    end_ms: Option<i64>,
    offset: u64,
    bytes: u64,
}

/// Rolling download bandwidth estimator ported from `LoaderMtproto::checkStats`
/// (`media_streaming_loader_mtproto.cpp`).
#[derive(Debug, Default)]
pub struct SpeedEstimator {
    stats: VecDeque<RequestStat>,
    first_request_start_ms: Option<i64>,
}

impl SpeedEstimator {
    /// Creates a new speed estimator.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the start of a range request at `now_ms`.
    pub fn on_request_start(&mut self, offset: u64, now_ms: i64) {
        if self.first_request_start_ms.is_none() {
            self.first_request_start_ms = Some(now_ms);
        }
        self.stats.push_back(RequestStat {
            start_ms: now_ms,
            end_ms: None,
            offset,
            bytes: PART_SIZE,
        });
    }

    /// Records completion of the request for `offset` with `bytes_received` at `now_ms`.
    pub fn on_request_complete(&mut self, offset: u64, bytes_received: u64, now_ms: i64) {
        for entry in &mut self.stats {
            if entry.offset == offset && entry.end_ms.is_none() && entry.start_ms <= now_ms {
                entry.end_ms = Some(now_ms);
                entry.bytes = bytes_received;
                break;
            }
        }
    }

    /// Computes the rolling speed estimate over the last 5 seconds of active download time,
    /// ignoring idle gaps when no requests were in flight.
    pub fn estimate(&mut self, now_ms: i64) -> Option<SpeedEstimate> {
        let from = now_ms - STATS_WINDOW_MS;
        while let Some(front) = self.stats.front() {
            if front.start_ms >= from {
                break;
            }
            if let Some(end) = front.end_ms
                && end < from
            {
                self.stats.pop_front();
                continue;
            }
            break;
        }
        let first = self.stats.front()?;
        let mut counted_till = first.start_ms;
        let mut active_duration_ms: i64 = 0;
        let mut received_bytes: u64 = 0;

        for entry in &self.stats {
            if entry.start_ms > counted_till {
                counted_till = entry.start_ms;
            }
            let till = entry.end_ms.unwrap_or(now_ms);
            if till > counted_till {
                active_duration_ms += till - counted_till;
                counted_till = till;
            }
            if entry.end_ms.is_some() {
                received_bytes = received_bytes.saturating_add(entry.bytes);
            }
        }

        if active_duration_ms <= 0 {
            return None;
        }
        let bps = (received_bytes.saturating_mul(1000) / (active_duration_ms as u64))
            .min(MAX_SPEED_BYTES_PER_SEC) as u32;
        Some(SpeedEstimate {
            bytes_per_second: bps,
            unreliable: received_bytes < RELIABLE_SPEED_MIN_BYTES,
        })
    }
}
