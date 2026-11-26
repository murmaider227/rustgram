//! Font resolution tests: which face egui picks per script, weight and monospace.
//!
//! tdesktop draws Latin, Cyrillic (Russian and Ukrainian) and digits with the bundled Open Sans
//! (`style_core_font.cpp` `ResolveFont`: family "Open Sans", `FontFlag::Bold` -> `DemiBold`) and
//! falls back to the system only for scripts Open Sans lacks.

use std::sync::Arc;

use egui::epaint::text::{Fonts, TextOptions};
use egui::{Color32, FontData, FontFamily, FontId};
use tg_style::FontFlags;

use super::*;

const RUSSIAN: &str = "АБВГДЕЁЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдеёжзийклмнопрстуфхцчшщъыьэюя";
const UKRAINIAN: &str = "ҐЄІЇґєії’";
const LATIN_DIGITS: &str = "ABCXYZabcxyz0123456789";

fn fonts_from(defs: egui::FontDefinitions) -> Fonts {
    Fonts::new(TextOptions::default(), defs)
}

/// First font in `family`'s chain that has `c` (the face egui draws `c` with).
fn resolved_face(fonts: &mut Fonts, family: &FontFamily, c: char) -> Option<String> {
    fonts
        .with_pixels_per_point(1.0)
        .characters(family)
        .get(&c)
        .and_then(|names| names.first().cloned())
}

#[test]
fn cyrillic_and_ukrainian_resolve_to_open_sans_in_every_weight() {
    let mut fonts = fonts_from(build_font_definitions_with(None));
    let cases = [
        (FontFlags::NONE, "open_sans_regular"),
        (FontFlags::SEMIBOLD, "open_sans_semibold"),
        // tdesktop maps `FontFlag::Bold` to `QFont::DemiBold`: bold text is Open Sans SemiBold.
        (FontFlags::BOLD, "open_sans_semibold"),
        (FontFlags::ITALIC, "open_sans_italic"),
        (
            FontFlags::BOLD.union(FontFlags::ITALIC),
            "open_sans_semibold_italic",
        ),
    ];
    for (flags, expected) in cases {
        let family = font_family_for_flags(flags);
        for c in RUSSIAN
            .chars()
            .chain(UKRAINIAN.chars())
            .chain(LATIN_DIGITS.chars())
        {
            let face = resolved_face(&mut fonts, &family, c);
            assert_eq!(
                face.as_deref(),
                Some(expected),
                "{c:?} with flags {:#04x} must be drawn by {expected}",
                flags.bits()
            );
        }
        println!(
            "measured: flags {:#04x} -> Cyrillic/Ukrainian/Latin/digits all in {expected}",
            flags.bits()
        );
    }
}

#[test]
fn script_widths_match_open_sans_alone() {
    // Measured check: a family holding ONLY Open Sans gives the same widths per script as the
    // app's families, so no fallback face is involved anywhere in these strings.
    let mut defs = build_font_definitions_with(None);
    for (key, bytes) in [
        ("only_regular", OPEN_SANS_REGULAR_BYTES),
        ("only_semibold", OPEN_SANS_SEMIBOLD_BYTES),
    ] {
        defs.font_data
            .insert(key.to_owned(), Arc::new(FontData::from_static(bytes)));
        defs.families
            .insert(FontFamily::Name(key.into()), vec![key.to_owned()]);
    }
    let mut fonts = fonts_from(defs);
    let samples = [
        ("ru", "Новая покупка Premium!"),
        ("ua", "Ґанок, їжак, єнот і ґудзик"),
        ("latin", "Premium plan purchased"),
        ("digits", "23.11.2026 03:10 UTC"),
    ];
    for (family, only) in [
        (family_regular(), "only_regular"),
        (family_semibold(), "only_semibold"),
    ] {
        for (script, text) in samples {
            let mut view = fonts.with_pixels_per_point(1.0);
            let app_w = view
                .layout_no_wrap(
                    text.to_owned(),
                    FontId::new(13.0, family.clone()),
                    Color32::BLACK,
                )
                .size()
                .x;
            let only_w = view
                .layout_no_wrap(
                    text.to_owned(),
                    FontId::new(13.0, FontFamily::Name(only.into())),
                    Color32::BLACK,
                )
                .size()
                .x;
            println!(
                "measured: {script:6} {only:13} 13px width {app_w:.2} (Open Sans only {only_w:.2})"
            );
            assert!(
                (app_w - only_w).abs() < 0.01,
                "{script} text in {family:?} is not pure Open Sans: {app_w} vs {only_w}"
            );
        }
    }
}

#[test]
fn monospace_uses_system_face_first_then_hack_then_open_sans() {
    let fake = SystemMonospace {
        family: "Consolas",
        path: "consola.ttf".into(),
        index: 0,
        bytes: Arc::new(HACK_REGULAR_BYTES.to_vec()),
    };
    let defs = build_font_definitions_with(Some(&fake));
    let chain = &defs.families[&FontFamily::Monospace];
    assert_eq!(
        chain.get(..3),
        Some(
            &[
                SYSTEM_MONOSPACE_KEY.to_owned(),
                "hack_regular".to_owned(),
                "open_sans_regular".to_owned(),
            ][..]
        )
    );
    assert!(defs.font_data.contains_key(SYSTEM_MONOSPACE_KEY));

    // Without a system face the bundled Hack leads (deterministic test renders).
    let plain = build_font_definitions_with(None);
    assert_eq!(
        plain.families[&FontFamily::Monospace]
            .first()
            .map(String::as_str),
        Some("hack_regular")
    );
    let mut fonts = fonts_from(plain);
    for c in "0123456789abcdefABCDEF{}();".chars() {
        assert_eq!(
            resolved_face(&mut fonts, &FontFamily::Monospace, c).as_deref(),
            Some("hack_regular"),
            "{c:?}"
        );
    }
    for c in RUSSIAN.chars().chain(UKRAINIAN.chars()) {
        let face = resolved_face(&mut fonts, &FontFamily::Monospace, c);
        assert!(
            matches!(face.as_deref(), Some("hack_regular" | "open_sans_regular")),
            "{c:?} in code spans resolved to {face:?}"
        );
    }
}

#[test]
fn system_monospace_follows_tdesktop_try_list() {
    let order: Vec<&str> = monospace_candidate_files()
        .iter()
        .map(|(family, _, _)| *family)
        .collect();
    let mut last = 0;
    for family in &order {
        let pos = MONOSPACE_TRY_FIRST
            .iter()
            .position(|f| f == family)
            .unwrap_or(usize::MAX);
        assert!(pos >= last && pos != usize::MAX, "{family} out of order");
        last = pos;
    }
    match resolve_system_monospace() {
        Some(sys) => {
            assert!(MONOSPACE_TRY_FIRST.contains(&sys.family));
            assert!(looks_like_font_file(&sys.bytes));
            println!(
                "measured: system monospace -> {} ({})",
                sys.family,
                sys.path.display()
            );
        }
        None => println!("measured: no system monospace face, bundled Hack stays"),
    }
    assert!(!looks_like_font_file(b"not a font"));
    assert!(looks_like_font_file(OPEN_SANS_REGULAR_BYTES));
}
