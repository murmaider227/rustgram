//! TGB-3026: a text drag that crosses messages selects them on release (the selection bar
//! opens), and a drag held above the list scrolls it (`DraggingScrollManager`). Mock data only.

use egui::{Event, Modifiers, PointerButton, Pos2, Rect, Vec2};
use egui_kittest::Harness;
use tg_app_core::stand_in::types::MsgId;

use crate::history::tests_w4_text_snap::{InnerState, group_peer, inner_harness, message};
use crate::history::tests_w5_copy_snap::{
    FIRST_MSG, FIRST_TEXT, SECOND_MSG, SECOND_TEXT, copy_harness, drag, press_copy, text_start,
};

fn press(harness: &mut Harness<'static, InnerState>, pos: Pos2, pressed: bool) {
    harness.input_mut().events.push(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    });
    harness.step();
}

fn move_to(harness: &mut Harness<'static, InnerState>, pos: Pos2) {
    harness.input_mut().events.push(Event::PointerMoved(pos));
    harness.step();
}

#[test]
fn a_released_drag_across_messages_selects_them_as_items() {
    let mut harness = copy_harness();
    let from = text_start(&harness, FIRST_MSG);
    let to = text_start(&harness, SECOND_MSG) + Vec2::new(30.0, 0.0);
    drag(&mut harness, from, to);
    let inner = &harness.state().inner;
    assert_eq!(
        inner.selected_msg_ids(),
        [MsgId(FIRST_MSG), MsgId(SECOND_MSG)],
        "the selection bar counts both messages"
    );
    assert_eq!(inner.selected_count(), 2);
    // Copying the selected messages gives what the drag copied before the release.
    let copied = press_copy(&mut harness).expect("copied");
    assert!(copied.contains(FIRST_TEXT) && copied.contains(SECOND_TEXT));
}

#[test]
fn a_drag_inside_one_message_stays_a_text_selection() {
    let mut harness = copy_harness();
    let start = text_start(&harness, FIRST_MSG);
    drag(&mut harness, start, start + Vec2::new(110.0, 0.0));
    let inner = &harness.state().inner;
    assert_eq!(
        inner.selected_count(),
        0,
        "no selection bar for a piece of text"
    );
    assert!(!inner.extract_selected_text().is_empty());
}

#[test]
fn holding_a_drag_above_the_list_scrolls_it_and_extends_the_selection() {
    let peer = group_peer();
    let messages: Vec<_> = (1..=40)
        .map(|id| {
            let mut m = message(peer, id, 1001, "Mira Vance", &format!("Line number {id}"));
            m.outgoing = true;
            m
        })
        .collect();
    let mut harness = inner_harness(messages, Vec2::new(720.0, 420.0));
    // The list leaves room above it, where the held pointer makes it scroll.
    harness.state_mut().list_rect =
        Rect::from_min_max(Pos2::new(0.0, 60.0), Pos2::new(720.0, 420.0));
    harness.run_steps(4);
    let before = harness.state().inner.scroll_offset_y();
    assert!(before > 200.0, "opened at the bottom: {before}");

    let start = text_start(&harness, 37);
    move_to(&mut harness, start);
    press(&mut harness, start, true);
    move_to(&mut harness, start + Vec2::new(30.0, 0.0));
    let above = Pos2::new(start.x, 20.0);
    for _ in 0..10 {
        move_to(&mut harness, above);
    }
    let after = harness.state().inner.scroll_offset_y();
    assert!(
        after < before - 100.0,
        "the list scrolled up while the drag was held above it: {before} -> {after}"
    );
    press(&mut harness, above, false);
    let selected = harness.state().inner.selected_count();
    assert!(
        selected > 8,
        "the selection grew past the first screen: {selected} messages"
    );
}
