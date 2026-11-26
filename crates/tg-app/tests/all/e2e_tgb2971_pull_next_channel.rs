//! TGB-2971: scrolling to the bottom of a channel does not switch to the next unread channel.
//!
//! tdesktop switches only on a deliberate pull (`HistoryView::PullToNextChannel`): a phased
//! scroll (a finger, or a trackpad that reports begin and end) stretches the list past its
//! bottom by the threshold, stays there, and the fingers lift. A classic mouse wheel never
//! stretches the list. The old port counted every wheel step at the bottom and switched 220 ms
//! after the last one.

use egui::{Event, Modifiers, MouseWheelSource, MouseWheelUnit, Pos2, TouchPhase, Vec2};
use egui_kittest::kittest::Queryable;
use tg_app::history::pull_to_next_channel::{READY_DWELL_SECS, pull_threshold_px};
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_app::window::section::SectionShow;
use tg_lang::tr;

use crate::common::{AppTestState, TEST_STEP_DT, build_harness, settle_layout, step_frames};

const WORKSPACE: Vec2 = Vec2::new(1280.0, 820.0);

pub type AppHarness = egui_kittest::Harness<'static, AppTestState>;

/// A point over the message list of the open chat.
pub fn list_point(size: Vec2) -> Pos2 {
    Pos2::new(size.x * 0.7, size.y * 0.5)
}

/// One scroll event over the list, then one frame. `down_px` scrolls towards newer messages.
pub fn scroll(harness: &mut AppHarness, at: Pos2, phase: TouchPhase, down_px: f32) {
    harness.input_mut().events.push(Event::PointerMoved(at));
    harness.input_mut().events.push(Event::MouseWheel {
        unit: MouseWheelUnit::Point,
        delta: Vec2::new(0.0, -down_px),
        modifiers: Modifiers::NONE,
        phase,
        source: MouseWheelSource::Unknown,
    });
    harness.step();
}

/// Fingers down on a trackpad, `travel_px` of scrolling past the bottom in 10 px steps; the
/// fingers stay down.
pub fn pull(harness: &mut AppHarness, at: Pos2, travel_px: f32) {
    scroll(harness, at, TouchPhase::Start, 0.0);
    let mut sent = 0.0;
    while sent < travel_px {
        scroll(harness, at, TouchPhase::Move, 10.0);
        sent += 10.0;
    }
}

/// Keeps the fingers still for the dwell that arms the pull.
pub fn hold_for_the_dwell(harness: &mut AppHarness) {
    let frames = (READY_DWELL_SECS / f64::from(TEST_STEP_DT)).ceil() as usize + 1;
    step_frames(harness, frames);
}

/// The fingers lift.
pub fn release(harness: &mut AppHarness, at: Pos2) {
    scroll(harness, at, TouchPhase::End, 0.0);
}

fn open_channel_at_bottom(peer: PeerId) -> AppHarness {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE);
    settle_layout(&mut harness);
    harness
        .state_mut()
        .controller
        .session_mut()
        .expect("session")
        .show_peer_history(peer, SectionShow::clear_stack(), MsgId::SHOW_AT_END);
    settle_layout(&mut harness);
    harness
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .request_scroll_to_bottom(peer);
    settle_layout(&mut harness);
    harness
}

fn active_peer(harness: &AppHarness) -> Option<PeerId> {
    harness
        .state()
        .controller
        .session()
        .map(|session| session.active_peer())
}

fn stretch(harness: &mut AppHarness) -> f32 {
    harness
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .pull_next_channel_mut()
        .pull_px()
}

fn armed(harness: &mut AppHarness) -> bool {
    harness
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .pull_next_channel_mut()
        .is_ready()
}

