//! Safe Rust FFmpeg utility helpers and RAII media decoder.
//!
//! Port of `Telegram/SourceFiles/ffmpeg/ffmpeg_utility.cpp` and
//! `Telegram/SourceFiles/ffmpeg/ffmpeg_frame_generator.cpp`.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::error::{Result, StreamingError, VideoError};
use crate::ffmpeg::sys;
use crate::types::{
    AudioFrame, DURATION_MAX, DURATION_UNAVAILABLE, FrameData, MAX_FRAME_AREA,
    MAX_FRAME_GENERATOR_AREA, Nv12Data, OutputPixelFormat, Rational, Rotation, Size, TIME_UNKNOWN,
    VideoFrame, Yuv420Data, valid_frame_size,
};

/// Bytes per RGBA/BGRA pixel (`kPixelBytesSize`).
pub const PIXEL_BYTES_SIZE: usize = 4;

/// Row alignment required by FFmpeg `swscale` SIMD kernels (`kAlignImageBy`).
pub const ALIGN_IMAGE_BY: usize = 64;

/// Custom AVIO read buffer block size (`kAVBlockSize`).
pub const AV_BLOCK_SIZE: usize = 4096;

/// Maximum pixel storage bytes for a single frame (`kMaxFrameStorageBytes`, 64 MiB).
pub const MAX_FRAME_STORAGE_BYTES: usize = 64 * 1024 * 1024;

/// Maximum scale factor allowed for a valid sample aspect ratio (`kMaxScaleByAspectRatio`).
pub const MAX_SCALE_BY_ASPECT_RATIO: i64 = 16;

const MAX_PIXELS_PADDING_RATIO: i64 = 32;
const MAX_PIXELS_FIXED_PADDING: i64 = 64 * 1024;

/// FFmpeg runtime library versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfmpegVersions {
    /// `libavformat` packed version.
    pub avformat: u32,
    /// `libavcodec` packed version.
    pub avcodec: u32,
    /// `libavutil` packed version.
    pub avutil: u32,
    /// `libswscale` packed version.
    pub swscale: u32,
    /// `libswresample` packed version.
    pub swresample: u32,
}

/// Queries the runtime versions of the linked FFmpeg shared libraries.
#[must_use]
pub fn ffmpeg_versions() -> FfmpegVersions {
    let mut v = FfmpegVersions {
        avformat: 0,
        avcodec: 0,
        avutil: 0,
        swscale: 0,
        swresample: 0,
    };
    // SAFETY: All pointers are valid stack references to `u32`.
    unsafe {
        sys::tg_ffmpeg_versions(
            &mut v.avformat,
            &mut v.avcodec,
            &mut v.avutil,
            &mut v.swscale,
            &mut v.swresample,
        );
    }
    v
}

/// Hardware decoding backend selected by FFmpeg.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HwAccelKind {
    /// Software CPU decoding.
    #[default]
    None,
    /// Direct3D 11 Video Acceleration (Windows).
    D3d11va,
    /// DirectX Video Acceleration 2 (Windows).
    Dxva2,
    /// NVIDIA CUDA / NVDEC.
    Cuda,
    /// Apple VideoToolbox (macOS).
    VideoToolbox,
    /// VA-API (Linux).
    Vaapi,
    /// VDPAU (Linux).
    Vdpau,
}

impl HwAccelKind {
    const fn from_raw(raw: c_int) -> Self {
        match raw {
            sys::TG_HW_D3D11VA => Self::D3d11va,
            sys::TG_HW_DXVA2 => Self::Dxva2,
            sys::TG_HW_CUDA => Self::Cuda,
            sys::TG_HW_VIDEOTOOLBOX => Self::VideoToolbox,
            sys::TG_HW_VAAPI => Self::Vaapi,
            sys::TG_HW_VDPAU => Self::Vdpau,
            _ => Self::None,
        }
    }
}

/// Formats an FFmpeg negative error code using `av_strerror`.
#[must_use]
pub fn av_error_string(code: i32) -> String {
    let mut buf = [0 as c_char; 128];
    // SAFETY: `buf` is a valid mutable byte buffer of length 128.
    unsafe {
        sys::tg_av_error_string(code, buf.as_mut_ptr(), buf.len());
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    }
}

/// Returns FFmpeg's `AV_NOPTS_VALUE`.
#[must_use]
pub fn av_nopts_value() -> i64 {
    // SAFETY: Pure C getter returning a compile-time constant.
    unsafe { sys::tg_av_nopts_value() }
}

/// Computes `AVCodecContext::max_pixels` slack for an area limit
/// (mirrors `FFmpeg::MaxPixelsForAreaLimit`).
#[must_use]
pub const fn max_pixels_for_area_limit(area: i64) -> i64 {
    area + area / MAX_PIXELS_PADDING_RATIO + MAX_PIXELS_FIXED_PADDING
}

/// Converts a stream PTS value to milliseconds (mirrors `FFmpeg::PtsToTime`).
#[must_use]
pub fn pts_to_time(pts: i64, time_base: Rational) -> i64 {
    if pts == av_nopts_value() || time_base.den == 0 {
        TIME_UNKNOWN
    } else {
        ((i128::from(pts) * 1000 * i128::from(time_base.num)) / i128::from(time_base.den)) as i64
    }
}

/// Converts a stream PTS value to milliseconds rounding up (mirrors `FFmpeg::PtsToTimeCeil`).
#[must_use]
pub fn pts_to_time_ceil(pts: i64, time_base: Rational) -> i64 {
    if pts == av_nopts_value() || time_base.den == 0 {
        TIME_UNKNOWN
    } else {
        let den = i128::from(time_base.den);
        ((i128::from(pts) * 1000 * i128::from(time_base.num) + den - 1) / den) as i64
    }
}

