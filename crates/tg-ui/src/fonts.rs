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
//!
//! Monospace: tdesktop does not bundle a monospace font. `MonospaceFont()` in
//! `style_core_font.cpp` asks the system for "Cascadia Mono", "Consolas", "Liberation Mono",
//! "Menlo", "Courier" in that order (Windows always prefers that list). The app does the same
//! after [`enable_system_monospace`]: it reads the first of those files the system has and
//! never ships it. Hack stays as the bundled stand-in for systems without any of them and for
//! deterministic test renders.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

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

/// `font_data` key of the system monospace face resolved by [`resolve_system_monospace`].
pub const SYSTEM_MONOSPACE_KEY: &str = "system_monospace";

/// tdesktop's `ManualMonospaceFont()` try-list (`style_core_font.cpp`), in order.
pub const MONOSPACE_TRY_FIRST: [&str; 5] = [
    "Cascadia Mono",
    "Consolas",
    "Liberation Mono",
    "Menlo",
    "Courier",
];

static SYSTEM_MONOSPACE_ENABLED: AtomicBool = AtomicBool::new(false);

/// Let [`build_tdesktop_font_definitions`] use the system monospace font like tdesktop.
///
/// Called once by the app binary before the window opens. Tests leave it off so their
/// renders do not depend on the fonts of the machine they run on.
pub fn enable_system_monospace() {
    SYSTEM_MONOSPACE_ENABLED.store(true, Ordering::Relaxed);
}

/// A monospace face read from the system font directory.
#[derive(Debug, Clone)]
pub struct SystemMonospace {
    /// Family name from [`MONOSPACE_TRY_FIRST`].
    pub family: &'static str,
    /// File the face was read from.
    pub path: PathBuf,
    /// Face index inside the file (`.ttc` collections).
    pub index: u32,
    /// Raw font file bytes.
    pub bytes: Arc<Vec<u8>>,
}

