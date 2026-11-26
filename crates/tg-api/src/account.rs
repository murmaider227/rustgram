//! Post-login account security, authorizations, notification settings, and account settings sync.
//!
//! Port origin:
//! - `api/api_cloud_password.cpp`, `core/core_cloud_password.cpp` (2FA password & recovery management)
//! - `api/api_authorizations.cpp`, `api/api_websites.cpp` (active sessions & web authorizations)
//! - `data/notify/data_notify_settings.cpp`, `data/notify/data_peer_notify_settings.cpp`,
//!   `apiwrap.cpp` (notification settings & exceptions)
//! - `api/api_global_privacy.cpp`, `api/api_sensitive_content.cpp`, `api/api_self_destruct.cpp`,
//!   `apiwrap.cpp` (`saveSelfBio`), `boxes/add_contact_box.cpp`, `boxes/username_box.cpp`,
//!   `core/local_url_handlers.cpp` (`UpdateBirthday`, `UpdatePersonalChannel`).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use tg_crypto::srp::{self, PasswordKdfAlgoModPow, SecureSecretAlgo};
use tg_data::{
    Birthday, ChannelId, MsgId, Peer, PeerId, PeerNotifySettings, PeerUpdateFlags, Session, TimeId,
    UserFlags, UserId,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::convert::{
    apply_tl_users_and_chats, input_channel_for, input_peer_for, peer_id_from_tl,
    peer_notify_settings_from_tl,
};
use crate::error::ApiError;
use crate::login::CloudPasswordState;
use crate::rpc::{InvocationError, RpcClient};

/// Fallback autoconfirm period (7 days in seconds) for unreviewed authorizations
/// (`kFallbackCount = 604800` in `api/api_authorizations.cpp:97`).
pub const AUTHORIZATION_AUTOCONFIRM_FALLBACK_SECS: TimeId = 604_800;

/// Minimum length of a Telegram username (`Ui::EditPeer::kMinUsernameLength = 5`).
pub const MIN_USERNAME_LENGTH: usize = 5;

/// Default Telegram Desktop official API IDs used for session label formatting
/// (`api/api_authorizations.cpp:28-30`).
pub const TDESKTOP_TEST_API_ID: i32 = 17349;
/// Snap package API ID (`api/api_authorizations.cpp:29`).
pub const TDESKTOP_SNAP_API_ID: i32 = 611335;
/// Official Telegram Desktop API ID (`api/api_authorizations.cpp:30`).
pub const TDESKTOP_API_ID: i32 = 2040;

// ============================================================================
// 1. Cloud Password (2FA) Management (`api/api_cloud_password.cpp`)
// ============================================================================

/// Outcome of setting/updating a 2FA cloud password or recovery email
/// (`Api::CloudPassword::SetOk`, `api/api_cloud_password.cpp:62-76`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetPasswordOutcome {
    /// Expected verification code length when the server returned `EMAIL_UNCONFIRMED_<N>`
    /// (`0` if confirmed immediately or no recovery email confirmation is pending).
    pub unconfirmed_code_length: i32,
    /// Refreshed `CloudPasswordState` fetched via `account.getPassword`.
    pub state: CloudPasswordState,
}

/// Outcome of `account.resetPassword` (`api/api_cloud_password.cpp:141-170`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResetPasswordOutcome {
    /// Password was reset immediately (`account.resetPasswordOk`).
    ResetOk {
        /// Refreshed `CloudPasswordState`.
        state: Box<CloudPasswordState>,
    },
    /// Reset request scheduled with a wait period (`account.resetPasswordRequestedWait`).
    RequestedWait {
        /// Unix timestamp when the reset will complete.
        until_date: TimeId,
    },
    /// Previous reset was cancelled recently; must wait before retrying
    /// (`account.resetPasswordFailedWait`).
    FailedWait {
        /// Unix timestamp when another reset may be requested.
        retry_date: TimeId,
    },
}

/// Optional deterministic randomness hooks for testing exact byte-level SRP and salt generation.
#[derive(Debug, Clone, Copy, Default)]
pub struct CloudPasswordRandom<'a> {
    /// Fixed 32-byte client salt1 extension for `new_algo`.
    pub new_salt1_extra: Option<&'a [u8; srp::ADDITIONAL_SALT_SIZE]>,
    /// Fixed 32-byte client salt extension for `new_secure_algo`.
    pub new_secure_salt_extra: Option<&'a [u8; srp::ADDITIONAL_SALT_SIZE]>,
    /// Fixed 256-byte ephemeral client secret `a` for the first SRP check
    /// (e.g., `account.getPasswordSettings` when changing a password, or the sole check).
    pub first_srp_a: Option<&'a [u8; 256]>,
    /// Fixed 256-byte ephemeral client secret `a` for the second SRP check
    /// (`account.updatePasswordSettings` after `account.getPasswordSettings` + fresh `getPassword`).
    pub second_srp_a: Option<&'a [u8; 256]>,
}

/// Converts a `PasswordKdfAlgoModPow` back to its TL representation
/// (`Core::PrepareCloudPasswordAlgo`, `core/core_cloud_password.cpp:212-222`).
#[must_use]
pub fn modpow_algo_to_tl(algo: &PasswordKdfAlgoModPow) -> tl_enums::PasswordKdfAlgo {
    tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(
        tl_types::PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow {
            salt1: algo.salt1.clone(),
            salt2: algo.salt2.clone(),
            g: algo.g,
            p: algo.p.clone(),
        },
    )
}

/// Parses a TL `SecurePasswordKdfAlgo` into `tg_crypto::srp::SecureSecretAlgo`
/// (`Core::ParseSecureSecretAlgo`, `core/core_cloud_password.cpp:258-270`).
#[must_use]
pub fn parse_secure_secret_algo(
    algo: &tl_enums::SecurePasswordKdfAlgo,
) -> Option<SecureSecretAlgo> {
    match algo {
        tl_enums::SecurePasswordKdfAlgo::PBKDF2HMACSHA512iter100000(s) => {
            Some(SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 {
                salt: s.salt.clone(),
            })
        }
        tl_enums::SecurePasswordKdfAlgo::SHA512(s) => Some(SecureSecretAlgo::Sha512 {
            salt: s.salt.clone(),
        }),
        tl_enums::SecurePasswordKdfAlgo::Unknown => None,
    }
}

/// Converts a `SecureSecretAlgo` back to TL (`Core::PrepareSecureSecretAlgo`,
/// `core/core_cloud_password.cpp:283-293`).
#[must_use]
pub fn secure_secret_algo_to_tl(algo: &SecureSecretAlgo) -> tl_enums::SecurePasswordKdfAlgo {
    match algo {
        SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 { salt } => {
            tl_enums::SecurePasswordKdfAlgo::PBKDF2HMACSHA512iter100000(
                tl_types::SecurePasswordKdfAlgoPBKDF2HMACSHA512iter100000 { salt: salt.clone() },
            )
        }
        SecureSecretAlgo::Sha512 { salt } => {
            tl_enums::SecurePasswordKdfAlgo::SHA512(tl_types::SecurePasswordKdfAlgoSHA512 {
                salt: salt.clone(),
            })
        }
    }
}

fn parse_password_state_with_random(
    data: &tl_types::account::Password,
    salt1_extra: Option<&[u8; srp::ADDITIONAL_SALT_SIZE]>,
    secure_salt_extra: Option<&[u8; srp::ADDITIONAL_SALT_SIZE]>,
) -> CloudPasswordState {
    let mut state = CloudPasswordState::from_account_password(data);
    if let Some(extra) = salt1_extra
        && let tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(m) =
            &data.new_algo
    {
        let mut salt1 = m.salt1.clone();
        salt1.extend_from_slice(extra);
        state.new_algo = Some(Box::new(PasswordKdfAlgoModPow {
            salt1,
            salt2: m.salt2.clone(),
            g: m.g,
            p: m.p.clone(),
        }));
    }
    if let Some(extra) = secure_salt_extra
        && let tl_enums::SecurePasswordKdfAlgo::PBKDF2HMACSHA512iter100000(s) =
            &data.new_secure_algo
    {
        let mut salt = s.salt.clone();
        salt.extend_from_slice(extra);
        state.new_secure_algo = Some(Box::new(SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 {
            salt,
        }));
    }
    state
}

fn generate_password_check(
    password: &str,
    state: &CloudPasswordState,
    fixed_a: Option<&[u8; 256]>,
) -> Result<tl_enums::InputCheckPasswordSRP, ApiError> {
    // Match `GeneratePasswordCheck` (`api/api_cloud_password.cpp:30-45`):
    // return `inputCheckPasswordEmpty` when password is empty or account has no password.
    if password.is_empty() || !state.has_password {
        return Ok(tl_enums::InputCheckPasswordSRP::InputCheckPasswordEmpty);
    }
    let algo = state
        .current_algo
        .as_ref()
        .ok_or_else(|| ApiError::InvalidState("missing current password KDF algo".to_owned()))?;
    let hash = srp::compute_hash(algo, password.as_bytes());
    let check = match fixed_a {
        Some(a_bytes) => {
            srp::compute_check_with_random(algo, &state.srp_b, state.srp_id, &hash, a_bytes)?
        }
        None => srp::compute_check_with_hash(algo, &state.srp_b, state.srp_id, &hash)?,
    };
    Ok(tl_enums::InputCheckPasswordSRP::InputCheckPasswordSRP(
        tl_types::InputCheckPasswordSRP {
            srp_id: check.srp_id,
            a: check.a.to_vec(),
            m1: check.m1.to_vec(),
        },
    ))
}

fn parse_email_unconfirmed_length(rpc_name: &str) -> Option<i32> {
    let rest = rpc_name.strip_prefix("EMAIL_UNCONFIRMED_")?;
    Some(rest.parse::<i32>().unwrap_or(0))
}

fn map_rpc<E: Into<InvocationError>>(err: E) -> ApiError {
    ApiError::Invocation(err.into())
}

/// Post-login 2FA cloud password manager (`Api::CloudPassword`, `api/api_cloud_password.cpp`).
#[derive(Debug, Default, Clone)]
pub struct CloudPasswordManager {
    state: Option<CloudPasswordState>,
}

impl CloudPasswordManager {
    /// Creates a new manager with no loaded state.
    #[must_use]
    pub const fn new() -> Self {
        Self { state: None }
    }

    /// Returns the last loaded `CloudPasswordState`, if any.
    #[must_use]
    pub fn state(&self) -> Option<&CloudPasswordState> {
        self.state.as_ref()
    }

    /// Reloads cloud password state via `account.getPassword` (`CloudPassword::reload`).
    pub async fn reload<C: RpcClient>(&mut self, rpc: &C) -> Result<CloudPasswordState, ApiError> {
        let tl_enums::account::Password::Password(pwd) = rpc
            .invoke(&tl_fns::account::GetPassword)
            .await
            .map_err(map_rpc)?;
        let parsed = CloudPasswordState::from_account_password(&pwd);
        self.state = Some(parsed.clone());
        Ok(parsed)
    }

