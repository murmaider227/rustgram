//! TGB-3011: deleted messages turn to dust where they were painted and the hole they leave
//! closes smoothly (tdesktop `Ui::ThanosEffectController`, `thanos_effect_controller.cpp`).
//!
//! The tests drive the real delete box of a mock chat. `w5_delete_frames` writes frame sheets
//! (`delete_single.png`, `delete_several.png` and the single frames) into `$W5_DELETE_SNAP_DIR`
//! when that variable is set. It uses only APIs that predate the fix, so the same test renders
//! the "before" evidence on the old code.

use egui::{Rect, Vec2};
use egui_kittest::HarnessBuilder;
use egui_kittest::kittest::Queryable;
use tg_ui::fonts::register_tdesktop_fonts;

use crate::history::HistoryWidget;
use crate::history::backend::HistoryBackend;
use crate::history::compose_boxes::DeleteConfirmBoxState;
use crate::history::mock::MockHistoryStore;
use tg_app_core::stand_in::strings::EnglishStrings;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::{MsgId, PeerId};

const SIZE: Vec2 = Vec2::new(900.0, 1000.0);
const STEP_DT: f32 = 1.0 / 60.0;

/// One GPU device at a time: renderers starting together have crashed test processes.
static GPU: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) struct DeleteState {
    _gpu: std::sync::MutexGuard<'static, ()>,
    pub(crate) widget: HistoryWidget,
    store: MockHistoryStore,
    palette: Palette,
    peer: PeerId,
}

pub(crate) type DeleteHarness = egui_kittest::Harness<'static, DeleteState>;

/// The mock group "Rustgram Core Port": enough messages to fill the view.
const CHAT: PeerId = PeerId::from_chat(2001);

/// A mock group chat opened at its bottom.
pub(crate) fn open_chat() -> DeleteHarness {
    open_chat_in(MockHistoryStore::new())
}

pub(crate) fn open_chat_in(store: MockHistoryStore) -> DeleteHarness {
    open_peer_in(store, CHAT)
}

pub(crate) fn open_peer_in(store: MockHistoryStore, peer: PeerId) -> DeleteHarness {
    let state = DeleteState {
        _gpu: GPU
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        widget: HistoryWidget::new(),
        store,
        palette: Palette::day(),
        peer,
    };
    let mut harness = HarnessBuilder::default()
        .with_size(SIZE)
        .with_step_dt(STEP_DT)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
        })
        .build_ui_state(
            |ui, st: &mut DeleteState| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        st.widget.show_with_backend(
                            ui,
                            st.peer,
                            &mut st.store,
                            true,
                            false,
                            false,
                            &st.palette,
                            &EnglishStrings,
                        );
                    });
            },
            state,
        );
    harness.run_steps(6);
    let peer = harness.state().peer;
    harness.state_mut().widget.request_scroll_to_bottom(peer);
    harness.run_steps(30);
    // Media placeholders settle their heights; the view stays at the bottom.
    harness.state_mut().widget.request_scroll_to_bottom(peer);
    harness.run_steps(30);
    harness
}

/// Regular (non-service) messages of the chat, oldest first.
pub(crate) fn regular_messages(harness: &DeleteHarness) -> Vec<MsgId> {
    harness
        .state()
        .widget
        .inner()
        .messages()
        .iter()
        .filter(|m| m.id.0 > 0 && !m.is_service())
        .map(|m| m.id)
        .collect()
}

pub(crate) fn bubble(harness: &DeleteHarness, id: MsgId) -> Option<Rect> {
    harness.state().widget.inner().last_painted_bubble_rect(id)
}

/// Opens the delete box for `ids` and presses its "Delete" button.
pub(crate) fn delete_with_box(harness: &mut DeleteHarness, ids: &[MsgId]) {
    harness.state_mut().widget.delete_box = Some(DeleteConfirmBoxState::new(
        ids.to_vec(),
        "Rustgram Core Port",
        true,
    ));
    // The modal area measures itself on its first frame.
    harness.run_steps(2);
    harness.get_by_label("Delete").click();
    harness.step();
}

fn save(image: &image::RgbaImage, name: &str) {
    if let Some(dir) = std::env::var_os("W5_DELETE_SNAP_DIR") {
        let path = std::path::PathBuf::from(dir).join(format!("{name}.png"));
        image.save(&path).expect("save the delete snapshot");
    }
}

