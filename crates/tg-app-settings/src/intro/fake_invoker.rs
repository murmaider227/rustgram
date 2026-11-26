//! Offline deterministic `tg_api::RpcClient` exercising real `tg-tl` wire serialization
//! and server-side `tg-crypto` SRP-6a verification for Intro.
//!
//! Dispatches incoming `tg_tl::RemoteCall` requests by TL constructor ID and serialized
//! bytes, returning generated `tg_tl` responses (codes, resend/call fallback, real SRP-6a
//! 2FA verified against `tg-crypto`, SignUp with `help.termsOfService`, Login Email setup,
//! QR `auth.loginToken` expiry/migration/acceptance, `Updates` broadcast channel, and
//! standard RPC errors such as `PHONE_NUMBER_INVALID`, `PHONE_CODE_INVALID`, and
//! `FLOOD_WAIT_X`) without ever contacting real servers or using real accounts or phone
//! numbers.

use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use num_bigint::BigUint;
use tg_api::rpc::{DcId, InvocationError, RpcClient, RpcError};
use tg_crypto::dh::GOOD_PRIME;
use tg_crypto::hash::{const_time_eq, sha256};
use tg_crypto::srp::{self, PasswordKdfAlgoModPow, compute_digest};
use tg_data::TimeId;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

pub use crate::intro::fake_langpack::FakeLanguageSuggestion;

// --- Synthetic test accounts (never real phone numbers or real people's names) ---

/// Synthetic test phone for standard App -> SMS -> Call code login.
pub const TEST_PHONE_APP: &str = "9996610000";
/// Synthetic test phone for 2FA SRP password login.
pub const TEST_PHONE_2FA: &str = "9996620000";
/// Synthetic test phone for new-account sign-up with Terms of Service.
pub const TEST_PHONE_SIGNUP: &str = "9996630000";
/// Synthetic test phone for mandatory login email setup flow.
pub const TEST_PHONE_EMAIL_SETUP: &str = "9996640000";
/// Synthetic test phone for Fragment anonymous number code flow.
pub const TEST_PHONE_FRAGMENT: &str = "9996650000";
/// Synthetic test phone that returns `PHONE_NUMBER_INVALID`.
pub const TEST_PHONE_INVALID: &str = "9990000000";
/// Synthetic test phone that returns `FLOOD_WAIT_30`.
pub const TEST_PHONE_FLOOD: &str = "9990000420";
/// Synthetic test phone that returns `PHONE_NUMBER_BANNED`.
pub const TEST_PHONE_BANNED: &str = "9990000403";

/// Expected verification code for [`TEST_PHONE_APP`].
pub const TEST_CODE_APP: &str = "12345";
/// Expected verification code for [`TEST_PHONE_2FA`].
pub const TEST_CODE_2FA: &str = "22222";
/// Expected verification code for [`TEST_PHONE_SIGNUP`].
pub const TEST_CODE_SIGNUP: &str = "33333";
/// Expected email verification code for [`TEST_PHONE_EMAIL_SETUP`].
pub const TEST_CODE_EMAIL: &str = "444444";
/// Expected Fragment verification code for [`TEST_PHONE_FRAGMENT`].
pub const TEST_CODE_FRAGMENT: &str = "55555";
/// Special code that triggers `PHONE_CODE_EXPIRED`.
pub const TEST_CODE_EXPIRED: &str = "00000";
/// Special code that triggers `FLOOD_WAIT_45`.
pub const TEST_CODE_FLOOD: &str = "99999";

/// Expected 2FA cloud password verified via real SRP-6a (`tg-crypto`).
pub const TEST_2FA_PASSWORD: &str = "rustgram-2fa";
/// Hint returned for the 2FA demo account.
pub const TEST_2FA_HINT: &str = "project name + 2fa";
/// Masked recovery email pattern returned by `auth.requestPasswordRecovery`.
pub const TEST_2FA_RECOVERY_EMAIL: &str = "t***@example.test";
/// Expected 2FA email recovery code.
pub const TEST_2FA_RECOVERY_CODE: &str = "654321";

/// Backwards-compatible alias for [`FakeRpcClient`].
pub type FakeInvoker = FakeRpcClient;
/// Backwards-compatible alias for [`FakeRpcConfig`].
pub type FakeInvokerConfig = FakeRpcConfig;

/// Configurable QR login behavior for [`FakeRpcClient`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeQrBehavior {
    /// Emit a fresh `tg://login?token=...` valid for `ttl_secs`.
    Pending {
        /// Token lifetime in seconds.
        ttl_secs: i32,
    },
    /// First ask client to migrate to `dc_id`, then authorize on import.
    MigrateThenAccept {
        /// Target data center ID.
        dc_id: DcId,
    },
    /// Accept QR login immediately without 2FA.
    AcceptDirect,
    /// Accept QR scan and require 2FA (`SESSION_PASSWORD_NEEDED`).
    AcceptWith2Fa,
}

/// Configurable account reset behavior for `account.deleteAccount`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeResetBehavior {
    /// Immediately delete account and allow sign-up.
    Immediate,
    /// Require waiting `seconds` (`2FA_CONFIRM_WAIT_<seconds>`).
    ConfirmWait(u32),
    /// Return `2FA_RECENT_CONFIRM`.
    RecentConfirm,
}

/// Configuration for [`FakeRpcClient`].
#[derive(Debug, Clone)]
pub struct FakeRpcConfig {
    /// Nearest DC country ISO2 code returned by `help.getNearestDc`.
    pub nearest_country: String,
    /// Nearest DC ID returned by `help.getNearestDc`.
    pub nearest_dc: DcId,
    /// Expected 2FA cloud password.
    pub password: String,
    /// Password hint.
    pub password_hint: String,
    /// Whether recovery email is configured on the 2FA account.
    pub has_recovery: bool,
    /// Whether Telegram Passport values exist (triggers warning dialog during recovery).
    pub has_secure_values: bool,
    /// Account reset behavior.
    pub reset_behavior: FakeResetBehavior,
    /// Initial QR login behavior.
    pub qr_behavior: FakeQrBehavior,
    /// Token TTL in seconds for generated QR tokens.
    pub qr_ttl_secs: i32,
}

