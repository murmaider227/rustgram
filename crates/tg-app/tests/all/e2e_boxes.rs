//! End-to-end UI tests for Modal Boxes (`BoxHost`):
//! - Create Group 2-step wizard (Details -> Add Members -> Create)
//! - Add Contact box (First Name, Last Name, synthetic phone -> Add)
//! - Share / Forward box (search target, select chat tile, comment -> Send)

use egui::{Pos2, Vec2};
use egui_kittest::kittest::Queryable;
use tg_app::dialogs::filters_menu::SIDEBAR_MENU_BUTTON_HEIGHT;
use tg_app::history::backend::HistoryBackend;
use tg_app::info::InfoBackend;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::MsgId;
use tg_app::startup::ipc::IncomingActivation;
use tg_app::window::main_menu::MAIN_MENU_COVER_HEIGHT;

use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, build_harness, click_at, settle_animations, settle_layout,
    type_text, wait_for_label_settled, wait_until,
};

const WORKSPACE_SIZE: Vec2 = Vec2::new(1280.0, 820.0);

fn open_main_menu(harness: &mut egui_kittest::Harness<'_, AppTestState>) {
    click_at(harness, Pos2::new(36.0, SIDEBAR_MENU_BUTTON_HEIGHT * 0.5));
    wait_for_label_settled(harness, "New Group");
}

fn main_menu_item_pos(row_after_sep: usize) -> Pos2 {
    let base_y = MAIN_MENU_COVER_HEIGHT + 6.0 + 42.0 + 8.0;
    Pos2::new(120.0, base_y + (row_after_sep as f32 + 0.5) * 42.0)
}

#[test]
fn box_create_group_flow() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // 1. Open Main Menu -> click "New Group" (row 0 after separator).
    open_main_menu(&mut harness);
    click_at(&mut harness, main_menu_item_pos(0));
    // Wait for BoxHost enter animation and Step 1 controls to settle.
    wait_for_label_settled(&mut harness, "1d");

    // Find the Group Name TextInput inside the centered modal via AccessKit.
    let modal_center = WORKSPACE_SIZE * 0.5;
    let name_input = harness
        .get_all_by_role(egui::accesskit::Role::TextInput)
        .find(|n| (n.rect().center().x - modal_center.x).abs() < 260.0)
        .expect("CreateGroup group-name TextInput should be discoverable via AccessKit");
    name_input.click();
    harness.step();
    type_text(&mut harness, "Rust QA Guild");

    // Step 1 footer button ("Next") sits at the bottom-right of the centered BoxModal (width ~380px).
    // Locate the "1d" selectable label (present in Step 1 auto-delete row) to anchor footer Y cleanly!
    let auto_del_rect = harness.get_by_label("1d").rect();
    let next_btn_pos = Pos2::new(modal_center.x + 140.0, auto_del_rect.max.y + 42.0);
    click_at(&mut harness, next_btn_pos);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "CreateGroup wizard did not advance to Step 2 (Add Members)",
        |h| {
            !h.state().controller.is_animating()
                && h.query_by_label("1d").is_none()
                && h.query_all_by_role(egui::accesskit::Role::TextInput)
                    .any(|n| (n.rect().center().x - modal_center.x).abs() < 260.0)
        },
    );

    // Step 2 (Add Members) has a search input near the top of the modal body and contact rows below.
    let search_input = harness
        .get_all_by_role(egui::accesskit::Role::TextInput)
        .find(|n| (n.rect().center().x - modal_center.x).abs() < 260.0)
        .expect("AddMembers search TextInput should appear in Step 2");
    let search_rect = search_input.rect();

    // Select the first contact row directly below the search input.
    click_at(
        &mut harness,
        Pos2::new(search_rect.center().x, search_rect.max.y + 38.0),
    );
    settle_animations(&mut harness);

    // Click the bottom-right "Create (1)" button in Step 2 (modal uses `max_body_h = 420.0` scroll area).
    // Modal height = title (48) + scroll body (420) + padding/footer (~72) = ~540 -> bottom around center.y + 270.
    // Footer button center Y is ~26px above modal bottom.
    let create_btn_pos = Pos2::new(modal_center.x + 135.0, modal_center.y + 244.0);
    click_at(&mut harness, create_btn_pos);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "completing Create Group wizard must create 'Rust QA Guild' in MockSession",
        |h| {
            !h.state().controller.is_animating()
                && h.state().controller.box_host().is_empty()
                && h.state()
                    .controller
                    .mock_session()
                    .all_peers()
                    .iter()
                    .any(|p| p.title == "Rust QA Guild")
        },
    );

    let created = harness
        .state()
        .controller
        .mock_session()
        .all_peers()
        .into_iter()
        .find(|p| p.title == "Rust QA Guild");
    let created_id = created.expect("created group").id;
    assert_eq!(
        harness
            .state()
            .controller
            .session()
            .expect("session")
            .active_peer(),
        created_id,
        "newly created group should be opened as the active chat"
    );
}

