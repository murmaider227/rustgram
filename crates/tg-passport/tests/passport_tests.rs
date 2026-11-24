//! Unit and known-answer tests for `tg-passport`.

use std::collections::BTreeMap;

use serde_json::Value as JsonValue;
use tg_crypto::aes::aes_cbc_encrypt;
use tg_crypto::dh::GOOD_PRIME;
use tg_crypto::hash::{md5_hex, sha256, sha512};
use tg_crypto::rsa::PRODUCTION_PUBLIC_KEYS;
use tg_crypto::srp::{
    SecureSecretAlgo, check_secret_bytes, compute_secure_secret_hash, count_secure_secret_id,
    encrypt_secure_secret, generate_secret_bytes,
};
use tg_data::UserId;
use tg_passport::PassportError;
use tg_passport::controller::{
    CallbackOutcome, PassportController, PasswordFlowState, UnlockOutcome, value_map_from_pairs,
};
use tg_passport::encryption::{
    ALIGN_TO, MAX_PADDING, MIN_PADDING, choose_padding_length, create_new_master_secret,
    decrypt_data, decrypt_value_secret, deserialize_data, encrypt_credentials_secret, encrypt_data,
    encrypt_data_deterministic, encrypt_data_with_secret, encrypt_file_for_upload,
    encrypt_value_secret, serialize_data, unlock_master_secret,
};
use tg_passport::types::{
    DOCUMENT_SCANS_LIMIT, EditFile, FileType, FormRequest, PassportConfig, PassportFile,
    PassportForm, PassportValue, ScopeType, ValueMap, ValueType, nonce_name_by_scope,
};
use tg_passport::validation::{
    AdditionalVisibility, FieldValidationError, compute_scope_row, compute_scope_row_ready,
    compute_scopes, native_names_visibility, postprocess_email, postprocess_phone,
    skip_field_check, validate_address, validate_contact, validate_date, validate_form,
    validate_identity_document, validate_latin_name, validate_personal_details, validate_postcode,
};
use tg_tl::{Deserializable, Serializable, enums as tl_enums, types as tl_types};

fn make_deterministic_secret(mut base: [u8; 32]) -> [u8; 32] {
    let sum: u64 = base.iter().map(|&b| u64::from(b)).sum();
    let modulo = sum % 255;
    let add = 255 + 239 - modulo;
    base[0] = ((u64::from(base[0]) + add) % 255) as u8;
    assert!(check_secret_bytes(&base));
    base
}

// ============================================================================
// Encryption Known-Answer Vectors (derived from passport_encryption.cpp)
// ============================================================================

#[test]
fn passport_data_encryption_known_answer_vector() {
    // Reproduces Passport::EncryptData / DecryptData step-by-step from passport_encryption.cpp.
    let secret = make_deterministic_secret([
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f, 0x20,
    ]);
    let plaintext = br#"{"first_name":"Ada","last_name":"Lovelace"}"#; // 43 bytes
    // Choose padding = 37 so 37 + 43 = 80 (multiple of 16, within 32..=255).
    let padding_tail = [0xA5u8; 36];

    // Independently compute expected reference wire output per passport_encryption.cpp:
    let mut expected_unencrypted = Vec::with_capacity(80);
    expected_unencrypted.push(37u8);
    expected_unencrypted.extend_from_slice(&padding_tail);
    expected_unencrypted.extend_from_slice(plaintext);
    let expected_hash = sha256(&[&expected_unencrypted]);
    let sha512_key_iv = sha512(&[&secret, &expected_hash]);
    let key: [u8; 32] = sha512_key_iv[0..32].try_into().unwrap();
    let iv: [u8; 16] = sha512_key_iv[32..48].try_into().unwrap();
    let expected_ciphertext = aes_cbc_encrypt(&expected_unencrypted, &key, &iv).unwrap();

    let encrypted = encrypt_data_deterministic(plaintext, &secret, &padding_tail).unwrap();
    assert_eq!(encrypted.secret, secret);
    assert_eq!(encrypted.hash, expected_hash);
    assert_eq!(encrypted.bytes, expected_ciphertext);
    assert_eq!(encrypted.bytes.len(), 80);

    // Round-trip decrypt
    let decrypted = decrypt_data(&encrypted.bytes, &encrypted.hash, &encrypted.secret).unwrap();
    assert_eq!(decrypted, plaintext);

    // Tampered ciphertext fails SHA-256 verification
    let mut tampered = encrypted.bytes.clone();
    tampered[0] ^= 0x01;
    assert!(matches!(
        decrypt_data(&tampered, &encrypted.hash, &encrypted.secret),
        Err(PassportError::HashMismatch)
    ));

    // Invalid padding (< 32) rejected on encrypt
    assert!(matches!(
        encrypt_data_deterministic(b"1234567890123456", &secret, &[0u8; 15]),
        Err(PassportError::InvalidPadding(16))
    ));
}

