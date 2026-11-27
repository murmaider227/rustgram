//! TGB-3054: which messages are drawn as large emoji, and the outlined image.

use tg_text::{EntityInText, EntityType, TextWithEntities};

use super::*;

fn count(text: &str) -> Option<usize> {
    isolated_emoji(&TextWithEntities::simple(text)).map(|found| found.len())
}

#[test]
fn one_to_three_emoji_are_isolated() {
    assert_eq!(count("\u{1F600}"), Some(1));
    assert_eq!(count("\u{1F600}\u{1F44D}\u{1F3FD}"), Some(2));
    assert_eq!(
        count("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}"),
        Some(1)
    );
    assert_eq!(
        count("\u{2764}\u{FE0F}\u{1F525}\u{1F1FA}\u{1F1E6}"),
        Some(3)
    );
    // Spaces after an emoji join its block (`BlockParser::parseCurrentChar`).
    assert_eq!(count("\u{1F600} \u{1F600}  \u{1F600} "), Some(3));
}

#[test]
fn other_text_is_not_isolated() {
    assert_eq!(count(""), None);
    assert_eq!(count("\u{1F600}\u{1F600}\u{1F600}\u{1F600}"), None);
    assert_eq!(count("hi \u{1F600}"), None);
    assert_eq!(count("\u{1F600}!"), None);
    assert_eq!(count(" \u{1F600}"), None);
    assert_eq!(count("\u{1F600}\n\u{1F600}"), None);
    assert_eq!(count("1"), None);
}

#[test]
fn links_spoilers_and_custom_emoji_keep_the_bubble() {
    let with = |kind: EntityType| TextWithEntities {
        text: "\u{1F600}".to_owned(),
        entities: vec![EntityInText::new(kind, 0, 2)],
    };
    assert!(isolated_emoji(&with(EntityType::Bold)).is_some());
    for kind in [
        EntityType::Spoiler,
        EntityType::CustomUrl,
        EntityType::CustomEmoji,
        EntityType::Code,
        EntityType::Blockquote,
    ] {
        assert!(isolated_emoji(&with(kind)).is_none(), "{kind:?}");
    }
}

#[test]
fn outlined_image_has_a_white_outline_ring() {
    let (emoji, _) = tg_text::emoji::find_emoji("\u{1F600}", 0).expect("grinning face");
    let (side, outline) = large_emoji_pixel_sizes(1.0);
    assert_eq!((side, outline), (36, 1));
    assert_eq!(large_emoji_pixel_sizes(1.5), (54, 2));
    let image = outlined_emoji_image(emoji, side, outline).expect("sheet has the emoji");
    assert_eq!(image.size, [38, 38]);
    // The yellow face's centre is opaque and not white.
    let centre = image.pixels[19 * 38 + 19];
    assert_eq!(centre.a(), 255);
    assert!(centre.b() < 200, "{centre:?}");
    // The outline: pixels the emoji itself leaves empty, covered now, are white.
    let plain = emoji_pixels(emoji, side).expect("sheet has the emoji");
    let mut ring = 0;
    for y in 0..38usize {
        for x in 0..38usize {
            let inside = (1..37).contains(&x) && (1..37).contains(&y);
            let own_alpha = if inside {
                plain[(y - 1) * 36 + x - 1][3]
            } else {
                0.0
            };
            let px = image.pixels[y * 38 + x];
            if own_alpha == 0.0 && px.a() > 0 {
                ring += 1;
                assert!(
                    px.r() == px.a() && px.g() == px.a() && px.b() == px.a(),
                    "{px:?}"
                );
            }
        }
    }
    assert!(ring > 20, "{ring} outline pixels");
}
