//! Raw FFI declarations for `csrc/tg_ffmpeg_shim.c`.
//!
//! Bindgen-free thin binding compiled via `cc` against `FFMPEG_DIR/include`.

use std::ffi::{c_char, c_int, c_void};

/// Opaque handle to `struct TgMediaContext`.
#[repr(C)]
pub struct TgMediaContext {
    _private: [u8; 0],
}

/// Custom AVIO read callback signature.
pub type TgReadCallback = unsafe extern "C" fn(*mut c_void, *mut u8, c_int) -> c_int;

/// Custom AVIO seek callback signature.
pub type TgSeekCallback = unsafe extern "C" fn(*mut c_void, i64, c_int) -> i64;

/// Output pixel format constants (`enum TgOutputPixFmt`).
pub const TG_OUT_RGBA: c_int = 0;
pub const TG_OUT_BGRA: c_int = 1;

/// Frame pixel format categories (`enum TgFramePixFmtKind`).
pub const TG_PIX_OTHER: c_int = 0;
pub const TG_PIX_YUV420P: c_int = 1;
pub const TG_PIX_NV12: c_int = 2;
pub const TG_PIX_YUVA420P: c_int = 3;
pub const TG_PIX_BGRA: c_int = 4;
pub const TG_PIX_RGBA: c_int = 5;

/// Hardware acceleration kinds (`enum TgHwAccelKind`).
pub const TG_HW_NONE: c_int = 0;
pub const TG_HW_D3D11VA: c_int = 1;
pub const TG_HW_DXVA2: c_int = 2;
pub const TG_HW_CUDA: c_int = 3;
pub const TG_HW_VIDEOTOOLBOX: c_int = 4;
pub const TG_HW_VAAPI: c_int = 5;
pub const TG_HW_VDPAU: c_int = 6;

/// Standard `whence` values passed to `TgSeekCallback`.
pub const SEEK_SET: c_int = 0;
pub const SEEK_CUR: c_int = 1;
pub const SEEK_END: c_int = 2;
/// FFmpeg `AVSEEK_SIZE` flag (`0x10000`).
pub const AVSEEK_SIZE: c_int = 0x10000;
/// FFmpeg `AVSEEK_FORCE` flag (`0x20000`).
pub const AVSEEK_FORCE: c_int = 0x20000;

/// Options for opening custom-IO media (`struct TgOpenOptions`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TgOpenOptions {
    pub seekable: c_int,
    pub ignore_editlist: c_int,
    pub hw_allowed: c_int,
    pub open_video: c_int,
    pub open_audio: c_int,
    pub video_max_area: i64,
    pub thread_count: c_int,
}

/// Discovered stream metadata (`struct TgStreamInfo`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TgStreamInfo {
    pub has_video: c_int,
    pub video_stream_index: c_int,
    pub video_width: c_int,
    pub video_height: c_int,
    pub video_timebase_num: c_int,
    pub video_timebase_den: c_int,
    pub video_duration_pts: i64,
    pub video_sar_num: c_int,
    pub video_sar_den: c_int,
    pub video_fps: f64,
    pub video_rotation: c_int,
    pub video_codec_id: c_int,
    pub video_is_h264: c_int,
    pub video_is_vp9: c_int,
    pub video_is_webm_vp9: c_int,
    pub video_codec_name: [c_char; 64],

    pub has_audio: c_int,
    pub audio_stream_index: c_int,
    pub audio_sample_rate: c_int,
    pub audio_channels: c_int,
    pub audio_timebase_num: c_int,
    pub audio_timebase_den: c_int,
    pub audio_duration_pts: i64,
    pub audio_codec_id: c_int,
    pub audio_is_opus: c_int,
    pub audio_codec_name: [c_char; 64],

    pub format_duration_us: i64,
    pub format_name: [c_char; 64],
}

impl Default for TgStreamInfo {
    fn default() -> Self {
        Self {
            has_video: 0,
            video_stream_index: -1,
            video_width: 0,
            video_height: 0,
            video_timebase_num: 0,
            video_timebase_den: 1,
            video_duration_pts: i64::MIN,
            video_sar_num: 1,
            video_sar_den: 1,
            video_fps: 0.0,
            video_rotation: 0,
            video_codec_id: 0,
            video_is_h264: 0,
            video_is_vp9: 0,
            video_is_webm_vp9: 0,
            video_codec_name: [0; 64],
            has_audio: 0,
            audio_stream_index: -1,
            audio_sample_rate: 0,
            audio_channels: 0,
            audio_timebase_num: 0,
            audio_timebase_den: 1,
            audio_duration_pts: i64::MIN,
            audio_codec_id: 0,
            audio_is_opus: 0,
            audio_codec_name: [0; 64],
            format_duration_us: i64::MIN,
            format_name: [0; 64],
        }
    }
}

