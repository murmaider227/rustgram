//! Live backend unit & wire-encoding tests for WP-54 (Bots, Inline Queries & Mini Apps)
//! over an in-process `FakeRpcClient`.
//!
//! Zero real network traffic, zero real accounts or credentials, fixture URLs only.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_api::convert::apply_tl_users_and_chats;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_app_core::bots::backend::BotsBackend;
use tg_app_core::stand_in::types::PeerId as AppPeerId;
use tg_app_core::webview::backend::WebViewBackend;
use tg_bots::callback::{AcceptUrlAuthParams, CallbackQueryRequest, UrlAuthSource};
use tg_bots::mini_app::{MINI_APP_PLATFORM, MiniAppLaunchParams, ThemeParams, WebViewSource};
use tg_bots::protocol::{RequestedWebViewButton, StartBotParams, UrlAuthMatchCodeResult};
use tg_bots::stand_in::{
    BotAppId, MsgId as BotMsgId, PeerId as BotPeerId, Rgba, UserId as BotUserId,
};
use tg_data::{ChannelId, ChatId, PeerId as DataPeerId, UserId as DataUserId};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    BOOL_FALSE_ID, BOOL_TRUE_ID, Deserializable, Identifiable, RemoteCall, Serializable,
    enums as tl_enums, functions as tl_fns, types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;

#[derive(Clone)]
struct CapturedRpc {
    ctor: u32,
    bytes: Vec<u8>,
}

type QueuedReply = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct BotsFakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    captured: Arc<Mutex<Vec<CapturedRpc>>>,
    responses: Arc<Mutex<VecDeque<QueuedReply>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl BotsFakeRpcClient {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(32);
        Self {
            main_dc: Arc::new(Mutex::new(2)),
            captured: Arc::new(Mutex::new(Vec::new())),
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

    fn enqueue_bool(&self, expected_ctor: u32, val: bool) {
        let id = if val { BOOL_TRUE_ID } else { BOOL_FALSE_ID };
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(id.to_le_bytes().to_vec())));
    }

    fn enqueue_rpc_err(&self, expected_ctor: u32, code: i32, name: &str) {
        self.responses.lock().expect("lock responses").push_back((
            expected_ctor,
            Err(InvocationError::Rpc(RpcError {
                code,
                name: name.to_owned(),
                value: None,
            })),
        ));
    }

    fn captured(&self) -> Vec<CapturedRpc> {
        self.captured.lock().expect("lock captured").clone()
    }
}