impl Default for FakeRpcConfig {
    fn default() -> Self {
        Self {
            nearest_country: "US".to_owned(),
            nearest_dc: 2,
            password: TEST_2FA_PASSWORD.to_owned(),
            password_hint: TEST_2FA_HINT.to_owned(),
            has_recovery: true,
            has_secure_values: false,
            reset_behavior: FakeResetBehavior::Immediate,
            qr_behavior: FakeQrBehavior::Pending { ttl_secs: 30 },
            qr_ttl_secs: 30,
        }
    }
}

#[derive(Debug)]
struct SrpSession {
    srp_id: i64,
    b_secret: BigUint,
    srp_b_padded: [u8; 256],
}

#[derive(Debug)]
struct FakeServerInner {
    config: FakeRpcConfig,
    /// Precomputed SRP verifier `v = g^x mod p` for `config.password`.
    srp_algo: PasswordKdfAlgoModPow,
    srp_verifier: BigUint,
    active_srp: Option<SrpSession>,
    next_srp_id: i64,
    /// Tracks resend stage per phone (`0` initial, `1` SMS resend, `2+` Call resend).
    resend_count: u32,
    /// Incremented on each QR token export so tokens rotate cleanly.
    qr_counter: u64,
    /// Optional forced next `SentCodeType` for unit tests.
    override_sent_type: Option<tl_enums::auth::SentCodeType>,
    /// Optional forced next RPC error on `auth.sendCode`.
    next_send_code_error: Option<RpcError>,
    /// Optional forced next RPC error on `auth.checkPassword`.
    next_password_error: Option<RpcError>,
    /// Last email entered during email setup.
    pending_setup_email: Option<String>,
    /// Count of `auth.cancelCode` invocations (verified by tests).
    cancel_code_calls: u32,
    /// Whether `auth.finishPasskeyLogin` should require 2FA (`SESSION_PASSWORD_NEEDED`).
    passkey_require_2fa: bool,
    /// Optional forced RPC error on the next passkey login RPC.
    next_passkey_error: Option<RpcError>,
    /// Number of `auth.initPasskeyLogin` calls observed.
    passkey_init_calls: u32,
    /// Last DC ID passed to `auth.finishPasskeyLogin`.
    last_passkey_finish_dc: Option<DcId>,
    /// Language suggested through `help.getConfig` (none by default).
    language_suggestion: Option<FakeLanguageSuggestion>,
}

impl FakeServerInner {
    fn new(config: FakeRpcConfig) -> Self {
        let srp_algo = PasswordKdfAlgoModPow {
            salt1: b"rustgram-fake-salt1-0123456789abcdef".to_vec(),
            salt2: b"rustgram-fake-salt2-fedcba9876543210".to_vec(),
            g: 3,
            p: GOOD_PRIME.to_vec(),
        };
        let verifier_bytes = compute_digest(&srp_algo, config.password.as_bytes())
            .expect("GOOD_PRIME and g=3 are valid SRP parameters");
        let srp_verifier = BigUint::from_bytes_be(&verifier_bytes);

        Self {
            config,
            srp_algo,
            srp_verifier,
            active_srp: None,
            next_srp_id: 1001,
            resend_count: 0,
            qr_counter: 0,
            override_sent_type: None,
            next_send_code_error: None,
            next_password_error: None,
            pending_setup_email: None,
            cancel_code_calls: 0,
            passkey_require_2fa: false,
            next_passkey_error: None,
            passkey_init_calls: 0,
            last_passkey_finish_dc: None,
            language_suggestion: None,
        }
    }

    fn issue_srp_session(&mut self) -> (i64, Vec<u8>) {
        let srp_id = self.next_srp_id;
        self.next_srp_id += 1;

        let p = BigUint::from_bytes_be(&self.srp_algo.p);
        let g = BigUint::from(self.srp_algo.g as u32);
        let p_padded = pad_256(&self.srp_algo.p);
        let g_padded = pad_256(&g.to_bytes_be());
        let k = BigUint::from_bytes_be(&sha256(&[&p_padded, &g_padded]));

        // Deterministic per-session ephemeral server secret `b` derived from `srp_id`.
        let mut b_bytes = [0u8; 256];
        for chunk_idx in 0..8 {
            let block = sha256(&[b"rustgram-srp-b", &srp_id.to_le_bytes(), &[chunk_idx as u8]]);
            b_bytes[chunk_idx * 32..(chunk_idx + 1) * 32].copy_from_slice(&block);
        }
        // Keep `b` comfortably in (1, p)
        b_bytes[0] &= 0x3F;
        b_bytes[255] |= 0x01;
        let b_secret = BigUint::from_bytes_be(&b_bytes);

        let kv = (&k * &self.srp_verifier) % &p;
        let g_b = g.modpow(&b_secret, &p);
        let b_pub = (kv + g_b) % &p;
        let srp_b_padded = pad_256(&b_pub.to_bytes_be());

        self.active_srp = Some(SrpSession {
            srp_id,
            b_secret,
            srp_b_padded,
        });
        (srp_id, srp_b_padded.to_vec())
    }

