//! Wave 4 MEDIA package, widget-level tests.
//!
//! - TGB-2982 / TGB-2958: the download clicks queued while painting reach the backend
//!   (`start_media_download` / `cancel_media_download`) and the new state is read back.
//! - TGB-2981: the demo stripped thumbnails decode to a picture, not to a black box.
//! - TGB-2959: reply header thumbnails are asked for by the message that holds the reply,
//!   media under a spoiler is blurred and gets the particles.

use egui::{Pos2, Rect, Vec2};
use egui_kittest::HarnessBuilder;
use tg_media::image::inline::from_inline_bytes;

use crate::history::backend::{HistoryMedia, ReplyHeader, ReplyThumb, VoiceAttachment};
use crate::history::inline_media::{
    InlineMediaDescriptor, MediaDownloadAction, PhotoLink, PhotoLoadState, VoiceLoadState,
    synthesize_tone_wav,
};
use crate::history::media::MediaAssetCache;
use crate::history::mock::{MockHistoryStore, synthetic_stripped_thumb, synthetic_thumb_picture};
use crate::history::reply_preview::{
    ReplyThumbPaint, decode_reply_thumb, paint_reply_thumb, reply_thumb_has_spoiler_particles,
};
use crate::history::tests_photo_snap::{
    build_photo_harness, build_photo_harness_with, photo_message,
};
use tg_app_core::stand_in::types::{MsgId, PeerId};

fn loading() -> InlineMediaDescriptor {
    InlineMediaDescriptor {
        photo_load: Some(PhotoLoadState::Loading {
            progress: Some(0.3),
            bytes_loaded: 74_000,
            total_bytes: 248_910,
        }),
        ..InlineMediaDescriptor::default()
    }
}

#[test]
fn photo_download_clicks_reach_the_backend_and_the_state_comes_back() {
    let peer = PeerId::from_user(7101);
    let msg = photo_message(peer, 2, (1280, 820), &synthetic_thumb_picture(0x31), "");
    let mut harness = build_photo_harness(
        peer,
        vec![msg],
        vec![(MsgId(2), loading())],
        Vec2::new(640.0, 400.0),
        false,
    );
    harness.step();
    assert_eq!(
        harness.state().inner.media_runtime().photo_link(MsgId(2)),
        PhotoLink::Cancel
    );

    harness
        .state_mut()
        .inner
        .media_runtime_mut()
        .queue_download(MsgId(2), MediaDownloadAction::Cancel);
    harness.step();
    assert_eq!(
        harness.state().store.download_calls(),
        vec![(MsgId(2), false)]
    );
    assert_eq!(
        harness.state().inner.media_runtime().photo_link(MsgId(2)),
        PhotoLink::Save,
        "after the cancel the photo is not loaded and not loading"
    );
    // It stays cancelled: the polls of loading media do not start it again.
    for _ in 0..8 {
        harness.step();
    }
    assert_eq!(
        harness.state().inner.media_runtime().photo_link(MsgId(2)),
        PhotoLink::Save
    );

    harness
        .state_mut()
        .inner
        .media_runtime_mut()
        .queue_download(MsgId(2), MediaDownloadAction::Start);
    harness.step();
    assert_eq!(
        harness.state().store.download_calls(),
        vec![(MsgId(2), false), (MsgId(2), true)]
    );
    assert_eq!(
        harness.state().inner.media_runtime().photo_link(MsgId(2)),
        PhotoLink::Cancel,
        "the second click downloads again"
    );
}

#[test]
fn the_first_play_of_a_voice_downloads_it_through_the_backend() {
    let peer = PeerId::from_user(7102);
    let mut msg = photo_message(peer, 3, (1, 1), &synthetic_thumb_picture(0x31), "");
    msg.media = Some(HistoryMedia::Voice(VoiceAttachment {
        duration_secs: 4,
        waveform_5bit: vec![0x55; 63],
        size_bytes: 16_300,
        is_unread: false,
        transcription: None,
    }));
    let not_loaded = InlineMediaDescriptor {
        voice_load: Some(VoiceLoadState::NotLoaded),
        ..InlineMediaDescriptor::default()
    };
    let mut harness = build_photo_harness(
        peer,
        vec![msg],
        vec![(MsgId(3), not_loaded)],
        Vec2::new(640.0, 300.0),
        false,
    );
    harness
        .state_mut()
        .store
        .set_voice_file(peer, MsgId(3), synthesize_tone_wav(300.0, 2.0));
    harness.step();
    assert!(
        harness.state().store.download_calls().is_empty(),
        "a voice message is not downloaded when it enters the list"
    );
    assert_eq!(
        harness.state().inner.media_runtime().voice_loaded(MsgId(3)),
        Some(false)
    );

    harness
        .state_mut()
        .inner
        .media_runtime_mut()
        .toggle_or_seek_voice(peer, MsgId(3), "Sample Sender", 4, None);
    harness.step();
    harness.step();
    let st = harness.state();
    assert_eq!(st.store.download_calls(), vec![(MsgId(3), true)]);
    assert_eq!(st.inner.media_runtime().voice_loaded(MsgId(3)), Some(true));
    assert!(
        st.inner
            .media_runtime()
            .audio_item_state(MsgId(3), tg_audio::AudioType::Voice)
            .0,
        "the voice message plays once its file arrived"
    );
}

