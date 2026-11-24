//! Integration tests for SVG & `.tgv` pattern wallpaper rasterization,
//! `.tdesktop-theme` SVG backgrounds, and `WallPaper` pattern tinting.

use std::io::{Cursor, Write};

use image::{Rgba, RgbaImage};
use tg_theme::{
    Color, Colorizer, ParsedTheme, PatternTileMode, SvgPatternError, SvgRasterOptions,
    TGV_MAX_DECOMPRESSED_BYTES, WallPaper, WallPaperFlags, compress_tgv, decompress_tgv,
    decompress_tgv_with_limit, is_gzip_bytes, is_svg_bytes, load_from_cache, load_theme_from_bytes,
    pack_theme, parse_theme, rasterize_svg_pattern_mask, rasterize_svg_rgba,
    rasterize_tgv_pattern_mask, render_tiled_pattern_wallpaper, tile_pattern_mask,
    tint_pattern_pixel,
};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

fn make_theme_zip(palette_text: &[u8], bg_entry_name: &str, bg_bytes: &[u8]) -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut cursor);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        zip.start_file("colors.tdesktop-theme", opts).unwrap();
        zip.write_all(palette_text).unwrap();
        zip.start_file(bg_entry_name, opts).unwrap();
        zip.write_all(bg_bytes).unwrap();
        zip.finish().unwrap();
    }
    cursor.into_inner()
}

/// Locally authored seamless geometric pattern SVG (32x32 tile: diamond + corner dots).
fn synthetic_pattern_svg() -> Vec<u8> {
    br##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 32 32">
  <polygon points="16,4 28,16 16,28 4,16" fill="#000000"/>
  <rect x="0" y="0" width="4" height="4" fill="#000000"/>
  <rect x="28" y="0" width="4" height="4" fill="#000000"/>
  <rect x="0" y="28" width="4" height="4" fill="#000000"/>
  <rect x="28" y="28" width="4" height="4" fill="#000000"/>
</svg>"##
        .to_vec()
}

#[test]
fn theme_zip_with_inline_svg_background_decodes_to_target_dimensions() {
    let svg =
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="96" height="64" viewBox="0 0 96 64">
        <rect x="0" y="0" width="48" height="64" fill="#204080"/>
        <rect x="48" y="0" width="48" height="64" fill="#c06020" fill-opacity="0.5"/>
    </svg>"##;
    let palette = b"windowBg: #141a24;\nwindowBgActive: #3399dd;\n";

    // Works both as `background.svg` and when inline SVG is stored inside `background.png` or `tiled.svg`.
    for (entry, expect_tiled) in [
        ("background.svg", false),
        ("background.png", false),
        ("tiled.svg", true),
    ] {
        let zip_bytes = make_theme_zip(palette, entry, svg);
        let parsed = parse_theme(&zip_bytes, false, None).unwrap();
        assert_eq!(parsed.tiled, expect_tiled);
        assert!(is_svg_bytes(&parsed.background));

        let instance = load_theme_from_bytes(&zip_bytes, &Colorizer::default(), None, false)
            .unwrap_or_else(|e| panic!("failed to load theme zip with '{entry}': {e}"));
        assert_eq!(instance.tiled, expect_tiled);
        let bg = instance
            .background
            .as_ref()
            .expect("SVG background should decode into an RgbaImage");
        assert_eq!(bg.dimensions(), (96, 64));

        // Left half: opaque #204080.
        assert_eq!(bg.get_pixel(16, 32).0, [0x20, 0x40, 0x80, 255]);
        // Right half: #c06020 at 50% alpha composited over opaque white -> ~(224, 176, 144, 255).
        let right = bg.get_pixel(72, 32).0;
        assert_eq!(right[3], 255);
        assert!(right[0] > 210 && right[1] > 165 && right[2] > 130);

        // BMP cache round-trip works identically.
        let cached = load_from_cache(&zip_bytes, &instance.cached)
            .expect("theme with SVG background should round-trip via cache");
        assert_eq!(cached.tiled, expect_tiled);
        assert_eq!(
            cached.background.as_ref().unwrap().dimensions(),
            bg.dimensions()
        );
        assert_eq!(
            cached.background.as_ref().unwrap().get_pixel(16, 32).0,
            [0x20, 0x40, 0x80, 255]
        );
    }
}

