//! Elided labels (TGB-3062): inline keyboard buttons, the top bar and the topic tabs cut text only
//! between graphemes, and a button label costs a handful of layouts, not one per character.

use std::time::Instant;

use egui::{Context, FontId, RawInput, Ui};
use tg_history::text_cut::grapheme_floor;

use super::truncate_single_line;
use crate::history::bars::ellipsize_to_width;
use crate::history::sections_ui::elide_tab_title;

/// Skin-tone emoji, flags and a ZWJ family: the sequences a char-by-char cut split.
const LABELS: [&str; 3] = [
    "Vote \u{1F44D}\u{1F3FD}\u{1F44D}\u{1F3FF}\u{1F44D}\u{1F3FB}\u{1F44D}\u{1F3FE} now",
    "Go \u{1F1FA}\u{1F1E6}\u{1F1EF}\u{1F1F5}\u{1F1E9}\u{1F1EA}\u{1F1EB}\u{1F1F7} trip",
    "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} plan",
];

fn with_ui(mut body: impl FnMut(&mut Ui)) {
    let ctx = Context::default();
    tg_ui::fonts::register_tdesktop_fonts(&ctx);
    let mut output = ctx.run_ui(RawInput::default(), |ui| body(ui));
    output.textures_delta.clear();
}

/// `out` is `text` cut between two graphemes, with `…` appended when cut.
fn assert_whole_graphemes(text: &str, out: &str, what: &str) {
    let kept = out.strip_suffix('…').unwrap_or(out).trim_end();
    assert!(
        text.starts_with(kept) && grapheme_floor(text, kept.len()) == kept.len(),
        "{what}: {text:?} cut to {out:?}"
    );
}

#[test]
fn elided_labels_keep_emoji_sequences_whole() {
    with_ui(|ui| {
        let font = FontId::proportional(14.0);
        let painter = ui.painter().clone();
        for text in LABELS {
            for width in (6..=220).map(|w| w as f32) {
                let button = truncate_single_line(ui, text, &font, width);
                assert_whole_graphemes(text, &button, &format!("button at {width}"));
                let (bar, _) = ellipsize_to_width(&painter, text, &font, width);
                assert_whole_graphemes(text, &bar, &format!("top bar at {width}"));
                let (tab, _) = elide_tab_title(&painter, text, &font, width);
                assert_whole_graphemes(text, &tab, &format!("tab at {width}"));
            }
        }
    });
}

#[test]
fn button_label_elision_measurement() {
    let label = "Open the full release notes for this version, with every change, fix and \
                 known issue listed in one place";
    const CALLS: usize = 400;
    with_ui(|ui| {
        let font = FontId::proportional(14.0);
        // Warm egui's galley cache the way an earlier frame would.
        let _ = truncate_single_line(ui, label, &font, 120.0);
        let started = Instant::now();
        let mut kept = 0usize;
        for _ in 0..CALLS {
            kept += truncate_single_line(ui, label, &font, 120.0).len();
        }
        let elapsed = started.elapsed();
        assert!(kept > 0);
        println!(
            "measured: {}-char button label elided to 120 px, {:.1} us per label per frame",
            label.chars().count(),
            elapsed.as_secs_f64() * 1e6 / CALLS as f64
        );
    });
}
