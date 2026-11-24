//! Digital signal processing primitives: resampling, channel conversion, and
//! pitch-preserving time stretching (WSOLA).
//!
//! Replaces FFmpeg's `libswresample` and `libavfilter` (`atempo`) used in
//! `media/audio/media_audio_ffmpeg_loader.cpp`.

use std::collections::VecDeque;

use crate::types::{clamp_speed, equal_speeds};

/// Converts interleaved multi-channel `f32` PCM into stereo (`2` channels).
#[must_use]
pub fn to_stereo_f32(samples: &[f32], channels: usize) -> Vec<f32> {
    match channels {
        0 => Vec::new(),
        1 => {
            let mut out = Vec::with_capacity(samples.len() * 2);
            for &s in samples {
                out.push(s);
                out.push(s);
            }
            out
        }
        2 => samples.to_vec(),
        n => {
            let frames = samples.len() / n;
            let mut out = Vec::with_capacity(frames * 2);
            for frame in samples.chunks_exact(n) {
                // Standard front-left / front-right fold-down with center if present.
                if n >= 3 {
                    let center = frame[2] * std::f32::consts::FRAC_1_SQRT_2;
                    out.push((frame[0] + center).clamp(-1.0, 1.0));
                    out.push((frame[1] + center).clamp(-1.0, 1.0));
                } else {
                    out.push(frame[0]);
                    out.push(frame[1]);
                }
            }
            out
        }
    }
}

/// Converts interleaved multi-channel `f32` PCM into mono (`1` channel).
#[must_use]
pub fn to_mono_f32(samples: &[f32], channels: usize) -> Vec<f32> {
    match channels {
        0 => Vec::new(),
        1 => samples.to_vec(),
        n => {
            let frames = samples.len() / n;
            let inv = 1.0 / (n as f32);
            let mut out = Vec::with_capacity(frames);
            for frame in samples.chunks_exact(n) {
                let sum: f32 = frame.iter().copied().sum();
                out.push(sum * inv);
            }
            out
        }
    }
}

/// Converts normalized `f32` samples (`-1.0..=1.0`) to signed 16-bit PCM.
#[must_use]
pub fn f32_to_i16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|&s| {
            let clamped = s.clamp(-1.0, 1.0);
            if clamped <= -1.0 {
                i16::MIN
            } else {
                (clamped * 32767.0).round() as i16
            }
        })
        .collect()
}

/// Converts signed 16-bit PCM samples to normalized `f32` (`-1.0..=1.0`).
#[must_use]
pub fn i16_to_f32(samples: &[i16]) -> Vec<f32> {
    samples.iter().map(|&s| f32::from(s) / 32768.0).collect()
}

/// Soft-clips an audio sample into `[-1.0, 1.0]` smoothly above `0.95`.
#[inline]
#[must_use]
pub fn soft_clip(x: f32) -> f32 {
    if x > 0.95 {
        0.95 + 0.05 * ((x - 0.95) / 0.05).tanh()
    } else if x < -0.95 {
        -0.95 + 0.05 * ((x + 0.95) / 0.05).tanh()
    } else {
        x
    }
}

/// Streaming stateful sample-rate converter for interleaved `f32` audio.
///
/// Uses cubic Hermite (Catmull-Rom) interpolation for clean, low-aliasing
/// arbitrary-ratio resampling (e.g. 44.1 kHz → 48 kHz, 16 kHz → 48 kHz, etc.).
#[derive(Debug, Clone)]
pub struct Resampler {
    in_rate: u32,
    out_rate: u32,
    channels: usize,
    /// Fractional read position in input frames relative to `history[1]`.
    phase: f64,
    /// Step in input frames per output frame (`in_rate / out_rate`).
    step: f64,
    /// Last 3 input frames kept across `process()` calls for cubic interpolation.
    history: Vec<f32>,
    has_started: bool,
}

impl Resampler {
    /// Creates a new resampler for `channels`-channel interleaved `f32` audio.
    #[must_use]
    pub fn new(in_rate: u32, out_rate: u32, channels: usize) -> Self {
        let in_rate = in_rate.max(1);
        let out_rate = out_rate.max(1);
        let channels = channels.max(1);
        Self {
            in_rate,
            out_rate,
            channels,
            phase: 0.0,
            step: f64::from(in_rate) / f64::from(out_rate),
            history: vec![0.0; 3 * channels],
            has_started: false,
        }
    }

