//! Login and account authentication state machine wired to `tg-tl` and `tg-crypto::srp`.
//!
//! Port origin: `intro/intro_phone.cpp`, `intro/intro_code.cpp`,
//! `intro/intro_email.cpp`, `intro/intro_password_check.cpp`,
//! `intro/intro_qr.cpp`, `intro/intro_signup.cpp`, `intro/intro_step.cpp`,
//! `intro/intro_widget.cpp`, `main/main_account.cpp`, `core/core_cloud_password.cpp`.

use std::time::{Duration, Instant};

use tg_crypto::srp::{self, PasswordKdfAlgoModPow};
use tg_data::TimeId;
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tracing::{debug, warn};

use crate::error::{ApiError, LoginErrorKind, map_login_error};
use crate::invoker::Invoker;
use crate::rpc::InvocationError;
use crate::types::{ApiCredentials, code_settings_desktop_default};

/// Default phone-call fallback timeout in seconds (`intro_phone.cpp:253`).
pub const DEFAULT_CALL_TIMEOUT_SECS: i32 = 60;

/// Minimum interval between `SRP_ID_INVALID` retries (`Core::kHandleSrpIdInvalidTimeout`).
pub const HANDLE_SRP_ID_INVALID_TIMEOUT: Duration = Duration::from_secs(60);

/// Maximum number of DC migrations followed automatically within one login call.
const MAX_MIGRATE_RETRIES: usize = 4;

/// Phone call resend status during `CodeInput` (`Intro::details::CallStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallStatus {
    /// Resend call is not available / disabled.
    Disabled,
    /// Counting down `call_timeout_secs` before auto-calling `auth.resendCode`.
    Waiting,
    /// `auth.resendCode` call request is currently in flight.
    Calling,
    /// Phone call has been initiated.
    Called,
}

/// Active sub-mode inside `LoginStep::PasswordCheck`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordCheckMode {
    /// Entering the 2FA cloud password.
    Password,
    /// Entering the email recovery code sent by `auth.requestPasswordRecovery`.
    Recovery {
        /// Masked recovery email pattern.
        email_pattern: String,
    },
    /// Recovery code was verified; ready to finalize `auth.recoverPassword`.
    RecoveryVerified {
        /// Verified recovery code.
        code: String,
    },
}

/// Parsed 2FA password state (`Core::CloudPasswordState`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudPasswordState {
    /// Active ModPow algorithm (if supported by `tg-crypto::srp`).
    pub current_algo: Option<PasswordKdfAlgoModPow>,
    /// Server SRP session ID (`0` if none / already consumed).
    pub srp_id: i64,
    /// Server SRP ephemeral public value `B`.
    pub srp_b: Vec<u8>,
    /// Validated `new_algo` with 32 random client salt bytes appended to `salt1`
    /// (`Core::ValidateNewCloudPasswordAlgo`, `core/core_cloud_password.cpp:201-210`).
    pub new_algo: Option<Box<PasswordKdfAlgoModPow>>,
    /// Validated `new_secure_algo` with 32 random client salt bytes appended
    /// (`Core::ValidateNewSecureSecretAlgo`, `core/core_cloud_password.cpp:272-281`).
    pub new_secure_algo: Option<Box<srp::SecureSecretAlgo>>,
    /// Whether the server reported an unknown current algorithm.
    pub unknown_algorithm: bool,
    /// Whether any of `current_algo`, `new_algo`, or `new_secure_algo` is unknown
    /// (`Core::ParseCloudPasswordState`, `core/core_cloud_password.cpp:322-353`).
    pub outdated_client: bool,
    /// Whether a password is set and computable.
    pub has_password: bool,
    /// Whether recovery email is available.
    pub has_recovery: bool,
    /// Whether Telegram Passport data would be wiped on password reset.
    pub not_empty_passport: bool,
    /// Optional password hint.
    pub hint: String,
    /// Optional unconfirmed recovery email pattern.
    pub unconfirmed_pattern: String,
    /// Optional login email pattern.
    pub login_email_pattern: String,
    /// Pending reset date timestamp (`0` if none).
    pub pending_reset_date: TimeId,
}

impl CloudPasswordState {
    /// Parses generated `tg_tl::types::account::Password` into `CloudPasswordState`
    /// (`Core::ParseCloudPasswordState`, `core/core_cloud_password.cpp:303-356`).
    #[must_use]
    pub fn from_account_password(data: &tl_types::account::Password) -> Self {
        let (current_algo, bad_current_algo) = match &data.current_algo {
            Some(
                tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(m),
            ) => (
                Some(PasswordKdfAlgoModPow {
                    salt1: m.salt1.clone(),
                    salt2: m.salt2.clone(),
                    g: m.g,
                    p: m.p.clone(),
                }),
                false,
            ),
            Some(tl_enums::PasswordKdfAlgo::Unknown) => (None, true),
            None => (None, false),
        };
        let has_password = current_algo.is_some()
            && data.srp_id.unwrap_or(0) != 0
            && data.srp_b.as_ref().is_some_and(|b| !b.is_empty());
        // Match `core/core_cloud_password.cpp:308`: `unknownAlgorithm = data.vcurrent_algo() && !hasPassword`.
        let unknown_algorithm = data.current_algo.is_some() && !has_password;

        let (new_algo, bad_new_algo) = match &data.new_algo {
            tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(m) => (
                Some(Box::new(srp::validate_new_algo(PasswordKdfAlgoModPow {
                    salt1: m.salt1.clone(),
                    salt2: m.salt2.clone(),
                    g: m.g,
                    p: m.p.clone(),
                }))),
                false,
            ),
            tl_enums::PasswordKdfAlgo::Unknown => (None, true),
        };

        let (new_secure_algo, bad_secure_algo) = match &data.new_secure_algo {
            tl_enums::SecurePasswordKdfAlgo::PBKDF2HMACSHA512iter100000(s) => (
                srp::validate_new_secure_secret_algo(
                    srp::SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 {
                        salt: s.salt.clone(),
                    },
                )
                .ok()
                .map(Box::new),
                false,
            ),
            tl_enums::SecurePasswordKdfAlgo::SHA512(_) => (None, false),
            tl_enums::SecurePasswordKdfAlgo::Unknown => (None, true),
        };

        let outdated_client = bad_secure_algo || bad_current_algo || bad_new_algo;

        Self {
            current_algo,
            srp_id: data.srp_id.unwrap_or(0),
            srp_b: data.srp_b.clone().unwrap_or_default(),
            new_algo,
            new_secure_algo,
            unknown_algorithm,
            outdated_client,
            has_password,
            has_recovery: data.has_recovery,
            not_empty_passport: data.has_secure_values,
            hint: data.hint.clone().unwrap_or_default(),
            unconfirmed_pattern: data.email_unconfirmed_pattern.clone().unwrap_or_default(),
            login_email_pattern: data.login_email_pattern.clone().unwrap_or_default(),
            pending_reset_date: data.pending_reset_date.unwrap_or(0),
        }
    }
}

