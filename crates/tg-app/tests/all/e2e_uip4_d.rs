//! UI parity stage 4, package D (main window, main menu, column layout).
//!
//! The ignored capture test writes the package's screens under `RUSTGRAM_SNAPSHOT_DIR` (or the
//! temp dir) as `<screen>_<size>_<stage>.png`, with the frameless title bar drawn above the
//! window body the way `app.rs` does when the native frame is off.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use egui::{Pos2, Vec2};
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_app::window::main_menu::MAIN_MENU_COVER_HEIGHT;
use tg_app::window::platform_shell::PlatformWindowShell;
use tg_app::window::section::{SectionShow, SettingsPage};
use tg_ui::fonts::register_tdesktop_fonts;

use crate::common::{AppTestState, TEST_STEP_DT, apply_deterministic_style, step_frames};

static WGPU_UIP4_D_LOCK: Mutex<()> = Mutex::new(());

struct ShellState {
    app: AppTestState,
    shell: PlatformWindowShell,
}

fn build_shell_harness(app: AppTestState, size: Vec2, wgpu: bool) -> Harness<'static, ShellState> {
    let builder = HarnessBuilder::default()
        .with_size(size)
        .with_step_dt(TEST_STEP_DT)
        .with_render_cursor(false)
        .allow_missing_glyphs()
        .with_accessibility_check(false);
    let builder = if wgpu {
        builder.renderer(crate::common::serialized_wgpu_renderer())
    } else {
        builder
    };
    builder
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
        .build_ui_state(
            |ui, st: &mut ShellState| {
                // kittest frames the UI with an 8 px outer margin; draw edge to edge like the
                // real window so pixels and pointer positions match `app.rs`.
                let full = ui.ctx().content_rect();
                ui.scope_builder(egui::UiBuilder::new().max_rect(full), |ui| {
                    ui.set_clip_rect(full);
                    let palette = st.app.controller.palette();
                    st.shell
                        .show_frameless_title_bar(ui, "Rustgram", &palette, false, false);
                    st.app.show(ui);
                });
            },
            ShellState {
                app,
                shell: PlatformWindowShell::new_win32(),
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

fn fresh(night: bool, size: Vec2, wgpu: bool) -> Harness<'static, ShellState> {
    let mut app = AppTestState::main_workspace();
    if night {
        app.controller.toggle_night_mode();
    }
    let mut h = build_shell_harness(app, size, wgpu);
    settle(&mut h);
    h
}

fn show_chat(h: &mut Harness<'static, ShellState>, peer: PeerId) {
    if let Some(session) = h.state_mut().app.controller.session_mut() {
        session.show_peer_history(peer, SectionShow::clear_stack(), MsgId::SHOW_AT_END);
    }
    settle(h);
}

/// Drags the dialogs / chat splitter from its current x to `to_x` with real pointer events.
fn drag_first_splitter(h: &mut Harness<'static, ShellState>, to_x: f32) {
    let layout = h
        .state()
        .app
        .controller
        .session()
        .expect("session")
        .layout();
    let from_x = (layout.filters_width + layout.dialogs_width) as f32;
    let y = 400.0;
    h.hover_at(Pos2::new(from_x, y));
    h.step();
    h.drag_at(Pos2::new(from_x, y));
    h.step();
    let steps = 8;
    for i in 1..=steps {
        let x = from_x + (to_x - from_x) * i as f32 / steps as f32;
        h.hover_at(Pos2::new(x, y));
        h.step();
    }
    h.drop_at(Pos2::new(to_x, y));
    settle(h);
}

#[test]
#[ignore = "captures package D screens under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_uip4_d_snapshots() {
    let _guard = WGPU_UIP4_D_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stage = std::env::var("RUSTGRAM_UIP4_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned());
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("uip4_d"));
    let wide = Vec2::new(1440.0, 900.0);
    let narrow = Vec2::new(390.0, 844.0);
    let shot = |name: &str, size: &str| dir.join(format!("{name}_{size}_{stage}.png"));

    for (size_label, size) in [("1440x900", wide), ("390x844", narrow)] {
        for night in [false, true] {
            let theme = if night { "night" } else { "day" };
            let mut h = fresh(night, size, true);
            show_chat(&mut h, session_peers::SELF);
            h.state_mut().app.controller.open_main_menu();
            settle(&mut h);
            save_png(&mut h, &shot(&format!("main_menu_{theme}"), size_label));
        }

        // The chat list after leaving a chat, as the stage-3 capture reached it.
        let mut h = fresh(false, size, true);
        show_chat(&mut h, session_peers::USER_MIRA);
        show_chat(&mut h, PeerId::ZERO);
        save_png(&mut h, &shot("chat_list", size_label));

        let mut h = fresh(false, size, true);
        show_chat(&mut h, session_peers::USER_MIRA);
        save_png(&mut h, &shot("chat", size_label));
    }

    let mut h = fresh(false, wide, true);
    show_chat(&mut h, session_peers::SELF);
    if let Some(session) = h.state_mut().app.controller.session_mut() {
        session.show_settings(SettingsPage::Main);
    }
    settle(&mut h);
    save_png(&mut h, &shot("settings_layer", "1440x900"));

    let mut h = fresh(false, wide, true);
    show_chat(&mut h, session_peers::SELF);
    drag_first_splitter(&mut h, 810.0);
    save_png(&mut h, &shot("dialogs_wide", "1440x900"));
}

