//! Unit and FFmpeg integration tests for `tg-video`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tg_video::ffmpeg::{
    aligned_frame_stride, correct_by_aspect, duration_by_packet, max_pixels_for_area_limit,
    premultiply_rgba_inplace, pts_to_time, pts_to_time_ceil, rotate_pixels_4bpp, time_to_pts,
    unpremultiply_rgba, validate_aspect_ratio,
};
use tg_video::streaming::{
    is_contiguous_serialization, parse_cached_map, serialize_complex_parts_map, world_time_delay,
};
use tg_video::{
    CacheKey, ClipMode, ClipReader, DEFAULT_PARTS_IN_SLICE, DEFAULT_SLICE_SIZE, DecodedPacket,
    Decoder, FfmpegStreamDecoder, FrameData, FrameFormat, FrameGenerator, FramePopOutcome,
    FrameQueue, HeaderMode, LoadedPart, MAX_FRAME_AREA, MAX_PARTS_IN_HEADER, MemoryRangeLoader,
    MemorySliceCache, OutputPixelFormat, PART_SIZE, PRELOAD_PARTS_AHEAD, PartsMap, PlaybackMode,
    PlaybackOptions, PlayerStage, PriorityQueue, QualityDescriptor, RangeLoader, Rational,
    Rotation, Size, SliceCache, SliceConfig, SpeedEstimate, SpeedEstimator, StreamInformation,
    StreamingDocument, StreamingError, StreamingPlayer, StreamingReader, StreamingUpdate,
    SyncClock, ThumbnailEncoding, ThumbnailOptions, TimePoint, TrackState, VideoError, VideoFrame,
    VideoInformation, apply_circle_mask_rgba, apply_rounded_rect_mask_rgba,
    check_streaming_support, check_streaming_support_file, encode_thumbnail, equal_speeds,
    extract_thumbnail, ffmpeg_versions, flatten_alpha_over_white, generate_circle_mask,
    valid_frame_size,
};

// ---------------------------------------------------------------------------
// Utility & math tests
// ---------------------------------------------------------------------------

#[test]
fn ffmpeg_libraries_linked_and_report_versions() {
    let v = ffmpeg_versions();
    assert!(v.avformat > 0);
    assert!(v.avcodec > 0);
    assert!(v.avutil > 0);
    assert!(v.swscale > 0);
    assert!(v.swresample > 0);
}

#[test]
fn pts_time_and_aspect_helpers_match_tdesktop() {
    let tb = Rational::new(1, 90_000);
    assert_eq!(pts_to_time(90_000, tb), 1000);
    assert_eq!(pts_to_time(45_000, tb), 500);
    assert_eq!(pts_to_time_ceil(1, tb), 1);
    assert_eq!(time_to_pts(1000, tb), 90_000);
    assert_eq!(
        duration_by_packet(90_000, 90_000, 3_000, tb),
        Some(1034) // 1000 + 33 + 1
    );

    assert_eq!(
        validate_aspect_ratio(Rational::new(4, 3)),
        Rational::new(4, 3)
    );
    assert_eq!(
        validate_aspect_ratio(Rational::new(0, 1)),
        Rational::NORMAL_ASPECT
    );
    assert_eq!(
        validate_aspect_ratio(Rational::new(100, 1)),
        Rational::NORMAL_ASPECT
    );
    assert_eq!(
        correct_by_aspect(Size::new(640, 480), Rational::new(3, 2)),
        Size::new(960, 480)
    );

    assert!(valid_frame_size(1920, 1080, MAX_FRAME_AREA));
    assert!(!valid_frame_size(0, 1080, MAX_FRAME_AREA));
    assert!(!valid_frame_size(8000, 8000, MAX_FRAME_AREA));
    assert!(equal_speeds(1.0, 1.04));
    assert!(!equal_speeds(1.0, 1.2));
    assert_eq!(
        max_pixels_for_area_limit(1920 * 1080),
        1920 * 1080 + (1920 * 1080) / 32 + 65_536
    );
    assert_eq!(aligned_frame_stride(100, 100, 0), Some(448)); // 400 rounded up to multiple of 64
}

#[test]
fn premultiply_unpremultiply_and_rotation_roundtrip() {
    let mut px = vec![200u8, 100, 50, 128, 255, 0, 0, 255, 10, 20, 30, 0];
    premultiply_rgba_inplace(&mut px);
    assert_eq!(&px[4..8], &[255, 0, 0, 255]);
    assert_eq!(&px[8..12], &[0, 0, 0, 0]);
    assert_eq!(px[0], 100); // 200 * 128 / 255 ≈ 100

    let restored = unpremultiply_rgba(&px);
    assert!((i16::from(restored[0]) - 200).abs() <= 2);
    assert_eq!(&restored[4..8], &[255, 0, 0, 255]);
    assert_eq!(&restored[8..12], &[0, 0, 0, 0]);

    // 2x3 pixel grid: top-left=(1,0,0,255), top-right=(2,0,0,255), bottom-left=(5,0,0,255).
    let src = vec![
        1, 0, 0, 255, 2, 0, 0, 255, // row 0
        3, 0, 0, 255, 4, 0, 0, 255, // row 1
        5, 0, 0, 255, 6, 0, 0, 255, // row 2
    ];
    let (r90, w90, h90) = rotate_pixels_4bpp(&src, 2, 3, Rotation::Deg90);
    assert_eq!((w90, h90), (3, 2));
    // After 90 deg CW: top-left was bottom-left (5), top-right was top-left (1).
    assert_eq!(r90[0], 5);
    assert_eq!(r90[8], 1);

    let (r180, w180, h180) = rotate_pixels_4bpp(&r90, w90, h90, Rotation::Deg90);
    assert_eq!((w180, h180), (2, 3));
    assert_eq!(r180[0], 6);

    let (r360, w360, h360) = rotate_pixels_4bpp(&r180, w180, h180, Rotation::Deg180);
    assert_eq!((w360, h360), (2, 3));
    assert_eq!(r360, src);
}

