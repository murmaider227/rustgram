//! Protocol conformance tests for Group 1 (Account/security) in `tg-app-live`
//! using an in-process `FakeRpcClient` (synthetic data only).
//!
//! Covers:
//! 1. `admin.rs`: 2FA password change fresh `srp_id`, Passport `SUGGEST_SECRET_RESET`,
//!    `EMAIL_UNCONFIRMED_<N>`, email-only (`flags=0x02`) update, `resendPasswordEmail`,
//!    `resetPassword`, and `declinePasswordReset` (`api/api_cloud_password.cpp`).
//! 2. `settings_sync.rs`: `account.setGlobalPrivacySettings` always sending
//!    `noncontact_peers_paid_stars` (including `0`) and retrying on
//!    `PREMIUM_ACCOUNT_REQUIRED` (`api/api_global_privacy.cpp`).
//! 3. `settings_sync.rs` + `convert.rs`: `silent == Some(true)` not treated as muted,
//!    `getNotifyExceptions` sending `compare_sound: false`, no fallback to iOS/Android
//!    sound, and preserving `Ringtone`/`Local` sounds (`data/notify/data_*notify_settings.cpp`).
//! 4. `convert.rs` + `session.rs`: `authorization_from_tl` `date_active == 0` -> `date_created`
//!    fallback, single-flag `changeAuthorizationSettings`, `confirmed: true` review path,
//!    and `account.updatePersonalChannel` (`api/api_authorizations.cpp`,
//!    `core/local_url_handlers.cpp`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_crypto::srp::{
    SecureSecretAlgo, compute_secure_secret_hash, count_secure_secret_id, encrypt_secure_secret,
};
use tg_data::{
    Channel, ChannelFlags, ChannelId, PeerId as DataPeerId, PeerUpdateFlags, User,
    UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use crate::convert::{authorization_from_tl, notify_settings_from_tl, notify_settings_to_tl};
use tg_app_core::settings::backend::{
    CloudPasswordError, CloudPasswordResetOutcome, GlobalPrivacySettings, NotifyScope,
    ScopeNotifySettings, SettingsBackend,
};

type RecordedRpc = (u32, Vec<u8>);
type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RecordedRpc>>>,
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

    fn recorded_calls(&self) -> Vec<RecordedRpc> {
        self.recorded.lock().expect("lock recorded").clone()
    }

    fn clear_recorded(&self) {
        self.recorded.lock().expect("lock recorded").clear();
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes(bytes[..4].try_into().unwrap_or([0; 4]));
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, bytes));

        // Requests that tdesktop sends independently (a scope's `getNotifySettings` and
        // `getNotifyExceptions`, TGB-3070) reach the server in either order: the first answer
        // queued for this constructor is taken. A request nothing was queued for still meets
        // the front answer and fails the constructor check below.
        let next = {
            let mut queue = self.responses.lock().expect("lock responses");
            match queue.iter().position(|(expected, _)| *expected == ctor) {
                Some(pos) => queue.remove(pos),
                None => queue.pop_front(),
            }
        };
        match next {
            Some((expected, res)) => {
                assert_eq!(
                    ctor, expected,
                    "unexpected RPC constructor: got 0x{ctor:08x}, expected 0x{expected:08x}"
                );
                let raw = res?;
                R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
            }
            None => Err(InvocationError::Dropped),
        }
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        self.invoke(req).await
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

fn build_live_session() -> (FakeRpcClient, LiveSession) {
    let rpc = FakeRpcClient::new();
    let cfg = SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..SessionRuntimeConfig::default()
    };
    let runtime = Arc::new(
        SessionRuntime::start(
            DataUserId(100),
            crate::ErasedRpcClient::new(rpc.clone()),
            cfg,
        )
        .expect("start SessionRuntime"),
    );
    let live = LiveSession::new(runtime, 0);
    live.write_session(|s| {
        let mut self_user = User::new(DataUserId(100), true);
        self_user.set_name(
            "Self".to_owned(),
            "Fixture".to_owned(),
            String::new(),
            "self_fixture".to_owned(),
        );
        s.upsert_user(self_user, PeerUpdateFlags::empty());
    });
    (rpc, live)
}

