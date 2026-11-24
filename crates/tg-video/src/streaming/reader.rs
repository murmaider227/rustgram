//! Sliced partial file reader with priority prefetching and LRU slice cache.
//!
//! Port of `Telegram/SourceFiles/media/streaming/media_streaming_reader.cpp`.
//!
//! Files are fetched in 128 KB parts (`PART_SIZE`) and grouped into slices
//! (default 1 MB = 8 parts; configurable up to tdesktop's 8 MB = 64 parts).
//! Each read prefetches up to `PRELOAD_PARTS_AHEAD` (8 parts = 1 MB) ahead of
//! reading demand, resets loader priorities on out-of-order seeks, keeps an LRU
//! window of active slices in memory, and serializes evicted slices to an
//! optional [`SliceCache`] using tdesktop's exact contiguous/complex wire format.

use std::collections::{BTreeMap, VecDeque};
use std::io::SeekFrom;
use std::sync::Arc;

use crate::error::{Result, StreamingError};
use crate::ffmpeg::CustomIoSource;
use crate::streaming::cache::{CacheKey, SliceCache, slice_cache_key};
use crate::streaming::loader::{PART_SIZE, PriorityQueue, RangeLoader};

/// Default number of 128 KB parts per slice (8 × 128 KB = 1 MB slices).
pub const DEFAULT_PARTS_IN_SLICE: u32 = 8;

/// Upstream tdesktop commit c2d0ad9 parts per slice (64 × 128 KB = 8 MB slices).
pub const TDESKTOP_PARTS_IN_SLICE: u32 = 64;

/// Default slice size in bytes (1 MB).
pub const DEFAULT_SLICE_SIZE: u64 = (DEFAULT_PARTS_IN_SLICE as u64) * PART_SIZE;

/// Maximum parts accumulated in the container header before flagging `NotStreamable`
/// (`kMaxPartsInHeader` = 64 parts = 8 MB).
pub const MAX_PARTS_IN_HEADER: usize = 64;

/// Files up to `80 * PART_SIZE` (10 MB) are stored entirely in the header slice
/// (`kMaxOnlyInHeader`).
pub const MAX_ONLY_IN_HEADER: u64 = 80 * PART_SIZE;

/// Maximum header parts outside slice 0 for `HeaderMode::Good`
/// (`kPartsOutsideFirstSliceGood`).
pub const PARTS_OUTSIDE_FIRST_SLICE_GOOD: usize = 8;

/// Maximum active slices retained in RAM (`kSlicesInMemory`).
pub const SLICES_IN_MEMORY: usize = 2;

/// Number of 128 KB parts prefetched ahead of reading demand (`kPreloadPartsAhead` = 8 = 1 MB).
pub const PRELOAD_PARTS_AHEAD: u64 = 8;

/// Maximum remote part requests dispatched per fill (`kLoadFromRemoteMax` = 8).
pub const LOAD_FROM_REMOTE_MAX: usize = 8;

/// Maximum single AVIO read request permitted by tdesktop (`kMaxSingleReadAmount` = 8 MiB).
pub const MAX_SINGLE_READ_AMOUNT: usize = 8 * 1024 * 1024;

/// Configuration for [`StreamingReader`] slicing and prefetch behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SliceConfig {
    /// Part size in bytes (default [`PART_SIZE`] = 128 KB).
    pub part_size: u64,
    /// Parts per slice (default [`DEFAULT_PARTS_IN_SLICE`] = 8 for 1 MB slices).
    pub parts_in_slice: u32,
    /// Parts prefetched ahead of each read (default [`PRELOAD_PARTS_AHEAD`] = 8 for 1 MB).
    pub preload_parts_ahead: u64,
    /// Maximum slices kept resident in RAM (default [`SLICES_IN_MEMORY`] = 2).
    pub slices_in_memory: usize,
    /// Maximum parts allowed during header parsing before returning `NotStreamable`.
    pub max_parts_in_header: usize,
}

impl Default for SliceConfig {
    fn default() -> Self {
        Self {
            part_size: PART_SIZE,
            parts_in_slice: DEFAULT_PARTS_IN_SLICE,
            preload_parts_ahead: PRELOAD_PARTS_AHEAD,
            slices_in_memory: SLICES_IN_MEMORY,
            max_parts_in_header: MAX_PARTS_IN_HEADER,
        }
    }
}

impl SliceConfig {
    /// Returns the slice size in bytes (`parts_in_slice * part_size`).
    #[must_use]
    pub const fn slice_size(&self) -> u64 {
        (self.parts_in_slice as u64) * self.part_size
    }

