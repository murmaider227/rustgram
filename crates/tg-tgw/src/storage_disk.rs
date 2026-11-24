//! Atomic disk-backed widget local storage with per-widget/global byte quotas and LRU eviction.
//!
//! Port origin: `Telegram/SourceFiles/tgw/tgw_storage_disk.h/.cpp` and `tgw_storage.h/.cpp`.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{Result, TgwError};
use crate::storage::{
    HeightRecord, LEGACY_MAX_STORED_BYTES, STORAGE_QUOTA_BYTES, StorageArea, StorageMeta,
    WidgetMsgKey, decode_storage_record, encode_storage_record, storage_area_bytes,
};

/// Default global byte quota across all persisted widgets (16 MiB).
pub const DEFAULT_TOTAL_DISK_QUOTA_BYTES: usize = 16 * 1024 * 1024;

/// Disk index magic header (`"TGWI"`).
const INDEX_MAGIC: [u8; 4] = *b"TGWI";

/// Disk record envelope magic header (`"TGWR"`).
const RECORD_MAGIC: [u8; 4] = *b"TGWR";

/// Current disk index format version (`3`: extends v2 with LRU access sequence & stored byte size).
pub const DISK_INDEX_VERSION: u32 = 3;

const INDEX_FILE_NAME: &str = "index.bin";
const RECORD_EXT: &str = ".rec";
const INDEX_ENTRY_BYTES_V3: usize = 8 + 8 + 8 + 4 + 4 + 8 + 8 + 8 + 8 + 4 + 8 + 4;

static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

/// Configuration for [`WidgetStorageDisk`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskStorageConfig {
    /// Owning account/session identifier; mismatches on reopen reject stale foreign indexes.
    pub owner: u64,
    /// Per-widget key+value UTF-8 byte quota (defaults to [`STORAGE_QUOTA_BYTES`] = 4 KiB).
    pub per_widget_quota_bytes: usize,
    /// Global byte quota across all persisted widgets (defaults to 16 MiB).
    pub total_quota_bytes: usize,
}

impl Default for DiskStorageConfig {
    fn default() -> Self {
        Self {
            owner: 0,
            per_widget_quota_bytes: STORAGE_QUOTA_BYTES,
            total_quota_bytes: DEFAULT_TOTAL_DISK_QUOTA_BYTES,
        }
    }
}

/// Outcome of a [`WidgetStorageDisk::write`] call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiskWriteOutcome {
    /// Whether the index metadata or file assignment changed.
    pub changed: bool,
    /// Keys evicted via LRU to stay within [`DiskStorageConfig::total_quota_bytes`].
    pub evicted: Vec<WidgetMsgKey>,
}

#[derive(Debug, Clone)]
struct DiskEntry {
    file_key: u64,
    meta: StorageMeta,
    height: Option<HeightRecord>,
    data_bytes: usize,
    last_access: u64,
}

/// Crash-safe disk-backed per-widget storage engine with per-widget/global quotas and LRU eviction
/// (`Tgw::StorageDisk`).
#[derive(Debug)]
pub struct WidgetStorageDisk {
    dir: PathBuf,
    config: DiskStorageConfig,
    entries: BTreeMap<WidgetMsgKey, DiskEntry>,
    next_file_key: u64,
    access_clock: u64,
    total_data_bytes: usize,
    index_dirty: bool,
}

