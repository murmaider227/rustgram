//! Wave 5 COPY package tests (TGB-3016): selected message text reaches the clipboard through
//! the backend's copy event (`Ctrl+C` on English and Russian layouts), a raw `Ctrl+C` key, and
//! the context menu's "Copy Selected Text". Mock data only; fixtures in `tests_w5_copy_snap`.

use egui::{Key, Modifiers, Vec2};
use egui_kittest::kittest::Queryable;

use crate::history::inner::InnerEvent;
use crate::history::tests_w4_text_snap::InnerState;
use crate::history::tests_w5_copy_snap::{
    FIRST_MSG, FIRST_TEXT, SECOND_MSG, SECOND_TEXT, copy_harness, drag, press_copy, right_click,
    step_until_copied, text_start,
};

/// Selects part of the first message's text with a mouse drag.
fn select_part_of_first(harness: &mut egui_kittest::Harness<'static, InnerState>) -> String {
    let start = text_start(harness, FIRST_MSG);
    drag(harness, start, start + Vec2::new(110.0, 0.0));
    let selected = harness.state().inner.extract_selected_text();
    assert!(
        !selected.is_empty() && selected.len() < FIRST_TEXT.len(),
        "a part of the text is selected: {selected:?}"
    );
    assert!(FIRST_TEXT.starts_with(&selected), "{selected:?}");
    selected
}

#[test]
fn the_backend_copy_event_copies_a_partial_selection() {
    // eframe sends `Event::Copy` (no `Key::C` event) for Ctrl+C, also on the Russian layout,
    // where it maps the shortcut through the physical C key. The old code waited for `Key::C`.
    let mut harness = copy_harness();
    let selected = select_part_of_first(&mut harness);
    harness.state_mut().events.clear();
    assert_eq!(press_copy(&mut harness), Some(selected));
    assert!(
        harness
            .state()
            .events
            .iter()
            .any(|e| matches!(e, InnerEvent::CopiedRich { summary, .. } if summary.is_empty())),
        "the rich copy is kept for pasting, with no toast: {:?}",
        harness.state().events
    );
}

#[test]
fn a_raw_ctrl_c_key_still_copies() {
    let mut harness = copy_harness();
    let selected = select_part_of_first(&mut harness);
    // A host that does not translate clipboard shortcuts sends the key itself. One frame, so
    // the copy shows in this frame's output.
    harness
        .input_mut()
        .events
        .push(egui::Event::ModifiersChanged(Modifiers::COMMAND));
    harness.input_mut().events.push(egui::Event::Key {
        key: Key::C,
        physical_key: Some(Key::C),
        pressed: true,
        repeat: false,
        modifiers: Modifiers::COMMAND,
    });
    let copied = step_until_copied(&mut harness);
    harness
        .input_mut()
        .events
        .push(egui::Event::ModifiersChanged(Modifiers::NONE));
    assert_eq!(copied, Some(selected));
}

#[test]
fn a_drag_across_two_messages_copies_both_with_time_and_author() {
    let mut harness = copy_harness();
    let from = text_start(&harness, FIRST_MSG);
    let to = text_start(&harness, SECOND_MSG) + Vec2::new(30.0, 0.0);
    drag(&mut harness, from, to);
    let copied = press_copy(&mut harness).expect("copied");
    let lines: Vec<&str> = copied.lines().collect();
    assert_eq!(lines.len(), 2, "{copied:?}");
    assert!(
        lines[0].starts_with('[') && lines[0].ends_with(&format!("] Mira Vance: {FIRST_TEXT}")),
        "{copied:?}"
    );
    assert!(
        lines[1].ends_with(&format!("] Mira Vance: {SECOND_TEXT}")),
        "{copied:?}"
    );
}

#[test]
fn selected_messages_are_tinted_below_their_date_badge() {
    let mut harness = copy_harness();
    let bubble = harness
        .state()
        .inner
        .last_painted_bubble_rect(tg_app_core::stand_in::types::MsgId(FIRST_MSG))
        .expect("painted");
    let pixel = |img: &image::RgbaImage, x: f32, y: f32| *img.get_pixel(x as u32, y as u32);
    let date_area = (12.0, bubble.top() - 14.0);
    let message_area = (12.0, bubble.center().y);
    let before = harness.render().expect("render");

    let from = text_start(&harness, FIRST_MSG);
    let to = text_start(&harness, SECOND_MSG) + Vec2::new(30.0, 0.0);
    drag(&mut harness, from, to);
    harness.step();
    let after = harness.render().expect("render");
    assert_eq!(
        pixel(&before, date_area.0, date_area.1),
        pixel(&after, date_area.0, date_area.1),
        "the date badge row is not tinted"
    );
    assert_ne!(
        pixel(&before, message_area.0, message_area.1),
        pixel(&after, message_area.0, message_area.1),
        "the selected message row is tinted"
    );
}

#[test]
fn the_context_menu_copies_the_selection() {
    let mut harness = copy_harness();
    let selected = select_part_of_first(&mut harness);
    let start = text_start(&harness, FIRST_MSG);
    right_click(&mut harness, start + Vec2::new(40.0, 0.0));
    assert_eq!(
        harness.state().inner.extract_selected_text(),
        selected,
        "a right click keeps the selection"
    );
    assert!(
        harness.query_by_label("Copy Text").is_none(),
        "tdesktop offers the whole-message copy only away from the selection"
    );
    harness.get_by_label("Copy Selected Text").click();
    assert_eq!(step_until_copied(&mut harness), Some(selected));
}

#[test]
fn the_context_menu_away_from_a_selection_copies_the_whole_message() {
    let mut harness = copy_harness();
    let start = text_start(&harness, SECOND_MSG);
    right_click(&mut harness, start + Vec2::new(20.0, 0.0));
    assert!(harness.query_by_label("Copy Selected Text").is_none());
    harness.get_by_label("Copy Text").click();
    assert_eq!(
        step_until_copied(&mut harness),
        Some(SECOND_TEXT.to_owned())
    );
}