    fn verify_srp_check(
        &mut self,
        check: &tl_types::InputCheckPasswordSRP,
    ) -> Result<(), RpcError> {
        let Some(session) = self.active_srp.take() else {
            return Err(RpcError::new(400, "SRP_ID_INVALID"));
        };
        if check.srp_id != session.srp_id {
            return Err(RpcError::new(400, "SRP_ID_INVALID"));
        }
        if check.a.len() > 256 || check.m1.len() != 32 {
            return Err(RpcError::new(400, "PASSWORD_HASH_INVALID"));
        }

        let p = BigUint::from_bytes_be(&self.srp_algo.p);
        let g = BigUint::from(self.srp_algo.g as u32);
        let a_pub = BigUint::from_bytes_be(&check.a);
        if a_pub == BigUint::from(0u32) || a_pub >= p {
            return Err(RpcError::new(400, "PASSWORD_HASH_INVALID"));
        }
        let a_padded = pad_256(&check.a);
        let b_padded = session.srp_b_padded;

        // Server-side SRP-6a verification matching Telegram's spec:
        // u = SHA256(pad(A) || pad(B))
        // S = (A * v^u)^b mod p
        // K = SHA256(pad(S))
        // M1 = SHA256((H(p) xor H(g)) || H(salt1) || H(salt2) || pad(A) || pad(B) || K)
        let u = BigUint::from_bytes_be(&sha256(&[&a_padded, &b_padded]));
        if u == BigUint::from(0u32) {
            return Err(RpcError::new(400, "PASSWORD_HASH_INVALID"));
        }
        let v_u = self.srp_verifier.modpow(&u, &p);
        let base = (&a_pub * &v_u) % &p;
        let s = base.modpow(&session.b_secret, &p);
        let s_padded = pad_256(&s.to_bytes_be());
        let k_session = sha256(&[&s_padded]);

        let p_padded = pad_256(&self.srp_algo.p);
        let g_padded = pad_256(&g.to_bytes_be());
        let h_p = sha256(&[&p_padded]);
        let h_g = sha256(&[&g_padded]);
        let mut h_p_xor_h_g = [0u8; 32];
        for i in 0..32 {
            h_p_xor_h_g[i] = h_p[i] ^ h_g[i];
        }
        let h_salt1 = sha256(&[&self.srp_algo.salt1]);
        let h_salt2 = sha256(&[&self.srp_algo.salt2]);
        let expected_m1 = sha256(&[
            &h_p_xor_h_g,
            &h_salt1,
            &h_salt2,
            &a_padded,
            &b_padded,
            &k_session,
        ]);

        if const_time_eq(&expected_m1, &check.m1) {
            Ok(())
        } else {
            Err(RpcError::new(400, "PASSWORD_HASH_INVALID"))
        }
    }

    fn demo_authorization(
        &self,
        phone: &str,
        first_name: &str,
        last_name: Option<String>,
    ) -> tl_enums::auth::Authorization {
        tl_enums::auth::Authorization::Authorization(tl_types::auth::Authorization {
            setup_password_required: false,
            otherwise_relogin_days: None,
            tmp_sessions: None,
            future_auth_token: None,
            user: tl_enums::User::User(demo_self_user(phone, first_name, last_name)),
        })
    }
}

/// Offline `tg_api::RpcClient` simulating all Telegram Intro & Login RPCs over generated `tg-tl` types.
///
/// Automatically implements `tg_api::Invoker` via `tg-api`'s blanket impl for `RpcClient`.
#[derive(Debug)]
pub struct FakeRpcClient {
    dc_id: AtomicI32,
    inner: Mutex<FakeServerInner>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl Default for FakeRpcClient {
    fn default() -> Self {
        Self::new(FakeRpcConfig::default())
    }
}

impl FakeRpcClient {
    /// Creates a new offline `FakeRpcClient` with the given configuration.
    #[must_use]
    pub fn new(config: FakeRpcConfig) -> Self {
        let initial_dc = config.nearest_dc;
        let (updates_tx, _) = broadcast::channel(32);
        Self {
            dc_id: AtomicI32::new(initial_dc),
            inner: Mutex::new(FakeServerInner::new(config)),
            updates_tx,
        }
    }

    /// Overrides the QR login behavior (e.g., to simulate scanning/accepting a QR code).
    pub fn set_qr_behavior(&self, behavior: FakeQrBehavior) {
        if let Ok(mut g) = self.inner.lock() {
            g.config.qr_behavior = behavior;
        }
    }

    /// Overrides the account reset behavior (`Immediate`, `ConfirmWait`, `RecentConfirm`).
    pub fn set_reset_behavior(&self, behavior: FakeResetBehavior) {
        if let Ok(mut g) = self.inner.lock() {
            g.config.reset_behavior = behavior;
        }
    }

    /// Sets whether the 2FA demo account has a recovery email configured.
    pub fn set_has_recovery(&self, has_recovery: bool) {
        if let Ok(mut g) = self.inner.lock() {
            g.config.has_recovery = has_recovery;
        }
    }

    /// Sets whether the 2FA demo account has Telegram Passport data.
    pub fn set_has_secure_values(&self, has_secure_values: bool) {
        if let Ok(mut g) = self.inner.lock() {
            g.config.has_secure_values = has_secure_values;
        }
    }

    /// Forces the next `auth.sendCode` to return a specific generated `SentCodeType`.
    pub fn set_next_sent_code_type(&self, code_type: Option<tl_enums::auth::SentCodeType>) {
        if let Ok(mut g) = self.inner.lock() {
            g.override_sent_type = code_type;
        }
    }

    /// Forces the next `auth.sendCode` to fail with `err`.
    pub fn set_next_send_code_error(&self, err: Option<RpcError>) {
        if let Ok(mut g) = self.inner.lock() {
            g.next_send_code_error = err;
        }
    }

    /// Forces the next `auth.checkPassword` to fail with `err` (e.g. `SRP_ID_INVALID` or `FLOOD_WAIT_15`).
    pub fn set_next_password_error(&self, err: Option<RpcError>) {
        if let Ok(mut g) = self.inner.lock() {
            g.next_password_error = err;
        }
    }

    /// Sets the language `help.getConfig` suggests (`suggested_lang_code`) and the
    /// `langpack.*` answers for it; `None` (the default) suggests no language.
    pub fn set_language_suggestion(&self, suggestion: Option<FakeLanguageSuggestion>) {
        if let Ok(mut g) = self.inner.lock() {
            g.language_suggestion = suggestion;
        }
    }