#[test]
fn circle_and_rounded_rect_masks_clear_corners_and_preserve_center() {
    let mask = generate_circle_mask(64, 64);
    assert_eq!(mask.len(), 64 * 64);
    assert_eq!(mask[0], 0); // top-left corner outside circle
    assert_eq!(mask[63], 0); // top-right corner outside circle
    assert_eq!(mask[32 * 64 + 32], 255); // center fully opaque

    let mut rgba = vec![200u8; 64 * 64 * 4];
    apply_circle_mask_rgba(&mut rgba, 64, 64);
    assert_eq!(&rgba[0..4], &[0, 0, 0, 0]);
    assert_eq!(rgba[(32 * 64 + 32) * 4 + 3], 200);

    let mut rect_rgba = vec![255u8; 40 * 40 * 4];
    apply_rounded_rect_mask_rgba(&mut rect_rgba, 40, 40, 10);
    assert_eq!(rect_rgba[3], 0); // corner clipped
    assert_eq!(rect_rgba[(20 * 40 + 20) * 4 + 3], 255); // center untouched

    let mut semi = vec![100u8, 0, 0, 0, 0, 200, 0, 128];
    flatten_alpha_over_white(&mut semi);
    assert_eq!(&semi[0..4], &[255, 255, 255, 255]);
    assert_eq!(semi[7], 255);
}

// ---------------------------------------------------------------------------
// MP4 streaming moov check tests
// ---------------------------------------------------------------------------

#[test]
fn check_streaming_support_detects_early_vs_late_moov() {
    // Fast-start MP4: 24-byte `ftyp` followed by `moov` at offset 24 (< 128 KB).
    let mut fast = Vec::new();
    fast.extend_from_slice(&24u32.to_be_bytes());
    fast.extend_from_slice(b"ftyp");
    fast.extend_from_slice(&[0u8; 16]);
    fast.extend_from_slice(&16u32.to_be_bytes());
    fast.extend_from_slice(b"moov");
    fast.extend_from_slice(&[0u8; 8]);
    assert!(check_streaming_support(&fast));

    // Non-streamable MP4: 200 KB `mdat` + another atom starting past 128 KB before `moov`
    // (mirrors tdesktop's `lastReadPosition >= kFindMoovBefore` check in media_clip_check_streaming.cpp).
    let mut slow = Vec::new();
    slow.extend_from_slice(&16u32.to_be_bytes());
    slow.extend_from_slice(b"ftyp");
    slow.extend_from_slice(&[0u8; 8]);
    let mdat_size = 200 * 1024u32;
    slow.extend_from_slice(&mdat_size.to_be_bytes());
    slow.extend_from_slice(b"mdat");
    slow.resize(slow.len() + (mdat_size as usize - 8), 0);
    slow.extend_from_slice(&16u32.to_be_bytes());
    slow.extend_from_slice(b"free");
    slow.extend_from_slice(&[0u8; 8]);
    slow.extend_from_slice(&16u32.to_be_bytes());
    slow.extend_from_slice(b"moov");
    slow.extend_from_slice(&[0u8; 8]);
    assert!(!check_streaming_support(&slow));

    // 64-bit extended atom size (`size == 1`) before `moov` within 128 KB.
    let mut ext64 = Vec::new();
    ext64.extend_from_slice(&1u32.to_be_bytes());
    ext64.extend_from_slice(b"free");
    ext64.extend_from_slice(&32u64.to_be_bytes());
    ext64.extend_from_slice(&[0u8; 16]);
    ext64.extend_from_slice(&16u32.to_be_bytes());
    ext64.extend_from_slice(b"moov");
    ext64.extend_from_slice(&[0u8; 8]);
    assert!(check_streaming_support(&ext64));
}

// ---------------------------------------------------------------------------
// PriorityQueue, SpeedEstimator, Slice serialization & StreamingReader tests
// ---------------------------------------------------------------------------

