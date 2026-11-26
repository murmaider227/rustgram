//! Screenshot harness for round video channel posts (mock data, generated test video).
//!
//! Writes `round_video_<theme>.png` into `$ROUNDVIDEO_SNAP_DIR` when that variable is set;
//! otherwise it only renders the frames (a smoke test for the painter).

use std::sync::Arc;

use egui::{Pos2, Rect, Vec2};
use egui_kittest::HarnessBuilder;
use tg_text::TextWithEntities;
use tg_ui::fonts::register_tdesktop_fonts;

use crate::history::backend::{
    HistoryMedia, HistoryMessage, HistoryPeerInfo, HistorySlice, MessageExtra, ReactionCounter,
};
use crate::history::inner::{HistoryInnerWidget, InnerEvent};
use crate::history::mock::MockHistoryStore;
use crate::history::sections_model::RoundVideoAttachment;
use crate::history::theme::HistoryColors;
use tg_app_core::core_types::dto::inline_media::{InlineMediaDescriptor, PhotoLoadState};
use tg_app_core::core_types::misc::InlineDocumentData;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind, PeerSummary};

const BASE_UNIX: i32 = 1_795_340_000;

pub(crate) struct RoundSnapState {
    pub(crate) inner: HistoryInnerWidget,
    store: MockHistoryStore,
    peer_info: HistoryPeerInfo,
    list_rect: Rect,
    colors: HistoryColors,
    pub(crate) events: Vec<InnerEvent>,
}

pub(crate) fn channel_peer_info(peer: PeerId) -> HistoryPeerInfo {
    HistoryPeerInfo {
        summary: PeerSummary {
            id: peer,
            kind: PeerKind::Channel,
            title: "Studio Notes".to_owned(),
            status: "12 400 subscribers".to_owned(),
            username: Some("studio_notes".to_owned()),
            phone_or_link: None,
            about: String::new(),
            initials: "SN".to_owned(),
            avatar_rgb: [0xE0, 0x6C, 0x9F],
            muted: false,
            can_send: false,
            is_saved_messages: false,
        },
        typing_text: None,
        contact_status: None,
        unread_count: 0,
        can_pin: false,
        can_delete: false,
        link_base: "https://t.me/studio_notes".to_owned(),
    }
}

pub(crate) fn round_post(peer: PeerId, id: i32, duration_secs: u32, views: u32) -> HistoryMessage {
    HistoryMessage {
        id: MsgId(id),
        peer_id: peer,
        from_id: peer,
        sender_name: "Studio Notes".to_owned(),
        date_unix: BASE_UNIX + id * 60,
        outgoing: false,
        is_post: true,
        text: TextWithEntities::simple(""),
        media: Some(HistoryMedia::RoundVideo(RoundVideoAttachment {
            duration_secs,
            diameter_px: 384,
            accent_rgb: ([0xE8, 0x7E, 0xB0], [0x9C, 0x3D, 0x6E]),
        })),
        service: None,
        reply: None,
        forward: None,
        reactions: vec![
            ReactionCounter {
                emoji: "⭐".to_owned(),
                count: 26,
                chosen: false,
            },
            ReactionCounter {
                emoji: "❤️".to_owned(),
                count: 1616,
                chosen: false,
            },
            ReactionCounter {
                emoji: "👍".to_owned(),
                count: 487,
                chosen: false,
            },
        ],
        views: Some(views),
        edited: false,
        is_read: true,
        is_sending: false,
        is_pinned: false,
        extra: MessageExtra {
            replies_count: Some(441),
            ..MessageExtra::default()
        },
    }
}

/// Generates a 3 s 384x384 H.264 + AAC test clip (`testsrc2` + sine), `None` without ffmpeg.
pub(crate) fn generated_round_mp4(tag: &str) -> Option<Vec<u8>> {
    let dir = std::env::temp_dir().join(format!("rustgram_round_video_{tag}"));
    let path = dir.join("round.mp4");
    tg_app_media::media::mock::generate_test_mp4(&path, 384, 384, 3, true).ok()?;
    let bytes = std::fs::read(&path).ok();
    let _ = std::fs::remove_dir_all(&dir);
    bytes
}