    /// Returns a configuration matching upstream tdesktop's 8 MB slice mode.
    #[must_use]
    pub const fn tdesktop_8mb() -> Self {
        Self {
            part_size: PART_SIZE,
            parts_in_slice: TDESKTOP_PARTS_IN_SLICE,
            preload_parts_ahead: PRELOAD_PARTS_AHEAD,
            slices_in_memory: SLICES_IN_MEMORY,
            max_parts_in_header: MAX_PARTS_IN_HEADER,
        }
    }
}

/// Header caching classification (mirrors `Reader::Slices::HeaderMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderMode {
    /// Header is still being discovered during `avformat_open_input` / `find_stream_info`.
    Unknown,
    /// Header touches too many parts outside slice 0 to bundle with slice 0.
    Small,
    /// Header has <= 8 parts outside slice 0 and is stored bundled with slice 0.
    Good,
    /// Entire file fits inside the header cache entry (`size <= MAX_ONLY_IN_HEADER`).
    Full,
    /// Persistent cache is disabled (e.g. local files).
    NoCache,
}

/// Serialized slice ready to be persisted to [`SliceCache`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerializedSlice {
    /// `0` for header entry, `1..=N` for data slices.
    pub number: u32,
    /// Serialized bytes (contiguous or complex format).
    pub data: Vec<u8>,
}

/// Map from relative offset inside a slice (or absolute offset for header) to part bytes.
pub type PartsMap = BTreeMap<u32, Vec<u8>>;

/// Serializes a sparse map of `(offset, bytes)` pairs using tdesktop's exact
/// `SerializeComplexPartsMap` little-endian binary layout.
#[must_use]
pub fn serialize_complex_parts_map(parts: &PartsMap) -> Vec<u8> {
    let mut out = Vec::new();
    let count = parts.len() as u32;
    out.extend_from_slice(&count.to_le_bytes());
    for (&offset, data) in parts {
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
    }
    out
}

/// Returns `true` if a serialized byte length matches contiguous serialization
/// (`IsContiguousSerialization` in `media_streaming_reader.cpp`).
#[must_use]
pub const fn is_contiguous_serialization(
    serialized_size: usize,
    max_slice_size: usize,
    part_size: u64,
) -> bool {
    (serialized_size as u64).is_multiple_of(part_size) || serialized_size == max_slice_size
}

/// Parses a complex serialized parts map, returning `(parts, remaining_slice)`.
#[must_use]
pub fn parse_complex_cached_map(
    mut data: &[u8],
    max_size: usize,
    part_size: u64,
) -> (PartsMap, &[u8]) {
    let mut result = PartsMap::new();
    let take_u32 = |buf: &mut &[u8]| -> Option<u32> {
        if buf.len() < 4 {
            return None;
        }
        let val = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        *buf = &buf[4..];
        Some(val)
    };

    let Some(count) = take_u32(&mut data) else {
        return (result, &[]);
    };
    let max_count = (MAX_ONLY_IN_HEADER / part_size) as u32;
    if count == 0 || count > max_count {
        return (result, data);
    }
    for _ in 0..count {
        let offset = take_u32(&mut data).unwrap_or(0) as usize;
        let size = take_u32(&mut data).unwrap_or(0) as usize;
        if size == 0
            || offset >= max_size
            || size > max_size
            || offset.saturating_add(size) > max_size
            || data.len() < size
        {
            return (result, &[]);
        }
        let part_bytes = data[..size].to_vec();
        data = &data[size..];
        result.entry(offset as u32).or_insert(part_bytes);
    }
    (result, data)
}

/// Parses either a contiguous or complex cached slice buffer (`ParseCachedMap`).
#[must_use]
pub fn parse_cached_map(data: &[u8], max_size: usize, part_size: u64) -> (PartsMap, &[u8]) {
    let size = data.len();
    if is_contiguous_serialization(size, max_size, part_size) {
        let mut result = PartsMap::new();
        if size > max_size {
            return (result, &[]);
        }
        let step = part_size as usize;
        let mut offset = 0usize;
        while offset < size {
            let len = step.min(size - offset);
            result.insert(offset as u32, data[offset..offset + len].to_vec());
            offset += step;
        }
        return (result, &[]);
    }
    parse_complex_cached_map(data, max_size, part_size)
}

#[derive(Debug, Default)]
struct SliceSlot {
    parts: PartsMap,
    loaded_from_cache: bool,
    changed_since_cache: bool,
    full_in_cache: bool,
}

