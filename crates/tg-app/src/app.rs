//! `eframe::App` shell integration, `tg-ui` fonts & theme synchronization, unified
//! `MockSession` wiring across `WindowController`, `AppNotifyTrayHost`, and `AppWebViewHost`,
//! and persisted window geometry tracking.
//!
//! Port origin: `Telegram/SourceFiles/window/main_window.cpp` and `Telegram/SourceFiles/core/launcher.cpp`.

use std::path::PathBuf;
use std::time::Duration;

use egui::{CentralPanel, Frame, Vec2, ViewportBuilder, ViewportCommand};

use crate::notifications::AppNotifyTrayHost;
use crate::notifications::settings::{MuteValue, NotifySettingsStore};
use crate::stand_in::backend::Backend;
use crate::stand_in::mock_session::{MockSession, session_peers};
use crate::stand_in::settings::{ShellSettings, WindowPosition};
use crate::stand_in::strings::Strings;
use crate::stand_in::theme::{Palette, ThemeMode};
use crate::stand_in::types::{PeerId, PeerKind};
use crate::startup::StartupContext;
use crate::startup::crash_window::{CRASH_WINDOW_HEIGHT, CRASH_WINDOW_WIDTH, CrashRecoveryAction};
use crate::startup::icon::load_window_icon;
use crate::startup::ipc::IncomingActivation;
use crate::startup::updater::UpdateUiAction;
use crate::webview::AppWebViewHost;
use crate::webview::backend::{ExternalOpener, PlatformExternalOpener, WebViewRequest};
use crate::webview::platform_win::PlatformClipboard;
use crate::webview::theme::build_webview_theme_params;
use crate::window::adaptive::{WINDOW_MIN_HEIGHT, WINDOW_MIN_WIDTH};
use crate::window::controller::{RootScreen, WebViewHostRequest, WindowController};
use crate::window::platform_shell::{
    PlatformWindowShell, apply_initial_viewport_frame, extract_eframe_hwnd,
};

/// Launch configuration for [`run_app`].
#[derive(Debug, Default)]
pub struct AppConfig {
    /// Skip intro and open straight into the signed-in mock main workspace (`--start-main`).
    pub start_signed_in: bool,
    /// Use real `tg-session` + `LiveSession` instead of `MockSession` (the default when API
    /// credentials are available; `--demo` selects `MockSession`).
    pub live_mode: bool,
    /// Optional override for `CoreSettings::native_window_frame` (`false` = custom frameless bar).
    pub native_window_frame: Option<bool>,
    /// Optional override path for persisted shell settings JSON.
    pub settings_path: Option<PathBuf>,
    /// Optional live startup context (single-instance guard, crash recovery, logger, updater).
    pub startup: Option<StartupContext>,
}

/// Top-level `eframe::App` implementation hosting [`WindowController`], [`AppNotifyTrayHost`],
/// and [`AppWebViewHost`] over ONE shared [`MockSession`], or delegating to
/// [`crate::live::LiveAppHost`] when launched in live mode (`--live` / `RUSTGRAM_LIVE=1`).
pub struct TgforkApp {
    /// The demo workspace; `None` in live mode, where the live host owns the real ones.
    demo: Option<Box<DemoShell>>,
    live_host: Option<crate::live::LiveAppHost>,
    /// Window size restored when the crash recovery screen is left.
    restore_position: WindowPosition,
    startup: Option<StartupContext>,
    initial_activations_dispatched: bool,
    started_hidden_to_tray: bool,
    platform_shell: PlatformWindowShell,
}

/// The demo-mode workspace over one shared [`MockSession`] (`--demo`).
struct DemoShell {
    controller: WindowController,
    applied_theme_gen: u64,
    notify_tray: AppNotifyTrayHost,
    webview_host: AppWebViewHost,
    calls_host: crate::calls::AppCallsHost,
    stories: crate::stories::AppStoriesHost,
}

