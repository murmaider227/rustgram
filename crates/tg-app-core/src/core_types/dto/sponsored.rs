//! Pure sponsored-message DTOs and per-peer state cache.

use std::collections::HashMap;

use tg_text::TextWithEntities;

use crate::stand_in::types::{PeerId, PeerKind};

/// Minimum interval between `messages.getSponsoredMessages` / `viewSponsoredMessage` calls
/// (`kRequestTimeLimit = 5 * 60 * 1000` in `data/components/sponsored_messages.cpp:33`).
pub const SPONSORED_REQUEST_TIME_LIMIT_MS: u64 = 5 * 60 * 1_000;

/// Official Telegram Ads platform URL (`kUrl` in `boxes/about_sponsored_box.cpp:22`).
pub const ADS_PLATFORM_URL: &str = "https://ads.telegram.org";

/// Preferred height of the inline channel sponsored message banner.
pub const SPONSORED_BANNER_HEIGHT: f32 = 88.0;

/// Single sponsored message entry (`Data::SponsoredMessage` in `sponsored_messages.h`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SponsoredMessageItem {
    /// Opaque server random identifier passed to view/click/report RPCs.
    pub random_id: Vec<u8>,
    /// Target link opened when clicking the CTA button.
    pub url: String,
    /// Advertiser / channel title shown in the header.
    pub title: String,
    /// Sponsored body text with formatting entities.
    pub text: TextWithEntities,
    /// Call-to-action button label (e.g. `"View Channel"`, `"Open Link"`).
    pub button_text: String,
    /// Optional sponsor legal/attribution notice (`sponsor_info`).
    pub sponsor_info: Option<String>,
    /// Optional additional regulatory info (`additional_info`).
    pub additional_info: Option<String>,
    /// True if labeled `"Recommended"` instead of `"Sponsored"`.
    pub recommended: bool,
    /// True if the ad can be reported via `messages.reportSponsoredMessage`.
    pub can_report: bool,
    /// Optional accent color index (`0..=6`).
    pub color_index: u8,
    /// Optional minimum display duration in milliseconds (for video ads).
    pub min_display_duration_ms: u64,
    /// Optional maximum display duration in milliseconds (for video ads).
    pub max_display_duration_ms: u64,
    /// Whether `url` is an internal Telegram link (`t.me` / `tg://`).
    pub is_link_internal: bool,
    /// Whether `messages.viewSponsoredMessage` has already been dispatched for this item.
    pub viewed: bool,
}

impl SponsoredMessageItem {
    /// Returns the badge text (`"Recommended"` or `"Sponsored"`).
    #[must_use]
    pub const fn badge_label(&self) -> &'static str {
        if self.recommended {
            "Recommended"
        } else {
            "Sponsored"
        }
    }
}

/// How sponsored messages should be placed in a chat (`SponsoredMessages::State`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SponsoredPlacementMode {
    /// No sponsored messages active.
    #[default]
    None,
    /// Append single sponsored message at the bottom of a broadcast channel.
    AppendToEnd,
    /// Inject sponsored messages every `posts_between` channel posts.
    InjectToMiddle,
    /// Pin sponsored bar at the top of a bot chat (`SponsoredMessages::isTopBarFor`).
    AppendToTopBar,
}

/// Per-peer cached sponsored messages list (`SponsoredMessages::List`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SponsoredPeerList {
    /// Optional post spacing for mid-history injection (`posts_between`).
    pub posts_between: Option<i32>,
    /// Optional video start delay in milliseconds.
    pub start_delay_ms: u64,
    /// Optional delay between video ads in milliseconds.
    pub between_delay_ms: u64,
    /// Computed placement mode.
    pub mode: SponsoredPlacementMode,
    /// Received sponsored messages.
    pub entries: Vec<SponsoredMessageItem>,
    /// Monotonic timestamp (ms) when this list was received.
    pub received_ms: u64,
    /// Number of entries already injected.
    pub injected_count: usize,
    /// True once all entries have been shown.
    pub showed_all: bool,
}

/// Single selectable option returned by `channels.sponsoredMessageReportResultChooseOption`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SponsoredReportOption {
    /// Opaque server option bytes echoed in the follow-up `reportSponsoredMessage` call.
    pub option: Vec<u8>,
    /// Human-readable reason label.
    pub text: String,
}

/// Decoded result of `messages.reportSponsoredMessage` (`SponsoredReportResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SponsoredReportOutcome {
    /// Server asks the user to choose a more specific sub-option.
    ChooseOption {
        /// Prompt title for the next option list.
        title: String,
        /// Available options.
        options: Vec<SponsoredReportOption>,
    },
    /// All ads were hidden (e.g. Premium user selected "Hide all ads").
    AdsHidden,
    /// Ad was reported and removed locally.
    Reported,
    /// User selected "I don't like it" (`option == b"1"`) and the ad was silenced/removed.
    Silence,
    /// Action requires Telegram Premium (`PREMIUM_ACCOUNT_REQUIRED`).
    PremiumRequired,
    /// Server or transport error message.
    Error(String),
}

/// Pure-logic manager for channel/bot sponsored messages, request/view throttling, and
/// Premium "hide ads" suppression (`Data::SponsoredMessages`).
#[derive(Debug, Clone, Default)]
pub struct SponsoredMessagesState {
    lists: HashMap<PeerId, SponsoredPeerList>,
    last_request_ms: HashMap<PeerId, u64>,
    last_view_ms: HashMap<Vec<u8>, u64>,
    /// True if the active account holds Telegram Premium.
    pub is_premium: bool,
    /// True if the user enabled the Premium "Hide Sponsored Messages" setting.
    pub hide_ads: bool,
}

