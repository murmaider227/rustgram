//! Rich text constructors, string transforms, song name formatting, value formatting,
//! and text parse option presets.
//!
//! Port of `Telegram/lib_ui/ui/text/text_utilities.cpp/.h`,
//! `Telegram/SourceFiles/ui/text/format_song_name.cpp/.h`,
//! `Telegram/SourceFiles/ui/text/format_values.cpp/.h` (pure formatting logic), and
//! `Telegram/SourceFiles/ui/text/text_options.cpp/.h`.

use std::sync::OnceLock;

use regex::Regex;

use crate::entity::{EntityInText, EntityType, TextParseFlags, TextWithEntities};
use crate::utf16::{slice_utf16, utf16_len};

// --- Rich text builders (text_utilities.cpp) ---

fn with_single_entity(text: &str, entity_type: EntityType, data: &str) -> TextWithEntities {
    let len = utf16_len(text) as i32;
    TextWithEntities {
        text: text.to_string(),
        entities: vec![EntityInText::with_data(entity_type, 0, len, data)],
    }
}

fn colorized_entity_data(index: u16, background_index: u16) -> String {
    if index == 0 && background_index == 0 {
        return String::new();
    }
    let mut out = String::new();
    if let Some(c) = char::from_u32(u32::from(index)) {
        out.push(c);
    }
    if background_index != 0
        && let Some(c) = char::from_u32(u32::from(background_index))
    {
        out.push(c);
    }
    out
}

/// Wraps `text` in a `Bold` entity (`Ui::Text::Bold`).
pub fn bold(text: &str) -> TextWithEntities {
    with_single_entity(text, EntityType::Bold, "")
}

/// Wraps `text` in a `Semibold` entity (`Ui::Text::Semibold`).
pub fn semibold(text: &str) -> TextWithEntities {
    with_single_entity(text, EntityType::Semibold, "")
}

/// Wraps `text` in an `Italic` entity (`Ui::Text::Italic`).
pub fn italic(text: &str) -> TextWithEntities {
    with_single_entity(text, EntityType::Italic, "")
}

/// Wraps `text` in an `Underline` entity (`Ui::Text::Underline`).
pub fn underline(text: &str) -> TextWithEntities {
    with_single_entity(text, EntityType::Underline, "")
}

/// Wraps `text` in a `StrikeOut` entity (`Ui::Text::StrikeOut`).
pub fn strike_out(text: &str) -> TextWithEntities {
    with_single_entity(text, EntityType::StrikeOut, "")
}

/// Wraps `text` in a `CustomUrl` entity (`Ui::Text::Link(QString, QString)`).
pub fn link(text: &str, url: &str) -> TextWithEntities {
    with_single_entity(text, EntityType::CustomUrl, url)
}

/// Wraps `text` in an indexed internal link `"internal:index<ch>"` (`Ui::Text::Link(QString, int)`).
pub fn link_with_index(text: &str, index: u16) -> TextWithEntities {
    let mut url = String::from("internal:index");
    if let Some(ch) = char::from_u32(u32::from(index)) {
        url.push(ch);
    }
    link(text, &url)
}

/// Wraps an existing `TextWithEntities` in a `CustomUrl` entity (`Ui::Text::Link(TextWithEntities, QString)`).
pub fn link_marked(text: TextWithEntities, url: &str) -> TextWithEntities {
    wrapped(text, EntityType::CustomUrl, url)
}

/// Wraps an existing `TextWithEntities` in an indexed internal link (`Ui::Text::Link(TextWithEntities, int)`).
pub fn link_marked_with_index(text: TextWithEntities, index: u16) -> TextWithEntities {
    let mut url = String::from("internal:index");
    if let Some(ch) = char::from_u32(u32::from(index)) {
        url.push(ch);
    }
    link_marked(text, &url)
}

/// Wraps `text` in a `Colorized` entity (`Ui::Text::Colorized(QString, int, int)`).
pub fn colorized(text: &str, index: u16, background_index: u16) -> TextWithEntities {
    let data = colorized_entity_data(index, background_index);
    with_single_entity(text, EntityType::Colorized, &data)
}

/// Wraps an existing `TextWithEntities` in a `Colorized` entity (`Ui::Text::Colorized(TextWithEntities, int, int)`).
pub fn colorized_marked(
    text: TextWithEntities,
    index: u16,
    background_index: u16,
) -> TextWithEntities {
    let data = colorized_entity_data(index, background_index);
    wrapped(text, EntityType::Colorized, &data)
}

