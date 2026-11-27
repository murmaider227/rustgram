//! Before/after captures of the chat-list stories strip for wave 7 (TGB-3037, TGB-3038,
//! TGB-3039). Ignored; run with `W7_SNAP_DIR=<dir> W7_SNAP_STAGE=before|after` and
//! `--ignored`. The photos are synthetic gradients, no real people.

use std::sync::Arc;

use egui::{Align2, CornerRadius, FontId, Pos2, Rect, Vec2};
use tg_stories::{ParsedAllStories, StoriesStore, StorySourcesList};
use tg_ui::userpic_images::{UserpicImage, UserpicImageSource, install_userpic_source};

use crate::stories::backend::StoriesBackend;
use crate::stories::mock::MockStoriesBackend;
use crate::stories::strip::{StoriesSearchSlot, StoriesStrip};
use tg_app_core::stand_in::theme::Palette;

const NOW: i32 = 1_760_000_000;
const SIZE: Vec2 = Vec2::new(420.0, 190.0);

/// A diagonal two-colour gradient with a light disc, different per peer.
struct GradientPhotos;

impl UserpicImageSource for GradientPhotos {
    fn userpic(&self, peer_id: i64) -> Option<Arc<UserpicImage>> {
        let seed = (peer_id.unsigned_abs() % 997) as u32;
        let hue = |k: u32| -> [u8; 3] {
            let h = ((seed * 67 + k * 131) % 360) as f32 / 60.0;
            let x = (1.0 - (h % 2.0 - 1.0).abs()) * 200.0;
            let (r, g, b) = match h as u32 {
                0 => (200.0, x, 40.0),
                1 => (x, 200.0, 40.0),
                2 => (40.0, 200.0, x),
                3 => (40.0, x, 200.0),
                4 => (x, 40.0, 200.0),
                _ => (200.0, 40.0, x),
            };
            [r as u8, g as u8, b as u8]
        };
        let (a, b) = (hue(0), hue(1));
        let side = 64usize;
        let mut rgba = Vec::with_capacity(side * side * 4);
        for y in 0..side {
            for x in 0..side {
                let t = (x + y) as f32 / (2 * side - 2) as f32;
                let (dx, dy) = (x as f32 - 40.0, y as f32 - 24.0);
                let disc = dx * dx + dy * dy < 100.0;
                for ch in 0..3 {
                    let v = f32::from(a[ch]) * (1.0 - t) + f32::from(b[ch]) * t;
                    rgba.push(if disc { 240 } else { v as u8 });
                }
                rgba.push(255);
            }
        }
        Some(Arc::new(UserpicImage {
            photo_id: i64::from(seed),
            width: side,
            height: side,
            rgba,
        }))
    }
}

struct SnapState {
    strip: StoriesStrip,
    store: StoriesStore,
    backend: MockStoriesBackend,
    palette: Palette,
}

fn field() -> Rect {
    Rect::from_min_size(Pos2::new(56.0, 9.0), Vec2::new(352.0, 35.0))
}

fn harness(state: SnapState) -> egui_kittest::Harness<'static, SnapState> {
    egui_kittest::HarnessBuilder::default()
        .with_size(SIZE)
        .with_render_cursor(false)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
            install_userpic_source(ctx, Arc::new(GradientPhotos));
        })
        .build_ui_state(
            |ui, st: &mut SnapState| {
                let painter = ui.painter().clone();
                painter.rect_filled(ui.max_rect(), 0.0, st.palette.window_bg);
                painter.rect_filled(field(), CornerRadius::same(17), st.palette.window_bg_over);
                painter.text(
                    Pos2::new(field().left() + 14.0, field().center().y),
                    Align2::LEFT_CENTER,
                    "Search",
                    FontId::proportional(13.0),
                    st.palette.window_sub_text_fg,
                );
                ui.allocate_space(Vec2::new(SIZE.x, 54.0));
                let slot = StoriesSearchSlot::for_field(field(), &st.palette);
                st.strip
                    .show_in_search_row(ui, slot, &st.store, &st.backend, &st.palette);
                let rest = ui.available_rect_before_wrap();
                painter.rect_filled(rest, 0.0, st.palette.window_bg);
                painter.text(
                    Pos2::new(rest.left() + 14.0, rest.top() + 20.0),
                    Align2::LEFT_CENTER,
                    "Chat list",
                    FontId::proportional(13.0),
                    st.palette.window_sub_text_fg,
                );
            },
            state,
        )
}

fn mock() -> (StoriesStore, MockStoriesBackend) {
    let mut backend = MockStoriesBackend::new(NOW);
    let mut store = StoriesStore::new(backend.self_peer_id());
    backend.bootstrap_store(&mut store, NOW);
    (store, backend)
}

fn save(h: &mut egui_kittest::Harness<'static, SnapState>, name: &str) {
    let Some(dir) = std::env::var_os("W7_SNAP_DIR").map(std::path::PathBuf::from) else {
        return;
    };
    let stage = std::env::var("W7_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned());
    let dir = dir.join(stage);
    std::fs::create_dir_all(&dir).expect("snap dir");
    let img = h.render().expect("wgpu render");
    img.save(dir.join(format!("{name}.png"))).expect("save png");
}

#[test]
#[ignore = "captures wave 7 strip PNGs under W7_SNAP_DIR"]
fn capture_w7_strip() {
    let palette = Palette::day();

    // TGB-3037: collapsed and expanded userpics with loaded photos.
    let (store, backend) = mock();
    let mut h = harness(SnapState {
        strip: StoriesStrip::new(),
        store,
        backend,
        palette,
    });
    h.run_steps(4);
    let small = h.state().strip.collapsed_rect(field());
    if let Some(small) = small {
        h.hover_at(small.center());
        for _ in 0..40 {
            h.step();
        }
    }
    save(&mut h, "strip_collapsed_photos_tooltip");
    h.state_mut().strip.set_expanded_immediate(true);
    h.hover_at(Pos2::new(5.0, SIZE.y - 5.0));
    h.run_steps(4);
    save(&mut h, "strip_expanded_photos");

    // TGB-3039 item 2: halfway between collapsed and expanded.
    let (store, backend) = mock();
    let mut h = harness(SnapState {
        strip: StoriesStrip::new(),
        store,
        backend,
        palette,
    });
    h.run_steps(2);
    let _ = h.state_mut().strip.apply_pull_height(44);
    h.step();
    save(&mut h, "strip_expand_midway");

    // TGB-3039 item 4: a failed upload of the own story.
    let (store, mut backend) = mock();
    let me = backend.self_peer_id();
    backend.set_pending_uploads(vec![tg_stories::StoryUploadStatus {
        peer_id: me,
        peer_name: "My Story".to_owned(),
        progress: 0.4,
        failed: true,
    }]);
    let mut h = harness(SnapState {
        strip: StoriesStrip::expanded(),
        store,
        backend,
        palette,
    });
    h.run_steps(40);
    save(&mut h, "strip_failed_upload");

    // TGB-3038: no stories, sources known.
    let backend = MockStoriesBackend::new(NOW);
    let mut store = StoriesStore::new(backend.self_peer_id());
    store.apply_all_stories(
        StorySourcesList::NotHidden,
        ParsedAllStories::All {
            has_more: false,
            count: 0,
            state: "s1".to_owned(),
            peer_stories: Vec::new(),
            stealth_mode: Default::default(),
        },
        NOW,
    );
    let mut h = harness(SnapState {
        strip: StoriesStrip::new(),
        store,
        backend,
        palette,
    });
    h.run_steps(4);
    save(&mut h, "search_field_no_stories");
}
