//! Entity parsing, message cleaning, trimming, splitting, accent folding, and search tokenization.
//!
//! Port of `Telegram/lib_ui/ui/text/text_entity.cpp` (`ParseEntities`, `PrepareForSending`,
//! `ApplyServerCleaning`, `Trim`, `CutPart`, `EscapeForRichParsing`, `SingleLine`,
//! `RemoveAccents`, `RemoveEmoji`, `NameSortKey`, `PrepareSearchWords`).

use std::sync::OnceLock;

use regex::Regex;

use crate::char_class::{
    is_almost_link_end, is_diacritic, is_link_end, is_newline, is_replaced_by_space,
    is_sentence_end, is_sentence_part_end, is_space, is_trimmed, is_word_separator,
};
use crate::emoji::{find_emoji, find_emoji_utf16};
use crate::entity::{EntitiesInText, EntityInText, EntityType, TextParseFlags, TextWithEntities};
use crate::markdown::parse_markdown_in_place;
use crate::url::{is_valid_protocol, is_valid_top_domain, match_domain, match_domain_explicit};
use crate::utf16::{OffsetMapper, utf16_len};

/// Option for string-only `prepare_for_sending_str` (`TextUtilities::PrepareTextOption`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrepareTextOption {
    /// Do not parse links/mentions/hashtags/commands.
    #[default]
    IgnoreLinks,
    /// Parse links, mentions, hashtags, and bot commands.
    CheckLinks,
}

const QUOTES: &str = "\u{00AB}\u{00BB}\u{201C}\u{201D}\u{2018}\u{2019}\u{2026}";

fn expression_separators(additional: &str) -> String {
    format!(r#"\s.,:;<>|'"\[\]{{}}~!?%^()\-+=\x10{QUOTES}{additional}"#)
}

/// Regex matching email local-part before `@` (`TextUtilities::RegExpMailNameAtEnd`).
pub fn regex_mail_name_at_end() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[a-zA-Z\-_\.0-9]{1,256}$").expect("valid email regex"))
}

/// Regex matching hashtags (`TextUtilities::RegExpHashtag`).
pub fn regex_hashtag(allow_with_mention: bool) -> &'static Regex {
    if allow_with_mention {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| {
            let sep = expression_separators(r"`\*/");
            Regex::new(&format!(
                r"(^|[{sep}])#[\w]{{2,64}}(@[A-Za-z_0-9]{{1,32}})?([\W]|$)"
            ))
            .expect("valid hashtag+mention regex")
        })
    } else {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| {
            let sep = expression_separators(r"`\*/");
            Regex::new(&format!(r"(^|[{sep}])#[\w]{{2,64}}([\W]|$)")).expect("valid hashtag regex")
        })
    }
}

/// Regex matching digit-only hashtags to exclude (`TextUtilities::RegExpHashtagExclude`).
pub fn regex_hashtag_exclude() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^#?\d+$").expect("valid hashtag exclude regex"))
}

/// Regex matching `@username` mentions (`TextUtilities::RegExpMention`).
pub fn regex_mention() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let sep = expression_separators(r"`\*/");
        Regex::new(&format!(r"(^|[{sep}])@[A-Za-z_0-9]{{1,32}}([\W]|$)"))
            .expect("valid mention regex")
    })
}

/// Regex matching `/command` and `/command@bot` (`TextUtilities::RegExpBotCommand`).
pub fn regex_bot_command() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let sep = expression_separators(r"`\*");
        Regex::new(&format!(
            r"(^|[{sep}])/[A-Za-z_0-9]{{1,64}}(@[A-Za-z_0-9]{{5,32}})?([\W]|$)"
        ))
        .expect("valid bot command regex")
    })
}

/// Regex matching `$CASHTAG` and `$CASHTAG@username` (Telegram cashtag format).
pub fn regex_cashtag() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let sep = expression_separators(r"`\*/");
        Regex::new(&format!(
            r"(^|[{sep}])\$[A-Z]{{1,8}}(@[A-Za-z_0-9]{{1,32}})?([\W]|$)"
        ))
        .expect("valid cashtag regex")
    })
}

/// Regex matching non-digits (`TextUtilities::RegExpDigitsExclude`).
pub fn regex_digits_exclude() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[^\d]").expect("valid digits exclude regex"))
}

/// Regex splitting search words (`RegExpWordSplit`).
pub fn regex_word_split() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"[@\s\-+()\[\]{}<>,.:!_;\"'\x00\u{AB}\u{BB}\u{2018}\u{2019}\u{201A}\u{201C}\u{201D}\u{201E}]"#)
            .expect("valid word split regex")
    })
}