impl DemoShell {
    fn new(
        cc: &eframe::CreationContext<'_>,
        mock_session: MockSession,
        settings: ShellSettings,
        settings_path: PathBuf,
    ) -> Self {
        tg_ui::fonts::register_tdesktop_fonts(&cc.egui_ctx);

        let handle = mock_session.handle();
        let notify_store = build_notify_settings_store();
        let sim_handle = handle.clone();
        let notify_tray = AppNotifyTrayHost::with_backend(
            Box::new(handle.clone()),
            notify_store,
            Box::new(move |now_unix| {
                let _ = sim_handle.simulate_incoming_message(now_unix);
            }),
        );
        let webview_host = AppWebViewHost::with_dependencies(
            Box::new(handle),
            Box::new(PlatformExternalOpener),
            Box::new(PlatformClipboard),
        );

        let mut controller = WindowController::with_session(mock_session, settings, settings_path);
        controller.theme_manager_mut().apply_to_egui(&cc.egui_ctx);
        let applied_theme_gen = controller.theme_generation();

        Self {
            controller,
            applied_theme_gen,
            notify_tray,
            webview_host,
            calls_host: crate::calls::AppCallsHost::new(),
            stories: crate::stories::AppStoriesHost::new(),
        }
    }
}

impl TgforkApp {
    /// Creates a new `TgforkApp` wired to one shared [`MockSession`].
    #[must_use]
    pub fn with_session(
        cc: &eframe::CreationContext<'_>,
        mock_session: MockSession,
        settings: ShellSettings,
        settings_path: PathBuf,
    ) -> Self {
        let restore_position = settings.window_position;
        let demo = DemoShell::new(cc, mock_session, settings, settings_path);
        Self {
            demo: Some(Box::new(demo)),
            live_host: None,
            restore_position,
            startup: None,
            initial_activations_dispatched: false,
            started_hidden_to_tray: false,
            platform_shell: PlatformWindowShell::new_win32(),
        }
    }

    /// Creates a new `TgforkApp` running in live Telegram mode (`--live` / `RUSTGRAM_LIVE=1`)
    /// backed by [`crate::live::LiveAppHost`] + `tg-session`.
    #[must_use]
    pub fn new_live(
        cc: &eframe::CreationContext<'_>,
        settings: ShellSettings,
        settings_path: PathBuf,
        working_dir: PathBuf,
        data_name: String,
    ) -> Self {
        Self::new_live_prefetched(cc, settings, settings_path, working_dir, data_name, None)
    }

    /// Like [`Self::new_live`], continuing a [`crate::live::LiveBootstrapPrefetch`] that opened
    /// `tdata` and started the MTProto sessions while the window was being created.
    #[must_use]
    pub fn new_live_prefetched(
        cc: &eframe::CreationContext<'_>,
        settings: ShellSettings,
        settings_path: PathBuf,
        working_dir: PathBuf,
        data_name: String,
        prefetch: Option<crate::live::LiveBootstrapPrefetch>,
    ) -> Self {
        crate::startup::profiler::mark("new_live_enter");
        // Live mode builds no demo workspace: the live host owns the real controller, tray,
        // web view, calls and stories hosts, and applies the same settings-derived theme to
        // egui that the demo controller used to apply after it.
        let restore_position = settings.window_position;
        let egui_ctx = Some(cc.egui_ctx.clone());
        // Auto-night mode: the live windows of the real app follow the system dark mode.
        crate::window::controller::follow_platform_dark_mode();
        let mut live_host = match prefetch {
            Some(prefetch) => crate::live::LiveAppHost::bootstrap_prefetched(
                egui_ctx,
                settings,
                settings_path,
                working_dir,
                data_name,
                prefetch,
            ),
            None => crate::live::LiveAppHost::bootstrap(
                egui_ctx,
                settings,
                settings_path,
                working_dir,
                data_name,
            ),
        };
        crate::startup::profiler::mark("new_live_after_bootstrap");
        live_host.set_window_hwnd(crate::live::passkeys::win32_hwnd_from_window_handle(cc));
        Self {
            demo: None,
            live_host: Some(live_host),
            restore_position,
            startup: None,
            initial_activations_dispatched: false,
            started_hidden_to_tray: false,
            platform_shell: PlatformWindowShell::new_win32(),
        }
    }

