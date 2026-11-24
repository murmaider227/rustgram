//! Unified audio decoder supporting Ogg Opus (via `opus-pure`) and
//! MP3, FLAC, AAC, WAV, Vorbis, ALAC, ADPCM, PCM, MP4/M4A, MKV (via `symphonia`).
//!
//! Port of `media/audio/media_audio_ffmpeg_loader.h`,
//! `media/audio/media_audio_ffmpeg_loader.cpp`, `media/audio/media_audio_loader.cpp`,
//! and `FFMpegAttributesReader` / `PrepareForSending` in `media/audio/media_audio.cpp`.

use std::io::{Read, Seek, SeekFrom};

use symphonia::core::audio::sample::Sample;
use symphonia::core::codecs::audio::{AudioDecoder as SymphoniaAudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::{MetadataOptions, StandardTag};
use symphonia::core::units::{Time, Timestamp};

use crate::error::{AudioError, AudioResult};
use crate::mixer::{AudioStream, PcmChunk};
use crate::opus_codec::{OggOpusStreamDecoder, is_ogg_opus_header};
use crate::sound_track::{MAX_SOUND_TRACK_FILE_SIZE, SoundTrack};
use crate::source::{AudioInput, ReadSeek};
use crate::waveform::{VoiceWaveform, compute_waveform_from_f32};

/// Extracted audio file metadata (`Ui::PreparedFileInformation::Song` /
/// `FFMpegAttributesReader` in `media_audio.cpp:1425-1549`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioAttributes {
    /// Duration in milliseconds (0 if unavailable).
    pub duration_ms: i64,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Number of channels.
    pub channels: usize,
    /// Track title tag, if present.
    pub title: Option<String>,
    /// Artist / performer / album-artist tag, if present.
    pub performer: Option<String>,
    /// Embedded cover artwork bytes, if present.
    pub cover_bytes: Option<Vec<u8>>,
    /// Embedded cover MIME type (e.g. `"image/jpeg"`, `"image/png"`).
    pub cover_mime: Option<String>,
}

/// Wrapper implementing Symphonia's [`MediaSource`] over any boxed [`ReadSeek`].
struct BoxedMediaSource {
    inner: Box<dyn ReadSeek>,
    byte_len: Option<u64>,
}

impl Read for BoxedMediaSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Seek for BoxedMediaSource {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

impl MediaSource for BoxedMediaSource {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        self.byte_len
    }
}

/// Internal decoder backend.
enum DecoderBackend {
    OggOpus(Box<OggOpusStreamDecoder<Box<dyn ReadSeek>>>),
    Symphonia {
        format: Box<dyn FormatReader>,
        decoder: Box<dyn SymphoniaAudioDecoder>,
        track_id: u32,
        sample_rate: u32,
        channels: usize,
        duration_ms: i64,
        scratch: Vec<f32>,
    },
}

/// Unified streaming audio decoder implementing [`AudioStream`].
///
/// Automatically detects Ogg Opus (`OggS` + `OpusHead`) and decodes it with the
/// pure-Rust RFC 6716/7845 Opus decoder, falling back to `symphonia` for MP3,
/// FLAC, AAC, WAV, Vorbis, ALAC, PCM, MP4/M4A, and MKV containers.
pub struct AudioDecoder {
    backend: DecoderBackend,
    attributes: AudioAttributes,
}

impl AudioDecoder {
    /// Opens an [`AudioInput`] and probes its container, codec, duration, and tags.
    pub fn open(mut input: AudioInput) -> AudioResult<Self> {
        // Peek first 64 bytes to detect Ogg Opus streams accurately.
        let mut header = [0u8; 64];
        let mut read_total = 0usize;
        while read_total < header.len() {
            match input.reader.read(&mut header[read_total..]) {
                Ok(0) => break,
                Ok(n) => read_total += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(AudioError::Io(e)),
            }
        }
        input.reader.seek(SeekFrom::Start(0))?;

        if is_ogg_opus_header(&header[..read_total]) {
            let opus = OggOpusStreamDecoder::new(input.reader)?;
            let tags = opus.tags();
            let title = tags.get("title").map(str::to_string);
            let performer = tags
                .get("artist")
                .or_else(|| tags.get("performer"))
                .or_else(|| tags.get("album_artist"))
                .map(str::to_string);
            let attributes = AudioAttributes {
                duration_ms: opus.duration_ms(),
                sample_rate: opus.sample_rate(),
                channels: usize::from(opus.channels()),
                title,
                performer,
                cover_bytes: None,
                cover_mime: None,
            };
            return Ok(Self {
                backend: DecoderBackend::OggOpus(Box::new(opus)),
                attributes,
            });
        }

        // Otherwise probe via Symphonia (MP3, FLAC, AAC, WAV, Vorbis, ALAC, MP4, MKV, ...).
        let source = BoxedMediaSource {
            inner: input.reader,
            byte_len: input.byte_len,
        };
        let mss = MediaSourceStream::new(Box::new(source), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = &input.hint_ext {
            hint.with_extension(ext);
        }

        let fmt_opts = FormatOptions::default();
        let meta_opts = MetadataOptions::default();
        let dec_opts = AudioDecoderOptions::default();

        let mut format = symphonia::default::get_probe()
            .probe(&hint, mss, fmt_opts, meta_opts)
            .map_err(|e| AudioError::Decode(e.to_string()))?;

        let track = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| AudioError::UnsupportedFormat("no audio track found".into()))?;

        let track_id = track.id;
        let audio_params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| {
                AudioError::UnsupportedFormat("missing audio codec parameters".into())
            })?;