    /// Input sample rate in Hz.
    #[must_use]
    pub const fn in_rate(&self) -> u32 {
        self.in_rate
    }

    /// Output sample rate in Hz.
    #[must_use]
    pub const fn out_rate(&self) -> u32 {
        self.out_rate
    }

    /// Channel count.
    #[must_use]
    pub const fn channels(&self) -> usize {
        self.channels
    }

    /// Resets internal filter history (e.g. after seeking).
    pub fn reset(&mut self) {
        self.phase = 0.0;
        self.history.fill(0.0);
        self.has_started = false;
    }

    /// Resamples interleaved input frames into `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let ch = self.channels;
        let in_frames = input.len() / ch;
        if in_frames == 0 {
            return;
        }
        if self.in_rate == self.out_rate {
            out.extend_from_slice(&input[..in_frames * ch]);
            return;
        }

        if !self.has_started {
            // Seed history frames with first input frame to avoid a startup click.
            for h in 0..3 {
                self.history[h * ch..(h + 1) * ch].copy_from_slice(&input[..ch]);
            }
            self.has_started = true;
        }

        let estimated = ((in_frames as f64) / self.step).ceil() as usize + 4;
        out.reserve(estimated * ch);

        // Virtual buffer: indices 0, 1, 2 come from `self.history`;
        // index 3..3+in_frames come from `input`.
        // We interpolate around `pos = self.phase + 1.0`, needing `idx - 1`, `idx`, `idx + 1`, `idx + 2`.
        // So we can advance while `floor(pos) + 2 < 3 + in_frames`, i.e. `floor(self.phase) < in_frames`.
        let sample_at = |frame_idx: usize, c: usize, history: &[f32], input: &[f32]| -> f32 {
            if frame_idx < 3 {
                history[frame_idx * ch + c]
            } else {
                input[(frame_idx - 3) * ch + c]
            }
        };

        while (self.phase.floor() as usize) < in_frames {
            let base = self.phase.floor() as usize;
            let t = (self.phase - (base as f64)) as f32;
            let idx = base + 1; // >= 1, and idx + 2 = base + 3 <= in_frames + 2 < 3 + in_frames
            for c in 0..ch {
                let y0 = sample_at(idx - 1, c, &self.history, input);
                let y1 = sample_at(idx, c, &self.history, input);
                let y2 = sample_at(idx + 1, c, &self.history, input);
                let y3 = sample_at(idx + 2, c, &self.history, input);
                out.push(cubic_hermite(y0, y1, y2, y3, t));
            }
            self.phase += self.step;
        }

        self.phase -= in_frames as f64;

        // Shift the last 3 frames of the combined buffer into `self.history`.
        let total = 3 + in_frames;
        let mut new_hist = vec![0.0; 3 * ch];
        for h in 0..3 {
            let src_frame = total - 3 + h;
            for c in 0..ch {
                new_hist[h * ch + c] = sample_at(src_frame, c, &self.history, input);
            }
        }
        self.history = new_hist;
    }

    /// Convenience helper to resample an entire buffer in one call.
    #[must_use]
    pub fn resample_all(input: &[f32], in_rate: u32, out_rate: u32, channels: usize) -> Vec<f32> {
        let mut r = Self::new(in_rate, out_rate, channels);
        let mut out = Vec::new();
        r.process(input, &mut out);
        out
    }
}

#[inline]
fn cubic_hermite(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
    let c0 = y1;
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + c0
}

/// Real-time pitch-preserving time-stretcher using WSOLA
/// (Waveform Similarity Overlap-Add).
///
/// Mirrors FFmpeg's `atempo` filter used in `media_audio_ffmpeg_loader.cpp`:
/// changes playback tempo in `[0.5, 2.5]` without altering pitch.
#[derive(Debug, Clone)]
pub struct TimeStretcher {
    sample_rate: u32,
    channels: usize,
    speed: f64,
    /// Analysis/synthesis window size in frames (~40 ms at 48 kHz = 1920 frames).
    window_frames: usize,
    /// Synthesis hop size (`window_frames / 2`).
    synthesis_hop: usize,
    /// Search tolerance radius in frames (~10 ms).
    search_radius: usize,
    /// Precomputed Hann window coefficients of length `window_frames`.
    hann: Vec<f32>,
    /// Queued interleaved input samples awaiting WSOLA analysis.
    input_buf: VecDeque<f32>,
    /// Previous overlap-add tail (`synthesis_hop * channels` samples).
    overlap_buf: Vec<f32>,
    /// Ideal reference template for WSOLA correlation (`synthesis_hop * channels` samples).
    natural_next: Vec<f32>,
    /// Fractional analysis read offset accumulator.
    analysis_offset: f64,
    /// Whether the first frame has been seeded.
    seeded: bool,
}

