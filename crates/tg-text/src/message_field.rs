//! Pure message compose field logic: mention tags, support hashtags, inline bot queries,
//! autocomplete queries, message link parsing, selection mapping, and instant replaces.
//!
//! Port of pure parts of `Telegram/SourceFiles/chat_helpers/message_field.cpp/.h` and
//! `InstantReplaces` from `Telegram/lib_ui/ui/widgets/fields/input_field.cpp/.h`.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;

use crate::char_class::{OBJECT_REPLACEMENT_CHAR, is_replaced_by_space, is_trimmed};
use crate::emoji::{all_suggestions, by_index, find_emoji_utf16, replacement_pairs};
use crate::entity::{TextParseFlags, TextSelection, TextWithEntities};
use crate::markdown::MarkdownTag;
use crate::parse::{prepare_for_sending, regex_mail_name_at_end, scan_url_tail};
use crate::tags::{
    CUSTOM_EMOJI_TAG_START, MENTION_TAG_START, TAG_BLOCKQUOTE, TAG_BLOCKQUOTE_COLLAPSED, TAG_BOLD,
    TAG_ITALIC, TAG_SPOILER, TAG_STRIKE_OUT, TAG_UNDERLINE, TextWithTags,
    convert_text_tags_to_entities, is_mention_link, is_valid_markdown_link, split_tags,
};
use crate::url::{is_valid_protocol, is_valid_top_domain, match_domain};
use crate::utf16::{OffsetMapper, utf16_len};

/// Maximum code-block language identifier length (`kCodeLanguageLimit` in `message_field.cpp`).
pub const CODE_LANGUAGE_LIMIT: usize = 32;

/// Builds a `"mention://<userId>.<accessHash>:<selfId>"` tag (`PrepareMentionTag`).
pub fn prepare_mention_tag(user_id: u64, access_hash: u64, self_id: u64) -> String {
    format!("{MENTION_TAG_START}{user_id}.{access_hash}:{self_id}")
}

/// Validates a fenced code block language name (`EditCodeLanguageBox` in `message_field.cpp`).
pub fn is_valid_code_language(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.len() > CODE_LANGUAGE_LIMIT {
        return false;
    }
    trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'-')
}

/// Validates a Telegram factcheck URL (`IsGoodFactcheckUrl` in `message_field.cpp`).
#[inline]
pub fn is_good_factcheck_url(url: &str) -> bool {
    url.starts_with("t.me/") || url.starts_with("https://t.me/")
}

/// Returns `true` if `text` contains at least one character that survives trimming and server
/// cleaning (`HasSendText` in `message_field.cpp`).
pub fn has_send_text(text: &str) -> bool {
    text.chars()
        .any(|ch| !is_trimmed(ch) && !is_replaced_by_space(ch))
}

/// Strips a trailing `#tsf...` Telegram Support Force hashtag from a message being edited
/// (`StripSupportHashtag` in `message_field.cpp`).
pub fn strip_support_hashtag(mut text: TextWithEntities) -> TextWithEntities {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)\n?#tsf[a-z0-9_-]*[\s#a-z0-9_-]*$").expect("valid tsf regex")
    });
    let Some(m) = re.find(&text.text) else {
        return text;
    };
    text.text.truncate(m.start());
    let new_u16_len = utf16_len(&text.text) as i32;
    if new_u16_len == 0 {
        return TextWithEntities::new();
    }
    text.entities.retain_mut(|entity| {
        if entity.offset() >= new_u16_len {
            false
        } else {
            if entity.offset() + entity.length() > new_u16_len {
                entity.update_text_end(new_u16_len);
            }
            true
        }
    });
    text
}

/// Compares original and updated edit texts after converting tags to entities and applying
/// `prepare_for_sending(..., empty)` (`EditTextChanged` in `message_field.cpp`).
///
/// Compares entities rather than tags because custom emoji tags carry unique counters.
pub fn edit_text_changed(original: &TextWithTags, updated: &TextWithTags) -> bool {
    let mut orig_ent = TextWithEntities {
        text: original.text.clone(),
        entities: convert_text_tags_to_entities(&original.tags),
    };
    let mut upd_ent = TextWithEntities {
        text: updated.text.clone(),
        entities: convert_text_tags_to_entities(&updated.tags),
    };
    prepare_for_sending(&mut orig_ent, TextParseFlags::empty());
    prepare_for_sending(&mut upd_ent, TextParseFlags::empty());
    orig_ent != upd_ent
}

