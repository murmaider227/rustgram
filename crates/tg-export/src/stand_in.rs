//! Export-local utilities and re-exports of workspace crate primitives.
//!
//! Types that already exist in workspace crates (`tg-data`, `tg-tl`, `tg-lang`, `tg-history`)
//! are re-exported directly from their canonical definitions. Only export-specific / not-yet-ported
//! utilities (`Birthday`, currency rules, filename sanitization, URL/Base64/civil-time helpers)
//! remain implemented here.
//!
//! Ported from `data/data_birthday.h`, `ui/text/format_values.cpp`,
//! `base/base_file_utilities.cpp`, and `base/platform/win/base_file_utilities_win.cpp`.

pub use tg_data::{BareId, ChannelId, ChatId, PeerId, TimeId, UserId};
pub use tg_history::geometry::{Rect, Size};
pub use tg_lang::tag::stand_in::ONE_STAR_IN_NANO;
pub use tg_lang::tag::{CreditsAmount, CreditsType};
pub use tg_tl::enums::{InputFileLocation, InputPeer, InputUser};

/// Low 48-bit mask isolating the bare identifier inside a `PeerId` (`PeerId::kChatTypeMask`).
pub const PEER_ID_CHAT_TYPE_MASK: u64 = PeerId::CHAT_TYPE_MASK;

/// Returns `true` if `amount` is denominated in TON.
#[inline]
pub const fn credits_is_ton(amount: CreditsAmount) -> bool {
    matches!(amount.kind(), CreditsType::Ton)
}

/// Returns `true` if both whole and nano parts of `amount` are zero.
#[inline]
pub const fn credits_is_empty(amount: CreditsAmount) -> bool {
    amount.whole() == 0 && amount.nano() == 0
}

/// User birthday (port of `data/data_birthday.h`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Birthday {
    day: i32,
    month: i32,
    year: i32,
}

impl Birthday {
    /// Minimum valid year in tdesktop.
    pub const YEAR_MIN: i32 = 1875;
    /// Maximum valid year in tdesktop.
    pub const YEAR_MAX: i32 = 2100;

    /// Constructs a birthday (`year == 0` means unspecified).
    pub const fn new(day: i32, month: i32, year: i32) -> Self {
        Self { day, month, year }
    }

    /// Day of month (1..=31).
    pub const fn day(self) -> i32 {
        self.day
    }

    /// Month (1..=12).
    pub const fn month(self) -> i32 {
        self.month
    }

    /// Year (0 if omitted).
    pub const fn year(self) -> i32 {
        self.year
    }

    /// Returns true if day and month are non-zero and in range.
    pub const fn is_valid(self) -> bool {
        self.day >= 1
            && self.day <= 31
            && self.month >= 1
            && self.month <= 12
            && (self.year == 0 || (self.year >= Self::YEAR_MIN && self.year <= Self::YEAR_MAX))
    }
}

/// Formatting rule for a fiat currency (port of `Ui::CurrencyRule` in `ui/text/format_values.h/.cpp`).
#[derive(Debug, Clone, Copy)]
pub struct CurrencyRule {
    /// International symbol (empty means use ISO code).
    pub international: &'static str,
    /// Thousands separator (`'\0'` means none).
    pub thousands: char,
    /// Decimal separator.
    pub decimal: char,
    /// Place currency symbol on the left of the number.
    pub left: bool,
    /// Insert space between currency symbol and number.
    pub space: bool,
    /// Number of decimal digits (usually 2, 0 for JPY/KRW, 3 for BHD/JOD/IQD).
    pub exponent: u32,
    /// Strip trailing `.00` if the amount is an integer.
    pub strip_dot_zero: bool,
}

impl Default for CurrencyRule {
    fn default() -> Self {
        Self {
            international: "",
            thousands: ',',
            decimal: '.',
            left: true,
            space: false,
            exponent: 2,
            strip_dot_zero: false,
        }
    }
}

