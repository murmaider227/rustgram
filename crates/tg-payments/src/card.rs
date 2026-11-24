//! Bank card detection, Luhn validation, expiry/CVC/billing validation, and field formatting.
//!
//! Port of `payments/stripe/stripe_card_validator.cpp`, `payments/stripe/stripe_card.cpp`,
//! `payments/ui/payments_edit_card.cpp`, and `payments/ui/payments_field.cpp`.

use std::fmt;

/// Minimum valid CVC digit count (`kMinCvcLength` in `stripe_card_validator.cpp`).
pub const MIN_CVC_LENGTH: usize = 3;

/// Supported bank card brands (`Stripe::CardBrand`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CardBrand {
    /// Visa (4x prefixes).
    Visa,
    /// American Express (34, 37 prefixes; 15 digits, 4-digit CVC).
    Amex,
    /// MasterCard / Maestro (50-59, 22-27, 67 prefixes).
    MasterCard,
    /// Discover (60, 64-65 prefixes).
    Discover,
    /// JCB (35 prefix).
    Jcb,
    /// Diners Club (30, 36, 38-39 prefixes).
    DinersClub,
    /// UnionPay (62, 81 prefixes).
    UnionPay,
    /// Unrecognized or ambiguous prefix.
    #[default]
    Unknown,
}

impl CardBrand {
    /// Display string matching `Stripe::CardBrandToString`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Amex => "American Express",
            Self::DinersClub => "Diners Club",
            Self::Discover => "Discover",
            Self::Jcb => "JCB",
            Self::MasterCard => "MasterCard",
            Self::UnionPay => "UnionPay",
            Self::Unknown => "Unknown",
            Self::Visa => "Visa",
        }
    }

    /// Parses a Stripe brand string (case-insensitive, matching `BrandFromString`).
    pub fn from_stripe_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "visa" => Self::Visa,
            "american express" => Self::Amex,
            "mastercard" => Self::MasterCard,
            "discover" => Self::Discover,
            "jcb" => Self::Jcb,
            "diners club" => Self::DinersClub,
            "unionpay" => Self::UnionPay,
            _ => Self::Unknown,
        }
    }

    /// Maximum CVC length for this brand (`MaxCvcLengthForBranch`).
    pub const fn max_cvc_length(self) -> usize {
        match self {
            Self::Amex | Self::Unknown => 4,
            _ => 3,
        }
    }
}

impl fmt::Display for CardBrand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Card funding category (`Stripe::CardFundingType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CardFundingType {
    /// Debit card.
    Debit,
    /// Credit card.
    Credit,
    /// Prepaid card.
    Prepaid,
    /// Other or unspecified funding type.
    #[default]
    Other,
}

impl CardFundingType {
    /// Parses a Stripe funding string (case-insensitive, matching `FundingFromString`).
    pub fn from_stripe_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "credit" => Self::Credit,
            "debit" => Self::Debit,
            "prepaid" => Self::Prepaid,
            _ => Self::Other,
        }
    }
}

/// Validation status for incremental card field checks (`Stripe::ValidationState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValidationState {
    /// Input contains invalid characters, fails Luhn, or is out of range.
    Invalid,
    /// Input is a valid prefix of a complete value, but too short yet.
    Incomplete,
    /// Input is valid.
    Valid,
}

/// Result of validating a card number (`Stripe::CardValidationResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardValidationResult {
    /// Current validation state.
    pub state: ValidationState,
    /// Detected card brand from the most specific matching BIN range.
    pub brand: CardBrand,
    /// `true` when the number has reached the maximum valid length and passed Luhn.
    pub finished: bool,
}

/// Result of validating an expiration date (`Stripe::ExpireDateValidationResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpireDateValidationResult {
    /// Current validation state.
    pub state: ValidationState,
    /// `true` when the date is complete and valid.
    pub finished: bool,
}

/// Result of validating a CVC code (`Stripe::CvcValidationResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CvcValidationResult {
    /// Current validation state.
    pub state: ValidationState,
    /// `true` when the CVC has reached the maximum length for the card brand.
    pub finished: bool,
}

/// Simple `(year, month)` date pair used for expiration thresholds without pulling a date crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct YearMonth {
    /// Four-digit year (e.g., `2026`).
    pub year: u32,
    /// Month in `1..=12`.
    pub month: u32,
}

