//! Parser for Telegram Desktop `.strings` language files (`resources/lang.strings`).
//!
//! Port of `Telegram/codegen/codegen/lang/parsed_file.cpp`,
//! `Telegram/codegen/codegen/common/basic_tokenized_file.cpp`, and
//! `Telegram/lib_base/base/parse_helper.cpp`.

use std::collections::HashMap;
use std::fmt;

/// Number of plural variants per pluralized phrase (`zero`, `one`, `two`, `few`, `many`, `other`).
pub const PLURAL_PART_COUNT: usize = 6;

/// Ordered plural variant suffixes matching CLDR / Telegram Desktop indices `0..6`.
pub const PLURAL_PARTS: [&str; PLURAL_PART_COUNT] = ["zero", "one", "two", "few", "many", "other"];

/// Special plural tag names that cannot appear in non-plural keys.
pub const PLURAL_TAGS: [&str; 3] = ["count", "count_short", "count_decimal"];

/// Control character marking the start and end of an encoded tag placeholder (`0x0010`).
pub const TEXT_COMMAND_CHAR: char = '\u{0010}';

/// Control character identifying a language-tag placeholder (`0x0020`).
pub const TEXT_COMMAND_LANG_TAG_CHAR: char = '\u{0020}';

/// Error produced while parsing a `.strings` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringsParseError {
    /// 1-based line number where the error occurred.
    pub line: usize,
    /// Human-readable error message.
    pub message: String,
}

impl fmt::Display for StringsParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for StringsParseError {}

/// A single entry produced by parsing a `.strings` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedEntry {
    /// Internal key (for plural entries, `<key_base>__plural<0..5>`).
    pub key: String,
    /// Default value with `{tag}` placeholders replaced by 4-char command markers.
    pub value: String,
    /// Base key name for plural entries; `None` for non-plural entries.
    pub key_base: Option<String>,
    /// Ordered list of tag names used by this entry (for plural entries, starts with `"count"`).
    pub tags: Vec<String>,
}

/// Complete parsed result of a `.strings` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLangPack {
    /// All expanded entries (plural entries occupy 6 consecutive slots).
    pub entries: Vec<ParsedEntry>,
    /// Ordered unique tag names; their index is the tag ID.
    pub tags: Vec<String>,
}

/// Compute the internal plural entry key matching `codegen::lang::ComputePluralKey`.
pub fn compute_plural_key(base: &str, index: usize) -> String {
    format!("{base}__plural{index}")
}

/// Build the 4-character command marker used inside lang values for `tag_index`.
pub fn prepare_command_string(tag_index: u16) -> String {
    let mut s = String::with_capacity(6);
    s.push(TEXT_COMMAND_CHAR);
    s.push(TEXT_COMMAND_LANG_TAG_CHAR);
    let encoded = char::from_u32(0x0020u32 + u32::from(tag_index)).unwrap_or('\u{FFFD}');
    s.push(encoded);
    s.push(TEXT_COMMAND_CHAR);
    s
}

