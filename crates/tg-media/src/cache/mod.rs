//! Local media file cache with LRU eviction and tdesktop-compatible cache key derivation.
//!
//! Port of `Telegram/SourceFiles/data/data_types.h`, `data/data_types.cpp`,
//! `Telegram/SourceFiles/ui/image/image_location.cpp`, and
//! `Telegram/lib_storage/storage/cache/storage_cache_types.h`.

use crate::error::{MediaError, Result};
use sha1::{Digest as _, Sha1};
use sha2::Sha256;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Maximum single file size kept in memory / local media cache (`Storage::kMaxFileInMemory`).
pub const MAX_FILE_IN_MEMORY: usize = 10 * 1024 * 1024;

/// Maximum sticker size kept in memory (`Storage::kMaxStickerBytesSize`).
pub const MAX_STICKER_BYTES_SIZE: usize = 2 * 1024 * 1024;

/// Cache tag for images (`Data::kImageCacheTag`).
pub const IMAGE_CACHE_TAG: u8 = 0x01;
/// Cache tag for stickers (`Data::kStickerCacheTag`).
pub const STICKER_CACHE_TAG: u8 = 0x02;
/// Cache tag for voice messages (`Data::kVoiceMessageCacheTag`).
pub const VOICE_MESSAGE_CACHE_TAG: u8 = 0x03;
/// Cache tag for round video messages (`Data::kVideoMessageCacheTag`).
pub const VIDEO_MESSAGE_CACHE_TAG: u8 = 0x04;
/// Cache tag for animations / GIFs (`Data::kAnimationCacheTag`).
pub const ANIMATION_CACHE_TAG: u8 = 0x05;

/// Prefix stored ahead of partially downloaded progressive files (`file_download.cpp:230`).
pub const PARTIAL_PREFIX: &[u8; 8] = b"partial:";

// Cache tag constants from `data/data_types.cpp:20-27`.
const DOCUMENT_CACHE_TAG: u64 = 0x0000_0000_0000_0100;
const DOCUMENT_CACHE_MASK: u64 = 0x0000_0000_0000_00FF;
const DOCUMENT_THUMB_CACHE_TAG: u64 = 0x0000_0000_0000_0200;
const DOCUMENT_THUMB_CACHE_MASK: u64 = 0x0000_0000_0000_00FF;
const AUDIO_ALBUM_THUMB_CACHE_TAG: u64 = 0x0000_0000_0000_0300;
const WEB_DOCUMENT_CACHE_TAG: u64 = 0x0000_0200_0000_0000;
const URL_CACHE_TAG: u64 = 0x0000_0300_0000_0000;
const GEO_POINT_CACHE_TAG: u64 = 0x0000_0400_0000_0000;

// Big-file base cache key tags from `ui/image/image_location.cpp:23-26`.
const DOCUMENT_BASE_CACHE_TAG: u64 = 0x0000_0000_0001_0000;
const DOCUMENT_BASE_CACHE_MASK: u64 = 0x0000_0000_0000_FF00;
const PHOTO_BASE_CACHE_TAG: u64 = 0x0000_0000_0002_0000;
const PHOTO_BASE_CACHE_MASK: u64 = 0x0000_0000_0000_FF00;

/// 128-bit storage cache key matching `Storage::Cache::Key` (`storage_cache_types.h:19`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct StorageKey {
    /// Upper 64 bits (tag, DC id, type, size letter, etc.).
    pub high: u64,
    /// Lower 64 bits (document/photo/volume id or hash).
    pub low: u64,
}

impl StorageKey {
    /// Creates a new `StorageKey` from `high` and `low` 64-bit parts.
    #[must_use]
    pub const fn new(high: u64, low: u64) -> Self {
        Self { high, low }
    }

