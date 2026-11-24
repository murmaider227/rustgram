//! Voice message recording: 48 kHz mono Opus in Ogg, 32 kbps, level metering,
//! fade-in/fade-out, skip window, max duration, and incremental waveform computation.
//!
//! Port of `media/audio/media_audio_capture.h`, `media/audio/media_audio_capture.cpp`,
//! `media/audio/media_audio_capture_common.h`, and duration limits from
//! `history/view/controls/history_view_voice_record_bar.cpp`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, StreamTrait};
use opus_pure::{Application, MAX_PACKET_BYTES, OggOpusWriter, OpusEncoder, OpusHead, OpusTags};

use crate::device::{DeviceManager, DeviceType};
use crate::dsp::{Resampler, f32_to_i16, to_mono_f32};
use crate::error::{AudioError, AudioResult};
use crate::opus_codec::{OPUS_VOICE_BITRATE_BPS, OPUS_VOICE_FRAME_SAMPLES};
use crate::types::DEFAULT_FREQUENCY;
use crate::waveform::{CaptureWaveformCollector, VoiceWaveform, encode_5bit, read_one_sample_i16};

/// Voice capture sample rate: 48 kHz (`kCaptureFrequency = Player::kDefaultFrequency`).
pub const CAPTURE_FREQUENCY: u32 = DEFAULT_FREQUENCY;

/// Initial silence skip duration in milliseconds (`kCaptureSkipDuration = 400`).
pub const CAPTURE_SKIP_DURATION_MS: i64 = 400;

/// Fade-in and fade-out ramp duration in milliseconds (`kCaptureFadeInDuration = 300`).
pub const CAPTURE_FADE_IN_DURATION_MS: i64 = 300;

/// Minimum interval between level-meter UI updates in milliseconds (`kCaptureUpdateDelta = 100`).
pub const CAPTURE_UPDATE_DELTA_MS: i64 = 100;

/// Maximum voice message duration in seconds (`kAudioVoiceMaxLength = 100 * 60` = 100 min).
pub const CAPTURE_MAX_DURATION_SECS: i64 = 100 * 60;

/// Maximum voice message sample count at 48 kHz.
pub const CAPTURE_MAX_SAMPLES: i64 = CAPTURE_FREQUENCY as i64 * CAPTURE_MAX_DURATION_SECS;

/// Minimum captured samples required to emit a valid recording on stop (`kCaptureFrequency` = 1 s).
pub const CAPTURE_MIN_SAMPLES: usize = CAPTURE_FREQUENCY as usize;

/// Live capture progress update (`Media::Capture::Update`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaptureUpdate {
    /// Total samples captured so far at 48 kHz.
    pub samples: i64,
    /// Peak amplitude (`0..=32768`) since the previous update.
    pub level: u16,
    /// `true` if capture reached `max_duration` and stopped automatically.
    pub finished: bool,
}

/// Completed (or paused snapshot) voice recording result (`Media::Capture::Result`).
#[derive(Debug, Clone)]
pub struct CaptureResult {
    /// Telegram-compatible `.ogg` Opus bytes (empty if recording was too short).
    pub bytes: Vec<u8>,
    /// 100-sample unpacked waveform (`0..=31`).
    pub waveform: VoiceWaveform,
    /// Telegram 5-bit packed waveform bytes (ready for `documentAttributeAudio.waveform`).
    pub waveform_5bit: Vec<u8>,
    /// Total duration in milliseconds.
    pub duration_ms: i64,
}

