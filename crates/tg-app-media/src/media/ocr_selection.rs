//! Interactive OCR text selection and coordinate mapping for [`crate::media::overlay::MediaOverlayWidget`].
//!
//! Port origin: `Telegram/SourceFiles/media/view/media_view_recognition_selection.h` and
//! `Telegram/SourceFiles/media/view/media_view_recognition_selection.cpp`.

use egui::{Pos2, Rect, Vec2};
use tg_platform::ocr::{OcrLine, OcrRect, OcrResult};

use crate::media::zoom::normalize_rotation;

/// Character cursor inside a recognized [`OcrResult`] (`RecognitionPosition`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OcrPosition {
    /// Index into [`OcrResult::lines`].
    pub line: usize,
    /// Character offset (`0..=line.text.chars().count()`).
    pub char_index: usize,
}

/// Highlighted slice of one recognized text line (`RecognitionSpan`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OcrSpan {
    /// Index into [`OcrResult::lines`].
    pub line: usize,
    /// Inclusive start character index.
    pub from: usize,
    /// Exclusive end character index.
    pub till: usize,
    /// `true` when this span begins a new visual reading-order row.
    pub row_start: bool,
}

struct ReadingOrder {
    order: Vec<usize>,
    row: Vec<usize>,
}

/// Computes visual top-to-bottom, left-to-right reading order across OCR lines
/// (exact port of `ComputeReadingOrder` in `media_view_recognition_selection.cpp:21-57`).
fn compute_reading_order(lines: &[OcrLine]) -> ReadingOrder {
    let count = lines.len();
    let mut order: Vec<usize> = (0..count).collect();
    let mut row = vec![0usize; count];

    let center_y = |i: usize| lines[i].rect.y + lines[i].rect.height * 0.5;
    order.sort_by(|&a, &b| center_y(a).total_cmp(&center_y(b)));

    let mut rows = 0usize;
    for k in 1..count {
        let prev = order[k - 1];
        let cur = order[k];
        let threshold = lines[prev].rect.height.max(lines[cur].rect.height) * 0.5;
        if center_y(cur) - center_y(prev) > threshold {
            rows += 1;
        }
        row[cur] = rows;
    }

    order.sort_by(|&a, &b| {
        row[a]
            .cmp(&row[b])
            .then_with(|| lines[a].rect.x.total_cmp(&lines[b].rect.x))
    });

    ReadingOrder { order, row }
}

/// Extracts full recognized text in visual reading order (`RecognizedText`).
#[must_use]
pub fn recognized_text_in_reading_order(result: &OcrResult) -> String {
    if result.lines.is_empty() {
        return result.text.clone();
    }
    let reading = compute_reading_order(&result.lines);
    let mut out = String::new();
    let mut last_row: Option<usize> = None;
    for idx in reading.order {
        let text = &result.lines[idx].text;
        if text.is_empty() {
            continue;
        }
        if !out.is_empty() {
            if last_row == Some(reading.row[idx]) {
                out.push(' ');
            } else {
                out.push('\n');
            }
        }
        out.push_str(text);
        last_row = Some(reading.row[idx]);
    }
    out
}

/// Maps an image-space rectangle (`0..coord_w`, `0..coord_h`) into screen space inside
/// `content_rect`, accounting for clockwise rotation (`0/90/180/270`) and flips.
#[must_use]
pub fn image_rect_to_screen(
    rect: OcrRect,
    coord_size: Vec2,
    content_rect: Rect,
    rotation: i32,
    flip_h: bool,
    flip_v: bool,
) -> Rect {
    if coord_size.x <= 0.0 || coord_size.y <= 0.0 {
        return Rect::NOTHING;
    }
    let corners = [
        Pos2::new(rect.x, rect.y),
        Pos2::new(rect.right(), rect.y),
        Pos2::new(rect.right(), rect.bottom()),
        Pos2::new(rect.x, rect.bottom()),
    ];
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for pt in corners {
        let screen = image_point_to_screen(pt, coord_size, content_rect, rotation, flip_h, flip_v);
        min_x = min_x.min(screen.x);
        min_y = min_y.min(screen.y);
        max_x = max_x.max(screen.x);
        max_y = max_y.max(screen.y);
    }
    Rect::from_min_max(Pos2::new(min_x, min_y), Pos2::new(max_x, max_y))
}

