//! Real-time full-duplex `cpal` WASAPI audio capture and playback plus a lock-free
//! virtual audio backend for deterministic loopback tests.
//!
//! Reuses `tg_audio::device::{DeviceManager, DeviceType}` for device enumeration and
//! `"default"` sentinel hot-swap resolution, and `tg_audio::dsp::{Resampler, to_mono_f32}`
//! for arbitrary hardware sample-rate/channel conversion to/from 48 kHz mono.
//!
//! Real-time `cpal` callbacks communicate with the media loop exclusively via lock-free
//! SPSC ring buffers (`ringbuf::HeapRb<f32>`) with zero heap allocations inside callbacks.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use cpal::traits::{DeviceTrait, StreamTrait};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use tg_audio::device::{DEFAULT_DEVICE_ID, DeviceInfo, DeviceManager, DeviceType};
use tg_audio::dsp::{Resampler, soft_clip, to_mono_f32};

use crate::error::{VoipError, VoipResult};
use crate::jitter::{VOIP_FRAME_SAMPLES, VOIP_SAMPLE_RATE};

/// Ring buffer capacity in 48 kHz mono samples (~200 ms = 9600 samples).
const RING_CAPACITY_SAMPLES: usize = VOIP_FRAME_SAMPLES * 10;

/// Returns `true` when `RUSTGRAM_HEADLESS_AUDIO` (or legacy `TGFORK_HEADLESS_AUDIO`) is set
/// (non-empty and not `"0"`), instructing `tg-voip` not to open real OS audio hardware.
#[must_use]
pub fn is_headless_audio() -> bool {
    std::env::var("RUSTGRAM_HEADLESS_AUDIO")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var("TGFORK_HEADLESS_AUDIO").ok())
        .is_some_and(|v| !v.is_empty() && v != "0")
}

/// Selection between live `cpal` OS audio devices and a headless virtual audio pump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioBackendKind {
    /// Real OS audio hardware via `cpal` (WASAPI on Windows).
    #[default]
    Hardware,
    /// Headless in-memory ring buffers (for unit/integration loopback tests).
    Virtual,
}

/// Shared handle to a [`AudioBackendKind::Virtual`] endpoint's PCM streams.
///
/// Tests feed synthetic microphone audio into `push_capture_pcm` and read decoded
/// speaker audio out of `drain_playback_pcm`.
pub struct VirtualAudioHandle {
    mic_prod: std::sync::Mutex<HeapProd<f32>>,
    spk_cons: std::sync::Mutex<HeapCons<f32>>,
}

impl VirtualAudioHandle {
    /// Push 48 kHz mono `f32` PCM samples as virtual microphone input.
    pub fn push_capture_pcm(&self, samples: &[f32]) -> usize {
        if let Ok(mut guard) = self.mic_prod.lock() {
            guard.push_slice(samples)
        } else {
            0
        }
    }

    /// Drain all available decoded 48 kHz mono `f32` speaker output samples.
    pub fn drain_playback_pcm(&self) -> Vec<f32> {
        let mut out = Vec::new();
        if let Ok(mut guard) = self.spk_cons.lock() {
            let avail = guard.occupied_len();
            if avail > 0 {
                out.resize(avail, 0.0);
                let n = guard.pop_slice(&mut out);
                out.truncate(n);
            }
        }
        out
    }
}

/// Full-duplex 48 kHz mono audio I/O bridge used by [`crate::MediaEngine`].
pub struct VoipAudioIo {
    backend: AudioBackendKind,
    devices: Arc<DeviceManager>,
    running: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
    input_volume_bits: Arc<AtomicU32>,
    output_volume_bits: Arc<AtomicU32>,
    capture_stream: Option<cpal::Stream>,
    playback_stream: Option<cpal::Stream>,
    capture_cons: HeapCons<f32>,
    playback_prod: HeapProd<f32>,
    input_device_id: String,
    output_device_id: String,
}

