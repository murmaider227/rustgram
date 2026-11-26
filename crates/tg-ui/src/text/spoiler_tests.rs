//! Spoilers and collapsed quotes of the block renderer, against tdesktop's `Ui::Text` (TGB-3009).

use egui::epaint::TextureId;
use egui::{CentralPanel, Context, Event, Frame, Modifiers, PointerButton, RawInput, Shape};
use tg_text::{EntityInText, EntityType, TextWithEntities, utf16_len};

use super::*;
use crate::fonts::register_tdesktop_fonts;

struct TestColors;

impl crate::theme::Colors for TestColors {
    fn get(&self, _name: &str) -> Option<Color32> {
        Some(Color32::from_rgb(0x40, 0x80, 0xC0))
    }
}

fn context() -> Context {
    let ctx = Context::default();
    register_tdesktop_fonts(&ctx);
    let mut init = ctx.run_ui(RawInput::default(), |_| {});
    init.textures_delta.clear();
    ctx
}

fn long_collapsed_quote() -> TextWithEntities {
    let quote = (0..8)
        .map(|i| format!("Quoted line {i} that is long enough to fill the width of the block."))
        .collect::<Vec<_>>()
        .join("\n");
    let entities = vec![EntityInText::with_data(
        EntityType::Blockquote,
        0,
        utf16_len(&quote) as i32,
        "1",
    )];
    TextWithEntities::with_entities(quote, entities)
}

fn layout(ctx: &Context, source: &TextWithEntities, options: &RichTextOptions) -> LaidOutRichText {
    layout_text_with_entities(
        ctx,
        source,
        &TextStyle::default_style(),
        &TextPalette::default_palette(&TestColors),
        options,
    )
}

fn has_ellipsis(block: &LaidOutBlock) -> bool {
    block
        .galley
        .rows
        .iter()
        .any(|row| row.glyphs.iter().any(|g| g.chr == '…'))
}

#[test]
fn a_collapsed_quote_shows_three_lines_and_no_ellipsis() {
    let ctx = context();
    let laid = layout(
        &ctx,
        &long_collapsed_quote(),
        &RichTextOptions::new(Color32::BLACK, 300.0),
    );
    let quote = &laid.blocks[0];
    assert_eq!(quote.galley.rows.len(), QUOTE_COLLAPSED_LINES);
    // `Renderer::draw` stops after `_quoteLinesLeft`; it does not elide the last line.
    assert!(!has_ellipsis(quote), "a collapsed quote ends without '…'");

    // Expanded it shows every line.
    let expanded = layout_text_with_entities_expanded(
        &ctx,
        &long_collapsed_quote(),
        &TextStyle::default_style(),
        &TextPalette::default_palette(&TestColors),
        &RichTextOptions::new(Color32::BLACK, 300.0),
        &[0],
    );
    assert!(expanded.blocks[0].galley.rows.len() > 8);
}

#[test]
fn a_max_lines_limit_still_elides() {
    let ctx = context();
    let text = TextWithEntities::simple(
        "One long paragraph that wraps over many lines when it is laid out narrow enough, \
         so that a limit of two lines has to cut it and end it with an ellipsis.",
    );
    let laid = layout(
        &ctx,
        &text,
        &RichTextOptions::new(Color32::BLACK, 160.0).max_lines(2),
    );
    assert!(has_ellipsis(&laid.blocks[0]));
}

#[test]
fn the_spoiler_runs_of_one_line_make_one_rectangle() {
    let ctx = context();
    let text = "Answer: secret words here, done.";
    let at = |part: &str| utf16_len(&text[..text.find(part).unwrap_or(0)]) as i32;
    let source = TextWithEntities::with_entities(
        text.to_owned(),
        vec![
            EntityInText::new(EntityType::Spoiler, at("secret"), 17),
            EntityInText::new(EntityType::Bold, at("words"), 5),
        ],
    );
    let laid = layout(&ctx, &source, &RichTextOptions::new(Color32::BLACK, 400.0));
    // Three runs (plain, bold, plain) under one spoiler: one rectangle, so no particle is
    // painted twice (`fillRectsFromRanges`).
    assert_eq!(laid.spoiler_rects.len(), 1, "{:?}", laid.spoiler_rects);
}

/// Particle quads painted on the frame (textured meshes other than the font atlas) and the
/// largest particle alpha.
fn cover(shapes: &[egui::epaint::ClippedShape]) -> (usize, u8) {
    let mut quads = 0;
    let mut alpha = 0;
    for clipped in shapes {
        if let Shape::Mesh(mesh) = &clipped.shape
            && mesh.texture_id != TextureId::default()
        {
            quads += mesh.vertices.len() / 4;
            alpha = mesh
                .vertices
                .iter()
                .map(|v| v.color.a())
                .fold(alpha, u8::max);
        }
    }
    (quads, alpha)
}

