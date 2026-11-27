//! Integration and unit tests for `LiveSession`, `TelegramRangeLoader`,
//! `LiveAppHost`, and `--live` / `RUSTGRAM_LIVE=1` mode selection.
//!
//! All tests run exclusively against synthetic keys/users and in-process fakes
//! (`FakeRpcClient` and `tg_mtproto::FakeTransportFactory`) with bounded timeouts.
//! No real Telegram network connections or real credentials are ever used.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_core::launcher::WorkDirEnv;
use tg_crypto::{AuthKey, Side};
use tg_data::{
    DocumentData, DocumentId, Message, MessageContent, MessageMedia, MsgId as DataMsgId,
    PeerId as DataPeerId, TextWithEntities as DataText, UserId as DataUserId,
};
use tg_mtproto::message::{
    build_encrypted_packet, parse_encrypted_packet, unpack_container, unwrap_gzip_layers,
};
use tg_mtproto::{Client as MtprotoClient, ClientConfig, FakeServerPeer, FakeTransportFactory};
use tg_session::{MtprotoRpc, SessionRuntime, SessionRuntimeConfig};
use tg_tl::mtproto::{
    GzipPacked, MsgContainer, RpcResult, enums as mtp_enums, functions as mtp_fns,
    types as mtp_types,
};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use tg_app::business::backend::BusinessBackend;
use tg_app::dialogs::backend::{DialogListQuery, DialogsBackend};
use tg_app::history::backend::{
    DraftMessage, HistoryBackend, SendMessageRequest, TypingAction as HistoryTypingAction,
};
use tg_app::info::backend::InfoBackend;
use tg_app::live::{LiveAppHost, LiveHostState, LiveSession};
use tg_app::media::backend::{MediaBackend, MediaItemId};
use tg_app::notifications::backend::{IncomingNotifyEvent, NotifyBackend};
use tg_app::payments::backend::{PassportBackend, PaymentsBackend};
use tg_app::settings::backend::{
    NotifyScope, PrivacyKey, PrivacyOption, PrivacyRule, ScopeNotifySettings, SettingsBackend,
};
use tg_app::stand_in::settings::ShellSettings;
use tg_app::stand_in::types::{FilterId, MsgId, PeerId};
use tg_app::startup::{StartupPlan, StartupPrefs, parse_live_mode_env, select_live_mode};
use tg_app::webview::backend::{WebViewBackend, WebViewRequest};
use tg_text::TextWithEntities;

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

// =============================================================================
// Synthetic TL builders & FakeRpcClient
// =============================================================================

fn synthetic_tl_user(id: i64, first_name: &str, username: &str, is_self: bool) -> tl_types::User {
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
        access_hash: Some(id * 1000 + 7),
        first_name: Some(first_name.to_owned()),
        last_name: Some("Tester".to_owned()),
        username: Some(username.to_owned()),
        phone: Some("0000000000".to_owned()),
        photo: None,
        status: Some(tl_enums::UserStatus::Recently(
            tl_types::UserStatusRecently { by_me: false },
        )),
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

fn synthetic_tl_message(
    id: i32,
    peer_uid: i64,
    from_uid: i64,
    out: bool,
    date: i32,
    text: &str,
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
        rich_message: None,
    })
}

fn synthetic_tl_dialog(peer_uid: i64, top_message: i32, unread_count: i32) -> tl_enums::Dialog {
    tl_enums::Dialog::Dialog(tl_types::Dialog {
        pinned: false,
        unread_mark: false,
        view_forum_as_messages: false,
        peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: peer_uid }),
        top_message,
        read_inbox_max_id: (top_message - unread_count).max(0),
        read_outbox_max_id: top_message,
        unread_count,
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
    })
}

type DynamicHandler =
    Arc<dyn Fn(u32, &[u8]) -> Option<Result<Vec<u8>, InvocationError>> + Send + Sync>;

type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<u32>>>,
    responses: Arc<Mutex<VecDeque<QueuedResponse>>>,
    dynamic: Arc<Mutex<Option<DynamicHandler>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(128);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            dynamic: Arc::new(Mutex::new(None)),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
    }

    fn set_dynamic_handler(&self, handler: DynamicHandler) {
        *self.dynamic.lock().expect("lock dynamic") = Some(handler);
    }

    fn push_update(&self, updates: tl_enums::Updates) {
        let _ = self.updates_tx.send(updates);
    }

    fn recorded_ctors(&self) -> Vec<u32> {
        self.recorded.lock().expect("lock recorded").clone()
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
        self.recorded.lock().expect("lock recorded").push(ctor);

        if let Some(handler) = self.dynamic.lock().expect("lock dynamic").clone()
            && let Some(res) = handler(ctor, &bytes)
        {
            let raw = res?;
            return R::Return::from_bytes(&raw)
                .map_err(|e| InvocationError::Deserialize(e.to_string()));
        }

        let next = {
            let mut q = self.responses.lock().expect("lock responses");
            if let Some(pos) = q.iter().position(|(c, _)| *c == ctor) {
                q.remove(pos).map(|(_, r)| r)
            } else if crate::common::is_background_list_request(ctor) {
                // Never the answer a test queued for another request (TGB-2941).
                None
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

struct LiveTestHarness {
    self_uid: i64,
    bob_uid: i64,
    bob_peer: PeerId,
    bob_user: tl_types::User,
    bob_top_msg: tl_enums::Message,
    fake: FakeRpcClient,
    runtime: Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
    live: LiveSession,
}

impl LiveTestHarness {
    fn start() -> Self {
        let self_uid = 500_001i64;
        let bob_uid = 500_002i64;
        let self_user = synthetic_tl_user(self_uid, "Alice", "alice_self", true);
        let bob_user = synthetic_tl_user(bob_uid, "Bob", "bob_test", false);
        let bob_top_msg = synthetic_tl_message(
            10,
            bob_uid,
            bob_uid,
            false,
            1_750_000_000,
            "Hello from Bob!",
        );
        let bob_dialog = synthetic_tl_dialog(bob_uid, 10, 2);

        let fake = FakeRpcClient::new(2);
        fake.enqueue_ok(
            tl_fns::users::GetUsers::CONSTRUCTOR_ID,
            &vec![tl_enums::User::User(self_user)],
        );
        fake.enqueue_ok(
            tl_fns::updates::GetState::CONSTRUCTOR_ID,
            &tl_enums::updates::State::State(tl_types::updates::State {
                pts: 100,
                qts: 0,
                date: 1_750_000_000,
                seq: 1,
                unread_count: 2,
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
                dialogs: vec![bob_dialog],
                messages: vec![bob_top_msg.clone()],
                chats: Vec::new(),
                users: vec![tl_enums::User::User(bob_user.clone())],
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
                    pts: 100,
                    qts: 0,
                    date: 1_750_000_001,
                    seq: 1,
                    unread_count: 2,
                }),
            }),
        );

        let runtime = Arc::new(
            SessionRuntime::start(
                DataUserId::from_tl(self_uid),
                tg_app::live::ErasedRpcClient::new(fake.clone()),
                SessionRuntimeConfig::default(),
            )
            .expect("start SessionRuntime"),
        );
        runtime
            .tokio_handle()
            .block_on(async { tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready()).await })
            .expect("wait_ready timeout")
            .expect("startup ready");

        let live = LiveSession::new(Arc::clone(&runtime), 1);

        Self {
            self_uid,
            bob_uid,
            bob_peer: PeerId::from_user(bob_uid as u64),
            bob_user,
            bob_top_msg,
            fake,
            runtime,
            live,
        }
    }

    fn shutdown(self) {
        drop(self.live);
        if let Ok(rt) = Arc::try_unwrap(self.runtime) {
            rt.shutdown();
        }
    }
}

// =============================================================================
// Test 1: Dialogs page + History slice over tg-session + FakeRpcClient
// =============================================================================

/// TGB-2941: the runtime's background dialog-entry request (`messages.getPeerDialogs`, sent
/// after a forward into Saved Messages) used to take the answer queued for the test's next
/// request, so `delete_messages` got "request dropped" 1 run in 6.
#[test]
fn fake_never_gives_a_background_chat_list_request_another_requests_answer() {
    let fake = FakeRpcClient::new(2);
    fake.enqueue_ok(
        tl_fns::messages::DeleteMessages::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 104,
                pts_count: 1,
            },
        ),
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    let background =
        rt.block_on(fake.invoke(&tl_fns::messages::GetPeerDialogs { peers: Vec::new() }));
    assert!(
        matches!(background, Err(InvocationError::Dropped)),
        "an unqueued background request gets no answer: {background:?}"
    );
    let deleted = rt.block_on(fake.invoke(&tl_fns::messages::DeleteMessages {
        revoke: true,
        id: vec![21],
    }));
    assert!(deleted.is_ok(), "the delete keeps its answer: {deleted:?}");
}

#[test]
fn live_session_dialogs_and_history_slice() {
    let h = LiveTestHarness::start();

    let slice = h
        .live
        .dialog_slice(DialogListQuery::Filter(FilterId::ALL), 0, 20);
    assert_eq!(slice.entries.len(), 1);
    assert_eq!(slice.entries[0].peer_id, h.bob_peer);
    assert_eq!(slice.entries[0].title, "Bob Tester");
    assert_eq!(slice.entries[0].badges.unread_counter, 2);
    assert_eq!(slice.entries[0].last_message, "Hello from Bob!");

    h.fake.enqueue_ok(
        tl_fns::messages::GetHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![h.bob_top_msg.clone()],
            topics: Vec::new(),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(h.bob_user.clone())],
        }),
    );

    let history = h
        .live
        .load_slice_around(h.bob_peer, MsgId::SHOW_AT_END, 20, 20);
    assert_eq!(history.messages.len(), 1);
    assert_eq!(history.messages[0].id, MsgId(10));
    assert_eq!(history.messages[0].text.text, "Hello from Bob!");
    assert!(!history.messages[0].outgoing);

    h.shutdown();
}

// =============================================================================
// Test 2: Send, edit, forward, delete, read, typing, and drafts
// =============================================================================

#[test]
fn live_session_send_edit_delete_forward_read_typing_draft() {
    let mut h = LiveTestHarness::start();
    let self_peer = PeerId::from_user(h.self_uid as u64);

    // Pre-load Bob's history so load_slice_around won't re-fetch GetHistory.
    h.fake.enqueue_ok(
        tl_fns::messages::GetHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![h.bob_top_msg.clone()],
            topics: Vec::new(),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(h.bob_user.clone())],
        }),
    );
    let initial = h
        .live
        .load_slice_around(h.bob_peer, MsgId::SHOW_AT_END, 20, 20);
    assert_eq!(initial.messages.len(), 1);

    // 1. Send message (with reply_to)
    h.fake.enqueue_ok(
        tl_fns::messages::SendMessage::CONSTRUCTOR_ID,
        &tl_enums::Updates::UpdateShortSentMessage(tl_types::UpdateShortSentMessage {
            out: true,
            id: 21,
            pts: 101,
            pts_count: 1,
            date: 1_750_000_010,
            media: None,
            entities: None,
            ttl_period: None,
        }),
    );
    let sent = HistoryBackend::send_message(
        &mut h.live,
        h.bob_peer,
        SendMessageRequest {
            text: TextWithEntities::simple("Reply to Bob"),
            reply_to: Some(MsgId(10)),
            silent: false,
        },
    )
    .expect("send_message");
    // Shown at once as a pending message; the server answer swaps in the server id.
    assert!(sent.id.is_local() && sent.is_sending);
    assert_eq!(sent.text.text, "Reply to Bob");
    assert!(sent.outgoing);
    let sent = crate::send_wait::wait_sent(&mut h.live, h.bob_peer, sent.id);
    assert_eq!(sent.id, MsgId(21));
    assert_eq!(sent.text.text, "Reply to Bob");
    assert!(sent.outgoing);

    // 2. Edit message
    let mut edited_tl = synthetic_tl_message(
        21,
        h.bob_uid,
        h.self_uid,
        true,
        1_750_000_010,
        "Edited reply",
    );
    if let tl_enums::Message::Message(ref mut m) = edited_tl {
        m.edit_date = Some(1_750_000_015);
    }
    h.fake.enqueue_ok(
        tl_fns::messages::EditMessage::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::EditMessage(tl_types::UpdateEditMessage {
                message: edited_tl,
                pts: 102,
                pts_count: 1,
            })],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_750_000_015,
            seq: 2,
        }),
    );
    let edited = h
        .live
        .edit_message(
            h.bob_peer,
            MsgId(21),
            TextWithEntities::simple("Edited reply"),
        )
        .expect("edit_message");
    assert_eq!(edited.text.text, "Edited reply");
    assert!(edited.edited);

    // 3. Forward message to Saved Messages (self_peer)
    let fwd_tl = synthetic_tl_message(
        30,
        h.self_uid,
        h.self_uid,
        true,
        1_750_000_020,
        "Edited reply",
    );
    h.fake.enqueue_ok(
        tl_fns::messages::ForwardMessages::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                message: fwd_tl,
                pts: 103,
                pts_count: 1,
            })],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_750_000_020,
            seq: 3,
        }),
    );
    let forwarded =
        HistoryBackend::forward_messages(&mut h.live, h.bob_peer, &[MsgId(21)], self_peer)
            .expect("forward_messages");
    assert_eq!(forwarded.len(), 1);
    assert_eq!(forwarded[0].id, MsgId(30));

    // 4. Delete message
    h.fake.enqueue_ok(
        tl_fns::messages::DeleteMessages::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 104,
                pts_count: 1,
            },
        ),
    );
    HistoryBackend::delete_messages(&mut h.live, h.bob_peer, &[MsgId(21)], true)
        .expect("delete_messages");
    let after_del = h
        .live
        .load_slice_around(h.bob_peer, MsgId::SHOW_AT_END, 20, 20);
    assert!(
        after_del.messages.iter().all(|m| m.id != MsgId(21)),
        "deleted message #21 must be removed from history"
    );

    // 5. Mark read
    h.fake.enqueue_ok(
        tl_fns::messages::ReadHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 105,
                pts_count: 1,
            },
        ),
    );
    h.live.mark_history_read(h.bob_peer, MsgId(10));
    let bob_row = h.live.dialog_by_peer(h.bob_peer).expect("bob dialog");
    assert_eq!(bob_row.badges.unread_counter, 0);

    // 6. Send typing
    h.fake
        .enqueue_ok(tl_fns::messages::SetTyping::CONSTRUCTOR_ID, &true);
    h.live.send_typing(h.bob_peer, HistoryTypingAction::Typing);

    // 7. Save & load draft, then clear draft
    h.fake
        .enqueue_ok(tl_fns::messages::SaveDraft::CONSTRUCTOR_ID, &true);
    h.live.save_draft(
        h.bob_peer,
        Some(DraftMessage {
            text: "Work in progress".to_owned(),
            reply_to: Some(MsgId(10)),
        }),
    );
    let loaded = h.live.load_draft(h.bob_peer).expect("draft stored");
    assert_eq!(loaded.text, "Work in progress");
    assert_eq!(loaded.reply_to, Some(MsgId(10)));

    h.fake
        .enqueue_ok(tl_fns::messages::SaveDraft::CONSTRUCTOR_ID, &true);
    h.live.save_draft(h.bob_peer, None);
    assert!(h.live.load_draft(h.bob_peer).is_none());

    // Typing, read receipts and drafts go out from the background queue.
    assert!(
        h.live
            .flush_background_rpcs(std::time::Duration::from_secs(10))
    );
    let ctors = h.fake.recorded_ctors();
    assert!(ctors.contains(&tl_fns::messages::SendMessage::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::messages::EditMessage::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::messages::ForwardMessages::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::messages::DeleteMessages::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::messages::ReadHistory::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::messages::SetTyping::CONSTRUCTOR_ID));
    assert!(ctors.contains(&tl_fns::messages::SaveDraft::CONSTRUCTOR_ID));

    h.shutdown();
}

// =============================================================================
// Test 3: Incoming live update produces NotifyBackend notification + bridge bump
// =============================================================================

#[test]
fn live_session_incoming_update_produces_notification() {
    let mut h = LiveTestHarness::start();
    // Drain any initial bridge events from startup.
    let _ = h.live.poll_events();
    let gen_before = h.live.bridge_state().generation();

    h.fake.push_update(tl_enums::Updates::UpdateShortMessage(
        tl_types::UpdateShortMessage {
            out: false,
            mentioned: false,
            media_unread: false,
            silent: false,
            id: 77,
            user_id: h.bob_uid,
            message: "Incoming push notification!".to_owned(),
            pts: 101,
            pts_count: 1,
            date: 1_750_000_050,
            fwd_from: None,
            via_bot_id: None,
            reply_to: None,
            entities: None,
            ttl_period: None,
        },
    ));

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut found = None;
    while Instant::now() < deadline {
        for ev in h.live.poll_events() {
            if let IncomingNotifyEvent::Notification(item) = ev
                && item.msg_id == MsgId(77)
            {
                found = Some(item);
                break;
            }
        }
        if found.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }

    let item = found.expect("expected IncomingNotifyEvent::Notification for msg #77");
    assert_eq!(item.peer_id, h.bob_peer);
    assert_eq!(item.chat_title, "Bob Tester");
    assert_eq!(item.text, "Incoming push notification!");
    assert!(
        h.live.bridge_state().generation() > gen_before,
        "bridge generation should increment on live update"
    );

    h.shutdown();
}

// =============================================================================
// Test 4: Peer info + Settings RPCs (privacy, active sessions, notifications)
// =============================================================================

