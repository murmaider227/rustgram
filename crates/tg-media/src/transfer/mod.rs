//! File transfer state machines, alignment rules, CDN crypto/hash verification,
//! `MediaInvoker` network abstraction trait, and an in-memory fake for tests.
//!
//! Port of `Telegram/SourceFiles/storage/file_download.cpp`, `file_download_mtproto.cpp`,
//! `download_manager_mtproto.cpp`, `file_upload.cpp`, and `streamed_file_downloader.cpp`.

pub mod download;
pub mod streamed;
pub mod upload;

use crate::error::{MediaError, Result};
use aes::Aes256;
use ctr::cipher::{KeyIvInit, StreamCipher};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

/// Boxed future alias used by the dyn-compatible [`MediaInvoker`] trait.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Zero-dependency concurrent `join_all` combinator for polling a wave of boxed futures in
/// parallel rather than sequentially (`DownloadManagerMtproto::checkSendNext` / `Uploader::maybeSend`).
pub(crate) fn join_all<F: Future + Unpin>(futs: Vec<F>) -> JoinAll<F> {
    JoinAll {
        elems: futs.into_iter().map(JoinSlot::Pending).collect(),
    }
}

enum JoinSlot<F: Future> {
    Pending(F),
    Done(Option<F::Output>),
}

pub(crate) struct JoinAll<F: Future + Unpin> {
    elems: Vec<JoinSlot<F>>,
}

impl<F: Future + Unpin> Unpin for JoinAll<F> {}

impl<F: Future + Unpin> Future for JoinAll<F> {
    type Output = Vec<F::Output>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let mut all_done = true;
        for slot in &mut this.elems {
            if let JoinSlot::Pending(fut) = slot {
                match Pin::new(fut).poll(cx) {
                    Poll::Ready(out) => *slot = JoinSlot::Done(Some(out)),
                    Poll::Pending => all_done = false,
                }
            }
        }
        if !all_done {
            return Poll::Pending;
        }
        let mut outputs = Vec::with_capacity(this.elems.len());
        for slot in &mut this.elems {
            if let JoinSlot::Done(out) = slot
                && let Some(val) = out.take()
            {
                outputs.push(val);
            }
        }
        Poll::Ready(outputs)
    }
}

/// Default download part size used by tdesktop (`Storage::kDownloadPartSize` = 128 KiB).
pub const DOWNLOAD_PART_SIZE: u32 = 128 * 1024;

/// Minimum recommended part size for chunked downloads (128 KiB).
pub const MIN_DOWNLOAD_PART_SIZE: u32 = 128 * 1024;

/// Maximum part size accepted by Telegram `upload.getFile` / `upload.getCdnFile` (1 MiB).
pub const MAX_DOWNLOAD_PART_SIZE: u32 = 1024 * 1024;

/// Fixed 128 KiB chunk size used by Telegram CDN SHA-256 hash verification.
pub const CDN_HASH_CHUNK_SIZE: u32 = 128 * 1024;

/// Required minimum granularity for offsets and limits in `upload.getFile` / `upload.getCdnFile` (4 KiB).
pub const OFFSET_GRANULARITY: u64 = 4096;

/// 512 KiB boundary constant used for large-part alignment rules.
pub const HALF_MEGABYTE: u64 = 512 * 1024;

/// 1 MiB boundary constant (`upload.getFile` / `upload.getCdnFile` must not cross 1 MiB boundaries).
pub const ONE_MEGABYTE: u64 = 1024 * 1024;

// ============================================================================
// Offset / part size alignment rules (upload.getFile & upload.getCdnFile)
// ============================================================================

