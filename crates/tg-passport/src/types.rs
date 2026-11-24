//! Telegram Passport data types, scopes, form model, and TL conversions.
//!
//! Port of `passport/passport_form_controller.h`, `passport/passport_form_controller.cpp`,
//! and `passport/passport_form_view_controller.h`.

use std::collections::BTreeMap;

use serde_json::Value as JsonValue;
use tg_data::{TimeId, UserId};
use tg_tl::{enums as tl_enums, types as tl_types};
use url::Url;

use crate::PassportError;

/// Maximum number of document scan files per value (`kDocumentScansLimit`).
pub const DOCUMENT_SCANS_LIMIT: usize = 20;
/// Maximum number of translation scan files per value (`kTranslationScansLimit`).
pub const TRANSLATION_SCANS_LIMIT: usize = 20;

/// Bitmask returned by [`PassportValue::what_not_filled`] when core scans are missing.
pub const NOT_FILLED_NOTHING: u32 = 0x100;
/// Bitmask returned by [`PassportValue::what_not_filled`] when required translation scans are missing.
pub const NOT_FILLED_NO_TRANSLATION: u32 = 0x010;
/// Bitmask returned by [`PassportValue::what_not_filled`] when required reverse-side or selfie scan is missing.
pub const NOT_FILLED_NO_SELFIE: u32 = 0x001;

/// Telegram Passport secure value type (`Passport::Value::Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ValueType {
    /// Personal details (name, birth date, gender, country, residence country).
    PersonalDetails,
    /// International passport.
    Passport,
    /// Driver license.
    DriverLicense,
    /// National identity card.
    IdentityCard,
    /// Internal passport.
    InternalPassport,
    /// Residential address details.
    Address,
    /// Utility bill (proof of address).
    UtilityBill,
    /// Bank statement (proof of address).
    BankStatement,
    /// Rental agreement (proof of address).
    RentalAgreement,
    /// Passport registration (proof of address).
    PassportRegistration,
    /// Temporary registration (proof of address).
    TemporaryRegistration,
    /// Verified phone number (plain text).
    Phone,
    /// Verified email address (plain text).
    Email,
}

impl ValueType {
    /// Converts from a generated TL [`tl_enums::SecureValueType`].
    pub const fn from_tl(tl: &tl_enums::SecureValueType) -> Self {
        match tl {
            tl_enums::SecureValueType::PersonalDetails => Self::PersonalDetails,
            tl_enums::SecureValueType::Passport => Self::Passport,
            tl_enums::SecureValueType::DriverLicense => Self::DriverLicense,
            tl_enums::SecureValueType::IdentityCard => Self::IdentityCard,
            tl_enums::SecureValueType::InternalPassport => Self::InternalPassport,
            tl_enums::SecureValueType::Address => Self::Address,
            tl_enums::SecureValueType::UtilityBill => Self::UtilityBill,
            tl_enums::SecureValueType::BankStatement => Self::BankStatement,
            tl_enums::SecureValueType::RentalAgreement => Self::RentalAgreement,
            tl_enums::SecureValueType::PassportRegistration => Self::PassportRegistration,
            tl_enums::SecureValueType::TemporaryRegistration => Self::TemporaryRegistration,
            tl_enums::SecureValueType::Phone => Self::Phone,
            tl_enums::SecureValueType::Email => Self::Email,
        }
    }

    /// Converts to a generated TL [`tl_enums::SecureValueType`].
    pub const fn to_tl(self) -> tl_enums::SecureValueType {
        match self {
            Self::PersonalDetails => tl_enums::SecureValueType::PersonalDetails,
            Self::Passport => tl_enums::SecureValueType::Passport,
            Self::DriverLicense => tl_enums::SecureValueType::DriverLicense,
            Self::IdentityCard => tl_enums::SecureValueType::IdentityCard,
            Self::InternalPassport => tl_enums::SecureValueType::InternalPassport,
            Self::Address => tl_enums::SecureValueType::Address,
            Self::UtilityBill => tl_enums::SecureValueType::UtilityBill,
            Self::BankStatement => tl_enums::SecureValueType::BankStatement,
            Self::RentalAgreement => tl_enums::SecureValueType::RentalAgreement,
            Self::PassportRegistration => tl_enums::SecureValueType::PassportRegistration,
            Self::TemporaryRegistration => tl_enums::SecureValueType::TemporaryRegistration,
            Self::Phone => tl_enums::SecureValueType::Phone,
            Self::Email => tl_enums::SecureValueType::Email,
        }
    }

