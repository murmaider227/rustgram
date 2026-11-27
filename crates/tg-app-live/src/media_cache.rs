//! Live inline media disk cache (`tg-storage`'s [`MediaCacheIndex`]) and asynchronous
//! `upload.getFile` (with CDN redirect) background download coordinator for [`super::LiveSession`].
//!
//! The encrypted index (SQLite, blob files, crypto) belongs to one actor thread ([`store`]);
//! callers keep a small in-memory layer (recently used blobs as `Arc<[u8]>`, blobs still being
//! written, known misses, in-flight downloads) behind a mutex that is never held across I/O.
//!
//! Port origin:
//! - `Telegram/lib_storage/storage/cache/storage_cache_types.h`
//! - `Telegram/lib_storage/storage/cache/storage_cache_database.cpp`
//! - `Telegram/lib_storage/storage/cache/storage_cache_database_object.cpp`
//! - `Telegram/SourceFiles/storage/storage_account.cpp`
//! - `Telegram/SourceFiles/settings/sections/settings_local_storage.cpp`

mod lru;
mod refresh;
mod store;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::anyhow;
use tg_api::RpcClient;
use tg_data::{
    DocumentData, DocumentKind, ExtendedMedia, Message, MessageContent, MessageMedia, PhotoData,
};
use tg_storage::StorageEncryptionKey;
use tg_storage::cache::legacy_temp::sweep_legacy_temp_dirs_once;
pub use tg_storage::cache::media_cache::MediaCacheTag;
use tg_storage::cache::media_cache::{MediaCacheIndex, register_cache_key, unregister_cache_key};
use tokio::runtime::Handle;
use tracing::{debug, warn};

use self::lru::ByteLru;
pub use self::refresh::RefreshOrigin;
use self::store::{Priority, Store, StoreActor};
use crate::bridge::LiveBridgeState;
use crate::media::{
    ResolvedMediaLocation, document_thumb_location, document_to_location, photo_thumb_location,
    photo_to_location,
};
use tg_app_core::core_types::dto::inline_media::{
    InlineMediaDescriptor, PhotoLoadState, VoiceLoadState,
};
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

/// Memory budget for recently read or downloaded blobs served without touching the disk.
const HOT_BLOB_BUDGET_BYTES: usize = 32 * 1024 * 1024;

/// Blobs larger than this are not kept by blocking reads (they would push out many photos).
const HOT_BLOB_MAX_ENTRY_BYTES: usize = 8 * 1024 * 1024;

/// Memory budget for decompressed Lottie JSON of animated stickers.
const LOTTIE_JSON_BUDGET_BYTES: usize = 16 * 1024 * 1024;

/// The set of keys known to be absent from the disk cache is reset when it grows past this.
const KNOWN_MISSES_CAP: usize = 4096;

/// Identity of the [`Store`] (the opened index) a result came from: a reopen or logout gives
/// the cache a new store, and results of the previous one are dropped.
static NEXT_STORE_ID: AtomicU64 = AtomicU64::new(1);

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

#[derive(Debug)]
struct InFlightDownload {
    bytes_loaded: u64,
    total_bytes: u64,
    /// Stops the download task ([`LiveMediaCache::cancel_download`]); set right after the
    /// task is spawned.
    abort: Option<tokio::task::AbortHandle>,
    /// Which download task this slot belongs to: a task that was cancelled (and maybe
    /// replaced by a new download of the same key) must not report into the slot.
    token: u64,
}

static NEXT_FLIGHT_TOKEN: AtomicU64 = AtomicU64::new(1);

/// Result of querying or scheduling an inline media download from [`LiveMediaCache`].
#[derive(Debug, Clone)]
pub enum CacheFetchState {
    /// Bytes are already available (memory or disk cache).
    Ready(Arc<[u8]>),
    /// Download is currently in flight (either previously started or just spawned).
    Loading {
        /// Bytes received so far.
        bytes_loaded: u64,
        /// Expected total bytes (`0` if unknown).
        total_bytes: u64,
    },
    /// The disk cache is still being read (only from non-blocking lookups); the UI is notified
    /// when the read finishes, and the next lookup returns `Ready` or `Loading`.
    Pending,
    /// Download was skipped (e.g. exceeds inline size cap or in brief failure cooldown).
    Skipped,
}

/// Whether a lookup that has to go to disk waits for the read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadMode {
    /// Wait for the actor (a short read; reads are served before queued writes).
    Blocking,
    /// Queue the read and return [`CacheFetchState::Pending`] at once.
    NonBlocking,
}

/// Work counters of a [`LiveMediaCache`], for tests and measurements.
#[derive(Debug, Default)]
pub struct MediaCacheCounters {
    disk_reads: AtomicU64,
    memory_hits: AtomicU64,
    blocking_reads: AtomicU64,
}

impl MediaCacheCounters {
    /// Blobs read and decrypted from disk (on the cache thread).
    #[must_use]
    pub fn disk_reads(&self) -> u64 {
        self.disk_reads.load(Ordering::Relaxed)
    }

    /// Lookups answered from memory without the cache thread.
    #[must_use]
    pub fn memory_hits(&self) -> u64 {
        self.memory_hits.load(Ordering::Relaxed)
    }

    /// Lookups whose caller waited for a disk read.
    #[must_use]
    pub fn blocking_reads(&self) -> u64 {
        self.blocking_reads.load(Ordering::Relaxed)
    }
}

/// In-memory state shared by all clones of a [`LiveMediaCache`]. Never locked across I/O.
struct Shared {
    store_id: u64,
    root_dir: PathBuf,
    is_persistent: bool,
    /// `true` when the index was opened with the account's storage key (derived from the `tdata`
    /// local key), `false` for a memory-only cache or one using a random per-process key.
    has_account_key: bool,
    encryption_key: StorageEncryptionKey,
    /// Folders outside the cache root that were given the account key
    /// ([`LiveMediaCache::register_account_dir`]).
    account_dirs: Vec<PathBuf>,
    in_flight: HashMap<String, InFlightDownload>,
    failed_at: HashMap<String, Instant>,
    lottie_json: ByteLru<u64, Arc<str>>,
    /// Recently read or downloaded blobs.
    hot: ByteLru<String, Arc<[u8]>>,
    /// Downloaded blobs queued for the disk write, served from memory until it lands.
    writing: HashMap<String, Arc<[u8]>>,
    /// Keys the disk cache was found not to have (cleared when they are stored).
    misses: HashSet<String>,
    /// Keys with a queued non-blocking read.
    reading: HashSet<String>,
    /// Keys whose download the user cancelled (`PhotoData::cancel`, kept for the session like
    /// tdesktop's `CancelledFileLoader`): not downloaded automatically again.
    cancelled: HashSet<String>,
}