impl SliceSlot {
    fn add_part(&mut self, rel_offset: u32, bytes: Vec<u8>) {
        if self.parts.contains_key(&rel_offset) {
            return;
        }
        self.parts.insert(rel_offset, bytes);
        if self.loaded_from_cache {
            self.changed_since_cache = true;
        }
    }

    fn unload(&mut self) {
        let full = self.full_in_cache;
        self.parts.clear();
        self.loaded_from_cache = false;
        self.changed_since_cache = false;
        self.full_in_cache = full;
    }

    fn find_not_loaded_start(&self, start_offset: u32) -> u32 {
        let mut cursor = start_offset;
        for (&part_start, bytes) in self
            .parts
            .range(..=start_offset)
            .rev()
            .take(1)
            .chain(self.parts.range(start_offset..))
        {
            let part_end = part_start.saturating_add(bytes.len() as u32);
            if part_start <= cursor && part_end >= cursor {
                cursor = part_end;
            } else if part_start > cursor {
                break;
            }
        }
        cursor
    }

    fn missing_offsets(&self, from: u32, till: u32, part_size: u32, max_count: usize) -> Vec<u32> {
        let mut missing = Vec::new();
        let mut off = from;
        while off < till && missing.len() < max_count {
            if !self.parts.contains_key(&off) {
                missing.push(off);
            }
            match off.checked_add(part_size) {
                Some(next) => off = next,
                None => break,
            }
        }
        missing
    }

    fn copy_range(&self, from: u32, till: u32, dst: &mut [u8]) -> bool {
        let mut filled = from;
        let mut dst_pos = 0usize;
        while filled < till {
            // Find the part containing `filled`.
            let Some((&part_start, bytes)) = self.parts.range(..=filled).next_back() else {
                return false;
            };
            let part_end = part_start.saturating_add(bytes.len() as u32);
            if part_end <= filled {
                return false;
            }
            let copy_till = part_end.min(till);
            let local_from = (filled - part_start) as usize;
            let count = (copy_till - filled) as usize;
            dst[dst_pos..dst_pos + count].copy_from_slice(&bytes[local_from..local_from + count]);
            dst_pos += count;
            filled += count as u32;
        }
        true
    }
}

/// Statistics emitted by [`StreamingReader`] for diagnostics and testing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReaderStats {
    /// Total parts fetched from the underlying [`RangeLoader`].
    pub remote_parts_loaded: u64,
    /// Total bytes fetched from the underlying [`RangeLoader`].
    pub remote_bytes_loaded: u64,
    /// Total prefetch hints issued to the [`RangeLoader`].
    pub prefetch_requests: u64,
    /// Total cache hits when loading slices from [`SliceCache`].
    pub cache_slice_hits: u64,
    /// Total slices written back to [`SliceCache`].
    pub cache_slice_writes: u64,
    /// Total priority resets triggered by out-of-order seeks.
    pub priority_resets: u64,
}

/// Sliced streaming reader porting `Media::Streaming::Reader`
/// (`media_streaming_reader.cpp`).
///
/// Implements [`CustomIoSource`] so it can be attached directly to [`crate::ffmpeg::FfmpegMedia`].
pub struct StreamingReader {
    loader: Arc<dyn RangeLoader>,
    cache: Option<Arc<dyn SliceCache>>,
    base_key: CacheKey,
    config: SliceConfig,
    size: u64,
    header_mode: HeaderMode,
    header: SliceSlot,
    slices: Vec<SliceSlot>,
    used_slices: VecDeque<usize>,
    loading_offsets: PriorityQueue,
    full_in_cache: bool,
    streaming_error: Option<StreamingError>,
    read_pos: u64,
    stats: ReaderStats,
}

impl StreamingReader {
    /// Creates a new `StreamingReader` with default slicing (`128 KB` parts, `1 MB` slices).
    pub fn new(loader: Arc<dyn RangeLoader>, cache: Option<Arc<dyn SliceCache>>) -> Result<Self> {
        Self::with_config(loader, cache, SliceConfig::default())
    }

