//! End-to-end UI tests for the Dialogs list, folders, search, archive, and pin/unpin context menu.

use egui::{Key, Modifiers, Pos2, Vec2};
use egui_kittest::kittest::Queryable;
use tg_app::dialogs::backend::{DialogListQuery, DialogsBackend};
use tg_app::dialogs::filters_menu::{SIDEBAR_FOLDER_BUTTON_HEIGHT, SIDEBAR_MENU_BUTTON_HEIGHT};
use tg_app::stand_in::mock_session::{session_folders, session_peers};
use tg_app::stand_in::types::FilterId;

use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, build_harness, click_at, press_key, right_click_at,
    settle_layout, type_text, wait_for_menu_item, wait_until,
};

const WORKSPACE_SIZE: Vec2 = Vec2::new(1280.0, 800.0);
/// Top search bar height in `DialogsPanel::show_search_top_bar` (`st::dialogsTopBarHeight = 54px`).
const DIALOGS_TOP_BAR_H: f32 = 54.0;
/// Default dialog & uncollapsed Archive row height (`DialogRowStyle::DEFAULT.height = 62px`).
const ROW_H: f32 = 62.0;

/// Computes the screen center of row `visual_row_index` inside the dialogs column.
///
/// When `FilterId::ALL` and archive exists, `visual_row_index = 0` is the Archive row,
/// and `visual_row_index = 1 + i` is dialog slice entry `i`.
fn dialog_row_center(
    harness: &egui_kittest::Harness<'_, AppTestState>,
    visual_row_index: usize,
) -> Pos2 {
    let geo = harness
        .state()
        .controller
        .session()
        .expect("session")
        .layout();
    let x = geo.filters_width as f32 + geo.dialogs_width as f32 * 0.5;
    let y = DIALOGS_TOP_BAR_H + (visual_row_index as f32 + 0.5) * ROW_H;
    Pos2::new(x, y)
}

#[test]
fn dialogs_open_chat_by_click() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // Initial active peer is GROUP_CORE (index 1 in initial dialogs).
    assert_eq!(
        harness
            .state()
            .controller
            .session()
            .expect("session")
            .active_peer(),
        session_peers::GROUP_CORE
    );

    // In FilterId::ALL with Archive row visible at index 0:
    // visual 0 = Archive, visual 1 = Saved Messages (pinned), visual 2 = Rustgram Core Port (pinned),
    // visual 3 = Mira Vance.
    let slice = harness.state().controller.mock_session().dialog_slice(
        DialogListQuery::Filter(FilterId::ALL),
        0,
        20,
    );
    let mira_idx = slice
        .entries
        .iter()
        .position(|d| d.peer_id == session_peers::USER_MIRA)
        .expect("Mira Vance must be in All Chats");

    let mira_pos = dialog_row_center(&harness, 1 + mira_idx);
    click_at(&mut harness, mira_pos);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking Mira Vance's dialog row must open her chat",
        |h| {
            !h.state().controller.is_animating()
                && h.state()
                    .controller
                    .session()
                    .is_some_and(|s| s.active_peer() == session_peers::USER_MIRA)
        },
    );
}

#[test]
fn dialogs_switch_folders_by_sidebar_click_and_keyboard_shortcuts() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    assert_eq!(
        harness
            .state()
            .controller
            .session()
            .expect("session")
            .active_filter(),
        FilterId::ALL
    );

    // Vertical folders sidebar is 72px wide on the left:
    // top 54px = hamburger menu, then 62px per folder:
    // idx 0 = All Chats, idx 1 = Personal, idx 2 = Work, idx 3 = Channels, ...
    let personal_btn = Pos2::new(
        36.0,
        SIDEBAR_MENU_BUTTON_HEIGHT + 1.5 * SIDEBAR_FOLDER_BUTTON_HEIGHT,
    );
    click_at(&mut harness, personal_btn);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking Personal in the vertical folders sidebar must switch active filter",
        |h| {
            h.state()
                .controller
                .session()
                .is_some_and(|s| s.active_filter() == session_folders::PERSONAL)
        },
    );

    // Switch to Work (slot 2 -> Ctrl+3) via global keyboard shortcut.
    press_key(&mut harness, Modifiers::COMMAND, Key::Num3);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Ctrl+3 must switch to the Work folder",
        |h| {
            h.state()
                .controller
                .session()
                .is_some_and(|s| s.active_filter() == session_folders::WORK)
        },
    );

    // Switch back to All Chats (slot 0 -> Ctrl+1).
    press_key(&mut harness, Modifiers::COMMAND, Key::Num1);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Ctrl+1 must return to All Chats",
        |h| {
            h.state()
                .controller
                .session()
                .is_some_and(|s| s.active_filter() == FilterId::ALL)
        },
    );
}

