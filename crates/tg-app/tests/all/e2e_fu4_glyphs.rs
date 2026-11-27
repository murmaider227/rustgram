//! Integration tests and before/after visual snapshot capture for Follow-Up 4 (`own_fu4`).
//!
//! Covers:
//! - Translate bar options menu ("Do not translate languages:" chips) rendering a painted
//!   `st::menu_icon_select` checkmark instead of the missing U+2713 glyph.
//! - Headless `egui_kittest` verification WITHOUT `.allow_missing_glyphs()` so missing glyphs
//!   fail loudly.

use std::path::Path;
use std::sync::Mutex;

use egui::Vec2;
use egui_kittest::kittest::Queryable;
use tg_app::stand_in::Backend;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::MsgId;
use tg_app::window::section::SectionShow;
use tg_core::settings::{CoreSettings, PowerSavingFlags};

use crate::common::{AppTestState, build_wgpu_harness, settle_layout, step_frames, wait_for_label};

const DESKTOP_SIZE: Vec2 = Vec2::new(1440.0, 900.0);
const MOBILE_SIZE: Vec2 = Vec2::new(390.0, 844.0);

static WGPU_FU4_LOCK: Mutex<()> = Mutex::new(());

fn save_harness_png(harness: &mut egui_kittest::Harness<'static, AppTestState>, path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create snap dir");
    }
    let img = harness.render().expect("wgpu render");
    img.save(path).expect("save png");
}

fn make_static_state() -> AppTestState {
    let mut state = AppTestState::main_workspace();
    let static_settings = CoreSettings {
        loop_animated_stickers: false,
        power_saving: PowerSavingFlags::ALL,
        ..CoreSettings::default()
    };
    state
        .controller
        .main_widget_mut()
        .apply_core_settings(&static_settings);
    state
}

fn build_snap_harness(size: Vec2) -> egui_kittest::Harness<'static, AppTestState> {
    let mut harness = build_wgpu_harness(make_static_state(), size);
    settle_layout(&mut harness);
    step_frames(&mut harness, 35);
    harness
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .request_scroll_to_bottom(session_peers::GROUP_CORE);
    settle_layout(&mut harness);
    harness
}

fn open_russian_dm_with_translate_bar(harness: &mut egui_kittest::Harness<'static, AppTestState>) {
    let _ = Backend::send_message(
        harness.state_mut().controller.session_backend_mut(),
        session_peers::USER_RILEY,
        "Привет! Проверяем автоматический перевод сообщений в этом чате и меню параметров перевода. Добро пожаловать на тестирование интерфейса.",
    );
    if let Some(session) = harness.state_mut().controller.session_mut() {
        session.show_peer_history(
            session_peers::USER_RILEY,
            SectionShow::clear_stack(),
            MsgId::SHOW_AT_END,
        );
    }
    settle_layout(harness);
    step_frames(harness, 10);
}

#[test]
#[ignore = "captures before/after design review PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_fu4_snapshots() {
    let _guard = WGPU_FU4_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stage = std::env::var("RUSTGRAM_FU4_SNAP_STAGE")
        .or_else(|_| std::env::var("TGFORK_FU4_SNAP_STAGE"))
        .unwrap_or_else(|_| "after".to_owned());
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("fu4"));

    for (tag, size) in [("1440x900", DESKTOP_SIZE), ("390x844", MOBILE_SIZE)] {
        let mut h = build_snap_harness(size);
        open_russian_dm_with_translate_bar(&mut h);
        wait_for_label(&mut h, "Translation options");
        h.get_by_label("Translation options").click();
        settle_layout(&mut h);
        save_harness_png(
            &mut h,
            &dir.join(format!("translate_bar_menu_{tag}_{stage}.png")),
        );
    }
}

#[test]
fn translate_bar_menu_and_replaced_ui_icons_render_without_missing_glyphs() {
    use egui::{Pos2, Rect};
    use egui_kittest::HarnessBuilder;
    use tg_app::history::compose_extras::{
        TranslateBarAction, TranslateBarState, VoiceRecorderController, paint_voice_recording_bar,
    };
    use tg_app::history::theme::HistoryColors;
    use tg_ui::fonts::register_tdesktop_fonts;

    struct State {
        bar: TranslateBarState,
        do_not_translate: Vec<String>,
        actions: Vec<TranslateBarAction>,
        voice: VoiceRecorderController,
    }

    let mut voice = VoiceRecorderController::new();
    voice
        .start_hold(0.0)
        .expect("headless runs record from the synthetic source");

    let initial = State {
        bar: TranslateBarState {
            menu_open: true,
            manually_shown: true,
            ..TranslateBarState::default()
        },
        do_not_translate: vec!["en".to_owned()],
        actions: Vec::new(),
        voice,
    };

    // Intentionally omit `.allow_missing_glyphs()` so ANY missing glyph panics.
    let mut harness = HarnessBuilder::default()
        .with_size(Vec2::new(480.0, 520.0))
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
        })
        .build_ui_state(
            |ui, st: &mut State| {
                let colors = HistoryColors::from_palette(&tg_app::stand_in::theme::Palette::day());
                let bar_rect = Rect::from_min_size(Pos2::new(8.0, 8.0), Vec2::new(464.0, 36.0));
                if let Some(act) = st.bar.show(
                    ui,
                    bar_rect,
                    Some("ru"),
                    "en",
                    &st.do_not_translate,
                    &colors,
                ) {
                    st.actions.push(act);
                }
                let voice_rect = Rect::from_min_size(Pos2::new(8.0, 440.0), Vec2::new(464.0, 44.0));
                let _ = paint_voice_recording_bar(ui, voice_rect, &st.voice, 1.5, &colors);
            },
            initial,
        );

    harness.step();
    harness.step();

    // Clicking the checked "English" chip under "Do not translate languages:" emits RemoveDoNotTranslate("en").
    let english_nodes: Vec<_> = harness.get_all_by_label("English").collect();
    assert!(
        english_nodes.len() >= 2,
        "expected English chip in both Translate to and Do not translate sections"
    );
    // The second "English" chip is the one under "Do not translate languages:".
    english_nodes[1].click();
    harness.step();
    assert_eq!(
        harness.state().actions.last(),
        Some(&TranslateBarAction::RemoveDoNotTranslate("en".to_owned()))
    );
}