    /// JSON key inside `secure_data` credentials object (`ValueCredentialsKey`).
    ///
    /// Returns `None` for plain-text types (`Phone`, `Email`).
    pub const fn credentials_key(self) -> Option<&'static str> {
        match self {
            Self::PersonalDetails => Some("personal_details"),
            Self::Passport => Some("passport"),
            Self::DriverLicense => Some("driver_license"),
            Self::IdentityCard => Some("identity_card"),
            Self::InternalPassport => Some("internal_passport"),
            Self::Address => Some("address"),
            Self::UtilityBill => Some("utility_bill"),
            Self::BankStatement => Some("bank_statement"),
            Self::RentalAgreement => Some("rental_agreement"),
            Self::PassportRegistration => Some("passport_registration"),
            Self::TemporaryRegistration => Some("temporary_registration"),
            Self::Phone | Self::Email => None,
        }
    }

    /// Human-readable English document/value name (used in ready summary strings).
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::PersonalDetails => "Personal Details",
            Self::Passport => "Passport",
            Self::DriverLicense => "Driver License",
            Self::IdentityCard => "Identity Card",
            Self::InternalPassport => "Internal Passport",
            Self::Address => "Residential Address",
            Self::UtilityBill => "Utility Bill",
            Self::BankStatement => "Bank Statement",
            Self::RentalAgreement => "Rental Agreement",
            Self::PassportRegistration => "Passport Registration",
            Self::TemporaryRegistration => "Temporary Registration",
            Self::Phone => "Phone Number",
            Self::Email => "Email",
        }
    }

    /// Maps a [`ValueType`] to its [`ScopeType`] (`ScopeTypeForValueType`).
    pub const fn scope_type(self) -> ScopeType {
        match self {
            Self::PersonalDetails => ScopeType::PersonalDetails,
            Self::Passport | Self::DriverLicense | Self::IdentityCard | Self::InternalPassport => {
                ScopeType::Identity
            }
            Self::Address => ScopeType::AddressDetails,
            Self::UtilityBill
            | Self::BankStatement
            | Self::RentalAgreement
            | Self::PassportRegistration
            | Self::TemporaryRegistration => ScopeType::Address,
            Self::Phone => ScopeType::Phone,
            Self::Email => ScopeType::Email,
        }
    }

    /// Returns `true` if this value is end-to-end encrypted (all types except `Phone`/`Email`).
    pub const fn is_encrypted(self) -> bool {
        !matches!(self, Self::Phone | Self::Email)
    }

    /// Returns `true` if server validation errors can apply to this type (`CanHaveErrors`).
    pub const fn can_have_errors(self) -> bool {
        !matches!(self, Self::Phone | Self::Email)
    }

    /// Returns `true` if `selfie_required` is legal for this type (`CanRequireSelfie`).
    pub const fn can_require_selfie(self) -> bool {
        matches!(self.scope_type(), ScopeType::Identity | ScopeType::Address)
    }

    /// Returns `true` if `translation_required` is legal for this type (`CanRequireTranslation`).
    pub const fn can_require_translation(self) -> bool {
        matches!(self.scope_type(), ScopeType::Identity | ScopeType::Address)
    }

    /// Returns `true` if `native_names` is legal for this type (`CanRequireNativeNames`).
    pub const fn can_require_native_names(self) -> bool {
        matches!(self, Self::PersonalDetails)
    }
}

/// High-level authorization form scope grouping (`Passport::Scope::Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ScopeType {
    /// Standalone personal details (when not inlined into `Identity`).
    PersonalDetails,
    /// Identity document(s), optionally with inlined `PersonalDetails`.
    Identity,
    /// Standalone residential address details (when not inlined into `Address`).
    AddressDetails,
    /// Address proof document(s), optionally with inlined `Address`.
    Address,
    /// Verified phone number.
    Phone,
    /// Verified email address.
    Email,
}

impl ScopeType {
    /// Associated details [`ValueType`] for this scope (`DetailsTypeForScopeType`).
    pub const fn details_type(self) -> ValueType {
        match self {
            Self::PersonalDetails | Self::Identity => ValueType::PersonalDetails,
            Self::AddressDetails | Self::Address => ValueType::Address,
            Self::Phone => ValueType::Phone,
            Self::Email => ValueType::Email,
        }
    }
}

