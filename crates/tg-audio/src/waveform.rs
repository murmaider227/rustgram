//! Voice message 100-sample waveform computation and 5-bit Telegram wire encoding.
//!
//! Port of:
//! - `media/audio/media_audio.cpp` (`FFMpegWaveformCounter`, `ReadOneSample`)
//! - `media/audio/media_audio_capture.cpp` (`CollectWaveform`, `processFrame` waveform accumulation)
//! - `data/data_document.cpp` (`documentWaveformEncode5bit`, `documentWaveformDecode`)

use crate::types::{DEFAULT_FREQUENCY, WAVEFORM_SAMPLES_COUNT};

/// Unpacked voice waveform: up to 100 bars, each in `0..=31`.
pub type VoiceWaveform = Vec<u8>;

/// Minimum normalization peak floor matching `std::max(..., 2500)` in tdesktop.
const WAVEFORM_PEAK_FLOOR: i32 = 2500;

/// Converts an unsigned 8-bit PCM sample to its unsigned 16-bit magnitude.
///
/// Port of `Media::Audio::ReadOneSample(uchar)` (`media_audio.h:403`).
#[inline]
#[must_use]
pub fn read_one_sample_u8(data: u8) -> u16 {
    ((i32::from(data) - 0x80) * 0x100).unsigned_abs() as u16
}

/// Converts a signed 16-bit PCM sample to its unsigned 16-bit magnitude.
///
/// Port of `Media::Audio::ReadOneSample(int16)` / `std::abs(int(*ptr))`
/// (`media_audio.h:407`, `media_audio_capture.cpp:745`).
#[inline]
#[must_use]
pub fn read_one_sample_i16(data: i16) -> u16 {
    i32::from(data).unsigned_abs() as u16
}

/// Normalizes raw 16-bit peak buckets into 5-bit (`0..=31`) waveform values.
///
/// Exact algorithm from `media_audio.cpp:1614-1622` and `media_audio_capture.cpp:68-76`:
/// ```text
/// sum = sum(peaks)
/// peak = max(int32(sum * 1.8 / peaks.len()), 2500)
/// result[i] = min(31, (min(peaks[i], peak) * 31) / peak)
/// ```
#[must_use]
pub fn normalize_peaks(peaks: &[u16]) -> VoiceWaveform {
    if peaks.is_empty() {
        return Vec::new();
    }
    let sum: i64 = peaks.iter().map(|&p| i64::from(p)).sum();
    let scaled = ((sum as f64) * 1.8 / (peaks.len() as f64)) as i32;
    let peak = scaled.max(WAVEFORM_PEAK_FLOOR) as u32;

    peaks
        .iter()
        .map(|&p| {
            let clamped = u32::from(p).min(peak);
            ((clamped * 31) / peak).min(31) as u8
        })
        .collect()
}

