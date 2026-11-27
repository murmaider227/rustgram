//! Pure-Rust Ogg Opus encoding, decoding, duration inspection, seeking, and
//! packet-level trimming/concatenation.
//!
//! Uses `opus-pure` (RFC 6716 Opus + RFC 7845 Ogg Opus container) with zero C
//! or CMake dependencies. Also ports `media/audio/media_audio_edit.cpp`
//! (`TrimAudioToRange`, `ConcatAudio`) for Ogg Opus voice messages.

use std::io::{Cursor, Read, Seek, SeekFrom};

use opus_pure::{
    Application, MAX_PACKET_BYTES, MAX_PACKET_SAMPLES, OggOpusReader, OggOpusWriter, OpusDecoder,
    OpusEncoder, OpusHead, OpusTags, Trim,
};

use crate::error::{AudioError, AudioResult};
use crate::types::DEFAULT_FREQUENCY;
use crate::waveform::{VoiceWaveform, compute_waveform_from_i16};

/// Standard Opus frame size for Telegram voice capture: 20 ms at 48 kHz = 960 samples.
pub const OPUS_VOICE_FRAME_SAMPLES: usize = 960;

/// Telegram Desktop voice recording bitrate (`d->codecContext->bit_rate = 32000`).
pub const OPUS_VOICE_BITRATE_BPS: i32 = 32_000;

/// Checks whether a byte prefix begins with an Ogg Opus stream (`OggS` + `OpusHead`).
#[must_use]
pub fn is_ogg_opus_header(prefix: &[u8]) -> bool {
    if prefix.len() < 36 || &prefix[..4] != b"OggS" {
        return false;
    }
    // Ogg page header is 27 bytes + number_page_segments bytes; segment table
    // starts at byte 27, first packet payload follows immediately after.
    let num_segments = usize::from(prefix[26]);
    let payload_offset = 27 + num_segments;
    prefix.len() >= payload_offset + 8 && &prefix[payload_offset..payload_offset + 8] == b"OpusHead"
}

/// Encodes mono or stereo 16-bit PCM into a Telegram-compatible `.ogg` Opus file.
///
/// Sample rate must be one of Opus's native rates (8000, 12000, 16000, 24000, or 48000 Hz;
/// Telegram voice messages use 48,000 Hz mono at 32,000 bps).
pub fn encode_ogg_opus_i16(
    pcm: &[i16],
    sample_rate: u32,
    channels: u8,
    bitrate_bps: i32,
    application: Application,
) -> AudioResult<Vec<u8>> {
    if channels != 1 && channels != 2 {
        return Err(AudioError::UnsupportedFormat(format!(
            "Opus supports 1 or 2 channels, got {channels}"
        )));
    }
    let ch = usize::from(channels);
    if !pcm.len().is_multiple_of(ch) {
        return Err(AudioError::InvalidOperation(
            "PCM sample count is not a multiple of channel count".into(),
        ));
    }

    // 20 ms frame size at `sample_rate`.
    let frame_samples = ((u64::from(sample_rate) * 20) / 1000) as usize;
    let mut encoder = OpusEncoder::new(sample_rate as i32, ch, application)?;
    encoder.bitrate_bps = bitrate_bps;

    let head = OpusHead::for_encoder(&encoder, sample_rate);
    let mut tags = OpusTags::new();
    tags.vendor = "tg-audio (opus-pure)".to_string();

    let mut writer = OggOpusWriter::with_tags(Vec::new(), head, tags)?;
    let mut packet = vec![0u8; MAX_PACKET_BYTES];

    let frame_len = frame_samples * ch;
    let total_input_frames = pcm.len() / ch;
    let mut offset = 0usize;

    // Encode all full frames.
    while offset + frame_len <= pcm.len() {
        let n = encoder.encode_s16(&pcm[offset..offset + frame_len], frame_samples, &mut packet)?;
        writer.write_packet(&packet[..n])?;
        offset += frame_len;
    }

    // Pad the final partial frame if any, trimming the Ogg granule position to exact end.
    let remainder = pcm.len() - offset;
    if remainder > 0 {
        let mut padded = vec![0i16; frame_len];
        padded[..remainder].copy_from_slice(&pcm[offset..]);
        let n = encoder.encode_s16(&padded, frame_samples, &mut packet)?;
        let rem_frames_48k = (((remainder / ch) as u64) * u64::from(DEFAULT_FREQUENCY)
            / u64::from(sample_rate)) as u32;
        writer.write_packet_with_duration(&packet[..n], rem_frames_48k.max(1))?;
    }

    // Emit one trailing flush frame to push the encoder's lookahead delay through
    // the stream so the decoder can recover all input samples after `pre_skip`.
    let lookahead_48k = usize::from(OpusHead::for_encoder(&encoder, sample_rate).pre_skip);
    if lookahead_48k > 0 && total_input_frames > 0 {
        let silence = vec![0i16; frame_len];
        let n = encoder.encode_s16(&silence, frame_samples, &mut packet)?;
        writer.write_packet_with_duration(&packet[..n], lookahead_48k as u32)?;
    }

    Ok(writer.finish()?)
}

