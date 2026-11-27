//! Session actor concurrency (TGB-2893, smoothness package A):
//! - no network request runs while the actor holds the `Session` write lock, so UI reads never
//!   wait for the server (`getDifference`, `getChannelDifference`, post-update reloads);
//! - history pages, sends, edits, deletes, typing and read receipts run beside the actor loop,
//!   so one slow request does not delay incoming updates;
//! - sends, edits and deletes still reach the server one at a time in the order asked for;
//! - pts / seq ordering and gap recovery are unchanged while requests are in flight;
//! - updates state writes are debounced, run off the actor and are flushed on shutdown.
//!
//! Timings printed with `--nocapture` are measured on the fake RPC below.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tg_api::{DcId, InvocationError, LoadDirection, RpcClient, SendOptions};
use tg_data::{ChangeEvent, FullMsgId, MsgId, PeerId, PersistedUpdatesState, Session, UserId};
use tg_session::{
    InMemoryUpdatesStateStore, SessionRuntime, SessionRuntimeConfig, UpdatesStateStore,
};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::{RwLock, Semaphore, broadcast};

type RpcResponseQueue = VecDeque<Result<Vec<u8>, InvocationError>>;

struct GatedRpcInner {
    main_dc: AtomicI32,
    /// Constructor ids in call order.
    calls: Mutex<Vec<u32>>,
    queues: Mutex<HashMap<u32, RpcResponseQueue>>,
    /// Requests of a gated constructor wait for a permit before answering.
    gates: Mutex<HashMap<u32, Arc<Semaphore>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
    /// The runtime's session, to check the write lock at request time.
    session: OnceLock<Arc<RwLock<Session>>>,
    /// `(constructor, write lock held)` for every request made by the updates path.
    lock_probe: Mutex<Vec<(u32, bool)>>,
    in_flight: AtomicUsize,
}

#[derive(Clone)]
struct GatedRpc {
    inner: Arc<GatedRpcInner>,
}

/// Requests the actor awaits itself (the updates path): any of them made while the session
/// write lock is held would block UI reads for the whole round trip.
const PROBED: [u32; 4] = [
    tl_fns::updates::GetDifference::CONSTRUCTOR_ID,
    tl_fns::updates::GetChannelDifference::CONSTRUCTOR_ID,
    tl_fns::updates::GetState::CONSTRUCTOR_ID,
    tl_fns::messages::GetPinnedDialogs::CONSTRUCTOR_ID,
];

impl GatedRpc {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(GatedRpcInner {
                main_dc: AtomicI32::new(2),
                calls: Mutex::new(Vec::new()),
                queues: Mutex::new(HashMap::new()),
                gates: Mutex::new(HashMap::new()),
                updates_tx,
                session: OnceLock::new(),
                lock_probe: Mutex::new(Vec::new()),
                in_flight: AtomicUsize::new(0),
            }),
        }
    }

    fn attach_session(&self, session: &Arc<RwLock<Session>>) {
        let _ = self.inner.session.set(Arc::clone(session));
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

    /// Holds every `R` request until [`Self::open`] gives it a permit.
    fn gate<R: Identifiable>(&self) {
        self.inner
            .gates
            .lock()
            .insert(R::CONSTRUCTOR_ID, Arc::new(Semaphore::new(0)));
    }

    fn open<R: Identifiable>(&self, permits: usize) {
        if let Some(gate) = self.inner.gates.lock().get(&R::CONSTRUCTOR_ID) {
            gate.add_permits(permits);
        }
    }

    fn calls(&self) -> Vec<u32> {
        self.inner.calls.lock().clone()
    }

    fn count<R: Identifiable>(&self) -> usize {
        self.calls()
            .into_iter()
            .filter(|&c| c == R::CONSTRUCTOR_ID)
            .count()
    }

    fn lock_probe(&self) -> Vec<(u32, bool)> {
        self.inner.lock_probe.lock().clone()
    }

    fn push_update(&self, updates: tl_enums::Updates) {
        let _ = self.inner.updates_tx.send(updates);
    }
}

