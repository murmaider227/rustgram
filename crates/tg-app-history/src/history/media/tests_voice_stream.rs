//! TGB-3000: a voice message that is not cached plays while it downloads. The first play
//! starts the track at once and asks the backend for a loader; sound starts after the parts the
//! decoder needs, not after the whole file. The old code downloaded the whole file first (and a
//! file over the inline cap never played), and moving on through the playlist stopped the
//! player while the next file downloaded, so the top bar closed and reopened.

use std::collections::HashSet;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tg_audio::{AudioType, encode_ogg_opus_i16};
use tg_video::{MemoryRangeLoader, PART_SIZE, RangeLoader};

use super::*;
use crate::history::backend::HistoryMessage;
use crate::history::inline_media::{InlineMediaDescriptor, VoiceLoadState};

const FIRST: MsgId = MsgId(51);
const SECOND: MsgId = MsgId(52);
const PEER: PeerId = PeerId(77);

/// An OGG/Opus voice file of `secs` seconds, large enough to span several 128 KB parts.
fn voice_file(secs: usize) -> Vec<u8> {
    let rate = 48_000;
    let pcm: Vec<i16> = (0..rate * secs)
        .map(|i| (((i as f32) * 0.07).sin() * 9_000.0) as i16)
        .collect();
    encode_ogg_opus_i16(
        &pcm,
        rate as u32,
        1,
        256_000,
        tg_audio::opus_codec::Application::Audio,
    )
    .expect("encode voice")
}

/// Serves only the parts the test released; a read of any other part waits for it.
struct GatedLoader {
    inner: MemoryRangeLoader,
    released: Mutex<HashSet<u64>>,
    changed: Condvar,
    asked: Mutex<Vec<u64>>,
}