impl YearMonth {
    /// Creates a new `YearMonth`.
    pub const fn new(year: u32, month: u32) -> Self {
        Self { year, month }
    }
}

/// Native card form fields (`Payments::Ui::CardField`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CardField {
    /// Primary account number (PAN).
    Number,
    /// Security code (CVC/CVV).
    Cvc,
    /// Expiration date (`MM/YY`).
    ExpireDate,
    /// Cardholder name.
    Name,
    /// Billing address ISO2 country code.
    AddressCountry,
    /// Billing address postal/ZIP code.
    AddressZip,
}

/// Native payment method field requirements (`Payments::Ui::NativeMethodDetails`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NativeMethodDetails {
    /// Default ISO2 country code suggested for billing address.
    pub default_country: String,
    /// Whether native card entry is supported for this form.
    pub supported: bool,
    /// Whether billing country is required.
    pub need_country: bool,
    /// Whether billing postal/ZIP code is required.
    pub need_zip: bool,
    /// Whether cardholder name is required.
    pub need_cardholder_name: bool,
    /// Whether credentials may be saved (or password can be set to enable saving).
    pub can_save_information: bool,
}

/// Unchecked native card form input (`Payments::Ui::UncheckedCardDetails`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UncheckedCardDetails {
    /// Card number (digits, optionally with spaces).
    pub number: String,
    /// Card security code (digits).
    pub cvc: String,
    /// Four-digit expiration year (e.g., `2028`).
    pub expire_year: u32,
    /// Expiration month (`1..=12`).
    pub expire_month: u32,
    /// Cardholder name.
    pub cardholder_name: String,
    /// Billing address ISO2 country code.
    pub address_country: String,
    /// Billing address postal/ZIP code.
    pub address_zip: String,
}

/// BIN range table entry matching `AllRanges()` in `stripe_card_validator.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BinRange {
    low: &'static str,
    high: &'static str,
    length: usize,
    brand: CardBrand,
}