/// Kind of attached scan/photo on a [`PassportValue`] (`Passport::FileType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FileType {
    /// Regular multi-page proof-of-address scans (`files`).
    Scan,
    /// Certified translation scans (`translation`).
    Translation,
    /// Front side of identity document (`front_side`).
    FrontSide,
    /// Reverse side of driver license or identity card (`reverse_side`).
    ReverseSide,
    /// Selfie holding identity/address document (`selfie`).
    Selfie,
}

impl FileType {
    /// JSON key for special scans inside `secure_data` (`SpecialScanCredentialsKey`).
    pub const fn special_scan_credentials_key(self) -> Option<&'static str> {
        match self {
            Self::FrontSide => Some("front_side"),
            Self::ReverseSide => Some("reverse_side"),
            Self::Selfie => Some("selfie"),
            Self::Scan | Self::Translation => None,
        }
    }
}

/// File upload/download progress status (`Passport::LoadStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoadStatus {
    /// Transfer completed (or idle).
    #[default]
    Done,
    /// Transfer in progress at byte `offset`.
    InProgress {
        /// Bytes transferred so far.
        offset: usize,
    },
    /// Transfer failed.
    Failed,
}

/// Encrypted file metadata stored on Telegram servers (`Passport::File`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PassportFile {
    /// File ID.
    pub id: i64,
    /// Server access hash (0 for freshly uploaded unsaved files).
    pub access_hash: i64,
    /// Encrypted file size in bytes.
    pub size: i64,
    /// Data center ID hosting the file.
    pub dc_id: i32,
    /// Upload Unix timestamp.
    pub date: TimeId,
    /// 32-byte SHA-256 hash of the padded plaintext file.
    pub hash: Vec<u8>,
    /// Decrypted 32-byte per-file secret (empty until unlocked).
    pub secret: Vec<u8>,
    /// Master-secret-encrypted 32-byte per-file secret from server.
    pub encrypted_secret: Vec<u8>,
    /// Download status.
    pub download_status: LoadStatus,
    /// Server-reported validation error for this file.
    pub error: String,
}

impl PassportFile {
    /// Parses a TL [`tl_enums::SecureFile`] (`FormController::parseFile`).
    pub fn from_tl(file: &tl_enums::SecureFile) -> Option<Self> {
        match file {
            tl_enums::SecureFile::Empty => None,
            tl_enums::SecureFile::SecureFile(f) => Some(Self {
                id: f.id,
                access_hash: f.access_hash,
                size: f.size,
                dc_id: f.dc_id,
                date: f.date,
                hash: f.file_hash.clone(),
                secret: Vec::new(),
                encrypted_secret: f.secret.clone(),
                download_status: LoadStatus::Done,
                error: String::new(),
            }),
        }
    }
}

/// Metadata for a locally encrypted scan pending upload/save (`Passport::UploadScanData`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UploadScanData {
    /// Client-generated file ID for `upload.saveFilePart`.
    pub file_id: i64,
    /// Number of uploaded file parts.
    pub parts_count: i32,
    /// Lowercase 32-char hex MD5 checksum of encrypted `bytes`.
    pub md5_checksum: String,
    /// SHA-256 hash of padded plaintext.
    pub hash: Vec<u8>,
    /// Encrypted bytes to upload.
    pub bytes: Vec<u8>,
    /// Current upload status.
    pub status: LoadStatus,
}

/// Editable scan slot inside a value edit session (`Passport::EditFile`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditFile {
    /// Scan slot kind.
    pub file_type: FileType,
    /// Underlying file metadata.
    pub fields: PassportFile,
    /// Upload state if newly added during this edit session.
    pub upload_data: Option<UploadScanData>,
    /// Whether the user marked this scan for deletion.
    pub deleted: bool,
}

impl EditFile {
    /// Returns `true` if this file was added or deleted compared to the saved state.
    pub fn is_changed(&self) -> bool {
        if self.upload_data.is_some() {
            !self.deleted
        } else {
            self.deleted
        }
    }

