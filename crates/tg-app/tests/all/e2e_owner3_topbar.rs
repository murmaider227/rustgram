//! Owner bugs of 2026-11-27, TOPBAR package, through the whole window:
//! - TGB-3183: the chat "three dots" menu lists `Filler::fillHistoryActions` rows;
//! - TGB-3184: a click on the chat title opens the profile as a layer over the window;
//! - TGB-3186: the third column's x closes it and the choice sticks.

use std::path::PathBuf;

use egui::{Pos2, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tg_app::boxes::BoxKind;
use tg_app::history::HISTORY_TOP_BAR_HEIGHT;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_app::window::section::{SectionMemento, SectionShow};
use tg_lang::tr;

use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, build_harness, build_wgpu_harness, click_at, settle_layout,
    wait_for_menu_item, wait_until,
};

const SIZE: Vec2 = Vec2::new(1440.0, 900.0);

fn open_chat(h: &mut Harness<'static, AppTestState>, peer: PeerId) {
    h.state_mut()
        .controller
        .session_mut()
        .expect("session")
        .show_peer_history(peer, SectionShow::clear_stack(), MsgId::SHOW_AT_END);
    settle_layout(h);
}

/// Left edge of the chat column (after the folders strip and the chats list).
fn chat_left(h: &Harness<'static, AppTestState>) -> f32 {
    let geo = h.state().controller.session().expect("session").layout();
    (geo.filters_width + geo.dialogs_width) as f32
}

fn open_chat_menu(h: &mut Harness<'static, AppTestState>) {
    let menu = h
        .query_all_by_label(&tr::lng_chat_menu())
        .map(|n| n.rect().center())
        .find(|c| c.y < HISTORY_TOP_BAR_HEIGHT)
        .expect("chat menu button in the top bar");
    click_at(h, Pos2::new(menu.x as f32, menu.y as f32));
}

/// TGB-3183: before the fix a group's menu had one row; now it has the `Filler` rows.
#[test]
fn supergroup_chat_menu_lists_filler_rows() {
    let mut h = build_harness(AppTestState::main_workspace(), SIZE);
    settle_layout(&mut h);
    open_chat(&mut h, session_peers::GROUP_UI_LAB);
    open_chat_menu(&mut h);
    wait_for_menu_item(&mut h, &tr::lng_manage_group_title());
    for label in [
        tr::lng_context_mute(),
        tr::lng_manage_group_title(),
        tr::lng_boost_group_button(),
        tr::lng_polls_create(),
        tr::lng_profile_export_chat(),
        tr::lng_profile_leave_group(),
    ] {
        assert!(h.query_by_label(&label).is_some(), "menu row {label:?}");
    }
    // "Manage group" opens the existing Edit group box.
    let manage = h
        .get_by_label(&tr::lng_manage_group_title())
        .rect()
        .center();
    click_at(&mut h, Pos2::new(manage.x as f32, manage.y as f32));
    wait_until(
        &mut h,
        DEFAULT_MAX_FRAMES,
        "Manage group opens the edit box",
        |h| {
            matches!(
                h.state().controller.box_host().top_kind(),
                Some(BoxKind::EditPeerInfo(_))
            )
        },
    );
}

fn third_column_profile(h: &Harness<'static, AppTestState>) -> Option<PeerId> {
    match h.state().controller.session()?.third_section() {
        Some(SectionMemento::PeerInfo { peer_id, .. }) => Some(*peer_id),
        _ => None,
    }
}

/// TGB-3186: the x of the third column closes it, and the next chat opens with two columns.
#[test]
fn third_column_profile_closes_on_its_x() {
    let mut h = build_harness(AppTestState::main_workspace(), SIZE);
    settle_layout(&mut h);
    open_chat(&mut h, session_peers::GROUP_UI_LAB);
    h.state_mut()
        .controller
        .session_mut()
        .expect("session")
        .toggle_peer_info(session_peers::GROUP_UI_LAB);
    settle_layout(&mut h);
    assert_eq!(
        third_column_profile(&h),
        Some(session_peers::GROUP_UI_LAB),
        "the info toggle opens the third column"
    );
    let geo = h.state().controller.session().expect("session").layout();
    let third_left = SIZE.x - geo.third_width as f32;
    let close = h
        .query_all_by_label(&tr::lng_close())
        .map(|n| n.rect().center())
        .find(|c| c.x > third_left && c.y < 60.0)
        .expect("the third column's x");
    click_at(&mut h, Pos2::new(close.x as f32, close.y as f32));
    settle_layout(&mut h);
    assert_eq!(third_column_profile(&h), None, "x closes the column");
    assert!(
        !h.state()
            .controller
            .session()
            .expect("session")
            .columns()
            .third_section_info_enabled(),
        "closeThirdSection stores thirdSectionInfoEnabled = false"
    );
    open_chat(&mut h, session_peers::CHANNEL_DEV_LOG);
    assert_eq!(
        third_column_profile(&h),
        None,
        "the next chat opens without it"
    );
}

