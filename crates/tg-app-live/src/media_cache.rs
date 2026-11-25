//! Live inline media disk cache (`tg-storage`'s [`MediaCacheIndex`]) and asynchronous
//! `upload.getFile` (with CDN redirect) background download coordinator for [`super::LiveSession`].
//!
//! Port origin:
//! - `Telegram/lib_storage/storage/cache/storage_cache_types.h`
//! - `Telegram/lib_storage/storage/cache/storage_cache_database.cpp`
//! - `Telegram/lib_storage/storage/cache/storage_cache_database_object.cpp`
//! - `Telegram/SourceFiles/storage/storage_account.cpp`
//! - `Telegram/SourceFiles/settings/sections/settings_local_storage.cpp`

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use tg_api::RpcClient;
use tg_data::{
    DocumentData, DocumentKind, ExtendedMedia, Message, MessageContent, MessageMedia, PhotoData,
};
use tg_storage::StorageEncryptionKey;
pub use tg_storage::cache::media_cache::MediaCacheTag;
use tg_storage::cache::media_cache::{
    MediaCacheIndex, read_encrypted_blob, unregister_cache_key, write_encrypted_blob,
};
use tokio::runtime::Handle;
use tracing::{debug, warn};

use crate::bridge::LiveBridgeState;
use crate::media::{
    ResolvedMediaLocation, document_thumb_location, document_to_location,
    download_file_bytes_with_progress_async, photo_thumb_location, photo_to_location,
};
use tg_app_core::core_types::dto::inline_media::{InlineMediaDescriptor, PhotoLoadState};
use tg_app_core::core_types::misc::InlineDocumentData;
use tg_app_core::info::backend::SharedMediaThumb;
use tg_app_core::settings::backend::LocalStorageStats;

/// Default tdesktop cache size limit (`Storage::Cache::details::Settings::totalSizeLimit` = 1 GiB).
pub const DEFAULT_CACHE_SIZE_LIMIT_BYTES: i64 = 1024 * 1024 * 1024;

/// Default tdesktop cache retention limit in days (`Settings::totalTimeLimit` = 31 days / 1 month).
pub const DEFAULT_CACHE_TIME_LIMIT_DAYS: i32 = 31;

/// Default tdesktop cache retention limit in seconds (`31 * 24 * 60 * 60`).
pub const DEFAULT_CACHE_TIME_LIMIT_SECS: i64 = 31 * 24 * 60 * 60;

/// Maximum single entry size in tdesktop's binlog cache (`kDataSizeLimit - 1` = ~16 MiB).
pub const MAX_CACHE_ENTRY_BYTES: usize = (1 << 24) - 1;

/// Minimum UI cache size limit in tdesktop (`Settings::kMinimalSizeLimit` = 100 MiB).
pub const MIN_CACHE_SIZE_LIMIT_BYTES: i64 = 100 * 1024 * 1024;

/// Maximum inline auto-download size for chat bubble previews (16 MiB).
pub const MAX_INLINE_AUTO_DOWNLOAD_BYTES: u64 = MAX_CACHE_ENTRY_BYTES as u64;

/// Number of seconds in one day.
pub const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

/// Retry cooldown after a failed background download so 60fps UI repaints do not spin-loop.
const FAILED_DOWNLOAD_RETRY_COOLDOWN: Duration = Duration::from_secs(5);

/// Encrypted sidecar manifest filename storing configured cache limits alongside `index.sqlite3`
/// using tdesktop's `TDEF` AES-256-CTR format so no plaintext metadata is written on disk.
const ENCRYPTED_MANIFEST_FILENAME: &str = "cache_meta.tdef";

/// Legacy plaintext manifest filename deleted automatically when opening a persistent cache.
const LEGACY_MANIFEST_FILENAME: &str = "cache_meta.json";

/// Builds the canonical cache key for a downloaded photo size.
#[must_use]
pub fn photo_cache_key(dc_id: i32, photo_id: u64, thumb_size: &str) -> String {
    format!("photo:{dc_id}:{photo_id}:{thumb_size}")
}

/// Builds the canonical cache key for a downloaded full document.
#[must_use]
pub fn document_cache_key(dc_id: i32, doc_id: u64) -> String {
    format!("doc:{dc_id}:{doc_id}")
}

/// Builds the canonical cache key for a downloaded document/video thumbnail.
#[must_use]
pub fn document_thumb_cache_key(dc_id: i32, doc_id: u64, thumb_size: &str) -> String {
    format!("docthumb:{dc_id}:{doc_id}:{thumb_size}")
}

