//! Automated screenshot capture for the 5 Intro login steps (`intro_*.png`).
//!
//! Run with:
//! ```sh
//! cargo test -p tg-app capture_intro_screenshots -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

use egui::{CentralPanel, Frame, Vec2, ViewportBuilder};

use crate::intro::IntroWidget;
use crate::intro::fake_invoker::{TEST_CODE_2FA, TEST_PHONE_2FA, TEST_PHONE_APP};
use tg_app_core::stand_in::backend::MockBackend;
use tg_app_core::stand_in::strings::EnglishStrings;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};

#[derive(Debug, Clone, Copy)]
enum ShotKind {
    Start,
    Phone,
    Code,
    Password,
    Qr,
}

impl ShotKind {
    const ALL: [Self; 5] = [
        Self::Start,
        Self::Phone,
        Self::Code,
        Self::Password,
        Self::Qr,
    ];

    const fn file_name(self) -> &'static str {
        match self {
            Self::Start => "intro_start.png",
            Self::Phone => "intro_phone.png",
            Self::Code => "intro_code.png",
            Self::Password => "intro_password.png",
            Self::Qr => "intro_qr.png",
        }
    }
}

struct IntroScreenshotApp {
    docs_dir: PathBuf,
    index: usize,
    frames_in_step: u32,
    requested: bool,
    done: Arc<AtomicBool>,
    intro: IntroWidget,
    backend: MockBackend,
    strings: EnglishStrings,
    palette: Palette,
}

impl IntroScreenshotApp {
    fn new(cc: &eframe::CreationContext<'_>, docs_dir: PathBuf) -> Self {
        let palette = Palette::for_mode(ThemeMode::Day);
        cc.egui_ctx
            .set_visuals(palette.to_egui_visuals(ThemeMode::Day));
        let mut app = Self {
            docs_dir,
            index: 0,
            frames_in_step: 0,
            requested: false,
            done: Arc::new(AtomicBool::new(false)),
            intro: IntroWidget::new(),
            backend: MockBackend::new(false),
            strings: EnglishStrings,
            palette,
        };
        app.prepare_step(ShotKind::ALL[0]);
        app
    }

    fn prepare_step(&mut self, shot: ShotKind) {
        let now = Instant::now();
        self.intro = IntroWidget::new();
        match shot {
            ShotKind::Start => {
                self.intro.fetch_nearest_dc();
            }
            ShotKind::Phone => {
                self.intro.switch_to_phone();
                self.intro.phone.select_country_iso2("US");
                self.intro.phone.on_phone_edited("5550101234");
            }
            ShotKind::Code => {
                self.intro.switch_to_phone();
                let _ = self.intro.submit_phone(&format!("+{TEST_PHONE_APP}"), now);
                let _ = self.intro.resend_code(now);
                let _ = self.intro.code.input.set_code("123");
            }
            ShotKind::Password => {
                self.intro.switch_to_phone();
                let _ = self.intro.submit_phone(&format!("+{TEST_PHONE_2FA}"), now);
                let _ = self.intro.submit_code(TEST_CODE_2FA, now);
                self.intro.password.password_input = "rustgram-2fa".to_owned();
                self.intro.password.reveal_reset_button();
            }
            ShotKind::Qr => {
                let _ = self.intro.start_qr_flow(now);
            }
        }
        // Clear slide/cover transition so the screenshot captures the settled frame cleanly.
        self.intro.slide_animation = None;
        self.intro.cover_transition = None;
        self.frames_in_step = 0;
        self.requested = false;
        self.done.store(false, Ordering::Release);
    }
}

impl eframe::App for IntroScreenshotApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
            let _ = self
                .intro
                .show(ui, &mut self.backend, &self.palette, &self.strings);
        });

        if self.done.load(Ordering::Acquire) {
            self.index += 1;
            if self.index >= ShotKind::ALL.len() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            self.prepare_step(ShotKind::ALL[self.index]);
            ctx.request_repaint();
            return;
        }

        self.frames_in_step += 1;
        if !self.requested && self.frames_in_step >= 6 {
            self.requested = true;
            let path = self.docs_dir.join(ShotKind::ALL[self.index].file_name());
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
#[ignore = "Launches a native eframe window to capture docs/intro_*.png screenshots"]
fn capture_intro_screenshots() {
    let docs_dir = tg_app_core::core_types::paths::workspace_app_docs_dir();
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title("Rustgram")
            .with_inner_size(Vec2::new(800.0, 600.0))
            .with_min_inner_size(Vec2::new(480.0, 480.0)),
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
        "Rustgram Intro Screenshots",
        options,
        Box::new(move |cc| Ok(Box::new(IntroScreenshotApp::new(cc, docs_dir)))),
    )
    .expect("eframe screenshot runner succeeded");
}
