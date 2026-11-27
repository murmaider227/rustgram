//! TGB-3024: a message whose text has blocks (a quote, a code block) selects through the list's
//! selection like plain text (`HistoryInner::_selectedTextSelection`), so the context menu,
//! Reply with Quote and a drag into the next message see it. Mock data only.

use egui::Vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tg_app_core::stand_in::types::MsgId;
use tg_text::{EntityInText, EntityType, TextWithEntities};

use crate::history::tests_w4_text_snap::{InnerState, inner_harness};
use crate::history::tests_w5_copy_snap::{
    FIRST_MSG, FIRST_TEXT, SECOND_MSG, SECOND_TEXT, copy_messages, drag, press_copy, right_click,
    step_until_copied, text_start,
};

/// The two copy-test messages, the first one's text all inside `kind`.
fn block_harness(kind: EntityType) -> Harness<'static, InnerState> {
    let mut messages = copy_messages();
    let len = i32::try_from(FIRST_TEXT.encode_utf16().count()).unwrap_or(i32::MAX);
    messages[0].text = TextWithEntities {
        text: FIRST_TEXT.to_owned(),
        entities: vec![EntityInText::new(kind, 0, len)],
    };
    inner_harness(messages, Vec2::new(720.0, 360.0))
}

/// A point on the first line of the block's text: a code block has its 20 px header
/// (`st::historyPre.header`) above the code.
fn in_block(harness: &Harness<'static, InnerState>, kind: EntityType) -> egui::Pos2 {
    let header = if kind == EntityType::Pre { 24.0 } else { 0.0 };
    text_start(harness, FIRST_MSG) + Vec2::new(24.0, 6.0 + header)
}

fn select_part_of_block(harness: &mut Harness<'static, InnerState>, kind: EntityType) -> String {
    let start = in_block(harness, kind);
    drag(harness, start, start + Vec2::new(110.0, 0.0));
    let selected = harness.state().inner.extract_selected_text();
    assert!(
        !selected.is_empty() && selected.len() < FIRST_TEXT.len(),
        "the list holds a part of the block's text: {selected:?}"
    );
    assert!(FIRST_TEXT.contains(&selected), "{selected:?}");
    selected
}

#[test]
fn a_drag_in_a_quote_is_the_list_selection_and_ctrl_c_copies_it() {
    let mut harness = block_harness(EntityType::Blockquote);
    let selected = select_part_of_block(&mut harness, EntityType::Blockquote);
    assert_eq!(press_copy(&mut harness), Some(selected));
}

#[test]
fn the_context_menu_over_a_code_block_selection_copies_the_selected_part() {
    let mut harness = block_harness(EntityType::Pre);
    let selected = select_part_of_block(&mut harness, EntityType::Pre);
    let over = in_block(&harness, EntityType::Pre) + Vec2::new(40.0, 0.0);
    right_click(&mut harness, over);
    assert_eq!(
        harness.state().inner.extract_selected_text(),
        selected,
        "a right click keeps the selection"
    );
    assert!(
        harness.query_by_label("Copy Text").is_none(),
        "the menu sees the selection, not the whole message"
    );
    harness.get_by_label("Copy Selected Text").click();
    assert_eq!(step_until_copied(&mut harness), Some(selected));
}

#[test]
fn a_drag_from_a_quote_into_the_next_message_selects_both() {
    let mut harness = block_harness(EntityType::Blockquote);
    let from = in_block(&harness, EntityType::Blockquote);
    let to = text_start(&harness, SECOND_MSG) + Vec2::new(30.0, 0.0);
    drag(&mut harness, from, to);
    let copied = press_copy(&mut harness).expect("copied");
    let lines: Vec<&str> = copied.lines().collect();
    assert_eq!(lines.len(), 2, "{copied:?}");
    assert!(lines[0].ends_with(&format!(": {FIRST_TEXT}")), "{copied:?}");
    assert!(
        lines[1].ends_with(&format!(": {SECOND_TEXT}")),
        "{copied:?}"
    );
}

#[test]
fn reply_with_quote_sees_a_selection_made_in_a_quote() {
    let mut harness = block_harness(EntityType::Blockquote);
    let selected = select_part_of_block(&mut harness, EntityType::Blockquote);
    let quote = harness
        .state()
        .inner
        .selected_quote_for_msg(MsgId(FIRST_MSG))
        .expect("a partial selection gives a quote");
    assert_eq!(quote.text, selected);
}
