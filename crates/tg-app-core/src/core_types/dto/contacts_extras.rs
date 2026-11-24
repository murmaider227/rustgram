//! DTOs for WP-66 contacts, spam-bar, contact/fee-waiver tokens, birthdays, contact notes,
//! contact/own profile photos, and promo-data hiding RPCs.
//!
//! Port origin:
//! - `Telegram/SourceFiles/info/profile/info_profile_qr_link.cpp`
//! - `Telegram/SourceFiles/window/window_session_controller_contact.cpp`
//! - `Telegram/SourceFiles/api/api_premium.cpp`
//! - `Telegram/SourceFiles/data/components/promo_suggestions.cpp`
//! - `Telegram/SourceFiles/window/window_peer_menu.cpp`
//! - `Telegram/SourceFiles/api/api_peer_search.cpp`
//! - `Telegram/SourceFiles/api/api_peer_photo.cpp`

use crate::info::backend::EmojiUserpicRequest;
use crate::info::values::Birthday;
use crate::stand_in::types::{MsgId, PeerId};

/// Exported temporary contact QR/deep-link token (`contacts.exportContactToken`,
/// `info/profile/info_profile_qr_link.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedContactTokenInfo {
    /// Shareable contact URL (e.g. `https://t.me/contact/<token>`).
    pub url: String,
    /// Unix timestamp (seconds) when this contact token expires.
    pub expires: i32,
}

/// Exported paid-message fee-waiver token (`contacts.exportNoPaidMessagesToken`,
/// `api/api_premium.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedNoPaidMessagesTokenInfo {
    /// Raw opaque token string.
    pub token: String,
    /// Shareable deep-link URL carrying the fee-waiver token.
    pub url: String,
    /// Optional Unix expiration timestamp (`None` for permanent tokens).
    pub expires: Option<i32>,
}

/// Outcome of redeeming a paid-message fee-waiver token (`contacts.importNoPaidMessagesToken`,
/// `api/api_premium.cpp:487-528`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoPaidMessagesTokenImportResult {
    /// Resolved user peer ID returned by the server.
    pub user_id: PeerId,
    /// `true` if the user previously required Stars per message and no longer does after redemption.
    pub price_removed: bool,
}

/// Single contact birthday entry returned by `contacts.getBirthdays`
/// (`data/components/promo_suggestions.cpp:284-346`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactBirthdayEntry {
    /// Contact user peer ID.
    pub peer_id: PeerId,
    /// Display name of the contact.
    pub display_name: String,
    /// Contact's birthday (day, month, optional year).
    pub birthday: Birthday,
}

/// Request payload for `contacts.blockFromReplies` (`window/window_peer_menu.cpp:2845-2854`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockFromRepliesRequest {
    /// Message ID inside the Replies chat identifying the offending reply.
    pub msg_id: MsgId,
    /// Whether to delete the single reply message (`flags.0`).
    pub delete_message: bool,
    /// Whether to delete all replies from that sender (`flags.1`).
    pub delete_history: bool,
    /// Whether to report the sender for spam (`flags.2`).
    pub report_spam: bool,
}

/// Contact online/last-seen status entry returned by `contacts.getStatuses`
/// (`apiwrap.cpp:2210-2229`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactStatusInfo {
    /// Contact user peer ID.
    pub peer_id: PeerId,
    /// Human-readable presence label (e.g. `"online"`, `"last seen recently"`).
    pub status_text: String,
    /// `true` when the contact is currently online.
    pub is_online: bool,
    /// Exact Unix timestamp until online / last seen (`0` when hidden or long ago).
    pub online_till: i32,
}

/// Sponsored peer search result returned by `contacts.getSponsoredPeers`
/// (`api/api_peer_search.cpp:100-131`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SponsoredPeerInfo {
    /// Sponsored target peer ID.
    pub peer_id: PeerId,
    /// Opaque server `random_id` bytes identifying this ad impression.
    pub random_id: Vec<u8>,
    /// Display title of the sponsored peer.
    pub title: String,
    /// Optional public `@username` without leading `@`.
    pub username: Option<String>,
    /// Optional sponsor disclosure string (`sponsor_info`).
    pub sponsor_info: Option<String>,
    /// Optional additional info string (`additional_info`).
    pub additional_info: Option<String>,
    /// Deterministic avatar palette color index (`0..7`).
    pub color_index: u8,
    /// Verified badge.
    pub verified: bool,
    /// Premium badge.
    pub premium: bool,
}

/// Operation mode for `photos.uploadContactProfilePhoto` (`api/api_peer_photo.cpp:611-631, 770-796`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContactProfilePhotoMode {
    /// Save a personal contact photo visible only to us (`flags.4 = save`),
    /// or clear the personal photo when no media/markup is supplied.
    Save,
    /// Suggest a profile photo to the contact (`flags.3 = suggest`).
    Suggest,
}

/// Upload or clear payload for `photos.uploadContactProfilePhoto`
/// (`api/api_peer_photo.cpp:611-631, 770-796`).
#[derive(Debug, Clone, PartialEq)]
pub struct ContactProfilePhotoUpload {
    /// Whether to save as a personal contact photo (`Save`) or suggest (`Suggest`).
    pub mode: ContactProfilePhotoMode,
    /// Optional static image `(file_name, bytes)` (`flags.0 = file`).
    pub image: Option<(String, Vec<u8>)>,
    /// Optional animated video avatar `(file_name, bytes)` (`flags.1 = video`).
    pub video: Option<(String, Vec<u8>)>,
    /// Optional video frame timestamp in seconds (`flags.2 = video_start_ts`, sent only when `video` is set).
    pub video_start_ts: Option<f64>,
    /// Optional emoji + gradient avatar markup (`flags.5 = video_emoji_markup`).
    pub emoji_markup: Option<EmojiUserpicRequest>,
}

impl ContactProfilePhotoUpload {
    /// Creates a request that clears the personal contact photo (`PeerPhoto::clearPersonal`,
    /// `api/api_peer_photo.cpp:611-631`: `save = true`, no `file`/`video`/`video_emoji_markup`).
    #[must_use]
    pub const fn clear_personal() -> Self {
        Self {
            mode: ContactProfilePhotoMode::Save,
            image: None,
            video: None,
            video_start_ts: None,
            emoji_markup: None,
        }
    }

    /// Creates a static contact photo upload (either personal `Save` or `Suggest`).
    #[must_use]
    pub fn static_photo(
        mode: ContactProfilePhotoMode,
        file_name: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Self {
        Self {
            mode,
            image: Some((file_name.into(), bytes)),
            video: None,
            video_start_ts: None,
            emoji_markup: None,
        }
    }

    /// Creates an animated video contact photo upload (either personal `Save` or `Suggest`).
    #[must_use]
    pub fn video_photo(
        mode: ContactProfilePhotoMode,
        file_name: impl Into<String>,
        bytes: Vec<u8>,
        video_start_ts: Option<f64>,
    ) -> Self {
        Self {
            mode,
            image: None,
            video: Some((file_name.into(), bytes)),
            video_start_ts,
            emoji_markup: None,
        }
    }

    /// Creates an emoji-markup contact photo upload (`videoSizeEmojiMarkup`).
    #[must_use]
    pub const fn emoji_markup(mode: ContactProfilePhotoMode, markup: EmojiUserpicRequest) -> Self {
        Self {
            mode,
            image: None,
            video: None,
            video_start_ts: None,
            emoji_markup: Some(markup),
        }
    }
}