/// Safe 2048-bit RFC 3526 Group 14 prime (`p`) and generator `g = 2` for deterministic SRP tests.
fn test_srp_prime() -> Vec<u8> {
    const HEX: &str = concat!(
        "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD1",
        "29024E088A67CC74020BBEA63B139B22514A08798E3404DD",
        "EF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245",
        "E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED",
        "EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3D",
        "C2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F",
        "83655D23DCA3AD961C62F356208552BB9ED529077096966D",
        "670C354E4ABC9804F1746C08CA18217C32905E462E36CE3B",
        "E39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9",
        "DE2BCBF6955817183995497CEA956AE515D2261898FA0510",
        "15728E5A8AACAA68FFFFFFFFFFFFFFFF",
    );
    (0..HEX.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&HEX[i..i + 2], 16).expect("valid hex"))
        .collect()
}

fn synthetic_account_password(
    has_password: bool,
    srp_id: i64,
    hint: Option<&str>,
    unconfirmed_pattern: Option<&str>,
    pending_reset_date: Option<i32>,
) -> tl_enums::account::Password {
    let p = test_srp_prime();
    let algo_tl = tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(
        tl_types::PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow {
            salt1: vec![0x11; 8],
            salt2: vec![0x22; 16],
            g: 2,
            p: p.clone(),
        },
    );
    let mut srp_b = vec![0u8; 256];
    srp_b[255] = 7;

    let new_algo = tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(
        tl_types::PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow {
            salt1: vec![0x33; 8],
            salt2: vec![0x44; 16],
            g: 2,
            p,
        },
    );
    let new_secure_algo = tl_enums::SecurePasswordKdfAlgo::PBKDF2HMACSHA512iter100000(
        tl_types::SecurePasswordKdfAlgoPBKDF2HMACSHA512iter100000 {
            salt: vec![0x55; 8],
        },
    );
    tl_enums::account::Password::Password(tl_types::account::Password {
        has_recovery: false,
        has_secure_values: has_password,
        has_password,
        current_algo: has_password.then_some(algo_tl),
        srp_b: has_password.then_some(srp_b),
        srp_id: has_password.then_some(srp_id),
        hint: hint.map(str::to_owned),
        email_unconfirmed_pattern: unconfirmed_pattern.map(str::to_owned),
        new_algo,
        new_secure_algo,
        secure_random: vec![0x77; 256],
        pending_reset_date,
        login_email_pattern: None,
    })
}

