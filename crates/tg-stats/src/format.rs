//! Number, currency (TON / Stars / USD), and UTC date formatting helpers.
//!
//! Port of:
//! - `Telegram/SourceFiles/statistics/statistics_format_values.{h,cpp}`
//! - `Telegram/SourceFiles/lang/lang_tag.cpp` (`FormatCountToShort`, `FormatCountDecimal`,
//!   `FormatExactCountDecimal`, `FormatCreditsAmountDecimal`)
//! - `Telegram/SourceFiles/core/credits_amount.h`
//! - `Telegram/SourceFiles/info/channel_statistics/earn/earn_format.{h,cpp}`

/// Number of nano-units per whole Star or TON (`kOneStarInNano`).
pub const ONE_STAR_IN_NANO: i64 = 1_000_000_000;

/// Currency code identifying Telegram Stars in chart `yTickFormatter` (`Ui::kCreditsCurrency`).
pub const CREDITS_CURRENCY: &str = "XTR";

/// Em-dash character (`\u{2014}`) used in date ranges and chart line names.
pub const EM_DASH: char = '\u{2014}';

/// Minus sign (`\u{2212}`) used in negative overview deltas.
pub const MINUS_SIGN: char = '\u{2212}';

/// Approximately-equal sign (`\u{2248}`) used in USD and premium audience approximations.
pub const APPROXIMATELY_SIGN: char = '\u{2248}';

/// Currency kind for a [`CreditsAmount`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CreditsType {
    /// Telegram Stars.
    #[default]
    Stars,
    /// Toncoin (TON).
    Ton,
}

/// Fixed-point currency amount in whole units + nano-units (`10^-9`).
///
/// Port of `CreditsAmount` from `core/credits_amount.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CreditsAmount {
    whole: i64,
    nano: i64,
    credits_type: CreditsType,
}

impl CreditsAmount {
    /// Creates a whole-unit amount.
    #[must_use]
    pub const fn from_whole(whole: i64, credits_type: CreditsType) -> Self {
        Self {
            whole,
            nano: 0,
            credits_type,
        }
    }

    /// Creates a normalized `(whole, nano)` amount.
    #[must_use]
    pub fn new(whole: i64, nano: i64, credits_type: CreditsType) -> Self {
        let mut amount = Self {
            whole,
            nano,
            credits_type,
        };
        amount.normalize();
        amount
    }

    /// Creates a TON amount from nanoton (`provided / 1e9`, `provided % 1e9`).
    #[must_use]
    pub fn from_nanoton(nanoton: i64) -> Self {
        Self::new(
            nanoton / ONE_STAR_IN_NANO,
            nanoton % ONE_STAR_IN_NANO,
            CreditsType::Ton,
        )
    }

    /// Returns the whole-unit part.
    #[must_use]
    pub const fn whole(&self) -> i64 {
        self.whole
    }

    /// Returns the fractional nano-unit part (`0..ONE_STAR_IN_NANO`).
    #[must_use]
    pub const fn nano(&self) -> i64 {
        self.nano
    }

    /// Returns the currency type.
    #[must_use]
    pub const fn credits_type(&self) -> CreditsType {
        self.credits_type
    }

    /// Returns `true` if this is a TON amount.
    #[must_use]
    pub const fn is_ton(&self) -> bool {
        matches!(self.credits_type, CreditsType::Ton)
    }

    /// Returns `true` if this is a Stars amount.
    #[must_use]
    pub const fn is_stars(&self) -> bool {
        matches!(self.credits_type, CreditsType::Stars)
    }