/// Looks up the currency formatting rule matching `Ui::LookupCurrencyRule`.
pub fn lookup_currency_rule(currency: &str) -> CurrencyRule {
    const fn r(
        international: &'static str,
        thousands: char,
        decimal: char,
        left: bool,
        space: bool,
        exponent: u32,
    ) -> CurrencyRule {
        CurrencyRule {
            international,
            thousands,
            decimal,
            left,
            space,
            exponent,
            strip_dot_zero: false,
        }
    }
    match currency {
        "AED" => r("", ',', '.', true, true, 2),
        "ALL" => r("", '.', ',', false, false, 2),
        "AMD" => r("", ',', '.', false, true, 2),
        "ARS" => r("", '.', ',', true, true, 2),
        "AUD" => r("AU$", ',', '.', true, false, 2),
        "AZN" => r("", ' ', ',', false, true, 2),
        "BAM" => r("", '.', ',', false, true, 2),
        "BDT" => r("", ',', '.', true, true, 2),
        "BGN" => r("", ' ', ',', false, true, 2),
        "BHD" => r("", ',', '.', true, true, 3),
        "BND" => r("", '.', ',', true, false, 2),
        "BOB" => r("", '.', ',', true, true, 2),
        "BRL" => r("R$", '.', ',', true, true, 2),
        "BYN" => r("", ' ', ',', false, true, 2),
        "CAD" => r("CA$", ',', '.', true, false, 2),
        "CHF" => r("", '\'', '.', false, true, 2),
        "CLP" => r("", '.', ',', true, true, 0),
        "CNY" => r("CN\u{00A5}", ',', '.', true, false, 2),
        "COP" => r("", '.', ',', true, true, 2),
        "CRC" => r("", '.', ',', true, false, 2),
        "CZK" => r("", ' ', ',', false, true, 2),
        "DKK" => r("", '\0', ',', false, true, 2),
        "DZD" => r("", ',', '.', true, true, 2),
        "EGP" => r("", ',', '.', true, true, 2),
        "EUR" => r("\u{20AC}", ' ', ',', false, true, 2),
        "GBP" => r("\u{00A3}", ',', '.', true, false, 2),
        "GEL" => r("", ' ', ',', false, true, 2),
        "HKD" => r("HK$", ',', '.', true, false, 2),
        "HNL" => r("", ',', '.', true, true, 2),
        "HRK" => r("", '.', ',', false, true, 2),
        "HUF" => r("", ' ', ',', false, true, 2),
        "IDR" => r("", '.', ',', true, false, 2),
        "ILS" => r("\u{20AA}", ',', '.', true, true, 2),
        "INR" => r("\u{20B9}", ',', '.', true, false, 2),
        "IQD" => r("", ',', '.', true, true, 3),
        "IRR" => r("", ',', '/', false, true, 2),
        "ISK" => r("", '.', ',', false, true, 0),
        "JOD" => r("", ',', '.', true, false, 3),
        "JPY" => r("\u{00A5}", ',', '.', true, false, 0),
        "KGS" => r("", ' ', '-', false, true, 2),
        "KRW" => r("\u{20A9}", ',', '.', true, false, 0),
        "KZT" => r("", ' ', '-', true, false, 2),
        "LBP" => r("", ',', '.', true, true, 2),
        "LKR" => r("", ',', '.', true, true, 2),
        "MAD" => r("", ',', '.', true, true, 2),
        "MDL" => r("", ',', '.', false, true, 2),
        "MNT" => r("", ' ', ',', true, false, 2),
        "MVR" => r("", ',', '.', false, true, 2),
        "MXN" => r("MX$", ',', '.', true, false, 2),
        "NIO" => r("", ',', '.', true, true, 2),
        "NOK" => r("", ' ', ',', true, true, 2),
        "NZD" => r("NZ$", ',', '.', true, false, 2),
        "PAB" => r("", ',', '.', true, true, 2),
        "PEN" => r("", ',', '.', true, true, 2),
        "PLN" => r("", ' ', ',', false, true, 2),
        "PYG" => r("", '.', ',', true, true, 0),
        "QAR" => r("", ',', '.', true, true, 2),
        "RON" => r("", '.', ',', false, true, 2),
        "RSD" => r("", '.', ',', false, true, 2),
        "RUB" => r("", ' ', ',', false, true, 2),
        "SAR" => r("", ',', '.', true, true, 2),
        "SEK" => r("", '.', ',', false, true, 2),
        "SYP" => r("", ',', '.', true, true, 2),
        "THB" => r("\u{0E3F}", ',', '.', true, false, 2),
        "TJS" => r("", ' ', ';', false, true, 2),
        "TRY" => r("", '.', ',', false, true, 2),
        "TWD" => r("NT$", ',', '.', true, false, 2),
        "UAH" => r("", ' ', ',', false, false, 2),
        "UGX" => r("", ',', '.', true, false, 0),
        "USD" => r("$", ',', '.', true, false, 2),
        "UYU" => r("", '.', ',', true, true, 2),
        "UZS" => r("", ' ', ',', false, true, 2),
        "VEF" => r("", '.', ',', true, true, 2),
        "VND" => r("\u{20AB}", '.', ',', false, true, 0),
        "YER" => r("", ',', '.', true, true, 2),
        "ZAR" => r("", ',', '.', true, true, 2),
        _ => CurrencyRule::default(),
    }
}

