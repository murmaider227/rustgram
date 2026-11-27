//! TGB-3078: when the custom emoji texture cache is full, the least recently painted frames go,
//! not every texture at once. The old code cleared all 1024 textures on the miss that hit the
//! cap, so the emoji on screen re-rasterized and re-uploaded every frame of their loop.

use std::time::{Duration, Instant};

use egui::{Color32, Context, LayerId, Painter, RawInput, Rect, pos2, vec2};

use crate::custom_emoji::{CustomEmojiAsset, CustomEmojiPaintContext, CustomEmojiRenderer};

/// A 30-frame (30 fps) animated Lottie: a dot moving across a 64×64 canvas.
fn animated_lottie_json() -> String {
    r#"{
        "v": "5.5.2", "fr": 30, "ip": 0, "op": 30, "w": 64, "h": 64,
        "layers": [{
            "ty": 4, "nm": "dot", "ip": 0, "op": 30, "st": 0,
            "ks": {
                "o": {"a": 0, "k": 100}, "r": {"a": 0, "k": 0},
                "p": {"a": 1, "k": [
                    {"t": 0, "s": [16, 32, 0], "e": [48, 32, 0]},
                    {"t": 30, "s": [48, 32, 0]}
                ]},
                "a": {"a": 0, "k": [0, 0, 0]}, "s": {"a": 0, "k": [100, 100, 100]}
            },
            "shapes": [{"ty": "gr", "it": [
                {"ty": "el", "p": {"a": 0, "k": [0, 0]}, "s": {"a": 0, "k": [20, 20]}},
                {"ty": "fl", "c": {"a": 0, "k": [0.9, 0.2, 0.3, 1]}, "o": {"a": 0, "k": 100}},
                {"ty": "tr", "p": {"a": 0, "k": [0, 0]}, "a": {"a": 0, "k": [0, 0]},
                 "s": {"a": 0, "k": [100, 100]}, "r": {"a": 0, "k": 0}, "o": {"a": 0, "k": 100}}
            ]}]
        }]
    }"#
    .to_owned()
}

const LOOP_FRAMES: usize = 30;
/// Emoji on screen, painted every frame at 32 px.
const VISIBLE: usize = 12;
/// Emoji scrolled past once, at two other sizes, filling the cache.
const SCROLLED: usize = 12;
/// New (scrolled) textures uploaded per frame while the cache fills.
const NEW_PER_FRAME: usize = 10;

fn id(i: usize) -> String {
    format!("71000{i:02}")
}

fn time_of(frame: usize) -> f64 {
    (frame % LOOP_FRAMES) as f64 / LOOP_FRAMES as f64 + 0.001
}

fn texture_count(renderer: &CustomEmojiRenderer) -> usize {
    let debug = format!("{renderer:?}");
    debug
        .split("textures_count: ")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(usize::MAX)
}

/// Runs one headless egui pass with a painter over the whole screen.
fn with_painter(ctx: &Context, mut body: impl FnMut(&Context, &Painter)) {
    let mut output = ctx.run_ui(RawInput::default(), |ui| {
        let painter = Painter::new(ui.ctx().clone(), LayerId::background(), Rect::EVERYTHING);
        body(ui.ctx(), &painter);
    });
    output.textures_delta.clear();
}

fn paint_visible(renderer: &mut CustomEmojiRenderer, ctx: &Context, painter: &Painter, f: usize) {
    let paint_ctx = CustomEmojiPaintContext::playing(time_of(f), Color32::WHITE);
    for i in 0..VISIBLE {
        let rect = Rect::from_min_size(pos2(i as f32 * 36.0, 10.0), vec2(32.0, 32.0));
        assert!(renderer.paint(ctx, painter, &id(i), rect, paint_ctx));
    }
}

