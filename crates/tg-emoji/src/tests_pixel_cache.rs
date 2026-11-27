//! TGB-3102: the scaled-emoji cache evicts the least recently used images instead of clearing
//! itself, and decodes and scales with its lock released.

use std::cell::Cell;
use std::sync::{Arc, Mutex};

use egui::Color32;

use crate::atlas::EmojiTileImage;
use crate::text_glyphs::{PixelCache, ScaledImages, cached_scaled_pixels};

fn image(tag: u8) -> Arc<[Color32]> {
    vec![Color32::from_gray(tag); 4].into()
}

fn tile() -> Arc<EmojiTileImage> {
    Arc::new(EmojiTileImage {
        width: 2,
        height: 2,
        rgba: vec![255; 16],
    })
}

#[test]
fn a_full_cache_drops_only_the_least_recently_used_quarter() {
    let mut images = ScaledImages::with_capacity(16);
    for i in 0..16 {
        images.insert((i, 20), image(1));
    }
    // The emoji on screen are used again before the cache overflows.
    for i in 8..16 {
        assert!(images.get((i, 20)).is_some());
    }
    images.insert((100, 20), image(2));
    assert_eq!(
        images.len(),
        13,
        "a quarter (4) goes, the new image comes in"
    );
    for i in 0..4 {
        assert!(images.get((i, 20)).is_none(), "{i} was used longest ago");
    }
    for i in (4..16).chain([100]) {
        assert!(images.get((i, 20)).is_some(), "{i} is kept");
    }
}

#[test]
fn overflowing_the_cache_causes_no_burst_of_rescales() {
    let cache = Mutex::new(PixelCache::default());
    let rescales = Cell::new(0usize);
    let request = |emoji: usize| {
        cached_scaled_pixels(
            &cache,
            (emoji, 22),
            0,
            || Some(tile()),
            |_| {
                rescales.set(rescales.get() + 1);
                Some(image(3))
            },
        )
        .expect("scaled")
    };
    let capacity = 4096;
    for emoji in 0..capacity {
        request(emoji);
    }
    assert_eq!(rescales.get(), capacity);
    // A screen of 300 emoji is laid out again, then one new emoji fills the cache over.
    for emoji in capacity - 300..capacity {
        request(emoji);
    }
    request(capacity);
    assert_eq!(rescales.get(), capacity + 1);
    for emoji in capacity - 300..=capacity {
        request(emoji);
    }
    assert_eq!(
        rescales.get(),
        capacity + 1,
        "the emoji on screen stay cached (the old clear-all rescaled all 300)"
    );
}

#[test]
fn tile_decode_and_scaling_run_with_the_cache_unlocked() {
    let cache = Mutex::new(PixelCache::default());
    let decoded = Cell::new(false);
    let pixels = cached_scaled_pixels(
        &cache,
        (7, 18),
        3,
        || {
            assert!(cache.try_lock().is_ok(), "the decode runs unlocked");
            decoded.set(true);
            Some(tile())
        },
        |_| {
            assert!(cache.try_lock().is_ok(), "the scaling runs unlocked");
            Some(image(9))
        },
    );
    assert!(pixels.is_some());
    assert!(decoded.get());
    // The tile is kept: another emoji from the same tile is not decoded again.
    let again = cached_scaled_pixels(
        &cache,
        (8, 18),
        3,
        || panic!("the kept tile is reused"),
        |_| Some(image(10)),
    );
    assert!(again.is_some());
}
