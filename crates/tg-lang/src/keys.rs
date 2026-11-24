//! High-level date, weekday, month, and name formatting helpers.
//!
//! Port of `Telegram/SourceFiles/lang/lang_keys.cpp` and `lang_keys.h`.

use crate::auto::{key_ids, tags};
use crate::instance::{Instance, with_current};

/// Default built-in language code (`"en"`).
pub const DEFAULT_LANGUAGE_ID: &str = "en";

/// Return the default language code (`"en"`).
#[inline]
pub const fn default_language_id() -> &'static str {
    DEFAULT_LANGUAGE_ID
}

/// Return `id` if non-empty, otherwise [`DEFAULT_LANGUAGE_ID`].
#[inline]
pub fn language_id_or_default(id: &str) -> &str {
    if id.is_empty() {
        DEFAULT_LANGUAGE_ID
    } else {
        id
    }
}

/// Simple calendar date `(year, month 1..=12, day 1..=31)` for date-formatting helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimpleDate {
    /// Four-digit year (e.g., `2026`).
    pub year: i32,
    /// Month (`1..=12`).
    pub month: u8,
    /// Day of month (`1..=31`).
    pub day: u8,
}

/// Simple wall-clock time `(hour 0..=23, minute 0..=59)` for date-time formatting helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimpleTime {
    /// Hour (`0..=23`).
    pub hour: u8,
    /// Minute (`0..=59`).
    pub minute: u8,
}

/// A strongly-typed reference to a plain (tag-free) phrase slot (`tr::phrase<>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlainPhrase(pub u16);

impl PlainPhrase {
    /// Resolve this phrase from `instance`.
    #[inline]
    pub fn get(self, instance: &Instance) -> String {
        instance.format_plain(self.0)
    }

    /// Resolve this phrase from the global active [`Instance`].
    #[inline]
    pub fn now(self) -> String {
        with_current(|inst| inst.format_plain(self.0))
    }
}

/// Look up the plain month name phrase for `index` (`1..=12`, `Lang::Month`).
pub const fn month(index: u8) -> Option<PlainPhrase> {
    Some(PlainPhrase(match index {
        1 => key_ids::lng_month1,
        2 => key_ids::lng_month2,
        3 => key_ids::lng_month3,
        4 => key_ids::lng_month4,
        5 => key_ids::lng_month5,
        6 => key_ids::lng_month6,
        7 => key_ids::lng_month7,
        8 => key_ids::lng_month8,
        9 => key_ids::lng_month9,
        10 => key_ids::lng_month10,
        11 => key_ids::lng_month11,
        12 => key_ids::lng_month12,
        _ => return None,
    }))
}

/// Look up the short month name phrase for `index` (`1..=12`, `Lang::MonthSmall`).
pub const fn month_small(index: u8) -> Option<PlainPhrase> {
    Some(PlainPhrase(match index {
        1 => key_ids::lng_month1_small,
        2 => key_ids::lng_month2_small,
        3 => key_ids::lng_month3_small,
        4 => key_ids::lng_month4_small,
        5 => key_ids::lng_month5_small,
        6 => key_ids::lng_month6_small,
        7 => key_ids::lng_month7_small,
        8 => key_ids::lng_month8_small,
        9 => key_ids::lng_month9_small,
        10 => key_ids::lng_month10_small,
        11 => key_ids::lng_month11_small,
        12 => key_ids::lng_month12_small,
        _ => return None,
    }))
}

/// Look up the genitive/day-form month phrase for `index` (`1..=12`, `Lang::MonthDay`).
pub const fn month_day(index: u8) -> Option<PlainPhrase> {
    Some(PlainPhrase(match index {
        1 => key_ids::lng_month_day1,
        2 => key_ids::lng_month_day2,
        3 => key_ids::lng_month_day3,
        4 => key_ids::lng_month_day4,
        5 => key_ids::lng_month_day5,
        6 => key_ids::lng_month_day6,
        7 => key_ids::lng_month_day7,
        8 => key_ids::lng_month_day8,
        9 => key_ids::lng_month_day9,
        10 => key_ids::lng_month_day10,
        11 => key_ids::lng_month_day11,
        12 => key_ids::lng_month_day12,
        _ => return None,
    }))
}

/// Look up the short weekday phrase for ISO weekday `index` (`1 = Mon ..= 7 = Sun`, `Lang::Weekday`).
pub const fn weekday(index: u8) -> Option<PlainPhrase> {
    Some(PlainPhrase(match index {
        1 => key_ids::lng_weekday1,
        2 => key_ids::lng_weekday2,
        3 => key_ids::lng_weekday3,
        4 => key_ids::lng_weekday4,
        5 => key_ids::lng_weekday5,
        6 => key_ids::lng_weekday6,
        7 => key_ids::lng_weekday7,
        _ => return None,
    }))
}

/// Look up the full weekday phrase for ISO weekday `index` (`1 = Mon ..= 7 = Sun`, `Lang::WeekdayFull`).
pub const fn weekday_full(index: u8) -> Option<PlainPhrase> {
    Some(PlainPhrase(match index {
        1 => key_ids::lng_hours_monday,
        2 => key_ids::lng_hours_tuesday,
        3 => key_ids::lng_hours_wednesday,
        4 => key_ids::lng_hours_thursday,
        5 => key_ids::lng_hours_friday,
        6 => key_ids::lng_hours_saturday,
        7 => key_ids::lng_hours_sunday,
        _ => return None,
    }))
}