/// Before | after shots for the TOPBAR package (`RUSTGRAM_SNAP_STAGE` = before / after).
#[test]
#[ignore = "captures PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_owner3_topbar_snapshots() {
    let stage = std::env::var("RUSTGRAM_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned());
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir()
                .join("rustgram-snap")
                .join("owner3-topbar")
        });
    std::fs::create_dir_all(&dir).expect("create snap dir");
    let save = |h: &mut Harness<'static, AppTestState>, name: &str| {
        let img = h.render().expect("wgpu render");
        img.save(dir.join(format!("{name}_1440x900_{stage}.png")))
            .expect("save png");
    };

    // TGB-3183: the group chat menu.
    let mut h = build_wgpu_harness(AppTestState::main_workspace(), SIZE);
    settle_layout(&mut h);
    open_chat(&mut h, session_peers::GROUP_UI_LAB);
    open_chat_menu(&mut h);
    settle_layout(&mut h);
    save(&mut h, "tgb3183_group_menu");

    // TGB-3184: a click on the chat title.
    let mut h = build_wgpu_harness(AppTestState::main_workspace(), SIZE);
    settle_layout(&mut h);
    open_chat(&mut h, session_peers::GROUP_UI_LAB);
    let title = Pos2::new(chat_left(&h) + 120.0, HISTORY_TOP_BAR_HEIGHT * 0.5);
    click_at(&mut h, title);
    settle_layout(&mut h);
    save(&mut h, "tgb3184_title_click");

    // TGB-3186: the third column after a click on its x.
    let mut h = build_wgpu_harness(AppTestState::main_workspace(), SIZE);
    settle_layout(&mut h);
    open_chat(&mut h, session_peers::GROUP_UI_LAB);
    h.state_mut()
        .controller
        .session_mut()
        .expect("session")
        .toggle_peer_info(session_peers::GROUP_UI_LAB);
    settle_layout(&mut h);
    let geo = h.state().controller.session().expect("session").layout();
    let third_left = SIZE.x - geo.third_width as f32;
    let close = h
        .query_all_by_label(&tr::lng_close())
        .map(|n| n.rect().center())
        .find(|c| c.x > third_left && c.y < 60.0);
    if let Some(close) = close {
        click_at(&mut h, Pos2::new(close.x, close.y));
    }
    settle_layout(&mut h);
    save(&mut h, "tgb3186_after_x_click");
}

fn open_third_column(h: &mut Harness<'static, AppTestState>) -> Pos2 {
    open_chat(h, session_peers::GROUP_UI_LAB);
    h.state_mut()
        .controller
        .session_mut()
        .expect("session")
        .toggle_peer_info(session_peers::GROUP_UI_LAB);
    settle_layout(h);
    let geo = h.state().controller.session().expect("session").layout();
    let third_left = SIZE.x - geo.third_width as f32;
    let close = h
        .query_all_by_label(&tr::lng_close())
        .map(|n| n.rect().center())
        .find(|c| c.x > third_left && c.y < 60.0)
        .expect("the third column's x");
    Pos2::new(close.x as f32, close.y as f32)
}

/// TGB-3186: a click whose move, press and release arrive in one frame (as posted window
/// messages do) still closes the column.
#[test]
fn third_column_x_closes_with_a_single_frame_click() {
    let mut h = build_harness(AppTestState::main_workspace(), SIZE);
    settle_layout(&mut h);
    let close = open_third_column(&mut h);
    h.input_mut().events.push(egui::Event::PointerMoved(close));
    h.input_mut().events.push(egui::Event::PointerButton {
        pos: close,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.input_mut().events.push(egui::Event::PointerButton {
        pos: close,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    settle_layout(&mut h);
    assert_eq!(third_column_profile(&h), None, "same-frame click");
}

/// TGB-3186: the x still closes the column after the profile under it was scrolled.
#[test]
fn third_column_x_closes_after_scrolling_the_profile() {
    let mut h = build_harness(AppTestState::main_workspace(), SIZE);
    settle_layout(&mut h);
    let close = open_third_column(&mut h);
    let mid = Pos2::new(close.x - 100.0, 500.0);
    h.input_mut().events.push(egui::Event::PointerMoved(mid));
    h.input_mut().events.push(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -400.0),
        modifiers: egui::Modifiers::NONE,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
    });
    h.step();
    settle_layout(&mut h);
    click_at(&mut h, close);
    settle_layout(&mut h);
    assert_eq!(third_column_profile(&h), None, "after scroll");
}
