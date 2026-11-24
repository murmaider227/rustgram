//! Streaming slice cache contract and `tg-storage` adapters.
//!
//! Port of `Storage::Cache::Key` / `Storage::Cache::Database` integration used by
//! `Telegram/SourceFiles/media/streaming/media_streaming_reader.cpp`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tg_storage::cache::binlog::{BinlogCache, CacheKey as BinlogCacheKey};
use tg_storage::cache::media_cache::{MediaCacheIndex, MediaCacheTag};

/// 128-bit streaming cache key, backed directly by [`tg_media::cache::StorageKey`].
pub type CacheKey = tg_media::cache::StorageKey;

/// Derives the key for a streaming slice (`slice_number == 0` is the header).
///
/// Mirrors `Storage::Cache::Key { base.high, base.low + slice_number }` using
/// [`tg_media::cache::StorageKey::with_part_offset`].
#[must_use]
pub const fn slice_cache_key(base: CacheKey, slice_number: u32) -> CacheKey {
    base.with_part_offset(slice_number as u64)
}

/// Persistent or in-memory slice cache trait mirroring `Storage::Cache::Database`
/// operations used by [`crate::streaming::StreamingReader`].
pub trait SliceCache: Send + Sync {
    /// Reads the cached bytes for `key`, if present.
    fn get(&self, key: CacheKey) -> Option<Vec<u8>>;

    /// Returns stored byte sizes for each requested key (`0` if absent).
    fn get_sizes(&self, keys: &[CacheKey]) -> Vec<usize> {
        keys.iter()
            .map(|k| self.get(*k).map_or(0, |v| v.len()))
            .collect()
    }

    /// Stores serialized slice bytes under `key`.
    fn put(&self, key: CacheKey, data: Vec<u8>);

    /// Flushes pending writes to durable storage (no-op for in-memory caches).
    fn sync(&self) {}
}

/// In-memory [`SliceCache`] implementation useful for tests and ephemeral caches.
#[derive(Debug, Clone, Default)]
pub struct MemorySliceCache {
    entries: Arc<Mutex<HashMap<CacheKey, Vec<u8>>>>,
}

impl MemorySliceCache {
    /// Creates an empty in-memory slice cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of cached entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.lock().map(|m| m.len()).unwrap_or(0)
    }

    /// Returns `true` if no entries are cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl SliceCache for MemorySliceCache {
    fn get(&self, key: CacheKey) -> Option<Vec<u8>> {
        self.entries.lock().ok()?.get(&key).cloned()
    }

    fn get_sizes(&self, keys: &[CacheKey]) -> Vec<usize> {
        let Ok(guard) = self.entries.lock() else {
            return vec![0; keys.len()];
        };
        keys.iter()
            .map(|k| guard.get(k).map_or(0, Vec::len))
            .collect()
    }

    fn put(&self, key: CacheKey, data: Vec<u8>) {
        if let Ok(mut guard) = self.entries.lock() {
            guard.insert(key, data);
        }
    }
}

/// [`SliceCache`] implementation backed by `tg-storage`'s SQLite [`MediaCacheIndex`].
pub struct MediaCacheSliceCache {
    index: Mutex<MediaCacheIndex>,
    tag: MediaCacheTag,
}

impl MediaCacheSliceCache {
    /// Wraps a [`MediaCacheIndex`] storing streaming slices under [`MediaCacheTag::Video`].
    #[must_use]
    pub fn new(index: MediaCacheIndex) -> Self {
        Self::with_tag(index, MediaCacheTag::Video)
    }

    /// Wraps a [`MediaCacheIndex`] storing streaming slices under the given `tag`.
    #[must_use]
    pub fn with_tag(index: MediaCacheIndex, tag: MediaCacheTag) -> Self {
        Self {
            index: Mutex::new(index),
            tag,
        }
    }

    /// Borrows the underlying mutex-guarded [`MediaCacheIndex`].
    #[must_use]
    pub fn index(&self) -> &Mutex<MediaCacheIndex> {
        &self.index
    }
}

impl SliceCache for MediaCacheSliceCache {
    fn get(&self, key: CacheKey) -> Option<Vec<u8>> {
        if !key.is_valid() {
            return None;
        }
        let guard = self.index.lock().ok()?;
        guard.get_blob(&key.to_hex()).ok().flatten()
    }

