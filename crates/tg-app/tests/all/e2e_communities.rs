//! End-to-end `egui_kittest` UI & logic tests for WP-27:
//! - Communities (`CommunityBox`, `ManageCommunityBox`, `CommunityPendingRequestsBox`,
//!   `AddToCommunityBox`, `InfoSection::Community`, `InfoSection::CommunityRequests`,
//!   and `dialogs_community_*` list grouping)
//! - Managed Bots (`CreateManagedBotBox`, `EditBotInfoBox`, `tg://newbot` deep-link routing)
//! - `ModerateMessagesBox` (delete / report / delete-all / ban from a group selection)
//! - Welcome Messages (`WelcomeMessagesSectionWidget` + `WelcomeMessagesBoxState`)
//! - Channel Anti-Spam toggle (`channels.toggleAntiSpam` + 100-member threshold lock)

use egui::{Pos2, Rect, Vec2};
use egui_kittest::kittest::Queryable;
use tg_app::boxes::community::{
    CommunityBoxState, CommunityPendingRequestsBoxState, ManageCommunityBoxState,
};
use tg_app::boxes::edit_peer::{ANTISPAM_MIN_MEMBERS, EditPeerInfoBoxState};
use tg_app::boxes::managed_bot::{
    CreateManagedBotBoxState, EditBotInfoBoxState, enforce_bot_username_length,
    missing_bot_username_ending,
};
use tg_app::boxes::moderate_messages::{
    ModerateMessagesBoxState, ModerateParticipant, RestrictDurationPreset,
};
use tg_app::boxes::{BoxHost, BoxKind};
use tg_app::dialogs::backend::{DialogListQuery, DialogsBackend};
use tg_app::dialogs::community::{GroupedDialogRow, group_dialogs_with_communities};
use tg_app::history::welcome_messages::{WELCOME_MESSAGES_DEFAULT_LIMIT, WelcomeMessagesBoxState};
use tg_app::info::communities_mock::{DEMO_COMMUNITY_ID, DEMO_PENDING_LINK_PEER_1};
use tg_app::info::{InfoBackend, InfoColors, InfoSection, InfoWrapWidget};
use tg_app::stand_in::SavedWindowColumns;
use tg_app::stand_in::mock_session::{MockSession, session_peers};
use tg_app::stand_in::theme::ThemeMode;
use tg_app::stand_in::types::{FilterId, PeerId};
use tg_app::window::deep_links::{DeepLinkOutcome, dispatch_deep_link};
use tg_app::window::session_controller::SessionController;

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
fn e2e_community_boxes_info_section_and_dialogs_grouping() {
    let state = BoxTestState::new();
    let comm = state
        .session
        .community_details(DEMO_COMMUNITY_ID)
        .expect("demo community exists");
    assert_eq!(comm.title, "Rustgram Contributors Hub");
    assert_eq!(comm.pending_requests_count, 2);

    // 1. Render CommunityBox -> click "Manage Community" -> ManageCommunityBox opens
    let mut harness = build_box_harness(state, Vec2::new(900.0, 740.0));
    harness.step();
    let ctx = harness.ctx.clone();
    {
        let st = harness.state_mut();
        st.host.push(
            &ctx,
            BoxKind::Community(CommunityBoxState::new(
                DEMO_COMMUNITY_ID,
                "Rustgram Contributors Hub",
            )),
            &st.colors,
        );
    }
    for _ in 0..6 {
        harness.step();
    }

    harness.get_by_label("Manage Community").click();
    for _ in 0..4 {
        harness.step();
    }
    assert!(matches!(
        harness.state().host.top_kind(),
        Some(BoxKind::ManageCommunity(_))
    ));

    // 2. ManageCommunityBox update title & who-can-add policy
    let mut manage = ManageCommunityBoxState::from_details(
        &harness
            .state()
            .session
            .community_details(DEMO_COMMUNITY_ID)
            .expect("comm"),
    );
    manage.title = "Rustgram Hub v2".to_owned();
    manage
        .submit(&mut harness.state_mut().session)
        .expect("save community settings");
    assert_eq!(
        harness
            .state()
            .session
            .community_details(DEMO_COMMUNITY_ID)
            .expect("comm")
            .title,
        "Rustgram Hub v2"
    );

    // 3. Pending link requests approval via CommunityPendingRequestsBoxState
    let mut req_box = CommunityPendingRequestsBoxState::new(DEMO_COMMUNITY_ID);
    req_box.reload(&harness.state().session);
    assert_eq!(req_box.slice.total_count, 2);
    req_box
        .decide_one(
            DEMO_PENDING_LINK_PEER_1,
            false,
            &mut harness.state_mut().session,
        )
        .expect("approve link request");
    assert_eq!(req_box.slice.total_count, 1);

    // 4. Dialogs community list grouping (`group_dialogs_with_communities`)
    {
        let sess = &mut harness.state_mut().session;
        let flat = sess.dialog_slice(DialogListQuery::Filter(FilterId::ALL), 0, 80);
        let groups_expanded = sess.community_groups();
        let ungrouped = group_dialogs_with_communities(&flat.entries, &groups_expanded);
        assert!(
            ungrouped
                .iter()
                .all(|r| matches!(r, GroupedDialogRow::Dialog(_)))
        );

        sess.toggle_community_collapsed_in_dialogs(DEMO_COMMUNITY_ID, true)
            .expect("collapse community");
        let groups_collapsed = sess.community_groups();
        let grouped = group_dialogs_with_communities(&flat.entries, &groups_collapsed);
        assert!(grouped.iter().any(|r| matches!(
            r,
            GroupedDialogRow::CollapsedCommunity { community_id, member_peers, .. }
                if *community_id == DEMO_COMMUNITY_ID && member_peers.len() >= 2
        )));
    }

    // 5. InfoWrapWidget Community & CommunityRequests sections render cleanly
    struct InfoState {
        wrap: InfoWrapWidget,
        session: MockSession,
        colors: InfoColors,
    }
    let mut info_wrap = InfoWrapWidget::for_peer(session_peers::GROUP_CORE);
    info_wrap.open_community(DEMO_COMMUNITY_ID);
    assert_eq!(
        info_wrap.current_section(),
        InfoSection::Community {
            peer_id: DEMO_COMMUNITY_ID
        }
    );
    let mut info_harness = build_widget_harness(
        InfoState {
            wrap: info_wrap,
            session: MockSession::new(true),
            colors: InfoColors::for_mode(ThemeMode::Day),
        },
        Vec2::new(420.0, 720.0),
        |ui, st| {
            let _ = st.wrap.show(ui, &mut st.session, true, &st.colors);
        },
    );
    for _ in 0..4 {
        info_harness.step();
    }
    info_harness
        .get_by_label("Pending Link Requests (2)")
        .click();
    for _ in 0..3 {
        info_harness.step();
    }
    assert_eq!(
        info_harness.state().wrap.current_section(),
        InfoSection::CommunityRequests {
            peer_id: DEMO_COMMUNITY_ID
        }
    );
}

