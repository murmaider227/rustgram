//! TGB-3116: a round video message plays from a range loader while it downloads. The clip is
//! opened and decoded on a worker thread, so the first frame is shown before the last part of
//! the file has been served, and the UI never waits for a part.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tg_video::{
    ClipMode, FrameData, PART_SIZE, RangeLoader, Size, StreamedClip, VideoFrame, generate_test_clip,
};

/// Serves a file from memory, but holds every read at or after `gate_from` until
/// [`GatedLoader::release`], like the parts of a file that have not arrived yet.
struct GatedLoader {
    data: Vec<u8>,
    gate_from: AtomicU64,
    open: Mutex<bool>,
    opened: Condvar,
    served_gated: AtomicBool,
    /// Set once a read has stopped at the gate.
    waited: AtomicBool,
}

impl GatedLoader {
    fn new(data: Vec<u8>, gate_from: u64) -> Self {
        Self {
            data,
            gate_from: AtomicU64::new(gate_from),
            open: Mutex::new(false),
            opened: Condvar::new(),
            served_gated: AtomicBool::new(false),
            waited: AtomicBool::new(false),
        }
    }

    fn release(&self) {
        *self.open.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.opened.notify_all();
    }

    /// Holds every read from now on, until [`GatedLoader::release`].
    fn hold_everything(&self) {
        *self.open.lock().unwrap_or_else(PoisonError::into_inner) = false;
        self.gate_from.store(0, Ordering::SeqCst);
    }

    fn wait_until_a_read_waits(&self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !self.waited.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "no read stopped at the gate");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// Lets the test stop the worker in its repaint callback, right after it has published a
/// frame, so the frame it published stays there until the test has looked at it.
#[derive(Default)]
struct WakeGate {
    held: Mutex<bool>,
    changed: Condvar,
}

impl WakeGate {
    fn hold(&self) {
        *self.held.lock().unwrap_or_else(PoisonError::into_inner) = true;
    }

    fn release(&self) {
        *self.held.lock().unwrap_or_else(PoisonError::into_inner) = false;
        self.changed.notify_all();
    }

    fn pass(&self) {
        let held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let _held = self
            .changed
            .wait_while(held, |held| *held)
            .unwrap_or_else(PoisonError::into_inner);
    }
}

/// Releases the worker on the way out, also when an assertion fails.
struct ReleaseOnDrop(Arc<WakeGate>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

impl RangeLoader for GatedLoader {
    fn size(&self) -> u64 {
        self.data.len() as u64
    }

    fn load_range(&self, offset: u64, len: usize) -> tg_video::Result<Vec<u8>> {
        if offset + len as u64 > self.gate_from.load(Ordering::SeqCst) {
            let open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
            if !*open {
                self.waited.store(true, Ordering::SeqCst);
            }
            let _open = self
                .opened
                .wait_while(open, |open| !*open)
                .unwrap_or_else(PoisonError::into_inner);
            self.served_gated.store(true, Ordering::SeqCst);
        }
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let end = start.saturating_add(len).min(self.data.len());
        self.data
            .get(start..end)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| tg_video::StreamingError::LoadFailed.into())
    }
}

fn round_clip(dir: &std::path::Path) -> Vec<u8> {
    let path = dir.join("round.mp4");
    generate_test_clip(&path, 480, 480, 30, 8_000, true).expect("generate the test clip");
    std::fs::read(&path).expect("read the test clip")
}

fn wait_for_frame(clip: &StreamedClip, target_ms: i64) -> VideoFrame {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        clip.request_frames_till(target_ms);
        if let Some(frame) = clip.take_frame() {
            return frame;
        }
        assert!(clip.error().is_none(), "clip failed: {:?}", clip.error());
        assert!(Instant::now() < deadline, "no frame for {target_ms} ms");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait_for_seeked_frame(clip: &StreamedClip) -> VideoFrame {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(frame) = clip.take_frame() {
            return frame;
        }
        assert!(clip.error().is_none(), "clip failed: {:?}", clip.error());
        assert!(Instant::now() < deadline, "no frame after the seek");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn first_round_frame_arrives_before_the_last_part_is_served() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = round_clip(dir.path());
    let parts = (file.len() as u64).div_ceil(PART_SIZE);
    assert!(
        parts >= 3,
        "the clip spans several parts ({} bytes)",
        file.len()
    );
    let last_part = (parts - 1) * PART_SIZE;
    let loader = Arc::new(GatedLoader::new(file, last_part));
    let wakes = Arc::new(AtomicUsize::new(0));

    let started = Instant::now();
    let clip = {
        let wakes = Arc::clone(&wakes);
        StreamedClip::spawn(
            Arc::clone(&loader) as Arc<dyn RangeLoader>,
            ClipMode::Normal,
            Some(Size::new(240, 240)),
            true,
            move || {
                wakes.fetch_add(1, Ordering::SeqCst);
            },
        )
    };
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "spawning never waits for the file"
    );
    let first = wait_for_frame(&clip, 0);
    assert!(
        !loader.served_gated.load(Ordering::SeqCst),
        "the first frame came before the last part"
    );
    assert!(clip.is_open());
    assert!(clip.duration_ms() >= 7_000, "{}", clip.duration_ms());
    assert!(
        wakes.load(Ordering::SeqCst) > 0,
        "the worker asks for a repaint"
    );
    assert_eq!((first.width, first.height), (240, 240));
    let FrameData::Rgba(pixels) = &first.data else {
        panic!("RGBA frame expected");
    };
    assert_eq!(pixels[3], 0, "the corner is outside the round mask");
    assert_eq!(
        pixels[(120 * 240 + 120) * 4 + 3],
        255,
        "the centre is opaque"
    );

    // A UI call never blocks, even while the worker waits for a part that has not arrived.
    let ask = Instant::now();
    clip.request_frames_till(7_900);
    let _ = clip.take_frame();
    let _ = clip.frame_presentation_time_ms();
    assert!(ask.elapsed() < Duration::from_millis(50));

    // Once the last part is there, the end of the clip plays too.
    loader.release();
    clip.request_seek(7_500);
    let late = wait_for_seeked_frame(&clip);
    assert!(late.pts_ms >= 7_000, "frame at {} ms", late.pts_ms);
    assert!(loader.served_gated.load(Ordering::SeqCst));
}

#[test]
fn seek_restarts_the_streamed_clip_at_the_position() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = round_clip(dir.path());
    let loader = Arc::new(GatedLoader::new(file, u64::MAX));
    let clip = StreamedClip::spawn(loader, ClipMode::Normal, None, false, || {});
    let _ = wait_for_frame(&clip, 0);
    clip.request_seek(4_000);
    let frame = wait_for_seeked_frame(&clip);
    assert!(
        (3_500..=4_500).contains(&frame.pts_ms),
        "frame at {} ms",
        frame.pts_ms
    );
    assert_eq!(clip.frame_real_time_ms(), frame.pts_ms.max(4_000));
}

