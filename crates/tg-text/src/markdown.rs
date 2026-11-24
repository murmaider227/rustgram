//! Markdown delimiter matching and Markdown -> entities/tags parsing for sending.
//!
//! Port of `Markdown*GoodBefore`/`Markdown*BadAfter` from `Telegram/lib_ui/ui/text/text_entity.cpp`
//! and `TagSearchItem`, `MarkdownTagAccumulator`, `ReadPreLanguageName`, and
//! `InputField::getTextWithAppliedMarkdown` from `Telegram/lib_ui/ui/widgets/fields/input_field.cpp`,
//! extended with `[text](url)` link parsing and nested inline markdown support.

use std::sync::OnceLock;

use regex::Regex;

use crate::char_class::{
    LINE_SEPARATOR, PARAGRAPH_SEPARATOR, TEXT_BEGINNING_OF_FRAME, TEXT_END_OF_FRAME,
    is_input_newline,
};
use crate::entity::{
    EntitiesInText, EntityInText, EntityType, TextParseFlags, TextWithEntities, sort_entities,
};
use crate::parse::parse_entities;
use crate::tags::{
    TAG_BOLD, TAG_CODE, TAG_ITALIC, TAG_PRE, TAG_SPOILER, TAG_STRIKE_OUT, TextTag, TextWithTags,
    convert_text_tags_to_entities, is_mention_link, is_valid_markdown_link, mention_entity_data,
};
use crate::url::validate_url;
use crate::utf16::{OffsetMapper, slice_utf16, utf16_len};

const QUOTES: &str = "\u{00AB}\u{00BB}\u{201C}\u{201D}\u{2018}\u{2019}\u{2026}";

fn separators(additional: &str) -> String {
    let mut s = String::from(" \x10\n\r\t.,:;<>|'\"[]{}!?%^()-+=");
    s.push(TEXT_BEGINNING_OF_FRAME);
    s.push(TEXT_END_OF_FRAME);
    s.push(PARAGRAPH_SEPARATOR);
    s.push(LINE_SEPARATOR);
    s.push_str(QUOTES);
    s.push_str(additional);
    s
}

/// Characters allowed immediately before `**` (`TextUtilities::MarkdownBoldGoodBefore`).
pub fn markdown_bold_good_before() -> String {
    separators("`~/")
}

/// Characters forbidden immediately after opening / before closing `**`
/// (`TextUtilities::MarkdownBoldBadAfter`).
pub fn markdown_bold_bad_after() -> &'static str {
    "*"
}

/// Characters allowed immediately before `__` (`TextUtilities::MarkdownItalicGoodBefore`).
pub fn markdown_italic_good_before() -> String {
    separators("`*~/")
}

/// Characters forbidden immediately after opening / before closing `__`
/// (`TextUtilities::MarkdownItalicBadAfter`).
pub fn markdown_italic_bad_after() -> &'static str {
    "_"
}

/// Characters allowed immediately before `~~` (`TextUtilities::MarkdownStrikeOutGoodBefore`).
pub fn markdown_strike_out_good_before() -> String {
    separators("`*~/")
}

/// Characters forbidden immediately after opening / before closing `~~`
/// (`TextUtilities::MarkdownStrikeOutBadAfter`).
pub fn markdown_strike_out_bad_after() -> &'static str {
    "~"
}

/// Characters allowed immediately before `` ` `` (`TextUtilities::MarkdownCodeGoodBefore`).
pub fn markdown_code_good_before() -> String {
    separators("*~/")
}

/// Characters forbidden immediately after opening / before closing `` ` ``
/// (`TextUtilities::MarkdownCodeBadAfter`).
pub fn markdown_code_bad_after() -> &'static str {
    "`\n\r"
}

/// Characters allowed immediately before ```` ``` ```` (`TextUtilities::MarkdownPreGoodBefore`).
pub fn markdown_pre_good_before() -> String {
    separators("*~/")
}

/// Characters forbidden immediately after opening / before closing ```` ``` ````
/// (`TextUtilities::MarkdownPreBadAfter`).
pub fn markdown_pre_bad_after() -> &'static str {
    "`"
}

/// Characters allowed immediately before `||` (`TextUtilities::MarkdownSpoilerGoodBefore`).
pub fn markdown_spoiler_good_before() -> String {
    separators("|*~/")
}

/// Characters forbidden immediately after opening / before closing `||`
/// (`TextUtilities::MarkdownSpoilerBadAfter`).
pub fn markdown_spoiler_bad_after() -> &'static str {
    "|"
}

