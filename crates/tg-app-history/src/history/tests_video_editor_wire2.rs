//! Tests and headless `egui_kittest` snapshot helpers for WP-46 Video Editor multi-clip
//! timeline, overlay visibility ranges, and versioned scene draft wiring.

use std::path::PathBuf;
use std::sync::Mutex;

use egui::Vec2;
use egui_kittest::HarnessBuilder;
use egui_kittest::kittest::Queryable;
use tempfile::TempDir;
use tg_app_core::stand_in::theme::Palette;
use tg_editor::video::VideoClipSource;
use tg_editor::{SceneDraft, Size as EditorSize};
use tg_ui::fonts::register_tdesktop_fonts;

use crate::history::compose_boxes::{AttachFilesBoxState, PhotoEditorModalState};
use crate::history::theme::HistoryColors;
use crate::history::video_editor::VideoEditorModalState;

static WGPU_EDITOR_LOCK: Mutex<()> = Mutex::new(());

fn save_image_png(img: &image::RgbaImage, path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create snap dir");
    }
    img.save(path).expect("save screenshot png");
}

fn build_video_editor_harness(
    modal: VideoEditorModalState,
    size: Vec2,
) -> egui_kittest::Harness<'static, VideoEditorModalState> {
    let colors = HistoryColors::from_palette(&Palette::day());
    HarnessBuilder::default()
        .with_size(size)
        .with_step_dt(1.0 / 30.0)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
            ctx.set_visuals(egui::Visuals::light());
            ctx.all_styles_mut(|s| {
                s.visuals.text_cursor.blink = false;
                s.animation_time = 0.0;
            });
        })
        .build_ui_state(
            move |ui, st: &mut VideoEditorModalState| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(colors.bar_bg))
                    .show(ui, |ui| {
                        let _ = st.show(ui, &colors);
                    });
            },
            modal,
        )
}

#[test]
fn video_editor_multi_clip_timeline_split_reorder_mute_and_overlay_timing_in_kittest() {
    let modal = VideoEditorModalState::open_for_file(
        0,
        "sample_reel.mp4",
        1280,
        720,
        12_000,
        4_200_000,
        None,
        11,
    );
    let mut harness = build_video_editor_harness(modal, Vec2::new(820.0, 720.0));
    // Centered `egui::Area` settles its rect across two initial passes.
    harness.step();
    harness.step();

    assert_eq!(harness.state().clip_player.timeline().len(), 1);
    assert_eq!(harness.state().clip_player.total_duration_ms(), 12_000);

    // 1. Click "+ Clip" to append a 6s clip -> total 18s across 2 clips.
    harness.get_by_label("+ Clip").click();
    harness.step();
    harness.step();
    assert_eq!(harness.state().clip_player.timeline().len(), 2);
    assert_eq!(harness.state().clip_player.total_duration_ms(), 18_000);
    assert_eq!(harness.state().selected_clip_index, 1);

    // 2. Seek into the middle of clip 1 (6_000 ms) and click "Split" -> 3 clips, total still 18s.
    let pos = harness
        .state_mut()
        .seek_global_ms(6_000)
        .expect("seek 6000ms");
    assert_eq!(pos.clip_index, 0);
    assert_eq!(pos.local_ms, 6_000);
    harness.step();
    harness.step();

    harness.get_by_label("Split").click();
    harness.step();
    harness.step();
    assert_eq!(harness.state().clip_player.timeline().len(), 3);
    assert_eq!(harness.state().clip_player.total_duration_ms(), 18_000);
    let second_id = harness
        .state()
        .clip_player
        .timeline()
        .get(1)
        .expect("c1")
        .id;

    // 3. Reorder selected clip left via "Move Left".
    assert_eq!(harness.state().selected_clip_index, 1);
    harness.get_by_label("Move Left").click();
    harness.step();
    harness.step();
    assert_eq!(harness.state().selected_clip_index, 0);
    assert_eq!(
        harness
            .state()
            .clip_player
            .timeline()
            .get(0)
            .expect("c0")
            .id,
        second_id
    );

    // 4. Mute selected clip via "Mute Clip" button.
    harness.get_by_label("Mute Clip").click();
    harness.step();
    harness.step();
    assert!(
        harness
            .state()
            .clip_player
            .timeline()
            .get(0)
            .expect("c0")
            .is_muted()
    );

    // 5. Add a "+ Text" overlay and constrain its visibility to "Second Half" (9s..18s).
    harness.get_by_label("+ Text").click();
    harness.step();
    harness.step();
    harness.get_by_label("Second Half").click();
    harness.step();
    harness.step();

    let _ = harness.state_mut().seek_global_ms(2_000);
    assert!(
        harness.state().visible_overlays_at_playhead().is_empty(),
        "overlay restricted to second half must be hidden at 2s"
    );
    let _ = harness.state_mut().seek_global_ms(12_000);
    assert_eq!(
        harness.state().visible_overlays_at_playhead().len(),
        1,
        "overlay restricted to second half must be active at 12s"
    );

    // 6. Verify invalid visibility range (start >= end) is rejected cleanly.
    let item_id = harness
        .state()
        .selected_overlay_id
        .expect("selected overlay");
    assert!(
        harness
            .state_mut()
            .set_overlay_visibility(item_id, 5_000, 5_000)
            .is_err()
    );

    // 7. Build deterministic multi-clip FFmpeg export plan.
    let plan = harness
        .state()
        .build_multi_clip_export_plan(std::path::Path::new("out.mp4"))
        .expect("build multi-clip export plan");
    assert_eq!(plan.inputs.len(), 3);
    assert_eq!(plan.total_duration_ms, 18_000);
    assert!(plan.filter_complex.contains("concat=n=3"));
}