/// A frame the worker was decoding when the UI asked for a seek shows the old position: it
/// must never be handed out after `request_seek` returns (tdesktop drops it on a seek).
#[test]
fn a_frame_decoded_before_a_seek_is_never_shown_after_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = round_clip(dir.path());
    let loader = Arc::new(GatedLoader::new(file, u64::MAX));
    let wakes = Arc::new(WakeGate::default());
    let clip = {
        let wakes = Arc::clone(&wakes);
        StreamedClip::spawn(
            Arc::clone(&loader) as Arc<dyn RangeLoader>,
            ClipMode::Normal,
            Some(Size::new(120, 120)),
            false,
            move || wakes.pass(),
        )
    };
    let _ = wait_for_frame(&clip, 0);

    // The worker starts a seek to the end and stops in a read that has not arrived yet.
    loader.hold_everything();
    clip.request_seek(7_500);
    loader.wait_until_a_read_waits();

    // The UI seeks again while that decode is still running, then the data comes in. The
    // worker stops in its repaint callback after each frame it publishes, so the first frame
    // the UI can take is exactly the one published first.
    let _release = ReleaseOnDrop(Arc::clone(&wakes));
    wakes.hold();
    clip.request_seek(1_000);
    let clock_after_seek = clip.frame_real_time_ms();
    loader.release();
    let frame = wait_for_seeked_frame(&clip);
    assert!(
        (500..=1_500).contains(&frame.pts_ms),
        "frame at {} ms after a seek to 1000 ms",
        frame.pts_ms
    );
    assert_eq!(clip.frame_real_time_ms(), frame.pts_ms.max(1_000));
    // Like `ClipReader::inspect_at`, the clock moves to the seek at once, so a caller that
    // compares it with the audio does not seek again while the frame is decoding.
    assert_eq!(clock_after_seek, 1_000);
}

#[test]
fn a_file_that_is_not_a_video_reports_an_error() {
    let loader = Arc::new(GatedLoader::new(vec![7u8; 4096], u64::MAX));
    let clip = StreamedClip::spawn(loader, ClipMode::Normal, None, false, || {});
    let deadline = Instant::now() + Duration::from_secs(20);
    while clip.error().is_none() {
        assert!(Instant::now() < deadline, "no error for a broken file");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!clip.is_open());
    assert!(clip.take_frame().is_none());
}