/// Exact BIN range table ported from `AllRanges()` in `payments/stripe/stripe_card_validator.cpp`.
const ALL_BIN_RANGES: &[BinRange] = &[
    // Unknown
    BinRange {
        low: "",
        high: "",
        length: 19,
        brand: CardBrand::Unknown,
    },
    // American Express
    BinRange {
        low: "34",
        high: "34",
        length: 15,
        brand: CardBrand::Amex,
    },
    BinRange {
        low: "37",
        high: "37",
        length: 15,
        brand: CardBrand::Amex,
    },
    // Diners Club
    BinRange {
        low: "30",
        high: "30",
        length: 16,
        brand: CardBrand::DinersClub,
    },
    BinRange {
        low: "36",
        high: "36",
        length: 14,
        brand: CardBrand::DinersClub,
    },
    BinRange {
        low: "38",
        high: "39",
        length: 16,
        brand: CardBrand::DinersClub,
    },
    // Discover
    BinRange {
        low: "60",
        high: "60",
        length: 16,
        brand: CardBrand::Discover,
    },
    BinRange {
        low: "64",
        high: "65",
        length: 16,
        brand: CardBrand::Discover,
    },
    // JCB
    BinRange {
        low: "35",
        high: "35",
        length: 16,
        brand: CardBrand::Jcb,
    },
    // Mastercard
    BinRange {
        low: "50",
        high: "59",
        length: 16,
        brand: CardBrand::MasterCard,
    },
    BinRange {
        low: "22",
        high: "27",
        length: 16,
        brand: CardBrand::MasterCard,
    },
    BinRange {
        low: "67",
        high: "67",
        length: 16,
        brand: CardBrand::MasterCard,
    }, // Maestro
    // UnionPay
    BinRange {
        low: "62",
        high: "62",
        length: 16,
        brand: CardBrand::UnionPay,
    },
    BinRange {
        low: "81",
        high: "81",
        length: 16,
        brand: CardBrand::UnionPay,
    },
    // Visa
    BinRange {
        low: "40",
        high: "49",
        length: 16,
        brand: CardBrand::Visa,
    },
    BinRange {
        low: "413600",
        high: "413600",
        length: 13,
        brand: CardBrand::Visa,
    },
    BinRange {
        low: "444509",
        high: "444509",
        length: 13,
        brand: CardBrand::Visa,
    },
    BinRange {
        low: "444550",
        high: "444550",
        length: 13,
        brand: CardBrand::Visa,
    },
    BinRange {
        low: "450603",
        high: "450603",
        length: 13,
        brand: CardBrand::Visa,
    },
    BinRange {
        low: "450617",
        high: "450617",
        length: 13,
        brand: CardBrand::Visa,
    },
    BinRange {
        low: "450628",
        high: "450629",
        length: 13,
        brand: CardBrand::Visa,
    },
    BinRange {
        low: "450636",
        high: "450636",
        length: 13,
        brand: CardBrand::Visa,
    },
    BinRange {
        low: "450640",
        high: "450641",
        length: 13,
        brand: CardBrand::Visa,
    },
    BinRange {
        low: "450662",
        high: "450662",
        length: 13,
        brand: CardBrand::Visa,
    },
    BinRange {
        low: "463100",
        high: "463100",
        length: 13,
        brand: CardBrand::Visa,
    },
    BinRange {
        low: "476142",
        high: "476142",
        length: 13,
        brand: CardBrand::Visa,
    },
    BinRange {
        low: "476143",
        high: "476143",
        length: 13,
        brand: CardBrand::Visa,
    },
    BinRange {
        low: "492901",
        high: "492902",
        length: 13,
        brand: CardBrand::Visa,
    },
    BinRange {
        low: "492920",
        high: "492920",
        length: 13,
        brand: CardBrand::Visa,
    },
    BinRange {
        low: "492923",
        high: "492923",
        length: 13,
        brand: CardBrand::Visa,
    },
    BinRange {
        low: "492928",
        high: "492930",
        length: 13,
        brand: CardBrand::Visa,
    },
    BinRange {
        low: "492937",
        high: "492937",
        length: 13,
        brand: CardBrand::Visa,
    },
    BinRange {
        low: "492939",
        high: "492939",
        length: 13,
        brand: CardBrand::Visa,
    },
    BinRange {
        low: "492960",
        high: "492960",
        length: 13,
        brand: CardBrand::Visa,
    },
];

const MIN_CARD_LENGTH: usize = 13;
const MAX_CARD_LENGTH: usize = 19;

