//! Long messages split where tdesktop's `TextUtilities::CutPart` splits them (TGB-3060).
//!
//! The old splitter cut every `limit` characters, mid-word, inside links and inside emoji
//! sequences. CutPart picks the best break (paragraph > newline > sentence end > space > word
//! separator) in the second half of the chunk and never cuts an emoji.

use tg_api::MessageSender;
use tg_data::{PeerId, UserId};
use tg_tl::{enums as tl_enums, types as tl_types};

fn split(text: &str, entities: &[tl_enums::MessageEntity], limit: usize) -> Vec<String> {
    let mut sender = MessageSender::default();
    sender.set_message_length_limit(limit);
    sender
        .prepare_text_chunks(PeerId::from_user(UserId(10)), text, entities)
        .into_iter()
        .map(|chunk| chunk.text)
        .collect()
}

#[test]
fn breaks_at_a_space_not_mid_word() {
    assert_eq!(
        split("Hello brave new world", &[], 13),
        ["Hello brave", "new world"]
    );
}

#[test]
fn prefers_a_newline_over_a_later_space() {
    assert_eq!(
        split("one two three\nfour five six", &[], 18),
        ["one two three", "four five six"]
    );
}

#[test]
fn keeps_a_link_whole_when_a_space_comes_before_it() {
    let text = "a b c d e f g https://x.io/abcdef";
    let url = tl_enums::MessageEntity::Url(tl_types::MessageEntityUrl {
        offset: 14,
        length: 19,
    });
    let mut sender = MessageSender::default();
    sender.set_message_length_limit(20);
    let chunks = sender.prepare_text_chunks(PeerId::from_user(UserId(10)), text, &[url]);
    let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, ["a b c d e f g", "https://x.io/abcdef"]);
    assert!(chunks[0].entities.is_empty());
    assert_eq!(
        chunks[1].entities,
        [tl_enums::MessageEntity::Url(tl_types::MessageEntityUrl {
            offset: 0,
            length: 19,
        })]
    );
}

#[test]
fn never_cuts_inside_an_emoji_sequence() {
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    let text = format!("ab{family}cd");
    assert_eq!(split(&text, &[], 3), ["ab", family, "cd"]);
}

#[test]
fn hard_cut_when_there_is_no_better_break() {
    assert_eq!(split("helloworld", &[], 5), ["hello", "world"]);
    assert_eq!(split("  x  ", &[], 5), ["x"]);
    assert_eq!(split("   ", &[], 5), [""]);
}