/// Validates `(offset, limit)` for Telegram `upload.getFile`.
///
/// Rules per Telegram MTProto spec:
/// - `limit` must be non-zero, a multiple of 4 KiB, and `<= 1 MiB`.
/// - When `precise == false` (standard mode):
///   - `1 MiB % limit == 0` (powers of two: 4 KiB .. 1 MiB; in the 128 KiB..1 MiB range:
///     128 KiB, 256 KiB, 512 KiB, 1 MiB; values `> 512 KiB` must be 1 MiB, a 512 KiB multiple).
///   - `offset % limit == 0`.
/// - When `precise == true`:
///   - `offset % 4096 == 0`.
///   - `[offset, offset + limit)` must not cross a 1 MiB boundary.
pub fn validate_get_file_request(offset: u64, limit: u32, precise: bool) -> Result<()> {
    let lim = u64::from(limit);
    if lim == 0 || lim > ONE_MEGABYTE || !lim.is_multiple_of(OFFSET_GRANULARITY) {
        return Err(MediaError::InvalidTransferParam(format!(
            "upload.getFile limit {limit} must be in 4096..=1048576 and divisible by 4096"
        )));
    }
    if precise {
        if !offset.is_multiple_of(OFFSET_GRANULARITY) {
            return Err(MediaError::InvalidTransferParam(format!(
                "upload.getFile precise offset {offset} must be divisible by 4096"
            )));
        }
        let end = offset
            .checked_add(lim)
            .ok_or_else(|| MediaError::InvalidTransferParam("offset + limit overflow".into()))?;
        if offset / ONE_MEGABYTE != (end - 1) / ONE_MEGABYTE {
            return Err(MediaError::InvalidTransferParam(format!(
                "upload.getFile range [{offset}, {end}) crosses a 1 MiB boundary"
            )));
        }
    } else {
        if !ONE_MEGABYTE.is_multiple_of(lim) {
            return Err(MediaError::InvalidTransferParam(format!(
                "upload.getFile limit {limit} must evenly divide 1 MiB"
            )));
        }
        if lim > HALF_MEGABYTE && !lim.is_multiple_of(HALF_MEGABYTE) {
            return Err(MediaError::InvalidTransferParam(format!(
                "upload.getFile limit {limit} above 512 KiB must be a 512 KiB multiple"
            )));
        }
        if !offset.is_multiple_of(lim) {
            return Err(MediaError::InvalidTransferParam(format!(
                "upload.getFile offset {offset} must be aligned to limit {limit}"
            )));
        }
    }
    Ok(())
}

/// Validates `(offset, limit)` for Telegram `upload.getCdnFile`.
///
/// Rules:
/// - `limit` must be in `4096..=1048576`, divisible by 4096, and evenly divide 1 MiB.
/// - `offset` must be divisible by 4096 (and by 16 for AES-CTR counter alignment).
/// - `[offset, offset + limit)` must not cross a 1 MiB boundary.
pub fn validate_get_cdn_file_request(offset: u64, limit: u32) -> Result<()> {
    let lim = u64::from(limit);
    if lim == 0
        || lim > ONE_MEGABYTE
        || !lim.is_multiple_of(OFFSET_GRANULARITY)
        || !ONE_MEGABYTE.is_multiple_of(lim)
    {
        return Err(MediaError::InvalidTransferParam(format!(
            "upload.getCdnFile limit {limit} must divide 1 MiB and be divisible by 4096"
        )));
    }
    if !offset.is_multiple_of(OFFSET_GRANULARITY) {
        return Err(MediaError::InvalidTransferParam(format!(
            "upload.getCdnFile offset {offset} must be divisible by 4096"
        )));
    }
    // Match `DownloadMtprotoTask::sendRequest` (`storage/download_manager_mtproto.cpp:514-529`)
    // and Telegram's `upload.getCdnFile` rule: `upload.getCdnFile` has no `precise` flag, so
    // `offset` must be divisible by `limit`.
    if !offset.is_multiple_of(lim) {
        return Err(MediaError::InvalidTransferParam(format!(
            "upload.getCdnFile offset {offset} must be aligned to limit {limit}"
        )));
    }
    let end = offset
        .checked_add(lim)
        .ok_or_else(|| MediaError::InvalidTransferParam("offset + limit overflow".into()))?;
    if offset / ONE_MEGABYTE != (end - 1) / ONE_MEGABYTE {
        return Err(MediaError::InvalidTransferParam(format!(
            "upload.getCdnFile range [{offset}, {end}) crosses a 1 MiB boundary"
        )));
    }
    Ok(())
}

/// Chooses an aligned download part size in `[128 KiB ..= 1 MiB]`.
///
/// - When `allow_cdn` is `true` (tdesktop's default in `download_manager_mtproto.h`), returns
///   [`DOWNLOAD_PART_SIZE`] (128 KiB) so every downloaded part maps 1:1 to a CDN SHA-256 hash block.
/// - When `allow_cdn` is `false`, scales up through `128 KiB -> 256 KiB -> 512 KiB -> 1 MiB`
///   respecting the 512 KiB multiple rule for large transfers.
#[must_use]
pub const fn choose_download_part_size(file_size: u64, allow_cdn: bool) -> u32 {
    if allow_cdn {
        return DOWNLOAD_PART_SIZE;
    }
    if file_size <= 2 * 1024 * 1024 {
        128 * 1024
    } else if file_size <= 16 * 1024 * 1024 {
        256 * 1024
    } else if file_size <= 64 * 1024 * 1024 {
        512 * 1024
    } else {
        1024 * 1024
    }
}

/// Rounds `offset` down to the nearest multiple of `part_size`.
#[must_use]
pub const fn align_download_offset(offset: u64, part_size: u32) -> u64 {
    let ps = part_size as u64;
    match offset.checked_div(ps) {
        Some(q) => q * ps,
        None => offset,
    }
}

