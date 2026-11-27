//! Wave 8 SCREENS: the interface scale restart box (TGB-3111).

use super::*;
use crate::mock_session::MockSession;
use tg_app_settings::settings::SettingsSection;
use tg_core::SCALE_AUTO;

#[test]
fn scale_restart_box_saves_the_scale_and_requests_a_restart() {
    let mock = MockSession::new(true);
    let mut ctrl = WindowController::with_session(
        MockSession::new(true),
        ShellSettings::default(),
        std::env::temp_dir().join("rustgram_w8_scale_restart_test.json"),
    );
    ctrl.main_widget.open_settings_section(
        SettingsSection::Main,
        Box::new(mock.handle()),
        tg_theme::EmbeddedType::DayBlue,
        None,
    );
    let ctx = egui::Context::default();
    assert_eq!(
        ctrl.main_widget
            .settings_mut()
            .map(|s| s.global_config.config_scale),
        Some(SCALE_AUTO)
    );

    // The Settings slider asks first: the box is shown and nothing is saved yet.
    ctrl.handle_main_widget_action(MainWidgetAction::ConfirmScaleRestart(150), &ctx);
    assert!(ctrl.layer_stack.is_layer_shown());
    assert_eq!(
        ctrl.main_widget
            .settings_mut()
            .map(|s| s.global_config.config_scale),
        Some(SCALE_AUTO)
    );
    assert!(!ctrl.take_app_restart_request());

    // "Restart": the scale is written for the settings file and the app restarts.
    ctrl.handle_layer_action(LayerAction::RestartWithScale(150));
    assert_eq!(
        ctrl.main_widget
            .settings_mut()
            .map(|s| s.global_config.config_scale),
        Some(150)
    );
    assert!(ctrl.take_app_restart_request());
    assert!(!ctrl.take_app_restart_request(), "taken once");
}

mod theme_studio {
    //! TGB-2984: the Theme Studio's changes reach the window's theme.

    use super::*;
    use tg_app_settings::themes::{AppThemesHost, ThemesStudio};
    use tg_theme::Color as ThemeColor;

    fn window() -> (WindowController, egui::Context) {
        let ctrl = WindowController::with_session(
            MockSession::new(true),
            ShellSettings::default(),
            std::env::temp_dir().join("rustgram_w8_theme_studio_test.json"),
        );
        (ctrl, egui::Context::default())
    }

    fn window_bg(ctrl: &WindowController) -> ThemeColor {
        ctrl.theme_manager.palette().window_bg
    }

    fn studio<R>(ctx: &egui::Context, f: impl FnOnce(&mut ThemesStudio) -> R) -> R {
        AppThemesHost::with_studio(ctx, f).expect("studio")
    }

    const PINK: ThemeColor = ThemeColor::rgba(0xff, 0x40, 0xa0, 0xff);

    #[test]
    fn an_editor_colour_edit_changes_the_window_palette() {
        let (mut ctrl, ctx) = window();
        // First frame: the studio shows the window's theme and hands nothing back.
        ctrl.exchange_studio_theme(&ctx);
        let before = window_bg(&ctrl);
        assert_eq!(
            studio(&ctx, |s| s.theme_manager.palette().window_bg),
            before
        );
        ctrl.exchange_studio_theme(&ctx);
        assert_eq!(window_bg(&ctrl), before, "no change without an edit");

        studio(&ctx, |s| {
            s.editor
                .apply_color_edit("windowBg", PINK, None, &mut s.theme_manager)
                .expect("edit windowBg");
        });
        ctrl.exchange_studio_theme(&ctx);
        assert_eq!(window_bg(&ctrl), PINK);
        assert!(!ctrl.layer_stack.is_layer_shown(), "editor edits are live");

        // The studio keeps its edit (and undo history) after the window took it.
        ctrl.exchange_studio_theme(&ctx);
        assert!(studio(&ctx, |s| s.editor.can_undo()));
    }

