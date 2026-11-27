//! WP-78 (UI parity with Telegram Desktop 7.4.6, stage 3 findings).
//!
//! The ignored capture test writes before/after PNGs of the screens WP-78 touches under
//! `RUSTGRAM_SNAPSHOT_DIR` (or the temp dir); `RUSTGRAM_WP78_STAGE` names the stage
//! (`before` / `after`, default `after`). It draws the frameless title bar above the main window
//! like `app.rs` does when the native frame is off.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use egui::Vec2;
use egui_kittest::{Harness, HarnessBuilder};
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_app::window::chat_preview::FramelessTitleBarState;
use tg_app::window::section::{SectionShow, SettingsPage};
use tg_ui::fonts::register_tdesktop_fonts;

use crate::common::{AppTestState, TEST_STEP_DT, apply_deterministic_style, step_frames};

static WGPU_WP78_LOCK: Mutex<()> = Mutex::new(());

struct ShellState {
    app: AppTestState,
    title: FramelessTitleBarState,
}

fn build_shell_harness(app: AppTestState, size: Vec2) -> Harness<'static, ShellState> {
    HarnessBuilder::default()
        .with_size(size)
        .with_step_dt(TEST_STEP_DT)
        .with_render_cursor(false)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .renderer(crate::common::serialized_wgpu_renderer())
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
        .build_ui_state(
            |ui, st: &mut ShellState| {
                let palette = st.app.controller.palette();
                egui::Panel::top("wp78_title_bar")
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        st.title.show_title_bar(ui, "Rustgram", &palette);
                    });
                st.app.show(ui);
            },
            ShellState {
                app,
                title: FramelessTitleBarState::default(),
            },
        )
}

fn save_png(harness: &mut Harness<'static, ShellState>, path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create snap dir");
    }
    let img = harness.render().expect("wgpu render");
    img.save(path).expect("save png");
}

fn settle(h: &mut Harness<'static, ShellState>) {
    step_frames(h, 12);
}

fn fresh(night: bool, size: Vec2) -> Harness<'static, ShellState> {
    let mut app = AppTestState::main_workspace();
    if night {
        app.controller.toggle_night_mode();
    }
    let mut h = build_shell_harness(app, size);
    settle(&mut h);
    h
}

fn open_chat(h: &mut Harness<'static, ShellState>, peer: PeerId) {
    if let Some(session) = h.state_mut().app.controller.session_mut() {
        session.show_peer_history(peer, SectionShow::default(), MsgId::SHOW_AT_END);
    }
    settle(h);
}

fn show_emoji_panel(h: &mut Harness<'static, ShellState>) {
    h.state_mut()
        .app
        .controller
        .main_widget_mut()
        .tabbed_panel_mut()
        .show_immediate();
    settle(h);
}

#[test]
#[ignore = "captures WP-78 before/after PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_wp78_snapshots() {
    let _guard = WGPU_WP78_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stage = std::env::var("RUSTGRAM_WP78_STAGE").unwrap_or_else(|_| "after".to_owned());
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("wp78"));
    let size = Vec2::new(1440.0, 900.0);

    for night in [false, true] {
        let theme = if night { "night" } else { "day" };
        let shot = |name: &str| dir.join(format!("{name}_{theme}_{stage}.png"));

        // Emoji panel in the app theme.
        let mut h = fresh(night, size);
        open_chat(&mut h, session_peers::USER_MIRA);
        show_emoji_panel(&mut h);
        save_png(&mut h, &shot("emoji_panel"));

        // Settings layer over an open chat.
        let mut h = fresh(night, size);
        open_chat(&mut h, session_peers::USER_MIRA);
        if let Some(session) = h.state_mut().app.controller.session_mut() {
            session.show_settings(SettingsPage::Main);
        }
        settle(&mut h);
        save_png(&mut h, &shot("settings"));

        // User profiles in the third column: one with details, one with nothing public.
        for (name, peer) in [
            ("profile_mira", session_peers::USER_MIRA),
            ("profile_riley", session_peers::USER_RILEY),
            ("profile_bot", session_peers::BOT_CATALOG),
        ] {
            let mut h = fresh(night, size);
            open_chat(&mut h, peer);
            if let Some(session) = h.state_mut().app.controller.session_mut() {
                session.show_peer_info(peer, SectionShow::clear_stack());
            }
            settle(&mut h);
            save_png(&mut h, &shot(name));
        }

        // Main menu drawer.
        let mut h = fresh(night, size);
        h.state_mut().app.controller.open_main_menu();
        settle(&mut h);
        save_png(&mut h, &shot("main_menu"));

        // Group chat with the pinned bar.
        let mut h = fresh(night, size);
        open_chat(&mut h, session_peers::GROUP_CORE);
        save_png(&mut h, &shot("group_chat"));
    }
}
