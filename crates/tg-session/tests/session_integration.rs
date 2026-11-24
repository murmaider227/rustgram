//! Integration tests for `tg-session`:
//! - `MtprotoRpc` glue over `tg_mtproto::Client` + `FakeTransportFactory`
//! - Simulated login -> `MtpAuthorization` storage round trip -> resume picks saved keys
//! - Passcode unlock & multi-account add / switch / remove / logout cleanup
//! - `SessionRuntime` dedicated Tokio thread: `getState` + `getDifference` + first dialogs page,
//!   history loading, message sending, mark read, typing, and live `ChangeEvent` streaming.
//!
//! Uses only isolated `tempfile::TempDir` directories, fake transports/RPC clients, and
//! synthetic keys/users. Never contacts real Telegram servers or uses real credentials.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tempfile::TempDir;
use tg_api::{
    ApiCredentials, DcId, InvocationError, LoadDirection, LoginStep, RpcClient, SendOptions,
    SendProgressKind,
};
use tg_core::domain::Environment;
use tg_core::launcher::{WorkDirEnv, parse_arguments};
use tg_crypto::{AuthKey, Side};
use tg_data::{ChangeEvent, MsgId, PeerId, UserId};
use tg_mtproto::message::{
    build_encrypted_packet, parse_encrypted_packet, unpack_container, unwrap_gzip_layers,
};
use tg_mtproto::{Client as MtprotoClient, ClientConfig, FakeTransportFactory, KeyDestroyOutcome};
use tg_session::{
    AccountManager, LoginFlow, MtprotoRpc, SessionEvent, SessionRuntime, SessionRuntimeConfig,
    WorkingDirOpenOptions,
};
use tg_storage::{PasscodeKdf, PasscodeKdfKind};
use tg_tl::mtproto::{
    GzipPacked, MsgContainer, RpcResult, enums as mtp_enums, functions as mtp_fns,
    types as mtp_types,
};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, types as tl_types,
};
use tokio::sync::broadcast;

/// Timeout guard applied to every async integration test so a regression fails fast instead of
/// hanging indefinitely.
const TEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Unwraps `gzip_packed`, `invokeWithLayer`, and `initConnection` wrappers to find the inner
/// TL function constructor ID and body.
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

/// Extracts `(inner_msg_id, inner_ctor_id)` pairs from a decrypted client packet (either a
/// single message or a `msg_container`).
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

fn synthetic_credentials() -> ApiCredentials {
    ApiCredentials {
        api_id: 99_001,
        api_hash: "0123456789abcdef0123456789abcdef".to_owned(),
    }
}