fn spoiler_text() -> TextWithEntities {
    let text = "Hidden: the spoiler words are here.";
    let at = utf16_len(&text[..8]) as i32;
    TextWithEntities::with_entities(
        text.to_owned(),
        vec![EntityInText::new(EntityType::Spoiler, at, 26)],
    )
}

/// One frame at `time` painting the spoiler text with `options`.
fn frame(
    ctx: &Context,
    time: f64,
    events: Vec<Event>,
    options: &RichTextOptions,
) -> (EntityTextResponse, (usize, u8)) {
    let source = spoiler_text();
    let mut response = None;
    let mut out = ctx.run_ui(
        RawInput {
            time: Some(time),
            events,
            ..RawInput::default()
        },
        |ui| {
            CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
                response = Some(render_text_with_entities(
                    ui,
                    Id::unique("spoiler_state_test"),
                    &source,
                    &TextStyle::default_style(),
                    &TextPalette::default_palette(&TestColors),
                    options,
                ));
            });
        },
    );
    out.textures_delta.clear();
    let painted = cover(&out.shapes);
    (response.expect("painted"), painted)
}

fn click_events(ctx: &Context, options: &RichTextOptions) -> Vec<Event> {
    // The spoiler starts after "Hidden: ".
    let (resp, _) = frame(ctx, 0.0, Vec::new(), options);
    let pos = resp.rect.min + Vec2::new(90.0, 8.0);
    vec![
        Event::PointerMoved(pos),
        Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        },
        Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        },
    ]
}

#[test]
fn a_caller_kept_reveal_is_only_reported_by_the_widget() {
    let ctx = context();
    let mut options = RichTextOptions::new(Color32::BLACK, 400.0);
    options.spoiler_reveal = Some(0.0);
    let events = click_events(&ctx, &options);
    let (clicked, covered) = frame(&ctx, 0.1, events, &options);
    assert!(clicked.spoiler_toggled, "the click is reported");
    assert!(covered.0 > 50);
    // The caller did not reveal: the cover stays.
    let (_, later) = frame(&ctx, 1.0, Vec::new(), &options);
    assert!(later.0 > 50, "still hidden");

    // The caller reveals: halfway through the fade the cover is half transparent.
    options.spoiler_reveal = Some(0.5);
    let (_, half) = frame(&ctx, 1.1, Vec::new(), &options);
    assert!(half.0 > 50);
    assert!(
        u32::from(half.1) * 10 <= u32::from(later.1) * 6,
        "fading: {} vs {}",
        half.1,
        later.1
    );
    options.spoiler_reveal = Some(1.0);
    let (_, done) = frame(&ctx, 1.2, Vec::new(), &options);
    assert_eq!(done.0, 0, "revealed");
}

#[test]
fn a_widget_kept_reveal_fades_in_over_two_hundred_milliseconds() {
    let ctx = context();
    let options = RichTextOptions::new(Color32::BLACK, 400.0);
    let events = click_events(&ctx, &options);
    let (clicked, _) = frame(&ctx, 1.0, events, &options);
    assert!(clicked.spoiler_toggled);
    let (_, fading) = frame(&ctx, 1.1, Vec::new(), &options);
    assert!(fading.0 > 0, "the cover fades out");
    let (_, done) = frame(&ctx, 1.25, Vec::new(), &options);
    assert_eq!(done.0, 0, "revealed after {SPOILER_REVEAL_SECS} s");
}

#[test]
fn the_quote_toggle_is_reported_and_kept_by_the_caller() {
    let ctx = context();
    let source = long_collapsed_quote();
    let mut options = RichTextOptions::new(Color32::BLACK, 300.0);
    options.expanded_blockquotes = Some(Vec::new());
    let laid = layout(&ctx, &source, &options);
    let toggle = laid.blocks[0].toggle_rect.expect("collapsible quote");
    let run = |events: Vec<Event>, options: &RichTextOptions| {
        let mut response = None;
        let mut out = ctx.run_ui(
            RawInput {
                events,
                ..RawInput::default()
            },
            |ui| {
                CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
                    response = Some(render_text_with_entities(
                        ui,
                        Id::unique("quote_toggle_test"),
                        &source,
                        &TextStyle::default_style(),
                        &TextPalette::default_palette(&TestColors),
                        options,
                    ));
                });
            },
        );
        out.textures_delta.clear();
        response.expect("painted")
    };
    let first = run(Vec::new(), &options);
    let pos = first.rect.min + toggle.center().to_vec2();
    let clicked = run(
        vec![
            Event::PointerMoved(pos),
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ],
        &options,
    );
    assert_eq!(clicked.blockquote_toggled, Some(0));
    // Not expanded until the caller says so.
    let still = run(Vec::new(), &options);
    assert_eq!(still.rect.height(), first.rect.height());
    options.expanded_blockquotes = Some(vec![0]);
    let expanded = run(Vec::new(), &options);
    assert!(expanded.rect.height() > first.rect.height() * 2.0);
}
