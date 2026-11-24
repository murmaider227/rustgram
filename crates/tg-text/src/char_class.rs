//! Character classification helpers for Telegram text processing.
//!
//! Port of `Telegram/lib_ui/ui/text/text.cpp` (`IsBad`, `IsWordSeparator`,
//! `IsAlmostLinkEnd`, `IsLinkEnd`, `IsNewline`, `IsSpace`, `IsDiacritic`,
//! `IsReplacedBySpace`, `IsTrimmed`) and `Telegram/lib_ui/ui/widgets/fields/input_field.cpp`
//! (`IsNewline`, `IsUsernameChar`).

use unicode_general_category::{GeneralCategory, get_general_category};

/// U+FFFC OBJECT REPLACEMENT CHARACTER.
pub const OBJECT_REPLACEMENT_CHAR: char = '\u{FFFC}';
/// U+2028 LINE SEPARATOR.
pub const LINE_SEPARATOR: char = '\u{2028}';
/// U+2029 PARAGRAPH SEPARATOR.
pub const PARAGRAPH_SEPARATOR: char = '\u{2029}';
/// U+200B ZERO WIDTH SPACE.
pub const ZERO_WIDTH_SPACE: char = '\u{200B}';
/// U+FDD0 QTextBeginningOfFrame.
pub const TEXT_BEGINNING_OF_FRAME: char = '\u{FDD0}';
/// U+FDD1 QTextEndOfFrame.
pub const TEXT_END_OF_FRAME: char = '\u{FDD1}';

/// Returns `true` for control/bad characters filtered by tdesktop (`Ui::Text::IsBad`).
#[inline]
pub fn is_bad(ch: char) -> bool {
    let code = ch as u32;
    code == 0
        || (8232..8237).contains(&code)
        || ((65024..65040).contains(&code) && code != 65039)
        || ((127..160).contains(&code) && code != 156)
}

/// Returns `true` if `ch` is a standalone word separator (`Ui::Text::IsWordSeparator(QChar)`).
#[inline]
pub fn is_word_separator(ch: char) -> bool {
    matches!(
        ch,
        ' ' | '\n'
            | '.'
            | ','
            | '?'
            | '!'
            | '@'
            | '#'
            | '$'
            | ':'
            | ';'
            | '-'
            | '<'
            | '>'
            | '['
            | ']'
            | '('
            | ')'
            | '{'
            | '}'
            | '='
            | '/'
            | '+'
            | '%'
            | '&'
            | '^'
            | '*'
            | '\''
            | '"'
            | '`'
            | '~'
            | '|'
            | '\u{2013}' // en dash
            | '\u{2014}' // em dash
            | '\u{2018}' // left single quotation mark
            | '\u{2019}' // right single quotation mark
            | '\u{201C}' // left double quotation mark
            | '\u{201D}' // right double quotation mark
            | '\u{2026}' // horizontal ellipsis
            | OBJECT_REPLACEMENT_CHAR
    )
}

/// Context-aware word separator check (`Ui::Text::IsWordSeparator(QString, int)`).
///
/// An apostrophe (`'`, U+2018, U+2019) inside a word (e.g. `"don't"`) is treated as
/// part of the word, and as a separator only when doubled or at a word boundary.
/// `utf16_pos` is a UTF-16 code unit index into `text`.
pub fn is_word_separator_at(text: &str, utf16_pos: usize) -> bool {
    let units: Vec<u16> = text.encode_utf16().collect();
    is_word_separator_in_utf16(&units, utf16_pos)
}

/// Context-aware word separator check on a UTF-16 slice.
pub fn is_word_separator_in_utf16(units: &[u16], pos: usize) -> bool {
    if pos >= units.len() {
        return true;
    }
    let code = units[pos];
    let is_apostrophe = code == b'\'' as u16 || code == 0x2018 || code == 0x2019;
    if !is_apostrophe {
        return char::from_u32(u32::from(code)).is_some_and(is_word_separator);
    }
    let is_edge = |idx: isize| -> bool {
        if idx < 0 || (idx as usize) >= units.len() {
            return true;
        }
        char::from_u32(u32::from(units[idx as usize])).is_some_and(is_word_separator)
    };
    is_edge(pos as isize - 1) || is_edge(pos as isize + 1)
}

