//! End-to-end UI tests for WP-10 Peer Management Gaps:
//! - `EditInviteLinksBox`: list links, create link with expiry/usage/approval, edit, stats, revoke, delete revoked
//! - `JoinRequestsBox` + in-chat Join Requests bar (`HistoryWidget`): bar visibility, click to open, approve/dismiss
//! - `EditPeerColorBox`: Name vs Profile tabs, swatch & emoji selection, applying updates `MockSession`
//! - `OwnershipTransferBox`: confirm -> 2FA password step -> wrong password error -> valid password transfer
//! - `InfoWrapWidget`: Similar Channels tab & Downloads Manager tab

use egui::{Pos2, Rect, Vec2};
use egui_kittest::kittest::Queryable;
use tg_app::boxes::invite_links::{
    EditInviteLinksBoxState, ExpirePreset, InviteLinksView, UsageLimitPreset,
};
use tg_app::boxes::join_requests::JoinRequestsBoxState;
use tg_app::boxes::ownership_transfer::{OwnershipTransferBoxState, OwnershipTransferStep};
use tg_app::boxes::peer_color::{BACKGROUND_EMOJI_PRESETS, EditPeerColorBoxState, PeerColorTab};
use tg_app::boxes::{BoxHost, BoxKind};
use tg_app::history::HistoryWidget;
use tg_app::history::backend::HistoryBackend;
use tg_app::info::peer_mgmt_mock::DEMO_TRANSFER_2FA_PASSWORD;
use tg_app::info::{InfoBackend, InfoColors, InfoSection, InfoWrapWidget};
use tg_app::stand_in::mock_session::{MockSession, session_peers};
use tg_app::stand_in::strings::EnglishStrings;
use tg_app::stand_in::theme::{Palette, ThemeMode};

use crate::common::{build_widget_harness, init_headless_env};

struct BoxTestState {
    host: BoxHost,
    session: MockSession,
    colors: InfoColors,
    events: Vec<tg_app::boxes::BoxEvent>,
}

impl BoxTestState {
    fn new() -> Self {
        init_headless_env();
        tg_app::clock::init_test_clock();
        Self {
            host: BoxHost::new(),
            session: MockSession::new(true),
            colors: InfoColors::for_mode(ThemeMode::Day),
            events: Vec::new(),
        }
    }
}

fn build_box_harness(
    initial: BoxTestState,
    size: Vec2,
) -> egui_kittest::Harness<'static, BoxTestState> {
    build_widget_harness(initial, size, |ui, state| {
        let ctx = ui.ctx().clone();
        let bounds = Rect::from_min_size(Pos2::ZERO, ctx.content_rect().size());
        let evs = state
            .host
            .show(&ctx, bounds, &mut state.session, &state.colors);
        state.events.extend(evs);
    })
}

#[test]
fn e2e_edit_invite_links_box_create_edit_stats_revoke_delete() {
    let state = BoxTestState::new();
    let group_id = session_peers::GROUP_CORE;
    let initial_active = state
        .session
        .exported_chat_invites(group_id, false)
        .expect("active invites")
        .links
        .len();
    assert!(initial_active >= 2);

    let mut harness = build_box_harness(state, Vec2::new(900.0, 720.0));
    harness.step();

    // Push EditInviteLinksBox onto BoxHost
    let ctx = harness.ctx.clone();
    {
        let st = harness.state_mut();
        st.host.push(
            &ctx,
            BoxKind::EditInviteLinks(EditInviteLinksBoxState::new(group_id, "Rustgram Core Port")),
            &st.colors,
        );
    }
    for _ in 0..8 {
        harness.step();
    }

    // Click "+ Create a New Link" button in the rendered UI
    harness.get_by_label("+ Create a New Link").click();
    for _ in 0..4 {
        harness.step();
    }

    {
        let st = harness.state_mut();
        let top = st.host.top_kind().expect("top box");
        assert!(matches!(
            top,
            BoxKind::EditInviteLinks(EditInviteLinksBoxState {
                view: InviteLinksView::Editor(_),
                ..
            })
        ));
    }

    // Verify full create -> edit -> stats -> revoke -> delete_all_revoked lifecycle on MockSession
    let mut box_state = EditInviteLinksBoxState::new(group_id, "Rustgram Core Port");
    box_state.reload(&harness.state().session);
    assert!(!box_state.revoked_links.is_empty());

    // 1. Create a new link with expiry + usage limit
    box_state.open_create_form();
    if let InviteLinksView::Editor(ref mut form) = box_state.view {
        form.title = "Beta Testers".to_owned();
        form.expire_preset = ExpirePreset::OneDay;
        form.usage_preset = UsageLimitPreset::Ten;
    }
    let created = box_state
        .submit_editor(&mut harness.state_mut().session)
        .expect("create invite link");
    assert_eq!(created.title, "Beta Testers");
    assert_eq!(created.usage_limit, Some(10));
    assert!(created.expire_date.is_some());

    // 2. Edit the link to require admin approval (clears usage limit)
    box_state.open_edit_form(&created);
    if let InviteLinksView::Editor(ref mut form) = box_state.view {
        form.title = "Beta Approval".to_owned();
        form.request_needed = true;
    }
    let edited = box_state
        .submit_editor(&mut harness.state_mut().session)
        .expect("edit invite link");
    assert!(edited.request_needed);
    assert_eq!(edited.usage_limit, None);

    // 3. Open stats & importers view
    box_state.open_stats(edited.clone(), &harness.state().session);
    assert!(matches!(box_state.view, InviteLinksView::Stats { .. }));

    // 4. Revoke link and then delete all revoked links
    box_state
        .revoke_link(&edited.link, &mut harness.state_mut().session)
        .expect("revoke invite");
    assert!(
        box_state
            .revoked_links
            .iter()
            .any(|l| l.link == edited.link)
    );

    box_state
        .delete_all_revoked(&mut harness.state_mut().session)
        .expect("delete all revoked");
    assert!(box_state.revoked_links.is_empty());
}