/// Matched markdown delimiter span (`Ui::InputField::MarkdownTag`).
///
/// Offsets and lengths are in UTF-16 code units.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MarkdownTag {
    /// Start in internal document coordinates.
    pub internal_start: i32,
    /// Length in internal document coordinates (`-1` while open).
    pub internal_length: i32,
    /// Start in adjusted UTF-16 text coordinates.
    pub adjusted_start: i32,
    /// Length in adjusted UTF-16 text coordinates (`-1` while open).
    pub adjusted_length: i32,
    /// Whether the closing delimiter was matched.
    pub closed: bool,
    /// Delimiter tag string (`"**"`, `"__"`, `"~~"`, ``"`"``, ````"```"````, `"||"`).
    pub tag: String,
}

/// Reads an optional programming language name on the first line of a ```` ``` ```` block
/// (`ReadPreLanguageName` in `input_field.cpp`).
pub fn read_pre_language_name(text: &str, pre_start_u16: usize, pre_length_u16: usize) -> String {
    let Some(view) = slice_utf16(text, pre_start_u16, pre_length_u16) else {
        return String::new();
    };
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^([a-zA-Z0-9\+\-]+)[\r\n]").expect("valid lang regex"));
    re.captures(view)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_lowercase())
        .unwrap_or_default()
}

struct TagStartExpression {
    tag: &'static str,
    good_before: String,
    bad_after: &'static str,
    bad_before: &'static str,
    good_after: String,
}

fn tag_start_expressions() -> &'static [TagStartExpression] {
    static EXPRS: OnceLock<Vec<TagStartExpression>> = OnceLock::new();
    EXPRS.get_or_init(|| {
        vec![
            TagStartExpression {
                tag: TAG_BOLD,
                good_before: markdown_bold_good_before(),
                bad_after: markdown_bold_bad_after(),
                bad_before: markdown_bold_bad_after(),
                good_after: markdown_bold_good_before(),
            },
            TagStartExpression {
                tag: TAG_ITALIC,
                good_before: markdown_italic_good_before(),
                bad_after: markdown_italic_bad_after(),
                bad_before: markdown_italic_bad_after(),
                good_after: markdown_italic_good_before(),
            },
            TagStartExpression {
                tag: TAG_STRIKE_OUT,
                good_before: markdown_strike_out_good_before(),
                bad_after: markdown_strike_out_bad_after(),
                bad_before: markdown_strike_out_bad_after(),
                good_after: String::new(),
            },
            TagStartExpression {
                tag: TAG_CODE,
                good_before: markdown_code_good_before(),
                bad_after: markdown_code_bad_after(),
                bad_before: markdown_code_bad_after(),
                good_after: markdown_code_good_before(),
            },
            TagStartExpression {
                tag: TAG_PRE,
                good_before: markdown_pre_good_before(),
                bad_after: markdown_pre_bad_after(),
                bad_before: markdown_pre_bad_after(),
                good_after: markdown_pre_good_before(),
            },
            TagStartExpression {
                tag: TAG_SPOILER,
                good_before: markdown_spoiler_good_before(),
                bad_after: markdown_spoiler_bad_after(),
                bad_before: markdown_spoiler_bad_after(),
                good_after: markdown_spoiler_good_before(),
            },
        ]
    })
}

fn tag_index_for(tag: &str) -> Option<usize> {
    match tag {
        TAG_BOLD => Some(0),
        TAG_ITALIC => Some(1),
        TAG_STRIKE_OUT => Some(2),
        TAG_CODE => Some(3),
        TAG_PRE => Some(4),
        TAG_SPOILER => Some(5),
        _ => None,
    }
}

const INVALID_POS: i32 = i32::MAX / 2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Edge {
    Open = 0,
    Close = 1,
}

#[derive(Clone, Default)]
struct TagSearchItem {
    offset: i32,
    position: i32,
    checked: [bool; 2],
}

impl TagSearchItem {
    fn new() -> Self {
        Self {
            offset: 0,
            position: -1,
            checked: [false, false],
        }
    }

    fn match_position(&self) -> i32 {
        if self.position >= 0 {
            self.position
        } else {
            INVALID_POS
        }
    }

    fn apply_offset(&mut self, offset: i32) {
        if self.position < offset {
            self.position = -1;
        }
        self.offset = self.offset.max(offset);
    }

