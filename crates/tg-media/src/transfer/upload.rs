//! Upload planning, part-size selection, MD5 computation, multi-session scheduler,
//! and async uploader state machine.
//!
//! Port of `Telegram/SourceFiles/storage/file_upload.h`,
//! `Telegram/SourceFiles/storage/file_upload.cpp`, and
//! `Telegram/SourceFiles/storage/localimageloader.h` / `localimageloader.cpp`.

use crate::error::{MediaError, Result};
use crate::transfer::download::CancelToken;
use crate::transfer::{MediaInvoker, SaveBigFilePartRequest, SaveFilePartRequest, join_all};
use md5::{Digest as _, Md5};
use rand::RngCore;
use std::collections::{BTreeSet, HashMap, VecDeque};

/// Standard Telegram API threshold above which `upload.saveBigFilePart` / `inputFileBig`
/// is used (10 MiB).
pub const BIG_FILE_THRESHOLD: u64 = 10 * 1024 * 1024;

/// tdesktop's `Storage::kUseBigFilesFrom` constant (`file_upload.h:29`, 30 MiB).
pub const TDESKTOP_USE_BIG_FILES_FROM: u64 = 30 * 1024 * 1024;

/// Maximum upload size for non-premium accounts (`kFileSizeLimit` = 2000 MiB).
pub const MAX_FILE_SIZE_DEFAULT: u64 = 2_000 * 1024 * 1024;

/// Maximum upload size for premium accounts (`kFileSizePremiumLimit` = 4000 MiB).
pub const MAX_FILE_SIZE_PREMIUM: u64 = 4_000 * 1024 * 1024;

/// Maximum upload part count for non-premium accounts (`kDocumentMaxPartsCountDefault`).
pub const MAX_UPLOAD_PARTS_DEFAULT: u32 = 4_000;

/// Maximum upload part count for Telegram Premium accounts (8000 * 512 KiB = 4000 MiB).
pub const MAX_UPLOAD_PARTS_PREMIUM: u32 = 8_000;

/// Part sizes used by `Uploader::Entry::setDocSize` (`file_upload.cpp:45-58`).
pub const UPLOAD_PART_SIZE_32K: u32 = 32 * 1024;
/// 64 KiB upload part size (for files `1 MiB..=32 MiB`).
pub const UPLOAD_PART_SIZE_64K: u32 = 64 * 1024;
/// 128 KiB upload part size.
pub const UPLOAD_PART_SIZE_128K: u32 = 128 * 1024;
/// 256 KiB upload part size.
pub const UPLOAD_PART_SIZE_256K: u32 = 256 * 1024;
/// 512 KiB maximum upload part size.
pub const UPLOAD_PART_SIZE_512K: u32 = 512 * 1024;

/// Photo and thumbnail upload slice size (`kPhotoUploadPartSize` in `localimageloader.cpp:53`).
pub const PHOTO_UPLOAD_PART_SIZE: u32 = 32 * 1024;

/// Maximum bytes in flight per upload session (`kMaxUploadPerSession` = 1 MiB).
pub const MAX_UPLOAD_PER_SESSION: usize = 1024 * 1024;

/// Maximum concurrent upload sessions (`kMaxSessionsCount` = 8).
pub const MAX_UPLOAD_SESSIONS: usize = 8;

/// Fast request threshold in ms (`kFastRequestThreshold` = 1 s).
pub const FAST_REQUEST_THRESHOLD_MS: u64 = 1_000;

/// Slow request threshold in ms (`kSlowRequestThreshold` = 8 s).
pub const SLOW_REQUEST_THRESHOLD_MS: u64 = 8_000;

/// Minimum wait between session removals (`kWaitForNormalizeTimeout` = 8 s).
pub const WAIT_FOR_NORMALIZE_TIMEOUT_MS: u64 = 8_000;

/// Minimum total queued + part bytes to classify a fast upload request (`kAcceptAsFastIfTotalAtLeast`).
pub const ACCEPT_AS_FAST_MIN_BYTES: usize = 512 * 1024;