    /// Attaches a live [`StartupContext`] (single-instance pipe queue, crash recovery,
    /// file logger, and background self-updater) to this app instance.
    #[must_use]
    pub fn with_startup(
        mut self,
        ctx: &egui::Context,
        startup: StartupContext,
        started_hidden_to_tray: bool,
    ) -> Self {
        startup.activation_queue.set_repaint_ctx(ctx.clone());
        let initial_snapshot = startup.update_controller.snapshot();
        let widget_helper_path = startup.plan.widget_helper_path.clone();
        if let Some(live) = self.live_host.as_mut() {
            live.set_update_snapshot(initial_snapshot.clone());
            live.set_widget_helper_path(widget_helper_path.clone());
        }
        if let Some(demo) = self.demo.as_deref_mut() {
            let _ = crate::startup::apply_profile_data_paths_native(
                &startup.plan.resolved_working_dir.path,
                &mut demo.webview_host,
                &mut demo.controller,
            );
            demo.controller.set_update_snapshot(initial_snapshot);
            demo.controller.set_widget_helper_path(widget_helper_path);
        }
        self.started_hidden_to_tray = started_hidden_to_tray;
        self.startup = Some(startup);
        self
    }

    /// Compatibility constructor accepting a legacy `Backend` and `Strings`.
    #[must_use]
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        backend: Box<dyn Backend>,
        _strings: Box<dyn Strings>,
        settings: ShellSettings,
        settings_path: PathBuf,
    ) -> Self {
        let start_signed_in = backend.current_user().is_some();
        Self::with_session(
            cc,
            MockSession::new(start_signed_in),
            settings,
            settings_path,
        )
    }

    /// Returns a mutable reference to the demo workspace's [`WindowController`] (`None` in
    /// live mode, where [`crate::live::LiveAppHost`] owns the controllers).
    pub fn controller_mut(&mut self) -> Option<&mut WindowController> {
        self.demo.as_deref_mut().map(|demo| &mut demo.controller)
    }

    /// Dark mode and palette for the crash recovery screen and the title bar.
    fn shell_theme(&self) -> (bool, Palette) {
        if let Some(live) = self.live_host.as_ref() {
            return (live.is_dark_theme(), live.current_palette());
        }
        match self.demo.as_deref() {
            Some(demo) => (
                demo.controller.theme_mode() == ThemeMode::Night,
                demo.controller.palette(),
            ),
            None => (false, Palette::day()),
        }
    }
}

impl DemoShell {
    fn sync_theme(&mut self, ctx: &egui::Context) {
        let current_gen = self.controller.theme_generation();
        if self.applied_theme_gen != current_gen {
            self.controller.theme_manager_mut().apply_to_egui(ctx);
            self.applied_theme_gen = current_gen;
        }
    }

    fn track_window_geometry(&mut self, ctx: &egui::Context) {
        let vp = ctx.input(|i| i.viewport().clone());
        let maximized = vp.maximized.unwrap_or(false);
        if let Some(inner) = vp.inner_rect {
            let pos = WindowPosition {
                x: vp.outer_rect.map(|r| r.min.x),
                y: vp.outer_rect.map(|r| r.min.y),
                width: inner.width(),
                height: inner.height(),
                maximized,
            };
            self.controller.note_window_position(pos);
        }
    }

    fn dispatch_webview_requests(&mut self) {
        let requests = self.controller.take_webview_requests();
        if requests.is_empty() {
            return;
        }
        let theme_params_json =
            build_webview_theme_params(self.controller.theme_manager().palette()).json;
        for req in requests {
            match req {
                WebViewHostRequest::OpenInstantView {
                    url,
                    webpage_id,
                    fragment,
                } => {
                    self.webview_host
                        .open_instant_view(&url, webpage_id, fragment.as_deref());
                }
                WebViewHostRequest::OpenMiniApp {
                    bot_id,
                    start_param,
                } => {
                    let bot_u64 = bot_id.max(0) as u64;
                    let url_override = self
                        .controller
                        .session_backend_mut()
                        .take_pending_mini_app_url(PeerId::from_user(bot_u64));
                    self.webview_host.open_mini_app(WebViewRequest {
                        bot_id: bot_u64,
                        start_param,
                        url_override,
                        fullscreen: false,
                        theme_params_json: theme_params_json.clone(),
                    });
                }
                WebViewHostRequest::OpenExternalUrl(url) => {
                    let _ = PlatformExternalOpener.open_url(&url);
                }
            }
        }
    }
}

