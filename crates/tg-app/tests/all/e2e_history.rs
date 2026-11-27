//! End-to-end UI tests for Chat History interactions:
//! - Send a message (appears at the bottom)
//! - Reply to a message
//! - Edit last outgoing message via Up arrow
//! - Delete a message via context menu + confirmation modal
//! - Forward a message to another chat
//! - React to a message
//! - Open Emoji panel and insert an emoji
//! - Format bold with `Ctrl+B`
//! - Open a forum topic & comments thread (both standalone `HistoryWidget` with `MockHistoryStore`
//!   and verifying `MockSession` integration)

use egui::{Key, Modifiers, Pos2, Rect, Vec2};
use egui_kittest::kittest::Queryable;
use tg_app::history::HistoryWidget;
use tg_app::history::backend::HistoryBackend;
use tg_app::history::compose::ComposeMode;
use tg_app::history::mock::MockHistoryStore;
use tg_app::history::mock_ext::{DEMO_CHANNEL_PEER_ID, DEMO_CHANNEL_POST_ID, DEMO_FORUM_PEER_ID};
use tg_app::history::sections_ui::HistorySectionKind;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::strings::EnglishStrings;
use tg_app::stand_in::theme::Palette;
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_app::window::section::SectionShow;
use tg_text::EntityType;

use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, build_harness, build_widget_harness, click_at, press_key,
    right_click_at, settle_animations, settle_layout, type_text, wait_for_label,
    wait_for_label_contains, wait_for_label_settled, wait_for_menu_item, wait_until,
};

const WORKSPACE_SIZE: Vec2 = Vec2::new(1280.0, 800.0);

/// Focuses the active chat's compose `TextEdit` via AccessKit (inside `last_compose_rect()`).
fn focus_compose_input(harness: &mut egui_kittest::Harness<'_, AppTestState>) {
    wait_until(
        harness,
        DEFAULT_MAX_FRAMES,
        "compose bar must be rendered before focusing",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .last_compose_rect()
                .is_positive()
        },
    );
    let compose_rect = harness
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .last_compose_rect();
    let node = harness
        .query_all_by_role(egui::accesskit::Role::TextInput)
        .chain(harness.query_all_by_role(egui::accesskit::Role::MultilineTextInput))
        .find(|n| compose_rect.contains(n.rect().center()))
        .expect("compose input TextEdit should be discoverable via AccessKit");
    node.click();
    harness.step();
}

/// Returns a point safely inside the bottom-most painted message row in the history list.
fn last_message_row_point(harness: &mut egui_kittest::Harness<'_, AppTestState>) -> Pos2 {
    let compose_rect = harness
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .last_compose_rect();
    // Bottom-most message sits directly above the compose bar (bottom-aligned with 12px padding).
    Pos2::new(compose_rect.center().x, compose_rect.top() - 36.0)
}

/// Switches to `peer_id` and waits until the chat transition and layout settle.
fn open_peer(harness: &mut egui_kittest::Harness<'_, AppTestState>, peer_id: PeerId) {
    harness
        .state_mut()
        .controller
        .session_mut()
        .expect("session")
        .show_peer_history(peer_id, SectionShow::clear_stack(), MsgId::SHOW_AT_END);
    wait_until(
        harness,
        DEFAULT_MAX_FRAMES,
        "switching active peer did not settle",
        |h| {
            !h.state().controller.is_animating()
                && h.state()
                    .controller
                    .session()
                    .is_some_and(|s| s.active_peer() == peer_id)
                && h.state_mut()
                    .controller
                    .main_widget_mut()
                    .history_mut()
                    .active_peer()
                    == peer_id
        },
    );
    settle_layout(harness);
}