/// Selects `(part_size, parts_count)` for uploading a document of `file_size` bytes.
///
/// Faithfully ports `Uploader::Entry::setDocSize` (`file_upload.cpp:148-167`) extended
/// to support Telegram Premium's 8000-part / 4000 MiB ceiling:
/// - `< 1 MiB`: 32 KiB
/// - `1 MiB ..= 32 MiB`: 64 KiB
/// - `<= 4000 * 128 KiB` (500 MiB): 128 KiB
/// - `<= 4000 * 256 KiB` (1000 MiB): 256 KiB
/// - `<= max_parts * 512 KiB` (2000 MiB default / 4000 MiB premium): 512 KiB
pub fn choose_upload_part_size(file_size: u64, is_premium: bool) -> Result<(u32, u32)> {
    if file_size == 0 {
        return Err(MediaError::InvalidFileSize(
            "cannot upload an empty (0-byte) file".into(),
        ));
    }
    let max_size = if is_premium {
        MAX_FILE_SIZE_PREMIUM
    } else {
        MAX_FILE_SIZE_DEFAULT
    };
    let max_parts = if is_premium {
        MAX_UPLOAD_PARTS_PREMIUM
    } else {
        MAX_UPLOAD_PARTS_DEFAULT
    };
    if file_size > max_size {
        return Err(MediaError::InvalidFileSize(format!(
            "file size {file_size} exceeds {} limit ({max_size})",
            if is_premium { "premium" } else { "standard" }
        )));
    }

    const LIMIT_0: u64 = 1024 * 1024;
    const LIMIT_1: u64 = 32 * LIMIT_0;

    let fits_default = |ps: u32| -> Option<u32> {
        let count = file_size.div_ceil(u64::from(ps));
        (count <= u64::from(MAX_UPLOAD_PARTS_DEFAULT)).then_some(count as u32)
    };

    if file_size < LIMIT_0
        && let Some(parts) = fits_default(UPLOAD_PART_SIZE_32K)
    {
        return Ok((UPLOAD_PART_SIZE_32K, parts));
    }
    if file_size <= LIMIT_1
        && let Some(parts) = fits_default(UPLOAD_PART_SIZE_64K)
    {
        return Ok((UPLOAD_PART_SIZE_64K, parts));
    }
    if let Some(parts) = fits_default(UPLOAD_PART_SIZE_128K) {
        return Ok((UPLOAD_PART_SIZE_128K, parts));
    }
    if let Some(parts) = fits_default(UPLOAD_PART_SIZE_256K) {
        return Ok((UPLOAD_PART_SIZE_256K, parts));
    }

    let parts_512k = file_size.div_ceil(u64::from(UPLOAD_PART_SIZE_512K));
    if parts_512k <= u64::from(max_parts) {
        Ok((UPLOAD_PART_SIZE_512K, parts_512k as u32))
    } else {
        Err(MediaError::InvalidFileSize(format!(
            "file requires {parts_512k} parts of 512 KiB, exceeding limit {max_parts}"
        )))
    }
}

/// Generates a non-zero random 64-bit Telegram upload `file_id`.
#[must_use]
pub fn generate_random_file_id() -> i64 {
    let mut rng = rand::thread_rng();
    generate_random_file_id_with_rng(&mut rng)
}

/// Generates a non-zero random 64-bit upload `file_id` from a caller-supplied RNG.
#[must_use]
pub fn generate_random_file_id_with_rng(rng: &mut dyn RngCore) -> i64 {
    loop {
        let id = rng.next_u64() as i64;
        if id != 0 {
            return id;
        }
    }
}

/// Upload configuration controlling small-vs-big threshold, premium limit, and concurrency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadConfig {
    /// Byte threshold above which `upload.saveBigFilePart` is used (default [`BIG_FILE_THRESHOLD`] = 10 MiB).
    pub big_file_threshold: u64,
    /// Whether the account has Telegram Premium (allows up to 8000 parts / 4000 MiB).
    pub is_premium: bool,
    /// If `true`, forces 32 KiB slices (`PHOTO_UPLOAD_PART_SIZE`) and ensures `.jpg` extension
    /// for photo uploads as in `file_upload.cpp:1130`.
    pub is_photo: bool,
    /// Maximum concurrent upload sessions (1..=8).
    pub max_sessions: usize,
}

