//! Civil calendar date/time types for history date separators and dialog timestamps.
//!
//! Port of `QDate` / `QDateTime` calendar calculations used by
//! `Telegram/SourceFiles/history/view/history_view_element.cpp` and
//! `Telegram/SourceFiles/ui/text/format_values.cpp`.

use std::cell::RefCell;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use jiff::tz::TimeZone;

/// Sentinel stored in [`UTC_OFFSET_OVERRIDE`] when no override is active.
const NO_OVERRIDE: i64 = i64::MIN;

/// Process-wide fixed UTC offset used instead of the system time zone (deterministic tests).
static UTC_OFFSET_OVERRIDE: AtomicI64 = AtomicI64::new(NO_OVERRIDE);

/// Forces every local-time conversion to use `offset_secs` (`None` restores the system zone).
///
/// Headless tests and screenshot captures call this so rendered times do not depend on the
/// machine's time zone.
pub fn set_utc_offset_override(offset_secs: Option<i32>) {
    UTC_OFFSET_OVERRIDE.store(
        offset_secs.map_or(NO_OVERRIDE, i64::from),
        Ordering::Relaxed,
    );
}

/// Width of the UTC ranges [`local_utc_offset_secs`] caches one offset for. Time-zone and
/// daylight-saving transitions since Telegram exists fall on whole quarter hours of UTC (every
/// zone's offset is a multiple of 15 minutes and clocks change on the local minute 0, 15, 30
/// or 45), so the offset is the same for every second of a range.
const OFFSET_RANGE_SECS: i64 = 900;
/// Ranges cached per thread (a chat list or a history page spans a handful).
const OFFSET_CACHE_SLOTS: usize = 64;
/// How long cached offsets are trusted before the system zone is asked again, so a changed
/// time zone shows within a minute.
const OFFSET_CACHE_TTL: Duration = Duration::from_secs(60);

/// Per-thread direct-mapped cache of `(range index, offset)`; `i64::MIN` marks an empty slot.
struct OffsetCache {
    filled_at: Option<Instant>,
    slots: [(i64, i32); OFFSET_CACHE_SLOTS],
}

thread_local! {
    static OFFSET_CACHE: RefCell<OffsetCache> = const {
        RefCell::new(OffsetCache {
            filled_at: None,
            slots: [(i64::MIN, 0); OFFSET_CACHE_SLOTS],
        })
    };
}

/// Asks the system time zone (a zone handle lookup plus a search of its transitions per call,
/// which is why the result is cached).
fn system_utc_offset_secs(unix_secs: i64) -> i32 {
    Timestamp::from_second(unix_secs).map_or(0, |ts| TimeZone::system().to_offset(ts).seconds())
}

/// UTC offset (seconds east of UTC) of the system local time zone at `unix_secs`, including
/// daylight saving time, or the value set by [`set_utc_offset_override`].
///
/// Every message and chat-list time calls this, so offsets are cached per 15-minute UTC range
/// and re-read from the system after [`OFFSET_CACHE_TTL`].
#[must_use]
pub fn local_utc_offset_secs(unix_secs: i64) -> i32 {
    let forced = UTC_OFFSET_OVERRIDE.load(Ordering::Relaxed);
    if forced != NO_OVERRIDE {
        return i32::try_from(forced).unwrap_or(0);
    }
    cached_system_utc_offset_secs(unix_secs)
}

/// [`system_utc_offset_secs`] through the per-thread range cache.
fn cached_system_utc_offset_secs(unix_secs: i64) -> i32 {
    let range = unix_secs.div_euclid(OFFSET_RANGE_SECS);
    OFFSET_CACHE.with(|cell| {
        let Ok(mut cache) = cell.try_borrow_mut() else {
            return system_utc_offset_secs(unix_secs);
        };
        let now = Instant::now();
        if cache
            .filled_at
            .is_none_or(|at| now.duration_since(at) >= OFFSET_CACHE_TTL)
        {
            cache.slots = [(i64::MIN, 0); OFFSET_CACHE_SLOTS];
            cache.filled_at = Some(now);
        }
        let slot = range.rem_euclid(OFFSET_CACHE_SLOTS as i64) as usize;
        if cache.slots[slot].0 == range {
            return cache.slots[slot].1;
        }
        let offset = system_utc_offset_secs(unix_secs);
        cache.slots[slot] = (range, offset);
        offset
    })
}

/// Formats a message time like tdesktop's `QLocale::toString(time, QLocale::ShortFormat)` with a
/// 24-hour system locale (`"H:mm"`, e.g. `"0:02"`, `"23:47"`).
#[must_use]
pub fn format_time_short(dt: LocalDateTime) -> String {
    format!("{}:{:02}", dt.hour, dt.minute)
}

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

    /// Converts a Unix timestamp into the system local time zone (`base::unixtime::parse`,
    /// which tdesktop renders through `QDateTime::fromSecsSinceEpoch` in local time).
    #[must_use]
    pub fn from_unix_local(unix_secs: i64) -> Self {
        Self::from_unix_with_offset(unix_secs, local_utc_offset_secs(unix_secs))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_conversion_uses_offset_override_and_short_time_format() {
        set_utc_offset_override(Some(2 * 3600));
        // 2026-11-24 22:02:00 UTC -> 2026-11-25 00:02 at UTC+2.
        let dt = LocalDateTime::from_unix_local(1_795_557_720);
        set_utc_offset_override(None);
        assert_eq!(dt.date, CivilDate::new(2026, 11, 25));
        assert_eq!(format_time_short(dt), "0:02");
        assert_eq!(
            format_time_short(LocalDateTime::new(2026, 11, 24, 23, 47, 0)),
            "23:47"
        );
    }

    #[test]
    fn cached_offset_matches_the_system_zone_across_dst_changes() {
        // Every 7 minutes over 2025-2026 (both DST changes of each year in zones that have
        // them), twice, so the second pass reads the cache.
        let start = 1_735_689_600_i64; // 2025-01-01 00:00 UTC
        let end = start + 2 * 366 * 86_400;
        for _ in 0..2 {
            let mut t = start;
            while t < end {
                assert_eq!(
                    cached_system_utc_offset_secs(t),
                    system_utc_offset_secs(t),
                    "offset at {t}"
                );
                t += 7 * 60;
            }
        }
        // Range edges and pre-1970 seconds.
        for t in [-1, 0, 899, 900, 901, -899, -900, -901] {
            assert_eq!(cached_system_utc_offset_secs(t), system_utc_offset_secs(t));
        }
    }

    #[test]
    fn cached_offset_is_cheaper_than_the_system_query() {
        const CALLS: i64 = 20_000;
        // A history page: timestamps a few minutes apart.
        let base = 1_795_557_720_i64;
        let started = std::time::Instant::now();
        let mut direct = 0_i64;
        for i in 0..CALLS {
            direct += i64::from(system_utc_offset_secs(base + i * 37));
        }
        let direct_time = started.elapsed();
        let started = std::time::Instant::now();
        let mut cached = 0_i64;
        for i in 0..CALLS {
            cached += i64::from(cached_system_utc_offset_secs(base + i * 37));
        }
        let cached_time = started.elapsed();
        assert_eq!(direct, cached);
        println!(
            "measured: {CALLS} timestamps, system query {:.3} us each, cached {:.3} us each",
            direct_time.as_secs_f64() * 1e6 / CALLS as f64,
            cached_time.as_secs_f64() * 1e6 / CALLS as f64
        );
        assert!(cached_time < direct_time);
    }
}