/// Prepends an outer entity spanning the entire `text` (`Ui::Text::Wrapped`).
pub fn wrapped(
    mut text: TextWithEntities,
    entity_type: EntityType,
    data: &str,
) -> TextWithEntities {
    let len = text.utf16_len() as i32;
    text.entities
        .insert(0, EntityInText::with_data(entity_type, 0, len, data));
    text
}

/// Creates a plain `TextWithEntities` with no entities (`Ui::Text::WithEntities`).
#[inline]
pub fn with_entities(text: &str) -> TextWithEntities {
    TextWithEntities::simple(text)
}

/// Parses lightweight `**bold**` and `__italic__` markup used in lang strings
/// (`Ui::Text::RichLangValue`).
pub fn rich_lang_value(text: &str) -> TextWithEntities {
    static RE_START: OnceLock<Regex> = OnceLock::new();
    let re = RE_START.get_or_init(|| Regex::new(r"(\*\*|__)").expect("valid rich lang regex"));

    let mut result = TextWithEntities::new();
    let mut offset = 0usize;
    while offset < text.len() {
        let Some(m) = re.find_at(text, offset) else {
            result.text.push_str(&text[offset..]);
            break;
        };
        let position = m.start();
        let from = m.end();
        let tag = m.as_str();
        let search_after = from + text[from..].chars().next().map_or(0, char::len_utf8);
        let till = if search_after <= text.len() {
            text[search_after..].find(tag).map(|rel| search_after + rel)
        } else {
            None
        };
        let Some(till) = till else {
            offset = from;
            continue;
        };
        if position > offset {
            result.text.push_str(&text[offset..position]);
        }
        let entity_type = if tag == "__" {
            EntityType::Italic
        } else {
            EntityType::Bold
        };
        let inner = &text[from..till];
        let out_offset = utf16_len(&result.text) as i32;
        let inner_u16_len = utf16_len(inner) as i32;
        result
            .entities
            .push(EntityInText::new(entity_type, out_offset, inner_u16_len));
        result.text.push_str(inner);
        offset = till + tag.len();
    }
    result
}

/// Creates a `TextWithEntities` containing a single custom emoji (`Ui::Text::SingleCustomEmoji`).
pub fn single_custom_emoji(data: impl Into<String>, text: &str) -> TextWithEntities {
    let fallback = if text.is_empty() { "@" } else { text };
    let len = utf16_len(fallback) as i32;
    TextWithEntities {
        text: fallback.to_string(),
        entities: vec![EntityInText::with_data(
            EntityType::CustomEmoji,
            0,
            len,
            data,
        )],
    }
}

/// Extracts a UTF-16 subrange `[position, position + n)` of `text`, clipping intersecting
/// entities to the subrange (`Ui::Text::Mid`).
pub fn mid(text: &TextWithEntities, position: usize, n: Option<usize>) -> TextWithEntities {
    let total_u16 = text.utf16_len();
    let start_pos = position.min(total_u16);
    let len = match n {
        Some(len) => len.min(total_u16 - start_pos),
        None => total_u16 - start_pos,
    };
    let mid_end = start_pos + len;
    let start_i32 = start_pos as i32;
    let end_i32 = mid_end as i32;

    let entities = text
        .entities
        .iter()
        .filter(|e| {
            let l1 = e.offset();
            let r1 = e.offset() + e.length() - 1;
            let l2 = start_i32;
            let r2 = end_i32 - 1;
            !(l1 > r2 || l2 > r1)
        })
        .map(|e| {
            let s = e.offset().max(start_i32);
            let end = (e.offset() + e.length()).min(end_i32);
            EntityInText::with_data(e.entity_type(), s - start_i32, end - s, e.data())
        })
        .collect();

    TextWithEntities {
        text: slice_utf16(&text.text, start_pos, len)
            .unwrap_or("")
            .to_string(),
        entities,
    }
}

/// Returns a copy of `text` keeping only entities whose type is in `types` (`Ui::Text::Filtered`).
pub fn filtered(text: &TextWithEntities, types: &[EntityType]) -> TextWithEntities {
    TextWithEntities {
        text: text.text.clone(),
        entities: text
            .entities
            .iter()
            .filter(|e| types.contains(&e.entity_type()))
            .cloned()
            .collect(),
    }
}

