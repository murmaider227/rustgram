//! TGB-2955: choosing the dark theme in Settings > Chat settings switches the app to dark.
//!
//! A click on a theme card of the other mode used to reach the window twice in one frame,
//! as "apply this theme" and then as "toggle night mode", so the chosen dark theme was
//! applied and flipped straight back to Classic. These tests click the real cards in the
//! mock (demo) window and in a live workspace, and cover the menu switch, the auto-night
//! confirmation and a restart.

use egui::{Pos2, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tg_app::dialogs::filters_menu::SIDEBAR_MENU_BUTTON_HEIGHT;
use tg_app::live::host::LiveActiveWorkspace;
use tg_app::settings::SettingsSection;
use tg_app::stand_in::mock_session::MockSession;
use tg_app::stand_in::settings::ShellSettings;
use tg_app::stand_in::theme::ThemeMode;
use tg_app::window::controller::WindowController;
use tg_app::window::section::SettingsPage;
use tg_theme::EmbeddedType;
use tg_ui::theme::Colors;

use super::live_tests_window_wiring::{LiveTestEnv, build_live_harness};
use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, build_harness, click_at, settle_layout, step_frames,
    wait_for_label, wait_until,
};

const WORKSPACE_SIZE: Vec2 = Vec2::new(1280.0, 820.0);

/// The localized names under the four theme cards (`SetupDefaultThemes`).
const CARD_CLASSIC: &str = "Classic";
const CARD_DAY: &str = "Day";
const CARD_TINTED: &str = "Tinted";
const CARD_NIGHT: &str = "Night";

fn open_chat_settings(ctrl: &mut WindowController) {
    let backend = ctrl.session_backend().create_settings_backend();
    let embedded = ctrl.theme_manager().embedded_type();
    let accent = ctrl.theme_manager().accent();
    ctrl.main_widget_mut()
        .open_settings_section(SettingsSection::Chat, backend, embedded, accent);
    ctrl.session_mut()
        .expect("session")
        .show_settings(SettingsPage::Main);
}

fn click_label<S>(harness: &mut Harness<'_, S>, label: &str) {
    wait_for_label(harness, label);
    let center = harness.get_by_label(label).rect().center();
    click_at(harness, center);
}

/// The window switches to `kind`, and from the next frame on its palette and the egui
/// visuals both show it. Later frames are checked too: the old bug applied the chosen theme
/// and undid it within the same frame.
fn assert_theme_shown<S>(
    harness: &mut Harness<'_, S>,
    controller: impl Fn(&S) -> &WindowController,
    kind: EmbeddedType,
) {
    wait_until(
        harness,
        8,
        "the window did not switch to the chosen theme",
        |h| controller(h.state()).theme_manager().embedded_type() == kind,
    );
    for frame in 0..4 {
        harness.step();
        let ctrl = controller(harness.state());
        assert_eq!(
            ctrl.theme_manager().embedded_type(),
            kind,
            "frame {frame} after the choice"
        );
        assert_eq!(ctrl.theme_manager().is_dark(), kind.is_dark());
        assert_eq!(ctrl.theme_mode().is_night(), kind.is_dark());
        let window_bg = ctrl.theme_manager().color("windowBg");
        assert_eq!(ctrl.palette().window_bg, window_bg);
        let visuals = harness.ctx.global_style().visuals.clone();
        assert_eq!(
            visuals.dark_mode,
            kind.is_dark(),
            "egui visuals, frame {frame}"
        );
        assert_eq!(
            visuals.panel_fill, window_bg,
            "egui panel fill, frame {frame}"
        );
    }
}

fn demo(state: &AppTestState) -> &WindowController {
    &state.controller
}