/// Formats a floating-point amount with custom decimal and thousands separators.
pub fn format_with_separators(
    amount: f64,
    precision: u32,
    decimal: char,
    thousands: char,
) -> String {
    let raw = format!("{amount:.prec$}", prec = precision as usize);
    let (int_part, frac_part) = match raw.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (raw.as_str(), None),
    };
    let mut out = String::with_capacity(raw.len() + int_part.len() / 3);
    if thousands != '\0' {
        let bytes = int_part.as_bytes();
        for (idx, &b) in bytes.iter().enumerate() {
            if idx > 0 && (bytes.len() - idx) % 3 == 0 {
                out.push(thousands);
            }
            out.push(b as char);
        }
    } else {
        out.push_str(int_part);
    }
    if let Some(frac) = frac_part {
        out.push(decimal);
        out.push_str(frac);
    }
    out
}

/// Formats a money amount and currency (port of `Ui::FillAmountAndCurrency`).
pub fn fill_amount_and_currency(amount: i64, currency: &str) -> String {
    if currency == "XTR" {
        return format!("\u{2B50}{}", amount.unsigned_abs());
    }
    let rule = lookup_currency_rule(currency);
    let mut result = String::new();
    if amount < 0 {
        result.push('\u{2212}');
    }
    let abs = amount.unsigned_abs() as f64;
    let value = abs / 10_f64.powi(rule.exponent as i32);
    let name = if !rule.international.is_empty() {
        rule.international
    } else {
        currency
    };
    if rule.left {
        result.push_str(name);
        if rule.space {
            result.push(' ');
        }
    }
    let precision = if !rule.strip_dot_zero || value.floor() != value {
        rule.exponent
    } else {
        0
    };
    result.push_str(&format_with_separators(
        value,
        precision,
        rule.decimal,
        rule.thousands,
    ));
    if !rule.left {
        if rule.space {
            result.push(' ');
        }
        result.push_str(name);
    }
    result
}

