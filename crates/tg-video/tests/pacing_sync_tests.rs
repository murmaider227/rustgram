//! A/V sync and frame pacing measurements for the streaming player (TGB-3031).
//!
//! - Audio must start with the picture: the decoder used to drop every audio packet decoded
//!   while it searched for the first video frame (at open and after a seek), so the sound
//!   started up to half a second into the stream.
//! - The UI thread must not wait for decoding or for the network: the viewer used to decode in
//!   `step_video` on the UI thread, so a slow network read froze the whole window and the
//!   frames that arrived after it were dropped as stale.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use tg_video::{
    AudioFrame, DecodedPacket, Decoder, FfmpegStreamDecoder, FrameData, FramePopOutcome,
    OutputPixelFormat, PlaybackMode, PlaybackOptions, PlayerStage, Rational, Rotation, Size,
    StreamInformation, StreamingPlayer, ThreadedDecoder, TrackState, VideoFrame, VideoInformation,
    generate_test_clip,
};

// ---------------------------------------------------------------------------------------------
// Audio starts with the picture
// ---------------------------------------------------------------------------------------------

fn first_audio_and_video_pts(dec: &mut FfmpegStreamDecoder) -> (i64, i64) {
    let (mut audio, mut video) = (None, None);
    for _ in 0..400 {
        match dec.next_frame().expect("decode") {
            DecodedPacket::Video(f) => {
                video.get_or_insert(f.pts_ms);
            }
            DecodedPacket::Audio(a) => {
                audio.get_or_insert(a.pts_ms);
            }
            DecodedPacket::EndOfStream => break,
            DecodedPacket::Pending => unreachable!("synchronous decoder"),
        }
        if let (Some(a), Some(v)) = (audio, video) {
            return (a, v);
        }
    }
    panic!("no audio/video frames: audio={audio:?} video={video:?}");
}

#[test]
fn audio_starts_with_the_first_video_frame_at_open_and_after_seek() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("av.mp4");
    generate_test_clip(&path, 320, 240, 30, 4_000, true).expect("generate clip");
    let opts = PlaybackOptions {
        mode: PlaybackMode::Both,
        output_format: OutputPixelFormat::Rgba,
        ..PlaybackOptions::default()
    };
    let mut dec = FfmpegStreamDecoder::from_file(&path, opts).expect("open");
    assert!(dec.info().audio.is_some(), "test clip has audio");

    let (audio, video) = first_audio_and_video_pts(&mut dec);
    assert!(
        (audio - video).abs() <= 50,
        "at open the first audio frame is at {audio} ms, the first video frame at {video} ms"
    );

    dec.seek(2_000).expect("seek");
    let (audio, video) = first_audio_and_video_pts(&mut dec);
    assert!(video >= 2_000, "seeked video starts at {video} ms");
    assert!(
        (2_000 - 100..=2_000 + 50).contains(&audio),
        "after a seek to 2000 ms the first audio frame is at {audio} ms (video {video} ms)"
    );
}

// ---------------------------------------------------------------------------------------------
// Pacing under a network stall
// ---------------------------------------------------------------------------------------------

const FPS_MS: i64 = 33;
const FRAMES: usize = 90;
const STALL_AT: usize = 30;
const STALL_MS: u64 = 1_200;

/// 30 fps fake stream; frame `STALL_AT` waits `STALL_MS` (longer than the decode-ahead
/// buffer) for "the network" like a
/// `RangeLoader::load_range` that has to fetch a part, with the io-wait flag raised.
struct StallingDecoder {
    info: StreamInformation,
    next: usize,
    stall_at: usize,
    seek_sleep_ms: u64,
    io_waiting: Arc<AtomicBool>,
    audio: VecDeque<AudioFrame>,
}

impl StallingDecoder {
    fn new() -> Self {
        let info = StreamInformation {
            video: Some(VideoInformation {
                state: TrackState {
                    position_ms: 0,
                    received_till_ms: 0,
                    duration_ms: FPS_MS * FRAMES as i64,
                },
                size: Size::new(16, 16),
                real_size: Size::new(16, 16),
                aspect: Rational::NORMAL_ASPECT,
                rotation: Rotation::Deg0,
                fps: 30.0,
                has_alpha: false,
                codec_name: "fake".into(),
                cover: None,
            }),
            audio: None,
            header_size: 0,
            format_name: "fake".into(),
        };
        Self {
            info,
            next: 0,
            stall_at: STALL_AT,
            seek_sleep_ms: 0,
            io_waiting: Arc::new(AtomicBool::new(false)),
            audio: VecDeque::new(),
        }
    }
}

impl Decoder for StallingDecoder {
    fn info(&self) -> &StreamInformation {
        &self.info
    }

    fn file_size(&self) -> u64 {
        1
    }

    fn is_remote(&self) -> bool {
        true
    }

    fn next_frame(&mut self) -> tg_video::Result<DecodedPacket> {
        if let Some(a) = self.audio.pop_front() {
            return Ok(DecodedPacket::Audio(a));
        }
        if self.next >= FRAMES {
            return Ok(DecodedPacket::EndOfStream);
        }
        if self.next == self.stall_at {
            self.io_waiting.store(true, Ordering::Release);
            thread::sleep(Duration::from_millis(STALL_MS));
            self.io_waiting.store(false, Ordering::Release);
        }
        let index = self.next;
        self.next += 1;
        Ok(DecodedPacket::Video(VideoFrame {
            width: 16,
            height: 16,
            pts_ms: index as i64 * FPS_MS,
            duration_ms: FPS_MS,
            index,
            has_alpha: false,
            rotation: Rotation::Deg0,
            data: FrameData::Rgba(vec![0; 16 * 16 * 4]),
        }))
    }

