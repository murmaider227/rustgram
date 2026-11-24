//! Broadcast-stream (RTMP-like chunked) playback path for large Telegram group calls / livestreams.
//!
//! Reference:
//! - `Telegram/SourceFiles/calls/group/calls_group_call.cpp` (`LoadPartTask`, `broadcastPartStart`,
//!   `MTPupload_GetFile(MTP_inputGroupCallStream(...))`)
//! - `TelegramMessenger/tgcalls` (`AudioStreamingPart`, `BroadcastPart`)
//!
//! Large group calls (> ~1000 listeners) and RTMP livestreams return `{"stream": true}` in
//! `updateGroupCallConnection.params`. Instead of WebRTC DTLS-SRTP, the client pulls sequential
//! time-aligned media chunks via MTProto `upload.getFile` (`inputGroupCallStream`) on `stream_dc_id`.
//!
//! This module defines the [`BroadcastChunkLoader`] trait (implemented by `tg-api`/`tg-app` over
//! `upload.getFile`) and [`BroadcastStreamPlayer`], which schedules timestamp-aligned chunk fetches,
//! handles `NotReady` / `ResyncNeeded`, demuxes Ogg Opus (or length-prefixed raw Opus) chunks into
//! 48 kHz mono PCM frames, and emits [`tg_calls::AudioLevelSample`] (`ssrc = 1`).

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use opus_pure::OpusDecoder;
use tg_audio::dsp::soft_clip;
use tg_calls::group::SPEAK_LEVEL_THRESHOLD;
use tg_calls::{AudioLevelSample, VideoQuality};

use crate::dsp::compute_rms_level;
use crate::error::{VoipError, VoipResult};
use crate::jitter::{VOIP_FRAME_SAMPLES, VOIP_SAMPLE_RATE};

/// RTMP / broadcast stream synthetic single audio SSRC (matches `tg_calls` RTMP volume target).
pub const BROADCAST_STREAM_AUDIO_SSRC: u32 = 1;

/// Time granularity scale for `inputGroupCallStream.scale` (`calls_group_call.cpp:449-456`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BroadcastChunkScale {
    /// `scale = 0`: 1000 ms chunks.
    #[default]
    Ms1000 = 0,
    /// `scale = 1`: 500 ms chunks.
    Ms500 = 1,
    /// `scale = 2`: 250 ms chunks.
    Ms250 = 2,
    /// `scale = 3`: 125 ms chunks.
    Ms125 = 3,
}

impl BroadcastChunkScale {
    /// Chunk duration in milliseconds matching `LoadPartTask` in `calls_group_call.cpp`.
    #[must_use]
    pub const fn period_ms(self) -> i64 {
        match self {
            BroadcastChunkScale::Ms1000 => 1_000,
            BroadcastChunkScale::Ms500 => 500,
            BroadcastChunkScale::Ms250 => 250,
            BroadcastChunkScale::Ms125 => 125,
        }
    }

    /// Integer `scale` field for TL `inputGroupCallStream`.
    #[must_use]
    pub const fn tl_scale(self) -> i32 {
        self as i32
    }
}

/// Request passed to [`BroadcastChunkLoader::fetch_chunk`] (maps 1:1 to `inputGroupCallStream`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BroadcastChunkRequest {
    /// Chunk timestamp in milliseconds (aligned to `scale.period_ms()`).
    pub time_ms: i64,
    /// Chunk duration scale (`0..=3`).
    pub scale: BroadcastChunkScale,
    /// Video channel ID (`0` for audio-only chunk).
    pub video_channel: i32,
    /// Video quality tier (`0 = Thumbnail`, `1 = Medium`, `2 = Full`) when `video_channel != 0`.
    pub video_quality: VideoQuality,
}