impl RpcClient for BotsFakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let dc = self.main_dc_id();
        self.invoke_in_dc(dc, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.captured
            .lock()
            .expect("lock captured")
            .push(CapturedRpc { ctor, bytes });

        let next = {
            let mut q = self.responses.lock().expect("lock responses");
            if let Some(pos) = q.iter().position(|(c, _)| *c == ctor) {
                q.remove(pos).map(|(_, r)| r)
            } else {
                q.pop_front().map(|(_, r)| r)
            }
        };
        let raw = next.unwrap_or(Err(InvocationError::Dropped))?;
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock main_dc")
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        *self.main_dc.lock().expect("lock main_dc") = dc_id;
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn synthetic_user(id: i64, username: &str, name: &str, is_bot: bool) -> tl_enums::User {
    tl_enums::User::User(tl_types::User {
        self_: id == 100,
        contact: false,
        mutual_contact: false,
        deleted: false,
        bot: is_bot,
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
        bot_attach_menu: is_bot,
        premium: false,
        attach_menu_enabled: is_bot,
        bot_can_edit: false,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: is_bot,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(id * 1000 + 9),
        first_name: Some(name.to_owned()),
        last_name: None,
        username: Some(username.to_owned()),
        phone: None,
        photo: None,
        status: None,
        bot_info_version: is_bot.then_some(1),
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

fn synthetic_group(id: i64, title: &str) -> tl_enums::Chat {
    tl_enums::Chat::Chat(tl_types::Chat {
        creator: true,
        left: false,
        deactivated: false,
        call_active: false,
        call_not_empty: false,
        noforwards: false,
        id,
        title: title.to_owned(),
        photo: tl_enums::ChatPhoto::Empty,
        participants_count: 10,
        date: 1_700_000_000,
        version: 1,
        migrated_to: None,
        admin_rights: None,
        default_banned_rights: None,
    })
}

fn synthetic_broadcast_channel(id: i64, title: &str) -> tl_enums::Chat {
    tl_enums::Chat::Channel(tl_types::Channel {
        creator: true,
        left: false,
        broadcast: true,
        verified: false,
        megagroup: false,
        restricted: false,
        signatures: false,
        min: false,
        scam: false,
        has_link: false,
        has_geo: false,
        slowmode_enabled: false,
        call_active: false,
        call_not_empty: false,
        fake: false,
        gigagroup: false,
        noforwards: false,
        join_to_send: false,
        join_request: false,
        forum: false,
        stories_hidden: false,
        stories_hidden_min: false,
        stories_unavailable: false,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        in_news: false,
        forum_sidebar: false,
        id,
        access_hash: Some(id * 1000 + 5),
        title: title.to_owned(),
        username: Some("fixture_broadcast".to_owned()),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_700_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(50),
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: None,
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_monoforum_id: None,
        linked_community_id: None,
        pinned_msg_id: None,
    })
}

fn empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_700_000_200,
        seq: 1,
    })
}

fn build_live_session() -> (tokio::runtime::Runtime, BotsFakeRpcClient, LiveSession) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build tokio runtime");
    let rpc = BotsFakeRpcClient::new();
    let cfg = SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..SessionRuntimeConfig::default()
    };
    let runtime = Arc::new(
        SessionRuntime::start_on(
            DataUserId(100),
            crate::ErasedRpcClient::new(rpc.clone()),
            rt.handle().clone(),
            cfg,
        )
        .expect("start test runtime"),
    );
    rt.block_on(async {
        let mut s = runtime.session().write().await;
        apply_tl_users_and_chats(
            &mut s,
            &[
                synthetic_user(100, "self_user", "Self", false),
                synthetic_user(500, "fixture_bot", "Fixture Bot", true),
            ],
            &[
                synthetic_group(800, "Fixture Group"),
                synthetic_broadcast_channel(900, "Fixture Broadcast"),
            ],
        );
    });
    let live = LiveSession::new(runtime, 1);
    (rt, rpc, live)
}