    /// Converts this edit file into a TL [`tl_enums::InputSecureFile`] for `account.saveSecureValue`.
    pub fn to_input_tl(&self) -> tl_enums::InputSecureFile {
        if let Some(ref upload) = self.upload_data {
            tl_enums::InputSecureFile::Uploaded(tl_types::InputSecureFileUploaded {
                id: self.fields.id,
                parts: upload.parts_count,
                md5_checksum: upload.md5_checksum.clone(),
                file_hash: self.fields.hash.clone(),
                secret: self.fields.encrypted_secret.clone(),
            })
        } else {
            tl_enums::InputSecureFile::InputSecureFile(tl_types::InputSecureFile {
                id: self.fields.id,
                access_hash: self.fields.access_hash,
            })
        }
    }
}

/// Single parsed text field with optional server validation error (`Passport::ValueField`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ValueField {
    /// Field text value.
    pub text: String,
    /// Server-supplied validation error text, if any.
    pub error: String,
}

/// Map of field keys to [`ValueField`] (`Passport::ValueMap`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ValueMap {
    /// Field entries keyed by Telegram Passport field name (e.g. `"first_name"`, `"city"`).
    pub fields: BTreeMap<String, ValueField>,
}

impl ValueMap {
    /// Creates a [`ValueMap`] from an iterator of `(key, value)` string pairs.
    pub fn from_texts<I, K, V>(iter: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let fields = iter
            .into_iter()
            .map(|(k, v)| {
                (
                    k.into(),
                    ValueField {
                        text: v.into(),
                        error: String::new(),
                    },
                )
            })
            .collect();
        Self { fields }
    }

    /// Returns the text for `key`, or `""` if absent.
    pub fn text(&self, key: &str) -> &str {
        self.fields.get(key).map(|f| f.text.as_str()).unwrap_or("")
    }

    /// Sets the text for `key`, clearing any previous error on that field.
    pub fn set_text(&mut self, key: impl Into<String>, text: impl Into<String>) {
        self.fields.insert(
            key.into(),
            ValueField {
                text: text.into(),
                error: String::new(),
            },
        );
    }

    /// Converts field texts into a plain `BTreeMap<String, String>` for JSON serialization (`GetTexts`).
    pub fn to_text_map(&self) -> BTreeMap<String, String> {
        self.fields
            .iter()
            .map(|(k, v)| (k.clone(), v.text.clone()))
            .collect()
    }
}

/// Encrypted and decrypted data buffers for a [`PassportValue`] (`Passport::ValueData`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ValueData {
    /// Raw AES-CBC ciphertext received from server (`secureData.data`).
    pub original: Vec<u8>,
    /// Decrypted 32-byte per-value data secret.
    pub secret: Vec<u8>,
    /// Decrypted parsed field map.
    pub parsed: ValueMap,
    /// SHA-256 hash of padded plaintext JSON (`secureData.data_hash`).
    pub hash: Vec<u8>,
    /// Master-secret-encrypted 32-byte data secret (`secureData.secret`).
    pub encrypted_secret: Vec<u8>,
    /// Staged field map during edit.
    pub parsed_in_edit: ValueMap,
    /// Staged SHA-256 data hash during save.
    pub hash_in_edit: Vec<u8>,
    /// Staged master-secret-encrypted data secret during save.
    pub encrypted_secret_in_edit: Vec<u8>,
}

/// Phone/email verification state for plain-text values (`Passport::Verification`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VerificationState {
    /// Server `phone_code_hash` for phone verification.
    pub phone_code_hash: String,
    /// Expected verification code length (`-1` for unknown length, `0` when inactive).
    pub code_length: i32,
    /// Fragment SMS URL if applicable.
    pub fragment_url: String,
    /// Last verification error message.
    pub error: String,
}

/// One Telegram Passport value (data + scans + scope requirements) (`Passport::Value`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassportValue {
    /// Value category.
    pub value_type: ValueType,
    /// Encrypted/parsed field payload.
    pub data: ValueData,
    /// Special scans (`FrontSide`, `ReverseSide`, `Selfie`).
    pub special_scans: BTreeMap<FileType, PassportFile>,
    /// General value-level error from server.
    pub error: String,
    /// Editable special scans.
    pub special_scans_in_edit: BTreeMap<FileType, EditFile>,
    /// Verification state for `Phone` / `Email`.
    pub verification: VerificationState,
    /// Server-provided value hash included in `account.acceptAuthorization` (`secureValue.hash`).
    pub submit_hash: Vec<u8>,
    /// Whether the bot requires a selfie for this value.
    pub selfie_required: bool,
    /// Whether the bot requires certified translations for this value.
    pub translation_required: bool,
    /// Whether the bot requires native-language names (only for `PersonalDetails`).
    pub native_names: bool,
    /// Proof-of-address scans (`files`).
    pub scans: Vec<PassportFile>,
    /// Translation scans (`translation`).
    pub translations: Vec<PassportFile>,
    /// Editable scans.
    pub scans_in_edit: Vec<EditFile>,
    /// Editable translations.
    pub translations_in_edit: Vec<EditFile>,
    /// Missing-scans error text.
    pub scan_missing_error: String,
    /// Missing-translations error text.
    pub translation_missing_error: String,
}