fn synthetic_tl_user(id: i64, first_name: &str, username: &str, is_self: bool) -> tl_types::User {
    tl_types::User {
        self_: is_self,
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

// =============================================================================
// Fake RpcClient for deterministic LoginFlow & SessionRuntime tests
// =============================================================================

#[derive(Debug, Clone)]
struct RecordedCall {
    constructor_id: u32,
    dc_id: DcId,
}

type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RecordedCall>>>,
    responses: Arc<Mutex<VecDeque<QueuedResponse>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(128);
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

    fn push_update(&self, updates: tl_enums::Updates) {
        let _ = self.updates_tx.send(updates);
    }

    fn recorded_calls(&self) -> Vec<RecordedCall> {
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
        dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push(RecordedCall {
                constructor_id: ctor,
                dc_id,
            });

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

// =============================================================================
// Test 1: MtprotoRpc over real tg_mtproto::Client + FakeTransportFactory
// =============================================================================

#[tokio::test]
async fn mtproto_rpc_invokes_and_bridges_updates_over_fake_transport() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let factory = Arc::new(FakeTransportFactory::new());
        let mut server = factory.push_connection().await;

        let key = AuthKey::new([0x42u8; 256]);
        let cfg = ClientConfig {
            api_id: 99_001,
            main_dc_id: 2,
            use_pfs: false,
            ..ClientConfig::default()
        };
        let (client, _raw_updates_rx) = MtprotoClient::with_transport(cfg, factory);
        client.import_persistent_keys(vec![(2, key.clone())]);

        let rpc = MtprotoRpc::new(client.clone());
        let mut updates_rx = rpc.subscribe_updates().expect("updates rx");
        let mut new_session_rx = rpc.subscribe_new_session();

        // Drive an RPC invoke (`updates.getState`) concurrently while our fake server responds.
        let rpc_call = rpc.clone();
        let call_task =
            tokio::spawn(
                async move { rpc_call.invoke(&tg_tl::functions::updates::GetState).await },
            );

        // Packet 1: with no initial salt, MTProto 2.0 starts with an initial `ping` before
        // sending user RPCs (`SessionState::on_transport_connected`).
        let pkt1 = tokio::time::timeout(Duration::from_secs(5), server.rx.recv())
            .await
            .expect("server recv pkt1 timeout")
            .expect("server received initial encrypted packet");
        let dec1 = parse_encrypted_packet(&pkt1, &key, Side::Server).expect("decrypt pkt1");
        let msgs1 = extract_client_messages(&dec1);
        assert_eq!(msgs1.len(), 1);
        assert_eq!(msgs1[0].1, mtp_fns::Ping::CONSTRUCTOR_ID);
        let ping_req = mtp_fns::Ping::from_bytes(&dec1.body).expect("parse Ping");

        let server_salt = 12_345u64;
        let mut srv_msg_id = (dec1.msg_id & !3) + 1;
        let mut srv_seq_no = 1u32;

        // 1. Send `new_session_created` (sets salt + transitions session into connected_state).
        let new_sess_body = mtp_enums::NewSession::Created(mtp_types::NewSessionCreated {
            first_msg_id: dec1.msg_id as i64,
            unique_id: 777,
            server_salt: server_salt as i64,
        })
        .to_bytes();
        let new_sess_pkt = build_encrypted_packet(
            server_salt,
            dec1.session_id,
            srv_msg_id,
            srv_seq_no,
            &new_sess_body,
            &key,
            Side::Server,
        );
        srv_msg_id += 4;
        srv_seq_no += 2;
        server.tx.send(Ok(new_sess_pkt)).expect("send new_session");

        // 2. Reply `pong` to the initial ping.
        let pong_body = mtp_enums::Pong::Pong(mtp_types::Pong {
            msg_id: dec1.msg_id as i64,
            ping_id: ping_req.ping_id,
        })
        .to_bytes();
        let pong_pkt = build_encrypted_packet(
            server_salt,
            dec1.session_id,
            srv_msg_id,
            srv_seq_no,
            &pong_body,
            &key,
            Side::Server,
        );
        srv_msg_id += 4;
        srv_seq_no += 2;
        server.tx.send(Ok(pong_pkt)).expect("send pong");

        // 3. Push an `Updates::TooLong` from server.
        let upd_body = tl_enums::Updates::TooLong.to_bytes();
        let upd_pkt = build_encrypted_packet(
            server_salt,
            dec1.session_id,
            srv_msg_id,
            srv_seq_no,
            &upd_body,
            &key,
            Side::Server,
        );
        srv_msg_id += 4;
        srv_seq_no += 2;
        server.tx.send(Ok(upd_pkt)).expect("send update");

        // Packet 2+: now that the session is connected, client flushes `updates.getState`
        // (wrapped in `invokeWithLayer(initConnection(...))`, possibly in a `msg_container`
        // alongside `get_future_salts`).
        let get_state_msg_id = loop {
            let pkt = tokio::time::timeout(Duration::from_secs(5), server.rx.recv())
                .await
                .expect("server recv getState packet timeout")
                .expect("server received follow-up packet");
            let dec = parse_encrypted_packet(&pkt, &key, Side::Server).expect("decrypt pkt");
            let msgs = extract_client_messages(&dec);
            if let Some((mid, _)) = msgs
                .into_iter()
                .find(|(_, ctor)| *ctor == tg_tl::functions::updates::GetState::CONSTRUCTOR_ID)
            {
                break mid;
            }
        };

        // 4. Send the `rpc_result` matching `updates.getState`'s `msg_id`.
        let state = tl_enums::updates::State::State(tl_types::updates::State {
            pts: 42,
            qts: 7,
            date: 1_750_000_000,
            seq: 3,
            unread_count: 1,
        });
        let rpc_res_body = tg_tl::Boxed(RpcResult {
            req_msg_id: get_state_msg_id as i64,
            result: tg_tl::RawObject(state.to_bytes()),
        })
        .to_bytes();
        let res_pkt = build_encrypted_packet(
            server_salt,
            dec1.session_id,
            srv_msg_id,
            srv_seq_no,
            &rpc_res_body,
            &key,
            Side::Server,
        );
        server.tx.send(Ok(res_pkt)).expect("send rpc_result");

        let returned_state = tokio::time::timeout(Duration::from_secs(5), call_task)
            .await
            .expect("call join timeout")
            .expect("call task panicked")
            .expect("invoke succeeded");
        assert_eq!(returned_state, state);

        let pushed_upd = tokio::time::timeout(Duration::from_secs(5), updates_rx.recv())
            .await
            .expect("updates recv timeout")
            .expect("updates received");
        assert_eq!(pushed_upd, tl_enums::Updates::TooLong);

        let new_sess_dc = tokio::time::timeout(Duration::from_secs(5), new_session_rx.recv())
            .await
            .expect("new_session recv timeout")
            .expect("new_session received");
        assert_eq!(new_sess_dc, 2);

        client.shutdown();
    })
    .await
    .expect("test timed out");
}

