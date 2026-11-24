//! Identity, address, and contact field validation rules, plus scope computation.
//!
//! Port of `passport/passport_panel_controller.cpp` (`GetDocumentScheme`, `GetContactScheme`,
//! `LatinToNativeMap`, `NativeToLatinMap`, `SkipFieldCheck`) and
//! `passport/passport_form_view_controller.cpp` (`ValidateForm`, `ComputeScopes`,
//! `ComputeScopeRowReadyString`, `ComputeScopeRow`).

use std::collections::{BTreeMap, BTreeSet};

use crate::PassportError;
use crate::types::{
    FileType, PassportForm, PassportScope, PassportValue, ScopeRow, ScopeType, ValueMap, ValueType,
};

/// Maximum length for personal names (`kMaxNameSize` in `passport_panel_controller.cpp`).
pub const MAX_NAME_SIZE: usize = 255;
/// Maximum length for document numbers (`kMaxDocumentSize`).
pub const MAX_DOCUMENT_SIZE: usize = 24;
/// Maximum length for street / city / state fields (`kMaxStreetSize`).
pub const MAX_STREET_SIZE: usize = 64;
/// Minimum length for city names (`kMinCitySize`).
pub const MIN_CITY_SIZE: usize = 2;
/// Maximum length for city names (`kMaxCitySize`).
pub const MAX_CITY_SIZE: usize = 64;
/// Maximum length for postal codes (`kMaxPostcodeSize`).
pub const MAX_POSTCODE_SIZE: usize = 10;

/// Visibility mode for native-language name fields (`EditDocumentScheme::AdditionalVisibility`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdditionalVisibility {
    /// Residence country is not set yet; hide native name fields.
    Hidden,
    /// Residence country uses English (`language_code == "en"`); show only if server reports an error
    /// and auto-copy Latin names as fallback.
    OnlyIfError,
    /// Show native name fields explicitly.
    Shown,
}

/// Computes native-name field visibility from residence `country_code` and preferred `language_code`
/// (`result.additionalShown` in `passport_panel_controller.cpp`).
pub fn native_names_visibility(country_code: &str, language_code: &str) -> AdditionalVisibility {
    if country_code.is_empty() {
        AdditionalVisibility::Hidden
    } else if language_code == "en" {
        AdditionalVisibility::OnlyIfError
    } else {
        AdditionalVisibility::Shown
    }
}

/// Validation failure for an individual passport field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValidationError {
    /// Required field is empty or out of allowed length bounds.
    InvalidLength,
    /// Latin name contains disallowed characters (`tr::lng_passport_bad_name`).
    BadLatinName,
    /// Date does not match `DD.MM.YYYY`.
    InvalidDateFormat,
    /// Gender is neither `"male"` nor `"female"`.
    InvalidGender,
    /// Country ISO2 code is empty or invalid.
    InvalidCountry,
    /// Postcode does not match `^[a-zA-Z0-9\-]{2,10}$`.
    InvalidPostcode,
    /// Phone number does not match `^\d{2,12}$`.
    InvalidPhone,
    /// Email address is missing `@` or trailing `.` after `@`.
    InvalidEmail,
}

/// Maps a Latin name key (`first_name`, `middle_name`, `last_name`) to its native counterpart
/// (`LatinToNativeMap`).
pub fn latin_to_native_key(key: &str) -> Option<&'static str> {
    match key {
        "first_name" => Some("first_name_native"),
        "last_name" => Some("last_name_native"),
        "middle_name" => Some("middle_name_native"),
        _ => None,
    }
}

/// Maps a native name key (`*_name_native`) to its Latin counterpart (`NativeToLatinMap`).
pub fn native_to_latin_key(key: &str) -> Option<&'static str> {
    match key {
        "first_name_native" => Some("first_name"),
        "last_name_native" => Some("last_name"),
        "middle_name_native" => Some("middle_name"),
        _ => None,
    }
}

/// Returns `true` if server data errors for `key` on `value` should be ignored
/// (`SkipFieldCheck` in `passport_panel_controller.cpp`).
pub fn skip_field_check(value: &PassportValue, key: &str) -> bool {
    if value.value_type != ValueType::PersonalDetails {
        return false;
    }
    if value.native_names {
        latin_to_native_key(key).is_some()
    } else {
        native_to_latin_key(key).is_some()
    }
}