/// Decoded video frame metadata (`struct TgVideoFrameMeta`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TgVideoFrameMeta {
    pub width: c_int,
    pub height: c_int,
    pub pts: i64,
    pub best_effort_timestamp: i64,
    pub pkt_dts: i64,
    pub duration: i64,
    pub pix_fmt: c_int,
    pub pix_fmt_kind: c_int,
    pub has_alpha: c_int,
    pub hw_accel_used: c_int,
    pub y_stride: c_int,
    pub u_stride: c_int,
    pub v_stride: c_int,
    pub chroma_width: c_int,
    pub chroma_height: c_int,
}

/// Decoded audio frame metadata (`struct TgAudioFrameMeta`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TgAudioFrameMeta {
    pub sample_rate: c_int,
    pub channels: c_int,
    pub nb_samples: c_int,
    pub pts: i64,
    pub best_effort_timestamp: i64,
    pub pkt_dts: i64,
    pub duration: i64,
}

/// Video export / transcode parameters (`struct TgVideoEncodeConfig`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TgVideoEncodeConfig {
    pub from_ms: i64,
    pub till_ms: i64,
    pub target_width: c_int,
    pub target_height: c_int,
    pub crop_x: c_int,
    pub crop_y: c_int,
    pub crop_w: c_int,
    pub crop_h: c_int,
    pub rotation: c_int,
    pub flipped: c_int,
    pub remove_audio: c_int,
    pub audio_volume: f32,
    pub fps_limit: f64,
    pub video_bitrate: i64,
}

/// Video export / transcode result (`struct TgVideoEncodeResult`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TgVideoEncodeResult {
    pub output_width: c_int,
    pub output_height: c_int,
    pub duration_ms: i64,
    pub has_audio: c_int,
    pub file_size: i64,
}

unsafe extern "C" {
    pub fn tg_averror_eof() -> c_int;
    pub fn tg_averror_eagain() -> c_int;
    pub fn tg_averror_invaliddata() -> c_int;
    pub fn tg_averror_external() -> c_int;
    pub fn tg_av_nopts_value() -> i64;
    pub fn tg_av_error_string(errnum: c_int, buf: *mut c_char, buf_size: usize);

    pub fn tg_media_open_custom_io(
        opaque: *mut c_void,
        read_cb: TgReadCallback,
        seek_cb: Option<TgSeekCallback>,
        options: *const TgOpenOptions,
        out_info: *mut TgStreamInfo,
        out_error: *mut c_int,
    ) -> *mut TgMediaContext;

    pub fn tg_media_close(ctx: *mut TgMediaContext);

    pub fn tg_media_seek_video(ctx: *mut TgMediaContext, pts: i64, backward: c_int) -> c_int;
    pub fn tg_media_seek_audio(ctx: *mut TgMediaContext, pts: i64, backward: c_int) -> c_int;
    pub fn tg_media_seek_to_start(ctx: *mut TgMediaContext) -> c_int;
    pub fn tg_media_flush_codecs(ctx: *mut TgMediaContext);

    pub fn tg_media_decode_next(
        ctx: *mut TgMediaContext,
        want_video: c_int,
        want_audio: c_int,
        out_video_meta: *mut TgVideoFrameMeta,
        out_audio_meta: *mut TgAudioFrameMeta,
    ) -> c_int;

    pub fn tg_media_convert_video_frame(
        ctx: *mut TgMediaContext,
        dst_width: c_int,
        dst_height: c_int,
        output_pix_fmt: c_int,
        dst_buf: *mut u8,
        dst_stride: c_int,
    ) -> c_int;

    pub fn tg_media_extract_yuv(
        ctx: *mut TgMediaContext,
        y_buf: *mut u8,
        y_len: usize,
        u_buf: *mut u8,
        u_len: usize,
        v_buf: *mut u8,
        v_len: usize,
    ) -> c_int;

    pub fn tg_media_audio_out_frames_bound(
        ctx: *mut TgMediaContext,
        dst_sample_rate: c_int,
    ) -> c_int;

    pub fn tg_media_convert_audio_f32(
        ctx: *mut TgMediaContext,
        dst_sample_rate: c_int,
        dst_channels: c_int,
        dst_buf: *mut f32,
        max_out_frames: c_int,
    ) -> c_int;

    pub fn tg_ffmpeg_versions(
        avformat_ver: *mut u32,
        avcodec_ver: *mut u32,
        avutil_ver: *mut u32,
        swscale_ver: *mut u32,
        swresample_ver: *mut u32,
    );

    pub fn tg_video_transcode_file(
        input_path: *const c_char,
        output_path: *const c_char,
        config: *const TgVideoEncodeConfig,
        out_result: *mut TgVideoEncodeResult,
        err_buf: *mut c_char,
        err_buf_size: usize,
    ) -> c_int;

    pub fn tg_video_generate_test_clip(
        output_path: *const c_char,
        width: c_int,
        height: c_int,
        fps: c_int,
        duration_ms: c_int,
        with_audio: c_int,
        err_buf: *mut c_char,
        err_buf_size: usize,
    ) -> c_int;
}
