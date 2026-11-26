//! Wave 4 SETTINGS: the Theme Studio and the app's theme stay apart, and Chat settings follows
//! the app's theme and the system dark-mode value.
//!
//! - TGB-2961: opening the Theme Studio in a dark theme painted the stock egui colours of the
//!   studio's own (Classic) theme over the whole window, and nothing put the app's back.
//! - TGB-2962: the wallpaper thumbnail in Chat settings stayed on the day wallpaper after a
//!   switch to a dark theme while no chat history was painted. The studio copied its own
//!   dark-mode flag into the shared wallpaper every frame; only a painted chat set it back.
//! - TGB-2963: the "Auto-night mode" row was shown although the system gave no dark-mode value.
//!
//! Every test renders what the app renders each frame: the window, then the Theme Studio host.

use egui::Vec2;
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use tg_app::settings::SettingsSection;
use tg_app::settings::chat::{ChatSettingsState, render_chat_settings_section};
use tg_app::settings::mock::MockSettingsBackend;
use tg_app::settings::theme_bridge::LiveThemeColors;
use tg_app::stand_in::theme::ThemeMode;
use tg_app::themes::{AppThemesHost, ThemesTab, active_chat_background};
use tg_app::window::controller::WindowController;
use tg_app::window::section::SettingsPage;
use tg_core::{CoreSettings, GlobalConfig};
use tg_lang::tr;
use tg_theme::EmbeddedType;
use tg_ui::theme::{Colors, DayPalette};

use crate::common::{
    AppTestState, TEST_STEP_DT, apply_deterministic_style, build_widget_harness, click_at,
    settle_layout, step_frames, wait_for_label, wait_for_label_contains, wait_until,
};

const WIDE: Vec2 = Vec2::new(1280.0, 820.0);
/// One column: Settings covers the chat, so no chat history is painted.
const PHONE: Vec2 = Vec2::new(390.0, 844.0);

/// The demo window followed by the Theme Studio host, as `TgforkApp` renders a frame.
fn build_app_harness(size: Vec2) -> Harness<'static, AppTestState> {
    HarnessBuilder::default()
        .with_size(size)
        .with_step_dt(TEST_STEP_DT)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
        .build_ui_state(
            |ui, st: &mut AppTestState| {
                st.show(ui);
                let ctx = ui.ctx().clone();
                let pal = st.controller.palette();
                AppThemesHost::update_and_render(&ctx, pal.layer_backdrop, pal.divider);
            },
            AppTestState::main_workspace(),
        )
}

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

fn shared_wallpaper_is_dark(harness: &Harness<'_, AppTestState>) -> bool {
    active_chat_background(&harness.ctx)
        .lock()
        .expect("shared chat background")
        .is_dark_mode()
}

/// The window's egui colours are the app's theme, whatever the studio shows.
fn assert_window_colours_are_the_apps(harness: &Harness<'_, AppTestState>, when: &str) {
    let ctrl = &harness.state().controller;
    let visuals = harness.ctx.global_style().visuals.clone();
    assert_eq!(
        visuals.dark_mode,
        ctrl.theme_manager().is_dark(),
        "egui dark mode, {when}"
    );
    assert_eq!(
        visuals.panel_fill,
        ctrl.theme_manager().color("windowBg"),
        "egui panel fill, {when}"
    );
    assert_eq!(
        visuals.override_text_color,
        Some(ctrl.theme_manager().color("windowFg")),
        "egui text colour, {when}"
    );
}

#[test]
fn tgb2961_theme_studio_leaves_the_windows_colours_on_the_apps_dark_theme() {
    let mut harness = build_app_harness(WIDE);
    settle_layout(&mut harness);
    harness
        .state_mut()
        .controller
        .choose_embedded_theme(EmbeddedType::Night, None);
    step_frames(&mut harness, 3);
    assert_eq!(harness.state().controller.theme_mode(), ThemeMode::Night);
    assert_window_colours_are_the_apps(&harness, "before the studio opens");
    assert!(shared_wallpaper_is_dark(&harness));

    for tab in [ThemesTab::Editor, ThemesTab::Backgrounds] {
        AppThemesHost::open(&harness.ctx, Some(tab));
        for frame in 0..4 {
            harness.step();
            assert!(AppThemesHost::is_open(&harness.ctx));
            assert_window_colours_are_the_apps(
                &harness,
                &format!("studio open on {tab:?}, frame {frame}"),
            );
            assert!(
                shared_wallpaper_is_dark(&harness),
                "the app's wallpaper stays dark with the studio open on {tab:?}, frame {frame}"
            );
        }
        AppThemesHost::close(&harness.ctx);
        for frame in 0..3 {
            harness.step();
            assert_window_colours_are_the_apps(
                &harness,
                &format!("studio closed after {tab:?}, frame {frame}"),
            );
        }
    }
}