/// Decompresses a Telegram `.tgs` (gzip-compressed Lottie JSON) payload into UTF-8 Lottie JSON.
///
/// Accepts either gzipped `.tgs` bytes or plain uncompressed UTF-8 Lottie JSON bytes.
pub fn decompress_tgs_to_lottie_json(bytes: &[u8]) -> Result<String, String> {
    if bytes.is_empty() {
        return Err("empty sticker payload".to_owned());
    }
    if tg_lottie::is_gzip(bytes) {
        let unpacked =
            tg_lottie::unpack_gzip(bytes).map_err(|e| format!("failed to gunzip .tgs: {e}"))?;
        tg_lottie::read_utf8(&unpacked)
            .map(str::to_owned)
            .map_err(|e| format!("decompressed .tgs is not valid UTF-8 Lottie JSON: {e}"))
    } else {
        tg_lottie::read_utf8(bytes)
            .map(str::to_owned)
            .map_err(|e| format!("sticker payload is not valid UTF-8: {e}"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedLimits {
    size_limit_bytes: i64,
    time_limit_days: i32,
}

#[derive(Debug, Clone, Copy)]
struct InFlightDownload {
    bytes_loaded: u64,
    total_bytes: u64,
}

/// Result of querying or scheduling an inline media download from [`LiveMediaCache`].
#[derive(Debug, Clone)]
pub enum CacheFetchState {
    /// Bytes are already available on disk / in cache.
    Ready(Vec<u8>),
    /// Download is currently in flight (either previously started or just spawned).
    Loading {
        /// Bytes received so far.
        bytes_loaded: u64,
        /// Expected total bytes (`0` if unknown).
        total_bytes: u64,
    },
    /// Download was skipped (e.g. exceeds inline size cap or in brief failure cooldown).
    Skipped,
}

struct LiveMediaCacheInner {
    index: MediaCacheIndex,
    root_dir: PathBuf,
    is_persistent: bool,
    size_limit_bytes: i64,
    time_limit_days: i32,
    in_flight: HashMap<String, InFlightDownload>,
    failed_at: HashMap<String, Instant>,
    lottie_json: HashMap<u64, String>,
}

impl LiveMediaCacheInner {
    fn from_index(index: MediaCacheIndex, root_dir: PathBuf, is_persistent: bool) -> Self {
        let mut inner = Self {
            index,
            root_dir,
            is_persistent,
            size_limit_bytes: DEFAULT_CACHE_SIZE_LIMIT_BYTES,
            time_limit_days: DEFAULT_CACHE_TIME_LIMIT_DAYS,
            in_flight: HashMap::new(),
            failed_at: HashMap::new(),
            lottie_json: HashMap::new(),
        };
        if is_persistent {
            // Purge any legacy plaintext sidecar manifest left by older builds.
            let _ = fs::remove_file(inner.root_dir.join(LEGACY_MANIFEST_FILENAME));
            inner.load_manifest();
        }
        inner
    }

    fn manifest_path(&self) -> PathBuf {
        self.root_dir.join(ENCRYPTED_MANIFEST_FILENAME)
    }

    fn load_manifest(&mut self) {
        let path = self.manifest_path();
        let Ok(raw) = read_encrypted_blob(&path, self.index.encryption_key()) else {
            return;
        };
        if let Ok(parsed) = serde_json::from_slice::<PersistedLimits>(&raw) {
            self.size_limit_bytes = parsed.size_limit_bytes.max(0);
            self.time_limit_days = parsed.time_limit_days.max(0);
        }
    }

    fn save_manifest(&self) {
        if !self.is_persistent {
            return;
        }
        let manifest = PersistedLimits {
            size_limit_bytes: self.size_limit_bytes,
            time_limit_days: self.time_limit_days,
        };
        if let Ok(json) = serde_json::to_vec(&manifest) {
            let _ = write_encrypted_blob(&self.manifest_path(), self.index.encryption_key(), &json);
        }
    }

    fn apply_limits_prune(&mut self) -> usize {
        let max_bytes = (self.size_limit_bytes > 0).then_some(self.size_limit_bytes as u64);
        let max_age = (self.time_limit_days > 0)
            .then_some(i64::from(self.time_limit_days).saturating_mul(SECONDS_PER_DAY));
        if max_bytes.is_none() && max_age.is_none() {
            return 0;
        }
        match self.index.prune(max_bytes, max_age) {
            Ok(evicted) => {
                if evicted > 0 {
                    self.save_manifest();
                }
                evicted
            }
            Err(err) => {
                warn!(error = %err, "media cache prune failed");
                0
            }
        }
    }

    fn compute_stats(&self) -> LocalStorageStats {
        let tag_stats = self.index.stats_by_tag().unwrap_or_default();
        LocalStorageStats {
            photos_bytes: i64::try_from(tag_stats.photos_bytes).unwrap_or(i64::MAX),
            videos_bytes: i64::try_from(tag_stats.videos_bytes).unwrap_or(i64::MAX),
            files_bytes: i64::try_from(tag_stats.files_bytes).unwrap_or(i64::MAX),
            stickers_bytes: i64::try_from(tag_stats.stickers_bytes).unwrap_or(i64::MAX),
            size_limit_bytes: self.size_limit_bytes,
            time_limit_days: self.time_limit_days,
        }
    }
}

/// Thread-safe live media disk cache & async download coordinator backed by `tg-storage`'s
/// encrypted [`MediaCacheIndex`].
#[derive(Clone)]
pub struct LiveMediaCache {
    inner: Arc<Mutex<LiveMediaCacheInner>>,
}

impl LiveMediaCache {
    /// Opens an encrypted persistent disk-backed [`MediaCacheIndex`] at `root_dir` using `key`.
    pub fn open_with_key(
        root_dir: impl Into<PathBuf>,
        key: StorageEncryptionKey,
    ) -> anyhow::Result<Self> {
        let dir = root_dir.into();
        let index = MediaCacheIndex::open_with_key(&dir, key)
            .map_err(|e| anyhow!("failed to open MediaCacheIndex at {}: {e}", dir.display()))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(LiveMediaCacheInner::from_index(
                index, dir, true,
            ))),
        })
    }

    /// Opens an encrypted persistent disk-backed [`MediaCacheIndex`] at `root_dir`
    /// (`index.sqlite3` + encrypted `blobs/`).
    pub fn open(root_dir: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let dir = root_dir.into();
        let index = MediaCacheIndex::open(&dir)
            .map_err(|e| anyhow!("failed to open MediaCacheIndex at {}: {e}", dir.display()))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(LiveMediaCacheInner::from_index(
                index, dir, true,
            ))),
        })
    }

    /// Opens an in-memory SQLite [`MediaCacheIndex`] storing encrypted blobs under `root_dir`.
    pub fn open_in_memory(root_dir: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let dir = root_dir.into();
        let index = MediaCacheIndex::open_in_memory(&dir)
            .map_err(|e| anyhow!("failed to open in-memory MediaCacheIndex: {e}"))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(LiveMediaCacheInner::from_index(
                index, dir, false,
            ))),
        })
    }

    /// Opens a strictly memory-only cache with zero disk or `%TEMP%` writes until an account
    /// `tdata` directory is attached via [`Self::reopen_at_with_key`] or [`Self::reopen_at`].
    #[must_use]
    pub fn open_memory_only() -> Self {
        let index = MediaCacheIndex::open_memory_only()
            .unwrap_or_else(|e| panic!("unable to initialize memory-only MediaCacheIndex: {e}"));
        Self {
            inner: Arc::new(Mutex::new(LiveMediaCacheInner::from_index(
                index,
                PathBuf::new(),
                false,
            ))),
        }
    }

    /// Opens a default memory-only cache for `session_id` before an account directory is attached.
    ///
    /// Security invariant (D1): never touches `%TEMP%` before an account directory and encryption
    /// key are known.
    #[must_use]
    pub fn open_default_for_session(_session_id: u64) -> Self {
        Self::open_memory_only()
    }

    /// Reopens this shared cache handle against a new persistent encrypted `root_dir` using
    /// `key` (derived from the account's `tdata` local key), affecting all clones of
    /// [`super::LiveSession`].
    pub fn reopen_at_with_key(
        &self,
        root_dir: impl Into<PathBuf>,
        key: StorageEncryptionKey,
    ) -> anyhow::Result<()> {
        let dir = root_dir.into();
        let index = MediaCacheIndex::open_with_key(&dir, key)
            .map_err(|e| anyhow!("failed to open MediaCacheIndex at {}: {e}", dir.display()))?;
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| anyhow!("media cache mutex poisoned"))?;
        *guard = LiveMediaCacheInner::from_index(index, dir, true);
        Ok(())
    }

    /// Reopens this shared cache handle against a new persistent `root_dir` (e.g. account `tdata`
    /// media cache folder or a test `TempDir`), affecting all clones of [`super::LiveSession`].
    pub fn reopen_at(&self, root_dir: impl Into<PathBuf>) -> anyhow::Result<()> {
        let dir = root_dir.into();
        let index = MediaCacheIndex::open(&dir)
            .map_err(|e| anyhow!("failed to open MediaCacheIndex at {}: {e}", dir.display()))?;
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| anyhow!("media cache mutex poisoned"))?;
        *guard = LiveMediaCacheInner::from_index(index, dir, true);
        Ok(())
    }

    /// Closes any on-disk SQLite and blob handles and switches this handle back to a fresh
    /// memory-only cache (called before deleting the account cache directory on logout so
    /// Windows does not hold open SQLite/WAL file handles).
    pub fn close_to_memory(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            let old_root = guard.root_dir.clone();
            if guard.is_persistent && !old_root.as_os_str().is_empty() {
                unregister_cache_key(&old_root);
            }
            if let Ok(index) = MediaCacheIndex::open_memory_only() {
                *guard = LiveMediaCacheInner::from_index(index, PathBuf::new(), false);
            }
        }
    }

    /// Returns `true` when this cache is attached to a persistent encrypted disk directory.
    #[must_use]
    pub fn is_persistent(&self) -> bool {
        self.inner.lock().is_ok_and(|g| g.is_persistent)
    }

    /// Returns the active cache root directory path (empty when in memory-only mode).
    #[must_use]
    pub fn root_dir(&self) -> PathBuf {
        self.inner
            .lock()
            .map(|g| g.root_dir.clone())
            .unwrap_or_default()
    }

    /// Reads cached bytes for `key`, updating its LRU `last_accessed` timestamp.
    #[must_use]
    pub fn get_blob(&self, key: &str) -> Option<Vec<u8>> {
        let guard = self.inner.lock().ok()?;
        guard.index.get_blob(key).ok().flatten()
    }

    /// Stores `bytes` under `key` with category `tag`, enforcing configured cache limits.
    pub fn put_blob(&self, key: &str, tag: MediaCacheTag, bytes: &[u8]) -> anyhow::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| anyhow!("media cache mutex poisoned"))?;
        let _entry = guard
            .index
            .put_blob(key, tag, bytes)
            .map_err(|e| anyhow!("MediaCacheIndex::put_blob failed: {e}"))?;
        guard.failed_at.remove(key);
        guard.apply_limits_prune();
        guard.save_manifest();
        Ok(())
    }

    /// Stores a `.tgs` animated sticker payload and caches its decompressed Lottie JSON.
    pub fn put_tgs_sticker(
        &self,
        doc_id: u64,
        key: &str,
        raw_bytes: &[u8],
    ) -> anyhow::Result<Option<String>> {
        self.put_blob(key, MediaCacheTag::Sticker, raw_bytes)?;
        match decompress_tgs_to_lottie_json(raw_bytes) {
            Ok(json) => {
                if let Ok(mut guard) = self.inner.lock() {
                    guard.lottie_json.insert(doc_id, json.clone());
                }
                Ok(Some(json))
            }
            Err(err) => {
                debug!(doc_id, error = %err, "unable to decompress .tgs sticker");
                Ok(None)
            }
        }
    }

    /// Returns cached decompressed Lottie JSON for `doc_id` if already available in memory or on disk.
    #[must_use]
    pub fn cached_lottie_json_for_doc(&self, doc: &DocumentData) -> Option<String> {
        if doc.kind != DocumentKind::AnimatedSticker {
            return None;
        }
        let doc_id = doc.id.0;
        {
            let guard = self.inner.lock().ok()?;
            if let Some(json) = guard.lottie_json.get(&doc_id) {
                return Some(json.clone());
            }
        }
        // Check persistent disk cache under this document's cache key (try both doc.dc_id and 0).
        let key = document_cache_key(doc.dc_id, doc_id);
        let bytes = self.get_blob(&key)?;
        let json = decompress_tgs_to_lottie_json(&bytes).ok()?;
        if let Ok(mut guard) = self.inner.lock() {
            guard.lottie_json.insert(doc_id, json.clone());
        }
        Some(json)
    }

    /// Returns whether `key` currently has an active background download in progress.
    #[must_use]
    pub fn is_in_flight(&self, key: &str) -> bool {
        self.inner
            .lock()
            .map(|g| g.in_flight.contains_key(key))
            .unwrap_or(false)
    }

    /// Returns real [`LocalStorageStats`] computed from [`MediaCacheIndex`].
    #[must_use]
    pub fn stats(&self) -> LocalStorageStats {
        self.inner
            .lock()
            .map(|g| g.compute_stats())
            .unwrap_or(LocalStorageStats {
                photos_bytes: 0,
                videos_bytes: 0,
                files_bytes: 0,
                stickers_bytes: 0,
                size_limit_bytes: DEFAULT_CACHE_SIZE_LIMIT_BYTES,
                time_limit_days: DEFAULT_CACHE_TIME_LIMIT_DAYS,
            })
    }

    /// Evicts every cached blob from [`MediaCacheIndex`] and returns total bytes freed.
    pub fn clear(&self) -> i64 {
        let Ok(mut guard) = self.inner.lock() else {
            return 0;
        };
        let freed = guard.compute_stats().total_bytes();
        if let Err(err) = guard.index.prune(Some(0), None) {
            warn!(error = %err, "MediaCacheIndex clear prune failed");
        }
        guard.lottie_json.clear();
        guard.failed_at.clear();
        guard.save_manifest();
        freed
    }

    /// Updates local storage size (`0` = unlimited) and retention time (`0` = forever) limits
    /// and immediately prunes [`MediaCacheIndex`] according to tdesktop's `updateSettings` behavior.
    pub fn set_limits(&self, size_limit_bytes: i64, time_limit_days: i32) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        guard.size_limit_bytes = size_limit_bytes.max(0);
        guard.time_limit_days = time_limit_days.max(0);
        guard.apply_limits_prune();
        guard.save_manifest();
    }

    /// Triggers an immediate LRU/age prune using the active limits, returning evicted entry count.
    pub fn prune_now(&self) -> usize {
        self.inner
            .lock()
            .map(|mut g| g.apply_limits_prune())
            .unwrap_or(0)
    }

    /// Returns cached bytes for `key`, or spawns an asynchronous `upload.getFile` (with CDN
    /// redirect support) task on `handle` if not yet cached or in-flight.
    #[allow(clippy::too_many_arguments)]
    pub fn fetch_or_spawn<C: RpcClient + Clone + 'static>(
        &self,
        key: String,
        tag: MediaCacheTag,
        resolved: ResolvedMediaLocation,
        tgs_doc_id: Option<u64>,
        rpc: &C,
        handle: &Handle,
        bridge: &Arc<LiveBridgeState>,
    ) -> CacheFetchState {
        if resolved.size > MAX_INLINE_AUTO_DOWNLOAD_BYTES {
            return CacheFetchState::Skipped;
        }

        // Fast-path: check cache, in-flight map, and failure cooldown under a single lock.
        {
            let Ok(mut guard) = self.inner.lock() else {
                return CacheFetchState::Skipped;
            };
            if let Ok(Some(bytes)) = guard.index.get_blob(&key) {
                if let Some(doc_id) = tgs_doc_id
                    && !guard.lottie_json.contains_key(&doc_id)
                    && let Ok(json) = decompress_tgs_to_lottie_json(&bytes)
                {
                    guard.lottie_json.insert(doc_id, json);
                }
                return CacheFetchState::Ready(bytes);
            }
            if let Some(flight) = guard.in_flight.get(&key) {
                return CacheFetchState::Loading {
                    bytes_loaded: flight.bytes_loaded,
                    total_bytes: flight.total_bytes,
                };
            }
            if let Some(&failed) = guard.failed_at.get(&key)
                && failed.elapsed() < FAILED_DOWNLOAD_RETRY_COOLDOWN
            {
                return CacheFetchState::Skipped;
            }
            guard.in_flight.insert(
                key.clone(),
                InFlightDownload {
                    bytes_loaded: 0,
                    total_bytes: resolved.size,
                },
            );
        }

        let cache = self.clone();
        let rpc = rpc.clone();
        let bridge = Arc::clone(bridge);
        let key_task = key.clone();
        let expected_size = (resolved.size > 0).then_some(resolved.size);
        let total_hint = resolved.size;

        handle.spawn(async move {
            let progress_cache = cache.clone();
            let progress_key = key_task.clone();
            let progress_bridge = Arc::clone(&bridge);

            let res = download_file_bytes_with_progress_async(
                &rpc,
                resolved.dc_id,
                resolved.location,
                expected_size,
                move |downloaded, total| {
                    if let Ok(mut guard) = progress_cache.inner.lock()
                        && let Some(slot) = guard.in_flight.get_mut(&progress_key)
                    {
                        slot.bytes_loaded = downloaded;
                        if total > 0 {
                            slot.total_bytes = total;
                        }
                    }
                    progress_bridge.notify_ui();
                },
            )
            .await;

            match res {
                Ok(bytes) => {
                    let store_res = if let Some(doc_id) = tgs_doc_id {
                        cache.put_tgs_sticker(doc_id, &key_task, &bytes).map(|_| ())
                    } else {
                        cache.put_blob(&key_task, tag, &bytes)
                    };
                    if let Err(err) = store_res {
                        warn!(key = %key_task, error = %err, "failed to store downloaded media blob");
                    }
                    if let Ok(mut guard) = cache.inner.lock() {
                        guard.in_flight.remove(&key_task);
                    }
                    bridge.notify_ui();
                }
                Err(err) => {
                    debug!(key = %key_task, error = %err, "background inline media download failed");
                    if let Ok(mut guard) = cache.inner.lock() {
                        guard.in_flight.remove(&key_task);
                        guard.failed_at.insert(key_task, Instant::now());
                    }
                    bridge.notify_ui();
                }
            }
        });

        CacheFetchState::Loading {
            bytes_loaded: 0,
            total_bytes: total_hint,
        }
    }

    /// Builds an [`InlineMediaDescriptor`] for `msg`, serving bytes immediately from
    /// [`MediaCacheIndex`] or kicking off an async `upload.getFile` download on `handle`.
    pub fn inline_media_descriptor_for_message<C: RpcClient + Clone + 'static>(
        &self,
        msg: &Message,
        rpc: &C,
        handle: &Handle,
        bridge: &Arc<LiveBridgeState>,
    ) -> Option<InlineMediaDescriptor> {
        let MessageContent::Regular { media, .. } = &msg.content else {
            return None;
        };
        self.descriptor_for_media(media, rpc, handle, bridge)
    }

    fn descriptor_for_media<C: RpcClient + Clone + 'static>(
        &self,
        media: &MessageMedia,
        rpc: &C,
        handle: &Handle,
        bridge: &Arc<LiveBridgeState>,
    ) -> Option<InlineMediaDescriptor> {
        match media {
            MessageMedia::Photo(photo) => {
                Some(self.descriptor_for_photo(photo, rpc, handle, bridge))
            }
            MessageMedia::Document(doc) => self.descriptor_for_document(doc, rpc, handle, bridge),
            MessageMedia::WebPage(wp) => {
                if let Some(ref photo) = wp.photo {
                    Some(self.descriptor_for_photo(photo, rpc, handle, bridge))
                } else if let Some(ref doc) = wp.document {
                    self.descriptor_for_document(doc, rpc, handle, bridge)
                } else {
                    None
                }
            }
            MessageMedia::Game(game) => {
                if let Some(ref doc) = game.document {
                    self.descriptor_for_document(doc, rpc, handle, bridge)
                } else {
                    game.photo
                        .as_ref()
                        .map(|photo| self.descriptor_for_photo(photo, rpc, handle, bridge))
                }
            }
            MessageMedia::PaidMedia(paid) => {
                for item in &paid.extended_media {
                    if let ExtendedMedia::Media(inner) = item {
                        return self.descriptor_for_media(inner, rpc, handle, bridge);
                    }
                }
                if !paid.extended_media.is_empty() {
                    Some(InlineMediaDescriptor {
                        has_spoiler: true,
                        ..InlineMediaDescriptor::default()
                    })
                } else {
                    None
                }
            }
            MessageMedia::Invoice(inv) => match inv.extended_media.as_deref() {
                Some(ExtendedMedia::Media(inner)) => {
                    self.descriptor_for_media(inner, rpc, handle, bridge)
                }
                Some(ExtendedMedia::Preview { .. }) => Some(InlineMediaDescriptor {
                    has_spoiler: true,
                    ..InlineMediaDescriptor::default()
                }),
                None => None,
            },
            _ => None,
        }
    }

    fn descriptor_for_photo<C: RpcClient + Clone + 'static>(
        &self,
        photo: &PhotoData,
        rpc: &C,
        handle: &Handle,
        bridge: &Arc<LiveBridgeState>,
    ) -> InlineMediaDescriptor {
        let resolved = photo_to_location(photo);
        let thumb_size = match &resolved.location {
            tg_tl::enums::InputFileLocation::InputPhotoFileLocation(loc) => loc.thumb_size.as_str(),
            _ => "y",
        };
        let key = photo_cache_key(resolved.dc_id, photo.id.0, thumb_size);
        let photo_load = match self.fetch_or_spawn(
            key,
            MediaCacheTag::Photo,
            resolved,
            None,
            rpc,
            handle,
            bridge,
        ) {
            CacheFetchState::Ready(bytes) => Some(PhotoLoadState::Ready { bytes }),
            CacheFetchState::Loading {
                bytes_loaded,
                total_bytes,
            } => Some(PhotoLoadState::Loading {
                progress: (total_bytes > 0)
                    .then(|| (bytes_loaded as f32 / total_bytes as f32).clamp(0.0, 1.0)),
                bytes_loaded,
                total_bytes,
            }),
            CacheFetchState::Skipped => None,
        };
        InlineMediaDescriptor {
            has_spoiler: photo.spoiler,
            photo_load,
            ..InlineMediaDescriptor::default()
        }
    }

    fn descriptor_for_document<C: RpcClient + Clone + 'static>(
        &self,
        doc: &DocumentData,
        rpc: &C,
        handle: &Handle,
        bridge: &Arc<LiveBridgeState>,
    ) -> Option<InlineMediaDescriptor> {
        let mut desc = InlineMediaDescriptor {
            has_spoiler: doc.spoiler,
            ..InlineMediaDescriptor::default()
        };

        match doc.kind {
            DocumentKind::Gif => {
                let resolved = document_to_location(doc);
                let key = document_cache_key(resolved.dc_id, doc.id.0);
                match self.fetch_or_spawn(
                    key,
                    MediaCacheTag::Video,
                    resolved,
                    None,
                    rpc,
                    handle,
                    bridge,
                ) {
                    CacheFetchState::Ready(bytes) => {
                        desc.video_data = Some(InlineDocumentData::Bytes(bytes));
                    }
                    CacheFetchState::Loading { .. } | CacheFetchState::Skipped => {
                        self.populate_doc_thumb(doc, &mut desc, rpc, handle, bridge);
                    }
                }
                Some(desc)
            }
            DocumentKind::Video => {
                // Check if the full video is already cached; otherwise fetch its thumbnail.
                let full_key = document_cache_key(doc.dc_id, doc.id.0);
                if let Some(video_bytes) = self.get_blob(&full_key) {
                    desc.video_data = Some(InlineDocumentData::Bytes(video_bytes));
                }
                self.populate_doc_thumb(doc, &mut desc, rpc, handle, bridge);
                if desc.video_data.is_some() || desc.photo_load.is_some() || desc.has_spoiler {
                    Some(desc)
                } else {
                    None
                }
            }
            DocumentKind::RoundVideo => {
                let resolved = document_to_location(doc);
                let ext = resolved.ext_hint.clone().or_else(|| Some("mp4".to_owned()));
                let key = document_cache_key(resolved.dc_id, doc.id.0);
                match self.fetch_or_spawn(
                    key,
                    MediaCacheTag::Voice,
                    resolved,
                    None,
                    rpc,
                    handle,
                    bridge,
                ) {
                    CacheFetchState::Ready(bytes) => {
                        desc.video_data = Some(InlineDocumentData::Bytes(bytes.clone()));
                        desc.audio_bytes = Some(bytes);
                        desc.audio_ext = ext;
                    }
                    CacheFetchState::Loading { .. } | CacheFetchState::Skipped => {
                        self.populate_doc_thumb(doc, &mut desc, rpc, handle, bridge);
                    }
                }
                Some(desc)
            }
            DocumentKind::Voice => {
                let resolved = document_to_location(doc);
                let ext = resolved.ext_hint.clone().or_else(|| Some("ogg".to_owned()));
                let key = document_cache_key(resolved.dc_id, doc.id.0);
                if let CacheFetchState::Ready(bytes) = self.fetch_or_spawn(
                    key,
                    MediaCacheTag::Voice,
                    resolved,
                    None,
                    rpc,
                    handle,
                    bridge,
                ) {
                    desc.audio_bytes = Some(bytes);
                    desc.audio_ext = ext;
                }
                Some(desc)
            }
            DocumentKind::Audio => {
                desc.music_title = doc.audio_title.clone();
                desc.music_performer = doc.audio_performer.clone();
                desc.music_duration_secs = doc.duration.map(|d| d.max(0.0).round() as u32);
                let resolved = document_to_location(doc);
                let ext = resolved.ext_hint.clone();
                let key = document_cache_key(resolved.dc_id, doc.id.0);
                if let Some(bytes) = self.get_blob(&key) {
                    desc.audio_bytes = Some(bytes);
                    desc.audio_ext = ext;
                }
                self.populate_doc_thumb(doc, &mut desc, rpc, handle, bridge);
                Some(desc)
            }
            DocumentKind::AnimatedSticker => {
                let resolved = document_to_location(doc);
                let key = document_cache_key(resolved.dc_id, doc.id.0);
                let _ = self.fetch_or_spawn(
                    key,
                    MediaCacheTag::Sticker,
                    resolved,
                    Some(doc.id.0),
                    rpc,
                    handle,
                    bridge,
                );
                Some(desc)
            }
            DocumentKind::VideoSticker => {
                desc.is_video_sticker = true;
                let resolved = document_to_location(doc);
                let key = document_cache_key(resolved.dc_id, doc.id.0);
                if let CacheFetchState::Ready(bytes) = self.fetch_or_spawn(
                    key,
                    MediaCacheTag::Sticker,
                    resolved,
                    None,
                    rpc,
                    handle,
                    bridge,
                ) {
                    desc.video_data = Some(InlineDocumentData::Bytes(bytes));
                }
                Some(desc)
            }
            DocumentKind::Sticker => {
                let resolved = document_to_location(doc);
                let key = document_cache_key(resolved.dc_id, doc.id.0);
                desc.photo_load = match self.fetch_or_spawn(
                    key,
                    MediaCacheTag::Sticker,
                    resolved,
                    None,
                    rpc,
                    handle,
                    bridge,
                ) {
                    CacheFetchState::Ready(bytes) => Some(PhotoLoadState::Ready { bytes }),
                    CacheFetchState::Loading {
                        bytes_loaded,
                        total_bytes,
                    } => Some(PhotoLoadState::Loading {
                        progress: (total_bytes > 0)
                            .then(|| (bytes_loaded as f32 / total_bytes as f32).clamp(0.0, 1.0)),
                        bytes_loaded,
                        total_bytes,
                    }),
                    CacheFetchState::Skipped => None,
                };
                Some(desc)
            }
            DocumentKind::File | DocumentKind::WallPaper => {
                self.populate_doc_thumb(doc, &mut desc, rpc, handle, bridge);
                if desc.photo_load.is_some() || desc.has_spoiler {
                    Some(desc)
                } else {
                    None
                }
            }
        }
    }

    fn populate_doc_thumb<C: RpcClient + Clone + 'static>(
        &self,
        doc: &DocumentData,
        desc: &mut InlineMediaDescriptor,
        rpc: &C,
        handle: &Handle,
        bridge: &Arc<LiveBridgeState>,
    ) {
        let Some(thumb_loc) = document_thumb_location(doc) else {
            return;
        };
        let thumb_size = match &thumb_loc.location {
            tg_tl::enums::InputFileLocation::InputDocumentFileLocation(loc) => {
                loc.thumb_size.as_str()
            }
            _ => "m",
        };
        let key = document_thumb_cache_key(thumb_loc.dc_id, doc.id.0, thumb_size);
        desc.photo_load = match self.fetch_or_spawn(
            key,
            MediaCacheTag::Photo,
            thumb_loc,
            None,
            rpc,
            handle,
            bridge,
        ) {
            CacheFetchState::Ready(bytes) => Some(PhotoLoadState::Ready { bytes }),
            CacheFetchState::Loading {
                bytes_loaded,
                total_bytes,
            } => Some(PhotoLoadState::Loading {
                progress: (total_bytes > 0)
                    .then(|| (bytes_loaded as f32 / total_bytes as f32).clamp(0.0, 1.0)),
                bytes_loaded,
                total_bytes,
            }),
            CacheFetchState::Skipped => None,
        };
    }

    /// Loads or asynchronously fetches a grid thumbnail for a profile shared-media message
    /// (photo, video, GIF, round video, or thumbed document / webpage photo) without blocking
    /// the UI thread (`info_media_list_widget.cpp`).
    pub fn shared_media_thumb_for_message<C: RpcClient + Clone + 'static>(
        &self,
        msg: &Message,
        rpc: &C,
        handle: &Handle,
        bridge: &Arc<LiveBridgeState>,
    ) -> SharedMediaThumb {
        let MessageContent::Regular { media, .. } = &msg.content else {
            return SharedMediaThumb::Unavailable;
        };
        self.shared_media_thumb_for_media(media, rpc, handle, bridge)
    }

    fn shared_media_thumb_for_media<C: RpcClient + Clone + 'static>(
        &self,
        media: &MessageMedia,
        rpc: &C,
        handle: &Handle,
        bridge: &Arc<LiveBridgeState>,
    ) -> SharedMediaThumb {
        match media {
            MessageMedia::Photo(photo) => {
                self.shared_media_thumb_for_photo(photo, rpc, handle, bridge)
            }
            MessageMedia::Document(doc) => {
                self.shared_media_thumb_for_document(doc, rpc, handle, bridge)
            }
            MessageMedia::WebPage(wp) => {
                if let Some(ref photo) = wp.photo {
                    self.shared_media_thumb_for_photo(photo, rpc, handle, bridge)
                } else if let Some(ref doc) = wp.document {
                    self.shared_media_thumb_for_document(doc, rpc, handle, bridge)
                } else {
                    SharedMediaThumb::Unavailable
                }
            }
            MessageMedia::Game(game) => {
                if let Some(ref photo) = game.photo {
                    self.shared_media_thumb_for_photo(photo, rpc, handle, bridge)
                } else if let Some(ref doc) = game.document {
                    self.shared_media_thumb_for_document(doc, rpc, handle, bridge)
                } else {
                    SharedMediaThumb::Unavailable
                }
            }
            MessageMedia::PaidMedia(paid) => {
                for item in &paid.extended_media {
                    if let ExtendedMedia::Media(inner) = item {
                        return self.shared_media_thumb_for_media(inner, rpc, handle, bridge);
                    }
                }
                SharedMediaThumb::Unavailable
            }
            _ => SharedMediaThumb::Unavailable,
        }
    }

    fn shared_media_thumb_for_photo<C: RpcClient + Clone + 'static>(
        &self,
        photo: &PhotoData,
        rpc: &C,
        handle: &Handle,
        bridge: &Arc<LiveBridgeState>,
    ) -> SharedMediaThumb {
        let Some(resolved) = photo_thumb_location(photo) else {
            return match photo.stripped_thumb.clone() {
                Some(stripped) if !stripped.is_empty() => SharedMediaThumb::Loading {
                    stripped_thumb: Some(stripped),
                },
                _ => SharedMediaThumb::Unavailable,
            };
        };
        let thumb_size = match &resolved.location {
            tg_tl::enums::InputFileLocation::InputPhotoFileLocation(loc) => loc.thumb_size.as_str(),
            _ => "m",
        };
        let key = photo_cache_key(resolved.dc_id, photo.id.0, thumb_size);
        match self.fetch_or_spawn(
            key,
            MediaCacheTag::Photo,
            resolved,
            None,
            rpc,
            handle,
            bridge,
        ) {
            CacheFetchState::Ready(bytes) => SharedMediaThumb::Ready { bytes },
            CacheFetchState::Loading { .. } | CacheFetchState::Skipped => {
                SharedMediaThumb::Loading {
                    stripped_thumb: photo.stripped_thumb.clone(),
                }
            }
        }
    }

    fn shared_media_thumb_for_document<C: RpcClient + Clone + 'static>(
        &self,
        doc: &DocumentData,
        rpc: &C,
        handle: &Handle,
        bridge: &Arc<LiveBridgeState>,
    ) -> SharedMediaThumb {
        let Some(thumb_loc) = document_thumb_location(doc) else {
            return match doc.stripped_thumb.clone() {
                Some(stripped) if !stripped.is_empty() => SharedMediaThumb::Loading {
                    stripped_thumb: Some(stripped),
                },
                _ => SharedMediaThumb::Unavailable,
            };
        };
        let thumb_size = match &thumb_loc.location {
            tg_tl::enums::InputFileLocation::InputDocumentFileLocation(loc) => {
                loc.thumb_size.as_str()
            }
            _ => "m",
        };
        let key = document_thumb_cache_key(thumb_loc.dc_id, doc.id.0, thumb_size);
        match self.fetch_or_spawn(
            key,
            MediaCacheTag::Photo,
            thumb_loc,
            None,
            rpc,
            handle,
            bridge,
        ) {
            CacheFetchState::Ready(bytes) => SharedMediaThumb::Ready { bytes },
            CacheFetchState::Loading { .. } | CacheFetchState::Skipped => {
                SharedMediaThumb::Loading {
                    stripped_thumb: doc.stripped_thumb.clone(),
                }
            }
        }
    }
}

