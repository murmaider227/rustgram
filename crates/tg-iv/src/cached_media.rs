//! Port of `Telegram/SourceFiles/iv/iv_cached_media.h` and `iv_cached_media.cpp`.
//!
//! Provides [`IvCachedMedia`], a thread-safe LRU byte cache for Instant View photos,
//! documents (video/audio/inline images), and geo-map tiles with magic-byte content-type
//! sniffing and optional disk backing.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::serve::{MediaBlob, MediaProvider};

/// Geo-point scale factor matching `kGeoPointScale` in `iv_cached_media.cpp`.
pub const GEO_POINT_SCALE: i32 = 1;

/// Minimum geo-point zoom level matching `kGeoPointZoomMin` in `iv_cached_media.cpp`.
pub const GEO_POINT_ZOOM_MIN: i32 = 13;

/// Default in-memory LRU capacity in bytes (`64 MiB`).
pub const DEFAULT_CACHE_MAX_BYTES: usize = 64 * 1024 * 1024;

/// Default maximum number of in-memory LRU entries.
pub const DEFAULT_CACHE_MAX_ENTRIES: usize = 1024;

/// Disk cache envelope magic header (`TGIV1\0`).
const DISK_MAGIC: &[u8; 6] = b"TGIV1\0";

/// Clamps a geo-map zoom level to at least [`GEO_POINT_ZOOM_MIN`].
#[must_use]
pub const fn normalize_geo_zoom(zoom: i32) -> i32 {
    if zoom < GEO_POINT_ZOOM_MIN {
        GEO_POINT_ZOOM_MIN
    } else {
        zoom
    }
}

/// Sniffs the MIME `Content-Type` of a media payload from its leading magic bytes.
///
/// Recognizes PNG, JPEG, GIF, WebP, BMP, AVIF/HEIC/MP4/QuickTime, WebM/Matroska,
/// Ogg, MP3, FLAC, WAV, SVG, and PDF; returns `"application/octet-stream"` otherwise.
#[must_use]
pub fn sniff_content_type(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 8 && bytes[..8] == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] {
        return "image/png";
    }
    if bytes.len() >= 3 && bytes[..3] == [0xFF, 0xD8, 0xFF] {
        return "image/jpeg";
    }
    if bytes.len() >= 6 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") {
        return "image/gif";
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" {
        if &bytes[8..12] == b"WEBP" {
            return "image/webp";
        }
        if &bytes[8..12] == b"WAVE" {
            return "audio/wav";
        }
    }
    if bytes.len() >= 2 && &bytes[..2] == b"BM" {
        return "image/bmp";
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        let brand = &bytes[8..12];
        if brand == b"avif" || brand == b"avis" {
            return "image/avif";
        }
        if brand == b"heic" || brand == b"heix" || brand == b"mif1" || brand == b"msf1" {
            return "image/heic";
        }
        if brand == b"qt  " {
            return "video/quicktime";
        }
        return "video/mp4";
    }
    if bytes.len() >= 4 && bytes[..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        return "video/webm";
    }
    if bytes.len() >= 4 && &bytes[..4] == b"OggS" {
        return "audio/ogg";
    }
    if bytes.len() >= 4 && &bytes[..4] == b"fLaC" {
        return "audio/flac";
    }
    if (bytes.len() >= 3 && &bytes[..3] == b"ID3")
        || (bytes.len() >= 2 && bytes[0] == 0xFF && matches!(bytes[1], 0xFB | 0xF3 | 0xF2))
    {
        return "audio/mpeg";
    }
    if bytes.len() >= 5 && &bytes[..5] == b"%PDF-" {
        return "application/pdf";
    }
    if looks_like_svg(bytes) {
        return "image/svg+xml";
    }
    "application/octet-stream"
}

fn looks_like_svg(bytes: &[u8]) -> bool {
    let prefix_len = bytes.len().min(512);
    let Ok(head) = std::str::from_utf8(&bytes[..prefix_len]) else {
        return false;
    };
    let trimmed = head.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '\u{FEFF}');
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("<svg") || (lower.starts_with("<?xml") && lower.contains("<svg"))
}