    /// Creates a new `StreamingReader` with custom [`SliceConfig`].
    pub fn with_config(
        loader: Arc<dyn RangeLoader>,
        cache: Option<Arc<dyn SliceCache>>,
        config: SliceConfig,
    ) -> Result<Self> {
        let size = loader.size();
        if size == 0 || config.part_size == 0 || config.parts_in_slice == 0 {
            return Err(StreamingError::OpenFailed.into());
        }
        let base_key = loader.base_cache_key();
        let use_cache = cache.is_some() && base_key.is_valid();
        let slice_size = config.slice_size();
        let full_in_header = size <= MAX_ONLY_IN_HEADER;
        let slice_count = if full_in_header {
            0
        } else {
            size.div_ceil(slice_size) as usize
        };

        let mut reader = Self {
            loader,
            cache: if use_cache { cache } else { None },
            base_key,
            config,
            size,
            header_mode: if use_cache {
                HeaderMode::Unknown
            } else {
                HeaderMode::NoCache
            },
            header: SliceSlot::default(),
            slices: (0..slice_count).map(|_| SliceSlot::default()).collect(),
            used_slices: VecDeque::new(),
            loading_offsets: PriorityQueue::new(),
            full_in_cache: false,
            streaming_error: None,
            read_pos: 0,
            stats: ReaderStats::default(),
        };

        if use_cache {
            reader.load_initial_header_from_cache();
        } else {
            reader.header.loaded_from_cache = true;
            for slice in &mut reader.slices {
                slice.loaded_from_cache = true;
            }
        }
        Ok(reader)
    }

    /// Total file size in bytes.
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// Active slice configuration.
    #[must_use]
    pub const fn config(&self) -> SliceConfig {
        self.config
    }

    /// Returns `true` if backed by a remote loader with a valid cache key.
    #[must_use]
    pub const fn is_remote_loader(&self) -> bool {
        self.base_key.is_valid()
    }

    /// Current header caching mode.
    #[must_use]
    pub const fn header_mode(&self) -> HeaderMode {
        self.header_mode
    }

    /// Bytes accumulated in the header during container opening.
    #[must_use]
    pub fn header_size(&self) -> usize {
        self.header.parts.len() * (self.config.part_size as usize)
    }

    /// Returns `true` when all slices of the file are fully cached.
    #[must_use]
    pub const fn full_in_cache(&self) -> bool {
        self.full_in_cache
    }

    /// Returns the last fatal streaming error, if any.
    #[must_use]
    pub const fn streaming_error(&self) -> Option<StreamingError> {
        self.streaming_error
    }

    /// Returns current reader telemetry statistics.
    #[must_use]
    pub const fn stats(&self) -> ReaderStats {
        self.stats
    }

    /// Marks container header discovery complete (`Reader::headerDone`).
    pub fn header_done(&mut self) {
        if self.header_mode != HeaderMode::Unknown {
            return;
        }
        self.header_mode = if self.is_full_in_header() {
            HeaderMode::Full
        } else if self.compute_is_good_header() {
            HeaderMode::Good
        } else {
            HeaderMode::Small
        };
        for slice in &mut self.slices {
            slice.loaded_from_cache = true;
        }
    }

    /// Flushes all modified cached slices to [`SliceCache`] (`Reader::finalizeCache`).
    pub fn finalize_cache(&mut self) {
        if self.cache.is_none()
            || self.header_mode == HeaderMode::Unknown
            || self.header_mode == HeaderMode::NoCache
        {
            return;
        }
        while let Some(serialized) = self.unload_next_dirty_slice() {
            self.put_to_cache(serialized);
        }
        if let Some(cache) = &self.cache {
            cache.sync();
        }
    }

    /// Reads bytes at `[offset, offset + buffer.len())` using cached slices,
    /// fetching missing 128 KB parts through `RangeLoader` and prefetching ahead.
    pub fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<usize> {
        if let Some(err) = self.streaming_error {
            return Err(err.into());
        }
        if offset >= self.size || buffer.is_empty() {
            return Ok(0);
        }
        let max_avail = (self.size - offset).min(buffer.len() as u64) as usize;
        if max_avail > MAX_SINGLE_READ_AMOUNT {
            return Err(StreamingError::InvalidData.into());
        }
        let slice_size = self.config.slice_size() as usize;
        let mut total_read = 0usize;
        while total_read < max_avail {
            let chunk_len = (max_avail - total_read).min(slice_size);
            let cur_off = offset + total_read as u64;
            self.fill_chunk(cur_off, &mut buffer[total_read..total_read + chunk_len])?;
            total_read += chunk_len;
        }
        Ok(total_read)
    }

    fn is_full_in_header(&self) -> bool {
        self.size <= MAX_ONLY_IN_HEADER
    }

    fn compute_is_good_header(&self) -> bool {
        if self.is_full_in_header() {
            return false;
        }
        let slice_size = self.config.slice_size() as u32;
        let outside = self.header.parts.range(slice_size..).count();
        outside <= PARTS_OUTSIDE_FIRST_SLICE_GOOD
    }