// =============================================================================
// Test 2: Simulated Login -> Storage Round Trip -> Resume Picks Saved Keys
// =============================================================================

#[tokio::test]
async fn simulated_login_persists_mtp_auth_and_resume_reloads_keys() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let tmp = TempDir::new().expect("tempdir");
        let workdir_str = tmp.path().to_string_lossy().into_owned();
        let creds = synthetic_credentials();

        let key_dc2 = AuthKey::new([0x2Au8; 256]);
        let key_dc4 = AuthKey::new([0x4Bu8; 256]);
        let self_user = synthetic_tl_user(7_111_222_333, "Alice", "alice_test", true);

        // --- Launch 1: Fresh working dir, simulated login, persist MtpAuthorization + passcode ---
        {
            let parsed = parse_arguments(["rustgram", "-workdir", &workdir_str]);
            let env = WorkDirEnv {
                exe_dir: tmp.path().to_path_buf(),
                app_data_dir: None,
                current_dir: tmp.path().to_path_buf(),
            };
            let (mut open_opts, resolved) =
                WorkingDirOpenOptions::from_launch_args(&parsed, &env, creds.clone());
            assert_eq!(resolved.path, tmp.path());
            open_opts = open_opts
                .with_transport_factory(Arc::new(FakeTransportFactory::new()))
                .with_pfs(false);

            let mut manager = AccountManager::open(open_opts).expect("open fresh");
            assert!(!manager.needs_passcode().expect("needs_passcode"));
            manager.unlock(b"").expect("unlock fresh");
            let idx = manager.active_index().expect("active slot 0");
            assert!(!manager.account(idx).expect("acc 0").is_authorized());

            // Simulate LoginFlow over a FakeRpcClient (phone -> code -> authorized).
            let fake_rpc = FakeRpcClient::new(2);
            fake_rpc.enqueue_ok(
                tg_tl::functions::auth::SendCode::CONSTRUCTOR_ID,
                &tl_enums::auth::SentCode::SentCode(tl_types::auth::SentCode {
                    r#type: tl_enums::auth::SentCodeType::App(tl_types::auth::SentCodeTypeApp {
                        length: 5,
                    }),
                    phone_code_hash: "hash_123".to_owned(),
                    next_type: None,
                    timeout: None,
                }),
            );
            fake_rpc.enqueue_ok(
                tg_tl::functions::auth::SignIn::CONSTRUCTOR_ID,
                &tl_enums::auth::Authorization::Authorization(tl_types::auth::Authorization {
                    setup_password_required: false,
                    otherwise_relogin_days: None,
                    tmp_sessions: None,
                    future_auth_token: None,
                    user: tl_enums::User::User(self_user.clone()),
                }),
            );
            fake_rpc.enqueue_ok(
                tg_tl::functions::messages::GetDialogFilters::CONSTRUCTOR_ID,
                &tl_enums::messages::DialogFilters::DialogFilters(
                    tl_types::messages::DialogFilters {
                        tags_enabled: false,
                        filters: Vec::new(),
                    },
                ),
            );

            let mut flow = LoginFlow::at_phone_input(fake_rpc, creds.clone());
            flow.submit_phone("+15550000001")
                .await
                .expect("submit_phone");
            assert!(matches!(flow.step(), LoginStep::CodeInput(_)));
            flow.submit_code("12345").await.expect("submit_code");
            let authed_user = flow
                .authorized_user()
                .expect("login reached Authorized")
                .clone();
            assert_eq!(authed_user.id, self_user.id);

            // Simulate the MTProto client having created persistent keys on DC 2 and DC 4
            // and having migrated main DC to DC 4 during login.
            {
                let live = manager.account_mut(idx).expect("acc 0");
                live.client.set_main_dc_id(4);
                live.client
                    .import_persistent_keys(vec![(2, key_dc2.clone()), (4, key_dc4.clone())]);
            }

            // Write MtpAuthorization + self_serialized back through tg-storage.
            let saved = manager
                .complete_login(idx, &authed_user)
                .expect("complete_login");
            assert_eq!(saved.user_id, self_user.id as u64);
            assert_eq!(saved.main_dc_id, 4);
            assert_eq!(saved.keys.len(), 2);

            // Arm a fast local passcode to also verify passcode-protected resume.
            manager
                .domain_mut()
                .storage_mut()
                .set_passcode(
                    b"test-pass-123",
                    PasscodeKdf {
                        kind: PasscodeKdfKind::Argon2id,
                        memory: 64,
                        time: 1,
                        parallel: 1,
                    },
                    true,
                )
                .expect("set passcode");

            manager.shutdown();
        }

        // --- Launch 2: Fresh reopen verifies passcode gate and resumed keys without login ---
        {
            let open_opts = WorkingDirOpenOptions::new(tmp.path(), creds)
                .with_transport_factory(Arc::new(FakeTransportFactory::new()))
                .with_pfs(false);

            let mut manager = AccountManager::open(open_opts).expect("reopen");
            assert!(manager.needs_passcode().expect("needs_passcode true"));
            assert!(manager.unlock(b"wrong-pass").is_err());
            manager
                .unlock(b"test-pass-123")
                .expect("unlock with passcode");

            let idx = manager.active_index().expect("active slot");
            let live = manager.account(idx).expect("resumed account");
            assert!(
                live.is_authorized(),
                "account should resume already authorized"
            );
            assert_eq!(live.user_id, Some(UserId(self_user.id as u64)));
            assert_eq!(live.main_dc_id(), 4);

            let resumed_keys = live.client.export_persistent_keys();
            assert_eq!(resumed_keys.len(), 2);
            assert!(resumed_keys.contains(&(2, key_dc2)));
            assert!(resumed_keys.contains(&(4, key_dc4)));

            // Verify Domain also loaded self username from AccountMap::self_serialized.
            let core_acc = manager.domain().account(idx).expect("core account");
            assert_eq!(
                core_acc.session.as_ref().map(|s| s.username.as_str()),
                Some("alice_test")
            );

            manager.shutdown();
        }
    })
    .await
    .expect("test timed out");
}

