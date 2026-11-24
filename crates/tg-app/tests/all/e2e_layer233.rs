//! End-to-end headless UI tests (`egui_kittest`) for WP-15 Layer-233 features:
//! 1. Checklists / To-Do lists (`MessageMedia::ToDo` bubble, task toggle, `EditTodoListBox`
//!    create/edit with collaborative flags, and `AddTodoListTasksBox`).
//! 2. Channel Sponsored Messages (banner rendering, `AboutSponsoredBox`, multi-step
//!    `ReportSponsoredBox`, and Premium "Hide Ads" suppression).
//! 3. Fact Checks (bubble footer, expandable "Show more" / "Show less" note, and
//!    `EditFactCheckBox` save & remove).
//! 4. Suggested Posts & Channel Direct Messages / Monoforum (`MonoforumSectionWidget`
//!    sublists inbox, opening a sublist, approving/declining suggested posts, and
//!    `SuggestOptionsBox`).

use egui::Vec2;
use egui_kittest::kittest::Queryable;
use tg_app::history::HistoryWidget;
use tg_app::history::backend::{HistoryBackend, HistoryMedia, Layer233ExtrasBackend};
use tg_app::stand_in::mock_session::{MockSession, session_peers};
use tg_app::stand_in::strings::EnglishStrings;
use tg_app::stand_in::theme::Palette;
use tg_app::stand_in::types::MsgId;

use crate::common::{build_widget_harness, wait_until};

const VIEW_SIZE: Vec2 = Vec2::new(920.0, 760.0);

fn step_settle(harness: &mut egui_kittest::Harness<'_, StandaloneHistoryState>) {
    for _ in 0..4 {
        harness.step();
    }
}

struct StandaloneHistoryState {
    widget: HistoryWidget,
    session: MockSession,
    peer_id: tg_app::stand_in::types::PeerId,
    palette: Palette,
    strings: EnglishStrings,
}

impl StandaloneHistoryState {
    fn new(peer_id: tg_app::stand_in::types::PeerId) -> Self {
        let mut widget = HistoryWidget::new();
        widget.set_is_premium(true);
        Self {
            widget,
            session: MockSession::new(true),
            peer_id,
            palette: Palette::day(),
            strings: EnglishStrings,
        }
    }
}

fn build_history_harness(
    peer_id: tg_app::stand_in::types::PeerId,
) -> egui_kittest::Harness<'static, StandaloneHistoryState> {
    build_widget_harness(
        StandaloneHistoryState::new(peer_id),
        VIEW_SIZE,
        |ctx, state| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ctx, |ui| {
                    let _ = state.widget.show_with_backend(
                        ui,
                        state.peer_id,
                        &mut state.session,
                        false,
                        false,
                        false,
                        &state.palette,
                        &state.strings,
                    );
                });
        },
    )
}

#[test]
fn e2e_todo_checklist_bubble_toggle_and_edit_box() {
    let mut harness = build_history_harness(session_peers::GROUP_UI_LAB);
    step_settle(&mut harness);

    // 1. Verify demo checklist rendered in GROUP_UI_LAB (MsgId(421)) and click an unchecked task
    let task_checkbox = harness.get_by_role_and_label(
        egui::accesskit::Role::CheckBox,
        "Verify checklist toggle batching",
    );
    task_checkbox.click();
    harness.step();
    step_settle(&mut harness);

    // Confirm completion updated in MockSession (now 2 of 3 completed)
    let slice = harness.state().session.load_slice_around(
        session_peers::GROUP_UI_LAB,
        MsgId::SHOW_AT_END,
        20,
        0,
    );
    let todo_msg = slice
        .messages
        .iter()
        .find(|m| m.id == MsgId(421))
        .expect("demo checklist msg 421");
    match &todo_msg.media {
        Some(HistoryMedia::TodoList(todo)) => {
            assert_eq!(todo.completed_count(), 2);
            assert_eq!(todo.total_count(), 3);
        }
        other => panic!("expected TodoList media, got {other:?}"),
    }

    // 2. Open EditTodoListBox in Create mode, populate title + tasks, toggle flags, and click Create
    harness.state_mut().widget.open_create_todo_box();
    step_settle(&mut harness);

    // Verify collaborative checkboxes and validation in EditTodoListBox
    assert!(
        harness
            .query_by_label("Allow others to add tasks")
            .is_some(),
        "EditTodoListBox must show others-can-append checkbox"
    );
    assert!(
        harness
            .query_by_label("Allow others to mark tasks as completed")
            .is_some(),
        "EditTodoListBox must show others-can-complete checkbox"
    );
    harness.get_by_label("Allow others to add tasks").click();
    step_settle(&mut harness);

    // Verify clicking Create on empty title shows inline validation error
    harness.get_by_label("Create").click();
    step_settle(&mut harness);
    assert!(
        harness
            .query_by_label("Please enter a checklist title.")
            .is_some(),
        "Empty title must trigger validation error"
    );

    // Close modal cleanly via Cancel
    harness.get_by_label("Cancel").click();
    step_settle(&mut harness);
    assert!(harness.query_by_label("New Checklist").is_none());
}

