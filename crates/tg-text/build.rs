//! Build script for `tg-text`: compiles `assets/emoji.txt` and
//! `assets/emoji_autocomplete.json` into static Rust lookup tables and tries.
//!
//! Port of `Telegram/codegen/codegen/emoji/{data_read,data,replaces,generator}.cpp`.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

const K_POSTFIX: char = '\u{FE0F}';
const COLORS: [char; 5] = [
    '\u{1F3FB}',
    '\u{1F3FC}',
    '\u{1F3FD}',
    '\u{1F3FE}',
    '\u{1F3FF}',
];
const COLOR_MASK: char = COLORS[0];

const ASCII_REPLACES: &[(&[char], &str)] = &[
    (&['\u{1F60A}'], ":-)"),
    (&['\u{1F60D}'], "8-)"),
    (&['\u{2764}'], "<3"),
    (&['\u{1F61A}'], ":-*"),
    (&['\u{1F60E}'], "B-)"),
    (&['\u{1F603}'], ":-D"),
    (&['\u{1F609}'], ";-)"),
    (&['\u{1F61C}'], ";-P"),
    (&['\u{1F60B}'], ":-p"),
    (&['\u{1F614}'], "3("),
    (&['\u{1F61E}'], ":-("),
    (&['\u{1F60F}'], ":]"),
    (&['\u{1F622}'], ":'("),
    (&['\u{1F62D}'], ":_("),
    (&['\u{1F629}'], ":(("),
    (&['\u{1F610}'], ":|"),
    (&['\u{1F60C}'], "3-)"),
    (&['\u{1F620}'], ">("),
    (&['\u{1F621}'], ">(("),
    (&['\u{1F607}'], "O:)"),
    (&['\u{1F630}'], ";o"),
    (&['\u{1F633}'], "8|"),
    (&['\u{1F632}'], "8o"),
    (&['\u{1F637}'], ":X"),
    (&['\u{1F608}'], "}:)"),
];

const POSTFIX_REQUIRED: &[&[char]] = &[
    &['\u{2122}', K_POSTFIX],
    &['\u{00A9}', K_POSTFIX],
    &['\u{00AE}', K_POSTFIX],
];

type Line = Vec<String>;
type Part = Vec<Line>;
type Section = Vec<Part>;
type FileSections = Vec<Section>;

struct DoubleColored {
    original: Vec<char>,
    same: Vec<char>,
    different: Vec<char>,
}

struct InputData {
    colored: Vec<Vec<char>>,
    double_colored: Vec<DoubleColored>,
    categories: [Vec<Vec<char>>; 7],
    other: Vec<Vec<char>>,
}

struct EmojiItem {
    id: String,
    postfixed: bool,
    variated: bool,
    original: usize, // 0 = none, else 1-based index of original
}

struct EmojiData {
    list: Vec<EmojiItem>,
    map: BTreeMap<String, usize>,
    postfix_required: BTreeSet<usize>,
    categories: Vec<Vec<usize>>,
    replaces: BTreeMap<String, usize>,
}

struct DoubleColorTemplates {
    same: Vec<char>,
    different: Vec<char>,
}

#[derive(Clone)]
struct ReplaceSuggestion {
    id: String,
    replacement: String,
    words: Vec<String>,
}

fn bare_id(chars: &[char]) -> String {
    chars.iter().copied().filter(|&c| c != K_POSTFIX).collect()
}

fn strip_postfix(s: &str) -> String {
    s.chars().filter(|&c| c != K_POSTFIX).collect()
}

fn parse_emoji_file(content: &str) -> FileSections {
    let normalized = content.replace("\r\n", "\n");
    let mut sections = Vec::new();
    let mut current_section = String::new();

    for line in normalized.lines() {
        if line.contains("--------") || line.contains("========") {
            if let Some(sec) = parse_section(&current_section) {
                sections.push(sec);
            }
            current_section.clear();
        } else {
            current_section.push_str(line);
            current_section.push('\n');
        }
    }
    if let Some(sec) = parse_section(&current_section) {
        sections.push(sec);
    }
    sections
}

