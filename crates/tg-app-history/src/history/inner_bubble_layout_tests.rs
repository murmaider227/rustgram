//! Group and channel bubble geometry as the history view computes it (TGB-2953), checked
//! against tdesktop's style constants.
//!
//! - A short incoming message keeps its time on the text line (`Element::skipBlockWidth`):
//!   the old code checked the fit with a rougher width than the bubble got, so every short
//!   incoming message took an extra line and attached messages looked far apart.
//! - The rank badge widens the name line by `msgPadding.right() + badge width`.
//! - Channel reaction chips wrap inside the bubble and never sit under the views and time.

use egui::{Context, Ui};
use tg_history::consts::{FontMetrics, MSG_PADDING};
use tg_text::TextWithEntities;

use super::*;
use crate::history::backend::{
    MessageExtra, ReactionCounter, SenderBadge, SenderBadgeRole, SenderNameStatus,
};

const VIEWPORT_W: i32 = 720;

fn message(id: i32, from: i64, text: &str) -> HistoryMessage {
    HistoryMessage {
        id: MsgId(id),
        peer_id: PeerId::from_channel(900),
        from_id: PeerId(from),
        sender_name: "Тестовий Автор".to_owned(),
        date_unix: 1_700_000_000 + id,
        outgoing: false,
        is_post: false,
        text: TextWithEntities::simple(text),
        media: None,
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

fn layout_of(ui: &Ui, msg: &HistoryMessage, attached: bool) -> tg_history::MessageLayout {
    let colors = HistoryColors::from_palette(&Palette::day());
    let mut item = build_list_item(msg, true, true);
    item.attach_state.attached_to_previous = attached;
    item.attach_state.bubble_attached_to_previous = attached;
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

fn overlaps(a: tg_history::Rect, b: tg_history::Rect) -> bool {
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}

#[test]
fn short_incoming_messages_keep_the_time_on_the_text_line() {
    with_ui(|ui| {
        let line_h = FontMetrics::default().normal.height;
        for text in [
            "с мд моим",
            "а гугл наоборот меньше мне может сделать чем клод",
        ] {
            let l = layout_of(ui, &message(2, 77, text), true);
            assert_eq!(
                l.geometry.height,
                MSG_PADDING.top + line_h + MSG_PADDING.bottom,
                "{text:?}: one text line with the time on it (tdesktop: 34px)"
            );
            // The info sits on the text line, right of the text.
            assert!(l.bottom_info_rect.y < l.text_rect.y + l.text_rect.height);
        }
    });
}

#[test]
fn attached_messages_are_two_pixels_apart() {
    with_ui(|ui| {
        let first = layout_of(ui, &message(1, 77, "у меня клод и так не задушен"), false);
        let next = layout_of(ui, &message(2, 77, "с мд моим"), true);
        // `msgMargin.bottom()` of the first + `msgMarginTopAttached` of the next.
        assert_eq!(first.margin_bottom + next.margin_top, 2);
        assert_eq!(
            next.message_rounding.top_left,
            tg_history::BubbleCornerRounding::Small
        );
    });
}

#[test]
fn rank_badge_and_premium_star_widen_the_name_line() {
    with_ui(|ui| {
        let plain = message(3, 77, "ок");
        let mut ranked = plain.clone();
        ranked.extra.sender_badge = Some(SenderBadge {
            text: "Оригинал Мудрец".to_owned(),
            role: SenderBadgeRole::Admin,
        });
        ranked.extra.sender_status = Some(SenderNameStatus {
            emoji_status_id: None,
            premium: true,
        });
        let (name_w, badge_w) = crate::history::name_line::name_line_widths(ui.painter(), &ranked);
        let (plain_name_w, _) = crate::history::name_line::name_line_widths(ui.painter(), &plain);
        assert_eq!(
            name_w,
            plain_name_w + crate::history::name_line::NAME_STATUS_WIDTH
        );
        assert!(badge_w > 17, "admin pill: 5 + text + 5");
        let l = layout_of(ui, &ranked, false);
        assert!(
            l.geometry.width >= MSG_PADDING.left + name_w + MSG_PADDING.right + 11 + badge_w,
            "bubble {} narrower than name {name_w} + badge {badge_w}",
            l.geometry.width
        );
    });
}

#[test]
fn channel_reactions_wrap_and_never_cover_views_and_time() {
    with_ui(|ui| {
        let mut post = message(4, 0, "Banked reset is online.");
        post.from_id = post.peer_id;
        post.is_post = true;
        post.views = Some(14_512);
        post.edited = true;
        post.reactions = [
            ("💩", 301),
            ("🤣", 59),
            ("🔥", 23),
            ("😡", 15),
            ("❤️", 11),
            ("😭", 11),
            ("👍", 8),
            ("🤡", 7),
            ("🖕", 7),
            ("👎", 1),
            ("👨‍💻", 1),
        ]
        .iter()
        .map(|(e, c)| ReactionCounter {
            emoji: (*e).to_owned(),
            count: *c,
            chosen: false,
        })
        .collect();
        let l = layout_of(ui, &post, false);
        assert_eq!(l.reaction_buttons.len(), post.reactions.len());
        let inner_right = l.geometry.x + l.geometry.width - MSG_PADDING.right;
        let first_row = l.reaction_buttons[0].y;
        assert!(
            l.reaction_buttons.iter().any(|b| b.y > first_row),
            "11 chips wrap onto a second row"
        );
        for b in &l.reaction_buttons {
            assert!(b.x + b.width <= inner_right, "chip {b:?} past the bubble");
            assert!(
                !overlaps(*b, l.bottom_info_rect),
                "chip {b:?} under the views/time {:?}",
                l.bottom_info_rect
            );
        }
        let bubble_bottom = l.geometry.y + l.geometry.height;
        assert!(l.bottom_info_rect.y + l.bottom_info_rect.height <= bubble_bottom);
    });
}