impl Default for UploadConfig {
    fn default() -> Self {
        Self {
            big_file_threshold: BIG_FILE_THRESHOLD,
            is_premium: false,
            is_photo: false,
            max_sessions: 4,
        }
    }
}

/// Describes a single planned upload RPC call (`upload.saveFilePart` or `upload.saveBigFilePart`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannedUploadPart {
    /// Zero-based part index (`file_part`).
    pub part_index: u32,
    /// Total number of parts (`file_total_parts`, sent for big files).
    pub total_parts: u32,
    /// Byte offset inside the source file.
    pub offset: u64,
    /// Exact byte length of this part.
    pub length: u32,
    /// `true` -> `upload.saveBigFilePart`; `false` -> `upload.saveFilePart`.
    pub is_big: bool,
}

/// Complete upload plan computed before sending any network requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadPlan {
    /// Random 64-bit `file_id`.
    pub file_id: i64,
    /// Effective filename to send in `inputFile` / `inputFileBig`.
    pub filename: String,
    /// Total byte length of the file.
    pub file_size: u64,
    /// Chosen part size in bytes.
    pub part_size: u32,
    /// Total number of parts.
    pub total_parts: u32,
    /// `true` if `file_size > big_file_threshold` (`upload.saveBigFilePart`).
    pub is_big: bool,
}

impl UploadPlan {
    /// Creates an [`UploadPlan`] for a file of `file_size` bytes using a random `file_id`.
    pub fn new(filename: impl Into<String>, file_size: u64, config: UploadConfig) -> Result<Self> {
        Self::with_file_id(generate_random_file_id(), filename, file_size, config)
    }

    /// Creates an [`UploadPlan`] with an explicit `file_id` (useful for deterministic tests).
    pub fn with_file_id(
        file_id: i64,
        filename: impl Into<String>,
        file_size: u64,
        config: UploadConfig,
    ) -> Result<Self> {
        let mut filename = filename.into();
        if config.is_photo && !filename.to_ascii_lowercase().ends_with(".jpg") {
            // Mirrors file_upload.cpp:1130-1135.
            filename.push_str(".jpg");
        }

        let (part_size, total_parts) = if config.is_photo && file_size <= config.big_file_threshold
        {
            if file_size == 0 {
                return Err(MediaError::InvalidFileSize(
                    "cannot upload an empty photo".into(),
                ));
            }
            let parts = file_size.div_ceil(u64::from(PHOTO_UPLOAD_PART_SIZE)) as u32;
            (PHOTO_UPLOAD_PART_SIZE, parts)
        } else {
            choose_upload_part_size(file_size, config.is_premium)?
        };

        let is_big = file_size > config.big_file_threshold;
        Ok(Self {
            file_id,
            filename,
            file_size,
            part_size,
            total_parts,
            is_big,
        })
    }

    /// Iterates over all planned parts in sequential order `0..total_parts`.
    pub fn parts(&self) -> impl Iterator<Item = PlannedUploadPart> + '_ {
        (0..self.total_parts).map(|part_index| {
            let offset = u64::from(part_index) * u64::from(self.part_size);
            let remaining = self.file_size.saturating_sub(offset);
            let length = remaining.min(u64::from(self.part_size)) as u32;
            PlannedUploadPart {
                part_index,
                total_parts: self.total_parts,
                offset,
                length,
                is_big: self.is_big,
            }
        })
    }
}

/// Computes lowercase 32-char hexadecimal MD5 digest for small file uploads (`hashMd5Hex`).
#[must_use]
pub fn compute_md5_hex(data: &[u8]) -> String {
    let digest = Md5::digest(data);
    let mut out = String::with_capacity(32);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Final `InputFile` descriptor produced after all upload parts succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadedInputFile {
    /// Small file (`inputFile`), includes 32-char lowercase hex MD5 checksum.
    Small {
        /// Random 64-bit file ID.
        id: i64,
        /// Total uploaded parts.
        parts: u32,
        /// File name.
        name: String,
        /// Lowercase hex MD5 checksum of the full file.
        md5_checksum: String,
    },
    /// Big file (`inputFileBig`), no MD5 checksum.
    Big {
        /// Random 64-bit file ID.
        id: i64,
        /// Total uploaded parts.
        parts: u32,
        /// File name.
        name: String,
    },
}

/// Upload progress snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadProgress {
    /// Total acknowledged uploaded bytes.
    pub uploaded: u64,
    /// Total file size in bytes.
    pub total: u64,
}