fn parse_section(text: &str) -> Option<Section> {
    let mut parts = Vec::new();
    for raw_part in text.split("\n\n") {
        let mut lines = Vec::new();
        for raw_line in raw_part.lines() {
            let mut items = Vec::new();
            for piece in raw_line.split(',') {
                if let Some(start) = piece.find('"') {
                    let rest = &piece[start + 1..];
                    if let Some(end) = rest.find('"') {
                        let s = &rest[..end];
                        if !s.is_empty() {
                            items.push(s.to_string());
                        }
                    }
                }
            }
            if !items.is_empty() {
                lines.push(items);
            }
        }
        if !lines.is_empty() {
            parts.push(lines);
        }
    }
    if parts.is_empty() { None } else { Some(parts) }
}

fn find_colored_line<'a>(file: &'a FileSections, target: &str) -> &'a Line {
    for part in &file[0] {
        for line in part {
            if !line.is_empty() && line[0] == target {
                return line;
            }
        }
    }
    panic!("Colored emoji not found in section 0: {target}");
}

fn read_input_data(path: &Path) -> InputData {
    let content = fs::read_to_string(path).expect("read assets/emoji.txt");
    let file = parse_emoji_file(&content);
    assert!(
        file.len() >= 3 && file[0].len() == 8 && file[1].len() <= 8,
        "unexpected emoji.txt structure: sections={}, s0={}, s1={}",
        file.len(),
        file[0].len(),
        file[1].len()
    );

    let mut colored = Vec::new();
    for line in &file[2][0] {
        for item in line {
            let found = find_colored_line(&file, item);
            assert_eq!(
                found.len(),
                6,
                "expected 6 entries for colored emoji {item}"
            );
            colored.push(found[1].chars().collect());
        }
    }

    let mut double_colored = Vec::new();
    if file[2].len() > 1 {
        for line in &file[2][1] {
            for item in line {
                let found = find_colored_line(&file, item);
                assert_eq!(
                    found.len(),
                    26,
                    "expected 26 entries for double-colored emoji {item}"
                );
                double_colored.push(DoubleColored {
                    original: found[0].chars().collect(),
                    same: found[1].chars().collect(),
                    different: found[2].chars().collect(),
                });
            }
        }
    }

    let mut categories: [Vec<Vec<char>>; 7] = Default::default();
    let mut index = 0usize;
    let mut replacements_used = 0usize;
    for section in &file[0] {
        let first = &section[0][0];
        let replacement = file[1].iter().find(|s| &s[0][0] == first);
        if replacement.is_some() {
            replacements_used += 1;
        }
        let chosen = replacement.unwrap_or(section);
        for line in chosen {
            for s in line {
                categories[index].push(s.chars().collect());
            }
        }
        if index + 1 < categories.len() {
            index += 1;
        }
    }
    assert_eq!(
        replacements_used,
        file[1].len(),
        "unused section replacements in emoji.txt"
    );

    let mut other = Vec::new();
    if file.len() > 3 {
        for part in &file[3] {
            for line in part {
                for s in line {
                    other.push(s.chars().collect());
                }
            }
        }
    }

    InputData {
        colored,
        double_colored,
        categories,
        other,
    }
}