/// Parses entities from `text` according to `flags` (`TextUtilities::ParseEntities`).
///
/// If `flags` includes `TextParseFlags::MARKDOWN`, markdown delimiters are parsed and stripped
/// first, then URLs, emails, and any requested mentions/hashtags/cashtags/bot commands are detected.
pub fn parse_entities(text: &str, flags: TextParseFlags) -> TextWithEntities {
    let mut result = TextWithEntities::simple(text);
    parse_entities_in_place(&mut result, flags);
    result
}

/// Parses entities in place, adding newly detected entities alongside existing ones
/// (`TextUtilities::ParseEntities(TextWithEntities&, int32)`).
pub fn parse_entities_in_place(result: &mut TextWithEntities, flags: TextParseFlags) {
    if flags.contains(TextParseFlags::MARKDOWN) {
        parse_markdown_in_place(result);
    }

    let with_hashtags = flags.contains(TextParseFlags::HASHTAGS);
    let with_mentions = flags.contains(TextParseFlags::MENTIONS);
    let with_bot_commands = flags.contains(TextParseFlags::BOT_COMMANDS);
    let with_cashtags = flags.contains(TextParseFlags::CASHTAGS);

    let text = result.text.as_str();
    let len = text.len();
    if len == 0 {
        return;
    }

    let mapper = OffsetMapper::new(text);
    let mut new_entities: EntitiesInText = Vec::new();
    let existing_count = result.entities.len();
    let mut existing_idx = 0usize;
    let mut existing_end_u16 = 0i32;

    let mut offset = 0usize;
    let mut match_offset = 0usize;
    let mut mention_skip = 0usize;

    while offset < len {
        let m_domain = match_domain(text, match_offset);
        let m_explicit = match_domain_explicit(text, match_offset);
        let m_hashtag = if with_hashtags {
            regex_hashtag(true).captures_at(text, match_offset)
        } else {
            None
        };
        let mut m_mention = if with_mentions {
            regex_mention().captures_at(text, mention_skip.max(match_offset))
        } else {
            None
        };
        let m_bot = if with_bot_commands {
            regex_bot_command().captures_at(text, match_offset)
        } else {
            None
        };
        let m_cashtag = if with_cashtags {
            regex_cashtag().captures_at(text, match_offset)
        } else {
            None
        };

        let mut domain_match = m_domain;
        if let Some(exp) = m_explicit {
            let dom_start = domain_match.as_ref().map_or(usize::MAX, |d| d.start);
            if exp.start < dom_start {
                domain_match = Some(exp);
            }
        }
        let domain_start = domain_match.as_ref().map_or(usize::MAX, |d| d.start);

        // Process hashtag match bounds & digit-only exclusion
        let mut hashtag_start = usize::MAX;
        let mut hashtag_end = usize::MAX;
        let mut hashtag_ignore = false;
        if let Some(ref caps) = m_hashtag
            && let Some(full) = caps.get(0)
        {
            hashtag_start = full.start();
            hashtag_end = full.end();
            if let Some(g1) = caps.get(1) {
                hashtag_start += g1.as_str().len();
            }
            if let Some(g3) = caps.get(3) {
                hashtag_end -= g3.as_str().len();
            }
            let body = &text[hashtag_start + 1..hashtag_end];
            if regex_hashtag_exclude().is_match(body) {
                hashtag_ignore = true;
            }
        }

        // Process mention match bounds & letter/alphanumeric validation loop
        let mut mention_start = usize::MAX;
        let mut mention_end = usize::MAX;
        let mut mention_ignore = false;
        while let Some(caps) = m_mention {
            let Some(full) = caps.get(0) else {
                break;
            };
            mention_start = full.start();
            mention_end = full.end();
            if let Some(g1) = caps.get(1) {
                mention_start += g1.as_str().len();
            }
            if let Some(g2) = caps.get(2) {
                mention_end -= g2.as_str().len();
            }
            let first_after_at = text[mention_start + 1..mention_end].chars().next();
            let last_char = text[mention_start..mention_end].chars().next_back();
            let valid = first_after_at.is_some_and(char::is_alphabetic)
                && last_char.is_some_and(char::is_alphanumeric);
            if !valid {
                mention_skip = mention_end;
                m_mention = regex_mention().captures_at(text, mention_skip.max(match_offset));
                if m_mention.is_none() {
                    mention_ignore = true;
                    break;
                }
            } else {
                break;
            }
        }

        // Process bot command match bounds
        let mut bot_start = usize::MAX;
        let mut bot_end = usize::MAX;
        if let Some(ref caps) = m_bot
            && let Some(full) = caps.get(0)
        {
            bot_start = full.start();
            bot_end = full.end();
            if let Some(g1) = caps.get(1) {
                bot_start += g1.as_str().len();
            }
            if let Some(g3) = caps.get(3) {
                bot_end -= g3.as_str().len();
            }
        }

        // Process cashtag match bounds
        let mut cash_start = usize::MAX;
        let mut cash_end = usize::MAX;
        if let Some(ref caps) = m_cashtag
            && let Some(full) = caps.get(0)
        {
            cash_start = full.start();
            cash_end = full.end();
            if let Some(g1) = caps.get(1) {
                cash_start += g1.as_str().len();
            }
            if let Some(g3) = caps.get(3) {
                cash_end -= g3.as_str().len();
            }
        }

        if domain_start == usize::MAX
            && hashtag_start == usize::MAX
            && mention_start == usize::MAX
            && bot_start == usize::MAX
            && cash_start == usize::MAX
        {
            break;
        }

        let mut lnk_type = EntityType::Url;
        let mut lnk_start = 0usize;
        let mut lnk_end = 0usize;

        if mention_start < hashtag_start
            && mention_start < domain_start
            && mention_start < bot_start
            && mention_start < cash_start
        {
            if mention_ignore {
                offset = mention_end;
                match_offset = mention_end;
                continue;
            }
            lnk_type = EntityType::Mention;
            lnk_start = mention_start;
            lnk_end = mention_end;
        } else if hashtag_start < domain_start
            && hashtag_start < bot_start
            && hashtag_start < cash_start
        {
            if hashtag_ignore {
                offset = hashtag_end;
                match_offset = hashtag_end;
                continue;
            }
            lnk_type = EntityType::Hashtag;
            lnk_start = hashtag_start;
            lnk_end = hashtag_end;
        } else if bot_start < domain_start && bot_start < cash_start {
            lnk_type = EntityType::BotCommand;
            lnk_start = bot_start;
            lnk_end = bot_end;
        } else if cash_start < domain_start {
            lnk_type = EntityType::Cashtag;
            lnk_start = cash_start;
            lnk_end = cash_end;
        } else {
            let Some(dm) = domain_match else {
                break;
            };
            let protocol = dm.protocol.to_lowercase();
            let top_domain = dm.top_domain.to_lowercase();
            let is_protocol_valid = protocol.is_empty() || is_valid_protocol(&protocol);
            let is_top_domain_valid = !protocol.is_empty() || is_valid_top_domain(&top_domain);

            if protocol.is_empty() && dm.start > offset + 1 && text.as_bytes()[dm.start - 1] == b'@'
            {
                let for_mail_name = &text[offset..dm.start - 1];
                if let Some(m_mail) = regex_mail_name_at_end().find(for_mail_name) {
                    let mail_start = offset + m_mail.start();
                    lnk_type = EntityType::Email;
                    lnk_start = mail_start;
                    lnk_end = dm.end;
                }
            }

            if lnk_type == EntityType::Url && lnk_end == 0 {
                if !is_protocol_valid || !is_top_domain_valid {
                    match_offset = dm.end;
                    continue;
                }
                lnk_start = dm.start;

                // Scan URL tail with balanced brackets and trailing punctuation handling.
                let tail_end = scan_url_tail(text, dm.end);
                if tail_end > dm.end {
                    let next_byte = text.as_bytes()[dm.end];
                    if next_byte != b'/' && next_byte != b'?' {
                        match_offset = dm.end;
                        continue;
                    }
                }
                lnk_end = tail_end;
            }
        }

        let Some((lnk_start_u16, lnk_len_u16)) = mapper.utf8_range_to_utf16(lnk_start..lnk_end)
        else {
            offset = lnk_end;
            match_offset = lnk_end;
            continue;
        };
        let lnk_start_i32 = lnk_start_u16 as i32;
        let lnk_len_i32 = lnk_len_u16 as i32;

        while existing_idx < existing_count
            && result.entities[existing_idx].offset() <= lnk_start_i32
        {
            let entity = result.entities[existing_idx].clone();
            existing_end_u16 = existing_end_u16.max(entity.offset() + entity.length());
            new_entities.push(entity);
            existing_idx += 1;
        }
        if lnk_start_i32 >= existing_end_u16 {
            new_entities.push(EntityInText::new(lnk_type, lnk_start_i32, lnk_len_i32));
        }

        offset = lnk_end;
        match_offset = lnk_end;
    }

    while existing_idx < existing_count {
        new_entities.push(result.entities[existing_idx].clone());
        existing_idx += 1;
    }
    result.entities = new_entities;
}

