//! Plural rules (CLDR), tag replacement, shortened count formatting, and numeric/entity helpers.
//!
//! Port of `Telegram/SourceFiles/lang/lang_tag.cpp`,
//! `Telegram/SourceFiles/lang/lang_numbers_animation.cpp`,
//! and `Telegram/SourceFiles/lang/lang_text_entity.cpp`.

use crate::strings_parser::{TEXT_COMMAND_CHAR, TEXT_COMMAND_LANG_TAG_CHAR};

/// Sentinel `key_base` passed to [`plural_with_rule`] for cloud-only plural values.
pub const PLURAL_KEY_BASE_FOR_CLOUD_VALUE: u16 = u16::MAX;

/// Number of `char`s in an encoded tag replacement marker (`\x10\x20<tag>\x10`).
pub const TAG_REPLACEMENT_CHARS: usize = 4;

/// One of the 6 CLDR plural forms supported by Telegram Desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum PluralForm {
    /// Zero form (index 0).
    Zero = 0,
    /// Singular form (index 1).
    One = 1,
    /// Dual form (index 2).
    Two = 2,
    /// Few form (index 3).
    Few = 3,
    /// Many form (index 4).
    Many = 4,
    /// Default / other form (index 5).
    Other = 5,
}

impl PluralForm {
    /// Numeric shift (`0..6`) within a 6-slot plural phrase group.
    #[inline]
    pub const fn shift(self) -> usize {
        self as usize
    }
}

/// How the numeric `{count}` placeholder in a plural phrase should be formatted.
///
/// Mirrors `lt_count`, `lt_count_short`, and `lt_count_decimal` in `lang_auto.h`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum CountFormat {
    /// Plain number (`12345`).
    #[default]
    Count,
    /// Abbreviated count (`12.3K`, `1.2M`, `1.2B`).
    CountShort,
    /// Locale-grouped decimal (`12,345`).
    CountDecimal,
}

/// Typed plural count parameter accepted by generated `tr::` plural functions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PluralCount {
    /// Numeric value used both for plural rule selection and formatting.
    pub value: f64,
    /// Formatting style (`Count`, `CountShort`, or `CountDecimal`).
    pub format: CountFormat,
}

impl PluralCount {
    /// Plain unformatted count (`lt_count`).
    #[inline]
    pub fn plain(value: impl Into<f64>) -> Self {
        Self {
            value: value.into(),
            format: CountFormat::Count,
        }
    }

    /// Shortened count (`lt_count_short`, e.g., `12.3K`).
    #[inline]
    pub fn short(value: impl Into<f64>) -> Self {
        Self {
            value: value.into(),
            format: CountFormat::CountShort,
        }
    }

    /// Decimal-grouped count (`lt_count_decimal`, e.g., `12,345`).
    #[inline]
    pub fn decimal(value: impl Into<f64>) -> Self {
        Self {
            value: value.into(),
            format: CountFormat::CountDecimal,
        }
    }
}

impl From<f64> for PluralCount {
    #[inline]
    fn from(value: f64) -> Self {
        Self {
            value,
            format: CountFormat::Count,
        }
    }
}

impl From<i32> for PluralCount {
    #[inline]
    fn from(value: i32) -> Self {
        Self::from(f64::from(value))
    }
}

impl From<u32> for PluralCount {
    #[inline]
    fn from(value: u32) -> Self {
        Self::from(f64::from(value))
    }
}

impl From<i64> for PluralCount {
    #[inline]
    fn from(value: i64) -> Self {
        Self::from(value as f64)
    }
}

impl From<u64> for PluralCount {
    #[inline]
    fn from(value: u64) -> Self {
        Self::from(value as f64)
    }
}

impl From<usize> for PluralCount {
    #[inline]
    fn from(value: usize) -> Self {
        Self::from(value as f64)
    }
}

impl From<isize> for PluralCount {
    #[inline]
    fn from(value: isize) -> Self {
        Self::from(value as f64)
    }
}

/// Result of [`format_count_to_short`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShortenedCount {
    /// Rounded numeric value used for plural selection (e.g. `123000` for `12345`).
    pub number: i64,
    /// Human-readable shortened representation (e.g. `"12.3K"`).
    pub string: String,
    /// `true` if shortening (`K`/`M`/`B`) was applied.
    pub shortened: bool,
}

/// Result of [`plural_with_rule`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluralResult {
    /// Selected plural form (`0..6`).
    pub form: PluralForm,
    /// Slot offset (`0..6`) from the plural base key.
    pub key_shift: usize,
    /// Formatted string to substitute for `{count}`.
    pub replacement: String,
}

/// Stand-in for `CreditsAmount` from `Telegram/SourceFiles/core/credits_amount.h`.
///
/// Used by [`format_credits_amount_to_short`], [`format_credits_amount_decimal`],
/// and [`format_credits_amount_rounded`].
pub mod stand_in {
    /// Nanounits per whole Star/TON (`10^9`).
    pub const ONE_STAR_IN_NANO: i64 = 1_000_000_000;

