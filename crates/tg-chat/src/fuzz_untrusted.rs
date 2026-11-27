//! Panic fuzz over compose-field queries (typed or pasted text) and saved recent emoji
//! (TGB-3065).

use tg_fuzz::Fuzz;

use crate::emoji::EmojiSuggestionConfig;
use crate::types::ComposeFeatures;

const TOKENS: &[&str] = &[
    "@",
    "#",
    "/",
    ":",
    "_",
    "a",
    "Z",
    "1",
    " ",
    "\n",
    "bot",
    "@bot ",
    "/start@bot",
    ":smile",
    ":-)",
    "\u{e9}",
    "\u{1f600}",
    "\u{200d}",
    "\u{fe0f}",
    "\u{301}",
    "'",
    "\"",
    "-",
    ".",
];

#[test]
fn compose_queries_do_not_panic() {
    let mut fuzz = Fuzz::new(0xC4A7_0001, 5_000);
    let config = EmojiSuggestionConfig {
        suggest_emoji_enabled: true,
        suggest_exact_first_word: true,
        modern_limit: 20,
        legacy_limit: 8,
    };
    for i in 0..fuzz.iterations() {
        let text = fuzz.text(TOKENS, 16);
        let boundaries: Vec<usize> = (0..=text.len())
            .filter(|&b| text.is_char_boundary(b))
            .collect();
        let cursor = *fuzz.rng().pick(&boundaries);
        let fragment_start = *fuzz.rng().pick(&boundaries);
        let selection = fuzz.rng().one_in(3);
        let first_tag = fuzz.rng().one_in(2).then(|| fuzz.rng().below(20));
        let max_chars = fuzz.rng().below(10);
        fuzz.check(
            "compose queries",
            i,
            &(&text, cursor, fragment_start),
            || {
                let features = ComposeFeatures::default();
                let _ = crate::autocomplete::parse_mention_hashtag_bot_command_query(
                    &text,
                    cursor,
                    fragment_start,
                    selection,
                    features,
                );
                let _ = crate::autocomplete::parse_cursor_autocomplete_query(
                    &text,
                    cursor,
                    fragment_start,
                    selection,
                    features,
                    max_chars,
                );
                let _ = crate::autocomplete::parse_inline_bot_query(&text, first_tag, |_| None);
                let _ = crate::autocomplete::prevent_inline_bot_query(&text);
                let _ =
                    crate::emoji::parse_emoji_colon_query(&text, cursor, fragment_start, max_chars);
                let _ = crate::emoji::parse_emoji_suggestion_query(
                    &text,
                    cursor,
                    fragment_start,
                    selection,
                    config,
                );
                let _ = crate::emoji::normalize_emoji_query(&text);
                let _ = crate::emoji::RecentEmojiDocument::parse(&text);
            },
        );
    }
    fuzz.finish();
}