impl TgforkApp {
    fn process_startup_hooks(&mut self, ctx: &egui::Context) {
        let mut incoming_batch = Vec::new();
        let mut open_settings_now = false;
        let mut latest_update_snapshot = None;

        if let Some(startup) = self.startup.as_ref() {
            if !self.initial_activations_dispatched {
                self.initial_activations_dispatched = true;
                open_settings_now = startup.plan.open_settings_on_start;
                if !startup.plan.initial_urls.is_empty()
                    || !startup.plan.initial_send_paths.is_empty()
                {
                    incoming_batch.push(IncomingActivation {
                        urls: startup.plan.initial_urls.clone(),
                        send_paths: startup.plan.initial_send_paths.clone(),
                        show: false,
                        quit: false,
                    });
                }
            }

            incoming_batch.extend(startup.activation_queue.drain());
            latest_update_snapshot = Some(startup.update_controller.snapshot());
        }

        if let Some(snap) = latest_update_snapshot {
            if let Some(live) = self.live_host.as_mut() {
                live.set_update_snapshot(snap);
            } else if let Some(demo) = self.demo.as_deref_mut() {
                demo.controller.set_update_snapshot(snap);
            }
        }
        let update_prefs_change = if let Some(live) = self.live_host.as_mut() {
            live.take_update_prefs_change()
        } else {
            self.demo
                .as_deref_mut()
                .and_then(|demo| demo.controller.take_update_prefs_change())
        };
        if let Some((auto_update, install_beta)) = update_prefs_change
            && let Some(startup) = self.startup.as_ref()
            && let Err(err) = crate::settings::advanced::propagate_update_preferences(
                Some(&startup.plan.resolved_working_dir.path),
                auto_update,
                install_beta,
                Some(&startup.update_controller),
            )
        {
            tracing::warn!(%err, "failed to persist update preferences");
        }
        if open_settings_now {
            if let Some(live) = self.live_host.as_mut() {
                live.open_settings();
            } else if let Some(demo) = self.demo.as_deref_mut() {
                demo.controller.open_settings();
            }
        }

        for act in incoming_batch {
            if act.quit {
                ctx.send_viewport_cmd(ViewportCommand::Close);
                return;
            }
            if act.show {
                self.started_hidden_to_tray = false;
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            }
            if let Some(live) = self.live_host.as_mut() {
                live.handle_incoming_activation(act, ctx);
            } else if let Some(demo) = self.demo.as_deref_mut() {
                demo.controller.handle_incoming_activation(act, ctx);
            }
        }

        let update_action = if let Some(live) = self.live_host.as_mut() {
            live.take_update_action()
        } else {
            self.demo
                .as_deref_mut()
                .and_then(|demo| demo.controller.take_update_action())
        };
        if let Some(action) = update_action {
            match action {
                UpdateUiAction::CheckNow => {
                    if let Some(startup) = self.startup.as_ref() {
                        startup.update_controller.request_check_now();
                    }
                }
                UpdateUiAction::RestartToUpdate => {
                    let launch_res = if let Some(startup) = self.startup.as_ref() {
                        startup.log_manager.close_main();
                        startup.crash_manager.finish();
                        match startup.update_controller.launch_updater_for_restart() {
                            Ok(_) => Ok(()),
                            Err(err) => {
                                let _ = startup.crash_manager.restart();
                                Err(err)
                            }
                        }
                    } else {
                        Err("updater is not initialized".to_owned())
                    };
                    match launch_res {
                        Ok(()) => ctx.send_viewport_cmd(ViewportCommand::Close),
                        Err(err) => {
                            let msg = format!("Could not launch updater: {err}");
                            if let Some(live) = self.live_host.as_mut() {
                                live.show_toast(msg);
                            } else if let Some(demo) = self.demo.as_deref_mut() {
                                demo.controller.show_toast(msg);
                            }
                        }
                    }
                }
            }
        }

        // While hidden to tray on startup, wake periodically so tray icon clicks are polled promptly.
        if self.started_hidden_to_tray {
            let visible = ctx.input(|i| i.viewport().minimized != Some(true));
            if !visible {
                ctx.request_repaint_after(Duration::from_millis(250));
            }
        }
    }
}