    /// Currency unit of a [`CreditsAmount`].
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
    pub enum CreditsType {
        /// Telegram Stars.
        #[default]
        Stars,
        /// TON cryptocurrency.
        Ton,
    }

    /// Fixed-point Stars/TON amount (`whole` + `nano / 1e9`).
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct CreditsAmount {
        whole: i64,
        nano: i64,
        kind: CreditsType,
    }

    impl CreditsAmount {
        /// Construct a whole-unit amount.
        pub const fn from_whole(whole: i64, kind: CreditsType) -> Self {
            Self {
                whole,
                nano: 0,
                kind,
            }
        }

        /// Construct a normalized `whole + nano / 1e9` amount.
        pub fn new(mut whole: i64, mut nano: i64, kind: CreditsType) -> Self {
            // `div_euclid` / `rem_euclid` normalize without the `-nano` overflow on `i64::MIN`.
            whole = whole.saturating_add(nano.div_euclid(ONE_STAR_IN_NANO));
            nano = nano.rem_euclid(ONE_STAR_IN_NANO);
            Self { whole, nano, kind }
        }

        /// Whole-unit part.
        #[inline]
        pub const fn whole(self) -> i64 {
            self.whole
        }

        /// Fractional nanounits (`0..1_000_000_000`).
        #[inline]
        pub const fn nano(self) -> i64 {
            self.nano
        }

        /// Floating-point value (`whole + nano / 1e9`).
        #[inline]
        pub fn value(self) -> f64 {
            (self.whole as f64) + (self.nano as f64) / (ONE_STAR_IN_NANO as f64)
        }

        /// Currency unit type.
        #[inline]
        pub const fn kind(self) -> CreditsType {
            self.kind
        }
    }

    /// Minimal entity span stand-in for `TextWithEntities` (`lang_text_entity.cpp`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct TextEntity {
        /// Opaque entity type identifier.
        pub entity_type: u32,
        /// Character/UTF-8 byte offset (caller-consistent units; this module uses UTF-8 byte offsets).
        pub offset: usize,
        /// Entity length in UTF-8 bytes.
        pub length: usize,
        /// Optional extra payload (e.g., URL).
        pub data: String,
    }

    /// Minimal rich text container stand-in for `TextWithEntities`.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct TextWithEntities {
        /// Plain text content.
        pub text: String,
        /// Ordered entity spans within `text`.
        pub entities: Vec<TextEntity>,
    }
}

pub use stand_in::{CreditsAmount, CreditsType, TextEntity, TextWithEntities};

/// Text with the byte span of a replaced number, for animated counters.
///
/// Port of `Ui::StringWithNumbers` usage in `lang_numbers_animation.cpp`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StringWithNumbers {
    /// Resulting formatted text.
    pub text: String,
    /// UTF-8 byte offset of the substituted numeric tag, if present.
    pub offset: Option<usize>,
    /// UTF-8 byte length of the substituted numeric tag.
    pub length: usize,
}

type ChoosePluralFn = fn(n: i64, i: i64, v: i64, w: i64, f: i64, t: i64) -> PluralForm;

/// Function pointer implementing a CLDR plural rule (`ChoosePluralMethod` in `lang_tag.cpp`).
///
/// Arguments follow CLDR operands:
/// - `n`: integer value of `|source|`, or `-1` for non-integers
/// - `i`: integer digits of `|source|`
/// - `v`: visible fractional digit count (with trailing zeros)
/// - `w`: visible fractional digit count (without trailing zeros)
/// - `f`: visible fractional digits as integer (with trailing zeros)
/// - `t`: visible fractional digits as integer (without trailing zeros)
#[derive(Clone, Copy)]
pub struct PluralRule {
    func: ChoosePluralFn,
    id: &'static str,
}

impl std::fmt::Debug for PluralRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluralRule").field("id", &self.id).finish()
    }
}

impl PartialEq for PluralRule {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for PluralRule {}

impl Default for PluralRule {
    fn default() -> Self {
        Self::english_default()
    }
}

impl PluralRule {
    /// Built-in default English plural rule (`ChoosePlural2ast`).
    #[inline]
    pub const fn english_default() -> Self {
        Self {
            func: choose_plural_2_ast,
            id: "2ast",
        }
    }