#[test]
fn priority_queue_resets_and_preempts_on_seek() {
    let mut pq = PriorityQueue::new();
    assert!(pq.add(0));
    assert!(pq.add(PART_SIZE));
    assert!(!pq.add(PART_SIZE)); // duplicate at same priority
    assert_eq!(pq.front(), Some(0));

    // Simulate seek: reset priorities, then request a higher offset.
    pq.reset_priorities();
    assert!(pq.add(10 * PART_SIZE));
    assert_eq!(pq.front(), Some(10 * PART_SIZE));
    assert_eq!(pq.take(), Some(10 * PART_SIZE));
    assert_eq!(pq.take(), Some(0));
    assert_eq!(pq.take(), Some(PART_SIZE));
    assert!(pq.is_empty());

    // Range extraction.
    pq.add(0);
    pq.add(PART_SIZE);
    pq.add(2 * PART_SIZE);
    pq.add(8 * PART_SIZE);
    let removed = pq.take_in_range(0, 3 * PART_SIZE);
    assert_eq!(removed, vec![0, PART_SIZE, 2 * PART_SIZE]);
    assert_eq!(pq.take(), Some(8 * PART_SIZE));
}

#[test]
fn loaded_part_validation_and_speed_estimator() {
    let total = 2 * PART_SIZE + 500;
    assert!(LoadedPart::new(0, vec![0u8; PART_SIZE as usize]).is_valid(total));
    assert!(LoadedPart::new(2 * PART_SIZE, vec![0u8; 500]).is_valid(total));
    assert!(!LoadedPart::new(0, vec![0u8; 500]).is_valid(total));
    assert!(!LoadedPart::failed().is_valid(total));

    let mut est = SpeedEstimator::new();
    est.on_request_start(0, 1000);
    est.on_request_complete(0, PART_SIZE, 1500);
    est.on_request_start(PART_SIZE, 1500);
    est.on_request_complete(PART_SIZE, PART_SIZE, 2000);
    est.on_request_start(2 * PART_SIZE, 2000);
    est.on_request_complete(2 * PART_SIZE, PART_SIZE, 2500);

    let speed = est.estimate(2500).expect("speed estimate expected");
    assert!(!speed.unreliable); // 3 * 128 KB >= RELIABLE_SPEED_MIN_BYTES
    // 384 KB over 1.5 s = 262,144 bytes/sec.
    assert_eq!(speed.bytes_per_second, (3 * PART_SIZE * 1000 / 1500) as u32);
}

#[test]
fn complex_and_contiguous_slice_serialization_roundtrip() {
    let mut parts = PartsMap::new();
    parts.insert(0, vec![0xAAu8; PART_SIZE as usize]);
    parts.insert((2 * PART_SIZE) as u32, vec![0xBBu8; PART_SIZE as usize]);

    let mut encoded = serialize_complex_parts_map(&parts);
    let max_slice = DEFAULT_SLICE_SIZE as usize;
    while is_contiguous_serialization(encoded.len(), max_slice, PART_SIZE) {
        encoded.push(0);
    }
    let (decoded, _rem) = parse_cached_map(&encoded, max_slice, PART_SIZE);
    assert_eq!(decoded, parts);
}

struct SpyRangeLoader {
    data: Vec<u8>,
    cache_key: CacheKey,
    load_calls: Mutex<Vec<(u64, usize)>>,
    prefetch_calls: Mutex<Vec<(u64, usize)>>,
    priority_resets: AtomicUsize,
}

impl SpyRangeLoader {
    fn new(size: usize, key: CacheKey) -> Self {
        let data: Vec<u8> = (0..size).map(|i| ((i * 31 + 7) & 0xFF) as u8).collect();
        Self {
            data,
            cache_key: key,
            load_calls: Mutex::new(Vec::new()),
            prefetch_calls: Mutex::new(Vec::new()),
            priority_resets: AtomicUsize::new(0),
        }
    }
}

impl RangeLoader for SpyRangeLoader {
    fn size(&self) -> u64 {
        self.data.len() as u64
    }

    fn base_cache_key(&self) -> CacheKey {
        self.cache_key
    }

    fn load_range(&self, offset: u64, len: usize) -> tg_video::Result<Vec<u8>> {
        self.load_calls.lock().unwrap().push((offset, len));
        let off = offset as usize;
        let end = (off + len).min(self.data.len());
        Ok(self.data[off..end].to_vec())
    }

    fn prefetch(&self, offset: u64, len: usize) {
        self.prefetch_calls.lock().unwrap().push((offset, len));
    }