/// Decodes an entire Ogg Opus buffer into interleaved `f32` PCM at 48 kHz.
///
/// Returns `(samples, sample_rate, channels, duration_ms)`.
pub fn decode_ogg_opus_to_f32(bytes: &[u8]) -> AudioResult<(Vec<f32>, u32, u8, i64)> {
    let mut dec = OggOpusStreamDecoder::new(Cursor::new(bytes))?;
    let sample_rate = dec.sample_rate();
    let channels = dec.channels();
    let mut out = Vec::new();
    while let Some(chunk) = dec.read_next_f32()? {
        out.extend_from_slice(&chunk);
    }
    let frames = out.len() / usize::from(channels);
    let duration_ms = ((frames as i64) * 1000) / i64::from(sample_rate);
    Ok((out, sample_rate, channels, duration_ms))
}

/// Decodes an entire Ogg Opus buffer into interleaved `i16` PCM at 48 kHz.
///
/// Returns `(samples, sample_rate, channels, duration_ms)`.
pub fn decode_ogg_opus_to_i16(bytes: &[u8]) -> AudioResult<(Vec<i16>, u32, u8, i64)> {
    let mut dec = OggOpusStreamDecoder::new(Cursor::new(bytes))?;
    let sample_rate = dec.sample_rate();
    let channels = dec.channels();
    let mut out = Vec::new();
    while let Some(chunk) = dec.read_next_i16()? {
        out.extend_from_slice(&chunk);
    }
    let frames = out.len() / usize::from(channels);
    let duration_ms = ((frames as i64) * 1000) / i64::from(sample_rate);
    Ok((out, sample_rate, channels, duration_ms))
}

/// Streaming packet-by-packet Ogg Opus decoder with accurate duration detection
/// and sample-accurate seeking.
pub struct OggOpusStreamDecoder<R: Read + Seek> {
    reader: OggOpusReader<R>,
    head: OpusHead,
    tags: OpusTags,
    decoder: OpusDecoder,
    trim: Trim,
    sample_rate: u32,
    channels: u8,
    duration_ms: i64,
    total_frames_48k: i64,
    current_frame_48k: i64,
    scratch_f32: Vec<f32>,
    scratch_i16: Vec<i16>,
}

impl<R: Read + Seek> OggOpusStreamDecoder<R> {
    /// Opens an Ogg Opus stream for decoding at 48 kHz.
    pub fn new(mut source: R) -> AudioResult<Self> {
        let total_granule = scan_last_ogg_granule(&mut source).unwrap_or(0);
        source.seek(SeekFrom::Start(0))?;

        let reader = OggOpusReader::new(source)?;
        let head = reader.head().clone();
        let tags = reader.tags().clone();
        let channels = head.channel_count;
        if channels == 0 || channels > 2 {
            return Err(AudioError::UnsupportedFormat(format!(
                "unsupported Opus channel count: {channels}"
            )));
        }
        let sample_rate = DEFAULT_FREQUENCY;
        let decoder = head.decoder(sample_rate as i32)?;
        let trim = Trim::new(&head, sample_rate as i32, usize::from(channels))?;

        let total_frames_48k = total_granule
            .saturating_sub(i64::from(head.pre_skip))
            .max(0);
        // The granule is read from the file: saturate instead of overflowing on a bogus one.
        let duration_ms = total_frames_48k.saturating_mul(1000) / i64::from(DEFAULT_FREQUENCY);

        let max_samples = MAX_PACKET_SAMPLES * usize::from(channels);
        Ok(Self {
            reader,
            head,
            tags,
            decoder,
            trim,
            sample_rate,
            channels,
            duration_ms,
            total_frames_48k,
            current_frame_48k: 0,
            scratch_f32: vec![0.0; max_samples],
            scratch_i16: vec![0; max_samples],
        })
    }