/// Candidate files for [`MONOSPACE_TRY_FIRST`] on this platform, in try order.
#[must_use]
pub fn monospace_candidate_files() -> Vec<(&'static str, PathBuf, u32)> {
    let mut out = Vec::new();
    if cfg!(windows) {
        let mut dirs = Vec::new();
        if let Some(win) = std::env::var_os("WINDIR").or_else(|| std::env::var_os("SystemRoot")) {
            dirs.push(PathBuf::from(win).join("Fonts"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(
                PathBuf::from(local)
                    .join("Microsoft")
                    .join("Windows")
                    .join("Fonts"),
            );
        }
        let files: [(&'static str, &str); 4] = [
            ("Cascadia Mono", "CascadiaMono.ttf"),
            ("Consolas", "consola.ttf"),
            ("Liberation Mono", "LiberationMono-Regular.ttf"),
            ("Courier", "cour.ttf"),
        ];
        for (family, file) in files {
            for dir in &dirs {
                out.push((family, dir.join(file), 0));
            }
        }
    } else if cfg!(target_os = "macos") {
        out.push(("Menlo", PathBuf::from("/System/Library/Fonts/Menlo.ttc"), 0));
        out.push((
            "Courier",
            PathBuf::from("/System/Library/Fonts/Courier.ttc"),
            0,
        ));
    }
    // Linux: tdesktop prefers the fontconfig monospace font there; without fontconfig the
    // bundled Hack (a DejaVu Sans Mono derivative, the usual Linux default) stands in.
    out
}

/// `true` if `bytes` starts like a TrueType / OpenType font or collection.
#[must_use]
pub fn looks_like_font_file(bytes: &[u8]) -> bool {
    bytes.len() > 1024
        && matches!(
            bytes.get(..4),
            Some([0, 1, 0, 0] | b"OTTO" | b"true" | b"ttcf")
        )
}

/// Resolve tdesktop's monospace family from the system (first readable file of
/// [`monospace_candidate_files`]). Cached for the process.
#[must_use]
pub fn resolve_system_monospace() -> Option<SystemMonospace> {
    static CACHE: OnceLock<Option<SystemMonospace>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            monospace_candidate_files()
                .into_iter()
                .find_map(|(family, path, index)| {
                    let bytes = std::fs::read(&path).ok()?;
                    looks_like_font_file(&bytes).then(|| SystemMonospace {
                        family,
                        path,
                        index,
                        bytes: Arc::new(bytes),
                    })
                })
        })
        .clone()
}

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

/// Monospace font family (system monospace if enabled, then `Hack-Regular`, then
/// `OpenSans-Regular` for scripts the monospace faces lack).
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
///
/// After [`enable_system_monospace`] the monospace family starts with the system face
/// tdesktop would pick ([`resolve_system_monospace`]).
#[must_use]
pub fn build_tdesktop_font_definitions() -> FontDefinitions {
    let system_mono = if SYSTEM_MONOSPACE_ENABLED.load(Ordering::Relaxed) {
        resolve_system_monospace()
    } else {
        None
    };
    build_font_definitions_with(system_mono.as_ref())
}

/// [`build_tdesktop_font_definitions`] with an explicit system monospace face.
#[must_use]
pub fn build_font_definitions_with(system_mono: Option<&SystemMonospace>) -> FontDefinitions {
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
    // Include OpenSans-Regular after the monospace faces so Cyrillic and extended Unicode
    // render cleanly inside inline code and `<pre>` blocks.
    let mut mono = Vec::new();
    if let Some(sys) = system_mono {
        defs.font_data.insert(
            SYSTEM_MONOSPACE_KEY.to_owned(),
            Arc::new(FontData::from_blob(sys.bytes.clone(), sys.index)),
        );
        mono.push(SYSTEM_MONOSPACE_KEY.to_owned());
    }
    mono.push("hack_regular".to_owned());
    mono.push("open_sans_regular".to_owned());
    defs.families
        .insert(FontFamily::Monospace, with_fallbacks(mono));

    defs
}

/// Gamma of the glyph coverage curve for light palettes (dark text on light backgrounds).
///
/// tdesktop draws text through Qt's GDI font engine (`core/launcher.cpp`,
/// `windows:fontengine=gdi`), which blends glyphs with Windows' ClearType contrast
/// (`SPI_GETFONTSMOOTHINGCONTRAST`, default 1200, read by Qt as gamma 1.2). egui blends
/// coverage linearly in gamma space, which leaves dark strokes heavier; `alpha = coverage^1.2`
/// brings Open Sans 13 / 19 px regular and semibold to within 7.4 % of tdesktop's ink per pixel
/// (TGB-3014, measured against GDI renders that match an official 7.4.6 capture exactly).
pub const LIGHT_TEXT_COVERAGE_GAMMA: f32 = 1.2;

/// How glyph coverage turns into alpha for a light (`dark_mode == false`) or dark palette.
///
/// egui's own dark default (`2c - c^2`) thickens light-on-dark strokes by 18-30 % against
/// tdesktop's night themes; linear coverage stays within 7.1 % (TGB-3014).
#[must_use]
pub fn tdesktop_font_color_transfer(dark_mode: bool) -> egui::epaint::FontColorTransferFunction {
    if dark_mode {
        egui::epaint::FontColorTransferFunction::Off
    } else {
        egui::epaint::FontColorTransferFunction::Gamma(LIGHT_TEXT_COVERAGE_GAMMA)
    }
}

/// Set tdesktop's text stroke weight ([`tdesktop_font_color_transfer`]) on `visuals`.
///
/// Every `Visuals` given to a whole `egui::Context` must go through this: the glyph atlas is
/// shared by all viewports and is rebuilt whenever the context's text options change.
pub fn apply_tdesktop_text_rendering(visuals: &mut egui::Visuals) {
    visuals.text_options.color_transfer_function = tdesktop_font_color_transfer(visuals.dark_mode);
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

#[cfg(test)]
#[path = "fonts_tests.rs"]
mod fonts_tests;
