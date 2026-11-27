//! One line of preview text with its spoiler parts hidden under the particle cover: the
//! chat-list row, the pinned bar, search results and reply headers (`Ui::Text::String::draw`
//! with `.spoiler` and `elisionLines = 1`; a preview never reveals a spoiler).

use std::ops::Range;
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Galley, Painter, Pos2, Rect};

/// One laid-out line of preview text.
#[derive(Clone)]
pub struct SpoilerLine {
    /// The line, its spoiler glyphs transparent.
    pub galley: Arc<Galley>,
    /// Where the hidden spoiler glyphs are, relative to the galley's top-left, one rectangle
    /// per run; the particle cover is drawn over them.
    pub spoilers: Vec<Rect>,
}

impl SpoilerLine {
    /// Paints the line at `pos` and the particle cover over its spoilers in `spoiler_fg`
    /// (the text palette's `spoilerFg`).
    pub fn paint(&self, painter: &Painter, pos: Pos2, fallback: Color32, spoiler_fg: Color32) {
        painter.galley(pos, Arc::clone(&self.galley), fallback);
        if self.spoilers.is_empty() {
            return;
        }
        let rects: Vec<Rect> = self
            .spoilers
            .iter()
            .map(|r| r.translate(pos.to_vec2()))
            .collect();
        crate::effects::paint_text_spoiler(painter.ctx(), painter, &rects, pos, spoiler_fg, 1.0);
    }
}

/// Lays out one line of `text`: its first `colored_len` bytes in `colored`, the rest in
/// `plain`, the byte ranges in `spoilers` hidden. With a finite `max_w` the line is cut with
/// an ellipsis that is never covered.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn layout_spoiler_line(
    painter: &Painter,
    text: &str,
    colored_len: usize,
    spoilers: &[Range<usize>],
    font: FontId,
    colored: Color32,
    plain: Color32,
    max_w: f32,
) -> SpoilerLine {
    let (job, hidden) = spoiler_line_job(text, colored_len, spoilers, font, colored, plain, max_w);
    let galley = painter.layout_job(job);
    let spoilers = if hidden.is_empty() {
        Vec::new()
    } else {
        hidden_glyph_rects(&galley, &hidden)
    };
    SpoilerLine { galley, spoilers }
}

/// The spoiler ranges of `full` that fall in `shown`, a cut of it: `shown` is `full` itself or
/// a prefix of it followed by an ellipsis, which is never hidden.
#[must_use]
pub fn spoilers_in_cut(full: &str, shown: &str, spoilers: &[Range<usize>]) -> Vec<Range<usize>> {
    let kept = if full.starts_with(shown) {
        shown.len()
    } else {
        shown
            .char_indices()
            .last()
            .map_or(0, |(at, _)| at)
            .min(full.len())
    };
    spoilers
        .iter()
        .map(|r| r.start.min(kept)..r.end.min(kept))
        .filter(|r| r.start < r.end)
        .collect()
}

/// The layout job of one line and, per char of `text`, whether it is hidden (a spoiler); the
/// second is empty when nothing is hidden.
#[must_use]
pub fn spoiler_line_job(
    text: &str,
    colored_len: usize,
    spoilers: &[Range<usize>],
    font: FontId,
    colored: Color32,
    plain: Color32,
    max_w: f32,
) -> (LayoutJob, Vec<bool>) {
    let floor = |mut at: usize| {
        at = at.min(text.len());
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        at
    };
    let split = floor(colored_len);
    let ranges: Vec<Range<usize>> = spoilers
        .iter()
        .map(|r| floor(r.start)..floor(r.end))
        .filter(|r| r.start < r.end)
        .collect();
    let mut cuts: Vec<usize> = vec![0, split, text.len()];
    cuts.extend(ranges.iter().flat_map(|r| [r.start, r.end]));
    cuts.sort_unstable();
    cuts.dedup();
    let mut job = LayoutJob::default();
    let mut hidden = Vec::new();
    let any_hidden = !ranges.is_empty();
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        if start >= end {
            continue;
        }
        let spoiler = ranges.iter().any(|r| r.start <= start && end <= r.end);
        let color = if spoiler {
            Color32::TRANSPARENT
        } else if start < split {
            colored
        } else {
            plain
        };
        if any_hidden {
            hidden.extend(std::iter::repeat_n(
                spoiler,
                text[start..end].chars().count(),
            ));
        }
        job.append(
            &text[start..end],
            0.0,
            TextFormat::simple(font.clone(), color),
        );
    }
    job.wrap.max_width = max_w.max(1.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('\u{2026}');
    (job, hidden)
}

/// Rectangles of the glyphs of the chars marked in `hidden`, one per run of neighbouring
/// glyphs. The line has no line breaks, so its glyphs are its chars in order, then the
/// elision mark of a cut line, which is past `hidden` and never covered.
#[must_use]
pub fn hidden_glyph_rects(galley: &Galley, hidden: &[bool]) -> Vec<Rect> {
    let mut rects: Vec<Rect> = Vec::new();
    let mut char_index = 0usize;
    for placed in &galley.rows {
        let row = &placed.row;
        let count = row.glyphs.len();
        let mut run: Option<Rect> = None;
        for (i, glyph) in row.glyphs.iter().enumerate() {
            let elision = galley.elided && i + 1 == count;
            let is_hidden = !elision && hidden.get(char_index).copied().unwrap_or(false);
            char_index += 1;
            if !is_hidden {
                rects.extend(run.take());
                continue;
            }
            let rect = Rect::from_min_max(
                Pos2::new(placed.pos.x + glyph.pos.x, placed.pos.y),
                Pos2::new(
                    placed.pos.x + glyph.pos.x + glyph.advance_width,
                    placed.pos.y + row.size.y,
                ),
            );
            run = Some(run.map_or(rect, |r| r.union(rect)));
        }
        rects.extend(run);
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cut_keeps_the_spoilers_before_the_ellipsis() {
        let full = "see the hidden part here";
        let spoilers = std::slice::from_ref(&(8..14));
        assert_eq!(spoilers_in_cut(full, full, spoilers), vec![8..14]);
        assert_eq!(
            spoilers_in_cut(full, "see the hid\u{2026}", spoilers),
            vec![8..11]
        );
        assert!(spoilers_in_cut(full, "see the\u{2026}", spoilers).is_empty());
    }
}
