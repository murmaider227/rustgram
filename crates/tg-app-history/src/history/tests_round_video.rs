//! Round video message parity tests (`HistoryView::Gif` round path in Telegram Desktop 7.4.6):
//! bubble-less layout with the pills and right actions outside the circle, and the playback
//! state machine (muted loop -> click: sound + ring -> pause -> muted loop; off-screen pause).

use egui::{Context, Vec2};
use egui_kittest::kittest::Queryable;
use tg_audio::AudioType;
use tg_history::InfoDisplayType;

use crate::history::inline_media::{
    HistoryMediaRuntime, RoundVideoPhase, media_item_id_for_msg, round_remaining_secs,
};
use crate::history::inner::InnerEvent;
use crate::history::tests_round_video_snap::{
    build_round_harness, generated_round_mp4, round_post, thumb_jpeg,
};
use tg_app_core::core_types::dto::inline_media::{InlineMediaDescriptor, PhotoLoadState};
use tg_app_core::core_types::misc::InlineDocumentData;
use tg_app_core::stand_in::theme::ThemeMode;
use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_app_media::media::inline::InlinePlaybackKind;

/// `st::maxVideoMessageSize`.
const ROUND_SIDE: i32 = 240;

#[test]
fn round_video_post_is_bubbleless_with_pills_and_comments_outside_the_circle() {
    let peer = PeerId::from_channel(77_101);
    let mut harness = build_round_harness(
        peer,
        vec![round_post(peer, 11, 34, 201_200)],
        ThemeMode::Day,
        Vec2::new(560.0, 520.0),
    );
    for _ in 0..4 {
        harness.step();
    }
    let layout = harness
        .state()
        .inner
        .cached_message_layout(MsgId(11))
        .expect("round post layout")
        .clone();

    // `Message::drawBubble` is false and the channel name is not drawn.
    assert!(
        !layout.has_bubble,
        "round video posts must not get a bubble"
    );
    assert!(layout.unwrapped);
    assert_eq!(layout.header_heights.from_name_or_via, 0);
    assert!(layout.comments_rect.is_empty(), "no in-bubble comments bar");

    // A bare 240 px circle (`st::maxVideoMessageSize`) at the media rect's top-left.
    let media = layout.media_rect;
    assert_eq!(media.height, ROUND_SIDE);
    assert_eq!(media.width.min(media.height), ROUND_SIDE);
    let circle_right = media.x + ROUND_SIDE;

    // Time / views pill on the service plate, hanging off the circle's bottom-right.
    let info = layout.bottom_info_rect;
    assert_eq!(layout.bottom_info_display, InfoDisplayType::Background);
    assert!(
        info.x + info.width > circle_right,
        "info pill must extend past the circle: {info:?} vs circle right {circle_right}"
    );
    assert!(info.x < circle_right, "info pill overlaps the circle edge");
    assert_eq!(info.y + info.height, media.y + media.height - 4);

    // Reactions under the circle, outside any bubble.
    assert!(layout.reactions_rect.y >= media.y + media.height);

    // Comments become the side right action with the short count.
    let comments = harness.get_by_label("441 comments");
    let origin_x = harness
        .state()
        .inner
        .last_painted_bubble_rect(MsgId(11))
        .expect("painted")
        .left()
        - media.x as f32;
    assert!(
        comments.rect().left() > origin_x + (info.x + info.width) as f32,
        "comments button sits right of the info pill: {:?}",
        comments.rect()
    );
    comments.click();
    harness.step();
    assert!(
        harness
            .state()
            .events
            .iter()
            .any(|e| matches!(e, InnerEvent::OpenComments(id) if *id == MsgId(11))),
        "side comments button opens the comments"
    );
}

#[test]
fn round_video_layout_ignores_large_document_dimensions() {
    // Round documents are often 384 or 400 px: the circle stays at 240 px.
    let peer = PeerId::from_channel(77_102);
    let mut post = round_post(peer, 21, 5, 10);
    post.reactions.clear();
    post.extra.replies_count = None;
    let mut harness =
        build_round_harness(peer, vec![post], ThemeMode::Night, Vec2::new(900.0, 400.0));
    for _ in 0..3 {
        harness.step();
    }
    let layout = harness
        .state()
        .inner
        .cached_message_layout(MsgId(21))
        .expect("layout")
        .clone();
    assert!(!layout.has_bubble);
    assert_eq!(layout.media_rect.height, ROUND_SIDE);
    assert_eq!(layout.media_rect.width, ROUND_SIDE);
}

fn loaded_descriptor(mp4: &[u8]) -> InlineMediaDescriptor {
    InlineMediaDescriptor {
        video_data: Some(InlineDocumentData::Bytes(mp4.to_vec())),
        audio_bytes: Some(mp4.to_vec()),
        audio_ext: Some("mp4".to_owned()),
        ..InlineMediaDescriptor::default()
    }
}

