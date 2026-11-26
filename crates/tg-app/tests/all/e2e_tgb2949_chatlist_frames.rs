//! TGB-2949: frames of the demo window that change no chat-list data read the chat list from the
//! backend only on the snapshot's age limit, never per frame, and prepare no row again.
//!
//! The demo session used to keep the trait's default `dialogs_version` (a new number on every
//! call), so the chat list re-read and re-converted every chat on every frame, and the history
//! column searched the whole chat list for its next-unread-channel hint on every frame.

use egui::{Event, Modifiers, MouseWheelSource, MouseWheelUnit, Pos2, TouchPhase, Vec2};
use tg_app::dialogs::backend::{DialogListQuery, DialogsBackend};
use tg_app::dialogs::list_cache::{DialogsFrameStats, SNAPSHOT_MAX_AGE_SECS};
use tg_app::stand_in::types::FilterId;

use crate::common::{AppTestState, TEST_STEP_DT, build_harness, settle_layout};

const WORKSPACE_SIZE: Vec2 = Vec2::new(1280.0, 800.0);
const ALL_CHATS: DialogListQuery = DialogListQuery::Filter(FilterId::ALL);

type AppHarness = egui_kittest::Harness<'static, AppTestState>;

fn stats(harness: &mut AppHarness) -> DialogsFrameStats {
    harness
        .state_mut()
        .controller
        .main_widget_mut()
        .dialogs_mut()
        .inner
        .frame_stats()
}

fn version(harness: &AppHarness) -> u64 {
    harness.state().controller.mock_session().dialogs_version()
}

/// Chat rows the demo session converted so far, by any caller.
fn items_built(harness: &AppHarness) -> u64 {
    harness
        .state()
        .controller
        .mock_session()
        .handle()
        .dialog_items_built()
}

/// Steps one frame and tells whether it converted any chat row.
fn frame_rebuilt_chats(harness: &mut AppHarness, step: impl FnOnce(&mut AppHarness)) -> bool {
    let before = items_built(harness);
    step(harness);
    items_built(harness) != before
}

/// Most list reads `frames` frames may make with no change: one per snapshot age, plus one for
/// a snapshot that was already old when they started.
fn age_reads(frames: usize) -> u64 {
    let secs = frames as f64 * f64::from(TEST_STEP_DT);
    (secs / SNAPSHOT_MAX_AGE_SECS).ceil() as u64 + 1
}

fn chat_list_point(harness: &AppHarness) -> Pos2 {
    let geo = harness
        .state()
        .controller
        .session()
        .expect("session")
        .layout();
    Pos2::new(
        geo.filters_width as f32 + geo.dialogs_width as f32 * 0.5,
        WORKSPACE_SIZE.y * 0.6,
    )
}

fn wheel(harness: &mut AppHarness, at: Pos2, dy: f32) {
    harness.input_mut().events.push(Event::PointerMoved(at));
    harness.input_mut().events.push(Event::MouseWheel {
        unit: MouseWheelUnit::Point,
        delta: Vec2::new(0.0, dy),
        modifiers: Modifiers::NONE,
        phase: TouchPhase::Move,
        source: MouseWheelSource::Unknown,
    });
    harness.step();
}

#[test]
fn idle_and_scroll_frames_do_not_rebuild_the_demo_chat_list() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);
    let at = chat_list_point(&harness);
    harness.input_mut().events.push(Event::PointerMoved(at));
    harness.step();
    assert!(
        !harness
            .state()
            .controller
            .session()
            .expect("session")
            .active_peer()
            .is_zero(),
        "a chat is open, so the history column runs too"
    );

    // Idle frames: nothing changes, so the version stays and no row is prepared again.
    const IDLE: usize = 90;
    let v0 = version(&harness);
    let s0 = stats(&mut harness);
    let mut rebuilding_frames = 0;
    for _ in 0..IDLE {
        if frame_rebuilt_chats(&mut harness, |h| h.step()) {
            rebuilding_frames += 1;
        }
    }
    let s1 = stats(&mut harness);
    assert!(
        rebuilding_frames <= age_reads(IDLE),
        "{rebuilding_frames} of {IDLE} idle frames converted chats (at most {} allowed)",
        age_reads(IDLE)
    );
    assert_eq!(
        version(&harness),
        v0,
        "an idle frame changed the demo session's chat-list version"
    );
    let idle_reads = s1.list_reads - s0.list_reads;
    assert!(
        idle_reads <= age_reads(IDLE),
        "{idle_reads} chat-list reads in {IDLE} idle frames (at most {} allowed)",
        age_reads(IDLE)
    );
    assert_eq!(
        s1.rows_prepared, s0.rows_prepared,
        "idle frames prepared rows again"
    );

    // Scroll frames: down and back up over the chat list.
    const SCROLL: usize = 40;
    let mut rebuilding_frames = 0;
    for i in 0..SCROLL {
        let dy = if i < SCROLL / 2 { -40.0 } else { 40.0 };
        if frame_rebuilt_chats(&mut harness, |h| wheel(h, at, dy)) {
            rebuilding_frames += 1;
        }
    }
    let s2 = stats(&mut harness);
    assert!(
        rebuilding_frames <= age_reads(SCROLL),
        "{rebuilding_frames} of {SCROLL} scroll frames converted chats (at most {} allowed)",
        age_reads(SCROLL)
    );
    assert_eq!(version(&harness), v0, "scrolling changed the version");
    let scroll_reads = s2.list_reads - s1.list_reads;
    assert!(
        scroll_reads <= age_reads(SCROLL),
        "{scroll_reads} chat-list reads in {SCROLL} scroll frames (at most {} allowed)",
        age_reads(SCROLL)
    );
}

#[test]
fn a_demo_change_moves_the_version_and_the_list_reads_it_on_the_next_frame() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    let slice = harness
        .state()
        .controller
        .mock_session()
        .dialog_slice(ALL_CHATS, 0, 50);
    let unpinned = slice
        .entries
        .iter()
        .find(|d| !d.pinned)
        .map(|d| d.peer_id)
        .expect("an unpinned demo chat");

    let v0 = version(&harness);
    let s0 = stats(&mut harness);
    assert!(
        DialogsBackend::toggle_pin(
            harness.state_mut().controller.mock_session_mut(),
            unpinned,
            ALL_CHATS,
        ),
        "pinning a demo chat"
    );
    assert!(version(&harness) > v0, "a pin did not move the version");

    harness.step();
    let s1 = stats(&mut harness);
    assert_eq!(
        s1.list_reads,
        s0.list_reads + 1,
        "the frame after a change reads the list once"
    );
    let shown = harness
        .state()
        .controller
        .mock_session()
        .dialog_slice(ALL_CHATS, 0, 50);
    assert!(
        shown
            .entries
            .iter()
            .any(|d| d.peer_id == unpinned && d.pinned),
        "the pinned chat is pinned in the list"
    );
}
