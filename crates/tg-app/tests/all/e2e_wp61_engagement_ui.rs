//! WP-61 end-to-end `egui_kittest` tests, `FakeRpcClient` checks, and headless `wgpu`
//! before/after visual snapshot captures:
//! - Unread `@` mention / heart reaction / poll-vote jump FABs + right-click mark-all-read
//! - "Seen by N" / "Read at HH:MM" context-menu rows + `channels.exportMessageLink` / `getMessageAuthor`
//! - Pull-to-next-unread-channel footer in broadcast channels
//! - `StartBot` deep-link dispatch (`messages.startBot`)
//! - `ConfirmPhoneCodeBox` modal (`tg://confirmphone` -> `account.confirmPhone`)
//! - Birthday `cake.tgs` Lottie emblem in suggestion cards
//! - `getFutureChatCreatorAfterLeave` ownership notice in leave-group confirmation

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use egui::{Key, Modifiers, Vec2};
use egui_kittest::kittest::Queryable;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_app::boxes::BoxKind;
use tg_app::boxes::delete_messages::DeleteMessagesBoxState;
use tg_app::core_types::dto::engagement::{UnreadThingsKind, UnreadThingsScope};
use tg_app::history::backend::{
    HistoryBackend, HistoryMessage, MessageExtra, ServiceAction, to_hist_full,
};
use tg_app::history::birthday_cake::{
    BUNDLED_CAKE_TGS, last_cake_emblem_used_lottie, rasterize_cake_tgs_frame,
};
use tg_app::info::backend::InfoBackend;
use tg_app::live::LiveSession;
use tg_app::session_api::AppSession;
use tg_app::settings::theme_bridge::LiveThemeColors;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::{FullMsgId, MsgId, PeerId};
use tg_app::window::section::SectionShow;
use tg_bots::protocol::StartBotParams;
use tg_bots::stand_in::{PeerId as BotPeerId, UserId as BotUserId};
use tg_core::settings::{CoreSettings, PowerSavingFlags};
use tg_data::{
    BotInfo, Channel, ChannelFlags, ChannelId, Chat, ChatFlags, ChatId, PeerUpdateFlags, User,
    UserFlags, UserId as DataUserId,
};
use tg_lang::tr;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_text::TextWithEntities;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::common::{
    AppTestState, build_harness, build_wgpu_harness, click_at, press_key, right_click_at,
    settle_layout, step_frames, wait_for_menu_item,
};

const DESKTOP_SIZE: Vec2 = Vec2::new(1440.0, 900.0);
const MOBILE_SIZE: Vec2 = Vec2::new(390.0, 844.0);

static WGPU_WP61_LOCK: Mutex<()> = Mutex::new(());

fn save_wp61_png(harness: &mut egui_kittest::Harness<'static, AppTestState>, name: &str) {
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("wp61"));
    std::fs::create_dir_all(&dir).expect("create wp61 snap dir");
    let img = harness.render().expect("wgpu render");
    let path = dir.join(format!("{name}.png"));
    img.save(&path).expect("save png");
}

fn build_wp61_snap_harness(size: Vec2) -> egui_kittest::Harness<'static, AppTestState> {
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
    let mut harness = build_wgpu_harness(state, size);
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

fn build_wp61_test_harness(size: Vec2) -> egui_kittest::Harness<'static, AppTestState> {
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
    let mut harness = build_harness(state, size);
    settle_layout(&mut harness);
    step_frames(&mut harness, 10);
    harness
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .request_scroll_to_bottom(session_peers::GROUP_CORE);
    settle_layout(&mut harness);
    harness
}

fn push_outgoing_read_message(state: &AppTestState, peer: PeerId, msg_id: MsgId, text: &str) {
    let handle = state.controller.session_handle();
    handle.push_history_message(
        peer,
        HistoryMessage {
            id: msg_id,
            peer_id: peer,
            from_id: PeerId::from_user(1001),
            sender_name: "Alex".to_owned(),
            date_unix: 1_795_362_120,
            outgoing: true,
            is_post: false,
            text: TextWithEntities::simple(text.to_owned()),
            media: None,
            service: None,
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: true,
            is_sending: false,
            is_pinned: false,
            extra: MessageExtra::default(),
        },
    );
}

fn push_birthday_card(state: &AppTestState, peer: PeerId) -> MsgId {
    let handle = state.controller.session_handle();
    let bday_id = MsgId(9601);
    handle.push_history_message(
        peer,
        HistoryMessage {
            id: bday_id,
            peer_id: peer,
            from_id: peer,
            sender_name: "Mira Vance".to_owned(),
            date_unix: 1_795_362_000,
            outgoing: false,
            is_post: false,
            text: TextWithEntities::default(),
            media: None,
            service: Some(ServiceAction::SuggestBirthday {
                day: 14,
                month: 5,
                year: Some(1994),
            }),
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: true,
            is_sending: false,
            is_pinned: false,
            extra: MessageExtra::default(),
        },
    );
    bday_id
}

// =============================================================================
// 1. egui_kittest End-to-End UI Tests
// =============================================================================

