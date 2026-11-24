//! Automated Day-theme screenshot harness capturing the 4 required section & box screenshots:
//! - `crates/tg-app/docs/sections_forum_topics.png`
//! - `crates/tg-app/docs/sections_comments_thread.png`
//! - `crates/tg-app/docs/sections_poll_box.png`
//! - `crates/tg-app/docs/sections_schedule_box.png`
//!
//! Run manually with:
//! `cargo test -p tg-app capture_sections_screenshots -- --ignored --nocapture`

use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use egui::{CentralPanel, Frame, Vec2, ViewportBuilder};

use crate::history::HistoryWidget;
use crate::history::compose_boxes::{CreatePollBoxState, ScheduleBoxMode, ScheduleBoxState};
use crate::history::mock::MockHistoryStore;
use crate::history::mock_ext::{
    DEMO_CHANNEL_PEER_ID, DEMO_CHANNEL_POST_ID, DEMO_FORUM_PEER_ID, EXT_BASE_UNIX,
};
use crate::history::sections_ui::HistorySectionKind;
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::strings::EnglishStrings;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::PeerId;

#[derive(Clone, Copy)]
enum CaptureScene {
    ForumTopics,
    CommentsThread,
    PollBox,
    ScheduleBox,
}

impl CaptureScene {
    fn file_name(self) -> &'static str {
        match self {
            Self::ForumTopics => "sections_forum_topics.png",
            Self::CommentsThread => "sections_comments_thread.png",
            Self::PollBox => "sections_poll_box.png",
            Self::ScheduleBox => "sections_schedule_box.png",
        }
    }

    fn window_size(self) -> Vec2 {
        match self {
            Self::ForumTopics => Vec2::new(720.0, 600.0),
            Self::CommentsThread => Vec2::new(720.0, 740.0),
            Self::PollBox => Vec2::new(660.0, 760.0),
            Self::ScheduleBox => Vec2::new(620.0, 620.0),
        }
    }
}

struct SectionsScreenshotApp {
    scene: CaptureScene,
    widget: HistoryWidget,
    store: MockHistoryStore,
    poll_box: CreatePollBoxState,
    schedule_box: ScheduleBoxState,
    strings: EnglishStrings,
    palette: Palette,
    output_path: PathBuf,
    frames: u32,
    requested: bool,
    done: Arc<AtomicBool>,
}

