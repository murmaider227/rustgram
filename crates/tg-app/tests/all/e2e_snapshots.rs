//! Headless `wgpu` image snapshot tests for Main Window in Day and Night modes.

use std::sync::Mutex;

use egui::Vec2;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::theme::ThemeMode;
use tg_core::settings::{CoreSettings, PowerSavingFlags};

use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, build_wgpu_harness, color_luminance, settle_layout,
    step_frames, wait_until,
};

const SNAPSHOT_SIZE: Vec2 = Vec2::new(1280.0, 768.0);

/// Serializes headless `wgpu` device creation & rendering across parallel test threads
/// so Windows DX12/WARP drivers never race during `cargo test`.
static WGPU_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Builds a snapshot harness with looping/ambient media animations disabled so rendered
/// pixels settle to a completely time-invariant state.
fn build_deterministic_snapshot_harness() -> egui_kittest::Harness<'static, AppTestState> {
    let mut state = AppTestState::main_workspace();
    let static_settings = CoreSettings {
        loop_animated_stickers: false,
        power_saving: PowerSavingFlags::ALL,
        ..CoreSettings::default()
    };
    state
        .controller
        .main_widget_mut()
        .apply_core_settings(&static_settings);
    let mut harness = build_wgpu_harness(state, SNAPSHOT_SIZE);
    settle_layout(&mut harness);
    // Advance past any single-shot Lottie sticker duration (1.0s = 30 frames at 1/30s)
    // so non-looping animated stickers settle permanently on frame 0, then pin scroll to bottom.
    step_frames(&mut harness, 35);
    harness
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .request_scroll_to_bottom(session_peers::GROUP_CORE);
    settle_layout(&mut harness);
    harness
}

#[test]
fn snapshot_main_window_day() {
    let _guard = WGPU_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut harness = build_deterministic_snapshot_harness();

    assert_eq!(harness.state().controller.theme_mode(), ThemeMode::Day);
    assert!(
        color_luminance(harness.state().controller.palette().window_bg) > 180.0,
        "Day theme background must be bright"
    );

    harness.snapshot("main_window_day");
}

#[test]
fn snapshot_main_window_night() {
    let _guard = WGPU_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut harness = build_deterministic_snapshot_harness();

    harness.state_mut().controller.toggle_night_mode();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Night mode toggle must apply dark theme",
        |h| {
            h.state().controller.theme_mode() == ThemeMode::Night
                && color_luminance(h.state().controller.palette().window_bg) < 80.0
        },
    );
    settle_layout(&mut harness);

    harness.snapshot("main_window_night");
}

#[test]
fn snapshot_is_independent_of_extra_settled_frames_and_clock_time() {
    let _guard = WGPU_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut harness = build_deterministic_snapshot_harness();
    let first = harness.render().expect("initial settled wgpu render");

    // Advance simulated time by 30 extra frames (1.0 s) after settling and render again;
    // settled MainWindow pixels must remain bit-for-bit identical.
    step_frames(&mut harness, 30);
    let second = harness.render().expect("post-advance settled wgpu render");

    assert_eq!(
        first.dimensions(),
        second.dimensions(),
        "settled snapshot dimensions must be invariant"
    );
    assert!(
        first.as_raw() == second.as_raw(),
        "settled snapshot pixels must be independent of extra elapsed frames / clock time"
    );
}