    /// Returns `true` if both whole and nano parts are zero.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.whole == 0 && self.nano == 0
    }

    /// Returns the floating-point value `whole + nano / 1e9`.
    #[must_use]
    pub fn value(&self) -> f64 {
        (self.whole as f64) + (self.nano as f64) / (ONE_STAR_IN_NANO as f64)
    }

    /// Multiplies this amount by `rate`, returning a normalized [`CreditsAmount`].
    #[must_use]
    pub fn multiplied(&self, rate: f64) -> Self {
        let result = self.value() * rate;
        let abs = result.abs();
        let whole = abs.floor() as i64;
        let nano = ((abs - (whole as f64)) * (ONE_STAR_IN_NANO as f64)).round() as i64;
        if result < 0.0 {
            Self::new(-whole, -nano, self.credits_type)
        } else {
            Self::new(whole, nano, self.credits_type)
        }
    }

    fn normalize(&mut self) {
        if self.nano < 0 {
            let shifts = (-self.nano + ONE_STAR_IN_NANO - 1) / ONE_STAR_IN_NANO;
            self.nano += shifts * ONE_STAR_IN_NANO;
            self.whole -= shifts;
        } else if self.nano >= ONE_STAR_IN_NANO {
            let shifts = self.nano / ONE_STAR_IN_NANO;
            self.nano -= shifts * ONE_STAR_IN_NANO;
            self.whole += shifts;
        }
    }
}

/// Output of [`format_count_to_short`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortenedCount {
    /// Scaled/truncated numeric value (matches `ShortenedCount::number`).
    pub number: i64,
    /// Human-readable string (e.g. `"12.3K"`, `"1M"`, `"9999"`).
    pub string: String,
    /// `true` if a `K`/`M`/`B` suffix was applied.
    pub shortened: bool,
}

/// Formats an integer count using Telegram Desktop's short-count rules (`K`/`M`/`B`).
///
/// Port of `Lang::FormatCountToShort` (`lang/lang_tag.cpp:920-947`):
/// - `|n| >= 1_000_000_000` (when `!only_k`) -> `B`
/// - `|n| >= 1_000_000` (when `!only_k`) -> `M`
/// - `|n| >= 10_000` -> `K`
/// - otherwise plain integer string.
#[must_use]
pub fn format_count_to_short(number: i64, only_k: bool) -> ShortenedCount {
    let abs = number.unsigned_abs();
    let shorten = |divider: u64, multiplier: char| -> ShortenedCount {
        let sign: i64 = if number > 0 { 1 } else { -1 };
        let rounded = abs / (divider / 10);
        let major = sign * ((rounded / 10) as i64);
        let minor = rounded % 10;
        let string = if minor != 0 {
            format!("{major}.{minor}{multiplier}")
        } else {
            format!("{major}{multiplier}")
        };
        let scaled = (rounded as i64).saturating_mul(divider as i64);
        ShortenedCount {
            number: scaled,
            string,
            shortened: true,
        }
    };

    if !only_k && abs >= 1_000_000_000 {
        shorten(1_000_000_000, 'B')
    } else if !only_k && abs >= 1_000_000 {
        shorten(1_000_000, 'M')
    } else if abs >= 10_000 {
        shorten(1_000, 'K')
    } else {
        ShortenedCount {
            number,
            string: number.to_string(),
            shortened: false,
        }
    }
}

