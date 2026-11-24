//! Automated 1440x900 desktop screenshot capture for the unified Rustgram application
//! (`crates/tg-app/docs/app_*.png`).
//!
//! Run with:
//! ```sh
//! cargo test -p tg-app capture_app_screenshots -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use egui::{CentralPanel, Frame, Vec2, ViewportBuilder};

use crate::mock_session::{MockSession, session_peers};
use crate::window::controller::WindowController;
use crate::window::section::{SectionShow, SettingsPage};
use tg_app_core::stand_in::settings::ShellSettings;
use tg_app_core::stand_in::types::MsgId;

#[derive(Debug, Clone, Copy)]
enum AppShot {
    MainChat,
    ProfileThirdColumn,
    MediaViewer,
    Settings,
    EmojiPanel,
}

impl AppShot {
    const ALL: [Self; 5] = [
        Self::MainChat,
        Self::ProfileThirdColumn,
        Self::MediaViewer,
        Self::Settings,
        Self::EmojiPanel,
    ];

    const fn file_name(self) -> &'static str {
        match self {
            Self::MainChat => "app_main.png",
            Self::ProfileThirdColumn => "app_profile.png",
            Self::MediaViewer => "app_media_viewer.png",
            Self::Settings => "app_settings.png",
            Self::EmojiPanel => "app_emoji_panel.png",
        }
    }
}

struct AppScreenshotRunner {
    docs_dir: PathBuf,
    index: usize,
    frames_in_step: u32,
    requested: bool,
    done: Arc<AtomicBool>,
    controller: WindowController,
}

impl AppScreenshotRunner {
    fn new(cc: &eframe::CreationContext<'_>, docs_dir: PathBuf) -> Self {
        tg_ui::fonts::register_tdesktop_fonts(&cc.egui_ctx);
        let settings = ShellSettings {
            folders_sidebar_enabled: true,
            ..ShellSettings::default()
        };
        let mut controller = WindowController::with_session(
            MockSession::new(true),
            settings,
            std::env::temp_dir().join("rustgram_app_screenshots_settings.json"),
        );
        controller.theme_manager_mut().apply_to_egui(&cc.egui_ctx);

        let mut runner = Self {
            docs_dir,
            index: 0,
            frames_in_step: 0,
            requested: false,
            done: Arc::new(AtomicBool::new(false)),
            controller,
        };
        runner.prepare_step(AppShot::ALL[0]);
        runner
    }

    fn prepare_step(&mut self, shot: AppShot) {
        self.controller.media_overlay_mut().close();
        self.controller
            .main_widget_mut()
            .tabbed_panel_mut()
            .hide_fast();

        if let Some(session) = self.controller.session_mut() {
            session.update_layout(1440);
            session.close_third_section();
        }

        match shot {
            AppShot::MainChat => {
                if let Some(session) = self.controller.session_mut() {
                    session.show_peer_history(
                        session_peers::GROUP_CORE,
                        SectionShow::instant_clear_stack(),
                        MsgId::SHOW_AT_END,
                    );
                }
            }
            AppShot::ProfileThirdColumn => {
                if let Some(session) = self.controller.session_mut() {
                    session.show_peer_history(
                        session_peers::CHANNEL_DEV_LOG,
                        SectionShow::instant_clear_stack(),
                        MsgId::SHOW_AT_END,
                    );
                    session.show_peer_info(
                        session_peers::CHANNEL_DEV_LOG,
                        SectionShow::forward().with_animated(false),
                    );
                }
            }
            AppShot::MediaViewer => {
                if let Some(session) = self.controller.session_mut() {
                    session.show_peer_history(
                        session_peers::GROUP_CORE,
                        SectionShow::instant_clear_stack(),
                        MsgId::SHOW_AT_END,
                    );
                }
                self.controller
                    .open_history_media(session_peers::GROUP_CORE, MsgId(205), None);
            }
            AppShot::Settings => {
                if let Some(session) = self.controller.session_mut() {
                    session.show_peer_history(
                        session_peers::GROUP_CORE,
                        SectionShow::instant_clear_stack(),
                        MsgId::SHOW_AT_END,
                    );
                    session.show_settings(SettingsPage::Main);
                    session.finish_slide_animation();
                }
            }
            AppShot::EmojiPanel => {
                if let Some(session) = self.controller.session_mut() {
                    session.show_peer_history(
                        session_peers::GROUP_CORE,
                        SectionShow::instant_clear_stack(),
                        MsgId::SHOW_AT_END,
                    );
                }
                self.controller
                    .main_widget_mut()
                    .tabbed_panel_mut()
                    .show_immediate();
            }
        }

        self.frames_in_step = 0;
        self.requested = false;
        self.done.store(false, Ordering::Release);
    }
}

impl eframe::App for AppScreenshotRunner {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
            self.controller.show(ui);
        });

        if self.done.load(Ordering::Acquire) {
            self.index += 1;
            if self.index >= AppShot::ALL.len() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            self.prepare_step(AppShot::ALL[self.index]);
            ctx.request_repaint();
            return;
        }

        self.frames_in_step += 1;
        if !self.requested && self.frames_in_step >= 10 {
            self.requested = true;
            let path = self.docs_dir.join(AppShot::ALL[self.index].file_name());
            let done = Arc::clone(&self.done);
            let ctx_cb = ctx.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                egui::ScreenshotCallback::new(move |img| {
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let [w, h] = img.size;
                    let mut raw = Vec::with_capacity(w * h * 4);
                    for px in &img.pixels {
                        let [r, g, b, a] = px.to_srgba_unmultiplied();
                        raw.extend_from_slice(&[r, g, b, a]);
                    }
                    if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, raw) {
                        let _ = buf.save(&path);
                    }
                    done.store(true, Ordering::Release);
                    ctx_cb.request_repaint();
                }),
            ));
        } else {
            ctx.request_repaint();
        }
    }
}

#[test]
#[ignore = "Launches a 1440x900 native eframe window to capture docs/app_*.png screenshots"]
fn capture_app_screenshots() {
    let docs_dir = tg_app_core::core_types::paths::workspace_app_docs_dir();
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title("Rustgram")
            .with_inner_size(Vec2::new(1440.0, 900.0))
            .with_min_inner_size(Vec2::new(1440.0, 900.0))
            .with_decorations(false),
        event_loop_builder: Some(Box::new(|builder| {
            #[cfg(windows)]
            {
                use winit::platform::windows::EventLoopBuilderExtWindows;
                builder.with_any_thread(true);
            }
            #[cfg(not(windows))]
            {
                let _ = builder;
            }
        })),
        ..Default::default()
    };
    eframe::run_native(
        "Rustgram App Screenshots",
        options,
        Box::new(move |cc| Ok(Box::new(AppScreenshotRunner::new(cc, docs_dir)))),
    )
    .expect("eframe app screenshot runner succeeded");
}