#[test]
fn theme_zip_with_gzipped_tgv_background_decodes_cleanly() {
    let svg =
        br##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="24" viewBox="0 0 40 24">
        <rect x="0" y="0" width="40" height="24" fill="#108040"/>
    </svg>"##;
    let tgv = compress_tgv(svg).unwrap();
    assert!(is_gzip_bytes(&tgv));

    let zip_bytes = make_theme_zip(b"windowBg: #ffffff;\n", "tiled.tgv", &tgv);
    let instance = load_theme_from_bytes(&zip_bytes, &Colorizer::default(), None, false).unwrap();
    assert!(instance.tiled);
    let bg = instance.background.as_ref().unwrap();
    assert_eq!(bg.dimensions(), (40, 24));
    assert_eq!(bg.get_pixel(20, 12).0, [0x10, 0x80, 0x40, 255]);

    // `pack_theme` -> `parse_theme` -> `load_theme_from_bytes` round-trip preserves SVG & TGV.
    let repacked = pack_theme(&ParsedTheme {
        palette: b"windowBg: #ffffff;\n".to_vec(),
        background: tgv.clone(),
        is_png: false,
        tiled: true,
    })
    .unwrap();
    let reparsed = parse_theme(&repacked, false, None).unwrap();
    assert!(reparsed.tiled);
    assert_eq!(reparsed.background, tgv);
    let reloaded = load_theme_from_bytes(&repacked, &Colorizer::default(), None, false).unwrap();
    assert_eq!(reloaded.background.as_ref().unwrap().dimensions(), (40, 24));
}

#[test]
fn theme_zip_svg_enforces_pixel_area_and_xml_entity_safety_limits() {
    let palette = b"windowBg: #ffffff;\n";

    // Exceeds 25 MP BACKGROUND_PIXEL_LIMIT (6000 x 6000 = 36 MP).
    let oversized_svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="6000" height="6000"><rect width="6000" height="6000" fill="#000"/></svg>"##;
    let oversized_zip = make_theme_zip(palette, "background.svg", oversized_svg);
    assert!(load_theme_from_bytes(&oversized_zip, &Colorizer::default(), None, false).is_err());

    // XML billion-laughs / DOCTYPE + ENTITY payload must be rejected.
    let xxe_svg = br#"<?xml version="1.0"?>
<!DOCTYPE svg [
  <!ENTITY lol "lol">
  <!ENTITY lol2 "&lol;&lol;&lol;&lol;">
]>
<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><text>&lol2;</text></svg>"#;
    let xxe_zip = make_theme_zip(palette, "background.png", xxe_svg);
    assert!(load_theme_from_bytes(&xxe_zip, &Colorizer::default(), None, false).is_err());

    // Custom entity reference without DOCTYPE must also be rejected.
    let custom_ent_svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><rect width="16" height="16" id="&evil;"/></svg>"#;
    let custom_zip = make_theme_zip(palette, "background.svg", custom_ent_svg);
    assert!(load_theme_from_bytes(&custom_zip, &Colorizer::default(), None, false).is_err());
}

