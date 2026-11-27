//! Audio-master A/V sync of the streaming player (TGB-3041).
//!
//! tdesktop's video track takes its time from the mixer's played position
//! (`VideoTrackObject::trackTime` with `syncVideoByAudio`). A player that runs the picture on
//! the wall clock drifts away from the sound whenever the sound card's clock differs from the
//! system clock or the output stalls for a moment, and the offset never comes back. These
//! tests play five minutes through a fake sound card that runs 300 ppm fast and stalls twice,
//! and measure the offset between each presented frame and the sound heard at that moment.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tg_video::{
    AudioFrame, AudioSink, DecodedPacket, Decoder, FrameData, FramePopOutcome, PlaybackMode,
    PlaybackOptions, PlayerAudio, Rational, Rotation, Size, StreamInformation, StreamingPlayer,
    ThreadedDecoder, TimePoint, TrackState, VideoFrame, VideoInformation,
};

const FPS_MS: i64 = 33;
const AUDIO_MS: i64 = 21;
const DURATION_MS: i64 = 300_000;

/// Interleaved 30 fps video and 21 ms audio frames, in pts order like a muxed file.
struct AvDecoder {
    info: StreamInformation,
    next_video: i64,
    next_audio: i64,
}

impl AvDecoder {
    fn new() -> Self {
        let state = TrackState {
            position_ms: 0,
            received_till_ms: 0,
            duration_ms: DURATION_MS,
        };
        let info = StreamInformation {
            video: Some(VideoInformation {
                state,
                size: Size::new(2, 2),
                real_size: Size::new(2, 2),
                aspect: Rational::NORMAL_ASPECT,
                rotation: Rotation::Deg0,
                fps: 30.0,
                has_alpha: false,
                codec_name: "fake".into(),
                cover: None,
            }),
            audio: Some(tg_video::AudioInformation {
                state,
                sample_rate: 48_000,
                channels: 2,
                codec_name: "fake".into(),
            }),
            header_size: 0,
            format_name: "fake".into(),
        };
        Self {
            info,
            next_video: 0,
            next_audio: 0,
        }
    }
}

impl Decoder for AvDecoder {
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
        let video_pts = self.next_video * FPS_MS;
        let audio_pts = self.next_audio * AUDIO_MS;
        if video_pts >= DURATION_MS && audio_pts >= DURATION_MS {
            return Ok(DecodedPacket::EndOfStream);
        }
        if audio_pts < video_pts || video_pts >= DURATION_MS {
            self.next_audio += 1;
            return Ok(DecodedPacket::Audio(AudioFrame {
                sample_rate: 48_000,
                channels: 2,
                pts_ms: audio_pts,
                duration_ms: AUDIO_MS,
                samples: Vec::new(),
            }));
        }
        self.next_video += 1;
        Ok(DecodedPacket::Video(VideoFrame {
            width: 2,
            height: 2,
            pts_ms: video_pts,
            duration_ms: FPS_MS,
            index: self.next_video as usize - 1,
            has_alpha: false,
            rotation: Rotation::Deg0,
            data: FrameData::Rgba(vec![0; 16]),
        }))
    }

    fn seek(&mut self, position_ms: i64) -> tg_video::Result<()> {
        self.next_video = position_ms / FPS_MS;
        self.next_audio = position_ms / AUDIO_MS;
        Ok(())
    }

    fn set_loop(&mut self, _loop_playback: bool) {}
}

/// A sound card: plays queued audio at `rate` × the world clock, holds its position while the
/// queue is dry or the output stalls.
#[derive(Default)]
struct Card {
    queue: VecDeque<(i64, i64)>,
    pushed: Vec<i64>,
    started: bool,
    playing: bool,
    position_ms: f64,
    /// Media time left in the frame being played.
    left_in_frame: f64,
    heard_ms: f64,
    /// How long `reset` takes to run: the UI thread preempted in the middle of a seek.
    reset_delay: Duration,
}

impl Card {
    fn advance(&mut self, world_dt_ms: f64, rate: f64, stalled: bool) {
        if !self.started
            && self.playing
            && let Some(&(pts, _)) = self.queue.front()
        {
            self.started = true;
            self.position_ms = pts as f64;
        }
        if !self.started || !self.playing || stalled {
            return;
        }
        let mut want = world_dt_ms * rate;
        while want > 0.0 {
            if self.left_in_frame <= 0.0 {
                let Some((pts, dur)) = self.queue.pop_front() else {
                    return;
                };
                self.position_ms = pts as f64;
                self.left_in_frame = dur as f64;
            }
            let take = want.min(self.left_in_frame);
            self.left_in_frame -= take;
            self.position_ms += take;
            self.heard_ms += take;
            want -= take;
        }
    }
}

