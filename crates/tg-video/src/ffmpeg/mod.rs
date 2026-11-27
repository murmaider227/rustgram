//! FFmpeg FFI bindings and safe utility wrappers.
//!
//! Port of `Telegram/SourceFiles/ffmpeg/ffmpeg_utility.cpp` and
//! `Telegram/SourceFiles/ffmpeg/ffmpeg_frame_generator.cpp`.

#[allow(dead_code)]
pub(crate) mod sys;
pub mod utility;

pub use utility::{
    ALIGN_IMAGE_BY, AV_BLOCK_SIZE, CustomIoSource, DecodedMediaFrame, FfmpegMedia,
    FfmpegOpenOptions, FfmpegVersions, FileIoSource, FrameGenerator, GeneratedFrame, HwAccelKind,
    MAX_FRAME_STORAGE_BYTES, MAX_SCALE_BY_ASPECT_RATIO, MediaStreamInfo, MemoryIoSource,
    PIXEL_BYTES_SIZE, aligned_frame_stride, av_error_string, av_nopts_value, correct_by_aspect,
    duration_by_packet, ffmpeg_build_id, ffmpeg_versions, max_pixels_for_area_limit,
    premultiply_rgba_inplace, pts_to_time, pts_to_time_ceil, rotate_pixels_4bpp, time_to_pts,
    unpremultiply_rgba, validate_aspect_ratio,
};
