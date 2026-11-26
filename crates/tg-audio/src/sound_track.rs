//! UI / notification sound track with looping, volume override, and peak envelope.
//!
//! Port of `media/audio/media_audio_track.h` and `media/audio/media_audio_track.cpp`.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::dsp::{Resampler, to_stereo_f32};
use crate::error::{AudioError, AudioResult};
use crate::types::DEFAULT_FREQUENCY;
use crate::waveform::read_one_sample_i16;

/// Maximum UI sound file size (10 MiB, `kMaxFileSize` in `media_audio_track.cpp:23`).
pub const MAX_SOUND_TRACK_FILE_SIZE: usize = 10 * 1024 * 1024;

/// Track state polling interval in ms (`kTrackUpdateTimeout` in `media_audio_track.cpp:25`).
pub const TRACK_UPDATE_TIMEOUT_MS: i64 = 100;

static NEXT_SOUND_ID: AtomicU64 = AtomicU64::new(1);

/// Unique identifier for a short UI / notification sound track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SoundTrackId(pub u64);

impl SoundTrackId {
    /// Allocates a fresh unique [`SoundTrackId`].
    #[must_use]
    pub fn next() -> Self {
        Self(NEXT_SOUND_ID.fetch_add(1, Ordering::Relaxed))
    }
}

/// Decoded short audio track for UI sounds, notification chimes, and ringtones.
///
/// Port of `Media::Audio::Track` (`media_audio_track.h` / `media_audio_track.cpp`).
/// Stores stereo 48 kHz `f32` samples ready for zero-allocation mixing in [`crate::mixer::MixerCore`],
/// plus optional normalized peak envelope values (`sample_peak_each` / `get_peak_value`).
#[derive(Debug, Clone)]
pub struct SoundTrack {
    /// Unique identifier for this sound track.
    pub id: SoundTrackId,
    /// Interleaved stereo `f32` PCM resampled to [`DEFAULT_FREQUENCY`] (48 kHz).
    samples_stereo_48k: Vec<f32>,
    /// Number of stereo frames at 48 kHz.
    frame_count: usize,
    /// Original track duration in milliseconds.
    length_ms: i64,
    /// Current playback frame cursor.
    cursor: usize,
    /// Whether this sound is currently playing.
    active: bool,
    /// Whether playback loops back to frame 0 upon reaching the end.
    looping: bool,
    /// Gain multiplier (`0.0..=1.0`).
    gain: f32,
    /// Peak-envelope sampling configuration (`samplePeakEach`).
    peak_duration_ms: i64,
    peak_each_frame: usize,
    peaks: Vec<u16>,
    peak_value_min: u16,
    peak_value_max: u16,
}

impl SoundTrack {
    /// Creates a [`SoundTrack`] from decoded interleaved `f32` PCM samples.
    pub fn from_pcm_f32(
        samples: &[f32],
        sample_rate: u32,
        channels: usize,
        peak_duration_ms: i64,
    ) -> AudioResult<Self> {
        if sample_rate == 0 || channels == 0 || samples.is_empty() {
            return Err(AudioError::InvalidOperation(
                "empty or invalid PCM buffer for SoundTrack".into(),
            ));
        }

        let stereo = to_stereo_f32(samples, channels);
        let stereo_48k = if sample_rate == DEFAULT_FREQUENCY {
            stereo
        } else {
            Resampler::resample_all(&stereo, sample_rate, DEFAULT_FREQUENCY, 2)
        };
        let frame_count = stereo_48k.len() / 2;
        let length_ms = ((frame_count as i64) * 1000) / i64::from(DEFAULT_FREQUENCY);

        let mut track = Self {
            id: SoundTrackId::next(),
            samples_stereo_48k: stereo_48k,
            frame_count,
            length_ms,
            cursor: 0,
            active: false,
            looping: false,
            gain: 1.0,
            peak_duration_ms,
            peak_each_frame: 0,
            peaks: Vec::new(),
            peak_value_min: 0x7FFF,
            peak_value_max: 0,
        };
        if peak_duration_ms > 0 {
            track.compute_peaks(peak_duration_ms);
        }
        Ok(track)
    }

    /// Computes peak amplitude buckets every `peak_duration_ms` milliseconds.
    ///
    /// Exact port of `Track::fillFromData` peak sampling (`media_audio_track.cpp:62-80, 99-105`).
    pub fn compute_peaks(&mut self, peak_duration_ms: i64) {
        self.peak_duration_ms = peak_duration_ms;
        self.peaks.clear();
        self.peak_value_min = 0x7FFF;
        self.peak_value_max = 0;

        if peak_duration_ms <= 0 || self.frame_count == 0 {
            self.peak_each_frame = 0;
            return;
        }
        let peak_each_frame =
            ((i64::from(DEFAULT_FREQUENCY) * peak_duration_ms) / 1000).max(1) as usize;
        self.peak_each_frame = peak_each_frame;
        let peak_each_sample = peak_each_frame * 2; // stereo

        let mut peak_val: u16 = 0;
        let mut count = 0usize;
        for &s in &self.samples_stereo_48k {
            let i16_sample = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            let mag = read_one_sample_i16(i16_sample);
            if mag > peak_val {
                peak_val = mag;
            }
            count += 1;
            if count >= peak_each_sample {
                count -= peak_each_sample;
                self.peaks.push(peak_val);
                if peak_val > self.peak_value_max {
                    self.peak_value_max = peak_val;
                }
                if peak_val < self.peak_value_min {
                    self.peak_value_min = peak_val;
                }
                peak_val = 0;
            }
        }
    }

