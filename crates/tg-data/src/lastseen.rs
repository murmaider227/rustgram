//! Port of `data/data_lastseen_status.h`, `LastseenFromMTP` from `data/data_user.cpp`,
//! and `OnlineText` / `OnlineTextFull` / `OnlineChangeTimeout` / `SortByOnlineValue`
//! from `data/data_peer_values.cpp`.

use crate::id::TimeId;

/// Epoch offset used by `LastseenStatus` (`Data::kLifeStartDate` = 2013-08-01 00:00:00 UTC).
pub const LIFE_START_DATE: TimeId = 1_375_315_200;

/// Pseudo-online duration in seconds after observed user activity (`kSetOnlineAfterActivity`).
pub const SET_ONLINE_AFTER_ACTIVITY: TimeId = 30;

/// Minimum refresh timeout for online status text in milliseconds (`kMinOnlineChangeTimeout`).
pub const MIN_ONLINE_CHANGE_TIMEOUT_MS: i64 = 1_000;

/// Maximum refresh timeout for online status text in milliseconds (`kMaxOnlineChangeTimeout`).
pub const MAX_ONLINE_CHANGE_TIMEOUT_MS: i64 = 86_400 * 1_000;

const SECONDS_IN_DAY: i32 = 86_400;

const LONG_AGO_VALUE: u32 = 0;
const RECENTLY_VALUE: u32 = 1;
const WITHIN_WEEK_VALUE: u32 = 2;
const WITHIN_MONTH_VALUE: u32 = 3;
const SPECIAL_VALUE_SKIP: u32 = 4;

/// TL-independent representation of Telegram's `UserStatus` union for converting
/// into `LastseenStatus`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum UserStatusInput {
    /// `userStatusEmpty`.
    Empty,
    /// `userStatusRecently`.
    Recently {
        /// Whether hidden because our own account hides last seen (`by_me`).
        by_me: bool,
    },
    /// `userStatusLastWeek`.
    LastWeek {
        /// Whether hidden because our own account hides last seen (`by_me`).
        by_me: bool,
    },
    /// `userStatusLastMonth`.
    LastMonth {
        /// Whether hidden because our own account hides last seen (`by_me`).
        by_me: bool,
    },
    /// `userStatusOnline`.
    Online {
        /// Unix timestamp until which the user is considered online.
        expires: TimeId,
    },
    /// `userStatusOffline`.
    Offline {
        /// Unix timestamp when the user was last online.
        was_online: TimeId,
    },
}

/// Compact 32-bit encoded user last-seen / online status (`Data::LastseenStatus`).
///
/// Exact bit layout:
/// - Bits `0..30`: value (`0` = long ago, `1` = recently, `2` = within week,
///   `3` = within month, `>= 4` = `online_till - LIFE_START_DATE`)
/// - Bit `30`: `available` (exact timestamp available vs hidden/pseudo-online)
/// - Bit `31`: `hidden_by_me`
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct LastseenStatus {
    value: u32,
    available: bool,
    hidden_by_me: bool,
}

impl LastseenStatus {
    /// "Last seen recently" status (`LastseenStatus::Recently`).
    #[inline]
    pub const fn recently(by_me: bool) -> Self {
        Self::new(RECENTLY_VALUE, false, by_me)
    }

    /// "Last seen within a week" status (`LastseenStatus::WithinWeek`).
    #[inline]
    pub const fn within_week(by_me: bool) -> Self {
        Self::new(WITHIN_WEEK_VALUE, false, by_me)
    }

    /// "Last seen within a month" status (`LastseenStatus::WithinMonth`).
    #[inline]
    pub const fn within_month(by_me: bool) -> Self {
        Self::new(WITHIN_MONTH_VALUE, false, by_me)
    }

    /// "Last seen a long time ago" status (`LastseenStatus::LongAgo`).
    #[inline]
    pub const fn long_ago(by_me: bool) -> Self {
        Self::new(LONG_AGO_VALUE, false, by_me)
    }

    /// Exact or pseudo-online timestamp (`LastseenStatus::OnlineTill`).
    #[inline]
    pub const fn online_till_status(till: TimeId, local: bool, hidden_by_me: bool) -> Self {
        if till >= LIFE_START_DATE + SPECIAL_VALUE_SKIP as i32 {
            Self::new((till - LIFE_START_DATE) as u32, !local, hidden_by_me)
        } else {
            Self::long_ago(hidden_by_me)
        }
    }

