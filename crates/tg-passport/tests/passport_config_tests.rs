//! Unit tests for `help.getPassportConfig` (`PassportConfig` + `PassportConfigNotModified`)
//! and `PassportController` country-to-language fallback integration.

use tg_data::UserId;
use tg_passport::controller::PassportController;
use tg_passport::types::{FormRequest, PassportConfig, PassportValue, ValueType};
use tg_tl::{enums as tl_enums, types as tl_types};

#[test]
fn passport_config_request_and_not_modified_caching() {
    let req = FormRequest::new(
        UserId::from_tl(55_001),
        "id",
        "https://example.invalid/cb",
        "-----BEGIN PUBLIC KEY-----\ntest\n-----END PUBLIC KEY-----",
        "nonce-1",
    );
    let mut ctrl = PassportController::new(req);

    // Initial request sends hash = 0 (`passport_form_controller.cpp:1248-1257`).
    let first_rpc = ctrl.build_get_passport_config_request();
    assert_eq!(first_rpc.hash, 0);

    // Apply `help.passportConfig`.
    let tl_cfg = tl_enums::help::PassportConfig::PassportConfig(tl_types::help::PassportConfig {
        hash: 0x1357_2468,
        countries_langs: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
            data: r#"{"US":"en","GB":"en","DE":"de","JP":"ja"}"#.to_owned(),
        }),
    });
    let changed = ctrl
        .apply_passport_config_tl(&tl_cfg)
        .expect("parse passport config");
    assert!(changed);
    assert_eq!(ctrl.passport_config().hash, 0x1357_2468);
    assert_eq!(ctrl.passport_config().preferred_language("US"), "en");
    assert_eq!(ctrl.passport_config().preferred_language("DE"), "de");
    assert_eq!(ctrl.passport_config().preferred_language("ZZ"), "");

    // Next request carries the cached hash.
    let second_rpc = ctrl.build_get_passport_config_request();
    assert_eq!(second_rpc.hash, 0x1357_2468);

    // `help.passportConfigNotModified` preserves the cached config.
    let not_mod = ctrl
        .apply_passport_config_tl(&tl_enums::help::PassportConfig::NotModified)
        .expect("not modified");
    assert!(!not_mod);
    assert_eq!(ctrl.passport_config().hash, 0x1357_2468);
    assert_eq!(ctrl.passport_config().preferred_language("JP"), "ja");
}

#[test]
fn passport_config_enables_native_name_fallback_for_english_country() {
    let req = FormRequest::new(
        UserId::from_tl(55_002),
        "id",
        "",
        "-----BEGIN PUBLIC KEY-----\ntest\n-----END PUBLIC KEY-----",
        "nonce-2",
    );
    let mut ctrl = PassportController::new(req);

    let mut details = PassportValue::new(ValueType::PersonalDetails);
    details.native_names = true;
    details.data.parsed.set_text("residence_country_code", "US");
    details.data.parsed.set_text("first_name", "Alex");
    details.data.parsed.set_text("last_name", "River");
    ctrl.form_mut()
        .values
        .insert(ValueType::PersonalDetails, details);

    // Without config, preferred language for "US" is "" (not English), so fallback does not fire.
    assert!(!ctrl.fill_native_from_fallback());

    // After applying `help.passportConfig` with `"US": "en"`, fallback populates native names.
    let cfg = PassportConfig::parse_json(99, br#"{"US":"en"}"#).expect("valid config");
    ctrl.set_passport_config(cfg);
    assert!(ctrl.fill_native_from_fallback());

    let parsed = &ctrl.form().values[&ValueType::PersonalDetails].data.parsed;
    assert_eq!(parsed.text("first_name_native"), "Alex");
    assert_eq!(parsed.text("last_name_native"), "River");
}