    /// Starts playing once from the beginning.
    /// `volume_override` in `(0.0, 1.0]` overrides `notifications_volume_pct` (`0..=100`).
    pub fn play_once(&mut self, volume_override: Option<f64>, notifications_volume_pct: u8) {
        self.play_with_looping(false, volume_override, notifications_volume_pct);
    }

    /// Starts playing in a continuous loop (e.g. incoming call ringtone).
    pub fn play_in_loop(&mut self, volume_override: Option<f64>, notifications_volume_pct: u8) {
        self.play_with_looping(true, volume_override, notifications_volume_pct);
    }

    fn play_with_looping(
        &mut self,
        looping: bool,
        volume_override: Option<f64>,
        notifications_volume_pct: u8,
    ) {
        if self.frame_count == 0 {
            self.active = false;
            return;
        }
        self.cursor = 0;
        self.looping = looping;
        let vol = match volume_override {
            Some(v) if v > 0.0 => v.clamp(0.0, 1.0),
            _ => (f64::from(notifications_volume_pct.min(100)) / 100.0).clamp(0.0, 1.0),
        };
        self.gain = vol as f32;
        self.active = true;
    }

    /// Stops playback and resets the cursor to 0.
    pub fn stop(&mut self) {
        self.active = false;
        self.cursor = 0;
    }

    /// Returns `true` if this sound is actively playing.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// Returns `true` if looping is enabled.
    #[must_use]
    pub const fn is_looping(&self) -> bool {
        self.looping
    }

    /// Duration of the sound in milliseconds (`getLengthMs`).
    #[must_use]
    pub const fn length_ms(&self) -> i64 {
        self.length_ms
    }

    /// Current playback position in frames at 48 kHz.
    #[must_use]
    pub const fn position_frames(&self) -> usize {
        self.cursor
    }

    /// Normalized peak value in `[0.0, 1.0]` at the current cursor plus `offset_ms`.
    ///
    /// Exact port of `Track::getPeakValue` (`media_audio_track.cpp:210-221`).
    #[must_use]
    pub fn get_peak_value(&self, offset_ms: i64) -> f64 {
        if !self.active
            || self.frame_count == 0
            || self.peaks.is_empty()
            || self.peak_each_frame == 0
            || self.peak_value_min == self.peak_value_max
        {
            return 0.0;
        }
        let delta_frames = (offset_ms * i64::from(DEFAULT_FREQUENCY)) / 1000;
        let len = self.frame_count as i64;
        let sample_index = (self.cursor as i64 + delta_frames).rem_euclid(len) as usize;
        let peak_index = (sample_index / self.peak_each_frame) % self.peaks.len();
        f64::from(self.peaks[peak_index] - self.peak_value_min)
            / f64::from(self.peak_value_max - self.peak_value_min)
    }

    /// Normalized peak value in `[0.0, 1.0]` at `position_ms` of a looped playback, whether or
    /// not this copy of the track is the one playing.
    ///
    /// Same lookup as [`Self::get_peak_value`], for a caller that knows when the loop started
    /// and keeps its own decoded copy while a clone plays in the mixer.
    #[must_use]
    pub fn peak_value_at_ms(&self, position_ms: i64) -> f64 {
        if self.frame_count == 0
            || self.peaks.is_empty()
            || self.peak_each_frame == 0
            || self.peak_value_min == self.peak_value_max
        {
            return 0.0;
        }
        let frames = (position_ms * i64::from(DEFAULT_FREQUENCY)) / 1000;
        let sample_index = frames.rem_euclid(self.frame_count as i64) as usize;
        let peak_index = (sample_index / self.peak_each_frame) % self.peaks.len();
        f64::from(self.peaks[peak_index] - self.peak_value_min)
            / f64::from(self.peak_value_max - self.peak_value_min)
    }

    /// Mixes stereo frames into `out_stereo` (additive), advancing the cursor.
    /// Returns `true` if playback finished during this render call.
    pub fn mix_into(&mut self, out_stereo: &mut [f32]) -> bool {
        if !self.active || self.frame_count == 0 {
            return false;
        }
        let frames_requested = out_stereo.len() / 2;
        let mut written = 0usize;

        while written < frames_requested && self.active {
            if self.cursor >= self.frame_count {
                if self.looping {
                    self.cursor = 0;
                } else {
                    self.active = false;
                    self.cursor = 0;
                    return true;
                }
            }
            let avail = self.frame_count - self.cursor;
            let count = (frames_requested - written).min(avail);
            let src_start = self.cursor * 2;
            let dst_start = written * 2;
            for i in 0..(count * 2) {
                out_stereo[dst_start + i] += self.samples_stereo_48k[src_start + i] * self.gain;
            }
            self.cursor += count;
            written += count;
            if self.cursor >= self.frame_count && !self.looping {
                self.active = false;
                self.cursor = 0;
                return true;
            }
        }
        false
    }
}