    /// Converts an API `UserStatusInput` into `LastseenStatus`, preserving local
    /// pseudo-online state on `Recently` (`LastseenFromMTP` in `data_user.cpp`).
    #[inline]
    pub const fn from_api(status: UserStatusInput, current: Self) -> Self {
        match status {
            UserStatusInput::Empty => Self::long_ago(false),
            UserStatusInput::Recently { by_me } => {
                if current.is_local_online_value() {
                    Self::online_till_status(current.online_till(), true, by_me)
                } else {
                    Self::recently(by_me)
                }
            }
            UserStatusInput::LastWeek { by_me } => Self::within_week(by_me),
            UserStatusInput::LastMonth { by_me } => Self::within_month(by_me),
            UserStatusInput::Online { expires } => Self::online_till_status(expires, false, false),
            UserStatusInput::Offline { was_online } => {
                Self::online_till_status(was_online, false, false)
            }
        }
    }

    #[inline]
    const fn new(value: u32, available: bool, hidden_by_me: bool) -> Self {
        Self {
            value: value & 0x3FFF_FFFF,
            available,
            hidden_by_me,
        }
    }

    /// Returns `true` if exact last-seen time is hidden (`isHidden`).
    #[inline]
    pub const fn is_hidden(self) -> bool {
        !self.available
    }

    /// Returns `true` if this is the "recently" bucket.
    #[inline]
    pub const fn is_recently(self) -> bool {
        !self.available && self.value == RECENTLY_VALUE
    }

    /// Returns `true` if this is the "within a week" bucket.
    #[inline]
    pub const fn is_within_week(self) -> bool {
        !self.available && self.value == WITHIN_WEEK_VALUE
    }

    /// Returns `true` if this is the "within a month" bucket.
    #[inline]
    pub const fn is_within_month(self) -> bool {
        !self.available && self.value == WITHIN_MONTH_VALUE
    }

    /// Returns `true` if this is the "long ago" / empty bucket.
    #[inline]
    pub const fn is_long_ago(self) -> bool {
        !self.available && self.value == LONG_AGO_VALUE
    }

    /// Returns `true` if last-seen is hidden because our own user hides theirs.
    #[inline]
    pub const fn is_hidden_by_me(self) -> bool {
        self.hidden_by_me
    }

    /// Returns `true` if the user is currently online at timestamp `now`.
    #[inline]
    pub const fn is_online(self, now: TimeId) -> bool {
        self.value >= SPECIAL_VALUE_SKIP
            && (LIFE_START_DATE as i64 + self.value as i64) > now as i64
    }

    /// Returns `true` if this holds a local pseudo-online value (hidden exact timestamp).
    #[inline]
    pub const fn is_local_online_value(self) -> bool {
        !self.available && self.value >= SPECIAL_VALUE_SKIP
    }

    /// Returns the exact or pseudo-online `online_till` timestamp, or `0` if none.
    #[inline]
    pub const fn online_till(self) -> TimeId {
        if self.value >= SPECIAL_VALUE_SKIP {
            LIFE_START_DATE.wrapping_add(self.value as i32)
        } else {
            0
        }
    }

    /// Packs this status into tdesktop's 32-bit storage format (`LastseenStatus::serialize`).
    #[inline]
    pub const fn serialize(self) -> u32 {
        (self.value & 0x3FFF_FFFF)
            | ((self.available as u32) << 30)
            | ((self.hidden_by_me as u32) << 31)
    }

    /// Unpacks from tdesktop's 32-bit storage format (`LastseenStatus::FromSerialized`).
    #[inline]
    pub const fn from_serialized(raw: u32) -> Self {
        let candidate = Self {
            value: raw & 0x3FFF_FFFF,
            available: ((raw >> 30) & 1) != 0,
            hidden_by_me: ((raw >> 31) & 1) != 0,
        };
        if candidate.is_valid() {
            candidate
        } else {
            Self::long_ago(false)
        }
    }

