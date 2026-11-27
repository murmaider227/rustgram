//! TGB-3010 evidence render: the main chat (chat list + message bubbles) and the open emoji panel
//! at 100% and 150% scale, so the emoji set can be compared with tdesktop's side by side.
//!
//! `TGB3010_EVIDENCE_DIR=<dir> cargo test -p tg-app --test all tgb3010_emoji_evidence -- --ignored --nocapture`

use egui::{Key, Modifiers, Vec2};
use egui_kittest::{Harness, HarnessBuilder};
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::MsgId;
use tg_app::window::section::SectionShow;
use tg_core::settings::{CoreSettings, PowerSavingFlags};

use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, click_at, press_key, settle_layout, step_frames, type_text,
    wait_until,
};

/// Sent first, so it shows in the bubbles and as the chat list preview.
const EMOJI_LINE: &str = "Emoji 😀😂🥰😎🤔😭👍🏽👋🏾❤️🔥🎉🚀💙🍀🇺🇦";

fn send_emoji_line(h: &mut Harness<'static, AppTestState>) {
    wait_until(h, DEFAULT_MAX_FRAMES, "compose bar", |h| {
        h.state_mut()
            .controller
            .main_widget_mut()
            .history_mut()
            .last_compose_rect()
            .is_positive()
    });
    let rect = h
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .last_compose_rect();
    click_at(h, rect.center());
    type_text(h, EMOJI_LINE);
    press_key(h, Modifiers::NONE, Key::Enter);
    settle_layout(h);
}

const SIZE: Vec2 = Vec2::new(1280.0, 768.0);

fn harness(ppp: f32) -> Harness<'static, AppTestState> {
    let mut state = AppTestState::main_workspace();
    state
        .controller
        .main_widget_mut()
        .apply_core_settings(&CoreSettings {
            loop_animated_stickers: false,
            power_saving: PowerSavingFlags::ALL,
            ..CoreSettings::default()
        });
    HarnessBuilder::default()
        .with_size(SIZE)
        .with_pixels_per_point(ppp)
        .with_render_cursor(false)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
            crate::common::apply_deterministic_style(ctx);
        })
        .build_ui_state(|ui, st: &mut AppTestState| st.show(ui), state)
}

fn settle(harness: &mut Harness<'static, AppTestState>) {
    settle_layout(harness);
    step_frames(harness, 35);
    // Emoji tiles decode on a worker; give them real time to arrive.
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        harness.step();
    }
    settle_layout(harness);
}

#[test]
#[ignore = "evidence render, run with TGB3010_EVIDENCE_DIR set"]
fn tgb3010_emoji_evidence() {
    let Some(dir) = std::env::var_os("TGB3010_EVIDENCE_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("evidence dir");
    for (ppp, tag) in [(1.0_f32, "100"), (1.5, "150")] {
        for panel in [false, true] {
            let mut h = harness(ppp);
            if let Some(session) = h.state_mut().controller.session_mut() {
                session.show_peer_history(
                    session_peers::GROUP_CORE,
                    SectionShow::instant_clear_stack(),
                    MsgId::SHOW_AT_END,
                );
            }
            send_emoji_line(&mut h);
            if panel {
                h.state_mut()
                    .controller
                    .main_widget_mut()
                    .tabbed_panel_mut()
                    .show_immediate();
            }
            settle(&mut h);
            let name = if panel { "panel" } else { "chat" };
            let out = dir.join(format!("{name}_{tag}.png"));
            h.render().expect("wgpu render").save(&out).expect("save");
            println!("wrote {}", out.display());
        }
    }
}