/// Escapes `&` as `&&` for menu action labels (`Ui::Text::FixAmpersandInAction`).
pub fn fix_ampersand_in_action(text: &str) -> String {
    text.replace('&', "&&")
}

/// Wraps the masked `*...*` portion of an email pattern in a `Spoiler` entity
/// (`Ui::Text::WrapEmailPattern`).
pub fn wrap_email_pattern(pattern: &str) -> TextWithEntities {
    if let (Some(from_b), Some(to_b)) = (pattern.find('*'), pattern.rfind('*')) {
        let from_u16 = utf16_len(&pattern[..from_b]) as i32;
        let to_u16 = utf16_len(&pattern[..to_b]) as i32;
        let length = to_u16 - from_u16 + 1;
        return TextWithEntities {
            text: pattern.to_string(),
            entities: vec![EntityInText::new(EntityType::Spoiler, from_u16, length)],
        };
    }
    TextWithEntities::simple(pattern)
}

/// Splits a lowercased string into Unicode word slices (`Ui::Text::Words`).
pub fn words(lower: &str) -> Vec<&str> {
    static RE_WORDS: OnceLock<Regex> = OnceLock::new();
    let re = RE_WORDS.get_or_init(|| Regex::new(r"\W").expect("valid word regex"));
    re.split(lower).filter(|s| !s.is_empty()).collect()
}

// --- Language-aware uppercasing & capitalization (text_utilities.cpp) ---

/// Language-specific case conversion rule set (`UpperRules` in `text_utilities.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpperRules {
    /// Standard Unicode uppercasing (plus Greek tonos/dialytika rules).
    #[default]
    Default,
    /// Turkish / Azerbaijani (`'i'` -> `'İ'`).
    Turkic,
    /// Georgian (keeps Mkhedruli `U+10D0..=U+10FF` unchanged).
    Georgian,
    /// Armenian (`'և'` -> `"ԵՎ"` / `"Եվ"`).
    Armenian,
}

/// Resolves `UpperRules` from a BCP-47 / ISO language identifier (`Ui::Text::SetUpperLanguage`).
pub fn upper_rules_for_language(language_id: &str) -> UpperRules {
    let lower = language_id.to_lowercase();
    let base = lower.split(['-', '_']).next().unwrap_or("");
    match base {
        "tr" | "az" => UpperRules::Turkic,
        "ka" => UpperRules::Georgian,
        "hy" => UpperRules::Armenian,
        _ => UpperRules::Default,
    }
}

#[inline]
fn is_mkhedruli(ch: char) -> bool {
    ('\u{10D0}'..='\u{10FF}').contains(&ch)
}

#[inline]
fn has_greek_tonos(ch: char) -> bool {
    let c = ch as u32;
    (0x0386..=0x0390).contains(&c)
        || (0x03AC..=0x03B0).contains(&c)
        || (0x03CC..=0x03CE).contains(&c)
}

fn without_greek_tonos(ch: char) -> char {
    match ch as u32 {
        0x0386 => '\u{0391}',
        0x0388 => '\u{0395}',
        0x0389 => '\u{0397}',
        0x038A => '\u{0399}',
        0x038C => '\u{039F}',
        0x038E => '\u{03A5}',
        0x038F => '\u{03A9}',
        0x0390 => '\u{03CA}',
        0x03AC => '\u{03B1}',
        0x03AD => '\u{03B5}',
        0x03AE => '\u{03B7}',
        0x03AF => '\u{03B9}',
        0x03B0 => '\u{03CB}',
        0x03CC => '\u{03BF}',
        0x03CD => '\u{03C5}',
        0x03CE => '\u{03C9}',
        _ => ch,
    }
}

fn greek_dialytika(accented: char, next: char) -> Option<char> {
    let first = without_greek_tonos(accented).to_lowercase().next()?;
    let second = next.to_lowercase().next()?;
    if second == '\u{03B9}' && matches!(first, '\u{03B1}' | '\u{03B5}' | '\u{03BF}' | '\u{03C5}') {
        Some('\u{03CA}')
    } else if second == '\u{03C5}' && matches!(first, '\u{03B1}' | '\u{03B5}' | '\u{03BF}') {
        Some('\u{03CB}')
    } else {
        None
    }
}