impl PassportValue {
    /// Creates an empty [`PassportValue`] for `value_type`.
    pub fn new(value_type: ValueType) -> Self {
        Self {
            value_type,
            data: ValueData::default(),
            special_scans: BTreeMap::new(),
            error: String::new(),
            special_scans_in_edit: BTreeMap::new(),
            verification: VerificationState::default(),
            submit_hash: Vec::new(),
            selfie_required: false,
            translation_required: false,
            native_names: false,
            scans: Vec::new(),
            translations: Vec::new(),
            scans_in_edit: Vec::new(),
            translations_in_edit: Vec::new(),
            scan_missing_error: String::new(),
            translation_missing_error: String::new(),
        }
    }

    /// Replaces value contents from `other` while preserving bot-requested scope flags
    /// (`Value::fillDataFrom`).
    pub fn fill_data_from(&mut self, mut other: Self) {
        other.selfie_required = self.selfie_required;
        other.translation_required = self.translation_required;
        other.native_names = self.native_names;
        *self = other;
    }

    /// Returns `true` if a special scan (`FrontSide`, `ReverseSide`, `Selfie`) is required
    /// (`Value::requiresSpecialScan`).
    pub const fn requires_special_scan(&self, file_type: FileType) -> bool {
        match file_type {
            FileType::FrontSide => matches!(
                self.value_type,
                ValueType::Passport
                    | ValueType::DriverLicense
                    | ValueType::IdentityCard
                    | ValueType::InternalPassport
            ),
            FileType::ReverseSide => {
                matches!(
                    self.value_type,
                    ValueType::DriverLicense | ValueType::IdentityCard
                )
            }
            FileType::Selfie => self.selfie_required,
            FileType::Scan | FileType::Translation => false,
        }
    }

    /// Returns `true` if scans of `file_type` are required (`Value::requiresScan`).
    pub const fn requires_scan(&self, file_type: FileType) -> bool {
        match file_type {
            FileType::Scan => matches!(
                self.value_type,
                ValueType::UtilityBill
                    | ValueType::BankStatement
                    | ValueType::RentalAgreement
                    | ValueType::PassportRegistration
                    | ValueType::TemporaryRegistration
            ),
            FileType::Translation => self.translation_required,
            FileType::FrontSide | FileType::ReverseSide | FileType::Selfie => {
                self.requires_special_scan(file_type)
            }
        }
    }

    /// Returns a bitmask of missing required scans (`Value::whatNotFilled`).
    pub fn what_not_filled(&self) -> u32 {
        let missing_special =
            |ft: FileType| self.requires_special_scan(ft) && !self.special_scans.contains_key(&ft);
        if self.requires_scan(FileType::Scan) && self.scans.is_empty() {
            return NOT_FILLED_NOTHING;
        }
        if missing_special(FileType::FrontSide) {
            return NOT_FILLED_NOTHING;
        }
        let mut result = 0u32;
        if self.requires_scan(FileType::Translation) && self.translations.is_empty() {
            result |= NOT_FILLED_NO_TRANSLATION;
        }
        if missing_special(FileType::ReverseSide) || missing_special(FileType::Selfie) {
            result |= NOT_FILLED_NO_SELFIE;
        }
        result
    }

    /// Returns `true` if all required scans for this value are present (`Value::scansAreFilled`).
    pub fn scans_are_filled(&self) -> bool {
        self.what_not_filled() == 0
    }

    /// Returns the list of files for `Scan` or `Translation`.
    pub fn files(&self, file_type: FileType) -> &[PassportFile] {
        match file_type {
            FileType::Scan => &self.scans,
            FileType::Translation => &self.translations,
            _ => &[],
        }
    }

