//! End-to-end integration tests for `tg-theme`.

use image::{Rgba, RgbaImage};
use tg_theme::{
    BubblesReadabilityArgs, Color, Colorizer, EmbeddedType, NightModeState, PALETTE_CHECKSUM,
    PALETTE_COUNT, PALETTE_ENTRY_COUNT, Palette, ParsedTheme, WallPaper, WallPaperFlags,
    colorize_in_content, colorize_named, colorizer_from, count_contrast, darken_under_white_text,
    default_accent_colors, embedded_scheme, embedded_themes, ensure_bubbles_readable,
    export_palette_for_editor, generate_background_image, generate_complex_gradient,
    generate_default_palette, generate_dithered_gradient, generate_linear_gradient,
    load_color_scheme, load_theme_from_bytes, pack_theme, parse_theme, parse_wallpaper_url,
    prepare_blurred_background, prepare_embedded_palette, replace_adjustable_colors,
};

#[test]
fn palette_checksum_is_stable_and_nonzero() {
    assert_ne!(PALETTE_CHECKSUM, 0);
    assert_eq!(Palette::CHECKSUM, PALETTE_CHECKSUM);
    assert_eq!(Palette::ENTRY_COUNT, PALETTE_ENTRY_COUNT);
    assert_eq!(Palette::COUNT, PALETTE_COUNT);
    assert_eq!(Palette::COUNT, Palette::ENTRY_COUNT + 2);
}

#[test]
fn night_keep_contrast_switches_foreground_when_accent_is_bright() {
    let night = embedded_scheme(EmbeddedType::Night);
    // Near-white accent makes activeButtonBg very light, so white activeButtonFg lacks contrast
    // against shifted check color (delta < 64) and must fall back to replacement (#17212b shifted).
    let colorizer = colorizer_from(&night, Color::rgb(250, 250, 240));
    let mut r = 255u8;
    let mut g = 255u8;
    let mut b = 255u8;
    colorize_named("activeButtonFg", &mut r, &mut g, &mut b, &colorizer);
    // Should be replaced by a dark color instead of remaining pure white
    assert!(Color::rgb(r, g, b).lightness() < 128);

    // Ignored keys must never be colorized
    let mut ir = 0x40u8;
    let mut ig = 0xa7u8;
    let mut ib = 0xe3u8;
    colorize_named(
        "historyPeer4UserpicBg",
        &mut ir,
        &mut ig,
        &mut ib,
        &colorizer,
    );
    assert_eq!((ir, ig, ib), (0x40, 0xa7, 0xe3));
}

#[test]
fn colorize_in_content_and_replace_adjustable_colors() {
    let day = embedded_scheme(EmbeddedType::Default);
    let accent = default_accent_colors(EmbeddedType::Default)[1];
    let colorizer = colorizer_from(&day, accent);

    let default_text = generate_default_palette();
    let shifted_text = colorize_in_content(&default_text, &colorizer).unwrap();
    assert_ne!(shifted_text, default_text);

    let mut pal = Palette::default();
    pal.msg_service_bg = Color::rgba(0x11, 0x22, 0x33, 0xaa);
    let adjusted = replace_adjustable_colors(&default_text, &pal).unwrap();
    let mut reloaded = Palette::new_uninitialized();
    load_color_scheme(&adjusted, &Colorizer::default(), &mut reloaded).unwrap();
    reloaded.finalize();
    assert_eq!(reloaded.msg_service_bg, Color::rgba(0x11, 0x22, 0x33, 0xaa));

    let editor_export = export_palette_for_editor(&reloaded);
    let mut from_export = Palette::new_uninitialized();
    load_color_scheme(&editor_export, &Colorizer::default(), &mut from_export).unwrap();
    from_export.finalize();
    assert_eq!(
        from_export.msg_service_bg,
        Color::rgba(0x11, 0x22, 0x33, 0xaa)
    );
}