/// Quantized cache key for a geo-map tile request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MapTileKey {
    /// Latitude multiplied by `1_000_000` and rounded.
    pub lat_e6: i64,
    /// Longitude multiplied by `1_000_000` and rounded.
    pub lon_e6: i64,
    /// Zoom level.
    pub zoom: i32,
    /// Tile width in logical pixels.
    pub width: i32,
    /// Tile height in logical pixels.
    pub height: i32,
}

impl MapTileKey {
    /// Constructs a [`MapTileKey`] from floating-point coordinates and tile geometry.
    #[must_use]
    pub fn new(lat: f64, lon: f64, zoom: i32, width: i32, height: i32) -> Self {
        let lat_e6 = if lat.is_finite() {
            (lat * 1_000_000.0).round() as i64
        } else {
            0
        };
        let lon_e6 = if lon.is_finite() {
            (lon * 1_000_000.0).round() as i64
        } else {
            0
        };
        Self {
            lat_e6,
            lon_e6,
            zoom,
            width: width.max(1),
            height: height.max(1),
        }
    }
}

/// Key identifying a cached Instant View media resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CachedMediaKey {
    /// Telegram photo by ID.
    Photo(i64),
    /// Telegram document (video, audio, inline image, file) by ID.
    Document(i64),
    /// Geo-map tile snapshot.
    MapTile(MapTileKey),
}

impl CachedMediaKey {
    fn disk_file_name(self) -> String {
        match self {
            Self::Photo(id) => format!("photo_{id}.bin"),
            Self::Document(id) => format!("doc_{id}.bin"),
            Self::MapTile(k) => {
                format!(
                    "map_{}_{}_{}_{}x{}.bin",
                    k.lat_e6, k.lon_e6, k.zoom, k.width, k.height
                )
            }
        }
    }
}

/// Diagnostic counters for [`IvCachedMedia`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CachedMediaStats {
    /// Number of entries currently resident in memory.
    pub entry_count: usize,
    /// Total payload bytes currently resident in memory.
    pub total_bytes: usize,
    /// Total cache hits (RAM + disk).
    pub hits: u64,
    /// Total cache misses.
    pub misses: u64,
    /// Number of in-memory entries evicted due to byte or count limits.
    pub evictions: u64,
    /// Number of hits served by loading from the disk backing directory.
    pub disk_hits: u64,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    blob: MediaBlob,
    last_used: u64,
}

#[derive(Debug)]
struct IvCachedMediaInner {
    max_bytes: usize,
    max_entries: usize,
    total_bytes: usize,
    clock: u64,
    entries: HashMap<CachedMediaKey, CacheEntry>,
    lru_order: BTreeSet<(u64, CachedMediaKey)>,
    hits: u64,
    misses: u64,
    evictions: u64,
    disk_hits: u64,
}

impl IvCachedMediaInner {
    fn new(max_bytes: usize, max_entries: usize) -> Self {
        Self {
            max_bytes,
            max_entries,
            total_bytes: 0,
            clock: 0,
            entries: HashMap::new(),
            lru_order: BTreeSet::new(),
            hits: 0,
            misses: 0,
            evictions: 0,
            disk_hits: 0,
        }
    }

    fn next_tick(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }

    fn insert_ram(&mut self, key: CachedMediaKey, blob: MediaBlob) {
        if let Some(old) = self.entries.remove(&key) {
            self.lru_order.remove(&(old.last_used, key));
            self.total_bytes = self.total_bytes.saturating_sub(old.blob.bytes.len());
        }

        let item_len = blob.bytes.len();
        if self.max_entries == 0 || (self.max_bytes > 0 && item_len > self.max_bytes) {
            return;
        }

        while (!self.entries.is_empty() && self.entries.len() >= self.max_entries)
            || (self.max_bytes > 0
                && self.total_bytes.saturating_add(item_len) > self.max_bytes
                && !self.entries.is_empty())
        {
            let Some(&(oldest_tick, oldest_key)) = self.lru_order.first() else {
                break;
            };
            self.lru_order.remove(&(oldest_tick, oldest_key));
            if let Some(evicted) = self.entries.remove(&oldest_key) {
                self.total_bytes = self.total_bytes.saturating_sub(evicted.blob.bytes.len());
                self.evictions += 1;
            }
        }

        let tick = self.next_tick();
        self.total_bytes = self.total_bytes.saturating_add(item_len);
        self.lru_order.insert((tick, key));
        self.entries.insert(
            key,
            CacheEntry {
                blob,
                last_used: tick,
            },
        );
    }

