//! Bridge between `tg-text` (`TextWithEntities`, markdown parsing) and `tg-ui` (`RichText`, egui layout).
//!
//! Port origin: `Telegram/lib_ui/ui/text/text.cpp` and `Telegram/SourceFiles/ui/text/text_utilities.cpp`.

use egui::{Color32, Galley, Pos2, Rect, Ui};
use tg_text::{EntityType, TextWithEntities, parse_entities, parse_markdown, utf16_to_utf8_offset};
use tg_ui::stand_in::{FontSpec, LinkUnderline, TextPalette, TextStyle};
use tg_ui::text::{RichText, TextEntityKind, build_layout_job};

/// Converts `tg_text::TextWithEntities` (UTF-16 offsets) into `tg_ui::text::RichText` (UTF-8 byte spans).
#[must_use]
pub fn entities_to_rich_text(source: &TextWithEntities) -> RichText {
    let mut rich = RichText::plain(source.text.clone());
    let text_u16_len = source.utf16_len();

    for entity in &source.entities {
        if !entity.valid_for_text(text_u16_len) {
            continue;
        }
        let start_u16 = entity.offset() as usize;
        let end_u16 = (entity.offset() + entity.length()) as usize;
        let Some(start) = utf16_to_utf8_offset(&source.text, start_u16) else {
            continue;
        };
        let Some(end) = utf16_to_utf8_offset(&source.text, end_u16) else {
            continue;
        };
        if end <= start {
            continue;
        }
        let slice = &source.text[start..end];
        // Hashtags, cashtags, bot commands and name mentions share their click target with the
        // block renderer (`tg_ui::entity_link`); the history widget handles them.
        if let Some(target) =
            tg_ui::entity_link::entity_link_target(entity.entity_type(), entity.data(), slice)
        {
            rich.add_span(start, end, TextEntityKind::Link(target));
            continue;
        }
        let kind = match entity.entity_type() {
            EntityType::Bold | EntityType::Semibold => TextEntityKind::Bold,
            EntityType::Italic => TextEntityKind::Italic,
            EntityType::Underline => TextEntityKind::Underline,
            EntityType::StrikeOut => TextEntityKind::Strikethrough,
            EntityType::Code | EntityType::Pre => TextEntityKind::Monospace,
            EntityType::Spoiler => TextEntityKind::Spoiler,
            EntityType::CustomUrl => TextEntityKind::Link(entity.data().to_owned()),
            EntityType::Url => {
                let target = if slice.contains("://") {
                    slice.to_owned()
                } else {
                    format!("https://{slice}")
                };
                TextEntityKind::Link(target)
            }
            EntityType::Mention => {
                let handle = slice.trim_start_matches('@');
                TextEntityKind::Link(format!("tg://resolve?domain={handle}"))
            }
            EntityType::MentionName => TextEntityKind::Link(entity.data().to_owned()),
            EntityType::TonAddress | EntityType::MediaTimestamp => {
                TextEntityKind::Link(slice.to_owned())
            }
            EntityType::Email => TextEntityKind::Link(format!("mailto:{slice}")),
            EntityType::Phone => TextEntityKind::Link(format!("tel:{slice}")),
            EntityType::BankCard => TextEntityKind::Link(format!("bankcard:{slice}")),
            EntityType::Blockquote => TextEntityKind::Italic,
            _ => continue,
        };
        rich.add_span(start, end, kind);
    }

    rich
}

/// Parses raw compose input with Telegram Markdown (`**bold**`, `__italic__`, `` `code` ``,
/// `~~strike~~`, `||spoiler||`, `[text](url)`) plus auto-detected links, mentions, hashtags,
/// and bot commands via `tg-text`.
#[must_use]
pub fn parse_compose_markdown(raw: &str) -> TextWithEntities {
    let parsed = parse_markdown(raw);
    let mut with_links = parsed.clone();
    let auto = parse_entities(
        &parsed.text,
        tg_text::TextParseFlags::LINKS
            | tg_text::TextParseFlags::MENTIONS
            | tg_text::TextParseFlags::HASHTAGS
            | tg_text::TextParseFlags::BOT_COMMANDS,
    );
    with_links.entities.extend(auto.entities);
    with_links.sort_entities();
    with_links
}

/// Message body [`TextStyle`]: tdesktop's `historyTextStyle` (`defaultTextStyle`, `msgFont` =
/// `normalFont` = Open Sans 13px, `lineHeight: 0` so lines are `normalFont->height` = 18px,
/// the same metric `tg_history::FontMetrics::normal` lays the bubble out with).
#[must_use]
pub fn message_text_style() -> TextStyle {
    TextStyle {
        font: FontSpec::regular(13.0),
        line_height: 18.0,
        link_underline: LinkUnderline::Active,
    }
}

