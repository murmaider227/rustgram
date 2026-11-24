//! Audio output engine bridging [`MixerCore`] to `cpal` hardware playback streams.
//!
//! Handles device-rate resampling (from the mixer's 48 kHz internal rate to the
//! OS output device's native sample rate), channel mapping (mono / stereo /
//! multi-channel), sample format conversion (`f32`, `i16`, `u16`), event
//! forwarding, and device switching / default-device following.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, StreamTrait};

use crate::device::{DeviceManager, DeviceResolvedId, DeviceType};
use crate::dsp::Resampler;
use crate::error::{AudioError, AudioResult};
use crate::mixer::{MixerCore, MixerEvent};
use crate::types::DEFAULT_FREQUENCY;

/// Callback invoked whenever [`MixerCore`] produces events on the audio callback.
pub type MixerEventCallback = Arc<dyn Fn(Vec<MixerEvent>) + Send + Sync>;

/// Output audio engine driving a `cpal::Stream` from a shared [`MixerCore`].
pub struct AudioOutputEngine {
    devices: Arc<DeviceManager>,
    mixer: Arc<Mutex<MixerCore>>,
    stream: Mutex<Option<ActiveStream>>,
    running: Arc<AtomicBool>,
    event_cb: Mutex<Option<MixerEventCallback>>,
}

struct ActiveStream {
    _stream: cpal::Stream,
    device_id: DeviceResolvedId,
    sample_rate: u32,
    channels: usize,
}

impl AudioOutputEngine {
    /// Creates a new [`AudioOutputEngine`] bound to `devices` and `mixer`.
    #[must_use]
    pub fn new(devices: Arc<DeviceManager>, mixer: Arc<Mutex<MixerCore>>) -> Self {
        Self {
            devices,
            mixer,
            stream: Mutex::new(None),
            running: Arc::new(AtomicBool::new(false)),
            event_cb: Mutex::new(None),
        }
    }

    /// Returns a reference to the shared [`MixerCore`].
    #[must_use]
    pub fn mixer(&self) -> &Arc<Mutex<MixerCore>> {
        &self.mixer
    }

    /// Returns a reference to the [`DeviceManager`].
    #[must_use]
    pub fn devices(&self) -> &Arc<DeviceManager> {
        &self.devices
    }

    /// Installs or clears a callback for [`MixerEvent`] batches emitted during playback.
    pub fn set_event_callback(&self, cb: Option<MixerEventCallback>) {
        if let Ok(mut guard) = self.event_cb.lock() {
            *guard = cb;
        }
    }

