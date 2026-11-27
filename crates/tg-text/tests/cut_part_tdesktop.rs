//! `cut_part` cuts where tdesktop's `TextUtilities::CutPart` (`ui/text/text_entity.cpp:1521`)
//! cuts (TGB-3069).
//!
//! CutPart checks `s >= limit` before it counts the current character, so the character at index
//! `limit` is still a break candidate and a forced cut sends `limit` characters. The old port
//! counted first and cut one character early, also splitting a text exactly `limit` long.

use tg_text::{EntityInText, EntityType, TextWithEntities, cut_part, cut_part_len};

fn cut(text: &str, limit: usize) -> (String, String) {
    let mut left = TextWithEntities::simple(text);
    let mut sending = TextWithEntities::new();
    assert!(cut_part(&mut sending, &mut left, limit));
    (sending.text, left.text)
}

#[test]
fn forced_cut_sends_limit_characters() {
    assert_eq!(
        cut("helloworld", 5),
        ("hello".to_owned(), "world".to_owned())
    );
}

#[test]
fn a_space_at_index_limit_is_a_break_candidate() {
    assert_eq!(cut("abcd efgh", 4), ("abcd".to_owned(), " efgh".to_owned()));
}

#[test]
fn a_text_exactly_limit_long_is_not_split() {
    assert_eq!(cut("hello", 5), ("hello".to_owned(), String::new()));
}

#[test]
fn counts_code_points_not_utf16_units() {
    // Four astral characters (two UTF-16 units each) fit a limit of 4 code points.
    let text = "\u{1D400}\u{1D401}\u{1D402}\u{1D403}";
    assert_eq!(cut(text, 4), (text.to_owned(), String::new()));
}

#[test]
fn only_pre_code_and_blockquote_are_split() {
    // The whole text is bold: CutPart finds no break outside it and cuts inside it. Bold is not
    // breakable, so tdesktop drops it from both parts instead of splitting it.
    let mut left = TextWithEntities {
        text: "abcdefghij".to_owned(),
        entities: vec![EntityInText::new(EntityType::Bold, 0, 10)],
    };
    let mut sending = TextWithEntities::new();
    assert!(cut_part(&mut sending, &mut left, 5));
    assert_eq!(
        (sending.text.as_str(), left.text.as_str()),
        ("abcde", "fghij")
    );
    assert!(sending.entities.is_empty(), "{:?}", sending.entities);
    assert!(left.entities.is_empty(), "{:?}", left.entities);

    // Code is breakable: both parts keep their share.
    let mut left = TextWithEntities {
        text: "abcdefghij".to_owned(),
        entities: vec![EntityInText::new(EntityType::Code, 0, 10)],
    };
    let mut sending = TextWithEntities::new();
    assert!(cut_part(&mut sending, &mut left, 5));
    assert_eq!(
        sending.entities,
        [EntityInText::new(EntityType::Code, 0, 5)]
    );
    assert_eq!(left.entities, [EntityInText::new(EntityType::Code, 0, 5)]);
}

#[test]
fn cut_part_len_is_the_byte_length_of_the_sent_part() {
    let no_entities: [(i32, i32, EntityType); 0] = [];
    assert_eq!(cut_part_len("helloworld", no_entities, 5), 5);
    assert_eq!(cut_part_len("hello", no_entities, 5), 5);
    assert_eq!(cut_part_len("", no_entities, 5), 0);
    // "é" is two bytes in UTF-8.
    assert_eq!(cut_part_len("\u{e9}\u{e9}\u{e9}\u{e9}", no_entities, 2), 4);
    // Hostile spans (negative, past the end, overflowing) are ignored, never a panic.
    let hostile = [
        (-5, 3, EntityType::Bold),
        (i32::MAX, 2, EntityType::Pre),
        (1, i32::MAX, EntityType::Code),
    ];
    assert_eq!(cut_part_len("helloworld", hostile, 5), 5);
}
