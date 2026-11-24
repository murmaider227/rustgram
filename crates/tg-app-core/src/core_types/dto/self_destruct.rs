//! Pure self-destruct / view-once DTOs, backend trait, and mock store.

use crate::stand_in::types::{MsgId, PeerId};

/// Telegram sentinel `ttl_seconds` value for View-Once media (`kTimeToLiveSingleView = 0x7FFFFFFF`).
pub const VIEW_ONCE_TTL_SECONDS: i32 = i32::MAX;

/// Exact selectable Account Self-Destruct TTL values in days (`Values(Type::Account)`).
pub const ACCOUNT_TTL_DAYS_OPTIONS: &[i32] = &[30, 90, 180, 365, 548, 720];

/// Exact selectable Inactive Sessions TTL values in days (`Values(Type::Sessions)`).
pub const SESSIONS_TTL_DAYS_OPTIONS: &[i32] = &[7, 30, 90, 180, 365];

/// Standard Default Auto-Delete History TTL values in seconds (`messages.setDefaultHistoryTTL`).
pub const DEFAULT_HISTORY_TTL_SECS_OPTIONS: &[i32] = &[
    0, 86_400,    // 1 day
    604_800,   // 1 week
    2_678_400, // 1 month (31 days)
];

/// Media self-destruct timer presets offered in `AttachFilesBox` / `SendFilesBox`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MediaTtlPreset {
    /// Normal persistent media (`ttl_seconds = None`).
    #[default]
    Off,
    /// View-once self-destructing media (`ttl_seconds = Some(0x7FFFFFFF)`).
    ViewOnce,
    /// Timed self-destruct after opening (`5`, `10`, `30`, or `60` seconds).
    Seconds(i32),
}

impl MediaTtlPreset {
    /// All presets shown in the media self-destruct picker.
    pub const ALL: &[Self] = &[
        Self::Off,
        Self::ViewOnce,
        Self::Seconds(5),
        Self::Seconds(10),
        Self::Seconds(30),
        Self::Seconds(60),
    ];

    /// Converts this preset into the Telegram `ttl_seconds` wire field.
    #[must_use]
    pub const fn to_ttl_seconds(self) -> Option<i32> {
        match self {
            Self::Off => None,
            Self::ViewOnce => Some(VIEW_ONCE_TTL_SECONDS),
            Self::Seconds(s) if s > 0 => Some(s),
            Self::Seconds(_) => None,
        }
    }

    /// Reconstructs a [`MediaTtlPreset`] from a `ttl_seconds` wire value.
    #[must_use]
    pub const fn from_ttl_seconds(ttl: Option<i32>) -> Self {
        match ttl {
            None => Self::Off,
            Some(v) if v <= 0 => Self::Off,
            Some(VIEW_ONCE_TTL_SECONDS) => Self::ViewOnce,
            Some(v) => Self::Seconds(v),
        }
    }

    /// Short human-readable label for buttons and badges.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Off => "Off".to_owned(),
            Self::ViewOnce => "View Once".to_owned(),
            Self::Seconds(s) => format!("{s}s"),
        }
    }
}

/// Returns `true` when `ttl_seconds` denotes a view-once media item (`0x7FFFFFFF`).
#[inline]
#[must_use]
pub const fn is_view_once_ttl(ttl_seconds: Option<i32>) -> bool {
    matches!(ttl_seconds, Some(VIEW_ONCE_TTL_SECONDS))
}

/// Formats a compact badge string for a self-destructing media bubble.
#[must_use]
pub fn format_media_ttl_badge(ttl_seconds: i32, viewed: bool) -> String {
    if viewed {
        "Expired".to_owned()
    } else if ttl_seconds == VIEW_ONCE_TTL_SECONDS {
        "1 · View Once".to_owned()
    } else if ttl_seconds > 0 {
        format!("🔥 {ttl_seconds}s")
    } else {
        String::new()
    }
}

// ---------------------------------------------------------------------------
// Backend contract (`api/api_self_destruct.cpp`)
// ---------------------------------------------------------------------------

