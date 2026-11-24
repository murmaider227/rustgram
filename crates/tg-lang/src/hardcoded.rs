//! Hardcoded fallback English phrases not stored in `lang.strings`.
//!
//! Port of `Telegram/SourceFiles/lang/lang_hardcoded.h`.

/// Title of the favorite stickers set.
#[inline]
pub const fn faved_set_title() -> &'static str {
    "Favorite stickers"
}

/// Error shown when the remote peer uses an incompatible call protocol.
#[inline]
pub const fn call_error_incompatible() -> &'static str {
    "{user}'s app is using an incompatible protocol. They need to update their app before you can call them."
}

/// Generic internal server error message.
#[inline]
pub const fn server_error() -> &'static str {
    "Internal server error."
}

/// Error shown when clearing a cache path fails.
#[inline]
pub const fn clear_path_failed() -> &'static str {
    "Clear failed :("
}

/// Prompt shown when the configured proxy settings are invalid.
#[inline]
pub const fn proxy_config_error() -> &'static str {
    "The proxy you are using is not configured correctly. Do you want to disable it and use your system connection settings?"
}

/// Error shown when the Telegram Passport authorization bot cannot be resolved.
#[inline]
pub const fn no_authorization_bot() -> &'static str {
    "Could not get authorization bot."
}

/// Error shown when saving a Telegram Passport value fails.
#[inline]
pub const fn secure_save_error() -> &'static str {
    "Error saving value."
}

/// Error shown when accepting a Telegram Passport form fails.
#[inline]
pub const fn secure_accept_error() -> &'static str {
    "Error accepting form."
}

/// Error shown when Telegram Passport data is corrupted during authorization.
#[inline]
pub const fn passport_corrupted() -> &'static str {
    "It seems your Telegram Passport data was corrupted.\n\nYou can reset your Telegram Passport and restart this authorization."
}

/// Error shown when Telegram Passport data is corrupted while changing cloud password.
#[inline]
pub const fn passport_corrupted_change() -> &'static str {
    "It seems your Telegram Passport data was corrupted.\n\nYou can reset your Telegram Passport and change your cloud password."
}

/// Reset action button label for corrupted Telegram Passport data.
#[inline]
pub const fn passport_corrupted_reset() -> &'static str {
    "Reset"
}

/// Confirmation prompt before resetting corrupted Telegram Passport data.
#[inline]
pub const fn passport_corrupted_reset_sure() -> &'static str {
    "Are you sure you want to reset your Telegram Passport data?"
}

/// Error shown when reading a Telegram Passport scan fails with an unknown error.
#[inline]
pub const fn unknown_secure_scan_error() -> &'static str {
    "Unknown scan read error."
}

/// Error shown when a 2FA email confirmation link has expired.
#[inline]
pub const fn email_confirmation_expired() -> &'static str {
    "This email confirmation has expired. Please setup two-step verification once again."
}

/// Error shown when enabling OS autostart fails.
#[inline]
pub const fn autostart_enable_error() -> &'static str {
    "Could not register for autostart."
}
