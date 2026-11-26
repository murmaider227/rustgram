//! TGB-2892: Settings -> "Telegram Premium" opens tdesktop's Premium section and
//! Settings -> "Send a Gift" opens the "Send a Gift" box (`boxes/star_gift_box.cpp`), never the
//! developer Payments overlay (screen tabs, filter pills, "Star Gift #<id>" titles).

use egui::accesskit::Role;
use egui::{Key, Modifiers, Vec2};
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use tempfile::TempDir;
use tg_ui::fonts::register_tdesktop_fonts;

use tg_app::payments::{AppPaymentsHost, PaymentsScreen, dev_overlay_enabled};
use tg_app::stand_in::mock_session::MockSession;
use tg_app::stand_in::settings::ShellSettings;
use tg_app::window::controller::WindowController;
use tg_app::window::section::SettingsPage;

const DEV_TABS: [&str; 7] = [
    "Checkout",
    "Stars",
    "Gifts",
    "Premium",
    "Boosts",
    "Affiliate",
    "Passport",
];

fn demo_controller() -> (WindowController, TempDir) {
    let tmp = TempDir::new().expect("tempdir");
    let ctrl = WindowController::with_session(
        MockSession::new(true),
        ShellSettings::default(),
        tmp.path().join("demo_settings.json"),
    );
    (ctrl, tmp)
}

fn demo_harness(ctrl: WindowController) -> Harness<'static, WindowController> {
    HarnessBuilder::default()
        .with_size(Vec2::new(1180.0, 1280.0))
        .with_step_dt(0.05)
        .with_max_steps(8)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
        })
        .build_ui_state(
            |ui, state: &mut WindowController| {
                let ctx = ui.ctx().clone();
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        state.show(ui);
                    });
                state.dispatch_demo_overlay_requests(&ctx);
                let palette = state.palette();
                let mode = state.theme_mode();
                AppPaymentsHost::update_and_render(&ctx, &palette, mode);
            },
            ctrl,
        )
}

fn open_settings_row(label: &str) -> (Harness<'static, WindowController>, TempDir) {
    let (mut ctrl, tmp) = demo_controller();
    ctrl.session_mut()
        .expect("session")
        .show_settings(SettingsPage::Main);
    let mut harness = demo_harness(ctrl);
    harness.step();
    harness.step();
    harness.get_by_label(label).click();
    for _ in 0..4 {
        harness.step();
    }
    (harness, tmp)
}

fn assert_no_developer_overlay(harness: &Harness<'_, WindowController>) {
    for tab in DEV_TABS {
        assert!(
            harness.query_by_role_and_label(Role::Tab, tab).is_none(),
            "developer payments tab '{tab}' must not be shown"
        );
    }
    assert!(
        harness
            .query_by_label_contains("Star Gifts & Marketplace")
            .is_none()
    );
    assert!(harness.query_by_label_contains("Star Gift #").is_none());
    assert!(
        harness
            .query_by_label_contains("My Gifts & Collections")
            .is_none()
    );
}

#[test]
fn settings_send_a_gift_opens_gift_box_not_developer_overlay() {
    if dev_overlay_enabled() {
        return;
    }
    let (harness, _tmp) = open_settings_row("Send a Gift");
    assert!(AppPaymentsHost::is_open(&harness.ctx));
    assert_eq!(
        AppPaymentsHost::active_screen(&harness.ctx),
        Some(PaymentsScreen::StarGifts)
    );
    assert_no_developer_overlay(&harness);

    // `MakeGiftsPriceTabs`: "All Gifts" is always there, the box closes with "Back".
    assert!(
        harness
            .query_by_role_and_label(Role::Tab, &tg_lang::tr::lng_gift_stars_tabs_all())
            .is_some()
    );
    assert!(
        harness
            .query_by_label(&tg_lang::tr::lng_create_group_back())
            .is_some()
    );

    // Gift cards are `giftBoxGiftHeight` x ((boxWideWidth - padding - skips) / 3) = 107 x 164.
    let card = harness.get_by_label("Starlight Cake");
    let rect = card.rect();
    assert!(
        (rect.width() - 107.0).abs() < 1.0,
        "card width {}",
        rect.width()
    );
    assert!(
        (rect.height() - 164.0).abs() < 1.0,
        "card height {}",
        rect.height()
    );
}

#[test]
fn gift_box_back_button_closes_the_box() {
    if dev_overlay_enabled() {
        return;
    }
    let (mut harness, _tmp) = open_settings_row("Send a Gift");
    harness
        .get_by_label(&tg_lang::tr::lng_create_group_back())
        .click();
    harness.step();
    harness.step();
    assert!(!AppPaymentsHost::is_open(&harness.ctx));
}

#[test]
fn settings_premium_opens_premium_section() {
    if dev_overlay_enabled() {
        return;
    }
    let (harness, _tmp) = open_settings_row("Telegram Premium");
    assert!(AppPaymentsHost::is_open(&harness.ctx));
    assert_eq!(
        AppPaymentsHost::active_screen(&harness.ctx),
        Some(PaymentsScreen::Premium)
    );
    assert_no_developer_overlay(&harness);
    // Feature rows from `EntryMap` with lang.strings titles, and the Subscribe button.
    for title in [
        tg_lang::tr::lng_premium_summary_subtitle_stories(),
        tg_lang::tr::lng_premium_summary_subtitle_double_limits(),
        tg_lang::tr::lng_premium_summary_subtitle_no_ads(),
    ] {
        assert!(
            harness.query_by_label(&title).is_some(),
            "missing Premium feature row '{title}'"
        );
    }
    assert!(harness.query_by_label_contains("Subscribe for").is_some());
    assert!(
        harness
            .query_by_role_and_label(
                Role::RadioButton,
                &tg_lang::tr::lng_premium_subscribe_months_12()
            )
            .is_some()
    );
}

#[test]
fn ctrl_shift_p_does_not_open_the_developer_overlay() {
    if dev_overlay_enabled() {
        return;
    }
    let (ctrl, _tmp) = demo_controller();
    let mut harness = demo_harness(ctrl);
    harness.step();
    harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::P);
    harness.step();
    harness.step();
    assert!(!AppPaymentsHost::is_open(&harness.ctx));
}