#[test]
fn history_send_message_appears_at_bottom() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    let peer = session_peers::GROUP_CORE;
    let before_len = harness
        .state()
        .controller
        .mock_session()
        .load_slice_around(peer, MsgId::SHOW_AT_END, 50, 0)
        .messages
        .len();

    focus_compose_input(&mut harness);
    type_text(&mut harness, "Hello from egui_kittest E2E!");
    press_key(&mut harness, Modifiers::NONE, Key::Enter);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "sent message must appear in MockSession and HistoryInnerWidget",
        |h| {
            h.state()
                .controller
                .mock_session()
                .load_slice_around(peer, MsgId::SHOW_AT_END, 50, 0)
                .messages
                .len()
                == before_len + 1
                && h.state_mut()
                    .controller
                    .main_widget_mut()
                    .history_mut()
                    .inner()
                    .messages()
                    .last()
                    .is_some_and(|m| m.text.text == "Hello from egui_kittest E2E!")
        },
    );

    let after = harness
        .state()
        .controller
        .mock_session()
        .load_slice_around(peer, MsgId::SHOW_AT_END, 50, 0)
        .messages;
    let last = after.last().expect("new message");
    assert_eq!(last.text.text, "Hello from egui_kittest E2E!");
    assert!(last.outgoing);
}

#[test]
fn history_reply_to_message() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // Reply to the bottom-most message via Ctrl+Up shortcut (core Telegram Desktop shortcut).
    focus_compose_input(&mut harness);
    press_key(&mut harness, Modifiers::COMMAND, Key::ArrowUp);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Ctrl+Up must enter ComposeMode::Reply",
        |h| {
            matches!(
                h.state_mut()
                    .controller
                    .main_widget_mut()
                    .history_mut()
                    .compose_mut()
                    .mode,
                ComposeMode::Reply { .. }
            )
        },
    );

    let mode = harness
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .compose_mut()
        .mode
        .clone();
    let expected_reply_id = match mode {
        ComposeMode::Reply { msg_id, .. } => msg_id,
        other => panic!("expected ComposeMode::Reply after Ctrl+Up, got {other:?}"),
    };

    type_text(&mut harness, "Replying to your message!");
    press_key(&mut harness, Modifiers::NONE, Key::Enter);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "sent reply must appear in MockSession",
        |h| {
            h.state()
                .controller
                .mock_session()
                .load_slice_around(session_peers::GROUP_CORE, MsgId::SHOW_AT_END, 50, 0)
                .messages
                .last()
                .is_some_and(|m| m.text.text == "Replying to your message!")
        },
    );

    let msgs = harness
        .state()
        .controller
        .mock_session()
        .load_slice_around(session_peers::GROUP_CORE, MsgId::SHOW_AT_END, 50, 0)
        .messages;
    let last = msgs.last().expect("sent reply");
    assert_eq!(
        last.reply.as_ref().map(|r| r.reply_to_msg_id),
        Some(expected_reply_id),
        "sent reply must link back to the replied message"
    );
}

#[test]
fn history_edit_last_outgoing_with_up_arrow() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // Send a fresh outgoing message so we know its exact content.
    focus_compose_input(&mut harness);
    type_text(&mut harness, "Original outgoing line");
    press_key(&mut harness, Modifiers::NONE, Key::Enter);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "outgoing message must be sent before editing",
        |h| {
            h.state()
                .controller
                .mock_session()
                .load_slice_around(session_peers::GROUP_CORE, MsgId::SHOW_AT_END, 50, 0)
                .messages
                .last()
                .is_some_and(|m| m.text.text == "Original outgoing line")
        },
    );

    // Compose is now empty and focused; press Up arrow to enter Edit mode for our last outgoing message.
    press_key(&mut harness, Modifiers::NONE, Key::ArrowUp);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "pressing Up arrow in empty compose input must enter ComposeMode::Edit",
        |h| {
            let compose = h
                .state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .compose_mut();
            matches!(compose.mode, ComposeMode::Edit { .. })
                && compose.text == "Original outgoing line"
        },
    );

    // Replace text and submit edit.
    press_key(&mut harness, Modifiers::COMMAND, Key::A);
    press_key(&mut harness, Modifiers::NONE, Key::Backspace);
    type_text(&mut harness, "Edited outgoing line");
    press_key(&mut harness, Modifiers::NONE, Key::Enter);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "submitted edit must update last message and set edited = true",
        |h| {
            h.state()
                .controller
                .mock_session()
                .load_slice_around(session_peers::GROUP_CORE, MsgId::SHOW_AT_END, 50, 0)
                .messages
                .last()
                .is_some_and(|m| m.text.text == "Edited outgoing line" && m.edited)
        },
    );
}