/// Status of a fetched broadcast chunk (`tgcalls::BroadcastPart::Status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastChunkStatus {
    /// Chunk downloaded successfully (`data` contains encoded media bytes).
    Success,
    /// Server reported `TIME_TOO_BIG` or flood wait; retry shortly.
    NotReady,
    /// CDN redirect or other error; re-synchronize stream timestamp.
    ResyncNeeded,
}

/// Result returned by [`BroadcastChunkLoader::fetch_chunk`] (`tgcalls::BroadcastPart`).
#[derive(Debug, Clone, PartialEq)]
pub struct BroadcastChunkResponse {
    /// Requested chunk timestamp in milliseconds.
    pub timestamp_ms: i64,
    /// Server response timestamp in Unix seconds (from `outerMsgId`), if available.
    pub response_timestamp_sec: f64,
    /// Fetch status.
    pub status: BroadcastChunkStatus,
    /// Raw chunk payload bytes (Ogg Opus container, framed Opus, or raw media segment).
    pub data: Vec<u8>,
}

/// Async chunk loader trait implemented by the MTProto caller (`upload.getFile` with
/// `inputGroupCallStream` on `stream_dc_id`).
pub trait BroadcastChunkLoader: Send + Sync {
    /// Fetch one time-aligned broadcast chunk from Telegram (`upload.getFile`).
    fn fetch_chunk(
        &self,
        req: BroadcastChunkRequest,
    ) -> Pin<Box<dyn Future<Output = BroadcastChunkResponse> + Send + '_>>;

    /// Request current server stream timestamp in milliseconds (`phone.getGroupCallStreamChannels`
    /// or synchronized MTProto server clock).
    fn request_current_time_ms(&self) -> Pin<Box<dyn Future<Output = Option<i64>> + Send + '_>>;
}

/// Chunked broadcast stream audio decoder & playout queue.
pub struct BroadcastStreamPlayer {
    loader: Arc<dyn BroadcastChunkLoader>,
    scale: BroadcastChunkScale,
    next_chunk_time_ms: Option<i64>,
    volume: f64,
    decoder: OpusDecoder,
    pcm_queue: VecDeque<f32>,
    chunks_played: u64,
    connected: bool,
    smoothed_level: f32,
}

impl BroadcastStreamPlayer {
    /// Create a new chunked broadcast stream player using `loader`.
    pub fn new(
        loader: Arc<dyn BroadcastChunkLoader>,
        scale: BroadcastChunkScale,
    ) -> VoipResult<Self> {
        let decoder = OpusDecoder::new(VOIP_SAMPLE_RATE as i32, 1)?;
        Ok(Self {
            loader,
            scale,
            next_chunk_time_ms: None,
            volume: 1.0,
            decoder,
            pcm_queue: VecDeque::with_capacity(VOIP_SAMPLE_RATE as usize * 4),
            chunks_played: 0,
            connected: false,
            smoothed_level: 0.0,
        })
    }

    /// Set playback linear volume multiplier (`0.0..=2.0`).
    pub fn set_volume(&mut self, volume: f64) {
        self.volume = volume.clamp(0.0, 2.0);
    }

    /// Whether at least one chunk has been successfully decoded.
    #[must_use]
    pub const fn is_connected(&self) -> bool {
        self.connected
    }

    /// Number of decoded broadcast chunks processed so far.
    #[must_use]
    pub const fn chunks_played(&self) -> u64 {
        self.chunks_played
    }

    /// Number of queued 48 kHz mono PCM samples awaiting playout.
    #[must_use]
    pub fn buffered_samples(&self) -> usize {
        self.pcm_queue.len()
    }