#[test]
fn e2e_join_requests_bar_in_chat_and_approval_box() {
    init_headless_env();
    tg_app::clock::init_test_clock();

    struct ChatBarState {
        history: HistoryWidget,
        session: MockSession,
        palette: Palette,
        strings: EnglishStrings,
    }

    let group_id = session_peers::GROUP_CORE;
    let initial = ChatBarState {
        history: HistoryWidget::new(),
        session: MockSession::new(true),
        palette: Palette::for_mode(ThemeMode::Day),
        strings: EnglishStrings,
    };
    assert_eq!(
        HistoryBackend::join_requests_bar(&initial.session, group_id)
            .expect("GROUP_CORE has demo join requests")
            .count,
        2
    );

    let mut harness = build_widget_harness(initial, Vec2::new(720.0, 640.0), move |ui, state| {
        egui::CentralPanel::default().show(ui, |ui| {
            let _ = state.history.show_with_backend(
                ui,
                group_id,
                &mut state.session,
                false,
                false,
                true,
                &state.palette,
                &state.strings,
            );
        });
    });
    for _ in 0..4 {
        harness.step();
    }

    // The in-chat join-requests bar should be visible and labeled "2 requests to join"
    harness.get_by_label("2 requests to join").click();
    for _ in 0..4 {
        harness.step();
    }

    // Clicking the bar opened the inline Join Requests modal; approve Avery Quinn via UI button!
    harness.get_by_label("Add to Group Avery Quinn").click();
    for _ in 0..4 {
        harness.step();
    }

    let after_one = HistoryBackend::join_requests_bar(&harness.state().session, group_id)
        .expect("1 request remaining");
    assert_eq!(after_one.count, 1);

    // Dismiss the remaining request via "Dismiss All"
    harness.get_by_label("Dismiss All").click();
    for _ in 0..4 {
        harness.step();
    }
    assert!(
        HistoryBackend::join_requests_bar(&harness.state().session, group_id).is_none(),
        "join-requests bar must disappear once all requests are handled"
    );

    // Also verify JoinRequestsBox hosted inside BoxHost
    let mut box_harness = build_box_harness(BoxTestState::new(), Vec2::new(800.0, 640.0));
    let ctx = box_harness.ctx.clone();
    {
        let st = box_harness.state_mut();
        st.host.push(
            &ctx,
            BoxKind::JoinRequests(JoinRequestsBoxState::new(group_id, true)),
            &st.colors,
        );
    }
    for _ in 0..6 {
        box_harness.step();
    }
    box_harness.get_by_label("Approve All").click();
    for _ in 0..4 {
        box_harness.step();
    }
    assert_eq!(
        box_harness
            .state()
            .session
            .join_requests_summary(group_id)
            .count,
        0
    );
}

#[test]
fn e2e_edit_peer_color_box_name_and_profile_tabs() {
    let channel_id = session_peers::CHANNEL_DEV_LOG;
    let mut harness = build_box_harness(BoxTestState::new(), Vec2::new(860.0, 720.0));
    harness.step();

    let ctx = harness.ctx.clone();
    {
        let st = harness.state_mut();
        let settings = st.session.peer_color_settings(channel_id);
        st.host.push(
            &ctx,
            BoxKind::EditPeerColor(EditPeerColorBoxState::new(
                channel_id,
                "Rustgram Dev Log",
                settings,
            )),
            &st.colors,
        );
    }
    for _ in 0..8 {
        harness.step();
    }

    // Switch to Profile tab, pick Color 9, and pick the Bolt emoji background preset
    harness.get_by_label("Profile").click();
    harness.step();
    harness.get_by_label("Color 9").click();
    harness.step();
    let (bolt_id, _glyph, lbl) = BACKGROUND_EMOJI_PRESETS[3];
    harness.get_by_label(lbl).click();
    harness.step();

    // Apply profile color update into MockSession
    let mut color_box = EditPeerColorBoxState::new(
        channel_id,
        "Rustgram Dev Log",
        harness.state().session.peer_color_settings(channel_id),
    );
    color_box.set_tab(PeerColorTab::Profile);
    color_box.profile_color_index = Some(9);
    color_box.set_current_emoji_id(bolt_id);
    color_box
        .submit(&mut harness.state_mut().session)
        .expect("apply profile color");

    let saved = harness.state().session.peer_color_settings(channel_id);
    assert_eq!(saved.profile_color_index, Some(9));
    assert_eq!(saved.profile_background_emoji_id, bolt_id);
}