// =============================================================================
// Test 3: Multi-account add / switch / remove & offline logout storage cleanup
// =============================================================================

#[tokio::test]
async fn multi_account_add_switch_remove_and_logout_cleanup() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let tmp = TempDir::new().expect("tempdir");
        let creds = synthetic_credentials();
        // Empty FakeTransportFactory simulates an unreachable/offline server; bounded timeouts
        // ensure logout never hangs and unconfirmed keys are persisted in `keys_to_destroy`.
        let open_opts = WorkingDirOpenOptions::new(tmp.path(), creds.clone())
            .with_transport_factory(Arc::new(FakeTransportFactory::new()))
            .with_pfs(false)
            .with_logout_timeout(Duration::from_millis(150))
            .with_destroy_keys_timeout(Duration::from_millis(150));

        let mut manager = AccountManager::open_and_unlock(open_opts, b"").expect("open & unlock");

        // Authenticate account 0.
        let user0 = synthetic_tl_user(1001, "First", "first_user", true);
        manager
            .account_mut(0)
            .expect("acc 0")
            .client
            .import_persistent_keys(vec![(2, AuthKey::new([0x11u8; 256]))]);
        manager.complete_login(0, &user0).expect("complete login 0");

        // Set a local passcode so we also verify passcode clearing when the last account logs out.
        manager
            .domain_mut()
            .storage_mut()
            .set_passcode(
                b"logout-pass",
                PasscodeKdf {
                    kind: PasscodeKdfKind::Argon2id,
                    memory: 64,
                    time: 1,
                    parallel: 1,
                },
                true,
            )
            .expect("set passcode");

        // Add account 1 and authenticate it.
        let idx1 = manager
            .add_account(Environment::Production)
            .expect("add account 1");
        assert_eq!(idx1, 1);
        assert_eq!(manager.active_index(), Some(1));

        let user1 = synthetic_tl_user(2002, "Second", "second_user", true);
        manager
            .account_mut(idx1)
            .expect("acc 1")
            .client
            .set_main_dc_id(4);
        manager
            .account_mut(idx1)
            .expect("acc 1")
            .client
            .import_persistent_keys(vec![(4, AuthKey::new([0x22u8; 256]))]);
        manager
            .complete_login(idx1, &user1)
            .expect("complete login 1");

        // Switch back to account 0.
        manager.switch_account(0).expect("switch to 0");
        assert_eq!(manager.active_index(), Some(0));
        assert_eq!(manager.account_slots().len(), 2);

        // Remove account 1 (runs logout + KeysDestroyer + storage cleanup).
        let destroy_res = manager
            .remove_account(idx1)
            .await
            .expect("remove account 1");
        assert!(destroy_res.is_some());
        assert_eq!(manager.account_slots().len(), 1);
        assert!(manager.account(idx1).is_none());

        // Logout sole remaining account 0 -> resets to fresh unauthenticated slot 0 and clears
        // the local passcode per `passcodeRemovalAuthorized`.
        let _ = manager.logout_account(0).await.expect("logout account 0");
        let acc0 = manager.account(0).expect("fresh slot 0");
        assert!(!acc0.is_authorized());
        assert!(acc0.client.export_persistent_keys().is_empty());
        assert!(
            !acc0.keys_to_destroy.is_empty(),
            "offline logout should retain unconfirmed keys in keys_to_destroy for later retry"
        );

        manager.shutdown();

        // Fresh reopen confirms account 0 is logged out and passcode was cleared on disk too.
        let reopen_opts = WorkingDirOpenOptions::new(tmp.path(), creds)
            .with_transport_factory(Arc::new(FakeTransportFactory::new()))
            .with_pfs(false);
        let mut reopened = AccountManager::open(reopen_opts).expect("reopen");
        assert!(
            !reopened.needs_passcode().expect("needs_passcode"),
            "passcode must be cleared after sole account logs out"
        );
        reopened.unlock(b"").expect("unlock without passcode");
        assert_eq!(reopened.account_slots().len(), 1);
        assert!(!reopened.account(0).expect("slot 0").is_authorized());
        assert!(
            !reopened
                .account(0)
                .expect("slot 0")
                .keys_to_destroy
                .is_empty(),
            "residual keys_to_destroy should survive restart"
        );
        reopened.shutdown();
    })
    .await
    .expect("test timed out");
}

