//! Unit and RPC integration tests for WP-15 Layer-233 history extras (`live/layer233_extras.rs`
//! and `live/convert.rs`) using an in-process `FakeRpcClient`.
//!
//! Covers:
//! 1. Checklists / to-do lists (`messages.sendMedia(inputMediaTodo)`, `messages.editMessage`,
//!    `messages.toggleTodoCompleted` + 3s batching, `messages.appendTodoList`, and
//!    `MessageMedia::ToDo` / `TodoCompletions` / `TodoAppendTasks` conversions).
//! 2. Sponsored messages (`messages.getSponsoredMessages`, `viewSponsoredMessage` 5-min throttle,
//!    `clickSponsoredMessage`, `reportSponsoredMessage` multi-step & `PREMIUM_ACCOUNT_REQUIRED`,
//!    `account.toggleSponsoredMessages`, and Premium "hide ads" suppression).
//! 3. Fact checks (`messages.getFactCheck` batched, `messages.editFactCheck`,
//!    `messages.deleteFactCheck`).
//! 4. Suggested posts & monoforum channel direct messages (`messages.getSavedDialogs`,
//!    `messages.getSavedHistory`, `messages.readSavedHistory`, `messages.sendMessage` with
//!    `inputReplyToMonoForum` + `suggestedPost`, `messages.toggleSuggestedPostApproval`, and
//!    suggested-post service action conversions).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tg_api::{DcId, InvocationError, RpcClient, convert::apply_tl_users_and_chats};
use tg_data::{
    ChannelId, Message, MsgId as DataMsgId, PeerId as DataPeerId,
    ServiceAction as DataServiceAction, StarsAmount, TextWithEntities as DataText, TodoItemData,
    UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_text::TextWithEntities;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use crate::convert::message_to_history_message;
use tg_app_core::core_types::dto::monoforum::{
    SuggestApprovalDecision, SuggestOptions, SuggestedCurrency, SuggestedPrice,
};
use tg_app_core::core_types::dto::sponsored::SponsoredReportOutcome;
use tg_app_core::core_types::dto::todo::{CreateTodoListRequest, TodoItemInput};
use tg_app_core::history::backend::{
    HistoryBackend, HistoryMedia, Layer233ExtrasBackend, ServiceAction as UiServiceAction,
};
use tg_app_core::stand_in::types::{MsgId, PeerId};

// =============================================================================
// Synthetic FakeRpcClient & fixtures
// =============================================================================

type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);
type RecordedCalls = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: RecordedCalls,
    responses: Arc<Mutex<VecDeque<QueuedResponse>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(64);
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

    fn recorded_ctors(&self) -> Vec<u32> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .map(|(c, _)| *c)
            .collect()
    }

    fn recorded_payloads(&self, ctor: u32) -> Vec<Vec<u8>> {
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

fn synthetic_user(id: i64, first_name: &str, is_self: bool, premium: bool) -> tl_enums::User {
    tl_enums::User::User(tl_types::User {
        self_: is_self,
        contact: !is_self,
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
        premium,
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
        access_hash: Some(id * 101 + 3),
        first_name: Some(first_name.to_owned()),
        last_name: Some("Tester".to_owned()),
        username: Some(first_name.to_lowercase()),
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

fn synthetic_channel(id: i64, title: &str, monoforum: bool) -> tl_enums::Chat {
    tl_enums::Chat::Channel(tl_types::Channel {
        creator: true,
        left: false,
        broadcast: !monoforum,
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
        monoforum,
        forum_tabs: false,
        in_news: false,
        forum_sidebar: false,
        id,
        access_hash: Some(id * 1009 + 17),
        title: title.to_owned(),
        username: Some(format!("chan_{id}")),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_795_200_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(120),
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

fn synthetic_tl_message(
    id: i32,
    peer: tl_enums::Peer,
    from: Option<tl_enums::Peer>,
    text: &str,
    media: Option<tl_enums::MessageMedia>,
    suggested_post: Option<tl_enums::SuggestedPost>,
) -> tl_enums::Message {
    tl_enums::Message::Message(tl_types::Message {
        out: true,
        mentioned: false,
        media_unread: false,
        silent: false,
        post: false,
        from_scheduled: false,
        legacy: false,
        edit_hide: false,
        pinned: false,
        noforwards: false,
        invert_media: false,
        offline: false,
        video_processing_pending: false,
        paid_suggested_post_stars: false,
        paid_suggested_post_ton: false,
        id,
        from_id: from,
        from_boosts_applied: None,
        from_rank: None,
        peer_id: peer,
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        guestchat_via_from: None,
        reply_to: None,
        date: 1_795_341_600,
        message: text.to_owned(),
        media,
        reply_markup: None,
        entities: None,
        views: None,
        forwards: None,
        replies: None,
        edit_date: None,
        post_author: None,
        grouped_id: None,
        reactions: None,
        restriction_reason: None,
        ttl_period: None,
        quick_reply_shortcut_id: None,
        effect: None,
        factcheck: None,
        report_delivery_until_date: None,
        paid_message_stars: None,
        suggested_post,
        schedule_repeat_period: None,
        summary_from_language: None,
        rich_message: None,
    })
}

fn make_live_session(self_premium: bool) -> (LiveSession, FakeRpcClient) {
    let rpc = FakeRpcClient::new();
    let rt = Arc::new(
        SessionRuntime::start(
            DataUserId(1000),
            crate::ErasedRpcClient::new(rpc.clone()),
            SessionRuntimeConfig {
                init_updates_state: false,
                run_get_difference_on_start: false,
                load_initial_dialogs: false,
                fetch_self_user_on_start: false,
                ..SessionRuntimeConfig::default()
            },
        )
        .expect("start SessionRuntime"),
    );
    let users = vec![
        synthetic_user(1000, "Taylor", true, self_premium),
        synthetic_user(1001, "Mira", false, false),
    ];
    let chats = vec![
        synthetic_channel(3001, "Dev Log", false),
        synthetic_channel(3099, "Dev Log Direct", true),
    ];
    let session_arc = Arc::clone(rt.session());
    crate::media::block_on_runtime(rt.tokio_handle(), async move {
        let mut s = session_arc.write().await;
        apply_tl_users_and_chats(&mut s, &users, &chats);
    });
    let live = LiveSession::new(rt, 1);
    (live, rpc)
}

// =============================================================================
// 1. Checklists / To-Do Lists
// =============================================================================

#[test]
fn live_todo_create_edit_toggle_batch_and_append() {
    let (mut live, rpc) = make_live_session(true);
    let peer = PeerId::from_user(1001);
    let tl_peer = tl_enums::Peer::User(tl_types::PeerUser { user_id: 1001 });
    let self_tl_peer = tl_enums::Peer::User(tl_types::PeerUser { user_id: 1000 });

    let todo_media = tl_enums::MessageMedia::ToDo(tl_types::MessageMediaToDo {
        todo: tl_enums::TodoList::TodoList(tl_types::TodoList {
            others_can_append: true,
            others_can_complete: true,
            title: tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                text: "Ship Layer 233".to_owned(),
                entities: Vec::new(),
            }),
            list: vec![
                tl_enums::TodoItem::TodoItem(tl_types::TodoItem {
                    id: 1,
                    title: tl_enums::TextWithEntities::TextWithEntities(
                        tl_types::TextWithEntities {
                            text: "Port checklists".to_owned(),
                            entities: Vec::new(),
                        },
                    ),
                }),
                tl_enums::TodoItem::TodoItem(tl_types::TodoItem {
                    id: 2,
                    title: tl_enums::TextWithEntities::TextWithEntities(
                        tl_types::TextWithEntities {
                            text: "Port fact checks".to_owned(),
                            entities: Vec::new(),
                        },
                    ),
                }),
            ],
        }),
        completions: None,
    });

    // 1. Create checklist -> messages.sendMedia(inputMediaTodo)
    let created_updates = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
            message: synthetic_tl_message(
                77,
                tl_peer.clone(),
                Some(self_tl_peer.clone()),
                "",
                Some(todo_media),
                None,
            ),
            pts: 10,
            pts_count: 1,
        })],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_341_600,
        seq: 0,
    });
    rpc.enqueue_ok(
        tl_fns::messages::SendMedia::CONSTRUCTOR_ID,
        &created_updates,
    );

    let created = live
        .create_todo_list(
            peer,
            CreateTodoListRequest {
                title: TextWithEntities::simple("Ship Layer 233"),
                items: vec![
                    TodoItemInput {
                        id: 1,
                        text: TextWithEntities::simple("Port checklists"),
                    },
                    TodoItemInput {
                        id: 2,
                        text: TextWithEntities::simple("Port fact checks"),
                    },
                ],
                others_can_append: true,
                others_can_complete: true,
            },
        )
        .expect("create_todo_list");
    assert_eq!(created.id, MsgId(77));
    match &created.media {
        Some(HistoryMedia::TodoList(todo)) => {
            assert_eq!(todo.title.text, "Ship Layer 233");
            assert_eq!(todo.items.len(), 2);
            assert!(todo.others_can_append && todo.others_can_complete);
        }
        other => panic!("expected TodoList media, got {other:?}"),
    }

    // 2. Toggle completion via 3s debounced batcher -> messages.toggleTodoCompleted
    let empty_updates = tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_341_605,
        seq: 2,
    });
    rpc.enqueue_ok(
        tl_fns::messages::ToggleTodoCompleted::CONSTRUCTOR_ID,
        &empty_updates,
    );

    live.queue_todo_toggle(peer, MsgId(77), 1, true, 1_000);
    live.queue_todo_toggle(peer, MsgId(77), 2, true, 1_200);
    live.queue_todo_toggle(peer, MsgId(77), 2, false, 1_500);
    assert_eq!(live.flush_todo_toggles(4_000, false).unwrap(), 0);
    assert_eq!(live.flush_todo_toggles(4_500, false).unwrap(), 1);

    let toggle_bytes = rpc
        .recorded_payloads(tl_fns::messages::ToggleTodoCompleted::CONSTRUCTOR_ID)
        .pop()
        .expect("toggleTodoCompleted payload");
    let decoded_toggle =
        tl_fns::messages::ToggleTodoCompleted::from_bytes(&toggle_bytes).expect("decode toggle");
    assert_eq!(decoded_toggle.msg_id, 77);
    assert_eq!(decoded_toggle.completed, vec![1]);
    assert_eq!(decoded_toggle.incompleted, vec![2]);

    // 3. Append tasks -> messages.appendTodoList
    rpc.enqueue_ok(
        tl_fns::messages::AppendTodoList::CONSTRUCTOR_ID,
        &empty_updates,
    );
    let appended = live
        .append_todo_list(
            peer,
            MsgId(77),
            vec![TodoItemInput {
                id: 3,
                text: TextWithEntities::simple("Port monoforum"),
            }],
        )
        .expect("append_todo_list")
        .expect("updated checklist message");
    match &appended.media {
        Some(HistoryMedia::TodoList(todo)) => {
            assert_eq!(todo.items.len(), 3);
            assert_eq!(todo.completed_count(), 1);
            assert_eq!(todo.items[2].text.text, "Port monoforum");
        }
        other => panic!("expected TodoList after append, got {other:?}"),
    }

    // 4. Verify live/convert.rs service message conversions for todo & suggested posts
    let session_arc = Arc::clone(live.runtime().session());
    crate::media::block_on_runtime(live.runtime().tokio_handle(), async move {
        let s = session_arc.read().await;
        let dp = DataPeerId::from_user(DataUserId(1001));
        let svc_comp = Message::new_service(
            dp,
            DataMsgId(90),
            Some(DataPeerId::from_user(DataUserId(1000))),
            1_795_341_600,
            DataServiceAction::TodoCompletions {
                completed: vec![1, 2],
                incompleted: vec![],
            },
        );
        let ui_comp = message_to_history_message(&s, &svc_comp);
        assert!(matches!(
            ui_comp.service,
            Some(UiServiceAction::TodoCompletions { .. })
        ));
        assert!(
            ui_comp
                .service
                .as_ref()
                .unwrap()
                .display_text("Taylor")
                .contains("completed 2 tasks")
        );

        let svc_app = Message::new_service(
            dp,
            DataMsgId(91),
            Some(DataPeerId::from_user(DataUserId(1000))),
            1_795_341_601,
            DataServiceAction::TodoAppendTasks {
                list: vec![TodoItemData {
                    id: 4,
                    title: DataText::plain("Write tests"),
                }],
            },
        );
        let ui_app = message_to_history_message(&s, &svc_app);
        assert!(
            ui_app
                .service
                .as_ref()
                .unwrap()
                .display_text("Taylor")
                .contains("Write tests")
        );

        let svc_sp = Message::new_service(
            DataPeerId::from_channel(ChannelId(3099)),
            DataMsgId(92),
            Some(DataPeerId::from_user(DataUserId(1000))),
            1_795_341_602,
            DataServiceAction::SuggestedPostApproval {
                rejected: false,
                balance_too_low: false,
                reject_comment: None,
                schedule_date: Some(1_795_400_000),
                price: Some(StarsAmount {
                    amount: 250,
                    nanos: 0,
                    is_ton: false,
                }),
            },
        );
        let ui_sp = message_to_history_message(&s, &svc_sp);
        assert!(
            ui_sp
                .service
                .as_ref()
                .unwrap()
                .display_text("Taylor")
                .contains("250 Stars")
        );
    });
}

