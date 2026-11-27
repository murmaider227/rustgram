//! Single-line text elision with `…` (`Qt::ElideRight` / `Qt::ElideMiddle` on a measured
//! width), shared by the info, box, statistics, media viewer, calls and export screens.
//!
//! Callers give a `fits` test (usually "the one-line layout is at most `max_width` wide"). The
//! cut is found by binary search over grapheme boundaries, so a string of `n` graphemes costs
//! `O(log n)` layouts instead of one layout per prefix. `fits` must not shrink as the candidate
//! grows (true for laid-out widths), which gives the same cut a linear scan finds.
//!
//! The cut never splits a grapheme: tdesktop elides whole emoji (`EmojiBlock`) and graphemes
//! (`QTextEngine::elidedText`), so "👍🏽" is never left as "👍" (another emoji) and a flag never
//! as a lone regional indicator.

use egui::{Color32, FontId, Painter};
use unicode_segmentation::UnicodeSegmentation;

/// The ellipsis elided text ends (or is split) with.
pub const ELLIPSIS: char = '\u{2026}';

/// Largest `k` in `0..=max` with `fits(k)`, taking `fits(0)` as true and `fits` monotone.
fn last_fitting(max: usize, mut fits: impl FnMut(usize) -> bool) -> usize {
    let (mut lo, mut hi) = (0, max);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// Byte offsets where each grapheme of `text` starts, followed by `text.len()`.
fn grapheme_bounds(text: &str) -> Vec<usize> {
    let mut bounds: Vec<usize> = text.grapheme_indices(true).map(|(at, _)| at).collect();
    bounds.push(text.len());
    bounds
}

/// `text` if it fits, otherwise its longest whole-grapheme prefix followed by `…` that fits,
/// or a lone `…` when no prefix does.
pub fn elide_end_by(text: &str, mut fits: impl FnMut(&str) -> bool) -> String {
    if fits(text) {
        return text.to_owned();
    }
    let bounds = grapheme_bounds(text);
    let graphemes = bounds.len() - 1;
    let mut candidate = String::with_capacity(text.len() + ELLIPSIS.len_utf8());
    let build = |keep: usize, out: &mut String| {
        out.clear();
        out.push_str(&text[..bounds[keep]]);
        out.push(ELLIPSIS);
    };
    // The whole text does not fit, so at most `graphemes - 1` are kept.
    let keep = last_fitting(graphemes.saturating_sub(1), |keep| {
        build(keep, &mut candidate);
        fits(&candidate)
    });
    build(keep, &mut candidate);
    candidate
}

/// `text` with its middle replaced by `…` so it fits: the most graphemes kept, the extra one at
/// the head (`ceil(k / 2)` from the start, `floor(k / 2)` from the end), or a lone `…`.
///
/// Callers check first that `text` itself does not fit.
pub fn elide_middle_by(text: &str, mut fits: impl FnMut(&str) -> bool) -> String {
    let bounds = grapheme_bounds(text);
    let graphemes = bounds.len() - 1;
    let build = |keep: usize, out: &mut String| {
        let tail = keep / 2;
        out.clear();
        out.push_str(&text[..bounds[keep - tail]]);
        out.push(ELLIPSIS);
        out.push_str(&text[bounds[graphemes - tail]..]);
    };
    let mut candidate = String::with_capacity(text.len() + ELLIPSIS.len_utf8());
    let keep = last_fitting(graphemes, |keep| {
        build(keep, &mut candidate);
        fits(&candidate)
    });
    build(keep, &mut candidate);
    candidate
}

/// Whether `text` laid out on one line in `font` is at most `max_width` wide.
pub fn fits_width(painter: &Painter, text: &str, font: &FontId, max_width: f32) -> bool {
    painter
        .layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x
        <= max_width
}

/// [`elide_end_by`] on the one-line width of `text` in `font`.
pub fn elide_end(painter: &Painter, text: &str, font: &FontId, max_width: f32) -> String {
    elide_end_by(text, |s| fits_width(painter, s, font, max_width))
}

/// [`elide_middle_by`] on the one-line width of `text` in `font`; `text` itself when it fits.
pub fn elide_middle(painter: &Painter, text: &str, font: &FontId, max_width: f32) -> String {
    let fits = |s: &str| fits_width(painter, s, font, max_width);
    if fits(text) {
        return text.to_owned();
    }
    elide_middle_by(text, fits)
}

#[cfg(test)]
#[path = "elide_tests.rs"]
#[cfg(test)]
mod tests;
