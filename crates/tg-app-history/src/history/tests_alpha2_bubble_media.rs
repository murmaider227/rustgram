//! Alpha.2 owner-test fixes for bubble media (TGB-2954): the voice message waveform and status
//! line, the reply header of a reply to a photo, and no web-page block for a bare internal
//! link. Mock data only.
//!
//! `bubble_media_snapshot` writes `bubble_media.png` into `$BUBBLE_MEDIA_SNAP_DIR` when that
//! variable is set. With `$BUBBLE_MEDIA_SNAP_BEFORE` set, the messages carry what the old live
//! mapping produced (a "[Photo]" reply without a thumbnail, an empty link preview).

use egui::{Pos2, Rect, Vec2};
use egui_kittest::HarnessBuilder;
use tg_text::TextWithEntities;
use tg_ui::fonts::register_tdesktop_fonts;

use crate::history::backend::{
    HistoryMedia, HistoryMessage, HistoryPeerInfo, HistorySlice, MessageExtra, ReplyHeader,
    ReplyThumb, VoiceAttachment,
};
use crate::history::inner::{HistoryInnerWidget, InnerEvent};
use crate::history::media::MediaAssetCache;
use crate::history::mock::{MockHistoryStore, synthetic_stripped_thumb};
use crate::history::reply_preview::{ReplyThumbPaint, paint_reply_thumb};
use crate::history::sections_model::LinkPreviewData;
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind, PeerSummary};
use tg_media::audio::waveform_encode_5bit;

const BASE_UNIX: i32 = 1_795_340_000;
const PHOTO_MSG: i32 = 5;
/// The message that replies to the photo: the backend is asked for its thumbnail.
const HOLDER_MSG: i32 = 11;

fn peer() -> PeerId {
    PeerId::from_channel(77_954)
}

fn group_info(peer: PeerId) -> HistoryPeerInfo {
    HistoryPeerInfo {
        summary: PeerSummary {
            id: peer,
            kind: PeerKind::Group,
            title: "Studio Crew".to_owned(),
            status: "4 members".to_owned(),
            username: None,
            phone_or_link: None,
            about: String::new(),
            initials: "SC".to_owned(),
            avatar_rgb: [0x5E, 0x72, 0xE4],
            muted: false,
            can_send: true,
            is_saved_messages: false,
        },
        typing_text: None,
        contact_status: None,
        unread_count: 0,
        can_pin: false,
        can_delete: false,
        link_base: String::new(),
    }
}

fn base_message(id: i32, from: u64, name: &str, text: &str) -> HistoryMessage {
    HistoryMessage {
        id: MsgId(id),
        peer_id: peer(),
        from_id: PeerId::from_user(from),
        sender_name: name.to_owned(),
        date_unix: BASE_UNIX + id * 60,
        outgoing: false,
        is_post: false,
        text: TextWithEntities::simple(text),
        media: None,
        service: None,
        reply: None,
        forward: None,
        reactions: Vec::new(),
        views: None,
        edited: false,
        is_read: true,
        is_sending: false,
        is_pinned: false,
        extra: MessageExtra::default(),
    }
}

/// A quiet recording (loudest sample 12 of 31), as real voice notes mostly are.
fn quiet_waveform() -> Vec<u8> {
    let samples: Vec<u8> = (0..100u32)
        .map(|i| {
            let wave = ((i as f32 * 0.31).sin() * 0.5 + 0.5) * ((i as f32 * 0.07).cos().abs());
            (wave * 12.0).round() as u8
        })
        .collect();
    waveform_encode_5bit(&samples)
}

fn voice(duration_secs: u32, size_bytes: u64, is_unread: bool) -> HistoryMedia {
    HistoryMedia::Voice(VoiceAttachment {
        duration_secs,
        waveform_5bit: quiet_waveform(),
        size_bytes,
        is_unread,
        transcription: None,
    })
}