    /// Output sample rate (always 48,000 Hz for Ogg Opus).
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Output channel count (1 = mono, 2 = stereo).
    #[must_use]
    pub const fn channels(&self) -> u8 {
        self.channels
    }

    /// Duration in milliseconds derived from the final Ogg page granule minus pre-skip.
    #[must_use]
    pub const fn duration_ms(&self) -> i64 {
        self.duration_ms
    }

    /// Total frames at 48 kHz.
    #[must_use]
    pub const fn total_frames(&self) -> i64 {
        self.total_frames_48k
    }

    /// Parsed RFC 7845 `OpusHead`.
    #[must_use]
    pub const fn head(&self) -> &OpusHead {
        &self.head
    }

    /// Parsed RFC 7845 `OpusTags` (Vorbis comments).
    #[must_use]
    pub const fn tags(&self) -> &OpusTags {
        &self.tags
    }

    /// Reads and decodes the next Ogg Opus packet as interleaved `f32` PCM.
    ///
    /// Returns `Ok(None)` at end of stream.
    pub fn read_next_f32(&mut self) -> AudioResult<Option<Vec<f32>>> {
        let ch = usize::from(self.channels);
        loop {
            let Some(packet) = self.reader.read_packet()? else {
                return Ok(None);
            };
            if packet.data.is_empty() {
                continue;
            }
            let decoded_frames =
                self.decoder
                    .decode(&packet.data, MAX_PACKET_SAMPLES, &mut self.scratch_f32)?;
            let slice = &self.scratch_f32[..decoded_frames * ch];
            let kept = self.trim.keep(&packet, slice);
            if kept.is_empty() {
                continue;
            }
            self.current_frame_48k += (kept.len() / ch) as i64;
            return Ok(Some(kept.to_vec()));
        }
    }

    /// Reads and decodes the next Ogg Opus packet as interleaved `i16` PCM.
    ///
    /// Returns `Ok(None)` at end of stream.
    pub fn read_next_i16(&mut self) -> AudioResult<Option<Vec<i16>>> {
        let ch = usize::from(self.channels);
        loop {
            let Some(packet) = self.reader.read_packet()? else {
                return Ok(None);
            };
            if packet.data.is_empty() {
                continue;
            }
            let decoded_frames =
                self.decoder
                    .decode_s16(&packet.data, MAX_PACKET_SAMPLES, &mut self.scratch_i16)?;
            let slice = &self.scratch_i16[..decoded_frames * ch];
            let kept = self.trim.keep(&packet, slice);
            if kept.is_empty() {
                continue;
            }
            self.current_frame_48k += (kept.len() / ch) as i64;
            return Ok(Some(kept.to_vec()));
        }
    }

    /// Seeks to `position_ms` from the beginning of the stream, decoding the
    /// RFC 7845 80 ms pre-roll so Opus filter state converges cleanly.
    pub fn seek_ms(&mut self, position_ms: i64) -> AudioResult<()> {
        let target_frame = ((position_ms.max(0)) * i64::from(self.sample_rate)) / 1000;
        self.reader.rewind()?;
        self.decoder = self.head.decoder(self.sample_rate as i32)?;
        self.trim = Trim::new(
            &self.head,
            self.sample_rate as i32,
            usize::from(self.channels),
        )?;
        self.current_frame_48k = 0;

        if target_frame == 0 {
            return Ok(());
        }

        let ch = usize::from(self.channels);
        // Decode & discard up to `target_frame` (accurate and fast for voice/music Ogg Opus).
        while self.current_frame_48k < target_frame {
            let Some(packet) = self.reader.read_packet()? else {
                break;
            };
            if packet.data.is_empty() {
                continue;
            }
            let decoded_frames =
                self.decoder
                    .decode(&packet.data, MAX_PACKET_SAMPLES, &mut self.scratch_f32)?;
            let kept = self
                .trim
                .keep(&packet, &self.scratch_f32[..decoded_frames * ch]);
            self.current_frame_48k += (kept.len() / ch) as i64;
        }
        Ok(())
    }
}

