//! Rich text layout, rendering, elision, selection, and hit-testing over `egui`.
//!
//! Supports both [`tg_text::TextWithEntities`] (with UTF-16 entity offsets converted via
//! `tg_text::OffsetMapper`) and the legacy [`RichText`] builder API:
//! - Inline styles: bold, semibold, italic, bold+italic, underline, strikethrough,
//!   inline monospace `code`, subscript, superscript, marked highlight, spoilers with
//!   shimmering particles, and custom emoji placeholders.
//! - Block elements: `<pre>` code blocks with tinted background, left accent bar, optional
//!   language header and copy button; `<blockquote>` with left accent bar, tinted background,
//!   quote glyph, and collapsible expand/collapse behavior.
//! - Clickable entities: URLs, custom URLs, emails, `@mentions`, `MentionName`, `#hashtags`,
//!   `$cashtags`, `/bot_commands`, phone numbers, bank cards, TON addresses, media timestamps.
//! - Multi-line wrapping, `max_lines` with `"…"` elision, link/character hit-testing, and
//!   drag/double-click UTF-16 text selection.
//!
//! Port of `Telegram/lib_ui/ui/text/text.cpp`, `Telegram/lib_ui/ui/text/text_renderer.cpp`,
//! and `Telegram/lib_ui/ui/text/text_quote_paint.cpp`.

use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Galley, Id, Pos2, Rect, Sense, Shape,
    Stroke, Ui, Vec2,
};
use tg_style::{FontFlags, font::SUB_SUPER_MULTIPLIER, st};
use tg_text::{
    EntityInText, EntityType, OffsetMapper, SyntaxTokenKind, TextSelection, TextWithEntities,
    highlight_code_spans,
};

use crate::anim::with_alpha;
use crate::custom_emoji::{
    CustomEmojiPaintContext, CustomEmojiRenderer, has_registered_custom_emoji,
    paint_registered_custom_emoji,
};
use crate::effects::merge_spoiler_rects;
use crate::fonts::font_id_for_flags;
use crate::icons::paint_style_icon;
use crate::style::{LinkUnderline, QuoteStyle, TextPalette, TextStyle, px};

mod prepared;
#[cfg(test)]
mod spoiler_tests;

pub use prepared::{
    PreparedEntityText, PreparedRichText, fonts_epoch, note_text_preparation,
    render_prepared_rich_text, render_prepared_rich_text_selected,
    render_prepared_rich_text_with_reveal, render_prepared_text_with_entities,
    text_preparation_count,
};

// ---------------------------------------------------------------------------
// Legacy RichText API (kept 100% source-compatible)
// ---------------------------------------------------------------------------

/// Formatting entity kind for a byte range inside [`RichText`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextEntityKind {
    Bold,
    Italic,
    Underline,
    Strikethrough,
    Monospace,
    Link(String),
    Spoiler,
}

/// Byte-range formatting span within [`RichText::text`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSpan {
    /// Start UTF-8 byte offset (inclusive).
    pub start: usize,
    /// End UTF-8 byte offset (exclusive).
    pub end: usize,
    /// Formatting attribute.
    pub kind: TextEntityKind,
}

/// Styled text model (plain string + non-exclusive formatting spans).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RichText {
    pub text: String,
    pub spans: Vec<TextSpan>,
}

impl RichText {
    /// Create empty rich text.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create plain unstyled text.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            spans: Vec::new(),
        }
    }

    /// Convert a [`tg_text::TextWithEntities`] into [`RichText`], mapping UTF-16 offsets
    /// via [`tg_text::OffsetMapper`].
    #[must_use]
    pub fn from_text_with_entities(src: &TextWithEntities) -> Self {
        let mapper = OffsetMapper::new(&src.text);
        let mut out = Self::plain(src.text.clone());
        for entity in &src.entities {
            let Some(range) = entity_utf8_range(entity, &mapper) else {
                continue;
            };
            let slice = &src.text[range.clone()];
            if let Some(target) =
                crate::entity_link::entity_link_target(entity.entity_type(), entity.data(), slice)
            {
                out.spans.push(TextSpan {
                    start: range.start,
                    end: range.end,
                    kind: TextEntityKind::Link(target),
                });
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
                EntityType::Url
                | EntityType::Mention
                | EntityType::Hashtag
                | EntityType::Cashtag
                | EntityType::BotCommand
                | EntityType::BankCard
                | EntityType::TonAddress
                | EntityType::MediaTimestamp => TextEntityKind::Link(slice.to_owned()),
                EntityType::Email => TextEntityKind::Link(format!("mailto:{slice}")),
                EntityType::Phone => TextEntityKind::Link(format!("tel:{slice}")),
                EntityType::MentionName => TextEntityKind::Link(entity.data().to_owned()),
                _ => continue,
            };
            out.spans.push(TextSpan {
                start: range.start,
                end: range.end,
                kind,
            });
        }
        out
    }

    /// Convert this [`RichText`] into a [`tg_text::TextWithEntities`] with UTF-16 entity offsets.
    #[must_use]
    pub fn to_text_with_entities(&self) -> TextWithEntities {
        let mapper = OffsetMapper::new(&self.text);
        let mut entities = Vec::with_capacity(self.spans.len());
        for sp in &self.spans {
            let s = clamp_char_boundary(&self.text, sp.start.min(self.text.len()));
            let e = clamp_char_boundary(&self.text, sp.end.min(self.text.len()));
            if e <= s {
                continue;
            }
            let Some((off, len)) = mapper.utf8_range_to_utf16(s..e) else {
                continue;
            };
            let (off_i32, len_i32) = (off as i32, len as i32);
            let entity = match &sp.kind {
                TextEntityKind::Bold => EntityInText::new(EntityType::Bold, off_i32, len_i32),
                TextEntityKind::Italic => EntityInText::new(EntityType::Italic, off_i32, len_i32),
                TextEntityKind::Underline => {
                    EntityInText::new(EntityType::Underline, off_i32, len_i32)
                }
                TextEntityKind::Strikethrough => {
                    EntityInText::new(EntityType::StrikeOut, off_i32, len_i32)
                }
                TextEntityKind::Monospace => EntityInText::new(EntityType::Code, off_i32, len_i32),
                TextEntityKind::Link(url) => {
                    EntityInText::with_data(EntityType::CustomUrl, off_i32, len_i32, url.clone())
                }
                TextEntityKind::Spoiler => EntityInText::new(EntityType::Spoiler, off_i32, len_i32),
            };
            entities.push(entity);
        }
        TextWithEntities::with_entities(self.text.clone(), entities)
    }

    /// Append plain text.
    pub fn push_plain(mut self, s: &str) -> Self {
        self.text.push_str(s);
        self
    }

    /// Append text with a single entity kind.
    pub fn push_styled(mut self, s: &str, kind: TextEntityKind) -> Self {
        let start = self.text.len();
        self.text.push_str(s);
        let end = self.text.len();
        if end > start {
            self.spans.push(TextSpan { start, end, kind });
        }
        self
    }

    /// Append bold text.
    pub fn push_bold(self, s: &str) -> Self {
        self.push_styled(s, TextEntityKind::Bold)
    }

    /// Append italic text.
    pub fn push_italic(self, s: &str) -> Self {
        self.push_styled(s, TextEntityKind::Italic)
    }

    /// Append monospace inline code text.
    pub fn push_code(self, s: &str) -> Self {
        self.push_styled(s, TextEntityKind::Monospace)
    }

    /// Append a clickable hyperlink.
    pub fn push_link(self, label: &str, url: impl Into<String>) -> Self {
        self.push_styled(label, TextEntityKind::Link(url.into()))
    }

    /// Append a hidden spoiler span (revealed on click).
    pub fn push_spoiler(self, s: &str) -> Self {
        self.push_styled(s, TextEntityKind::Spoiler)
    }

    /// Append strikethrough text.
    pub fn push_strike(self, s: &str) -> Self {
        self.push_styled(s, TextEntityKind::Strikethrough)
    }

    /// Append underlined text.
    pub fn push_underline(self, s: &str) -> Self {
        self.push_styled(s, TextEntityKind::Underline)
    }

    /// Add an arbitrary span with safe bounds clamping.
    pub fn add_span(&mut self, start: usize, end: usize, kind: TextEntityKind) {
        let len = self.text.len();
        let s = clamp_char_boundary(&self.text, start.min(len));
        let e = clamp_char_boundary(&self.text, end.min(len));
        if e > s {
            self.spans.push(TextSpan {
                start: s,
                end: e,
                kind,
            });
        }
    }

    /// Whether the text has no characters.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// Result returned by [`render_rich_text`].
#[derive(Debug, Clone)]
pub struct RichTextResponse {
    /// Clicked link URL, if any.
    pub clicked_link: Option<String>,
    /// Whether any spoiler span was toggled/revealed this frame.
    pub spoiler_toggled: bool,
    /// Bounding rect occupied by the rendered text.
    pub rect: Rect,
}

impl Default for RichTextResponse {
    fn default() -> Self {
        Self {
            clicked_link: None,
            spoiler_toggled: false,
            rect: Rect::NOTHING,
        }
    }
}

#[derive(Debug, Clone)]
struct LegacySegment {
    start: usize,
    end: usize,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    mono: bool,
    link: Option<String>,
    spoiler: bool,
}

fn split_legacy_segments(rich: &RichText) -> Vec<LegacySegment> {
    if rich.text.is_empty() {
        return Vec::new();
    }
    let len = rich.text.len();
    let mut cuts = BTreeSet::new();
    cuts.insert(0);
    cuts.insert(len);
    for sp in &rich.spans {
        cuts.insert(clamp_char_boundary(&rich.text, sp.start.min(len)));
        cuts.insert(clamp_char_boundary(&rich.text, sp.end.min(len)));
    }
    let points: Vec<usize> = cuts.into_iter().collect();
    let mut segments = Vec::with_capacity(points.len().saturating_sub(1));
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b <= a {
            continue;
        }
        let mut seg = LegacySegment {
            start: a,
            end: b,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            mono: false,
            link: None,
            spoiler: false,
        };
        for sp in &rich.spans {
            if sp.start <= a && sp.end >= b {
                match &sp.kind {
                    TextEntityKind::Bold => seg.bold = true,
                    TextEntityKind::Italic => seg.italic = true,
                    TextEntityKind::Underline => seg.underline = true,
                    TextEntityKind::Strikethrough => seg.strike = true,
                    TextEntityKind::Monospace => seg.mono = true,
                    TextEntityKind::Link(url) => seg.link = Some(url.clone()),
                    TextEntityKind::Spoiler => seg.spoiler = true,
                }
            }
        }
        segments.push(seg);
    }
    segments
}