fn prepare_data(input: &InputData) -> EmojiData {
    let mut aliases: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut add_alias = |orig: String, aliased: String| {
        let entry = aliases.entry(orig).or_default();
        if !entry.contains(&aliased) {
            entry.push(aliased);
        }
    };

    // FlagAliases: CP -> FR
    add_alias(
        ['\u{1F1EB}', '\u{1F1F7}'].into_iter().collect(),
        ['\u{1F1E8}', '\u{1F1F5}'].into_iter().collect(),
    );

    // FillVariatedIds
    let mut variated_ids = BTreeSet::new();
    for row in &input.colored {
        assert!(row.len() >= 2 && row[1] == COLOR_MASK);
        let id: String = row
            .iter()
            .enumerate()
            .filter(|&(i, &c)| i != 1 && c != K_POSTFIX)
            .map(|(_, &c)| c)
            .collect();
        variated_ids.insert(id);
    }

    // FillDoubleVariatedIds
    let mut double_variated_ids: BTreeMap<String, DoubleColorTemplates> = BTreeMap::new();
    for dc in &input.double_colored {
        let color_count = dc.same.iter().filter(|&&c| c == COLOR_MASK).count();
        let variated_id = if color_count == 1 {
            for &color in &COLORS {
                let mut copy = dc.same.clone();
                copy[1] = color;
                let same_with_color = bare_id(&copy);
                let diff_with_color: String = dc
                    .different
                    .iter()
                    .copied()
                    .filter(|&c| c != K_POSTFIX)
                    .map(|c| {
                        if c == COLORS[0] || c == COLORS[1] {
                            color
                        } else {
                            c
                        }
                    })
                    .collect();
                add_alias(same_with_color, diff_with_color);
            }
            bare_id(&dc.original)
        } else {
            if dc.original.len() == 1 {
                for &color in &COLORS {
                    let mut orig_c = dc.original.clone();
                    orig_c.push(color);
                    let orig_with_color = bare_id(&orig_c);
                    let same_with_color: String = dc
                        .same
                        .iter()
                        .copied()
                        .filter(|&c| c != K_POSTFIX)
                        .map(|c| if c == COLOR_MASK { color } else { c })
                        .collect();
                    add_alias(orig_with_color, same_with_color);
                }
            }
            bare_id(&dc.original)
        };
        let same_template = if dc.original.len() == 1 {
            vec![dc.original[0], COLOR_MASK]
        } else {
            dc.same.clone()
        };
        double_variated_ids.insert(
            variated_id,
            DoubleColorTemplates {
                same: same_template,
                different: dc.different.clone(),
            },
        );
    }

    let postfix_required_ids: BTreeSet<String> =
        POSTFIX_REQUIRED.iter().map(|r| bare_id(r)).collect();

    let mut data = EmojiData {
        list: Vec::new(),
        map: BTreeMap::new(),
        postfix_required: BTreeSet::new(),
        categories: Vec::new(),
        replaces: BTreeMap::new(),
    };
    let mut aliases_added = BTreeSet::new();

    let mut append_category = |category: &[Vec<char>]| {
        let mut cat_indices = Vec::with_capacity(category.len());
        for id_chars in category {
            let bare = bare_id(id_chars);
            let mut postfixed = false;
            let mut end = id_chars.len();
            if end == 2 && id_chars[1] == K_POSTFIX {
                postfixed = true;
                end = 1;
            }
            let emoji_str: String = id_chars[..end].iter().copied().collect();

            let add_one = |data: &mut EmojiData,
                           aliases_added: &mut BTreeSet<String>,
                           bare: &str,
                           item: EmojiItem|
             -> usize {
                if let Some(&idx) = data.map.get(bare) {
                    assert_eq!(data.list[idx].postfixed, item.postfixed);
                    assert_eq!(data.list[idx].id, item.id);
                    idx
                } else {
                    let idx = data.list.len();
                    data.map.insert(bare.to_string(), idx);
                    data.list.push(item);
                    if let Some(alias_list) = aliases.get(bare) {
                        aliases_added.insert(bare.to_string());
                        for alias in alias_list {
                            let prev = data.map.insert(alias.clone(), idx);
                            assert!(prev.is_none(), "duplicate alias: {alias}");
                        }
                    }
                    if postfix_required_ids.contains(bare) {
                        data.postfix_required.insert(idx);
                    }
                    idx
                }
            };

            let idx = add_one(
                &mut data,
                &mut aliases_added,
                &bare,
                EmojiItem {
                    id: emoji_str,
                    postfixed,
                    variated: false,
                    original: 0,
                },
            );

            if variated_ids.contains(&bare) {
                data.list[idx].variated = true;
                let base_char = id_chars[0];
                let mut from = 1usize;
                let slice = &id_chars[from..end];
                if slice.iter().filter(|&&c| c == K_POSTFIX).count() == 2
                    && id_chars[from] == K_POSTFIX
                {
                    from += 1;
                }
                for &color in &COLORS {
                    let mut colored_id = String::new();
                    colored_id.push(base_char);
                    colored_id.push(color);
                    let mut bare_colored = colored_id.clone();
                    for &c in &id_chars[from..end] {
                        colored_id.push(c);
                        if c != K_POSTFIX {
                            bare_colored.push(c);
                        }
                    }
                    add_one(
                        &mut data,
                        &mut aliases_added,
                        &bare_colored,
                        EmojiItem {
                            id: colored_id,
                            postfixed: false,
                            variated: false,
                            original: idx + 1,
                        },
                    );
                }
            } else if let Some(tpl) = double_variated_ids.get(&bare) {
                for &c1 in &COLORS {
                    for &c2 in &COLORS {
                        let pattern = if c1 == c2 { &tpl.same } else { &tpl.different };
                        let p_postfixed = pattern.last() == Some(&K_POSTFIX);
                        let colored_id: String = pattern
                            .iter()
                            .copied()
                            .filter(|&c| c != K_POSTFIX)
                            .map(|c| {
                                if c == COLORS[0] {
                                    c1
                                } else if c == COLORS[1] {
                                    c2
                                } else {
                                    c
                                }
                            })
                            .collect();
                        let bare_colored = colored_id.clone();
                        add_one(
                            &mut data,
                            &mut aliases_added,
                            &bare_colored,
                            EmojiItem {
                                id: colored_id,
                                postfixed: p_postfixed,
                                variated: false,
                                original: 0,
                            },
                        );
                    }
                }
            }

            cat_indices.push(idx);
        }
        data.categories.push(cat_indices);
    };

    for cat in &input.categories {
        append_category(cat);
    }
    append_category(&input.other);

    assert_eq!(
        aliases_added.len(),
        aliases.len(),
        "not all emoji aliases were attached"
    );

    // Fill ASCII emoticon replaces
    for &(codes, rep) in ASCII_REPLACES {
        let key: String = codes.iter().copied().collect();
        let &idx = data
            .map
            .get(&key)
            .unwrap_or_else(|| panic!("replace emoji not found for {rep}"));
        data.replaces.insert(rep.to_string(), idx);
    }

    data
}

