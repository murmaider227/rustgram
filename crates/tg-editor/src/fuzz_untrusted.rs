//! Panic regression and fuzz tests over saved editor drafts and settings (TGB-3065).

use tg_fuzz::Fuzz;

#[test]
fn hex_argb_with_multibyte_char_does_not_panic() {
    // Old code sliced `hex[0..2]` on a 6-byte string such as `a\u{e9}bcd`.
    assert_eq!(crate::geometry::Color::from_hex_argb("a\u{e9}bcd"), None);
    assert_eq!(crate::geometry::Color::from_hex_argb("#abcdef\u{e9}"), None);
}

const TOKENS: &[&str] = &[
    "{",
    "}",
    "[",
    "]",
    ":",
    ",",
    "\"",
    "\"items\"",
    "\"type\"",
    "\"x\"",
    "\"y\"",
    "\"scale\"",
    "\"rotation\"",
    "\"flipped\"",
    "\"color\"",
    "\"points\"",
    "\"width\"",
    "\"text\"",
    "\"version\"",
    "\"crop\"",
    "\"angle\"",
    "\"paint\"",
    "-1",
    "1e400",
    "-0",
    "null",
    "true",
    "\"#ff00ff\"",
    "\"#\u{e9}a\"",
    "\"sticker\"",
    "\"line\"",
    "\"arrow\"",
    "00:01",
    "1:02:03",
    ".",
    "99999999999",
];

#[test]
fn drafts_and_settings_do_not_panic() {
    let mut fuzz = Fuzz::new(0xED17_0001, 3_000);
    let seed = crate::scene_draft::SceneDraft::default()
        .to_bytes()
        .unwrap_or_default();
    for i in 0..fuzz.iterations() {
        let text = fuzz.text(TOKENS, 24);
        let bytes = if fuzz.rng().one_in(2) {
            fuzz.mutate(&seed)
        } else {
            fuzz.bytes(64)
        };
        fuzz.check("editor drafts", i, &(&text, &bytes), || {
            let _ = crate::scene_draft::SceneDraft::from_json(&text);
            let _ = crate::scene_draft::SceneDraft::from_bytes(&bytes);
            let _ = crate::common::PhotoModifications::from_json(&text);
            let _ = crate::color_picker::BrushState::deserialize_tdesktop(&bytes);
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                let _ = crate::scene_draft::parse_scene_items_json(&value);
            }
            let _ = crate::geometry::Color::from_hex_argb(&text);
            let _ = crate::video::timeline::parse_stamp(&text);
        });
    }
    fuzz.finish();
}

#[test]
fn typed_stamp_with_multibyte_fraction_does_not_panic() {
    // Old code sliced `frac_str[..3]` by bytes, so `0:01.55\u{e9}` panicked.
    assert_eq!(crate::video::timeline::parse_stamp("0:01.55\u{e9}"), None);
    assert_eq!(
        crate::video::timeline::parse_stamp("0:01.500\u{e9}"),
        Some(1_500)
    );
}