fn clamp_char_boundary(s: &str, mut idx: usize) -> usize {
    idx = idx.min(s.len());
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Build an egui [`LayoutJob`] from [`RichText`], `style`, and `palette`.
pub fn build_layout_job(
    rich: &RichText,
    style: &TextStyle,
    palette: &TextPalette,
    base_color: Color32,
    wrap_width: f32,
    hovered_link: Option<&str>,
    spoilers_revealed: bool,
) -> LayoutJob {
    let segments = split_legacy_segments(rich);
    layout_job_for_segments(
        &rich.text,
        &segments,
        style,
        palette,
        base_color,
        wrap_width,
        hovered_link,
        spoilers_revealed,
    )
}

/// [`build_layout_job`] for `segments` split from `text` earlier
/// ([`split_legacy_segments`]): a prepared text splits once and builds a job per hover state.
#[allow(clippy::too_many_arguments)]
fn layout_job_for_segments(
    text: &str,
    segments: &[LegacySegment],
    style: &TextStyle,
    palette: &TextPalette,
    base_color: Color32,
    wrap_width: f32,
    hovered_link: Option<&str>,
    spoilers_revealed: bool,
) -> LayoutJob {
    prepared::note_text_preparation();
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width.max(1.0);

    let base_size = style.font.size;
    let base_line_height = (style.line_height > 0.0).then_some(style.line_height);

    for seg in segments {
        let slice = &text[seg.start..seg.end];
        let mut flags = style.font.to_tg_flags();
        if seg.mono {
            flags = FontFlags::MONOSPACE;
        } else {
            if seg.bold {
                flags = flags.union(FontFlags::BOLD);
            }
            if seg.italic {
                flags = flags.union(FontFlags::ITALIC);
            }
        }
        let font_size = if flags.is_monospace() {
            (base_size - 0.5).max(10.0)
        } else {
            base_size
        };
        let mut color = if seg.link.is_some() {
            palette.link_fg
        } else if seg.mono {
            palette.mono_fg
        } else {
            base_color
        };

        // Hidden spoiler text becomes fully transparent while particles cover it.
        if seg.spoiler && !spoilers_revealed {
            color = Color32::TRANSPARENT;
        }

        let link_underlined = seg
            .link
            .as_deref()
            .is_some_and(|url| match style.link_underline {
                LinkUnderline::Always => true,
                LinkUnderline::Never => false,
                LinkUnderline::Active => {
                    palette.link_always_active || hovered_link.is_some_and(|h| h == url)
                }
            });

        let underline =
            if (seg.underline || link_underlined) && !(seg.spoiler && !spoilers_revealed) {
                Stroke::new(1.0, color)
            } else {
                Stroke::NONE
            };
        let strikethrough = if seg.strike && !(seg.spoiler && !spoilers_revealed) {
            Stroke::new(1.0, color)
        } else {
            Stroke::NONE
        };
        let background = if seg.mono && !(seg.spoiler && !spoilers_revealed) {
            palette.mark_bg
        } else {
            Color32::TRANSPARENT
        };

        let format = TextFormat {
            font_id: font_id_for_flags(font_size, flags),
            color,
            background,
            // We use real OpenSans-Italic / OpenSans-SemiBoldItalic faces, so do not double-slant.
            italics: false,
            underline,
            strikethrough,
            line_height: base_line_height,
            ..Default::default()
        };
        job.append(slice, 0.0, format);
    }

    job
}

/// Render [`RichText`] inside `ui` with interactive links and click-to-reveal spoilers.
pub fn render_rich_text(
    ui: &mut Ui,
    id: Id,
    rich: &RichText,
    style: &TextStyle,
    palette: &TextPalette,
    base_color: Color32,
    max_width: f32,
) -> RichTextResponse {
    // The widget itself lives in `prepared`; called like this it keeps nothing between frames.
    prepared::render_rich_text_cached(
        ui,
        id,
        rich,
        &mut prepared::RichTextCache::default(),
        style,
        palette,
        base_color,
        max_width,
        None,
        &[],
    )
}

// ---------------------------------------------------------------------------
// Full tg-text TextWithEntities layout, rendering, elision, selection & hit-test
// ---------------------------------------------------------------------------

/// Default collapsed blockquote line limit matching tdesktop's `kQuoteCollapsedLines`.
pub const QUOTE_COLLAPSED_LINES: usize = 3;

/// Clickable link/entity descriptor resolved from a [`tg_text::EntityInText`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichTextLink {
    /// Original Telegram entity kind (`Url`, `CustomUrl`, `Mention`, `Hashtag`, `BotCommand`, ...).
    pub kind: EntityType,
    /// Target URL, `@username`, `#hashtag`, `mailto:...`, `tel:...`, or `MentionName` payload.
    pub target: String,
    /// Visible slice of text inside the message.
    pub text: String,
    /// UTF-16 code-unit range `(start, end)` in the source [`TextWithEntities`].
    pub utf16_range: (usize, usize),
}

/// Positioned placeholder for a `CustomEmoji` entity.
#[derive(Debug, Clone, PartialEq)]
pub struct CustomEmojiPlacement {
    /// Custom emoji document ID string from `EntityInText::data()`.
    pub document_id: String,
    /// Fallback Unicode emoji text from the message slice.
    pub fallback_text: String,
    /// Local bounding rectangle relative to the top-left of the laid-out rich text.
    pub rect: Rect,
    /// UTF-16 code-unit range `(start, end)` in the source text.
    pub utf16_range: (usize, usize),
}

/// 8-slot syntax highlighting color palette ported from `SyntaxHighlightColors`
/// (`Telegram/SourceFiles/ui/chat/chat_style.cpp:143-159`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxHighlightPalette {
    /// Slot 1: comments, doc comments, prolog, doctype, CDATA (`statisticsChartLineLightblue`).
    pub comment: Color32,
    /// Slot 2: punctuation (`statisticsChartLineRed` / muted operator punctuation).
    pub punctuation: Color32,
    /// Slot 3: numbers, booleans, constants, tags, properties, deletions (`statisticsChartLineRed`).
    pub literal: Color32,
    /// Slot 4: strings, chars, builtins, selectors, attr-names (`statisticsChartLineOrange`).
    pub string: Color32,
    /// Slot 5: operators, entities, URLs (`statisticsChartLineRed`).
    pub operator: Color32,
    /// Slot 6: keywords, functions, at-rules, attr-values (`statisticsChartLineBlue`).
    pub keyword: Color32,
    /// Slot 7: class / type / struct names (`statisticsChartLinePurple`).
    pub class_name: Color32,
    /// Slot 8: diff insertions (`statisticsChartLineGreen`).
    pub inserted: Color32,
}

impl SyntaxHighlightPalette {
    /// Light / Day theme syntax palette matching tdesktop's Day `colors.palette` chart line colors.
    #[must_use]
    pub const fn day() -> Self {
        Self {
            comment: Color32::from_rgb(0x6C, 0x8A, 0xA8),
            punctuation: Color32::from_rgb(0x6E, 0x77, 0x81),
            literal: Color32::from_rgb(0xD9, 0x45, 0x45),
            string: Color32::from_rgb(0xC8, 0x6D, 0x1F),
            operator: Color32::from_rgb(0xC5, 0x3B, 0x53),
            keyword: Color32::from_rgb(0x2B, 0x6C, 0xD4),
            class_name: Color32::from_rgb(0x7B, 0x4E, 0xC8),
            inserted: Color32::from_rgb(0x2E, 0x9B, 0x4E),
        }
    }

    /// Dark / Night theme syntax palette matching tdesktop's Night `colors.palette` chart line colors.
    #[must_use]
    pub const fn night() -> Self {
        Self {
            comment: Color32::from_rgb(0x7A, 0xA2, 0xC8),
            punctuation: Color32::from_rgb(0x9E, 0xAB, 0xB8),
            literal: Color32::from_rgb(0xF0, 0x6A, 0x6A),
            string: Color32::from_rgb(0xE8, 0x9D, 0x4D),
            operator: Color32::from_rgb(0xE5, 0x73, 0x73),
            keyword: Color32::from_rgb(0x5C, 0xA8, 0xF5),
            class_name: Color32::from_rgb(0xB4, 0x8E, 0xFA),
            inserted: Color32::from_rgb(0x58, 0xC9, 0x74),
        }
    }

    /// Automatically chooses Day or Night syntax colors based on `base_color` luminance.
    #[must_use]
    pub const fn for_base_color(base_color: Color32) -> Self {
        let luma = (base_color.r() as u32 * 299
            + base_color.g() as u32 * 587
            + base_color.b() as u32 * 114)
            / 1000;
        if luma >= 140 {
            Self::night()
        } else {
            Self::day()
        }
    }

    /// Returns the foreground [`Color32`] for a [`SyntaxTokenKind`].
    #[must_use]
    pub const fn color_for(self, kind: SyntaxTokenKind) -> Color32 {
        match kind {
            SyntaxTokenKind::Comment => self.comment,
            SyntaxTokenKind::Punctuation => self.punctuation,
            SyntaxTokenKind::Literal => self.literal,
            SyntaxTokenKind::String => self.string,
            SyntaxTokenKind::Operator => self.operator,
            SyntaxTokenKind::Keyword => self.keyword,
            SyntaxTokenKind::ClassName => self.class_name,
            SyntaxTokenKind::Inserted => self.inserted,
        }
    }
}

/// Options controlling [`layout_text_with_entities`] and [`render_text_with_entities`].
#[derive(Debug, Clone)]
pub struct RichTextOptions {
    /// Maximum layout width in logical pixels.
    pub max_width: f32,
    /// Optional maximum number of rendered lines (`None` or `Some(0)` = unlimited).
    pub max_lines: Option<usize>,
    /// Append `"…"` ellipsis when text exceeds `max_lines` (default `true`).
    pub elide: bool,
    /// Allow mouse drag / double-click text selection and Ctrl+C copying.
    pub selectable: bool,
    /// Default text foreground color.
    pub base_color: Color32,
    /// External UTF-16 text selection range to highlight (combined with interactive selection).
    pub selection: TextSelection,
    /// Whether spoiler spans are currently revealed.
    pub spoilers_revealed: bool,
    /// Index of the currently hovered link (used for `LinkUnderline::Active`).
    pub hovered_link_index: Option<usize>,
    /// Optional override background fill for `<blockquote>` blocks.
    pub quote_bg: Option<Color32>,
    /// Optional override left accent bar color for `<blockquote>` blocks.
    pub quote_outline: Option<Color32>,
    /// Optional override background fill for `<pre>` code blocks.
    pub pre_bg: Option<Color32>,
    /// Optional override left accent bar color for `<pre>` code blocks.
    pub pre_outline: Option<Color32>,
    /// Blockquote visual metrics (`st::history_text_style.blockquote`).
    pub blockquote_style: QuoteStyle,
    /// Pre block visual metrics (`st::history_text_style.pre`).
    pub pre_style: QuoteStyle,
    /// Optional override syntax highlighting palette (`None` derives Day/Night from `base_color`).
    pub syntax_palette: Option<SyntaxHighlightPalette>,
    /// Pause inline animated custom emoji playback (power saving / offscreen).
    pub pause_custom_emoji: bool,
    /// Reveal progress of the spoilers when the caller keeps that state (tdesktop's
    /// `String::setSpoilerRevealed`, a [`SPOILER_REVEAL_SECS`] fade): `0` hidden, `1` revealed.
    /// `None`: the widget keeps it itself and a click on a hidden spoiler reveals the text.
    /// Either way a click on a hidden spoiler sets [`EntityTextResponse::spoiler_toggled`].
    pub spoiler_reveal: Option<f32>,
    /// Expanded collapsible quotes (block indices) when the caller keeps that state, so the
    /// height it lays the text out with matches what is painted. `None`: the widget keeps it.
    /// Either way a click on a quote's toggle sets [`EntityTextResponse::blockquote_toggled`].
    pub expanded_blockquotes: Option<Vec<usize>>,
    /// The caller keeps the selection, like a message list that selects across messages
    /// (`HistoryInner::_selectedTextSelection`): with `selectable` the widget highlights only
    /// `selection`, reports left-button drags in [`EntityTextResponse::drag`] and neither keeps
    /// nor copies a selection of its own.
    pub external_selection: bool,
}