#[test]
fn wp61_unread_jump_fabs_click_and_mark_all_read() {
    let mut h = build_wp61_test_harness(DESKTOP_SIZE);
    let peer = session_peers::GROUP_CORE;
    let msg_ids: Vec<MsgId> = h
        .state()
        .controller
        .main_widget()
        .history()
        .inner()
        .messages()
        .iter()
        .filter(|m| !m.is_service())
        .map(|m| m.id)
        .collect();
    assert!(
        msg_ids.len() >= 3,
        "expected at least 3 messages in GROUP_CORE"
    );

    let handle = h.state().controller.session_handle();
    handle.set_unread_things(
        peer,
        UnreadThingsKind::Mentions,
        vec![msg_ids[0], msg_ids[1]],
    );
    handle.set_unread_things(
        peer,
        UnreadThingsKind::Reactions,
        vec![msg_ids[1], msg_ids[2]],
    );
    handle.set_unread_things(peer, UnreadThingsKind::PollVotes, vec![msg_ids[2]]);
    step_frames(&mut h, 6);

    let (mention_rect, reaction_rect, poll_rect) = {
        let widget = h
            .state_mut()
            .controller
            .main_widget_mut()
            .history_mut()
            .unread_things_mut();
        (
            widget
                .button_rect(UnreadThingsKind::Mentions)
                .expect("mention FAB visible"),
            widget
                .button_rect(UnreadThingsKind::Reactions)
                .expect("reaction FAB visible"),
            widget
                .button_rect(UnreadThingsKind::PollVotes)
                .expect("poll-vote FAB visible"),
        )
    };

    // Stacked bottom-to-top: PollVotes below Reactions below Mentions, with zero overlap.
    assert!(
        mention_rect.bottom() < reaction_rect.top(),
        "mention FAB {mention_rect:?} must sit above reaction FAB {reaction_rect:?}"
    );
    assert!(
        reaction_rect.bottom() < poll_rect.top(),
        "reaction FAB {reaction_rect:?} must sit above poll-vote FAB {poll_rect:?}"
    );

    // Left-click the `@` mention FAB -> jumps/highlights first unread mention.
    click_at(&mut h, mention_rect.center());
    step_frames(&mut h, 2);
    let now_ms = (h.ctx.input(|i| i.time) * 1000.0).round().max(0.0) as i64;
    let target_full = to_hist_full(FullMsgId {
        peer,
        msg: msg_ids[0],
    });
    let hl_state = h
        .state()
        .controller
        .main_widget()
        .history()
        .inner()
        .highlighter()
        .state_for(target_full, now_ms);
    assert!(
        hl_state.opacity > 0.0,
        "clicking @ mention FAB should highlight first unread mention {:?}",
        msg_ids[0]
    );

    // Right-click the heart reaction FAB (re-read rect since jumping may show the scroll-down FAB).
    let reaction_rect_now = h
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .unread_things_mut()
        .button_rect(UnreadThingsKind::Reactions)
        .expect("reaction FAB still visible after jump");
    right_click_at(&mut h, reaction_rect_now.center());
    step_frames(&mut h, 4);
    let mark_reactions_label = tr::lng_context_mark_read_reactions_all();
    let menu_node = h
        .query_by_label(&mark_reactions_label)
        .expect("mark-all-reactions popup item should appear on right-click");
    let menu_center = menu_node.rect().center();
    click_at(&mut h, menu_center);
    step_frames(&mut h, 6);

    let scope = UnreadThingsScope::history(peer);
    let counts = HistoryBackend::unread_things_counts(&handle, scope);
    assert_eq!(
        counts.reactions, 0,
        "right-click 'Read all reactions' should clear unread reactions count"
    );
    assert_eq!(
        counts.mentions, 2,
        "clearing reactions must leave mentions untouched"
    );
    assert_eq!(
        counts.poll_votes, 1,
        "clearing reactions must leave poll votes untouched"
    );
}

#[test]
fn wp61_read_receipts_group_seen_by_and_channel_author_context_menu() {
    let mut h = build_wp61_test_harness(DESKTOP_SIZE);
    let out_id = MsgId(9610);
    push_outgoing_read_message(
        h.state(),
        session_peers::GROUP_CORE,
        out_id,
        "Shipped WP-61 read receipts and unread jump buttons!",
    );
    let handle = h.state().controller.session_handle();
    h.state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .reload_active_peer(&handle);
    h.state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .request_scroll_to_bottom(session_peers::GROUP_CORE);
    settle_layout(&mut h);

    // 1. Group outgoing read message -> "3 Seen" row opens SeenByModalWidget, and Copy Message Link works.
    let outgoing_rect = h
        .state()
        .controller
        .main_widget()
        .history()
        .inner()
        .last_painted_bubble_rect(out_id)
        .expect("painted outgoing bubble in GROUP_CORE");
    right_click_at(&mut h, outgoing_rect.center());
    let seen_label = tr::lng_context_seen_text(3);
    wait_for_menu_item(&mut h, &seen_label);

    assert!(
        h.query_by_label(&tr::lng_context_copy_message_link())
            .is_some(),
        "group context menu should contain localized 'Copy Message Link'"
    );

    let seen_center = h
        .query_by_label(&seen_label)
        .expect("Seen by row")
        .rect()
        .center();
    click_at(&mut h, seen_center);
    step_frames(&mut h, 4);

    {
        let modal = h
            .state()
            .controller
            .main_widget()
            .history()
            .seen_by_modal()
            .expect("SeenByModalWidget should open after clicking Seen row");
        assert_eq!(modal.readers.len(), 3);
        assert_eq!(modal.readers[0].name, "Mira Vance");
    }

    // Escape closes the Seen-By modal.
    press_key(&mut h, Modifiers::NONE, Key::Escape);
    step_frames(&mut h, 3);
    assert!(
        h.state()
            .controller
            .main_widget()
            .history()
            .seen_by_modal()
            .is_none(),
        "Escape should close SeenByModalWidget"
    );

    // 2. Broadcast channel post -> "Sent by Mira Vance" (`channels.getMessageAuthor`) & "Copy Post Link".
    if let Some(session) = h.state_mut().controller.session_mut() {
        session.show_peer_history(
            session_peers::CHANNEL_DEV_LOG,
            SectionShow::default(),
            MsgId::SHOW_AT_END,
        );
    }
    settle_layout(&mut h);
    h.state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .request_scroll_to_bottom(session_peers::CHANNEL_DEV_LOG);
    settle_layout(&mut h);

    let post_rect = {
        let inner = h.state().controller.main_widget().history().inner();
        inner
            .messages()
            .iter()
            .rev()
            .filter(|m| !m.is_service())
            .find_map(|m| inner.last_painted_bubble_rect(m.id))
            .expect("painted channel post in CHANNEL_DEV_LOG")
    };
    right_click_at(&mut h, post_rect.center());
    let sent_by_label = tr::lng_context_sent_by("Mira Vance");
    wait_for_menu_item(&mut h, &sent_by_label);
    assert!(
        h.query_by_label(&tr::lng_context_copy_post_link())
            .is_some(),
        "channel post context menu should contain localized 'Copy Post Link'"
    );
}