fn synthetic_tl_user(id: i64, first_name: &str) -> tl_types::User {
    tl_types::User {
        self_: false,
        contact: true,
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
        access_hash: Some(id * 100 + 7),
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

// =============================================================================
// 1. Cloud Password (2FA) Conformance (`api/api_cloud_password.cpp`)
// =============================================================================

fn valid_passport_secret(fill: u8) -> [u8; 32] {
    let mut secret = [fill; 32];
    let sum_rest: u32 = secret[1..].iter().map(|&b| u32::from(b)).sum();
    // Telegram Passport secret invariant: `sum(secret) % 255 == 239`.
    secret[0] = ((239 + 255 - (sum_rest % 255)) % 255) as u8;
    secret
}

#[test]
fn password_change_fetches_fresh_srp_id_and_reencrypts_passport_secret() {
    // Port of `CloudPassword::set` (`api/api_cloud_password.cpp:264-315`):
    // changing an existing password must call `getPassword` (srp_id=101) ->
    // `getPasswordSettings` (srp_id=101) -> fresh `getPassword` (srp_id=202) ->
    // `updatePasswordSettings` (srp_id=202) -> refresh `getPassword`.
    let (rpc, mut live) = build_live_session();

    let plain_secret = valid_passport_secret(0xAB);
    let old_sec_algo = SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 {
        salt: vec![0x66; 8],
    };
    let old_hash = compute_secure_secret_hash(&old_sec_algo, b"OldPass123!");
    let encrypted = encrypt_secure_secret(&plain_secret, &old_hash).expect("encrypt secret");
    let secret_id = count_secure_secret_id(&plain_secret) as i64;

    rpc.enqueue_ok(
        tl_fns::account::GetPassword::CONSTRUCTOR_ID,
        &synthetic_account_password(true, 101, Some("old_hint"), None, None),
    );
    rpc.enqueue_ok(
        tl_fns::account::GetPasswordSettings::CONSTRUCTOR_ID,
        &tl_enums::account::PasswordSettings::PasswordSettings(
            tl_types::account::PasswordSettings {
                email: None,
                secure_settings: Some(tl_enums::SecureSecretSettings::SecureSecretSettings(
                    tl_types::SecureSecretSettings {
                        secure_algo: tl_enums::SecurePasswordKdfAlgo::PBKDF2HMACSHA512iter100000(
                            tl_types::SecurePasswordKdfAlgoPBKDF2HMACSHA512iter100000 {
                                salt: vec![0x66; 8],
                            },
                        ),
                        secure_secret: encrypted.to_vec(),
                        secure_secret_id: secret_id,
                    },
                )),
            },
        ),
    );
    // Second `account.getPassword` returns fresh unconsumed `srp_id = 202`.
    rpc.enqueue_ok(
        tl_fns::account::GetPassword::CONSTRUCTOR_ID,
        &synthetic_account_password(true, 202, Some("old_hint"), None, None),
    );
    rpc.enqueue_err(
        tl_fns::account::UpdatePasswordSettings::CONSTRUCTOR_ID,
        InvocationError::Rpc(RpcError {
            code: 400,
            name: "EMAIL_UNCONFIRMED_6".to_owned(),
            value: Some(6),
        }),
    );
    rpc.enqueue_ok(
        tl_fns::account::GetPassword::CONSTRUCTOR_ID,
        &synthetic_account_password(true, 303, Some("new_hint"), Some("r***@example.test"), None),
    );

    let state = live
        .set_cloud_password(
            Some("OldPass123!"),
            "NewPass456!",
            "new_hint",
            Some("recovery@example.test"),
        )
        .expect("set_cloud_password succeeds on EMAIL_UNCONFIRMED_6");

    assert_eq!(state.unconfirmed_code_length, 6);
    assert_eq!(state.unconfirmed_email_pattern, "r***@example.test");
    assert_eq!(state.hint, "new_hint");

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 5);
    assert_eq!(calls[0].0, tl_fns::account::GetPassword::CONSTRUCTOR_ID);
    assert_eq!(
        calls[1].0,
        tl_fns::account::GetPasswordSettings::CONSTRUCTOR_ID
    );
    assert_eq!(calls[2].0, tl_fns::account::GetPassword::CONSTRUCTOR_ID);
    assert_eq!(
        calls[3].0,
        tl_fns::account::UpdatePasswordSettings::CONSTRUCTOR_ID
    );
    assert_eq!(calls[4].0, tl_fns::account::GetPassword::CONSTRUCTOR_ID);

    let get_ps = tl_fns::account::GetPasswordSettings::from_bytes(&calls[1].1).expect("decode");
    let tl_enums::InputCheckPasswordSRP::InputCheckPasswordSRP(srp1) = get_ps.password else {
        panic!("expected SRP check for getPasswordSettings");
    };
    assert_eq!(srp1.srp_id, 101);

    let upd_ps = tl_fns::account::UpdatePasswordSettings::from_bytes(&calls[3].1).expect("decode");
    let tl_enums::InputCheckPasswordSRP::InputCheckPasswordSRP(srp2) = upd_ps.password else {
        panic!("expected SRP check for updatePasswordSettings");
    };
    assert_eq!(
        srp2.srp_id, 202,
        "must use fresh srp_id from 2nd getPassword"
    );

    let tl_enums::account::PasswordInputSettings::PasswordInputSettings(new_s) =
        upd_ps.new_settings;
    assert_eq!(new_s.hint.as_deref(), Some("new_hint"));
    assert_eq!(new_s.email.as_deref(), Some("recovery@example.test"));
    let Some(tl_enums::SecureSecretSettings::SecureSecretSettings(reenc)) =
        new_s.new_secure_settings
    else {
        panic!("expected re-encrypted Passport secure_settings");
    };
    assert_eq!(reenc.secure_secret_id, secret_id);
}

#[test]
fn password_change_passport_secret_mismatch_returns_suggest_secret_reset() {
    // Port of `CloudPassword::set` (`api/api_cloud_password.cpp:290-300`):
    // Passport secret decryption / ID mismatch must surface `SUGGEST_SECRET_RESET`.
    let (rpc, mut live) = build_live_session();

    let plain_secret = valid_passport_secret(0xCD);
    let old_sec_algo = SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 {
        salt: vec![0x66; 8],
    };
    let old_hash = compute_secure_secret_hash(&old_sec_algo, b"OldPass123!");
    let encrypted = encrypt_secure_secret(&plain_secret, &old_hash).expect("encrypt secret");

    rpc.enqueue_ok(
        tl_fns::account::GetPassword::CONSTRUCTOR_ID,
        &synthetic_account_password(true, 101, None, None, None),
    );
    rpc.enqueue_ok(
        tl_fns::account::GetPasswordSettings::CONSTRUCTOR_ID,
        &tl_enums::account::PasswordSettings::PasswordSettings(
            tl_types::account::PasswordSettings {
                email: None,
                secure_settings: Some(tl_enums::SecureSecretSettings::SecureSecretSettings(
                    tl_types::SecureSecretSettings {
                        secure_algo: tl_enums::SecurePasswordKdfAlgo::PBKDF2HMACSHA512iter100000(
                            tl_types::SecurePasswordKdfAlgoPBKDF2HMACSHA512iter100000 {
                                salt: vec![0x66; 8],
                            },
                        ),
                        secure_secret: encrypted.to_vec(),
                        secure_secret_id: 999_999_999, // Wrong ID -> SUGGEST_SECRET_RESET
                    },
                )),
            },
        ),
    );

    let err = live
        .set_cloud_password(Some("OldPass123!"), "NewPass456!", "hint", None)
        .expect_err("must fail with SuggestSecretReset");
    assert_eq!(err, CloudPasswordError::SuggestSecretReset);
    assert_eq!(rpc.recorded_calls().len(), 2);
}

#[test]
fn email_only_update_resend_reset_and_decline_password_reset_wire_format() {
    // Port of `CloudPassword::setEmail`, `resendEmailCode`, `resetPassword`,
    // `cancelResetPassword` (`api/api_cloud_password.cpp:141-185, 376-438`).
    let (rpc, mut live) = build_live_session();

    // 1. Email-only update (`flags = 0x02`, leaving new_algo/new_password_hash/hint None).
    rpc.enqueue_ok(
        tl_fns::account::GetPassword::CONSTRUCTOR_ID,
        &synthetic_account_password(true, 404, Some("h"), None, None),
    );
    rpc.enqueue_err(
        tl_fns::account::UpdatePasswordSettings::CONSTRUCTOR_ID,
        InvocationError::Rpc(RpcError {
            code: 400,
            name: "EMAIL_UNCONFIRMED_6".to_owned(),
            value: Some(6),
        }),
    );
    rpc.enqueue_ok(
        tl_fns::account::GetPassword::CONSTRUCTOR_ID,
        &synthetic_account_password(true, 405, Some("h"), Some("u***@example.test"), None),
    );

    let email_state = live
        .set_recovery_email("MyPass!", "user@example.test")
        .expect("set_recovery_email");
    assert_eq!(email_state.unconfirmed_code_length, 6);
    let calls = rpc.recorded_calls();
    let upd = tl_fns::account::UpdatePasswordSettings::from_bytes(&calls[1].1).expect("decode");
    let tl_enums::account::PasswordInputSettings::PasswordInputSettings(s) = upd.new_settings;
    assert!(s.new_algo.is_none());
    assert!(s.new_password_hash.is_none());
    assert!(s.hint.is_none());
    assert!(s.new_secure_settings.is_none());
    assert_eq!(s.email.as_deref(), Some("user@example.test"));

    // 2. Resend recovery email (`account.resendPasswordEmail` + `account.getPassword`).
    rpc.clear_recorded();
    rpc.enqueue_ok(tl_fns::account::ResendPasswordEmail::CONSTRUCTOR_ID, &true);
    rpc.enqueue_ok(
        tl_fns::account::GetPassword::CONSTRUCTOR_ID,
        &synthetic_account_password(true, 406, Some("h"), Some("u***@example.test"), None),
    );
    let _ = live.resend_recovery_email().expect("resend_recovery_email");
    let calls = rpc.recorded_calls();
    assert_eq!(
        calls[0].0,
        tl_fns::account::ResendPasswordEmail::CONSTRUCTOR_ID
    );
    assert_eq!(calls[1].0, tl_fns::account::GetPassword::CONSTRUCTOR_ID);

    // 3. Reset password (`account.resetPassword` -> RequestedWait) + Decline (`account.declinePasswordReset`).
    rpc.clear_recorded();
    rpc.enqueue_ok(
        tl_fns::account::ResetPassword::CONSTRUCTOR_ID,
        &tl_enums::account::ResetPasswordResult::ResetPasswordRequestedWait(
            tl_types::account::ResetPasswordRequestedWait {
                until_date: 1_800_000_000,
            },
        ),
    );
    rpc.enqueue_ok(
        tl_fns::account::GetPassword::CONSTRUCTOR_ID,
        &synthetic_account_password(true, 407, Some("h"), None, Some(1_800_000_000)),
    );
    let outcome = live.reset_cloud_password().expect("reset_cloud_password");
    assert_eq!(
        outcome,
        CloudPasswordResetOutcome::RequestedWait {
            until_date: 1_800_000_000
        }
    );

    rpc.clear_recorded();
    rpc.enqueue_ok(tl_fns::account::DeclinePasswordReset::CONSTRUCTOR_ID, &true);
    rpc.enqueue_ok(
        tl_fns::account::GetPassword::CONSTRUCTOR_ID,
        &synthetic_account_password(true, 408, Some("h"), None, None),
    );
    let declined = live
        .decline_cloud_password_reset()
        .expect("decline_cloud_password_reset");
    assert_eq!(declined.pending_reset_date, 0);
    let calls = rpc.recorded_calls();
    assert_eq!(
        calls[0].0,
        tl_fns::account::DeclinePasswordReset::CONSTRUCTOR_ID
    );
    assert_eq!(calls[1].0, tl_fns::account::GetPassword::CONSTRUCTOR_ID);
}

// =============================================================================
// 2. Global Privacy Conformance (`api/api_global_privacy.cpp`)
// =============================================================================

#[test]
fn set_global_privacy_always_sends_paid_stars_zero_and_retries_on_premium_required() {
    // Port of `GlobalPrivacy::update` (`api/api_global_privacy.cpp:246,270,280-288`):
    // - always sets `noncontact_peers_paid_stars` (even when `0`),
    // - retries once without premium-only fields when server replies `PREMIUM_ACCOUNT_REQUIRED`.
    let (rpc, mut live) = build_live_session();

    // Case A: zero paid stars must still be serialized as `Some(0)`.
    rpc.enqueue_ok(
        tl_fns::account::SetGlobalPrivacySettings::CONSTRUCTOR_ID,
        &tl_enums::GlobalPrivacySettings::GlobalPrivacySettings(tl_types::GlobalPrivacySettings {
            archive_and_mute_new_noncontact_peers: true,
            keep_archived_unmuted: false,
            keep_archived_folders: false,
            hide_read_marks: false,
            new_noncontact_peers_require_premium: false,
            display_gifts_button: false,
            noncontact_peers_paid_stars: Some(0),
            disallowed_gifts: None,
        }),
    );

    live.set_global_privacy(GlobalPrivacySettings {
        archive_and_mute_new: true,
        noncontact_peers_paid_stars: Some(0),
        ..GlobalPrivacySettings::default()
    });
    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 1);
    let req = tl_fns::account::SetGlobalPrivacySettings::from_bytes(&calls[0].1).expect("decode");
    let tl_enums::GlobalPrivacySettings::GlobalPrivacySettings(inner) = req.settings;
    assert_eq!(inner.noncontact_peers_paid_stars, Some(0));

    // Case B: `PREMIUM_ACCOUNT_REQUIRED` triggers automatic fallback retry.
    rpc.clear_recorded();
    rpc.enqueue_err(
        tl_fns::account::SetGlobalPrivacySettings::CONSTRUCTOR_ID,
        InvocationError::Rpc(RpcError {
            code: 403,
            name: "PREMIUM_ACCOUNT_REQUIRED".to_owned(),
            value: None,
        }),
    );
    rpc.enqueue_ok(
        tl_fns::account::SetGlobalPrivacySettings::CONSTRUCTOR_ID,
        &tl_enums::GlobalPrivacySettings::GlobalPrivacySettings(tl_types::GlobalPrivacySettings {
            archive_and_mute_new_noncontact_peers: true,
            keep_archived_unmuted: false,
            keep_archived_folders: false,
            hide_read_marks: true,
            new_noncontact_peers_require_premium: false,
            display_gifts_button: false,
            noncontact_peers_paid_stars: Some(0),
            disallowed_gifts: None,
        }),
    );

    live.set_global_privacy(GlobalPrivacySettings {
        archive_and_mute_new: true,
        hide_read_time: true,
        new_noncontact_peers_require_premium: true,
        noncontact_peers_paid_stars: Some(75),
        ..GlobalPrivacySettings::default()
    });

    let calls = rpc.recorded_calls();
    assert_eq!(
        calls.len(),
        2,
        "must retry once on PREMIUM_ACCOUNT_REQUIRED"
    );
    let first = tl_fns::account::SetGlobalPrivacySettings::from_bytes(&calls[0].1).expect("decode");
    let tl_enums::GlobalPrivacySettings::GlobalPrivacySettings(s1) = first.settings;
    assert!(s1.new_noncontact_peers_require_premium);
    assert_eq!(s1.noncontact_peers_paid_stars, Some(75));

    let retry = tl_fns::account::SetGlobalPrivacySettings::from_bytes(&calls[1].1).expect("decode");
    let tl_enums::GlobalPrivacySettings::GlobalPrivacySettings(s2) = retry.settings;
    assert!(s2.archive_and_mute_new_noncontact_peers);
    assert!(s2.hide_read_marks);
    assert!(!s2.new_noncontact_peers_require_premium);
    assert_eq!(s2.noncontact_peers_paid_stars, Some(0));
}

