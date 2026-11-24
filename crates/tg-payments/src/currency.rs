//! Currency formatting rules and money input field validation.
//!
//! Port of `ui/text/format_values.cpp` (`CurrencyRule`, `LookupCurrencyRule`,
//! `FillAmountAndCurrency`, `FormatWithSeparators`) and `payments/ui/payments_field.cpp`
//! (`CleanMoneyState`, `PostprocessMoneyResult`, `Parse`/`Format` for `FieldType::Money`).

/// Telegram Stars ISO-like currency code (`Ui::kCreditsCurrency`).
pub const CREDITS_CURRENCY: &str = "XTR";

/// Currency display and formatting rule (`Ui::CurrencyRule`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrencyRule {
    /// International symbol (e.g. `"$"`, `"€"`, `"£"`), or `""` to use the ISO code.
    pub international: &'static str,
    /// Thousands grouping separator (or `'\0'` for none).
    pub thousands: char,
    /// Decimal point separator.
    pub decimal: char,
    /// Whether the currency symbol/code appears on the left of the number.
    pub left: bool,
    /// Whether a space separates the currency symbol/code from the number.
    pub space: bool,
    /// Number of fractional decimal digits (e.g., `2` for USD, `0` for JPY, `3` for BHD).
    pub exponent: u32,
    /// Whether `.00` fractional zeros are stripped when formatting whole amounts.
    pub strip_dot_zero: bool,
}

impl Default for CurrencyRule {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl CurrencyRule {
    /// Default currency rule matching C++ `CurrencyRule{}` in `format_values.h`.
    pub const DEFAULT: Self = Self {
        international: "",
        thousands: ',',
        decimal: '.',
        left: true,
        space: false,
        exponent: 2,
        strip_dot_zero: false,
    };

    const fn sym(international: &'static str) -> Self {
        Self {
            international,
            ..Self::DEFAULT
        }
    }

    const fn full(
        international: &'static str,
        thousands: char,
        decimal: char,
        left: bool,
        space: bool,
        exponent: u32,
    ) -> Self {
        Self {
            international,
            thousands,
            decimal,
            left,
            space,
            exponent,
            strip_dot_zero: false,
        }
    }

