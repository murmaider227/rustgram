//! Audio waveform 5-bit packing/unpacking, waveform peak conversion, and voice duration helpers.
//!
//! Port of `Telegram/SourceFiles/data/data_document.cpp` (`documentWaveformDecode`,
//! `documentWaveformEncode5bit`), `Telegram/SourceFiles/media/audio/media_audio.cpp`
//! (`FFMpegWaveformCounter`), `Telegram/SourceFiles/media/audio/media_audio_capture.cpp`
//! (`CollectWaveform`), and `Telegram/SourceFiles/ui/text/format_values.cpp`.

/// Target number of bars in a Telegram voice waveform (`Media::Player::kWaveformSamplesCount`).
pub const WAVEFORM_SAMPLES_COUNT: usize = 100;

/// Maximum amplitude level representable in 5-bit Telegram waveforms (`0..=31`).
pub const WAVEFORM_MAX_VALUE: u8 = 31;

/// Minimum normalization floor used by tdesktop (`std::max(int32(sum * 1.8 / peaks.size()), 2500)`).
pub const WAVEFORM_NORM_FLOOR: i64 = 2500;

/// Decodes a Telegram 5-bit packed waveform byte vector into amplitude values in `0..=31`.
///
/// Exact port of `documentWaveformDecode` (`data/data_document.cpp:1411`).
/// Values are packed little-endian across byte boundaries (`bit_index = i * 5`).
#[must_use]
pub fn waveform_decode_5bit(encoded5bit: &[u8]) -> Vec<u8> {
    let bits_count = encoded5bit.len() * 8;
    let values_count = bits_count / 5;
    if values_count == 0 {
        return Vec::new();
    }

    let mut result = vec![0u8; values_count];
    for (i, slot) in result.iter_mut().enumerate() {
        let byte_index = (i * 5) / 8;
        let bit_shift = (i * 5) % 8;
        let lo = u16::from(encoded5bit[byte_index]);
        let hi = encoded5bit
            .get(byte_index + 1)
            .map_or(0u16, |&b| u16::from(b));
        let word = lo | (hi << 8);
        *slot = ((word >> bit_shift) & 0x1F) as u8;
    }
    result
}

/// Encodes amplitude values (each masked to 5 bits, `0..=31`) into Telegram's 5-bit packed format.
///
/// Exact port of `documentWaveformEncode5bit` (`data/data_document.cpp:1444`).
#[must_use]
pub fn waveform_encode_5bit(waveform: &[u8]) -> Vec<u8> {
    if waveform.is_empty() {
        return Vec::new();
    }
    let bits_count = waveform.len() * 5;
    let bytes_count = bits_count.div_ceil(8);
    // Reserve 1 extra scratch byte to simplify 16-bit writes, then truncate.
    let mut result = vec![0u8; bytes_count + 1];

    for (i, &sample) in waveform.iter().enumerate() {
        let byte_index = (i * 5) / 8;
        let bit_shift = (i * 5) % 8;
        let value = (u16::from(sample) & 0x1F) << bit_shift;
        let prev = u16::from_le_bytes([result[byte_index], result[byte_index + 1]]);
        let next = (prev | value).to_le_bytes();
        result[byte_index] = next[0];
        result[byte_index + 1] = next[1];
    }

    result.truncate(bytes_count);
    result
}

/// Converts raw signed 16-bit PCM audio samples into a 100-bar `0..=31` Telegram waveform.
///
/// Mirrors `FFMpegWaveformCounter` in `media/audio/media_audio.cpp:1553-1625`:
/// takes the peak `|sample|` in each of [`WAVEFORM_SAMPLES_COUNT`] buckets, then normalizes
/// against `max(floor(mean(peaks) * 1.8), 2500)` into `0..=31`.
#[must_use]
pub fn convert_waveform_from_pcm_i16(samples: &[i16]) -> Vec<u8> {
    let count = samples.len();
    if count < WAVEFORM_SAMPLES_COUNT {
        return Vec::new();
    }

    let mut peaks: Vec<u16> = Vec::with_capacity(WAVEFORM_SAMPLES_COUNT);
    let mut peak = 0u16;
    let mut acc = 0usize;
    for &s in samples {
        let mag = s.unsigned_abs();
        if mag > peak {
            peak = mag;
        }
        acc += WAVEFORM_SAMPLES_COUNT;
        if acc >= count {
            acc -= count;
            peaks.push(peak);
            peak = 0;
        }
    }
    if acc > 0 && peaks.len() < WAVEFORM_SAMPLES_COUNT {
        peaks.push(peak);
    }

    normalize_waveform_peaks(&peaks)
}

/// Converts captured `u8` amplitude envelope values (scaled by 256) into a 100-bar `0..=31` waveform.
///
/// Exact port of `CollectWaveform` (`media/audio/media_audio_capture.cpp:42-79`).
#[must_use]
pub fn convert_waveform_from_u8_peaks(envelope: &[u8]) -> Vec<u8> {
    let count = envelope.len();
    if count < WAVEFORM_SAMPLES_COUNT {
        return Vec::new();
    }

    let mut peaks: Vec<u16> = Vec::with_capacity(WAVEFORM_SAMPLES_COUNT);
    let mut peak = 0u16;
    let mut acc = 0usize;
    for &v in envelope {
        let sample = u16::from(v) * 256;
        if sample > peak {
            peak = sample;
        }
        acc += WAVEFORM_SAMPLES_COUNT;
        if acc >= count {
            acc -= count;
            peaks.push(peak);
            peak = 0;
        }
    }

    normalize_waveform_peaks(&peaks)
}