/// Converts milliseconds to a stream PTS value (mirrors `FFmpeg::TimeToPts`).
#[must_use]
pub fn time_to_pts(time_ms: i64, time_base: Rational) -> i64 {
    if time_ms == TIME_UNKNOWN || time_base.num == 0 {
        av_nopts_value()
    } else {
        ((i128::from(time_ms) * i128::from(time_base.den)) / (1000 * i128::from(time_base.num)))
            as i64
    }
}

/// Computes duration upper bound from a packet's position and duration
/// (mirrors `FFmpeg::DurationByPacket`). Returns `None` on invalid values.
#[must_use]
pub fn duration_by_packet(pts: i64, dts: i64, duration: i64, time_base: Rational) -> Option<i64> {
    let nopts = av_nopts_value();
    let position = pts_to_time(if pts == nopts { dts } else { pts }, time_base);
    let dur = pts_to_time(duration, time_base).max(1);
    let bad = |t: i64| !(0..=DURATION_MAX).contains(&t);
    let total = position.checked_add(dur)?.checked_add(1)?;
    if bad(position) || bad(dur) || bad(total) {
        None
    } else {
        Some(total)
    }
}

/// Validates a sample aspect ratio (mirrors `FFmpeg::ValidateAspectRatio`).
#[must_use]
pub const fn validate_aspect_ratio(aspect: Rational) -> Rational {
    let num = aspect.num as i64;
    let den = aspect.den as i64;
    if num > 0
        && den > 0
        && num <= den * MAX_SCALE_BY_ASPECT_RATIO
        && den <= num * MAX_SCALE_BY_ASPECT_RATIO
    {
        aspect
    } else {
        Rational::NORMAL_ASPECT
    }
}

/// Adjusts a video frame size by a validated sample aspect ratio
/// (mirrors `FFmpeg::CorrectByAspect`).
#[must_use]
pub fn correct_by_aspect(size: Size, aspect: Rational) -> Size {
    let valid = validate_aspect_ratio(aspect);
    let corrected_w = ((f64::from(size.width) * valid.as_f64()) as u32).max(1);
    Size::new(corrected_w, size.height)
}

/// Computes an `ALIGN_IMAGE_BY`-aligned row stride and checks `MAX_FRAME_STORAGE_BYTES`.
#[must_use]
pub fn aligned_frame_stride(width: u32, height: u32, min_bytes_per_line: usize) -> Option<usize> {
    if width == 0 || height == 0 {
        return None;
    }
    let requested = (width as usize)
        .checked_mul(PIXEL_BYTES_SIZE)?
        .max(min_bytes_per_line);
    let rem = requested % ALIGN_IMAGE_BY;
    let stride = if rem == 0 {
        requested
    } else {
        requested.checked_add(ALIGN_IMAGE_BY - rem)?
    };
    let total = stride
        .checked_mul(height as usize)?
        .checked_add(ALIGN_IMAGE_BY)?;
    if stride > MAX_FRAME_STORAGE_BYTES || total > MAX_FRAME_STORAGE_BYTES {
        None
    } else {
        Some(stride)
    }
}

/// Premultiplies straight RGBA8 pixels in place (`PremultiplyInplace`).
pub fn premultiply_rgba_inplace(rgba: &mut [u8]) {
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = u16::from(px[3]);
        if a == 255 {
            continue;
        }
        if a == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
        } else {
            px[0] = ((u16::from(px[0]) * a + 127) / 255) as u8;
            px[1] = ((u16::from(px[1]) * a + 127) / 255) as u8;
            px[2] = ((u16::from(px[2]) * a + 127) / 255) as u8;
        }
    }
}

/// Un-premultiplies premultiplied RGBA8 pixels into straight RGBA8 (`UnPremultiply`).
#[must_use]
pub fn unpremultiply_rgba(src: &[u8]) -> Vec<u8> {
    let mut dst = vec![0u8; src.len()];
    for (s, d) in src
        .as_chunks::<4>()
        .0
        .iter()
        .zip(dst.as_chunks_mut::<4>().0.iter_mut())
    {
        let a = u16::from(s[3]);
        d[3] = s[3];
        if a == 0 {
            d[0] = 0;
            d[1] = 0;
            d[2] = 0;
        } else if a == 255 {
            d[0] = s[0];
            d[1] = s[1];
            d[2] = s[2];
        } else {
            d[0] = ((u16::from(s[0]) * 255 + a / 2) / a).min(255) as u8;
            d[1] = ((u16::from(s[1]) * 255 + a / 2) / a).min(255) as u8;
            d[2] = ((u16::from(s[2]) * 255 + a / 2) / a).min(255) as u8;
        }
    }
    dst
}

