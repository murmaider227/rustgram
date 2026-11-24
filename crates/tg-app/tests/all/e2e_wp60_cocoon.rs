//! End-to-end `egui_kittest` tests and headless `wgpu` screenshot capture for WP-60 About Cocoon UI.

use std::path::PathBuf;
use std::sync::Mutex;

use egui::{Key, Modifiers, Vec2};
use egui_kittest::kittest::Queryable;
use tg_app::boxes::compose_ai::ComposeAiBoxState;
use tg_app::stand_in::Backend;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::MsgId;
use tg_app::window::section::SectionShow;
use tg_core::settings::{CoreSettings, PowerSavingFlags};
use tg_lang::tr;
use tg_text::TextWithEntities;

use crate::common::{
    AppTestState, build_harness, build_wgpu_harness, click_at, press_key, settle_layout,
    step_frames, wait_for_label, wait_for_label_contains,
};

const DESKTOP_SIZE: Vec2 = Vec2::new(1440.0, 900.0);
const MOBILE_SIZE: Vec2 = Vec2::new(390.0, 844.0);

static WGPU_WP60_LOCK: Mutex<()> = Mutex::new(());

fn save_wgpu_png(harness: &mut egui_kittest::Harness<'static, AppTestState>, name: &str) {
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("wp60"));
    std::fs::create_dir_all(&dir).expect("create snap dir");
    let img = harness.render().expect("wgpu render");
    let path = dir.join(format!("{name}.png"));
    img.save(&path).expect("save png");
}

fn make_static_state() -> AppTestState {
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
    state
}

fn build_snap_harness(size: Vec2) -> egui_kittest::Harness<'static, AppTestState> {
    let mut harness = build_wgpu_harness(make_static_state(), size);
    settle_layout(&mut harness);
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

fn open_russian_dm_with_translate_bar(harness: &mut egui_kittest::Harness<'static, AppTestState>) {
    let _ = Backend::send_message(
        harness.state_mut().controller.session_backend_mut(),
        session_peers::USER_RILEY,
        "Привет! Проверяем автоматический перевод сообщений в этом чате и меню параметров перевода. Добро пожаловать на тестирование интерфейса.",
    );
    if let Some(session) = harness.state_mut().controller.session_mut() {
        session.show_peer_history(
            session_peers::USER_RILEY,
            SectionShow::clear_stack(),
            MsgId::SHOW_AT_END,
        );
    }
    settle_layout(harness);
    step_frames(harness, 10);
}

#[test]
fn compose_ai_box_opens_and_closes_about_cocoon_box() {
    let mut harness = build_harness(make_static_state(), DESKTOP_SIZE);
    settle_layout(&mut harness);

    harness
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .open_compose_ai_box(ComposeAiBoxState::new(TextWithEntities::simple(
            "hey we fixed the build thing and tests pass now",
        )));
    settle_layout(&mut harness);

    let info_label = tr::lng_sr_ai_compose_info();
    wait_for_label(&mut harness, &info_label);
    harness.get_by_label(&info_label).click();
    settle_layout(&mut harness);

    assert!(
        harness
            .state()
            .controller
            .main_widget()
            .history()
            .is_compose_ai_about_cocoon_open(),
        "clicking About AI Editor info button must open AboutCocoonBox"
    );

    // Verify feature headings and primary CTA render with localized strings.
    let done_label = tr::lng_translate_cocoon_done();
    wait_for_label(&mut harness, &done_label);
    wait_for_label(&mut harness, &tr::lng_translate_cocoon_private_title());
    wait_for_label(&mut harness, &tr::lng_translate_cocoon_efficient_title());
    wait_for_label(&mut harness, &tr::lng_translate_cocoon_everyone_title());

    // Clicking "Understood" dismisses AboutCocoonBox and returns to ComposeAiBox.
    harness.get_by_label(&done_label).click();
    settle_layout(&mut harness);
    assert!(
        !harness
            .state()
            .controller
            .main_widget()
            .history()
            .is_compose_ai_about_cocoon_open(),
        "clicking Understood must close AboutCocoonBox"
    );
    assert!(
        harness.query_by_label(&info_label).is_some(),
        "parent ComposeAiBox must remain open after closing AboutCocoonBox"
    );
}

#[test]
fn translate_bar_menu_opens_and_closes_about_cocoon_box() {
    let mut harness = build_harness(make_static_state(), DESKTOP_SIZE);
    settle_layout(&mut harness);
    open_russian_dm_with_translate_bar(&mut harness);

    wait_for_label(&mut harness, "Translation options");
    harness.get_by_label("Translation options").click();
    settle_layout(&mut harness);

    let link_phrase = tr::lng_translate_cocoon_link();
    wait_for_label_contains(&mut harness, &link_phrase);
    harness.get_by_label_contains(&link_phrase).click();
    settle_layout(&mut harness);

    assert!(
        harness
            .state()
            .controller
            .main_widget()
            .history()
            .is_about_cocoon_open(),
        "clicking Cocoon footer item in translate bar menu must open AboutCocoonBox"
    );
    wait_for_label(&mut harness, &tr::lng_translate_cocoon_done());

    // Pressing Escape dismisses AboutCocoonBox cleanly.
    press_key(&mut harness, Modifiers::NONE, Key::Escape);
    settle_layout(&mut harness);
    assert!(
        !harness
            .state()
            .controller
            .main_widget()
            .history()
            .is_about_cocoon_open(),
        "pressing Escape must close AboutCocoonBox"
    );
}

#[test]
#[ignore = "generates WP-60 before screenshots under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_wp60_before_screenshots() {
    let _guard = WGPU_WP60_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    for (tag, size) in [("1440x900", DESKTOP_SIZE), ("390x844", MOBILE_SIZE)] {
        {
            let mut h = build_snap_harness(size);
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .open_compose_ai_box(ComposeAiBoxState::new(TextWithEntities::simple(
                    "hey we fixed the build thing and tests pass now",
                )));
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("compose_ai_box_{tag}_before"));
        }

        {
            let mut h = build_snap_harness(size);
            open_russian_dm_with_translate_bar(&mut h);
            let menu_pos = egui::Pos2::new(size.x - 54.0, 56.0 + 20.0);
            click_at(&mut h, menu_pos);
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("translate_bar_menu_{tag}_before"));
        }
    }
}

#[test]
#[ignore = "generates WP-60 after screenshots under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_wp60_after_screenshots() {
    let _guard = WGPU_WP60_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    for (tag, size) in [("1440x900", DESKTOP_SIZE), ("390x844", MOBILE_SIZE)] {
        // 1. Compose with AI modal (after adding About Cocoon info button + mobile fit)
        {
            let mut h = build_snap_harness(size);
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .open_compose_ai_box(ComposeAiBoxState::new(TextWithEntities::simple(
                    "hey we fixed the build thing and tests pass now",
                )));
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("compose_ai_box_{tag}_after"));
        }

        // 2. Translate Bar options menu (after adding Cocoon attribution row + clamping)
        {
            let mut h = build_snap_harness(size);
            open_russian_dm_with_translate_bar(&mut h);
            h.get_by_label("Translation options").click();
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("translate_bar_menu_{tag}_after"));
        }

        // 3. About Cocoon modal box (opened from Translate Bar menu)
        {
            let mut h = build_snap_harness(size);
            open_russian_dm_with_translate_bar(&mut h);
            h.get_by_label("Translation options").click();
            settle_layout(&mut h);
            let link_phrase = tr::lng_translate_cocoon_link();
            h.get_by_label_contains(&link_phrase).click();
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("about_cocoon_box_{tag}_after"));
        }
    }
}