impl SectionsScreenshotApp {
    fn new(cc: &eframe::CreationContext<'_>, scene: CaptureScene, output_path: PathBuf) -> Self {
        let palette = Palette::for_mode(ThemeMode::Day);
        cc.egui_ctx
            .set_visuals(palette.to_egui_visuals(ThemeMode::Day));

        let store = MockHistoryStore::new();
        let mut widget = HistoryWidget::new();
        match scene {
            CaptureScene::ForumTopics => {
                widget.open_section(HistorySectionKind::ForumTopics {
                    peer_id: DEMO_FORUM_PEER_ID,
                });
            }
            CaptureScene::CommentsThread => {
                widget.open_section(HistorySectionKind::Replies {
                    peer_id: DEMO_CHANNEL_PEER_ID,
                    root_msg_id: DEMO_CHANNEL_POST_ID,
                });
            }
            CaptureScene::PollBox | CaptureScene::ScheduleBox => {}
        }

        // Rich Quiz Poll showcase state
        let mut poll_box = CreatePollBoxState::new();
        poll_box.question =
            "Which crate powers animated stickers & animated reactions in Rustgram?".to_owned();
        poll_box.options = vec![
            "tg-lottie (tiny-skia software rasterizer)".to_owned(),
            "tg-audio (48 kHz Opus voice capture)".to_owned(),
            "tg-platform (Windows ISpellChecker)".to_owned(),
            "tg-editor (photo crop & brush)".to_owned(),
        ];
        poll_box.set_quiz_mode(true);
        poll_box.correct_option = Some(0);
        poll_box.explanation =
            "tg-lottie parses .tgs / Lottie JSON animations and rasterizes frames via tiny-skia."
                .to_owned();

        // Schedule Message box showcase state with "Send when online"
        let mut schedule_box = ScheduleBoxState::new(
            ScheduleBoxMode::ScheduleMessage,
            EXT_BASE_UNIX,
            true,
            "Jordan Patel",
        );
        schedule_box.selected.day = 25;
        schedule_box.selected.hour = 14;
        schedule_box.selected.minute = 30;

        Self {
            scene,
            widget,
            store,
            poll_box,
            schedule_box,
            strings: EnglishStrings,
            palette,
            output_path,
            frames: 0,
            requested: false,
            done: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl eframe::App for SectionsScreenshotApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let colors = HistoryColors::from_palette(&self.palette);
        CentralPanel::default()
            .frame(Frame::NONE)
            .show(ui, |ui| match self.scene {
                CaptureScene::ForumTopics => {
                    let _ = self.widget.show_with_backend(
                        ui,
                        DEMO_FORUM_PEER_ID,
                        &mut self.store,
                        true,
                        false,
                        false,
                        &self.palette,
                        &self.strings,
                    );
                }
                CaptureScene::CommentsThread => {
                    let _ = self.widget.show_with_backend(
                        ui,
                        DEMO_CHANNEL_PEER_ID,
                        &mut self.store,
                        true,
                        false,
                        false,
                        &self.palette,
                        &self.strings,
                    );
                }
                CaptureScene::PollBox => {
                    let _ = self.widget.show_with_backend(
                        ui,
                        DEMO_FORUM_PEER_ID,
                        &mut self.store,
                        true,
                        false,
                        false,
                        &self.palette,
                        &self.strings,
                    );
                    let _ = self.poll_box.show(ui, &colors, false);
                }
                CaptureScene::ScheduleBox => {
                    let _ = self.widget.show_with_backend(
                        ui,
                        DEMO_FORUM_PEER_ID,
                        &mut self.store,
                        true,
                        false,
                        false,
                        &self.palette,
                        &self.strings,
                    );
                    let _ = self.schedule_box.show(ui, &colors, false);
                }
            });

        self.frames += 1;
        if self.done.load(Ordering::Acquire) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if !self.requested && self.frames >= 10 {
            self.requested = true;
            let path = self.output_path.clone();
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

fn run_capture(scene: CaptureScene) {
    let output_path =
        tg_app_core::core_types::paths::workspace_app_docs_dir().join(scene.file_name());
    let size = scene.window_size();
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title(format!("Rustgram — {}", scene.file_name()))
            .with_inner_size(size),
        #[cfg(target_os = "windows")]
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::windows::EventLoopBuilderExtWindows;
            builder.with_any_thread(true);
        })),
        ..Default::default()
    };
    let out_clone = output_path.clone();
    eframe::run_native(
        "Rustgram Sections Screenshot",
        options,
        Box::new(move |cc| Ok(Box::new(SectionsScreenshotApp::new(cc, scene, out_clone)))),
    )
    .expect("eframe screenshot capture");
    assert!(
        output_path.exists(),
        "screenshot was not written to {}",
        output_path.display()
    );
    let _ = PeerId::ZERO;
}

#[test]
#[ignore = "opens native eframe window to capture crates/tg-app/docs/sections_forum_topics.png"]
fn capture_sections_forum_topics_screenshot() {
    run_capture(CaptureScene::ForumTopics);
}

#[test]
#[ignore = "opens native eframe window to capture crates/tg-app/docs/sections_comments_thread.png"]
fn capture_sections_comments_thread_screenshot() {
    run_capture(CaptureScene::CommentsThread);
}

#[test]
#[ignore = "opens native eframe window to capture crates/tg-app/docs/sections_poll_box.png"]
fn capture_sections_poll_box_screenshot() {
    run_capture(CaptureScene::PollBox);
}

#[test]
#[ignore = "opens native eframe window to capture crates/tg-app/docs/sections_schedule_box.png"]
fn capture_sections_schedule_box_screenshot() {
    run_capture(CaptureScene::ScheduleBox);
}