/// Mean luma and the spread between the brightest and darkest channel mean of a thumbnail.
fn luma_and_spread(seed: u8) -> (f32, f32) {
    let img = from_inline_bytes(&synthetic_stripped_thumb(seed)).expect("stripped thumb decodes");
    let n = (img.width() * img.height()) as f32;
    let mut sum = [0f32; 3];
    for px in img.pixels() {
        for (k, total) in sum.iter_mut().enumerate() {
            *total += f32::from(px.0[k]);
        }
    }
    let mean = sum.map(|v| v / n);
    let luma = 0.299 * mean[0] + 0.587 * mean[1] + 0.114 * mean[2];
    let spread =
        mean.iter().copied().fold(0f32, f32::max) - mean.iter().copied().fold(255f32, f32::min);
    (luma, spread)
}

#[test]
fn demo_stripped_thumbs_are_pictures_not_black_boxes() {
    // Every seed the demo chats and the benchmark data use.
    let seeds = [
        0x21u8, 0x31, 0x3C, 0x42, 0x45, 0x46, 0x52, 0x63, 0x64, 0x74, 0x78, 0x8A, 0, 199,
    ];
    for seed in seeds {
        let (luma, spread) = luma_and_spread(seed);
        assert!(
            luma > 70.0,
            "seed {seed:#x}: the demo thumbnail is dark (mean luma {luma:.1})"
        );
        assert!(
            spread > 8.0,
            "seed {seed:#x}: the demo thumbnail has no colour (channel spread {spread:.1})"
        );
    }
    // Different seeds give different pictures (album tiles must not look the same).
    assert_ne!(
        synthetic_stripped_thumb(0x52),
        synthetic_stripped_thumb(0x64)
    );
    // And the same seed is stable.
    assert_eq!(
        synthetic_stripped_thumb(0x52),
        synthetic_stripped_thumb(0x52)
    );
}

// --- reply header thumbnails (TGB-2959) ----------------------------------------------------

const HOLDER: MsgId = MsgId(31);

fn peer() -> PeerId {
    PeerId::from_chat(7200)
}

fn jpeg(rgb: [u8; 3]) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(64, 48, image::Rgb(rgb));
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(
            &mut std::io::Cursor::new(&mut out),
            image::ImageFormat::Jpeg,
        )
        .expect("encode jpeg");
    out
}

fn reply(thumb: ReplyThumb) -> ReplyHeader {
    ReplyHeader {
        // A reply to a story has no target message id at all.
        reply_to_msg_id: MsgId(0),
        author_name: "Noor Ilves".to_owned(),
        author_color_id: 1003,
        preview_text: "Story".to_owned(),
        preview_thumb_rgb: Some([0x6E, 0xC9, 0xCB]),
        preview_thumb: Some(thumb),
        preview_colored_len: 0,
        quote_text: None,
    }
}

struct ThumbState {
    assets: MediaAssetCache,
    store: MockHistoryStore,
    reply: ReplyHeader,
    painted: Vec<Option<ReplyThumbPaint>>,
    particles: Vec<usize>,
}

/// Paints the reply thumbnail of [`HOLDER`] for a few frames.
fn run_thumb(store: MockHistoryStore, reply: ReplyHeader) -> ThumbState {
    let state = ThumbState {
        assets: MediaAssetCache::new(),
        store,
        reply,
        painted: Vec::new(),
        particles: Vec::new(),
    };
    let mut harness = HarnessBuilder::default()
        .with_size(Vec2::new(320.0, 80.0))
        .with_max_steps(8)
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, st: &mut ThumbState| {
                let now_ms = (ui.ctx().input(|i| i.time) * 1000.0) as i64;
                st.assets.reply_previews.poll(ui.ctx(), &st.store, now_ms);
                let reply_rect = Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::new(300.0, 40.0));
                let before = ui.painter().add(egui::Shape::Noop);
                st.painted.push(paint_reply_thumb(
                    ui,
                    &mut st.assets,
                    peer(),
                    HOLDER,
                    &st.reply,
                    reply_rect,
                ));
                let after = ui.painter().add(egui::Shape::Noop);
                // Shapes added by the paint: the image, then one circle per particle.
                st.particles
                    .push(after.0.saturating_sub(before.0).saturating_sub(2));
            },
            state,
        );
    harness.run_steps(6);
    let st = harness.state_mut();
    ThumbState {
        assets: std::mem::replace(&mut st.assets, MediaAssetCache::new()),
        store: MockHistoryStore::new(),
        reply: st.reply.clone(),
        painted: st.painted.clone(),
        particles: st.particles.clone(),
    }
}

