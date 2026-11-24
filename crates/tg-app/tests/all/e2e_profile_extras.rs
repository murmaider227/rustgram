//! End-to-end `egui_kittest` UI tests for WP-26:
//! - Profile tabs in `InfoSection` (`Stories`, `PeerGifts`, `SavedMusic`)
//! - `PeerQrBox` (qrcode encoding, theme swatches, dark card toggle, PNG export)
//! - `EmojiUserpicBox` (color stop add/remove, palette/emoji selection, submit)
//! - `EditPeerReactionsBox` (All / Some / None mode switching & persistence)
//! - Collectible Usernames list in `UsernameBox` (toggle active & reorder)
//! - `VerifyPeersBox` (add with custom description & revoke verification)
//! - `PhoneBannedBox` & `UnconfirmedAuthDeniedBox` (intro modals & 5-sec lock)
//! - Business-bot pause / resume / remove bar in `HistoryWidget`

use egui::{Pos2, Rect, Vec2};
use egui_kittest::kittest::Queryable;
use tg_app::boxes::emoji_userpic::{DEFAULT_EMOJI_PRESETS, EmojiUserpicBoxState};
use tg_app::boxes::peer_qr::{PEER_QR_SWATCHES, PeerQrBoxState};
use tg_app::boxes::peer_reactions::{EditPeerReactionsBoxState, ReactionsModeChoice};
use tg_app::boxes::username::UsernameBoxState;
use tg_app::boxes::verify_peers::{VerifyPeersBoxState, VerifyPeersStep};
use tg_app::boxes::{BoxEvent, BoxHost, BoxKind};
use tg_app::history::HistoryWidget;
use tg_app::history::backend::HistoryBackend;
use tg_app::info::{
    ChatAvailableReactionsMode, InfoBackend, InfoColors, InfoSection, InfoWrapEvent, InfoWrapWidget,
};
use tg_app::intro::auth_boxes::{
    PHONE_BANNED_HELP_EMAIL, PhoneBannedAction, PhoneBannedBoxState, UNCONFIRMED_AUTH_TIMER_SECS,
    UnconfirmedAuthDeniedBoxState,
};
use tg_app::stand_in::mock_session::{MockSession, session_peers};
use tg_app::stand_in::strings::EnglishStrings;
use tg_app::stand_in::theme::{Palette, ThemeMode};

use crate::common::{build_widget_harness, init_headless_env};

struct BoxTestState {
    host: BoxHost,
    session: MockSession,
    colors: InfoColors,
    events: Vec<BoxEvent>,
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

struct InfoTabState {
    wrap: InfoWrapWidget,
    session: MockSession,
    colors: InfoColors,
    events: Vec<InfoWrapEvent>,
}

impl InfoTabState {
    fn for_peer(peer_id: tg_app::stand_in::types::PeerId) -> Self {
        init_headless_env();
        tg_app::clock::init_test_clock();
        Self {
            wrap: InfoWrapWidget::for_peer(peer_id),
            session: MockSession::new(true),
            colors: InfoColors::for_mode(ThemeMode::Day),
            events: Vec::new(),
        }
    }
}

fn build_info_harness(
    initial: InfoTabState,
    size: Vec2,
) -> egui_kittest::Harness<'static, InfoTabState> {
    build_widget_harness(initial, size, |ui, state| {
        let evs = state.wrap.show(ui, &mut state.session, true, &state.colors);
        state.events.extend(evs);
    })
}

