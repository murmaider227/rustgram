//! Runtime API credential loading and MTProto `initConnection` configuration assembly.
//!
//! Port origin: `main/main_account.cpp` (`Account::startMtp`), `mtproto/session_private.cpp`
//! (`prepareInitParams`), and `core/application.cpp` (app metadata).
//!
//! Credentials (`api_id` and `api_hash`) are never embedded in code; they are always
//! read from `RUSTGRAM_API_ID` and `RUSTGRAM_API_HASH` (with legacy `TGFORK_API_ID` /
//! `TGFORK_API_HASH` fallback) at runtime or injected by the caller.

use tg_api::ApiCredentials;
use tg_core::domain::Environment as CoreEnvironment;
use tg_core::version::{APP_NAME, APP_VERSION_STR};
use tg_lang::{CLOUD_LANG_PACK_NAME, default_language_id, language_id_or_default, with_current};
use tg_mtproto::{ClientConfig, DcId, Environment as MtprotoEnvironment};
use tg_platform::system_info;

use crate::error::{Result, SessionError};

/// Environment variable name holding the Telegram application `api_id`.
pub const ENV_API_ID: &str = "RUSTGRAM_API_ID";

/// Environment variable name holding the Telegram application `api_hash`.
pub const ENV_API_HASH: &str = "RUSTGRAM_API_HASH";

/// Legacy fallback environment variable name for `api_id`.
pub const LEGACY_ENV_API_ID: &str = "TGFORK_API_ID";

/// Legacy fallback environment variable name for `api_hash`.
pub const LEGACY_ENV_API_HASH: &str = "TGFORK_API_HASH";

/// Default initial Telegram DC ID (`DC 2`) when none is stored yet.
pub const DEFAULT_MAIN_DC_ID: DcId = 2;

/// Reads Telegram `ApiCredentials` from `RUSTGRAM_API_ID` and `RUSTGRAM_API_HASH`
/// (falling back to `TGFORK_API_ID` and `TGFORK_API_HASH`).
pub fn load_api_credentials_from_env() -> Result<ApiCredentials> {
    let id_raw = env_var_with_fallback(ENV_API_ID, LEGACY_ENV_API_ID);
    let hash_raw = env_var_with_fallback(ENV_API_HASH, LEGACY_ENV_API_HASH);
    load_api_credentials_from_vars(id_raw.as_deref(), hash_raw.as_deref())
}

fn env_var_with_fallback(primary: &str, legacy: &str) -> Option<String> {
    std::env::var(primary)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| std::env::var(legacy).ok().filter(|v| !v.trim().is_empty()))
}

/// Parses `ApiCredentials` from optional raw environment string values.
pub fn load_api_credentials_from_vars(
    api_id_raw: Option<&str>,
    api_hash_raw: Option<&str>,
) -> Result<ApiCredentials> {
    let id_str = api_id_raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(SessionError::MissingEnvVar(ENV_API_ID))?;
    let api_id = id_str
        .parse::<i32>()
        .ok()
        .filter(|&v| v > 0)
        .ok_or_else(|| SessionError::InvalidApiId(id_str.to_owned()))?;

    let api_hash = api_hash_raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(SessionError::MissingEnvVar(ENV_API_HASH))?
        .to_owned();

    Ok(ApiCredentials { api_id, api_hash })
}

/// Converts [`tg_core::Environment`] into [`tg_mtproto::Environment`].
#[must_use]
pub const fn core_env_to_mtproto(env: CoreEnvironment) -> MtprotoEnvironment {
    match env {
        CoreEnvironment::Production => MtprotoEnvironment::Production,
        CoreEnvironment::Test => MtprotoEnvironment::Test,
    }
}

/// Converts [`tg_mtproto::Environment`] into [`tg_core::Environment`].
#[must_use]
pub const fn mtproto_env_to_core(env: MtprotoEnvironment) -> CoreEnvironment {
    match env {
        MtprotoEnvironment::Production => CoreEnvironment::Production,
        MtprotoEnvironment::Test => CoreEnvironment::Test,
    }
}