impl SponsoredMessagesState {
    /// Creates a fresh manager with Premium hide-ads disabled by default.
    #[must_use]
    pub fn new(is_premium: bool, hide_ads: bool) -> Self {
        Self {
            lists: HashMap::new(),
            last_request_ms: HashMap::new(),
            last_view_ms: HashMap::new(),
            is_premium,
            hide_ads,
        }
    }

    /// Returns `true` when sponsored messages must be hidden/suppressed due to the Premium
    /// "Hide Ads" setting (`AmPremiumValue` + `account.toggleSponsoredMessages`).
    #[inline]
    #[must_use]
    pub const fn ads_suppressed(&self) -> bool {
        self.is_premium && self.hide_ads
    }

    /// Updates Premium tier and hide-ads setting; clears cached sponsored entries immediately
    /// when ads become suppressed (`SponsoredMessages::clear`).
    pub fn set_premium_hide_ads(&mut self, is_premium: bool, hide_ads: bool) {
        self.is_premium = is_premium;
        self.hide_ads = hide_ads;
        if self.ads_suppressed() {
            self.clear();
        }
    }

    /// Clears all cached sponsored messages and request timers.
    pub fn clear(&mut self) {
        self.lists.clear();
        self.last_request_ms.clear();
        self.last_view_ms.clear();
    }

    /// Returns `true` if `peer_kind` / `is_bot` is eligible for sponsored messages
    /// (`SponsoredMessages::canHaveFor`).
    #[must_use]
    pub const fn can_have_for(peer_kind: PeerKind, is_bot: bool) -> bool {
        matches!(peer_kind, PeerKind::Channel) || (matches!(peer_kind, PeerKind::User) && is_bot)
    }

    /// Returns `true` if `received_ms` is still within the 5-minute throttle window
    /// (`TooEarlyForRequest`).
    #[must_use]
    pub const fn too_early(received_ms: u64, now_ms: u64) -> bool {
        received_ms > 0 && received_ms.saturating_add(SPONSORED_REQUEST_TIME_LIMIT_MS) > now_ms
    }

    /// Returns `true` when `messages.getSponsoredMessages` should be invoked for `peer_id`.
    #[must_use]
    pub fn should_request(&self, peer_id: PeerId, now_ms: u64) -> bool {
        if self.ads_suppressed() {
            return false;
        }
        let last = self.last_request_ms.get(&peer_id).copied().unwrap_or(0);
        !Self::too_early(last, now_ms)
    }

    /// Stores a freshly fetched sponsored messages list for `peer_id`.
    pub fn store_list(&mut self, peer_id: PeerId, mut list: SponsoredPeerList, now_ms: u64) {
        self.last_request_ms.insert(peer_id, now_ms);
        if self.ads_suppressed() {
            self.lists.remove(&peer_id);
            return;
        }
        list.received_ms = now_ms;
        self.lists.insert(peer_id, list);
    }

    /// Returns the first active sponsored message for `peer_id`, respecting Premium hide-ads.
    #[must_use]
    pub fn active_for_peer(&self, peer_id: PeerId) -> Option<&SponsoredMessageItem> {
        if self.ads_suppressed() {
            return None;
        }
        self.lists.get(&peer_id)?.entries.first()
    }

    /// Returns all cached sponsored messages for `peer_id` (empty when suppressed).
    #[must_use]
    pub fn list_for_peer(&self, peer_id: PeerId) -> &[SponsoredMessageItem] {
        if self.ads_suppressed() {
            return &[];
        }
        self.lists
            .get(&peer_id)
            .map_or(&[], |l| l.entries.as_slice())
    }

    /// Returns `true` if `messages.viewSponsoredMessage` should be sent for `random_id` now,
    /// recording the view timestamp and marking matching cached items viewed.
    pub fn record_view(&mut self, random_id: &[u8], now_ms: u64) -> bool {
        if self.ads_suppressed() || random_id.is_empty() {
            return false;
        }
        let last = self.last_view_ms.get(random_id).copied().unwrap_or(0);
        if Self::too_early(last, now_ms) {
            return false;
        }
        self.last_view_ms.insert(random_id.to_vec(), now_ms);
        for list in self.lists.values_mut() {
            for entry in &mut list.entries {
                if entry.random_id == random_id {
                    entry.viewed = true;
                }
            }
        }
        true
    }

    /// Removes a sponsored entry by `random_id` (e.g. after report or local dismissal).
    pub fn remove_by_random_id(&mut self, random_id: &[u8]) -> bool {
        let mut removed = false;
        for list in self.lists.values_mut() {
            let before = list.entries.len();
            list.entries.retain(|e| e.random_id != random_id);
            if list.entries.len() != before {
                removed = true;
            }
        }
        removed
    }

    /// Computes message indices after which sponsored entries should be injected when
    /// `posts_between` is set (`SponsoredMessages::inject`).
    #[must_use]
    pub fn compute_injection_indices(
        message_count: usize,
        posts_between: Option<i32>,
        entry_count: usize,
    ) -> Vec<usize> {
        let Some(step) = posts_between.filter(|&v| v > 0) else {
            return Vec::new();
        };
        if message_count == 0 || entry_count == 0 {
            return Vec::new();
        }
        let step = step as usize;
        let mut slots = Vec::new();
        let mut pos = step.saturating_sub(1);
        while slots.len() < entry_count && pos < message_count {
            slots.push(pos);
            pos = pos.saturating_add(step);
        }
        if slots.is_empty() {
            slots.push(message_count - 1);
        }
        slots
    }
}

// ---------------------------------------------------------------------------
// Channel sponsored message banner (`ui/chat/sponsored_message_bar.cpp`)
// ---------------------------------------------------------------------------