    /// Returns a mutable reference to the list of files for `Scan` or `Translation`.
    pub fn files_mut(&mut self, file_type: FileType) -> Option<&mut Vec<PassportFile>> {
        match file_type {
            FileType::Scan => Some(&mut self.scans),
            FileType::Translation => Some(&mut self.translations),
            _ => None,
        }
    }

    /// Returns the missing-file error message for `Scan` or `Translation`.
    pub fn file_missing_error(&self, file_type: FileType) -> &str {
        match file_type {
            FileType::Scan => &self.scan_missing_error,
            FileType::Translation => &self.translation_missing_error,
            _ => "",
        }
    }

    /// Copies current saved scans and fields into edit buffers (`Value::saveInEdit`).
    pub fn save_in_edit(&mut self) {
        self.scans_in_edit = self
            .scans
            .iter()
            .cloned()
            .map(|fields| EditFile {
                file_type: FileType::Scan,
                fields,
                upload_data: None,
                deleted: false,
            })
            .collect();
        self.translations_in_edit = self
            .translations
            .iter()
            .cloned()
            .map(|fields| EditFile {
                file_type: FileType::Translation,
                fields,
                upload_data: None,
                deleted: false,
            })
            .collect();
        self.special_scans_in_edit = self
            .special_scans
            .iter()
            .map(|(&ft, fields)| {
                (
                    ft,
                    EditFile {
                        file_type: ft,
                        fields: fields.clone(),
                        upload_data: None,
                        deleted: false,
                    },
                )
            })
            .collect();
        self.data.parsed_in_edit = self.data.parsed.clone();
    }

    /// Clears staged edit buffers (`Value::clearEditData`).
    pub fn clear_edit_data(&mut self) {
        self.scans_in_edit.clear();
        self.translations_in_edit.clear();
        self.special_scans_in_edit.clear();
        self.data.encrypted_secret_in_edit.clear();
        self.data.hash_in_edit.clear();
        self.data.parsed_in_edit = ValueMap::default();
    }

    /// Returns `true` if another scan of `file_type` can be added without exceeding limits
    /// (`FormController::canAddScan`).
    pub fn can_add_scan(&self, file_type: FileType) -> bool {
        let limit = match file_type {
            FileType::Scan => DOCUMENT_SCANS_LIMIT,
            FileType::Translation => TRANSLATION_SCANS_LIMIT,
            _ => return true,
        };
        let list = match file_type {
            FileType::Scan => &self.scans_in_edit,
            FileType::Translation => &self.translations_in_edit,
            _ => return true,
        };
        let active = list.iter().filter(|f| !f.deleted).count();
        active < limit
    }