    /// Converts from legacy signed 32-bit last-seen storage (`LastseenStatus::FromLegacy`).
    #[inline]
    pub const fn from_legacy(raw: i32) -> Self {
        if raw == -2 {
            Self::recently(false)
        } else if raw == -3 {
            Self::within_week(false)
        } else if raw == -4 {
            Self::within_month(false)
        } else if raw < -30 {
            Self::online_till_status(raw.saturating_neg(), true, false)
        } else if raw > 0 {
            Self::online_till_status(raw, false, false)
        } else {
            Self::long_ago(false)
        }
    }

    #[inline]
    const fn is_valid(self) -> bool {
        const MAX_SUM: u32 = TimeId::MAX as u32;
        (MAX_SUM - self.value > LIFE_START_DATE as u32)
            && (!self.available || self.value >= SPECIAL_VALUE_SKIP)
    }
}

/// Structured online/last-seen status category for localization and UI display.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum OnlineStatusText {
    /// Currently online.
    Online,
    /// Last seen a long time ago / offline.
    LongAgo,
    /// Last seen recently (including expired local pseudo-online).
    Recently,
    /// Last seen within a week.
    WithinWeek,
    /// Last seen within a month.
    WithinMonth,
    /// Last seen just now (< 1 minute ago).
    JustNow,
    /// Last seen `minutes` ago (`1..60`).
    MinutesAgo(i32),
    /// Last seen `hours` ago (`1..12`).
    HoursAgo(i32),
    /// Last seen today at exact timestamp `till`.
    TodayAt(TimeId),
    /// Last seen yesterday at exact timestamp `till`.
    YesterdayAt(TimeId),
    /// Last seen on an older date at exact timestamp `till`.
    DateAt(TimeId),
}

impl OnlineStatusText {
    /// Classifies `status` at current timestamp `now` (`Data::OnlineText` logic).
    ///
    /// `tz_offset_secs` is the local timezone offset from UTC in seconds (use `0` for UTC).
    pub fn classify(status: LastseenStatus, now: TimeId, tz_offset_secs: i32) -> Self {
        if let Some(common) = Self::classify_common(status, now) {
            return common;
        }
        let till = status.online_till();
        let diff = (now as i64) - (till as i64);
        let minutes = diff / 60;
        if minutes <= 0 {
            return Self::JustNow;
        } else if minutes < 60 {
            return Self::MinutesAgo(minutes as i32);
        }
        let hours = diff / 3600;
        if hours < 12 {
            return Self::HoursAgo(hours as i32);
        }
        let till_day = civil_day_number(till, tz_offset_secs);
        let now_day = civil_day_number(now, tz_offset_secs);
        if till_day == now_day {
            Self::TodayAt(till)
        } else if till_day + 1 == now_day {
            Self::YesterdayAt(till)
        } else {
            Self::DateAt(till)
        }
    }

    /// Classifies `status` for full timestamp display (`Data::OnlineTextFull` logic),
    /// always using Today/Yesterday/Date rather than relative minutes/hours.
    pub fn classify_full(status: LastseenStatus, now: TimeId, tz_offset_secs: i32) -> Self {
        if let Some(common) = Self::classify_common(status, now) {
            return common;
        }
        let till = status.online_till();
        let till_day = civil_day_number(till, tz_offset_secs);
        let now_day = civil_day_number(now, tz_offset_secs);
        if till_day == now_day {
            Self::TodayAt(till)
        } else if till_day + 1 == now_day {
            Self::YesterdayAt(till)
        } else {
            Self::DateAt(till)
        }
    }

    fn classify_common(status: LastseenStatus, now: TimeId) -> Option<Self> {
        if status.is_online(now) {
            Some(Self::Online)
        } else if status.is_long_ago() {
            Some(Self::LongAgo)
        } else if status.is_recently() {
            Some(Self::Recently)
        } else if status.is_within_week() {
            Some(Self::WithinWeek)
        } else if status.is_within_month() {
            Some(Self::WithinMonth)
        } else if status.is_hidden() {
            // Expired local pseudo-online falls back to "last seen recently".
            Some(Self::Recently)
        } else {
            None
        }
    }