#[test]
fn live_start_bot_private_group_broadcast_and_empty_token() {
    let (_rt, rpc, mut live) = build_live_session();
    let group_peer = BotPeerId(DataPeerId::from_chat(ChatId(800)).0);
    let broadcast_peer = BotPeerId(DataPeerId::from_channel(ChannelId(900)).0);

    // 1. 1:1 `?start=deep_tok` invokes `messages.startBot` with `peer = InputPeer::Empty`.
    rpc.enqueue_ok(tl_fns::messages::StartBot::CONSTRUCTOR_ID, &empty_updates());
    live.start_bot(&StartBotParams::for_private_chat(
        BotUserId(500),
        "deep_tok",
        12345,
    ))
    .expect("1:1 start_bot ok");

    // 2. Group `?startgroup=grp_tok` invokes `messages.startBot` with `InputPeer::Chat(800)`.
    rpc.enqueue_ok(tl_fns::messages::StartBot::CONSTRUCTOR_ID, &empty_updates());
    live.start_bot(&StartBotParams::for_group_chat(
        BotUserId(500),
        group_peer,
        "grp_tok",
        67890,
    ))
    .expect("group start_bot ok");

    // 3. Broadcast channel is rejected locally with USER_BOT (no RPC sent).
    let err = live
        .start_bot(&StartBotParams::for_group_chat(
            BotUserId(500),
            broadcast_peer,
            "tok",
            1,
        ))
        .expect_err("broadcast channel must fail");
    assert!(err.contains("USER_BOT"));

    // 4. Empty start_param falls back to plain `/start@fixture_bot` via `messages.sendMessage`.
    rpc.enqueue_ok(
        tl_fns::messages::SendMessage::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    live.start_bot(&StartBotParams::for_group_chat(
        BotUserId(500),
        group_peer,
        "",
        0,
    ))
    .expect("empty start_param group fallback ok");

    let cap = rpc.captured();
    assert_eq!(cap.len(), 3);
    let sb1 = tl_fns::messages::StartBot::from_bytes(&cap[0].bytes).unwrap();
    assert_eq!(sb1.peer, tl_enums::InputPeer::Empty);
    assert_eq!(sb1.start_param, "deep_tok");
    assert_eq!(sb1.random_id, 12345);

    let sb2 = tl_fns::messages::StartBot::from_bytes(&cap[1].bytes).unwrap();
    assert!(matches!(sb2.peer, tl_enums::InputPeer::Chat(c) if c.chat_id == 800));
    assert_eq!(sb2.start_param, "grp_tok");

    let sm = tl_fns::messages::SendMessage::from_bytes(&cap[2].bytes).unwrap();
    assert_eq!(sm.message, "/start@fixture_bot");
}

#[test]
fn live_check_url_auth_match_code_and_bot_recommendations() {
    let (_rt, rpc, mut live) = build_live_session();

    rpc.enqueue_bool(
        tl_fns::messages::CheckUrlAuthMatchCode::CONSTRUCTOR_ID,
        true,
    );
    rpc.enqueue_bool(
        tl_fns::messages::CheckUrlAuthMatchCode::CONSTRUCTOR_ID,
        false,
    );
    rpc.enqueue_rpc_err(
        tl_fns::messages::CheckUrlAuthMatchCode::CONSTRUCTOR_ID,
        400,
        "URL_EXPIRED",
    );

    assert_eq!(
        live.check_url_auth_match_code("https://fixture.invalid/auth", "🚀")
            .unwrap(),
        UrlAuthMatchCodeResult::Matched {
            match_code: "🚀".to_owned()
        }
    );
    assert_eq!(
        live.check_url_auth_match_code("https://fixture.invalid/auth", "🦊")
            .unwrap(),
        UrlAuthMatchCodeResult::MismatchOrExpired
    );
    assert_eq!(
        live.check_url_auth_match_code("https://fixture.invalid/expired", "🚀")
            .unwrap(),
        UrlAuthMatchCodeResult::Expired
    );

    // bots.getBotRecommendations
    rpc.enqueue_ok(
        tl_fns::bots::GetBotRecommendations::CONSTRUCTOR_ID,
        &tl_enums::users::Users::Slice(tl_types::users::UsersSlice {
            count: 4,
            users: vec![synthetic_user(601, "rec_bot", "Rec Bot", true)],
        }),
    );
    let recs = live.get_bot_recommendations(BotUserId(500)).unwrap();
    assert_eq!(recs.bot_ids, vec![BotUserId(601)]);
    assert_eq!(recs.more_count, 3);
}

#[test]
fn live_webview_get_bot_app_chat_join_and_all_bots_permission_rpcs() {
    let (_rt, rpc, mut live) = build_live_session();

    // 1. messages.getBotApp (fresh -> caches hash -> NotModified resolves cached descriptor)
    rpc.enqueue_ok(
        tl_fns::messages::GetBotApp::CONSTRUCTOR_ID,
        &tl_enums::messages::BotApp::BotApp(tl_types::messages::BotApp {
            inactive: true,
            request_write_access: true,
            has_settings: true,
            app: tl_enums::BotApp::BotApp(tl_types::BotApp {
                id: 7001,
                access_hash: 888_999,
                short_name: "shop".to_owned(),
                title: "Fixture Shop".to_owned(),
                description: "Demo".to_owned(),
                photo: tl_enums::Photo::Empty(tl_types::PhotoEmpty { id: 0 }),
                document: None,
                hash: 123_456,
            }),
        }),
    );
    rpc.enqueue_ok(
        tl_fns::messages::GetBotApp::CONSTRUCTOR_ID,
        &tl_enums::messages::BotApp::BotApp(tl_types::messages::BotApp {
            inactive: false,
            request_write_access: false,
            has_settings: true,
            app: tl_enums::BotApp::NotModified,
        }),
    );
    let first = live.get_bot_app(500, "shop", 0).unwrap();
    assert_eq!(first.app.as_ref().unwrap().id, BotAppId(7001));
    assert!(!first.not_modified);

    let second = live.get_bot_app(500, "shop", 0).unwrap();
    assert!(second.not_modified);
    assert_eq!(second.app.as_ref().unwrap().title, "Fixture Shop");

    // 2. messages.requestChatJoinWebView (both direct & via request_web_view_full JoinChat)
    rpc.enqueue_ok(
        tl_fns::messages::RequestChatJoinWebView::CONSTRUCTOR_ID,
        &tl_enums::WebViewResult::Url(tl_types::WebViewResultUrl {
            fullsize: false,
            fullscreen: false,
            same_origin: true,
            query_id: Some(99_001),
            url: "https://fixture.invalid/join".to_owned(),
        }),
    );
    let join_launch = live.request_chat_join_web_view(500, 99_001, None).unwrap();
    assert_eq!(join_launch.url, "https://fixture.invalid/join");
    assert_eq!(join_launch.query_id, 99_001);

    // Also verify WebViewSource::JoinChat in request_web_view_full routes to RequestChatJoinWebView
    rpc.enqueue_ok(
        tl_fns::messages::RequestChatJoinWebView::CONSTRUCTOR_ID,
        &tl_enums::WebViewResult::Url(tl_types::WebViewResultUrl {
            fullsize: false,
            fullscreen: false,
            same_origin: true,
            query_id: Some(99_002),
            url: "https://fixture.invalid/join2".to_owned(),
        }),
    );
    let params = MiniAppLaunchParams::new(
        BotUserId(500),
        WebViewSource::JoinChat { query_id: 99_002 },
        ThemeParams {
            body_bg: Rgba::rgb(255, 255, 255),
            title_bg: Rgba::TRANSPARENT,
            scroll_bg: Rgba::rgb(0, 0, 0),
            scroll_bg_over: Rgba::rgb(0, 0, 0),
            scroll_bar_bg: Rgba::rgb(0, 0, 0),
            scroll_bar_bg_over: Rgba::rgb(0, 0, 0),
            json: String::new(),
        },
    );
    let join2 = live.request_web_view_full(&params).unwrap();
    assert_eq!(join2.url, "https://fixture.invalid/join2");

    // 3. bots.canSendMessage & bots.allowSendMessage
    rpc.enqueue_bool(tl_fns::bots::CanSendMessage::CONSTRUCTOR_ID, false);
    assert!(!live.can_send_message(500).unwrap());
    rpc.enqueue_ok(
        tl_fns::bots::AllowSendMessage::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    assert!(live.allow_send_message(500).unwrap());

    // 4. bots.checkDownloadFileParams
    rpc.enqueue_bool(tl_fns::bots::CheckDownloadFileParams::CONSTRUCTOR_ID, true);
    assert!(
        live.check_download_file_params(500, "file.txt", "https://fixture.invalid/file.txt")
            .unwrap()
    );
    assert!(
        live.check_download_file_params(500, "", "https://fixture.invalid/file.txt")
            .is_err()
    );

    // 5. bots.getPopularAppBots (filters non-bots, defaults limit to 100)
    rpc.enqueue_ok(
        tl_fns::bots::GetPopularAppBots::CONSTRUCTOR_ID,
        &tl_enums::bots::PopularAppBots::PopularAppBots(tl_types::bots::PopularAppBots {
            next_offset: Some("next_1".to_owned()),
            users: vec![
                synthetic_user(510, "pop_bot", "Pop Bot", true),
                synthetic_user(511, "not_a_bot", "Human", false),
            ],
        }),
    );
    let pop = live.get_popular_app_bots("", 0).unwrap();
    assert_eq!(pop.bot_ids, vec![BotUserId(510)]);
    assert_eq!(pop.next_offset.as_deref(), Some("next_1"));

    // 6. bots.getRequestedWebViewButton + send_requested_web_view_peers
    rpc.enqueue_ok(
        tl_fns::bots::GetRequestedWebViewButton::CONSTRUCTOR_ID,
        &tl_enums::KeyboardButton::KeyboardButton(tl_types::KeyboardButton {
            style: None,
            text: "Create".to_owned(),
            r#type: tl_enums::ButtonType::RequestPeer(tl_types::ButtonTypeRequestPeer {
                button_id: 55,
                peer_type: tl_enums::RequestPeerType::CreateBot(
                    tl_types::RequestPeerTypeCreateBot {
                        bot_managed: true,
                        suggested_name: Some("Child Bot".to_owned()),
                        suggested_username: Some("child_bot".to_owned()),
                    },
                ),
                max_quantity: 1,
            }),
        }),
    );
    let btn = live.get_requested_web_view_button(500, "req_99").unwrap();
    assert!(matches!(
        btn,
        RequestedWebViewButton::CreateBot { button_id: 55, ref request }
            if request.suggested_username == "child_bot"
    ));

    rpc.enqueue_ok(
        tl_fns::messages::SendBotRequestedPeer::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    live.send_requested_web_view_peers(
        500,
        "req_99",
        55,
        &[AppPeerId(DataPeerId::from_chat(ChatId(800)).0)],
    )
    .unwrap();

    // 7. bots.toggleUserEmojiStatusPermission
    rpc.enqueue_bool(
        tl_fns::bots::ToggleUserEmojiStatusPermission::CONSTRUCTOR_ID,
        true,
    );
    assert!(live.toggle_user_emoji_status_permission(500, true).unwrap());

    // Verify wire details of captured requests
    let cap = rpc.captured();
    let get_apps: Vec<_> = cap
        .iter()
        .filter(|r| r.ctor == tl_fns::messages::GetBotApp::CONSTRUCTOR_ID)
        .map(|r| tl_fns::messages::GetBotApp::from_bytes(&r.bytes).unwrap())
        .collect();
    assert_eq!(get_apps.len(), 2);
    assert_eq!(get_apps[0].hash, 0);
    assert_eq!(get_apps[1].hash, 123_456);

    let pop_req = cap
        .iter()
        .find(|r| r.ctor == tl_fns::bots::GetPopularAppBots::CONSTRUCTOR_ID)
        .map(|r| tl_fns::bots::GetPopularAppBots::from_bytes(&r.bytes).unwrap())
        .unwrap();
    assert_eq!(pop_req.limit, 100);

    let send_peer_req = cap
        .iter()
        .find(|r| r.ctor == tl_fns::messages::SendBotRequestedPeer::CONSTRUCTOR_ID)
        .map(|r| tl_fns::messages::SendBotRequestedPeer::from_bytes(&r.bytes).unwrap())
        .unwrap();
    assert!(send_peer_req.msg_id.is_none());
    assert_eq!(send_peer_req.webapp_req_id.as_deref(), Some("req_99"));
    assert_eq!(send_peer_req.button_id, 55);

    // Ensure no prolongWebView was spawned for JoinChat
    assert!(
        cap.iter()
            .all(|r| r.ctor != tl_fns::messages::ProlongWebView::CONSTRUCTOR_ID)
    );
}

#[test]
fn live_flag_omission_and_prolong_options_conformance() {
    let (_rt, rpc, mut live) = build_live_session();
    let bot_peer = BotPeerId(DataPeerId::from_user(DataUserId(500)).0);

    // 1. Game callback must omit data even if caller populated Some(vec![...])
    rpc.enqueue_ok(
        tl_fns::messages::GetBotCallbackAnswer::CONSTRUCTOR_ID,
        &tl_enums::messages::BotCallbackAnswer::BotCallbackAnswer(
            tl_types::messages::BotCallbackAnswer {
                alert: false,
                has_url: true,
                native_ui: false,
                message: None,
                url: Some("https://fixture.invalid/game".to_owned()),
                cache_time: 0,
            },
        ),
    );
    let _ = live
        .send_callback_query(&CallbackQueryRequest {
            peer_id: bot_peer,
            msg_id: BotMsgId(10),
            is_game: true,
            data: Some(b"ignored".to_vec()),
            requires_password: false,
            ephemeral_id: None,
        })
        .unwrap();

    // 2. Ephemeral callback with empty data omits f_data
    rpc.enqueue_ok(
        tl_fns::ephemeral::GetCallbackAnswer::CONSTRUCTOR_ID,
        &tl_enums::messages::BotCallbackAnswer::BotCallbackAnswer(
            tl_types::messages::BotCallbackAnswer {
                alert: false,
                has_url: false,
                native_ui: false,
                message: Some("ok".to_owned()),
                url: None,
                cache_time: 0,
            },
        ),
    );
    let _ = live
        .send_callback_query(&CallbackQueryRequest {
            peer_id: bot_peer,
            msg_id: BotMsgId(11),
            is_game: false,
            data: Some(Vec::new()),
            requires_password: false,
            ephemeral_id: Some(77),
        })
        .unwrap();

    // 3. AcceptUrlAuth with Some("") match_code omits f_match_code
    rpc.enqueue_ok(
        tl_fns::messages::AcceptUrlAuth::CONSTRUCTOR_ID,
        &tl_enums::UrlAuthResult::Default,
    );
    let _ = live
        .accept_url_auth(&AcceptUrlAuthParams {
            source: UrlAuthSource::Url {
                url: "https://fixture.invalid/login".to_owned(),
            },
            write_allowed: false,
            share_phone_number: false,
            match_code: Some(String::new()),
        })
        .unwrap();

    // 4. RequestSimpleWebView(SwitchWebView) omits start_param; ProlongWebView propagates silent
    rpc.enqueue_ok(
        tl_fns::messages::RequestSimpleWebView::CONSTRUCTOR_ID,
        &tl_enums::WebViewResult::Url(tl_types::WebViewResultUrl {
            fullsize: false,
            fullscreen: false,
            same_origin: true,
            query_id: None,
            url: "https://fixture.invalid/switch".to_owned(),
        }),
    );
    let mut sw = MiniAppLaunchParams::new(
        BotUserId(500),
        WebViewSource::SwitchWebView,
        ThemeParams {
            body_bg: Rgba::rgb(255, 255, 255),
            title_bg: Rgba::TRANSPARENT,
            scroll_bg: Rgba::rgb(0, 0, 0),
            scroll_bg_over: Rgba::rgb(0, 0, 0),
            scroll_bar_bg: Rgba::rgb(0, 0, 0),
            scroll_bar_bg_over: Rgba::rgb(0, 0, 0),
            json: String::new(),
        },
    );
    sw.url = Some("https://fixture.invalid/switch".to_owned());
    sw.start_param = Some("must_be_omitted".to_owned());
    let _ = live.request_web_view_full(&sw).unwrap();

    // 5. ProlongWebView with silent=true fires and propagates silent flag
    rpc.enqueue_bool(tl_fns::messages::ProlongWebView::CONSTRUCTOR_ID, false);
    let handle = live.start_prolong_web_view_with_options(
        AppPeerId(bot_peer.0),
        500,
        44_001,
        Duration::from_millis(10),
        true,
        None,
    );
    std::thread::sleep(Duration::from_millis(80));
    handle.stop();

    let cap = rpc.captured();
    let cb = tl_fns::messages::GetBotCallbackAnswer::from_bytes(&cap[0].bytes).unwrap();
    assert!(cb.game);
    assert!(cb.data.is_none());

    let eph = tl_fns::ephemeral::GetCallbackAnswer::from_bytes(&cap[1].bytes).unwrap();
    assert!(eph.data.is_none());

    let acc = tl_fns::messages::AcceptUrlAuth::from_bytes(&cap[2].bytes).unwrap();
    assert!(acc.match_code.is_none());

    let simple = tl_fns::messages::RequestSimpleWebView::from_bytes(&cap[3].bytes).unwrap();
    assert_eq!(simple.platform, MINI_APP_PLATFORM);
    assert!(simple.from_switch_webview);
    assert!(simple.start_param.is_none());

    let prolong = cap
        .iter()
        .find(|r| r.ctor == tl_fns::messages::ProlongWebView::CONSTRUCTOR_ID)
        .map(|r| tl_fns::messages::ProlongWebView::from_bytes(&r.bytes).unwrap())
        .expect("prolongWebView should have fired");
    assert!(prolong.silent);
    assert_eq!(prolong.query_id, 44_001);
}
