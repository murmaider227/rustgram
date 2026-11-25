//! UI parity stage 2, Package D: before/after captures of the chat list, its context menu,
//! global search, user profile, group info, the "Forward to..." ShareBox and the New Group box
//! at 1440x900 and 390x844 (Day, plus a Night profile), compared against Telegram Desktop 7.4.6.
//!
//! Run with `RUSTGRAM_SNAPSHOT_DIR=<dir> RUSTGRAM_SNAPSHOT_TAG=before|after
//! cargo test -p tg-app --test all e2e_uipd -- --ignored --nocapture`.

use std::path::PathBuf;
use std::sync::Mutex;

use egui::accesskit::Role;
use egui::{Pos2, Vec2};
use egui_kittest::kittest::Queryable;
use tg_app::boxes::BoxKind;
use tg_app::boxes::create_peer::CreateGroupBoxState;
use tg_app::boxes::share::ShareBoxState;
use tg_app::info::InfoColors;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::theme::ThemeMode;
use tg_app::window::section::SectionShow;
use tg_core::settings::{CoreSettings, PowerSavingFlags};

use crate::common::{
    AppTestState, build_wgpu_harness, right_click_at, settle_layout, step_frames, type_text,
};

const DESKTOP_SIZE: Vec2 = Vec2::new(1440.0, 900.0);
const MOBILE_SIZE: Vec2 = Vec2::new(390.0, 844.0);

static WGPU_UIPD_LOCK: Mutex<()> = Mutex::new(());

fn snap_tag() -> String {
    std::env::var("RUSTGRAM_SNAPSHOT_TAG").unwrap_or_else(|_| "after".to_owned())
}

fn save_png(harness: &mut egui_kittest::Harness<'static, AppTestState>, name: &str) {
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("uipd"));
    std::fs::create_dir_all(&dir).expect("create snap dir");
    let img = harness.render().expect("wgpu render");
    img.save(dir.join(format!("{name}_{}.png", snap_tag())))
        .expect("save png");
}

fn build_snap_harness(size: Vec2, night: bool) -> egui_kittest::Harness<'static, AppTestState> {
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
    if night {
        state.controller.toggle_night_mode();
    }
    let mut harness = build_wgpu_harness(state, size);
    settle_layout(&mut harness);
    step_frames(&mut harness, 20);
    harness
}

/// At phone width the mock opens a chat; tap the history Back arrow to show the chat list.
fn show_list_if_narrow(harness: &mut egui_kittest::Harness<'static, AppTestState>, size: Vec2) {
    if size.x < 600.0 {
        crate::common::click_at(harness, Pos2::new(33.0, 35.0));
        step_frames(harness, 20);
    }
}

fn size_tag(size: Vec2) -> String {
    format!("{}x{}", size.x as i32, size.y as i32)
}

fn open_row_menu(harness: &mut egui_kittest::Harness<'static, AppTestState>, title: &str) {
    let pos = harness
        .query_all_by_label_contains(title)
        .map(|n| n.rect())
        .filter(|r| r.width() > 120.0 && r.height() >= 40.0)
        .map(|r| r.center())
        .next();
    if let Some(pos) = pos {
        right_click_at(harness, pos);
        step_frames(harness, 15);
    }
}

fn open_global_search(harness: &mut egui_kittest::Harness<'static, AppTestState>, query: &str) {
    let input = harness
        .query_all_by_role(Role::TextInput)
        .map(|n| n.rect())
        .find(|r| r.center().y < 120.0)
        .map(|r| r.center());
    if let Some(pos) = input {
        crate::common::click_at(harness, pos);
        harness.step();
        type_text(harness, query);
        step_frames(harness, 40);
    }
}

#[test]
#[ignore = "captures UI parity Package D screenshots under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_uipd_screenshots() {
    let _guard = WGPU_UIPD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let only = std::env::var("RUSTGRAM_UIPD_SIZES").unwrap_or_default();
    for size in [DESKTOP_SIZE, MOBILE_SIZE] {
        if !only.is_empty() && !only.contains(&(size.x as i32).to_string()) {
            continue;
        }
        let tag = size_tag(size);
        let colors = InfoColors::for_mode(ThemeMode::Day);

        {
            let mut h = build_snap_harness(size, false);
            show_list_if_narrow(&mut h, size);
            save_png(&mut h, &format!("01_chat_list_{tag}"));
        }
        {
            let mut h = build_snap_harness(size, false);
            show_list_if_narrow(&mut h, size);
            open_row_menu(&mut h, "Mira Vance");
            save_png(&mut h, &format!("10_chat_list_menu_{tag}"));
        }
        {
            let mut h = build_snap_harness(size, false);
            show_list_if_narrow(&mut h, size);
            open_global_search(&mut h, "rust");
            save_png(&mut h, &format!("18_search_global_{tag}"));
        }
        {
            let mut h = build_snap_harness(size, false);
            if let Some(session) = h.state_mut().controller.session_mut() {
                session.show_peer_info(session_peers::USER_MIRA, SectionShow::clear_stack());
            }
            settle_layout(&mut h);
            step_frames(&mut h, 10);
            save_png(&mut h, &format!("11_user_profile_{tag}"));
        }
        {
            let mut h = build_snap_harness(size, false);
            if let Some(session) = h.state_mut().controller.session_mut() {
                session.show_peer_info(session_peers::GROUP_UI_LAB, SectionShow::clear_stack());
            }
            settle_layout(&mut h);
            step_frames(&mut h, 10);
            save_png(&mut h, &format!("12_group_info_{tag}"));
        }
        {
            let mut h = build_snap_harness(size, false);
            let ctx = h.ctx.clone();
            h.state_mut().controller.box_host_mut().push(
                &ctx,
                BoxKind::Share(ShareBoxState::new(vec![1], None)),
                &colors,
            );
            settle_layout(&mut h);
            step_frames(&mut h, 10);
            save_png(&mut h, &format!("20_forward_box_{tag}"));
        }
        {
            let mut h = build_snap_harness(size, false);
            let ctx = h.ctx.clone();
            h.state_mut().controller.box_host_mut().push(
                &ctx,
                BoxKind::CreateGroup(CreateGroupBoxState::new()),
                &colors,
            );
            settle_layout(&mut h);
            step_frames(&mut h, 10);
            save_png(&mut h, &format!("21_new_group_box_{tag}"));
        }
        {
            let mut h = build_snap_harness(size, true);
            if let Some(session) = h.state_mut().controller.session_mut() {
                session.show_peer_info(session_peers::USER_MIRA, SectionShow::clear_stack());
            }
            settle_layout(&mut h);
            step_frames(&mut h, 10);
            save_png(&mut h, &format!("27_night_profile_{tag}"));
        }
    }
}
