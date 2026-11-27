//! The binary-search elision returns what the per-prefix scans it replaced returned (moved
//! from `tg_app_info::elide`, TGB-3066), and never splits a grapheme.

use std::time::Instant;

use egui::{Color32, FontId};

use super::*;

/// The scan `statistics::sections::elide_text` did: drop one char at a time from the end.
fn linear_end(text: &str, fits: &mut dyn FnMut(&str) -> bool) -> String {
    if fits(text) {
        return text.to_owned();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let candidate: String = chars.iter().collect::<String>() + "\u{2026}";
        if fits(&candidate) {
            return candidate;
        }
    }
    "\u{2026}".to_owned()
}

/// The scan `media::overlay::ellipsize_middle` did.
fn linear_middle(text: &str, fits: &mut dyn FnMut(&str) -> bool) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut keep = chars.len();
    while keep > 0 {
        let head = keep.div_ceil(2);
        let tail = keep / 2;
        let candidate: String = chars[..head]
            .iter()
            .chain(std::iter::once(&'\u{2026}'))
            .chain(chars[chars.len() - tail..].iter())
            .collect();
        if fits(&candidate) {
            return candidate;
        }
        keep -= 1;
    }
    "\u{2026}".to_owned()
}

/// A monotone width: ASCII 1, other chars 2, the ellipsis 1.
fn width(s: &str) -> usize {
    s.chars()
        .map(|c| {
            if c == '\u{2026}' || c.is_ascii() {
                1
            } else {
                2
            }
        })
        .sum()
}

const SAMPLES: &[&str] = &[
    "",
    "a",
    "ab",
    "Hello, world",
    "Привет, мир — тест",
    "日本語のテキストです",
    "mixed ascii и кириллица 😀 emoji",
    "line one\nline two that is longer",
];

#[test]
fn end_elision_matches_the_linear_scan() {
    for text in SAMPLES {
        for max in 0..=width(text) + 2 {
            let mut fits = |s: &str| width(s) <= max;
            let expected = linear_end(text, &mut fits);
            assert_eq!(elide_end_by(text, fits), expected, "{text:?} at {max}");
        }
    }
}

#[test]
fn middle_elision_matches_the_linear_scan() {
    for text in SAMPLES {
        for max in 0..width(text) {
            let mut fits = |s: &str| width(s) <= max;
            let expected = linear_middle(text, &mut fits);
            assert_eq!(elide_middle_by(text, fits), expected, "{text:?} at {max}");
        }
    }
}

#[test]
fn laid_out_text_matches_the_linear_scan_and_costs_log_n_layouts() {
    let ctx = egui::Context::default();
    ctx.set_fonts(crate::fonts::build_tdesktop_font_definitions());
    let font = FontId::proportional(13.0);
    // A long channel post, as the statistics "recent posts" row elides it every frame.
    let post = "Weekly digest: new features, fixes and a long list of changes. ".repeat(40);
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        let painter = ui.painter();
        let layouts = std::cell::Cell::new(0usize);
        let mut fits = |s: &str| {
            layouts.set(layouts.get() + 1);
            painter
                .layout_no_wrap(s.to_owned(), font.clone(), Color32::WHITE)
                .size()
                .x
                <= 420.0
        };
        // The search runs first so that it gets no galleys cached by the scan.
        let started = Instant::now();
        let after = elide_end_by(&post, &mut fits);
        let search_time = started.elapsed();
        let search_layouts = layouts.replace(0);

        let started = Instant::now();
        let before = linear_end(&post, &mut fits);
        let linear_time = started.elapsed();
        let linear_layouts = layouts.get();

        assert_eq!(after, before);
        assert!(search_layouts <= 16, "{search_layouts} layouts");
        println!(
            "elide {} chars to 420px: per-prefix scan {linear_layouts} layouts in {linear_time:?}, \
             binary search {search_layouts} layouts in {search_time:?}",
            post.chars().count()
        );
    });
    output.textures_delta.clear();
}

/// Grapheme-aware reference scan for the end elision.
fn linear_end_graphemes(text: &str, fits: &mut dyn FnMut(&str) -> bool) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    if fits(text) {
        return text.to_owned();
    }
    let mut graphemes: Vec<&str> = text.graphemes(true).collect();
    while !graphemes.is_empty() {
        graphemes.pop();
        let candidate = graphemes.concat() + "\u{2026}";
        if fits(&candidate) {
            return candidate;
        }
    }
    "\u{2026}".to_owned()
}

/// Grapheme-aware reference scan for the middle elision.
fn linear_middle_graphemes(text: &str, fits: &mut dyn FnMut(&str) -> bool) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    let graphemes: Vec<&str> = text.graphemes(true).collect();
    let mut keep = graphemes.len();
    while keep > 0 {
        let tail = keep / 2;
        let candidate = graphemes[..keep - tail].concat()
            + "\u{2026}"
            + &graphemes[graphemes.len() - tail..].concat();
        if fits(&candidate) {
            return candidate;
        }
        keep -= 1;
    }
    "\u{2026}".to_owned()
}

const EMOJI_SAMPLES: &[&str] = &[
    "Team \u{1F44D}\u{1F3FD} rocks",
    "Go \u{1F1FA}\u{1F1E6} now",
    "fam \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} trip",
    "e\u{0301}te\u{0301} cafe\u{0301}",
];

#[test]
fn emoji_sequences_and_combining_marks_are_never_split() {
    for text in EMOJI_SAMPLES {
        for max in 0..=width(text) + 2 {
            let mut fits = |s: &str| width(s) <= max;
            let expected = linear_end_graphemes(text, &mut fits);
            assert_eq!(elide_end_by(text, fits), expected, "{text:?} at {max}");
            if max < width(text) {
                let mut fits = |s: &str| width(s) <= max;
                let expected = linear_middle_graphemes(text, &mut fits);
                assert_eq!(elide_middle_by(text, fits), expected, "{text:?} at {max}");
            }
        }
    }
    // The char-by-char copies cut "Team 👍🏽 rocks" to "Team 👍…": a different emoji.
    let fits = |s: &str| width(s) <= 8;
    assert_eq!(
        elide_end_by("Team \u{1F44D}\u{1F3FD} rocks", fits),
        "Team \u{2026}"
    );
}

#[test]
fn painter_helpers_keep_text_that_fits() {
    let ctx = egui::Context::default();
    ctx.set_fonts(crate::fonts::build_tdesktop_font_definitions());
    let font = FontId::proportional(13.0);
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        let painter = ui.painter();
        assert_eq!(elide_end(painter, "short", &font, 400.0), "short");
        assert_eq!(elide_middle(painter, "short", &font, 400.0), "short");
        let long = "a_very_long_file_name_from_a_channel_post.mp4";
        let middle = elide_middle(painter, long, &font, 120.0);
        assert!(
            middle.starts_with("a_") && middle.ends_with(".mp4"),
            "{middle}"
        );
        assert!(middle.contains('\u{2026}') && fits_width(painter, &middle, &font, 120.0));
        let end = elide_end(painter, long, &font, 120.0);
        assert!(end.ends_with('\u{2026}') && fits_width(painter, &end, &font, 120.0));
    });
    output.textures_delta.clear();
}