/// A small JPEG thumbnail standing in for a round video's document thumbnail.
pub(crate) fn thumb_jpeg() -> Vec<u8> {
    let img = image::RgbImage::from_fn(90, 90, |x, y| {
        image::Rgb([(120 + x) as u8, (60 + y) as u8, (150 + (x + y) / 4) as u8])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    let _ = image::DynamicImage::ImageRgb8(img).write_to(&mut out, image::ImageFormat::Jpeg);
    out.into_inner()
}

pub(crate) fn build_round_harness(
    peer: PeerId,
    messages: Vec<HistoryMessage>,
    mode: ThemeMode,
    size: Vec2,
) -> egui_kittest::Harness<'static, RoundSnapState> {
    let mut inner = HistoryInnerWidget::new();
    let total_count = messages.len();
    inner.apply_slice(
        peer,
        HistorySlice {
            messages,
            loaded_at_top: true,
            loaded_at_bottom: true,
            unread_count: 0,
            first_unread_id: None,
            total_count,
        },
        PeerKind::Channel,
        true,
    );
    let colors = HistoryColors::from_palette(&Palette::for_mode(mode));
    let state = RoundSnapState {
        inner,
        store: MockHistoryStore::new(),
        peer_info: channel_peer_info(peer),
        list_rect: Rect::from_min_size(Pos2::ZERO, size),
        colors,
        events: Vec::new(),
    };
    HarnessBuilder::default()
        .with_size(size)
        .with_step_dt(0.05)
        .with_max_steps(8)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
        })
        .build_ui_state(
            |ui, st: &mut RoundSnapState| {
                ui.painter()
                    .rect_filled(st.list_rect, 0.0, st.colors.canvas_bg);
                let evs = st.inner.show(
                    ui,
                    st.list_rect,
                    &st.peer_info,
                    false,
                    "",
                    &st.colors,
                    &mut st.store,
                );
                st.events.extend(evs);
            },
            state,
        )
}

#[test]
fn round_video_channel_posts_snapshot() {
    let Some(mp4) = generated_round_mp4("snap") else {
        return;
    };
    let mp4 = Arc::new(mp4);
    for (tag, mode) in [("day", ThemeMode::Day), ("night", ThemeMode::Night)] {
        let peer = PeerId::from_channel(77_001);
        let messages = vec![
            round_post(peer, 11, 3, 201_200),
            round_post(peer, 12, 3, 74_600),
            round_post(peer, 13, 34, 75_600),
        ];
        let mut harness = build_round_harness(peer, messages, mode, Vec2::new(560.0, 1060.0));
        {
            let rt = harness.state_mut().inner.media_runtime_mut();
            for id in [11, 12] {
                rt.register_descriptor(
                    MsgId(id),
                    InlineMediaDescriptor {
                        video_data: Some(InlineDocumentData::Bytes(mp4.as_ref().clone())),
                        audio_bytes: Some(mp4.as_ref().clone()),
                        audio_ext: Some("mp4".to_owned()),
                        ..InlineMediaDescriptor::default()
                    },
                );
            }
            rt.register_descriptor(
                MsgId(13),
                InlineMediaDescriptor {
                    photo_load: Some(PhotoLoadState::Ready {
                        bytes: thumb_jpeg().into(),
                    }),
                    ..InlineMediaDescriptor::default()
                },
            );
        }
        for _ in 0..6 {
            harness.step();
        }
        harness
            .state_mut()
            .inner
            .media_runtime_mut()
            .click_round_video_sound(peer, MsgId(12), "Studio Notes", 3);
        for _ in 0..6 {
            harness.step();
            harness
                .state_mut()
                .inner
                .media_runtime_mut()
                .advance_audio_ms(150);
        }
        if let Some(dir) = std::env::var_os("ROUNDVIDEO_SNAP_DIR") {
            let img = harness.render().expect("render round video snapshot");
            let path = std::path::PathBuf::from(dir).join(format!("round_video_{tag}.png"));
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create snap dir");
            }
            img.save(&path).expect("save round video snapshot");
        }
    }
}