    /// Checks a password via `account.getPassword` + `account.getPasswordSettings`
    /// (`CloudPassword::check`, `api/api_cloud_password.cpp:332-353`).
    pub async fn check_password<C: RpcClient>(
        &mut self,
        rpc: &C,
        password: &str,
    ) -> Result<tl_types::account::PasswordSettings, ApiError> {
        self.check_password_with_random(rpc, password, None).await
    }

    /// Deterministic variant of [`Self::check_password`] for known-answer SRP tests.
    pub async fn check_password_with_random<C: RpcClient>(
        &mut self,
        rpc: &C,
        password: &str,
        fixed_a: Option<&[u8; 256]>,
    ) -> Result<tl_types::account::PasswordSettings, ApiError> {
        let tl_enums::account::Password::Password(pwd) = rpc
            .invoke(&tl_fns::account::GetPassword)
            .await
            .map_err(map_rpc)?;
        let latest = CloudPasswordState::from_account_password(&pwd);
        let input = generate_password_check(password, &latest, fixed_a)?;
        let tl_enums::account::PasswordSettings::PasswordSettings(settings) = rpc
            .invoke(&tl_fns::account::GetPasswordSettings { password: input })
            .await
            .map_err(map_rpc)?;
        Ok(settings)
    }

    /// Sets, changes, or removes (`new_password == ""`) the 2FA cloud password
    /// (`CloudPassword::set`, `api/api_cloud_password.cpp:187-330`).
    ///
    /// When changing an existing password (`has_password && !old_password.is_empty() && !new_password.is_empty()`),
    /// matches tdesktop's two-step SRP flow:
    /// 1. `account.getPassword` -> `account.getPasswordSettings(SRP)` to decrypt & re-encrypt
    ///    any Telegram Passport `secure_settings` (failing with `"SUGGEST_SECRET_RESET"` on mismatch),
    /// 2. second `account.getPassword` to obtain a fresh, unconsumed `srp_id`/`srp_B` before
    ///    calling `account.updatePasswordSettings` with `handleFloodErrors()`.
    pub async fn set_password<C: RpcClient>(
        &mut self,
        rpc: &C,
        old_password: &str,
        new_password: &str,
        hint: &str,
        has_recovery_email: bool,
        recovery_email: &str,
    ) -> Result<SetPasswordOutcome, ApiError> {
        self.set_password_with_random(
            rpc,
            old_password,
            new_password,
            hint,
            has_recovery_email,
            recovery_email,
            CloudPasswordRandom::default(),
        )
        .await
    }

    /// Deterministic variant of [`Self::set_password`] for byte-exact conformance testing.
    #[allow(clippy::too_many_arguments)]
    pub async fn set_password_with_random<C: RpcClient>(
        &mut self,
        rpc: &C,
        old_password: &str,
        new_password: &str,
        hint: &str,
        has_recovery_email: bool,
        recovery_email: &str,
        random: CloudPasswordRandom<'_>,
    ) -> Result<SetPasswordOutcome, ApiError> {
        let tl_enums::account::Password::Password(pwd) = rpc
            .invoke(&tl_fns::account::GetPassword)
            .await
            .map_err(map_rpc)?;
        let mut latest = parse_password_state_with_random(
            &pwd,
            random.new_salt1_extra,
            random.new_secure_salt_extra,
        );

        let mut plain_passport_secret: Option<[u8; 32]> = None;
        let mut srp_a_for_update = random.first_srp_a;

        // Match `api/api_cloud_password.cpp:264-319`:
        // when changing an existing password, first fetch `account.getPasswordSettings`
        // to preserve/re-encrypt Passport secure secret, then fetch a fresh `account.getPassword`
        // so `account.updatePasswordSettings` uses an unconsumed `srp_id`.
        if latest.has_password && !old_password.is_empty() && !new_password.is_empty() {
            let check_for_settings =
                generate_password_check(old_password, &latest, random.first_srp_a)?;
            let tl_enums::account::PasswordSettings::PasswordSettings(ps) = rpc
                .invoke(&tl_fns::account::GetPasswordSettings {
                    password: check_for_settings,
                })
                .await
                .map_err(map_rpc)?;
            if let Some(tl_enums::SecureSecretSettings::SecureSecretSettings(sec)) =
                ps.secure_settings
            {
                let old_algo = parse_secure_secret_algo(&sec.secure_algo)
                    .ok_or_else(|| ApiError::ServerError("SUGGEST_SECRET_RESET".to_owned()))?;
                let old_hash = srp::compute_secure_secret_hash(&old_algo, old_password.as_bytes());
                let secret = srp::decrypt_secure_secret(&sec.secure_secret, &old_hash)
                    .map_err(|_| ApiError::ServerError("SUGGEST_SECRET_RESET".to_owned()))?;
                if srp::count_secure_secret_id(&secret) != sec.secure_secret_id as u64 {
                    return Err(ApiError::ServerError("SUGGEST_SECRET_RESET".to_owned()));
                }
                plain_passport_secret = Some(secret);
            }
            let tl_enums::account::Password::Password(fresh_pwd) = rpc
                .invoke(&tl_fns::account::GetPassword)
                .await
                .map_err(map_rpc)?;
            latest = parse_password_state_with_random(
                &fresh_pwd,
                random.new_salt1_extra,
                random.new_secure_salt_extra,
            );
            srp_a_for_update = random.second_srp_a;
        }

        let (new_algo_tl, new_password_hash) = if new_password.is_empty() {
            (tl_enums::PasswordKdfAlgo::Unknown, Vec::new())
        } else {
            let new_algo = latest
                .new_algo
                .as_ref()
                .ok_or_else(|| ApiError::ServerError("INTERNAL_SERVER_ERROR".to_owned()))?;
            let digest = srp::compute_digest(new_algo, new_password.as_bytes())
                .map_err(|_| ApiError::ServerError("INTERNAL_SERVER_ERROR".to_owned()))?;
            (modpow_algo_to_tl(new_algo), digest.to_vec())
        };

        let new_secure_settings = if let Some(secret) = plain_passport_secret {
            let new_sec_algo = latest
                .new_secure_algo
                .as_ref()
                .ok_or_else(|| ApiError::ServerError("INTERNAL_SERVER_ERROR".to_owned()))?;
            let new_sec_hash =
                srp::compute_secure_secret_hash(new_sec_algo, new_password.as_bytes());
            let encrypted = srp::encrypt_secure_secret(&secret, &new_sec_hash)?;
            let secret_id = srp::count_secure_secret_id(&secret) as i64;
            Some(tl_enums::SecureSecretSettings::SecureSecretSettings(
                tl_types::SecureSecretSettings {
                    secure_algo: secure_secret_algo_to_tl(new_sec_algo),
                    secure_secret: encrypted.to_vec(),
                    secure_secret_id: secret_id,
                },
            ))
        } else {
            None
        };

        let current_check = generate_password_check(old_password, &latest, srp_a_for_update)?;
        let req = tl_fns::account::UpdatePasswordSettings {
            password: current_check,
            new_settings: tl_enums::account::PasswordInputSettings::PasswordInputSettings(
                tl_types::account::PasswordInputSettings {
                    new_algo: Some(new_algo_tl),
                    new_password_hash: Some(new_password_hash),
                    hint: Some(hint.to_owned()),
                    email: has_recovery_email.then(|| recovery_email.to_owned()),
                    new_secure_settings,
                },
            ),
        };

        let code_length = match rpc.invoke_handle_flood(&req).await {
            Ok(_) => 0,
            Err(err) => {
                let mapped: InvocationError = err.into();
                if let InvocationError::Rpc(ref rpc_err) = mapped
                    && let Some(len) = parse_email_unconfirmed_length(&rpc_err.name)
                {
                    len
                } else {
                    return Err(ApiError::Invocation(mapped));
                }
            }
        };

        let tl_enums::account::Password::Password(refreshed_tl) = rpc
            .invoke_handle_flood(&tl_fns::account::GetPassword)
            .await
            .map_err(map_rpc)?;
        let refreshed = CloudPasswordState::from_account_password(&refreshed_tl);
        self.state = Some(refreshed.clone());
        Ok(SetPasswordOutcome {
            unconfirmed_code_length: code_length,
            state: refreshed,
        })
    }

    /// Updates only the recovery email (`flags = f_email` only, leaving password untouched)
    /// (`CloudPassword::setEmail`, `api/api_cloud_password.cpp:395-438`).
    pub async fn set_recovery_email<C: RpcClient>(
        &mut self,
        rpc: &C,
        old_password: &str,
        recovery_email: &str,
        fixed_a: Option<&[u8; 256]>,
    ) -> Result<SetPasswordOutcome, ApiError> {
        let tl_enums::account::Password::Password(pwd) = rpc
            .invoke(&tl_fns::account::GetPassword)
            .await
            .map_err(map_rpc)?;
        let latest = CloudPasswordState::from_account_password(&pwd);
        let current_check = generate_password_check(old_password, &latest, fixed_a)?;

        let req = tl_fns::account::UpdatePasswordSettings {
            password: current_check,
            new_settings: tl_enums::account::PasswordInputSettings::PasswordInputSettings(
                tl_types::account::PasswordInputSettings {
                    new_algo: None,
                    new_password_hash: None,
                    hint: None,
                    email: Some(recovery_email.to_owned()),
                    new_secure_settings: None,
                },
            ),
        };

        let code_length = match rpc.invoke_handle_flood(&req).await {
            Ok(_) => 0,
            Err(err) => {
                let mapped: InvocationError = err.into();
                if let InvocationError::Rpc(ref rpc_err) = mapped
                    && let Some(len) = parse_email_unconfirmed_length(&rpc_err.name)
                {
                    len
                } else {
                    return Err(ApiError::Invocation(mapped));
                }
            }
        };

        let tl_enums::account::Password::Password(refreshed_tl) = rpc
            .invoke_handle_flood(&tl_fns::account::GetPassword)
            .await
            .map_err(map_rpc)?;
        let refreshed = CloudPasswordState::from_account_password(&refreshed_tl);
        self.state = Some(refreshed.clone());
        Ok(SetPasswordOutcome {
            unconfirmed_code_length: code_length,
            state: refreshed,
        })
    }

    /// Confirms a pending 2FA recovery email (`account.confirmPasswordEmail`,
    /// `api/api_cloud_password.cpp:355-374`).
    pub async fn confirm_password_email<C: RpcClient>(
        &mut self,
        rpc: &C,
        code: &str,
    ) -> Result<CloudPasswordState, ApiError> {
        let _ = rpc
            .invoke_handle_flood(&tl_fns::account::ConfirmPasswordEmail {
                code: code.to_owned(),
            })
            .await
            .map_err(map_rpc)?;
        self.reload(rpc).await
    }

