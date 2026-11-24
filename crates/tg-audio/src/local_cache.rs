//! Local notification sound conversion (44.1 kHz S16LE WAV, max 3 s) and
//! memory/disk caching.
//!
//! Port of `media/audio/media_audio_local_cache.h` and
//! `media/audio/media_audio_local_cache.cpp`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::decoder::AudioDecoder;
use crate::dsp::{Resampler, f32_to_i16, to_stereo_f32};
use crate::error::{AudioError, AudioResult};
use crate::mixer::AudioStream;
use crate::source::AudioInput;
use tg_data::DocumentId;

/// Maximum notification sound duration in milliseconds (`kMaxDuration = 3 * 1000`).
pub const LOCAL_SOUND_MAX_DURATION_MS: i64 = 3_000;

/// Output sample rate for cached WAV notification sounds (`rate = 44'100`).
pub const LOCAL_SOUND_SAMPLE_RATE: u32 = 44_100;

/// Decoded and truncated notification sound in RIFF WAV format (`Media::Audio::LocalSound`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalSound {
    /// Document ID (`DocumentId(0)` for the built-in default sound).
    pub id: DocumentId,
    /// RIFF WAV PCM S16LE bytes (44.1 kHz, mono or stereo, <= 3 seconds).
    pub wav: Vec<u8>,
}

impl LocalSound {
    /// Returns `true` if the sound contains valid WAV bytes.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self.wav.is_empty()
    }
}

/// Converts arbitrary supported audio bytes (Ogg Opus, MP3, FLAC, WAV, AAC, etc.)
/// into a 44.1 kHz 16-bit PCM WAV truncated to at most [`LOCAL_SOUND_MAX_DURATION_MS`] (3 seconds).
///
/// Port of `ConvertAndCut` (`media_audio_local_cache.cpp:19-286`).
pub fn convert_and_cut_to_wav(bytes: &[u8]) -> AudioResult<Vec<u8>> {
    if bytes.is_empty() {
        return Err(AudioError::InvalidOperation(
            "empty input bytes for ConvertAndCut".into(),
        ));
    }

    let input = AudioInput::from_bytes(bytes.to_vec(), None);
    let mut decoder = AudioDecoder::open(input)?;
    let in_channels = decoder.channels();
    let out_channels: usize = if in_channels == 1 { 1 } else { 2 };

    let max_frames =
        ((LOCAL_SOUND_MAX_DURATION_MS * i64::from(LOCAL_SOUND_SAMPLE_RATE)) / 1000) as usize;
    let mut pcm_f32 = Vec::new();
    let mut resampler: Option<Resampler> = None;

    while pcm_f32.len() / out_channels < max_frames {
        let Some(chunk) = decoder.read_chunk()? else {
            break;
        };
        if chunk.samples.is_empty() {
            continue;
        }
        let adapted = if out_channels == 1 && chunk.channels == 1 {
            chunk.samples
        } else {
            to_stereo_f32(&chunk.samples, chunk.channels)
        };
        if chunk.sample_rate == LOCAL_SOUND_SAMPLE_RATE {
            pcm_f32.extend_from_slice(&adapted);
        } else {
            let r = resampler.get_or_insert_with(|| {
                Resampler::new(chunk.sample_rate, LOCAL_SOUND_SAMPLE_RATE, out_channels)
            });
            if r.in_rate() != chunk.sample_rate {
                *r = Resampler::new(chunk.sample_rate, LOCAL_SOUND_SAMPLE_RATE, out_channels);
            }
            r.process(&adapted, &mut pcm_f32);
        }
    }

    let max_samples = max_frames * out_channels;
    if pcm_f32.len() > max_samples {
        pcm_f32.truncate(max_samples);
    }
    if pcm_f32.is_empty() {
        return Err(AudioError::Decode("no audio samples decoded".into()));
    }

    let pcm_i16 = f32_to_i16(&pcm_f32);
    Ok(encode_wav_pcm_s16le(
        &pcm_i16,
        LOCAL_SOUND_SAMPLE_RATE,
        out_channels as u16,
    ))
}

