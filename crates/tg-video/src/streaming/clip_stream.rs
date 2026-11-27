//! A [`ClipReader`] over a file that is still downloading, opened and decoded on its own thread
//! (TGB-3116).
//!
//! tdesktop plays a video message through `Media::Streaming` (`Gif::playAnimation`), whose
//! reader and decoders run off the main thread. Reads from a [`RangeLoader`] block until their
//! parts arrive, so here the UI only posts the time it wants a frame for and takes the newest
//! frame the worker has decoded: it never waits. The calls mirror [`ClipReader`], so the inline
//! player drives a streamed clip the same way as a local one, one repaint behind.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use crate::clip::{ClipMode, ClipReader};
use crate::streaming::loader::RangeLoader;
use crate::types::{Size, VideoFrame};

/// What the UI asked the worker for last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wanted {
    /// [`ClipReader::read_frames_till`].
    FramesTill(i64),
    /// [`ClipReader::inspect_at`].
    Seek(i64),
}

#[derive(Default)]
struct State {
    wanted: Option<Wanted>,
    closed: bool,
    opened: bool,
    duration_ms: i64,
    /// The newest decoded frame the UI has not taken yet.
    frame: Option<VideoFrame>,
    presentation_ms: i64,
    real_ms: i64,
    loop_count: u64,
    error: Option<String>,
}

type Wake = Box<dyn Fn() + Send + Sync>;

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    wake: Wake,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn fail(&self, error: String) {
        self.lock().error = Some(error);
        (self.wake)();
    }
}

/// A clip decoded on a background thread from a [`RangeLoader`]. Dropping it stops the worker
/// after its current read; the loader is left running (the sound track may share it).
pub struct StreamedClip {
    shared: Arc<Shared>,
}

impl StreamedClip {
    /// Starts the worker: it opens the clip, decodes the first frame at once and then the
    /// frames the UI asks for. `wake` runs on the worker after every new frame or error (a
    /// repaint request).
    #[must_use]
    pub fn spawn(
        loader: Arc<dyn RangeLoader>,
        mode: ClipMode,
        resize: Option<Size>,
        round_circle: bool,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            wake: Box::new(wake),
        });
        let worker = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("tg-clip-stream".to_owned())
            .spawn(move || run(&worker, loader, mode, resize, round_circle));
        if let Err(err) = spawned {
            shared.fail(err.to_string());
        }
        Self { shared }
    }

    /// Asks for the frame due at `target_ms` on the playback clock
    /// ([`ClipReader::read_frames_till`]). A pending seek goes first.
    pub fn request_frames_till(&self, target_ms: i64) {
        let mut state = self.shared.lock();
        match state.wanted {
            Some(Wanted::Seek(_)) => return,
            Some(Wanted::FramesTill(pending)) if pending >= target_ms => return,
            _ => {}
        }
        state.wanted = Some(Wanted::FramesTill(target_ms));
        drop(state);
        self.shared.changed.notify_all();
    }

    /// Asks for the frame at `position_ms` in the stream, restarting the clock there
    /// ([`ClipReader::inspect_at`]).
    pub fn request_seek(&self, position_ms: i64) {
        let mut state = self.shared.lock();
        state.wanted = Some(Wanted::Seek(position_ms));
        state.frame = None;
        drop(state);
        self.shared.changed.notify_all();
    }

    /// The frame decoded since the last call, if any.
    #[must_use]
    pub fn take_frame(&self) -> Option<VideoFrame> {
        self.shared.lock().frame.take()
    }

    /// Whether the clip has been opened (its header is downloaded).
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.shared.lock().opened
    }

    /// [`ClipReader::duration_ms`], `0` until the clip is open.
    #[must_use]
    pub fn duration_ms(&self) -> i64 {
        self.shared.lock().duration_ms
    }

    /// [`ClipReader::frame_presentation_time_ms`] of the newest decoded frame.
    #[must_use]
    pub fn frame_presentation_time_ms(&self) -> i64 {
        self.shared.lock().presentation_ms
    }

    /// [`ClipReader::frame_real_time_ms`] of the newest decoded frame.
    #[must_use]
    pub fn frame_real_time_ms(&self) -> i64 {
        self.shared.lock().real_ms
    }

    /// [`ClipReader::loop_count`].
    #[must_use]
    pub fn loop_count(&self) -> u64 {
        self.shared.lock().loop_count
    }

    /// Why the clip stopped: it could not be opened or read. The worker has exited then.
    #[must_use]
    pub fn error(&self) -> Option<String> {
        self.shared.lock().error.clone()
    }
}

impl Drop for StreamedClip {
    fn drop(&mut self) {
        self.shared.lock().closed = true;
        self.shared.changed.notify_all();
    }
}

fn run(
    shared: &Shared,
    loader: Arc<dyn RangeLoader>,
    mode: ClipMode,
    resize: Option<Size>,
    round_circle: bool,
) {
    let mut reader = match ClipReader::from_range_loader(loader, mode) {
        Ok(reader) => reader,
        Err(err) => return shared.fail(err.to_string()),
    };
    reader.set_resize(resize);
    reader.set_round_circle(round_circle);
    {
        let mut state = shared.lock();
        state.opened = true;
        state.duration_ms = reader.duration_ms();
    }
    // The first frame is decoded at once, so the circle shows a picture before playing.
    let mut first = Some(Wanted::FramesTill(0));
    loop {
        let wanted = match first.take() {
            Some(wanted) => wanted,
            None => {
                let state = shared.lock();
                let mut state = shared
                    .changed
                    .wait_while(state, |s| !s.closed && s.wanted.is_none())
                    .unwrap_or_else(PoisonError::into_inner);
                if state.closed {
                    return;
                }
                let Some(next) = state.wanted else {
                    continue;
                };
                if let Wanted::FramesTill(target_ms) = next
                    && state.presentation_ms > target_ms
                {
                    // The newest frame already covers that time.
                    state.wanted = None;
                    continue;
                }
                next
            }
        };
        let decoded = match wanted {
            Wanted::FramesTill(target_ms) => reader.read_frames_till(target_ms),
            Wanted::Seek(position_ms) => reader.inspect_at(position_ms),
        };
        let mut state = shared.lock();
        match decoded {
            Ok(frame) => {
                state.frame = Some(frame);
                state.presentation_ms = reader.frame_presentation_time_ms();
                state.real_ms = reader.frame_real_time_ms();
                state.loop_count = reader.loop_count();
                if state.wanted == Some(wanted) {
                    state.wanted = None;
                }
            }
            Err(err) => state.error = Some(err.to_string()),
        }
        let failed = state.error.is_some();
        drop(state);
        (shared.wake)();
        if failed {
            return;
        }
    }
}