// =============================================================================
// Test 3b: Online logout sends auth.logOut + KeysDestroyer destroy_auth_key
// =============================================================================

async fn drive_fake_logout_server(
    mut server: tg_mtproto::FakeServerPeer,
    key: AuthKey,
    expect_destroy_auth_key: bool,
) {
    // Packet 1: initial ping.
    let Some(pkt1) = server.rx.recv().await else {
        return;
    };
    let dec1 = parse_encrypted_packet(&pkt1, &key, Side::Server).expect("decrypt initial ping");
    let ping = mtp_fns::Ping::from_bytes(&dec1.body).expect("parse initial ping");
    let salt = 55_555u64;
    let mut srv_msg_id = (dec1.msg_id & !3) + 1;
    let mut srv_seq_no = 1u32;

    let new_sess = mtp_enums::NewSession::Created(mtp_types::NewSessionCreated {
        first_msg_id: dec1.msg_id as i64,
        unique_id: 901,
        server_salt: salt as i64,
    })
    .to_bytes();
    let _ = server.tx.send(Ok(build_encrypted_packet(
        salt,
        dec1.session_id,
        srv_msg_id,
        srv_seq_no,
        &new_sess,
        &key,
        Side::Server,
    )));
    srv_msg_id += 4;
    srv_seq_no += 2;

    let pong = mtp_enums::Pong::Pong(mtp_types::Pong {
        msg_id: dec1.msg_id as i64,
        ping_id: ping.ping_id,
    })
    .to_bytes();
    let _ = server.tx.send(Ok(build_encrypted_packet(
        salt,
        dec1.session_id,
        srv_msg_id,
        srv_seq_no,
        &pong,
        &key,
        Side::Server,
    )));
    srv_msg_id += 4;
    srv_seq_no += 2;

    let mut saw_logout = false;
    let mut saw_destroy = false;

    while let Some(pkt) = server.rx.recv().await {
        let dec = parse_encrypted_packet(&pkt, &key, Side::Server).expect("decrypt client pkt");
        for (mid, ctor) in extract_client_messages(&dec) {
            if ctor == tg_tl::functions::auth::LogOut::CONSTRUCTOR_ID {
                saw_logout = true;
                let logged_out = tl_enums::auth::LoggedOut::LoggedOut(tl_types::auth::LoggedOut {
                    future_auth_token: None,
                });
                let res = tg_tl::Boxed(RpcResult {
                    req_msg_id: mid as i64,
                    result: tg_tl::RawObject(logged_out.to_bytes()),
                })
                .to_bytes();
                let _ = server.tx.send(Ok(build_encrypted_packet(
                    salt,
                    dec1.session_id,
                    srv_msg_id,
                    srv_seq_no,
                    &res,
                    &key,
                    Side::Server,
                )));
                srv_msg_id += 4;
                srv_seq_no += 2;
            } else if ctor == mtp_fns::DestroyAuthKey::CONSTRUCTOR_ID {
                saw_destroy = true;
                let res = tg_tl::Boxed(RpcResult {
                    req_msg_id: mid as i64,
                    result: tg_tl::RawObject(
                        mtp_enums::DestroyAuthKeyRes::DestroyAuthKeyOk.to_bytes(),
                    ),
                })
                .to_bytes();
                let _ = server.tx.send(Ok(build_encrypted_packet(
                    salt,
                    dec1.session_id,
                    srv_msg_id,
                    srv_seq_no,
                    &res,
                    &key,
                    Side::Server,
                )));
                srv_msg_id += 4;
                srv_seq_no += 2;
            }
        }
    }
    assert!(saw_logout, "expected auth.logOut RPC");
    if expect_destroy_auth_key {
        assert!(saw_destroy, "expected destroy_auth_key RPC");
    }
}

