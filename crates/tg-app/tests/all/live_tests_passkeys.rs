//! `FakeRpcClient` + `FakeWebAuthnProvider` integration tests for Passkeys:
//! - Account passkey management (`account.getPasskeys`, `account.initPasskeyRegistration`,
//!   `account.registerPasskey`, `account.deletePasskey`) via [`LiveSession`] and [`LivePasskeysHandle`];
//! - Cross-device caBLE / hybrid QR passkey registration (`FIDO:/...`);
//! - Intro passkey sign-in (`auth.initPasskeyLogin`, `auth.finishPasskeyLogin`, DC routing from
//!   `user_handle`, and `SESSION_PASSWORD_NEEDED` -> SRP 2FA) via [`IntroWidget`];
//! - `MockSession` & `MockSettingsBackend` demo passkeys and `SettingsController` section wiring.
//!
//! All tests use [`FakeWebAuthnProvider`] and never call real `webauthn.dll` UI or hardware.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tg_api::types::ApiCredentials;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::UserId as DataUserId;
use tg_platform::webauthn::{
    FakeWebAuthnProvider, LoginResult, WebAuthnErrorKind, WebAuthnProvider, encode_base64url,
    encode_base64url_padded, parse_attestation_object, parse_client_data,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use tg_app::intro::IntroWidget;
use tg_app::intro::fake_invoker::{FakeRpcClient as IntroFakeRpcClient, TEST_2FA_PASSWORD};
use tg_app::live::multi_account::MultiAccountWorkspaceHost;
use tg_app::live::session::LiveSession;
use tg_app::notifications::manager::NotificationSystem;
use tg_app::notifications::settings::{NotificationPreferences, NotifySettingsStore};
use tg_app::settings::backend::SettingsBackend;
use tg_app::settings::mock::MockSettingsBackend;
use tg_app::settings::passkeys::PasskeysState;
use tg_app::settings::{SettingsController, SettingsSection};
use tg_app::stand_in::mock_session::MockSession;
use tg_app::stand_in::settings::ShellSettings;
use tg_app::tray::MockTrayPlatform;

type QueuedResp = (u32, Result<Vec<u8>, InvocationError>);
type RecordedCalls = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

#[derive(Clone)]
struct FakePasskeyRpc {
    main_dc: Arc<Mutex<DcId>>,
    recorded: RecordedCalls,
    responses: Arc<Mutex<VecDeque<QueuedResp>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakePasskeyRpc {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(32);
        Self {
            main_dc: Arc::new(Mutex::new(2)),
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<R>(&self, resp: &R::Return)
    where
        R: RemoteCall + Identifiable,
        R::Return: Serializable,
    {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((R::CONSTRUCTOR_ID, Ok(resp.to_bytes())));
    }

    fn enqueue_rpc_err<R: Identifiable>(&self, code: i32, name: &str) {
        self.responses.lock().expect("lock responses").push_back((
            R::CONSTRUCTOR_ID,
            Err(InvocationError::Rpc(RpcError {
                code,
                name: name.to_owned(),
                value: None,
            })),
        ));
    }

    fn recorded_calls(&self) -> Vec<(u32, Vec<u8>)> {
        self.recorded.lock().expect("lock recorded").clone()
    }
}

impl RpcClient for FakePasskeyRpc {
    type Error = InvocationError;

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock dc")
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        *self.main_dc.lock().expect("lock dc") = dc_id;
    }

    async fn invoke<R: RemoteCall + Send + Sync>(
        &self,
        req: &R,
    ) -> Result<R::Return, InvocationError> {
        self.invoke_in_dc(self.main_dc_id(), req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, InvocationError> {
        let bytes = req.to_bytes();
        let cid = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((cid, bytes));

        let next = {
            let mut q = self.responses.lock().expect("lock responses");
            q.iter()
                .position(|(expected_cid, _)| *expected_cid == cid)
                .and_then(|idx| q.remove(idx))
        };
        match next {
            Some((_, Ok(resp_bytes))) => R::Return::from_bytes(&resp_bytes)
                .map_err(|e| InvocationError::Deserialize(e.to_string())),
            Some((_, Err(err))) => Err(err),
            None => Err(InvocationError::Rpc(RpcError::new(
                501,
                "NO_QUEUED_FAKE_RESPONSE",
            ))),
        }
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn no_sync_runtime_config() -> SessionRuntimeConfig {
    SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        changes_capacity: 32,
        idle_ping_enabled: false,
        ..SessionRuntimeConfig::default()
    }
}

fn make_live_session(
    rpc: FakePasskeyRpc,
    provider: Arc<FakeWebAuthnProvider>,
) -> (tokio::runtime::Runtime, LiveSession) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let session_rt = Arc::new(
        SessionRuntime::start_on(
            DataUserId(1000),
            tg_app::live::ErasedRpcClient::new(rpc),
            rt.handle().clone(),
            no_sync_runtime_config(),
        )
        .expect("start session runtime"),
    );
    let live = LiveSession::new(session_rt, 42);
    live.passkeys_handle()
        .set_provider(provider as Arc<dyn WebAuthnProvider>);
    (rt, live)
}

fn sample_register_options_tl() -> tl_enums::account::PasskeyRegistrationOptions {
    let options_json = serde_json::json!({
        "publicKey": {
            "rp": { "id": "telegram.org", "name": "Telegram" },
            "user": {
                "id": encode_base64url_padded(b"2:1000"),
                "name": "demo_user",
                "displayName": "Demo User"
            },
            "challenge": encode_base64url(b"live-reg-challenge-xyz"),
            "pubKeyCredParams": [
                { "type": "public-key", "alg": -7 },
                { "type": "public-key", "alg": -257 }
            ],
            "timeout": 60000
        }
    })
    .to_string();
    tl_enums::account::PasskeyRegistrationOptions::PasskeyRegistrationOptions(
        tl_types::account::PasskeyRegistrationOptions {
            options: tl_enums::DataJSON::DataJSON(tl_types::DataJSON { data: options_json }),
        },
    )
}

fn tl_passkey(id: &str, name: &str, date: i32, last_usage_date: Option<i32>) -> tl_enums::Passkey {
    tl_enums::Passkey::Passkey(tl_types::Passkey {
        id: id.to_owned(),
        name: name.to_owned(),
        date,
        software_emoji_id: None,
        last_usage_date,
    })
}

#[test]
fn live_passkeys_list_register_delete_and_hybrid_qr_over_fake_rpc() {
    let rpc = FakePasskeyRpc::new();
    let provider = Arc::new(FakeWebAuthnProvider::new_supported());
    provider.set_default_credential_id(b"cred-live-test-01".to_vec());

    // 1. Initial `account.getPasskeys`
    rpc.enqueue_ok::<tl_fns::account::GetPasskeys>(&tl_enums::account::Passkeys::Passkeys(
        tl_types::account::Passkeys {
            passkeys: vec![tl_passkey(
                "pk_existing",
                "Hardware Key A",
                1_736_000_000,
                Some(1_739_000_000),
            )],
        },
    ));

    // 2. `account.initPasskeyRegistration` + `account.registerPasskey`
    rpc.enqueue_ok::<tl_fns::account::InitPasskeyRegistration>(&sample_register_options_tl());
    rpc.enqueue_ok::<tl_fns::account::RegisterPasskey>(&tl_passkey(
        "pk_new_01",
        "Windows Hello",
        1_740_000_000,
        None,
    ));

    // 3. `account.deletePasskey` + reload `account.getPasskeys`
    rpc.enqueue_ok::<tl_fns::account::DeletePasskey>(&true);
    rpc.enqueue_ok::<tl_fns::account::GetPasskeys>(&tl_enums::account::Passkeys::Passkeys(
        tl_types::account::Passkeys {
            passkeys: vec![tl_passkey(
                "pk_new_01",
                "Windows Hello",
                1_740_000_000,
                None,
            )],
        },
    ));

    // 4. Hybrid QR registration (`account.initPasskeyRegistration` fresh after previous completed)
    rpc.enqueue_ok::<tl_fns::account::InitPasskeyRegistration>(&sample_register_options_tl());

    let (_rt, mut live) = make_live_session(rpc.clone(), Arc::clone(&provider));
    assert!(live.is_passkey_supported());
    assert!(live.can_register_passkey());

    live.refresh_passkeys();
    let initial = live.passkeys();
    assert_eq!(initial.len(), 1);
    assert_eq!(initial[0].id, "pk_existing");
    assert_eq!(initial[0].name, "Hardware Key A");

    // Register a new passkey via SettingsBackend
    let added = live.register_passkey().expect("register passkey");
    assert_eq!(added.id, "pk_new_01");
    assert_eq!(live.passkeys().len(), 2);

    // Verify exact TL payload sent in `account.registerPasskey`
    let calls = rpc.recorded_calls();
    let (_, reg_bytes) = calls
        .iter()
        .find(|(cid, _)| *cid == tl_fns::account::RegisterPasskey::CONSTRUCTOR_ID)
        .expect("registerPasskey call recorded");
    let reg_req =
        tl_fns::account::RegisterPasskey::from_bytes(reg_bytes).expect("decode registerPasskey");
    let tl_enums::InputPasskeyCredential::PublicKey(pk) = reg_req.credential;
    let expected_id = encode_base64url_padded(b"cred-live-test-01");
    assert_eq!(pk.id, expected_id);
    assert_eq!(pk.raw_id, expected_id);
    let tl_enums::InputPasskeyResponse::Register(resp) = pk.response else {
        panic!("expected InputPasskeyResponse::Register");
    };
    let tl_enums::DataJSON::DataJSON(cdj) = resp.client_data;
    let parsed_cd = parse_client_data(cdj.data.as_bytes()).expect("parse clientDataJSON");
    assert_eq!(parsed_cd.kind, "webauthn.create");
    assert_eq!(parsed_cd.challenge, b"live-reg-challenge-xyz");
    assert_eq!(parsed_cd.origin, "https://telegram.org");
    let parsed_att =
        parse_attestation_object(&resp.attestation_data).expect("valid CBOR attestation");
    assert_eq!(parsed_att.fmt, "none");
    assert_eq!(
        parsed_att.parsed_auth_data.credential_id.as_deref(),
        Some(&b"cred-live-test-01"[..])
    );

    // Delete the old passkey
    live.delete_passkey("pk_existing").expect("delete passkey");
    let remaining = live.passkeys();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, "pk_new_01");

    // Hybrid QR registration returns a valid FIDO:/... URI
    let qr_uri = live
        .start_passkey_hybrid_qr()
        .expect("hybrid qr registration");
    assert!(qr_uri.starts_with("FIDO:/"));
    assert!(qr_uri["FIDO:/".len()..].bytes().all(|b| b.is_ascii_digit()));
}

#[test]
fn live_passkeys_handles_cancellation_unsupported_quota_and_rpc_error() {
    let rpc = FakePasskeyRpc::new();
    let provider = Arc::new(FakeWebAuthnProvider::new_supported());
    provider.push_make_credential_result(Err(WebAuthnErrorKind::Cancelled));

    rpc.enqueue_ok::<tl_fns::account::InitPasskeyRegistration>(&sample_register_options_tl());
    rpc.enqueue_rpc_err::<tl_fns::account::DeletePasskey>(400, "PASSKEY_ID_INVALID");

    let (_rt, mut live) = make_live_session(rpc.clone(), Arc::clone(&provider));

    // User cancellation propagates cleanly without calling `account.registerPasskey`
    let err = live.register_passkey().expect_err("cancelled");
    assert!(err.contains("cancelled"));
    assert!(
        !rpc.recorded_calls()
            .iter()
            .any(|(cid, _)| *cid == tl_fns::account::RegisterPasskey::CONSTRUCTOR_ID)
    );

    // Pending registration is cached within timeout_ms (second init reuses cached options without RPC)
    let cached = live
        .passkeys_handle()
        .init_registration()
        .expect("cached init");
    assert_eq!(cached.challenge, b"live-reg-challenge-xyz");
    let init_count = rpc
        .recorded_calls()
        .iter()
        .filter(|(cid, _)| *cid == tl_fns::account::InitPasskeyRegistration::CONSTRUCTOR_ID)
        .count();
    assert_eq!(init_count, 1);

    // Delete error surfaces RPC failure
    let del_err = live.delete_passkey("nonexistent").expect_err("rpc err");
    assert!(del_err.contains("PASSKEY_ID_INVALID"));

    // Quota limit disables `can_register_passkey`
    live.passkeys_handle().set_max_passkeys(0);
    assert!(!live.can_register_passkey());

    // Unsupported provider disables both `is_passkey_supported` and `can_register_passkey`
    live.passkeys_handle()
        .set_provider(Arc::new(FakeWebAuthnProvider::new_unsupported()));
    assert!(!live.is_passkey_supported());
    assert!(!live.can_register_passkey());
}

#[test]
fn intro_passkey_login_routes_to_user_dc_and_authorizes() {
    let fake_rpc = IntroFakeRpcClient::default();
    let mut intro = IntroWidget::with_invoker(
        ApiCredentials {
            api_id: 12345,
            api_hash: "test-hash".to_owned(),
        },
        fake_rpc,
    );
    let provider = Arc::new(FakeWebAuthnProvider::new_supported());
    provider.set_default_user_handle(b"4:777001".to_vec());
    intro.set_passkey_provider(Arc::clone(&provider) as Arc<dyn WebAuthnProvider>);
    assert!(intro.qr.passkey_supported);

    let now = Instant::now();
    intro.start_qr_flow(now).expect("enter QR flow");
    intro.login_with_passkey(now).expect("passkey login");

    assert_eq!(intro.main_dc_id(), 4);
    assert_eq!(intro.fake_rpc().unwrap().last_passkey_finish_dc(), Some(4));
    assert_eq!(intro.fake_rpc().unwrap().passkey_init_calls(), 1);
    let user = intro
        .passkey_authorized_user()
        .expect("passkey login authorized user");
    assert_eq!(user.first_name.as_deref(), Some("Passkey"));
    assert_eq!(provider.recorded_logins().len(), 1);
    assert_eq!(provider.recorded_logins()[0].rp_id, "telegram.org");
}

#[test]
fn intro_passkey_login_with_2fa_password_and_error_cases() {
    let fake_rpc = IntroFakeRpcClient::default();
    fake_rpc.set_passkey_require_2fa(true);
    let mut intro = IntroWidget::with_invoker(
        ApiCredentials {
            api_id: 12345,
            api_hash: "test-hash".to_owned(),
        },
        fake_rpc,
    );
    let provider = Arc::new(FakeWebAuthnProvider::new_supported());
    provider.set_default_user_handle(b"2:1000".to_vec());
    intro.set_passkey_provider(Arc::clone(&provider) as Arc<dyn WebAuthnProvider>);

    let now = Instant::now();
    intro.start_qr_flow(now).expect("start qr");

    // 1. Cancelled ceremony returns Ok(()) without setting an error
    provider.push_get_assertion_result(Err(WebAuthnErrorKind::Cancelled));
    intro.login_with_passkey(now).expect("cancelled is silent");
    assert!(intro.error.is_empty());
    assert!(intro.passkey_authorized_user().is_none());

    // 2. Invalid userHandle fails with PASSKEY_HANDLE_INVALID before sending finishPasskeyLogin
    provider.push_get_assertion_result(Ok(LoginResult {
        client_data_json: b"{}".to_vec(),
        credential_id: b"cid".to_vec(),
        authenticator_data: vec![0u8; 37],
        signature: vec![1u8; 32],
        user_handle: b"0:bad".to_vec(),
    }));
    let bad_err = intro
        .login_with_passkey(now)
        .expect_err("invalid user handle");
    assert!(bad_err.to_string().contains("PASSKEY_HANDLE_INVALID"));
    assert_eq!(intro.fake_rpc().unwrap().last_passkey_finish_dc(), None);

    // 3. Valid assertion with 2FA required -> transitions to PasswordCheck -> SRP succeeds
    intro
        .login_with_passkey(now)
        .expect("transitions to passkey 2FA");
    assert!(intro.is_passkey_password_check());
    assert!(intro.passkey_authorized_user().is_none());

    intro
        .submit_password(TEST_2FA_PASSWORD, now)
        .expect("2FA SRP check completes passkey login");
    assert!(!intro.is_passkey_password_check());
    assert!(intro.passkey_authorized_user().is_some());
}

#[test]
fn mock_session_and_settings_controller_passkeys_crud() {
    let mut session = MockSession::new(true);
    assert!(session.is_passkey_supported());
    assert!(session.can_register_passkey());
    let initial = session.passkeys();
    assert_eq!(initial.len(), 2);

    let created = session.register_passkey().expect("mock register passkey");
    assert_eq!(session.passkeys().len(), 3);
    assert!(!PasskeysState::display_name(&created).is_empty());

    let qr = session
        .start_passkey_hybrid_qr()
        .expect("mock hybrid qr uri");
    assert!(qr.starts_with("FIDO:/"));

    session
        .delete_passkey(&created.id)
        .expect("mock delete passkey");
    assert_eq!(session.passkeys().len(), 2);

    let mut ctrl = SettingsController::new(Box::new(MockSettingsBackend::new()));
    ctrl.push_section(SettingsSection::Passkeys);
    assert_eq!(ctrl.current_section(), SettingsSection::Passkeys);
    assert_eq!(SettingsSection::Passkeys.title(), "Passkeys");
}

#[test]
fn multi_account_host_and_intro_forward_window_hwnd_to_passkey_provider() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");

    let rpc0 = FakePasskeyRpc::new();
    let rpc1 = FakePasskeyRpc::new();
    rpc0.enqueue_ok::<tl_fns::account::InitPasskeyRegistration>(&sample_register_options_tl());
    rpc0.enqueue_ok::<tl_fns::account::RegisterPasskey>(&tl_passkey(
        "pk_slot0",
        "Account 0 Key",
        1_740_000_001,
        None,
    ));
    rpc1.enqueue_ok::<tl_fns::account::InitPasskeyRegistration>(&sample_register_options_tl());
    rpc1.enqueue_ok::<tl_fns::account::RegisterPasskey>(&tl_passkey(
        "pk_slot1",
        "Account 1 Key",
        1_740_000_002,
        None,
    ));

    let rt0 = Arc::new(
        SessionRuntime::start_on(
            DataUserId(1000),
            tg_app::live::ErasedRpcClient::new(rpc0),
            rt.handle().clone(),
            no_sync_runtime_config(),
        )
        .expect("start rt0"),
    );
    let rt1 = Arc::new(
        SessionRuntime::start_on(
            DataUserId(2000),
            tg_app::live::ErasedRpcClient::new(rpc1),
            rt.handle().clone(),
            no_sync_runtime_config(),
        )
        .expect("start rt1"),
    );

    let settings_dir = tempfile::tempdir().expect("tempdir");
    let mut multi = MultiAccountWorkspaceHost::new_headless_with_tray(
        ShellSettings::default(),
        settings_dir.path().join("passkeys_hwnd.json"),
        NotificationSystem::new(
            NotificationPreferences::default(),
            NotifySettingsStore::default(),
        ),
        Box::new(MockTrayPlatform::default()),
    );

    // Add slot 0 before setting HWND, then set HWND, then add slot 1 after setting HWND.
    multi.add_account(0, DataUserId(1000), false, false, rt0, None, true, false);
    const FAKE_HWND: usize = 0x0012_3456;
    multi.set_window_hwnd(FAKE_HWND);
    multi.add_account(1, DataUserId(2000), false, false, rt1, None, false, false);

    let provider0 = Arc::new(FakeWebAuthnProvider::new_supported());
    let provider1 = Arc::new(FakeWebAuthnProvider::new_supported());

    for (slot, provider) in [(0, Arc::clone(&provider0)), (1, Arc::clone(&provider1))] {
        let session = multi
            .account_mut(slot)
            .expect("account slot")
            .workspace
            .live_session_mut();
        assert_eq!(session.passkeys_handle().window_hwnd(), FAKE_HWND);
        session
            .passkeys_handle()
            .set_provider(provider as Arc<dyn WebAuthnProvider>);
        let created = session.register_passkey().expect("register passkey");
        assert!(created.id.starts_with("pk_slot"));
    }

    assert_eq!(provider0.recorded_registration_hwnds(), vec![FAKE_HWND]);
    assert_eq!(provider1.recorded_registration_hwnds(), vec![FAKE_HWND]);

    // Intro passkey login also forwards the configured HWND.
    let fake_rpc = IntroFakeRpcClient::default();
    let mut intro = IntroWidget::with_invoker(
        ApiCredentials {
            api_id: 12345,
            api_hash: "test-hash".to_owned(),
        },
        fake_rpc,
    );
    let intro_provider = Arc::new(FakeWebAuthnProvider::new_supported());
    intro_provider.set_default_user_handle(b"2:1000".to_vec());
    intro.set_passkey_provider(Arc::clone(&intro_provider) as Arc<dyn WebAuthnProvider>);
    intro.set_passkey_window_hwnd(FAKE_HWND);

    let now = Instant::now();
    intro.start_qr_flow(now).expect("enter QR flow");
    intro.login_with_passkey(now).expect("passkey login");
    assert_eq!(intro_provider.recorded_login_hwnds(), vec![FAKE_HWND]);
}
