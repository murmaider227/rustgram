//! Emoji in text drawn from tdesktop's sheets, like `Ui::Text` emoji blocks (TGB-3048).
//!
//! tdesktop paints each emoji of a text with `Ui::Emoji::Draw` (`text_renderer.cpp`) in a block
//! `st::emojiSize + 2 * st::emojiPadding` wide (`text_block.cpp`); selecting and copying it gives
//! back the original codepoints. These tests render text through the tg-ui text engine and the
//! compose field (egui `TextEdit`) on the GPU and look for the sheet's colours where the emoji are.
//!
//! Writes `emoji_text_<scale>.png` into `$EMOJI_SNAP_DIR` when that variable is set.
// These tests render on a GPU through egui_kittest's wgpu backend. The Linux CI runner has no adapter
// (wgpu setup panics), so they run on Windows only; the Windows CI job and the merge gate cover them.
#![cfg(windows)]

use egui::{Color32, Pos2, Rect, Vec2};
use egui_kittest::Harness;
use tg_text::{EntityInText, EntityType, TextSelection, TextWithEntities, utf16_len};
use tg_ui::fonts::register_tdesktop_fonts;
use tg_ui::style::{TextPalette, TextStyle};
use tg_ui::text::{RichTextOptions, layout_text_with_entities, render_text_with_entities};
use tg_ui::theme::Colors;

const TEXT: &str = "Hi 😀❤️🇺🇦 there";
const CANVAS: Vec2 = Vec2::new(240.0, 64.0);
const ORIGIN: Pos2 = Pos2::new(10.0, 10.0);
/// 100% and 150% display scaling.
const SCALES: [f32; 2] = [1.0, 1.5];

struct TestColors;

impl Colors for TestColors {
    fn get(&self, _name: &str) -> Option<Color32> {
        Some(Color32::from_gray(0x80))
    }
}

/// One GPU device at a time: two renderers starting at once crashed the test process.
static GPU: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn source(spoiler: bool) -> TextWithEntities {
    let entities = if spoiler {
        // "😀❤️🇺🇦" under a spoiler.
        let start = utf16_len("Hi ") as i32;
        vec![EntityInText::new(
            EntityType::Spoiler,
            start,
            utf16_len("😀❤️🇺🇦") as i32,
        )]
    } else {
        Vec::new()
    };
    TextWithEntities::with_entities(TEXT.to_owned(), entities)
}

fn render(scale: f32, ui_fn: impl FnMut(&mut egui::Ui) + 'static) -> image::RgbaImage {
    let _gpu = GPU
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut ui_fn = ui_fn;
    let mut harness = Harness::builder()
        .with_size(CANVAS)
        .with_pixels_per_point(scale)
        .with_accessibility_check(false)
        .with_context_setup(register_tdesktop_fonts)
        .wgpu()
        .build_ui(move |ui| {
            let canvas = Rect::from_min_size(Pos2::ZERO, CANVAS);
            ui.painter().rect_filled(canvas, 0.0, Color32::WHITE);
            let inner = Rect::from_min_size(ORIGIN, CANVAS - ORIGIN.to_vec2() * 2.0);
            ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| ui_fn(ui));
        });
    harness.run();
    harness.render().expect("render the text")
}

fn render_message(scale: f32, spoiler: bool) -> image::RgbaImage {
    render(scale, move |ui| {
        let options = RichTextOptions::new(Color32::BLACK, 220.0);
        let _ = render_text_with_entities(
            ui,
            egui::Id::unique("emoji_text"),
            &source(spoiler),
            &TextStyle::default_style(),
            &TextPalette::default_palette(&TestColors),
            &options,
        );
    })
}

fn is_yellow(p: &image::Rgba<u8>) -> bool {
    p[0] > 200 && p[1] > 150 && p[2] < 110
}

fn is_red(p: &image::Rgba<u8>) -> bool {
    p[0] > 170 && p[1] < 70 && p[2] < 80
}

fn is_blue(p: &image::Rgba<u8>) -> bool {
    p[2] > 150 && p[0] < 90 && p[1] < 190 && p[2] > p[1]
}

/// Pixels of `kind` and their bounding box (in physical pixels).
fn find(img: &image::RgbaImage, kind: fn(&image::Rgba<u8>) -> bool) -> (usize, Option<Rect>) {
    let mut count = 0;
    let mut bbox: Option<Rect> = None;
    for (x, y, p) in img.enumerate_pixels() {
        if kind(p) {
            count += 1;
            let r = Rect::from_min_size(Pos2::new(x as f32, y as f32), Vec2::splat(1.0));
            bbox = Some(bbox.map_or(r, |b| b.union(r)));
        }
    }
    (count, bbox)
}

fn save(img: &image::RgbaImage, name: &str) {
    if let Some(dir) = std::env::var_os("EMOJI_SNAP_DIR") {
        let path = std::path::Path::new(&dir).join(name);
        img.save(&path).expect("write snapshot");
    }
}