#[test]
fn a_story_reply_thumbnail_is_asked_for_by_the_message_that_holds_the_reply() {
    // The old cache asked the backend with the reply's target id, which a story reply (and a
    // reply to a message of another chat) does not have: the thumbnail never loaded.
    let mut store = MockHistoryStore::new();
    store.set_reply_preview_thumb(peer(), HOLDER, jpeg([200, 40, 40]));
    let st = run_thumb(
        store,
        reply(ReplyThumb {
            stripped: Some(synthetic_stripped_thumb(0x31)),
            key: 11,
            ..ReplyThumb::default()
        }),
    );
    assert_eq!(st.painted.first(), Some(&Some(ReplyThumbPaint::Blurred)));
    assert_eq!(st.painted.last(), Some(&Some(ReplyThumbPaint::Real)));
    assert_eq!(st.assets.reply_previews.backend_asks(), 1);
    assert_eq!(st.particles.last(), Some(&0), "no spoiler, no particles");
}

#[test]
fn a_spoiler_thumbnail_stays_blurred_and_gets_the_particles() {
    // With an inline thumbnail: the file is never asked for.
    let mut store = MockHistoryStore::new();
    store.set_reply_preview_thumb(peer(), HOLDER, jpeg([200, 40, 40]));
    let header = reply(ReplyThumb {
        stripped: Some(synthetic_stripped_thumb(0x31)),
        spoiler: true,
        spoiler_animation: true,
        key: 12,
        ..ReplyThumb::default()
    });
    assert!(reply_thumb_has_spoiler_particles(&header));
    let st = run_thumb(store, header);
    assert_eq!(st.painted.last(), Some(&Some(ReplyThumbPaint::Blurred)));
    assert_eq!(st.assets.reply_previews.backend_asks(), 0);
    assert!(
        st.particles.last().is_some_and(|&n| n > 0),
        "spoiler particles are drawn over the preview: {:?}",
        st.particles
    );

    // Without an inline thumbnail the file is loaded and blurred
    // (`ReplyPreview::image`: `inlineThumbnailBytes.isEmpty()`).
    let mut store = MockHistoryStore::new();
    store.set_reply_preview_thumb(peer(), HOLDER, jpeg([200, 40, 40]));
    let st = run_thumb(
        store,
        reply(ReplyThumb {
            stripped: None,
            spoiler: true,
            spoiler_animation: true,
            key: 13,
            ..ReplyThumb::default()
        }),
    );
    assert_eq!(st.painted.last(), Some(&Some(ReplyThumbPaint::RealBlurred)));
    assert_eq!(st.assets.reply_previews.backend_asks(), 1);

    // A reply to media from another chat is blurred by the holder's flag but has no
    // particles (`Reply::_spoiler` exists only for a resolved message).
    let external = reply(ReplyThumb {
        stripped: Some(synthetic_stripped_thumb(0x31)),
        spoiler: true,
        spoiler_animation: false,
        key: 14,
        ..ReplyThumb::default()
    });
    assert!(!reply_thumb_has_spoiler_particles(&external));
    let st = run_thumb(MockHistoryStore::new(), external);
    assert_eq!(st.particles.last(), Some(&0));
}

#[test]
fn a_blurred_decode_loses_the_detail_of_the_file() {
    // Left half black, right half white: sharp it has two flat halves, blurred the columns
    // next to the edge are grey.
    let mut img = image::RgbImage::new(80, 80);
    for (x, _, px) in img.enumerate_pixels_mut() {
        *px = image::Rgb(if x < 40 { [0, 0, 0] } else { [255, 255, 255] });
    }
    let mut png = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encode png");
    let sharp = decode_reply_thumb(&png, 80, false).expect("decoded");
    let blurred = decode_reply_thumb(&png, 80, true).expect("decoded");
    let greys = |image: &egui::ColorImage| {
        image
            .pixels
            .iter()
            .filter(|p| p.r() > 24 && p.r() < 232)
            .count()
    };
    assert_eq!(greys(&sharp), 0);
    assert!(greys(&blurred) > 80 * 4, "{} grey pixels", greys(&blurred));
}