/// Configuration for [`VoiceCapturePipeline`] and [`AudioRecorder`].
#[derive(Debug, Clone, Copy)]
pub struct CaptureConfig {
    /// Output Opus sample rate (default 48,000 Hz).
    pub sample_rate: u32,
    /// Target Opus bitrate in bits/sec (default 32,000 bps).
    pub bitrate_bps: i32,
    /// Initial muted skip duration in ms (default 400 ms).
    pub skip_duration_ms: i64,
    /// Fade-in and tail fade-out duration in ms (default 300 ms).
    pub fade_duration_ms: i64,
    /// Interval in ms between [`CaptureUpdate`] level notifications (default 100 ms).
    pub update_delta_ms: i64,
    /// Minimum total samples to produce a non-empty result on stop (default 48,000 = 1.0 s).
    pub min_samples: usize,
    /// Maximum duration in seconds before auto-finishing (default 6000 s = 100 min).
    pub max_duration_secs: i64,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            sample_rate: CAPTURE_FREQUENCY,
            bitrate_bps: OPUS_VOICE_BITRATE_BPS,
            skip_duration_ms: CAPTURE_SKIP_DURATION_MS,
            fade_duration_ms: CAPTURE_FADE_IN_DURATION_MS,
            update_delta_ms: CAPTURE_UPDATE_DELTA_MS,
            min_samples: CAPTURE_MIN_SAMPLES,
            max_duration_secs: CAPTURE_MAX_DURATION_SECS,
        }
    }
}

/// Pure software voice capture & Ogg Opus encoding pipeline.
///
/// Faithfully ports `Media::Capture::Instance::Inner::{process, processFrame, pause, stop}`
/// (`media_audio_capture.cpp:464-796`) without requiring hardware audio I/O,
/// making it directly unit-testable and reusable from any audio input backend.
pub struct VoiceCapturePipeline {
    config: CaptureConfig,
    frame_samples: usize,
    encoder: OpusEncoder,
    writer: Option<OggOpusWriter<Vec<u8>>>,
    packet_buf: Vec<u8>,
    captured: Vec<i16>,
    full_samples: i64,
    last_update_samples: i64,
    level_max: u16,
    waveform: CaptureWaveformCollector,
    paused: bool,
    finished: bool,
    input_resampler: Option<Resampler>,
    input_channels: usize,
}

impl VoiceCapturePipeline {
    /// Creates a new capture pipeline with the given [`CaptureConfig`].
    pub fn new(config: CaptureConfig) -> AudioResult<Self> {
        let frame_samples = if config.sample_rate == CAPTURE_FREQUENCY {
            OPUS_VOICE_FRAME_SAMPLES
        } else {
            ((u64::from(config.sample_rate) * 20) / 1000) as usize
        };
        let mut encoder = OpusEncoder::new(config.sample_rate as i32, 1, Application::Voip)?;
        encoder.bitrate_bps = config.bitrate_bps;

        let head = OpusHead::for_encoder(&encoder, config.sample_rate);
        let mut tags = OpusTags::new();
        tags.vendor = "tg-audio".to_string();
        let writer = OggOpusWriter::with_tags(Vec::new(), head, tags)?;

        Ok(Self {
            config,
            frame_samples,
            encoder,
            writer: Some(writer),
            packet_buf: vec![0u8; MAX_PACKET_BYTES],
            captured: Vec::with_capacity(frame_samples * 32),
            full_samples: 0,
            last_update_samples: 0,
            level_max: 0,
            waveform: CaptureWaveformCollector::new(config.sample_rate),
            paused: false,
            finished: false,
            input_resampler: None,
            input_channels: 1,
        })
    }

    /// Creates a capture pipeline configured for Telegram's exact defaults
    /// (48 kHz mono, 32 kbps Opus, 400 ms skip, 300 ms fade, 100 ms level updates).
    pub fn telegram_default() -> AudioResult<Self> {
        Self::new(CaptureConfig::default())
    }

    /// Configures automatic downmixing and resampling for arbitrary hardware input
    /// sample rates and channel counts (e.g. 44.1 kHz stereo mic → 48 kHz mono).
    pub fn configure_input_format(&mut self, input_sample_rate: u32, input_channels: usize) {
        self.input_channels = input_channels.max(1);
        if input_sample_rate != self.config.sample_rate {
            self.input_resampler = Some(Resampler::new(
                input_sample_rate,
                self.config.sample_rate,
                1,
            ));
        } else {
            self.input_resampler = None;
        }
    }

    /// Returns whether capture is currently paused.
    #[must_use]
    pub const fn is_paused(&self) -> bool {
        self.paused
    }