/// A left-button text drag over a [`RichTextOptions::external_selection`] widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextDrag {
    /// The drag began this frame (else it moved on).
    pub started: bool,
    /// UTF-16 offset in the source text under the pointer.
    pub utf16_offset: usize,
}

/// `fadeWrapDuration`: how long revealed spoiler text takes to fade in.
pub const SPOILER_REVEAL_SECS: f64 = 0.2;

/// How far the spoilers revealed `elapsed_secs` ago have faded in (`0..=1`, linear like
/// tdesktop's `Animations::Simple`); `1` at once when animations are off.
#[must_use]
pub fn spoiler_reveal_progress(elapsed_secs: f64) -> f32 {
    if crate::anim::disabled() {
        return 1.0;
    }
    (elapsed_secs / SPOILER_REVEAL_SECS).clamp(0.0, 1.0) as f32
}

impl RichTextOptions {
    /// Create default options for `base_color` and `max_width`.
    #[must_use]
    pub fn new(base_color: Color32, max_width: f32) -> Self {
        Self {
            max_width: max_width.max(1.0),
            max_lines: None,
            elide: true,
            selectable: false,
            base_color,
            selection: TextSelection::default(),
            spoilers_revealed: false,
            hovered_link_index: None,
            quote_bg: None,
            quote_outline: None,
            pre_bg: None,
            pre_outline: None,
            blockquote_style: QuoteStyle::history_blockquote(),
            pre_style: QuoteStyle::history_pre(),
            syntax_palette: None,
            pause_custom_emoji: false,
            spoiler_reveal: None,
            expanded_blockquotes: None,
            external_selection: false,
        }
    }

    /// Limit rendered lines with optional ellipsis.
    #[must_use]
    pub fn max_lines(mut self, lines: usize) -> Self {
        self.max_lines = Some(lines);
        self
    }

    /// Toggle ellipsis truncation when `max_lines` is exceeded.
    #[must_use]
    pub fn elide(mut self, elide: bool) -> Self {
        self.elide = elide;
        self
    }

    /// Enable or disable interactive text selection.
    #[must_use]
    pub fn selectable(mut self, selectable: bool) -> Self {
        self.selectable = selectable;
        self
    }

    /// Set an explicit UTF-16 selection range to highlight.
    #[must_use]
    pub fn selection(mut self, selection: TextSelection) -> Self {
        self.selection = selection;
        self
    }

    /// Let the caller keep the selection (see [`RichTextOptions::external_selection`]).
    #[must_use]
    pub fn external_selection(mut self, external: bool) -> Self {
        self.external_selection = external;
        self
    }

    /// Set whether spoilers start revealed.
    #[must_use]
    pub fn spoilers_revealed(mut self, revealed: bool) -> Self {
        self.spoilers_revealed = revealed;
        self
    }
}

/// Top-level block partition of a [`TextWithEntities`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RichBlockKind {
    /// Regular flowing paragraph text.
    Normal,
    /// Preformatted code block (`EntityType::Pre`) with optional syntax language name.
    Pre { language: String },
    /// Blockquote (`EntityType::Blockquote`) with optional collapsed flag.
    Blockquote { collapsed: bool },
}

/// One resolved inline span within a block (UTF-8 byte offsets relative to the full source text).
#[derive(Debug, Clone, PartialEq, Eq)]
struct InlineRun {
    start: usize,
    end: usize,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    code: bool,
    sub_or_super: bool,
    marked: bool,
    spoiler: bool,
    link_index: Option<usize>,
    custom_emoji_id: Option<String>,
    colorized: Option<SyntaxTokenKind>,
}

/// One partitioned block before layout (byte range relative to `source.text`).
#[derive(Debug, Clone)]
struct SourceBlock {
    kind: RichBlockKind,
    byte_range: Range<usize>,
}

/// One laid-out visual block inside [`LaidOutRichText`].
#[derive(Debug, Clone)]
pub struct LaidOutBlock {
    /// Block type (normal, pre, blockquote).
    pub kind: RichBlockKind,
    /// UTF-8 byte range in the source `TextWithEntities::text`.
    pub byte_range: Range<usize>,
    /// Outer block bounding rectangle in local coordinates.
    pub outer_rect: Rect,
    /// Text galley origin in local coordinates.
    pub text_origin: Pos2,
    /// Laid-out `egui::Galley` for this block's text slice.
    pub galley: Arc<Galley>,
    /// Optional header rectangle for `<pre>` language / copy bar.
    pub header_rect: Option<Rect>,
    /// Optional clickable copy button rectangle inside `<pre>`.
    pub copy_rect: Option<Rect>,
    /// Optional expand/collapse toggle rectangle inside collapsed `<blockquote>`.
    pub toggle_rect: Option<Rect>,
    /// Whether this block's content was truncated/elided.
    pub elided: bool,
}

/// Hit-test region for a link inside [`LaidOutRichText`].
#[derive(Debug, Clone, PartialEq)]
pub struct LinkHitRegion {
    /// Index into [`LaidOutRichText::links`].
    pub link_index: usize,
    /// Bounding rectangle in local coordinates.
    pub rect: Rect,
}

/// Result of [`LaidOutRichText::hit_test`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextHitResult {
    /// UTF-8 byte offset in the source string closest to the tested point.
    pub utf8_offset: usize,
    /// UTF-16 code unit offset in the source string closest to the tested point.
    pub utf16_offset: usize,
    /// Whether the point fell on the trailing half of the hit glyph.
    pub after_char: bool,
    /// Whether the point landed directly on a glyph.
    pub upon_char: bool,
    /// Link entity under the point, if any.
    pub link: Option<RichTextLink>,
    /// Index of the link in [`LaidOutRichText::links`], if any.
    pub link_index: Option<usize>,
    /// Whether the point is inside a spoiler span.
    pub in_spoiler: bool,
    /// Custom emoji document ID under the point, if any.
    pub custom_emoji_id: Option<String>,
    /// Index of `<pre>` block whose copy button was hit, if any.
    pub pre_copy_block: Option<usize>,
    /// Index of `<blockquote>` block whose expand/collapse area was hit, if any.
    pub blockquote_toggle_block: Option<usize>,
}

/// Full laid-out representation of a [`TextWithEntities`], ready for painting and hit-testing.
#[derive(Debug, Clone)]
pub struct LaidOutRichText {
    /// Copy of source plain text.
    pub text: String,
    /// Precomputed UTF-8 <-> UTF-16 mapper for `text`.
    pub offset_mapper: OffsetMapper,
    /// Laid-out blocks in vertical order.
    pub blocks: Vec<LaidOutBlock>,
    /// All clickable links/mentions/hashtags/commands extracted from entities.
    pub links: Vec<RichTextLink>,
    /// Per-row hit rectangles for each link.
    pub link_regions: Vec<LinkHitRegion>,
    /// Per-row rectangles covering spoiler spans (in local coordinates).
    pub spoiler_rects: Vec<Rect>,
    /// Per-row rectangles covering inline `Code` spans (in local coordinates) for pill backgrounds.
    pub inline_code_rects: Vec<Rect>,
    /// Positioned custom emoji placeholders (in local coordinates).
    pub custom_emojis: Vec<CustomEmojiPlacement>,
    /// Total laid-out size in logical pixels.
    pub size: Vec2,
    /// `true` if `max_lines` caused truncation/elision.
    pub elided: bool,
    /// Total number of laid-out visual rows across all blocks.
    pub line_count: usize,
}

impl LaidOutRichText {
    /// Hit-test a local point (relative to the top-left of the rendered rich text).
    #[must_use]
    pub fn hit_test(&self, local_pos: Pos2) -> TextHitResult {
        let mut result = TextHitResult::default();
        if self.blocks.is_empty() {
            return result;
        }

        // Check `<pre>` copy button and collapsed blockquote toggles first.
        for (idx, block) in self.blocks.iter().enumerate() {
            if let Some(copy_r) = block.copy_rect
                && copy_r.contains(local_pos)
            {
                result.pre_copy_block = Some(idx);
            }
            if let Some(toggle_r) = block.toggle_rect
                && toggle_r.contains(local_pos)
            {
                result.blockquote_toggle_block = Some(idx);
            }
        }

        // Check spoiler rects.
        if self.spoiler_rects.iter().any(|r| r.contains(local_pos)) {
            result.in_spoiler = true;
        }

        // Check custom emojis.
        for ce in &self.custom_emojis {
            if ce.rect.contains(local_pos) {
                result.custom_emoji_id = Some(ce.document_id.clone());
                break;
            }
        }

        // Check links (when not hidden by spoiler; caller decides if spoilers are revealed).
        for region in &self.link_regions {
            if region.rect.contains(local_pos) {
                result.link_index = Some(region.link_index);
                result.link = self.links.get(region.link_index).cloned();
                break;
            }
        }

        // Locate nearest block vertically for character / cursor hit-testing.
        let block = self
            .blocks
            .iter()
            .find(|b| local_pos.y >= b.outer_rect.min.y && local_pos.y <= b.outer_rect.max.y)
            .or_else(|| {
                if local_pos.y < self.blocks[0].outer_rect.min.y {
                    self.blocks.first()
                } else {
                    self.blocks.last()
                }
            });

        if let Some(block) = block {
            let block_slice = &self.text[block.byte_range.clone()];
            let rel = (local_pos - block.text_origin).to_pos2();
            let (local_byte, after_char, upon_char) =
                hit_test_galley(&block.galley, block_slice, rel);
            let abs_byte = (block.byte_range.start + local_byte).min(self.text.len());
            let abs_byte = clamp_char_boundary(&self.text, abs_byte);
            result.utf8_offset = abs_byte;
            result.utf16_offset = self
                .offset_mapper
                .utf8_to_utf16(abs_byte)
                .unwrap_or_else(|| self.offset_mapper.utf16_len());
            result.after_char = after_char;
            result.upon_char = upon_char;
        }

        result
    }