/// Validates a Latin name matching `^[a-zA-Z0-9.,/&'\- ]+$` with length `1..=255` (`NameValidate`).
pub fn validate_latin_name(value: &str) -> Result<(), FieldValidationError> {
    let len = value.chars().count();
    if len == 0 || len > MAX_NAME_SIZE {
        return Err(FieldValidationError::InvalidLength);
    }
    let valid = value.bytes().all(|b| {
        b.is_ascii_alphanumeric() || matches!(b, b'.' | b',' | b'/' | b'&' | b'-' | b'\'' | b' ')
    });
    if !valid {
        return Err(FieldValidationError::BadLatinName);
    }
    Ok(())
}

/// Validates an optional Latin name (`middle_name`: empty or valid Latin name).
pub fn validate_latin_name_or_empty(value: &str) -> Result<(), FieldValidationError> {
    if value.is_empty() {
        Ok(())
    } else {
        validate_latin_name(value)
    }
}

/// Validates a required native-script name (`1..=255` characters, `NativeNameValidate`).
pub fn validate_native_name(value: &str) -> Result<(), FieldValidationError> {
    let len = value.chars().count();
    if (1..=MAX_NAME_SIZE).contains(&len) {
        Ok(())
    } else {
        Err(FieldValidationError::InvalidLength)
    }
}

/// Validates an optional native-script name (`0..=255` characters, `NativeNameOrEmptyValidate`).
pub fn validate_native_name_or_empty(value: &str) -> Result<(), FieldValidationError> {
    if value.chars().count() <= MAX_NAME_SIZE {
        Ok(())
    } else {
        Err(FieldValidationError::InvalidLength)
    }
}

/// Validates a document number (`1..=24` characters, `DocumentValidate`).
pub fn validate_document_no(value: &str) -> Result<(), FieldValidationError> {
    let len = value.chars().count();
    if (1..=MAX_DOCUMENT_SIZE).contains(&len) {
        Ok(())
    } else {
        Err(FieldValidationError::InvalidLength)
    }
}

/// Validates a passport date string matching `^\d{2}\.\d{2}\.\d{4}$` (`DateValidate`).
pub fn validate_date(value: &str) -> Result<(), FieldValidationError> {
    let b = value.as_bytes();
    if b.len() == 10
        && b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2] == b'.'
        && b[3].is_ascii_digit()
        && b[4].is_ascii_digit()
        && b[5] == b'.'
        && b[6].is_ascii_digit()
        && b[7].is_ascii_digit()
        && b[8].is_ascii_digit()
        && b[9].is_ascii_digit()
    {
        Ok(())
    } else {
        Err(FieldValidationError::InvalidDateFormat)
    }
}

/// Validates an optional passport date (`expiry_date`: empty or `DD.MM.YYYY`).
pub fn validate_date_or_empty(value: &str) -> Result<(), FieldValidationError> {
    if value.is_empty() {
        Ok(())
    } else {
        validate_date(value)
    }
}

/// Validates gender (`"male"` or `"female"`, `GenderValidate`).
pub fn validate_gender(value: &str) -> Result<(), FieldValidationError> {
    if value == "male" || value == "female" {
        Ok(())
    } else {
        Err(FieldValidationError::InvalidGender)
    }
}

/// Validates a country ISO2 code (`CountryValidate`).
pub fn validate_country_code(value: &str) -> Result<(), FieldValidationError> {
    let b = value.as_bytes();
    if b.len() == 2 && b[0].is_ascii_alphabetic() && b[1].is_ascii_alphabetic() {
        Ok(())
    } else {
        Err(FieldValidationError::InvalidCountry)
    }
}

/// Validates street line 1 (`1..=64` characters, `StreetValidate`).
pub fn validate_street(value: &str) -> Result<(), FieldValidationError> {
    let len = value.chars().count();
    if (1..=MAX_STREET_SIZE).contains(&len) {
        Ok(())
    } else {
        Err(FieldValidationError::InvalidLength)
    }
}

/// Validates city name (`2..=64` characters, `CityValidate`).
pub fn validate_city(value: &str) -> Result<(), FieldValidationError> {
    let len = value.chars().count();
    if (MIN_CITY_SIZE..=MAX_CITY_SIZE).contains(&len) {
        Ok(())
    } else {
        Err(FieldValidationError::InvalidLength)
    }
}

/// Validates postal code matching `^[a-zA-Z0-9\-]{2,10}$` (`PostcodeValidate`).
pub fn validate_postcode(value: &str) -> Result<(), FieldValidationError> {
    let b = value.as_bytes();
    if (2..=MAX_POSTCODE_SIZE).contains(&b.len())
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
    {
        Ok(())
    } else {
        Err(FieldValidationError::InvalidPostcode)
    }
}