    /// Resends the 2FA recovery email confirmation code (`account.resendPasswordEmail`,
    /// `api/api_cloud_password.cpp:376-393`).
    pub async fn resend_password_email<C: RpcClient>(
        &mut self,
        rpc: &C,
    ) -> Result<CloudPasswordState, ApiError> {
        let _ = rpc
            .invoke_handle_flood(&tl_fns::account::ResendPasswordEmail)
            .await
            .map_err(map_rpc)?;
        self.reload(rpc).await
    }

    /// Cancels a pending unconfirmed 2FA recovery email (`account.cancelPasswordEmail`,
    /// `api/api_cloud_password.cpp:109-126`). Always reloads state afterward.
    pub async fn cancel_password_email<C: RpcClient>(
        &mut self,
        rpc: &C,
    ) -> Result<CloudPasswordState, ApiError> {
        let _ = rpc.invoke(&tl_fns::account::CancelPasswordEmail).await;
        self.reload(rpc).await
    }

    /// Requests a 2FA password recovery email code (`auth.requestPasswordRecovery`,
    /// `api/api_cloud_password.cpp:509-522`).
    pub async fn request_password_recovery<C: RpcClient>(
        &self,
        rpc: &C,
    ) -> Result<String, ApiError> {
        let tl_enums::auth::PasswordRecovery::PasswordRecovery(rec) = rpc
            .invoke(&tl_fns::auth::RequestPasswordRecovery)
            .await
            .map_err(map_rpc)?;
        Ok(rec.email_pattern)
    }

    /// Checks a 2FA password recovery email code (`auth.checkRecoveryPassword`,
    /// `api/api_cloud_password.cpp:524-537`).
    pub async fn check_recovery_password<C: RpcClient>(
        &self,
        rpc: &C,
        code: &str,
    ) -> Result<bool, ApiError> {
        let ok = rpc
            .invoke_handle_flood(&tl_fns::auth::CheckRecoveryPassword {
                code: code.to_owned(),
            })
            .await
            .map_err(map_rpc)?;
        Ok(ok)
    }

    /// Recovers the 2FA password using a verified code, optionally setting a new password & hint
    /// (`CloudPassword::recoverPassword`, `api/api_cloud_password.cpp:440-507`).
    pub async fn recover_password<C: RpcClient>(
        &mut self,
        rpc: &C,
        code: &str,
        new_password: &str,
        new_hint: &str,
        new_salt1_extra: Option<&[u8; srp::ADDITIONAL_SALT_SIZE]>,
    ) -> Result<CloudPasswordState, ApiError> {
        let tl_enums::account::Password::Password(pwd) = rpc
            .invoke(&tl_fns::account::GetPassword)
            .await
            .map_err(map_rpc)?;
        let latest = parse_password_state_with_random(&pwd, new_salt1_extra, None);

        let new_settings = if new_password.is_empty() {
            None
        } else {
            let new_algo = latest
                .new_algo
                .as_ref()
                .ok_or_else(|| ApiError::ServerError("INTERNAL_SERVER_ERROR".to_owned()))?;
            let digest = srp::compute_digest(new_algo, new_password.as_bytes())
                .map_err(|_| ApiError::ServerError("INTERNAL_SERVER_ERROR".to_owned()))?;
            Some(
                tl_enums::account::PasswordInputSettings::PasswordInputSettings(
                    tl_types::account::PasswordInputSettings {
                        new_algo: Some(modpow_algo_to_tl(new_algo)),
                        new_password_hash: Some(digest.to_vec()),
                        hint: Some(new_hint.to_owned()),
                        email: None,
                        new_secure_settings: None,
                    },
                ),
            )
        };

        let _ = rpc
            .invoke_handle_flood(&tl_fns::auth::RecoverPassword {
                code: code.to_owned(),
                new_settings,
            })
            .await
            .map_err(map_rpc)?;

        let tl_enums::account::Password::Password(refreshed_tl) = rpc
            .invoke_handle_flood(&tl_fns::account::GetPassword)
            .await
            .map_err(map_rpc)?;
        let refreshed = CloudPasswordState::from_account_password(&refreshed_tl);
        self.state = Some(refreshed.clone());
        Ok(refreshed)
    }

    /// Initiates or completes a wait-period 2FA password reset (`account.resetPassword`,
    /// `api/api_cloud_password.cpp:141-170`).
    pub async fn reset_password<C: RpcClient>(
        &mut self,
        rpc: &C,
    ) -> Result<ResetPasswordOutcome, ApiError> {
        let result = rpc
            .invoke(&tl_fns::account::ResetPassword)
            .await
            .map_err(map_rpc)?;
        match result {
            tl_enums::account::ResetPasswordResult::ResetPasswordOk => {
                let state = self.reload(rpc).await?;
                Ok(ResetPasswordOutcome::ResetOk {
                    state: Box::new(state),
                })
            }
            tl_enums::account::ResetPasswordResult::ResetPasswordRequestedWait(wait) => {
                if let Some(st) = &mut self.state {
                    st.pending_reset_date = wait.until_date;
                } else {
                    let _ = self.reload(rpc).await;
                }
                Ok(ResetPasswordOutcome::RequestedWait {
                    until_date: wait.until_date,
                })
            }
            tl_enums::account::ResetPasswordResult::ResetPasswordFailedWait(wait) => {
                Ok(ResetPasswordOutcome::FailedWait {
                    retry_date: wait.retry_date,
                })
            }
        }
    }

    /// Declines a pending 2FA password reset (`account.declinePasswordReset`,
    /// `api/api_cloud_password.cpp:172-185`).
    pub async fn decline_password_reset<C: RpcClient>(
        &mut self,
        rpc: &C,
    ) -> Result<CloudPasswordState, ApiError> {
        let _ = rpc
            .invoke(&tl_fns::account::DeclinePasswordReset)
            .await
            .map_err(map_rpc)?;
        self.reload(rpc).await
    }
}

// ============================================================================
// 2. Active Sessions & Web Authorizations (`api_authorizations.cpp`, `api_websites.cpp`)
// ============================================================================

/// Parsed active Telegram session entry (`Api::Authorizations::Entry`,
/// `api/api_authorizations.cpp:32-89`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationEntry {
    /// Session hash (`0` for the current session per `api_authorizations.cpp:35`).
    pub hash: i64,
    /// True if awaiting 2FA password (`password_pending`).
    pub incomplete: bool,
    /// True if session is still unconfirmed (`unconfirmed`).
    pub unconfirmed: bool,
    /// True if incoming calls are disabled on this session (`call_requests_disabled`).
    pub calls_disabled: bool,
    /// True if secret chats are disabled on this session (`encrypted_requests_disabled`).
    pub encrypted_disabled: bool,
    /// True if official client (`official_app`).
    pub official_app: bool,
    /// Client `api_id`.
    pub api_id: i32,
    /// Device model (overridden by local device model when `hash == 0` in tdesktop).
    pub device_model: String,
    /// Platform name.
    pub platform: String,
    /// System version string.
    pub system_version: String,
    /// Application name.
    pub app_name: String,
    /// Application version string.
    pub app_version: String,
    /// Last active timestamp (`date_active` if non-zero, else `date_created`).
    pub active_time: TimeId,
    /// Session creation timestamp.
    pub date_created: TimeId,
    /// IP address.
    pub ip: String,
    /// Country string.
    pub country: String,
    /// Region string.
    pub region: String,
}

impl AuthorizationEntry {
    /// Parses TL `Authorization` matching `ParseEntry` (`api/api_authorizations.cpp:32-89`).
    #[must_use]
    pub fn from_tl(data: &tl_types::Authorization) -> Self {
        let hash = if data.current { 0 } else { data.hash };
        let active_time = if data.date_active != 0 {
            data.date_active
        } else {
            data.date_created
        };
        Self {
            hash,
            incomplete: data.password_pending,
            unconfirmed: data.unconfirmed,
            calls_disabled: data.call_requests_disabled,
            encrypted_disabled: data.encrypted_requests_disabled,
            official_app: data.official_app,
            api_id: data.api_id,
            device_model: data.device_model.clone(),
            platform: data.platform.clone(),
            system_version: data.system_version.clone(),
            app_name: data.app_name.clone(),
            app_version: data.app_version.clone(),
            active_time,
            date_created: data.date_created,
            ip: data.ip.clone(),
            country: data.country.clone(),
            region: data.region.clone(),
        }
    }
}

/// Unreviewed login session from `updateNewAuthorization` (`Data::UnreviewedAuth`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreviewedAuthorization {
    /// Session hash.
    pub hash: i64,
    /// Whether still unconfirmed.
    pub unconfirmed: bool,
    /// Login timestamp.
    pub date: TimeId,
    /// Device description.
    pub device: String,
    /// Location description.
    pub location: String,
}

/// Active sessions manager (`Api::Authorizations`, `api/api_authorizations.cpp`).
#[derive(Debug, Clone)]
pub struct AuthorizationsManager {
    list: Vec<AuthorizationEntry>,
    ttl_days: i32,
    calls_disabled_here: bool,
    unreviewed: Vec<UnreviewedAuthorization>,
    reviewed: HashMap<i64, TimeId>,
    autoconfirm_period_secs: TimeId,
}

impl Default for AuthorizationsManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthorizationsManager {
    /// Creates an empty authorizations manager with default 7-day autoconfirm period.
    #[must_use]
    pub fn new() -> Self {
        Self {
            list: Vec::new(),
            ttl_days: 0,
            calls_disabled_here: false,
            unreviewed: Vec::new(),
            reviewed: HashMap::new(),
            autoconfirm_period_secs: AUTHORIZATION_AUTOCONFIRM_FALLBACK_SECS,
        }
    }

    /// Sets the `authorization_autoconfirm_period` from app config.
    pub fn set_autoconfirm_period(&mut self, secs: TimeId) {
        self.autoconfirm_period_secs = secs.max(1);
    }

    /// Returns the cached active sessions list.
    #[must_use]
    pub fn list(&self) -> &[AuthorizationEntry] {
        &self.list
    }

    /// Returns the count of completed sessions (excluding `password_pending` / `incomplete`),
    /// matching `Authorizations::total` (`api/api_authorizations.cpp:289-293`).
    #[must_use]
    pub fn total(&self) -> usize {
        self.list.iter().filter(|e| !e.incomplete).count()
    }

    /// Returns the authorization auto-termination TTL in days (`0` if not loaded).
    #[must_use]
    pub const fn ttl_days(&self) -> i32 {
        self.ttl_days
    }

    /// Returns whether incoming calls are disabled on the current session (`hash == 0`).
    #[must_use]
    pub const fn calls_disabled_here(&self) -> bool {
        self.calls_disabled_here
    }

