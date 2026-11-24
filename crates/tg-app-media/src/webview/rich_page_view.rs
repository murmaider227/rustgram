//! Native `egui` renderer and layout measurer for [`tg_iv::rich_page::RichPage`] documents
//! (rich messages, editor preview, and Instant View math/article rendering).
//!
//! Port origin: `Telegram/SourceFiles/iv/markdown/iv_markdown_prepare.cpp` and
//! `Telegram/SourceFiles/history/view/media/history_view_rich_message.cpp`.
//!
//! Renders all [`RichBlockKind`] blocks and inline [`TextWithEntities`] spans, painting both
//! display math (`RichBlockKind::Math`) and inline formulas (`\u{FFFC}` formula text objects)
//! at the text baseline via [`crate::webview::formula_widget`].

use egui::text::LayoutJob;
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontFamily, FontId, Id, Painter, Pos2, Rect, Sense,
    Stroke, StrokeKind, TextFormat, Ui, Vec2,
};
use tg_iv::markdown::MathLayoutEngine;
use tg_iv::model::{ordered_alpha_text, ordered_roman_text};
use tg_iv::rich_page::{
    EntityType, InlineTextObjectEntity, OBJECT_REPLACEMENT_CHAR, RichBlock, RichBlockKind,
    RichButtonColor, RichListKind, RichPage, RichTableAlignment, RichTaskState, TextWithEntities,
    parse_inline_text_object_entity, rich_page_from_model_page, utf16_to_byte_offset,
};

use crate::webview::formula_widget::{
    FormulaWidgetStyle, layout_formula_for_style, paint_formula_at_baseline, with_math_engine,
};

pub use tg_app_core::core_types::dto::rich_message::*;

/// Visual theme parameters for [`measure_rich_page`] and [`paint_rich_page`].
#[derive(Debug, Clone, Copy)]
pub struct RichPageViewStyle {
    /// Base body font size in logical pixels.
    pub base_font_size: f32,
    /// Primary text color.
    pub text_fg: Color32,
    /// Muted / secondary caption & footer color.
    pub muted_fg: Color32,
    /// Accent color for links, quote bars, and checkboxes.
    pub accent: Color32,
    /// Inline and block code background fill.
    pub code_bg: Color32,
    /// Inline and block code text color.
    pub code_fg: Color32,
    /// Blockquote tinted background fill.
    pub quote_bg: Color32,
    /// Highlighted / `<mark>` background fill.
    pub mark_bg: Color32,
    /// Spoiler veil background fill.
    pub spoiler_bg: Color32,
    /// Table header / striped row tint.
    pub table_header_bg: Color32,
    /// Table zebra stripe tint.
    pub table_stripe_bg: Color32,
    /// Divider and table border stroke color.
    pub border: Color32,
    /// Vertical gap between sibling blocks in logical pixels.
    pub block_spacing: f32,
}

impl RichPageViewStyle {
    /// Light theme defaults suitable for Day mode bubbles and previews.
    #[must_use]
    pub const fn day() -> Self {
        Self {
            base_font_size: 14.0,
            text_fg: Color32::from_rgb(24, 28, 34),
            muted_fg: Color32::from_rgb(104, 114, 128),
            accent: Color32::from_rgb(22, 138, 205),
            code_bg: Color32::from_rgba_premultiplied(20, 30, 45, 22),
            code_fg: Color32::from_rgb(168, 42, 74),
            quote_bg: Color32::from_rgba_premultiplied(22, 138, 205, 18),
            mark_bg: Color32::from_rgba_premultiplied(255, 224, 102, 110),
            spoiler_bg: Color32::from_rgb(168, 178, 190),
            table_header_bg: Color32::from_rgba_premultiplied(20, 30, 45, 22),
            table_stripe_bg: Color32::from_rgba_premultiplied(20, 30, 45, 10),
            border: Color32::from_rgb(208, 214, 222),
            block_spacing: 6.0,
        }
    }

    /// Dark theme defaults suitable for Night mode bubbles and previews.
    #[must_use]
    pub const fn night() -> Self {
        Self {
            base_font_size: 14.0,
            text_fg: Color32::from_rgb(235, 240, 246),
            muted_fg: Color32::from_rgb(142, 156, 174),
            accent: Color32::from_rgb(90, 178, 245),
            code_bg: Color32::from_rgba_premultiplied(255, 255, 255, 22),
            code_fg: Color32::from_rgb(245, 165, 190),
            quote_bg: Color32::from_rgba_premultiplied(90, 178, 245, 26),
            mark_bg: Color32::from_rgba_premultiplied(230, 188, 52, 84),
            spoiler_bg: Color32::from_rgb(76, 88, 102),
            table_header_bg: Color32::from_rgba_premultiplied(255, 255, 255, 22),
            table_stripe_bg: Color32::from_rgba_premultiplied(255, 255, 255, 10),
            border: Color32::from_rgb(66, 78, 92),
            block_spacing: 6.0,
        }
    }

    /// Derives a [`RichPageViewStyle`] from chat history theme colors.
    #[must_use]
    pub fn from_history_colors(
        colors: &tg_app_core::history::theme::HistoryColors,
        outgoing: bool,
    ) -> Self {
        let mut base = if colors.is_night {
            Self::night()
        } else {
            Self::day()
        };
        base.text_fg = colors.text_fg;
        base.muted_fg = colors.sub_text_fg;
        base.accent = if outgoing {
            colors.link_out_fg
        } else {
            colors.link_in_fg
        };
        base.code_bg = colors.mono_bg;
        base.code_fg = if outgoing {
            colors.mono_out_fg
        } else {
            colors.mono_in_fg
        };
        base.border = colors.divider;
        base
    }
}

/// Interactive event emitted by [`paint_rich_page`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RichPageViewEvent {
    /// User clicked a hyperlink inside the rich document.
    OpenLink(String),
}

/// One inline segment inside a formatted rich text leaf: either styled text or an inline object.
#[derive(Debug, Clone)]
pub enum InlineSegment {
    /// Styled plain text slice with active formatting flags.
    Text {
        /// UTF-8 text content.
        text: String,
        /// Start offset in UTF-16 code units.
        utf16_start: usize,
        /// End offset in UTF-16 code units.
        utf16_end: usize,
        /// Bold formatting active.
        bold: bool,
        /// Italic formatting active.
        italic: bool,
        /// Underline formatting active.
        underline: bool,
        /// Strikethrough formatting active.
        strike: bool,
        /// Inline monospace code active.
        code: bool,
        /// Spoiler active.
        spoiler: bool,
        /// Subscript active.
        subscript: bool,
        /// Superscript active.
        superscript: bool,
        /// Highlighted (`<mark>`) active.
        marked: bool,
        /// Optional clickable link URL.
        link: Option<String>,
    },
    /// Inline math formula (`\u{FFFC}` placeholder) rendered via `MathLayoutEngine`.
    Formula {
        /// Trimmed TeX source.
        tex: String,
        /// Start UTF-16 offset of the placeholder.
        utf16_start: usize,
        /// End UTF-16 offset of the placeholder.
        utf16_end: usize,
    },
    /// Inline button chip (`textButton`).
    Button {
        /// Button label.
        label: String,
        /// Button color style.
        color: RichButtonColor,
        /// Optional URL payload.
        url: Option<String>,
        /// Start UTF-16 offset of the placeholder.
        utf16_start: usize,
        /// End UTF-16 offset of the placeholder.
        utf16_end: usize,
    },
    /// Inline IV image placeholder.
    Image {
        /// Replacement text (e.g. `"[image]"`).
        label: String,
        /// Start UTF-16 offset.
        utf16_start: usize,
        /// End UTF-16 offset.
        utf16_end: usize,
    },
}