    fn max_slice_size(&self, slice_number: u32) -> usize {
        if slice_number == 0 {
            self.size as usize
        } else {
            let idx = (slice_number - 1) as usize;
            let slice_size = self.config.slice_size();
            if idx + 1 < self.slices.len() {
                slice_size as usize
            } else {
                (self.size - (idx as u64) * slice_size) as usize
            }
        }
    }

    fn load_initial_header_from_cache(&mut self) {
        let Some(cache) = self.cache.clone() else {
            return;
        };
        let key0 = slice_cache_key(self.base_key, 0);
        let cached = cache.get(key0);
        if !self.is_full_in_header() {
            let keys: Vec<CacheKey> = (0..self.slices.len())
                .map(|i| slice_cache_key(self.base_key, (i + 1) as u32))
                .collect();
            let sizes = cache.get_sizes(&keys);
            self.process_cached_sizes(&sizes);
        }
        if let Some(bytes) = cached {
            self.stats.cache_slice_hits += 1;
            let max_size = self.max_slice_size(0);
            let (parts, remaining) = parse_cached_map(&bytes, max_size, self.config.part_size);
            self.header.parts = parts;
            self.header.loaded_from_cache = true;
            self.check_slice_full_loaded(0);

            if !self.header.parts.is_empty() {
                if self.is_full_in_header() {
                    self.header_mode = HeaderMode::Full;
                } else {
                    let is_good = self.compute_is_good_header();
                    let slice_size = self.config.slice_size();
                    for (&abs_off, part) in &self.header.parts {
                        let idx = (abs_off as u64 / slice_size) as usize;
                        if idx < self.slices.len() {
                            let rel = (abs_off as u64 - (idx as u64) * slice_size) as u32;
                            self.slices[idx].parts.insert(rel, part.clone());
                        }
                    }
                    if is_good && !remaining.is_empty() && !self.slices.is_empty() {
                        let (included, _) = parse_cached_map(
                            remaining,
                            self.max_slice_size(1),
                            self.config.part_size,
                        );
                        for (rel, part) in included {
                            self.slices[0].parts.entry(rel).or_insert(part);
                        }
                        self.slices[0].loaded_from_cache = true;
                        self.check_slice_full_loaded(1);
                    }
                    self.header_mode = if is_good {
                        HeaderMode::Good
                    } else {
                        HeaderMode::Small
                    };
                }
            }
        } else {
            self.header.loaded_from_cache = true;
        }
    }

    fn process_cached_sizes(&mut self, sizes: &[usize]) {
        if sizes.len() != self.slices.len() {
            return;
        }
        let mut loaded_count = 0usize;
        for (i, &sz) in sizes.iter().enumerate() {
            let expected = self.max_slice_size((i + 1) as u32);
            if self.slices[i].full_in_cache || sz == expected {
                self.slices[i].full_in_cache = true;
                loaded_count += 1;
            }
        }
        self.full_in_cache = !self.slices.is_empty() && loaded_count == self.slices.len();
    }

    fn check_slice_full_loaded(&mut self, slice_number: u32) {
        if slice_number == 0 && !self.is_full_in_header() {
            return;
        }
        let part_size = self.config.part_size;
        let parts_count = if slice_number == 0 {
            self.size.div_ceil(part_size) as usize
        } else {
            let idx = (slice_number - 1) as usize;
            if idx + 1 < self.slices.len() {
                self.config.parts_in_slice as usize
            } else {
                let slice_bytes = self.max_slice_size(slice_number) as u64;
                slice_bytes.div_ceil(part_size) as usize
            }
        };
        let slot = if slice_number == 0 {
            &mut self.header
        } else {
            &mut self.slices[(slice_number - 1) as usize]
        };
        let loaded = slot.parts.len() == parts_count;
        if slot.full_in_cache && !loaded {
            slot.full_in_cache = false;
            self.full_in_cache = false;
        } else if !slot.full_in_cache && loaded {
            slot.full_in_cache = true;
            self.full_in_cache = if self.is_full_in_header() {
                self.header.full_in_cache
            } else {
                self.slices.iter().all(|s| s.full_in_cache)
            };
        }
    }