    /// Compute highlight rectangles (in local coordinates) for a UTF-16 [`TextSelection`].
    #[must_use]
    pub fn selection_rects(&self, selection: TextSelection) -> Vec<Rect> {
        if selection.is_empty() || self.text.is_empty() {
            return Vec::new();
        }
        let (u16_from, u16_to) = if selection.from <= selection.to {
            (usize::from(selection.from), usize::from(selection.to))
        } else {
            (usize::from(selection.to), usize::from(selection.from))
        };
        let sel_start = self.offset_mapper.utf16_to_utf8_floor(u16_from);
        let sel_end = self.offset_mapper.utf16_to_utf8_ceil(u16_to);
        if sel_end <= sel_start {
            return Vec::new();
        }

        let mut rects = Vec::new();
        for block in &self.blocks {
            let ov_start = sel_start.max(block.byte_range.start);
            let ov_end = sel_end.min(block.byte_range.end);
            if ov_end <= ov_start {
                continue;
            }
            let block_slice = &self.text[block.byte_range.clone()];
            let rel_start = ov_start - block.byte_range.start;
            let rel_end = ov_end - block.byte_range.start;
            rects.extend(byte_range_rects(
                &block.galley,
                block_slice,
                rel_start,
                rel_end,
                block.text_origin,
            ));
        }
        rects
    }

    /// Extract the selected substring for a UTF-16 [`TextSelection`].
    #[must_use]
    pub fn selected_text(&self, selection: TextSelection) -> String {
        if selection.is_empty() || self.text.is_empty() {
            return String::new();
        }
        let (u16_from, u16_to) = if selection.from <= selection.to {
            (usize::from(selection.from), usize::from(selection.to))
        } else {
            (usize::from(selection.to), usize::from(selection.from))
        };
        let start = self.offset_mapper.utf16_to_utf8_floor(u16_from);
        let end = self.offset_mapper.utf16_to_utf8_ceil(u16_to);
        if end > start && end <= self.text.len() {
            self.text[start..end].to_owned()
        } else {
            String::new()
        }
    }

    /// Return the raw code text for `<pre>` block at `block_index`, if any.
    #[must_use]
    pub fn pre_block_text(&self, block_index: usize) -> Option<&str> {
        let block = self.blocks.get(block_index)?;
        if matches!(block.kind, RichBlockKind::Pre { .. }) {
            Some(&self.text[block.byte_range.clone()])
        } else {
            None
        }
    }
}

/// Result returned by [`render_text_with_entities`].
#[derive(Debug, Clone)]
pub struct EntityTextResponse {
    /// Clicked link/mention/hashtag/bot-command entity, if any.
    pub clicked_link: Option<RichTextLink>,
    /// Currently hovered link entity, if any.
    pub hovered_link: Option<RichTextLink>,
    /// Whether any spoiler span was toggled/revealed this frame.
    pub spoiler_toggled: bool,
    /// Code text copied when the user clicked a `<pre>` block's copy button.
    pub copied_pre_code: Option<String>,
    /// Block index of the collapsible quote whose expand / collapse toggle was clicked.
    pub blockquote_toggled: Option<usize>,
    /// Current active UTF-16 text selection (when `selectable = true`).
    pub selection: TextSelection,
    /// A left-button drag when the caller keeps the selection
    /// ([`RichTextOptions::external_selection`]).
    pub drag: Option<TextDrag>,
    /// Whether the text was elided due to `max_lines`.
    pub elided: bool,
    /// Bounding rect occupied by the rendered rich text in screen coordinates.
    pub rect: Rect,
}

impl Default for EntityTextResponse {
    fn default() -> Self {
        Self {
            clicked_link: None,
            hovered_link: None,
            spoiler_toggled: false,
            copied_pre_code: None,
            blockquote_toggled: None,
            selection: TextSelection::default(),
            drag: None,
            elided: false,
            rect: Rect::NOTHING,
        }
    }
}

/// Convert a [`tg_text::EntityInText`]'s UTF-16 `(offset, length)` into a valid UTF-8 byte
/// range using [`OffsetMapper`] (snapping safely on mid-surrogate or out-of-bounds indices).
#[must_use]
pub fn entity_utf8_range(entity: &EntityInText, mapper: &OffsetMapper) -> Option<Range<usize>> {
    if !entity.is_valid() || entity.offset() < 0 || entity.length() <= 0 {
        return None;
    }
    let u16_start = entity.offset() as usize;
    let u16_len = entity.length() as usize;
    let u16_end = u16_start.saturating_add(u16_len);
    let max_u16 = mapper.utf16_len();
    if u16_start >= max_u16 {
        return None;
    }
    let start = mapper.utf16_to_utf8_floor(u16_start);
    let end = mapper.utf16_to_utf8_ceil(u16_end.min(max_u16));
    (end > start).then_some(start..end)
}

fn is_link_entity(kind: EntityType) -> bool {
    matches!(
        kind,
        EntityType::Url
            | EntityType::CustomUrl
            | EntityType::Email
            | EntityType::Hashtag
            | EntityType::Cashtag
            | EntityType::Mention
            | EntityType::MentionName
            | EntityType::BotCommand
            | EntityType::Phone
            | EntityType::BankCard
            | EntityType::TonAddress
            | EntityType::MediaTimestamp
    )
}

fn link_target_for_entity(entity: &EntityInText, slice: &str) -> String {
    if let Some(target) =
        crate::entity_link::entity_link_target(entity.entity_type(), entity.data(), slice)
    {
        return target;
    }
    match entity.entity_type() {
        EntityType::CustomUrl | EntityType::MentionName => entity.data().to_owned(),
        EntityType::Email => format!("mailto:{slice}"),
        EntityType::Phone => format!("tel:{slice}"),
        _ => slice.to_owned(),
    }
}

/// Partition `source` into top-level `Normal`, `Pre`, and `Blockquote` blocks, trimming
/// single separator newlines right around block boundaries so quotes/code blocks stack cleanly.
fn partition_blocks(source: &TextWithEntities, mapper: &OffsetMapper) -> Vec<SourceBlock> {
    if source.text.is_empty() {
        return Vec::new();
    }

    let mut block_spans: Vec<(Range<usize>, RichBlockKind)> = Vec::new();
    for entity in &source.entities {
        let kind = match entity.entity_type() {
            EntityType::Pre => RichBlockKind::Pre {
                language: entity.pre_language().unwrap_or("").to_owned(),
            },
            EntityType::Blockquote => RichBlockKind::Blockquote {
                collapsed: entity.is_collapsed_blockquote(),
            },
            _ => continue,
        };
        if let Some(range) = entity_utf8_range(entity, mapper) {
            block_spans.push((range, kind));
        }
    }

    if block_spans.is_empty() {
        return vec![SourceBlock {
            kind: RichBlockKind::Normal,
            byte_range: 0..source.text.len(),
        }];
    }

    // Sort by start ascending, longer span first; discard nested block-inside-block overlaps.
    block_spans.sort_by(|a, b| {
        a.0.start
            .cmp(&b.0.start)
            .then_with(|| b.0.len().cmp(&a.0.len()))
    });

    let mut non_overlapping: Vec<(Range<usize>, RichBlockKind)> = Vec::new();
    let mut cursor = 0usize;
    for (range, kind) in block_spans {
        if range.start < cursor {
            continue;
        }
        cursor = range.end;
        non_overlapping.push((range, kind));
    }

    let mut blocks = Vec::new();
    let mut pos = 0usize;
    let bytes = source.text.as_bytes();

    for (range, kind) in non_overlapping {
        if range.start > pos {
            let mut n_start = pos;
            let mut n_end = range.start;
            // Strip one trailing '\n' directly preceding a block container.
            if n_end > n_start && bytes[n_end - 1] == b'\n' {
                n_end -= 1;
            }
            if n_start > 0 && n_start < n_end && bytes[n_start] == b'\n' {
                n_start += 1;
            }
            if n_end > n_start {
                blocks.push(SourceBlock {
                    kind: RichBlockKind::Normal,
                    byte_range: n_start..n_end,
                });
            }
        }
        // Trim one trailing '\n' inside the block itself if present.
        let b_start = range.start;
        let mut b_end = range.end;
        if b_end > b_start && bytes[b_end - 1] == b'\n' {
            b_end -= 1;
        }
        if b_end > b_start {
            blocks.push(SourceBlock {
                kind,
                byte_range: b_start..b_end,
            });
        }
        pos = range.end;
    }

    if pos < source.text.len() {
        let mut n_start = pos;
        if bytes[n_start] == b'\n' {
            n_start += 1;
        }
        if n_start < source.text.len() {
            blocks.push(SourceBlock {
                kind: RichBlockKind::Normal,
                byte_range: n_start..source.text.len(),
            });
        }
    }

    blocks
}

/// Extract all clickable links from `source` and return `(links, per-link byte ranges)`.
fn extract_links(
    source: &TextWithEntities,
    mapper: &OffsetMapper,
) -> (Vec<RichTextLink>, Vec<Range<usize>>) {
    let mut links = Vec::new();
    let mut ranges = Vec::new();
    for entity in &source.entities {
        if !is_link_entity(entity.entity_type()) {
            continue;
        }
        let Some(range) = entity_utf8_range(entity, mapper) else {
            continue;
        };
        let slice = &source.text[range.clone()];
        let u16_start = entity.offset().max(0) as usize;
        let u16_end = u16_start.saturating_add(entity.length().max(0) as usize);
        links.push(RichTextLink {
            kind: entity.entity_type(),
            target: link_target_for_entity(entity, slice),
            text: slice.to_owned(),
            utf16_range: (u16_start, u16_end),
        });
        ranges.push(range);
    }
    (links, ranges)
}

