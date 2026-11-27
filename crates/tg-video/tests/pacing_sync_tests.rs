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
//
// The harness steps the player like the media viewer (one `step_video` per 60 Hz UI frame) but
// hands it a virtual UI clock that advances exactly 16 ms per step. With the wall clock, a busy
// machine stretched the test's own `sleep(16)` to ~65 ms (measured under 2x core-count busy
// threads, TGB-3076); two frames were then due per step and the player rightly skipped one, as
// tdesktop's `VideoTrack` skips stale frames. Every such drop had the decoder queue full, so it
// measured the test thread's scheduling, not the player. The network stall is a gate the test
// opens once playback has waited for data, so its length no longer races the frame buffer.

const FPS_MS: i64 = 33;
const FRAMES: usize = 90;
const STALL_AT: usize = 30;
const STALL_MS: u64 = 1_200;
const UI_STEP_MS: i64 = 16;
/// Steps playback must wait for data before the test lets "the network" deliver.
const WAIT_STEPS_BEFORE_DATA: usize = 10;

/// How the fake network delivers frame `STALL_AT`.
enum Stall {
    None,
    /// A blocking read of fixed length (the synchronous comparison below).
    Sleep(Duration),
    /// Blocks until the test opens the gate.
    Gate(Arc<AtomicBool>),
}

/// 30 fps fake stream; frame `STALL_AT` waits for "the network" like a
/// `RangeLoader::load_range` that has to fetch a part, with the io-wait flag raised.
struct StallingDecoder {
    info: StreamInformation,
    next: usize,
    stall: Stall,
    stall_at: usize,
    /// The stall is a network wait (io-wait flag raised); `false` models a decoder thread that
    /// is merely slow (a busy CPU).
    stall_is_io: bool,
    seek_sleep_ms: u64,
    io_waiting: Arc<AtomicBool>,
    audio: VecDeque<AudioFrame>,
}