/// Code delivery state tracked during `LoginStep::CodeInput`.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeInputState {
    /// Normalized phone digits (`DigitsOnly`).
    pub phone: String,
    /// Server `phone_code_hash`.
    pub phone_hash: String,
    /// Whether the code was delivered via another Telegram app session.
    pub code_by_telegram: bool,
    /// Non-empty when code must be retrieved via Fragment URL.
    pub fragment_url: Option<String>,
    /// Non-empty when signing in via login email code.
    pub email_pattern_login: Option<String>,
    /// Expected number of digits in the verification code.
    pub code_length: i32,
    /// Optional fallback delivery channel.
    pub next_type: Option<tl_enums::auth::CodeType>,
    /// Phone-call fallback countdown status.
    pub call_status: CallStatus,
    /// Seconds remaining before auto-calling `auth.resendCode`.
    pub call_timeout_secs: i32,
}

/// QR login state tracked during `LoginStep::QrLogin`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrLoginState {
    /// Raw login token bytes.
    pub token: Vec<u8>,
    /// Token expiration unix timestamp (seconds).
    pub expires: TimeId,
    /// Encoded `tg://login?token=<base64url>` URI.
    pub qr_url: String,
    /// Recommended delay in seconds before calling `refresh_qr_token`.
    pub refresh_after_secs: u64,
    /// Whether an `updateLoginToken` arrived while a request was in flight.
    pub force_refresh: bool,
    /// Already-authorized user IDs excluded from QR login.
    pub except_ids: Vec<i64>,
}

/// Current step of the Telegram Desktop login state machine (`intro/`).
#[derive(Debug, Clone, PartialEq)]
pub enum LoginStep {
    /// Initial welcome step (`intro_start.cpp`).
    Start {
        /// Nearest DC response if fetched.
        nearest_dc: Option<tl_types::NearestDc>,
        /// Suggested ISO2 country code.
        country: String,
    },
    /// Phone number entry step (`intro_phone.cpp`).
    PhoneInput,
    /// Mandatory login email setup step (`intro_email.cpp`).
    EmailSetup {
        /// Phone digits.
        phone: String,
        /// Server `phone_code_hash`.
        phone_hash: String,
    },
    /// Confirming newly configured login email (`intro_code.cpp` with `emailPatternSetup`).
    EmailVerify {
        /// Phone digits.
        phone: String,
        /// Server `phone_code_hash`.
        phone_hash: String,
        /// Email address entered by user.
        email: String,
        /// Masked email pattern.
        email_pattern: String,
        /// Expected code length.
        code_length: i32,
    },
    /// Verification code input (`intro_code.cpp`).
    CodeInput(CodeInputState),
    /// Two-step verification password / recovery (`intro_password_check.cpp`).
    PasswordCheck {
        /// Phone digits (empty if entered from QR login).
        phone: String,
        /// Server `phone_code_hash` (empty if entered from QR login).
        phone_hash: String,
        /// Parsed cloud password state.
        password_state: CloudPasswordState,
        /// Active password vs. recovery mode.
        mode: PasswordCheckMode,
        /// Whether this step was entered from QR login (determines back/reset target).
        from_qr: bool,
    },
    /// New account sign-up (`intro_signup.cpp`).
    SignUp {
        /// Phone digits.
        phone: String,
        /// Server `phone_code_hash`.
        phone_hash: String,
        /// Optional Terms of Service returned by server.
        terms_of_service: Option<tl_types::help::TermsOfService>,
        /// Whether the user accepted the Terms of Service popup.
        terms_accepted: bool,
    },
    /// QR code login (`intro_qr.cpp`).
    QrLogin(QrLoginState),
    /// Login completed (`intro_step.cpp::finish` / `createSession`).
    Authorized {
        /// Authorized self TL user.
        user: Box<tl_types::User>,
        /// Optional temporary sessions TTL.
        tmp_sessions: Option<i32>,
        /// Whether server requested 2FA password setup.
        setup_password_required: bool,
        /// Dialog filters preloaded during `Step::finish`.
        preloaded_filters: tl_types::messages::DialogFilters,
        /// Whether authorization completed via 2FA password recovery.
        password_recovered: bool,
    },
}

/// Strips all non-ASCII-digit characters (`Intro::details::DigitsOnly`).
#[must_use]
pub fn digits_only(phone: &str) -> String {
    phone.chars().filter(char::is_ascii_digit).collect()
}

/// Returns `true` if `phone` contains more than 1 digit (`Intro::details::AllowPhoneAttempt`).
#[must_use]
pub fn allow_phone_attempt(phone: &str) -> bool {
    phone.chars().filter(char::is_ascii_digit).count() > 1
}