impl TimeStretcher {
    /// Creates a new WSOLA time-stretcher at `sample_rate` Hz and `channels` channels.
    #[must_use]
    pub fn new(sample_rate: u32, channels: usize, speed: f64) -> Self {
        let sample_rate = sample_rate.max(8_000);
        let channels = channels.max(1);
        // 40 ms window, 20 ms hop (50% Hann overlap sums identically to 1.0!).
        let mut window_frames = ((u64::from(sample_rate) * 40) / 1000) as usize;
        if !window_frames.is_multiple_of(2) {
            window_frames += 1;
        }
        window_frames = window_frames.max(64);
        let synthesis_hop = window_frames / 2;
        let search_radius = ((u64::from(sample_rate) * 10) / 1000).max(16) as usize;

        // Periodic Hann window: w[i] + w[i + N/2] == 1.0 exactly.
        let hann: Vec<f32> = (0..window_frames)
            .map(|i| {
                let angle = (2.0 * std::f64::consts::PI * (i as f64)) / (window_frames as f64);
                (0.5 * (1.0 - angle.cos())) as f32
            })
            .collect();

        Self {
            sample_rate,
            channels,
            speed: clamp_speed(speed),
            window_frames,
            synthesis_hop,
            search_radius,
            hann,
            input_buf: VecDeque::new(),
            overlap_buf: vec![0.0; synthesis_hop * channels],
            natural_next: vec![0.0; synthesis_hop * channels],
            analysis_offset: 0.0,
            seeded: false,
        }
    }

    /// Returns the current playback speed multiplier.
    #[must_use]
    pub const fn speed(&self) -> f64 {
        self.speed
    }

    /// Updates playback speed cleanly (clamped to `[SPEED_MIN, SPEED_MAX]`).
    pub fn set_speed(&mut self, speed: f64) {
        let clamped = clamp_speed(speed);
        if !equal_speeds(self.speed, clamped) {
            self.speed = clamped;
        }
    }

    /// Resets internal buffers (e.g. after seeking).
    pub fn reset(&mut self) {
        self.input_buf.clear();
        self.overlap_buf.fill(0.0);
        self.natural_next.fill(0.0);
        self.analysis_offset = 0.0;
        self.seeded = false;
    }

