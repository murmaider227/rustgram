//! TGB-3159 (walk OB-09): the tray menu holds exactly tdesktop's entries (`Tray::rebuildMenu`,
//! `tray.cpp:80-115`): "Minimize to tray" / "Open Telegram", "Disable notifications" /
//! "Enable notifications" (hidden while passcode-locked), "Quit", then the accounts block.

use tg_platform::tray::TrayMenuItem;

use super::{MockTrayPlatform, TrayController, WorkMode};
use crate::notifications::settings::NotificationPreferences;

const NOW_UNIX: i64 = 1_795_340_000;

fn release_tray() -> TrayController {
    let mut tray = TrayController::with_platform(
        Box::new(MockTrayPlatform::default()),
        WorkMode::WindowAndTray,
        false,
        None,
    );
    // What a release build gets by default.
    tray.set_include_debug_simulate_item(false);
    tray
}

fn labels(items: &[TrayMenuItem]) -> Vec<String> {
    items
        .iter()
        .map(|item| match item {
            TrayMenuItem::Action { label, .. } => label.clone(),
            TrayMenuItem::Separator => "---".to_owned(),
        })
        .collect()
}

#[test]
fn release_menu_has_exactly_the_tdesktop_entries() {
    let tray = release_tray();
    let prefs = NotificationPreferences::default();
    assert_eq!(
        labels(&tray.build_menu(&prefs, NOW_UNIX)),
        vec![
            tg_lang::tr::lng_minimize_to_tray(),
            tg_lang::tr::lng_disable_notifications_from_tray(),
            tg_lang::tr::lng_quit_from_tray(),
        ]
    );
}

#[test]
fn release_menu_while_globally_muted_has_no_mute_entry() {
    let tray = release_tray();
    let mut prefs = NotificationPreferences::default();
    prefs.mute_globally_for(NOW_UNIX, 3600);
    prefs.desktop_notify = false;
    assert_eq!(
        labels(&tray.build_menu(&prefs, NOW_UNIX)),
        vec![
            tg_lang::tr::lng_minimize_to_tray(),
            tg_lang::tr::lng_enable_notifications_from_tray(),
            tg_lang::tr::lng_quit_from_tray(),
        ]
    );
}

#[test]
fn passcode_locked_menu_has_open_and_quit_only() {
    let mut tray = release_tray();
    let prefs = NotificationPreferences::default();
    tray.sync_state(false, true, &prefs, NOW_UNIX);
    assert_eq!(
        labels(&tray.build_menu(&prefs, NOW_UNIX)),
        vec![
            tg_lang::tr::lng_open_from_tray(),
            tg_lang::tr::lng_quit_from_tray(),
        ]
    );
}

/// Writes the default menu of this build as text into `$CHATUI_SNAP_DIR/<stage>/` (the
/// before | after record of the menu; the stage is `$CHATUI_SNAP_STAGE`).
#[test]
#[ignore = "writes the tray menu dump under CHATUI_SNAP_DIR"]
fn snap_tray_menu() {
    let Some(dir) = std::env::var_os("CHATUI_SNAP_DIR").map(std::path::PathBuf::from) else {
        return;
    };
    let stage = std::env::var("CHATUI_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned());
    let dir = dir.join(stage);
    std::fs::create_dir_all(&dir).expect("snapshot dir");
    let mut out = String::new();
    for (name, debug_item) in [("release", false), ("debug", true)] {
        let mut tray = release_tray();
        tray.set_include_debug_simulate_item(debug_item);
        let prefs = NotificationPreferences::default();
        out.push_str(&format!("[{name}]\n"));
        for label in labels(&tray.build_menu(&prefs, NOW_UNIX)) {
            out.push_str(&label);
            out.push('\n');
        }
    }
    std::fs::write(dir.join("tgb3159_tray_menu.txt"), out).expect("write dump");
}

#[test]
fn the_simulate_entry_is_on_only_in_debug_builds() {
    let tray = TrayController::with_platform(
        Box::new(MockTrayPlatform::default()),
        WorkMode::WindowAndTray,
        false,
        None,
    );
    let prefs = NotificationPreferences::default();
    let has_simulate = labels(&tray.build_menu(&prefs, NOW_UNIX))
        .iter()
        .any(|l| l == super::strings::SIMULATE_INCOMING_MESSAGE);
    assert_eq!(has_simulate, cfg!(debug_assertions));
}