#[test]
fn embedded_theme_zip_round_trip_preserves_palette() {
    for scheme in embedded_themes() {
        let pal = prepare_embedded_palette(scheme.kind, None).unwrap();
        let text = export_palette_for_editor(&pal);
        let parsed = ParsedTheme {
            palette: text,
            background: Vec::new(),
            is_png: false,
            tiled: false,
        };
        let zipped = pack_theme(&parsed).unwrap();
        let unpacked = parse_theme(&zipped, false, None).unwrap();
        assert_eq!(unpacked.palette, parsed.palette);

        let loaded = load_theme_from_bytes(&zipped, &Colorizer::default(), None, false).unwrap();
        assert_eq!(loaded.palette, pal);
    }
}

#[test]
fn readability_and_darken_under_white_text() {
    let bright = vec![Color::rgb(240, 240, 200), Color::rgb(230, 220, 255)];
    let darkened = darken_under_white_text(bright);
    for c in darkened {
        assert!(count_contrast(Color::WHITE, c) >= 4.5);
    }

    let reference = prepare_embedded_palette(EmbeddedType::Night, None).unwrap();
    let mut pal = reference.clone();
    // Corrupt outgoing text contrast on dark bubble, then repair via ensure_bubbles_readable
    pal.msg_out_bg = Color::rgb(230, 230, 230);
    ensure_bubbles_readable(
        &mut pal,
        &BubblesReadabilityArgs {
            dark: true,
            outgoing_gradient: Vec::new(),
            reference: Some(&reference),
            derive_outgoing_selection: false,
        },
    );
    assert!(count_contrast(pal.history_text_out_fg, pal.msg_out_bg) >= 4.5);
}

#[test]
fn wallpaper_legacy_deserialization_and_gradient_pipeline() {
    // Construct legacy serialized bytes (version_tag != 0x7FFFFFFF)
    let mut legacy = Vec::new();
    legacy.extend_from_slice(&12345u64.to_be_bytes()); // id
    legacy.extend_from_slice(&67890u64.to_be_bytes()); // access_hash
    legacy.extend_from_slice(&((1i32 << 3) | (1i32 << 4)).to_be_bytes()); // Pattern | Dark
    legacy.extend_from_slice(&0xffff_ffffu32.to_be_bytes()); // empty QString slug
    legacy.extend_from_slice(&(1i32 << 1).to_be_bytes()); // settings: blur
    legacy.extend_from_slice(&0x0011_2233u32.to_be_bytes()); // background color
    legacy.extend_from_slice(&40i32.to_be_bytes()); // intensity

    let wp = WallPaper::from_serialized(&legacy).unwrap();
    assert_eq!(wp.id, 12345);
    assert!(wp.flags.contains(WallPaperFlags::PATTERN));
    assert!(wp.flags.contains(WallPaperFlags::DARK));
    assert!(wp.blurred);
    assert_eq!(wp.intensity, 40);
    assert_eq!(wp.background_colors, vec![Color::rgb(0x11, 0x22, 0x33)]);

    // Blur scales oversized images down to 900 max dimension
    let big = RgbaImage::from_pixel(1200, 600, Rgba([80, 120, 160, 255]));
    let blurred = prepare_blurred_background(&big);
    assert_eq!(blurred.dimensions(), (900, 450));

    // Deterministic dithered gradient + linear + complex + background composition
    let parsed =
        parse_wallpaper_url("https://t.me/bg/dbddbb~6ba587~d5d88d~88b884?intensity=50").unwrap();
    let wp_def = parsed.local_wallpaper.unwrap();
    let dithered =
        generate_dithered_gradient(&wp_def.background_colors, wp_def.gradient_rotation());
    assert_eq!(dithered.dimensions(), (512, 512));
    assert_eq!(
        dithered.as_raw(),
        generate_dithered_gradient(&wp_def.background_colors, wp_def.gradient_rotation()).as_raw()
    );

    let lin = generate_linear_gradient(32, 32, &[Color::BLACK, Color::WHITE], 45);
    assert_eq!(lin.dimensions(), (32, 32));
    let cplx = generate_complex_gradient(32, 32, &wp_def.background_colors, 90, 0.5);
    assert_eq!(cplx.dimensions(), (32, 32));
    let bg = generate_background_image(32, 32, &wp_def.background_colors, 0, 0.5, None);
    assert_eq!(bg.dimensions(), (32, 32));

    // Night mode sanity check
    let mut nm = NightModeState::new(false, true);
    assert!(nm.on_system_dark_mode_changed(Some(true)).is_some());
}