#[test]
fn a_full_cache_keeps_the_textures_of_emoji_on_screen() {
    let ctx = Context::default();
    let mut renderer = CustomEmojiRenderer::new();
    let json = animated_lottie_json();
    for i in 0..VISIBLE + SCROLLED {
        assert!(renderer.register_asset(id(i), &CustomEmojiAsset::lottie(&json)));
    }
    // Every (emoji, frame, size) a scroll past the other emoji uploads once.
    let scrolled: Vec<(usize, usize, f32)> = (VISIBLE..VISIBLE + SCROLLED)
        .flat_map(|i| {
            [40.0, 48.0]
                .into_iter()
                .flat_map(move |side| (0..LOOP_FRAMES).map(move |frame| (i, frame, side)))
        })
        .collect();

    let mut frame = 0usize;
    let mut frame_times = Vec::new();
    let mut evicted_at = None;
    for batch in scrolled.chunks(NEW_PER_FRAME) {
        let before = texture_count(&renderer);
        let started = Instant::now();
        with_painter(&ctx, |ctx, painter| {
            paint_visible(&mut renderer, ctx, painter, frame);
            for &(i, f, side) in batch {
                let rect = Rect::from_min_size(pos2(0.0, 60.0), vec2(side, side));
                let paint_ctx = CustomEmojiPaintContext::playing(time_of(f), Color32::WHITE);
                assert!(renderer.paint(ctx, painter, &id(i), rect, paint_ctx));
            }
        });
        frame_times.push(started.elapsed());
        if evicted_at.is_none() && texture_count(&renderer) < before {
            evicted_at = Some(frame);
        }
        frame += 1;
    }
    let evicted_at = evicted_at.expect("the scroll fills the cache past its cap");

    // One more loop of the emoji on screen: every frame of it should still be uploaded.
    let before = texture_count(&renderer);
    let started = Instant::now();
    let mut slowest = Duration::ZERO;
    for _ in 0..LOOP_FRAMES {
        let one = Instant::now();
        with_painter(&ctx, |ctx, painter| {
            paint_visible(&mut renderer, ctx, painter, frame)
        });
        slowest = slowest.max(one.elapsed());
        frame += 1;
    }
    let loop_time = started.elapsed();
    let reuploaded = texture_count(&renderer) - before;
    println!(
        "measured: cap crossed on frame {evicted_at} ({:.2} ms); the next {LOOP_FRAMES} frames of {VISIBLE} emoji on screen: {:.2} ms total, slowest {:.2} ms, {reuploaded} textures re-rasterized",
        frame_times[evicted_at].as_secs_f64() * 1e3,
        loop_time.as_secs_f64() * 1e3,
        slowest.as_secs_f64() * 1e3,
    );
    assert_eq!(
        reuploaded, 0,
        "the emoji on screen lost their textures when the cache filled"
    );
    drop(renderer);
    with_painter(&ctx, |_, _| {});
}

#[test]
fn frame_for_cache_keeps_recent_frames_when_full() {
    let mut renderer = CustomEmojiRenderer::new();
    let json = animated_lottie_json();
    for i in 0..VISIBLE + SCROLLED {
        assert!(renderer.register_asset(id(i), &CustomEmojiAsset::lottie(&json)));
    }
    let frame_count = |renderer: &CustomEmojiRenderer| {
        let debug = format!("{renderer:?}");
        debug
            .split("frame_cache_count: ")
            .nth(1)
            .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|n| n.parse::<usize>().ok())
            .unwrap_or(usize::MAX)
    };
    // 24 emoji x 30 frames x 2 sizes = 1440 decoded frames, more than the cap of 1024.
    for size in [32, 48] {
        for i in 0..VISIBLE + SCROLLED {
            for f in 0..LOOP_FRAMES {
                let paint_ctx = CustomEmojiPaintContext::playing(time_of(f), Color32::WHITE);
                assert!(renderer.frame_for(&id(i), size, paint_ctx).is_some());
            }
        }
    }
    let count = frame_count(&renderer);
    assert!((512..=1024).contains(&count), "{count}");
    // The last emoji asked for (48 px) are all still cached.
    let before = frame_count(&renderer);
    for f in 0..LOOP_FRAMES {
        let paint_ctx = CustomEmojiPaintContext::playing(time_of(f), Color32::WHITE);
        assert!(
            renderer
                .frame_for(&id(VISIBLE + SCROLLED - 1), 48, paint_ctx)
                .is_some()
        );
    }
    assert_eq!(frame_count(&renderer), before);
}
