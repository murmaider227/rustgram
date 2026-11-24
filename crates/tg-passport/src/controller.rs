//! Telegram Passport authorization form controller and password/secret flow state machine.
//!
//! Port of `passport/passport_form_controller.cpp` (pure logic, no GUI or direct network calls).

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Map as JsonMap, Value as JsonValue};
use tg_crypto::srp::{
    InputCheckPasswordSrp, PasswordKdfAlgoModPow, SecureSecretAlgo, compute_check, compute_hash,
    compute_secure_secret_hash, count_secure_secret_id, decrypt_secure_secret,
    encrypt_secure_secret, generate_secret_bytes, validate_new_algo,
    validate_new_secure_secret_algo,
};
use tg_tl::{enums as tl_enums, functions as tl_fn, types as tl_types};
use zeroize::Zeroize;

use crate::PassportError;
use crate::encryption::{
    SECRET_SIZE, decrypt_data, decrypt_value_secret, encrypt_credentials_secret, encrypt_data,
    encrypt_data_with_secret, encrypt_value_secret, serialize_data,
};
use crate::types::{
    FileType, FormRequest, PassportConfig, PassportFile, PassportForm, PassportValue, RequestedRow,
    SavedCredentials, ValueField, ValueMap, ValueType, forward_service_error_required,
    nonce_name_by_scope, validate_http_url,
};
use crate::validation::{
    AdditionalVisibility, compute_scope_row, compute_scopes, native_names_visibility,
    skip_field_check, validate_form, validate_native_name, validate_native_name_or_empty,
};

/// Parsed SRP challenge parameters from `account.password`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrpChallenge {
    /// Current SRP KDF parameters.
    pub algo: PasswordKdfAlgoModPow,
    /// Server ephemeral public value `srp_B`.
    pub srp_b: Vec<u8>,
    /// SRP session ID (`srp_id`).
    pub srp_id: i64,
}

/// Parsed account cloud-password settings (`Passport::PasswordSettings`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PasswordSettings {
    /// Active SRP challenge if the account has a 2FA password set.
    pub challenge: Option<SrpChallenge>,
    /// Prepared KDF algorithm for creating a new 2FA password.
    pub new_algo: Option<PasswordKdfAlgoModPow>,
    /// Prepared KDF algorithm for encrypting a new Passport secret.
    pub new_secure_algo: Option<SecureSecretAlgo>,
    /// Optional password hint.
    pub hint: String,
    /// Unconfirmed recovery email pattern, if pending confirmation.
    pub unconfirmed_pattern: String,
    /// Confirmed recovery email (used as default contact email in passport forms).
    pub confirmed_email: String,
    /// Whether password recovery email is configured.
    pub has_recovery: bool,
    /// Whether the account already has stored Passport secure values.
    pub not_empty_passport: bool,
    /// `true` if `current_algo` was present but unsupported by this client.
    pub unknown_algo: bool,
    /// Pending password reset timestamp (`0` if none).
    pub pending_reset_date: i32,
}

/// Current state of the Passport password/secret unlock flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordFlowState {
    /// Waiting for `account.getAuthorizationForm` and `account.getPassword`.
    Loading,
    /// Account has no 2FA cloud password yet (must set one up before using Passport).
    NeedSetupPassword,
    /// Prompt the user for their 2FA cloud password.
    NeedPassword {
        /// Optional password hint.
        hint: String,
        /// Whether recovery email is available.
        has_recovery: bool,
        /// Whether account already stores Passport values.
        not_empty_passport: bool,
    },
    /// Server reported a corrupted/mismatched secret; user must confirm resetting Passport.
    NeedResetSecret,
    /// Master secret is unlocked and all form values have been decrypted.
    Unlocked,
    /// Client needs an update to support an unknown KDF algorithm or `APP_VERSION_OUTDATED`.
    UpdateAppRequired,
    /// Unrecoverable form error (e.g., `BOT_INVALID`, `PUBLIC_KEY_INVALID`).
    CriticalError(String),
}

/// Outcome of the Passport authorization flow for callback URL construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackOutcome {
    /// User authorized and `account.acceptAuthorization` succeeded.
    Success,
    /// User cancelled authorization.
    Cancel,
    /// Service error code occurred.
    Error(String),
}

/// Prepared `account.acceptAuthorization` payload and credentials JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedAuthorization {
    /// Compact JSON payload (`{"secure_data":{...},"nonce"|"payload":"..."}`) before encryption.
    pub credentials_json: Vec<u8>,
    /// Ready-to-send TL RPC call `account.acceptAuthorization`.
    pub request: tl_fn::account::AcceptAuthorization,
}

/// Prepared `account.updatePasswordSettings` payload for saving a newly generated master secret.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedNewSecret {
    /// Newly generated 32-byte master secret.
    pub secret: [u8; SECRET_SIZE],
    /// 64-bit secret ID (`CountSecureSecretId(secret)`).
    pub secret_id: u64,
    /// Credentials snapshot to cache in memory (`SavedCredentials`).
    pub saved_credentials: SavedCredentials,
    /// Ready-to-send TL RPC call `account.updatePasswordSettings`.
    pub request: tl_fn::account::UpdatePasswordSettings,
}

/// Result of unlocking `account.passwordSettings` (`apply_password_settings_tl`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnlockOutcome {
    /// Existing master secret decrypted and verified; all form values decrypted.
    Unlocked {
        /// Credentials snapshot to cache for 30 minutes (`kRememberCredentialsDelay`).
        saved_credentials: SavedCredentials,
    },
    /// Account has no Passport secret yet; caller should generate & save a new secret.
    NeedGenerateSecret,
    /// Stored secret could not be decrypted or its ID mismatched; caller must prompt reset.
    NeedResetSecret,
    /// Server returned an unsupported `SecurePasswordKdfAlgo`.
    UpdateAppRequired,
}