#[test]
fn main_menu_opens_below_the_title_bar() {
    let mut h = fresh(false, Vec2::new(1440.0, 900.0), false);
    h.state_mut().app.controller.open_main_menu();
    settle(&mut h);
    let title_bar_bottom = h.get_by_label("Rustgram").rect().bottom();
    let my_profile = h.get_by_label("My Profile").rect();
    // The cover (134 px) and the first `mainMenuSkip` sit between the title bar and the row.
    assert!(
        my_profile.top() >= title_bar_bottom + MAIN_MENU_COVER_HEIGHT,
        "My Profile at {my_profile:?}, title bar ends at {title_bar_bottom}"
    );
}

#[test]
fn dialogs_column_drags_past_540_up_to_the_chat_minimum() {
    let mut h = fresh(false, Vec2::new(1440.0, 900.0), false);
    show_chat(&mut h, session_peers::SELF);
    drag_first_splitter(&mut h, 810.0);
    let layout = h
        .state()
        .app
        .controller
        .session()
        .expect("session")
        .layout();
    let right = layout.filters_width + layout.dialogs_width;
    assert!(
        (right - 810).abs() <= 2,
        "dialogs column ends at {right}, expected about 810"
    );

    drag_first_splitter(&mut h, 1400.0);
    let layout = h
        .state()
        .app
        .controller
        .session()
        .expect("session")
        .layout();
    assert_eq!(
        layout.chat_width, 380,
        "the chat keeps columnMinimalWidthMain"
    );
}

#[test]
fn settings_layer_leaves_no_back_arrow_in_the_chat_header() {
    let mut h = fresh(false, Vec2::new(1440.0, 900.0), false);
    show_chat(&mut h, session_peers::SELF);
    if let Some(session) = h.state_mut().app.controller.session_mut() {
        session.show_settings(SettingsPage::Main);
    }
    settle(&mut h);
    assert!(
        h.query_by_label("Back").is_none(),
        "the chat header behind the Settings layer shows a Back arrow"
    );
}

#[test]
fn side_menu_bot_right_click_removes_it_from_the_menu() {
    let mut h = fresh(false, Vec2::new(1440.0, 900.0), false);
    h.state_mut().app.controller.open_main_menu();
    settle(&mut h);
    let remove = tg_lang::tr::lng_bot_remove_from_menu();
    assert!(h.query_by_label("Catalog Shop").is_some());
    assert!(h.query_by_label(&remove).is_none());

    h.get_by_label("Catalog Shop").click_secondary();
    settle(&mut h);
    h.get_by_label(&remove).click();
    settle(&mut h);

    assert!(
        h.query_by_label("Catalog Shop").is_none(),
        "the removed bot is still listed in the main menu"
    );
    assert!(
        h.query_by_label("My Profile").is_some(),
        "removing a bot closed the main menu"
    );
}