impl UploadProgress {
    /// Normalized progress in `0.0..=1.0`.
    #[must_use]
    pub fn fraction(self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            (self.uploaded as f64 / self.total as f64).clamp(0.0, 1.0)
        }
    }
}

// ============================================================================
// Multi-session upload scheduler state (port of Uploader DC index scaling)
// ============================================================================

/// Dynamic multi-session upload balancer mirroring `Storage::Uploader` (`file_upload.cpp:690-1098`).
#[derive(Debug, Clone)]
pub struct UploadSessionBalancer {
    sent_per_session: Vec<usize>,
    fast_sessions: BTreeSet<usize>,
    latest_added_ms: u64,
    latest_removed_ms: u64,
    max_sessions: usize,
}

impl UploadSessionBalancer {
    /// Creates a balancer with `max_sessions` capped at [`MAX_UPLOAD_SESSIONS`] (8).
    #[must_use]
    pub fn new(max_sessions: usize) -> Self {
        Self {
            sent_per_session: Vec::new(),
            fast_sessions: BTreeSet::new(),
            latest_added_ms: 0,
            latest_removed_ms: 0,
            max_sessions: max_sessions.clamp(1, MAX_UPLOAD_SESSIONS),
        }
    }

    /// Number of currently active upload sessions.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.sent_per_session.len()
    }

    /// Returns `true` if a new upload session can be added (`Uploader::canAddDcIndex`).
    #[must_use]
    pub fn can_add_session(&self) -> bool {
        let count = self.sent_per_session.len();
        count < self.max_sessions && count == self.fast_sessions.len()
    }

    /// Picks the best upload session index for the next part of `part_bytes` bytes
    /// (`Uploader::chooseDcIndexForNextRequest`).
    pub fn choose_session(
        &mut self,
        part_bytes: usize,
        used: &BTreeSet<usize>,
        now_ms: u64,
    ) -> Option<usize> {
        // Prefer an idle session first.
        for (i, &sent) in self.sent_per_session.iter().enumerate() {
            if sent == 0 && !used.contains(&i) {
                return Some(i);
            }
        }
        // Grow session pool if all current sessions proved fast.
        if self.can_add_session() {
            let idx = self.sent_per_session.len();
            self.sent_per_session.push(0);
            self.fast_sessions.clear();
            self.latest_added_ms = now_ms;
            return Some(idx);
        }
        // Otherwise pick the least-loaded session that fits within MAX_UPLOAD_PER_SESSION.
        let (idx, &sent) = self
            .sent_per_session
            .iter()
            .enumerate()
            .filter(|(i, _)| !used.contains(i))
            .min_by_key(|(_, sent)| *sent)?;
        (sent + part_bytes <= MAX_UPLOAD_PER_SESSION).then_some(idx)
    }

    /// Records bytes dispatched on `session_index`, returning the previously queued bytes on it.
    pub fn on_part_sent(&mut self, session_index: usize, part_bytes: usize) -> usize {
        if let Some(slot) = self.sent_per_session.get_mut(session_index) {
            let queued = *slot;
            *slot += part_bytes;
            queued
        } else {
            0
        }
    }

    /// Records completion of a part on `session_index`.
    ///
    /// Returns `Some(removed_session_index)` if a slow request triggered shrinking the session pool.
    pub fn on_part_finished(
        &mut self,
        session_index: usize,
        part_bytes: usize,
        queued_before: usize,
        sent_at_ms: u64,
        now_ms: u64,
    ) -> Option<usize> {
        if let Some(slot) = self.sent_per_session.get_mut(session_index) {
            *slot = slot.saturating_sub(part_bytes);
        }
        let duration = now_ms.saturating_sub(sent_at_ms);
        let fast = duration < FAST_REQUEST_THRESHOLD_MS;
        let slow = duration >= SLOW_REQUEST_THRESHOLD_MS;

        if !fast {
            self.fast_sessions.clear();
            if slow
                && now_ms.saturating_sub(self.latest_removed_ms) >= WAIT_FOR_NORMALIZE_TIMEOUT_MS
                && self.sent_per_session.len() > 1
            {
                let removed = self.sent_per_session.len() - 1;
                self.sent_per_session.pop();
                self.fast_sessions.remove(&removed);
                self.latest_removed_ms = now_ms;
                return Some(removed);
            }
        } else if sent_at_ms > self.latest_added_ms
            && queued_before + part_bytes >= ACCEPT_AS_FAST_MIN_BYTES
        {
            self.fast_sessions.insert(session_index);
        }
        None
    }
}