#[test]
fn round_video_state_machine_silent_loop_click_sound_pause_and_offscreen() {
    let Some(mp4) = generated_round_mp4("state") else {
        return;
    };
    let ctx = Context::default();
    let peer = PeerId::from_channel(77_103);
    let msg = MsgId(31);
    let mut rt = HistoryMediaRuntime::new(false);

    // 0. Still downloading: thumbnail + loader, clicks are ignored.
    rt.register_descriptor(
        msg,
        InlineMediaDescriptor {
            photo_load: Some(PhotoLoadState::Ready {
                bytes: thumb_jpeg().into(),
            }),
            ..InlineMediaDescriptor::default()
        },
    );
    assert_eq!(rt.round_video_phase(msg), RoundVideoPhase::NotLoaded);
    rt.click_round_video(peer, msg, "Studio Notes", 3, 0);
    assert!(!rt.is_round_video_unmuted(msg));

    // 1. Downloaded: muted looping autoplay, no progress ring.
    rt.register_descriptor(msg, loaded_descriptor(&mp4));
    assert_eq!(rt.round_video_phase(msg), RoundVideoPhase::SilentLoop);
    rt.begin_frame(100);
    let frame = rt
        .inline_clip_frame(
            &ctx,
            msg,
            InlinePlaybackKind::RoundVideo,
            Some((240, 240)),
            100,
        )
        .expect("clip frame");
    assert!(frame.texture.is_some(), "first frame decoded");
    assert!(frame.is_playing, "muted loop autoplays");
    rt.finish_frame(100);

    // 2. Click: restart with sound; the ring follows the audio.
    rt.begin_frame(1_000);
    rt.click_round_video(peer, msg, "Studio Notes", 3, 1_000);
    assert!(rt.is_round_video_unmuted(msg));
    assert_eq!(rt.round_video_phase(msg), RoundVideoPhase::SoundPlaying);
    rt.advance_audio_ms(1_200);
    rt.sync_round_video(msg, 3, 2_200);
    let (active, playing, progress, _, _) = rt.audio_item_state(msg, AudioType::Voice);
    assert!(active && playing, "sound playback runs");
    assert!(
        progress > 0.2,
        "audio progress drives the ring, got {progress}"
    );
    assert_eq!(round_remaining_secs(3, progress), 2);
    rt.finish_frame(2_200);

    // 3. Second click pauses (ring stays with the seek handle), third resumes.
    rt.begin_frame(2_300);
    rt.click_round_video(peer, msg, "Studio Notes", 3, 2_300);
    assert_eq!(rt.round_video_phase(msg), RoundVideoPhase::SoundPaused);
    assert!(
        rt.inline.is_item_paused(media_item_id_for_msg(msg)),
        "the clip freezes with the audio"
    );
    rt.click_round_video(peer, msg, "Studio Notes", 3, 2_400);
    assert_eq!(rt.round_video_phase(msg), RoundVideoPhase::SoundPlaying);
    rt.finish_frame(2_400);

    // 4. Scrolled away: the next frame does not mark it visible, so the clip pauses.
    rt.begin_frame(2_500);
    rt.finish_frame(2_500);
    assert!(rt.is_paused_offscreen(msg), "off-screen round video pauses");

    // 5. Audio ends: back to the muted loop.
    rt.begin_frame(2_600);
    rt.mark_visible(msg);
    rt.advance_audio_ms(5_000);
    rt.sync_round_video(msg, 3, 7_600);
    assert!(!rt.is_round_video_unmuted(msg));
    assert_eq!(rt.round_video_phase(msg), RoundVideoPhase::SilentLoop);
}

#[test]
fn downloaded_audio_replaces_the_synthesized_stand_in_track() {
    let peer = PeerId::from_channel(77_104);
    let msg = MsgId(41);
    let mut rt = HistoryMediaRuntime::new(false);
    // Registered while downloading: the playlist sync falls back to a synthesized track.
    rt.register_descriptor(msg, InlineMediaDescriptor::default());
    let first = rt.ensure_voice_registered(peer, msg, "Studio Notes", 3, true);
    assert!(rt.audio_registered(msg));
    // The real bytes arrive: the stand-in registration is dropped and re-made on next use.
    rt.register_descriptor(
        msg,
        InlineMediaDescriptor {
            audio_bytes: Some(vec![1, 2, 3]),
            audio_ext: Some("mp4".to_owned()),
            ..InlineMediaDescriptor::default()
        },
    );
    assert!(!rt.audio_registered(msg), "stand-in track dropped");
    let second = rt.ensure_voice_registered(peer, msg, "Studio Notes", 3, true);
    assert_eq!(first, second, "same track id");
    assert!(rt.audio_registered(msg));
}

#[test]
fn remaining_time_counts_down_like_update_status_text() {
    assert_eq!(round_remaining_secs(34, 0.0), 34);
    assert_eq!(round_remaining_secs(34, 0.5), 18);
    assert_eq!(round_remaining_secs(34, 0.999), 1);
    assert_eq!(round_remaining_secs(34, 1.0), 1);
}

#[test]
fn round_video_plays_again_with_sound_after_it_finished() {
    let Some(mp4) = generated_round_mp4("replay") else {
        return;
    };
    let peer = PeerId::from_channel(77_105);
    let msg = MsgId(51);
    let mut rt = HistoryMediaRuntime::new(false);
    rt.register_descriptor(msg, loaded_descriptor(&mp4));
    rt.click_round_video(peer, msg, "Studio Notes", 3, 0);
    rt.advance_audio_ms(300);
    rt.sync_round_video(msg, 3, 300);
    rt.advance_audio_ms(5_000);
    rt.sync_round_video(msg, 3, 5_300);
    assert_eq!(rt.round_video_phase(msg), RoundVideoPhase::SilentLoop);

    // Clicking again restarts the sound from the beginning.
    rt.click_round_video(peer, msg, "Studio Notes", 3, 6_000);
    rt.advance_audio_ms(500);
    rt.sync_round_video(msg, 3, 6_500);
    let (active, playing, progress, _, _) = rt.audio_item_state(msg, AudioType::Voice);
    assert!(active && playing, "replay must play, progress {progress}");
    assert!(
        progress < 0.5,
        "replay starts from the beginning, got {progress}"
    );
    assert_eq!(rt.round_video_phase(msg), RoundVideoPhase::SoundPlaying);
}
