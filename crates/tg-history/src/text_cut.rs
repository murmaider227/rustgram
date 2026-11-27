//! Where elided text may be cut: only between grapheme clusters.
//!
//! tdesktop never splits an emoji (each is one `EmojiBlock` of `Ui::Text::String`) or a
//! grapheme (`QTextEngine::elidedText` steps back to a grapheme boundary). Cutting at `char`
//! boundaries instead turns "👍🏽" into "👍" (another emoji), a flag into a lone regional
//! indicator, and drops combining marks from their letter.

use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

/// The grapheme cluster boundary at or before byte `idx` of `text`.
#[must_use]
pub fn grapheme_floor(text: &str, idx: usize) -> usize {
    let mut idx = idx.min(text.len());
    while !text.is_char_boundary(idx) {
        idx -= 1;
    }
    let mut cursor = GraphemeCursor::new(idx, text.len(), true);
    match cursor.is_boundary(text, 0) {
        Ok(true) | Err(_) => idx,
        Ok(false) => cursor.prev_boundary(text, 0).ok().flatten().unwrap_or(0),
    }
}

/// The grapheme cluster boundary at or after byte `idx` of `text`.
#[must_use]
pub fn grapheme_ceil(text: &str, idx: usize) -> usize {
    let mut idx = idx.min(text.len());
    while !text.is_char_boundary(idx) {
        idx += 1;
    }
    let mut cursor = GraphemeCursor::new(idx, text.len(), true);
    match cursor.is_boundary(text, 0) {
        Ok(true) | Err(_) => idx,
        Ok(false) => cursor
            .next_boundary(text, 0)
            .ok()
            .flatten()
            .unwrap_or(text.len()),
    }
}

/// Byte length of the longest prefix of `text` that ends between grapheme clusters and for
/// which `fits` holds (`0` when none does).
///
/// `fits` must hold for every prefix of a prefix it holds for (a wider text never fits where a
/// narrower one did not), so the prefixes are bisected: about `log2(graphemes)` calls.
#[must_use]
pub fn longest_fitting_prefix(text: &str, mut fits: impl FnMut(&str) -> bool) -> usize {
    let ends: Vec<usize> = text
        .grapheme_indices(true)
        .map(|(start, grapheme)| start + grapheme.len())
        .collect();
    // `kept` graphemes fit; `kept + 1 ..= upper` are not known to.
    let mut kept = 0usize;
    let mut upper = ends.len();
    while kept < upper {
        let mid = kept + (upper - kept).div_ceil(2);
        if fits(&text[..ends[mid - 1]]) {
            kept = mid;
        } else {
            upper = mid - 1;
        }
    }
    kept.checked_sub(1).map_or(0, |last| ends[last])
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLES: [&str; 4] = [
        "ab\u{1F44D}\u{1F3FD}cd",                       // thumbs up + skin tone
        "x\u{1F1FA}\u{1F1E6}\u{1F1FA}\u{1F1E6}y",       // two flags
        "e\u{301}e\u{301}e\u{301}",                     // e + combining acute
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}z", // family (ZWJ sequence)
    ];

    fn is_grapheme_boundary(text: &str, idx: usize) -> bool {
        idx == 0 || idx == text.len() || text.grapheme_indices(true).any(|(i, _)| i == idx)
    }

    #[test]
    fn floor_and_ceil_land_on_grapheme_boundaries() {
        for text in SAMPLES {
            for idx in 0..=text.len() + 2 {
                let floor = grapheme_floor(text, idx);
                let ceil = grapheme_ceil(text, idx);
                assert!(floor <= idx.min(text.len()) && is_grapheme_boundary(text, floor));
                assert!(ceil >= idx.min(text.len()) && is_grapheme_boundary(text, ceil));
                if is_grapheme_boundary(text, idx.min(text.len())) {
                    assert_eq!((floor, ceil), (idx.min(text.len()), idx.min(text.len())));
                }
            }
        }
    }

    #[test]
    fn longest_fitting_prefix_keeps_whole_graphemes() {
        // One width unit per `char`: a char-by-char cut would keep half of each sequence.
        for text in SAMPLES {
            for budget in 0..=text.chars().count() {
                let end = longest_fitting_prefix(text, |p| p.chars().count() <= budget);
                assert!(is_grapheme_boundary(text, end), "{text:?} budget {budget}");
                assert!(text[..end].chars().count() <= budget);
                let next = grapheme_ceil(text, end + 1);
                assert!(end == text.len() || text[..next].chars().count() > budget);
            }
        }
        assert_eq!(longest_fitting_prefix("", |_| true), 0);
        assert_eq!(longest_fitting_prefix("abc", |_| false), 0);
        assert_eq!(longest_fitting_prefix("abc", |_| true), 3);
    }
}