#[derive(Clone, Default)]
struct FakeOutput(Arc<Mutex<Card>>);

impl AudioSink for FakeOutput {
    fn push(&self, frame: AudioFrame) {
        let mut card = self.0.lock().expect("card");
        card.pushed.push(frame.pts_ms);
        card.queue.push_back((frame.pts_ms, frame.duration_ms));
    }

    fn clear(&self) {
        self.0.lock().expect("card").queue.clear();
    }
}

impl PlayerAudio for FakeOutput {
    fn sink(&self) -> Arc<dyn AudioSink> {
        Arc::new(self.clone())
    }

    fn reset(&mut self) {
        let delay = self.0.lock().expect("card").reset_delay;
        thread::sleep(delay);
        let mut card = self.0.lock().expect("card");
        card.started = false;
        card.left_in_frame = 0.0;
    }

    fn set_playing(&mut self, playing: bool) {
        self.0.lock().expect("card").playing = playing;
    }

    fn set_speed(&mut self, _speed: f64) {}

    fn set_volume(&mut self, _volume: f64) {}

    fn sync_point(&mut self, world_now_ms: i64) -> Option<TimePoint> {
        let card = self.0.lock().expect("card");
        card.started
            .then(|| TimePoint::new(card.position_ms.round() as i64, world_now_ms))
    }
}

struct SyncStats {
    max_abs_offset_ms: i64,
    final_offset_ms: i64,
    frames: usize,
    heard_ms: f64,
}

/// Plays the whole stream, stepping like the viewer (at the next frame's due time, at most one
/// 60 Hz frame apart, with a few ms of jitter), and measures each presented frame's pts minus
/// the position of the sound heard at that moment.
fn play(sync_by_audio: bool) -> SyncStats {
    const CARD_RATE: f64 = 1.0003;
    let stalls = [(100_000_i64, 120_i64), (200_000, 120)];
    let opts = PlaybackOptions {
        mode: PlaybackMode::Both,
        ..PlaybackOptions::default()
    };
    let mut player = StreamingPlayer::new(AvDecoder::new(), opts).expect("player");
    let card = FakeOutput::default();
    if sync_by_audio {
        assert!(player.attach_audio(Box::new(card.clone())));
    } else {
        // The old viewer: the picture runs on the wall clock and the sound plays on its own.
        card.0.lock().expect("card").playing = true;
    }
    let mut world: i64 = 0;
    player.start(world);
    let mut stats = SyncStats {
        max_abs_offset_ms: 0,
        final_offset_ms: 0,
        frames: 0,
        heard_ms: 0.0,
    };
    let mut jitter = 0_i64;
    loop {
        let outcome = player.step_video(world).expect("step");
        if !sync_by_audio {
            while let Some(frame) = player.pop_audio_frame() {
                card.push(frame);
            }
        }
        let delay = match &outcome {
            FramePopOutcome::Ready { frame, .. } => {
                let heard = card.0.lock().expect("card").position_ms;
                let offset = frame.pts_ms - heard.round() as i64;
                stats.max_abs_offset_ms = stats.max_abs_offset_ms.max(offset.abs());
                stats.final_offset_ms = offset;
                stats.frames += 1;
                1
            }
            FramePopOutcome::Wait { delay_ms } => (*delay_ms).clamp(1, 16),
            FramePopOutcome::Empty if player.stage() == tg_video::PlayerStage::Finished => break,
            FramePopOutcome::Empty => 16,
        };
        jitter = (jitter * 7 + 3) % 4;
        let dt = delay + jitter;
        let stalled = stalls
            .iter()
            .any(|&(at, len)| world >= at && world < at + len);
        card.0
            .lock()
            .expect("card")
            .advance(dt as f64, CARD_RATE, stalled);
        world += dt;
        assert!(world < DURATION_MS * 2, "playback never finished");
    }
    stats.heard_ms = card.0.lock().expect("card").heard_ms;
    stats
}