/// Splits a [`TextWithEntities`] into ordered [`InlineSegment`]s for baseline-aligned painting
/// of styled text and inline formulas.
#[must_use]
pub fn decompose_inline_segments(rich: &TextWithEntities) -> Vec<InlineSegment> {
    let total_u16 = rich.utf16_len();
    if total_u16 == 0 {
        return Vec::new();
    }

    // Collect boundary UTF-16 offsets from all entities plus inline object placeholders.
    let mut cuts = vec![0usize, total_u16];
    let mut objects: Vec<(usize, usize, InlineTextObjectEntity)> = Vec::new();
    for e in &rich.entities {
        let s = e.offset.min(total_u16);
        let t = e.end().min(total_u16);
        if t > s {
            cuts.push(s);
            cuts.push(t);
            if e.kind == EntityType::CustomEmoji
                && let Some(obj) = parse_inline_text_object_entity(&e.data)
            {
                objects.push((s, t, obj));
            }
        }
    }
    cuts.sort_unstable();
    cuts.dedup();

    let mut segments = Vec::new();
    for win in cuts.windows(2) {
        let (from, till) = (win[0], win[1]);
        if till <= from {
            continue;
        }
        // Check if this span is an inline text object (`\u{FFFC}`).
        if let Some((_, _, obj)) = objects.iter().find(|(s, t, _)| *s <= from && *t >= till) {
            let b0 = utf16_to_byte_offset(&rich.text, from);
            let b1 = utf16_to_byte_offset(&rich.text, till);
            let slice = &rich.text[b0..b1];
            if slice.contains(OBJECT_REPLACEMENT_CHAR) {
                match obj {
                    InlineTextObjectEntity::Formula(f) => segments.push(InlineSegment::Formula {
                        tex: f.trimmed_tex.clone(),
                        utf16_start: from,
                        utf16_end: till,
                    }),
                    InlineTextObjectEntity::Button(btn) => {
                        let url = (btn.button_type == tg_iv::rich_page::RichButtonType::Url)
                            .then(|| String::from_utf8_lossy(&btn.data).into_owned());
                        segments.push(InlineSegment::Button {
                            label: btn.label.text.clone(),
                            color: btn.color,
                            url,
                            utf16_start: from,
                            utf16_end: till,
                        });
                    }
                    InlineTextObjectEntity::IvImage(img) => segments.push(InlineSegment::Image {
                        label: if img.replacement_text.is_empty() {
                            "[image]".to_owned()
                        } else {
                            img.replacement_text.clone()
                        },
                        utf16_start: from,
                        utf16_end: till,
                    }),
                }
                continue;
            }
        }

        let b0 = utf16_to_byte_offset(&rich.text, from);
        let b1 = utf16_to_byte_offset(&rich.text, till);
        let slice = &rich.text[b0..b1];
        if slice.is_empty() {
            continue;
        }

        let mut bold = false;
        let mut italic = false;
        let mut underline = false;
        let mut strike = false;
        let mut code = false;
        let mut spoiler = false;
        let mut subscript = false;
        let mut superscript = false;
        let mut marked = false;
        let mut link: Option<String> = None;

        for e in &rich.entities {
            if e.offset <= from && e.end() >= till && e.length > 0 {
                match e.kind {
                    EntityType::Bold => bold = true,
                    EntityType::Italic => italic = true,
                    EntityType::Underline => underline = true,
                    EntityType::StrikeOut => strike = true,
                    EntityType::Code => code = true,
                    EntityType::Spoiler => spoiler = true,
                    EntityType::Subscript => subscript = true,
                    EntityType::Superscript => superscript = true,
                    EntityType::Marked => marked = true,
                    EntityType::CustomUrl => {
                        if !e.data.is_empty() {
                            link = Some(e.data.clone());
                        }
                    }
                    EntityType::Url => link = Some(slice.to_owned()),
                    EntityType::Email => link = Some(format!("mailto:{slice}")),
                    EntityType::Phone => link = Some(format!("tel:{slice}")),
                    EntityType::Mention => {
                        link = Some(format!(
                            "tg://resolve?domain={}",
                            slice.trim_start_matches('@')
                        ));
                    }
                    EntityType::Hashtag | EntityType::BotCommand | EntityType::Cashtag => {
                        link = Some(slice.to_owned());
                    }
                    _ => {}
                }
            }
        }

        segments.push(InlineSegment::Text {
            text: slice.to_owned(),
            utf16_start: from,
            utf16_end: till,
            bold,
            italic,
            underline,
            strike,
            code,
            spoiler,
            subscript,
            superscript,
            marked,
            link,
        });
    }
    segments
}

/// Returns true if `rich` contains at least one inline object (`\u{FFFC}` formula/button/image).
#[must_use]
pub fn has_inline_objects(rich: &TextWithEntities) -> bool {
    rich.text.contains(OBJECT_REPLACEMENT_CHAR)
}

/// Builds an `egui::text::LayoutJob` for a [`TextWithEntities`] without inline objects.
#[must_use]
pub fn build_rich_leaf_layout_job(
    rich: &TextWithEntities,
    font_size: f32,
    base_color: Color32,
    style: &RichPageViewStyle,
    max_width: f32,
    force_bold: bool,
    force_italic: bool,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = max_width.max(24.0);
    let segments = decompose_inline_segments(rich);
    if segments.is_empty() {
        return job;
    }
    for seg in segments {
        match seg {
            InlineSegment::Text {
                text,
                bold,
                italic,
                underline,
                strike,
                code,
                spoiler,
                subscript,
                superscript,
                marked,
                link,
                ..
            } => {
                let mut sz = font_size;
                let mut valign = egui::Align::BOTTOM;
                if subscript {
                    sz = (font_size * 0.76).max(8.0);
                    valign = egui::Align::BOTTOM;
                } else if superscript {
                    sz = (font_size * 0.76).max(8.0);
                    valign = egui::Align::TOP;
                }
                let family = if code {
                    FontFamily::Monospace
                } else {
                    FontFamily::Proportional
                };
                let mut fg = if code {
                    style.code_fg
                } else if link.is_some() {
                    style.accent
                } else {
                    base_color
                };
                let mut bg = if code {
                    style.code_bg
                } else if marked {
                    style.mark_bg
                } else {
                    Color32::TRANSPARENT
                };
                if spoiler {
                    bg = style.spoiler_bg;
                    fg = style.spoiler_bg;
                }
                let underline_stroke = if underline || link.is_some() {
                    Stroke::new(1.0, fg)
                } else {
                    Stroke::NONE
                };
                let strike_stroke = if strike {
                    Stroke::new(1.0, fg)
                } else {
                    Stroke::NONE
                };
                let fmt = TextFormat {
                    font_id: FontId::new(sz, family),
                    color: fg,
                    background: bg,
                    italics: italic || force_italic,
                    underline: underline_stroke,
                    strikethrough: strike_stroke,
                    valign,
                    extra_letter_spacing: if bold || force_bold { 0.2 } else { 0.0 },
                    ..TextFormat::default()
                };
                job.append(&text, 0.0, fmt);
            }
            InlineSegment::Formula { tex, .. } => {
                let fmt = TextFormat {
                    font_id: FontId::monospace((font_size * 0.92).max(9.0)),
                    color: style.accent,
                    background: style.code_bg,
                    ..TextFormat::default()
                };
                job.append(&format!("${tex}$"), 0.0, fmt);
            }
            InlineSegment::Button { label, .. } | InlineSegment::Image { label, .. } => {
                let fmt = TextFormat {
                    font_id: FontId::proportional(font_size),
                    color: style.accent,
                    background: style.code_bg,
                    ..TextFormat::default()
                };
                job.append(&label, 0.0, fmt);
            }
        }
    }
    job
}