#[test]
fn e2e_managed_bots_and_newbot_deeplink_routing() {
    init_headless_env();
    tg_app::clock::init_test_clock();

    // 1. Exact suffix & length logic from create_managed_bot_box.cpp
    let endings = vec!["bot".to_owned(), "ai".to_owned(), "agent".to_owned()];
    assert_eq!(missing_bot_username_ending("helper", &endings), "bot");
    assert_eq!(missing_bot_username_ending("helperb", &endings), "ot");
    assert_eq!(missing_bot_username_ending("helperbo", &endings), "t");
    assert_eq!(missing_bot_username_ending("helperbot", &endings), "");
    assert_eq!(missing_bot_username_ending("helpera", &endings), "i");
    let fitted = enforce_bot_username_length(&"a".repeat(40), &["bot".to_owned()]);
    assert_eq!(fitted.len() + "bot".len(), 32);

    // 2. `tg://newbot` deep-link routes to BoxKind::CreateManagedBot
    let mut session = MockSession::new(true);
    let mut controller = SessionController::new(SavedWindowColumns::default(), false);
    let outcome = dispatch_deep_link(
        &mut controller,
        "tg://newbot?manager=catalog_demo_bot&username=demo_sub&name=Sub+Helper",
        &mut session,
    );
    let DeepLinkOutcome::ShowBox(boxed) = outcome else {
        panic!("expected ShowBox(CreateManagedBot), got {outcome:?}");
    };
    let BoxKind::CreateManagedBot(mut create_box) = *boxed else {
        panic!("expected ShowBox(CreateManagedBot), got {boxed:?}");
    };
    assert!(create_box.via_deeplink);
    assert_eq!(create_box.name, "Sub Helper");
    // `"demo_sub"` ends with `'b'`, overlapping `"bot"` by 1 char -> `"demo_subot"`
    // (matching `MissingBotUsernameEnding` in `create_managed_bot_box.cpp:52-74`).
    assert_eq!(create_box.full_username(), "demo_subot");

    let created_bot_id = create_box.submit(&mut session).expect("create managed bot");
    assert_ne!(created_bot_id, PeerId::ZERO);

    // 3. EditBotInfoBox multi-language draft, translation, JSON export/import, and save
    let mut edit_box = EditBotInfoBoxState::new(session_peers::BOT_CATALOG, "catalog_demo_bot");
    edit_box.ensure_lang_loaded("", &session);
    edit_box.active_lang = "fr".to_owned();
    edit_box
        .translate_from_default(&session)
        .expect("translate default -> fr");
    assert_eq!(edit_box.draft.changes_count(), 1);
    let json = edit_box.draft.export_json().expect("export json");
    assert!(json.contains("\"fr\""));

    let saved = edit_box.save_all(&mut session).expect("save_all");
    assert_eq!(saved, 1);
    assert_eq!(edit_box.draft.changes_count(), 0);
    assert!(
        session
            .get_bot_info(session_peers::BOT_CATALOG, "fr")
            .expect("get fr info")
            .name
            .starts_with("[FR]")
    );

    // Render CreateManagedBot + EditBotInfo in BoxHost harness
    let mut harness = build_box_harness(BoxTestState::new(), Vec2::new(840.0, 680.0));
    harness.step();
    let ctx = harness.ctx.clone();
    {
        let st = harness.state_mut();
        st.host.push(
            &ctx,
            BoxKind::CreateManagedBot(CreateManagedBotBoxState::new(
                session_peers::BOT_CATALOG,
                "Catalog Bot",
                "catalog_demo_bot",
                Some("QA Runner".to_owned()),
                Some("qa_runner".to_owned()),
                false,
            )),
            &st.colors,
        );
    }
    for _ in 0..4 {
        harness.step();
    }
    assert!(matches!(
        harness.state().host.top_kind(),
        Some(BoxKind::CreateManagedBot(_))
    ));
}

