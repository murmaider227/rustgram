//! Automated Day-theme screenshot harness capturing `crates/tg-app/docs/history.png`.
//!
//! Run manually with:
//! `cargo test -p tg-app capture_history_day_screenshot -- --ignored --nocapture`

use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use egui::{CentralPanel, Frame, Vec2, ViewportBuilder};

use crate::history::HistoryWidget;
use crate::history::mock::MockHistoryStore;
use tg_app_core::stand_in::strings::EnglishStrings;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerId};

struct HistoryScreenshotApp {
    widget: HistoryWidget,
    store: MockHistoryStore,
    peer_id: PeerId,
    strings: EnglishStrings,
    palette: Palette,
    output_path: PathBuf,
    frames: u32,
    requested: bool,
    done: Arc<AtomicBool>,
}

impl HistoryScreenshotApp {
    fn new(cc: &eframe::CreationContext<'_>, output_path: PathBuf) -> Self {
        let palette = Palette::for_mode(ThemeMode::Day);
        cc.egui_ctx
            .set_visuals(palette.to_egui_visuals(ThemeMode::Day));

        let store = MockHistoryStore::new();
        let peer_id = PeerId::from_chat(2001);
        let widget = HistoryWidget::new();

        Self {
            widget,
            store,
            peer_id,
            strings: EnglishStrings,
            palette,
            output_path,
            frames: 0,
            requested: false,
            done: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl eframe::App for HistoryScreenshotApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // After the peer loads on frame 1, set up reply compose bar and frame the rich
        // showcase messages (album, voice waveform, Lottie sticker, unread bar, poll, reply, FAB).
        if self.frames == 2 {
            self.widget.compose_mut().start_reply(
                MsgId(102),
                "Sam Chen".to_owned(),
                "Milestone 2 History Checklist — We now wire tg-history...".to_owned(),
            );
            self.widget.compose_mut().text =
                "Looks great! Shipping **60fps** virtualized history with `tg-history`.".to_owned();
            // Position viewport so the 4-tile album, voice waveform, Lottie animated sticker,
            // Unread messages bar, interactive poll, and outgoing reply are visible together.
            if let Some(idx) = self
                .widget
                .inner()
                .messages()
                .iter()
                .position(|m| m.id == MsgId(107))
            {
                let y = self.widget.inner().list().item_top(idx) as f32 + 275.0;
                self.widget.inner_mut().set_scroll_offset(y);
            }
        }
        CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
            let _ = self.widget.show_with_backend(
                ui,
                self.peer_id,
                &mut self.store,
                true,
                false,
                false,
                &self.palette,
                &self.strings,
            );
        });

        self.frames += 1;
        if self.done.load(Ordering::Acquire) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if !self.requested && self.frames >= 12 {
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

#[test]
#[ignore = "opens native eframe window to capture crates/tg-app/docs/history.png"]
fn capture_history_day_screenshot() {
    let output_path = tg_app_core::core_types::paths::workspace_app_docs_dir().join("history.png");
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title("Rustgram — History View (Day Theme)")
            .with_inner_size(Vec2::new(780.0, 960.0)),
        #[cfg(target_os = "windows")]
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::windows::EventLoopBuilderExtWindows;
            builder.with_any_thread(true);
        })),
        ..Default::default()
    };
    let out_clone = output_path.clone();
    eframe::run_native(
        "Rustgram History Screenshot",
        options,
        Box::new(move |cc| Ok(Box::new(HistoryScreenshotApp::new(cc, out_clone)))),
    )
    .expect("eframe screenshot capture");
    assert!(
        output_path.exists(),
        "screenshot was not written to {}",
        output_path.display()
    );
}
