//! UI parity stage 2, Package A (window shell, layers, menus, input contrast).
//!
//! The ignored capture test writes before/after PNGs of the shell screens touched by Package A
//! under `RUSTGRAM_SNAPSHOT_DIR` (or the temp dir). It draws the frameless title bar above the
//! main window, the same way `app.rs` does when the native frame is off.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use egui::Vec2;
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use tg_app::boxes::BoxKind;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_app::window::chat_preview::FramelessTitleBarState;
use tg_app::window::main_widget::{SETTINGS_LAYER_WIDTH, settings_layer_rect};
use tg_app::window::section::{SectionMemento, SectionShow, SettingsPage};
use tg_ui::fonts::register_tdesktop_fonts;

use crate::common::{
    AppTestState, TEST_STEP_DT, apply_deterministic_style, build_harness, step_frames,
};

static WGPU_UIP2_A_LOCK: Mutex<()> = Mutex::new(());

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
        .wgpu()
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
        .build_ui_state(
            |ui, st: &mut ShellState| {
                let palette = st.app.controller.palette();
                egui::Panel::top("uip2a_title_bar")
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

fn open_chat(h: &mut Harness<'static, ShellState>) {
    if let Some(session) = h.state_mut().app.controller.session_mut() {
        session.show_peer_history(
            session_peers::USER_MIRA,
            SectionShow::default(),
            MsgId::SHOW_AT_END,
        );
    }
    settle(h);
}

#[test]
#[ignore = "captures Package A before/after PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_uip2_a_snapshots() {
    let _guard = WGPU_UIP2_A_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stage = std::env::var("RUSTGRAM_UIP2_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned());
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("uip2_a"));

    for (size_label, size) in [
        ("1440x900", Vec2::new(1440.0, 900.0)),
        ("390x844", Vec2::new(390.0, 844.0)),
    ] {
        for night in [false, true] {
            let theme = if night { "night" } else { "day" };
            let shot = |name: &str| dir.join(format!("{name}_{theme}_{size_label}_{stage}.png"));

            // Chat list + empty chat pane (title bar, stories strip, proxy pill, search box).
            let mut h = fresh(night, size);
            if let Some(session) = h.state_mut().app.controller.session_mut() {
                session.show_peer_history(
                    PeerId::ZERO,
                    SectionShow::clear_stack(),
                    MsgId::SHOW_AT_END,
                );
            }
            settle(&mut h);
            save_png(&mut h, &shot("chat_list"));

            // Main menu drawer.
            let mut h = fresh(night, size);
            h.state_mut().app.controller.open_main_menu();
            settle(&mut h);
            save_png(&mut h, &shot("main_menu"));

            // Settings (layer at wide sizes, full section when narrow).
            let mut h = fresh(night, size);
            open_chat(&mut h);
            if let Some(session) = h.state_mut().app.controller.session_mut() {
                session.show_settings(SettingsPage::Main);
            }
            settle(&mut h);
            save_png(&mut h, &shot("settings"));

            // Chat with typed composer text (Day input contrast) and emoji panel.
            let mut h = fresh(night, size);
            open_chat(&mut h);
            h.state_mut()
                .app
                .controller
                .main_widget_mut()
                .history_mut()
                .compose_mut()
                .text = "Typed composer text".to_owned();
            h.state_mut()
                .app
                .controller
                .main_widget_mut()
                .tabbed_panel_mut()
                .show_immediate();
            settle(&mut h);
            save_png(&mut h, &shot("emoji_panel"));
            h.state_mut()
                .app
                .controller
                .main_widget_mut()
                .tabbed_panel_mut()
                .hide_fast();

            // Forward from the message menu.
            h.state_mut()
                .app
                .controller
                .main_widget_mut()
                .request_forward_box(session_peers::USER_MIRA, vec![MsgId(1)]);
            settle(&mut h);
            save_png(&mut h, &shot("forward"));
        }
    }
}

fn plain_harness(size: Vec2) -> Harness<'static, AppTestState> {
    let mut h = build_harness(AppTestState::main_workspace(), size);
    step_frames(&mut h, 4);
    h
}

fn open_chat_plain(h: &mut Harness<'static, AppTestState>) {
    if let Some(session) = h.state_mut().controller.session_mut() {
        session.show_peer_history(
            session_peers::USER_MIRA,
            SectionShow::default(),
            MsgId::SHOW_AT_END,
        );
    }
    step_frames(h, 4);
}