impl VoipAudioIo {
    /// Open a hardware `cpal` capture + playback bridge (falls back to a headless virtual bridge
    /// when `RUSTGRAM_HEADLESS_AUDIO=1` is set, or returns an error if the OS has no devices).
    pub fn open_hardware(input_device_id: &str, output_device_id: &str) -> VoipResult<Self> {
        if is_headless_audio() {
            let (mut io, _handle) = Self::open_virtual(RING_CAPACITY_SAMPLES);
            io.input_device_id = input_device_id.to_string();
            io.output_device_id = output_device_id.to_string();
            return Ok(io);
        }

        let devices = DeviceManager::shared();
        let _ = devices.set_preferred_device(DeviceType::Capture, input_device_id);
        let _ = devices.set_preferred_device(DeviceType::Playback, output_device_id);

        let running = Arc::new(AtomicBool::new(true));
        let muted = Arc::new(AtomicBool::new(false));
        let input_volume_bits = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let output_volume_bits = Arc::new(AtomicU32::new(1.0f32.to_bits()));

        let (cap_prod, cap_cons) = HeapRb::<f32>::new(RING_CAPACITY_SAMPLES).split();
        let (play_prod, play_cons) = HeapRb::<f32>::new(RING_CAPACITY_SAMPLES).split();

        let capture_stream = build_capture_stream(
            &devices,
            cap_prod,
            Arc::clone(&running),
            Arc::clone(&muted),
            Arc::clone(&input_volume_bits),
        )?;
        let playback_stream = build_playback_stream(
            &devices,
            play_cons,
            Arc::clone(&running),
            Arc::clone(&output_volume_bits),
        )?;

        Ok(Self {
            backend: AudioBackendKind::Hardware,
            devices,
            running,
            muted,
            input_volume_bits,
            output_volume_bits,
            capture_stream: Some(capture_stream),
            playback_stream: Some(playback_stream),
            capture_cons: cap_cons,
            playback_prod: play_prod,
            input_device_id: input_device_id.to_string(),
            output_device_id: output_device_id.to_string(),
        })
    }

    /// Create a headless virtual audio I/O bridge paired with a [`VirtualAudioHandle`].
    #[must_use]
    pub fn open_virtual(capacity_samples: usize) -> (Self, Arc<VirtualAudioHandle>) {
        let cap = capacity_samples.max(RING_CAPACITY_SAMPLES);
        let (mic_prod, cap_cons) = HeapRb::<f32>::new(cap).split();
        let (play_prod, spk_cons) = HeapRb::<f32>::new(cap).split();

        let io = Self {
            backend: AudioBackendKind::Virtual,
            devices: DeviceManager::shared(),
            running: Arc::new(AtomicBool::new(true)),
            muted: Arc::new(AtomicBool::new(false)),
            input_volume_bits: Arc::new(AtomicU32::new(1.0f32.to_bits())),
            output_volume_bits: Arc::new(AtomicU32::new(1.0f32.to_bits())),
            capture_stream: None,
            playback_stream: None,
            capture_cons: cap_cons,
            playback_prod: play_prod,
            input_device_id: "virtual".to_string(),
            output_device_id: "virtual".to_string(),
        };
        let handle = Arc::new(VirtualAudioHandle {
            mic_prod: std::sync::Mutex::new(mic_prod),
            spk_cons: std::sync::Mutex::new(spk_cons),
        });
        (io, handle)
    }

    /// Enumerate available OS audio devices (`DeviceType::Capture` or `DeviceType::Playback`).
    ///
    /// When `RUSTGRAM_HEADLESS_AUDIO=1` is set, returns deterministic headless virtual devices
    /// without querying OS audio hardware.
    pub fn list_devices(device_type: DeviceType) -> VoipResult<Vec<DeviceInfo>> {
        if is_headless_audio() {
            let (default_name, alt_id, alt_name) = match device_type {
                DeviceType::Capture => (
                    "Headless Virtual Microphone",
                    "headless-capture-2",
                    "Headless Headset Microphone",
                ),
                DeviceType::Playback => (
                    "Headless Virtual Speakers",
                    "headless-playback-2",
                    "Headless Headphones",
                ),
            };
            return Ok(vec![
                DeviceInfo {
                    id: DEFAULT_DEVICE_ID.to_string(),
                    name: default_name.to_string(),
                    device_type,
                    is_default: true,
                    inactive: false,
                },
                DeviceInfo {
                    id: alt_id.to_string(),
                    name: alt_name.to_string(),
                    device_type,
                    is_default: false,
                    inactive: false,
                },
            ]);
        }
        let mgr = DeviceManager::new();
        mgr.devices(device_type)
            .map_err(|e| VoipError::AudioIo(e.to_string()))
    }