#[test]
fn tgb2962_wallpaper_follows_the_theme_while_only_settings_is_shown() {
    let mut harness = build_app_harness(PHONE);
    settle_layout(&mut harness);
    open_chat_settings(&mut harness.state_mut().controller);
    settle_layout(&mut harness);
    assert!(!shared_wallpaper_is_dark(&harness));

    let night = "Night";
    wait_for_label(&mut harness, night);
    let center = harness.get_by_label(night).rect().center();
    click_at(&mut harness, center);
    wait_until(&mut harness, 8, "the window did not turn dark", |h| {
        h.state().controller.theme_manager().is_dark()
    });
    // The frame after the change, and the ones that follow.
    for frame in 0..4 {
        harness.step();
        assert!(
            shared_wallpaper_is_dark(&harness),
            "the wallpaper thumbnail is dark on frame {frame} after the switch"
        );
    }

    let classic = "Classic";
    let center = harness.get_by_label(classic).rect().center();
    click_at(&mut harness, center);
    wait_until(&mut harness, 8, "the window did not turn light", |h| {
        !h.state().controller.theme_manager().is_dark()
    });
    for frame in 0..4 {
        harness.step();
        assert!(
            !shared_wallpaper_is_dark(&harness),
            "the wallpaper thumbnail is light again on frame {frame}"
        );
    }
}

#[test]
fn tgb2962_a_wallpaper_set_outside_the_studio_is_not_replaced_by_the_studios() {
    let mut harness = build_app_harness(WIDE);
    settle_layout(&mut harness);
    let chosen = tg_app::themes::BackgroundConfig::solid(tg_theme::Color::rgb(0x12, 0x34, 0x56));
    let generation = {
        let shared = active_chat_background(&harness.ctx);
        let mut shared = shared.lock().expect("shared chat background");
        shared.set_global(chosen);
        shared.generation()
    };
    step_frames(&mut harness, 4);
    let shared = active_chat_background(&harness.ctx);
    let shared = shared.lock().expect("shared chat background");
    assert_eq!(
        shared.generation(),
        generation,
        "nothing rewrites the app's wallpaper while the studio's is unchanged"
    );
}

#[test]
fn tgb2963_auto_night_row_exists_only_while_the_system_reports_dark_mode() {
    let row = tr::lng_settings_auto_night_mode();
    let mut harness = build_app_harness(WIDE);
    settle_layout(&mut harness);
    open_chat_settings(&mut harness.state_mut().controller);
    settle_layout(&mut harness);
    // The mock demo gets no system value.
    wait_for_label_contains(&mut harness, &tr::lng_settings_font_family());
    assert!(
        harness.query_by_label_contains(&row).is_none(),
        "no system dark-mode value: no Auto-night mode row"
    );

    for system_dark in [false, true] {
        harness
            .state_mut()
            .controller
            .set_system_dark_mode(Some(system_dark));
        step_frames(&mut harness, 3);
        assert!(
            harness.query_by_label_contains(&row).is_some(),
            "system dark mode {system_dark}: the row is shown"
        );
    }

    harness.state_mut().controller.set_system_dark_mode(None);
    step_frames(&mut harness, 3);
    assert!(harness.query_by_label_contains(&row).is_none());
}

/// The section called directly, as many tests do: the row follows `system_dark_mode` of the
/// state, and a click on it flips the setting (`setSystemDarkModeEnabled(!enabled)`).
#[test]
fn tgb2963_chat_section_row_follows_the_state_and_toggles_the_setting() {
    struct Section {
        state: ChatSettingsState,
        core: CoreSettings,
        global: GlobalConfig,
        live: LiveThemeColors,
        backend: MockSettingsBackend,
        colors: DayPalette,
    }
    let row = tr::lng_settings_auto_night_mode();
    let mut harness = build_widget_harness(
        Section {
            state: ChatSettingsState::default(),
            core: CoreSettings::new(),
            global: GlobalConfig::default(),
            live: LiveThemeColors::default(),
            backend: MockSettingsBackend::new(),
            colors: DayPalette::new(),
        },
        Vec2::new(420.0, 2400.0),
        |ui, s| {
            let _ = render_chat_settings_section(
                ui,
                &mut s.state,
                &mut s.core,
                &mut s.global,
                &mut s.live,
                &mut s.backend,
                &s.colors,
            );
        },
    );
    step_frames(&mut harness, 3);
    assert_eq!(harness.state().state.system_dark_mode, None);
    assert!(harness.query_by_label_contains(&row).is_none());
    let hidden_font_row_top = harness
        .get_by_label_contains(&tr::lng_settings_font_family())
        .rect()
        .top();

    harness.state_mut().state.system_dark_mode = Some(false);
    step_frames(&mut harness, 3);
    let shown_font_row_top = harness
        .get_by_label_contains(&tr::lng_settings_font_family())
        .rect()
        .top();
    assert!(
        shown_font_row_top > hidden_font_row_top,
        "the row takes its place above the font row"
    );
    let before = harness.state().core.system_dark_mode_enabled;
    harness.get_by_label_contains(&row).click();
    step_frames(&mut harness, 3);
    assert_eq!(harness.state().core.system_dark_mode_enabled, !before);
}