    /// Returns currently pending unreviewed logins (pruning expired entries).
    pub fn unreviewed(&mut self, now_secs: TimeId) -> &[UnreviewedAuthorization] {
        self.remove_expired_unreviewed(now_secs);
        &self.unreviewed
    }

    /// Reloads active sessions via `account.getAuthorizations`, reconciling unreviewed entries
    /// and syncing `calls_disabled_here` (`Authorizations::reload`, `api/api_authorizations.cpp:143-178`).
    pub async fn reload<C: RpcClient>(
        &mut self,
        rpc: &C,
        now_secs: TimeId,
    ) -> Result<&[AuthorizationEntry], ApiError> {
        let snapshot = self.unreviewed.clone();
        let tl_enums::account::Authorizations::Authorizations(resp) = rpc
            .invoke(&tl_fns::account::GetAuthorizations)
            .await
            .map_err(map_rpc)?;

        self.ttl_days = resp.authorization_ttl_days;
        self.list = resp
            .authorizations
            .iter()
            .map(|a| {
                let tl_enums::Authorization::Authorization(inner) = a;
                AuthorizationEntry::from_tl(inner)
            })
            .collect();

        // Reconcile unreviewed authorizations against server snapshot (`api_authorizations.cpp:520-539`).
        self.unreviewed.retain(|auth| {
            let was_in_snapshot = snapshot.contains(auth);
            if !was_in_snapshot {
                return true;
            }
            resp.authorizations.iter().any(|a| {
                let tl_enums::Authorization::Authorization(inner) = a;
                inner.hash == auth.hash && inner.unconfirmed
            })
        });
        self.remove_expired_unreviewed(now_secs);

        if let Some(current) = self.list.iter().find(|e| e.hash == 0) {
            self.calls_disabled_here = current.calls_disabled;
        }
        Ok(&self.list)
    }

    /// Terminates a single session by hash (`account.resetAuthorization`).
    /// Refuses `hash == 0` (the current session).
    pub async fn terminate<C: RpcClient>(&mut self, rpc: &C, hash: i64) -> Result<bool, ApiError> {
        if hash == 0 {
            return Err(ApiError::Validation(
                "cannot terminate current session (hash == 0) via resetAuthorization".to_owned(),
            ));
        }
        let ok = rpc
            .invoke(&tl_fns::account::ResetAuthorization { hash })
            .await
            .map_err(map_rpc)?;
        if ok {
            self.list.retain(|e| e.hash != hash);
        }
        Ok(ok)
    }

    /// Terminates all other active sessions (`auth.resetAuthorizations`,
    /// `api/api_authorizations.cpp:180-208`).
    pub async fn terminate_all_other<C: RpcClient>(&mut self, rpc: &C) -> Result<bool, ApiError> {
        let ok = rpc
            .invoke(&tl_fns::auth::ResetAuthorizations)
            .await
            .map_err(map_rpc)?;
        if ok {
            self.list.retain(|e| e.hash == 0);
        }
        Ok(ok)
    }

    /// Updates inactive-session auto-termination TTL (`account.setAuthorizationTTL`,
    /// `api/api_authorizations.cpp:231-241`).
    pub async fn set_ttl_days<C: RpcClient>(
        &mut self,
        rpc: &C,
        days: i32,
    ) -> Result<bool, ApiError> {
        let ok = rpc
            .invoke(&tl_fns::account::SetAuthorizationTTL {
                authorization_ttl_days: days,
            })
            .await
            .map_err(map_rpc)?;
        if ok {
            self.ttl_days = days;
        }
        Ok(ok)
    }

    /// Updates per-session authorization settings (`account.changeAuthorizationSettings`,
    /// `api/api_authorizations.cpp:247-266, 474-483`).
    ///
    /// Preserves exact TL flags: only sets `flags.3` (`confirmed`), `flags.0`
    /// (`encrypted_requests_disabled`), and `flags.1` (`call_requests_disabled`) as requested.
    pub async fn change_settings<C: RpcClient>(
        &mut self,
        rpc: &C,
        hash: i64,
        confirmed: bool,
        encrypted_requests_disabled: Option<bool>,
        call_requests_disabled: Option<bool>,
    ) -> Result<bool, ApiError> {
        let ok = rpc
            .invoke(&tl_fns::account::ChangeAuthorizationSettings {
                confirmed,
                hash,
                encrypted_requests_disabled,
                call_requests_disabled,
            })
            .await
            .map_err(map_rpc)?;
        if ok {
            if let Some(entry) = self.list.iter_mut().find(|e| e.hash == hash) {
                if confirmed {
                    entry.unconfirmed = false;
                }
                if let Some(enc) = encrypted_requests_disabled {
                    entry.encrypted_disabled = enc;
                }
                if let Some(calls) = call_requests_disabled {
                    entry.calls_disabled = calls;
                }
            }
            if hash == 0
                && let Some(calls) = call_requests_disabled
            {
                self.calls_disabled_here = calls;
            }
        }
        Ok(ok)
    }

    /// Applies an incoming `updateNewAuthorization` (`Authorizations::apply`,
    /// `api/api_authorizations.cpp:590-642`).
    pub fn apply_new_authorization(
        &mut self,
        update: &tl_types::UpdateNewAuthorization,
        now_secs: TimeId,
    ) {
        self.remove_expired_unreviewed(now_secs);
        let item = UnreviewedAuthorization {
            hash: update.hash,
            unconfirmed: update.unconfirmed,
            date: update.date.unwrap_or(0),
            device: update.device.clone().unwrap_or_default(),
            location: update.location.clone().unwrap_or_default(),
        };

        if item.unconfirmed
            && let Some(&rev_date) = self.reviewed.get(&item.hash)
            && item.date <= rev_date
        {
            return;
        }

        if let Some(pos) = self.unreviewed.iter().position(|a| a.hash == item.hash) {
            if !item.unconfirmed {
                self.reviewed.insert(item.hash, self.unreviewed[pos].date);
                self.unreviewed.remove(pos);
            } else if item.date >= self.unreviewed[pos].date {
                self.unreviewed[pos] = item;
            }
        } else if item.unconfirmed {
            self.unreviewed.push(item);
        }
        self.remove_expired_unreviewed(now_secs);
    }

    /// Confirms (`confirm = true`) or rejects (`confirm = false`) an unreviewed authorization
    /// (`Authorizations::reviewOne` / `finishReview`, `api/api_authorizations.cpp:403-497`).
    ///
    /// Treats `HASH_INVALID` as already resolved and removes the entry from `unreviewed`.
    pub async fn review_unreviewed<C: RpcClient>(
        &mut self,
        rpc: &C,
        hash: i64,
        confirm: bool,
        now_secs: TimeId,
    ) -> Result<bool, ApiError> {
        let auth_date = self
            .unreviewed
            .iter()
            .find(|a| a.hash == hash)
            .map_or(now_secs, |a| a.date);

        let rpc_res = if confirm {
            rpc.invoke(&tl_fns::account::ChangeAuthorizationSettings {
                confirmed: true,
                hash,
                encrypted_requests_disabled: None,
                call_requests_disabled: None,
            })
            .await
        } else {
            rpc.invoke(&tl_fns::account::ResetAuthorization { hash })
                .await
        };

        match rpc_res {
            Ok(ok) => {
                if ok {
                    if !confirm {
                        self.list.retain(|e| e.hash != hash);
                    }
                    self.reviewed.insert(hash, auth_date);
                    self.unreviewed.retain(|a| a.hash != hash);
                }
                Ok(ok)
            }
            Err(err) => {
                let mapped: InvocationError = err.into();
                if let InvocationError::Rpc(ref rpc_err) = mapped
                    && rpc_err.name == "HASH_INVALID"
                {
                    // Already gone on server (`api_authorizations.cpp:443`).
                    self.reviewed.insert(hash, auth_date);
                    self.unreviewed.retain(|a| a.hash != hash);
                    return Ok(false);
                }
                Err(ApiError::Invocation(mapped))
            }
        }
    }

    fn remove_expired_unreviewed(&mut self, now_secs: TimeId) {
        let period = self.autoconfirm_period_secs;
        self.reviewed
            .retain(|_, date| now_secs.saturating_sub(*date) < period);
        self.unreviewed
            .retain(|auth| now_secs.saturating_sub(auth.date) < period);
    }
}

/// Connected website authorization entry (`Api::Websites::Entry`, `api/api_websites.cpp:20-37`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebAuthorizationEntry {
    /// Web session hash.
    pub hash: i64,
    /// Associated bot user ID.
    pub bot_id: UserId,
    /// Domain name.
    pub domain: String,
    /// Browser string.
    pub browser: String,
    /// Platform string.
    pub platform: String,
    /// Last active timestamp (`date_active` if non-zero, else `date_created`).
    pub active_time: TimeId,
    /// Creation timestamp.
    pub date_created: TimeId,
    /// IP address.
    pub ip: String,
    /// Region string.
    pub region: String,
}

impl WebAuthorizationEntry {
    /// Parses TL `WebAuthorization` matching `ParseEntry` (`api/api_websites.cpp:20-37`).
    #[must_use]
    pub fn from_tl(data: &tl_types::WebAuthorization) -> Self {
        let active_time = if data.date_active != 0 {
            data.date_active
        } else {
            data.date_created
        };
        Self {
            hash: data.hash,
            bot_id: UserId::from_tl(data.bot_id),
            domain: data.domain.clone(),
            browser: data.browser.clone(),
            platform: data.platform.clone(),
            active_time,
            date_created: data.date_created,
            ip: data.ip.clone(),
            region: data.region.clone(),
        }
    }
}

/// Connected websites manager (`Api::Websites`, `api/api_websites.cpp`).
#[derive(Debug, Default, Clone)]
pub struct WebAuthorizationsManager {
    list: Vec<WebAuthorizationEntry>,
}

impl WebAuthorizationsManager {
    /// Creates an empty web authorizations manager.
    #[must_use]
    pub const fn new() -> Self {
        Self { list: Vec::new() }
    }

    /// Returns the cached web authorizations list.
    #[must_use]
    pub fn list(&self) -> &[WebAuthorizationEntry] {
        &self.list
    }