    fn get_ram(&mut self, key: &CachedMediaKey) -> Option<MediaBlob> {
        let old_tick = self.entries.get(key)?.last_used;
        let new_tick = self.next_tick();
        self.lru_order.remove(&(old_tick, *key));
        self.lru_order.insert((new_tick, *key));
        if let Some(entry) = self.entries.get_mut(key) {
            entry.last_used = new_tick;
            self.hits += 1;
            return Some(entry.blob.clone());
        }
        None
    }

    fn remove_ram(&mut self, key: &CachedMediaKey) -> bool {
        if let Some(old) = self.entries.remove(key) {
            self.lru_order.remove(&(old.last_used, *key));
            self.total_bytes = self.total_bytes.saturating_sub(old.blob.bytes.len());
            true
        } else {
            false
        }
    }

    fn clear_ram(&mut self) {
        self.entries.clear();
        self.lru_order.clear();
        self.total_bytes = 0;
    }
}

/// Thread-safe LRU photo, document, and geo-map tile cache with content-type sniffing
/// and optional on-disk persistence (`iv/iv_cached_media.cpp`).
#[derive(Debug)]
pub struct IvCachedMedia {
    inner: Mutex<IvCachedMediaInner>,
    disk_dir: Option<PathBuf>,
}

impl Default for IvCachedMedia {
    fn default() -> Self {
        Self::with_defaults()
    }
}

impl IvCachedMedia {
    /// Creates an in-memory LRU cache with the specified byte and entry limits.
    ///
    /// Pass `max_bytes == 0` for no byte limit (bounded only by `max_entries`).
    #[must_use]
    pub fn new(max_bytes: usize, max_entries: usize) -> Self {
        Self {
            inner: Mutex::new(IvCachedMediaInner::new(max_bytes, max_entries)),
            disk_dir: None,
        }
    }

