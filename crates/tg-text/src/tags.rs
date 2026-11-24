//! Tag representation, tag <-> entity conversion, and Qt-compatible tag serialization.
//!
//! Port of tag helpers in `Telegram/lib_ui/ui/text/text_entity.cpp/.h` and
//! `Telegram/lib_ui/ui/widgets/fields/input_field.cpp/.h`.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use regex::Regex;

use crate::char_class::{is_input_newline, is_space};
use crate::entity::{
    EntitiesInText, EntityInText, EntityType, FormattedDateFlags, deserialize_formatted_date_data,
    serialize_formatted_date_data,
};
use crate::url::external_url_from_internal_url;
use crate::utf16::{slice_utf16, utf16_len};

/// Separator between combined tags in a `TextTag::id` (`kTagSeparator`).
pub const TAG_SEPARATOR: char = '\\';
/// Bold tag (`InputField::kTagBold`).
pub const TAG_BOLD: &str = "**";
/// Italic tag (`InputField::kTagItalic`).
pub const TAG_ITALIC: &str = "__";
/// Underline tag (`InputField::kTagUnderline`).
pub const TAG_UNDERLINE: &str = "^^";
/// Strikethrough tag (`InputField::kTagStrikeOut`).
pub const TAG_STRIKE_OUT: &str = "~~";
/// Inline monospace tag (`InputField::kTagCode`).
pub const TAG_CODE: &str = "`";
/// Fenced code block tag prefix (`InputField::kTagPre`).
pub const TAG_PRE: &str = "```";
/// Spoiler tag (`InputField::kTagSpoiler`).
pub const TAG_SPOILER: &str = "||";
/// Blockquote tag (`InputField::kTagBlockquote`).
pub const TAG_BLOCKQUOTE: &str = ">";
/// Collapsed blockquote tag (`InputField::kTagBlockquoteCollapsed`).
pub const TAG_BLOCKQUOTE_COLLAPSED: &str = ">^";
/// Instant View marked/highlighted tag (`InputField::kTagIvMarked`).
pub const TAG_IV_MARKED: &str = "iv-marked";
/// Instant View subscript tag (`InputField::kTagIvSubscript`).
pub const TAG_IV_SUBSCRIPT: &str = "iv-subscript";
/// Instant View superscript tag (`InputField::kTagIvSuperscript`).
pub const TAG_IV_SUPERSCRIPT: &str = "iv-superscript";
/// Instant View math tag (`InputField::kTagIvMath`).
pub const TAG_IV_MATH: &str = "iv-math";
/// Mention link tag prefix (`TextUtilities::kMentionTagStart`).
pub const MENTION_TAG_START: &str = "mention://";
/// Custom emoji link tag prefix (`InputField::kCustomEmojiTagStart`).
pub const CUSTOM_EMOJI_TAG_START: &str = "custom-emoji://";
/// Custom date link tag prefix (`InputField::kCustomDateTagStart`).
pub const CUSTOM_DATE_TAG_START: &str = "custom-date://";
/// Formatted date metadata tag prefix (`kFormattedDateMetaTagPrefix`).
pub const FORMATTED_DATE_META_TAG_PREFIX: &str = "td-date-data-";
/// Internal check-link sentinel (`kTagCheckLinkMeta`).
pub const TAG_CHECK_LINK_META: &str = "^:/:/:^";
/// Clipboard MIME type for serialized tags (`TextUtilities::TagsMimeType`).
pub const TAGS_MIME_TYPE: &str = "application/x-td-field-tags";
/// Clipboard MIME type for tagged text (`TextUtilities::TagsTextMimeType`).
pub const TAGS_TEXT_MIME_TYPE: &str = "application/x-td-field-text";

static CUSTOM_EMOJI_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Single formatting tag span in UTF-16 code units (`TextWithTags::Tag`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct TextTag {
    /// Start offset in UTF-16 code units.
    pub offset: i32,
    /// Length in UTF-16 code units.
    pub length: i32,
    /// Combined `\`-separated tag identifier (e.g. `"**\\__"` or `"https://example.com"`).
    pub id: String,
}

impl TextTag {
    /// Creates a new `TextTag`.
    #[inline]
    pub fn new(offset: i32, length: i32, id: impl Into<String>) -> Self {
        Self {
            offset,
            length,
            id: id.into(),
        }
    }
}

/// Ordered list of non-overlapping `TextTag` spans (`TextWithTags::Tags`).
pub type TextTags = Vec<TextTag>;

/// Text paired with editor-style formatting tags (`TextWithTags`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct TextWithTags {
    /// UTF-8 text content.
    pub text: String,
    /// Non-overlapping tags indexed in UTF-16 code units.
    pub tags: TextTags,
}

impl TextWithTags {
    /// Creates an empty `TextWithTags`.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a `TextWithTags` with plain text and no tags.
    #[inline]
    pub fn simple(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tags: Vec::new(),
        }
    }

    /// Returns `true` if `text` is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// Returns `true` if `link` starts with `"mention://"` (`TextUtilities::IsMentionLink`).
#[inline]
pub fn is_mention_link(link: &str) -> bool {
    link.starts_with(MENTION_TAG_START)
}

/// Extracts `"userId.accessHash:selfId"` from a `"mention://..."` link (`TextUtilities::MentionEntityData`).
pub fn mention_entity_data(link: &str) -> String {
    let Some(rest) = link.strip_prefix(MENTION_TAG_START) else {
        return String::new();
    };
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^(\d+\.\d+:\d+)(/|$)").expect("valid mention regex"));
    re.captures(rest)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default()
}