/// Scans past `domain_end` to find the end of a URL, handling trailing punctuation
/// (`is_almost_link_end`) and balanced brackets `() [] {} <>`.
pub(crate) fn scan_url_tail(text: &str, domain_end: usize) -> usize {
    let mut parenth: Vec<(usize, char)> = Vec::new();
    let mut p = domain_end;
    let len = text.len();

    while p < len {
        let Some(mut ch) = text[p..].chars().next() else {
            break;
        };
        if is_link_end(ch) {
            break;
        } else if is_almost_link_end(ch) {
            let mut end_test = p + ch.len_utf8();
            while end_test < len {
                let Some(next_ch) = text[end_test..].chars().next() else {
                    break;
                };
                if is_almost_link_end(next_ch) {
                    end_test += next_ch.len_utf8();
                } else {
                    break;
                }
            }
            if end_test >= len || text[end_test..].chars().next().is_none_or(is_link_end) {
                break;
            }
            p = end_test;
            let Some(next_ch) = text[p..].chars().next() else {
                break;
            };
            ch = next_ch;
        }
        if matches!(ch, '(' | '[' | '{' | '<') {
            parenth.push((p, ch));
        } else if matches!(ch, ')' | ']' | '}' | '>') {
            let Some((open_pos, open)) = parenth.pop() else {
                break;
            };
            let matched = (ch == ')' && open == '(')
                || (ch == ']' && open == '[')
                || (ch == '}' && open == '{')
                || (ch == '>' && open == '<');
            if !matched {
                p = open_pos;
                break;
            }
        }
        p += ch.len_utf8();
    }
    p
}

