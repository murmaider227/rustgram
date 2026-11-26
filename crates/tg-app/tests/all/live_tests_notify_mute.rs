//! TGB-2969: muted chats must not notify on a live account.
//!
//! Each test drives the real pipeline: chat-list pages go through `apply_tl_dialog_in_folder`,
//! server pushes through `SessionRuntime::apply_updates_blocking`, new messages arrive as live
//! unread messages, the bridge turns them into notification events, and a headless
//! [`NotificationSystem`] decides with the account's synced settings
//! (`Window::Notifications::System::computeSkipState` in tdesktop). The per-type defaults come
//! from the fake `account.getNotifySettings`.
//!
//! TGB-2980: the same fake answers `account.getNotifySettings(inputNotifyPeer)` for the author
//! of a mention whose own settings were never received.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_app::live::{ErasedRpcClient, LiveSession};
use tg_app::notifications::manager::{NotificationSystem, WindowFocusState};
use tg_app::notifications::settings::{NotificationPreferences, NotifySettingsStore};
use tg_data::{
    Chat, ChatId, Message, MessageFlags, MsgId as DataMsgId, PeerId as DataPeerId, PeerUpdateFlags,
    TextWithEntities, User, UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

const SESSION_ID: u64 = 1;
const SELF_UID: u64 = 6_001;
const ALICE_UID: u64 = 6_002;
const BOB_UID: u64 = 6_003;
const GROUP_ID: u64 = 700;
const FOREVER: i32 = i32::MAX;
const PUMP_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-type default `mute_until` the fake server reports: users, groups, channels.
#[derive(Clone, Copy)]
struct Defaults([i32; 3]);

#[derive(Clone)]
struct NotifyFakeRpc {
    defaults: Arc<Mutex<Defaults>>,
    default_requests: Arc<Mutex<usize>>,
    /// `mute_until` the server holds for any single peer asked about; `None`: the request fails.
    peer_mute: Arc<Mutex<Option<i32>>>,
    peer_requests: Arc<Mutex<usize>>,
    /// While set, a single-peer request stays unanswered.
    hold_peer_requests: Arc<AtomicBool>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl NotifyFakeRpc {
    fn new(defaults: Defaults) -> Self {
        let (updates_tx, _) = broadcast::channel(16);
        Self {
            defaults: Arc::new(Mutex::new(defaults)),
            default_requests: Arc::new(Mutex::new(0)),
            peer_mute: Arc::new(Mutex::new(None)),
            peer_requests: Arc::new(Mutex::new(0)),
            hold_peer_requests: Arc::new(AtomicBool::new(false)),
            updates_tx,
        }
    }

    fn peer_requests(&self) -> usize {
        *self.peer_requests.lock().expect("count")
    }

    fn default_settings(&self, request: &[u8]) -> Option<tl_enums::PeerNotifySettings> {
        let scope = &request[4..];
        let defaults = self.defaults.lock().expect("defaults").0;
        let mute_until = [
            tl_enums::InputNotifyPeer::InputNotifyUsers,
            tl_enums::InputNotifyPeer::InputNotifyChats,
            tl_enums::InputNotifyPeer::InputNotifyBroadcasts,
        ]
        .iter()
        .position(|peer| peer.to_bytes() == scope)
        .map(|index| defaults[index])?;
        Some(notify_settings(Some(mute_until)))
    }
}

impl RpcClient for NotifyFakeRpc {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        self.invoke_in_dc(2, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if ctor == tl_fns::account::GetNotifySettings::CONSTRUCTOR_ID
            && let Some(settings) = self.default_settings(&bytes)
        {
            *self.default_requests.lock().expect("count") += 1;
            return R::Return::from_bytes(&settings.to_bytes())
                .map_err(|e| InvocationError::Deserialize(e.to_string()));
        }
        if ctor == tl_fns::account::GetNotifySettings::CONSTRUCTOR_ID {
            *self.peer_requests.lock().expect("count") += 1;
            while self.hold_peer_requests.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            let mute_until = *self.peer_mute.lock().expect("peer mute");
            if let Some(mute_until) = mute_until {
                return R::Return::from_bytes(&notify_settings(Some(mute_until)).to_bytes())
                    .map_err(|e| InvocationError::Deserialize(e.to_string()));
            }
        }
        Err(InvocationError::Rpc(RpcError {
            code: 500,
            name: format!("UNMOCKED_RPC_{ctor:#010x}"),
            value: None,
        }))
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }

    fn main_dc_id(&self) -> DcId {
        2
    }
}

fn notify_settings(mute_until: Option<i32>) -> tl_enums::PeerNotifySettings {
    tl_enums::PeerNotifySettings::PeerNotifySettings(tl_types::PeerNotifySettings {
        show_previews: Some(true),
        silent: Some(false),
        mute_until,
        ios_sound: None,
        android_sound: None,
        other_sound: None,
        stories_muted: None,
        stories_hide_sender: None,
        stories_ios_sound: None,
        stories_android_sound: None,
        stories_other_sound: None,
    })
}

fn tl_dialog(peer: tl_enums::Peer, top_message: i32, mute_until: Option<i32>) -> tl_enums::Dialog {
    tl_enums::Dialog::Dialog(tl_types::Dialog {
        pinned: false,
        unread_mark: false,
        view_forum_as_messages: false,
        peer,
        top_message,
        read_inbox_max_id: top_message,
        read_outbox_max_id: top_message,
        unread_count: 0,
        unread_mentions_count: 0,
        unread_reactions_count: 0,
        unread_poll_votes_count: 0,
        notify_settings: notify_settings(mute_until),
        pts: None,
        draft: None,
        folder_id: None,
        ttl_period: None,
    })
}

fn alice() -> DataPeerId {
    DataPeerId::from_user(DataUserId(ALICE_UID))
}

fn bob() -> DataPeerId {
    DataPeerId::from_user(DataUserId(BOB_UID))
}

fn group() -> DataPeerId {
    DataPeerId::from_chat(ChatId(GROUP_ID))
}

fn tl_user_peer(uid: u64) -> tl_enums::Peer {
    tl_enums::Peer::User(tl_types::PeerUser {
        user_id: uid as i64,
    })
}

fn tl_group_peer() -> tl_enums::Peer {
    tl_enums::Peer::Chat(tl_types::PeerChat {
        chat_id: GROUP_ID as i64,
    })
}

struct Fixture {
    runtime: Arc<SessionRuntime<ErasedRpcClient>>,
    rpc: NotifyFakeRpc,
    live: LiveSession,
    system: NotificationSystem,
    focus: WindowFocusState,
    now_ms: i64,
    _userpics: tempfile::TempDir,
}

impl Fixture {
    fn new(defaults: Defaults) -> Self {
        let rpc = NotifyFakeRpc::new(defaults);
        let cfg = SessionRuntimeConfig {
            init_updates_state: false,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: false,
            ..Default::default()
        };
        let runtime = Arc::new(
            SessionRuntime::start(DataUserId(SELF_UID), ErasedRpcClient::new(rpc.clone()), cfg)
                .expect("start runtime"),
        );
        let session = Arc::clone(runtime.session());
        runtime.tokio_handle().block_on(async move {
            let mut s = session.write().await;
            for (uid, name, is_self) in [
                (SELF_UID, "Self", true),
                (ALICE_UID, "Alice", false),
                (BOB_UID, "Bob", false),
            ] {
                let mut user = User::new(DataUserId(uid), is_self);
                user.first_name = name.to_owned();
                user.access_hash = uid * 7;
                s.upsert_user(user, PeerUpdateFlags::empty());
            }
            let mut chat = Chat::new(ChatId(GROUP_ID));
            chat.common.name = "Team".to_owned();
            s.upsert_chat(chat, PeerUpdateFlags::empty());
        });
        let live = LiveSession::new(Arc::clone(&runtime), SESSION_ID);
        let userpics = tempfile::tempdir().expect("userpics dir");
        let system = NotificationSystem::headless(
            NotificationPreferences::default(),
            NotifySettingsStore::default(),
            userpics.path().to_path_buf(),
        );
        let deadline = Instant::now() + PUMP_TIMEOUT;
        while !runtime.is_ready() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            runtime.is_ready(),
            "runtime must finish its (empty) startup"
        );
        Self {
            runtime,
            rpc,
            live,
            system,
            focus: WindowFocusState::with_defaults(SESSION_ID),
            now_ms: 1_000,
            _userpics: userpics,
        }
    }

    /// One chat-list page entry, as `messages.getDialogs` delivers it.
    fn apply_dialog(&self, peer: tl_enums::Peer, mute_until: Option<i32>) {
        let session = Arc::clone(self.runtime.session());
        let dialog = tl_dialog(peer, 1, mute_until);
        self.runtime.tokio_handle().block_on(async move {
            let mut s = session.write().await;
            let now = tg_app::live::convert::now_unix_i32();
            let _ = tg_api::convert::apply_tl_dialog_in_folder(&mut s, &dialog, None, now);
        });
    }

    /// A server push (`updateShort`), routed through the session's updates handler.
    fn apply_update(&self, update: tl_enums::Update) {
        let updates = tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
            update,
            date: tg_app::live::convert::now_unix_i32(),
        });
        self.runtime
            .apply_updates_blocking(updates)
            .expect("apply update");
    }

    /// A new incoming message delivered live (`NewMessageType::Unread`).
    fn receive(&self, chat: DataPeerId, from: DataPeerId, id: i64, mentions_me: bool) {
        let session = Arc::clone(self.runtime.session());
        let mut msg = Message::new_text(
            chat,
            DataMsgId(id),
            Some(from),
            tg_app::live::convert::now_unix_i32(),
            TextWithEntities::plain(format!("message {id}")),
        );
        if mentions_me {
            msg.flags.insert(MessageFlags::MENTIONS_ME);
        }
        self.runtime.tokio_handle().block_on(async move {
            session.write().await.insert_new_unread_message(msg);
        });
    }

    fn tick(&mut self) {
        let now_unix = i64::from(tg_app::live::convert::now_unix_i32());
        self.system
            .tick(&mut self.live, &self.focus, self.now_ms, now_unix);
        self.now_ms += 1_500;
    }

    /// Whether the session received `peer`'s own notification settings.
    fn notify_known(&self, peer: DataPeerId) -> bool {
        let session = Arc::clone(self.runtime.session());
        self.runtime
            .tokio_handle()
            .block_on(async move { session.read().await.peer_notify_known(peer) })
    }

    fn shown(&self) -> Vec<i32> {
        self.system
            .dispatched_notifications()
            .iter()
            .map(|n| n.id.msg_id)
            .collect()
    }

    fn sounds(&self) -> usize {
        self.system.sound_player().play_history().len()
    }

    /// Ticks until notification `msg_id` is shown (it is sent after the ones under test, and
    /// the bridge keeps order, so every earlier message has been judged by then).
    fn pump_until_shown(&mut self, msg_id: i32) {
        let deadline = Instant::now() + PUMP_TIMEOUT;
        while Instant::now() < deadline {
            self.tick();
            if self.shown().contains(&msg_id) {
                // A few more rounds so anything due at the same time is dispatched too.
                for _ in 0..3 {
                    self.tick();
                }
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!(
            "notification {msg_id} never shown; shown: {:?}",
            self.shown()
        );
    }
}

const ALL_UNMUTED: Defaults = Defaults([0, 0, 0]);

#[test]
fn muted_chat_from_dialogs_gives_no_notification_or_sound() {
    let mut f = Fixture::new(ALL_UNMUTED);
    f.apply_dialog(tl_user_peer(ALICE_UID), Some(0));
    f.apply_dialog(tl_group_peer(), Some(FOREVER));
    f.tick();

    f.receive(group(), bob(), 101, false);
    f.receive(alice(), alice(), 102, false);
    f.pump_until_shown(102);

    assert_eq!(f.shown(), vec![102], "the muted group must stay silent");
    assert_eq!(f.sounds(), 1, "only the unmuted chat plays a sound");
}

#[test]
fn mention_in_muted_group_notifies_unless_the_author_is_muted() {
    let mut f = Fixture::new(ALL_UNMUTED);
    f.apply_dialog(tl_user_peer(ALICE_UID), Some(0));
    f.apply_dialog(tl_group_peer(), Some(FOREVER));

    // `HistoryItem::specialNotificationPeer`: a mention in a muted group notifies when its
    // author is not muted.
    f.receive(group(), bob(), 201, true);
    f.receive(alice(), alice(), 202, false);
    f.pump_until_shown(202);
    // Bob's own settings were never received: the mention is shown once the request for them
    // is over (here it fails, which leaves him on the per-type default).
    f.pump_until_shown(201);

    // Muting the author (a private-chat exception) silences their mentions too.
    f.apply_update(tl_enums::Update::NotifySettings(
        tl_types::UpdateNotifySettings {
            peer: tl_enums::NotifyPeer::NotifyPeer(tl_types::NotifyPeer {
                peer: tl_user_peer(BOB_UID),
            }),
            notify_settings: notify_settings(Some(FOREVER)),
        },
    ));
    f.receive(group(), bob(), 203, true);
    f.receive(alice(), alice(), 204, false);
    f.pump_until_shown(204);
    assert!(
        !f.shown().contains(&203),
        "mention from a muted author stays silent"
    );
}

/// The author's mute lives only on the server (muted on another device, private chat not in
/// the loaded chat list). tdesktop asks for the author's settings and holds the mention until
/// they arrive (`NotifySettings::request(notifyBy)`, `SkipState::Unknown`). Before TGB-2980 the
/// author read as "inherits the default" and the mention notified at once.
#[test]
fn mention_by_an_author_muted_only_on_the_server_waits_and_stays_silent() {
    let mut f = Fixture::new(ALL_UNMUTED);
    *f.rpc.peer_mute.lock().expect("peer mute") = Some(FOREVER);
    f.rpc.hold_peer_requests.store(true, Ordering::Release);
    f.apply_dialog(tl_user_peer(ALICE_UID), Some(0));
    f.apply_dialog(tl_group_peer(), Some(FOREVER));

    f.receive(group(), bob(), 301, true);
    f.receive(alice(), alice(), 302, false);
    f.pump_until_shown(302);
    assert!(
        !f.shown().contains(&301),
        "the mention waits for its author's settings"
    );
    assert_eq!(
        f.rpc.peer_requests(),
        1,
        "the author's settings are requested"
    );

    f.rpc.hold_peer_requests.store(false, Ordering::Release);
    let deadline = Instant::now() + PUMP_TIMEOUT;
    while !f.notify_known(bob()) {
        assert!(Instant::now() < deadline, "the author's settings arrive");
        std::thread::sleep(Duration::from_millis(5));
    }
    f.receive(group(), bob(), 303, true);
    f.receive(alice(), alice(), 304, false);
    f.pump_until_shown(304);
    assert_eq!(
        f.shown(),
        vec![302, 304],
        "an author muted on the server stays silent"
    );
    assert_eq!(
        f.rpc.peer_requests(),
        1,
        "known settings are not requested again"
    );
}

/// The other answer: the server holds no mute for the author, so the held mention is shown.
#[test]
fn mention_by_an_unknown_author_is_shown_once_the_server_says_not_muted() {
    let mut f = Fixture::new(ALL_UNMUTED);
    *f.rpc.peer_mute.lock().expect("peer mute") = Some(0);
    f.rpc.hold_peer_requests.store(true, Ordering::Release);
    f.apply_dialog(tl_user_peer(ALICE_UID), Some(0));
    f.apply_dialog(tl_group_peer(), Some(FOREVER));

    f.receive(group(), bob(), 311, true);
    f.receive(alice(), alice(), 312, false);
    f.pump_until_shown(312);
    assert!(
        !f.shown().contains(&311),
        "held while the answer is pending"
    );

    f.rpc.hold_peer_requests.store(false, Ordering::Release);
    f.pump_until_shown(311);
    assert_eq!(f.rpc.peer_requests(), 1);
}

#[test]
fn unmuting_through_update_notify_settings_notifies_again() {
    let mut f = Fixture::new(ALL_UNMUTED);
    f.apply_dialog(tl_user_peer(ALICE_UID), Some(0));
    f.apply_dialog(tl_group_peer(), Some(FOREVER));
    f.receive(group(), bob(), 301, false);
    f.receive(alice(), alice(), 302, false);
    f.pump_until_shown(302);
    assert!(!f.shown().contains(&301), "muted before the update");

    // The owner unmutes the group on another device: `updateNotifySettings(notifyPeer)`.
    f.apply_update(tl_enums::Update::NotifySettings(
        tl_types::UpdateNotifySettings {
            peer: tl_enums::NotifyPeer::NotifyPeer(tl_types::NotifyPeer {
                peer: tl_group_peer(),
            }),
            notify_settings: notify_settings(Some(0)),
        },
    ));
    f.receive(group(), bob(), 303, false);
    f.pump_until_shown(303);
}

#[test]
fn per_type_default_mutes_groups_that_inherit_it() {
    // Groups muted by default; Alice's private chat inherits the unmuted users default.
    let mut f = Fixture::new(Defaults([0, FOREVER, 0]));
    f.apply_dialog(tl_user_peer(ALICE_UID), None);
    f.apply_dialog(tl_group_peer(), None);

    f.receive(group(), bob(), 401, false);
    f.receive(alice(), alice(), 402, false);
    f.pump_until_shown(402);
    assert!(
        !f.shown().contains(&401),
        "a group inheriting a muted default stays silent"
    );
    assert!(
        *f.rpc.default_requests.lock().expect("count") >= 3,
        "the per-type defaults are loaded from account.getNotifySettings"
    );

    // A per-chat exception overrides the default (`NotifySettings::isMuted(peer)`).
    f.apply_update(tl_enums::Update::NotifySettings(
        tl_types::UpdateNotifySettings {
            peer: tl_enums::NotifyPeer::NotifyPeer(tl_types::NotifyPeer {
                peer: tl_group_peer(),
            }),
            notify_settings: notify_settings(Some(0)),
        },
    ));
    f.receive(group(), bob(), 403, false);
    f.pump_until_shown(403);

    // A server change of the default itself reaches chats that inherit it.
    f.apply_update(tl_enums::Update::NotifySettings(
        tl_types::UpdateNotifySettings {
            peer: tl_enums::NotifyPeer::NotifyUsers,
            notify_settings: notify_settings(Some(FOREVER)),
        },
    ));
    f.receive(alice(), alice(), 404, false);
    f.receive(group(), bob(), 405, false);
    f.pump_until_shown(405);
    assert!(
        !f.shown().contains(&404),
        "muting the users default silences Alice's chat"
    );
}

#[test]
fn timed_mute_expires_by_unix_seconds() {
    // `mute_until` is a Unix time in seconds (`MutedFromUntil`): a mute that ended a minute ago
    // does not silence the chat, one that ends in an hour does.
    let now = tg_app::live::convert::now_unix_i32();
    let mut f = Fixture::new(ALL_UNMUTED);
    f.apply_dialog(tl_user_peer(ALICE_UID), Some(now - 60));
    f.apply_dialog(tl_group_peer(), Some(now + 3_600));
    f.receive(group(), bob(), 501, false);
    f.receive(alice(), alice(), 502, false);
    f.pump_until_shown(502);
    assert!(!f.shown().contains(&501));
}
