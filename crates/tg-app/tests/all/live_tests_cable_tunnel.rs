//! End-to-end tests for the caBLEv2 hybrid transport tunnel client wired into
//! `LivePasskeysHandle` (registration / `MakeCredential`) and `IntroWidget`
//! (sign-in / `GetAssertion`) using in-process fakes (`FakeCableBleScanner`,
//! `FakeCableTunnelConnector`, `FakeCablePhone`) with zero network or Bluetooth calls.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tg_api::types::ApiCredentials;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_app::intro::IntroWidget;
use tg_app::live::bridge::LiveRuntimeBridge;
use tg_app::live::passkeys::LivePasskeysHandle;
use tg_app::settings::backend::PasskeyHybridQrStatus;
use tg_app_core::core_types::erased_rpc::ErasedRpcClient;
use tg_data::UserId as DataUserId;
use tg_platform::cable_scanner::{
    BleAdvertisement, CableServiceUuid, FakeCableBleScanner, build_cable_service_data_section,
};
use tg_platform::cable_tunnel::{CableTunnelError, FakeCablePhone, FakeCableTunnelConnector};
use tg_platform::webauthn::{
    CTAP_ERR_OPERATION_DENIED, FakeWebAuthnProvider, cable_encrypt_advert, encode_base64url,
    encode_base64url_padded,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

type QueuedResp = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeCableRpc {
    main_dc: Arc<Mutex<DcId>>,
    responses: Arc<Mutex<VecDeque<QueuedResp>>>,
    recorded_cids: Arc<Mutex<Vec<u32>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeCableRpc {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(16);
        Self {
            main_dc: Arc::new(Mutex::new(2)),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            recorded_cids: Arc::new(Mutex::new(Vec::new())),
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

    fn recorded_cids(&self) -> Vec<u32> {
        self.recorded_cids.lock().expect("lock recorded").clone()
    }
}

impl RpcClient for FakeCableRpc {
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
        self.recorded_cids.lock().expect("lock recorded").push(cid);
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
        changes_capacity: 16,
        idle_ping_enabled: false,
        ..SessionRuntimeConfig::default()
    }
}

fn sample_register_options() -> tl_enums::account::PasskeyRegistrationOptions {
    let options_json = serde_json::json!({
        "publicKey": {
            "rp": { "id": "telegram.org", "name": "Telegram" },
            "user": {
                "id": encode_base64url_padded(b"2:1000"),
                "name": "demo_user",
                "displayName": "Demo User"
            },
            "challenge": encode_base64url(b"cable-reg-challenge-001"),
            "pubKeyCredParams": [{ "type": "public-key", "alg": -7 }],
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

fn sample_eid(domain_id: u16) -> [u8; 16] {
    let mut eid = [0u8; 16];
    eid[0] = 0x00;
    eid[1..11].copy_from_slice(&[0x44u8; 10]);
    eid[11..14].copy_from_slice(&[0x12, 0x34, 0x56]);
    eid[14..16].copy_from_slice(&domain_id.to_le_bytes());
    eid
}

fn fixed_test_scalar(seed: u8) -> [u8; 32] {
    let mut scalar = [0u8; 32];
    scalar[0] = 0x22;
    scalar[31] = seed;
    scalar
}

#[test]
fn live_passkeys_hybrid_qr_completes_make_credential_and_reports_errors() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let rpc = FakeCableRpc::new();
    rpc.enqueue_ok::<tl_fns::account::InitPasskeyRegistration>(&sample_register_options());
    rpc.enqueue_ok::<tl_fns::account::RegisterPasskey>(&tl_enums::Passkey::Passkey(
        tl_types::Passkey {
            id: "pk_cable_phone_01".to_owned(),
            name: "Demo Phone Passkey".to_owned(),
            date: 1_740_000_000,
            software_emoji_id: None,
            last_usage_date: None,
        },
    ));

    let runtime = Arc::new(
        SessionRuntime::start_on(
            DataUserId(1000),
            ErasedRpcClient::new(rpc.clone()),
            rt.handle().clone(),
            no_sync_runtime_config(),
        )
        .expect("start runtime"),
    );
    let bridge = LiveRuntimeBridge::spawn(&runtime, 801);
    let handle = LivePasskeysHandle::with_provider(
        runtime,
        &bridge,
        Arc::new(FakeWebAuthnProvider::new_supported()),
    );

    let scanner = Arc::new(FakeCableBleScanner::new(true));
    let phone = Arc::new(FakeCablePhone::new());
    phone.set_credential_id(b"hybrid-phone-cred-99");
    let tunnel = Arc::new(FakeCableTunnelConnector::unbound(phone.clone()));
    assert!(tunnel.set_responder_ephemeral_bytes(&fixed_test_scalar(0x11)));

    handle.set_cable_scanner(scanner.clone());
    handle.set_cable_tunnel(tunnel.clone());

    let qr_uri = handle
        .start_hybrid_qr_registration()
        .expect("start hybrid qr");
    assert!(qr_uri.starts_with("FIDO:/"));
    assert_eq!(handle.hybrid_qr_status(), PasskeyHybridQrStatus::Scanning);

    let eid = sample_eid(0); // cable.ua5v.com
    let eid_key = handle.active_cable_eid_key().expect("active eid key");
    let qr_secret = handle.active_cable_qr_secret().expect("active qr secret");
    let id_pub = handle
        .active_cable_identity_public_x962()
        .expect("active identity pub");
    tunnel.bind_knpsk0_qr(&qr_secret, &eid, id_pub);

    let advert = cable_encrypt_advert(&eid, &eid_key);
    let delivered = scanner.emit_advertisement(&BleAdvertisement {
        sections: vec![build_cable_service_data_section(
            CableServiceUuid::Fido,
            &advert,
        )],
        bluetooth_address: None,
        rssi: Some(-48),
    });
    assert_eq!(delivered, 1);

    // Wait for background tunnel + account.registerPasskey completion.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if handle.hybrid_qr_status() == PasskeyHybridQrStatus::Completed {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(handle.hybrid_qr_status(), PasskeyHybridQrStatus::Completed);
    let registered = handle
        .last_hybrid_registered()
        .expect("registered passkey entry");
    assert_eq!(registered.id, "pk_cable_phone_01");
    assert!(
        handle
            .passkeys()
            .iter()
            .any(|p| p.id == "pk_cable_phone_01")
    );
    assert_eq!(phone.recorded_make_credentials().len(), 1);
    assert_eq!(tunnel.connected_endpoints().len(), 1);
    assert!(
        tunnel.connected_endpoints()[0]
            .wss_url()
            .starts_with("wss://cable.ua5v.com/cable/connect/123456/")
    );
    assert!(
        rpc.recorded_cids()
            .contains(&tl_fns::account::RegisterPasskey::CONSTRUCTOR_ID)
    );

    // Second ceremony: phone denies operation -> status transitions cleanly to Error.
    rpc.enqueue_ok::<tl_fns::account::InitPasskeyRegistration>(&sample_register_options());
    phone.set_ctap_error(Some(CTAP_ERR_OPERATION_DENIED));
    let _qr2 = handle
        .start_hybrid_qr_registration()
        .expect("start hybrid qr 2");
    let eid2 = sample_eid(1); // cable.auth.com
    let eid_key2 = handle.active_cable_eid_key().expect("active eid key 2");
    let qr_secret2 = handle.active_cable_qr_secret().expect("active qr secret 2");
    let id_pub2 = handle
        .active_cable_identity_public_x962()
        .expect("active identity pub 2");
    tunnel.bind_knpsk0_qr(&qr_secret2, &eid2, id_pub2);

    let advert2 = cable_encrypt_advert(&eid2, &eid_key2);
    scanner.emit_advertisement(&BleAdvertisement {
        sections: vec![build_cable_service_data_section(
            CableServiceUuid::Fido,
            &advert2,
        )],
        bluetooth_address: None,
        rssi: Some(-55),
    });

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if matches!(handle.hybrid_qr_status(), PasskeyHybridQrStatus::Error(_)) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    match handle.hybrid_qr_status() {
        PasskeyHybridQrStatus::Error(msg) => {
            assert!(
                msg.contains("0x27") || msg.to_ascii_lowercase().contains("cancel"),
                "expected clear CTAP denial error, got: {msg}"
            );
        }
        other => panic!("expected Error status after CTAP denial, got {other:?}"),
    }
}

#[test]
fn intro_hybrid_qr_login_completes_get_assertion_and_surfaces_tunnel_errors() {
    let rpc = FakeCableRpc::new();
    let login_opts = tl_enums::auth::PasskeyLoginOptions::PasskeyLoginOptions(
        tl_types::auth::PasskeyLoginOptions {
            options: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                data: serde_json::json!({
                    "publicKey": {
                        "challenge": encode_base64url(b"cable-login-challenge-001"),
                        "rpId": "telegram.org",
                        "timeout": 60000
                    }
                })
                .to_string(),
            }),
        },
    );
    rpc.enqueue_ok::<tl_fns::auth::InitPasskeyLogin>(&login_opts);
    rpc.enqueue_ok::<tl_fns::auth::FinishPasskeyLogin>(
        &tl_enums::auth::Authorization::Authorization(tl_types::auth::Authorization {
            setup_password_required: false,
            otherwise_relogin_days: None,
            tmp_sessions: None,
            future_auth_token: None,
            user: tl_enums::User::User(tl_types::User {
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
                stories_unavailable: true,
                contact_require_premium: false,
                bot_business: false,
                bot_has_main_app: false,
                bot_forum_view: false,
                bot_forum_can_manage_topics: false,
                bot_can_manage_bots: false,
                bot_guestchat: false,
                bot_guard: false,
                bot_forum_sidebar: false,
                id: 777_001,
                access_hash: Some(42),
                first_name: Some("Demo".to_owned()),
                last_name: Some("Account".to_owned()),
                username: Some("demo_cable".to_owned()),
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
            }),
        }),
    );

    let mut intro = IntroWidget::with_rpc_client(
        ApiCredentials {
            api_id: 1,
            api_hash: "test".to_owned(),
        },
        rpc,
    );
    let scanner = Arc::new(FakeCableBleScanner::new(true));
    let phone = Arc::new(FakeCablePhone::new());
    phone.set_credential_id(b"hybrid-login-cred-77");
    phone.set_user_handle(b"2:777001");
    let tunnel = Arc::new(FakeCableTunnelConnector::unbound(phone.clone()));
    assert!(tunnel.set_responder_ephemeral_bytes(&fixed_test_scalar(0x22)));

    intro.set_passkey_provider(Arc::new(FakeWebAuthnProvider::new_supported()));
    intro.set_cable_scanner(scanner.clone());
    intro.set_cable_tunnel(tunnel.clone());

    let now = Instant::now();
    let qr = intro
        .start_passkey_hybrid_login(now)
        .expect("start intro hybrid login");
    assert!(qr.starts_with("FIDO:/"));
    assert_eq!(
        intro.passkey_hybrid_login_status(),
        PasskeyHybridQrStatus::Scanning
    );

    let eid = sample_eid(0);
    let eid_key = intro
        .active_passkey_cable_eid_key()
        .expect("active login eid key");
    let qr_secret = intro
        .active_passkey_cable_qr_secret()
        .expect("active login qr secret");
    let id_pub = intro
        .active_passkey_cable_identity_public_x962()
        .expect("active login id pub");
    tunnel.bind_knpsk0_qr(&qr_secret, &eid, id_pub);

    let advert = cable_encrypt_advert(&eid, &eid_key);
    scanner.emit_advertisement(&BleAdvertisement {
        sections: vec![build_cable_service_data_section(
            CableServiceUuid::Fido,
            &advert,
        )],
        bluetooth_address: None,
        rssi: Some(-50),
    });

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut completed = false;
    while Instant::now() < deadline {
        if intro
            .poll_passkey_hybrid_login(Instant::now())
            .expect("poll hybrid login")
        {
            completed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(completed, "hybrid login ceremony should finish cleanly");
    assert_eq!(
        intro.passkey_hybrid_login_status(),
        PasskeyHybridQrStatus::Completed
    );
    assert_eq!(intro.passkey_authorized_user().map(|u| u.id), Some(777_001));
    assert_eq!(phone.recorded_get_assertions().len(), 1);

    // Verify tunnel connection failure transitions IntroWidget to Error.
    tunnel.set_connect_error(Some(CableTunnelError::Transport(
        "simulated tunnel offline".to_owned(),
    )));
    let err_rpc = FakeCableRpc::new();
    err_rpc.enqueue_ok::<tl_fns::auth::InitPasskeyLogin>(&login_opts);
    let mut intro_err = IntroWidget::with_rpc_client(
        ApiCredentials {
            api_id: 1,
            api_hash: "test".to_owned(),
        },
        err_rpc,
    );
    let scanner2 = Arc::new(FakeCableBleScanner::new(true));
    intro_err.set_passkey_provider(Arc::new(FakeWebAuthnProvider::new_supported()));
    intro_err.set_cable_scanner(scanner2.clone());
    intro_err.set_cable_tunnel(tunnel.clone());
    let _ = intro_err
        .start_passkey_hybrid_login(Instant::now())
        .expect("start err hybrid login");
    let eid_key2 = intro_err.active_passkey_cable_eid_key().unwrap();
    let advert2 = cable_encrypt_advert(&eid, &eid_key2);
    scanner2.emit_advertisement(&BleAdvertisement {
        sections: vec![build_cable_service_data_section(
            CableServiceUuid::Fido,
            &advert2,
        )],
        bluetooth_address: None,
        rssi: Some(-60),
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if matches!(
            intro_err.passkey_hybrid_login_status(),
            PasskeyHybridQrStatus::Error(_)
        ) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    match intro_err.passkey_hybrid_login_status() {
        PasskeyHybridQrStatus::Error(msg) => assert!(msg.contains("simulated tunnel offline")),
        other => panic!("expected Error status on tunnel failure, got {other:?}"),
    }
}
