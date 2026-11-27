//! Runs a [`Decoder`] on its own thread so the UI thread never waits for decoding or for the
//! network.
//!
//! tdesktop decodes each track on a separate thread (`VideoTrackObject` lives on its own
//! `crl::object_on_queue`, `media_streaming_video_track.cpp`) and the UI thread only takes the
//! already decoded frame that is due. [`ThreadedDecoder`] does the same for any [`Decoder`]: a
//! worker decodes ahead into a small bounded queue, and [`Decoder::next_frame`] returns
//! [`DecodedPacket::Pending`] instead of blocking when nothing is decoded yet.

use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};

use crate::error::{Result, VideoError};
use crate::streaming::audio_output::AudioSink;
use crate::streaming::decoder::{DecodedPacket, Decoder};
use crate::types::{FrameData, StreamInformation, VideoFrame};

/// Most decoded video frames the worker keeps ready on top of the player's own frame queue.
pub const THREADED_VIDEO_AHEAD: usize = 16;

/// The worker always keeps this many frames ready, whatever their size.
const MIN_VIDEO_AHEAD: usize = 4;

/// Byte budget of decoded frames ready ahead (1080p RGBA: about 7 frames).
const VIDEO_AHEAD_BYTES: usize = 64 * 1024 * 1024;

/// Upper bound of queued packets of any kind (keeps audio-only streams bounded too).
const MAX_QUEUED_PACKETS: usize = 256;

enum Command {
    Seek(i64),
    SetLoop(bool),
    SetPriority(i32),
}

struct State {
    packets: VecDeque<Result<DecodedPacket>>,
    queued_video: usize,
    queued_video_bytes: usize,
    commands: VecDeque<Command>,
    /// Bumped on every seek; a packet decoded for an older generation is dropped.
    generation: u64,
    finished: bool,
    stop: bool,
    /// Decoded audio goes here directly instead of into `packets`.
    audio_sink: Option<Arc<dyn AudioSink>>,
}

impl State {
    /// Queues a packet the worker decoded for the current generation.
    fn accept(&mut self, packet: Result<DecodedPacket>) {
        match &packet {
            Ok(DecodedPacket::Video(frame)) => {
                self.queued_video += 1;
                self.queued_video_bytes += frame_bytes(frame);
            }
            Ok(DecodedPacket::EndOfStream) | Err(_) => self.finished = true,
            Ok(DecodedPacket::Audio(_) | DecodedPacket::Pending) => {}
        }
        if !matches!(packet, Ok(DecodedPacket::Pending)) {
            self.packets.push_back(packet);
        }
    }
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A [`Decoder`] that decodes on a background thread (see the module docs).
pub struct ThreadedDecoder {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    info: StreamInformation,
    file_size: u64,
    is_remote: bool,
    full_in_cache: bool,
    io_waiting: Option<Arc<AtomicBool>>,
}

impl std::fmt::Debug for ThreadedDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThreadedDecoder")
            .field("file_size", &self.file_size)
            .field("is_remote", &self.is_remote)
            .finish_non_exhaustive()
    }
}

impl ThreadedDecoder {
    /// Moves `decoder` to a new worker thread and starts decoding ahead. The first video frame,
    /// already decoded by the open (the cover), is taken over directly so it shows at once.
    pub fn spawn<D: Decoder + 'static>(mut decoder: D) -> Result<Self> {
        let info = decoder.info().clone();
        let file_size = decoder.file_size();
        let is_remote = decoder.is_remote();
        let full_in_cache = decoder.full_in_cache();
        let io_waiting = decoder.io_wait_flag().cloned();
        let mut state = State {
            packets: VecDeque::new(),
            queued_video: 0,
            queued_video_bytes: 0,
            commands: VecDeque::new(),
            generation: 0,
            finished: false,
            stop: false,
            audio_sink: None,
        };
        if info.video.is_some() {
            state.accept(decoder.next_frame());
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            wake: Condvar::new(),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("tg-video-decoder".into())
            .spawn(move || run_worker(decoder, &worker_shared))
            .map_err(|e| VideoError::InvalidArgument(format!("decoder thread: {e}")))?;
        Ok(Self {
            shared,
            worker: Some(worker),
            info,
            file_size,
            is_remote,
            full_in_cache,
            io_waiting,
        })
    }

    fn push_command(&self, command: Command) {
        let mut st = self.shared.lock();
        if let Command::Seek(_) = command {
            st.generation = st.generation.wrapping_add(1);
            st.packets.clear();
            st.queued_video = 0;
            st.queued_video_bytes = 0;
            st.finished = false;
            // Under the same lock as the generation bump: the worker pushes only under it, so
            // the sink holds nothing from before the seek and loses nothing after it.
            if let Some(sink) = &st.audio_sink {
                sink.clear();
            }
        }
        st.commands.push_back(command);
        drop(st);
        self.shared.wake.notify_all();
    }