/// Rotates a tightly-packed 4-bytes-per-pixel image clockwise by `rotation`.
#[must_use]
pub fn rotate_pixels_4bpp(
    src: &[u8],
    width: u32,
    height: u32,
    rotation: Rotation,
) -> (Vec<u8>, u32, u32) {
    let w = width as usize;
    let h = height as usize;
    let expected = w.saturating_mul(h).saturating_mul(4);
    if rotation == Rotation::Deg0 || src.len() < expected || w == 0 || h == 0 {
        return (src.to_vec(), width, height);
    }
    match rotation {
        Rotation::Deg0 => (src.to_vec(), width, height),
        Rotation::Deg90 => {
            let dst_w = h;
            let dst_h = w;
            let mut dst = vec![0u8; expected];
            for y in 0..h {
                for x in 0..w {
                    let s = (y * w + x) * 4;
                    let dx = h - 1 - y;
                    let dy = x;
                    let d = (dy * dst_w + dx) * 4;
                    dst[d..d + 4].copy_from_slice(&src[s..s + 4]);
                }
            }
            (dst, dst_w as u32, dst_h as u32)
        }
        Rotation::Deg180 => {
            let mut dst = vec![0u8; expected];
            for y in 0..h {
                for x in 0..w {
                    let s = (y * w + x) * 4;
                    let dx = w - 1 - x;
                    let dy = h - 1 - y;
                    let d = (dy * w + dx) * 4;
                    dst[d..d + 4].copy_from_slice(&src[s..s + 4]);
                }
            }
            (dst, width, height)
        }
        Rotation::Deg270 => {
            let dst_w = h;
            let dst_h = w;
            let mut dst = vec![0u8; expected];
            for y in 0..h {
                for x in 0..w {
                    let s = (y * w + x) * 4;
                    let dx = y;
                    let dy = w - 1 - x;
                    let d = (dy * dst_w + dx) * 4;
                    dst[d..d + 4].copy_from_slice(&src[s..s + 4]);
                }
            }
            (dst, dst_w as u32, dst_h as u32)
        }
    }
}

/// Custom I/O data source fed into FFmpeg's `AVIOContext` (`RestrictToCustomIO`).
pub trait CustomIoSource: Send {
    /// Reads up to `buf.len()` bytes. Returning `Ok(0)` signals EOF.
    fn read_bytes(&mut self, buf: &mut [u8]) -> std::io::Result<usize>;

    /// Seeks to `pos` and returns the new byte offset from the start.
    fn seek_to(&mut self, pos: SeekFrom) -> std::io::Result<u64>;

    /// Returns total stream length in bytes, if known.
    fn total_size(&self) -> Option<u64>;
}

/// In-memory byte buffer implementing [`CustomIoSource`].
#[derive(Debug, Clone)]
pub struct MemoryIoSource {
    data: Vec<u8>,
    pos: usize,
}

impl MemoryIoSource {
    /// Wraps an owned byte vector as a custom I/O source.
    #[must_use]
    pub fn new(data: Vec<u8>) -> Self {
        Self { data, pos: 0 }
    }
}

impl CustomIoSource for MemoryIoSource {
    fn read_bytes(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.data.len() {
            return Ok(0);
        }
        let n = buf.len().min(self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }

    fn seek_to(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let len = self.data.len() as i64;
        let next = match pos {
            SeekFrom::Start(off) => i64::try_from(off).unwrap_or(i64::MAX),
            SeekFrom::Current(off) => (self.pos as i64).saturating_add(off),
            SeekFrom::End(off) => len.saturating_add(off),
        };
        if next < 0 || next > len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek out of bounds",
            ));
        }
        self.pos = next as usize;
        Ok(self.pos as u64)
    }

    fn total_size(&self) -> Option<u64> {
        Some(self.data.len() as u64)
    }
}

/// Local file reader implementing [`CustomIoSource`].
#[derive(Debug)]
pub struct FileIoSource {
    file: File,
    size: u64,
}

impl FileIoSource {
    /// Opens a file at `path` for read-only custom I/O decoding.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        Ok(Self { file, size })
    }
}

impl CustomIoSource for FileIoSource {
    fn read_bytes(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buf)
    }

    fn seek_to(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.file.seek(pos)
    }

    fn total_size(&self) -> Option<u64> {
        Some(self.size)
    }
}

struct IoBox {
    source: Box<dyn CustomIoSource>,
}

unsafe extern "C" fn rust_avio_read(opaque: *mut c_void, buf: *mut u8, buf_size: c_int) -> c_int {
    if opaque.is_null() || buf.is_null() || buf_size <= 0 {
        // SAFETY: Pure constant getter.
        return unsafe { sys::tg_averror_external() };
    }
    // SAFETY: `opaque` is a valid `*mut IoBox` owned by `FfmpegMedia` for the lifetime of `AVIOContext`.
    let io_box = unsafe { &mut *(opaque as *mut IoBox) };
    // SAFETY: FFmpeg guarantees `buf` is valid for writes of `buf_size` bytes.
    let slice = unsafe { std::slice::from_raw_parts_mut(buf, buf_size as usize) };
    match io_box.source.read_bytes(slice) {
        Ok(0) => {
            // SAFETY: Pure constant getter.
            unsafe { sys::tg_averror_eof() }
        }
        Ok(n) => n as c_int,
        Err(_) => {
            // SAFETY: Pure constant getter.
            unsafe { sys::tg_averror_external() }
        }
    }
}

unsafe extern "C" fn rust_avio_seek(opaque: *mut c_void, offset: i64, whence: c_int) -> i64 {
    if opaque.is_null() {
        return -1;
    }
    // SAFETY: `opaque` is a valid `*mut IoBox` owned by `FfmpegMedia`.
    let io_box = unsafe { &mut *(opaque as *mut IoBox) };
    if (whence & sys::AVSEEK_SIZE) != 0 {
        return io_box
            .source
            .total_size()
            .and_then(|s| i64::try_from(s).ok())
            .unwrap_or(-1);
    }
    let base_whence = whence & !sys::AVSEEK_FORCE;
    let seek_from = match base_whence {
        sys::SEEK_SET => match u64::try_from(offset) {
            Ok(off) => SeekFrom::Start(off),
            Err(_) => return -1,
        },
        sys::SEEK_CUR => SeekFrom::Current(offset),
        sys::SEEK_END => SeekFrom::End(offset),
        _ => return -1,
    };
    match io_box.source.seek_to(seek_from) {
        Ok(pos) => i64::try_from(pos).unwrap_or(-1),
        Err(_) => -1,
    }
}