// --- Minimal JSON parser for assets/emoji_autocomplete.json ---

struct JsonScanner<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> JsonScanner<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            bytes: s.as_bytes(),
            pos: 0,
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.bytes.get(self.pos).copied()
    }

    fn expect(&mut self, b: u8) {
        self.skip_ws();
        assert_eq!(
            self.bytes.get(self.pos).copied(),
            Some(b),
            "JSON parse error at byte {}: expected '{}'",
            self.pos,
            b as char
        );
        self.pos += 1;
    }

    fn parse_string(&mut self) -> String {
        self.expect(b'"');
        let mut out = String::new();
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            self.pos += 1;
            if b == b'"' {
                return out;
            }
            if b == b'\\' {
                let esc = self.bytes[self.pos];
                self.pos += 1;
                match esc {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{0008}'),
                    b'f' => out.push('\u{000C}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let hex = std::str::from_utf8(&self.bytes[self.pos..self.pos + 4]).unwrap();
                        self.pos += 4;
                        let code = u32::from_str_radix(hex, 16).unwrap();
                        out.push(char::from_u32(code).unwrap());
                    }
                    other => panic!("unexpected JSON escape: \\{}", other as char),
                }
            } else if b < 0x80 {
                out.push(b as char);
            } else {
                // Multi-byte UTF-8 sequence
                let start = self.pos - 1;
                let width = if b & 0xE0 == 0xC0 {
                    2
                } else if b & 0xF0 == 0xE0 {
                    3
                } else {
                    4
                };
                self.pos = start + width;
                let s = std::str::from_utf8(&self.bytes[start..self.pos]).unwrap();
                out.push_str(s);
            }
        }
        panic!("unterminated JSON string");
    }

    fn parse_object_of_strings(&mut self) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        self.expect(b'{');
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return map;
        }
        loop {
            let k = self.parse_string();
            self.expect(b':');
            let v = self.parse_string();
            map.insert(k, v);
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                other => panic!("unexpected JSON token in object: {other:?}"),
            }
        }
        map
    }

    fn parse_root(&mut self) -> Vec<(String, BTreeMap<String, String>)> {
        let mut entries = Vec::new();
        self.expect(b'{');
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return entries;
        }
        loop {
            let key = self.parse_string();
            self.expect(b':');
            let obj = self.parse_object_of_strings();
            entries.push((key, obj));
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                other => panic!("unexpected JSON token at root: {other:?}"),
            }
        }
        entries
    }
}