/// Pure-logic state machine for Telegram Passport authorization (`Passport::FormController`).
#[derive(Debug, Clone)]
pub struct PassportController {
    request: FormRequest,
    form: PassportForm,
    password: PasswordSettings,
    config: PassportConfig,
    state: PasswordFlowState,
    form_loaded: bool,
    password_loaded: bool,
    secret: Option<[u8; SECRET_SIZE]>,
    secret_id: u64,
    submit_success: bool,
}

impl Drop for PassportController {
    fn drop(&mut self) {
        if let Some(ref mut s) = self.secret {
            s.zeroize();
        }
    }
}

impl PassportController {
    /// Creates a new [`PassportController`] for `request`.
    pub fn new(request: FormRequest) -> Self {
        Self {
            request,
            form: PassportForm::default(),
            password: PasswordSettings::default(),
            config: PassportConfig::default(),
            state: PasswordFlowState::Loading,
            form_loaded: false,
            password_loaded: false,
            secret: None,
            secret_id: 0,
            submit_success: false,
        }
    }

    /// Returns the normalized [`FormRequest`].
    pub fn request(&self) -> &FormRequest {
        &self.request
    }

    /// Returns the parsed [`PassportForm`].
    pub fn form(&self) -> &PassportForm {
        &self.form
    }

    /// Returns a mutable reference to the [`PassportForm`] (e.g., for scan edit operations).
    pub fn form_mut(&mut self) -> &mut PassportForm {
        &mut self.form
    }

    /// Returns the parsed [`PasswordSettings`].
    pub fn password_settings(&self) -> &PasswordSettings {
        &self.password
    }

    /// Returns the current [`PasswordFlowState`].
    pub fn state(&self) -> &PasswordFlowState {
        &self.state
    }

    /// Returns the unlocked 32-byte master secret, if ready.
    pub fn secret(&self) -> Option<&[u8; SECRET_SIZE]> {
        self.secret.as_ref()
    }

    /// Returns the 64-bit secret ID (`0` until unlocked).
    pub fn secret_id(&self) -> u64 {
        self.secret_id
    }

    /// Returns the cached `help.passportConfig` country-to-language map.
    pub fn passport_config(&self) -> &PassportConfig {
        &self.config
    }

    /// Updates the cached `help.passportConfig` country-to-language map.
    pub fn set_passport_config(&mut self, config: PassportConfig) {
        self.config = config;
    }

    /// Builds `help.getPassportConfig` using the currently cached config hash
    /// (`FormController::preferredLanguage` in `passport/passport_form_controller.cpp:1255-1257`).
    pub fn build_get_passport_config_request(&self) -> tl_fn::help::GetPassportConfig {
        tl_fn::help::GetPassportConfig {
            hash: self.config.hash,
        }
    }

    /// Applies a `help.PassportConfig` TL response (`passport/passport_form_controller.cpp:1257-1263`).
    ///
    /// Returns `Ok(true)` when a new config payload was parsed and stored, or `Ok(false)` on
    /// `help.passportConfigNotModified` (preserving the cached config).
    pub fn apply_passport_config_tl(
        &mut self,
        tl: &tl_enums::help::PassportConfig,
    ) -> Result<bool, PassportError> {
        match tl {
            tl_enums::help::PassportConfig::NotModified => Ok(false),
            tl_enums::help::PassportConfig::PassportConfig(data) => {
                let tl_enums::DataJSON::DataJSON(ref json) = data.countries_langs;
                let parsed = PassportConfig::parse_json(data.hash, json.data.as_bytes())?;
                self.config = parsed;
                Ok(true)
            }
        }
    }

    /// Builds the `account.getAuthorizationForm` TL request (`FormController::requestForm`).
    ///
    /// Fails with [`PassportError::MissingNonce`] if `request.nonce` is empty (producing
    /// `"NONCE_EMPTY"` or `"PAYLOAD_EMPTY"` matching tdesktop).
    pub fn build_get_authorization_form_request(
        &self,
    ) -> Result<tl_fn::account::GetAuthorizationForm, PassportError> {
        if self.request.nonce.is_empty() {
            let err = if nonce_name_by_scope(&self.request.scope) == "nonce" {
                "NONCE_EMPTY"
            } else {
                "PAYLOAD_EMPTY"
            };
            return Err(PassportError::MissingNonce(err));
        }
        Ok(tl_fn::account::GetAuthorizationForm {
            bot_id: self.request.bot_id.to_tl(),
            scope: self.request.scope.clone(),
            public_key: self.request.public_key.clone(),
        })
    }

    /// Applies the `account.authorizationForm` response (`FormController::parseForm`).
    pub fn apply_authorization_form_tl(
        &mut self,
        tl: &tl_enums::account::AuthorizationForm,
    ) -> Result<&PasswordFlowState, PassportError> {
        let tl_enums::account::AuthorizationForm::AuthorizationForm(data) = tl;

        let mut form = PassportForm::default();
        for raw_value in &data.values {
            let parsed = PassportValue::from_tl(raw_value);
            if form.values.contains_key(&parsed.value_type) {
                return Err(PassportError::InvalidForm(format!(
                    "duplicate value for type {:?} in authorization form",
                    parsed.value_type
                )));
            }
            form.values.insert(parsed.value_type, parsed);
        }

        if let Some(ref url) = data.privacy_policy_url {
            form.privacy_policy_url = validate_http_url(url);
        }

        for required in &data.required_types {
            let row = RequestedRow::from_tl(required);
            let mut types = Vec::with_capacity(row.values.len());
            for req in &row.values {
                let entry = form
                    .values
                    .entry(req.value_type)
                    .or_insert_with(|| PassportValue::new(req.value_type));
                entry.translation_required = req.translation_required;
                entry.selfie_required = req.selfie_required;
                entry.native_names = req.native_names;
                types.push(req.value_type);
            }
            form.request.push(types);
        }

        validate_form(&form)?;
        form.pending_errors = data.errors.clone();

        self.form = form;
        self.form_loaded = true;
        self.refresh_flow_state();
        Ok(&self.state)
    }