    /// Thousands separator used inside editable money input fields (`FieldThousandsSeparator`).
    pub const fn field_thousands_separator(self) -> char {
        if self.thousands == '.' || self.thousands == ',' {
            ' '
        } else {
            self.thousands
        }
    }
}

/// Built-in currency table matching `CurrencyRulesMap()` in `ui/text/format_values.cpp`.
const CURRENCY_RULES: &[(&str, CurrencyRule)] = &[
    ("AED", CurrencyRule::full("", ',', '.', true, true, 2)),
    ("AFN", CurrencyRule::DEFAULT),
    ("ALL", CurrencyRule::full("", '.', ',', false, false, 2)),
    ("AMD", CurrencyRule::full("", ',', '.', false, true, 2)),
    ("ARS", CurrencyRule::full("", '.', ',', true, true, 2)),
    ("AUD", CurrencyRule::sym("AU$")),
    ("AZN", CurrencyRule::full("", ' ', ',', false, true, 2)),
    ("BAM", CurrencyRule::full("", '.', ',', false, true, 2)),
    ("BDT", CurrencyRule::full("", ',', '.', true, true, 2)),
    ("BGN", CurrencyRule::full("", ' ', ',', false, true, 2)),
    ("BHD", CurrencyRule::full("", ',', '.', true, true, 3)),
    ("BND", CurrencyRule::full("", '.', ',', true, false, 2)),
    ("BOB", CurrencyRule::full("", '.', ',', true, true, 2)),
    ("BRL", CurrencyRule::full("R$", '.', ',', true, true, 2)),
    ("BYN", CurrencyRule::full("", ' ', ',', false, true, 2)),
    ("CAD", CurrencyRule::sym("CA$")),
    ("CHF", CurrencyRule::full("", '\'', '.', false, true, 2)),
    ("CLP", CurrencyRule::full("", '.', ',', true, true, 0)),
    ("CNY", CurrencyRule::sym("CN\u{00A5}")),
    ("COP", CurrencyRule::full("", '.', ',', true, true, 2)),
    ("CRC", CurrencyRule::full("", '.', ',', true, false, 2)),
    ("CZK", CurrencyRule::full("", ' ', ',', false, true, 2)),
    ("DKK", CurrencyRule::full("", '\0', ',', false, true, 2)),
    ("DOP", CurrencyRule::DEFAULT),
    ("DZD", CurrencyRule::full("", ',', '.', true, true, 2)),
    ("EGP", CurrencyRule::full("", ',', '.', true, true, 2)),
    ("ETB", CurrencyRule::DEFAULT),
    (
        "EUR",
        CurrencyRule::full("\u{20AC}", ' ', ',', false, true, 2),
    ),
    ("GBP", CurrencyRule::sym("\u{00A3}")),
    ("GEL", CurrencyRule::full("", ' ', ',', false, true, 2)),
    ("GHS", CurrencyRule::DEFAULT),
    ("GTQ", CurrencyRule::DEFAULT),
    ("HKD", CurrencyRule::sym("HK$")),
    ("HNL", CurrencyRule::full("", ',', '.', true, true, 2)),
    ("HRK", CurrencyRule::full("", '.', ',', false, true, 2)),
    ("HUF", CurrencyRule::full("", ' ', ',', false, true, 2)),
    ("IDR", CurrencyRule::full("", '.', ',', true, false, 2)),
    (
        "ILS",
        CurrencyRule::full("\u{20AA}", ',', '.', true, true, 2),
    ),
    ("INR", CurrencyRule::sym("\u{20B9}")),
    ("IQD", CurrencyRule::full("", ',', '.', true, true, 3)),
    ("IRR", CurrencyRule::full("", ',', '/', false, true, 2)),
    ("ISK", CurrencyRule::full("", '.', ',', false, true, 0)),
    ("JMD", CurrencyRule::DEFAULT),
    ("JOD", CurrencyRule::full("", ',', '.', true, false, 3)),
    (
        "JPY",
        CurrencyRule::full("\u{00A5}", ',', '.', true, false, 0),
    ),
    ("KES", CurrencyRule::DEFAULT),
    ("KGS", CurrencyRule::full("", ' ', '-', false, true, 2)),
    (
        "KRW",
        CurrencyRule::full("\u{20A9}", ',', '.', true, false, 0),
    ),
    ("KZT", CurrencyRule::full("", ' ', '-', true, false, 2)),
    ("LBP", CurrencyRule::full("", ',', '.', true, true, 2)),
    ("LKR", CurrencyRule::full("", ',', '.', true, true, 2)),
    ("MAD", CurrencyRule::full("", ',', '.', true, true, 2)),
    ("MDL", CurrencyRule::full("", ',', '.', false, true, 2)),
    ("MMK", CurrencyRule::DEFAULT),
    ("MNT", CurrencyRule::full("", ' ', ',', true, false, 2)),
    ("MOP", CurrencyRule::DEFAULT),
    ("MUR", CurrencyRule::DEFAULT),
    ("MVR", CurrencyRule::full("", ',', '.', false, true, 2)),
    ("MXN", CurrencyRule::sym("MX$")),
    ("MYR", CurrencyRule::DEFAULT),
    ("MZN", CurrencyRule::DEFAULT),
    ("NGN", CurrencyRule::DEFAULT),
    ("NIO", CurrencyRule::full("", ',', '.', true, true, 2)),
    ("NOK", CurrencyRule::full("", ' ', ',', true, true, 2)),
    ("NPR", CurrencyRule::DEFAULT),
    ("NZD", CurrencyRule::sym("NZ$")),
    ("PAB", CurrencyRule::full("", ',', '.', true, true, 2)),
    ("PEN", CurrencyRule::full("", ',', '.', true, true, 2)),
    ("PHP", CurrencyRule::DEFAULT),
    ("PKR", CurrencyRule::DEFAULT),
    ("PLN", CurrencyRule::full("", ' ', ',', false, true, 2)),
    ("PYG", CurrencyRule::full("", '.', ',', true, true, 0)),
    ("QAR", CurrencyRule::full("", ',', '.', true, true, 2)),
    ("RON", CurrencyRule::full("", '.', ',', false, true, 2)),
    ("RSD", CurrencyRule::full("", '.', ',', false, true, 2)),
    ("RUB", CurrencyRule::full("", ' ', ',', false, true, 2)),
    ("SAR", CurrencyRule::full("", ',', '.', true, true, 2)),
    ("SEK", CurrencyRule::full("", '.', ',', false, true, 2)),
    ("SGD", CurrencyRule::DEFAULT),
    ("SYP", CurrencyRule::full("", ',', '.', true, true, 2)),
    ("THB", CurrencyRule::sym("\u{0E3F}")),
    ("TJS", CurrencyRule::full("", ' ', ';', false, true, 2)),
    ("TRY", CurrencyRule::full("", '.', ',', false, true, 2)),
    ("TTD", CurrencyRule::DEFAULT),
    ("TWD", CurrencyRule::sym("NT$")),
    ("TZS", CurrencyRule::DEFAULT),
    ("UAH", CurrencyRule::full("", ' ', ',', false, false, 2)),
    ("UGX", CurrencyRule::full("", ',', '.', true, false, 0)),
    ("USD", CurrencyRule::sym("$")),
    ("UYU", CurrencyRule::full("", '.', ',', true, true, 2)),
    ("UZS", CurrencyRule::full("", ' ', ',', false, true, 2)),
    ("VEF", CurrencyRule::full("", '.', ',', true, true, 2)),
    (
        "VND",
        CurrencyRule::full("\u{20AB}", '.', ',', false, true, 0),
    ),
    ("YER", CurrencyRule::full("", ',', '.', true, true, 2)),
    ("ZAR", CurrencyRule::full("", ',', '.', true, true, 2)),
];

/// Looks up the [`CurrencyRule`] for an ISO-4217 currency code (`Ui::LookupCurrencyRule`).
pub fn lookup_currency_rule(currency: &str) -> CurrencyRule {
    CURRENCY_RULES
        .binary_search_by_key(&currency, |&(code, _)| code)
        .map(|idx| CURRENCY_RULES[idx].1)
        .unwrap_or_default()
}

/// Returns `true` if `currency` is in Telegram's built-in currency table (`Ui::KnownCurrency`).
pub fn known_currency(currency: &str) -> bool {
    CURRENCY_RULES
        .binary_search_by_key(&currency, |&(code, _)| code)
        .is_ok()
}

/// Returns the display symbol or code for `currency` (`Ui::CurrencyName`).
pub fn currency_name(currency: &str) -> &str {
    let rule = lookup_currency_rule(currency);
    if !rule.international.is_empty() {
        rule.international
    } else {
        currency
    }
}

/// Formats a non-negative floating-point number with custom decimal and thousands separators
/// (`Ui::FormatWithSeparators`).
pub fn format_with_separators(
    amount: f64,
    precision: u32,
    decimal: char,
    thousands: char,
) -> String {
    let formatted = format!("{amount:.prec$}", prec = precision as usize);
    let (int_part, frac_part) = match formatted.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (formatted.as_str(), None),
    };
    let mut out = String::with_capacity(formatted.len() + int_part.len() / 3);
    if thousands == '\0' {
        out.push_str(int_part);
    } else {
        let len = int_part.len();
        for (idx, ch) in int_part.chars().enumerate() {
            if idx > 0 && (len - idx).is_multiple_of(3) {
                out.push(thousands);
            }
            out.push(ch);
        }
    }
    if let Some(frac) = frac_part
        && !frac.is_empty()
    {
        out.push(decimal);
        out.push_str(frac);
    }
    out
}

