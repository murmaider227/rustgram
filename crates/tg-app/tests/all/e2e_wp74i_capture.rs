//! Before/after screenshots for the WP-74 UI wiring. Uses only APIs that exist both before and
//! after the change, so the same test produces the "before" shots on the previous code
//! (`RUSTGRAM_WP74I_SNAP_STAGE=before`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use egui::{Pos2, Rect, Vec2};
use egui_kittest::{Harness, HarnessBuilder};
use tg_app::dialogs::backend::{
    DialogItem, DialogListQuery, DialogMediaPreview, DialogMediaPreviewKind, DialogPeerKind,
    DialogsBackend, SpecialRowKind,
};
use tg_app::dialogs::layout::{DialogColors, RowPaintState, paint_dialog_row};
use tg_app::dialogs::mock::MockDialogsStore;
use tg_app::info::{InfoBackend, SharedMediaTab};
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::{FilterId, PeerId};
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_history::{DialogSendState, LocalDateTime};
use tg_lang::tr;
use tg_ui::fonts::register_tdesktop_fonts;

use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, TEST_STEP_DT, apply_deterministic_style, build_wgpu_harness,
    right_click_at, settle_layout, step_frames, wait_until,
};

static WGPU_WP74I_CAPTURE_LOCK: Mutex<()> = Mutex::new(());

const DIALOGS_TOP_BAR_H: f32 = 54.0;
const ROW_H: f32 = 62.0;
const ROW_W: f32 = 360.0;

fn save_png<S>(harness: &mut Harness<'static, S>, path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create snap dir");
    }
    let img = harness.render().expect("wgpu render");
    img.save(path).expect("save png");
}

fn solid_stripped_thumb(rgb: [u8; 3]) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(40, 40, image::Rgba([rgb[0], rgb[1], rgb[2], 255]));
    tg_media::image::inline::encode_stripped_thumb(&img)
}

fn preview_row(title: &str, text: &str, preview: Option<DialogMediaPreview>) -> DialogItem {
    let store = MockDialogsStore::new();
    let slice = store.dialog_slice(DialogListQuery::Filter(FilterId::ALL), 0, 40);
    let mut item = slice
        .entries
        .into_iter()
        .find(|d| d.kind == DialogPeerKind::User && d.special == SpecialRowKind::None)
        .expect("a user row in the mock store");
    title.clone_into(&mut item.title);
    item.last_message = text.to_owned();
    item.last_sender = None;
    item.media_preview = preview;
    item.draft = None;
    item.typing = None;
    item.send_state = DialogSendState::None;
    item.badges = Default::default();
    item.muted = false;
    item.pinned = false;
    item.pinned_order = None;
    item.verified = false;
    item.premium = false;
    item.online = false;
    item.last_message_time = LocalDateTime::new(2026, 11, 22, 9, 5, 0);
    item
}

fn preview(kind: DialogMediaPreviewKind, thumb: Option<[u8; 3]>) -> Option<DialogMediaPreview> {
    Some(DialogMediaPreview {
        kind,
        stripped_thumb: thumb.map(solid_stripped_thumb),
        use_service_color: true,
        has_caption: false,
    })
}

struct RowsState {
    rows: Vec<DialogItem>,
    palette: Palette,
}

fn build_rows_harness(rows: Vec<DialogItem>, mode: ThemeMode) -> Harness<'static, RowsState> {
    let height = ROW_H * rows.len() as f32;
    HarnessBuilder::default()
        .with_size(Vec2::new(ROW_W, height))
        .with_step_dt(TEST_STEP_DT)
        .with_render_cursor(false)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .renderer(crate::common::serialized_wgpu_renderer())
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
        .build_ui_state(
            |ui, st: &mut RowsState| {
                let colors = DialogColors::new(&st.palette);
                let painter = ui.painter().clone();
                let now = LocalDateTime::new(2026, 11, 22, 14, 30, 0);
                for (i, item) in st.rows.iter().enumerate() {
                    let rect = Rect::from_min_size(
                        Pos2::new(0.0, i as f32 * ROW_H),
                        Vec2::new(ROW_W, ROW_H),
                    );
                    let _ = paint_dialog_row(
                        &painter,
                        rect,
                        item,
                        now,
                        RowPaintState::default(),
                        &st.palette,
                        &colors,
                    );
                }
            },
            RowsState {
                rows,
                palette: Palette::for_mode(mode),
            },
        )
}

