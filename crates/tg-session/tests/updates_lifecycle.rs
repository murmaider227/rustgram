//! End-to-end `SessionRuntime` & `AccountManager` tests for real-time updates resilience:
//! - gap filling itself within `WAIT_FOR_SKIPPED_TIMEOUT_MS`
//! - unfilled gap triggering `updates.getDifference` promptly via dynamic deadline timer
//! - restart resume from `FileUpdatesStateStore` (skipping `updates.getState`) and encrypted
//!   `tdata` account preferences (`AccountManager::save_updates_state` / `load_updates_state`)
//! - reconnect, network change (`restart_sessions` + `getDifference`), wake-from-sleep ping,
//!   and active channel short-polling.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use tg_api::{ApiCredentials, DcId, InvocationError, RpcClient};
use tg_data::{ChangeEvent, ChannelId, FullMsgId, MsgId, PeerId, PersistedUpdatesState, UserId};
use tg_session::{
    AccountManager, FileUpdatesStateStore, SessionRuntime, SessionRuntimeConfig, UpdatesStateStore,
    WorkingDirOpenOptions,
};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
struct RecordedCall {
    constructor_id: u32,
    request_bytes: Vec<u8>,
}

impl RecordedCall {
    fn decode<R: Identifiable + Deserializable>(&self) -> Option<R> {
        if self.constructor_id == R::CONSTRUCTOR_ID {
            R::from_bytes(&self.request_bytes).ok()
        } else {
            None
        }
    }
}

type RpcResponseQueue = VecDeque<Result<Vec<u8>, InvocationError>>;

struct ScriptedRpcInner {
    main_dc: AtomicI32,
    calls: Mutex<Vec<RecordedCall>>,
    queues: Mutex<HashMap<u32, RpcResponseQueue>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
    ping_count: AtomicUsize,
    fail_next_ping: AtomicBool,
    restart_count: AtomicUsize,
}

#[derive(Clone)]
struct ScriptedRpc {
    inner: Arc<ScriptedRpcInner>,
}

impl ScriptedRpc {
    fn new(dc_id: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(128);
        Self {
            inner: Arc::new(ScriptedRpcInner {
                main_dc: AtomicI32::new(dc_id),
                calls: Mutex::new(Vec::new()),
                queues: Mutex::new(HashMap::new()),
                updates_tx,
                ping_count: AtomicUsize::new(0),
                fail_next_ping: AtomicBool::new(false),
                restart_count: AtomicUsize::new(0),
            }),
        }
    }

    fn enqueue_ok<R>(&self, resp: &R::Return)
    where
        R: Identifiable + RemoteCall,
        R::Return: Serializable,
    {
        self.inner
            .queues
            .lock()
            .entry(R::CONSTRUCTOR_ID)
            .or_default()
            .push_back(Ok(resp.to_bytes()));
    }

    fn push_update(&self, updates: tl_enums::Updates) {
        let _ = self.inner.updates_tx.send(updates);
    }

    fn calls_of<R: Identifiable + Deserializable>(&self) -> Vec<R> {
        self.inner
            .calls
            .lock()
            .iter()
            .filter_map(RecordedCall::decode::<R>)
            .collect()
    }

    fn set_fail_next_ping(&self, fail: bool) {
        self.inner.fail_next_ping.store(fail, Ordering::SeqCst);
    }

    fn ping_count(&self) -> usize {
        self.inner.ping_count.load(Ordering::SeqCst)
    }

    fn restart_count(&self) -> usize {
        self.inner.restart_count.load(Ordering::SeqCst)
    }
}

impl RpcClient for ScriptedRpc {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.inner.calls.lock().push(RecordedCall {
            constructor_id: ctor,
            request_bytes: bytes,
        });
        let next = self
            .inner
            .queues
            .lock()
            .get_mut(&ctor)
            .and_then(VecDeque::pop_front);
        let resp_bytes = match next {
            Some(res) => res?,
            None => {
                let name = tg_tl::from_constructor_id(ctor).unwrap_or("unknown");
                return Err(InvocationError::Transport(format!(
                    "no queued response for {name} (0x{ctor:08x})"
                )));
            }
        };
        R::Return::from_bytes(&resp_bytes).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    fn main_dc_id(&self) -> DcId {
        self.inner.main_dc.load(Ordering::SeqCst)
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        self.inner.main_dc.store(dc_id, Ordering::SeqCst);
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.inner.updates_tx.subscribe())
    }

    async fn ping(&self) -> Result<(), Self::Error> {
        self.inner.ping_count.fetch_add(1, Ordering::SeqCst);
        if self.inner.fail_next_ping.swap(false, Ordering::SeqCst) {
            Err(InvocationError::Transport("simulated ping timeout".into()))
        } else {
            Ok(())
        }
    }

    fn restart_sessions(&self) {
        self.inner.restart_count.fetch_add(1, Ordering::SeqCst);
    }
}