/// Configuration for opening an [`FfmpegMedia`] container.
#[derive(Debug, Clone, Copy)]
pub struct FfmpegOpenOptions {
    /// Whether the custom I/O source supports seeking.
    pub seekable: bool,
    /// Pass `ignore_editlist=1` to MP4/MOV demuxer.
    pub ignore_editlist: bool,
    /// Allow hardware decoding (D3D11VA/DXVA2/CUDA on Windows).
    pub hw_allowed: bool,
    /// Open video decoder if a video stream exists.
    pub open_video: bool,
    /// Open audio decoder if an audio stream exists.
    pub open_audio: bool,
    /// Maximum allowed video pixel area (`MAX_FRAME_AREA`).
    pub video_max_area: u64,
    /// FFmpeg decoder thread count (`0` = auto, `-1` = single-threaded).
    pub thread_count: i32,
}

impl Default for FfmpegOpenOptions {
    fn default() -> Self {
        Self {
            seekable: true,
            ignore_editlist: false,
            hw_allowed: false,
            open_video: true,
            open_audio: true,
            video_max_area: MAX_FRAME_AREA,
            thread_count: 0,
        }
    }
}

/// Container and stream metadata reported by [`FfmpegMedia`].
#[derive(Debug, Clone)]
pub struct MediaStreamInfo {
    /// Video stream present.
    pub has_video: bool,
    /// Video stream index (`-1` if none).
    pub video_stream_index: i32,
    /// Raw encoded video width.
    pub video_width: u32,
    /// Raw encoded video height.
    pub video_height: u32,
    /// Video stream time base.
    pub video_time_base: Rational,
    /// Video duration in milliseconds (`DURATION_UNAVAILABLE` if unknown).
    pub video_duration_ms: i64,
    /// Validated sample aspect ratio.
    pub video_aspect: Rational,
    /// Guessed frames per second.
    pub video_fps: f64,
    /// Rotation metadata.
    pub video_rotation: Rotation,
    /// True if video codec is H.264.
    pub video_is_h264: bool,
    /// True if video codec is VP9.
    pub video_is_vp9: bool,
    /// True if container is WebM with VP9 video (unreliable format duration).
    pub video_is_webm_vp9: bool,
    /// Video codec name.
    pub video_codec_name: String,

    /// Audio stream present.
    pub has_audio: bool,
    /// Audio stream index (`-1` if none).
    pub audio_stream_index: i32,
    /// Audio sample rate in Hz.
    pub audio_sample_rate: u32,
    /// Audio channel count.
    pub audio_channels: u16,
    /// Audio stream time base.
    pub audio_time_base: Rational,
    /// Audio duration in milliseconds (`DURATION_UNAVAILABLE` if unknown).
    pub audio_duration_ms: i64,
    /// Audio codec name.
    pub audio_codec_name: String,

    /// Container-level duration in milliseconds (`DURATION_UNAVAILABLE` if unknown).
    pub format_duration_ms: i64,
    /// Demuxer format name.
    pub format_name: String,
}

fn c_buf_to_string(buf: &[c_char]) -> String {
    // SAFETY: `buf` is always NUL-terminated by `copy_string_safe` in `tg_ffmpeg_shim.c`.
    unsafe { CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned() }
}

/// Decoded item returned by [`FfmpegMedia::decode_next`].
#[derive(Debug, Clone)]
pub enum DecodedMediaFrame {
    /// Decoded video frame.
    Video(VideoFrame),
    /// Decoded audio frame.
    Audio(AudioFrame),
}

/// Safe RAII wrapper over FFmpeg demuxer, decoders, `SwsContext`, and `SwrContext`.
pub struct FfmpegMedia {
    raw: *mut sys::TgMediaContext,
    _io_box: Box<IoBox>,
    info: MediaStreamInfo,
    max_area: u64,
    video_frame_index: usize,
    last_hw_accel: HwAccelKind,
}

// SAFETY: `FfmpegMedia` exclusively owns its `TgMediaContext` and `IoBox`; FFmpeg contexts
// are safe to move across threads when accessed with `&mut self` synchronization.
unsafe impl Send for FfmpegMedia {}

impl Drop for FfmpegMedia {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: `self.raw` was created by `tg_media_open_custom_io` and is freed once,
            // before `_io_box` is dropped (Rust drops fields in declaration order).
            unsafe { sys::tg_media_close(self.raw) };
            self.raw = std::ptr::null_mut();
        }
    }
}

