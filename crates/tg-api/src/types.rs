//! High-level configuration and option types for `tg-api`.
//!
//! Port origin: `api/api_common.h`, `data/data_search_controller.h`, `intro/intro_phone.cpp`.
//!
//! All Telegram wire requests and responses use the generated `tg_tl` types directly.

use tg_data::TimeId;
use tg_tl::{enums as tl_enums, types as tl_types};

/// Application API credentials injected from configuration.
///
/// Never embed hardcoded `api_id` / `api_hash` values in library code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiCredentials {
    /// Telegram application `api_id`.
    pub api_id: i32,
    /// Telegram application `api_hash`.
    pub api_hash: String,
}

/// Slice load direction (`Data::LoadDirection`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LoadDirection {
    /// Load older messages before `message_id`.
    Before,
    /// Load messages centered around `message_id`.
    Around,
    /// Load newer messages after `message_id`.
    After,
}

/// Forward display mode (`Data::ForwardOptions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ForwardOptions {
    /// Keep sender names and media captions.
    #[default]
    PreserveInfo,
    /// Hide sender names (`drop_author = true`).
    NoSenderNames,
    /// Hide sender names and media captions (`drop_author = true, drop_media_captions = true`).
    NoNamesAndCaptions,
}

/// Send options shared across send, edit, and forward (`Api::SendOptions`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SendOptions {
    /// Send without notification sound (`silent`).
    pub silent: bool,
    /// Suppress webpage preview (`no_webpage`).
    pub no_webpage: bool,
    /// Clear cloud draft on send (`clear_draft`).
    pub clear_draft: bool,
    /// Protect sent content from forwarding and saving (`noforwards`).
    ///
    /// Matches `Api::SendOptions::noforwards` (`api/api_common.h`, `apiwrap.cpp:4920`).
    pub noforwards: bool,
    /// Invert media above text (`invert_media`).
    pub invert_media: bool,
    /// Optional schedule unix timestamp (`schedule_date`).
    pub schedule_date: Option<TimeId>,
    /// Optional recurring schedule period (`schedule_repeat_period`).
    pub schedule_repeat_period: Option<i32>,
    /// Optional send-as peer (`send_as`).
    pub send_as: Option<tl_enums::InputPeer>,
    /// Optional message effect ID (`effect`).
    pub effect_id: Option<i64>,
    /// Optional approved paid stars (`allow_paid_stars`).
    pub allow_paid_stars: Option<i64>,
}

/// Returns the exact default `tg_tl::types::CodeSettings` sent by Telegram Desktop
/// (`intro_phone.cpp:210`: `MTP_flags(0)` — `allow_flashcall = false`, `allow_firebase = false`).
#[must_use]
pub const fn code_settings_desktop_default() -> tl_types::CodeSettings {
    tl_types::CodeSettings {
        allow_flashcall: false,
        current_number: false,
        allow_app_hash: false,
        allow_missed_call: false,
        allow_firebase: false,
        unknown_number: false,
        logout_tokens: None,
        token: None,
        app_sandbox: None,
    }
}