fn synthetic_user(id: i64, first_name: &str, is_self: bool) -> tl_types::User {
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
        stories_unavailable: true,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_forum_sidebar: false,
        bot_guard: false,
        bot_guestchat: false,
        id,
        access_hash: Some(id * 100),
        first_name: Some(first_name.to_owned()),
        last_name: Some("Synthetic".to_owned()),
        username: Some(format!("user_{id}")),
        phone: Some(format!("999000{id}")),
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
    }
}

fn synthetic_channel(id: i64, title: &str) -> tl_types::Channel {
    tl_types::Channel {
        creator: false,
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
        stories_unavailable: true,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        forum_sidebar: false,
        in_news: false,
        id,
        access_hash: Some(id * 100),
        title: title.to_owned(),
        username: Some(format!("chan_{id}")),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_750_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(10),
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: None,
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
        linked_monoforum_id: None,
        pinned_msg_id: None,
    }
}

fn synthetic_msg(peer: tl_enums::Peer, id: i32, date: i32, text: &str) -> tl_enums::Message {
    tl_enums::Message::Message(tl_types::Message {
        out: false,
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
        from_id: Some(peer.clone()),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: peer,
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

fn tl_state(pts: i32, qts: i32, date: i32, seq: i32) -> tl_enums::updates::State {
    tl_enums::updates::State::State(tl_types::updates::State {
        pts,
        qts,
        date,
        seq,
        unread_count: 0,
    })
}

async fn wait_for_message(
    changes_rx: &mut broadcast::Receiver<ChangeEvent>,
    target: FullMsgId,
    timeout: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match tokio::time::timeout_at(deadline, changes_rx.recv()).await {
            Ok(Ok(ChangeEvent::MessageUpdated { id, .. })) if id == target => return true,
            Ok(Ok(_)) => continue,
            _ => return false,
        }
    }
}

#[tokio::test]
async fn session_runtime_gap_fill_and_timeout_get_difference() {
    let self_uid = 500_001i64;
    let peer_uid = 500_002i64;
    let peer_id = PeerId::from_user(UserId::from_tl(peer_uid));
    let rpc = ScriptedRpc::new(2);

    rpc.enqueue_ok::<tl_fns::updates::GetState>(&tl_state(100, 0, 1_750_000_000, 1));
    // Expected for the second (unfilled) gap at pts=104 (missing 103).
    rpc.enqueue_ok::<tl_fns::updates::GetDifference>(&tl_enums::updates::Difference::Difference(
        tl_types::updates::Difference {
            new_messages: vec![
                synthetic_msg(
                    tl_enums::Peer::User(tl_types::PeerUser { user_id: peer_uid }),
                    103,
                    1_750_000_030,
                    "recovered 103",
                ),
                synthetic_msg(
                    tl_enums::Peer::User(tl_types::PeerUser { user_id: peer_uid }),
                    104,
                    1_750_000_040,
                    "recovered 104",
                ),
            ],
            new_encrypted_messages: Vec::new(),
            other_updates: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
            state: tl_state(104, 0, 1_750_000_040, 2),
        },
    ));

    let cfg = SessionRuntimeConfig {
        init_updates_state: true,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..SessionRuntimeConfig::default()
    };

    let rt = SessionRuntime::start_on(
        UserId::from_tl(self_uid),
        rpc.clone(),
        tokio::runtime::Handle::current(),
        cfg,
    )
    .expect("start runtime");

    // Seed peer so short/new messages apply cleanly.
    {
        let mut s = rt.session().write().await;
        tg_api::convert::apply_tl_user(
            &mut s,
            &tl_enums::User::User(synthetic_user(peer_uid, "PeerOne", false)),
        );
    }

    let mut changes_rx = rt.subscribe_changes();
    rt.wait_ready().await.expect("ready");

    // 1. Gap that fills itself within 1s: push pts=102 then pts=101 -> ZERO getDifference!
    rpc.push_update(tl_enums::Updates::UpdateShortMessage(
        tl_types::UpdateShortMessage {
            out: false,
            mentioned: false,
            media_unread: false,
            silent: false,
            id: 102,
            user_id: peer_uid,
            message: "msg 102".into(),
            pts: 102,
            pts_count: 1,
            date: 1_750_000_020,
            fwd_from: None,
            via_bot_id: None,
            reply_to: None,
            entities: None,
            ttl_period: None,
        },
    ));
    tokio::time::sleep(Duration::from_millis(80)).await;
    rpc.push_update(tl_enums::Updates::UpdateShortMessage(
        tl_types::UpdateShortMessage {
            out: false,
            mentioned: false,
            media_unread: false,
            silent: false,
            id: 101,
            user_id: peer_uid,
            message: "msg 101".into(),
            pts: 101,
            pts_count: 1,
            date: 1_750_000_010,
            fwd_from: None,
            via_bot_id: None,
            reply_to: None,
            entities: None,
            ttl_period: None,
        },
    ));

    assert!(
        wait_for_message(
            &mut changes_rx,
            FullMsgId::new(peer_id, MsgId(102)),
            Duration::from_secs(2)
        )
        .await,
        "gap should have filled and applied msg 102"
    );
    assert!(rpc.calls_of::<tl_fns::updates::GetDifference>().is_empty());
    assert_eq!(rt.persisted_updates_state().await.expect("state").pts, 102);

    // 2. Unfilled gap: push pts=104 (missing 103) -> dynamic deadline fires getDifference at ~1s!
    rpc.push_update(tl_enums::Updates::UpdateShortMessage(
        tl_types::UpdateShortMessage {
            out: false,
            mentioned: false,
            media_unread: false,
            silent: false,
            id: 104,
            user_id: peer_uid,
            message: "msg 104".into(),
            pts: 104,
            pts_count: 1,
            date: 1_750_000_040,
            fwd_from: None,
            via_bot_id: None,
            reply_to: None,
            entities: None,
            ttl_period: None,
        },
    ));

    assert!(
        wait_for_message(
            &mut changes_rx,
            FullMsgId::new(peer_id, MsgId(104)),
            Duration::from_secs(3)
        )
        .await,
        "unfilled gap should trigger getDifference after ~1s"
    );
    let diffs = rpc.calls_of::<tl_fns::updates::GetDifference>();
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].pts, 102);
    assert_eq!(rt.persisted_updates_state().await.expect("state").pts, 104);

    rt.shutdown_async().await;
}