/// Measures a rich inline paragraph (supporting inline formulas at text baseline) within `max_width`.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn measure_inline_rich_text(
    painter: &Painter,
    engine: &mut MathLayoutEngine,
    rich: &TextWithEntities,
    font_size: f32,
    base_color: Color32,
    style: &RichPageViewStyle,
    max_width: f32,
    force_bold: bool,
    force_italic: bool,
) -> Vec2 {
    if rich.is_empty() {
        return Vec2::new(0.0, font_size * 1.35);
    }
    if !has_inline_objects(rich) {
        let job = build_rich_leaf_layout_job(
            rich,
            font_size,
            base_color,
            style,
            max_width,
            force_bold,
            force_italic,
        );
        let galley = painter.layout_job(job);
        return galley.size();
    }
    layout_inline_flow(
        painter,
        engine,
        rich,
        font_size,
        base_color,
        style,
        max_width,
        force_bold,
        force_italic,
    )
    .size
}

struct InlineFlowItem {
    x: f32,
    width: f32,
    ascent: f32,
    depth: f32,
    kind: InlineFlowItemKind,
}

enum InlineFlowItemKind {
    Galley {
        galley: std::sync::Arc<egui::Galley>,
        color: Color32,
        bold: bool,
        link: Option<String>,
    },
    Formula {
        rendered: tg_iv::markdown::RenderedFormula,
        style: FormulaWidgetStyle,
    },
    Button {
        label: String,
        color: RichButtonColor,
        url: Option<String>,
    },
}

struct InlineFlowLine {
    items: Vec<InlineFlowItem>,
    ascent: f32,
    depth: f32,
    width: f32,
}

struct InlineFlowLayout {
    lines: Vec<InlineFlowLine>,
    size: Vec2,
}

#[allow(clippy::too_many_arguments)]
fn layout_inline_flow(
    painter: &Painter,
    engine: &mut MathLayoutEngine,
    rich: &TextWithEntities,
    font_size: f32,
    base_color: Color32,
    style: &RichPageViewStyle,
    max_width: f32,
    force_bold: bool,
    force_italic: bool,
) -> InlineFlowLayout {
    let segments = decompose_inline_segments(rich);
    let max_w = max_width.max(40.0);
    let default_ascent = font_size * 0.85;
    let default_depth = font_size * 0.35;

    let mut lines: Vec<InlineFlowLine> = vec![InlineFlowLine {
        items: Vec::new(),
        ascent: default_ascent,
        depth: default_depth,
        width: 0.0,
    }];

    let push_new_line = |lines: &mut Vec<InlineFlowLine>| {
        lines.push(InlineFlowLine {
            items: Vec::new(),
            ascent: default_ascent,
            depth: default_depth,
            width: 0.0,
        });
    };

    for seg in segments {
        match seg {
            InlineSegment::Text {
                text,
                bold,
                italic,
                underline,
                strike,
                code,
                spoiler,
                subscript,
                superscript,
                marked,
                link,
                ..
            } => {
                // Split by explicit newlines, then by whitespace-preserving word tokens so lines wrap cleanly.
                let mut first_line = true;
                for raw_line in text.split('\n') {
                    if !first_line {
                        push_new_line(&mut lines);
                    }
                    first_line = false;
                    if raw_line.is_empty() {
                        continue;
                    }
                    for token in split_keep_whitespace(raw_line) {
                        let sz = if subscript || superscript {
                            (font_size * 0.76).max(8.0)
                        } else {
                            font_size
                        };
                        let family = if code {
                            FontFamily::Monospace
                        } else {
                            FontFamily::Proportional
                        };
                        let mut fg = if code {
                            style.code_fg
                        } else if link.is_some() {
                            style.accent
                        } else {
                            base_color
                        };
                        let mut bg = if code {
                            style.code_bg
                        } else if marked {
                            style.mark_bg
                        } else {
                            Color32::TRANSPARENT
                        };
                        if spoiler {
                            bg = style.spoiler_bg;
                            fg = style.spoiler_bg;
                        }
                        let mut job = LayoutJob::default();
                        job.append(
                            token,
                            0.0,
                            TextFormat {
                                font_id: FontId::new(sz, family),
                                color: fg,
                                background: bg,
                                italics: italic || force_italic,
                                underline: if underline || link.is_some() {
                                    Stroke::new(1.0, fg)
                                } else {
                                    Stroke::NONE
                                },
                                strikethrough: if strike {
                                    Stroke::new(1.0, fg)
                                } else {
                                    Stroke::NONE
                                },
                                ..TextFormat::default()
                            },
                        );
                        let galley = painter.layout_job(job);
                        let w = galley.size().x;
                        let h = galley.size().y.max(font_size);
                        let ascent = h * 0.78;
                        let depth = (h - ascent).max(default_depth * 0.5);

                        let cur = lines.last().expect("at least one line");
                        if cur.width > 0.0 && cur.width + w > max_w {
                            push_new_line(&mut lines);
                        }
                        let cur = lines.last_mut().expect("at least one line");
                        let x = cur.width;
                        cur.width += w;
                        cur.ascent = cur.ascent.max(ascent);
                        cur.depth = cur.depth.max(depth);
                        cur.items.push(InlineFlowItem {
                            x,
                            width: w,
                            ascent,
                            depth,
                            kind: InlineFlowItemKind::Galley {
                                galley,
                                color: fg,
                                bold: bold || force_bold,
                                link: link.clone(),
                            },
                        });
                    }
                }
            }
            InlineSegment::Formula { tex, .. } => {
                let mut fstyle = FormulaWidgetStyle::inline(font_size, base_color);
                fstyle.max_width = (max_w.round() as i32).max(64);
                let (rendered, metrics) = layout_formula_for_style(engine, &tex, &fstyle);
                let cur = lines.last().expect("at least one line");
                if cur.width > 0.0 && cur.width + metrics.size.x > max_w {
                    push_new_line(&mut lines);
                }
                let cur = lines.last_mut().expect("at least one line");
                let x = cur.width;
                cur.width += metrics.size.x;
                cur.ascent = cur.ascent.max(metrics.baseline_y);
                cur.depth = cur
                    .depth
                    .max((metrics.size.y - metrics.baseline_y).max(0.0));
                cur.items.push(InlineFlowItem {
                    x,
                    width: metrics.size.x,
                    ascent: metrics.baseline_y,
                    depth: (metrics.size.y - metrics.baseline_y).max(0.0),
                    kind: InlineFlowItemKind::Formula {
                        rendered,
                        style: fstyle,
                    },
                });
            }
            InlineSegment::Button {
                label, color, url, ..
            } => {
                let g = painter.layout_no_wrap(
                    label.clone(),
                    FontId::proportional((font_size * 0.9).max(10.0)),
                    style.text_fg,
                );
                let w = g.size().x + 14.0;
                let h = g.size().y + 6.0;
                let ascent = h * 0.75;
                let depth = h - ascent;
                let cur = lines.last().expect("at least one line");
                if cur.width > 0.0 && cur.width + w > max_w {
                    push_new_line(&mut lines);
                }
                let cur = lines.last_mut().expect("at least one line");
                let x = cur.width;
                cur.width += w + 3.0;
                cur.ascent = cur.ascent.max(ascent);
                cur.depth = cur.depth.max(depth);
                cur.items.push(InlineFlowItem {
                    x,
                    width: w,
                    ascent,
                    depth,
                    kind: InlineFlowItemKind::Button { label, color, url },
                });
            }
            InlineSegment::Image { label, .. } => {
                let g = painter.layout_no_wrap(
                    label,
                    FontId::proportional(font_size * 0.9),
                    style.muted_fg,
                );
                let w = g.size().x + 4.0;
                let h = g.size().y;
                let ascent = h * 0.78;
                let depth = h - ascent;
                let cur = lines.last_mut().expect("at least one line");
                let x = cur.width;
                cur.width += w;
                cur.ascent = cur.ascent.max(ascent);
                cur.depth = cur.depth.max(depth);
                cur.items.push(InlineFlowItem {
                    x,
                    width: w,
                    ascent,
                    depth,
                    kind: InlineFlowItemKind::Galley {
                        galley: g,
                        color: style.muted_fg,
                        bold: false,
                        link: None,
                    },
                });
            }
        }
    }

    let mut total_w = 0.0_f32;
    let mut total_h = 0.0_f32;
    for (idx, line) in lines.iter().enumerate() {
        total_w = total_w.max(line.width);
        total_h += line.ascent + line.depth;
        if idx + 1 < lines.len() {
            total_h += 2.0;
        }
    }
    InlineFlowLayout {
        lines,
        size: Vec2::new(total_w, total_h.max(font_size * 1.25)),
    }
}