    /// Returns whether capture has reached max duration or been finalized.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        self.finished
    }

    /// Total encoded + buffered samples at the capture sample rate.
    #[must_use]
    pub fn total_samples(&self) -> i64 {
        self.full_samples + self.captured.len() as i64
    }

    /// Current duration in milliseconds.
    #[must_use]
    pub fn duration_ms(&self) -> i64 {
        (self.total_samples() * 1000) / i64::from(self.config.sample_rate)
    }

    /// Pauses or resumes capture. When pausing, returns a snapshot of the
    /// current waveform and duration (matching `Instance::Inner::pause`).
    pub fn set_paused(&mut self, paused: bool) -> Option<CaptureResult> {
        self.paused = paused;
        if !paused {
            return None;
        }
        let waveform = if self.full_samples > 0 {
            self.waveform.collect()
        } else {
            Vec::new()
        };
        let waveform_5bit = encode_5bit(&waveform);
        let bytes = self
            .writer
            .as_ref()
            .and_then(|w| w.get_ref())
            .filter(|_| self.full_samples > 0)
            .cloned()
            .unwrap_or_default();
        Some(CaptureResult {
            bytes,
            waveform,
            waveform_5bit,
            duration_ms: (self.full_samples * 1000) / i64::from(self.config.sample_rate),
        })
    }

    /// Feeds interleaved `f32` PCM from an input device (downmixing & resampling if configured).
    pub fn push_f32(&mut self, samples: &[f32]) -> AudioResult<Option<CaptureUpdate>> {
        if self.paused || self.finished || samples.is_empty() {
            return Ok(None);
        }
        let mono = to_mono_f32(samples, self.input_channels);
        let resampled = if let Some(resampler) = &mut self.input_resampler {
            let mut out = Vec::new();
            resampler.process(&mono, &mut out);
            out
        } else {
            mono
        };
        let pcm_i16 = f32_to_i16(&resampled);
        self.push_mono_i16(&pcm_i16)
    }

    /// Feeds mono 16-bit PCM at `config.sample_rate` Hz.
    ///
    /// Exact port of `Instance::Inner::process` (`media_audio_capture.cpp:641-701`).
    pub fn push_mono_i16(&mut self, samples: &[i16]) -> AudioResult<Option<CaptureUpdate>> {
        if self.paused || self.finished || samples.is_empty() {
            return Ok(None);
        }

        let max_samples = self.config.max_duration_secs * i64::from(self.config.sample_rate);
        let remaining_allowed = (max_samples - self.total_samples()).max(0) as usize;
        let take_count = samples.len().min(remaining_allowed);
        let hit_limit =
            take_count < samples.len() || (self.total_samples() + take_count as i64 >= max_samples);

        let slice = &samples[..take_count];
        let prev_len = self.captured.len();
        self.captured.extend_from_slice(slice);

        // Level metering (media_audio_capture.cpp:663-685).
        let skip_samples =
            (self.config.skip_duration_ms * i64::from(self.config.sample_rate)) / 1000;
        let fade_samples =
            (self.config.fade_duration_ms * i64::from(self.config.sample_rate)) / 1000;
        for (offset, &sample) in slice.iter().enumerate() {
            let level_index = self.full_samples + prev_len as i64 + offset as i64;
            if level_index > skip_samples {
                let mut value = read_one_sample_i16(sample);
                if fade_samples > 0 && level_index < skip_samples + fade_samples {
                    let faded = f64::from(value) * ((level_index - skip_samples) as f64)
                        / (fade_samples as f64);
                    value = faded.round().clamp(0.0, f64::from(u16::MAX)) as u16;
                }
                if self.level_max < value {
                    self.level_max = value;
                }
            }
        }

        let samples_full = self.full_samples + self.captured.len() as i64;
        let update_threshold =
            (self.config.update_delta_ms * i64::from(self.config.sample_rate)) / 1000;
        let update = if hit_limit || (samples_full - self.last_update_samples > update_threshold) {
            let u = CaptureUpdate {
                samples: samples_full,
                level: self.level_max,
                finished: hit_limit,
            };
            self.last_update_samples = samples_full;
            self.level_max = 0;
            Some(u)
        } else {
            None
        };

        // Keep at least `fade_samples` buffered so stop() can apply the 300ms tail fade-out!
        // Matches `while (_captured.size() >= encoded + framesize + fadeSamples * sizeof(short))`.
        let fade_tail = fade_samples.max(0) as usize;
        let mut encoded = 0usize;
        while self.captured.len() >= encoded + self.frame_samples + fade_tail {
            self.process_frame(encoded)?;
            encoded += self.frame_samples;
        }
        if encoded > 0 {
            self.captured.drain(..encoded);
        }

        if hit_limit {
            self.finished = true;
        }
        Ok(update)
    }

    /// Processes and encodes one `frame_samples`-sized mono frame starting at `offset`.
    ///
    /// Exact port of `Instance::Inner::processFrame` (`media_audio_capture.cpp:706-796`).
    fn process_frame(&mut self, offset: usize) -> AudioResult<()> {
        let samples_cnt = self.frame_samples;
        let skip_samples =
            (self.config.skip_duration_ms * i64::from(self.config.sample_rate)) / 1000;
        let fade_samples =
            (self.config.fade_duration_ms * i64::from(self.config.sample_rate)) / 1000;

        let frame = &mut self.captured[offset..offset + samples_cnt];

        // Apply initial 400ms mute skip + 300ms linear fade-in (lines 725-741).
        if self.full_samples < skip_samples + fade_samples {
            let faded_cnt = (samples_cnt as i64)
                .min(skip_samples + fade_samples - self.full_samples)
                .max(0) as usize;
            let zero_cnt =
                (samples_cnt as i64).min((skip_samples - self.full_samples).max(0)) as usize;
            let coef = if fade_samples > 0 {
                1.0 / (fade_samples as f64)
            } else {
                1.0
            };
            let mut faded_from = (self.full_samples - skip_samples) as f64;

            for sample in frame.iter_mut().take(zero_cnt) {
                *sample = 0;
                faded_from += 1.0;
            }
            for sample in frame.iter_mut().take(faded_cnt).skip(zero_cnt) {
                let scaled = (faded_from * coef * f64::from(*sample)).round();
                *sample = scaled.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
                faded_from += 1.0;
            }
        }

        // Accumulate waveform peaks every 480 samples (lines 743-754).
        self.waveform.push_samples(frame);

        // Encode Opus packet and write into Ogg container.
        let n = self
            .encoder
            .encode_s16(frame, samples_cnt, &mut self.packet_buf)?;
        if let Some(writer) = &mut self.writer {
            writer.write_packet(&self.packet_buf[..n])?;
        }
        self.full_samples += samples_cnt as i64;
        Ok(())
    }

    /// Finalizes the recording: applies 300 ms tail fade-out, pads final partial
    /// frame, flushes encoder lookahead, writes the Ogg EOS page, and computes
    /// the 100-sample 5-bit waveform.
    ///
    /// Exact port of `Instance::Inner::stop` (`media_audio_capture.cpp:501-618`).
    pub fn finish(mut self) -> AudioResult<CaptureResult> {
        self.finished = true;
        let fade_samples = ((self.config.fade_duration_ms * i64::from(self.config.sample_rate))
            / 1000)
            .max(0) as usize;
        let captured_samples = self.captured.len();
        let total_samples = (self.full_samples as usize).saturating_add(captured_samples);

        if captured_samples == 0
            || total_samples < self.config.min_samples
            || captured_samples < fade_samples
        {
            return Ok(CaptureResult {
                bytes: Vec::new(),
                waveform: Vec::new(),
                waveform_5bit: Vec::new(),
                duration_ms: 0,
            });
        }

        // Apply linear fade-out over the last `fade_samples` (lines 513-517).
        if fade_samples > 0 {
            let coef = 1.0 / (fade_samples as f64);
            for i in 0..fade_samples {
                let idx = captured_samples - 1 - i;
                let faded = ((i as f64) * coef * f64::from(self.captured[idx])).round();
                self.captured[idx] = faded.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
            }
        }

        // Pad to a whole multiple of `frame_samples` (lines 518-522).
        let rem = self.captured.len() % self.frame_samples;
        if rem != 0 {
            self.captured
                .resize(self.captured.len() + (self.frame_samples - rem), 0);
        }

        let mut encoded = 0usize;
        while encoded + self.frame_samples <= self.captured.len() {
            self.process_frame(encoded)?;
            encoded += self.frame_samples;
        }

        // Flush encoder lookahead so decoding recovers the full signal cleanly.
        let lookahead_48k =
            usize::from(OpusHead::for_encoder(&self.encoder, self.config.sample_rate).pre_skip);
        if lookahead_48k > 0 {
            let silence = vec![0i16; self.frame_samples];
            let n = self
                .encoder
                .encode_s16(&silence, self.frame_samples, &mut self.packet_buf)?;
            if let Some(writer) = &mut self.writer {
                writer.write_packet_with_duration(&self.packet_buf[..n], lookahead_48k as u32)?;
            }
        }

        let bytes = match self.writer.take() {
            Some(writer) => writer.finish()?,
            None => Vec::new(),
        };
        let waveform = self.waveform.collect();
        let waveform_5bit = encode_5bit(&waveform);
        let duration_ms = (self.full_samples * 1000) / i64::from(self.config.sample_rate);

        Ok(CaptureResult {
            bytes,
            waveform,
            waveform_5bit,
            duration_ms,
        })
    }
}

