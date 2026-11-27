//! End-to-end UI tests for Settings:
//! - Open Settings from the hamburger Main Menu drawer
//! - Toggle Night Mode and verify theme mode & palette luminance flip
//! - Change interface scale via Settings Main scale controls
//! - Open Privacy and Security and Notifications and Sounds sections

use egui::{Key, Modifiers, Pos2, Vec2};
use egui_kittest::kittest::Queryable;
use tg_app::dialogs::filters_menu::SIDEBAR_MENU_BUTTON_HEIGHT;
use tg_app::settings::SettingsSection;
use tg_app::stand_in::theme::ThemeMode;
use tg_app::window::main_menu::MAIN_MENU_COVER_HEIGHT;
use tg_app::window::section::{SectionMemento, SettingsPage};

use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, build_harness, click_at, color_luminance, press_key,
    settle_animations, settle_layout, type_text, wait_for_label_settled, wait_until,
};

const WORKSPACE_SIZE: Vec2 = Vec2::new(1280.0, 820.0);

/// Opens the slide-out Main Menu drawer by clicking the top-left hamburger button.
fn open_main_menu(harness: &mut egui_kittest::Harness<'_, AppTestState>) {
    click_at(harness, Pos2::new(36.0, SIDEBAR_MENU_BUTTON_HEIGHT * 0.5));
    wait_for_label_settled(harness, "Rustgram Desktop");
}

/// Computes the vertical center of row index `idx` after the separator in `MainMenu`:
/// Cover (134px) + space (6px) + My Profile (42px) + Separator (8px) + rows (42px each):
/// 0=New Group, 1=New Channel, 2=Contacts, 3=Calls, 4=Saved Messages, 5=Downloads, 6=Settings, 7=Night Mode.
/// The mock session's two side-menu bots ("Catalog Shop", "Quick Wallet") sit between
/// My Profile and the separator (`SetupMenuBots`), 40 px each.
const MOCK_SIDE_MENU_BOTS_H: f32 = 2.0 * 40.0;

fn main_menu_item_pos(row_after_sep: usize) -> Pos2 {
    let base_y = MAIN_MENU_COVER_HEIGHT + 6.0 + 42.0 + MOCK_SIDE_MENU_BOTS_H + 8.0;
    Pos2::new(120.0, base_y + (row_after_sep as f32 + 0.5) * 42.0)
}

#[test]
fn settings_open_from_main_menu_and_toggle_night_mode() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // Starts in Day mode with bright background.
    assert_eq!(harness.state().controller.theme_mode(), ThemeMode::Day);
    let day_lum = color_luminance(harness.state().controller.palette().window_bg);
    assert!(
        day_lum > 180.0,
        "Day palette window_bg should be light, got {day_lum}"
    );

    // 1. Open Main Menu and click Night Mode row (index 7 after separator).
    open_main_menu(&mut harness);
    click_at(&mut harness, main_menu_item_pos(7));
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking Night Mode in Main Menu must switch ThemeMode to Night",
        |h| {
            h.state().controller.theme_mode() == ThemeMode::Night
                && color_luminance(h.state().controller.palette().window_bg) < 80.0
        },
    );

    // 2. Click Settings row in Main Menu (index 6 after separator; drawer stayed open after Night Mode toggle).
    click_at(&mut harness, main_menu_item_pos(6));
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking Settings in Main Menu must open Settings Main page",
        |h| {
            !h.state().controller.is_animating()
                && h.state().controller.session().is_some_and(|s| {
                    s.main_section()
                        == Some(&SectionMemento::Settings {
                            page: SettingsPage::Main,
                        })
                })
                && h.query_by_label("Settings").is_some()
        },
    );
}