/// Maps a point in unrotated source image coordinates into screen coordinates.
#[must_use]
pub fn image_point_to_screen(
    pt: Pos2,
    coord_size: Vec2,
    content_rect: Rect,
    rotation: i32,
    flip_h: bool,
    flip_v: bool,
) -> Pos2 {
    let nx = (pt.x / coord_size.x).clamp(0.0, 1.0);
    let ny = (pt.y / coord_size.y).clamp(0.0, 1.0);

    // Apply clockwise rotation to normalized coordinates.
    let (mut rx, mut ry) = match normalize_rotation(rotation) {
        90 => (1.0 - ny, nx),
        180 => (1.0 - nx, 1.0 - ny),
        270 => (ny, 1.0 - nx),
        _ => (nx, ny),
    };
    if flip_h {
        rx = 1.0 - rx;
    }
    if flip_v {
        ry = 1.0 - ry;
    }
    Pos2::new(
        content_rect.min.x + rx * content_rect.width(),
        content_rect.min.y + ry * content_rect.height(),
    )
}

/// Maps a screen pointer position back into unrotated source image pixel coordinates.
#[must_use]
pub fn screen_point_to_image(
    pos: Pos2,
    coord_size: Vec2,
    content_rect: Rect,
    rotation: i32,
    flip_h: bool,
    flip_v: bool,
) -> Pos2 {
    if content_rect.width() <= 0.0 || content_rect.height() <= 0.0 {
        return Pos2::ZERO;
    }
    let mut rx = (pos.x - content_rect.min.x) / content_rect.width();
    let mut ry = (pos.y - content_rect.min.y) / content_rect.height();
    if flip_h {
        rx = 1.0 - rx;
    }
    if flip_v {
        ry = 1.0 - ry;
    }
    let (nx, ny) = match normalize_rotation(rotation) {
        90 => (ry, 1.0 - rx),
        180 => (1.0 - rx, 1.0 - ry),
        270 => (1.0 - ry, rx),
        _ => (rx, ry),
    };
    Pos2::new(nx * coord_size.x, ny * coord_size.y)
}

/// Drag-selection state machine over a recognized [`OcrResult`] (`Media::View::RecognitionSelection`).
#[derive(Debug, Clone, Default)]
pub struct OcrSelection {
    anchor: Option<OcrPosition>,
    focus: Option<OcrPosition>,
    selecting: bool,
    dragged: bool,
}

impl OcrSelection {
    /// Starts a drag selection at `pos`.
    pub fn start(&mut self, pos: OcrPosition) {
        self.anchor = Some(pos);
        self.focus = Some(pos);
        self.selecting = true;
        self.dragged = false;
    }

    /// Updates the selection focus while dragging.
    pub fn update_focus(&mut self, pos: OcrPosition) -> bool {
        if self.focus != Some(pos) {
            self.focus = Some(pos);
            self.dragged = true;
            true
        } else {
            false
        }
    }

    /// Finishes the active drag gesture (keeping any non-empty selection).
    pub fn finish(&mut self) {
        self.selecting = false;
    }

    /// Returns `true` while a pointer drag selection is in progress.
    #[must_use]
    pub const fn is_selecting(&self) -> bool {
        self.selecting
    }

    /// Selects every recognized character across all lines in reading order.
    pub fn select_all(&mut self, result: &OcrResult) {
        if result.lines.is_empty() {
            self.clear();
            return;
        }
        let reading = compute_reading_order(&result.lines);
        if let (Some(&first), Some(&last)) = (reading.order.first(), reading.order.last()) {
            let last_len = result.lines[last].text.chars().count();
            self.anchor = Some(OcrPosition {
                line: first,
                char_index: 0,
            });
            self.focus = Some(OcrPosition {
                line: last,
                char_index: last_len,
            });
            self.selecting = false;
            self.dragged = true;
        }
    }