impl GatedLoader {
    fn new(bytes: Vec<u8>) -> Arc<Self> {
        Arc::new(Self {
            inner: MemoryRangeLoader::new(bytes).expect("loader"),
            released: Mutex::new(HashSet::new()),
            changed: Condvar::new(),
            asked: Mutex::new(Vec::new()),
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

    fn release_all(&self) {
        for part in 0..self.part_count() {
            self.release(part);
        }
    }

    fn served(&self) -> HashSet<u64> {
        self.released
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn asked(&self) -> Vec<u64> {
        self.asked
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
        let part = offset / PART_SIZE;
        self.asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(part);
        let released = self.released.lock().unwrap_or_else(PoisonError::into_inner);
        let (_released, timeout) = self
            .changed
            .wait_timeout_while(released, Duration::from_secs(20), |r| !r.contains(&part))
            .unwrap_or_else(PoisonError::into_inner);
        if timeout.timed_out() {
            return Err(tg_video::StreamingError::LoadFailed.into());
        }
        self.inner.load_range(offset, len)
    }
}

fn not_cached() -> InlineMediaDescriptor {
    InlineMediaDescriptor {
        voice_load: Some(VoiceLoadState::NotLoaded),
        ..InlineMediaDescriptor::default()
    }
}

/// Advances the software mixer until `done` holds (the decoder runs on its own thread).
fn advance_until(
    rt: &mut HistoryMediaRuntime,
    what: &str,
    mut done: impl FnMut(&HistoryMediaRuntime) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done(rt) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        rt.advance_audio_ms(20);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn position_ms(rt: &HistoryMediaRuntime, msg_id: MsgId) -> i64 {
    let (active, _, _, _, state) = rt.audio_item_state(msg_id, AudioType::Voice);
    if active {
        state.position * 1000 / i64::from(state.frequency.max(1))
    } else {
        0
    }
}

#[test]
fn a_voice_that_is_not_cached_starts_at_once_and_plays_while_it_downloads() {
    let file = voice_file(12);
    let loader = GatedLoader::new(file);
    assert!(loader.part_count() >= 3, "the file spans several parts");

    let mut rt = HistoryMediaRuntime::new(false);
    rt.register_descriptor(FIRST, not_cached());
    rt.toggle_or_seek_voice(PEER, FIRST, "Ivo Marten", 12, None);
    assert_eq!(
        rt.take_download_actions(),
        vec![(FIRST, MediaDownloadAction::Stream)],
        "the play asks for a stream, not for the whole file"
    );
    assert!(!rt.voice_play_pending(FIRST));
    let (active, playing, ..) = rt.audio_item_state(FIRST, AudioType::Voice);
    assert!(
        active && playing,
        "the track starts before any byte is there"
    );

    rt.provide_voice_stream(FIRST, Some(Arc::clone(&loader) as Arc<dyn RangeLoader>));
    // The decoder needs the first part and the last one (the Ogg duration); the middle stays
    // downloading.
    loader.release(0);
    loader.release(loader.part_count() - 1);
    advance_until(&mut rt, "sound from the first part", |rt| {
        position_ms(rt, FIRST) >= 500
    });
    let (active, playing, ..) = rt.audio_item_state(FIRST, AudioType::Voice);
    assert!(active && playing);
    assert!(
        !loader.served().contains(&1),
        "sound started before the second part arrived"
    );
    assert!(
        rt.take_download_actions().is_empty(),
        "no full download is started (a file over the inline cap plays too)"
    );

    loader.release_all();
    advance_until(&mut rt, "the end of the message", |rt| {
        !rt.audio_item_state(FIRST, AudioType::Voice).0
    });
    assert!(loader.asked().contains(&1));
}

#[test]
fn moving_on_to_a_voice_that_is_not_cached_keeps_the_player_open() {
    let first = GatedLoader::new(voice_file(2));
    first.release_all();
    let second = GatedLoader::new(voice_file(2));

    let mut rt = HistoryMediaRuntime::new(false);
    // The playlist is built before the load state of the messages is known: the tracks are
    // registered first and switch to streaming once their descriptors come.
    let messages: Vec<HistoryMessage> = [FIRST, SECOND]
        .into_iter()
        .map(|id| {
            let mut msg =
                crate::history::tests_w4_text_snap::message(PEER, id.0, 7001, "Ivo Marten", "");
            msg.media = Some(HistoryMedia::Voice(VoiceAttachment {
                duration_secs: 2,
                waveform_5bit: vec![0x55; 63],
                size_bytes: 40_000,
                is_unread: false,
                transcription: None,
            }));
            msg
        })
        .collect();
    rt.sync_peer_audio_tracks(PEER, &messages);
    rt.register_descriptor(FIRST, not_cached());
    rt.register_descriptor(SECOND, not_cached());

    rt.toggle_or_seek_voice(PEER, FIRST, "Ivo Marten", 2, None);
    for (id, action) in rt.take_download_actions() {
        assert_eq!(action, MediaDownloadAction::Stream);
        let loader: Arc<dyn RangeLoader> = if id == FIRST {
            Arc::clone(&first) as Arc<dyn RangeLoader>
        } else {
            Arc::clone(&second) as Arc<dyn RangeLoader>
        };
        rt.provide_voice_stream(id, Some(loader));
    }

    // Play the first message to its end; the player moves on to the second one, which waits
    // for its loader and then for its first part, without stopping.
    let mut stopped_between = false;
    advance_until(&mut rt, "the second message", |rt| {
        rt.audio_item_state(SECOND, AudioType::Voice).0
    });
    for _ in 0..20 {
        rt.guard_unloaded_voice();
        for (id, action) in rt.take_download_actions() {
            assert_eq!((id, action), (SECOND, MediaDownloadAction::Stream));
            rt.provide_voice_stream(id, Some(Arc::clone(&second) as Arc<dyn RangeLoader>));
        }
        rt.advance_audio_ms(20);
        stopped_between |= !rt.audio_item_state(SECOND, AudioType::Voice).0;
    }
    assert!(
        !stopped_between,
        "the player waits for the second file instead of stopping"
    );
    let waiting_at = position_ms(&rt, SECOND);
    assert_eq!(waiting_at, 0, "silence while the first part is missing");
    second.release_all();
    advance_until(&mut rt, "sound of the second message", |rt| {
        position_ms(rt, SECOND) >= 300
    });
}