/// Strip `// ...` and `/* ... */` comments outside string literals, preserving newlines.
///
/// Port of `base::parse::stripComments` from `Telegram/lib_base/base/parse_helper.cpp`.
pub fn strip_comments(content: &[u8]) -> Vec<u8> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum CommentState {
        None,
        SingleLine,
        MultiLine,
    }

    let mut state = CommentState::None;
    let mut inside_string = false;
    let mut result = Vec::with_capacity(content.len());
    let mut i = 0;
    let len = content.len();

    while i < len {
        let cur = content[i];
        let next = if i + 1 < len { content[i + 1] } else { 0 };

        if state == CommentState::None && cur == b'"' {
            let mut backslashes = 0usize;
            let mut k = i;
            while k > 0 && content[k - 1] == b'\\' {
                backslashes += 1;
                k -= 1;
            }
            if backslashes.is_multiple_of(2) {
                inside_string = !inside_string;
            }
        }

        if inside_string {
            result.push(cur);
            i += 1;
            continue;
        }

        match state {
            CommentState::None => {
                if cur == b'/' && next == b'/' {
                    state = CommentState::SingleLine;
                    i += 2;
                } else if cur == b'/' && next == b'*' {
                    state = CommentState::MultiLine;
                    i += 2;
                } else {
                    result.push(cur);
                    i += 1;
                }
            }
            CommentState::SingleLine => {
                if cur == b'\r' && next == b'\n' {
                    result.push(b' ');
                    result.push(b'\r');
                    result.push(b'\n');
                    state = CommentState::None;
                    i += 2;
                } else if cur == b'\n' {
                    result.push(b' ');
                    result.push(b'\n');
                    state = CommentState::None;
                    i += 1;
                } else {
                    i += 1;
                }
            }
            CommentState::MultiLine => {
                if cur == b'*' && next == b'/' {
                    result.push(b' ');
                    state = CommentState::None;
                    i += 2;
                } else if cur == b'\r' && next == b'\n' {
                    result.push(b' ');
                    result.push(b'\r');
                    result.push(b'\n');
                    i += 2;
                } else if cur == b'\n' {
                    result.push(b' ');
                    result.push(b'\n');
                    i += 1;
                } else {
                    i += 1;
                }
            }
        }
    }

    result
}

/// Validate a `.strings` key name matching `ValidateKey` in `parsed_file.cpp`.
///
/// When `allow_all_plurals` is `false` (strict English source mode), only `#one` and `#other`
/// suffixes are accepted. When `true`, all 6 CLDR plural suffixes are accepted.
pub fn validate_key(key: &str, allow_all_plurals: bool) -> bool {
    if key.is_empty() || key.contains("__") {
        return false;
    }
    let (base, suffix) = match key.split_once('#') {
        Some((b, s)) => (b, Some(s)),
        None => (key, None),
    };
    if base.is_empty() {
        return false;
    }
    if !base
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
    {
        return false;
    }
    if let Some(s) = suffix {
        let lower = s.to_ascii_lowercase();
        if allow_all_plurals {
            PLURAL_PARTS.contains(&lower.as_str())
        } else {
            lower == "one" || lower == "other"
        }
    } else {
        true
    }
}

/// Validate a `{tag}` identifier matching `ValidateTag` in `parsed_file.cpp`.
pub fn validate_tag(tag: &str) -> bool {
    !tag.is_empty()
        && !tag.contains("__")
        && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Parse an English source `lang.strings` file using strict codegen rules.
pub fn parse_lang_strings(input: &str) -> Result<ParsedLangPack, StringsParseError> {
    parse_lang_strings_with_options(input, false)
}

/// Parse a `.strings` file, optionally accepting all 6 plural suffixes (`#zero`..`#other`).
pub fn parse_lang_strings_with_options(
    input: &str,
    allow_all_plurals: bool,
) -> Result<ParsedLangPack, StringsParseError> {
    let stripped = strip_comments(input.as_bytes());
    let text = String::from_utf8(stripped).map_err(|e| StringsParseError {
        line: 1,
        message: format!("invalid UTF-8 after stripping comments: {e}"),
    })?;

    let mut lexer = Lexer::new(text.as_bytes());
    let mut state = ParserState::new(allow_all_plurals);

    loop {
        lexer.skip_whitespace();
        if lexer.at_end() {
            break;
        }
        let key_line = lexer.line;
        let key = lexer.read_string()?;
        if !validate_key(&key, allow_all_plurals) {
            return Err(StringsParseError {
                line: key_line,
                message: format!("invalid string key name '{key}'"),
            });
        }

        lexer.skip_whitespace();
        lexer.expect_byte(b'=', &format!("'=' for '{key}' key"))?;

        lexer.skip_whitespace();
        let val_line = lexer.line;
        let value = lexer.read_string()?;

        lexer.skip_whitespace();
        lexer.expect_byte(b';', &format!("';' after value for '{key}' key"))?;

        state.add_entity(key, &value, val_line)?;
    }

    state.fill_plural_tags()?;
    Ok(state.result)
}

struct Lexer<'a> {
    bytes: &'a [u8],
    pos: usize,
    line: usize,
}