impl Shared {
    fn new(
        store_id: u64,
        root_dir: PathBuf,
        is_persistent: bool,
        has_account_key: bool,
        encryption_key: StorageEncryptionKey,
    ) -> Self {
        Self {
            store_id,
            root_dir,
            is_persistent,
            has_account_key,
            encryption_key,
            account_dirs: Vec::new(),
            in_flight: HashMap::new(),
            failed_at: HashMap::new(),
            lottie_json: ByteLru::new(LOTTIE_JSON_BUDGET_BYTES),
            hot: ByteLru::new(HOT_BLOB_BUDGET_BYTES),
            writing: HashMap::new(),
            misses: HashSet::new(),
            reading: HashSet::new(),
            cancelled: HashSet::new(),
        }
    }

    /// Removes the account key from every folder it was registered for (logout, or the cache is
    /// being reopened with another key).
    fn forget_account_dirs(&mut self) {
        for dir in self.account_dirs.drain(..) {
            unregister_cache_key(&dir);
        }
    }

    fn memory_hit(&mut self, key: &str) -> Option<Arc<[u8]>> {
        self.hot
            .get(key)
            .or_else(|| self.writing.get(key).map(Arc::clone))
    }

    fn remember(&mut self, key: &str, bytes: &Arc<[u8]>) {
        self.hot
            .insert(key.to_owned(), Arc::clone(bytes), bytes.len());
    }

    fn note_miss(&mut self, key: &str) {
        if self.misses.len() >= KNOWN_MISSES_CAP {
            self.misses.clear();
        }
        self.misses.insert(key.to_owned());
    }

    fn note_stored(&mut self, key: &str) {
        self.failed_at.remove(key);
        self.misses.remove(key);
        self.cancelled.remove(key);
    }

    fn cached_lottie(&mut self, doc_id: u64) -> Option<Arc<str>> {
        self.lottie_json.get(&doc_id)
    }

    fn remember_lottie(&mut self, doc_id: u64, json: Arc<str>) {
        let size = json.len();
        self.lottie_json.insert(doc_id, json, size);
    }
}

/// Thread-safe live media disk cache & async download coordinator backed by `tg-storage`'s
/// encrypted [`MediaCacheIndex`].
#[derive(Clone)]
pub struct LiveMediaCache {
    shared: Arc<Mutex<Shared>>,
    actor: Arc<StoreActor>,
    counters: Arc<MediaCacheCounters>,
}

fn new_store(index: MediaCacheIndex, root_dir: PathBuf, is_persistent: bool) -> (u64, Store) {
    let id = NEXT_STORE_ID.fetch_add(1, Ordering::Relaxed);
    let mut store = Store::from_index(index, root_dir, is_persistent);
    store.id = id;
    (id, store)
}

