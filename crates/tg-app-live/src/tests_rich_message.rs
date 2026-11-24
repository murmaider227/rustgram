//! Live Layer-233 rich message RPC tests (`messages.sendMessage`, `messages.editMessage`,
//! `messages.getRichMessage`) using an in-process `FakeRpcClient`.
//!
//! Zero real network traffic, zero real accounts or credentials.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_api::{DcId, InvocationError, RpcClient};
use tg_iv::editor::EditorState;
use tg_iv::rich_page::{
    InMemoryMediaResolver, RichBlock, RichBlockKind, RichMessageLimits, RichPage,
    SerializeRichMessageMode, SerializeRichMessageStatus,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use crate::rich_message::fetch_rich_message_live;
use tg_app_core::history::backend::HistoryBackend;
use tg_app_core::stand_in::types::{MsgId, PeerId};

#[derive(Clone)]
struct CapturedRequest {
    ctor: u32,
    bytes: Vec<u8>,
}

type QueuedReply = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct RichFakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    captured: Arc<Mutex<Vec<CapturedRequest>>>,
    responses: Arc<Mutex<VecDeque<QueuedReply>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl RichFakeRpcClient {
    fn new(dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(32);
        Self {
            main_dc: Arc::new(Mutex::new(dc)),
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

    fn captured_requests(&self) -> Vec<CapturedRequest> {
        self.captured.lock().expect("lock captured").clone()
    }
}

impl RpcClient for RichFakeRpcClient {
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
            .push(CapturedRequest { ctor, bytes });

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

fn synthetic_user(id: i64, first_name: &str, username: &str, is_self: bool) -> tl_types::User {
    tl_types::User {
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
        access_hash: Some(id * 1000 + 3),
        first_name: Some(first_name.to_owned()),
        last_name: Some("Tester".to_owned()),
        username: Some(username.to_owned()),
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

fn synthetic_message(
    id: i32,
    peer_uid: i64,
    from_uid: i64,
    out: bool,
    date: i32,
    text: &str,
    rich_message: Option<tl_enums::RichMessage>,
) -> tl_enums::Message {
    tl_enums::Message::Message(tl_types::Message {
        out,
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
        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
            user_id: from_uid,
        })),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: peer_uid }),
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        guestchat_via_from: None,
        reply_to: None,
        date,
        message: text.to_owned(),
        media: None,
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
        suggested_post: None,
        schedule_repeat_period: None,
        summary_from_language: None,
        rich_message,
    })
}

fn make_live_rich_harness() -> (
    i64,
    i64,
    PeerId,
    RichFakeRpcClient,
    Arc<SessionRuntime<crate::ErasedRpcClient>>,
    LiveSession,
) {
    let self_uid = 610_001i64;
    let peer_uid = 610_002i64;
    let self_user = synthetic_user(self_uid, "Morgan", "morgan_self", true);
    let peer_user = synthetic_user(peer_uid, "Jordan", "jordan_peer", false);
    let top_msg = synthetic_message(10, peer_uid, peer_uid, false, 1_750_000_000, "Hi", None);
    let dialog = tl_enums::Dialog::Dialog(tl_types::Dialog {
        pinned: false,
        unread_mark: false,
        view_forum_as_messages: false,
        peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: peer_uid }),
        top_message: 10,
        read_inbox_max_id: 10,
        read_outbox_max_id: 10,
        unread_count: 0,
        unread_mentions_count: 0,
        unread_reactions_count: 0,
        unread_poll_votes_count: 0,
        notify_settings: tl_enums::PeerNotifySettings::PeerNotifySettings(
            tl_types::PeerNotifySettings {
                show_previews: Some(true),
                silent: Some(false),
                mute_until: Some(0),
                ios_sound: None,
                android_sound: None,
                other_sound: None,
                stories_muted: None,
                stories_hide_sender: None,
                stories_ios_sound: None,
                stories_android_sound: None,
                stories_other_sound: None,
            },
        ),
        pts: None,
        draft: None,
        folder_id: None,
        ttl_period: None,
    });

    let fake = RichFakeRpcClient::new(2);
    fake.enqueue_ok(
        tl_fns::users::GetUsers::CONSTRUCTOR_ID,
        &vec![tl_enums::User::User(self_user)],
    );
    fake.enqueue_ok(
        tl_fns::updates::GetState::CONSTRUCTOR_ID,
        &tl_enums::updates::State::State(tl_types::updates::State {
            pts: 50,
            qts: 0,
            date: 1_750_000_000,
            seq: 1,
            unread_count: 0,
        }),
    );
    fake.enqueue_ok(
        tl_fns::updates::GetDifference::CONSTRUCTOR_ID,
        &tl_enums::updates::Difference::Empty(tl_types::updates::DifferenceEmpty {
            date: 1_750_000_001,
            seq: 1,
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
            dialogs: vec![dialog],
            messages: vec![top_msg],
            chats: Vec::new(),
            users: vec![tl_enums::User::User(peer_user)],
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetPinnedDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
            dialogs: Vec::new(),
            messages: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
            state: tl_enums::updates::State::State(tl_types::updates::State {
                pts: 50,
                qts: 0,
                date: 1_750_000_000,
                seq: 1,
                unread_count: 0,
            }),
        }),
    );

    let runtime = Arc::new(
        SessionRuntime::start(
            tg_data::UserId::from_tl(self_uid),
            crate::ErasedRpcClient::new(fake.clone()),
            SessionRuntimeConfig::default(),
        )
        .expect("start rich test runtime"),
    );
    runtime
        .tokio_handle()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(15), runtime.wait_ready()).await
        })
        .expect("wait_ready timeout")
        .expect("startup ready");
    let live = LiveSession::new(Arc::clone(&runtime), 1);
    (
        self_uid,
        peer_uid,
        PeerId::from_user(peer_uid as u64),
        fake,
        runtime,
        live,
    )
}

