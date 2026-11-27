//! UI parity stage 2, Package E: before/after screenshot capture of the Emoji / Stickers / GIFs
//! panel (`ChatHelpers::TabbedPanel`) in a 1:1 chat at 1440x900 and 390x844, Day and Night.
//!
//! Run with `RUSTGRAM_SNAPSHOT_DIR=<dir>` and `RUSTGRAM_UIP2_E_SNAP_STAGE=before|after`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use egui::Vec2;
use tg_app::chat_helpers::tabbed_selector::SelectorTab;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::MsgId;
use tg_app::window::section::SectionShow;

use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, apply_deterministic_style, build_wgpu_harness, settle_layout,
    step_frames, wait_until,
};

/// State of the standalone docked-column capture (`TabbedSection`, 292 px third column).
struct DockedState {
    panel: tg_app::chat_helpers::TabbedPanel,
    backend: tg_app::chat_helpers::MockChatHelpersBackend,
    emoji_cache: tg_emoji::EmojiTextureCache,
    palette: tg_app::stand_in::theme::Palette,
}

fn docked_harness(tab: SelectorTab) -> egui_kittest::Harness<'static, DockedState> {
    let mut panel = tg_app::chat_helpers::TabbedPanel::new();
    panel.set_active_tab(tab);
    panel.show_immediate();
    let state = DockedState {
        panel,
        backend: tg_app::chat_helpers::MockChatHelpersBackend::new(),
        emoji_cache: tg_emoji::EmojiTextureCache::new(),
        palette: tg_app::stand_in::theme::Palette::day(),
    };
    egui_kittest::HarnessBuilder::default()
        .with_size(Vec2::new(
            tg_app::chat_helpers::tabbed_panel::TABBED_SECTION_WIDTH,
            856.0,
        ))
        .with_step_dt(1.0 / 30.0)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .renderer(crate::common::serialized_wgpu_renderer())
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
        .build_ui_state(
            |ui, st: &mut DockedState| {
                // kittest wraps the closure in a margined `Frame::central_panel`; the docked
                // column fills the whole 292 px viewport instead.
                ui.ctx().set_visuals(egui::Visuals::light());
                let rect = ui.ctx().content_rect();
                let _ = st.panel.show_docked(
                    ui,
                    rect,
                    &mut st.backend,
                    &mut st.emoji_cache,
                    &st.palette,
                    |_| {},
                );
            },
            state,
        )
}

static WGPU_UIP2_E_LOCK: Mutex<()> = Mutex::new(());

fn save_harness_png(harness: &mut egui_kittest::Harness<'static, AppTestState>, path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create snap dir");
    }
    let img = harness.render().expect("wgpu render");
    img.save(path).expect("save png");
}

fn open_panel(
    size: Vec2,
    tab: SelectorTab,
    night: bool,
) -> egui_kittest::Harness<'static, AppTestState> {
    let mut harness = build_wgpu_harness(AppTestState::main_workspace(), size);
    settle_layout(&mut harness);
    if night {
        harness.state_mut().controller.toggle_night_mode();
        settle_layout(&mut harness);
    }
    if let Some(session) = harness.state_mut().controller.session_mut() {
        session.show_peer_history(
            session_peers::USER_THEO,
            SectionShow::clear_stack(),
            MsgId::SHOW_AT_END,
        );
    }
    settle_layout(&mut harness);
    {
        let panel = harness
            .state_mut()
            .controller
            .main_widget_mut()
            .tabbed_panel_mut();
        panel.show_immediate();
        panel.set_active_tab(tab);
    }
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "the tabbed panel must be open",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .tabbed_panel_mut()
                .is_open()
        },
    );
    // Sticker previews are rasterized off the UI thread: give the workers real time to
    // deliver the first frames, stepping the UI so results are uploaded.
    for _ in 0..40 {
        step_frames(&mut harness, 1);
        std::thread::sleep(Duration::from_millis(25));
    }
    step_frames(&mut harness, 2);
    harness
}

#[test]
#[ignore = "captures before/after design review PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_uip2_e_snapshots() {
    let _guard = WGPU_UIP2_E_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stage = std::env::var("RUSTGRAM_UIP2_E_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned());
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("uip2_e"));

    let screens = [
        ("06_emoji_panel", SelectorTab::Emoji, false),
        ("07_sticker_panel", SelectorTab::Stickers, false),
        ("08_gif_panel", SelectorTab::Gifs, false),
        ("28_night_emoji_panel", SelectorTab::Emoji, true),
    ];
    for (size_label, size) in [
        ("1440x900", Vec2::new(1440.0, 900.0)),
        ("390x844", Vec2::new(390.0, 844.0)),
    ] {
        for (name, tab, night) in screens {
            let mut harness = open_panel(size, tab, night);
            save_harness_png(
                &mut harness,
                &dir.join(format!("{name}_{size_label}_{stage}.png")),
            );
        }
    }

    // Docked third-column mode (rendered standalone until the window shell hosts it).
    if stage == "after" {
        for (name, tab) in [
            ("docked_emoji", SelectorTab::Emoji),
            ("docked_stickers", SelectorTab::Stickers),
            ("docked_gifs", SelectorTab::Gifs),
        ] {
            let mut h = docked_harness(tab);
            for _ in 0..40 {
                h.step();
                std::thread::sleep(Duration::from_millis(25));
            }
            h.step();
            let img = h.render().expect("wgpu render");
            std::fs::create_dir_all(&dir).expect("create snap dir");
            img.save(dir.join(format!("{name}_292x856_{stage}.png")))
                .expect("save png");
        }
    }
}