    /// Creates an in-memory LRU cache with [`DEFAULT_CACHE_MAX_BYTES`] and
    /// [`DEFAULT_CACHE_MAX_ENTRIES`].
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_CACHE_MAX_BYTES, DEFAULT_CACHE_MAX_ENTRIES)
    }

    /// Creates a disk-backed LRU cache storing evicted/persisted blobs in `dir`.
    ///
    /// Creates `dir` (and parent directories) if it does not already exist.
    pub fn with_disk_dir(
        dir: impl Into<PathBuf>,
        max_bytes: usize,
        max_entries: usize,
    ) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self {
            inner: Mutex::new(IvCachedMediaInner::new(max_bytes, max_entries)),
            disk_dir: Some(dir),
        })
    }

    /// Returns the disk backing directory, if configured.
    #[must_use]
    pub fn disk_dir(&self) -> Option<&Path> {
        self.disk_dir.as_deref()
    }

    /// Inserts a photo payload, sniffing `Content-Type` when `content_type` is `None` or empty.
    pub fn insert_photo(&self, id: i64, content_type: Option<&str>, bytes: impl Into<Vec<u8>>) {
        self.insert(CachedMediaKey::Photo(id), content_type, bytes);
    }

    /// Inserts a document (video/audio/inline image/file) payload, sniffing `Content-Type`
    /// when `content_type` is `None` or empty.
    pub fn insert_document(&self, id: i64, content_type: Option<&str>, bytes: impl Into<Vec<u8>>) {
        self.insert(CachedMediaKey::Document(id), content_type, bytes);
    }

    /// Inserts a geo-map tile payload, sniffing `Content-Type` when `content_type` is `None`
    /// or empty.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_map_tile(
        &self,
        lat: f64,
        lon: f64,
        zoom: i32,
        width: i32,
        height: i32,
        content_type: Option<&str>,
        bytes: impl Into<Vec<u8>>,
    ) {
        let key = CachedMediaKey::MapTile(MapTileKey::new(lat, lon, zoom, width, height));
        self.insert(key, content_type, bytes);
    }

    /// Inserts a media blob under `key`, persisting to disk if a disk directory is configured.
    pub fn insert(
        &self,
        key: CachedMediaKey,
        content_type: Option<&str>,
        bytes: impl Into<Vec<u8>>,
    ) {
        let bytes = bytes.into();
        let resolved_ct = content_type
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| sniff_content_type(&bytes).to_string());
        let blob = MediaBlob {
            content_type: resolved_ct,
            bytes,
        };
        if let Some(ref dir) = self.disk_dir {
            let _ = write_disk_entry(dir, key, &blob);
        }
        if let Ok(mut guard) = self.inner.lock() {
            guard.insert_ram(key, blob);
        }
    }

    /// Retrieves a cached blob by [`CachedMediaKey`], promoting it to MRU and falling back
    /// to disk storage if evicted from RAM.
    #[must_use]
    pub fn get(&self, key: &CachedMediaKey) -> Option<MediaBlob> {
        if let Ok(mut guard) = self.inner.lock()
            && let Some(blob) = guard.get_ram(key)
        {
            return Some(blob);
        }

        if let Some(ref dir) = self.disk_dir
            && let Some(blob) = read_disk_entry(dir, *key)
        {
            if let Ok(mut guard) = self.inner.lock() {
                guard.hits += 1;
                guard.disk_hits += 1;
                guard.insert_ram(*key, blob.clone());
            }
            return Some(blob);
        }

        if let Ok(mut guard) = self.inner.lock() {
            guard.misses += 1;
        }
        None
    }

    /// Looks up a cached photo by ID.
    #[must_use]
    pub fn get_photo(&self, id: i64) -> Option<MediaBlob> {
        self.get(&CachedMediaKey::Photo(id))
    }

    /// Looks up a cached document by ID.
    #[must_use]
    pub fn get_document(&self, id: i64) -> Option<MediaBlob> {
        self.get(&CachedMediaKey::Document(id))
    }

    /// Looks up a cached geo-map tile.
    #[must_use]
    pub fn get_map_tile(
        &self,
        lat: f64,
        lon: f64,
        zoom: i32,
        width: i32,
        height: i32,
    ) -> Option<MediaBlob> {
        let key = CachedMediaKey::MapTile(MapTileKey::new(lat, lon, zoom, width, height));
        self.get(&key)
    }

    /// Returns `true` if `key` is currently resident in the in-memory LRU cache.
    #[must_use]
    pub fn contains_in_memory(&self, key: &CachedMediaKey) -> bool {
        self.inner
            .lock()
            .map(|g| g.entries.contains_key(key))
            .unwrap_or(false)
    }

    /// Removes `key` from both RAM and disk storage.
    pub fn remove(&self, key: &CachedMediaKey) -> bool {
        let ram_removed = self
            .inner
            .lock()
            .map(|mut g| g.remove_ram(key))
            .unwrap_or(false);
        let disk_removed = self
            .disk_dir
            .as_ref()
            .is_some_and(|dir| fs::remove_file(dir.join(key.disk_file_name())).is_ok());
        ram_removed || disk_removed
    }

    /// Releases all in-memory payloads while preserving disk-backed cache files
    /// (`CachedPagePhotoRuntime::releaseHeavyData` / `CachedPageDocumentRuntime::releaseHeavyData`).
    pub fn release_heavy_data(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.clear_ram();
        }
    }

    /// Clears all in-memory entries and removes cached `.bin` files from the disk directory.
    pub fn clear(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.clear_ram();
        }
        if let Some(ref dir) = self.disk_dir
            && let Ok(read_dir) = fs::read_dir(dir)
        {
            for entry in read_dir.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("bin") {
                    let _ = fs::remove_file(path);
                }
            }
        }
    }

    /// Returns a snapshot of cache occupancy and hit/miss/eviction counters.
    #[must_use]
    pub fn stats(&self) -> CachedMediaStats {
        self.inner
            .lock()
            .map(|g| CachedMediaStats {
                entry_count: g.entries.len(),
                total_bytes: g.total_bytes,
                hits: g.hits,
                misses: g.misses,
                evictions: g.evictions,
                disk_hits: g.disk_hits,
            })
            .unwrap_or_default()
    }
}

