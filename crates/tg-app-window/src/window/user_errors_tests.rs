//! A `400` from the server at representative call sites shows tdesktop's text, never the raw
//! error (TGB-2957).

use super::*;
use crate::mock_session::MockSession;

fn controller() -> WindowController {
    WindowController::with_session(
        MockSession::new(true),
        ShellSettings::default(),
        std::env::temp_dir().join("rustgram_user_errors_test.json"),
    )
}

fn fail_next(ctrl: &mut WindowController, err: &str) {
    ctrl.session_backend
        .as_any_mut()
        .downcast_mut::<MockSession>()
        .expect("mock session")
        .fail_next_request(err);
}

fn assert_no_raw(text: &str) {
    for raw in ["RpcError {", "RPC error", "invocation error", "Could not"] {
        assert!(
            !text.contains(raw),
            "raw error text {raw:?} in toast {text:?}"
        );
    }
}

fn join(ctrl: &mut WindowController, is_group: bool) {
    let ctx = egui::Context::default();
    ctrl.handle_box_event(
        BoxEvent::JoinInviteConfirmed {
            hash: "AbCdEf".to_owned(),
            is_group,
        },
        &ctx,
    );
}

#[test]
fn join_chat_400_shows_localized_toast() {
    let mut ctrl = controller();
    fail_next(&mut ctrl, "RPC error 400: USERS_TOO_MUCH");
    join(&mut ctrl, true);
    let text = ctrl.layer_stack.toast_text().expect("toast").to_owned();
    assert_eq!(text, tg_lang::tr::lng_group_invite_no_room());
    assert_no_raw(&text);

    let mut ctrl = controller();
    fail_next(&mut ctrl, "RPC error 400: INVITE_HASH_EXPIRED");
    join(&mut ctrl, true);
    assert_eq!(
        ctrl.layer_stack.toast_text(),
        Some(tg_lang::tr::lng_group_invite_bad_link().as_str())
    );

    // The old debug form some backends may still produce maps the same way.
    let mut ctrl = controller();
    fail_next(
        &mut ctrl,
        "RPC error RpcError { code: 400, name: \"INVITE_REQUEST_SENT\", value: None }",
    );
    join(&mut ctrl, false);
    assert_eq!(
        ctrl.layer_stack.toast_text(),
        Some(tg_lang::tr::lng_group_request_sent_channel().as_str())
    );
}

#[test]
fn join_chat_channels_too_much_shows_limit_box_not_toast() {
    let mut ctrl = controller();
    fail_next(&mut ctrl, "RPC error 400: CHANNELS_TOO_MUCH");
    join(&mut ctrl, true);
    assert_eq!(ctrl.layer_stack.toast_text(), None);
    assert!(ctrl.layer_stack.is_layer_shown());
}

#[test]
fn unmapped_rpc_failure_in_any_toast_shows_only_the_error_type() {
    let mut ctrl = controller();
    ctrl.show_toast("Could not frobnicate: RPC error 400: PEER_ID_INVALID");
    assert_eq!(ctrl.layer_stack.toast_text(), Some("PEER_ID_INVALID"));
}