fn replacement_words(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut word = String::new();
    let feed = |word: &mut String, counts: &mut BTreeMap<String, usize>| {
        if !word.is_empty() {
            *counts.entry(std::mem::take(word)).or_insert(0) += 1;
        }
    };
    for i in 0..chars.len() {
        let ch = chars[i];
        if ch.is_alphanumeric()
            || ((ch == '-' || ch == '+') && i + 1 < chars.len() && chars[i + 1].is_numeric())
        {
            word.push(ch);
            continue;
        }
        feed(&mut word, &mut counts);
    }
    feed(&mut word, &mut counts);
    let mut result = Vec::new();
    for (w, count) in counts {
        for _ in 0..count {
            result.push(w.clone());
        }
    }
    result
}

fn has_tone_suffix(replacement: &str) -> bool {
    let bytes = replacement.as_bytes();
    for w in bytes.windows(6) {
        if &w[..5] == b"_tone" && w[5].is_ascii_digit() {
            return true;
        }
    }
    false
}

fn prepare_suggestions(path: &Path, data: &EmojiData) -> Vec<ReplaceSuggestion> {
    let content = fs::read_to_string(path).expect("read assets/emoji_autocomplete.json");
    let mut scanner = JsonScanner::new(&content);
    let raw_entries = scanner.parse_root();

    // NotSupported set (from replaces.cpp)
    let mut not_supported: BTreeSet<String> = BTreeSet::new();
    not_supported.insert("\u{0023}\u{FE0F}".to_string());
    not_supported.insert("\u{002A}\u{FE0F}".to_string());
    for i in 0..10u32 {
        not_supported.insert(format!("{}\u{FE0F}", char::from_u32(0x30 + i).unwrap()));
    }
    for &c in &COLORS {
        not_supported.insert(c.to_string());
    }
    for i in 0..26u32 {
        not_supported.insert(char::from_u32(0x1F1E6 + i).unwrap().to_string());
    }
    not_supported.insert("\u{2640}\u{FE0F}".to_string());
    not_supported.insert("\u{2642}\u{FE0F}".to_string());
    not_supported.insert("\u{2695}\u{FE0F}".to_string());

    // ConvertMap (from replaces.cpp)
    let male_mod = "\u{200D}\u{2642}\u{FE0F}";
    let female_mod = "\u{200D}\u{2640}\u{FE0F}";
    let mut convert_map: BTreeMap<String, String> = BTreeMap::new();
    for &(ch, modifier) in &[
        ('\u{1F926}', male_mod),
        ('\u{1F937}', female_mod),
        ('\u{1F938}', male_mod),
        ('\u{1F939}', male_mod),
        ('\u{1F93C}', male_mod),
        ('\u{1F93D}', male_mod),
        ('\u{1F93E}', female_mod),
    ] {
        let s = ch.to_string();
        convert_map.insert(s.clone(), format!("{s}{modifier}"));
    }
    convert_map.insert(
        "\u{1F469}\u{200D}\u{2764}\u{FE0F}\u{200D}\u{1F48B}\u{200D}\u{1F468}".to_string(),
        "\u{1F48F}".to_string(),
    );
    convert_map.insert(
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F466}".to_string(),
        "\u{1F46A}".to_string(),
    );
    convert_map.insert(
        "\u{1F469}\u{200D}\u{2764}\u{FE0F}\u{200D}\u{1F468}".to_string(),
        "\u{1F491}".to_string(),
    );
    let flag_pair = |a: char, b: char| -> String {
        let c1 = char::from_u32(0x1F1E6 + (a as u32 - 'a' as u32)).unwrap();
        let c2 = char::from_u32(0x1F1E6 + (b as u32 - 'a' as u32)).unwrap();
        format!("{c1}{c2}")
    };
    for &(a, b, c, d) in &[
        ('a', 'c', 's', 'h'),
        ('b', 'v', 'n', 'o'),
        ('c', 'p', 'f', 'r'),
        ('d', 'g', 'i', 'o'),
        ('e', 'a', 'e', 's'),
        ('h', 'm', 'a', 'u'),
        ('m', 'f', 'f', 'r'),
        ('s', 'j', 'n', 'o'),
        ('t', 'a', 's', 'h'),
        ('u', 'm', 'u', 's'),
    ] {
        convert_map.insert(flag_pair(a, b), flag_pair(c, d));
    }

    let mut list = Vec::new();
    let mut add = |id: &str, rep: &str| {
        let words = replacement_words(rep);
        assert!(!words.is_empty(), "replacement {rep} has no words");
        list.push(ReplaceSuggestion {
            id: id.to_string(),
            replacement: rep.to_string(),
            words,
        });
    };

    for (_key, obj) in raw_entries {
        let output = &obj["output"];
        let replacement = &obj["alpha_code"];
        let aliases_raw = &obj["aliases"];

        let mut id = String::new();
        for part in output.split('-') {
            let code = u32::from_str_radix(part, 16).expect("valid hex output part");
            id.push(char::from_u32(code).expect("valid scalar"));
        }

        if has_tone_suffix(replacement) || not_supported.contains(&id) {
            continue;
        }
        let id = convert_map.get(&id).cloned().unwrap_or(id);

        add(&id, replacement);
        if !aliases_raw.is_empty() {
            for alias in aliases_raw.split('|') {
                if !alias.is_empty() && alias != ":shrug:" {
                    add(&id, alias);
                }
            }
        }
    }

    add("\u{1F44D}", ":like:");
    add("\u{1F44E}", ":dislike:");
    add("\u{1F914}", ":hmm:");
    add("\u{1F973}", ":party:");

    // CheckAndConvertReplaces
    let mut by_emoji: BTreeMap<String, Vec<ReplaceSuggestion>> = BTreeMap::new();
    let canonical = |id: &str| -> Option<String> {
        let idx = if let Some(&i) = data.map.get(id) {
            i
        } else {
            let stripped = strip_postfix(id);
            *data.map.get(&stripped)?
        };
        let item = &data.list[idx];
        Some(if item.postfixed {
            format!("{}{}", item.id, K_POSTFIX)
        } else {
            item.id.clone()
        })
    };

    for mut rep in list {
        let resolved = canonical(&rep.id)
            .or_else(|| {
                rep.id
                    .strip_suffix(male_mod)
                    .and_then(canonical)
                    .or_else(|| rep.id.strip_suffix(female_mod).and_then(canonical))
            })
            .or_else(|| {
                let with_male = format!("{}{}", rep.id, male_mod);
                let with_female = format!("{}{}", rep.id, female_mod);
                canonical(&with_male).or_else(|| canonical(&with_female))
            });
        let canon = resolved.unwrap_or_else(|| {
            panic!(
                "suggestion replacement '{}' ({:?}) not found in emoji map",
                rep.replacement, rep.id
            )
        });
        rep.id = canon.clone();
        // Match QMultiMap insertion order (front-insert per key so latest is first,
        // or preserve order — in C++ QMultiMap::insert prepends to the key's list).
        by_emoji.entry(canon).or_default().insert(0, rep);
    }

    let mut ordered = Vec::new();
    for category in &data.categories {
        for &idx in category {
            let item = &data.list[idx];
            let key = if item.postfixed {
                format!("{}{}", item.id, K_POSTFIX)
            } else {
                item.id.clone()
            };
            if let Some(entries) = by_emoji.remove(&key) {
                ordered.extend(entries);
            }
        }
    }
    assert!(
        by_emoji.is_empty(),
        "unconsumed suggestion emojis: {}",
        by_emoji.len()
    );
    ordered
}