impl DemoShell {
    /// First half of a demo frame, up to the startup hooks: window frame, title bar, geometry.
    /// Returns `(dark_mode, passcode_locked)`.
    fn begin_frame(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        hwnd: isize,
        has_crash_recovery: bool,
        platform_shell: &mut PlatformWindowShell,
    ) -> (bool, bool) {
        self.controller.set_window_hwnd(hwnd);
        self.controller.set_native_hwnd(hwnd);
        let native_frame = self.controller.sync_native_window_frame_setting();
        let dark_mode = self.controller.theme_mode() == ThemeMode::Night;
        let passcode_locked = self.controller.root_screen() == RootScreen::PasscodeLock;
        platform_shell.sync_window_frame(
            Some(ctx),
            hwnd,
            dark_mode,
            native_frame,
            has_crash_recovery,
            passcode_locked,
        );
        let title_palette = self.controller.palette();
        platform_shell.show_frameless_title_bar(
            ui,
            "Rustgram",
            &title_palette,
            native_frame,
            has_crash_recovery,
        );

        self.track_window_geometry(ctx);
        (dark_mode, passcode_locked)
    }

    /// Second half of a demo frame: calls, the workspace, overlays, tray and web views.
    fn finish_frame(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        hwnd: isize,
        dark_mode: bool,
        passcode_locked: bool,
        platform_shell: &mut PlatformWindowShell,
    ) {
        self.calls_host.set_passcode_locked(passcode_locked);
        self.calls_host.update_and_render(ui);
        CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
            self.controller
                .show_with_stories(ui, Some(&mut self.stories));
        });
        self.calls_host
            .set_passcode_locked(self.controller.root_screen() == RootScreen::PasscodeLock);
        let dark_taskbar = tg_platform::system_info::is_dark_taskbar().unwrap_or(dark_mode);
        let audio_bar = self.controller.main_widget_mut().audio_player_bar_mut();
        let _ = platform_shell.poll_and_dispatch_taskbar_clicks(audio_bar);
        platform_shell.sync_taskbar_from_audio_bar(hwnd, audio_bar, dark_taskbar);
        if self.controller.take_open_recent_calls_request() {
            self.calls_host.open_recent_calls();
        }
        if let Some((peer_id, video)) = self.controller.take_start_call_request() {
            self.calls_host.start_outgoing_call(peer_id, video);
        }
        self.controller.dispatch_demo_overlay_requests(ctx);

        self.dispatch_webview_requests();
        let locked = self.controller.root_screen() == RootScreen::PasscodeLock;
        self.webview_host.set_passcode_locked(locked);
        if locked {
            close_demo_overlays_for_passcode_lock(ctx);
            self.stories.close_for_passcode_lock();
        }

        self.notify_tray
            .set_active_peer(self.controller.active_focused_peer());
        self.notify_tray.update_and_render(
            ctx,
            &self.controller.palette(),
            self.controller.root_screen() == RootScreen::PasscodeLock,
        );
        self.controller.poll_notification_opens(Some(ctx));

        self.webview_host
            .update_and_render(ctx, self.controller.theme_mode());
        for ev in self.webview_host.take_events() {
            self.controller.handle_webview_event(ev, ctx);
        }
        if locked {
            return;
        }
        crate::statistics::AppStatisticsHost::update_and_render(
            ctx,
            &self.controller.palette(),
            self.controller.theme_mode(),
        );
        crate::export::AppExportHost::update_and_render(
            ctx,
            &self.controller.palette(),
            self.controller.theme_mode(),
        );
        crate::payments::AppPaymentsHost::update_and_render(
            ctx,
            &self.controller.palette(),
            self.controller.theme_mode(),
        );
        crate::wallet::AppWalletHost::update_and_render(
            ctx,
            &self.controller.palette(),
            self.controller.theme_mode(),
        );
        crate::business::AppBusinessHost::update_and_render(
            ctx,
            &self.controller.palette(),
            self.controller.theme_mode(),
        );
        crate::bots::AppBotsHost::update_and_render(
            ctx,
            &self.controller.palette(),
            self.controller.theme_mode(),
        );
        crate::themes::AppThemesHost::update_and_render(
            ctx,
            self.controller.palette().layer_backdrop,
            self.controller.palette().divider,
        );
        self.stories
            .update_and_render(ctx, &self.controller.palette());
        if self.stories.take_update_app_request() {
            self.controller.request_update_check();
        }
    }
}