impl FfmpegMedia {
    /// Opens a media stream from any [`CustomIoSource`].
    pub fn open(
        source: Box<dyn CustomIoSource>,
        options: FfmpegOpenOptions,
        duration_override_ms: i64,
    ) -> Result<Self> {
        let mut io_box = Box::new(IoBox { source });
        let raw_opts = sys::TgOpenOptions {
            seekable: i32::from(options.seekable),
            ignore_editlist: i32::from(options.ignore_editlist),
            hw_allowed: i32::from(options.hw_allowed),
            open_video: i32::from(options.open_video),
            open_audio: i32::from(options.open_audio),
            video_max_area: i64::try_from(options.video_max_area).unwrap_or(i64::MAX),
            thread_count: options.thread_count,
        };
        let mut raw_info = sys::TgStreamInfo::default();
        let mut raw_err: c_int = 0;

        let opaque = std::ptr::from_mut(io_box.as_mut()).cast::<c_void>();
        // SAFETY: `opaque` remains pinned in `Box<IoBox>` for the full lifetime of `FfmpegMedia`,
        // and `FfmpegMedia::drop` closes `raw` before `io_box` is dropped.
        let raw = unsafe {
            sys::tg_media_open_custom_io(
                opaque,
                rust_avio_read,
                if options.seekable {
                    Some(rust_avio_seek)
                } else {
                    None
                },
                &raw_opts,
                &mut raw_info,
                &mut raw_err,
            )
        };
        if raw.is_null() {
            let message = av_error_string(raw_err);
            tracing::warn!(code = raw_err, %message, "tg_media_open_custom_io failed");
            return Err(VideoError::Ffmpeg {
                context: "tg_media_open_custom_io",
                code: raw_err,
                message,
            });
        }

        let nopts = av_nopts_value();
        let format_duration_ms = if raw_info.format_duration_us != nopts {
            let ms = pts_to_time(raw_info.format_duration_us, Rational::UNIVERSAL_TIME_BASE);
            if ms > 0 && ms <= DURATION_MAX {
                ms + 1
            } else {
                DURATION_UNAVAILABLE
            }
        } else {
            DURATION_UNAVAILABLE
        };

        let video_tb = Rational::new(raw_info.video_timebase_num, raw_info.video_timebase_den);
        let audio_tb = Rational::new(raw_info.audio_timebase_num, raw_info.audio_timebase_den);

        let compute_stream_dur = |stream_pts: i64, tb: Rational, unreliable_fmt: bool| -> i64 {
            if duration_override_ms > 0 {
                return duration_override_ms;
            }
            let d = if stream_pts != nopts {
                pts_to_time(stream_pts, tb)
            } else if unreliable_fmt {
                TIME_UNKNOWN
            } else if raw_info.format_duration_us != nopts {
                pts_to_time(raw_info.format_duration_us, Rational::UNIVERSAL_TIME_BASE)
            } else {
                TIME_UNKNOWN
            };
            if d == TIME_UNKNOWN || d <= 0 || d + 1 > DURATION_MAX {
                DURATION_UNAVAILABLE
            } else {
                d + 1
            }
        };

        let is_webm_vp9 = raw_info.video_is_webm_vp9 != 0;
        let info = MediaStreamInfo {
            has_video: raw_info.has_video != 0,
            video_stream_index: raw_info.video_stream_index,
            video_width: raw_info.video_width.max(0) as u32,
            video_height: raw_info.video_height.max(0) as u32,
            video_time_base: video_tb,
            video_duration_ms: compute_stream_dur(
                raw_info.video_duration_pts,
                video_tb,
                is_webm_vp9,
            ),
            video_aspect: validate_aspect_ratio(Rational::new(
                raw_info.video_sar_num,
                raw_info.video_sar_den,
            )),
            video_fps: raw_info.video_fps,
            video_rotation: Rotation::from_degrees(raw_info.video_rotation),
            video_is_h264: raw_info.video_is_h264 != 0,
            video_is_vp9: raw_info.video_is_vp9 != 0,
            video_is_webm_vp9: is_webm_vp9,
            video_codec_name: c_buf_to_string(&raw_info.video_codec_name),

            has_audio: raw_info.has_audio != 0,
            audio_stream_index: raw_info.audio_stream_index,
            audio_sample_rate: raw_info.audio_sample_rate.max(0) as u32,
            audio_channels: raw_info.audio_channels.max(0) as u16,
            audio_time_base: audio_tb,
            audio_duration_ms: compute_stream_dur(raw_info.audio_duration_pts, audio_tb, false),
            audio_codec_name: c_buf_to_string(&raw_info.audio_codec_name),

            format_duration_ms,
            format_name: c_buf_to_string(&raw_info.format_name),
        };

        Ok(Self {
            raw,
            _io_box: io_box,
            info,
            max_area: options.video_max_area,
            video_frame_index: 0,
            last_hw_accel: HwAccelKind::None,
        })
    }

    /// Opens a local file path through custom I/O.
    pub fn open_file(path: &Path, options: FfmpegOpenOptions) -> Result<Self> {
        let source = FileIoSource::open(path)?;
        Self::open(Box::new(source), options, 0)
    }

    /// Opens an in-memory buffer through custom I/O.
    pub fn open_bytes(bytes: Vec<u8>, options: FfmpegOpenOptions) -> Result<Self> {
        Self::open(Box::new(MemoryIoSource::new(bytes)), options, 0)
    }

    /// Returns container and stream metadata.
    #[must_use]
    pub fn info(&self) -> &MediaStreamInfo {
        &self.info
    }

    /// Returns the hardware accelerator actually used on the most recently decoded video frame.
    #[must_use]
    pub const fn last_hw_accel(&self) -> HwAccelKind {
        self.last_hw_accel
    }

