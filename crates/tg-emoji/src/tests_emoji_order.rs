//! The tdesktop sheets must line up with `tg-text`'s emoji index order (TGB-3010).
//!
//! tdesktop's codegen draws cell `i` of `emoji_1..8.webp` for the `i`-th emoji it generates from
//! `Telegram/lib_ui/emoji.txt`; `tg-text`'s `build.rs` ports that codegen over the same file. These
//! tests pin the input, the index -> codepoint table, the cell count and a few colour signatures, so
//! an off-by-one or a changed table cannot ship silently.

use tg_text::emoji;

use crate::{CELL_SIZE_PX, EMOJIS_PER_SHEET, EmojiAtlas, SHEET_COUNT};

fn fnv1a(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

#[test]
fn emoji_table_input_is_tdesktops_emoji_txt() {
    // tdesktop c2d0ad9 (7.4.5) `Telegram/lib_ui/emoji.txt`, CR removed: the codegen input of the
    // shipped `emoji_*.webp`.
    let text = include_str!("../../tg-text/assets/emoji.txt").replace('\r', "");
    assert_eq!(fnv1a(FNV_OFFSET, text.as_bytes()), 0x9a00_eeca_8ea5_374c);
}

#[test]
fn index_to_codepoints_table_is_unchanged() {
    let count = emoji::full_count();
    assert_eq!(count, 3900);
    let mut hash = FNV_OFFSET;
    for index in 0..count {
        let emoji_ref = emoji::by_index(index).expect("index below full_count");
        assert_eq!(emoji_ref.index(), index);
        hash = fnv1a(hash, emoji_ref.text().as_bytes());
        hash = fnv1a(hash, &[0]);
    }
    assert_eq!(hash, INDEX_TABLE_FNV, "got {hash:#018x}");
}

/// FNV-1a over every emoji's UTF-8 text in index order (NUL separated), taken with the Noto
/// sheets before the switch.
const INDEX_TABLE_FNV: u64 = 0xb5d6_1abb_9a28_fd0f;

#[test]
fn every_index_has_a_cell_and_nothing_follows_the_table() {
    let count = emoji::full_count();
    let missing: Vec<usize> = (0..count).filter(|&i| !EmojiAtlas::has_index(i)).collect();
    assert!(missing.is_empty(), "indices without pixels: {missing:?}");
    let extra: Vec<usize> = (count..SHEET_COUNT * EMOJIS_PER_SHEET)
        .filter(|&i| EmojiAtlas::has_index(i))
        .collect();
    assert!(extra.is_empty(), "cells past the table: {extra:?}");
}

/// Mean straight RGB of the opaque pixels of `seq`'s cell (top half, bottom half).
fn mean_rgb(seq: &str) -> ([f32; 3], [f32; 3]) {
    let atlas = EmojiAtlas::new();
    let emoji_ref = emoji::find_emoji(seq, 0).expect("known emoji").0;
    let sprite = atlas.sprite(emoji_ref).expect("sprite");
    assert!(!sprite.is_fallback, "{seq} must have its own cell");
    let rgba = atlas.emoji_rgba(emoji_ref).expect("decode").expect("cell");
    let mut sums = [[0f32; 4]; 2];
    for (i, px) in rgba.as_chunks::<4>().0.iter().enumerate() {
        if px[3] < 200 {
            continue;
        }
        let half = usize::from(i / CELL_SIZE_PX as usize >= CELL_SIZE_PX as usize / 2);
        for c in 0..3 {
            sums[half][c] += f32::from(px[c]);
        }
        sums[half][3] += 1.0;
    }
    let mean = |s: [f32; 4]| [s[0] / s[3], s[1] / s[3], s[2] / s[3]];
    (mean(sums[0]), mean(sums[1]))
}

fn whole(seq: &str) -> [f32; 3] {
    let (top, bottom) = mean_rgb(seq);
    [0, 1, 2].map(|c| (top[c] + bottom[c]) / 2.0)
}

#[test]
fn neighbouring_cells_have_the_expected_colours() {
    // Adjacent table entries with distinct colours: a shift by one cell fails here.
    let [r, g, b] = whole("❤️");
    assert!(
        r > 150.0 && r > g * 2.0 && r > b * 2.0,
        "red heart {r} {g} {b}"
    );
    let [r, g, b] = whole("💚");
    assert!(g > r * 1.3 && g > b * 1.3, "green heart {r} {g} {b}");
    let [r, g, b] = whole("💙");
    assert!(b > r * 1.5 && b > g, "blue heart {r} {g} {b}");
    let [r, g, b] = whole("😀");
    assert!(
        r > 180.0 && g > 120.0 && b < g * 0.6,
        "yellow face {r} {g} {b}"
    );
    // Flags sit on the last sheet.
    let (top, bottom) = mean_rgb("🇺🇦");
    assert!(top[2] > top[0] * 1.5, "Ukraine top must be blue {top:?}");
    assert!(
        bottom[0] > bottom[2] * 1.5 && bottom[1] > bottom[2] * 1.5,
        "Ukraine bottom must be yellow {bottom:?}"
    );
}