impl eframe::App for TgforkApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let _first_frame_marks = crate::startup::profiler::FrameMarks::begin();
        let _frame_trace =
            crate::startup::frame_trace::begin_frame(ui.ctx(), frame.info().cpu_usage);
        let ctx = ui.ctx().clone();
        // tdesktop's device pixel ratio and interface scale, `-scale` included (TGB-3040).
        let config_scale = self
            .startup
            .as_ref()
            .map_or(tg_core::SCALE_AUTO, |s| s.plan.global_config.config_scale);
        tg_app_window::window::interface_scale::apply(&ctx, config_scale);
        let hwnd = extract_eframe_hwnd(frame);
        let has_crash_recovery = self
            .startup
            .as_ref()
            .is_some_and(|s| s.crash_recovery.is_some());
        if let Some(demo) = self.demo.as_deref_mut() {
            demo.sync_theme(&ctx);
        }

        // If the previous launch exited uncleanly (`tdata/working`), show the crash recovery
        // screen before entering the main workspace (`LastCrashedWindow`).
        if self
            .startup
            .as_ref()
            .is_some_and(|s| s.crash_recovery.is_some())
        {
            let (dark_mode, palette) = self.shell_theme();
            self.platform_shell
                .sync_window_frame(Some(&ctx), hwnd, dark_mode, true, true, false);
            let Some(startup) = self.startup.as_mut() else {
                return;
            };
            let Some(recovery) = startup.crash_recovery.as_mut() else {
                return;
            };
            let decision = CentralPanel::default()
                .frame(Frame::NONE)
                .show(ui, |ui| recovery.show(ui, &palette))
                .inner;
            if let Some(action) = decision {
                match action {
                    CrashRecoveryAction::Continue => {
                        let _ = startup.crash_manager.restart();
                        startup.crash_recovery = None;
                        let pos = self.restore_position.clamped();
                        ctx.send_viewport_cmd(ViewportCommand::Title("Rustgram".to_owned()));
                        ctx.send_viewport_cmd(ViewportCommand::InnerSize(Vec2::new(
                            pos.width, pos.height,
                        )));
                        if pos.maximized {
                            ctx.send_viewport_cmd(ViewportCommand::Maximized(true));
                        }
                        ctx.request_repaint();
                    }
                    CrashRecoveryAction::Quit => {
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }
                }
            }
            return;
        }

        if self.live_host.is_some() {
            self.process_startup_hooks(&ctx);
            if let Some(live) = self.live_host.as_mut() {
                if hwnd != 0 {
                    live.set_window_hwnd(hwnd as usize);
                }
                let native_frame = live.native_window_frame();
                let dark_mode = live.is_dark_theme();
                let passcode_locked = live.is_passcode_locked();
                let palette = live.current_palette();
                self.platform_shell.sync_window_frame(
                    Some(&ctx),
                    hwnd,
                    dark_mode,
                    native_frame,
                    has_crash_recovery,
                    passcode_locked,
                );
                self.platform_shell.show_frameless_title_bar(
                    ui,
                    "Rustgram",
                    &palette,
                    native_frame,
                    has_crash_recovery,
                );
                let vp = ctx.input(|i| i.viewport().clone());
                if let Some(inner) = vp.inner_rect {
                    live.note_window_position(WindowPosition {
                        x: vp.outer_rect.map(|r| r.min.x),
                        y: vp.outer_rect.map(|r| r.min.y),
                        width: inner.width(),
                        height: inner.height(),
                        maximized: vp.maximized.unwrap_or(false),
                    });
                }
                live.set_native_hwnd(hwnd);
                live.show(&ctx, ui);
                if let Some(audio_bar) = live.active_audio_player_bar_mut() {
                    let _ = self
                        .platform_shell
                        .poll_and_dispatch_taskbar_clicks(audio_bar);
                    let dark_taskbar =
                        tg_platform::system_info::is_dark_taskbar().unwrap_or(dark_mode);
                    self.platform_shell
                        .sync_taskbar_from_audio_bar(hwnd, audio_bar, dark_taskbar);
                }
            }
            return;
        }

        let Some(demo) = self.demo.as_deref_mut() else {
            return;
        };
        let (dark_mode, passcode_locked) =
            demo.begin_frame(ui, &ctx, hwnd, has_crash_recovery, &mut self.platform_shell);
        self.process_startup_hooks(&ctx);
        if let Some(demo) = self.demo.as_deref_mut() {
            demo.finish_frame(
                ui,
                &ctx,
                hwnd,
                dark_mode,
                passcode_locked,
                &mut self.platform_shell,
            );
        }
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        crate::startup::frame_trace::raw_input_hook(ctx, raw_input);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        crate::startup::frame_trace::shutdown();
        if let Some(live) = self.live_host.as_mut() {
            live.on_exit();
        } else if let Some(demo) = self.demo.as_deref_mut() {
            demo.calls_host.on_exit();
            demo.webview_host.on_exit();
            demo.notify_tray.on_exit();
            demo.controller.flush_settings();
        }

        if let Some(mut startup) = self.startup.take() {
            startup.update_controller.shutdown();
            // Only remove `tdata/working` if we actually passed crash recovery into normal execution.
            if startup.crash_recovery.is_none() {
                startup.crash_manager.finish();
            }
            startup.log_manager.finish();
            drop(startup.single_instance_guard);
        }
    }
}

