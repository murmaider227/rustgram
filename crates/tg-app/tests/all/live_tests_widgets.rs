//! `FakeRpcClient` tests for live widget RPCs (`live/widgets.rs`), Layer-233 TL
//! `messageMediaWidget` conversion, `tg_data::MessageMedia::Widget` -> `HistoryMedia::Widget`
//! conversion, and `MockSession` widget demo data.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::{
    Message, MessageContent, MessageMedia, MsgId as DataMsgId, PeerId as DataPeerId,
    PeerUpdateFlags, User, UserId as DataUserId, WidgetData,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tgw::{
    PublicDataPollResult, RendererToHostMsg, RuntimeState, WidgetRequestFailure,
    WidgetRequestOutcome, pack_payload_zlib,
};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use tg_app::history::backend::{HistoryBackend, HistoryMedia};
use tg_app::live::LiveSession;
use tg_app::live::convert::{message_to_history_message, now_unix_i32};
use tg_app::live::widgets::{
    LiveWidgetHost, apply_widget_data_update, attach_cached_widget_states,
    record_widget_updates_from_batch, tl_message_media_widget_to_attachment,
    tl_message_media_widget_to_fields, widget_edit_shared_data_live, widget_query_typed_live,
    widgets_state_for_session,
};
use tg_app::stand_in::mock_session::{MockSession, session_peers};
use tg_app::stand_in::types::{MsgId, PeerId};

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
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(32);
        Self {
            main_dc: Arc::new(Mutex::new(2)),
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

    fn enqueue_err(&self, expected_ctor: u32, err: InvocationError) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Err(err)));
    }

    fn recorded(&self) -> Vec<RequestCapture> {
        self.recorded.lock().expect("lock recorded").clone()
    }

    fn clear_recorded(&self) {
        self.recorded.lock().expect("lock recorded").clear();
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
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, bytes));
        let next = {
            let mut q = self.responses.lock().expect("lock responses");
            q.iter()
                .position(|(c, _)| *c == ctor)
                .and_then(|pos| q.remove(pos))
                .map(|(_, r)| r)
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

fn synthetic_self_user(id: i64) -> tl_types::User {
    tl_types::User {
        self_: true,
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
        access_hash: Some(0x1111_2222),
        first_name: Some("Alex".to_owned()),
        last_name: None,
        username: Some("alex_tester".to_owned()),
        phone: None,
        photo: None,
        status: None,
        bot_info_version: None,
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: Some("en".to_owned()),
        emoji_status: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    }
}

fn build_live_widget_fixture() -> (tokio::runtime::Runtime, FakeRpcClient, LiveSession, PeerId) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build tokio rt");
    let rpc = FakeRpcClient::new();
    let self_uid = DataUserId::from_tl(100_001);
    let bot_uid = DataUserId::from_tl(880_001);
    let bot_peer = DataPeerId::from_user(bot_uid);

    rpc.enqueue_ok(
        tl_fns::users::GetUsers::CONSTRUCTOR_ID,
        &vec![tl_enums::User::User(synthetic_self_user(100_001))],
    );
    rpc.enqueue_ok(
        tl_fns::updates::GetState::CONSTRUCTOR_ID,
        &tl_enums::updates::State::State(tl_types::updates::State {
            pts: 10,
            qts: 0,
            date: now_unix_i32(),
            seq: 1,
            unread_count: 0,
        }),
    );
    rpc.enqueue_ok(
        tl_fns::updates::GetDifference::CONSTRUCTOR_ID,
        &tl_enums::updates::Difference::Empty(tl_types::updates::DifferenceEmpty {
            date: now_unix_i32(),
            seq: 1,
        }),
    );
    rpc.enqueue_ok(
        tl_fns::messages::GetDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
            dialogs: Vec::new(),
            messages: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    rpc.enqueue_ok(
        tl_fns::messages::GetPinnedDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
            dialogs: Vec::new(),
            messages: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
            state: tl_enums::updates::State::State(tl_types::updates::State {
                pts: 10,
                qts: 0,
                date: now_unix_i32(),
                seq: 1,
                unread_count: 0,
            }),
        }),
    );

    let runtime = Arc::new(
        SessionRuntime::start_on(
            self_uid,
            tg_app::live::ErasedRpcClient::new(rpc.clone()),
            rt.handle().clone(),
            SessionRuntimeConfig::default(),
        )
        .expect("start widget test runtime"),
    );
    rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(15), runtime.wait_ready())
            .await
            .expect("wait_ready timeout")
            .expect("startup ready");
    });

    let widget_html = b"<title>Live Ticker</title><div>Price: 12.50</div>";
    let payload = pack_payload_zlib(widget_html);

    rt.block_on(async {
        let mut s = runtime.session().write().await;
        let mut bot = User::new(bot_uid, false);
        bot.first_name = "Ticker Bot".to_owned();
        bot.access_hash = 0x1234_5678_9ABC;
        s.upsert_user(bot, PeerUpdateFlags::empty());

        let mut msg = Message::new_text(
            bot_peer,
            DataMsgId(42),
            Some(bot_peer),
            now_unix_i32(),
            tg_data::TextWithEntities::plain("Live widget"),
        );
        msg.content = MessageContent::Regular {
            text: tg_data::TextWithEntities::plain("Live widget"),
            media: MessageMedia::Widget(Box::new(WidgetData {
                widget_id: Some(77_001),
                width: 320,
                height: 200,
                payload,
                is_copy: false,
            })),
        };
        s.insert_message(msg);
    });
    rpc.clear_recorded();

    let live = LiveSession::new(runtime, 701);
    (rt, rpc, live, PeerId(bot_peer.0))
}