/// Parses a Telegram `FILE_PART_<index>_MISSING` RPC error identifier and returns `<index>`.
#[must_use]
pub fn parse_missing_file_part(error_name: &str) -> Option<u32> {
    let rest = error_name.strip_prefix("FILE_PART_")?;
    let digits = rest.strip_suffix("_MISSING")?;
    digits.parse::<u32>().ok()
}

fn is_retryable_upload_err(err: &MediaError) -> bool {
    match err {
        MediaError::Rpc { code, message } => {
            *code < 0
                || *code >= 500
                || message.starts_with("FLOOD_WAIT_")
                || message.starts_with("FLOOD_PREMIUM_WAIT_")
                || parse_missing_file_part(message).is_some()
        }
        _ => false,
    }
}

/// Uploads one specific part `part_index` of `plan` from `data` (used for targeted re-upload on
/// `FILE_PART_<X>_MISSING` when finalizing a message or media upload).
pub async fn upload_single_part<I: MediaInvoker + ?Sized>(
    invoker: &I,
    plan: &UploadPlan,
    data: &[u8],
    part_index: u32,
    session_index: usize,
    cancel: &CancelToken,
) -> Result<()> {
    if cancel.is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    if part_index >= plan.total_parts {
        return Err(MediaError::InvalidTransferParam(format!(
            "upload part index {part_index} out of range (total_parts={})",
            plan.total_parts
        )));
    }
    let offset = (u64::from(part_index) * u64::from(plan.part_size)) as usize;
    let end = (offset + plan.part_size as usize).min(data.len());
    let slice = data[offset..end].to_vec();

    const MAX_RETRIES: usize = 3;
    let mut attempt = 0usize;
    loop {
        if cancel.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        let res = if plan.is_big {
            invoker
                .save_big_file_part(
                    session_index,
                    SaveBigFilePartRequest {
                        file_id: plan.file_id,
                        file_part: part_index,
                        file_total_parts: plan.total_parts,
                        bytes: slice.clone(),
                    },
                )
                .await
        } else {
            invoker
                .save_file_part(
                    session_index,
                    SaveFilePartRequest {
                        file_id: plan.file_id,
                        file_part: part_index,
                        bytes: slice.clone(),
                    },
                )
                .await
        };
        match res {
            Ok(true) => return Ok(()),
            Ok(false) => {
                if attempt >= MAX_RETRIES {
                    return Err(MediaError::Rpc {
                        code: 500,
                        message: "UPLOAD_PART_REJECTED".into(),
                    });
                }
                attempt += 1;
            }
            Err(err) if is_retryable_upload_err(&err) && attempt < MAX_RETRIES => {
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    }
}

/// Uploads an in-memory byte slice using `invoker`, respecting [`UploadConfig`],
/// incremental MD5 hashing for small files, cooperative cancellation, concurrent multi-session
/// wave dispatch, failed-part retries, `FILE_PART_X_MISSING` re-queuing, and progress reporting.
pub async fn upload_bytes<I: MediaInvoker + ?Sized>(
    invoker: &I,
    filename: impl Into<String>,
    data: &[u8],
    config: UploadConfig,
    cancel: &CancelToken,
    mut on_progress: impl FnMut(UploadProgress),
) -> Result<UploadedInputFile> {
    if cancel.is_cancelled() {
        return Err(MediaError::Cancelled);
    }

    let plan = UploadPlan::new(filename, data.len() as u64, config)?;
    let parts: Vec<PlannedUploadPart> = plan.parts().collect();

    // Compute MD5 sequentially over the file for small files only (`Uploader::readDocPart` /
    // `Uploader::finishFront` in `storage/file_upload.cpp:665, 1175-1187`).
    let md5_checksum = if plan.is_big {
        String::new()
    } else {
        let mut hasher = Md5::new();
        for part in &parts {
            let start = part.offset as usize;
            let end = start + part.length as usize;
            hasher.update(&data[start..end]);
        }
        let digest = hasher.finalize();
        let mut s = String::with_capacity(32);
        for b in digest {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
        }
        s
    };

    const MAX_PART_RETRIES: usize = 3;
    let concurrency = config.max_sessions.clamp(1, MAX_UPLOAD_SESSIONS);
    let mut pending: VecDeque<u32> = (0..plan.total_parts).collect();
    let mut retries: HashMap<u32, usize> = HashMap::new();
    let mut acked: BTreeSet<u32> = BTreeSet::new();
    let mut uploaded: u64 = 0;

    while !pending.is_empty() {
        if cancel.is_cancelled() {
            return Err(MediaError::Cancelled);
        }

        let mut wave_parts = Vec::with_capacity(concurrency);
        while wave_parts.len() < concurrency {
            let Some(part_idx) = pending.pop_front() else {
                break;
            };
            if !acked.contains(&part_idx) {
                wave_parts.push(parts[part_idx as usize]);
            }
        }
        if wave_parts.is_empty() {
            break;
        }

        // Dispatch the wave concurrently across upload sessions (`Uploader::maybeSend` in
        // `storage/file_upload.cpp:871-914`), polling all futures in parallel via `join_all`.
        let mut futs = Vec::with_capacity(wave_parts.len());
        for (session_index, part) in wave_parts.iter().enumerate() {
            let start = part.offset as usize;
            let end = start + part.length as usize;
            let slice = data[start..end].to_vec();

            if part.is_big {
                futs.push(invoker.save_big_file_part(
                    session_index,
                    SaveBigFilePartRequest {
                        file_id: plan.file_id,
                        file_part: part.part_index,
                        file_total_parts: part.total_parts,
                        bytes: slice,
                    },
                ));
            } else {
                futs.push(invoker.save_file_part(
                    session_index,
                    SaveFilePartRequest {
                        file_id: plan.file_id,
                        file_part: part.part_index,
                        bytes: slice,
                    },
                ));
            }
        }

        let results = join_all(futs).await;
        for (part, res) in wave_parts.into_iter().zip(results) {
            if cancel.is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            match res {
                Ok(true) => {
                    if acked.insert(part.part_index) {
                        uploaded += u64::from(part.length);
                        on_progress(UploadProgress {
                            uploaded,
                            total: plan.file_size,
                        });
                    }
                }
                Ok(false) => {
                    let count = retries.entry(part.part_index).or_insert(0);
                    if *count >= MAX_PART_RETRIES {
                        return Err(MediaError::Rpc {
                            code: 500,
                            message: "UPLOAD_PART_REJECTED".into(),
                        });
                    }
                    *count += 1;
                    pending.push_front(part.part_index);
                }
                Err(err) => {
                    if let MediaError::Rpc { ref message, .. } = err
                        && let Some(missing_idx) = parse_missing_file_part(message)
                        && missing_idx < plan.total_parts
                    {
                        let count = retries.entry(missing_idx).or_insert(0);
                        if *count >= MAX_PART_RETRIES {
                            return Err(err);
                        }
                        *count += 1;
                        if acked.remove(&missing_idx) {
                            uploaded = uploaded
                                .saturating_sub(u64::from(parts[missing_idx as usize].length));
                        }
                        pending.push_front(missing_idx);
                        if missing_idx != part.part_index {
                            pending.push_back(part.part_index);
                        }
                        continue;
                    }
                    if is_retryable_upload_err(&err) {
                        let count = retries.entry(part.part_index).or_insert(0);
                        if *count < MAX_PART_RETRIES {
                            *count += 1;
                            pending.push_front(part.part_index);
                            continue;
                        }
                    }
                    return Err(err);
                }
            }
        }
    }

    if plan.is_big {
        Ok(UploadedInputFile::Big {
            id: plan.file_id,
            parts: plan.total_parts,
            name: plan.filename,
        })
    } else {
        Ok(UploadedInputFile::Small {
            id: plan.file_id,
            parts: plan.total_parts,
            name: plan.filename,
            md5_checksum,
        })
    }
}
