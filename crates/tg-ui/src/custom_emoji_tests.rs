//! Paint path of [`CustomEmojiRenderer`] (TGB-3062): what one frame of visible custom emoji
//! costs, and that a cached frame is painted from its texture alone.

use std::time::Instant;

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

/// Runs `body` inside one headless egui pass with a painter over the whole screen.
fn with_painter(ctx: &Context, mut body: impl FnMut(&Context, &Painter)) {
    let mut output = ctx.run_ui(RawInput::default(), |ui| {
        let painter = Painter::new(ui.ctx().clone(), LayerId::background(), Rect::EVERYTHING);
        body(ui.ctx(), &painter);
    });
    // No renderer here: drop the texture uploads the pass queued.
    output.textures_delta.clear();
}

/// Drops `renderer` (freeing its textures) and the frees it queued in `ctx`.
fn finish(ctx: &Context, renderer: CustomEmojiRenderer) {
    drop(renderer);
    with_painter(ctx, |_, _| {});
}

const EMOJI: usize = 24;

fn renderer_with_emoji() -> CustomEmojiRenderer {
    let mut renderer = CustomEmojiRenderer::new();
    let json = animated_lottie_json();
    for i in 0..EMOJI {
        assert!(renderer.register_asset(format!("70000{i}"), &CustomEmojiAsset::lottie(&json)));
    }
    renderer
}

fn emoji_rect(i: usize, side: f32) -> Rect {
    Rect::from_min_size(pos2(i as f32 * (side + 4.0), 10.0), vec2(side, side))
}

#[test]
fn custom_emoji_paint_frame_measurement() {
    // 32 px: an emoji in message text at 100% scale; 64 px: the same at 200%.
    for side in [32.0, 64.0] {
        measure_paints(side);
    }
}

fn measure_paints(side: f32) {
    let ctx = Context::default();
    let mut renderer = renderer_with_emoji();
    // A 30-frame loop at 30 fps: one pass over all of it fills the caches, the timed passes
    // then paint frames that are already uploaded (what a steady chat frame does).
    let times: Vec<f64> = (0..30).map(|f| f64::from(f) / 30.0 + 0.001).collect();
    with_painter(&ctx, |ctx, painter| {
        for &t in &times {
            for i in 0..EMOJI {
                let paint_ctx = CustomEmojiPaintContext::playing(t, Color32::WHITE);
                assert!(renderer.paint(
                    ctx,
                    painter,
                    &format!("70000{i}"),
                    emoji_rect(i, side),
                    paint_ctx
                ));
            }
        }
    });
    let ids: Vec<String> = (0..EMOJI).map(|i| format!("70000{i}")).collect();
    const PASSES: usize = 20;
    let mut paints = 0usize;
    let mut elapsed = std::time::Duration::ZERO;
    with_painter(&ctx, |ctx, painter| {
        let started = Instant::now();
        for _ in 0..PASSES {
            for &t in &times {
                for (i, id) in ids.iter().enumerate() {
                    let paint_ctx = CustomEmojiPaintContext::playing(t, Color32::WHITE);
                    renderer.paint(ctx, painter, id, emoji_rect(i, side), paint_ctx);
                    paints += 1;
                }
            }
        }
        elapsed = started.elapsed();
    });
    println!(
        "measured: {EMOJI} animated custom emoji at {side} px, {paints} cached-frame paints, {:.3} us per paint, {:.1} us per frame of {EMOJI} emoji",
        elapsed.as_secs_f64() * 1e6 / paints as f64,
        elapsed.as_secs_f64() * 1e6 / paints as f64 * EMOJI as f64,
    );
    finish(&ctx, renderer);
}

#[test]
fn painting_keeps_no_cpu_copy_of_uploaded_frames_and_remove_drops_textures() {
    let ctx = Context::default();
    let mut renderer = renderer_with_emoji();
    with_painter(&ctx, |ctx, painter| {
        for f in 0..30 {
            let paint_ctx = CustomEmojiPaintContext::playing(f64::from(f) / 30.0, Color32::WHITE);
            for i in 0..EMOJI {
                assert!(renderer.paint(
                    ctx,
                    painter,
                    &format!("70000{i}"),
                    emoji_rect(i, 32.0),
                    paint_ctx
                ));
            }
        }
    });
    // Every frame of every emoji has a texture; none keeps a second, CPU copy of its pixels
    // (the old paint path kept both, up to 1024 frames of up to 64 KB each).
    let debug = format!("{renderer:?}");
    assert!(debug.contains("frame_cache_count: 0"), "{debug}");
    assert!(
        debug.contains(&format!("textures_count: {}", 30 * EMOJI)),
        "{debug}"
    );

    renderer.remove("700000");
    let debug = format!("{renderer:?}");
    assert!(
        debug.contains(&format!("textures_count: {}", 30 * (EMOJI - 1))),
        "{debug}"
    );
    assert!(!renderer.contains("700000"));
    finish(&ctx, renderer);
}

#[test]
fn paint_and_frame_for_pick_the_same_frame() {
    let ctx = Context::default();
    let mut renderer = renderer_with_emoji();
    for (time, expected) in [(0.0, 0), (0.5, 15), (0.99, 29), (1.2, 6)] {
        let paint_ctx = CustomEmojiPaintContext::playing(time, Color32::WHITE);
        let frame = renderer
            .frame_for("700001", 32, paint_ctx)
            .map(|f| f.frame_index);
        assert_eq!(frame, Some(expected), "time {time}");
    }
    let paused_last = CustomEmojiPaintContext {
        paused: true,
        force_last_frame: true,
        ..CustomEmojiPaintContext::playing(0.4, Color32::WHITE)
    };
    assert_eq!(
        renderer
            .frame_for("700001", 32, paused_last)
            .map(|f| f.frame_index),
        Some(29)
    );
    // `frame_for` keeps the four Lottie frames it rasterized (0.99 s and "paused on the last
    // frame" are both frame 29); painting adds no CPU frame, only one texture per (frame, size).
    with_painter(&ctx, |ctx, painter| {
        assert!(renderer.paint(ctx, painter, "700001", emoji_rect(0, 32.0), paused_last));
        assert!(renderer.paint(ctx, painter, "700001", emoji_rect(0, 32.0), paused_last));
        assert!(!renderer.paint(ctx, painter, "missing", emoji_rect(0, 32.0), paused_last));
    });
    let debug = format!("{renderer:?}");
    assert!(debug.contains("frame_cache_count: 4"), "{debug}");
    assert!(debug.contains("textures_count: 1"), "{debug}");
    finish(&ctx, renderer);
}