/// Encodes raw bytes with base64url (matching Qt's `QByteArray::toBase64(QByteArray::Base64UrlEncoding)`).
#[must_use]
pub fn encode_base64url(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = if chunk.len() > 1 {
            u32::from(chunk[1])
        } else {
            0
        };
        let b2 = if chunk.len() > 2 {
            u32::from(chunk[2])
        } else {
            0
        };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Formats a QR login URI (`tg://login?token=...`) from raw token bytes (`QrWidget::showToken`).
#[must_use]
pub fn format_qr_login_url(token: &[u8]) -> String {
    format!("tg://login?token={}", encode_base64url(token))
}

/// Stateful login coordinator implementing tdesktop's `intro/` flows and step navigation stack.
#[derive(Debug)]
pub struct LoginStateMachine {
    credentials: ApiCredentials,
    step: LoginStep,
    step_stack: Vec<LoginStep>,
    code_settings: tl_types::CodeSettings,
    last_srp_id_invalid_at: Option<Instant>,
    qr_request_in_flight: bool,
}

impl LoginStateMachine {
    /// Creates a new login state machine starting at `LoginStep::Start`.
    #[must_use]
    pub fn new(credentials: ApiCredentials, initial_country: impl Into<String>) -> Self {
        Self {
            credentials,
            step: LoginStep::Start {
                nearest_dc: None,
                country: initial_country.into(),
            },
            step_stack: Vec::new(),
            code_settings: code_settings_desktop_default(),
            last_srp_id_invalid_at: None,
            qr_request_in_flight: false,
        }
    }

    /// Creates a new login state machine starting directly at `LoginStep::PhoneInput`.
    #[must_use]
    pub fn at_phone_input(credentials: ApiCredentials) -> Self {
        Self {
            credentials,
            step: LoginStep::PhoneInput,
            step_stack: Vec::new(),
            code_settings: code_settings_desktop_default(),
            last_srp_id_invalid_at: None,
            qr_request_in_flight: false,
        }
    }

    /// Overrides the `CodeSettings` sent in `auth.sendCode` (keeping `allow_flashcall = false`
    /// and `allow_firebase = false` as required on desktop).
    pub fn set_code_settings(&mut self, mut settings: tl_types::CodeSettings) {
        settings.allow_flashcall = false;
        settings.allow_firebase = false;
        self.code_settings = settings;
    }

    /// Returns a reference to the current `LoginStep`.
    #[must_use]
    pub fn step(&self) -> &LoginStep {
        &self.step
    }

    /// Returns the number of previous steps stored on the navigation stack.
    #[must_use]
    pub fn step_stack_len(&self) -> usize {
        self.step_stack.len()
    }

    /// Pops the previous step from the navigation stack (`Intro::Widget::goBack`),
    /// returning `true` if a previous step was restored.
    pub fn go_back(&mut self) -> bool {
        if let Some(prev) = self.step_stack.pop() {
            self.step = prev;
            true
        } else {
            false
        }
    }

    /// Moves from `Start` (or QR) to `PhoneInput`, pushing the current step onto the stack.
    pub fn enter_phone_input(&mut self) {
        self.push_step(LoginStep::PhoneInput);
    }

    /// Fetches nearest DC & country (`Intro::Widget::getNearestDC`).
    pub async fn fetch_nearest_dc<I: Invoker>(
        &mut self,
        invoker: &I,
    ) -> Result<tl_types::NearestDc, ApiError> {
        let nearest = invoker.help_get_nearest_dc().await?;
        invoker.set_main_dc_id(nearest.nearest_dc);
        if let LoginStep::Start {
            nearest_dc,
            country,
        } = &mut self.step
        {
            *country = nearest.country.clone();
            *nearest_dc = Some(nearest.clone());
        }
        Ok(nearest)
    }

    /// Submits a phone number (`PhoneWidget::submit`), handling `PHONE_MIGRATE_X` automatically.
    pub async fn submit_phone<I: Invoker>(
        &mut self,
        invoker: &I,
        phone: &str,
    ) -> Result<&LoginStep, ApiError> {
        if !allow_phone_attempt(phone) {
            return Err(ApiError::Login(LoginErrorKind::PhoneNumberInvalid));
        }

        let req = tl_fns::auth::SendCode {
            phone_number: phone.to_owned(),
            api_id: self.credentials.api_id,
            api_hash: self.credentials.api_hash.clone(),
            settings: tl_enums::CodeSettings::CodeSettings(self.code_settings.clone()),
        };

        let mut attempts = 0;
        let sent_code = loop {
            match invoker.auth_send_code(req.clone()).await {
                Ok(res) => break res,
                Err(InvocationError::Rpc(rpc)) => {
                    if let Some(target_dc) = rpc.migrate_dc() {
                        attempts += 1;
                        if attempts <= MAX_MIGRATE_RETRIES {
                            debug!(
                                target_dc,
                                attempts, "Handling PHONE_MIGRATE by switching main DC"
                            );
                            invoker.set_main_dc_id(target_dc);
                            continue;
                        }
                    }
                    return Err(ApiError::Login(map_login_error(&rpc)));
                }
                Err(other) => return Err(ApiError::Invocation(other)),
            }
        };

        self.apply_sent_code(invoker, &digits_only(phone), sent_code, true)
            .await?;
        Ok(&self.step)
    }

    /// Submits an email address when `LoginStep::EmailSetup` is active (`EmailWidget::submit`).
    pub async fn submit_email_setup<I: Invoker>(
        &mut self,
        invoker: &I,
        email: &str,
    ) -> Result<&LoginStep, ApiError> {
        let (phone, phone_hash) = match &self.step {
            LoginStep::EmailSetup { phone, phone_hash } => (phone.clone(), phone_hash.clone()),
            _ => {
                return Err(ApiError::InvalidState(
                    "submit_email_setup requires EmailSetup step".into(),
                ));
            }
        };
        if email.trim().is_empty() {
            return Err(ApiError::Login(LoginErrorKind::EmailInvalid));
        }
        let sent = invoker
            .account_send_verify_email_code(tl_fns::account::SendVerifyEmailCode {
                purpose: tl_enums::EmailVerifyPurpose::LoginSetup(
                    tl_types::EmailVerifyPurposeLoginSetup {
                        phone_number: phone.clone(),
                        phone_code_hash: phone_hash.clone(),
                    },
                ),
                email: email.to_owned(),
            })
            .await
            .map_err(map_invocation_login_error)?;

        self.push_step(LoginStep::EmailVerify {
            phone,
            phone_hash,
            email: email.to_owned(),
            email_pattern: sent.email_pattern,
            code_length: sent.length,
        });
        Ok(&self.step)
    }

    /// Submits a verification code in `CodeInput` or `EmailVerify` (`CodeWidget::submitCode`).
    pub async fn submit_code<I: Invoker>(
        &mut self,
        invoker: &I,
        code: &str,
    ) -> Result<&LoginStep, ApiError> {
        match self.step.clone() {
            LoginStep::EmailVerify {
                phone,
                phone_hash,
                code_length,
                ..
            } => {
                if code.len() as i32 != code_length {
                    return Err(ApiError::Login(LoginErrorKind::PhoneCodeInvalid));
                }
                let verified = invoker
                    .account_verify_email(tl_fns::account::VerifyEmail {
                        purpose: tl_enums::EmailVerifyPurpose::LoginSetup(
                            tl_types::EmailVerifyPurposeLoginSetup {
                                phone_number: phone.clone(),
                                phone_code_hash: phone_hash,
                            },
                        ),
                        verification: tl_enums::EmailVerification::Code(
                            tl_types::EmailVerificationCode {
                                code: code.to_owned(),
                            },
                        ),
                    })
                    .await
                    .map_err(map_invocation_login_error)?;
                match verified {
                    tl_enums::account::EmailVerified::EmailVerified(_) => {
                        Err(ApiError::ServerError(
                            "Unexpected response: emailVerified instead of emailVerifiedLogin"
                                .into(),
                        ))
                    }
                    tl_enums::account::EmailVerified::Login(v) => {
                        self.apply_sent_code(invoker, &phone, v.sent_code, true)
                            .await?;
                        Ok(&self.step)
                    }
                }
            }
            LoginStep::CodeInput(state) => {
                if state.code_length > 0 && code.len() as i32 != state.code_length {
                    return Err(ApiError::Login(LoginErrorKind::PhoneCodeInvalid));
                }
                let (phone_code, email_verification) = if state.email_pattern_login.is_some() {
                    (
                        None,
                        Some(tl_enums::EmailVerification::Code(
                            tl_types::EmailVerificationCode {
                                code: code.to_owned(),
                            },
                        )),
                    )
                } else {
                    (Some(code.to_owned()), None)
                };
                let result = invoker
                    .auth_sign_in(tl_fns::auth::SignIn {
                        phone_number: state.phone.clone(),
                        phone_code_hash: state.phone_hash.clone(),
                        phone_code,
                        email_verification,
                    })
                    .await;
                match result {
                    Ok(auth) => {
                        self.finish_authorization(
                            invoker,
                            auth,
                            state.phone,
                            state.phone_hash,
                            false,
                        )
                        .await?;
                        Ok(&self.step)
                    }
                    Err(InvocationError::Rpc(rpc)) => {
                        let mapped = map_login_error(&rpc);
                        match mapped {
                            LoginErrorKind::SessionPasswordNeeded => {
                                self.enter_password_check(
                                    invoker,
                                    state.phone,
                                    state.phone_hash,
                                    false,
                                )
                                .await?;
                                Ok(&self.step)
                            }
                            LoginErrorKind::PhoneNumberInvalid
                            | LoginErrorKind::PhoneCodeExpired
                            | LoginErrorKind::PhoneNumberBanned => {
                                self.reset_to_phone_input();
                                Err(ApiError::Login(mapped))
                            }
                            other => Err(ApiError::Login(other)),
                        }
                    }
                    Err(other) => Err(ApiError::Invocation(other)),
                }
            }
            _ => Err(ApiError::InvalidState(
                "submit_code requires CodeInput or EmailVerify step".into(),
            )),
        }
    }

    /// Manually resends the login code (`CodeWidget::noTelegramCode`).
    pub async fn resend_code<I: Invoker>(&mut self, invoker: &I) -> Result<&LoginStep, ApiError> {
        let (phone, phone_hash) = match &self.step {
            LoginStep::CodeInput(state) => (state.phone.clone(), state.phone_hash.clone()),
            _ => {
                return Err(ApiError::InvalidState(
                    "resend_code requires CodeInput step".into(),
                ));
            }
        };
        let sent = invoker
            .auth_resend_code(tl_fns::auth::ResendCode {
                phone_number: phone.clone(),
                phone_code_hash: phone_hash,
                reason: None,
            })
            .await
            .map_err(map_invocation_login_error)?;

        self.apply_sent_code(invoker, &phone, sent, false).await?;
        if let LoginStep::CodeInput(state) = &mut self.step {
            state.code_by_telegram = false;
        }
        Ok(&self.step)
    }

    /// Advances the 1-second call fallback timer (`CodeWidget::sendCall`).
    ///
    /// When the countdown reaches 0, automatically sends `auth.resendCode` and
    /// transitions `call_status` from `Waiting` -> `Calling` -> `Called`.
    pub async fn tick_call_countdown<I: Invoker>(
        &mut self,
        invoker: &I,
    ) -> Result<CallStatus, ApiError> {
        let (phone, phone_hash, should_call) = match &mut self.step {
            LoginStep::CodeInput(state) => {
                if state.code_by_telegram || state.call_status != CallStatus::Waiting {
                    return Ok(state.call_status);
                }
                state.call_timeout_secs -= 1;
                if state.call_timeout_secs <= 0 {
                    state.call_status = CallStatus::Calling;
                    (state.phone.clone(), state.phone_hash.clone(), true)
                } else {
                    return Ok(state.call_status);
                }
            }
            _ => {
                return Err(ApiError::InvalidState(
                    "tick_call_countdown requires CodeInput step".into(),
                ));
            }
        };

        if should_call {
            let sent = invoker
                .auth_resend_code(tl_fns::auth::ResendCode {
                    phone_number: phone.clone(),
                    phone_code_hash: phone_hash,
                    reason: None,
                })
                .await
                .map_err(map_invocation_login_error)?;
            self.apply_sent_code(invoker, &phone, sent, false).await?;
            if let LoginStep::CodeInput(state) = &mut self.step {
                state.call_status = CallStatus::Called;
                return Ok(state.call_status);
            }
        }
        Ok(CallStatus::Called)
    }

    /// Cancels the active login code and returns to `PhoneInput` (`CodeWidget::cancelled`).
    pub async fn cancel_code<I: Invoker>(&mut self, invoker: &I) -> Result<(), ApiError> {
        let (phone, phone_hash) = match &self.step {
            LoginStep::CodeInput(state) => (state.phone.clone(), state.phone_hash.clone()),
            LoginStep::EmailSetup { phone, phone_hash }
            | LoginStep::EmailVerify {
                phone, phone_hash, ..
            } => (phone.clone(), phone_hash.clone()),
            _ => {
                return Err(ApiError::InvalidState(
                    "cancel_code requires an active phone code step".into(),
                ));
            }
        };
        let _ = invoker
            .auth_cancel_code(tl_fns::auth::CancelCode {
                phone_number: phone,
                phone_code_hash: phone_hash,
            })
            .await;
        self.reset_to_phone_input();
        Ok(())
    }

    /// Submits the 2FA cloud password using `tg_crypto::srp`
    /// (`PasswordCheckWidget::submit` + `handleSrpIdInvalid`).
    ///
    /// On `PASSWORD_EMPTY` / `AUTH_KEY_UNREGISTERED`, mirrors tdesktop's `goBack()`
    /// by popping the step stack (restoring `QrLogin` when entered from QR, or the
    /// previous `CodeInput` state otherwise).
    pub async fn submit_password<I: Invoker>(
        &mut self,
        invoker: &I,
        password: &str,
        now: Instant,
    ) -> Result<&LoginStep, ApiError> {
        self.submit_password_internal(invoker, password, None, now)
            .await
    }

    /// Submits the 2FA cloud password using an explicit 256-byte SRP random `a` value so
    /// conformance tests can verify `A` and `M1` byte-for-byte against
    /// `core/core_cloud_password.cpp`.
    pub async fn submit_password_with_random<I: Invoker>(
        &mut self,
        invoker: &I,
        password: &str,
        random_a: [u8; 256],
        now: Instant,
    ) -> Result<&LoginStep, ApiError> {
        self.submit_password_internal(invoker, password, Some(random_a), now)
            .await
    }

    async fn submit_password_internal<I: Invoker>(
        &mut self,
        invoker: &I,
        password: &str,
        random_a: Option<[u8; 256]>,
        now: Instant,
    ) -> Result<&LoginStep, ApiError> {
        let (phone, phone_hash, mut pwd_state) = match &self.step {
            LoginStep::PasswordCheck {
                phone,
                phone_hash,
                password_state,
                ..
            } => (phone.clone(), phone_hash.clone(), password_state.clone()),
            _ => {
                return Err(ApiError::InvalidState(
                    "submit_password requires PasswordCheck step".into(),
                ));
            }
        };

        let initial_algo = pwd_state
            .current_algo
            .clone()
            .ok_or(ApiError::Login(LoginErrorKind::OutdatedClient))?;
        let mut password_hash = srp::compute_hash(&initial_algo, password.as_bytes());
        let mut hashed_for_algo = initial_algo;

        loop {
            if pwd_state.srp_id == 0 {
                let fresh = invoker
                    .account_get_password()
                    .await
                    .map_err(map_invocation_login_error)?;
                pwd_state = CloudPasswordState::from_account_password(&fresh);
                if !pwd_state.has_password {
                    if pwd_state.unknown_algorithm {
                        return Err(ApiError::Login(LoginErrorKind::OutdatedClient));
                    }
                    // Matches tdesktop `intro_password_check.cpp`: goBack() to QR or Code step.
                    self.go_back_or_phone_input();
                    return Err(ApiError::Login(LoginErrorKind::PasswordEmptyOrUnregistered));
                }
            }

            let active_algo = pwd_state
                .current_algo
                .as_ref()
                .ok_or(ApiError::Login(LoginErrorKind::OutdatedClient))?;
            if *active_algo != hashed_for_algo {
                password_hash = srp::compute_hash(active_algo, password.as_bytes());
                hashed_for_algo = active_algo.clone();
            }

            let check = match random_a {
                Some(a_bytes) => srp::compute_check_with_random(
                    active_algo,
                    &pwd_state.srp_b,
                    pwd_state.srp_id,
                    &password_hash,
                    &a_bytes,
                )?,
                None => srp::compute_check_with_hash(
                    active_algo,
                    &pwd_state.srp_b,
                    pwd_state.srp_id,
                    &password_hash,
                )?,
            };
            // Consume srp_id so a second attempt without a refresh fetches fresh parameters.
            pwd_state.srp_id = 0;

            let tl_check = tl_enums::InputCheckPasswordSRP::InputCheckPasswordSRP(
                tl_types::InputCheckPasswordSRP {
                    srp_id: check.srp_id,
                    a: check.a.to_vec(),
                    m1: check.m1.to_vec(),
                },
            );

            match invoker
                .auth_check_password(tl_fns::auth::CheckPassword { password: tl_check })
                .await
            {
                Ok(auth) => {
                    self.finish_authorization(invoker, auth, phone, phone_hash, false)
                        .await?;
                    return Ok(&self.step);
                }
                Err(InvocationError::Rpc(rpc)) => {
                    let mapped = map_login_error(&rpc);
                    match mapped {
                        LoginErrorKind::SrpIdInvalid => {
                            if let Some(last) = self.last_srp_id_invalid_at
                                && now.saturating_duration_since(last)
                                    < HANDLE_SRP_ID_INVALID_TIMEOUT
                            {
                                self.update_password_state(pwd_state);
                                return Err(ApiError::ServerError(
                                    "Repeated SRP_ID_INVALID within 60s".into(),
                                ));
                            }
                            self.last_srp_id_invalid_at = Some(now);
                            continue;
                        }
                        LoginErrorKind::PasswordEmptyOrUnregistered => {
                            self.go_back_or_phone_input();
                            return Err(ApiError::Login(mapped));
                        }
                        other => {
                            self.update_password_state(pwd_state);
                            return Err(ApiError::Login(other));
                        }
                    }
                }
                Err(other) => {
                    self.update_password_state(pwd_state);
                    return Err(ApiError::Invocation(other));
                }
            }
        }
    }

    /// Starts 2FA email password recovery (`PasswordCheckWidget::toRecover`).
    pub async fn request_password_recovery<I: Invoker>(
        &mut self,
        invoker: &I,
    ) -> Result<String, ApiError> {
        let has_recovery = match &self.step {
            LoginStep::PasswordCheck { password_state, .. } => password_state.has_recovery,
            _ => {
                return Err(ApiError::InvalidState(
                    "request_password_recovery requires PasswordCheck step".into(),
                ));
            }
        };
        if !has_recovery {
            return Err(ApiError::Login(LoginErrorKind::PasswordRecoveryUnavailable));
        }

        let recovery = invoker
            .auth_request_password_recovery()
            .await
            .map_err(map_invocation_login_error)?;
        if let LoginStep::PasswordCheck { mode, .. } = &mut self.step {
            *mode = PasswordCheckMode::Recovery {
                email_pattern: recovery.email_pattern.clone(),
            };
        }
        Ok(recovery.email_pattern)
    }

    /// Checks the 2FA email recovery code (`PasswordCheckWidget::submit` in recovery mode).
    pub async fn check_recovery_code<I: Invoker>(
        &mut self,
        invoker: &I,
        code: &str,
    ) -> Result<&LoginStep, ApiError> {
        let trimmed = code.trim();
        if trimmed.is_empty() {
            return Err(ApiError::Login(LoginErrorKind::RecoveryCodeInvalid));
        }
        if !matches!(self.step, LoginStep::PasswordCheck { .. }) {
            return Err(ApiError::InvalidState(
                "check_recovery_code requires PasswordCheck step".into(),
            ));
        }

        match invoker
            .auth_check_recovery_password(tl_fns::auth::CheckRecoveryPassword {
                code: trimmed.to_owned(),
            })
            .await
        {
            Ok(true) => {
                if let LoginStep::PasswordCheck { mode, .. } = &mut self.step {
                    *mode = PasswordCheckMode::RecoveryVerified {
                        code: trimmed.to_owned(),
                    };
                }
                Ok(&self.step)
            }
            Ok(false) => Err(ApiError::Login(LoginErrorKind::RecoveryCodeInvalid)),
            Err(InvocationError::Rpc(rpc)) => {
                let mapped = map_login_error(&rpc);
                match mapped {
                    LoginErrorKind::PasswordEmptyOrUnregistered => {
                        self.go_back_or_phone_input();
                    }
                    LoginErrorKind::PasswordRecoveryExpired => {
                        if let LoginStep::PasswordCheck { mode, .. } = &mut self.step {
                            *mode = PasswordCheckMode::Password;
                        }
                    }
                    _ => {}
                }
                Err(ApiError::Login(mapped))
            }
            Err(other) => Err(ApiError::Invocation(other)),
        }
    }

    /// Completes 2FA password recovery via `auth.recoverPassword` using a verified code.
    pub async fn complete_password_recovery<I: Invoker>(
        &mut self,
        invoker: &I,
    ) -> Result<&LoginStep, ApiError> {
        let (phone, phone_hash, code) = match &self.step {
            LoginStep::PasswordCheck {
                phone,
                phone_hash,
                mode: PasswordCheckMode::RecoveryVerified { code },
                ..
            } => (phone.clone(), phone_hash.clone(), code.clone()),
            _ => {
                return Err(ApiError::InvalidState(
                    "complete_password_recovery requires verified recovery code".into(),
                ));
            }
        };

        let auth = invoker
            .auth_recover_password(tl_fns::auth::RecoverPassword {
                code,
                new_settings: None,
            })
            .await
            .map_err(map_invocation_login_error)?;
        self.finish_authorization(invoker, auth, phone, phone_hash, true)
            .await?;
        Ok(&self.step)
    }

    /// Resets the account after forgetting the 2FA password (`Intro::Widget::resetAccount`).
    pub async fn reset_account<I: Invoker>(&mut self, invoker: &I) -> Result<&LoginStep, ApiError> {
        let (phone, phone_hash) = match &self.step {
            LoginStep::PasswordCheck {
                phone, phone_hash, ..
            } => (phone.clone(), phone_hash.clone()),
            _ => {
                return Err(ApiError::InvalidState(
                    "reset_account requires PasswordCheck step".into(),
                ));
            }
        };

        invoker
            .account_delete_account(tl_fns::account::DeleteAccount {
                reason: "Forgot password".to_owned(),
                password: None,
            })
            .await
            .map_err(map_invocation_login_error)?;

        if phone.is_empty() {
            self.reset_to_phone_input();
        } else {
            self.push_step(LoginStep::SignUp {
                phone,
                phone_hash,
                terms_of_service: None,
                terms_accepted: true,
            });
        }
        Ok(&self.step)
    }

    /// Marks the Terms of Service popup as accepted on `LoginStep::SignUp`.
    pub fn accept_terms(&mut self) -> Result<(), ApiError> {
        match &mut self.step {
            LoginStep::SignUp { terms_accepted, .. } => {
                *terms_accepted = true;
                Ok(())
            }
            _ => Err(ApiError::InvalidState(
                "accept_terms requires SignUp step".into(),
            )),
        }
    }

    /// Submits first/last name to complete new-user registration (`SignupWidget::submit`).
    pub async fn submit_sign_up<I: Invoker>(
        &mut self,
        invoker: &I,
        first_name: &str,
        last_name: &str,
    ) -> Result<&LoginStep, ApiError> {
        let (phone, phone_hash, terms, terms_accepted) = match &self.step {
            LoginStep::SignUp {
                phone,
                phone_hash,
                terms_of_service,
                terms_accepted,
            } => (
                phone.clone(),
                phone_hash.clone(),
                terms_of_service.clone(),
                *terms_accepted,
            ),
            _ => {
                return Err(ApiError::InvalidState(
                    "submit_sign_up requires SignUp step".into(),
                ));
            }
        };

        let first = first_name.trim();
        let last = last_name.trim();
        if first.is_empty() {
            return Err(ApiError::Login(LoginErrorKind::FirstNameInvalid));
        }
        if let Some(tos) = &terms
            && tos.popup
            && !tos.text.is_empty()
            && !terms_accepted
        {
            return Err(ApiError::Login(LoginErrorKind::TermsNotAccepted));
        }

        match invoker
            .auth_sign_up(tl_fns::auth::SignUp {
                no_joined_notifications: false,
                phone_number: phone.clone(),
                phone_code_hash: phone_hash.clone(),
                first_name: first.to_owned(),
                last_name: last.to_owned(),
            })
            .await
        {
            Ok(auth) => {
                self.finish_authorization(invoker, auth, phone, phone_hash, false)
                    .await?;
                Ok(&self.step)
            }
            Err(InvocationError::Rpc(rpc)) => {
                let mapped = map_login_error(&rpc);
                match mapped {
                    LoginErrorKind::PhoneNumberInvalid
                    | LoginErrorKind::PhoneNumberBanned
                    | LoginErrorKind::PhoneCodeExpired
                    | LoginErrorKind::PhoneCodeInvalid
                    | LoginErrorKind::PhoneNumberOccupied => {
                        self.reset_to_phone_input();
                    }
                    _ => {}
                }
                Err(ApiError::Login(mapped))
            }
            Err(other) => Err(ApiError::Invocation(other)),
        }
    }

    /// Starts QR code login (`QrWidget::refreshCode`).
    pub async fn start_qr_login<I: Invoker>(
        &mut self,
        invoker: &I,
        now_unix: TimeId,
        except_ids: Vec<i64>,
    ) -> Result<&LoginStep, ApiError> {
        self.push_step(LoginStep::QrLogin(QrLoginState {
            token: Vec::new(),
            expires: 0,
            qr_url: String::new(),
            refresh_after_secs: 1,
            force_refresh: false,
            except_ids,
        }));
        self.refresh_qr_token(invoker, now_unix).await
    }

    /// Exports or refreshes the QR login token (`QrWidget::refreshCode`, `handleTokenResult`,
    /// `importTo`).
    ///
    /// Automatically migrates DC via `auth.importLoginToken` when the server returns
    /// `LoginToken::MigrateTo`, transitions to `PasswordCheck` on `SESSION_PASSWORD_NEEDED`,
    /// and completes authorization on `LoginToken::Success`.
    pub async fn refresh_qr_token<I: Invoker>(
        &mut self,
        invoker: &I,
        now_unix: TimeId,
    ) -> Result<&LoginStep, ApiError> {
        let except_ids = match &self.step {
            LoginStep::QrLogin(qr) => qr.except_ids.clone(),
            _ => {
                return Err(ApiError::InvalidState(
                    "refresh_qr_token requires QrLogin step".into(),
                ));
            }
        };

        self.qr_request_in_flight = true;
        let result = invoker
            .auth_export_login_token(tl_fns::auth::ExportLoginToken {
                api_id: self.credentials.api_id,
                api_hash: self.credentials.api_hash.clone(),
                except_ids,
            })
            .await;
        self.qr_request_in_flight = false;

        match result {
            Ok(token) => self.handle_qr_token_result(invoker, token, now_unix).await,
            Err(InvocationError::Rpc(rpc)) => {
                let mapped = map_login_error(&rpc);
                if mapped == LoginErrorKind::SessionPasswordNeeded {
                    self.enter_password_check(invoker, String::new(), String::new(), true)
                        .await?;
                    Ok(&self.step)
                } else {
                    // Match `QrWidget::showTokenError` (`intro/intro_qr.cpp:480-481`):
                    // if `updateLoginToken` arrived while the failed request was in flight,
                    // consume `force_refresh` and immediately retry `exportLoginToken`.
                    let retry = if let LoginStep::QrLogin(qr) = &mut self.step {
                        std::mem::take(&mut qr.force_refresh)
                    } else {
                        false
                    };
                    if retry {
                        return Box::pin(self.refresh_qr_token(invoker, now_unix)).await;
                    }
                    Err(ApiError::Login(mapped))
                }
            }
            Err(other) => Err(ApiError::Invocation(other)),
        }
    }

    /// Processes an incoming TL `Updates` while on `LoginStep::QrLogin`
    /// (`QrWidget::checkForTokenUpdate`).
    ///
    /// If `Update::LoginToken` is present, triggers an immediate token refresh
    /// (or flags `force_refresh` if a request is already in flight).
    pub async fn on_qr_updates<I: Invoker>(
        &mut self,
        invoker: &I,
        updates: &tl_enums::Updates,
        now_unix: TimeId,
    ) -> Result<bool, ApiError> {
        if !contains_login_token_update(updates) {
            return Ok(false);
        }
        if let LoginStep::QrLogin(qr) = &mut self.step {
            if self.qr_request_in_flight {
                qr.force_refresh = true;
                return Ok(true);
            }
        } else {
            return Ok(false);
        }
        self.refresh_qr_token(invoker, now_unix).await?;
        Ok(true)
    }

    /// Logs out the current authorized session (`Main::Account::logOut`).
    pub async fn log_out<I: Invoker>(
        &mut self,
        invoker: &I,
    ) -> Result<tl_types::auth::LoggedOut, ApiError> {
        let logged_out = invoker
            .auth_log_out()
            .await
            .map_err(map_invocation_login_error)?;
        self.reset_to_phone_input();
        Ok(logged_out)
    }

    // --- Internal helpers ---

    fn push_step(&mut self, next: LoginStep) {
        let prev = std::mem::replace(&mut self.step, next);
        self.step_stack.push(prev);
    }

    fn reset_to_phone_input(&mut self) {
        self.step_stack.clear();
        self.step = LoginStep::PhoneInput;
    }

    fn go_back_or_phone_input(&mut self) {
        if !self.go_back() {
            self.step = LoginStep::PhoneInput;
        }
    }

    async fn handle_qr_token_result<I: Invoker>(
        &mut self,
        invoker: &I,
        mut token: tl_enums::auth::LoginToken,
        now_unix: TimeId,
    ) -> Result<&LoginStep, ApiError> {
        let mut migrations = 0;
        loop {
            match token {
                tl_enums::auth::LoginToken::LoginToken(tok) => {
                    let qr_url = format_qr_login_url(&tok.token);
                    let refresh_after_secs = i64::from(tok.expires)
                        .saturating_sub(i64::from(now_unix))
                        .max(1) as u64;
                    let force_refresh = if let LoginStep::QrLogin(qr) = &mut self.step {
                        qr.token = tok.token;
                        qr.expires = tok.expires;
                        qr.qr_url = qr_url;
                        qr.refresh_after_secs = refresh_after_secs;
                        std::mem::take(&mut qr.force_refresh)
                    } else {
                        false
                    };
                    if force_refresh {
                        return Box::pin(self.refresh_qr_token(invoker, now_unix)).await;
                    }
                    return Ok(&self.step);
                }
                tl_enums::auth::LoginToken::MigrateTo(mig) => {
                    migrations += 1;
                    if migrations > MAX_MIGRATE_RETRIES {
                        return Err(ApiError::ServerError(
                            "Too many QR login DC migrations".into(),
                        ));
                    }
                    debug!(dc_id = mig.dc_id, "Migrating QR login token to target DC");
                    invoker.set_main_dc_id(mig.dc_id);
                    match invoker
                        .auth_import_login_token(
                            mig.dc_id,
                            tl_fns::auth::ImportLoginToken { token: mig.token },
                        )
                        .await
                    {
                        Ok(next) => token = next,
                        Err(InvocationError::Rpc(rpc)) => {
                            let mapped = map_login_error(&rpc);
                            if mapped == LoginErrorKind::SessionPasswordNeeded {
                                self.enter_password_check(
                                    invoker,
                                    String::new(),
                                    String::new(),
                                    true,
                                )
                                .await?;
                                return Ok(&self.step);
                            }
                            return Err(ApiError::Login(mapped));
                        }
                        Err(other) => return Err(ApiError::Invocation(other)),
                    }
                }
                tl_enums::auth::LoginToken::Success(succ) => {
                    self.finish_authorization(
                        invoker,
                        succ.authorization,
                        String::new(),
                        String::new(),
                        false,
                    )
                    .await?;
                    return Ok(&self.step);
                }
            }
        }
    }

    async fn apply_sent_code<I: Invoker>(
        &mut self,
        invoker: &I,
        phone_digits: &str,
        sent: tl_enums::auth::SentCode,
        push_history: bool,
    ) -> Result<(), ApiError> {
        match sent {
            tl_enums::auth::SentCode::Success(succ) => {
                self.finish_authorization(
                    invoker,
                    succ.authorization,
                    phone_digits.to_owned(),
                    String::new(),
                    false,
                )
                .await
            }
            tl_enums::auth::SentCode::PaymentRequired(_) => {
                warn!("Unexpected auth.sentCodePaymentRequired on desktop");
                Err(ApiError::ServerError(
                    "Unexpected auth.sentCodePaymentRequired".into(),
                ))
            }
            tl_enums::auth::SentCode::SentCode(data) => {
                self.apply_sent_code_data(phone_digits, data, push_history);
                Ok(())
            }
        }
    }

    fn apply_sent_code_data(
        &mut self,
        phone_digits: &str,
        data: tl_types::auth::SentCode,
        push_history: bool,
    ) {
        let mut code_by_telegram = false;
        let mut fragment_url = None;
        let mut email_pattern_login = None;
        let mut code_length = 0;

        match data.r#type {
            tl_enums::auth::SentCodeType::App(t) => {
                code_by_telegram = true;
                code_length = t.length;
            }
            tl_enums::auth::SentCodeType::Sms(t) => {
                code_length = t.length;
            }
            tl_enums::auth::SentCodeType::Call(t) => {
                code_length = t.length;
            }
            tl_enums::auth::SentCodeType::FragmentSms(t) => {
                fragment_url = Some(t.url);
                code_length = t.length;
            }
            tl_enums::auth::SentCodeType::EmailCode(t) => {
                email_pattern_login = Some(t.email_pattern);
                code_length = t.length;
            }
            tl_enums::auth::SentCodeType::SetUpEmailRequired(_) => {
                let next = LoginStep::EmailSetup {
                    phone: phone_digits.to_owned(),
                    phone_hash: data.phone_code_hash,
                };
                if push_history {
                    self.push_step(next);
                } else {
                    self.step = next;
                }
                return;
            }
            tl_enums::auth::SentCodeType::FlashCall(_) => {
                warn!("Unexpected SentCodeType::FlashCall on desktop — skipped");
            }
            tl_enums::auth::SentCodeType::MissedCall(_) => {
                warn!("Unexpected SentCodeType::MissedCall on desktop — skipped");
            }
            tl_enums::auth::SentCodeType::FirebaseSms(_) => {
                warn!("Unexpected SentCodeType::FirebaseSms on desktop — skipped");
            }
            tl_enums::auth::SentCodeType::SmsWord(_) => {
                warn!("Unexpected SentCodeType::SmsWord on desktop — skipped");
            }
            tl_enums::auth::SentCodeType::SmsPhrase(_) => {
                warn!("Unexpected SentCodeType::SmsPhrase on desktop — skipped");
            }
        }

        let (call_status, call_timeout_secs) =
            if data.next_type == Some(tl_enums::auth::CodeType::Call) {
                (
                    CallStatus::Waiting,
                    data.timeout.unwrap_or(DEFAULT_CALL_TIMEOUT_SECS),
                )
            } else {
                (CallStatus::Disabled, 0)
            };

        let next = LoginStep::CodeInput(CodeInputState {
            phone: phone_digits.to_owned(),
            phone_hash: data.phone_code_hash,
            code_by_telegram,
            fragment_url,
            email_pattern_login,
            code_length,
            next_type: data.next_type,
            call_status,
            call_timeout_secs,
        });
        if push_history {
            self.push_step(next);
        } else {
            self.step = next;
        }
    }

    async fn enter_password_check<I: Invoker>(
        &mut self,
        invoker: &I,
        phone: String,
        phone_hash: String,
        from_qr: bool,
    ) -> Result<(), ApiError> {
        let pwd = invoker
            .account_get_password()
            .await
            .map_err(map_invocation_login_error)?;
        let state = CloudPasswordState::from_account_password(&pwd);
        if pwd.current_algo.is_none() || pwd.srp_id.is_none() || pwd.srp_b.is_none() {
            return Err(ApiError::ServerError(
                "No current password parameters received on login".into(),
            ));
        }
        if !state.has_password {
            return Err(ApiError::Login(LoginErrorKind::OutdatedClient));
        }
        self.push_step(LoginStep::PasswordCheck {
            phone,
            phone_hash,
            password_state: state,
            mode: PasswordCheckMode::Password,
            from_qr,
        });
        Ok(())
    }

    async fn finish_authorization<I: Invoker>(
        &mut self,
        invoker: &I,
        auth: tl_enums::auth::Authorization,
        phone: String,
        phone_hash: String,
        password_recovered: bool,
    ) -> Result<(), ApiError> {
        match auth {
            tl_enums::auth::Authorization::SignUpRequired(req) => {
                let terms_of_service = req.terms_of_service.map(|tos| {
                    let tl_enums::help::TermsOfService::TermsOfService(t) = tos;
                    t
                });
                self.push_step(LoginStep::SignUp {
                    phone,
                    phone_hash,
                    terms_of_service,
                    terms_accepted: false,
                });
                Ok(())
            }
            tl_enums::auth::Authorization::Authorization(auth_data) => {
                let tl_enums::User::User(user) = auth_data.user else {
                    return Err(ApiError::ServerError(
                        "Invalid self user returned by authorization".into(),
                    ));
                };
                if !user.self_ || user.id == 0 {
                    return Err(ApiError::ServerError(
                        "Invalid self user returned by authorization".into(),
                    ));
                }
                // Matches Step::finish(): preload dialog filters, falling back to empty on error.
                let preloaded_filters =
                    invoker
                        .messages_get_dialog_filters()
                        .await
                        .unwrap_or_else(|_| tl_types::messages::DialogFilters {
                            tags_enabled: false,
                            filters: Vec::new(),
                        });
                self.step_stack.clear();
                self.step = LoginStep::Authorized {
                    user: Box::new(user),
                    tmp_sessions: auth_data.tmp_sessions,
                    setup_password_required: auth_data.setup_password_required,
                    preloaded_filters,
                    password_recovered,
                };
                Ok(())
            }
        }
    }

    fn update_password_state(&mut self, new_state: CloudPasswordState) {
        if let LoginStep::PasswordCheck { password_state, .. } = &mut self.step {
            *password_state = new_state;
        }
    }
}

fn map_invocation_login_error(err: InvocationError) -> ApiError {
    match err {
        InvocationError::Rpc(rpc) => ApiError::Login(map_login_error(&rpc)),
        other => ApiError::Invocation(other),
    }
}

fn contains_login_token_update(updates: &tl_enums::Updates) -> bool {
    match updates {
        tl_enums::Updates::UpdateShort(s) => matches!(s.update, tl_enums::Update::LoginToken),
        tl_enums::Updates::Updates(u) => u
            .updates
            .iter()
            .any(|item| matches!(item, tl_enums::Update::LoginToken)),
        tl_enums::Updates::Combined(c) => c
            .updates
            .iter()
            .any(|item| matches!(item, tl_enums::Update::LoginToken)),
        _ => false,
    }
}