fn split_keep_whitespace(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut in_space = false;
    for (idx, ch) in s.char_indices() {
        let is_sp = ch.is_whitespace();
        if idx > start && is_sp != in_space {
            out.push(&s[start..idx]);
            start = idx;
        }
        in_space = is_sp;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Paints a formatted [`TextWithEntities`] leaf inside `ui` with baseline-aligned inline formulas.
///
/// Returns `(allocated_size, optional_clicked_url)`.
#[allow(clippy::too_many_arguments)]
pub fn paint_inline_rich_text(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    engine: &mut MathLayoutEngine,
    rich: &TextWithEntities,
    font_size: f32,
    base_color: Color32,
    style: &RichPageViewStyle,
    max_width: f32,
    force_bold: bool,
    force_italic: bool,
) -> (Vec2, Option<String>) {
    if rich.is_empty() {
        let h = font_size * 1.3;
        let _ = ui.allocate_exact_size(Vec2::new(1.0, h), Sense::hover());
        return (Vec2::new(1.0, h), None);
    }

    let flow = layout_inline_flow(
        ui.painter(),
        engine,
        rich,
        font_size,
        base_color,
        style,
        max_width,
        force_bold,
        force_italic,
    );
    let alloc_w = flow.size.x.min(max_width.max(24.0)).max(1.0);
    let (rect, _resp) = ui.allocate_exact_size(Vec2::new(alloc_w, flow.size.y), Sense::hover());
    let mut clicked_link = None;
    let mut y = rect.top();
    let base_id = ui.scope_id().with(id_salt);

    for (line_idx, line) in flow.lines.iter().enumerate() {
        let baseline_y = y + line.ascent;
        for (item_idx, item) in line.items.iter().enumerate() {
            match &item.kind {
                InlineFlowItemKind::Galley {
                    galley,
                    color,
                    bold,
                    link,
                } => {
                    let pos = Pos2::new(rect.left() + item.x, baseline_y - item.ascent);
                    ui.painter().galley(pos, galley.clone(), *color);
                    if *bold {
                        ui.painter()
                            .galley(pos + Vec2::new(0.45, 0.0), galley.clone(), *color);
                    }
                    if let Some(url) = link {
                        let hit_rect = Rect::from_min_size(
                            pos,
                            Vec2::new(item.width, item.ascent + item.depth),
                        );
                        let r = ui.interact(
                            hit_rect,
                            base_id.with(("lnk", line_idx, item_idx)),
                            Sense::click(),
                        );
                        if r.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        }
                        if r.clicked() {
                            clicked_link = Some(url.clone());
                        }
                    }
                }
                InlineFlowItemKind::Formula {
                    rendered,
                    style: fstyle,
                } => {
                    let baseline_left = Pos2::new(rect.left() + item.x, baseline_y);
                    paint_formula_at_baseline(ui.painter(), baseline_left, rendered, fstyle);
                }
                InlineFlowItemKind::Button { label, color, url } => {
                    let btn_rect = Rect::from_min_size(
                        Pos2::new(rect.left() + item.x, baseline_y - item.ascent),
                        Vec2::new(item.width, item.ascent + item.depth),
                    );
                    let fill = button_fill_color(*color, style);
                    ui.painter()
                        .rect_filled(btn_rect, CornerRadius::same(5), fill);
                    ui.painter().text(
                        btn_rect.center(),
                        Align2::CENTER_CENTER,
                        label,
                        FontId::proportional((font_size * 0.88).max(10.0)),
                        if *color == RichButtonColor::Normal {
                            style.text_fg
                        } else {
                            Color32::WHITE
                        },
                    );
                    if let Some(u) = url {
                        let r = ui.interact(
                            btn_rect,
                            base_id.with(("btn", line_idx, item_idx)),
                            Sense::click(),
                        );
                        if r.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        }
                        if r.clicked() {
                            clicked_link = Some(u.clone());
                        }
                    }
                }
            }
        }
        y += line.ascent + line.depth + 2.0;
    }

    (flow.size, clicked_link)
}

fn button_fill_color(color: RichButtonColor, style: &RichPageViewStyle) -> Color32 {
    match color {
        RichButtonColor::Normal => style.code_bg,
        RichButtonColor::Primary => style.accent,
        RichButtonColor::Danger => Color32::from_rgb(212, 68, 68),
        RichButtonColor::Success => Color32::from_rgb(46, 160, 88),
    }
}

/// Computes heading font size scaled by heading level `1..=6`.
#[must_use]
pub fn heading_font_size(level: u8, base: f32) -> f32 {
    match level {
        1 => base * 1.55,
        2 => base * 1.35,
        3 => base * 1.20,
        4 => base * 1.08,
        5 => base * 1.00,
        _ => base * 0.92,
    }
}

/// Formats an ordered list marker (`1.`, `a.`, `I.`, etc.) for item `index`.
#[must_use]
pub fn format_ordered_marker(block: &RichBlock, index: usize) -> String {
    if let Some(item) = block.list_items.get(index)
        && let Some(ref raw) = item.number.num
        && !raw.is_empty()
    {
        return format!("{raw}.");
    }
    let count = block.list_items.len() as i32;
    let start = block
        .ordered_list
        .start
        .unwrap_or(if block.ordered_list.reversed {
            count.max(1)
        } else {
            1
        });
    let value = block
        .list_items
        .get(index)
        .and_then(|i| i.number.value)
        .unwrap_or_else(|| {
            if block.ordered_list.reversed {
                start - index as i32
            } else {
                start + index as i32
            }
        });
    let list_type = block
        .list_items
        .get(index)
        .and_then(|i| i.number.list_type.as_deref())
        .or(block.ordered_list.list_type.as_deref())
        .unwrap_or("1");
    let formatted = match list_type {
        "a" => {
            let s = ordered_alpha_text(value, false);
            if s.is_empty() { value.to_string() } else { s }
        }
        "A" => {
            let s = ordered_alpha_text(value, true);
            if s.is_empty() { value.to_string() } else { s }
        }
        "i" => {
            let s = ordered_roman_text(value, false);
            if s.is_empty() { value.to_string() } else { s }
        }
        "I" => {
            let s = ordered_roman_text(value, true);
            if s.is_empty() { value.to_string() } else { s }
        }
        _ => value.to_string(),
    };
    format!("{formatted}.")
}

/// Measures the total `(width, height)` required to render `page` within `max_width` using `painter`.
#[must_use]
pub fn measure_rich_page(
    painter: &Painter,
    engine: &mut MathLayoutEngine,
    page: &RichPage,
    max_width: f32,
    style: &RichPageViewStyle,
) -> Vec2 {
    measure_blocks(painter, engine, &page.blocks, max_width.max(80.0), style, 0)
}

