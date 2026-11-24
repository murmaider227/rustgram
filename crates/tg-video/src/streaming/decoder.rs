//! `Decoder` trait and FFmpeg-backed streaming decoder implementation.
//!
//! Port of `Telegram/SourceFiles/media/streaming/media_streaming_file.cpp`,
//! `media_streaming_video_track.cpp`, `media_streaming_audio_track.cpp`, and
//! `media_streaming_utility.cpp`.

use std::path::Path;
use std::sync::Arc;

use crate::error::{Result, StreamingError};
use crate::ffmpeg::{
    CustomIoSource, DecodedMediaFrame, FfmpegMedia, FfmpegOpenOptions, HwAccelKind,
    correct_by_aspect,
};
use crate::streaming::cache::SliceCache;
use crate::streaming::loader::{LocalFileLoader, MemoryRangeLoader, RangeLoader};
use crate::streaming::reader::{SliceConfig, StreamingReader};
use crate::types::{
    AudioFrame, AudioInformation, DURATION_UNAVAILABLE, MAX_FRAME_AREA, PlaybackMode,
    PlaybackOptions, StreamInformation, TIME_UNKNOWN, TrackState, VideoFrame, VideoInformation,
};

/// Result of decoding one step from a [`Decoder`].
#[derive(Debug, Clone)]
pub enum DecodedPacket {
    /// A decoded video frame.
    Video(VideoFrame),
    /// A decoded resampled audio frame.
    Audio(AudioFrame),
    /// Decoder reached end-of-stream (when looping is disabled).
    EndOfStream,
}

/// Abstract media decoder trait implemented by [`FfmpegStreamDecoder`] and usable with
/// deterministic fake decoders in tests.
pub trait Decoder: Send {
    /// Returns metadata for the opened stream(s).
    fn info(&self) -> &StreamInformation;

    /// Total media file size in bytes.
    fn file_size(&self) -> u64;

    /// Returns `true` if backed by a remote streaming loader.
    fn is_remote(&self) -> bool;

    /// Returns `true` if the entire stream is cached locally.
    fn full_in_cache(&self) -> bool {
        !self.is_remote()
    }

    /// Decodes the next video or audio frame.
    fn next_frame(&mut self) -> Result<DecodedPacket>;

    /// Seeks to `position_ms` (performing exact frame-skip after keyframe seek).
    fn seek(&mut self, position_ms: i64) -> Result<()>;

    /// Enables or disables seamless looping at EOF.
    fn set_loop(&mut self, loop_playback: bool);

    /// Updates loader scheduling priority (forwarded to `RangeLoader`).
    fn set_loader_priority(&mut self, _priority: i32) {}
}

/// FFmpeg-backed streaming decoder combining [`StreamingReader`] and [`FfmpegMedia`].
pub struct FfmpegStreamDecoder {
    media: FfmpegMedia,
    info: StreamInformation,
    options: PlaybackOptions,
    file_size: u64,
    is_remote: bool,
    loader: Arc<dyn RangeLoader>,
    pending_first_video: Option<VideoFrame>,
    pending_first_audio: Option<AudioFrame>,
    looping_shift_ms: i64,
    last_video_pts_ms: i64,
    last_video_dur_ms: i64,
    observed_duration_ms: i64,
}

impl FfmpegStreamDecoder {
    /// Opens a streaming decoder backed by any [`RangeLoader`] and optional [`SliceCache`].
    pub fn from_range_loader(
        loader: Arc<dyn RangeLoader>,
        cache: Option<Arc<dyn SliceCache>>,
        slice_config: SliceConfig,
        options: PlaybackOptions,
    ) -> Result<Self> {
        let file_size = loader.size();
        let is_remote = loader.base_cache_key().is_valid();
        let reader = StreamingReader::with_config(Arc::clone(&loader), cache, slice_config)?;
        Self::from_custom_io(Box::new(reader), loader, file_size, is_remote, options)
    }

    /// Opens a streaming decoder from a local file path.
    pub fn from_file(path: &Path, options: PlaybackOptions) -> Result<Self> {
        let loader: Arc<dyn RangeLoader> = Arc::new(LocalFileLoader::open(path)?);
        Self::from_range_loader(loader, None, SliceConfig::default(), options)
    }

    /// Opens a streaming decoder from an in-memory buffer.
    pub fn from_bytes(bytes: Vec<u8>, options: PlaybackOptions) -> Result<Self> {
        let loader: Arc<dyn RangeLoader> = Arc::new(MemoryRangeLoader::new(bytes)?);
        Self::from_range_loader(loader, None, SliceConfig::default(), options)
    }