fn open_dialog_menu(harness: &mut Harness<'static, AppTestState>, peer: PeerId) {
    let slice = harness.state().controller.mock_session().dialog_slice(
        DialogListQuery::Filter(FilterId::ALL),
        0,
        20,
    );
    let Some(idx) = slice.entries.iter().position(|d| d.peer_id == peer) else {
        return;
    };
    let geo = harness
        .state()
        .controller
        .session()
        .expect("session")
        .layout();
    let pos = Pos2::new(
        geo.filters_width as f32 + geo.dialogs_width as f32 * 0.5,
        DIALOGS_TOP_BAR_H + (idx as f32 + 1.5) * ROW_H,
    );
    right_click_at(harness, pos);
    step_frames(harness, 30);
}

/// Captures: preview rows (Day / Night), the main window chat list, Theo's chat-list menu and a
/// single (non-album) photo in the media viewer.
#[test]
#[ignore = "captures PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_wp74i_snapshots() {
    let _guard = WGPU_WP74I_CAPTURE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stage = std::env::var("RUSTGRAM_WP74I_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned());
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("wp74i"));

    for (mode, label) in [(ThemeMode::Day, "day"), (ThemeMode::Night, "night")] {
        let rows = vec![
            preview_row(
                "Avery Lane",
                &tr::lng_in_dlg_photo(),
                preview(DialogMediaPreviewKind::Photo, Some([70, 140, 90])),
            ),
            preview_row(
                "Jordan Hale",
                &tr::lng_in_dlg_video(),
                preview(DialogMediaPreviewKind::Video, Some([90, 110, 170])),
            ),
            preview_row(
                "Quinn Marsh",
                "sample_notes.txt",
                preview(DialogMediaPreviewKind::File, None),
            ),
            preview_row(
                "Rowan Pike",
                "\u{1F4CA} Lunch on Friday?",
                preview(DialogMediaPreviewKind::Poll, None),
            ),
            preview_row(
                "Sage Morrow",
                &tr::lng_in_dlg_contact(),
                preview(DialogMediaPreviewKind::Contact, None),
            ),
            preview_row(
                "Rustgram Dev Log",
                "Channel created",
                preview(DialogMediaPreviewKind::Service, None),
            ),
            preview_row("Tatum Reyes", "Plain text message", None),
        ];
        let mut harness = build_rows_harness(rows, mode);
        step_frames(&mut harness, 3);
        save_png(
            &mut harness,
            &dir.join(format!("{stage}_preview_rows_{label}.png")),
        );
    }

    for (size_label, size) in [
        ("1440x900", Vec2::new(1440.0, 900.0)),
        ("390x844", Vec2::new(390.0, 844.0)),
    ] {
        let mut harness = build_wgpu_harness(AppTestState::main_workspace(), size);
        settle_layout(&mut harness);
        step_frames(&mut harness, 10);
        save_png(
            &mut harness,
            &dir.join(format!("{stage}_chat_list_{size_label}.png")),
        );
        if size.x >= 1000.0 {
            open_dialog_menu(&mut harness, session_peers::USER_THEO);
            save_png(
                &mut harness,
                &dir.join(format!("{stage}_theo_menu_{size_label}.png")),
            );
        }

        let mut harness = build_wgpu_harness(AppTestState::main_workspace(), size);
        settle_layout(&mut harness);
        let photos = harness.state().controller.mock_session().shared_media(
            session_peers::GROUP_CORE,
            SharedMediaTab::PhotosVideos,
            "",
        );
        if let Some(first) = photos.first().map(|p| p.id()) {
            harness.state_mut().controller.open_shared_media(
                session_peers::GROUP_CORE,
                first,
                SharedMediaTab::PhotosVideos,
            );
            wait_until(
                &mut harness,
                DEFAULT_MAX_FRAMES,
                "the media viewer opens",
                |h| h.state().controller.media_overlay().is_open(),
            );
            step_frames(&mut harness, 20);
            save_png(
                &mut harness,
                &dir.join(format!("{stage}_media_viewer_{size_label}.png")),
            );
        }
    }
}
