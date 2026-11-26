//! The media cache's storage side: the encrypted [`MediaCacheIndex`] (SQLite + `TDEF` blob files)
//! owned by one actor thread, so no SQLite, disk or crypto work runs on the UI thread or on a
//! Tokio worker, and nobody holds a lock across it.
//!
//! Port origin: `Telegram/lib_storage/storage/cache/storage_cache_database.cpp` (the cache
//! database lives on its own `crl` queue; callers post work and get results back).

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tg_storage::cache::media_cache::{
    MediaCacheIndex, MediaCacheTag, read_encrypted_blob, write_encrypted_blob,
};
use tracing::warn;

use super::{
    DEFAULT_CACHE_SIZE_LIMIT_BYTES, DEFAULT_CACHE_TIME_LIMIT_DAYS, ENCRYPTED_MANIFEST_FILENAME,
    LEGACY_MANIFEST_FILENAME, SECONDS_PER_DAY,
};

/// Age-based pruning runs at most this often while blobs are being written (size-based pruning
/// still runs on every write that takes the cache over its size limit).
const AGE_PRUNE_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedLimits {
    size_limit_bytes: i64,
    time_limit_days: i32,
}

/// Everything that touches the disk. Only the actor thread has one.
pub(super) struct Store {
    /// Which opened index this is (see `NEXT_STORE_ID`); set by the owner after construction.
    pub(super) id: u64,
    pub(super) index: MediaCacheIndex,
    pub(super) root_dir: PathBuf,
    pub(super) is_persistent: bool,
    pub(super) size_limit_bytes: i64,
    pub(super) time_limit_days: i32,
    /// Upper bound of the bytes stored (exact after open and after each prune; writes add their
    /// size), so a write only scans the table when the cache may be over its size limit.
    estimated_total: u64,
    last_age_prune: Option<Instant>,
    /// Keys read since the last flush whose `last_accessed` still has to be written.
    touches: HashSet<String>,
}