// --- Flat UTF-16 trie builder ---

struct BuildTrieNode {
    children: BTreeMap<u16, usize>,
    emoji_index: u16, // 0 = none, else 1-based
    postfix_required: bool,
}

struct FlatTrieNode {
    unit: u16,
    children_start: u32,
    children_len: u16,
    emoji_index: u16,
    postfix_required: bool,
}

fn build_flat_trie(
    entries: &BTreeMap<String, usize>,
    postfix_required: &BTreeSet<usize>,
) -> Vec<FlatTrieNode> {
    let mut nodes = vec![BuildTrieNode {
        children: BTreeMap::new(),
        emoji_index: 0,
        postfix_required: false,
    }];

    for (key, &idx) in entries {
        let mut cur = 0usize;
        for unit in key.encode_utf16() {
            let next = if let Some(&child) = nodes[cur].children.get(&unit) {
                child
            } else {
                let new_idx = nodes.len();
                nodes.push(BuildTrieNode {
                    children: BTreeMap::new(),
                    emoji_index: 0,
                    postfix_required: false,
                });
                nodes[cur].children.insert(unit, new_idx);
                new_idx
            };
            cur = next;
        }
        nodes[cur].emoji_index = (idx + 1) as u16;
        nodes[cur].postfix_required = postfix_required.contains(&idx);
    }

    // Flatten in BFS order so each node's children are contiguous and sorted by `unit`.
    let mut flat = Vec::with_capacity(nodes.len());
    let mut queue = std::collections::VecDeque::new();
    flat.push(FlatTrieNode {
        unit: 0,
        children_start: 0,
        children_len: 0,
        emoji_index: nodes[0].emoji_index,
        postfix_required: nodes[0].postfix_required,
    });
    queue.push_back((0usize, 0usize)); // (build_idx, flat_idx)

    while let Some((b_idx, f_idx)) = queue.pop_front() {
        let children_start = flat.len() as u32;
        let children_len = nodes[b_idx].children.len() as u16;
        flat[f_idx].children_start = children_start;
        flat[f_idx].children_len = children_len;
        for (&unit, &child_b_idx) in &nodes[b_idx].children {
            let child_f_idx = flat.len();
            flat.push(FlatTrieNode {
                unit,
                children_start: 0,
                children_len: 0,
                emoji_index: nodes[child_b_idx].emoji_index,
                postfix_required: nodes[child_b_idx].postfix_required,
            });
            queue.push_back((child_b_idx, child_f_idx));
        }
    }

    flat
}