#[tokio::test]
async fn logout_sends_auth_logout_and_destroys_keys_over_fake_transport() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let tmp = TempDir::new().expect("tempdir");
        let creds = synthetic_credentials();
        let factory = Arc::new(FakeTransportFactory::new());

        // Connection 1: client.logout() -> auth.logOut.
        let logout_server = factory.push_connection().await;
        // Connection 2: KeysDestroyer -> auth.logOut + destroy_auth_key -> destroy_auth_key_ok.
        let destroy_server = factory.push_connection().await;

        let key = AuthKey::new([0x77u8; 256]);
        let key_id = key.key_id_u64();

        let open_opts = WorkingDirOpenOptions::new(tmp.path(), creds.clone())
            .with_transport_factory(factory)
            .with_pfs(false)
            .with_logout_timeout(Duration::from_secs(5))
            .with_destroy_keys_timeout(Duration::from_secs(5));

        let mut manager = AccountManager::open_and_unlock(open_opts, b"").expect("open & unlock");
        let user = synthetic_tl_user(9001, "Online", "online_user", true);
        manager
            .account_mut(0)
            .expect("acc 0")
            .client
            .import_persistent_keys(vec![(2, key.clone())]);
        manager.complete_login(0, &user).expect("complete_login");

        let s1 = tokio::spawn(drive_fake_logout_server(logout_server, key.clone(), false));
        let s2 = tokio::spawn(drive_fake_logout_server(destroy_server, key, true));

        let destroy_res = manager.logout_account(0).await.expect("logout_account");
        s1.await.expect("logout server task");
        s2.await.expect("destroy server task");

        assert!(destroy_res.all_destroyed());
        assert_eq!(
            destroy_res.outcomes,
            vec![(2, key_id, KeyDestroyOutcome::Destroyed)]
        );
        let fresh = manager.account(0).expect("fresh slot 0");
        assert!(!fresh.is_authorized());
        assert!(fresh.client.export_persistent_keys().is_empty());
        assert!(fresh.keys_to_destroy.is_empty());

        manager.shutdown();
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn logout_without_persistent_keys_makes_zero_network_connections() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let tmp = TempDir::new().expect("tempdir");
        let creds = synthetic_credentials();
        let factory = Arc::new(FakeTransportFactory::new());

        let open_opts = WorkingDirOpenOptions::new(tmp.path(), creds)
            .with_test_mode(true)
            .with_transport_factory(factory.clone())
            .with_logout_timeout(Duration::from_secs(5))
            .with_destroy_keys_timeout(Duration::from_secs(5));

        let mut manager = AccountManager::open_and_unlock(open_opts, b"").expect("open & unlock");
        let destroy_res = manager.logout_account(0).await.expect("logout_account");

        assert!(destroy_res.all_destroyed());
        assert!(destroy_res.outcomes.is_empty());
        assert_eq!(
            factory.connect_calls(),
            0,
            "logging out an unauthenticated account with 0 keys must not dial any DC"
        );
        manager.shutdown();
    })
    .await
    .expect("test timed out");
}