#[test]
fn message_text_draws_the_sheet_emoji_at_tdesktops_size() {
    for scale in SCALES {
        let img = render_message(scale, false);
        save(&img, &format!("emoji_text_{}.png", (scale * 100.0) as u32));
        let side = (18.0 * scale).round();
        let (yellow, _) = find(&img, is_yellow);
        let (red, red_box) = find(&img, is_red);
        let (blue, _) = find(&img, is_blue);
        // An 18 px emoji covers about 250 px at 100%: 😀 is mostly yellow, ❤️ mostly red, the flag
        // half blue and half yellow. Without sheet emoji there is no colour at all.
        let area = side * side;
        assert!(
            yellow as f32 > 0.35 * area,
            "😀 + 🇺🇦 yellow at {scale}: {yellow}"
        );
        assert!(red as f32 > 0.3 * area, "❤️ red at {scale}: {red}");
        assert!(blue as f32 > 0.15 * area, "🇺🇦 blue at {scale}: {blue}");
        let red_box = red_box.expect("a red heart");
        assert!(
            red_box.width() <= side + 1.0 && red_box.height() <= side + 1.0,
            "❤️ fits its {side} px cell at {scale}: {red_box:?}"
        );
        assert!(
            red_box.width() >= side * 0.7,
            "❤️ is drawn at the emoji size, not a font glyph's: {red_box:?} at {scale}"
        );
    }
}

#[test]
fn emoji_take_tdesktops_advance_and_select_and_copy_as_codepoints() {
    let ctx = egui::Context::default();
    register_tdesktop_fonts(&ctx);
    let mut out = ctx.run_ui(egui::RawInput::default(), |_| {});
    out.textures_delta.clear();
    let laid = layout_text_with_entities(
        &ctx,
        &source(false),
        &TextStyle::default_style(),
        &TextPalette::default_palette(&TestColors),
        &RichTextOptions::new(Color32::BLACK, 400.0),
    );
    let glyphs = &laid.blocks[0].galley.rows[0].row.glyphs;
    assert_eq!(glyphs.len(), TEXT.chars().count(), "one glyph per char");
    // "Hi " then 😀, ❤ + U+FE0F, 🇺 + 🇦.
    let advances: Vec<f32> = glyphs[3..8].iter().map(|g| g.advance_width).collect();
    assert_eq!(advances, vec![20.0, 20.0, 0.0, 20.0, 0.0]);

    // A point past the middle of the flag puts the cursor after the whole flag.
    let flag = &glyphs[6];
    let origin = laid.blocks[0].text_origin;
    let hit = laid.hit_test(Pos2::new(
        origin.x + flag.pos.x + 15.0,
        origin.y + flag.pos.y - 4.0,
    ));
    let end_of_flag = utf16_len("Hi 😀❤️🇺🇦");
    assert_eq!(hit.utf16_offset, end_of_flag);
    let selection = TextSelection::new(utf16_len("Hi ") as u16, end_of_flag as u16);
    assert_eq!(laid.selected_text(selection), "😀❤️🇺🇦");
    let rects = laid.selection_rects(selection);
    assert_eq!(rects.len(), 1);
    assert!((rects[0].width() - 60.0).abs() < 0.5, "{rects:?}");
}

#[test]
fn hidden_spoiler_hides_its_emoji() {
    let img = render_message(1.0, true);
    let (red, _) = find(&img, is_red);
    let (blue, _) = find(&img, is_blue);
    let (yellow, _) = find(&img, is_yellow);
    assert_eq!(red + blue + yellow, 0, "spoiler cover hides the emoji");
}

#[test]
fn compose_field_draws_the_sheet_emoji() {
    for scale in SCALES {
        let img = render(scale, |ui| {
            let mut text = "Hello ❤️".to_owned();
            ui.add(egui::TextEdit::singleline(&mut text).desired_width(200.0));
        });
        save(
            &img,
            &format!("emoji_compose_{}.png", (scale * 100.0) as u32),
        );
        let side = (18.0 * scale).round();
        let (red, _) = find(&img, is_red);
        assert!(
            red as f32 > 0.3 * side * side,
            "❤️ in the field at {scale}: {red}"
        );
    }
}

/// Layout cost of text with emoji (TGB-3048 frame-cost check), printed, not asserted:
/// `cargo test -p tg-ui --test emoji_text frame_cost -- --ignored --nocapture`.
#[test]
#[ignore = "measurement"]
fn frame_cost() {
    fn lay_out(ctx: &egui::Context, lines: &[String]) -> std::time::Duration {
        let start = std::time::Instant::now();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            for line in lines {
                let _ = ui.painter().layout(
                    line.clone(),
                    egui::FontId::proportional(13.0),
                    Color32::BLACK,
                    300.0,
                );
            }
        });
        out.textures_delta.clear();
        start.elapsed()
    }
    let batch = |tag: &str, with_emoji: bool| -> Vec<String> {
        (0..300)
            .map(|i| {
                if with_emoji {
                    format!("{tag} message {i} 😀❤️🇺🇦👍🏽🔥 with some text")
                } else {
                    format!("{tag} message {i} abcdefghi with some text")
                }
            })
            .collect()
    };
    let ctx = egui::Context::default();
    register_tdesktop_fonts(&ctx);
    let _ = lay_out(&ctx, &batch("warmup", false));
    let plain = lay_out(&ctx, &batch("plain", false));
    let cold = lay_out(&ctx, &batch("cold", true));
    let warm = lay_out(&ctx, &batch("warm", true));
    let cached = lay_out(&ctx, &batch("warm", true));
    println!(
        "300 lines: plain {plain:?}, emoji first use {cold:?}, emoji new lines {warm:?}, \
         emoji same lines again (galley cache) {cached:?}"
    );
}