// =============================================================================
// 2. Sponsored Messages + Premium "Hide Ads"
// =============================================================================

#[test]
fn live_sponsored_messages_view_click_report_and_premium_hide_ads() {
    let (mut live, rpc) = make_live_session(true);
    let channel = PeerId::from_channel(3001);
    assert!(live.as_layer233_extras().is_some());

    let ad_id = b"rnd_ad_233".to_vec();
    let tl_ads = tl_enums::messages::SponsoredMessages::SponsoredMessages(
        tl_types::messages::SponsoredMessages {
            posts_between: Some(5),
            start_delay: None,
            between_delay: None,
            messages: vec![tl_enums::SponsoredMessage::SponsoredMessage(
                tl_types::SponsoredMessage {
                    recommended: false,
                    can_report: true,
                    random_id: ad_id.clone(),
                    url: "https://t.me/rust_demo_ad".to_owned(),
                    title: "Rust Demo Ad".to_owned(),
                    message: "Build native desktop apps in Rust.".to_owned(),
                    entities: None,
                    photo: None,
                    media: None,
                    color: None,
                    button_text: "Open Channel".to_owned(),
                    sponsor_info: Some("Demo Labs".to_owned()),
                    additional_info: None,
                    min_display_duration: None,
                    max_display_duration: None,
                },
            )],
            chats: Vec::new(),
            users: Vec::new(),
        },
    );
    rpc.enqueue_ok(
        tl_fns::messages::GetSponsoredMessages::CONSTRUCTOR_ID,
        &tl_ads,
    );

    let ads = live
        .get_sponsored_messages(channel, None)
        .expect("get_sponsored_messages");
    assert_eq!(ads.len(), 1);
    assert_eq!(ads[0].title, "Rust Demo Ad");
    assert_eq!(ads[0].button_text, "Open Channel");
    assert!(ads[0].is_link_internal);

    // View sponsored message (throttled on duplicate)
    rpc.enqueue_ok(
        tl_fns::messages::ViewSponsoredMessage::CONSTRUCTOR_ID,
        &true,
    );
    live.view_sponsored_message(&ad_id)
        .expect("first view_sponsored_message");
    live.view_sponsored_message(&ad_id)
        .expect("second throttled view");
    assert_eq!(
        rpc.recorded_payloads(tl_fns::messages::ViewSponsoredMessage::CONSTRUCTOR_ID)
            .len(),
        1
    );

    // Click sponsored message
    rpc.enqueue_ok(
        tl_fns::messages::ClickSponsoredMessage::CONSTRUCTOR_ID,
        &true,
    );
    live.click_sponsored_message(&ad_id, true, false)
        .expect("click_sponsored_message");

    // Multi-step report: ChooseOption -> Reported
    let choose = tl_enums::channels::SponsoredMessageReportResult::ChooseOption(
        tl_types::channels::SponsoredMessageReportResultChooseOption {
            title: "Why are you reporting this ad?".to_owned(),
            options: vec![
                tl_enums::SponsoredMessageReportOption::SponsoredMessageReportOption(
                    tl_types::SponsoredMessageReportOption {
                        text: "Spam".to_owned(),
                        option: b"opt_spam".to_vec(),
                    },
                ),
            ],
        },
    );
    rpc.enqueue_ok(
        tl_fns::messages::ReportSponsoredMessage::CONSTRUCTOR_ID,
        &choose,
    );
    let step1 = live
        .report_sponsored_message(&ad_id, &[])
        .expect("report step 1");
    assert!(matches!(step1, SponsoredReportOutcome::ChooseOption { .. }));

    rpc.enqueue_ok(
        tl_fns::messages::ReportSponsoredMessage::CONSTRUCTOR_ID,
        &tl_enums::channels::SponsoredMessageReportResult::Reported,
    );
    let step2 = live
        .report_sponsored_message(&ad_id, b"opt_spam")
        .expect("report step 2");
    assert_eq!(step2, SponsoredReportOutcome::Reported);

    // Premium "hide ads" setting (`account.toggleSponsoredMessages(enabled: false)`) suppresses ads
    rpc.enqueue_ok(
        tl_fns::account::ToggleSponsoredMessages::CONSTRUCTOR_ID,
        &true,
    );
    live.set_sponsored_hidden(true)
        .expect("set_sponsored_hidden");
    assert!(live.is_sponsored_hidden());

    let before_count = rpc
        .recorded_payloads(tl_fns::messages::GetSponsoredMessages::CONSTRUCTOR_ID)
        .len();
    let suppressed = live
        .get_sponsored_messages(channel, None)
        .expect("suppressed get_sponsored_messages");
    assert!(suppressed.is_empty());
    assert_eq!(
        rpc.recorded_payloads(tl_fns::messages::GetSponsoredMessages::CONSTRUCTOR_ID)
            .len(),
        before_count,
        "GetSponsoredMessages RPC must not fire when Premium hide-ads is active"
    );
}

