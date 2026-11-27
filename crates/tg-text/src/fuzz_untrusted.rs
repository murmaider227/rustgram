//! Panic fuzz over the entry points that take server or user text (TGB-3065).
//!
//! Deterministic (fixed seeds); `TGFORK_FUZZ_ITERATIONS` raises the iteration count.

use tg_fuzz::Fuzz;

use crate::entity::{EntityInText, EntityType, TextParseFlags, TextWithEntities};
use crate::tags::{TextTag, TextWithTags};

const ITERATIONS: usize = 20_000;

const TYPES: &[EntityType] = &[
    EntityType::Invalid,
    EntityType::Url,
    EntityType::CustomUrl,
    EntityType::Email,
    EntityType::Hashtag,
    EntityType::Cashtag,
    EntityType::Mention,
    EntityType::MentionName,
    EntityType::CustomEmoji,
    EntityType::BotCommand,
    EntityType::MediaTimestamp,
    EntityType::Colorized,
    EntityType::Phone,
    EntityType::BankCard,
    EntityType::TonAddress,
    EntityType::Bold,
    EntityType::Semibold,
    EntityType::Italic,
    EntityType::Underline,
    EntityType::StrikeOut,
    EntityType::Code,
    EntityType::Pre,
    EntityType::Blockquote,
    EntityType::Spoiler,
    EntityType::Subscript,
    EntityType::Superscript,
    EntityType::Marked,
    EntityType::FormattedDate,
];

const EXTRA: &[&str] = &[
    "1.2:3",
    "/start",
    "@bot",
    "internal:",
    "tg-emoji://",
    "1700000000:3",
];

fn bound(fuzz: &mut Fuzz, len: usize) -> i32 {
    fuzz.rng().int(len) as i32
}

fn gen_twe(fuzz: &mut Fuzz) -> TextWithEntities {
    let text = fuzz.text(EXTRA, 24);
    let len = crate::utf16::utf16_len(&text);
    let count = fuzz.rng().below(5);
    let entities = (0..count)
        .map(|_| {
            let data = if fuzz.rng().one_in(2) {
                String::new()
            } else {
                fuzz.text(EXTRA, 8)
            };
            let kind = *fuzz.rng().pick(TYPES);
            EntityInText::with_data(kind, bound(fuzz, len), bound(fuzz, len), data)
        })
        .collect();
    TextWithEntities { text, entities }
}

#[test]
fn string_entry_points_do_not_panic() {
    let mut fuzz = Fuzz::new(0x9E37_79B9_7F4A_7C15, ITERATIONS);
    for i in 0..fuzz.iterations() {
        let s = fuzz.text(EXTRA, 24);
        let flags = TextParseFlags::from_bits_truncate(fuzz.rng().next_u64() as u32);
        fuzz.check("parse_entities", i, &s, || {
            let _ = crate::parse::parse_entities(&s, flags);
        });
        fuzz.check("parse_markdown", i, &s, || {
            let _ = crate::markdown::parse_markdown(&s);
        });
        fuzz.check("url fns", i, &s, || {
            let _ = crate::url::validate_url(&s);
            let _ = crate::url::url_decode(&s);
            let _ = crate::url::encode_for_opening(&s);
            let _ = crate::url::strip_url_protocol(&s);
            let _ = crate::url::suspicious_ranges(&s);
            let _ = crate::url::is_suspicious(&s);
            let _ = crate::url::external_url_from_internal_url(&s);
            let _ = crate::url::is_email(&s);
            let _ = crate::url::url_append_query_or_hash(&s, &s);
        });
        fuzz.check("tags fns", i, &s, || {
            let _ = crate::tags::mention_entity_data(&s);
            let _ = crate::tags::custom_emoji_entity_data(&s);
            let _ = crate::tags::is_valid_markdown_link(&s);
            let _ = crate::tags::is_instant_view_anchor_link(&s);
        });
        fuzz.check("parse helpers", i, &s, || {
            let _ = crate::parse::single_line(&s);
            let _ = crate::parse::remove_accents(&s);
            let _ = crate::parse::remove_emoji(&s);
            let _ = crate::parse::name_sort_key(&s);
            let _ = crate::parse::prepare_search_words(&s);
            let _ = crate::parse::escape_for_rich_parsing(&s);
            let _ = crate::entity::deserialize_formatted_date_data(&s);
            let _ = crate::emoji::from_url(&s);
            let _ = crate::utilities::rich_lang_value(&s);
            let _ = crate::utilities::upper(&s);
            let _ = crate::utilities::capitalized(&s);
            let _ = crate::utilities::words(&s);
            let _ = crate::highlight::highlight_code_entities(&s, "rust");
            let _ = crate::highlight::highlight_code_entities(&s, "js");
            let _ = crate::markdown::read_pre_language_name(&s, 0, 3);
            let _ = crate::markdown::read_pre_language_name(&s, 2, 40);
        });
    }
    fuzz.finish();
}