    /// Returns `true` if any staged scan or field differs from saved data (`Passport::ValueChanged`).
    pub fn has_changes(&self, candidate: &ValueMap) -> bool {
        if self.scans_in_edit.iter().any(EditFile::is_changed)
            || self.translations_in_edit.iter().any(EditFile::is_changed)
            || self
                .special_scans_in_edit
                .values()
                .any(EditFile::is_changed)
        {
            return true;
        }
        for (key, field) in &candidate.fields {
            match self.data.parsed.fields.get(key) {
                Some(existing) => {
                    if existing.text != field.text {
                        return true;
                    }
                }
                None => {
                    if !field.text.is_empty() {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Parses a TL [`tl_enums::SecureValue`] (`FormController::parseValue`).
    pub fn from_tl(tl: &tl_enums::SecureValue) -> Self {
        let tl_enums::SecureValue::SecureValue(v) = tl;
        let vt = ValueType::from_tl(&v.r#type);
        let mut result = Self::new(vt);
        result.submit_hash = v.hash.clone();
        if let Some(tl_enums::SecureData::SecureData(ref d)) = v.data {
            result.data.original = d.data.clone();
            result.data.hash = d.data_hash.clone();
            result.data.encrypted_secret = d.secret.clone();
        }
        if let Some(ref files) = v.files {
            result.scans = files.iter().filter_map(PassportFile::from_tl).collect();
        }
        if let Some(ref translations) = v.translation {
            result.translations = translations
                .iter()
                .filter_map(PassportFile::from_tl)
                .collect();
        }
        if let Some(ref f) = v.front_side
            && let Some(parsed) = PassportFile::from_tl(f)
        {
            result.special_scans.insert(FileType::FrontSide, parsed);
        }
        if let Some(ref f) = v.reverse_side
            && let Some(parsed) = PassportFile::from_tl(f)
        {
            result.special_scans.insert(FileType::ReverseSide, parsed);
        }
        if let Some(ref f) = v.selfie
            && let Some(parsed) = PassportFile::from_tl(f)
        {
            result.special_scans.insert(FileType::Selfie, parsed);
        }
        if let Some(ref plain) = v.plain_data {
            match plain {
                tl_enums::SecurePlainData::SecurePlainPhone(p) => {
                    result.data.parsed.set_text("value", p.phone.clone());
                }
                tl_enums::SecurePlainData::SecurePlainEmail(e) => {
                    result.data.parsed.set_text("value", e.email.clone());
                }
            }
        }
        result
    }
}

/// Requested type requirement inside an authorization form (`Passport::RequestedValue`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestedValue {
    /// Required value type.
    pub value_type: ValueType,
    /// Whether a selfie is required.
    pub selfie_required: bool,
    /// Whether certified translation scans are required.
    pub translation_required: bool,
    /// Whether native names are required (for `PersonalDetails`).
    pub native_names: bool,
}

/// Flattened row of mutually alternative ("one of") requested values (`Passport::RequestedRow`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RequestedRow {
    /// Alternative values in this row (1 for a mandatory type, >= 2 for `secureRequiredTypeOneOf`).
    pub values: Vec<RequestedValue>,
}

impl RequestedRow {
    /// Recursively collects a TL [`tl_enums::SecureRequiredType`] into a [`RequestedRow`]
    /// (`CollectRequestedRow`).
    pub fn from_tl(tl: &tl_enums::SecureRequiredType) -> Self {
        let mut row = Self::default();
        collect_to_requested_row(&mut row, tl);
        row
    }
}

fn collect_to_requested_row(row: &mut RequestedRow, tl: &tl_enums::SecureRequiredType) {
    match tl {
        tl_enums::SecureRequiredType::SecureRequiredType(req) => {
            row.values.push(RequestedValue {
                value_type: ValueType::from_tl(&req.r#type),
                selfie_required: req.selfie_required,
                translation_required: req.translation_required,
                native_names: req.native_names,
            });
        }
        tl_enums::SecureRequiredType::OneOf(one_of) => {
            row.values.reserve(one_of.types.len());
            for inner in &one_of.types {
                collect_to_requested_row(row, inner);
            }
        }
    }
}

/// Resolved authorization scope combining optional details + document choices (`Passport::Scope`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassportScope {
    /// Scope category.
    pub scope_type: ScopeType,
    /// Optional details value type (e.g., `PersonalDetails` inlined into `Identity`).
    pub details: Option<ValueType>,
    /// Candidate document value types (empty for `PersonalDetails`/`AddressDetails`/`Phone`/`Email`).
    pub documents: Vec<ValueType>,
}

/// Summary status for a single [`PassportScope`] (`Passport::ScopeRow`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeRow {
    /// Scope category.
    pub scope_type: ScopeType,
    /// Summary string when all required fields and scans are filled and valid; `None` if incomplete.
    pub ready: Option<String>,
    /// First active server validation error, if any.
    pub error: Option<String>,
}

/// Parsed Telegram Passport authorization form (`Passport::Form`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PassportForm {
    /// All known or requested values indexed by [`ValueType`].
    pub values: BTreeMap<ValueType, PassportValue>,
    /// Ordered requested rows (each inner `Vec` represents a single required type or `one_of` group).
    pub request: Vec<Vec<ValueType>>,
    /// Validated `http://` or `https://` privacy policy URL, if provided.
    pub privacy_policy_url: String,
    /// Raw server validation errors applied once values are decrypted.
    pub pending_errors: Vec<tl_enums::SecureValueError>,
}

/// Bot authorization request parameters (`Passport::FormRequest`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormRequest {
    /// Requesting bot's user ID.
    pub bot_id: UserId,
    /// Scope string (either modern JSON `"{...}"` or legacy space-separated scope).
    pub scope: String,
    /// Validated callback URL (`http://` or `https://`), or empty if omitted/invalid.
    pub callback_url: String,
    /// Bot's RSA public key PEM (line endings normalized to `\n`).
    pub public_key: String,
    /// Bot-supplied anti-replay nonce / payload.
    pub nonce: String,
}

impl FormRequest {
    /// Constructs a [`FormRequest`], validating `callback_url` and normalizing `public_key`
    /// (`FormRequest::FormRequest` + `PreprocessRequest`).
    pub fn new(
        bot_id: UserId,
        scope: impl Into<String>,
        callback_url: &str,
        public_key: &str,
        nonce: impl Into<String>,
    ) -> Self {
        Self {
            bot_id,
            scope: scope.into(),
            callback_url: validate_http_url(callback_url),
            public_key: public_key.replace("\r\n", "\n"),
            nonce: nonce.into(),
        }
    }
}

/// Returns `"nonce"` for JSON scopes (`{...}`) and `"payload"` for legacy scopes
/// (`Passport::NonceNameByScope`).
pub fn nonce_name_by_scope(scope: &str) -> &'static str {
    if scope.starts_with('{') && scope.ends_with('}') {
        "nonce"
    } else {
        "payload"
    }
}