/// Cleans, parses entities/markdown, and trims a message for sending
/// (`TextUtilities::PrepareForSending`).
pub fn prepare_for_sending(result: &mut TextWithEntities, flags: TextParseFlags) {
    apply_server_cleaning(result);
    if !flags.is_empty() {
        parse_entities_in_place(result, flags);
    }
    trim(result);
}

/// Convenience helper for preparing plain text for sending (`TextUtilities::PrepareForSending(QString)`).
pub fn prepare_for_sending_str(text: &str, option: PrepareTextOption) -> String {
    let mut result = TextWithEntities::simple(text);
    let flags = match option {
        PrepareTextOption::IgnoreLinks => TextParseFlags::empty(),
        PrepareTextOption::CheckLinks => {
            TextParseFlags::LINKS
                | TextParseFlags::MENTIONS
                | TextParseFlags::HASHTAGS
                | TextParseFlags::BOT_COMMANDS
        }
    };
    prepare_for_sending(&mut result, flags);
    result.text
}

/// Replaces `\t` with two spaces, removes `\r`, and replaces server-sanitized characters with `' '`,
/// updating UTF-16 entity offsets and lengths faithfully (`TextUtilities::ApplyServerCleaning`).
pub fn apply_server_cleaning(result: &mut TextWithEntities) {
    if result.text.is_empty() {
        return;
    }

    // Build a UTF-16 position mapping from original code unit positions to cleaned positions.
    let old_u16_len = utf16_len(&result.text);
    let mut pos_map: Vec<i32> = Vec::with_capacity(old_u16_len + 1);
    let mut cleaned = String::with_capacity(result.text.len());
    let mut new_u16 = 0i32;

    for ch in result.text.chars() {
        let step = ch.len_utf16();
        if ch == '\t' {
            pos_map.push(new_u16);
            cleaned.push_str("  ");
            new_u16 += 2;
        } else if ch == '\r' {
            pos_map.push(new_u16);
            // Removed: new_u16 does not advance.
        } else if is_replaced_by_space(ch) {
            for _ in 0..step {
                pos_map.push(new_u16);
            }
            cleaned.push(' ');
            new_u16 += 1;
        } else {
            for k in 0..step {
                pos_map.push(new_u16 + k as i32);
            }
            cleaned.push(ch);
            new_u16 += step as i32;
        }
    }
    pos_map.push(new_u16);

    for entity in &mut result.entities {
        let old_start = entity.offset().clamp(0, old_u16_len as i32) as usize;
        let old_end = (entity.offset() + entity.length()).clamp(0, old_u16_len as i32) as usize;
        let new_start = pos_map[old_start];
        let new_end = pos_map[old_end];
        let new_len = (new_end - new_start).max(0);
        *entity = EntityInText::with_data(
            entity.entity_type(),
            new_start,
            new_len,
            entity.data().to_string(),
        );
    }

    result.text = cleaned;
}