/// Converts `text` to uppercase using default rules (including Greek tonos/dialytika rules)
/// (`Ui::Text::Upper`).
pub fn upper(text: &str) -> String {
    upper_with_rules(text, UpperRules::Default)
}

/// Converts `text` to uppercase using explicit `UpperRules` (`Ui::Text::Upper`).
pub fn upper_with_rules(text: &str, rules: UpperRules) -> String {
    let mut chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    if chars.iter().copied().any(has_greek_tonos) {
        let orig = chars.clone();
        let is_letter = |idx: isize| -> bool {
            idx >= 0 && (idx as usize) < len && orig[idx as usize].is_alphabetic()
        };
        for i in 0..len {
            let ch = orig[i];
            if !has_greek_tonos(ch) {
                continue;
            }
            let code = ch as u32;
            let disjunctive = (code == 0x03AE || code == 0x0389)
                && !is_letter(i as isize - 1)
                && !is_letter(i as isize + 1);
            if disjunctive {
                continue;
            }
            chars[i] = without_greek_tonos(ch);
            if i + 1 < len
                && let Some(d) = greek_dialytika(ch, orig[i + 1])
            {
                chars[i + 1] = d;
            }
        }
    }

    let prepared: String = chars.into_iter().collect();
    match rules {
        UpperRules::Turkic => prepared.replace('i', "\u{0130}").to_uppercase(),
        UpperRules::Georgian => {
            let mut out = String::with_capacity(prepared.len());
            for ch in prepared.chars() {
                if is_mkhedruli(ch) {
                    out.push(ch);
                } else {
                    for u in ch.to_uppercase() {
                        out.push(u);
                    }
                }
            }
            out
        }
        UpperRules::Armenian => prepared.replace('\u{0587}', "եվ").to_uppercase(),
        UpperRules::Default => prepared.to_uppercase(),
    }
}

/// Capitalizes the first character of `text` using default rules (`Ui::Text::Capitalized`).
pub fn capitalized(text: &str) -> String {
    capitalized_with_rules(text, UpperRules::Default)
}

/// Capitalizes the first character of `text` using explicit `UpperRules` (`Ui::Text::Capitalized`).
pub fn capitalized_with_rules(text: &str, rules: UpperRules) -> String {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let rest = chars.as_str();
    match rules {
        UpperRules::Turkic if first == 'i' => return format!("\u{0130}{rest}"),
        UpperRules::Georgian if is_mkhedruli(first) => return text.to_string(),
        UpperRules::Armenian if first == '\u{0587}' => return format!("Եվ{rest}"),
        _ => {}
    }
    let mut out = String::with_capacity(text.len());
    for u in first.to_uppercase() {
        out.push(u);
    }
    out.push_str(rest);
    out
}

// --- Song name formatting (SourceFiles/ui/text/format_song_name.cpp) ---

/// Resolved title and performer pair (`FormatSongName::ComposedName`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposedSongName {
    /// Track title (or filename / `"Unknown Track"`).
    pub title: String,
    /// Performer name (empty if omitted).
    pub performer: String,
}

/// Formats audio track names with optional bold/semibold performer (`Ui::Text::FormatSongName`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatSongName {
    composed: ComposedSongName,
    no_dash: bool,
}

impl FormatSongName {
    /// Constructs a `FormatSongName` from filename, song title, and song performer.
    pub fn new(filename: &str, song_title: &str, song_performer: &str) -> Self {
        const UNKNOWN: &str = "Unknown Track";
        let composed = if song_title.is_empty() && song_performer.is_empty() {
            ComposedSongName {
                title: if filename.is_empty() {
                    UNKNOWN.to_string()
                } else {
                    filename.to_string()
                },
                performer: String::new(),
            }
        } else if song_performer.is_empty() {
            ComposedSongName {
                title: song_title.to_string(),
                performer: String::new(),
            }
        } else {
            ComposedSongName {
                title: if song_title.is_empty() {
                    UNKNOWN.to_string()
                } else {
                    song_title.to_string()
                },
                performer: song_performer.to_string(),
            }
        };
        Self {
            composed,
            no_dash: false,
        }
    }

    /// Returns the decomposed title and performer.
    #[inline]
    pub fn composed_name(&self) -> &ComposedSongName {
        &self.composed
    }