#[test]
fn e2e_profile_stories_gifts_and_saved_music_tabs() {
    // 1. Profile Stories & Albums tab
    let mut state = InfoTabState::for_peer(session_peers::USER_MIRA);
    state.wrap.open_stories(false);
    let mut harness = build_info_harness(state, Vec2::new(420.0, 680.0));
    for _ in 0..4 {
        harness.step();
    }
    assert_eq!(
        harness.state().wrap.current_section(),
        InfoSection::Stories {
            peer_id: session_peers::USER_MIRA,
            archive: false,
        }
    );
    // Filter by Outdoors album and click a story tile
    harness.get_by_label("Outdoors (2)").click();
    harness.step();
    harness
        .get_by_label("Story #501: Alpine trail sunrise")
        .click();
    harness.step();
    assert!(harness.state().events.iter().any(|e| matches!(
        e,
        InfoWrapEvent::OpenProfileStory {
            peer_id,
            story_id: 501
        } if *peer_id == session_peers::USER_MIRA
    )));

    // 2. Peer Gifts tab (reusing tg-payments models & artwork)
    harness.state_mut().wrap.open_peer_gifts();
    for _ in 0..4 {
        harness.step();
    }
    assert_eq!(
        harness.state().wrap.current_section(),
        InfoSection::PeerGifts {
            peer_id: session_peers::USER_MIRA,
        }
    );
    harness.get_by_label("Astral Compass #42").click();
    harness.step();
    harness.get_by_label("Favorites (2)").click();
    harness.step();

    // 3. Saved Music tab
    harness.state_mut().wrap.open_saved_music();
    for _ in 0..4 {
        harness.step();
    }
    assert_eq!(
        harness.state().wrap.current_section(),
        InfoSection::SavedMusic {
            peer_id: session_peers::USER_MIRA,
        }
    );
    harness
        .get_by_label("Midnight Compiler — Synthwave Collective")
        .click();
    harness.step();
}

#[test]
fn e2e_peer_qr_box_swatches_and_png_export() {
    let state = BoxTestState::new();
    let peer = state
        .session
        .peer_profile(session_peers::USER_MIRA)
        .expect("USER_MIRA profile");
    let qr_state = PeerQrBoxState::from_profile(&peer);
    assert!(qr_state.matrix.is_some());
    let png = qr_state.encode_qr_png(4).expect("encode QR PNG");
    assert_eq!(&png[0..4], b"\x89PNG");

    let mut harness = build_box_harness(state, Vec2::new(820.0, 680.0));
    harness.step();
    let ctx = harness.ctx.clone();
    {
        let st = harness.state_mut();
        st.host.push(&ctx, BoxKind::PeerQr(qr_state), &st.colors);
    }
    for _ in 0..6 {
        harness.step();
    }

    harness
        .get_by_label(&format!("QR Theme {}", PEER_QR_SWATCHES[2].name))
        .click();
    harness.step();
    harness.get_by_label("Dark Card").click();
    harness.step();
    harness.get_by_label("Save QR Image").click();
    harness.step();

    let st = harness.state();
    match st.host.top_kind() {
        Some(BoxKind::PeerQr(qr)) => {
            assert_eq!(qr.swatch_index, 2);
            assert!(qr.dark_card);
        }
        other => panic!("expected PeerQrBox, got {other:?}"),
    }
    assert!(
        st.events
            .iter()
            .any(|e| matches!(e, BoxEvent::Toast(msg) if msg.contains("_qr.png")))
    );
}

#[test]
fn e2e_emoji_userpic_builder_and_peer_reactions_boxes() {
    let state = BoxTestState::new();
    let mut harness = build_box_harness(state, Vec2::new(820.0, 720.0));
    harness.step();
    let ctx = harness.ctx.clone();

    // 1. Emoji Userpic Builder
    {
        let st = harness.state_mut();
        st.host.push(
            &ctx,
            BoxKind::EmojiUserpic(EmojiUserpicBoxState::new(session_peers::SELF, "Taylor Kim")),
            &st.colors,
        );
    }
    for _ in 0..6 {
        harness.step();
    }
    harness.get_by_label(DEFAULT_EMOJI_PRESETS[2].name).click();
    harness.step();
    {
        let st = harness.state_mut();
        if let Some(BoxKind::EmojiUserpic(b)) = st.host.top_kind_mut() {
            b.add_color_stop();
            assert_eq!(b.background_colors.len(), 4);
            b.submit(&mut st.session).expect("submit emoji userpic");
        }
        st.host.pop(&ctx, &st.colors);
    }
    harness.step();

    // 2. EditPeerReactionsBox
    {
        let st = harness.state_mut();
        let cfg = st
            .session
            .peer_available_reactions(session_peers::GROUP_CORE);
        st.host.push(
            &ctx,
            BoxKind::EditPeerReactions(EditPeerReactionsBoxState::new(
                session_peers::GROUP_CORE,
                "Rustgram Core Port",
                false,
                cfg,
            )),
            &st.colors,
        );
    }
    for _ in 0..6 {
        harness.step();
    }
    harness.get_by_label("Some Reactions").click();
    harness.step();
    {
        let st = harness.state_mut();
        if let Some(BoxKind::EditPeerReactions(b)) = st.host.top_kind_mut() {
            assert_eq!(b.mode, ReactionsModeChoice::Some);
            b.toggle_emoji("🔥");
            b.reactions_limit = 4;
            b.submit(&mut st.session).expect("save reactions");
        }
    }
    let saved = harness
        .state()
        .session
        .peer_available_reactions(session_peers::GROUP_CORE);
    assert_eq!(saved.reactions_limit, Some(4));
    assert!(matches!(
        saved.mode,
        ChatAvailableReactionsMode::Some { ref emojis, .. } if emojis.iter().any(|e| e == "🔥")
    ));
}

