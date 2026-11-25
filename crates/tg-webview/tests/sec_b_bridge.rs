//! Security review #1 (SEC-B, item B1) regression tests for `tg-webview`:
//! - Embedded WebView mode derives origin strictly from WebView2 `source_url` and rejects raw
//!   object payloads (including payloads carrying a forged `"origin"`).
//! - External-shell object payloads require external-shell mode, a verified `web.telegram.org`
//!   sender, and the exact session shell token.

use std::time::Instant;

use tg_theme::Palette;
use tg_webview::{
    EXTERNAL_MESSAGE_TYPE, MiniAppAction, MiniAppController, MiniAppPanelConfig, WebMessage,
    is_external_shell_origin, parse_mini_app_event, parse_native_message,
    parse_native_message_with_shell,
};

#[test]
fn b1_forged_origin_object_payload_is_rejected_in_standard_mode() {
    let palette = Palette::default();
    let now = Instant::now();

    let mut ctrl = MiniAppController::new(MiniAppPanelConfig {
        initial_url: "https://trusted.bot.example/app".to_owned(),
        same_origin: true,
        allow_clipboard_read: false,
        fullscreen: false,
    });

    // An evil page navigated in the WebView attempts to spoof the trusted origin inside a JSON
    // object payload. Old code accepted `obj["origin"]` and passed the same-origin check!
    let forged = WebMessage {
        text:
            r#"{"eventType":"web_app_ready","eventData":{},"origin":"https://trusted.bot.example"}"#
                .to_owned(),
        source_url: "https://evil.attacker.example/phish".to_owned(),
    };
    assert!(
        parse_native_message(&forged.text, &forged.source_url).is_err(),
        "standard embedded mode must reject JSON object payloads"
    );
    assert!(
        parse_mini_app_event(&forged.text, &forged.source_url).is_err(),
        "parse_mini_app_event must reject forged-origin object payloads"
    );
    assert!(
        ctrl.handle_web_message(&forged, &palette, now).is_err(),
        "MiniAppController must reject forged-origin object payloads"
    );
    assert!(!ctrl.state().ready);

    // Even when `source_url` itself matches, raw object payloads are rejected outside
    // external-shell mode.
    let raw_obj_same_source = WebMessage {
        text: r#"{"eventType":"web_app_ready","eventData":{}}"#.to_owned(),
        source_url: "https://trusted.bot.example/app".to_owned(),
    };
    assert!(
        ctrl.handle_web_message(&raw_obj_same_source, &palette, now)
            .is_err()
    );
    assert!(!ctrl.state().ready);

    // Legitimate array payload derives origin strictly from WebView2 `source_url`.
    let bad_source_array = WebMessage {
        text: r#"["web_app_ready", {}]"#.to_owned(),
        source_url: "https://evil.attacker.example/phish".to_owned(),
    };
    assert!(
        ctrl.handle_web_message(&bad_source_array, &palette, now)
            .is_err()
    );

    let good_array = WebMessage {
        text: r#"["web_app_ready", {}]"#.to_owned(),
        source_url: "https://trusted.bot.example/app".to_owned(),
    };
    ctrl.handle_web_message(&good_array, &palette, now)
        .expect("matching source_url array payload must succeed");
    assert!(ctrl.state().ready);
}

#[test]
fn b1_external_shell_requires_web_telegram_org_sender_and_valid_token() {
    assert!(is_external_shell_origin("https://web.telegram.org"));
    assert!(is_external_shell_origin("https://web.telegram.org:443"));
    assert!(!is_external_shell_origin("https://web.telegram.org/path"));
    assert!(!is_external_shell_origin("https://evil.web.telegram.org"));
    assert!(!is_external_shell_origin("http://web.telegram.org"));

    let token = "tok_9f8e7d6c5b4a3210";
    let valid_envelope = serde_json::json!({
        "type": EXTERNAL_MESSAGE_TYPE,
        "source": "webapp",
        "token": token,
        "origin": "https://trusted.bot.example",
        "eventType": "web_app_ready",
        "eventData": {}
    })
    .to_string();

    // Valid external-shell sender + valid token succeeds.
    let parsed = parse_native_message_with_shell(
        &valid_envelope,
        "https://web.telegram.org:443/blank.html",
        true,
        Some(token),
    )
    .expect("valid external-shell envelope must parse");
    assert_eq!(parsed.origin, "https://trusted.bot.example:443");
    assert_eq!(parsed.command, "web_app_ready");

    // Wrong sender (not web.telegram.org) is rejected even with valid token.
    assert!(
        parse_native_message_with_shell(
            &valid_envelope,
            "https://evil.attacker.example/blank.html",
            true,
            Some(token),
        )
        .is_err()
    );

    // Wrong token is rejected even from web.telegram.org.
    assert!(
        parse_native_message_with_shell(
            &valid_envelope,
            "https://web.telegram.org:443/blank.html",
            true,
            Some("different_token"),
        )
        .is_err()
    );

    // Controller in external-shell mode enforces same-origin against the envelope's webapp origin
    // and routes location events through `handle_web_message`.
    let palette = Palette::default();
    let now = Instant::now();
    let mut ctrl = MiniAppController::new(MiniAppPanelConfig {
        initial_url: "https://trusted.bot.example/app".to_owned(),
        same_origin: true,
        allow_clipboard_read: false,
        fullscreen: false,
    })
    .with_external_shell_token(token);

    let loc_envelope = serde_json::json!({
        "type": EXTERNAL_MESSAGE_TYPE,
        "source": "webapp",
        "token": token,
        "origin": "https://trusted.bot.example",
        "eventType": "web_app_check_location",
        "eventData": {}
    })
    .to_string();
    let actions = ctrl
        .handle_web_message(
            &WebMessage {
                text: loc_envelope,
                source_url: "https://web.telegram.org/blank.html".to_owned(),
            },
            &palette,
            now,
        )
        .expect("verified external-shell location check must pass");
    assert_eq!(actions, vec![MiniAppAction::CheckLocation]);
}