    /// Applies an `account.password` TL response (`FormController::applyPassword`).
    pub fn apply_password_tl(&mut self, tl: &tl_enums::account::Password) -> &PasswordFlowState {
        let tl_enums::account::Password::Password(data) = tl;

        let parsed_current = data.current_algo.as_ref().and_then(parse_password_kdf_algo);
        let challenge = match (parsed_current, data.srp_b.as_ref(), data.srp_id) {
            (Some(algo), Some(srp_b), Some(srp_id)) if srp_id != 0 => Some(SrpChallenge {
                algo,
                srp_b: srp_b.clone(),
                srp_id,
            }),
            _ => None,
        };
        let unknown_algo = data.current_algo.is_some() && challenge.is_none();

        let new_algo = parse_password_kdf_algo(&data.new_algo).map(validate_new_algo);
        let new_secure_algo = parse_secure_secret_algo(&data.new_secure_algo)
            .and_then(|a| validate_new_secure_secret_algo(a).ok());

        self.password = PasswordSettings {
            challenge,
            new_algo,
            new_secure_algo,
            hint: data.hint.clone().unwrap_or_default(),
            unconfirmed_pattern: data.email_unconfirmed_pattern.clone().unwrap_or_default(),
            confirmed_email: self.password.confirmed_email.clone(),
            has_recovery: data.has_recovery,
            not_empty_passport: data.has_secure_values,
            unknown_algo,
            pending_reset_date: data.pending_reset_date.unwrap_or(0),
        };
        self.password_loaded = true;
        self.refresh_flow_state();
        &self.state
    }

    fn refresh_flow_state(&mut self) {
        if !self.form_loaded || !self.password_loaded {
            return;
        }
        if self.secret.is_some() {
            self.state = PasswordFlowState::Unlocked;
            return;
        }
        if self.password.unknown_algo
            || self.password.new_algo.is_none()
            || self.password.new_secure_algo.is_none()
        {
            self.state = PasswordFlowState::UpdateAppRequired;
        } else if self.password.challenge.is_some() {
            self.state = PasswordFlowState::NeedPassword {
                hint: self.password.hint.clone(),
                has_recovery: self.password.has_recovery,
                not_empty_passport: self.password.not_empty_passport,
            };
        } else {
            self.state = PasswordFlowState::NeedSetupPassword;
        }
    }

    /// Handles a form-level RPC error (`FormController::formFail`).
    pub fn apply_form_error(&mut self, error_code: &str) -> &PasswordFlowState {
        if error_code == "APP_VERSION_OUTDATED" {
            self.state = PasswordFlowState::UpdateAppRequired;
        } else {
            self.state = PasswordFlowState::CriticalError(error_code.to_string());
        }
        &self.state
    }

    /// Computes SRP proof for `password` and builds `account.getPasswordSettings`
    /// (`FormController::submitPassword`).
    ///
    /// Returns `(hash_for_auth, request)` so the caller can keep `hash_for_auth` for caching or
    /// subsequent secret creation.
    pub fn prepare_password_check(
        &self,
        password: &[u8],
    ) -> Result<([u8; 32], tl_fn::account::GetPasswordSettings), PassportError> {
        let challenge = self.password.challenge.as_ref().ok_or_else(|| {
            PassportError::InvalidState("no active SRP challenge in password settings".into())
        })?;
        let hash_for_auth = compute_hash(&challenge.algo, password);
        let check = compute_check(
            &challenge.algo,
            &challenge.srp_b,
            challenge.srp_id,
            password,
        )?;
        Ok((
            hash_for_auth,
            tl_fn::account::GetPasswordSettings {
                password: srp_check_to_tl(&check),
            },
        ))
    }

    /// Applies the `account.passwordSettings` response (`FormController::validateSecureSecret` +
    /// `decryptValues` + `fillErrors` + `fillNativeFromFallback`).
    pub fn apply_password_settings_tl(
        &mut self,
        tl: &tl_enums::account::PasswordSettings,
        password: &[u8],
        hash_for_auth: [u8; 32],
    ) -> Result<UnlockOutcome, PassportError> {
        let tl_enums::account::PasswordSettings::PasswordSettings(data) = tl;
        if let Some(ref email) = data.email {
            self.password.confirmed_email = email.clone();
        }

        let Some(tl_enums::SecureSecretSettings::SecureSecretSettings(ref settings)) =
            data.secure_settings
        else {
            return Ok(UnlockOutcome::NeedGenerateSecret);
        };

        let Some(algo) = parse_secure_secret_algo(&settings.secure_algo) else {
            self.state = PasswordFlowState::UpdateAppRequired;
            return Ok(UnlockOutcome::UpdateAppRequired);
        };

        if settings.secure_secret.is_empty() {
            return Ok(UnlockOutcome::NeedGenerateSecret);
        }

        let hash_for_secret = compute_secure_secret_hash(&algo, password);
        let decrypted = match decrypt_secure_secret(&settings.secure_secret, &hash_for_secret) {
            Ok(s) => s,
            Err(_) => {
                self.reset_all_values();
                self.state = PasswordFlowState::NeedResetSecret;
                return Ok(UnlockOutcome::NeedResetSecret);
            }
        };

        let expected_id = settings.secure_secret_id as u64;
        if count_secure_secret_id(&decrypted) != expected_id {
            self.reset_all_values();
            self.state = PasswordFlowState::NeedResetSecret;
            return Ok(UnlockOutcome::NeedResetSecret);
        }

        self.secret = Some(decrypted);
        self.secret_id = expected_id;
        self.decrypt_all_values();
        self.fill_errors();
        self.fill_native_from_fallback();
        self.state = PasswordFlowState::Unlocked;

        Ok(UnlockOutcome::Unlocked {
            saved_credentials: SavedCredentials {
                hash_for_auth,
                hash_for_secret,
                secret_id: expected_id,
            },
        })
    }