#[test]
fn history_delete_message_via_context_menu_and_confirm() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // Open Mira Vance (only 2 short messages) and send one disposable message.
    open_peer(&mut harness, session_peers::USER_MIRA);
    focus_compose_input(&mut harness);
    type_text(&mut harness, "Temporary message to delete");
    press_key(&mut harness, Modifiers::NONE, Key::Enter);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "disposable message must appear in Mira Vance's chat",
        |h| {
            h.state()
                .controller
                .mock_session()
                .load_slice_around(session_peers::USER_MIRA, MsgId::SHOW_AT_END, 50, 0)
                .messages
                .last()
                .is_some_and(|m| m.text.text == "Temporary message to delete")
        },
    );
    settle_layout(&mut harness);

    let before = harness
        .state()
        .controller
        .mock_session()
        .load_slice_around(session_peers::USER_MIRA, MsgId::SHOW_AT_END, 50, 0)
        .messages;
    let target_id = before.last().expect("temp msg").id;
    let count_before = before.len();

    // Right-click the bottom-most message row and wait for PopupMenuState's 150ms reveal animation.
    let msg_pos = last_message_row_point(&mut harness);
    right_click_at(&mut harness, msg_pos);
    wait_for_menu_item(&mut harness, "Delete");

    // Click the compact 7.4.6 menu's "Delete" row where it is painted.
    let delete_item_pos = harness.get_by_label("Delete").rect().center();
    click_at(&mut harness, delete_item_pos);
    // tdesktop's DeleteMessagesBox has no title: the question is its first label.
    wait_for_label_settled(&mut harness, "Do you want to delete this message?");

    // The box's "Delete" button is the rightmost one, right of "Cancel" (`addButton` order).
    let cancel_rect = harness.get_by_label("Cancel").rect();
    let confirm_rect = harness.get_by_label("Delete").rect();
    assert!(confirm_rect.left() > cancel_rect.right());
    click_at(&mut harness, confirm_rect.center());
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "confirming delete must remove the message from MockSession",
        |h| {
            let msgs = h
                .state()
                .controller
                .mock_session()
                .load_slice_around(session_peers::USER_MIRA, MsgId::SHOW_AT_END, 50, 0)
                .messages;
            msgs.len() == count_before - 1 && !msgs.iter().any(|m| m.id == target_id)
        },
    );
}

#[test]
fn history_forward_message_to_another_chat() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // Open Theo Park (1 short incoming message, not outgoing -> exact known context menu items).
    open_peer(&mut harness, session_peers::USER_THEO);
    let msg_pos = last_message_row_point(&mut harness);
    right_click_at(&mut harness, msg_pos);
    wait_for_menu_item(&mut harness, "Forward");

    // Click the compact 7.4.6 menu's "Forward" row where it is painted.
    let forward_item_pos = harness.get_by_label("Forward").rect().center();
    click_at(&mut harness, forward_item_pos);
    // tdesktop opens the "Forward to..." box (`Window::ShowForwardMessagesBox`) instead of a
    // forward bar in the current chat.
    wait_for_forward_box(&mut harness);

    // Pick Sam Chen in the box and send the forwarded message.
    let before_sam = harness
        .state()
        .controller
        .mock_session()
        .load_slice_around(session_peers::USER_SAM, MsgId::SHOW_AT_END, 50, 0)
        .messages
        .len();
    submit_forward_box_to(&mut harness, session_peers::USER_SAM);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "forwarding must append a forwarded message to Sam Chen's chat",
        |h| {
            let msgs = h
                .state()
                .controller
                .mock_session()
                .load_slice_around(session_peers::USER_SAM, MsgId::SHOW_AT_END, 50, 0)
                .messages;
            msgs.len() == before_sam + 1
        },
    );
    assert!(
        !matches!(
            harness
                .state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .compose_mut()
                .mode,
            ComposeMode::Forward { .. }
        ),
        "the source chat's composer must not switch to a forward bar"
    );
}