    /// Clears the selection; returns `true` if a selection was present.
    pub fn clear(&mut self) -> bool {
        self.selecting = false;
        self.dragged = false;
        let had = self.anchor.is_some() || self.focus.is_some();
        self.anchor = None;
        self.focus = None;
        had
    }

    /// Returns `true` when at least one character is selected.
    #[must_use]
    pub fn has_selection(&self, result: &OcrResult) -> bool {
        !self.spans(result).is_empty()
    }

    /// Hit-tests `screen_pos` against recognized lines in `result`.
    #[must_use]
    pub fn position_at(
        result: &OcrResult,
        screen_pos: Pos2,
        content_rect: Rect,
        rotation: i32,
        flip_h: bool,
        flip_v: bool,
        allow_outside: bool,
    ) -> Option<OcrPosition> {
        if result.lines.is_empty() || result.coordinate_width == 0 || result.coordinate_height == 0
        {
            return None;
        }
        let coord_size = Vec2::new(
            result.coordinate_width as f32,
            result.coordinate_height as f32,
        );
        let img_pos = screen_point_to_image(
            screen_pos,
            coord_size,
            content_rect,
            rotation,
            flip_h,
            flip_v,
        );

        let mut found: Option<usize> = None;
        let mut best_dy = f32::INFINITY;
        let mut best_dx = f32::INFINITY;

        for (idx, line) in result.lines.iter().enumerate() {
            let r = line.rect;
            if allow_outside {
                let dy = if img_pos.y < r.y {
                    r.y - img_pos.y
                } else if img_pos.y > r.bottom() {
                    img_pos.y - r.bottom()
                } else {
                    0.0
                };
                let dx = if img_pos.x < r.x {
                    r.x - img_pos.x
                } else if img_pos.x > r.right() {
                    img_pos.x - r.right()
                } else {
                    0.0
                };
                if dy < best_dy || ((dy - best_dy).abs() < 1e-3 && dx < best_dx) {
                    best_dy = dy;
                    best_dx = dx;
                    found = Some(idx);
                }
            } else if img_pos.x >= r.x
                && img_pos.x <= r.right()
                && img_pos.y >= r.y
                && img_pos.y <= r.bottom()
            {
                found = Some(idx);
                break;
            }
        }

        let line_idx = found?;
        let line = &result.lines[line_idx];
        let bounds = char_x_bounds(line);
        let char_count = line.text.chars().count();
        let mut char_index = char_count;
        for i in 0..char_count {
            let center = (bounds[i] + bounds[i + 1]) * 0.5;
            if img_pos.x < center {
                char_index = i;
                break;
            }
        }
        Some(OcrPosition {
            line: line_idx,
            char_index,
        })
    }

    /// Computes ordered selection spans between anchor and focus.
    #[must_use]
    pub fn spans(&self, result: &OcrResult) -> Vec<OcrSpan> {
        let (Some(mut from), Some(mut till)) = (self.anchor, self.focus) else {
            return Vec::new();
        };
        let count = result.lines.len();
        if from.line >= count || till.line >= count {
            return Vec::new();
        }
        let reading = compute_reading_order(&result.lines);
        let mut rank = vec![0usize; count];
        for (k, &line_idx) in reading.order.iter().enumerate() {
            rank[line_idx] = k;
        }
        let mut from_rank = rank[from.line];
        let mut till_rank = rank[till.line];
        if till_rank < from_rank || (till_rank == from_rank && till.char_index < from.char_index) {
            std::mem::swap(&mut from, &mut till);
            std::mem::swap(&mut from_rank, &mut till_rank);
        }
        if from_rank == till_rank && from.char_index == till.char_index {
            return Vec::new();
        }

        let mut spans = Vec::new();
        let mut last_row: Option<usize> = None;
        for &line_idx in &reading.order[from_rank..=till_rank] {
            let len = result.lines[line_idx].text.chars().count();
            let c0 = if line_idx == from.line {
                from.char_index.min(len)
            } else {
                0
            };
            let c1 = if line_idx == till.line {
                till.char_index.min(len)
            } else {
                len
            };
            if c1 <= c0 {
                continue;
            }
            let row_start = last_row != Some(reading.row[line_idx]);
            spans.push(OcrSpan {
                line: line_idx,
                from: c0,
                till: c1,
                row_start,
            });
            last_row = Some(reading.row[line_idx]);
        }
        spans
    }