fn photo_reply(before: bool) -> ReplyHeader {
    ReplyHeader {
        reply_to_msg_id: MsgId(PHOTO_MSG),
        author_name: "Noor Ilves".to_owned(),
        author_color_id: 1003,
        author_accent: None,
        preview_text: if before { "[Photo]" } else { "Photo" }.to_owned(),
        preview_thumb_rgb: (!before).then_some([0x6E, 0xC9, 0xCB]),
        preview_thumb: (!before).then(|| ReplyThumb {
            stripped: Some(synthetic_stripped_thumb(0x31)),
            round: false,
            spoiler: false,
            spoiler_animation: false,
            key: 1,
        }),
        preview_colored_len: if before { 0 } else { "Photo".len() },
        quote_text: None,
    }
}

fn messages(before: bool) -> Vec<HistoryMessage> {
    let mut text_reply = base_message(10, 1002, "Ivo Marten", "");
    text_reply.media = Some(voice(4, 16_300, true));
    text_reply.reply = Some(ReplyHeader {
        reply_to_msg_id: MsgId(3),
        author_name: "Mira Vance".to_owned(),
        author_color_id: 1001,
        author_accent: None,
        preview_text: "So far the new build feels quick, but the first start is slow".to_owned(),
        preview_thumb_rgb: None,
        preview_thumb: None,
        preview_colored_len: 0,
        quote_text: None,
    });

    let mut to_photo = base_message(HOLDER_MSG, 1002, "Ivo Marten", "");
    to_photo.media = Some(voice(7, 28_400, true));
    to_photo.reply = Some(photo_reply(before));

    let mut link = base_message(12, 1004, "Tavi Roan", "https://t.me/c/1234567890/261238");
    if before {
        // `messageMediaWebPage` with `webPageEmpty`, as the old mapping passed it on.
        link.extra.link_preview = Some(LinkPreviewData {
            url: String::new(),
            site_name: String::new(),
            title: String::new(),
            description: String::new(),
            thumb_rgb: Some([0x5E, 0x72, 0xE4]),
            large_media: false,
            above_text: false,
        });
    }
    vec![text_reply, to_photo, link]
}

/// A 320x240 JPEG standing in for the replied-to photo's "m" thumbnail.
fn thumb_jpeg() -> Vec<u8> {
    let img = image::RgbImage::from_fn(320, 240, |x, y| {
        let sky = y < 130;
        let sun = (i64::from(x) - 230).pow(2) + (i64::from(y) - 70).pow(2) < 900;
        if sun {
            image::Rgb([250, 214, 90])
        } else if sky {
            image::Rgb([90, (150 + y / 3) as u8, 230])
        } else {
            image::Rgb([(40 + x / 8) as u8, (120 + (y - 130) / 2) as u8, 70])
        }
    });
    let mut out = std::io::Cursor::new(Vec::new());
    let _ = image::DynamicImage::ImageRgb8(img).write_to(&mut out, image::ImageFormat::Jpeg);
    out.into_inner()
}

struct SnapState {
    inner: HistoryInnerWidget,
    store: MockHistoryStore,
    peer_info: HistoryPeerInfo,
    list_rect: Rect,
    colors: HistoryColors,
    events: Vec<InnerEvent>,
}

#[test]
fn bubble_media_snapshot() {
    let before = std::env::var_os("BUBBLE_MEDIA_SNAP_BEFORE").is_some();
    let size = Vec2::new(520.0, 330.0);
    let messages = messages(before);
    let mut inner = HistoryInnerWidget::new();
    let total_count = messages.len();
    inner.apply_slice(
        peer(),
        HistorySlice {
            messages,
            loaded_at_top: true,
            loaded_at_bottom: true,
            unread_count: 0,
            first_unread_id: None,
            total_count,
        },
        PeerKind::Group,
        true,
    );
    let mut store = MockHistoryStore::new();
    store.set_reply_preview_thumb(peer(), MsgId(HOLDER_MSG), thumb_jpeg());
    let state = SnapState {
        inner,
        store,
        peer_info: group_info(peer()),
        list_rect: Rect::from_min_size(Pos2::ZERO, size),
        colors: HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Day)),
        events: Vec::new(),
    };
    let mut harness = HarnessBuilder::default()
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
            |ui, st: &mut SnapState| {
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
        );
    harness.run_steps(4);
    if let Some(dir) = std::env::var_os("BUBBLE_MEDIA_SNAP_DIR") {
        let img = harness.render().expect("render bubble media snapshot");
        let path = std::path::PathBuf::from(dir).join("bubble_media.png");
        img.save(&path).expect("save bubble media snapshot");
    }
}