/// Formats a duration in seconds as `H:MM:SS` or `MM:SS` (port of `Ui::FormatDurationText`).
pub fn format_duration_text(duration: i64) -> String {
    let hours = duration / 3600;
    let minutes = (duration % 3600) / 60;
    let seconds = duration % 60;
    if hours != 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// Formats image dimensions as `W×H` using U+00D7 (port of `Ui::FormatImageSizeText`).
pub fn format_image_size_text(width: i32, height: i32) -> String {
    format!("{width}\u{00D7}{height}")
}

/// Formats a phone number using `tg_lang::countries` (`Ui::FormatPhone` / `Countries::Instance()`).
pub fn format_phone(phone: &str) -> String {
    tg_lang::countries::format_phone(phone)
}

/// Converts an ISO-3166-1 alpha-2 country code into its Unicode flag emoji via `tg_lang`.
pub fn flag_emoji_by_iso2(iso2: &str) -> String {
    tg_lang::countries::CountriesInstance::flag_emoji_by_iso2(&iso2.to_ascii_uppercase())
}

/// Looks up an English country name for an ISO-3166-1 alpha-2 code via `tg_lang`.
pub fn country_name_by_iso2(iso2: &str) -> String {
    let guard = tg_lang::countries::global_countries()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.country_name_by_iso2(
        &iso2.to_ascii_uppercase(),
        tg_lang::countries::Naming::Default,
    )
}

/// Returns true if `code` is a bidirectional control character.
fn is_bidi_control(code: u32) -> bool {
    code == 0x061C
        || code == 0x200E
        || code == 0x200F
        || (0x202A..=0x202E).contains(&code)
        || (0x2066..=0x2069).contains(&code)
}

/// Sanitizes a user-supplied filename (port of `base::FileNameFromUserString` + Windows rules).
pub fn file_name_from_user_string(name: &str) -> String {
    const BAD_CHARS: [char; 9] = ['/', '\\', '<', '>', ':', '"', '|', '?', '*'];
    let mut sanitized: String = name
        .chars()
        .map(|ch| {
            let code = ch as u32;
            if code < 32 || is_bidi_control(code) || BAD_CHARS.contains(&ch) {
                '_'
            } else {
                ch
            }
        })
        .collect();
    if sanitized.is_empty() || sanitized.ends_with(' ') || sanitized.ends_with('.') {
        sanitized.push('_');
    }
    // Windows-specific safety rules from `base/platform/win/base_file_utilities_win.cpp`.
    let lower = sanitized.to_ascii_lowercase();
    if lower.ends_with(".lnk") || lower.ends_with(".scf") {
        sanitized.push_str(".download");
    }
    const BAD_NAMES: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let upper = sanitized.to_ascii_uppercase();
    for bad in BAD_NAMES {
        if upper.starts_with(bad)
            && (sanitized.len() == bad.len() || sanitized.as_bytes()[bad.len()] == b'.')
        {
            sanitized.insert(0, '_');
            break;
        }
    }
    sanitized
}

/// Scales `size` keeping aspect ratio to fit within `max_width` × `max_height` with
/// Qt `QSize::scaled(..., Qt::KeepAspectRatio)` rounding semantics.
pub fn scale_size_keep_aspect_ratio(size: Size, max_width: i32, max_height: i32) -> Size {
    if size.is_empty() || max_width <= 0 || max_height <= 0 {
        return Size::ZERO;
    }
    let use_height =
        (max_width as i64) * (size.height as i64) >= (max_height as i64) * (size.width as i64);
    if use_height {
        let rw =
            ((max_height as i64 * size.width as i64) as f64 / size.height as f64).round() as i32;
        Size::new(rw, max_height)
    } else {
        let rh =
            ((max_width as i64 * size.height as i64) as f64 / size.width as f64).round() as i32;
        Size::new(max_width, rh)
    }
}

/// Returns the bounding box of two rectangles (matching `QRect::united`).
pub fn rect_united(a: Rect, b: Rect) -> Rect {
    if a.is_empty() {
        return b;
    }
    if b.is_empty() {
        return a;
    }
    let left = a.x.min(b.x);
    let top = a.y.min(b.y);
    let right = a.right().max(b.right());
    let bottom = a.bottom().max(b.bottom());
    Rect::new(left, top, right - left, bottom - top)
}

/// Computes media-group collage tile geometries via `tg_history::grouped::layout_media_group`.
pub fn layout_media_group_geometry(
    sizes: &[Size],
    max_width: i32,
    min_width: i32,
    spacing: i32,
) -> Vec<Rect> {
    tg_history::grouped::layout_media_group(sizes, max_width, min_width, spacing)
        .into_iter()
        .map(|item| item.geometry)
        .collect()
}

/// Encodes bytes as URL-safe Base64 without `=` padding (matching Qt's
/// `QByteArray::Base64UrlEncoding | QByteArray::OmitTrailingEquals`).
pub fn base64_url_no_pad(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((input.len() * 4).div_ceil(3));
    let (chunks, rem) = input.as_chunks::<3>();
    for chunk in chunks {
        let n = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | (chunk[2] as u32);
        out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
        out.push(TABLE[(n & 0x3F) as usize] as char);
    }
    match rem.len() {
        1 => {
            let n = (rem[0] as u32) << 16;
            out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
            out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        }
        2 => {
            let n = ((rem[0] as u32) << 16) | ((rem[1] as u32) << 8);
            out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
            out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
            out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
        }
        _ => {}
    }
    out
}

/// Percent-encodes UTF-8 bytes matching `QUrl::toPercentEncoding` with default unreserved set
/// (`ALPHA / DIGIT / "-" / "." / "_" / "~"`).
pub fn percent_encode_url(input: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(input.len());
    for &b in input {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0F) as usize] as char);
        }
    }
    out
}

/// Converts a Unix timestamp (seconds since 1970-01-01T00:00:00Z) shifted by `utc_offset_secs`
/// into `(year, month (1..=12), day (1..=31), hour, minute, second)`.
pub fn unix_to_civil(timestamp: i64, utc_offset_secs: i32) -> (i32, u32, u32, u32, u32, u32) {
    let shifted = timestamp + utc_offset_secs as i64;
    let days = shifted.div_euclid(86_400);
    let secs_of_day = shifted.rem_euclid(86_400) as u32;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;

    // Howard Hinnant's civil_from_days algorithm.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = (y + if m <= 2 { 1 } else { 0 }) as i32;
    (year, m as u32, d as u32, hour, minute, second)
}

/// Formats a UTC offset in seconds as `UTC+HH:MM` or `UTC-HH:MM` (matching Qt's `OffsetName`).
pub fn format_utc_offset(utc_offset_secs: i32) -> String {
    let sign = if utc_offset_secs < 0 { '-' } else { '+' };
    let abs = utc_offset_secs.unsigned_abs();
    let hours = abs / 3600;
    let minutes = (abs % 3600) / 60;
    format!("UTC{sign}{hours:02}:{minutes:02}")
}