#[test]
fn settings_open_as_centered_layer_when_wide_and_as_section_when_narrow() {
    let mut wide = plain_harness(Vec2::new(1440.0, 900.0));
    open_chat_plain(&mut wide);
    if let Some(session) = wide.state_mut().controller.session_mut() {
        session.show_settings(SettingsPage::Main);
    }
    step_frames(&mut wide, 4);
    assert!(
        wide.state_mut()
            .controller
            .main_widget_mut()
            .settings_shown_as_layer(),
        "1440 px window must show Settings as a layer over the chat"
    );
    let layer = settings_layer_rect(egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        Vec2::new(1440.0, 900.0),
    ));
    assert!((layer.width() - SETTINGS_LAYER_WIDTH).abs() < 0.5);
    assert!((layer.center().x - 720.0).abs() <= 1.0);
    assert!((layer.top() - 37.0).abs() < 0.5);
    // The layer's close button returns to the chat.
    wide.get_by_label("Close").click();
    step_frames(&mut wide, 4);
    assert!(
        !matches!(
            wide.state_mut()
                .controller
                .session_mut()
                .and_then(|s| s.main_section().cloned()),
            Some(SectionMemento::Settings { .. })
        ),
        "the layer close button must close Settings"
    );

    let mut narrow = plain_harness(Vec2::new(390.0, 844.0));
    if let Some(session) = narrow.state_mut().controller.session_mut() {
        session.show_settings(SettingsPage::Main);
    }
    step_frames(&mut narrow, 4);
    assert!(
        !narrow
            .state_mut()
            .controller
            .main_widget_mut()
            .settings_shown_as_layer(),
        "390 px window is below Info::LayerWidget::MinimalSupportedWidth"
    );
}

/// 7.4.6 has one top bar over Settings in both modes (`Info::LayerWidget` when wide,
/// `Info::WrapWidget` when narrow), so one search button and, once it is clicked, one search
/// field. The hidden controller top bar must not leave a second button in the AccessKit tree.
#[test]
fn settings_has_one_search_button_and_one_search_field_in_both_modes() {
    let label = tg_lang::tr::lng_dlg_filter();
    for (size, as_layer) in [
        (Vec2::new(1440.0, 900.0), true),
        (Vec2::new(390.0, 844.0), false),
    ] {
        let mut h = plain_harness(size);
        if let Some(session) = h.state_mut().controller.session_mut() {
            session.show_settings(SettingsPage::Main);
        }
        step_frames(&mut h, 4);
        assert_eq!(
            h.state_mut()
                .controller
                .main_widget_mut()
                .settings_shown_as_layer(),
            as_layer
        );
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        let area = if as_layer {
            settings_layer_rect(window)
        } else {
            window
        };
        let search_buttons = |h: &Harness<'static, AppTestState>| {
            h.query_all_by_role_and_label(egui::accesskit::Role::Button, &label)
                .filter(|n| area.contains(n.rect().center()))
                .map(|n| n.rect())
                .collect::<Vec<_>>()
        };
        let search_fields = |h: &Harness<'static, AppTestState>| {
            h.query_all_by_role(egui::accesskit::Role::TextInput)
                .filter(|n| area.contains(n.rect().center()))
                .count()
        };
        let buttons = search_buttons(&h);
        assert_eq!(
            buttons.len(),
            1,
            "{size:?}: Settings search buttons {buttons:?}"
        );
        assert_eq!(
            search_fields(&h),
            0,
            "{size:?}: search field shown before the click"
        );
        h.query_all_by_label(&label)
            .find(|n| n.rect() == buttons[0])
            .expect("search button")
            .click();
        step_frames(&mut h, 4);
        assert_eq!(
            search_fields(&h),
            1,
            "{size:?}: one search field after the click"
        );
        assert_eq!(
            search_buttons(&h).len(),
            1,
            "{size:?}: still one search button"
        );
    }
}

#[test]
fn emoji_panel_docks_as_third_column_only_when_wide() {
    for (size, docked) in [
        (Vec2::new(1440.0, 900.0), true),
        (Vec2::new(390.0, 844.0), false),
    ] {
        let mut h = plain_harness(size);
        open_chat_plain(&mut h);
        h.state_mut()
            .controller
            .main_widget_mut()
            .tabbed_panel_mut()
            .show_immediate();
        step_frames(&mut h, 4);
        assert_eq!(
            h.state_mut()
                .controller
                .main_widget_mut()
                .tabbed_panel_docked(),
            docked,
            "dock state at {size:?}"
        );
    }
}

#[test]
fn message_menu_forward_opens_share_box() {
    let mut h = plain_harness(Vec2::new(1440.0, 900.0));
    open_chat_plain(&mut h);
    h.state_mut()
        .controller
        .main_widget_mut()
        .request_forward_box(session_peers::USER_MIRA, vec![MsgId(1)]);
    step_frames(&mut h, 3);
    assert!(
        matches!(
            h.state_mut().controller.box_host_mut().top_kind(),
            Some(BoxKind::Share(s)) if s.message_ids == vec![1]
        ),
        "Forward must open the Forward to... ShareBox"
    );
}

#[test]
fn main_menu_has_tdesktop_rows_and_no_extra_entries() {
    let mut h = plain_harness(Vec2::new(1440.0, 900.0));
    h.state_mut().controller.open_main_menu();
    step_frames(&mut h, 3);
    for label in [
        "My Profile",
        "New Group",
        "New Channel",
        "Contacts",
        "Calls",
        "Saved Messages",
        "Settings",
        "Night Mode",
        "Set Emoji Status",
    ] {
        assert!(
            h.query_all_by_label(label).next().is_some(),
            "main menu must show {label}"
        );
    }
    for label in [
        "Downloads",
        "Log Out",
        "Lock Rustgram",
        "Lock Rustgram (Set...)",
    ] {
        assert!(
            h.query_all_by_label(label).next().is_none(),
            "7.4.6 main menu has no {label} row"
        );
    }
}