#[test]
fn settings_change_interface_scale_and_open_privacy_and_notifications() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // Open Settings from Main Menu.
    open_main_menu(&mut harness);
    click_at(&mut harness, main_menu_item_pos(6));
    wait_for_label_settled(&mut harness, "100%");

    // Verify we can open Privacy and Security via live Settings Search (AccessKit TextInput!).
    let geo = harness
        .state()
        .controller
        .session()
        .expect("session")
        .layout();
    let main_left = (geo.filters_width + geo.dialogs_width) as f32;

    // 1. Change interface scale: locate the "100%" scale label via AccessKit on Settings Main;
    // the MediaSlider sits immediately to the left of the percentage label on the same horizontal row!
    let scale_lbl = harness
        .query_by_label("100%")
        .expect("Default Interface Scale percentage label '100%' should be in Settings Main");
    scale_lbl.scroll_to_me();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "100% scale label must scroll into viewport",
        |h| {
            !h.state().controller.is_animating()
                && h.query_by_label("100%").is_some_and(|n| {
                    let r = n.rect();
                    r.is_positive() && r.min.y >= 0.0 && r.max.y <= WORKSPACE_SIZE.y
                })
        },
    );
    wait_for_label_settled(&mut harness, "100%");
    let lbl_rect = harness.get_by_label("100%").rect();
    // Click along the MediaSlider to the left of the "100%" label to change scale (e.g. 200%/250%).
    let slider_click = Pos2::new(lbl_rect.min.x - 120.0, lbl_rect.center().y);
    click_at(&mut harness, slider_click);
    // A scale that changes the evaluated scale asks "Restart now?" first (TGB-3111,
    // `SetupInterfaceScale`'s `setScale`); "Restart" saves it and asks the app to restart.
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking the Default Interface Scale slider must ask to restart",
        |h| h.query_by_label("Restart").is_some(),
    );
    let restart = harness.get_by_label("Restart").rect().center();
    click_at(&mut harness, restart);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Restart must save a scale other than 100%",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .settings_mut()
                .is_some_and(|s| s.global_config.scale != 100)
        },
    );
    assert!(
        harness.state_mut().controller.take_app_restart_request(),
        "Restart must ask the app to restart"
    );

    // 2. Open Privacy and Security via Settings Search: the header search icon (7.4.6 has no
    // always-visible search field on Settings Main) opens the search input.
    let search_label = tg_lang::tr::lng_dlg_filter();
    let header_search = harness
        .get_all_by_label(&search_label)
        .find(|n| n.rect().center().x > main_left && n.rect().center().y < 80.0)
        .expect("Settings header search icon button should be discoverable via AccessKit");
    header_search.click();
    settle_animations(&mut harness);
    let search_box = harness
        .get_all_by_role(egui::accesskit::Role::TextInput)
        .find(|n| n.rect().center().x > main_left)
        .expect("Settings search TextInput should be discoverable via AccessKit");
    search_box.scroll_to_me();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Settings search TextInput must scroll back into viewport",
        |h| {
            !h.state().controller.is_animating()
                && h.query_all_by_role(egui::accesskit::Role::TextInput)
                    .find(|n| n.rect().center().x > main_left)
                    .is_some_and(|n| {
                        let r = n.rect();
                        r.is_positive() && r.min.y >= 0.0 && r.max.y <= WORKSPACE_SIZE.y
                    })
        },
    );
    settle_animations(&mut harness);
    let search_box = harness
        .get_all_by_role(egui::accesskit::Role::TextInput)
        .find(|n| n.rect().center().x > main_left)
        .expect("Settings search TextInput after scrolling back to top");
    let search_rect = search_box.rect();
    search_box.click();
    harness.step();
    type_text(&mut harness, "blocked");
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Settings search query must update to 'blocked'",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .settings_mut()
                .is_some_and(|s| s.search_query == "blocked")
        },
    );
    harness.step();

    // Click the first search result row directly below the "Search Results" subsection heading.
    let first_hit_pos = Pos2::new(search_rect.center().x, search_rect.max.y + 62.0);
    click_at(&mut harness, first_hit_pos);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking Privacy search result must open Privacy and Security section",
        |h| {
            !h.state().controller.is_animating()
                && h.state_mut()
                    .controller
                    .main_widget_mut()
                    .settings_mut()
                    .is_some_and(|s| {
                        s.section_stack.last().copied() == Some(SettingsSection::PrivacySecurity)
                    })
                && h.query_by_label("Privacy and Security").is_some()
        },
    );

    // 3. Navigate back to Settings Main via Escape, then open Notifications and Sounds.
    press_key(&mut harness, Modifiers::NONE, Key::Escape);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Escape must return to Settings Main",
        |h| {
            !h.state().controller.is_animating()
                && h.state_mut()
                    .controller
                    .main_widget_mut()
                    .settings_mut()
                    .is_some_and(|s| s.section_stack.last().copied() == Some(SettingsSection::Main))
        },
    );

    // Search for "corner" and open the Notifications and Sounds section.
    let search_box = harness
        .get_all_by_role(egui::accesskit::Role::TextInput)
        .find(|n| n.rect().center().x > main_left)
        .expect("Settings search TextInput");
    let search_rect = search_box.rect();
    search_box.click();
    harness.step();
    press_key(&mut harness, Modifiers::COMMAND, Key::A);
    press_key(&mut harness, Modifiers::NONE, Key::Backspace);
    type_text(&mut harness, "corner");
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Settings search query must update to 'corner'",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .settings_mut()
                .is_some_and(|s| s.search_query == "corner")
        },
    );
    harness.step();

    let notify_hit_pos = Pos2::new(search_rect.center().x, search_rect.max.y + 62.0);
    click_at(&mut harness, notify_hit_pos);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking Notifications search result must open Notifications and Sounds section",
        |h| {
            !h.state().controller.is_animating()
                && h.state_mut()
                    .controller
                    .main_widget_mut()
                    .settings_mut()
                    .is_some_and(|s| {
                        s.section_stack.last().copied() == Some(SettingsSection::Notifications)
                    })
                && h.query_by_label("Notifications and Sounds").is_some()
        },
    );
}