fn wait_for_forward_box(harness: &mut egui_kittest::Harness<'static, crate::common::AppTestState>) {
    wait_until(
        harness,
        DEFAULT_MAX_FRAMES,
        "clicking Forward must open the Forward to... ShareBox",
        |h| {
            matches!(
                h.state_mut().controller.box_host_mut().top_kind(),
                Some(tg_app::boxes::BoxKind::Share(share)) if !share.message_ids.is_empty()
            )
        },
    );
}

fn submit_forward_box_to(
    harness: &mut egui_kittest::Harness<'static, crate::common::AppTestState>,
    target: PeerId,
) {
    let mut forwarded = Err(String::new());
    let session = harness.state().controller.session_handle();
    if let Some(tg_app::boxes::BoxKind::Share(share)) =
        harness.state_mut().controller.box_host_mut().top_kind_mut()
    {
        share.toggle_target(target);
        let mut backend = session;
        forwarded = share.submit(&mut backend);
    }
    assert!(forwarded.is_ok(), "ShareBox submit failed: {forwarded:?}");
    harness
        .state_mut()
        .controller
        .box_host_mut()
        .pop_immediate();
    settle_animations(harness);
}

#[test]
fn history_react_to_message_via_context_menu() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    open_peer(&mut harness, session_peers::USER_THEO);
    let target_id = harness
        .state()
        .controller
        .mock_session()
        .load_slice_around(session_peers::USER_THEO, MsgId::SHOW_AT_END, 50, 0)
        .messages
        .last()
        .expect("Theo message")
        .id;

    let msg_pos = last_message_row_point(&mut harness);
    right_click_at(&mut harness, msg_pos);
    wait_for_menu_item(&mut harness, "Forward");

    // 👍 in the reaction strip above the compact 7.4.6 context menu.
    let thumbs_up_pos = harness.get_by_label("👍").rect().center();
    click_at(&mut harness, thumbs_up_pos);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking 👍 reaction in context menu must add a chosen 👍 reaction",
        |h| {
            h.state()
                .controller
                .mock_session()
                .load_slice_around(session_peers::USER_THEO, MsgId::SHOW_AT_END, 50, 0)
                .messages
                .iter()
                .find(|m| m.id == target_id)
                .is_some_and(|m| {
                    m.reactions
                        .iter()
                        .any(|r| r.emoji == "👍" && r.chosen && r.count >= 1)
                })
        },
    );
}

#[test]
fn history_open_emoji_panel_and_insert_emoji() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    let compose_rect = harness
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .last_compose_rect();

    // Compose layout (`history/compose.rs:979-986`):
    // send_rect center X = compose_rect.right() - 26.0, width 36 -> send_rect.left() = right - 44.0.
    // emoji_rect center = (send_rect.left() - 22.0, compose_rect.bottom() - 24.0).
    let emoji_btn_pos = Pos2::new(
        compose_rect.right() - 44.0 - 22.0,
        compose_rect.bottom() - 24.0,
    );
    click_at(&mut harness, emoji_btn_pos);
    // Wait until TabbedPanel opens and its 200ms show animation settles.
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking compose emoji button must open and settle TabbedPanel",
        |h| {
            !h.state().controller.is_animating()
                && h.state_mut()
                    .controller
                    .main_widget_mut()
                    .tabbed_panel_mut()
                    .is_open()
        },
    );

    // Use TabbedPanel's own exact geometry calculation to click the first cell of the emoji grid.
    // Layout inside panel_rect:
    //   tab bar (38px) + search bar (~45px) + scroll margin (2px) + section header (30px) + spacing (~8px)
    //   -> first emoji row (38px tall) spans roughly [top + 123 .. top + 161].
    // At this width the selector docks as a 292 px third column (`TabbedSection`).
    let docked = harness
        .state_mut()
        .controller
        .main_widget_mut()
        .docked_tabbed_rect();
    let panel_rect = docked.unwrap_or_else(|| {
        harness
            .state_mut()
            .controller
            .main_widget_mut()
            .tabbed_panel_mut()
            .compute_panel_rect(
                compose_rect,
                Rect::from_min_size(Pos2::ZERO, WORKSPACE_SIZE),
            )
    });
    let first_emoji_pos = Pos2::new(panel_rect.left() + 28.0, panel_rect.top() + 142.0);
    click_at(&mut harness, first_emoji_pos);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking an emoji in the open TabbedPanel must insert it into the compose field",
        |h| {
            !h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .compose_mut()
                .text
                .is_empty()
        },
    );
}