/// Post-processes a phone number by stripping all non-digit characters (`GetContactScheme:Phone`).
pub fn postprocess_phone(value: &str) -> String {
    value.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// Validates a post-processed phone number matching `^\d{2,12}$` (`GetContactScheme:Phone`).
pub fn validate_phone(value: &str) -> Result<(), FieldValidationError> {
    let b = value.as_bytes();
    if (2..=12).contains(&b.len()) && b.iter().all(u8::is_ascii_digit) {
        Ok(())
    } else {
        Err(FieldValidationError::InvalidPhone)
    }
}

/// Post-processes an email address by trimming leading and trailing whitespace (`GetContactScheme:Email`).
pub fn postprocess_email(value: &str) -> String {
    value.trim().to_string()
}

/// Validates an email address matching tdesktop's `(at > 0) && (dot > at)` check (`GetContactScheme:Email`).
pub fn validate_email(value: &str) -> Result<(), FieldValidationError> {
    let at = value.find('@');
    let dot = value.rfind('.');
    match (at, dot) {
        (Some(a), Some(d)) if a > 0 && d > a => Ok(()),
        _ => Err(FieldValidationError::InvalidEmail),
    }
}

/// Validates personal details fields (identity data part) and returns a map of invalid field keys.
pub fn validate_personal_details(
    map: &ValueMap,
    native_names: bool,
) -> BTreeMap<&'static str, FieldValidationError> {
    let mut errors = BTreeMap::new();
    let get = |k: &str| map.text(k);

    if let Err(e) = validate_latin_name(get("first_name")) {
        errors.insert("first_name", e);
    }
    if let Err(e) = validate_latin_name_or_empty(get("middle_name")) {
        errors.insert("middle_name", e);
    }
    if let Err(e) = validate_latin_name(get("last_name")) {
        errors.insert("last_name", e);
    }
    if let Err(e) = validate_date(get("birth_date")) {
        errors.insert("birth_date", e);
    }
    if let Err(e) = validate_gender(get("gender")) {
        errors.insert("gender", e);
    }
    if let Err(e) = validate_country_code(get("country_code")) {
        errors.insert("country_code", e);
    }
    if let Err(e) = validate_country_code(get("residence_country_code")) {
        errors.insert("residence_country_code", e);
    }

    if native_names {
        if let Err(e) = validate_native_name(get("first_name_native")) {
            errors.insert("first_name_native", e);
        }
        if let Err(e) = validate_native_name_or_empty(get("middle_name_native")) {
            errors.insert("middle_name_native", e);
        }
        if let Err(e) = validate_native_name(get("last_name_native")) {
            errors.insert("last_name_native", e);
        }
    }

    errors
}

/// Validates identity document fields (`document_no`, `expiry_date`).
pub fn validate_identity_document(map: &ValueMap) -> BTreeMap<&'static str, FieldValidationError> {
    let mut errors = BTreeMap::new();
    if let Err(e) = validate_document_no(map.text("document_no")) {
        errors.insert("document_no", e);
    }
    if let Err(e) = validate_date_or_empty(map.text("expiry_date")) {
        errors.insert("expiry_date", e);
    }
    errors
}

/// Validates residential address fields (`street_line1`, `city`, `country_code`, `post_code`).
///
/// `street_line2` and `state` are optional (no validator in `GetDocumentScheme:Address`),
/// matching `DontValidate` in `passport_panel_controller.cpp`.
pub fn validate_address(map: &ValueMap) -> BTreeMap<&'static str, FieldValidationError> {
    let mut errors = BTreeMap::new();
    if let Err(e) = validate_street(map.text("street_line1")) {
        errors.insert("street_line1", e);
    }
    if let Err(e) = validate_city(map.text("city")) {
        errors.insert("city", e);
    }
    if let Err(e) = validate_country_code(map.text("country_code")) {
        errors.insert("country_code", e);
    }
    if let Err(e) = validate_postcode(map.text("post_code")) {
        errors.insert("post_code", e);
    }
    errors
}

/// Validates a contact field (`Phone` or `Email`).
pub fn validate_contact(scope_type: ScopeType, value: &str) -> Result<(), FieldValidationError> {
    match scope_type {
        ScopeType::Phone => validate_phone(value),
        ScopeType::Email => validate_email(value),
        _ => Ok(()),
    }
}