/// Headless height/width estimator for [`RichPage`] when no `egui::Painter` is available yet.
#[must_use]
pub fn estimate_rich_page_size(page: &RichPage, max_width: f32, style: &RichPageViewStyle) -> Vec2 {
    with_math_engine(|engine| estimate_blocks(engine, &page.blocks, max_width.max(80.0), style, 0))
}

fn estimate_blocks(
    engine: &mut MathLayoutEngine,
    blocks: &[RichBlock],
    max_width: f32,
    style: &RichPageViewStyle,
    depth: usize,
) -> Vec2 {
    if depth > 12 || blocks.is_empty() {
        return Vec2::ZERO;
    }
    let mut w = 0.0_f32;
    let mut h = 0.0_f32;
    let char_w = style.base_font_size * 0.54;
    let line_h = style.base_font_size * 1.38;

    let est_text = |text: &str, avail: f32, fsz: f32| -> Vec2 {
        if text.is_empty() {
            return Vec2::new(0.0, fsz * 1.25);
        }
        let cw = fsz * 0.54;
        let lh = fsz * 1.38;
        let chars_per_line = ((avail / cw).floor() as usize).max(8);
        let mut lines = 0usize;
        let mut max_line_w = 0.0_f32;
        for l in text.split('\n') {
            let n = l.chars().count();
            max_line_w = max_line_w.max((n as f32 * cw).min(avail));
            lines += n.div_ceil(chars_per_line).max(1);
        }
        Vec2::new(max_line_w, lines as f32 * lh)
    };

    for (idx, block) in blocks.iter().enumerate() {
        let sz = match block.kind {
            RichBlockKind::Heading => {
                let fsz = heading_font_size(block.heading_level, style.base_font_size);
                est_text(&block.text.text.text, max_width, fsz)
            }
            RichBlockKind::Paragraph | RichBlockKind::Thinking => {
                est_text(&block.text.text.text, max_width, style.base_font_size)
            }
            RichBlockKind::Footer => est_text(
                &block.text.text.text,
                max_width,
                style.base_font_size * 0.88,
            ),
            RichBlockKind::Code => {
                let body = est_text(
                    &block.text.text.text,
                    (max_width - 20.0).max(60.0),
                    style.base_font_size * 0.9,
                );
                let header_h = if block.language.is_empty() { 0.0 } else { 18.0 };
                Vec2::new((body.x + 20.0).min(max_width), body.y + header_h + 16.0)
            }
            RichBlockKind::Math => {
                let mut fstyle =
                    FormulaWidgetStyle::display(style.base_font_size * 1.1, style.text_fg);
                fstyle.max_width = max_width.round() as i32;
                let (_, m) = layout_formula_for_style(engine, &block.formula, &fstyle);
                Vec2::new(m.size.x.min(max_width), m.size.y + 8.0)
            }
            RichBlockKind::Divider => Vec2::new(max_width.min(240.0), 10.0),
            RichBlockKind::Quote => {
                let inner_w = (max_width - 18.0).max(60.0);
                let mut qh = 8.0;
                let mut qw = 0.0_f32;
                if !block.text.text.is_empty() {
                    let t = est_text(&block.text.text.text, inner_w, style.base_font_size);
                    qw = qw.max(t.x);
                    qh += t.y;
                }
                if !block.blocks.is_empty() {
                    let c = estimate_blocks(engine, &block.blocks, inner_w, style, depth + 1);
                    qw = qw.max(c.x);
                    qh += c.y;
                }
                if !block.caption.text.is_empty() {
                    let cap = est_text(
                        &block.caption.text.text,
                        inner_w,
                        style.base_font_size * 0.88,
                    );
                    qw = qw.max(cap.x);
                    qh += cap.y + 4.0;
                }
                Vec2::new((qw + 18.0).min(max_width), qh)
            }
            RichBlockKind::Details => {
                let title = est_text(
                    &block.text.text.text,
                    max_width - 22.0,
                    style.base_font_size,
                );
                let body = if block.open {
                    estimate_blocks(
                        engine,
                        &block.blocks,
                        (max_width - 16.0).max(60.0),
                        style,
                        depth + 1,
                    )
                } else {
                    Vec2::ZERO
                };
                Vec2::new(
                    (title.x + 22.0).max(body.x + 16.0).min(max_width),
                    title.y + 8.0 + if body.y > 0.0 { body.y + 4.0 } else { 0.0 },
                )
            }
            RichBlockKind::List => {
                let inner_w = (max_width - 24.0).max(60.0);
                let mut lh_acc = 0.0_f32;
                let mut lw = 0.0_f32;
                for item in &block.list_items {
                    if !item.text.text.is_empty() || item.blocks.is_empty() {
                        let t = est_text(&item.text.text.text, inner_w, style.base_font_size);
                        lw = lw.max(t.x + 24.0);
                        lh_acc += t.y + 3.0;
                    }
                    if !item.blocks.is_empty() {
                        let c = estimate_blocks(engine, &item.blocks, inner_w, style, depth + 1);
                        lw = lw.max(c.x + 24.0);
                        lh_acc += c.y + 3.0;
                    }
                }
                Vec2::new(lw.min(max_width), lh_acc.max(line_h))
            }
            RichBlockKind::Table => {
                let title_h = if block.text.text.is_empty() {
                    0.0
                } else {
                    line_h + 4.0
                };
                let row_h = if block.compact { 22.0 } else { 28.0 };
                let rows_h = block.table_rows.len() as f32 * row_h;
                Vec2::new(max_width, title_h + rows_h + 8.0)
            }
            RichBlockKind::ButtonRow => Vec2::new(max_width.min(280.0), 34.0),
            RichBlockKind::Anchor | RichBlockKind::Unsupported => Vec2::ZERO,
            _ => {
                let lbl_w = (24.0 * char_w).min(max_width);
                Vec2::new(lbl_w, 34.0)
            }
        };
        if sz.y > 0.0 {
            w = w.max(sz.x);
            h += sz.y;
            if idx + 1 < blocks.len() {
                h += style.block_spacing;
            }
        }
    }
    Vec2::new(w.min(max_width), h)
}

fn measure_blocks(
    painter: &Painter,
    engine: &mut MathLayoutEngine,
    blocks: &[RichBlock],
    max_width: f32,
    style: &RichPageViewStyle,
    depth: usize,
) -> Vec2 {
    if depth > 12 || blocks.is_empty() {
        return Vec2::ZERO;
    }
    let mut w = 0.0_f32;
    let mut h = 0.0_f32;
    for (idx, block) in blocks.iter().enumerate() {
        let sz = measure_single_block(painter, engine, block, max_width, style, depth);
        if sz.y > 0.0 {
            w = w.max(sz.x);
            h += sz.y;
            if idx + 1 < blocks.len() {
                h += style.block_spacing;
            }
        }
    }
    Vec2::new(w.min(max_width), h)
}