/// Return whether the active language places last name before first name (`langFirstNameGoesSecond`).
pub fn first_name_goes_second(instance: &Instance) -> bool {
    const FIRST: &str = "\u{0001}";
    const LAST: &str = "\u{0002}";
    let fullname = instance.format_tagged(
        key_ids::lng_full_name,
        &[(tags::FIRST_NAME, FIRST), (tags::LAST_NAME, LAST)],
    );
    match (fullname.find(LAST), fullname.find(FIRST)) {
        (Some(last_pos), Some(first_pos)) => last_pos < first_pos,
        _ => false,
    }
}

/// Format a person's full name according to the language's name order (`langFullName`).
pub fn full_name(instance: &Instance, first_name: &str, last_name: &str) -> String {
    if first_name.is_empty() {
        return last_name.to_string();
    }
    if last_name.is_empty() {
        return first_name.to_string();
    }
    if first_name_goes_second(instance) {
        format!("{last_name} {first_name}")
    } else {
        format!("{first_name} {last_name}")
    }
}

fn should_show_year(date: SimpleDate, today: SimpleDate) -> bool {
    if date.year == today.year {
        return false;
    }
    let year_is_much_greater = |y: i32, other: i32| y > other + 1;
    let month_is_much_greater = |y: i32, m: u8, other_y: i32, other_m: u8| {
        y == other_y + 1 && i32::from(m) + 12 > i32::from(other_m) + 3
    };
    year_is_much_greater(date.year, today.year)
        || year_is_much_greater(today.year, date.year)
        || month_is_much_greater(date.year, date.month, today.year, today.month)
        || month_is_much_greater(today.year, today.month, date.year, date.month)
}

/// Format `"Month Day"` or `"Month Day, Year"` using short month names (`langDayOfMonth`).
pub fn day_of_month(instance: &Instance, date: SimpleDate, today: SimpleDate) -> String {
    let Some(m) = month_small(date.month) else {
        return "MONTH_ERR".to_string();
    };
    let month_str = m.get(instance);
    let day_str = date.day.to_string();
    if should_show_year(date, today) {
        let year_str = date.year.to_string();
        instance.format_tagged(
            key_ids::lng_month_day_year,
            &[
                (tags::MONTH, &month_str),
                (tags::DAY, &day_str),
                (tags::YEAR, &year_str),
            ],
        )
    } else {
        instance.format_tagged(
            key_ids::lng_month_day,
            &[(tags::MONTH, &month_str), (tags::DAY, &day_str)],
        )
    }
}

/// Format `"Month Day"` or `"Month Day, Year"` using full day-form month names (`langDayOfMonthFull`).
pub fn day_of_month_full(instance: &Instance, date: SimpleDate, today: SimpleDate) -> String {
    let Some(m) = month_day(date.month) else {
        return "MONTH_ERR".to_string();
    };
    let month_str = m.get(instance);
    let day_str = date.day.to_string();
    if should_show_year(date, today) {
        let year_str = date.year.to_string();
        instance.format_tagged(
            key_ids::lng_month_day_year,
            &[
                (tags::MONTH, &month_str),
                (tags::DAY, &day_str),
                (tags::YEAR, &year_str),
            ],
        )
    } else {
        instance.format_tagged(
            key_ids::lng_month_day,
            &[(tags::MONTH, &month_str), (tags::DAY, &day_str)],
        )
    }
}

/// Format `"Month Year"` (`langMonthOfYearFull`).
pub fn month_of_year_full(instance: &Instance, month_index: u8, year: i32) -> String {
    let Some(m) = month(month_index) else {
        return "MONTH_ERR".to_string();
    };
    let month_str = m.get(instance);
    let year_str = year.to_string();
    instance.format_tagged(
        key_ids::lng_month_year,
        &[(tags::MONTH, &month_str), (tags::YEAR, &year_str)],
    )
}

/// Format capitalized month heading, optionally including the year (`langMonthFull`).
pub fn month_full(instance: &Instance, date: SimpleDate, today: SimpleDate) -> String {
    let Some(m) = month(date.month) else {
        return "MONTH_ERR".to_string();
    };
    let text = if should_show_year(date, today) {
        month_of_year_full(instance, date.month, date.year)
    } else {
        m.get(instance)
    };
    capitalize_first(&text)
}

/// Format short weekday (`langDayOfWeek`, `1..=7`).
pub fn day_of_week(instance: &Instance, index: u8) -> String {
    weekday(index)
        .map(|p| p.get(instance))
        .unwrap_or_else(|| "DAY_ERR".to_string())
}

/// Format full weekday (`langDayOfWeekFull`, `1..=7`).
pub fn day_of_week_full(instance: &Instance, index: u8) -> String {
    weekday_full(index)
        .map(|p| p.get(instance))
        .unwrap_or_else(|| "DAY_ERR".to_string())
}

/// Format date and time (`langDateTime`).
pub fn date_time(
    instance: &Instance,
    date: SimpleDate,
    time: SimpleTime,
    today: SimpleDate,
) -> String {
    let date_str = day_of_month(instance, date, today);
    let time_str = format!("{:02}:{:02}", time.hour, time.minute);
    instance.format_tagged(
        key_ids::lng_mediaview_date_time,
        &[(tags::DATE, &date_str), (tags::TIME, &time_str)],
    )
}

/// Format full date and time (`langDateTimeFull`).
pub fn date_time_full(
    instance: &Instance,
    date: SimpleDate,
    time: SimpleTime,
    today: SimpleDate,
) -> String {
    let date_str = day_of_month_full(instance, date, today);
    let time_str = format!("{:02}:{:02}", time.hour, time.minute);
    instance.format_tagged(
        key_ids::lng_mediaview_date_time,
        &[(tags::DATE, &date_str), (tags::TIME, &time_str)],
    )
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => {
            let mut out = first.to_uppercase().to_string();
            out.push_str(chars.as_str());
            out
        }
    }
}
