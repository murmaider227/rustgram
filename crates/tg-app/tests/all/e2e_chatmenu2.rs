//! Chat-list menu follow-ups: "Delete chat" / "Leave" open tdesktop's `DeleteChatBox`
//! (TGB-2876), "Unmute" shows the time left (TGB-2877), "Clear history" links to Auto-Delete
//! (TGB-2879) and message search results carry their media preview (TGB-2878).

use std::path::PathBuf;

use egui::{Pos2, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tg_app::dialogs::backend::{DialogListQuery, DialogMediaPreviewKind, DialogsBackend};
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::{FilterId, PeerId};
use tg_lang::tr;

use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, build_harness, build_wgpu_harness, click_at, right_click_at,
    settle_layout, step_frames, type_text, wait_for_menu_item, wait_until,
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

fn has_label(h: &Harness<'static, AppTestState>, label: &str) -> bool {
    h.query_by_label(label).is_some()
}

fn dialog_exists(h: &Harness<'static, AppTestState>, peer: PeerId) -> bool {
    h.state()
        .controller
        .mock_session()
        .dialog_by_peer(peer)
        .is_some()
}

#[test]
fn delete_bot_box_blocks_and_removes_from_folders() {
    let bot = session_peers::BOT_CATALOG;
    let mut h = build_harness(AppTestState::main_workspace(), SIZE);
    settle_layout(&mut h);
    assert!(
        !h.state()
            .controller
            .mock_session()
            .folders_including_peer(bot)
            .is_empty(),
        "the bot is an included chat of the Bots folder"
    );

    choose_menu_item(&mut h, bot, &tr::lng_profile_delete_conversation());
    // `DeleteChatBox`: the "Delete chat" header beside the userpic, the text and both rows.
    assert!(has_label(&h, &tr::lng_profile_delete_conversation()));
    let block = tr::lng_profile_block_bot();
    let folders = tr::lng_filters_checkbox_remove_bot();
    assert!(has_label(&h, &block), "Stop and block bot");
    assert!(has_label(&h, &folders), "Remove bot from all folders");
    click_label(&mut h, &block);
    click_label(&mut h, &folders);
    click_label(&mut h, &tr::lng_box_delete());

    let mock = h.state().controller.mock_session();
    assert!(!dialog_exists(&h, bot), "Delete removes the chat");
    // The mock drops the deleted peer's record, but the folders keep their own lists.
    assert!(
        mock.folders_including_peer(bot).is_empty(),
        "the checked row removed the bot from its folders"
    );
}

#[test]
fn delete_chat_finish_blocks_the_bot_before_deleting() {
    let bot = session_peers::BOT_CATALOG;
    let mut h = build_harness(AppTestState::main_workspace(), SIZE);
    let mock = h.state_mut().controller.mock_session_mut();
    assert!(!mock.is_peer_blocked(bot));
    tg_app_info::boxes::delete_chat::before_delete_conversation(mock, bot, true, false);
    assert!(mock.is_peer_blocked(bot), "Stop and block bot blocks it");
    // Blocking twice never unblocks (`blockedPeers().block`).
    tg_app_info::boxes::delete_chat::before_delete_conversation(mock, bot, true, false);
    assert!(mock.is_peer_blocked(bot));
    assert!(
        !mock.folders_including_peer(bot).is_empty(),
        "folders untouched"
    );
}

#[test]
fn leave_channel_box_says_leave_and_cancel_keeps_the_chat() {
    let channel = session_peers::CHANNEL_RELEASES;
    let mut h = build_harness(AppTestState::main_workspace(), SIZE);
    settle_layout(&mut h);
    choose_menu_item(&mut h, channel, &tr::lng_profile_leave_channel());
    assert!(has_label(&h, &tr::lng_filters_checkbox_remove_channel()));
    assert!(has_label(&h, &tr::lng_box_leave()));
    click_label(&mut h, &tr::lng_cancel());
    assert!(dialog_exists(&h, channel), "Cancel keeps the chat");
}

#[test]
fn unmute_row_shows_the_time_left() {
    let peer = session_peers::USER_THEO;
    let mut h = build_harness(AppTestState::main_workspace(), SIZE);
    settle_layout(&mut h);
    h.state_mut()
        .controller
        .mock_session_mut()
        .mute_dialog_for(peer, 2 * 3600)
        .expect("mute for 2 hours");
    step_frames(&mut h, 5);
    open_row_menu(&mut h, peer);
    wait_for_menu_item(&mut h, &format!("{} 2h", tr::lng_context_unmute()));
}

#[test]
fn clear_history_links_to_auto_delete() {
    let peer = session_peers::USER_THEO;
    let mut h = build_harness(AppTestState::main_workspace(), SIZE);
    settle_layout(&mut h);
    choose_menu_item(&mut h, peer, &tr::lng_profile_clear_history());
    let link = tr::lng_enable_auto_delete();
    assert!(has_label(&h, &link), "Enable Auto-Delete link");
    click_label(&mut h, &link);
    assert!(
        has_label(&h, &tr::lng_settings_save()),
        "the link opens the Auto-Delete box (its Save button)"
    );
}

#[test]
fn photo_search_result_carries_its_thumbnail() {
    let h = build_harness(AppTestState::main_workspace(), SIZE);
    let results = h.state().controller.mock_session().search_dialogs(
        &tg_app::dialogs::backend::SearchRequest {
            query: "studio poster".to_owned(),
            ..Default::default()
        },
    );
    let hit = results
        .messages
        .iter()
        .find(|m| m.peer_id == session_peers::USER_MIRA)
        .expect("the photo message matches");
    let preview = hit.media_preview.as_ref().expect("media preview");
    assert_eq!(preview.kind, DialogMediaPreviewKind::Photo);
    assert!(preview.stripped_thumb.is_some(), "16 px mini thumbnail");
}

/// After shots for the reference | before | after pairs (TGB-2876/2877/2878/2879).
#[test]
#[ignore = "captures PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_chatmenu2_snapshots() {
    let stage = std::env::var("RUSTGRAM_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned());
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("chatmenu2"));
    std::fs::create_dir_all(&dir).expect("create snap dir");
    let shots: [(&str, PeerId, String); 4] = [
        (
            "delete_user",
            session_peers::USER_THEO,
            tr::lng_profile_delete_conversation(),
        ),
        (
            "delete_bot",
            session_peers::BOT_CATALOG,
            tr::lng_profile_delete_conversation(),
        ),
        (
            "leave_channel",
            session_peers::CHANNEL_RELEASES,
            tr::lng_profile_leave_channel(),
        ),
        (
            "clear_history",
            session_peers::USER_THEO,
            tr::lng_profile_clear_history(),
        ),
    ];
    for (name, peer, label) in shots {
        let mut h = build_wgpu_harness(AppTestState::main_workspace(), SIZE);
        settle_layout(&mut h);
        choose_menu_item(&mut h, peer, &label);
        step_frames(&mut h, 30);
        let img = h.render().expect("wgpu render");
        img.save(dir.join(format!("{name}_1440x900_{stage}.png")))
            .expect("save png");
    }

    let mut h = build_wgpu_harness(AppTestState::main_workspace(), SIZE);
    settle_layout(&mut h);
    let peer = session_peers::USER_THEO;
    let _ = h
        .state_mut()
        .controller
        .mock_session_mut()
        .mute_dialog_for(peer, 2 * 3600);
    step_frames(&mut h, 5);
    open_row_menu(&mut h, peer);
    let unmute = tr::lng_context_unmute();
    let label = if stage == "after" {
        format!("{unmute} 2h")
    } else {
        unmute
    };
    wait_for_menu_item(&mut h, &label);
    step_frames(&mut h, 20);
    let img = h.render().expect("wgpu render");
    img.save(dir.join(format!("unmute_menu_1440x900_{stage}.png")))
        .expect("save png");

    // Message search with a photo result (TGB-2878).
    let mut h = build_wgpu_harness(AppTestState::main_workspace(), SIZE);
    settle_layout(&mut h);
    let geo = h.state().controller.session().expect("session").layout();
    let dialogs_right = (geo.filters_width + geo.dialogs_width) as f32;
    let input = h
        .get_all_by_role(egui::accesskit::Role::TextInput)
        .find(|n| n.rect().center().x < dialogs_right && n.rect().center().y < DIALOGS_TOP_BAR_H)
        .expect("dialogs search input");
    input.click();
    h.step();
    let query = "studio poster";
    type_text(&mut h, query);
    wait_until(&mut h, DEFAULT_MAX_FRAMES, "search mode is active", |h| {
        let dialogs = h.state_mut().controller.main_widget_mut().dialogs_mut();
        dialogs.search_active && dialogs.search_query == query
    });
    step_frames(&mut h, 40);
    let img = h.render().expect("wgpu render");
    img.save(dir.join(format!("search_photo_1440x900_{stage}.png")))
        .expect("save png");
}