fn measure_single_block(
    painter: &Painter,
    engine: &mut MathLayoutEngine,
    block: &RichBlock,
    max_width: f32,
    style: &RichPageViewStyle,
    depth: usize,
) -> Vec2 {
    match block.kind {
        RichBlockKind::Heading => {
            let fsz = heading_font_size(block.heading_level, style.base_font_size);
            measure_inline_rich_text(
                painter,
                engine,
                &block.text.text,
                fsz,
                style.text_fg,
                style,
                max_width,
                true,
                false,
            )
        }
        RichBlockKind::Paragraph | RichBlockKind::Thinking => measure_inline_rich_text(
            painter,
            engine,
            &block.text.text,
            style.base_font_size,
            style.text_fg,
            style,
            max_width,
            false,
            false,
        ),
        RichBlockKind::Footer => measure_inline_rich_text(
            painter,
            engine,
            &block.text.text,
            style.base_font_size * 0.88,
            style.muted_fg,
            style,
            max_width,
            false,
            false,
        ),
        RichBlockKind::Code => {
            let inner_w = (max_width - 20.0).max(60.0);
            let galley = painter.layout(
                block.text.text.text.clone(),
                FontId::monospace((style.base_font_size * 0.9).max(10.0)),
                style.code_fg,
                inner_w,
            );
            let header_h = if block.language.is_empty() { 0.0 } else { 18.0 };
            Vec2::new(
                (galley.size().x + 20.0).min(max_width).max(120.0),
                galley.size().y.max(style.base_font_size * 1.2) + header_h + 16.0,
            )
        }
        RichBlockKind::Math => {
            let mut fstyle = FormulaWidgetStyle::display(style.base_font_size * 1.1, style.text_fg);
            fstyle.max_width = max_width.round() as i32;
            let (_, m) = layout_formula_for_style(engine, &block.formula, &fstyle);
            Vec2::new(m.size.x.min(max_width), m.size.y + 6.0)
        }
        RichBlockKind::Divider => Vec2::new(max_width, 10.0),
        RichBlockKind::Quote => {
            let inner_w = (max_width - 18.0).max(60.0);
            let mut qw = 0.0_f32;
            let mut qh = 8.0_f32;
            if !block.text.text.is_empty() {
                let t = measure_inline_rich_text(
                    painter,
                    engine,
                    &block.text.text,
                    style.base_font_size,
                    style.text_fg,
                    style,
                    inner_w,
                    false,
                    block.pullquote,
                );
                qw = qw.max(t.x);
                qh += t.y;
            }
            if !block.blocks.is_empty() {
                let c = measure_blocks(painter, engine, &block.blocks, inner_w, style, depth + 1);
                qw = qw.max(c.x);
                qh += c.y;
            }
            if !block.caption.text.is_empty() {
                let cap = measure_inline_rich_text(
                    painter,
                    engine,
                    &block.caption.text,
                    style.base_font_size * 0.88,
                    style.muted_fg,
                    style,
                    inner_w,
                    false,
                    true,
                );
                qw = qw.max(cap.x);
                qh += cap.y + 4.0;
            }
            Vec2::new((qw + 18.0).min(max_width), qh)
        }
        RichBlockKind::Details => {
            let title = measure_inline_rich_text(
                painter,
                engine,
                &block.text.text,
                style.base_font_size,
                style.text_fg,
                style,
                (max_width - 22.0).max(60.0),
                true,
                false,
            );
            let body = if block.open {
                measure_blocks(
                    painter,
                    engine,
                    &block.blocks,
                    (max_width - 16.0).max(60.0),
                    style,
                    depth + 1,
                )
            } else {
                Vec2::ZERO
            };
            Vec2::new(
                (title.x + 22.0).max(body.x + 16.0).min(max_width),
                title.y + 8.0 + if body.y > 0.0 { body.y + 4.0 } else { 0.0 },
            )
        }
        RichBlockKind::List => {
            let inner_w = (max_width - 26.0).max(60.0);
            let mut lw = 0.0_f32;
            let mut lh = 0.0_f32;
            for item in &block.list_items {
                if !item.text.text.is_empty() || item.blocks.is_empty() {
                    let t = measure_inline_rich_text(
                        painter,
                        engine,
                        &item.text.text,
                        style.base_font_size,
                        style.text_fg,
                        style,
                        inner_w,
                        false,
                        false,
                    );
                    lw = lw.max(t.x + 26.0);
                    lh += t.y + 3.0;
                }
                if !item.blocks.is_empty() {
                    let c =
                        measure_blocks(painter, engine, &item.blocks, inner_w, style, depth + 1);
                    lw = lw.max(c.x + 26.0);
                    lh += c.y + 3.0;
                }
            }
            Vec2::new(lw.min(max_width), lh)
        }
        RichBlockKind::Table => {
            let title_h = if block.text.text.is_empty() {
                0.0
            } else {
                measure_inline_rich_text(
                    painter,
                    engine,
                    &block.text.text,
                    style.base_font_size,
                    style.text_fg,
                    style,
                    max_width,
                    true,
                    false,
                )
                .y + 4.0
            };
            let cols = block
                .table_rows
                .iter()
                .map(|r| {
                    r.cells
                        .iter()
                        .map(|c| c.colspan.max(1) as usize)
                        .sum::<usize>()
                })
                .max()
                .unwrap_or(1)
                .max(1);
            let col_w = ((max_width - 8.0) / cols as f32).max(40.0);
            let cell_pad = if block.compact { 6.0 } else { 10.0 };
            let mut rows_h = 0.0_f32;
            for row in &block.table_rows {
                let mut rh: f32 = if block.compact { 22.0 } else { 26.0 };
                for cell in &row.cells {
                    let cw = (col_w * cell.colspan.max(1) as f32 - cell_pad).max(24.0);
                    let m = measure_inline_rich_text(
                        painter,
                        engine,
                        &cell.text.text,
                        style.base_font_size * 0.92,
                        style.text_fg,
                        style,
                        cw,
                        cell.header,
                        false,
                    );
                    rh = rh.max(m.y + cell_pad);
                }
                rows_h += rh;
            }
            Vec2::new(max_width, title_h + rows_h + 4.0)
        }
        RichBlockKind::ButtonRow => Vec2::new(max_width, 32.0),
        RichBlockKind::Anchor | RichBlockKind::Unsupported => Vec2::ZERO,
        _ => Vec2::new(max_width.min(260.0), 34.0),
    }
}

/// Paints an entire [`RichPage`] inside `ui` with blocks and baseline-aligned formulas.
pub fn paint_rich_page(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    engine: &mut MathLayoutEngine,
    page: &RichPage,
    max_width: f32,
    style: &RichPageViewStyle,
) -> Vec<RichPageViewEvent> {
    let mut events = Vec::new();
    let base_id = ui.scope_id().with(id_salt);
    ui.vertical(|ui| {
        ui.set_max_width(max_width.max(80.0));
        ui.spacing_mut().item_spacing.y = style.block_spacing;
        paint_blocks(
            ui,
            base_id,
            engine,
            &page.blocks,
            max_width.max(80.0),
            style,
            0,
            &mut events,
        );
    });
    events
}

/// Paints a `tg_iv::model::Page` natively (including Instant View math blocks & inline formulas)
/// by converting through [`rich_page_from_model_page`] and painting with [`paint_rich_page`].
pub fn paint_iv_model_page(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    engine: &mut MathLayoutEngine,
    page: &tg_iv::Page,
    max_width: f32,
    style: &RichPageViewStyle,
) -> Vec<RichPageViewEvent> {
    let rich = rich_page_from_model_page(page);
    paint_rich_page(ui, id_salt, engine, &rich, max_width, style)
}

