//! UI parity stage 4, package F: "Clear history" / "Delete chat" from the chat-list menu open
//! tdesktop's `DeleteMessagesBox` (`ClearHistoryHandler` / `DeleteAndLeaveHandler`) and send
//! nothing before it is confirmed.

use std::path::PathBuf;

use egui::{Pos2, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tg_app::dialogs::backend::{DialogListQuery, DialogsBackend};
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::{FilterId, PeerId};

use crate::common::{
    AppTestState, build_harness, build_wgpu_harness, click_at, right_click_at, settle_layout,
    step_frames, wait_for_menu_item,
};

const SIZE: Vec2 = Vec2::new(1440.0, 900.0);
const ROW_H: f32 = 62.0;
const DIALOGS_TOP_BAR_H: f32 = 54.0;

/// Right-clicks `peer`'s chat-list row (the mock list starts with the Archive row).
fn open_row_menu(h: &mut Harness<'static, AppTestState>, peer: PeerId) {
    let slice = h.state().controller.mock_session().dialog_slice(
        DialogListQuery::Filter(FilterId::ALL),
        0,
        40,
    );
    let idx = slice
        .entries
        .iter()
        .position(|d| d.peer_id == peer)
        .expect("peer row in the chat list");
    let geo = h.state().controller.session().expect("session").layout();
    let pos = Pos2::new(
        geo.filters_width as f32 + 60.0,
        DIALOGS_TOP_BAR_H + (idx as f32 + 1.5) * ROW_H,
    );
    right_click_at(h, pos);
}

/// Clicks the first widget labelled `label` with a real pointer click.
fn click_label(h: &mut Harness<'static, AppTestState>, label: &str) {
    let center = h
        .query_by_label(label)
        .map(|n| n.rect().center())
        .unwrap_or_else(|| panic!("no widget labelled {label:?}"));
    click_at(h, center);
    step_frames(h, 20);
}

fn choose_menu_item(h: &mut Harness<'static, AppTestState>, peer: PeerId, label: &str) {
    open_row_menu(h, peer);
    wait_for_menu_item(h, label);
    click_label(h, label);
}

/// The chat's last-message preview (empty once its history is cleared).
fn last_message(h: &Harness<'static, AppTestState>, peer: PeerId) -> String {
    h.state()
        .controller
        .mock_session()
        .dialog_by_peer(peer)
        .map(|d| d.last_message)
        .unwrap_or_default()
}

fn dialog_exists(h: &Harness<'static, AppTestState>, peer: PeerId) -> bool {
    h.state()
        .controller
        .mock_session()
        .dialog_by_peer(peer)
        .is_some()
}

#[test]
fn chat_list_clear_and_delete_wait_for_confirm() {
    let peer = session_peers::USER_THEO;
    let mut h = build_harness(AppTestState::main_workspace(), SIZE);
    settle_layout(&mut h);
    let before = last_message(&h, peer);
    assert!(!before.is_empty(), "the mock chat has messages to clear");

    // "Clear history": the box opens, nothing is cleared yet.
    choose_menu_item(&mut h, peer, &tg_lang::tr::lng_profile_clear_history());
    assert_eq!(last_message(&h, peer), before, "cleared before Confirm");
    let delete = tg_lang::tr::lng_box_delete();
    assert!(
        h.query_by_label(&delete).is_some(),
        "DeleteMessagesBox with its Delete button is shown"
    );
    assert!(
        h.query_by_label(&tg_lang::tr::lng_cancel()).is_some(),
        "the box has Cancel next to Delete"
    );
    click_label(&mut h, &delete);
    assert!(
        last_message(&h, peer).is_empty(),
        "Confirm clears the history"
    );
    assert!(dialog_exists(&h, peer), "clearing keeps the chat");

    // "Delete chat": Cancel sends nothing, Confirm deletes.
    let delete_chat = tg_lang::tr::lng_profile_delete_conversation();
    choose_menu_item(&mut h, peer, &delete_chat);
    assert!(dialog_exists(&h, peer), "deleted before Confirm");
    click_label(&mut h, &tg_lang::tr::lng_cancel());
    assert!(dialog_exists(&h, peer), "Cancel keeps the chat");

    choose_menu_item(&mut h, peer, &delete_chat);
    assert!(dialog_exists(&h, peer), "deleted before Confirm");
    click_label(&mut h, &delete);
    assert!(!dialog_exists(&h, peer), "Confirm deletes the chat");
}

/// Before/after shots of the chat-list "Clear history" and "Delete chat" clicks.
#[test]
#[ignore = "captures PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_uip4_f_confirm_boxes() {
    let stage = std::env::var("RUSTGRAM_UIP4F_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned());
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("uip4_f"));
    std::fs::create_dir_all(&dir).expect("create snap dir");
    let peer = session_peers::USER_THEO;
    for (name, label) in [
        (
            "confirm_clear_history",
            tg_lang::tr::lng_profile_clear_history(),
        ),
        (
            "confirm_delete_chat",
            tg_lang::tr::lng_profile_delete_conversation(),
        ),
    ] {
        let mut h = build_wgpu_harness(AppTestState::main_workspace(), SIZE);
        settle_layout(&mut h);
        choose_menu_item(&mut h, peer, &label);
        step_frames(&mut h, 30);
        let img = h.render().expect("wgpu render");
        img.save(dir.join(format!("{name}_1440x900_{stage}.png")))
            .expect("save png");
    }
}
