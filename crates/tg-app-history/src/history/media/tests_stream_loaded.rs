//! TGB-3115: a voice message that played in full from a stream counts as loaded once the
//! backend kept its file (asked for again after its track stopped), so the status line drops
//! the size and the next play reads the file instead of streaming again. A streamed round
//! video that waits for data shows tdesktop's waiting radial after half a second.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tg_audio::{AudioType, encode_ogg_opus_i16};
use tg_video::{MemoryRangeLoader, RangeLoader};

use super::*;
use crate::history::inline_media::{InlineMediaDescriptor, VoiceLoadState};

const VOICE: MsgId = MsgId(71);
const ROUND: MsgId = MsgId(72);
const PEER: PeerId = PeerId(77);

fn voice_file(secs: usize) -> Vec<u8> {
    let rate = 48_000;
    let pcm: Vec<i16> = (0..rate * secs)
        .map(|i| (((i as f32) * 0.07).sin() * 9_000.0) as i16)
        .collect();
    encode_ogg_opus_i16(
        &pcm,
        rate as u32,
        1,
        64_000,
        tg_audio::opus_codec::Application::Audio,
    )
    .expect("encode voice")
}

fn not_cached() -> InlineMediaDescriptor {
    InlineMediaDescriptor {
        voice_load: Some(VoiceLoadState::NotLoaded),
        ..InlineMediaDescriptor::default()
    }
}

#[test]
fn a_voice_played_in_full_from_a_stream_reads_as_loaded() {
    let file = voice_file(1);
    let mut rt = HistoryMediaRuntime::new(false);
    rt.register_descriptor(VOICE, not_cached());
    rt.toggle_or_seek_voice(PEER, VOICE, "Ivo Marten", 1, None);
    assert_eq!(
        rt.take_download_actions(),
        vec![(VOICE, MediaDownloadAction::Stream)]
    );
    let loader: Arc<dyn RangeLoader> =
        Arc::new(MemoryRangeLoader::new(file.clone()).expect("loader"));
    rt.provide_voice_stream(VOICE, Some(loader));

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut started = false;
    loop {
        assert!(Instant::now() < deadline, "the message plays to its end");
        let (active, ..) = rt.audio_item_state(VOICE, AudioType::Voice);
        started |= active;
        let (due, _) = rt.take_stream_rechecks();
        if active {
            assert!(due.is_empty(), "nothing is asked while it plays");
        } else if started {
            assert_eq!(due, vec![VOICE], "asked again once it stopped");
            break;
        }
        rt.advance_audio_ms(20);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(rt.voice_loaded(VOICE), Some(false));

    // The backend kept the file the stream read: the next ask finds it in the cache.
    rt.register_descriptor(
        VOICE,
        InlineMediaDescriptor {
            audio_bytes: Some(file),
            audio_ext: Some("ogg".to_owned()),
            ..InlineMediaDescriptor::default()
        },
    );
    assert_eq!(
        rt.voice_loaded(VOICE),
        Some(true),
        "no size in the status line"
    );
    assert_eq!(
        rt.take_stream_rechecks().0,
        Vec::<MsgId>::new(),
        "asked no more"
    );

    rt.toggle_or_seek_voice(PEER, VOICE, "Ivo Marten", 1, None);
    assert!(
        rt.take_download_actions().is_empty(),
        "the next play reads the file instead of streaming again"
    );
    assert!(rt.audio_item_state(VOICE, AudioType::Voice).0);
}

#[test]
fn a_voice_stopped_early_is_asked_for_a_few_times_only() {
    let mut rt = HistoryMediaRuntime::new(false);
    rt.register_descriptor(VOICE, not_cached());
    rt.toggle_or_seek_voice(PEER, VOICE, "Ivo Marten", 3, None);
    let _ = rt.take_download_actions();
    let loader: Arc<dyn RangeLoader> =
        Arc::new(MemoryRangeLoader::new(voice_file(3)).expect("loader"));
    rt.provide_voice_stream(VOICE, Some(loader));
    assert!(rt.audio_item_state(VOICE, AudioType::Voice).0);
    assert!(rt.take_stream_rechecks().0.is_empty());
    rt.audio_player_bar_mut()
        .player_mut()
        .stop(AudioType::Voice, false);

    let mut asked = 0;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (due, more) = rt.take_stream_rechecks();
        asked += due.len();
        if !more {
            break;
        }
        assert!(Instant::now() < deadline, "the asks end");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(asked, 8, "a bounded number of asks");
}

#[test]
fn a_streamed_round_waiting_for_data_shows_the_radial_after_half_a_second() {
    use crate::history::inline_media::WaitingRadial;

    let mut radial = WaitingRadial::default();
    assert_eq!(radial.update(true, 10.0), 0.0);
    assert_eq!(
        radial.update(true, 10.4),
        0.0,
        "hidden for the first 500 ms"
    );
    assert!(
        (radial.update(true, 10.75) - 0.5).abs() < 1e-6,
        "fades in over 500 ms"
    );
    assert_eq!(radial.update(true, 11.2), 1.0);
    assert!((radial.update(false, 11.3) - 1.0).abs() < 1e-6);
    assert!(
        (radial.update(false, 11.4) - 0.5).abs() < 1e-6,
        "fades out in 200 ms"
    );
    assert_eq!(radial.update(false, 11.6), 0.0);
    assert_eq!(radial.update(true, 11.7), 0.0, "the delay applies again");
}

#[test]
fn a_streamed_round_waits_until_its_first_frame_and_sound() {
    let mut rt = HistoryMediaRuntime::new(false);
    rt.register_descriptor(ROUND, not_cached());
    assert!(!rt.round_stream_waiting(ROUND, false), "not streamed yet");
    rt.click_round_video(PEER, ROUND, "Ivo Marten", 3, 1_000);
    let _ = rt.take_download_actions();
    assert!(
        rt.round_stream_waiting(ROUND, false),
        "no loader and no frame yet"
    );
    // The track waits for its loader: the stream has no data.
    rt.advance_audio_ms(20);
    assert!(rt.round_stream_waiting(ROUND, true), "sound waits");
}