/// Formats an integer with ASCII comma thousands separators (e.g. `1,234,567`).
///
/// Port of `Lang::FormatCountDecimal`.
#[must_use]
pub fn format_count_decimal(number: i64) -> String {
    let negative = number < 0;
    let digits = number.unsigned_abs().to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3 + usize::from(negative));
    if negative {
        out.push('-');
    }
    for (idx, ch) in digits.chars().enumerate() {
        if idx > 0 && (len - idx).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Formats a floating-point count up to 6 decimal places without trailing zeros.
///
/// Port of `Lang::FormatExactCountDecimal` (`lang/lang_tag.cpp:953-971`).
#[must_use]
pub fn format_exact_count_decimal(number: f64) -> String {
    let rounded = number.round();
    if (number - rounded).abs() <= 1e-12 * number.abs().max(1.0) {
        return format_count_decimal(rounded as i64);
    }

    let formatted = format!("{number:.6}");
    let trimmed = formatted.trim_end_matches('0');
    if let Some(int_part) = trimmed.strip_suffix('.') {
        if let Ok(parsed) = int_part.parse::<i64>() {
            return format_count_decimal(parsed);
        }
        return format_count_decimal(rounded as i64);
    }
    if let Some((int_part, frac_part)) = trimmed.split_once('.')
        && let Ok(parsed) = int_part.parse::<i64>()
    {
        let prefix = if parsed == 0 && number < 0.0 {
            "-0".to_string()
        } else {
            format_count_decimal(parsed)
        };
        return format!("{prefix}.{frac_part}");
    }
    trimmed.to_string()
}

/// Formats a [`CreditsAmount`] with exact decimals.
///
/// Port of `Lang::FormatCreditsAmountDecimal`.
#[must_use]
pub fn format_credits_amount_decimal(amount: CreditsAmount) -> String {
    format_exact_count_decimal(amount.value())
}

/// Formats the integer (major) part of a nanoton `u64` value.
///
/// Port of `Info::ChannelEarn::MajorPart(EarnInt)`.
#[must_use]
pub fn major_part_nanoton(value: u64) -> String {
    const MINOR_LEN: usize = 9;
    let s = value.to_string();
    if s.len() <= MINOR_LEN {
        "0".to_string()
    } else {
        s[..s.len() - MINOR_LEN].to_string()
    }
}

/// Formats the fractional (minor) part of a nanoton `u64` value with leading `.`,
/// keeping at least 2 decimal digits.
///
/// Port of `Info::ChannelEarn::MinorPart(EarnInt)`.
#[must_use]
pub fn minor_part_nanoton(value: u64) -> String {
    const MINOR_LEN: usize = 9;
    const MAX_CHOPPED_ZERO: usize = MINOR_LEN - 2;
    if value == 0 {
        return ".00".to_string();
    }
    let s = value.to_string();
    let result = if s.len() < MINOR_LEN {
        format!(".{:0>width$}", s, width = MINOR_LEN)
    } else {
        format!(".{}", &s[s.len() - MINOR_LEN..])
    };
    let bytes = result.as_bytes();
    let mut zero_count = 0usize;
    let mut idx = bytes.len();
    while idx > 1 {
        idx -= 1;
        if bytes[idx] == b'0' && zero_count < MAX_CHOPPED_ZERO {
            zero_count += 1;
        } else {
            break;
        }
    }
    result[..result.len() - zero_count].to_string()
}

/// Formats the major (integer) part of a [`CreditsAmount`].
///
/// Port of `Info::ChannelEarn::MajorPart(CreditsAmount)`.
#[must_use]
pub fn major_part_credits(amount: CreditsAmount) -> String {
    (amount.value() as i64).to_string()
}

/// Formats the minor (2-digit fractional) part of a [`CreditsAmount`], e.g. `".25"`.
///
/// Port of `Info::ChannelEarn::MinorPart(CreditsAmount)`.
#[must_use]
pub fn minor_part_credits(amount: CreditsAmount) -> String {
    let fractional = ((amount.value() * 100.0) as i64).unsigned_abs() % 100;
    format!(".{:02}", fractional)
}

/// Formats a currency value converted to USD: `"≈$12.34"`.
///
/// Port of `Info::ChannelEarn::ToUsd` (`earn_format.cpp:81-93`).
/// When `after_float == 0`, defaults to 2 decimal places.
#[must_use]
pub fn to_usd(value: f64, rate: f64, after_float: usize) -> String {
    let precision = if after_float == 0 { 2 } else { after_float };
    format!(
        "{APPROXIMATELY_SIGN}${:.prec$}",
        value * rate,
        prec = precision
    )
}

/// Formats a [`CreditsAmount`] converted to USD.
#[must_use]
pub fn credits_to_usd(amount: CreditsAmount, rate: f64, after_float: usize) -> String {
    to_usd(amount.value(), rate, after_float)
}

/// UTC calendar date and time broken down into fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UtcDateTime {
    /// Full Gregorian year (e.g. `2024`).
    pub year: i32,
    /// Month `1..=12`.
    pub month: u8,
    /// Day of month `1..=31`.
    pub day: u8,
    /// ISO weekday: `1` = Monday .. `7` = Sunday.
    pub weekday: u8,
    /// Hour `0..=23`.
    pub hour: u8,
    /// Minute `0..=59`.
    pub minute: u8,
    /// Second `0..=59`.
    pub second: u8,
}