/// Computes a 100-bar voice waveform from signed 16-bit PCM samples.
///
/// Port of `FFMpegWaveformCounter::open` (`media_audio.cpp:1558-1625`).
/// `samples` may be mono or interleaved multi-channel; every sample value is
/// iterated identically to `IterateSamples<int16>`.
/// Returns an empty vector if there are fewer than `WAVEFORM_SAMPLES_COUNT`
/// sample frames.
#[must_use]
pub fn compute_waveform_from_i16(samples: &[i16], channels: usize) -> VoiceWaveform {
    let channels = channels.max(1);
    let frame_count = samples.len() / channels;
    if frame_count < WAVEFORM_SAMPLES_COUNT {
        return Vec::new();
    }

    // In C++ FFMpegWaveformCounter:
    //   countbytes = sampleSize() * samplesCount  (bytes = frame_count * channels * 2)
    //   IterateSamples<int16> invokes callback once per int16 sample (so frame_count * channels times),
    //   adding kWaveformSamplesCount to sumbytes each sample and comparing against countbytes.
    // Wait: let's check media_audio.cpp:1566-1604!
    // In C++:
    //   sampleSize() is channels * sizeof(int16) = channels * 2 bytes.
    //   samplesCount = samplesFrequency * duration / 1000.
    //   IterateSamples<int16> calls callback(sample) once per i16 sample (2 bytes),
    //   while sumbytes += 100 and compares against countbytes = channels * 2 * samplesCount.
    // Wait: notice that in C++ `FFMpegWaveformCounter`, `countbytes` is in bytes while `callback`
    // fires once per sample (every 2 bytes for 16-bit), which would only reach half the peaks for
    // 16-bit if using raw byte count; whereas in `CollectWaveform` (capture) it uses sample count
    // `count` directly so it always produces exactly 100 peaks!
    // Using `total_samples = samples.len()` produces exact 100-bucket coverage across the entire clip.
    let total_samples = samples.len() as i64;
    let target = WAVEFORM_SAMPLES_COUNT as i64;
    let mut peaks = Vec::with_capacity(WAVEFORM_SAMPLES_COUNT);
    let mut sum: i64 = 0;
    let mut peak: u16 = 0;

    for &s in samples {
        let mag = read_one_sample_i16(s);
        if mag > peak {
            peak = mag;
        }
        sum += target;
        if sum >= total_samples {
            sum -= total_samples;
            peaks.push(peak);
            peak = 0;
        }
    }
    if sum > 0 && peaks.len() < WAVEFORM_SAMPLES_COUNT {
        peaks.push(peak);
    }

    normalize_peaks(&peaks)
}

/// Computes a 100-bar voice waveform from normalized `f32` PCM samples (`-1.0..=1.0`).
#[must_use]
pub fn compute_waveform_from_f32(samples: &[f32], channels: usize) -> VoiceWaveform {
    let converted: Vec<i16> = samples
        .iter()
        .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0).round() as i16)
        .collect();
    compute_waveform_from_i16(&converted, channels)
}

/// Incremental waveform collector matching `media_audio_capture.cpp:239-242, 743-754, 42-79`.
///
/// During live voice recording at 48 kHz mono, this downsamples peaks every
/// `sample_rate / 100` samples (480 samples = 10 ms at 48 kHz) into a compact
/// `Vec<u8>`, then collapses it to 100 5-bit bars on [`Self::collect`].
#[derive(Debug, Clone)]
pub struct CaptureWaveformCollector {
    waveform_mod: i64,
    waveform_each: i64,
    waveform_peak: u16,
    bins: Vec<u8>,
}

impl CaptureWaveformCollector {
    /// Creates a collector for `sample_rate` Hz (typically [`DEFAULT_FREQUENCY`] = 48,000).
    #[must_use]
    pub fn new(sample_rate: u32) -> Self {
        let waveform_each = (i64::from(sample_rate) / 100).max(1);
        Self {
            waveform_mod: 0,
            waveform_each,
            waveform_peak: 0,
            bins: Vec::new(),
        }
    }

    /// Feeds mono 16-bit PCM samples from one capture frame (`processFrame` lines 743-754).
    pub fn push_samples(&mut self, samples: &[i16]) {
        self.bins
            .reserve((samples.len() as i64 / self.waveform_each) as usize + 1);
        for &s in samples {
            let value = read_one_sample_i16(s);
            if self.waveform_peak < value {
                self.waveform_peak = value;
            }
            self.waveform_mod += 1;
            if self.waveform_mod == self.waveform_each {
                self.waveform_mod -= self.waveform_each;
                self.bins.push((self.waveform_peak / 256) as u8);
                self.waveform_peak = 0;
            }
        }
    }

    /// Returns the number of intermediate 10ms bins collected so far.
    #[must_use]
    pub fn bin_count(&self) -> usize {
        self.bins.len()
    }

    /// Resets all accumulated state.
    pub fn clear(&mut self) {
        self.waveform_mod = 0;
        self.waveform_peak = 0;
        self.bins.clear();
    }