#[tokio::test]
async fn session_runtime_restart_resume_from_file_store_and_tdata_prefs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_path = tmp.path().join("updates_state.bin");
    let self_uid = 600_001i64;
    let peer_uid = 600_002i64;
    let peer_id = PeerId::from_user(UserId::from_tl(peer_uid));

    // --- Launch 1: first start (no persisted file yet) -> calls updates.getState ---
    let rpc1 = ScriptedRpc::new(2);
    rpc1.enqueue_ok::<tl_fns::updates::GetState>(&tl_state(200, 4, 1_760_000_000, 10));

    let cfg1 = SessionRuntimeConfig {
        init_updates_state: true,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..SessionRuntimeConfig::default()
    }
    .with_updates_state_path(&state_path);

    let rt1 = SessionRuntime::start_on(
        UserId::from_tl(self_uid),
        rpc1.clone(),
        tokio::runtime::Handle::current(),
        cfg1,
    )
    .expect("start rt1");
    {
        let mut s = rt1.session().write().await;
        tg_api::convert::apply_tl_user(
            &mut s,
            &tl_enums::User::User(synthetic_user(peer_uid, "PeerTwo", false)),
        );
    }
    let mut changes1 = rt1.subscribe_changes();
    rt1.wait_ready().await.expect("rt1 ready");
    assert_eq!(rpc1.calls_of::<tl_fns::updates::GetState>().len(), 1);
    assert!(rpc1.calls_of::<tl_fns::updates::GetDifference>().is_empty());

    // Receive live update advancing pts 200 -> 201 and shut down cleanly.
    rpc1.push_update(tl_enums::Updates::UpdateShortMessage(
        tl_types::UpdateShortMessage {
            out: false,
            mentioned: false,
            media_unread: false,
            silent: false,
            id: 501,
            user_id: peer_uid,
            message: "before restart".into(),
            pts: 201,
            pts_count: 1,
            date: 1_760_000_010,
            fwd_from: None,
            via_bot_id: None,
            reply_to: None,
            entities: None,
            ttl_period: None,
        },
    ));
    assert!(
        wait_for_message(
            &mut changes1,
            FullMsgId::new(peer_id, MsgId(501)),
            Duration::from_secs(2)
        )
        .await
    );
    rt1.shutdown_async().await;

    // Verify file store persisted pts=201, qts=4, date=1_760_000_010, seq=10.
    let store = FileUpdatesStateStore::new(&state_path);
    let saved = store.load().expect("load").expect("saved state present");
    assert_eq!(saved.pts, 201);
    assert_eq!(saved.qts, 4);
    assert_eq!(saved.date, 1_760_000_010);
    assert_eq!(saved.seq, 10);

    // --- Launch 2: restart with same file store -> NO getState, calls getDifference(201)! ---
    let rpc2 = ScriptedRpc::new(2);
    rpc2.enqueue_ok::<tl_fns::updates::GetDifference>(&tl_enums::updates::Difference::Difference(
        tl_types::updates::Difference {
            new_messages: vec![synthetic_msg(
                tl_enums::Peer::User(tl_types::PeerUser { user_id: peer_uid }),
                502,
                1_760_000_099,
                "missed during restart",
            )],
            new_encrypted_messages: Vec::new(),
            other_updates: Vec::new(),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(synthetic_user(
                peer_uid, "PeerTwo", false,
            ))],
            state: tl_state(202, 5, 1_760_000_099, 11),
        },
    ));

    let cfg2 = SessionRuntimeConfig {
        init_updates_state: true,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..SessionRuntimeConfig::default()
    }
    .with_updates_state_path(&state_path);

    let rt2 = SessionRuntime::start_on(
        UserId::from_tl(self_uid),
        rpc2.clone(),
        tokio::runtime::Handle::current(),
        cfg2,
    )
    .expect("start rt2");
    rt2.wait_ready().await.expect("rt2 ready");

    // Must have skipped updates.getState and called updates.getDifference with saved watermarks!
    assert!(rpc2.calls_of::<tl_fns::updates::GetState>().is_empty());
    let diffs = rpc2.calls_of::<tl_fns::updates::GetDifference>();
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].pts, 201);
    assert_eq!(diffs[0].qts, 4);
    assert_eq!(diffs[0].date, 1_760_000_010);

    let msgs = rt2.last_messages_async(peer_id, 5).await;
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].id, MsgId(502));
    assert_eq!(rt2.persisted_updates_state().await.expect("state").pts, 202);
    rt2.shutdown_async().await;

    // --- Verify encrypted tdata AccountManager persistence roundtrip ---
    let creds = ApiCredentials {
        api_id: 1,
        api_hash: "0".repeat(32),
    };
    let fake_transport = Arc::new(tg_mtproto::FakeTransportFactory::new());
    let tdata_dir = tmp.path().join("workdir");
    let mut mgr = AccountManager::open_and_unlock(
        WorkingDirOpenOptions::new(&tdata_dir, creds.clone())
            .with_transport_factory(fake_transport.clone())
            .with_pfs(false),
        b"",
    )
    .expect("open account manager");
    assert_eq!(mgr.load_updates_state(0).expect("load empty"), None);

    let expected_state = PersistedUpdatesState {
        pts: 202,
        qts: 5,
        date: 1_760_000_099,
        seq: 11,
        channel_pts: [(ChannelId(77), 33)].into_iter().collect(),
    };
    mgr.save_updates_state(0, &expected_state)
        .expect("save updates state");
    mgr.shutdown();

    let mut reopened = AccountManager::open_and_unlock(
        WorkingDirOpenOptions::new(&tdata_dir, creds)
            .with_transport_factory(fake_transport)
            .with_pfs(false),
        b"",
    )
    .expect("reopen account manager");
    assert_eq!(
        reopened
            .load_updates_state(0)
            .expect("reload updates state"),
        Some(expected_state)
    );
    reopened.shutdown();
}