impl UtcDateTime {
    /// Converts a Unix timestamp in seconds to a [`UtcDateTime`].
    ///
    /// Uses Howard Hinnant's civil-from-days algorithm for exact proleptic Gregorian dates.
    #[must_use]
    pub fn from_unix_seconds(seconds: i64) -> Self {
        let days = seconds.div_euclid(86_400);
        let secs_of_day = seconds.rem_euclid(86_400) as u32;
        let hour = (secs_of_day / 3600) as u8;
        let minute = ((secs_of_day % 3600) / 60) as u8;
        let second = (secs_of_day % 60) as u8;

        // 1970-01-01 was Thursday (ISO weekday 4).
        let weekday = ((days + 3).rem_euclid(7) + 1) as u8;

        // Civil from days (Hinnant).
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = (z - era * 146_097) as u64;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = (yoe as i64) + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = (y + i64::from(m <= 2)) as i32;

        Self {
            year,
            month: m as u8,
            day: d as u8,
            weekday,
            hour,
            minute,
            second,
        }
    }
}

/// Short English month name (`"Jan"`..`"Dec"`).
#[must_use]
pub const fn month_short(month: u8) -> &'static str {
    match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "",
    }
}

/// Short English weekday name (`1` = `"Mon"`..`7` = `"Sun"`).
#[must_use]
pub const fn weekday_short(weekday: u8) -> &'static str {
    match weekday {
        1 => "Mon",
        2 => "Tue",
        3 => "Wed",
        4 => "Thu",
        5 => "Fri",
        6 => "Sat",
        7 => "Sun",
        _ => "",
    }
}

/// Formats a UTC timestamp in seconds as `"D Mon YYYY"` (e.g. `"15 Nov 2023"`).
///
/// Port of `Statistic::LangDayMonthYear`.
#[must_use]
pub fn lang_day_month_year(seconds: i64) -> String {
    let dt = UtcDateTime::from_unix_seconds(seconds);
    format!("{} {} {}", dt.day, month_short(dt.month), dt.year)
}

/// Formats a UTC timestamp in seconds as `"D Mon"` (e.g. `"15 Nov"`).
///
/// Port of `Statistic::LangDayMonth`.
#[must_use]
pub fn lang_day_month(seconds: i64) -> String {
    let dt = UtcDateTime::from_unix_seconds(seconds);
    format!("{} {}", dt.day, month_short(dt.month))
}

/// Formats a UTC timestamp in seconds with weekday and optional time.
///
/// Port of `Statistic::LangDetailedDayMonth`.
#[must_use]
pub fn lang_detailed_day_month(seconds: i64) -> String {
    const ONE_DAY: i64 = 3600 * 24;
    let dt = UtcDateTime::from_unix_seconds(seconds);
    if dt.hour != 0 || dt.minute != 0 {
        if seconds < ONE_DAY {
            return format!("{:02}:{:02}", dt.hour, dt.minute);
        }
        format!(
            "{}, {} {}, {:02}:{:02}",
            weekday_short(dt.weekday),
            dt.day,
            month_short(dt.month),
            dt.hour,
            dt.minute
        )
    } else {
        format!(
            "{}, {} {} {}",
            weekday_short(dt.weekday),
            dt.day,
            month_short(dt.month),
            dt.year
        )
    }
}

/// Formats a week range starting at `timestamp_ms`: `"D Mon — D Mon YYYY"`.
///
/// Port of `FormatWeek` in `statistics/widgets/point_details_widget.cpp`.
#[must_use]
pub fn format_week(timestamp_ms: f64) -> String {
    const SEVEN_DAYS: i64 = 3600 * 24 * 7;
    let secs = (timestamp_ms / 1000.0) as i64;
    format!(
        "{} {EM_DASH} {}",
        lang_day_month(secs),
        lang_day_month_year(secs + SEVEN_DAYS)
    )
}