#[test]
fn demo_chat_settings_theme_cards_switch_between_light_and_dark_at_once() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);
    assert_eq!(harness.state().controller.theme_mode(), ThemeMode::Day);
    open_chat_settings(&mut harness.state_mut().controller);
    settle_layout(&mut harness);

    // Light to dark: the reported case.
    click_label(&mut harness, CARD_NIGHT);
    assert_theme_shown(&mut harness, demo, EmbeddedType::NightGreen);
    // Dark to dark, dark to light, light to light.
    click_label(&mut harness, CARD_TINTED);
    assert_theme_shown(&mut harness, demo, EmbeddedType::Night);
    click_label(&mut harness, CARD_DAY);
    assert_theme_shown(&mut harness, demo, EmbeddedType::DayBlue);
    click_label(&mut harness, CARD_CLASSIC);
    assert_theme_shown(&mut harness, demo, EmbeddedType::Default);
    click_label(&mut harness, CARD_TINTED);
    assert_theme_shown(&mut harness, demo, EmbeddedType::Night);
}

#[test]
fn demo_dark_theme_choice_survives_a_restart() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);
    open_chat_settings(&mut harness.state_mut().controller);
    settle_layout(&mut harness);
    click_label(&mut harness, CARD_DAY);
    assert_theme_shown(&mut harness, demo, EmbeddedType::DayBlue);
    click_label(&mut harness, CARD_NIGHT);
    assert_theme_shown(&mut harness, demo, EmbeddedType::NightGreen);

    // Exit writes the settings; the next start reads them.
    harness.state_mut().controller.flush_settings();
    let path = harness.state()._temp_dir.path().join("shell_settings.json");
    let mut restarted = WindowController::with_session(
        MockSession::new(true),
        ShellSettings::load_from(&path),
        path,
    );
    assert_eq!(
        restarted.theme_manager().embedded_type(),
        EmbeddedType::NightGreen
    );
    assert_eq!(restarted.theme_mode(), ThemeMode::Night);
    let ctx = egui::Context::default();
    restarted.theme_manager().apply_to_egui(&ctx);
    let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
    output.textures_delta.clear();
    assert!(ctx.global_style().visuals.dark_mode);

    // The day theme chosen before the restart is the one the night switch returns to.
    restarted.request_night_mode_toggle();
    assert_eq!(
        restarted.theme_manager().embedded_type(),
        EmbeddedType::DayBlue
    );
}

#[test]
fn demo_main_menu_night_switch_toggles_and_keeps_each_modes_theme() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);
    harness
        .state_mut()
        .controller
        .choose_embedded_theme(EmbeddedType::DayBlue, None);
    step_frames(&mut harness, 2);

    click_at(
        &mut harness,
        Pos2::new(36.0, SIDEBAR_MENU_BUTTON_HEIGHT * 0.5),
    );
    let night_row = tg_lang::tr::lng_menu_night_mode();
    click_label(&mut harness, &night_row);
    assert_theme_shown(&mut harness, demo, EmbeddedType::Night);
    click_label(&mut harness, &night_row);
    assert_theme_shown(&mut harness, demo, EmbeddedType::DayBlue);
}

#[test]
fn demo_auto_night_asks_before_a_switch_against_the_system_and_follows_only_when_enabled() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);
    open_chat_settings(&mut harness.state_mut().controller);
    // The system is light and auto-night mode is on (its default).
    harness
        .state_mut()
        .controller
        .set_system_dark_mode(Some(false));
    settle_layout(&mut harness);
    assert!(harness.state().controller.follows_system_dark_mode());

    click_label(&mut harness, CARD_NIGHT);
    let disable = tg_lang::tr::lng_settings_auto_night_disable();
    wait_for_label(&mut harness, &disable);
    assert_eq!(
        harness.state().controller.theme_mode(),
        ThemeMode::Day,
        "nothing changes before the answer"
    );
    click_label(&mut harness, &disable);
    assert_theme_shown(&mut harness, demo, EmbeddedType::NightGreen);
    assert!(!harness.state().controller.follows_system_dark_mode());

    // Auto-night is off now: a system change does nothing.
    harness
        .state_mut()
        .controller
        .set_system_dark_mode(Some(true));
    step_frames(&mut harness, 2);
    harness
        .state_mut()
        .controller
        .set_system_dark_mode(Some(false));
    assert_theme_shown(&mut harness, demo, EmbeddedType::NightGreen);

    // Turned back on (the "Auto-night mode" row), the app follows the system at once.
    harness
        .state_mut()
        .controller
        .main_widget_mut()
        .settings_mut()
        .expect("settings")
        .core_settings
        .system_dark_mode_enabled = true;
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "auto-night mode must switch to the light system mode",
        |h| h.state().controller.theme_mode() == ThemeMode::Day,
    );
    harness
        .state_mut()
        .controller
        .set_system_dark_mode(Some(true));
    assert_theme_shown(&mut harness, demo, EmbeddedType::NightGreen);
}

