//! Intro passkey login RPC helpers (`auth.initPasskeyLogin` / `auth.finishPasskeyLogin`).
//!
//! Port origin: `Telegram/SourceFiles/data/components/passkeys.cpp` and
//! `Telegram/SourceFiles/intro/intro_qr.cpp` (`setupPasskeyLink`).

use tg_api::error::{ApiError, map_login_error};
use tg_api::rpc::{DcId, InvocationError};
use tg_api::types::ApiCredentials;
use tg_platform::webauthn::{
    LoginData, LoginResult, WebAuthnErrorKind, deserialize_login_data, encode_base64url,
    parse_user_handle,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::intro::DynIntroRpc;

/// Builds the TL `InputPasskeyCredential` and target DC ID for `auth.finishPasskeyLogin`
/// (`passkeys.cpp:168-189`).
pub fn build_login_credential_tl(
    result: &LoginResult,
) -> Result<(DcId, tl_enums::InputPasskeyCredential), WebAuthnErrorKind> {
    let parsed_handle = parse_user_handle(&result.user_handle)?;
    let user_handle_str = String::from_utf8_lossy(&result.user_handle).into_owned();
    let credential_id_b64 = encode_base64url(&result.credential_id);
    let client_data_str = String::from_utf8_lossy(&result.client_data_json).into_owned();

    let credential =
        tl_enums::InputPasskeyCredential::PublicKey(tl_types::InputPasskeyCredentialPublicKey {
            id: credential_id_b64.clone(),
            raw_id: credential_id_b64,
            response: tl_enums::InputPasskeyResponse::Login(tl_types::InputPasskeyResponseLogin {
                client_data: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                    data: client_data_str,
                }),
                authenticator_data: result.authenticator_data.clone(),
                signature: result.signature.clone(),
                user_handle: user_handle_str,
            }),
        });
    Ok((parsed_handle.dc_id, credential))
}

// ============================================================================
// Intro Passkey Login RPCs (`InitPasskeyLogin` / `FinishPasskeyLogin`)
// ============================================================================

/// Calls `auth.initPasskeyLogin` and parses the returned WebAuthn [`LoginData`].
pub async fn init_passkey_login_async(
    rpc: &dyn DynIntroRpc,
    credentials: &ApiCredentials,
) -> Result<(LoginData, DcId), ApiError> {
    let initial_dc = rpc.main_dc_id();
    let resp = rpc
        .auth_init_passkey_login(tl_fns::auth::InitPasskeyLogin {
            api_id: credentials.api_id,
            api_hash: credentials.api_hash.clone(),
        })
        .await
        .map_err(map_invocation_error)?;

    let tl_enums::auth::PasskeyLoginOptions::PasskeyLoginOptions(opts) = resp;
    let tl_enums::DataJSON::DataJSON(json_data) = opts.options;
    let login_data = deserialize_login_data(json_data.data.as_bytes()).map_err(|err| {
        ApiError::ServerError(format!("failed to parse passkey login options: {err}"))
    })?;
    Ok((login_data, initial_dc))
}

/// Calls `auth.finishPasskeyLogin` on the user's home DC encoded in `result.user_handle`.
pub async fn finish_passkey_login_async(
    rpc: &dyn DynIntroRpc,
    initial_dc: DcId,
    from_auth_key_id: Option<i64>,
    result: &LoginResult,
) -> Result<tl_enums::auth::Authorization, ApiError> {
    let (user_dc, credential) = build_login_credential_tl(result).map_err(|_| {
        ApiError::Login(tg_api::error::LoginErrorKind::Other(
            "PASSKEY_HANDLE_INVALID".to_owned(),
        ))
    })?;

    rpc.set_main_dc_id(user_dc);
    let req = tl_fns::auth::FinishPasskeyLogin {
        credential,
        from_dc_id: from_auth_key_id.map(|_| initial_dc),
        from_auth_key_id,
    };
    rpc.auth_finish_passkey_login(user_dc, req)
        .await
        .map_err(map_invocation_error)
}

fn map_invocation_error(err: InvocationError) -> ApiError {
    match err {
        InvocationError::Rpc(rpc_err) => ApiError::Login(map_login_error(&rpc_err)),
        other => ApiError::Invocation(other),
    }
}
