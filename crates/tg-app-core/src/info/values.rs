//! Profile formatting, validation, and Telegram Desktop limits/constants.
//!
//! Port origin: `Telegram/SourceFiles/info/profile/info_profile_values.cpp`,
//! `Telegram/SourceFiles/boxes/peers/edit_peer_common.h`,
//! `Telegram/SourceFiles/boxes/peers/edit_peer_permissions_box.cpp`,
//! `Telegram/SourceFiles/boxes/add_contact_box.cpp`,
//! `Telegram/SourceFiles/boxes/username_box.cpp`,
//! and `Telegram/SourceFiles/menu/menu_ttl.cpp`.

/// Maximum length of a group or channel title (`Ui::EditPeer::kMaxGroupChannelTitle`).
pub const MAX_GROUP_CHANNEL_TITLE: usize = 128;

/// Maximum length of a user's first or last name (`Ui::EditPeer::kMaxUserFirstLastName`).
pub const MAX_USER_FIRST_LAST_NAME: usize = 64;

/// Maximum length of a channel or group description (`Ui::EditPeer::kMaxChannelDescription`).
pub const MAX_CHANNEL_DESCRIPTION: usize = 255;

/// Minimum length of a Telegram username (`Ui::EditPeer::kMinUsernameLength`).
pub const MIN_USERNAME_LENGTH: usize = 5;

/// Maximum length of a Telegram username (`Ui::EditPeer::kMaxUsernameLength`).
pub const MAX_USERNAME_LENGTH: usize = 32;

/// Maximum length of a custom admin rank/title (`kMaxAdminTitleLength`).
pub const MAX_ADMIN_TITLE_LENGTH: usize = 16;

/// Maximum length of a report comment (`kReportReasonLengthMax` in `report_messages_box.cpp`).
pub const MAX_REPORT_COMMENT_LENGTH: usize = 512;

/// Member count threshold after which member search is enabled in profile (`kEnableSearchMembersAfterCount`).
pub const ENABLE_SEARCH_MEMBERS_AFTER_COUNT: usize = 20;

/// Standard Telegram Desktop slow-mode delay steps in seconds (`SlowmodeDelayByIndex`, 8 values).
pub const SLOWMODE_DELAYS_SECONDS: [u32; 8] = [0, 5, 10, 30, 60, 5 * 60, 15 * 60, 60 * 60];

/// Standard Telegram Desktop auto-delete timer values in seconds (`TTLBox` in `menu_ttl.cpp`, plus Off = 0).
pub const AUTO_DELETE_TTL_PRESETS: [u32; 17] = [
    0,
    86_400,
    86_400 * 2,
    86_400 * 3,
    86_400 * 4,
    86_400 * 5,
    86_400 * 6,
    86_400 * 7,
    86_400 * 14,
    86_400 * 21,
    86_400 * 31,
    86_400 * 31 * 2,
    86_400 * 31 * 3,
    86_400 * 31 * 4,
    86_400 * 31 * 5,
    86_400 * 31 * 6,
    86_400 * 365,
];

/// Calendar birthday stored on a user profile (`Data::Birthday`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Birthday {
    /// Day of month (`1..=31`).
    pub day: u8,
    /// Month of year (`1..=12`).
    pub month: u8,
    /// Optional birth year (e.g. `1994`).
    pub year: Option<u16>,
}

/// Validation & availability status for a Telegram username (`UsernameBox` / `EditPeerTypeBox`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsernameAvailability {
    /// Input is empty (clears username on a user profile).
    Empty,
    /// Username is syntactically valid and available.
    Available,
    /// Username is already taken by another peer.
    Occupied,
    /// Username contains characters outside `[A-Za-z0-9_]`.
    BadSymbols,
    /// Username starts with a digit or underscore, or ends with an underscore, or has `__`.
    InvalidFormat,
    /// Username is shorter than [`MIN_USERNAME_LENGTH`] (5).
    TooShort,
    /// Username exceeds [`MAX_USERNAME_LENGTH`] (32).
    TooLong,
    /// Account has reached the maximum number of public channels/groups
    /// (`CHANNELS_ADMIN_PUBLIC_TOO_MUCH`, `edit_peer_type_box.cpp:764`).
    TooManyPublicChannels,
    /// Username is reserved as a Fragment collectible and available for purchase
    /// (`USERNAME_PURCHASE_AVAILABLE`, `edit_peer_type_box.cpp:776`).
    PurchaseAvailable,
    /// Public link cannot be assigned to this channel/group (`CHANNEL_PUBLIC_GROUP_NA`,
    /// `edit_peer_type_box.cpp:761`).
    PublicGroupNotAvailable,
}