    /// Unlocks directly from cached [`SavedCredentials`] against `account.passwordSettings`
    /// (`FormController::checkSavedPasswordSettings`).
    pub fn apply_saved_credentials_settings_tl(
        &mut self,
        tl: &tl_enums::account::PasswordSettings,
        saved: &SavedCredentials,
    ) -> Result<bool, PassportError> {
        let tl_enums::account::PasswordSettings::PasswordSettings(data) = tl;
        let Some(tl_enums::SecureSecretSettings::SecureSecretSettings(ref settings)) =
            data.secure_settings
        else {
            return Ok(false);
        };
        if parse_secure_secret_algo(&settings.secure_algo).is_none() {
            self.state = PasswordFlowState::UpdateAppRequired;
            return Err(PassportError::UnsupportedKdfAlgo);
        }
        if settings.secure_secret.is_empty()
            || (settings.secure_secret_id as u64) != saved.secret_id
        {
            return Ok(false);
        }
        let Ok(decrypted) = decrypt_secure_secret(&settings.secure_secret, &saved.hash_for_secret)
        else {
            return Ok(false);
        };
        if count_secure_secret_id(&decrypted) != saved.secret_id {
            return Ok(false);
        }

        if let Some(ref email) = data.email {
            self.password.confirmed_email = email.clone();
        }
        self.secret = Some(decrypted);
        self.secret_id = saved.secret_id;
        self.decrypt_all_values();
        self.fill_errors();
        self.fill_native_from_fallback();
        self.state = PasswordFlowState::Unlocked;
        Ok(true)
    }

    /// Generates a new 32-byte Passport master secret, encrypts it under `new_secure_algo`,
    /// and builds `account.updatePasswordSettings` (`FormController::generateSecret` + `saveSecret`).
    pub fn prepare_new_secret(
        &self,
        password: &[u8],
        hash_for_auth: [u8; 32],
        srp_check: &InputCheckPasswordSrp,
    ) -> Result<PreparedNewSecret, PassportError> {
        let new_secure_algo = self
            .password
            .new_secure_algo
            .as_ref()
            .ok_or(PassportError::UnsupportedKdfAlgo)?;
        let secret = generate_secret_bytes();
        let secret_id = count_secure_secret_id(&secret);
        let hash_for_secret = compute_secure_secret_hash(new_secure_algo, password);
        let encrypted_secret = encrypt_secure_secret(&secret, &hash_for_secret)?;

        let request = tl_fn::account::UpdatePasswordSettings {
            password: srp_check_to_tl(srp_check),
            new_settings: tl_enums::account::PasswordInputSettings::PasswordInputSettings(
                tl_types::account::PasswordInputSettings {
                    new_algo: None,
                    new_password_hash: None,
                    hint: None,
                    email: None,
                    new_secure_settings: Some(
                        tl_enums::SecureSecretSettings::SecureSecretSettings(
                            tl_types::SecureSecretSettings {
                                secure_algo: secure_secret_algo_to_tl(new_secure_algo),
                                secure_secret: encrypted_secret.to_vec(),
                                secure_secret_id: secret_id as i64,
                            },
                        ),
                    ),
                },
            ),
        };

        Ok(PreparedNewSecret {
            secret,
            secret_id,
            saved_credentials: SavedCredentials {
                hash_for_auth,
                hash_for_secret,
                secret_id,
            },
            request,
        })
    }

    /// Builds `account.updatePasswordSettings` to clear a corrupted server Passport secret
    /// (`FormController::resetSecret`).
    pub fn prepare_reset_secret(
        &self,
        srp_check: &InputCheckPasswordSrp,
    ) -> tl_fn::account::UpdatePasswordSettings {
        tl_fn::account::UpdatePasswordSettings {
            password: srp_check_to_tl(srp_check),
            new_settings: tl_enums::account::PasswordInputSettings::PasswordInputSettings(
                tl_types::account::PasswordInputSettings {
                    new_algo: None,
                    new_password_hash: None,
                    hint: None,
                    email: None,
                    new_secure_settings: Some(
                        tl_enums::SecureSecretSettings::SecureSecretSettings(
                            tl_types::SecureSecretSettings {
                                secure_algo: tl_enums::SecurePasswordKdfAlgo::Unknown,
                                secure_secret: Vec::new(),
                                secure_secret_id: 0,
                            },
                        ),
                    ),
                },
            ),
        }
    }

    /// Activates a newly saved master secret after `account.updatePasswordSettings` succeeds.
    pub fn apply_new_secret_saved(&mut self, secret: [u8; SECRET_SIZE], secret_id: u64) {
        self.secret = Some(secret);
        self.secret_id = secret_id;
        self.state = PasswordFlowState::Unlocked;
    }

    /// Directly installs an already-unlocked master secret (useful for tests or restored state)
    /// and decrypts all loaded form values.
    pub fn unlock_with_secret(&mut self, secret: [u8; SECRET_SIZE]) {
        self.secret_id = count_secure_secret_id(&secret);
        self.secret = Some(secret);
        self.decrypt_all_values();
        self.fill_errors();
        self.fill_native_from_fallback();
        self.state = PasswordFlowState::Unlocked;
    }

    fn reset_all_values(&mut self) {
        for value in self.form.values.values_mut() {
            let vt = value.value_type;
            value.fill_data_from(PassportValue::new(vt));
        }
    }

    fn decrypt_all_values(&mut self) {
        let Some(master_secret) = self.secret else {
            return;
        };
        for value in self.form.values.values_mut() {
            decrypt_value_in_place(value, &master_secret);
        }
    }

