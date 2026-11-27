//! TGB-3048 evidence render: emoji inside text (a message bubble, the chat-list preview, a chat
//! name and a compose-field draft) at 100% and 150%, to compare with tdesktop side by side.
//!
//! `TGB3048_EVIDENCE_DIR=<dir> cargo test -p tg-app --test all tgb3048_emoji_text_evidence -- --ignored --nocapture`

use egui::{Key, Modifiers, Vec2};
use egui_kittest::{Harness, HarnessBuilder};
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::MsgId;
use tg_app::window::section::SectionShow;
use tg_app_core::info::backend::{InfoBackend, UpdatePeerInfoRequest};
use tg_core::settings::{CoreSettings, PowerSavingFlags};

use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, click_at, press_key, settle_layout, step_frames, type_text,
    wait_until,
};

const SENT: &str = "Emoji 😀😂❤️🔥👍🏽🇺🇦 in a message";
const DRAFT: &str = "Draft 😀❤️🇺🇦";
const TITLE: &str = "Core Team 🚀🇺🇦";
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
    let mut handle = state.controller.session_handle();
    InfoBackend::update_peer_info(
        &mut handle,
        session_peers::GROUP_CORE,
        UpdatePeerInfoRequest {
            title: TITLE.to_owned(),
            description: String::new(),
            is_public: false,
            username: None,
            no_forwards: false,
        },
    )
    .expect("rename the group");
    HarnessBuilder::default()
        .with_size(SIZE)
        .with_pixels_per_point(ppp)
        .with_render_cursor(false)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .renderer(crate::common::serialized_wgpu_renderer())
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
            crate::common::apply_deterministic_style(ctx);
        })
        .build_ui_state(|ui, st: &mut AppTestState| st.show(ui), state)
}

fn compose(h: &mut Harness<'static, AppTestState>, text: &str, send: bool) {
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
    type_text(h, text);
    if send {
        press_key(h, Modifiers::NONE, Key::Enter);
    }
    settle_layout(h);
}

#[test]
#[ignore = "evidence render, run with TGB3048_EVIDENCE_DIR set"]
fn tgb3048_emoji_text_evidence() {
    let Some(dir) = std::env::var_os("TGB3048_EVIDENCE_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("evidence dir");
    for (ppp, tag) in [(1.0_f32, "100"), (1.5, "150")] {
        let mut h = harness(ppp);
        if let Some(session) = h.state_mut().controller.session_mut() {
            session.show_peer_history(
                session_peers::GROUP_CORE,
                SectionShow::instant_clear_stack(),
                MsgId::SHOW_AT_END,
            );
        }
        compose(&mut h, SENT, true);
        compose(&mut h, DRAFT, false);
        settle_layout(&mut h);
        step_frames(&mut h, 35);
        settle_layout(&mut h);
        let out = dir.join(format!("chat_{tag}.png"));
        h.render().expect("wgpu render").save(&out).expect("save");
        println!("wrote {}", out.display());
    }
}