impl RpcClient for GatedRpc {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.inner.calls.lock().push(ctor);
        if PROBED.contains(&ctor)
            && let Some(session) = self.inner.session.get()
        {
            let locked = session.try_read().is_err();
            self.inner.lock_probe.lock().push((ctor, locked));
        }
        self.inner.in_flight.fetch_add(1, Ordering::SeqCst);
        let gate = self.inner.gates.lock().get(&ctor).cloned();
        if let Some(gate) = gate
            && let Ok(permit) = gate.acquire().await
        {
            permit.forget();
        }
        self.inner.in_flight.fetch_sub(1, Ordering::SeqCst);
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

const SELF_UID: i64 = 700_001;
const PEER_UID: i64 = 700_002;
const OTHER_UID: i64 = 700_003;

fn tl_state(pts: i32, date: i32, seq: i32) -> tl_types::updates::State {
    tl_types::updates::State {
        pts,
        qts: 0,
        date,
        seq,
        unread_count: 0,
    }
}

fn user_peer(uid: i64) -> tl_enums::Peer {
    tl_enums::Peer::User(tl_types::PeerUser { user_id: uid })
}

fn short_message(id: i32, pts: i32, text: &str) -> tl_enums::Updates {
    tl_enums::Updates::UpdateShortMessage(tl_types::UpdateShortMessage {
        out: false,
        mentioned: false,
        media_unread: false,
        silent: false,
        id,
        user_id: PEER_UID,
        message: text.into(),
        pts,
        pts_count: 1,
        date: 1_750_000_000 + id,
        fwd_from: None,
        via_bot_id: None,
        reply_to: None,
        entities: None,
        ttl_period: None,
    })
}

fn difference(
    messages: Vec<tl_enums::Message>,
    pts: i32,
    seq: i32,
) -> tl_enums::updates::Difference {
    tl_enums::updates::Difference::Difference(tl_types::updates::Difference {
        new_messages: messages,
        new_encrypted_messages: Vec::new(),
        other_updates: Vec::new(),
        chats: Vec::new(),
        users: Vec::new(),
        state: tl_enums::updates::State::State(tl_state(pts, 1_750_000_500, seq)),
    })
}

fn history_page(peer_uid: i64, ids: &[i32]) -> tl_enums::messages::Messages {
    tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
        messages: ids
            .iter()
            .rev()
            .map(|&id| synthetic_msg(user_peer(peer_uid), id, 1_749_000_000 + id, "old"))
            .collect(),
        topics: Vec::new(),
        chats: Vec::new(),
        users: Vec::new(),
    })
}

fn sent(id: i32, pts: i32) -> tl_enums::Updates {
    tl_enums::Updates::UpdateShortSentMessage(tl_types::UpdateShortSentMessage {
        out: true,
        id,
        pts,
        pts_count: 1,
        date: 1_750_000_900,
        media: None,
        entities: None,
        ttl_period: None,
    })
}

fn affected(pts: i32) -> tl_enums::messages::AffectedMessages {
    tl_enums::messages::AffectedMessages::AffectedMessages(tl_types::messages::AffectedMessages {
        pts,
        pts_count: 1,
    })
}

fn peer(uid: i64) -> PeerId {
    PeerId::from_user(UserId::from_tl(uid))
}

type Rt = Arc<SessionRuntime<GatedRpc>>;

/// Starts a runtime on the current test runtime with `updates.getState` -> `pts = 100`, the
/// two test users known and the fake RPC watching the session lock.
async fn start(store: Option<Arc<dyn UpdatesStateStore>>) -> (GatedRpc, Rt) {
    let rpc = GatedRpc::new();
    rpc.enqueue_ok::<tl_fns::updates::GetState>(&tl_enums::updates::State::State(tl_state(
        100,
        1_750_000_000,
        1,
    )));
    let mut cfg = SessionRuntimeConfig {
        init_updates_state: true,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        idle_ping_enabled: false,
        ..SessionRuntimeConfig::default()
    };
    if let Some(store) = store {
        cfg = cfg.with_updates_state_store(store);
    }
    let rt = SessionRuntime::start_on(
        UserId::from_tl(SELF_UID),
        rpc.clone(),
        tokio::runtime::Handle::current(),
        cfg,
    )
    .expect("start runtime");
    {
        let mut s = rt.session().write().await;
        for (uid, name) in [(PEER_UID, "PeerOne"), (OTHER_UID, "PeerTwo")] {
            tg_api::convert::apply_tl_user(
                &mut s,
                &tl_enums::User::User(synthetic_user(uid, name, false)),
            );
        }
    }
    // The probe watches the lock only from here: this fixture's own write above (held, or
    // queued behind the runtime's startup read) made the startup `updates.getState` look like
    // a request under the lock on a slow runner (TGB-3133). The runtime never holds the lock
    // across a request; every request after this point is checked.
    rpc.attach_session(rt.session());
    rt.wait_ready().await.expect("ready");
    (rpc, Arc::new(rt))
}