/// The header first shows the blurred inline thumbnail, asks the backend once, then shows the
/// real thumbnail; frames after that neither ask again nor decode again.
#[test]
fn reply_thumbnail_goes_from_blurred_to_real_with_one_backend_call() {
    struct State {
        assets: MediaAssetCache,
        store: MockHistoryStore,
        painted: Vec<Option<ReplyThumbPaint>>,
    }
    let mut store = MockHistoryStore::new();
    store.set_reply_preview_thumb(peer(), MsgId(HOLDER_MSG), thumb_jpeg());
    let state = State {
        assets: MediaAssetCache::new(),
        store,
        painted: Vec::new(),
    };
    let mut harness = HarnessBuilder::default()
        .with_size(Vec2::new(320.0, 80.0))
        .with_max_steps(8)
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, st: &mut State| {
                let now_ms = (ui.ctx().input(|i| i.time) * 1000.0) as i64;
                st.assets.reply_previews.poll(ui.ctx(), &st.store, now_ms);
                let reply_rect = Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::new(300.0, 40.0));
                st.painted.push(paint_reply_thumb(
                    ui,
                    &mut st.assets,
                    peer(),
                    MsgId(HOLDER_MSG),
                    &photo_reply(false),
                    reply_rect,
                ));
            },
            state,
        );
    harness.run_steps(6);
    let st = harness.state();
    assert_eq!(st.painted.first(), Some(&Some(ReplyThumbPaint::Blurred)));
    assert_eq!(st.painted.last(), Some(&Some(ReplyThumbPaint::Real)));
    assert_eq!(
        st.assets.reply_previews.backend_asks(),
        1,
        "asked once, not on every frame"
    );
}

/// A reply without a media preview paints no thumbnail, and one whose target has no image at
/// all (no inline bytes, nothing to download) falls back to the tint.
#[test]
fn reply_thumbnail_fallbacks() {
    struct State {
        assets: MediaAssetCache,
        store: MockHistoryStore,
        painted: Vec<(Option<ReplyThumbPaint>, Option<ReplyThumbPaint>)>,
    }
    let state = State {
        assets: MediaAssetCache::new(),
        store: MockHistoryStore::new(),
        painted: Vec::new(),
    };
    let mut harness = HarnessBuilder::default()
        .with_size(Vec2::new(320.0, 80.0))
        .with_max_steps(8)
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, st: &mut State| {
                let now_ms = (ui.ctx().input(|i| i.time) * 1000.0) as i64;
                st.assets.reply_previews.poll(ui.ctx(), &st.store, now_ms);
                let reply_rect = Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::new(300.0, 40.0));
                let none = paint_reply_thumb(
                    ui,
                    &mut st.assets,
                    peer(),
                    MsgId(HOLDER_MSG),
                    &photo_reply(true),
                    reply_rect,
                );
                let mut bare = photo_reply(false);
                bare.preview_thumb = Some(ReplyThumb::default());
                let tint = paint_reply_thumb(
                    ui,
                    &mut st.assets,
                    peer(),
                    MsgId(HOLDER_MSG),
                    &bare,
                    reply_rect,
                );
                st.painted.push((none, tint));
            },
            state,
        );
    harness.run_steps(4);
    let st = harness.state();
    assert!(
        st.painted
            .iter()
            .all(|p| *p == (None, Some(ReplyThumbPaint::Tint)))
    );
    assert_eq!(st.assets.reply_previews.backend_asks(), 1);
}
