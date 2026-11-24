//! Inline clip reader (GIF/MP4 animations, round videos, WebM/VP9 stickers with alpha),
//! MP4 streaming check, and circular / rounded-rectangle mask helpers.
//!
//! Port of `Telegram/SourceFiles/media/clip/media_clip_reader.cpp`,
//! `media_clip_ffmpeg.cpp`, `media_clip_check_streaming.cpp`, and
//! `media/streaming/media_streaming_round_preview.cpp`.

use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use crate::error::{Result, StreamingError};
use crate::ffmpeg::{DecodedMediaFrame, FfmpegMedia, FfmpegOpenOptions, MediaStreamInfo};
use crate::types::{
    DURATION_UNAVAILABLE, FrameData, MAX_FRAME_AREA, MAX_INLINE_AREA, OutputPixelFormat, Size,
    VideoFrame,
};

/// Maximum in-memory clip size for GIFv / WebM sticker classification (`kMaxInMemory` = 10 MB).
pub const MAX_CLIP_IN_MEMORY: u64 = 10 * 1024 * 1024;

/// Maximum byte offset where MP4 `moov` atom may start for streaming (`kFindMoovBefore` = 128 KB).
pub const FIND_MOOV_BEFORE: u64 = 128 * 1024;

/// Operating mode for [`ClipReader`] (mirrors `Media::Clip::ReaderImplementation::Mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipMode {
    /// Inline looping animation playback (4K capped at `MAX_INLINE_AREA` = 1280×720).
    #[default]
    Normal,
    /// Inspection/sending mode (allows up to 4K `MAX_FRAME_AREA` = 3840×2160 and inspects audio).
    Inspecting,
}

impl ClipMode {
    /// Maximum allowed decoded pixel area for this mode (`MaxAreaForMode`).
    #[must_use]
    pub const fn max_area(self) -> u64 {
        match self {
            Self::Normal => MAX_INLINE_AREA,
            Self::Inspecting => MAX_FRAME_AREA,
        }
    }
}

/// Checks whether an MP4/MOV stream has its `moov` atom within the first 128 KB
/// (port of `Media::Clip::CheckStreamingSupport` in `media_clip_check_streaming.cpp`).
pub fn check_streaming_support_reader<R: Read + Seek>(reader: &mut R, total_size: u64) -> bool {
    const HEADER_SIZE: u64 = 8;
    if total_size < HEADER_SIZE {
        return false;
    }
    let mut last_read_pos = 0u64;
    let mut atom_header = [0u8; 8];

    loop {
        let Ok(position) = reader.stream_position() else {
            return false;
        };
        if reader.read_exact(&mut atom_header).is_err() {
            return false;
        }
        if last_read_pos >= FIND_MOOV_BEFORE {
            return false;
        }
        if &atom_header[4..8] == b"moov" {
            return true;
        }

        let raw_len = u32::from_be_bytes([
            atom_header[0],
            atom_header[1],
            atom_header[2],
            atom_header[3],
        ]);
        let (header_size, atom_len) = if raw_len == 1 {
            let mut ext = [0u8; 8];
            if reader.read_exact(&mut ext).is_err() {
                return false;
            }
            (HEADER_SIZE * 2, u64::from_be_bytes(ext))
        } else {
            (HEADER_SIZE, u64::from(raw_len))
        };

        if atom_len < header_size || position > total_size || atom_len > total_size - position {
            return false;
        }
        let next_pos = position + atom_len;
        if reader.seek(SeekFrom::Start(next_pos)).is_err() {
            return false;
        }
        last_read_pos = position;
    }
}

/// Checks whether in-memory MP4 bytes support progressive streaming (`moov` within 128 KB).
#[must_use]
pub fn check_streaming_support(bytes: &[u8]) -> bool {
    let mut cursor = Cursor::new(bytes);
    check_streaming_support_reader(&mut cursor, bytes.len() as u64)
}

/// Checks whether a local MP4 file supports progressive streaming (`moov` within 128 KB).
pub fn check_streaming_support_file(path: &Path) -> std::io::Result<bool> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    Ok(check_streaming_support_reader(&mut file, size))
}

/// Clip reader for GIF/MP4 animations, round videos, and VP9/WebM video stickers with alpha.
///
/// Port of `Media::Clip::internal::FFMpegReaderImplementation` (`media_clip_ffmpeg.cpp`)
/// and `Media::Clip::Reader` (`media_clip_reader.cpp`).
pub struct ClipReader {
    media: FfmpegMedia,
    data_size: u64,
    _mode: ClipMode,
    resize: Option<Size>,
    round_circle: bool,
    frame_ms: i64,
    frame_time_ms: i64,
    current_frame_delay_ms: i64,
    next_frame_delay_ms: i64,
    had_frame: bool,
    frame_index: usize,
    current_frame: Option<VideoFrame>,
    loop_count: u64,
}