// ============================================================================
// CDN AES-256-CTR decryption and SHA-256 hash verification
// ============================================================================

/// Computes the per-chunk AES-256-CTR IV for a CDN file chunk at `offset`.
///
/// Exact port of `DownloadMtprotoTask::cdnPartLoaded` (`download_manager_mtproto.cpp:690-698`):
/// copies the 16-byte base IV and writes `counter_offset = (offset >> 4) as u32` in big-endian
/// into bytes `12..16`.
pub fn cdn_ctr_iv(base_iv: &[u8; 16], offset: u64) -> Result<[u8; 16]> {
    if !offset.is_multiple_of(16) {
        return Err(MediaError::InvalidTransferParam(format!(
            "CDN offset {offset} must be divisible by 16 for AES-CTR"
        )));
    }
    let counter_offset = (offset >> 4) as u32;
    let mut iv = *base_iv;
    iv[12..16].copy_from_slice(&counter_offset.to_be_bytes());
    Ok(iv)
}

/// Applies AES-256-CTR encryption/decryption in place to a CDN file chunk at `offset`.
pub fn apply_cdn_aes_ctr(
    data: &mut [u8],
    key: &[u8; 32],
    base_iv: &[u8; 16],
    offset: u64,
) -> Result<()> {
    let iv = cdn_ctr_iv(base_iv, offset)?;
    let mut cipher = ctr::Ctr128BE::<Aes256>::new(key.into(), (&iv).into());
    cipher.apply_keystream(data);
    Ok(())
}

/// A single CDN file hash entry (`fileHash` TL constructor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdnFileHash {
    /// Byte offset (multiple of 128 KiB).
    pub offset: u64,
    /// Number of bytes covered by `hash` (up to 128 KiB, smaller for the final chunk).
    pub limit: u32,
    /// Expected SHA-256 digest (32 bytes).
    pub hash: Vec<u8>,
}

/// Result of checking a decrypted CDN chunk against stored hashes (`CheckCdnHashResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckCdnHashResult {
    /// No hash covering this offset has been received yet; call `upload.getCdnFileHashes`.
    NoHash,
    /// Hash was present and did NOT match SHA-256(chunk).
    Invalid,
    /// Hash was present and matched SHA-256(chunk).
    Good,
}

/// Verifies a decrypted chunk against a single expected SHA-256 hash.
#[must_use]
pub fn verify_cdn_chunk_sha256(chunk: &[u8], expected_hash: &[u8]) -> bool {
    let digest = Sha256::digest(chunk);
    digest.as_slice() == expected_hash
}

/// Tracks CDN `fileHash` entries and verifies decrypted 128 KiB chunks (`download_manager_mtproto.cpp:724`).
#[derive(Debug, Default, Clone)]
pub struct CdnHashVerifier {
    hashes: BTreeMap<u64, CdnFileHash>,
}

impl CdnHashVerifier {
    /// Creates an empty verifier.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts hash entries returned by `upload.fileCdnRedirect`, `upload.reuploadCdnFile`,
    /// or `upload.getCdnFileHashes`.
    pub fn add_hashes(&mut self, hashes: impl IntoIterator<Item = CdnFileHash>) {
        for h in hashes {
            self.hashes.entry(h.offset).or_insert(h);
        }
    }

    /// Checks a decrypted chunk at `offset`.
    ///
    /// Supports both single 128 KiB chunks and multi-128 KiB chunks (verifying each 128 KiB sub-block).
    #[must_use]
    pub fn check(&self, offset: u64, data: &[u8]) -> CheckCdnHashResult {
        if data.len() <= CDN_HASH_CHUNK_SIZE as usize {
            let Some(entry) = self.hashes.get(&offset) else {
                return CheckCdnHashResult::NoHash;
            };
            return if verify_cdn_chunk_sha256(data, &entry.hash) {
                CheckCdnHashResult::Good
            } else {
                CheckCdnHashResult::Invalid
            };
        }

        // For larger chunks, verify every 128 KiB sub-block.
        let mut cursor = 0usize;
        while cursor < data.len() {
            let sub_offset = offset + cursor as u64;
            let end = (cursor + CDN_HASH_CHUNK_SIZE as usize).min(data.len());
            let Some(entry) = self.hashes.get(&sub_offset) else {
                return CheckCdnHashResult::NoHash;
            };
            if !verify_cdn_chunk_sha256(&data[cursor..end], &entry.hash) {
                return CheckCdnHashResult::Invalid;
            }
            cursor = end;
        }
        CheckCdnHashResult::Good
    }