#[test]
fn wp61_read_receipts_dm_outbox_read_at_row() {
    let mut h = build_wp61_test_harness(DESKTOP_SIZE);
    let out_id = MsgId(9611);
    push_outgoing_read_message(
        h.state(),
        session_peers::USER_THEO,
        out_id,
        "Did you review the outbox read-date row?",
    );
    if let Some(session) = h.state_mut().controller.session_mut() {
        session.show_peer_history(
            session_peers::USER_THEO,
            SectionShow::default(),
            MsgId::SHOW_AT_END,
        );
    }
    settle_layout(&mut h);
    h.state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .request_scroll_to_bottom(session_peers::USER_THEO);
    settle_layout(&mut h);

    let outgoing_rect = h
        .state()
        .controller
        .main_widget()
        .history()
        .inner()
        .last_painted_bubble_rect(out_id)
        .expect("painted outgoing read message in USER_THEO");
    right_click_at(&mut h, outgoing_rect.center());
    step_frames(&mut h, 10);

    let read_at_node = h
        .query_by_label_contains("Read at ")
        .expect("1:1 DM outgoing read message should display 'Read at ...' row");
    assert!(read_at_node.rect().is_positive());
}

#[test]
fn wp61_pull_to_next_channel_navigates_to_next_unread_channel() {
    let mut h = build_wp61_test_harness(DESKTOP_SIZE);
    if let Some(session) = h.state_mut().controller.session_mut() {
        session.show_peer_history(
            session_peers::CHANNEL_DEV_LOG,
            SectionShow::default(),
            MsgId::SHOW_AT_END,
        );
    }
    settle_layout(&mut h);
    h.state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .request_scroll_to_bottom(session_peers::CHANNEL_DEV_LOG);
    settle_layout(&mut h);

    // 1. Feed overscroll past the release threshold with current egui time, then release -> jumps to CHANNEL_RELEASES.
    let now = h.ctx.input(|i| i.time);
    {
        let pull = h
            .state_mut()
            .controller
            .main_widget_mut()
            .history_mut()
            .pull_next_channel_mut();
        pull.feed_bottom_overscroll(260.0, now, true);
        assert!(
            pull.is_ready(),
            "overscroll past threshold should flip ready=true"
        );
    }
    // Wait past the 220ms release threshold (~10 frames at 1/30s = 333ms).
    step_frames(&mut h, 12);
    settle_layout(&mut h);

    let active_after_pull = h.state().controller.session().map(|s| s.active_peer());
    assert_eq!(
        active_after_pull,
        Some(session_peers::CHANNEL_RELEASES),
        "releasing pull-to-next-channel past threshold should navigate to CHANNEL_RELEASES"
    );

    // 2. Clicking the accessible pull footer on CHANNEL_RELEASES navigates back to unread CHANNEL_DEV_LOG.
    h.state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .request_scroll_to_bottom(session_peers::CHANNEL_RELEASES);
    settle_layout(&mut h);
    let pull_prompt = tr::lng_pull_next_channel();
    let footer_node = h
        .query_by_label_contains(&pull_prompt)
        .expect("pull-to-next-channel footer should display pull prompt");
    let footer_center = footer_node.rect().center();
    click_at(&mut h, footer_center);
    settle_layout(&mut h);

    let active_after_click = h.state().controller.session().map(|s| s.active_peer());
    assert_eq!(
        active_after_click,
        Some(session_peers::CHANNEL_DEV_LOG),
        "clicking the pull-to-next-channel footer should navigate to the next unread channel"
    );
}

#[test]
fn wp61_start_bot_deep_link_dispatches_rpc_without_composer_prefill() {
    let mut h = build_wp61_test_harness(DESKTOP_SIZE);
    let handle = h.state().controller.session_handle();

    h.state_mut()
        .controller
        .open_link("https://t.me/catalog_demo_bot?start=wp61_ref42");
    settle_layout(&mut h);

    let started = handle.started_bots();
    assert_eq!(
        started.len(),
        1,
        "opening bot ?start= deep link must invoke BotsBackend::start_bot"
    );
    assert_eq!(
        started[0].bot_id,
        BotUserId(session_peers::BOT_CATALOG.bare())
    );
    assert_eq!(started[0].peer_id, None);
    assert_eq!(started[0].start_param, "wp61_ref42");

    // Crucial WP-54/WP-61 regression check: composer must NOT be prefilled with "/start wp61_ref42".
    let compose_text = h
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .compose_mut()
        .text
        .clone();
    assert!(
        !compose_text.contains("/start"),
        "StartBot deep link must NOT prefill '/start' in composer, got: {compose_text:?}"
    );
}