#[test]
fn video_and_photo_editor_scene_draft_disk_roundtrip_and_attach_box_restore() {
    let dir = TempDir::new().expect("temp dir for scene draft");
    let draft_file = dir.path().join("reel_draft.tged");

    let mut modal =
        VideoEditorModalState::open_for_file(0, "reel.mp4", 1280, 720, 10_000, 3_000_000, None, 5);
    modal.append_clip(VideoClipSource::synthetic(
        5_000,
        EditorSize::new(1280, 720),
        true,
    ));
    let _ = modal.set_selected_clip_muted(true);
    modal.model.rotate_cw();
    modal.model.quality.set_shorter_side(480);

    let fp_saved = modal
        .save_draft_to_path(&draft_file)
        .expect("save scene draft to temp file");
    assert!(draft_file.is_file());

    // Load into a brand-new modal and verify exact multi-clip + rotation + quality restoration.
    let mut restored =
        VideoEditorModalState::open_for_file(0, "reel.mp4", 1280, 720, 10_000, 3_000_000, None, 5);
    let fp_loaded = restored
        .load_draft_from_path(&draft_file)
        .expect("load scene draft from temp file");
    assert_eq!(fp_saved, fp_loaded);
    assert_eq!(restored.clip_player.timeline().len(), 2);
    assert_eq!(restored.clip_player.total_duration_ms(), 15_000);
    assert!(
        restored
            .clip_player
            .timeline()
            .get(1)
            .expect("c1")
            .is_muted()
    );
    assert_eq!(restored.model.geometry.angle, 90);
    assert_eq!(restored.model.quality.selected_shorter_side(), 480);

    // Unsupported future draft version fails cleanly.
    let future_json = r#"{"version":999,"crop_enhance":{},"scene":{}}"#;
    assert!(SceneDraft::from_json(future_json).is_err());

    // PhotoEditorModalState draft round-trip + AttachFilesBoxState storage.
    let mut photo = PhotoEditorModalState::open_for_file(0, "cover.jpg", 3);
    photo.rotate_cw();
    photo.add_text_overlay("Hello Draft");
    let photo_draft = photo.build_scene_draft();
    let json = photo_draft.to_json().expect("serialize photo draft");
    let decoded = SceneDraft::from_json(&json).expect("deserialize photo draft");

    let mut photo2 = PhotoEditorModalState::open_for_file(0, "cover.jpg", 3);
    photo2
        .apply_scene_draft(&decoded)
        .expect("apply decoded photo draft");
    assert_eq!(photo2.mods.angle, 90);
    assert_eq!(photo2.paint_scene.active_items_by_z().len(), 1);

    let mut attach = AttachFilesBoxState::new(false, false);
    attach.scene_drafts.insert("cover.jpg".to_owned(), decoded);
    assert!(attach.scene_drafts.contains_key("cover.jpg"));
}

#[test]
#[ignore = "captures before/after design review PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_wire2_editor_snapshots() {
    let _guard = WGPU_EDITOR_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stage = std::env::var("RUSTGRAM_WIRE2_SNAP_STAGE")
        .or_else(|_| std::env::var("TGFORK_WIRE2_SNAP_STAGE"))
        .unwrap_or_else(|_| "after".to_owned());
    let out_dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("wire2"));
    let sizes = [
        ("1440x900", Vec2::new(1440.0, 900.0)),
        ("390x844", Vec2::new(390.0, 844.0)),
    ];

    for (tag, size) in sizes {
        let mut modal = VideoEditorModalState::open_for_file(
            0,
            "sample_reel.mp4",
            1280,
            720,
            12_000,
            4_200_000,
            None,
            11,
        );
        if stage != "before" {
            let _ = modal.append_sample_clip(6_000);
            let _ = modal.seek_global_ms(5_000);
            let _ = modal.split_at_playhead();
            let _ = modal.set_selected_clip_muted(true);
            let dim = modal.model.source_info.dimensions;
            let id = modal.overlay_scene.add_text(tg_editor::TextItem::new(
                "Caption",
                tg_editor::Color::WHITE,
                28.0,
                tg_editor::TextStyle::SemiTransparent,
                tg_editor::TextTypeface::Default,
                tg_editor::TextAlignment::Center,
                dim,
                tg_editor::PointF::new(f64::from(dim.width) * 0.5, f64::from(dim.height) * 0.5),
            ));
            modal.selected_overlay_id = Some(id);
            let _ = modal.set_overlay_visibility(id, 4_000, 14_000);
            let draft = modal.build_scene_draft();
            modal.saved_draft = Some(draft);
        }
        let colors = HistoryColors::from_palette(&Palette::day());
        let mut harness = HarnessBuilder::default()
            .with_size(size)
            .with_step_dt(1.0 / 30.0)
            .with_render_cursor(false)
            .allow_missing_glyphs()
            .with_accessibility_check(false)
            .wgpu()
            .with_context_setup(|ctx| {
                register_tdesktop_fonts(ctx);
                ctx.set_visuals(egui::Visuals::light());
                ctx.all_styles_mut(|s| {
                    s.visuals.text_cursor.blink = false;
                    s.animation_time = 0.0;
                });
            })
            .build_ui_state(
                move |ui, st: &mut VideoEditorModalState| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE.fill(colors.bar_bg))
                        .show(ui, |ui| {
                            let _ = st.show(ui, &colors);
                        });
                },
                modal,
            );
        harness.step();
        harness.step();
        let img = harness.render().expect("render Video Editor snapshot");
        save_image_png(
            &img,
            &out_dir.join(format!("video_editor_{tag}_{stage}.png")),
        );
    }
}