    /// Returns `true` if either half is non-zero (`Storage::Cache::Key::valid`).
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.high != 0 || self.low != 0
    }

    /// Formats this key as a 32-character lowercase hexadecimal filename stem.
    #[must_use]
    pub fn to_hex(self) -> String {
        format!("{:016x}{:016x}", self.high, self.low)
    }

    /// Parses a 32-character hexadecimal stem produced by [`StorageKey::to_hex`].
    #[must_use]
    pub fn from_hex(s: &str) -> Option<Self> {
        // Byte-checked first: slicing a non-ASCII name at byte 16 would panic, and
        // `from_str_radix` alone would accept a `+` sign.
        if s.len() != 32 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let (high, low) = s.split_at(16);
        Some(Self {
            high: u64::from_str_radix(high, 16).ok()?,
            low: u64::from_str_radix(low, 16).ok()?,
        })
    }

    /// Derives a part-specific key for streamed/big-file chunks (`base.low + part_index`).
    #[must_use]
    pub const fn with_part_offset(self, part_index: u64) -> Self {
        Self {
            high: self.high,
            low: self.low.wrapping_add(part_index),
        }
    }
}

/// Discriminant matching `StorageFileLocation::Type` (`ui/image/image_location.h:55`).
///
/// Numeric values are serialized and embedded directly into cache keys; do not reorder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum StorageLocationType {
    /// Legacy volume_id + local_id location.
    Legacy = 0x00,
    /// Secret chat encrypted file location.
    Encrypted = 0x01,
    /// Cloud document or document thumbnail.
    Document = 0x02,
    /// Telegram Passport secure file.
    Secure = 0x03,
    /// Takeout file location.
    Takeout = 0x04,
    /// Cloud photo size location.
    Photo = 0x05,
    /// Peer profile photo location.
    PeerPhoto = 0x06,
    /// Sticker set thumbnail location.
    StickerSetThumb = 0x07,
    /// Group call stream segment location.
    GroupCallStream = 0x08,
}

/// Parameters describing a Telegram file location for cache key derivation,
/// mirroring `StorageFileLocation` (`ui/image/image_location.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StorageLocationKeyInput {
    /// Location type tag.
    pub kind: StorageLocationType,
    /// Data center ID.
    pub dc_id: i32,
    /// Primary object ID (`document.id`, `photo.id`, `stickerset.id`, `peer.id`, `call.id`).
    pub id: u64,
    /// Volume ID (legacy), `photo_id` (peer photo), or `time_ms` (group call stream).
    pub volume_id: u64,
    /// Local ID (legacy), `thumb_version` (sticker set thumb), or `scale` (group call stream).
    pub local_id: i32,
    /// Single-byte size letter (e.g., `b'x'`, `b'm'`, `b'a'`, `b'c'`, or `0` for full document).
    pub size_letter: u8,
}

/// Derives `Storage::Cache::Key` for a `StorageFileLocation`
/// (`StorageFileLocation::cacheKey` in `ui/image/image_location.cpp:457`).
#[must_use]
pub const fn storage_file_cache_key(loc: StorageLocationKeyInput) -> StorageKey {
    // Skip 1 (legacy document cache key) and 2 (good fullsize document thumbnails).
    let shifted = ((loc.kind as u64) + 3) << 8;
    let sliced = (loc.dc_id as u64) & 0xFF;

    match loc.kind {
        StorageLocationType::Legacy | StorageLocationType::PeerPhoto => StorageKey {
            high: shifted | sliced | ((loc.local_id as u32 as u64) << 16),
            low: loc.volume_id,
        },
        StorageLocationType::StickerSetThumb => StorageKey {
            high: shifted | sliced | ((loc.local_id as u32 as u64) << 16),
            low: loc.id,
        },
        StorageLocationType::Encrypted | StorageLocationType::Secure => StorageKey {
            high: shifted | sliced,
            low: loc.id,
        },
        StorageLocationType::Document => {
            if loc.size_letter == 0 {
                document_cache_key(loc.dc_id, loc.id)
            } else {
                StorageKey {
                    high: shifted | sliced | ((loc.size_letter as u64) << 16),
                    low: loc.id,
                }
            }
        }
        StorageLocationType::Photo => StorageKey {
            high: shifted | sliced | ((loc.size_letter as u64) << 16),
            low: loc.id,
        },
        StorageLocationType::Takeout => StorageKey {
            high: shifted,
            low: 0,
        },
        StorageLocationType::GroupCallStream => {
            // Note: matches C++ `(uint32(_localId) << 16)` 32-bit shift before widening.
            let local_shifted = (loc.local_id as u32).wrapping_shl(16) as u64;
            StorageKey {
                high: shifted
                    | sliced
                    | local_shifted
                    | (loc.volume_id.wrapping_shl(20))
                    | ((loc.size_letter as u64) << 56),
                low: loc.id,
            }
        }
    }
}

