//! TGB-2997: a round video message that does not autoplay is not downloaded when it is listed.
//! It shows the play button over its thumbnail (`Gif::draw`: the video can be played), a click
//! downloads it and it then starts with sound. The old code ignored clicks on a round video
//! that was not downloaded, because every round video was downloaded when listed.

use tg_app_core::core_types::misc::InlineDocumentData;

use super::*;
use crate::history::inline_media::{
    InlineMediaDescriptor, RoundVideoPhase, VoiceLoadState, synthesize_tone_wav,
};

const ROUND: MsgId = MsgId(61);
const PEER: PeerId = PeerId(77);

#[test]
fn a_click_downloads_a_round_video_that_does_not_autoplay_and_plays_it() {
    let mut rt = HistoryMediaRuntime::new(false);
    rt.register_descriptor(
        ROUND,
        InlineMediaDescriptor {
            voice_load: Some(VoiceLoadState::NotLoaded),
            ..InlineMediaDescriptor::default()
        },
    );
    assert_eq!(
        rt.round_video_phase(ROUND),
        RoundVideoPhase::Static,
        "the play button, not a loader: nothing downloads"
    );
    assert!(rt.take_download_actions().is_empty());
    // A backend that streams plays it at once instead (TGB-3116, `tests_round_stream`).
    rt.provide_voice_stream(MsgId(1), None);

    rt.click_round_video(PEER, ROUND, "Ivo Marten", 3, 1_000);
    assert_eq!(
        rt.take_download_actions(),
        vec![(ROUND, MediaDownloadAction::Start)],
        "the click downloads the video"
    );
    assert_eq!(rt.round_video_phase(ROUND), RoundVideoPhase::NotLoaded);
    assert_eq!(
        rt.pending_voice_msg(),
        Some(ROUND),
        "the widget polls for it"
    );
    assert!(!rt.is_round_video_unmuted(ROUND));

    // The file arrives: the round starts with sound, as the click asked.
    rt.register_descriptor(
        ROUND,
        InlineMediaDescriptor {
            video_data: Some(InlineDocumentData::Bytes(b"round-video".to_vec())),
            audio_bytes: Some(synthesize_tone_wav(300.0, 3.0)),
            audio_ext: Some("wav".to_owned()),
            ..InlineMediaDescriptor::default()
        },
    );
    assert!(rt.is_round_video_unmuted(ROUND));
    assert_eq!(rt.round_video_phase(ROUND), RoundVideoPhase::SoundPlaying);
    assert_eq!(rt.pending_voice_msg(), None);
    assert!(rt.take_download_actions().is_empty());
}

#[test]
fn a_click_on_a_round_video_that_already_downloads_only_waits_for_it() {
    let mut rt = HistoryMediaRuntime::new(false);
    rt.register_descriptor(
        ROUND,
        InlineMediaDescriptor {
            voice_load: Some(VoiceLoadState::Loading),
            ..InlineMediaDescriptor::default()
        },
    );
    assert_eq!(rt.round_video_phase(ROUND), RoundVideoPhase::NotLoaded);
    rt.click_round_video(PEER, ROUND, "Ivo Marten", 3, 1_000);
    assert!(rt.take_download_actions().is_empty());
    assert_eq!(rt.pending_voice_msg(), Some(ROUND));
}

/// Renders a round video that is not downloaded: with no download state (the old look: the
/// loader) and waiting for a click (the play button). Saved for review with
/// `ROUNDVIDEO_SNAP_DIR` set.
#[test]
fn a_round_video_waiting_for_a_click_shows_the_play_button() {
    use crate::history::tests_round_video_snap::{build_round_harness, round_post, thumb_jpeg};
    use tg_app_core::core_types::dto::inline_media::PhotoLoadState;
    use tg_app_core::stand_in::theme::ThemeMode;

    for (tag, load) in [
        ("loading", None),
        ("waiting_for_click", Some(VoiceLoadState::NotLoaded)),
    ] {
        let peer = PeerId::from_channel(77_061);
        let mut harness = build_round_harness(
            peer,
            vec![round_post(peer, 14, 3, 1_200)],
            ThemeMode::Day,
            egui::Vec2::new(420.0, 360.0),
        );
        harness
            .state_mut()
            .inner
            .media_runtime_mut()
            .register_descriptor(
                MsgId(14),
                InlineMediaDescriptor {
                    photo_load: Some(PhotoLoadState::Ready {
                        bytes: thumb_jpeg().into(),
                    }),
                    voice_load: load,
                    ..InlineMediaDescriptor::default()
                },
            );
        for _ in 0..4 {
            harness.step();
        }
        let expected = if load.is_some() {
            RoundVideoPhase::Static
        } else {
            RoundVideoPhase::NotLoaded
        };
        assert_eq!(
            harness
                .state()
                .inner
                .media_runtime()
                .round_video_phase(MsgId(14)),
            expected
        );
        if let Some(dir) = std::env::var_os("ROUNDVIDEO_SNAP_DIR") {
            let img = harness.render().expect("render");
            let path = std::path::PathBuf::from(dir).join(format!("round_on_demand_{tag}.png"));
            img.save(&path).expect("save");
        }
    }
}