#[test]
fn fake_rpc_widget_query_answer_and_error_mapping() {
    let (_rt, rpc, mut live, bot_peer) = build_live_widget_fixture();

    // 1. Successful query returning a JSON result.
    rpc.enqueue_ok(
        tl_fns::messages::GetBotWidgetQueryAnswer::CONSTRUCTOR_ID,
        &tl_enums::messages::BotWidgetQueryAnswer::BotWidgetQueryAnswer(
            tl_types::messages::BotWidgetQueryAnswer {
                result: Some(r#"{"status":"voted","count":11}"#.to_owned()),
            },
        ),
    );
    let ok = live
        .widget_query(bot_peer, MsgId(42), b"action=vote", true)
        .expect("widget query rpc");
    assert_eq!(
        ok,
        WidgetRequestOutcome::ok(Some(r#"{"status":"voted","count":11}"#.to_owned()))
    );

    // Verify wire request encoded `with_wallet = true` and data bytes.
    let recorded = rpc.recorded();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].0,
        tl_fns::messages::GetBotWidgetQueryAnswer::CONSTRUCTOR_ID
    );
    let decoded_req = tl_fns::messages::GetBotWidgetQueryAnswer::from_bytes(&recorded[0].1)
        .expect("decode GetBotWidgetQueryAnswer");
    assert_eq!(decoded_req.msg_id, 42);
    assert_eq!(decoded_req.data, b"action=vote");
    assert_eq!(
        decoded_req.query_type,
        tl_enums::WidgetQueryType::Data(tl_types::WidgetQueryTypeData { with_wallet: true })
    );

    // 2. WIDGET_RESPONSE_TIMEOUT maps to Timeout.
    rpc.enqueue_err(
        tl_fns::messages::GetBotWidgetQueryAnswer::CONSTRUCTOR_ID,
        InvocationError::Rpc(RpcError {
            code: 400,
            name: "WIDGET_RESPONSE_TIMEOUT".to_owned(),
            value: None,
        }),
    );
    let timeout = live
        .widget_query(bot_peer, MsgId(42), b"slow", false)
        .expect("timeout outcome");
    assert_eq!(
        timeout,
        WidgetRequestOutcome::err(WidgetRequestFailure::Timeout)
    );

    // 3. FLOOD_WAIT_20 maps to RateLimited.
    rpc.enqueue_err(
        tl_fns::messages::GetBotWidgetQueryAnswer::CONSTRUCTOR_ID,
        InvocationError::Rpc(RpcError {
            code: 420,
            name: "FLOOD_WAIT_20".to_owned(),
            value: Some(20),
        }),
    );
    let rate_limited = live
        .widget_query(bot_peer, MsgId(42), b"burst", false)
        .expect("rate limited outcome");
    assert_eq!(
        rate_limited,
        WidgetRequestOutcome::err(WidgetRequestFailure::RateLimited)
    );

    // 4. Generic RPC error maps to Failed.
    rpc.enqueue_err(
        tl_fns::messages::GetBotWidgetQueryAnswer::CONSTRUCTOR_ID,
        InvocationError::Rpc(RpcError {
            code: 400,
            name: "MESSAGE_ID_INVALID".to_owned(),
            value: None,
        }),
    );
    let failed = live
        .widget_query(bot_peer, MsgId(42), b"bad", false)
        .expect("failed outcome");
    assert_eq!(
        failed,
        WidgetRequestOutcome::err(WidgetRequestFailure::Failed)
    );

    // 5. Typed query with empty answer.
    rpc.enqueue_ok(
        tl_fns::messages::GetBotWidgetQueryAnswer::CONSTRUCTOR_ID,
        &tl_enums::messages::BotWidgetQueryAnswer::BotWidgetQueryAnswer(
            tl_types::messages::BotWidgetQueryAnswer { result: None },
        ),
    );
    let empty_ans = widget_query_typed_live(
        &live,
        bot_peer,
        MsgId(42),
        tl_enums::WidgetQueryType::Data(tl_types::WidgetQueryTypeData { with_wallet: false }),
        b"ping",
    )
    .expect("empty answer");
    assert_eq!(empty_ans, WidgetRequestOutcome::ok(None));
}

#[test]
fn fake_rpc_widget_public_data_poll_and_edit_rpcs() {
    let (rt, rpc, mut live, bot_peer) = build_live_widget_fixture();

    // 1. Poll returns updated WidgetPublicData with period clamped from 2s -> 5s (5000ms).
    rpc.enqueue_ok(
        tl_fns::messages::GetWidgetPublicData::CONSTRUCTOR_ID,
        &tl_enums::messages::WidgetPublicData::WidgetPublicData(
            tl_types::messages::WidgetPublicData {
                hash: 999_111,
                data: r#"{"score":42}"#.to_owned(),
                period: 2,
            },
        ),
    );
    let updated = live
        .widget_poll_public_data(bot_peer, MsgId(42), 0)
        .expect("poll public data");
    assert_eq!(
        updated,
        PublicDataPollResult::Updated {
            hash: 999_111,
            data: r#"{"score":42}"#.to_owned(),
            period_ms: 5_000,
        }
    );

    // Verify poller cached the updated state under widget_id 77_001.
    let st_arc = widgets_state_for_session(live.runtime().session());
    {
        let st = st_arc.lock().expect("lock widgets state");
        assert_eq!(
            st.poller.latest_public_data(77_001),
            Some((Some(999_111), r#"{"score":42}"#))
        );
    }

    // 2. Poll returns NotModified with period clamped from 99999s -> 3600s (3_600_000ms).
    rpc.enqueue_ok(
        tl_fns::messages::GetWidgetPublicData::CONSTRUCTOR_ID,
        &tl_enums::messages::WidgetPublicData::NotModified(
            tl_types::messages::WidgetPublicDataNotModified { period: 99_999 },
        ),
    );
    let not_mod = live
        .widget_poll_public_data(bot_peer, MsgId(42), 999_111)
        .expect("poll not modified");
    assert_eq!(
        not_mod,
        PublicDataPollResult::NotModified {
            period_ms: 3_600_000,
        }
    );

    // 3. messages.editMessageWidgetData
    rpc.enqueue_ok(
        tl_fns::messages::EditMessageWidgetData::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::MessageWidgetData(
                tl_types::UpdateMessageWidgetData {
                    peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 880_001 }),
                    msg_id: 42,
                    public_data: Some(r#"{"score":50}"#.to_owned()),
                    public_data_hash: Some(555_001),
                    chat_data: Some(r#"{"local":true}"#.to_owned()),
                    pts: 10,
                    pts_count: 1,
                },
            )],
            users: Vec::new(),
            chats: Vec::new(),
            date: now_unix_i32(),
            seq: 1,
        }),
    );
    live.widget_edit_data(
        bot_peer,
        MsgId(42),
        Some(r#"{"score":50}"#),
        Some(r#"{"local":true}"#),
    )
    .expect("editMessageWidgetData");

    // 4. messages.editWidgetData
    rpc.enqueue_ok(tl_fns::messages::EditWidgetData::CONSTRUCTOR_ID, &true);
    let edited = widget_edit_shared_data_live(&live, 77_001, r#"{"score":77}"#, Some(60))
        .expect("editWidgetData");
    assert!(edited);

    // 5. Direct UpdateMessageWidgetData application and batch recording + HistoryMessage overlay
    let changed = apply_widget_data_update(
        live.runtime().session(),
        &tl_types::UpdateMessageWidgetData {
            peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 880_001 }),
            msg_id: 42,
            public_data: Some(r#"{"score":88}"#.to_owned()),
            public_data_hash: Some(888_002),
            chat_data: None,
            pts: 11,
            pts_count: 1,
        },
    );
    assert!(changed);

    let batch = tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
        update: tl_enums::Update::MessageWidgetData(tl_types::UpdateMessageWidgetData {
            peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 880_001 }),
            msg_id: 42,
            public_data: Some(r#"{"score":99}"#.to_owned()),
            public_data_hash: Some(888_003),
            chat_data: Some(r#"{"tab":"live"}"#.to_owned()),
            pts: 12,
            pts_count: 1,
        }),
        date: now_unix_i32(),
    });
    assert!(record_widget_updates_from_batch(
        live.runtime().session(),
        &batch
    ));

    let mut hist_msg = rt.block_on(async {
        let s = live.runtime().session().read().await;
        let msg = s
            .message(tg_data::FullMsgId::new(
                DataPeerId(bot_peer.0),
                DataMsgId(42),
            ))
            .expect("fixture widget msg");
        message_to_history_message(&s, msg)
    });
    attach_cached_widget_states(
        live.runtime().session(),
        std::slice::from_mut(&mut hist_msg),
    );
    match hist_msg.media {
        Some(HistoryMedia::Widget(w)) => {
            assert_eq!(w.chat_data.as_deref(), Some(r#"{"tab":"live"}"#));
            assert!(w.public_data.is_some());
        }
        other => panic!("expected HistoryMedia::Widget, got {other:?}"),
    }
}

#[test]
fn convert_tl_message_media_widget_and_data_message() {
    let html = "<title>Countdown</title><p>Launch in 3 minutes</p>";
    let payload = pack_payload_zlib(html.as_bytes());
    let tl_widget = tl_types::MessageMediaWidget {
        is_copy: false,
        can_query: true,
        background_query: true,
        width: 340,
        height: 180,
        payload: payload.clone(),
        public_data: Some(r#"{"t":180}"#.to_owned()),
        public_data_hash: Some(12345),
        chat_data: Some(r#"{"tz":"UTC"}"#.to_owned()),
        widget_id: Some(9001),
    };

    let fields = tl_message_media_widget_to_fields(&tl_widget);
    assert_eq!(fields.anchors.width, 340);
    assert_eq!(fields.anchors.height, 180);
    assert!(fields.flags.can_query && fields.flags.background_query && !fields.flags.is_copy);
    assert_eq!(fields.widget_id, Some(9001));
    assert_eq!(fields.public_data_hash, Some(12345));
    assert_eq!(
        fields.state.public_data.as_deref(),
        Some(br#"{"t":180}"#.as_slice())
    );
    assert_eq!(
        fields.state.chat_data.as_deref(),
        Some(br#"{"tz":"UTC"}"#.as_slice())
    );

    let att = tl_message_media_widget_to_attachment(&tl_widget);
    assert_eq!(att.title, "Countdown");
    assert_eq!(att.html.as_deref(), Some(html));
    assert!(att.preview_text.contains("Launch in 3 minutes"));

    // Verify full `tg_data::Message` -> `HistoryMessage` conversion in `live/convert.rs`.
    let (rt, _rpc, live, bot_peer) = build_live_widget_fixture();
    let hist_msg = rt.block_on(async {
        let s = live.runtime().session().read().await;
        let msg = s
            .message(tg_data::FullMsgId::new(
                DataPeerId(bot_peer.0),
                DataMsgId(42),
            ))
            .expect("fixture widget msg");
        message_to_history_message(&s, msg)
    });
    match hist_msg.media {
        Some(HistoryMedia::Widget(w)) => {
            assert_eq!(w.widget_id, Some(77_001));
            assert_eq!(w.title, "Live Ticker");
            assert!(w.preview_text.contains("Price: 12.50"));
        }
        other => panic!("expected HistoryMedia::Widget, got {other:?}"),
    }

    // Verify LiveWidgetHost state machine initialization and inbox draining.
    let mut host = LiveWidgetHost::new(bot_peer, MsgId(42), html.as_bytes(), true);
    assert_eq!(host.state(), RuntimeState::Idle);
    host.on_resize(240);
    assert_eq!(
        host.state_machine.height_layout().map(|h| h.height),
        Some(240)
    );
    host.inbox
        .lock()
        .expect("lock inbox")
        .push(r#"{"type":"size","height":260}"#.to_owned());
    let drained = host.drain_messages();
    assert_eq!(drained, vec![RendererToHostMsg::Size { height: 260.0 }]);
}

#[test]
fn mock_session_includes_demo_widget_and_handles_queries() {
    let mut session = MockSession::new(true);
    let slice = session.load_slice_around(session_peers::BOT_CATALOG, MsgId::SHOW_AT_END, 50, 50);
    let widget_msg = slice
        .messages
        .iter()
        .find(|m| matches!(m.media, Some(HistoryMedia::Widget(_))))
        .expect("MockSession BOT_CATALOG should include a demo HTML widget message");
    if let Some(HistoryMedia::Widget(ref w)) = widget_msg.media {
        assert!(w.html.is_some());
        assert!(!w.title.is_empty());
    }

    let ans = session
        .widget_query(session_peers::BOT_CATALOG, widget_msg.id, b"refresh", false)
        .expect("MockSession widget_query");
    assert!(ans.is_ok());
    assert!(ans.answer.result.is_some());

    let poll = session
        .widget_poll_public_data(session_peers::BOT_CATALOG, widget_msg.id, 0)
        .expect("MockSession widget_poll_public_data");
    assert!(matches!(poll, PublicDataPollResult::Updated { .. }));
}