impl StallingDecoder {
    fn new(stall: Stall) -> Self {
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
            stall,
            stall_at: STALL_AT,
            stall_is_io: true,
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
            self.io_waiting.store(self.stall_is_io, Ordering::Release);
            match &self.stall {
                Stall::None => {}
                Stall::Sleep(d) => thread::sleep(*d),
                Stall::Gate(open) => {
                    let started = Instant::now();
                    while !open.load(Ordering::Acquire) && started.elapsed() < MAX_TEST_TIME {
                        thread::sleep(Duration::from_millis(2));
                    }
                }
            }
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

/// Wall-clock bound of one scenario (a hung player fails instead of hanging the suite).
const MAX_TEST_TIME: Duration = Duration::from_secs(60);

struct Pacing {
    max_step_ms: f64,
    presented: Vec<usize>,
    dropped: u64,
    waited_for_data: bool,
    /// `(from, to)` of each step where the reported position went back.
    position_went_back: Vec<(i64, i64)>,
}

/// Drives `player` like the media viewer: one `step_video` per 60 Hz UI frame, on the virtual
/// UI clock (see the section comment). Opens `gate` once playback has waited for data for
/// `WAIT_STEPS_BEFORE_DATA` steps and `STALL_MS` of wall time.
fn drive_like_the_viewer<D: Decoder>(
    mut player: StreamingPlayer<D>,
    gate: Option<&AtomicBool>,
    virtual_clock: bool,
) -> Pacing {
    let base = Instant::now();
    let mut step = 0i64;
    let now = |step: i64| {
        if virtual_clock {
            step * UI_STEP_MS
        } else {
            base.elapsed().as_millis() as i64
        }
    };
    player.start(now(step));
    let mut pacing = Pacing {
        max_step_ms: 0.0,
        presented: Vec::new(),
        dropped: 0,
        waited_for_data: false,
        position_went_back: Vec::new(),
    };
    let mut last_position = i64::MIN;
    let mut waiting_steps = 0;
    let mut waiting_since = None;
    while base.elapsed() < MAX_TEST_TIME && player.stage() != PlayerStage::Finished {
        let t0 = Instant::now();
        let outcome = player.step_video(now(step)).expect("step");
        pacing.max_step_ms = pacing.max_step_ms.max(t0.elapsed().as_secs_f64() * 1000.0);
        let position = player.position_ms(now(step));
        if position < last_position {
            pacing.position_went_back.push((last_position, position));
        }
        last_position = position;
        if player.is_waiting_for_data() {
            pacing.waited_for_data = true;
            waiting_steps += 1;
            waiting_since.get_or_insert_with(Instant::now);
        }
        if let Some(gate) = gate
            && waiting_steps >= WAIT_STEPS_BEFORE_DATA
            && waiting_since
                .is_some_and(|t: Instant| t.elapsed() >= Duration::from_millis(STALL_MS))
        {
            gate.store(true, Ordering::Release);
        }
        if let FramePopOutcome::Ready { frame, .. } = outcome {
            pacing.presented.push(frame.index);
        }
        thread::sleep(Duration::from_millis(UI_STEP_MS as u64));
        step += 1;
    }
    if let Some(gate) = gate {
        gate.store(true, Ordering::Release);
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

fn check_network_stall_does_not_block_the_ui_thread_or_drop_frames() {
    let gate = Arc::new(AtomicBool::new(false));
    let player = StreamingPlayer::new(
        ThreadedDecoder::spawn(StallingDecoder::new(Stall::Gate(Arc::clone(&gate))))
            .expect("spawn"),
        options(),
    )
    .expect("player");
    let pacing = drive_like_the_viewer(player, Some(&gate), true);

    // The decoder blocks for more than STALL_MS; a step that waited for it would take that
    // long. The bound leaves room for preemption on a busy machine.
    assert!(
        pacing.max_step_ms < STALL_MS as f64 / 2.0,
        "the UI thread waited {:.0} ms in one step while the decoder waited for the network",
        pacing.max_step_ms
    );
    assert!(
        pacing.waited_for_data,
        "playback must pause for data while the decoder waits for the network"
    );
    assert_eq!(
        pacing.dropped, 0,
        "frames dropped; presented: {:?}",
        pacing.presented
    );
    assert_eq!(
        pacing.presented,
        (0..FRAMES).collect::<Vec<_>>(),
        "every frame is shown once, in order"
    );
    // The clock ran up to STARVE_GRACE_MS past the last frame before pausing and restarted at
    // the next one: the progress bar stepped back ~40 ms (found on story playback, TGB-3042).
    assert!(
        pacing.position_went_back.is_empty(),
        "position went back: {:?}",
        pacing.position_went_back
    );
}

#[test]
fn network_stall_does_not_block_the_ui_thread_or_drop_frames() {
    check_network_stall_does_not_block_the_ui_thread_or_drop_frames();
}

/// The same measurement on the synchronous decoder (how the viewer decoded before), on the
/// wall clock: it shows the stall the threaded path removes, so the harness above really
/// detects it.
#[test]
fn synchronous_decoding_shows_the_stall_the_threaded_path_removes() {
    let player = StreamingPlayer::new(
        StallingDecoder::new(Stall::Sleep(Duration::from_millis(STALL_MS))),
        options(),
    )
    .expect("player");
    let pacing = drive_like_the_viewer(player, None, false);
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
    let mut dec = ThreadedDecoder::spawn(StallingDecoder::new(Stall::Sleep(
        Duration::from_millis(STALL_MS),
    )))
    .expect("spawn");
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

fn check_first_frames_after_a_seek_are_shown_not_dropped() {
    let mut decoder = StallingDecoder::new(Stall::None);
    decoder.seek_sleep_ms = 150;
    let mut player =
        StreamingPlayer::new(ThreadedDecoder::spawn(decoder).expect("spawn"), options())
            .expect("player");
    let base = Instant::now();
    let now = |step: i64| step * UI_STEP_MS;
    player.start(now(0));
    let mut after_seek = Vec::new();
    let mut seeked = false;
    // Seek at 304 ms of UI time, then play 900 ms of UI time.
    for step in 0..75 {
        if base.elapsed() > MAX_TEST_TIME {
            break;
        }
        if !seeked && step == 19 {
            player.seek(FPS_MS * 60, Some(now(step))).expect("seek");
            seeked = true;
        }
        let dropped_before = player.dropped_frames();
        if let FramePopOutcome::Ready { frame, .. } = player.step_video(now(step)).expect("step")
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
        thread::sleep(Duration::from_millis(UI_STEP_MS as u64));
    }
    assert!(after_seek.len() >= 10, "frames after seek: {after_seek:?}");
    let expected: Vec<usize> = (60..60 + after_seek.len()).collect();
    assert_eq!(
        after_seek, expected,
        "playback resumes at the seek target, frame by frame"
    );
}

#[test]
fn first_frames_after_a_seek_are_shown_not_dropped() {
    check_first_frames_after_a_seek_are_shown_not_dropped();
}

/// After a long UI step (a 500 ms hitch) the player shows the frame due now, as tdesktop's
/// `prepareState` reads past every stale frame. Before TGB-3076 it skipped at most one queue
/// (4 frames) per step and showed frame 4 here, then 8, 12, 16: the video ran behind the clock
/// in fast forward for several steps.
#[test]
fn a_late_ui_step_shows_the_frame_due_now() {
    let mut player =
        StreamingPlayer::new(StallingDecoder::new(Stall::None), options()).expect("player");
    player.start(0);
    let first = player.step_video(0).expect("step");
    assert!(
        matches!(&first, FramePopOutcome::Ready { frame, .. } if frame.index == 0),
        "first step shows frame 0: {first:?}"
    );
    let late = player.step_video(500).expect("step");
    let FramePopOutcome::Ready { frame, dropped } = late else {
        panic!("a frame is due at 500 ms: {late:?}");
    };
    assert_eq!(frame.index, 15, "frame due at 500 ms (15 * 33 = 495 ms)");
    assert_eq!(dropped, 14, "frames 1..=14 are skipped");
    assert_eq!(player.dropped_frames(), 14);
    let next = player.step_video(528).expect("step");
    assert!(
        matches!(&next, FramePopOutcome::Ready { frame, dropped: 0 } if frame.index == 16),
        "playback goes on frame by frame: {next:?}"
    );
}

/// A decoder thread that runs late at start (first scheduled ~0.5 s after the player started on
/// a busy PC, TGB-3076) must not make playback jump from the cover frame: the player holds the
/// clock until its frame queue is full, as it does after a seek. Before, frames 1..=14 were
/// dropped here.
#[test]
fn a_late_decoder_thread_at_start_does_not_skip_the_first_frames() {
    let gate = Arc::new(AtomicBool::new(false));
    let mut decoder = StallingDecoder::new(Stall::Gate(Arc::clone(&gate)));
    decoder.stall_at = 1;
    decoder.stall_is_io = false;
    let mut player =
        StreamingPlayer::new(ThreadedDecoder::spawn(decoder).expect("spawn"), options())
            .expect("player");
    let base = Instant::now();
    player.start(0);
    let mut presented = Vec::new();
    let mut step = 0i64;
    while base.elapsed() < MAX_TEST_TIME && presented.len() < 20 {
        // The worker gets no frame past the cover for 500 ms of UI time.
        if step == 31 {
            gate.store(true, Ordering::Release);
        }
        if let FramePopOutcome::Ready { frame, .. } =
            player.step_video(step * UI_STEP_MS).expect("step")
        {
            presented.push(frame.index);
        }
        thread::sleep(Duration::from_millis(1));
        step += 1;
    }
    gate.store(true, Ordering::Release);
    assert_eq!(player.dropped_frames(), 0, "presented: {presented:?}");
    assert_eq!(presented, (0..20).collect::<Vec<_>>());
}

/// 60 fps stream with ffmpeg-style millisecond timestamps (0, 16, 33, 50, ...).
struct Fps60 {
    info: StreamInformation,
    next: usize,
}

impl Fps60 {
    fn new() -> Self {
        let mut info = StallingDecoder::new(Stall::None).info;
        if let Some(video) = &mut info.video {
            video.fps = 60.0;
            video.state.duration_ms = 4_000;
        }
        Self { info, next: 0 }
    }
}

impl Decoder for Fps60 {
    fn info(&self) -> &StreamInformation {
        &self.info
    }

    fn file_size(&self) -> u64 {
        1
    }

    fn is_remote(&self) -> bool {
        false
    }

    fn next_frame(&mut self) -> tg_video::Result<DecodedPacket> {
        if self.next >= 240 {
            return Ok(DecodedPacket::EndOfStream);
        }
        let index = self.next;
        self.next += 1;
        let pts = (index as i64 * 1000) / 60;
        let next_pts = ((index as i64 + 1) * 1000) / 60;
        Ok(DecodedPacket::Video(VideoFrame {
            width: 16,
            height: 16,
            pts_ms: pts,
            duration_ms: next_pts - pts,
            index,
            has_alpha: false,
            rotation: Rotation::Deg0,
            data: FrameData::Rgba(vec![0; 16 * 16 * 4]),
        }))
    }

    fn seek(&mut self, position_ms: i64) -> tg_video::Result<()> {
        self.next = usize::try_from(position_ms.max(0) * 60 / 1000).unwrap_or(0);
        Ok(())
    }

    fn set_loop(&mut self, _loop_playback: bool) {}
}

/// 60 fps video stepped at 60 Hz with +-2 ms of step jitter (a real frame loop) plays every
/// frame, after the start and after a seek. The clock used to restart exactly at the first
/// queued frame's pts, so every later step sat on a frame boundary: a step 1 ms early showed
/// nothing and the next one dropped a frame (seen with a 1080p60 file in `pacing_probe`,
/// TGB-3076).
#[test]
fn sixty_fps_video_with_step_jitter_drops_nothing_after_start_or_seek() {
    let mut player = StreamingPlayer::new(Fps60::new(), options()).expect("player");
    let step_at = |n: i64| {
        // The clock restarts on a late step; every other step comes early.
        let jitter = if n % 2 == 0 { 2.0 } else { -2.0 };
        (1_000.0 + n as f64 * 1000.0 / 60.0 + jitter).round() as i64
    };
    player.start(step_at(0));
    let mut presented = Vec::new();
    for n in 0..100 {
        if let FramePopOutcome::Ready { frame, .. } = player.step_video(step_at(n)).expect("step") {
            presented.push(frame.index);
        }
    }
    assert_eq!(player.dropped_frames(), 0, "after start: {presented:?}");
    assert_eq!(presented, (0..100).collect::<Vec<_>>());

    player.seek(2_000, Some(step_at(100))).expect("seek");
    presented.clear();
    for n in 100..200 {
        if let FramePopOutcome::Ready { frame, .. } = player.step_video(step_at(n)).expect("step") {
            presented.push(frame.index);
        }
    }
    assert_eq!(player.dropped_frames(), 0, "after seek: {presented:?}");
    assert_eq!(presented, (120..220).collect::<Vec<_>>());
}

/// Starts 2x core-count busy threads; they spin until the returned guard drops.
struct CpuLoad {
    stop: Arc<AtomicBool>,
    threads: Vec<thread::JoinHandle<()>>,
}

impl CpuLoad {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let n = thread::available_parallelism().map_or(8, std::num::NonZeroUsize::get) * 2;
        let threads = (0..n)
            .map(|_| {
                let stop = Arc::clone(&stop);
                thread::spawn(move || {
                    let mut x = 1u64;
                    while !stop.load(Ordering::Relaxed) {
                        x = x
                            .wrapping_mul(6_364_136_223_846_793_005)
                            .wrapping_add(1_442_695_040_888_963_407);
                        std::hint::black_box(x);
                    }
                })
            })
            .collect();
        Self { stop, threads }
    }
}

impl Drop for CpuLoad {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// TGB-3076: the two pacing checks under 2x core-count busy threads. Ignored by default (it
/// loads every core for ~15 s); run with `cargo test -p tg-video --test pacing_sync_tests --
/// --ignored`.
#[test]
#[ignore = "loads every core; run explicitly"]
fn pacing_holds_under_cpu_load() {
    let _load = CpuLoad::start();
    for _ in 0..3 {
        check_network_stall_does_not_block_the_ui_thread_or_drop_frames();
        check_first_frames_after_a_seek_are_shown_not_dropped();
    }
}