/// Formats the chart header subtitle date range for `[x_index_min, x_index_max]`.
///
/// Port of `HeaderSubTitle` in `statistics/chart_widget.cpp`.
#[must_use]
pub fn header_subtitle(
    x: &[f64],
    week_format: bool,
    x_index_min: usize,
    x_index_max: usize,
) -> String {
    const ONE_DAY_MS: f64 = (3600 * 24 * 1000) as f64;
    const SEVEN_DAYS_SECS: i64 = 3600 * 24 * 7;

    let Some(&left_ts) = x.get(x_index_min) else {
        return String::new();
    };
    if left_ts < ONE_DAY_MS {
        return String::new();
    }
    let left_text = lang_day_month_year((left_ts / 1000.0) as i64);
    if x_index_min == x_index_max && !week_format {
        left_text
    } else {
        let right_raw = x.get(x_index_max).copied().unwrap_or(left_ts);
        let right_secs =
            (right_raw / 1000.0) as i64 + if week_format { SEVEN_DAYS_SECS } else { 0 };
        format!("{left_text} {EM_DASH} {}", lang_day_month_year(right_secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_counts_match_tdesktop() {
        assert_eq!(format_count_to_short(0, false).string, "0");
        assert_eq!(format_count_to_short(9_999, false).string, "9999");
        assert!(!format_count_to_short(9_999, false).shortened);
        assert_eq!(format_count_to_short(10_000, false).string, "10K");
        assert!(format_count_to_short(10_000, false).shortened);
        assert_eq!(format_count_to_short(12_345, false).string, "12.3K");
        assert_eq!(format_count_to_short(999_999, false).string, "999.9K");
        assert_eq!(format_count_to_short(1_000_000, false).string, "1M");
        assert_eq!(format_count_to_short(1_500_000, false).string, "1.5M");
        assert_eq!(format_count_to_short(1_500_000, true).string, "1500K");
        assert_eq!(format_count_to_short(2_340_000_000, false).string, "2.3B");
        assert_eq!(format_count_to_short(-12_345, false).string, "-12.3K");
    }

    #[test]
    fn decimal_and_exact_counts() {
        assert_eq!(format_count_decimal(0), "0");
        assert_eq!(format_count_decimal(1234), "1,234");
        assert_eq!(format_count_decimal(1_234_567), "1,234,567");
        assert_eq!(format_count_decimal(-987_654), "-987,654");
        assert_eq!(format_exact_count_decimal(1234.0), "1,234");
        assert_eq!(format_exact_count_decimal(1234.5), "1,234.5");
        assert_eq!(format_exact_count_decimal(0.125), "0.125");
    }

    #[test]
    fn earn_and_credits_formatting() {
        assert_eq!(major_part_nanoton(0), "0");
        assert_eq!(minor_part_nanoton(0), ".00");
        assert_eq!(major_part_nanoton(12_345_000_000), "12");
        assert_eq!(minor_part_nanoton(12_345_000_000), ".345");
        assert_eq!(major_part_nanoton(50_000_000), "0");
        assert_eq!(minor_part_nanoton(50_000_000), ".05");
        assert_eq!(minor_part_nanoton(1_000_000_000), ".00");

        let ton = CreditsAmount::from_nanoton(1_500_000_000);
        assert_eq!(major_part_credits(ton), "1");
        assert_eq!(minor_part_credits(ton), ".50");
        assert_eq!(credits_to_usd(ton, 5.2, 0), "\u{2248}$7.80");
        assert_eq!(credits_to_usd(ton, 5.2, 3), "\u{2248}$7.800");

        let neg = CreditsAmount::new(1, -250_000_000, CreditsType::Ton);
        assert_eq!(neg.whole(), 0);
        assert_eq!(neg.nano(), 750_000_000);
    }

    #[test]
    fn utc_dates_known_answers() {
        // 1700006400 = 2023-11-15 00:00:00 UTC (Wednesday)
        assert_eq!(lang_day_month(1_700_006_400), "15 Nov");
        assert_eq!(lang_day_month_year(1_700_006_400), "15 Nov 2023");
        assert_eq!(lang_detailed_day_month(1_700_006_400), "Wed, 15 Nov 2023");
        // 2023-11-15 14:30:00 UTC
        assert_eq!(
            lang_detailed_day_month(1_700_006_400 + 14 * 3600 + 30 * 60),
            "Wed, 15 Nov, 14:30"
        );
        // 1970-01-01 (Thursday)
        assert_eq!(lang_detailed_day_month(0), "Thu, 1 Jan 1970");
        // Leap day 2024-02-29 00:00:00 UTC (Thursday) = 1709164800
        assert_eq!(lang_detailed_day_month(1_709_164_800), "Thu, 29 Feb 2024");
        assert_eq!(
            format_week(1_700_006_400_000.0),
            "15 Nov \u{2014} 22 Nov 2023"
        );
    }
}