/// Cached snapshot of account, session, and default-history self-destruct TTLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelfDestructSnapshot {
    /// Account inactivity self-destruct TTL in days (`account.getAccountTTL`).
    pub account_ttl_days: i32,
    /// Inactive authorizations termination TTL in days (`account.setAuthorizationTTL`).
    pub sessions_ttl_days: i32,
    /// Global default auto-delete timer for new chats in seconds (`messages.getDefaultHistoryTTL`).
    pub default_history_ttl_secs: i32,
}

impl Default for SelfDestructSnapshot {
    fn default() -> Self {
        Self {
            account_ttl_days: 180,
            sessions_ttl_days: 180,
            default_history_ttl_secs: 0,
        }
    }
}

/// Backend contract porting `Api::SelfDestruct` (`api_self_destruct.cpp`) plus view-once
/// media read acknowledgement (`messages.readMessageContents`).
pub trait SelfDestructBackend {
    /// Reloads account TTL (`account.getAccountTTL`) and default history TTL
    /// (`messages.getDefaultHistoryTTL`).
    fn reload_self_destruct(&mut self) -> Result<SelfDestructSnapshot, String>;

    /// Updates account inactivity TTL (`account.setAccountTTL`).
    fn update_account_ttl(&mut self, days: i32) -> Result<(), String>;

    /// Updates inactive sessions TTL (`account.setAuthorizationTTL`).
    fn update_sessions_ttl(&mut self, days: i32) -> Result<(), String>;

    /// Updates default history auto-delete period for new chats (`messages.setDefaultHistoryTTL`).
    fn update_default_history_ttl(&mut self, period_secs: i32) -> Result<(), String>;

    /// Acknowledges opening a self-destructing / view-once media item (`messages.readMessageContents`).
    fn mark_ttl_media_viewed(&mut self, peer_id: PeerId, msg_id: MsgId) -> Result<(), String>;
}

// ---------------------------------------------------------------------------
// Offline Mock Store (`MockSession` demo data)
// ---------------------------------------------------------------------------

/// Offline demo store for [`SelfDestructBackend`].
#[derive(Debug, Clone, Default)]
pub struct MockSelfDestructStore {
    /// Current TTL snapshot.
    pub snapshot: SelfDestructSnapshot,
    /// Messages whose view-once / TTL media have been viewed & expired.
    pub viewed_messages: Vec<(PeerId, MsgId)>,
}

impl MockSelfDestructStore {
    /// Creates a demo store initialized with standard Telegram defaults (6 months account TTL).
    #[must_use]
    pub fn new() -> Self {
        Self {
            snapshot: SelfDestructSnapshot::default(),
            viewed_messages: Vec::new(),
        }
    }

    /// Returns `true` if `(peer_id, msg_id)` has already been viewed & burned.
    #[must_use]
    pub fn is_viewed(&self, peer_id: PeerId, msg_id: MsgId) -> bool {
        self.viewed_messages.contains(&(peer_id, msg_id))
    }
}

impl SelfDestructBackend for MockSelfDestructStore {
    fn reload_self_destruct(&mut self) -> Result<SelfDestructSnapshot, String> {
        Ok(self.snapshot)
    }

    fn update_account_ttl(&mut self, days: i32) -> Result<(), String> {
        if days <= 0 {
            return Err("TTL days must be positive.".to_owned());
        }
        self.snapshot.account_ttl_days = days;
        Ok(())
    }

    fn update_sessions_ttl(&mut self, days: i32) -> Result<(), String> {
        if days <= 0 {
            return Err("Session TTL days must be positive.".to_owned());
        }
        self.snapshot.sessions_ttl_days = days;
        Ok(())
    }

    fn update_default_history_ttl(&mut self, period_secs: i32) -> Result<(), String> {
        if period_secs < 0 {
            return Err("History TTL period cannot be negative.".to_owned());
        }
        self.snapshot.default_history_ttl_secs = period_secs;
        Ok(())
    }

    fn mark_ttl_media_viewed(&mut self, peer_id: PeerId, msg_id: MsgId) -> Result<(), String> {
        if !self.viewed_messages.contains(&(peer_id, msg_id)) {
            self.viewed_messages.push((peer_id, msg_id));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 1. SelfDestructionBox Modal (`boxes/self_destruction_box.cpp`)
// ---------------------------------------------------------------------------
