//! Download manager, per-DC session balancer, priority queue, out-of-order part assembler,
//! CDN decryption/hash-verification flow, and `mtpFileLoader` state machine.
//!
//! Port of `Telegram/SourceFiles/storage/download_manager_mtproto.cpp`,
//! `Telegram/SourceFiles/storage/file_download.cpp`, and
//! `Telegram/SourceFiles/storage/file_download_mtproto.cpp`.

use crate::cache::{LocalFileCache, StorageKey, decode_partial_payload, encode_partial_payload};
use crate::error::{MediaError, Result};
use crate::transfer::{
    CdnHashVerifier, CdnRedirect, CheckCdnHashResult, DOWNLOAD_PART_SIZE, FileOrigin,
    GetCdnFileRequest, GetCdnFileResponse, GetFileRequest, GetFileResponse, GetWebFileRequest,
    MediaInvoker, TransferLocation, apply_cdn_aes_ctr, join_all, validate_get_file_request,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Initial in-flight byte budget per DC session (`kStartWaitedInSession` = 4 * 128 KiB).
pub const START_WAITED_IN_SESSION: usize = 4 * (DOWNLOAD_PART_SIZE as usize);

/// Maximum in-flight byte budget per DC session (`kMaxWaitedInSession` = 16 * 128 KiB).
pub const MAX_WAITED_IN_SESSION: usize = 16 * (DOWNLOAD_PART_SIZE as usize);

/// Initial number of download sessions per DC (`kStartSessionsCount`).
pub const START_SESSIONS_COUNT: usize = 1;

/// Maximum number of parallel download sessions per DC (`kMaxSessionsCount`).
pub const MAX_SESSIONS_COUNT: usize = 8;

/// Maximum tracked session-removal count for backoff (`kMaxTrackedSessionRemoves`).
pub const MAX_TRACKED_SESSION_REMOVES: usize = 64;

/// Delay multiplier in milliseconds before re-adding a removed session (`kRetryAddSessionTimeout`).
pub const RETRY_ADD_SESSION_TIMEOUT_MS: u64 = 8_000;

/// Required successes in every active session before adding a new session (`kRetryAddSessionSuccesses`).
pub const RETRY_ADD_SESSION_SUCCESSES: usize = 3;

/// Maximum tracked successes per session (`kMaxTrackedSuccesses`).
pub const MAX_TRACKED_SUCCESSES: usize = RETRY_ADD_SESSION_SUCCESSES * MAX_TRACKED_SESSION_REMOVES;

/// Number of timeouts before shedding a download session (`kRemoveSessionAfterTimeouts`).
pub const REMOVE_SESSION_AFTER_TIMEOUTS: usize = 4;

/// Request duration threshold (ms) treated as a session timeout (`kBadRequestDurationThreshold`).
pub const BAD_REQUEST_DURATION_MS: u64 = 8_000;

/// Timeout (ms) before idle download sessions are closed (`kKillSessionTimeout`).
pub const KILL_SESSION_TIMEOUT_MS: u64 = 15_000;

/// Delay (ms) before resetting default priority 0 tasks to -1 (`kResetDownloadPrioritiesTimeout`).
pub const RESET_DOWNLOAD_PRIORITIES_TIMEOUT_MS: u64 = 200;

// ============================================================================
// Per-DC download priority queue (DownloadManagerMtproto::Queue)
// ============================================================================

/// Enqueued task entry in [`DownloadQueue`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EnqueuedTask {
    task_id: u64,
    priority: i32,
}

/// Priority queue for a single DC mirroring `DownloadManagerMtproto::Queue`
/// (`download_manager_mtproto.cpp:43-110`).
///
/// Higher `priority` values are served first; tasks with equal priority are ordered LIFO on
/// re-enqueue (newest highest-priority request jumps ahead of older equal-priority requests).
#[derive(Debug, Default, Clone)]
pub struct DownloadQueue {
    tasks: Vec<EnqueuedTask>,
}

impl DownloadQueue {
    /// Creates an empty download queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or re-prioritizes `task_id` at `priority`.
    pub fn enqueue(&mut self, task_id: u64, priority: i32) {
        let position = self
            .tasks
            .iter()
            .position(|t| t.priority <= priority)
            .unwrap_or(self.tasks.len());

        if let Some(now) = self.tasks.iter().position(|t| t.task_id == task_id) {
            self.tasks[now].priority = priority;
            if position < now {
                self.tasks[position..=now].rotate_right(1);
            } else if position > now + 1 {
                self.tasks[now..position].rotate_left(1);
            }
        } else {
            self.tasks
                .insert(position, EnqueuedTask { task_id, priority });
        }
    }

    /// Removes `task_id` from the queue.
    pub fn remove(&mut self, task_id: u64) {
        self.tasks.retain(|t| t.task_id != task_id);
    }

    /// Demotes all priority-0 tasks to priority -1 (`Queue::resetGeneration`).
    pub fn reset_generation(&mut self) {
        let Some(from) = self.tasks.iter().position(|t| t.priority == 0) else {
            return;
        };
        for entry in &mut self.tasks[from..] {
            if entry.priority != 0 {
                break;
            }
            entry.priority = -1;
        }
    }

    /// Returns `true` if no tasks are queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Returns the next task ID for which `is_ready(task_id)` returns `true`.
    ///
    /// When `only_highest_priority` is `true` and the front task has `priority > 0`,
    /// only tasks sharing that highest priority are considered.
    #[must_use]
    pub fn next_task(
        &self,
        only_highest_priority: bool,
        is_ready: impl Fn(u64) -> bool,
    ) -> Option<u64> {
        let first = self.tasks.first()?;
        let highest = first.priority;
        let limit = if only_highest_priority && highest > 0 {
            self.tasks
                .iter()
                .position(|t| t.priority != highest)
                .unwrap_or(self.tasks.len())
        } else {
            self.tasks.len()
        };
        self.tasks[..limit]
            .iter()
            .find(|t| is_ready(t.task_id))
            .map(|t| t.task_id)
    }

