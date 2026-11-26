//! Wave 4 SETTINGS evidence renders (TGB-2961, TGB-2962, TGB-2963) from the mock demo.
//!
//! Ignored by default. Run with `TGFORK_W4_SETTINGS_SNAP=<dir>` and
//! `TGFORK_W4_SETTINGS_SNAP_TAG=before|after`; the PNGs stay outside the repository.

use std::path::PathBuf;

use egui::Vec2;
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use tg_app::settings::SettingsSection;
use tg_app::themes::{AppThemesHost, ThemesTab};
use tg_app::window::controller::WindowController;
use tg_app::window::section::SettingsPage;
use tg_theme::EmbeddedType;

use crate::common::{
    AppTestState, TEST_STEP_DT, apply_deterministic_style, click_at, settle_layout, step_frames,
    wait_for_label,
};

const SIZES: [(&str, Vec2); 2] = [
    ("phone", Vec2::new(390.0, 844.0)),
    ("desktop", Vec2::new(1440.0, 900.0)),
];

fn build(size: Vec2) -> Harness<'static, AppTestState> {
    HarnessBuilder::default()
        .with_size(size)
        .with_step_dt(TEST_STEP_DT)
        .with_render_cursor(false)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
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

fn save(harness: &mut Harness<'_, AppTestState>, dir: &std::path::Path, name: &str) {
    let tag = std::env::var("TGFORK_W4_SETTINGS_SNAP_TAG").unwrap_or_else(|_| "after".to_owned());
    let image = harness.render().expect("render");
    let path = dir.join(format!("{name}_{tag}.png"));
    image.save(&path).expect("save png");
}

#[test]
#[ignore = "evidence renders; set TGFORK_W4_SETTINGS_SNAP"]
fn w4_settings_evidence_renders() {
    let Some(dir) = std::env::var_os("TGFORK_W4_SETTINGS_SNAP").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).expect("snap dir");

    for (size_name, size) in SIZES {
        // TGB-2962 + TGB-2963: Chat settings after a click on the Night card.
        let mut harness = build(size);
        settle_layout(&mut harness);
        open_chat_settings(&mut harness.state_mut().controller);
        settle_layout(&mut harness);
        save(
            &mut harness,
            &dir,
            &format!("chat_settings_day_{size_name}"),
        );
        wait_for_label(&mut harness, "Night");
        let center = harness.get_by_label("Night").rect().center();
        click_at(&mut harness, center);
        step_frames(&mut harness, 6);
        save(
            &mut harness,
            &dir,
            &format!("chat_settings_night_{size_name}"),
        );

        // TGB-2961: the dark app with the Theme Studio opened and closed again.
        let mut harness = build(size);
        settle_layout(&mut harness);
        harness
            .state_mut()
            .controller
            .choose_embedded_theme(EmbeddedType::Night, None);
        step_frames(&mut harness, 4);
        open_chat_settings(&mut harness.state_mut().controller);
        settle_layout(&mut harness);
        AppThemesHost::open(&harness.ctx, Some(ThemesTab::Editor));
        step_frames(&mut harness, 4);
        save(&mut harness, &dir, &format!("studio_open_{size_name}"));
        AppThemesHost::close(&harness.ctx);
        step_frames(&mut harness, 4);
        save(&mut harness, &dir, &format!("studio_closed_{size_name}"));
    }
}