#[test]
fn the_picture_follows_the_sound_for_five_minutes() {
    let synced = play(true);
    let old = play(false);
    println!(
        "A/V offset over 5 min, card +300 ppm and two 120 ms output stalls: \
         audio-master max |offset| {} ms (end {} ms, {} frames); \
         wall clock max |offset| {} ms (end {} ms)",
        synced.max_abs_offset_ms,
        synced.final_offset_ms,
        synced.frames,
        old.max_abs_offset_ms,
        old.final_offset_ms
    );
    assert!(
        synced.heard_ms > (DURATION_MS - 1_000) as f64,
        "the sound played: {} ms",
        synced.heard_ms
    );
    assert!(synced.frames > 8_000, "frames presented: {}", synced.frames);
    assert!(
        synced.max_abs_offset_ms <= 40,
        "picture and sound drift apart by up to {} ms",
        synced.max_abs_offset_ms
    );
    // The measurement catches the old behaviour: on the wall clock the drift only grows.
    assert!(
        old.max_abs_offset_ms > 120,
        "wall-clock control drifted only {} ms",
        old.max_abs_offset_ms
    );
}

#[test]
fn sound_pauses_with_the_picture_and_restarts_at_the_seek_target() {
    let opts = PlaybackOptions {
        mode: PlaybackMode::Both,
        ..PlaybackOptions::default()
    };
    let mut player = StreamingPlayer::new(AvDecoder::new(), opts).expect("player");
    let card = FakeOutput::default();
    assert!(player.attach_audio(Box::new(card.clone())));
    assert!(
        !card.0.lock().expect("card").playing,
        "no sound before start"
    );
    player.start(0);
    let _ = player.step_video(0).expect("step");
    assert!(card.0.lock().expect("card").playing);

    player.pause(100);
    assert!(
        !card.0.lock().expect("card").playing,
        "pause stops the sound"
    );
    player.resume(200);
    assert!(card.0.lock().expect("card").playing);

    player.seek(60_000, Some(300)).expect("seek");
    {
        let card = card.0.lock().expect("card");
        assert!(!card.started, "the seek drops the old sound");
        assert!(
            !card.playing,
            "the sound waits for the first frames after a seek"
        );
    }
    let mut world = 300;
    for _ in 0..10 {
        let _ = player.step_video(world).expect("step");
        world += 16;
    }
    let card_state = card.0.lock().expect("card");
    assert!(card_state.playing, "the sound resumes with the picture");
    let first_after_seek = card_state
        .queue
        .front()
        .map(|&(pts, _)| pts)
        .expect("audio after the seek");
    assert!(
        (59_980..=60_000).contains(&first_after_seek),
        "sound restarts at the seek target: {first_after_seek} ms"
    );
}

#[test]
fn the_decoder_thread_feeds_the_sound_and_drops_it_on_seek() {
    let opts = PlaybackOptions {
        mode: PlaybackMode::Both,
        ..PlaybackOptions::default()
    };
    let decoder = ThreadedDecoder::spawn(AvDecoder::new()).expect("spawn");
    let mut player =
        StreamingPlayer::new(Box::new(decoder) as Box<dyn Decoder>, opts).expect("player");
    let card = FakeOutput::default();
    assert!(player.attach_audio(Box::new(card.clone())));
    player.start(0);
    // The UI thread is preempted for 30 ms inside each seek (the merge gate hit this under
    // load): the decoder thread decodes the new position meanwhile, and none of it may be lost.
    card.0.lock().expect("card").reset_delay = Duration::from_millis(30);

    let wait_for_audio = |min_pts: i64| {
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if card
                .0
                .lock()
                .expect("card")
                .pushed
                .last()
                .is_some_and(|&pts| pts >= min_pts)
            {
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
        panic!("no audio from the decoder thread past {min_pts} ms");
    };
    // The worker decodes ahead and pushes audio without the UI thread asking for it.
    wait_for_audio(200);
    assert!(
        player.pop_audio_frame().is_none(),
        "audio goes to the sound output, not the player's queue"
    );
    {
        let card = card.0.lock().expect("card");
        assert_eq!(
            card.pushed.first(),
            Some(&0),
            "nothing before the first frame is lost"
        );
        assert!(
            card.pushed.windows(2).all(|w| w[1] == w[0] + AUDIO_MS),
            "audio arrives in order without gaps"
        );
    }

    for round in 0..20 {
        let target = 120_000 + round * 5_000;
        player.seek(target, Some(1_000)).expect("seek");
        // Nothing decoded before the seek may arrive once it returned.
        let before = card.0.lock().expect("card").pushed.len();
        wait_for_audio(target + 200);
        let card = card.0.lock().expect("card");
        let stale: Vec<i64> = card.pushed[before..]
            .iter()
            .copied()
            .filter(|&pts| pts < target - AUDIO_MS)
            .collect();
        assert!(stale.is_empty(), "audio from before the seek: {stale:?}");
        assert!(
            card.queue
                .front()
                .is_some_and(|&(pts, _)| pts >= target - AUDIO_MS),
            "the queue restarts at the seek target"
        );
    }
}