    #[test]
    fn a_theme_file_goes_on_trial_and_revert_restores_the_old_theme() {
        let (mut ctrl, ctx) = window();
        ctrl.exchange_studio_theme(&ctx);
        let before = window_bg(&ctrl);

        let mut file = tg_theme::Palette::default();
        file.window_bg = PINK;
        ctrl.start_theme_trial(file.clone());
        assert_eq!(window_bg(&ctrl), PINK);
        assert!(
            ctrl.layer_stack.is_layer_shown(),
            "Keep this theme? is shown"
        );
        ctrl.handle_layer_action(LayerAction::RevertTheme);
        assert_eq!(window_bg(&ctrl), before);

        ctrl.start_theme_trial(file);
        ctrl.handle_layer_action(LayerAction::KeepTheme);
        assert_eq!(window_bg(&ctrl), PINK);
        // A later revert has nothing to undo.
        ctrl.handle_layer_action(LayerAction::RevertTheme);
        assert_eq!(window_bg(&ctrl), PINK);
    }

    #[test]
    fn the_trial_reverts_by_itself_after_sixteen_seconds() {
        use crate::window::layer_stack::keep_theme_seconds_left;
        use std::time::Duration;
        assert_eq!(keep_theme_seconds_left(Duration::ZERO), 15);
        assert_eq!(keep_theme_seconds_left(Duration::from_millis(14_999)), 1);
        assert_eq!(keep_theme_seconds_left(Duration::from_millis(15_000)), 0);
        assert_eq!(keep_theme_seconds_left(Duration::from_secs(60)), 0);
    }

    #[test]
    fn the_studio_follows_a_window_theme_change() {
        let (mut ctrl, ctx) = window();
        ctrl.exchange_studio_theme(&ctx);
        ctrl.apply_theme(tg_theme::EmbeddedType::NightGreen, None);
        ctrl.exchange_studio_theme(&ctx);
        assert!(studio(&ctx, |s| s.theme_manager.is_dark()));
        assert_eq!(
            studio(&ctx, |s| s.theme_manager.palette().window_bg),
            window_bg(&ctrl)
        );
    }
}

/// TGB-2920: lookups of one chat (separate windows, chat preview, Ctrl+Tab, "Open in new
/// window") share one chat-list read per list version instead of converting every chat each
/// time.
#[test]
fn chat_lookups_share_one_list_read_per_version() {
    let mut ctrl = WindowController::with_session(
        MockSession::new(true),
        ShellSettings::default(),
        std::env::temp_dir().join("rustgram_w8_dialogs_snapshot_test.json"),
    );
    let first = ctrl.all_dialogs();
    assert!(!first.is_empty());
    for _ in 0..60 {
        let again = ctrl.all_dialogs();
        assert!(std::sync::Arc::ptr_eq(&first, &again));
    }
    assert_eq!(ctrl.all_dialogs_reads(), 1);
    // Same entries as the backend's own list, archived chats and all.
    assert_eq!(
        first.iter().map(|d| d.peer.id).collect::<Vec<_>>(),
        ctrl.session_backend()
            .dialogs(FilterId::ALL, "")
            .iter()
            .map(|d| d.peer.id)
            .collect::<Vec<_>>()
    );
}

/// TGB-2920 timing: one frame's chat lookup before (a full `dialogs()` read) and after (the
/// shared snapshot). Prints numbers only; run with `--ignored --nocapture`.
#[test]
#[ignore = "timing for TGB-2920; prints numbers"]
fn measure_chat_lookup_per_frame() {
    let mut ctrl = WindowController::with_session(
        MockSession::new(true),
        ShellSettings::default(),
        std::env::temp_dir().join("rustgram_w8_dialogs_timing.json"),
    );
    let frames = 600_u32;
    let started = std::time::Instant::now();
    for _ in 0..frames {
        std::hint::black_box(ctrl.session_backend().dialogs(FilterId::ALL, ""));
    }
    let before = started.elapsed() / frames;
    let started = std::time::Instant::now();
    for _ in 0..frames {
        std::hint::black_box(ctrl.all_dialogs());
    }
    let after = started.elapsed() / frames;
    println!(
        "TGB-2920 chats {}: dialogs() {before:?} per frame, snapshot {after:?} per frame",
        ctrl.all_dialogs().len()
    );
}