/// Lays `frames` side by side, each cropped to the lower part of the chat.
fn frame_sheet(frames: &[image::RgbaImage]) -> image::RgbaImage {
    let crop_top = 150u32;
    let crop_h = 790u32;
    let crop_left = 0u32;
    let crop_w = 900u32;
    let gap = 6u32;
    let mut sheet = image::RgbaImage::from_pixel(
        (crop_w + gap) * frames.len() as u32,
        crop_h,
        image::Rgba([40, 40, 40, 255]),
    );
    for (i, frame) in frames.iter().enumerate() {
        let x0 = (crop_w + gap) * i as u32;
        for y in 0..crop_h {
            for x in 0..crop_w {
                let (sx, sy) = (crop_left + x, crop_top + y);
                if sx < frame.width() && sy < frame.height() {
                    sheet.put_pixel(x0 + x, y, *frame.get_pixel(sx, sy));
                }
            }
        }
    }
    sheet
}

/// Renders the frame before the delete and frames at the given milliseconds after it.
fn record(harness: &mut DeleteHarness, ids: &[MsgId], name: &str) {
    let first = harness.render().expect("render");
    save(&first, &format!("{name}_before"));
    let mut frames = vec![first];
    delete_with_box(harness, ids);
    let mut elapsed_ms = 0.0f32;
    for at_ms in [16.0f32, 100.0, 200.0, 350.0, 600.0, 1000.0, 1600.0] {
        while elapsed_ms + 0.5 < at_ms {
            harness.step();
            elapsed_ms += STEP_DT * 1000.0;
        }
        let frame = harness.render().expect("render");
        save(&frame, &format!("{name}_{}ms", at_ms as u32));
        frames.push(frame);
    }
    save(&frame_sheet(&frames), name);
}

#[test]
fn w5_delete_frames() {
    if std::env::var_os("W5_DELETE_SNAP_DIR").is_none() {
        return;
    }
    let mut harness = open_chat();
    let ids = regular_messages(&harness);
    let last = *ids.last().expect("the chat has messages");
    record(&mut harness, &[last], "delete_single");
    drop(harness);

    let mut harness = open_chat();
    let ids = regular_messages(&harness);
    let n = ids.len();
    assert!(n >= 5, "the mock chat has enough messages");
    record(
        &mut harness,
        &[ids[n - 4], ids[n - 3], ids[n - 1]],
        "delete_several",
    );
}

/// tdesktop dissolves the snapshot of the deleted message at the place it was painted
/// (`captureView`: the view's own rect, mapped to the window).
#[test]
fn the_dust_starts_from_the_deleted_bubble() {
    let mut harness = open_chat();
    let ids = regular_messages(&harness);
    let last = *ids.last().expect("the chat has messages");
    let painted = bubble(&harness, last).expect("the last message is on screen");
    delete_with_box(&mut harness, &[last]);
    harness.run_steps(4);
    let items = harness.state().widget.thanos_effect.items();
    assert_eq!(items.len(), 1, "one message turns to dust");
    let rect = items[0].rect;
    assert!(
        (rect.min - painted.min).length() < 1.0 && (rect.max - painted.max).length() < 1.0,
        "the dust covers the painted bubble {painted:?}, not {rect:?}"
    );
}

/// Messages that are not on screen leave no dust (`captureView` returns early).
#[test]
fn only_messages_on_screen_turn_to_dust() {
    let mut harness = open_chat();
    let ids = regular_messages(&harness);
    let (on_screen, off_screen): (Vec<MsgId>, Vec<MsgId>) = ids
        .iter()
        .copied()
        .partition(|id| bubble(&harness, *id).is_some());
    let n = on_screen.len();
    assert!(n >= 3 && !off_screen.is_empty(), "{n} messages on screen");
    let targets = [off_screen[0], on_screen[n - 1], on_screen[n - 3]];
    delete_with_box(&mut harness, &targets);
    // The frame the dust is cut from: the messages are still painted in it.
    harness.step();
    let painted: Vec<Rect> = [on_screen[n - 1], on_screen[n - 3]]
        .iter()
        .map(|id| bubble(&harness, *id).expect("still painted while the frame is captured"))
        .collect();
    harness.run_steps(3);
    let items = harness.state().widget.thanos_effect.items();
    assert_eq!(items.len(), 2, "two of the deleted messages were on screen");
    for rect in painted {
        assert!(
            items
                .iter()
                .any(|item| (item.rect.min - rect.min).length() < 1.0),
            "dust for the bubble at {rect:?}: {:?}",
            items.iter().map(|item| item.rect).collect::<Vec<_>>()
        );
    }
}

