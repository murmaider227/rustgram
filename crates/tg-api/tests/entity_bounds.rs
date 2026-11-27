//! Server entity bounds are validated once, at conversion (TGB-3081).
//!
//! `MessageEntity` offsets and lengths came through unchecked, so `offset = i32::MAX` or a
//! negative length reached every consumer, where `offset + length` overflows (a panic in debug
//! builds, a wrong range in release). After conversion every entity has `0 <= offset`,
//! `0 < length` and `offset + length <= i32::MAX`, and when the text is known,
//! `offset + length <= text length` in UTF-16 units.

use tg_api::convert::{entities_from_tl, message_from_tl, text_with_entities_from_tl};
use tg_data::{EntityKind, TextEntity, UserId};
use tg_tl::{enums as tl_enums, types as tl_types};

fn hostile_entities() -> Vec<tl_enums::MessageEntity> {
    vec![
        tl_enums::MessageEntity::Bold(tl_types::MessageEntityBold {
            offset: i32::MAX,
            length: 2,
        }),
        tl_enums::MessageEntity::Italic(tl_types::MessageEntityItalic {
            offset: 1,
            length: i32::MAX,
        }),
        tl_enums::MessageEntity::Code(tl_types::MessageEntityCode {
            offset: -5,
            length: 3,
        }),
        tl_enums::MessageEntity::Underline(tl_types::MessageEntityUnderline {
            offset: 0,
            length: 0,
        }),
        tl_enums::MessageEntity::Strike(tl_types::MessageEntityStrike {
            offset: 0,
            length: -4,
        }),
        tl_enums::MessageEntity::Spoiler(tl_types::MessageEntitySpoiler {
            offset: 3,
            length: 1,
        }),
        tl_enums::MessageEntity::Url(tl_types::MessageEntityUrl {
            offset: 0,
            length: 3,
        }),
    ]
}

fn tl_message(text: &str, entities: Vec<tl_enums::MessageEntity>) -> tl_enums::Message {
    tl_enums::Message::Message(tl_types::Message {
        out: false,
        mentioned: false,
        media_unread: false,
        silent: false,
        post: false,
        from_scheduled: false,
        legacy: false,
        edit_hide: false,
        pinned: false,
        noforwards: false,
        invert_media: false,
        offline: false,
        video_processing_pending: false,
        paid_suggested_post_stars: false,
        paid_suggested_post_ton: false,
        guestchat_via_from: None,
        rich_message: None,
        id: 10,
        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser { user_id: 200 })),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: 200 }),
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        reply_to: None,
        date: 1_700_000_000,
        message: text.to_owned(),
        media: None,
        reply_markup: None,
        entities: Some(entities),
        views: None,
        forwards: None,
        replies: None,
        edit_date: None,
        post_author: None,
        grouped_id: None,
        reactions: None,
        restriction_reason: None,
        ttl_period: None,
        quick_reply_shortcut_id: None,
        effect: None,
        factcheck: None,
        report_delivery_until_date: None,
        paid_message_stars: None,
        suggested_post: None,
        schedule_repeat_period: None,
        summary_from_language: None,
    })
}

#[test]
fn entities_without_text_have_no_overflowing_bounds() {
    assert_eq!(
        entities_from_tl(&hostile_entities()),
        [
            TextEntity::new(1, i32::MAX - 1, EntityKind::Italic),
            TextEntity::new(3, 1, EntityKind::Spoiler),
            TextEntity::new(0, 3, EntityKind::Url),
        ]
    );
}

#[test]
fn message_entities_are_clamped_to_the_message_text() {
    let message = message_from_tl(&tl_message("abc", hostile_entities()), UserId(100))
        .expect("a regular message converts");
    let text = message.text().expect("a text message");
    assert_eq!(
        text.entities,
        [
            TextEntity::new(1, 2, EntityKind::Italic),
            TextEntity::new(0, 3, EntityKind::Url),
        ]
    );
}

#[test]
fn text_with_entities_is_clamped_in_utf16_units() {
    // "a😀b" is four UTF-16 units: the emoji is a surrogate pair.
    let tl = tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
        text: "a\u{1F600}b".to_owned(),
        entities: vec![
            tl_enums::MessageEntity::Bold(tl_types::MessageEntityBold {
                offset: 1,
                length: 100,
            }),
            tl_enums::MessageEntity::Italic(tl_types::MessageEntityItalic {
                offset: 4,
                length: 1,
            }),
        ],
    });
    assert_eq!(
        text_with_entities_from_tl(&tl).entities,
        [TextEntity::new(1, 3, EntityKind::Bold)]
    );
}
