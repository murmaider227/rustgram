//! TGB-3025: copying ignores "Restrict saving content". A protected message
//! (`HistoryItem::forbidsForward`) or a chat that does not allow forwarding
//! (`PeerData::allowsForwarding`) is not copied: `Ctrl+C` shows tdesktop's toast and the
//! context menu has no copy actions. Mock data only.

use egui::Vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use crate::history::inner::InnerEvent;
use crate::history::tests_w4_text_snap::{InnerState, group_peer, inner_harness};
use crate::history::tests_w5_copy_snap::{
    FIRST_MSG, FIRST_TEXT, copy_messages, drag, press_copy, right_click, text_start,
};

const GROUP_TOAST: &str = "Sorry, copying from this group is disabled by admins.";

fn harness(first_protected: bool) -> Harness<'static, InnerState> {
    let mut messages = copy_messages();
    messages[0].extra.no_forwards = first_protected;
    inner_harness(messages, Vec2::new(720.0, 360.0))
}

fn select_part_of_first(harness: &mut Harness<'static, InnerState>) {
    let start = text_start(harness, FIRST_MSG);
    drag(harness, start, start + Vec2::new(110.0, 0.0));
    let selected = harness.state().inner.extract_selected_text();
    assert!(FIRST_TEXT.starts_with(&selected) && !selected.is_empty());
}

fn refused_with_toast(harness: &Harness<'static, InnerState>) -> bool {
    harness
        .state()
        .events
        .iter()
        .any(|e| matches!(e, InnerEvent::CopyRestricted(t) if t == GROUP_TOAST))
}

#[test]
fn ctrl_c_in_a_protected_message_shows_the_toast_and_copies_nothing() {
    let mut harness = harness(true);
    select_part_of_first(&mut harness);
    harness.state_mut().events.clear();
    assert_eq!(press_copy(&mut harness), None);
    assert!(refused_with_toast(&harness), "{:?}", harness.state().events);
    assert!(
        !harness
            .state()
            .events
            .iter()
            .any(|e| matches!(e, InnerEvent::CopiedRich { .. })),
        "nothing reaches the paste buffer"
    );
}

#[test]
fn ctrl_c_in_a_chat_that_restricts_saving_content_is_refused() {
    let mut harness = harness(false);
    harness
        .state_mut()
        .store
        .set_copy_restricted(group_peer(), true);
    select_part_of_first(&mut harness);
    harness.state_mut().events.clear();
    assert_eq!(press_copy(&mut harness), None);
    assert!(refused_with_toast(&harness), "{:?}", harness.state().events);
}

#[test]
fn the_menu_of_a_protected_message_has_no_copy_actions() {
    let mut harness = harness(true);
    select_part_of_first(&mut harness);
    let start = text_start(&harness, FIRST_MSG);
    right_click(&mut harness, start + Vec2::new(40.0, 0.0));
    assert!(harness.query_by_label("Copy Selected Text").is_none());
    assert!(harness.query_by_label("Copy Text").is_none());
}

#[test]
fn an_unprotected_message_still_copies() {
    let mut harness = harness(false);
    select_part_of_first(&mut harness);
    assert!(press_copy(&mut harness).is_some());
    let start = text_start(&harness, FIRST_MSG);
    right_click(&mut harness, start + Vec2::new(300.0, 0.0));
    assert!(
        harness.query_by_label("Copy Text").is_some()
            || harness.query_by_label("Copy Selected Text").is_some()
    );
}
