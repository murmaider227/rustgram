//! Video stories as the desktop binary plays them: decoding on a worker thread (TGB-3042).
//!
//! A separate test binary, because the background-decode switch is process-wide. The story is
//! streamed through a loader whose middle part waits for "the network", like
//! `crates/tg-video/tests/pacing_sync_tests.rs` does for the media viewer. The UI clock is
//! virtual (16 ms per step) so a busy machine's scheduling does not count as dropped frames.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use tg_app_media::media::generate_test_mp4;
use tg_app_media::stories::StoryVideoPlayback;
use tg_data::{FullStoryId, PeerId};
use tg_stories::PlaybackStatus;
use tg_video::streaming::{LocalFileLoader, RangeLoader};

const UI_STEP_MS: i64 = 16;
const STALL_MS: u64 = 1_200;
const MAX_TEST_TIME: Duration = Duration::from_secs(60);

/// Serves a local file; the first read past the middle waits until `open` is set or
/// `max_stall` passed, with the time spent counted as a network wait by the decoder.
struct StallingLoader {
    inner: LocalFileLoader,
    stall_from: u64,
    stalled: AtomicBool,
    open: Arc<AtomicBool>,
    max_stall: Duration,
}

impl RangeLoader for StallingLoader {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn load_range(&self, offset: u64, len: usize) -> tg_video::Result<Vec<u8>> {
        if offset >= self.stall_from && !self.stalled.swap(true, Ordering::AcqRel) {
            let started = Instant::now();
            while !self.open.load(Ordering::Acquire) && started.elapsed() < self.max_stall {
                thread::sleep(Duration::from_millis(2));
            }
        }
        self.inner.load_range(offset, len)
    }
}

fn open_story(path: &Path, open: &Arc<AtomicBool>, max_stall: Duration) -> StoryVideoPlayback {
    let inner = LocalFileLoader::open(path).expect("open file");
    let stall_from = inner.size() / 2;
    let loader = StallingLoader {
        inner,
        stall_from,
        stalled: AtomicBool::new(false),
        open: Arc::clone(open),
        max_stall,
    };
    StoryVideoPlayback::from_range_loader(
        FullStoryId {
            peer: PeerId::from_raw(777_000),
            story: 1,
        },
        Arc::new(loader),
        true,
        0.8,
        false,
        0,
    )
    .expect("story opens")
}

struct Run {
    max_step: Duration,
    waited_for_data: bool,
    finished: bool,
    dropped: u64,
    positions_monotonic: bool,
}

/// Steps the story like the viewer, one step per 60 Hz frame on the virtual UI clock. Opens
/// `open` once playback has waited for data for `STALL_MS` of wall time.
fn drive(mut video: StoryVideoPlayback, open: &AtomicBool) -> Run {
    let base = Instant::now();
    let mut run = Run {
        max_step: Duration::ZERO,
        waited_for_data: false,
        finished: false,
        dropped: 0,
        positions_monotonic: true,
    };
    let mut waiting_since: Option<Instant> = None;
    let mut last_position = 0;
    let mut step = 0i64;
    while base.elapsed() < MAX_TEST_TIME {
        let t0 = Instant::now();
        let state = video.step(None, step * UI_STEP_MS);
        run.max_step = run.max_step.max(t0.elapsed());
        if state.position < last_position {
            println!(
                "position went back from {last_position} to {} ms at step {step}",
                state.position
            );
            run.positions_monotonic = false;
        }
        last_position = state.position;
        if video.is_waiting_for_data() {
            run.waited_for_data = true;
            if waiting_since.get_or_insert_with(Instant::now).elapsed()
                >= Duration::from_millis(STALL_MS)
            {
                open.store(true, Ordering::Release);
            }
        }
        if state.status == PlaybackStatus::StoppedAtEnd {
            run.finished = true;
            break;
        }
        thread::sleep(Duration::from_millis(UI_STEP_MS as u64));
        step += 1;
    }
    open.store(true, Ordering::Release);
    run.dropped = video.dropped_frames();
    run
}

#[test]
fn story_video_waits_for_the_network_off_the_ui_thread() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mp4 = dir.path().join("story.mp4");
    generate_test_mp4(&mp4, 640, 360, 6, false).expect("generate mp4");

    // How stories decoded before TGB-3042 (synchronously on the UI thread): the stalled read
    // blocks one UI step for the whole stall.
    tg_emoji::background::set_background_decode(false);
    let open = Arc::new(AtomicBool::new(false));
    let sync = drive(
        open_story(&mp4, &open, Duration::from_millis(STALL_MS)),
        &open,
    );
    println!("synchronous story: max UI step {:?}", sync.max_step);
    assert!(
        sync.max_step >= Duration::from_millis(STALL_MS - 50),
        "the synchronous path blocks the UI step on the read: {:?}",
        sync.max_step
    );

    // The app's path: the decoder thread waits, the UI keeps stepping and playback pauses.
    tg_emoji::background::set_background_decode(true);
    let open = Arc::new(AtomicBool::new(false));
    let threaded = drive(open_story(&mp4, &open, Duration::from_secs(30)), &open);
    println!(
        "threaded story: max UI step {:?}, waited for data {}, dropped {}",
        threaded.max_step, threaded.waited_for_data, threaded.dropped
    );
    assert!(
        threaded.max_step < Duration::from_millis(STALL_MS / 2),
        "one story step took {:?} while the decoder waited for the network",
        threaded.max_step
    );
    assert!(
        threaded.waited_for_data,
        "playback pauses while the decoder waits for the network"
    );
    assert!(threaded.finished, "the story plays to the end");
    assert_eq!(threaded.dropped, 0, "no frames dropped after the stall");
    assert!(threaded.positions_monotonic, "position never jumps back");
}