    fn fill(&mut self, units: &[u16], edge: Edge, expr: &TagStartExpression) {
        let len = units.len() as i32;
        let tag_units: Vec<u16> = expr.tag.encode_utf16().collect();
        let tag_len = tag_units.len() as i32;

        let is_good_before =
            |ch: char| expr.good_before.is_empty() || expr.good_before.contains(ch);
        let is_bad_after = |ch: char| !expr.bad_after.is_empty() && expr.bad_after.contains(ch);
        let is_bad_before = |ch: char| !expr.bad_before.is_empty() && expr.bad_before.contains(ch);
        let is_good_after = |ch: char| expr.good_after.is_empty() || expr.good_after.contains(ch);

        let check = |pos: i32, edge: Edge| -> bool {
            if pos > 0 {
                let before = char::from_u32(u32::from(units[(pos - 1) as usize])).unwrap_or('\0');
                if expr.tag == TAG_PRE && before != '\n' && before != '\r' {
                    return false;
                }
                if (edge == Edge::Open && !is_good_before(before))
                    || (edge == Edge::Close && is_bad_before(before))
                {
                    return false;
                }
            }
            if pos + tag_len < len {
                let after =
                    char::from_u32(u32::from(units[(pos + tag_len) as usize])).unwrap_or('\0');
                if (edge == Edge::Open && is_bad_after(after))
                    || (edge == Edge::Close && !is_good_after(after))
                {
                    return false;
                }
            }
            true
        };

        let edge_idx = edge as usize;
        if self.position >= 0 {
            if self.checked[edge_idx] {
                return;
            } else if check(self.position, edge) {
                self.checked[edge_idx] = true;
                return;
            } else {
                self.checked = [false, false];
            }
        }

        loop {
            let found = find_utf16_subslice(units, &tag_units, self.offset.max(0) as usize);
            let Some(p) = found else {
                self.offset = INVALID_POS;
                self.position = INVALID_POS;
                break;
            };
            self.position = p as i32;
            self.offset = self.position + tag_len;
            if check(self.position, edge) {
                break;
            }
        }

        if self.position == INVALID_POS {
            self.checked = [true, true];
        } else {
            self.checked = [false, false];
            self.checked[edge_idx] = true;
        }
    }
}

fn find_utf16_subslice(haystack: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return (from <= haystack.len()).then_some(from);
    }
    if from + needle.len() > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|rel| from + rel)
}

/// Scans text chunks for tdesktop markdown tags (`MarkdownTagAccumulator` in `input_field.cpp`).
pub struct MarkdownTagAccumulator {
    tags: Vec<MarkdownTag>,
    items: Vec<TagSearchItem>,
    current_tag: usize,
    current_free_tag: usize,
    current_internal_length: i32,
    current_adjusted_length: i32,
}

impl Default for MarkdownTagAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkdownTagAccumulator {
    /// Creates a new `MarkdownTagAccumulator`.
    pub fn new() -> Self {
        let count = tag_start_expressions().len();
        Self {
            tags: Vec::new(),
            items: vec![TagSearchItem::new(); count],
            current_tag: 0,
            current_free_tag: 0,
            current_internal_length: 0,
            current_adjusted_length: 0,
        }
    }

    /// Feeds a text segment with its adjusted UTF-16 length and existing formatting tag.
    pub fn feed(&mut self, text: &str, adjusted_text_length: i32, text_tag: &str) {
        let units: Vec<u16> = text.encode_utf16().collect();
        let internal_len = units.len() as i32;

        if !text_tag.is_empty() {
            self.finish_tags();
            self.current_internal_length += internal_len;
            self.current_adjusted_length += adjusted_text_length;
            return;
        }

        for item in &mut self.items {
            *item = TagSearchItem::new();
        }
        let exprs = tag_start_expressions();
        let mut try_finish_tag = self.current_tag;

        loop {
            while try_finish_tag != self.current_free_tag {
                if self.tags[try_finish_tag].internal_length >= 0 {
                    try_finish_tag += 1;
                    continue;
                }
                let Some(tag_idx) = tag_index_for(&self.tags[try_finish_tag].tag) else {
                    try_finish_tag += 1;
                    continue;
                };
                let tag_u16_len = utf16_len(&self.tags[try_finish_tag].tag) as i32;
                let at_least_offset = self.tags[try_finish_tag].internal_start + tag_u16_len + 1
                    - self.current_internal_length;
                self.items[tag_idx].apply_offset(at_least_offset);
                self.items[tag_idx].fill(&units, Edge::Close, &exprs[tag_idx]);

                if self.finish_by_newline(try_finish_tag, &units, tag_idx) {
                    try_finish_tag += 1;
                    continue;
                }
                let position = self.items[tag_idx].match_position();
                if position < INVALID_POS {
                    let till = position + tag_u16_len;
                    self.finish_tag(try_finish_tag, till, true);
                    self.items[tag_idx].apply_offset(till);
                }
                try_finish_tag += 1;
            }

            for (i, expr) in exprs.iter().enumerate() {
                self.items[i].fill(&units, Edge::Open, expr);
            }

            let min_idx = self.min_open_index();
            let Some(min) = min_idx else {
                break;
            };
            let pos = self.items[min].match_position();
            self.start_tag(pos, exprs[min].tag);
        }

        self.current_internal_length += internal_len;
        self.current_adjusted_length += adjusted_text_length;
    }

