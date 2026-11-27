//! TGB-3122: closing the main window hides it to the tray; a tray click restores it; the tray's
//! Quit quits; without a tray the "Window close" setting decides.

use std::sync::{Arc, Mutex};

use egui::{Context, RawInput, ViewportCommand, ViewportId};
use tg_core::CoreSettings;
use tg_core::settings::{CloseBehavior, WorkMode as CoreWorkMode};
use tg_platform::tray::TrayEvent;

use super::AppNotifyTrayHost;
use crate::notifications::manager::NotificationSystem;
use crate::notifications::mock::MockNotifyBackend;
use crate::notifications::settings::{NotificationPreferences, NotifySettingsStore};
use crate::tray::window_close::{WindowCloseAction, is_quitting};
use crate::tray::{MockTrayPlatform, menu_id};

fn host() -> (AppNotifyTrayHost, Arc<Mutex<MockTrayPlatform>>) {
    let platform = Arc::new(Mutex::new(MockTrayPlatform::default()));
    let system = NotificationSystem::headless(
        NotificationPreferences::default(),
        NotifySettingsStore::default(),
        std::env::temp_dir().join("rustgram_w8_close_userpics"),
    );
    let host = AppNotifyTrayHost::with_backend_and_platform(
        Box::new(MockNotifyBackend::new()),
        system,
        Box::new(Arc::clone(&platform)),
        Box::new(|_| {}),
    );
    (host, platform)
}

/// Runs one frame of the tray host and returns the main viewport's commands.
fn frame(
    ctx: &Context,
    host: &mut AppNotifyTrayHost,
    close_requested: bool,
    has_auth: bool,
) -> (Option<WindowCloseAction>, Vec<ViewportCommand>) {
    let mut input = RawInput::default();
    if close_requested {
        input.viewports.entry(ViewportId::ROOT).or_default().events =
            vec![egui::ViewportEvent::Close];
    }
    let mut action = None;
    let palette = tg_app_core::stand_in::theme::Palette::for_mode(
        tg_app_core::stand_in::theme::ThemeMode::Day,
    );
    let mut out = ctx.run_ui(input, |ui| {
        host.update_and_render(ui.ctx(), &palette, false);
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            action = Some(host.on_main_window_close_requested(ui.ctx(), has_auth));
        }
    });
    out.textures_delta.clear();
    let mut cmds = out
        .viewport_output
        .get(&ViewportId::ROOT)
        .map(|v| v.commands.clone())
        .unwrap_or_default();
    cmds.retain(|c| !matches!(c, ViewportCommand::SetTheme(_)));
    (action, cmds)
}

#[test]
fn close_hides_to_the_tray_and_a_tray_click_restores() {
    let ctx = Context::default();
    let (mut host, platform) = host();
    let (action, cmds) = frame(&ctx, &mut host, true, true);
    assert_eq!(action, Some(WindowCloseAction::HideToTray));
    assert!(cmds.contains(&ViewportCommand::CancelClose));
    assert!(cmds.contains(&ViewportCommand::Visible(false)));
    assert!(host.is_window_hidden());
    assert!(!is_quitting(&ctx));

    if let Ok(mut p) = platform.lock() {
        p.push_event(TrayEvent::LeftClick);
    }
    let (_, cmds) = frame(&ctx, &mut host, false, true);
    assert!(cmds.contains(&ViewportCommand::Visible(true)), "{cmds:?}");
    assert!(!host.is_window_hidden());
}

#[test]
fn tray_quit_quits_and_its_close_is_not_turned_into_a_hide() {
    let ctx = Context::default();
    let (mut host, platform) = host();
    if let Ok(mut p) = platform.lock() {
        p.push_event(TrayEvent::MenuItem(menu_id::QUIT));
    }
    let (_, cmds) = frame(&ctx, &mut host, false, true);
    assert!(cmds.contains(&ViewportCommand::Close));
    assert!(is_quitting(&ctx));
    // The close that follows quits.
    let (action, cmds) = frame(&ctx, &mut host, true, true);
    assert_eq!(action, Some(WindowCloseAction::Quit));
    assert!(!cmds.contains(&ViewportCommand::CancelClose));
}

#[test]
fn window_only_follows_the_close_setting() {
    for (behavior, expected) in [
        (CloseBehavior::Quit, WindowCloseAction::Quit),
        (CloseBehavior::CloseToTaskbar, WindowCloseAction::Minimize),
        (
            CloseBehavior::RunInBackground,
            WindowCloseAction::HideInBackground,
        ),
    ] {
        let ctx = Context::default();
        let (mut host, _platform) = host();
        let core = CoreSettings {
            work_mode: CoreWorkMode::WindowOnly,
            close_behavior: behavior,
            ..CoreSettings::new()
        };
        host.apply_core_settings(&core);
        let (action, cmds) = frame(&ctx, &mut host, true, true);
        assert_eq!(action, Some(expected), "{behavior:?}");
        assert_eq!(
            cmds.contains(&ViewportCommand::CancelClose),
            expected != WindowCloseAction::Quit,
            "{behavior:?}: {cmds:?}"
        );
    }
}

#[test]
fn close_without_an_account_quits() {
    let ctx = Context::default();
    let (mut host, _platform) = host();
    let (action, cmds) = frame(&ctx, &mut host, true, false);
    assert_eq!(action, Some(WindowCloseAction::Quit));
    assert!(!cmds.contains(&ViewportCommand::CancelClose));
}
