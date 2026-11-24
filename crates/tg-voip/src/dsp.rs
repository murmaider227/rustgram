//! Full-duplex 48 kHz voice DSP chain (WebRTC AEC3 + NS + AGC2 + HPF) and Opus voice encoder.
//!
//! Uses `aec3` (pure-Rust WebRTC Echo Canceller 3, Noise Suppressor, Gain Controller 2,
//! High-Pass Filter) and `opus-pure` (RFC 6716 Opus, matching `crates/tg-audio/src/opus_codec.rs`).
//!
//! Reference: `tgcalls/Instance.h` (`Config::enableAEC`, `enableNS`, `enableAGC`),
//! `crates/tg-calls/docs/media-engine.md` §3.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use aec3::graph::NodeControlState;
use aec3::nodes::audio::AudioFormat;
use aec3::pipelines::linear;
use opus_pure::{Application, MAX_PACKET_BYTES, OpusEncoder, Signal};

use crate::error::{VoipError, VoipResult};
use crate::jitter::{VOIP_FRAME_SAMPLES, VOIP_SAMPLE_RATE};

static ACTIVE_DSP_THREADS: AtomicUsize = AtomicUsize::new(0);

/// Number of currently running `tg-voip-dsp` worker OS threads in this process.
#[must_use]
pub fn active_dsp_thread_count() -> usize {
    ACTIVE_DSP_THREADS.load(Ordering::Acquire)
}

struct DspThreadCounterGuard;