#[tokio::test]
async fn session_runtime_network_change_sleep_ping_failure_and_active_channel_poll() {
    let self_uid = 700_001i64;
    let ch_id = ChannelId(909);
    let ch_pid = PeerId::from_channel(ch_id);
    let rpc = ScriptedRpc::new(2);

    rpc.enqueue_ok::<tl_fns::updates::GetState>(&tl_state(10, 0, 1_770_000_000, 1));
    // 1. on_network_change -> restart_sessions + getDifference
    rpc.enqueue_ok::<tl_fns::updates::GetDifference>(&tl_enums::updates::Difference::Empty(
        tl_types::updates::DifferenceEmpty {
            date: 1_770_000_010,
            seq: 2,
        },
    ));
    // 2. on_wake_from_sleep(true) with failing ping -> ping fails, triggers getDifference + active channel diff!
    rpc.enqueue_ok::<tl_fns::updates::GetDifference>(&tl_enums::updates::Difference::Empty(
        tl_types::updates::DifferenceEmpty {
            date: 1_770_000_020,
            seq: 3,
        },
    ));
    rpc.enqueue_ok::<tl_fns::updates::GetChannelDifference>(
        &tl_enums::updates::ChannelDifference::Empty(tl_types::updates::ChannelDifferenceEmpty {
            r#final: true,
            pts: 55,
            timeout: Some(30),
        }),
    );

    let cfg = SessionRuntimeConfig {
        init_updates_state: true,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..SessionRuntimeConfig::default()
    }
    .with_initial_updates_state(PersistedUpdatesState::default());

    let rt = SessionRuntime::start_on(
        UserId::from_tl(self_uid),
        rpc.clone(),
        tokio::runtime::Handle::current(),
        cfg,
    )
    .expect("start");
    rt.wait_ready().await.expect("ready");

    // Seed channel peer & initialize channel PTS via OpenHistory.
    {
        let mut s = rt.session().write().await;
        tg_api::convert::apply_tl_chat(
            &mut s,
            &tl_enums::Chat::Channel(synthetic_channel(909, "LiveChannel")),
        );
    }
    rpc.enqueue_ok::<tl_fns::messages::GetHistory>(&tl_enums::messages::Messages::ChannelMessages(
        tl_types::messages::ChannelMessages {
            inexact: false,
            pts: 50,
            count: 0,
            offset_id_offset: None,
            messages: Vec::new(),
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        },
    ));
    rt.open_history(ch_pid, MsgId::ZERO, tg_api::LoadDirection::Before)
        .await
        .expect("open channel history");

    // 1. Network change restarts sessions and runs getDifference.
    rt.on_network_change().await.expect("on_network_change");
    assert_eq!(rpc.restart_count(), 1);
    assert_eq!(rpc.calls_of::<tl_fns::updates::GetDifference>().len(), 1);

    // 2. Mark channel active and simulate wake from sleep with failing ping -> resyncs both common + active channel!
    rt.set_active_channel(ch_id, true)
        .await
        .expect("set_active_channel");
    rpc.set_fail_next_ping(true);
    let acted = rt.on_wake_from_sleep(true).await.expect("wake");
    assert!(acted);
    assert_eq!(rpc.ping_count(), 1);
    assert_eq!(rpc.calls_of::<tl_fns::updates::GetDifference>().len(), 2);
    assert_eq!(
        rpc.calls_of::<tl_fns::updates::GetChannelDifference>()
            .len(),
        1
    );
    assert_eq!(
        rt.persisted_updates_state()
            .await
            .expect("state")
            .channel_pts
            .get(&ch_id),
        Some(&55)
    );

    rt.shutdown_async().await;
}