#[test]
fn live_session_peer_info_and_settings_rpcs() {
    let mut h = LiveTestHarness::start();

    // InfoBackend::peer_profile
    let profile = InfoBackend::peer_profile(&h.live, h.bob_peer).expect("bob profile");
    assert_eq!(profile.title, "Bob Tester");
    assert!(
        profile.usernames.iter().any(|u| u.username == "bob_test"),
        "expected bob_test username, got {:?}",
        profile.usernames
    );

    // Privacy rule read + write
    h.fake.enqueue_ok(
        tl_fns::account::GetPrivacy::CONSTRUCTOR_ID,
        &tl_enums::account::PrivacyRules::PrivacyRules(tl_types::account::PrivacyRules {
            rules: vec![tl_enums::PrivacyRule::PrivacyValueAllowContacts],
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    let rule = h.live.privacy_rule(PrivacyKey::LastSeen);
    assert_eq!(rule.option, PrivacyOption::Contacts);

    h.fake.enqueue_ok(
        tl_fns::account::SetPrivacy::CONSTRUCTOR_ID,
        &tl_enums::account::PrivacyRules::PrivacyRules(tl_types::account::PrivacyRules {
            rules: vec![tl_enums::PrivacyRule::PrivacyValueDisallowAll],
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    h.live.set_privacy_rule(
        PrivacyKey::LastSeen,
        PrivacyRule {
            option: PrivacyOption::Nobody,
            ..PrivacyRule::default()
        },
    );
    assert_eq!(
        h.live.privacy_rule(PrivacyKey::LastSeen).option,
        PrivacyOption::Nobody
    );

    // Active sessions read + terminate
    h.fake.enqueue_ok(
        tl_fns::account::GetAuthorizations::CONSTRUCTOR_ID,
        &tl_enums::account::Authorizations::Authorizations(tl_types::account::Authorizations {
            authorization_ttl_days: 90,
            authorizations: vec![
                tl_enums::Authorization::Authorization(tl_types::Authorization {
                    current: true,
                    official_app: true,
                    password_pending: false,
                    encrypted_requests_disabled: false,
                    call_requests_disabled: false,
                    unconfirmed: false,
                    hash: 0,
                    device_model: "Rustgram Desktop".to_owned(),
                    platform: "Windows".to_owned(),
                    system_version: "11".to_owned(),
                    api_id: 99_001,
                    app_name: "Rustgram".to_owned(),
                    app_version: "0.1".to_owned(),
                    date_created: 1_750_000_000,
                    date_active: 1_750_000_100,
                    ip: "127.0.0.1".to_owned(),
                    country: "Local".to_owned(),
                    region: String::new(),
                }),
                tl_enums::Authorization::Authorization(tl_types::Authorization {
                    current: false,
                    official_app: true,
                    password_pending: false,
                    encrypted_requests_disabled: false,
                    call_requests_disabled: false,
                    unconfirmed: false,
                    hash: 4242,
                    device_model: "Old Laptop".to_owned(),
                    platform: "Linux".to_owned(),
                    system_version: "6.8".to_owned(),
                    api_id: 99_001,
                    app_name: "Rustgram".to_owned(),
                    app_version: "0.1".to_owned(),
                    date_created: 1_740_000_000,
                    date_active: 1_749_000_000,
                    ip: "127.0.0.2".to_owned(),
                    country: "Local".to_owned(),
                    region: String::new(),
                }),
            ],
        }),
    );
    // Settings reads answer at once and load in the background (TGB-3070).
    let _ = h.live.active_sessions();
    assert!(h.live.wait_settings_reads(Duration::from_secs(10)));
    let sessions = h.live.active_sessions();
    assert_eq!(sessions.len(), 2);
    assert_eq!(h.live.session_ttl_days(), 90);

    h.fake
        .enqueue_ok(tl_fns::account::ResetAuthorization::CONSTRUCTOR_ID, &true);
    assert!(h.live.terminate_session(4242));
    assert_eq!(h.live.active_sessions().len(), 1);

    // Scope notifications read + write
    h.fake.enqueue_ok(
        tl_fns::account::GetNotifySettings::CONSTRUCTOR_ID,
        &tl_enums::PeerNotifySettings::PeerNotifySettings(tl_types::PeerNotifySettings {
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
        }),
    );
    let _ = h.live.scope_notifications(NotifyScope::PrivateChats);
    assert!(h.live.wait_settings_reads(Duration::from_secs(10)));
    let scope = h.live.scope_notifications(NotifyScope::PrivateChats);
    assert!(scope.enabled);
    assert!(scope.show_preview);

    h.fake
        .enqueue_ok(tl_fns::account::UpdateNotifySettings::CONSTRUCTOR_ID, &true);
    h.live.set_scope_notifications(
        NotifyScope::PrivateChats,
        ScopeNotifySettings {
            enabled: false,
            show_preview: false,
            sound: "None".to_owned(),
            exceptions: Vec::new(),
        },
    );
    let updated_scope = h.live.scope_notifications(NotifyScope::PrivateChats);
    assert!(!updated_scope.enabled);
    assert!(!updated_scope.show_preview);

    h.shutdown();
}

// =============================================================================
// Test 5: Media download & TelegramRangeLoader over upload.getFile
// =============================================================================

#[test]
fn live_session_media_download_and_range_loader() {
    let h = LiveTestHarness::start();
    let payload: Vec<u8> = (0..(200 * 1024usize))
        .map(|i| ((i * 31 + 7) & 0xFF) as u8)
        .collect();
    let payload_for_rpc = Arc::new(payload.clone());

    // Seed a document message in Bob's history directly in Session.
    let data_peer = DataPeerId(h.bob_peer.0);
    let doc = DocumentData {
        id: DocumentId(888_001),
        access_hash: 999_002,
        file_reference: vec![1, 2, 3, 4],
        dc_id: 2,
        size: payload.len() as i64,
        mime_type: "video/mp4".to_owned(),
        file_name: "clip.mp4".to_owned(),
        ..DocumentData::default()
    };
    let mut media_msg = Message::new_text(
        data_peer,
        DataMsgId(50),
        Some(data_peer),
        1_750_000_030,
        DataText::plain("clip caption"),
    );
    media_msg.content = MessageContent::Regular {
        text: DataText::plain("clip caption"),
        media: MessageMedia::Document(Box::new(doc)),
    };
    h.runtime.tokio_handle().block_on(async {
        let mut s = h.runtime.session().write().await;
        s.insert_message(media_msg);
    });

    // Dynamic upload.getFile handler slicing exact (offset, limit) chunks.
    h.fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
        if ctor != tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            return None;
        }
        let req = tl_fns::upload::GetFile::from_bytes(raw).expect("decode upload.getFile");
        let start = usize::try_from(req.offset).unwrap_or(0);
        let limit = usize::try_from(req.limit).unwrap_or(0);
        let end = start.saturating_add(limit).min(payload_for_rpc.len());
        let bytes = if start < payload_for_rpc.len() {
            payload_for_rpc[start..end].to_vec()
        } else {
            Vec::new()
        };
        let resp = tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FileMp4,
            mtime: 1_750_000_030,
            bytes,
        });
        Some(Ok(resp.to_bytes()))
    }));

    let siblings = h.live.siblings_in_chat(h.bob_peer.0, MediaItemId(0));
    assert_eq!(siblings.len(), 1);
    let item_id = siblings[0].id;
    assert_eq!(siblings[0].file_name, "clip.mp4");

    // Full download across 2 chunks (128 KiB + 72 KiB).
    let downloaded = h.live.load_bytes(item_id).expect("load_bytes");
    assert_eq!(downloaded, payload);

    // RangeLoader spanning the 128 KiB boundary (100_000 .. 140_000).
    let loader = h
        .live
        .open_range_loader(item_id)
        .expect("open_range_loader");
    let slice = loader.load_range(100_000, 40_000).expect("load_range");
    assert_eq!(slice.as_slice(), &payload[100_000..140_000]);

    h.shutdown();
}

// =============================================================================
// Test 7: LiveSession over real tg_mtproto::Client + FakeTransportFactory
// =============================================================================

fn unwrap_tl_call_ctor(raw: &[u8]) -> u32 {
    let mut budget = 1024 * 1024;
    let unpacked = if raw.len() >= 4
        && u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) == GzipPacked::CONSTRUCTOR_ID
    {
        unwrap_gzip_layers(raw, &mut budget, 0).expect("unwrap gzip")
    } else {
        raw.to_vec()
    };
    let mut slice = unpacked.as_slice();
    loop {
        if slice.len() < 4 {
            return 0;
        }
        let ctor = u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]);
        if ctor == tg_tl::functions::InvokeWithLayer::<tg_tl::RawObject>::CONSTRUCTOR_ID {
            let mut cur = tg_tl::Cursor::new(&slice[4..]);
            let _layer = i32::deserialize(&mut cur).expect("invokeWithLayer.layer");
            slice = &slice[slice.len() - cur.remaining()..];
            continue;
        }
        if ctor == tg_tl::functions::InitConnection::<tg_tl::RawObject>::CONSTRUCTOR_ID {
            let mut cur = tg_tl::Cursor::new(&slice[4..]);
            let flags = i32::deserialize(&mut cur).expect("initConnection.flags") as u32;
            let _api_id = i32::deserialize(&mut cur).expect("initConnection.api_id");
            let _device_model = String::deserialize(&mut cur).expect("initConnection.device_model");
            let _system_version =
                String::deserialize(&mut cur).expect("initConnection.system_version");
            let _app_version = String::deserialize(&mut cur).expect("initConnection.app_version");
            let _system_lang_code =
                String::deserialize(&mut cur).expect("initConnection.system_lang_code");
            let _lang_pack = String::deserialize(&mut cur).expect("initConnection.lang_pack");
            let _lang_code = String::deserialize(&mut cur).expect("initConnection.lang_code");
            if (flags & (1 << 0)) != 0 {
                let _proxy = tl_enums::InputClientProxy::deserialize(&mut cur)
                    .expect("initConnection.proxy");
            }
            if (flags & (1 << 1)) != 0 {
                let _params =
                    tl_enums::JSONValue::deserialize(&mut cur).expect("initConnection.params");
            }
            slice = &slice[slice.len() - cur.remaining()..];
            continue;
        }
        return ctor;
    }
}

fn extract_client_messages(dec: &tg_mtproto::message::DecryptedEnvelope) -> Vec<(u64, u32)> {
    if dec.body.len() >= 4
        && u32::from_le_bytes([dec.body[0], dec.body[1], dec.body[2], dec.body[3]])
            == MsgContainer::CONSTRUCTOR_ID
    {
        let items = unpack_container(&dec.body).expect("unpack msg_container");
        items
            .into_iter()
            .map(|m| (m.msg_id as u64, unwrap_tl_call_ctor(&m.body.0)))
            .collect()
    } else {
        vec![(dec.msg_id, unwrap_tl_call_ctor(&dec.body))]
    }
}

async fn drive_mtproto_fake_server(
    mut server: FakeServerPeer,
    key: AuthKey,
    self_uid: i64,
    bob_uid: i64,
) {
    let Some(pkt1) = server.rx.recv().await else {
        return;
    };
    let dec1 = parse_encrypted_packet(&pkt1, &key, Side::Server).expect("decrypt pkt1");
    let ping = mtp_fns::Ping::from_bytes(&dec1.body).expect("parse Ping");

    let salt = 12_345u64;
    let mut srv_msg_id = (dec1.msg_id & !3) + 1;
    let mut srv_seq_no = 1u32;

    let mut send_server = |body: &[u8]| {
        let pkt = build_encrypted_packet(
            salt,
            dec1.session_id,
            srv_msg_id,
            srv_seq_no,
            body,
            &key,
            Side::Server,
        );
        srv_msg_id += 4;
        srv_seq_no += 2;
        let _ = server.tx.send(Ok(pkt));
    };

    send_server(
        &mtp_enums::NewSession::Created(mtp_types::NewSessionCreated {
            first_msg_id: dec1.msg_id as i64,
            unique_id: 9001,
            server_salt: salt as i64,
        })
        .to_bytes(),
    );
    send_server(
        &mtp_enums::Pong::Pong(mtp_types::Pong {
            msg_id: dec1.msg_id as i64,
            ping_id: ping.ping_id,
        })
        .to_bytes(),
    );

    let self_user = synthetic_tl_user(self_uid, "Alice", "alice_mtproto", true);
    let bob_user = synthetic_tl_user(bob_uid, "Bob", "bob_mtproto", false);
    let bob_msg = synthetic_tl_message(10, bob_uid, bob_uid, false, 1_750_000_000, "Over MTProto!");
    let bob_dialog = synthetic_tl_dialog(bob_uid, 10, 1);

    while let Some(pkt) = server.rx.recv().await {
        let dec = parse_encrypted_packet(&pkt, &key, Side::Server).expect("decrypt client pkt");
        for (mid, ctor) in extract_client_messages(&dec) {
            let reply_bytes: Option<Vec<u8>> = if ctor == tl_fns::users::GetUsers::CONSTRUCTOR_ID {
                Some(vec![tl_enums::User::User(self_user.clone())].to_bytes())
            } else if ctor == tl_fns::updates::GetState::CONSTRUCTOR_ID {
                Some(
                    tl_enums::updates::State::State(tl_types::updates::State {
                        pts: 10,
                        qts: 0,
                        date: 1_750_000_000,
                        seq: 1,
                        unread_count: 1,
                    })
                    .to_bytes(),
                )
            } else if ctor == tl_fns::updates::GetDifference::CONSTRUCTOR_ID {
                Some(
                    tl_enums::updates::Difference::Empty(tl_types::updates::DifferenceEmpty {
                        date: 1_750_000_001,
                        seq: 1,
                    })
                    .to_bytes(),
                )
            } else if ctor == tl_fns::messages::GetDialogs::CONSTRUCTOR_ID {
                Some(
                    tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
                        dialogs: vec![bob_dialog.clone()],
                        messages: vec![bob_msg.clone()],
                        chats: Vec::new(),
                        users: vec![tl_enums::User::User(bob_user.clone())],
                    })
                    .to_bytes(),
                )
            } else if ctor == tl_fns::messages::GetPinnedDialogs::CONSTRUCTOR_ID {
                Some(
                    tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
                        dialogs: Vec::new(),
                        messages: Vec::new(),
                        chats: Vec::new(),
                        users: Vec::new(),
                        state: tl_enums::updates::State::State(tl_types::updates::State {
                            pts: 10,
                            qts: 0,
                            date: 1_750_000_001,
                            seq: 1,
                            unread_count: 1,
                        }),
                    })
                    .to_bytes(),
                )
            } else if ctor == tl_fns::messages::SendMessage::CONSTRUCTOR_ID {
                Some(
                    tl_enums::Updates::UpdateShortSentMessage(tl_types::UpdateShortSentMessage {
                        out: true,
                        id: 42,
                        pts: 11,
                        pts_count: 1,
                        date: 1_750_000_010,
                        media: None,
                        entities: None,
                        ttl_period: None,
                    })
                    .to_bytes(),
                )
            } else {
                None
            };

            if let Some(payload) = reply_bytes {
                let res = tg_tl::Boxed(RpcResult {
                    req_msg_id: mid as i64,
                    result: tg_tl::RawObject(payload),
                })
                .to_bytes();
                send_server(&res);
            }
        }
    }
}

#[test]
fn live_session_over_mtproto_fake_transport() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build tokio runtime");

    let self_uid = 600_001i64;
    let bob_uid = 600_002i64;
    let bob_peer = PeerId::from_user(bob_uid as u64);

    let (client, server_task, runtime) = rt.block_on(async {
        let factory = Arc::new(FakeTransportFactory::new());
        let server = factory.push_connection().await;
        let key = AuthKey::new([0x5Au8; 256]);
        let cfg = ClientConfig {
            api_id: 99_001,
            main_dc_id: 2,
            use_pfs: false,
            ..ClientConfig::default()
        };
        let (client, _raw_updates) = MtprotoClient::with_transport(cfg, factory);
        client.import_persistent_keys(vec![(2, key.clone())]);

        let server_task = tokio::spawn(drive_mtproto_fake_server(server, key, self_uid, bob_uid));
        let rpc = MtprotoRpc::new(client.clone());
        let runtime = Arc::new(
            tg_app::live::start_mtproto_erased_on(
                DataUserId::from_tl(self_uid),
                rpc,
                tokio::runtime::Handle::current(),
                SessionRuntimeConfig::default(),
            )
            .expect("start_mtproto_on"),
        );
        tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready())
            .await
            .expect("mtproto runtime wait_ready timeout")
            .expect("mtproto runtime startup ready");
        (client, server_task, runtime)
    });

    let mut live = LiveSession::new(Arc::clone(&runtime), 1);

    let dialogs = live.dialog_slice(DialogListQuery::Filter(FilterId::ALL), 0, 20);
    assert_eq!(dialogs.entries.len(), 1);
    assert_eq!(dialogs.entries[0].peer_id, bob_peer);
    assert_eq!(dialogs.entries[0].last_message, "Over MTProto!");

    let sent = HistoryBackend::send_message(
        &mut live,
        bob_peer,
        SendMessageRequest {
            text: TextWithEntities::simple("MTProto live hello"),
            reply_to: None,
            silent: false,
        },
    )
    .expect("send_message over MTProto");
    assert!(sent.id.is_local());
    let sent = crate::send_wait::wait_sent(&mut live, bob_peer, sent.id);
    assert_eq!(sent.id, MsgId(42));
    assert_eq!(sent.text.text, "MTProto live hello");

    drop(live);
    if let Ok(rt_inner) = Arc::try_unwrap(runtime) {
        rt_inner.shutdown();
    }
    client.shutdown();
    let _ = rt.block_on(async { tokio::time::timeout(Duration::from_secs(2), server_task).await });
    rt.shutdown_timeout(Duration::from_secs(2));
}

// =============================================================================
// Test 8: Mode selection (credentials, --demo, --live / RUSTGRAM_LIVE=1) & missing credentials UI
// =============================================================================

#[test]
fn live_mode_selection_and_missing_credentials_ui() {
    let tmp = TempDir::new().expect("tempdir");
    let env = WorkDirEnv {
        exe_dir: tmp.path().to_path_buf(),
        app_data_dir: Some(tmp.path().to_path_buf()),
        current_dir: tmp.path().to_path_buf(),
    };
    let prefs = StartupPrefs::default();

    // A plain run carries no explicit mode switch.
    let default_plan =
        StartupPlan::build(["rustgram"], &env, Some(Path::new("rustgram.exe")), &prefs);
    assert!(!default_plan.live_mode);
    assert!(!default_plan.demo_mode);

    // Default mode: live when credentials are available (env or built in), demo otherwise.
    assert!(select_live_mode(false, false, false, true));
    assert!(!select_live_mode(false, false, false, false));
    // `--demo` always wins, for screenshots and tests.
    assert!(!select_live_mode(false, true, false, true));
    assert!(!select_live_mode(true, true, true, true));
    // `--live` / `RUSTGRAM_LIVE=1` force live mode even without credentials (setup screen) and
    // are a no-op when credentials exist.
    assert!(select_live_mode(true, false, false, false));
    assert!(select_live_mode(false, false, true, false));
    assert!(select_live_mode(true, false, false, true));

    let demo_plan = StartupPlan::build(
        ["rustgram", "--demo"],
        &env,
        Some(Path::new("rustgram.exe")),
        &prefs,
    );
    assert!(demo_plan.demo_mode);
    assert!(!demo_plan.live_mode);
    assert!(
        !demo_plan
            .parsed_args
            .start_urls
            .iter()
            .any(|u| u == "--demo"),
        "`--demo` must not reach tdesktop's argument parser"
    );

    // `--live` switch enables LiveSession mode.
    let live_plan = StartupPlan::build(
        ["rustgram", "--live"],
        &env,
        Some(Path::new("rustgram.exe")),
        &prefs,
    );
    assert!(live_plan.live_mode);

    // `RUSTGRAM_LIVE` env parser.
    assert!(parse_live_mode_env(Some("1")));
    assert!(parse_live_mode_env(Some("true")));
    assert!(parse_live_mode_env(Some("YES")));
    assert!(!parse_live_mode_env(Some("0")));
    assert!(!parse_live_mode_env(Some("false")));
    assert!(!parse_live_mode_env(None));

    // Missing credentials show clear in-app MissingCredentials message.
    let host_missing = LiveAppHost::bootstrap_with_env_vars(
        None,
        ShellSettings::default(),
        tmp.path().join("settings.json"),
        tmp.path().to_path_buf(),
        "data".to_owned(),
        None,
        None,
    );
    assert_eq!(host_missing.state(), LiveHostState::MissingCredentials);
    let msg = host_missing
        .status_message()
        .expect("missing credentials message");
    assert!(
        msg.contains("RUSTGRAM_API_ID") && msg.contains("RUSTGRAM_API_HASH"),
        "expected env var instructions in message, got: {msg}"
    );

    // Valid synthetic credentials in a fresh working dir open the Intro login flow.
    let mut host_intro = LiveAppHost::bootstrap_with_env_vars(
        None,
        ShellSettings::default(),
        tmp.path().join("settings.json"),
        tmp.path().to_path_buf(),
        "data".to_owned(),
        Some("99001"),
        Some("0123456789abcdef0123456789abcdef"),
    );
    assert_eq!(host_intro.state(), LiveHostState::Intro);
    host_intro.on_exit();
}