/// Derives the base cache key for chunked big-file caching
/// (`StorageFileLocation::bigFileBaseCacheKey` in `ui/image/image_location.cpp:505`).
#[must_use]
pub const fn big_file_base_cache_key(loc: StorageLocationKeyInput) -> Option<StorageKey> {
    match loc.kind {
        StorageLocationType::Document => {
            // Exact formula from image_location.cpp:508-514.
            let high = DOCUMENT_BASE_CACHE_TAG
                | (((loc.dc_id as u64) << 16) & DOCUMENT_BASE_CACHE_MASK)
                | (loc.id >> 48);
            let low = loc.id << 16;
            Some(StorageKey { high, low })
        }
        StorageLocationType::StickerSetThumb => {
            let high = ((loc.local_id as u32 as u64) << 24)
                | (((loc.kind as u64) + 1) << 16)
                | (((loc.dc_id as u64) & 0xFF) << 8)
                | (loc.id >> 56);
            let low = loc.id << 8;
            Some(StorageKey { high, low })
        }
        StorageLocationType::Photo => {
            let high = PHOTO_BASE_CACHE_TAG
                | (((loc.dc_id as u64) << 16) & PHOTO_BASE_CACHE_MASK)
                | (loc.id >> 48);
            let low = loc.id << 16;
            Some(StorageKey { high, low })
        }
        _ => None,
    }
}

/// Derives `Data::DocumentCacheKey(dcId, id)` (`data/data_types.cpp:31`).
#[must_use]
pub const fn document_cache_key(dc_id: i32, id: u64) -> StorageKey {
    StorageKey {
        high: DOCUMENT_CACHE_TAG | ((dc_id as u64) & DOCUMENT_CACHE_MASK),
        low: id,
    }
}

/// Derives `Data::DocumentThumbCacheKey(dcId, id)` (`data/data_types.cpp:38`).
#[must_use]
pub const fn document_thumb_cache_key(dc_id: i32, id: u64) -> StorageKey {
    StorageKey {
        high: DOCUMENT_THUMB_CACHE_TAG | ((dc_id as u64) & DOCUMENT_THUMB_CACHE_MASK),
        low: id,
    }
}

/// Derives `Data::AudioAlbumThumbCacheKey` (`data/data_types.cpp:93`).
#[must_use]
pub const fn audio_album_thumb_cache_key(document_id: u64) -> StorageKey {
    StorageKey {
        high: AUDIO_ALBUM_THUMB_CACHE_TAG,
        low: document_id,
    }
}

/// Derives `Data::WebDocumentCacheKey` (`data/data_types.cpp:46`).
#[must_use]
pub fn web_document_cache_key(url: &[u8]) -> StorageKey {
    const CACHE_DC_ID: u64 = 4;
    let hash = Sha256::digest(url);
    let part1 = u32::from_le_bytes([hash[0], hash[1], hash[2], hash[3]]);
    let part2 = u64::from_le_bytes([
        hash[4], hash[5], hash[6], hash[7], hash[8], hash[9], hash[10], hash[11],
    ]);
    StorageKey {
        high: WEB_DOCUMENT_CACHE_TAG | ((CACHE_DC_ID & 0xFF) << 32) | u64::from(part1),
        low: part2,
    }
}