    /// Number of decoded video frames waiting in the worker queue.
    #[must_use]
    pub fn queued_video_frames(&self) -> usize {
        self.shared.lock().queued_video
    }
}

impl Drop for ThreadedDecoder {
    fn drop(&mut self) {
        self.shared.lock().stop = true;
        self.shared.wake.notify_all();
        // The worker may sit in a blocking network read; let it finish on its own instead of
        // making the UI thread wait for it.
        drop(self.worker.take());
    }
}

fn run_worker<D: Decoder>(mut decoder: D, shared: &Shared) {
    loop {
        let generation;
        {
            let mut st = shared.lock();
            loop {
                if st.stop {
                    return;
                }
                if let Some(command) = st.commands.pop_front() {
                    drop(st);
                    let failed = match command {
                        Command::Seek(position_ms) => decoder.seek(position_ms).err(),
                        Command::SetLoop(on) => {
                            decoder.set_loop(on);
                            None
                        }
                        Command::SetPriority(priority) => {
                            decoder.set_loader_priority(priority);
                            None
                        }
                    };
                    st = shared.lock();
                    if let Some(err) = failed
                        && st.commands.is_empty()
                    {
                        st.packets.push_back(Err(err));
                        st.finished = true;
                        shared.wake.notify_all();
                    }
                    continue;
                }
                let full = st.queued_video >= THREADED_VIDEO_AHEAD
                    || (st.queued_video >= MIN_VIDEO_AHEAD
                        && st.queued_video_bytes >= VIDEO_AHEAD_BYTES)
                    || st.packets.len() >= MAX_QUEUED_PACKETS;
                if !st.finished && !full {
                    break;
                }
                st = shared.wake.wait(st).unwrap_or_else(PoisonError::into_inner);
            }
            generation = st.generation;
        }

        let packet = decoder.next_frame();

        let mut st = shared.lock();
        if st.generation != generation {
            continue;
        }
        // Pushed under the lock: a seek bumps the generation under it too, so no frame decoded
        // before a seek reaches the sink after the seek returned.
        match (packet, &st.audio_sink) {
            (Ok(DecodedPacket::Audio(frame)), Some(sink)) => sink.push(frame),
            (packet, _) => st.accept(packet),
        }
        drop(st);
        shared.wake.notify_all();
    }
}

impl Decoder for ThreadedDecoder {
    fn info(&self) -> &StreamInformation {
        &self.info
    }

    fn file_size(&self) -> u64 {
        self.file_size
    }

    fn is_remote(&self) -> bool {
        self.is_remote
    }

    fn full_in_cache(&self) -> bool {
        self.full_in_cache
    }

    fn next_frame(&mut self) -> Result<DecodedPacket> {
        let mut st = self.shared.lock();
        let Some(packet) = st.packets.pop_front() else {
            return Ok(DecodedPacket::Pending);
        };
        if let Ok(DecodedPacket::Video(frame)) = &packet {
            st.queued_video = st.queued_video.saturating_sub(1);
            st.queued_video_bytes = st.queued_video_bytes.saturating_sub(frame_bytes(frame));
            drop(st);
            self.shared.wake.notify_all();
        } else if matches!(packet, Ok(DecodedPacket::EndOfStream)) {
            // Keep reporting the end until a seek restarts decoding.
            st.packets.push_front(Ok(DecodedPacket::EndOfStream));
        }
        packet
    }

    fn seek(&mut self, position_ms: i64) -> Result<()> {
        self.push_command(Command::Seek(position_ms));
        Ok(())
    }

    fn set_loop(&mut self, loop_playback: bool) {
        self.push_command(Command::SetLoop(loop_playback));
    }

    fn set_loader_priority(&mut self, priority: i32) {
        self.push_command(Command::SetPriority(priority));
    }

    fn io_wait_flag(&self) -> Option<&Arc<AtomicBool>> {
        self.io_waiting.as_ref()
    }

    fn set_audio_sink(&mut self, sink: Arc<dyn AudioSink>) -> bool {
        let mut st = self.shared.lock();
        let packets = std::mem::take(&mut st.packets);
        for packet in packets {
            match packet {
                Ok(DecodedPacket::Audio(frame)) => sink.push(frame),
                other => st.packets.push_back(other),
            }
        }
        st.audio_sink = Some(sink);
        true
    }
}

fn frame_bytes(frame: &VideoFrame) -> usize {
    match &frame.data {
        FrameData::Rgba(b) | FrameData::Bgra(b) => b.len(),
        FrameData::Yuv420(_) | FrameData::Nv12(_) => {
            (frame.width as usize) * (frame.height as usize) * 3 / 2
        }
    }
}
