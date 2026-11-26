//! Micro-measurement of palette colour lookups by name (TGB-2893, package F).
//!
//! Run with `cargo test --release -p tg-theme --test name_lookup_bench -- --ignored --nocapture`.

use std::hint::black_box;
use std::time::Instant;

use tg_theme::Palette;

/// Names a chat-list row and a message bubble look up every frame, plus late table entries.
const HOT_NAMES: &[&str] = &[
    "windowBg",
    "windowFg",
    "windowSubTextFg",
    "windowBgActive",
    "dialogsBg",
    "dialogsBgOver",
    "dialogsBgActive",
    "dialogsNameFg",
    "dialogsTextFg",
    "dialogsDateFg",
    "dialogsUnreadBg",
    "dialogsUnreadBgMuted",
    "msgInBg",
    "msgOutBg",
    "msgInDateFg",
    "historyTextInFg",
    "historyLinkInFg",
    "sideBarBg",
    "sideBarTextFg",
    "boxTextFg",
];

#[test]
#[ignore = "timing; run explicitly with --ignored --nocapture"]
fn palette_name_lookup_timing() {
    let palette = Palette::default();
    let mut names: Vec<&str> = HOT_NAMES.to_vec();
    names.extend(Palette::ROWS.iter().rev().take(20).map(|r| r.name));
    names.push("noSuchColourName");
    const ROUNDS: usize = 200_000;
    let start = Instant::now();
    let mut acc = 0u64;
    for _ in 0..ROUNDS {
        for name in &names {
            if let Some(c) = palette.color_by_name(black_box(name)) {
                acc = acc.wrapping_add(u64::from(c.r));
            }
        }
    }
    let elapsed = start.elapsed();
    let lookups = ROUNDS * names.len();
    println!(
        "palette name lookups: {lookups} in {elapsed:?} = {:.1} ns/lookup (acc {acc})",
        elapsed.as_nanos() as f64 / lookups as f64
    );
}

#[test]
#[ignore = "timing; run explicitly with --ignored --nocapture"]
fn palette_typed_lookup_timing() {
    let palette = Palette::default();
    let mut colors: Vec<tg_theme::PaletteColor> = HOT_NAMES
        .iter()
        .filter_map(|name| Palette::color_index(name))
        .collect();
    colors.extend(
        Palette::ROWS
            .iter()
            .rev()
            .take(20)
            .filter_map(|r| Palette::color_index(r.name)),
    );
    const ROUNDS: usize = 200_000;
    let start = Instant::now();
    let mut acc = 0u64;
    for _ in 0..ROUNDS {
        for color in &colors {
            acc = acc.wrapping_add(u64::from(palette.at(black_box(*color)).r));
        }
    }
    let elapsed = start.elapsed();
    let lookups = ROUNDS * colors.len();
    println!(
        "palette typed lookups: {lookups} in {elapsed:?} = {:.1} ns/lookup (acc {acc})",
        elapsed.as_nanos() as f64 / lookups as f64
    );
}