#[test]
fn history_format_bold_with_ctrl_b() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    focus_compose_input(&mut harness);
    type_text(&mut harness, "Important bold text");
    // Select all text and toggle bold via Ctrl+B.
    press_key(&mut harness, Modifiers::COMMAND, Key::A);
    press_key(&mut harness, Modifiers::COMMAND, Key::B);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Ctrl+B on selected text must create an EntityType::Bold entity",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .compose_mut()
                .rich_edit
                .entities
                .iter()
                .any(|e| e.entity_type() == EntityType::Bold)
        },
    );

    // Send the formatted message and verify the Bold entity persists in MockSession!
    press_key(&mut harness, Modifiers::NONE, Key::Enter);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "sent bold message must preserve EntityType::Bold in MockSession",
        |h| {
            h.state()
                .controller
                .mock_session()
                .load_slice_around(session_peers::GROUP_CORE, MsgId::SHOW_AT_END, 50, 0)
                .messages
                .last()
                .is_some_and(|m| {
                    m.text.text == "Important bold text"
                        && m.text
                            .entities
                            .iter()
                            .any(|e| e.entity_type() == EntityType::Bold)
                })
        },
    );
}

#[test]
fn history_open_forum_topic_and_comments_thread_sections() {
    // Part A: Drive HistoryWidget over MockHistoryStore (which populates forum topics and
    // channel comments) via egui_kittest to verify the ForumTopics -> ForumTopicHistory
    // and Replies/Comments UI flows end-to-end.
    struct SectionState {
        widget: HistoryWidget,
        store: MockHistoryStore,
        peer: PeerId,
    }

    let mut widget = HistoryWidget::new();
    widget.open_section(HistorySectionKind::ForumTopics {
        peer_id: DEMO_FORUM_PEER_ID,
    });

    let mut harness = build_widget_harness(
        SectionState {
            widget,
            store: MockHistoryStore::new(),
            peer: DEMO_FORUM_PEER_ID,
        },
        Vec2::new(820.0, 680.0),
        |ui, st: &mut SectionState| {
            let pal = Palette::day();
            let strings = EnglishStrings;
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(pal.window_bg))
                .show(ui, |ui| {
                    let _ = st.widget.show_with_backend(
                        ui,
                        st.peer,
                        &mut st.store,
                        false,
                        false,
                        true,
                        &pal,
                        &strings,
                    );
                });
        },
    );
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "standalone ForumTopics section did not settle",
        |h| {
            matches!(
                h.state().widget.active_section(),
                Some(HistorySectionKind::ForumTopics { peer_id }) if peer_id == DEMO_FORUM_PEER_ID
            )
        },
    );

    // Click the first forum topic row (top bar is 54px, topic rows are 68px tall).
    let first_topic_pos = Pos2::new(300.0, 54.0 + 34.0);
    click_at(&mut harness, first_topic_pos);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking a forum topic row must open ForumTopicHistory",
        |h| {
            matches!(
                h.state().widget.active_section(),
                Some(HistorySectionKind::ForumTopicHistory { peer_id, .. })
                    if peer_id == DEMO_FORUM_PEER_ID
            )
        },
    );

    // Now open the comments thread for channel post `DEMO_CHANNEL_POST_ID` and post a comment via AccessKit!
    harness.state_mut().peer = DEMO_CHANNEL_PEER_ID;
    harness
        .state_mut()
        .widget
        .open_section(HistorySectionKind::Replies {
            peer_id: DEMO_CHANNEL_PEER_ID,
            root_msg_id: DEMO_CHANNEL_POST_ID,
        });
    wait_for_label_contains(&mut harness, "Channel Post");

    let comment_input = harness.get_by_role(egui::accesskit::Role::TextInput);
    comment_input.click();
    harness.step();
    type_text(&mut harness, "Great architecture update!");
    harness.get_by_label("Send").click();
    wait_for_label(&mut harness, "Great architecture update!");

    // Keep unused import check clean if needed
    let _ = Rect::NOTHING;
}