/// Measures a [`TextWithEntities`] string's `(single_line_width, wrapped_height, longest_wrapped_line_width)`
/// at `max_wrap_width` using `egui::Painter` layout.
#[must_use]
pub fn measure_rich_text(
    ui: &Ui,
    source: &TextWithEntities,
    palette: &TextPalette,
    base_color: Color32,
    max_wrap_width: f32,
) -> (i32, i32, i32) {
    if source.text.is_empty() {
        return (0, 0, 0);
    }
    let rich = entities_to_rich_text(source);
    let style = message_text_style();

    let single_job = build_layout_job(
        &rich,
        &style,
        palette,
        base_color,
        f32::INFINITY,
        None,
        true,
    );
    let single_galley = ui.painter().layout_job(single_job);
    let single_w = single_galley.size().x.ceil() as i32;

    let wrap_job = build_layout_job(
        &rich,
        &style,
        palette,
        base_color,
        max_wrap_width.max(40.0),
        None,
        true,
    );
    let wrap_galley = ui.painter().layout_job(wrap_job);
    let wrapped_h = wrap_galley.size().y.ceil() as i32;
    let real_w = wrap_galley.size().x.ceil() as i32;

    (single_w, wrapped_h, real_w)
}

/// Fast headless heuristic text measurement for off-screen height estimation and unit tests.
#[must_use]
pub fn estimate_text_metrics(text: &str, max_width: i32) -> (i32, i32) {
    if text.is_empty() {
        return (0, 0);
    }
    const AVG_CHAR_W: i32 = 7;
    const LINE_H: i32 = 19;
    let wrap_w = max_width.max(60);
    let mut max_line_w = 0;
    let mut total_lines = 0;
    for line in text.split('\n') {
        let line_w = (line.chars().count() as i32 * AVG_CHAR_W).max(AVG_CHAR_W);
        max_line_w = max_line_w.max(line_w);
        let wrapped = (line_w + wrap_w - 1) / wrap_w;
        total_lines += wrapped.max(1);
    }
    (max_line_w, total_lines * LINE_H)
}

/// Converts a screen pointer position over a rendered [`Galley`] into a character index
/// for drag-selection (`FlatSelectionEndpoint`).
#[must_use]
pub fn pointer_to_char_index(galley: &Galley, origin: Pos2, pointer: Pos2) -> (u16, bool) {
    let rel = pointer - origin;
    let cursor = galley.cursor_from_pos(rel);
    let idx = cursor.index.0.min(u16::MAX as usize) as u16;
    (idx, false)
}

/// Extracts a character-indexed slice `[from..to)` from `text` safely.
#[must_use]
pub fn slice_by_char_range(text: &str, from: u16, to: u16) -> String {
    let start = usize::from(from);
    let end = usize::from(to);
    if end <= start {
        return String::new();
    }
    text.chars().skip(start).take(end - start).collect()
}

/// Computes per-row selection highlight rectangles for char range `[from_char..to_char)` inside `galley`.
#[must_use]
pub fn char_range_rects(
    galley: &Galley,
    origin: Pos2,
    from_char: usize,
    to_char: usize,
) -> Vec<Rect> {
    if to_char <= from_char {
        return Vec::new();
    }
    let mut rects = Vec::new();
    let mut global_char = 0usize;

    for placed in &galley.rows {
        let row_chars = placed.row.glyphs.len();
        let row_start = global_char;
        let row_end = row_start + row_chars;
        let row_top = origin.y + placed.pos.y;
        let row_bottom = row_top + placed.row.size.y;

        let ov_start = from_char.max(row_start);
        let ov_end = to_char.min(row_end);
        if ov_end > ov_start {
            let first_g = &placed.row.glyphs[ov_start - row_start];
            let last_g = &placed.row.glyphs[ov_end - row_start - 1];
            let min_x = origin.x + placed.pos.x + first_g.pos.x;
            let max_x = origin.x + placed.pos.x + last_g.pos.x + last_g.advance_width;
            if max_x > min_x {
                rects.push(Rect::from_min_max(
                    Pos2::new(min_x, row_top),
                    Pos2::new(max_x, row_bottom),
                ));
            }
        }
        global_char = row_end;
        if placed.ends_with_newline {
            global_char += 1;
        }
    }
    rects
}