impl MediaProvider for IvCachedMedia {
    fn photo(&self, id: i64) -> Option<MediaBlob> {
        self.get_photo(id)
    }

    fn document(&self, id: i64) -> Option<MediaBlob> {
        self.get_document(id)
    }

    fn map_tile(
        &self,
        lat: f64,
        lon: f64,
        zoom: i32,
        width: i32,
        height: i32,
    ) -> Option<MediaBlob> {
        self.get_map_tile(lat, lon, zoom, width, height)
    }
}

fn write_disk_entry(dir: &Path, key: CachedMediaKey, blob: &MediaBlob) -> io::Result<()> {
    let ct_bytes = blob.content_type.as_bytes();
    let ct_len = u16::try_from(ct_bytes.len()).unwrap_or(u16::MAX);
    let mut buf = Vec::with_capacity(DISK_MAGIC.len() + 2 + usize::from(ct_len) + blob.bytes.len());
    buf.extend_from_slice(DISK_MAGIC);
    buf.extend_from_slice(&ct_len.to_le_bytes());
    buf.extend_from_slice(&ct_bytes[..usize::from(ct_len)]);
    buf.extend_from_slice(&blob.bytes);
    let path = dir.join(key.disk_file_name());
    fs::write(path, buf)
}

fn read_disk_entry(dir: &Path, key: CachedMediaKey) -> Option<MediaBlob> {
    let path = dir.join(key.disk_file_name());
    let raw = fs::read(path).ok()?;
    if raw.len() >= DISK_MAGIC.len() + 2 && &raw[..DISK_MAGIC.len()] == DISK_MAGIC {
        let ct_len = usize::from(u16::from_le_bytes([
            raw[DISK_MAGIC.len()],
            raw[DISK_MAGIC.len() + 1],
        ]));
        let payload_start = DISK_MAGIC.len() + 2 + ct_len;
        if payload_start <= raw.len() {
            let ct = std::str::from_utf8(&raw[DISK_MAGIC.len() + 2..payload_start])
                .ok()
                .filter(|s| !s.is_empty());
            let bytes = raw[payload_start..].to_vec();
            let content_type = ct
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| sniff_content_type(&bytes).to_string());
            return Some(MediaBlob {
                content_type,
                bytes,
            });
        }
    }
    let content_type = sniff_content_type(&raw).to_string();
    Some(MediaBlob {
        content_type,
        bytes: raw,
    })
}

/// Serializes native IV channel context (`SerializeNativeIvChannelContext` in `iv_cached_media.cpp`).
#[must_use]
pub fn serialize_native_iv_channel_context(channel_id: u64, username: &str) -> String {
    if username.is_empty() {
        channel_id.to_string()
    } else {
        format!("{channel_id}\n{username}")
    }
}

/// Resolves display title text for a cached page document (`CachedPageDocumentTitleText`).
#[must_use]
pub fn cached_page_document_title_text(title: &str, file_name: &str, audio: bool) -> String {
    if audio && !title.is_empty() {
        title.to_string()
    } else if !file_name.is_empty() {
        file_name.to_string()
    } else if audio {
        "Audio file".to_string()
    } else {
        "File".to_string()
    }
}

/// Resolves display subtitle text for a cached page document (`CachedPageDocumentSubtitleText`).
#[must_use]
pub fn cached_page_document_subtitle_text(
    title: &str,
    performer: &str,
    file_name: &str,
    audio: bool,
) -> String {
    if !audio {
        return String::new();
    }
    if !performer.is_empty() {
        return performer.to_string();
    }
    let resolved_title = cached_page_document_title_text(title, file_name, audio);
    if !file_name.is_empty() && file_name != resolved_title {
        file_name.to_string()
    } else {
        String::new()
    }
}

/// Formats clipboard copy text for a cached page document (`CachedPageDocumentCopyText`).
#[must_use]
pub fn cached_page_document_copy_text(
    title: &str,
    performer: &str,
    file_name: &str,
    audio: bool,
) -> String {
    let resolved_title = cached_page_document_title_text(title, file_name, audio);
    let subtitle = cached_page_document_subtitle_text(title, performer, file_name, audio);
    if subtitle.is_empty() {
        resolved_title
    } else {
        format!("{resolved_title}\n{subtitle}")
    }
}