#[allow(clippy::too_many_arguments)]
fn paint_blocks(
    ui: &mut Ui,
    base_id: Id,
    engine: &mut MathLayoutEngine,
    blocks: &[RichBlock],
    max_width: f32,
    style: &RichPageViewStyle,
    depth: usize,
    events: &mut Vec<RichPageViewEvent>,
) {
    if depth > 12 {
        return;
    }
    for (idx, block) in blocks.iter().enumerate() {
        let blk_id = base_id.with((depth, idx));
        paint_single_block(ui, blk_id, engine, block, max_width, style, depth, events);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_single_block(
    ui: &mut Ui,
    blk_id: Id,
    engine: &mut MathLayoutEngine,
    block: &RichBlock,
    max_width: f32,
    style: &RichPageViewStyle,
    depth: usize,
    events: &mut Vec<RichPageViewEvent>,
) {
    match block.kind {
        RichBlockKind::Heading => {
            let fsz = heading_font_size(block.heading_level, style.base_font_size);
            let (_, clicked) = paint_inline_rich_text(
                ui,
                blk_id,
                engine,
                &block.text.text,
                fsz,
                style.text_fg,
                style,
                max_width,
                true,
                false,
            );
            if let Some(url) = clicked {
                events.push(RichPageViewEvent::OpenLink(url));
            }
        }
        RichBlockKind::Paragraph | RichBlockKind::Thinking => {
            let (_, clicked) = paint_inline_rich_text(
                ui,
                blk_id,
                engine,
                &block.text.text,
                style.base_font_size,
                style.text_fg,
                style,
                max_width,
                false,
                false,
            );
            if let Some(url) = clicked {
                events.push(RichPageViewEvent::OpenLink(url));
            }
        }
        RichBlockKind::Footer => {
            let (_, clicked) = paint_inline_rich_text(
                ui,
                blk_id,
                engine,
                &block.text.text,
                style.base_font_size * 0.88,
                style.muted_fg,
                style,
                max_width,
                false,
                false,
            );
            if let Some(url) = clicked {
                events.push(RichPageViewEvent::OpenLink(url));
            }
        }
        RichBlockKind::Code => {
            egui::Frame::NONE
                .fill(style.code_bg)
                .stroke(Stroke::new(1.0, style.border))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(10, 7))
                .show(ui, |ui| {
                    ui.set_max_width((max_width - 20.0).max(60.0));
                    if !block.language.is_empty() {
                        ui.label(
                            egui::RichText::new(&block.language)
                                .size((style.base_font_size * 0.78).max(9.5))
                                .color(style.muted_fg),
                        );
                    }
                    ui.label(
                        egui::RichText::new(&block.text.text.text)
                            .font(FontId::monospace((style.base_font_size * 0.9).max(10.0)))
                            .color(style.code_fg),
                    );
                });
        }
        RichBlockKind::Math => {
            let mut fstyle = FormulaWidgetStyle::display(style.base_font_size * 1.1, style.text_fg);
            fstyle.max_width = (max_width.round() as i32).max(64);
            let (rendered, metrics) = layout_formula_for_style(engine, &block.formula, &fstyle);
            let box_w = max_width.max(metrics.size.x);
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(box_w, metrics.size.y + 4.0), Sense::hover());
            if ui.is_rect_visible(rect) {
                let left_x = rect.left() + ((rect.width() - metrics.size.x) * 0.5).max(0.0);
                let baseline_y = rect.top() + 2.0 + metrics.baseline_y;
                paint_formula_at_baseline(
                    ui.painter(),
                    Pos2::new(left_x, baseline_y),
                    &rendered,
                    &fstyle,
                );
            }
        }
        RichBlockKind::Divider => {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(max_width, 10.0), Sense::hover());
            let y = rect.center().y;
            ui.painter().line_segment(
                [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
                Stroke::new(1.0, style.border),
            );
        }
        RichBlockKind::Quote => {
            let resp = egui::Frame::NONE
                .fill(style.quote_bg)
                .corner_radius(CornerRadius::same(5))
                .inner_margin(egui::Margin {
                    left: 12,
                    right: 8,
                    top: 6,
                    bottom: 6,
                })
                .show(ui, |ui| {
                    let inner_w = (max_width - 20.0).max(60.0);
                    ui.set_max_width(inner_w);
                    if !block.text.text.is_empty() {
                        let (_, clicked) = paint_inline_rich_text(
                            ui,
                            blk_id.with("qt"),
                            engine,
                            &block.text.text,
                            style.base_font_size,
                            style.text_fg,
                            style,
                            inner_w,
                            false,
                            block.pullquote,
                        );
                        if let Some(url) = clicked {
                            events.push(RichPageViewEvent::OpenLink(url));
                        }
                    }
                    if !block.blocks.is_empty() {
                        paint_blocks(
                            ui,
                            blk_id.with("qb"),
                            engine,
                            &block.blocks,
                            inner_w,
                            style,
                            depth + 1,
                            events,
                        );
                    }
                    if !block.caption.text.is_empty() {
                        let (_, clicked) = paint_inline_rich_text(
                            ui,
                            blk_id.with("qc"),
                            engine,
                            &block.caption.text,
                            style.base_font_size * 0.88,
                            style.muted_fg,
                            style,
                            inner_w,
                            false,
                            true,
                        );
                        if let Some(url) = clicked {
                            events.push(RichPageViewEvent::OpenLink(url));
                        }
                    }
                })
                .response;
            let bar = Rect::from_min_size(resp.rect.min, Vec2::new(3.0, resp.rect.height()));
            ui.painter()
                .rect_filled(bar, CornerRadius::same(2), style.accent);
        }
        RichBlockKind::Details => {
            let state_id = blk_id.with("details_open");
            let mut open = ui
                .ctx()
                .data_mut(|d| *d.get_temp_mut_or(state_id, block.open));
            ui.horizontal(|ui| {
                let dir = if open {
                    tg_app_core::ui_icons::ArrowDir::Down
                } else {
                    tg_app_core::ui_icons::ArrowDir::Right
                };
                if tg_app_core::ui_icons::small_icon_button(
                    ui,
                    if open {
                        "Collapse details"
                    } else {
                        "Expand details"
                    },
                    true,
                    move |p, c, s, col| {
                        tg_app_core::ui_icons::paint_triangle(p, c, s * 0.78, dir, col);
                    },
                )
                .clicked()
                {
                    open = !open;
                    ui.ctx().data_mut(|d| d.insert_temp(state_id, open));
                }
                let (_, clicked) = paint_inline_rich_text(
                    ui,
                    blk_id.with("dt"),
                    engine,
                    &block.text.text,
                    style.base_font_size,
                    style.text_fg,
                    style,
                    (max_width - 24.0).max(60.0),
                    true,
                    false,
                );
                if let Some(url) = clicked {
                    events.push(RichPageViewEvent::OpenLink(url));
                }
            });
            if open && !block.blocks.is_empty() {
                ui.indent(blk_id.with("db"), |ui| {
                    paint_blocks(
                        ui,
                        blk_id.with("dbc"),
                        engine,
                        &block.blocks,
                        (max_width - 18.0).max(60.0),
                        style,
                        depth + 1,
                        events,
                    );
                });
            }
        }
        RichBlockKind::List => {
            let inner_w = (max_width - 26.0).max(60.0);
            for (item_idx, item) in block.list_items.iter().enumerate() {
                ui.horizontal_top(|ui| {
                    ui.allocate_ui(Vec2::new(22.0, style.base_font_size * 1.3), |ui| match item
                        .task_state
                    {
                        RichTaskState::Checked | RichTaskState::Unchecked => {
                            let (r, _) = ui.allocate_exact_size(
                                Vec2::splat(style.base_font_size),
                                Sense::hover(),
                            );
                            tg_app_core::ui_icons::paint_checkbox_glyph(
                                ui.painter(),
                                r.center(),
                                style.base_font_size * 0.9,
                                item.task_state == RichTaskState::Checked,
                                style.accent,
                            );
                        }
                        RichTaskState::None => {
                            let marker = match block.list_kind {
                                RichListKind::Bullet => "•".to_owned(),
                                RichListKind::Ordered => format_ordered_marker(block, item_idx),
                            };
                            ui.label(
                                egui::RichText::new(marker)
                                    .size(style.base_font_size)
                                    .color(style.accent),
                            );
                        }
                    });
                    ui.vertical(|ui| {
                        if !item.text.text.is_empty() || item.blocks.is_empty() {
                            let (_, clicked) = paint_inline_rich_text(
                                ui,
                                blk_id.with(("li", item_idx)),
                                engine,
                                &item.text.text,
                                style.base_font_size,
                                style.text_fg,
                                style,
                                inner_w,
                                false,
                                false,
                            );
                            if let Some(url) = clicked {
                                events.push(RichPageViewEvent::OpenLink(url));
                            }
                        }
                        if !item.blocks.is_empty() {
                            paint_blocks(
                                ui,
                                blk_id.with(("lib", item_idx)),
                                engine,
                                &item.blocks,
                                inner_w,
                                style,
                                depth + 1,
                                events,
                            );
                        }
                    });
                });
            }
        }
        RichBlockKind::Table => {
            if !block.text.text.is_empty() {
                let (_, clicked) = paint_inline_rich_text(
                    ui,
                    blk_id.with("tt"),
                    engine,
                    &block.text.text,
                    style.base_font_size,
                    style.text_fg,
                    style,
                    max_width,
                    true,
                    false,
                );
                if let Some(url) = clicked {
                    events.push(RichPageViewEvent::OpenLink(url));
                }
            }
            paint_rich_table(ui, blk_id, engine, block, max_width, style, events);
        }
        RichBlockKind::ButtonRow => {
            ui.horizontal_wrapped(|ui| {
                for (b_idx, btn) in block.buttons.iter().enumerate() {
                    let fill = button_fill_color(btn.color, style);
                    let fg = if btn.color == RichButtonColor::Normal {
                        style.text_fg
                    } else {
                        Color32::WHITE
                    };
                    let resp = ui.add(
                        egui::Button::new(
                            egui::RichText::new(&btn.text.text.text)
                                .size(style.base_font_size * 0.92)
                                .color(fg),
                        )
                        .fill(fill)
                        .corner_radius(CornerRadius::same(6)),
                    );
                    if resp.clicked()
                        && btn.button_type == tg_iv::rich_page::RichButtonType::Url
                        && let Ok(url) = std::str::from_utf8(&btn.data)
                    {
                        events.push(RichPageViewEvent::OpenLink(url.to_owned()));
                    }
                    let _ = b_idx;
                }
            });
        }
        RichBlockKind::Anchor | RichBlockKind::Unsupported => {}
        _ => {
            use tg_ui::tg_style::st;
            let (icon_opt, summary) = match block.kind {
                RichBlockKind::Photo => (
                    Some(st::menu_icon_photo),
                    format!("Photo #{}", block.photo_id),
                ),
                RichBlockKind::Video => (
                    Some(st::menu_icon_video_chat),
                    format!("Video #{}", block.document_id),
                ),
                RichBlockKind::Audio => (
                    Some(st::menu_icon_sound_select),
                    if block.audio_title.is_empty() {
                        format!("Audio #{}", block.document_id)
                    } else {
                        format!("{} — {}", block.audio_performer, block.audio_title)
                    },
                ),
                RichBlockKind::File => (
                    Some(st::menu_icon_file),
                    if block.file_name.is_empty() {
                        format!("File #{}", block.document_id)
                    } else {
                        block.file_name.clone()
                    },
                ),
                RichBlockKind::Map => (
                    Some(st::media_menu_icon_location),
                    format!("Map ({:.4}, {:.4})", block.latitude, block.longitude),
                ),
                RichBlockKind::GroupedMedia => (
                    Some(st::menu_icon_photo),
                    format!("Media gallery ({} items)", block.media_items.len()),
                ),
                RichBlockKind::AuthorDate => (None, format!("By {}", block.text.text.text)),
                RichBlockKind::Channel => {
                    (Some(st::menu_icon_channel), block.channel_title.clone())
                }
                _ => (None, format!("[{:?}]", block.kind)),
            };
            egui::Frame::NONE
                .fill(style.code_bg)
                .stroke(Stroke::new(1.0, style.border))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(8, 5))
                .show(ui, |ui| {
                    let rt = egui::RichText::new(summary)
                        .size(style.base_font_size * 0.9)
                        .color(style.muted_fg);
                    if let Some(icon) = icon_opt {
                        tg_app_core::ui_icons::inline_icon_label(
                            ui,
                            14.0,
                            |p, c, s| {
                                tg_app_core::ui_icons::paint_style_tinted(
                                    p,
                                    Rect::from_center_size(c, Vec2::splat(s)),
                                    icon,
                                    style.muted_fg,
                                );
                            },
                            rt,
                        );
                    } else {
                        ui.label(rt);
                    }
                });
        }
    }
}