// =============================================================================
// Test 4: SessionRuntime actor (startup sync, dialogs, history, send, read, typing, changes)
// =============================================================================

#[test]
fn session_runtime_end_to_end_with_fake_rpc_and_dedicated_tokio_thread() {
    let self_uid = 500_001i64;
    let peer_uid = 500_002i64;
    let self_user = synthetic_tl_user(self_uid, "Me", "me_user", true);
    let peer_user = synthetic_tl_user(peer_uid, "Bob", "bob_user", false);
    let peer_id = PeerId::from_user(UserId::from_tl(peer_uid));
    let self_peer_id = PeerId::from_user(UserId::from_tl(self_uid));

    let fake = FakeRpcClient::new(2);

    // 1. Startup: users.getUsers([Self])
    fake.enqueue_ok(
        tg_tl::functions::users::GetUsers::CONSTRUCTOR_ID,
        &vec![tl_enums::User::User(self_user.clone())],
    );
    // 2. Startup: updates.getState
    fake.enqueue_ok(
        tg_tl::functions::updates::GetState::CONSTRUCTOR_ID,
        &tl_enums::updates::State::State(tl_types::updates::State {
            pts: 100,
            qts: 0,
            date: 1_750_000_000,
            seq: 1,
            unread_count: 2,
        }),
    );
    // 3. Startup: updates.getDifference (called on start)
    fake.enqueue_ok(
        tg_tl::functions::updates::GetDifference::CONSTRUCTOR_ID,
        &tl_enums::updates::Difference::Empty(tl_types::updates::DifferenceEmpty {
            date: 1_750_000_001,
            seq: 1,
        }),
    );
    // 4. Startup: messages.getDialogs (first page) + messages.getPinnedDialogs
    let top_msg = tl_enums::Message::Message(tl_types::Message {
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
        id: 10,
        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
            user_id: peer_uid,
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
        date: 1_750_000_000,
        message: "Hello from Bob!".to_owned(),
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
    });
    let dialog = tl_enums::Dialog::Dialog(tl_types::Dialog {
        pinned: false,
        unread_mark: false,
        view_forum_as_messages: false,
        peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: peer_uid }),
        top_message: 10,
        read_inbox_max_id: 8,
        read_outbox_max_id: 9,
        unread_count: 2,
        unread_mentions_count: 0,
        unread_reactions_count: 0,
        unread_poll_votes_count: 0,
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
        pts: None,
        draft: None,
        folder_id: None,
        ttl_period: None,
    });
    fake.enqueue_ok(
        tg_tl::functions::messages::GetDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
            dialogs: vec![dialog],
            messages: vec![top_msg.clone()],
            chats: Vec::new(),
            users: vec![tl_enums::User::User(peer_user.clone())],
        }),
    );
    fake.enqueue_ok(
        tg_tl::functions::messages::GetPinnedDialogs::CONSTRUCTOR_ID,
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

    // Start SessionRuntime on its own dedicated Tokio thread.
    let runtime = SessionRuntime::start(
        UserId::from_tl(self_uid),
        fake.clone(),
        SessionRuntimeConfig::default(),
    )
    .expect("start SessionRuntime");

    let mut changes_rx = runtime.subscribe_changes();
    let mut events_rx = runtime.subscribe_events();

    runtime
        .tokio_handle()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(5), runtime.wait_ready()).await
        })
        .expect("wait_ready timeout")
        .expect("startup ready");
    assert!(runtime.is_ready(), "expected runtime.is_ready() == true");

    // Verify Ready event and startup RPC sequence (including getDifference on start!).
    let mut saw_ready = false;
    while let Ok(ev) = events_rx.try_recv() {
        if ev == SessionEvent::Ready {
            saw_ready = true;
        }
    }
    assert!(saw_ready, "expected SessionEvent::Ready");

    let startup_calls = fake.recorded_calls();
    assert!(startup_calls.iter().all(|c| c.dc_id == 2));
    let startup_ctors: Vec<u32> = startup_calls.iter().map(|c| c.constructor_id).collect();
    assert!(startup_ctors.contains(&tg_tl::functions::updates::GetState::CONSTRUCTOR_ID));
    assert!(startup_ctors.contains(&tg_tl::functions::updates::GetDifference::CONSTRUCTOR_ID));
    assert!(startup_ctors.contains(&tg_tl::functions::messages::GetDialogs::CONSTRUCTOR_ID));

    // Verify first dialogs page populated tg_data::Session.
    let dialogs = runtime.dialog_summaries(20);
    assert_eq!(dialogs.len(), 1);
    assert_eq!(dialogs[0].peer_id, Some(peer_id));
    assert_eq!(dialogs[0].title, "Bob Tester");
    assert_eq!(dialogs[0].unread_count, 2);
    assert_eq!(dialogs[0].top_message_preview, "Hello from Bob!");

    // Open history slice for Bob.
    fake.enqueue_ok(
        tg_tl::functions::messages::GetHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![top_msg],
            topics: Vec::new(),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(peer_user)],
        }),
    );
    let slice = runtime
        .open_history_blocking(peer_id, MsgId::ZERO, LoadDirection::Before)
        .expect("open_history")
        .expect("slice present");
    assert_eq!(slice.message_ids, vec![MsgId(10)]);
    let last_msgs = runtime.last_messages(peer_id, 5);
    assert_eq!(last_msgs.len(), 1);
    assert_eq!(last_msgs[0].id, MsgId(10));

    // Send a message to Saved Messages (UpdateShortSentMessage promotion path).
    fake.enqueue_ok(
        tg_tl::functions::messages::SendMessage::CONSTRUCTOR_ID,
        &tl_enums::Updates::UpdateShortSentMessage(tl_types::UpdateShortSentMessage {
            out: true,
            id: 55,
            pts: 101,
            pts_count: 1,
            date: 1_750_000_010,
            media: None,
            entities: None,
            ttl_period: None,
        }),
    );
    let outcome = runtime
        .tokio_handle()
        .block_on(async {
            tokio::time::timeout(
                Duration::from_secs(5),
                runtime.send_message(self_peer_id, "Saved note", SendOptions::default()),
            )
            .await
        })
        .expect("send_message timeout")
        .expect("send_message");
    assert_eq!(outcome.messages.len(), 1);
    assert_eq!(outcome.messages[0].local_id.peer, self_peer_id);
    let saved_msgs = runtime.last_messages(self_peer_id, 5);
    assert_eq!(saved_msgs.len(), 1);
    assert_eq!(saved_msgs[0].id, MsgId(55));

    // Mark Bob's chat read.
    fake.enqueue_ok(
        tg_tl::functions::messages::ReadHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 102,
                pts_count: 1,
            },
        ),
    );
    let flushed = runtime
        .mark_read_blocking(peer_id, MsgId(10), true)
        .expect("mark_read");
    assert_eq!(flushed.len(), 1);
    let after_read = runtime.dialog_summaries(20);
    let bob_row = after_read
        .iter()
        .find(|d| d.peer_id == Some(peer_id))
        .expect("bob dialog");
    assert_eq!(bob_row.unread_count, 0);

    // Send typing indicator to Bob.
    fake.enqueue_ok(tg_tl::functions::messages::SetTyping::CONSTRUCTOR_ID, &true);
    let sent_typing = runtime
        .send_typing_blocking(peer_id, None, SendProgressKind::Typing, 0)
        .expect("send_typing");
    assert!(sent_typing);

    // Drain any prior change events, then push a live incoming short message and verify
    // the background actor pumps it into Session and broadcasts a ChangeEvent!
    while changes_rx.try_recv().is_ok() {}
    fake.push_update(tl_enums::Updates::UpdateShortMessage(
        tl_types::UpdateShortMessage {
            out: false,
            mentioned: false,
            media_unread: false,
            silent: false,
            id: 11,
            user_id: peer_uid,
            message: "Live incoming push!".to_owned(),
            pts: 103,
            pts_count: 1,
            date: 1_750_000_020,
            fwd_from: None,
            via_bot_id: None,
            reply_to: None,
            entities: None,
            ttl_period: None,
        },
    ));

    let received_change = runtime.tokio_handle().block_on(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            match tokio::time::timeout_at(deadline, changes_rx.recv()).await {
                Ok(Ok(ChangeEvent::MessageUpdated { id, .. }))
                    if id.peer == peer_id && id.msg == MsgId(11) =>
                {
                    return true;
                }
                Ok(Ok(_)) => continue,
                _ => return false,
            }
        }
    });
    assert!(
        received_change,
        "expected ChangeEvent::MessageUpdated for live pushed message #11"
    );

    runtime.shutdown();
}