/// Derives `Data::UrlCacheKey` (`data/data_types.cpp:62`).
#[must_use]
pub fn url_cache_key(url: &str) -> StorageKey {
    let hash = Sha256::digest(url.as_bytes());
    let part1 = u32::from_le_bytes([hash[0], hash[1], hash[2], hash[3]]);
    let part2 = u64::from_le_bytes([
        hash[4], hash[5], hash[6], hash[7], hash[8], hash[9], hash[10], hash[11],
    ]);
    let part3 = u16::from_le_bytes([hash[12], hash[13]]);
    StorageKey {
        high: URL_CACHE_TAG | (u64::from(part3) << 32) | u64::from(part1),
        low: part2,
    }
}

/// Derives `Data::GeoPointCacheKey` (`data/data_types.cpp:80`).
#[must_use]
pub fn geo_point_cache_key(
    lat: f64,
    lon: f64,
    width: i32,
    height: i32,
    zoom: i32,
    scale: i32,
) -> StorageKey {
    let zoomscale = (((zoom as u32) & 0x0F) << 8) | ((scale as u32) & 0x0F);
    let widthheight = (((width as u32) & 0xFFFF) << 16) | ((height as u32) & 0xFFFF);
    let lat_enc = ((lat + 360.0).abs() * 1_000_000.0).round() as u64;
    let lon_enc = ((lon + 360.0).abs() * 1_000_000.0).round() as u64;
    StorageKey {
        high: GEO_POINT_CACHE_TAG | (u64::from(zoomscale) << 32) | u64::from(widthheight),
        low: (lat_enc << 32) | lon_enc,
    }
}

/// Derives `inMemoryKey(const WebFileLocation&)` / `inMemoryKey(const InMemoryLocation&)` using SHA-1
/// (`ui/image/image_location.cpp:693, 726`).
#[must_use]
pub fn sha1_in_memory_key(bytes: &[u8]) -> StorageKey {
    let hash = Sha1::digest(bytes);
    let high = u64::from_le_bytes([
        hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7],
    ]);
    let low = u64::from_le_bytes([
        hash[8], hash[9], hash[10], hash[11], hash[12], hash[13], hash[14], hash[15],
    ]);
    StorageKey { high, low }
}

/// Legacy `LocationType` constants used in `mediaKey` (`data/data_types.h:175`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum MediaLocationType {
    /// Photo / unknown file location.
    Unknown = 0,
    /// `inputDocumentFileLocation`.
    Document = 0x4e45_abe9,
    /// `inputAudioFileLocation`.
    Audio = 0x74dc_404d,
    /// `inputVideoFileLocation`.
    Video = 0x3d03_64ec,
    /// `inputSecureFileLocation`.
    Secure = 0xcbc7_ee28,
}

/// Computes tdesktop's `mediaKey(type, dc, id)` pair (`storage/localstorage.h`).
#[must_use]
pub const fn media_key(location_type: MediaLocationType, dc_id: i32, id: u64) -> StorageKey {
    let high = ((location_type as u64) << 32) | ((dc_id as u32) as u64);
    StorageKey { high, low: id }
}

/// Wraps a partially downloaded buffer with tdesktop's `"partial:"` marker (`file_download.cpp:482`).
#[must_use]
pub fn encode_partial_payload(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(PARTIAL_PREFIX.len() + data.len());
    out.extend_from_slice(PARTIAL_PREFIX);
    out.extend_from_slice(data);
    out
}

/// Checks whether a cached payload carries the `"partial:"` prefix and strips it if present.
#[must_use]
pub fn decode_partial_payload(data: &[u8]) -> Option<&[u8]> {
    data.strip_prefix(PARTIAL_PREFIX.as_slice())
}

// ============================================================================
// LocalFileCache (disk-backed LRU cache with size/time limits and tag stats)
// ============================================================================

/// Configuration for [`LocalFileCache`], mirroring `Storage::Cache::details::Settings`.
#[derive(Debug, Clone)]
pub struct CacheSettings {
    /// Directory where cached files and metadata are stored.
    pub root_dir: PathBuf,
    /// Maximum total bytes across all cached entries (default 1 GiB as in tdesktop).
    pub total_size_limit: u64,
    /// Maximum age of an entry since last access before expiration (default 31 days).
    pub total_time_limit: Option<Duration>,
    /// Maximum size of a single cached entry (default [`MAX_FILE_IN_MEMORY`] = 10 MiB).
    pub max_entry_size: usize,
}

