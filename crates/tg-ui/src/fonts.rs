//! Bundled Telegram Desktop fonts (Open Sans + Hack monospace) and `egui` font registration.
//!
//! Port of font loading and family selection from `Telegram/lib_ui/fonts/fonts.qrc`
//! and `Telegram/lib_ui/ui/style/style_core_font.cpp`.
//!
//! # Licenses (GPL-3.0 compatible)
//! - `OpenSans-Regular.ttf`, `OpenSans-SemiBold.ttf`, `OpenSans-Italic.ttf`,
//!   `OpenSans-SemiBoldItalic.ttf`: SIL Open Font License 1.1 (copied from tdesktop
//!   `Telegram/lib_ui/fonts/`, includes full Latin and Cyrillic coverage).
//! - `Hack-Regular.ttf`: MIT + Bitstream Vera license (see `assets/fonts/Hack-Regular.txt`),
//!   with `OpenSans-Regular` configured as fallback so Cyrillic renders in monospace spans.

use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily, FontId, Id, TextStyle as EguiTextStyle};
use tg_style::{Font, FontFlags, Scale, scale as current_scale, st};

/// Open Sans Regular TTF bytes (SIL OFL 1.1, includes Latin & Cyrillic).
pub const OPEN_SANS_REGULAR_BYTES: &[u8] = include_bytes!("../assets/fonts/OpenSans-Regular.ttf");
/// Open Sans SemiBold TTF bytes (SIL OFL 1.1, used for both semibold & bold in tdesktop).
pub const OPEN_SANS_SEMIBOLD_BYTES: &[u8] = include_bytes!("../assets/fonts/OpenSans-SemiBold.ttf");
/// Open Sans Italic TTF bytes (SIL OFL 1.1).
pub const OPEN_SANS_ITALIC_BYTES: &[u8] = include_bytes!("../assets/fonts/OpenSans-Italic.ttf");
/// Open Sans SemiBold Italic TTF bytes (SIL OFL 1.1).
pub const OPEN_SANS_SEMIBOLD_ITALIC_BYTES: &[u8] =
    include_bytes!("../assets/fonts/OpenSans-SemiBoldItalic.ttf");
/// Hack Regular monospace TTF bytes (MIT + Bitstream Vera).
pub const HACK_REGULAR_BYTES: &[u8] = include_bytes!("../assets/fonts/Hack-Regular.ttf");

/// Custom `egui` font family name for `OpenSans-SemiBold` (bold/semibold).
pub const FAMILY_NAME_SEMIBOLD: &str = "tg_semibold";
/// Custom `egui` font family name for `OpenSans-Italic`.
pub const FAMILY_NAME_ITALIC: &str = "tg_italic";
/// Custom `egui` font family name for `OpenSans-SemiBoldItalic`.
pub const FAMILY_NAME_SEMIBOLD_ITALIC: &str = "tg_semibold_italic";

/// Return all embedded font names and raw TTF byte slices.
#[must_use]
pub fn embedded_font_bytes() -> [(&'static str, &'static [u8]); 5] {
    [
        ("open_sans_regular", OPEN_SANS_REGULAR_BYTES),
        ("open_sans_semibold", OPEN_SANS_SEMIBOLD_BYTES),
        ("open_sans_italic", OPEN_SANS_ITALIC_BYTES),
        ("open_sans_semibold_italic", OPEN_SANS_SEMIBOLD_ITALIC_BYTES),
        ("hack_regular", HACK_REGULAR_BYTES),
    ]
}

/// Regular proportional font family (`OpenSans-Regular`).
#[must_use]
pub fn family_regular() -> FontFamily {
    FontFamily::Proportional
}

/// Semibold/bold proportional font family (`OpenSans-SemiBold`).
#[must_use]
pub fn family_semibold() -> FontFamily {
    FontFamily::Name(Arc::from(FAMILY_NAME_SEMIBOLD))
}

/// Italic proportional font family (`OpenSans-Italic`).
#[must_use]
pub fn family_italic() -> FontFamily {
    FontFamily::Name(Arc::from(FAMILY_NAME_ITALIC))
}

/// Semibold/bold italic proportional font family (`OpenSans-SemiBoldItalic`).
#[must_use]
pub fn family_semibold_italic() -> FontFamily {
    FontFamily::Name(Arc::from(FAMILY_NAME_SEMIBOLD_ITALIC))
}

/// Monospace font family (`Hack-Regular` with `OpenSans-Regular` Cyrillic fallback).
#[must_use]
pub fn family_monospace() -> FontFamily {
    FontFamily::Monospace
}

/// Choose the matching `egui::FontFamily` for tdesktop [`FontFlags`].
#[must_use]
pub fn font_family_for_flags(flags: FontFlags) -> FontFamily {
    if flags.is_monospace() {
        family_monospace()
    } else if flags.is_bold() && flags.is_italic() {
        family_semibold_italic()
    } else if flags.is_bold() {
        family_semibold()
    } else if flags.is_italic() {
        family_italic()
    } else {
        family_regular()
    }
}

/// Build an `egui::FontId` from a pixel size and tdesktop [`FontFlags`].
#[must_use]
pub fn font_id_for_flags(size: f32, flags: FontFlags) -> FontId {
    FontId::new(size.max(1.0), font_family_for_flags(flags))
}

/// Convert a generated `tg_style::Font` into an `egui::FontId` at the current global DPI scale.
#[must_use]
pub fn font_id_from_style(font: Font) -> FontId {
    font_id_from_style_at(font, current_scale())
}

