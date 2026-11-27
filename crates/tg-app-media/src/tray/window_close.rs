//! What closing the main window does (TGB-3122).
//!
//! Port of `MainWindow::closeEvent` (`mainwindow.cpp`) and `Window::MainWindow::hideNoQuit` /
//! `minimizeToTray` (`window/main_window.cpp`): unless the app is quitting, a window with a
//! signed-in account hides to the tray when the work mode shows a tray icon; otherwise the
//! "Window close" setting (`Core::Settings::CloseBehavior`) decides between quitting,
//! minimizing to the taskbar and running in the background.
//!
//! Every real quit (tray "Quit", Ctrl+Q, a restart, `-quit` from another instance) goes through
//! [`request_quit`], which marks the context as quitting (`Core::Quitting`) before it closes the
//! window, so those closes are never turned into a hide.

use egui::{Context, Id, ViewportCommand};
use tg_core::settings::CloseBehavior;

use super::WorkMode;

/// What the main window does with a close request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowCloseAction {
    /// Let the window close and the app quit (`Core::Quit`).
    Quit,
    /// Hide the window and keep the tray icon (`minimizeToTray`).
    HideToTray,
    /// Minimize the window to the taskbar (`CloseBehavior::CloseToTaskbar`).
    Minimize,
    /// Hide the window without a tray icon (`CloseBehavior::RunInBackground`,
    /// `closeWithoutDestroy`); starting the app again shows it.
    HideInBackground,
}

/// `MainWindow::closeEvent` + `hideNoQuit` for the main window.
#[must_use]
pub const fn window_close_action(
    quitting: bool,
    has_auth: bool,
    work_mode: WorkMode,
    tray_has_icon: bool,
    close_behavior: CloseBehavior,
) -> WindowCloseAction {
    if quitting || !has_auth {
        return WindowCloseAction::Quit;
    }
    if matches!(work_mode, WorkMode::TrayOnly | WorkMode::WindowAndTray) && tray_has_icon {
        return WindowCloseAction::HideToTray;
    }
    match close_behavior {
        CloseBehavior::RunInBackground => WindowCloseAction::HideInBackground,
        CloseBehavior::CloseToTaskbar => WindowCloseAction::Minimize,
        CloseBehavior::Quit => WindowCloseAction::Quit,
    }
}

fn quitting_id() -> Id {
    Id::unique("tg_app_quitting")
}

/// Quits the app: marks `ctx` as quitting and closes the main window. Use it for every real
/// quit; a bare `ViewportCommand::Close` is a window close, which may only hide the window.
pub fn request_quit(ctx: &Context) {
    ctx.data_mut(|d| d.insert_temp(quitting_id(), true));
    ctx.send_viewport_cmd(ViewportCommand::Close);
}

/// Whether [`request_quit`] was called for `ctx` (`Core::Quitting`).
#[must_use]
pub fn is_quitting(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp(quitting_id()).unwrap_or(false))
}

/// Sends the viewport commands for `action` after a close request of the main window. For
/// anything but [`WindowCloseAction::Quit`] the close is cancelled (`QCloseEvent::ignore`).
pub fn apply_window_close_action(ctx: &Context, action: WindowCloseAction) {
    match action {
        WindowCloseAction::Quit => {}
        WindowCloseAction::HideToTray | WindowCloseAction::HideInBackground => {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        }
        WindowCloseAction::Minimize => {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_BEHAVIORS: [CloseBehavior; 3] = [
        CloseBehavior::Quit,
        CloseBehavior::CloseToTaskbar,
        CloseBehavior::RunInBackground,
    ];

    #[test]
    fn default_work_mode_with_a_tray_icon_hides_to_the_tray() {
        for behavior in ALL_BEHAVIORS {
            for mode in [WorkMode::WindowAndTray, WorkMode::TrayOnly] {
                assert_eq!(
                    window_close_action(false, true, mode, true, behavior),
                    WindowCloseAction::HideToTray,
                    "{mode:?} {behavior:?}"
                );
            }
        }
    }

    #[test]
    fn quitting_or_no_account_always_quits() {
        for behavior in ALL_BEHAVIORS {
            assert_eq!(
                window_close_action(true, true, WorkMode::WindowAndTray, true, behavior),
                WindowCloseAction::Quit
            );
            assert_eq!(
                window_close_action(false, false, WorkMode::WindowAndTray, true, behavior),
                WindowCloseAction::Quit
            );
        }
    }

    #[test]
    fn without_a_tray_the_close_behavior_decides() {
        for (mode, has_icon) in [
            (WorkMode::WindowOnly, false),
            // `minimizeToTray` fails when the tray icon could not be created.
            (WorkMode::WindowAndTray, false),
        ] {
            assert_eq!(
                window_close_action(false, true, mode, has_icon, CloseBehavior::Quit),
                WindowCloseAction::Quit
            );
            assert_eq!(
                window_close_action(false, true, mode, has_icon, CloseBehavior::CloseToTaskbar),
                WindowCloseAction::Minimize
            );
            assert_eq!(
                window_close_action(false, true, mode, has_icon, CloseBehavior::RunInBackground),
                WindowCloseAction::HideInBackground
            );
        }
        // WindowOnly ignores a tray icon that has not gone away yet.
        assert_eq!(
            window_close_action(false, true, WorkMode::WindowOnly, true, CloseBehavior::Quit),
            WindowCloseAction::Quit
        );
    }

    #[test]
    fn request_quit_marks_the_context_and_closes() {
        let ctx = Context::default();
        assert!(!is_quitting(&ctx));
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| request_quit(ui.ctx()));
        out.textures_delta.clear();
        assert!(is_quitting(&ctx));
        let cmds = out
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.commands.clone())
            .unwrap_or_default();
        assert!(cmds.contains(&ViewportCommand::Close));
    }

    #[test]
    fn hiding_cancels_the_close() {
        for (action, expected) in [
            (
                WindowCloseAction::HideToTray,
                vec![
                    ViewportCommand::CancelClose,
                    ViewportCommand::Visible(false),
                ],
            ),
            (
                WindowCloseAction::HideInBackground,
                vec![
                    ViewportCommand::CancelClose,
                    ViewportCommand::Visible(false),
                ],
            ),
            (
                WindowCloseAction::Minimize,
                vec![
                    ViewportCommand::CancelClose,
                    ViewportCommand::Minimized(true),
                ],
            ),
            (WindowCloseAction::Quit, Vec::new()),
        ] {
            let ctx = Context::default();
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                apply_window_close_action(ui.ctx(), action);
            });
            out.textures_delta.clear();
            let mut cmds = out
                .viewport_output
                .get(&egui::ViewportId::ROOT)
                .map(|v| v.commands.clone())
                .unwrap_or_default();
            // egui's own first-frame command.
            cmds.retain(|c| !matches!(c, ViewportCommand::SetTheme(_)));
            assert_eq!(cmds, expected, "{action:?}");
        }
    }
}
