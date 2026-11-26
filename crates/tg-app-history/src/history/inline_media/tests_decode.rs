//! Decode pool: photos, video thumbnails and Lottie frames are decoded off the paint call, at
//! display size, with the blurred state shown meanwhile, the same step timing, bounded memory.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Context, Pos2, RawInput, Rect, Vec2};
use image::{ImageFormat, Rgba, RgbaImage};

use super::*;

fn jpeg(width: u32, height: u32, seed: u8) -> Arc<[u8]> {
    let img = RgbaImage::from_fn(width, height, |x, y| {
        Rgba([
            (x as u8).wrapping_add(seed),
            (y as u8).wrapping_mul(3),
            seed,
            255,
        ])
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .to_rgb8()
        .write_to(&mut std::io::Cursor::new(&mut out), ImageFormat::Jpeg)
        .expect("encode jpeg");
    out.into()
}

fn ready(bytes: &Arc<[u8]>) -> InlineMediaDescriptor {
    InlineMediaDescriptor {
        photo_load: Some(PhotoLoadState::Ready {
            bytes: Arc::clone(bytes),
        }),
        ..InlineMediaDescriptor::default()
    }
}

const BUBBLE: Rect = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(320.0, 240.0));

/// Paints `ids` in one egui pass at time `t`; returns the time spent in the paint calls.
fn paint_photos(ctx: &Context, rt: &mut HistoryMediaRuntime, ids: &[MsgId], t: f64) -> Duration {
    let mut spent = Duration::ZERO;
    let input = RawInput {
        time: Some(t),
        ..RawInput::default()
    };
    let mut output = ctx.run_ui(input, |ui| {
        let painter = ui.painter().clone();
        let started = Instant::now();
        for &id in ids {
            rt.paint_photo_load_and_blur_up(
                ui.ctx(),
                &painter,
                id,
                BUBBLE,
                egui::CornerRadius::ZERO,
                crate::history::photo_frame::PhotoFit::Single,
                t,
                0.0,
            );
        }
        spent = started.elapsed();
    });
    // No renderer here: drop the texture uploads the pass produced.
    output.textures_delta.clear();
    spent
}

fn wait_until(mut pred: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    false
}

#[test]
fn photos_decode_off_the_paint_call_at_display_size() {
    const COUNT: i32 = 24;
    let ids: Vec<MsgId> = (1..=COUNT).map(MsgId).collect();
    let photos: Vec<Arc<[u8]>> = (0..COUNT).map(|i| jpeg(1600, 1200, i as u8)).collect();

    // Before: every photo decoded inside the first paint (as on master, now at display size).
    let sync_ctx = Context::default();
    let mut sync_rt = HistoryMediaRuntime::new(false);
    sync_rt.set_background_decode(Some(false));
    for (id, bytes) in ids.iter().zip(&photos) {
        sync_rt.register_descriptor(*id, ready(bytes));
    }
    let sync_first = paint_photos(&sync_ctx, &mut sync_rt, &ids, 1.0);
    assert!(
        ids.iter()
            .all(|id| sync_rt.decoded_photo_size(*id).is_some())
    );

    // After: the paint only queues the work; the blurred thumbnail stays until it lands.
    let ctx = Context::default();
    let mut rt = HistoryMediaRuntime::new(false);
    rt.set_background_decode(Some(true));
    for (id, bytes) in ids.iter().zip(&photos) {
        rt.register_descriptor(*id, ready(bytes));
    }
    let bg_first = paint_photos(&ctx, &mut rt, &ids, 1.0);
    assert!(
        ids.iter().all(|id| rt.decoded_photo_size(*id).is_none()),
        "nothing is painted over the blurred thumbnail before the decode lands"
    );
    let started = Instant::now();
    let mut t = 1.0;
    assert!(wait_until(|| {
        t += 0.016;
        paint_photos(&ctx, &mut rt, &ids, t);
        ids.iter().all(|id| rt.decoded_photo_size(*id).is_some())
    }));
    let bg_total = started.elapsed();
    println!(
        "{COUNT} photos 1600x1200: first paint {sync_first:?} decoding inline, \
         {bg_first:?} with background decode (all landed after {bg_total:?})"
    );
    assert!(bg_first < sync_first);
    assert_eq!(rt.background_decode_jobs(), COUNT as u64);

    // Display size: a 320x240 bubble at 1x gets a 384-px bucket, not the 1600x1200 source.
    let size = rt.decoded_photo_size(MsgId(1)).expect("decoded");
    assert_eq!(size, [384, 288]);
    assert_eq!(sync_rt.decoded_photo_size(MsgId(1)), Some([384, 288]));

    // Warm paints neither decode again nor copy: the job count stays put.
    for _ in 0..10 {
        t += 0.016;
        paint_photos(&ctx, &mut rt, &ids, t);
    }
    assert_eq!(rt.background_decode_jobs(), COUNT as u64);

    // Reloading the same bytes keeps the pixels: no new decode.
    rt.register_descriptor(MsgId(1), ready(&photos[0]));
    paint_photos(&ctx, &mut rt, &ids, t + 0.016);
    assert_eq!(rt.background_decode_jobs(), COUNT as u64);
    assert!(rt.decoded_photo_size(MsgId(1)).is_some());

    // New bytes for a message decode again; the stale result of the old bytes is dropped.
    let replacement = jpeg(200, 100, 99);
    rt.register_descriptor(MsgId(2), ready(&replacement));
    assert!(wait_until(|| {
        t += 0.016;
        paint_photos(&ctx, &mut rt, &ids, t);
        rt.decoded_photo_size(MsgId(2)) == Some([200, 100])
    }));
}

#[test]
fn decoded_photo_memory_stays_within_the_budget() {
    let ctx = Context::default();
    let mut rt = HistoryMediaRuntime::new(false);
    rt.set_background_decode(Some(false));
    // 384x288 RGBA = 432 KiB per photo; keep room for 3.
    rt.full_photos = AssetLru::new(3 * 384 * 288 * 4);
    let ids: Vec<MsgId> = (1..=10).map(MsgId).collect();
    for id in &ids {
        rt.register_descriptor(*id, ready(&jpeg(800, 600, id.0 as u8)));
    }
    for id in &ids {
        paint_photos(&ctx, &mut rt, &[*id], 1.0);
    }
    let (bytes, evicted) = rt.decoded_asset_stats();
    assert_eq!(bytes, 3 * 384 * 288 * 4);
    assert_eq!(evicted, 7);
    // An evicted photo comes back through the same path, with the same pixels size.
    paint_photos(&ctx, &mut rt, &[MsgId(1)], 2.0);
    assert_eq!(rt.decoded_photo_size(MsgId(1)), Some([384, 288]));
}

fn minimal_lottie_json() -> String {
    r#"{"v":"5.5.2","fr":30,"ip":0,"op":30,"w":64,"h":64,"nm":"t","ddd":0,"assets":[],"layers":[]}"#
        .to_owned()
}

fn lottie_step(ctx: &Context, texture: &TextureHandle) -> String {
    ctx.tex_manager()
        .read()
        .meta(texture.id())
        .map(|meta| meta.name.clone())
        .unwrap_or_default()
}

#[test]
fn lottie_frames_render_in_the_background_with_the_same_steps() {
    let json = minimal_lottie_json();
    let sync_ctx = Context::default();
    let mut sync_rt = HistoryMediaRuntime::new(false);
    sync_rt.set_background_decode(Some(false));

    let ctx = Context::default();
    let mut rt = HistoryMediaRuntime::new(false);
    rt.set_background_decode(Some(true));

    // First paint: parsing happens on a worker.
    assert!(matches!(
        rt.lottie_frame_per_size(&ctx, MsgId(5), 77, &json, 0.0, 128),
        LottieFrameState::Pending
    ));
    let mut first = None;
    assert!(wait_until(|| {
        if let LottieFrameState::Ready { texture, .. } =
            rt.lottie_frame_per_size(&ctx, MsgId(5), 77, &json, 0.0, 128)
        {
            first = Some(texture);
        }
        first.is_some()
    }));
    // The other steps were queued too: the whole loop lands without further paints asking.
    assert!(wait_until(|| {
        rt.pump_decoded(&ctx);
        rt.lottie_frame_cache_len() == 24
    }));

    // Same step for the same time as the synchronous path, for a whole loop.
    let _ = sync_rt.lottie_frame_per_size(&sync_ctx, MsgId(5), 77, &json, 0.0, 128);
    for i in 0..48 {
        let t = f64::from(i) * 0.021;
        let LottieFrameState::Ready { texture: a, .. } =
            rt.lottie_frame_per_size(&ctx, MsgId(5), 77, &json, t, 128)
        else {
            panic!("background frame missing at {t}");
        };
        let LottieFrameState::Ready { texture: b, .. } =
            sync_rt.lottie_frame_per_size(&sync_ctx, MsgId(5), 77, &json, t, 128)
        else {
            panic!("sync frame missing at {t}");
        };
        assert_eq!(lottie_step(&ctx, &a), lottie_step(&sync_ctx, &b), "t = {t}");
    }

    // Broken JSON is reported as such (the caller paints its fallback badge).
    let mut state = LottieFrameState::Pending;
    assert!(wait_until(|| {
        state = rt.lottie_frame_per_size(&ctx, MsgId(6), 78, "not json", 0.0, 128);
        !matches!(state, LottieFrameState::Pending)
    }));
    assert!(matches!(state, LottieFrameState::Unavailable));
}

#[test]
fn evicted_lottie_textures_are_refilled_from_the_next_frame_on() {
    let json = minimal_lottie_json();
    let ctx = Context::default();
    let mut rt = HistoryMediaRuntime::new(false);
    rt.set_background_decode(Some(false));
    // Room for 4 frames of 128 px; a whole loop has 24.
    rt.lottie_size_frames = AssetLru::new(4 * 128 * 128 * 4);
    let created = |ctx: &Context| {
        ctx.tex_manager()
            .read()
            .allocated()
            .filter(|(_, meta)| meta.name.starts_with("tg_lottie_"))
            .count()
    };
    for i in 0..48 {
        // One egui pass per step, like the app's frames.
        let mut output = ctx.run_ui(RawInput::default(), |ui| {
            let t = f64::from(i) * 0.034;
            let _ = rt.lottie_frame_per_size(ui.ctx(), MsgId(5), 77, &json, t, 128);
        });
        output.textures_delta.clear();
    }
    assert!(rt.reused_textures() > 0, "evicted frames must be refilled");
    assert!(
        created(&ctx) <= 4 + LOTTIE_TEXTURE_POOL_CAP + 1,
        "textures created: {}",
        created(&ctx)
    );
}

#[test]
fn a_failed_video_thumbnail_is_not_retried_every_paint() {
    let ctx = Context::default();
    let mut rt = HistoryMediaRuntime::new(false);
    rt.set_background_decode(Some(true));
    rt.register_descriptor(
        MsgId(9),
        InlineMediaDescriptor {
            video_data: Some(InlineDocumentData::Bytes(b"not a video".to_vec())),
            ..InlineMediaDescriptor::default()
        },
    );
    assert!(
        rt.get_or_generate_video_thumb(&ctx, MsgId(9), 160, false)
            .is_none()
    );
    assert!(wait_until(|| {
        rt.pump_decoded(&ctx);
        rt.video_thumbs.contains(&MsgId(9))
    }));
    for _ in 0..20 {
        assert!(
            rt.get_or_generate_video_thumb(&ctx, MsgId(9), 160, false)
                .is_none()
        );
    }
    assert_eq!(rt.background_decode_jobs(), 1);
    let _ = Vec2::ZERO;
}