/// Checks whether a cache directory path has an initialized `index.sqlite3`.
#[must_use]
pub fn has_persisted_index(dir: &Path) -> bool {
    dir.join("index.sqlite3").exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_storage::AuthKey;
    use tg_storage::cache::media_cache::derive_media_cache_key;

    #[test]
    fn d1_default_session_cache_never_uses_temp_dir_and_encrypts_on_attach() {
        let session_id = 0xDEAD_BEEF_1234_5678u64;
        let legacy_temp = std::env::temp_dir().join(format!("rustgram_media_cache_{session_id}"));
        let _ = fs::remove_dir_all(&legacy_temp);

        let cache = LiveMediaCache::open_default_for_session(session_id);
        assert!(
            !cache.is_persistent(),
            "unattached session cache must be memory-only"
        );
        assert_eq!(cache.root_dir(), PathBuf::new());
        cache
            .put_blob("photo:2:1:m", MediaCacheTag::Photo, b"RAM_ONLY_SECRET")
            .expect("put in memory");
        assert_eq!(
            cache.get_blob("photo:2:1:m").as_deref(),
            Some(&b"RAM_ONLY_SECRET"[..])
        );
        assert!(
            !legacy_temp.exists(),
            "unattached session cache must NEVER create %TEMP%\\rustgram_media_cache_<id>"
        );

        // Attach to an account directory with a derived key and verify disk encryption.
        let dir = tempfile::TempDir::new().expect("tempdir");
        let local_key = AuthKey::new([0x3Cu8; 256]);
        let key = derive_media_cache_key(&local_key, "data");
        cache
            .reopen_at_with_key(dir.path(), key.clone())
            .expect("attach encrypted dir");
        assert!(cache.is_persistent());

        let secret = b"ATTACHED_ENCRYPTED_MEDIA_PAYLOAD_XYZ";
        cache
            .put_blob("photo:2:99:x", MediaCacheTag::Photo, secret)
            .expect("put encrypted");
        cache.set_limits(500 * 1024 * 1024, 14);

        assert!(
            !dir.path().join(LEGACY_MANIFEST_FILENAME).exists(),
            "plaintext cache_meta.json must not be created"
        );
        assert!(
            dir.path().join(ENCRYPTED_MANIFEST_FILENAME).exists(),
            "encrypted cache_meta.tdef must be created"
        );
        let manifest_raw =
            fs::read(dir.path().join(ENCRYPTED_MANIFEST_FILENAME)).expect("manifest");
        assert!(manifest_raw.starts_with(b"TDEF"));

        // Reopen with same derived key -> limits and blob survive.
        let reopened = LiveMediaCache::open_with_key(dir.path(), key).expect("reopen");
        assert_eq!(
            reopened.get_blob("photo:2:99:x").as_deref(),
            Some(&secret[..])
        );
        assert_eq!(reopened.stats().size_limit_bytes, 500 * 1024 * 1024);
        assert_eq!(reopened.stats().time_limit_days, 14);
        assert_eq!(reopened.stats().photos_bytes, secret.len() as i64);

        // Closing to memory releases disk handles cleanly.
        reopened.close_to_memory();
        cache.close_to_memory();
        assert!(!cache.is_persistent());
    }
}