async fn wait_until(what: &str, timeout: Duration, mut cond: impl AsyncFnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + timeout;
    while !cond().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

async fn has_message(rt: &Rt, peer_uid: i64, id: i64) -> bool {
    rt.session()
        .read()
        .await
        .message(FullMsgId::new(peer(peer_uid), MsgId(id)))
        .is_some()
}

async fn pts(rt: &Rt) -> i32 {
    rt.persisted_updates_state().await.expect("state").pts
}

fn assert_no_request_under_lock(rpc: &GatedRpc) {
    let probe = rpc.lock_probe();
    assert!(!probe.is_empty(), "the updates path made no probed request");
    let locked: Vec<_> = probe.iter().filter(|(_, locked)| *locked).collect();
    assert!(
        locked.is_empty(),
        "requests made while the session write lock was held: {locked:?}"
    );
}

async fn shutdown(rt: Rt) {
    match Arc::try_unwrap(rt) {
        Ok(rt) => rt.shutdown_async().await,
        Err(_) => panic!("runtime still shared at shutdown"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_difference_leaves_the_session_readable() {
    let (rpc, rt) = start(None).await;
    rpc.gate::<tl_fns::updates::GetDifference>();
    rpc.enqueue_ok::<tl_fns::updates::GetDifference>(&difference(
        vec![synthetic_msg(
            user_peer(PEER_UID),
            101,
            1_750_000_101,
            "caught up",
        )],
        101,
        2,
    ));

    let reconnect = tokio::spawn({
        let rt = Arc::clone(&rt);
        async move { rt.on_reconnect().await }
    });
    wait_until(
        "getDifference in flight",
        Duration::from_secs(2),
        async || rpc.count::<tl_fns::updates::GetDifference>() == 1,
    )
    .await;

    // A UI read while the server has not answered: before this change it waited for the whole
    // round trip (the actor held the write lock across `getDifference`).
    let started = Instant::now();
    let read = tokio::time::timeout(Duration::from_millis(500), rt.session().read()).await;
    let waited = started.elapsed();
    assert!(read.is_ok(), "UI read blocked behind getDifference");
    drop(read);
    println!("measured: UI read during an unanswered getDifference took {waited:?}");

    rpc.open::<tl_fns::updates::GetDifference>(1);
    reconnect
        .await
        .expect("join")
        .expect("on_reconnect succeeds");
    assert!(has_message(&rt, PEER_UID, 101).await);
    assert_eq!(pts(&rt).await, 101);
    assert_no_request_under_lock(&rpc);
    shutdown(rt).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_history_page_does_not_delay_incoming_updates() {
    let (rpc, rt) = start(None).await;
    rpc.gate::<tl_fns::messages::GetHistory>();
    rpc.enqueue_ok::<tl_fns::messages::GetHistory>(&history_page(OTHER_UID, &[1, 2, 3]));

    let history = tokio::spawn({
        let rt = Arc::clone(&rt);
        async move {
            rt.open_history(peer(OTHER_UID), MsgId(0), LoadDirection::Before)
                .await
        }
    });
    wait_until("getHistory in flight", Duration::from_secs(2), async || {
        rpc.count::<tl_fns::messages::GetHistory>() == 1
    })
    .await;

    // The same page asked again while in flight is deduplicated, as before.
    let dup = rt
        .open_history(peer(OTHER_UID), MsgId(0), LoadDirection::Before)
        .await
        .expect("dedup answer");
    assert!(
        dup.is_none(),
        "duplicate history request should answer None"
    );

    let pushed = Instant::now();
    rpc.push_update(short_message(101, 101, "live"));
    wait_until(
        "update applied while history is in flight",
        Duration::from_secs(2),
        async || has_message(&rt, PEER_UID, 101).await,
    )
    .await;
    println!(
        "measured: update applied {:?} after push while getHistory was unanswered",
        pushed.elapsed()
    );
    assert!(!history.is_finished(), "history must still be in flight");
    assert_eq!(pts(&rt).await, 101);

    rpc.open::<tl_fns::messages::GetHistory>(1);
    let slice = history
        .await
        .expect("join")
        .expect("history ok")
        .expect("history slice");
    assert_eq!(slice.message_ids.len(), 3);
    for id in 1..=3 {
        assert!(
            has_message(&rt, OTHER_UID, id).await,
            "history message {id}"
        );
    }
    // A finished request no longer blocks a new one for the same page.
    rpc.enqueue_ok::<tl_fns::messages::GetHistory>(&history_page(OTHER_UID, &[1, 2, 3]));
    rpc.open::<tl_fns::messages::GetHistory>(1);
    assert!(
        rt.open_history(peer(OTHER_UID), MsgId(0), LoadDirection::Before)
            .await
            .expect("second page")
            .is_some()
    );
    shutdown(rt).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gap_recovery_runs_while_requests_are_in_flight_without_losing_updates() {
    let (rpc, rt) = start(None).await;
    rpc.gate::<tl_fns::messages::GetHistory>();
    rpc.gate::<tl_fns::messages::SendMessage>();
    rpc.enqueue_ok::<tl_fns::messages::GetHistory>(&history_page(OTHER_UID, &[5, 6]));
    rpc.enqueue_ok::<tl_fns::messages::SendMessage>(&sent(110, 106));
    // The gap at pts 104 (102 and 103 missing) is recovered by getDifference.
    rpc.enqueue_ok::<tl_fns::updates::GetDifference>(&difference(
        vec![
            synthetic_msg(user_peer(PEER_UID), 102, 1_750_000_102, "gap 102"),
            synthetic_msg(user_peer(PEER_UID), 103, 1_750_000_103, "gap 103"),
            synthetic_msg(user_peer(PEER_UID), 104, 1_750_000_104, "gap 104"),
        ],
        104,
        2,
    ));

    let history = tokio::spawn({
        let rt = Arc::clone(&rt);
        async move {
            rt.open_history(peer(OTHER_UID), MsgId(0), LoadDirection::Before)
                .await
        }
    });
    let send = tokio::spawn({
        let rt = Arc::clone(&rt);
        async move {
            rt.send_message(peer(OTHER_UID), "hello", SendOptions::default())
                .await
        }
    });
    wait_until(
        "history and send in flight",
        Duration::from_secs(2),
        async || {
            rpc.count::<tl_fns::messages::GetHistory>() == 1
                && rpc.count::<tl_fns::messages::SendMessage>() == 1
        },
    )
    .await;

    rpc.push_update(short_message(101, 101, "in order"));
    rpc.push_update(short_message(104, 104, "after a gap"));
    rpc.push_update(tl_enums::Updates::TooLong);
    wait_until("gap recovered", Duration::from_secs(3), async || {
        has_message(&rt, PEER_UID, 104).await
    })
    .await;
    for id in 101..=104 {
        assert!(has_message(&rt, PEER_UID, id).await, "message {id} lost");
    }
    assert_eq!(pts(&rt).await, 104);
    let diffs = rpc.count::<tl_fns::updates::GetDifference>();
    assert_eq!(diffs, 1, "one getDifference for the gap");
    assert!(!history.is_finished() && !send.is_finished());

    // The send answers after the difference: its pts (106 = 104 + 1 + an unseen 105) is a
    // gap of its own and must wait like any other, not be applied out of order.
    rpc.enqueue_ok::<tl_fns::updates::GetDifference>(&difference(
        vec![synthetic_msg(
            user_peer(PEER_UID),
            105,
            1_750_000_105,
            "gap 105",
        )],
        106,
        3,
    ));
    rpc.open::<tl_fns::messages::SendMessage>(1);
    rpc.open::<tl_fns::messages::GetHistory>(1);
    let outcome = send.await.expect("join").expect("send ok");
    assert_eq!(outcome.messages.len(), 1);
    history
        .await
        .expect("join")
        .expect("history ok")
        .expect("slice");
    wait_until(
        "send's pts gap recovered",
        Duration::from_secs(4),
        async || pts(&rt).await == 106,
    )
    .await;
    assert!(has_message(&rt, PEER_UID, 105).await);
    assert!(
        has_message(&rt, OTHER_UID, 110).await,
        "sent message promoted to its server id"
    );
    assert!(has_message(&rt, OTHER_UID, 5).await && has_message(&rt, OTHER_UID, 6).await);
    assert_no_request_under_lock(&rpc);
    shutdown(rt).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sends_edits_and_deletes_reach_the_server_in_order() {
    let (rpc, rt) = start(None).await;
    rpc.gate::<tl_fns::messages::SendMessage>();
    rpc.enqueue_ok::<tl_fns::messages::SendMessage>(&sent(201, 102));
    rpc.enqueue_ok::<tl_fns::messages::DeleteMessages>(&affected(103));
    rpc.enqueue_ok::<tl_fns::messages::SendMessage>(&sent(202, 104));

    let first = tokio::spawn({
        let rt = Arc::clone(&rt);
        async move {
            rt.send_message(peer(OTHER_UID), "first", SendOptions::default())
                .await
        }
    });
    wait_until("first send in flight", Duration::from_secs(2), async || {
        rpc.count::<tl_fns::messages::SendMessage>() == 1
    })
    .await;
    // The delete is queued before the second send is even created: two separately spawned
    // tasks reached the command channel in either order (TGB-3133), which the runtime then
    // kept, as it should.
    let (queued_tx, queued_rx) = tokio::sync::oneshot::channel();
    let delete = tokio::spawn({
        let rt = Arc::clone(&rt);
        async move {
            let mut delete =
                std::pin::pin!(rt.delete_messages(peer(OTHER_UID), vec![MsgId(7)], true));
            // The first poll sends the command; the answer comes later.
            let first_poll =
                std::future::poll_fn(|cx| std::task::Poll::Ready(delete.as_mut().poll(cx))).await;
            let _ = queued_tx.send(());
            match first_poll {
                std::task::Poll::Ready(res) => res,
                std::task::Poll::Pending => delete.await,
            }
        }
    });
    queued_rx.await.expect("delete queued");
    let second = tokio::spawn({
        let rt = Arc::clone(&rt);
        async move {
            rt.send_message(peer(OTHER_UID), "second", SendOptions::default())
                .await
        }
    });
    // Commands are queued behind the first send; updates still flow meanwhile.
    rpc.push_update(short_message(150, 101, "while sending"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rpc.count::<tl_fns::messages::SendMessage>(), 1);
    assert_eq!(rpc.count::<tl_fns::messages::DeleteMessages>(), 0);

    rpc.open::<tl_fns::messages::SendMessage>(2);
    first.await.expect("join").expect("first send");
    delete.await.expect("join").expect("delete");
    second.await.expect("join").expect("second send");

    let order: Vec<u32> = rpc
        .calls()
        .into_iter()
        .filter(|&c| {
            c == tl_fns::messages::SendMessage::CONSTRUCTOR_ID
                || c == tl_fns::messages::DeleteMessages::CONSTRUCTOR_ID
        })
        .collect();
    assert_eq!(
        order,
        vec![
            tl_fns::messages::SendMessage::CONSTRUCTOR_ID,
            tl_fns::messages::DeleteMessages::CONSTRUCTOR_ID,
            tl_fns::messages::SendMessage::CONSTRUCTOR_ID,
        ]
    );
    assert!(has_message(&rt, OTHER_UID, 201).await && has_message(&rt, OTHER_UID, 202).await);
    assert!(has_message(&rt, PEER_UID, 150).await);
    shutdown(rt).await;
}

/// A store whose writes take `delay`, counting them.
struct SlowStore {
    inner: InMemoryUpdatesStateStore,
    delay: Duration,
    saves: AtomicUsize,
}

impl UpdatesStateStore for SlowStore {
    fn load(&self) -> tg_session::Result<Option<PersistedUpdatesState>> {
        self.inner.load()
    }

    fn save(&self, state: &PersistedUpdatesState) -> tg_session::Result<()> {
        std::thread::sleep(self.delay);
        self.saves.fetch_add(1, Ordering::SeqCst);
        self.inner.save(state)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_writes_are_debounced_off_the_actor_and_flushed_on_shutdown() {
    let store = Arc::new(SlowStore {
        inner: InMemoryUpdatesStateStore::new(),
        delay: Duration::from_millis(50),
        saves: AtomicUsize::new(0),
    });
    let (rpc, rt) = start(Some(store.clone() as Arc<dyn UpdatesStateStore>)).await;
    let mut changes = rt.subscribe_changes();

    const COUNT: i32 = 40;
    let started = Instant::now();
    for i in 1..=COUNT {
        rpc.push_update(short_message(1_000 + i, 100 + i, "burst"));
    }
    let last = FullMsgId::new(peer(PEER_UID), MsgId(i64::from(1_000 + COUNT)));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(deadline, changes.recv()).await {
            Ok(Ok(ChangeEvent::MessageUpdated { id, .. })) if id == last => break,
            Ok(_) => {}
            Err(_) => panic!("burst not applied"),
        }
    }
    let applied_in = started.elapsed();
    println!(
        "measured: {COUNT} updates applied in {applied_in:?} with a 50 ms store write \
         ({} writes so far; one synchronous write per update would block the actor >= {:?})",
        store.saves.load(Ordering::SeqCst),
        Duration::from_millis(50) * COUNT as u32,
    );
    assert!(
        applied_in < Duration::from_millis(50) * (COUNT as u32) / 2,
        "the store write is still on the actor path"
    );
    assert_eq!(pts(&rt).await, 100 + COUNT);

    shutdown(rt).await;
    let saves = store.saves.load(Ordering::SeqCst);
    assert!(
        (1..=3).contains(&saves),
        "expected the burst to be coalesced into a few writes, got {saves}"
    );
    assert_eq!(
        store.inner.snapshot().map(|s| s.pts),
        Some(100 + COUNT),
        "shutdown must write the newest state"
    );
}
