//! TGB-3088: server entities are cut to the text they belong to before the app sees them, so
//! code that slices the text by entity range can rely on `offset + length <= text length`.
//! Child module of `layer233_extras`.

use tg_tl::{enums as tl_enums, types as tl_types};

fn bold(offset: i32, length: i32) -> tl_enums::MessageEntity {
    tl_enums::MessageEntity::Bold(tl_types::MessageEntityBold { offset, length })
}

fn tl_text(text: &str, entities: Vec<tl_enums::MessageEntity>) -> tl_enums::TextWithEntities {
    tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
        text: text.to_owned(),
        entities,
    })
}

fn assert_within_text(text: &tg_text::TextWithEntities) {
    let len = tg_text::utf16_len(&text.text);
    for e in &text.entities {
        let end = usize::try_from(e.offset() + e.length()).expect("non-negative end");
        assert!(
            end <= len,
            "entity {e:?} ends past the text ({len} UTF-16 units)"
        );
    }
}

#[test]
fn layer233_text_cuts_an_entity_that_ends_past_the_text() {
    // "héllo" is 5 UTF-16 units; the server says the bold run is 40 long.
    let ui = super::tl_text_with_entities_to_ui(&tl_text("héllo", vec![bold(2, 40), bold(9, 3)]));
    assert_eq!(ui.text, "héllo");
    assert_within_text(&ui);
    assert_eq!(
        ui.entities.len(),
        1,
        "an entity that starts past the end goes"
    );
    assert_eq!(ui.entities[0].offset(), 2);
    assert_eq!(ui.entities[0].length(), 3);
}

#[test]
fn compose_ai_text_cuts_an_entity_that_ends_past_the_text() {
    let ui = crate::compose_ai::parse_tl_text_with_entities(&tl_text("ok 👍", vec![bold(0, 100)]));
    assert_within_text(&ui);
    assert_eq!(ui.entities.len(), 1);
    assert_eq!(ui.entities[0].length(), 5, "\"ok \" plus a surrogate pair");
}
