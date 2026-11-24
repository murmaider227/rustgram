//! Rustgram-specific strings for `tray/` that have no exact key in `resources/lang.strings`.

/// Default application display name shown in tray tooltips and taskbar metadata.
pub const APP_NAME: &str = "Rustgram";

/// Tray menu action to temporarily mute all notifications for one hour.
pub const MUTE_FOR_ONE_HOUR: &str = "Mute for 1 hour";

/// Tray menu label while a 1-hour global mute is active.
#[must_use]
pub fn unmute_minutes_left(mins: i64) -> String {
    format!("Unmute notifications ({mins}m left)")
}

/// Debug tray menu action to simulate an incoming message notification.
pub const SIMULATE_INCOMING_MESSAGE: &str = "Simulate incoming message";
