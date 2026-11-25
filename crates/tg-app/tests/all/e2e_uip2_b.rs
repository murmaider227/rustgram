//! UI parity stage 2, Package B: before/after screenshots of the Settings pages
//! (main, notifications, privacy, chat, advanced; Day and Night) and of the chat view's
//! default Classic wallpaper, at 1440x900 and 390x844.
//!
//! Run with `RUSTGRAM_SNAPSHOT_DIR=<dir>` and `RUSTGRAM_UIP2B_SNAP_STAGE=before|after`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use egui::{Event, MouseWheelUnit, Pos2, Vec2};
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::MsgId;
use tg_app::window::section::{SectionShow, SettingsPage};

use crate::common::{AppTestState, build_wgpu_harness, settle_layout, step_frames};

const DESKTOP_SIZE: Vec2 = Vec2::new(1440.0, 900.0);
const MOBILE_SIZE: Vec2 = Vec2::new(390.0, 844.0);

static WGPU_UIP2B_LOCK: Mutex<()> = Mutex::new(());

fn snap_dir() -> PathBuf {
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("uip2_b"));
    std::fs::create_dir_all(&dir).expect("create snap dir");
    dir
}

fn save_harness_png(harness: &mut egui_kittest::Harness<'static, AppTestState>, path: &Path) {
    let img = harness.render().expect("wgpu render");
    img.save(path).expect("save png");
}

fn scroll_panel_down(
    harness: &mut egui_kittest::Harness<'static, AppTestState>,
    size: Vec2,
    pixels: f32,
) {
    let target = if size.x >= 900.0 {
        Pos2::new(size.x * 0.65, size.y * 0.50)
    } else {
        Pos2::new(size.x * 0.50, size.y * 0.50)
    };
    harness.input_mut().events.push(Event::PointerMoved(target));
    harness.step();
    let steps = 8;
    for _ in 0..steps {
        harness.input_mut().events.push(Event::PointerMoved(target));
        harness.input_mut().events.push(Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: Vec2::new(0.0, -(pixels / steps as f32)),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
        });
        harness.step();
    }
    step_frames(harness, 4);
}

#[allow(clippy::too_many_arguments)]
fn capture_settings(
    dir: &Path,
    name: &str,
    page: SettingsPage,
    night: bool,
    scroll: f32,
    size: Vec2,
    size_label: &str,
    stage: &str,
) {
    let mut state = AppTestState::main_workspace();
    if night {
        state.controller.toggle_night_mode();
    }
    let mut h = build_wgpu_harness(state, size);
    settle_layout(&mut h);
    if let Some(session) = h.state_mut().controller.session_mut() {
        session.show_settings(page);
    }
    settle_layout(&mut h);
    step_frames(&mut h, 6);
    if scroll > 0.0 {
        // Park the pointer off the rows afterwards so no hover highlight is captured.
        scroll_panel_down(&mut h, size, scroll);
    }
    h.input_mut()
        .events
        .push(Event::PointerMoved(Pos2::new(size.x - 2.0, size.y - 2.0)));
    step_frames(&mut h, 4);
    save_harness_png(
        &mut h,
        &dir.join(format!("{name}_{size_label}_{stage}.png")),
    );
}

#[test]
#[ignore = "captures before/after design review PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_uip2_b_snapshots() {
    let _guard = WGPU_UIP2B_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stage = std::env::var("RUSTGRAM_UIP2B_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned());
    let dir = snap_dir();

    for (size_label, size) in [("1440x900", DESKTOP_SIZE), ("390x844", MOBILE_SIZE)] {
        let pages: [(&str, SettingsPage, bool, f32); 8] = [
            ("13_settings_main", SettingsPage::Main, false, 0.0),
            ("13b_settings_main_lower", SettingsPage::Main, false, 600.0),
            (
                "14_settings_notifications",
                SettingsPage::Notifications,
                false,
                0.0,
            ),
            (
                "15_settings_privacy",
                SettingsPage::PrivacySecurity,
                false,
                0.0,
            ),
            ("16_settings_chat", SettingsPage::ChatSettings, false, 0.0),
            ("17_settings_advanced", SettingsPage::Advanced, false, 0.0),
            ("26_night_settings", SettingsPage::Main, true, 0.0),
            (
                "26b_night_notifications",
                SettingsPage::Notifications,
                true,
                0.0,
            ),
        ];
        for (name, page, night, scroll) in pages {
            capture_settings(&dir, name, page, night, scroll, size, size_label, &stage);
        }

        // Chat view with the default Classic wallpaper.
        for night in [false, true] {
            let mut state = AppTestState::main_workspace();
            if night {
                state.controller.toggle_night_mode();
            }
            state
                .controller
                .session_mut()
                .expect("session")
                .show_peer_history(
                    session_peers::USER_MIRA,
                    SectionShow::default(),
                    MsgId::SHOW_AT_END,
                );
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            step_frames(&mut h, 6);
            let name = if night {
                "wallpaper_chat_night"
            } else {
                "wallpaper_chat_day"
            };
            save_harness_png(
                &mut h,
                &dir.join(format!("{name}_{size_label}_{stage}.png")),
            );
        }
    }
}