/// Split one block's byte range into flat styled [`InlineRun`]s, applying Prism syntax
/// highlighting spans for `<pre>` code blocks carrying a language tag.
fn split_block_runs(
    source: &TextWithEntities,
    mapper: &OffsetMapper,
    block_range: &Range<usize>,
    block_kind: &RichBlockKind,
    link_ranges: &[Range<usize>],
) -> Vec<InlineRun> {
    if block_range.start >= block_range.end {
        return Vec::new();
    }
    let is_pre = matches!(block_kind, RichBlockKind::Pre { .. });
    let mut cuts = BTreeSet::new();
    cuts.insert(block_range.start);
    cuts.insert(block_range.end);

    let mut entity_ranges: Vec<(&EntityInText, Range<usize>)> = Vec::new();
    let mut has_explicit_colorized = false;
    for entity in &source.entities {
        let Some(range) = entity_utf8_range(entity, mapper) else {
            continue;
        };
        let ov_s = range.start.max(block_range.start);
        let ov_e = range.end.min(block_range.end);
        if ov_e > ov_s {
            if entity.entity_type() == EntityType::Colorized {
                has_explicit_colorized = true;
            }
            cuts.insert(ov_s);
            cuts.insert(ov_e);
            entity_ranges.push((entity, ov_s..ov_e));
        }
    }

    // Automatically run Prism-style syntax highlighting for `<pre>` blocks with a language tag
    // unless `EntityType::Colorized` spans are already supplied on the block.
    let mut syntax_spans: Vec<(Range<usize>, SyntaxTokenKind)> = Vec::new();
    if let RichBlockKind::Pre { language } = block_kind
        && !language.is_empty()
        && !has_explicit_colorized
    {
        let block_slice = &source.text[block_range.clone()];
        for sp in highlight_code_spans(block_slice, language) {
            let abs_s = block_range.start + sp.range.start;
            let abs_e = block_range.start + sp.range.end;
            if abs_e > abs_s && abs_e <= block_range.end {
                cuts.insert(abs_s);
                cuts.insert(abs_e);
                syntax_spans.push((abs_s..abs_e, sp.kind));
            }
        }
    }

    let points: Vec<usize> = cuts.into_iter().collect();
    let mut runs = Vec::with_capacity(points.len().saturating_sub(1));
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b <= a {
            continue;
        }
        let mut run = InlineRun {
            start: a,
            end: b,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            code: is_pre,
            sub_or_super: false,
            marked: false,
            spoiler: false,
            link_index: None,
            custom_emoji_id: None,
            colorized: None,
        };
        for &(entity, ref range) in &entity_ranges {
            if range.start <= a && range.end >= b {
                match entity.entity_type() {
                    EntityType::Bold | EntityType::Semibold => run.bold = true,
                    EntityType::Italic => run.italic = true,
                    EntityType::Underline => run.underline = true,
                    EntityType::StrikeOut => run.strike = true,
                    EntityType::Code | EntityType::Pre => run.code = true,
                    EntityType::Subscript | EntityType::Superscript => run.sub_or_super = true,
                    EntityType::Marked => run.marked = true,
                    EntityType::Spoiler => run.spoiler = true,
                    EntityType::CustomEmoji => {
                        run.custom_emoji_id = Some(entity.data().to_owned());
                    }
                    EntityType::Colorized => {
                        if let Some(kind) = SyntaxTokenKind::from_entity_data(entity.data()) {
                            run.colorized = Some(kind);
                        }
                    }
                    _ => {}
                }
            }
        }
        if run.colorized.is_none() {
            for &(ref sr, kind) in &syntax_spans {
                if sr.start <= a && sr.end >= b {
                    run.colorized = Some(kind);
                    break;
                }
            }
        }
        for (li, lr) in link_ranges.iter().enumerate() {
            if lr.start <= a && lr.end >= b {
                run.link_index = Some(li);
                break;
            }
        }
        runs.push(run);
    }
    runs
}

/// Lay out a [`tg_text::TextWithEntities`] into [`LaidOutRichText`] using `ctx`'s font atlas.
///
/// Works both during live rendering and inside headless unit tests (after registering fonts
/// or calling `ctx.run_ui`).
#[must_use]
pub fn layout_text_with_entities(
    ctx: &egui::Context,
    source: &TextWithEntities,
    style: &TextStyle,
    palette: &TextPalette,
    options: &RichTextOptions,
) -> LaidOutRichText {
    layout_text_with_entities_expanded(ctx, source, style, palette, options, &[])
}

/// Lay out a [`TextWithEntities`] with an explicit set of expanded blockquote block indices.
#[must_use]
pub fn layout_text_with_entities_expanded(
    ctx: &egui::Context,
    source: &TextWithEntities,
    style: &TextStyle,
    palette: &TextPalette,
    options: &RichTextOptions,
    expanded_blockquotes: &[usize],
) -> LaidOutRichText {
    prepared::note_text_preparation();
    let mapper = OffsetMapper::new(&source.text);
    let (links, link_ranges) = extract_links(source, &mapper);
    let source_blocks = partition_blocks(source, &mapper);

    let max_lines_limit = options.max_lines.filter(|&n| n > 0);
    let mut remaining_lines = max_lines_limit;
    let mut laid_blocks = Vec::with_capacity(source_blocks.len());
    let mut link_regions = Vec::new();
    let mut spoiler_rects = Vec::new();
    let mut inline_code_rects = Vec::new();
    let mut custom_emojis = Vec::new();

    let mut cursor_y = 0.0_f32;
    let mut max_w = 0.0_f32;
    let mut total_elided = false;
    let mut total_lines = 0usize;

    let base_size = style.font.size;
    let base_line_height = (style.line_height > 0.0).then_some(style.line_height);

    for (block_idx, sb) in source_blocks.iter().enumerate() {
        if let Some(0) = remaining_lines {
            total_elided = true;
            break;
        }

        let is_pre = matches!(sb.kind, RichBlockKind::Pre { .. });
        let is_quote = matches!(sb.kind, RichBlockKind::Blockquote { .. });
        let quote_st = if is_pre {
            &options.pre_style
        } else {
            &options.blockquote_style
        };

        let vertical_skip = if (is_pre || is_quote) && block_idx > 0 {
            quote_st.vertical_skip.max(4.0)
        } else if block_idx > 0 {
            4.0
        } else {
            0.0
        };
        cursor_y += vertical_skip;

        let inner_pad = if is_pre || is_quote {
            quote_st.padding
        } else {
            crate::style::Margins::default()
        };
        let header_h = if let RichBlockKind::Pre { ref language } = sb.kind {
            if !language.is_empty() || quote_st.header > 0.0 {
                quote_st.header.max(20.0)
            } else {
                0.0
            }
        } else {
            0.0
        };

        let text_wrap_w = (options.max_width - inner_pad.horizontal()).max(24.0);
        let runs = split_block_runs(source, &mapper, &sb.byte_range, &sb.kind, &link_ranges);
        let syntax_palette = options
            .syntax_palette
            .unwrap_or_else(|| SyntaxHighlightPalette::for_base_color(options.base_color));

        // Determine effective row cap for this block (either global `max_lines` or collapsed quote).
        let collapsed_cap = match sb.kind {
            RichBlockKind::Blockquote { collapsed: true }
                if !expanded_blockquotes.contains(&block_idx) =>
            {
                Some(QUOTE_COLLAPSED_LINES)
            }
            _ => None,
        };
        let block_max_rows = match (remaining_lines, collapsed_cap) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };

        // A collapsed quote just stops after its lines (`Renderer::draw` skips the lines past
        // `_quoteLinesLeft`); only a `max_lines` limit ends the text with an ellipsis.
        let elide_rows = options.elide
            && remaining_lines.is_some_and(|rem| collapsed_cap.is_none_or(|cap| rem <= cap));
        let mut job = LayoutJob {
            wrap: TextWrapping {
                max_width: text_wrap_w,
                max_rows: block_max_rows.unwrap_or(usize::MAX),
                break_anywhere: block_max_rows == Some(1) && options.elide,
                overflow_character: if elide_rows { Some('…') } else { None },
            },
            ..Default::default()
        };

        for run in &runs {
            let slice = &source.text[run.start..run.end];
            let mut flags = style.font.to_tg_flags();
            if run.code {
                flags = FontFlags::MONOSPACE;
            } else {
                if run.bold {
                    flags = flags.union(FontFlags::BOLD);
                }
                if run.italic {
                    flags = flags.union(FontFlags::ITALIC);
                }
            }

            let mut font_size = if run.code {
                (base_size - 0.5).max(10.0)
            } else {
                base_size
            };
            if run.sub_or_super && !run.code {
                font_size = (font_size * SUB_SUPER_MULTIPLIER as f32).round().max(8.0);
            }

            let mut color = if run.link_index.is_some() {
                palette.link_fg
            } else if let Some(syntax_kind) = run.colorized {
                syntax_palette.color_for(syntax_kind)
            } else if run.code {
                palette.mono_fg
            } else {
                options.base_color
            };

            let hidden_spoiler = run.spoiler && !options.spoilers_revealed;
            if hidden_spoiler {
                color = Color32::TRANSPARENT;
            } else if let Some(ref doc_id) = run.custom_emoji_id
                && has_registered_custom_emoji(doc_id)
            {
                // Keep glyph spacing for hit-testing/selection while the animated custom emoji
                // texture paints cleanly over this slot.
                color = Color32::TRANSPARENT;
            }

            let link_underlined = run.link_index.is_some_and(|li| match style.link_underline {
                LinkUnderline::Always => true,
                LinkUnderline::Never => false,
                LinkUnderline::Active => {
                    palette.link_always_active || options.hovered_link_index == Some(li)
                }
            });

            let underline = if (run.underline || link_underlined) && !hidden_spoiler {
                Stroke::new(1.0, color)
            } else {
                Stroke::NONE
            };
            let strikethrough = if run.strike && !hidden_spoiler {
                Stroke::new(1.0, color)
            } else {
                Stroke::NONE
            };
            let background = if run.marked && !hidden_spoiler {
                palette.mark_bg
            } else {
                Color32::TRANSPARENT
            };

            let format = TextFormat {
                font_id: font_id_for_flags(font_size, flags),
                color,
                background,
                italics: false,
                underline,
                strikethrough,
                line_height: base_line_height,
                ..Default::default()
            };
            job.append(slice, 0.0, format);
        }

        // Emoji glyphs are sized in physical pixels (`tg_emoji::text_glyphs`), and this layout
        // may run outside a pass.
        tg_emoji::set_text_pixels_per_point(ctx.pixels_per_point());
        let galley = ctx.fonts_mut(|f| f.layout_job(job));
        let galley_rows = galley.rows.len().max(1);
        total_lines += galley_rows;

        if let Some(rem) = &mut remaining_lines {
            *rem = rem.saturating_sub(galley_rows);
        }
        if galley.elided {
            total_elided = true;
        }

        let block_w = if is_pre || is_quote {
            (galley.size().x + inner_pad.horizontal())
                .max(options.max_width.min(180.0))
                .min(options.max_width)
        } else {
            galley.size().x.min(options.max_width)
        };
        let block_h = header_h + inner_pad.top + galley.size().y + inner_pad.bottom;
        let outer_rect = Rect::from_min_size(Pos2::new(0.0, cursor_y), Vec2::new(block_w, block_h));
        let text_origin = Pos2::new(
            outer_rect.min.x + inner_pad.left,
            outer_rect.min.y + header_h + inner_pad.top,
        );

        let header_rect = (is_pre && header_h > 0.0)
            .then(|| Rect::from_min_size(outer_rect.min, Vec2::new(outer_rect.width(), header_h)));
        let copy_rect = if is_pre {
            let h = header_h.max(20.0);
            Some(Rect::from_min_size(
                Pos2::new(outer_rect.max.x - 24.0, outer_rect.min.y),
                Vec2::new(24.0, h),
            ))
        } else {
            None
        };
        let toggle_rect = match sb.kind {
            RichBlockKind::Blockquote { collapsed: true } => Some(Rect::from_min_size(
                Pos2::new(outer_rect.max.x - 22.0, outer_rect.max.y - 20.0),
                Vec2::new(20.0, 18.0),
            )),
            _ => None,
        };

        // Compute hit/decoration rects for links, spoilers, inline code, and custom emojis.
        let block_slice = &source.text[sb.byte_range.clone()];
        for run in &runs {
            let needs_rects = run.link_index.is_some()
                || run.spoiler
                || (run.code && !is_pre)
                || run.custom_emoji_id.is_some();
            if !needs_rects {
                continue;
            }
            let rel_s = run.start - sb.byte_range.start;
            let rel_e = run.end - sb.byte_range.start;
            let rects = byte_range_rects(&galley, block_slice, rel_s, rel_e, text_origin);
            if let Some(li) = run.link_index {
                for r in &rects {
                    link_regions.push(LinkHitRegion {
                        link_index: li,
                        rect: *r,
                    });
                }
            }
            if run.spoiler {
                spoiler_rects.extend(rects.iter().copied());
            }
            if run.code && !is_pre {
                inline_code_rects.extend(rects.iter().copied());
            }
            if let Some(doc_id) = &run.custom_emoji_id
                && let Some(&first_rect) = rects.first()
            {
                let emoji_sz = px(st::emoji_size).max(base_size);
                let placed = Rect::from_min_size(
                    Pos2::new(first_rect.min.x, first_rect.center().y - emoji_sz * 0.5),
                    Vec2::new(first_rect.width().max(emoji_sz), emoji_sz),
                );
                let u16_s = mapper.utf8_to_utf16(run.start).unwrap_or(0);
                let u16_e = mapper.utf8_to_utf16(run.end).unwrap_or(u16_s);
                custom_emojis.push(CustomEmojiPlacement {
                    document_id: doc_id.clone(),
                    fallback_text: source.text[run.start..run.end].to_owned(),
                    rect: placed,
                    utf16_range: (u16_s, u16_e),
                });
            }
        }

        max_w = max_w.max(outer_rect.width());
        cursor_y = outer_rect.max.y;

        laid_blocks.push(LaidOutBlock {
            kind: sb.kind.clone(),
            byte_range: sb.byte_range.clone(),
            outer_rect,
            text_origin,
            galley,
            header_rect,
            copy_rect,
            toggle_rect,
            elided: galley_elided_flag(&laid_blocks, total_elided),
        });
        if let Some(last) = laid_blocks.last_mut() {
            last.elided = last.galley.elided;
        }
    }

    LaidOutRichText {
        text: source.text.clone(),
        offset_mapper: mapper,
        blocks: laid_blocks,
        links,
        link_regions,
        spoiler_rects: merge_spoiler_rects(&spoiler_rects),
        inline_code_rects,
        custom_emojis,
        size: Vec2::new(max_w, cursor_y),
        elided: total_elided,
        line_count: total_lines,
    }
}