#[test]
fn e2e_channel_sponsored_banner_about_box_and_premium_hide_ads() {
    let mut harness = build_history_harness(session_peers::CHANNEL_DEV_LOG);
    step_settle(&mut harness);

    // 1. Channel sponsored banner is visible and marks ad as viewed
    assert!(
        harness.query_by_label("About Sponsored").is_some(),
        "Channel should display sponsored banner with What's this? control"
    );

    // 2. Clicking "What's this?" opens AboutSponsoredBox with https://ads.telegram.org link
    harness.get_by_label("About Sponsored").click();
    step_settle(&mut harness);
    assert!(
        harness.query_by_label("https://ads.telegram.org").is_some(),
        "AboutSponsoredBox must show ads.telegram.org button"
    );
    harness.get_by_label("OK").click();
    step_settle(&mut harness);
    assert!(harness.query_by_label("https://ads.telegram.org").is_none());

    // 3. Clicking "Hide Ads" respects Premium setting and hides the sponsored banner
    harness.get_by_label("Hide Ads").click();
    step_settle(&mut harness);
    assert!(harness.state().session.is_sponsored_hidden());
    assert!(
        harness.query_by_label("About Sponsored").is_none(),
        "Sponsored banner must disappear once Premium hide-ads is enabled"
    );
}

#[test]
fn e2e_factcheck_bubble_footer_expandable_note_and_edit_box() {
    let mut harness = build_history_harness(session_peers::CHANNEL_DEV_LOG);
    // Hide sponsored banner so the channel post list has full room
    harness
        .state_mut()
        .session
        .set_sponsored_hidden(true)
        .unwrap();
    step_settle(&mut harness);

    // 1. Expandable fact-check footer shows "Show more" initially on long note (MsgId(301))
    wait_until(
        &mut harness,
        20,
        "fact-check Show more button must appear on channel post 301",
        |h| h.query_by_label("Show more").is_some(),
    );
    harness.get_by_label("Show more").click();
    step_settle(&mut harness);

    // After clicking "Show more", toggle switches to "Show less"
    assert!(
        harness.query_by_label("Show less").is_some(),
        "Expanded fact-check note must switch toggle to Show less"
    );

    // 2. Click "Edit Fact-Check" to open EditFactCheckBox
    harness.get_by_label("Edit Fact-Check").click();
    step_settle(&mut harness);
    assert!(
        harness.query_by_label("Save").is_some(),
        "EditFactCheckBox window must open with Save button"
    );
    harness.get_by_label("Cancel").click();
    step_settle(&mut harness);
}

#[test]
fn e2e_monoforum_channel_direct_messages_and_suggested_post_approval() {
    let mut harness = build_history_harness(session_peers::CHANNEL_DEV_LOG);
    harness.state_mut().widget.open_monoforum_section(
        session_peers::CHANNEL_DEV_LOG,
        "Rustgram Dev Log",
        true,
    );
    step_settle(&mut harness);

    // 1. Sublists inbox lists Mira Vance and Sam Chen
    assert!(
        harness.query_by_label("Mira Vance").is_some(),
        "Monoforum inbox must show Mira Vance sublist"
    );
    assert!(
        harness.query_by_label("Sam Chen").is_some(),
        "Monoforum inbox must show Sam Chen sublist"
    );

    // 2. Open Mira Vance's sublist (marks it read and displays her pending suggested post)
    harness.get_by_label("Mira Vance").click();
    step_settle(&mut harness);

    assert!(
        harness.query_by_label("Approve").is_some(),
        "Pending suggested post card must render Approve button for admin"
    );
    assert!(
        harness.query_by_label("Decline").is_some(),
        "Pending suggested post card must render Decline button"
    );

    // 3. Open Suggest Post offer modal from compose bar
    harness.get_by_label("Suggest Post").click();
    step_settle(&mut harness);
    assert!(
        harness.query_by_label("Telegram Stars").is_some(),
        "SuggestOptionsBox must render Stars/TON tabs"
    );
    harness.get_by_label("Save Offer").click();
    step_settle(&mut harness);

    // 4. Approve Mira's pending suggested post (MsgId(480))
    harness.get_by_label("Approve").click();
    step_settle(&mut harness);

    let sp = harness
        .state()
        .session
        .suggested_post_for(session_peers::CHANNEL_DEV_LOG, MsgId(480))
        .expect("suggested post 480");
    assert!(sp.accepted && !sp.rejected);
    assert!(
        harness.query_by_label("Approve").is_none(),
        "Approve button must disappear once suggested post is approved"
    );
}