/// Prepends a leading space if the first non-replacement character is `'@'`, preventing accidental
/// inline bot invocation (`PreventInlineBotQuery` in `message_field.cpp`).
pub fn prevent_inline_bot_query(text: TextWithEntities) -> TextWithEntities {
    let first = text.text.chars().find(|&ch| ch != OBJECT_REPLACEMENT_CHAR);
    if first == Some('@') {
        let mut out = TextWithEntities::simple(" ");
        out.append(text);
        out
    } else {
        text
    }
}

/// Parsed `@bot query` inline bot invocation (`InlineBotQuery` pure fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedInlineBotQuery {
    /// Bot username without leading `@`.
    pub username: String,
    /// Query string following the whitespace after `@username` (empty if no trailing space).
    pub query: String,
}

/// Extracts an inline bot `@username` and query from a compose field's `TextWithTags`
/// (pure parsing part of `ParseInlineBotQuery` in `message_field.cpp`).
pub fn parse_inline_bot_query(full: &TextWithTags) -> Option<ParsedInlineBotQuery> {
    let text = &full.text;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    if chars.len() <= 2 || chars[0].1 != '@' || !chars[1].1.is_alphabetic() {
        return None;
    }

    let mut username_char_len = 1usize;
    for &(_, ch) in &chars[2..] {
        if ch.is_alphanumeric() || ch == '_' {
            username_char_len += 1;
        } else if ch.is_whitespace() {
            break;
        } else {
            return None;
        }
    }
    if username_char_len < 3 {
        return None;
    }

    let end_char_idx = 1 + username_char_len;
    let equals_text = end_char_idx == chars.len();
    let valid = if equals_text {
        text.ends_with("bot")
    } else {
        chars[end_char_idx].1.is_whitespace()
    };
    if !valid {
        return None;
    }

    let username_end_byte = if equals_text {
        text.len()
    } else {
        chars[end_char_idx].0
    };
    let username_end_u16 = utf16_len(&text[..username_end_byte]) as i32;
    if full
        .tags
        .first()
        .is_some_and(|t| t.offset < username_end_u16)
    {
        return None;
    }

    let username = text[1..username_end_byte].to_string();
    let query = if equals_text {
        String::new()
    } else {
        let query_start = chars[end_char_idx].0 + chars[end_char_idx].1.len_utf8();
        text[query_start..].to_string()
    };
    Some(ParsedInlineBotQuery { username, query })
}

/// Feature flags controlling mention/hashtag/command autocomplete detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutocompleteFeatures {
    /// Enable `@mention` autocomplete.
    pub autocomplete_mentions: bool,
    /// Enable `#hashtag` autocomplete.
    pub autocomplete_hashtags: bool,
    /// Enable `/command` autocomplete.
    pub autocomplete_commands: bool,
}

impl Default for AutocompleteFeatures {
    fn default() -> Self {
        Self {
            autocomplete_mentions: true,
            autocomplete_hashtags: true,
            autocomplete_commands: true,
        }
    }
}

/// Active autocomplete query at the cursor (`AutocompleteQuery` in `message_field.h`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AutocompleteQuery {
    /// Query including leading `@`, `#`, or `/` (empty if none active).
    pub query: String,
    /// `true` if the trigger symbol is at index 0 of the text.
    pub from_start: bool,
}

