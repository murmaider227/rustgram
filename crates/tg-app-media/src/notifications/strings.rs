//! Rustgram-specific strings for `notifications/` that have no exact key in `resources/lang.strings`.

/// Application display name used when sender identity is hidden.
pub const APP_NAME: &str = "Rustgram";

/// Short avatar fallback monogram when sender identity is hidden.
pub const APP_MONOGRAM: &str = "TG";

/// Collapsed burst summary when multiple plain messages arrive simultaneously in one chat.
#[must_use]
pub fn collapsed_new_messages(count: usize) -> String {
    if count == 1 {
        "1 new message".to_owned()
    } else {
        format!("{count} new messages")
    }
}

/// Fallback label when a pinned-message notification has no text snippet.
#[must_use]
pub fn pinned_generic_message(actor: &str) -> String {
    format!("{actor} pinned a message")
}