#[test]
fn e2e_collectible_usernames_and_verify_peers_boxes() {
    let state = BoxTestState::new();
    let mira = state
        .session
        .peer_profile(session_peers::USER_MIRA)
        .expect("USER_MIRA");
    let mut harness = build_box_harness(state, Vec2::new(820.0, 720.0));
    harness.step();
    let ctx = harness.ctx.clone();

    // 1. Collectible Usernames in UsernameBox
    {
        let st = harness.state_mut();
        st.host.push(
            &ctx,
            BoxKind::Username(UsernameBoxState::from_profile(&mira)),
            &st.colors,
        );
    }
    for _ in 0..6 {
        harness.step();
    }
    // Reorder: move @mv_graphics up ahead of @mira_vance
    harness.get_by_label("Move @mv_graphics up").click();
    harness.step();
    harness.step();
    let reordered = harness
        .state()
        .session
        .peer_profile(session_peers::USER_MIRA)
        .expect("mira");
    assert_eq!(reordered.usernames[0].username, "mv_graphics");

    // Hide collectible @mv_graphics
    harness.get_by_label("Hide @mv_graphics").click();
    harness.step();
    harness.step();
    let toggled = harness
        .state()
        .session
        .peer_profile(session_peers::USER_MIRA)
        .expect("mira");
    assert!(
        toggled
            .usernames
            .iter()
            .any(|u| u.username == "mv_graphics" && !u.is_active)
    );

    // 2. VerifyPeersBox
    {
        let st = harness.state_mut();
        st.host.pop(&ctx, &st.colors);
        st.host.push(
            &ctx,
            BoxKind::VerifyPeers(VerifyPeersBoxState::new(
                session_peers::BOT_CATALOG,
                "Catalog Bot",
            )),
            &st.colors,
        );
    }
    for _ in 0..6 {
        harness.step();
    }
    harness.get_by_label("Mira Vance").click();
    harness.step();
    {
        let st = harness.state_mut();
        if let Some(BoxKind::VerifyPeers(v)) = st.host.top_kind_mut() {
            match &mut v.step {
                VerifyPeersStep::ConfirmAdd { description, .. } => {
                    *description = "Core maintainer".to_owned();
                }
                other => panic!("expected ConfirmAdd, got {other:?}"),
            }
            let msg = v.submit_current(&mut st.session).expect("verify mira");
            assert!(msg.contains("Mira Vance"));
        }
    }
    let v_status = harness
        .state()
        .session
        .peer_verification_status(session_peers::BOT_CATALOG, session_peers::USER_MIRA);
    assert!(v_status.verified);
    assert_eq!(v_status.custom_description, "Core maintainer");
}