#[test]
fn an_edited_target_loads_its_new_thumbnail() {
    // The image key changes when the replied-to media changes: the cache must not keep
    // showing the old texture for the same holder.
    struct State {
        assets: MediaAssetCache,
        store: MockHistoryStore,
        key: u64,
    }
    let mut store = MockHistoryStore::new();
    store.set_reply_preview_thumb(peer(), HOLDER, jpeg([200, 40, 40]));
    let mut harness = HarnessBuilder::default()
        .with_size(Vec2::new(320.0, 80.0))
        .with_max_steps(8)
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, st: &mut State| {
                let now_ms = (ui.ctx().input(|i| i.time) * 1000.0) as i64;
                st.assets.reply_previews.poll(ui.ctx(), &st.store, now_ms);
                let reply_rect = Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::new(300.0, 40.0));
                let header = reply(ReplyThumb {
                    key: st.key,
                    ..ReplyThumb::default()
                });
                let _ = paint_reply_thumb(ui, &mut st.assets, peer(), HOLDER, &header, reply_rect);
            },
            State {
                assets: MediaAssetCache::new(),
                store,
                key: 1,
            },
        );
    harness.run_steps(4);
    assert_eq!(harness.state().assets.reply_previews.backend_asks(), 1);
    harness.state_mut().key = 2;
    harness.run_steps(4);
    assert_eq!(
        harness.state().assets.reply_previews.backend_asks(),
        2,
        "a new image key asks the backend again"
    );
}

// --- evidence images -------------------------------------------------------------------------

