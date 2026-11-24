//! Streamed file downloader, random-access priority queue, and download speed estimator.
//!
//! Port of `Telegram/SourceFiles/storage/streamed_file_downloader.cpp`,
//! `Telegram/SourceFiles/media/streaming/media_streaming_loader.cpp`, and
//! `Telegram/SourceFiles/media/streaming/media_streaming_loader_mtproto.cpp`.

use crate::error::{MediaError, Result};
use crate::transfer::DOWNLOAD_PART_SIZE;
use crate::transfer::download::PartAssembler;
use std::collections::{BTreeSet, VecDeque};

/// Fixed 128 KiB streaming part size (`Media::Streaming::Loader::kPartSize`).
pub const STREAMING_PART_SIZE: u64 = DOWNLOAD_PART_SIZE as u64;

/// Maximum parts requested concurrently by `StreamedFileDownloader` (`kRequestPartsCount`).
pub const MAX_STREAMED_REQUEST_PARTS: usize = 32;

/// Initial window (ms) before emitting reliable speed estimates (`kInitialStatsWait`).
pub const INITIAL_STATS_WAIT_MS: u64 = 5_000;

/// Maximum clamped speed estimate in bytes/sec (64 MiB/s, `media_streaming_loader_mtproto.cpp:214`).
pub const MAX_SPEED_ESTIMATE_BPS: u64 = 64 * 1024 * 1024;

/// A single loaded part from a streaming loader (`Media::Streaming::LoadedPart`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedPart {
    /// Byte offset (multiple of [`STREAMING_PART_SIZE`]).
    pub offset: u64,
    /// Part bytes.
    pub bytes: Vec<u8>,
}

impl LoadedPart {
    /// Validates that the loaded part has either a full 128 KiB size or reaches exact `file_size`.
    #[must_use]
    pub fn is_valid(&self, file_size: u64) -> bool {
        self.offset < file_size
            && self.offset.is_multiple_of(STREAMING_PART_SIZE)
            && (self.bytes.len() as u64 == STREAMING_PART_SIZE
                || self.offset + self.bytes.len() as u64 == file_size)
    }
}

/// Entry inside [`StreamingPriorityQueue`]: higher `priority` sorts first, then lower `offset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PriorityEntry {
    priority: i32,
    offset: u64,
}

impl Ord for PriorityEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Higher priority first, then smaller offset first (`media_streaming_loader.cpp:19-29`).
        other
            .priority
            .cmp(&self.priority)
            .then_with(|| self.offset.cmp(&other.offset))
    }
}

impl PartialOrd for PriorityEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Random-access streaming part priority queue (`Media::Streaming::PriorityQueue`).
///
/// Calling [`StreamingPriorityQueue::reset_priorities`] bumps the internal priority generation
/// so newly requested offsets (e.g., after a video seek) jump ahead of older queued offsets.
#[derive(Debug, Default, Clone)]
pub struct StreamingPriorityQueue {
    data: BTreeSet<PriorityEntry>,
    current_priority: i32,
}

impl StreamingPriorityQueue {
    /// Creates an empty priority queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `offset` at the current priority, or promotes an existing entry to the current priority.
    /// Returns `true` if inserted or promoted.
    pub fn add(&mut self, offset: u64) -> bool {
        if let Some(existing) = self.data.iter().find(|e| e.offset == offset).copied() {
            if existing.priority != self.current_priority {
                self.data.remove(&existing);
                self.data.insert(PriorityEntry {
                    priority: self.current_priority,
                    offset,
                });
                return true;
            }
            return false;
        }
        self.data.insert(PriorityEntry {
            priority: self.current_priority,
            offset,
        });
        true
    }

    /// Removes `offset` if queued. Returns `true` if present.
    pub fn remove(&mut self, offset: u64) -> bool {
        if let Some(existing) = self.data.iter().find(|e| e.offset == offset).copied() {
            self.data.remove(&existing)
        } else {
            false
        }
    }

    /// Increments the priority generation so subsequent `add` calls outrank older entries.
    pub fn reset_priorities(&mut self) {
        self.current_priority = self.current_priority.saturating_add(1);
    }

    /// Returns `true` if empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Peeks the highest-priority offset.
    #[must_use]
    pub fn front(&self) -> Option<u64> {
        self.data.first().map(|e| e.offset)
    }

    /// Pops and returns the highest-priority offset.
    pub fn take(&mut self) -> Option<u64> {
        self.data.pop_first().map(|e| e.offset)
    }

