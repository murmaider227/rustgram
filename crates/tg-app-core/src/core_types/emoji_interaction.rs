//! Pure interactive-emoji emoticon normalization helpers.
//!
//! Port origin: `Telegram/SourceFiles/chat_helpers/stickers_emoji_pack.cpp`.

/// Colored heart variants mapped to the canonical red heart (`❤`) for interactive emoji
/// animations (`kHearts` in `stickers_emoji_pack.cpp:240-252`).
const COLORED_HEARTS: &[&str] = &[
    "\u{1F49B}", // 💛
    "\u{1F499}", // 💙
    "\u{1F49A}", // 💚
    "\u{1F49C}", // 💜
    "\u{1F9E1}", // 🧡
    "\u{1F5A4}", // 🖤
    "\u{1F90E}", // 🤎
    "\u{1F90D}", // 🤍
];

/// Parses the 0-based animation variant index (`0..=9`) encoded in an animation pack's
/// keycap emoticon (`IndexFromEmoticon` in `stickers_emoji_pack.cpp:44-54`).
///
/// Keycap digits `"1️⃣"`..`"9️⃣"` (or `"1\u{20E3}"`..`"9\u{20E3}"`) map to `0..=8`, and
/// `"🔟"` (`U+1F51F`) maps to `9`.
#[must_use]
pub fn index_from_emoticon(emoticon: &str) -> Option<usize> {
    let mut chars = emoticon.chars();
    let first = chars.next()?;
    if first == '\u{1F51F}' {
        return Some(9);
    }
    if ('1'..='9').contains(&first) && chars.next().is_some() {
        return Some((first as usize) - ('1' as usize));
    }
    None
}

/// Normalizes an emoticon key for lookup in [`EmojiStickersPack`]:
/// trims whitespace, strips variation selector `U+FE0F`, and maps colored hearts to `❤`.
#[must_use]
pub fn normalize_interaction_emoticon(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 8 {
        return None;
    }
    let normalized: String = trimmed.chars().filter(|&c| c != '\u{FE0F}').collect();
    if normalized.is_empty() {
        return None;
    }
    if COLORED_HEARTS.contains(&normalized.as_str()) {
        return Some("\u{2764}".to_owned());
    }
    Some(normalized)
}