impl UsernameAvailability {
    /// Returns `true` if this username can be saved as a public username.
    #[must_use]
    pub const fn is_valid_public(self) -> bool {
        matches!(self, Self::Available)
    }

    /// Returns a human-readable hint or error message matching tdesktop's username checks.
    #[must_use]
    pub fn message(self, username: &str) -> String {
        use tg_lang::tr;
        let clean = username.trim().trim_start_matches('@');
        match self {
            Self::Empty => String::new(),
            Self::Available => format!("@{clean} — {}", tr::lng_username_available()),
            Self::Occupied => tr::lng_username_occupied(),
            Self::BadSymbols => tr::lng_username_bad_symbols(),
            Self::InvalidFormat => tr::lng_username_invalid(),
            Self::TooShort => tr::lng_username_too_short(),
            Self::TooLong => format!("Username must not exceed {MAX_USERNAME_LENGTH} characters."),
            Self::TooManyPublicChannels => {
                "You have created too many public channels or groups. Make one private to free up a link."
                    .to_owned()
            }
            Self::PurchaseAvailable => {
                format!("@{clean} is available for purchase on Fragment.")
            }
            Self::PublicGroupNotAvailable => {
                "Public links are not available for this group or channel.".to_owned()
            }
        }
    }
}

/// Normalizes a raw username by trimming whitespace and a leading `@`.
#[must_use]
pub fn normalize_username(raw: &str) -> &str {
    let trimmed = raw.trim();
    trimmed.strip_prefix('@').unwrap_or(trimmed)
}

/// Validates syntax rules for a Telegram username (`UsernameEditor::changed` in `username_box.cpp`).
#[must_use]
pub fn validate_username_syntax(raw: &str) -> UsernameAvailability {
    let name = normalize_username(raw);
    if name.is_empty() {
        return UsernameAvailability::Empty;
    }
    for ch in name.chars() {
        if !(ch.is_ascii_alphanumeric() || ch == '_') {
            return UsernameAvailability::BadSymbols;
        }
    }
    if name.len() < MIN_USERNAME_LENGTH {
        return UsernameAvailability::TooShort;
    }
    if name.len() > MAX_USERNAME_LENGTH {
        return UsernameAvailability::TooLong;
    }
    let bytes = name.as_bytes();
    if !bytes[0].is_ascii_alphabetic() || bytes[bytes.len() - 1] == b'_' || name.contains("__") {
        return UsernameAvailability::InvalidFormat;
    }
    UsernameAvailability::Available
}

/// Validates a phone number using the exact rule from `IsValidPhone` in `add_contact_box.cpp`.
#[must_use]
pub fn is_valid_phone(raw: &str) -> bool {
    let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
    digits.len() >= 8
        || digits == "333"
        || (digits.starts_with("42")
            && (digits.len() == 2 || digits.len() == 5 || digits.len() == 6 || digits == "4242"))
}

/// Formats a phone string cleanly with leading `+` and readable digit groups.
#[must_use]
pub fn format_phone(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.contains(' ') {
        return if trimmed.starts_with('+') {
            trimmed.to_owned()
        } else {
            format!("+{trimmed}")
        };
    }
    let digits: String = trimmed.chars().filter(char::is_ascii_digit).collect();
    if digits.len() == 10 {
        format!(
            "+{} {} {} {}",
            &digits[0..3],
            &digits[3..6],
            &digits[6..8],
            &digits[8..10]
        )
    } else if digits.len() > 6 {
        format!("+{} {} {}", &digits[0..3], &digits[3..6], &digits[6..])
    } else {
        format!("+{digits}")
    }
}

