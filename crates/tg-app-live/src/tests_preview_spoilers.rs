//! TGB-3027: the one-line previews of a message (reply header, chat list) keep the spoiler
//! ranges of its text, so they can be drawn under the particle cover instead of in clear.

use tg_data::{
    EntityKind, Message, MessageContent, MessageMedia, MsgId, PeerId, PhotoData, Session,
    TextEntity, TextWithEntities, UserId,
};

use crate::preview_text::{
    message_preview, message_preview_text, one_line, one_line_with_spoilers,
};
use crate::reply_preview::reply_preview_text;

fn utf16_len(s: &str) -> i32 {
    i32::try_from(s.encode_utf16().count()).unwrap_or(i32::MAX)
}

/// `text` with a spoiler over the first occurrence of `hidden`.
fn with_spoiler(text: &str, hidden: &str) -> TextWithEntities {
    let at = text.find(hidden).expect("hidden part is in the text");
    TextWithEntities {
        text: text.to_owned(),
        entities: vec![TextEntity::new(
            utf16_len(&text[..at]),
            utf16_len(hidden),
            EntityKind::Spoiler,
        )],
    }
}

fn message(text: TextWithEntities, media: MessageMedia) -> Message {
    let peer = PeerId::from_user(UserId(7));
    let mut msg = Message::new_text(peer, MsgId(5), Some(peer), 1_795_000_000, text);
    if let MessageContent::Regular { media: m, .. } = &mut msg.content {
        *m = media;
    }
    msg
}

fn photo() -> MessageMedia {
    MessageMedia::Photo(Box::new(PhotoData {
        id: tg_data::PhotoId(77),
        ..PhotoData::default()
    }))
}

fn shown<'a>(text: &'a str, ranges: &[std::ops::Range<usize>]) -> Vec<&'a str> {
    ranges.iter().map(|r| &text[r.clone()]).collect()
}

#[test]
fn a_text_message_keeps_its_spoiler_in_the_chat_list_preview() {
    let session = Session::new(UserId(1));
    let msg = message(
        with_spoiler("the answer is 42, see you", "42"),
        MessageMedia::Empty,
    );
    let preview = message_preview(&session, &msg);
    assert_eq!(preview.text, message_preview_text(&session, &msg));
    assert_eq!(shown(&preview.text, &preview.spoilers), ["42"]);
}

#[test]
fn a_reply_to_a_captioned_photo_moves_the_spoiler_after_the_media_part() {
    let session = Session::new(UserId(1));
    let msg = message(with_spoiler("look secret", "secret"), photo());
    let reply = reply_preview_text(&session, &msg);
    assert_eq!(reply.text, "Photo, look secret");
    assert_eq!(reply.colored_len, "Photo,".len());
    assert_eq!(shown(&reply.text, &reply.spoilers), ["secret"]);

    // The chat list shows a mini image and then the caption alone.
    let preview = message_preview(&session, &msg);
    assert_eq!(preview.text, "look secret");
    assert_eq!(shown(&preview.text, &preview.spoilers), ["secret"]);
}

#[test]
fn line_breaks_become_spaces_and_the_spoiler_keeps_covering_them() {
    let text = with_spoiler("  first\r\n\n secret\nline \n\nlast  ", "secret\nline");
    let (line, spoilers) = one_line_with_spoilers(&text);
    assert_eq!(line, one_line(&text.text));
    assert_eq!(line, "first  secret line  last");
    assert_eq!(shown(&line, &spoilers), ["secret line"]);
}

#[test]
fn utf16_offsets_after_astral_emoji_map_to_byte_ranges() {
    // "😀" is two UTF-16 units and four bytes; "ü" one unit and two bytes.
    let text = with_spoiler("😀 über geheim 😀", "geheim");
    let (line, spoilers) = one_line_with_spoilers(&text);
    assert_eq!(shown(&line, &spoilers), ["geheim"]);
}

#[test]
fn a_spoiler_on_dropped_blank_lines_only_is_dropped() {
    let mut text = with_spoiler("a\n\n\nb", "\n\n\n");
    let (line, spoilers) = one_line_with_spoilers(&text);
    assert_eq!(line, "a b");
    assert!(spoilers.is_empty(), "{spoilers:?}");

    // Entities from the server are not trusted: out of range or negative ones are skipped.
    text.entities = vec![
        TextEntity::new(-3, 2, EntityKind::Spoiler),
        TextEntity::new(2, 99, EntityKind::Spoiler),
        TextEntity::new(40, 2, EntityKind::Spoiler),
        TextEntity::new(0, i32::MAX, EntityKind::Spoiler),
    ];
    let (line, spoilers) = one_line_with_spoilers(&text);
    assert_eq!(shown(&line, &spoilers), ["b", "a b"]);
}

#[test]
fn media_without_its_caption_in_the_preview_has_no_spoilers() {
    let session = Session::new(UserId(1));
    let mut ttl_photo = PhotoData {
        id: tg_data::PhotoId(78),
        ..PhotoData::default()
    };
    ttl_photo.ttl_seconds = Some(10);
    let msg = message(
        with_spoiler("hidden words", "hidden"),
        MessageMedia::Photo(Box::new(ttl_photo)),
    );
    let reply = reply_preview_text(&session, &msg);
    assert!(!reply.text.contains("hidden"), "{}", reply.text);
    assert!(reply.spoilers.is_empty());
}