/// Returns `true` for punctuation that terminates a URL when followed by a link end
/// (`Ui::Text::IsAlmostLinkEnd`).
#[inline]
pub fn is_almost_link_end(ch: char) -> bool {
    matches!(ch, '?' | ',' | '.' | '"' | ':' | '!' | '\'')
}

/// Returns `true` for characters that unconditionally terminate a URL (`Ui::Text::IsLinkEnd`).
///
/// Matches C++ behavior where UTF-16 surrogate code units (i.e. all non-BMP characters,
/// including emoji) end links.
#[inline]
pub fn is_link_end(ch: char) -> bool {
    is_bad(ch) || is_space(ch) || is_newline(ch) || (ch as u32) > 0xFFFF
}

/// Returns `true` for newline characters in rendered/parsed text (`Ui::Text::IsNewline`).
#[inline]
pub fn is_newline(ch: char) -> bool {
    ch == '\n' || ch as u32 == 156
}

/// Returns `true` for newline characters in input fields (`Ui::IsNewline` in `input_field.cpp`).
#[inline]
pub fn is_input_newline(ch: char) -> bool {
    matches!(
        ch,
        '\r' | '\n'
            | TEXT_BEGINNING_OF_FRAME
            | TEXT_END_OF_FRAME
            | PARAGRAPH_SEPARATOR
            | LINE_SEPARATOR
    )
}

/// Returns `true` for whitespace/control/separator characters (`Ui::Text::IsSpace`).
#[inline]
pub fn is_space(ch: char) -> bool {
    ch.is_whitespace()
        || (ch as u32) < 32
        || ch == PARAGRAPH_SEPARATOR
        || ch == LINE_SEPARATOR
        || ch == OBJECT_REPLACEMENT_CHAR
        || ch == '\r'
        || ch == '\t'
}

/// Returns `true` for non-spacing combining marks and variation selectors (`Ui::Text::IsDiacritic`).
#[inline]
pub fn is_diacritic(ch: char) -> bool {
    let code = ch as u32;
    get_general_category(ch) == GeneralCategory::NonspacingMark
        || code == 1652
        || (64606..=64611).contains(&code)
}

/// Returns `true` for characters replaced with ASCII space by Telegram servers
/// (`Ui::Text::IsReplacedBySpace`).
#[inline]
pub fn is_replaced_by_space(ch: char) -> bool {
    let code = ch as u32;
    code <= 0x02
        || (0x07..=0x09).contains(&code)
        || (0x0B..=0x1F).contains(&code)
        || code == 819
        || code == 831
        || code == 778
        || (8232..=8237).contains(&code)
}

/// Returns `true` for characters trimmed from the start/end of outgoing messages
/// (`Ui::Text::IsTrimmed`).
#[inline]
pub fn is_trimmed(ch: char) -> bool {
    (ch != OBJECT_REPLACEMENT_CHAR && is_space(ch)) || is_bad(ch) || ch == ZERO_WIDTH_SPACE
}

/// Returns `true` for `,`, `:`, `;` (`IsSentencePartEnd` in `text_entity.cpp`).
#[inline]
pub fn is_sentence_part_end(ch: char) -> bool {
    matches!(ch, ',' | ':' | ';')
}

/// Returns `true` for `.`, `?`, `!` (`IsSentenceEnd` in `text_entity.cpp`).
#[inline]
pub fn is_sentence_end(ch: char) -> bool {
    matches!(ch, '.' | '?' | '!')
}

/// Returns `true` for ASCII username characters `[A-Za-z0-9_]` (`IsUsernameChar` in `input_field.cpp`).
#[inline]
pub fn is_username_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}
