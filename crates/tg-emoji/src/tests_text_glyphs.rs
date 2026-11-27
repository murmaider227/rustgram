//! Emoji glyphs inside egui text (TGB-3048): tdesktop's sizes, the sheet's pixels, one glyph per
//! emoji sequence.

use std::sync::Arc;

use egui::epaint::text::{FontFamily, GlyphRasterizerRequest, RasterizedGlyph};
use egui::{Color32, Context, FontId};

use tg_text::emoji;

use crate::atlas::{CELL_SIZE_PX, EmojiAtlas};
use crate::resample::area_downscale;
use crate::text_glyphs::{
    EmojiGlyphMetrics, cluster_emoji, install_text_emoji, rasterize_emoji_cluster,
    text_pixels_per_point,
};

fn rasterize(cluster: &str, font_size_px: f32, ppp: f32) -> Option<RasterizedGlyph> {
    let family = FontFamily::Proportional;
    let request = GlyphRasterizerRequest {
        cluster,
        family: &family,
        font_size_px,
        subpixel_offset_px: 0.0,
    };
    rasterize_emoji_cluster(&request, 18.0, 1.0, ppp)
}

fn pixel(glyph: &RasterizedGlyph, x: usize, y: usize) -> Color32 {
    glyph.bitmap.image[(x, y)]
}

#[test]
fn clusters_made_of_emoji_are_found_like_tdesktops_parser() {
    for (cluster, count) in [
        ("😀", 1),
        ("❤️", 1),
        ("❤", 1),
        ("🇺🇦", 1),
        ("👍🏽", 1),
        ("1\u{FE0F}\u{20E3}", 1),
        ("👩\u{200D}💻", 1),
    ] {
        let found = cluster_emoji(cluster).unwrap_or_else(|| panic!("{cluster:?} not found"));
        assert_eq!(found.len(), count, "{cluster:?}");
    }
    for text in ["a", "1", "#", "й", "€", "\u{0301}", "😀x"] {
        assert!(cluster_emoji(text).is_none(), "{text:?} is not an emoji");
    }
}

#[test]
fn emoji_glyph_has_tdesktops_size_padding_and_advance() {
    // 100%: st::emojiSize 18 + 2 * st::emojiPadding 1 (`text_block.cpp`).
    let glyph = rasterize("😀", 13.0, 1.0).expect("😀 is drawn from the sheet");
    assert!(glyph.bitmap.is_color);
    assert_eq!(glyph.advance_px, 20.0);
    assert_eq!(glyph.bitmap.image.size, [18, 18]);
    assert_eq!(glyph.bitmap.offset_px.x, 1.0);
    // Centred on the 13 px font box: top 14 px above the baseline like tdesktop's
    // `emojiY = (font->height - st::emojiSize) / 2` with Open Sans 13 (ascent 14, height 18).
    assert_eq!(glyph.bitmap.offset_px.y, -14.0);

    // 150% through pixels per point: 27 px, padding ConvertScale(1) = 2 px.
    let glyph = rasterize("😀", 19.5, 1.5).expect("😀 at 150%");
    assert_eq!(glyph.bitmap.image.size, [27, 27]);
    assert_eq!(glyph.advance_px, 31.0);
    assert_eq!(
        EmojiGlyphMetrics::new(18.0, 1.0, 1.5, 19.5),
        EmojiGlyphMetrics {
            size_px: 27,
            padding_px: 2,
            top_px: -21,
        }
    );
}

#[test]
fn emoji_glyph_pixels_come_from_the_sheet() {
    let grin = rasterize("😀", 13.0, 1.0).expect("😀");
    let sheet = EmojiAtlas::global()
        .emoji_rgba(emoji::find_emoji("😀", 0).expect("😀").0)
        .expect("tile decodes")
        .expect("😀 has a cell");
    let side = CELL_SIZE_PX as usize;
    let expected = egui::ColorImage::from_rgba_unmultiplied(
        [18, 18],
        &area_downscale(&sheet, side, side, 18, 18),
    );
    assert_eq!(
        grin.bitmap.image.pixels, expected.pixels,
        "the sheet cell at 18 px"
    );
    let face = pixel(&grin, 9, 4);
    assert!(
        face.r() > 200 && face.g() > 150 && face.a() > 240,
        "😀 forehead is yellow, got {face:?}"
    );
    assert_eq!(pixel(&grin, 0, 0).a(), 0, "the cell corner is transparent");

    let heart = rasterize("❤️", 13.0, 1.0).expect("❤️");
    let red = pixel(&heart, 9, 8);
    assert!(
        red.r() > 180 && red.g() < 80 && red.b() < 80,
        "❤️ is red, got {red:?}"
    );

    let flag = rasterize("🇺🇦", 13.0, 1.0).expect("🇺🇦");
    let top = pixel(&flag, 9, 6);
    let bottom = pixel(&flag, 9, 12);
    assert!(
        top.b() > top.r() && top.b() > 120,
        "🇺🇦 top is blue, got {top:?}"
    );
    assert!(
        bottom.r() > 180 && bottom.g() > 150 && bottom.b() < 100,
        "🇺🇦 bottom is yellow, got {bottom:?}"
    );
}

#[test]
fn text_clusters_are_left_to_the_fonts() {
    for cluster in ["a", "Ж", "1", "©", "→"] {
        assert!(rasterize(cluster, 13.0, 1.0).is_none(), "{cluster:?}");
    }
}

#[test]
fn galley_draws_each_emoji_sequence_as_one_wide_colour_glyph() {
    let ctx = Context::default();
    install_text_emoji(&ctx, Arc::new(|| (18.0, 1.0)));
    for ppp in [1.0_f32, 1.5] {
        ctx.set_pixels_per_point(ppp);
        let mut galley = None;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            galley = Some(ui.painter().layout_no_wrap(
                "A😀❤️🇺🇦B".to_owned(),
                FontId::proportional(13.0),
                Color32::BLACK,
            ));
        });
        output.textures_delta.clear();
        assert_eq!(text_pixels_per_point(), ppp, "the plugin records each pass");
        let galley = galley.expect("laid out");
        let glyphs = &galley.rows[0].row.glyphs;
        let chars: Vec<char> = glyphs.iter().map(|g| g.chr).collect();
        assert_eq!(chars, "A😀❤️🇺🇦B".chars().collect::<Vec<_>>());
        let advance = (if ppp > 1.0 { 31.0 } else { 20.0 }) / ppp;
        for (i, expected) in [(1, advance), (2, advance), (3, 0.0), (4, advance), (5, 0.0)] {
            assert!(
                (glyphs[i].advance_width - expected).abs() < 0.01,
                "glyph {i} ({:?}) at {ppp}: advance {} != {expected}",
                glyphs[i].chr,
                glyphs[i].advance_width
            );
        }
        assert!(glyphs[1].is_color && glyphs[2].is_color && glyphs[4].is_color);
        assert!(!glyphs[0].is_color && !glyphs[6].is_color);
    }
}