    /// Seeks the primary stream (video if available, else audio) to `position_ms`
    /// using `AVSEEK_FLAG_BACKWARD` (matching `File::Context::seekToPosition`).
    pub fn seek_ms(&mut self, position_ms: i64, backward: bool) -> Result<()> {
        let clamped = position_ms.max(0);
        let err = if self.info.has_video {
            let target = if self.info.video_duration_ms != DURATION_UNAVAILABLE
                && self.info.video_duration_ms > 1
            {
                clamped.min(self.info.video_duration_ms - 1)
            } else {
                clamped
            };
            let pts = time_to_pts(target, self.info.video_time_base);
            // SAFETY: `self.raw` is a live `TgMediaContext`.
            unsafe { sys::tg_media_seek_video(self.raw, pts, i32::from(backward)) }
        } else if self.info.has_audio {
            let target = if self.info.audio_duration_ms != DURATION_UNAVAILABLE
                && self.info.audio_duration_ms > 1
            {
                clamped.min(self.info.audio_duration_ms - 1)
            } else {
                clamped
            };
            let pts = time_to_pts(target, self.info.audio_time_base);
            // SAFETY: `self.raw` is a live `TgMediaContext`.
            unsafe { sys::tg_media_seek_audio(self.raw, pts, i32::from(backward)) }
        } else {
            return Err(StreamingError::OpenFailed.into());
        };
        if err < 0 {
            return Err(VideoError::Ffmpeg {
                context: "av_seek_frame",
                code: err,
                message: av_error_string(err),
            });
        }
        self.video_frame_index = 0;
        Ok(())
    }

    /// Seeks back to the beginning of the stream using tdesktop's multi-flag fallback chain.
    pub fn seek_to_start(&mut self) -> Result<()> {
        // SAFETY: `self.raw` is a live `TgMediaContext`.
        let err = unsafe { sys::tg_media_seek_to_start(self.raw) };
        if err < 0 {
            return Err(VideoError::Ffmpeg {
                context: "tg_media_seek_to_start",
                code: err,
                message: av_error_string(err),
            });
        }
        self.video_frame_index = 0;
        Ok(())
    }

    /// Flushes internal codec buffers.
    pub fn flush(&mut self) {
        // SAFETY: `self.raw` is a live `TgMediaContext`.
        unsafe { sys::tg_media_flush_codecs(self.raw) };
    }

    /// Decodes the next available video or audio frame. Returns `Ok(None)` on EOF.
    #[allow(clippy::too_many_arguments)]
    pub fn decode_next(
        &mut self,
        want_video: bool,
        want_audio: bool,
        output_format: OutputPixelFormat,
        resize: Option<Size>,
        apply_rotation: bool,
        audio_sample_rate: u32,
        audio_channels: u16,
    ) -> Result<Option<DecodedMediaFrame>> {
        let mut v_meta = sys::TgVideoFrameMeta::default();
        let mut a_meta = sys::TgAudioFrameMeta::default();

        // SAFETY: `self.raw` is valid; `v_meta` and `a_meta` are valid stack structs.
        let res = unsafe {
            sys::tg_media_decode_next(
                self.raw,
                i32::from(want_video),
                i32::from(want_audio),
                &mut v_meta,
                &mut a_meta,
            )
        };
        match res {
            0 => Ok(None),
            1 => {
                let frame =
                    self.extract_video_frame(&v_meta, output_format, resize, apply_rotation)?;
                Ok(Some(DecodedMediaFrame::Video(frame)))
            }
            2 => {
                let frame = self.extract_audio_frame(&a_meta, audio_sample_rate, audio_channels)?;
                Ok(Some(DecodedMediaFrame::Audio(frame)))
            }
            err => Err(VideoError::Ffmpeg {
                context: "tg_media_decode_next",
                code: err,
                message: av_error_string(err),
            }),
        }
    }

    fn compute_frame_pts(
        best_effort: i64,
        pts: i64,
        pkt_dts: i64,
        tb: Rational,
        duration_ms: i64,
    ) -> i64 {
        let nopts = av_nopts_value();
        let chosen = if best_effort != nopts {
            best_effort
        } else if pts != nopts {
            pts
        } else {
            pkt_dts
        };
        let t = pts_to_time(chosen, tb);
        if t == TIME_UNKNOWN {
            return 0;
        }
        let non_neg = t.max(0);
        if duration_ms != DURATION_UNAVAILABLE && duration_ms > 0 {
            non_neg.min(duration_ms)
        } else {
            non_neg
        }
    }

