//! End-to-end UI tests for Incoming Message Notifications:
//! - Simulate an incoming message into the shared `MockSession`
//! - Verify the in-app notification popup renders with sender title and message body via AccessKit
//! - Click the notification popup to open the sender's chat in `WindowController`
//! - Test `"Mark as read"` and `"×"` dismiss buttons via AccessKit

use egui::Vec2;
use egui_kittest::kittest::Queryable;
use tg_app::notifications::manager::MINIMAL_DELAY_MS;
use tg_app::stand_in::mock_session::session_peers;

use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, build_harness, click_at, right_click_at, settle_layout,
    type_text, wait_for_label_gone, wait_for_label_settled, wait_until,
};

const WORKSPACE_SIZE: Vec2 = Vec2::new(1280.0, 800.0);

#[test]
fn notification_simulate_incoming_and_click_opens_chat() {
    let mut harness = build_harness(AppTestState::main_with_notifications(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // Active chat starts on GROUP_CORE (not Mira Vance).
    assert_eq!(
        harness
            .state()
            .controller
            .session()
            .expect("session")
            .active_peer(),
        session_peers::GROUP_CORE
    );

    // 1. Simulate an incoming message (step 0 in MockSession::simulate_incoming_message is from USER_MIRA).
    let now_unix = harness
        .state()
        .notifications
        .as_ref()
        .expect("notify")
        .now_unix;
    let sim = harness
        .state()
        .controller
        .session_handle()
        .simulate_incoming_message(now_unix)
        .expect("simulated notification");
    assert_eq!(sim.peer_id, session_peers::USER_MIRA);

    // Tick once to ingest the IncomingNotifyEvent and schedule it, then advance past MINIMAL_DELAY_MS.
    harness.step();
    if let Some(ref mut n) = harness.state_mut().notifications {
        n.now_ms += MINIMAL_DELAY_MS + 50;
    }
    wait_for_label_settled(&mut harness, "Mira Vance");

    // 2. Verify custom popup card is visible via AccessKit with Mira Vance's title and message preview!
    let title_node = harness
        .query_by_label("Mira Vance")
        .expect("Notification popup title 'Mira Vance' should be discoverable via AccessKit");
    assert!(
        harness
            .query_by_label_contains("unified navigation wiring")
            .is_some(),
        "Notification popup body should display the simulated incoming message text"
    );

    // 3. Click the notification popup card to activate it and open Mira Vance's chat!
    let click_pos = title_node.rect().center();
    click_at(&mut harness, click_pos);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking an incoming message notification popup must open the sender's chat",
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
fn notification_popup_mark_as_read_button_via_accesskit() {
    let mut harness = build_harness(AppTestState::main_with_notifications(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    let now_unix = harness
        .state()
        .notifications
        .as_ref()
        .expect("notify")
        .now_unix;
    let _ = harness
        .state()
        .controller
        .session_handle()
        .simulate_incoming_message(now_unix);

    harness.step();
    if let Some(ref mut n) = harness.state_mut().notifications {
        n.now_ms += MINIMAL_DELAY_MS + 50;
    }
    wait_for_label_settled(&mut harness, "Mark as read");

    assert!(harness.query_by_label("Mira Vance").is_some());
    harness.get_by_label("Mark as read").click();
    wait_for_label_gone(&mut harness, "Mark as read");

    // Active chat should remain GROUP_CORE (Mark as read does not navigate away).
    assert_eq!(
        harness
            .state()
            .controller
            .session()
            .expect("session")
            .active_peer(),
        session_peers::GROUP_CORE
    );
}

#[test]
fn notification_popup_right_click_dismisses_and_inner_buttons_work() {
    let mut harness = build_harness(AppTestState::main_with_notifications(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // Helper to trigger and reveal one simulated incoming notification for a non-focused chat.
    let trigger_next = |h: &mut egui_kittest::Harness<'_, AppTestState>| {
        let active = h
            .state()
            .controller
            .session()
            .expect("session")
            .active_peer();
        let sim = loop {
            let now_unix = h.state().notifications.as_ref().expect("notify").now_unix;
            let candidate = h
                .state()
                .controller
                .session_handle()
                .simulate_incoming_message(now_unix)
                .expect("simulated notification");
            if candidate.peer_id != active {
                break candidate;
            }
        };
        h.step();
        if let Some(ref mut n) = h.state_mut().notifications {
            n.now_ms += MINIMAL_DELAY_MS + 50;
            n.now_unix += 5;
        }
        wait_for_label_settled(h, "Mark as read");
        sim
    };

    // 1. Right-click on the popup card dismisses it without changing the active chat.
    let sim1 = trigger_next(&mut harness);
    assert_eq!(sim1.peer_id, session_peers::USER_MIRA);
    let title_pos = harness.get_by_label("Mira Vance").rect().center();
    right_click_at(&mut harness, title_pos);
    // Card is immediately unlinked (action buttons removed) and fades out over NOTIFY_FAST_ANIM_MS.
    wait_for_label_gone(&mut harness, "Mark as read");
    if let Some(ref mut n) = harness.state_mut().notifications {
        n.now_ms += 300;
    }
    wait_for_label_gone(&mut harness, "Mira Vance");
    assert_eq!(
        harness
            .state()
            .controller
            .session()
            .expect("session")
            .active_peer(),
        session_peers::GROUP_CORE
    );

    // 2. Inner Close button dismisses the card without opening the chat.
    let _sim2 = trigger_next(&mut harness);
    wait_for_label_settled(&mut harness, "Close");
    harness.get_by_label("Close").click();
    wait_for_label_gone(&mut harness, "Mark as read");
    if let Some(ref mut n) = harness.state_mut().notifications {
        n.now_ms += 300;
    }
    wait_for_label_gone(&mut harness, "Close");
    assert_eq!(
        harness
            .state()
            .controller
            .session()
            .expect("session")
            .active_peer(),
        session_peers::GROUP_CORE
    );

    // 3. Inner Reply button opens inline reply field (without opening chat) and sends a quick reply.
    let sim3 = trigger_next(&mut harness);
    let reply_label = tg_lang::tr::lng_notification_reply();
    let send_label = tg_lang::tr::lng_send_button();
    harness.get_by_label(&reply_label).click();
    wait_for_label_settled(&mut harness, &send_label);
    assert_eq!(
        harness
            .state()
            .controller
            .session()
            .expect("session")
            .active_peer(),
        session_peers::GROUP_CORE,
        "clicking Reply button must not navigate away to the sender's chat"
    );

    // Focus the inline reply input inside the popup card, type, and click Send.
    let send_btn = harness.get_by_label(&send_label);
    let input_pos = egui::Pos2::new(send_btn.rect().min.x - 60.0, send_btn.rect().center().y);
    click_at(&mut harness, input_pos);
    type_text(&mut harness, "Quick reply from popup");
    harness.get_by_label(&send_label).click();
    if let Some(ref mut n) = harness.state_mut().notifications {
        n.now_ms += 300;
    }
    wait_for_label_gone(&mut harness, &send_label);

    let msgs = tg_app::history::backend::HistoryBackend::load_slice_around(
        harness.state().controller.mock_session(),
        sim3.peer_id,
        tg_app::stand_in::types::MsgId::SHOW_AT_END,
        50,
        0,
    )
    .messages;
    assert_eq!(
        msgs.last().expect("quick reply msg").text.text,
        "Quick reply from popup"
    );
}