/// Callback invoked with live [`CaptureUpdate`] level metering events.
pub type CaptureUpdateCallback = Arc<dyn Fn(CaptureUpdate) + Send + Sync>;

/// Live microphone voice recorder backed by `cpal` input streams and
/// [`VoiceCapturePipeline`].
pub struct AudioRecorder {
    devices: Arc<DeviceManager>,
    config: CaptureConfig,
    pipeline: Arc<Mutex<Option<VoiceCapturePipeline>>>,
    stream: Mutex<Option<cpal::Stream>>,
    recording: Arc<AtomicBool>,
}

impl AudioRecorder {
    /// Creates a new [`AudioRecorder`] using `devices` and `config`.
    #[must_use]
    pub fn new(devices: Arc<DeviceManager>, config: CaptureConfig) -> Self {
        Self {
            devices,
            config,
            pipeline: Arc::new(Mutex::new(None)),
            stream: Mutex::new(None),
            recording: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Returns `true` if a microphone capture device is available.
    #[must_use]
    pub fn available(&self) -> bool {
        self.devices.has_capture_device()
    }

    /// Returns `true` if recording is currently active.
    #[must_use]
    pub fn is_recording(&self) -> bool {
        self.recording.load(Ordering::Acquire)
    }

    /// Starts capturing audio from the resolved capture device.
    pub fn start(&self, on_update: Option<CaptureUpdateCallback>) -> AudioResult<()> {
        if self.is_recording() {
            return Err(AudioError::InvalidOperation(
                "recording is already in progress".into(),
            ));
        }

        let device = self.devices.open_cpal_device(DeviceType::Capture)?;
        let default_cfg = device
            .default_input_config()
            .map_err(|e| AudioError::Device(e.to_string()))?;
        let sample_format = default_cfg.sample_format();
        let stream_cfg: cpal::StreamConfig = default_cfg.into();
        let in_rate = stream_cfg.sample_rate;
        let in_channels = usize::from(stream_cfg.channels);

        let mut pipe = VoiceCapturePipeline::new(self.config)?;
        pipe.configure_input_format(in_rate, in_channels);
        {
            let mut guard = self
                .pipeline
                .lock()
                .map_err(|_| AudioError::Device("capture pipeline lock poisoned".into()))?;
            *guard = Some(pipe);
        }

        let pipe_ref = Arc::clone(&self.pipeline);
        let rec_flag = Arc::clone(&self.recording);

        let err_fn = |err: cpal::Error| {
            tracing::error!("audio capture stream error: {err}");
        };

        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                let on_upd = on_update.clone();
                device.build_input_stream(
                    stream_cfg,
                    move |data: &[f32], _| {
                        handle_input_f32(&pipe_ref, &rec_flag, data, &on_upd);
                    },
                    err_fn,
                    None,
                )
            }
            cpal::SampleFormat::I16 => {
                let on_upd = on_update.clone();
                device.build_input_stream(
                    stream_cfg,
                    move |data: &[i16], _| {
                        let floats: Vec<f32> =
                            data.iter().map(|&s| f32::from(s) / 32768.0).collect();
                        handle_input_f32(&pipe_ref, &rec_flag, &floats, &on_upd);
                    },
                    err_fn,
                    None,
                )
            }
            cpal::SampleFormat::U16 => {
                let on_upd = on_update;
                device.build_input_stream(
                    stream_cfg,
                    move |data: &[u16], _| {
                        let floats: Vec<f32> = data
                            .iter()
                            .map(|&s| (f32::from(s) - 32768.0) / 32768.0)
                            .collect();
                        handle_input_f32(&pipe_ref, &rec_flag, &floats, &on_upd);
                    },
                    err_fn,
                    None,
                )
            }
            other => {
                return Err(AudioError::UnsupportedFormat(format!(
                    "unsupported cpal capture sample format: {other:?}"
                )));
            }
        }
        .map_err(|e| AudioError::Device(e.to_string()))?;

        stream
            .play()
            .map_err(|e| AudioError::Device(e.to_string()))?;

        {
            let mut sguard = self
                .stream
                .lock()
                .map_err(|_| AudioError::Device("capture stream lock poisoned".into()))?;
            *sguard = Some(stream);
        }
        self.recording.store(true, Ordering::Release);
        Ok(())
    }