/// Trims whitespace/bad characters from both ends of `result`, updating entity offsets and
/// preserving leading whitespace inside leading monospace (`Pre`/`Code`) entities
/// (`TextUtilities::Trim`).
pub fn trim(result: &mut TextWithEntities) {
    // Right trim
    let mut right_byte_end = None;
    for (idx, ch) in result.text.char_indices().rev() {
        if !is_trimmed(ch) {
            right_byte_end = Some(idx + ch.len_utf8());
            break;
        }
    }
    let Some(end_byte) = right_byte_end else {
        *result = TextWithEntities::new();
        return;
    };
    if end_byte < result.text.len() {
        result.text.truncate(end_byte);
        let new_u16_len = utf16_len(&result.text) as i32;
        for entity in &mut result.entities {
            entity.update_text_end(new_u16_len);
        }
    }

    let text_u16_len = utf16_len(&result.text);
    let first_monospace = EntityInText::first_monospace_offset(&result.entities, text_u16_len);

    // Left trim (stops at first non-trimmed char or first_monospace offset)
    let mut cur_u16 = 0usize;
    for (byte_idx, ch) in result.text.char_indices() {
        if !is_trimmed(ch) || cur_u16 == first_monospace {
            if byte_idx > 0 {
                let shift = cur_u16 as i32;
                for entity in &mut result.entities {
                    entity.shift_left(shift);
                }
                result.text = result.text[byte_idx..].to_string();
            }
            break;
        }
        cur_u16 += ch.len_utf16();
    }
}

/// Splits the next message chunk of up to `limit` characters/codepoints from `left` into `sending`,
/// choosing the best break point (paragraphs, newlines, sentence ends, spaces, word separators)
/// without splitting emojis or non-breakable entities (`TextUtilities::CutPart`).
///
/// Returns `false` if `left` is empty.
pub fn cut_part(sending: &mut TextWithEntities, left: &mut TextWithEntities, limit: usize) -> bool {
    if limit == 0 || left.text.is_empty() {
        return false;
    }

    let units: Vec<u16> = left.text.encode_utf16().collect();
    let end = units.len();

    let half = limit / 2;
    let mut s = 0usize;
    let mut good_level = 0i32;
    let mut good_pos = 0usize;
    let mut i = 0usize;

    let is_breakable_for_cut = |t: EntityType| {
        matches!(
            t,
            EntityType::Pre
                | EntityType::Blockquote
                | EntityType::Code
                | EntityType::Bold
                | EntityType::Italic
                | EntityType::Underline
                | EntityType::StrikeOut
                | EntityType::Spoiler
        )
    };

    while i < end {
        if s > half {
            // Check ALL entities spanning UTF-16 offset `i`, not just a single cursor index,
            // so nested and overlapping entities (e.g. Bold inside Blockquote, Bold + Italic)
            // are never dropped or corrupted when splitting (`ui/text/text_entity.cpp`,
            // `TextUtilities::CutPart`).
            let i_i32 = i as i32;
            let mut in_entity = false;
            let mut can_break_entity = true;
            let mut has_block_boundary = false;

            for entity in &left.entities {
                let ent_start = entity.offset();
                let ent_end = ent_start + entity.length();
                if i_i32 > ent_start && i_i32 < ent_end {
                    in_entity = true;
                    if !matches!(
                        entity.entity_type(),
                        EntityType::Pre | EntityType::Blockquote | EntityType::Code
                    ) {
                        can_break_entity = false;
                    }
                }
                if ((i + 1) as i32 == ent_start || i_i32 == ent_end)
                    && matches!(
                        entity.entity_type(),
                        EntityType::Pre | EntityType::Blockquote
                    )
                {
                    has_block_boundary = true;
                }
            }
            let no_entity_level = if in_entity { 0 } else { 1 };

            let mut mark_good = |new_level: i32| {
                if good_level > new_level {
                    return;
                }
                good_level = new_level;
                good_pos = i;
            };

            let ch = char::from_u32(u32::from(units[i])).unwrap_or('\u{FFFD}');
            let next_is_newline =
                i + 1 < end && char::from_u32(u32::from(units[i + 1])).is_some_and(is_newline);
            let prev_ch = if i > 0 {
                char::from_u32(u32::from(units[i - 1])).unwrap_or('\u{FFFD}')
            } else {
                '\0'
            };

            if in_entity && !can_break_entity {
                mark_good(0);
            } else if is_newline(ch) {
                if in_entity {
                    if next_is_newline {
                        mark_good(12);
                    } else {
                        mark_good(11);
                    }
                } else if next_is_newline {
                    mark_good(15);
                } else if has_block_boundary {
                    mark_good(14);
                } else {
                    mark_good(13);
                }
            } else if is_space(ch) {
                if is_sentence_end(prev_ch) {
                    mark_good(9 + no_entity_level);
                } else if is_sentence_part_end(prev_ch) {
                    mark_good(7 + no_entity_level);
                } else {
                    mark_good(5 + no_entity_level);
                }
            } else if is_word_separator(prev_ch) {
                mark_good(3 + no_entity_level);
            } else {
                mark_good(1 + no_entity_level);
            }
        }

        // Advance past whole emoji or surrogate pair, counting Unicode scalar values in `s`.
        if let Some((_, elen)) = find_emoji_utf16(&units, i) {
            let mut j = 0;
            while j < elen {
                let u = units[i + j];
                if (0xD800..=0xDBFF).contains(&u)
                    && j + 1 < elen
                    && (0xDC00..=0xDFFF).contains(&units[i + j + 1])
                {
                    j += 2;
                } else {
                    j += 1;
                }
                s += 1;
            }
            i += elen;
        } else if (0xD800..=0xDBFF).contains(&units[i])
            && i + 1 < end
            && (0xDC00..=0xDFFF).contains(&units[i + 1])
        {
            i += 2;
            s += 1;
        } else {
            i += 1;
            s += 1;
        }

        if s >= limit {
            let split_u16 = if good_pos > 0 { good_pos } else { i };
            let mapper = OffsetMapper::new(&left.text);
            let split_byte = mapper.utf16_to_utf8_floor(split_u16);
            sending.text = left.text[..split_byte].to_string();
            left.text = left.text[split_byte..].to_string();
            let shift = split_u16 as i32;

            let old_entities = std::mem::take(&mut left.entities);
            sending.entities.clear();
            for entity in old_entities {
                let ent_start = entity.offset();
                let ent_end = ent_start + entity.length();
                if ent_end <= shift {
                    sending.entities.push(entity);
                } else if ent_start >= shift {
                    let mut moved = entity;
                    moved.shift_left(shift);
                    if moved.length() > 0 {
                        left.entities.push(moved);
                    }
                } else if is_breakable_for_cut(entity.entity_type()) {
                    let mut head = entity.clone();
                    head.update_text_end(shift);
                    if head.length() > 0 {
                        sending.entities.push(head);
                    }
                    let mut tail = entity;
                    tail.shift_left(shift);
                    if tail.length() > 0 {
                        left.entities.push(tail);
                    }
                }
            }
            return true;
        }
    }

    *sending = std::mem::take(left);
    true
}