#[test]
fn tgv_pattern_tiles_seamlessly_over_four_color_gradient_with_positive_and_negative_intensity() {
    let svg = synthetic_pattern_svg();
    let tgv = compress_tgv(&svg).unwrap();
    assert_eq!(decompress_tgv(&tgv).unwrap(), svg);

    // Verify alpha mask rasterization from `.tgv`.
    let mask = rasterize_tgv_pattern_mask(&tgv, None, TGV_MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(mask.dimensions(), (32, 32));
    // Center of diamond (16, 16) is covered; (2, 16) is outside diamond and corner squares.
    assert_eq!(mask.get_pixel(16, 16).0, [0, 0, 0, 255]);
    assert_eq!(mask.get_pixel(2, 16).0, [0, 0, 0, 0]);

    // Verify seamless tiling periodicity in both Repeat and Centered modes.
    for mode in [PatternTileMode::Repeat, PatternTileMode::Centered] {
        let tiled = tile_pattern_mask(&mask, 128, 96, mode);
        assert_eq!(tiled.dimensions(), (128, 96));
        for y in 0..96 {
            for x in 0..(128 - 32) {
                assert_eq!(tiled.get_pixel(x, y), tiled.get_pixel(x + 32, y));
            }
        }
        for y in 0..(96 - 32) {
            for x in 0..128 {
                assert_eq!(tiled.get_pixel(x, y), tiled.get_pixel(x, y + 32));
            }
        }
    }

    // 4-color gradient matching a light Telegram-style palette.
    let colors = vec![
        Color::rgb(219, 221, 187),
        Color::rgb(107, 165, 135),
        Color::rgb(213, 216, 141),
        Color::rgb(136, 184, 132),
    ];

    // Positive intensity (+50): pattern overlaid on top of the 4-color gradient.
    let positive = render_tiled_pattern_wallpaper(
        &tgv,
        128,
        96,
        &colors,
        0,
        50,
        None,
        PatternTileMode::Repeat,
    )
    .unwrap();
    assert_eq!(positive.dimensions(), (128, 96));
    assert!(positive.pixels().all(|p| p.0[3] == 255));

    // Zero intensity (pure 4-color gradient, no pattern modulation).
    let zero =
        render_tiled_pattern_wallpaper(&tgv, 128, 96, &colors, 0, 0, None, PatternTileMode::Repeat)
            .unwrap();
    // Unmasked pixel (2, 16) is untouched by positive pattern overlay.
    assert_eq!(positive.get_pixel(2, 16), zero.get_pixel(2, 16));
    // Masked pixel at tile 0 (16, 16) and tile 2 (80, 48) are darkened via SoftLight.
    let p_masked_0 = positive.get_pixel(16, 16).0;
    let z_masked_0 = zero.get_pixel(16, 16).0;
    assert!(p_masked_0[0] < z_masked_0[0] && p_masked_0[1] < z_masked_0[1]);
    let p_masked_tile = positive.get_pixel(80, 48).0;
    let z_masked_tile = zero.get_pixel(80, 48).0;
    assert!(p_masked_tile[0] < z_masked_tile[0] && p_masked_tile[1] < z_masked_tile[1]);

    // Negative intensity (-50 & -100): dark background with gradient-tinted pattern.
    let neg50 = render_tiled_pattern_wallpaper(
        &tgv,
        128,
        96,
        &colors,
        0,
        -50,
        None,
        PatternTileMode::Repeat,
    )
    .unwrap();
    let neg100 = render_tiled_pattern_wallpaper(
        &tgv,
        128,
        96,
        &colors,
        0,
        -100,
        None,
        PatternTileMode::Repeat,
    )
    .unwrap();

    assert_eq!(neg50.dimensions(), (128, 96));
    assert!(neg50.pixels().all(|p| p.0[3] == 255));
    assert!(neg100.pixels().all(|p| p.0[3] == 255));

    // Unmasked background areas are pure dark black `(0, 0, 0, 255)` across every tile.
    for (x, y) in [(2, 16), (34, 16), (66, 48), (98, 80)] {
        assert_eq!(neg50.get_pixel(x, y).0, [0, 0, 0, 255]);
        assert_eq!(neg100.get_pixel(x, y).0, [0, 0, 0, 255]);
    }

    // Masked pattern shapes show tinted gradient colors, brighter at -100 than at -50.
    for (x, y) in [(16, 16), (48, 16), (80, 48), (112, 80)] {
        let px50 = neg50.get_pixel(x, y).0;
        let px100 = neg100.get_pixel(x, y).0;
        assert!(px50[1] > 50, "expected tinted pattern pixel at ({x},{y})");
        assert!(
            px100[1] > px50[1],
            "expected -100 intensity ({}) brighter than -50 ({})",
            px100[1],
            px50[1]
        );
    }
}

#[test]
fn tgv_decompression_bomb_is_rejected_at_default_and_custom_caps() {
    // 9 MiB of spaces compresses to ~9 KiB of gzip but exceeds the 8 MiB default cap.
    let nine_mib = vec![b' '; TGV_MAX_DECOMPRESSED_BYTES + 1024 * 1024];
    let bomb = compress_tgv(&nine_mib).unwrap();
    assert!(
        bomb.len() < 32 * 1024,
        "synthetic bomb should be tiny compressed"
    );

    // Rejected by `decompress_tgv` (default 8 MiB cap).
    assert!(matches!(
        decompress_tgv(&bomb),
        Err(SvgPatternError::DecompressionBomb {
            limit: TGV_MAX_DECOMPRESSED_BYTES
        })
    ));

    // Rejected by `rasterize_svg_pattern_mask` and `render_tiled_pattern_wallpaper`.
    assert!(matches!(
        rasterize_svg_pattern_mask(&bomb, None),
        Err(SvgPatternError::DecompressionBomb { .. })
    ));
    assert!(matches!(
        render_tiled_pattern_wallpaper(
            &bomb,
            64,
            64,
            &[Color::rgb(100, 150, 200)],
            0,
            50,
            None,
            PatternTileMode::Repeat
        ),
        Err(SvgPatternError::DecompressionBomb { .. })
    ));

    // Custom small cap also triggers before full expansion.
    assert!(matches!(
        decompress_tgv_with_limit(&bomb, 1024),
        Err(SvgPatternError::DecompressionBomb { limit: 1024 })
    ));

    // Non-gzip bytes rejected by `decompress_tgv`.
    assert!(matches!(
        decompress_tgv(b"<svg/>"),
        Err(SvgPatternError::InvalidGzip(_))
    ));
}

#[test]
fn wallpaper_pattern_tinting_helpers_cover_full_intensity_range_and_inversion() {
    let svg = synthetic_pattern_svg();
    let tgv = compress_tgv(&svg).unwrap();

    // Positive intensity on bright 4-color wallpaper: dark SoftLight strokes (not inverted).
    let light_wp = WallPaper::construct_default().with_pattern_intensity(60);
    assert!(!light_wp.is_negative_pattern());
    assert!(!light_wp.should_invert_pattern());
    assert_eq!(light_wp.clamped_pattern_intensity(), 60);

    let bg_mid = Color::rgb(160, 180, 150);
    let tinted_pos = light_wp.tint_pattern_color(bg_mid, 255);
    assert!(tinted_pos.r < bg_mid.r && tinted_pos.g < bg_mid.g);
    assert_eq!(light_wp.tint_pattern_color(bg_mid, 0), bg_mid);

    // Positive intensity on dark background (value_f <= 0.3): automatically inverts to white strokes.
    let mut dark_bg_wp = WallPaper::custom()
        .with_background_colors(vec![Color::rgb(20, 30, 40)])
        .with_pattern_intensity(70);
    dark_bg_wp.flags = WallPaperFlags::PATTERN;
    assert!(dark_bg_wp.should_invert_pattern());
    let tinted_inv = dark_bg_wp.tint_pattern_color(Color::rgb(20, 30, 40), 255);
    assert!(tinted_inv.r > 20 && tinted_inv.g > 30 && tinted_inv.b > 40);

    // Negative intensity dark-mode wallpaper: black background + tinted pattern.
    let neg_wp = light_wp.with_pattern_intensity(-40);
    assert!(neg_wp.is_negative_pattern());
    assert!(!neg_wp.should_invert_pattern());
    assert_eq!(neg_wp.tint_pattern_color(bg_mid, 0), Color::BLACK);
    let tinted_neg = neg_wp.tint_pattern_color(bg_mid, 255);
    assert_eq!(tinted_neg, Color::rgb(64, 72, 60));

    // Out-of-range intensities clamp to [-100..=100].
    let clamped_hi = light_wp.with_pattern_intensity(250);
    let exact_hi = light_wp.with_pattern_intensity(100);
    assert_eq!(clamped_hi.clamped_pattern_intensity(), 100);
    assert_eq!(
        clamped_hi.tint_pattern_color(bg_mid, 255),
        exact_hi.tint_pattern_color(bg_mid, 255)
    );
    let clamped_lo = light_wp.with_pattern_intensity(-250);
    assert_eq!(clamped_lo.clamped_pattern_intensity(), -100);
    assert_eq!(clamped_lo.tint_pattern_color(bg_mid, 255), bg_mid);

    // End-to-end `WallPaper::render_svg_pattern` matches `tint_pattern_image`.
    let mask = rasterize_svg_pattern_mask(&tgv, None).unwrap();
    let via_render = neg_wp.render_svg_pattern(&tgv, 64, 64, None).unwrap();
    let via_tint = neg_wp.tint_pattern_image(&mask, 64, 64);
    assert_eq!(via_render.as_raw(), via_tint.as_raw());
    assert_eq!(via_render.get_pixel(2, 16).0, [0, 0, 0, 255]);
    assert!(via_render.get_pixel(16, 16).0[1] > 40);

    // Direct `tint_pattern_pixel` sanity check on synthetic RgbaImage.
    let direct = tint_pattern_pixel(Color::rgb(200, 100, 50), 255, -50, false);
    assert_eq!(direct, Color::rgb(100, 50, 25));

    // Explicit target size override in `rasterize_svg_rgba`.
    let scaled: RgbaImage = rasterize_svg_rgba(
        &svg,
        &SvgRasterOptions {
            target_size: Some((64, 64)),
            force_opaque_white: true,
            ..SvgRasterOptions::default()
        },
    )
    .unwrap();
    assert_eq!(scaled.dimensions(), (64, 64));
    assert_eq!(scaled.get_pixel(32, 32), &Rgba([0, 0, 0, 255]));
    assert_eq!(scaled.get_pixel(4, 32), &Rgba([255, 255, 255, 255]));
}