#[test]
fn wp61_confirm_phone_deep_link_modal_validation_and_submit() {
    let mut h = build_wp61_test_harness(DESKTOP_SIZE);
    let handle = h.state().controller.session_handle();

    h.state_mut()
        .controller
        .open_link("tg://confirmphone?phone=15550100&hash=h99");
    settle_layout(&mut h);

    // Verify ConfirmPhoneCode modal is topmost with formatted phone and hash.
    {
        let top = h
            .state()
            .controller
            .box_host()
            .top_kind()
            .expect("ConfirmPhoneCode box should open");
        let BoxKind::ConfirmPhoneCode(ref state) = *top else {
            panic!("expected BoxKind::ConfirmPhoneCode, got {top:?}");
        };
        assert_eq!(state.display_phone(), "+15550100");
        assert_eq!(state.phone_code_hash, "mock_confirm_hash");
    }

    // 1. Submit invalid code "00000" -> stays open with localized bad-code error.
    if let Some(BoxKind::ConfirmPhoneCode(state)) =
        h.state_mut().controller.box_host_mut().top_kind_mut()
    {
        state.code_input = "00000".to_owned();
    }
    step_frames(&mut h, 2);
    let send_btn = h
        .query_by_label(&tr::lng_confirm_phone_send())
        .expect("Send button in ConfirmPhoneCodeBox")
        .rect()
        .center();
    click_at(&mut h, send_btn);
    step_frames(&mut h, 4);

    {
        let top = h
            .state()
            .controller
            .box_host()
            .top_kind()
            .expect("ConfirmPhoneCode should stay open on invalid code");
        let BoxKind::ConfirmPhoneCode(ref state) = *top else {
            panic!("expected ConfirmPhoneCode still open");
        };
        assert_eq!(state.error.as_deref(), Some(tr::lng_bad_code().as_str()));
    }

    // 2. Submit valid code "54321" -> calls account.confirmPhone, closes box, shows success Inform modal.
    if let Some(BoxKind::ConfirmPhoneCode(state)) =
        h.state_mut().controller.box_host_mut().top_kind_mut()
    {
        state.code_input = "54321".to_owned();
    }
    step_frames(&mut h, 2);
    let send_btn = h
        .query_by_label(&tr::lng_confirm_phone_send())
        .expect("Send button in ConfirmPhoneCodeBox")
        .rect()
        .center();
    click_at(&mut h, send_btn);
    step_frames(&mut h, 6);

    assert!(
        h.state().controller.box_host().is_empty(),
        "ConfirmPhoneCode box should close on successful confirmation"
    );
    let recorded = handle.confirmed_phone_codes();
    assert!(
        recorded
            .iter()
            .any(|(hash, code)| hash == "mock_confirm_hash" && code == "54321"),
        "expected confirmed phone code ('mock_confirm_hash', '54321'), got {recorded:?}"
    );
    assert!(
        h.state().controller.layer_stack().is_layer_shown(),
        "success confirmation Inform box should be shown on LayerStack"
    );
    assert!(
        h.query_by_label_contains("+15550100").is_some(),
        "success confirmation Inform box should include formatted phone '+15550100'"
    );
}

#[test]
fn wp61_birthday_card_renders_bundled_cake_tgs_and_falls_back_on_invalid_bytes() {
    // 1. Direct Lottie rasterization check on bundled `cake.tgs` and invalid fallback bytes.
    let (w, h_px, rgba) = rasterize_cake_tgs_frame(BUNDLED_CAKE_TGS, 96, Some(0))
        .expect("bundled cake.tgs must decode via tg_lottie");
    assert_eq!((w, h_px), (96, 96));
    assert_eq!(rgba.len(), 96 * 96 * 4);
    assert!(
        rgba.as_chunks::<4>().0.iter().any(|px| px[3] > 16),
        "decoded cake.tgs frame must contain visible non-transparent pixels"
    );
    assert!(
        rasterize_cake_tgs_frame(b"not-valid-tgs", 96, Some(0)).is_none(),
        "invalid bytes must return None so vector fallback activates cleanly"
    );

    // 2. End-to-end UI render of birthday suggestion card uses the Lottie texture.
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
    let _ = push_birthday_card(&state, session_peers::USER_MIRA);
    if let Some(session) = state.controller.session_mut() {
        session.show_peer_history(
            session_peers::USER_MIRA,
            SectionShow::default(),
            MsgId::SHOW_AT_END,
        );
    }
    let mut h = build_harness(state, DESKTOP_SIZE);
    settle_layout(&mut h);
    h.state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .request_scroll_to_bottom(session_peers::USER_MIRA);
    settle_layout(&mut h);

    assert!(
        last_cake_emblem_used_lottie(),
        "birthday suggestion card should render the bundled cake.tgs Lottie texture"
    );
}