    /// Active audio backend kind.
    #[must_use]
    pub const fn backend(&self) -> AudioBackendKind {
        self.backend
    }

    /// Active capture device identifier.
    #[must_use]
    pub fn input_device_id(&self) -> &str {
        &self.input_device_id
    }

    /// Active playback device identifier.
    #[must_use]
    pub fn output_device_id(&self) -> &str {
        &self.output_device_id
    }

    /// Mute or unmute microphone capture.
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Release);
    }

    /// Whether capture is currently muted.
    #[must_use]
    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Acquire)
    }

    /// Set microphone input gain (`0.0..=2.0`).
    pub fn set_input_volume(&self, volume: f32) {
        self.input_volume_bits
            .store(volume.clamp(0.0, 2.0).to_bits(), Ordering::Release);
    }

    /// Set speaker output gain (`0.0..=2.0`).
    pub fn set_output_volume(&self, volume: f32) {
        self.output_volume_bits
            .store(volume.clamp(0.0, 2.0).to_bits(), Ordering::Release);
    }

    /// Hot-swap the capture device (`"default"` follows the OS default).
    pub fn set_input_device(&mut self, device_id: &str) -> VoipResult<()> {
        self.input_device_id = device_id.to_string();
        if self.backend != AudioBackendKind::Hardware || is_headless_audio() {
            return Ok(());
        }
        self.devices
            .set_preferred_device(DeviceType::Capture, device_id)
            .map_err(|e| VoipError::AudioIo(e.to_string()))?;
        self.capture_stream = None;
        let (cap_prod, cap_cons) = HeapRb::<f32>::new(RING_CAPACITY_SAMPLES).split();
        self.capture_cons = cap_cons;
        self.capture_stream = Some(build_capture_stream(
            &self.devices,
            cap_prod,
            Arc::clone(&self.running),
            Arc::clone(&self.muted),
            Arc::clone(&self.input_volume_bits),
        )?);
        Ok(())
    }

    /// Hot-swap the playback device (`"default"` follows the OS default).
    pub fn set_output_device(&mut self, device_id: &str) -> VoipResult<()> {
        self.output_device_id = device_id.to_string();
        if self.backend != AudioBackendKind::Hardware || is_headless_audio() {
            return Ok(());
        }
        self.devices
            .set_preferred_device(DeviceType::Playback, device_id)
            .map_err(|e| VoipError::AudioIo(e.to_string()))?;
        self.playback_stream = None;
        let (play_prod, play_cons) = HeapRb::<f32>::new(RING_CAPACITY_SAMPLES).split();
        self.playback_prod = play_prod;
        self.playback_stream = Some(build_playback_stream(
            &self.devices,
            play_cons,
            Arc::clone(&self.running),
            Arc::clone(&self.output_volume_bits),
        )?);
        Ok(())
    }

    /// Try to read a complete 20 ms (`960`-sample) 48 kHz mono captured audio frame.
    ///
    /// Returns `true` if a full frame was populated (with mute/input gain applied).
    pub fn try_pop_capture_frame(&mut self, out: &mut [f32; VOIP_FRAME_SAMPLES]) -> bool {
        if self.capture_cons.occupied_len() < VOIP_FRAME_SAMPLES {
            return false;
        }
        let read = self.capture_cons.pop_slice(out);
        if read < VOIP_FRAME_SAMPLES {
            out[read..].fill(0.0);
        }
        if self.backend == AudioBackendKind::Virtual {
            if self.is_muted() {
                out.fill(0.0);
            } else {
                let gain = f32::from_bits(self.input_volume_bits.load(Ordering::Acquire));
                if (gain - 1.0).abs() > 1e-4 {
                    for s in out.iter_mut() {
                        *s = soft_clip(*s * gain);
                    }
                }
            }
        }
        true
    }

    /// Push a decoded 20 ms (`960`-sample) 48 kHz mono audio frame to the playback ring buffer.
    pub fn push_playback_frame(&mut self, frame: &[f32; VOIP_FRAME_SAMPLES]) -> usize {
        if self.backend == AudioBackendKind::Virtual {
            let gain = f32::from_bits(self.output_volume_bits.load(Ordering::Acquire));
            if (gain - 1.0).abs() > 1e-4 {
                let mut scaled = *frame;
                for s in &mut scaled {
                    *s = soft_clip(*s * gain);
                }
                return self.playback_prod.push_slice(&scaled);
            }
        }
        self.playback_prod.push_slice(frame)
    }

    /// Stop and drop active `cpal` streams.
    pub fn stop(&mut self) {
        self.running.store(false, Ordering::Release);
        self.capture_stream = None;
        self.playback_stream = None;
    }
}

