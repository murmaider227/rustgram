//! Streaming video/audio playback subsystem.
//!
//! Port of `Telegram/SourceFiles/media/streaming/`.

pub mod cache;
pub mod clock;
pub mod decoder;
pub mod document;
pub mod loader;
pub mod player;
pub mod reader;

pub use cache::{
    BinlogSliceCache, CacheKey, MediaCacheSliceCache, MemorySliceCache, SliceCache, slice_cache_key,
};

pub use clock::{
    DEFAULT_FRAME_QUEUE_CAPACITY, FramePopOutcome, FrameQueue, SyncClock, TimePoint,
    world_time_delay,
};
pub use decoder::{DecodedPacket, Decoder, FfmpegStreamDecoder};
pub use document::{
    InstanceId, SWITCH_QUALITY_UP_PRELOADED_THRESHOLD_MS, SWITCH_QUALITY_UP_SPEED_MULTIPLIER,
    StreamingDocument, StreamingInstance,
};
pub use loader::{
    LoadedPart, LocalFileLoader, MAX_FILE_SIZE, MemoryRangeLoader, PART_SIZE, PriorityQueue,
    RangeLoader, SpeedEstimator, validate_local_size,
};
pub use player::{
    BUFFER_FOR_MS, LOAD_IN_ADVANCE_LOCAL_MS, LOAD_IN_ADVANCE_REMOTE_MS, PlayerStage,
    StreamingPlayer,
};
pub use reader::{
    DEFAULT_PARTS_IN_SLICE, DEFAULT_SLICE_SIZE, HeaderMode, LOAD_FROM_REMOTE_MAX,
    MAX_ONLY_IN_HEADER, MAX_PARTS_IN_HEADER, MAX_SINGLE_READ_AMOUNT,
    PARTS_OUTSIDE_FIRST_SLICE_GOOD, PRELOAD_PARTS_AHEAD, PartsMap, ReaderStats, SLICES_IN_MEMORY,
    SerializedSlice, SliceConfig, StreamingReader, TDESKTOP_PARTS_IN_SLICE,
    is_contiguous_serialization, parse_cached_map, parse_complex_cached_map,
    serialize_complex_parts_map,
};
