//! Shared tile textures: one upload per context, no CPU copy kept, background decode.

use std::time::{Duration, Instant};

use egui::Context;
use tg_text::emoji;

use crate::{EmojiAtlas, EmojiTextureCache};

fn leaked_atlas() -> &'static EmojiAtlas {
    Box::leak(Box::new(EmojiAtlas::new()))
}

fn tile_textures(ctx: &Context) -> usize {
    ctx.tex_manager()
        .read()
        .allocated()
        .filter(|(_, meta)| meta.name.starts_with("tg_emoji_tile_"))
        .count()
}

#[test]
fn handles_on_one_context_share_one_upload_and_keep_no_cpu_copy() {
    let atlas = leaked_atlas();
    let ctx = Context::default();
    let grin = emoji::find_emoji("😀", 0).expect("grin").0;

    let mut first = EmojiTextureCache::with_atlas(atlas);
    let mut second = EmojiTextureCache::with_atlas(atlas);
    first.set_background_decode(Some(false));
    second.set_background_decode(Some(false));

    let (tex_a, uv_a) = first.texture_and_uv(&ctx, grin).expect("first upload");
    let (tex_b, uv_b) = second.texture_and_uv(&ctx, grin).expect("shared texture");
    assert_eq!(
        tex_a, tex_b,
        "every handle must reuse the same tile texture"
    );
    assert_eq!(uv_a, uv_b);
    assert_eq!(tile_textures(&ctx), 1, "the tile must be uploaded once");
    assert_eq!(
        atlas.retained_tile_count(),
        0,
        "the decoded CPU copy must be dropped after upload"
    );

    // A different context gets its own texture (textures belong to a renderer).
    let other_ctx = Context::default();
    let mut third = EmojiTextureCache::with_atlas(atlas);
    third.set_background_decode(Some(false));
    let (tex_c, _) = third.texture_and_uv(&other_ctx, grin).expect("other ctx");
    assert_eq!(tile_textures(&other_ctx), 1);
    let _ = tex_c;
}

#[test]
fn single_emoji_extraction_keeps_only_its_tile_until_released() {
    let atlas = leaked_atlas();
    let grin = emoji::find_emoji("😀", 0).expect("grin").0;
    assert!(atlas.emoji_rgba(grin).expect("rgba").is_some());
    assert_eq!(atlas.retained_tile_count(), 1);
    assert!(atlas.emoji_rgba(grin).expect("rgba again").is_some());
    assert_eq!(atlas.retained_tile_count(), 1);
    let tile = atlas.sprite(grin).expect("sprite").tile;
    atlas.release_tile(tile);
    assert_eq!(atlas.retained_tile_count(), 0);
}

#[test]
fn background_decode_reports_pending_then_ready_without_blocking_the_paint() {
    let atlas = leaked_atlas();
    let ctx = Context::default();
    let rocket = emoji::find_emoji("🚀", 0).expect("rocket").0;
    let mut cache = EmojiTextureCache::with_atlas(atlas);
    cache.set_background_decode(Some(true));

    let started = Instant::now();
    let first = cache.texture_and_uv(&ctx, rocket);
    let first_call = started.elapsed();
    assert!(
        first.is_none(),
        "the first call must hand the decode to a worker"
    );

    let deadline = Instant::now() + Duration::from_secs(20);
    let ready = loop {
        if let Some(found) = cache.texture_and_uv(&ctx, rocket) {
            break Some(found);
        }
        if Instant::now() > deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    let decode_wall = started.elapsed();
    assert!(ready.is_some(), "the worker must finish the tile");
    assert_eq!(tile_textures(&ctx), 1);
    assert_eq!(atlas.retained_tile_count(), 0);
    // Measured, printed for the report (`cargo test -- --nocapture`).
    println!(
        "emoji tile: paint call {:?} with background decode, decode+upload {:?} on the worker",
        first_call, decode_wall
    );

    // A second handle on the same context gets the texture at once.
    let mut other = EmojiTextureCache::with_atlas(atlas);
    other.set_background_decode(Some(true));
    assert_eq!(
        other.texture_and_uv(&ctx, rocket).map(|(id, _)| id),
        ready.map(|(id, _)| id)
    );
}

#[test]
fn concurrent_background_requests_upload_every_tile_once_and_none_stays_pending() {
    let atlas = leaked_atlas();
    let ctx = Context::default();
    let drawable: Vec<_> = (0..emoji::full_count())
        .filter(|&i| EmojiAtlas::has_index(i))
        .filter_map(emoji::by_index)
        .collect();
    let tiles: std::collections::BTreeSet<_> = drawable
        .iter()
        .filter_map(|&e| atlas.sprite(e).map(|s| s.tile))
        .collect();

    std::thread::scope(|scope| {
        for worker in 0..4 {
            let (ctx, drawable) = (ctx.clone(), &drawable);
            scope.spawn(move || {
                let mut cache = EmojiTextureCache::with_atlas(atlas);
                cache.set_background_decode(Some(true));
                let deadline = Instant::now() + Duration::from_secs(60);
                // Each thread walks the emoji in a different order until all are drawable.
                let mut pending: Vec<_> = drawable.to_vec();
                pending.rotate_left(worker * drawable.len() / 4);
                while !pending.is_empty() {
                    pending.retain(|&e| cache.texture_and_uv(&ctx, e).is_none());
                    assert!(
                        Instant::now() < deadline,
                        "{} emoji never ready",
                        pending.len()
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
            });
        }
    });
    assert_eq!(
        tile_textures(&ctx),
        tiles.len(),
        "every tile must be uploaded exactly once"
    );
    assert_eq!(atlas.retained_tile_count(), 0);
}
