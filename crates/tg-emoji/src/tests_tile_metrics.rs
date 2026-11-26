//! Measurements for the emoji texture path, printed for reports (ignored by default).
//!
//! Run in a release build:
//! `cargo test -p tg-emoji --release --lib tests_tile_metrics -- --ignored --nocapture --test-threads 1`

use std::time::{Duration, Instant};

use egui::Context;
use tg_text::emoji;

use crate::{EmojiAtlas, EmojiTextureCache};

const FRAME: Duration = Duration::from_micros(16_667);

fn leaked_atlas() -> &'static EmojiAtlas {
    Box::leak(Box::new(EmojiAtlas::new()))
}

fn emoji_texture_bytes(ctx: &Context) -> (usize, usize) {
    let manager = ctx.tex_manager();
    let manager = manager.read();
    manager
        .allocated()
        .filter(|(_, meta)| meta.name.starts_with("tg_emoji_"))
        .fold((0, 0), |(count, bytes), (_, meta)| {
            (count + 1, bytes + meta.size[0] * meta.size[1] * 4)
        })
}

/// First use of an emoji whose texture is not loaded yet, with background decode on (the
/// desktop setting): wall time until it can be drawn and frames at 60 Hz until it is drawn.
#[test]
#[ignore = "measurement, run with --ignored --nocapture in a release build"]
fn measure_first_use_decode_to_display() {
    // One drawable emoji from each sprite sheet's index range (512 indices per sheet).
    let samples: Vec<usize> = (0..8)
        .filter_map(|sheet| {
            (sheet * 512 + 100..(sheet + 1) * 512).find(|&i| EmojiAtlas::has_index(i))
        })
        .collect();
    assert_eq!(samples.len(), 8);
    let mut walls = Vec::new();
    let mut frames_all = Vec::new();
    for index in samples {
        let emoji_ref = emoji::by_index(index).expect("sample emoji");
        let seq = format!("#{index}");

        // Latency: poll without frame pacing.
        let ctx = Context::default();
        let mut cache = EmojiTextureCache::with_atlas(leaked_atlas());
        cache.set_background_decode(Some(true));
        let started = Instant::now();
        while cache.texture_and_uv(&ctx, emoji_ref).is_none() {
            assert!(started.elapsed() < Duration::from_secs(20), "never ready");
            std::thread::sleep(Duration::from_micros(200));
        }
        let wall = started.elapsed();

        // Frames: one paint per 60 Hz frame, count the frames until the emoji is drawn.
        let ctx = Context::default();
        let mut cache = EmojiTextureCache::with_atlas(leaked_atlas());
        cache.set_background_decode(Some(true));
        let mut frames = 0u32;
        let mut next = Instant::now();
        loop {
            frames += 1;
            let mut drawn = false;
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                drawn = cache.texture_and_uv(ui.ctx(), emoji_ref).is_some();
            });
            if drawn || frames > 600 {
                break;
            }
            next += FRAME;
            if let Some(wait) = next.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
        }
        // Frame 1 is the frame that asked for it; `frames - 1` frames showed an empty slot.
        println!(
            "first use {seq}: ready after {:.1} ms, drawn on frame {frames} (empty slot for {} frames)",
            wall.as_secs_f64() * 1000.0,
            frames - 1
        );
        walls.push(wall.as_secs_f64() * 1000.0);
        frames_all.push(frames - 1);
    }
    walls.sort_by(f64::total_cmp);
    frames_all.sort_unstable();
    println!(
        "first use summary: median {:.1} ms, max {:.1} ms; empty-slot frames median {}, max {}",
        walls[walls.len() / 2],
        walls[walls.len() - 1],
        frames_all[frames_all.len() / 2],
        frames_all[frames_all.len() - 1]
    );
}

/// Every emoji of every category painted once on one context: GPU texture bytes and CPU copies.
#[test]
#[ignore = "measurement, run with --ignored --nocapture in a release build"]
fn measure_memory_after_all_categories() {
    let atlas = leaked_atlas();
    let ctx = Context::default();
    let mut cache = EmojiTextureCache::with_atlas(atlas);
    cache.set_background_decode(Some(false));
    let started = Instant::now();
    let mut drawable = 0usize;
    for index in 0..emoji::full_count() {
        if let Some(emoji_ref) = emoji::by_index(index)
            && cache.texture_and_uv(&ctx, emoji_ref).is_some()
        {
            drawable += 1;
        }
    }
    let elapsed = started.elapsed();
    let (textures, bytes) = emoji_texture_bytes(&ctx);
    println!(
        "all categories: {drawable} emoji drawable, {textures} textures, {:.1} MiB GPU texels, \
         {} CPU decoded copies kept, total sync decode+upload {:.0} ms",
        bytes as f64 / (1024.0 * 1024.0),
        atlas.retained_tile_count(),
        elapsed.as_secs_f64() * 1000.0
    );
}