/// Validates an entire [`PassportForm`] structure (`ValidateForm` in `passport_form_view_controller.cpp`).
pub fn validate_form(form: &PassportForm) -> Result<(), PassportError> {
    let mut seen = BTreeSet::new();
    for requested in &form.request {
        if requested.is_empty() {
            return Err(PassportError::InvalidForm(
                "empty types list in authorization form row".to_string(),
            ));
        }
        let scope_type = requested[0].scope_type();
        let owns_details = !matches!(scope_type, ScopeType::Identity | ScopeType::Address);
        if owns_details && requested.len() != 1 {
            return Err(PassportError::InvalidForm(
                "multi-type one_of row not allowed for details/contact scope".to_string(),
            ));
        }
        for &vt in requested {
            if !seen.insert(vt) {
                return Err(PassportError::InvalidForm(format!(
                    "duplicate value type {vt:?} in authorization form"
                )));
            }
        }
    }

    for (&vt, value) in &form.values {
        if value.selfie_required && !vt.can_require_selfie() {
            return Err(PassportError::InvalidForm(format!(
                "value type {vt:?} cannot require selfie"
            )));
        }
        if value.translation_required && !vt.can_require_translation() {
            return Err(PassportError::InvalidForm(format!(
                "value type {vt:?} cannot require translation"
            )));
        }
        if value.native_names && !vt.can_require_native_names() {
            return Err(PassportError::InvalidForm(format!(
                "value type {vt:?} cannot require native names"
            )));
        }
    }

    Ok(())
}

fn should_inline_details(request: &[Vec<ValueType>], into: ScopeType, details: ValueType) -> bool {
    let count = request
        .iter()
        .filter(|types| !types.is_empty() && types[0].scope_type() == into)
        .count();
    if count != 1 {
        return false;
    }
    request
        .iter()
        .any(|types| !types.is_empty() && types[0] == details)
}

fn inline_details_for(request: &[Vec<ValueType>], details: ValueType) -> bool {
    match details {
        ValueType::PersonalDetails => should_inline_details(request, ScopeType::Identity, details),
        ValueType::Address => should_inline_details(request, ScopeType::Address, details),
        _ => false,
    }
}

/// Computes the UI/submission scopes from a validated [`PassportForm`]
/// (`ComputeScopes` in `passport_form_view_controller.cpp`).
///
/// When both `PersonalDetails` and exactly one `Identity` document row are requested,
/// `PersonalDetails` is inlined into the `Identity` scope (and similarly `Address` into
/// a single `Address` document scope).
pub fn compute_scopes(form: &PassportForm) -> Vec<PassportScope> {
    let mut result = Vec::new();
    for requested in &form.request {
        if requested.is_empty() {
            continue;
        }
        let scope_type = requested[0].scope_type();
        let details_type = scope_type.details_type();
        let owns_details = !matches!(scope_type, ScopeType::Identity | ScopeType::Address);
        let inline_details = inline_details_for(&form.request, details_type);
        if owns_details && inline_details {
            continue;
        }
        let details = if owns_details || inline_details {
            Some(details_type)
        } else {
            None
        };
        let documents = if owns_details {
            Vec::new()
        } else {
            requested.clone()
        };
        result.push(PassportScope {
            scope_type,
            details,
            documents,
        });
    }
    result
}

