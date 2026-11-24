//! Civil calendar date/time types for history date separators and dialog timestamps.
//!
//! Port of `QDate` / `QDateTime` calendar calculations used by
//! `Telegram/SourceFiles/history/view/history_view_element.cpp` and
//! `Telegram/SourceFiles/ui/text/format_values.cpp`.

/// Day of week (Monday = 1 .. Sunday = 7, matching Qt `QDate::dayOfWeek()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Weekday {
    /// Monday (1).
    Mon = 1,
    /// Tuesday (2).
    Tue = 2,
    /// Wednesday (3).
    Wed = 3,
    /// Thursday (4).
    Thu = 4,
    /// Friday (5).
    Fri = 5,
    /// Saturday (6).
    Sat = 6,
    /// Sunday (7).
    Sun = 7,
}

impl Weekday {
    /// Short English 3-letter weekday name (`Mon`, `Tue`, ..., `Sun`).
    #[must_use]
    pub const fn short_name(self) -> &'static str {
        match self {
            Self::Mon => "Mon",
            Self::Tue => "Tue",
            Self::Wed => "Wed",
            Self::Thu => "Thu",
            Self::Fri => "Fri",
            Self::Sat => "Sat",
            Self::Sun => "Sun",
        }
    }
}

/// Proleptic Gregorian civil date (`year`, `month` 1..=12, `day` 1..=31).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CivilDate {
    /// Calendar year (e.g. 2026).
    pub year: i32,
    /// Month in `1..=12`.
    pub month: u8,
    /// Day of month in `1..=31`.
    pub day: u8,
}

impl CivilDate {
    /// Creates a civil date.
    #[must_use]
    pub const fn new(year: i32, month: u8, day: u8) -> Self {
        Self { year, month, day }
    }

    /// Converts this date to days since 1970-01-01 using Howard Hinnant's algorithm.
    #[must_use]
    pub const fn days_since_epoch(self) -> i64 {
        let y = self.year as i64 - if self.month <= 2 { 1 } else { 0 };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = (y - era * 400) as u64;
        let m = self.month as u64;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + self.day as u64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe as i64 - 719_468
    }

    /// Converts days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    #[must_use]
    pub const fn from_days_since_epoch(days: i64) -> Self {
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = (z - era * 146_097) as u64;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let y = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = (y + if m <= 2 { 1 } else { 0 }) as i32;
        Self {
            year,
            month: m as u8,
            day: d as u8,
        }
    }

    /// Signed number of days from `self` to `other` (`QDate::daysTo`).
    #[must_use]
    pub const fn days_to(self, other: Self) -> i64 {
        other.days_since_epoch() - self.days_since_epoch()
    }

    /// Day of week for this date (1970-01-01 was Thursday).
    #[must_use]
    pub const fn weekday(self) -> Weekday {
        let d = self.days_since_epoch();
        // 1970-01-01 is Thu (index 3 when Mon=0..Sun=6).
        let idx = ((d + 3) % 7 + 7) % 7;
        match idx {
            0 => Weekday::Mon,
            1 => Weekday::Tue,
            2 => Weekday::Wed,
            3 => Weekday::Thu,
            4 => Weekday::Fri,
            5 => Weekday::Sat,
            _ => Weekday::Sun,
        }
    }
}

/// Local calendar date + time of day, plus a Unix timestamp in seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalDateTime {
    /// Calendar date in local time.
    pub date: CivilDate,
    /// Hour in `0..=23`.
    pub hour: u8,
    /// Minute in `0..=59`.
    pub minute: u8,
    /// Second in `0..=59`.
    pub second: u8,
}

impl LocalDateTime {
    /// Creates a `LocalDateTime` from calendar components.
    #[must_use]
    pub const fn new(year: i32, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Self {
        Self {
            date: CivilDate::new(year, month, day),
            hour,
            minute,
            second,
        }
    }

    /// Converts a Unix timestamp + fixed UTC offset (in seconds) into `LocalDateTime`.
    #[must_use]
    pub const fn from_unix_with_offset(unix_secs: i64, utc_offset_secs: i32) -> Self {
        let local = unix_secs + utc_offset_secs as i64;
        let days = local.div_euclid(86_400);
        let rem = local.rem_euclid(86_400);
        let hour = (rem / 3600) as u8;
        let minute = ((rem % 3600) / 60) as u8;
        let second = (rem % 60) as u8;
        Self {
            date: CivilDate::from_days_since_epoch(days),
            hour,
            minute,
            second,
        }
    }

    /// Converts a UTC Unix timestamp (`utc_offset_secs = 0`) into `LocalDateTime`.
    #[must_use]
    pub const fn from_unix_utc(unix_secs: i64) -> Self {
        Self::from_unix_with_offset(unix_secs, 0)
    }

    /// Total seconds since 1970-01-01 00:00:00 in the same local offset.
    #[must_use]
    pub const fn to_local_epoch_secs(self) -> i64 {
        self.date.days_since_epoch() * 86_400
            + self.hour as i64 * 3600
            + self.minute as i64 * 60
            + self.second as i64
    }

    /// Signed seconds from `self` to `other` (`QDateTime::secsTo`).
    #[must_use]
    pub const fn secs_to(self, other: Self) -> i64 {
        other.to_local_epoch_secs() - self.to_local_epoch_secs()
    }
}