    fn from_custom_io(
        source: Box<dyn CustomIoSource>,
        loader: Arc<dyn RangeLoader>,
        file_size: u64,
        is_remote: bool,
        mut options: PlaybackOptions,
    ) -> Result<Self> {
        if !options.seekable {
            options.position_ms = 0;
        }
        let want_video = matches!(
            options.mode,
            PlaybackMode::Both | PlaybackMode::Video | PlaybackMode::Inspection
        );
        let want_audio = matches!(options.mode, PlaybackMode::Both | PlaybackMode::Audio);

        let open_opts = FfmpegOpenOptions {
            seekable: options.seekable,
            ignore_editlist: false,
            hw_allowed: options.hw_allowed,
            open_video: want_video,
            open_audio: want_audio,
            video_max_area: MAX_FRAME_AREA,
            thread_count: 0,
        };
        let mut media = FfmpegMedia::open(source, open_opts, options.duration_override_ms)?;
        let raw_info = media.info().clone();

        if (options.mode == PlaybackMode::Video && !raw_info.has_video)
            || (options.mode == PlaybackMode::Audio && !raw_info.has_audio)
            || (!raw_info.has_video && !raw_info.has_audio)
        {
            return Err(StreamingError::OpenFailed.into());
        }

        if options.seekable && options.position_ms > 0 {
            media.seek_ms(options.position_ms, true)?;
        }

        // Decode initial frame(s) with exact-seek skipping (matching `tryReadFirstFrame`).
        let mut pending_first_video = None;
        let mut pending_first_audio = None;
        let mut last_video_pts_ms = 0i64;
        let mut last_video_dur_ms = 0i64;

        if want_video && raw_info.has_video {
            let mut skipped_last: Option<VideoFrame> = None;
            loop {
                match media.decode_next(
                    true,
                    false,
                    options.output_format,
                    options.resize,
                    options.apply_rotation,
                    options.audio_sample_rate,
                    options.audio_channels,
                )? {
                    Some(DecodedMediaFrame::Video(frame)) => {
                        last_video_pts_ms = frame.pts_ms;
                        last_video_dur_ms = frame.duration_ms;
                        if frame.pts_ms >= options.position_ms {
                            pending_first_video = Some(frame);
                            break;
                        }
                        skipped_last = Some(frame);
                    }
                    Some(DecodedMediaFrame::Audio(_)) => {}
                    None => {
                        if let Some(last) = skipped_last {
                            pending_first_video = Some(last);
                            break;
                        }
                        return Err(StreamingError::InvalidData.into());
                    }
                }
            }
        } else if want_audio && raw_info.has_audio {
            let mut skipped_last: Option<AudioFrame> = None;
            loop {
                match media.decode_next(
                    false,
                    true,
                    options.output_format,
                    options.resize,
                    options.apply_rotation,
                    options.audio_sample_rate,
                    options.audio_channels,
                )? {
                    Some(DecodedMediaFrame::Audio(frame)) => {
                        if frame.pts_ms >= options.position_ms {
                            pending_first_audio = Some(frame);
                            break;
                        }
                        skipped_last = Some(frame);
                    }
                    Some(DecodedMediaFrame::Video(_)) => {}
                    None => {
                        if let Some(last) = skipped_last {
                            pending_first_audio = Some(last);
                            break;
                        }
                        return Err(StreamingError::InvalidData.into());
                    }
                }
            }
        }

        let video_info = if want_video && raw_info.has_video {
            let cover = pending_first_video.clone();
            let raw_size = crate::types::Size::new(raw_info.video_width, raw_info.video_height);
            let display_size = raw_info
                .video_rotation
                .transpose_size(correct_by_aspect(raw_size, raw_info.video_aspect));
            let real_size = raw_info.video_rotation.transpose_size(raw_size);
            let pos = cover.as_ref().map_or(0, |f| f.pts_ms);
            let has_alpha = cover.as_ref().is_some_and(|f| f.has_alpha);
            Some(VideoInformation {
                state: TrackState {
                    position_ms: pos,
                    received_till_ms: pos,
                    duration_ms: raw_info.video_duration_ms,
                },
                size: display_size,
                real_size,
                aspect: raw_info.video_aspect,
                rotation: raw_info.video_rotation,
                fps: raw_info.video_fps,
                has_alpha,
                codec_name: raw_info.video_codec_name.clone(),
                cover,
            })
        } else {
            None
        };

        let audio_info = if want_audio && raw_info.has_audio {
            let pos = pending_first_audio
                .as_ref()
                .map_or(options.position_ms, |f| f.pts_ms);
            Some(AudioInformation {
                state: TrackState {
                    position_ms: pos,
                    received_till_ms: pos,
                    duration_ms: raw_info.audio_duration_ms,
                },
                sample_rate: raw_info.audio_sample_rate,
                channels: raw_info.audio_channels,
                codec_name: raw_info.audio_codec_name.clone(),
            })
        } else {
            None
        };

        let info = StreamInformation {
            video: video_info,
            audio: audio_info,
            header_size: 0,
            format_name: raw_info.format_name.clone(),
        };

        Ok(Self {
            media,
            info,
            options,
            file_size,
            is_remote,
            loader,
            pending_first_video,
            pending_first_audio,
            looping_shift_ms: 0,
            last_video_pts_ms,
            last_video_dur_ms,
            observed_duration_ms: raw_info.video_duration_ms,
        })
    }