#[test]
fn dialogs_search_chat_via_accesskit_and_open() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // Find the dialogs Search TextInput via AccessKit (leftmost TextInput in the dialogs column).
    let geo = harness
        .state()
        .controller
        .session()
        .expect("session")
        .layout();
    let dialogs_right = (geo.filters_width + geo.dialogs_width) as f32;

    let search_input = harness
        .get_all_by_role(egui::accesskit::Role::TextInput)
        .find(|n| n.rect().center().x < dialogs_right && n.rect().center().y < DIALOGS_TOP_BAR_H)
        .expect("Dialogs Search TextInput should be discoverable via AccessKit");

    search_input.click();
    harness.step();
    type_text(&mut harness, "Theo");
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "typing in Search must activate search mode for 'Theo'",
        |h| {
            let dialogs = h.state_mut().controller.main_widget_mut().dialogs_mut();
            dialogs.search_active && dialogs.search_query == "Theo"
        },
    );

    // Verify search backend matches Theo Park and click the first search result row.
    // Search header tabs (34px) + "CHATS AND CONTACTS" section header (28px) + first row (62px).
    let first_result_pos = Pos2::new(
        geo.filters_width as f32 + geo.dialogs_width as f32 * 0.5,
        DIALOGS_TOP_BAR_H + 34.0 + 28.0 + 31.0,
    );
    click_at(&mut harness, first_result_pos);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking Theo Park in search results must open Theo Park's chat",
        |h| {
            !h.state().controller.is_animating()
                && h.state()
                    .controller
                    .session()
                    .is_some_and(|s| s.active_peer() == session_peers::USER_THEO)
        },
    );
}

#[test]
fn dialogs_open_archived_chats_and_escape_back() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // Visual row 0 in All Chats is the Archived Chats row.
    let archive_pos = dialog_row_center(&harness, 0);
    click_at(&mut harness, archive_pos);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking Archive row must open Archived Chats view",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .dialogs_mut()
                .opened_archive
        },
    );

    // Click the archived channel row (visual index 0 inside Archive view).
    let first_archived_pos = dialog_row_center(&harness, 0);
    click_at(&mut harness, first_archived_pos);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking the archived channel inside Archived Chats must open it",
        |h| {
            !h.state().controller.is_animating()
                && h.state()
                    .controller
                    .session()
                    .is_some_and(|s| s.active_peer() == session_peers::CHANNEL_ARCHIVE)
        },
    );

    // Escape exits Archived Chats back to All Chats.
    press_key(&mut harness, Modifiers::NONE, Key::Escape);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "pressing Escape must close Archived Chats view",
        |h| {
            !h.state_mut()
                .controller
                .main_widget_mut()
                .dialogs_mut()
                .opened_archive
        },
    );
}

#[test]
fn dialogs_pin_and_unpin_via_context_menu() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    let initial_pinned = harness
        .state()
        .controller
        .mock_session()
        .dialog_by_peer(session_peers::USER_MIRA)
        .expect("Mira dialog")
        .pinned;
    assert!(!initial_pinned, "Mira Vance starts unpinned");

    // Locate Mira Vance's row and right-click it.
    let slice = harness.state().controller.mock_session().dialog_slice(
        DialogListQuery::Filter(FilterId::ALL),
        0,
        20,
    );
    let mira_idx = slice
        .entries
        .iter()
        .position(|d| d.peer_id == session_peers::USER_MIRA)
        .expect("Mira in All Chats");
    let mira_pos = dialog_row_center(&harness, 1 + mira_idx);
    right_click_at(&mut harness, mira_pos);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "right-clicking a dialog row must open its context menu",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .dialogs_mut()
                .inner
                .is_context_menu_open()
        },
    );
    let pin_label = tg_lang::tr::lng_context_pin_to_top();
    wait_for_menu_item(&mut harness, &pin_label);

    harness.get_by_label(&pin_label).click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking 'Pin to top' in context menu must pin Mira Vance",
        |h| {
            h.state()
                .controller
                .mock_session()
                .dialog_by_peer(session_peers::USER_MIRA)
                .is_some_and(|d| d.pinned)
        },
    );

    // Now find Mira Vance's updated (pinned) row index, right-click again, and click "Unpin from top".
    let slice_after_pin = harness.state().controller.mock_session().dialog_slice(
        DialogListQuery::Filter(FilterId::ALL),
        0,
        20,
    );
    let pinned_idx = slice_after_pin
        .entries
        .iter()
        .position(|d| d.peer_id == session_peers::USER_MIRA)
        .expect("Mira still in All Chats");
    let pinned_pos = dialog_row_center(&harness, 1 + pinned_idx);
    right_click_at(&mut harness, pinned_pos);
    let unpin_label = tg_lang::tr::lng_context_unpin_from_top();
    wait_for_menu_item(&mut harness, &unpin_label);

    harness.get_by_label(&unpin_label).click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking 'Unpin from top' in context menu must unpin Mira Vance",
        |h| {
            h.state()
                .controller
                .mock_session()
                .dialog_by_peer(session_peers::USER_MIRA)
                .is_some_and(|d| !d.pinned)
        },
    );
}