    /// Finalizes scanning and returns the matched `MarkdownTag` list.
    pub fn finish(mut self) -> Vec<MarkdownTag> {
        self.finish_tags();
        self.tags.truncate(self.current_tag);
        self.tags
    }

    fn finish_tag(&mut self, index: usize, offset_from_accumulated: i32, closed: bool) {
        if self.tags[index].internal_length < 0 {
            self.tags[index].internal_length = self.current_internal_length
                + offset_from_accumulated
                - self.tags[index].internal_start;
            self.tags[index].adjusted_length = self.current_adjusted_length
                + offset_from_accumulated
                - self.tags[index].adjusted_start;
            self.tags[index].closed = closed;
        }
        if index == self.current_tag {
            self.current_tag += 1;
        }
    }

    fn finish_by_newline(&mut self, index: usize, units: &[u16], tag_idx: usize) -> bool {
        if self.tags[index].tag != TAG_CODE {
            return false;
        }
        let start_off =
            (self.tags[index].internal_start + 1 - self.current_internal_length).max(0) as usize;
        let mut end_pos = INVALID_POS;
        for (i, &u) in units.iter().enumerate().skip(start_off) {
            if char::from_u32(u32::from(u)).is_some_and(is_input_newline) {
                end_pos = i as i32;
                break;
            }
        }
        if self.items[tag_idx].match_position() <= end_pos {
            return false;
        }
        self.finish_tag(index, end_pos, false);
        true
    }

    fn finish_tags(&mut self) {
        while self.current_tag != self.current_free_tag {
            self.finish_tag(self.current_tag, 0, false);
        }
    }

    fn start_tag(&mut self, offset_from_accumulated: i32, tag: &str) {
        let new_tag = MarkdownTag {
            internal_start: self.current_internal_length + offset_from_accumulated,
            internal_length: -1,
            adjusted_start: self.current_adjusted_length + offset_from_accumulated,
            adjusted_length: -1,
            closed: false,
            tag: tag.to_string(),
        };
        if self.current_free_tag < self.tags.len() {
            self.tags[self.current_free_tag] = new_tag;
        } else {
            self.tags.push(new_tag);
        }
        self.current_free_tag += 1;
    }

    fn min_open_index(&self) -> Option<usize> {
        let mut best: Option<usize> = None;
        let mut min_pos = INVALID_POS;
        for (i, item) in self.items.iter().enumerate() {
            let pos = item.match_position();
            if pos < min_pos {
                min_pos = pos;
                best = Some(i);
            }
        }
        best
    }
}

/// Scans a `TextWithTags` with `MarkdownTagAccumulator` (skipping ranges already covered by tags).
pub fn find_markdown_tags(text_with_tags: &TextWithTags) -> Vec<MarkdownTag> {
    let mut acc = MarkdownTagAccumulator::new();
    let total_u16 = utf16_len(&text_with_tags.text);
    let mut pos = 0usize;
    for tag in &text_with_tags.tags {
        let start = (tag.offset.max(0) as usize).min(total_u16);
        let end = ((tag.offset + tag.length).max(0) as usize).min(total_u16);
        if start > pos
            && let Some(chunk) = slice_utf16(&text_with_tags.text, pos, start - pos)
        {
            acc.feed(chunk, (start - pos) as i32, "");
        }
        if end > start
            && let Some(chunk) = slice_utf16(&text_with_tags.text, start, end - start)
        {
            acc.feed(chunk, (end - start) as i32, &tag.id);
        }
        pos = pos.max(end);
    }
    if total_u16 > pos
        && let Some(chunk) = slice_utf16(&text_with_tags.text, pos, total_u16 - pos)
    {
        acc.feed(chunk, (total_u16 - pos) as i32, "");
    }
    acc.finish()
}

/// Applies typed Markdown tags to `text_with_tags`, stripping delimiters and preserving links
/// (`InputField::getTextWithAppliedMarkdown`).
pub fn apply_markdown(text_with_tags: &TextWithTags) -> TextWithTags {
    let markdown_tags = find_markdown_tags(text_with_tags);
    apply_markdown_with_tags(text_with_tags, &markdown_tags)
}