    /// Clears all cached CDN hashes (used when leaving CDN or resetting params).
    pub fn clear(&mut self) {
        self.hashes.clear();
    }
}

// ============================================================================
// MediaInvoker trait & TL-independent transfer types
// ============================================================================

/// Origin context used to refresh expired `file_reference` tokens (`Data::FileOrigin`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct FileOrigin {
    /// Optional peer ID owning the message.
    pub peer_id: Option<i64>,
    /// Optional message ID.
    pub msg_id: Option<i32>,
    /// Optional sticker set ID.
    pub sticker_set_id: Option<u64>,
}

/// TL-independent file location passed to [`MediaInvoker`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TransferLocation {
    /// `inputDocumentFileLocation`.
    Document {
        /// Document ID.
        id: u64,
        /// Access hash.
        access_hash: u64,
        /// File reference bytes (may be refreshed on `FILE_REFERENCE_*` errors).
        file_reference: Vec<u8>,
        /// Optional thumbnail size letter (empty for full document).
        thumb_size: String,
    },
    /// `inputPhotoFileLocation`.
    Photo {
        /// Photo ID.
        id: u64,
        /// Access hash.
        access_hash: u64,
        /// File reference bytes.
        file_reference: Vec<u8>,
        /// Photo size letter (e.g., `"x"`, `"y"`, `"m"`).
        thumb_size: String,
    },
    /// `inputPeerPhotoFileLocation`.
    PeerPhoto {
        /// Encoded peer ID.
        peer_id: i64,
        /// Peer access hash.
        access_hash: u64,
        /// Photo ID.
        photo_id: u64,
        /// `true` for `'c'` big photo, `false` for `'a'` small photo.
        big: bool,
    },
    /// `inputStickerSetThumb`.
    StickerSetThumb {
        /// Sticker set ID.
        set_id: u64,
        /// Sticker set access hash.
        access_hash: u64,
        /// Thumbnail version.
        thumb_version: i32,
    },
    /// `inputWebFileLocation`.
    WebFile {
        /// External URL bytes.
        url: Vec<u8>,
        /// Web file access hash.
        access_hash: u64,
    },
}

impl TransferLocation {
    /// Returns the current `file_reference` bytes, if this location carries one.
    #[must_use]
    pub fn file_reference(&self) -> &[u8] {
        match self {
            Self::Document { file_reference, .. } | Self::Photo { file_reference, .. } => {
                file_reference
            }
            _ => &[],
        }
    }

    /// Updates `file_reference` in place. Returns `true` if the bytes changed.
    pub fn set_file_reference(&mut self, new_ref: Vec<u8>) -> bool {
        if new_ref.is_empty() {
            return false;
        }
        match self {
            Self::Document { file_reference, .. } | Self::Photo { file_reference, .. } => {
                if *file_reference == new_ref {
                    false
                } else {
                    *file_reference = new_ref;
                    true
                }
            }
            _ => false,
        }
    }
}

/// Request payload for `upload.getFile`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetFileRequest {
    /// File location.
    pub location: TransferLocation,
    /// Byte offset.
    pub offset: u64,
    /// Requested byte limit.
    pub limit: u32,
    /// Whether `precise` flag is set.
    pub precise: bool,
    /// Whether `cdn_supported` flag is set.
    pub cdn_supported: bool,
}

/// CDN redirect payload (`upload.fileCdnRedirect`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdnRedirect {
    /// Target CDN DC ID.
    pub dc_id: i32,
    /// Opaque CDN file token.
    pub file_token: Vec<u8>,
    /// 32-byte AES-256-CTR key.
    pub encryption_key: Vec<u8>,
    /// 16-byte AES-256-CTR base IV.
    pub encryption_iv: Vec<u8>,
    /// Initial batch of 128 KiB SHA-256 hashes.
    pub file_hashes: Vec<CdnFileHash>,
}

/// Response from `upload.getFile`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GetFileResponse {
    /// `upload.file`.
    File {
        /// Raw downloaded bytes.
        bytes: Vec<u8>,
    },
    /// `upload.fileCdnRedirect`.
    CdnRedirect(CdnRedirect),
}

/// Request payload for `upload.getWebFile`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetWebFileRequest {
    /// Web location.
    pub location: TransferLocation,
    /// Byte offset.
    pub offset: u64,
    /// Requested byte limit.
    pub limit: u32,
}

/// Response from `upload.getWebFile`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetWebFileResponse {
    /// Full file size reported by the server (`upload.webFile.size`).
    pub size: u64,
    /// MIME type reported by the server.
    pub mime_type: String,
    /// Downloaded bytes for this chunk.
    pub bytes: Vec<u8>,
}