/// Escapes `\` and `[` for rich parsing (`TextUtilities::EscapeForRichParsing`).
pub fn escape_for_rich_parsing(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch == '\\' || ch == '[' {
            result.push('\\');
        }
        result.push(ch);
    }
    result
}

/// Trims leading/trailing trimmed characters and replaces newlines with ASCII space
/// (`TextUtilities::SingleLine(QString)`).
pub fn single_line(text: &str) -> String {
    let trimmed = text.trim_matches(is_trimmed);
    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        if is_newline(ch) {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out
}

/// Trims and converts a `TextWithEntities` to a single line, preserving entities
/// (`TextUtilities::SingleLine(TextWithEntities)`).
pub fn single_line_with_entities(text: &TextWithEntities) -> TextWithEntities {
    let mut copy = text.clone();
    trim(&mut copy);
    TextWithEntities {
        text: single_line(&copy.text),
        entities: copy.entities,
    }
}

/// Strips combining diacritics and folds accented Latin/fullwidth/Cyrillic `ё` characters
/// to their base letters (`TextUtilities::RemoveAccents`).
pub fn remove_accents(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if (ch as u32) < 128 {
            out.push(ch);
            continue;
        }
        if is_diacritic(ch) {
            continue;
        }
        if let Some(folded) = remove_one_accent(ch as u32) {
            out.push(folded);
        } else {
            out.push(ch);
        }
    }
    out
}

/// Removes all recognized emoji sequences from `text` (`TextUtilities::RemoveEmoji`).
pub fn remove_emoji(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pos = 0usize;
    while pos < text.len() {
        if let Some((_, len)) = find_emoji(text, pos) {
            pos += len;
        } else if let Some(ch) = text[pos..].chars().next() {
            out.push(ch);
            pos += ch.len_utf8();
        } else {
            break;
        }
    }
    out
}

/// Computes an accent-folded lowercase sort key (`TextUtilities::NameSortKey`).
#[inline]
pub fn name_sort_key(text: &str) -> String {
    remove_accents(text).to_lowercase()
}

/// Splits a search query into accent-folded lowercase words (`TextUtilities::PrepareSearchWords`).
pub fn prepare_search_words(query: &str) -> Vec<String> {
    prepare_search_words_with_splitter(query, regex_word_split())
}