impl LiveMediaCache {
    fn from_index(
        index: MediaCacheIndex,
        root_dir: PathBuf,
        is_persistent: bool,
        has_account_key: bool,
    ) -> Self {
        let key = index.encryption_key().clone();
        let (id, store) = new_store(index, root_dir.clone(), is_persistent);
        Self {
            shared: Arc::new(Mutex::new(Shared::new(
                id,
                root_dir,
                is_persistent,
                has_account_key,
                key,
            ))),
            actor: Arc::new(StoreActor::spawn(store)),
            counters: Arc::new(MediaCacheCounters::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Work counters (disk reads, memory hits, blocking reads) of this cache.
    #[must_use]
    pub fn counters(&self) -> &MediaCacheCounters {
        &self.counters
    }

    /// Opens an encrypted persistent disk-backed [`MediaCacheIndex`] at `root_dir` using `key`.
    pub fn open_with_key(
        root_dir: impl Into<PathBuf>,
        key: StorageEncryptionKey,
    ) -> anyhow::Result<Self> {
        let dir = root_dir.into();
        let index = MediaCacheIndex::open_with_key(&dir, key)
            .map_err(|e| anyhow!("failed to open MediaCacheIndex at {}: {e}", dir.display()))?;
        Ok(Self::from_index(index, dir, true, true))
    }

    /// Opens an encrypted persistent disk-backed [`MediaCacheIndex`] at `root_dir`
    /// (`index.sqlite3` + encrypted `blobs/`).
    pub fn open(root_dir: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let dir = root_dir.into();
        let index = MediaCacheIndex::open(&dir)
            .map_err(|e| anyhow!("failed to open MediaCacheIndex at {}: {e}", dir.display()))?;
        Ok(Self::from_index(index, dir, true, false))
    }

    /// Opens an in-memory SQLite [`MediaCacheIndex`] storing encrypted blobs under `root_dir`.
    pub fn open_in_memory(root_dir: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let dir = root_dir.into();
        let index = MediaCacheIndex::open_in_memory(&dir)
            .map_err(|e| anyhow!("failed to open in-memory MediaCacheIndex: {e}"))?;
        Ok(Self::from_index(index, dir, false, false))
    }

    /// Opens a strictly memory-only cache with zero disk or `%TEMP%` writes until an account
    /// `tdata` directory is attached via [`Self::reopen_at_with_key`] or [`Self::reopen_at`].
    #[must_use]
    pub fn open_memory_only() -> Self {
        let index = MediaCacheIndex::open_memory_only()
            .unwrap_or_else(|e| panic!("unable to initialize memory-only MediaCacheIndex: {e}"));
        Self::from_index(index, PathBuf::new(), false, false)
    }

    /// Opens a default memory-only cache for `session_id` before an account directory is attached.
    ///
    /// Security invariant (D1): never touches `%TEMP%` before an account directory and encryption
    /// key are known.
    #[must_use]
    pub fn open_default_for_session(_session_id: u64) -> Self {
        Self::open_memory_only()
    }

    /// Swaps the actor's store for one over `index` and resets the in-memory layer, so nothing
    /// read from or downloaded for the previous store is served afterwards.
    fn replace_store(
        &self,
        index: MediaCacheIndex,
        root_dir: PathBuf,
        is_persistent: bool,
        has_account_key: bool,
    ) -> anyhow::Result<()> {
        let key = index.encryption_key().clone();
        let (id, store) = new_store(index, root_dir.clone(), is_persistent);
        // The swap runs with read priority, so reads queued after it see the new store.
        self.actor
            .call(Priority::Read, move |current| {
                current.flush_touches();
                *current = store;
            })
            .ok_or_else(|| anyhow!("media cache thread stopped"))?;
        let mut guard = self.lock();
        guard.forget_account_dirs();
        *guard = Shared::new(id, root_dir, is_persistent, has_account_key, key);
        Ok(())
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
        self.replace_store(index, dir, true, true)?;
        // An account folder is attached: media no longer goes to the system temp directory, so
        // whatever older builds left there can be removed (once per process, in the background).
        sweep_legacy_temp_dirs_once();
        Ok(())
    }

    /// Reopens this shared cache handle against a new persistent `root_dir` (e.g. account `tdata`
    /// media cache folder or a test `TempDir`), affecting all clones of [`super::LiveSession`].
    pub fn reopen_at(&self, root_dir: impl Into<PathBuf>) -> anyhow::Result<()> {
        let dir = root_dir.into();
        let index = MediaCacheIndex::open(&dir)
            .map_err(|e| anyhow!("failed to open MediaCacheIndex at {}: {e}", dir.display()))?;
        self.replace_store(index, dir, true, false)?;
        sweep_legacy_temp_dirs_once();
        Ok(())
    }

    /// Closes any on-disk SQLite and blob handles and switches this handle back to a fresh
    /// memory-only cache (called before deleting the account cache directory on logout so
    /// Windows does not hold open SQLite/WAL file handles).
    pub fn close_to_memory(&self) {
        let (old_root, was_persistent) = {
            let guard = self.lock();
            (guard.root_dir.clone(), guard.is_persistent)
        };
        if was_persistent && !old_root.as_os_str().is_empty() {
            unregister_cache_key(&old_root);
        }
        match MediaCacheIndex::open_memory_only() {
            Ok(index) => {
                if let Err(err) = self.replace_store(index, PathBuf::new(), false, false) {
                    warn!(error = %err, "unable to close the media cache to memory");
                }
            }
            Err(err) => {
                warn!(error = %err, "unable to open a memory-only media cache");
                self.lock().forget_account_dirs();
            }
        }
    }

    /// Returns `true` when this cache is attached to a persistent encrypted disk directory.
    #[must_use]
    pub fn is_persistent(&self) -> bool {
        self.lock().is_persistent
    }

    /// Returns the active cache root directory path (empty when in memory-only mode).
    #[must_use]
    pub fn root_dir(&self) -> PathBuf {
        self.lock().root_dir.clone()
    }

    /// Gives `dir`, a folder outside the cache root that holds other data of this account at rest
    /// (the Mini App bot storage), the account's storage key: writers there find it with
    /// `tg_storage::cache::media_cache::lookup_cache_key_for_path` and encrypt with it.
    ///
    /// Returns `false` and registers nothing while this cache has no account key (memory-only,
    /// or opened with a random per-process key): data encrypted with such a key could not be
    /// read back after a restart, so those writers keep their data in memory instead.
    pub fn register_account_dir(&self, dir: &Path) -> bool {
        let mut guard = self.lock();
        if !guard.has_account_key {
            return false;
        }
        register_cache_key(dir, guard.encryption_key.clone());
        if !guard.account_dirs.iter().any(|known| known == dir) {
            guard.account_dirs.push(dir.to_path_buf());
        }
        true
    }

    /// Returns the directory for streamed media files (`<cache root>/streamed`) once the cache is
    /// attached to an account folder. Files there are encrypted with the cache key. `None` before
    /// an account folder exists: streamed media must then use an ephemeral key instead.
    #[must_use]
    pub fn streamed_dir(&self) -> Option<PathBuf> {
        let guard = self.lock();
        (guard.is_persistent && !guard.root_dir.as_os_str().is_empty())
            .then(|| guard.root_dir.join("streamed"))
    }

    /// Reads `key` on the cache thread and waits for it, keeping the result in memory.
    /// `None` when the disk cache does not have it.
    fn read_blocking(&self, key: &str) -> Option<Arc<[u8]>> {
        if let Some(bytes) = self.lock().memory_hit(key) {
            self.counters.memory_hits.fetch_add(1, Ordering::Relaxed);
            return Some(bytes);
        }
        if self.lock().misses.contains(key) {
            return None;
        }
        self.counters.blocking_reads.fetch_add(1, Ordering::Relaxed);
        let owned = key.to_owned();
        let counters = Arc::clone(&self.counters);
        let (store_id, data) = self.actor.call(Priority::Read, move |store| {
            counters.disk_reads.fetch_add(1, Ordering::Relaxed);
            (store.id, store.read(&owned))
        })?;
        let mut guard = self.lock();
        if guard.store_id != store_id {
            return None;
        }
        match data {
            Some(bytes) => {
                let bytes: Arc<[u8]> = bytes.into();
                if bytes.len() <= HOT_BLOB_MAX_ENTRY_BYTES {
                    guard.remember(key, &bytes);
                }
                Some(bytes)
            }
            None => {
                guard.note_miss(key);
                None
            }
        }
    }

    /// Reads cached bytes for `key`, updating its LRU `last_accessed` timestamp.
    #[must_use]
    pub fn get_blob(&self, key: &str) -> Option<Vec<u8>> {
        self.read_blocking(key).map(|bytes| bytes.to_vec())
    }

    /// Stores `bytes` under `key` with category `tag`, enforcing configured cache limits.
    pub fn put_blob(&self, key: &str, tag: MediaCacheTag, bytes: &[u8]) -> anyhow::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let owned_key = key.to_owned();
        let data: Arc<[u8]> = Arc::from(bytes);
        let for_store = Arc::clone(&data);
        let result = self
            .actor
            .call(Priority::Write, move |store| {
                store.write(&owned_key, tag, &for_store)
            })
            .ok_or_else(|| anyhow!("media cache thread stopped"))?;
        if result.is_ok() {
            let mut guard = self.lock();
            guard.note_stored(key);
            if data.len() <= HOT_BLOB_MAX_ENTRY_BYTES {
                guard.remember(key, &data);
            }
        }
        result
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
                self.lock()
                    .remember_lottie(doc_id, Arc::from(json.as_str()));
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
        if let Some(json) = self.lock().cached_lottie(doc_id) {
            return Some(json.as_ref().to_owned());
        }
        let key = document_cache_key(doc.dc_id, doc_id);
        let bytes = self.read_blocking(&key)?;
        let json = decompress_tgs_to_lottie_json(&bytes).ok()?;
        self.lock()
            .remember_lottie(doc_id, Arc::from(json.as_str()));
        Some(json)
    }

    /// Makes sure the Lottie JSON of `doc_id` is in memory once its `.tgs` bytes are.
    fn ensure_lottie(&self, tgs_doc_id: Option<u64>, bytes: &[u8]) {
        let Some(doc_id) = tgs_doc_id else {
            return;
        };
        if self.lock().lottie_json.contains(&doc_id) {
            return;
        }
        if let Ok(json) = decompress_tgs_to_lottie_json(bytes) {
            self.lock().remember_lottie(doc_id, Arc::from(json));
        }
    }

    /// Returns whether `key` currently has an active background download in progress.
    #[must_use]
    pub fn is_in_flight(&self, key: &str) -> bool {
        self.lock().in_flight.contains_key(key)
    }

    /// Returns real [`LocalStorageStats`] computed from [`MediaCacheIndex`].
    #[must_use]
    pub fn stats(&self) -> LocalStorageStats {
        self.actor
            .call(Priority::Write, |store| {
                let tag_stats = store.index.stats_by_tag().unwrap_or_default();
                LocalStorageStats {
                    photos_bytes: i64::try_from(tag_stats.photos_bytes).unwrap_or(i64::MAX),
                    videos_bytes: i64::try_from(tag_stats.videos_bytes).unwrap_or(i64::MAX),
                    files_bytes: i64::try_from(tag_stats.files_bytes).unwrap_or(i64::MAX),
                    stickers_bytes: i64::try_from(tag_stats.stickers_bytes).unwrap_or(i64::MAX),
                    size_limit_bytes: store.size_limit_bytes,
                    time_limit_days: store.time_limit_days,
                }
            })
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
        let freed = self
            .actor
            .call(Priority::Write, |store| {
                let tag_stats = store.index.stats_by_tag().unwrap_or_default();
                let freed = tag_stats
                    .photos_bytes
                    .saturating_add(tag_stats.videos_bytes)
                    .saturating_add(tag_stats.files_bytes)
                    .saturating_add(tag_stats.stickers_bytes);
                store.clear_all();
                i64::try_from(freed).unwrap_or(i64::MAX)
            })
            .unwrap_or(0);
        let mut guard = self.lock();
        guard.hot.clear();
        guard.writing.clear();
        guard.misses.clear();
        guard.lottie_json.clear();
        guard.failed_at.clear();
        freed
    }

    /// Updates local storage size (`0` = unlimited) and retention time (`0` = forever) limits
    /// and immediately prunes [`MediaCacheIndex`] according to tdesktop's `updateSettings` behavior.
    pub fn set_limits(&self, size_limit_bytes: i64, time_limit_days: i32) {
        let _ = self.actor.call(Priority::Write, move |store| {
            store.size_limit_bytes = size_limit_bytes.max(0);
            store.time_limit_days = time_limit_days.max(0);
            store.apply_limits_prune();
            store.save_manifest();
        });
    }

    /// Triggers an immediate LRU/age prune using the active limits, returning evicted entry count.
    pub fn prune_now(&self) -> usize {
        self.actor
            .call(Priority::Write, Store::apply_limits_prune)
            .unwrap_or(0)
    }

    /// Returns cached bytes for `key`, or spawns an asynchronous `upload.getFile` (with CDN
    /// redirect support) task on `handle` if not yet cached or in-flight.
    ///
    /// A disk-cache hit is read on the cache thread while the caller waits (never behind queued
    /// writes); this never returns [`CacheFetchState::Pending`].
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
        self.fetch(
            key,
            tag,
            resolved,
            tgs_doc_id,
            rpc,
            handle,
            bridge,
            ReadMode::Blocking,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn fetch<C: RpcClient + Clone + 'static>(
        &self,
        key: String,
        tag: MediaCacheTag,
        resolved: ResolvedMediaLocation,
        tgs_doc_id: Option<u64>,
        rpc: &C,
        handle: &Handle,
        bridge: &Arc<LiveBridgeState>,
        mode: ReadMode,
        refresh: Option<&RefreshOrigin>,
    ) -> CacheFetchState {
        if resolved.size > MAX_INLINE_AUTO_DOWNLOAD_BYTES {
            return CacheFetchState::Skipped;
        }

        // Fast path: memory, in-flight map, failure cooldown and known misses under one short lock.
        let store_id = {
            let mut guard = self.lock();
            if let Some(bytes) = guard.memory_hit(&key) {
                drop(guard);
                self.counters.memory_hits.fetch_add(1, Ordering::Relaxed);
                self.ensure_lottie(tgs_doc_id, &bytes);
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
            if guard.misses.contains(&key) {
                return self.begin_download(
                    guard, key, tag, resolved, tgs_doc_id, rpc, handle, bridge, refresh,
                );
            }
            if mode == ReadMode::NonBlocking && guard.reading.contains(&key) {
                return CacheFetchState::Pending;
            }
            if mode == ReadMode::NonBlocking {
                guard.reading.insert(key.clone());
            }
            guard.store_id
        };

        if mode == ReadMode::NonBlocking {
            let cache = self.clone();
            let rpc = rpc.clone();
            let handle = handle.clone();
            let bridge = Arc::clone(bridge);
            let counters = Arc::clone(&self.counters);
            let refresh = refresh.cloned();
            let posted = self.actor.post(Priority::Read, move |store| {
                if store.id != store_id {
                    return;
                }
                counters.disk_reads.fetch_add(1, Ordering::Relaxed);
                let data = store.read(&key);
                cache.finish_read(
                    store_id,
                    key,
                    data,
                    tag,
                    resolved,
                    tgs_doc_id,
                    &rpc,
                    &handle,
                    &bridge,
                    refresh.as_ref(),
                );
            });
            if !posted {
                return CacheFetchState::Skipped;
            }
            return CacheFetchState::Pending;
        }

        self.counters.blocking_reads.fetch_add(1, Ordering::Relaxed);
        let read_key = key.clone();
        let counters = Arc::clone(&self.counters);
        let Some((read_store, data)) = self.actor.call(Priority::Read, move |store| {
            counters.disk_reads.fetch_add(1, Ordering::Relaxed);
            (store.id, store.read(&read_key))
        }) else {
            return CacheFetchState::Skipped;
        };
        let mut guard = self.lock();
        if guard.store_id != store_id || read_store != store_id {
            return CacheFetchState::Skipped;
        }
        match data {
            Some(bytes) => {
                let bytes: Arc<[u8]> = bytes.into();
                if bytes.len() <= HOT_BLOB_MAX_ENTRY_BYTES {
                    guard.remember(&key, &bytes);
                }
                drop(guard);
                self.ensure_lottie(tgs_doc_id, &bytes);
                CacheFetchState::Ready(bytes)
            }
            None => {
                guard.note_miss(&key);
                if let Some(flight) = guard.in_flight.get(&key) {
                    return CacheFetchState::Loading {
                        bytes_loaded: flight.bytes_loaded,
                        total_bytes: flight.total_bytes,
                    };
                }
                self.begin_download(
                    guard, key, tag, resolved, tgs_doc_id, rpc, handle, bridge, refresh,
                )
            }
        }
    }

    /// Completes a non-blocking read on the cache thread: a hit goes to memory, a miss starts
    /// the download; either way the UI is told to look again.
    #[allow(clippy::too_many_arguments)]
    fn finish_read<C: RpcClient + Clone + 'static>(
        &self,
        store_id: u64,
        key: String,
        data: Option<Vec<u8>>,
        tag: MediaCacheTag,
        resolved: ResolvedMediaLocation,
        tgs_doc_id: Option<u64>,
        rpc: &C,
        handle: &Handle,
        bridge: &Arc<LiveBridgeState>,
        refresh: Option<&RefreshOrigin>,
    ) {
        let mut guard = self.lock();
        if guard.store_id != store_id {
            return;
        }
        guard.reading.remove(&key);
        match data {
            Some(bytes) => {
                let bytes: Arc<[u8]> = bytes.into();
                // Kept whatever its size: the waiting lookup takes it from memory.
                guard.remember(&key, &bytes);
                drop(guard);
                self.ensure_lottie(tgs_doc_id, &bytes);
            }
            None => {
                guard.note_miss(&key);
                if !guard.in_flight.contains_key(&key) {
                    let _ = self.begin_download(
                        guard, key, tag, resolved, tgs_doc_id, rpc, handle, bridge, refresh,
                    );
                }
            }
        }
        bridge.notify_ui();
    }

    /// Registers `key` as in flight (under the caller's lock) and spawns its download.
    #[allow(clippy::too_many_arguments)]
    fn begin_download<C: RpcClient + Clone + 'static>(
        &self,
        mut guard: MutexGuard<'_, Shared>,
        key: String,
        tag: MediaCacheTag,
        resolved: ResolvedMediaLocation,
        tgs_doc_id: Option<u64>,
        rpc: &C,
        handle: &Handle,
        bridge: &Arc<LiveBridgeState>,
        refresh: Option<&RefreshOrigin>,
    ) -> CacheFetchState {
        let store_id = guard.store_id;
        let token = NEXT_FLIGHT_TOKEN.fetch_add(1, Ordering::Relaxed);
        guard.in_flight.insert(
            key.clone(),
            InFlightDownload {
                bytes_loaded: 0,
                total_bytes: resolved.size,
                abort: None,
                token,
            },
        );
        drop(guard);

        let cache = self.clone();
        let rpc = rpc.clone();
        let bridge = Arc::clone(bridge);
        let key_task = key.clone();
        let expected_size = (resolved.size > 0).then_some(resolved.size);
        let total_hint = resolved.size;
        let started = Instant::now();
        let refresh = refresh.cloned();
        debug!(key = %key, dc = resolved.dc_id, size = total_hint, "inline media download started");

        let task = handle.spawn(async move {
            let progress_cache = cache.clone();
            let progress_key = key_task.clone();
            let progress_bridge = Arc::clone(&bridge);

            let res = refresh::download(
                &rpc,
                refresh,
                resolved.dc_id,
                resolved.location,
                expected_size,
                move |downloaded, total| {
                    if let Some(slot) = progress_cache
                        .lock()
                        .in_flight
                        .get_mut(&progress_key)
                        .filter(|slot| slot.token == token)
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

            // An aborted task stops at its next await; one that got its last answer in the
            // same poll still arrives here and must not store what the user cancelled.
            if !cache
                .lock()
                .in_flight
                .get(&key_task)
                .is_some_and(|slot| slot.token == token)
            {
                return;
            }
            match res {
                Ok(bytes) => {
                    debug!(
                        key = %key_task,
                        bytes = bytes.len(),
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "inline media downloaded"
                    );
                    cache.finish_download(store_id, key_task, tag, tgs_doc_id, bytes);
                    bridge.notify_ui();
                }
                Err(err) => {
                    debug!(
                        key = %key_task,
                        error = %err,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "background inline media download failed"
                    );
                    {
                        let mut guard = cache.lock();
                        guard.in_flight.remove(&key_task);
                        guard.failed_at.insert(key_task, Instant::now());
                    }
                    bridge.notify_ui();
                }
            }
        });
        // A task that already finished has removed its slot; there is nothing to abort then.
        if let Some(slot) = self
            .lock()
            .in_flight
            .get_mut(&key)
            .filter(|slot| slot.token == token)
        {
            slot.abort = Some(task.abort_handle());
        }

        CacheFetchState::Loading {
            bytes_loaded: 0,
            total_bytes: total_hint,
        }
    }

    /// Stops the download of `key` and marks it cancelled: it is not downloaded automatically
    /// again until [`Self::resume_download`] (`PhotoData::cancel`). Returns whether a download
    /// was running.
    pub fn cancel_download(&self, key: &str) -> bool {
        let mut guard = self.lock();
        let Some(flight) = guard.in_flight.remove(key) else {
            return false;
        };
        if guard.cancelled.len() >= KNOWN_MISSES_CAP {
            guard.cancelled.clear();
        }
        guard.cancelled.insert(key.to_owned());
        drop(guard);
        if let Some(abort) = flight.abort {
            abort.abort();
        }
        debug!(key = %key, "inline media download cancelled");
        true
    }

    /// Whether the download of `key` was cancelled and not started again.
    #[must_use]
    pub fn is_cancelled(&self, key: &str) -> bool {
        self.lock().cancelled.contains(key)
    }

    /// Lets a cancelled `key` download again.
    pub fn resume_download(&self, key: &str) {
        self.lock().cancelled.remove(key);
    }

    /// State of a photo that is not downloaded automatically (auto-download is off for the
    /// chat, or the user cancelled it): cached, downloading because the user asked, or not
    /// loaded.
    fn manual_photo_state(&self, key: &str) -> PhotoLoadState {
        if let Some(bytes) = self.read_blocking(key) {
            return PhotoLoadState::Ready { bytes };
        }
        match self.lock().in_flight.get(key) {
            Some(flight) => PhotoLoadState::Loading {
                progress: (flight.total_bytes > 0).then(|| {
                    (flight.bytes_loaded as f32 / flight.total_bytes as f32).clamp(0.0, 1.0)
                }),
                bytes_loaded: flight.bytes_loaded,
                total_bytes: flight.total_bytes,
            },
            None => PhotoLoadState::NotLoaded,
        }
    }

    /// The file a click on `media` downloads or cancels: the photo, or a voice message's
    /// audio (`Photo::_savel` / `_cancell`, `DocumentOpenClickHandler` for voice).
    pub(crate) fn download_target(
        media: &MessageMedia,
    ) -> Option<(String, MediaCacheTag, ResolvedMediaLocation)> {
        let photo = |photo: &PhotoData| {
            let resolved = photo_to_location(photo);
            let thumb_size = match &resolved.location {
                tg_tl::enums::InputFileLocation::InputPhotoFileLocation(loc) => {
                    loc.thumb_size.as_str()
                }
                _ => "y",
            };
            let key = photo_cache_key(resolved.dc_id, photo.id.0, thumb_size);
            (key, MediaCacheTag::Photo, resolved)
        };
        match media {
            MessageMedia::Photo(p) => Some(photo(p)),
            MessageMedia::Document(doc)
                if matches!(doc.kind, DocumentKind::Voice | DocumentKind::RoundVideo) =>
            {
                let resolved = document_to_location(doc);
                let key = document_cache_key(resolved.dc_id, doc.id.0);
                Some((key, MediaCacheTag::Voice, resolved))
            }
            MessageMedia::WebPage(wp) => wp.photo.as_ref().map(photo),
            MessageMedia::Game(game) if game.document.is_none() => game.photo.as_ref().map(photo),
            MessageMedia::PaidMedia(paid) => {
                paid.extended_media.iter().find_map(|item| match item {
                    ExtendedMedia::Media(inner) => Self::download_target(inner),
                    ExtendedMedia::Preview { .. } => None,
                })
            }
            MessageMedia::Invoice(inv) => match inv.extended_media.as_deref() {
                Some(ExtendedMedia::Media(inner)) => Self::download_target(inner),
                _ => None,
            },
            _ => None,
        }
    }

    /// Cancels the running download of `msg`'s photo or voice file. Returns whether one ran.
    pub fn cancel_media_download_for_message(&self, msg: &Message) -> bool {
        let MessageContent::Regular { media, .. } = &msg.content else {
            return false;
        };
        Self::download_target(media).is_some_and(|(key, _, _)| self.cancel_download(&key))
    }

    /// Starts (or restarts after a cancel) the download of `msg`'s photo or voice file without
    /// blocking; a file already cached or downloading is left alone.
    pub fn start_media_download_for_message<C: RpcClient + Clone + 'static>(
        &self,
        msg: &Message,
        rpc: &C,
        handle: &Handle,
        bridge: &Arc<LiveBridgeState>,
    ) {
        let MessageContent::Regular { media, .. } = &msg.content else {
            return;
        };
        if let Some((key, tag, resolved)) = Self::download_target(media) {
            self.resume_download(&key);
            let _ = self.fetch_nonblocking(key, tag, resolved, rpc, handle, bridge);
        }
    }

    /// Makes a finished download available at once (memory) and queues its encrypted disk
    /// write on the cache thread; the downloading worker never waits for the disk.
    fn finish_download(
        &self,
        store_id: u64,
        key: String,
        tag: MediaCacheTag,
        tgs_doc_id: Option<u64>,
        bytes: Vec<u8>,
    ) {
        let bytes: Arc<[u8]> = bytes.into();
        let lottie = tgs_doc_id.and_then(|doc_id| match decompress_tgs_to_lottie_json(&bytes) {
            Ok(json) => Some((doc_id, Arc::<str>::from(json))),
            Err(err) => {
                debug!(doc_id, error = %err, "unable to decompress .tgs sticker");
                None
            }
        });
        let mut guard = self.lock();
        guard.in_flight.remove(&key);
        if guard.store_id != store_id {
            // The cache was reopened for another account or closed on logout meanwhile.
            return;
        }
        guard.note_stored(&key);
        guard.remember(&key, &bytes);
        guard.writing.insert(key.clone(), Arc::clone(&bytes));
        if let Some((doc_id, json)) = lottie {
            guard.remember_lottie(doc_id, json);
        }
        drop(guard);

        let shared = Arc::clone(&self.shared);
        self.actor.post(Priority::Write, move |store| {
            if store.id == store_id
                && let Err(err) = store.write(&key, tag, &bytes)
            {
                warn!(key = %key, error = %err, "failed to store downloaded media blob");
            }
            let mut guard = shared.lock().unwrap_or_else(PoisonError::into_inner);
            if guard
                .writing
                .get(&key)
                .is_some_and(|queued| Arc::ptr_eq(queued, &bytes))
            {
                guard.writing.remove(&key);
            }
        });
    }

    /// Like [`Self::fetch_or_spawn`] for the chat-history paint path: a disk-cache hit is read
    /// in the background and reported as [`CacheFetchState::Pending`] meanwhile.
    #[allow(clippy::too_many_arguments)]
    fn fetch_nonblocking<C: RpcClient + Clone + 'static>(
        &self,
        key: String,
        tag: MediaCacheTag,
        resolved: ResolvedMediaLocation,
        rpc: &C,
        handle: &Handle,
        bridge: &Arc<LiveBridgeState>,
    ) -> CacheFetchState {
        self.fetch_nonblocking_from(key, tag, resolved, None, rpc, handle, bridge)
    }

    /// [`Self::fetch_nonblocking`] whose download fetches an expired `file_reference` again
    /// from `refresh` (`Data::FileOrigin`, `FileLoader` on `FILE_REFERENCE_*` errors).
    #[allow(clippy::too_many_arguments)]
    fn fetch_nonblocking_from<C: RpcClient + Clone + 'static>(
        &self,
        key: String,
        tag: MediaCacheTag,
        resolved: ResolvedMediaLocation,
        refresh: Option<&RefreshOrigin>,
        rpc: &C,
        handle: &Handle,
        bridge: &Arc<LiveBridgeState>,
    ) -> CacheFetchState {
        self.fetch(
            key,
            tag,
            resolved,
            None,
            rpc,
            handle,
            bridge,
            ReadMode::NonBlocking,
            refresh,
        )
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
        self.inline_media_descriptor_for_message_with(msg, true, rpc, handle, bridge)
    }

