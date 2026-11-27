use tg_text::{EntityInText, EntityType, TextWithEntities};

use super::{slice_text_with_entities, text_with_entities_to_markdown};

fn text_with(entities: Vec<EntityInText>) -> TextWithEntities {
    TextWithEntities {
        text: "hello world".to_owned(),
        entities,
    }
}

#[test]
fn markdown_skips_entity_whose_end_overflows_i32() {
    let source = text_with(vec![EntityInText::new(EntityType::Bold, i32::MAX, 2)]);
    assert_eq!(text_with_entities_to_markdown(&source), "hello world");
}

#[test]
fn markdown_skips_negative_and_oversized_entities() {
    let source = text_with(vec![
        EntityInText::new(EntityType::Bold, -5, 10),
        EntityInText::new(EntityType::Italic, 3, i32::MAX),
        EntityInText::new(EntityType::Code, i32::MIN, i32::MAX),
    ]);
    assert_eq!(text_with_entities_to_markdown(&source), "hello world");
}

#[test]
fn markdown_keeps_valid_entity_next_to_hostile_one() {
    let source = text_with(vec![
        EntityInText::new(EntityType::Bold, 0, 5),
        EntityInText::new(EntityType::Italic, i32::MAX, 2),
    ]);
    assert_eq!(text_with_entities_to_markdown(&source), "**hello** world");
}

#[test]
fn slice_clamps_entity_whose_end_overflows_i32() {
    let source = text_with(vec![
        EntityInText::new(EntityType::Bold, i32::MAX, 2),
        EntityInText::new(EntityType::Italic, 6, i32::MAX),
    ]);
    let sliced = slice_text_with_entities(&source, 4, 9);
    assert_eq!(sliced.text, "o wor");
    assert_eq!(sliced.entities.len(), 1);
    assert_eq!(sliced.entities[0].entity_type(), EntityType::Italic);
    assert_eq!(sliced.entities[0].offset(), 2);
    assert_eq!(sliced.entities[0].length(), 3);
}