    /// Returns task IDs in priority order (for inspection/tests).
    #[must_use]
    pub fn ordered_ids(&self) -> Vec<(u64, i32)> {
        self.tasks.iter().map(|t| (t.task_id, t.priority)).collect()
    }
}

// ============================================================================
// Per-DC session balancer (DownloadManagerMtproto::DcBalanceData)
// ============================================================================

/// Per-session bandwidth/in-flight accounting (`DcSessionBalanceData`).
#[derive(Debug, Clone)]
pub struct DcSessionBalance {
    /// Currently in-flight requested bytes on this session.
    pub requested: usize,
    /// Consecutive successful requests since the last timeout in this DC.
    pub successes: usize,
    /// Dynamic in-flight byte ceiling for this session (`START_WAITED_IN_SESSION..=MAX_WAITED_IN_SESSION`).
    pub max_waited_amount: usize,
}

impl Default for DcSessionBalance {
    fn default() -> Self {
        Self {
            requested: 0,
            successes: 0,
            max_waited_amount: START_WAITED_IN_SESSION,
        }
    }
}

/// Per-DC dynamic session pool state (`DcBalanceData` in `download_manager_mtproto.cpp`).
#[derive(Debug, Clone)]
pub struct DcBalanceState {
    /// Active sessions on this DC (1..=8).
    pub sessions: Vec<DcSessionBalance>,
    /// Timestamp (ms) of the last session removal (0 if never removed).
    pub last_session_remove_ms: u64,
    /// Index of the most recently removed session.
    pub session_remove_index: usize,
    /// Consecutive times `session_remove_index` was removed (for backoff).
    pub session_remove_times: usize,
    /// Timeout counter since all sessions reached required successes.
    pub timeouts: usize,
    /// Total bytes currently requested across all sessions in this DC.
    pub total_requested: usize,
}

impl Default for DcBalanceState {
    fn default() -> Self {
        Self {
            sessions: vec![DcSessionBalance::default(); START_SESSIONS_COUNT],
            last_session_remove_ms: 0,
            session_remove_index: 0,
            session_remove_times: 0,
            timeouts: 0,
            total_requested: 0,
        }
    }
}

impl DcBalanceState {
    /// Picks the session index with the lowest in-flight bytes that still has room for `part_size`.
    ///
    /// Mirrors `DownloadManagerMtproto::trySendNextPart` (`download_manager_mtproto.cpp:183-193`).
    #[must_use]
    pub fn best_session_for_part(&self, part_size: usize) -> Option<usize> {
        let (idx, best) = self.sessions.iter().enumerate().min_by_key(|(_, s)| {
            if s.requested < s.max_waited_amount {
                s.requested
            } else {
                MAX_WAITED_IN_SESSION
            }
        })?;
        (best.requested + part_size <= best.max_waited_amount).then_some(idx)
    }

    /// Picks the session index with the smallest `requested` count (`chooseSessionIndex`).
    #[must_use]
    pub fn least_loaded_session(&self) -> usize {
        self.sessions
            .iter()
            .enumerate()
            .min_by_key(|(_, s)| s.requested)
            .map_or(0, |(idx, _)| idx)
    }

    /// Adjusts the requested byte counters for `session_index` by `delta`.
    pub fn change_requested(&mut self, session_index: usize, delta: isize) -> usize {
        if let Some(session) = self.sessions.get_mut(session_index) {
            if delta >= 0 {
                let d = delta as usize;
                session.requested = session.requested.saturating_add(d);
                self.total_requested = self.total_requested.saturating_add(d);
            } else {
                let d = delta.unsigned_abs();
                session.requested = session.requested.saturating_sub(d);
                self.total_requested = self.total_requested.saturating_sub(d);
            }
            session.requested
        } else {
            0
        }
    }

    /// Updates dynamic window and session count after a successful part download
    /// (`DownloadManagerMtproto::requestSucceeded`, `download_manager_mtproto.cpp:229-299`).
    ///
    /// Returns `true` if a new session was added.
    pub fn record_request_succeeded(
        &mut self,
        session_index: usize,
        amount_at_start: usize,
        start_time_ms: u64,
        now_ms: u64,
        part_size: usize,
    ) -> bool {
        let Some(data) = self.sessions.get_mut(session_index) else {
            return false;
        };
        let overloaded = start_time_ms <= self.last_session_remove_ms
            || amount_at_start > data.max_waited_amount;
        if overloaded {
            return false;
        }
        let duration = now_ms.saturating_sub(start_time_ms);
        if duration >= BAD_REQUEST_DURATION_MS {
            self.record_timeout(session_index, now_ms);
            return false;
        }
        if amount_at_start == data.max_waited_amount
            && data.max_waited_amount < MAX_WAITED_IN_SESSION
        {
            data.max_waited_amount =
                (data.max_waited_amount + part_size).min(MAX_WAITED_IN_SESSION);
        }
        data.successes = (data.successes + 1).min(MAX_TRACKED_SUCCESSES);

        let required = (self.session_remove_times + 1) * RETRY_ADD_SESSION_SUCCESSES;
        if self.sessions.iter().any(|s| s.successes < required) {
            return false;
        }
        for s in &mut self.sessions {
            s.successes = 0;
        }
        if self.timeouts > 0 {
            self.timeouts -= 1;
            return false;
        }
        if self.sessions.len() >= MAX_SESSIONS_COUNT {
            return false;
        }
        let delay = ((self.session_remove_times + 1) as u64) * RETRY_ADD_SESSION_TIMEOUT_MS;
        if self.last_session_remove_ms != 0 && now_ms < self.last_session_remove_ms + delay {
            return false;
        }
        self.sessions.push(DcSessionBalance::default());
        true
    }