/// Applies precomputed `markdown_tags` to `text_with_tags` (`InputField::getTextWithAppliedMarkdown`).
pub fn apply_markdown_with_tags(
    text_with_tags: &TextWithTags,
    markdown_tags: &[MarkdownTag],
) -> TextWithTags {
    if markdown_tags.is_empty() {
        return text_with_tags.clone();
    }
    let original_text = &text_with_tags.text;
    let original_tags = &text_with_tags.tags;
    let total_u16 = utf16_len(original_text) as i32;

    // Ignore tags that partially intersect detected links (e.g. http://test.com/__test__/test).
    let links = parse_entities(original_text, TextParseFlags::empty()).entities;

    let mut result = TextWithTags::new();
    result.text.reserve(original_text.len());
    result
        .tags
        .reserve(original_tags.len() + markdown_tags.len());

    let mut removed = 0i32;
    let mut orig_tag_idx = 0usize;
    let mut from = 0i32;
    let mut link_idx = 0usize;

    let add_original_tags_up_till =
        |offset: i32, orig_tag_idx: &mut usize, removed: i32, result: &mut TextWithTags| {
            while *orig_tag_idx < original_tags.len()
                && original_tags[*orig_tag_idx].offset + original_tags[*orig_tag_idx].length
                    <= offset
            {
                let mut t = original_tags[*orig_tag_idx].clone();
                t.offset -= removed;
                result.tags.push(t);
                *orig_tag_idx += 1;
            }
        };

    let add_original_text_up_till = |offset: i32, from: i32, result: &mut TextWithTags| {
        if offset > from
            && let Some(chunk) = slice_utf16(original_text, from as usize, (offset - from) as usize)
        {
            result.text.push_str(chunk);
        }
    };

    for tag in markdown_tags {
        let tag_length = utf16_len(&tag.tag) as i32;
        if !tag.closed || tag.adjusted_start < from {
            continue;
        }
        let mut entity_length = tag.adjusted_length - 2 * tag_length;
        if entity_length <= 0 {
            continue;
        }
        add_original_tags_up_till(tag.adjusted_start, &mut orig_tag_idx, removed, &mut result);
        let tag_adjusted_end = tag.adjusted_start + tag.adjusted_length;
        if orig_tag_idx < original_tags.len()
            && original_tags[orig_tag_idx].offset < tag_adjusted_end
        {
            continue;
        }
        while link_idx < links.len()
            && links[link_idx].offset() + links[link_idx].length() <= tag.adjusted_start
        {
            link_idx += 1;
        }
        if link_idx < links.len()
            && links[link_idx].offset() < tag_adjusted_end
            && (links[link_idx].offset() + links[link_idx].length() > tag_adjusted_end
                || links[link_idx].offset() < tag.adjusted_start)
        {
            continue;
        }

        add_original_text_up_till(tag.adjusted_start, from, &mut result);

        let mut tag_id = tag.tag.clone();
        let mut entity_start = tag.adjusted_start + tag_length;
        if tag_id == TAG_PRE {
            let lang = read_pre_language_name(
                original_text,
                entity_start as usize,
                entity_length as usize,
            );
            if !lang.is_empty() {
                let lang_u16 = utf16_len(&lang) as i32;
                entity_start += lang_u16 + 1;
                entity_length -= lang_u16 + 1;
                tag_id.push_str(&lang);
            } else if slice_utf16(original_text, entity_start as usize, 1)
                .and_then(|s| s.chars().next())
                .is_some_and(is_input_newline)
                && (result.text.is_empty()
                    || result
                        .text
                        .chars()
                        .next_back()
                        .is_some_and(is_input_newline))
            {
                entity_start += 1;
                entity_length -= 1;
            }
            if entity_length > 0 {
                let entity_end = entity_start + entity_length;
                let last_is_nl = slice_utf16(original_text, (entity_end - 1) as usize, 1)
                    .and_then(|s| s.chars().next())
                    .is_some_and(is_input_newline);
                let after_is_nl = total_u16 <= entity_end + tag_length
                    || slice_utf16(original_text, (entity_end + tag_length) as usize, 1)
                        .and_then(|s| s.chars().next())
                        .is_some_and(is_input_newline);
                if last_is_nl && after_is_nl {
                    entity_length -= 1;
                }
            }
        }

        if entity_length > 0 {
            let out_offset = utf16_len(&result.text) as i32;
            result
                .tags
                .push(TextTag::new(out_offset, entity_length, tag_id));
            if let Some(chunk) =
                slice_utf16(original_text, entity_start as usize, entity_length as usize)
            {
                result.text.push_str(chunk);
            }
        }

        from = tag.adjusted_start + tag.adjusted_length;
        removed += tag.adjusted_length - entity_length;
    }

    add_original_tags_up_till(total_u16, &mut orig_tag_idx, removed, &mut result);
    add_original_text_up_till(total_u16, from, &mut result);
    result
}

