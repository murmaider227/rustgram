//! Country metadata, ISO2/calling-code lookup, phone number formatting, and cloud updates.
//!
//! Port of `Telegram/SourceFiles/countries/countries_instance.cpp`,
//! `Telegram/SourceFiles/countries/countries_manager.cpp`, and
//! `Ui::FormatPhone` from `Telegram/SourceFiles/ui/text/format_values.cpp`.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use tokio::sync::broadcast;

use tg_storage::qdatastream::{DataStreamError, QDataStreamReader, QDataStreamWriter};

const EVENT_CHANNEL_CAPACITY: usize = 16;

/// Calling code, optional area prefixes, and digit grouping patterns for a country.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallingCodeInfo {
    /// International calling code digits without leading `+` (e.g., `"1"`, `"44"`, `"380"`).
    pub calling_code: String,
    /// Optional prefixes disambiguating shared calling codes (e.g., `"403"` for CA under `+1`).
    /// An empty list is treated as `[""]` during matching (matching any prefix).
    pub prefixes: Vec<String>,
    /// Formatting patterns like `"XXX XXX XXXX"` or `"XX XXX XX XX"`.
    pub patterns: Vec<String>,
}

/// Country entry matching `Countries::Info`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CountryInfo {
    /// Default English country name.
    pub name: String,
    /// Two-letter ISO 3166-1 alpha-2 code (e.g., `"US"`, `"DE"`, `"UA"`).
    pub iso2: String,
    /// Optional alternative searchable name (e.g., `"United States of America"`).
    pub alternative_name: String,
    /// One or more calling code definitions for this country.
    pub codes: Vec<CallingCodeInfo>,
    /// Whether this country is hidden from pickers.
    pub is_hidden: bool,
}

/// Naming context for [`CountriesInstance::country_name_by_iso2`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Naming {
    /// Standard country name lookup.
    #[default]
    Default,
    /// Poll display naming (`"FT"` -> `"Fragment"`).
    Polls,
}

/// Input arguments for [`CountriesInstance::format`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormatArgs<'a> {
    /// Phone digits (without leading `+`).
    pub phone: &'a str,
    /// Compute digit group lengths instead of formatted string.
    pub only_groups: bool,
    /// Omit the `+<code> ` prefix (or leading code group).
    pub skip_code: bool,
    /// Compute groups for a partial/incomplete phone number while typing.
    pub incomplete: bool,
    /// Extract only the matched calling code.
    pub only_code: bool,
}

/// Output of [`CountriesInstance::format`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormatResult {
    /// Formatted phone number (e.g., `"+1 555 123 4567"`), empty when `only_groups` or `only_code`.
    pub formatted: String,
    /// Digit group lengths (e.g., `[1, 3, 3, 4]`).
    pub groups: Vec<usize>,
    /// Matched calling code when `only_code` is set.
    pub code: String,
}

/// Plain Rust representation of TL `help.countryCode` (`help.getCountriesList`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloudCountryCode {
    /// Country calling code digits.
    pub country_code: String,
    /// Optional prefixes list (`None` or empty means match all).
    pub prefixes: Option<Vec<String>>,
    /// Optional formatting patterns list.
    pub patterns: Option<Vec<String>>,
}

/// Plain Rust representation of TL `help.country` (`help.getCountriesList`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloudCountry {
    /// Whether the country is hidden.
    pub hidden: bool,
    /// Two-letter ISO2 code.
    pub iso2: String,
    /// Default English name.
    pub default_name: String,
    /// Optional localized name.
    pub name: Option<String>,
    /// Calling code definitions.
    pub country_codes: Vec<CloudCountryCode>,
}

/// Plain Rust representation of TL `help.CountriesList` (`help.getCountriesList` response).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudCountriesList {
    /// Server reports the cached hash is up to date (`help.countriesListNotModified`).
    NotModified,
    /// Server returned an updated countries list (`help.countriesList`).
    Modified {
        /// New cache hash to send on subsequent `help.getCountriesList` requests.
        hash: i32,
        /// Full list of countries from the server.
        countries: Vec<CloudCountry>,
    },
}

/// Serialized local cache payload matching `Countries::FileData` in `countries_manager.cpp`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CountriesCacheData {
    /// Server hash for `help.getCountriesList`.
    pub hash: i32,
    /// Cached country entries.
    pub infos: Vec<CountryInfo>,
}

/// Country registry, lookup tables, and phone formatter (`Countries::CountriesInstance`).
#[derive(Debug)]
pub struct CountriesInstance {
    list: Vec<CountryInfo>,
    by_code: HashMap<String, usize>,
    by_iso2: HashMap<String, usize>,
    hash: i32,
    updated: broadcast::Sender<()>,
}

impl Default for CountriesInstance {
    fn default() -> Self {
        Self::new()
    }
}