    /// Like [`Self::inline_media_descriptor_for_message`]; with `photo_autoload == false`
    /// (auto-download of photos is off for this chat, `Data::AutoDownload::Should`) a photo is
    /// only served when it is already cached, and never downloaded.
    pub fn inline_media_descriptor_for_message_with<C: RpcClient + Clone + 'static>(
        &self,
        msg: &Message,
        photo_autoload: bool,
        rpc: &C,
        handle: &Handle,
        bridge: &Arc<LiveBridgeState>,
    ) -> Option<InlineMediaDescriptor> {
        self.inline_media_descriptor_refreshing(msg, photo_autoload, None, rpc, handle, bridge)
    }

    /// [`Self::inline_media_descriptor_for_message_with`] whose sticker download asks the server
    /// for `msg` again when its `file_reference` expired (`session` is the live session).
    pub fn inline_media_descriptor_refreshing<C: RpcClient + Clone + 'static>(
        &self,
        msg: &Message,
        photo_autoload: bool,
        session: Option<&Arc<tokio::sync::RwLock<tg_data::Session>>>,
        rpc: &C,
        handle: &Handle,
        bridge: &Arc<LiveBridgeState>,
    ) -> Option<InlineMediaDescriptor> {
        let MessageContent::Regular { media, .. } = &msg.content else {
            return None;
        };
        let refresh = session.map(|session| RefreshOrigin::message(session, msg));
        self.descriptor_for_media(media, photo_autoload, refresh.as_ref(), rpc, handle, bridge)
    }

    fn descriptor_for_media<C: RpcClient + Clone + 'static>(
        &self,
        media: &MessageMedia,
        photo_autoload: bool,
        refresh: Option<&RefreshOrigin>,
        rpc: &C,
        handle: &Handle,
        bridge: &Arc<LiveBridgeState>,
    ) -> Option<InlineMediaDescriptor> {
        match media {
            MessageMedia::Photo(photo) => {
                Some(self.descriptor_for_photo(photo, photo_autoload, rpc, handle, bridge))
            }
            MessageMedia::Document(doc) => {
                self.descriptor_for_document(doc, photo_autoload, refresh, rpc, handle, bridge)
            }
            MessageMedia::WebPage(wp) => {
                if let Some(ref photo) = wp.photo {
                    Some(self.descriptor_for_photo(photo, photo_autoload, rpc, handle, bridge))
                } else if let Some(ref doc) = wp.document {
                    self.descriptor_for_document(doc, photo_autoload, None, rpc, handle, bridge)
                } else {
                    None
                }
            }
            MessageMedia::Game(game) => {
                if let Some(ref doc) = game.document {
                    self.descriptor_for_document(doc, photo_autoload, None, rpc, handle, bridge)
                } else {
                    game.photo.as_ref().map(|photo| {
                        self.descriptor_for_photo(photo, photo_autoload, rpc, handle, bridge)
                    })
                }
            }
            MessageMedia::PaidMedia(paid) => {
                for item in &paid.extended_media {
                    if let ExtendedMedia::Media(inner) = item {
                        return self.descriptor_for_media(
                            inner,
                            photo_autoload,
                            None,
                            rpc,
                            handle,
                            bridge,
                        );
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
                    self.descriptor_for_media(inner, photo_autoload, None, rpc, handle, bridge)
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
        photo_autoload: bool,
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
        if !photo_autoload || self.is_cancelled(&key) {
            // `PhotoMedia::automaticLoad` loads neither; the user starts it with a click.
            return InlineMediaDescriptor {
                has_spoiler: photo.spoiler,
                photo_load: Some(self.manual_photo_state(&key)),
                ..InlineMediaDescriptor::default()
            };
        }
        let photo_load = photo_load_state(self.fetch_nonblocking(
            key,
            MediaCacheTag::Photo,
            resolved,
            rpc,
            handle,
            bridge,
        ));
        InlineMediaDescriptor {
            has_spoiler: photo.spoiler,
            photo_load,
            ..InlineMediaDescriptor::default()
        }
    }

    /// `autoload`: the message's media may download by itself (for a round video: its
    /// autoplay is on, [`crate::auto_download_gate::round_video_autoplay_allowed`]).
    fn descriptor_for_document<C: RpcClient + Clone + 'static>(
        &self,
        doc: &DocumentData,
        autoload: bool,
        refresh: Option<&RefreshOrigin>,
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
                        desc.video_data = Some(InlineDocumentData::Bytes(bytes.to_vec()));
                    }
                    CacheFetchState::Loading { .. }
                    | CacheFetchState::Pending
                    | CacheFetchState::Skipped => {
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
            DocumentKind::RoundVideo if !autoload => {
                // TGB-2997: a video message is not downloaded when it enters the list unless it
                // autoplays; only the cache is read, a click downloads it (`download_target`).
                let resolved = document_to_location(doc);
                let ext = resolved.ext_hint.clone().or_else(|| Some("mp4".to_owned()));
                let key = document_cache_key(resolved.dc_id, doc.id.0);
                match self.read_blocking(&key) {
                    Some(bytes) => {
                        desc.video_data = Some(InlineDocumentData::Bytes(bytes.to_vec()));
                        desc.audio_bytes = Some(bytes.to_vec());
                        desc.audio_ext = ext;
                    }
                    None => {
                        desc.voice_load = Some(if self.is_in_flight(&key) {
                            VoiceLoadState::Loading
                        } else {
                            VoiceLoadState::NotLoaded
                        });
                        self.populate_doc_thumb(doc, &mut desc, rpc, handle, bridge);
                    }
                }
                Some(desc)
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
                        desc.video_data = Some(InlineDocumentData::Bytes(bytes.to_vec()));
                        desc.audio_bytes = Some(bytes.to_vec());
                        desc.audio_ext = ext;
                    }
                    CacheFetchState::Loading { .. }
                    | CacheFetchState::Pending
                    | CacheFetchState::Skipped => {
                        self.populate_doc_thumb(doc, &mut desc, rpc, handle, bridge);
                    }
                }
                Some(desc)
            }
            DocumentKind::Voice => {
                let resolved = document_to_location(doc);
                let ext = resolved.ext_hint.clone().or_else(|| Some("ogg".to_owned()));
                let key = document_cache_key(resolved.dc_id, doc.id.0);
                // Never downloaded automatically (`Data::AutoDownload::Should` is false for a
                // voice message): only the cache is read here, the first play downloads it.
                match self.read_blocking(&key) {
                    Some(bytes) => {
                        desc.audio_bytes = Some(bytes.to_vec());
                        desc.audio_ext = ext;
                    }
                    None => {
                        desc.voice_load = Some(if self.is_in_flight(&key) {
                            VoiceLoadState::Loading
                        } else {
                            VoiceLoadState::NotLoaded
                        });
                    }
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
                let _ = self.fetch(
                    key,
                    MediaCacheTag::Sticker,
                    resolved,
                    Some(doc.id.0),
                    rpc,
                    handle,
                    bridge,
                    ReadMode::Blocking,
                    refresh,
                );
                Some(desc)
            }
            DocumentKind::VideoSticker => {
                desc.is_video_sticker = true;
                let resolved = document_to_location(doc);
                let key = document_cache_key(resolved.dc_id, doc.id.0);
                if let CacheFetchState::Ready(bytes) = self.fetch(
                    key,
                    MediaCacheTag::Sticker,
                    resolved,
                    None,
                    rpc,
                    handle,
                    bridge,
                    ReadMode::Blocking,
                    refresh,
                ) {
                    desc.video_data = Some(InlineDocumentData::Bytes(bytes.to_vec()));
                }
                Some(desc)
            }
            DocumentKind::Sticker => {
                let resolved = document_to_location(doc);
                let key = document_cache_key(resolved.dc_id, doc.id.0);
                desc.photo_load = photo_load_state(self.fetch_nonblocking_from(
                    key,
                    MediaCacheTag::Sticker,
                    resolved,
                    refresh,
                    rpc,
                    handle,
                    bridge,
                ));
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
        desc.photo_load = photo_load_state(self.fetch_nonblocking(
            key,
            MediaCacheTag::Photo,
            thumb_loc,
            rpc,
            handle,
            bridge,
        ));
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

    pub(crate) fn shared_media_thumb_for_photo<C: RpcClient + Clone + 'static>(
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
        match self.fetch_nonblocking(key, MediaCacheTag::Photo, resolved, rpc, handle, bridge) {
            CacheFetchState::Ready(bytes) => SharedMediaThumb::Ready {
                bytes: bytes.to_vec(),
            },
            CacheFetchState::Loading { .. }
            | CacheFetchState::Pending
            | CacheFetchState::Skipped => SharedMediaThumb::Loading {
                stripped_thumb: photo.stripped_thumb.clone(),
            },
        }
    }

    pub(crate) fn shared_media_thumb_for_document<C: RpcClient + Clone + 'static>(
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
        match self.fetch_nonblocking(key, MediaCacheTag::Photo, thumb_loc, rpc, handle, bridge) {
            CacheFetchState::Ready(bytes) => SharedMediaThumb::Ready {
                bytes: bytes.to_vec(),
            },
            CacheFetchState::Loading { .. }
            | CacheFetchState::Pending
            | CacheFetchState::Skipped => SharedMediaThumb::Loading {
                stripped_thumb: doc.stripped_thumb.clone(),
            },
        }
    }
}

/// Maps a cache lookup to the photo state the history paints: a pending disk read paints like
/// no state at all (blurred thumbnail) but is re-polled like a download.
fn photo_load_state(state: CacheFetchState) -> Option<PhotoLoadState> {
    match state {
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
        CacheFetchState::Pending => Some(PhotoLoadState::Pending),
        CacheFetchState::Skipped => None,
    }
}

/// Checks whether a cache directory path has an initialized `index.sqlite3`.
#[must_use]
pub fn has_persisted_index(dir: &Path) -> bool {
    dir.join("index.sqlite3").exists()
}

#[cfg(test)]
mod tests_actor;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
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

    #[test]
    fn sec3_b2_account_dir_gets_the_account_key_only_when_one_is_known() {
        use tg_storage::cache::media_cache::lookup_cache_key_for_path;

        let root = tempfile::TempDir::new().expect("tempdir");
        let bot_dir = root.path().join("bot_storage").join("user_7");
        let key = derive_media_cache_key(&AuthKey::new([0x4Du8; 256]), "data");

        // Memory-only cache: no account key yet.
        let cache = LiveMediaCache::open_memory_only();
        assert!(!cache.register_account_dir(&bot_dir));
        assert!(lookup_cache_key_for_path(&bot_dir).is_none());

        // Persistent cache with a random per-process key: still no account key.
        cache
            .reopen_at(root.path().join("cache_ephemeral"))
            .expect("reopen without key");
        assert!(!cache.register_account_dir(&bot_dir));
        assert!(lookup_cache_key_for_path(&bot_dir).is_none());

        // Attached with the account key: the folder gets exactly that key.
        cache
            .reopen_at_with_key(root.path().join("cache"), key.clone())
            .expect("attach with key");
        assert!(cache.register_account_dir(&bot_dir));
        assert_eq!(
            lookup_cache_key_for_path(&bot_dir)
                .as_ref()
                .map(|k| k.as_bytes()),
            Some(key.as_bytes())
        );

        // Logout (cache closed to memory) removes the key again.
        cache.close_to_memory();
        assert!(lookup_cache_key_for_path(&bot_dir).is_none());
    }
}