/// Removes all ASCII/Unicode whitespace from `value`.
pub fn remove_whitespaces(value: &str) -> String {
    value.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Removes all non-ASCII-digit characters from `value`.
pub fn remove_non_digits(value: &str) -> String {
    value.chars().filter(|c| c.is_ascii_digit()).collect()
}

fn is_numeric(value: &str) -> bool {
    value.bytes().all(|b| b.is_ascii_digit())
}

fn parse_prefix_int(s: &str) -> i32 {
    if s.is_empty() {
        0
    } else {
        s.parse::<i32>().unwrap_or(0)
    }
}

fn bin_range_matches_number(range: &BinRange, sanitized: &str) -> bool {
    let min_low = sanitized.len().min(range.low.len());
    if parse_prefix_int(&sanitized[..min_low]) < parse_prefix_int(&range.low[..min_low]) {
        return false;
    }
    let min_high = sanitized.len().min(range.high.len());
    parse_prefix_int(&sanitized[..min_high]) <= parse_prefix_int(&range.high[..min_high])
}

fn bin_ranges_for_number(sanitized: &str) -> Vec<BinRange> {
    ALL_BIN_RANGES
        .iter()
        .copied()
        .filter(|range| bin_range_matches_number(range, sanitized))
        .collect()
}

fn most_specific_bin_range(sanitized: &str) -> BinRange {
    let mut possible = bin_ranges_for_number(sanitized);
    possible.sort_by(|a, b| {
        if sanitized.is_empty() {
            let a_unknown = a.brand == CardBrand::Unknown;
            let b_unknown = b.brand == CardBrand::Unknown;
            if a_unknown && !b_unknown {
                return std::cmp::Ordering::Less;
            } else if !a_unknown && b_unknown {
                return std::cmp::Ordering::Greater;
            }
        }
        a.low.len().cmp(&b.low.len())
    });
    possible.last().copied().unwrap_or(ALL_BIN_RANGES[0])
}

/// Returns all non-`Unknown` card brands whose BIN ranges match `number`.
pub fn possible_brands_for_number(number: &str) -> Vec<CardBrand> {
    let sanitized = remove_whitespaces(number);
    if !is_numeric(&sanitized) {
        return Vec::new();
    }
    let mut result = Vec::new();
    for range in bin_ranges_for_number(&sanitized) {
        if range.brand != CardBrand::Unknown && !result.contains(&range.brand) {
            result.push(range.brand);
        }
    }
    result
}

/// Detects the unique bank card brand for `number` (`BrandForNumber` in `stripe_card_validator.cpp`).
///
/// Returns [`CardBrand::Unknown`] if the input is non-numeric or matches zero or multiple brands.
pub fn detect_card_brand(number: &str) -> CardBrand {
    let sanitized = remove_whitespaces(number);
    if !is_numeric(&sanitized) {
        return CardBrand::Unknown;
    }
    let possible = possible_brands_for_number(&sanitized);
    if possible.len() == 1 {
        possible[0]
    } else {
        CardBrand::Unknown
    }
}

/// Checks whether a numeric card number satisfies the Luhn checksum (`IsValidLuhn`).
///
/// Expects digits only (whitespace is stripped before checking; non-digits return `false`).
pub fn is_valid_luhn(number: &str) -> bool {
    let sanitized = remove_whitespaces(number);
    if sanitized.is_empty() || !is_numeric(&sanitized) {
        return false;
    }
    let mut odd = true;
    let mut sum: u32 = 0;
    for b in sanitized.bytes().rev() {
        let mut digit = u32::from(b - b'0');
        odd = !odd;
        if odd {
            digit *= 2;
        }
        if digit > 9 {
            digit -= 9;
        }
        sum += digit;
    }
    sum.is_multiple_of(10)
}

/// Validates a card number string (`Stripe::ValidateCard`).
pub fn validate_card(number: &str) -> CardValidationResult {
    let sanitized = remove_whitespaces(number);
    if !is_numeric(&sanitized) {
        return CardValidationResult {
            state: ValidationState::Invalid,
            brand: CardBrand::Unknown,
            finished: false,
        };
    }
    if sanitized.is_empty() {
        return CardValidationResult {
            state: ValidationState::Incomplete,
            brand: CardBrand::Unknown,
            finished: false,
        };
    }
    let range = most_specific_bin_range(&sanitized);
    let brand = range.brand;

    if sanitized.len() > MAX_CARD_LENGTH {
        CardValidationResult {
            state: ValidationState::Invalid,
            brand,
            finished: false,
        }
    } else if sanitized.len() < MIN_CARD_LENGTH {
        CardValidationResult {
            state: ValidationState::Incomplete,
            brand,
            finished: false,
        }
    } else if !is_valid_luhn(&sanitized) {
        CardValidationResult {
            state: ValidationState::Invalid,
            brand,
            finished: false,
        }
    } else if sanitized.len() < MAX_CARD_LENGTH {
        CardValidationResult {
            state: ValidationState::Valid,
            brand,
            finished: false,
        }
    } else {
        CardValidationResult {
            state: ValidationState::Valid,
            brand,
            finished: true,
        }
    }
}

/// Computes the Russian-phone expiration threshold override from `Form::overrideExpireDateThreshold`.
///
/// Telegram Desktop overrides the expiration threshold to `2022-02` for phone numbers starting
/// with `'7'` (due to extended Russian bank card expiration dates).
pub fn override_expire_date_threshold(user_phone: &str) -> Option<YearMonth> {
    let trimmed = user_phone.trim_start_matches('+');
    if trimmed.starts_with('7') {
        Some(YearMonth::new(2022, 2))
    } else {
        None
    }
}

/// Validates an expiration date string (`MM/YY` or `MMYY`) against `threshold`
/// (`Stripe::ValidateExpireDate`).
pub fn validate_expire_date(date: &str, threshold: YearMonth) -> ExpireDateValidationResult {
    let sanitized: String = remove_whitespaces(date).replace('/', "");
    if !is_numeric(&sanitized) {
        return ExpireDateValidationResult {
            state: ValidationState::Invalid,
            finished: false,
        };
    }
    if sanitized.len() < 2 {
        return ExpireDateValidationResult {
            state: ValidationState::Incomplete,
            finished: false,
        };
    }
    let normalized = if sanitized.as_bytes()[0] > b'1' {
        format!("0{sanitized}")
    } else {
        sanitized
    };
    let month: u32 = match normalized[..2].parse() {
        Ok(m) => m,
        Err(_) => {
            return ExpireDateValidationResult {
                state: ValidationState::Invalid,
                finished: false,
            };
        }
    };
    if !(1..=12).contains(&month) {
        return ExpireDateValidationResult {
            state: ValidationState::Invalid,
            finished: false,
        };
    }
    if normalized.len() < 4 {
        return ExpireDateValidationResult {
            state: ValidationState::Incomplete,
            finished: false,
        };
    }
    if normalized.len() > 4 {
        return ExpireDateValidationResult {
            state: ValidationState::Invalid,
            finished: false,
        };
    }
    let yy: u32 = match normalized[2..4].parse() {
        Ok(y) => y,
        Err(_) => {
            return ExpireDateValidationResult {
                state: ValidationState::Invalid,
                finished: false,
            };
        }
    };
    let year = 2000 + yy;
    if year < threshold.year || (year == threshold.year && month < threshold.month) {
        return ExpireDateValidationResult {
            state: ValidationState::Invalid,
            finished: false,
        };
    }
    ExpireDateValidationResult {
        state: ValidationState::Valid,
        finished: true,
    }
}

/// Validates a parsed `(month, year)` pair (`Stripe::ValidateParsedExpireDate`).
pub fn validate_parsed_expire_date(month: u32, year: u32, threshold: YearMonth) -> ValidationState {
    if (year / 100) != 20 {
        return ValidationState::Invalid;
    }
    let date = format!("{month:02}{:02}", year % 100);
    validate_expire_date(&date, threshold).state
}

/// Validates a CVC security code for the given card number (`Stripe::ValidateCvc`).
pub fn validate_cvc(card_number: &str, cvc: &str) -> CvcValidationResult {
    if !is_numeric(cvc) {
        return CvcValidationResult {
            state: ValidationState::Invalid,
            finished: false,
        };
    }
    if cvc.len() < MIN_CVC_LENGTH {
        return CvcValidationResult {
            state: ValidationState::Incomplete,
            finished: false,
        };
    }
    let max_len = detect_card_brand(card_number).max_cvc_length();
    if cvc.len() > max_len {
        return CvcValidationResult {
            state: ValidationState::Invalid,
            finished: false,
        };
    }
    CvcValidationResult {
        state: ValidationState::Valid,
        finished: cvc.len() == max_len,
    }
}

/// Returns the visual digit-grouping pattern for a card number (`Stripe::CardNumberFormat`).
///
/// - Diners Club (14-digit): `[4, 6, 4]`
/// - American Express: `[4, 6, 5]`
/// - Default: `[4, 4, 4, 4]`
pub fn card_number_format(number: &str) -> Vec<usize> {
    let sanitized = remove_whitespaces(number);
    if !is_numeric(&sanitized) {
        return vec![4, 4, 4, 4];
    }
    let range = most_specific_bin_range(&sanitized);
    if range.brand == CardBrand::DinersClub && range.length == 14 {
        vec![4, 6, 4]
    } else if range.brand == CardBrand::Amex {
        vec![4, 6, 5]
    } else {
        vec![4, 4, 4, 4]
    }
}

/// Formats a raw card number string into spaced groups according to [`card_number_format`]
/// (port of `PostprocessCardValidateResult` in `payments_edit_card.cpp`).
pub fn format_card_number(number: &str) -> String {
    let digits = remove_non_digits(number);
    let groups = card_number_format(&digits);
    let mut out = String::with_capacity(digits.len() + groups.len());
    let mut idx = 0;
    for &len in &groups {
        if idx >= digits.len() {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        let end = (idx + len).min(digits.len());
        out.push_str(&digits[idx..end]);
        idx = end;
    }
    if idx < digits.len() {
        out.push_str(&digits[idx..]);
    }
    out
}

/// Formats an incremental expiry input into `MM/YY` form
/// (port of `PostprocessExpireDateValidateResult` in `payments_edit_card.cpp`).
pub fn format_expire_date_input(raw: &str) -> String {
    let mut value = remove_non_digits(raw);
    if value.is_empty() {
        return value;
    }
    let bytes = value.as_bytes();
    if bytes[0] == b'1' && value.len() > 1 && bytes[1] > b'2' {
        value.truncate(2);
        return value;
    } else if bytes[0] > b'1' {
        value.insert(0, '0');
    }
    if value.len() > 1 {
        if value.len() > 4 {
            value.truncate(4);
        }
        value.insert(2, '/');
    }
    value
}

/// Normalizes a cardholder name input to uppercase (`CardHolderNameValidator`).
pub fn format_cardholder_name(raw: &str) -> String {
    raw.to_uppercase()
}

/// Parses an `MM/YY` string into `(month, full_year)` (`ExtractMonth`/`ExtractYear`).
pub fn parse_expire_date_mm_yy(formatted: &str) -> Option<(u32, u32)> {
    let mut parts = formatted.split('/');
    let month: u32 = parts.next()?.parse().ok()?;
    let yy: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((month, 2000 + yy))
}

/// Validates all fields of [`UncheckedCardDetails`] against [`NativeMethodDetails`]
/// (port of `Form::cardErrorLocal` in `payments_form.cpp`).
///
/// Returns the first Telegram local error code (`LOCAL_CARD_*_INVALID`) if any field is invalid.
pub fn validate_card_details(
    details: &UncheckedCardDetails,
    native: &NativeMethodDetails,
    threshold: YearMonth,
) -> Result<(), &'static str> {
    if validate_card(&details.number).state != ValidationState::Valid {
        return Err("LOCAL_CARD_NUMBER_INVALID");
    }
    if validate_parsed_expire_date(details.expire_month, details.expire_year, threshold)
        != ValidationState::Valid
    {
        return Err("LOCAL_CARD_EXPIRE_DATE_INVALID");
    }
    if validate_cvc(&details.number, &details.cvc).state != ValidationState::Valid {
        return Err("LOCAL_CARD_CVC_INVALID");
    }
    if native.need_cardholder_name && details.cardholder_name.trim().is_empty() {
        return Err("LOCAL_CARD_HOLDER_NAME_INVALID");
    }
    if native.need_country && !is_valid_country_iso2(&details.address_country) {
        return Err("LOCAL_CARD_BILLING_COUNTRY_INVALID");
    }
    if native.need_zip && details.address_zip.trim().is_empty() {
        return Err("LOCAL_CARD_BILLING_ZIP_INVALID");
    }
    Ok(())
}

/// Checks whether `iso2` is a syntactically valid 2-letter ISO-3166-1 alpha-2 country code.
pub fn is_valid_country_iso2(iso2: &str) -> bool {
    let bytes = iso2.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1].is_ascii_alphabetic()
}