    /// Finalizes the collected bins into a 100-sample 5-bit [`VoiceWaveform`].
    ///
    /// Exact port of `CollectWaveform` (`media_audio_capture.cpp:42-79`).
    #[must_use]
    pub fn collect(&self) -> VoiceWaveform {
        collect_waveform_from_bins(&self.bins)
    }
}

impl Default for CaptureWaveformCollector {
    fn default() -> Self {
        Self::new(DEFAULT_FREQUENCY)
    }
}

/// Exact port of `CollectWaveform(const QVector<uchar> &waveformVector)`
/// (`media_audio_capture.cpp:42-79`).
#[must_use]
pub fn collect_waveform_from_bins(bins: &[u8]) -> VoiceWaveform {
    if bins.is_empty() {
        return Vec::new();
    }
    let count = bins.len() as i64;
    let target = WAVEFORM_SAMPLES_COUNT as i64;
    if count < target {
        return Vec::new();
    }

    let mut peaks = Vec::with_capacity(WAVEFORM_SAMPLES_COUNT);
    let mut sum: i64 = 0;
    let mut peak: u16 = 0;

    for &b in bins {
        let sample = u16::from(b) * 256;
        if peak < sample {
            peak = sample;
        }
        sum += target;
        if sum >= count {
            sum -= count;
            peaks.push(peak);
            peak = 0;
        }
    }

    normalize_peaks(&peaks)
}

/// Packs a [`VoiceWaveform`] (values `0..=31`) into Telegram's 5-bit packed wire format.
///
/// Exact port of `documentWaveformEncode5bit` (`data_document.cpp:1444-1462`).
/// For 100 samples, produces `ceil(500 / 8) = 63` bytes.
#[must_use]
pub fn encode_5bit(waveform: &[u8]) -> Vec<u8> {
    if waveform.is_empty() {
        return Vec::new();
    }
    let bits_count = waveform.len() * 5;
    let bytes_count = bits_count.div_ceil(8);
    // Reserve one extra zero byte so every 16-bit write is in-bounds, matching C++.
    let mut result = vec![0u8; bytes_count + 1];

    for (i, &sample) in waveform.iter().enumerate() {
        let byte_index = (i * 5) / 8;
        let bit_shift = (i * 5) % 8;
        let value = (u16::from(sample) & 0x1F) << bit_shift;
        let prev = u16::from_le_bytes([result[byte_index], result[byte_index + 1]]);
        let combined = (prev | value).to_le_bytes();
        result[byte_index] = combined[0];
        result[byte_index + 1] = combined[1];
    }

    result.truncate(bytes_count);
    result
}

/// Unpacks Telegram's 5-bit packed waveform bytes into individual `0..=31` values.
///
/// Exact port of `documentWaveformDecode` (`data_document.cpp:1411-1442`).
#[must_use]
pub fn decode_5bit(encoded: &[u8]) -> VoiceWaveform {
    let bits_count = encoded.len() * 8;
    let values_count = bits_count / 5;
    if values_count == 0 {
        return Vec::new();
    }

    let mut result = vec![0u8; values_count];
    for (i, out) in result.iter_mut().enumerate().take(values_count - 1) {
        let byte_index = (i * 5) / 8;
        let bit_shift = (i * 5) % 8;
        let value = u16::from_le_bytes([encoded[byte_index], encoded[byte_index + 1]]);
        *out = ((value >> bit_shift) & 0x1F) as u8;
    }

    let last_byte_index = ((values_count - 1) * 5) / 8;
    let last_bit_shift = ((values_count - 1) * 5) % 8;
    let last_value = if last_byte_index == encoded.len() - 1 {
        u16::from(encoded[last_byte_index])
    } else {
        u16::from_le_bytes([encoded[last_byte_index], encoded[last_byte_index + 1]])
    };
    result[values_count - 1] = ((last_value >> last_bit_shift) & 0x1F) as u8;

    result
}