impl<'a> Lexer<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        let pos = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            3
        } else {
            0
        };
        Self {
            bytes,
            pos,
            line: 1,
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn skip_whitespace(&mut self) {
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b'\n' => {
                    self.line += 1;
                    self.pos += 1;
                }
                b'\r' | b' ' | b'\t' => {
                    self.pos += 1;
                }
                _ => break,
            }
        }
    }

    fn expect_byte(&mut self, expected: u8, desc: &str) -> Result<(), StringsParseError> {
        if self.pos < self.bytes.len() && self.bytes[self.pos] == expected {
            self.pos += 1;
            Ok(())
        } else {
            Err(StringsParseError {
                line: self.line,
                message: format!("unexpected token, expected {desc}"),
            })
        }
    }

    fn read_string(&mut self) -> Result<String, StringsParseError> {
        let start_line = self.line;
        if self.pos >= self.bytes.len() || self.bytes[self.pos] != b'"' {
            return Err(StringsParseError {
                line: start_line,
                message: "expected string literal starting with '\"'".to_string(),
            });
        }
        self.pos += 1;

        let mut buf = Vec::new();
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b == b'"' {
                self.pos += 1;
                return String::from_utf8(buf).map_err(|e| StringsParseError {
                    line: start_line,
                    message: format!("incorrect UTF-8 string literal: {e}"),
                });
            }
            if b == b'\n' {
                return Err(StringsParseError {
                    line: start_line,
                    message: "unterminated string literal".to_string(),
                });
            }
            if b == b'\\' {
                self.pos += 1;
                if self.pos >= self.bytes.len() || self.bytes[self.pos] == b'\n' {
                    return Err(StringsParseError {
                        line: start_line,
                        message: "unterminated string literal".to_string(),
                    });
                }
                match self.bytes[self.pos] {
                    b'n' => buf.push(b'\n'),
                    b't' => buf.push(b'\t'),
                    b'"' => buf.push(b'"'),
                    b'\\' => buf.push(b'\\'),
                    _ => {}
                }
                self.pos += 1;
            } else {
                buf.push(b);
                self.pos += 1;
            }
        }

        Err(StringsParseError {
            line: start_line,
            message: "unterminated string literal at EOF".to_string(),
        })
    }
}

struct ParserState {
    allow_all_plurals: bool,
    result: ParsedLangPack,
    key_index: HashMap<String, usize>,
    plural_base_index: HashMap<String, usize>,
    tag_index: HashMap<String, usize>,
}

struct ExtractedTags {
    value: String,
    tags: Vec<String>,
    numeric_entries: Vec<(String, String)>,
}

impl ParserState {
    fn new(allow_all_plurals: bool) -> Self {
        Self {
            allow_all_plurals,
            result: ParsedLangPack {
                entries: Vec::new(),
                tags: Vec::new(),
            },
            key_index: HashMap::new(),
            plural_base_index: HashMap::new(),
            tag_index: HashMap::new(),
        }
    }

    fn intern_tag(&mut self, tag: &str) -> Result<u16, StringsParseError> {
        if let Some(&idx) = self.tag_index.get(tag) {
            return Ok(idx as u16);
        }
        let idx = self.result.tags.len();
        if idx >= usize::from(u16::MAX) {
            return Err(StringsParseError {
                line: 0,
                message: "too many tags (exceeds u16::MAX)".to_string(),
            });
        }
        self.result.tags.push(tag.to_string());
        self.tag_index.insert(tag.to_string(), idx);
        Ok(idx as u16)
    }

