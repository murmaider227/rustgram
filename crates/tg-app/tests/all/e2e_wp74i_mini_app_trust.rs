//! WP-75 + WP-74: Mini App consent goes through the account's persisted trust
//! (`Storage::Account::isPeerTrustedOpenWebView` / `markPeerTrustedOpenWebView`) instead of a
//! per-window set, so a confirmed bot opens without the box again after a restart.

use egui::Vec2;
use egui_kittest::kittest::Queryable;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_app::window::section::SectionShow;

use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, build_harness, settle_layout, wait_for_label_settled,
    wait_until,
};

const WORKSPACE: Vec2 = Vec2::new(1280.0, 820.0);

fn open_peer(harness: &mut egui_kittest::Harness<'_, AppTestState>, peer_id: PeerId) {
    harness
        .state_mut()
        .controller
        .session_mut()
        .expect("session")
        .show_peer_history(peer_id, SectionShow::clear_stack(), MsgId::SHOW_AT_END);
    wait_until(
        harness,
        DEFAULT_MAX_FRAMES,
        "switching active peer did not settle",
        |h| {
            !h.state().controller.is_animating()
                && h.state_mut()
                    .controller
                    .main_widget_mut()
                    .history_mut()
                    .active_peer()
                    == peer_id
        },
    );
    settle_layout(harness);
}

fn requests(h: &egui_kittest::Harness<'_, AppTestState>) -> usize {
    h.state()
        .controller
        .mock_session()
        .webview_requests_log()
        .len()
}

fn untrusted_catalog_bot() -> egui_kittest::Harness<'static, AppTestState> {
    let mut h = build_harness(AppTestState::main_with_webview(), WORKSPACE);
    settle_layout(&mut h);
    h.state_mut()
        .controller
        .mock_session_mut()
        .set_peer_verified(session_peers::BOT_CATALOG, false);
    open_peer(&mut h, session_peers::BOT_CATALOG);
    h
}

#[test]
fn wp74i_mini_app_trust_is_saved_to_the_account_and_survives_a_reload() {
    let bot = session_peers::BOT_CATALOG;

    // Session 1: consent box, Launch -> trust is written to the account backend.
    let mut h = untrusted_catalog_bot();
    assert!(
        !h.state()
            .controller
            .mock_session()
            .handle()
            .is_mini_app_bot_trusted(bot)
    );
    let before = requests(&h);
    h.get_by_label("Open Store App").click();
    wait_until(&mut h, DEFAULT_MAX_FRAMES, "consent box first", |h| {
        matches!(
            h.state_mut().controller.box_host_mut().top_kind(),
            Some(tg_app::boxes::BoxKind::MiniAppOpenConfirm(_))
        )
    });
    let launch = tg_lang::tr::lng_view_button_bot_app();
    wait_for_label_settled(&mut h, &launch);
    h.get_by_label(&launch).click();
    wait_until(
        &mut h,
        DEFAULT_MAX_FRAMES,
        "Launch opens the Mini App",
        |h| requests(h) == before + 1,
    );
    assert!(
        h.state()
            .controller
            .mock_session()
            .handle()
            .is_mini_app_bot_trusted(bot),
        "Launch must persist the trust (markPeerTrustedOpenWebView)"
    );

    // Session 2: a fresh window (empty in-memory set) whose account storage already holds the
    // trust, as after a restart -> the Mini App opens with no box.
    let mut h = untrusted_catalog_bot();
    h.state()
        .controller
        .mock_session()
        .handle()
        .trust_mini_app_bot(bot);
    assert!(h.state().controller.is_peer_trusted_open_webview(bot));
    let before = requests(&h);
    h.get_by_label("Open Store App").click();
    wait_until(
        &mut h,
        DEFAULT_MAX_FRAMES,
        "a bot trusted in an earlier session opens directly",
        |h| requests(h) == before + 1,
    );
    assert!(h.state().controller.box_host().is_empty());
}