/// Builds a [`tg_mtproto::ClientConfig`] populated with runtime `initConnection` parameters:
/// - `api_id` from `credentials`
/// - `device_model` and `system_version` from `tg-platform`
/// - `app_version` from `tg-core`
/// - `lang_code` / `lang_pack` / `system_lang_code` from `tg-lang` and `tg-platform`
/// - `tz_offset_secs` from the local system clock
#[must_use]
pub fn build_mtproto_client_config(
    credentials: &ApiCredentials,
    main_dc_id: DcId,
    environment: CoreEnvironment,
    lang_code_override: Option<&str>,
) -> ClientConfig {
    let device_model = non_empty_or(system_info::device_model_pretty(), "Rustgram Desktop");
    let system_version = non_empty_or(system_info::system_version_pretty(), "Windows");
    let system_lang_code = non_empty_or(system_info::system_language(), default_language_id());
    let resolved_lang = match lang_code_override {
        Some(code) => language_id_or_default(code).to_owned(),
        None => with_current(|inst| language_id_or_default(inst.id()).to_owned()),
    };
    let tz_offset_secs = chrono::Local::now().offset().local_minus_utc();

    ClientConfig {
        api_id: credentials.api_id,
        device_model,
        system_version,
        app_version: format!("{APP_NAME} {APP_VERSION_STR}"),
        system_lang_code,
        lang_pack: CLOUD_LANG_PACK_NAME.to_owned(),
        lang_code: resolved_lang,
        tz_offset_secs: Some(tz_offset_secs),
        environment: core_env_to_mtproto(environment),
        main_dc_id: if main_dc_id > 0 {
            main_dc_id
        } else {
            DEFAULT_MAIN_DC_ID
        },
        ..ClientConfig::default()
    }
}

fn non_empty_or(value: String, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_credentials_validation() {
        let missing_id = load_api_credentials_from_vars(None, Some("abcdef"));
        assert!(matches!(
            missing_id,
            Err(SessionError::MissingEnvVar(ENV_API_ID))
        ));

        let missing_hash = load_api_credentials_from_vars(Some("12345"), Some("  "));
        assert!(matches!(
            missing_hash,
            Err(SessionError::MissingEnvVar(ENV_API_HASH))
        ));

        let bad_id = load_api_credentials_from_vars(Some("not_a_number"), Some("abcdef"));
        assert!(matches!(bad_id, Err(SessionError::InvalidApiId(_))));

        let zero_id = load_api_credentials_from_vars(Some("0"), Some("abcdef"));
        assert!(matches!(zero_id, Err(SessionError::InvalidApiId(_))));

        let valid = load_api_credentials_from_vars(Some(" 4242 "), Some(" deadbeef "))
            .expect("valid credentials");
        assert_eq!(valid.api_id, 4242);
        assert_eq!(valid.api_hash, "deadbeef");
    }

    #[test]
    fn client_config_populates_init_connection_fields() {
        let creds = ApiCredentials {
            api_id: 99999,
            api_hash: "synthetic_hash".into(),
        };
        let cfg = build_mtproto_client_config(&creds, 4, CoreEnvironment::Test, Some("uk"));
        assert_eq!(cfg.api_id, 99999);
        assert_eq!(cfg.main_dc_id, 4);
        assert_eq!(cfg.environment, MtprotoEnvironment::Test);
        assert_eq!(cfg.app_version, format!("{APP_NAME} {APP_VERSION_STR}"));
        assert_eq!(cfg.lang_pack, CLOUD_LANG_PACK_NAME);
        assert_eq!(cfg.lang_code, "uk");
        assert!(!cfg.device_model.is_empty());
        assert!(!cfg.system_version.is_empty());
        assert!(!cfg.system_lang_code.is_empty());
        assert!(cfg.tz_offset_secs.is_some());

        let fallback_dc = build_mtproto_client_config(&creds, 0, CoreEnvironment::Production, None);
        assert_eq!(fallback_dc.main_dc_id, DEFAULT_MAIN_DC_ID);
        assert_eq!(fallback_dc.environment, MtprotoEnvironment::Production);
        assert_eq!(fallback_dc.lang_code, "en");
    }
}