    /// Reloads connected websites via `account.getWebAuthorizations` and applies returned bot
    /// users into `session` (`Websites::reload`, `api/api_websites.cpp:46-67`).
    pub async fn reload<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: &mut Session,
    ) -> Result<&[WebAuthorizationEntry], ApiError> {
        let tl_enums::account::WebAuthorizations::WebAuthorizations(resp) = rpc
            .invoke(&tl_fns::account::GetWebAuthorizations)
            .await
            .map_err(map_rpc)?;
        apply_tl_users_and_chats(session, &resp.users, &[]);
        self.list = resp
            .authorizations
            .iter()
            .map(|a| {
                let tl_enums::WebAuthorization::WebAuthorization(inner) = a;
                WebAuthorizationEntry::from_tl(inner)
            })
            .collect();
        Ok(&self.list)
    }

    /// Disconnects a single website session (`account.resetWebAuthorization`,
    /// `api/api_websites.cpp:73-103`).
    pub async fn terminate<C: RpcClient>(&mut self, rpc: &C, hash: i64) -> Result<bool, ApiError> {
        let ok = rpc
            .invoke(&tl_fns::account::ResetWebAuthorization { hash })
            .await
            .map_err(map_rpc)?;
        self.list.retain(|e| e.hash != hash);
        Ok(ok)
    }

    /// Disconnects all connected websites (`account.resetWebAuthorizations`).
    pub async fn terminate_all<C: RpcClient>(&mut self, rpc: &C) -> Result<bool, ApiError> {
        let ok = rpc
            .invoke(&tl_fns::account::ResetWebAuthorizations)
            .await
            .map_err(map_rpc)?;
        self.list.clear();
        Ok(ok)
    }
}

// ============================================================================
// 3. Notification Settings & Exceptions (`data/notify/data_*notify_settings.cpp`)
// ============================================================================

pub use tg_data::DefaultNotifyScope;

/// Converts a default scope to TL `InputNotifyPeer` (`Data::DefaultNotifyToMTP`,
/// `data/notify/data_notify_settings.cpp:79-86`).
#[must_use]
pub const fn default_notify_scope_to_tl(scope: DefaultNotifyScope) -> tl_enums::InputNotifyPeer {
    match scope {
        DefaultNotifyScope::Users => tl_enums::InputNotifyPeer::InputNotifyUsers,
        DefaultNotifyScope::Chats => tl_enums::InputNotifyPeer::InputNotifyChats,
        DefaultNotifyScope::Broadcasts => tl_enums::InputNotifyPeer::InputNotifyBroadcasts,
    }
}

/// Target for `account.getNotifySettings` / `account.updateNotifySettings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NotifyTarget {
    /// Specific user, chat, or channel (`inputNotifyPeer`).
    Peer(PeerId),
    /// Scope default (`inputNotifyUsers` / `inputNotifyChats` / `inputNotifyBroadcasts`).
    Scope(DefaultNotifyScope),
    /// Forum topic (`inputNotifyForumTopic`).
    ForumTopic {
        /// Forum channel peer ID.
        peer: PeerId,
        /// Topic root message ID.
        top_msg_id: MsgId,
    },
    /// Community (`inputNotifyCommunity`).
    Community(ChannelId),
}

impl NotifyTarget {
    /// Converts to TL `InputNotifyPeer` using access hashes in `session`.
    #[must_use]
    pub fn to_tl(self, session: &Session) -> tl_enums::InputNotifyPeer {
        match self {
            Self::Peer(peer_id) => {
                tl_enums::InputNotifyPeer::InputNotifyPeer(tl_types::InputNotifyPeer {
                    peer: input_peer_for(session, peer_id),
                })
            }
            Self::Scope(scope) => default_notify_scope_to_tl(scope),
            Self::ForumTopic { peer, top_msg_id } => {
                tl_enums::InputNotifyPeer::InputNotifyForumTopic(tl_types::InputNotifyForumTopic {
                    peer: input_peer_for(session, peer),
                    top_msg_id: top_msg_id.0 as i32,
                })
            }
            Self::Community(channel_id) => {
                tl_enums::InputNotifyPeer::InputNotifyCommunity(tl_types::InputNotifyCommunity {
                    community: input_channel_for(session, channel_id),
                })
            }
        }
    }

    /// Converts a TL `NotifyPeer` into a `NotifyTarget`.
    #[must_use]
    pub const fn from_tl(peer: &tl_enums::NotifyPeer) -> Self {
        match peer {
            tl_enums::NotifyPeer::NotifyPeer(p) => Self::Peer(peer_id_from_tl(&p.peer)),
            tl_enums::NotifyPeer::NotifyUsers => Self::Scope(DefaultNotifyScope::Users),
            tl_enums::NotifyPeer::NotifyChats => Self::Scope(DefaultNotifyScope::Chats),
            tl_enums::NotifyPeer::NotifyBroadcasts => Self::Scope(DefaultNotifyScope::Broadcasts),
            tl_enums::NotifyPeer::NotifyForumTopic(t) => Self::ForumTopic {
                peer: peer_id_from_tl(&t.peer),
                top_msg_id: MsgId(t.top_msg_id as i64),
            },
            tl_enums::NotifyPeer::NotifyCommunity(c) => {
                Self::Community(ChannelId::from_tl(c.community_id))
            }
        }
    }
}

/// Typed notification sound (`Data::NotifySound`, `data/notify/data_peer_notify_settings.cpp:27-55`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NotifySound {
    /// Default system notification sound (`notificationSoundDefault`).
    #[default]
    Default,
    /// Silent / no sound (`notificationSoundNone`).
    None,
    /// Platform-local sound (`notificationSoundLocal`).
    Local {
        /// Display title.
        title: String,
        /// Sound identifier / data.
        data: String,
    },
    /// Uploaded custom ringtone document ID (`notificationSoundRingtone`).
    Ringtone(i64),
}

impl NotifySound {
    /// Parses TL `NotificationSound` (`ParseSound`, `data/notify/data_peer_notify_settings.cpp:27-40`).
    #[must_use]
    pub fn from_tl(sound: &tl_enums::NotificationSound) -> Self {
        match sound {
            tl_enums::NotificationSound::Default => Self::Default,
            tl_enums::NotificationSound::None => Self::None,
            tl_enums::NotificationSound::Local(l) => Self::Local {
                title: l.title.clone(),
                data: l.data.clone(),
            },
            tl_enums::NotificationSound::Ringtone(r) => Self::Ringtone(r.id),
        }
    }

    /// Serializes to TL `NotificationSound` (`SerializeSound`,
    /// `data/notify/data_peer_notify_settings.cpp:42-55`).
    #[must_use]
    pub fn to_tl(&self) -> tl_enums::NotificationSound {
        match self {
            Self::Default => tl_enums::NotificationSound::Default,
            Self::None => tl_enums::NotificationSound::None,
            Self::Ringtone(id) if *id != 0 => {
                tl_enums::NotificationSound::Ringtone(tl_types::NotificationSoundRingtone {
                    id: *id,
                })
            }
            Self::Ringtone(_) => tl_enums::NotificationSound::Default,
            Self::Local { title, data } if !title.is_empty() => {
                tl_enums::NotificationSound::Local(tl_types::NotificationSoundLocal {
                    title: title.clone(),
                    data: data.clone(),
                })
            }
            Self::Local { .. } => tl_enums::NotificationSound::Default,
        }
    }
}

/// Mute duration specification (`Data::MuteValue`, `data/notify/data_peer_notify_settings.cpp:59-69`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuteValue {
    /// Unmute (`mute_until = 0`).
    Unmute,
    /// Mute forever (`mute_until = i32::MAX`).
    Forever,
    /// Mute for `seconds` starting from `now_secs` (clamped to `i32::MAX`).
    Period(i32),
}

impl MuteValue {
    /// Computes the `mute_until` Unix timestamp matching `MuteValue::until()`.
    #[must_use]
    pub fn until(self, now_secs: TimeId) -> TimeId {
        match self {
            Self::Forever => i32::MAX,
            Self::Period(period) if period > 0 => {
                (i64::from(now_secs) + i64::from(period)).min(i64::from(i32::MAX)) as TimeId
            }
            Self::Unmute | Self::Period(_) => 0,
        }
    }
}

/// Full protocol notification settings preserving all TL fields including stories fields
/// (`data/notify/data_peer_notify_settings.cpp`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FullNotifySettings {
    /// Whether message previews are shown (`flags.0`).
    pub show_previews: Option<bool>,
    /// Whether broadcast posts are silent (`flags.1`).
    pub silent: Option<bool>,
    /// Mute expiration timestamp (`flags.2`; `0` = unmuted, `i32::MAX` = forever).
    pub mute_until: Option<TimeId>,
    /// Desktop notification sound (`other_sound` on receive, `sound` on send).
    pub sound: Option<NotifySound>,
    /// Whether stories notifications are muted (`flags.6`).
    pub stories_muted: Option<bool>,
    /// Whether sender name is hidden in story notifications (`flags.7`).
    pub stories_hide_sender: Option<bool>,
    /// Story notification sound (`stories_other_sound` on receive, `stories_sound` on send).
    pub stories_sound: Option<NotifySound>,
}

impl FullNotifySettings {
    /// Parses TL `PeerNotifySettings`, reading ONLY `other_sound` and `stories_other_sound`
    /// matching `NotifyPeerSettingsValue::change` (`data/notify/data_peer_notify_settings.cpp:108-124`).
    #[must_use]
    pub fn from_tl(settings: &tl_enums::PeerNotifySettings) -> Self {
        let tl_enums::PeerNotifySettings::PeerNotifySettings(s) = settings;
        Self {
            show_previews: s.show_previews,
            silent: s.silent,
            mute_until: s.mute_until,
            sound: s.other_sound.as_ref().map(NotifySound::from_tl),
            stories_muted: s.stories_muted,
            stories_hide_sender: s.stories_hide_sender,
            stories_sound: s.stories_other_sound.as_ref().map(NotifySound::from_tl),
        }
    }

    /// Serializes into TL `InputPeerNotifySettings` (`NotifyPeerSettingsValue::serialize`,
    /// `data/notify/data_peer_notify_settings.cpp:184-202`).
    #[must_use]
    pub fn to_tl(&self) -> tl_enums::InputPeerNotifySettings {
        tl_enums::InputPeerNotifySettings::InputPeerNotifySettings(
            tl_types::InputPeerNotifySettings {
                show_previews: self.show_previews,
                silent: self.silent,
                mute_until: self.mute_until,
                sound: self.sound.as_ref().map(NotifySound::to_tl),
                stories_muted: self.stories_muted,
                stories_hide_sender: self.stories_hide_sender,
                stories_sound: self.stories_sound.as_ref().map(NotifySound::to_tl),
            },
        )
    }

    /// Returns empty `InputPeerNotifySettings` (`flags = 0`) to reset overrides to scope defaults
    /// (`DefaultSettings` in `data/notify/data_peer_notify_settings.cpp:15-25`).
    #[must_use]
    pub const fn reset_to_default_tl() -> tl_enums::InputPeerNotifySettings {
        tl_enums::InputPeerNotifySettings::InputPeerNotifySettings(
            tl_types::InputPeerNotifySettings {
                show_previews: None,
                silent: None,
                mute_until: None,
                sound: None,
                stories_muted: None,
                stories_hide_sender: None,
                stories_sound: None,
            },
        )
    }