    fn seek(&mut self, position_ms: i64) -> tg_video::Result<()> {
        thread::sleep(Duration::from_millis(self.seek_sleep_ms));
        self.next = (position_ms / FPS_MS).max(0) as usize;
        Ok(())
    }

    fn set_loop(&mut self, _loop_playback: bool) {}

    fn io_wait_flag(&self) -> Option<&Arc<AtomicBool>> {
        Some(&self.io_waiting)
    }
}

struct Pacing {
    max_step_ms: f64,
    presented: Vec<usize>,
    dropped: u64,
    waited_for_data: bool,
}

/// Drives `player` like the media viewer: one `step_video` per 60 Hz UI frame.
fn drive_like_the_viewer<D: Decoder>(mut player: StreamingPlayer<D>) -> Pacing {
    let base = Instant::now();
    let now = || base.elapsed().as_millis() as i64;
    player.start(now());
    let mut pacing = Pacing {
        max_step_ms: 0.0,
        presented: Vec::new(),
        dropped: 0,
        waited_for_data: false,
    };
    let deadline = Duration::from_secs(10);
    while base.elapsed() < deadline && player.stage() != PlayerStage::Finished {
        let t0 = Instant::now();
        let outcome = player.step_video(now()).expect("step");
        pacing.max_step_ms = pacing.max_step_ms.max(t0.elapsed().as_secs_f64() * 1000.0);
        pacing.waited_for_data |= player.is_waiting_for_data();
        if let FramePopOutcome::Ready { frame, .. } = outcome {
            pacing.presented.push(frame.index);
        }
        thread::sleep(Duration::from_millis(16));
    }
    pacing.dropped = player.dropped_frames();
    pacing
}

fn options() -> PlaybackOptions {
    PlaybackOptions {
        mode: PlaybackMode::Video,
        ..PlaybackOptions::default()
    }
}

#[test]
fn network_stall_does_not_block_the_ui_thread_or_drop_frames() {
    let player = StreamingPlayer::new(
        ThreadedDecoder::spawn(StallingDecoder::new()).expect("spawn"),
        options(),
    )
    .expect("player");
    let pacing = drive_like_the_viewer(player);

    assert!(
        pacing.max_step_ms < 50.0,
        "the UI thread waited {:.0} ms in one step for a {STALL_MS} ms network read",
        pacing.max_step_ms
    );
    assert!(
        pacing.waited_for_data,
        "playback must pause for data while the decoder waits for the network"
    );
    assert_eq!(pacing.dropped, 0, "frames dropped after the stall");
    assert_eq!(
        pacing.presented,
        (0..FRAMES).collect::<Vec<_>>(),
        "every frame is shown once, in order"
    );
}

/// The same measurement on the synchronous decoder (how the viewer decoded before): it shows
/// the stall the threaded path removes, so the harness above really detects it.
#[test]
fn synchronous_decoding_shows_the_stall_the_threaded_path_removes() {
    let player = StreamingPlayer::new(StallingDecoder::new(), options()).expect("player");
    let pacing = drive_like_the_viewer(player);
    assert!(
        pacing.max_step_ms >= STALL_MS as f64 - 20.0,
        "synchronous step blocked only {:.0} ms",
        pacing.max_step_ms
    );
    assert!(
        pacing.dropped > 0,
        "the clock ran on during the stall, so late frames are dropped"
    );
}

#[test]
fn threaded_decoder_seek_drops_frames_decoded_before_it() {
    let mut dec = ThreadedDecoder::spawn(StallingDecoder::new()).expect("spawn");
    // Let the worker fill its queue with frames from the start.
    let start = Instant::now();
    while dec.queued_video_frames() < 4 && start.elapsed() < Duration::from_secs(5) {
        thread::sleep(Duration::from_millis(2));
    }
    dec.seek(FPS_MS * 60).expect("seek");
    let start = Instant::now();
    let first = loop {
        match dec.next_frame().expect("next") {
            DecodedPacket::Video(f) => break f,
            DecodedPacket::Pending | DecodedPacket::Audio(_) => {
                assert!(
                    start.elapsed() < Duration::from_secs(5),
                    "no frame after seek"
                );
                thread::sleep(Duration::from_millis(1));
            }
            DecodedPacket::EndOfStream => panic!("unexpected end"),
        }
    };
    assert_eq!(
        first.index, 60,
        "the first frame after a seek is at the target"
    );
}

#[test]
fn first_frames_after_a_seek_are_shown_not_dropped() {
    let mut decoder = StallingDecoder::new();
    decoder.stall_at = usize::MAX;
    decoder.seek_sleep_ms = 150;
    let mut player =
        StreamingPlayer::new(ThreadedDecoder::spawn(decoder).expect("spawn"), options())
            .expect("player");
    let base = Instant::now();
    let now = || base.elapsed().as_millis() as i64;
    player.start(now());
    let mut after_seek = Vec::new();
    let mut seeked = false;
    while base.elapsed() < Duration::from_millis(1_200) {
        if !seeked && base.elapsed() >= Duration::from_millis(300) {
            player.seek(FPS_MS * 60, Some(now())).expect("seek");
            seeked = true;
        }
        let dropped_before = player.dropped_frames();
        if let FramePopOutcome::Ready { frame, .. } = player.step_video(now()).expect("step")
            && seeked
        {
            after_seek.push(frame.index);
        }
        if seeked {
            assert_eq!(
                player.dropped_frames(),
                dropped_before,
                "dropped after seek"
            );
        }
        thread::sleep(Duration::from_millis(16));
    }
    assert!(after_seek.len() >= 10, "frames after seek: {after_seek:?}");
    let expected: Vec<usize> = (60..60 + after_seek.len()).collect();
    assert_eq!(
        after_seek, expected,
        "playback resumes at the seek target, frame by frame"
    );
}