    /// Returns the hardware decoding backend active on the last decoded frame.
    #[must_use]
    pub const fn last_hw_accel(&self) -> HwAccelKind {
        self.media.last_hw_accel()
    }

    fn effective_loop_duration_ms(&self) -> i64 {
        if self.observed_duration_ms != DURATION_UNAVAILABLE
            && self.observed_duration_ms != TIME_UNKNOWN
            && self.observed_duration_ms > 0
        {
            return self.observed_duration_ms;
        }
        (self.last_video_pts_ms + self.last_video_dur_ms.max(1)).max(1)
    }

    fn want_video(&self) -> bool {
        self.info.video.is_some()
            && matches!(
                self.options.mode,
                PlaybackMode::Both | PlaybackMode::Video | PlaybackMode::Inspection
            )
    }

    fn want_audio(&self) -> bool {
        self.info.audio.is_some()
            && matches!(self.options.mode, PlaybackMode::Both | PlaybackMode::Audio)
    }
}

impl Decoder for FfmpegStreamDecoder {
    fn info(&self) -> &StreamInformation {
        &self.info
    }

    fn file_size(&self) -> u64 {
        self.file_size
    }

    fn is_remote(&self) -> bool {
        self.is_remote
    }

    fn next_frame(&mut self) -> Result<DecodedPacket> {
        if let Some(mut v) = self.pending_first_video.take() {
            v.pts_ms += self.looping_shift_ms;
            return Ok(DecodedPacket::Video(v));
        }
        if let Some(mut a) = self.pending_first_audio.take() {
            a.pts_ms += self.looping_shift_ms;
            return Ok(DecodedPacket::Audio(a));
        }

        let want_v = self.want_video();
        let want_a = self.want_audio();

        match self.media.decode_next(
            want_v,
            want_a,
            self.options.output_format,
            self.options.resize,
            self.options.apply_rotation,
            self.options.audio_sample_rate,
            self.options.audio_channels,
        )? {
            Some(DecodedMediaFrame::Video(mut frame)) => {
                self.last_video_pts_ms = frame.pts_ms;
                self.last_video_dur_ms = frame.duration_ms;
                frame.pts_ms += self.looping_shift_ms;
                Ok(DecodedPacket::Video(frame))
            }
            Some(DecodedMediaFrame::Audio(mut frame)) => {
                frame.pts_ms += self.looping_shift_ms;
                Ok(DecodedPacket::Audio(frame))
            }
            None => {
                if !self.options.loop_playback {
                    return Ok(DecodedPacket::EndOfStream);
                }
                let dur = self.effective_loop_duration_ms();
                self.looping_shift_ms = self.looping_shift_ms.saturating_add(dur);
                self.media.seek_to_start()?;
                // Decode the first frame of the new loop iteration; if empty, stop.
                match self.media.decode_next(
                    want_v,
                    want_a,
                    self.options.output_format,
                    self.options.resize,
                    self.options.apply_rotation,
                    self.options.audio_sample_rate,
                    self.options.audio_channels,
                )? {
                    Some(DecodedMediaFrame::Video(mut frame)) => {
                        self.last_video_pts_ms = frame.pts_ms;
                        self.last_video_dur_ms = frame.duration_ms;
                        frame.pts_ms += self.looping_shift_ms;
                        Ok(DecodedPacket::Video(frame))
                    }
                    Some(DecodedMediaFrame::Audio(mut frame)) => {
                        frame.pts_ms += self.looping_shift_ms;
                        Ok(DecodedPacket::Audio(frame))
                    }
                    None => Ok(DecodedPacket::EndOfStream),
                }
            }
        }
    }

    fn seek(&mut self, position_ms: i64) -> Result<()> {
        if !self.options.seekable {
            return Err(StreamingError::NotStreamable.into());
        }
        let target = position_ms.max(0);
        self.pending_first_video = None;
        self.pending_first_audio = None;
        self.looping_shift_ms = 0;

        if target == 0 {
            self.media.seek_to_start()?;
            return Ok(());
        }
        self.media.seek_ms(target, true)?;

        // Skip old frames from the preceding keyframe up to `target`
        // (mirrors `VideoTrackObject::tryReadFirstFrame`).
        if self.want_video() {
            let mut last_seen: Option<VideoFrame> = None;
            loop {
                match self.media.decode_next(
                    true,
                    false,
                    self.options.output_format,
                    self.options.resize,
                    self.options.apply_rotation,
                    self.options.audio_sample_rate,
                    self.options.audio_channels,
                )? {
                    Some(DecodedMediaFrame::Video(frame)) => {
                        if frame.pts_ms >= target {
                            self.pending_first_video = Some(frame);
                            break;
                        }
                        last_seen = Some(frame);
                    }
                    Some(DecodedMediaFrame::Audio(_)) => {}
                    None => {
                        self.pending_first_video = last_seen;
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    fn set_loop(&mut self, loop_playback: bool) {
        self.options.loop_playback = loop_playback;
    }

    fn set_loader_priority(&mut self, priority: i32) {
        self.loader.set_priority(priority);
    }
}