    /// Records a timeout on `session_index`; sheds the highest session index after
    /// [`REMOVE_SESSION_AFTER_TIMEOUTS`] timeouts (`sessionTimedOut` / `removeSession`).
    ///
    /// Returns `Some(removed_session_index)` if a session was removed.
    pub fn record_timeout(&mut self, session_index: usize, now_ms: u64) -> Option<usize> {
        if session_index >= self.sessions.len() {
            return None;
        }
        for s in &mut self.sessions {
            s.successes = 0;
        }
        // Match `DownloadManagerMtproto::sessionTimedOut` (`storage/download_manager_mtproto.cpp:325-328`):
        // short-circuit when already at `kStartSessionsCount` so `timeouts` is NOT incremented
        // while only a single session is open.
        if self.sessions.len() <= START_SESSIONS_COUNT {
            return None;
        }
        self.timeouts += 1;
        if self.timeouts < REMOVE_SESSION_AFTER_TIMEOUTS {
            return None;
        }
        self.timeouts = 0;
        let removed_index = self.sessions.len() - 1;
        if self.session_remove_index == removed_index {
            self.session_remove_times =
                (self.session_remove_times + 1).min(MAX_TRACKED_SESSION_REMOVES);
        } else {
            self.session_remove_index = removed_index;
            self.session_remove_times = 1;
        }
        let removed = self.sessions.pop()?;
        self.total_requested = self.total_requested.saturating_sub(removed.requested);
        self.last_session_remove_ms = now_ms;
        Some(removed_index)
    }
}

/// Multi-DC download scheduler combining [`DownloadQueue`] and [`DcBalanceState`] per DC.
#[derive(Debug, Default)]
pub struct DownloadScheduler {
    queues: HashMap<i32, DownloadQueue>,
    balances: HashMap<i32, DcBalanceState>,
}

impl DownloadScheduler {
    /// Creates a new empty scheduler.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enqueues `task_id` on `dc_id` with `priority`.
    pub fn enqueue(&mut self, dc_id: i32, task_id: u64, priority: i32) {
        self.balances.entry(dc_id).or_default();
        self.queues
            .entry(dc_id)
            .or_default()
            .enqueue(task_id, priority);
    }

    /// Removes `task_id` from `dc_id`'s queue.
    pub fn remove(&mut self, dc_id: i32, task_id: u64) {
        if let Some(q) = self.queues.get_mut(&dc_id) {
            q.remove(task_id);
        }
    }

    /// Resets generation (priority 0 -> -1) across all DC queues.
    pub fn reset_generation(&mut self) {
        for q in self.queues.values_mut() {
            q.reset_generation();
        }
    }

    /// Schedules the next `(task_id, session_index)` on `dc_id`, if both a session slot and a
    /// ready task are available.
    pub fn poll_next(
        &mut self,
        dc_id: i32,
        part_size: usize,
        is_ready: impl Fn(u64) -> bool,
    ) -> Option<(u64, usize)> {
        let balance = self.balances.entry(dc_id).or_default();
        let session_index = balance.best_session_for_part(part_size)?;
        let only_highest = balance.total_requested > 0;
        let queue = self.queues.get(&dc_id)?;
        let task_id = queue.next_task(only_highest, is_ready)?;
        balance.change_requested(session_index, part_size as isize);
        Some((task_id, session_index))
    }

    /// Mutable reference to a DC's [`DcBalanceState`].
    pub fn dc_balance_mut(&mut self, dc_id: i32) -> &mut DcBalanceState {
        self.balances.entry(dc_id).or_default()
    }

    /// Read-only reference to a DC's [`DcBalanceState`].
    #[must_use]
    pub fn dc_balance(&self, dc_id: i32) -> Option<&DcBalanceState> {
        self.balances.get(&dc_id)
    }
}

// ============================================================================
// Out-of-order byte assembler (FileLoader::writeResultPart / finalizeResult)
// ============================================================================

/// Reassembles out-of-order downloaded chunks into a contiguous buffer while tracking holes,
/// port of `FileLoader::writeResultPart`, `readLoadedPartBack`, and `finalizeResult`
/// (`storage/file_download.cpp:373-457`).
#[derive(Debug, Default, Clone)]
pub struct PartAssembler {
    data: Vec<u8>,
    /// Sorted, merged half-open byte ranges `[start, end)` that have been written.
    /// Tracking exact ranges prevents duplicate or partially-extending writes (e.g. during
    /// CDN / file-reference retries) from corrupting hole accounting (`FileLoader::writeResultPart`).
    filled: Vec<(usize, usize)>,
}