    /// Pushes a synthetic `tg_tl::enums::Updates` onto the live updates stream (e.g. `Update::LoginToken`).
    pub fn push_update(&self, updates: tl_enums::Updates) {
        let _ = self.updates_tx.send(updates);
    }

    /// Returns the number of `auth.cancelCode` RPCs observed.
    #[must_use]
    pub fn cancel_code_calls(&self) -> u32 {
        self.inner.lock().map_or(0, |g| g.cancel_code_calls)
    }

    /// Returns the number of QR tokens generated so far.
    #[must_use]
    pub fn qr_export_count(&self) -> u64 {
        self.inner.lock().map_or(0, |g| g.qr_counter)
    }

    /// Configures whether `auth.finishPasskeyLogin` requires 2FA (`SESSION_PASSWORD_NEEDED`).
    pub fn set_passkey_require_2fa(&self, require_2fa: bool) {
        if let Ok(mut g) = self.inner.lock() {
            g.passkey_require_2fa = require_2fa;
        }
    }

    /// Forces the next passkey login RPC (`initPasskeyLogin` or `finishPasskeyLogin`) to fail with `err`.
    pub fn set_next_passkey_error(&self, err: Option<RpcError>) {
        if let Ok(mut g) = self.inner.lock() {
            g.next_passkey_error = err;
        }
    }

    /// Returns the number of `auth.initPasskeyLogin` calls observed.
    #[must_use]
    pub fn passkey_init_calls(&self) -> u32 {
        self.inner.lock().map_or(0, |g| g.passkey_init_calls)
    }

    /// Returns the DC ID used for the last `auth.finishPasskeyLogin` call.
    #[must_use]
    pub fn last_passkey_finish_dc(&self) -> Option<DcId> {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.last_passkey_finish_dc)
    }

    fn dispatch_bytes(
        &self,
        dc_id: DcId,
        constructor_id: u32,
        request_bytes: &[u8],
    ) -> Result<Vec<u8>, InvocationError> {
        match constructor_id {
            tl_fns::help::GetNearestDc::CONSTRUCTOR_ID => {
                let g = self.inner.lock().map_err(|_| InvocationError::Dropped)?;
                let resp = tl_enums::NearestDc::NearestDc(tl_types::NearestDc {
                    country: g.config.nearest_country.clone(),
                    this_dc: self.main_dc_id(),
                    nearest_dc: g.config.nearest_dc,
                });
                Ok(resp.to_bytes())
            }
            tl_fns::auth::SendCode::CONSTRUCTOR_ID => {
                let req = decode_req::<tl_fns::auth::SendCode>(request_bytes)?;
                self.handle_send_code(req).map(|r| r.to_bytes())
            }
            tl_fns::auth::ResendCode::CONSTRUCTOR_ID => {
                let req = decode_req::<tl_fns::auth::ResendCode>(request_bytes)?;
                self.handle_resend_code(req).map(|r| r.to_bytes())
            }
            tl_fns::auth::CancelCode::CONSTRUCTOR_ID => {
                let _req = decode_req::<tl_fns::auth::CancelCode>(request_bytes)?;
                let mut g = self.inner.lock().map_err(|_| InvocationError::Dropped)?;
                g.cancel_code_calls += 1;
                Ok(true.to_bytes())
            }
            tl_fns::auth::SignIn::CONSTRUCTOR_ID => {
                let req = decode_req::<tl_fns::auth::SignIn>(request_bytes)?;
                self.handle_sign_in(req).map(|r| r.to_bytes())
            }
            tl_fns::auth::SignUp::CONSTRUCTOR_ID => {
                let req = decode_req::<tl_fns::auth::SignUp>(request_bytes)?;
                self.handle_sign_up(req).map(|r| r.to_bytes())
            }
            tl_fns::account::GetPassword::CONSTRUCTOR_ID => {
                self.handle_get_password().map(|r| r.to_bytes())
            }
            tl_fns::auth::CheckPassword::CONSTRUCTOR_ID => {
                let req = decode_req::<tl_fns::auth::CheckPassword>(request_bytes)?;
                self.handle_check_password(req).map(|r| r.to_bytes())
            }
            tl_fns::auth::RequestPasswordRecovery::CONSTRUCTOR_ID => self
                .handle_request_password_recovery()
                .map(|r| r.to_bytes()),
            tl_fns::auth::CheckRecoveryPassword::CONSTRUCTOR_ID => {
                let req = decode_req::<tl_fns::auth::CheckRecoveryPassword>(request_bytes)?;
                self.handle_check_recovery_password(req)
                    .map(|r| r.to_bytes())
            }
            tl_fns::auth::RecoverPassword::CONSTRUCTOR_ID => {
                let req = decode_req::<tl_fns::auth::RecoverPassword>(request_bytes)?;
                self.handle_recover_password(req).map(|r| r.to_bytes())
            }
            tl_fns::account::SendVerifyEmailCode::CONSTRUCTOR_ID => {
                let req = decode_req::<tl_fns::account::SendVerifyEmailCode>(request_bytes)?;
                self.handle_send_verify_email_code(req)
                    .map(|r| r.to_bytes())
            }
            tl_fns::account::VerifyEmail::CONSTRUCTOR_ID => {
                let req = decode_req::<tl_fns::account::VerifyEmail>(request_bytes)?;
                self.handle_verify_email(req).map(|r| r.to_bytes())
            }
            tl_fns::account::DeleteAccount::CONSTRUCTOR_ID => {
                let _req = decode_req::<tl_fns::account::DeleteAccount>(request_bytes)?;
                self.handle_delete_account().map(|r| r.to_bytes())
            }
            tl_fns::auth::ExportLoginToken::CONSTRUCTOR_ID => {
                let _req = decode_req::<tl_fns::auth::ExportLoginToken>(request_bytes)?;
                self.handle_export_login_token().map(|r| r.to_bytes())
            }
            tl_fns::auth::ImportLoginToken::CONSTRUCTOR_ID => {
                let _req = decode_req::<tl_fns::auth::ImportLoginToken>(request_bytes)?;
                self.handle_import_login_token(dc_id).map(|r| r.to_bytes())
            }
            tl_fns::auth::InitPasskeyLogin::CONSTRUCTOR_ID => {
                let _req = decode_req::<tl_fns::auth::InitPasskeyLogin>(request_bytes)?;
                self.handle_init_passkey_login().map(|r| r.to_bytes())
            }
            tl_fns::auth::FinishPasskeyLogin::CONSTRUCTOR_ID => {
                let req = decode_req::<tl_fns::auth::FinishPasskeyLogin>(request_bytes)?;
                self.handle_finish_passkey_login(dc_id, req)
                    .map(|r| r.to_bytes())
            }
            tl_fns::auth::LogOut::CONSTRUCTOR_ID => {
                let resp = tl_enums::auth::LoggedOut::LoggedOut(tl_types::auth::LoggedOut {
                    future_auth_token: None,
                });
                Ok(resp.to_bytes())
            }
            tl_fns::messages::GetDialogFilters::CONSTRUCTOR_ID => {
                let resp = tl_enums::messages::DialogFilters::DialogFilters(
                    tl_types::messages::DialogFilters {
                        tags_enabled: false,
                        filters: Vec::new(),
                    },
                );
                Ok(resp.to_bytes())
            }
            _ => {
                let suggestion = self
                    .inner
                    .lock()
                    .map_err(|_| InvocationError::Dropped)?
                    .language_suggestion
                    .clone();
                crate::intro::fake_langpack::dispatch(
                    suggestion.as_ref(),
                    self.main_dc_id(),
                    constructor_id,
                    request_bytes,
                )
                .unwrap_or_else(|| {
                    Err(InvocationError::Rpc(RpcError::new(
                        501,
                        "NOT_IMPLEMENTED_IN_FAKE_INTRO",
                    )))
                })
            }
        }
    }