// --- Full Markdown parser for sending (supporting nested formatting and [text](url) links) ---

/// Parses Telegram Markdown into `TextWithEntities` with UTF-16 offsets.
///
/// Supports:
/// - `**bold**`
/// - `__italic__`
/// - `~~strike~~`
/// - `` `code` `` (no nested markdown inside)
/// - ```` ```[lang]\npre``` ```` (no nested markdown inside, strips redundant outer newlines)
/// - `||spoiler||`
/// - `[text](url)` markdown links (supports balanced parentheses in URLs and nested inline formatting in `text`)
/// - Nested inline formatting (e.g. `**bold __and italic__**`, `||**hidden**||`)
/// - Preserves URLs containing underscores or symbols (e.g. `https://example.com/__test__/a`).
pub fn parse_markdown(text: &str) -> TextWithEntities {
    let mut result = TextWithEntities::simple(text);
    parse_markdown_in_place(&mut result);
    result
}

/// Parses Markdown in place on `result`, merging any pre-existing entities if present.
pub fn parse_markdown_in_place(result: &mut TextWithEntities) {
    if !result.entities.is_empty() {
        // When entities already exist, apply tdesktop's tag-preserving pipeline so existing
        // entities are shifted accurately without disturbing them.
        let as_tags = TextWithTags {
            text: result.text.clone(),
            tags: crate::tags::convert_entities_to_text_tags(&result.entities),
        };
        let applied = apply_markdown(&as_tags);
        result.text = applied.text;
        result.entities = convert_text_tags_to_entities(&applied.tags);
        return;
    }

    let links = parse_entities(&result.text, TextParseFlags::empty()).entities;
    let mapper = OffsetMapper::new(&result.text);
    let link_byte_ranges: Vec<(usize, usize)> = links
        .iter()
        .filter_map(|e| {
            mapper
                .utf16_range_to_utf8(e.offset() as usize, e.length() as usize)
                .map(|r| (r.start, r.end))
        })
        .collect();

    let mut out_text = String::with_capacity(result.text.len());
    let mut out_entities: EntitiesInText = Vec::new();
    parse_markdown_segment(
        &result.text,
        0,
        result.text.len(),
        &link_byte_ranges,
        &mut out_text,
        &mut out_entities,
        0,
    );
    sort_entities(&mut out_entities);
    result.text = out_text;
    result.entities = out_entities;
}

const MAX_MARKDOWN_NESTING: usize = 16;

fn intersects_link_partially(start: usize, end: usize, link_ranges: &[(usize, usize)]) -> bool {
    link_ranges
        .iter()
        .any(|&(ls, le)| ls < end && le > start && (ls < start || le > end))
}

fn check_delimiter_open(
    full: &str,
    pos: usize,
    delim: &str,
    good_before: &str,
    bad_after: &str,
) -> bool {
    if pos > 0
        && let Some(before) = full[..pos].chars().next_back()
    {
        if delim == TAG_PRE && before != '\n' && before != '\r' {
            return false;
        }
        if !good_before.is_empty() && !good_before.contains(before) {
            return false;
        }
    }
    let after_pos = pos + delim.len();
    if after_pos < full.len()
        && let Some(after) = full[after_pos..].chars().next()
        && !bad_after.is_empty()
        && bad_after.contains(after)
    {
        return false;
    }
    true
}

fn check_delimiter_close(
    full: &str,
    pos: usize,
    delim: &str,
    bad_before: &str,
    good_after: &str,
) -> bool {
    if pos > 0
        && let Some(before) = full[..pos].chars().next_back()
    {
        if delim == TAG_PRE && before != '\n' && before != '\r' {
            return false;
        }
        if !bad_before.is_empty() && bad_before.contains(before) {
            return false;
        }
    }
    let after_pos = pos + delim.len();
    if after_pos < full.len()
        && let Some(after) = full[after_pos..].chars().next()
        && !good_after.is_empty()
        && !good_after.contains(after)
    {
        return false;
    }
    true
}