/// Parses an active `@mention`, `#hashtag`, or `/command` autocomplete query ending at
/// `cursor_utf16` (`ParseMentionHashtagBotCommandQuery` in `message_field.cpp`).
pub fn parse_mention_hashtag_bot_command_query(
    text: &str,
    cursor_utf16: usize,
    features: AutocompleteFeatures,
) -> AutocompleteQuery {
    let mut result = AutocompleteQuery::default();
    let units: Vec<u16> = text.encode_utf16().collect();
    if cursor_utf16 == 0 || cursor_utf16 > units.len() {
        return result;
    }
    let char_at = |idx: usize| -> char { char::from_u32(u32::from(units[idx])).unwrap_or('\0') };
    let is_word_ch = |ch: char| ch.is_alphanumeric() || ch == '_';

    let mut mention_in_command = false;
    let mut i = cursor_utf16;
    while i > 0 {
        let ch = char_at(i - 1);
        if ch == '@' {
            if !features.autocomplete_mentions {
                return AutocompleteQuery::default();
            }
            let after_ok = cursor_utf16 - i < 1 || char_at(i).is_alphanumeric();
            let prev_is_word = i >= 2 && is_word_ch(char_at(i - 2));
            if after_ok && !prev_is_word {
                result.from_start = i == 1;
                result.query = String::from_utf16_lossy(&units[i - 1..cursor_utf16]);
            } else if after_ok && i > 2 && prev_is_word && !mention_in_command {
                mention_in_command = true;
                i -= 1;
                continue;
            }
            return result;
        } else if ch == '#' {
            if !features.autocomplete_hashtags {
                return AutocompleteQuery::default();
            }
            let prev_is_word = i >= 2 && is_word_ch(char_at(i - 2));
            if !prev_is_word {
                result.from_start = i == 1;
                result.query = String::from_utf16_lossy(&units[i - 1..cursor_utf16]);
            }
            return result;
        } else if ch == '/' {
            if !features.autocomplete_commands {
                return AutocompleteQuery::default();
            }
            if i == 1 {
                result.from_start = true;
                result.query = String::from_utf16_lossy(&units[0..cursor_utf16]);
            }
            return result;
        }

        if cursor_utf16 - i > 127 || (!mention_in_command && (cursor_utf16 - i > 63)) {
            break;
        }
        if !is_word_ch(ch) {
            break;
        }
        i -= 1;
    }

    result
}

/// Detected link range in a compose message (`MessageLinkRange` in `message_field.h`).
///
/// `start` and `length` are in UTF-16 code units.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct MessageLinkRange {
    /// Start offset in UTF-16 code units.
    pub start: usize,
    /// Length in UTF-16 code units.
    pub length: usize,
    /// Custom target URL (empty for plain URLs taken directly from text).
    pub custom: String,
}

