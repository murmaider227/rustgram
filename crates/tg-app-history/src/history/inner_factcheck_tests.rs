//! TGB-3022: a channel post's fact-check card takes its own height in the text block, so the
//! reactions and the time come below it (`HistoryView::Factcheck`, a web-page-like block).

use egui::{Context, Ui};
use tg_history::consts::FontMetrics;
use tg_text::TextWithEntities;

use super::*;
use crate::history::backend::{MessageExtra, ReactionCounter};
use crate::history::factcheck::{FactCheckData, factcheck_rect, factcheck_reserved_height};

fn post(reactions: bool) -> HistoryMessage {
    HistoryMessage {
        id: MsgId(901),
        peer_id: PeerId(-1_000_901),
        from_id: PeerId(-1_000_901),
        sender_name: "Rustgram Dev Log".to_owned(),
        date_unix: 1_700_000_000,
        outgoing: false,
        is_post: true,
        text: TextWithEntities::simple(
            "Wave 8 is in: spoilers in reply headers, copy restrictions and large custom emoji.",
        ),
        media: None,
        service: None,
        reply: None,
        forward: None,
        reactions: if reactions {
            vec![ReactionCounter {
                emoji: "\u{1F44D}".to_owned(),
                count: 12,
                chosen: false,
            }]
        } else {
            Vec::new()
        },
        views: Some(1200),
        edited: false,
        is_read: true,
        is_sending: false,
        is_pinned: false,
        extra: MessageExtra::default(),
    }
}

fn factcheck() -> FactCheckData {
    FactCheckData {
        need_check: false,
        country: Some("US".to_owned()),
        text: Some(TextWithEntities::simple(
            "The release notes list these changes; the custom emoji part needs Premium to send. \
             Independent checks found the claim accurate. Read the full note for the details \
             of what was measured and how.",
        )),
        hash: 7,
    }
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

/// The post's layout with the card reserved, and the card's rect in element coordinates.
fn laid_out(ui: &Ui, msg: &HistoryMessage, expanded: bool) -> (tg_history::MessageLayout, Rect) {
    let colors = HistoryColors::from_palette(&Palette::day());
    let item = build_list_item(msg, false, true, false);
    let reserved = factcheck_reserved_height(&factcheck(), expanded);
    let layout = match compute_item_layout_egui(
        ui,
        msg,
        &item,
        720,
        false,
        false,
        &colors,
        &FontMetrics::default(),
        reserved,
        0,
        None,
    ) {
        CachedItemLayout::Message(layout) => *layout,
        CachedItemLayout::Service(_) => panic!("expected a message layout"),
    };
    let text = crate::history::bubble::hist_rect_to_egui(layout.text_rect, 0.0, 0.0);
    let date_line = if msg.reactions.is_empty() {
        layout.bottom_info.current_size.height as f32
    } else {
        0.0
    };
    (layout.clone(), factcheck_rect(text, reserved, date_line))
}

fn top(r: tg_history::Rect) -> f32 {
    r.y as f32
}

#[test]
fn the_reactions_start_below_the_factcheck_card() {
    with_ui(|ui| {
        for expanded in [false, true] {
            let (layout, card) = laid_out(ui, &post(true), expanded);
            assert!(!layout.reactions_rect.is_empty());
            assert!(
                card.bottom() <= top(layout.reactions_rect) + 0.5,
                "expanded {expanded}: card {card:?} vs reactions {:?}",
                layout.reactions_rect
            );
            // The "Show more" line is the card's last one: inside the reserved block.
            assert!(card.top() >= top(layout.text_rect), "{card:?}");
            assert!(card.height() > 40.0);
        }
    });
}

#[test]
fn the_time_comes_below_the_factcheck_card() {
    with_ui(|ui| {
        let (layout, card) = laid_out(ui, &post(false), false);
        assert!(
            card.bottom() <= top(layout.bottom_info_rect) + 0.5,
            "card {card:?} vs time {:?}",
            layout.bottom_info_rect
        );
    });
}