    /// Applies `form.pending_errors` onto decrypted values, fields, and files
    /// (`FormController::fillErrors`).
    pub fn fill_errors(&mut self) {
        let errors = self.form.pending_errors.clone();
        for err in &errors {
            match err {
                tl_enums::SecureValueError::SecureValueError(e) => {
                    let vt = ValueType::from_tl(&e.r#type);
                    if let Some(val) = self.form.values.get_mut(&vt)
                        && vt.can_have_errors()
                    {
                        val.error = e.text.clone();
                    }
                }
                tl_enums::SecureValueError::Data(e) => {
                    let vt = ValueType::from_tl(&e.r#type);
                    if let Some(val) = self.form.values.get_mut(&vt)
                        && vt.can_have_errors()
                        && !skip_field_check(val, &e.field)
                    {
                        val.data
                            .parsed
                            .fields
                            .entry(e.field.clone())
                            .or_default()
                            .error = e.text.clone();
                    }
                }
                tl_enums::SecureValueError::File(e) => {
                    let vt = ValueType::from_tl(&e.r#type);
                    if let Some(val) = self.form.values.get_mut(&vt)
                        && val.requires_scan(FileType::Scan)
                        && let Some(file) = val.scans.iter_mut().find(|f| f.hash == e.file_hash)
                    {
                        file.error = e.text.clone();
                    }
                }
                tl_enums::SecureValueError::Files(e) => {
                    let vt = ValueType::from_tl(&e.r#type);
                    if let Some(val) = self.form.values.get_mut(&vt)
                        && val.requires_scan(FileType::Scan)
                    {
                        val.scan_missing_error = e.text.clone();
                    }
                }
                tl_enums::SecureValueError::TranslationFile(e) => {
                    let vt = ValueType::from_tl(&e.r#type);
                    if let Some(val) = self.form.values.get_mut(&vt)
                        && val.requires_scan(FileType::Translation)
                        && let Some(file) =
                            val.translations.iter_mut().find(|f| f.hash == e.file_hash)
                    {
                        file.error = e.text.clone();
                    }
                }
                tl_enums::SecureValueError::TranslationFiles(e) => {
                    let vt = ValueType::from_tl(&e.r#type);
                    if let Some(val) = self.form.values.get_mut(&vt)
                        && val.requires_scan(FileType::Translation)
                    {
                        val.translation_missing_error = e.text.clone();
                    }
                }
                tl_enums::SecureValueError::FrontSide(e) => {
                    self.set_special_scan_error(&e.r#type, FileType::FrontSide, &e.text);
                }
                tl_enums::SecureValueError::ReverseSide(e) => {
                    self.set_special_scan_error(&e.r#type, FileType::ReverseSide, &e.text);
                }
                tl_enums::SecureValueError::Selfie(e) => {
                    self.set_special_scan_error(&e.r#type, FileType::Selfie, &e.text);
                }
            }
        }
    }

    fn set_special_scan_error(
        &mut self,
        tl_type: &tl_enums::SecureValueType,
        file_type: FileType,
        text: &str,
    ) {
        let vt = ValueType::from_tl(tl_type);
        if let Some(val) = self.form.values.get_mut(&vt)
            && val.requires_special_scan(file_type)
            && let Some(scan) = val.special_scans.get_mut(&file_type)
        {
            scan.error = text.to_string();
        }
    }

    /// Copies Latin names into empty native-name fields when the residence country uses English
    /// (`FormController::fillNativeFromFallback`).
    ///
    /// Returns `true` if any native field was populated and needs saving.
    pub fn fill_native_from_fallback(&mut self) -> bool {
        let Some(details) = self.form.values.get_mut(&ValueType::PersonalDetails) else {
            return false;
        };
        if !details.native_names {
            return false;
        }
        let country = details
            .data
            .parsed
            .text("residence_country_code")
            .to_string();
        let lang = self.config.preferred_language(&country);
        if native_names_visibility(&country, lang) != AdditionalVisibility::OnlyIfError {
            return false;
        }

        let pairs = [
            ("first_name_native", "first_name", true),
            ("middle_name_native", "middle_name", false),
            ("last_name_native", "last_name", true),
        ];

        let mut updates = Vec::new();
        for &(native_key, latin_key, required) in &pairs {
            if let Some(existing) = details.data.parsed.fields.get(native_key)
                && (!existing.text.is_empty() || !existing.error.is_empty())
            {
                return false;
            }
            let latin = details.data.parsed.text(latin_key);
            let valid = if required {
                validate_native_name(latin).is_ok()
            } else {
                validate_native_name_or_empty(latin).is_ok()
            };
            if !valid {
                return false;
            }
            if !latin.is_empty() {
                updates.push((native_key.to_string(), latin.to_string()));
            }
        }

        if updates.is_empty() {
            return false;
        }
        for (k, v) in updates {
            details.data.parsed.set_text(k, v);
        }
        true
    }

    /// Encrypts staged field/scan edits for `value_type` and builds `account.saveSecureValue`
    /// (`FormController::saveEncryptedValue` / `savePlainTextValue`).
    pub fn prepare_save_value(
        &mut self,
        value_type: ValueType,
        changes: ValueMap,
    ) -> Result<tl_fn::account::SaveSecureValue, PassportError> {
        let value = self
            .form
            .values
            .entry(value_type)
            .or_insert_with(|| PassportValue::new(value_type));

        value.data.parsed_in_edit = value.data.parsed.clone();
        for (k, v) in changes.fields {
            value.data.parsed_in_edit.fields.insert(k, v);
        }

        if !value_type.is_encrypted() {
            let text = value.data.parsed_in_edit.text("value").to_string();
            let plain_data = match value_type {
                ValueType::Phone => {
                    tl_enums::SecurePlainData::SecurePlainPhone(tl_types::SecurePlainPhone {
                        phone: text,
                    })
                }
                ValueType::Email => {
                    tl_enums::SecurePlainData::SecurePlainEmail(tl_types::SecurePlainEmail {
                        email: text,
                    })
                }
                _ => unreachable!(),
            };
            return Ok(tl_fn::account::SaveSecureValue {
                value: tl_enums::InputSecureValue::InputSecureValue(tl_types::InputSecureValue {
                    r#type: value_type.to_tl(),
                    data: None,
                    front_side: None,
                    reverse_side: None,
                    selfie: None,
                    translation: None,
                    files: None,
                    plain_data: Some(plain_data),
                }),
                secure_secret_id: self.secret_id as i64,
            });
        }

        let master_secret = self.secret.ok_or(PassportError::SecretNotReady)?;

        let data_secret: [u8; SECRET_SIZE] = if value.data.secret.len() == SECRET_SIZE {
            let mut arr = [0u8; SECRET_SIZE];
            arr.copy_from_slice(&value.data.secret);
            arr
        } else {
            let fresh = generate_secret_bytes();
            value.data.secret = fresh.to_vec();
            fresh
        };

        let secure_data = if value.data.parsed_in_edit.fields.is_empty() {
            None
        } else {
            let json_bytes = serialize_data(&value.data.parsed_in_edit.to_text_map())?;
            let encrypted = encrypt_data_with_secret(&json_bytes, &data_secret)?;
            let enc_secret = encrypt_value_secret(&data_secret, &master_secret, &encrypted.hash)?;
            value.data.hash_in_edit = encrypted.hash.to_vec();
            value.data.encrypted_secret_in_edit = enc_secret.to_vec();
            Some(tl_enums::SecureData::SecureData(tl_types::SecureData {
                data: encrypted.bytes,
                data_hash: encrypted.hash.to_vec(),
                secret: enc_secret.to_vec(),
            }))
        };

        let files: Vec<tl_enums::InputSecureFile> = value
            .scans_in_edit
            .iter()
            .filter(|f| !f.deleted)
            .map(|f| f.to_input_tl())
            .collect();
        let translations: Vec<tl_enums::InputSecureFile> = value
            .translations_in_edit
            .iter()
            .filter(|f| !f.deleted)
            .map(|f| f.to_input_tl())
            .collect();

        let special = |ft: FileType| -> Option<tl_enums::InputSecureFile> {
            value
                .special_scans_in_edit
                .get(&ft)
                .filter(|f| !f.deleted)
                .map(|f| f.to_input_tl())
        };

        Ok(tl_fn::account::SaveSecureValue {
            value: tl_enums::InputSecureValue::InputSecureValue(tl_types::InputSecureValue {
                r#type: value_type.to_tl(),
                data: secure_data,
                front_side: special(FileType::FrontSide),
                reverse_side: special(FileType::ReverseSide),
                selfie: special(FileType::Selfie),
                translation: if translations.is_empty() {
                    None
                } else {
                    Some(translations)
                },
                files: if files.is_empty() { None } else { Some(files) },
                plain_data: None,
            }),
            secure_secret_id: self.secret_id as i64,
        })
    }

    /// Applies a saved `SecureValue` response from `account.saveSecureValue`
    /// (`FormController::sendSaveRequest` done handler).
    pub fn apply_saved_value_tl(&mut self, tl: &tl_enums::SecureValue) {
        let mut refreshed = PassportValue::from_tl(tl);
        if let Some(master_secret) = self.secret {
            decrypt_value_in_place(&mut refreshed, &master_secret);
        }
        let vt = refreshed.value_type;
        if let Some(existing) = self.form.values.get_mut(&vt) {
            existing.fill_data_from(refreshed);
        } else {
            self.form.values.insert(vt, refreshed);
        }
    }

    /// Builds `account.deleteSecureValue` for `value_type`.
    pub fn build_delete_value_request(
        &self,
        value_type: ValueType,
    ) -> tl_fn::account::DeleteSecureValue {
        tl_fn::account::DeleteSecureValue {
            types: vec![value_type.to_tl()],
        }
    }

    /// Builds `account.sendVerifyPhoneCode` for phone verification.
    pub fn build_send_verify_phone_code_request(
        &self,
        phone: impl Into<String>,
    ) -> tl_fn::account::SendVerifyPhoneCode {
        tl_fn::account::SendVerifyPhoneCode {
            phone_number: phone.into(),
            settings: tl_enums::CodeSettings::CodeSettings(tl_types::CodeSettings {
                allow_flashcall: false,
                current_number: false,
                allow_app_hash: false,
                allow_missed_call: false,
                allow_firebase: false,
                unknown_number: false,
                logout_tokens: None,
                token: None,
                app_sandbox: None,
            }),
        }
    }

    /// Builds `account.verifyPhone` for confirming a phone verification code.
    pub fn build_verify_phone_request(
        &self,
        phone: impl Into<String>,
        phone_code_hash: impl Into<String>,
        code: &str,
    ) -> tl_fn::account::VerifyPhone {
        tl_fn::account::VerifyPhone {
            phone_number: phone.into(),
            phone_code_hash: phone_code_hash.into(),
            phone_code: code.trim().to_string(),
        }
    }

    /// Builds `account.sendVerifyEmailCode` for email verification.
    pub fn build_send_verify_email_code_request(
        &self,
        email: impl Into<String>,
    ) -> tl_fn::account::SendVerifyEmailCode {
        tl_fn::account::SendVerifyEmailCode {
            purpose: tl_enums::EmailVerifyPurpose::Passport,
            email: email.into(),
        }
    }

    /// Builds `account.verifyEmail` for confirming an email verification code.
    pub fn build_verify_email_request(&self, code: &str) -> tl_fn::account::VerifyEmail {
        tl_fn::account::VerifyEmail {
            purpose: tl_enums::EmailVerifyPurpose::Passport,
            verification: tl_enums::EmailVerification::Code(tl_types::EmailVerificationCode {
                code: code.trim().to_string(),
            }),
        }
    }

    /// Validates all scopes, builds the credentials JSON, encrypts it with a fresh secret +
    /// bot RSA-OAEP public key, and returns `account.acceptAuthorization`
    /// (`FormController::prepareFinalData` + `FormController::submitGetErrors`).
    pub fn prepare_accept_authorization(&self) -> Result<PreparedAuthorization, PassportError> {
        let scopes = compute_scopes(&self.form);
        let mut errors = Vec::new();
        let mut value_hashes = Vec::new();
        let mut secure_data = JsonMap::new();

        for scope in &scopes {
            let row = compute_scope_row(&self.form, scope);
            if row.ready.is_none() || row.error.is_some() {
                if let Some(dt) = scope.details {
                    errors.push(dt);
                } else if let Some(&doc) = scope.documents.first() {
                    errors.push(doc);
                }
                continue;
            }
            if let Some(dt) = scope.details
                && let Some(val) = self.form.values.get(&dt)
            {
                append_value_to_credentials(val, &mut value_hashes, &mut secure_data);
            }
            if !scope.documents.is_empty() {
                for dt in &scope.documents {
                    if let Some(doc) = self.form.values.get(dt)
                        && doc.scans_are_filled()
                    {
                        append_value_to_credentials(doc, &mut value_hashes, &mut secure_data);
                        break;
                    }
                }
            }
        }

        if !errors.is_empty() {
            return Err(PassportError::IncompleteScopes(errors));
        }

        let mut root = JsonMap::new();
        root.insert("secure_data".to_string(), JsonValue::Object(secure_data));
        root.insert(
            nonce_name_by_scope(&self.request.scope).to_string(),
            JsonValue::String(self.request.nonce.clone()),
        );
        let credentials_json = serde_json::to_vec(&JsonValue::Object(root))
            .map_err(|e| PassportError::Json(e.to_string()))?;

        let encrypted = encrypt_data(&credentials_json)?;
        let encrypted_secret =
            encrypt_credentials_secret(&encrypted.secret, &self.request.public_key)?;

        let request = tl_fn::account::AcceptAuthorization {
            bot_id: self.request.bot_id.to_tl(),
            scope: self.request.scope.clone(),
            public_key: self.request.public_key.clone(),
            value_hashes,
            credentials: tl_enums::SecureCredentialsEncrypted::SecureCredentialsEncrypted(
                tl_types::SecureCredentialsEncrypted {
                    data: encrypted.bytes,
                    hash: encrypted.hash.to_vec(),
                    secret: encrypted_secret,
                },
            ),
        };

        Ok(PreparedAuthorization {
            credentials_json,
            request,
        })
    }

    /// Marks authorization as successfully submitted.
    pub fn mark_submit_success(&mut self) {
        self.submit_success = true;
    }

    /// Returns `true` if `account.acceptAuthorization` has succeeded.
    pub fn submit_success(&self) -> bool {
        self.submit_success
    }

    /// Computes the redirect callback URL with `tg_passport=...` query/hash suffix
    /// (`FormController::cancelSure`), or `None` if no callback should be opened.
    pub fn callback_url_for_outcome(&self, outcome: &CallbackOutcome) -> Option<String> {
        if self.request.callback_url.is_empty() {
            return None;
        }
        let suffix = match outcome {
            CallbackOutcome::Success => "tg_passport=success".to_string(),
            CallbackOutcome::Cancel => "tg_passport=cancel".to_string(),
            CallbackOutcome::Error(code) => {
                if !forward_service_error_required(code) {
                    return None;
                }
                format!("tg_passport=error&error={code}")
            }
        };
        Some(append_query_or_hash(&self.request.callback_url, &suffix))
    }
}

fn append_query_or_hash(base: &str, param: &str) -> String {
    if let Some(hash_idx) = base.find('#') {
        let after_hash = &base[hash_idx + 1..];
        let sep = if after_hash.is_empty() || after_hash.ends_with('&') || after_hash.ends_with('?')
        {
            ""
        } else {
            "&"
        };
        format!("{base}{sep}{param}")
    } else if base.contains('?') {
        let sep = if base.ends_with('?') || base.ends_with('&') {
            ""
        } else {
            "&"
        };
        format!("{base}{sep}{param}")
    } else {
        format!("{base}?{param}")
    }
}

fn file_to_credentials_json(file: &PassportFile) -> JsonValue {
    let mut obj = JsonMap::new();
    obj.insert(
        "file_hash".to_string(),
        JsonValue::String(BASE64.encode(&file.hash)),
    );
    obj.insert(
        "secret".to_string(),
        JsonValue::String(BASE64.encode(&file.secret)),
    );
    JsonValue::Object(obj)
}

fn append_value_to_credentials(
    value: &PassportValue,
    hashes: &mut Vec<tl_enums::SecureValueHash>,
    secure_data: &mut JsonMap<String, JsonValue>,
) {
    hashes.push(tl_enums::SecureValueHash::SecureValueHash(
        tl_types::SecureValueHash {
            r#type: value.value_type.to_tl(),
            hash: value.submit_hash.clone(),
        },
    ));
    let Some(cred_key) = value.value_type.credentials_key() else {
        return;
    };
    let mut entry = JsonMap::new();
    if !value.data.parsed.fields.is_empty() {
        let mut data_obj = JsonMap::new();
        data_obj.insert(
            "data_hash".to_string(),
            JsonValue::String(BASE64.encode(&value.data.hash)),
        );
        data_obj.insert(
            "secret".to_string(),
            JsonValue::String(BASE64.encode(&value.data.secret)),
        );
        entry.insert("data".to_string(), JsonValue::Object(data_obj));
    }
    if !value.scans.is_empty() {
        entry.insert(
            "files".to_string(),
            JsonValue::Array(value.scans.iter().map(file_to_credentials_json).collect()),
        );
    }
    if value.translation_required && !value.translations.is_empty() {
        entry.insert(
            "translation".to_string(),
            JsonValue::Array(
                value
                    .translations
                    .iter()
                    .map(file_to_credentials_json)
                    .collect(),
            ),
        );
    }
    for (&ft, scan) in &value.special_scans {
        if value.requires_special_scan(ft)
            && let Some(scan_key) = ft.special_scan_credentials_key()
        {
            entry.insert(scan_key.to_string(), file_to_credentials_json(scan));
        }
    }
    secure_data.insert(cred_key.to_string(), JsonValue::Object(entry));
}

fn decrypt_value_in_place(value: &mut PassportValue, master_secret: &[u8; SECRET_SIZE]) {
    if !validate_value_secrets(value, master_secret) {
        let vt = value.value_type;
        value.fill_data_from(PassportValue::new(vt));
        return;
    }
    if !value.data.original.is_empty() {
        let Ok(decrypted) =
            decrypt_data(&value.data.original, &value.data.hash, &value.data.secret)
        else {
            let vt = value.value_type;
            value.fill_data_from(PassportValue::new(vt));
            return;
        };
        let Ok(fields) = crate::encryption::deserialize_data(&decrypted) else {
            let vt = value.value_type;
            value.fill_data_from(PassportValue::new(vt));
            return;
        };
        value.data.parsed.fields = fields
            .into_iter()
            .map(|(k, text)| {
                (
                    k,
                    ValueField {
                        text,
                        error: String::new(),
                    },
                )
            })
            .collect();
    }
}

fn validate_value_secrets(value: &mut PassportValue, master_secret: &[u8; SECRET_SIZE]) -> bool {
    if !value.data.original.is_empty() {
        let Ok(secret) = decrypt_value_secret(
            &value.data.encrypted_secret,
            master_secret,
            &value.data.hash,
        ) else {
            return false;
        };
        value.data.secret = secret.to_vec();
    }
    for file in &mut value.scans {
        let Ok(secret) = decrypt_value_secret(&file.encrypted_secret, master_secret, &file.hash)
        else {
            return false;
        };
        file.secret = secret.to_vec();
    }
    for file in &mut value.translations {
        let Ok(secret) = decrypt_value_secret(&file.encrypted_secret, master_secret, &file.hash)
        else {
            return false;
        };
        file.secret = secret.to_vec();
    }
    for file in value.special_scans.values_mut() {
        let Ok(secret) = decrypt_value_secret(&file.encrypted_secret, master_secret, &file.hash)
        else {
            return false;
        };
        file.secret = secret.to_vec();
    }
    true
}

/// Converts a TL `PasswordKdfAlgo` into `tg_crypto::srp::PasswordKdfAlgoModPow`.
pub fn parse_password_kdf_algo(tl: &tl_enums::PasswordKdfAlgo) -> Option<PasswordKdfAlgoModPow> {
    match tl {
        tl_enums::PasswordKdfAlgo::Unknown => None,
        tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(a) => {
            Some(PasswordKdfAlgoModPow {
                salt1: a.salt1.clone(),
                salt2: a.salt2.clone(),
                g: a.g,
                p: a.p.clone(),
            })
        }
    }
}

/// Converts a TL `SecurePasswordKdfAlgo` into `tg_crypto::srp::SecureSecretAlgo`.
pub fn parse_secure_secret_algo(tl: &tl_enums::SecurePasswordKdfAlgo) -> Option<SecureSecretAlgo> {
    match tl {
        tl_enums::SecurePasswordKdfAlgo::Unknown => None,
        tl_enums::SecurePasswordKdfAlgo::SHA512(s) => Some(SecureSecretAlgo::Sha512 {
            salt: s.salt.clone(),
        }),
        tl_enums::SecurePasswordKdfAlgo::PBKDF2HMACSHA512iter100000(s) => {
            Some(SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 {
                salt: s.salt.clone(),
            })
        }
    }
}

/// Converts `tg_crypto::srp::SecureSecretAlgo` into a TL `SecurePasswordKdfAlgo`.
pub fn secure_secret_algo_to_tl(algo: &SecureSecretAlgo) -> tl_enums::SecurePasswordKdfAlgo {
    match algo {
        SecureSecretAlgo::Sha512 { salt } => {
            tl_enums::SecurePasswordKdfAlgo::SHA512(tl_types::SecurePasswordKdfAlgoSHA512 {
                salt: salt.clone(),
            })
        }
        SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 { salt } => {
            tl_enums::SecurePasswordKdfAlgo::PBKDF2HMACSHA512iter100000(
                tl_types::SecurePasswordKdfAlgoPBKDF2HMACSHA512iter100000 { salt: salt.clone() },
            )
        }
    }
}

/// Converts `tg_crypto::srp::InputCheckPasswordSrp` into a TL `InputCheckPasswordSRP`.
pub fn srp_check_to_tl(check: &InputCheckPasswordSrp) -> tl_enums::InputCheckPasswordSRP {
    tl_enums::InputCheckPasswordSRP::InputCheckPasswordSRP(tl_types::InputCheckPasswordSRP {
        srp_id: check.srp_id,
        a: check.a.to_vec(),
        m1: check.m1.to_vec(),
    })
}

/// Helper to build a [`ValueMap`] from plain string pairs (convenience for callers/tests).
pub fn value_map_from_pairs(pairs: &[(&str, &str)]) -> ValueMap {
    ValueMap {
        fields: pairs
            .iter()
            .map(|&(k, v)| {
                (
                    k.to_string(),
                    ValueField {
                        text: v.to_string(),
                        error: String::new(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>(),
    }
}