#[test]
fn wp61_leave_group_confirmation_displays_future_creator_notice() {
    let mut h = build_wp61_test_harness(DESKTOP_SIZE);
    let ctx = h.ctx.clone();
    let colors = LiveThemeColors::default();
    let creator = {
        let backend = h.state_mut().controller.session_backend_mut();
        HistoryBackend::future_chat_creator_after_leave(backend, session_peers::GROUP_CORE)
            .expect("future creator lookup")
    };
    assert!(
        creator.is_some(),
        "GROUP_CORE should report a future creator"
    );
    let peer = InfoBackend::peer_profile(
        h.state().controller.session_backend(),
        session_peers::GROUP_CORE,
    )
    .expect("GROUP_CORE profile");

    let box_state =
        DeleteMessagesBoxState::for_delete_or_leave(&peer, false).with_future_creator(creator);
    let modal = box_state.to_modal("leave_test");
    assert_eq!(modal.title, tr::lng_leave_next_owner_box_title_group());

    h.state_mut()
        .controller
        .box_host_mut()
        .push(&ctx, BoxKind::DeleteMessages(box_state), &colors);
    settle_layout(&mut h);

    assert!(
        h.query_by_label_contains("will become the new owner")
            .is_some(),
        "leave-group modal should display future owner transfer notice for 'Mira Vance'"
    );
}

// =============================================================================
// 2. FakeRpcClient Protocol Tests (LiveSession TL Wire Verification)
// =============================================================================

type RequestCapture = (u32, Vec<u8>);
type ResponseEntry = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RequestCapture>>>,
    responses: Arc<Mutex<VecDeque<ResponseEntry>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
    }

    fn recorded_of(&self, ctor: u32) -> Vec<Vec<u8>> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(c, _)| *c == ctor)
            .map(|(_, b)| b.clone())
            .collect()
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let dc = self.main_dc_id();
        self.invoke_in_dc(dc, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        request: &R,
    ) -> Result<R::Return, Self::Error> {
        let raw = request.to_bytes();
        let ctor = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, raw));

        let next = self.responses.lock().expect("lock responses").pop_front();
        match next {
            Some((expected, outcome)) => {
                assert_eq!(
                    ctor, expected,
                    "unexpected RPC ctor: got {ctor:#010x}, expected {expected:#010x}"
                );
                let bytes = outcome?;
                R::Return::from_bytes(&bytes)
                    .map_err(|e| InvocationError::Deserialize(e.to_string()))
            }
            None => Err(InvocationError::Rpc(RpcError {
                code: 500,
                name: format!("NO_FAKE_RESPONSE_{ctor:#010x}"),
                value: None,
            })),
        }
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock main_dc")
    }
}

fn synthetic_tl_user(id: i64, first_name: &str, last_name: &str, username: &str) -> tl_enums::User {
    tl_enums::User::User(tl_types::User {
        self_: false,
        contact: false,
        mutual_contact: false,
        deleted: false,
        bot: false,
        bot_chat_history: false,
        bot_nochats: false,
        verified: false,
        restricted: false,
        min: false,
        bot_inline_geo: false,
        support: false,
        scam: false,
        apply_min_photo: false,
        fake: false,
        bot_attach_menu: false,
        premium: false,
        attach_menu_enabled: false,
        bot_can_edit: false,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(0x1122_3344_5566),
        first_name: Some(first_name.to_owned()),
        last_name: Some(last_name.to_owned()),
        username: Some(username.to_owned()),
        phone: None,
        photo: None,
        status: None,
        bot_info_version: None,
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: None,
        emoji_status: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    })
}

fn make_wp61_live_session() -> (LiveSession, FakeRpcClient, PeerId, PeerId, PeerId, PeerId) {
    let fake = FakeRpcClient::new(2);
    let self_uid = DataUserId(10_610);
    let friend_uid = DataUserId(20_610);
    let bot_uid = DataUserId(30_610);
    let group_cid = ChatId(40_610);
    let chan_cid = ChannelId(50_610);

    let cfg = SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..Default::default()
    };
    let runtime = Arc::new(
        SessionRuntime::start(
            self_uid,
            tg_app::live::ErasedRpcClient::new(fake.clone()),
            cfg,
        )
        .expect("start SessionRuntime"),
    );

    runtime.tokio_handle().block_on(async {
        let mut s = runtime.session().write().await;
        let mut self_user = User::new(self_uid, true);
        self_user.first_name = "Alex".to_owned();
        self_user.flags.insert(UserFlags::SELF);
        s.upsert_user(self_user, PeerUpdateFlags::NAME);

        let mut friend = User::new(friend_uid, false);
        friend.first_name = "Mira".to_owned();
        friend.last_name = "Vance".to_owned();
        friend.access_hash = 0x1122_3344_5566;
        s.upsert_user(friend, PeerUpdateFlags::NAME);

        let mut bot = User::new(bot_uid, false);
        bot.first_name = "Catalog Bot".to_owned();
        bot.access_hash = 0x7788_99AA_BBCC;
        bot.bot_info = Some(BotInfo::default());
        s.upsert_user(bot, PeerUpdateFlags::NAME | PeerUpdateFlags::IS_BOT);

        let mut chat = Chat::new(group_cid);
        chat.common.name = "Core Team".to_owned();
        chat.flags |= ChatFlags::CREATOR;
        s.upsert_chat(chat, PeerUpdateFlags::NAME);

        let mut channel = Channel::new(chan_cid);
        channel.common.name = "Dev Log".to_owned();
        channel.access_hash = 0xCAFE_BABE_1234;
        channel.flags |= ChannelFlags::BROADCAST | ChannelFlags::CREATOR;
        s.upsert_channel(channel, PeerUpdateFlags::NAME);
    });

    let live = LiveSession::new(runtime, 1);
    (
        live,
        fake,
        PeerId::from_user(friend_uid.0),
        PeerId::from_user(bot_uid.0),
        PeerId::from_chat(group_cid.0),
        PeerId::from_channel(chan_cid.0),
    )
}