/// Validates that `raw` is an `http://` or `https://` URL, returning `""` otherwise (`ValidateUrl`).
pub fn validate_http_url(raw: &str) -> String {
    match Url::parse(raw.trim()) {
        Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => parsed.into(),
        _ => String::new(),
    }
}

/// Cached passport password credentials kept in memory for 30 minutes (`Passport::SavedCredentials`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedCredentials {
    /// 32-byte primary SRP auth hash `x`.
    pub hash_for_auth: [u8; 32],
    /// 64-byte KDF hash used to encrypt/decrypt the 32-byte master secret.
    pub hash_for_secret: [u8; 64],
    /// 64-bit ID of the unlocked master secret.
    pub secret_id: u64,
}

/// Country-to-preferred-language mapping from `help.getPassportConfig` (`_passportConfig`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PassportConfig {
    /// Config hash for `help.getPassportConfig`.
    pub hash: i32,
    /// ISO2 country code -> preferred language code (e.g. `"US" -> "en"`, `"RU" -> "ru"`).
    pub languages_by_country_code: BTreeMap<String, String>,
}

impl PassportConfig {
    /// Parses `countries_langs` JSON from `help.passportConfig` (`ParseConfig`).
    pub fn parse_json(hash: i32, json_bytes: &[u8]) -> Result<Self, PassportError> {
        let val: JsonValue =
            serde_json::from_slice(json_bytes).map_err(|e| PassportError::Json(e.to_string()))?;
        let obj = val
            .as_object()
            .ok_or_else(|| PassportError::Json("passport config root is not an object".into()))?;
        let mut languages_by_country_code = BTreeMap::new();
        for (country, lang) in obj {
            if let Some(s) = lang.as_str() {
                languages_by_country_code.insert(country.clone(), s.to_string());
            }
        }
        Ok(Self {
            hash,
            languages_by_country_code,
        })
    }

    /// Returns the preferred language code for `country_code`, or `""` if unknown.
    pub fn preferred_language(&self, country_code: &str) -> &str {
        self.languages_by_country_code
            .get(country_code)
            .map(String::as_str)
            .unwrap_or("")
    }
}

/// Returns `true` if a form load error must be forwarded to the bot's `callback_url`
/// (`ForwardServiceErrorRequired`).
pub fn forward_service_error_required(error: &str) -> bool {
    matches!(
        error,
        "BOT_INVALID"
            | "PUBLIC_KEY_REQUIRED"
            | "PUBLIC_KEY_INVALID"
            | "SCOPE_EMPTY"
            | "PAYLOAD_EMPTY"
    )
}

/// Returns `true` if a `saveSecureValue` RPC error requires restarting the passport flow
/// (`SaveErrorRequiresRestart`).
pub fn save_error_requires_restart(error: &str) -> bool {
    matches!(
        error,
        "PASSWORD_REQUIRED" | "SECURE_SECRET_REQUIRED" | "SECURE_SECRET_INVALID"
    )
}

/// Returns `true` if an `acceptAuthorization` RPC error requires restarting the passport flow
/// (`AcceptErrorRequiresRestart`).
pub fn accept_error_requires_restart(error: &str) -> bool {
    matches!(
        error,
        "PASSWORD_REQUIRED"
            | "SECURE_SECRET_REQUIRED"
            | "SECURE_VALUE_EMPTY"
            | "SECURE_VALUE_HASH_INVALID"
    )
}