// =============================================================================
// 3. Notification Settings Conformance (`data/notify/data_*notify_settings.cpp`)
// =============================================================================

#[test]
fn notify_settings_silent_flag_sounds_and_get_exceptions_wire_format() {
    // Port of `NotifyPeerSettingsValue::change`/`serialize`
    // (`data/notify/data_peer_notify_settings.cpp:42-55, 108-124, 184-202`) and
    // `NotifySettings::loadExceptions` (`data/notify/data_notify_settings.cpp:663-678`).
    let tl_in = tl_types::PeerNotifySettings {
        show_previews: Some(true),
        silent: Some(true), // silent broadcast posts != muted!
        mute_until: Some(0),
        ios_sound: Some(tl_enums::NotificationSound::Ringtone(
            tl_types::NotificationSoundRingtone { id: 999 },
        )),
        android_sound: Some(tl_enums::NotificationSound::None),
        other_sound: None,
        stories_muted: None,
        stories_hide_sender: None,
        stories_ios_sound: None,
        stories_android_sound: None,
        stories_other_sound: None,
    };
    let parsed = notify_settings_from_tl(&tl_in);
    assert!(
        parsed.enabled,
        "silent == Some(true) must NOT treat scope as muted"
    );
    assert_eq!(
        parsed.sound, "Default",
        "must not fall back to ios_sound or android_sound"
    );

    // Ringtone & Local sounds must survive `notify_settings_to_tl` instead of collapsing to Default.
    let ringtone_tl = notify_settings_to_tl(&ScopeNotifySettings {
        enabled: true,
        show_preview: true,
        sound: "Ringtone 12345".to_owned(),
        exceptions: Vec::new(),
    });
    let tl_enums::InputPeerNotifySettings::InputPeerNotifySettings(r_inner) = ringtone_tl;
    assert_eq!(r_inner.silent, None);
    assert_eq!(r_inner.mute_until, Some(0));
    assert_eq!(
        r_inner.sound,
        Some(tl_enums::NotificationSound::Ringtone(
            tl_types::NotificationSoundRingtone { id: 12345 }
        ))
    );

    let local_tl = notify_settings_to_tl(&ScopeNotifySettings {
        enabled: false,
        show_preview: false,
        sound: "Chime".to_owned(),
        exceptions: Vec::new(),
    });
    let tl_enums::InputPeerNotifySettings::InputPeerNotifySettings(l_inner) = local_tl;
    assert_eq!(l_inner.silent, None);
    assert_eq!(l_inner.mute_until, Some(i32::MAX));
    assert_eq!(
        l_inner.sound,
        Some(tl_enums::NotificationSound::Local(
            tl_types::NotificationSoundLocal {
                title: "Chime".to_owned(),
                data: "Chime".to_owned(),
            }
        ))
    );

    // `getNotifyExceptions` must send `compare_sound: false` and not mark `silent: Some(true)`
    // peer as muted.
    let (rpc, live) = build_live_session();
    rpc.enqueue_ok(
        tl_fns::account::GetNotifySettings::CONSTRUCTOR_ID,
        &tl_enums::PeerNotifySettings::PeerNotifySettings(tl_in),
    );
    rpc.enqueue_ok(
        tl_fns::account::GetNotifyExceptions::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::NotifySettings(
                tl_types::UpdateNotifySettings {
                    peer: tl_enums::NotifyPeer::NotifyPeer(tl_types::NotifyPeer {
                        peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 200 }),
                    }),
                    notify_settings: tl_enums::PeerNotifySettings::PeerNotifySettings(
                        tl_types::PeerNotifySettings {
                            show_previews: Some(true),
                            silent: Some(true),
                            mute_until: Some(0),
                            ios_sound: None,
                            android_sound: None,
                            other_sound: Some(tl_enums::NotificationSound::Ringtone(
                                tl_types::NotificationSoundRingtone { id: 77 },
                            )),
                            stories_muted: None,
                            stories_hide_sender: None,
                            stories_ios_sound: None,
                            stories_android_sound: None,
                            stories_other_sound: None,
                        },
                    ),
                },
            )],
            users: vec![tl_enums::User::User(synthetic_tl_user(200, "Peer"))],
            chats: Vec::new(),
            date: 1_750_000_000,
            seq: 1,
        }),
    );

    // The first read answers at once and loads in the background (TGB-3070).
    let _ = live.scope_notifications(NotifyScope::PrivateChats);
    assert!(live.wait_settings_reads(std::time::Duration::from_secs(10)));
    let scope = live.scope_notifications(NotifyScope::PrivateChats);
    assert!(scope.enabled);
    assert_eq!(scope.exceptions.len(), 1);
    assert!(
        !scope.exceptions[0].muted,
        "peer with silent=true and mute_until=0 must not be muted"
    );

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 2);
    let exc_call = calls
        .iter()
        .find(|(ctor, _)| *ctor == tl_fns::account::GetNotifyExceptions::CONSTRUCTOR_ID)
        .expect("getNotifyExceptions was sent");
    let exc_req = tl_fns::account::GetNotifyExceptions::from_bytes(&exc_call.1).expect("decode");
    assert!(
        !exc_req.compare_sound,
        "tdesktop sends compare_sound: false"
    );
    assert!(!exc_req.compare_stories);
    assert_eq!(
        exc_req.peer,
        Some(tl_enums::InputNotifyPeer::InputNotifyUsers)
    );
}