/// Returns `true` if `link` starts with `"custom-emoji://"` (`InputField::IsCustomEmojiLink`).
#[inline]
pub fn is_custom_emoji_link(link: &str) -> bool {
    link.starts_with(CUSTOM_EMOJI_TAG_START)
}

/// Returns `true` if `link` starts with `"custom-date://"` (`InputField::IsCustomDateLink`).
#[inline]
pub fn is_custom_date_link(link: &str) -> bool {
    link.starts_with(CUSTOM_DATE_TAG_START)
}

/// Returns `true` if `link` is a valid markdown link tag (`InputField::IsValidMarkdownLink`).
#[inline]
pub fn is_valid_markdown_link(link: &str) -> bool {
    (link.contains('.') || link.contains(':'))
        && !is_custom_emoji_link(link)
        && !is_custom_date_link(link)
}

/// Returns `true` if `link` is an Instant View anchor link (`"#..."`, length > 1)
/// (`InputField::IsInstantViewAnchorLink`).
#[inline]
pub fn is_instant_view_anchor_link(link: &str) -> bool {
    link.len() > 1 && link.starts_with('#')
}

/// Returns `true` if `tag` is an Instant View editor tag (`InputField::IsInstantViewEditorTag`).
#[inline]
pub fn is_instant_view_editor_tag(tag: &str) -> bool {
    tag == TAG_IV_MARKED
        || tag == TAG_IV_SUBSCRIPT
        || tag == TAG_IV_SUPERSCRIPT
        || tag == TAG_IV_MATH
        || is_instant_view_anchor_link(tag)
}

/// Returns `true` if `tag` is a rich formatting tag (`TextUtilities::IsRichFormattingTag`).
#[inline]
pub fn is_rich_formatting_tag(tag: &str) -> bool {
    tag == TAG_IV_MARKED || tag == TAG_IV_SUBSCRIPT || tag == TAG_IV_SUPERSCRIPT
}

/// Returns `true` if `tag` is a rich link tag (`TextUtilities::IsRichLinkTag`).
#[inline]
pub fn is_rich_link_tag(tag: &str) -> bool {
    is_instant_view_anchor_link(tag)
}

/// Creates a unique custom emoji link tag (`"custom-emoji://<entity_data>?<counter>"`)
/// (`InputField::CustomEmojiLink`).
pub fn custom_emoji_link(entity_data: &str) -> String {
    let base = match entity_data.find('?') {
        Some(idx) => &entity_data[..idx],
        None => entity_data,
    };
    let n = CUSTOM_EMOJI_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    format!("{CUSTOM_EMOJI_TAG_START}{base}?{n}")
}

/// Extracts the entity data (document ID) from a `"custom-emoji://..."` tag
/// (`InputField::CustomEmojiEntityData`).
pub fn custom_emoji_entity_data(link: &str) -> String {
    let Some(rest) = link.strip_prefix(CUSTOM_EMOJI_TAG_START) else {
        return String::new();
    };
    let data = match rest.find('?') {
        Some(idx) => &rest[..idx],
        None => rest,
    };
    data.to_string()
}

/// Returns `true` for tags that cannot combine with arbitrary inline formatting
/// (`TextUtilities::IsSeparateTag`: `TAG_CODE` or `TAG_PRE`).
#[inline]
pub fn is_separate_tag(tag: &str) -> bool {
    tag == TAG_CODE || tag == TAG_PRE
}

/// Returns `true` if `tag` starts with `"```"` (`IsTagPre`).
#[inline]
pub fn is_tag_pre(tag: &str) -> bool {
    tag.starts_with(TAG_PRE)
}

/// Returns `true` if `tag` is a block-level tag (blockquote or pre) (`IsBlockTag`).
#[inline]
pub fn is_block_tag(tag: &str) -> bool {
    tag == TAG_BLOCKQUOTE || tag == TAG_BLOCKQUOTE_COLLAPSED || is_tag_pre(tag)
}