// =============================================================================
// Test 9: BusinessBackend live RPCs & model conversions
// =============================================================================

fn synthetic_tl_channel(
    id: i64,
    title: &str,
    username: Option<&str>,
    broadcast: bool,
) -> tl_types::Channel {
    tl_types::Channel {
        creator: true,
        left: false,
        broadcast,
        verified: false,
        megagroup: !broadcast,
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
        access_hash: Some(id * 10 + 3),
        title: title.to_owned(),
        username: username.map(str::to_owned),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_750_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(12),
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
    }
}

#[test]
fn live_session_business_backend_rpcs() {
    use tg_app::business::model::{
        ActiveEmojiStatus, AwaySchedule, AwayScheduleType, AwaySettings, BusinessBotCandidate,
        BusinessChatTypes, BusinessChats, BusinessLocation, BusinessRecipients, ChatIntro,
        ChatbotsPermissions, ChatbotsSettings, EmojiStatusDuration, GeoPoint, GreetingSettings,
        SECONDS_PER_DAY, WorkingHours, WorkingInterval, WorkingIntervals,
    };

    let mut h = LiveTestHarness::start();
    let bot_uid = 700_001i64;
    let mut bot_user = synthetic_tl_user(bot_uid, "Support", "support_biz_bot", false);
    bot_user.bot = true;
    bot_user.bot_business = true;

    let mut self_user = synthetic_tl_user(h.self_uid, "Alice", "alice_self", true);
    self_user.premium = true;
    self_user.emoji_status = Some(tl_enums::EmojiStatus::EmojiStatus(tl_types::EmojiStatus {
        document_id: 444_555,
        until: None,
    }));

    // 1. Enqueue responses for `load_overview`.
    let full_user = tl_types::UserFull {
        blocked: false,
        phone_calls_available: false,
        phone_calls_private: false,
        can_pin_message: true,
        has_scheduled: false,
        video_calls_available: false,
        voice_messages_forbidden: false,
        translations_disabled: false,
        stories_pinned_available: false,
        blocked_my_stories_from: false,
        wallpaper_overridden: false,
        contact_require_premium: false,
        read_dates_private: false,
        sponsored_enabled: false,
        can_view_revenue: false,
        bot_can_manage_emoji_status: false,
        display_gifts_button: false,
        noforwards_my_enabled: false,
        noforwards_peer_enabled: false,
        unofficial_security_risk: false,
        id: h.self_uid,
        about: Some("Studio account".to_owned()),
        settings: tl_enums::PeerSettings::PeerSettings(tl_types::PeerSettings {
            report_spam: false,
            add_contact: false,
            block_contact: false,
            share_contact: false,
            need_contacts_exception: false,
            report_geo: false,
            autoarchived: false,
            invite_members: false,
            request_chat_broadcast: false,
            business_bot_paused: false,
            business_bot_can_reply: false,
            geo_distance: None,
            request_chat_title: None,
            request_chat_date: None,
            business_bot_id: None,
            business_bot_manage_url: None,
            charge_paid_message_stars: None,
            registration_month: None,
            phone_country: None,
            name_change_date: None,
            photo_change_date: None,
        }),
        personal_photo: None,
        profile_photo: None,
        fallback_photo: None,
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
        bot_info: None,
        pinned_msg_id: None,
        common_chats_count: 0,
        folder_id: None,
        ttl_period: None,
        theme: None,
        private_forward_name: None,
        bot_group_admin_rights: None,
        bot_broadcast_admin_rights: None,
        wallpaper: None,
        stories: None,
        business_work_hours: Some(tl_enums::BusinessWorkHours::BusinessWorkHours(
            tl_types::BusinessWorkHours {
                open_now: true,
                timezone_id: "America/New_York".to_owned(),
                weekly_open: vec![tl_enums::BusinessWeeklyOpen::BusinessWeeklyOpen(
                    tl_types::BusinessWeeklyOpen {
                        start_minute: 9 * 60,
                        end_minute: 17 * 60,
                    },
                )],
            },
        )),
        business_location: Some(tl_enums::BusinessLocation::BusinessLocation(
            tl_types::BusinessLocation {
                geo_point: Some(tl_enums::GeoPoint::GeoPoint(tl_types::GeoPoint {
                    long: -74.006,
                    lat: 40.7128,
                    access_hash: 1,
                    accuracy_radius: None,
                })),
                address: "100 Main St, NY".to_owned(),
            },
        )),
        business_greeting_message: Some(
            tl_enums::BusinessGreetingMessage::BusinessGreetingMessage(
                tl_types::BusinessGreetingMessage {
                    shortcut_id: 11,
                    recipients: tl_enums::BusinessRecipients::BusinessRecipients(
                        tl_types::BusinessRecipients {
                            existing_chats: false,
                            new_chats: false,
                            contacts: true,
                            non_contacts: false,
                            exclude_selected: true,
                            users: None,
                        },
                    ),
                    no_activity_days: 14,
                },
            ),
        ),
        business_away_message: Some(tl_enums::BusinessAwayMessage::BusinessAwayMessage(
            tl_types::BusinessAwayMessage {
                offline_only: true,
                shortcut_id: 12,
                schedule: tl_enums::BusinessAwayMessageSchedule::OutsideWorkHours,
                recipients: tl_enums::BusinessRecipients::BusinessRecipients(
                    tl_types::BusinessRecipients {
                        existing_chats: false,
                        new_chats: true,
                        contacts: false,
                        non_contacts: false,
                        exclude_selected: false,
                        users: Some(vec![h.bob_uid]),
                    },
                ),
            },
        )),
        business_intro: Some(tl_enums::BusinessIntro::BusinessIntro(
            tl_types::BusinessIntro {
                title: "Welcome!".to_owned(),
                description: "Ask anything.".to_owned(),
                sticker: None,
            },
        )),
        birthday: None,
        personal_channel_id: None,
        personal_channel_message: None,
        stargifts_count: None,
        starref_program: None,
        bot_verification: None,
        send_paid_messages_stars: None,
        disallowed_gifts: None,
        stars_rating: None,
        stars_my_pending_rating: None,
        stars_my_pending_rating_date: None,
        main_tab: None,
        saved_music: None,
        note: None,
        bot_manager_id: None,
        wallet_card: None,
        share_theme: None,
    };

    h.fake.enqueue_ok(
        tl_fns::users::GetFullUser::CONSTRUCTOR_ID,
        &tl_enums::users::UserFull::UserFull(tl_types::users::UserFull {
            full_user: tl_enums::UserFull::UserFull(full_user),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(self_user)],
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::help::GetTimezonesList::CONSTRUCTOR_ID,
        &tl_enums::help::TimezonesList::TimezonesList(tl_types::help::TimezonesList {
            timezones: vec![
                tl_enums::Timezone::Timezone(tl_types::Timezone {
                    id: "America/New_York".to_owned(),
                    name: "New York".to_owned(),
                    utc_offset: -18_000,
                }),
                tl_enums::Timezone::Timezone(tl_types::Timezone {
                    id: "UTC".to_owned(),
                    name: "UTC".to_owned(),
                    utc_offset: 0,
                }),
            ],
            hash: 99,
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::account::GetConnectedBots::CONSTRUCTOR_ID,
        &tl_enums::account::ConnectedBots::ConnectedBots(tl_types::account::ConnectedBots {
            connected_bots: vec![tl_enums::ConnectedBot::ConnectedBot(
                tl_types::ConnectedBot {
                    bot_id: bot_uid,
                    recipients: tl_enums::BusinessBotRecipients::BusinessBotRecipients(
                        tl_types::BusinessBotRecipients {
                            existing_chats: false,
                            new_chats: true,
                            contacts: false,
                            non_contacts: true,
                            exclude_selected: false,
                            users: None,
                            exclude_users: Some(vec![h.bob_uid]),
                        },
                    ),
                    rights: tl_enums::BusinessBotRights::BusinessBotRights(
                        tl_types::BusinessBotRights {
                            reply: true,
                            read_messages: true,
                            delete_sent_messages: false,
                            delete_received_messages: false,
                            edit_name: false,
                            edit_bio: false,
                            edit_profile_photo: false,
                            edit_username: false,
                            view_gifts: false,
                            sell_gifts: false,
                            change_gift_settings: false,
                            transfer_and_upgrade_gifts: false,
                            transfer_stars: false,
                            manage_stories: true,
                        },
                    ),
                    device: None,
                    date: None,
                    location: None,
                },
            )],
            users: vec![tl_enums::User::User(bot_user.clone())],
        }),
    );

    let mut qr_msg_1 = match synthetic_tl_message(
        501,
        h.self_uid,
        h.self_uid,
        true,
        1_750_000_000,
        "Hello customer!",
    ) {
        tl_enums::Message::Message(m) => m,
        _ => unreachable!(),
    };
    qr_msg_1.quick_reply_shortcut_id = Some(11);
    let mut qr_msg_2 = match synthetic_tl_message(
        502,
        h.self_uid,
        h.self_uid,
        true,
        1_750_000_060,
        "Second greeting line",
    ) {
        tl_enums::Message::Message(m) => m,
        _ => unreachable!(),
    };
    qr_msg_2.quick_reply_shortcut_id = Some(11);

    h.fake.enqueue_ok(
        tl_fns::messages::GetQuickReplies::CONSTRUCTOR_ID,
        &tl_enums::messages::QuickReplies::QuickReplies(tl_types::messages::QuickReplies {
            quick_replies: vec![tl_enums::QuickReply::QuickReply(tl_types::QuickReply {
                shortcut_id: 11,
                shortcut: "hello".to_owned(),
                top_message: 501,
                count: 2,
            })],
            messages: vec![tl_enums::Message::Message(qr_msg_1.clone())],
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::messages::GetQuickReplyMessages::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![
                tl_enums::Message::Message(qr_msg_1),
                tl_enums::Message::Message(qr_msg_2),
            ],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::account::GetBusinessChatLinks::CONSTRUCTOR_ID,
        &tl_enums::account::BusinessChatLinks::BusinessChatLinks(
            tl_types::account::BusinessChatLinks {
                links: vec![tl_enums::BusinessChatLink::BusinessChatLink(
                    tl_types::BusinessChatLink {
                        link: "https://t.me/m/abc123".to_owned(),
                        message: "Hi there!".to_owned(),
                        entities: None,
                        title: Some("Landing CTA".to_owned()),
                        views: 42,
                    },
                )],
                chats: Vec::new(),
                users: Vec::new(),
            },
        ),
    );
    let inactive_ch = synthetic_tl_channel(880_001, "Old Announcements", None, true);
    h.fake.enqueue_ok(
        tl_fns::channels::GetInactiveChannels::CONSTRUCTOR_ID,
        &tl_enums::messages::InactiveChats::InactiveChats(tl_types::messages::InactiveChats {
            dates: vec![1_700_000_000],
            chats: vec![tl_enums::Chat::Channel(inactive_ch)],
            users: Vec::new(),
        }),
    );
    let public_ch = synthetic_tl_channel(880_002, "Public Feed", Some("public_feed"), true);
    h.fake.enqueue_ok(
        tl_fns::channels::GetAdminedPublicChannels::CONSTRUCTOR_ID,
        &tl_enums::messages::Chats::Chats(tl_types::messages::Chats {
            chats: vec![tl_enums::Chat::Channel(public_ch)],
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::account::GetDefaultEmojiStatuses::CONSTRUCTOR_ID,
        &tl_enums::account::EmojiStatuses::EmojiStatuses(tl_types::account::EmojiStatuses {
            hash: 1,
            statuses: vec![tl_enums::EmojiStatus::EmojiStatus(tl_types::EmojiStatus {
                document_id: 444_555,
                until: None,
            })],
        }),
    );

    let snap = h.live.load_overview().expect("business load_overview");
    assert!(snap.is_premium);
    assert!(snap.sponsored_disabled);
    assert_eq!(snap.details.hours.timezone_id, "America/New_York");
    assert_eq!(
        snap.details.hours.intervals.list,
        vec![WorkingInterval::new(9 * 3600, 17 * 3600)]
    );
    assert_eq!(snap.details.location.address, "100 Main St, NY");
    assert!(snap.details.location.point.is_some());
    assert_eq!(snap.details.intro.title, "Welcome!");
    assert_eq!(snap.greeting.shortcut_id, 11);
    assert_eq!(snap.greeting.no_activity_days, 14);
    assert_eq!(snap.away.shortcut_id, 12);
    assert!(snap.away.offline_only);
    assert_eq!(
        snap.away.schedule.schedule_type,
        AwayScheduleType::OutsideWorkingHours
    );
    assert_eq!(snap.timezones.len(), 2);
    assert_eq!(
        snap.chatbots.bot.as_ref().expect("connected bot").id,
        bot_uid
    );
    assert!(
        snap.chatbots
            .permissions
            .contains(ChatbotsPermissions::MANAGE_STORIES)
    );
    assert_eq!(
        snap.shortcuts.get(&11).expect("shortcut 11").messages.len(),
        2
    );
    assert_eq!(snap.chat_links.len(), 1);
    assert_eq!(snap.chat_links[0].clicks, 42);
    assert_eq!(snap.inactive_channels.len(), 1);
    assert_eq!(snap.inactive_channels[0].id, 880_001);
    assert_eq!(snap.public_link_channels.len(), 1);
    assert_eq!(snap.emoji_status.status_id, Some(444_555));

    // 2. Test all Business mutation RPCs + capture exact request bytes for key conversions.
    let captured_work_hours: Arc<Mutex<Option<tl_fns::account::UpdateBusinessWorkHours>>> =
        Arc::new(Mutex::new(None));
    let cap_wh = Arc::clone(&captured_work_hours);
    h.fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
        if ctor == tl_fns::account::UpdateBusinessWorkHours::CONSTRUCTOR_ID {
            let decoded =
                tl_fns::account::UpdateBusinessWorkHours::from_bytes(raw).expect("decode wh");
            *cap_wh.lock().expect("lock") = Some(decoded);
            return Some(Ok(true.to_bytes()));
        }
        None
    }));

    h.live
        .save_working_hours(WorkingHours {
            timezone_id: "UTC".to_owned(),
            intervals: WorkingIntervals::new(vec![WorkingInterval::new(
                SECONDS_PER_DAY + 8 * 3600,
                SECONDS_PER_DAY + 16 * 3600,
            )]),
        })
        .expect("save_working_hours");
    let wh_req = captured_work_hours
        .lock()
        .expect("lock")
        .clone()
        .expect("captured UpdateBusinessWorkHours");
    let tl_enums::BusinessWorkHours::BusinessWorkHours(wh_inner) =
        wh_req.business_work_hours.expect("hours set");
    assert_eq!(wh_inner.timezone_id, "UTC");
    let tl_enums::BusinessWeeklyOpen::BusinessWeeklyOpen(slot) = &wh_inner.weekly_open[0];
    assert_eq!(slot.start_minute, (SECONDS_PER_DAY + 8 * 3600) / 60);
    assert_eq!(slot.end_minute, (SECONDS_PER_DAY + 16 * 3600) / 60);

    h.fake.enqueue_ok(
        tl_fns::account::UpdateBusinessLocation::CONSTRUCTOR_ID,
        &true,
    );
    h.live
        .save_location(BusinessLocation {
            address: "200 Oak Ave".to_owned(),
            point: Some(GeoPoint {
                lat: 51.5074,
                lon: -0.1278,
            }),
        })
        .expect("save_location");

    h.fake
        .enqueue_ok(tl_fns::account::UpdateBusinessIntro::CONSTRUCTOR_ID, &true);
    h.live
        .save_chat_intro(ChatIntro {
            title: "Hello".to_owned(),
            description: "Fast support".to_owned(),
            sticker_id: None,
        })
        .expect("save_chat_intro");

    h.fake.enqueue_ok(
        tl_fns::account::UpdateBusinessGreetingMessage::CONSTRUCTOR_ID,
        &true,
    );
    h.live
        .save_greeting_settings(GreetingSettings {
            recipients: BusinessRecipients {
                included: BusinessChats::default(),
                excluded: BusinessChats::default(),
                all_but_excluded: true,
            },
            no_activity_days: 7,
            shortcut_id: 11,
        })
        .expect("save_greeting_settings");

    h.fake.enqueue_ok(
        tl_fns::account::UpdateBusinessAwayMessage::CONSTRUCTOR_ID,
        &true,
    );
    h.live
        .save_away_settings(AwaySettings {
            recipients: BusinessRecipients {
                included: BusinessChats {
                    types: BusinessChatTypes::NON_CONTACTS,
                    user_ids: Vec::new(),
                },
                excluded: BusinessChats::default(),
                all_but_excluded: false,
            },
            schedule: AwaySchedule {
                schedule_type: AwayScheduleType::Always,
                custom_interval: WorkingInterval::default(),
            },
            shortcut_id: 12,
            offline_only: false,
        })
        .expect("save_away_settings");

    let empty_updates = tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_750_000_100,
        seq: 5,
    });
    h.fake.enqueue_ok(
        tl_fns::account::UpdateConnectedBot::CONSTRUCTOR_ID,
        &empty_updates,
    );
    h.live
        .save_chatbots(ChatbotsSettings {
            bot: Some(BusinessBotCandidate {
                id: bot_uid,
                name: "Support".to_owned(),
                username: "support_biz_bot".to_owned(),
                about: String::new(),
                supports_business: true,
                avatar_rgb: [0, 0, 0],
            }),
            recipients: BusinessRecipients::default().make_valid(),
            permissions: ChatbotsPermissions::VIEW_MESSAGES
                .union(ChatbotsPermissions::REPLY_TO_MESSAGES),
        })
        .expect("save_chatbots");

    // Quick reply mutations + sendQuickReplyMessages
    h.fake.enqueue_ok(
        tl_fns::messages::EditQuickReplyShortcut::CONSTRUCTOR_ID,
        &true,
    );
    let renamed = h
        .live
        .rename_shortcut(11, "welcome")
        .expect("rename_shortcut");
    assert_eq!(renamed.name, "welcome");

    h.fake.enqueue_ok(
        tl_fns::messages::SendQuickReplyMessages::CONSTRUCTOR_ID,
        &empty_updates,
    );
    h.live
        .send_quick_reply_messages(h.bob_peer, 11, &[501])
        .expect("send_quick_reply_messages");

    h.fake.enqueue_ok(
        tl_fns::messages::DeleteQuickReplyShortcut::CONSTRUCTOR_ID,
        &true,
    );
    h.live.remove_shortcut(11).expect("remove_shortcut");

    // Business chat links CRUD
    h.fake.enqueue_ok(
        tl_fns::account::CreateBusinessChatLink::CONSTRUCTOR_ID,
        &tl_enums::BusinessChatLink::BusinessChatLink(tl_types::BusinessChatLink {
            link: "https://t.me/m/newlink".to_owned(),
            message: "Start order".to_owned(),
            entities: None,
            title: Some("Orders".to_owned()),
            views: 0,
        }),
    );
    let created_link = h
        .live
        .create_chat_link("Orders", "Start order")
        .expect("create_chat_link");
    assert_eq!(created_link.link, "https://t.me/m/newlink");

    h.fake.enqueue_ok(
        tl_fns::account::EditBusinessChatLink::CONSTRUCTOR_ID,
        &tl_enums::BusinessChatLink::BusinessChatLink(tl_types::BusinessChatLink {
            link: "https://t.me/m/newlink".to_owned(),
            message: "Updated order".to_owned(),
            entities: None,
            title: Some("Orders v2".to_owned()),
            views: 3,
        }),
    );
    let edited_link = h
        .live
        .edit_chat_link("https://t.me/m/newlink", "Orders v2", "Updated order")
        .expect("edit_chat_link");
    assert_eq!(edited_link.title, "Orders v2");

    h.fake.enqueue_ok(
        tl_fns::account::DeleteBusinessChatLink::CONSTRUCTOR_ID,
        &true,
    );
    h.live
        .delete_chat_link("https://t.me/m/newlink")
        .expect("delete_chat_link");

    // Emoji status + sponsored toggle + inactive/public channel cleanup
    h.fake
        .enqueue_ok(tl_fns::account::UpdateEmojiStatus::CONSTRUCTOR_ID, &true);
    h.live
        .set_emoji_status(ActiveEmojiStatus {
            status_id: Some(999_111),
            duration: EmojiStatusDuration::OneHour,
        })
        .expect("set_emoji_status");

    h.fake.enqueue_ok(
        tl_fns::account::ToggleSponsoredMessages::CONSTRUCTOR_ID,
        &true,
    );
    h.live
        .set_sponsored_disabled(true)
        .expect("set_sponsored_disabled");

    h.fake.enqueue_ok(
        tl_fns::channels::LeaveChannel::CONSTRUCTOR_ID,
        &empty_updates,
    );
    let left_count = h
        .live
        .leave_inactive_channels(&[880_001])
        .expect("leave_inactive_channels");
    assert_eq!(left_count, 1);

    h.fake
        .enqueue_ok(tl_fns::channels::UpdateUsername::CONSTRUCTOR_ID, &true);
    h.live
        .revoke_public_link(880_002)
        .expect("revoke_public_link");

    let ctors = h.fake.recorded_ctors();
    for expected in [
        tl_fns::users::GetFullUser::CONSTRUCTOR_ID,
        tl_fns::help::GetTimezonesList::CONSTRUCTOR_ID,
        tl_fns::account::GetConnectedBots::CONSTRUCTOR_ID,
        tl_fns::messages::GetQuickReplies::CONSTRUCTOR_ID,
        tl_fns::messages::GetQuickReplyMessages::CONSTRUCTOR_ID,
        tl_fns::account::GetBusinessChatLinks::CONSTRUCTOR_ID,
        tl_fns::channels::GetInactiveChannels::CONSTRUCTOR_ID,
        tl_fns::account::UpdateBusinessWorkHours::CONSTRUCTOR_ID,
        tl_fns::account::UpdateBusinessLocation::CONSTRUCTOR_ID,
        tl_fns::account::UpdateBusinessIntro::CONSTRUCTOR_ID,
        tl_fns::account::UpdateBusinessGreetingMessage::CONSTRUCTOR_ID,
        tl_fns::account::UpdateBusinessAwayMessage::CONSTRUCTOR_ID,
        tl_fns::account::UpdateConnectedBot::CONSTRUCTOR_ID,
        tl_fns::messages::EditQuickReplyShortcut::CONSTRUCTOR_ID,
        tl_fns::messages::SendQuickReplyMessages::CONSTRUCTOR_ID,
        tl_fns::messages::DeleteQuickReplyShortcut::CONSTRUCTOR_ID,
        tl_fns::account::CreateBusinessChatLink::CONSTRUCTOR_ID,
        tl_fns::account::EditBusinessChatLink::CONSTRUCTOR_ID,
        tl_fns::account::DeleteBusinessChatLink::CONSTRUCTOR_ID,
        tl_fns::account::UpdateEmojiStatus::CONSTRUCTOR_ID,
        tl_fns::account::ToggleSponsoredMessages::CONSTRUCTOR_ID,
        tl_fns::channels::LeaveChannel::CONSTRUCTOR_ID,
        tl_fns::channels::UpdateUsername::CONSTRUCTOR_ID,
    ] {
        assert!(
            ctors.contains(&expected),
            "missing expected business TL ctor 0x{expected:08x}"
        );
    }

    h.shutdown();
}

// =============================================================================
// Test 10: PaymentsBackend (Checkout + fake HTTP tokenization + Stars + Gifts + Promo + Boosts)
// =============================================================================

struct FakePaymentHttpClient {
    recorded: Arc<Mutex<Vec<tg_payments::providers::HttpRequest>>>,
    response_body: Vec<u8>,
}

impl tg_payments::providers::HttpClient for FakePaymentHttpClient {
    fn send(
        &self,
        request: &tg_payments::providers::HttpRequest,
    ) -> Result<tg_payments::providers::HttpResponse, tg_payments::providers::HttpTransportError>
    {
        self.recorded
            .lock()
            .expect("lock http")
            .push(request.clone());
        Ok(tg_payments::providers::HttpResponse {
            status_code: 200,
            body: self.response_body.clone(),
        })
    }
}

#[test]
fn live_session_payments_stars_gifts_promo_giveaway_rpcs() {
    use tg_app::payments::backend::{PaymentSubmitOutcome, ReceivedGiftAction};
    use tg_payments::card::UncheckedCardDetails;
    use tg_payments::credits::CreditsAmount;
    use tg_payments::form::{PostAddress, RequestedInformation};
    use tg_payments::premium::GiveawayState;

    let mut h = LiveTestHarness::start();
    let seller_bot = synthetic_tl_user(710_001, "ShopBot", "shop_bot", false);
    let provider_bot = synthetic_tl_user(710_002, "StripeTest", "stripe_test", false);

    // 1. Load checkout form (`payments.getPaymentForm`) with Stripe native params.
    let tl_invoice = tl_enums::Invoice::Invoice(tl_types::Invoice {
        test: true,
        name_requested: true,
        phone_requested: false,
        email_requested: true,
        shipping_address_requested: true,
        flexible: true,
        phone_to_provider: false,
        email_to_provider: false,
        recurring: false,
        currency: "USD".to_owned(),
        prices: vec![tl_enums::LabeledPrice::LabeledPrice(
            tl_types::LabeledPrice {
                label: "Pro Pack".to_owned(),
                amount: 1_500,
            },
        )],
        max_tip_amount: Some(500),
        suggested_tip_amounts: Some(vec![100, 300]),
        terms_url: None,
        subscription_period: None,
    });
    h.fake.enqueue_ok(
        tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID,
        &tl_enums::payments::PaymentForm::PaymentForm(tl_types::payments::PaymentForm {
            can_save_credentials: true,
            password_missing: false,
            form_id: 987_654,
            bot_id: seller_bot.id,
            title: "Pro Pack".to_owned(),
            description: "Digital synth pack".to_owned(),
            photo: None,
            invoice: tl_invoice.clone(),
            provider_id: provider_bot.id,
            url: "https://example.test/checkout".to_owned(),
            native_provider: Some("stripe".to_owned()),
            native_params: Some(tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                data: r#"{"publishable_key":"pk_test_synthetic_unit_key","need_country":true,"need_zip":true,"need_cardholder_name":true}"#
                    .to_owned(),
            })),
            additional_methods: None,
            saved_info: None,
            saved_credentials: None,
            users: vec![
                tl_enums::User::User(seller_bot.clone()),
                tl_enums::User::User(provider_bot.clone()),
            ],
        }),
    );

    let bundle = h
        .live
        .load_checkout_form("synth_invoice_slug")
        .expect("load_checkout_form");
    assert_eq!(bundle.form.details.form_id, 987_654);
    assert_eq!(bundle.form.invoice.currency, "USD");
    assert_eq!(bundle.form.invoice.cover.title, "Pro Pack");
    assert_eq!(bundle.cover_badge, "TEST");

    // 2. Validate requested info (`payments.validateRequestedInfo`).
    h.fake.enqueue_ok(
        tl_fns::payments::ValidateRequestedInfo::CONSTRUCTOR_ID,
        &tl_enums::payments::ValidatedRequestedInfo::ValidatedRequestedInfo(
            tl_types::payments::ValidatedRequestedInfo {
                id: Some("req_info_99".to_owned()),
                shipping_options: Some(vec![tl_enums::ShippingOption::ShippingOption(
                    tl_types::ShippingOption {
                        id: "ship_fast".to_owned(),
                        title: "Express".to_owned(),
                        prices: vec![tl_enums::LabeledPrice::LabeledPrice(
                            tl_types::LabeledPrice {
                                label: "Shipping".to_owned(),
                                amount: 400,
                            },
                        )],
                    },
                )]),
            },
        ),
    );
    let validated = h
        .live
        .validate_requested_info(
            "synth_invoice_slug",
            &RequestedInformation {
                save: true,
                name: "Alex Tester".to_owned(),
                email: "alex@example.test".to_owned(),
                shipping_address: PostAddress {
                    address1: "123 Test Ln".to_owned(),
                    city: "Testville".to_owned(),
                    country_iso2: "US".to_owned(),
                    postcode: "10001".to_owned(),
                    ..PostAddress::default()
                },
                ..RequestedInformation::default()
            },
        )
        .expect("validate_requested_info");
    assert_eq!(validated.requested_info_id, "req_info_99");
    assert_eq!(validated.shipping_options.len(), 1);

    // 3. Tokenize card via `tg-payments` StripeClient behind fake HttpClient (zero network!).
    let http_recorded = Arc::new(Mutex::new(Vec::new()));
    let fake_http = Arc::new(FakePaymentHttpClient {
        recorded: Arc::clone(&http_recorded),
        response_body: br#"{"id":"tok_synth_4242","livemode":false,"created":1750000000,"card":{"id":"card_1","last4":"4242","brand":"Visa","exp_month":12,"exp_year":2030}}"#.to_vec(),
    });
    h.live.set_payment_http_client(fake_http);
    let creds = h
        .live
        .tokenize_test_card(
            &UncheckedCardDetails {
                number: "4242 4242 4242 4242".to_owned(),
                cvc: "123".to_owned(),
                expire_year: 2030,
                expire_month: 12,
                cardholder_name: "Alex Tester".to_owned(),
                address_country: "US".to_owned(),
                address_zip: "10001".to_owned(),
            },
            true,
        )
        .expect("tokenize_test_card");
    assert_eq!(creds.title, "visa *4242");
    assert!(creds.data.contains("tok_synth_4242"));
    let sent_http = http_recorded.lock().expect("lock http");
    assert_eq!(sent_http.len(), 1);
    assert_eq!(sent_http[0].url, "https://api.stripe.com/v1/tokens");
    assert!(
        sent_http[0]
            .headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer pk_test_synthetic_unit_key")
    );
    drop(sent_http);

    // 4. Submit payment form (`payments.sendPaymentForm`) -> 3DS VerificationNeeded + Paid.
    h.fake.enqueue_ok(
        tl_fns::payments::SendPaymentForm::CONSTRUCTOR_ID,
        &tl_enums::payments::PaymentResult::PaymentVerificationNeeded(
            tl_types::payments::PaymentVerificationNeeded {
                url: "https://verify.example.test/3ds".to_owned(),
            },
        ),
    );
    let outcome_3ds = h
        .live
        .submit_payment_form(
            "synth_invoice_slug",
            987_654,
            Some("ship_fast"),
            100,
            "",
            true,
        )
        .expect("submit_payment_form 3ds");
    match outcome_3ds {
        PaymentSubmitOutcome::VerificationNeeded { url, .. } => {
            assert_eq!(url, "https://verify.example.test/3ds");
        }
        other => panic!("expected VerificationNeeded, got {other:?}"),
    }

    h.fake.enqueue_ok(
        tl_fns::payments::SendPaymentForm::CONSTRUCTOR_ID,
        &tl_enums::payments::PaymentResult::PaymentResult(tl_types::payments::PaymentResult {
            updates: tl_enums::Updates::Updates(tl_types::Updates {
                updates: Vec::new(),
                users: Vec::new(),
                chats: Vec::new(),
                date: 1_750_000_200,
                seq: 10,
            }),
        }),
    );
    let outcome_paid = h
        .live
        .submit_payment_form(
            "synth_invoice_slug",
            987_654,
            Some("ship_fast"),
            100,
            "",
            false,
        )
        .expect("submit_payment_form paid");
    match outcome_paid {
        PaymentSubmitOutcome::Paid {
            total_amount,
            currency,
            ..
        } => {
            assert_eq!(total_amount, 2_000); // 1500 base + 400 shipping + 100 tip
            assert_eq!(currency, "USD");
        }
        other => panic!("expected Paid, got {other:?}"),
    }

    // 5. Load receipt (`payments.getPaymentReceipt`).
    h.fake.enqueue_ok(
        tl_fns::payments::GetPaymentReceipt::CONSTRUCTOR_ID,
        &tl_enums::payments::PaymentReceipt::PaymentReceipt(tl_types::payments::PaymentReceipt {
            date: 1_750_000_200,
            bot_id: seller_bot.id,
            provider_id: provider_bot.id,
            title: "Pro Pack".to_owned(),
            description: "Paid receipt".to_owned(),
            photo: None,
            invoice: tl_invoice,
            info: None,
            shipping: None,
            tip_amount: Some(100),
            currency: "USD".to_owned(),
            total_amount: 2_000,
            credentials_title: "Visa *4242".to_owned(),
            users: vec![tl_enums::User::User(seller_bot.clone())],
        }),
    );
    let receipt = h
        .live
        .load_payment_receipt(h.bob_peer, MsgId(10))
        .expect("load_payment_receipt");
    assert!(receipt.invoice.receipt.paid);
    assert_eq!(receipt.invoice.receipt.total_amount, 2_000);

    // 6. Stars checkout form -> `payments.sendStarsForm`.
    h.fake.enqueue_ok(
        tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID,
        &tl_enums::payments::PaymentForm::Stars(tl_types::payments::PaymentFormStars {
            form_id: 555_001,
            bot_id: seller_bot.id,
            title: "Stars Item".to_owned(),
            description: "Unlock feature".to_owned(),
            photo: None,
            invoice: tl_enums::Invoice::Invoice(tl_types::Invoice {
                test: false,
                name_requested: false,
                phone_requested: false,
                email_requested: false,
                shipping_address_requested: false,
                flexible: false,
                phone_to_provider: false,
                email_to_provider: false,
                recurring: false,
                currency: "XTR".to_owned(),
                prices: vec![tl_enums::LabeledPrice::LabeledPrice(
                    tl_types::LabeledPrice {
                        label: "Stars".to_owned(),
                        amount: 50,
                    },
                )],
                max_tip_amount: None,
                suggested_tip_amounts: None,
                terms_url: None,
                subscription_period: None,
            }),
            users: vec![tl_enums::User::User(seller_bot)],
        }),
    );
    let stars_bundle = h
        .live
        .load_checkout_form("stars_slug")
        .expect("load stars form");
    assert_eq!(stars_bundle.cover_badge, "STARS");
    h.fake.enqueue_ok(
        tl_fns::payments::SendStarsForm::CONSTRUCTOR_ID,
        &tl_enums::payments::PaymentResult::PaymentResult(tl_types::payments::PaymentResult {
            updates: tl_enums::Updates::Updates(tl_types::Updates {
                updates: Vec::new(),
                users: Vec::new(),
                chats: Vec::new(),
                date: 1_750_000_210,
                seq: 11,
            }),
        }),
    );
    let stars_paid = h
        .live
        .submit_payment_form("stars_slug", 555_001, None, 0, "", false)
        .expect("submit stars form");
    assert!(matches!(stars_paid, PaymentSubmitOutcome::Paid { .. }));

    // 7. Stars overview (`getStarsStatus`, `getStarsTransactions`, `getStarsTopupOptions`).
    let stars_status_resp =
        tl_enums::payments::StarsStatus::StarsStatus(tl_types::payments::StarsStatus {
            balance: tl_enums::StarsAmount::StarsAmount(tl_types::StarsAmount {
                amount: 750,
                nanos: 0,
            }),
            subscriptions: None,
            subscriptions_next_offset: None,
            subscriptions_missing_balance: None,
            history: Some(vec![tl_enums::StarsTransaction::StarsTransaction(
                tl_types::StarsTransaction {
                    refund: false,
                    pending: false,
                    failed: false,
                    gift: false,
                    reaction: false,
                    stargift_upgrade: false,
                    business_transfer: false,
                    stargift_resale: false,
                    posts_search: false,
                    stargift_prepaid_upgrade: false,
                    stargift_drop_original_details: false,
                    phonegroup_message: false,
                    stargift_auction_bid: false,
                    offer: false,
                    id: "tx_live_1".to_owned(),
                    amount: tl_enums::StarsAmount::StarsAmount(tl_types::StarsAmount {
                        amount: 500,
                        nanos: 0,
                    }),
                    date: 1_750_000_000,
                    peer: tl_enums::StarsTransactionPeer::Fragment,
                    title: Some("Top-up".to_owned()),
                    description: None,
                    photo: None,
                    transaction_date: None,
                    transaction_url: None,
                    bot_payload: None,
                    msg_id: None,
                    extended_media: None,
                    subscription_period: None,
                    giveaway_post_id: None,
                    stargift: None,
                    floodskip_number: None,
                    starref_commission_permille: None,
                    starref_peer: None,
                    starref_amount: None,
                    paid_messages: None,
                    premium_gift_months: None,
                    ads_proceeds_from_date: None,
                    ads_proceeds_to_date: None,
                },
            )]),
            next_offset: None,
            chats: Vec::new(),
            users: Vec::new(),
        });
    h.fake.enqueue_ok(
        tl_fns::payments::GetStarsStatus::CONSTRUCTOR_ID,
        &stars_status_resp,
    );
    h.fake.enqueue_ok(
        tl_fns::payments::GetStarsTransactions::CONSTRUCTOR_ID,
        &stars_status_resp,
    );
    h.fake.enqueue_ok(
        tl_fns::payments::GetStarsStatus::CONSTRUCTOR_ID,
        &tl_enums::payments::StarsStatus::StarsStatus(tl_types::payments::StarsStatus {
            balance: tl_enums::StarsAmount::StarsTonAmount(tl_types::StarsTonAmount {
                amount: 2_500_000_000,
            }),
            subscriptions: None,
            subscriptions_next_offset: None,
            subscriptions_missing_balance: None,
            history: None,
            next_offset: None,
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::payments::GetStarsTopupOptions::CONSTRUCTOR_ID,
        &vec![tl_enums::StarsTopupOption::StarsTopupOption(
            tl_types::StarsTopupOption {
                extended: false,
                stars: 100,
                store_product: Some("org.tg.stars100".to_owned()),
                currency: "USD".to_owned(),
                amount: 199,
            },
        )],
    );
    let credits = h
        .live
        .load_credits_overview()
        .expect("load_credits_overview");
    assert_eq!(credits.balance, CreditsAmount::stars(750));
    assert_eq!(credits.ton_balance.whole(), 2);
    assert_eq!(credits.history.len(), 1);
    assert_eq!(credits.topup_options.len(), 1);
    assert_eq!(credits.topup_options[0].credits, 100);

    // 8. Star Gifts (`getStarGifts`, `getSavedStarGifts`, `convertStarGift`).
    let tl_gift = tl_enums::StarGift::StarGift(tl_types::StarGift {
        limited: true,
        sold_out: false,
        birthday: false,
        require_premium: false,
        limited_per_user: false,
        peer_color_available: false,
        auction: false,
        id: 77_001,
        sticker: tl_enums::Document::Empty(tl_types::DocumentEmpty { id: 1234 }),
        stars: 100,
        availability_remains: Some(10),
        availability_total: Some(100),
        availability_resale: None,
        convert_stars: 85,
        first_sale_date: None,
        last_sale_date: None,
        upgrade_stars: Some(25),
        resell_min_stars: None,
        title: Some("Crystal Star".to_owned()),
        released_by: None,
        per_user_total: None,
        per_user_remains: None,
        locked_until_date: None,
        auction_slug: None,
        gifts_per_round: None,
        auction_start_date: None,
        upgrade_variants: None,
        background: None,
    });
    h.fake.enqueue_ok(
        tl_fns::payments::GetStarsStatus::CONSTRUCTOR_ID,
        &stars_status_resp,
    );
    h.fake.enqueue_ok(
        tl_fns::payments::GetStarGifts::CONSTRUCTOR_ID,
        &tl_enums::payments::StarGifts::StarGifts(tl_types::payments::StarGifts {
            hash: 1,
            gifts: vec![tl_gift.clone()],
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::payments::GetSavedStarGifts::CONSTRUCTOR_ID,
        &tl_enums::payments::SavedStarGifts::SavedStarGifts(tl_types::payments::SavedStarGifts {
            count: 1,
            chat_notifications_enabled: None,
            gifts: vec![tl_enums::SavedStarGift::SavedStarGift(
                tl_types::SavedStarGift {
                    name_hidden: false,
                    unsaved: false,
                    refunded: false,
                    can_upgrade: true,
                    pinned_to_top: false,
                    upgrade_separate: false,
                    from_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: h.bob_uid,
                    })),
                    date: 1_750_000_000,
                    gift: tl_gift,
                    message: None,
                    msg_id: Some(901),
                    saved_id: None,
                    convert_stars: Some(85),
                    upgrade_stars: None,
                    can_export_at: None,
                    transfer_stars: None,
                    can_transfer_at: None,
                    can_resell_at: None,
                    collection_id: None,
                    prepaid_upgrade_hash: None,
                    drop_original_details_stars: None,
                    gift_num: Some(1),
                    can_craft_at: None,
                },
            )],
            next_offset: None,
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    let gifts = h.live.load_star_gifts().expect("load_star_gifts");
    assert_eq!(gifts.catalog.len(), 1);
    assert_eq!(gifts.catalog[0].title, "Crystal Star");
    assert_eq!(gifts.received.len(), 1);
    assert_eq!(gifts.received[0].sender_name, "Bob Tester");

    h.fake
        .enqueue_ok(tl_fns::payments::ConvertStarGift::CONSTRUCTOR_ID, &true);
    let conv_msg = h
        .live
        .apply_received_gift_action(77_001, ReceivedGiftAction::ConvertToStars)
        .expect("convertStarGift");
    assert!(conv_msg.contains("77001"));

    // 9. Premium promo (`help.getPremiumPromo`).
    h.fake.enqueue_ok(
        tl_fns::help::GetPremiumPromo::CONSTRUCTOR_ID,
        &tl_enums::help::PremiumPromo::PremiumPromo(tl_types::help::PremiumPromo {
            status_text: "Unlock double limits".to_owned(),
            status_entities: Vec::new(),
            video_sections: vec!["more_upload".to_owned(), "faster_download".to_owned()],
            videos: vec![
                tl_enums::Document::Empty(tl_types::DocumentEmpty { id: 101 }),
                tl_enums::Document::Empty(tl_types::DocumentEmpty { id: 102 }),
            ],
            period_options: vec![
                tl_enums::PremiumSubscriptionOption::PremiumSubscriptionOption(
                    tl_types::PremiumSubscriptionOption {
                        current: false,
                        can_purchase_upgrade: false,
                        transaction: None,
                        months: 1,
                        currency: "USD".to_owned(),
                        amount: 499,
                        bot_url: "https://t.me/PremiumBot?start=1m".to_owned(),
                        store_product: None,
                    },
                ),
                tl_enums::PremiumSubscriptionOption::PremiumSubscriptionOption(
                    tl_types::PremiumSubscriptionOption {
                        current: false,
                        can_purchase_upgrade: false,
                        transaction: None,
                        months: 12,
                        currency: "USD".to_owned(),
                        amount: 3599,
                        bot_url: "https://t.me/PremiumBot?start=12m".to_owned(),
                        store_product: None,
                    },
                ),
            ],
            users: Vec::new(),
        }),
    );
    let promo = h.live.load_premium_promo().expect("load_premium_promo");
    assert_eq!(promo.promo.status_text, "Unlock double limits");
    assert_eq!(promo.promo.period_options.len(), 2);
    assert_eq!(promo.features.len(), 2);

    // 10. Boosts & Giveaway info (`premium.getBoostsStatus/List/MyBoosts` + `payments.getGiveawayInfo`).
    h.live.set_giveaway_target(h.bob_peer, MsgId(10));
    h.fake.enqueue_ok(
        tl_fns::premium::GetBoostsStatus::CONSTRUCTOR_ID,
        &tl_enums::premium::BoostsStatus::BoostsStatus(tl_types::premium::BoostsStatus {
            my_boost: true,
            level: 3,
            current_level_boosts: 10,
            boosts: 14,
            gift_boosts: None,
            next_level_boosts: Some(20),
            premium_audience: None,
            boost_url: "https://t.me/boost/test".to_owned(),
            prepaid_giveaways: None,
            my_boost_slots: Some(vec![1]),
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::premium::GetBoostsList::CONSTRUCTOR_ID,
        &tl_enums::premium::BoostsList::BoostsList(tl_types::premium::BoostsList {
            count: 0,
            boosts: Vec::new(),
            next_offset: None,
            users: Vec::new(),
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::premium::GetMyBoosts::CONSTRUCTOR_ID,
        &tl_enums::premium::MyBoosts::MyBoosts(tl_types::premium::MyBoosts {
            my_boosts: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    h.fake.enqueue_ok(
        tl_fns::payments::GetGiveawayInfo::CONSTRUCTOR_ID,
        &tl_enums::payments::GiveawayInfo::GiveawayInfo(tl_types::payments::GiveawayInfo {
            participating: true,
            preparing_results: false,
            start_date: 1_750_000_000,
            joined_too_early_date: None,
            admin_disallowed_chat_id: None,
            disallowed_country: None,
        }),
    );
    let boosts = h.live.load_boosts_giveaway().expect("load_boosts_giveaway");
    assert_eq!(boosts.boost_status.overview.level, 3);
    assert_eq!(boosts.boost_status.overview.boost_count, 14);
    assert_eq!(boosts.giveaways.len(), 1);
    assert_eq!(boosts.giveaways[0].info.state, GiveawayState::Running);
    assert!(boosts.giveaways[0].info.participating);

    let ctors = h.fake.recorded_ctors();
    for expected in [
        tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID,
        tl_fns::payments::ValidateRequestedInfo::CONSTRUCTOR_ID,
        tl_fns::payments::SendPaymentForm::CONSTRUCTOR_ID,
        tl_fns::payments::GetPaymentReceipt::CONSTRUCTOR_ID,
        tl_fns::payments::SendStarsForm::CONSTRUCTOR_ID,
        tl_fns::payments::GetStarsStatus::CONSTRUCTOR_ID,
        tl_fns::payments::GetStarsTransactions::CONSTRUCTOR_ID,
        tl_fns::payments::GetStarsTopupOptions::CONSTRUCTOR_ID,
        tl_fns::payments::GetStarGifts::CONSTRUCTOR_ID,
        tl_fns::payments::GetSavedStarGifts::CONSTRUCTOR_ID,
        tl_fns::payments::ConvertStarGift::CONSTRUCTOR_ID,
        tl_fns::help::GetPremiumPromo::CONSTRUCTOR_ID,
        tl_fns::premium::GetBoostsStatus::CONSTRUCTOR_ID,
        tl_fns::payments::GetGiveawayInfo::CONSTRUCTOR_ID,
    ] {
        assert!(
            ctors.contains(&expected),
            "missing expected payments TL ctor 0x{expected:08x}"
        );
    }

    h.shutdown();
}

// =============================================================================
// Test 11: PassportBackend live RPCs with real SRP + AES-256-CBC + RSA-OAEP encryption
// =============================================================================

fn build_synthetic_srp_password_tl(
    password: &str,
    has_password: bool,
    has_recovery: bool,
    has_secure_values: bool,
    hint: Option<&str>,
    unconfirmed_email: Option<&str>,
) -> (
    tl_enums::account::Password,
    tg_crypto::srp::PasswordKdfAlgoModPow,
) {
    use num_bigint::BigUint;

    let algo = tg_crypto::srp::PasswordKdfAlgoModPow {
        salt1: b"test_salt_1_0123456789ab".to_vec(),
        salt2: b"test_salt_2_fedcba987654".to_vec(),
        g: 3,
        p: tg_crypto::dh::GOOD_PRIME.to_vec(),
    };
    let tl_algo = tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(
        tl_types::PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow {
            salt1: algo.salt1.clone(),
            salt2: algo.salt2.clone(),
            g: algo.g,
            p: algo.p.clone(),
        },
    );

    let (srp_b, srp_id, current_algo) = if has_password {
        let p = BigUint::from_bytes_be(&algo.p);
        let g = BigUint::from(algo.g as u32);
        let x = BigUint::from_bytes_be(&tg_crypto::srp::compute_hash(&algo, password.as_bytes()));
        let v = g.modpow(&x, &p);
        let mut g_padded = [0u8; 256];
        g_padded[255] = algo.g as u8;
        let k = BigUint::from_bytes_be(&tg_crypto::sha256(&[&algo.p, &g_padded]));
        let b_exp = BigUint::from_bytes_be(&[0x42u8; 256]);
        let g_b = g.modpow(&b_exp, &p);
        let big_b = ((&k * &v) + &g_b) % &p;
        let mut b_bytes = big_b.to_bytes_be();
        if b_bytes.len() < 256 {
            let mut pad = vec![0u8; 256 - b_bytes.len()];
            pad.extend_from_slice(&b_bytes);
            b_bytes = pad;
        }
        (Some(b_bytes), Some(123_456_789i64), Some(tl_algo.clone()))
    } else {
        (None, None, None)
    };

    let tl_pwd = tl_enums::account::Password::Password(tl_types::account::Password {
        has_recovery,
        has_secure_values,
        has_password,
        current_algo,
        srp_b,
        srp_id,
        hint: hint.map(str::to_owned),
        email_unconfirmed_pattern: unconfirmed_email.map(str::to_owned),
        new_algo: tl_algo,
        new_secure_algo: tl_enums::SecurePasswordKdfAlgo::PBKDF2HMACSHA512iter100000(
            tl_types::SecurePasswordKdfAlgoPBKDF2HMACSHA512iter100000 {
                salt: b"sec_salt_0123456789".to_vec(),
            },
        ),
        secure_random: vec![0xAB; 32],
        pending_reset_date: None,
        login_email_pattern: None,
    });
    (tl_pwd, algo)
}

#[test]
fn live_session_passport_rpcs() {
    use tg_passport::controller::value_map_from_pairs;
    use tg_passport::encryption::{
        create_new_master_secret, encrypt_data_with_secret, encrypt_value_secret, serialize_data,
    };
    use tg_passport::types::{FormRequest, ValueType};

    let mut h = LiveTestHarness::start();
    let cloud_pwd = "PassportPass99!";
    let bot_uid = DataUserId::from_tl(720_001);
    let rsa_pem = tg_crypto::rsa::PRODUCTION_PUBLIC_KEYS[0];

    let (master_secret, secret_id, encrypted_secret, _sec_hash, prepared_sec_algo) =
        create_new_master_secret(
            tg_crypto::srp::SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 {
                salt: b"sec_salt_initial".to_vec(),
            },
            cloud_pwd.as_bytes(),
        )
        .expect("create_new_master_secret");

    // Pre-encrypt an existing PersonalDetails value under `master_secret`.
    let mut initial_fields = std::collections::BTreeMap::new();
    initial_fields.insert("first_name".to_owned(), "Alice".to_owned());
    initial_fields.insert("last_name".to_owned(), "Example".to_owned());
    initial_fields.insert("birth_date".to_owned(), "14.05.1995".to_owned());
    initial_fields.insert("gender".to_owned(), "female".to_owned());
    initial_fields.insert("country_code".to_owned(), "US".to_owned());
    initial_fields.insert("residence_country_code".to_owned(), "US".to_owned());
    let json_bytes = serialize_data(&initial_fields).expect("serialize");
    let value_secret = tg_crypto::srp::generate_secret_bytes();
    let enc_val = encrypt_data_with_secret(&json_bytes, &value_secret).expect("encrypt data");
    let enc_val_secret =
        encrypt_value_secret(&value_secret, &master_secret, &enc_val.hash).expect("encrypt secret");
    let stored_secure_value = tl_enums::SecureValue::SecureValue(tl_types::SecureValue {
        r#type: tl_enums::SecureValueType::PersonalDetails,
        data: Some(tl_enums::SecureData::SecureData(tl_types::SecureData {
            data: enc_val.bytes,
            data_hash: enc_val.hash.to_vec(),
            secret: enc_val_secret.to_vec(),
        })),
        front_side: None,
        reverse_side: None,
        selfie: None,
        translation: None,
        files: None,
        plain_data: None,
        hash: enc_val.hash.to_vec(),
    });

    h.live.configure_passport_request(FormRequest::new(
        bot_uid,
        "id",
        "https://example.test/passport-cb",
        rsa_pem,
        "nonce_synth_42",
    ));

    let (tl_pwd, _) =
        build_synthetic_srp_password_tl(cloud_pwd, true, true, true, Some("my hint"), None);

    // `load_authorization_form` requests `help.getPassportConfig` first (WP-55).
    h.fake.enqueue_ok(
        tl_fns::help::GetPassportConfig::CONSTRUCTOR_ID,
        &tl_enums::help::PassportConfig::NotModified,
    );
    h.fake.enqueue_ok(
        tl_fns::account::GetAuthorizationForm::CONSTRUCTOR_ID,
        &tl_enums::account::AuthorizationForm::AuthorizationForm(
            tl_types::account::AuthorizationForm {
                required_types: vec![tl_enums::SecureRequiredType::SecureRequiredType(
                    tl_types::SecureRequiredType {
                        native_names: false,
                        selfie_required: false,
                        translation_required: false,
                        r#type: tl_enums::SecureValueType::PersonalDetails,
                    },
                )],
                values: vec![stored_secure_value.clone()],
                errors: Vec::new(),
                users: Vec::new(),
                privacy_policy_url: Some("https://example.test/privacy".to_owned()),
            },
        ),
    );
    h.fake
        .enqueue_ok(tl_fns::account::GetPassword::CONSTRUCTOR_ID, &tl_pwd);

    let session_bundle = h
        .live
        .load_authorization_form("passport_bot")
        .expect("load_authorization_form");
    assert!(session_bundle.has_cloud_password);
    assert_eq!(session_bundle.password_hint, "my hint");
    assert!(session_bundle.has_secure_values);

    // `account.getAllSecureValues`
    h.fake.enqueue_ok(
        tl_fns::account::GetAllSecureValues::CONSTRUCTOR_ID,
        &vec![stored_secure_value.clone()],
    );
    let all_vals = h
        .live
        .load_all_secure_values()
        .expect("load_all_secure_values");
    assert_eq!(all_vals.len(), 1);
    assert_eq!(all_vals[0].value_type, ValueType::PersonalDetails);

    // Unlock via SRP (`account.getPasswordSettings`) and verify decryption of PersonalDetails.
    h.fake.enqueue_ok(
        tl_fns::account::GetPasswordSettings::CONSTRUCTOR_ID,
        &tl_enums::account::PasswordSettings::PasswordSettings(
            tl_types::account::PasswordSettings {
                email: Some("alice@example.test".to_owned()),
                secure_settings: Some(tl_enums::SecureSecretSettings::SecureSecretSettings(
                    tl_types::SecureSecretSettings {
                        secure_algo: tg_passport::controller::secure_secret_algo_to_tl(
                            &prepared_sec_algo,
                        ),
                        secure_secret: encrypted_secret.to_vec(),
                        secure_secret_id: secret_id as i64,
                    },
                )),
            },
        ),
    );
    let unlocked = PassportBackend::verify_cloud_password(&mut h.live, cloud_pwd)
        .expect("verify_cloud_password");
    let decrypted_details = unlocked
        .form
        .values
        .get(&ValueType::PersonalDetails)
        .expect("PersonalDetails present");
    assert_eq!(decrypted_details.data.parsed.text("first_name"), "Alice");
    assert_eq!(decrypted_details.data.parsed.text("last_name"), "Example");

    // Save an updated encrypted value (`account.saveSecureValue`) and echo it back dynamically.
    let saved_req_seen = Arc::new(Mutex::new(false));
    let seen_flag = Arc::clone(&saved_req_seen);
    let accept_req_seen = Arc::new(Mutex::new(false));
    let accept_flag = Arc::clone(&accept_req_seen);
    h.fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
        if ctor == tl_fns::account::SaveSecureValue::CONSTRUCTOR_ID {
            let req = tl_fns::account::SaveSecureValue::from_bytes(raw).expect("decode save");
            let tl_enums::InputSecureValue::InputSecureValue(iv) = req.value;
            let data = iv.data.expect("encrypted SecureData");
            let hash = match &data {
                tl_enums::SecureData::SecureData(sd) => sd.data_hash.clone(),
            };
            assert!(!hash.is_empty());
            *seen_flag.lock().expect("lock") = true;
            let echoed = tl_enums::SecureValue::SecureValue(tl_types::SecureValue {
                r#type: iv.r#type,
                data: Some(data),
                front_side: None,
                reverse_side: None,
                selfie: None,
                translation: None,
                files: None,
                plain_data: None,
                hash,
            });
            return Some(Ok(echoed.to_bytes()));
        }
        if ctor == tl_fns::account::AcceptAuthorization::CONSTRUCTOR_ID {
            let req = tl_fns::account::AcceptAuthorization::from_bytes(raw).expect("decode accept");
            assert_eq!(req.bot_id, 720_001);
            assert_eq!(req.value_hashes.len(), 1);
            let tl_enums::SecureCredentialsEncrypted::SecureCredentialsEncrypted(creds) =
                req.credentials;
            assert_eq!(creds.secret.len(), 256, "RSA-2048 OAEP encrypted secret");
            assert!(!creds.data.is_empty());
            *accept_flag.lock().expect("lock") = true;
            return Some(Ok(true.to_bytes()));
        }
        None
    }));

    let updated_form = h
        .live
        .save_passport_value(
            ValueType::PersonalDetails,
            value_map_from_pairs(&[("first_name", "Alicia")]),
            Vec::new(),
        )
        .expect("save_passport_value");
    assert!(*saved_req_seen.lock().expect("lock"));
    assert_eq!(
        updated_form
            .values
            .get(&ValueType::PersonalDetails)
            .expect("details")
            .data
            .parsed
            .text("first_name"),
        "Alicia"
    );

    let accept_cb = h.live.accept_authorization().expect("accept_authorization");
    assert!(*accept_req_seen.lock().expect("lock"));
    assert!(accept_cb.contains("tg_passport=success"));

    h.shutdown();
}

// =============================================================================
// Test 12: InfoBackend admin mutations + SettingsBackend 2FA SRP & tg-export
// =============================================================================

#[test]
fn live_session_admin_2fa_and_export_rpcs() {
    use tg_app::info::backend::{CreateChannelRequest, CreateGroupRequest, UpdatePeerInfoRequest};
    use tg_app::settings::backend::{ExportFormat, ExportRequest};
    use tg_data::rights::{ChatAdminRights, ChatBannedRights};

    let mut h = LiveTestHarness::start();
    let empty_updates = tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_750_000_300,
        seq: 0,
    });

    // 1. Create basic group (`messages.createChat`).
    let created_chat = tl_enums::Chat::Chat(tl_types::Chat {
        creator: true,
        left: false,
        deactivated: false,
        call_active: false,
        call_not_empty: false,
        noforwards: false,
        id: 910_001,
        title: "Rust Guild".to_owned(),
        photo: tl_enums::ChatPhoto::Empty,
        participants_count: 2,
        date: 1_750_000_300,
        version: 1,
        migrated_to: None,
        admin_rights: None,
        default_banned_rights: None,
    });
    h.fake.enqueue_ok(
        tl_fns::messages::CreateChat::CONSTRUCTOR_ID,
        &tl_enums::messages::InvitedUsers::InvitedUsers(tl_types::messages::InvitedUsers {
            updates: tl_enums::Updates::Updates(tl_types::Updates {
                updates: Vec::new(),
                users: Vec::new(),
                chats: vec![created_chat],
                date: 1_750_000_300,
                seq: 0,
            }),
            missing_invitees: Vec::new(),
        }),
    );
    let group_peer = h
        .live
        .create_group(CreateGroupRequest {
            title: "Rust Guild".to_owned(),
            member_ids: vec![h.bob_peer],
            auto_delete_ttl_seconds: 86_400,
            has_custom_photo: false,
        })
        .expect("create_group");
    assert_eq!(group_peer, PeerId::from_chat(910_001));

    // 2. Create broadcast channel (`channels.createChannel` + `updateUsername` + `toggleNoForwards`).
    let created_ch = synthetic_tl_channel(920_001, "Dev News", None, true);
    let discussion_ch = synthetic_tl_channel(920_002, "Dev Chat", None, false);
    h.fake.enqueue_ok(
        tl_fns::channels::CreateChannel::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: vec![
                tl_enums::Chat::Channel(created_ch),
                tl_enums::Chat::Channel(discussion_ch),
            ],
            date: 1_750_000_305,
            seq: 0,
        }),
    );
    h.fake
        .enqueue_ok(tl_fns::channels::UpdateUsername::CONSTRUCTOR_ID, &true);
    h.fake.enqueue_ok(
        tl_fns::messages::ToggleNoForwards::CONSTRUCTOR_ID,
        &empty_updates,
    );
    let channel_peer = h
        .live
        .create_channel(CreateChannelRequest {
            title: "Dev News".to_owned(),
            description: "Daily releases".to_owned(),
            is_public: true,
            username: Some("dev_news_test".to_owned()),
            no_forwards: true,
            has_custom_photo: false,
        })
        .expect("create_channel");
    assert_eq!(channel_peer, PeerId::from_channel(920_001));
    let discussion_peer = PeerId::from_channel(920_002);

    // 3. Edit title/about/username/photo (`channels.editTitle`, `messages.editChatAbout`, `channels.updateUsername`, `channels.editPhoto`).
    h.fake
        .enqueue_ok(tl_fns::channels::EditTitle::CONSTRUCTOR_ID, &empty_updates);
    h.fake
        .enqueue_ok(tl_fns::messages::EditChatAbout::CONSTRUCTOR_ID, &true);
    h.fake
        .enqueue_ok(tl_fns::channels::UpdateUsername::CONSTRUCTOR_ID, &true);
    h.live
        .update_peer_info(
            channel_peer,
            UpdatePeerInfoRequest {
                title: "Dev News Weekly".to_owned(),
                description: "Weekly digests".to_owned(),
                is_public: true,
                username: Some("dev_news_weekly".to_owned()),
                no_forwards: false,
            },
        )
        .expect("update_peer_info");

    h.fake
        .enqueue_ok(tl_fns::channels::EditPhoto::CONSTRUCTOR_ID, &empty_updates);
    h.live
        .edit_chat_photo(channel_peer, tl_enums::InputChatPhoto::Empty)
        .expect("edit_chat_photo");

    // 4. Default permissions + slow mode (`messages.editChatDefaultBannedRights` + `channels.toggleSlowMode`).
    h.fake.enqueue_ok(
        tl_fns::messages::EditChatDefaultBannedRights::CONSTRUCTOR_ID,
        &empty_updates,
    );
    h.fake.enqueue_ok(
        tl_fns::channels::ToggleSlowMode::CONSTRUCTOR_ID,
        &empty_updates,
    );
    h.live
        .update_default_permissions(
            discussion_peer,
            ChatBannedRights::SEND_POLLS | ChatBannedRights::SEND_MEDIA,
            30,
        )
        .expect("update_default_permissions");

    // 5. Admin rights (`channels.editAdmin`), invite (`channels.inviteToChannel`), kick (`channels.editBanned`).
    h.fake
        .enqueue_ok(tl_fns::channels::EditAdmin::CONSTRUCTOR_ID, &empty_updates);
    h.live
        .update_admin_rights(
            discussion_peer,
            h.bob_peer,
            ChatAdminRights::DELETE_MESSAGES | ChatAdminRights::BAN_USERS,
            "Moderator".to_owned(),
        )
        .expect("update_admin_rights");

    h.fake.enqueue_ok(
        tl_fns::channels::InviteToChannel::CONSTRUCTOR_ID,
        &tl_enums::messages::InvitedUsers::InvitedUsers(tl_types::messages::InvitedUsers {
            updates: empty_updates.clone(),
            missing_invitees: Vec::new(),
        }),
    );
    let added = h
        .live
        .add_members(discussion_peer, &[h.bob_peer])
        .expect("add_members");
    assert_eq!(added, 1);

    h.fake
        .enqueue_ok(tl_fns::channels::EditBanned::CONSTRUCTOR_ID, &empty_updates);
    h.live
        .remove_member(discussion_peer, h.bob_peer)
        .expect("remove_member");

    // 6. Auto-delete TTL (`messages.setHistoryTTL`), discussion group (`channels.setDiscussionGroup`),
    //    channel username (`channels.updateUsername`).
    h.fake.enqueue_ok(
        tl_fns::messages::SetHistoryTTL::CONSTRUCTOR_ID,
        &empty_updates,
    );
    h.live
        .set_auto_delete_ttl(discussion_peer, 604_800)
        .expect("set_auto_delete_ttl");

    h.fake
        .enqueue_ok(tl_fns::channels::SetDiscussionGroup::CONSTRUCTOR_ID, &true);
    h.live
        .set_linked_discussion(channel_peer, Some(discussion_peer))
        .expect("set_linked_discussion");

    h.fake
        .enqueue_ok(tl_fns::channels::UpdateUsername::CONSTRUCTOR_ID, &true);
    InfoBackend::update_username(&mut h.live, channel_peer, "dev_news_final")
        .expect("update_username channel");

    // 7. SettingsBackend 2FA SRP flow: set (+ EMAIL_UNCONFIRMED_6) -> confirm email -> verify -> disable.
    let (pwd_none, _) = build_synthetic_srp_password_tl("", false, false, false, None, None);
    let (pwd_unconfirmed, _) = build_synthetic_srp_password_tl(
        "MyNewPass42!",
        true,
        false,
        false,
        Some("pet"),
        Some("a***@example.test"),
    );
    let (pwd_confirmed, _) =
        build_synthetic_srp_password_tl("MyNewPass42!", true, true, false, Some("pet"), None);

    let captured_update_pwd: Arc<Mutex<Vec<tl_fns::account::UpdatePasswordSettings>>> =
        Arc::new(Mutex::new(Vec::new()));
    let cap_pwd = Arc::clone(&captured_update_pwd);
    let update_call_idx = Arc::new(Mutex::new(0usize));
    let idx_ref = Arc::clone(&update_call_idx);
    h.fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
        if ctor == tl_fns::account::UpdatePasswordSettings::CONSTRUCTOR_ID {
            let req = tl_fns::account::UpdatePasswordSettings::from_bytes(raw)
                .expect("decode updatePasswordSettings");
            cap_pwd.lock().expect("lock").push(req);
            let mut n = idx_ref.lock().expect("lock");
            *n += 1;
            if *n == 1 {
                // First call sets password + recovery email -> Telegram returns EMAIL_UNCONFIRMED_6
                return Some(Err(InvocationError::Rpc(tg_api::RpcError {
                    code: 400,
                    name: "EMAIL_UNCONFIRMED_6".to_owned(),
                    value: Some(6),
                })));
            }
            return Some(Ok(true.to_bytes()));
        }
        None
    }));

    h.fake
        .enqueue_ok(tl_fns::account::GetPassword::CONSTRUCTOR_ID, &pwd_none);
    h.fake.enqueue_ok(
        tl_fns::account::GetPassword::CONSTRUCTOR_ID,
        &pwd_unconfirmed,
    );
    let set_state = h
        .live
        .set_cloud_password(None, "MyNewPass42!", "pet", Some("alice@example.test"))
        .expect("set_cloud_password");
    assert!(set_state.has_password);
    assert_eq!(set_state.unconfirmed_email_pattern, "a***@example.test");

    h.fake
        .enqueue_ok(tl_fns::account::ConfirmPasswordEmail::CONSTRUCTOR_ID, &true);
    h.fake
        .enqueue_ok(tl_fns::account::GetPassword::CONSTRUCTOR_ID, &pwd_confirmed);
    let confirmed_state = h
        .live
        .confirm_recovery_email("654321")
        .expect("confirm_recovery_email");
    assert!(confirmed_state.has_recovery);

    h.fake
        .enqueue_ok(tl_fns::account::GetPassword::CONSTRUCTOR_ID, &pwd_confirmed);
    h.fake.enqueue_ok(
        tl_fns::account::GetPasswordSettings::CONSTRUCTOR_ID,
        &tl_enums::account::PasswordSettings::PasswordSettings(
            tl_types::account::PasswordSettings {
                email: Some("alice@example.test".to_owned()),
                secure_settings: None,
            },
        ),
    );
    SettingsBackend::verify_cloud_password(&h.live, "MyNewPass42!").expect("verify_cloud_password");

    h.fake
        .enqueue_ok(tl_fns::account::GetPassword::CONSTRUCTOR_ID, &pwd_confirmed);
    h.fake
        .enqueue_ok(tl_fns::account::GetPassword::CONSTRUCTOR_ID, &pwd_none);
    let disabled_state = h
        .live
        .disable_cloud_password("MyNewPass42!")
        .expect("disable_cloud_password");
    assert!(!disabled_state.has_password);

    {
        let updates = captured_update_pwd.lock().expect("lock");
        assert_eq!(updates.len(), 2);
        // Call 1: setting new password -> 256-byte SRP verifier
        let tl_enums::account::PasswordInputSettings::PasswordInputSettings(ref s1) =
            updates[0].new_settings;
        assert_eq!(
            s1.new_password_hash.as_ref().map(Vec::len),
            Some(256),
            "SRP verifier must be 256 bytes"
        );
        // Call 2: disabling password -> SRP check present, new_algo Unknown, empty new_password_hash
        assert!(matches!(
            updates[1].password,
            tl_enums::InputCheckPasswordSRP::InputCheckPasswordSRP(_)
        ));
        let tl_enums::account::PasswordInputSettings::PasswordInputSettings(ref s2) =
            updates[1].new_settings;
        assert!(matches!(
            s2.new_algo,
            Some(tl_enums::PasswordKdfAlgo::Unknown)
        ));
        assert_eq!(s2.new_password_hash.as_deref(), Some(&[][..]));
    }

    // 8. Data export kickoff routed to `tg-export`'s `TlExportSource`.
    let export_dir = TempDir::new().expect("export tempdir");
    let self_user_for_export = synthetic_tl_user(h.self_uid, "Alice", "alice_self", true);
    h.fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
        // Unwrap `invokeWithTakeout` so inner export queries are handled cleanly.
        let inner_ctor =
            if ctor == tg_tl::functions::InvokeWithTakeout::<tg_tl::RawObject>::CONSTRUCTOR_ID {
                if raw.len() >= 16 {
                    u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]])
                } else {
                    0
                }
            } else {
                ctor
            };
        if inner_ctor == tl_fns::users::GetUsers::CONSTRUCTOR_ID {
            return Some(Ok(
                vec![tl_enums::User::User(self_user_for_export.clone())].to_bytes()
            ));
        }
        if inner_ctor == tl_fns::users::GetFullUser::CONSTRUCTOR_ID {
            return Some(Ok(tl_enums::users::UserFull::UserFull(
                tl_types::users::UserFull {
                    full_user: tl_enums::UserFull::UserFull(tl_types::UserFull {
                        blocked: false,
                        phone_calls_available: false,
                        phone_calls_private: false,
                        can_pin_message: true,
                        has_scheduled: false,
                        video_calls_available: false,
                        voice_messages_forbidden: false,
                        translations_disabled: false,
                        stories_pinned_available: false,
                        blocked_my_stories_from: false,
                        wallpaper_overridden: false,
                        contact_require_premium: false,
                        read_dates_private: false,
                        sponsored_enabled: false,
                        can_view_revenue: false,
                        bot_can_manage_emoji_status: false,
                        display_gifts_button: false,
                        noforwards_my_enabled: false,
                        noforwards_peer_enabled: false,
                        unofficial_security_risk: false,
                        id: self_user_for_export.id,
                        about: Some("Export bio".to_owned()),
                        settings: tl_enums::PeerSettings::PeerSettings(tl_types::PeerSettings {
                            report_spam: false,
                            add_contact: false,
                            block_contact: false,
                            share_contact: false,
                            need_contacts_exception: false,
                            report_geo: false,
                            autoarchived: false,
                            invite_members: false,
                            request_chat_broadcast: false,
                            business_bot_paused: false,
                            business_bot_can_reply: false,
                            geo_distance: None,
                            request_chat_title: None,
                            request_chat_date: None,
                            business_bot_id: None,
                            business_bot_manage_url: None,
                            charge_paid_message_stars: None,
                            registration_month: None,
                            phone_country: None,
                            name_change_date: None,
                            photo_change_date: None,
                        }),
                        personal_photo: None,
                        profile_photo: None,
                        fallback_photo: None,
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
                        bot_info: None,
                        pinned_msg_id: None,
                        common_chats_count: 0,
                        folder_id: None,
                        ttl_period: None,
                        theme: None,
                        private_forward_name: None,
                        bot_group_admin_rights: None,
                        bot_broadcast_admin_rights: None,
                        wallpaper: None,
                        stories: None,
                        business_work_hours: None,
                        business_location: None,
                        business_greeting_message: None,
                        business_away_message: None,
                        business_intro: None,
                        birthday: None,
                        personal_channel_id: None,
                        personal_channel_message: None,
                        stargifts_count: None,
                        starref_program: None,
                        bot_verification: None,
                        send_paid_messages_stars: None,
                        disallowed_gifts: None,
                        stars_rating: None,
                        stars_my_pending_rating: None,
                        stars_my_pending_rating_date: None,
                        main_tab: None,
                        saved_music: None,
                        note: None,
                        bot_manager_id: None,
                        wallet_card: None,
                        share_theme: None,
                    }),
                    chats: Vec::new(),
                    users: vec![tl_enums::User::User(self_user_for_export.clone())],
                },
            )
            .to_bytes()));
        }
        if inner_ctor == tl_fns::account::InitTakeoutSession::CONSTRUCTOR_ID {
            return Some(Ok(tl_enums::account::Takeout::Takeout(
                tl_types::account::Takeout { id: 777_888 },
            )
            .to_bytes()));
        }
        if inner_ctor == tl_fns::contacts::GetSaved::CONSTRUCTOR_ID {
            return Some(Ok(Vec::<tl_enums::SavedContact>::new().to_bytes()));
        }
        if inner_ctor == tl_fns::contacts::GetContacts::CONSTRUCTOR_ID {
            return Some(Ok(tl_enums::contacts::Contacts::Contacts(
                tl_types::contacts::Contacts {
                    contacts: Vec::new(),
                    saved_count: 0,
                    users: Vec::new(),
                },
            )
            .to_bytes()));
        }
        if inner_ctor == tl_fns::contacts::GetTopPeers::CONSTRUCTOR_ID {
            return Some(Ok(tl_enums::contacts::TopPeers::TopPeers(
                tl_types::contacts::TopPeers {
                    categories: Vec::new(),
                    chats: Vec::new(),
                    users: Vec::new(),
                },
            )
            .to_bytes()));
        }
        if inner_ctor == tl_fns::account::FinishTakeoutSession::CONSTRUCTOR_ID {
            return Some(Ok(true.to_bytes()));
        }
        None
    }));

    let status_msg = h.live.request_export(&ExportRequest {
        account_info: true,
        personal_chats: false,
        groups_and_channels: false,
        include_media: false,
        format: ExportFormat::Json,
        path: export_dir.path().to_string_lossy().into_owned(),
    });
    assert!(
        status_msg.contains("Data export started"),
        "unexpected export status: {status_msg}"
    );
    let handle = h.live.export_handle().expect("export_handle active");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if handle.current_state().is_stopped() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    match handle.current_state() {
        tg_export::ExportState::Finished(done) => {
            assert!(
                Path::new(&done.path).exists(),
                "expected exported file at {}",
                done.path
            );
        }
        other => panic!("expected ExportState::Finished, got {other:?}"),
    }

    let ctors = h.fake.recorded_ctors();
    for expected in [
        tl_fns::messages::CreateChat::CONSTRUCTOR_ID,
        tl_fns::channels::CreateChannel::CONSTRUCTOR_ID,
        tl_fns::channels::EditTitle::CONSTRUCTOR_ID,
        tl_fns::messages::EditChatAbout::CONSTRUCTOR_ID,
        tl_fns::channels::EditPhoto::CONSTRUCTOR_ID,
        tl_fns::messages::EditChatDefaultBannedRights::CONSTRUCTOR_ID,
        tl_fns::channels::ToggleSlowMode::CONSTRUCTOR_ID,
        tl_fns::channels::EditAdmin::CONSTRUCTOR_ID,
        tl_fns::channels::InviteToChannel::CONSTRUCTOR_ID,
        tl_fns::channels::EditBanned::CONSTRUCTOR_ID,
        tl_fns::messages::SetHistoryTTL::CONSTRUCTOR_ID,
        tl_fns::channels::SetDiscussionGroup::CONSTRUCTOR_ID,
        tl_fns::channels::UpdateUsername::CONSTRUCTOR_ID,
        tl_fns::account::UpdatePasswordSettings::CONSTRUCTOR_ID,
        tl_fns::account::ConfirmPasswordEmail::CONSTRUCTOR_ID,
        tl_fns::account::InitTakeoutSession::CONSTRUCTOR_ID,
    ] {
        assert!(
            ctors.contains(&expected),
            "missing expected admin/2FA/export TL ctor 0x{expected:08x}"
        );
    }

    h.shutdown();
}