/// Formats a public `https://t.me/<username>` link.
#[must_use]
pub fn format_username_link(username: &str) -> String {
    format!("https://t.me/{}", normalize_username(username))
}

/// Formats a [`Birthday`] like `info_profile_values.cpp` (`"May 14, 1994 (32 years old)"`).
#[must_use]
pub fn format_birthday(birthday: &Birthday, current_year: Option<u16>) -> String {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let month_name = MONTHS
        .get(usize::from(birthday.month.saturating_sub(1)))
        .copied()
        .unwrap_or("Unknown");
    match birthday.year {
        Some(year) => {
            let date = format!("{month_name} {}, {year}", birthday.day);
            if let Some(now_year) = current_year
                && now_year > year
            {
                let age = i64::from(now_year - year);
                tg_lang::tr::lng_info_birthday_years(age, &date)
            } else {
                date
            }
        }
        None => format!("{month_name} {}", birthday.day),
    }
}

/// Formats a group member count line (`"18 members, 6 online"`).
#[must_use]
pub fn format_members_status(total: u32, online: u32) -> String {
    let base = tg_lang::tr::lng_chat_status_members(i64::from(total));
    if online > 0 && total > 1 {
        let online_str = tg_lang::tr::lng_chat_status_online(i64::from(online));
        tg_lang::tr::lng_chat_status_members_online(&base, &online_str)
    } else {
        base
    }
}

/// Formats a broadcast channel subscriber count line (`"4,280 subscribers"`).
#[must_use]
pub fn format_subscribers_status(total: u32) -> String {
    let raw = tg_lang::tr::lng_chat_status_subscribers(i64::from(total));
    if total >= 1_000 {
        raw.replacen(&total.to_string(), &format_thousands(total), 1)
    } else {
        raw
    }
}