#[test]
fn entity_entry_points_do_not_panic() {
    let mut fuzz = Fuzz::new(0xD1B5_4A32_D192_ED03, ITERATIONS);
    for i in 0..fuzz.iterations() {
        let twe = gen_twe(&mut fuzz);
        let flags = TextParseFlags::from_bits_truncate(fuzz.rng().next_u64() as u32);
        let limit = fuzz.rng().below(40);
        let position = fuzz.rng().below(40);
        fuzz.check("entity fns", i, &twe, || {
            let mut t = twe.clone();
            crate::parse::parse_entities_in_place(&mut t, flags);
            let mut t = twe.clone();
            crate::markdown::parse_markdown_in_place(&mut t);
            let mut t = twe.clone();
            crate::parse::prepare_for_sending(&mut t, flags);
            let mut t = twe.clone();
            crate::parse::apply_server_cleaning(&mut t);
            let mut t = twe.clone();
            crate::parse::trim(&mut t);
            let mut sending = twe.clone();
            let mut left = twe.clone();
            let _ = crate::parse::cut_part(&mut sending, &mut left, limit);
            let _ = crate::parse::single_line_with_entities(&twe);
            let _ = crate::utilities::mid(&twe, position, Some(limit));
            let _ = crate::utilities::mid(&twe, position, None);
            let _ = crate::utilities::filtered(&twe, &[EntityType::Bold, EntityType::Pre]);
            let mut t = twe.clone();
            let _ = crate::highlight::try_highlight_syntax(&mut t);
            let _ = crate::message_field::strip_support_hashtag(twe.clone());
            let _ = crate::message_field::prevent_inline_bot_query(twe.clone());
            let _ = crate::entity::merge_and_sort_entities(twe.entities.clone());
            let tags = crate::tags::convert_entities_to_text_tags(&twe.entities);
            let _ = crate::tags::convert_text_tags_to_entities(&tags);
            let _ = crate::tags::simplify_tags(tags.clone());
            let bytes = crate::tags::serialize_tags(&tags);
            let _ = crate::tags::deserialize_tags(&bytes, twe.utf16_len());
            let _ = crate::entity::TextForMimeData::with_expanded_links(&twe);
            let mut a = twe.clone();
            a.append(twe.clone());
        });
        let tags: Vec<TextTag> = twe
            .entities
            .iter()
            .map(|e| TextTag::new(e.offset(), e.length(), e.data().to_string()))
            .collect();
        let twt = TextWithTags {
            text: twe.text.clone(),
            tags: tags.clone(),
        };
        fuzz.check("tag fns", i, &twt, || {
            let _ = crate::markdown::apply_markdown(&twt);
            let _ = crate::tags::convert_text_tags_to_entities(&tags);
            let _ = crate::tags::simplify_tags(tags.clone());
            let bytes = crate::tags::serialize_tags(&tags);
            let _ = crate::tags::deserialize_tags(&bytes, twe.utf16_len());
        });
        let raw = if fuzz.rng().one_in(2) {
            let tags = crate::tags::convert_entities_to_text_tags(&twe.entities);
            fuzz.mutate(&crate::tags::serialize_tags(&tags))
        } else {
            fuzz.bytes(64)
        };
        let text_len = fuzz.rng().below(64);
        fuzz.check("deserialize_tags", i, &raw, || {
            let _ = crate::tags::deserialize_tags(&raw, text_len);
        });
    }
    fuzz.finish();
}

#[test]
fn link_tag_with_multibyte_char_across_byte_nine_does_not_panic() {
    // Old code sliced `trimmed[..9]` and panicked when byte 9 fell inside a character.
    let tags = [TextTag::new(0, 1, "abcdefgh\u{e9}x")];
    let entities = crate::tags::convert_text_tags_to_entities(&tags);
    assert!(entities.len() <= 1);
}

#[test]
fn hostile_entity_bounds_saturate_instead_of_overflowing() {
    let mut entity = EntityInText::new(EntityType::Bold, i32::MAX, 2);
    assert_eq!(entity.end(), i32::MAX);
    entity.update_text_end(10);
    assert_eq!((entity.offset(), entity.length()), (10, 0));
    let mut entity = EntityInText::new(EntityType::Bold, i32::MIN, 3);
    entity.shift_left(5);
    assert_eq!((entity.offset(), entity.length()), (0, 0));
    let tags = crate::tags::simplify_tags(vec![
        TextTag::new(0, i32::MAX, "**"),
        TextTag::new(1, i32::MAX, "**"),
    ]);
    assert_eq!(tags.len(), 1);
}