    /// Formats a default English human-readable status string.
    pub fn format_english(self, tz_offset_secs: i32) -> String {
        match self {
            Self::Online => "online".to_owned(),
            Self::LongAgo => "last seen a long time ago".to_owned(),
            Self::Recently => "last seen recently".to_owned(),
            Self::WithinWeek => "last seen within a week".to_owned(),
            Self::WithinMonth => "last seen within a month".to_owned(),
            Self::JustNow => "last seen just now".to_owned(),
            Self::MinutesAgo(1) => "last seen 1 minute ago".to_owned(),
            Self::MinutesAgo(m) => format!("last seen {m} minutes ago"),
            Self::HoursAgo(1) => "last seen 1 hour ago".to_owned(),
            Self::HoursAgo(h) => format!("last seen {h} hours ago"),
            Self::TodayAt(t) => {
                let (_, _, _, hh, mm) = split_unix_time(t, tz_offset_secs);
                format!("last seen today at {hh:02}:{mm:02}")
            }
            Self::YesterdayAt(t) => {
                let (_, _, _, hh, mm) = split_unix_time(t, tz_offset_secs);
                format!("last seen yesterday at {hh:02}:{mm:02}")
            }
            Self::DateAt(t) => {
                let (y, m, d, _, _) = split_unix_time(t, tz_offset_secs);
                format!("last seen {y:04}-{m:02}-{d:02}")
            }
        }
    }
}

/// Computes the refresh timeout in milliseconds until `OnlineText` could change
/// (`Data::OnlineChangeTimeout` in `data_peer_values.cpp`).
pub fn online_change_timeout_ms(status: LastseenStatus, now: TimeId, tz_offset_secs: i32) -> i64 {
    let seconds = online_phrase_change_in_seconds(status, now, tz_offset_secs);
    let ms = (seconds as i64).saturating_mul(1_000);
    ms.clamp(MIN_ONLINE_CHANGE_TIMEOUT_MS, MAX_ONLINE_CHANGE_TIMEOUT_MS)
}

fn online_phrase_change_in_seconds(
    status: LastseenStatus,
    now: TimeId,
    tz_offset_secs: i32,
) -> i32 {
    let till = status.online_till();
    if till > now {
        return ((till as i64) - (now as i64)).min(i32::MAX as i64) as i32;
    } else if status.is_hidden() {
        return i32::MAX;
    }
    let elapsed = (now as i64) - (till as i64);
    let minutes = elapsed / 60;
    if minutes < 60 {
        return ((minutes + 1) * 60 - elapsed).max(0) as i32;
    }
    let hours = elapsed / 3600;
    if hours < 12 {
        return ((hours + 1) * 3600 - elapsed).max(0) as i32;
    }
    let local_now = (now as i64) + (tz_offset_secs as i64);
    let secs_into_day = local_now.rem_euclid(SECONDS_IN_DAY as i64);
    (SECONDS_IN_DAY as i64 - secs_into_day).max(0) as i32
}

/// Computes the sort key for sorting users by online status (`Data::SortByOnlineValue`).
///
/// Bots and service users should return `-1` (handled on `User::sort_by_online_value`).
pub const fn sort_by_online_status(status: LastseenStatus, now: TimeId) -> TimeId {
    let till = status.online_till();
    if till != 0 {
        till
    } else if status.is_recently() {
        now - 3 * SECONDS_IN_DAY
    } else if status.is_within_week() {
        now - 7 * SECONDS_IN_DAY
    } else if status.is_within_month() {
        now - 30 * SECONDS_IN_DAY
    } else {
        0
    }
}

/// Returns the local day number since Unix epoch (`1970-01-01` = 0).
#[inline]
fn civil_day_number(ts: TimeId, tz_offset_secs: i32) -> i64 {
    ((ts as i64) + (tz_offset_secs as i64)).div_euclid(SECONDS_IN_DAY as i64)
}

/// Howard Hinnant's civil-from-days algorithm converting Unix timestamp + offset
/// to `(year, month, day, hour, minute)`.
fn split_unix_time(ts: TimeId, tz_offset_secs: i32) -> (i32, u32, u32, u32, u32) {
    let local = (ts as i64) + (tz_offset_secs as i64);
    let days = local.div_euclid(SECONDS_IN_DAY as i64);
    let rem = local.rem_euclid(SECONDS_IN_DAY as i64);
    let hour = (rem / 3600) as u32;
    let minute = ((rem % 3600) / 60) as u32;

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year as i32, m as u32, d as u32, hour, minute)
}