#[tokio::test]
async fn on_reconnect_get_difference_increments_unread_for_incoming_messages() {
    let self_uid = 700_050i64;
    let sender_uid = 700_051i64;
    let sender_pid = PeerId::from_user(UserId::from_tl(sender_uid));
    let rpc = ScriptedRpc::new(2);

    rpc.enqueue_ok::<tl_fns::updates::GetState>(&tl_state(20, 0, 1_770_000_000, 1));
    rpc.enqueue_ok::<tl_fns::updates::GetDifference>(&tl_enums::updates::Difference::Difference(
        tl_types::updates::Difference {
            new_messages: vec![synthetic_msg(
                tl_enums::Peer::User(tl_types::PeerUser {
                    user_id: sender_uid,
                }),
                101,
                1_770_000_010,
                "recovered-after-tcp-drop",
            )],
            new_encrypted_messages: Vec::new(),
            other_updates: Vec::new(),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(synthetic_user(
                sender_uid, "Peer", false,
            ))],
            state: tl_state(21, 0, 1_770_000_010, 2),
        },
    ));

    let cfg = SessionRuntimeConfig {
        init_updates_state: true,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..SessionRuntimeConfig::default()
    }
    .with_initial_updates_state(PersistedUpdatesState::default());

    let rt = SessionRuntime::start_on(
        UserId::from_tl(self_uid),
        rpc.clone(),
        tokio::runtime::Handle::current(),
        cfg,
    )
    .expect("start");
    rt.wait_ready().await.expect("ready");

    rt.on_reconnect().await.expect("on_reconnect");

    let session = rt.session().read().await;
    let msg = session
        .message(FullMsgId::new(sender_pid, MsgId(101)))
        .expect("recovered message stored in session");
    assert_eq!(
        msg.text().map(|t| t.text.as_str()),
        Some("recovered-after-tcp-drop")
    );
    let unread = session
        .history(sender_pid)
        .and_then(|h| h.unread_count)
        .unwrap_or(0);
    assert_eq!(
        unread, 1,
        "incoming message recovered by on_reconnect getDifference must increment unread_count"
    );
    drop(session);

    rt.shutdown_async().await;
}