impl Drop for VoipAudioIo {
    fn drop(&mut self) {
        self.stop();
    }
}

fn build_capture_stream(
    devices: &DeviceManager,
    mut prod: HeapProd<f32>,
    running: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
    gain_bits: Arc<AtomicU32>,
) -> VoipResult<cpal::Stream> {
    let device = devices
        .open_cpal_device(DeviceType::Capture)
        .map_err(|e| VoipError::AudioIo(e.to_string()))?;
    let default_cfg = device
        .default_input_config()
        .map_err(|e| VoipError::AudioIo(e.to_string()))?;
    let fmt = default_cfg.sample_format();
    let cfg: cpal::StreamConfig = default_cfg.into();
    let in_rate = cfg.sample_rate;
    let in_channels = usize::from(cfg.channels);

    // Pre-allocate callback scratch buffers so the real-time callback does zero allocations.
    let mut resampler =
        (in_rate != VOIP_SAMPLE_RATE).then(|| Resampler::new(in_rate, VOIP_SAMPLE_RATE, 1));
    let mut mono_buf = Vec::with_capacity(4096);
    let mut resampled_buf = Vec::with_capacity(4096);

    let err_fn = |err: cpal::Error| {
        tracing::error!("voip capture stream error: {err}");
    };

    let mut push_f32_interleaved = move |samples: &[f32]| {
        if !running.load(Ordering::Acquire) {
            return;
        }
        mono_buf.clear();
        if muted.load(Ordering::Acquire) {
            let frames = samples.len() / in_channels.max(1);
            let out_frames = if in_rate == VOIP_SAMPLE_RATE {
                frames
            } else {
                ((frames as u64 * u64::from(VOIP_SAMPLE_RATE)) / u64::from(in_rate.max(1))) as usize
            };
            mono_buf.resize(out_frames, 0.0);
            let _ = prod.push_slice(&mono_buf);
            return;
        }

        let gain = f32::from_bits(gain_bits.load(Ordering::Acquire));
        if in_channels <= 1 {
            for &s in samples {
                mono_buf.push(soft_clip(s * gain));
            }
        } else {
            let down = to_mono_f32(samples, in_channels);
            for s in down {
                mono_buf.push(soft_clip(s * gain));
            }
        }

        if let Some(rs) = &mut resampler {
            resampled_buf.clear();
            rs.process(&mono_buf, &mut resampled_buf);
            let _ = prod.push_slice(&resampled_buf);
        } else {
            let _ = prod.push_slice(&mono_buf);
        }
    };

    let stream = match fmt {
        cpal::SampleFormat::F32 => device.build_input_stream(
            cfg,
            move |data: &[f32], _| push_f32_interleaved(data),
            err_fn,
            None,
        ),
        cpal::SampleFormat::I16 => {
            let mut conv = Vec::with_capacity(4096);
            device.build_input_stream(
                cfg,
                move |data: &[i16], _| {
                    conv.clear();
                    conv.extend(data.iter().map(|&s| f32::from(s) / 32768.0));
                    push_f32_interleaved(&conv);
                },
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let mut conv = Vec::with_capacity(4096);
            device.build_input_stream(
                cfg,
                move |data: &[u16], _| {
                    conv.clear();
                    conv.extend(data.iter().map(|&s| (f32::from(s) / 32767.5) - 1.0));
                    push_f32_interleaved(&conv);
                },
                err_fn,
                None,
            )
        }
        other => {
            return Err(VoipError::AudioIo(format!(
                "unsupported capture sample format: {other:?}"
            )));
        }
    }
    .map_err(|e| VoipError::AudioIo(e.to_string()))?;

    stream
        .play()
        .map_err(|e| VoipError::AudioIo(e.to_string()))?;
    Ok(stream)
}

fn build_playback_stream(
    devices: &DeviceManager,
    mut cons: HeapCons<f32>,
    running: Arc<AtomicBool>,
    gain_bits: Arc<AtomicU32>,
) -> VoipResult<cpal::Stream> {
    let device = devices
        .open_cpal_device(DeviceType::Playback)
        .map_err(|e| VoipError::AudioIo(e.to_string()))?;
    let default_cfg = device
        .default_output_config()
        .map_err(|e| VoipError::AudioIo(e.to_string()))?;
    let fmt = default_cfg.sample_format();
    let cfg: cpal::StreamConfig = default_cfg.into();
    let out_rate = cfg.sample_rate;
    let out_channels = usize::from(cfg.channels).max(1);

    let mut resampler =
        (out_rate != VOIP_SAMPLE_RATE).then(|| Resampler::new(VOIP_SAMPLE_RATE, out_rate, 1));
    let mut mono_in = Vec::with_capacity(4096);
    let mut mono_out = Vec::with_capacity(4096);
    let mut mono_out_offset = 0usize;

    let err_fn = |err: cpal::Error| {
        tracing::error!("voip playback stream error: {err}");
    };

    let mut fill_f32 = move |out: &mut [f32]| {
        if !running.load(Ordering::Acquire) {
            out.fill(0.0);
            return;
        }
        let gain = f32::from_bits(gain_bits.load(Ordering::Acquire));
        let frames_needed = out.len() / out_channels;

        if let Some(rs) = &mut resampler {
            let mut written = 0usize;
            while written < frames_needed {
                if mono_out_offset >= mono_out.len() {
                    mono_out.clear();
                    mono_out_offset = 0;
                    mono_in.resize(VOIP_FRAME_SAMPLES, 0.0);
                    let got = cons.pop_slice(&mut mono_in);
                    if got == 0 {
                        break;
                    }
                    mono_in.truncate(got);
                    rs.process(&mono_in, &mut mono_out);
                    if mono_out.is_empty() {
                        break;
                    }
                }
                let sample = soft_clip(mono_out[mono_out_offset] * gain);
                mono_out_offset += 1;
                for ch in 0..out_channels {
                    out[written * out_channels + ch] = sample;
                }
                written += 1;
            }
            for f in written..frames_needed {
                for ch in 0..out_channels {
                    out[f * out_channels + ch] = 0.0;
                }
            }
        } else {
            mono_in.resize(frames_needed, 0.0);
            let got = cons.pop_slice(&mut mono_in);
            for (f, &s) in mono_in[..got].iter().enumerate() {
                let sample = soft_clip(s * gain);
                for ch in 0..out_channels {
                    out[f * out_channels + ch] = sample;
                }
            }
            for f in got..frames_needed {
                for ch in 0..out_channels {
                    out[f * out_channels + ch] = 0.0;
                }
            }
        }
    };

    let stream = match fmt {
        cpal::SampleFormat::F32 => {
            device.build_output_stream(cfg, move |data: &mut [f32], _| fill_f32(data), err_fn, None)
        }
        cpal::SampleFormat::I16 => {
            let mut tmp = Vec::with_capacity(4096);
            device.build_output_stream(
                cfg,
                move |data: &mut [i16], _| {
                    tmp.resize(data.len(), 0.0);
                    fill_f32(&mut tmp);
                    for (dst, &src) in data.iter_mut().zip(tmp.iter()) {
                        *dst = (src.clamp(-1.0, 1.0) * 32767.0).round() as i16;
                    }
                },
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let mut tmp = Vec::with_capacity(4096);
            device.build_output_stream(
                cfg,
                move |data: &mut [u16], _| {
                    tmp.resize(data.len(), 0.0);
                    fill_f32(&mut tmp);
                    for (dst, &src) in data.iter_mut().zip(tmp.iter()) {
                        *dst = ((src.clamp(-1.0, 1.0) * 0.5 + 0.5) * 65535.0).round() as u16;
                    }
                },
                err_fn,
                None,
            )
        }
        other => {
            return Err(VoipError::AudioIo(format!(
                "unsupported playback sample format: {other:?}"
            )));
        }
    }
    .map_err(|e| VoipError::AudioIo(e.to_string()))?;

    stream
        .play()
        .map_err(|e| VoipError::AudioIo(e.to_string()))?;
    Ok(stream)
}