fn galley_elided_flag(_existing: &[LaidOutBlock], _fallback: bool) -> bool {
    false
}

#[derive(Debug, Clone, Default)]
struct EntityTextInteractiveState {
    /// When a click revealed the spoilers (`ui.input` time), if the widget keeps that state.
    spoilers_revealed_at: Option<f64>,
    hovered_link_index: Option<usize>,
    selection: TextSelection,
    drag_anchor_utf16: Option<usize>,
    expanded_blockquotes: Vec<usize>,
}

/// Render a [`tg_text::TextWithEntities`] inside `ui` with full tdesktop styling,
/// syntax-highlighted `<pre>` code blocks, `<blockquote>` left accent bars, animated inline
/// custom emoji (Lottie / WebM frames from the shared registry, with fallback badges),
/// link hover/click, animated spoiler particles, `max_lines` + elision, and text selection.
pub fn render_text_with_entities(
    ui: &mut Ui,
    id: Id,
    source: &TextWithEntities,
    style: &TextStyle,
    palette: &TextPalette,
    options: &RichTextOptions,
) -> EntityTextResponse {
    render_text_with_entities_and_emoji(ui, id, source, style, palette, options, None)
}

/// Same as [`render_text_with_entities`], with an optional explicit [`CustomEmojiRenderer`]
/// checked before falling back to the process-wide custom emoji registry.
pub fn render_text_with_entities_and_emoji(
    ui: &mut Ui,
    id: Id,
    source: &TextWithEntities,
    style: &TextStyle,
    palette: &TextPalette,
    options: &RichTextOptions,
    emoji_renderer: Option<&mut CustomEmojiRenderer>,
) -> EntityTextResponse {
    render_entity_text_cached(
        ui,
        id,
        source,
        &mut prepared::EntityLayoutCache::default(),
        style,
        palette,
        options,
        emoji_renderer,
    )
}

