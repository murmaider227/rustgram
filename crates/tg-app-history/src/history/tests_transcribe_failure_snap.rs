//! Before | after render of a failed voice transcription in the bubble (TGB-2978), written to
//! `$TGFORK_SHOTS_DIR`:
//!
//! ```sh
//! TGFORK_SHOTS_DIR=<dir> cargo test -p tg-app-history transcribe_failure_shots -- --ignored
//! ```
//!
//! "Before" paints the old text and colour ("Transcription unavailable.", secondary colour,
//! upright; the old code also showed a toast); "after" goes through the real painter.

use egui::{CornerRadius, Pos2, Rect, Vec2};
use egui_kittest::HarnessBuilder;
use tg_app_core::history::backend::TranscribeUiEntry;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::MsgId;
use tg_ui::fonts::register_tdesktop_fonts;

use crate::history::media::{paint_expanded_transcript, transcript_job};
use crate::history::theme::HistoryColors;

fn render(name: &str, dir: &std::path::Path, too_long: bool, old: bool) {
    let colors = HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Day));
    let mut harness = HarnessBuilder::default()
        .with_size(Vec2::new(320.0, 90.0))
        .with_render_cursor(false)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(register_tdesktop_fonts)
        .build_ui(move |ui| {
            let bubble = Rect::from_min_size(Pos2::new(12.0, 12.0), Vec2::new(280.0, 60.0));
            ui.painter()
                .rect_filled(ui.max_rect(), CornerRadius::ZERO, colors.canvas_bg);
            ui.painter()
                .rect_filled(bubble, CornerRadius::same(12), colors.msg_in_bg);
            let pos = bubble.min + Vec2::new(12.0, 20.0);
            if old {
                let text = if too_long {
                    "Audio is too long to transcribe."
                } else {
                    "Transcription unavailable."
                };
                let galley = ui.painter().layout_job(transcript_job(
                    text.to_owned(),
                    false,
                    colors.sub_text_fg,
                    256.0,
                ));
                ui.painter().galley(pos, galley, colors.sub_text_fg);
            } else {
                let entry = TranscribeUiEntry {
                    shown: true,
                    failed: true,
                    too_long,
                    ..TranscribeUiEntry::default()
                };
                let _ = paint_expanded_transcript(
                    ui,
                    MsgId(1),
                    pos,
                    256.0,
                    Some(&entry),
                    None,
                    &colors,
                    colors.accent,
                );
            }
        });
    harness.step();
    let image = harness.render().expect("wgpu render");
    image.save(dir.join(name)).expect("save png");
}

#[test]
#[ignore = "writes PNGs; run with TGFORK_SHOTS_DIR set"]
fn transcribe_failure_shots() {
    let Some(dir) = std::env::var_os("TGFORK_SHOTS_DIR").map(std::path::PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).expect("shots dir");
    render("transcribe_failed_before.png", &dir, false, true);
    render("transcribe_failed_after.png", &dir, false, false);
    render("transcribe_too_long_before.png", &dir, true, true);
    render("transcribe_too_long_after.png", &dir, true, false);
}