/// Formats an integer with ASCII comma thousands separators.
#[must_use]
pub fn format_thousands(value: u32) -> String {
    let s = value.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (idx, ch) in s.chars().enumerate() {
        let rem = s.len() - idx;
        if idx > 0 && rem.is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Formats media duration in `M:SS` or `H:MM:SS`.
#[must_use]
pub fn format_duration(seconds: u32) -> String {
    let h = seconds / 3600;
    let m = (seconds % 3600) / 60;
    let s = seconds % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Formats a byte size into human-readable units (`B`, `KB`, `MB`, `GB`).
#[must_use]
pub fn format_file_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Maps a slow-mode slider index (`0..8`) to seconds (`SlowmodeDelayByIndex`).
#[must_use]
pub fn slowmode_delay_by_index(index: usize) -> u32 {
    SLOWMODE_DELAYS_SECONDS.get(index).copied().unwrap_or(0)
}

/// Maps slow-mode seconds to the nearest slider index (`0..8`).
#[must_use]
pub fn slowmode_index_for_seconds(seconds: u32) -> usize {
    SLOWMODE_DELAYS_SECONDS
        .iter()
        .position(|&s| s >= seconds)
        .unwrap_or(SLOWMODE_DELAYS_SECONDS.len() - 1)
}

/// Formats a slow-mode delay in seconds for slider ticks and summary rows.
#[must_use]
pub fn format_slowmode(seconds: u32) -> String {
    match seconds {
        0 => tg_lang::tr::lng_rights_slowmode_off(),
        1..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        _ => format!("{}h", seconds / 3600),
    }
}

/// Formats an auto-delete TTL in seconds (`Ui::FormatTTL` in `menu_ttl.cpp`).
#[must_use]
pub fn format_auto_delete_ttl(seconds: u32) -> String {
    use tg_lang::tr;
    const DAY: u32 = 86_400;
    const WEEK: u32 = DAY * 7;
    const MONTH: u32 = DAY * 31;
    const YEAR: u32 = DAY * 365;
    if seconds == 0 {
        return tr::lng_rights_slowmode_off();
    }
    if seconds >= YEAR {
        return tr::lng_years(i64::from(seconds / YEAR));
    }
    if seconds >= MONTH {
        return tr::lng_months(i64::from(seconds / MONTH));
    }
    if seconds >= WEEK && seconds.is_multiple_of(WEEK) {
        return tr::lng_weeks(i64::from(seconds / WEEK));
    }
    tr::lng_days(i64::from((seconds / DAY).max(1)))
}

// ============================================================================
// Fragment Collectible helpers (`ui/boxes/collectible_info_box.cpp`,
// `info/profile/info_profile_phone_menu.cpp`)
// ============================================================================

/// Default anonymous collectible phone number prefixes (`fragment_prefixes` default `["888"]`).
pub const DEFAULT_FRAGMENT_PHONE_PREFIXES: &[&str] = &["888"];

/// Fragment collectible kind matching `Ui::CollectibleType` (`collectible_info_box.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectibleEntityKind {
    /// Anonymous Fragment phone number (e.g. `+888 0123 4567`).
    Phone,
    /// Fragment collectible username (e.g. `@example`).
    Username,
}

/// Classifies a collectible entity string (`Ui::DetectCollectibleType` in `collectible_info_box.cpp`).
#[must_use]
pub fn detect_collectible_type(entity: &str) -> CollectibleEntityKind {
    if entity.trim().starts_with('+') {
        CollectibleEntityKind::Phone
    } else {
        CollectibleEntityKind::Username
    }
}

/// Returns `true` if `phone` matches one of the Fragment anonymous number prefixes
/// (`Info::Profile::IsCollectiblePhone` in `info_profile_phone_menu.cpp:118-128`).
#[must_use]
pub fn is_collectible_phone(phone: &str, prefixes: &[&str]) -> bool {
    let digits: String = phone.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return false;
    }
    let effective = if prefixes.is_empty() {
        DEFAULT_FRAGMENT_PHONE_PREFIXES
    } else {
        prefixes
    };
    effective
        .iter()
        .any(|prefix| !prefix.is_empty() && digits.starts_with(prefix))
}

/// Formats a collectible entity for display (`FormatEntity` in `collectible_info_box.cpp:36-46`).
#[must_use]
pub fn format_collectible_entity(kind: CollectibleEntityKind, entity: &str) -> String {
    match kind {
        CollectibleEntityKind::Phone => {
            let digits: String = entity.chars().filter(char::is_ascii_digit).collect();
            if digits.len() == 11 && digits.starts_with("888") {
                format!("+{} {} {}", &digits[0..3], &digits[3..7], &digits[7..11])
            } else {
                format_phone(&digits)
            }
        }
        CollectibleEntityKind::Username => {
            let clean = normalize_username(entity);
            format!("@{clean}")
        }
    }
}

/// Formats a collectible purchase crypto price (`FormatPrice` in `collectible_info_box.cpp`).
#[must_use]
pub fn format_collectible_crypto_amount(crypto_currency: &str, crypto_amount: u64) -> String {
    if crypto_currency.eq_ignore_ascii_case("TON") {
        let formatted = tg_wallet::format_ton(crypto_amount as i64);
        format!("{formatted} TON")
    } else {
        format!("{crypto_amount} {crypto_currency}")
    }
}

/// Formats a collectible purchase fiat amount in smallest currency units (e.g. cents -> `$125.50`).
#[must_use]
pub fn format_collectible_fiat_amount(currency: &str, amount: u64) -> String {
    let whole = amount / 100;
    let cents = amount % 100;
    let code = currency.to_ascii_uppercase();
    match code.as_str() {
        "USD" => format!("${whole}.{cents:02}"),
        "EUR" => format!("€{whole}.{cents:02}"),
        "GBP" => format!("£{whole}.{cents:02}"),
        _ => format!("{whole}.{cents:02} {code}"),
    }
}

/// Formats a Unix timestamp (seconds) as a readable UTC date (`"Nov 14, 2023"`).
#[must_use]
pub fn format_collectible_date(unix_secs: i32) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = i64::from(unix_secs).div_euclid(86_400);
    // Howard Hinnant's civil-from-days algorithm (Gregorian calendar).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(m <= 2);
    let month_str = MONTHS
        .get((m as usize).saturating_sub(1))
        .copied()
        .unwrap_or("Jan");
    format!("{month_str} {d}, {year}")
}