/// Computes the ready summary string for a scope, or `None` if any required fields/scans are
/// incomplete or invalid (`ComputeScopeRowReadyString` in `passport_form_view_controller.cpp`).
pub fn compute_scope_row_ready(form: &PassportForm, scope: &PassportScope) -> Option<String> {
    match scope.scope_type {
        ScopeType::Phone | ScopeType::Email => {
            let details_type = scope.details?;
            let value = form.values.get(&details_type)?;
            let text = value.data.parsed.text("value");
            if text.is_empty() {
                None
            } else {
                Some(text.to_string())
            }
        }
        ScopeType::PersonalDetails
        | ScopeType::Identity
        | ScopeType::AddressDetails
        | ScopeType::Address => {
            let details_val = scope.details.and_then(|dt| form.values.get(&dt));
            let chosen_doc = scope
                .documents
                .iter()
                .filter_map(|dt| form.values.get(dt))
                .find(|v| v.scans_are_filled());

            if !scope.documents.is_empty() && chosen_doc.is_none() {
                return None;
            }

            let mut parts: Vec<String> = Vec::new();
            if let Some(doc) = chosen_doc
                && (scope.documents.len() > 1
                    || (details_val.is_none() && doc.value_type.scope_type() == ScopeType::Address))
            {
                parts.push(doc.value_type.display_name().to_string());
            }

            if let Some(details) = details_val {
                match scope.scope_type {
                    ScopeType::PersonalDetails | ScopeType::Identity => {
                        if !validate_personal_details(&details.data.parsed, details.native_names)
                            .is_empty()
                        {
                            return None;
                        }
                        let mut full_name =
                            details.data.parsed.text("first_name").trim().to_string();
                        for key in ["middle_name", "last_name"] {
                            let part = details.data.parsed.text(key).trim();
                            if !part.is_empty() {
                                if !full_name.is_empty() {
                                    full_name.push(' ');
                                }
                                full_name.push_str(part);
                            }
                        }
                        if !full_name.is_empty() {
                            parts.push(full_name);
                        }
                        for key in [
                            "birth_date",
                            "gender",
                            "country_code",
                            "residence_country_code",
                        ] {
                            let t = details.data.parsed.text(key).trim();
                            if !t.is_empty() {
                                parts.push(t.to_string());
                            }
                        }
                        if details.native_names {
                            let same_as_latin = [
                                ("first_name_native", "first_name"),
                                ("middle_name_native", "middle_name"),
                                ("last_name_native", "last_name"),
                            ]
                            .iter()
                            .all(|&(nat, lat)| {
                                details.data.parsed.text(nat) == details.data.parsed.text(lat)
                            });
                            if !same_as_latin {
                                let mut native_full = details
                                    .data
                                    .parsed
                                    .text("first_name_native")
                                    .trim()
                                    .to_string();
                                for key in ["middle_name_native", "last_name_native"] {
                                    let part = details.data.parsed.text(key).trim();
                                    if !part.is_empty() {
                                        if !native_full.is_empty() {
                                            native_full.push(' ');
                                        }
                                        native_full.push_str(part);
                                    }
                                }
                                if !native_full.is_empty() {
                                    parts.push(native_full);
                                }
                            }
                        }
                    }
                    ScopeType::AddressDetails | ScopeType::Address => {
                        if !validate_address(&details.data.parsed).is_empty() {
                            return None;
                        }
                        for key in [
                            "street_line1",
                            "street_line2",
                            "city",
                            "state",
                            "country_code",
                            "post_code",
                        ] {
                            let t = details.data.parsed.text(key).trim();
                            if !t.is_empty() {
                                parts.push(t.to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }

            if let Some(doc) = chosen_doc
                && matches!(
                    scope.scope_type,
                    ScopeType::PersonalDetails | ScopeType::Identity
                )
            {
                if !validate_identity_document(&doc.data.parsed).is_empty() {
                    return None;
                }
                for key in ["document_no", "expiry_date"] {
                    let t = doc.data.parsed.text(key).trim();
                    if !t.is_empty() {
                        parts.push(t.to_string());
                    }
                }
            }

            if parts.is_empty() {
                None
            } else {
                Some(parts.join(", "))
            }
        }
    }
}

/// Computes the ready status and first active error for a scope
/// (`ComputeScopeRow` in `passport_form_view_controller.cpp`).
pub fn compute_scope_row(form: &PassportForm, scope: &PassportScope) -> ScopeRow {
    let ready = compute_scope_row_ready(form, scope);
    let mut errors = Vec::new();

    let collect_value_errors = |value: &PassportValue, out: &mut Vec<String>| {
        if !value.error.is_empty() {
            out.push(value.error.clone());
        }
        for file_type in [FileType::Scan, FileType::Translation] {
            let missing = value.file_missing_error(file_type);
            if !missing.is_empty() {
                out.push(missing.to_string());
            }
            for f in value.files(file_type) {
                if !f.error.is_empty() {
                    out.push(f.error.clone());
                }
            }
        }
        for scan in value.special_scans.values() {
            if !scan.error.is_empty() {
                out.push(scan.error.clone());
            }
        }
        for field in value.data.parsed.fields.values() {
            if !field.error.is_empty() {
                out.push(field.error.clone());
            }
        }
    };

    let chosen_doc = scope
        .documents
        .iter()
        .filter_map(|dt| form.values.get(dt))
        .find(|v| v.scans_are_filled());

    if let Some(doc) = chosen_doc {
        collect_value_errors(doc, &mut errors);
    }
    if let Some(details) = scope.details.and_then(|dt| form.values.get(&dt)) {
        collect_value_errors(details, &mut errors);
    }

    ScopeRow {
        scope_type: scope.scope_type,
        ready,
        error: errors.into_iter().next(),
    }
}