    /// Pauses or resumes active capture. When pausing, returns a snapshot [`CaptureResult`].
    pub fn pause(&self, paused: bool) -> AudioResult<Option<CaptureResult>> {
        let mut guard = self
            .pipeline
            .lock()
            .map_err(|_| AudioError::Device("capture pipeline lock poisoned".into()))?;
        Ok(guard.as_mut().and_then(|p| p.set_paused(paused)))
    }

    /// Stops capturing and finalizes the Ogg Opus voice recording.
    pub fn stop(&self) -> AudioResult<CaptureResult> {
        self.recording.store(false, Ordering::Release);
        if let Ok(mut sguard) = self.stream.lock() {
            let _ = sguard.take();
        }
        let pipe = {
            let mut guard = self
                .pipeline
                .lock()
                .map_err(|_| AudioError::Device("capture pipeline lock poisoned".into()))?;
            guard.take()
        };
        match pipe {
            Some(p) => p.finish(),
            None => Ok(CaptureResult {
                bytes: Vec::new(),
                waveform: Vec::new(),
                waveform_5bit: Vec::new(),
                duration_ms: 0,
            }),
        }
    }

    /// Cancels an active recording and discards captured data.
    pub fn cancel(&self) {
        self.recording.store(false, Ordering::Release);
        if let Ok(mut sguard) = self.stream.lock() {
            let _ = sguard.take();
        }
        if let Ok(mut guard) = self.pipeline.lock() {
            let _ = guard.take();
        }
    }
}

fn handle_input_f32(
    pipe_ref: &Arc<Mutex<Option<VoiceCapturePipeline>>>,
    rec_flag: &Arc<AtomicBool>,
    data: &[f32],
    on_update: &Option<CaptureUpdateCallback>,
) {
    if !rec_flag.load(Ordering::Acquire) {
        return;
    }
    let update = {
        let Ok(mut guard) = pipe_ref.lock() else {
            return;
        };
        let Some(pipe) = guard.as_mut() else {
            return;
        };
        match pipe.push_f32(data) {
            Ok(upd) => upd,
            Err(e) => {
                tracing::error!("capture encode error: {e}");
                rec_flag.store(false, Ordering::Release);
                None
            }
        }
    };
    if let (Some(u), Some(cb)) = (update, on_update) {
        cb(u);
    }
}