#[test]
fn live_sponsored_report_maps_premium_required_error() {
    let (mut live, rpc) = make_live_session(false);
    rpc.enqueue_err(
        tl_fns::messages::ReportSponsoredMessage::CONSTRUCTOR_ID,
        InvocationError::Rpc(tg_api::RpcError {
            code: 400,
            name: "PREMIUM_ACCOUNT_REQUIRED".to_owned(),
            value: None,
        }),
    );
    let outcome = live
        .report_sponsored_message(b"ad_id", b"hide_all")
        .expect("report outcome");
    assert_eq!(outcome, SponsoredReportOutcome::PremiumRequired);
}

// =============================================================================
// 3. Fact Checks (Batched Get + Edit + Delete)
// =============================================================================

#[test]
fn live_factcheck_batched_get_edit_and_delete() {
    let (mut live, rpc) = make_live_session(true);
    let channel = PeerId::from_channel(3001);

    // Enqueue 2 messages and flush in a single batched `messages.getFactCheck` call
    live.enqueue_factcheck(channel, MsgId(101), 500);
    live.enqueue_factcheck(channel, MsgId(102), 600);

    let tl_fc_resp = vec![
        tl_enums::FactCheck::FactCheck(tl_types::FactCheck {
            need_check: false,
            country: Some("DE".to_owned()),
            text: Some(tl_enums::TextWithEntities::TextWithEntities(
                tl_types::TextWithEntities {
                    text: "Verified by independent reviewers.".to_owned(),
                    entities: Vec::new(),
                },
            )),
            hash: 9911,
        }),
        tl_enums::FactCheck::FactCheck(tl_types::FactCheck {
            need_check: false,
            country: None,
            text: None,
            hash: 0,
        }),
    ];
    rpc.enqueue_ok(tl_fns::messages::GetFactCheck::CONSTRUCTOR_ID, &tl_fc_resp);

    assert_eq!(live.flush_factcheck_batch(1_000, false).unwrap(), 0);
    assert_eq!(live.flush_factcheck_batch(1_600, false).unwrap(), 2);

    let get_payload = rpc
        .recorded_payloads(tl_fns::messages::GetFactCheck::CONSTRUCTOR_ID)
        .pop()
        .expect("getFactCheck payload");
    let decoded = tl_fns::messages::GetFactCheck::from_bytes(&get_payload).expect("decode");
    assert_eq!(decoded.msg_id, vec![101, 102]);

    let fc1 = live
        .cached_factcheck(channel, MsgId(101))
        .expect("cached factcheck 101");
    assert_eq!(fc1.country.as_deref(), Some("DE"));
    assert_eq!(fc1.body_text(), Some("Verified by independent reviewers."));
    assert!(live.cached_factcheck(channel, MsgId(102)).is_none());

    // Edit fact-check -> messages.editFactCheck
    let empty_updates = tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_341_700,
        seq: 3,
    });
    rpc.enqueue_ok(
        tl_fns::messages::EditFactCheck::CONSTRUCTOR_ID,
        &empty_updates,
    );
    let edited = live
        .edit_factcheck(
            channel,
            MsgId(101),
            TextWithEntities::simple("Updated fact-check note."),
        )
        .expect("edit_factcheck")
        .expect("non-empty factcheck");
    assert_eq!(edited.body_text(), Some("Updated fact-check note."));

    // Clear fact-check -> messages.deleteFactCheck
    rpc.enqueue_ok(
        tl_fns::messages::DeleteFactCheck::CONSTRUCTOR_ID,
        &empty_updates,
    );
    let deleted = live
        .edit_factcheck(channel, MsgId(101), TextWithEntities::simple("   "))
        .expect("delete_factcheck");
    assert!(deleted.is_none());
    assert!(live.cached_factcheck(channel, MsgId(101)).is_none());
    assert!(
        rpc.recorded_ctors()
            .contains(&tl_fns::messages::DeleteFactCheck::CONSTRUCTOR_ID)
    );
}