// =============================================================================
// Test 13: Inline keyboard reply markup flows end-to-end from fake getHistory
// =============================================================================

#[test]
fn live_session_get_history_exposes_inline_reply_markup() {
    let h = LiveTestHarness::start();
    // Seed a bot peer with no cached history so load_slice_around must invoke messages.getHistory.
    let kb_bot_uid = 500_099i64;
    let kb_bot_user = synthetic_tl_bot(kb_bot_uid, "Keyboard", "kb_bot", false);
    let kb_bot_peer = PeerId::from_user(kb_bot_uid as u64);
    h.runtime.tokio_handle().block_on(async {
        let mut s = h.runtime.session().write().await;
        tg_api::convert::apply_tl_users_and_chats(
            &mut s,
            &[tl_enums::User::User(kb_bot_user.clone())],
            &[],
        );
    });

    let mut msg_with_markup = synthetic_tl_message(
        11,
        kb_bot_uid,
        kb_bot_uid,
        false,
        1_750_000_020,
        "Pick an option:",
    );
    if let tl_enums::Message::Message(ref mut m) = msg_with_markup {
        m.reply_markup = Some(tl_enums::ReplyMarkup::ReplyInlineMarkup(
            tl_types::ReplyInlineMarkup {
                force_reply: false,
                rows: vec![tl_enums::KeyboardInlineButtonRow::KeyboardInlineButtonRow(
                    tl_types::KeyboardInlineButtonRow {
                        buttons: vec![
                            tl_enums::KeyboardInlineButton::KeyboardInlineButton(
                                tl_types::KeyboardInlineButton {
                                    style: Some(
                                        tl_enums::KeyboardButtonStyle::KeyboardButtonStyle(
                                            tl_types::KeyboardButtonStyle {
                                                bg_primary: true,
                                                bg_danger: false,
                                                bg_success: false,
                                                icon: Some(555),
                                            },
                                        ),
                                    ),
                                    text: "Click Me".to_owned(),
                                    r#type: tl_enums::InlineButtonType::Callback(
                                        tl_types::InlineButtonTypeCallback {
                                            requires_password: false,
                                            data: b"act_1".to_vec(),
                                        },
                                    ),
                                },
                            ),
                            tl_enums::KeyboardInlineButton::KeyboardInlineButton(
                                tl_types::KeyboardInlineButton {
                                    style: None,
                                    text: "Open App".to_owned(),
                                    r#type: tl_enums::InlineButtonType::WebView(
                                        tl_types::InlineButtonTypeWebView {
                                            url: "https://app.example.org".to_owned(),
                                        },
                                    ),
                                },
                            ),
                        ],
                    },
                )],
            },
        ));
    }

    h.fake.enqueue_ok(
        tl_fns::messages::GetHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![msg_with_markup],
            topics: Vec::new(),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(kb_bot_user)],
        }),
    );

    let history = h
        .live
        .load_slice_around(kb_bot_peer, MsgId::SHOW_AT_END, 20, 20);
    assert!(
        h.fake
            .recorded_ctors()
            .contains(&tl_fns::messages::GetHistory::CONSTRUCTOR_ID),
        "expected messages.getHistory to be sent"
    );
    let msg = history
        .messages
        .iter()
        .find(|m| m.id == MsgId(11))
        .expect("message #11 in history slice");
    let markup = msg
        .extra
        .reply_markup
        .as_ref()
        .expect("reply_markup must be populated on HistoryMessage");
    assert!(markup.is_inline());
    assert_eq!(markup.rows.len(), 1);
    assert_eq!(markup.rows[0].len(), 2);
    assert_eq!(markup.rows[0][0].text, "Click Me");
    assert_eq!(
        markup.rows[0][0].visual.color,
        tg_bots::markup::ButtonColor::Primary
    );
    assert_eq!(
        markup.rows[0][0].visual.icon_id,
        tg_bots::stand_in::DocumentId(555)
    );
    assert!(matches!(
        &markup.rows[0][0].kind,
        tg_bots::markup::ButtonKind::Callback { data } if data == b"act_1"
    ));
    assert!(matches!(
        &markup.rows[0][1].kind,
        tg_bots::markup::ButtonKind::WebView { url } if url == "https://app.example.org"
    ));

    h.shutdown();
}