impl WidgetStorageDisk {
    /// Opens or initializes a disk-backed widget storage directory.
    ///
    /// If an existing index is corrupt or belongs to a different owner, stale files are cleaned up
    /// and an empty store is initialized (matching `StorageDisk::StorageDisk` in
    /// `tgw_storage_disk.cpp:129-144`).
    pub fn open(dir: impl AsRef<Path>, config: DiskStorageConfig) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)
            .map_err(|e| TgwError::Storage(format!("create widget storage dir: {e}")))?;

        let mut store = Self {
            dir,
            config,
            entries: BTreeMap::new(),
            next_file_key: 1,
            access_clock: 0,
            total_data_bytes: 0,
            index_dirty: false,
        };

        let index_path = store.index_path();
        if index_path.exists() {
            match store.read_index() {
                Ok(()) => {
                    store.cleanup_orphans()?;
                }
                Err(err) => {
                    tracing::warn!(
                        "widget storage index corrupt or mismatched ({err}); resetting directory"
                    );
                    store.entries.clear();
                    store.total_data_bytes = 0;
                    store.remove_all_files()?;
                }
            }
        } else {
            store.cleanup_orphans()?;
        }

        // Enforce global quota in case the store was reopened with a tighter limit.
        if store.total_data_bytes > store.config.total_quota_bytes {
            let _ = store.evict_lru_excluding(None)?;
        }

        Ok(store)
    }

    /// Returns the root directory used by this store.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Returns the active [`DiskStorageConfig`].
    #[must_use]
    pub const fn config(&self) -> DiskStorageConfig {
        self.config
    }

    /// Returns `true` if no widget entries are tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the number of tracked widget entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns total persisted payload bytes across all widgets.
    #[must_use]
    pub const fn total_bytes(&self) -> usize {
        self.total_data_bytes
    }

    /// Looks up stored [`StorageMeta`] for `key`.
    #[must_use]
    pub fn lookup(&self, key: WidgetMsgKey) -> Option<StorageMeta> {
        self.entries.get(&key).map(|e| e.meta)
    }

    /// Reads the stored [`HeightRecord`] for `key`.
    #[must_use]
    pub fn height(&self, key: WidgetMsgKey) -> Option<HeightRecord> {
        self.entries
            .get(&key)
            .and_then(|e| e.height)
            .filter(|h| h.height.is_finite() && h.height > 0.0)
    }

    /// Returns the earliest positive expiry timestamp across all entries (`StorageDisk::nextExpiry`).
    #[must_use]
    pub fn next_expiry(&self) -> Option<i32> {
        self.entries
            .values()
            .filter(|e| e.meta.expires > 0)
            .map(|e| e.meta.expires)
            .min()
    }

    /// Reads the persisted [`StorageArea`] for `key`, updating its LRU timestamp.
    ///
    /// Legacy records exceeding the active per-widget quota are retired on read and removed from
    /// disk, returning `Ok(None)` (matching `tgw_storage_disk.cpp:170-181`).
    pub fn read(&mut self, key: WidgetMsgKey) -> Result<Option<StorageArea>> {
        let file_key = match self.entries.get(&key) {
            Some(entry) if entry.file_key != 0 => entry.file_key,
            _ => return Ok(None),
        };
        let path = self.record_path(file_key);
        let raw = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(TgwError::Storage(format!(
                    "read widget storage record {}: {err}",
                    path.display()
                )));
            }
        };
        let payload = match unwrap_framed_payload(&raw, RECORD_MAGIC) {
            Ok(p) => p,
            Err(_) => return Ok(Some(StorageArea::new())),
        };
        let area = match decode_storage_record(payload) {
            Ok(a) => a,
            Err(_) => return Ok(Some(StorageArea::new())),
        };

        let bytes = storage_area_bytes(&area);
        if bytes > self.config.per_widget_quota_bytes {
            // Retire legacy over-quota record.
            let _ = fs::remove_file(&path);
            if let Some(entry) = self.entries.get_mut(&key) {
                self.total_data_bytes = self.total_data_bytes.saturating_sub(entry.data_bytes);
                entry.file_key = 0;
                entry.data_bytes = 0;
                if entry.height.is_none() {
                    self.entries.remove(&key);
                }
            }
            self.write_index()?;
            return Ok(None);
        }

        let seq = self.tick_access();
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.last_access = seq;
            self.index_dirty = true;
        }
        Ok(Some(area))
    }

    /// Atomically persists `local` for `key`, enforcing per-widget and global quotas with LRU
    /// eviction (`StorageDisk::write`).
    pub fn write(
        &mut self,
        key: WidgetMsgKey,
        meta: StorageMeta,
        local: &StorageArea,
    ) -> Result<DiskWriteOutcome> {
        if !is_storable(key) {
            return Ok(DiskWriteOutcome::default());
        }
        let area_bytes = storage_area_bytes(local);
        if area_bytes > self.config.per_widget_quota_bytes {
            return Err(TgwError::Storage(format!(
                "widget storage area ({area_bytes} bytes) exceeds per-widget quota ({} bytes)",
                self.config.per_widget_quota_bytes
            )));
        }
        if area_bytes > self.config.total_quota_bytes {
            return Err(TgwError::Storage(format!(
                "widget storage area ({area_bytes} bytes) exceeds total quota ({} bytes)",
                self.config.total_quota_bytes
            )));
        }

        if !self.entries.contains_key(&key) && local.is_empty() {
            return Ok(DiskWriteOutcome::default());
        }

        let encoded = encode_storage_record_with_limit(local, self.config.per_widget_quota_bytes)?;
        let framed = wrap_framed_payload(RECORD_MAGIC, &encoded);

        let seq = self.tick_access();
        let existing_file = self.entries.get(&key).map_or(0, |e| e.file_key);
        let attach = existing_file == 0 && !local.is_empty();
        let file_key = if attach {
            self.allocate_file_key()
        } else {
            existing_file
        };

        if file_key != 0 {
            let record_path = self.record_path(file_key);
            atomic_write(&record_path, &framed)?;
        }

        let prev_bytes = self.entries.get(&key).map_or(0, |e| e.data_bytes);
        let prev_meta = self.entries.get(&key).map(|e| e.meta);
        let changed = attach || prev_meta != Some(meta) || prev_bytes != area_bytes;

        let entry = self.entries.entry(key).or_insert_with(|| DiskEntry {
            file_key: 0,
            meta,
            height: None,
            data_bytes: 0,
            last_access: seq,
        });
        entry.file_key = file_key;
        entry.meta = meta;
        entry.data_bytes = if file_key != 0 { area_bytes } else { 0 };
        entry.last_access = seq;

        self.total_data_bytes = self
            .total_data_bytes
            .saturating_sub(prev_bytes)
            .saturating_add(entry.data_bytes);

        let evicted = if self.total_data_bytes > self.config.total_quota_bytes {
            self.evict_lru_excluding(Some(key))?
        } else {
            Vec::new()
        };

        if changed || !evicted.is_empty() || self.index_dirty {
            self.write_index()?;
        }

        Ok(DiskWriteOutcome { changed, evicted })
    }

    /// Caches a positive finite [`HeightRecord`] for `key` (marks index dirty until [`Self::flush`]).
    pub fn set_height(
        &mut self,
        key: WidgetMsgKey,
        meta: StorageMeta,
        height: HeightRecord,
    ) -> bool {
        if !is_storable(key) || !height.height.is_finite() || height.height <= 0.0 {
            return false;
        }
        let seq = self.tick_access();
        let entry = self.entries.entry(key).or_insert_with(|| DiskEntry {
            file_key: 0,
            meta,
            height: None,
            data_bytes: 0,
            last_access: seq,
        });
        if entry.meta == meta && entry.height == Some(height) {
            return false;
        }
        entry.meta = meta;
        entry.height = Some(height);
        entry.last_access = seq;
        self.index_dirty = true;
        true
    }

    /// Updates metadata for `key` if present and persists the index (`StorageDisk::refresh`).
    pub fn refresh(&mut self, key: WidgetMsgKey, meta: StorageMeta) -> Result<bool> {
        let Some(entry) = self.entries.get_mut(&key) else {
            return Ok(false);
        };
        if entry.meta == meta {
            return Ok(false);
        }
        entry.meta = meta;
        self.write_index()?;
        Ok(true)
    }

    /// Removes all entries matching `predicate`, deleting their record files atomically.
    pub fn remove_where<F>(&mut self, mut predicate: F) -> Result<usize>
    where
        F: FnMut(WidgetMsgKey, &StorageMeta) -> bool,
    {
        let mut to_remove = Vec::new();
        for (&key, entry) in &self.entries {
            if predicate(key, &entry.meta) {
                to_remove.push((key, entry.file_key, entry.data_bytes));
            }
        }
        if to_remove.is_empty() {
            return Ok(0);
        }
        for (key, file_key, bytes) in &to_remove {
            self.entries.remove(key);
            self.total_data_bytes = self.total_data_bytes.saturating_sub(*bytes);
            if *file_key != 0 {
                let _ = fs::remove_file(self.record_path(*file_key));
            }
        }
        self.write_index()?;
        Ok(to_remove.len())
    }

    /// Removes all entries belonging to `peer_id`.
    pub fn clear_chat(&mut self, peer_id: i64) -> Result<usize> {
        self.remove_where(|k, _| k.peer_id == peer_id)
    }

    /// Removes all entries whose `meta.expires > 0 && meta.expires <= now_unix`.
    pub fn prune_expired(&mut self, now_unix: i32) -> Result<usize> {
        self.remove_where(|_, meta| meta.expires > 0 && meta.expires <= now_unix)
    }

    /// Moves a persisted entry from `from` to `to` (e.g. pending message assigned a server ID).
    pub fn move_entry(&mut self, from: WidgetMsgKey, to: WidgetMsgKey) -> Result<bool> {
        if from == to || !is_storable(to) {
            return Ok(false);
        }
        let Some(entry) = self.entries.remove(&from) else {
            return Ok(false);
        };
        if let Some(old_target) = self.entries.insert(to, entry) {
            self.total_data_bytes = self.total_data_bytes.saturating_sub(old_target.data_bytes);
            if old_target.file_key != 0 {
                let _ = fs::remove_file(self.record_path(old_target.file_key));
            }
        }
        self.write_index()?;
        Ok(true)
    }

    /// Flushes any pending index changes (such as [`Self::set_height`] or LRU reads) to disk.
    pub fn flush(&mut self) -> Result<()> {
        if self.index_dirty {
            self.write_index()?;
        }
        Ok(())
    }

    /// Erases all entries and removes all index/record files from disk (`StorageDisk::erase`).
    pub fn erase(&mut self) -> Result<()> {
        self.entries.clear();
        self.total_data_bytes = 0;
        self.index_dirty = false;
        self.remove_all_files()
    }

    fn tick_access(&mut self) -> u64 {
        self.access_clock = self.access_clock.saturating_add(1);
        self.access_clock
    }

    fn allocate_file_key(&mut self) -> u64 {
        let used: HashSet<u64> = self
            .entries
            .values()
            .filter(|e| e.file_key != 0)
            .map(|e| e.file_key)
            .collect();
        while self.next_file_key == 0 || used.contains(&self.next_file_key) {
            self.next_file_key = self.next_file_key.wrapping_add(1);
            if self.next_file_key == 0 {
                self.next_file_key = 1;
            }
        }
        let key = self.next_file_key;
        self.next_file_key = self.next_file_key.wrapping_add(1).max(1);
        key
    }

    fn evict_lru_excluding(&mut self, protect: Option<WidgetMsgKey>) -> Result<Vec<WidgetMsgKey>> {
        let mut candidates: Vec<(u64, WidgetMsgKey)> = self
            .entries
            .iter()
            .filter(|&(&k, e)| Some(k) != protect && e.data_bytes > 0)
            .map(|(&k, e)| (e.last_access, k))
            .collect();
        candidates.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

        let mut evicted = Vec::new();
        for (_, key) in candidates {
            if self.total_data_bytes <= self.config.total_quota_bytes {
                break;
            }
            if let Some(entry) = self.entries.remove(&key) {
                self.total_data_bytes = self.total_data_bytes.saturating_sub(entry.data_bytes);
                if entry.file_key != 0 {
                    let _ = fs::remove_file(self.record_path(entry.file_key));
                }
                evicted.push(key);
            }
        }
        if !evicted.is_empty() {
            self.write_index()?;
        }
        Ok(evicted)
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join(INDEX_FILE_NAME)
    }

    fn record_path(&self, file_key: u64) -> PathBuf {
        self.dir.join(format!("{file_key:016x}{RECORD_EXT}"))
    }

    fn write_index(&mut self) -> Result<()> {
        let mut buf = Vec::with_capacity(16 + self.entries.len() * INDEX_ENTRY_BYTES_V3);
        buf.extend_from_slice(&DISK_INDEX_VERSION.to_be_bytes());
        buf.extend_from_slice(&self.config.owner.to_be_bytes());
        buf.extend_from_slice(&(self.entries.len() as u32).to_be_bytes());

        for (key, entry) in &self.entries {
            let height_val = entry.height.map_or(0.0, |h| h.height);
            let bottom_flag: u32 = u32::from(entry.height.is_some_and(|h| h.bottom));
            buf.extend_from_slice(&key.peer_id.to_be_bytes());
            buf.extend_from_slice(&i64::from(key.msg_id).to_be_bytes());
            buf.extend_from_slice(&entry.file_key.to_be_bytes());
            buf.extend_from_slice(&entry.meta.date.to_be_bytes());
            buf.extend_from_slice(&entry.meta.expires.to_be_bytes());
            buf.extend_from_slice(&i64::from(entry.meta.topic_root_id).to_be_bytes());
            buf.extend_from_slice(&entry.meta.sublist_peer_id.to_be_bytes());
            buf.extend_from_slice(&entry.meta.from_peer_id.to_be_bytes());
            buf.extend_from_slice(&height_val.to_bits().to_be_bytes());
            buf.extend_from_slice(&bottom_flag.to_be_bytes());
            buf.extend_from_slice(&entry.last_access.to_be_bytes());
            buf.extend_from_slice(&(entry.data_bytes as u32).to_be_bytes());
        }

        let framed = wrap_framed_payload(INDEX_MAGIC, &buf);
        atomic_write(&self.index_path(), &framed)?;
        self.index_dirty = false;
        Ok(())
    }

    fn read_index(&mut self) -> Result<()> {
        let raw = fs::read(self.index_path())
            .map_err(|e| TgwError::Storage(format!("read widget storage index: {e}")))?;
        let payload = unwrap_framed_payload(&raw, INDEX_MAGIC)?;
        if payload.len() < 16 {
            return Err(TgwError::Storage(
                "truncated widget storage index".to_owned(),
            ));
        }
        let version = read_be_u32(payload, 0)?;
        let owner = read_be_u64(payload, 4)?;
        let count = read_be_u32(payload, 12)? as usize;

        if version == 0 || version > DISK_INDEX_VERSION {
            return Err(TgwError::Storage(format!(
                "unsupported widget storage index version {version}"
            )));
        }
        if owner != self.config.owner {
            return Err(TgwError::Storage(format!(
                "widget storage owner mismatch: expected {}, got {owner}",
                self.config.owner
            )));
        }

        let entry_size = match version {
            1 => 6 * 8 + 2 * 4,
            2 => 6 * 8 + 2 * 4 + 8 + 4,
            3 => INDEX_ENTRY_BYTES_V3,
            _ => unreachable!(),
        };
        let body = &payload[16..];
        if count.checked_mul(entry_size) != Some(body.len()) {
            return Err(TgwError::Storage(format!(
                "widget storage index length mismatch: count={count}, bytes={}",
                body.len()
            )));
        }

        let mut entries = BTreeMap::new();
        let mut seen_files = HashSet::new();
        let mut max_file_key = 0u64;
        let mut max_access = 0u64;
        let mut total_bytes = 0usize;

        for idx in 0..count {
            let slice = &body[idx * entry_size..(idx + 1) * entry_size];
            let peer_id = read_be_i64(slice, 0)?;
            let msg_raw = read_be_i64(slice, 8)?;
            let msg_id = i32::try_from(msg_raw)
                .map_err(|_| TgwError::Storage("message id out of i32 range".to_owned()))?;
            let file_key = read_be_u64(slice, 16)?;
            let date = read_be_i32(slice, 24)?;
            let expires = read_be_i32(slice, 28)?;
            let topic_root_raw = read_be_i64(slice, 32)?;
            let topic_root_id = i32::try_from(topic_root_raw).unwrap_or(0);
            let sublist_peer_id = read_be_i64(slice, 40)?;
            let from_peer_id = read_be_i64(slice, 48)?;

            let (height_val, anchor) = if version >= 2 {
                (
                    f64::from_bits(read_be_u64(slice, 56)?),
                    read_be_u32(slice, 64)?,
                )
            } else {
                (0.0, 0)
            };
            let (last_access, data_bytes) = if version >= 3 {
                (read_be_u64(slice, 68)?, read_be_u32(slice, 76)? as usize)
            } else {
                ((idx + 1) as u64, 0)
            };

            let key = WidgetMsgKey { peer_id, msg_id };
            if !is_storable(key)
                || !height_val.is_finite()
                || height_val < 0.0
                || anchor > 1
                || (file_key == 0 && height_val <= 0.0)
            {
                return Err(TgwError::Storage(
                    "invalid entry in widget storage index".to_owned(),
                ));
            }
            if file_key != 0 && !seen_files.insert(file_key) {
                return Err(TgwError::Storage(
                    "duplicate file key in widget storage index".to_owned(),
                ));
            }
            max_file_key = max_file_key.max(file_key);
            max_access = max_access.max(last_access);
            total_bytes = total_bytes.saturating_add(data_bytes);

            let height = (height_val > 0.0).then_some(HeightRecord {
                height: height_val,
                bottom: anchor == 1,
            });
            let entry = DiskEntry {
                file_key,
                meta: StorageMeta {
                    date,
                    expires,
                    topic_root_id,
                    sublist_peer_id,
                    from_peer_id,
                },
                height,
                data_bytes,
                last_access,
            };
            if entries.insert(key, entry).is_some() {
                return Err(TgwError::Storage(
                    "duplicate message key in widget storage index".to_owned(),
                ));
            }
        }

        self.entries = entries;
        self.next_file_key = max_file_key.saturating_add(1).max(1);
        self.access_clock = max_access;
        self.total_data_bytes = total_bytes;
        Ok(())
    }

    fn cleanup_orphans(&mut self) -> Result<()> {
        let referenced: HashSet<u64> = self
            .entries
            .values()
            .filter(|e| e.file_key != 0)
            .map(|e| e.file_key)
            .collect();
        let mut present = HashSet::new();

        let read_dir = fs::read_dir(&self.dir)
            .map_err(|e| TgwError::Storage(format!("scan widget storage dir: {e}")))?;
        for dir_entry in read_dir {
            let Ok(dir_entry) = dir_entry else {
                continue;
            };
            let path = dir_entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name == INDEX_FILE_NAME {
                continue;
            }
            if let Some(stem) = name.strip_suffix(RECORD_EXT)
                && stem.len() == 16
                && let Ok(key) = u64::from_str_radix(stem, 16)
                && referenced.contains(&key)
            {
                present.insert(key);
                continue;
            }
            // Orphan record or leftover atomic temp file.
            let _ = fs::remove_file(&path);
        }

        let before = self.entries.len();
        self.entries.retain(|_, entry| {
            if entry.file_key != 0 && !present.contains(&entry.file_key) {
                self.total_data_bytes = self.total_data_bytes.saturating_sub(entry.data_bytes);
                false
            } else {
                true
            }
        });
        if self.entries.len() != before {
            self.write_index()?;
        }
        Ok(())
    }

    fn remove_all_files(&self) -> Result<()> {
        let read_dir = fs::read_dir(&self.dir)
            .map_err(|e| TgwError::Storage(format!("read widget storage dir for erase: {e}")))?;
        for dir_entry in read_dir {
            let Ok(dir_entry) = dir_entry else {
                continue;
            };
            let path = dir_entry.path();
            if path.is_file() {
                let _ = fs::remove_file(&path);
            }
        }
        Ok(())
    }
}