impl CacheSettings {
    /// Creates settings with tdesktop defaults rooted at `root_dir`.
    #[must_use]
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
            total_size_limit: 1024 * 1024 * 1024,
            total_time_limit: Some(Duration::from_secs(31 * 24 * 60 * 60)),
            max_entry_size: MAX_FILE_IN_MEMORY,
        }
    }
}

/// Summary counts for a tag or the entire cache (`Storage::Cache::details::TaggedSummary`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheSummary {
    /// Number of entries.
    pub count: usize,
    /// Sum of entry byte sizes.
    pub total_size: u64,
}

/// Cache statistics snapshot (`Storage::Cache::details::Stats`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Aggregate count and byte size across all tags.
    pub full: CacheSummary,
    /// Per-tag count and byte size.
    pub tagged: BTreeMap<u8, CacheSummary>,
}

#[derive(Debug, Clone)]
struct CacheEntryMeta {
    tag: u8,
    size: u64,
    last_access_epoch_ms: u64,
    lru_seq: u64,
}

/// Disk-backed local media cache with LRU eviction, per-tag summaries, and size/age limits.
pub struct LocalFileCache {
    settings: CacheSettings,
    entries: HashMap<StorageKey, CacheEntryMeta>,
    lru_order: BTreeMap<u64, StorageKey>,
    next_seq: u64,
    total_size: u64,
}

impl LocalFileCache {
    /// Opens (or creates) a [`LocalFileCache`] in `settings.root_dir`, rebuilding its index
    /// from any existing cached files on disk.
    pub fn open(settings: CacheSettings) -> Result<Self> {
        std::fs::create_dir_all(&settings.root_dir)?;
        let mut cache = Self {
            settings,
            entries: HashMap::new(),
            lru_order: BTreeMap::new(),
            next_seq: 1,
            total_size: 0,
        };
        cache.scan_existing()?;
        cache.evict_expired(now_epoch_ms())?;
        cache.evict_to_size_limit()?;
        Ok(cache)
    }

    /// Returns the configured root directory.
    #[must_use]
    pub fn root_dir(&self) -> &Path {
        &self.settings.root_dir
    }

    /// Updates size and time limits and immediately prunes any excess/expired entries.
    pub fn update_limits(
        &mut self,
        total_size_limit: u64,
        total_time_limit: Option<Duration>,
    ) -> Result<()> {
        self.settings.total_size_limit = total_size_limit;
        self.settings.total_time_limit = total_time_limit;
        self.evict_expired(now_epoch_ms())?;
        self.evict_to_size_limit()?;
        Ok(())
    }

    /// Stores `bytes` under `key` with cache `tag`, evicting least-recently-used entries
    /// as needed to stay within `total_size_limit`.
    pub fn put(&mut self, key: StorageKey, tag: u8, bytes: &[u8]) -> Result<()> {
        self.put_at(key, tag, bytes, now_epoch_ms())
    }

