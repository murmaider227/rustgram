//! Error types and tdesktop RPC error-to-UX mappings.
//!
//! Port origin: `intro/intro_phone.cpp`, `intro/intro_code.cpp`,
//! `intro/intro_password_check.cpp`, `intro/intro_signup.cpp`,
//! `intro/intro_email.cpp`, `intro/intro_widget.cpp`, `apiwrap.cpp`.

use crate::rpc::{DcId, InvocationError, RpcError};
use thiserror::Error;

/// Top-level error returned by `tg-api` operations.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// Underlying MTProto/RPC invocation error.
    #[error("invocation error: {0}")]
    Invocation(#[from] InvocationError),
    /// SRP or cryptographic check failed (`tg-crypto`).
    #[error("SRP error: {0}")]
    Srp(String),
    /// Operation was called in an invalid login or workflow state.
    #[error("invalid state: {0}")]
    InvalidState(String),
    /// Local input validation failed before sending a network request.
    #[error("validation error: {0}")]
    Validation(String),
    /// Server returned an unexpected/malformed response payload.
    #[error("unexpected server response: {0}")]
    ServerError(String),
    /// Login requires user action or a mapped login error occurred.
    #[error("login error: {0}")]
    Login(LoginErrorKind),
}

impl From<tg_crypto::CryptoError> for ApiError {
    fn from(err: tg_crypto::CryptoError) -> Self {
        Self::Srp(err.to_string())
    }
}

/// Structured login error categories matching tdesktop's `intro/` error handling.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum LoginErrorKind {
    /// `PHONE_NUMBER_INVALID` or local digit check (`digits <= 1`).
    #[error("Invalid phone number. Please try again.")]
    PhoneNumberInvalid,
    /// `PHONE_NUMBER_BANNED`.
    #[error("This phone number is banned.")]
    PhoneNumberBanned,
    /// `PHONE_NUMBER_FLOOD`.
    #[error("Too many attempts for this phone number. Please try again later.")]
    PhoneNumberFlood,
    /// `PHONE_NUMBER_OCCUPIED`.
    #[error("This phone number is already registered.")]
    PhoneNumberOccupied,
    /// `PHONE_CODE_EMPTY` or `PHONE_CODE_INVALID`.
    #[error("Invalid code. Please try again.")]
    PhoneCodeInvalid,
    /// `PHONE_CODE_EXPIRED`.
    #[error("The code has expired. Please request a new code.")]
    PhoneCodeExpired,
    /// `SEND_CODE_UNAVAILABLE`.
    #[error("Resending code is currently unavailable.")]
    SendCodeUnavailable,
    /// `SESSION_PASSWORD_NEEDED`.
    #[error("Two-step verification password is required.")]
    SessionPasswordNeeded,
    /// `PASSWORD_HASH_INVALID` or `SRP_PASSWORD_CHANGED`.
    #[error("You have entered an invalid password.")]
    PasswordInvalid,
    /// `PASSWORD_EMPTY` or `AUTH_KEY_UNREGISTERED`.
    #[error("Authorization session reset; please start over.")]
    PasswordEmptyOrUnregistered,
    /// `SRP_ID_INVALID`.
    #[error("SRP session expired; retry required.")]
    SrpIdInvalid,
    /// `PASSWORD_RECOVERY_NA`.
    #[error("Password recovery is not available.")]
    PasswordRecoveryUnavailable,
    /// `PASSWORD_RECOVERY_EXPIRED`.
    #[error("Password recovery code has expired.")]
    PasswordRecoveryExpired,
    /// `CODE_INVALID` (during 2FA recovery).
    #[error("You have entered an invalid recovery code.")]
    RecoveryCodeInvalid,
    /// `FIRSTNAME_INVALID`.
    #[error("Invalid first name.")]
    FirstNameInvalid,
    /// `LASTNAME_INVALID`.
    #[error("Invalid last name.")]
    LastNameInvalid,
    /// `EMAIL_INVALID`.
    #[error("Invalid email address.")]
    EmailInvalid,
    /// `EMAIL_NOT_ALLOWED`.
    #[error("This email address is not allowed.")]
    EmailNotAllowed,
    /// `EMAIL_HASH_EXPIRED`.
    #[error("Email confirmation expired. Please try again.")]
    EmailHashExpired,
    /// `2FA_CONFIRM_WAIT_X` during account reset.
    #[error("Please wait {seconds} seconds before resetting your account.")]
    TwoFaConfirmWait {
        /// Remaining wait time in seconds.
        seconds: u32,
    },
    /// `2FA_RECENT_CONFIRM` during account reset.
    #[error("Account reset was recently cancelled.")]
    TwoFaRecentConfirm,
    /// `FLOOD_WAIT_X` or `FLOOD_PREMIUM_WAIT_X`.
    /// Message matches tdesktop `lng_flood_error` (`Telegram/Resources/langs/lang.strings:315`,
    /// used in `intro/intro_phone.cpp`, `intro/intro_code.cpp`, `intro/intro_password_check.cpp`).
    #[error("Too many tries. Please try again later.")]
    FloodWait {
        /// Wait duration in seconds (0 if server omitted a number).
        seconds: u32,
    },
    /// `PHONE_MIGRATE_X` / `USER_MIGRATE_X` / `NETWORK_MIGRATE_X`.
    #[error("Account requires migration to DC {dc_id}.")]
    MigrateDc {
        /// Target data center ID.
        dc_id: DcId,
    },
    /// Terms of Service must be accepted before `auth.signUp`.
    #[error("Terms of Service must be accepted before signing up.")]
    TermsNotAccepted,
    /// Client build does not support the server's password KDF algorithm.
    #[error("App is out of date and cannot process this cloud password.")]
    OutdatedClient,
    /// Error code 406 (`MTP::IgnoreError`), suppressed in UI.
    #[error("Ignored error ({raw}).")]
    Ignored {
        /// Raw error string.
        raw: String,
    },
    /// Any other unmapped RPC error text shown verbatim in tdesktop.
    #[error("{0}")]
    Other(String),
}