fn is_storable(key: WidgetMsgKey) -> bool {
    key.msg_id > 0 && key.peer_id != 0
}

fn encode_storage_record_with_limit(area: &StorageArea, limit: usize) -> Result<Vec<u8>> {
    let bytes = storage_area_bytes(area);
    if bytes > limit {
        return Err(TgwError::Storage(format!(
            "storage area ({bytes} bytes) exceeds limit ({limit} bytes)"
        )));
    }
    if limit <= STORAGE_QUOTA_BYTES {
        return encode_storage_record(area);
    }
    if bytes > LEGACY_MAX_STORED_BYTES {
        return Err(TgwError::Storage(format!(
            "storage area ({bytes} bytes) exceeds legacy maximum ({LEGACY_MAX_STORED_BYTES} bytes)"
        )));
    }
    // Custom limit between STORAGE_QUOTA_BYTES and LEGACY_MAX_STORED_BYTES:
    // encode with the exact same binary layout so `decode_storage_record` parses it.
    let mut buf = Vec::new();
    buf.extend_from_slice(&crate::storage::STORAGE_RECORD_VERSION.to_be_bytes());
    buf.extend_from_slice(&(area.len() as u32).to_be_bytes());
    for (k, v) in area {
        write_record_bytes(&mut buf, k.as_bytes());
        write_record_bytes(&mut buf, v.as_bytes());
    }
    Ok(buf)
}