#[test]
fn box_add_contact_flow() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    let contacts_before = InfoBackend::contacts(harness.state().controller.mock_session()).len();

    // Open Main Menu -> click "Contacts" (row 2 after separator, opens AddContactBox).
    open_main_menu(&mut harness);
    click_at(&mut harness, main_menu_item_pos(2));

    let modal_center = WORKSPACE_SIZE * 0.5;
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "AddContactBox must expose 3 settled TextInputs (first, last, phone)",
        |h| {
            !h.state().controller.is_animating()
                && h.query_all_by_role(egui::accesskit::Role::TextInput)
                    .filter(|n| (n.rect().center().x - modal_center.x).abs() < 260.0)
                    .count()
                    >= 3
        },
    );
    harness.step();

    let mut modal_inputs: Vec<_> = harness
        .get_all_by_role(egui::accesskit::Role::TextInput)
        .filter(|n| (n.rect().center().x - modal_center.x).abs() < 260.0)
        .collect();
    modal_inputs.sort_by(|a, b| {
        a.rect()
            .min
            .y
            .partial_cmp(&b.rect().min.y)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let first_rect = modal_inputs[0].rect();
    let last_rect = modal_inputs[1].rect();
    let phone_rect = modal_inputs[2].rect();

    click_at(&mut harness, first_rect.center());
    type_text(&mut harness, "Casey");

    click_at(&mut harness, last_rect.center());
    type_text(&mut harness, "Lin");

    click_at(&mut harness, phone_rect.center());
    type_text(&mut harness, "+999 661 5555");

    // Click the bottom-right "Add" button (`create`) directly below the phone field.
    let add_btn_pos = Pos2::new(phone_rect.max.x - 24.0, phone_rect.max.y + 42.0);
    click_at(&mut harness, add_btn_pos);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "submitting AddContactBox must add a new contact and close the modal",
        |h| {
            !h.state().controller.is_animating()
                && h.state().controller.box_host().is_empty()
                && InfoBackend::contacts(h.state().controller.mock_session()).len()
                    == contacts_before + 1
        },
    );

    let contacts_after = InfoBackend::contacts(harness.state().controller.mock_session());
    assert!(
        contacts_after.iter().any(|c| c.title == "Casey Lin"),
        "added contact 'Casey Lin' must appear in MockSession::contacts()"
    );
}

#[test]
fn box_share_flow() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    let mira_before = harness
        .state()
        .controller
        .mock_session()
        .load_slice_around(session_peers::USER_MIRA, MsgId::SHOW_AT_END, 50, 0)
        .messages
        .len();

    // Open ShareBox cleanly via `handle_incoming_activation` (`-sendpath` path), which pushes `BoxKind::Share`.
    let ctx = harness.ctx.clone();
    harness.state_mut().controller.handle_incoming_activation(
        IncomingActivation {
            send_paths: vec![std::path::PathBuf::from("release_notes.pdf")],
            ..IncomingActivation::default()
        },
        &ctx,
    );

    let modal_center = WORKSPACE_SIZE * 0.5;
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "ShareBox must open, settle, and expose its search TextInput",
        |h| {
            !h.state().controller.is_animating()
                && !h.state().controller.box_host().is_empty()
                && h.query_all_by_role(egui::accesskit::Role::TextInput)
                    .any(|n| (n.rect().center().x - modal_center.x).abs() < 260.0)
        },
    );
    harness.step();

    // ShareBox has a rounded search TextInput at top and a multiline comment TextInput at bottom.
    let mut modal_inputs: Vec<_> = harness
        .get_all_by_role(egui::accesskit::Role::TextInput)
        .chain(harness.get_all_by_role(egui::accesskit::Role::MultilineTextInput))
        .filter(|n| (n.rect().center().x - modal_center.x).abs() < 260.0)
        .collect();
    modal_inputs.sort_by(|a, b| {
        a.rect()
            .min
            .y
            .partial_cmp(&b.rect().min.y)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    assert!(
        !modal_inputs.is_empty(),
        "ShareBox must expose search TextInput via AccessKit"
    );
    let search_rect = modal_inputs[0].rect();

    // Filter avatar grid to "Mira" so Mira Vance is tile #0 (top-left of the 4-column grid)!
    click_at(&mut harness, search_rect.center());
    type_text(&mut harness, "Mira");
    settle_animations(&mut harness);

    // Click tile #0 (first column of the 4-column avatar grid, immediately below the search box).
    let tile_pos = Pos2::new(search_rect.min.x + 42.0, search_rect.max.y + 48.0);
    click_at(&mut harness, tile_pos);
    settle_animations(&mut harness);

    // Click the bottom-right "Send (1)" button in the ShareBox footer.
    // ShareBox uses scroll area with `max_body_h = 420.0`, width = box_wide_width(364) + 48 = 412.
    let send_btn_pos = Pos2::new(modal_center.x + 150.0, modal_center.y + 244.0);
    click_at(&mut harness, send_btn_pos);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "submitting ShareBox must forward the shared item into Mira Vance's chat",
        |h| {
            !h.state().controller.is_animating()
                && h.state().controller.box_host().is_empty()
                && h.state()
                    .controller
                    .mock_session()
                    .load_slice_around(session_peers::USER_MIRA, MsgId::SHOW_AT_END, 50, 0)
                    .messages
                    .len()
                    == mira_before + 1
        },
    );
}