    /// Controls whether performer and title are separated by `" "` instead of `" – "`.
    #[inline]
    pub fn set_no_dash(&mut self, no_dash: bool) {
        self.no_dash = no_dash;
    }

    /// Formats the combined song string.
    pub fn string(&self) -> String {
        let dash = if self.composed.title.is_empty() || self.composed.performer.is_empty() {
            ""
        } else if self.no_dash {
            " "
        } else {
            " \u{2013} "
        };
        format!("{}{dash}{}", self.composed.performer, self.composed.title)
    }

    /// Formats the song name as `TextWithEntities` with `Semibold` on the performer (or full title).
    pub fn text_with_entities(&self, bold_only_performer: bool) -> TextWithEntities {
        let text = self.string();
        let mut entities = Vec::new();
        if !bold_only_performer || !self.composed.performer.is_empty() {
            let bold_len = if self.composed.performer.is_empty() {
                utf16_len(&text) as i32
            } else {
                utf16_len(&self.composed.performer) as i32
            };
            entities.push(EntityInText::new(EntityType::Semibold, 0, bold_len));
        }
        TextWithEntities { text, entities }
    }
}

// --- Pure value formatters (SourceFiles/ui/text/format_values.cpp) ---

/// Telegram Stars currency code (`Ui::kCreditsCurrency`).
pub const CREDITS_CURRENCY: &str = "XTR";

/// Formatting rules for a currency (`Ui::CurrencyRule`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrencyRule {
    /// International currency symbol (empty means use ISO code).
    pub international: &'static str,
    /// Thousands separator (`'\0'` for none).
    pub thousands: char,
    /// Decimal separator.
    pub decimal: char,
    /// Whether the currency symbol/code appears on the left.
    pub left: bool,
    /// Whether a space separates the symbol/code and amount.
    pub space: bool,
    /// Decimal digits (exponent of 10).
    pub exponent: u32,
    /// Whether `.00` is stripped when the amount is an integer.
    pub strip_dot_zero: bool,
}

impl Default for CurrencyRule {
    fn default() -> Self {
        Self {
            international: "",
            thousands: ',',
            decimal: '.',
            left: true,
            space: false,
            exponent: 2,
            strip_dot_zero: false,
        }
    }
}

const fn cr(
    international: &'static str,
    thousands: char,
    decimal: char,
    left: bool,
    space: bool,
    exponent: u32,
) -> CurrencyRule {
    CurrencyRule {
        international,
        thousands,
        decimal,
        left,
        space,
        exponent,
        strip_dot_zero: false,
    }
}

const DEFAULT_CR: CurrencyRule = cr("", ',', '.', true, false, 2);

