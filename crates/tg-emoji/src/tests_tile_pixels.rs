//! The tiles must draw exactly like the WebP sprite sheets they replace.
//!
//! The "old path" here is the pre-tile code: decode `assets/sheet_N.webp` and sample the cell at
//! its sheet UV. The tiles are compared against it texel by texel (as the premultiplied
//! `Color32` that egui uploads) and as drawn by a bilinear sampler at the sizes the UI uses.

use egui::{Color32, ColorImage};
use tg_text::emoji;

use crate::atlas::TILE_PNG;
use crate::{
    CELL_SIZE_PX, EmojiAtlas, SHEET_COUNT, SHEET_HEIGHT_PX, SHEET_ROWS, SHEET_WIDTH_PX,
    TILE_HEIGHT_PX, TILE_WIDTH_PX,
};

/// The removed sheet decoder: the embedded WebP decoded to straight RGBA8.
fn decode_sheet_old_path(sheet: usize) -> image::RgbaImage {
    let path = format!("{}/assets/sheet_{sheet}.webp", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(&path).unwrap_or_else(|err| panic!("{path}: {err}"));
    image::load_from_memory_with_format(&bytes, image::ImageFormat::WebP)
        .unwrap_or_else(|err| panic!("{path}: {err}"))
        .to_rgba8()
}

/// The removed sheet UV of cell `index` (0.5-texel inset inside the 2048x1024 sheet).
fn sheet_uv_old_path(index: usize) -> [f32; 4] {
    let row = (index >> 5) & 0x0F;
    let col = index & 0x1F;
    let px = (col as u32) * CELL_SIZE_PX;
    let py = (row as u32) * CELL_SIZE_PX;
    let half_u = 0.5 / (SHEET_WIDTH_PX as f32);
    let half_v = 0.5 / (SHEET_HEIGHT_PX as f32);
    [
        (px as f32) / (SHEET_WIDTH_PX as f32) + half_u,
        (py as f32) / (SHEET_HEIGHT_PX as f32) + half_v,
        ((px + CELL_SIZE_PX) as f32) / (SHEET_WIDTH_PX as f32) - half_u,
        ((py + CELL_SIZE_PX) as f32) / (SHEET_HEIGHT_PX as f32) - half_v,
    ]
}

fn premultiplied(px: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(px[0], px[1], px[2], px[3])
}

#[test]
fn every_tile_texel_matches_the_sheet_it_was_cut_from() {
    let atlas = EmojiAtlas::new();
    let mut compared_tiles = 0usize;
    let mut straight_rgb_rewrites = 0usize;
    for sheet in 0..SHEET_COUNT {
        let old = decode_sheet_old_path(sheet);
        assert_eq!(old.dimensions(), (SHEET_WIDTH_PX, SHEET_HEIGHT_PX));
        for row in 0..SHEET_ROWS {
            let tile_index = sheet * SHEET_ROWS + row;
            let y0 = row as u32 * CELL_SIZE_PX;
            if TILE_PNG[tile_index].is_empty() {
                for y in y0..y0 + CELL_SIZE_PX {
                    for x in 0..SHEET_WIDTH_PX {
                        assert_eq!(old.get_pixel(x, y).0[3], 0, "empty tile {tile_index}");
                    }
                }
                continue;
            }
            let tile = atlas
                .decode_tile_transient(tile_index)
                .unwrap_or_else(|err| panic!("tile {tile_index}: {err}"));
            assert_eq!((tile.width, tile.height), (TILE_WIDTH_PX, TILE_HEIGHT_PX));
            for y in 0..TILE_HEIGHT_PX {
                for x in 0..TILE_WIDTH_PX {
                    let before = old.get_pixel(x, y0 + y).0;
                    let i = ((y * TILE_WIDTH_PX + x) * 4) as usize;
                    let after = [
                        tile.rgba[i],
                        tile.rgba[i + 1],
                        tile.rgba[i + 2],
                        tile.rgba[i + 3],
                    ];
                    assert_eq!(
                        premultiplied(before),
                        premultiplied(after),
                        "tile {tile_index} texel ({x}, {y}): {before:?} vs {after:?}"
                    );
                    assert_eq!(before[3], after[3], "alpha of tile {tile_index} ({x}, {y})");
                    if before != after {
                        straight_rgb_rewrites += 1;
                    }
                }
            }
            compared_tiles += 1;
        }
    }
    println!(
        "{compared_tiles} tiles identical to the sheets as uploaded; {straight_rgb_rewrites} \
         texels had hidden straight-alpha colour normalised"
    );
    assert!(
        compared_tiles >= 100,
        "only {compared_tiles} tiles compared"
    );
}

/// Bilinear, clamp-to-edge sample in premultiplied space, as `TextureOptions::LINEAR` does.
fn sample(image: &ColorImage, u: f32, v: f32) -> [f32; 4] {
    let [w, h] = image.size;
    let x = u * w as f32 - 0.5;
    let y = v * h as f32 - 0.5;
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let texel = |xi: f32, yi: f32| {
        let xi = (xi.max(0.0) as usize).min(w - 1);
        let yi = (yi.max(0.0) as usize).min(h - 1);
        image.pixels[yi * w + xi].to_array().map(f32::from)
    };
    let (a, b) = (texel(x0, y0), texel(x0 + 1.0, y0));
    let (c, d) = (texel(x0, y0 + 1.0), texel(x0 + 1.0, y0 + 1.0));
    std::array::from_fn(|k| {
        let top = a[k] + (b[k] - a[k]) * fx;
        let bottom = c[k] + (d[k] - c[k]) * fx;
        top + (bottom - top) * fy
    })
}

/// Draws the cell at `uv` into a `side`-pixel square whose top-left is at sub-pixel `offset`.
fn draw(image: &ColorImage, uv: [f32; 4], side: f32, offset: f32) -> Vec<[u8; 4]> {
    let pixels = (side + offset).ceil() as usize;
    let mut out = Vec::with_capacity(pixels * pixels);
    for py in 0..pixels {
        for px in 0..pixels {
            let tx = ((px as f32 + 0.5 - offset) / side).clamp(0.0, 1.0);
            let ty = ((py as f32 + 0.5 - offset) / side).clamp(0.0, 1.0);
            let u = uv[0] + (uv[2] - uv[0]) * tx;
            let v = uv[1] + (uv[3] - uv[1]) * ty;
            out.push(sample(image, u, v).map(|c| c.round() as u8));
        }
    }
    out
}

#[test]
fn sampled_emoji_draw_the_same_from_tiles_as_from_sheets() {
    let atlas = EmojiAtlas::new();
    let sheets: Vec<ColorImage> = (0..SHEET_COUNT)
        .map(|sheet| {
            let old = decode_sheet_old_path(sheet);
            ColorImage::from_rgba_unmultiplied(
                [old.width() as usize, old.height() as usize],
                old.as_raw(),
            )
        })
        .collect();

    // Every 23rd drawable emoji (~150, all sheets, all columns/rows) plus common ones.
    let mut samples: Vec<usize> = (0..emoji::full_count())
        .filter(|&i| EmojiAtlas::has_index(i))
        .step_by(23)
        .collect();
    for seq in ["😀", "😂", "❤️", "🔥", "👍🏽", "🚀"] {
        samples.push(emoji::find_emoji(seq, 0).expect("common emoji").0.index());
    }

    let mut drawn_pixels = 0usize;
    let mut max_diff = 0u8;
    let mut differing = 0usize;
    for &index in &samples {
        let sprite = atlas.sprite_by_index(index).expect("drawable sample");
        let tile = atlas
            .decode_tile_transient(sprite.tile)
            .expect("tile")
            .to_color_image();
        let sheet = &sheets[sprite.sheet];
        for side in [14.0, 16.0, 18.0, 21.0, 22.0, 27.5, 36.0, 48.0, 64.0, 100.0] {
            for offset in [0.0, 0.25, 0.5] {
                let before = draw(sheet, sheet_uv_old_path(index), side, offset);
                let after = draw(&tile, sprite.uv, side, offset);
                for (a, b) in before.iter().zip(&after) {
                    let diff = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max();
                    let diff = diff.unwrap_or(0);
                    max_diff = max_diff.max(diff);
                    differing += usize::from(diff > 0);
                }
                drawn_pixels += before.len();
            }
        }
    }
    println!(
        "{} emoji drawn at 10 sizes x 3 offsets: {drawn_pixels} pixels, {differing} differ, \
         max channel difference {max_diff}",
        samples.len()
    );
    // f32 UVs in a 64-texel-high tile vs a 1024-texel-high sheet may round a weight differently.
    assert!(max_diff <= 1, "max channel difference {max_diff}");
    assert!(
        differing * 1000 <= drawn_pixels,
        "{differing} of {drawn_pixels} pixels differ"
    );
}