    /// Returns `true` if all override fields are `None` (inherits defaults).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.show_previews.is_none()
            && self.silent.is_none()
            && self.mute_until.is_none()
            && self.sound.is_none()
            && self.stories_muted.is_none()
            && self.stories_hide_sender.is_none()
            && self.stories_sound.is_none()
    }
}

/// Checks whether `peer` should be excluded from per-scope notification exception lists
/// (`SkipAddException`, `data/notify/data_notify_settings.cpp:46-55`).
#[must_use]
pub fn skip_add_notify_exception(peer: &Peer) -> bool {
    match peer {
        Peer::User(u) => u.is_self() || u.flags.contains(UserFlags::DELETED),
        Peer::Chat(c) => c.is_deactivated() || c.is_forbidden(),
        Peer::Channel(ch) => ch.is_forbidden(),
    }
}

/// Notification settings and exceptions manager (`Data::NotifySettings`,
/// `data/notify/data_notify_settings.cpp`).
#[derive(Debug, Default, Clone)]
pub struct NotifySettingsManager {
    defaults: BTreeMap<DefaultNotifyScope, FullNotifySettings>,
    peers: BTreeMap<PeerId, FullNotifySettings>,
    topics: BTreeMap<(PeerId, MsgId), FullNotifySettings>,
    exceptions: BTreeMap<DefaultNotifyScope, BTreeSet<PeerId>>,
}

impl NotifySettingsManager {
    /// Creates an empty notification settings manager.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns stored scope default settings, if known.
    #[must_use]
    pub fn default_settings(&self, scope: DefaultNotifyScope) -> Option<&FullNotifySettings> {
        self.defaults.get(&scope)
    }

    /// Returns stored peer override settings, if known.
    #[must_use]
    pub fn peer_settings(&self, peer_id: PeerId) -> Option<&FullNotifySettings> {
        self.peers.get(&peer_id)
    }

    /// Returns stored forum topic override settings, if known.
    #[must_use]
    pub fn topic_settings(
        &self,
        peer_id: PeerId,
        top_msg_id: MsgId,
    ) -> Option<&FullNotifySettings> {
        self.topics.get(&(peer_id, top_msg_id))
    }

    /// Returns the set of exception peer IDs for `scope`.
    #[must_use]
    pub fn exceptions(&self, scope: DefaultNotifyScope) -> Option<&BTreeSet<PeerId>> {
        self.exceptions.get(&scope)
    }

    /// Computes effective mute state for `peer_id` using `Peer -> ScopeDefault -> true` fallback
    /// (`NotifySettings::isMuted`, `data/notify/data_notify_settings.cpp:547-561`).
    #[must_use]
    pub fn is_peer_muted(&self, session: &Session, peer_id: PeerId, now_secs: TimeId) -> bool {
        if let Some(until) = self.peers.get(&peer_id).and_then(|s| s.mute_until) {
            return until > now_secs;
        }
        let scope = session
            .peer(peer_id)
            .map(DefaultNotifyScope::for_peer)
            .unwrap_or(DefaultNotifyScope::Users);
        if let Some(until) = self.defaults.get(&scope).and_then(|s| s.mute_until) {
            return until > now_secs;
        }
        true
    }

    /// Computes effective mute state for a forum topic (`Topic -> Peer -> ScopeDefault`,
    /// `data/notify/data_notify_settings.cpp:513-521`).
    #[must_use]
    pub fn is_topic_muted(
        &self,
        session: &Session,
        peer_id: PeerId,
        top_msg_id: MsgId,
        now_secs: TimeId,
    ) -> bool {
        if let Some(until) = self
            .topics
            .get(&(peer_id, top_msg_id))
            .and_then(|s| s.mute_until)
        {
            return until > now_secs;
        }
        self.is_peer_muted(session, peer_id, now_secs)
    }

    /// Computes effective notification sound for `peer_id`, explicitly ignoring per-peer sound
    /// for Saved Messages (`self`) to follow scope default
    /// (`NotifySettings::sound`, `data/notify/data_notify_settings.cpp:581-595`).
    #[must_use]
    pub fn effective_peer_sound(&self, session: &Session, peer_id: PeerId) -> NotifySound {
        let is_self = peer_id == session.self_peer_id();
        if !is_self && let Some(sound) = self.peers.get(&peer_id).and_then(|s| s.sound.as_ref()) {
            return sound.clone();
        }
        let scope = session
            .peer(peer_id)
            .map(DefaultNotifyScope::for_peer)
            .unwrap_or(DefaultNotifyScope::Users);
        self.defaults
            .get(&scope)
            .and_then(|s| s.sound.clone())
            .unwrap_or_default()
    }

    /// Fetches notification settings via `account.getNotifySettings` and applies them locally.
    pub async fn get_notify_settings<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: &mut Session,
        target: NotifyTarget,
        now_secs: TimeId,
    ) -> Result<FullNotifySettings, ApiError> {
        let tl_settings = rpc
            .invoke(&tl_fns::account::GetNotifySettings {
                peer: target.to_tl(session),
            })
            .await
            .map_err(map_rpc)?;
        let parsed = self.apply_target(session, target, &tl_settings, now_secs);
        Ok(parsed)
    }

    /// Updates notification settings via `account.updateNotifySettings` and applies them locally.
    pub async fn update_notify_settings<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: &mut Session,
        target: NotifyTarget,
        settings: &FullNotifySettings,
        now_secs: TimeId,
    ) -> Result<bool, ApiError> {
        let ok = rpc
            .invoke(&tl_fns::account::UpdateNotifySettings {
                peer: target.to_tl(session),
                settings: settings.to_tl(),
            })
            .await
            .map_err(map_rpc)?;
        if ok {
            self.apply_full_target(session, target, settings.clone(), now_secs);
        }
        Ok(ok)
    }

    /// Resets a peer/topic/scope back to defaults via `account.updateNotifySettings` with
    /// `InputPeerNotifySettings` having `flags = 0` (`NotifySettings::resetToDefault`).
    pub async fn reset_notify_settings<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: &mut Session,
        target: NotifyTarget,
        now_secs: TimeId,
    ) -> Result<bool, ApiError> {
        let ok = rpc
            .invoke(&tl_fns::account::UpdateNotifySettings {
                peer: target.to_tl(session),
                settings: FullNotifySettings::reset_to_default_tl(),
            })
            .await
            .map_err(map_rpc)?;
        if ok {
            self.apply_full_target(session, target, FullNotifySettings::default(), now_secs);
        }
        Ok(ok)
    }

    /// Fetches per-peer notification exceptions via `account.getNotifyExceptions` and applies
    /// returned users/chats and `updateNotifySettings` entries (`NotifySettings::loadExceptions`,
    /// `data/notify/data_notify_settings.cpp:663-678`).
    pub async fn get_notify_exceptions<C: RpcClient>(
        &mut self,
        rpc: &C,
        session: &mut Session,
        scope: Option<DefaultNotifyScope>,
        compare_sound: bool,
        compare_stories: bool,
        now_secs: TimeId,
    ) -> Result<usize, ApiError> {
        let updates = rpc
            .invoke(&tl_fns::account::GetNotifyExceptions {
                compare_sound,
                compare_stories,
                peer: scope.map(default_notify_scope_to_tl),
            })
            .await
            .map_err(map_rpc)?;
        let (items, users, chats) = match updates {
            tl_enums::Updates::Updates(u) => (u.updates, u.users, u.chats),
            tl_enums::Updates::Combined(c) => (c.updates, c.users, c.chats),
            tl_enums::Updates::UpdateShort(s) => (vec![s.update], Vec::new(), Vec::new()),
            _ => (Vec::new(), Vec::new(), Vec::new()),
        };
        apply_tl_users_and_chats(session, &users, &chats);
        let mut applied = 0usize;
        for upd in items {
            if let tl_enums::Update::NotifySettings(ns) = upd {
                self.apply_update(session, &ns.peer, &ns.notify_settings, now_secs);
                applied += 1;
            }
        }
        Ok(applied)
    }

    /// Applies an incoming `updateNotifySettings` (`NotifySettings::apply`,
    /// `data/notify/data_notify_settings.cpp:122-227`).
    pub fn apply_update(
        &mut self,
        session: &mut Session,
        notify_peer: &tl_enums::NotifyPeer,
        tl_settings: &tl_enums::PeerNotifySettings,
        now_secs: TimeId,
    ) -> FullNotifySettings {
        let target = NotifyTarget::from_tl(notify_peer);
        self.apply_target(session, target, tl_settings, now_secs)
    }

    fn apply_target(
        &mut self,
        session: &mut Session,
        target: NotifyTarget,
        tl_settings: &tl_enums::PeerNotifySettings,
        now_secs: TimeId,
    ) -> FullNotifySettings {
        let parsed = FullNotifySettings::from_tl(tl_settings);
        let data_settings = peer_notify_settings_from_tl(tl_settings);
        self.apply_parsed(session, target, parsed.clone(), data_settings, now_secs);
        parsed
    }

    fn apply_full_target(
        &mut self,
        session: &mut Session,
        target: NotifyTarget,
        parsed: FullNotifySettings,
        now_secs: TimeId,
    ) {
        let data_settings = PeerNotifySettings {
            mute_until: parsed.mute_until,
            silent: parsed.silent,
            show_previews: parsed.show_previews,
            sound: parsed.sound.as_ref().map(|s| match s {
                NotifySound::Default => "default".to_owned(),
                NotifySound::None => "none".to_owned(),
                NotifySound::Local { data, .. } => data.clone(),
                NotifySound::Ringtone(id) => format!("ringtone:{id}"),
            }),
            stories_muted: parsed.stories_muted,
        };
        self.apply_parsed(session, target, parsed, data_settings, now_secs);
    }

    fn apply_parsed(
        &mut self,
        session: &mut Session,
        target: NotifyTarget,
        parsed: FullNotifySettings,
        data_settings: PeerNotifySettings,
        now_secs: TimeId,
    ) {
        match target {
            NotifyTarget::Scope(scope) => {
                self.defaults.insert(scope, parsed);
                // `NotifySettings::updateLocal(DefaultNotify)` (`data_notify_settings.cpp:431-457`).
                session.set_notify_default(scope, Some(data_settings), now_secs);
            }
            NotifyTarget::Peer(peer_id) => {
                let has_mute_override = parsed.mute_until.is_some();
                if parsed.is_empty() {
                    self.peers.remove(&peer_id);
                } else {
                    self.peers.insert(peer_id, parsed);
                }
                if let Some(peer) = session.peer(peer_id) {
                    let scope = DefaultNotifyScope::for_peer(peer);
                    if !has_mute_override {
                        if let Some(set) = self.exceptions.get_mut(&scope) {
                            set.remove(&peer_id);
                        }
                    } else if !skip_add_notify_exception(peer) {
                        self.exceptions.entry(scope).or_default().insert(peer_id);
                    }
                }
                if let Some(peer) = session.peer_mut(peer_id) {
                    peer.common_mut().notify = data_settings;
                }
                let muted = self.is_peer_muted(session, peer_id, now_secs);
                let prev_muted = session.history(peer_id).map(|h| h.muted);
                session.set_history_muted(peer_id, muted);
                if prev_muted == Some(muted) {
                    session
                        .changes()
                        .peer_updated(peer_id, PeerUpdateFlags::NOTIFICATIONS);
                }
            }
            NotifyTarget::Community(channel_id) => {
                let peer_id = PeerId::from_channel(channel_id);
                if parsed.is_empty() {
                    self.peers.remove(&peer_id);
                } else {
                    self.peers.insert(peer_id, parsed);
                }
                if let Some(peer) = session.peer_mut(peer_id) {
                    peer.common_mut().notify = data_settings;
                }
                let muted = self.is_peer_muted(session, peer_id, now_secs);
                let prev_muted = session.history(peer_id).map(|h| h.muted);
                session.set_history_muted(peer_id, muted);
                if prev_muted == Some(muted) {
                    session
                        .changes()
                        .peer_updated(peer_id, PeerUpdateFlags::NOTIFICATIONS);
                }
            }
            NotifyTarget::ForumTopic { peer, top_msg_id } => {
                if parsed.is_empty() {
                    self.topics.remove(&(peer, top_msg_id));
                } else {
                    self.topics.insert((peer, top_msg_id), parsed);
                }
            }
        }
    }
}