    /// Returns the image-space highlight rectangle for a single [`OcrSpan`].
    #[must_use]
    pub fn span_image_rect(result: &OcrResult, span: OcrSpan) -> Option<OcrRect> {
        let line = result.lines.get(span.line)?;
        let len = line.text.chars().count();
        let c0 = span.from.min(len);
        let c1 = span.till.min(len);
        if c1 <= c0 {
            return None;
        }
        let bounds = char_x_bounds(line);
        let left = bounds[c0];
        let right = bounds[c1];
        if right > left {
            Some(OcrRect::new(
                left,
                line.rect.y,
                right - left,
                line.rect.height,
            ))
        } else {
            Some(line.rect)
        }
    }

    /// Extracts the currently selected text joined in reading order.
    #[must_use]
    pub fn selected_text(&self, result: &OcrResult) -> String {
        let spans = self.spans(result);
        let mut out = String::new();
        for span in spans {
            if !out.is_empty() {
                out.push(if span.row_start { '\n' } else { ' ' });
            }
            let chars: String = result.lines[span.line]
                .text
                .chars()
                .skip(span.from)
                .take(span.till.saturating_sub(span.from))
                .collect();
            out.push_str(&chars);
        }
        out
    }
}

/// Computes `char_count + 1` horizontal boundaries across `line`, using word bounding boxes
/// when word lengths match and falling back to uniform character interpolation.
fn char_x_bounds(line: &OcrLine) -> Vec<f32> {
    let chars: Vec<char> = line.text.chars().collect();
    let len = chars.len();
    if len == 0 {
        return vec![line.rect.x];
    }

    // Attempt word-aligned placement when words map cleanly onto `line.text`.
    if !line.words.is_empty() {
        let mut bounds = vec![0.0f32; len + 1];
        let mut cursor = 0usize;
        let mut matched_all = true;
        for word in &line.words {
            while cursor < len && chars[cursor].is_whitespace() {
                cursor += 1;
            }
            let wchars: Vec<char> = word.text.chars().collect();
            let wlen = wchars.len();
            if wlen == 0 || cursor + wlen > len {
                matched_all = false;
                break;
            }
            // Interpolate whitespace before this word if any.
            let prev_end = if cursor > 0 {
                bounds[cursor]
            } else {
                word.rect.x
            };
            let gap_start = cursor;
            let _ = (prev_end, gap_start);
            for i in 0..=wlen {
                bounds[cursor + i] = word.rect.x + (word.rect.width * i as f32) / (wlen as f32);
            }
            cursor += wlen;
        }
        if matched_all && cursor > 0 {
            // Fill any whitespace character boundaries monotonically between adjacent words.
            bounds[0] = bounds[0].max(line.rect.x);
            for i in 1..=len {
                if bounds[i] < bounds[i - 1] {
                    bounds[i] = bounds[i - 1];
                }
            }
            if bounds[len] < line.rect.right() {
                bounds[len] = line.rect.right();
            }
            return bounds;
        }
    }

    let mut bounds = Vec::with_capacity(len + 1);
    for i in 0..=len {
        bounds.push(line.rect.x + (line.rect.width * i as f32) / (len as f32));
    }
    bounds
}