/// Request payload for `upload.getCdnFile`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetCdnFileRequest {
    /// CDN file token from `upload.fileCdnRedirect`.
    pub file_token: Vec<u8>,
    /// Byte offset.
    pub offset: u64,
    /// Requested byte limit.
    pub limit: u32,
}

/// Response from `upload.getCdnFile`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GetCdnFileResponse {
    /// `upload.cdnFile` (AES-256-CTR encrypted bytes).
    Bytes(Vec<u8>),
    /// `upload.cdnFileReuploadNeeded` (`request_token` to send to main DC via `upload.reuploadCdnFile`).
    ReuploadNeeded {
        /// Request token for `upload.reuploadCdnFile`.
        request_token: Vec<u8>,
    },
}

/// Request payload for `upload.saveFilePart` (small files).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveFilePartRequest {
    /// Random 64-bit file ID.
    pub file_id: i64,
    /// Zero-based part index.
    pub file_part: u32,
    /// Part payload bytes.
    pub bytes: Vec<u8>,
}

/// Request payload for `upload.saveBigFilePart` (big files).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveBigFilePartRequest {
    /// Random 64-bit file ID.
    pub file_id: i64,
    /// Zero-based part index.
    pub file_part: u32,
    /// Total number of parts in the file.
    pub file_total_parts: u32,
    /// Part payload bytes.
    pub bytes: Vec<u8>,
}

/// Network abstraction implemented by `tg-api` (or [`FakeMediaInvoker`] in tests).
///
/// All download and upload state machines in `tg-media` perform network I/O exclusively
/// through this trait so `tg-media` remains independent of `tg-mtproto` and the GUI.
pub trait MediaInvoker: Send + Sync {
    /// Invokes `upload.getFile` on `(dc_id, session_index)`.
    fn get_file(
        &self,
        dc_id: i32,
        session_index: usize,
        req: GetFileRequest,
    ) -> BoxFuture<'_, Result<GetFileResponse>>;

    /// Invokes `upload.getWebFile` on `(dc_id, session_index)`.
    fn get_web_file(
        &self,
        dc_id: i32,
        session_index: usize,
        req: GetWebFileRequest,
    ) -> BoxFuture<'_, Result<GetWebFileResponse>>;

    /// Invokes `upload.getCdnFile` on `(cdn_dc_id, session_index)`.
    fn get_cdn_file(
        &self,
        cdn_dc_id: i32,
        session_index: usize,
        req: GetCdnFileRequest,
    ) -> BoxFuture<'_, Result<GetCdnFileResponse>>;

    /// Invokes `upload.getCdnFileHashes` on the file's home DC `(dc_id, session_index)`.
    fn get_cdn_file_hashes(
        &self,
        dc_id: i32,
        session_index: usize,
        file_token: Vec<u8>,
        offset: u64,
    ) -> BoxFuture<'_, Result<Vec<CdnFileHash>>>;

    /// Invokes `upload.reuploadCdnFile` on the file's home DC `(dc_id, session_index)`.
    fn reupload_cdn_file(
        &self,
        dc_id: i32,
        session_index: usize,
        file_token: Vec<u8>,
        request_token: Vec<u8>,
    ) -> BoxFuture<'_, Result<Vec<CdnFileHash>>>;

    /// Refreshes an expired `file_reference` for `location` using `origin`.
    fn refresh_file_reference(
        &self,
        origin: FileOrigin,
        location: TransferLocation,
    ) -> BoxFuture<'_, Result<Vec<u8>>>;

    /// Invokes `upload.saveFilePart` on upload session `session_index`.
    fn save_file_part(
        &self,
        session_index: usize,
        req: SaveFilePartRequest,
    ) -> BoxFuture<'_, Result<bool>>;

    /// Invokes `upload.saveBigFilePart` on upload session `session_index`.
    fn save_big_file_part(
        &self,
        session_index: usize,
        req: SaveBigFilePartRequest,
    ) -> BoxFuture<'_, Result<bool>>;
}

// ============================================================================
// FakeMediaInvoker (in-memory fake for unit and integration tests)
// ============================================================================

/// Configuration for a CDN-hosted file inside [`FakeMediaInvoker`].
#[derive(Debug, Clone)]
pub struct FakeCdnFile {
    /// CDN DC ID.
    pub cdn_dc_id: i32,
    /// Opaque CDN file token.
    pub file_token: Vec<u8>,
    /// 32-byte AES-256-CTR key.
    pub key: [u8; 32],
    /// 16-byte AES-256-CTR base IV.
    pub iv: [u8; 16],
    /// Plaintext file bytes (encrypted on the fly using `apply_cdn_aes_ctr`).
    pub plaintext: Vec<u8>,
    /// Number of initial 128 KiB hashes included in `upload.fileCdnRedirect` (`None` = all).
    pub initial_hash_count: Option<usize>,
    /// If `true`, the first `upload.getCdnFile` call returns `ReuploadNeeded` once.
    pub require_reupload_once: bool,
    /// Optional byte offset at which to corrupt a byte after encryption (tests SHA-256 failure).
    pub corrupt_offset: Option<u64>,
}