#[test]
fn passport_padding_bounds_and_value_secret_roundtrip() {
    for len in 0..=64 {
        for r in [0u32, 1, 15, 16, 100, 207, 10_000, u32::MAX] {
            let pad = choose_padding_length(len, r);
            assert!((MIN_PADDING..=MAX_PADDING).contains(&pad));
            assert_eq!((len + pad) % ALIGN_TO, 0);
        }
    }

    let master_secret = generate_secret_bytes();
    let value_secret = generate_secret_bytes();
    let enc_data = encrypt_data_with_secret(b"passport-scan-bytes", &value_secret).unwrap();

    let encrypted_val_secret =
        encrypt_value_secret(&value_secret, &master_secret, &enc_data.hash).unwrap();
    let decrypted_val_secret =
        decrypt_value_secret(&encrypted_val_secret, &master_secret, &enc_data.hash).unwrap();
    assert_eq!(decrypted_val_secret, value_secret);

    // Wrong master secret fails mod-255 checksum (with overwhelming probability)
    let wrong_master = make_deterministic_secret([0x55; 32]);
    assert!(decrypt_value_secret(&encrypted_val_secret, &wrong_master, &enc_data.hash).is_err());

    // Invalid value secret checksum rejected before encrypting
    let mut bad_secret = value_secret;
    bad_secret[0] = bad_secret[0].wrapping_add(1);
    assert!(matches!(
        encrypt_value_secret(&bad_secret, &master_secret, &enc_data.hash),
        Err(PassportError::InvalidSecretChecksum)
    ));

    // File upload encryption produces valid MD5 hex checksum of ciphertext
    let upload = encrypt_file_for_upload(b"fake-jpeg-data", Some(value_secret)).unwrap();
    assert_eq!(upload.md5_checksum, md5_hex(&upload.bytes));
    assert_eq!(upload.md5_checksum.len(), 32);
    assert_eq!(
        decrypt_data(&upload.bytes, &upload.hash, &upload.file_secret).unwrap(),
        b"fake-jpeg-data"
    );
}