/// Convert a generated `tg_style::Font` into an `egui::FontId` at an explicit [`Scale`].
#[must_use]
pub fn font_id_from_style_at(font: Font, target_scale: Scale) -> FontId {
    let px = font.scaled_size_at(target_scale).max(1) as f32;
    font_id_for_flags(px, font.flags)
}

/// Build `egui::FontDefinitions` populated with Open Sans (Regular, SemiBold, Italic,
/// SemiBoldItalic), Hack monospace, and egui's built-in emoji/icon fallbacks.
#[must_use]
pub fn build_tdesktop_font_definitions() -> FontDefinitions {
    let mut defs = FontDefinitions::default();

    defs.font_data.insert(
        "open_sans_regular".to_owned(),
        Arc::new(FontData::from_static(OPEN_SANS_REGULAR_BYTES)),
    );
    defs.font_data.insert(
        "open_sans_semibold".to_owned(),
        Arc::new(FontData::from_static(OPEN_SANS_SEMIBOLD_BYTES)),
    );
    defs.font_data.insert(
        "open_sans_italic".to_owned(),
        Arc::new(FontData::from_static(OPEN_SANS_ITALIC_BYTES)),
    );
    defs.font_data.insert(
        "open_sans_semibold_italic".to_owned(),
        Arc::new(FontData::from_static(OPEN_SANS_SEMIBOLD_ITALIC_BYTES)),
    );
    defs.font_data.insert(
        "hack_regular".to_owned(),
        Arc::new(FontData::from_static(HACK_REGULAR_BYTES)),
    );

    // Collect emoji / symbol fallback fonts that already exist in FontDefinitions::default().
    let mut fallbacks: Vec<String> = Vec::new();
    for name in ["NotoEmoji-Regular", "emoji-icon-font"] {
        if defs.font_data.contains_key(name) {
            fallbacks.push(name.to_owned());
        }
    }

    let with_fallbacks = |mut primary: Vec<String>| -> Vec<String> {
        primary.extend(fallbacks.iter().cloned());
        primary
    };

    defs.families.insert(
        FontFamily::Proportional,
        with_fallbacks(vec!["open_sans_regular".to_owned()]),
    );
    defs.families.insert(
        family_semibold(),
        with_fallbacks(vec![
            "open_sans_semibold".to_owned(),
            "open_sans_regular".to_owned(),
        ]),
    );
    defs.families.insert(
        family_italic(),
        with_fallbacks(vec![
            "open_sans_italic".to_owned(),
            "open_sans_regular".to_owned(),
        ]),
    );
    defs.families.insert(
        family_semibold_italic(),
        with_fallbacks(vec![
            "open_sans_semibold_italic".to_owned(),
            "open_sans_semibold".to_owned(),
            "open_sans_italic".to_owned(),
            "open_sans_regular".to_owned(),
        ]),
    );
    // Include OpenSans-Regular after Hack-Regular so Cyrillic and extended Unicode
    // render cleanly inside inline code and `<pre>` blocks.
    defs.families.insert(
        FontFamily::Monospace,
        with_fallbacks(vec![
            "hack_regular".to_owned(),
            "open_sans_regular".to_owned(),
        ]),
    );

    defs
}

/// Register Telegram Desktop's fonts and default `egui::TextStyle` sizes on `ctx`.
pub fn register_tdesktop_fonts(ctx: &egui::Context) {
    ctx.set_fonts(build_tdesktop_font_definitions());
    apply_tdesktop_text_styles(ctx);
    ctx.data_mut(|d| d.insert_temp(fonts_registered_id(), current_scale().percent()));
}

/// Apply tdesktop's scaled `st::normal_font`, `st::box_title_font`, and monospace sizes
/// to `ctx`'s default `egui::Style::text_styles` for both light and dark themes.
pub fn apply_tdesktop_text_styles(ctx: &egui::Context) {
    let normal_px = st::normal_font.scaled_size().max(1) as f32;
    let heading_px = st::box_title_font.scaled_size().max(1) as f32;
    let small_px = (normal_px - 2.0).max(10.0);

    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        ctx.style_mut_of(theme, |style| {
            style.text_styles.insert(
                EguiTextStyle::Body,
                FontId::new(normal_px, family_regular()),
            );
            style.text_styles.insert(
                EguiTextStyle::Button,
                FontId::new(normal_px, family_semibold()),
            );
            style.text_styles.insert(
                EguiTextStyle::Small,
                FontId::new(small_px, family_regular()),
            );
            style.text_styles.insert(
                EguiTextStyle::Heading,
                FontId::new(heading_px, family_semibold()),
            );
            style.text_styles.insert(
                EguiTextStyle::Monospace,
                FontId::new(normal_px, family_monospace()),
            );
        });
    }
}

/// Register tdesktop fonts once per `egui::Context` (and refresh sizes if DPI scale changed).
pub fn ensure_tdesktop_fonts(ctx: &egui::Context) {
    let key = fonts_registered_id();
    let cur = current_scale().percent();
    let existing = ctx.data_mut(|d| d.get_temp::<i32>(key));
    match existing {
        None => register_tdesktop_fonts(ctx),
        Some(prev) if prev != cur => {
            apply_tdesktop_text_styles(ctx);
            ctx.data_mut(|d| d.insert_temp(key, cur));
        }
        Some(_) => {}
    }
}

fn fonts_registered_id() -> Id {
    Id::unique("tg_ui_fonts_registered_scale")
}