// =============================================================================
// Test 14: BotsBackend over FakeRpcClient (callbacks + SRP, URL auth, inline,
//          bot commands, menu button, attach menu, game scores, request peer)
// =============================================================================

fn synthetic_tl_bot(
    id: i64,
    first_name: &str,
    username: &str,
    has_main_app: bool,
) -> tl_types::User {
    let mut u = synthetic_tl_user(id, first_name, username, false);
    u.bot = true;
    u.bot_attach_menu = true;
    u.bot_has_main_app = has_main_app;
    u.bot_inline_placeholder = Some("Search catalog...".to_owned());
    u
}

#[test]
fn live_session_bots_backend_all_rpcs() {
    use tg_app::bots::backend::BotsBackend;
    use tg_bots::callback::{AcceptUrlAuthParams, CallbackQueryRequest, UrlAuthSource};
    use tg_bots::commands::BotMenuButton;
    use tg_bots::inline::GetInlineBotResultsRequest;
    use tg_bots::mini_app::{AttachMenuToggleState, SetGameScoreRequest};
    use tg_bots::stand_in::{
        FullMsgId as BotFullMsgId, MsgId as BotMsgId, PeerId as BotPeerId, UserId as BotUserId,
    };

    let mut h = LiveTestHarness::start();
    let bot_uid = 700_001i64;
    let bot_tl = synthetic_tl_bot(bot_uid, "Catalog Bot", "catalog_bot", true);
    let bot_peer_data = DataPeerId::from_user(DataUserId::from_tl(bot_uid));
    let bot_peer = BotPeerId(bot_peer_data.0);
    let bot_id = BotUserId(bot_uid as u64);

    // Seed bot user into session.
    h.runtime.tokio_handle().block_on(async {
        let mut s = h.runtime.session().write().await;
        tg_api::convert::apply_tl_users_and_chats(
            &mut s,
            &[tl_enums::User::User(bot_tl.clone())],
            &[],
        );
    });
    h.live.set_active_bot_peer(bot_peer);

    // 1. Bot commands & menu button from UserFull.bot_info via users.getFullUser
    let full_user_resp = tl_enums::users::UserFull::UserFull(tl_types::users::UserFull {
        full_user: tl_enums::UserFull::UserFull(tl_types::UserFull {
            blocked: false,
            phone_calls_available: false,
            phone_calls_private: false,
            can_pin_message: true,
            has_scheduled: false,
            video_calls_available: false,
            voice_messages_forbidden: false,
            translations_disabled: false,
            stories_pinned_available: false,
            blocked_my_stories_from: false,
            wallpaper_overridden: false,
            contact_require_premium: false,
            read_dates_private: false,
            sponsored_enabled: false,
            can_view_revenue: false,
            bot_can_manage_emoji_status: false,
            display_gifts_button: false,
            noforwards_my_enabled: false,
            noforwards_peer_enabled: false,
            unofficial_security_risk: false,
            id: bot_uid,
            about: Some("Test catalog bot".to_owned()),
            settings: tl_enums::PeerSettings::PeerSettings(tl_types::PeerSettings {
                report_spam: false,
                add_contact: false,
                block_contact: false,
                share_contact: false,
                need_contacts_exception: false,
                report_geo: false,
                autoarchived: false,
                invite_members: false,
                request_chat_broadcast: false,
                business_bot_paused: false,
                business_bot_can_reply: false,
                geo_distance: None,
                request_chat_title: None,
                request_chat_date: None,
                business_bot_id: None,
                business_bot_manage_url: None,
                charge_paid_message_stars: None,
                registration_month: None,
                phone_country: None,
                name_change_date: None,
                photo_change_date: None,
            }),
            personal_photo: None,
            profile_photo: None,
            fallback_photo: None,
            notify_settings: tl_enums::PeerNotifySettings::PeerNotifySettings(
                tl_types::PeerNotifySettings {
                    show_previews: None,
                    silent: None,
                    mute_until: None,
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
            bot_info: Some(tl_enums::BotInfo::BotInfo(tl_types::BotInfo {
                has_preview_medias: false,
                user_id: Some(bot_uid),
                description: Some("Browse items".to_owned()),
                description_photo: None,
                description_document: None,
                commands: Some(vec![
                    tl_enums::BotCommand::BotCommand(tl_types::BotCommand {
                        ephemeral: false,
                        command: "start".to_owned(),
                        description: "Start bot".to_owned(),
                    }),
                    tl_enums::BotCommand::BotCommand(tl_types::BotCommand {
                        ephemeral: false,
                        command: "catalog".to_owned(),
                        description: "Open catalog".to_owned(),
                    }),
                ]),
                menu_button: Some(tl_enums::BotMenuButton::BotMenuButton(
                    tl_types::BotMenuButton {
                        text: "Shop".to_owned(),
                        url: "https://shop.example.org".to_owned(),
                    },
                )),
                privacy_policy_url: Some("https://shop.example.org/privacy".to_owned()),
                app_settings: None,
                verifier_settings: None,
            })),
            pinned_msg_id: None,
            common_chats_count: 0,
            folder_id: None,
            ttl_period: None,
            theme: None,
            private_forward_name: None,
            bot_group_admin_rights: None,
            bot_broadcast_admin_rights: None,
            wallpaper: None,
            stories: None,
            business_work_hours: None,
            business_location: None,
            business_greeting_message: None,
            business_away_message: None,
            business_intro: None,
            birthday: None,
            personal_channel_id: None,
            personal_channel_message: None,
            stargifts_count: None,
            starref_program: None,
            bot_verification: None,
            send_paid_messages_stars: None,
            disallowed_gifts: None,
            stars_rating: None,
            stars_my_pending_rating: None,
            stars_my_pending_rating_date: None,
            main_tab: None,
            saved_music: None,
            note: None,
            bot_manager_id: None,
            wallet_card: None,
            share_theme: None,
        }),
        chats: Vec::new(),
        users: vec![tl_enums::User::User(bot_tl.clone())],
    });
    h.fake
        .enqueue_ok(tl_fns::users::GetFullUser::CONSTRUCTOR_ID, &full_user_resp);

    let cmds = h.live.commands_for_peer(bot_peer);
    assert_eq!(cmds.len(), 2);
    assert_eq!(cmds[0].command, "start");
    assert_eq!(cmds[1].command, "catalog");

    let snap = h
        .live
        .load_bot_chat_for_peer(bot_peer)
        .expect("bot snapshot");
    assert_eq!(
        snap.bot_info.menu_button,
        BotMenuButton::WebApp {
            text: "Shop".to_owned(),
            url: "https://shop.example.org".to_owned(),
        }
    );

    // 2. bots.getBotMenuButton
    h.fake.enqueue_ok(
        tl_fns::bots::GetBotMenuButton::CONSTRUCTOR_ID,
        &tl_enums::BotMenuButton::Commands,
    );
    let btn = h
        .live
        .fetch_bot_menu_button(bot_id)
        .expect("fetch_bot_menu_button");
    assert_eq!(btn, BotMenuButton::Commands);

    // 3. Regular callback query + SRP password-protected callback query
    h.fake.enqueue_ok(
        tl_fns::messages::GetBotCallbackAnswer::CONSTRUCTOR_ID,
        &tl_enums::messages::BotCallbackAnswer::BotCallbackAnswer(
            tl_types::messages::BotCallbackAnswer {
                alert: true,
                has_url: false,
                native_ui: false,
                message: Some("Confirmed!".to_owned()),
                url: None,
                cache_time: 10,
            },
        ),
    );
    let cb_ans = h
        .live
        .send_callback_query(&CallbackQueryRequest {
            peer_id: bot_peer,
            msg_id: BotMsgId(101),
            is_game: false,
            data: Some(b"ok".to_vec()),
            requires_password: false,
            ephemeral_id: None,
        })
        .expect("regular callback");
    assert!(cb_ans.alert);
    assert_eq!(cb_ans.message.as_deref(), Some("Confirmed!"));

    // Password-protected callback via account.getPassword + SRP + getBotCallbackAnswer
    let p_bytes = tg_crypto::GOOD_PRIME.to_vec();
    let algo = tg_crypto::srp::PasswordKdfAlgoModPow {
        salt1: b"salt1_test".to_vec(),
        salt2: b"salt2_test".to_vec(),
        g: 3,
        p: p_bytes.clone(),
    };
    let password = "super-secret-2fa-password";
    let digest = tg_crypto::srp::compute_digest(&algo, password.as_bytes()).expect("srp digest");
    // Synthesize valid server B = (k*v + g^b) mod p with a 256-byte exponent b so g^b passes IsGoodModExpFirst.
    let p_big = num_bigint::BigUint::from_bytes_be(&p_bytes);
    let g_big = num_bigint::BigUint::from(3u32);
    let mut g_pad = [0u8; 256];
    g_pad[255] = 3;
    let k_big = num_bigint::BigUint::from_bytes_be(&tg_crypto::sha256(&[&p_bytes, &g_pad]));
    let v_big = num_bigint::BigUint::from_bytes_be(&digest);
    let b_secret = num_bigint::BigUint::from_bytes_be(&[0x42u8; 256]);
    let gb_big = g_big.modpow(&b_secret, &p_big);
    let b_val = (&k_big * &v_big + &gb_big) % &p_big;
    let b_raw = b_val.to_bytes_be();
    let mut srp_b = vec![0u8; 256];
    srp_b[256 - b_raw.len()..].copy_from_slice(&b_raw);

    let captured_srp = Arc::new(Mutex::new(None::<tl_types::InputCheckPasswordSRP>));
    let captured_srp_clone = Arc::clone(&captured_srp);
    h.fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
        if ctor == tl_fns::messages::GetBotCallbackAnswer::CONSTRUCTOR_ID {
            let req = tl_fns::messages::GetBotCallbackAnswer::from_bytes(raw)
                .expect("decode GetBotCallbackAnswer");
            if let Some(tl_enums::InputCheckPasswordSRP::InputCheckPasswordSRP(srp)) = req.password
            {
                *captured_srp_clone.lock().expect("lock srp") = Some(srp);
                let ans = tl_enums::messages::BotCallbackAnswer::BotCallbackAnswer(
                    tl_types::messages::BotCallbackAnswer {
                        alert: false,
                        has_url: false,
                        native_ui: false,
                        message: Some("2FA approved".to_owned()),
                        url: None,
                        cache_time: 0,
                    },
                );
                return Some(Ok(ans.to_bytes()));
            }
        }
        None
    }));
    h.fake.enqueue_ok(
        tl_fns::account::GetPassword::CONSTRUCTOR_ID,
        &tl_enums::account::Password::Password(tl_types::account::Password {
            has_recovery: false,
            has_secure_values: false,
            has_password: true,
            current_algo: Some(
                tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(
                    tl_types::PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow {
                        salt1: algo.salt1.clone(),
                        salt2: algo.salt2.clone(),
                        g: algo.g,
                        p: algo.p.clone(),
                    },
                ),
            ),
            srp_b: Some(srp_b),
            srp_id: Some(987_654),
            hint: Some("hint".to_owned()),
            email_unconfirmed_pattern: None,
            new_algo: tl_enums::PasswordKdfAlgo::Unknown,
            new_secure_algo: tl_enums::SecurePasswordKdfAlgo::Unknown,
            secure_random: vec![1, 2, 3, 4],
            pending_reset_date: None,
            login_email_pattern: None,
        }),
    );

    let srp_ans = h
        .live
        .send_callback_query_with_password(
            &CallbackQueryRequest {
                peer_id: bot_peer,
                msg_id: BotMsgId(102),
                is_game: false,
                data: Some(b"payout".to_vec()),
                requires_password: true,
                ephemeral_id: None,
            },
            password,
        )
        .expect("SRP callback");
    assert_eq!(srp_ans.message.as_deref(), Some("2FA approved"));
    let sent_srp = captured_srp
        .lock()
        .expect("lock")
        .clone()
        .expect("InputCheckPasswordSRP must be transmitted");
    assert_eq!(sent_srp.srp_id, 987_654);
    assert_eq!(sent_srp.a.len(), 256);
    assert_eq!(sent_srp.m1.len(), 32);
    // Clear dynamic handler for subsequent tests.
    h.fake.set_dynamic_handler(Arc::new(|_, _| None));

    // 4. URL Auth: requestUrlAuth, acceptUrlAuth, declineUrlAuth
    h.fake.enqueue_ok(
        tl_fns::messages::RequestUrlAuth::CONSTRUCTOR_ID,
        &tl_enums::UrlAuthResult::Request(tl_types::UrlAuthResultRequest {
            request_write_access: true,
            request_phone_number: false,
            match_codes_first: false,
            is_app: false,
            request_wallet: false,
            bot: tl_enums::User::User(bot_tl.clone()),
            domain: "portal.example.org".to_owned(),
            browser: Some("Rustgram".to_owned()),
            platform: Some("Windows".to_owned()),
            ip: Some("127.0.0.1".to_owned()),
            region: Some("Local".to_owned()),
            match_codes: Some(vec!["A1B".to_owned()]),
            user_id_hint: None,
            verified_app_name: None,
            tc_session_id: None,
            tc_client_id: None,
        }),
    );
    let auth_src = UrlAuthSource::Button {
        peer_id: bot_peer,
        msg_id: BotMsgId(105),
        button_id: 9,
        url: "https://portal.example.org/login".to_owned(),
    };
    let req_res = h
        .live
        .request_url_auth(&auth_src)
        .expect("request_url_auth");
    assert!(matches!(
        req_res,
        tg_bots::callback::UrlAuthResultInput::Request(ref r) if r.domain == "portal.example.org"
    ));

    h.fake.enqueue_ok(
        tl_fns::messages::AcceptUrlAuth::CONSTRUCTOR_ID,
        &tl_enums::UrlAuthResult::Accepted(tl_types::UrlAuthResultAccepted {
            url: Some("https://portal.example.org/login?tgAuthResult=ok".to_owned()),
        }),
    );
    let acc_res = h
        .live
        .accept_url_auth(&AcceptUrlAuthParams {
            source: auth_src,
            write_allowed: true,
            share_phone_number: false,
            match_code: Some("A1B".to_owned()),
        })
        .expect("accept_url_auth");
    assert!(matches!(
        acc_res,
        tg_bots::callback::UrlAuthResultInput::Accepted { url: Some(ref u) } if u.contains("tgAuthResult=ok")
    ));

    h.fake
        .enqueue_ok(tl_fns::messages::DeclineUrlAuth::CONSTRUCTOR_ID, &true);
    h.live
        .decline_url_auth("https://portal.example.org/login")
        .expect("decline_url_auth");

    // 5. Inline bot results with next_offset + sendInlineBotResult
    h.fake.enqueue_ok(
        tl_fns::messages::GetInlineBotResults::CONSTRUCTOR_ID,
        &tl_enums::messages::BotResults::BotResults(tl_types::messages::BotResults {
            gallery: false,
            query_id: 42_000,
            next_offset: Some("cursor_2".to_owned()),
            switch_pm: None,
            switch_webview: None,
            results: vec![tl_enums::BotInlineResult::BotInlineResult(
                tl_types::BotInlineResult {
                    id: "item_1".to_owned(),
                    r#type: "article".to_owned(),
                    title: Some("Widget Pro".to_owned()),
                    description: Some("First page result".to_owned()),
                    url: Some("https://shop.example.org/1".to_owned()),
                    thumb: None,
                    content: None,
                    send_message: tl_enums::BotInlineMessage::Text(
                        tl_types::BotInlineMessageText {
                            no_webpage: false,
                            invert_media: false,
                            message: "Widget Pro".to_owned(),
                            entities: None,
                            reply_markup: None,
                        },
                    ),
                },
            )],
            cache_time: 30,
            users: Vec::new(),
        }),
    );
    let page = h
        .live
        .query_inline_results(&GetInlineBotResultsRequest {
            bot_id,
            peer_id: bot_peer,
            query: "widget".to_owned(),
            offset: String::new(),
        })
        .expect("query_inline_results");
    assert_eq!(page.next_offset.as_deref(), Some("cursor_2"));
    assert_eq!(page.results.len(), 1);
    assert_eq!(page.results[0].id, "item_1");

    let inline_sent_tl =
        synthetic_tl_message(201, bot_uid, h.self_uid, true, 1_750_000_040, "Widget Pro");
    h.fake.enqueue_ok(
        tl_fns::messages::SendInlineBotResult::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                message: inline_sent_tl,
                pts: 110,
                pts_count: 1,
            })],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_750_000_040,
            seq: 5,
        }),
    );
    let sent_inline = h
        .live
        .send_inline_result(bot_id, bot_peer, &page.results[0])
        .expect("send_inline_result");
    assert_eq!(sent_inline.id.msg, BotMsgId(201));

    // 6. Attach menu bots: getAttachMenuBots + toggleBotInAttachMenu
    h.fake.enqueue_ok(
        tl_fns::messages::GetAttachMenuBots::CONSTRUCTOR_ID,
        &tl_enums::AttachMenuBots::AttachMenuBots(tl_types::AttachMenuBots {
            hash: 1234,
            bots: vec![tl_enums::AttachMenuBot::AttachMenuBot(
                tl_types::AttachMenuBot {
                    inactive: false,
                    has_settings: true,
                    request_write_access: true,
                    show_in_attach_menu: true,
                    show_in_side_menu: true,
                    side_menu_disclaimer_needed: false,
                    bot_id: bot_uid,
                    short_name: "Shop".to_owned(),
                    peer_types: Some(vec![tl_enums::AttachMenuPeerType::PM]),
                    icons: Vec::new(),
                },
            )],
            users: vec![tl_enums::User::User(bot_tl.clone())],
        }),
    );
    let store = h.live.load_attach_menu();
    assert_eq!(store.bots_hash, 1234);
    assert_eq!(store.bots.len(), 1);
    assert_eq!(store.bots[0].short_name, "Shop");

    h.fake.enqueue_ok(
        tl_fns::messages::ToggleBotInAttachMenu::CONSTRUCTOR_ID,
        &true,
    );
    let updated_store = h
        .live
        .toggle_attach_menu_bot(bot_id, AttachMenuToggleState::Removed)
        .expect("toggle_attach_menu_bot");
    assert!(updated_store.bots[0].inactive);

    // 7. Game scores: setGameScore + getGameHighScores
    h.fake.enqueue_ok(
        tl_fns::messages::SetGameScore::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_750_000_045,
            seq: 6,
        }),
    );
    h.live
        .set_game_score(&SetGameScoreRequest {
            edit_message: true,
            force: false,
            peer_id: bot_peer,
            msg_id: BotMsgId(300),
            user_id: BotUserId(h.self_uid as u64),
            score: 4200,
        })
        .expect("set_game_score");

    h.fake.enqueue_ok(
        tl_fns::messages::GetGameHighScores::CONSTRUCTOR_ID,
        &tl_enums::messages::HighScores::HighScores(tl_types::messages::HighScores {
            scores: vec![tl_enums::HighScore::HighScore(tl_types::HighScore {
                pos: 1,
                user_id: h.self_uid,
                score: 4200,
            })],
            users: Vec::new(),
        }),
    );
    let scores = h
        .live
        .get_game_high_scores(bot_peer, BotMsgId(300), BotUserId(h.self_uid as u64))
        .expect("get_game_high_scores");
    assert_eq!(scores.len(), 1);
    assert_eq!(scores[0].score, 4200);

    // 8. Request peer: messages.sendBotRequestedPeer
    let peer_sent_tl =
        synthetic_tl_message(202, bot_uid, h.self_uid, true, 1_750_000_050, "Shared peer");
    h.fake.enqueue_ok(
        tl_fns::messages::SendBotRequestedPeer::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                message: peer_sent_tl,
                pts: 111,
                pts_count: 1,
            })],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_750_000_050,
            seq: 7,
        }),
    );
    let req_peer_msg = h
        .live
        .send_requested_peers(
            BotFullMsgId::new(bot_peer, BotMsgId(150)),
            7,
            &[BotPeerId(h.bob_peer.0)],
        )
        .expect("send_requested_peers");
    assert_eq!(req_peer_msg.id.msg, BotMsgId(202));

    let ctors = h.fake.recorded_ctors();
    for expected in [
        tl_fns::users::GetFullUser::CONSTRUCTOR_ID,
        tl_fns::bots::GetBotMenuButton::CONSTRUCTOR_ID,
        tl_fns::messages::GetBotCallbackAnswer::CONSTRUCTOR_ID,
        tl_fns::account::GetPassword::CONSTRUCTOR_ID,
        tl_fns::messages::RequestUrlAuth::CONSTRUCTOR_ID,
        tl_fns::messages::AcceptUrlAuth::CONSTRUCTOR_ID,
        tl_fns::messages::DeclineUrlAuth::CONSTRUCTOR_ID,
        tl_fns::messages::GetInlineBotResults::CONSTRUCTOR_ID,
        tl_fns::messages::SendInlineBotResult::CONSTRUCTOR_ID,
        tl_fns::messages::GetAttachMenuBots::CONSTRUCTOR_ID,
        tl_fns::messages::ToggleBotInAttachMenu::CONSTRUCTOR_ID,
        tl_fns::messages::SetGameScore::CONSTRUCTOR_ID,
        tl_fns::messages::GetGameHighScores::CONSTRUCTOR_ID,
        tl_fns::messages::SendBotRequestedPeer::CONSTRUCTOR_ID,
    ] {
        assert!(
            ctors.contains(&expected),
            "expected RPC ctor 0x{expected:08x} to be recorded"
        );
    }

    h.shutdown();
}