fn find_closing_delimiter(
    full: &str,
    search_start: usize,
    seg_end: usize,
    delim: &str,
    bad_before: &str,
    good_after: &str,
    stop_at_newline: bool,
) -> Option<usize> {
    let mut pos = search_start;
    while pos + delim.len() <= seg_end {
        if stop_at_newline
            && let Some(ch) = full[pos..seg_end].chars().next()
            && is_input_newline(ch)
        {
            return None;
        }
        if full[pos..seg_end].starts_with(delim)
            && check_delimiter_close(full, pos, delim, bad_before, good_after)
        {
            return Some(pos);
        }
        let step = full[pos..seg_end].chars().next().map_or(1, char::len_utf8);
        pos += step;
    }
    None
}

/// Attempts to parse `[label](url)` starting at `pos` inside `full[..seg_end]`.
/// Returns `(label_start, label_end, url_str, total_end_byte, entity_type)`.
fn try_parse_markdown_link(
    full: &str,
    pos: usize,
    seg_end: usize,
) -> Option<(usize, usize, String, usize, EntityType)> {
    if !full[pos..seg_end].starts_with('[') {
        return None;
    }
    // Find matching ']' supporting nested brackets inside label (e.g. `[foo [bar]](url)`).
    let label_start = pos + 1;
    let mut i = label_start;
    let mut bracket_depth = 1usize;
    let mut label_end = None;
    while i < seg_end {
        let ch = full[i..seg_end].chars().next()?;
        if is_input_newline(ch) {
            return None;
        }
        if ch == '[' {
            bracket_depth += 1;
        } else if ch == ']' {
            bracket_depth -= 1;
            if bracket_depth == 0 {
                label_end = Some(i);
                break;
            }
        }
        i += ch.len_utf8();
    }
    let label_end = label_end?;
    if label_end == label_start {
        return None;
    }
    let paren_open = label_end + 1;
    if paren_open >= seg_end || full.as_bytes()[paren_open] != b'(' {
        return None;
    }

    // Find matching ')' supporting balanced parentheses inside the URL.
    let url_start = paren_open + 1;
    let mut j = url_start;
    let mut paren_depth = 1usize;
    let mut url_end = None;
    while j < seg_end {
        let ch = full[j..seg_end].chars().next()?;
        if ch.is_whitespace() || is_input_newline(ch) {
            return None;
        }
        if ch == '(' {
            paren_depth += 1;
        } else if ch == ')' {
            paren_depth -= 1;
            if paren_depth == 0 {
                url_end = Some(j);
                break;
            }
        }
        j += ch.len_utf8();
    }
    let url_end = url_end?;
    let raw_url = &full[url_start..url_end];
    if raw_url.is_empty() {
        return None;
    }

    if is_mention_link(raw_url) {
        let data = mention_entity_data(raw_url);
        if !data.is_empty() {
            return Some((
                label_start,
                label_end,
                data,
                url_end + 1,
                EntityType::MentionName,
            ));
        }
        return None;
    }

    let validated = validate_url(raw_url);
    if !validated.is_empty() {
        return Some((
            label_start,
            label_end,
            validated,
            url_end + 1,
            EntityType::CustomUrl,
        ));
    }
    if is_valid_markdown_link(raw_url) && raw_url.contains("://") {
        return Some((
            label_start,
            label_end,
            raw_url.to_string(),
            url_end + 1,
            EntityType::CustomUrl,
        ));
    }
    None
}