#[test]
fn live_chat_settings_dark_theme_card_switches_the_live_window_to_dark() {
    let env = LiveTestEnv::boot(2_955_001, 2_955_002, 2_955_003);
    let runtime = env.runtime;
    let mut ws = env.workspace;
    open_chat_settings(ws.controller_mut());
    let mut harness = build_live_harness(ws);
    step_frames(&mut harness, 6);
    assert_eq!(harness.state().controller().theme_mode(), ThemeMode::Day);

    click_label(&mut harness, CARD_NIGHT);
    assert_theme_shown(
        &mut harness,
        LiveActiveWorkspace::controller,
        EmbeddedType::NightGreen,
    );
    click_label(&mut harness, CARD_CLASSIC);
    assert_theme_shown(
        &mut harness,
        LiveActiveWorkspace::controller,
        EmbeddedType::Default,
    );

    // The live window persists the choice like the demo one.
    click_label(&mut harness, CARD_TINTED);
    assert_theme_shown(
        &mut harness,
        LiveActiveWorkspace::controller,
        EmbeddedType::Night,
    );
    harness.state_mut().controller_mut().flush_settings();
    assert_eq!(
        harness
            .state()
            .controller()
            .settings()
            .resolved_embedded_type(),
        EmbeddedType::Night
    );

    drop(harness);
    LiveTestEnv::shutdown_runtime(runtime);
}

/// Writes the before / after evidence images: Chat settings in the mock window right after a
/// click on the "Night" theme card, at desktop and phone size. Makes no assertion about the
/// theme, so it also runs on the code before the fix. Set `TGB2955_SHOT_DIR` (and
/// `TGB2955_STAGE`, `before` or `after`).
#[test]
#[ignore = "writes evidence images; run by hand with TGB2955_SHOT_DIR set"]
fn tgb2955_evidence_shots() {
    let Ok(dir) = std::env::var("TGB2955_SHOT_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let stage = std::env::var("TGB2955_STAGE").unwrap_or_else(|_| "after".to_owned());
    std::fs::create_dir_all(&dir).expect("create shot dir");
    for (name, size) in [
        ("desktop_1440x900", Vec2::new(1440.0, 900.0)),
        ("phone_390x844", Vec2::new(390.0, 844.0)),
    ] {
        let mut harness = crate::common::build_wgpu_harness(AppTestState::main_workspace(), size);
        settle_layout(&mut harness);
        open_chat_settings(&mut harness.state_mut().controller);
        settle_layout(&mut harness);
        click_label(&mut harness, CARD_NIGHT);
        step_frames(&mut harness, 12);
        let image = harness.render().expect("wgpu render");
        image
            .save(dir.join(format!("chat_settings_pick_night_{name}_{stage}.png")))
            .expect("save png");
    }
}

/// A theme change made with no input event behind it (the system went dark) still gets the
/// frame that repaints the window in the new theme.
#[test]
fn demo_theme_change_requests_the_frame_that_repaints_it() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);
    step_frames(&mut harness, 3);
    harness
        .state_mut()
        .controller
        .set_system_dark_mode(Some(true));
    harness.step();
    assert_eq!(harness.state().controller.theme_mode(), ThemeMode::Night);
    assert!(
        !crate::common::is_repaint_settled(&harness),
        "the frame that changed the theme must request the next one"
    );
    assert_theme_shown(&mut harness, demo, EmbeddedType::Night);
}
