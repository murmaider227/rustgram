//! TGB-3054: which messages are drawn as large emoji, and the outlined image.

use tg_text::{EntityInText, EntityType, TextWithEntities};

use super::*;

fn count(text: &str) -> Option<usize> {
    match large_emoji_text(&TextWithEntities::simple(text))? {
        LargeEmoji::Isolated(found) => Some(found.len()),
        LargeEmoji::OnlyCustom(_) => None,
    }
}

fn isolated_emoji(text: &TextWithEntities) -> Option<IsolatedEmoji> {
    match large_emoji_text(text)? {
        LargeEmoji::Isolated(found) => Some(found),
        LargeEmoji::OnlyCustom(_) => None,
    }
}

/// `text` where each `(utf16 offset, utf16 length, document id)` is a custom emoji.
fn with_custom(text: &str, customs: &[(i32, i32, &str)]) -> TextWithEntities {
    TextWithEntities {
        text: text.to_owned(),
        entities: customs
            .iter()
            .map(|&(offset, length, id)| {
                EntityInText::with_data(EntityType::CustomEmoji, offset, length, id)
            })
            .collect(),
    }
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
fn only_custom_emoji_are_drawn_large_in_lines() {
    // TGB-3097: `Ui::Text::OnlyCustomEmoji`, any number per line, checked before isolated.
    let one = with_custom("\u{1F600}", &[(0, 2, "11")]);
    assert_eq!(
        large_emoji_text(&one),
        Some(LargeEmoji::OnlyCustom(vec![vec!["11".to_owned()]]))
    );
    let text = "\u{1F600}\u{1F600}\u{1F600}\u{1F600}\n\u{1F525}";
    let lines = with_custom(
        text,
        &[
            (0, 2, "1"),
            (2, 2, "2"),
            (4, 2, "3"),
            (6, 2, "4"),
            (9, 2, "5"),
        ],
    );
    let Some(LargeEmoji::OnlyCustom(found)) = large_emoji_text(&lines) else {
        panic!("only custom emoji");
    };
    assert_eq!(found, [vec!["1", "2", "3", "4"], vec!["5"]]);
    // A custom emoji next to a regular one is an isolated large emoji with a custom item.
    let mixed = with_custom("\u{1F600}\u{1F525}", &[(2, 2, "7")]);
    let Some(LargeEmoji::Isolated(items)) = large_emoji_text(&mixed) else {
        panic!("mixed isolated emoji");
    };
    assert!(matches!(items[0], LargeEmojiItem::Emoji(_)));
    assert_eq!(items[1], LargeEmojiItem::Custom("7".to_owned()));
    // Four items with a regular one, a newline with a regular one, or a word: text again.
    let four = with_custom("\u{1F600}\u{1F600}\u{1F600}\u{1F600}", &[(0, 2, "1")]);
    assert_eq!(large_emoji_text(&four), None);
    let two_lines = with_custom("\u{1F600}\n\u{1F525}", &[(0, 2, "1")]);
    assert_eq!(large_emoji_text(&two_lines), None);
    let word = with_custom("\u{1F600} ok", &[(0, 2, "1")]);
    assert_eq!(large_emoji_text(&word), None);
    // Overlapping or out-of-range entities from the server never panic.
    let hostile = with_custom("\u{1F600}", &[(0, 2, "1"), (1, 9, "2"), (i32::MAX, 2, "3")]);
    assert_eq!(large_emoji_text(&hostile), None);
}

#[test]
fn links_spoilers_and_code_keep_the_bubble() {
    let with = |kind: EntityType| TextWithEntities {
        text: "\u{1F600}".to_owned(),
        entities: vec![EntityInText::new(kind, 0, 2)],
    };
    assert!(isolated_emoji(&with(EntityType::Bold)).is_some());
    for kind in [
        EntityType::Spoiler,
        EntityType::CustomUrl,
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

#[test]
fn colored_matches_tdesktop_images_colored() {
    // TGB-3100: `Images::Colored` with `msgStickerOverlay` (#358cd47f) on premultiplied pixels.
    let overlay = Color32::from_rgba_unmultiplied(0x35, 0x8C, 0xD4, 0x7F);
    let mut image = ColorImage::new(
        [3, 1],
        vec![
            Color32::from_rgb(255, 0, 0),
            Color32::TRANSPARENT,
            Color32::from_rgba_premultiplied(64, 64, 64, 128),
        ],
    );
    colored(&mut image, overlay);
    // ((256 - 127) * 256 * p + (a + 1) * c * 128) >> 16.
    assert_eq!(
        image.pixels[0],
        Color32::from_rgba_premultiplied(154, 70, 106, 255)
    );
    assert_eq!(image.pixels[1], Color32::TRANSPARENT);
    assert_eq!(
        image.pixels[2],
        Color32::from_rgba_premultiplied(45, 67, 85, 128)
    );
}