/// Sorted table of known currency rules (`CurrencyRulesMap` in `format_values.cpp`).
const CURRENCY_RULES: &[(&str, CurrencyRule)] = &[
    ("AED", cr("", ',', '.', true, true, 2)),
    ("AFN", DEFAULT_CR),
    ("ALL", cr("", '.', ',', false, false, 2)),
    ("AMD", cr("", ',', '.', false, true, 2)),
    ("ARS", cr("", '.', ',', true, true, 2)),
    ("AUD", cr("AU$", ',', '.', true, false, 2)),
    ("AZN", cr("", ' ', ',', false, true, 2)),
    ("BAM", cr("", '.', ',', false, true, 2)),
    ("BDT", cr("", ',', '.', true, true, 2)),
    ("BGN", cr("", ' ', ',', false, true, 2)),
    ("BHD", cr("", ',', '.', true, true, 3)),
    ("BND", cr("", '.', ',', true, false, 2)),
    ("BOB", cr("", '.', ',', true, true, 2)),
    ("BRL", cr("R$", '.', ',', true, true, 2)),
    ("BYN", cr("", ' ', ',', false, true, 2)),
    ("CAD", cr("CA$", ',', '.', true, false, 2)),
    ("CHF", cr("", '\'', '.', false, true, 2)),
    ("CLP", cr("", '.', ',', true, true, 0)),
    ("CNY", cr("CN\u{00A5}", ',', '.', true, false, 2)),
    ("COP", cr("", '.', ',', true, true, 2)),
    ("CRC", cr("", '.', ',', true, false, 2)),
    ("CZK", cr("", ' ', ',', false, true, 2)),
    ("DKK", cr("", '\0', ',', false, true, 2)),
    ("DOP", DEFAULT_CR),
    ("DZD", cr("", ',', '.', true, true, 2)),
    ("EGP", cr("", ',', '.', true, true, 2)),
    ("ETB", DEFAULT_CR),
    ("EUR", cr("\u{20AC}", ' ', ',', false, true, 2)),
    ("GBP", cr("\u{00A3}", ',', '.', true, false, 2)),
    ("GEL", cr("", ' ', ',', false, true, 2)),
    ("GHS", DEFAULT_CR),
    ("GTQ", DEFAULT_CR),
    ("HKD", cr("HK$", ',', '.', true, false, 2)),
    ("HNL", cr("", ',', '.', true, true, 2)),
    ("HRK", cr("", '.', ',', false, true, 2)),
    ("HUF", cr("", ' ', ',', false, true, 2)),
    ("IDR", cr("", '.', ',', true, false, 2)),
    ("ILS", cr("\u{20AA}", ',', '.', true, true, 2)),
    ("INR", cr("\u{20B9}", ',', '.', true, false, 2)),
    ("IQD", cr("", ',', '.', true, true, 3)),
    ("IRR", cr("", ',', '/', false, true, 2)),
    ("ISK", cr("", '.', ',', false, true, 0)),
    ("JMD", DEFAULT_CR),
    ("JOD", cr("", ',', '.', true, false, 3)),
    ("JPY", cr("\u{00A5}", ',', '.', true, false, 0)),
    ("KES", DEFAULT_CR),
    ("KGS", cr("", ' ', '-', false, true, 2)),
    ("KRW", cr("\u{20A9}", ',', '.', true, false, 0)),
    ("KZT", cr("", ' ', '-', true, false, 2)),
    ("LBP", cr("", ',', '.', true, true, 2)),
    ("LKR", cr("", ',', '.', true, true, 2)),
    ("MAD", cr("", ',', '.', true, true, 2)),
    ("MDL", cr("", ',', '.', false, true, 2)),
    ("MMK", DEFAULT_CR),
    ("MNT", cr("", ' ', ',', true, false, 2)),
    ("MOP", DEFAULT_CR),
    ("MUR", DEFAULT_CR),
    ("MVR", cr("", ',', '.', false, true, 2)),
    ("MXN", cr("MX$", ',', '.', true, false, 2)),
    ("MYR", DEFAULT_CR),
    ("MZN", DEFAULT_CR),
    ("NGN", DEFAULT_CR),
    ("NIO", cr("", ',', '.', true, true, 2)),
    ("NOK", cr("", ' ', ',', true, true, 2)),
    ("NPR", DEFAULT_CR),
    ("NZD", cr("NZ$", ',', '.', true, false, 2)),
    ("PAB", cr("", ',', '.', true, true, 2)),
    ("PEN", cr("", ',', '.', true, true, 2)),
    ("PHP", DEFAULT_CR),
    ("PKR", DEFAULT_CR),
    ("PLN", cr("", ' ', ',', false, true, 2)),
    ("PYG", cr("", '.', ',', true, true, 0)),
    ("QAR", cr("", ',', '.', true, true, 2)),
    ("RON", cr("", '.', ',', false, true, 2)),
    ("RSD", cr("", '.', ',', false, true, 2)),
    ("RUB", cr("", ' ', ',', false, true, 2)),
    ("SAR", cr("", ',', '.', true, true, 2)),
    ("SEK", cr("", '.', ',', false, true, 2)),
    ("SGD", DEFAULT_CR),
    ("SYP", cr("", ',', '.', true, true, 2)),
    ("THB", cr("\u{0E3F}", ',', '.', true, false, 2)),
    ("TJS", cr("", ' ', ';', false, true, 2)),
    ("TRY", cr("", '.', ',', false, true, 2)),
    ("TTD", DEFAULT_CR),
    ("TWD", cr("NT$", ',', '.', true, false, 2)),
    ("TZS", DEFAULT_CR),
    ("UAH", cr("", ' ', ',', false, false, 2)),
    ("UGX", cr("", ',', '.', true, false, 0)),
    ("USD", cr("$", ',', '.', true, false, 2)),
    ("UYU", cr("", '.', ',', true, true, 2)),
    ("UZS", cr("", ' ', ',', false, true, 2)),
    ("VEF", cr("", '.', ',', true, true, 2)),
    ("VND", cr("\u{20AB}", '.', ',', false, true, 0)),
    ("YER", cr("", ',', '.', true, true, 2)),
    ("ZAR", cr("", ',', '.', true, true, 2)),
];

