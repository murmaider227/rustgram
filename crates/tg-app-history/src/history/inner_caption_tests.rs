//! Bubble height must hold every wrapped line of the text as it is painted.
//!
//! Regression for a photo caption in Cyrillic whose last line was drawn below the bubble while
//! the time sat over the previous line: the caption height was measured at the widest bubble
//! while the photo made the bubble narrower (tdesktop measures at the real width:
//! `Message::resizeGetHeight` -> `Ui::Text::String::countHeight`).

use egui::{Context, Ui};
use tg_history::consts::{FontMetrics, MSG_DATE_SPACE};
use tg_text::TextWithEntities;
use tg_ui::text::build_layout_job;

use super::*;
use crate::history::backend::{HistoryMedia, MessageExtra, PhotoAttachment};

const VIEWPORT_W: i32 = 720;
const CAPTION: &str =
    "у моей девушки прохождение клаттерфанка в геометри дэш в 360р на ютубе собрало 34к просмотров";

fn message(text: &str, photo: Option<(i32, i32)>) -> HistoryMessage {
    HistoryMessage {
        id: MsgId(501),
        peer_id: PeerId(-1_000_500),
        from_id: PeerId(77),
        sender_name: "Тестовий Автор".to_owned(),
        date_unix: 1_700_000_000,
        outgoing: false,
        is_post: false,
        text: TextWithEntities::simple(text),
        media: photo.map(|(width, height)| {
            HistoryMedia::Photo(PhotoAttachment {
                width,
                height,
                stripped_thumb: None,
                fallback_rgb: ([0x30, 0x10, 0x10], [0x10, 0x05, 0x05]),
                file_name: "frame.jpg".to_owned(),
                size_bytes: 10_000,
            })
        }),
        service: None,
        reply: None,
        forward: None,
        reactions: Vec::new(),
        views: None,
        edited: false,
        is_read: true,
        is_sending: false,
        is_pinned: false,
        extra: MessageExtra::default(),
    }
}

fn layout_of(ui: &Ui, msg: &HistoryMessage) -> tg_history::MessageLayout {
    let colors = HistoryColors::from_palette(&Palette::day());
    let item = build_list_item(msg, true, true, false);
    match compute_item_layout_egui(
        ui,
        msg,
        &item,
        VIEWPORT_W,
        false,
        true,
        &colors,
        &FontMetrics::default(),
        0,
        0,
        None,
    ) {
        CachedItemLayout::Message(layout) => *layout,
        CachedItemLayout::Service(_) => panic!("expected a message layout"),
    }
}

/// The caption galley exactly as `bubble.rs` paints it inside `text_rect`.
fn painted_rows(ui: &Ui, msg: &HistoryMessage, wrap: f32) -> (f32, f32, usize) {
    let colors = HistoryColors::from_palette(&Palette::day());
    let rich = entities_to_rich_text(&msg.text);
    let job = build_layout_job(
        &rich,
        &message_text_style(),
        &colors.text_palette(false),
        colors.text_fg,
        wrap.max(40.0),
        None,
        true,
    );
    let galley = ui.painter().layout_job(job);
    let last_w = galley
        .rows
        .last()
        .map_or(0.0, |r| r.rect_without_leading_space().width());
    (galley.size().y, last_w, galley.rows.len())
}

fn with_ui(f: impl FnOnce(&Ui)) {
    let ctx = Context::default();
    tg_ui::fonts::register_tdesktop_fonts(&ctx);
    let mut f = Some(f);
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
        if let Some(f) = f.take() {
            f(ui);
        }
    });
    out.textures_delta.clear();
}

fn assert_text_inside_bubble(ui: &Ui, msg: &HistoryMessage, label: &str) {
    let layout = layout_of(ui, msg);
    let text = layout.text_rect;
    let (painted_h, last_w, rows) = painted_rows(ui, msg, text.width as f32);
    let text_bottom = text.y as f32 + painted_h;
    let geometry_bottom = (layout.geometry.y + layout.geometry.height) as f32;
    let info = layout.bottom_info_rect;
    let info_w = info.width;
    let time_on_last_line = last_w.ceil() as i32 + MSG_DATE_SPACE + info_w <= text.width;
    println!(
        "measured: {label}: bubble {}x{}, text wrap {} -> {rows} rows, painted h {painted_h}, \
         reserved h {}, last row {last_w:.1}px, time on last line: {time_on_last_line}",
        layout.geometry.width, layout.geometry.height, text.width, text.height
    );
    assert!(
        painted_h <= text.height as f32 + 0.5,
        "{label}: painted text {painted_h}px is taller than the reserved {}px",
        text.height
    );
    assert!(
        text_bottom <= geometry_bottom,
        "{label}: last line ends at {text_bottom}, bubble ends at {geometry_bottom}"
    );
    if !time_on_last_line {
        // The time gets its own line under the text, never over it.
        assert!(
            info.y as f32 >= text_bottom - 1.0,
            "{label}: time at y={} overlaps the last text line ending at {text_bottom}",
            info.y
        );
    }
}

/// Extend `base` with Cyrillic words until its last painted line is within 24px of `wrap`.
fn caption_with_full_last_line(ui: &Ui, base: &str, photo: Option<(i32, i32)>) -> String {
    let mut text = base.to_owned();
    for _ in 0..400 {
        let msg = message(&text, photo);
        let wrap = layout_of(ui, &msg).text_rect.width as f32;
        let (_, last_w, _) = painted_rows(ui, &msg, wrap);
        if last_w >= wrap - 24.0 && last_w <= wrap {
            return text;
        }
        text.push_str(if text.len().is_multiple_of(2) {
            " ж"
        } else {
            "ї"
        });
    }
    panic!("could not build a caption whose last line fills the bubble");
}

#[test]
fn cyrillic_photo_caption_fits_inside_the_bubble() {
    with_ui(|ui| {
        // Portrait photo: the bubble is the photo's width, narrower than the widest bubble.
        let photo = Some((600, 900));
        assert_text_inside_bubble(ui, &message(CAPTION, photo), "caption (screenshot text)");
        let full = caption_with_full_last_line(ui, CAPTION, photo);
        assert_text_inside_bubble(ui, &message(&full, photo), "caption, full last line");
    });
}

#[test]
fn cyrillic_text_with_full_last_line_keeps_time_off_the_text() {
    with_ui(|ui| {
        let base = "Нова покупка Premium! Тариф: day (1 дн.), сума 85 XTR, транзакція \
                    8a77b8bb4d10, преміум до 23.11.2026 03:10 UTC; ґанок, їжак, єнот";
        let full = caption_with_full_last_line(ui, base, None);
        assert_text_inside_bubble(ui, &message(&full, None), "text, full last line");
        assert_text_inside_bubble(ui, &message("Так", None), "short text");
    });
}