/// Normalizes raw `u16` peaks to `0..=31` using tdesktop's exact `1.8x mean / 2500 floor` rule.
#[must_use]
pub fn normalize_waveform_peaks(peaks: &[u16]) -> Vec<u8> {
    if peaks.is_empty() {
        return Vec::new();
    }
    let sum: i64 = peaks.iter().map(|&p| i64::from(p)).sum();
    let mean_scaled = ((sum as f64) * 1.8 / (peaks.len() as f64)) as i64;
    let norm_peak = mean_scaled.max(WAVEFORM_NORM_FLOOR) as u32;

    peaks
        .iter()
        .map(|&p| {
            let clamped = u32::from(p).min(norm_peak);
            ((clamped * u32::from(WAVEFORM_MAX_VALUE)) / norm_peak)
                .min(u32::from(WAVEFORM_MAX_VALUE)) as u8
        })
        .collect()
}

/// Resamples a decoded waveform to `target_bars` bars (taking max within each bucket when
/// downsampling, or repeating nearest bar when upsampling).
#[must_use]
pub fn resample_waveform(waveform: &[u8], target_bars: usize) -> Vec<u8> {
    if waveform.is_empty() || target_bars == 0 {
        return Vec::new();
    }
    if waveform.len() == target_bars {
        return waveform.to_vec();
    }
    let mut out = Vec::with_capacity(target_bars);
    let n = waveform.len();
    for i in 0..target_bars {
        let start = (i * n) / target_bars;
        let end = (((i + 1) * n) / target_bars).max(start + 1).min(n);
        let peak = waveform[start..end]
            .iter()
            .copied()
            .max()
            .unwrap_or(0)
            .min(WAVEFORM_MAX_VALUE);
        out.push(peak);
    }
    out
}

// ============================================================================
// Voice message duration & formatting helpers
// ============================================================================

/// Converts a floating-point duration in seconds to milliseconds with safe rounding,
/// mirroring `MillisecondsFromSeconds` (`data/data_document.cpp:94`).
#[must_use]
pub fn milliseconds_from_seconds(seconds: f64) -> i64 {
    let ms = seconds * 1000.0;
    if ms.is_finite() && ms.abs() < (i64::MAX as f64) {
        ms.round() as i64
    } else {
        0
    }
}

/// Formats a duration in seconds as `MM:SS` or `H:MM:SS`,
/// exact match to `Ui::FormatDurationText` (`ui/text/format_values.cpp:135`).
#[must_use]
pub fn format_duration_text(duration_secs: i64) -> String {
    let clamped = duration_secs.max(0);
    let hours = clamped / 3600;
    let minutes = (clamped % 3600) / 60;
    let seconds = clamped % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// Formats playback progress `"MM:SS / MM:SS"` (`Ui::FormatPlayedText`).
#[must_use]
pub fn format_played_text(played_secs: i64, duration_secs: i64) -> String {
    format!(
        "{} / {}",
        format_duration_text(played_secs),
        format_duration_text(duration_secs)
    )
}

/// Voice message playback / progress helper struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceDuration {
    /// Total voice message duration in milliseconds.
    pub total_ms: u64,
}

impl VoiceDuration {
    /// Creates a `VoiceDuration` from whole seconds (as returned in `documentAttributeAudio.duration`).
    #[must_use]
    pub const fn from_secs(seconds: u32) -> Self {
        Self {
            total_ms: (seconds as u64) * 1000,
        }
    }

    /// Creates a `VoiceDuration` from milliseconds.
    #[must_use]
    pub const fn from_millis(total_ms: u64) -> Self {
        Self { total_ms }
    }

    /// Total whole seconds (rounded down).
    #[must_use]
    pub const fn total_secs(self) -> u64 {
        self.total_ms / 1000
    }

    /// Computes normalized playback progress in `0.0..=1.0` for `position_ms`.
    #[must_use]
    pub fn progress(self, position_ms: u64) -> f64 {
        if self.total_ms == 0 {
            return 0.0;
        }
        (position_ms as f64 / self.total_ms as f64).clamp(0.0, 1.0)
    }

    /// Returns the number of active waveform bars out of `total_bars` for `position_ms`.
    #[must_use]
    pub fn active_bars(self, position_ms: u64, total_bars: usize) -> usize {
        if self.total_ms == 0 || total_bars == 0 {
            return 0;
        }
        ((self.progress(position_ms) * (total_bars as f64)).round() as usize).min(total_bars)
    }

    /// Formats the duration using [`format_duration_text`].
    #[must_use]
    pub fn format(self) -> String {
        format_duration_text(self.total_secs() as i64)
    }
}
