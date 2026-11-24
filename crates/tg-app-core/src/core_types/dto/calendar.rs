//! Pure calendar day/month DTOs and UTC month boundary helpers.

use std::collections::BTreeMap;

use crate::history::sections_model::{ScheduleDateTime, days_in_month};
use crate::stand_in::types::{MsgId, PeerId};

/// Minimum year selectable in Telegram Desktop's calendar (`calendar_box.cpp:86`, Telegram launch year).
pub const CALENDAR_MIN_YEAR: i32 = 2013;

/// Per-day summary returned by `messages.getSearchResultsCalendar` (`SearchResultsCalendarPeriod`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarDayInfo {
    /// Calendar year (e.g. `2026`).
    pub year: i32,
    /// Calendar month (`1..=12`).
    pub month: u32,
    /// Day of month (`1..=31`).
    pub day: u32,
    /// Unix timestamp (seconds) representing this day (`period.date`).
    pub date_unix: i32,
    /// Oldest matching message ID on this day (`period.min_msg_id`).
    pub min_msg_id: MsgId,
    /// Newest matching message ID on this day (`period.max_msg_id`).
    pub max_msg_id: MsgId,
    /// Number of messages on this day (`period.count`).
    pub count: u32,
    /// Optional preview thumbnail RGB tint (for media calendar days).
    pub preview_rgb: Option<[u8; 3]>,
}

impl CalendarDayInfo {
    /// Constructs a [`CalendarDayInfo`] from raw `messages.getSearchResultsCalendar` period fields.
    #[must_use]
    pub fn from_period(date_unix: i32, min_msg_id: i32, max_msg_id: i32, count: i32) -> Self {
        let dt = ScheduleDateTime::from_unix_utc(i64::from(date_unix));
        Self {
            year: dt.year,
            month: dt.month,
            day: dt.day,
            date_unix,
            min_msg_id: MsgId(min_msg_id),
            max_msg_id: MsgId(max_msg_id),
            count: u32::try_from(count).unwrap_or(0),
            preview_rgb: None,
        }
    }
}

/// Cached calendar data for a single `(year, month)`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CalendarMonthData {
    /// Calendar year.
    pub year: i32,
    /// Calendar month (`1..=12`).
    pub month: u32,
    /// Days (`1..=31`) that have at least one message.
    pub days: BTreeMap<u32, CalendarDayInfo>,
    /// Whether this month has finished loading from the backend.
    pub loaded: bool,
    /// Server-reported minimum date across the queried slice (`min_date`).
    pub min_date: i32,
    /// Server-reported minimum message ID across the queried slice (`min_msg_id`).
    pub min_msg_id: i32,
}

impl CalendarMonthData {
    /// Returns the [`CalendarDayInfo`] for `day` (`1..=31`), if any messages exist on that day.
    #[must_use]
    pub fn day_info(&self, day: u32) -> Option<&CalendarDayInfo> {
        self.days.get(&day)
    }

    /// Returns total message count across all populated days in this month.
    #[must_use]
    pub fn total_messages(&self) -> u32 {
        self.days.values().map(|d| d.count).sum()
    }
}

/// Computes the Unix UTC timestamp for the end of `(year, month)` (`23:59:59` on the last day),
/// matching `SearchCalendarController::performMonthRequest` (`data_search_calendar.cpp:104-108`).
#[must_use]
pub fn month_end_unix_utc(year: i32, month: u32) -> i32 {
    let clamped_month = month.clamp(1, 12);
    let last_day = days_in_month(year, clamped_month);
    let dt = ScheduleDateTime {
        year,
        month: clamped_month,
        day: last_day,
        hour: 23,
        minute: 59,
    };
    (dt.to_unix_utc() + 59) as i32
}

/// Computes the Unix UTC timestamp for the start of `(year, month)` (`00:00:00` on day 1).
#[must_use]
pub fn month_start_unix_utc(year: i32, month: u32) -> i32 {
    let clamped_month = month.clamp(1, 12);
    let dt = ScheduleDateTime {
        year,
        month: clamped_month,
        day: 1,
        hour: 0,
        minute: 0,
    };
    dt.to_unix_utc() as i32
}

/// Action emitted by [`CalendarBoxState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalendarBoxAction {
    /// User chose a date to jump to inside `peer_id`.
    JumpToDate {
        /// Target chat peer id.
        peer_id: PeerId,
        /// Target message ID returned by `messages.getSearchResultsCalendar` (`MsgId(0)` if date-only).
        msg_id: MsgId,
        /// Selected day Unix timestamp (UTC).
        date_unix: i32,
    },
    /// User closed / cancelled the calendar modal.
    Cancelled,
}