// ============================================================================
// 4. Account Settings Sync (`api_global_privacy.cpp`, `api_sensitive_content.cpp`,
//    `api_self_destruct.cpp`, profile/username/birthday/personal channel)
// ============================================================================

/// Archive unarchiving behavior (`Api::UnarchiveOnNewMessage`, `api/api_global_privacy.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnarchiveOnNewMessage {
    /// Keep all archived chats archived (`keep_archived_unmuted = true, keep_archived_folders = true`).
    None,
    /// Only unarchive chats not in folders (`keep_archived_unmuted = false, keep_archived_folders = true`).
    NotInFoldersUnmuted,
    /// Unarchive any unmuted chat (`keep_archived_unmuted = false, keep_archived_folders = false`).
    #[default]
    AnyUnmuted,
}

/// Disallowed gift types & gift icon toggle (`Api::DisallowedGiftTypes`,
/// `api/api_global_privacy.cpp:249-264, 309-334`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DisallowedGifts {
    /// Disallow unlimited Star gifts.
    pub unlimited: bool,
    /// Disallow limited Star gifts.
    pub limited: bool,
    /// Disallow unique Star gifts.
    pub unique: bool,
    /// Disallow Telegram Premium gifts.
    pub premium: bool,
    /// Disallow Star gifts from channels.
    pub from_channels: bool,
    /// Display the gift button in input field (`display_gifts_button`).
    pub display_gifts_button: bool,
}

/// Parsed global privacy settings (`Api::GlobalPrivacy`, `api/api_global_privacy.cpp`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlobalPrivacyState {
    /// Archive and mute new non-contact peers.
    pub archive_and_mute_new_noncontact_peers: bool,
    /// Unarchive behavior (`keep_archived_unmuted` + `keep_archived_folders`).
    pub unarchive_on_new_message: UnarchiveOnNewMessage,
    /// Hide read timestamps.
    pub hide_read_marks: bool,
    /// Require Telegram Premium for new non-contacts.
    pub new_noncontact_peers_require_premium: bool,
    /// Stars charged to non-contacts (`0` = free).
    pub noncontact_peers_paid_stars: i64,
    /// Gift restriction settings.
    pub disallowed_gifts: DisallowedGifts,
}

impl GlobalPrivacyState {
    /// Parses TL `GlobalPrivacySettings` matching `GlobalPrivacy::apply`
    /// (`api/api_global_privacy.cpp:298-335`).
    #[must_use]
    pub fn from_tl(settings: &tl_enums::GlobalPrivacySettings) -> Self {
        let tl_enums::GlobalPrivacySettings::GlobalPrivacySettings(d) = settings;
        let unarchive_on_new_message = if d.keep_archived_unmuted {
            UnarchiveOnNewMessage::None
        } else if d.keep_archived_folders {
            UnarchiveOnNewMessage::NotInFoldersUnmuted
        } else {
            UnarchiveOnNewMessage::AnyUnmuted
        };
        let mut disallowed_gifts = DisallowedGifts {
            display_gifts_button: d.display_gifts_button,
            ..DisallowedGifts::default()
        };
        if let Some(tl_enums::DisallowedGiftsSettings::DisallowedGiftsSettings(g)) =
            &d.disallowed_gifts
        {
            disallowed_gifts.unlimited = g.disallow_unlimited_stargifts;
            disallowed_gifts.limited = g.disallow_limited_stargifts;
            disallowed_gifts.unique = g.disallow_unique_stargifts;
            disallowed_gifts.premium = g.disallow_premium_gifts;
            disallowed_gifts.from_channels = g.disallow_stargifts_from_channels;
        }
        Self {
            archive_and_mute_new_noncontact_peers: d.archive_and_mute_new_noncontact_peers,
            unarchive_on_new_message,
            hide_read_marks: d.hide_read_marks,
            new_noncontact_peers_require_premium: d.new_noncontact_peers_require_premium,
            noncontact_peers_paid_stars: d.noncontact_peers_paid_stars.unwrap_or(0),
            disallowed_gifts,
        }
    }

    /// Serializes to TL `GlobalPrivacySettings` matching `GlobalPrivacy::update`
    /// (`api/api_global_privacy.cpp:224-272`): always sets `noncontact_peers_paid_stars`
    /// (`flags.5`) and `disallowed_gifts` (`flags.6`), and gates `new_noncontact_peers_require_premium`
    /// by `allow_require_premium`.
    #[must_use]
    pub fn to_tl(&self, allow_require_premium: bool) -> tl_enums::GlobalPrivacySettings {
        let keep_archived_unmuted = self.unarchive_on_new_message == UnarchiveOnNewMessage::None;
        let keep_archived_folders =
            self.unarchive_on_new_message != UnarchiveOnNewMessage::AnyUnmuted;
        tl_enums::GlobalPrivacySettings::GlobalPrivacySettings(tl_types::GlobalPrivacySettings {
            archive_and_mute_new_noncontact_peers: self.archive_and_mute_new_noncontact_peers,
            keep_archived_unmuted,
            keep_archived_folders,
            hide_read_marks: self.hide_read_marks,
            new_noncontact_peers_require_premium: self.new_noncontact_peers_require_premium
                && allow_require_premium,
            display_gifts_button: self.disallowed_gifts.display_gifts_button,
            noncontact_peers_paid_stars: Some(self.noncontact_peers_paid_stars),
            disallowed_gifts: Some(tl_enums::DisallowedGiftsSettings::DisallowedGiftsSettings(
                tl_types::DisallowedGiftsSettings {
                    disallow_unlimited_stargifts: self.disallowed_gifts.unlimited,
                    disallow_limited_stargifts: self.disallowed_gifts.limited,
                    disallow_unique_stargifts: self.disallowed_gifts.unique,
                    disallow_premium_gifts: self.disallowed_gifts.premium,
                    disallow_stargifts_from_channels: self.disallowed_gifts.from_channels,
                },
            )),
        })
    }
}

/// Sensitive content settings (`Api::SensitiveContent`, `api/api_sensitive_content.cpp`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContentSettingsState {
    /// Whether sensitive content is enabled (`sensitive_enabled`).
    pub sensitive_enabled: bool,
    /// Whether the user is allowed to toggle sensitive content (`sensitive_can_change`).
    pub sensitive_can_change: bool,
    /// Whether settings have been fetched from the server.
    pub loaded: bool,
}

/// Result of `account.checkUsername` (`boxes/username_box.cpp:168-194`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsernameCheckResult {
    /// Username is available to claim.
    Available,
    /// Username is already occupied (`false` or `USERNAME_OCCUPIED`).
    Occupied,
    /// Username is available for purchase on Fragment (`USERNAME_PURCHASE_AVAILABLE`).
    PurchaseAvailable,
    /// Username failed local or server syntax validation (`USERNAME_INVALID`).
    Invalid,
}

/// Account settings & profile synchronization manager (`api_global_privacy.cpp`,
/// `api_sensitive_content.cpp`, `api_self_destruct.cpp`, `apiwrap.cpp`, `boxes/*`).
#[derive(Debug, Default, Clone)]
pub struct AccountSettingsManager {
    content: ContentSettingsState,
    global_privacy: GlobalPrivacyState,
    account_ttl_days: i32,
}

impl AccountSettingsManager {
    /// Creates an empty account settings manager.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns cached sensitive content settings.
    #[must_use]
    pub const fn content_settings(&self) -> ContentSettingsState {
        self.content
    }

    /// Returns cached global privacy settings.
    #[must_use]
    pub const fn global_privacy(&self) -> &GlobalPrivacyState {
        &self.global_privacy
    }

    /// Returns cached account self-destruct TTL in days (`0` if not loaded).
    #[must_use]
    pub const fn account_ttl_days(&self) -> i32 {
        self.account_ttl_days
    }

    /// Fetches sensitive content settings (`account.getContentSettings`,
    /// `api/api_sensitive_content.cpp:33-65`).
    pub async fn get_content_settings<C: RpcClient>(
        &mut self,
        rpc: &C,
    ) -> Result<ContentSettingsState, ApiError> {
        let tl_enums::account::ContentSettings::ContentSettings(resp) = rpc
            .invoke(&tl_fns::account::GetContentSettings)
            .await
            .map_err(map_rpc)?;
        self.content = ContentSettingsState {
            sensitive_enabled: resp.sensitive_enabled,
            sensitive_can_change: resp.sensitive_can_change,
            loaded: true,
        };
        Ok(self.content)
    }

    /// Updates sensitive content settings (`account.setContentSettings`,
    /// `api/api_sensitive_content.cpp:96-119`).
    ///
    /// Rejects the call without issuing an RPC if `loaded && !sensitive_can_change`.
    pub async fn set_content_settings<C: RpcClient>(
        &mut self,
        rpc: &C,
        sensitive_enabled: bool,
    ) -> Result<bool, ApiError> {
        if self.content.loaded && !self.content.sensitive_can_change {
            return Err(ApiError::InvalidState(
                "sensitive content setting cannot be changed on this account".to_owned(),
            ));
        }
        let ok = rpc
            .invoke(&tl_fns::account::SetContentSettings { sensitive_enabled })
            .await
            .map_err(map_rpc)?;
        if ok {
            self.content.sensitive_enabled = sensitive_enabled;
        }
        Ok(ok)
    }