    fn put(&self, key: CacheKey, data: Vec<u8>) {
        if !key.is_valid() || data.is_empty() {
            return;
        }
        if let Ok(guard) = self.index.lock() {
            let _ = guard.put_blob(&key.to_hex(), self.tag, &data);
        }
    }
}

/// [`SliceCache`] implementation backed by `tg-storage`'s tdesktop-compatible [`BinlogCache`].
pub struct BinlogSliceCache {
    cache: Mutex<BinlogCache>,
    tag: u8,
}

impl BinlogSliceCache {
    /// Wraps a [`BinlogCache`] storing slices with the given category `tag`.
    #[must_use]
    pub fn new(cache: BinlogCache, tag: u8) -> Self {
        Self {
            cache: Mutex::new(cache),
            tag,
        }
    }

    /// Borrows the underlying mutex-guarded [`BinlogCache`].
    #[must_use]
    pub fn cache(&self) -> &Mutex<BinlogCache> {
        &self.cache
    }

    const fn to_binlog_key(key: CacheKey) -> BinlogCacheKey {
        BinlogCacheKey::new(key.high, key.low)
    }
}

impl SliceCache for BinlogSliceCache {
    fn get(&self, key: CacheKey) -> Option<Vec<u8>> {
        if !key.is_valid() {
            return None;
        }
        let guard = self.cache.lock().ok()?;
        guard.get(Self::to_binlog_key(key)).ok().flatten()
    }

    fn get_sizes(&self, keys: &[CacheKey]) -> Vec<usize> {
        let Ok(guard) = self.cache.lock() else {
            return vec![0; keys.len()];
        };
        keys.iter()
            .map(|&k| {
                if !k.is_valid() {
                    0
                } else {
                    guard.meta(Self::to_binlog_key(k)).map_or(0, |m| m.size)
                }
            })
            .collect()
    }

    fn put(&self, key: CacheKey, data: Vec<u8>) {
        if !key.is_valid() || data.is_empty() {
            return;
        }
        if let Ok(mut guard) = self.cache.lock() {
            let _ = guard.put(Self::to_binlog_key(key), &data, self.tag);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_storage::EncryptionKey;

    #[test]
    fn slice_cache_key_derivation_matches_storage_key() {
        let base = CacheKey::new(0x0001_0002, 0x0000_0010);
        assert!(base.is_valid());
        assert_eq!(slice_cache_key(base, 0), base);
        assert_eq!(
            slice_cache_key(base, 3),
            CacheKey::new(0x0001_0002, 0x0000_0013)
        );
    }

    #[test]
    fn media_cache_slice_cache_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let index = MediaCacheIndex::open_in_memory(dir.path()).unwrap();
        let cache = MediaCacheSliceCache::new(index);

        let k0 = slice_cache_key(CacheKey::new(0xABCD, 100), 0);
        let k1 = slice_cache_key(CacheKey::new(0xABCD, 100), 1);
        assert_eq!(cache.get(k0), None);

        cache.put(k0, vec![1, 2, 3, 4]);
        cache.put(k1, vec![5; 128]);

        assert_eq!(cache.get(k0), Some(vec![1, 2, 3, 4]));
        assert_eq!(cache.get(k1), Some(vec![5; 128]));
        assert_eq!(
            cache.get_sizes(&[k0, k1, CacheKey::new(0xABCD, 999)]),
            vec![4, 128, 0]
        );
    }

    #[test]
    fn binlog_slice_cache_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let key = EncryptionKey::new(vec![0x42u8; 256]).unwrap();
        let binlog = BinlogCache::open(dir.path(), key, false).unwrap();
        let cache = BinlogSliceCache::new(binlog, tg_media::cache::VIDEO_MESSAGE_CACHE_TAG);

        let k0 = slice_cache_key(CacheKey::new(0x1111, 200), 0);
        let k1 = slice_cache_key(CacheKey::new(0x1111, 200), 1);
        assert_eq!(cache.get(k0), None);

        cache.put(k0, vec![9, 8, 7]);
        cache.put(k1, vec![4; 64]);

        assert_eq!(cache.get(k0), Some(vec![9, 8, 7]));
        assert_eq!(cache.get(k1), Some(vec![4; 64]));
        assert_eq!(
            cache.get_sizes(&[k0, k1, CacheKey::new(0x1111, 999)]),
            vec![3, 64, 0]
        );
    }
}