    fn extract_video_frame(
        &mut self,
        meta: &sys::TgVideoFrameMeta,
        output_format: OutputPixelFormat,
        resize: Option<Size>,
        apply_rotation: bool,
    ) -> Result<VideoFrame> {
        let src_w = meta.width.max(0) as u32;
        let src_h = meta.height.max(0) as u32;
        if !valid_frame_size(src_w, src_h, self.max_area) {
            return Err(VideoError::FrameSizeExceeded {
                width: src_w,
                height: src_h,
                max_area: self.max_area,
            });
        }
        self.last_hw_accel = HwAccelKind::from_raw(meta.hw_accel_used);

        let pts_ms = Self::compute_frame_pts(
            meta.best_effort_timestamp,
            meta.pts,
            meta.pkt_dts,
            self.info.video_time_base,
            self.info.video_duration_ms,
        );
        let dur_ms = {
            let d = pts_to_time(meta.duration, self.info.video_time_base);
            if d == TIME_UNKNOWN || d < 0 { 0 } else { d }
        };
        let has_alpha = meta.has_alpha != 0;
        let rotation = self.info.video_rotation;
        let frame_index = self.video_frame_index;
        self.video_frame_index += 1;

        if output_format == OutputPixelFormat::PreferYuv
            && resize.is_none()
            && !has_alpha
            && (meta.pix_fmt_kind == sys::TG_PIX_YUV420P || meta.pix_fmt_kind == sys::TG_PIX_NV12)
        {
            let cw = meta.chroma_width.max(1) as u32;
            let ch = meta.chroma_height.max(1) as u32;
            let mut y = vec![0u8; (src_w as usize) * (src_h as usize)];
            if meta.pix_fmt_kind == sys::TG_PIX_YUV420P {
                let c_len = (cw as usize) * (ch as usize);
                let mut u = vec![0u8; c_len];
                let mut v = vec![0u8; c_len];
                // SAFETY: `y`, `u`, `v` have exact lengths for the active YUV420P frame.
                let err = unsafe {
                    sys::tg_media_extract_yuv(
                        self.raw,
                        y.as_mut_ptr(),
                        y.len(),
                        u.as_mut_ptr(),
                        u.len(),
                        v.as_mut_ptr(),
                        v.len(),
                    )
                };
                if err < 0 {
                    return Err(VideoError::Ffmpeg {
                        context: "tg_media_extract_yuv",
                        code: err,
                        message: av_error_string(err),
                    });
                }
                return Ok(VideoFrame {
                    width: src_w,
                    height: src_h,
                    pts_ms,
                    duration_ms: dur_ms,
                    index: frame_index,
                    has_alpha: false,
                    rotation,
                    data: FrameData::Yuv420(Yuv420Data {
                        chroma_width: cw,
                        chroma_height: ch,
                        y,
                        u,
                        v,
                    }),
                });
            } else {
                let mut uv = vec![0u8; (cw as usize) * 2 * (ch as usize)];
                // SAFETY: `y` and `uv` have exact lengths for the active NV12 frame.
                let err = unsafe {
                    sys::tg_media_extract_yuv(
                        self.raw,
                        y.as_mut_ptr(),
                        y.len(),
                        uv.as_mut_ptr(),
                        uv.len(),
                        std::ptr::null_mut(),
                        0,
                    )
                };
                if err < 0 {
                    return Err(VideoError::Ffmpeg {
                        context: "tg_media_extract_yuv",
                        code: err,
                        message: av_error_string(err),
                    });
                }
                return Ok(VideoFrame {
                    width: src_w,
                    height: src_h,
                    pts_ms,
                    duration_ms: dur_ms,
                    index: frame_index,
                    has_alpha: false,
                    rotation,
                    data: FrameData::Nv12(Nv12Data {
                        chroma_width: cw,
                        chroma_height: ch,
                        y,
                        uv,
                    }),
                });
            }
        }

        let target = match resize {
            Some(sz) if !sz.is_empty() => {
                if apply_rotation && rotation.swaps_width_height() {
                    sz.transposed()
                } else {
                    sz
                }
            }
            _ => Size::new(src_w, src_h),
        };
        if !valid_frame_size(target.width, target.height, self.max_area) {
            return Err(VideoError::FrameSizeExceeded {
                width: target.width,
                height: target.height,
                max_area: self.max_area,
            });
        }

        let stride = aligned_frame_stride(target.width, target.height, 0).ok_or(
            VideoError::FrameSizeExceeded {
                width: target.width,
                height: target.height,
                max_area: self.max_area,
            },
        )?;
        let mut aligned_buf = vec![0u8; stride * (target.height as usize)];
        let raw_out_fmt = match output_format {
            OutputPixelFormat::Bgra => sys::TG_OUT_BGRA,
            OutputPixelFormat::Rgba | OutputPixelFormat::PreferYuv => sys::TG_OUT_RGBA,
        };

        // SAFETY: `aligned_buf` is allocated with `stride * target.height` bytes.
        let err = unsafe {
            sys::tg_media_convert_video_frame(
                self.raw,
                target.width as c_int,
                target.height as c_int,
                raw_out_fmt,
                aligned_buf.as_mut_ptr(),
                stride as c_int,
            )
        };
        if err < 0 {
            return Err(VideoError::Ffmpeg {
                context: "sws_scale",
                code: err,
                message: av_error_string(err),
            });
        }

        // Pack into tight `width * 4` stride.
        let row_bytes = (target.width as usize) * PIXEL_BYTES_SIZE;
        let mut tight = if stride == row_bytes {
            aligned_buf
        } else {
            let mut out = vec![0u8; row_bytes * (target.height as usize)];
            for y in 0..target.height as usize {
                let s = y * stride;
                let d = y * row_bytes;
                out[d..d + row_bytes].copy_from_slice(&aligned_buf[s..s + row_bytes]);
            }
            out
        };

        let (final_buf, out_w, out_h) = if apply_rotation && rotation != Rotation::Deg0 {
            rotate_pixels_4bpp(&tight, target.width, target.height, rotation)
        } else {
            (std::mem::take(&mut tight), target.width, target.height)
        };

        let data = match output_format {
            OutputPixelFormat::Bgra => FrameData::Bgra(final_buf),
            OutputPixelFormat::Rgba | OutputPixelFormat::PreferYuv => FrameData::Rgba(final_buf),
        };

        Ok(VideoFrame {
            width: out_w,
            height: out_h,
            pts_ms,
            duration_ms: dur_ms,
            index: frame_index,
            has_alpha,
            rotation,
            data,
        })
    }