    /// Removes and returns all queued offsets in `[from, till)`.
    pub fn take_in_range(&mut self, from: u64, till: u64) -> Vec<u64> {
        let mut taken = Vec::new();
        let to_remove: Vec<PriorityEntry> = self
            .data
            .iter()
            .copied()
            .filter(|e| e.offset >= from && e.offset < till)
            .collect();
        for entry in to_remove {
            self.data.remove(&entry);
            taken.push(entry.offset);
        }
        taken.sort_unstable();
        taken
    }

    /// Removes all entries.
    pub fn clear(&mut self) {
        self.data.clear();
    }
}

/// State machine mirroring `Storage::StreamedFileDownloader` (`streamed_file_downloader.cpp`).
///
/// Tracks which 128 KiB parts of a streamed file have been persisted, bounds in-flight
/// part requests to [`MAX_STREAMED_REQUEST_PARTS`] (32), supports random-access `read_loaded_part`,
/// and finalizes once all parts are saved.
#[derive(Debug, Clone)]
pub struct StreamedFileDownloaderState {
    full_size: u64,
    parts_count: usize,
    part_is_saved: Vec<bool>,
    next_part_index: usize,
    parts_requested: usize,
    parts_saved: usize,
    cancelled: bool,
    finished: bool,
    assembler: PartAssembler,
}

impl StreamedFileDownloaderState {
    /// Creates a new state machine for a file of `full_size` bytes.
    pub fn new(full_size: u64) -> Result<Self> {
        if full_size == 0 {
            return Err(MediaError::InvalidTransferParam(
                "streamed downloader requires full_size > 0".into(),
            ));
        }
        let parts_count = full_size.div_ceil(STREAMING_PART_SIZE) as usize;
        Ok(Self {
            full_size,
            parts_count,
            part_is_saved: vec![false; parts_count],
            next_part_index: 0,
            parts_requested: 0,
            parts_saved: 0,
            cancelled: false,
            finished: false,
            assembler: PartAssembler::with_capacity(full_size.min(4 * 1024 * 1024) as usize),
        })
    }

    /// Total file size in bytes.
    #[must_use]
    pub const fn full_size(&self) -> u64 {
        self.full_size
    }

    /// Total number of 128 KiB parts.
    #[must_use]
    pub const fn parts_count(&self) -> usize {
        self.parts_count
    }

    /// Number of saved parts so far.
    #[must_use]
    pub const fn parts_saved(&self) -> usize {
        self.parts_saved
    }

    /// Returns `true` when all parts have been saved.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        self.finished
    }

    /// Returns `true` if cancelled.
    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// Collects up to [`MAX_STREAMED_REQUEST_PARTS`] next unsaved part offsets to request
    /// (`StreamedFileDownloader::requestParts`).
    pub fn take_next_request_offsets(&mut self) -> Vec<u64> {
        let mut offsets = Vec::new();
        while !self.finished
            && !self.cancelled
            && self.next_part_index < self.parts_count
            && self.parts_requested < MAX_STREAMED_REQUEST_PARTS
        {
            let Some(rel) = self.part_is_saved[self.next_part_index..]
                .iter()
                .position(|&saved| !saved)
            else {
                self.next_part_index = self.parts_count;
                break;
            };
            let idx = self.next_part_index + rel;
            self.next_part_index = idx + 1;
            self.parts_requested += 1;
            offsets.push((idx as u64) * STREAMING_PART_SIZE);
        }
        offsets
    }

    /// Saves a loaded part into the assembler (`StreamedFileDownloader::savePart`).
    ///
    /// Returns `Ok(true)` if this part completed the entire file.
    pub fn save_part(&mut self, part: &LoadedPart) -> Result<bool> {
        if self.finished || self.cancelled {
            return Ok(self.finished);
        }
        if !part.is_valid(self.full_size) {
            return Err(MediaError::InvalidTransferParam(format!(
                "invalid streamed part at offset {} len {} for full_size {}",
                part.offset,
                part.bytes.len(),
                self.full_size
            )));
        }
        let idx = (part.offset / STREAMING_PART_SIZE) as usize;
        if self.part_is_saved[idx] {
            return Ok(false);
        }
        self.part_is_saved[idx] = true;
        self.parts_saved += 1;
        if idx < self.next_part_index {
            self.parts_requested = self.parts_requested.saturating_sub(1);
        }
        self.assembler.write_part(part.offset, &part.bytes)?;
        if self.parts_saved == self.parts_count {
            self.finished = true;
            return Ok(true);
        }
        Ok(false)
    }

    /// Reads an already-saved part back at aligned `offset` (`StreamedFileDownloader::readLoadedPart`).
    #[must_use]
    pub fn read_loaded_part(&self, offset: u64) -> Option<&[u8]> {
        if offset >= self.full_size || !offset.is_multiple_of(STREAMING_PART_SIZE) {
            return None;
        }
        let idx = (offset / STREAMING_PART_SIZE) as usize;
        if !*self.part_is_saved.get(idx)? {
            return None;
        }
        let len = (self.full_size - offset).min(STREAMING_PART_SIZE) as usize;
        self.assembler.read_back(offset, len)
    }

    /// Cancels the streamed downloader, resetting in-flight tracking (`cancelHook`).
    pub fn cancel(&mut self) {
        self.cancelled = true;
        self.parts_requested = 0;
        self.next_part_index = 0;
    }

    /// Consumes the state machine and returns the assembled file bytes once finished.
    pub fn into_bytes(self) -> Result<Vec<u8>> {
        if !self.finished {
            return Err(MediaError::InvalidTransferParam(format!(
                "streamed download incomplete: {}/{} parts",
                self.parts_saved, self.parts_count
            )));
        }
        self.assembler.finalize()
    }
}