    /// Fetch and decode the next broadcast audio chunk if the playout buffer is below ~1.5 s.
    ///
    /// Returns `Ok(true)` if a new chunk was decoded into the playout queue.
    pub async fn fetch_next_chunk_if_needed(&mut self) -> VoipResult<bool> {
        // Keep up to ~1.5 s buffered ahead for smooth playback.
        if self.pcm_queue.len() >= (VOIP_SAMPLE_RATE as usize * 3) / 2 {
            return Ok(false);
        }

        let period = self.scale.period_ms();
        let req_time = match self.next_chunk_time_ms {
            Some(t) => t,
            None => {
                // `GroupCall::requestCurrentTimeStart` (`calls_group_call.cpp:3356-3362`) reports
                // `0` when RTMP stream channels are empty or the request fails, treating it as
                // "not ready, ask again". Do not clamp to `time_ms = 0`; return `Ok(false)` and
                // retry `request_current_time_ms()` on the next poll.
                let Some(now_ms) = self
                    .loader
                    .request_current_time_ms()
                    .await
                    .filter(|&t| t > 0)
                else {
                    return Ok(false);
                };
                // Align down to chunk period and step back 1 chunk to avoid `TIME_TOO_BIG`.
                let aligned = (now_ms / period) * period - period;
                self.next_chunk_time_ms = Some(aligned.max(0));
                aligned.max(0)
            }
        };

        let resp = self
            .loader
            .fetch_chunk(BroadcastChunkRequest {
                time_ms: req_time,
                scale: self.scale,
                video_channel: 0,
                video_quality: VideoQuality::Thumbnail,
            })
            .await;

        match resp.status {
            BroadcastChunkStatus::Success => {
                self.decode_chunk_bytes(&resp.data)?;
                self.next_chunk_time_ms = Some(req_time + period);
                self.chunks_played += 1;
                self.connected = true;
                Ok(true)
            }
            BroadcastChunkStatus::NotReady => Ok(false),
            BroadcastChunkStatus::ResyncNeeded => {
                if resp.response_timestamp_sec > 0.0 {
                    let server_ms = (resp.response_timestamp_sec * 1000.0) as i64;
                    let aligned = (server_ms / period) * period - period;
                    self.next_chunk_time_ms = Some(aligned.max(0));
                } else {
                    self.next_chunk_time_ms = None;
                }
                Ok(false)
            }
        }
    }

    /// Pull one 20 ms (`960`-sample) 48 kHz mono frame for speaker playout and return the
    /// broadcast stream's [`AudioLevelSample`] (`ssrc = 1`).
    pub fn pull_frame(&mut self, out: &mut [f32; VOIP_FRAME_SAMPLES]) -> Option<AudioLevelSample> {
        if self.pcm_queue.len() < VOIP_FRAME_SAMPLES {
            out.fill(0.0);
            return None;
        }
        let vol = self.volume as f32;
        let mut raw_frame = [0.0f32; VOIP_FRAME_SAMPLES];
        for (dst, raw) in out.iter_mut().zip(raw_frame.iter_mut()) {
            let s = self.pcm_queue.pop_front().unwrap_or(0.0);
            *raw = s;
            *dst = soft_clip(s * vol);
        }
        let rms = compute_rms_level(&raw_frame);
        let norm = (rms * 2.5).clamp(0.0, 1.0);
        self.smoothed_level = self.smoothed_level * 0.4 + norm * 0.6;
        Some(AudioLevelSample {
            ssrc: BROADCAST_STREAM_AUDIO_SSRC,
            level: self.smoothed_level,
            voice: self.smoothed_level >= SPEAK_LEVEL_THRESHOLD,
        })
    }

    /// Decode a broadcast chunk payload (Ogg Opus container or `TGOA` length-prefixed Opus stream).
    fn decode_chunk_bytes(&mut self, data: &[u8]) -> VoipResult<()> {
        if data.is_empty() {
            return Ok(());
        }
        let packets = if data.starts_with(b"OggS") {
            extract_ogg_opus_packets(data)?
        } else if data.starts_with(b"TGOA") {
            extract_framed_opus_packets(&data[4..])?
        } else {
            // Fallback: treat payload as a single raw Opus packet.
            vec![data.to_vec()]
        };

        let mut frame_buf = [0.0f32; 5760]; // max 120 ms Opus frame at 48 kHz
        for pkt in packets {
            if pkt.is_empty() {
                continue;
            }
            let decoded = self.decoder.decode(&pkt, frame_buf.len(), &mut frame_buf)?;
            self.pcm_queue.extend(&frame_buf[..decoded]);
        }
        Ok(())
    }
}