#[test]
fn wp61_fake_rpc_start_bot_private_and_group() {
    let (mut live, fake, _friend, bot_peer, group_peer, _chan) = make_wp61_live_session();
    let bot_uid = BotUserId(bot_peer.bare());

    // 1. Private 1:1 `?start=ref42` -> messages.startBot(peer = InputPeer::Empty)
    fake.enqueue_ok(
        tl_fns::messages::StartBot::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_795_362_100,
            seq: 1,
        }),
    );
    let p_private = StartBotParams::for_private_chat(bot_uid, "ref42", 9001);
    let msg = tg_app::bots::backend::BotsBackend::start_bot(&mut live, &p_private)
        .expect("private start_bot");
    assert!(msg.outgoing);

    // 2. Group `?startgroup=grp7` -> messages.startBot(peer = InputPeer::Chat)
    fake.enqueue_ok(
        tl_fns::messages::StartBot::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_795_362_101,
            seq: 2,
        }),
    );
    let p_group = StartBotParams::for_group_chat(bot_uid, BotPeerId(group_peer.0), "grp7", 9002);
    let _ = tg_app::bots::backend::BotsBackend::start_bot(&mut live, &p_group)
        .expect("group start_bot");

    let calls = fake.recorded_of(tl_fns::messages::StartBot::CONSTRUCTOR_ID);
    assert_eq!(calls.len(), 2, "expected 2 messages.startBot RPCs");

    let req1 = tl_fns::messages::StartBot::from_bytes(&calls[0]).expect("decode req1");
    assert_eq!(req1.start_param, "ref42");
    assert!(matches!(req1.peer, tl_enums::InputPeer::Empty));

    let req2 = tl_fns::messages::StartBot::from_bytes(&calls[1]).expect("decode req2");
    assert_eq!(req2.start_param, "grp7");
    assert!(matches!(req2.peer, tl_enums::InputPeer::Chat(_)));
}

#[test]
fn wp61_fake_rpc_confirm_phone_send_and_confirm() {
    let (mut live, fake, ..) = make_wp61_live_session();

    fake.enqueue_ok(
        tl_fns::account::SendConfirmPhoneCode::CONSTRUCTOR_ID,
        &tl_enums::auth::SentCode::SentCode(tl_types::auth::SentCode {
            r#type: tl_enums::auth::SentCodeType::Sms(tl_types::auth::SentCodeTypeSms {
                length: 5,
            }),
            phone_code_hash: "srv_hash_77".to_owned(),
            next_type: None,
            timeout: Some(60),
        }),
    );
    fake.enqueue_ok(tl_fns::account::ConfirmPhone::CONSTRUCTOR_ID, &true);

    let hash = live
        .send_confirm_phone_code("deep_hash_99")
        .expect("send_confirm_phone_code");
    assert_eq!(hash, "srv_hash_77");

    let ok = live
        .confirm_phone_code(&hash, "54321")
        .expect("confirm_phone_code");
    assert!(ok);

    let send_calls = fake.recorded_of(tl_fns::account::SendConfirmPhoneCode::CONSTRUCTOR_ID);
    assert_eq!(send_calls.len(), 1);
    let confirm_calls = fake.recorded_of(tl_fns::account::ConfirmPhone::CONSTRUCTOR_ID);
    assert_eq!(confirm_calls.len(), 1);
    let confirm_req =
        tl_fns::account::ConfirmPhone::from_bytes(&confirm_calls[0]).expect("decode ConfirmPhone");
    assert_eq!(confirm_req.phone_code_hash, "srv_hash_77");
    assert_eq!(confirm_req.phone_code, "54321");
}

