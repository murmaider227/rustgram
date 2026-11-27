//! Heap allocations the mixer makes per decoded chunk (TGB-3068).
//!
//! `MixerTrack::ensure_buffered` built up to three new `Vec<f32>` for every chunk (stereo
//! fold, resampler output, time-stretcher output) on the audio thread. The track now keeps
//! scratch buffers. A counting global allocator measures it exactly; this is its own test
//! binary so the allocator sees nothing else.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tg_audio::types::CHECK_PLAYBACK_POSITION_DELTA;
use tg_audio::{AudioMsgId, AudioType, MemoryPcmStream, MixerCore};

struct Counting;

static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards every call to the system allocator unchanged; only counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Chunk size of [`MemoryPcmStream`].
const CHUNK_FRAMES: usize = 2048;

/// How a test drives the mixer.
#[derive(Clone, Copy, Debug)]
enum Render {
    /// `render_stereo_48k`: a new `Vec` of events per call.
    ReturningVec,
    /// `render_stereo_48k_into` with one events buffer, like the device engine (TGB-3103).
    IntoReusedBuffer,
}

/// Renders `seconds` of a mono 44.1 kHz song at `speed` (stereo fold + resample + stretch on
/// every chunk) and returns `(allocations, chunks decoded, position events)` in steady
/// state.
fn allocations_while_playing(speed: f64, seconds: usize, render: Render) -> (usize, usize, usize) {
    let rate = 44_100;
    let samples: Vec<f32> = (0..rate * 30)
        .map(|i| ((i as f32) * 0.05).sin() * 0.3)
        .collect();
    let mut mixer = MixerCore::new();
    let song = AudioMsgId {
        external_play_id: AudioMsgId::create_external_play_id(),
        audio_type: AudioType::Song,
        ..AudioMsgId::default()
    };
    mixer
        .play(
            song,
            Box::new(MemoryPcmStream::new(samples, rate as u32, 1)),
            0,
            speed,
            false,
        )
        .expect("play");
    let mut buf = vec![0.0_f32; 480 * 2];
    let mut events = Vec::with_capacity(16);
    let mut step = |mixer: &mut MixerCore| match render {
        Render::ReturningVec => {
            let _ = mixer.render_stereo_48k(&mut buf);
        }
        Render::IntoReusedBuffer => {
            events.clear();
            mixer.render_stereo_48k_into(&mut buf, &mut events);
        }
    };
    // Warm up: scratch buffers and queues reach their steady sizes.
    for _ in 0..100 {
        step(&mut mixer);
    }
    let ticks = seconds * 100;
    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    for _ in 0..ticks {
        step(&mut mixer);
    }
    COUNTING.store(false, Ordering::Relaxed);
    let consumed_frames = (ticks * 480) as f64 * speed * f64::from(rate as u32) / 48_000.0;
    (
        ALLOCATIONS.load(Ordering::Relaxed),
        (consumed_frames / CHUNK_FRAMES as f64).round() as usize,
        ticks * 480 / CHECK_PLAYBACK_POSITION_DELTA as usize,
    )
}

#[test]
fn mixer_allocates_only_the_streams_chunk_per_decoded_chunk() {
    for speed in [1.0, 1.5] {
        let (returning, _, events) = allocations_while_playing(speed, 10, Render::ReturningVec);
        let (allocations, chunks, _) =
            allocations_while_playing(speed, 10, Render::IntoReusedBuffer);
        println!(
            "speed {speed}: {allocations} allocations for ~{chunks} chunks with a reused events \
             buffer ({:.2} per chunk); {returning} when each render returns a Vec \
             ({events} position events)",
            allocations as f64 / chunks.max(1) as f64
        );
        // Only the stream's own chunk `Vec` (`MemoryPcmStream::read_chunk`) is one per chunk;
        // the mixer itself may allocate only rarely (queue growth). Before TGB-3068 it made ~3
        // more per chunk (4.93 per chunk at speed 1); before TGB-3103 every `PositionUpdated`
        // event (one per 50 ms) came in a new `Vec`.
        assert!(
            allocations <= chunks + chunks / 10 + 4,
            "speed {speed}: {allocations} allocations for {chunks} chunks"
        );
        assert!(
            returning >= allocations + events / 2,
            "speed {speed}: the Vec-returning wrapper still allocates per event batch"
        );
    }
}