/// Looks up formatting rules for `currency` (`Ui::LookupCurrencyRule`).
pub fn lookup_currency_rule(currency: &str) -> CurrencyRule {
    CURRENCY_RULES
        .binary_search_by_key(&currency, |&(code, _)| code)
        .ok()
        .map_or_else(CurrencyRule::default, |idx| CURRENCY_RULES[idx].1)
}

/// Returns `true` if `currency` is in the known currency rules table (`Ui::KnownCurrency`).
pub fn known_currency(currency: &str) -> bool {
    CURRENCY_RULES
        .binary_search_by_key(&currency, |&(code, _)| code)
        .is_ok()
}

/// Returns the international symbol for `currency`, or `currency` itself (`Ui::CurrencyName`).
pub fn currency_name(currency: &str) -> &str {
    let rule = lookup_currency_rule(currency);
    if rule.international.is_empty() {
        currency
    } else {
        rule.international
    }
}

/// Formats a floating-point amount with grouping thousands separators and fixed decimal precision
/// (`Ui::FormatWithSeparators`).
pub fn format_with_separators(
    amount: f64,
    precision: usize,
    decimal: char,
    thousands: char,
) -> String {
    let formatted = format!("{amount:.precision$}");
    let (neg, unsigned) = match formatted.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, formatted.as_str()),
    };
    let (int_part, frac_part) = match unsigned.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (unsigned, None),
    };
    let mut out = String::with_capacity(formatted.len() + int_part.len() / 3 + 1);
    if neg {
        out.push('-');
    }
    let digits = int_part.as_bytes();
    for (idx, &b) in digits.iter().enumerate() {
        if idx > 0 && thousands != '\0' && (digits.len() - idx).is_multiple_of(3) {
            out.push(thousands);
        }
        out.push(b as char);
    }
    if let Some(frac) = frac_part {
        out.push(decimal);
        out.push_str(frac);
    }
    out
}

/// Formats an integer minor-unit amount and currency code (`Ui::FillAmountAndCurrency`).
pub fn fill_amount_and_currency(amount: i64, currency: &str, force_strip_dot_zero: bool) -> String {
    let abs_amount = amount.unsigned_abs();
    if currency == CREDITS_CURRENCY {
        return format!("\u{2B50}{abs_amount}");
    }
    let rule = lookup_currency_rule(currency);
    let prefix = if amount < 0 { "\u{2212}" } else { "" };
    let value = (abs_amount as f64) / 10f64.powi(rule.exponent as i32);
    let name = currency_name(currency);
    let mut result = String::from(prefix);
    if rule.left {
        result.push_str(name);
        if rule.space {
            result.push(' ');
        }
    }
    let precision = if (!rule.strip_dot_zero && !force_strip_dot_zero) || value.floor() != value {
        rule.exponent as usize
    } else {
        0
    };
    result.push_str(&format_with_separators(
        value,
        precision,
        rule.decimal,
        rule.thousands,
    ));
    if !rule.left {
        if rule.space {
            result.push(' ');
        }
        result.push_str(name);
    }
    result
}

/// Formats a duration in seconds as `MM:SS` or `H:MM:SS` (`Ui::FormatDurationText`).
pub fn format_duration_text(duration: i64) -> String {
    let hours = duration / 3600;
    let minutes = (duration % 3600) / 60;
    let seconds = duration % 60;
    if hours != 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// Formats pixel dimensions as `"W×H"` (`Ui::FormatImageSizeText`).
pub fn format_image_size_text(width: i32, height: i32) -> String {
    format!("{width}\u{00D7}{height}")
}

/// Formats a byte size with caller-provided unit labels (`Ui::FormatSizeText`).
pub fn format_size_text_with_units(size: i64, bytes: &str, kb: &str, mb: &str) -> String {
    if size >= 1024 * 1024 {
        let tenths = (size * 10) / (1024 * 1024);
        format!("{}.{} {mb}", tenths / 10, tenths % 10)
    } else if size >= 1024 {
        let tenths = (size * 10) / 1024;
        format!("{}.{} {kb}", tenths / 10, tenths % 10)
    } else {
        format!("{size} {bytes}")
    }
}

/// Formats a byte size with English labels (`"B"`, `"KB"`, `"MB"`).
pub fn format_size_text(size: i64) -> String {
    format_size_text_with_units(size, "B", "KB", "MB")
}

// --- TextParseOptions presets (SourceFiles/ui/text/text_options.cpp + lib_ui/ui/text/text.cpp) ---

/// Text layout direction (`Qt::LayoutDirection` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LayoutDirection {
    /// Automatically detect from content.
    #[default]
    Auto,
    /// Left-to-right.
    LeftToRight,
    /// Right-to-left.
    RightToLeft,
}

