//! Security review #3, D2: the self user read from local storage is used only when it is the
//! authorized user, and it is refreshed from the server once after the startup sync without
//! delaying the first chat list.
//!
//! Uses a fake RPC client only; never contacts Telegram servers.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::{PeerId, UserId};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums};
use tg_tl::{functions as tl_fns, types as tl_types};
use tokio::sync::{Semaphore, broadcast};

const SELF_ID: i64 = 610_001;
const OTHER_ID: i64 = 610_002;

#[derive(Clone)]
struct FakeRpc {
    calls: Arc<Mutex<Vec<u32>>>,
    self_user: tl_types::User,
    users_gate: Arc<Semaphore>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpc {
    fn new(self_user: tl_types::User) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            self_user,
            users_gate: Arc::new(Semaphore::new(0)),
            updates_tx: broadcast::channel(8).0,
        }
    }

    fn calls(&self) -> Vec<u32> {
        self.calls.lock().map(|c| c.clone()).unwrap_or_default()
    }

    fn count(&self, ctor: u32) -> usize {
        self.calls().iter().filter(|c| **c == ctor).count()
    }
}

impl RpcClient for FakeRpc {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(ctor);
        }
        let raw = if ctor == tl_fns::users::GetUsers::CONSTRUCTOR_ID {
            let _permit = self
                .users_gate
                .acquire()
                .await
                .map_err(|_| InvocationError::Dropped)?;
            vec![tl_enums::User::User(self.self_user.clone())].to_bytes()
        } else if ctor == tl_fns::messages::GetDialogs::CONSTRUCTOR_ID {
            tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
                dialogs: Vec::new(),
                messages: Vec::new(),
                chats: Vec::new(),
                users: Vec::new(),
            })
            .to_bytes()
        } else if ctor == tl_fns::messages::GetPinnedDialogs::CONSTRUCTOR_ID {
            tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
                dialogs: Vec::new(),
                messages: Vec::new(),
                chats: Vec::new(),
                users: Vec::new(),
                state: tl_enums::updates::State::State(tl_types::updates::State {
                    pts: 1,
                    qts: 0,
                    date: 1_750_000_000,
                    seq: 1,
                    unread_count: 0,
                }),
            })
            .to_bytes()
        } else {
            return Err(InvocationError::Dropped);
        };
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    fn main_dc_id(&self) -> DcId {
        2
    }

    fn set_main_dc_id(&self, _dc_id: DcId) {}

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn user(id: i64, first_name: &str) -> tl_types::User {
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
        access_hash: Some(id * 3 + 1),
        first_name: Some(first_name.to_owned()),
        last_name: None,
        username: None,
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
    }
}

fn config(stored_self: tl_types::User) -> SessionRuntimeConfig {
    SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        idle_ping_enabled: false,
        initial_self_user: Some(stored_self),
        ..SessionRuntimeConfig::default()
    }
}

fn first_name_and_self(runtime: &SessionRuntime<FakeRpc>, id: i64) -> Option<(String, bool)> {
    let session = Arc::clone(runtime.session());
    runtime.tokio_handle().block_on(async move {
        let session = session.read().await;
        session
            .user(UserId::from_tl(id))
            .map(|u| (u.first_name.clone(), u.is_self()))
    })
}

fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    for _ in 0..200 {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

fn wait_ready(runtime: &SessionRuntime<FakeRpc>) {
    runtime
        .tokio_handle()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(5), runtime.wait_ready()).await
        })
        .expect("wait_ready timeout")
        .expect("startup ready");
}

#[test]
fn sec3_d2_stored_self_is_refreshed_after_the_chat_list_without_delaying_it() {
    let fake = FakeRpc::new(user(SELF_ID, "ServerName"));
    let runtime = SessionRuntime::start(
        UserId::from_tl(SELF_ID),
        fake.clone(),
        config(user(SELF_ID, "StoredName")),
    )
    .expect("start runtime");

    // Startup finishes while the self refresh is still held by the server: it never waits.
    wait_ready(&runtime);
    assert_eq!(
        first_name_and_self(&runtime, SELF_ID),
        Some(("StoredName".to_owned(), true)),
        "the stored self user is shown at once"
    );
    assert!(
        wait_until(|| fake.count(tl_fns::users::GetUsers::CONSTRUCTOR_ID) == 1),
        "the self user is requested once in the background"
    );
    let calls = fake.calls();
    let users_at = calls
        .iter()
        .position(|c| *c == tl_fns::users::GetUsers::CONSTRUCTOR_ID);
    let dialogs_at = calls
        .iter()
        .position(|c| *c == tl_fns::messages::GetDialogs::CONSTRUCTOR_ID);
    assert!(
        dialogs_at < users_at,
        "the chat list is requested before the self refresh: {calls:x?}"
    );

    fake.users_gate.add_permits(1);
    assert!(
        wait_until(
            || first_name_and_self(&runtime, SELF_ID).is_some_and(|(name, _)| name == "ServerName")
        ),
        "the server's self user replaces the stored copy"
    );
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(fake.count(tl_fns::users::GetUsers::CONSTRUCTOR_ID), 1);
    runtime.shutdown();
}

#[test]
fn sec3_d2_stored_self_of_another_user_is_ignored() {
    let fake = FakeRpc::new(user(SELF_ID, "ServerName"));
    fake.users_gate.add_permits(4);
    let runtime = SessionRuntime::start(
        UserId::from_tl(SELF_ID),
        fake.clone(),
        config(user(OTHER_ID, "Someone")),
    )
    .expect("start runtime");
    wait_ready(&runtime);

    assert_eq!(
        first_name_and_self(&runtime, OTHER_ID),
        None,
        "a stored user that is not the authorized one must not become a SELF peer"
    );
    assert_eq!(
        first_name_and_self(&runtime, SELF_ID),
        Some(("ServerName".to_owned(), true)),
        "the real self user is fetched during startup instead"
    );
    assert_eq!(fake.count(tl_fns::users::GetUsers::CONSTRUCTOR_ID), 1);
    let session = Arc::clone(runtime.session());
    let self_peer = runtime
        .tokio_handle()
        .block_on(async move { session.read().await.self_peer_id() });
    assert_eq!(self_peer, PeerId::from_user(UserId::from_tl(SELF_ID)));
    runtime.shutdown();
}
