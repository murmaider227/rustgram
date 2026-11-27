//! TGB-3083: message text entities with hostile server bounds (`offset = i32::MAX`) must not
//! overflow `offset + length` in the history text paths (debug builds panicked).

use tg_text::{EntityInText, EntityType, TextWithEntities};

use super::compose_extras::extract_first_url;
use super::message_text_state::ends_with_quote;
use super::text_bridge::entities_to_rich_text;

fn hostile(kind: EntityType) -> TextWithEntities {
    TextWithEntities {
        text: "quoted text".to_owned(),
        entities: vec![
            EntityInText::new(kind, i32::MAX, 2),
            EntityInText::new(kind, 2, i32::MAX),
            EntityInText::new(kind, i32::MIN, -2),
        ],
    }
}

#[test]
fn ends_with_quote_survives_hostile_entity_bounds() {
    // The quote whose length runs past the end of the text still ends the text.
    assert!(ends_with_quote(&hostile(EntityType::Blockquote)));
    assert!(ends_with_quote(&hostile(EntityType::Pre)));
    let only_far = TextWithEntities {
        text: "x".to_owned(),
        entities: vec![EntityInText::new(EntityType::Blockquote, i32::MAX, 2)],
    };
    // A quote that starts past the end of the text is not in the text at all.
    assert!(!ends_with_quote(&only_far));
}

#[test]
fn text_bridge_skips_hostile_entities() {
    let rich = entities_to_rich_text(&hostile(EntityType::Bold));
    assert_eq!(rich.text, "quoted text");
}

#[test]
fn first_url_is_found_with_clamped_bounds() {
    assert_eq!(
        extract_first_url("see https://example.org/x now").as_deref(),
        Some("https://example.org/x")
    );
}