    fn extract_tags_data(
        &mut self,
        value: &str,
        line: usize,
    ) -> Result<ExtractedTags, StringsParseError> {
        let Some(mut tag_start) = value.find('{') else {
            return Ok(ExtractedTags {
                value: value.to_string(),
                tags: Vec::new(),
                numeric_entries: Vec::new(),
            });
        };

        let mut tag_end = 0usize;
        let mut final_value = String::with_capacity(value.len() * 2);
        let mut entry_tags: Vec<String> = Vec::new();
        let mut numeric_entries: Vec<(String, String)> = Vec::new();

        loop {
            if tag_start > tag_end {
                final_value.push_str(&value[tag_end..tag_start]);
            }
            let content_start = tag_start + 1;
            let Some(rel_end) = value[content_start..].find('}') else {
                return Err(StringsParseError {
                    line,
                    message: "unexpected end of value, end of tag '}' expected".to_string(),
                });
            };
            tag_end = content_start + rel_end;
            let tag_text = &value[content_start..tag_end];
            let cmd =
                self.extract_tag_data(tag_text, &mut entry_tags, &mut numeric_entries, line)?;
            final_value.push_str(&cmd);
            tag_end += 1;

            match value[tag_end..].find('{') {
                Some(rel_next) => tag_start = tag_end + rel_next,
                None => break,
            }
        }

        if tag_end < value.len() {
            final_value.push_str(&value[tag_end..]);
        }

        Ok(ExtractedTags {
            value: final_value,
            tags: entry_tags,
            numeric_entries,
        })
    }

    fn extract_tag_data(
        &mut self,
        tag_text: &str,
        entry_tags: &mut Vec<String>,
        numeric_entries: &mut Vec<(String, String)>,
        line: usize,
    ) -> Result<String, StringsParseError> {
        let numeric_part = tag_text.find(':');
        let tag = match numeric_part {
            Some(pos) if pos > 0 => &tag_text[..pos],
            _ => tag_text,
        };
        if !validate_tag(tag) {
            return Err(StringsParseError {
                line,
                message: format!("bad tag characters: '{tag_text}'"),
            });
        }
        if entry_tags.iter().any(|t| t == tag) {
            return Err(StringsParseError {
                line,
                message: format!("duplicate found for tag '{tag_text}'"),
            });
        }
        let tag_idx = self.intern_tag(tag)?;
        let cmd = prepare_command_string(tag_idx);

        if let Some(pos) = numeric_part.filter(|&p| p > 0) {
            let parts: Vec<&str> = tag_text[pos + 1..].split('|').collect();
            if parts.len() != 3 {
                return Err(StringsParseError {
                    line,
                    message: format!("bad option count for plural key part in tag: '{tag_text}'"),
                });
            }
            for (idx, part) in parts.iter().enumerate() {
                if part.matches('#').count() > 1 {
                    return Err(StringsParseError {
                        line,
                        message: format!(
                            "bad option for plural key part in tag: '{tag_text}', too many '#'"
                        ),
                    });
                }
                numeric_entries.push((format!("{tag}{idx}"), part.replace('#', &cmd)));
            }
        }

        entry_tags.push(tag.to_string());
        Ok(cmd)
    }