/// Recorded upload stored by [`FakeMediaInvoker`].
#[derive(Debug, Clone, Default)]
pub struct FakeUploadedFile {
    /// Whether `saveBigFilePart` was used.
    pub is_big: bool,
    /// Reported `file_total_parts` for big uploads.
    pub total_parts: Option<u32>,
    /// Uploaded parts keyed by `file_part`.
    pub parts: BTreeMap<u32, Vec<u8>>,
}

impl FakeUploadedFile {
    /// Concatenates uploaded parts in order `0..parts.len()`.
    #[must_use]
    pub fn assemble(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for bytes in self.parts.values() {
            out.extend_from_slice(bytes);
        }
        out
    }
}

#[derive(Default)]
struct FakeInvokerState {
    files: HashMap<u64, Vec<u8>>,
    web_files: HashMap<Vec<u8>, (String, Vec<u8>)>,
    cdn_by_doc: HashMap<u64, FakeCdnFile>,
    cdn_by_token: HashMap<Vec<u8>, FakeCdnFile>,
    expired_refs: HashMap<u64, Vec<u8>>,
    refreshed_refs: HashMap<u64, Vec<u8>>,
    fail_token_invalid_once: HashMap<Vec<u8>, bool>,
    uploads: HashMap<i64, FakeUploadedFile>,
    get_file_calls: usize,
    get_cdn_file_calls: usize,
    get_cdn_hashes_calls: usize,
    reupload_cdn_calls: usize,
}

/// In-memory [`MediaInvoker`] implementation for testing downloaders, streaming, CDN, and uploaders.
#[derive(Clone, Default)]
pub struct FakeMediaInvoker {
    state: Arc<Mutex<FakeInvokerState>>,
}

impl FakeMediaInvoker {
    /// Creates an empty [`FakeMediaInvoker`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a regular downloadable file (by document/photo ID).
    pub fn insert_file(&self, id: u64, bytes: Vec<u8>) {
        if let Ok(mut st) = self.state.lock() {
            st.files.insert(id, bytes);
        }
    }

    /// Registers a web file served via `upload.getWebFile`.
    pub fn insert_web_file(&self, url: Vec<u8>, mime_type: impl Into<String>, bytes: Vec<u8>) {
        if let Ok(mut st) = self.state.lock() {
            st.web_files.insert(url, (mime_type.into(), bytes));
        }
    }

    /// Registers a CDN-redirected file for `id` (also stores plaintext as fallback on home DC).
    pub fn insert_cdn_file(&self, id: u64, cdn: FakeCdnFile) {
        if let Ok(mut st) = self.state.lock() {
            st.files.insert(id, cdn.plaintext.clone());
            st.cdn_by_token.insert(cdn.file_token.clone(), cdn.clone());
            st.cdn_by_doc.insert(id, cdn);
        }
    }

    /// Configures `id` to reject `old_ref` with `400 FILE_REFERENCE_EXPIRED` and refresh to `new_ref`.
    pub fn set_file_reference_refresh(&self, id: u64, old_ref: Vec<u8>, new_ref: Vec<u8>) {
        if let Ok(mut st) = self.state.lock() {
            st.expired_refs.insert(id, old_ref);
            st.refreshed_refs.insert(id, new_ref);
        }
    }

    /// Causes the next `upload.getCdnFile` for `file_token` to fail with `FILE_TOKEN_INVALID`
    /// and disables the CDN redirect for `id` so fallback to the home DC succeeds.
    pub fn inject_cdn_token_invalid_once(&self, file_token: Vec<u8>) {
        if let Ok(mut st) = self.state.lock() {
            st.fail_token_invalid_once.insert(file_token, true);
        }
    }

    /// Returns a snapshot of an uploaded file by `file_id`.
    #[must_use]
    pub fn uploaded_file(&self, file_id: i64) -> Option<FakeUploadedFile> {
        self.state
            .lock()
            .ok()
            .and_then(|st| st.uploads.get(&file_id).cloned())
    }

    /// Returns `(get_file_calls, get_cdn_file_calls, get_cdn_hashes_calls, reupload_cdn_calls)`.
    #[must_use]
    pub fn call_counts(&self) -> (usize, usize, usize, usize) {
        self.state
            .lock()
            .map(|st| {
                (
                    st.get_file_calls,
                    st.get_cdn_file_calls,
                    st.get_cdn_hashes_calls,
                    st.reupload_cdn_calls,
                )
            })
            .unwrap_or((0, 0, 0, 0))
    }

