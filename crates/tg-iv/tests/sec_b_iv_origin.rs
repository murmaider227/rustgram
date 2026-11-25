//! Security review #1 (SEC-B, item B2) regression tests for `tg-iv`:
//! - Instant View bridge commands must be authorized against the controller's expected IV origin.

use tg_iv::{ControllerAction, IvController, ZOOM_DEFAULT};

#[test]
fn b2_iv_controller_authorizes_bridge_sender_against_expected_origin() {
    let mut ctrl = IvController::new("rustgram-internal://iv", ZOOM_DEFAULT);
    let link_cmd =
        r#"{"type":"link_click","href":"https://example.org/article","new_window":true}"#;

    // Legitimate IV document origin is accepted.
    assert!(ctrl.is_authorized_bridge_sender("rustgram-internal://iv/index.html"));
    assert!(ctrl.is_authorized_bridge_sender("rustgram-internal://iv"));
    assert!(ctrl.is_authorized_bridge_sender("RUSTGRAM-INTERNAL://IV/page?v=1#top"));
    let actions = ctrl
        .handle_bridge_json_from_source(link_cmd, "rustgram-internal://iv/index.html")
        .expect("IV origin must be accepted");
    assert!(matches!(
        actions.as_slice(),
        [ControllerAction::OpenExternal { .. }]
    ));

    // Untrusted, foreign, empty, userinfo-spoofed, or prefix-confused origins are rejected.
    let bad_sources = [
        "",
        "about:blank",
        "https://evil.example/page",
        "https://embed.magic.org/frame",
        "rustgram-internal://iv.evil.example/index.html",
        "rustgram-internal://iv@evil.example/index.html",
        "file:///C:/Windows/System32/calc.exe",
    ];
    for bad in bad_sources {
        assert!(
            !ctrl.is_authorized_bridge_sender(bad),
            "must reject unauthorized IV sender '{bad}'"
        );
        assert!(
            ctrl.handle_bridge_json_from_source(link_cmd, bad).is_err(),
            "handle_bridge_json_from_source must error for '{bad}'"
        );
    }
}