    /// Returns `true` if a hardware output stream is currently attached and running.
    #[must_use]
    pub fn is_attached(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Returns the currently attached device's `(sample_rate, channels)`, if attached.
    #[must_use]
    pub fn active_format(&self) -> Option<(u32, usize)> {
        self.stream
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| (s.sample_rate, s.channels)))
    }

    /// Ensures the output stream is open on the currently resolved playback device.
    pub fn ensure_attached(&self) -> AudioResult<()> {
        if self.is_attached() {
            return Ok(());
        }
        self.attach_to_resolved_device()
    }

    /// Closes the current output stream (if any) and re-opens it on the newly
    /// resolved playback device (e.g. after switching output devices or OS default change).
    pub fn switch_device(&self, preferred_id: impl Into<String>) -> AudioResult<()> {
        self.devices
            .set_preferred_device(DeviceType::Playback, preferred_id)?;
        self.detach();
        self.attach_to_resolved_device()
    }

    /// Checks whether the OS default or preferred playback device changed, and
    /// re-attaches seamlessly if so (`DetachIfDeviceChanged` in `media_audio.cpp:1407`).
    pub fn reattach_if_device_changed(&self) -> AudioResult<bool> {
        let changed = self.devices.refresh(DeviceType::Playback)?;
        let current_attached = self
            .stream
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| s.device_id.clone()));

        if let Some(new_id) = changed
            && current_attached.as_ref() != Some(&new_id)
            && self.is_attached()
        {
            self.detach();
            self.attach_to_resolved_device()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Detaches from the audio output device if the mixer has been idle past
    /// [`crate::types::DETACH_DEVICE_TIMEOUT_MS`].
    pub fn detach_if_idle(&self) -> bool {
        let should = self
            .mixer
            .lock()
            .map(|m| m.should_detach_device())
            .unwrap_or(false);
        if should && self.is_attached() {
            self.detach();
            return true;
        }
        false
    }

    /// Closes and drops the active `cpal` output stream.
    pub fn detach(&self) {
        self.running.store(false, Ordering::Release);
        if let Ok(mut guard) = self.stream.lock() {
            let _ = guard.take();
        }
    }

    fn attach_to_resolved_device(&self) -> AudioResult<()> {
        let device = self.devices.open_cpal_device(DeviceType::Playback)?;
        let resolved_id = self.devices.resolved(DeviceType::Playback);
        let default_cfg = device
            .default_output_config()
            .map_err(|e| AudioError::Device(e.to_string()))?;
        let sample_format = default_cfg.sample_format();
        let stream_cfg: cpal::StreamConfig = default_cfg.into();
        let out_rate = stream_cfg.sample_rate;
        let out_channels = usize::from(stream_cfg.channels);

        let mixer = Arc::clone(&self.mixer);
        let cb_slot = self.event_cb.lock().ok().and_then(|g| g.clone());
        let running = Arc::clone(&self.running);

        let mut renderer = StreamRenderer::new(out_rate, out_channels);
        let err_fn = move |err: cpal::Error| {
            tracing::error!("audio playback stream error: {err}");
        };

        let stream = match sample_format {
            cpal::SampleFormat::F32 => device.build_output_stream(
                stream_cfg,
                move |data: &mut [f32], _| {
                    if !running.load(Ordering::Acquire) {
                        data.fill(0.0);
                        return;
                    }
                    renderer.fill_f32(&mixer, &cb_slot, data);
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_output_stream(
                stream_cfg,
                move |data: &mut [i16], _| {
                    if !running.load(Ordering::Acquire) {
                        data.fill(0);
                        return;
                    }
                    let mut tmp = vec![0.0f32; data.len()];
                    renderer.fill_f32(&mixer, &cb_slot, &mut tmp);
                    for (dst, &src) in data.iter_mut().zip(tmp.iter()) {
                        *dst = (src.clamp(-1.0, 1.0) * 32767.0).round() as i16;
                    }
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_output_stream(
                stream_cfg,
                move |data: &mut [u16], _| {
                    if !running.load(Ordering::Acquire) {
                        data.fill(u16::MAX / 2);
                        return;
                    }
                    let mut tmp = vec![0.0f32; data.len()];
                    renderer.fill_f32(&mixer, &cb_slot, &mut tmp);
                    for (dst, &src) in data.iter_mut().zip(tmp.iter()) {
                        *dst = ((src.clamp(-1.0, 1.0) * 0.5 + 0.5) * 65535.0).round() as u16;
                    }
                },
                err_fn,
                None,
            ),
            other => {
                return Err(AudioError::UnsupportedFormat(format!(
                    "unsupported cpal playback sample format: {other:?}"
                )));
            }
        }
        .map_err(|e| AudioError::Device(e.to_string()))?;

        self.running.store(true, Ordering::Release);
        stream
            .play()
            .map_err(|e| AudioError::Device(e.to_string()))?;

        if let Ok(mut guard) = self.stream.lock() {
            *guard = Some(ActiveStream {
                _stream: stream,
                device_id: resolved_id,
                sample_rate: out_rate,
                channels: out_channels,
            });
        }
        Ok(())
    }
}

struct StreamRenderer {
    out_rate: u32,
    out_channels: usize,
    resampler: Option<Resampler>,
    mixer_buf: Vec<f32>,
    resampled_buf: Vec<f32>,
    resampled_offset: usize,
}

impl StreamRenderer {
    fn new(out_rate: u32, out_channels: usize) -> Self {
        let resampler = if out_rate != DEFAULT_FREQUENCY {
            Some(Resampler::new(DEFAULT_FREQUENCY, out_rate, 2))
        } else {
            None
        };
        Self {
            out_rate,
            out_channels: out_channels.max(1),
            resampler,
            mixer_buf: Vec::new(),
            resampled_buf: Vec::new(),
            resampled_offset: 0,
        }
    }

    fn fill_f32(
        &mut self,
        mixer: &Arc<Mutex<MixerCore>>,
        cb: &Option<MixerEventCallback>,
        out: &mut [f32],
    ) {
        let out_frames = out.len() / self.out_channels;
        if out_frames == 0 {
            return;
        }

        let mut all_events = Vec::new();

        if self.out_rate == DEFAULT_FREQUENCY {
            self.mixer_buf.resize(out_frames * 2, 0.0);
            if let Ok(mut m) = mixer.lock() {
                all_events = m.render_stereo_48k(&mut self.mixer_buf);
            } else {
                self.mixer_buf.fill(0.0);
            }
            map_stereo_to_channels(&self.mixer_buf, out, self.out_channels);
        } else {
            // Pull enough 48 kHz stereo frames through the output resampler to satisfy `out_frames`.
            let needed_stereo = out_frames * 2;
            while self.resampled_buf.len() - self.resampled_offset < needed_stereo {
                if self.resampled_offset > 0 {
                    self.resampled_buf.drain(..self.resampled_offset);
                    self.resampled_offset = 0;
                }
                let pull_frames = 512usize;
                self.mixer_buf.resize(pull_frames * 2, 0.0);
                if let Ok(mut m) = mixer.lock() {
                    let ev = m.render_stereo_48k(&mut self.mixer_buf);
                    all_events.extend(ev);
                } else {
                    self.mixer_buf.fill(0.0);
                }
                if let Some(resampler) = &mut self.resampler {
                    resampler.process(&self.mixer_buf, &mut self.resampled_buf);
                }
            }
            let slice =
                &self.resampled_buf[self.resampled_offset..self.resampled_offset + needed_stereo];
            map_stereo_to_channels(slice, out, self.out_channels);
            self.resampled_offset += needed_stereo;
            if self.resampled_offset >= 4096 {
                self.resampled_buf.drain(..self.resampled_offset);
                self.resampled_offset = 0;
            }
        }

        if !all_events.is_empty()
            && let Some(callback) = cb
        {
            callback(all_events);
        }
    }
}

fn map_stereo_to_channels(stereo: &[f32], out: &mut [f32], out_channels: usize) {
    let frames = (stereo.len() / 2).min(out.len() / out_channels);
    match out_channels {
        1 => {
            for f in 0..frames {
                out[f] = 0.5 * (stereo[f * 2] + stereo[f * 2 + 1]);
            }
        }
        2 => {
            out[..frames * 2].copy_from_slice(&stereo[..frames * 2]);
        }
        ch => {
            out.fill(0.0);
            for f in 0..frames {
                out[f * ch] = stereo[f * 2];
                out[f * ch + 1] = stereo[f * 2 + 1];
            }
        }
    }
}