/// The hole closes over 400..600 ms with `anim::halfSine` (`startCollapseAnimation`); at the
/// bottom of the chat the messages above slide down into it instead of jumping.
#[test]
fn the_messages_above_slide_down_into_the_hole() {
    let mut harness = open_chat();
    let ids = regular_messages(&harness);
    let n = ids.len();
    let (above, last) = (ids[n - 2], ids[n - 1]);
    let start = bubble(&harness, above).expect("on screen").top();
    delete_with_box(&mut harness, &[last]);
    harness.run_steps(12);
    let mid = bubble(&harness, above).expect("on screen").top();
    harness.run_steps(60);
    let end = bubble(&harness, above).expect("on screen").top();
    assert!(
        end > start + 10.0,
        "the message moved down: {start} -> {end}"
    );
    assert!(
        mid > start + 1.0 && mid < end - 1.0,
        "200 ms in, the message is on its way: {start} -> {mid} -> {end}"
    );
    assert!(
        harness.state().widget.thanos_effect.items().is_empty()
            || harness.state().widget.thanos_effect.animating(),
    );
}

/// Several deleted messages: each leaves its own hole, all close together, and the list ends
/// exactly where a fresh layout without them puts it.
#[test]
fn several_holes_close_to_the_final_layout() {
    let mut harness = open_chat();
    let ids = regular_messages(&harness);
    let n = ids.len();
    let keep = ids[n - 2];
    let deleted = [ids[n - 4], ids[n - 3], ids[n - 1]];
    delete_with_box(&mut harness, &deleted);
    harness.run_steps(8);
    let early = bubble(&harness, keep).expect("on screen").top();
    harness.run_steps(80);
    let settled = bubble(&harness, keep).expect("on screen").top();
    assert!(early < settled - 1.0, "the kept message is still moving");

    drop(harness);
    let mut store = MockHistoryStore::new();
    store
        .delete_messages(CHAT, &deleted, true)
        .expect("mock delete");
    let mut fresh = open_chat_in(store);
    fresh.run_steps(2);
    let fresh_top = bubble(&fresh, keep).expect("on screen").top();
    assert!(
        (settled - fresh_top).abs() < 1.5,
        "after the animation the list matches a fresh layout: {settled} vs {fresh_top}"
    );
}

/// A short chat sits at the bottom of the view (`_historyMarginTop`): the hole closes there too,
/// the messages above slide down and the ones below stay where they are.
#[test]
fn a_short_chat_closes_the_hole_in_place() {
    let mut store = MockHistoryStore::new();
    let saved = store.self_peer().0;
    store.populate_many_messages(saved, 4);
    let mut harness = open_peer_in(store, saved);
    let ids = regular_messages(&harness);
    let n = ids.len();
    assert_eq!(n, 4, "the short chat has four messages");
    let (above, gone, below) = (ids[n - 3], ids[n - 2], ids[n - 1]);
    let top = |h: &DeleteHarness, id| bubble(h, id).expect("on screen").top();
    let first_top = top(&harness, ids[0]);
    assert!(
        first_top > 300.0,
        "the chat is shorter than the view: {first_top}"
    );
    let (above_start, below_start) = (top(&harness, above), top(&harness, below));
    delete_with_box(&mut harness, &[gone]);
    harness.run_steps(12);
    let (above_mid, below_mid) = (top(&harness, above), top(&harness, below));
    harness.run_steps(60);
    let (above_end, below_end) = (top(&harness, above), top(&harness, below));
    for (when, y) in [("during", below_mid), ("after", below_end)] {
        assert!(
            (y - below_start).abs() < 1.0,
            "the message below the hole stays {when} the collapse: {below_start} -> {y}"
        );
    }
    assert!(
        above_end > above_start + 10.0,
        "{above_start} -> {above_end}"
    );
    assert!(
        above_mid > above_start + 1.0 && above_mid < above_end - 1.0,
        "the message above slides down: {above_start} -> {above_mid} -> {above_end}"
    );
}