    fn extract_audio_frame(
        &mut self,
        meta: &sys::TgAudioFrameMeta,
        dst_sample_rate: u32,
        dst_channels: u16,
    ) -> Result<AudioFrame> {
        let rate = if dst_sample_rate > 0 {
            dst_sample_rate
        } else {
            meta.sample_rate.max(8000) as u32
        };
        let channels = if dst_channels > 0 {
            dst_channels
        } else {
            meta.channels.clamp(1, 8) as u16
        };

        // SAFETY: `self.raw` holds an active decoded audio frame.
        let bound = unsafe { sys::tg_media_audio_out_frames_bound(self.raw, rate as c_int) };
        if bound < 0 {
            return Err(VideoError::Ffmpeg {
                context: "tg_media_audio_out_frames_bound",
                code: bound,
                message: av_error_string(bound),
            });
        }
        let max_frames = bound.max(meta.nb_samples.max(1));
        let mut samples = vec![0.0f32; (max_frames as usize) * (channels as usize)];

        // SAFETY: `samples` has capacity `max_frames * channels` floats.
        let written = unsafe {
            sys::tg_media_convert_audio_f32(
                self.raw,
                rate as c_int,
                c_int::from(channels),
                samples.as_mut_ptr(),
                max_frames,
            )
        };
        if written < 0 {
            return Err(VideoError::Ffmpeg {
                context: "swr_convert",
                code: written,
                message: av_error_string(written),
            });
        }
        samples.truncate((written as usize) * (channels as usize));

        let pts_ms = Self::compute_frame_pts(
            meta.best_effort_timestamp,
            meta.pts,
            meta.pkt_dts,
            self.info.audio_time_base,
            self.info.audio_duration_ms,
        );
        let dur_ms = {
            let d = pts_to_time(meta.duration, self.info.audio_time_base);
            if d == TIME_UNKNOWN || d <= 0 {
                (i64::from(written) * 1000) / i64::from(rate)
            } else {
                d
            }
        };

        Ok(AudioFrame {
            sample_rate: rate,
            channels,
            pts_ms,
            duration_ms: dur_ms,
            samples,
        })
    }
}

/// Output frame from [`FrameGenerator`].
#[derive(Debug, Clone)]
pub struct GeneratedFrame {
    /// Decoded RGBA frame.
    pub frame: VideoFrame,
    /// Display duration in milliseconds.
    pub duration_ms: i64,
    /// `true` when this is the final frame before EOF.
    pub last: bool,
}

/// Sequential video frame generator porting `FFmpeg::FrameGenerator`
/// (`ffmpeg_frame_generator.cpp`) used for WebM video stickers and previews.
pub struct FrameGenerator {
    media: FfmpegMedia,
    current: Option<VideoFrame>,
    next: Option<VideoFrame>,
    frame_position_ms: i64,
    current_delay_ms: i64,
    next_delay_ms: i64,
}

impl FrameGenerator {
    /// Opens a frame generator from raw bytes in memory.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        let media = FfmpegMedia::open_bytes(
            bytes,
            FfmpegOpenOptions {
                seekable: true,
                ignore_editlist: false,
                hw_allowed: false,
                open_video: true,
                open_audio: false,
                video_max_area: MAX_FRAME_GENERATOR_AREA,
                thread_count: 0,
            },
        )?;
        Ok(Self {
            media,
            current: None,
            next: None,
            frame_position_ms: 0,
            current_delay_ms: 0,
            next_delay_ms: 0,
        })
    }

    /// Opens a frame generator from a file on disk.
    pub fn from_file(path: &Path) -> Result<Self> {
        let media = FfmpegMedia::open_file(
            path,
            FfmpegOpenOptions {
                seekable: true,
                ignore_editlist: false,
                hw_allowed: false,
                open_video: true,
                open_audio: false,
                video_max_area: MAX_FRAME_GENERATOR_AREA,
                thread_count: 0,
            },
        )?;
        Ok(Self {
            media,
            current: None,
            next: None,
            frame_position_ms: 0,
            current_delay_ms: 0,
            next_delay_ms: 0,
        })
    }

    /// Returns metadata of the underlying media stream.
    #[must_use]
    pub fn info(&self) -> &MediaStreamInfo {
        self.media.info()
    }

    /// Rewinds back to the first frame (`FrameGenerator::jumpToStart`).
    pub fn jump_to_start(&mut self) -> Result<()> {
        self.media.seek_to_start()?;
        self.current = None;
        self.next = None;
        self.frame_position_ms = 0;
        self.current_delay_ms = 0;
        self.next_delay_ms = 0;
        Ok(())
    }

    fn read_next_internal(&mut self, target_size: Option<Size>) -> Result<()> {
        let decoded = self.media.decode_next(
            true,
            false,
            OutputPixelFormat::Rgba,
            target_size,
            true,
            0,
            0,
        )?;
        if let Some(DecodedMediaFrame::Video(mut frame)) = decoded {
            let mut frame_pos = frame.pts_ms;
            self.current_delay_ms = self.next_delay_ms;
            if self.frame_position_ms + self.current_delay_ms < frame_pos {
                self.current_delay_ms = frame_pos - self.frame_position_ms;
            } else if frame_pos < self.frame_position_ms + self.current_delay_ms {
                frame_pos = self.frame_position_ms + self.current_delay_ms;
            }
            self.next_delay_ms = frame.duration_ms.max(0);
            self.frame_position_ms = frame_pos;
            frame.pts_ms = frame_pos;
            self.next = Some(frame);
        } else {
            self.next = None;
        }
        Ok(())
    }

    /// Decodes and returns the next frame (`FrameGenerator::renderNext`).
    /// Returns `Ok(None)` after the final frame has been returned.
    pub fn render_next(&mut self, target_size: Option<Size>) -> Result<Option<GeneratedFrame>> {
        if self.current.is_none() && self.next.is_none() {
            self.read_next_internal(target_size)?;
        }
        self.current = self.next.take();
        let Some(cur) = self.current.clone() else {
            return Ok(None);
        };
        self.read_next_internal(target_size)?;
        let duration_ms = match &self.next {
            Some(next) => (next.pts_ms - cur.pts_ms).max(0),
            None => cur.duration_ms.max(0),
        };
        Ok(Some(GeneratedFrame {
            frame: cur,
            duration_ms,
            last: self.next.is_none(),
        }))
    }
}