impl ClipReader {
    /// Opens a clip reader from an in-memory buffer.
    pub fn from_bytes(bytes: Vec<u8>, mode: ClipMode) -> Result<Self> {
        let data_size = bytes.len() as u64;
        let media = FfmpegMedia::open_bytes(
            bytes,
            FfmpegOpenOptions {
                seekable: true,
                ignore_editlist: false,
                hw_allowed: false,
                open_video: true,
                open_audio: false,
                video_max_area: mode.max_area(),
                thread_count: 0,
            },
        )?;
        Self::finish_init(media, data_size, mode)
    }

    /// Opens a clip reader from a local file path.
    pub fn from_file(path: &Path, mode: ClipMode) -> Result<Self> {
        let data_size = std::fs::metadata(path)?.len();
        let media = FfmpegMedia::open_file(
            path,
            FfmpegOpenOptions {
                seekable: true,
                ignore_editlist: false,
                hw_allowed: false,
                open_video: true,
                open_audio: false,
                video_max_area: mode.max_area(),
                thread_count: 0,
            },
        )?;
        Self::finish_init(media, data_size, mode)
    }

    fn finish_init(media: FfmpegMedia, data_size: u64, mode: ClipMode) -> Result<Self> {
        if !media.info().has_video {
            return Err(StreamingError::OpenFailed.into());
        }
        Ok(Self {
            media,
            data_size,
            _mode: mode,
            resize: None,
            round_circle: false,
            frame_ms: 0,
            frame_time_ms: 0,
            current_frame_delay_ms: 0,
            next_frame_delay_ms: 0,
            had_frame: false,
            frame_index: 0,
            current_frame: None,
            loop_count: 0,
        })
    }

    /// Configures optional target output size for rendered frames.
    pub fn set_resize(&mut self, size: Option<Size>) {
        self.resize = size;
    }

    /// Enables or disables automatic circular masking (for round video notes / `RoundPreview`).
    pub fn set_round_circle(&mut self, round: bool) {
        self.round_circle = round;
    }

    /// Returns metadata for the underlying media stream.
    #[must_use]
    pub fn info(&self) -> &MediaStreamInfo {
        self.media.info()
    }

    /// Returns `true` if the container also contains an audio stream (`hasAudio`).
    #[must_use]
    pub fn has_audio(&self) -> bool {
        self.media.info().has_audio
    }

    /// Returns `true` if this clip qualifies as a silent H.264 GIFv animation (`isGifv`).
    #[must_use]
    pub fn is_gifv(&self) -> bool {
        !self.has_audio() && self.data_size <= MAX_CLIP_IN_MEMORY && self.media.info().video_is_h264
    }

    /// Returns `true` if this clip qualifies as a silent VP9 WebM video sticker (`isWebmSticker`).
    #[must_use]
    pub fn is_webm_sticker(&self) -> bool {
        !self.has_audio() && self.data_size <= MAX_CLIP_IN_MEMORY && self.media.info().video_is_vp9
    }

    /// Returns stream duration in milliseconds (`durationMs`), or `0` if unavailable.
    #[must_use]
    pub fn duration_ms(&self) -> i64 {
        let d = self.media.info().video_duration_ms;
        if d == DURATION_UNAVAILABLE || d < 0 {
            0
        } else {
            d
        }
    }

    /// Returns the number of completed loop wrap-arounds.
    #[must_use]
    pub const fn loop_count(&self) -> u64 {
        self.loop_count
    }

    /// Returns the presentation timestamp of the current frame on the continuous playback clock.
    #[must_use]
    pub const fn frame_presentation_time_ms(&self) -> i64 {
        if self.frame_time_ms < 0 {
            0
        } else {
            self.frame_time_ms
        }
    }

    /// Returns the stream-relative timestamp of the current frame (`frameRealTime`).
    #[must_use]
    pub const fn frame_real_time_ms(&self) -> i64 {
        self.frame_ms
    }

    /// Decodes the next frame, automatically looping back to start on EOF
    /// (`FFMpegReaderImplementation::readNextFrame`).
    pub fn next_frame(&mut self) -> Result<VideoFrame> {
        loop {
            match self.media.decode_next(
                true,
                false,
                OutputPixelFormat::Rgba,
                self.resize,
                true,
                0,
                0,
            )? {
                Some(DecodedMediaFrame::Video(mut frame)) => {
                    let mut frame_ms = frame.pts_ms;
                    self.current_frame_delay_ms = self.next_frame_delay_ms;
                    if self.frame_ms + self.current_frame_delay_ms < frame_ms {
                        self.current_frame_delay_ms = frame_ms - self.frame_ms;
                    } else if frame_ms < self.frame_ms + self.current_frame_delay_ms {
                        frame_ms = self.frame_ms + self.current_frame_delay_ms;
                    }
                    self.next_frame_delay_ms = frame.duration_ms.max(0);
                    self.frame_ms = frame_ms;
                    self.frame_time_ms += self.current_frame_delay_ms;
                    self.had_frame = true;
                    frame.index = self.frame_index;
                    self.frame_index += 1;

                    if self.round_circle
                        && let FrameData::Rgba(ref mut pixels) = frame.data
                    {
                        apply_circle_mask_rgba(pixels, frame.width, frame.height);
                        frame.has_alpha = true;
                    }

                    self.current_frame = Some(frame.clone());
                    return Ok(frame);
                }
                Some(DecodedMediaFrame::Audio(_)) => {}
                None => {
                    if !self.had_frame {
                        return Err(StreamingError::InvalidData.into());
                    }
                    self.media.seek_to_start()?;
                    self.had_frame = false;
                    self.frame_ms = 0;
                    self.frame_index = 0;
                    self.loop_count += 1;
                }
            }
        }
    }