/// Splits a search query using a custom splitter regex (`TextUtilities::PrepareSearchWords`).
pub fn prepare_search_words_with_splitter(query: &str, splitter: &Regex) -> Vec<String> {
    let clean = remove_accents(&query.trim().to_lowercase());
    if clean.is_empty() {
        return Vec::new();
    }
    splitter
        .split(&clean)
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Accent folding lookup table ported directly from `RemoveOneAccent` in `text_entity.cpp`
/// (originally from `https://github.com/aristus/accent-folding`).
fn remove_one_accent(code: u32) -> Option<char> {
    let mapped: u32 = match code {
        7834 | 193 | 225 | 192 | 224 | 258 | 259 | 7854 | 7855 | 7856 | 7857 | 7860 | 7861
        | 7858 | 7859 | 194 | 226 | 7844 | 7845 | 7846 | 7847 | 7850 | 7851 | 7848 | 7849 | 461
        | 462 | 197 | 229 | 506 | 507 | 196 | 228 | 478 | 479 | 195 | 227 | 550 | 551 | 480
        | 481 | 260 | 261 | 256 | 257 | 7842 | 7843 | 512 | 513 | 514 | 515 | 7840 | 7841
        | 7862 | 7863 | 7852 | 7853 | 7680 | 7681 | 570 | 11365 | 508 | 509 | 482 | 483 | 313 => 97, // 'a'
        7682 | 7683 | 7684 | 7685 | 7686 | 7687 | 579 | 384 | 7532 | 385 | 595 | 386 | 387 => 98, // 'b'
        262 | 263 | 264 | 265 | 268 | 269 | 266 | 267 | 199 | 231 | 7688 | 7689 | 571 | 572
        | 391 | 392 | 597 => 99, // 'c'
        270 | 271 | 7690 | 7691 | 7696 | 7697 | 7692 | 7693 | 7698 | 7699 | 7694 | 7695 | 272
        | 273 | 7533 | 393 | 598 | 394 | 599 | 395 | 396 | 545 | 240 => 100, // 'd'
        201 | 399 | 398 | 477 | 233 | 200 | 232 | 276 | 277 | 202 | 234 | 7870 | 7871 | 7872
        | 7873 | 7876 | 7877 | 7874 | 7875 | 282 | 283 | 203 | 235 | 7868 | 7869 | 278 | 279
        | 552 | 553 | 7708 | 7709 | 280 | 281 | 274 | 275 | 7702 | 7703 | 7700 | 7701 | 7866
        | 7867 | 516 | 517 | 518 | 519 | 7864 | 7865 | 7878 | 7879 | 7704 | 7705 | 7706 | 7707
        | 582 | 583 | 602 | 605 => 101, // 'e'
        7710 | 7711 | 7534 | 401 | 402 => 102, // 'f'
        500 | 501 | 286 | 287 | 284 | 285 | 486 | 487 | 288 | 289 | 290 | 291 | 7712 | 7713
        | 484 | 485 | 403 | 608 => 103, // 'g'
        292 | 293 | 542 | 543 | 7718 | 7719 | 7714 | 7715 | 7720 | 7721 | 7716 | 7717 | 7722
        | 7723 | 817 | 7830 | 294 | 295 | 11367 | 11368 => 104, // 'h'
        205 | 237 | 204 | 236 | 300 | 301 | 206 | 238 | 463 | 464 | 207 | 239 | 7726 | 7727
        | 296 | 297 | 304 | 302 | 303 | 298 | 299 | 7880 | 7881 | 520 | 521 | 522 | 523 | 7882
        | 7883 | 7724 | 7725 | 305 | 407 | 616 => 105, // 'i'
        308 | 309 | 780 | 496 | 567 | 584 | 585 | 669 | 607 | 644 => 106, // 'j'
        7728 | 7729 | 488 | 489 | 310 | 311 | 7730 | 7731 | 7732 | 7733 | 408 | 409 | 11369
        | 11370 => 107, // 'k'
        314 | 317 | 318 | 315 | 316 | 7734 | 7735 | 7736 | 7737 | 7740 | 7741 | 7738 | 7739
        | 321 | 322 | 803 | 319 | 320 | 573 | 410 | 11360 | 11361 | 11362 | 619 | 620 | 621
        | 564 => 108, // 'l'
        7742 | 7743 | 7744 | 7745 | 7746 | 7747 | 625 => 109, // 'm'
        323 | 324 | 504 | 505 | 327 | 328 | 209 | 241 | 7748 | 7749 | 325 | 326 | 7750 | 7751
        | 7754 | 7755 | 7752 | 7753 | 413 | 626 | 544 | 414 | 627 | 565 => 110, // 'n'
        211 | 243 | 210 | 242 | 334 | 335 | 212 | 244 | 7888 | 7889 | 7890 | 7891 | 7894 | 7895
        | 7892 | 7893 | 465 | 466 | 214 | 246 | 554 | 555 | 336 | 337 | 213 | 245 | 7756 | 7757
        | 7758 | 7759 | 556 | 557 | 558 | 559 | 560 | 561 | 216 | 248 | 510 | 511 | 490 | 491
        | 492 | 493 | 332 | 333 | 7762 | 7763 | 7760 | 7761 | 7886 | 7887 | 524 | 525 | 526
        | 527 | 416 | 417 | 7898 | 7899 | 7900 | 7901 | 7904 | 7905 | 7902 | 7903 | 7906 | 7907
        | 7884 | 7885 | 7896 | 7897 | 415 | 629 => 111, // 'o'
        7764 | 7765 | 7766 | 7767 | 11363 | 420 | 421 | 771 => 112, // 'p'
        672 | 586 | 587 => 113,                // 'q'
        340 | 341 | 344 | 345 | 7768 | 7769 | 342 | 343 | 528 | 529 | 530 | 531 | 7770 | 7771
        | 7772 | 7773 | 7774 | 7775 | 588 | 589 | 7538 | 636 | 11364 | 637 | 638 | 7539 => 114, // 'r'
        223 | 346 | 347 | 7780 | 7781 | 348 | 349 | 352 | 353 | 7782 | 7783 | 7776 | 7777
        | 7835 | 350 | 351 | 7778 | 7779 | 7784 | 7785 | 536 | 537 | 642 | 809 => 115, // 's'
        776 | 222 | 254 | 356 | 357 | 7831 | 7786 | 7787 | 354 | 355 | 7788 | 7789 | 538 | 539
        | 7792 | 7793 | 7790 | 7791 | 358 | 359 | 574 | 11366 | 7541 | 427 | 428 | 429 | 430
        | 648 | 566 => 116, // 't'
        218 | 250 | 217 | 249 | 364 | 365 | 219 | 251 | 467 | 468 | 366 | 367 | 220 | 252 | 471
        | 472 | 475 | 476 | 473 | 474 | 469 | 470 | 368 | 369 | 360 | 361 | 7800 | 7801 | 370
        | 371 | 362 | 363 | 7802 | 7803 | 7910 | 7911 | 532 | 533 | 534 | 535 | 431 | 432
        | 7912 | 7913 | 7914 | 7915 | 7918 | 7919 | 7916 | 7917 | 7920 | 7921 | 7908 | 7909
        | 7794 | 7795 | 7798 | 7799 | 7796 | 7797 | 580 | 649 => 117, // 'u'
        7804 | 7805 | 7806 | 7807 | 434 | 651 => 118, // 'v'
        7810 | 7811 | 7808 | 7809 | 372 | 373 | 7832 | 7812 | 7813 | 7814 | 7815 | 7816 | 7817 => {
            119
        } // 'w'
        7818..=7821 => 120,                           // 'x'
        778 | 221 | 253 | 7922 | 7923 | 374 | 375 | 7833 | 376 | 255 | 7928 | 7929 | 7822
        | 7823 | 562 | 563 | 7926 | 7927 | 7924 | 7925 | 655 | 590 | 591 | 435 | 436 => 121, // 'y'
        377 | 378 | 7824 | 7825 | 381 | 382 | 379 | 380 | 7826 | 7827 | 7828 | 7829 | 437 | 438
        | 548 | 549 | 656 | 657 | 11371 | 11372 | 494 | 495 | 442 => 122, // 'z'
        // Fullwidth ASCII digits and letters
        65296 => 48,
        65297 => 49,
        65298 => 50,
        65299 => 51,
        65300 => 52,
        65301 => 53,
        65302 => 54,
        65303 => 55,
        65304 => 56,
        65305 => 57,
        65313 => 65,
        65314 => 66,
        65315 => 67,
        65316 => 68,
        65317 => 69,
        65318 => 70,
        65319 => 71,
        65320 => 72,
        65321 => 73,
        65322 => 74,
        65323 => 75,
        65324 => 76,
        65325 => 77,
        65326 => 78,
        65327 => 79,
        65328 => 80,
        65329 => 81,
        65330 => 82,
        65331 => 83,
        65332 => 84,
        65333 => 85,
        65334 => 86,
        65335 => 87,
        65336 => 88,
        65337 => 89,
        65338 => 90,
        65345 => 97,
        65346 => 98,
        65347 => 99,
        65348 => 100,
        65349 => 101,
        65350 => 102,
        65351 => 103,
        65352 => 104,
        65353 => 105,
        65354 => 106,
        65355 => 107,
        65356 => 108,
        65357 => 109,
        65358 => 110,
        65359 => 111,
        65360 => 112,
        65361 => 113,
        65362 => 114,
        65363 => 115,
        65364 => 116,
        65365 => 117,
        65366 => 118,
        65367 => 119,
        65368 => 120,
        65369 => 121,
        65370 => 122,
        // Cyrillic ё -> е
        1105 => 1077,
        _ => return None,
    };
    char::from_u32(mapped)
}
