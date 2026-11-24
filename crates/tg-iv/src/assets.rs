//! Embedded GPL-3.0 Instant View CSS and JavaScript assets.
//!
//! Copies `Telegram/Resources/export_rich/css/rich_message.css` and
//! `Telegram/Resources/export_rich/js/rich_message.js` from Telegram Desktop
//! alongside Rustgram's WebView bridge stylesheet and script.

/// Full Instant View stylesheet (light/dark theme variables + all IV block rules).
pub const PAGE_CSS: &str = include_str!("../assets/page.css");

/// Instant View WebView bridge and interactive slideshow script.
pub const PAGE_JS: &str = include_str!("../assets/page.js");

/// Verbatim GPL-3.0 stylesheet copied from tdesktop's `Resources/export_rich/css/rich_message.css`.
pub const RICH_MESSAGE_CSS: &str = include_str!("../assets/rich_message.css");

/// Verbatim GPL-3.0 slideshow script copied from tdesktop's `Resources/export_rich/js/rich_message.js`.
pub const RICH_MESSAGE_JS: &str = include_str!("../assets/rich_message.js");
