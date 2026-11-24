//! Port of `iv/editor/iv_editor_auto_pair.h` and `iv/editor/iv_editor_auto_pair.cpp`.
//!
//! Pure, UI-independent auto-pairing logic for `(`, `[`, `{`, and `"` including selection
//! wrapping, closing-delimiter skip-over, backspace pair removal, and emoticon guard (`:-(`).

/// Outcome of an auto-pair key operation on a UTF-8 string with `(anchor, head)` UTF-16 offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoPairOutcome {
    /// Auto-pair did not apply; the caller should handle the key normally.
    Unhandled,
    /// Auto-pair transformed the text and updated `(anchor, head)` UTF-16 offsets.
    Applied {
        /// New anchor UTF-16 offset.
        anchor: usize,
        /// New cursor head UTF-16 offset.
        head: usize,
    },
}

pub(crate) const fn closing_delimiter(opening: char) -> Option<char> {
    match opening {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' => Some('"'),
        _ => None,
    }
}

const fn is_closing_delimiter(ch: char) -> bool {
    matches!(ch, ')' | ']' | '}' | '"')
}

fn allowed_before(next: Option<char>) -> bool {
    match next {
        None => true,
        Some(ch) => {
            ch.is_whitespace()
                || is_closing_delimiter(ch)
                || matches!(ch, '.' | ',' | ';' | ':' | '!' | '?')
        }
    }
}

fn allowed_after(typed: char, previous: Option<char>) -> bool {
    if typed == '"' {
        return !previous.is_some_and(char::is_alphanumeric);
    }
    match previous {
        None => true,
        Some(ch) if ch.is_whitespace() || ch.is_alphanumeric() => true,
        // Smileys like ":-(" stay unpaired unless preceded by another delimiter.
        Some(ch) => closing_delimiter(ch).is_some() || is_closing_delimiter(ch),
    }
}

fn char_before_byte(text: &str, byte_pos: usize) -> Option<char> {
    text[..byte_pos].chars().next_back()
}

fn char_after_byte(text: &str, byte_pos: usize) -> Option<char> {
    text[byte_pos..].chars().next()
}

/// Handles typing `typed` with auto-pair rules (`HandleAutoPairKey` in `iv_editor_auto_pair.cpp`).
pub fn handle_auto_pair_char(
    text: &mut String,
    anchor_u16: usize,
    head_u16: usize,
    typed: char,
) -> AutoPairOutcome {
    let from_u16 = anchor_u16.min(head_u16);
    let till_u16 = anchor_u16.max(head_u16);
    let has_selection = from_u16 != till_u16;
    let from_b = crate::rich_page::utf16_to_byte_offset(text, from_u16);
    let till_b = crate::rich_page::utf16_to_byte_offset(text, till_u16);
    let prev = char_before_byte(text, from_b);
    let next = char_after_byte(text, till_b);

    // Skip over matching closing character when cursor is collapsed
    if !has_selection && next == Some(typed) && is_closing_delimiter(typed) {
        let next_pos = till_u16 + typed.len_utf16();
        return AutoPairOutcome::Applied {
            anchor: next_pos,
            head: next_pos,
        };
    }

    let Some(closing) = closing_delimiter(typed) else {
        return AutoPairOutcome::Unhandled;
    };

    if has_selection {
        text.insert(till_b, closing);
        text.insert(from_b, typed);
        let shift = typed.len_utf16();
        return AutoPairOutcome::Applied {
            anchor: from_u16 + shift,
            head: till_u16 + shift,
        };
    }

    if allowed_after(typed, prev) && allowed_before(next) {
        let mut pair = String::with_capacity(typed.len_utf8() + closing.len_utf8());
        pair.push(typed);
        pair.push(closing);
        text.insert_str(from_b, &pair);
        let caret = from_u16 + typed.len_utf16();
        return AutoPairOutcome::Applied {
            anchor: caret,
            head: caret,
        };
    }

    AutoPairOutcome::Unhandled
}

/// Handles `Backspace` between an empty auto-paired delimiter pair (`()`, `[]`, `{}`, `""`).
pub fn handle_auto_pair_backspace(
    text: &mut String,
    anchor_u16: usize,
    head_u16: usize,
) -> AutoPairOutcome {
    if anchor_u16 != head_u16 || anchor_u16 == 0 {
        return AutoPairOutcome::Unhandled;
    }
    let pos_b = crate::rich_page::utf16_to_byte_offset(text, anchor_u16);
    let Some(prev) = char_before_byte(text, pos_b) else {
        return AutoPairOutcome::Unhandled;
    };
    let Some(next) = char_after_byte(text, pos_b) else {
        return AutoPairOutcome::Unhandled;
    };
    if closing_delimiter(prev) != Some(next) {
        return AutoPairOutcome::Unhandled;
    }
    let remove_start = pos_b - prev.len_utf8();
    let remove_end = pos_b + next.len_utf8();
    text.replace_range(remove_start..remove_end, "");
    let new_pos = anchor_u16 - prev.len_utf16();
    AutoPairOutcome::Applied {
        anchor: new_pos,
        head: new_pos,
    }
}