    /// Fetches global privacy settings (`account.getGlobalPrivacySettings`,
    /// `api/api_global_privacy.cpp:49-76`).
    pub async fn get_global_privacy_settings<C: RpcClient>(
        &mut self,
        rpc: &C,
    ) -> Result<GlobalPrivacyState, ApiError> {
        let resp = rpc
            .invoke(&tl_fns::account::GetGlobalPrivacySettings)
            .await
            .map_err(map_rpc)?;
        self.global_privacy = GlobalPrivacyState::from_tl(&resp);
        Ok(self.global_privacy.clone())
    }

    /// Updates global privacy settings (`account.setGlobalPrivacySettings`,
    /// `api/api_global_privacy.cpp:217-296`).
    ///
    /// Automatically retries once without premium-only options (`require_premium = false`,
    /// `paid_stars = 0`, `disallowed_gifts = empty`) if the server replies with
    /// `PREMIUM_ACCOUNT_REQUIRED`.
    pub async fn set_global_privacy_settings<C: RpcClient>(
        &mut self,
        rpc: &C,
        settings: GlobalPrivacyState,
        allow_require_premium: bool,
    ) -> Result<GlobalPrivacyState, ApiError> {
        let req = tl_fns::account::SetGlobalPrivacySettings {
            settings: settings.to_tl(allow_require_premium),
        };
        let resp = match rpc.invoke(&req).await {
            Ok(resp) => resp,
            Err(err) => {
                let mapped: InvocationError = err.into();
                if let InvocationError::Rpc(ref rpc_err) = mapped
                    && rpc_err.name == "PREMIUM_ACCOUNT_REQUIRED"
                {
                    let fallback = GlobalPrivacyState {
                        archive_and_mute_new_noncontact_peers: settings
                            .archive_and_mute_new_noncontact_peers,
                        unarchive_on_new_message: settings.unarchive_on_new_message,
                        hide_read_marks: settings.hide_read_marks,
                        new_noncontact_peers_require_premium: false,
                        noncontact_peers_paid_stars: 0,
                        disallowed_gifts: DisallowedGifts::default(),
                    };
                    rpc.invoke(&tl_fns::account::SetGlobalPrivacySettings {
                        settings: fallback.to_tl(false),
                    })
                    .await
                    .map_err(map_rpc)?
                } else {
                    return Err(ApiError::Invocation(mapped));
                }
            }
        };
        self.global_privacy = GlobalPrivacyState::from_tl(&resp);
        Ok(self.global_privacy.clone())
    }

    /// Fetches account self-destruct TTL in days (`account.getAccountTTL`,
    /// `api/api_self_destruct.cpp:18-27`).
    pub async fn get_account_ttl<C: RpcClient>(&mut self, rpc: &C) -> Result<i32, ApiError> {
        let tl_enums::AccountDaysTTL::AccountDaysTTL(ttl) = rpc
            .invoke(&tl_fns::account::GetAccountTTL)
            .await
            .map_err(map_rpc)?;
        self.account_ttl_days = ttl.days;
        Ok(ttl.days)
    }

    /// Sets account self-destruct TTL in days (`account.setAccountTTL`,
    /// `api/api_self_destruct.cpp:52-62`).
    pub async fn set_account_ttl<C: RpcClient>(
        &mut self,
        rpc: &C,
        days: i32,
    ) -> Result<bool, ApiError> {
        let ok = rpc
            .invoke(&tl_fns::account::SetAccountTTL {
                ttl: tl_enums::AccountDaysTTL::AccountDaysTTL(tl_types::AccountDaysTTL { days }),
            })
            .await
            .map_err(map_rpc)?;
        if ok {
            self.account_ttl_days = days;
        }
        Ok(ok)
    }

    /// Updates self first & last name (`account.updateProfile` with `flags = 0x03`, `about = None`,
    /// `boxes/add_contact_box.cpp:1680-1718`).
    ///
    /// If `first_name` is empty and `last_name` is non-empty, promotes `last_name` to `first_name`
    /// matching `EditNameBox::saveSelf`. Handles `NAME_NOT_MODIFIED` cleanly.
    pub async fn update_profile_name<C: RpcClient>(
        &self,
        rpc: &C,
        session: &mut Session,
        first_name: &str,
        last_name: &str,
    ) -> Result<(), ApiError> {
        let mut first = first_name.trim().to_owned();
        let mut last = last_name.trim().to_owned();
        if first.is_empty() && last.is_empty() {
            return Err(ApiError::Validation(
                "first_name cannot be empty".to_owned(),
            ));
        }
        if first.is_empty() {
            first = std::mem::take(&mut last);
        }
        let req = tl_fns::account::UpdateProfile {
            first_name: Some(first.clone()),
            last_name: Some(last.clone()),
            about: None,
        };
        match rpc.invoke(&req).await {
            Ok(user) => {
                apply_tl_users_and_chats(session, &[user], &[]);
                Ok(())
            }
            Err(err) => {
                let mapped: InvocationError = err.into();
                if let InvocationError::Rpc(ref rpc_err) = mapped
                    && rpc_err.name == "NAME_NOT_MODIFIED"
                {
                    let self_uid = session.self_user_id();
                    if let Some(user) = session.user_mut(self_uid) {
                        let editable = user.usernames.editable_username().to_owned();
                        user.set_name(first, last, String::new(), editable);
                    }
                    return Ok(());
                }
                Err(ApiError::Invocation(mapped))
            }
        }
    }

    /// Updates self bio (`account.updateProfile` with `flags = 0x04` only,
    /// `ApiWrap::saveSelfBio`, `apiwrap.cpp:5801-5823`).
    pub async fn update_profile_bio<C: RpcClient>(
        &self,
        rpc: &C,
        session: &mut Session,
        bio: &str,
    ) -> Result<(), ApiError> {
        let req = tl_fns::account::UpdateProfile {
            first_name: None,
            last_name: None,
            about: Some(bio.to_owned()),
        };
        let user = rpc.invoke(&req).await.map_err(map_rpc)?;
        apply_tl_users_and_chats(session, &[user], &[]);
        let self_peer = session.self_peer_id();
        if let Some(peer) = session.peer_mut(self_peer) {
            peer.common_mut().about = bio.to_owned();
            session
                .changes()
                .peer_updated(self_peer, PeerUpdateFlags::ABOUT);
        }
        Ok(())
    }

    /// Checks whether a username is available (`account.checkUsername`,
    /// `boxes/username_box.cpp:168-194`).
    pub async fn check_username<C: RpcClient>(
        &self,
        rpc: &C,
        username: &str,
    ) -> Result<UsernameCheckResult, ApiError> {
        let cleaned = username.trim().trim_start_matches('@');
        if cleaned.len() < MIN_USERNAME_LENGTH
            || !cleaned
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Ok(UsernameCheckResult::Invalid);
        }
        match rpc
            .invoke(&tl_fns::account::CheckUsername {
                username: cleaned.to_owned(),
            })
            .await
        {
            Ok(true) => Ok(UsernameCheckResult::Available),
            Ok(false) => Ok(UsernameCheckResult::Occupied),
            Err(err) => {
                let mapped: InvocationError = err.into();
                if let InvocationError::Rpc(ref rpc_err) = mapped {
                    match rpc_err.name.as_str() {
                        "USERNAME_INVALID" => return Ok(UsernameCheckResult::Invalid),
                        "USERNAME_OCCUPIED" => return Ok(UsernameCheckResult::Occupied),
                        "USERNAME_PURCHASE_AVAILABLE" => {
                            return Ok(UsernameCheckResult::PurchaseAvailable);
                        }
                        _ => {}
                    }
                }
                Err(ApiError::Invocation(mapped))
            }
        }
    }

    /// Updates or clears (`""`) self editable username (`account.updateUsername`,
    /// `boxes/username_box.cpp:135-152`). Handles `USERNAME_NOT_MODIFIED`.
    pub async fn update_username<C: RpcClient>(
        &self,
        rpc: &C,
        session: &mut Session,
        username: &str,
    ) -> Result<(), ApiError> {
        let cleaned = username.trim().trim_start_matches('@').to_owned();
        match rpc
            .invoke(&tl_fns::account::UpdateUsername { username: cleaned })
            .await
        {
            Ok(user) => {
                apply_tl_users_and_chats(session, &[user], &[]);
                Ok(())
            }
            Err(err) => {
                let mapped: InvocationError = err.into();
                if let InvocationError::Rpc(ref rpc_err) = mapped
                    && rpc_err.name == "USERNAME_NOT_MODIFIED"
                {
                    return Ok(());
                }
                Err(ApiError::Invocation(mapped))
            }
        }
    }

    /// Updates or clears (`None`) self birthday (`account.updateBirthday` with `handleFloodErrors()`,
    /// `core/local_url_handlers.cpp:1050-1071`).
    pub async fn update_birthday<C: RpcClient>(
        &self,
        rpc: &C,
        session: &mut Session,
        birthday: Option<Birthday>,
    ) -> Result<bool, ApiError> {
        let tl_bday = birthday.map(|b| {
            tl_enums::Birthday::Birthday(tl_types::Birthday {
                day: i32::from(b.day),
                month: i32::from(b.month),
                year: (b.year != 0).then_some(i32::from(b.year)),
            })
        });
        let ok = rpc
            .invoke_handle_flood(&tl_fns::account::UpdateBirthday { birthday: tl_bday })
            .await
            .map_err(map_rpc)?;
        if ok {
            let self_uid = session.self_user_id();
            let self_peer = session.self_peer_id();
            if let Some(user) = session.user_mut(self_uid) {
                user.birthday = birthday;
                session
                    .changes()
                    .peer_updated(self_peer, PeerUpdateFlags::BIRTHDAY);
            }
        }
        Ok(ok)
    }

    /// Sets or clears (`None`) self personal channel (`account.updatePersonalChannel`,
    /// `core/local_url_handlers.cpp:205-218`).
    pub async fn update_personal_channel<C: RpcClient>(
        &self,
        rpc: &C,
        session: &mut Session,
        channel_id: Option<ChannelId>,
    ) -> Result<bool, ApiError> {
        let channel = match channel_id {
            Some(cid) => input_channel_for(session, cid),
            None => tl_enums::InputChannel::Empty,
        };
        let ok = rpc
            .invoke(&tl_fns::account::UpdatePersonalChannel { channel })
            .await
            .map_err(map_rpc)?;
        if ok {
            let self_uid = session.self_user_id();
            let self_peer = session.self_peer_id();
            if let Some(user) = session.user_mut(self_uid) {
                user.personal_channel_id = channel_id;
                if channel_id.is_none() {
                    user.personal_channel_message_id = MsgId::ZERO;
                }
                session
                    .changes()
                    .peer_updated(self_peer, PeerUpdateFlags::PERSONAL_CHANNEL);
            }
        }
        Ok(ok)
    }
}