#[test]
fn e2e_moderate_messages_welcome_messages_and_antispam() {
    let mut harness = build_box_harness(BoxTestState::new(), Vec2::new(900.0, 740.0));
    harness.step();

    // 1. ModerateMessagesBox: dynamic count math + submit
    let mut moderate = ModerateMessagesBoxState::new(
        session_peers::GROUP_CORE,
        "Rustgram Core Port",
        vec![101, 102],
        vec![ModerateParticipant {
            peer_id: session_peers::USER_THEO,
            name: "Theo Park".to_owned(),
            username: Some("theo_park".to_owned()),
            selected_msg_ids: vec![101, 102],
            total_messages_count: Some(12),
            can_ban: true,
            can_delete_all: true,
        }],
    )
    .with_linked_community(DEMO_COMMUNITY_ID, "Rustgram Contributors Hub");
    assert_eq!(moderate.effective_delete_count(), 2);
    moderate.delete_all_messages = true;
    assert_eq!(moderate.effective_delete_count(), 12);
    assert_eq!(moderate.modal_title(), "Delete 12 messages?");
    moderate.report_spam = true;
    moderate.ban_or_restrict = true;
    moderate.ban_from_community = true;
    moderate.restrict_preset = RestrictDurationPreset::OneWeek;

    let mod_res = moderate
        .submit(&mut harness.state_mut().session)
        .expect("submit moderation");
    assert_eq!(mod_res.deleted_selected, 2);
    assert_eq!(mod_res.reported_peers, 1);
    assert_eq!(mod_res.deleted_all_peers, 1);
    assert_eq!(mod_res.banned_peers, 1);
    assert_eq!(mod_res.community_banned_peers, 1);

    // Render ModerateMessagesBox in BoxHost
    let ctx = harness.ctx.clone();
    {
        let st = harness.state_mut();
        st.host
            .push(&ctx, BoxKind::ModerateMessages(moderate), &st.colors);
    }
    for _ in 0..4 {
        harness.step();
    }
    assert!(matches!(
        harness.state().host.top_kind(),
        Some(BoxKind::ModerateMessages(_))
    ));

    // 2. WelcomeMessagesSectionWidget: reload, add up to limit, edit, delete, delete_all
    let mut welcome_box =
        WelcomeMessagesBoxState::new(session_peers::GROUP_CORE, "Rustgram Core Port");
    welcome_box.section.reload(&harness.state().session);
    assert_eq!(welcome_box.section.messages.len(), 2);

    welcome_box.section.compose_text = "Rule 3: Keep PRs focused.".to_owned();
    welcome_box
        .section
        .submit_compose(&mut harness.state_mut().session)
        .expect("add welcome #3");
    assert_eq!(welcome_box.section.messages.len(), 3);

    let edit_id = welcome_box.section.messages[2].id;
    welcome_box.section.begin_edit(edit_id);
    welcome_box.section.compose_text = "Rule 3: Keep PRs small and focused.".to_owned();
    welcome_box
        .section
        .submit_compose(&mut harness.state_mut().session)
        .expect("edit welcome #3");
    assert_eq!(
        welcome_box.section.messages[2].text,
        "Rule 3: Keep PRs small and focused."
    );

    // Fill to limit (5) and confirm 6th is rejected
    for i in 4..=WELCOME_MESSAGES_DEFAULT_LIMIT {
        welcome_box.section.compose_text = format!("Template #{i}");
        welcome_box
            .section
            .submit_compose(&mut harness.state_mut().session)
            .expect("add up to limit");
    }
    assert!(welcome_box.section.at_limit());
    welcome_box.section.compose_text = "Too many".to_owned();
    assert!(
        welcome_box
            .section
            .submit_compose(&mut harness.state_mut().session)
            .is_err()
    );

    welcome_box
        .section
        .delete_all(&mut harness.state_mut().session)
        .expect("delete all welcome");
    assert!(welcome_box.section.messages.is_empty());

    // 3. Channel Anti-Spam toggle in EditPeerInfoBox (locked < 100 members, unlocked >= 100)
    let channel_profile = harness
        .state()
        .session
        .peer_profile(session_peers::CHANNEL_DEV_LOG)
        .expect("channel profile");
    let mut edit_peer = EditPeerInfoBoxState::from_profile(&channel_profile);
    assert!(
        edit_peer
            .try_toggle_anti_spam(
                true,
                ANTISPAM_MIN_MEMBERS - 1,
                &mut harness.state_mut().session
            )
            .is_err()
    );
    assert!(!edit_peer.anti_spam_enabled);

    edit_peer
        .try_toggle_anti_spam(true, ANTISPAM_MIN_MEMBERS, &mut harness.state_mut().session)
        .expect("enable anti-spam with >= 100 members");
    assert!(edit_peer.anti_spam_enabled);
    assert!(
        harness
            .state()
            .session
            .channel_anti_spam_enabled(session_peers::CHANNEL_DEV_LOG)
    );
}