/// Encodes interleaved 16-bit little-endian PCM samples into a standard RIFF WAVE file.
#[must_use]
pub fn encode_wav_pcm_s16le(samples: &[i16], sample_rate: u32, channels: u16) -> Vec<u8> {
    let data_bytes = (samples.len() * 2) as u32;
    let riff_size = 36u32.saturating_add(data_bytes);
    let block_align = channels * 2;
    let byte_rate = sample_rate.saturating_mul(u32::from(block_align));

    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM format tag
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    for &s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// In-memory cache of converted notification sounds (`Media::Audio::LocalCache`).
#[derive(Debug, Default)]
pub struct LocalCache {
    cache: HashMap<DocumentId, Vec<u8>>,
}

impl LocalCache {
    /// Creates an empty [`LocalCache`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolves and caches a converted notification sound for `id`, falling back
    /// to `fallback_original_bytes` under `DocumentId(0)` if conversion fails.
    ///
    /// Exact port of `LocalCache::sound` (`media_audio_local_cache.cpp:290-304`).
    pub fn sound<F, G>(
        &mut self,
        id: DocumentId,
        resolve_original_bytes: F,
        fallback_original_bytes: Option<G>,
    ) -> LocalSound
    where
        F: FnOnce() -> Vec<u8>,
        G: FnOnce() -> Vec<u8>,
    {
        if let Some(existing) = self.cache.get(&id)
            && !existing.is_empty()
        {
            return LocalSound {
                id,
                wav: existing.clone(),
            };
        }
        let original = resolve_original_bytes();
        let converted = convert_and_cut_to_wav(&original).unwrap_or_default();
        self.cache.insert(id, converted.clone());
        if !converted.is_empty() {
            LocalSound { id, wav: converted }
        } else if let Some(fallback) = fallback_original_bytes {
            self.sound::<_, fn() -> Vec<u8>>(DocumentId(0), fallback, None)
        } else {
            LocalSound::default()
        }
    }
}

/// Disk-backed cache writing `TD_<HEX_ID>.wav` / `TD_Default.wav` files for OS
/// notification backends (`Media::Audio::LocalDiskCache`).
#[derive(Debug)]
pub struct LocalDiskCache {
    base: PathBuf,
    names: HashMap<DocumentId, String>,
}

impl LocalDiskCache {
    /// Creates a [`LocalDiskCache`] rooted at `folder`, creating it if missing.
    pub fn new(folder: impl AsRef<Path>) -> AudioResult<Self> {
        let base = folder.as_ref().to_path_buf();
        fs::create_dir_all(&base)?;
        Ok(Self {
            base,
            names: HashMap::new(),
        })
    }

    /// Ensures `sound` is written to disk and returns its logical name
    /// (`TD_<UPPER_HEX>` or `TD_Default`), matching `LocalDiskCache::name`
    /// (`media_audio_local_cache.cpp:311-333`).
    pub fn name(&mut self, sound: &LocalSound) -> AudioResult<Option<String>> {
        if !sound.is_valid() {
            return Ok(None);
        }
        if let Some(existing) = self.names.get(&sound.id) {
            return Ok(Some(existing.clone()));
        }
        let name = if sound.id.0 != 0 {
            format!("TD_{:X}", sound.id.0)
        } else {
            "TD_Default".to_string()
        };
        let path = self.base.join(format!("{name}.wav"));
        fs::write(&path, &sound.wav)?;
        self.names.insert(sound.id, name.clone());
        Ok(Some(name))
    }

    /// Ensures `sound` is written to disk and returns its full `.wav` path.
    pub fn path(&mut self, sound: &LocalSound) -> AudioResult<Option<PathBuf>> {
        Ok(self
            .name(sound)?
            .map(|name| self.base.join(format!("{name}.wav"))))
    }
}