/// Maps a local card validation error code (`LOCAL_CARD_*_INVALID`) to its [`CardField`].
pub fn card_field_for_local_error(code: &str) -> Option<CardField> {
    match code {
        "LOCAL_CARD_NUMBER_INVALID" => Some(CardField::Number),
        "LOCAL_CARD_EXPIRE_DATE_INVALID" => Some(CardField::ExpireDate),
        "LOCAL_CARD_CVC_INVALID" => Some(CardField::Cvc),
        "LOCAL_CARD_HOLDER_NAME_INVALID" => Some(CardField::Name),
        "LOCAL_CARD_BILLING_COUNTRY_INVALID" => Some(CardField::AddressCountry),
        "LOCAL_CARD_BILLING_ZIP_INVALID" => Some(CardField::AddressZip),
        _ => None,
    }
}

/// Maps a Stripe error description (`InvalidNumber`, `ExpiredCard`, etc.) to its [`CardField`].
pub fn card_field_for_stripe_error(description: &str) -> Option<CardField> {
    match description {
        "InvalidNumber" | "IncorrectNumber" => Some(CardField::Number),
        "InvalidCVC" | "IncorrectCVC" => Some(CardField::Cvc),
        "InvalidExpiryMonth" | "InvalidExpiryYear" | "ExpiredCard" => Some(CardField::ExpireDate),
        _ => None,
    }
}