    fn compute_hashes(plaintext: &[u8], from_offset: u64) -> Vec<CdnFileHash> {
        let mut hashes = Vec::new();
        let chunk = CDN_HASH_CHUNK_SIZE as usize;
        let mut off = (from_offset as usize / chunk) * chunk;
        while off < plaintext.len() {
            let end = (off + chunk).min(plaintext.len());
            let slice = &plaintext[off..end];
            hashes.push(CdnFileHash {
                offset: off as u64,
                limit: slice.len() as u32,
                hash: Sha256::digest(slice).to_vec(),
            });
            off = end;
        }
        hashes
    }

    fn location_id(loc: &TransferLocation) -> u64 {
        match loc {
            TransferLocation::Document { id, .. } | TransferLocation::Photo { id, .. } => *id,
            TransferLocation::PeerPhoto { photo_id, .. } => *photo_id,
            TransferLocation::StickerSetThumb { set_id, .. } => *set_id,
            TransferLocation::WebFile { access_hash, .. } => *access_hash,
        }
    }
}

impl MediaInvoker for FakeMediaInvoker {
    fn get_file(
        &self,
        _dc_id: i32,
        _session_index: usize,
        req: GetFileRequest,
    ) -> BoxFuture<'_, Result<GetFileResponse>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            validate_get_file_request(req.offset, req.limit, req.precise)?;
            let mut st = state.lock().map_err(|_| MediaError::Rpc {
                code: 500,
                message: "FAKE_LOCK_POISONED".into(),
            })?;
            st.get_file_calls += 1;
            let id = Self::location_id(&req.location);

            if let Some(expired) = st.expired_refs.get(&id)
                && req.location.file_reference() == expired.as_slice()
            {
                return Err(MediaError::Rpc {
                    code: 400,
                    message: "FILE_REFERENCE_EXPIRED".into(),
                });
            }

            if req.cdn_supported
                && let Some(cdn) = st.cdn_by_doc.get(&id).cloned()
            {
                let mut hashes = Self::compute_hashes(&cdn.plaintext, 0);
                if let Some(n) = cdn.initial_hash_count {
                    hashes.truncate(n);
                }
                return Ok(GetFileResponse::CdnRedirect(CdnRedirect {
                    dc_id: cdn.cdn_dc_id,
                    file_token: cdn.file_token,
                    encryption_key: cdn.key.to_vec(),
                    encryption_iv: cdn.iv.to_vec(),
                    file_hashes: hashes,
                }));
            }