impl PartAssembler {
    /// Creates an empty assembler, optionally pre-allocating `capacity` bytes.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            data: Vec::with_capacity(capacity),
            filled: Vec::new(),
        }
    }

    /// Writes `buffer` at `offset`, updating filled-range tracking.
    pub fn write_part(&mut self, offset: u64, buffer: &[u8]) -> Result<()> {
        if buffer.is_empty() {
            return Ok(());
        }
        let off = usize::try_from(offset)
            .map_err(|_| MediaError::InvalidTransferParam("offset overflows usize".into()))?;
        let end = off
            .checked_add(buffer.len())
            .ok_or_else(|| MediaError::InvalidTransferParam("offset + len overflow".into()))?;

        if off > self.data.len() {
            self.data.resize(off, 0);
        }
        if off == self.data.len() {
            self.data.extend_from_slice(buffer);
        } else {
            if end > self.data.len() {
                self.data.resize(end, 0);
            }
            self.data[off..end].copy_from_slice(buffer);
        }
        self.record_filled(off, end);
        Ok(())
    }

    fn record_filled(&mut self, start: usize, end: usize) {
        let mut merged = Vec::with_capacity(self.filled.len() + 1);
        let mut cur = (start, end);
        let mut inserted = false;
        for &(s, e) in &self.filled {
            if e < cur.0 {
                merged.push((s, e));
            } else if cur.1 < s {
                if !inserted {
                    merged.push(cur);
                    inserted = true;
                }
                merged.push((s, e));
            } else {
                cur.0 = cur.0.min(s);
                cur.1 = cur.1.max(e);
            }
        }
        if !inserted {
            merged.push(cur);
        }
        self.filled = merged;
    }

    fn missing_bytes(&self) -> usize {
        let covered: usize = self.filled.iter().map(|&(s, e)| e - s).sum();
        self.data.len().saturating_sub(covered)
    }

    /// Returns total downloaded bytes excluding holes (`FileLoader::currentOffset`).
    #[must_use]
    pub fn downloaded_bytes(&self) -> u64 {
        self.filled.iter().map(|&(s, e)| (e - s) as u64).sum()
    }

    /// Reads back an already-written slice (`FileLoader::readLoadedPartBack`).
    #[must_use]
    pub fn read_back(&self, offset: u64, size: usize) -> Option<&[u8]> {
        let off = usize::try_from(offset).ok()?;
        let end = off.checked_add(size)?;
        self.data.get(off..end)
    }

    /// Finalizes and returns the contiguous bytes, failing if any holes remain (`_skippedBytes != 0`).
    pub fn finalize(self) -> Result<Vec<u8>> {
        let missing = self.missing_bytes();
        if missing != 0 {
            return Err(MediaError::InvalidTransferParam(format!(
                "download finalized with {missing} missing bytes"
            )));
        }
        Ok(self.data)
    }
}

// ============================================================================
// Cancellation handle & async DownloadRequest / download_file
// ============================================================================

/// Lightweight cooperative cancellation token shared across download/upload tasks.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
}

impl CancelToken {
    /// Creates a new active (non-cancelled) token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Signals cancellation.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Returns `true` if [`CancelToken::cancel`] has been called.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// Parameters for executing a complete or partial MTProto file download via [`download_file`].
#[derive(Debug, Clone)]
pub struct DownloadRequest {
    /// Target DC ID.
    pub dc_id: i32,
    /// File location (`Document`, `Photo`, `WebFile`, etc.).
    pub location: TransferLocation,
    /// File origin used if `FILE_REFERENCE_*` refresh is required.
    pub origin: FileOrigin,
    /// Number of bytes to load (`<= full_size`, or `0` if full size is unknown).
    pub load_size: u64,
    /// Total file size in bytes (`0` if unknown — e.g. web files or un-sized photos).
    pub full_size: u64,
    /// Part size in bytes (defaults to [`DOWNLOAD_PART_SIZE`] = 128 KiB).
    pub part_size: u32,
    /// Maximum number of concurrent in-flight part requests (clamped to `1..=16`).
    pub max_parallel_parts: usize,
    /// Whether `upload.getFile` sets the `precise` flag (`false` by default, matching
    /// `DownloadMtprotoTask::sendRequest` in `storage/download_manager_mtproto.cpp:581`).
    pub precise: bool,
    /// Whether `upload.getFile` sets the `cdn_supported` flag (`true` by default, matching
    /// `DownloadMtprotoTask::sendRequest` in `storage/download_manager_mtproto.cpp:581`).
    pub cdn_supported: bool,
    /// Optional cache key and tag if integrating with [`LocalFileCache`].
    pub cache_target: Option<(StorageKey, u8)>,
}

impl DownloadRequest {
    /// Constructs a download request with tdesktop defaults (128 KiB parts, 4 parallel parts).
    #[must_use]
    pub fn new(dc_id: i32, location: TransferLocation, full_size: u64) -> Self {
        Self {
            dc_id,
            location,
            origin: FileOrigin::default(),
            load_size: full_size,
            full_size,
            part_size: DOWNLOAD_PART_SIZE,
            max_parallel_parts: 4,
            precise: false,
            cdn_supported: true,
            cache_target: None,
        }
    }
}

/// Download progress snapshot emitted to callbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress {
    /// Bytes downloaded so far.
    pub downloaded: u64,
    /// Target byte count (`load_size`, or `0` when unknown).
    pub total: u64,
}

impl DownloadProgress {
    /// Normalized progress in `0.0..=1.0` (`FileLoader::currentProgress`).
    #[must_use]
    pub fn fraction(self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            (self.downloaded as f64 / self.total as f64).clamp(0.0, 1.0)
        }
    }
}

/// Internal CDN session state attached to a running download task.
#[derive(Debug, Clone)]
struct ActiveCdn {
    dc_id: i32,
    file_token: Vec<u8>,
    key: [u8; 32],
    iv: [u8; 16],
    verifier: CdnHashVerifier,
}

impl ActiveCdn {
    fn from_redirect(redirect: CdnRedirect) -> Result<Self> {
        let key: [u8; 32] = redirect.encryption_key.try_into().map_err(|v: Vec<u8>| {
            MediaError::InvalidCdnParams(format!("expected 32-byte CDN key, got {}", v.len()))
        })?;
        let iv: [u8; 16] = redirect.encryption_iv.try_into().map_err(|v: Vec<u8>| {
            MediaError::InvalidCdnParams(format!("expected 16-byte CDN IV, got {}", v.len()))
        })?;
        let mut verifier = CdnHashVerifier::new();
        verifier.add_hashes(redirect.file_hashes);
        Ok(Self {
            dc_id: redirect.dc_id,
            file_token: redirect.file_token,
            key,
            iv,
            verifier,
        })
    }
}