impl CountriesInstance {
    /// Create a new [`CountriesInstance`] pre-populated with all 231 built-in fallback countries.
    pub fn new() -> Self {
        let (updated, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let mut inst = Self {
            list: fallback_countries_list(),
            by_code: HashMap::new(),
            by_iso2: HashMap::new(),
            hash: 0,
            updated,
        };
        inst.rebuild_indices();
        inst
    }

    /// Subscribe to notifications fired whenever the country list is replaced.
    pub fn subscribe_updated(&self) -> broadcast::Receiver<()> {
        self.updated.subscribe()
    }

    /// Current list of countries.
    pub fn list(&self) -> &[CountryInfo] {
        &self.list
    }

    /// Current cloud `help.getCountriesList` hash.
    pub fn hash(&self) -> i32 {
        self.hash
    }

    /// Replace the country list and rebuild lookup maps (`CountriesInstance::setList`).
    pub fn set_list(&mut self, infos: Vec<CountryInfo>) {
        self.list = if infos.is_empty() {
            fallback_countries_list()
        } else {
            infos
        };
        self.rebuild_indices();
        let _ = self.updated.send(());
    }

    /// Apply a cloud `help.getCountriesList` response (`Countries::Manager::request` handler).
    ///
    /// Returns `true` if the list was modified (and caller should persist the cache).
    pub fn apply_cloud_list(&mut self, response: CloudCountriesList) -> bool {
        match response {
            CloudCountriesList::NotModified => false,
            CloudCountriesList::Modified { hash, countries } => {
                self.hash = hash;
                let mut infos = Vec::with_capacity(countries.len());
                for country in countries {
                    if country.hidden {
                        continue;
                    }
                    let alternative_name = if country.default_name == "USA" {
                        "United States of America".to_string()
                    } else {
                        String::new()
                    };
                    let codes = country
                        .country_codes
                        .into_iter()
                        .map(|c| {
                            let prefixes = match c.prefixes {
                                Some(p) if !p.is_empty() => p,
                                _ => vec![String::new()],
                            };
                            let patterns = c.patterns.unwrap_or_default();
                            CallingCodeInfo {
                                calling_code: c.country_code,
                                prefixes,
                                patterns,
                            }
                        })
                        .collect();
                    infos.push(CountryInfo {
                        name: country.default_name,
                        iso2: country.iso2,
                        alternative_name,
                        codes,
                        is_hidden: country.hidden,
                    });
                }
                self.set_list(infos);
                true
            }
        }
    }

    /// Look up a country by calling code (e.g., `"1"`, `"44"`, `"380"`).
    pub fn by_code(&self, code: &str) -> Option<&CountryInfo> {
        self.by_code.get(code).and_then(|&idx| self.list.get(idx))
    }

    /// Look up a country by two-letter ISO2 code (e.g., `"US"`, `"GB"`).
    pub fn by_iso2(&self, iso2: &str) -> Option<&CountryInfo> {
        self.by_iso2.get(iso2).and_then(|&idx| self.list.get(idx))
    }

    /// Find the longest valid calling code prefix of `full_code` (`CountriesInstance::validPhoneCode`).
    pub fn valid_phone_code(&self, full_code: &str) -> String {
        let mut end = full_code.len();
        while end > 0 {
            let candidate = &full_code[..end];
            if self.by_code.contains_key(candidate) {
                return candidate.to_string();
            }
            // Step back by one UTF-8 character boundary.
            end = full_code[..end]
                .char_indices()
                .next_back()
                .map_or(0, |(idx, _)| idx);
        }
        String::new()
    }

    /// Look up the country display name for an ISO2 code (`CountriesInstance::countryNameByISO2`).
    pub fn country_name_by_iso2(&self, iso: &str, naming: Naming) -> String {
        if naming == Naming::Polls && iso.eq_ignore_ascii_case("FT") {
            return "Fragment".to_string();
        }
        self.by_iso2(iso)
            .map(|c| c.name.clone())
            .unwrap_or_default()
    }

    /// Resolve a country's ISO2 code from a phone number or calling code (`countryISO2ByPhone`).
    pub fn country_iso2_by_phone(&self, phone: &str) -> String {
        let code = self.valid_phone_code(phone);
        self.by_code(&code)
            .map(|c| c.iso2.clone())
            .unwrap_or_default()
    }

    /// Convert a two-letter uppercase ISO2 code to its regional-indicator flag emoji
    /// (`CountriesInstance::flagEmojiByISO2`). `"FT"` returns the pirate flag emoji.
    pub fn flag_emoji_by_iso2(iso: &str) -> String {
        let bytes = iso.as_bytes();
        if bytes.len() != 2 || !bytes[0].is_ascii_uppercase() || !bytes[1].is_ascii_uppercase() {
            return String::new();
        }
        if iso == "FT" {
            return "\u{1F3F4}\u{200D}\u{2620}\u{FE0F}".to_string();
        }
        let c0 = char::from_u32(0x1F1E6 + u32::from(bytes[0] - b'A')).unwrap_or('\u{FFFD}');
        let c1 = char::from_u32(0x1F1E6 + u32::from(bytes[1] - b'A')).unwrap_or('\u{FFFD}');
        let mut out = String::with_capacity(8);
        out.push(c0);
        out.push(c1);
        out
    }

    /// Format a phone number or compute digit groups (`CountriesInstance::format`, ported from TDLib).
    pub fn format(&self, args: FormatArgs<'_>) -> FormatResult {
        if args.phone.is_empty() {
            return FormatResult::default();
        }
        let phone = args.phone;

        let mut best_country: Option<&CountryInfo> = None;
        let mut best_code: Option<&CallingCodeInfo> = None;
        let mut best_length = 0usize;

        for country in &self.list {
            for calling_code in &country.codes {
                if let Some(rest) = phone.strip_prefix(calling_code.calling_code.as_str()) {
                    let code_size = calling_code.calling_code.len();
                    if calling_code.prefixes.is_empty() {
                        if code_size > best_length {
                            best_country = Some(country);
                            best_code = Some(calling_code);
                            best_length = code_size;
                        }
                    } else {
                        for prefix in &calling_code.prefixes {
                            let match_len = code_size + prefix.len();
                            if match_len > best_length && rest.starts_with(prefix.as_str()) {
                                best_country = Some(country);
                                best_code = Some(calling_code);
                                best_length = match_len;
                            }
                        }
                    }
                }
            }
        }

        let (Some(_country), Some(best_calling_code)) = (best_country, best_code) else {
            return FormatResult {
                formatted: phone.to_string(),
                groups: Vec::new(),
                code: String::new(),
            };
        };

        if args.only_code {
            return FormatResult {
                formatted: String::new(),
                groups: Vec::new(),
                code: best_calling_code.calling_code.clone(),
            };
        }

        let code_size = best_calling_code.calling_code.len();

        if args.only_groups && args.incomplete {
            let initial_groups = if args.skip_code {
                Vec::new()
            } else {
                vec![code_size]
            };
            if best_calling_code.patterns.is_empty() {
                return FormatResult {
                    formatted: String::new(),
                    groups: initial_groups,
                    code: String::new(),
                };
            }
            let phone_chars: Vec<char> = phone.chars().collect();
            let mut best_groups = initial_groups.clone();
            let mut best_groups_size = 0usize;
            let mut best_pattern_max_matches: isize = -1;

            for pattern in &best_calling_code.patterns {
                let mut groups = initial_groups.clone();
                let mut group_size = 0usize;
                let mut last_spaces_count = 0usize;
                let mut max_matched_digits: isize = 0;
                let mut is_not_best_pattern = false;

                for (i, c) in pattern.chars().enumerate() {
                    if c.is_ascii_digit() {
                        let n = (i - last_spaces_count) + code_size;
                        if let Some(&pc) = phone_chars.get(n) {
                            if pc == c {
                                max_matched_digits += 1;
                            } else {
                                is_not_best_pattern = true;
                            }
                        } else {
                            is_not_best_pattern = true;
                        }
                    }
                    if c.is_whitespace() {
                        groups.push(std::mem::take(&mut group_size));
                        last_spaces_count += 1;
                    } else {
                        group_size += 1;
                    }
                }
                if max_matched_digits > best_pattern_max_matches {
                    best_pattern_max_matches = if is_not_best_pattern {
                        -1
                    } else {
                        max_matched_digits
                    };
                    best_groups = groups;
                    best_groups_size = group_size;
                }
            }
            if best_groups_size > 0 {
                best_groups.push(best_groups_size);
            }
            return FormatResult {
                formatted: String::new(),
                groups: best_groups,
                code: String::new(),
            };
        }

        let formatted_part = &phone[code_size..];
        let mut formatted_result = formatted_part.to_string();
        let mut groups = Vec::new();
        let mut max_matched_digits = 0usize;

        for pattern in &best_calling_code.patterns {
            let pattern_chars: Vec<char> = pattern.chars().collect();
            let mut result_groups = Vec::new();
            let mut result = String::with_capacity(formatted_part.len() + pattern.len());
            let mut current_pos = 0usize;
            let mut is_failed_match = false;
            let mut matched_digits = 0usize;
            let mut group_size = 0usize;

            for c in formatted_part.chars() {
                while current_pos < pattern_chars.len()
                    && pattern_chars[current_pos] != 'X'
                    && !pattern_chars[current_pos].is_ascii_digit()
                {
                    if args.only_groups {
                        result_groups.push(group_size);
                        group_size = 0;
                    } else {
                        result.push(pattern_chars[current_pos]);
                    }
                    current_pos += 1;
                }

                if current_pos >= pattern_chars.len() || pattern_chars[current_pos] == 'X' {
                    current_pos += 1;
                    if args.only_groups {
                        group_size += 1;
                    } else {
                        result.push(c);
                    }
                } else if c == pattern_chars[current_pos] {
                    matched_digits += 1;
                    current_pos += 1;
                    if args.only_groups {
                        group_size += 1;
                    } else {
                        result.push(c);
                    }
                } else {
                    is_failed_match = true;
                    break;
                }
            }

            if group_size > 0 {
                result_groups.push(group_size);
            }
            if !is_failed_match && matched_digits >= max_matched_digits {
                max_matched_digits = matched_digits;
                if args.only_groups {
                    groups = result_groups;
                } else {
                    formatted_result = result;
                }
            }
        }

        if !args.skip_code {
            if args.only_groups {
                groups.insert(0, code_size);
            } else {
                formatted_result =
                    format!("+{} {}", best_calling_code.calling_code, formatted_result);
            }
        }

        FormatResult {
            formatted: if args.only_groups {
                String::new()
            } else {
                formatted_result
            },
            groups,
            code: String::new(),
        }
    }

    /// Format a phone number for display (`Ui::FormatPhone`).
    ///
    /// Preserves strings starting with `'0'`, strips spaces and an optional leading `'+'`,
    /// then formats with [`Self::format`].
    pub fn format_phone(&self, phone: &str) -> String {
        if phone.is_empty() {
            return String::new();
        }
        if phone.starts_with('0') {
            return phone.to_string();
        }
        let cleaned: String = phone.chars().filter(|c| !c.is_whitespace()).collect();
        let digits = cleaned.strip_prefix('+').unwrap_or(&cleaned);
        self.format(FormatArgs {
            phone: digits,
            ..FormatArgs::default()
        })
        .formatted
    }

    /// Extract the calling code from a phone number (`Countries::ExtractPhoneCode`).
    pub fn extract_phone_code(&self, phone: &str) -> String {
        self.format(FormatArgs {
            phone,
            only_code: true,
            ..FormatArgs::default()
        })
        .code
    }

    /// Compute incomplete input digit groups (`Countries::Groups`).
    pub fn groups(&self, phone: &str) -> Vec<usize> {
        self.format(FormatArgs {
            phone,
            only_groups: true,
            incomplete: true,
            ..FormatArgs::default()
        })
        .groups
    }

    /// Serialize the current countries list and hash to Qt 5 `QDataStream` format
    /// matching `tdata/countries` in `countries_manager.cpp`.
    pub fn serialize_cache(&self) -> Vec<u8> {
        serialize_countries_cache(&CountriesCacheData {
            hash: self.hash,
            infos: self.list.clone(),
        })
    }

    /// Load countries list and hash from `tdata/countries` bytes (`countries_manager.cpp`).
    pub fn load_from_cache(&mut self, data: &[u8]) -> Result<(), DataStreamError> {
        let cache = deserialize_countries_cache(data)?;
        self.hash = cache.hash;
        self.set_list(cache.infos);
        Ok(())
    }

    /// Load cached countries from `<tdata>/countries` via [`tg_storage::LocalStorage`]
    /// (`Countries::Manager::read` in `countries/countries_manager.cpp`).
    ///
    /// Returns `Ok(true)` if a cache file existed and was loaded, or `Ok(false)` if absent.
    pub fn load_from_local_storage(
        &mut self,
        local: &tg_storage::LocalStorage,
    ) -> Result<bool, tg_storage::StorageError> {
        let Some(bytes) = local.read_countries_cache()? else {
            return Ok(false);
        };
        self.load_from_cache(&bytes)?;
        Ok(true)
    }

    /// Persist the current countries list and hash to `<tdata>/countries` via
    /// [`tg_storage::LocalStorage`] (`Countries::Manager::write` in `countries/countries_manager.cpp`).
    pub fn save_to_local_storage(
        &self,
        local: &tg_storage::LocalStorage,
    ) -> Result<(), tg_storage::StorageError> {
        local.write_countries_cache(&self.serialize_cache())
    }

    fn rebuild_indices(&mut self) {
        self.by_code.clear();
        self.by_iso2.clear();
        self.by_code.reserve(self.list.len());
        self.by_iso2.reserve(self.list.len());
        for (idx, entry) in self.list.iter().enumerate() {
            for code in &entry.codes {
                self.by_code.insert(code.calling_code.clone(), idx);
            }
            self.by_iso2.insert(entry.iso2.clone(), idx);
        }
    }
}

/// Serialize [`CountriesCacheData`] in the exact nested `QDataStream` format used by `countries_manager.cpp`.
pub fn serialize_countries_cache(data: &CountriesCacheData) -> Vec<u8> {
    let mut inner = QDataStreamWriter::with_capacity(data.infos.len() * 64);
    inner.write_i32(data.hash);
    inner.write_i32(data.infos.len() as i32);
    for info in &data.infos {
        let mut info_w = QDataStreamWriter::with_capacity(64);
        info_w.write_qstring(&info.name);
        info_w.write_qstring(&info.iso2);
        info_w.write_qstring(&info.alternative_name);
        info_w.write_bool(info.is_hidden);
        info_w.write_i32(info.codes.len() as i32);
        for code in &info.codes {
            let mut code_w = QDataStreamWriter::with_capacity(32);
            code_w.write_qstring(&code.calling_code);
            code_w.write_i32(code.prefixes.len() as i32);
            code_w.write_i32(code.patterns.len() as i32);
            for prefix in &code.prefixes {
                code_w.write_qstring(prefix);
            }
            for pattern in &code.patterns {
                code_w.write_qstring(pattern);
            }
            info_w.write_byte_array(&code_w.into_bytes());
        }
        inner.write_byte_array(&info_w.into_bytes());
    }
    let inner_bytes = inner.into_bytes();
    // Manager::write wraps the serialized QByteArray in an outer QDataStream << QByteArray.
    let mut outer = QDataStreamWriter::with_capacity(inner_bytes.len() + 4);
    outer.write_byte_array(&inner_bytes);
    outer.into_bytes()
}

/// Deserialize [`CountriesCacheData`] from bytes written by [`serialize_countries_cache`] or tdesktop.
pub fn deserialize_countries_cache(data: &[u8]) -> Result<CountriesCacheData, DataStreamError> {
    let mut outer = QDataStreamReader::new(data);
    let inner_bytes = outer.read_byte_array()?;
    let mut reader = QDataStreamReader::new(&inner_bytes);
    let hash = reader.read_i32()?;
    let infos_count = reader.read_i32()?;
    if !(0..=10_000).contains(&infos_count) {
        return Err(DataStreamError::InvalidLength(i64::from(infos_count)));
    }
    let mut infos = Vec::with_capacity(infos_count as usize);
    for _ in 0..infos_count {
        let info_bytes = reader.read_byte_array()?;
        let mut ir = QDataStreamReader::new(&info_bytes);
        let name = ir.read_qstring()?;
        let iso2 = ir.read_qstring()?;
        let alternative_name = ir.read_qstring()?;
        let is_hidden = ir.read_bool()?;
        let codes_count = ir.read_i32()?;
        if !(0..=1_000).contains(&codes_count) {
            return Err(DataStreamError::InvalidLength(i64::from(codes_count)));
        }
        let mut codes = Vec::with_capacity(codes_count as usize);
        for _ in 0..codes_count {
            let code_bytes = ir.read_byte_array()?;
            let mut cr = QDataStreamReader::new(&code_bytes);
            let calling_code = cr.read_qstring()?;
            let prefixes_count = cr.read_i32()?;
            let patterns_count = cr.read_i32()?;
            if !(0..=10_000).contains(&prefixes_count) {
                return Err(DataStreamError::InvalidLength(i64::from(prefixes_count)));
            }
            if !(0..=10_000).contains(&patterns_count) {
                return Err(DataStreamError::InvalidLength(i64::from(patterns_count)));
            }
            let mut prefixes = Vec::with_capacity(prefixes_count as usize);
            for _ in 0..prefixes_count {
                prefixes.push(cr.read_qstring()?);
            }
            let mut patterns = Vec::with_capacity(patterns_count as usize);
            for _ in 0..patterns_count {
                patterns.push(cr.read_qstring()?);
            }
            codes.push(CallingCodeInfo {
                calling_code,
                prefixes,
                patterns,
            });
        }
        infos.push(CountryInfo {
            name,
            iso2,
            alternative_name,
            codes,
            is_hidden,
        });
    }
    Ok(CountriesCacheData { hash, infos })
}

// ============================================================================
// Global singleton (`Countries::Instance`)
// ============================================================================

static GLOBAL_COUNTRIES: OnceLock<RwLock<CountriesInstance>> = OnceLock::new();

/// Access the process-wide [`CountriesInstance`] singleton (`Countries::Instance`).
pub fn global_countries() -> &'static RwLock<CountriesInstance> {
    GLOBAL_COUNTRIES.get_or_init(|| RwLock::new(CountriesInstance::new()))
}