// =============================================================================
// Test 15: WebViewBackend over FakeRpcClient (all 4 request*WebView, theme JSON,
//          sendWebViewData, prolongWebView loop, invokeWebViewCustomMethod,
//          getWebPage -> tg_iv::Page, channels.joinChannel, remove_bot_from_menu)
// =============================================================================

#[test]
fn live_session_webview_backend_all_rpcs_and_instant_view() {
    let mut h = LiveTestHarness::start();
    let bot_uid = 700_002i64;
    let bot_tl = synthetic_tl_bot(bot_uid, "MiniApp Bot", "miniapp_bot", false);
    let chan_id = 900_001i64;
    let chan_tl = tl_enums::Chat::Channel(tl_types::Channel {
        creator: false,
        left: true,
        broadcast: true,
        verified: true,
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
        id: chan_id,
        access_hash: Some(55_555),
        title: "Rustgram News".to_owned(),
        username: Some("rustgram_news".to_owned()),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_750_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(1200),
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
    });

    h.runtime.tokio_handle().block_on(async {
        let mut s = h.runtime.session().write().await;
        tg_api::convert::apply_tl_users_and_chats(
            &mut s,
            &[tl_enums::User::User(bot_tl)],
            std::slice::from_ref(&chan_tl),
        );
    });

    // 1. messages.requestWebView with palette theme_params JSON
    let captured_theme = Arc::new(Mutex::new(String::new()));
    let captured_theme_clone = Arc::clone(&captured_theme);
    h.fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
        if ctor == tl_fns::messages::RequestWebView::CONSTRUCTOR_ID {
            let req =
                tl_fns::messages::RequestWebView::from_bytes(raw).expect("decode RequestWebView");
            if let Some(tl_enums::DataJSON::DataJSON(dj)) = req.theme_params {
                *captured_theme_clone.lock().expect("lock theme") = dj.data;
            }
            let res = tl_enums::WebViewResult::Url(tl_types::WebViewResultUrl {
                fullsize: true,
                fullscreen: true,
                same_origin: true,
                query_id: Some(77_700),
                url: "https://miniapp.example.org/open#tgWebAppPlatform=tdesktop".to_owned(),
            });
            return Some(Ok(res.to_bytes()));
        }
        None
    }));

    let launch = h
        .live
        .request_web_view(WebViewRequest {
            bot_id: bot_uid as u64,
            start_param: Some("promo".to_owned()),
            url_override: Some("https://miniapp.example.org/open".to_owned()),
            fullscreen: true,
            theme_params_json: "{}".to_owned(), // empty triggers palette JSON generation
        })
        .expect("request_web_view");
    assert_eq!(launch.query_id, 77_700);
    assert!(launch.fullscreen);
    assert!(launch.same_origin);
    assert_eq!(launch.bot.username, "miniapp_bot");
    let theme_json = captured_theme.lock().expect("lock").clone();
    let parsed_theme: serde_json::Value =
        serde_json::from_str(&theme_json).expect("valid theme_params JSON");
    assert!(
        parsed_theme.get("bg_color").is_some() && parsed_theme.get("button_color").is_some(),
        "palette theme params missing expected keys: {theme_json}"
    );
    h.live.stop_all_prolong_web_views();
    h.fake.set_dynamic_handler(Arc::new(|_, _| None));

    // 2. requestSimpleWebView, requestMainWebView, requestAppWebView
    let wv_ok = tl_enums::WebViewResult::Url(tl_types::WebViewResultUrl {
        fullsize: false,
        fullscreen: false,
        same_origin: false,
        query_id: None,
        url: "https://miniapp.example.org/simple".to_owned(),
    });
    h.fake.enqueue_ok(
        tl_fns::messages::RequestSimpleWebView::CONSTRUCTOR_ID,
        &wv_ok,
    );
    let simple = h
        .live
        .request_simple_web_view(
            bot_uid as u64,
            Some("https://miniapp.example.org/simple".to_owned()),
            None,
            false,
            true,
            false,
            None,
        )
        .expect("request_simple_web_view");
    assert_eq!(simple.url, "https://miniapp.example.org/simple");

    h.fake
        .enqueue_ok(tl_fns::messages::RequestMainWebView::CONSTRUCTOR_ID, &wv_ok);
    let main_wv = h
        .live
        .request_main_web_view(
            PeerId::from_user(bot_uid as u64),
            bot_uid as u64,
            Some("start".to_owned()),
            true,
            false,
            None,
        )
        .expect("request_main_web_view");
    assert_eq!(main_wv.url, "https://miniapp.example.org/simple");

    h.fake
        .enqueue_ok(tl_fns::messages::RequestAppWebView::CONSTRUCTOR_ID, &wv_ok);
    let app_wv = h
        .live
        .request_app_web_view(
            PeerId::from_user(bot_uid as u64),
            bot_uid as u64,
            tl_enums::InputBotApp::ID(tl_types::InputBotAppID {
                id: 500,
                access_hash: 600,
            }),
            Some("deep".to_owned()),
            true,
            false,
            true,
            None,
        )
        .expect("request_app_web_view");
    assert_eq!(app_wv.url, "https://miniapp.example.org/simple");

    // 3. prolongWebView loop
    let prolong_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let prolong_count_clone = Arc::clone(&prolong_count);
    h.fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
        if ctor == tl_fns::messages::ProlongWebView::CONSTRUCTOR_ID {
            let req =
                tl_fns::messages::ProlongWebView::from_bytes(raw).expect("decode ProlongWebView");
            assert_eq!(req.query_id, 88_800);
            prolong_count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return Some(Ok(true.to_bytes()));
        }
        None
    }));
    let prolong = h.live.start_prolong_web_view(
        PeerId::from_user(bot_uid as u64),
        bot_uid as u64,
        88_800,
        Duration::from_millis(15),
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while prolong_count.load(std::sync::atomic::Ordering::SeqCst) < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    prolong.stop();
    assert!(
        prolong_count.load(std::sync::atomic::Ordering::SeqCst) >= 2,
        "expected prolongWebView loop to fire at least twice"
    );
    // `stop()` only signals the loop: a prolong request already in flight can still reach the
    // fake afterwards, and without a handler it would consume the next queued response.
    h.fake.set_dynamic_handler(Arc::new(|ctor, _| {
        (ctor == tl_fns::messages::ProlongWebView::CONSTRUCTOR_ID).then(|| Ok(true.to_bytes()))
    }));

    // 4. messages.sendWebViewData
    h.fake.enqueue_ok(
        tl_fns::messages::SendWebViewData::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_750_000_060,
            seq: 8,
        }),
    );
    h.live
        .send_web_app_data(bot_uid as u64, "Checkout", "{\"qty\":2}")
        .expect("send_web_app_data");

    // 5. bots.invokeWebViewCustomMethod
    h.fake.enqueue_ok(
        tl_fns::bots::InvokeWebViewCustomMethod::CONSTRUCTOR_ID,
        &tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
            data: "{\"plan\":\"pro\",\"ok\":true}".to_owned(),
        }),
    );
    let custom_res = h
        .live
        .invoke_custom_method(
            bot_uid as u64,
            "getStorageValues",
            serde_json::json!({"keys": ["plan"]}),
        )
        .expect("invoke_custom_method");
    assert_eq!(custom_res["plan"], "pro");
    assert_eq!(custom_res["ok"], true);

    // 6. messages.getWebPage -> tg_iv::Page conversion
    let iv_webpage = tl_enums::messages::WebPage::WebPage(tl_types::messages::WebPage {
        webpage: tl_enums::WebPage::WebPage(tl_types::WebPage {
            has_large_media: false,
            video_cover_photo: false,
            id: 99_123,
            url: "https://telegra.ph/Rustgram-Live-IV".to_owned(),
            display_url: "telegra.ph/Rustgram-Live-IV".to_owned(),
            hash: 7,
            r#type: Some("article".to_owned()),
            site_name: Some("Telegraph".to_owned()),
            title: Some("Rustgram Instant View".to_owned()),
            description: Some("Live conversion test".to_owned()),
            photo: None,
            embed_url: None,
            embed_type: None,
            embed_width: None,
            embed_height: None,
            duration: None,
            author: None,
            document: None,
            cached_page: Some(tl_enums::Page::Page(tl_types::Page {
                part: false,
                rtl: false,
                v2: true,
                url: "https://telegra.ph/Rustgram-Live-IV".to_owned(),
                blocks: vec![
                    tl_enums::PageBlock::Title(tl_types::PageBlockTitle {
                        text: tl_enums::RichText::TextPlain(tl_types::TextPlain {
                            text: "Live IV Article".to_owned(),
                        }),
                    }),
                    tl_enums::PageBlock::Paragraph(tl_types::PageBlockParagraph {
                        text: tl_enums::RichText::TextBold(tl_types::TextBold {
                            text: Box::new(tl_enums::RichText::TextPlain(tl_types::TextPlain {
                                text: "Hello Instant View".to_owned(),
                            })),
                        }),
                    }),
                    tl_enums::PageBlock::Channel(tl_types::PageBlockChannel {
                        channel: chan_tl.clone(),
                    }),
                    tl_enums::PageBlock::Heading2(tl_types::PageBlockHeading2 {
                        text: tl_enums::RichText::TextPlain(tl_types::TextPlain {
                            text: "Details".to_owned(),
                        }),
                    }),
                ],
                photos: Vec::new(),
                documents: Vec::new(),
                views: Some(321),
            })),
            attributes: None,
        }),
        chats: vec![chan_tl],
        users: Vec::new(),
    });
    h.fake
        .enqueue_ok(tl_fns::messages::GetWebPage::CONSTRUCTOR_ID, &iv_webpage);
    let bundle = h
        .live
        .fetch_iv_page("https://telegra.ph/Rustgram-Live-IV", None)
        .expect("fetch_iv_page")
        .expect("expected Some(IvPageBundle)");
    assert_eq!(bundle.page.id, 99_123);
    assert_eq!(bundle.page.site_name.as_deref(), Some("Telegraph"));
    assert!(bundle.page.v2);
    assert_eq!(bundle.page.views, Some(321));
    assert_eq!(bundle.page.blocks.len(), 4);
    assert!(matches!(
        &bundle.page.blocks[0],
        tg_iv::PageBlock::Title(tg_iv::RichText::Plain(s)) if s == "Live IV Article"
    ));
    assert!(matches!(
        &bundle.page.blocks[2],
        tg_iv::PageBlock::Channel(ch) if ch.id == chan_id && ch.title == "Rustgram News" && !ch.joined
    ));
    assert!(matches!(
        &bundle.page.blocks[3],
        tg_iv::PageBlock::Heading { level: 2, .. }
    ));

    // 7. channels.joinChannel + remove_bot_from_menu
    h.fake.enqueue_ok(
        tl_fns::channels::JoinChannel::CONSTRUCTOR_ID,
        &tl_enums::messages::ChatInviteJoinResult::Ok(tl_types::messages::ChatInviteJoinResultOk {
            updates: tl_enums::Updates::Updates(tl_types::Updates {
                updates: Vec::new(),
                users: Vec::new(),
                chats: Vec::new(),
                date: 1_750_000_070,
                seq: 9,
            }),
        }),
    );
    h.live.join_channel(chan_id).expect("join_channel");

    h.fake.enqueue_ok(
        tl_fns::messages::ToggleBotInAttachMenu::CONSTRUCTOR_ID,
        &true,
    );
    h.live
        .remove_bot_from_menu(bot_uid as u64)
        .expect("remove_bot_from_menu");

    let ctors = h.fake.recorded_ctors();
    for expected in [
        tl_fns::messages::RequestWebView::CONSTRUCTOR_ID,
        tl_fns::messages::RequestSimpleWebView::CONSTRUCTOR_ID,
        tl_fns::messages::RequestMainWebView::CONSTRUCTOR_ID,
        tl_fns::messages::RequestAppWebView::CONSTRUCTOR_ID,
        tl_fns::messages::ProlongWebView::CONSTRUCTOR_ID,
        tl_fns::messages::SendWebViewData::CONSTRUCTOR_ID,
        tl_fns::bots::InvokeWebViewCustomMethod::CONSTRUCTOR_ID,
        tl_fns::messages::GetWebPage::CONSTRUCTOR_ID,
        tl_fns::channels::JoinChannel::CONSTRUCTOR_ID,
        tl_fns::messages::ToggleBotInAttachMenu::CONSTRUCTOR_ID,
    ] {
        assert!(
            ctors.contains(&expected),
            "expected WebView RPC ctor 0x{expected:08x} to be recorded"
        );
    }

    h.shutdown();
}