// ============================================================================
// Speed estimator (Media::Streaming::LoaderMtproto::checkStats)
// ============================================================================

/// Estimated download throughput (`Media::Streaming::SpeedEstimate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeedEstimate {
    /// Estimated throughput in bytes per second (clamped to [`MAX_SPEED_ESTIMATE_BPS`]).
    pub bytes_per_second: u64,
    /// `true` if fewer than 3 parts have finished in the sliding window.
    pub unreliable: bool,
}

#[derive(Debug, Clone, Copy)]
struct SpeedSample {
    start_ms: u64,
    end_ms: Option<u64>,
    offset: u64,
}

/// Sliding-window throughput estimator counting only active-request intervals,
/// exact port of `LoaderMtproto::checkStats` (`media_streaming_loader_mtproto.cpp:173-218`).
#[derive(Debug, Default, Clone)]
pub struct SpeedEstimator {
    samples: VecDeque<SpeedSample>,
}

impl SpeedEstimator {
    /// Creates an empty speed estimator.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the start of a 128 KiB part request at `now_ms`.
    pub fn record_start(&mut self, offset: u64, now_ms: u64) {
        self.samples.push_back(SpeedSample {
            start_ms: now_ms,
            end_ms: None,
            offset,
        });
    }

    /// Records completion of the part request at `offset` at `now_ms`.
    pub fn record_finish(&mut self, offset: u64, now_ms: u64) {
        for s in &mut self.samples {
            if s.offset == offset && s.end_ms.is_none() && s.start_ms < now_ms {
                s.end_ms = Some(now_ms);
                break;
            }
        }
    }

    /// Prunes entries older than `INITIAL_STATS_WAIT_MS` and computes the current [`SpeedEstimate`].
    pub fn estimate(&mut self, now_ms: u64) -> Option<SpeedEstimate> {
        let from = now_ms.saturating_sub(INITIAL_STATS_WAIT_MS);
        while let Some(front) = self.samples.front() {
            if front.start_ms >= from {
                break;
            }
            if front.end_ms.is_some_and(|e| e < from) {
                self.samples.pop_front();
            } else {
                break;
            }
        }
        let first = self.samples.front()?;
        let mut counted_till = first.start_ms;
        let mut duration_ms = 0u64;
        let mut received = 0u64;

        for s in &self.samples {
            if s.start_ms > counted_till {
                counted_till = s.start_ms;
            }
            let till = s.end_ms.unwrap_or(now_ms);
            if till > counted_till {
                duration_ms += till - counted_till;
                counted_till = till;
            }
            if s.end_ms.is_some() {
                received += STREAMING_PART_SIZE;
            }
        }

        if duration_ms == 0 {
            return None;
        }
        let bps = ((received as u128 * 1000) / (duration_ms as u128))
            .min(MAX_SPEED_ESTIMATE_BPS as u128) as u64;
        Some(SpeedEstimate {
            bytes_per_second: bps,
            unreliable: received < 3 * STREAMING_PART_SIZE,
        })
    }
}