// =============================================================================
// 4. Suggested Posts & Monoforum (Channel Direct Messages)
// =============================================================================

#[test]
fn live_monoforum_sublists_history_send_and_suggest_approval() {
    let (mut live, rpc) = make_live_session(true);
    let mf_peer = PeerId::from_channel(3099);
    let mira = PeerId::from_user(1001);
    let mf_tl_peer = tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id: 3099 });
    let mira_tl_peer = tl_enums::Peer::User(tl_types::PeerUser { user_id: 1001 });

    // 1. messages.getSavedDialogs with parent_peer -> MonoForumDialog
    let saved_dlgs =
        tl_enums::messages::SavedDialogs::SavedDialogs(tl_types::messages::SavedDialogs {
            dialogs: vec![tl_enums::SavedDialog::MonoForumDialog(
                tl_types::MonoForumDialog {
                    unread_mark: false,
                    nopaid_messages_exception: true,
                    peer: mira_tl_peer.clone(),
                    top_message: 55,
                    read_inbox_max_id: 54,
                    read_outbox_max_id: 54,
                    unread_count: 1,
                    unread_reactions_count: 0,
                    draft: None,
                },
            )],
            messages: vec![synthetic_tl_message(
                55,
                mf_tl_peer.clone(),
                Some(mira_tl_peer.clone()),
                "Proposed sponsored article",
                None,
                None,
            )],
            chats: Vec::new(),
            users: Vec::new(),
        });
    rpc.enqueue_ok(
        tl_fns::messages::GetSavedDialogs::CONSTRUCTOR_ID,
        &saved_dlgs,
    );

    let sublists = live.monoforum_sublists(mf_peer);
    assert_eq!(sublists.len(), 1);
    assert_eq!(sublists[0].sublist_peer_id, mira);
    assert_eq!(sublists[0].unread_count, 1);
    assert!(sublists[0].nopaid_messages_exception);

    // 2. messages.getSavedHistory with suggested_post payload
    let sp_tl = tl_enums::SuggestedPost::SuggestedPost(tl_types::SuggestedPost {
        accepted: false,
        rejected: false,
        price: Some(tl_enums::StarsAmount::StarsAmount(tl_types::StarsAmount {
            amount: 500,
            nanos: 0,
        })),
        schedule_date: Some(1_795_428_000),
    });
    let saved_hist = tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
        messages: vec![synthetic_tl_message(
            55,
            mf_tl_peer.clone(),
            Some(mira_tl_peer.clone()),
            "Proposed sponsored article",
            None,
            Some(sp_tl),
        )],
        topics: Vec::new(),
        chats: Vec::new(),
        users: Vec::new(),
    });
    rpc.enqueue_ok(
        tl_fns::messages::GetSavedHistory::CONSTRUCTOR_ID,
        &saved_hist,
    );

    let msgs = live.monoforum_sublist_messages(mf_peer, mira);
    assert_eq!(msgs.len(), 1);
    let sp = live
        .suggested_post_for(mf_peer, MsgId(55))
        .expect("suggested post 55");
    assert!(sp.is_pending());
    assert_eq!(sp.price, Some(SuggestedPrice::stars(500)));

    // 3. Mark sublist read -> messages.readSavedHistory
    rpc.enqueue_ok(tl_fns::messages::ReadSavedHistory::CONSTRUCTOR_ID, &true);
    live.mark_monoforum_sublist_read(mf_peer, mira, MsgId(55))
        .expect("mark_monoforum_sublist_read");
    assert_eq!(live.monoforum_sublists(mf_peer)[0].unread_count, 0);

    // 4. Approve suggested post -> messages.toggleSuggestedPostApproval
    let empty_updates = tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_341_800,
        seq: 4,
    });
    rpc.enqueue_ok(
        tl_fns::messages::ToggleSuggestedPostApproval::CONSTRUCTOR_ID,
        &empty_updates,
    );
    live.toggle_suggested_post_approval(
        mf_peer,
        MsgId(55),
        SuggestApprovalDecision::Approve {
            schedule_date: Some(1_795_430_000),
        },
    )
    .expect("approve suggested post");
    let approved = live.suggested_post_for(mf_peer, MsgId(55)).unwrap();
    assert!(approved.accepted && !approved.rejected);

    // 5. Send a new TON suggested post in monoforum -> messages.sendMessage(InputReplyToMonoForum + suggestedPost)
    let sent_updates = tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::NewChannelMessage(
            tl_types::UpdateNewChannelMessage {
                message: synthetic_tl_message(
                    56,
                    mf_tl_peer,
                    Some(tl_enums::Peer::User(tl_types::PeerUser { user_id: 1000 })),
                    "Counter-offer post",
                    None,
                    None,
                ),
                pts: 20,
                pts_count: 1,
            },
        )],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_341_900,
        seq: 0,
    });
    rpc.enqueue_ok(tl_fns::messages::SendMessage::CONSTRUCTOR_ID, &sent_updates);
    let sent = live
        .send_monoforum_message(
            mf_peer,
            mira,
            TextWithEntities::simple("Counter-offer post"),
            Some(SuggestOptions {
                price: Some(SuggestedPrice {
                    currency: SuggestedCurrency::Ton,
                    whole: 2,
                    nanos: 500_000_000,
                }),
                schedule_date: None,
            }),
        )
        .expect("send_monoforum_message");
    assert_eq!(sent.id, MsgId(56));

    let send_bytes = rpc
        .recorded_payloads(tl_fns::messages::SendMessage::CONSTRUCTOR_ID)
        .pop()
        .expect("sendMessage payload");
    let decoded_send =
        tl_fns::messages::SendMessage::from_bytes(&send_bytes).expect("decode sendMessage");
    assert!(matches!(
        decoded_send.reply_to,
        Some(tl_enums::InputReplyTo::MonoForum(_))
    ));
    assert!(decoded_send.suggested_post.is_some());
}