    fn ensure_slice_loaded_from_cache(&mut self, slice_idx: usize) {
        if self.header_mode == HeaderMode::NoCache
            || self.header_mode == HeaderMode::Unknown
            || self.slices[slice_idx].loaded_from_cache
        {
            return;
        }
        let Some(cache) = self.cache.clone() else {
            self.slices[slice_idx].loaded_from_cache = true;
            return;
        };
        let slice_number = (slice_idx + 1) as u32;
        if slice_number == 1 && self.header_mode == HeaderMode::Good {
            let key0 = slice_cache_key(self.base_key, 0);
            if let Some(bytes) = cache.get(key0) {
                self.stats.cache_slice_hits += 1;
                let (hdr_parts, remaining) =
                    parse_cached_map(&bytes, self.max_slice_size(0), self.config.part_size);
                for (abs_off, part) in hdr_parts {
                    if (abs_off as u64) < self.config.slice_size() {
                        self.slices[0].parts.entry(abs_off).or_insert(part);
                    }
                }
                if !remaining.is_empty() {
                    let (inc, _) =
                        parse_cached_map(remaining, self.max_slice_size(1), self.config.part_size);
                    for (rel, part) in inc {
                        self.slices[0].parts.entry(rel).or_insert(part);
                    }
                }
            }
            self.slices[0].loaded_from_cache = true;
            self.check_slice_full_loaded(1);
            return;
        }

        let key = slice_cache_key(self.base_key, slice_number);
        if let Some(bytes) = cache.get(key) {
            self.stats.cache_slice_hits += 1;
            let (parts, _) = parse_cached_map(
                &bytes,
                self.max_slice_size(slice_number),
                self.config.part_size,
            );
            for (rel, part) in parts {
                self.slices[slice_idx].parts.entry(rel).or_insert(part);
            }
        }
        self.slices[slice_idx].loaded_from_cache = true;
        self.check_slice_full_loaded(slice_number);
    }

    fn header_wont_be_filled(&self) -> bool {
        self.header_mode == HeaderMode::Unknown
            && self.header.parts.len() >= self.config.max_parts_in_header
    }

    fn check_priority_and_fetch(&mut self, abs_offset: u64) -> Result<()> {
        if self.loading_offsets.front().unwrap_or(abs_offset) != abs_offset {
            self.loading_offsets.reset_priorities();
            self.loader.reset_priorities();
            self.stats.priority_resets += 1;
        }
        self.loading_offsets.add(abs_offset);
        let bytes = self
            .loader
            .load_range(abs_offset, self.config.part_size as usize)
            .map_err(|_| {
                self.streaming_error = Some(StreamingError::LoadFailed);
                StreamingError::LoadFailed
            })?;
        let expected_len = (self.size - abs_offset).min(self.config.part_size) as usize;
        if bytes.len() != expected_len {
            self.streaming_error = Some(StreamingError::LoadFailed);
            return Err(StreamingError::LoadFailed.into());
        }
        self.loading_offsets.remove(abs_offset);
        self.stats.remote_parts_loaded += 1;
        self.stats.remote_bytes_loaded += bytes.len() as u64;
        self.insert_part(abs_offset, bytes);
        Ok(())
    }

    fn insert_part(&mut self, abs_offset: u64, bytes: Vec<u8>) {
        if self.is_full_in_header() {
            self.header.add_part(abs_offset as u32, bytes);
            self.check_slice_full_loaded(0);
            return;
        }
        let slice_size = self.config.slice_size();
        let slice_idx = (abs_offset / slice_size) as usize;
        let rel = (abs_offset - (slice_idx as u64) * slice_size) as u32;
        self.slices[slice_idx].add_part(rel, bytes);
        self.check_slice_full_loaded((slice_idx + 1) as u32);
    }

    fn issue_prefetch_hints(&mut self, start_abs: u64) {
        let part_size = self.config.part_size;
        let slice_size = self.config.slice_size();
        let mut issued = 0usize;
        let mut cursor = start_abs;
        let limit = start_abs
            .saturating_add(self.config.preload_parts_ahead * part_size)
            .min(self.size);
        while cursor < limit && issued < LOAD_FROM_REMOTE_MAX {
            let already_loaded = if self.is_full_in_header() {
                self.header.parts.contains_key(&(cursor as u32))
            } else {
                let idx = (cursor / slice_size) as usize;
                let rel = (cursor - (idx as u64) * slice_size) as u32;
                idx < self.slices.len() && self.slices[idx].parts.contains_key(&rel)
            };
            if !already_loaded && self.loading_offsets.add(cursor) {
                let len = (self.size - cursor).min(part_size) as usize;
                self.loader.prefetch(cursor, len);
                self.stats.prefetch_requests += 1;
                issued += 1;
            }
            cursor = cursor.saturating_add(part_size);
        }
    }