/// Scans backwards from the end of an Ogg stream to locate the last page's
/// absolute granule position (in 48 kHz samples per RFC 7845).
fn scan_last_ogg_granule<R: Read + Seek>(source: &mut R) -> AudioResult<i64> {
    let file_len = source.seek(SeekFrom::End(0))?;
    if file_len < 27 {
        return Ok(0);
    }
    let window = file_len.min(65_536);
    source.seek(SeekFrom::Start(file_len - window))?;
    let mut buf = vec![0u8; window as usize];
    source.read_exact(&mut buf)?;

    // Search backwards for the last valid "OggS" page capture pattern with a
    // non-negative granule position.
    let mut pos = buf.len().saturating_sub(27);
    loop {
        if &buf[pos..pos + 4] == b"OggS" && buf[pos + 4] == 0 {
            let granule = i64::from_le_bytes([
                buf[pos + 6],
                buf[pos + 7],
                buf[pos + 8],
                buf[pos + 9],
                buf[pos + 10],
                buf[pos + 11],
                buf[pos + 12],
                buf[pos + 13],
            ]);
            if granule > 0 {
                return Ok(granule);
            }
        }
        if pos == 0 {
            break;
        }
        pos -= 1;
    }
    Ok(0)
}

/// Result of trimming or concatenating Ogg Opus voice messages.
///
/// Port of `Media::AudioEditResult` (`media/audio/media_audio_edit.h`).
#[derive(Debug, Clone)]
pub struct AudioEditResult {
    /// Encoded Ogg Opus bytes.
    pub content: Vec<u8>,
    /// Recomputed 100-bar waveform.
    pub waveform: VoiceWaveform,
    /// Duration in milliseconds.
    pub duration_ms: i64,
}

/// Trims an Ogg Opus voice recording to `[from_ms, till_ms)`.
///
/// Port of `Media::TrimAudioToRange` (`media/audio/media_audio_edit.cpp:15-192`).
pub fn trim_ogg_opus(content: &[u8], from_ms: i64, till_ms: i64) -> AudioResult<AudioEditResult> {
    if content.is_empty() || from_ms < 0 || till_ms <= from_ms {
        return Err(AudioError::InvalidOperation(
            "invalid trim range or empty audio".into(),
        ));
    }

    let (pcm, sample_rate, channels, _) = decode_ogg_opus_to_i16(content)?;
    let ch = usize::from(channels);
    let total_frames = pcm.len() / ch;
    let start_frame = (((from_ms as u64) * u64::from(sample_rate)) / 1000) as usize;
    let end_frame =
        ((((till_ms as u64) * u64::from(sample_rate)) / 1000) as usize).min(total_frames);
    if start_frame >= end_frame {
        return Err(AudioError::InvalidOperation(
            "trim range lies outside audio duration".into(),
        ));
    }

    let sliced = &pcm[start_frame * ch..end_frame * ch];
    let encoded = encode_ogg_opus_i16(
        sliced,
        sample_rate,
        channels,
        OPUS_VOICE_BITRATE_BPS,
        Application::Voip,
    )?;
    let waveform = compute_waveform_from_i16(sliced, ch);
    let duration_ms = (((end_frame - start_frame) as i64) * 1000) / i64::from(sample_rate);
    Ok(AudioEditResult {
        content: encoded,
        waveform,
        duration_ms,
    })
}

/// Concatenates two Ogg Opus voice recordings into a single Ogg Opus stream.
///
/// Port of `Media::ConcatAudio` (`media/audio/media_audio_edit.cpp:194-405`).
pub fn concat_ogg_opus(first: &[u8], second: &[u8]) -> AudioResult<AudioEditResult> {
    if first.is_empty() || second.is_empty() {
        return Err(AudioError::InvalidOperation(
            "cannot concatenate empty audio".into(),
        ));
    }
    let (mut pcm1, rate1, ch1, _) = decode_ogg_opus_to_i16(first)?;
    let (pcm2, rate2, ch2, _) = decode_ogg_opus_to_i16(second)?;
    if rate1 != rate2 || ch1 != ch2 {
        return Err(AudioError::UnsupportedFormat(
            "concatenated Ogg Opus clips must have matching rate and channels".into(),
        ));
    }
    pcm1.extend_from_slice(&pcm2);
    let ch = usize::from(ch1);
    let encoded =
        encode_ogg_opus_i16(&pcm1, rate1, ch1, OPUS_VOICE_BITRATE_BPS, Application::Voip)?;
    let waveform = compute_waveform_from_i16(&pcm1, ch);
    let duration_ms = (((pcm1.len() / ch) as i64) * 1000) / i64::from(rate1);
    Ok(AudioEditResult {
        content: encoded,
        waveform,
        duration_ms,
    })
}