/// Closes every demo overlay that could show account content over the passcode lock
/// (`Application::closeAdditionalWindows`, `application.cpp`): Statistics, Business, Theme
/// Studio, Payments and the wallet.
pub fn close_demo_overlays_for_passcode_lock(ctx: &egui::Context) {
    crate::statistics::AppStatisticsHost::close(ctx);
    crate::business::AppBusinessHost::close(ctx);
    crate::themes::AppThemesHost::close(ctx);
    crate::payments::AppPaymentsHost::close(ctx);
    crate::wallet::AppWalletHost::close(ctx);
}

fn build_notify_settings_store() -> NotifySettingsStore {
    let mut store = NotifySettingsStore::default();
    store.register_peer_kind(session_peers::SELF, PeerKind::User);
    store.register_peer_kind(session_peers::USER_MIRA, PeerKind::User);
    store.register_peer_kind(session_peers::USER_THEO, PeerKind::User);
    store.register_peer_kind(session_peers::USER_SAM, PeerKind::User);
    store.register_peer_kind(session_peers::USER_RILEY, PeerKind::User);
    store.register_peer_kind(session_peers::BOT_CATALOG, PeerKind::User);
    store.register_peer_kind(session_peers::GROUP_CORE, PeerKind::Group);
    store.register_peer_kind(session_peers::GROUP_UI_LAB, PeerKind::Group);
    store.register_peer_kind(session_peers::CHANNEL_DEV_LOG, PeerKind::Channel);
    store.register_peer_kind(session_peers::CHANNEL_RELEASES, PeerKind::Channel);
    store.register_peer_kind(session_peers::CHANNEL_ARCHIVE, PeerKind::Channel);
    store.update_peer(
        session_peers::CHANNEL_RELEASES,
        MuteValue::forever(),
        None,
        None,
        None,
        None,
        0,
    );
    store.update_peer(
        session_peers::CHANNEL_ARCHIVE,
        MuteValue::forever(),
        None,
        None,
        None,
        None,
        0,
    );
    store
}

/// Live-mode working directory and `tdata` key (`-key`, default `data`).
fn live_working_dir_and_key(startup: Option<&StartupContext>) -> (PathBuf, String) {
    startup
        .map(|s| {
            let key = if s.plan.parsed_args.key.is_empty() {
                "data".to_owned()
            } else {
                s.plan.parsed_args.key.clone()
            };
            (s.plan.resolved_working_dir.path.clone(), key)
        })
        .unwrap_or_else(|| {
            (
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                "data".to_owned(),
            )
        })
}