fn parse_markdown_segment(
    full: &str,
    seg_start: usize,
    seg_end: usize,
    link_ranges: &[(usize, usize)],
    out_text: &mut String,
    out_entities: &mut EntitiesInText,
    depth: usize,
) {
    if depth >= MAX_MARKDOWN_NESTING {
        out_text.push_str(&full[seg_start..seg_end]);
        return;
    }

    let exprs = tag_start_expressions();
    // Order: check ``` before ` so triple backticks match before single backticks.
    const ORDER: &[(usize, EntityType, bool)] = &[
        (4, EntityType::Pre, false),      // ```
        (3, EntityType::Code, false),     // `
        (0, EntityType::Bold, true),      // **
        (1, EntityType::Italic, true),    // __
        (2, EntityType::StrikeOut, true), // ~~
        (5, EntityType::Spoiler, true),   // ||
    ];

    let mut pos = seg_start;
    while pos < seg_end {
        // 1. Check for [text](url) link.
        if full.as_bytes()[pos] == b'['
            && let Some((l_start, l_end, url_data, after_link, entity_type)) =
                try_parse_markdown_link(full, pos, seg_end)
        {
            let start_u16 = utf16_len(out_text) as i32;
            parse_markdown_segment(
                full,
                l_start,
                l_end,
                link_ranges,
                out_text,
                out_entities,
                depth + 1,
            );
            let end_u16 = utf16_len(out_text) as i32;
            if end_u16 > start_u16 {
                out_entities.push(EntityInText::with_data(
                    entity_type,
                    start_u16,
                    end_u16 - start_u16,
                    url_data,
                ));
            }
            pos = after_link;
            continue;
        }

        // 2. Check for symmetric markdown delimiters.
        let mut matched = false;
        for &(expr_idx, entity_type, allow_nested) in ORDER {
            let expr = &exprs[expr_idx];
            if !full[pos..seg_end].starts_with(expr.tag) {
                continue;
            }
            if !check_delimiter_open(full, pos, expr.tag, &expr.good_before, expr.bad_after) {
                continue;
            }
            let inner_start = pos + expr.tag.len();
            if inner_start >= seg_end {
                continue;
            }
            let stop_at_nl = expr.tag == TAG_CODE;
            let Some(close_pos) = find_closing_delimiter(
                full,
                inner_start
                    + full[inner_start..seg_end]
                        .chars()
                        .next()
                        .map_or(1, char::len_utf8),
                seg_end,
                expr.tag,
                expr.bad_before,
                &expr.good_after,
                stop_at_nl,
            ) else {
                continue;
            };
            let full_end = close_pos + expr.tag.len();
            if intersects_link_partially(pos, full_end, link_ranges) {
                continue;
            }

            if entity_type == EntityType::Pre {
                let mut content_start = inner_start;
                let mut content_end = close_pos;
                let inner_slice = &full[content_start..content_end];
                let lang = read_pre_language_name(inner_slice, 0, utf16_len(inner_slice));
                if !lang.is_empty() {
                    // Advance past `lang` and the actual newline sequence (`\r\n` or single newline
                    // char, including multi-byte `\u{2028}`/`\u{2029}`) so UTF-8 char boundaries
                    // are never violated and `\r\n` does not leave a stray `\n` (`input_field.cpp`,
                    // `ReadPreLanguageName`).
                    content_start += lang.len();
                    let rest = &full[content_start..content_end];
                    if rest.starts_with("\r\n") {
                        content_start += 2;
                    } else if let Some(nl) = rest.chars().next().filter(|&c| is_input_newline(c)) {
                        content_start += nl.len_utf8();
                    }
                } else if full[content_start..content_end]
                    .chars()
                    .next()
                    .is_some_and(is_input_newline)
                    && (out_text.is_empty()
                        || out_text.chars().next_back().is_some_and(is_input_newline))
                {
                    content_start += full[content_start..content_end]
                        .chars()
                        .next()
                        .map_or(0, char::len_utf8);
                }
                if content_end > content_start {
                    let last_ch = full[content_start..content_end].chars().next_back();
                    let after_ch = full[full_end..seg_end].chars().next();
                    if last_ch.is_some_and(is_input_newline)
                        && after_ch.is_none_or(is_input_newline)
                    {
                        content_end -= last_ch.map_or(0, char::len_utf8);
                    }
                }
                if content_end > content_start {
                    let start_u16 = utf16_len(out_text) as i32;
                    out_text.push_str(&full[content_start..content_end]);
                    let end_u16 = utf16_len(out_text) as i32;
                    out_entities.push(EntityInText::with_data(
                        EntityType::Pre,
                        start_u16,
                        end_u16 - start_u16,
                        lang,
                    ));
                }
            } else if !allow_nested {
                let start_u16 = utf16_len(out_text) as i32;
                out_text.push_str(&full[inner_start..close_pos]);
                let end_u16 = utf16_len(out_text) as i32;
                if end_u16 > start_u16 {
                    out_entities.push(EntityInText::new(
                        entity_type,
                        start_u16,
                        end_u16 - start_u16,
                    ));
                }
            } else {
                let start_u16 = utf16_len(out_text) as i32;
                parse_markdown_segment(
                    full,
                    inner_start,
                    close_pos,
                    link_ranges,
                    out_text,
                    out_entities,
                    depth + 1,
                );
                let end_u16 = utf16_len(out_text) as i32;
                if end_u16 > start_u16 {
                    out_entities.push(EntityInText::new(
                        entity_type,
                        start_u16,
                        end_u16 - start_u16,
                    ));
                }
            }

            pos = full_end;
            matched = true;
            break;
        }

        if !matched {
            if let Some(ch) = full[pos..seg_end].chars().next() {
                out_text.push(ch);
                pos += ch.len_utf8();
            } else {
                break;
            }
        }
    }
}