/// Complete OCR UI state embedded inside [`crate::media::overlay::MediaOverlayWidget`].
#[derive(Debug, Clone, Default)]
pub struct OcrOverlayState {
    /// Whether OCR highlight/selection mode is currently shown over the photo.
    pub active: bool,
    /// Cached OCR result for the currently open photo, if recognition has run.
    pub result: Option<OcrResult>,
    /// Active drag selection state.
    pub selection: OcrSelection,
}

impl OcrOverlayState {
    /// Resets OCR state when switching photos or closing the media overlay.
    pub fn reset(&mut self) {
        self.active = false;
        self.result = None;
        self.selection.clear();
    }

    /// Returns the text to copy: selected text if any, otherwise full recognized text.
    #[must_use]
    pub fn copyable_text(&self) -> Option<String> {
        let res = self.result.as_ref()?;
        let selected = self.selection.selected_text(res);
        if !selected.is_empty() {
            return Some(selected);
        }
        let full = recognized_text_in_reading_order(res);
        (!full.trim().is_empty()).then_some(full)
    }
}

#[cfg(test)]
mod tests {
    use tg_platform::ocr::{OcrLine, OcrRect, OcrResult, OcrWord};

    use super::*;

    fn sample_ocr_result() -> OcrResult {
        OcrResult {
            lines: vec![
                OcrLine {
                    text: "Second line".into(),
                    rect: OcrRect::new(20.0, 80.0, 220.0, 30.0),
                    words: vec![
                        OcrWord {
                            text: "Second".into(),
                            rect: OcrRect::new(20.0, 80.0, 120.0, 30.0),
                        },
                        OcrWord {
                            text: "line".into(),
                            rect: OcrRect::new(150.0, 80.0, 90.0, 30.0),
                        },
                    ],
                },
                OcrLine {
                    text: "First line".into(),
                    rect: OcrRect::new(20.0, 20.0, 200.0, 30.0),
                    words: vec![
                        OcrWord {
                            text: "First".into(),
                            rect: OcrRect::new(20.0, 20.0, 100.0, 30.0),
                        },
                        OcrWord {
                            text: "line".into(),
                            rect: OcrRect::new(130.0, 20.0, 90.0, 30.0),
                        },
                    ],
                },
            ],
            text: "Second line\nFirst line".into(),
            text_angle: Some(0.0),
            coordinate_width: 400,
            coordinate_height: 200,
            language_tag: "en-US".into(),
        }
    }

    #[test]
    fn reading_order_sorts_top_to_bottom_and_supports_multiline_selection() {
        let res = sample_ocr_result();
        assert_eq!(
            recognized_text_in_reading_order(&res),
            "First line\nSecond line"
        );

        let mut sel = OcrSelection::default();
        sel.start(OcrPosition {
            line: 1,
            char_index: 6,
        }); // "line" in "First line"
        sel.update_focus(OcrPosition {
            line: 0,
            char_index: 6,
        }); // "Second" in "Second line"
        sel.finish();
        assert!(sel.has_selection(&res));
        assert_eq!(sel.selected_text(&res), "line\nSecond");

        sel.select_all(&res);
        assert_eq!(sel.selected_text(&res), "First line\nSecond line");
        assert!(sel.clear());
        assert!(!sel.has_selection(&res));
    }

    #[test]
    fn screen_and_image_coordinate_roundtrip_across_rotations() {
        let coord = Vec2::new(400.0, 200.0);
        let content = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(800.0, 400.0));
        let img_pt = Pos2::new(120.0, 40.0);

        for rot in [0, 90, 180, 270] {
            for fh in [false, true] {
                for fv in [false, true] {
                    let screen = image_point_to_screen(img_pt, coord, content, rot, fh, fv);
                    let back = screen_point_to_image(screen, coord, content, rot, fh, fv);
                    assert!(
                        (back.x - img_pt.x).abs() < 0.01 && (back.y - img_pt.y).abs() < 0.01,
                        "mismatch for rot={rot} fh={fh} fv={fv}: {back:?} vs {img_pt:?}"
                    );
                }
            }
        }
    }
}