/// Maps a Telegram `RpcError` to tdesktop's structured `LoginErrorKind`.
#[must_use]
pub fn map_login_error(err: &RpcError) -> LoginErrorKind {
    if err.is_flood_error() {
        return LoginErrorKind::FloodWait {
            seconds: err.flood_wait_seconds().unwrap_or(0),
        };
    }
    if let Some(dc_id) = err.migrate_dc() {
        return LoginErrorKind::MigrateDc { dc_id };
    }
    if let Some(seconds) = err.confirm_wait_seconds() {
        return LoginErrorKind::TwoFaConfirmWait { seconds };
    }
    match err.name.as_str() {
        "PHONE_NUMBER_INVALID" => LoginErrorKind::PhoneNumberInvalid,
        "PHONE_NUMBER_BANNED" => LoginErrorKind::PhoneNumberBanned,
        "PHONE_NUMBER_FLOOD" => LoginErrorKind::PhoneNumberFlood,
        "PHONE_NUMBER_OCCUPIED" => LoginErrorKind::PhoneNumberOccupied,
        "PHONE_CODE_EMPTY" | "PHONE_CODE_INVALID" => LoginErrorKind::PhoneCodeInvalid,
        "PHONE_CODE_EXPIRED" => LoginErrorKind::PhoneCodeExpired,
        "SEND_CODE_UNAVAILABLE" => LoginErrorKind::SendCodeUnavailable,
        "SESSION_PASSWORD_NEEDED" => LoginErrorKind::SessionPasswordNeeded,
        "PASSWORD_HASH_INVALID" | "SRP_PASSWORD_CHANGED" => LoginErrorKind::PasswordInvalid,
        "PASSWORD_EMPTY" | "AUTH_KEY_UNREGISTERED" => LoginErrorKind::PasswordEmptyOrUnregistered,
        "SRP_ID_INVALID" => LoginErrorKind::SrpIdInvalid,
        "PASSWORD_RECOVERY_NA" => LoginErrorKind::PasswordRecoveryUnavailable,
        "PASSWORD_RECOVERY_EXPIRED" => LoginErrorKind::PasswordRecoveryExpired,
        "CODE_INVALID" => LoginErrorKind::RecoveryCodeInvalid,
        "FIRSTNAME_INVALID" => LoginErrorKind::FirstNameInvalid,
        "LASTNAME_INVALID" => LoginErrorKind::LastNameInvalid,
        "EMAIL_INVALID" => LoginErrorKind::EmailInvalid,
        "EMAIL_NOT_ALLOWED" => LoginErrorKind::EmailNotAllowed,
        "EMAIL_HASH_EXPIRED" => LoginErrorKind::EmailHashExpired,
        "2FA_RECENT_CONFIRM" => LoginErrorKind::TwoFaRecentConfirm,
        _ if err.is_ignored() => LoginErrorKind::Ignored {
            raw: err.name.clone(),
        },
        _ => LoginErrorKind::Other(err.name.clone()),
    }
}