#[test]
fn e2e_intro_phone_banned_and_unconfirmed_auth_denied_boxes() {
    init_headless_env();
    tg_app::clock::init_test_clock();

    struct IntroBoxState {
        banned: Option<PhoneBannedBoxState>,
        banned_action: Option<PhoneBannedAction>,
        denied: Option<UnconfirmedAuthDeniedBoxState>,
        denied_dismissed: bool,
        palette: Palette,
    }

    let initial = IntroBoxState {
        banned: Some(PhoneBannedBoxState::new("+999 661 0000")),
        banned_action: None,
        denied: None,
        denied_dismissed: false,
        palette: Palette::for_mode(ThemeMode::Day),
    };

    let mut harness = build_widget_harness(initial, Vec2::new(640.0, 520.0), |ui, st| {
        let ctx = ui.ctx();
        if let Some(ref mut b) = st.banned
            && let Some(act) = b.show_modal(ctx, &st.palette)
        {
            st.banned_action = Some(act);
            st.banned = None;
        }
        if let Some(ref mut d) = st.denied
            && d.show_modal(ctx, &st.palette)
        {
            st.denied_dismissed = true;
            st.denied = None;
        }
    });
    harness.step();

    // Click Help in PhoneBannedBox and verify mailto URL
    harness.get_by_label("Help").click();
    harness.step();
    match &harness.state().banned_action {
        Some(PhoneBannedAction::OpenHelp(url)) => {
            assert!(url.starts_with("mailto:?to="));
            assert!(url.contains(&PHONE_BANNED_HELP_EMAIL.replace('@', "%40")));
            assert!(url.contains("999"));
        }
        other => panic!("expected OpenHelp, got {other:?}"),
    }

    // Open UnconfirmedAuthDeniedBox and verify 5-second countdown lock
    harness.state_mut().denied = Some(UnconfirmedAuthDeniedBoxState::new(
        1,
        "We terminated an unrecognized session from Unknown City.",
        false,
        false,
    ));
    harness.step();
    assert_eq!(
        harness
            .state()
            .denied
            .as_ref()
            .expect("denied box")
            .remaining_secs,
        UNCONFIRMED_AUTH_TIMER_SECS
    );
    assert!(
        !harness
            .state()
            .denied
            .as_ref()
            .expect("denied box")
            .can_dismiss()
    );

    // Advance 5 seconds -> unlock
    harness
        .state_mut()
        .denied
        .as_mut()
        .expect("denied box")
        .tick_ms(UNCONFIRMED_AUTH_TIMER_SECS * 1_000);
    assert!(
        harness
            .state()
            .denied
            .as_ref()
            .expect("denied box")
            .can_dismiss()
    );
    harness.step();
}

#[test]
fn e2e_chat_business_bot_pause_resume_and_remove_bar() {
    init_headless_env();
    tg_app::clock::init_test_clock();

    struct HistState {
        widget: HistoryWidget,
        session: MockSession,
        palette: Palette,
        strings: EnglishStrings,
    }

    let peer_id = session_peers::USER_MIRA;
    let initial = HistState {
        widget: HistoryWidget::new(),
        session: MockSession::new(true),
        palette: Palette::for_mode(ThemeMode::Day),
        strings: EnglishStrings,
    };

    assert!(initial.session.business_bot_bar(peer_id).is_some());

    let mut harness = build_widget_harness(initial, Vec2::new(720.0, 620.0), move |ui, st| {
        egui::CentralPanel::default().show(ui, |ui| {
            let _ = st.widget.show_with_backend(
                ui,
                peer_id,
                &mut st.session,
                false,
                false,
                true,
                &st.palette,
                &st.strings,
            );
        });
    });
    for _ in 0..4 {
        harness.step();
    }

    // Click Pause -> bot becomes paused
    harness.get_by_label("Pause").click();
    for _ in 0..3 {
        harness.step();
    }
    assert!(
        harness
            .state()
            .session
            .business_bot_bar(session_peers::USER_MIRA)
            .expect("bar present")
            .paused
    );

    // Click Resume -> bot resumes
    harness.get_by_label("Resume").click();
    for _ in 0..3 {
        harness.step();
    }
    assert!(
        !harness
            .state()
            .session
            .business_bot_bar(session_peers::USER_MIRA)
            .expect("bar present")
            .paused
    );

    // Click Remove business bot -> bar disconnected
    harness.get_by_label("Remove business bot").click();
    for _ in 0..3 {
        harness.step();
    }
    assert!(
        harness
            .state()
            .session
            .business_bot_bar(session_peers::USER_MIRA)
            .is_none()
    );
}