#[test]
fn wp61_fake_rpc_engagement_context_menu_and_leave_owner() {
    let (mut live, fake, friend_peer, _bot, group_peer, chan_peer) = make_wp61_live_session();
    let friend_bare = friend_peer.bare() as i64;

    // 1. messages.getMessageReadParticipants
    fake.enqueue_ok(
        tl_fns::messages::GetMessageReadParticipants::CONSTRUCTOR_ID,
        &vec![tl_enums::ReadParticipantDate::ReadParticipantDate(
            tl_types::ReadParticipantDate {
                user_id: friend_bare,
                date: 1_795_362_300,
            },
        )],
    );
    let readers = HistoryBackend::get_message_read_participants(&live, group_peer, MsgId(50))
        .expect("get_message_read_participants");
    assert_eq!(readers.len(), 1);
    assert_eq!(readers[0].user_id, friend_peer);
    assert_eq!(readers[0].read_date_unix, 1_795_362_300);

    // 2. messages.getOutboxReadDate
    fake.enqueue_ok(
        tl_fns::messages::GetOutboxReadDate::CONSTRUCTOR_ID,
        &tl_enums::OutboxReadDate::OutboxReadDate(tl_types::OutboxReadDate {
            date: 1_795_362_400,
        }),
    );
    let dm_status = HistoryBackend::get_outbox_read_date(&live, friend_peer, MsgId(51))
        .expect("get_outbox_read_date");
    assert_eq!(
        dm_status,
        tg_app::core_types::dto::engagement::OutboxReadDateStatus::Read {
            date_unix: 1_795_362_400
        }
    );

    // 3. channels.exportMessageLink
    fake.enqueue_ok(
        tl_fns::channels::ExportMessageLink::CONSTRUCTOR_ID,
        &tl_enums::ExportedMessageLink::ExportedMessageLink(tl_types::ExportedMessageLink {
            link: "https://t.me/devlog/77".to_owned(),
            html: "<script></script>".to_owned(),
        }),
    );
    let exported = HistoryBackend::export_message_link(&live, chan_peer, MsgId(77), false, false)
        .expect("export_message_link");
    assert_eq!(exported.link, "https://t.me/devlog/77");

    // 4. channels.getMessageAuthor
    fake.enqueue_ok(
        tl_fns::channels::GetMessageAuthor::CONSTRUCTOR_ID,
        &synthetic_tl_user(friend_bare, "Mira", "Vance", "mira_v"),
    );
    let author = HistoryBackend::get_message_author(&live, chan_peer, MsgId(77))
        .expect("get_message_author");
    assert_eq!(author.name, "Mira Vance");

    // 5. messages.getFutureChatCreatorAfterLeave
    fake.enqueue_ok(
        tl_fns::messages::GetFutureChatCreatorAfterLeave::CONSTRUCTOR_ID,
        &synthetic_tl_user(friend_bare, "Mira", "Vance", "mira_v"),
    );
    let future = HistoryBackend::future_chat_creator_after_leave(&live, group_peer)
        .expect("future_chat_creator_after_leave")
        .expect("expected Mira Vance");
    assert_eq!(future.name, "Mira Vance");
    assert_eq!(future.username.as_deref(), Some("mira_v"));

    // 6. messages.getUnreadMentions + messages.readMentions
    fake.enqueue_ok(
        tl_fns::messages::GetUnreadMentions::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: Vec::new(),
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::ReadMentions::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedHistory::AffectedHistory(
            tl_types::messages::AffectedHistory {
                pts: 10,
                pts_count: 1,
                offset: 0,
            },
        ),
    );
    let _ = HistoryBackend::load_unread_things_page(
        &mut live,
        UnreadThingsScope::history(group_peer),
        UnreadThingsKind::Mentions,
    )
    .expect("load_unread_things_page");
    let _ = HistoryBackend::read_unread_mentions(&mut live, group_peer, None)
        .expect("read_unread_mentions");

    assert_eq!(
        fake.recorded_of(tl_fns::messages::GetMessageReadParticipants::CONSTRUCTOR_ID)
            .len(),
        1
    );
    assert_eq!(
        fake.recorded_of(tl_fns::messages::GetOutboxReadDate::CONSTRUCTOR_ID)
            .len(),
        1
    );
    assert_eq!(
        fake.recorded_of(tl_fns::channels::ExportMessageLink::CONSTRUCTOR_ID)
            .len(),
        1
    );
    assert_eq!(
        fake.recorded_of(tl_fns::channels::GetMessageAuthor::CONSTRUCTOR_ID)
            .len(),
        1
    );
    assert_eq!(
        fake.recorded_of(tl_fns::messages::GetFutureChatCreatorAfterLeave::CONSTRUCTOR_ID)
            .len(),
        1
    );
    assert_eq!(
        fake.recorded_of(tl_fns::messages::GetUnreadMentions::CONSTRUCTOR_ID)
            .len(),
        1
    );
    assert_eq!(
        fake.recorded_of(tl_fns::messages::ReadMentions::CONSTRUCTOR_ID)
            .len(),
        1
    );
}

// =============================================================================
// 3. Headless wgpu Before/After Visual Snapshot Captures
// =============================================================================

#[test]
#[ignore = "captures WP-61 before screenshots on master under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_wp61_before_screenshots() {
    let _guard = WGPU_WP61_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    for (tag, size) in [("1440x900", DESKTOP_SIZE), ("390x844", MOBILE_SIZE)] {
        {
            let mut h = build_wp61_snap_harness(size);
            let outgoing_rect = {
                let inner = h.state().controller.main_widget().history().inner();
                inner
                    .messages()
                    .iter()
                    .rev()
                    .filter(|m| m.outgoing && !m.is_service())
                    .find_map(|m| inner.last_painted_bubble_rect(m.id))
            };
            if let Some(bubble_rect) = outgoing_rect {
                right_click_at(&mut h, bubble_rect.center());
                step_frames(&mut h, 4);
            }
            save_wp61_png(&mut h, &format!("chat_engagement_{tag}_before"));
        }

        {
            let mut h = build_wp61_snap_harness(size);
            if let Some(session) = h.state_mut().controller.session_mut() {
                session.show_peer_history(
                    session_peers::CHANNEL_DEV_LOG,
                    SectionShow::default(),
                    MsgId::SHOW_AT_END,
                );
            }
            settle_layout(&mut h);
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .request_scroll_to_bottom(session_peers::CHANNEL_DEV_LOG);
            settle_layout(&mut h);
            save_wp61_png(&mut h, &format!("channel_pull_next_{tag}_before"));
        }

        {
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
            let _ = push_birthday_card(&state, session_peers::USER_MIRA);
            if let Some(session) = state.controller.session_mut() {
                session.show_peer_history(
                    session_peers::USER_MIRA,
                    SectionShow::default(),
                    MsgId::SHOW_AT_END,
                );
            }
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .request_scroll_to_bottom(session_peers::USER_MIRA);
            settle_layout(&mut h);
            save_wp61_png(&mut h, &format!("birthday_cake_card_{tag}_before"));
        }

        {
            let mut h = build_wp61_snap_harness(size);
            h.state_mut()
                .controller
                .open_link("tg://confirmphone?phone=15550100&hash=h99");
            settle_layout(&mut h);
            save_wp61_png(&mut h, &format!("confirm_phone_box_{tag}_before"));
        }

        {
            let mut h = build_wp61_snap_harness(size);
            let ctx = h.ctx.clone();
            let colors = LiveThemeColors::default();
            if let Some(peer) = InfoBackend::peer_profile(
                h.state().controller.session_backend(),
                session_peers::GROUP_CORE,
            ) {
                let box_state = DeleteMessagesBoxState::for_delete_or_leave(&peer, false);
                h.state_mut().controller.box_host_mut().push(
                    &ctx,
                    BoxKind::DeleteMessages(box_state),
                    &colors,
                );
            }
            settle_layout(&mut h);
            save_wp61_png(&mut h, &format!("leave_group_creator_{tag}_before"));
        }
    }
}