/// Extract Opus audio packets from an Ogg Opus container chunk (`OggS` pages), skipping
/// `OpusHead` and `OpusTags` header packets.
pub fn extract_ogg_opus_packets(data: &[u8]) -> VoipResult<Vec<Vec<u8>>> {
    let mut packets = Vec::new();
    let mut current_pkt = Vec::new();
    let mut pos = 0usize;

    while pos + 27 <= data.len() {
        if &data[pos..pos + 4] != b"OggS" {
            return Err(VoipError::GroupCall(
                "invalid OggS page capture pattern in broadcast chunk".into(),
            ));
        }
        let seg_count = usize::from(data[pos + 26]);
        let seg_table_start = pos + 27;
        let body_start = seg_table_start + seg_count;
        if body_start > data.len() {
            return Err(VoipError::GroupCall("truncated Ogg segment table".into()));
        }
        let segs = &data[seg_table_start..body_start];
        let body_len: usize = segs.iter().map(|&s| usize::from(s)).sum();
        if body_start + body_len > data.len() {
            return Err(VoipError::GroupCall("truncated Ogg page body".into()));
        }

        let mut body_cursor = body_start;
        for &seg_len in segs {
            let sl = usize::from(seg_len);
            current_pkt.extend_from_slice(&data[body_cursor..body_cursor + sl]);
            body_cursor += sl;
            if seg_len < 255 {
                let pkt = std::mem::take(&mut current_pkt);
                if !pkt.starts_with(b"OpusHead") && !pkt.starts_with(b"OpusTags") && !pkt.is_empty()
                {
                    packets.push(pkt);
                }
            }
        }
        pos = body_start + body_len;
    }

    if !current_pkt.is_empty()
        && !current_pkt.starts_with(b"OpusHead")
        && !current_pkt.starts_with(b"OpusTags")
    {
        packets.push(current_pkt);
    }
    Ok(packets)
}

/// Encode raw Opus packets into a minimal valid Ogg Opus byte stream (useful for tests/mock loaders).
#[must_use]
pub fn encode_ogg_opus_chunk(packets: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    let serial: u32 = 0x5447_4243;

    // Page 0: OpusHead (19 bytes, 48 kHz mono).
    let mut opus_head = Vec::with_capacity(19);
    opus_head.extend_from_slice(b"OpusHead");
    opus_head.push(1); // version
    opus_head.push(1); // channels
    opus_head.extend_from_slice(&0u16.to_le_bytes()); // pre-skip
    opus_head.extend_from_slice(&VOIP_SAMPLE_RATE.to_le_bytes());
    opus_head.extend_from_slice(&0u16.to_le_bytes()); // output gain
    opus_head.push(0); // mapping family
    append_ogg_page(&mut out, 0x02, 0, serial, 0, &[opus_head]);

    // Page 1: OpusTags.
    let mut opus_tags = Vec::new();
    opus_tags.extend_from_slice(b"OpusTags");
    let vendor = b"tg-voip";
    opus_tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    opus_tags.extend_from_slice(vendor);
    opus_tags.extend_from_slice(&0u32.to_le_bytes()); // 0 comments
    append_ogg_page(&mut out, 0x00, 0, serial, 1, &[opus_tags]);

    // Page 2: Audio packets.
    let granule = (packets.len() as u64) * (VOIP_FRAME_SAMPLES as u64);
    append_ogg_page(&mut out, 0x04, granule, serial, 2, packets);
    out
}