    fn add_entity(
        &mut self,
        key: String,
        value: &str,
        line: usize,
    ) -> Result<(), StringsParseError> {
        let (base_key, plural_index) = if let Some((b, part)) = key.split_once('#') {
            let lower = part.to_ascii_lowercase();
            let idx = PLURAL_PARTS
                .iter()
                .position(|&p| p == lower)
                .ok_or_else(|| StringsParseError {
                    line,
                    message: format!("bad plural part for key '{key}': '{part}'"),
                })?;
            if !self.allow_all_plurals && idx != 1 && idx != 5 {
                return Err(StringsParseError {
                    line,
                    message: format!("unexpected plural part '{part}' in source key '{key}'"),
                });
            }
            (b.to_string(), Some(idx))
        } else {
            (key, None)
        };

        if let Some(&existing_idx) = self.key_index.get(&base_key) {
            let existing = &self.result.entries[existing_idx];
            if existing.key_base.is_none() || !existing.tags.is_empty() {
                return Err(StringsParseError {
                    line,
                    message: format!("duplicate found for key '{base_key}'"),
                });
            }
        }

        let ExtractedTags {
            value: parsed_value,
            tags: entry_tags,
            numeric_entries,
        } = self.extract_tags_data(value, line)?;

        if let Some(p_idx) = plural_index {
            if !numeric_entries.is_empty() {
                return Err(StringsParseError {
                    line,
                    message: format!("numeric tag options not allowed in plural key '{base_key}'"),
                });
            }
            // Ensure "count" tag exists in the global tag table even if a variant omits {count}.
            self.intern_tag(PLURAL_TAGS[0])?;

            let plural_key = compute_plural_key(&base_key, p_idx);
            if let Some(&existing_idx) = self.key_index.get(&plural_key) {
                let existing = &self.result.entries[existing_idx];
                if existing.key_base.is_none() || !existing.tags.is_empty() {
                    return Err(StringsParseError {
                        line,
                        message: format!("duplicate found for key '{plural_key}'"),
                    });
                }
            }

            let base_idx = match self.plural_base_index.get(&base_key) {
                Some(&idx) => idx,
                None => {
                    let start = self.result.entries.len();
                    for i in 0..PLURAL_PART_COUNT {
                        let pk = compute_plural_key(&base_key, i);
                        self.key_index.insert(pk.clone(), start + i);
                        self.result.entries.push(ParsedEntry {
                            key: pk,
                            value: String::new(),
                            key_base: Some(base_key.clone()),
                            tags: Vec::new(),
                        });
                    }
                    self.plural_base_index.insert(base_key.clone(), start);
                    start
                }
            };

            let real_entry = &mut self.result.entries[base_idx + p_idx];
            real_entry.value = parsed_value;
            real_entry.tags = vec![PLURAL_TAGS[0].to_string()];
            for tag in entry_tags {
                if !real_entry.tags.contains(&tag) {
                    real_entry.tags.push(tag);
                }
            }
        } else {
            for tag in &entry_tags {
                if PLURAL_TAGS.contains(&tag.as_str()) {
                    return Err(StringsParseError {
                        line,
                        message: format!("plural tag '{tag}' used in non-plural key '{base_key}'"),
                    });
                }
            }
            let idx = self.result.entries.len();
            self.key_index.insert(base_key.clone(), idx);
            self.result.entries.push(ParsedEntry {
                key: base_key.clone(),
                value: parsed_value,
                key_base: None,
                tags: entry_tags,
            });
            for (tag_key, tag_val) in numeric_entries {
                let full_key = format!("{base_key}__{tag_key}");
                let idx = self.result.entries.len();
                self.key_index.insert(full_key.clone(), idx);
                self.result.entries.push(ParsedEntry {
                    key: full_key,
                    value: tag_val,
                    key_base: None,
                    tags: Vec::new(),
                });
            }
        }

        Ok(())
    }

    fn fill_plural_tags(&mut self) -> Result<(), StringsParseError> {
        let count = self.result.entries.len();
        let mut i = 0;
        while i < count {
            if self.result.entries[i].key_base.is_none() {
                i += 1;
                continue;
            }
            if i + PLURAL_PART_COUNT > count {
                return Err(StringsParseError {
                    line: 0,
                    message: "internal error: truncated plural group".to_string(),
                });
            }
            let mut tags: Vec<String> = Vec::new();
            for entry in &self.result.entries[i..i + PLURAL_PART_COUNT] {
                if tags.is_empty() {
                    tags = entry.tags.clone();
                } else {
                    for tag in &entry.tags {
                        if !tags.contains(tag) {
                            tags.push(tag.clone());
                        }
                    }
                }
            }
            if tags.is_empty() || tags[0] != PLURAL_TAGS[0] {
                return Err(StringsParseError {
                    line: 0,
                    message: format!(
                        "internal error: plural group '{}' missing leading 'count' tag",
                        self.result.entries[i].key_base.as_deref().unwrap_or("")
                    ),
                });
            }
            for entry in &mut self.result.entries[i..i + PLURAL_PART_COUNT] {
                entry.tags = tags.clone();
            }
            i += PLURAL_PART_COUNT;
        }
        Ok(())
    }
}