/// Launches the native Windows desktop window titled `"Rustgram"`.
pub fn run_app(mut config: AppConfig) -> anyhow::Result<()> {
    let startup = config.startup.take();
    let settings_path = config
        .settings_path
        .unwrap_or_else(ShellSettings::default_path);
    let mut settings = ShellSettings::load_from(&settings_path);
    crate::startup::profiler::mark("settings_load");
    if let Some(override_frame) = config.native_window_frame {
        settings.native_window_frame = override_frame;
    }
    let pos = settings.window_position.clamped();

    let has_crash_recovery = startup.as_ref().is_some_and(|s| s.crash_recovery.is_some());
    let start_in_tray = !has_crash_recovery
        && startup
            .as_ref()
            .is_some_and(|s| s.plan.start_minimized_to_tray);

    let initial_native_frame = config
        .native_window_frame
        .unwrap_or(settings.native_window_frame);
    let icon = load_window_icon();
    crate::startup::profiler::mark("load_window_icon");
    let mut viewport = apply_initial_viewport_frame(
        ViewportBuilder::default()
            .with_icon(icon)
            .with_min_inner_size(Vec2::new(WINDOW_MIN_WIDTH as f32, WINDOW_MIN_HEIGHT as f32)),
        initial_native_frame,
        has_crash_recovery,
    );

    if has_crash_recovery {
        viewport = viewport
            .with_title("Rustgram — Crash Recovery")
            .with_inner_size(Vec2::new(CRASH_WINDOW_WIDTH, CRASH_WINDOW_HEIGHT))
            .with_maximized(false);
    } else {
        viewport = viewport
            .with_title("Rustgram")
            .with_inner_size(Vec2::new(pos.width, pos.height))
            .with_maximized(pos.maximized);
        if let (Some(x), Some(y)) = (pos.x, pos.y) {
            viewport = viewport.with_position(egui::Pos2::new(x, y));
        }
        if start_in_tray {
            viewport = viewport.with_visible(false);
        }
    }

    let mut native_options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    // eframe's default features also compile in wgpu, and with both renderers available eframe
    // picks wgpu, which here cost ~370 MiB of private bytes (driver probing of every adapter and
    // pre-allocated memory blocks) and ~0.5 s before the first frame. `RUSTGRAM_RENDERER=wgpu`
    // switches back, e.g. for a machine without a usable OpenGL driver.
    if std::env::var("RUSTGRAM_RENDERER").is_ok_and(|v| v.trim().eq_ignore_ascii_case("wgpu")) {
        native_options.renderer = eframe::Renderer::Wgpu;
    }

    let start_signed_in = config.start_signed_in;
    let live_mode = config.live_mode;
    let live_target = live_mode.then(|| live_working_dir_and_key(startup.as_ref()));
    // Opening `tdata` and starting the MTProto sessions does not need the window, so it runs
    // while eframe creates the window and the GL context.
    let live_prefetch = live_target.as_ref().and_then(|(working_dir, data_name)| {
        crate::live::LiveBootstrapPrefetch::spawn(working_dir.clone(), data_name.clone())
    });
    crate::startup::profiler::mark("before_eframe_run_native");
    eframe::run_native(
        "Rustgram",
        native_options,
        Box::new(move |cc| {
            crate::startup::profiler::mark("creator_cb_enter");
            let mut app = if let Some((working_dir, data_name)) = live_target {
                TgforkApp::new_live_prefetched(
                    cc,
                    settings,
                    settings_path,
                    working_dir,
                    data_name,
                    live_prefetch,
                )
            } else {
                let mock_session = MockSession::new(start_signed_in);
                TgforkApp::with_session(cc, mock_session, settings, settings_path)
            };
            if let Some(startup_ctx) = startup {
                app = app.with_startup(&cc.egui_ctx, startup_ctx, start_in_tray);
            }
            crate::startup::profiler::mark("creator_cb_return");
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyhow::anyhow!("eframe failed to launch Rustgram window: {err}"))
}