    fn handle_send_code(
        &self,
        req: tl_fns::auth::SendCode,
    ) -> Result<tl_enums::auth::SentCode, InvocationError> {
        let mut g = self.inner.lock().map_err(|_| InvocationError::Dropped)?;
        if let Some(err) = g.next_send_code_error.take() {
            return Err(InvocationError::Rpc(err));
        }

        let phone = digits_only(&req.phone_number);
        if phone.len() < 5 || phone == TEST_PHONE_INVALID {
            return Err(InvocationError::Rpc(RpcError::new(
                400,
                "PHONE_NUMBER_INVALID",
            )));
        }
        if phone == TEST_PHONE_FLOOD {
            return Err(InvocationError::Rpc(RpcError::new(420, "FLOOD_WAIT_30")));
        }
        if phone == TEST_PHONE_BANNED {
            return Err(InvocationError::Rpc(RpcError::new(
                400,
                "PHONE_NUMBER_BANNED",
            )));
        }
        if phone == "9990000429" {
            return Err(InvocationError::Rpc(RpcError::new(
                400,
                "PHONE_NUMBER_FLOOD",
            )));
        }

        g.resend_count = 0;

        if let Some(custom_type) = g.override_sent_type.take() {
            return Ok(tl_enums::auth::SentCode::SentCode(
                tl_types::auth::SentCode {
                    r#type: custom_type,
                    phone_code_hash: format!("hash_{phone}"),
                    next_type: Some(tl_enums::auth::CodeType::Call),
                    timeout: Some(60),
                },
            ));
        }

        let data = match phone.as_str() {
            TEST_PHONE_EMAIL_SETUP => tl_types::auth::SentCode {
                r#type: tl_enums::auth::SentCodeType::SetUpEmailRequired(
                    tl_types::auth::SentCodeTypeSetUpEmailRequired {
                        apple_signin_allowed: false,
                        google_signin_allowed: false,
                    },
                ),
                phone_code_hash: format!("hash_{phone}"),
                next_type: None,
                timeout: None,
            },
            TEST_PHONE_FRAGMENT => tl_types::auth::SentCode {
                r#type: tl_enums::auth::SentCodeType::FragmentSms(
                    tl_types::auth::SentCodeTypeFragmentSms {
                        url: "https://fragment.com/number/88800000000".to_owned(),
                        length: 5,
                    },
                ),
                phone_code_hash: format!("hash_{phone}"),
                next_type: None,
                timeout: None,
            },
            _ => tl_types::auth::SentCode {
                r#type: tl_enums::auth::SentCodeType::App(tl_types::auth::SentCodeTypeApp {
                    length: 5,
                }),
                phone_code_hash: format!("hash_{phone}"),
                next_type: Some(tl_enums::auth::CodeType::Sms),
                timeout: Some(60),
            },
        };
        Ok(tl_enums::auth::SentCode::SentCode(data))
    }

    fn handle_resend_code(
        &self,
        req: tl_fns::auth::ResendCode,
    ) -> Result<tl_enums::auth::SentCode, InvocationError> {
        let mut g = self.inner.lock().map_err(|_| InvocationError::Dropped)?;
        g.resend_count += 1;
        let phone = digits_only(&req.phone_number);
        let data = if g.resend_count == 1 {
            tl_types::auth::SentCode {
                r#type: tl_enums::auth::SentCodeType::Sms(tl_types::auth::SentCodeTypeSms {
                    length: 5,
                }),
                phone_code_hash: req.phone_code_hash,
                next_type: Some(tl_enums::auth::CodeType::Call),
                timeout: Some(60),
            }
        } else {
            tl_types::auth::SentCode {
                r#type: tl_enums::auth::SentCodeType::Call(tl_types::auth::SentCodeTypeCall {
                    length: 5,
                }),
                phone_code_hash: format!("hash_call_{phone}"),
                next_type: None,
                timeout: None,
            }
        };
        Ok(tl_enums::auth::SentCode::SentCode(data))
    }

    fn handle_sign_in(
        &self,
        req: tl_fns::auth::SignIn,
    ) -> Result<tl_enums::auth::Authorization, InvocationError> {
        let g = self.inner.lock().map_err(|_| InvocationError::Dropped)?;
        let phone = digits_only(&req.phone_number);
        let code = req
            .phone_code
            .as_deref()
            .or(match req.email_verification.as_ref() {
                Some(tl_enums::EmailVerification::Code(c)) => Some(c.code.as_str()),
                _ => None,
            })
            .unwrap_or("");

        if code == TEST_CODE_EXPIRED {
            return Err(InvocationError::Rpc(RpcError::new(
                400,
                "PHONE_CODE_EXPIRED",
            )));
        }
        if code == TEST_CODE_FLOOD {
            return Err(InvocationError::Rpc(RpcError::new(420, "FLOOD_WAIT_45")));
        }

        match phone.as_str() {
            TEST_PHONE_2FA => {
                if code != TEST_CODE_2FA {
                    return Err(InvocationError::Rpc(RpcError::new(
                        400,
                        "PHONE_CODE_INVALID",
                    )));
                }
                Err(InvocationError::Rpc(RpcError::new(
                    401,
                    "SESSION_PASSWORD_NEEDED",
                )))
            }
            TEST_PHONE_SIGNUP => {
                if code != TEST_CODE_SIGNUP {
                    return Err(InvocationError::Rpc(RpcError::new(
                        400,
                        "PHONE_CODE_INVALID",
                    )));
                }
                Ok(tl_enums::auth::Authorization::SignUpRequired(
                    tl_types::auth::AuthorizationSignUpRequired {
                        terms_of_service: Some(tl_enums::help::TermsOfService::TermsOfService(
                            tl_types::help::TermsOfService {
                                popup: true,
                                id: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                                    data: "{\"data\":\"rustgram_tos_v1\"}".to_owned(),
                                }),
                                text: "By signing up for Rustgram demo mode, you agree to keep test accounts local and never use real credentials in mock tests.".to_owned(),
                                entities: Vec::new(),
                                min_age_confirm: None,
                            },
                        )),
                    },
                ))
            }
            TEST_PHONE_EMAIL_SETUP => {
                if code != TEST_CODE_EMAIL {
                    return Err(InvocationError::Rpc(RpcError::new(
                        400,
                        "PHONE_CODE_INVALID",
                    )));
                }
                Ok(g.demo_authorization(&phone, "Email", Some("Tester".to_owned())))
            }
            TEST_PHONE_FRAGMENT => {
                if code != TEST_CODE_FRAGMENT {
                    return Err(InvocationError::Rpc(RpcError::new(
                        400,
                        "PHONE_CODE_INVALID",
                    )));
                }
                Ok(g.demo_authorization(&phone, "Fragment", Some("User".to_owned())))
            }
            _ => {
                if code != TEST_CODE_APP {
                    return Err(InvocationError::Rpc(RpcError::new(
                        400,
                        "PHONE_CODE_INVALID",
                    )));
                }
                Ok(g.demo_authorization(&phone, "Demo", Some("Account".to_owned())))
            }
        }
    }

    fn handle_sign_up(
        &self,
        req: tl_fns::auth::SignUp,
    ) -> Result<tl_enums::auth::Authorization, InvocationError> {
        let g = self.inner.lock().map_err(|_| InvocationError::Dropped)?;
        let first = req.first_name.trim();
        if first.is_empty() {
            return Err(InvocationError::Rpc(RpcError::new(
                400,
                "FIRSTNAME_INVALID",
            )));
        }
        let last = req.last_name.trim();
        let last_opt = (!last.is_empty()).then(|| last.to_owned());
        Ok(g.demo_authorization(&req.phone_number, first, last_opt))
    }

    fn handle_get_password(&self) -> Result<tl_enums::account::Password, InvocationError> {
        let mut g = self.inner.lock().map_err(|_| InvocationError::Dropped)?;
        let (srp_id, srp_b) = g.issue_srp_session();
        let current_algo =
            tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(
                tl_types::PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow {
                    salt1: g.srp_algo.salt1.clone(),
                    salt2: g.srp_algo.salt2.clone(),
                    g: g.srp_algo.g,
                    p: g.srp_algo.p.clone(),
                },
            );
        Ok(tl_enums::account::Password::Password(
            tl_types::account::Password {
                has_recovery: g.config.has_recovery,
                has_secure_values: g.config.has_secure_values,
                has_password: true,
                current_algo: Some(current_algo.clone()),
                srp_b: Some(srp_b),
                srp_id: Some(srp_id),
                hint: (!g.config.password_hint.is_empty()).then(|| g.config.password_hint.clone()),
                email_unconfirmed_pattern: None,
                new_algo: current_algo,
                new_secure_algo: tl_enums::SecurePasswordKdfAlgo::Unknown,
                secure_random: vec![0xA5; 32],
                pending_reset_date: None,
                login_email_pattern: None,
            },
        ))
    }

    fn handle_check_password(
        &self,
        req: tl_fns::auth::CheckPassword,
    ) -> Result<tl_enums::auth::Authorization, InvocationError> {
        let mut g = self.inner.lock().map_err(|_| InvocationError::Dropped)?;
        if let Some(err) = g.next_password_error.take() {
            return Err(InvocationError::Rpc(err));
        }
        let check = match req.password {
            tl_enums::InputCheckPasswordSRP::InputCheckPasswordSRP(c) => c,
            tl_enums::InputCheckPasswordSRP::InputCheckPasswordEmpty => {
                return Err(InvocationError::Rpc(RpcError::new(400, "PASSWORD_EMPTY")));
            }
        };
        g.verify_srp_check(&check).map_err(InvocationError::Rpc)?;
        Ok(g.demo_authorization(TEST_PHONE_2FA, "Secure", Some("Account".to_owned())))
    }

    fn handle_request_password_recovery(
        &self,
    ) -> Result<tl_enums::auth::PasswordRecovery, InvocationError> {
        let g = self.inner.lock().map_err(|_| InvocationError::Dropped)?;
        if !g.config.has_recovery {
            return Err(InvocationError::Rpc(RpcError::new(
                400,
                "PASSWORD_RECOVERY_NA",
            )));
        }
        Ok(tl_enums::auth::PasswordRecovery::PasswordRecovery(
            tl_types::auth::PasswordRecovery {
                email_pattern: TEST_2FA_RECOVERY_EMAIL.to_owned(),
            },
        ))
    }

    fn handle_check_recovery_password(
        &self,
        req: tl_fns::auth::CheckRecoveryPassword,
    ) -> Result<bool, InvocationError> {
        let code = req.code.trim();
        if code == "000000" {
            return Err(InvocationError::Rpc(RpcError::new(
                400,
                "PASSWORD_RECOVERY_EXPIRED",
            )));
        }
        if code == TEST_2FA_RECOVERY_CODE {
            Ok(true)
        } else {
            Err(InvocationError::Rpc(RpcError::new(400, "CODE_INVALID")))
        }
    }

    fn handle_recover_password(
        &self,
        req: tl_fns::auth::RecoverPassword,
    ) -> Result<tl_enums::auth::Authorization, InvocationError> {
        let g = self.inner.lock().map_err(|_| InvocationError::Dropped)?;
        if req.code.trim() != TEST_2FA_RECOVERY_CODE {
            return Err(InvocationError::Rpc(RpcError::new(400, "CODE_INVALID")));
        }
        Ok(g.demo_authorization(TEST_PHONE_2FA, "Recovered", Some("Account".to_owned())))
    }

    fn handle_send_verify_email_code(
        &self,
        req: tl_fns::account::SendVerifyEmailCode,
    ) -> Result<tl_enums::account::SentEmailCode, InvocationError> {
        let mut g = self.inner.lock().map_err(|_| InvocationError::Dropped)?;
        let email = req.email.trim();
        if email.eq_ignore_ascii_case("blocked@example.test") {
            return Err(InvocationError::Rpc(RpcError::new(
                400,
                "EMAIL_NOT_ALLOWED",
            )));
        }
        if !email.contains('@') || !email.contains('.') {
            return Err(InvocationError::Rpc(RpcError::new(400, "EMAIL_INVALID")));
        }
        let pattern = mask_email(email);
        g.pending_setup_email = Some(email.to_owned());
        Ok(tl_enums::account::SentEmailCode::SentEmailCode(
            tl_types::account::SentEmailCode {
                email_pattern: pattern,
                length: 6,
            },
        ))
    }

    fn handle_verify_email(
        &self,
        req: tl_fns::account::VerifyEmail,
    ) -> Result<tl_enums::account::EmailVerified, InvocationError> {
        let g = self.inner.lock().map_err(|_| InvocationError::Dropped)?;
        let code = match req.verification {
            tl_enums::EmailVerification::Code(c) => c.code,
            _ => String::new(),
        };
        if code.trim() != TEST_CODE_EMAIL {
            return Err(InvocationError::Rpc(RpcError::new(
                400,
                "PHONE_CODE_INVALID",
            )));
        }
        let phone_code_hash = match req.purpose {
            tl_enums::EmailVerifyPurpose::LoginSetup(setup) => setup.phone_code_hash,
            _ => String::new(),
        };
        let email = g
            .pending_setup_email
            .clone()
            .unwrap_or_else(|| "user@example.test".to_owned());
        let pattern = mask_email(&email);
        Ok(tl_enums::account::EmailVerified::Login(
            tl_types::account::EmailVerifiedLogin {
                email,
                sent_code: tl_enums::auth::SentCode::SentCode(tl_types::auth::SentCode {
                    r#type: tl_enums::auth::SentCodeType::EmailCode(
                        tl_types::auth::SentCodeTypeEmailCode {
                            apple_signin_allowed: false,
                            google_signin_allowed: false,
                            email_pattern: pattern,
                            length: 6,
                            reset_available_period: None,
                            reset_pending_date: None,
                        },
                    ),
                    phone_code_hash,
                    next_type: None,
                    timeout: None,
                }),
            },
        ))
    }

    fn handle_delete_account(&self) -> Result<bool, InvocationError> {
        let g = self.inner.lock().map_err(|_| InvocationError::Dropped)?;
        match g.config.reset_behavior {
            FakeResetBehavior::Immediate => Ok(true),
            FakeResetBehavior::ConfirmWait(secs) => Err(InvocationError::Rpc(RpcError::new(
                400,
                format!("2FA_CONFIRM_WAIT_{secs}"),
            ))),
            FakeResetBehavior::RecentConfirm => Err(InvocationError::Rpc(RpcError::new(
                400,
                "2FA_RECENT_CONFIRM",
            ))),
        }
    }

    fn handle_export_login_token(&self) -> Result<tl_enums::auth::LoginToken, InvocationError> {
        let mut g = self.inner.lock().map_err(|_| InvocationError::Dropped)?;
        g.qr_counter += 1;
        let counter = g.qr_counter;
        match g.config.qr_behavior.clone() {
            FakeQrBehavior::Pending { ttl_secs } => {
                let token = sha256(&[b"rustgram-qr-token", &counter.to_le_bytes()]).to_vec();
                let expires = current_unix_time().saturating_add(ttl_secs.max(1));
                Ok(tl_enums::auth::LoginToken::LoginToken(
                    tl_types::auth::LoginToken { expires, token },
                ))
            }
            FakeQrBehavior::MigrateThenAccept { dc_id } => {
                let token = sha256(&[b"rustgram-qr-migrate", &counter.to_le_bytes()]).to_vec();
                Ok(tl_enums::auth::LoginToken::MigrateTo(
                    tl_types::auth::LoginTokenMigrateTo { dc_id, token },
                ))
            }
            FakeQrBehavior::AcceptDirect => Ok(tl_enums::auth::LoginToken::Success(
                tl_types::auth::LoginTokenSuccess {
                    authorization: g.demo_authorization(
                        "9996600000",
                        "QR",
                        Some("Account".to_owned()),
                    ),
                },
            )),
            FakeQrBehavior::AcceptWith2Fa => Err(InvocationError::Rpc(RpcError::new(
                401,
                "SESSION_PASSWORD_NEEDED",
            ))),
        }
    }

    fn handle_import_login_token(
        &self,
        dc_id: DcId,
    ) -> Result<tl_enums::auth::LoginToken, InvocationError> {
        self.set_main_dc_id(dc_id);
        let g = self.inner.lock().map_err(|_| InvocationError::Dropped)?;
        Ok(tl_enums::auth::LoginToken::Success(
            tl_types::auth::LoginTokenSuccess {
                authorization: g.demo_authorization("9996600000", "QR", Some("Account".to_owned())),
            },
        ))
    }

    fn handle_init_passkey_login(
        &self,
    ) -> Result<tl_enums::auth::PasskeyLoginOptions, InvocationError> {
        let mut g = self.inner.lock().map_err(|_| InvocationError::Dropped)?;
        g.passkey_init_calls += 1;
        if let Some(err) = g.next_passkey_error.take() {
            return Err(InvocationError::Rpc(err));
        }
        let challenge_b64 =
            tg_platform::webauthn::encode_base64url(b"fake-passkey-login-challenge");
        let options_json = serde_json::json!({
            "publicKey": {
                "challenge": challenge_b64,
                "rpId": "telegram.org",
                "timeout": 60000,
                "userVerification": "preferred"
            }
        })
        .to_string();
        Ok(tl_enums::auth::PasskeyLoginOptions::PasskeyLoginOptions(
            tl_types::auth::PasskeyLoginOptions {
                options: tl_enums::DataJSON::DataJSON(tl_types::DataJSON { data: options_json }),
            },
        ))
    }

    fn handle_finish_passkey_login(
        &self,
        dc_id: DcId,
        req: tl_fns::auth::FinishPasskeyLogin,
    ) -> Result<tl_enums::auth::Authorization, InvocationError> {
        let mut g = self.inner.lock().map_err(|_| InvocationError::Dropped)?;
        g.last_passkey_finish_dc = Some(dc_id);
        if let Some(err) = g.next_passkey_error.take() {
            return Err(InvocationError::Rpc(err));
        }
        let tl_enums::InputPasskeyCredential::PublicKey(pk) = req.credential;
        let tl_enums::InputPasskeyResponse::Login(login_resp) = pk.response else {
            return Err(InvocationError::Rpc(RpcError::new(
                400,
                "CREDENTIAL_INVALID",
            )));
        };
        if login_resp.signature.is_empty() || login_resp.authenticator_data.is_empty() {
            return Err(InvocationError::Rpc(RpcError::new(
                400,
                "CREDENTIAL_INVALID",
            )));
        }
        if g.passkey_require_2fa {
            return Err(InvocationError::Rpc(RpcError::new(
                401,
                "SESSION_PASSWORD_NEEDED",
            )));
        }
        Ok(g.demo_authorization("9996600000", "Passkey", Some("Demo".to_owned())))
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    fn main_dc_id(&self) -> DcId {
        self.dc_id.load(Ordering::Acquire)
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        self.dc_id.store(dc_id, Ordering::Release);
    }

    async fn invoke<R: RemoteCall + Send + Sync>(
        &self,
        req: &R,
    ) -> Result<R::Return, InvocationError> {
        self.invoke_in_dc(self.main_dc_id(), req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, InvocationError> {
        let request_bytes = req.to_bytes();
        if request_bytes.len() < 4 {
            return Err(InvocationError::Transport(
                "serialized TL request shorter than 4 bytes".into(),
            ));
        }
        let constructor_id = u32::from_le_bytes([
            request_bytes[0],
            request_bytes[1],
            request_bytes[2],
            request_bytes[3],
        ]);
        let resp_bytes = self.dispatch_bytes(dc_id, constructor_id, &request_bytes)?;
        R::Return::from_bytes(&resp_bytes)
            .map_err(|e| InvocationError::Deserialize(format!("fake intro response decode: {e}")))
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn decode_req<R: Deserializable>(bytes: &[u8]) -> Result<R, InvocationError> {
    R::from_bytes(bytes)
        .map_err(|e| InvocationError::Deserialize(format!("fake intro request decode: {e}")))
}

fn demo_self_user(phone: &str, first_name: &str, last_name: Option<String>) -> tl_types::User {
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
        id: 1000,
        access_hash: Some(4242),
        first_name: Some(first_name.to_owned()),
        last_name,
        username: Some("demo_account".to_owned()),
        phone: Some(phone.to_owned()),
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

fn digits_only(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

fn current_unix_time() -> TimeId {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1_700_000_000, |d| d.as_secs() as TimeId)
}

fn pad_256(bytes: &[u8]) -> [u8; 256] {
    let mut out = [0u8; 256];
    let len = bytes.len().min(256);
    out[256 - len..].copy_from_slice(&bytes[bytes.len() - len..]);
    out
}

fn mask_email(email: &str) -> String {
    let Some((local, domain)) = email.split_once('@') else {
        return "***".to_owned();
    };
    let first = local.chars().next().unwrap_or('*');
    format!("{first}***@{domain}")
}

/// Ensures SRP-6a computation via `tg_crypto::srp` matches the verifier generated in [`FakeRpcClient`].
#[must_use]
pub fn verify_srp_roundtrip(password: &str) -> bool {
    let algo = PasswordKdfAlgoModPow {
        salt1: b"rustgram-fake-salt1-0123456789abcdef".to_vec(),
        salt2: b"rustgram-fake-salt2-fedcba9876543210".to_vec(),
        g: 3,
        p: GOOD_PRIME.to_vec(),
    };
    srp::compute_digest(&algo, password.as_bytes()).is_ok()
}