    /// Resolve the plural rule for a Telegram / BCP-47 language code (`Lang::UpdatePluralRules`).
    ///
    /// Normalizes ASCII case and `_` -> `-`, checks the full tag first (e.g. `pt_PT`, `sr_Latn`,
    /// `ro_MD`), then falls back to the base subtag before `-` (e.g. `pt-br` -> `pt`).
    /// Unlisted languages (e.g. `zh`, `ja`, `ko`, `vi`, `th`, `id`) return the single-form
    /// `Other` rule (`ChoosePlural1`), while `""` returns `ChoosePlural2ast` (English).
    pub fn for_language(language_id: &str) -> Self {
        let normalized: String = language_id
            .chars()
            .map(|c| {
                if c == '_' {
                    '-'
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect();

        if let Some(rule) = lookup_plural_rule(&normalized) {
            return rule;
        }
        if let Some((parent, _)) = normalized.split_once('-')
            && let Some(rule) = lookup_plural_rule(parent)
        {
            return rule;
        }
        Self {
            func: choose_plural_1,
            id: "1",
        }
    }

    /// Evaluate the plural rule on CLDR operands `(n, i, v, w, f, t)`.
    #[inline]
    pub fn choose(self, n: i64, i: i64, v: i64, w: i64, f: i64, t: i64) -> PluralForm {
        (self.func)(n, i, v, w, f, t)
    }

    /// Convenience helper: choose the plural form for an integer count.
    #[inline]
    pub fn choose_int(self, count: i64) -> PluralForm {
        let abs = count.unsigned_abs() as i64;
        self.choose(abs, abs, 0, 0, 0, 0)
    }

    /// Convenience helper: choose the plural form for a floating-point count.
    pub fn choose_f64(self, value: f64) -> PluralForm {
        let (n_arg, i, v, w, f, t) = decompose_cldr_operands(value.abs());
        self.choose(n_arg, i, v, w, f, t)
    }
}

// ============================================================================
// CLDR Plural Rule Functions (exact port of lang_tag.cpp lines 48-495)
// ============================================================================

fn choose_plural_1(_n: i64, _i: i64, _v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    PluralForm::Other
}

fn choose_plural_2_fil(_n: i64, i: i64, v: i64, _w: i64, f: i64, _t: i64) -> PluralForm {
    if v == 0 {
        let mod10 = i % 10;
        if (i == 1 || i == 2 || i == 3) || (mod10 != 4 && mod10 != 6 && mod10 != 9) {
            return PluralForm::One;
        }
        return PluralForm::Other;
    }
    let mod10 = f % 10;
    if mod10 != 4 && mod10 != 6 && mod10 != 9 {
        PluralForm::One
    } else {
        PluralForm::Other
    }
}

fn choose_plural_2_tzm(n: i64, _i: i64, _v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    if n == 0 || n == 1 || (11..=99).contains(&n) {
        PluralForm::One
    } else {
        PluralForm::Other
    }
}

fn choose_plural_2_is(_n: i64, i: i64, _v: i64, _w: i64, _f: i64, t: i64) -> PluralForm {
    if t == 0 {
        let mod10 = i % 10;
        let mod100 = i % 100;
        if mod10 == 1 && mod100 != 11 {
            return PluralForm::One;
        }
        return PluralForm::Other;
    }
    PluralForm::One
}

fn choose_plural_2_mk(_n: i64, i: i64, v: i64, _w: i64, f: i64, _t: i64) -> PluralForm {
    if v == 0 {
        let mod10 = i % 10;
        let mod100 = i % 100;
        if mod10 == 1 && mod100 != 11 {
            return PluralForm::One;
        }
    }
    let mod10 = f % 10;
    let mod100 = f % 100;
    if mod10 == 1 && mod100 != 11 {
        return PluralForm::One;
    }
    PluralForm::Other
}

fn choose_plural_2_ak(n: i64, _i: i64, _v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    if n == 0 || n == 1 {
        PluralForm::One
    } else {
        PluralForm::Other
    }
}

fn choose_plural_2_am(n: i64, i: i64, _v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    if i == 0 || n == 1 {
        PluralForm::One
    } else {
        PluralForm::Other
    }
}

fn choose_plural_2_hy(_n: i64, i: i64, _v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    if i == 0 || i == 1 {
        PluralForm::One
    } else {
        PluralForm::Other
    }
}

fn choose_plural_2_si(n: i64, i: i64, _v: i64, _w: i64, f: i64, _t: i64) -> PluralForm {
    if n == 0 || n == 1 || (i == 0 && f == 1) {
        PluralForm::One
    } else {
        PluralForm::Other
    }
}

fn choose_plural_2_bh(n: i64, _i: i64, _v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    if n == 0 || n == 1 {
        PluralForm::One
    } else {
        PluralForm::Other
    }
}

fn choose_plural_2_af(n: i64, _i: i64, _v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    if n == 1 {
        PluralForm::One
    } else {
        PluralForm::Other
    }
}

fn choose_plural_2_ast(_n: i64, i: i64, v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    if i == 1 && v == 0 {
        PluralForm::One
    } else {
        PluralForm::Other
    }
}

fn choose_plural_2_da(n: i64, i: i64, _v: i64, _w: i64, _f: i64, t: i64) -> PluralForm {
    if n == 1 || (t != 0 && (i == 0 || i == 1)) {
        PluralForm::One
    } else {
        PluralForm::Other
    }
}

fn choose_plural_3_lv(n: i64, _i: i64, v: i64, _w: i64, f: i64, _t: i64) -> PluralForm {
    let nmod10 = n % 10;
    let nmod100 = n % 100;
    let fmod10 = f % 10;
    let fmod100 = f % 100;
    if nmod10 == 0 || (11..=19).contains(&nmod100) || (v == 2 && (11..=19).contains(&fmod100)) {
        PluralForm::Zero
    } else if (nmod10 == 1 && nmod100 != 11)
        || (v == 2 && fmod10 == 1 && fmod100 != 11)
        || (v != 2 && fmod10 == 1)
    {
        PluralForm::One
    } else {
        PluralForm::Other
    }
}

fn choose_plural_3_ksh(n: i64, _i: i64, _v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    if n == 0 {
        PluralForm::Zero
    } else if n == 1 {
        PluralForm::One
    } else {
        PluralForm::Other
    }
}

fn choose_plural_3_lag(n: i64, i: i64, _v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    if n == 0 {
        PluralForm::Zero
    } else if n != 0 && (i == 0 || i == 1) {
        PluralForm::One
    } else {
        PluralForm::Other
    }
}

fn choose_plural_3_kw(n: i64, _i: i64, _v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    if n == 1 {
        PluralForm::One
    } else if n == 2 {
        PluralForm::Two
    } else {
        PluralForm::Other
    }
}

fn choose_plural_3_bs(_n: i64, i: i64, v: i64, _w: i64, f: i64, _t: i64) -> PluralForm {
    if v == 0 {
        let mod10 = i % 10;
        let mod100 = i % 100;
        if (2..=4).contains(&mod10) && !(12..=14).contains(&mod100) {
            return PluralForm::Few;
        } else if mod10 == 1 && mod100 != 11 {
            return PluralForm::One;
        }
        return PluralForm::Other;
    }
    let mod10 = f % 10;
    let mod100 = f % 100;
    if (2..=4).contains(&mod10) && !(12..=14).contains(&mod100) {
        PluralForm::Few
    } else if mod10 == 1 && mod100 != 11 {
        PluralForm::One
    } else {
        PluralForm::Other
    }
}

fn choose_plural_3_shi(n: i64, i: i64, _v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    if i == 0 || n == 1 {
        PluralForm::One
    } else if (2..=10).contains(&n) {
        PluralForm::Few
    } else {
        PluralForm::Other
    }
}

fn choose_plural_3_mo(n: i64, i: i64, v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    if v == 0 {
        let mod100 = n % 100;
        if i == 1 {
            return PluralForm::One;
        } else if n == 0 || (n != 1 && (1..=19).contains(&mod100)) {
            return PluralForm::Few;
        }
        return PluralForm::Other;
    }
    PluralForm::Few
}

fn choose_plural_4_be(n: i64, _i: i64, _v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    let mod10 = n % 10;
    let mod100 = n % 100;
    if (2..=4).contains(&mod10) && !(12..=14).contains(&mod100) {
        PluralForm::Few
    } else if mod10 == 1 && mod100 != 11 {
        PluralForm::One
    } else if mod10 == 0 || (5..=9).contains(&mod10) || (11..=14).contains(&mod100) {
        PluralForm::Many
    } else {
        PluralForm::Other
    }
}

fn choose_plural_4_ru(_n: i64, i: i64, v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    if v == 0 {
        let mod10 = i % 10;
        let mod100 = i % 100;
        if (2..=4).contains(&mod10) && !(12..=14).contains(&mod100) {
            return PluralForm::Few;
        } else if mod10 == 1 && mod100 != 11 {
            return PluralForm::One;
        }
        return PluralForm::Many;
    }
    PluralForm::Other
}

fn choose_plural_4_pl(_n: i64, i: i64, v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    if v == 0 {
        if i == 1 {
            return PluralForm::One;
        }
        let mod10 = i % 10;
        let mod100 = i % 100;
        if (2..=4).contains(&mod10) && !(12..=14).contains(&mod100) {
            return PluralForm::Few;
        }
        return PluralForm::Many;
    }
    PluralForm::Other
}

fn choose_plural_4_lt(n: i64, _i: i64, _v: i64, _w: i64, f: i64, _t: i64) -> PluralForm {
    let mod10 = n % 10;
    let mod100 = n % 100;
    if (2..=9).contains(&mod10) && !(11..=19).contains(&mod100) {
        PluralForm::Few
    } else if mod10 == 1 && mod100 != 11 {
        PluralForm::One
    } else if f != 0 {
        PluralForm::Many
    } else {
        PluralForm::Other
    }
}

fn choose_plural_4_cs(_n: i64, i: i64, v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    if v == 0 {
        if i == 1 {
            return PluralForm::One;
        } else if (2..=4).contains(&i) {
            return PluralForm::Few;
        }
        return PluralForm::Other;
    }
    PluralForm::Many
}

fn choose_plural_4_gd(n: i64, _i: i64, _v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    if n == 1 || n == 11 {
        PluralForm::One
    } else if n == 2 || n == 12 {
        PluralForm::Two
    } else if (3..=10).contains(&n) || (13..=19).contains(&n) {
        PluralForm::Few
    } else {
        PluralForm::Other
    }
}

fn choose_plural_4_dsb(_n: i64, i: i64, v: i64, _w: i64, f: i64, _t: i64) -> PluralForm {
    if v == 0 {
        let imod100 = i % 100;
        if imod100 == 1 {
            return PluralForm::One;
        } else if imod100 == 2 {
            return PluralForm::Two;
        } else if imod100 == 3 || imod100 == 4 {
            return PluralForm::Few;
        }
    }
    let fmod100 = f % 100;
    if fmod100 == 1 {
        PluralForm::One
    } else if fmod100 == 2 {
        PluralForm::Two
    } else if fmod100 == 3 || fmod100 == 4 {
        PluralForm::Few
    } else {
        PluralForm::Other
    }
}

fn choose_plural_4_sl(_n: i64, i: i64, v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    if v == 0 {
        let imod100 = i % 100;
        if imod100 == 3 || imod100 == 4 {
            return PluralForm::Few;
        } else if imod100 == 1 {
            return PluralForm::One;
        } else if imod100 == 2 {
            return PluralForm::Two;
        }
        return PluralForm::Other;
    }
    PluralForm::Few
}

fn choose_plural_4_he(n: i64, i: i64, v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    if v == 0 {
        if i == 1 {
            return PluralForm::One;
        } else if i == 2 {
            return PluralForm::Two;
        } else if n != 0 && n != 10 && (n % 10) == 0 {
            return PluralForm::Many;
        }
        return PluralForm::Other;
    }
    PluralForm::Other
}

fn choose_plural_4_mt(n: i64, _i: i64, _v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    let mod100 = n % 100;
    if n == 1 {
        PluralForm::One
    } else if n == 0 || (2..=10).contains(&mod100) {
        PluralForm::Few
    } else if (11..=19).contains(&mod100) {
        PluralForm::Many
    } else {
        PluralForm::Other
    }
}

fn choose_plural_5_gv(_n: i64, i: i64, v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    if v == 0 {
        let mod10 = i % 10;
        let mod20 = i % 20;
        if mod10 == 1 {
            return PluralForm::One;
        } else if mod10 == 2 {
            return PluralForm::Two;
        } else if mod20 == 0 {
            return PluralForm::Few;
        }
        return PluralForm::Other;
    }
    PluralForm::Many
}

fn choose_plural_5_br(n: i64, _i: i64, _v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    let mod10 = n % 10;
    let mod100 = n % 100;
    if mod10 == 1 && mod100 != 11 && mod100 != 71 && mod100 != 91 {
        PluralForm::One
    } else if mod10 == 2 && mod100 != 12 && mod100 != 72 && mod100 != 92 {
        PluralForm::Two
    } else if (mod10 == 3 || mod10 == 4 || mod10 == 9)
        && !(10..=19).contains(&mod100)
        && !(70..=79).contains(&mod100)
        && !(90..=99).contains(&mod100)
    {
        PluralForm::Few
    } else if n != 0 && (n % 1_000_000) == 0 {
        PluralForm::Many
    } else {
        PluralForm::Other
    }
}

fn choose_plural_5_ga(n: i64, _i: i64, _v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    if n == 1 {
        PluralForm::One
    } else if n == 2 {
        PluralForm::Two
    } else if (3..=6).contains(&n) {
        PluralForm::Few
    } else if (7..=10).contains(&n) {
        PluralForm::Many
    } else {
        PluralForm::Other
    }
}

fn choose_plural_6_ar(n: i64, _i: i64, _v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    if n == 0 {
        return PluralForm::Zero;
    } else if n == 1 {
        return PluralForm::One;
    } else if n == 2 {
        return PluralForm::Two;
    } else if n < 0 {
        return PluralForm::Other;
    }
    let mod100 = n % 100;
    if (3..=10).contains(&mod100) {
        PluralForm::Few
    } else if (11..=99).contains(&mod100) {
        PluralForm::Many
    } else {
        PluralForm::Other
    }
}

fn choose_plural_6_cy(n: i64, _i: i64, _v: i64, _w: i64, _f: i64, _t: i64) -> PluralForm {
    match n {
        0 => PluralForm::Zero,
        1 => PluralForm::One,
        2 => PluralForm::Two,
        3 => PluralForm::Few,
        6 => PluralForm::Many,
        _ => PluralForm::Other,
    }
}

/// Exact mapping from normalized lowercase/hyphenated language tag to CLDR rule
/// (`GeneratePluralRulesMap` in `lang_tag.cpp` lines 521-734).
fn lookup_plural_rule(code: &str) -> Option<PluralRule> {
    let (func, id): (ChoosePluralFn, &'static str) = match code {
        "fil" | "tl" => (choose_plural_2_fil, "2fil"),
        "tzm" => (choose_plural_2_tzm, "2tzm"),
        "is" => (choose_plural_2_is, "2is"),
        "mk" => (choose_plural_2_mk, "2mk"),
        "ak" | "guw" | "ln" | "mg" | "nso" | "pa" | "ti" | "wa" => (choose_plural_2_ak, "2ak"),
        "am" | "as" | "bn" | "gu" | "hi" | "kn" | "mr" | "fa" | "zu" => (choose_plural_2_am, "2am"),
        "hy" | "fr" | "ff" | "kab" | "pt" => (choose_plural_2_hy, "2hy"),
        "si" => (choose_plural_2_si, "2si"),
        "bh" | "bho" => (choose_plural_2_bh, "2bh"),
        "af" | "sq" | "asa" | "az" | "eu" | "bem" | "bez" | "brx" | "bg" | "ckb" | "ce" | "chr"
        | "cgg" | "dv" | "eo" | "ee" | "fo" | "fur" | "ka" | "el" | "ha" | "haw" | "hu" | "kaj"
        | "kkj" | "kl" | "ks" | "kk" | "ku" | "ky" | "lb" | "jmc" | "ml" | "mas" | "mgo" | "mn"
        | "nah" | "ne" | "nnh" | "jgo" | "nd" | "no" | "nb" | "nn" | "ny" | "nyn" | "or" | "om"
        | "os" | "pap" | "ps" | "rm" | "rof" | "rwk" | "ssy" | "saq" | "seh" | "ksb" | "sn"
        | "sd" | "xog" | "so" | "nr" | "sdh" | "st" | "es" | "ss" | "gsw" | "syr" | "ta" | "te"
        | "teo" | "tig" | "ts" | "tn" | "tr" | "tk" | "kcg" | "ug" | "uz" | "ve" | "vo" | "vun"
        | "wae" | "xh" => (choose_plural_2_af, "2af"),
        "" | "ast" | "ca" | "nl" | "en" | "et" | "pt-pt" | "fi" | "gl" | "lg" | "de" | "io"
        | "ia" | "it" | "sc" | "scn" | "sw" | "sv" | "ur" | "fy" | "ji" | "yi" => {
            (choose_plural_2_ast, "2ast")
        }
        "da" => (choose_plural_2_da, "2da"),
        "lv" | "prg" => (choose_plural_3_lv, "3lv"),
        "ksh" => (choose_plural_3_ksh, "3ksh"),
        "lag" => (choose_plural_3_lag, "3lag"),
        "kw" | "smn" | "iu" | "smj" | "naq" | "se" | "smi" | "sms" | "sma" => {
            (choose_plural_3_kw, "3kw")
        }
        "bs" | "hr" | "sr" | "sh" | "sr-latn" => (choose_plural_3_bs, "3bs"),
        "shi" => (choose_plural_3_shi, "3shi"),
        "mo" | "ro-md" | "ro" => (choose_plural_3_mo, "3mo"),
        "be" => (choose_plural_4_be, "4be"),
        "ru" | "uk" => (choose_plural_4_ru, "4ru"),
        "pl" => (choose_plural_4_pl, "4pl"),
        "lt" => (choose_plural_4_lt, "4lt"),
        "cs" | "sk" => (choose_plural_4_cs, "4cs"),
        "gd" => (choose_plural_4_gd, "4gd"),
        "dsb" | "hsb" => (choose_plural_4_dsb, "4dsb"),
        "sl" => (choose_plural_4_sl, "4sl"),
        "he" | "iw" => (choose_plural_4_he, "4he"),
        "mt" => (choose_plural_4_mt, "4mt"),
        "gv" => (choose_plural_5_gv, "5gv"),
        "br" => (choose_plural_5_br, "5br"),
        "ga" => (choose_plural_5_ga, "5ga"),
        "ar" | "ars" => (choose_plural_6_ar, "6ar"),
        "cy" => (choose_plural_6_cy, "6cy"),
        _ => return None,
    };
    Some(PluralRule { func, id })
}

// ============================================================================
// Formatting & Tag Replacement Helpers (lang_tag.cpp lines 873-1060)
// ============================================================================

/// Format a floating-point number with up to 6 fractional digits, stripping trailing zeros.
///
/// Port of `Lang::FormatDouble`.
pub fn format_double(value: f64) -> String {
    let mut s = format!("{value:.6}");
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" {
        return "0".to_string();
    }
    s
}

/// Parse a fractional digit string after skipping leading zeros (`Lang::NonZeroPartToInt`).
pub fn non_zero_part_to_int(value: &str) -> i64 {
    let trimmed = value.trim_start_matches('0');
    if trimmed.is_empty() {
        0
    } else {
        trimmed.parse::<i64>().unwrap_or(0)
    }
}

fn decompose_cldr_operands(n: f64) -> (i64, i64, i64, i64, i64, i64) {
    let i = n.floor() as i64;
    let is_integer = (n.ceil() as i64) == i;
    if is_integer {
        return (i, i, 0, 0, 0, 0);
    }
    let formatted = format_double(n);
    let fraction = formatted
        .split_once('.')
        .map(|(_, frac)| frac)
        .unwrap_or("");
    let v = fraction.len() as i64;
    let w = v;
    let f = non_zero_part_to_int(fraction);
    let t = f;
    (-1, i, v, w, f, t)
}

/// Shorten large counts into `K` / `M` / `B` form (`Lang::FormatCountToShort`).
pub fn format_count_to_short(number: i64, only_k: bool) -> ShortenedCount {
    let mut result = ShortenedCount {
        number,
        string: String::new(),
        shortened: false,
    };
    let abs = number.unsigned_abs();
    let mut shorten = |divider: u64, multiplier: char| {
        let sign: i64 = if number > 0 { 1 } else { -1 };
        let rounded = (abs / (divider / 10)) as i64;
        result.string = (sign * rounded / 10).to_string();
        if rounded % 10 != 0 {
            result.string.push('.');
            result.string.push_str(&(rounded % 10).to_string());
        }
        result.string.push(multiplier);
        // Matches tdesktop line 934 (`result.number = rounded * divider`).
        result.number = rounded.saturating_mul(divider as i64);
        result.shortened = true;
    };

    if !only_k && abs >= 1_000_000_000 {
        shorten(1_000_000_000, 'B');
    } else if !only_k && abs >= 1_000_000 {
        shorten(1_000_000, 'M');
    } else if abs >= 10_000 {
        shorten(1_000, 'K');
    } else {
        result.string = number.to_string();
    }
    result
}

/// Format an integer with comma thousands separators (`Lang::FormatCountDecimal`).
pub fn format_count_decimal(number: i64) -> String {
    format_count_decimal_with_sep(number, ',')
}

/// Format an integer with a custom thousands separator.
pub fn format_count_decimal_with_sep(number: i64, thousands_sep: char) -> String {
    let negative = number < 0;
    let digits = number.unsigned_abs().to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3 + usize::from(negative));
    if negative {
        out.push('-');
    }
    for (idx, ch) in digits.chars().enumerate() {
        if idx > 0 && (len - idx).is_multiple_of(3) {
            out.push(thousands_sep);
        }
        out.push(ch);
    }
    out
}

/// Format a floating-point count with thousands separators and up to 6 trimmed decimal digits
/// (`Lang::FormatExactCountDecimal`).
pub fn format_exact_count_decimal(number: f64) -> String {
    let rounded = number.round();
    if (number - rounded).abs() <= 1e-12 * number.abs().max(1.0) {
        return format_count_decimal(rounded as i64);
    }
    let formatted = format_double(number);
    match formatted.split_once('.') {
        Some((int_part, frac_part)) => {
            let int_val = int_part.parse::<i64>().unwrap_or(0);
            let mut prefix = if number < 0.0 && int_val == 0 {
                "-0".to_string()
            } else {
                format_count_decimal(int_val)
            };
            prefix.push('.');
            prefix.push_str(frac_part);
            prefix
        }
        None => format_count_decimal(rounded as i64),
    }
}

/// Shorten a [`CreditsAmount`] if large, otherwise format exact decimal (`Lang::FormatCreditsAmountToShort`).
pub fn format_credits_amount_to_short(amount: CreditsAmount) -> ShortenedCount {
    let attempt = format_count_to_short(amount.whole(), false);
    if attempt.shortened {
        attempt
    } else {
        ShortenedCount {
            number: 0,
            string: format_credits_amount_decimal(amount),
            shortened: false,
        }
    }
}

/// Format a [`CreditsAmount`] with exact decimal digits (`Lang::FormatCreditsAmountDecimal`).
pub fn format_credits_amount_decimal(amount: CreditsAmount) -> String {
    format_exact_count_decimal(amount.value())
}

/// Format a [`CreditsAmount`] rounded to 2 decimal places (`Lang::FormatCreditsAmountRounded`).
pub fn format_credits_amount_rounded(amount: CreditsAmount) -> String {
    let value = amount.value();
    format_exact_count_decimal((value * 100.0).round() / 100.0)
}

/// Compute the plural shift and formatted count replacement for a given rule
/// (`Lang::Plural` in `lang_tag.cpp`).
pub fn plural_with_rule(
    rule: PluralRule,
    use_non_default: bool,
    value: f64,
    format: CountFormat,
) -> PluralResult {
    let shortened = if format == CountFormat::CountShort {
        format_count_to_short(value.round() as i64, false)
    } else {
        ShortenedCount::default()
    };

    let n = if shortened.number != 0 {
        (shortened.number as f64).abs()
    } else {
        value.abs()
    };
    let (n_arg, i, v, w, f, t) = decompose_cldr_operands(n);
    let is_integer = n_arg >= 0;

    let active_rule = if use_non_default {
        rule
    } else {
        PluralRule::english_default()
    };
    let form = active_rule.choose(n_arg, i, v, w, f, t);

    let replacement = if is_integer {
        let round = value.round() as i64;
        match format {
            CountFormat::CountShort => shortened.string,
            CountFormat::CountDecimal => format_count_decimal(round),
            CountFormat::Count => round.to_string(),
        }
    } else {
        format_double(value)
    };

    PluralResult {
        form,
        key_shift: form.shift(),
        replacement,
    }
}

/// Find the UTF-8 byte `(start, end)` span of an encoded placeholder marker for `tag`.
///
/// Port of `Lang::FindTagReplacementPosition` (UTF-8 char-boundary safe).
pub fn find_tag_replacement_span(original: &str, tag: u16) -> Option<(usize, usize)> {
    let target = char::from_u32(0x0020u32 + u32::from(tag))?;
    let mut iter = original.char_indices();
    while let Some((start, c0)) = iter.next() {
        if c0 != TEXT_COMMAND_CHAR {
            continue;
        }
        let mut look = iter.clone();
        if let (Some((_, c1)), Some((_, c2)), Some((end_start, c3))) =
            (look.next(), look.next(), look.next())
            && c1 == TEXT_COMMAND_LANG_TAG_CHAR
            && c3 == TEXT_COMMAND_CHAR
        {
            let end = end_start + c3.len_utf8();
            if c2 == target {
                return Some((start, end));
            }
            iter = look;
        }
    }
    None
}

/// Replace the encoded placeholder for `tag` in `original` with `replacement`.
///
/// Port of `Lang::ReplaceTag<QString>::Call`.
pub fn replace_tag_str(mut original: String, tag: u16, replacement: &str) -> String {
    if let Some((start, end)) = find_tag_replacement_span(&original, tag) {
        original.replace_range(start..end, replacement);
    }
    original
}

/// Replace `tag` in a [`StringWithNumbers`], recording the UTF-8 byte offset and length
/// of the substituted number (`Lang::ReplaceTag<Ui::StringWithNumbers>::Call`).
pub fn replace_tag_numbers(
    mut original: StringWithNumbers,
    tag: u16,
    replacement: &str,
) -> StringWithNumbers {
    if let Some((start, end)) = find_tag_replacement_span(&original.text, tag) {
        original.offset = Some(start);
        original.text.replace_range(start..end, replacement);
        original.length = replacement.len();
    }
    original
}

/// Replace `tag` in a [`TextWithEntities`], adjusting existing entity spans and splicing in
/// the replacement's entities (`Lang::ReplaceTag<TextWithEntities>::Call`).
pub fn replace_tag_entities(
    original: TextWithEntities,
    tag: u16,
    replacement: &TextWithEntities,
) -> TextWithEntities {
    let Some((start, end)) = find_tag_replacement_span(&original.text, tag) else {
        return original;
    };
    let marker_len = end - start;
    let mut new_text = original.text;
    new_text.replace_range(start..end, &replacement.text);

    if original.entities.is_empty() && replacement.entities.is_empty() {
        return TextWithEntities {
            text: new_text,
            entities: Vec::new(),
        };
    }

    let mut entities = Vec::with_capacity(original.entities.len() + replacement.entities.len());
    let replacement_end = start + replacement.text.len();
    let mut repl_idx = 0usize;

    let add_replacement_until = |until: usize, repl_idx: &mut usize, out: &mut Vec<TextEntity>| {
        while *repl_idx < replacement.entities.len() {
            let re = &replacement.entities[*repl_idx];
            let mut new_offset = start + re.offset;
            if new_offset >= until {
                return;
            }
            let mut new_end = new_offset + re.length;
            new_offset = new_offset.clamp(start, replacement_end);
            new_end = new_end.clamp(start, replacement_end);
            if let Some(new_len) = new_end.checked_sub(new_offset).filter(|&l| l > 0) {
                out.push(TextEntity {
                    entity_type: re.entity_type,
                    offset: new_offset,
                    length: new_len,
                    data: re.data.clone(),
                });
            }
            *repl_idx += 1;
        }
    };

    let text_len = new_text.len();
    let delta_isize = replacement.text.len() as isize - marker_len as isize;
    for entity in original.entities {
        let mut offset = entity.offset;
        let mut ent_end = entity.offset + entity.length;
        if offset > start {
            offset = (offset as isize + delta_isize).clamp(0, text_len as isize) as usize;
        }
        if ent_end > start {
            ent_end = (ent_end as isize + delta_isize).clamp(0, text_len as isize) as usize;
        }
        offset = offset.min(text_len);
        ent_end = ent_end.min(text_len);

        add_replacement_until(offset, &mut repl_idx, &mut entities);
        if let Some(length) = ent_end.checked_sub(offset).filter(|&l| l > 0) {
            entities.push(TextEntity {
                entity_type: entity.entity_type,
                offset,
                length,
                data: entity.data,
            });
        }
    }
    add_replacement_until(text_len, &mut repl_idx, &mut entities);

    TextWithEntities {
        text: new_text,
        entities,
    }
}