    /// Like [`LocalFileCache::put`], with an explicit timestamp in milliseconds (for deterministic tests).
    pub fn put_at(&mut self, key: StorageKey, tag: u8, bytes: &[u8], now_ms: u64) -> Result<()> {
        if !key.is_valid() {
            return Err(MediaError::Cache(
                "cannot store under zero StorageKey".into(),
            ));
        }
        if bytes.len() > self.settings.max_entry_size {
            return Err(MediaError::LimitExceeded(format!(
                "cache entry {} bytes exceeds max_entry_size {}",
                bytes.len(),
                self.settings.max_entry_size
            )));
        }
        let size = bytes.len() as u64;
        if size > self.settings.total_size_limit {
            return Err(MediaError::LimitExceeded(format!(
                "cache entry {size} bytes exceeds total_size_limit {}",
                self.settings.total_size_limit
            )));
        }

        // Write file atomically with 1-byte tag prefix on disk so restarts preserve tags.
        let path = self.entry_path(key);
        let tmp_path = path.with_extension("tmp");
        let mut disk_bytes = Vec::with_capacity(1 + bytes.len());
        disk_bytes.push(tag);
        disk_bytes.extend_from_slice(bytes);
        std::fs::write(&tmp_path, &disk_bytes)?;
        // Windows rename fails if target exists; remove existing before rename.
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
        std::fs::rename(&tmp_path, &path)?;

        if let Some(old) = self.entries.remove(&key) {
            self.lru_order.remove(&old.lru_seq);
            self.total_size = self.total_size.saturating_sub(old.size);
        }

        let seq = self.alloc_seq();
        self.entries.insert(
            key,
            CacheEntryMeta {
                tag,
                size,
                last_access_epoch_ms: now_ms,
                lru_seq: seq,
            },
        );
        self.lru_order.insert(seq, key);
        self.total_size = self.total_size.saturating_add(size);

        self.evict_expired(now_ms)?;
        self.evict_to_size_limit()?;
        Ok(())
    }

    /// Stores `bytes` under `key` only if `key` is not already present (`putIfEmpty`).
    pub fn put_if_empty(&mut self, key: StorageKey, tag: u8, bytes: &[u8]) -> Result<bool> {
        if self.entries.contains_key(&key) {
            return Ok(false);
        }
        self.put(key, tag, bytes)?;
        Ok(true)
    }

    /// Copies an existing entry from `from` to `to` if `to` does not exist (`copyIfEmpty`).
    pub fn copy_if_empty(&mut self, from: StorageKey, to: StorageKey) -> Result<bool> {
        if from == to || self.entries.contains_key(&to) {
            return Ok(false);
        }
        let Some(tag) = self.entries.get(&from).map(|m| m.tag) else {
            return Ok(false);
        };
        let Some(bytes) = self.get(from)? else {
            return Ok(false);
        };
        self.put(to, tag, &bytes)?;
        Ok(true)
    }

    /// Reads the cached payload for `key`, updating its LRU recency.
    pub fn get(&mut self, key: StorageKey) -> Result<Option<Vec<u8>>> {
        self.get_at(key, now_epoch_ms())
    }

    /// Like [`LocalFileCache::get`], with an explicit timestamp in milliseconds.
    pub fn get_at(&mut self, key: StorageKey, now_ms: u64) -> Result<Option<Vec<u8>>> {
        if !key.is_valid() {
            return Ok(None);
        }
        let Some(meta) = self.entries.get(&key).cloned() else {
            return Ok(None);
        };
        if let Some(limit) = self.settings.total_time_limit {
            let limit_ms = limit.as_millis() as u64;
            if now_ms.saturating_sub(meta.last_access_epoch_ms) > limit_ms {
                self.remove(key)?;
                return Ok(None);
            }
        }

        let path = self.entry_path(key);
        let mut raw = match std::fs::read(&path) {
            Ok(data) if !data.is_empty() => data,
            Ok(_) => {
                self.remove(key)?;
                return Ok(None);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.entries.remove(&key);
                self.lru_order.remove(&meta.lru_seq);
                self.total_size = self.total_size.saturating_sub(meta.size);
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        };

        // Touch LRU order.
        let new_seq = self.alloc_seq();
        self.lru_order.remove(&meta.lru_seq);
        self.lru_order.insert(new_seq, key);
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.lru_seq = new_seq;
            entry.last_access_epoch_ms = now_ms;
        }

        // Drop the tag byte in place instead of copying the whole payload.
        raw.drain(..1);
        Ok(Some(raw))
    }

    /// Returns `true` if `key` is present in the cache index without updating LRU recency.
    #[must_use]
    pub fn contains(&self, key: StorageKey) -> bool {
        self.entries.contains_key(&key)
    }

    /// Removes `key` from disk and memory. Returns `true` if an entry was removed.
    pub fn remove(&mut self, key: StorageKey) -> Result<bool> {
        let Some(meta) = self.entries.remove(&key) else {
            return Ok(false);
        };
        self.lru_order.remove(&meta.lru_seq);
        self.total_size = self.total_size.saturating_sub(meta.size);
        let path = self.entry_path(key);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(true)
    }