// =============================================================================
// 4. Authorizations & Personal Channel (`api_authorizations.cpp`, `local_url_handlers.cpp`)
// =============================================================================

#[test]
fn authorization_date_fallback_single_flag_change_review_and_personal_channel() {
    // Port of `ParseEntry` (`api/api_authorizations.cpp:76-78`),
    // `toggleCallsDisabled` (`api/api_authorizations.cpp:247-266`),
    // `reviewOne` (`api/api_authorizations.cpp:439-495`), and
    // `UpdatePersonalChannel` (`core/local_url_handlers.cpp:205-218`).
    let auth_zero_active = tl_types::Authorization {
        current: false,
        official_app: true,
        password_pending: false,
        encrypted_requests_disabled: false,
        call_requests_disabled: false,
        unconfirmed: true,
        hash: 777_888,
        device_model: "Desktop".to_owned(),
        platform: "Windows".to_owned(),
        system_version: "11".to_owned(),
        api_id: 2040,
        app_name: "Rustgram".to_owned(),
        app_version: "0.1".to_owned(),
        date_created: 1_700_000_000,
        date_active: 0,
        ip: "127.0.0.1".to_owned(),
        country: "CH".to_owned(),
        region: "ZH".to_owned(),
    };
    let parsed_zero = authorization_from_tl(&auth_zero_active);
    let mut auth_explicit = auth_zero_active.clone();
    auth_explicit.date_active = 1_700_000_000;
    let parsed_explicit = authorization_from_tl(&auth_explicit);
    assert_eq!(
        parsed_zero.active_time, parsed_explicit.active_time,
        "date_active == 0 must fall back to date_created"
    );

    let (rpc, mut live) = build_live_session();
    rpc.enqueue_ok(
        tl_fns::account::GetAuthorizations::CONSTRUCTOR_ID,
        &tl_enums::account::Authorizations::Authorizations(tl_types::account::Authorizations {
            authorization_ttl_days: 180,
            authorizations: vec![tl_enums::Authorization::Authorization(auth_zero_active)],
        }),
    );
    let _ = live.active_sessions();
    assert!(live.wait_settings_reads(std::time::Duration::from_secs(10)));
    let sessions = live.active_sessions();
    assert_eq!(sessions.len(), 1);
    assert!(sessions[0].accept_calls);
    assert!(sessions[0].accept_secret_chats);

    // Toggling ONLY `accept_calls` must send `call_requests_disabled: Some(true)` and
    // `encrypted_requests_disabled: None`.
    rpc.clear_recorded();
    rpc.enqueue_ok(
        tl_fns::account::ChangeAuthorizationSettings::CONSTRUCTOR_ID,
        &true,
    );
    live.update_session_permissions(777_888, false, true);
    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 1);
    let chg =
        tl_fns::account::ChangeAuthorizationSettings::from_bytes(&calls[0].1).expect("decode");
    assert!(!chg.confirmed);
    assert_eq!(chg.hash, 777_888);
    assert_eq!(chg.call_requests_disabled, Some(true));
    assert_eq!(chg.encrypted_requests_disabled, None);

    // Confirming an unreviewed authorization sends `confirmed: true` with both disabled flags None.
    rpc.clear_recorded();
    rpc.enqueue_ok(
        tl_fns::account::ChangeAuthorizationSettings::CONSTRUCTOR_ID,
        &true,
    );
    assert!(
        live.review_authorization(777_888, true)
            .expect("review confirm")
    );
    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 1);
    let rev =
        tl_fns::account::ChangeAuthorizationSettings::from_bytes(&calls[0].1).expect("decode");
    assert!(rev.confirmed);
    assert_eq!(rev.hash, 777_888);
    assert_eq!(rev.call_requests_disabled, None);
    assert_eq!(rev.encrypted_requests_disabled, None);

    // `account.updatePersonalChannel` (set channel, then clear with `InputChannel::Empty`).
    let cid = ChannelId(5001);
    let ch_peer = DataPeerId::from_channel(cid);
    live.write_session(|s| {
        let mut ch = Channel::new(cid);
        ch.access_hash = 987_654;
        ch.flags |= ChannelFlags::BROADCAST;
        s.upsert_channel(ch, PeerUpdateFlags::empty());
    });

    rpc.clear_recorded();
    rpc.enqueue_ok(
        tl_fns::account::UpdatePersonalChannel::CONSTRUCTOR_ID,
        &true,
    );
    rpc.enqueue_ok(
        tl_fns::account::UpdatePersonalChannel::CONSTRUCTOR_ID,
        &true,
    );

    assert!(
        live.update_personal_channel(Some(ch_peer.0))
            .expect("set personal channel")
    );
    assert_eq!(
        live.read_session(|s| s.user(DataUserId(100)).and_then(|u| u.personal_channel_id)),
        Some(cid)
    );

    assert!(
        live.update_personal_channel(None)
            .expect("clear personal channel")
    );
    assert_eq!(
        live.read_session(|s| s.user(DataUserId(100)).and_then(|u| u.personal_channel_id)),
        None
    );

    let calls = rpc.recorded_calls();
    assert_eq!(calls.len(), 2);
    let set_req = tl_fns::account::UpdatePersonalChannel::from_bytes(&calls[0].1).expect("decode");
    assert_eq!(
        set_req.channel,
        tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
            channel_id: 5001,
            access_hash: 987_654,
        })
    );
    let clear_req =
        tl_fns::account::UpdatePersonalChannel::from_bytes(&calls[1].1).expect("decode");
    assert_eq!(clear_req.channel, tl_enums::InputChannel::Empty);
}