/// Format a phone number using the global [`CountriesInstance`] (`Ui::FormatPhone`).
pub fn format_phone(phone: &str) -> String {
    let guard = global_countries()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.format_phone(phone)
}

/// Extract the calling code from `phone` using the global [`CountriesInstance`].
pub fn extract_phone_code(phone: &str) -> String {
    let guard = global_countries()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.extract_phone_code(phone)
}

/// Compute incomplete digit groups for `phone` using the global [`CountriesInstance`].
pub fn phone_groups(phone: &str) -> Vec<usize> {
    let guard = global_countries()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.groups(phone)
}

// ============================================================================
// Built-in 231-country FallbackList (exact port of countries_instance.cpp)
// ============================================================================

struct RawCountry {
    name: &'static str,
    iso2: &'static str,
    alt: &'static str,
    code: &'static str,
    prefixes: &'static [&'static str],
    patterns: &'static [&'static str],
}

const FALLBACK_DATA: &[RawCountry] = &[
    RawCountry {
        name: "Andorra",
        iso2: "AD",
        alt: "",
        code: "376",
        prefixes: &[],
        patterns: &["XX XX XX"],
    },
    RawCountry {
        name: "United Arab Emirates",
        iso2: "AE",
        alt: "",
        code: "971",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Afghanistan",
        iso2: "AF",
        alt: "",
        code: "93",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "Antigua & Barbuda",
        iso2: "AG",
        alt: "",
        code: "1268",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Anguilla",
        iso2: "AI",
        alt: "",
        code: "1264",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Albania",
        iso2: "AL",
        alt: "",
        code: "355",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Armenia",
        iso2: "AM",
        alt: "",
        code: "374",
        prefixes: &[],
        patterns: &["XX XXX XXX"],
    },
    RawCountry {
        name: "Angola",
        iso2: "AO",
        alt: "",
        code: "244",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "Argentina",
        iso2: "AR",
        alt: "",
        code: "54",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "American Samoa",
        iso2: "AS",
        alt: "",
        code: "1684",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Austria",
        iso2: "AT",
        alt: "",
        code: "43",
        prefixes: &[],
        patterns: &["X XXXXXXXX"],
    },
    RawCountry {
        name: "Australia",
        iso2: "AU",
        alt: "",
        code: "61",
        prefixes: &[],
        patterns: &["X XXXX XXXX"],
    },
    RawCountry {
        name: "Aruba",
        iso2: "AW",
        alt: "",
        code: "297",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Azerbaijan",
        iso2: "AZ",
        alt: "",
        code: "994",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Bosnia & Herzegovina",
        iso2: "BA",
        alt: "",
        code: "387",
        prefixes: &[],
        patterns: &["XX XXX XXX"],
    },
    RawCountry {
        name: "Barbados",
        iso2: "BB",
        alt: "",
        code: "1246",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Bangladesh",
        iso2: "BD",
        alt: "",
        code: "880",
        prefixes: &[],
        patterns: &["XX XXX XXX"],
    },
    RawCountry {
        name: "Belgium",
        iso2: "BE",
        alt: "",
        code: "32",
        prefixes: &[],
        patterns: &["XXX XX XX XX"],
    },
    RawCountry {
        name: "Burkina Faso",
        iso2: "BF",
        alt: "",
        code: "226",
        prefixes: &[],
        patterns: &["XX XX XX XX"],
    },
    RawCountry {
        name: "Bulgaria",
        iso2: "BG",
        alt: "",
        code: "359",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Bahrain",
        iso2: "BH",
        alt: "",
        code: "973",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Burundi",
        iso2: "BI",
        alt: "",
        code: "257",
        prefixes: &[],
        patterns: &["XX XX XXXX"],
    },
    RawCountry {
        name: "Benin",
        iso2: "BJ",
        alt: "",
        code: "229",
        prefixes: &[],
        patterns: &["XX XXX XXX"],
    },
    RawCountry {
        name: "Bermuda",
        iso2: "BM",
        alt: "",
        code: "1441",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Brunei Darussalam",
        iso2: "BN",
        alt: "",
        code: "673",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Bolivia",
        iso2: "BO",
        alt: "",
        code: "591",
        prefixes: &[],
        patterns: &["X XXX XXXX"],
    },
    RawCountry {
        name: "Bonaire, Sint Eustatius & Saba",
        iso2: "BQ",
        alt: "",
        code: "599",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Brazil",
        iso2: "BR",
        alt: "",
        code: "55",
        prefixes: &[],
        patterns: &["XX XXXXX XXXX"],
    },
    RawCountry {
        name: "Bahamas",
        iso2: "BS",
        alt: "",
        code: "1242",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Bhutan",
        iso2: "BT",
        alt: "",
        code: "975",
        prefixes: &[],
        patterns: &["XX XXX XXX"],
    },
    RawCountry {
        name: "Botswana",
        iso2: "BW",
        alt: "",
        code: "267",
        prefixes: &[],
        patterns: &["XX XXX XXX"],
    },
    RawCountry {
        name: "Belarus",
        iso2: "BY",
        alt: "",
        code: "375",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Belize",
        iso2: "BZ",
        alt: "",
        code: "501",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Canada",
        iso2: "CA",
        alt: "",
        code: "1",
        prefixes: &["403"],
        patterns: &["XXX XXX XXXX"],
    },
    RawCountry {
        name: "Congo (Dem. Rep.)",
        iso2: "CD",
        alt: "",
        code: "243",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Central African Rep.",
        iso2: "CF",
        alt: "",
        code: "236",
        prefixes: &[],
        patterns: &["XX XX XX XX"],
    },
    RawCountry {
        name: "Congo (Rep.)",
        iso2: "CG",
        alt: "",
        code: "242",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Switzerland",
        iso2: "CH",
        alt: "",
        code: "41",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Côte d'Ivoire",
        iso2: "CI",
        alt: "",
        code: "225",
        prefixes: &[],
        patterns: &["XX XX XX XXXX"],
    },
    RawCountry {
        name: "Cook Islands",
        iso2: "CK",
        alt: "",
        code: "682",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Chile",
        iso2: "CL",
        alt: "",
        code: "56",
        prefixes: &[],
        patterns: &["X XXXX XXXX"],
    },
    RawCountry {
        name: "Cameroon",
        iso2: "CM",
        alt: "",
        code: "237",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "China",
        iso2: "CN",
        alt: "",
        code: "86",
        prefixes: &[],
        patterns: &["XXX XXXX XXXX"],
    },
    RawCountry {
        name: "Colombia",
        iso2: "CO",
        alt: "",
        code: "57",
        prefixes: &[],
        patterns: &["XXX XXX XXXX"],
    },
    RawCountry {
        name: "Costa Rica",
        iso2: "CR",
        alt: "",
        code: "506",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Cuba",
        iso2: "CU",
        alt: "",
        code: "53",
        prefixes: &[],
        patterns: &["X XXX XXXX"],
    },
    RawCountry {
        name: "Cape Verde",
        iso2: "CV",
        alt: "",
        code: "238",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Curaçao",
        iso2: "CW",
        alt: "",
        code: "599",
        prefixes: &["9"],
        patterns: &[],
    },
    RawCountry {
        name: "Cyprus",
        iso2: "CY",
        alt: "",
        code: "357",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Czech Republic",
        iso2: "CZ",
        alt: "",
        code: "420",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "Germany",
        iso2: "DE",
        alt: "",
        code: "49",
        prefixes: &[],
        patterns: &["XXXX XXXXXXX"],
    },
    RawCountry {
        name: "Djibouti",
        iso2: "DJ",
        alt: "",
        code: "253",
        prefixes: &[],
        patterns: &["XX XX XX XX"],
    },
    RawCountry {
        name: "Denmark",
        iso2: "DK",
        alt: "",
        code: "45",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Dominica",
        iso2: "DM",
        alt: "",
        code: "1767",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Dominican Rep.",
        iso2: "DO",
        alt: "",
        code: "1809",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Algeria",
        iso2: "DZ",
        alt: "",
        code: "213",
        prefixes: &[],
        patterns: &["XXX XX XX XX"],
    },
    RawCountry {
        name: "Ecuador",
        iso2: "EC",
        alt: "",
        code: "593",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Estonia",
        iso2: "EE",
        alt: "",
        code: "372",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Egypt",
        iso2: "EG",
        alt: "",
        code: "20",
        prefixes: &[],
        patterns: &["XX XXXX XXXX"],
    },
    RawCountry {
        name: "Eritrea",
        iso2: "ER",
        alt: "",
        code: "291",
        prefixes: &[],
        patterns: &["X XXX XXX"],
    },
    RawCountry {
        name: "Spain",
        iso2: "ES",
        alt: "",
        code: "34",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "Ethiopia",
        iso2: "ET",
        alt: "",
        code: "251",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Finland",
        iso2: "FI",
        alt: "",
        code: "358",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Fiji",
        iso2: "FJ",
        alt: "",
        code: "679",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Falkland Islands",
        iso2: "FK",
        alt: "",
        code: "500",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Micronesia",
        iso2: "FM",
        alt: "",
        code: "691",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Faroe Islands",
        iso2: "FO",
        alt: "",
        code: "298",
        prefixes: &[],
        patterns: &["XXX XXX"],
    },
    RawCountry {
        name: "France",
        iso2: "FR",
        alt: "",
        code: "33",
        prefixes: &[],
        patterns: &["X XX XX XX XX"],
    },
    RawCountry {
        name: "Gabon",
        iso2: "GA",
        alt: "",
        code: "241",
        prefixes: &[],
        patterns: &["X XX XX XX"],
    },
    RawCountry {
        name: "United Kingdom",
        iso2: "GB",
        alt: "",
        code: "44",
        prefixes: &[],
        patterns: &["XXXX XXXXXX"],
    },
    RawCountry {
        name: "Grenada",
        iso2: "GD",
        alt: "",
        code: "1473",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Georgia",
        iso2: "GE",
        alt: "",
        code: "995",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "French Guiana",
        iso2: "GF",
        alt: "",
        code: "594",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Ghana",
        iso2: "GH",
        alt: "",
        code: "233",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Gibraltar",
        iso2: "GI",
        alt: "",
        code: "350",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Greenland",
        iso2: "GL",
        alt: "",
        code: "299",
        prefixes: &[],
        patterns: &["XXX XXX"],
    },
    RawCountry {
        name: "Gambia",
        iso2: "GM",
        alt: "",
        code: "220",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Guinea",
        iso2: "GN",
        alt: "",
        code: "224",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "Guadeloupe",
        iso2: "GP",
        alt: "",
        code: "590",
        prefixes: &[],
        patterns: &["XXX XX XX XX"],
    },
    RawCountry {
        name: "Equatorial Guinea",
        iso2: "GQ",
        alt: "",
        code: "240",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "Greece",
        iso2: "GR",
        alt: "",
        code: "30",
        prefixes: &[],
        patterns: &["XXX XXX XXXX"],
    },
    RawCountry {
        name: "Guatemala",
        iso2: "GT",
        alt: "",
        code: "502",
        prefixes: &[],
        patterns: &["X XXX XXXX"],
    },
    RawCountry {
        name: "Guam",
        iso2: "GU",
        alt: "",
        code: "1671",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Guinea-Bissau",
        iso2: "GW",
        alt: "",
        code: "245",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Guyana",
        iso2: "GY",
        alt: "",
        code: "592",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Hong Kong",
        iso2: "HK",
        alt: "",
        code: "852",
        prefixes: &[],
        patterns: &["X XXX XXXX"],
    },
    RawCountry {
        name: "Honduras",
        iso2: "HN",
        alt: "",
        code: "504",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Croatia",
        iso2: "HR",
        alt: "",
        code: "385",
        prefixes: &[],
        patterns: &["XX XXX XXX"],
    },
    RawCountry {
        name: "Haiti",
        iso2: "HT",
        alt: "",
        code: "509",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Hungary",
        iso2: "HU",
        alt: "",
        code: "36",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "Indonesia",
        iso2: "ID",
        alt: "",
        code: "62",
        prefixes: &[],
        patterns: &["XXX XXXXXX"],
    },
    RawCountry {
        name: "Ireland",
        iso2: "IE",
        alt: "",
        code: "353",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Israel",
        iso2: "IL",
        alt: "",
        code: "972",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "India",
        iso2: "IN",
        alt: "",
        code: "91",
        prefixes: &[],
        patterns: &["XXXXX XXXXX"],
    },
    RawCountry {
        name: "Diego Garcia",
        iso2: "IO",
        alt: "",
        code: "246",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Iraq",
        iso2: "IQ",
        alt: "",
        code: "964",
        prefixes: &[],
        patterns: &["XXX XXX XXXX"],
    },
    RawCountry {
        name: "Iran",
        iso2: "IR",
        alt: "",
        code: "98",
        prefixes: &[],
        patterns: &["XXX XXX XXXX"],
    },
    RawCountry {
        name: "Iceland",
        iso2: "IS",
        alt: "",
        code: "354",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Italy",
        iso2: "IT",
        alt: "",
        code: "39",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "Jamaica",
        iso2: "JM",
        alt: "",
        code: "1876",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Jordan",
        iso2: "JO",
        alt: "",
        code: "962",
        prefixes: &[],
        patterns: &["X XXXX XXXX"],
    },
    RawCountry {
        name: "Japan",
        iso2: "JP",
        alt: "",
        code: "81",
        prefixes: &[],
        patterns: &["XX XXXX XXXX"],
    },
    RawCountry {
        name: "Kenya",
        iso2: "KE",
        alt: "",
        code: "254",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "Kyrgyzstan",
        iso2: "KG",
        alt: "",
        code: "996",
        prefixes: &[],
        patterns: &["XXX XXXXXX"],
    },
    RawCountry {
        name: "Cambodia",
        iso2: "KH",
        alt: "",
        code: "855",
        prefixes: &[],
        patterns: &["XX XXX XXX"],
    },
    RawCountry {
        name: "Kiribati",
        iso2: "KI",
        alt: "",
        code: "686",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Comoros",
        iso2: "KM",
        alt: "",
        code: "269",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Saint Kitts & Nevis",
        iso2: "KN",
        alt: "",
        code: "1869",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "North Korea",
        iso2: "KP",
        alt: "",
        code: "850",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "South Korea",
        iso2: "KR",
        alt: "",
        code: "82",
        prefixes: &[],
        patterns: &["XX XXXX XXX"],
    },
    RawCountry {
        name: "Kuwait",
        iso2: "KW",
        alt: "",
        code: "965",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Cayman Islands",
        iso2: "KY",
        alt: "",
        code: "1345",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Kazakhstan",
        iso2: "KZ",
        alt: "",
        code: "7",
        prefixes: &["6"],
        patterns: &["XXX XXX XX XX"],
    },
    RawCountry {
        name: "Laos",
        iso2: "LA",
        alt: "",
        code: "856",
        prefixes: &[],
        patterns: &["XX XX XXX XXX"],
    },
    RawCountry {
        name: "Lebanon",
        iso2: "LB",
        alt: "",
        code: "961",
        prefixes: &[],
        patterns: &["XX XXX XXX"],
    },
    RawCountry {
        name: "Saint Lucia",
        iso2: "LC",
        alt: "",
        code: "1758",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Liechtenstein",
        iso2: "LI",
        alt: "",
        code: "423",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Sri Lanka",
        iso2: "LK",
        alt: "",
        code: "94",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Liberia",
        iso2: "LR",
        alt: "",
        code: "231",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Lesotho",
        iso2: "LS",
        alt: "",
        code: "266",
        prefixes: &[],
        patterns: &["XX XXX XXX"],
    },
    RawCountry {
        name: "Lithuania",
        iso2: "LT",
        alt: "",
        code: "370",
        prefixes: &[],
        patterns: &["XXX XXXXX"],
    },
    RawCountry {
        name: "Luxembourg",
        iso2: "LU",
        alt: "",
        code: "352",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "Latvia",
        iso2: "LV",
        alt: "",
        code: "371",
        prefixes: &[],
        patterns: &["XXX XXXXX"],
    },
    RawCountry {
        name: "Libya",
        iso2: "LY",
        alt: "",
        code: "218",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Morocco",
        iso2: "MA",
        alt: "",
        code: "212",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Monaco",
        iso2: "MC",
        alt: "",
        code: "377",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Moldova",
        iso2: "MD",
        alt: "",
        code: "373",
        prefixes: &[],
        patterns: &["XX XXX XXX"],
    },
    RawCountry {
        name: "Montenegro",
        iso2: "ME",
        alt: "",
        code: "382",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Madagascar",
        iso2: "MG",
        alt: "",
        code: "261",
        prefixes: &[],
        patterns: &["XX XX XXX XX"],
    },
    RawCountry {
        name: "Marshall Islands",
        iso2: "MH",
        alt: "",
        code: "692",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "North Macedonia",
        iso2: "MK",
        alt: "",
        code: "389",
        prefixes: &[],
        patterns: &["XX XXX XXX"],
    },
    RawCountry {
        name: "Mali",
        iso2: "ML",
        alt: "",
        code: "223",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Myanmar",
        iso2: "MM",
        alt: "",
        code: "95",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Mongolia",
        iso2: "MN",
        alt: "",
        code: "976",
        prefixes: &[],
        patterns: &["XX XX XXXX"],
    },
    RawCountry {
        name: "Macau",
        iso2: "MO",
        alt: "",
        code: "853",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Northern Mariana Islands",
        iso2: "MP",
        alt: "",
        code: "1670",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Martinique",
        iso2: "MQ",
        alt: "",
        code: "596",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Mauritania",
        iso2: "MR",
        alt: "",
        code: "222",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Montserrat",
        iso2: "MS",
        alt: "",
        code: "1664",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Malta",
        iso2: "MT",
        alt: "",
        code: "356",
        prefixes: &[],
        patterns: &["XX XX XX XX"],
    },
    RawCountry {
        name: "Mauritius",
        iso2: "MU",
        alt: "",
        code: "230",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Maldives",
        iso2: "MV",
        alt: "",
        code: "960",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Malawi",
        iso2: "MW",
        alt: "",
        code: "265",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Mexico",
        iso2: "MX",
        alt: "",
        code: "52",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Malaysia",
        iso2: "MY",
        alt: "",
        code: "60",
        prefixes: &[],
        patterns: &["XX XXXX XXXX"],
    },
    RawCountry {
        name: "Mozambique",
        iso2: "MZ",
        alt: "",
        code: "258",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Namibia",
        iso2: "NA",
        alt: "",
        code: "264",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "New Caledonia",
        iso2: "NC",
        alt: "",
        code: "687",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Niger",
        iso2: "NE",
        alt: "",
        code: "227",
        prefixes: &[],
        patterns: &["XX XX XX XX"],
    },
    RawCountry {
        name: "Norfolk Island",
        iso2: "NF",
        alt: "",
        code: "672",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Nigeria",
        iso2: "NG",
        alt: "",
        code: "234",
        prefixes: &[],
        patterns: &["XX XXXX XXXX"],
    },
    RawCountry {
        name: "Nicaragua",
        iso2: "NI",
        alt: "",
        code: "505",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Netherlands",
        iso2: "NL",
        alt: "",
        code: "31",
        prefixes: &[],
        patterns: &["X XX XX XX XX"],
    },
    RawCountry {
        name: "Norway",
        iso2: "NO",
        alt: "",
        code: "47",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Nepal",
        iso2: "NP",
        alt: "",
        code: "977",
        prefixes: &[],
        patterns: &["XX XXXX XXXX"],
    },
    RawCountry {
        name: "Nauru",
        iso2: "NR",
        alt: "",
        code: "674",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Niue",
        iso2: "NU",
        alt: "",
        code: "683",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "New Zealand",
        iso2: "NZ",
        alt: "",
        code: "64",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Oman",
        iso2: "OM",
        alt: "",
        code: "968",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Panama",
        iso2: "PA",
        alt: "",
        code: "507",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Peru",
        iso2: "PE",
        alt: "",
        code: "51",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "French Polynesia",
        iso2: "PF",
        alt: "",
        code: "689",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Papua New Guinea",
        iso2: "PG",
        alt: "",
        code: "675",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Philippines",
        iso2: "PH",
        alt: "",
        code: "63",
        prefixes: &[],
        patterns: &["XXX XXX XXXX"],
    },
    RawCountry {
        name: "Pakistan",
        iso2: "PK",
        alt: "",
        code: "92",
        prefixes: &[],
        patterns: &["XXX XXX XXXX"],
    },
    RawCountry {
        name: "Poland",
        iso2: "PL",
        alt: "",
        code: "48",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "Saint Pierre & Miquelon",
        iso2: "PM",
        alt: "",
        code: "508",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Puerto Rico",
        iso2: "PR",
        alt: "",
        code: "1787",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Palestine",
        iso2: "PS",
        alt: "",
        code: "970",
        prefixes: &[],
        patterns: &["XXX XX XXXX"],
    },
    RawCountry {
        name: "Portugal",
        iso2: "PT",
        alt: "",
        code: "351",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "Palau",
        iso2: "PW",
        alt: "",
        code: "680",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Paraguay",
        iso2: "PY",
        alt: "",
        code: "595",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "Qatar",
        iso2: "QA",
        alt: "",
        code: "974",
        prefixes: &[],
        patterns: &["XX XXX XXX"],
    },
    RawCountry {
        name: "Réunion",
        iso2: "RE",
        alt: "",
        code: "262",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "Romania",
        iso2: "RO",
        alt: "",
        code: "40",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "Serbia",
        iso2: "RS",
        alt: "",
        code: "381",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Russian Federation",
        iso2: "RU",
        alt: "",
        code: "7",
        prefixes: &[],
        patterns: &["XXX XXX XXXX"],
    },
    RawCountry {
        name: "Rwanda",
        iso2: "RW",
        alt: "",
        code: "250",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "Saudi Arabia",
        iso2: "SA",
        alt: "",
        code: "966",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Solomon Islands",
        iso2: "SB",
        alt: "",
        code: "677",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Seychelles",
        iso2: "SC",
        alt: "",
        code: "248",
        prefixes: &[],
        patterns: &["X XX XX XX"],
    },
    RawCountry {
        name: "Sudan",
        iso2: "SD",
        alt: "",
        code: "249",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Sweden",
        iso2: "SE",
        alt: "",
        code: "46",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Singapore",
        iso2: "SG",
        alt: "",
        code: "65",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Saint Helena",
        iso2: "SH",
        alt: "",
        code: "247",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Slovenia",
        iso2: "SI",
        alt: "",
        code: "386",
        prefixes: &[],
        patterns: &["XX XXX XXX"],
    },
    RawCountry {
        name: "Slovakia",
        iso2: "SK",
        alt: "",
        code: "421",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "Sierra Leone",
        iso2: "SL",
        alt: "",
        code: "232",
        prefixes: &[],
        patterns: &["XX XXX XXX"],
    },
    RawCountry {
        name: "San Marino",
        iso2: "SM",
        alt: "",
        code: "378",
        prefixes: &[],
        patterns: &["XXX XXX XXXX"],
    },
    RawCountry {
        name: "Senegal",
        iso2: "SN",
        alt: "",
        code: "221",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Somalia",
        iso2: "SO",
        alt: "",
        code: "252",
        prefixes: &[],
        patterns: &["XX XXX XXX"],
    },
    RawCountry {
        name: "Suriname",
        iso2: "SR",
        alt: "",
        code: "597",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "South Sudan",
        iso2: "SS",
        alt: "",
        code: "211",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "São Tomé & Príncipe",
        iso2: "ST",
        alt: "",
        code: "239",
        prefixes: &[],
        patterns: &["XX XXXXX"],
    },
    RawCountry {
        name: "El Salvador",
        iso2: "SV",
        alt: "",
        code: "503",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Sint Maarten",
        iso2: "SX",
        alt: "",
        code: "1721",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Syria",
        iso2: "SY",
        alt: "",
        code: "963",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "Eswatini",
        iso2: "SZ",
        alt: "",
        code: "268",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Turks & Caicos Islands",
        iso2: "TC",
        alt: "",
        code: "1649",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Chad",
        iso2: "TD",
        alt: "",
        code: "235",
        prefixes: &[],
        patterns: &["XX XX XX XX"],
    },
    RawCountry {
        name: "Togo",
        iso2: "TG",
        alt: "",
        code: "228",
        prefixes: &[],
        patterns: &["XX XXX XXX"],
    },
    RawCountry {
        name: "Thailand",
        iso2: "TH",
        alt: "",
        code: "66",
        prefixes: &[],
        patterns: &["X XXXX XXXX"],
    },
    RawCountry {
        name: "Tajikistan",
        iso2: "TJ",
        alt: "",
        code: "992",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Tokelau",
        iso2: "TK",
        alt: "",
        code: "690",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Timor-Leste",
        iso2: "TL",
        alt: "",
        code: "670",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Turkmenistan",
        iso2: "TM",
        alt: "",
        code: "993",
        prefixes: &[],
        patterns: &["XX XXXXXX"],
    },
    RawCountry {
        name: "Tunisia",
        iso2: "TN",
        alt: "",
        code: "216",
        prefixes: &[],
        patterns: &["XX XXX XXX"],
    },
    RawCountry {
        name: "Tonga",
        iso2: "TO",
        alt: "",
        code: "676",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Turkey",
        iso2: "TR",
        alt: "",
        code: "90",
        prefixes: &[],
        patterns: &["XXX XXX XXXX"],
    },
    RawCountry {
        name: "Trinidad & Tobago",
        iso2: "TT",
        alt: "",
        code: "1868",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Tuvalu",
        iso2: "TV",
        alt: "",
        code: "688",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Taiwan",
        iso2: "TW",
        alt: "",
        code: "886",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "Tanzania",
        iso2: "TZ",
        alt: "",
        code: "255",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Ukraine",
        iso2: "UA",
        alt: "",
        code: "380",
        prefixes: &[],
        patterns: &["XX XXX XX XX"],
    },
    RawCountry {
        name: "Uganda",
        iso2: "UG",
        alt: "",
        code: "256",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "USA",
        iso2: "US",
        alt: "United States of America",
        code: "1",
        prefixes: &[],
        patterns: &["XXX XXX XXXX"],
    },
    RawCountry {
        name: "Uruguay",
        iso2: "UY",
        alt: "",
        code: "598",
        prefixes: &[],
        patterns: &["X XXX XXXX"],
    },
    RawCountry {
        name: "Uzbekistan",
        iso2: "UZ",
        alt: "",
        code: "998",
        prefixes: &[],
        patterns: &["XX XXX XX XX"],
    },
    RawCountry {
        name: "Saint Vincent & the Grenadines",
        iso2: "VC",
        alt: "",
        code: "1784",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Venezuela",
        iso2: "VE",
        alt: "",
        code: "58",
        prefixes: &[],
        patterns: &["XXX XXX XXXX"],
    },
    RawCountry {
        name: "British Virgin Islands",
        iso2: "VG",
        alt: "",
        code: "1284",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "US Virgin Islands",
        iso2: "VI",
        alt: "",
        code: "1340",
        prefixes: &[],
        patterns: &["XXX XXXX"],
    },
    RawCountry {
        name: "Vietnam",
        iso2: "VN",
        alt: "",
        code: "84",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Vanuatu",
        iso2: "VU",
        alt: "",
        code: "678",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Wallis & Futuna",
        iso2: "WF",
        alt: "",
        code: "681",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Samoa",
        iso2: "WS",
        alt: "",
        code: "685",
        prefixes: &[],
        patterns: &[],
    },
    RawCountry {
        name: "Kosovo",
        iso2: "XK",
        alt: "",
        code: "383",
        prefixes: &[],
        patterns: &["XXXX XXXX"],
    },
    RawCountry {
        name: "Yemen",
        iso2: "YE",
        alt: "",
        code: "967",
        prefixes: &[],
        patterns: &["XXX XXX XXX"],
    },
    RawCountry {
        name: "South Africa",
        iso2: "ZA",
        alt: "",
        code: "27",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Zambia",
        iso2: "ZM",
        alt: "",
        code: "260",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
    RawCountry {
        name: "Zimbabwe",
        iso2: "ZW",
        alt: "",
        code: "263",
        prefixes: &[],
        patterns: &["XX XXX XXXX"],
    },
];

/// Return the built-in 231-country fallback list (`FallbackList` in `countries_instance.cpp`).
pub fn fallback_countries_list() -> Vec<CountryInfo> {
    FALLBACK_DATA
        .iter()
        .map(|raw| CountryInfo {
            name: raw.name.to_string(),
            iso2: raw.iso2.to_string(),
            alternative_name: raw.alt.to_string(),
            codes: vec![CallingCodeInfo {
                calling_code: raw.code.to_string(),
                prefixes: raw.prefixes.iter().map(|s| (*s).to_string()).collect(),
                patterns: raw.patterns.iter().map(|s| (*s).to_string()).collect(),
            }],
            is_hidden: false,
        })
        .collect()
}