#[test]
#[ignore = "captures WP-61 after screenshots under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_wp61_after_screenshots() {
    let _guard = WGPU_WP61_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    for (tag, size) in [("1440x900", DESKTOP_SIZE), ("390x844", MOBILE_SIZE)] {
        // 1. Group chat engagement: stacked unread @/heart/poll FABs + outgoing "3 Seen" context menu
        {
            let mut h = build_wp61_snap_harness(size);
            let out_id = MsgId(9620);
            push_outgoing_read_message(
                h.state(),
                session_peers::GROUP_CORE,
                out_id,
                "Shipped WP-61 read receipts and unread jump buttons!",
            );
            let reload_handle = h.state().controller.session_handle();
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .reload_active_peer(&reload_handle);
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .request_scroll_to_bottom(session_peers::GROUP_CORE);
            settle_layout(&mut h);

            let msg_ids: Vec<MsgId> = h
                .state()
                .controller
                .main_widget()
                .history()
                .inner()
                .messages()
                .iter()
                .filter(|m| !m.is_service())
                .map(|m| m.id)
                .collect();
            let handle = h.state().controller.session_handle();
            if msg_ids.len() >= 3 {
                handle.set_unread_things(
                    session_peers::GROUP_CORE,
                    UnreadThingsKind::Mentions,
                    vec![msg_ids[0], msg_ids[1]],
                );
                handle.set_unread_things(
                    session_peers::GROUP_CORE,
                    UnreadThingsKind::Reactions,
                    vec![msg_ids[0], msg_ids[1], msg_ids[2]],
                );
                handle.set_unread_things(
                    session_peers::GROUP_CORE,
                    UnreadThingsKind::PollVotes,
                    vec![msg_ids[2]],
                );
            }
            step_frames(&mut h, 8);
            if let Some(bubble_rect) = h
                .state()
                .controller
                .main_widget()
                .history()
                .inner()
                .last_painted_bubble_rect(out_id)
            {
                // Anchor context menu on the left edge of the outgoing bubble so the stacked
                // unread @/heart/poll FABs on the right remain clearly visible beside the menu.
                let click_pos = egui::Pos2::new(
                    (bubble_rect.left() + 24.0).min(bubble_rect.center().x),
                    bubble_rect.center().y,
                );
                right_click_at(&mut h, click_pos);
                step_frames(&mut h, 10);
            }
            save_wp61_png(&mut h, &format!("chat_engagement_{tag}_after"));
        }

        // 2. Broadcast channel pull-to-next-channel ready indicator + footer
        {
            let mut h = build_wp61_snap_harness(size);
            if let Some(session) = h.state_mut().controller.session_mut() {
                session.show_peer_history(
                    session_peers::CHANNEL_DEV_LOG,
                    SectionShow::default(),
                    MsgId::SHOW_AT_END,
                );
            }
            settle_layout(&mut h);
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .request_scroll_to_bottom(session_peers::CHANNEL_DEV_LOG);
            settle_layout(&mut h);
            let now = h.ctx.input(|i| i.time);
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .pull_next_channel_mut()
                .set_pull_preview(104.0, now);
            step_frames(&mut h, 4);
            save_wp61_png(&mut h, &format!("channel_pull_next_{tag}_after"));
        }

        // 3. Birthday suggestion card with bundled `cake.tgs` Lottie emblem
        {
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
            let _ = push_birthday_card(&state, session_peers::USER_MIRA);
            if let Some(session) = state.controller.session_mut() {
                session.show_peer_history(
                    session_peers::USER_MIRA,
                    SectionShow::default(),
                    MsgId::SHOW_AT_END,
                );
            }
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .request_scroll_to_bottom(session_peers::USER_MIRA);
            settle_layout(&mut h);
            save_wp61_png(&mut h, &format!("birthday_cake_card_{tag}_after"));
        }

        // 4. ConfirmPhoneCodeBox modal (`tg://confirmphone?phone=15550100&hash=h99`)
        {
            let mut h = build_wp61_snap_harness(size);
            h.state_mut()
                .controller
                .open_link("tg://confirmphone?phone=15550100&hash=h99");
            settle_layout(&mut h);
            save_wp61_png(&mut h, &format!("confirm_phone_box_{tag}_after"));
        }

        // 5. Leave group confirmation modal with future creator notice (`getFutureChatCreatorAfterLeave`)
        {
            let mut h = build_wp61_snap_harness(size);
            let ctx = h.ctx.clone();
            let colors = LiveThemeColors::default();
            let creator = HistoryBackend::future_chat_creator_after_leave(
                h.state_mut().controller.session_backend_mut(),
                session_peers::GROUP_CORE,
            )
            .ok()
            .flatten();
            if let Some(peer) = InfoBackend::peer_profile(
                h.state().controller.session_backend(),
                session_peers::GROUP_CORE,
            ) {
                let box_state = DeleteMessagesBoxState::for_delete_or_leave(&peer, false)
                    .with_future_creator(creator);
                h.state_mut().controller.box_host_mut().push(
                    &ctx,
                    BoxKind::DeleteMessages(box_state),
                    &colors,
                );
            }
            settle_layout(&mut h);
            save_wp61_png(&mut h, &format!("leave_group_creator_{tag}_after"));
        }
    }
}