/// Formats an integer minor-unit amount and currency code (`Ui::FillAmountAndCurrency`).
///
/// For Telegram Stars (`"XTR"`), formats as `⭐<abs_amount>`.
pub fn fill_amount_and_currency(amount: i64, currency: &str, force_strip_dot_zero: bool) -> String {
    let abs_amount = amount.unsigned_abs();
    if currency == CREDITS_CURRENCY {
        return format!("\u{2B50}{abs_amount}");
    }
    let rule = lookup_currency_rule(currency);
    let value = (abs_amount as f64) / 10_f64.powi(rule.exponent as i32);
    let name = currency_name(currency);
    let mut result = String::new();
    if amount < 0 {
        result.push('\u{2212}'); // Unicode minus sign U+2212 matching C++ "\xe2\x88\x92"
    }
    if rule.left {
        result.push_str(name);
        if rule.space {
            result.push(' ');
        }
    }
    let precision = if (!rule.strip_dot_zero && !force_strip_dot_zero) || value.floor() != value {
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

/// Cursor-tracked text state for money input field validation (`SimpleFieldState`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoneyFieldState {
    /// Current field text.
    pub value: String,
    /// Cursor position in UTF-8 character indices.
    pub position: usize,
}

fn digits_only(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

fn char_slice(s: &str, start_char: usize, end_char: Option<usize>) -> String {
    match end_char {
        Some(end) => s
            .chars()
            .skip(start_char)
            .take(end.saturating_sub(start_char))
            .collect(),
        None => s.chars().skip(start_char).collect(),
    }
}

/// Cleans and normalizes raw money input text and cursor position
/// (`CleanMoneyState` in `payments/ui/payments_field.cpp`).
pub fn clean_money_state(rule: CurrencyRule, mut state: MoneyFieldState) -> MoneyFieldState {
    let with_decimal: String = state
        .value
        .chars()
        .map(|c| {
            if c == '.' || c == ',' {
                rule.decimal
            } else {
                c
            }
        })
        .collect();
    let digits_limit = 16usize.saturating_sub(rule.exponent as usize);
    let before_position = char_slice(&state.value, 0, Some(state.position));
    let mut decimal_pos: Option<usize> = with_decimal
        .chars()
        .enumerate()
        .filter(|&(_, c)| c == rule.decimal)
        .map(|(i, _)| i)
        .last();

    if let Some(dec_pos) = decimal_pos {
        let only_before = digits_only(&char_slice(&state.value, 0, Some(dec_pos)));
        let only_after = digits_only(&char_slice(&state.value, dec_pos + 1, None));
        let new_val = format!("{only_before}{}{only_after}", rule.decimal);
        let new_pos =
            digits_only(&before_position).chars().count() + usize::from(state.position > dec_pos);
        state = MoneyFieldState {
            value: new_val,
            position: new_pos,
        };
        let new_dec_pos = only_before.chars().count();
        decimal_pos = Some(new_dec_pos);
        let max_len = new_dec_pos + 1 + rule.exponent as usize;
        if state.value.chars().count() > max_len {
            state = MoneyFieldState {
                value: char_slice(&state.value, 0, Some(max_len)),
                position: state.position.min(max_len),
            };
        }
    } else {
        state = MoneyFieldState {
            value: digits_only(&state.value),
            position: digits_only(&before_position).chars().count(),
        };
    }

    if state.value.starts_with(rule.decimal) {
        state.value.insert(0, '0');
        state.position += 1;
        if let Some(ref mut dp) = decimal_pos {
            *dp += 1;
        }
    }

    let chars: Vec<char> = state.value.chars().collect();
    let mut skip = 0usize;
    while chars.len() > skip + 1 && chars[skip] == '0' && chars[skip + 1] != rule.decimal {
        skip += 1;
    }
    if skip > 0 {
        state.value = chars[skip..].iter().collect();
        state.position = state.position.saturating_sub(skip);
    }

    if let Some(mut dp) = decimal_pos {
        dp = dp.saturating_sub(skip);
        if dp > digits_limit {
            let head = char_slice(&state.value, 0, Some(digits_limit));
            let tail = char_slice(&state.value, dp, None);
            let new_pos = if state.position > digits_limit {
                state
                    .position
                    .saturating_sub(dp - digits_limit)
                    .max(digits_limit)
            } else {
                state.position
            };
            state = MoneyFieldState {
                value: format!("{head}{tail}"),
                position: new_pos,
            };
        }
    } else if state.value.chars().count() > digits_limit {
        state = MoneyFieldState {
            value: char_slice(&state.value, 0, Some(digits_limit)),
            position: state.position.min(digits_limit),
        };
    }
    state
}

/// Inserts field thousands separators into a cleaned money state
/// (`PostprocessMoneyResult` in `payments/ui/payments_field.cpp`).
pub fn postprocess_money_result(rule: CurrencyRule, state: MoneyFieldState) -> MoneyFieldState {
    let sep = rule.field_thousands_separator();
    if sep == '\0' {
        return state;
    }
    let mut chars: Vec<char> = state.value.chars().collect();
    let from = chars
        .iter()
        .position(|&c| c == rule.decimal)
        .unwrap_or(chars.len());
    let mut pos = state.position;
    let mut insert_at = from as isize - 3;
    while insert_at > 0 {
        let idx = insert_at as usize;
        chars.insert(idx, sep);
        if pos >= idx {
            pos += 1;
        }
        insert_at -= 3;
    }
    MoneyFieldState {
        value: chars.into_iter().collect(),
        position: pos,
    }
}

/// Formats an integer minor-unit amount for display inside a money field (`Parse` in `payments_field.cpp`).
pub fn format_money_field_value(amount: i64, currency: &str) -> String {
    if amount == 0 {
        return String::new();
    }
    let rule = lookup_currency_rule(currency);
    let value = (amount.unsigned_abs() as f64) / 10_f64.powi(rule.exponent as i32);
    let precision = if !rule.strip_dot_zero || value.floor() != value {
        rule.exponent
    } else {
        0
    };
    format_with_separators(
        value,
        precision,
        rule.decimal,
        rule.field_thousands_separator(),
    )
}

/// Parses an edited money field text back into minor-unit integer amount (`Format` in `payments_field.cpp`).
pub fn parse_money_field_value(text: &str, currency: &str) -> i64 {
    let rule = lookup_currency_rule(currency);
    let normalized: String = text
        .chars()
        .map(|c| {
            if c == rule.decimal || c == ',' {
                '.'
            } else {
                c
            }
        })
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let real: f64 = normalized.parse().unwrap_or(0.0);
    (real * 10_f64.powi(rule.exponent as i32)).round() as i64
}