    fn reset_priorities(&self) {
        self.priority_resets.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn streaming_reader_128kb_parts_1mb_slices_prefetch_and_cache() {
    assert_eq!(PART_SIZE, 128 * 1024);
    assert_eq!(DEFAULT_PARTS_IN_SLICE, 8);
    assert_eq!(DEFAULT_SLICE_SIZE, 1024 * 1024);
    assert_eq!(PRELOAD_PARTS_AHEAD, 8);

    // 12 MB file (> MAX_ONLY_IN_HEADER = 10 MB) so multi-slice LRU + cache path is exercised.
    let total_size = 12 * 1024 * 1024;
    let key = CacheKey::new(0x1234, 0x5678);
    let spy = Arc::new(SpyRangeLoader::new(total_size, key));
    let cache = Arc::new(MemorySliceCache::new());

    {
        let mut reader = StreamingReader::new(
            Arc::clone(&spy) as Arc<dyn RangeLoader>,
            Some(Arc::clone(&cache) as Arc<dyn SliceCache>),
        )
        .unwrap();

        // Read header at offset 0 (4 KB) -> fetches part 0 (128 KB) and prefetches 8 parts ahead.
        let mut buf = vec![0u8; 4096];
        let n = reader.read_at(0, &mut buf).unwrap();
        assert_eq!(n, 4096);
        assert_eq!(buf, spy.data[0..4096]);
        assert_eq!(reader.stats().remote_parts_loaded, 1);
        assert_eq!(reader.stats().prefetch_requests, PRELOAD_PARTS_AHEAD);

        // Reading again inside part 0 hits memory without another load_range call.
        let mut buf2 = vec![0u8; 4096];
        reader.read_at(8192, &mut buf2).unwrap();
        assert_eq!(buf2, spy.data[8192..8192 + 4096]);
        assert_eq!(reader.stats().remote_parts_loaded, 1);

        reader.header_done();
        assert_eq!(reader.header_mode(), HeaderMode::Good);

        // Read across slices 0, 1, 2, 3 to trigger LRU eviction (> SLICES_IN_MEMORY = 2) and priority resets.
        for slice_idx in 1..=3u64 {
            let off = slice_idx * DEFAULT_SLICE_SIZE + 512;
            let mut slice_buf = vec![0u8; 1024];
            reader.read_at(off, &mut slice_buf).unwrap();
            assert_eq!(slice_buf, spy.data[off as usize..off as usize + 1024]);
        }
        assert!(spy.priority_resets.load(Ordering::Relaxed) >= 1);
        reader.finalize_cache();
    }

    assert!(!cache.is_empty());

    // Re-open with the warm cache and verify header + cached slice hits without remote fetches.
    let spy2 = Arc::new(SpyRangeLoader::new(total_size, key));
    let mut reader2 = StreamingReader::new(
        Arc::clone(&spy2) as Arc<dyn RangeLoader>,
        Some(Arc::clone(&cache) as Arc<dyn SliceCache>),
    )
    .unwrap();
    let mut cached_buf = vec![0u8; 4096];
    reader2.read_at(0, &mut cached_buf).unwrap();
    assert_eq!(cached_buf, spy.data[0..4096]);
    assert_eq!(reader2.stats().remote_parts_loaded, 0);
    assert!(reader2.stats().cache_slice_hits >= 1);
}

#[test]
fn streaming_reader_enforces_not_streamable_header_limit() {
    let total_size = 12 * 1024 * 1024;
    let key = CacheKey::new(0xAAAA, 0xBBBB);
    let spy = Arc::new(SpyRangeLoader::new(total_size, key));
    let cache = Arc::new(MemorySliceCache::new());
    let mut reader = StreamingReader::with_config(
        spy,
        Some(cache),
        SliceConfig {
            max_parts_in_header: 4,
            ..SliceConfig::default()
        },
    )
    .unwrap();

    // Reading more than 4 distinct parts before `header_done()` must yield NotStreamable.
    let mut buf = [0u8; 16];
    for i in 0..4u64 {
        reader.read_at(i * PART_SIZE, &mut buf).unwrap();
    }
    let err = reader.read_at(4 * PART_SIZE, &mut buf).unwrap_err();
    assert!(matches!(
        err,
        VideoError::Streaming(StreamingError::NotStreamable)
    ));
    assert_eq!(MAX_PARTS_IN_HEADER, 64);
}

// ---------------------------------------------------------------------------
// SyncClock, FrameQueue, StreamingPlayer, StreamingDocument & Instance tests
// ---------------------------------------------------------------------------

#[test]
fn sync_clock_handles_pause_resume_speed_and_audio_sync() {
    let mut clock = SyncClock::new(0, 1.0, true);
    assert!(clock.is_paused());

    clock.resume(1000);
    assert!(!clock.is_paused());
    assert_eq!(clock.track_time(1500).track_time_ms, 500);

    // Pause for 400 ms -> track time freezes at 500 ms.
    clock.pause(1500);
    assert!(clock.is_paused());
    assert_eq!(clock.track_time(1900).track_time_ms, 500);

    // Resume at 1900 ms -> continues smoothly from 500 ms.
    clock.resume(1900);
    assert_eq!(clock.track_time(2100).track_time_ms, 700);

    // Double playback speed at 2100 ms (track=700).
    clock.set_speed(2.0, 2100);
    assert_eq!(clock.track_time(2600).track_time_ms, 1700);
    assert_eq!(world_time_delay(100, 2.0), 50);

    // External audio sync point after resume time overrides anchor.
    clock.sync_to_audio(TimePoint::new(1680, 2600));
    assert_eq!(clock.track_time(2600).track_time_ms, 1680);

    // Stale audio sync point before resume time is ignored.
    clock.sync_to_audio(TimePoint::new(0, 1800));
    assert_eq!(clock.track_time(2600).track_time_ms, 1680);
}

fn make_fake_frame(pts_ms: i64, index: usize) -> VideoFrame {
    VideoFrame {
        width: 4,
        height: 4,
        pts_ms,
        duration_ms: 33,
        index,
        has_alpha: false,
        rotation: Rotation::Deg0,
        data: FrameData::Rgba(vec![(index as u8).wrapping_mul(17); 4 * 4 * 4]),
    }
}

#[test]
fn frame_queue_drops_late_frames_and_respects_mark_as_shown() {
    let mut q = FrameQueue::new(4, false);
    q.push(make_fake_frame(0, 0));
    q.push(make_fake_frame(33, 1));
    q.push(make_fake_frame(66, 2));
    q.push(make_fake_frame(100, 3));

    // Clock jumped to 70ms -> frames 0 and 33 are dropped; frame 66 is returned!
    match q.pop_ready(70, 1.0) {
        FramePopOutcome::Ready { frame, dropped } => {
            assert_eq!(frame.pts_ms, 66);
            assert_eq!(dropped, 2);
        }
        other => panic!("expected Ready, got {other:?}"),
    }
    assert_eq!(q.dropped_total(), 2);

    // Next frame is at 100ms, still 30ms in the future.
    match q.pop_ready(70, 1.0) {
        FramePopOutcome::Wait { delay_ms } => assert_eq!(delay_ms, 30),
        other => panic!("expected Wait, got {other:?}"),
    }

    // With wait_for_mark_as_shown = true, no frames are dropped and queue blocks until shown.
    let mut strict_q = FrameQueue::new(4, true);
    strict_q.push(make_fake_frame(0, 0));
    strict_q.push(make_fake_frame(33, 1));
    match strict_q.pop_ready(100, 1.0) {
        FramePopOutcome::Ready { frame, dropped } => {
            assert_eq!(frame.pts_ms, 0);
            assert_eq!(dropped, 0);
        }
        other => panic!("expected Ready, got {other:?}"),
    }
    assert!(matches!(
        strict_q.pop_ready(100, 1.0),
        FramePopOutcome::Wait { .. }
    ));
    assert!(strict_q.mark_frame_shown());
    match strict_q.pop_ready(100, 1.0) {
        FramePopOutcome::Ready { frame, dropped } => {
            assert_eq!(frame.pts_ms, 33);
            assert_eq!(dropped, 0);
        }
        other => panic!("expected Ready after mark_frame_shown, got {other:?}"),
    }
}

struct FakeDecoder {
    info: StreamInformation,
    file_size: u64,
    frames: Vec<VideoFrame>,
    cursor: usize,
    loop_playback: bool,
    loop_shift_ms: i64,
    last_priority: Arc<Mutex<i32>>,
}

impl FakeDecoder {
    fn new(
        frame_count: usize,
        step_ms: i64,
        file_size: u64,
        last_priority: Arc<Mutex<i32>>,
    ) -> Self {
        let duration_ms = (frame_count as i64) * step_ms;
        let frames: Vec<VideoFrame> = (0..frame_count)
            .map(|i| make_fake_frame((i as i64) * step_ms, i))
            .collect();
        let info = StreamInformation {
            video: Some(VideoInformation {
                state: TrackState {
                    position_ms: 0,
                    received_till_ms: 0,
                    duration_ms,
                },
                size: Size::new(4, 4),
                real_size: Size::new(4, 4),
                aspect: Rational::NORMAL_ASPECT,
                rotation: Rotation::Deg0,
                fps: 1000.0 / (step_ms as f64),
                has_alpha: false,
                codec_name: "fake".to_string(),
                cover: frames.first().cloned(),
            }),
            audio: None,
            header_size: 4096,
            format_name: "fake".to_string(),
        };
        Self {
            info,
            file_size,
            frames,
            cursor: 0,
            loop_playback: false,
            loop_shift_ms: 0,
            last_priority,
        }
    }
}

impl Decoder for FakeDecoder {
    fn info(&self) -> &StreamInformation {
        &self.info
    }

    fn file_size(&self) -> u64 {
        self.file_size
    }

    fn is_remote(&self) -> bool {
        true
    }

    fn next_frame(&mut self) -> tg_video::Result<DecodedPacket> {
        if self.cursor >= self.frames.len() {
            if !self.loop_playback {
                return Ok(DecodedPacket::EndOfStream);
            }
            let dur = self
                .info
                .video
                .as_ref()
                .map_or(1000, |v| v.state.duration_ms);
            self.loop_shift_ms += dur;
            self.cursor = 0;
        }
        let mut frame = self.frames[self.cursor].clone();
        frame.pts_ms += self.loop_shift_ms;
        self.cursor += 1;
        Ok(DecodedPacket::Video(frame))
    }

    fn seek(&mut self, position_ms: i64) -> tg_video::Result<()> {
        self.loop_shift_ms = 0;
        self.cursor = self
            .frames
            .iter()
            .position(|f| f.pts_ms >= position_ms)
            .unwrap_or(self.frames.len().saturating_sub(1));
        Ok(())
    }

    fn set_loop(&mut self, loop_playback: bool) {
        self.loop_playback = loop_playback;
    }

    fn set_loader_priority(&mut self, priority: i32) {
        *self.last_priority.lock().unwrap() = priority;
    }
}

#[test]
fn streaming_player_document_and_instances_coordinate_priority_and_quality() {
    let prio = Arc::new(Mutex::new(0));
    let dec = FakeDecoder::new(10, 100, 1_000_000, Arc::clone(&prio));
    let opts = PlaybackOptions {
        mode: PlaybackMode::Video,
        ..PlaybackOptions::default()
    };
    let mut player = StreamingPlayer::new(dec, opts).unwrap();
    assert_eq!(player.stage(), PlayerStage::Ready);

    // Step through frames at 0ms and 350ms (dropping 100ms and 200ms).
    let f0 = player.step_video(0).unwrap();
    assert!(matches!(f0, FramePopOutcome::Ready { frame, dropped: 0 } if frame.pts_ms == 0));
    let f3 = player.step_video(350).unwrap();
    assert!(matches!(f3, FramePopOutcome::Ready { frame, dropped: 2 } if frame.pts_ms == 300));

    // Seek back to 200ms.
    player.seek(200, Some(350)).unwrap();
    let f2 = player.step_video(350).unwrap();
    assert!(matches!(f2, FramePopOutcome::Ready { frame, .. } if frame.pts_ms == 200));

    // Loop mode wraps past 1000ms.
    player.set_loop(true);
    let mut saw_looped = false;
    for _ in 0..20 {
        if let Some(f) = player.next_video_frame().unwrap()
            && f.pts_ms >= 1000
        {
            saw_looped = true;
            break;
        }
    }
    assert!(saw_looped);

    // Test StreamingDocument + StreamingInstance priority arbitration and quality switch.
    let prio2 = Arc::new(Mutex::new(0));
    let dec2 = FakeDecoder::new(100, 100, 2_000_000, Arc::clone(&prio2)); // 10s, 2 MB = 200 KB/s
    let player2 = StreamingPlayer::new(dec2, PlaybackOptions::default()).unwrap();
    let qualities = vec![
        QualityDescriptor {
            height: 360,
            size_in_bytes: 800_000,
        },
        QualityDescriptor {
            height: 720,
            size_in_bytes: 4_000_000, // 400 KB/s; needs >= 480 KB/s at 1.2x
        },
        QualityDescriptor {
            height: 1080,
            size_in_bytes: 8_000_000, // 800 KB/s; needs >= 960 KB/s at 1.2x
        },
    ];
    let doc = StreamingDocument::new(player2, qualities);
    let mut inst_a = doc.create_instance();
    let inst_b = doc.create_instance();
    assert_eq!(doc.instance_count(), 2);

    inst_a.set_priority(5);
    inst_b.set_priority(2);
    assert_eq!(*prio2.lock().unwrap(), 5);
    inst_a.lock_player();
    assert!(doc.with_player(|p| p.is_locked()).unwrap());
    drop(inst_a);
    assert!(!doc.with_player(|p| p.is_locked()).unwrap());
    assert_eq!(*prio2.lock().unwrap(), 2);

    // High speed + >= 4s preloaded -> upgrades to 720p (600 KB/s >= 480 KB/s, < 960 KB/s).
    let up = doc.update_network_state(
        SpeedEstimate {
            bytes_per_second: 600_000,
            unreliable: false,
        },
        TrackState {
            position_ms: 1000,
            received_till_ms: 6000,
            duration_ms: 10_000,
        },
        false,
    );
    assert_eq!(up, Some(720));

    // Buffering stall -> downgrades to 360p.
    let down = doc.update_network_state(
        SpeedEstimate {
            bytes_per_second: 100_000,
            unreliable: false,
        },
        TrackState {
            position_ms: 1000,
            received_till_ms: 1500,
            duration_ms: 10_000,
        },
        true,
    );
    assert_eq!(down, Some(360));
    drop(inst_b);

    // Verify updates queue drained cleanly.
    let updates = player.drain_updates();
    assert!(
        updates
            .iter()
            .any(|u| matches!(u, StreamingUpdate::Information(_)))
    );
}

// ---------------------------------------------------------------------------
// Real FFmpeg end-to-end integration tests (auto-skip if ffmpeg.exe is missing)
// ---------------------------------------------------------------------------

fn find_ffmpeg_exe() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("FFMPEG_EXE") {
        let p = PathBuf::from(env_path);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(ffmpeg_dir) = std::env::var("FFMPEG_DIR") {
        let bin_name = if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        };
        let candidate = PathBuf::from(ffmpeg_dir).join("bin").join(bin_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| PathBuf::from("ffmpeg"))
}

fn run_ffmpeg(exe: &Path, args: &[&str]) {
    let out = Command::new(exe)
        .args(args)
        .output()
        .expect("failed to invoke ffmpeg executable");
    assert!(
        out.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn ffmpeg_h264_mp4_decode_rgba_yuv_audio_seek_loop_stream_and_thumbnail() {
    let Some(ffmpeg_exe) = find_ffmpeg_exe() else {
        eprintln!("skipping ffmpeg H.264 MP4 test: ffmpeg.exe not found");
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let mp4_path = dir.path().join("test_av.mp4");
    let mp4_str = mp4_path.to_str().unwrap();

    // Generate a 2-second 160x120 15fps H.264 MP4 with 440Hz AAC audio and faststart moov.
    run_ffmpeg(
        &ffmpeg_exe,
        &[
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=2:size=160x120:rate=15",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=2:sample_rate=44100",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "15",
            "-c:a",
            "aac",
            "-movflags",
            "+faststart",
            mp4_str,
        ],
    );

    // 1. Streaming moov check must pass because of +faststart.
    assert!(check_streaming_support_file(&mp4_path).unwrap());

    // 2. Decode both video (RGBA) and audio (f32 PCM) via StreamingReader over MemoryRangeLoader.
    let raw_bytes = std::fs::read(&mp4_path).unwrap();
    let loader: Arc<dyn RangeLoader> = Arc::new(
        MemoryRangeLoader::with_cache_key(raw_bytes.clone(), CacheKey::new(1, 1)).unwrap(),
    );
    let cache: Arc<dyn SliceCache> = Arc::new(MemorySliceCache::new());
    let opts = PlaybackOptions {
        mode: PlaybackMode::Both,
        output_format: OutputPixelFormat::Rgba,
        audio_sample_rate: 48_000,
        audio_channels: 2,
        ..PlaybackOptions::default()
    };
    let mut dec = FfmpegStreamDecoder::from_range_loader(
        loader,
        Some(cache),
        SliceConfig::default(),
        opts.clone(),
    )
    .unwrap();

    let v_info = dec.info().video.as_ref().expect("video stream expected");
    assert_eq!(v_info.size, Size::new(160, 120));
    assert_eq!(v_info.codec_name, "h264");
    assert!(v_info.state.duration_ms >= 1900);
    assert!(dec.info().audio.is_some());

    let mut video_frames = Vec::new();
    let mut audio_samples = 0usize;
    loop {
        match dec.next_frame().unwrap() {
            DecodedPacket::Video(f) => video_frames.push(f),
            DecodedPacket::Audio(a) => {
                assert_eq!(a.sample_rate, 48_000);
                assert_eq!(a.channels, 2);
                audio_samples += a.samples.len();
            }
            DecodedPacket::EndOfStream => break,
            DecodedPacket::Pending => unreachable!("FfmpegStreamDecoder decodes synchronously"),
        }
    }
    assert!(
        video_frames.len() >= 25,
        "decoded {} frames",
        video_frames.len()
    );
    assert!(
        audio_samples > 10_000,
        "decoded {audio_samples} audio samples"
    );
    assert_eq!(video_frames[0].data.format(), FrameFormat::Rgba);
    assert_eq!(video_frames[0].data.as_rgba().unwrap().len(), 160 * 120 * 4);
    assert!(video_frames.last().unwrap().pts_ms >= 1500);

    // 3. Exact seek to 1000ms.
    dec.seek(1000).unwrap();
    let seeked = loop {
        match dec.next_frame().unwrap() {
            DecodedPacket::Video(f) => break f,
            DecodedPacket::Audio(_) | DecodedPacket::Pending => continue,
            DecodedPacket::EndOfStream => panic!("unexpected EOF after seek"),
        }
    };
    assert!(
        seeked.pts_ms >= 900 && seeked.pts_ms <= 1200,
        "seeked pts was {}ms",
        seeked.pts_ms
    );

    // 4. Looping playback wraps seamlessly past duration.
    let mut loop_dec = FfmpegStreamDecoder::from_bytes(
        raw_bytes.clone(),
        PlaybackOptions {
            mode: PlaybackMode::Video,
            loop_playback: true,
            ..PlaybackOptions::default()
        },
    )
    .unwrap();
    let mut looped_count = 0usize;
    let mut max_pts = 0i64;
    for _ in 0..45 {
        if let DecodedPacket::Video(f) = loop_dec.next_frame().unwrap() {
            looped_count += 1;
            max_pts = max_pts.max(f.pts_ms);
        }
    }
    assert_eq!(looped_count, 45);
    assert!(max_pts >= 2200, "looped max_pts={max_pts}ms");

    // 5. YUV420P zero-conversion output.
    let mut yuv_dec = FfmpegStreamDecoder::from_bytes(
        raw_bytes,
        PlaybackOptions {
            mode: PlaybackMode::Video,
            output_format: OutputPixelFormat::PreferYuv,
            ..PlaybackOptions::default()
        },
    )
    .unwrap();
    if let DecodedPacket::Video(yf) = yuv_dec.next_frame().unwrap() {
        match yf.data {
            FrameData::Yuv420(yuv) => {
                assert_eq!(yuv.y.len(), 160 * 120);
                assert_eq!(yuv.chroma_width, 80);
                assert_eq!(yuv.chroma_height, 60);
                assert_eq!(yuv.u.len(), 80 * 60);
                assert_eq!(yuv.v.len(), 80 * 60);
            }
            other => panic!("expected Yuv420, got {:?}", other.format()),
        }
    } else {
        panic!("expected video frame");
    }

    // 6. Thumbnail extraction & PNG/JPEG encoding.
    let thumb = extract_thumbnail(
        &mp4_path,
        ThumbnailOptions {
            position_ms: 500,
            max_size: Some(Size::new(80, 80)),
            keep_alpha: false,
            round_circle: true,
        },
    )
    .unwrap();
    assert_eq!(thumb.width, 80);
    assert_eq!(thumb.height, 60);
    assert_eq!(thumb.data.as_rgba().unwrap()[3], 0); // corner masked out by circle
    let png = encode_thumbnail(&thumb, ThumbnailEncoding::Png).unwrap();
    assert_eq!(&png[0..8], b"\x89PNG\r\n\x1a\n");
    let jpg = encode_thumbnail(&thumb, ThumbnailEncoding::Jpeg).unwrap();
    assert_eq!(&jpg[0..2], &[0xFF, 0xD8]);
}

#[test]
fn ffmpeg_vp9_webm_with_alpha_preserves_transparency_and_clip_reader_loops() {
    let Some(ffmpeg_exe) = find_ffmpeg_exe() else {
        eprintln!("skipping ffmpeg VP9 WebM alpha test: ffmpeg.exe not found");
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let webm_path = dir.path().join("sticker_alpha.webm");
    let webm_str = webm_path.to_str().unwrap();

    // Create a 0.5-second 128x128 10fps VP9 WebM with alpha: left half transparent, right half opaque red.
    run_ffmpeg(
        &ffmpeg_exe,
        &[
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:size=128x128:rate=10:duration=0.5,format=rgba,geq=r='255':g='0':b='0':a='if(lt(X,64),0,255)'",
            "-c:v",
            "libvpx-vp9",
            "-pix_fmt",
            "yuva420p",
            "-auto-alt-ref",
            "0",
            webm_str,
        ],
    );

    // ClipReader detects WebM sticker, preserves alpha via libvpx-vp9, and auto-loops!
    let mut clip = ClipReader::from_file(&webm_path, ClipMode::Normal).unwrap();
    assert!(clip.is_webm_sticker());
    assert!(!clip.is_gifv());
    assert!(!clip.has_audio());

    let frame = clip.next_frame().unwrap();
    assert_eq!((frame.width, frame.height), (128, 128));
    assert!(frame.has_alpha, "VP9 WebM alpha channel must be detected");
    let rgba = frame.data.as_rgba().unwrap();

    // Left half pixel (16, 64) should be transparent (alpha ~ 0).
    let left_alpha = rgba[(64 * 128 + 16) * 4 + 3];
    // Right half pixel (96, 64) should be opaque red (alpha ~ 255, r > 200).
    let right_idx = (64 * 128 + 96) * 4;
    assert!(
        left_alpha < 20,
        "expected transparent left half, got alpha={left_alpha}"
    );
    assert!(
        rgba[right_idx + 3] > 235,
        "expected opaque right half, got alpha={}",
        rgba[right_idx + 3]
    );
    assert!(
        rgba[right_idx] > 180,
        "expected red channel > 180, got {}",
        rgba[right_idx]
    );

    // Advance past 5 frames to verify ClipReader auto-loops seamlessly.
    for _ in 0..8 {
        let _ = clip.next_frame().unwrap();
    }
    assert!(clip.loop_count() >= 1);

    // FrameGenerator also decodes WebM frames sequentially with `last` marker.
    let mut generator = FrameGenerator::from_file(&webm_path).unwrap();
    let mut count = 0usize;
    let mut saw_last = false;
    while let Some(gf) = generator.render_next(Some(Size::new(64, 64))).unwrap() {
        assert_eq!((gf.frame.width, gf.frame.height), (64, 64));
        assert!(gf.frame.has_alpha);
        if gf.last {
            saw_last = true;
        }
        count += 1;
    }
    assert!(count >= 4);
    assert!(saw_last);
    generator.jump_to_start().unwrap();
    assert!(generator.render_next(None).unwrap().is_some());
}

#[test]
fn ffmpeg_rotation_metadata_swaps_dimensions_and_rotates_pixels() {
    let Some(ffmpeg_exe) = find_ffmpeg_exe() else {
        eprintln!("skipping ffmpeg rotation test: ffmpeg.exe not found");
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let raw_path = dir.path().join("raw_160x96.mp4");
    let mp4_path = dir.path().join("rotated_90.mp4");
    let raw_str = raw_path.to_str().unwrap();
    let mp4_str = mp4_path.to_str().unwrap();

    // Generate 160x96 video, then attach a 90-degree displaymatrix side-data rotation.
    run_ffmpeg(
        &ffmpeg_exe,
        &[
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=0.4:size=160x96:rate=10",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            raw_str,
        ],
    );
    run_ffmpeg(
        &ffmpeg_exe,
        &[
            "-y",
            "-display_rotation:v:0",
            "90",
            "-i",
            raw_str,
            "-c",
            "copy",
            mp4_str,
        ],
    );

    let mut dec = FfmpegStreamDecoder::from_file(
        &mp4_path,
        PlaybackOptions {
            mode: PlaybackMode::Video,
            apply_rotation: true,
            ..PlaybackOptions::default()
        },
    )
    .unwrap();

    let v_info = dec.info().video.as_ref().unwrap();
    assert!(
        v_info.rotation.swaps_width_height(),
        "expected 90 or 270 deg rotation, got {:?}",
        v_info.rotation
    );
    assert_eq!(v_info.size, Size::new(96, 160));

    if let DecodedPacket::Video(frame) = dec.next_frame().unwrap() {
        assert_eq!((frame.width, frame.height), (96, 160));
    } else {
        panic!("expected rotated video frame");
    }
}