fn is_cdn_token_error(err: &MediaError) -> bool {
    matches!(
        err,
        MediaError::Rpc { message, .. }
            if message == "FILE_TOKEN_INVALID" || message == "REQUEST_TOKEN_INVALID"
    )
}

fn is_file_reference_rpc_error(err: &MediaError) -> Option<&str> {
    match err {
        MediaError::Rpc { code: 400, message } if message.starts_with("FILE_REFERENCE_") => {
            Some(message.as_str())
        }
        _ => None,
    }
}

/// Executes a full (or progressive-prefix) download using `invoker`, optionally consulting and
/// populating `cache`.
///
/// Faithfully implements tdesktop's `mtpFileLoader` + `DownloadMtprotoTask` protocol:
/// - Cache lookup (including `"partial:"` prefix resumption on 128 KiB boundaries)
/// - `upload.getFile` and `upload.getWebFile`
/// - `FILE_REFERENCE_*` refresh & automatic retry
/// - `upload.fileCdnRedirect` -> `upload.getCdnFile` + AES-256-CTR decryption + SHA-256 verification
/// - `upload.getCdnFileHashes` when a chunk's hash is not yet cached
/// - `upload.reuploadCdnFile` on `upload.cdnFileReuploadNeeded`
/// - Fallback to home DC on `FILE_TOKEN_INVALID` / `REQUEST_TOKEN_INVALID`
/// - Cooperative cancellation and progress reporting
pub async fn download_file<I: MediaInvoker + ?Sized>(
    invoker: &I,
    mut req: DownloadRequest,
    mut cache: Option<&mut LocalFileCache>,
    cancel: &CancelToken,
    mut on_progress: impl FnMut(DownloadProgress),
) -> Result<Vec<u8>> {
    if cancel.is_cancelled() {
        return Err(MediaError::Cancelled);
    }

    let part_size = req.part_size;
    // Validate `part_size` upfront against Telegram's `upload.getFile` limit rules
    // (`storage/download_manager_mtproto.cpp:514-585`).
    validate_get_file_request(0, part_size, req.precise)?;
    let mut assembler = PartAssembler::with_capacity(req.load_size.min(4 * 1024 * 1024) as usize);
    let mut next_offset: u64 = 0;

    // 1. Try local cache first (mirrors FileLoader::tryLoadLocal & startLoadingWithPartial).
    if let (Some((key, _)), Some(c)) = (req.cache_target, cache.as_deref_mut())
        && let Some(cached) = c.get(key)?
    {
        if let Some(partial_body) = decode_partial_payload(&cached) {
            let target = req.load_size as usize;
            if target > 0 && partial_body.len() >= target {
                on_progress(DownloadProgress {
                    downloaded: partial_body.len() as u64,
                    total: req.load_size,
                });
                return Ok(partial_body.to_vec());
            }
            // Reuse aligned complete parts from partial cache.
            let usable = (partial_body.len() / part_size as usize) * part_size as usize;
            if usable > 0 {
                assembler.write_part(0, &partial_body[..usable])?;
                next_offset = usable as u64;
                on_progress(DownloadProgress {
                    downloaded: next_offset,
                    total: req.load_size,
                });
            }
        } else {
            on_progress(DownloadProgress {
                downloaded: cached.len() as u64,
                total: cached.len() as u64,
            });
            return Ok(cached);
        }
    }

    const MAX_CDN_REUPLOADS: usize = 8;
    const MAX_CDN_TOKEN_FALLBACKS: usize = 4;

    let is_web = matches!(req.location, TransferLocation::WebFile { .. });
    let mut full_size = req.full_size;
    let mut load_size = if req.load_size > 0 {
        req.load_size
    } else {
        full_size
    };
    let mut cdn: Option<ActiveCdn> = None;
    let mut cdn_supported = req.cdn_supported;
    let mut last_complete = false;
    let mut session_idx = 0usize;
    let max_parallel = req.max_parallel_parts.clamp(1, 16);
    let mut ref_refreshed = false;
    let mut reupload_retries: usize = 0;
    let mut cdn_token_fallbacks: usize = 0;
    let mut completed_offsets: HashSet<u64> = HashSet::new();

    while !last_complete && (full_size == 0 || next_offset < load_size) {
        if cancel.is_cancelled() {
            return Err(MediaError::Cancelled);
        }

        // Advance `next_offset` past any offsets already completed in an earlier partial wave.
        while completed_offsets.contains(&next_offset)
            && (full_size == 0 || next_offset < load_size)
        {
            next_offset += u64::from(part_size);
        }
        if full_size > 0 && next_offset >= load_size {
            break;
        }

        // Determine how many offsets to fetch in this wave.
        let mut wave_offsets = Vec::new();
        let wave_cap = if full_size == 0 || (cdn.is_none() && next_offset == 0) {
            // Probe first chunk sequentially when size is unknown or before knowing whether CDN redirects.
            1
        } else {
            max_parallel
        };

        let mut cursor = next_offset;
        while wave_offsets.len() < wave_cap && (full_size == 0 || cursor < load_size) {
            if !completed_offsets.contains(&cursor) {
                wave_offsets.push(cursor);
            }
            cursor += u64::from(part_size);
        }

        // Fetch chunks concurrently when wave_offsets.len() > 1.
        let mut unchecked_cdn: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
        let mut need_retry_wave = false;

        if wave_offsets.len() == 1 {
            let offset = wave_offsets[0];
            let chunk = fetch_one_part(
                invoker,
                req.dc_id,
                session_idx,
                &mut req.location,
                &req.origin,
                offset,
                part_size,
                req.precise,
                &mut cdn_supported,
                is_web,
                &mut full_size,
                &mut load_size,
                &mut cdn,
                &mut ref_refreshed,
                &mut reupload_retries,
                &mut cdn_token_fallbacks,
                MAX_CDN_REUPLOADS,
                MAX_CDN_TOKEN_FALLBACKS,
            )
            .await?;

            match chunk {
                FetchedPart::Plain(bytes) => {
                    if bytes.len() > part_size as usize {
                        return Err(MediaError::InvalidTransferParam(
                            "server returned part larger than requested limit".into(),
                        ));
                    }
                    assembler.write_part(offset, &bytes)?;
                    completed_offsets.insert(offset);
                    next_offset = offset + u64::from(part_size);
                    if bytes.is_empty()
                        || bytes.len() % 1024 != 0
                        || bytes.len() < part_size as usize
                    {
                        last_complete = true;
                    }
                }
                FetchedPart::CdnUnchecked(bytes) => {
                    unchecked_cdn.insert(offset, bytes);
                }
                FetchedPart::SwitchedTransport => {
                    need_retry_wave = true;
                }
            }
        } else {
            // Concurrent wave over the active transport (main DC or CDN).
            // Polled genuinely in parallel via `join_all` (matching `DownloadManagerMtproto::checkSendNext`,
            // `storage/download_manager_mtproto.cpp:171-203`).
            if let Some(active_cdn) = cdn.as_ref() {
                let mut futs = Vec::with_capacity(wave_offsets.len());
                for (i, &off) in wave_offsets.iter().enumerate() {
                    let sidx = (session_idx + i) % MAX_SESSIONS_COUNT;
                    futs.push(invoker.get_cdn_file(
                        active_cdn.dc_id,
                        sidx,
                        GetCdnFileRequest {
                            file_token: active_cdn.file_token.clone(),
                            offset: off,
                            limit: part_size,
                        },
                    ));
                }
                let results = join_all(futs).await;

                for (off, res) in wave_offsets.iter().copied().zip(results) {
                    if cancel.is_cancelled() {
                        return Err(MediaError::Cancelled);
                    }
                    match res {
                        Ok(GetCdnFileResponse::Bytes(mut enc)) => {
                            // Match `DownloadMtprotoTask::partLoaded` (`storage/download_manager_mtproto.cpp:922`).
                            if enc.len() > part_size as usize {
                                return Err(MediaError::InvalidTransferParam(
                                    "server returned CDN part larger than requested limit".into(),
                                ));
                            }
                            // Match `mtpFileLoader::feedPart` (`storage/file_download_mtproto.cpp:150`):
                            // empty CDN chunk signals EOF without requiring a non-existent EOF `fileHash`.
                            if enc.is_empty() {
                                completed_offsets.insert(off);
                                last_complete = true;
                                continue;
                            }
                            let active = cdn.as_mut().ok_or(MediaError::Cancelled)?;
                            apply_cdn_aes_ctr(&mut enc, &active.key, &active.iv, off)?;
                            match active.verifier.check(off, &enc) {
                                CheckCdnHashResult::Good => {
                                    assembler.write_part(off, &enc)?;
                                    completed_offsets.insert(off);
                                    if enc.len() % 1024 != 0 || enc.len() < part_size as usize {
                                        last_complete = true;
                                    }
                                }
                                CheckCdnHashResult::NoHash => {
                                    unchecked_cdn.insert(off, enc);
                                }
                                CheckCdnHashResult::Invalid => {
                                    return Err(MediaError::CdnHashMismatch { offset: off });
                                }
                            }
                        }
                        Ok(GetCdnFileResponse::ReuploadNeeded { request_token }) => {
                            reupload_retries += 1;
                            if reupload_retries > MAX_CDN_REUPLOADS {
                                return Err(MediaError::Rpc {
                                    code: 500,
                                    message: "CDN_REUPLOAD_LIMIT_EXCEEDED".into(),
                                });
                            }
                            let active = cdn.as_mut().ok_or(MediaError::Cancelled)?;
                            match invoker
                                .reupload_cdn_file(
                                    req.dc_id,
                                    session_idx,
                                    active.file_token.clone(),
                                    request_token,
                                )
                                .await
                            {
                                Ok(new_hashes) => {
                                    active.verifier.add_hashes(new_hashes);
                                }
                                Err(ref e) if is_cdn_token_error(e) => {
                                    // Match `DownloadMtprotoTask::cdnPartFailed`
                                    // (`storage/download_manager_mtproto.cpp:671, 965-978`).
                                    cdn = None;
                                    unchecked_cdn.clear();
                                    cdn_token_fallbacks += 1;
                                    if cdn_token_fallbacks > MAX_CDN_TOKEN_FALLBACKS {
                                        cdn_supported = false;
                                    }
                                }
                                Err(e) => return Err(e),
                            }
                            need_retry_wave = true;
                            break;
                        }
                        Err(ref e) if is_cdn_token_error(e) => {
                            // Match `DownloadMtprotoTask::cdnPartFailed`
                            // (`storage/download_manager_mtproto.cpp:965-978`): fall back to home
                            // DC and discard any unchecked CDN parts tied to the invalidated token.
                            cdn = None;
                            unchecked_cdn.clear();
                            cdn_token_fallbacks += 1;
                            if cdn_token_fallbacks > MAX_CDN_TOKEN_FALLBACKS {
                                cdn_supported = false;
                            }
                            need_retry_wave = true;
                            break;
                        }
                        Err(e) => return Err(e),
                    }
                }
                if !need_retry_wave && unchecked_cdn.is_empty() {
                    next_offset = cursor;
                }
            } else {
                // Concurrent main-DC wave.
                let mut futs = Vec::with_capacity(wave_offsets.len());
                for (i, &off) in wave_offsets.iter().enumerate() {
                    let sidx = (session_idx + i) % MAX_SESSIONS_COUNT;
                    let loc = req.location.clone();
                    if is_web {
                        futs.push(Box::pin(async move {
                            let r = invoker
                                .get_web_file(
                                    req.dc_id,
                                    sidx,
                                    GetWebFileRequest {
                                        location: loc,
                                        offset: off,
                                        limit: part_size,
                                    },
                                )
                                .await?;
                            Ok(GetFileResponse::File { bytes: r.bytes })
                        })
                            as crate::transfer::BoxFuture<'_, Result<GetFileResponse>>);
                    } else {
                        futs.push(invoker.get_file(
                            req.dc_id,
                            sidx,
                            GetFileRequest {
                                location: loc,
                                offset: off,
                                limit: part_size,
                                precise: req.precise,
                                cdn_supported,
                            },
                        ));
                    }
                }
                let results = join_all(futs).await;
                for (off, res) in wave_offsets.iter().copied().zip(results) {
                    if cancel.is_cancelled() {
                        return Err(MediaError::Cancelled);
                    }
                    match res {
                        Ok(GetFileResponse::File { bytes }) => {
                            if bytes.len() > part_size as usize {
                                return Err(MediaError::InvalidTransferParam(
                                    "server returned part larger than requested limit".into(),
                                ));
                            }
                            assembler.write_part(off, &bytes)?;
                            completed_offsets.insert(off);
                            if bytes.is_empty()
                                || bytes.len() % 1024 != 0
                                || bytes.len() < part_size as usize
                            {
                                last_complete = true;
                            }
                        }
                        Ok(GetFileResponse::CdnRedirect(redirect)) => {
                            cdn = Some(ActiveCdn::from_redirect(redirect)?);
                            need_retry_wave = true;
                            break;
                        }
                        Err(ref e)
                            if is_file_reference_rpc_error(e).is_some() && !ref_refreshed =>
                        {
                            let msg = is_file_reference_rpc_error(e)
                                .unwrap_or("FILE_REFERENCE_EXPIRED")
                                .to_owned();
                            let updated = invoker
                                .refresh_file_reference(req.origin.clone(), req.location.clone())
                                .await?;
                            if !req.location.set_file_reference(updated) {
                                return Err(MediaError::Rpc {
                                    code: 400,
                                    message: msg,
                                });
                            }
                            ref_refreshed = true;
                            need_retry_wave = true;
                            break;
                        }
                        Err(e) => return Err(e),
                    }
                }
                if !need_retry_wave {
                    next_offset = cursor;
                }
            }
        }

        // Resolve any unchecked CDN chunks by calling `upload.getCdnFileHashes`.
        while !unchecked_cdn.is_empty() {
            let first_offset = *unchecked_cdn.keys().next().ok_or(MediaError::Cancelled)?;
            let Some(active) = cdn.as_mut() else {
                // CDN transport was cleared (e.g. token invalidated mid-wave); drop unchecked parts
                // so they are re-fetched cleanly from the home DC.
                unchecked_cdn.clear();
                need_retry_wave = true;
                break;
            };
            let more_hashes = match invoker
                .get_cdn_file_hashes(
                    req.dc_id,
                    session_idx,
                    active.file_token.clone(),
                    first_offset,
                )
                .await
            {
                Ok(h) => h,
                Err(ref e) if is_cdn_token_error(e) => {
                    // Match `DownloadMtprotoTask::cdnPartFailed`
                    // (`storage/download_manager_mtproto.cpp:616, 965-978`).
                    cdn = None;
                    unchecked_cdn.clear();
                    cdn_token_fallbacks += 1;
                    if cdn_token_fallbacks > MAX_CDN_TOKEN_FALLBACKS {
                        cdn_supported = false;
                    }
                    need_retry_wave = true;
                    break;
                }
                Err(e) => return Err(e),
            };
            active.verifier.add_hashes(more_hashes);

            let mut checked_any = false;
            let keys: Vec<u64> = unchecked_cdn.keys().copied().collect();
            for off in keys {
                let bytes = unchecked_cdn.get(&off).ok_or(MediaError::Cancelled)?;
                match active.verifier.check(off, bytes) {
                    CheckCdnHashResult::NoHash => {}
                    CheckCdnHashResult::Invalid => {
                        return Err(MediaError::CdnHashMismatch { offset: off });
                    }
                    CheckCdnHashResult::Good => {
                        checked_any = true;
                        let good = unchecked_cdn.remove(&off).ok_or(MediaError::Cancelled)?;
                        assembler.write_part(off, &good)?;
                        completed_offsets.insert(off);
                        if good.is_empty()
                            || good.len() % 1024 != 0
                            || good.len() < part_size as usize
                        {
                            last_complete = true;
                        }
                    }
                }
            }
            if !checked_any {
                return Err(MediaError::CdnHashMissing {
                    offset: first_offset,
                });
            }
        }

        if !need_retry_wave {
            while completed_offsets.contains(&next_offset)
                && (full_size == 0 || next_offset < load_size)
            {
                next_offset += u64::from(part_size);
            }
            session_idx = (session_idx + 1) % MAX_SESSIONS_COUNT;
            on_progress(DownloadProgress {
                downloaded: assembler.downloaded_bytes(),
                total: load_size,
            });
        }
    }

    let bytes = assembler.finalize()?;

    // Store in local cache (with `"partial:"` prefix if only a progressive prefix was loaded).
    if let (Some((key, tag)), Some(c)) = (req.cache_target, cache)
        && key.is_valid()
        && bytes.len() <= crate::cache::MAX_FILE_IN_MEMORY
    {
        let is_complete = full_size == 0 || (bytes.len() as u64) >= full_size;
        if is_complete {
            let _ = c.put(key, tag, &bytes);
        } else {
            let partial = encode_partial_payload(&bytes);
            let _ = c.put(key, tag, &partial);
        }
    }

    Ok(bytes)
}

enum FetchedPart {
    Plain(Vec<u8>),
    CdnUnchecked(Vec<u8>),
    SwitchedTransport,
}

#[allow(clippy::too_many_arguments)]
async fn fetch_one_part<I: MediaInvoker + ?Sized>(
    invoker: &I,
    dc_id: i32,
    session_idx: usize,
    location: &mut TransferLocation,
    origin: &FileOrigin,
    offset: u64,
    part_size: u32,
    precise: bool,
    cdn_supported: &mut bool,
    is_web: bool,
    full_size: &mut u64,
    load_size: &mut u64,
    cdn: &mut Option<ActiveCdn>,
    ref_refreshed: &mut bool,
    reupload_retries: &mut usize,
    cdn_token_fallbacks: &mut usize,
    max_cdn_reuploads: usize,
    max_cdn_token_fallbacks: usize,
) -> Result<FetchedPart> {
    if let Some(active) = cdn.as_mut() {
        match invoker
            .get_cdn_file(
                active.dc_id,
                session_idx,
                GetCdnFileRequest {
                    file_token: active.file_token.clone(),
                    offset,
                    limit: part_size,
                },
            )
            .await
        {
            Ok(GetCdnFileResponse::Bytes(mut enc)) => {
                // Match `DownloadMtprotoTask::partLoaded` (`storage/download_manager_mtproto.cpp:922`).
                if enc.len() > part_size as usize {
                    return Err(MediaError::InvalidTransferParam(
                        "server returned CDN part larger than requested limit".into(),
                    ));
                }
                // Empty CDN chunk signals EOF without requiring a non-existent EOF `fileHash`.
                if enc.is_empty() {
                    return Ok(FetchedPart::Plain(enc));
                }
                apply_cdn_aes_ctr(&mut enc, &active.key, &active.iv, offset)?;
                match active.verifier.check(offset, &enc) {
                    CheckCdnHashResult::Good => Ok(FetchedPart::Plain(enc)),
                    CheckCdnHashResult::NoHash => Ok(FetchedPart::CdnUnchecked(enc)),
                    CheckCdnHashResult::Invalid => Err(MediaError::CdnHashMismatch { offset }),
                }
            }
            Ok(GetCdnFileResponse::ReuploadNeeded { request_token }) => {
                *reupload_retries += 1;
                if *reupload_retries > max_cdn_reuploads {
                    return Err(MediaError::Rpc {
                        code: 500,
                        message: "CDN_REUPLOAD_LIMIT_EXCEEDED".into(),
                    });
                }
                match invoker
                    .reupload_cdn_file(dc_id, session_idx, active.file_token.clone(), request_token)
                    .await
                {
                    Ok(hashes) => {
                        active.verifier.add_hashes(hashes);
                        Ok(FetchedPart::SwitchedTransport)
                    }
                    Err(ref e) if is_cdn_token_error(e) => {
                        // Match `DownloadMtprotoTask::cdnPartFailed`
                        // (`storage/download_manager_mtproto.cpp:671, 965-978`).
                        *cdn = None;
                        *cdn_token_fallbacks += 1;
                        if *cdn_token_fallbacks > max_cdn_token_fallbacks {
                            *cdn_supported = false;
                        }
                        Ok(FetchedPart::SwitchedTransport)
                    }
                    Err(e) => Err(e),
                }
            }
            Err(ref e) if is_cdn_token_error(e) => {
                *cdn = None;
                *cdn_token_fallbacks += 1;
                if *cdn_token_fallbacks > max_cdn_token_fallbacks {
                    *cdn_supported = false;
                }
                Ok(FetchedPart::SwitchedTransport)
            }
            Err(e) => Err(e),
        }
    } else if is_web {
        let resp = invoker
            .get_web_file(
                dc_id,
                session_idx,
                GetWebFileRequest {
                    location: location.clone(),
                    offset,
                    limit: part_size,
                },
            )
            .await?;
        if *full_size == 0 || *full_size == resp.size {
            *full_size = resp.size;
            *load_size = resp.size;
            Ok(FetchedPart::Plain(resp.bytes))
        } else {
            Err(MediaError::InvalidTransferParam(format!(
                "web file size mismatch: expected {}, got {}",
                *full_size, resp.size
            )))
        }
    } else {
        match invoker
            .get_file(
                dc_id,
                session_idx,
                GetFileRequest {
                    location: location.clone(),
                    offset,
                    limit: part_size,
                    precise,
                    cdn_supported: *cdn_supported,
                },
            )
            .await
        {
            Ok(GetFileResponse::File { bytes }) => Ok(FetchedPart::Plain(bytes)),
            Ok(GetFileResponse::CdnRedirect(redirect)) => {
                *cdn = Some(ActiveCdn::from_redirect(redirect)?);
                Ok(FetchedPart::SwitchedTransport)
            }
            Err(ref e) if is_file_reference_rpc_error(e).is_some() && !*ref_refreshed => {
                let msg = is_file_reference_rpc_error(e)
                    .unwrap_or("FILE_REFERENCE_EXPIRED")
                    .to_owned();
                let updated = invoker
                    .refresh_file_reference(origin.clone(), location.clone())
                    .await?;
                if !location.set_file_reference(updated) {
                    return Err(MediaError::Rpc {
                        code: 400,
                        message: msg,
                    });
                }
                *ref_refreshed = true;
                Ok(FetchedPart::SwitchedTransport)
            }
            Err(e) => Err(e),
        }
    }
}