/// Encodes formatted-date entity data into a hex metadata tag (`TextUtilities::FormattedDateMetaTag`).
pub fn formatted_date_meta_tag(data: &str) -> String {
    if data.is_empty() {
        return String::new();
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(FORMATTED_DATE_META_TAG_PREFIX.len() + data.len() * 2);
    out.push_str(FORMATTED_DATE_META_TAG_PREFIX);
    for &b in data.as_bytes() {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

/// Returns `true` if `tag` is a formatted-date metadata tag (`TextUtilities::IsFormattedDateMetaTag`).
#[inline]
pub fn is_formatted_date_meta_tag(tag: &str) -> bool {
    tag.starts_with(FORMATTED_DATE_META_TAG_PREFIX)
}

/// Decodes a hex-encoded formatted-date metadata tag (`TextUtilities::FormattedDateMetaTagData`).
pub fn formatted_date_meta_tag_data(tag: &str) -> String {
    let Some(encoded) = tag.strip_prefix(FORMATTED_DATE_META_TAG_PREFIX) else {
        return String::new();
    };
    if encoded.is_empty() || !encoded.len().is_multiple_of(2) {
        return String::new();
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    let raw = encoded.as_bytes();
    for pair in raw.as_chunks::<2>().0 {
        let Some(hi) = hex_nibble(pair[0]) else {
            return String::new();
        };
        let Some(lo) = hex_nibble(pair[1]) else {
            return String::new();
        };
        bytes.push((hi << 4) | lo);
    }
    String::from_utf8(bytes).unwrap_or_default()
}

#[inline]
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Splits a combined `\`-separated tag string into individual tag parts (`TextUtilities::SplitTags`).
pub fn split_tags(tag: &str) -> Vec<&str> {
    tag.split(TAG_SEPARATOR).collect()
}

/// Joins individual tag parts with `\`, dropping separate tags (`code`/`pre`) unless
/// `code` follows only blockquote tags (`TextUtilities::JoinTag`).
pub fn join_tag(list: &[&str]) -> String {
    if list.is_empty() {
        return String::new();
    }
    let is_quote = |t: &str| t == TAG_BLOCKQUOTE || t == TAG_BLOCKQUOTE_COLLAPSED;
    let total_len: usize = list.iter().map(|s| s.len()).sum::<usize>() + list.len() - 1;
    let mut result = String::with_capacity(total_len);
    result.push_str(list[0]);
    let mut only_quotes = is_quote(list[0]);
    for &tag in &list[1..] {
        if !is_separate_tag(tag) || (only_quotes && tag == TAG_CODE) {
            result.push(TAG_SEPARATOR);
            result.push_str(tag);
        }
        only_quotes = only_quotes && is_quote(tag);
    }
    result
}

/// Removes `removed` from a combined tag string (`TextUtilities::TagWithRemoved`).
pub fn tag_with_removed(tag: &str, removed: &str) -> String {
    if tag == removed {
        return String::new();
    }
    let list: Vec<&str> = split_tags(tag)
        .into_iter()
        .filter(|&t| t != removed)
        .collect();
    join_tag(&list)
}

/// Adds `added` to a combined tag string in sorted order (`TextUtilities::TagWithAdded`).
pub fn tag_with_added(tag: &str, added: &str) -> String {
    if tag.is_empty() || tag == added {
        return added.to_string();
    }
    let mut list = split_tags(tag);
    if list.contains(&added) {
        return tag.to_string();
    }
    list.push(added);
    list.sort_unstable();
    join_tag(&list)
}

/// Merges adjacent tags whose normalized tag sets match (`TextUtilities::SimplifyTags`).
pub fn simplify_tags(mut tags: TextTags) -> TextTags {
    let mut i = 0;
    while i + 1 < tags.len() {
        let j = i + 1;
        if tags[j].offset > tags[i].offset + tags[i].length {
            i += 1;
            continue;
        }
        let mut il = split_tags(&tags[i].id);
        il.sort_unstable();
        let mut jl = split_tags(&tags[j].id);
        jl.sort_unstable();
        if join_tag(&il) == join_tag(&jl) {
            tags[i].length = tags[j].offset + tags[j].length - tags[i].offset;
            tags.remove(j);
        } else {
            i += 1;
        }
    }
    tags
}

/// Finds the first block tag inside a combined tag string (`FindBlockTag`).
pub fn find_block_tag(tag: &str) -> Option<&str> {
    split_tags(tag).into_iter().find(|t| is_block_tag(t))
}

/// Returns `true` if `tag` contains any block tag (`HasBlockTag`).
#[inline]
pub fn has_block_tag(tag: &str) -> bool {
    find_block_tag(tag).is_some()
}

/// Returns `true` if `tag` contains `TAG_SPOILER` (`HasSpoilerTag`).
#[inline]
pub fn has_spoiler_tag(tag: &str) -> bool {
    split_tags(tag).contains(&TAG_SPOILER)
}

/// Removes all block tags from a combined tag string (`WithBlockTagRemoved`).
pub fn with_block_tag_removed(tag: &str) -> String {
    let list: Vec<&str> = split_tags(tag)
        .into_iter()
        .filter(|t| !is_block_tag(t))
        .collect();
    if list.is_empty() {
        String::new()
    } else {
        join_tag(&list)
    }
}

/// Adds `added` to `tag`, dropping conflicting Instant View tags when enabled
/// (`TagWithAddedDroppingConflicts` in `input_field.cpp`).
pub fn tag_with_added_dropping_conflicts(
    tag: &str,
    added: &str,
    instant_view_editor_tags_enabled: bool,
) -> String {
    if instant_view_editor_tags_enabled && added == TAG_IV_MATH {
        return TAG_IV_MATH.to_string();
    }
    let base = if instant_view_editor_tags_enabled && added != TAG_IV_MATH {
        let without_math = tag_with_removed(tag, TAG_IV_MATH);
        if added == TAG_IV_SUBSCRIPT {
            tag_with_removed(&without_math, TAG_IV_SUPERSCRIPT)
        } else if added == TAG_IV_SUPERSCRIPT {
            tag_with_removed(&without_math, TAG_IV_SUBSCRIPT)
        } else {
            without_math
        }
    } else {
        tag.to_string()
    };
    tag_with_added(&base, added)
}

/// Drops formatting tag parts whose union covers the entire text `[0, utf16_len)`
/// except blockquotes (`TrimFullCoverageTags` in `input_field.cpp`).
pub fn trim_full_coverage_tags(parsed: &mut TextWithTags) {
    let full = utf16_len(&parsed.text) as i32;
    if full == 0 || parsed.tags.is_empty() {
        return;
    }
    let mut parts: Vec<String> = Vec::new();
    for tag in &parsed.tags {
        for part in split_tags(&tag.id) {
            if !parts.iter().any(|p| p == part) {
                parts.push(part.to_string());
            }
        }
    }
    let covers_full = |part: &str| -> bool {
        let mut spans: Vec<(i32, i32)> = parsed
            .tags
            .iter()
            .filter(|t| split_tags(&t.id).contains(&part))
            .map(|t| (t.offset, t.offset + t.length))
            .collect();
        if spans.is_empty() {
            return false;
        }
        spans.sort_unstable();
        if spans[0].0 != 0 {
            return false;
        }
        let mut covered = 0;
        for (from, till) in spans {
            if from > covered {
                return false;
            }
            covered = covered.max(till);
        }
        covered >= full
    };

    let whole_span: Vec<String> = parts
        .into_iter()
        .filter(|p| p != TAG_BLOCKQUOTE && p != TAG_BLOCKQUOTE_COLLAPSED && covers_full(p))
        .collect();
    if whole_span.is_empty() {
        return;
    }
    parsed.tags.retain_mut(|tag| {
        let mut id = tag.id.clone();
        for part in &whole_span {
            id = tag_with_removed(&id, part);
        }
        if id.is_empty() {
            false
        } else {
            tag.id = id;
            true
        }
    });
    parsed.tags = simplify_tags(std::mem::take(&mut parsed.tags));
}

/// Shifts a leading block tag right by 1 if the text starts with a newline
/// (`ShiftLeftBlockTag` in `input_field.cpp`).
pub fn shift_left_block_tag(mut text: TextWithTags) -> TextWithTags {
    while text.tags.first().is_some_and(|t| t.length <= 0) {
        text.tags.remove(0);
    }
    let first_ch = text.text.chars().next();
    if text.tags.is_empty() || first_ch.is_none_or(|c| !is_input_newline(c)) {
        return text;
    }
    if text.tags[0].offset > 0 || !has_block_tag(&text.tags[0].id) {
        return text;
    }
    let stripped = with_block_tag_removed(&text.tags[0].id);
    if text.tags[0].length == 1 {
        if stripped.is_empty() {
            text.tags.remove(0);
        } else {
            text.tags[0].id = stripped;
        }
    } else {
        text.tags[0].offset += 1;
        text.tags[0].length -= 1;
        if !stripped.is_empty() {
            text.tags.insert(0, TextTag::new(0, 1, stripped));
        }
    }
    text
}

/// Shifts a trailing block tag left by 1 if the text ends with a newline
/// (`ShiftRightBlockTag` in `input_field.cpp`).
pub fn shift_right_block_tag(mut text: TextWithTags) -> TextWithTags {
    while text.tags.last().is_some_and(|t| t.length <= 0) {
        text.tags.pop();
    }
    let last_ch = text.text.chars().next_back();
    if text.tags.is_empty() || last_ch.is_none_or(|c| !is_input_newline(c)) {
        return text;
    }
    let text_u16_len = utf16_len(&text.text) as i32;
    let last_idx = text.tags.len() - 1;
    if text.tags[last_idx].offset + text.tags[last_idx].length < text_u16_len
        || !has_block_tag(&text.tags[last_idx].id)
    {
        return text;
    }
    let stripped = with_block_tag_removed(&text.tags[last_idx].id);
    if text.tags[last_idx].length == 1 {
        if stripped.is_empty() {
            text.tags.pop();
        } else {
            text.tags[last_idx].id = stripped;
        }
    } else {
        text.tags[last_idx].length -= 1;
        if !stripped.is_empty() {
            text.tags.push(TextTag::new(text_u16_len - 1, 1, stripped));
        }
    }
    text
}

fn is_editable_link_tag(tag: &str, iv_enabled: bool) -> bool {
    is_valid_markdown_link(tag)
        || is_custom_date_link(tag)
        || (iv_enabled && is_instant_view_anchor_link(tag))
}

/// Checks if the entire non-whitespace content of `text_with_tags` is covered by `tag`
/// (or a single editable link when `tag == TAG_CHECK_LINK_META`)
/// (`CheckFullTextTag` in `input_field.cpp`).
pub fn check_full_text_tag(
    text_with_tags: &TextWithTags,
    tag: &str,
    instant_view_editor_tags_enabled: bool,
) -> String {
    let mut result_link = String::new();
    let checking_link = tag == TAG_CHECK_LINK_META;
    let units: Vec<u16> = text_with_tags.text.encode_utf16().collect();
    let mut from = 0usize;
    let mut till = units.len();

    let is_ignorable = |u: u16| -> bool {
        char::from_u32(u32::from(u)).is_some_and(|c| is_input_newline(c) || is_space(c))
    };

    for existing in &text_with_tags.tags {
        while from < till && is_ignorable(units[from]) {
            from += 1;
        }
        if existing.offset.max(0) as usize > from {
            return String::new();
        }
        let mut found = false;
        for single in split_tags(&existing.id) {
            if checking_link && is_editable_link_tag(single, instant_view_editor_tags_enabled) {
                if result_link.is_empty() {
                    result_link = single.to_string();
                    found = true;
                    break;
                } else if result_link == single {
                    found = true;
                    break;
                }
                return String::new();
            } else if !checking_link {
                let matches = if tag == TAG_PRE {
                    is_tag_pre(single)
                } else {
                    let normalized = if is_tag_pre(single) { TAG_CODE } else { single };
                    tag == normalized
                };
                if matches {
                    found = true;
                    break;
                }
            }
        }
        if !found {
            return String::new();
        }
        let end = (existing.offset + existing.length).max(0) as usize;
        from = end.clamp(from, till);
    }
    while till > from && is_ignorable(units[till - 1]) {
        till -= 1;
    }
    if from < till {
        String::new()
    } else if checking_link {
        result_link
    } else {
        tag.to_string()
    }
}

/// Returns `true` if `check_full_text_tag` succeeds (`HasFullTextTag`).
#[inline]
pub fn has_full_text_tag(text_with_tags: &TextWithTags, tag: &str) -> bool {
    !check_full_text_tag(text_with_tags, tag, false).is_empty()
}

/// Expands custom link tags in `TextWithTags` into `"text (url)"` (`ExpandCustomLinks`).
pub fn expand_custom_links(text: &TextWithTags) -> String {
    let entities = convert_text_tags_to_entities(&text.tags);
    let urls: Vec<&EntityInText> = entities
        .iter()
        .filter(|e| e.entity_type() == EntityType::CustomUrl)
        .collect();
    if urls.is_empty() {
        return text.text.clone();
    }
    let total_u16 = utf16_len(&text.text);
    let mut result = String::new();
    let mut offset = 0usize;
    for entity in urls {
        let till = (entity.offset() + entity.length()).max(0) as usize;
        if till > offset
            && let Some(chunk) = slice_utf16(&text.text, offset, till - offset)
        {
            result.push_str(chunk);
        }
        let external = external_url_from_internal_url(entity.data());
        let url = if external.is_empty() {
            entity.data()
        } else {
            external.as_str()
        };
        result.push_str(" (");
        result.push_str(url);
        result.push(')');
        offset = till;
    }
    if total_u16 > offset
        && let Some(tail) = slice_utf16(&text.text, offset, total_u16 - offset)
    {
        result.push_str(tail);
    }
    result
}

// --- ConvertTextTagsToEntities / ConvertEntitiesToTextTags ---

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct ParsedLinkTag {
    entity_type: EntityType,
    data: String,
    identity: String,
}

impl ParsedLinkTag {
    fn is_valid(&self) -> bool {
        self.entity_type != EntityType::Invalid
    }
}

fn parse_link_tag(tag: &str) -> ParsedLinkTag {
    let trimmed = tag.trim();
    if trimmed.len() >= 9 && trimmed[..9].eq_ignore_ascii_case("internal:") {
        return ParsedLinkTag::default();
    }
    if is_mention_link(tag) {
        let data = mention_entity_data(tag);
        if data.is_empty() {
            ParsedLinkTag::default()
        } else {
            ParsedLinkTag {
                entity_type: EntityType::MentionName,
                data,
                identity: String::new(),
            }
        }
    } else if is_custom_emoji_link(tag) {
        let data = custom_emoji_entity_data(tag);
        if data.is_empty() {
            ParsedLinkTag::default()
        } else {
            ParsedLinkTag {
                entity_type: EntityType::CustomEmoji,
                data,
                identity: tag.to_string(),
            }
        }
    } else if is_custom_date_link(tag) {
        ParsedLinkTag {
            entity_type: EntityType::FormattedDate,
            data: tag.to_string(),
            identity: String::new(),
        }
    } else if is_valid_markdown_link(tag) || is_instant_view_anchor_link(tag) {
        ParsedLinkTag {
            entity_type: EntityType::CustomUrl,
            data: tag.to_string(),
            identity: String::new(),
        }
    } else {
        ParsedLinkTag::default()
    }
}

fn formatted_date_data_for_tag(visible: &str, metadata: &str) -> String {
    let Some(rest) = visible.strip_prefix(CUSTOM_DATE_TAG_START) else {
        return String::new();
    };
    let date = rest.parse::<i32>().unwrap_or(0);
    if date <= 0 {
        return String::new();
    }
    let (saved_date, saved_flags) = deserialize_formatted_date_data(metadata);
    let use_flags = if saved_date > 0 {
        saved_flags
    } else {
        FormattedDateFlags::empty()
    };
    serialize_formatted_date_data(date, use_flags)
}

/// Converts editor `TextTags` into nested Telegram `EntitiesInText`
/// (`TextUtilities::ConvertTextTagsToEntities`).
pub fn convert_text_tags_to_entities(tags: &[TextTag]) -> EntitiesInText {
    let mut result: EntitiesInText = Vec::new();
    if tags.is_empty() {
        return result;
    }

    const IN_MASK_INLINE: &[EntityType] = &[
        EntityType::Bold,
        EntityType::Italic,
        EntityType::Underline,
        EntityType::StrikeOut,
        EntityType::Spoiler,
        EntityType::Code,
        EntityType::Marked,
        EntityType::Subscript,
        EntityType::Superscript,
    ];
    const IN_MASK_BLOCK: &[EntityType] = &[EntityType::Pre, EntityType::Blockquote];

    #[derive(Clone, Default)]
    struct State {
        link: ParsedLinkTag,
        formatted_date_metadata: String,
        language: String,
        mask: u32,
        collapsed: bool,
    }
    impl State {
        fn set(&mut self, t: EntityType) {
            self.mask |= 1u32 << (t as u8);
        }
        fn remove(&mut self, t: EntityType) {
            self.mask &= !(1u32 << (t as u8));
        }
        fn has(&self, t: EntityType) -> bool {
            (self.mask & (1u32 << (t as u8))) != 0
        }
    }

    let mut offset = 0i32;
    let mut state = State::default();
    let mut not_closed: Vec<usize> = Vec::new();

    let close_one = |result: &mut EntitiesInText,
                     state: &mut State,
                     not_closed: &mut Vec<usize>,
                     offset: i32| {
        if let Some(idx) = not_closed.pop() {
            let t = result[idx].entity_type();
            let start = result[idx].offset();
            let data = result[idx].data().to_string();
            result[idx] = EntityInText::with_data(t, start, offset - start, data);
            if IN_MASK_INLINE.contains(&t) || IN_MASK_BLOCK.contains(&t) {
                state.remove(t);
            } else {
                state.link = ParsedLinkTag::default();
            }
        }
    };

    let close_type = |result: &mut EntitiesInText,
                      state: &mut State,
                      not_closed: &mut Vec<usize>,
                      offset: i32,
                      t: EntityType| {
        let count = not_closed.len();
        for close_count in 0..count {
            let idx = count - close_count - 1;
            if result[not_closed[idx]].entity_type() == t {
                for _ in 0..=close_count {
                    close_one(result, state, not_closed, offset);
                }
                break;
            }
        }
    };

    let open_type = |result: &mut EntitiesInText,
                     not_closed: &mut Vec<usize>,
                     offset: i32,
                     t: EntityType,
                     data: String| {
        not_closed.push(result.len());
        result.push(EntityInText::with_data(t, offset, -1, data));
    };

    let process_state = |result: &mut EntitiesInText,
                         state: &mut State,
                         not_closed: &mut Vec<usize>,
                         offset: i32,
                         next_state: State| {
        let link_changed = (next_state.link != state.link)
            || ((next_state.formatted_date_metadata != state.formatted_date_metadata)
                && (state.link.entity_type == EntityType::FormattedDate
                    || next_state.link.entity_type == EntityType::FormattedDate));
        let close_link = link_changed && state.link.is_valid();
        let close_custom_emoji = close_link && state.link.entity_type == EntityType::CustomEmoji;
        if close_custom_emoji {
            close_type(result, state, not_closed, offset, EntityType::CustomEmoji);
        }
        for &t in IN_MASK_INLINE {
            if state.has(t) && !next_state.has(t) {
                close_type(result, state, not_closed, offset, t);
            }
        }
        let close_custom_date = close_link && state.link.entity_type == EntityType::FormattedDate;
        if close_link && !close_custom_emoji && !close_custom_date {
            let link_t = state.link.entity_type;
            close_type(result, state, not_closed, offset, link_t);
        }
        if close_custom_date {
            close_type(result, state, not_closed, offset, EntityType::FormattedDate);
        }
        for &t in IN_MASK_BLOCK {
            if state.has(t) && !next_state.has(t) {
                close_type(result, state, not_closed, offset, t);
            }
        }

        let open_link = link_changed && next_state.link.is_valid();
        let open_custom_emoji = open_link && next_state.link.entity_type == EntityType::CustomEmoji;
        let open_custom_date =
            open_link && next_state.link.entity_type == EntityType::FormattedDate;
        for &t in IN_MASK_BLOCK.iter().rev() {
            if next_state.has(t) && !state.has(t) {
                let data = if t == EntityType::Pre {
                    next_state.language.clone()
                } else if t == EntityType::Blockquote && next_state.collapsed {
                    "1".to_string()
                } else {
                    String::new()
                };
                open_type(result, not_closed, offset, t, data);
            }
        }
        if open_link && !open_custom_emoji && !open_custom_date {
            open_type(
                result,
                not_closed,
                offset,
                next_state.link.entity_type,
                next_state.link.data.clone(),
            );
        }
        for &t in IN_MASK_INLINE.iter().rev() {
            if next_state.has(t) && !state.has(t) {
                open_type(result, not_closed, offset, t, String::new());
            }
        }
        if open_custom_emoji && !next_state.link.data.is_empty() {
            open_type(
                result,
                not_closed,
                offset,
                EntityType::CustomEmoji,
                next_state.link.data.clone(),
            );
        }
        if open_custom_date {
            let data = formatted_date_data_for_tag(
                &next_state.link.data,
                &next_state.formatted_date_metadata,
            );
            if !data.is_empty() {
                open_type(result, not_closed, offset, EntityType::FormattedDate, data);
            }
        }
        *state = next_state;
    };

    let state_for_tag = |tag: &str| -> State {
        let mut st = State::default();
        for single in split_tags(tag) {
            if single == TAG_BOLD {
                st.set(EntityType::Bold);
            } else if single == TAG_ITALIC {
                st.set(EntityType::Italic);
            } else if single == TAG_UNDERLINE {
                st.set(EntityType::Underline);
            } else if single == TAG_STRIKE_OUT {
                st.set(EntityType::StrikeOut);
            } else if single == TAG_CODE {
                st.set(EntityType::Code);
            } else if single == TAG_PRE {
                st.set(EntityType::Pre);
            } else if let Some(lang) = single.strip_prefix(TAG_PRE) {
                if !lang.is_empty() {
                    st.set(EntityType::Pre);
                    st.language = lang.to_string();
                }
            } else if single == TAG_BLOCKQUOTE {
                st.set(EntityType::Blockquote);
                st.collapsed = false;
            } else if single == TAG_BLOCKQUOTE_COLLAPSED {
                st.set(EntityType::Blockquote);
                st.collapsed = true;
            } else if single == TAG_SPOILER {
                st.set(EntityType::Spoiler);
            } else if is_rich_formatting_tag(single) {
                if single == TAG_IV_MARKED {
                    st.set(EntityType::Marked);
                } else if single == TAG_IV_SUBSCRIPT {
                    st.set(EntityType::Subscript);
                } else if single == TAG_IV_SUPERSCRIPT {
                    st.set(EntityType::Superscript);
                }
            } else if is_formatted_date_meta_tag(single) {
                st.formatted_date_metadata = formatted_date_meta_tag_data(single);
            } else {
                let link = parse_link_tag(single);
                if link.is_valid() {
                    st.link = link;
                }
            }
        }
        st
    };

    for tag in tags {
        if tag.offset > offset {
            process_state(
                &mut result,
                &mut state,
                &mut not_closed,
                offset,
                State::default(),
            );
        }
        offset = tag.offset;
        let next = state_for_tag(&tag.id);
        process_state(&mut result, &mut state, &mut not_closed, offset, next);
        offset += tag.length;
    }
    process_state(
        &mut result,
        &mut state,
        &mut not_closed,
        offset,
        State::default(),
    );

    result.retain(|e| e.length() > 0);
    result
}

/// Converts Telegram `EntitiesInText` into non-overlapping editor `TextTags`
/// (`TextUtilities::ConvertEntitiesToTextTags`).
pub fn convert_entities_to_text_tags(entities: &[EntityInText]) -> TextTags {
    let mut result: TextTags = Vec::new();
    if entities.is_empty() {
        return result;
    }

    static RE_MENTION_DATA: OnceLock<Regex> = OnceLock::new();
    static RE_DIGITS: OnceLock<Regex> = OnceLock::new();
    static RE_LANG: OnceLock<Regex> = OnceLock::new();

    let mut offset = 0i32;
    let mut current = String::new();
    let mut to_remove: Vec<(i32, String)> = Vec::new();

    let update_current = |result: &mut TextTags,
                          offset: &mut i32,
                          current: &mut String,
                          next_offset: i32,
                          next: String| {
        if next == *current {
            return;
        }
        if next_offset > *offset {
            if !current.is_empty() {
                result.push(TextTag::new(
                    *offset,
                    next_offset - *offset,
                    current.clone(),
                ));
            }
            *offset = next_offset;
        }
        *current = next;
    };

    let remove_till = |result: &mut TextTags,
                       offset: &mut i32,
                       current: &mut String,
                       to_remove: &mut Vec<(i32, String)>,
                       next_offset: i32| {
        while let Some(&(end_off, _)) = to_remove.first() {
            if end_off > next_offset {
                break;
            }
            let (end_off, rem_tag) = to_remove.remove(0);
            let next = tag_with_removed(current, &rem_tag);
            update_current(result, offset, current, end_off, next);
        }
    };

    let push_tag = |result: &mut TextTags,
                    offset: &mut i32,
                    current: &mut String,
                    to_remove: &mut Vec<(i32, String)>,
                    entity: &EntityInText,
                    tag: String| {
        remove_till(result, offset, current, to_remove, entity.offset());
        let next = tag_with_added(current, &tag);
        update_current(result, offset, current, entity.offset(), next);
        to_remove.push((*offset + entity.length(), tag));
        to_remove.sort();
    };

    for entity in entities {
        match entity.entity_type() {
            EntityType::MentionName => {
                let re = RE_MENTION_DATA
                    .get_or_init(|| Regex::new(r"^\d+\.\d+:\d+$").expect("valid regex"));
                if re.is_match(entity.data()) {
                    push_tag(
                        &mut result,
                        &mut offset,
                        &mut current,
                        &mut to_remove,
                        entity,
                        format!("{MENTION_TAG_START}{}", entity.data()),
                    );
                }
            }
            EntityType::CustomUrl => {
                let external = external_url_from_internal_url(entity.data());
                let url = if external.is_empty() {
                    entity.data()
                } else {
                    external.as_str()
                };
                if (is_valid_markdown_link(url) && !is_mention_link(url))
                    || is_instant_view_anchor_link(url)
                {
                    push_tag(
                        &mut result,
                        &mut offset,
                        &mut current,
                        &mut to_remove,
                        entity,
                        url.to_string(),
                    );
                }
            }
            EntityType::CustomEmoji => {
                let re = RE_DIGITS.get_or_init(|| Regex::new(r"^\d+$").expect("valid regex"));
                if re.is_match(entity.data()) {
                    push_tag(
                        &mut result,
                        &mut offset,
                        &mut current,
                        &mut to_remove,
                        entity,
                        custom_emoji_link(entity.data()),
                    );
                }
            }
            EntityType::Bold => push_tag(
                &mut result,
                &mut offset,
                &mut current,
                &mut to_remove,
                entity,
                TAG_BOLD.to_string(),
            ),
            EntityType::Italic => push_tag(
                &mut result,
                &mut offset,
                &mut current,
                &mut to_remove,
                entity,
                TAG_ITALIC.to_string(),
            ),
            EntityType::Underline => push_tag(
                &mut result,
                &mut offset,
                &mut current,
                &mut to_remove,
                entity,
                TAG_UNDERLINE.to_string(),
            ),
            EntityType::StrikeOut => push_tag(
                &mut result,
                &mut offset,
                &mut current,
                &mut to_remove,
                entity,
                TAG_STRIKE_OUT.to_string(),
            ),
            EntityType::Code => push_tag(
                &mut result,
                &mut offset,
                &mut current,
                &mut to_remove,
                entity,
                TAG_CODE.to_string(),
            ),
            EntityType::Pre => {
                let lang_re =
                    RE_LANG.get_or_init(|| Regex::new(r"^[a-zA-Z0-9\-\+]+$").expect("valid regex"));
                let tag = if !entity.data().is_empty() && lang_re.is_match(entity.data()) {
                    format!("{TAG_PRE}{}", entity.data())
                } else {
                    TAG_PRE.to_string()
                };
                push_tag(
                    &mut result,
                    &mut offset,
                    &mut current,
                    &mut to_remove,
                    entity,
                    tag,
                );
            }
            EntityType::Blockquote => {
                let tag = if entity.data().is_empty() {
                    TAG_BLOCKQUOTE
                } else {
                    TAG_BLOCKQUOTE_COLLAPSED
                };
                push_tag(
                    &mut result,
                    &mut offset,
                    &mut current,
                    &mut to_remove,
                    entity,
                    tag.to_string(),
                );
            }
            EntityType::Spoiler => push_tag(
                &mut result,
                &mut offset,
                &mut current,
                &mut to_remove,
                entity,
                TAG_SPOILER.to_string(),
            ),
            EntityType::Subscript => push_tag(
                &mut result,
                &mut offset,
                &mut current,
                &mut to_remove,
                entity,
                TAG_IV_SUBSCRIPT.to_string(),
            ),
            EntityType::Superscript => push_tag(
                &mut result,
                &mut offset,
                &mut current,
                &mut to_remove,
                entity,
                TAG_IV_SUPERSCRIPT.to_string(),
            ),
            EntityType::Marked => push_tag(
                &mut result,
                &mut offset,
                &mut current,
                &mut to_remove,
                entity,
                TAG_IV_MARKED.to_string(),
            ),
            EntityType::FormattedDate => {
                let (date, date_flags) = deserialize_formatted_date_data(entity.data());
                if date > 0 {
                    push_tag(
                        &mut result,
                        &mut offset,
                        &mut current,
                        &mut to_remove,
                        entity,
                        format!("{CUSTOM_DATE_TAG_START}{date}"),
                    );
                    if !date_flags.is_empty() {
                        let metadata = formatted_date_meta_tag(entity.data());
                        if !metadata.is_empty() {
                            push_tag(
                                &mut result,
                                &mut offset,
                                &mut current,
                                &mut to_remove,
                                entity,
                                metadata,
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if let Some(&(last_end, _)) = to_remove.last() {
        remove_till(
            &mut result,
            &mut offset,
            &mut current,
            &mut to_remove,
            last_end,
        );
    }
    result
}

// --- Binary serialization matching QDataStream::Qt_5_1 (big-endian) ---

/// Returns the serialized byte size including the outer `QByteArray` 4-byte length prefix
/// (`TextUtilities::SerializeTagsSize`).
pub fn serialize_tags_size(tags: &[TextTag]) -> usize {
    let mut result = 4usize; // QByteArray length prefix
    if tags.is_empty() {
        return result;
    }
    result += 4; // tag count
    for tag in tags {
        result += 8 // offset (i32) + length (i32)
            + 4 // QString byte length (u32)
            + utf16_len(&tag.id) * 2;
    }
    result
}

/// Serializes `tags` in `QDataStream::Qt_5_1` big-endian binary format (`TextUtilities::SerializeTags`).
pub fn serialize_tags(tags: &[TextTag]) -> Vec<u8> {
    if tags.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(serialize_tags_size(tags).saturating_sub(4));
    out.extend_from_slice(&(tags.len() as i32).to_be_bytes());
    for tag in tags {
        out.extend_from_slice(&tag.offset.to_be_bytes());
        out.extend_from_slice(&tag.length.to_be_bytes());
        let u16s: Vec<u16> = tag.id.encode_utf16().collect();
        let byte_len = (u16s.len() * 2) as u32;
        out.extend_from_slice(&byte_len.to_be_bytes());
        for unit in u16s {
            out.extend_from_slice(&unit.to_be_bytes());
        }
    }
    out
}

/// Deserializes `tags` from `QDataStream::Qt_5_1` big-endian binary format, validating
/// every tag against `text_utf16_len` (`TextUtilities::DeserializeTags`).
pub fn deserialize_tags(data: &[u8], text_utf16_len: usize) -> TextTags {
    let mut result = Vec::new();
    if data.len() < 4 {
        return result;
    }
    let mut pos = 0usize;
    let read_i32 = |pos: &mut usize| -> Option<i32> {
        let bytes: [u8; 4] = data.get(*pos..*pos + 4)?.try_into().ok()?;
        *pos += 4;
        Some(i32::from_be_bytes(bytes))
    };
    let read_u32 = |pos: &mut usize| -> Option<u32> {
        let bytes: [u8; 4] = data.get(*pos..*pos + 4)?.try_into().ok()?;
        *pos += 4;
        Some(u32::from_be_bytes(bytes))
    };

    let Some(tag_count) = read_i32(&mut pos) else {
        return result;
    };
    let Ok(text_len_i32) = i32::try_from(text_utf16_len) else {
        return result;
    };
    if tag_count <= 0 || tag_count > text_len_i32 {
        return result;
    }

    for _ in 0..tag_count {
        let (Some(offset), Some(length), Some(str_bytes)) =
            (read_i32(&mut pos), read_i32(&mut pos), read_u32(&mut pos))
        else {
            return result;
        };
        let id = if str_bytes == 0xFFFF_FFFF {
            String::new()
        } else {
            let byte_len = str_bytes as usize;
            if !byte_len.is_multiple_of(2) || pos + byte_len > data.len() {
                return result;
            }
            let units: Vec<u16> = data[pos..pos + byte_len]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            pos += byte_len;
            String::from_utf16_lossy(&units)
        };
        if offset < 0 || length <= 0 || offset.saturating_add(length) > text_len_i32 {
            return result;
        }
        result.push(TextTag { offset, length, id });
    }
    result
}