#[test]
fn history_mock_session_forum_topics_and_comments_wired_check() {
    // Part B: Verify whether the app's shared `MockSession` actually supports forum topics
    // and channel post comments threads (exposes wiring gap if unimplemented in MockSession).
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    let session_topics = harness
        .state()
        .controller
        .mock_session()
        .forum_topics(session_peers::GROUP_CORE);
    let channel_comments = harness
        .state()
        .controller
        .mock_session()
        .replies_root_and_comments(session_peers::CHANNEL_DEV_LOG, MsgId(301));

    assert!(
        !session_topics.is_empty(),
        "APP BUG: MockSession does not implement HistoryBackend::forum_topics (returns empty) and defines no forum supergroup peer"
    );
    assert!(
        channel_comments.is_some(),
        "APP BUG: MockSession does not implement HistoryBackend::replies_root_and_comments (returns None) and no channel post sets extra.replies_count"
    );
}

#[test]
fn history_forward_preserves_target_chat_draft_text() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // Save an existing draft in Sam Chen before initiating a forward from Theo Park.
    open_peer(&mut harness, session_peers::USER_SAM);
    focus_compose_input(&mut harness);
    type_text(&mut harness, "Draft before forward");
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "draft text must populate Sam Chen's compose input",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .compose_mut()
                .text
                == "Draft before forward"
        },
    );

    // Switch to Theo Park (flushes Sam Chen's draft) and forward Theo's message.
    open_peer(&mut harness, session_peers::USER_THEO);
    let msg_pos = last_message_row_point(&mut harness);
    right_click_at(&mut harness, msg_pos);
    wait_for_menu_item(&mut harness, "Forward");
    harness.get_by_label("Forward").click();
    wait_for_forward_box(&mut harness);

    let before_sam = harness
        .state()
        .controller
        .mock_session()
        .load_slice_around(session_peers::USER_SAM, MsgId::SHOW_AT_END, 50, 0)
        .messages
        .len();
    submit_forward_box_to(&mut harness, session_peers::USER_SAM);

    // Switch back to Sam Chen: the forwarded message arrived and the saved draft is restored.
    open_peer(&mut harness, session_peers::USER_SAM);
    let msgs = harness
        .state()
        .controller
        .mock_session()
        .load_slice_around(session_peers::USER_SAM, MsgId::SHOW_AT_END, 50, 0)
        .messages;
    assert_eq!(
        msgs.len(),
        before_sam + 1,
        "forward must reach Sam Chen's chat"
    );
    assert_eq!(
        harness
            .state_mut()
            .controller
            .main_widget_mut()
            .history_mut()
            .compose_mut()
            .text,
        "Draft before forward",
        "target chat's saved draft text must survive the forward"
    );
}