fn paint_rich_table(
    ui: &mut Ui,
    blk_id: Id,
    engine: &mut MathLayoutEngine,
    block: &RichBlock,
    max_width: f32,
    style: &RichPageViewStyle,
    events: &mut Vec<RichPageViewEvent>,
) {
    let cols = block
        .table_rows
        .iter()
        .map(|r| {
            r.cells
                .iter()
                .map(|c| c.colspan.max(1) as usize)
                .sum::<usize>()
        })
        .max()
        .unwrap_or(1)
        .max(1);
    let col_w = ((max_width - 2.0) / cols as f32).max(40.0);
    let cell_pad = if block.compact { 4.0 } else { 6.0 };

    egui::Frame::NONE
        .stroke(if block.bordered {
            Stroke::new(1.0, style.border)
        } else {
            Stroke::NONE
        })
        .corner_radius(CornerRadius::same(4))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            for (r_idx, row) in block.table_rows.iter().enumerate() {
                // Pre-measure row height across cells.
                let mut row_h: f32 = if block.compact { 22.0 } else { 26.0 };
                for cell in &row.cells {
                    let cw = (col_w * cell.colspan.max(1) as f32 - cell_pad * 2.0).max(20.0);
                    let m = measure_inline_rich_text(
                        ui.painter(),
                        engine,
                        &cell.text.text,
                        style.base_font_size * 0.92,
                        style.text_fg,
                        style,
                        cw,
                        cell.header,
                        false,
                    );
                    row_h = row_h.max(m.y + cell_pad * 2.0);
                }

                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    for (c_idx, cell) in row.cells.iter().enumerate() {
                        let cw = col_w * cell.colspan.max(1) as f32;
                        let (cell_rect, _) =
                            ui.allocate_exact_size(Vec2::new(cw, row_h), Sense::hover());
                        let fill = if cell.header {
                            style.table_header_bg
                        } else if block.striped && r_idx % 2 == 1 {
                            style.table_stripe_bg
                        } else {
                            Color32::TRANSPARENT
                        };
                        if fill != Color32::TRANSPARENT {
                            ui.painter()
                                .rect_filled(cell_rect, CornerRadius::ZERO, fill);
                        }
                        if block.bordered {
                            ui.painter().rect_stroke(
                                cell_rect,
                                CornerRadius::ZERO,
                                Stroke::new(0.75, style.border),
                                StrokeKind::Inside,
                            );
                        }
                        let inner_rect = cell_rect.shrink(cell_pad);
                        let align = match cell.alignment {
                            RichTableAlignment::Left => egui::Align::Min,
                            RichTableAlignment::Center => egui::Align::Center,
                            RichTableAlignment::Right => egui::Align::Max,
                        };
                        ui.scope_builder(
                            egui::UiBuilder::new()
                                .max_rect(inner_rect)
                                .layout(egui::Layout::top_down(align)),
                            |ui| {
                                let (_, clicked) = paint_inline_rich_text(
                                    ui,
                                    blk_id.with(("tc", r_idx, c_idx)),
                                    engine,
                                    &cell.text.text,
                                    style.base_font_size * 0.92,
                                    style.text_fg,
                                    style,
                                    inner_rect.width().max(16.0),
                                    cell.header,
                                    false,
                                );
                                if let Some(url) = clicked {
                                    events.push(RichPageViewEvent::OpenLink(url));
                                }
                            },
                        );
                    }
                });
            }
        });
}