            let Some(data) = st.files.get(&id) else {
                return Err(MediaError::Rpc {
                    code: 404,
                    message: "FILE_ID_INVALID".into(),
                });
            };
            let start = (req.offset as usize).min(data.len());
            let end = start.saturating_add(req.limit as usize).min(data.len());
            Ok(GetFileResponse::File {
                bytes: data[start..end].to_vec(),
            })
        })
    }

    fn get_web_file(
        &self,
        _dc_id: i32,
        _session_index: usize,
        req: GetWebFileRequest,
    ) -> BoxFuture<'_, Result<GetWebFileResponse>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            validate_get_file_request(req.offset, req.limit, false)?;
            let st = state.lock().map_err(|_| MediaError::Rpc {
                code: 500,
                message: "FAKE_LOCK_POISONED".into(),
            })?;
            let url = match req.location {
                TransferLocation::WebFile { url, .. } => url,
                _ => {
                    return Err(MediaError::InvalidTransferParam(
                        "expected WebFile location".into(),
                    ));
                }
            };
            let Some((mime, data)) = st.web_files.get(&url) else {
                return Err(MediaError::Rpc {
                    code: 404,
                    message: "WEBFILE_NOT_AVAILABLE".into(),
                });
            };
            let start = (req.offset as usize).min(data.len());
            let end = start.saturating_add(req.limit as usize).min(data.len());
            Ok(GetWebFileResponse {
                size: data.len() as u64,
                mime_type: mime.clone(),
                bytes: data[start..end].to_vec(),
            })
        })
    }

    fn get_cdn_file(
        &self,
        _cdn_dc_id: i32,
        _session_index: usize,
        req: GetCdnFileRequest,
    ) -> BoxFuture<'_, Result<GetCdnFileResponse>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            validate_get_cdn_file_request(req.offset, req.limit)?;
            let mut st = state.lock().map_err(|_| MediaError::Rpc {
                code: 500,
                message: "FAKE_LOCK_POISONED".into(),
            })?;
            st.get_cdn_file_calls += 1;

            if st
                .fail_token_invalid_once
                .remove(&req.file_token)
                .unwrap_or(false)
            {
                // Disable CDN redirect for matching doc so fallback to main DC succeeds.
                let token = req.file_token.clone();
                st.cdn_by_doc.retain(|_, v| v.file_token != token);
                return Err(MediaError::Rpc {
                    code: 400,
                    message: "FILE_TOKEN_INVALID".into(),
                });
            }

            let Some(cdn) = st.cdn_by_token.get_mut(&req.file_token) else {
                return Err(MediaError::Rpc {
                    code: 400,
                    message: "FILE_TOKEN_INVALID".into(),
                });
            };
            if cdn.require_reupload_once {
                cdn.require_reupload_once = false;
                return Ok(GetCdnFileResponse::ReuploadNeeded {
                    request_token: b"reupload-tok".to_vec(),
                });
            }

            let start = (req.offset as usize).min(cdn.plaintext.len());
            let end = start
                .saturating_add(req.limit as usize)
                .min(cdn.plaintext.len());
            let mut chunk = cdn.plaintext[start..end].to_vec();
            apply_cdn_aes_ctr(&mut chunk, &cdn.key, &cdn.iv, req.offset)?;
            if cdn.corrupt_offset == Some(req.offset) && !chunk.is_empty() {
                chunk[0] ^= 0xFF;
            }
            Ok(GetCdnFileResponse::Bytes(chunk))
        })
    }

    fn get_cdn_file_hashes(
        &self,
        _dc_id: i32,
        _session_index: usize,
        file_token: Vec<u8>,
        offset: u64,
    ) -> BoxFuture<'_, Result<Vec<CdnFileHash>>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let mut st = state.lock().map_err(|_| MediaError::Rpc {
                code: 500,
                message: "FAKE_LOCK_POISONED".into(),
            })?;
            st.get_cdn_hashes_calls += 1;
            let Some(cdn) = st.cdn_by_token.get(&file_token) else {
                return Err(MediaError::Rpc {
                    code: 400,
                    message: "FILE_TOKEN_INVALID".into(),
                });
            };
            Ok(Self::compute_hashes(&cdn.plaintext, offset))
        })
    }

    fn reupload_cdn_file(
        &self,
        _dc_id: i32,
        _session_index: usize,
        file_token: Vec<u8>,
        _request_token: Vec<u8>,
    ) -> BoxFuture<'_, Result<Vec<CdnFileHash>>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let mut st = state.lock().map_err(|_| MediaError::Rpc {
                code: 500,
                message: "FAKE_LOCK_POISONED".into(),
            })?;
            st.reupload_cdn_calls += 1;
            let Some(cdn) = st.cdn_by_token.get(&file_token) else {
                return Err(MediaError::Rpc {
                    code: 400,
                    message: "REQUEST_TOKEN_INVALID".into(),
                });
            };
            Ok(Self::compute_hashes(&cdn.plaintext, 0))
        })
    }

    fn refresh_file_reference(
        &self,
        _origin: FileOrigin,
        location: TransferLocation,
    ) -> BoxFuture<'_, Result<Vec<u8>>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let st = state.lock().map_err(|_| MediaError::Rpc {
                code: 500,
                message: "FAKE_LOCK_POISONED".into(),
            })?;
            let id = Self::location_id(&location);
            st.refreshed_refs
                .get(&id)
                .cloned()
                .ok_or_else(|| MediaError::Rpc {
                    code: 400,
                    message: "FILE_REFERENCE_REFRESH_FAILED".into(),
                })
        })
    }

    fn save_file_part(
        &self,
        _session_index: usize,
        req: SaveFilePartRequest,
    ) -> BoxFuture<'_, Result<bool>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let mut st = state.lock().map_err(|_| MediaError::Rpc {
                code: 500,
                message: "FAKE_LOCK_POISONED".into(),
            })?;
            let entry = st.uploads.entry(req.file_id).or_default();
            entry.is_big = false;
            entry.parts.insert(req.file_part, req.bytes);
            Ok(true)
        })
    }

    fn save_big_file_part(
        &self,
        _session_index: usize,
        req: SaveBigFilePartRequest,
    ) -> BoxFuture<'_, Result<bool>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let mut st = state.lock().map_err(|_| MediaError::Rpc {
                code: 500,
                message: "FAKE_LOCK_POISONED".into(),
            })?;
            let entry = st.uploads.entry(req.file_id).or_default();
            entry.is_big = true;
            entry.total_parts = Some(req.file_total_parts);
            entry.parts.insert(req.file_part, req.bytes);
            Ok(true)
        })
    }
}