    /// Removes all cached entries matching `tag`. Returns the number of entries removed.
    pub fn clear_by_tag(&mut self, tag: u8) -> Result<usize> {
        let keys: Vec<StorageKey> = self
            .entries
            .iter()
            .filter_map(|(&k, m)| (m.tag == tag).then_some(k))
            .collect();
        let mut removed = 0usize;
        for key in keys {
            if self.remove(key)? {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Removes all cached entries from disk and memory.
    pub fn clear(&mut self) -> Result<()> {
        let keys: Vec<StorageKey> = self.entries.keys().copied().collect();
        for key in keys {
            self.remove(key)?;
        }
        Ok(())
    }

    /// Computes aggregate and per-tag statistics (`Database::stats`).
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        let mut stats = CacheStats::default();
        for meta in self.entries.values() {
            stats.full.count += 1;
            stats.full.total_size += meta.size;
            let entry = stats.tagged.entry(meta.tag).or_default();
            entry.count += 1;
            entry.total_size += meta.size;
        }
        stats
    }

    /// Evicts entries older than `total_time_limit` relative to `now_ms`.
    pub fn evict_expired(&mut self, now_ms: u64) -> Result<usize> {
        let Some(limit) = self.settings.total_time_limit else {
            return Ok(0);
        };
        let limit_ms = limit.as_millis() as u64;
        let expired: Vec<StorageKey> = self
            .entries
            .iter()
            .filter_map(|(&k, m)| {
                (now_ms.saturating_sub(m.last_access_epoch_ms) > limit_ms).then_some(k)
            })
            .collect();
        let mut count = 0usize;
        for key in expired {
            if self.remove(key)? {
                count += 1;
            }
        }
        Ok(count)
    }

    fn evict_to_size_limit(&mut self) -> Result<()> {
        while self.total_size > self.settings.total_size_limit {
            let Some((&oldest_seq, &oldest_key)) = self.lru_order.iter().next() else {
                break;
            };
            self.lru_order.remove(&oldest_seq);
            if let Some(meta) = self.entries.remove(&oldest_key) {
                self.total_size = self.total_size.saturating_sub(meta.size);
                let path = self.entry_path(oldest_key);
                if path.exists() {
                    std::fs::remove_file(path)?;
                }
            }
        }
        Ok(())
    }

    fn scan_existing(&mut self) -> Result<()> {
        let mut found: Vec<(u64, StorageKey, u8, u64)> = Vec::new();
        // One entry that cannot be read (locked, a directory, removed meanwhile) is skipped; it
        // must not fail the whole cache.
        for entry in std::fs::read_dir(&self.settings.root_dir)?.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("bin") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some(key) = StorageKey::from_hex(stem) else {
                continue;
            };
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if !meta.is_file() || meta.len() < 1 {
                continue;
            }
            // Read only the 1-byte tag prefix.
            let Ok(mut file) = std::fs::File::open(&path) else {
                continue;
            };
            let mut tag_buf = [0u8; 1];
            use std::io::Read as _;
            if file.read_exact(&mut tag_buf).is_err() {
                continue;
            }
            let mtime_ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as u64);
            found.push((mtime_ms, key, tag_buf[0], meta.len() - 1));
        }
        // Sort oldest first so LRU sequence matches disk modification order.
        found.sort_by_key(|&(mtime, key, _, _)| (mtime, key));
        for (mtime, key, tag, size) in found {
            let seq = self.alloc_seq();
            self.entries.insert(
                key,
                CacheEntryMeta {
                    tag,
                    size,
                    last_access_epoch_ms: mtime,
                    lru_seq: seq,
                },
            );
            self.lru_order.insert(seq, key);
            self.total_size = self.total_size.saturating_add(size);
        }
        Ok(())
    }

    fn entry_path(&self, key: StorageKey) -> PathBuf {
        self.settings.root_dir.join(format!("{}.bin", key.to_hex()))
    }

    fn alloc_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