fn write_record_bytes(buf: &mut Vec<u8>, bytes: &[u8]) {
    if bytes.is_empty() {
        buf.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
    } else {
        buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        buf.extend_from_slice(bytes);
    }
}

fn wrap_framed_payload(magic: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let crc = crc32fast::hash(payload);
    let mut out = Vec::with_capacity(12 + payload.len());
    out.extend_from_slice(&magic);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&crc.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn unwrap_framed_payload(raw: &[u8], expected_magic: [u8; 4]) -> Result<&[u8]> {
    if raw.len() < 12 {
        return Err(TgwError::Storage(
            "truncated storage file envelope".to_owned(),
        ));
    }
    if raw[..4] != expected_magic {
        return Err(TgwError::Storage("invalid storage file magic".to_owned()));
    }
    let len = read_be_u32(raw, 4)? as usize;
    let expected_crc = read_be_u32(raw, 8)?;
    let payload = &raw[12..];
    if payload.len() != len {
        return Err(TgwError::Storage(format!(
            "storage file payload length mismatch: expected {len}, got {}",
            payload.len()
        )));
    }
    let actual_crc = crc32fast::hash(payload);
    if actual_crc != expected_crc {
        return Err(TgwError::Storage(format!(
            "storage file CRC32 mismatch: expected {expected_crc:#010x}, got {actual_crc:#010x}"
        )));
    }
    Ok(payload)
}

fn atomic_write(target: &Path, bytes: &[u8]) -> Result<()> {
    let parent = target.parent().ok_or_else(|| {
        TgwError::Storage(format!("storage path has no parent: {}", target.display()))
    })?;
    let file_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("data");
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_path = parent.join(format!("{file_name}.tmp.{}.{seq}", std::process::id()));

    let write_res = (|| -> std::io::Result<()> {
        let mut file = File::create(&tmp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(TgwError::Storage(format!(
            "write temp storage file {}: {err}",
            tmp_path.display()
        )));
    }

    if let Err(err) = fs::rename(&tmp_path, target) {
        let _ = fs::remove_file(&tmp_path);
        return Err(TgwError::Storage(format!(
            "atomically replace {}: {err}",
            target.display()
        )));
    }
    Ok(())
}

fn read_be_u32(slice: &[u8], offset: usize) -> Result<u32> {
    let arr: [u8; 4] = slice
        .get(offset..offset + 4)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| TgwError::Storage("truncated u32 in storage file".to_owned()))?;
    Ok(u32::from_be_bytes(arr))
}

fn read_be_i32(slice: &[u8], offset: usize) -> Result<i32> {
    let arr: [u8; 4] = slice
        .get(offset..offset + 4)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| TgwError::Storage("truncated i32 in storage file".to_owned()))?;
    Ok(i32::from_be_bytes(arr))
}

fn read_be_u64(slice: &[u8], offset: usize) -> Result<u64> {
    let arr: [u8; 8] = slice
        .get(offset..offset + 8)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| TgwError::Storage("truncated u64 in storage file".to_owned()))?;
    Ok(u64::from_be_bytes(arr))
}

fn read_be_i64(slice: &[u8], offset: usize) -> Result<i64> {
    let arr: [u8; 8] = slice
        .get(offset..offset + 8)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| TgwError::Storage("truncated i64 in storage file".to_owned()))?;
    Ok(i64::from_be_bytes(arr))
}