/// [`render_text_with_entities_and_emoji`] with the block layout taken from `layout_cache`,
/// which lays the text out again only when an input of the layout changed.
#[allow(clippy::too_many_arguments)]
fn render_entity_text_cached(
    ui: &mut Ui,
    id: Id,
    source: &TextWithEntities,
    layout_cache: &mut prepared::EntityLayoutCache,
    style: &TextStyle,
    palette: &TextPalette,
    options: &RichTextOptions,
    mut emoji_renderer: Option<&mut CustomEmojiRenderer>,
) -> EntityTextResponse {
    let state_key = id.with("_tg_entity_text_state");
    let mut state = ui
        .ctx()
        .data_mut(|d| d.get_temp::<EntityTextInteractiveState>(state_key))
        .unwrap_or_default();

    let now = ui.input(|i| i.time);
    let reveal = match options.spoiler_reveal {
        Some(progress) => progress.clamp(0.0, 1.0),
        None if options.spoilers_revealed => 1.0,
        None => state
            .spoilers_revealed_at
            .map_or(0.0, |at| spoiler_reveal_progress(now - at)),
    };
    let effective_revealed = reveal >= 1.0;
    // A spoiler takes the click only while it is fully hidden.
    let spoilers_clickable = reveal <= 0.0;
    if !effective_revealed && !spoilers_clickable {
        ui.ctx().request_repaint();
    }
    let expanded_quotes = options
        .expanded_blockquotes
        .clone()
        .unwrap_or_else(|| state.expanded_blockquotes.clone());
    let mut layout_opts = options.clone();
    layout_opts.spoilers_revealed = effective_revealed;
    layout_opts.spoiler_reveal = None;
    layout_opts.expanded_blockquotes = None;
    layout_opts.hovered_link_index = state.hovered_link_index;

    // While the spoilers fade in, the revealed text is painted over them (`_spoilerOpacity`).
    let fading_in: Vec<(Pos2, Arc<Galley>)> = if effective_revealed || spoilers_clickable {
        layout_cache.clear_fade();
        Vec::new()
    } else {
        let mut revealed_opts = layout_opts.clone();
        revealed_opts.spoilers_revealed = true;
        layout_cache
            .fade_layout(
                ui.ctx(),
                source,
                style,
                palette,
                &revealed_opts,
                &expanded_quotes,
            )
            .blocks
            .iter()
            .map(|b| (b.text_origin, Arc::clone(&b.galley)))
            .collect()
    };

    let laid = layout_cache.layout(
        ui.ctx(),
        source,
        style,
        palette,
        &layout_opts,
        &expanded_quotes,
    );

    let sense = if options.selectable {
        Sense::click_and_drag()
    } else {
        Sense::click()
    };
    let (rect, response) = ui.allocate_exact_size(laid.size, sense);
    let origin = rect.min;

    let mut out = EntityTextResponse {
        elided: laid.elided,
        rect,
        selection: if !options.selection.is_empty() {
            options.selection
        } else {
            state.selection
        },
        ..Default::default()
    };

    // Hover hit-testing.
    let mut new_hovered_link: Option<usize> = None;
    let mut cursor_icon = if options.selectable {
        Some(CursorIcon::Text)
    } else {
        None
    };

    if let Some(pointer) = ui.ctx().pointer_hover_pos()
        && rect.contains(pointer)
    {
        let hit = laid.hit_test((pointer - origin).to_pos2());
        if (spoilers_clickable && hit.in_spoiler)
            || hit.pre_copy_block.is_some()
            || hit.blockquote_toggle_block.is_some()
        {
            cursor_icon = Some(CursorIcon::PointingHand);
        } else if let Some(li) = hit.link_index {
            new_hovered_link = Some(li);
            out.hovered_link = hit.link;
            cursor_icon = Some(CursorIcon::PointingHand);
        }
    }

    if let Some(icon) = cursor_icon
        && response.hovered()
    {
        ui.ctx().set_cursor_icon(icon);
    }

    if new_hovered_link != state.hovered_link_index {
        state.hovered_link_index = new_hovered_link;
        ui.ctx().request_repaint();
    }

    // Selection dragging and double-click word selection.
    if options.selectable && options.external_selection {
        // Only the left button selects: a right press on the selection opens the caller's
        // context menu over it.
        let started = response.drag_started_by(egui::PointerButton::Primary);
        if (started || response.dragged_by(egui::PointerButton::Primary))
            && let Some(pos) = ui.ctx().pointer_interact_pos()
        {
            out.drag = Some(TextDrag {
                started,
                utf16_offset: laid.hit_test((pos - origin).to_pos2()).utf16_offset,
            });
        }
        out.selection = options.selection;
    } else if options.selectable {
        if response.drag_started()
            && let Some(pos) = ui.ctx().pointer_interact_pos()
        {
            let hit = laid.hit_test((pos - origin).to_pos2());
            state.drag_anchor_utf16 = Some(hit.utf16_offset);
            let off = hit.utf16_offset.min(u16::MAX as usize) as u16;
            state.selection = TextSelection::new(off, off);
        }
        if response.dragged()
            && let (Some(anchor), Some(pos)) =
                (state.drag_anchor_utf16, ui.ctx().pointer_interact_pos())
        {
            let hit = laid.hit_test((pos - origin).to_pos2());
            let a = anchor.min(u16::MAX as usize) as u16;
            let b = hit.utf16_offset.min(u16::MAX as usize) as u16;
            state.selection = TextSelection::new(a.min(b), a.max(b));
        }
        if response.drag_stopped() {
            state.drag_anchor_utf16 = None;
        }
        if response.double_clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let hit = laid.hit_test((pos - origin).to_pos2());
            state.selection = word_selection_at(&laid.text, &laid.offset_mapper, hit.utf8_offset);
        }
        if !state.selection.is_empty() && ui.input(crate::clipboard::copy_requested) {
            let copied = laid.selected_text(state.selection);
            if !copied.is_empty() {
                ui.ctx().copy_text(copied);
            }
        }
        out.selection = if !options.selection.is_empty() {
            options.selection
        } else {
            state.selection
        };
    }

    // Click handling: spoiler reveal, `<pre>` copy, blockquote toggle, link activation.
    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let hit = laid.hit_test((pos - origin).to_pos2());
        if spoilers_clickable && hit.in_spoiler {
            if options.spoiler_reveal.is_none() {
                state.spoilers_revealed_at = Some(now);
            }
            out.spoiler_toggled = true;
            ui.ctx().request_repaint();
        } else if let Some(pre_idx) = hit.pre_copy_block {
            if let Some(code) = laid.pre_block_text(pre_idx) {
                let code_owned = code.to_owned();
                ui.ctx().copy_text(code_owned.clone());
                out.copied_pre_code = Some(code_owned);
            }
        } else if let Some(bq_idx) = hit.blockquote_toggle_block {
            if options.expanded_blockquotes.is_none() {
                if let Some(p) = state.expanded_blockquotes.iter().position(|&x| x == bq_idx) {
                    state.expanded_blockquotes.remove(p);
                } else {
                    state.expanded_blockquotes.push(bq_idx);
                }
            }
            out.blockquote_toggled = Some(bq_idx);
            ui.ctx().request_repaint();
        } else if let Some(link) = hit.link {
            out.clicked_link = Some(link);
        } else if options.selectable && !options.external_selection {
            state.selection = TextSelection::default();
            out.selection = TextSelection::default();
        }
    }

    // Paint blocks, backgrounds, selection highlights, galleys, custom emojis, spoilers.
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let offset = origin.to_vec2();

        // 1. Paint `<pre>` and `<blockquote>` containers behind text.
        for (idx, block) in laid.blocks.iter().enumerate() {
            let outer = block.outer_rect.translate(offset);
            match &block.kind {
                RichBlockKind::Normal => {}
                RichBlockKind::Pre { language } => {
                    let q = &options.pre_style;
                    let radius = q.radius.max(4.0).round().clamp(0.0, 255.0) as u8;
                    let outline_w = q.outline.max(3.0);
                    let bg = options
                        .pre_bg
                        .unwrap_or_else(|| with_alpha(palette.mono_fg, 0.10));
                    let bar_col = options.pre_outline.unwrap_or(palette.mono_fg);

                    painter.rect_filled(outer, CornerRadius::same(radius), bg);
                    // Left accent bar clipped cleanly with left rounded corners.
                    let bar_rect =
                        Rect::from_min_size(outer.min, Vec2::new(outline_w, outer.height()));
                    painter.rect_filled(
                        bar_rect,
                        CornerRadius {
                            nw: radius,
                            sw: radius,
                            ne: 0,
                            se: 0,
                        },
                        bar_col,
                    );

                    // Optional header strip with language label and copy icon.
                    if let Some(hdr) = block.header_rect {
                        let hdr_s = hdr.translate(offset);
                        painter.rect_filled(
                            hdr_s,
                            CornerRadius {
                                nw: radius,
                                ne: radius,
                                sw: 0,
                                se: 0,
                            },
                            with_alpha(bar_col, 0.08),
                        );
                        let label = if language.is_empty() {
                            "code"
                        } else {
                            language.as_str()
                        };
                        painter.text(
                            Pos2::new(
                                hdr_s.min.x + q.header_position.x.max(10.0),
                                hdr_s.center().y,
                            ),
                            Align2::LEFT_CENTER,
                            label,
                            font_id_for_flags(
                                (style.font.size - 1.5).max(10.0),
                                FontFlags::MONOSPACE,
                            ),
                            with_alpha(palette.mono_fg, 0.85),
                        );
                    }

                    if let Some(copy_r) = block.copy_rect {
                        let cr = copy_r.translate(offset);
                        let icon_rect = Rect::from_center_size(cr.center(), Vec2::splat(13.0));
                        let painted = paint_style_icon(
                            painter,
                            icon_rect,
                            q.icon,
                            &tg_style::DefaultPaletteResolver,
                            Some(with_alpha(palette.mono_fg, 0.75)),
                        );
                        if !painted {
                            let s = Stroke::new(1.1, with_alpha(palette.mono_fg, 0.75));
                            painter.rect_stroke(
                                Rect::from_center_size(cr.center(), Vec2::splat(9.0)),
                                CornerRadius::same(1),
                                s,
                                egui::StrokeKind::Inside,
                            );
                        }
                    }
                }
                RichBlockKind::Blockquote { collapsed } => {
                    let q = &options.blockquote_style;
                    let radius = q.radius.max(4.0).round().clamp(0.0, 255.0) as u8;
                    let outline_w = q.outline.max(3.0);
                    let bar_col = options.quote_outline.unwrap_or(palette.link_fg);
                    let bg = options
                        .quote_bg
                        .unwrap_or_else(|| with_alpha(bar_col, 0.10));

                    painter.rect_filled(outer, CornerRadius::same(radius), bg);
                    let bar_rect =
                        Rect::from_min_size(outer.min, Vec2::new(outline_w, outer.height()));
                    painter.rect_filled(
                        bar_rect,
                        CornerRadius {
                            nw: radius,
                            sw: radius,
                            ne: 0,
                            se: 0,
                        },
                        bar_col,
                    );

                    // Top-right mini quote icon (`st::history_text_style.blockquote.icon`).
                    let quote_icon_rect = Rect::from_min_size(
                        Pos2::new(outer.max.x - 16.0, outer.min.y + q.icon_position.y.max(4.0)),
                        Vec2::splat(12.0),
                    );
                    let painted = paint_style_icon(
                        painter,
                        quote_icon_rect,
                        q.icon,
                        &tg_style::DefaultPaletteResolver,
                        Some(with_alpha(bar_col, 0.7)),
                    );
                    if !painted {
                        painter.text(
                            quote_icon_rect.center(),
                            Align2::CENTER_CENTER,
                            "”",
                            FontId::proportional(14.0),
                            with_alpha(bar_col, 0.75),
                        );
                    }

                    // Expand/collapse chevron for collapsible blockquotes.
                    if *collapsed && let Some(tr) = block.toggle_rect {
                        let tr_s = tr.translate(offset);
                        let expanded = expanded_quotes.contains(&idx);
                        let chevron = if expanded { q.collapse } else { q.expand };
                        let painted = paint_style_icon(
                            painter,
                            Rect::from_center_size(tr_s.center(), Vec2::splat(11.0)),
                            chevron,
                            &tg_style::DefaultPaletteResolver,
                            Some(bar_col),
                        );
                        if !painted {
                            let c = tr_s.center();
                            let dy = if expanded { -2.0 } else { 2.0 };
                            painter.add(Shape::line(
                                vec![
                                    Pos2::new(c.x - 4.0, c.y - dy),
                                    Pos2::new(c.x, c.y + dy),
                                    Pos2::new(c.x + 4.0, c.y - dy),
                                ],
                                Stroke::new(1.5, bar_col),
                            ));
                        }
                    }
                }
            }
        }

        // 2. Paint inline `Code` subtle rounded backgrounds.
        let code_bg = with_alpha(palette.mark_bg, 0.85);
        for cr in &laid.inline_code_rects {
            let sr = cr.translate(offset).expand2(Vec2::new(2.0, 0.0));
            painter.rect_filled(sr, CornerRadius::same(3), code_bg);
        }

        // 3. Paint active text selection highlight behind glyphs.
        if !out.selection.is_empty() {
            let sel_col = if palette.select_bg.a() > 0 {
                palette.select_bg
            } else {
                palette.select_overlay
            };
            for sr in laid.selection_rects(out.selection) {
                painter.rect_filled(sr.translate(offset), CornerRadius::same(2), sel_col);
            }
        }

        // 4. Paint custom emoji placeholder badges behind fallback glyphs when no animated
        //    texture is registered yet.
        let mut animated_custom_emojis: Vec<(usize, Rect)> = Vec::new();
        for (ce_idx, ce) in laid.custom_emojis.iter().enumerate() {
            let er = ce.rect.translate(offset);
            let has_explicit = emoji_renderer
                .as_deref()
                .is_some_and(|r| r.contains(&ce.document_id));
            if has_explicit || has_registered_custom_emoji(&ce.document_id) {
                animated_custom_emojis.push((ce_idx, er));
            } else {
                painter.rect_filled(er, CornerRadius::same(4), with_alpha(palette.link_fg, 0.14));
                painter.rect_stroke(
                    er,
                    CornerRadius::same(4),
                    Stroke::new(1.0, with_alpha(palette.link_fg, 0.35)),
                    egui::StrokeKind::Inside,
                );
            }
        }

        // 5. Paint laid-out text galleys.
        for block in &laid.blocks {
            painter.galley(
                block.text_origin + offset,
                Arc::clone(&block.galley),
                options.base_color,
            );
        }

        // 5b. Paint animated inline custom emoji textures (Lottie / WebM / static RGBA) over
        //     their laid-out inline slots (`text_lottie_custom_emoji.cpp`).
        if !animated_custom_emojis.is_empty() {
            let paint_ctx = CustomEmojiPaintContext {
                time_secs: ui.ctx().input(|i| i.time),
                paused: options.pause_custom_emoji,
                force_first_frame: false,
                force_last_frame: false,
                text_color: options.base_color,
                overlay: None,
            };
            for (ce_idx, er) in animated_custom_emojis {
                let doc_id = &laid.custom_emojis[ce_idx].document_id;
                let painted_explicit = emoji_renderer
                    .as_deref_mut()
                    .is_some_and(|r| r.paint(ui.ctx(), painter, doc_id, er, paint_ctx));
                if !painted_explicit {
                    let _ = paint_registered_custom_emoji(ui.ctx(), painter, doc_id, er, paint_ctx);
                }
            }
        }

        // 5c. Spoilers being revealed: their text fades in over the hidden layout.
        if !fading_in.is_empty() {
            for sr in &laid.spoiler_rects {
                let mut fade =
                    painter.with_clip_rect(painter.clip_rect().intersect(sr.translate(offset)));
                fade.multiply_opacity(reveal);
                for (text_origin, galley) in &fading_in {
                    fade.galley(
                        *text_origin + offset,
                        Arc::clone(galley),
                        options.base_color,
                    );
                }
            }
        }

        // 6. The particle cover over hidden spoilers (`paintSpoilerRects`), fading out while
        //    they are revealed.
        if !effective_revealed && !laid.spoiler_rects.is_empty() {
            let rects: Vec<Rect> = laid
                .spoiler_rects
                .iter()
                .map(|r| r.translate(offset))
                .collect();
            // The selected part of the cover is drawn in `selectSpoilerFg`
            // (`Renderer::pushSpoilerRange`).
            let selected: Vec<Rect> = if out.selection.is_empty() {
                Vec::new()
            } else {
                laid.selection_rects(out.selection)
                    .into_iter()
                    .map(|r| r.translate(offset))
                    .collect()
            };
            crate::effects::paint_text_spoiler_selected(
                ui.ctx(),
                painter,
                &rects,
                &selected,
                origin,
                palette.spoiler_fg,
                palette.select_spoiler_fg,
                1.0 - reveal,
            );
        }
    }

    ui.ctx().data_mut(|d| d.insert_temp(state_key, state));
    out
}