#[test]
fn e2e_ownership_transfer_box_wrong_and_valid_2fa_password() {
    let group_id = session_peers::GROUP_CORE;
    let successor = session_peers::USER_MIRA;

    let mut harness = build_box_harness(BoxTestState::new(), Vec2::new(860.0, 640.0));
    harness.step();

    let ctx = harness.ctx.clone();
    {
        let st = harness.state_mut();
        st.host.push(
            &ctx,
            BoxKind::OwnershipTransfer(OwnershipTransferBoxState::new(
                group_id,
                "Rustgram Core Port",
                false,
                successor,
                "Mira Vance",
            )),
            &st.colors,
        );
    }
    for _ in 0..8 {
        harness.step();
    }

    // Drive full flow through OwnershipTransferBoxState + MockSession
    let mut transfer = OwnershipTransferBoxState::new(
        group_id,
        "Rustgram Core Port",
        false,
        successor,
        "Mira Vance",
    );
    transfer.run_precheck(&harness.state().session);
    assert_eq!(transfer.step, OwnershipTransferStep::Confirm);
    assert!(!transfer.password_hint.is_empty());

    transfer.proceed_to_password();
    assert_eq!(transfer.step, OwnershipTransferStep::EnterPassword);

    // Wrong password produces inline error and stays on EnterPassword
    transfer.password = "bad-password".to_owned();
    assert!(
        transfer
            .submit_password(&mut harness.state_mut().session)
            .is_err()
    );
    assert!(transfer.error.is_some());
    assert_eq!(transfer.step, OwnershipTransferStep::EnterPassword);

    // Valid demo 2FA password transfers ownership cleanly
    transfer.password = DEMO_TRANSFER_2FA_PASSWORD.to_owned();
    transfer
        .submit_password(&mut harness.state_mut().session)
        .expect("valid 2FA password transfers ownership");
    assert_eq!(transfer.step, OwnershipTransferStep::Completed);
    assert!(transfer.error.is_none());

    let members = harness.state().session.peer_members(group_id, "");
    let mira = members
        .iter()
        .find(|m| m.peer_id == successor)
        .expect("Mira is a member");
    assert!(
        matches!(mira.role, tg_app::info::MemberRole::Owner { .. }),
        "Mira must become the group owner after transfer"
    );
}

#[test]
fn e2e_info_similar_channels_and_downloads_manager_tabs() {
    init_headless_env();
    tg_app::clock::init_test_clock();

    struct InfoTabState {
        wrap: InfoWrapWidget,
        session: MockSession,
        colors: InfoColors,
        events: Vec<tg_app::info::InfoWrapEvent>,
    }

    let chan_id = session_peers::CHANNEL_DEV_LOG;
    let initial = InfoTabState {
        wrap: InfoWrapWidget::for_peer(chan_id),
        session: MockSession::new(true),
        colors: InfoColors::for_mode(ThemeMode::Day),
        events: Vec::new(),
    };

    let mut harness = build_widget_harness(initial, Vec2::new(420.0, 760.0), |ui, state| {
        egui::CentralPanel::default().show(ui, |ui| {
            let evs = state
                .wrap
                .show(ui, &mut state.session, false, &state.colors);
            state.events.extend(evs);
        });
    });
    for _ in 0..4 {
        harness.step();
    }

    // 1. Open Similar Channels tab
    harness.state_mut().wrap.open_similar_channels();
    for _ in 0..4 {
        harness.step();
    }
    assert_eq!(
        harness.state().wrap.current_section(),
        InfoSection::SimilarChannels { peer_id: chan_id }
    );
    // Click a recommended channel ("Nightly Builds") to navigate to its profile
    harness.get_by_label("Nightly Builds").click();
    for _ in 0..4 {
        harness.step();
    }
    assert_eq!(
        harness.state().wrap.current_section().peer_id(),
        session_peers::CHANNEL_RELEASES
    );

    // 2. Open Downloads Manager tab
    harness.state_mut().wrap.open_downloads();
    for _ in 0..4 {
        harness.step();
    }
    assert!(matches!(
        harness.state().wrap.current_section(),
        InfoSection::Downloads { .. }
    ));
    let before_dl = harness.state().session.downloads_list("").len();
    assert!(before_dl >= 3);

    // Pause active download via UI button
    harness
        .get_by_label("Pause architecture_layer233.pdf")
        .click();
    for _ in 0..3 {
        harness.step();
    }
    let after_pause = harness.state().session.downloads_list("architecture");
    assert!(matches!(
        after_pause[0].status,
        tg_app::info::DownloadStatus::Paused { .. }
    ));

    // Clear completed downloads via UI button
    harness.get_by_label("Clear Completed").click();
    for _ in 0..3 {
        harness.step();
    }
    let remaining = harness.state().session.downloads_list("");
    assert!(
        remaining
            .iter()
            .all(|d| !matches!(d.status, tg_app::info::DownloadStatus::Completed))
    );
}