impl DspThreadCounterGuard {
    fn enter() -> Self {
        ACTIVE_DSP_THREADS.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for DspThreadCounterGuard {
    fn drop(&mut self) {
        ACTIVE_DSP_THREADS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// 10 ms frame size at 48 kHz mono required by WebRTC AEC3 (`480` samples).
pub const DSP_TEN_MS_SAMPLES: usize = 480;

/// Default initial Opus voice bitrate for 1:1 calls (`32_000` bps, matching `InstanceV2Impl.cpp`).
pub const DEFAULT_VOICE_BITRATE_BPS: i32 = 32_000;

/// Configuration for the capture voice processing chain (`VoiceDsp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceDspConfig {
    /// Enable WebRTC Acoustic Echo Cancellation (`AEC3`).
    pub enable_aec: bool,
    /// Enable WebRTC Noise Suppression (`NS`).
    pub enable_ns: bool,
    /// Enable WebRTC Automatic Gain Control (`AGC2`).
    pub enable_agc: bool,
    /// Enable 80 Hz high-pass filter before AEC3.
    pub enable_hpf: bool,
    /// Initial estimated render-to-capture audio delay in milliseconds.
    pub initial_delay_ms: i32,
}

impl Default for VoiceDspConfig {
    fn default() -> Self {
        Self {
            enable_aec: true,
            enable_ns: true,
            enable_agc: true,
            enable_hpf: true,
            initial_delay_ms: 40,
        }
    }
}

impl VoiceDspConfig {
    /// Bypass all DSP stages (useful for deterministic pure-tone loopback tests).
    #[must_use]
    pub const fn bypass() -> Self {
        Self {
            enable_aec: false,
            enable_ns: false,
            enable_agc: false,
            enable_hpf: false,
            initial_delay_ms: 0,
        }
    }

    const fn any_enabled(self) -> bool {
        self.enable_aec || self.enable_ns || self.enable_agc || self.enable_hpf
    }
}

enum DspCommand {
    Render([f32; VOIP_FRAME_SAMPLES]),
    Capture {
        frame: [f32; VOIP_FRAME_SAMPLES],
        reply: mpsc::SyncSender<VoipResult<[f32; VOIP_FRAME_SAMPLES]>>,
    },
    SetDelayMs(i32),
    SetAecEnabled(bool),
    Shutdown,
}

struct DspWorker {
    tx: mpsc::Sender<DspCommand>,
    join: Option<JoinHandle<()>>,
}

impl Drop for DspWorker {
    fn drop(&mut self) {
        let _ = self.tx.send(DspCommand::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Full-duplex 48 kHz mono voice DSP processor wrapping WebRTC `aec3::pipelines::linear`.
///
/// Accepts 20 ms (`960`-sample) render and capture frames, splitting each into two
/// 10 ms (`480`-sample) WebRTC processing frames, and exposes runtime enable/bypass
/// controls plus RMS audio-level metering for UI indicators.
///
/// Because `aec3::pipelines::linear::LinearPipeline` uses internal `Rc` buffers (`!Send`),
/// an active pipeline runs on a dedicated lightweight worker thread so `VoiceDsp` is
/// safely `Send` across Tokio multi-thread tasks with zero `unsafe`.
pub struct VoiceDsp {
    config: VoiceDspConfig,
    worker: Option<DspWorker>,
}

impl VoiceDsp {
    /// Construct a new 48 kHz mono [`VoiceDsp`] processor.
    pub fn new(config: VoiceDspConfig) -> VoipResult<Self> {
        if !config.any_enabled() {
            return Ok(Self {
                config,
                worker: None,
            });
        }

        let (cmd_tx, cmd_rx) = mpsc::channel::<DspCommand>();
        let (init_tx, init_rx) = mpsc::sync_channel::<VoipResult<()>>(1);

        let join = thread::Builder::new()
            .name("tg-voip-dsp".into())
            .spawn(move || {
                let _thread_counter = DspThreadCounterGuard::enter();
                let fmt = AudioFormat::ten_ms(VOIP_SAMPLE_RATE, 1);
                let pipe_res = linear::builder(fmt, fmt)
                    .enable_high_pass_filter(config.enable_hpf)
                    .enable_noise_suppression(config.enable_ns)
                    .enable_gain_controller2(config.enable_agc)
                    .enable_post_filter(false)
                    .initial_delay_ms(config.initial_delay_ms)
                    .build()
                    .map_err(|e| VoipError::Dsp(format!("failed to build AEC3 pipeline: {e:?}")));

                let mut pipe = match pipe_res {
                    Ok(mut p) => {
                        if !config.enable_aec {
                            let aec_id = p.handles().aec3.node_id();
                            if let Err(e) = p.set_node_state(aec_id, NodeControlState::Bypassed) {
                                let _ = init_tx.send(Err(VoipError::Dsp(format!(
                                    "failed to bypass AEC3: {e:?}"
                                ))));
                                return;
                            }
                        }
                        let _ = init_tx.send(Ok(()));
                        p
                    }
                    Err(err) => {
                        let _ = init_tx.send(Err(err));
                        return;
                    }
                };

                let mut scratch_10ms = [0.0f32; DSP_TEN_MS_SAMPLES];
                while let Ok(cmd) = cmd_rx.recv() {
                    match cmd {
                        DspCommand::Render(render_20ms) => {
                            for chunk in render_20ms.as_chunks::<DSP_TEN_MS_SAMPLES>().0 {
                                let _ = pipe.handle_render_frame(chunk);
                            }
                        }
                        DspCommand::Capture { mut frame, reply } => {
                            let mut err_out = None;
                            for half in 0..2 {
                                let start = half * DSP_TEN_MS_SAMPLES;
                                let end = start + DSP_TEN_MS_SAMPLES;
                                match pipe
                                    .process_capture_frame(&frame[start..end], &mut scratch_10ms)
                                {
                                    Ok(true) => frame[start..end].copy_from_slice(&scratch_10ms),
                                    Ok(false) => {}
                                    Err(e) => {
                                        err_out =
                                            Some(VoipError::Dsp(format!("capture error: {e:?}")));
                                        break;
                                    }
                                }
                            }
                            let _ = reply.send(match err_out {
                                Some(e) => Err(e),
                                None => Ok(frame),
                            });
                        }
                        DspCommand::SetDelayMs(ms) => {
                            let _ = pipe.set_delay_ms(ms);
                        }
                        DspCommand::SetAecEnabled(enabled) => {
                            let aec_id = pipe.handles().aec3.node_id();
                            let state = if enabled {
                                NodeControlState::Active
                            } else {
                                NodeControlState::Bypassed
                            };
                            let _ = pipe.set_node_state(aec_id, state);
                        }
                        DspCommand::Shutdown => break,
                    }
                }
            })
            .map_err(|e| VoipError::Dsp(format!("failed to spawn DSP thread: {e}")))?;

        init_rx
            .recv()
            .map_err(|_| VoipError::Dsp("DSP thread terminated during init".into()))??;

        Ok(Self {
            config,
            worker: Some(DspWorker {
                tx: cmd_tx,
                join: Some(join),
            }),
        })
    }

    /// Current DSP configuration.
    #[must_use]
    pub const fn config(&self) -> VoiceDspConfig {
        self.config
    }

    /// Feed a 20 ms (`960`-sample) far-end playback reference frame into AEC3.
    pub fn feed_render_frame(&mut self, render_20ms: &[f32; VOIP_FRAME_SAMPLES]) -> VoipResult<()> {
        if let Some(worker) = &self.worker {
            worker
                .tx
                .send(DspCommand::Render(*render_20ms))
                .map_err(|_| VoipError::Dsp("DSP worker channel closed".into()))?;
        }
        Ok(())
    }

    /// Process a 20 ms (`960`-sample) microphone capture frame in place through
    /// HPF -> AEC3 -> NS -> AGC2, returning the normalized RMS audio level (`0.0..=1.0`).
    pub fn process_capture_frame(
        &mut self,
        capture_20ms: &mut [f32; VOIP_FRAME_SAMPLES],
    ) -> VoipResult<f32> {
        if let Some(worker) = &self.worker {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            worker
                .tx
                .send(DspCommand::Capture {
                    frame: *capture_20ms,
                    reply: reply_tx,
                })
                .map_err(|_| VoipError::Dsp("DSP worker channel closed".into()))?;
            let processed = reply_rx
                .recv()
                .map_err(|_| VoipError::Dsp("DSP worker dropped reply".into()))??;
            *capture_20ms = processed;
        }
        Ok(compute_rms_level(capture_20ms))
    }

    /// Update the external render-to-capture delay estimate in milliseconds.
    pub fn set_delay_ms(&mut self, delay_ms: i32) -> VoipResult<()> {
        if let Some(worker) = &self.worker {
            worker
                .tx
                .send(DspCommand::SetDelayMs(delay_ms))
                .map_err(|_| VoipError::Dsp("DSP worker channel closed".into()))?;
        }
        Ok(())
    }

    /// Enable or bypass echo cancellation dynamically (`setEchoCancellationStrength`).
    pub fn set_aec_enabled(&mut self, enabled: bool) -> VoipResult<()> {
        self.config.enable_aec = enabled;
        if let Some(worker) = &self.worker {
            worker
                .tx
                .send(DspCommand::SetAecEnabled(enabled))
                .map_err(|_| VoipError::Dsp("DSP worker channel closed".into()))?;
        }
        Ok(())
    }
}

/// Compute normalized RMS amplitude (`0.0..=1.0`) of an audio frame for level metering.
#[must_use]
pub fn compute_rms_level(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    ((sum_sq / samples.len() as f64).sqrt() as f32).clamp(0.0, 1.0)
}

/// 48 kHz mono VoIP Opus encoder wrapper around `opus-pure` (same codec crate as `tg-audio`).
pub struct VoiceOpusEncoder {
    encoder: OpusEncoder,
    packet_buf: Vec<u8>,
}

impl VoiceOpusEncoder {
    /// Create a 48 kHz mono `Application::Voip` Opus encoder configured for Telegram calls
    /// (in-band FEC enabled, adaptive bitrate, voice signal hint).
    pub fn new(bitrate_bps: i32) -> VoipResult<Self> {
        let mut encoder = OpusEncoder::new(VOIP_SAMPLE_RATE as i32, 1, Application::Voip)?;
        encoder.bitrate_bps = bitrate_bps.clamp(8_000, 64_000);
        encoder.use_inband_fec = true;
        encoder.packet_loss_perc = 5;
        encoder.use_dtx = false;
        encoder.signal_type = Some(Signal::Voice);
        Ok(Self {
            encoder,
            packet_buf: vec![0u8; MAX_PACKET_BYTES],
        })
    }

    /// Update target Opus bitrate in bits per second (`8_000..=64_000`).
    pub fn set_bitrate_bps(&mut self, bitrate_bps: i32) {
        self.encoder.bitrate_bps = bitrate_bps.clamp(8_000, 64_000);
    }

    /// Current target Opus bitrate in bits per second.
    #[must_use]
    pub const fn bitrate_bps(&self) -> i32 {
        self.encoder.bitrate_bps
    }

    /// Update expected packet loss percentage (`0..=100`) to steer Opus LBRR/FEC coding.
    pub fn set_packet_loss_perc(&mut self, perc: i32) {
        self.encoder.packet_loss_perc = perc.clamp(0, 100);
        self.encoder.use_inband_fec = perc > 0;
    }

    /// Enable or disable Discontinuous Transmission (DTX).
    pub fn set_dtx(&mut self, enabled: bool) {
        self.encoder.use_dtx = enabled;
    }

    /// Encode one 20 ms (`960`-sample) 48 kHz mono `f32` frame into an Opus packet.
    pub fn encode_frame(&mut self, pcm: &[f32; VOIP_FRAME_SAMPLES]) -> VoipResult<Vec<u8>> {
        let n = self
            .encoder
            .encode(pcm, VOIP_FRAME_SAMPLES, &mut self.packet_buf)?;
        Ok(self.packet_buf[..n].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_dsp_aec3_ns_agc2_processes_frames_cleanly() {
        let mut dsp = VoiceDsp::new(VoiceDspConfig::default()).unwrap();
        let mut render = [0.0f32; VOIP_FRAME_SAMPLES];
        let mut capture = [0.0f32; VOIP_FRAME_SAMPLES];

        for i in 0..VOIP_FRAME_SAMPLES {
            let t = i as f32 / VOIP_SAMPLE_RATE as f32;
            render[i] = (2.0 * std::f32::consts::PI * 300.0 * t).sin() * 0.25;
            capture[i] = (2.0 * std::f32::consts::PI * 600.0 * t).sin() * 0.25 + render[i] * 0.4;
        }

        // Run several frames to verify stability and finite outputs.
        for _ in 0..10 {
            let mut cap = capture;
            dsp.feed_render_frame(&render).unwrap();
            let level = dsp.process_capture_frame(&mut cap).unwrap();
            assert!(level.is_finite() && (0.0..=1.0).contains(&level));
            assert!(cap.iter().all(|s| s.is_finite()));
        }
    }

    #[test]
    fn voice_opus_encoder_encodes_48k_mono_frame() {
        let mut enc = VoiceOpusEncoder::new(DEFAULT_VOICE_BITRATE_BPS).unwrap();
        let mut pcm = [0.0f32; VOIP_FRAME_SAMPLES];
        for (i, s) in pcm.iter_mut().enumerate() {
            let t = i as f32 / VOIP_SAMPLE_RATE as f32;
            *s = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.4;
        }
        let pkt = enc.encode_frame(&pcm).unwrap();
        assert!(!pkt.is_empty());
        assert!(pkt.len() < 400);
    }
}
