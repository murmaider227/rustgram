//! TGB-3116: a round video message that is not downloaded plays while it downloads. A click
//! asks the backend for a loader instead of the whole file; the circle shows its first frame
//! and the sound starts before the last part of the file has been served. The old code
//! downloaded the whole file first (TGB-2997) and started it only then.

use std::collections::HashSet;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tg_audio::AudioType;
use tg_video::{MemoryRangeLoader, PART_SIZE, RangeLoader};

use super::*;
use crate::history::inline_media::{InlineMediaDescriptor, RoundVideoPhase, VoiceLoadState};

const ROUND: MsgId = MsgId(63);
const PEER: PeerId = PeerId(77);

/// Serves only the parts the test released; a read of any other part waits for it.
struct GatedLoader {
    inner: MemoryRangeLoader,
    released: Mutex<HashSet<u64>>,
    changed: Condvar,
    served: Mutex<HashSet<u64>>,
}

impl GatedLoader {
    fn new(bytes: Vec<u8>) -> Arc<Self> {
        Arc::new(Self {
            inner: MemoryRangeLoader::new(bytes).expect("loader"),
            released: Mutex::new(HashSet::new()),
            changed: Condvar::new(),
            served: Mutex::new(HashSet::new()),
        })
    }

    fn part_count(&self) -> u64 {
        self.inner.size().div_ceil(PART_SIZE)
    }

    fn release(&self, part: u64) {
        self.released
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(part);
        self.changed.notify_all();
    }

    fn served(&self) -> HashSet<u64> {
        self.served
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl RangeLoader for GatedLoader {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn load_range(&self, offset: u64, len: usize) -> tg_video::Result<Vec<u8>> {
        let first = offset / PART_SIZE;
        let last = (offset + len.max(1) as u64 - 1) / PART_SIZE;
        let released = self.released.lock().unwrap_or_else(PoisonError::into_inner);
        let (_released, timeout) = self
            .changed
            .wait_timeout_while(released, Duration::from_secs(20), |r| {
                !(first..=last).all(|part| r.contains(&part))
            })
            .unwrap_or_else(PoisonError::into_inner);
        if timeout.timed_out() {
            return Err(tg_video::StreamingError::LoadFailed.into());
        }
        self.served
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(first..=last);
        self.inner.load_range(offset, len)
    }
}

/// A faststart H.264 + AAC round video spanning several parts.
fn round_file() -> Vec<u8> {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("round.mp4");
    tg_video::generate_test_clip(&path, 480, 480, 30, 8_000, true).expect("generate the clip");
    std::fs::read(&path).expect("read the clip")
}

fn not_downloaded() -> InlineMediaDescriptor {
    InlineMediaDescriptor {
        voice_load: Some(VoiceLoadState::NotLoaded),
        ..InlineMediaDescriptor::default()
    }
}

fn position_ms(rt: &HistoryMediaRuntime) -> i64 {
    let (active, _, _, _, state) = rt.audio_item_state(ROUND, AudioType::Voice);
    if active {
        state.position * 1000 / i64::from(state.frequency.max(1))
    } else {
        0
    }
}

#[test]
fn a_round_video_that_is_not_downloaded_plays_while_it_downloads() {
    let loader = GatedLoader::new(round_file());
    let parts = loader.part_count();
    assert!(parts >= 3, "the file spans several parts");
    for part in 0..parts - 1 {
        loader.release(part);
    }

    let mut rt = HistoryMediaRuntime::new(false);
    rt.register_descriptor(ROUND, not_downloaded());
    assert_eq!(rt.round_video_phase(ROUND), RoundVideoPhase::Static);
    rt.click_round_video(PEER, ROUND, "Ivo Marten", 8, 1_000);
    assert_eq!(
        rt.take_download_actions(),
        vec![(ROUND, MediaDownloadAction::Stream)],
        "the click asks for a stream, not for the whole file"
    );
    assert!(rt.is_round_video_unmuted(ROUND));
    assert_eq!(rt.round_video_phase(ROUND), RoundVideoPhase::SoundPlaying);
    assert_eq!(rt.pending_voice_msg(), None, "nothing waits for a download");

    rt.provide_voice_stream(ROUND, Some(Arc::clone(&loader) as Arc<dyn RangeLoader>));

    let ctx = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut now_ms = 1_000;
    let (mut frame_shown, mut sound_ms) = (false, 0);
    while !(frame_shown && sound_ms >= 500) {
        assert!(
            Instant::now() < deadline,
            "frame {frame_shown}, sound at {sound_ms} ms"
        );
        let frame = rt.round_clip_frame(&ctx, ROUND, 240, true, now_ms);
        assert!(
            frame.as_ref().is_some_and(|f| f.error.is_none()),
            "no error"
        );
        frame_shown |= frame.is_some_and(|f| f.texture.is_some());
        rt.advance_audio_ms(20);
        now_ms += 20;
        sound_ms = position_ms(&rt);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        !loader.served().contains(&(parts - 1)),
        "the first frame and the sound came before the last part"
    );
    assert!(rt.take_download_actions().is_empty(), "no full download");

    loader.release(parts - 1);
}

#[test]
fn a_backend_that_cannot_stream_downloads_the_round_video_first() {
    let mut rt = HistoryMediaRuntime::new(false);
    rt.register_descriptor(ROUND, not_downloaded());
    rt.click_round_video(PEER, ROUND, "Ivo Marten", 3, 1_000);
    assert_eq!(
        rt.take_download_actions(),
        vec![(ROUND, MediaDownloadAction::Stream)]
    );
    rt.provide_voice_stream(ROUND, None);
    assert!(!rt.is_round_video_unmuted(ROUND), "no sound without a file");
    assert_eq!(
        rt.take_download_actions(),
        vec![(ROUND, MediaDownloadAction::Start)],
        "the whole file downloads instead (TGB-2997)"
    );
    assert_eq!(rt.round_video_phase(ROUND), RoundVideoPhase::NotLoaded);
    assert_eq!(
        rt.pending_voice_msg(),
        Some(ROUND),
        "and plays when it is there"
    );
}