impl Store {
    pub(super) fn from_index(
        index: MediaCacheIndex,
        root_dir: PathBuf,
        is_persistent: bool,
    ) -> Self {
        let estimated_total = index.stats().map(|s| s.total_bytes).unwrap_or(0);
        let mut store = Self {
            id: 0,
            index,
            root_dir,
            is_persistent,
            size_limit_bytes: DEFAULT_CACHE_SIZE_LIMIT_BYTES,
            time_limit_days: DEFAULT_CACHE_TIME_LIMIT_DAYS,
            estimated_total,
            last_age_prune: None,
            touches: HashSet::new(),
        };
        if is_persistent {
            // Purge any legacy plaintext sidecar manifest left by older builds.
            let _ = fs::remove_file(store.root_dir.join(LEGACY_MANIFEST_FILENAME));
            store.load_manifest();
        }
        store
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

    pub(super) fn save_manifest(&self) {
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

    /// Reads and decrypts `key`; the LRU access time is recorded later in one batch.
    pub(super) fn read(&mut self, key: &str) -> Option<Vec<u8>> {
        let bytes = self.index.get_blob_deferred_touch(key).ok().flatten()?;
        self.touches.insert(key.to_owned());
        Some(bytes)
    }

    /// Encrypts and stores `bytes`, pruning when the cache may be over a configured limit.
    pub(super) fn write(
        &mut self,
        key: &str,
        tag: MediaCacheTag,
        bytes: &[u8],
    ) -> anyhow::Result<()> {
        self.index
            .put_blob(key, tag, bytes)
            .map_err(|e| anyhow::anyhow!("MediaCacheIndex::put_blob failed: {e}"))?;
        // `put_blob` already set `last_accessed`.
        self.touches.remove(key);
        self.estimated_total = self.estimated_total.saturating_add(bytes.len() as u64);
        let over_size =
            self.size_limit_bytes > 0 && self.estimated_total > self.size_limit_bytes as u64;
        let age_due = self.time_limit_days > 0
            && self
                .last_age_prune
                .is_none_or(|at| at.elapsed() >= AGE_PRUNE_INTERVAL);
        if over_size || age_due {
            self.apply_limits_prune();
        }
        Ok(())
    }

    pub(super) fn flush_touches(&mut self) {
        if self.touches.is_empty() {
            return;
        }
        if let Err(err) = self
            .index
            .touch_keys(self.touches.iter().map(String::as_str))
        {
            warn!(error = %err, "media cache access-time update failed");
        }
        self.touches.clear();
    }

    pub(super) fn apply_limits_prune(&mut self) -> usize {
        self.flush_touches();
        let max_bytes = (self.size_limit_bytes > 0).then_some(self.size_limit_bytes as u64);
        let max_age = (self.time_limit_days > 0)
            .then_some(i64::from(self.time_limit_days).saturating_mul(SECONDS_PER_DAY));
        if max_bytes.is_none() && max_age.is_none() {
            return 0;
        }
        self.last_age_prune = Some(Instant::now());
        let evicted = match self.index.prune(max_bytes, max_age) {
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
        };
        self.estimated_total = self
            .index
            .stats()
            .map_or(self.estimated_total, |s| s.total_bytes);
        evicted
    }

    /// Evicts every blob (the "Clear cache" button).
    pub(super) fn clear_all(&mut self) {
        self.touches.clear();
        if let Err(err) = self.index.prune(Some(0), None) {
            warn!(error = %err, "MediaCacheIndex clear prune failed");
        }
        self.estimated_total = 0;
        self.save_manifest();
    }
}

/// Which queue a job goes to: reads run before queued writes, so a cache hit never waits behind
/// a burst of downloads being encrypted to disk.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Priority {
    Read,
    Write,
}

type Job = Box<dyn FnOnce(&mut Store) + Send>;

/// Handle to the actor thread that owns the [`Store`]. The thread ends (and closes SQLite) when
/// the last handle is dropped.
pub(super) struct StoreActor {
    jobs: Sender<(Priority, Job)>,
}

impl StoreActor {
    pub(super) fn spawn(store: Store) -> Self {
        let (jobs, rx) = mpsc::channel::<(Priority, Job)>();
        let spawned = std::thread::Builder::new()
            .name("tg-media-cache".to_owned())
            .spawn(move || run_actor(store, rx));
        if let Err(err) = spawned {
            warn!(error = %err, "unable to start the media cache thread");
        }
        Self { jobs }
    }

    /// Queues `job`; returns `false` when the actor is gone.
    pub(super) fn post(
        &self,
        priority: Priority,
        job: impl FnOnce(&mut Store) + Send + 'static,
    ) -> bool {
        self.jobs.send((priority, Box::new(job))).is_ok()
    }

    /// Runs `job` on the actor and waits for its result (`None` when the actor is gone).
    pub(super) fn call<R: Send + 'static>(
        &self,
        priority: Priority,
        job: impl FnOnce(&mut Store) -> R + Send + 'static,
    ) -> Option<R> {
        let (tx, rx) = mpsc::sync_channel(1);
        if !self.post(priority, move |store| {
            let _ = tx.send(job(store));
        }) {
            return None;
        }
        rx.recv().ok()
    }
}

fn run_actor(mut store: Store, rx: Receiver<(Priority, Job)>) {
    let mut reads: VecDeque<Job> = VecDeque::new();
    let mut writes: VecDeque<Job> = VecDeque::new();
    let enqueue = |(priority, job): (Priority, Job),
                   reads: &mut VecDeque<Job>,
                   writes: &mut VecDeque<Job>| {
        if priority == Priority::Read {
            reads.push_back(job);
        } else {
            writes.push_back(job);
        }
    };
    while let Ok(first) = rx.recv() {
        enqueue(first, &mut reads, &mut writes);
        loop {
            while let Ok(next) = rx.try_recv() {
                enqueue(next, &mut reads, &mut writes);
            }
            let Some(job) = reads.pop_front().or_else(|| writes.pop_front()) else {
                break;
            };
            if catch_unwind(AssertUnwindSafe(|| job(&mut store))).is_err() {
                warn!("media cache job panicked");
            }
        }
        // Idle: record the batched LRU access times.
        store.flush_touches();
    }
    store.flush_touches();
}