/// Extracts all previewable link ranges from `text_with_tags` and `markdown_tags`
/// (`MessageLinksParser::parse` in `message_field.cpp`).
pub fn parse_message_links(
    text_with_tags: &TextWithTags,
    markdown_tags: &[MarkdownTag],
) -> Vec<MessageLinkRange> {
    let text = &text_with_tags.text;
    let tags = &text_with_tags.tags;
    if text.is_empty() {
        return Vec::new();
    }

    let tag_can_intersect = |tag: &str| {
        matches!(
            tag,
            TAG_BOLD
                | TAG_ITALIC
                | TAG_UNDERLINE
                | TAG_STRIKE_OUT
                | TAG_SPOILER
                | TAG_BLOCKQUOTE
                | TAG_BLOCKQUOTE_COLLAPSED
        )
    };

    let mut ranges: Vec<MessageLinkRange> = Vec::new();
    let mut tag_idx = 0usize;
    let process_tag = |tag_idx: &mut usize, ranges: &mut Vec<MessageLinkRange>| {
        let t = &tags[*tag_idx];
        if is_valid_markdown_link(&t.id) && !is_mention_link(&t.id) {
            ranges.push(MessageLinkRange {
                start: t.offset.max(0) as usize,
                length: t.length.max(0) as usize,
                custom: t.id.clone(),
            });
        }
        *tag_idx += 1;
    };
    let process_tags_before =
        |offset_u16: usize, tag_idx: &mut usize, ranges: &mut Vec<MessageLinkRange>| {
            while *tag_idx < tags.len()
                && ((tags[*tag_idx].offset + tags[*tag_idx].length).max(0) as usize <= offset_u16
                    || tag_can_intersect(&tags[*tag_idx].id))
            {
                process_tag(tag_idx, ranges);
            }
        };
    let has_tags_intersection =
        |till_u16: usize, tag_idx: &mut usize, ranges: &mut Vec<MessageLinkRange>| -> bool {
            if *tag_idx >= tags.len() || (tags[*tag_idx].offset.max(0) as usize) >= till_u16 {
                return false;
            }
            while *tag_idx < tags.len() && (tags[*tag_idx].offset.max(0) as usize) < till_u16 {
                process_tag(tag_idx, ranges);
            }
            true
        };

    let mut md_idx = 0usize;
    let mut md_allows = |from_u16: i32, len_u16: i32| -> bool {
        while md_idx < markdown_tags.len()
            && (markdown_tags[md_idx].adjusted_start + markdown_tags[md_idx].adjusted_length
                <= from_u16
                || !markdown_tags[md_idx].closed
                || tag_can_intersect(&markdown_tags[md_idx].tag))
        {
            md_idx += 1;
        }
        if md_idx >= markdown_tags.len()
            || markdown_tags[md_idx].adjusted_start >= from_u16 + len_u16
        {
            return true;
        }
        markdown_tags[md_idx].adjusted_start > from_u16
            || markdown_tags[md_idx].adjusted_start + markdown_tags[md_idx].adjusted_length
                < from_u16 + len_u16
    };

    let mapper = OffsetMapper::new(text);
    let len = text.len();
    let mut offset = 0usize;
    let mut match_offset = 0usize;

    while offset < len {
        let Some(dm) = match_domain(text, match_offset) else {
            break;
        };
        let protocol = dm.protocol.to_lowercase();
        let top_domain = dm.top_domain.to_lowercase();
        let is_protocol_valid = protocol.is_empty() || is_valid_protocol(&protocol);
        let is_top_domain_valid = !protocol.is_empty() || is_valid_top_domain(&top_domain);

        if protocol.is_empty() && dm.start > offset + 1 && text.as_bytes()[dm.start - 1] == b'@' {
            let for_mail_name = &text[offset..dm.start - 1];
            if regex_mail_name_at_end().is_match(for_mail_name) {
                offset = dm.end;
                match_offset = dm.end;
                continue;
            }
        }
        if !is_protocol_valid || !is_top_domain_valid {
            offset = dm.end;
            match_offset = dm.end;
            continue;
        }

        let tail_end = scan_url_tail(text, dm.end);
        if tail_end > dm.end {
            let next_byte = text.as_bytes()[dm.end];
            if next_byte != b'/' && next_byte != b'?' {
                match_offset = dm.end;
                continue;
            }
        }

        if let Some((start_u16, len_u16)) = mapper.utf8_range_to_utf16(dm.start..tail_end) {
            process_tags_before(start_u16, &mut tag_idx, &mut ranges);
            if !has_tags_intersection(start_u16 + len_u16, &mut tag_idx, &mut ranges)
                && md_allows(start_u16 as i32, len_u16 as i32)
            {
                ranges.push(MessageLinkRange {
                    start: start_u16,
                    length: len_u16,
                    custom: String::new(),
                });
            }
        }

        offset = tail_end;
        match_offset = tail_end;
    }

    process_tags_before(usize::MAX, &mut tag_idx, &mut ranges);
    ranges
}

/// Maps a UTF-16 `TextSelection` from full text coordinates into collapsed input-field
/// coordinates where each emoji or custom emoji occupies 1 unit (`EditFieldSelection` in `message_field.cpp`).
pub fn edit_field_selection(text: &TextWithTags, selection: TextSelection) -> TextSelection {
    let units: Vec<u16> = text.text.encode_utf16().collect();
    let size = units.len();

    let is_custom_emoji = |id: &str| {
        split_tags(id)
            .into_iter()
            .any(|t| t.starts_with(CUSTOM_EMOJI_TAG_START))
    };

    let field_offset = |requested: usize, is_end: bool| -> usize {
        let offset = requested.min(size);
        let mut result = 0usize;
        let mut p = 0usize;
        while p < offset {
            let mut length = 1usize;
            let mut collapsed = false;
            for tag in &text.tags {
                if tag.offset >= 0 && tag.offset as usize == p && is_custom_emoji(&tag.id) {
                    length = tag.length.max(1) as usize;
                    collapsed = true;
                    break;
                }
            }
            if !collapsed && let Some((_, emoji_len)) = find_emoji_utf16(&units, p) {
                length = emoji_len;
                collapsed = true;
            }
            if p + length > offset {
                return result
                    + if collapsed {
                        usize::from(is_end)
                    } else {
                        offset - p
                    };
            }
            result += if collapsed { 1 } else { length };
            p += length;
        }
        result + (offset - p)
    };

    TextSelection::new(
        field_offset(usize::from(selection.from), false).min(0xFFFF) as u16,
        field_offset(usize::from(selection.to), true).min(0xFFFF) as u16,
    )
}