/// Options controlling text parsing and layout constraints (`TextParseOptions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextParseOptions {
    /// Entity/markdown parsing flags.
    pub flags: TextParseFlags,
    /// Optional maximum width hint (`0` = unlimited).
    pub max_w: i32,
    /// Optional maximum height / lines hint (`0` = unlimited).
    pub max_h: i32,
    /// Paragraph direction.
    pub dir: LayoutDirection,
}

/// Default text parse options (`kDefaultTextOptions`).
pub fn default_text_options() -> TextParseOptions {
    TextParseOptions {
        flags: TextParseFlags::LINKS | TextParseFlags::MULTILINE | TextParseFlags::MARKDOWN,
        max_w: 0,
        max_h: 0,
        dir: LayoutDirection::Auto,
    }
}

/// Markup text parse options (`kMarkupTextOptions`).
pub fn markup_text_options() -> TextParseOptions {
    TextParseOptions {
        flags: TextParseFlags::LINKS
            | TextParseFlags::MENTIONS
            | TextParseFlags::HASHTAGS
            | TextParseFlags::BOT_COMMANDS
            | TextParseFlags::MULTILINE
            | TextParseFlags::MARKDOWN,
        max_w: 0,
        max_h: 0,
        dir: LayoutDirection::Auto,
    }
}

/// Plain text parse options (`kPlainTextOptions`).
pub fn plain_text_options() -> TextParseOptions {
    TextParseOptions {
        flags: TextParseFlags::MULTILINE,
        max_w: 0,
        max_h: 0,
        dir: LayoutDirection::Auto,
    }
}

/// History message default options (`Ui::ItemTextDefaultOptions`).
pub fn history_text_options() -> TextParseOptions {
    TextParseOptions {
        flags: TextParseFlags::LINKS
            | TextParseFlags::MENTIONS
            | TextParseFlags::HASHTAGS
            | TextParseFlags::MULTILINE
            | TextParseFlags::MARKDOWN,
        max_w: 0,
        max_h: 0,
        dir: LayoutDirection::Auto,
    }
}

/// History bot message options (`Ui::ItemTextBotDefaultOptions`).
pub fn history_bot_options() -> TextParseOptions {
    TextParseOptions {
        flags: TextParseFlags::LINKS
            | TextParseFlags::MENTIONS
            | TextParseFlags::HASHTAGS
            | TextParseFlags::BOT_COMMANDS
            | TextParseFlags::MULTILINE
            | TextParseFlags::MARKDOWN,
        max_w: 0,
        max_h: 0,
        dir: LayoutDirection::Auto,
    }
}

/// History service message options (`Ui::ItemTextServiceOptions`).
pub fn history_service_options() -> TextParseOptions {
    TextParseOptions {
        flags: TextParseFlags::LINKS
            | TextParseFlags::MENTIONS
            | TextParseFlags::HASHTAGS
            | TextParseFlags::MARKDOWN,
        max_w: 0,
        max_h: 0,
        dir: LayoutDirection::Auto,
    }
}

/// Name text options (`Ui::NameTextOptions`).
pub fn name_text_options() -> TextParseOptions {
    TextParseOptions {
        flags: TextParseFlags::empty(),
        max_w: 4096,
        max_h: 1,
        dir: LayoutDirection::Auto,
    }
}

/// Dialog preview text options (`Ui::DialogTextOptions`).
pub fn dialog_text_options() -> TextParseOptions {
    TextParseOptions {
        flags: TextParseFlags::COLORIZED | TextParseFlags::MARKDOWN,
        max_w: 0,
        max_h: 1,
        dir: LayoutDirection::Auto,
    }
}
