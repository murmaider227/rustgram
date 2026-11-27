//! Pure-Rust port of Telegram Desktop's bot subsystem (`tg-bots`).
//!
//! Covers:
//! - [`markup`]: Reply markups, inline/reply keyboard button kinds, request-peer criteria,
//!   forwarding transformations, and reply-keyboard state derivation.
//! - [`keyboard_layout`]: Inline and reply keyboard row width distribution, natural sizing,
//!   normal/tiny style selection, corner rounding, and hit-testing.
//! - [`callback`]: Button activation dispatch, callback query answer handling (alert vs toast,
//!   URL/game confirmation rules, caching by `cache_time`), switch-inline flow, and URL auth flow.
//! - [`mosaic`]: Exact port of `layout/layout_mosaic.cpp` and `layout/layout_position.cpp`.
//! - [`inline`]: Inline bot result parsing/validation, gallery vs list layout decisions,
//!   item sizing, `@bot query` parsing, debounce timing (400 ms), pagination (`next_offset`),
//!   and `switch_pm` / `switch_webview`.
//! - [`commands`]: Bot commands per scope, chat bot-command registry, command wrapping,
//!   menu buttons, `BotInfo`, and privacy policy resolution.
//! - [`mini_app`]: Attach menu bots, Mini App launch parameters, palette-driven `theme_params`,
//!   and HTML5 games / share game score.
//! - [`protocol`]: Protocol DTOs and state machines for `messages.startBot`, `messages.getBotApp`,
//!   `messages.checkUrlAuthMatchCode`, `messages.requestChatJoinWebView`, and `bots.*` permissions/discovery.
//! - [`deep_links`]: Parsing and validation of `t.me` / `tg://` bot deep links and start parameters.
//! - [`stand_in`]: Plain stand-in domain types matching `tg-data` contracts in `docs/ARCHITECTURE.md`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod callback;
pub mod commands;
pub mod deep_links;
pub mod inline;
pub mod keyboard_layout;
pub mod markup;
pub mod mini_app;
pub mod mosaic;
pub mod protocol;
pub mod stand_in;
pub mod tl_convert;

#[cfg(test)]
mod fuzz_untrusted;
