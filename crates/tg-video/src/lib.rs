//! Telegram Desktop video decoding, streaming reader/player, clip/sticker reader,
//! and thumbnail extraction ported to Rust.
//!
//! Ported from:
//! - `Telegram/SourceFiles/ffmpeg/ffmpeg_utility.cpp` & `ffmpeg_frame_generator.cpp`
//! - `Telegram/SourceFiles/media/streaming/media_streaming_*.cpp`
//! - `Telegram/SourceFiles/media/clip/media_clip_*.cpp`

pub mod clip;
pub mod encode;
pub mod error;
pub mod ffmpeg;
pub mod streaming;
pub mod thumbnail;
pub mod types;

pub use clip::{
    ClipMode, ClipReader, FIND_MOOV_BEFORE, MAX_CLIP_IN_MEMORY, apply_circle_mask_rgba,
    apply_rounded_rect_mask_rgba, check_streaming_support, check_streaming_support_file,
    check_streaming_support_reader, generate_circle_mask,
};
pub use encode::{
    VideoEncodeConfig, VideoEncodeResult, extract_frame_at_ms, extract_timeline_frames,
    ffmpeg_runtime_available, generate_test_clip, transcode_video_file,
};
pub use error::{Result, StreamingError, VideoError};
pub use ffmpeg::{
    CustomIoSource, DecodedMediaFrame, FfmpegMedia, FfmpegOpenOptions, FfmpegVersions,
    FileIoSource, FrameGenerator, GeneratedFrame, HwAccelKind, MediaStreamInfo, MemoryIoSource,
    ffmpeg_versions,
};
pub use streaming::{
    AudioSink, BUFFER_FOR_MS, BinlogSliceCache, CacheKey, DEFAULT_FRAME_QUEUE_CAPACITY,
    DEFAULT_PARTS_IN_SLICE, DEFAULT_SLICE_SIZE, DecodedPacket, Decoder, FfmpegStreamDecoder,
    FramePopOutcome, FrameQueue, HeaderMode, InstanceId, LOAD_FROM_REMOTE_MAX,
    LOAD_IN_ADVANCE_LOCAL_MS, LOAD_IN_ADVANCE_REMOTE_MS, LoadedPart, LocalFileLoader,
    MAX_FILE_SIZE, MAX_ONLY_IN_HEADER, MAX_PARTS_IN_HEADER, MAX_SINGLE_READ_AMOUNT,
    MediaCacheSliceCache, MemoryRangeLoader, MemorySliceCache, PART_SIZE, PRELOAD_PARTS_AHEAD,
    PartsMap, PlayerAudio, PlayerStage, PriorityQueue, RangeLoader, ReaderStats, SLICES_IN_MEMORY,
    SliceCache, SliceConfig, SpeedEstimator, StreamingDocument, StreamingInstance, StreamingPlayer,
    StreamingReader, SyncClock, TDESKTOP_PARTS_IN_SLICE, THREADED_VIDEO_AHEAD, ThreadedDecoder,
    TimePoint, slice_cache_key,
};
pub use thumbnail::{
    GOOD_THUMB_QUALITY, ThumbnailEncoding, ThumbnailOptions, encode_thumbnail, extract_thumbnail,
    extract_thumbnail_from_bytes, flatten_alpha_over_white,
};
pub use types::{
    AudioFrame, AudioInformation, DURATION_MAX, DURATION_UNAVAILABLE, FrameData, FrameFormat,
    FrameRequest, MAX_FRAME_AREA, MAX_FRAME_GENERATOR_AREA, MAX_INLINE_AREA, Nv12Data,
    OutputPixelFormat, PlaybackMode, PlaybackOptions, QualityDescriptor, Rational, Rotation,
    SPED_UP_DEFAULT, SPEED_MAX, SPEED_MIN, Size, SpeedEstimate, StreamInformation, StreamingUpdate,
    TIME_UNKNOWN, TrackState, VideoFrame, VideoInformation, Yuv420Data, equal_speeds,
    valid_frame_size,
};
