//! An [`AudioStream`] decoded on its own thread, for audio whose bytes arrive over the network.
//!
//! tdesktop plays a voice message while it downloads: `Media::Streaming::Player` reads the file
//! through a `Loader` on the streaming thread and the audio track feeds decoded frames to the
//! mixer, which plays silence and keeps its position while it waits for data
//! (`waitingForData`). Here the decoder (and every blocking read it makes) runs on a background
//! thread; the mixer only takes ready chunks from a queue, so the audio callback never waits for
//! the network.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;

use crate::error::{AudioError, AudioResult};
use crate::mixer::{AudioStream, PcmChunk};
use crate::types::DEFAULT_FREQUENCY;

/// Opens the real stream on the background thread (it may block on the network).
pub type AudioStreamOpener = Box<dyn FnOnce() -> AudioResult<Box<dyn AudioStream>> + Send>;

/// Decoded samples kept ahead of the mixer: about two seconds of 48 kHz stereo.
const MAX_QUEUED_SAMPLES: usize = 2 * 2 * DEFAULT_FREQUENCY as usize;

#[derive(Default)]
struct State {
    chunks: VecDeque<PcmChunk>,
    queued_samples: usize,
    /// Bumped by every seek; a chunk decoded before it is dropped.
    generation: u64,
    seek_to: Option<i64>,
    ended: bool,
    failed: Option<String>,
    closed: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// An [`AudioStream`] whose decoder runs on a background thread (see the module docs).
pub struct BackgroundAudioStream {
    shared: Arc<Shared>,
    duration_ms: i64,
}

impl BackgroundAudioStream {
    /// Starts a thread that opens the stream with `open` and decodes ahead of the mixer.
    /// `duration_ms` is the known length (from the message), `0` when unknown.
    ///
    /// # Errors
    /// [`AudioError::InvalidOperation`] when the thread cannot be started.
    pub fn spawn(open: AudioStreamOpener, duration_ms: i64) -> AudioResult<Self> {
        let shared = Arc::new(Shared::default());
        let worker = Arc::clone(&shared);
        thread::Builder::new()
            .name("tg-audio-stream".to_owned())
            .spawn(move || decode_loop(&worker, open))
            .map_err(|err| AudioError::InvalidOperation(format!("stream thread: {err}")))?;
        Ok(Self {
            shared,
            duration_ms: duration_ms.max(0),
        })
    }
}

fn decode_loop(shared: &Shared, open: AudioStreamOpener) {
    let mut stream = match open() {
        Ok(stream) => stream,
        Err(err) => {
            shared.lock().failed = Some(err.to_string());
            return;
        }
    };
    loop {
        let (seek, generation) = {
            let mut st = shared.lock();
            while !st.closed
                && st.seek_to.is_none()
                && (st.ended || st.failed.is_some() || st.queued_samples >= MAX_QUEUED_SAMPLES)
            {
                st = shared.wake.wait(st).unwrap_or_else(PoisonError::into_inner);
            }
            if st.closed {
                return;
            }
            (st.seek_to.take(), st.generation)
        };
        let result = match seek {
            Some(position_ms) => stream.seek_ms(position_ms).map(|()| None),
            None => stream.read_chunk(),
        };
        let mut st = shared.lock();
        if st.generation != generation {
            continue;
        }
        match result {
            Ok(Some(chunk)) => {
                st.queued_samples += chunk.samples.len();
                st.chunks.push_back(chunk);
            }
            Ok(None) if seek.is_some() => {}
            Ok(None) => st.ended = true,
            Err(err) => st.failed = Some(err.to_string()),
        }
    }
}

impl AudioStream for BackgroundAudioStream {
    fn sample_rate(&self) -> u32 {
        self.shared
            .lock()
            .chunks
            .front()
            .map_or(DEFAULT_FREQUENCY, |c| c.sample_rate)
    }

    fn channels(&self) -> usize {
        self.shared.lock().chunks.front().map_or(1, |c| c.channels)
    }

    fn duration_ms(&self) -> i64 {
        self.duration_ms
    }

    fn seek_ms(&mut self, position_ms: i64) -> AudioResult<()> {
        let mut st = self.shared.lock();
        st.generation += 1;
        st.chunks.clear();
        st.queued_samples = 0;
        st.ended = false;
        st.seek_to = Some(position_ms.max(0));
        drop(st);
        self.shared.wake.notify_one();
        Ok(())
    }

    fn read_chunk(&mut self) -> AudioResult<Option<PcmChunk>> {
        let mut st = self.shared.lock();
        if let Some(chunk) = st.chunks.pop_front() {
            st.queued_samples = st.queued_samples.saturating_sub(chunk.samples.len());
            drop(st);
            self.shared.wake.notify_one();
            return Ok(Some(chunk));
        }
        if let Some(err) = &st.failed {
            return Err(AudioError::Decode(err.clone()));
        }
        if st.ended {
            return Ok(None);
        }
        Ok(Some(PcmChunk {
            samples: Vec::new(),
            sample_rate: DEFAULT_FREQUENCY,
            channels: 2,
        }))
    }

    fn waiting_for_data(&self) -> bool {
        let st = self.shared.lock();
        st.chunks.is_empty() && !st.ended && st.failed.is_none()
    }
}

impl Drop for BackgroundAudioStream {
    fn drop(&mut self) {
        self.shared.lock().closed = true;
        self.shared.wake.notify_one();
    }
}