    /// Advances playback up to `target_ms`, skipping at most one extra frame to keep up
    /// (`FFMpegReaderImplementation::readFramesTill`).
    pub fn read_frames_till(&mut self, target_ms: i64) -> Result<VideoFrame> {
        if let Some(cur) = &self.current_frame
            && self.frame_time_ms > target_ms
        {
            return Ok(cur.clone());
        }
        let first = self.next_frame()?;
        if self.frame_time_ms > target_ms {
            return Ok(first);
        }
        let second = self.next_frame()?;
        if self.frame_time_ms <= target_ms {
            self.frame_time_ms = target_ms + 5;
        }
        Ok(second)
    }

    /// Seeks to `position_ms` for inspection (`FFMpegReaderImplementation::inspectAt`).
    pub fn inspect_at(&mut self, position_ms: i64) -> Result<VideoFrame> {
        if position_ms > 0 {
            if self.media.seek_ms(position_ms, false).is_err() {
                self.media.seek_ms(position_ms, true)?;
            }
        } else {
            self.media.seek_to_start()?;
        }
        self.frame_ms = position_ms.max(0);
        self.frame_time_ms = position_ms.max(0);
        self.current_frame_delay_ms = 0;
        self.next_frame_delay_ms = 0;
        self.next_frame()
    }
}

/// Generates an antialiased circular alpha mask of size `width * height` (`0` outside, `255` inside).
///
/// Matches Telegram's round video notes (inscribed ellipse/circle with 1-pixel smooth edge).
#[must_use]
pub fn generate_circle_mask(width: u32, height: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let mut mask = vec![0u8; w.saturating_mul(h)];
    if w == 0 || h == 0 {
        return mask;
    }
    let cx = (w as f32) * 0.5;
    let cy = (h as f32) * 0.5;
    let r = cx.min(cy);
    let inner = (r - 0.5).max(0.0);
    let outer = r + 0.5;

    for y in 0..h {
        let dy = (y as f32 + 0.5) - cy;
        for x in 0..w {
            let dx = (x as f32 + 0.5) - cx;
            let dist = (dx * dx + dy * dy).sqrt();
            let alpha = if dist <= inner {
                255u8
            } else if dist >= outer {
                0u8
            } else {
                let t = (outer - dist) / (outer - inner);
                (t.clamp(0.0, 1.0) * 255.0).round() as u8
            };
            mask[y * w + x] = alpha;
        }
    }
    mask
}

/// Applies an antialiased circular mask in-place to tightly-packed RGBA8 pixels
/// (for Telegram round video messages / video notes).
pub fn apply_circle_mask_rgba(rgba: &mut [u8], width: u32, height: u32) {
    let mask = generate_circle_mask(width, height);
    for (px, &m) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(mask.iter()) {
        if m == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            px[3] = 0;
        } else if m < 255 {
            let a = (u16::from(px[3]) * u16::from(m) + 127) / 255;
            px[3] = a as u8;
        }
    }
}

/// Applies an antialiased rounded-rectangle corner mask in-place to RGBA8 pixels.
pub fn apply_rounded_rect_mask_rgba(rgba: &mut [u8], width: u32, height: u32, radius: u32) {
    let w = width as usize;
    let h = height as usize;
    if w == 0 || h == 0 || radius == 0 || rgba.len() < w.saturating_mul(h).saturating_mul(4) {
        return;
    }
    let r = (radius as f32).min((w as f32) * 0.5).min((h as f32) * 0.5);
    let inner = (r - 0.5).max(0.0);
    let outer = r + 0.5;
    let wf = w as f32;
    let hf = h as f32;

    for y in 0..h {
        let py = y as f32 + 0.5;
        let cy = if py < r {
            r
        } else if py > hf - r {
            hf - r
        } else {
            continue;
        };
        for x in 0..w {
            let px_f = x as f32 + 0.5;
            let cx = if px_f < r {
                r
            } else if px_f > wf - r {
                wf - r
            } else {
                continue;
            };
            let dx = px_f - cx;
            let dy = py - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let idx = (y * w + x) * 4;
            if dist >= outer {
                rgba[idx] = 0;
                rgba[idx + 1] = 0;
                rgba[idx + 2] = 0;
                rgba[idx + 3] = 0;
            } else if dist > inner {
                let t = (outer - dist) / (outer - inner);
                let m = (t.clamp(0.0, 1.0) * 255.0).round() as u16;
                rgba[idx + 3] = ((u16::from(rgba[idx + 3]) * m + 127) / 255) as u8;
            }
        }
    }
}