/// Expand a byte offset to the enclosing word's UTF-16 [`TextSelection`].
fn word_selection_at(text: &str, mapper: &OffsetMapper, byte_offset: usize) -> TextSelection {
    if text.is_empty() {
        return TextSelection::default();
    }
    let idx = clamp_char_boundary(text, byte_offset);
    let mut start = idx;
    for (b, ch) in text[..idx].char_indices().rev() {
        if ch.is_alphanumeric() || ch == '_' {
            start = b;
        } else {
            break;
        }
    }
    let mut end = idx;
    for (rel_b, ch) in text[idx..].char_indices() {
        if ch.is_alphanumeric() || ch == '_' {
            end = idx + rel_b + ch.len_utf8();
        } else {
            break;
        }
    }
    if let Some((off, len)) = mapper.utf8_range_to_utf16(start..end) {
        let from = off.min(u16::MAX as usize) as u16;
        let to = off.saturating_add(len).min(u16::MAX as usize) as u16;
        TextSelection::new(from, to)
    } else {
        TextSelection::default()
    }
}

/// Hit-test a local point `rel_pos` inside a single `egui::Galley` representing `slice`.
///
/// Returns `(utf8_byte_offset, after_char, upon_char)`.
fn hit_test_galley(galley: &Galley, slice: &str, rel_pos: Pos2) -> (usize, bool, bool) {
    if galley.rows.is_empty() || slice.is_empty() {
        return (0, false, false);
    }

    // Map char index -> UTF-8 byte offset once.
    let char_bytes: Vec<usize> = slice
        .char_indices()
        .map(|(b, _)| b)
        .chain(std::iter::once(slice.len()))
        .collect();

    // Find the row containing `rel_pos.y`, or clamp to first/last row.
    let mut target_row_idx = galley.rows.len() - 1;
    let mut row_char_start = 0usize;
    let mut acc_chars = 0usize;

    if rel_pos.y < galley.rows[0].pos.y {
        return (0, false, false);
    }

    for (idx, placed) in galley.rows.iter().enumerate() {
        let top = placed.pos.y;
        let bottom = top + placed.row.size.y;
        if rel_pos.y >= top && rel_pos.y <= bottom {
            target_row_idx = idx;
            row_char_start = acc_chars;
            break;
        }
        if idx + 1 == galley.rows.len() {
            target_row_idx = idx;
            row_char_start = acc_chars;
        }
        acc_chars += placed.row.glyphs.len();
        if placed.ends_with_newline {
            acc_chars += 1;
        }
    }

    let placed = &galley.rows[target_row_idx];
    let row_y_ok = rel_pos.y >= placed.pos.y && rel_pos.y <= placed.pos.y + placed.row.size.y;
    let local_x = rel_pos.x - placed.pos.x;

    if placed.row.glyphs.is_empty() {
        let byte_off = char_bytes
            .get(row_char_start)
            .copied()
            .unwrap_or(slice.len());
        return (byte_off, false, false);
    }

    for (gi, g) in placed.row.glyphs.iter().enumerate() {
        let g_left = g.pos.x;
        let g_right = g.pos.x + g.advance_width;
        if local_x < g_left && gi == 0 {
            let byte_off = char_bytes.get(row_char_start).copied().unwrap_or(0);
            return (byte_off, false, false);
        }
        if local_x >= g_left && local_x <= g_right {
            let mid = (g_left + g_right) * 0.5;
            let after = local_x >= mid;
            // An emoji is one glyph followed by zero-width glyphs for the rest of its chars
            // (joiners, selectors, the second flag letter): past its middle, the cursor goes
            // after the whole sequence, like tdesktop's emoji block.
            let cluster_tail = if after {
                placed.row.glyphs[gi + 1..]
                    .iter()
                    .take_while(|next| next.advance_width == 0.0)
                    .count()
            } else {
                0
            };
            let ch_idx = row_char_start + gi + usize::from(after) + cluster_tail;
            let byte_off = char_bytes.get(ch_idx).copied().unwrap_or(slice.len());
            return (byte_off, after, row_y_ok);
        }
    }

    // Past the right edge of the row.
    let end_char = row_char_start + placed.row.glyphs.len();
    let byte_off = char_bytes.get(end_char).copied().unwrap_or(slice.len());
    (byte_off, true, false)
}

/// Map a UTF-8 byte range `[start_byte, end_byte)` in `full_text` to per-row screen rectangles
/// in `galley`.
fn byte_range_rects(
    galley: &Galley,
    full_text: &str,
    start_byte: usize,
    end_byte: usize,
    origin: Pos2,
) -> Vec<Rect> {
    let start_b = clamp_char_boundary(full_text, start_byte.min(full_text.len()));
    let end_b = clamp_char_boundary(full_text, end_byte.min(full_text.len()));
    if end_b <= start_b {
        return Vec::new();
    }
    let start_char = full_text[..start_b].chars().count();
    let end_char = full_text[..end_b].chars().count();
    if end_char <= start_char {
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

        let ov_start = start_char.max(row_start);
        let ov_end = end_char.min(row_end);
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
        // Account for newline characters skipped between rows.
        global_char = row_end;
        if placed.ends_with_newline {
            global_char += 1;
        }
    }

    rects
}

#[cfg(test)]
mod wp25_tests {
    use super::*;
    use crate::custom_emoji::CustomEmojiAsset;
    use crate::fonts::register_tdesktop_fonts;

    struct TestColors;

    impl crate::theme::Colors for TestColors {
        fn get(&self, _name: &str) -> Option<Color32> {
            Some(Color32::from_rgb(0x40, 0x80, 0xC0))
        }
    }

    #[test]
    fn pre_code_block_applies_prism_syntax_colors_in_galley() {
        let ctx = egui::Context::default();
        register_tdesktop_fonts(&ctx);
        let mut init = ctx.run_ui(egui::RawInput::default(), |_| {});
        init.textures_delta.clear();

        let code = "let x = 42; // answer";
        let src = TextWithEntities::with_entities(
            code,
            vec![EntityInText::with_data(
                EntityType::Pre,
                0,
                tg_text::utf16_len(code) as i32,
                "rust",
            )],
        );
        let style = TextStyle::default_style();
        let palette = TextPalette::default_palette(&TestColors);
        let syntax_pal = SyntaxHighlightPalette::day();
        let mut opts = RichTextOptions::new(Color32::BLACK, 360.0);
        opts.syntax_palette = Some(syntax_pal);

        let laid = layout_text_with_entities(&ctx, &src, &style, &palette, &opts);
        assert_eq!(laid.blocks.len(), 1);
        let job = &laid.blocks[0].galley.job;

        let color_of = |needle: &str| {
            let start = egui::text::ByteIndex(code.find(needle).expect("needle present"));
            let end = egui::text::ByteIndex(start.0 + needle.len());
            job.sections
                .iter()
                .find(|s| s.byte_range.start <= start && s.byte_range.end >= end)
                .map(|s| s.format.color)
        };
        assert_eq!(color_of("let"), Some(syntax_pal.keyword));
        assert_eq!(color_of("42"), Some(syntax_pal.literal));
        assert_eq!(color_of("// answer"), Some(syntax_pal.comment));
    }

    #[test]
    fn inline_custom_emoji_paints_registered_lottie_asset() {
        let ctx = egui::Context::default();
        register_tdesktop_fonts(&ctx);
        let mut init = ctx.run_ui(egui::RawInput::default(), |_| {});
        init.textures_delta.clear();

        let lottie = r#"{"v":"5.5.2","fr":30,"ip":0,"op":10,"w":32,"h":32,"layers":[{"ty":4,"nm":"s","ip":0,"op":10,"st":0,"ks":{"o":{"a":0,"k":100},"r":{"a":0,"k":0},"p":{"a":0,"k":[16,16,0]},"a":{"a":0,"k":[0,0,0]},"s":{"a":0,"k":[100,100,100]}},"shapes":[{"ty":"gr","it":[{"ty":"el","p":{"a":0,"k":[0,0]},"s":{"a":0,"k":[16,16]}},{"ty":"fl","c":{"a":0,"k":[0.2,0.6,1.0,1]},"o":{"a":0,"k":100}},{"ty":"tr","p":{"a":0,"k":[0,0]},"a":{"a":0,"k":[0,0]},"s":{"a":0,"k":[100,100]},"r":{"a":0,"k":0},"o":{"a":0,"k":100}}]}]}]}"#;
        let mut renderer = CustomEmojiRenderer::new();
        assert!(renderer.register_asset("777001", &CustomEmojiAsset::lottie(lottie)));

        let text = "Hello ✨ world";
        let emoji_off = tg_text::utf16_len("Hello ") as i32;
        let emoji_len = tg_text::utf16_len("✨") as i32;
        let src = TextWithEntities::with_entities(
            text,
            vec![EntityInText::with_data(
                EntityType::CustomEmoji,
                emoji_off,
                emoji_len,
                "777001",
            )],
        );
        let style = TextStyle::default_style();
        let palette = TextPalette::default_palette(&TestColors);
        let opts = RichTextOptions::new(Color32::WHITE, 260.0);

        let laid = layout_text_with_entities(&ctx, &src, &style, &palette, &opts);
        assert_eq!(laid.custom_emojis.len(), 1);
        assert_eq!(laid.custom_emojis[0].document_id, "777001");

        let mut rendered_rect = Rect::NOTHING;
        let mut frame = ctx.run_ui(egui::RawInput::default(), |ui| {
            let resp = render_text_with_entities_and_emoji(
                ui,
                Id::unique("wp25_custom_emoji_test"),
                &src,
                &style,
                &palette,
                &opts,
                Some(&mut renderer),
            );
            rendered_rect = resp.rect;
        });
        frame.textures_delta.clear();
        assert!(rendered_rect.width() > 0.0 && rendered_rect.height() > 0.0);
    }
}