#[test]
fn live_send_and_edit_rich_message_request_shape_with_fake_rpc() {
    let (self_uid, peer_uid, peer_id, fake, _rt, mut live) = make_live_rich_harness();

    // 1. Author a rich document via EditorState and serialize with FinalSubmit + InMemoryMediaResolver.
    let page = RichPage {
        blocks: vec![
            RichBlock::heading(1, "Experiment Summary"),
            RichBlock::paragraph("Measured convergence rate:"),
            RichBlock::math(r"\sum_{k=1}^{n} \frac{1}{k^2}"),
        ],
        ..RichPage::default()
    };
    let editor = EditorState::from_rich_page(page.clone(), RichMessageLimits::default());
    let resolver = InMemoryMediaResolver::default();
    let ser = editor.to_input_rich_message(SerializeRichMessageMode::FinalSubmit, &resolver);
    assert_eq!(ser.status, SerializeRichMessageStatus::Success);
    let input = ser.value.expect("serialized InputRichMessage");
    assert_eq!(input.blocks.len(), 3);

    // Enqueue server Updates echoing back the sent rich message (id = 77).
    let echoed_rm = tl_enums::RichMessage::RichMessage(tl_types::RichMessage {
        rtl: false,
        part: false,
        blocks: input.blocks.clone(),
        photos: Vec::new(),
        documents: Vec::new(),
    });
    let server_msg = synthetic_message(
        77,
        peer_uid,
        self_uid,
        true,
        1_750_000_120,
        "",
        Some(echoed_rm),
    );
    fake.enqueue_ok(
        tl_fns::messages::SendMessage::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                message: server_msg,
                pts: 51,
                pts_count: 1,
            })],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_750_000_120,
            seq: 2,
        }),
    );

    let sent = live
        .send_rich_message(peer_id, input, page, Some(MsgId(10)), true)
        .expect("send_rich_message succeeds");
    assert_eq!(sent.id, MsgId(77));
    assert!(sent.extra.rich_message.is_some());

    // Confirm `load_slice_around` re-attaches the cached rich payload onto history messages.
    let slice = live.load_slice_around(peer_id, MsgId(77), 5, 5);
    assert!(
        slice
            .messages
            .iter()
            .any(|m| m.id == MsgId(77) && m.extra.rich_message.is_some()),
        "load_slice_around must attach cached rich_message payload"
    );

    // Inspect the exact wire request captured by FakeRpcClient!
    let send_req_bytes = fake
        .captured_requests()
        .into_iter()
        .find(|r| r.ctor == tl_fns::messages::SendMessage::CONSTRUCTOR_ID)
        .expect("messages.sendMessage must be invoked")
        .bytes;
    let decoded_send =
        tl_fns::messages::SendMessage::from_bytes(&send_req_bytes).expect("decode SendMessage");
    assert!(decoded_send.silent);
    assert!(decoded_send.clear_draft);
    assert_eq!(decoded_send.message, "");
    assert!(decoded_send.reply_to.is_some());
    let Some(tl_enums::InputRichMessage::InputRichMessage(sent_irm)) = decoded_send.rich_message
    else {
        panic!("expected InputRichMessage::InputRichMessage on messages.sendMessage");
    };
    assert_eq!(sent_irm.blocks.len(), 3);
    assert!(matches!(
        sent_irm.blocks[0],
        tl_enums::PageBlock::Heading1(_)
    ));
    assert!(matches!(
        sent_irm.blocks[1],
        tl_enums::PageBlock::Paragraph(_)
    ));
    assert!(matches!(sent_irm.blocks[2], tl_enums::PageBlock::Math(_)));

    // 2. Edit the rich message via `edit_rich_message` and verify `messages.editMessage` wire shape.
    let edited_page = RichPage {
        blocks: vec![
            RichBlock::heading(2, "Revised Summary"),
            RichBlock::math(r"\int_0^1 x^2\,dx = \frac{1}{3}"),
        ],
        ..RichPage::default()
    };
    let ed_state = EditorState::from_rich_page(edited_page.clone(), RichMessageLimits::default());
    let edit_input = ed_state
        .to_input_rich_message(SerializeRichMessageMode::FinalSubmit, &resolver)
        .value
        .expect("edit InputRichMessage");

    let edited_tl_rm = tl_enums::RichMessage::RichMessage(tl_types::RichMessage {
        rtl: false,
        part: false,
        blocks: edit_input.blocks.clone(),
        photos: Vec::new(),
        documents: Vec::new(),
    });
    let edited_server_msg = synthetic_message(
        77,
        peer_uid,
        self_uid,
        true,
        1_750_000_120,
        "",
        Some(edited_tl_rm),
    );
    fake.enqueue_ok(
        tl_fns::messages::EditMessage::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::EditMessage(tl_types::UpdateEditMessage {
                message: edited_server_msg,
                pts: 52,
                pts_count: 1,
            })],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_750_000_130,
            seq: 3,
        }),
    );

    let edited = live
        .edit_rich_message(peer_id, MsgId(77), edit_input, edited_page)
        .expect("edit_rich_message succeeds");
    assert!(edited.edited);
    assert_eq!(
        edited
            .extra
            .rich_message
            .as_ref()
            .expect("edited rich payload")
            .page
            .blocks
            .len(),
        2
    );

    let edit_req_bytes = fake
        .captured_requests()
        .into_iter()
        .find(|r| r.ctor == tl_fns::messages::EditMessage::CONSTRUCTOR_ID)
        .expect("messages.editMessage must be invoked")
        .bytes;
    let decoded_edit =
        tl_fns::messages::EditMessage::from_bytes(&edit_req_bytes).expect("decode EditMessage");
    assert_eq!(decoded_edit.id, 77);
    assert!(decoded_edit.message.is_none());
    let Some(tl_enums::InputRichMessage::InputRichMessage(edit_irm)) = decoded_edit.rich_message
    else {
        panic!("expected InputRichMessage on messages.editMessage");
    };
    assert_eq!(edit_irm.blocks.len(), 2);
    assert!(matches!(
        edit_irm.blocks[0],
        tl_enums::PageBlock::Heading2(_)
    ));
    assert!(matches!(edit_irm.blocks[1], tl_enums::PageBlock::Math(_)));

    // 3. Verify `messages.getRichMessage` fetches and decodes via `rich_page_from_tl_rich_message`.
    let fetched_rm = tl_enums::RichMessage::RichMessage(tl_types::RichMessage {
        rtl: false,
        part: false,
        blocks: vec![tl_enums::PageBlock::Math(tl_types::PageBlockMath {
            source: r"a^2 + b^2 = c^2".to_owned(),
        })],
        photos: Vec::new(),
        documents: Vec::new(),
    });
    fake.enqueue_ok(
        tl_fns::messages::GetRichMessage::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![synthetic_message(
                88,
                peer_uid,
                peer_uid,
                false,
                1_750_000_200,
                "",
                Some(fetched_rm),
            )],
            chats: Vec::new(),
            users: Vec::new(),
            topics: Vec::new(),
        }),
    );

    let fetched = fetch_rich_message_live(&live, peer_id, MsgId(88))
        .expect("getRichMessage succeeds")
        .expect("rich payload decoded");
    assert_eq!(fetched.page.blocks.len(), 1);
    assert_eq!(fetched.page.blocks[0].kind, RichBlockKind::Math);
    assert_eq!(fetched.page.blocks[0].formula, r"a^2 + b^2 = c^2");
}