#[test]
fn history_forum_click_opens_topics_and_create_send_topic() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // Switch to another chat first, then click the forum supergroup row in the chat list.
    harness.get_by_label("Mira Vance (1 unread)").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking Mira Vance must switch active peer",
        |h| {
            !h.state().controller.is_animating()
                && h.state()
                    .controller
                    .session()
                    .is_some_and(|s| s.active_peer() == session_peers::USER_MIRA)
        },
    );

    // Clicking the forum supergroup opens its ForumTopics list in place of history (tdesktop behavior).
    // `Rustgram Core Port` was opened on startup and marked read before switching to Mira Vance,
    // so its dialog row label no longer has an unread suffix.
    harness.get_by_label("Rustgram Core Port").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking a forum supergroup in the dialogs list must open ForumTopics",
        |h| {
            !h.state().controller.is_animating()
                && h.state_mut()
                    .controller
                    .main_widget_mut()
                    .history_mut()
                    .active_section()
                    == Some(HistorySectionKind::ForumTopics {
                        peer_id: session_peers::GROUP_CORE,
                    })
                && h.query_by_label_contains("UI & Theme Architecture")
                    .is_some()
        },
    );

    // Click the "UI & Theme Architecture" topic row by AccessKit label (includes unread count).
    harness
        .get_by_label_contains("UI & Theme Architecture")
        .click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking topic row must open ForumTopicHistory for GROUP_CORE",
        |h| {
            !h.state().controller.is_animating()
                && matches!(
                    h.state_mut()
                        .controller
                        .main_widget_mut()
                        .history_mut()
                        .active_section(),
                    Some(HistorySectionKind::ForumTopicHistory { peer_id, .. })
                        if peer_id == session_peers::GROUP_CORE
                )
        },
    );
    settle_animations(&mut harness);

    let opened_topic = match harness
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .active_section()
    {
        Some(HistorySectionKind::ForumTopicHistory {
            peer_id,
            topic_root_id,
        }) if peer_id == session_peers::GROUP_CORE => topic_root_id,
        other => panic!("expected ForumTopicHistory for GROUP_CORE, got {other:?}"),
    };

    // Send a message inside the topic via the section compose bar (TextInput in right pane).
    let before_msgs = harness
        .state()
        .controller
        .mock_session()
        .load_topic_messages(session_peers::GROUP_CORE, opened_topic)
        .len();
    let topic_input_pos = harness
        .query_all_by_role(egui::accesskit::Role::TextInput)
        .find(|n| n.rect().center().x > 400.0)
        .expect("topic compose TextInput")
        .rect()
        .center();
    click_at(&mut harness, topic_input_pos);
    type_text(&mut harness, "Topic reply from E2E test");
    harness.get_by_label("Send").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "topic reply must be appended to MockSession",
        |h| {
            let msgs = h
                .state()
                .controller
                .mock_session()
                .load_topic_messages(session_peers::GROUP_CORE, opened_topic);
            msgs.len() == before_msgs + 1
                && msgs
                    .last()
                    .is_some_and(|m| m.text.text == "Topic reply from E2E test")
        },
    );

    // Verify creating a new forum topic persists in MockSession.
    let created = harness
        .state_mut()
        .controller
        .mock_session_mut()
        .create_forum_topic(
            session_peers::GROUP_CORE,
            "Accessibility Audit".to_owned(),
            0x40A7E3,
            None,
        )
        .expect("create topic");
    assert_eq!(created.title, "Accessibility Audit");
    assert!(
        harness
            .state()
            .controller
            .mock_session()
            .forum_topics(session_peers::GROUP_CORE)
            .iter()
            .any(|t| t.title == "Accessibility Audit")
    );
}

#[test]
fn history_channel_post_comments_button_opens_replies() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // Open channel "Rustgram Dev Log" via AccessKit dialog row.
    harness.get_by_label("Rustgram Dev Log (3 unread)").click();
    wait_for_label_settled(&mut harness, "3 comments");

    // Click the "3 comments" button under channel post MsgId(301) via AccessKit!
    let comments_btn = harness
        .query_by_label("3 comments")
        .expect("channel post must render an accessible '3 comments' button");
    let pos = comments_btn.rect().center();
    click_at(&mut harness, pos);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking '3 comments' under a channel post must open Replies section",
        |h| {
            !h.state().controller.is_animating()
                && h.state_mut()
                    .controller
                    .main_widget_mut()
                    .history_mut()
                    .active_section()
                    == Some(HistorySectionKind::Replies {
                        peer_id: session_peers::CHANNEL_DEV_LOG,
                        root_msg_id: MsgId(301),
                    })
                && h.query_all_by_role(egui::accesskit::Role::TextInput)
                    .any(|n| n.rect().center().x > 400.0)
        },
    );

    // Post a new comment and verify MockSession records it (TextInput in right pane).
    let comment_input_pos = harness
        .query_all_by_role(egui::accesskit::Role::TextInput)
        .find(|n| n.rect().center().x > 400.0)
        .expect("replies comment TextInput")
        .rect()
        .center();
    click_at(&mut harness, comment_input_pos);
    type_text(&mut harness, "Channel comments work end-to-end!");
    harness.get_by_label("Send").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "new channel comment must be recorded in MockSession",
        |h| {
            h.state()
                .controller
                .mock_session()
                .replies_root_and_comments(session_peers::CHANNEL_DEV_LOG, MsgId(301))
                .is_some_and(|(_, comments)| {
                    comments.len() == 4
                        && comments
                            .last()
                            .is_some_and(|c| c.text.text == "Channel comments work end-to-end!")
                })
        },
    );
}