/// Renders the scenes of this package into `WAVE4_SNAP_DIR` (skipped when it is not set), with
/// `WAVE4_SNAP_TAG` in the file names. The "old data" variant holds what the old conversion
/// code produced for the same messages.
#[test]
fn wave4_media_snapshots() {
    use crate::history::backend::{AlbumItem, PhotoAttachment};
    let Some(dir) = std::env::var_os("WAVE4_SNAP_DIR").map(std::path::PathBuf::from) else {
        return;
    };
    let tag = std::env::var("WAVE4_SNAP_TAG").unwrap_or_else(|_| "after".to_owned());
    std::fs::create_dir_all(&dir).expect("create snap dir");
    let save = |name: &str, img: image::RgbaImage| {
        img.save(dir.join(format!("{name}_{tag}.png")))
            .expect("save snapshot");
    };
    let peer = PeerId::from_user(7001);
    let picture = synthetic_thumb_picture(0x31);

    // TGB-2981: a demo photo and a demo album, with the thumbnails the mock data carries and
    // no full-size bytes.
    let mut demo_photo = photo_message(peer, 1, (1280, 820), &picture, "");
    if let Some(HistoryMedia::Photo(PhotoAttachment { stripped_thumb, .. })) = &mut demo_photo.media
    {
        *stripped_thumb = Some(synthetic_stripped_thumb(0x3C));
    }
    let mut album = photo_message(peer, 2, (1, 1), &picture, "Studio gallery album (3 items)");
    album.media = Some(HistoryMedia::Album(
        [(20, 0x21u8, None), (21, 0x45, None), (22, 0x63, Some(14))]
            .into_iter()
            .map(|(id, seed, video)| AlbumItem {
                msg_id: MsgId(id),
                width: 800,
                height: 800,
                stripped_thumb: Some(synthetic_stripped_thumb(seed)),
                fallback_rgb: ([0x4E, 0x9A, 0xD4], [0x2A, 0x62, 0x92]),
                video_duration_secs: video,
                label: "album.jpg".to_owned(),
            })
            .collect(),
    ));
    let mut harness = build_photo_harness(
        peer,
        vec![demo_photo, album],
        Vec::new(),
        Vec2::new(560.0, 640.0),
        true,
    );
    for _ in 0..4 {
        harness.step();
    }
    save("demo_photos", harness.render().expect("render"));

    // TGB-2982: a loading photo, the pointer over it, and after the cancel.
    let photo_id = egui::Id::unique(("tg_photo", 3));
    for (name, state, hover) in [
        ("photo_loading", loading(), false),
        ("photo_loading_hover", loading(), true),
        (
            "photo_cancelled",
            InlineMediaDescriptor {
                photo_load: Some(PhotoLoadState::NotLoaded),
                ..InlineMediaDescriptor::default()
            },
            false,
        ),
    ] {
        let msg = photo_message(peer, 3, (1280, 820), &picture, "");
        let mut harness = build_photo_harness(
            peer,
            vec![msg],
            vec![(MsgId(3), state)],
            Vec2::new(560.0, 360.0),
            true,
        );
        harness.step();
        if hover && let Some(resp) = harness.ctx.read_response(photo_id) {
            harness
                .input_mut()
                .events
                .push(egui::Event::PointerMoved(resp.rect.center()));
        }
        for _ in 0..8 {
            harness.step();
        }
        save(name, harness.render().expect("render"));
    }

    // TGB-2958: a voice message whose file is in the cache (chat reopened, never played in
    // this view), and one that is not cached.
    for (name, desc) in [
        (
            "voice_cached",
            InlineMediaDescriptor {
                audio_bytes: Some(synthesize_tone_wav(300.0, 2.0)),
                audio_ext: Some("wav".to_owned()),
                ..InlineMediaDescriptor::default()
            },
        ),
        (
            "voice_not_cached",
            InlineMediaDescriptor {
                voice_load: Some(VoiceLoadState::NotLoaded),
                ..InlineMediaDescriptor::default()
            },
        ),
    ] {
        let mut msg = photo_message(peer, 4, (1, 1), &picture, "");
        msg.media = Some(HistoryMedia::Voice(VoiceAttachment {
            duration_secs: 4,
            waveform_5bit: crate::history::mock::sample_voice_waveform_5bit(7),
            size_bytes: 16_300,
            is_unread: false,
            transcription: None,
        }));
        let mut harness = build_photo_harness(
            peer,
            vec![msg],
            vec![(MsgId(4), desc)],
            Vec2::new(560.0, 140.0),
            true,
        );
        for _ in 0..4 {
            harness.step();
        }
        save(name, harness.render().expect("render"));
    }

    // TGB-2959: replies to a spoilered photo, a story, a video with a cover and an invoice.
    for old in [false, true] {
        let header = |text: &str, thumb: Option<ReplyThumb>| ReplyHeader {
            reply_to_msg_id: MsgId(0),
            author_name: "Noor Ilves".to_owned(),
            author_color_id: 1003,
            preview_text: text.to_owned(),
            preview_thumb_rgb: thumb.as_ref().map(|_| [0x6E, 0xC9, 0xCB]),
            preview_thumb: thumb,
            preview_colored_len: text.find(',').map_or(text.len(), |i| i + 1),
            quote_text: None,
        };
        let stripped = |seed| Some(synthetic_stripped_thumb(seed));
        // What the old conversion produced: the spoiler flag without particles, the video's
        // own thumbnail instead of its cover, and no preview for a story or an invoice.
        let replies = [
            (
                "Photo",
                Some(ReplyThumb {
                    stripped: stripped(0x21),
                    spoiler: true,
                    spoiler_animation: !old,
                    key: 1,
                    ..ReplyThumb::default()
                }),
                None,
            ),
            (
                "Story, sunset",
                (!old).then(|| ReplyThumb {
                    stripped: stripped(0x45),
                    key: 2,
                    ..ReplyThumb::default()
                }),
                Some([230u8, 120, 40]),
            ),
            (
                "Video",
                Some(ReplyThumb {
                    stripped: stripped(if old { 0x63 } else { 0x8A }),
                    key: 3,
                    ..ReplyThumb::default()
                }),
                Some(if old { [40u8, 60, 90] } else { [200, 60, 160] }),
            ),
            (
                "Pro Plan",
                (!old).then(|| ReplyThumb {
                    key: 4,
                    ..ReplyThumb::default()
                }),
                Some([60u8, 170, 90]),
            ),
        ];
        let mut messages = Vec::new();
        let mut store_thumbs = Vec::new();
        for (i, (text, thumb, file)) in replies.into_iter().enumerate() {
            let id = 40 + i as i32;
            let mut msg = photo_message(peer, id, (1, 1), &picture, "Answer to this one");
            msg.media = None;
            if thumb.is_some()
                && let Some(rgb) = file
            {
                store_thumbs.push((MsgId(id), jpeg(rgb)));
            }
            msg.reply = Some(header(text, thumb));
            messages.push(msg);
        }
        let mut harness = build_photo_harness_with(
            peer,
            messages,
            Vec::new(),
            Vec2::new(560.0, 330.0),
            true,
            |store| {
                for (id, bytes) in store_thumbs {
                    store.set_reply_preview_thumb(peer, id, bytes);
                }
            },
        );
        for _ in 0..8 {
            harness.step();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        save(
            if old { "replies_old_data" } else { "replies" },
            harness.render().expect("render"),
        );
    }
}