        let sample_rate = audio_params.sample_rate.unwrap_or(48_000);
        let channels = audio_params
            .channels
            .as_ref()
            .map(|c| c.count())
            .unwrap_or(2)
            .max(1);

        let duration_ms = track
            .num_frames
            .and_then(|frames| {
                track
                    .time_base
                    .and_then(|tb| {
                        let ts = Timestamp::new(frames as i64);
                        tb.calc_time(ts).map(|t| (t.as_nanos() / 1_000_000) as i64)
                    })
                    .or_else(|| Some(((frames as i64) * 1000) / i64::from(sample_rate.max(1))))
            })
            .unwrap_or(0);

        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(audio_params, &dec_opts)
            .map_err(|e| AudioError::Decode(e.to_string()))?;

        let mut attributes = AudioAttributes {
            duration_ms,
            sample_rate,
            channels,
            title: None,
            performer: None,
            cover_bytes: None,
            cover_mime: None,
        };
        extract_symphonia_metadata(&mut format, &mut attributes);

        Ok(Self {
            backend: DecoderBackend::Symphonia {
                format,
                decoder,
                track_id,
                sample_rate,
                channels,
                duration_ms,
                scratch: Vec::new(),
            },
            attributes,
        })
    }

    /// Opens a decoder directly from an in-memory byte slice.
    pub fn from_bytes(bytes: Vec<u8>, hint_ext: Option<String>) -> AudioResult<Self> {
        Self::open(AudioInput::from_bytes(bytes, hint_ext))
    }

    /// Returns the extracted [`AudioAttributes`] (duration, tags, cover art).
    #[must_use]
    pub const fn attributes(&self) -> &AudioAttributes {
        &self.attributes
    }
}

impl AudioStream for AudioDecoder {
    fn sample_rate(&self) -> u32 {
        match &self.backend {
            DecoderBackend::OggOpus(dec) => dec.sample_rate(),
            DecoderBackend::Symphonia { sample_rate, .. } => *sample_rate,
        }
    }

    fn channels(&self) -> usize {
        match &self.backend {
            DecoderBackend::OggOpus(dec) => usize::from(dec.channels()),
            DecoderBackend::Symphonia { channels, .. } => *channels,
        }
    }

    fn duration_ms(&self) -> i64 {
        match &self.backend {
            DecoderBackend::OggOpus(dec) => dec.duration_ms(),
            DecoderBackend::Symphonia { duration_ms, .. } => *duration_ms,
        }
    }

    fn seek_ms(&mut self, position_ms: i64) -> AudioResult<()> {
        match &mut self.backend {
            DecoderBackend::OggOpus(dec) => dec.seek_ms(position_ms),
            DecoderBackend::Symphonia {
                format,
                decoder,
                track_id,
                ..
            } => {
                let time = Time::from_millis(position_ms.max(0));
                format
                    .seek(
                        SeekMode::Accurate,
                        SeekTo::Time {
                            time,
                            track_id: Some(*track_id),
                        },
                    )
                    .or_else(|_| {
                        format.seek(
                            SeekMode::Coarse,
                            SeekTo::Time {
                                time,
                                track_id: Some(*track_id),
                            },
                        )
                    })
                    .map_err(|e| AudioError::Decode(e.to_string()))?;
                decoder.reset();
                Ok(())
            }
        }
    }

    fn read_chunk(&mut self) -> AudioResult<Option<PcmChunk>> {
        match &mut self.backend {
            DecoderBackend::OggOpus(dec) => {
                let Some(samples) = dec.read_next_f32()? else {
                    return Ok(None);
                };
                Ok(Some(PcmChunk {
                    samples,
                    sample_rate: dec.sample_rate(),
                    channels: usize::from(dec.channels()),
                }))
            }
            DecoderBackend::Symphonia {
                format,
                decoder,
                track_id,
                sample_rate,
                channels,
                scratch,
                ..
            } => loop {
                let packet = match format.next_packet() {
                    Ok(Some(pkt)) => pkt,
                    Ok(None) => return Ok(None),
                    Err(SymphoniaError::ResetRequired) => {
                        decoder.reset();
                        continue;
                    }
                    Err(SymphoniaError::IoError(e))
                        if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                    {
                        return Ok(None);
                    }
                    Err(e) => return Err(AudioError::Decode(e.to_string())),
                };
                if packet.track_id != *track_id {
                    continue;
                }
                match decoder.decode(&packet) {
                    Ok(audio_buf) => {
                        let spec = audio_buf.spec();
                        *sample_rate = spec.rate();
                        *channels = spec.channels().count().max(1);
                        let len = audio_buf.samples_interleaved();
                        if len == 0 {
                            continue;
                        }
                        scratch.resize(len, f32::MID);
                        audio_buf.copy_to_slice_interleaved(&mut *scratch);
                        return Ok(Some(PcmChunk {
                            samples: scratch.clone(),
                            sample_rate: *sample_rate,
                            channels: *channels,
                        }));
                    }
                    Err(SymphoniaError::DecodeError(_)) => continue,
                    Err(e) => return Err(AudioError::Decode(e.to_string())),
                }
            },
        }
    }
}

