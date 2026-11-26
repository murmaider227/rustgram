//! `InvocationError`/`RpcError` display for logs and reading the error back from a string
//! (TGB-2957).

use tg_api::{ApiError, InvocationError, RpcError};

#[test]
fn rpc_display_is_code_and_name_not_debug() {
    let err = InvocationError::Rpc(RpcError::new(400, "CHANNELS_TOO_MUCH"));
    assert_eq!(err.to_string(), "RPC error 400: CHANNELS_TOO_MUCH");
    let api = ApiError::Invocation(err);
    assert!(!api.to_string().contains("RpcError {"), "{api}");
}

#[test]
fn find_in_text_reads_wrapped_display_form() {
    let text = format!(
        "messages.importChatInvite failed: {}",
        ApiError::Invocation(RpcError::new(420, "FLOOD_WAIT_30").into())
    );
    let found = RpcError::find_in_text(&text).expect("rpc error in text");
    assert_eq!(found.code, 420);
    assert_eq!(found.name, "FLOOD_WAIT_30");
    assert_eq!(found.flood_wait_seconds(), Some(30));
}

#[test]
fn find_in_text_reads_old_debug_form_and_mtproto_form() {
    let old = "RPC error RpcError { code: 400, name: \"STICKERSET_INVALID\", value: None }";
    assert_eq!(
        RpcError::find_in_text(old),
        Some(RpcError::new(400, "STICKERSET_INVALID"))
    );
    let mtproto = "RPC error 500: INTERNAL: something went wrong";
    assert_eq!(
        RpcError::find_in_text(mtproto),
        Some(RpcError::new(500, "INTERNAL"))
    );
    assert_eq!(
        RpcError::find_in_text("RPC error -503: Timeout"),
        Some(RpcError::new(-503, "Timeout"))
    );
}

#[test]
fn find_in_text_ignores_non_rpc_text() {
    assert_eq!(
        RpcError::find_in_text("transport error: connection reset"),
        None
    );
    assert_eq!(RpcError::find_in_text("RPC error : X"), None);
    assert_eq!(RpcError::find_in_text(""), None);
}
