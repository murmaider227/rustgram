//! TGB-2904: the profile panel paints a peer's loaded photo userpic in the cover and in the
//! members list (`PeerData::paintUserpic`), falling back to the empty userpic.
//!
//! Writes `profile_<peer>.png` into `$TGB2904_SNAP_DIR` when set; otherwise only renders.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use egui_kittest::Harness;
use tg_app_core::stand_in::theme::ThemeMode;
use tg_app_core::stand_in::types::PeerId;
use tg_ui::userpic_images::{UserpicImage, UserpicImageSource, install_userpic_source};

use crate::info::colors::InfoColors;
use crate::info::mock::{MockBackend, mock_peers};
use crate::info::wrap_widget::InfoWrapWidget;

/// A synthetic "photo": a diagonal two-tone pattern with a disc, unlike any empty userpic.
fn synthetic_photo(seed: i64) -> UserpicImage {
    const SIDE: usize = 160;
    let mut rgba = Vec::with_capacity(SIDE * SIDE * 4);
    let tint = (seed.unsigned_abs() % 5) as u8 * 30;
    for y in 0..SIDE {
        for x in 0..SIDE {
            let dx = x as f32 - 100.0;
            let dy = y as f32 - 60.0;
            let (r, g, b) = if dx * dx + dy * dy < 22.0 * 22.0 {
                (250, 220, 90)
            } else if x + y < SIDE + 20 {
                (40 + tint, 110, 170)
            } else {
                (30, 70 + tint, 60)
            };
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    UserpicImage {
        photo_id: 7_000 + seed,
        width: SIDE,
        height: SIDE,
        rgba,
    }
}

#[derive(Default)]
struct RecordingSource {
    asked: Mutex<BTreeSet<i64>>,
}

impl UserpicImageSource for RecordingSource {
    fn userpic(&self, peer_id: i64) -> Option<Arc<UserpicImage>> {
        if let Ok(mut asked) = self.asked.lock() {
            asked.insert(peer_id);
        }
        Some(Arc::new(synthetic_photo(peer_id)))
    }
}

struct State {
    wrap: InfoWrapWidget,
    backend: MockBackend,
}

fn render_profile(peer: PeerId, tag: &str, source: &Arc<RecordingSource>) {
    let colors = InfoColors::for_mode(ThemeMode::Day);
    let source = Arc::clone(source);
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(420.0, 900.0))
        .with_context_setup(move |ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
            install_userpic_source(ctx, source.clone());
        })
        .wgpu()
        .build_ui_state(
            move |ui, st: &mut State| {
                st.wrap.show(ui, &mut st.backend, true, &colors);
            },
            State {
                wrap: InfoWrapWidget::for_peer(peer),
                backend: MockBackend::new(),
            },
        );
    harness.run();
    if let Some(dir) = std::env::var_os("TGB2904_SNAP_DIR") {
        let img = harness.render().expect("render profile panel");
        let path = std::path::PathBuf::from(dir).join(format!("profile_{tag}.png"));
        img.save(&path).expect("save profile snapshot");
    }
}

#[test]
fn profile_cover_and_members_paint_photo_userpics() {
    let source = Arc::new(RecordingSource::default());
    render_profile(mock_peers::USER_MIRA, "user", &source);
    render_profile(mock_peers::GROUP_CORE, "group", &source);
    let asked = source.asked.lock().map(|a| a.clone()).unwrap_or_default();
    assert!(
        asked.contains(&mock_peers::USER_MIRA.0),
        "the user profile cover asks for the peer's photo: {asked:?}"
    );
    assert!(
        asked.contains(&mock_peers::GROUP_CORE.0),
        "the group profile cover asks for the group's photo: {asked:?}"
    );
    assert!(
        asked.len() > 2,
        "the members list asks for each member's photo: {asked:?}"
    );
}