fn escape_rust_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c if c.is_ascii_graphic() || c == ' ' => out.push(c),
            c => {
                let _ = write!(out, "\\u{{{:X}}}", c as u32);
            }
        }
    }
    out.push('"');
    out
}

fn main() {
    println!("cargo:rerun-if-changed=assets/emoji.txt");
    println!("cargo:rerun-if-changed=assets/emoji_autocomplete.json");
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let assets_dir = Path::new(&manifest_dir).join("assets");

    let input = read_input_data(&assets_dir.join("emoji.txt"));
    let data = prepare_data(&input);
    let suggestions = prepare_suggestions(&assets_dir.join("emoji_autocomplete.json"), &data);

    let emoji_trie = build_flat_trie(&data.map, &data.postfix_required);
    let empty_set = BTreeSet::new();
    let replace_trie = build_flat_trie(&data.replaces, &empty_set);

    let out_dir = env::var("OUT_DIR").unwrap();
    let dest = Path::new(&out_dir).join("emoji_data.rs");
    let mut code = String::with_capacity(512 * 1024);

    // 1. Emoji items
    code.push_str("pub(crate) const EMOJI_ITEMS: &[EmojiEntry] = &[\n");
    for (i, item) in data.list.iter().enumerate() {
        let _ = writeln!(
            code,
            "    EmojiEntry {{ id: {}, original: {}, index: {}, has_postfix: {}, colorizable: {} }},",
            escape_rust_str(&item.id),
            item.original,
            i,
            item.postfixed,
            item.variated
        );
    }
    code.push_str("];\n\n");

    // 2. Sections (7 UI categories: People, Nature, Food, Activity, Travel, Objects, Symbols)
    code.push_str("pub(crate) const SECTIONS: [&[u16]; 7] = [\n");
    for cat in data.categories.iter().take(7) {
        code.push_str("    &[");
        for (i, &idx) in cat.iter().enumerate() {
            if i > 0 {
                code.push(',');
            }
            let _ = write!(code, "{idx}");
        }
        code.push_str("],\n");
    }
    code.push_str("];\n\n");

    // 3. Emoji lookup trie
    code.push_str("pub(crate) const EMOJI_TRIE: &[TrieNode] = &[\n");
    for n in &emoji_trie {
        let _ = writeln!(
            code,
            "    TrieNode {{ unit: 0x{:04X}, children_start: {}, children_len: {}, emoji_index: {}, postfix_required: {} }},",
            n.unit, n.children_start, n.children_len, n.emoji_index, n.postfix_required
        );
    }
    code.push_str("];\n\n");

    // 4. ASCII emoticon replacement pairs & trie
    code.push_str("pub(crate) const REPLACEMENT_PAIRS: &[(&str, u16)] = &[\n");
    for (what, &idx) in &data.replaces {
        let _ = writeln!(code, "    ({}, {}),", escape_rust_str(what), idx);
    }
    code.push_str("];\n\n");

    code.push_str("pub(crate) const REPLACE_TRIE: &[TrieNode] = &[\n");
    for n in &replace_trie {
        let _ = writeln!(
            code,
            "    TrieNode {{ unit: 0x{:04X}, children_start: {}, children_len: {}, emoji_index: {}, postfix_required: false }},",
            n.unit, n.children_start, n.children_len, n.emoji_index
        );
    }
    code.push_str("];\n\n");

    // 5. Emoji autocomplete suggestions
    let max_len = suggestions
        .iter()
        .map(|s| s.replacement.encode_utf16().count())
        .max()
        .unwrap_or(0);
    let _ = writeln!(
        code,
        "pub(crate) const SUGGESTION_MAX_LENGTH: usize = {max_len};\n"
    );

    code.push_str("pub(crate) const SUGGESTIONS: &[SuggestionEntry] = &[\n");
    let mut by_first_char: BTreeMap<char, Vec<u16>> = BTreeMap::new();
    for (i, s) in suggestions.iter().enumerate() {
        code.push_str("    SuggestionEntry { emoji: ");
        code.push_str(&escape_rust_str(&s.id));
        code.push_str(", replacement: ");
        code.push_str(&escape_rust_str(&s.replacement));
        code.push_str(", words: &[");
        for (wi, w) in s.words.iter().enumerate() {
            if wi > 0 {
                code.push_str(", ");
            }
            code.push_str(&escape_rust_str(w));
            let first = w.chars().next().unwrap();
            let entry = by_first_char.entry(first).or_default();
            if entry.last().copied() != Some(i as u16) {
                entry.push(i as u16);
            }
        }
        code.push_str("] },\n");
    }
    code.push_str("];\n\n");

    code.push_str("pub(crate) const SUGGESTION_BY_FIRST: &[(char, &[u16])] = &[\n");
    for (ch, indices) in &by_first_char {
        let _ = write!(code, "    ({ch:?}, &[");
        for (i, &idx) in indices.iter().enumerate() {
            if i > 0 {
                code.push(',');
            }
            let _ = write!(code, "{idx}");
        }
        code.push_str("]),\n");
    }
    code.push_str("];\n\n");

    // Sorted by replacement string for O(log n) lookup in get_suggestion_emoji
    let mut by_replacement: Vec<(&str, u16)> = suggestions
        .iter()
        .enumerate()
        .map(|(i, s)| (s.replacement.as_str(), i as u16))
        .collect();
    by_replacement.sort_by_key(|&(rep, _)| rep);
    code.push_str("pub(crate) const SUGGESTION_BY_REPLACEMENT: &[(&str, u16)] = &[\n");
    for (rep, idx) in by_replacement {
        let _ = writeln!(code, "    ({}, {}),", escape_rust_str(rep), idx);
    }
    code.push_str("];\n");

    fs::write(&dest, code).expect("write generated emoji_data.rs");
}