#[test]
fn credentials_rsa_oaep_and_master_secret_kdf() {
    // Encrypt 32-byte credentials secret with PEM RSA-2048 public key (with CRLF normalization)
    let pem_crlf = PRODUCTION_PUBLIC_KEYS[0].replace('\n', "\r\n");
    let enc_creds = encrypt_data(br#"{"secure_data":{},"nonce":"abc"}"#).unwrap();
    let rsa_secret = encrypt_credentials_secret(&enc_creds.secret, &pem_crlf).unwrap();
    assert_eq!(rsa_secret.len(), 256);

    // Master secret creation + unlock round-trip (PBKDF2-HMAC-SHA512)
    let algo = SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 {
        salt: b"server-salt".to_vec(),
    };
    let password = b"correct-horse-battery-staple";
    let (secret, secret_id, enc_secret, hash_for_secret, prepared_algo) =
        create_new_master_secret(algo, password).unwrap();
    assert!(check_secret_bytes(&secret));
    assert_eq!(count_secure_secret_id(&secret), secret_id);
    if let SecureSecretAlgo::Pbkdf2HmacSha512Iter100000 { salt } = &prepared_algo {
        assert_eq!(salt.len(), b"server-salt".len() + 32);
    } else {
        panic!("expected PBKDF2 algo");
    }
    let unlocked = unlock_master_secret(&enc_secret, &hash_for_secret, secret_id).unwrap();
    assert_eq!(unlocked, secret);

    // Mismatched secret_id returns SecretIdMismatch
    assert!(matches!(
        unlock_master_secret(&enc_secret, &hash_for_secret, secret_id ^ 1),
        Err(PassportError::SecretIdMismatch { .. })
    ));

    // JSON data serialization & deserialization rules
    let mut map = BTreeMap::new();
    map.insert("first_name".to_string(), "Ada".to_string());
    map.insert("birth_date".to_string(), "10.12.1815".to_string());
    let json = serialize_data(&map).unwrap();
    assert_eq!(deserialize_data(&json).unwrap(), map);

    // Null -> "", Number -> string, Bool/Array/Object -> error
    let mixed = deserialize_data(br#"{"a":"ok","b":null,"c":42}"#).unwrap();
    assert_eq!(mixed.get("a").map(String::as_str), Some("ok"));
    assert_eq!(mixed.get("b").map(String::as_str), Some(""));
    assert_eq!(mixed.get("c").map(String::as_str), Some("42"));
    assert!(deserialize_data(br#"{"bad":true}"#).is_err());
    assert!(deserialize_data(br#"{"bad":[1,2]}"#).is_err());
    assert!(deserialize_data(br#"{"bad":{}}"#).is_err());
}

// ============================================================================
// Field & Form Validation + Scope Computation
// ============================================================================

#[test]
fn identity_address_and_contact_validation_rules() {
    assert!(validate_latin_name("Jean-Luc O'Brien, Jr.").is_ok());
    assert_eq!(
        validate_latin_name("Иван"),
        Err(FieldValidationError::BadLatinName)
    );
    assert_eq!(
        validate_latin_name(""),
        Err(FieldValidationError::InvalidLength)
    );

    assert!(validate_date("10.12.1815").is_ok());
    assert_eq!(
        validate_date("1815-12-10"),
        Err(FieldValidationError::InvalidDateFormat)
    );
    assert!(validate_postcode("SW1A-2AA").is_ok());
    assert_eq!(
        validate_postcode("1"),
        Err(FieldValidationError::InvalidPostcode)
    );
    assert_eq!(
        validate_postcode("12345678901"),
        Err(FieldValidationError::InvalidPostcode)
    );

    // Phone and Email
    let phone = postprocess_phone("+1 (415) 555-0199");
    assert_eq!(phone, "14155550199");
    assert!(validate_contact(ScopeType::Phone, &phone).is_ok());
    assert!(validate_contact(ScopeType::Phone, "1").is_err());

    let email = postprocess_email("  ada@example.com  ");
    assert_eq!(email, "ada@example.com");
    assert!(validate_contact(ScopeType::Email, &email).is_ok());
    assert!(validate_contact(ScopeType::Email, "invalid@nodot").is_err());

    // Personal details with and without native names
    let pd = value_map_from_pairs(&[
        ("first_name", "Ada"),
        ("last_name", "Lovelace"),
        ("birth_date", "10.12.1815"),
        ("gender", "female"),
        ("country_code", "GB"),
        ("residence_country_code", "GB"),
    ]);
    assert!(validate_personal_details(&pd, false).is_empty());
    let native_errs = validate_personal_details(&pd, true);
    assert!(native_errs.contains_key("first_name_native"));
    assert!(native_errs.contains_key("last_name_native"));

    // Identity document
    let doc = value_map_from_pairs(&[("document_no", "AB123456"), ("expiry_date", "01.01.2030")]);
    assert!(validate_identity_document(&doc).is_empty());

    // Address
    let addr = value_map_from_pairs(&[
        ("street_line1", "10 Downing St"),
        ("city", "London"),
        ("country_code", "GB"),
        ("post_code", "SW1A2AA"),
    ]);
    assert!(validate_address(&addr).is_empty());

    // Native name visibility & SkipFieldCheck
    assert_eq!(
        native_names_visibility("", "en"),
        AdditionalVisibility::Hidden
    );
    assert_eq!(
        native_names_visibility("US", "en"),
        AdditionalVisibility::OnlyIfError
    );
    assert_eq!(
        native_names_visibility("RU", "ru"),
        AdditionalVisibility::Shown
    );

    let mut pd_val = PassportValue::new(ValueType::PersonalDetails);
    pd_val.native_names = false;
    assert!(skip_field_check(&pd_val, "first_name_native"));
    assert!(!skip_field_check(&pd_val, "first_name"));
    pd_val.native_names = true;
    assert!(skip_field_check(&pd_val, "first_name"));
    assert!(!skip_field_check(&pd_val, "first_name_native"));
}

#[test]
fn form_validation_and_scope_inlining() {
    let mut form = PassportForm::default();
    form.values.insert(
        ValueType::PersonalDetails,
        PassportValue::new(ValueType::PersonalDetails),
    );
    form.values
        .insert(ValueType::Passport, PassportValue::new(ValueType::Passport));
    form.values.insert(
        ValueType::DriverLicense,
        PassportValue::new(ValueType::DriverLicense),
    );
    form.values
        .insert(ValueType::Phone, PassportValue::new(ValueType::Phone));
    form.request = vec![
        vec![ValueType::PersonalDetails],
        vec![ValueType::Passport, ValueType::DriverLicense],
        vec![ValueType::Phone],
    ];
    assert!(validate_form(&form).is_ok());

    // PersonalDetails inlines into single Identity one_of row!
    let scopes = compute_scopes(&form);
    assert_eq!(scopes.len(), 2);
    assert_eq!(scopes[0].scope_type, ScopeType::Identity);
    assert_eq!(scopes[0].details, Some(ValueType::PersonalDetails));
    assert_eq!(
        scopes[0].documents,
        vec![ValueType::Passport, ValueType::DriverLicense]
    );
    assert_eq!(scopes[1].scope_type, ScopeType::Phone);

    // Invalid: duplicate type across rows
    let mut dup_form = form.clone();
    dup_form.request.push(vec![ValueType::Phone]);
    assert!(validate_form(&dup_form).is_err());

    // Invalid: selfie_required on PersonalDetails
    let mut bad_selfie = form.clone();
    bad_selfie
        .values
        .get_mut(&ValueType::PersonalDetails)
        .unwrap()
        .selfie_required = true;
    assert!(validate_form(&bad_selfie).is_err());

    // Scan limit check
    let mut utility = PassportValue::new(ValueType::UtilityBill);
    for i in 0..DOCUMENT_SCANS_LIMIT {
        utility.scans_in_edit.push(EditFile {
            file_type: FileType::Scan,
            fields: PassportFile {
                id: i as i64,
                ..PassportFile::default()
            },
            upload_data: None,
            deleted: false,
        });
    }
    assert!(!utility.can_add_scan(FileType::Scan));
}

// ============================================================================
// PassportController End-to-End State Machine
// ============================================================================

#[test]
fn passport_controller_unlock_decrypt_save_and_accept_authorization() {
    let bot_pem = PRODUCTION_PUBLIC_KEYS[0];
    let scope_json = r#"{"data":["personal_details","passport","phone_number"],"v":1}"#;
    assert_eq!(nonce_name_by_scope(scope_json), "nonce");
    assert_eq!(nonce_name_by_scope("id address"), "payload");

    // Empty nonce produces NONCE_EMPTY
    let empty_req = FormRequest::new(UserId(500), scope_json, "", bot_pem, "");
    let empty_ctrl = PassportController::new(empty_req);
    assert!(matches!(
        empty_ctrl.build_get_authorization_form_request(),
        Err(PassportError::MissingNonce("NONCE_EMPTY"))
    ));

    // Set up valid request with callback URL
    let req = FormRequest::new(
        UserId(500),
        scope_json,
        "https://bot.example/passport/done",
        bot_pem,
        "bot-nonce-xyz",
    );
    let mut ctrl = PassportController::new(req);
    ctrl.set_passport_config(PassportConfig::parse_json(1, br#"{"GB":"en","RU":"ru"}"#).unwrap());

    // Prepare encrypted server-side Passport values using a known master secret + password
    let password = b"my-2fa-password";
    let kdf_algo = SecureSecretAlgo::Sha512 {
        salt: b"test-salt-sha512".to_vec(),
    };
    let hash_for_secret = compute_secure_secret_hash(&kdf_algo, password);
    let master_secret = generate_secret_bytes();
    let master_secret_id = count_secure_secret_id(&master_secret);
    let encrypted_master_secret = encrypt_secure_secret(&master_secret, &hash_for_secret).unwrap();

    // Encrypt PersonalDetails (without native names -> will auto-fill from GB/en fallback!)
    let mut pd_map = BTreeMap::new();
    pd_map.insert("first_name".to_string(), "Ada".to_string());
    pd_map.insert("last_name".to_string(), "Lovelace".to_string());
    pd_map.insert("birth_date".to_string(), "10.12.1815".to_string());
    pd_map.insert("gender".to_string(), "female".to_string());
    pd_map.insert("country_code".to_string(), "GB".to_string());
    pd_map.insert("residence_country_code".to_string(), "GB".to_string());
    let pd_enc = encrypt_data(&serialize_data(&pd_map).unwrap()).unwrap();
    let pd_enc_secret = encrypt_value_secret(&pd_enc.secret, &master_secret, &pd_enc.hash).unwrap();

    // Encrypt Passport document data + front_side scan
    let mut pass_map = BTreeMap::new();
    pass_map.insert("document_no".to_string(), "UK998877".to_string());
    let pass_enc = encrypt_data(&serialize_data(&pass_map).unwrap()).unwrap();
    let pass_enc_secret =
        encrypt_value_secret(&pass_enc.secret, &master_secret, &pass_enc.hash).unwrap();

    let scan_enc = encrypt_data(b"front-scan-jpeg").unwrap();
    let scan_enc_secret =
        encrypt_value_secret(&scan_enc.secret, &master_secret, &scan_enc.hash).unwrap();

    let auth_form_tl = tl_enums::account::AuthorizationForm::AuthorizationForm(
        tl_types::account::AuthorizationForm {
            required_types: vec![
                tl_enums::SecureRequiredType::SecureRequiredType(tl_types::SecureRequiredType {
                    native_names: true,
                    selfie_required: false,
                    translation_required: false,
                    r#type: tl_enums::SecureValueType::PersonalDetails,
                }),
                tl_enums::SecureRequiredType::SecureRequiredType(tl_types::SecureRequiredType {
                    native_names: false,
                    selfie_required: false,
                    translation_required: false,
                    r#type: tl_enums::SecureValueType::Passport,
                }),
                tl_enums::SecureRequiredType::SecureRequiredType(tl_types::SecureRequiredType {
                    native_names: false,
                    selfie_required: false,
                    translation_required: false,
                    r#type: tl_enums::SecureValueType::Phone,
                }),
            ],
            values: vec![
                tl_enums::SecureValue::SecureValue(tl_types::SecureValue {
                    r#type: tl_enums::SecureValueType::PersonalDetails,
                    data: Some(tl_enums::SecureData::SecureData(tl_types::SecureData {
                        data: pd_enc.bytes.clone(),
                        data_hash: pd_enc.hash.to_vec(),
                        secret: pd_enc_secret.to_vec(),
                    })),
                    front_side: None,
                    reverse_side: None,
                    selfie: None,
                    translation: None,
                    files: None,
                    plain_data: None,
                    hash: vec![0x11; 32],
                }),
                tl_enums::SecureValue::SecureValue(tl_types::SecureValue {
                    r#type: tl_enums::SecureValueType::Passport,
                    data: Some(tl_enums::SecureData::SecureData(tl_types::SecureData {
                        data: pass_enc.bytes.clone(),
                        data_hash: pass_enc.hash.to_vec(),
                        secret: pass_enc_secret.to_vec(),
                    })),
                    front_side: Some(tl_enums::SecureFile::SecureFile(tl_types::SecureFile {
                        id: 7001,
                        access_hash: 8001,
                        size: scan_enc.bytes.len() as i64,
                        dc_id: 2,
                        date: 1_700_000_000,
                        file_hash: scan_enc.hash.to_vec(),
                        secret: scan_enc_secret.to_vec(),
                    })),
                    reverse_side: None,
                    selfie: None,
                    translation: None,
                    files: None,
                    plain_data: None,
                    hash: vec![0x22; 32],
                }),
                tl_enums::SecureValue::SecureValue(tl_types::SecureValue {
                    r#type: tl_enums::SecureValueType::Phone,
                    data: None,
                    front_side: None,
                    reverse_side: None,
                    selfie: None,
                    translation: None,
                    files: None,
                    plain_data: Some(tl_enums::SecurePlainData::SecurePlainPhone(
                        tl_types::SecurePlainPhone {
                            phone: "447700900123".to_string(),
                        },
                    )),
                    hash: vec![0x33; 32],
                }),
            ],
            errors: Vec::new(),
            users: Vec::new(),
            privacy_policy_url: Some("https://bot.example/privacy".to_string()),
        },
    );

    ctrl.apply_authorization_form_tl(&auth_form_tl).unwrap();
    assert_eq!(
        ctrl.form().privacy_policy_url,
        "https://bot.example/privacy"
    );

    // Apply account.password -> NeedPassword
    let password_tl = tl_enums::account::Password::Password(tl_types::account::Password {
        has_recovery: true,
        has_secure_values: true,
        has_password: true,
        current_algo: Some(
            tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(
                tl_types::PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow {
                    salt1: b"s1".to_vec(),
                    salt2: b"s2".to_vec(),
                    g: 3,
                    p: GOOD_PRIME.to_vec(),
                },
            ),
        ),
        srp_b: Some(vec![0x02; 256]),
        srp_id: Some(12345),
        hint: Some("my hint".to_string()),
        email_unconfirmed_pattern: None,
        new_algo: tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(
            tl_types::PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow {
                salt1: b"ns1".to_vec(),
                salt2: b"ns2".to_vec(),
                g: 3,
                p: GOOD_PRIME.to_vec(),
            },
        ),
        new_secure_algo: tl_enums::SecurePasswordKdfAlgo::PBKDF2HMACSHA512iter100000(
            tl_types::SecurePasswordKdfAlgoPBKDF2HMACSHA512iter100000 {
                salt: b"nsec".to_vec(),
            },
        ),
        secure_random: vec![0x42; 32],
        pending_reset_date: None,
        login_email_pattern: None,
    });
    assert!(matches!(
        ctrl.apply_password_tl(&password_tl),
        PasswordFlowState::NeedPassword { hint, .. } if hint == "my hint"
    ));

    // Apply account.passwordSettings -> unlocks secret, decrypts values, runs native fallback
    let settings_tl = tl_enums::account::PasswordSettings::PasswordSettings(
        tl_types::account::PasswordSettings {
            email: Some("ada@example.com".to_string()),
            secure_settings: Some(tl_enums::SecureSecretSettings::SecureSecretSettings(
                tl_types::SecureSecretSettings {
                    secure_algo: tl_enums::SecurePasswordKdfAlgo::SHA512(
                        tl_types::SecurePasswordKdfAlgoSHA512 {
                            salt: b"test-salt-sha512".to_vec(),
                        },
                    ),
                    secure_secret: encrypted_master_secret.to_vec(),
                    secure_secret_id: master_secret_id as i64,
                },
            )),
        },
    );
    let outcome = ctrl
        .apply_password_settings_tl(&settings_tl, password, [0x77; 32])
        .unwrap();
    assert!(matches!(outcome, UnlockOutcome::Unlocked { .. }));
    assert_eq!(ctrl.state(), &PasswordFlowState::Unlocked);

    // Decrypted fields + auto-filled native names from GB/en fallback!
    let pd = ctrl.form().values.get(&ValueType::PersonalDetails).unwrap();
    assert_eq!(pd.data.parsed.text("first_name"), "Ada");
    assert_eq!(pd.data.parsed.text("first_name_native"), "Ada");
    assert_eq!(pd.data.parsed.text("last_name_native"), "Lovelace");

    // Scope rows should all be ready
    let scopes = compute_scopes(ctrl.form());
    for scope in &scopes {
        assert!(compute_scope_row_ready(ctrl.form(), scope).is_some());
        let row = compute_scope_row(ctrl.form(), scope);
        assert!(row.error.is_none());
    }

    // Prepare account.acceptAuthorization and verify credentials JSON + TL round-trip
    let prepared = ctrl.prepare_accept_authorization().unwrap();
    let creds_json: JsonValue = serde_json::from_slice(&prepared.credentials_json).unwrap();
    assert_eq!(creds_json["nonce"], "bot-nonce-xyz");
    assert!(creds_json["secure_data"]["personal_details"]["data"]["data_hash"].is_string());
    assert!(creds_json["secure_data"]["passport"]["front_side"]["file_hash"].is_string());
    assert!(creds_json["secure_data"].get("phone").is_none());
    assert_eq!(prepared.request.value_hashes.len(), 3);

    let wire = prepared.request.to_bytes();
    let decoded = tg_tl::functions::account::AcceptAuthorization::from_bytes(&wire).unwrap();
    assert_eq!(decoded, prepared.request);

    // Prepare saving an edited value (encrypted + plain)
    let save_req = ctrl
        .prepare_save_value(
            ValueType::PersonalDetails,
            ValueMap::from_texts([("middle_name", "King")]),
        )
        .unwrap();
    assert_eq!(save_req.secure_secret_id, master_secret_id as i64);
    let phone_save_req = ctrl
        .prepare_save_value(
            ValueType::Phone,
            ValueMap::from_texts([("value", "447700900999")]),
        )
        .unwrap();
    assert!(matches!(
        phone_save_req.value,
        tl_enums::InputSecureValue::InputSecureValue(ref v) if v.plain_data.is_some()
    ));

    // Callback URL outcomes
    assert_eq!(
        ctrl.callback_url_for_outcome(&CallbackOutcome::Success)
            .as_deref(),
        Some("https://bot.example/passport/done?tg_passport=success")
    );
    assert_eq!(
        ctrl.callback_url_for_outcome(&CallbackOutcome::Cancel)
            .as_deref(),
        Some("https://bot.example/passport/done?tg_passport=cancel")
    );
    assert_eq!(
        ctrl.callback_url_for_outcome(&CallbackOutcome::Error("PUBLIC_KEY_INVALID".into()))
            .as_deref(),
        Some("https://bot.example/passport/done?tg_passport=error&error=PUBLIC_KEY_INVALID")
    );
    assert_eq!(
        ctrl.callback_url_for_outcome(&CallbackOutcome::Error("RANDOM_ERROR".into())),
        None
    );
}