    fn fill_chunk(&mut self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        let till = offset + buffer.len() as u64;
        let part_size = self.config.part_size;

        if self.is_full_in_header() {
            let from_part = (offset / part_size) * part_size;
            let till_part = till.div_ceil(part_size) * part_size;
            let missing = self.header.missing_offsets(
                from_part as u32,
                till_part.min(self.size) as u32,
                part_size as u32,
                LOAD_FROM_REMOTE_MAX,
            );
            for off in missing {
                self.check_priority_and_fetch(u64::from(off))?;
            }
            if !self.header.copy_range(offset as u32, till as u32, buffer) {
                // Fetch any remaining required parts if chunk exceeded LOAD_FROM_REMOTE_MAX.
                let mut p = from_part;
                while p < till {
                    if !self.header.parts.contains_key(&(p as u32)) {
                        self.check_priority_and_fetch(p)?;
                    }
                    p += part_size;
                }
                if !self.header.copy_range(offset as u32, till as u32, buffer) {
                    self.streaming_error = Some(StreamingError::LoadFailed);
                    return Err(StreamingError::LoadFailed.into());
                }
            }
            self.issue_prefetch_hints(till_part);
            return Ok(());
        }

        let slice_size = self.config.slice_size();
        let from_slice = (offset / slice_size) as usize;
        let till_slice = till.div_ceil(slice_size) as usize;

        for slice_idx in from_slice..till_slice {
            self.ensure_slice_loaded_from_cache(slice_idx);
            let slice_base = (slice_idx as u64) * slice_size;
            let local_from = offset.saturating_sub(slice_base) as u32;
            let local_till = (till - slice_base).min(slice_size) as u32;
            let from_part = (local_from / part_size as u32) * (part_size as u32);
            let till_part = local_till.div_ceil(part_size as u32) * (part_size as u32);

            let mut p = from_part;
            while p < till_part {
                let abs = slice_base + u64::from(p);
                if abs >= self.size {
                    break;
                }
                if !self.slices[slice_idx].parts.contains_key(&p) {
                    if self.header_wont_be_filled() {
                        self.streaming_error = Some(StreamingError::NotStreamable);
                        return Err(StreamingError::NotStreamable.into());
                    }
                    self.check_priority_and_fetch(abs)?;
                }
                p = p.saturating_add(part_size as u32);
            }
        }

        // Copy out of the now-populated slices.
        let mut dst_pos = 0usize;
        for slice_idx in from_slice..till_slice {
            self.mark_slice_used(slice_idx);
            let slice_base = (slice_idx as u64) * slice_size;
            let local_from = offset.saturating_sub(slice_base) as u32;
            let local_till = (till - slice_base).min(slice_size) as u32;
            let copy_len = (local_till - local_from) as usize;

            if !self.slices[slice_idx].copy_range(
                local_from,
                local_till,
                &mut buffer[dst_pos..dst_pos + copy_len],
            ) {
                self.streaming_error = Some(StreamingError::LoadFailed);
                return Err(StreamingError::LoadFailed.into());
            }

            // Record parts read during header discovery.
            if self.header_mode == HeaderMode::Unknown {
                let from_part = (local_from / part_size as u32) * (part_size as u32);
                let mut p = from_part;
                while p < local_till {
                    let abs = (slice_base + u64::from(p)) as u32;
                    if !self.header.parts.contains_key(&abs)
                        && self.header.parts.len() < self.config.max_parts_in_header
                        && let Some(part_bytes) = self.slices[slice_idx].parts.get(&p)
                    {
                        self.header.add_part(abs, part_bytes.clone());
                    }
                    p = p.saturating_add(part_size as u32);
                }
            }
            dst_pos += copy_len;
        }

        // Unload least-recently-used slices beyond `slices_in_memory`.
        if let Some(to_cache) = self.serialize_and_unload_unused() {
            let cancel_idx = to_cache.number.max(1) - 1;
            let from = u64::from(cancel_idx) * slice_size;
            let end = from + slice_size;
            for off in self.loading_offsets.take_in_range(from, end) {
                self.loader.cancel(off);
            }
            self.put_to_cache(to_cache);
        }

        let next_part_after_read = till.div_ceil(part_size) * part_size;
        self.issue_prefetch_hints(next_part_after_read);
        Ok(())
    }

