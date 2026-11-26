//! The MTProto shape of the owner's message (TGB-3009): a line with a mention, then a collapsed
//! (expandable) `messageEntityBlockquote` whose text is all `messageEntitySpoiler`, as layer
//! 233 sends it. Synthetic data, no network.
//!
//! The history widget draws a text with the block renderer when it holds a quote and measures
//! the collapsed quote with three lines; that needs both entities to reach it with the
//! collapsed flag (`"1"` as the quote's data) and the spoiler over the quote's exact range.

use tg_api::convert::message_from_tl;
use tg_data::UserId as DataUserId;
use tg_text::{EntityType, utf16_len};
use tg_tl::{enums as tl_enums, types as tl_types};

use crate::convert::data_text_to_ui_text;
use crate::tests_reply_targets::{SELF_ID, tl_message};

#[test]
fn a_spoiler_inside_a_collapsed_quote_reaches_the_history_text() {
    let head = "Послание от Тестовый Бот для @sample_user:\n";
    let quote = "Строка цитаты под спойлером, достаточно длинная для нескольких строк.";
    let text = format!("{head}{quote}");
    let mention_at = utf16_len("Послание от Тестовый Бот для ") as i32;
    let quote_at = utf16_len(head) as i32;
    let quote_len = utf16_len(quote) as i32;

    let peer = tl_enums::Peer::Chat(tl_types::PeerChat { chat_id: 77 });
    let mut msg = tl_message(5, peer, 200, &text, None);
    let tl_enums::Message::Message(ref mut inner) = msg else {
        panic!("a regular message");
    };
    inner.entities = Some(vec![
        tl_enums::MessageEntity::Mention(tl_types::MessageEntityMention {
            offset: mention_at,
            length: utf16_len("@sample_user") as i32,
        }),
        tl_enums::MessageEntity::Blockquote(tl_types::MessageEntityBlockquote {
            collapsed: true,
            offset: quote_at,
            length: quote_len,
        }),
        tl_enums::MessageEntity::Spoiler(tl_types::MessageEntitySpoiler {
            offset: quote_at,
            length: quote_len,
        }),
    ]);

    let data = message_from_tl(&msg, DataUserId(SELF_ID as u64)).expect("converted");
    let tg_data::MessageContent::Regular { text: body, .. } = &data.content else {
        panic!("a text message");
    };
    let ui = data_text_to_ui_text(body);
    assert_eq!(ui.text, text);
    let quote_entity = ui
        .entities
        .iter()
        .find(|e| e.entity_type() == EntityType::Blockquote)
        .expect("the quote");
    assert!(quote_entity.is_collapsed_blockquote(), "expandable quote");
    assert_eq!(
        (quote_entity.offset(), quote_entity.length()),
        (quote_at, quote_len)
    );
    let spoiler = ui
        .entities
        .iter()
        .find(|e| e.entity_type() == EntityType::Spoiler)
        .expect("the spoiler");
    assert_eq!((spoiler.offset(), spoiler.length()), (quote_at, quote_len));
}