// --- InstantReplaces reverse-trie (input_field.cpp) ---

#[derive(Debug, Default)]
struct ReplaceNode {
    text: String,
    tail: BTreeMap<char, ReplaceNode>,
}

/// Reverse-suffix trie for instant typing replacements like `--` -> `—`, `:-)` -> `😊`,
/// `:smile:` -> `😄` (`Ui::InstantReplaces` in `input_field.cpp`).
#[derive(Debug, Default)]
pub struct InstantReplaces {
    max_length: usize,
    reverse_map: ReplaceNode,
}

impl InstantReplaces {
    /// Creates an empty `InstantReplaces` trie.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a replacement mapping `what` -> `with`.
    pub fn add(&mut self, what: &str, with: &str) {
        let mut node = &mut self.reverse_map;
        let mut char_count = 0usize;
        for ch in what.chars().rev() {
            node = node.tail.entry(ch).or_default();
            char_count += 1;
        }
        node.text = with.to_string();
        self.max_length = self.max_length.max(char_count);
    }

    /// Maximum trigger length in Unicode scalar values.
    #[inline]
    pub fn max_length(&self) -> usize {
        self.max_length
    }

    /// Checks whether the end of `text` matches an instant replacement trigger.
    /// Returns `(matched_trigger_byte_len, replacement_str)` if matched.
    pub fn find_suffix_match<'a>(&'a self, text: &str) -> Option<(usize, &'a str)> {
        let mut node = &self.reverse_map;
        let mut matched_bytes = 0usize;
        for ch in text.chars().rev() {
            let next = node.tail.get(&ch)?;
            matched_bytes += ch.len_utf8();
            node = next;
            if !node.text.is_empty() {
                return Some((matched_bytes, node.text.as_str()));
            }
        }
        None
    }

    /// Text-only instant replaces (`--`, `<<`, `>>`, `:shrug:`) (`InstantReplaces::TextOnly`).
    pub fn text_only() -> &'static Self {
        static TEXT_ONLY: OnceLock<InstantReplaces> = OnceLock::new();
        TEXT_ONLY.get_or_init(|| {
            let mut r = InstantReplaces::new();
            r.add("--", "\u{2014}");
            r.add("<<", "\u{00AB}");
            r.add(">>", "\u{00BB}");
            r.add(":shrug:", "\u{00AF}\\_(\u{30C4})_/\u{00AF}");
            r
        })
    }

    /// Full default instant replaces including text replaces, emoji shortcodes, and ASCII emoticons
    /// (`InstantReplaces::Default`).
    pub fn default_replaces() -> &'static Self {
        static DEFAULT: OnceLock<InstantReplaces> = OnceLock::new();
        DEFAULT.get_or_init(|| {
            let mut r = InstantReplaces::new();
            r.add("--", "\u{2014}");
            r.add("<<", "\u{00AB}");
            r.add(">>", "\u{00BB}");
            r.add(":shrug:", "\u{00AF}\\_(\u{30C4})_/\u{00AF}");
            r.add(":o ", "\u{1F628}");
            r.add("xD ", "\u{1F606}");
            for s in all_suggestions() {
                r.add(s.replacement, s.emoji);
            }
            for &(what, idx) in replacement_pairs() {
                if let Some(emoji) = by_index(idx as usize) {
                    r.add(what, &emoji.text());
                }
            }
            r
        })
    }
}