    fn mark_slice_used(&mut self, slice_idx: usize) {
        if let Some(pos) = self.used_slices.iter().position(|&idx| idx == slice_idx) {
            self.used_slices.remove(pos);
        }
        self.used_slices.push_back(slice_idx);
    }

    fn serialize_and_unload_unused(&mut self) -> Option<SerializedSlice> {
        if self.header_mode == HeaderMode::Unknown
            || self.used_slices.len() <= self.config.slices_in_memory
        {
            return None;
        }
        let purge = self.used_slices.pop_front()?;
        if !self.slices[purge].loaded_from_cache {
            return None;
        }
        let no_need_to_save = if self.header_mode == HeaderMode::NoCache {
            true
        } else if !self.slices[purge].changed_since_cache {
            !(self.header_mode == HeaderMode::Good && purge == 0 && self.header.changed_since_cache)
        } else {
            false
        };
        if no_need_to_save {
            self.slices[purge].unload();
            return None;
        }
        self.serialize_and_unload_slice((purge + 1) as u32)
    }

    fn serialize_and_unload_slice(&mut self, slice_number: u32) -> Option<SerializedSlice> {
        if self.header_mode == HeaderMode::Unknown || self.header_mode == HeaderMode::NoCache {
            return None;
        }
        if self.header_mode == HeaderMode::Good && slice_number == 1 {
            return self.serialize_and_unload_slice(0);
        }
        let write_header_and_slice = self.header_mode == HeaderMode::Good && slice_number == 0;
        let max_size = self.max_slice_size(slice_number);

        let data = {
            let slot = if slice_number == 0 {
                &self.header
            } else {
                &self.slices[(slice_number - 1) as usize]
            };
            if slot.parts.is_empty() {
                return None;
            }
            let last_offset = slot.parts.keys().next_back().copied().unwrap_or(0);
            let continuous_till = if write_header_and_slice {
                0
            } else {
                slot.find_not_loaded_start(0)
            };
            if continuous_till > last_offset {
                let mut buf =
                    Vec::with_capacity(slot.parts.len() * (self.config.part_size as usize));
                for part in slot.parts.values() {
                    buf.extend_from_slice(part);
                }
                buf
            } else {
                let mut buf = serialize_complex_parts_map(&slot.parts);
                if write_header_and_slice && !self.slices.is_empty() {
                    let mut first_only = self.slices[0].parts.clone();
                    for k in self.header.parts.keys() {
                        first_only.remove(k);
                    }
                    buf.extend_from_slice(&serialize_complex_parts_map(&first_only));
                }
                while is_contiguous_serialization(buf.len(), max_size, self.config.part_size) {
                    buf.push(0);
                }
                buf
            }
        };

        if slice_number == 0 {
            self.header.changed_since_cache = false;
            if write_header_and_slice && !self.slices.is_empty() {
                self.slices[0].unload();
            }
        } else {
            self.slices[(slice_number - 1) as usize].unload();
        }
        Some(SerializedSlice {
            number: slice_number,
            data,
        })
    }

    fn unload_next_dirty_slice(&mut self) -> Option<SerializedSlice> {
        if self.header.changed_since_cache {
            return self.serialize_and_unload_slice(0);
        }
        for i in 0..self.slices.len() {
            if self.slices[i].changed_since_cache {
                return self.serialize_and_unload_slice((i + 1) as u32);
            }
        }
        None
    }

    fn put_to_cache(&mut self, slice: SerializedSlice) {
        if let Some(cache) = &self.cache {
            cache.put(slice_cache_key(self.base_key, slice.number), slice.data);
            self.stats.cache_slice_writes += 1;
        }
    }
}

impl Drop for StreamingReader {
    fn drop(&mut self) {
        self.finalize_cache();
    }
}

impl CustomIoSource for StreamingReader {
    fn read_bytes(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self
            .read_at(self.read_pos, buf)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        self.read_pos += n as u64;
        Ok(n)
    }

    fn seek_to(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let len = i64::try_from(self.size).unwrap_or(i64::MAX);
        let cur = i64::try_from(self.read_pos).unwrap_or(i64::MAX);
        let next = match pos {
            SeekFrom::Start(off) => i64::try_from(off).unwrap_or(i64::MAX),
            SeekFrom::Current(off) => cur.saturating_add(off),
            SeekFrom::End(off) => len.saturating_add(off),
        };
        if next < 0 || next > len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "streaming reader seek out of bounds",
            ));
        }
        self.read_pos = next as u64;
        Ok(self.read_pos)
    }

    fn total_size(&self) -> Option<u64> {
        Some(self.size)
    }
}