fn extract_symphonia_metadata(format: &mut Box<dyn FormatReader>, attrs: &mut AudioAttributes) {
    let mut meta = format.metadata();
    while !meta.is_latest() {
        if meta.pop().is_none() {
            break;
        }
    }
    let Some(rev) = meta.current() else {
        return;
    };

    for tag in &rev.media.tags {
        if let Some(std_tag) = &tag.std {
            match std_tag {
                StandardTag::TrackTitle(v) if attrs.title.is_none() => {
                    attrs.title = Some(v.as_ref().clone());
                }
                StandardTag::Artist(v) if attrs.performer.is_none() => {
                    attrs.performer = Some(v.as_ref().clone());
                }
                StandardTag::Performer(v) if attrs.performer.is_none() => {
                    attrs.performer = Some(v.as_ref().clone());
                }
                StandardTag::AlbumArtist(v) if attrs.performer.is_none() => {
                    attrs.performer = Some(v.as_ref().clone());
                }
                _ => {}
            }
        }
        // Fallback to raw tag keys matching FFMpegAttributesReader (`title`, `artist`, `performer`, `album_artist`).
        let key = tag.raw.key.to_ascii_lowercase();
        let val_str = tag.raw.value.to_string();
        if !val_str.is_empty() {
            if key == "title" && attrs.title.is_none() {
                attrs.title = Some(val_str);
            } else if matches!(key.as_str(), "artist" | "performer" | "album_artist")
                && attrs.performer.is_none()
            {
                attrs.performer = Some(val_str);
            }
        }
    }

    if attrs.cover_bytes.is_none()
        && let Some(visual) = rev.media.visuals.first()
        && !visual.data.is_empty()
    {
        attrs.cover_bytes = Some(visual.data.to_vec());
        attrs.cover_mime = visual.media_type.clone();
    }
}

/// Probes audio metadata (`title`, `performer`, `duration_ms`, `cover_bytes`)
/// before sending an audio file.
///
/// Port of `Media::Player::PrepareForSending` (`media_audio.cpp:1536-1549`).
pub fn prepare_for_sending(
    bytes: Vec<u8>,
    hint_ext: Option<String>,
) -> AudioResult<AudioAttributes> {
    let decoder = AudioDecoder::from_bytes(bytes, hint_ext)?;
    Ok(decoder.attributes().clone())
}

/// Decodes any supported audio file bytes and computes its 100-sample waveform.
///
/// Port of `audioCountWaveform` (`media_audio.cpp:1641-1650`).
pub fn count_waveform_from_bytes(
    bytes: Vec<u8>,
    hint_ext: Option<String>,
) -> AudioResult<VoiceWaveform> {
    let mut decoder = AudioDecoder::from_bytes(bytes, hint_ext)?;
    let channels = decoder.channels();
    let mut all_samples = Vec::new();
    while let Some(chunk) = decoder.read_chunk()? {
        all_samples.extend_from_slice(&chunk.samples);
    }
    Ok(compute_waveform_from_f32(&all_samples, channels))
}

/// Decodes a UI/notification sound file (up to 10 MiB) into a ready-to-play [`SoundTrack`].
///
/// Port of `Media::Audio::Track::fillFromData` (`media_audio_track.cpp:53-111`).
pub fn decode_sound_track(
    bytes: Vec<u8>,
    hint_ext: Option<String>,
    peak_duration_ms: i64,
) -> AudioResult<SoundTrack> {
    if bytes.is_empty() || bytes.len() > MAX_SOUND_TRACK_FILE_SIZE {
        return Err(AudioError::LimitExceeded(format!(
            "sound track byte size {} out of bounds (1..={MAX_SOUND_TRACK_FILE_SIZE})",
            bytes.len()
        )));
    }
    let mut decoder = AudioDecoder::from_bytes(bytes, hint_ext)?;
    let sample_rate = decoder.sample_rate();
    let channels = decoder.channels();
    let mut all_samples = Vec::new();
    while let Some(chunk) = decoder.read_chunk()? {
        all_samples.extend_from_slice(&chunk.samples);
    }
    SoundTrack::from_pcm_f32(&all_samples, sample_rate, channels, peak_duration_ms)
}