#[test]
fn wheel_scrolling_to_the_bottom_and_past_it_does_not_switch_the_channel() {
    let mut harness = open_channel_at_bottom(session_peers::CHANNEL_DEV_LOG);
    let at = list_point(WORKSPACE);
    let pull_hint = tr::lng_pull_next_channel();

    // Up a few notches, then back down with ordinary wheel steps and far past the bottom.
    for _ in 0..6 {
        scroll(&mut harness, at, TouchPhase::Move, -120.0);
        step_frames(&mut harness, 2);
    }
    for step in 0..60 {
        scroll(&mut harness, at, TouchPhase::Move, 120.0);
        assert_eq!(
            stretch(&mut harness),
            0.0,
            "wheel step {step} stretched the list"
        );
        assert!(
            harness.query_by_label_contains(&pull_hint).is_none(),
            "wheel step {step} showed the pull hint"
        );
    }
    // A fast wheel: several steps in one frame, then nothing (the old port switched 220 ms
    // after the last step).
    for _ in 0..10 {
        for _ in 0..4 {
            harness.input_mut().events.push(Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: Vec2::new(0.0, -3.0),
                modifiers: Modifiers::NONE,
                phase: TouchPhase::Move,
                source: MouseWheelSource::Unknown,
            });
        }
        harness.step();
    }
    step_frames(&mut harness, 30);
    assert_eq!(stretch(&mut harness), 0.0);
    assert_eq!(
        active_peer(&harness),
        Some(session_peers::CHANNEL_DEV_LOG),
        "wheel scrolling at the bottom keeps the channel open"
    );
}

#[test]
fn a_pull_past_the_threshold_held_and_released_switches_the_channel() {
    let mut harness = open_channel_at_bottom(session_peers::CHANNEL_DEV_LOG);
    let at = list_point(WORKSPACE);

    pull(&mut harness, at, 100.0);
    assert!(
        (stretch(&mut harness) - 40.0).abs() < 0.5,
        "100 px of travel stretches the list by 40 px, got {}",
        stretch(&mut harness)
    );
    assert!(
        harness
            .query_by_label_contains(&tr::lng_pull_next_channel())
            .is_some(),
        "the hint over the bottom bar says to pull"
    );
    assert!(!armed(&mut harness));

    // On to 300 px of travel: past the threshold, and held there.
    for _ in 0..20 {
        scroll(&mut harness, at, TouchPhase::Move, 10.0);
    }
    assert!(stretch(&mut harness) >= pull_threshold_px());
    hold_for_the_dwell(&mut harness);
    assert!(
        armed(&mut harness),
        "held past the threshold, the pull arms"
    );
    step_frames(&mut harness, 10);
    assert!(
        harness
            .query_by_label_contains(&tr::lng_release_next_channel())
            .is_some(),
        "the hint says to release"
    );
    assert_eq!(
        active_peer(&harness),
        Some(session_peers::CHANNEL_DEV_LOG),
        "nothing switches while the fingers are down"
    );

    release(&mut harness, at);
    settle_layout(&mut harness);
    assert_eq!(
        active_peer(&harness),
        Some(session_peers::CHANNEL_RELEASES),
        "lifting the fingers from an armed pull opens the next unread channel"
    );
}

#[test]
fn a_flick_or_a_short_pull_at_the_bottom_does_not_switch_the_channel() {
    let mut harness = open_channel_at_bottom(session_peers::CHANNEL_DEV_LOG);
    let at = list_point(WORKSPACE);

    // A flick: far past the threshold, the fingers lift on the next frame.
    scroll(&mut harness, at, TouchPhase::Start, 0.0);
    scroll(&mut harness, at, TouchPhase::Move, 400.0);
    assert!(stretch(&mut harness) >= pull_threshold_px());
    release(&mut harness, at);
    step_frames(&mut harness, 45);
    assert_eq!(stretch(&mut harness), 0.0, "the stretch springs back");
    assert_eq!(active_peer(&harness), Some(session_peers::CHANNEL_DEV_LOG));

    // A short pull, held for a second.
    pull(&mut harness, at, 200.0);
    assert!(stretch(&mut harness) < pull_threshold_px());
    step_frames(&mut harness, 30);
    assert!(!armed(&mut harness));
    release(&mut harness, at);
    step_frames(&mut harness, 45);
    assert_eq!(active_peer(&harness), Some(session_peers::CHANNEL_DEV_LOG));
}