/// Structured send-message failure categories matching `ApiWrap::sendMessageFail`.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SendMessageErrorKind {
    /// `MESSAGE_EMPTY` — tdesktop destroys the local message silently.
    #[error("Message is empty.")]
    MessageEmpty,
    /// `PEER_FLOOD` — spam restriction.
    #[error("Sorry, you can only send messages to mutual contacts at the moment.")]
    PeerFlood,
    /// `USER_BANNED_IN_CHANNEL`.
    #[error("You are banned from sending messages in public groups.")]
    UserBannedInChannel,
    /// `SLOWMODE_WAIT_X`.
    #[error("Slowmode enabled; wait {seconds} seconds.")]
    SlowmodeWait {
        /// Remaining slowmode cooldown in seconds.
        seconds: u32,
    },
    /// `SCHEDULE_STATUS_PRIVATE`.
    #[error("Cannot schedule message until online because recipient hides online status.")]
    ScheduleStatusPrivate,
    /// `CHAT_FORWARDS_RESTRICTED`.
    #[error("Forwarding messages is restricted in this chat.")]
    ChatForwardsRestricted,
    /// `PREMIUM_ACCOUNT_REQUIRED`.
    #[error("Telegram Premium is required for this action.")]
    PremiumAccountRequired,
    /// `SCHEDULE_TOO_MUCH`.
    #[error("Too many scheduled messages in this chat.")]
    ScheduleTooMuch,
    /// `ALLOW_PAYMENT_REQUIRED_X`.
    #[error("Payment of {stars} stars is required to message this peer.")]
    PaymentRequired {
        /// Updated stars price per message.
        stars: u32,
    },
    /// `TOPIC_CLOSED`.
    #[error("This forum topic is closed.")]
    TopicClosed,
    /// `FLOOD_WAIT_X`.
    #[error("Flood wait: {seconds} seconds.")]
    FloodWait {
        /// Wait duration in seconds.
        seconds: u32,
    },
    /// Any other RPC or transport error.
    #[error("{0}")]
    Other(String),
}

/// Maps an `InvocationError` from sending a message to `SendMessageErrorKind`.
#[must_use]
pub fn map_send_message_error(err: &InvocationError) -> SendMessageErrorKind {
    let InvocationError::Rpc(rpc) = err else {
        return SendMessageErrorKind::Other(err.to_string());
    };
    if rpc.is_flood_error() {
        return SendMessageErrorKind::FloodWait {
            seconds: rpc.flood_wait_seconds().unwrap_or(0),
        };
    }
    if let Some(seconds) = rpc.slowmode_wait_seconds() {
        return SendMessageErrorKind::SlowmodeWait { seconds };
    }
    if let Some(stars) = rpc.allow_payment_required_stars() {
        return SendMessageErrorKind::PaymentRequired { stars };
    }
    match rpc.name.as_str() {
        "MESSAGE_EMPTY" => SendMessageErrorKind::MessageEmpty,
        "PEER_FLOOD" => SendMessageErrorKind::PeerFlood,
        "USER_BANNED_IN_CHANNEL" => SendMessageErrorKind::UserBannedInChannel,
        "SCHEDULE_STATUS_PRIVATE" => SendMessageErrorKind::ScheduleStatusPrivate,
        "CHAT_FORWARDS_RESTRICTED" => SendMessageErrorKind::ChatForwardsRestricted,
        "PREMIUM_ACCOUNT_REQUIRED" => SendMessageErrorKind::PremiumAccountRequired,
        "SCHEDULE_TOO_MUCH" => SendMessageErrorKind::ScheduleTooMuch,
        "TOPIC_CLOSED" => SendMessageErrorKind::TopicClosed,
        _ => SendMessageErrorKind::Other(rpc.name.clone()),
    }
}