fn append_ogg_page(
    out: &mut Vec<u8>,
    header_type: u8,
    granule_pos: u64,
    serial: u32,
    page_seq: u32,
    packets: &[Vec<u8>],
) {
    let mut segs = Vec::new();
    let mut body = Vec::new();
    for pkt in packets {
        let mut rem = pkt.len();
        while rem >= 255 {
            segs.push(255u8);
            rem -= 255;
        }
        segs.push(rem as u8);
        body.extend_from_slice(pkt);
    }

    let page_start = out.len();
    out.extend_from_slice(b"OggS");
    out.push(0); // version
    out.push(header_type);
    out.extend_from_slice(&granule_pos.to_le_bytes());
    out.extend_from_slice(&serial.to_le_bytes());
    out.extend_from_slice(&page_seq.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // CRC placeholder
    out.push(segs.len() as u8);
    out.extend_from_slice(&segs);
    out.extend_from_slice(&body);

    let crc = ogg_crc32(&out[page_start..]);
    out[page_start + 22..page_start + 26].copy_from_slice(&crc.to_le_bytes());
}

fn ogg_crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in bytes {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            if (crc & 0x8000_0000) != 0 {
                crc = (crc << 1) ^ 0x04C1_1DB7;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

fn extract_framed_opus_packets(data: &[u8]) -> VoipResult<Vec<Vec<u8>>> {
    let mut packets = Vec::new();
    let mut pos = 0usize;
    while pos + 2 <= data.len() {
        let len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2;
        if pos + len > data.len() {
            return Err(VoipError::GroupCall(
                "truncated length-prefixed Opus broadcast chunk".into(),
            ));
        }
        packets.push(data[pos..pos + len].to_vec());
        pos += len;
    }
    Ok(packets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::{DEFAULT_VOICE_BITRATE_BPS, VoiceOpusEncoder};

    struct SyntheticChunkLoader {
        chunk_bytes: Vec<u8>,
    }

    impl BroadcastChunkLoader for SyntheticChunkLoader {
        fn fetch_chunk(
            &self,
            req: BroadcastChunkRequest,
        ) -> Pin<Box<dyn Future<Output = BroadcastChunkResponse> + Send + '_>> {
            let data = self.chunk_bytes.clone();
            Box::pin(async move {
                BroadcastChunkResponse {
                    timestamp_ms: req.time_ms,
                    response_timestamp_sec: (req.time_ms as f64) / 1000.0,
                    status: BroadcastChunkStatus::Success,
                    data,
                }
            })
        }

        fn request_current_time_ms(
            &self,
        ) -> Pin<Box<dyn Future<Output = Option<i64>> + Send + '_>> {
            Box::pin(async { Some(10_000) })
        }
    }

    #[tokio::test]
    async fn broadcast_stream_player_decodes_ogg_opus_chunks() {
        let mut enc = VoiceOpusEncoder::new(DEFAULT_VOICE_BITRATE_BPS).unwrap();
        let mut packets = Vec::new();
        for f in 0..10usize {
            let mut pcm = [0.0f32; VOIP_FRAME_SAMPLES];
            for (i, s) in pcm.iter_mut().enumerate() {
                let t = (f * VOIP_FRAME_SAMPLES + i) as f32 / VOIP_SAMPLE_RATE as f32;
                *s = (2.0 * std::f32::consts::PI * 500.0 * t).sin() * 0.4;
            }
            packets.push(enc.encode_frame(&pcm).unwrap());
        }
        let ogg_chunk = encode_ogg_opus_chunk(&packets);
        let loader = Arc::new(SyntheticChunkLoader {
            chunk_bytes: ogg_chunk,
        });

        let mut player = BroadcastStreamPlayer::new(loader, BroadcastChunkScale::Ms1000).unwrap();
        assert!(player.fetch_next_chunk_if_needed().await.unwrap());
        assert!(player.is_connected());
        assert_eq!(player.chunks_played(), 1);
        assert!(player.buffered_samples() >= VOIP_FRAME_SAMPLES * 8);

        let mut frame = [0.0f32; VOIP_FRAME_SAMPLES];
        let mut saw_voice = false;
        for _ in 0..8 {
            if let Some(lvl) = player.pull_frame(&mut frame) {
                assert_eq!(lvl.ssrc, BROADCAST_STREAM_AUDIO_SSRC);
                if lvl.voice {
                    saw_voice = true;
                }
            }
        }
        assert!(
            saw_voice,
            "decoded broadcast tone should register audio level"
        );
    }

    struct DelayedRtmpLoader {
        chunk_bytes: Vec<u8>,
        times: std::sync::Mutex<VecDeque<Option<i64>>>,
        time_requests: std::sync::atomic::AtomicUsize,
        fetched_times: std::sync::Mutex<Vec<i64>>,
    }

    impl BroadcastChunkLoader for DelayedRtmpLoader {
        fn fetch_chunk(
            &self,
            req: BroadcastChunkRequest,
        ) -> Pin<Box<dyn Future<Output = BroadcastChunkResponse> + Send + '_>> {
            self.fetched_times.lock().unwrap().push(req.time_ms);
            let data = self.chunk_bytes.clone();
            Box::pin(async move {
                BroadcastChunkResponse {
                    timestamp_ms: req.time_ms,
                    response_timestamp_sec: (req.time_ms as f64) / 1000.0,
                    status: BroadcastChunkStatus::Success,
                    data,
                }
            })
        }

        fn request_current_time_ms(
            &self,
        ) -> Pin<Box<dyn Future<Output = Option<i64>> + Send + '_>> {
            self.time_requests
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let next = self.times.lock().unwrap().pop_front().flatten();
            Box::pin(async move { next })
        }
    }

    #[tokio::test]
    async fn broadcast_stream_player_retries_when_current_time_is_zero() {
        let mut enc = VoiceOpusEncoder::new(DEFAULT_VOICE_BITRATE_BPS).unwrap();
        let pcm = [0.0f32; VOIP_FRAME_SAMPLES];
        let ogg_chunk = encode_ogg_opus_chunk(&[enc.encode_frame(&pcm).unwrap()]);

        let loader = Arc::new(DelayedRtmpLoader {
            chunk_bytes: ogg_chunk,
            times: std::sync::Mutex::new(VecDeque::from([Some(0), Some(0), Some(15_500)])),
            time_requests: std::sync::atomic::AtomicUsize::new(0),
            fetched_times: std::sync::Mutex::new(Vec::new()),
        });

        let mut player =
            BroadcastStreamPlayer::new(Arc::clone(&loader) as _, BroadcastChunkScale::Ms1000)
                .unwrap();

        // Empty RTMP channel (`Some(0)`) must return `Ok(false)`, never request `time_ms = 0`,
        // and retry `request_current_time_ms()` on subsequent polls.
        assert!(!player.fetch_next_chunk_if_needed().await.unwrap());
        assert!(!player.fetch_next_chunk_if_needed().await.unwrap());
        assert_eq!(
            loader
                .time_requests
                .load(std::sync::atomic::Ordering::SeqCst),
            2
        );
        assert!(
            loader.fetched_times.lock().unwrap().is_empty(),
            "must not fetch time_ms = 0 while current time is 0"
        );
        assert!(!player.is_connected());
        assert_eq!(player.chunks_played(), 0);

        // Once a positive timestamp arrives, fetch the aligned chunk `(15_500 / 1000) * 1000 - 1000`.
        assert!(player.fetch_next_chunk_if_needed().await.unwrap());
        assert_eq!(
            loader
                .time_requests
                .load(std::sync::atomic::Ordering::SeqCst),
            3
        );
        assert_eq!(*loader.fetched_times.lock().unwrap(), vec![14_000]);
        assert!(player.is_connected());
        assert_eq!(player.chunks_played(), 1);
    }
}