    /// Pushes interleaved input samples and appends time-stretched output samples to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if equal_speeds(self.speed, 1.0) && self.input_buf.is_empty() && !self.seeded {
            out.extend_from_slice(input);
            return;
        }

        self.input_buf.extend(input.iter().copied());
        self.drain_ready(out);
    }

    /// Flushes any remaining queued audio at end-of-stream.
    pub fn flush(&mut self, out: &mut Vec<f32>) {
        let ch = self.channels;
        if equal_speeds(self.speed, 1.0) && !self.seeded {
            out.extend(self.input_buf.drain(..));
            return;
        }

        // Pad enough silence so the remaining real frames get synthesized smoothly.
        let remaining_frames = self.input_buf.len() / ch;
        if remaining_frames > 0 {
            let expected_out_frames = ((remaining_frames as f64) / self.speed).ceil() as usize;
            let pad_frames = self.window_frames + self.search_radius * 2 + 16;
            self.input_buf
                .resize(self.input_buf.len() + pad_frames * ch, 0.0);
            let before = out.len();
            self.drain_ready(out);
            // Trim trailing padding beyond expected length.
            let max_len = before + (expected_out_frames + self.synthesis_hop) * ch;
            if out.len() > max_len {
                out.truncate(max_len);
            }
        } else if self.seeded {
            out.extend_from_slice(&self.overlap_buf);
        }
        self.reset();
    }

    fn drain_ready(&mut self, out: &mut Vec<f32>) {
        let ch = self.channels;
        let hop = self.synthesis_hop;
        let win = self.window_frames;
        let radius = self.search_radius;

        if !self.seeded {
            if self.input_buf.len() < (win + hop) * ch {
                return;
            }
            // Output the first `hop` frames directly (fade-in not needed at t=0),
            // and store the second half weighted by the falling Hann half as `overlap_buf`.
            out.reserve(hop * ch);
            for f in 0..hop {
                for c in 0..ch {
                    out.push(self.input_buf[f * ch + c]);
                }
            }
            for f in 0..hop {
                let w = self.hann[hop + f];
                for c in 0..ch {
                    let s = self.input_buf[(hop + f) * ch + c];
                    self.overlap_buf[f * ch + c] = s * w;
                    self.natural_next[f * ch + c] = s;
                }
            }
            self.analysis_offset = (hop as f64) * self.speed;
            self.seeded = true;
        }

        loop {
            let target = self.analysis_offset.round() as usize;
            let search_start = target.saturating_sub(radius);
            let search_end = target + radius;
            let needed_frames = search_end + win + hop;
            if self.input_buf.len() / ch < needed_frames {
                break;
            }

            // Make a contiguous slice view for fast correlation search.
            let slice = self.input_buf.make_contiguous();

            // Find best alignment offset `best` in `[search_start, search_end]` maximizing
            // normalized cross-correlation with `self.natural_next` over `hop` frames.
            // Step by 2 samples on the coarse pass, then refine ±2 for speed + precision.
            let best =
                find_best_offset(slice, &self.natural_next, ch, hop, search_start, search_end);

            // Overlap-add: first half of selected window (rising Hann) + `overlap_buf`.
            out.reserve(hop * ch);
            for f in 0..hop {
                let w = self.hann[f];
                let src_base = (best + f) * ch;
                let ov_base = f * ch;
                for c in 0..ch {
                    out.push(self.overlap_buf[ov_base + c] + slice[src_base + c] * w);
                }
            }

            // Prepare next overlap tail (falling Hann) and natural continuation template.
            for f in 0..hop {
                let w = self.hann[hop + f];
                let src_base = (best + hop + f) * ch;
                let ov_base = f * ch;
                for c in 0..ch {
                    let s = slice[src_base + c];
                    self.overlap_buf[ov_base + c] = s * w;
                    self.natural_next[ov_base + c] = s;
                }
            }

            // Advance analysis pointer by `hop * speed` and discard consumed input prefix.
            self.analysis_offset += (hop as f64) * self.speed;
            let drop_frames = (self.analysis_offset.floor() as usize).saturating_sub(radius);
            if drop_frames > 0 {
                self.input_buf.drain(..drop_frames * ch);
                self.analysis_offset -= drop_frames as f64;
            }
        }
    }

    /// Returns the configured sample rate in Hz.
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

fn find_best_offset(
    buf: &[f32],
    template: &[f32],
    channels: usize,
    hop: usize,
    start: usize,
    end: usize,
) -> usize {
    // Downsample correlation stride to ~4 samples for fast real-time search.
    let frame_stride = 4usize.min(hop.max(1));
    let mut best_offset = start;
    let mut best_score = f32::NEG_INFINITY;

    let eval = |offset: usize| -> f32 {
        let mut dot = 0.0f32;
        let mut energy = 1e-12f32;
        let mut f = 0;
        while f < hop {
            let a = buf[(offset + f) * channels];
            let b = template[f * channels];
            dot += a * b;
            energy += a * a;
            f += frame_stride;
        }
        dot / energy.sqrt()
    };

    // Coarse search with step 2.
    let mut off = start;
    while off <= end {
        let score = eval(off);
        if score > best_score {
            best_score = score;
            best_offset = off;
        }
        off += 2;
    }

    // Fine refinement around coarse peak.
    let fine_start = best_offset.saturating_sub(2).max(start);
    let fine_end = (best_offset + 2).min(end);
    for cand in fine_start..=fine_end {
        let mut dot = 0.0f32;
        let mut energy = 1e-12f32;
        for f in (0..hop).step_by(2) {
            let a = buf[(cand + f) * channels];
            let b = template[f * channels];
            dot += a * b;
            energy += a * a;
        }
        let score = dot / energy.sqrt();
        if score > best_score {
            best_score = score;
            best_offset = cand;
        }
    }

    best_offset
}
