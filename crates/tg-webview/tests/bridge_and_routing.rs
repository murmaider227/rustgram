//! Headless unit tests for Mini App bridge protocol parsing/serialization,
//! panel state machine logic, custom-scheme request routing, range requests,
//! restricted-origin CSP, dialog flood protection, and Windows runtime detection.

use std::io::SeekFrom;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tg_theme::{Color, EmbeddedType, Palette, prepare_embedded_palette};
use tg_webview::{
    AccessRequestStatus, BiometryInfo, CLIPBOARD_READ_TIMEOUT, DataRequest, DataResponse,
    DataResult, DataStream, DialogArgs, DialogType, FileDownloadStatus, HapticFeedbackParams,
    HapticNotificationType, HomeScreenStatus, HostEvent, ImpactStyle, InlineChatType,
    MemoryDataStream, MiniAppAction, MiniAppController, MiniAppEvent, MiniAppPanelConfig,
    MiniAppPopupButtonType, NamedColorKey, OPEN_LINK_TIMEOUT, POPUPS_QUICKLY_DELAY,
    PhoneRequestStatus, PopupButtonType, PopupFloodGuard, PopupResult, SafeAreaInsets,
    SchemeRouter, SecondaryButtonPosition, ThemeChangedPayload, ViewportParams, WebMessage,
    availability, dialog_result_from_popup, dialog_to_popup_args, format_dynamic_viewport_script,
    format_post_event_script, generate_storage_token, is_allowed_navigation_scheme,
    is_allowed_restricted_url, make_restricted_content, mime_from_path, mini_app_init_script,
    origin_check_script, origin_from_url, origins_match, parse_hex_color, parse_mini_app_event,
    parse_native_message, parse_range_header, parse_range_header_for, resolve_ripple_color,
    resolve_user_data_dir, scrollbar_theme_scripts,
};

#[test]
fn parse_all_core_mini_app_events_from_tdesktop_array_wire_format() {
    let src = "https://bot.example.com/app";

    // No-argument events (omitted, null, "", "\"\"")
    let (_, ev) = parse_mini_app_event(r#"["web_app_ready"]"#, src).unwrap();
    assert_eq!(ev, MiniAppEvent::WebAppReady);

    let (_, ev) = parse_mini_app_event(r#"["web_app_expand", ""]"#, src).unwrap();
    assert_eq!(ev, MiniAppEvent::WebAppExpand);

    let (_, ev) = parse_mini_app_event(r#"["web_app_request_theme", "\"\""]"#, src).unwrap();
    assert_eq!(ev, MiniAppEvent::WebAppRequestTheme);

    let (_, ev) = parse_mini_app_event(r#"["web_app_request_viewport", null]"#, src).unwrap();
    assert_eq!(ev, MiniAppEvent::WebAppRequestViewport);

    let (_, ev) = parse_mini_app_event(r#"["web_app_request_fullscreen"]"#, src).unwrap();
    assert_eq!(ev, MiniAppEvent::WebAppRequestFullscreen);

    let (_, ev) = parse_mini_app_event(r#"["web_app_exit_fullscreen"]"#, src).unwrap();
    assert_eq!(ev, MiniAppEvent::WebAppExitFullscreen);

    let (_, ev) = parse_mini_app_event(r#"["web_app_request_phone"]"#, src).unwrap();
    assert_eq!(ev, MiniAppEvent::WebAppRequestPhone);

    let (_, ev) = parse_mini_app_event(r#"["web_app_request_write_access"]"#, src).unwrap();
    assert_eq!(ev, MiniAppEvent::WebAppRequestWriteAccess);

    let (_, ev) = parse_mini_app_event(r#"["web_app_biometry_get_info"]"#, src).unwrap();
    assert_eq!(ev, MiniAppEvent::WebAppBiometryGetInfo);

    // JSON-stringified eventData (what official telegram-web-app.js sends)
    let (_, ev) =
        parse_mini_app_event(r#"["web_app_data_send", "{\"data\":\"order_123\"}"]"#, src).unwrap();
    match ev {
        MiniAppEvent::WebAppDataSend(p) => assert_eq!(p.data, "order_123"),
        other => panic!("unexpected event: {other:?}"),
    }

    let (_, ev) = parse_mini_app_event(
        r#"["web_app_open_link", {"url":"https://example.org","try_instant_view":true}]"#,
        src,
    )
    .unwrap();
    match ev {
        MiniAppEvent::WebAppOpenLink(p) => {
            assert_eq!(p.url, "https://example.org");
            assert!(p.try_instant_view);
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let (_, ev) = parse_mini_app_event(
        r#"["web_app_open_tg_link", {"path_full":"/telegram/123"}]"#,
        src,
    )
    .unwrap();
    match ev {
        MiniAppEvent::WebAppOpenTgLink(p) => assert_eq!(p.path_full, "/telegram/123"),
        other => panic!("unexpected event: {other:?}"),
    }

    let (_, ev) =
        parse_mini_app_event(r#"["web_app_open_invoice", {"slug":"inv_abc"}]"#, src).unwrap();
    match ev {
        MiniAppEvent::WebAppOpenInvoice(p) => assert_eq!(p.slug, "inv_abc"),
        other => panic!("unexpected event: {other:?}"),
    }

    // Main & secondary button setup
    let (_, ev) = parse_mini_app_event(
        r##"["web_app_setup_main_button", "{\"is_visible\":true,\"is_active\":true,\"text\":\"Checkout\",\"color\":\"#40a7e3\",\"text_color\":\"#ffffff\",\"is_progress_visible\":false,\"icon_custom_emoji_id\":\"5368324170671202286\"}"]"##,
        src,
    )
    .unwrap();
    match ev {
        MiniAppEvent::WebAppSetupMainButton(p) => {
            assert!(p.is_visible);
            assert!(p.is_active);
            assert_eq!(p.text, "Checkout");
            assert_eq!(p.color.as_deref(), Some("#40a7e3"));
            assert_eq!(
                p.icon_custom_emoji_id.as_deref(),
                Some("5368324170671202286")
            );
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let (_, ev) = parse_mini_app_event(
        r#"["web_app_setup_secondary_button", {"is_visible":true,"is_active":true,"text":"Cancel","position":"bottom"}]"#,
        src,
    )
    .unwrap();
    match ev {
        MiniAppEvent::WebAppSetupSecondaryButton(p) => {
            assert_eq!(p.position, SecondaryButtonPosition::Bottom);
            assert_eq!(p.button.text, "Cancel");
        }
        other => panic!("unexpected event: {other:?}"),
    }

    // Back / Settings / Closing behavior
    let (_, ev) =
        parse_mini_app_event(r#"["web_app_setup_back_button", {"is_visible":true}]"#, src).unwrap();
    assert!(matches!(ev, MiniAppEvent::WebAppSetupBackButton(v) if v.is_visible));

    let (_, ev) = parse_mini_app_event(
        r#"["web_app_setup_settings_button", {"is_visible":true}]"#,
        src,
    )
    .unwrap();
    assert!(matches!(ev, MiniAppEvent::WebAppSetupSettingsButton(v) if v.is_visible));

    let (_, ev) = parse_mini_app_event(
        r#"["web_app_setup_closing_behavior", {"need_confirmation":true}]"#,
        src,
    )
    .unwrap();
    assert!(matches!(ev, MiniAppEvent::WebAppSetupClosingBehavior(c) if c.need_confirmation));

    // Colors
    let (_, ev) = parse_mini_app_event(
        r#"["web_app_set_header_color", {"color_key":"secondary_bg_color"}]"#,
        src,
    )
    .unwrap();
    match ev {
        MiniAppEvent::WebAppSetHeaderColor(c) => {
            assert_eq!(c.color_key, Some(NamedColorKey::SecondaryBgColor));
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let (_, ev) = parse_mini_app_event(
        r##"["web_app_set_background_color", {"color":"#17212b"}]"##,
        src,
    )
    .unwrap();
    match ev {
        MiniAppEvent::WebAppSetBackgroundColor(c) => {
            assert_eq!(c.color.as_deref(), Some("#17212b"));
        }
        other => panic!("unexpected event: {other:?}"),
    }

    // Haptic feedback variants
    let (_, ev) = parse_mini_app_event(
        r#"["web_app_trigger_haptic_feedback", {"type":"impact","impact_style":"heavy"}]"#,
        src,
    )
    .unwrap();
    assert_eq!(
        ev,
        MiniAppEvent::WebAppTriggerHapticFeedback(HapticFeedbackParams::Impact {
            impact_style: ImpactStyle::Heavy
        })
    );

    let (_, ev) = parse_mini_app_event(
        r#"["web_app_trigger_haptic_feedback", {"type":"notification","notification_type":"success"}]"#,
        src,
    )
    .unwrap();
    assert_eq!(
        ev,
        MiniAppEvent::WebAppTriggerHapticFeedback(HapticFeedbackParams::Notification {
            notification_type: HapticNotificationType::Success
        })
    );

    let (_, ev) = parse_mini_app_event(
        r#"["web_app_trigger_haptic_feedback", {"type":"selection_change"}]"#,
        src,
    )
    .unwrap();
    assert_eq!(
        ev,
        MiniAppEvent::WebAppTriggerHapticFeedback(HapticFeedbackParams::SelectionChange)
    );

    // Switch inline query + invalid chat type resets list (matches tdesktop)
    let (_, ev) = parse_mini_app_event(
        r#"["web_app_switch_inline_query", {"query":"cats","chat_types":["users","groups"]}]"#,
        src,
    )
    .unwrap();
    match ev {
        MiniAppEvent::WebAppSwitchInlineQuery(p) => {
            assert_eq!(p.query, "cats");
            assert_eq!(
                p.chat_types,
                vec![InlineChatType::Users, InlineChatType::Groups]
            );
        }
        other => panic!("unexpected event: {other:?}"),
    }
    let (_, ev) = parse_mini_app_event(
        r#"["web_app_switch_inline_query", {"query":"cats","chat_types":["users","invalid"]}]"#,
        src,
    )
    .unwrap();
    match ev {
        MiniAppEvent::WebAppSwitchInlineQuery(p) => assert!(p.chat_types.is_empty()),
        other => panic!("unexpected event: {other:?}"),
    }

    // Popup + clipboard + biometry + QR
    let (_, ev) = parse_mini_app_event(
        r#"["web_app_open_popup", {"title":"Delete","message":"Sure?","buttons":[{"id":"del","type":"destructive","text":"Delete"},{"id":"cancel","type":"cancel"}]}]"#,
        src,
    )
    .unwrap();
    match ev {
        MiniAppEvent::WebAppOpenPopup(p) => {
            assert_eq!(p.title, "Delete");
            assert_eq!(p.buttons.len(), 2);
            assert_eq!(
                p.buttons[0].button_type,
                MiniAppPopupButtonType::Destructive
            );
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let (_, ev) = parse_mini_app_event(
        r#"["web_app_read_text_from_clipboard", {"req_id":"clip-7"}]"#,
        src,
    )
    .unwrap();
    match ev {
        MiniAppEvent::WebAppReadTextFromClipboard(p) => assert_eq!(p.req_id, json!("clip-7")),
        other => panic!("unexpected event: {other:?}"),
    }

    let (_, ev) = parse_mini_app_event(
        r#"["web_app_open_scan_qr_popup", {"text":"Scan code"}]"#,
        src,
    )
    .unwrap();
    match ev {
        MiniAppEvent::WebAppOpenScanQrPopup(p) => assert_eq!(p.text.as_deref(), Some("Scan code")),
        other => panic!("unexpected event: {other:?}"),
    }

    let (_, ev) = parse_mini_app_event(
        r#"["web_app_biometry_update_token", {"token":"secret","reason":"Save"}]"#,
        src,
    )
    .unwrap();
    match ev {
        MiniAppEvent::WebAppBiometryUpdateToken(p) => {
            assert_eq!(p.token.as_deref(), Some("secret"));
            assert_eq!(p.reason.as_deref(), Some("Save"));
        }
        other => panic!("unexpected event: {other:?}"),
    }

    // Forward-compatible unknown event (array wire format; raw objects are rejected outside
    // authenticated external-shell mode).
    let (_, ev) =
        parse_mini_app_event(r#"["web_app_future_feature", {"flag":true}]"#, src).unwrap();
    match ev {
        MiniAppEvent::Unknown {
            event_type,
            event_data,
        } => {
            assert_eq!(event_type, "web_app_future_feature");
            assert_eq!(event_data.get("flag"), Some(&json!(true)));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn rejects_invalid_and_shell_namespace_bridge_messages() {
    let src = "https://bot.example.com";
    assert!(parse_native_message(r#"["shell_close", {}]"#, src).is_err());
    assert!(parse_native_message(r#"["tdesktop_shell_ready", {}]"#, src).is_err());
    assert!(parse_native_message(r#"[""]"#, src).is_err());
    assert!(parse_native_message(r#"12345"#, src).is_err());
    assert!(parse_native_message(r#"["web_app_ready", 42]"#, src).is_err());
    assert!(parse_native_message(r#"["web_app_ready", "[1,2]"]"#, src).is_err());
    assert!(
        parse_mini_app_event(
            r#"["web_app_open_popup", {"message":"","buttons":[]}]"#,
            src
        )
        .is_err()
    );
    assert!(parse_mini_app_event(r#"["web_app_read_text_from_clipboard", {}]"#, src).is_err());
}

#[test]
fn host_event_serialization_and_post_script_formatting() {
    let palette = Palette::default();
    let theme_event = HostEvent::ThemeChanged(ThemeChangedPayload::from_palette(&palette));
    let (name, data) = theme_event.to_wire_parts();
    assert_eq!(name, "theme_changed");
    let theme_obj = data.unwrap();
    assert_eq!(theme_obj["theme_params"]["bg_color"], json!("#ffffff"));
    assert_eq!(theme_obj["theme_params"]["button_color"], json!("#40a7e3"));

    // Roundtrip serde for typed HostEvent
    let serialized = serde_json::to_string(&theme_event).unwrap();
    let decoded: HostEvent = serde_json::from_str(&serialized).unwrap();
    assert_eq!(decoded, theme_event);

    // No-payload events omit the second JS argument
    let main_script = format_post_event_script(&HostEvent::MainButtonPressed, None);
    assert!(main_script.contains(r#"receiveEvent("main_button_pressed")"#));

    let back_script = format_post_event_script(
        &HostEvent::BackButtonPressed,
        Some("https://bot.example.com:443"),
    );
    assert!(back_script.contains(r#"this.location.origin === "https://bot.example.com""#));
    assert!(back_script.contains(r#"receiveEvent("back_button_pressed")"#));

    // popup_closed omits data when dismissed without clicking a button
    let dismissed = format_post_event_script(&HostEvent::PopupClosed { button_id: None }, None);
    assert!(dismissed.contains(r#"receiveEvent("popup_closed")"#));
    let clicked = format_post_event_script(
        &HostEvent::PopupClosed {
            button_id: Some("ok".to_owned()),
        },
        None,
    );
    assert!(clicked.contains(r#"receiveEvent("popup_closed", {"button_id":"ok"})"#));

    // viewport_changed dynamic & explicit
    let dyn_vp = format_dynamic_viewport_script(None);
    assert!(dyn_vp.contains("window.innerHeight"));
    let explicit_vp = HostEvent::ViewportChanged(ViewportParams {
        height: 600.0,
        is_state_stable: true,
        is_expanded: true,
    });
    let (vp_name, vp_data) = explicit_vp.to_wire_parts();
    assert_eq!(vp_name, "viewport_changed");
    assert_eq!(vp_data.unwrap()["height"], json!(600.0));

    // Status enums serialize as snake_case strings expected by telegram-web-app.js
    let (_, wa) = HostEvent::WriteAccessRequested {
        status: AccessRequestStatus::Allowed,
    }
    .to_wire_parts();
    assert_eq!(wa.unwrap()["status"], json!("allowed"));

    let (_, ph) = HostEvent::PhoneRequested {
        status: PhoneRequestStatus::Sent,
    }
    .to_wire_parts();
    assert_eq!(ph.unwrap()["status"], json!("sent"));

    let (_, dl) = HostEvent::FileDownloadRequested {
        status: FileDownloadStatus::Downloading,
    }
    .to_wire_parts();
    assert_eq!(dl.unwrap()["status"], json!("downloading"));

    assert!(mini_app_init_script().contains("TelegramWebviewProxy"));
    assert!(mini_app_init_script().contains("Telegram.WebView"));
}

#[test]
fn mini_app_controller_state_machine_and_security_gates() {
    let day = Palette::default();
    let night = prepare_embedded_palette(EmbeddedType::Night, None).unwrap();
    let t0 = Instant::now();

    let mut ctrl = MiniAppController::new(MiniAppPanelConfig {
        initial_url: "https://app.example.com/mini".to_owned(),
        same_origin: true,
        allow_clipboard_read: true,
        fullscreen: false,
    });

    // Reject message from wrong origin when same_origin is true
    let bad_origin = WebMessage {
        text: r#"["web_app_ready"]"#.to_owned(),
        source_url: "https://evil.example.net/phish".to_owned(),
    };
    assert!(ctrl.handle_web_message(&bad_origin, &day, t0).is_err());

    // Accept matching origin (default port 443 normalized)
    let ok_ready = WebMessage {
        text: r#"["web_app_ready"]"#.to_owned(),
        source_url: "https://app.example.com:443/other/path".to_owned(),
    };
    ctrl.handle_web_message(&ok_ready, &day, t0).unwrap();
    assert!(ctrl.state().ready);

    // Main button with whitespace-only text and no custom emoji stays hidden
    let actions = ctrl
        .handle_web_message(
            &WebMessage {
                text: r#"["web_app_setup_main_button", {"is_visible":true,"is_active":true,"text":"   "}]"#
                    .to_owned(),
                source_url: "https://app.example.com/mini".to_owned(),
            },
            &day,
            t0,
        )
        .unwrap();
    assert!(!ctrl.state().main_button.is_visible);
    assert!(actions.is_empty());

    // Main button with real text becomes visible and emits SendDynamicViewport
    let actions = ctrl
        .handle_web_message(
            &WebMessage {
                text: r#"["web_app_setup_main_button", {"is_visible":true,"is_active":true,"text":"Buy"}]"#
                    .to_owned(),
                source_url: "https://app.example.com/mini".to_owned(),
            },
            &day,
            t0,
        )
        .unwrap();
    assert!(ctrl.state().main_button.is_visible);
    assert_eq!(actions, vec![MiniAppAction::SendDynamicViewport]);
    assert_eq!(
        ctrl.on_main_button_clicked(t0),
        Some(MiniAppAction::PostEvent(HostEvent::MainButtonPressed))
    );

    // Open-link gate: one-shot within OPEN_LINK_TIMEOUT (3s)
    let actions = ctrl
        .handle_web_message(
            &WebMessage {
                text: r#"["web_app_open_link", {"url":"https://telegram.org"}]"#.to_owned(),
                source_url: "https://app.example.com/mini".to_owned(),
            },
            &day,
            t0 + Duration::from_secs(1),
        )
        .unwrap();
    assert!(matches!(
        &actions[0],
        MiniAppAction::OpenExternalLink {
            needs_confirmation: false,
            ..
        }
    ));
    // Second call on the same interaction requires confirmation
    let actions2 = ctrl
        .handle_web_message(
            &WebMessage {
                text: r#"["web_app_open_link", {"url":"https://telegram.org"}]"#.to_owned(),
                source_url: "https://app.example.com/mini".to_owned(),
            },
            &day,
            t0 + Duration::from_secs(2),
        )
        .unwrap();
    assert!(matches!(
        &actions2[0],
        MiniAppAction::OpenExternalLink {
            needs_confirmation: true,
            ..
        }
    ));

    // Clipboard gate: allowed within CLIPBOARD_READ_TIMEOUT (10s), denied after
    let actions_clip_ok = ctrl
        .handle_web_message(
            &WebMessage {
                text: r#"["web_app_read_text_from_clipboard", {"req_id":"c1"}]"#.to_owned(),
                source_url: "https://app.example.com/mini".to_owned(),
            },
            &day,
            t0 + Duration::from_secs(5),
        )
        .unwrap();
    assert!(matches!(
        &actions_clip_ok[0],
        MiniAppAction::ReadClipboard { allowed: true, .. }
    ));

    let actions_clip_expired = ctrl
        .handle_web_message(
            &WebMessage {
                text: r#"["web_app_read_text_from_clipboard", {"req_id":"c2"}]"#.to_owned(),
                source_url: "https://app.example.com/mini".to_owned(),
            },
            &day,
            t0 + CLIPBOARD_READ_TIMEOUT + Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(
        actions_clip_expired,
        vec![MiniAppAction::PostEvent(HostEvent::ClipboardTextReceived {
            req_id: json!("c2"),
            data: None,
        })]
    );

    // Named header color tracks palette switch from Day to Night
    ctrl.handle_web_message(
        &WebMessage {
            text: r#"["web_app_set_header_color", {"color_key":"secondary_bg_color"}]"#.to_owned(),
            source_url: "https://app.example.com/mini".to_owned(),
        },
        &day,
        t0,
    )
    .unwrap();
    assert_eq!(ctrl.state().header_color, Some(day.box_divider_bg));
    let theme_action = ctrl.on_palette_changed(&night);
    assert_eq!(ctrl.state().header_color, Some(night.box_divider_bg));
    assert!(matches!(
        theme_action,
        MiniAppAction::PostEvent(HostEvent::ThemeChanged(_))
    ));

    // Desktop-unsupported features emit exact tdesktop responses
    let check_home = ctrl.handle_event(MiniAppEvent::WebAppCheckHomeScreen, &day, t0);
    assert_eq!(
        check_home,
        vec![MiniAppAction::PostEvent(HostEvent::HomeScreenChecked {
            status: HomeScreenStatus::Unsupported
        })]
    );
    let bio = ctrl.handle_event(MiniAppEvent::WebAppBiometryGetInfo, &day, t0);
    assert_eq!(
        bio,
        vec![MiniAppAction::PostEvent(HostEvent::BiometryInfoReceived(
            BiometryInfo {
                available: false,
                ..BiometryInfo::default()
            }
        ))]
    );

    // Closing confirmation
    ctrl.handle_web_message(
        &WebMessage {
            text: r#"["web_app_setup_closing_behavior", {"need_confirmation":true}]"#.to_owned(),
            source_url: "https://app.example.com/mini".to_owned(),
        },
        &day,
        t0,
    )
    .unwrap();
    assert_eq!(
        ctrl.on_window_close_requested(),
        MiniAppAction::ConfirmClosePanel
    );

    // Safe area & fullscreen
    let fs = ctrl.handle_event(MiniAppEvent::WebAppRequestFullscreen, &day, t0);
    assert!(ctrl.state().fullscreen);
    assert!(
        fs.contains(&MiniAppAction::PostEvent(HostEvent::FullscreenChanged {
            is_fullscreen: true
        }))
    );
    assert!(
        fs.contains(&MiniAppAction::PostEvent(HostEvent::SafeAreaChanged(
            SafeAreaInsets::default()
        )))
    );

    // Verify timeout constants match tdesktop
    assert_eq!(OPEN_LINK_TIMEOUT, Duration::from_secs(3));
    assert_eq!(CLIPBOARD_READ_TIMEOUT, Duration::from_secs(10));
}

#[test]
fn custom_scheme_routing_and_range_requests() {
    let mut router = SchemeRouter::default();
    router.add_html("index.html", "<h1>Hello Rustgram</h1>");
    router.add_static(
        "media/clip.bin",
        "application/octet-stream",
        b"0123456789abcdefghij".to_vec(),
    );
    router.add_prefix_handler("iv/", |rest, req| {
        let body = format!("IV page: {rest}");
        Some(DataResponse::from_bytes_for_request(
            body.as_bytes(),
            "text/plain; charset=utf-8",
            req,
        ))
    });
    router.set_fallback(Arc::new(|req, _done| {
        if req.id == "fallback/ok" {
            DataResult::Done(DataResponse::from_bytes(
                b"fallback-body".to_vec(),
                "text/plain",
            ))
        } else {
            DataResult::Failed
        }
    }));

    assert_eq!(
        router.url_for("index.html"),
        "rustgram-internal://app/index.html"
    );
    assert_eq!(
        router.extract_resource_id("rustgram-internal://app/index.html?v=1#top"),
        Some("index.html".to_owned())
    );
    assert_eq!(
        router.extract_resource_id("http://desktop-app-resource/media/clip.bin"),
        Some("media/clip.bin".to_owned())
    );
    assert_eq!(router.extract_resource_id("https://example.com/a"), None);

    // 200 OK static HTML
    let resp = router
        .route_sync("rustgram-internal://app/index.html", None)
        .unwrap();
    assert_eq!(resp.status_code, 200);
    assert_eq!(resp.reason_phrase, "OK");
    assert!(
        resp.headers
            .contains("Content-Type: text/html; charset=utf-8")
    );
    assert!(resp.headers.contains("Accept-Ranges: bytes"));
    assert!(resp.headers.contains("Access-Control-Allow-Origin: *"));
    let mut stream = resp.stream.unwrap();
    let mut buf = vec![0u8; 64];
    let n = stream.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"<h1>Hello Rustgram</h1>");

    // 206 Partial Content with closed range bytes=4-9 (6 bytes: "456789")
    let partial = router
        .route_sync("rustgram-internal://app/media/clip.bin", Some("bytes=4-9"))
        .unwrap();
    assert_eq!(partial.status_code, 206);
    assert_eq!(partial.reason_phrase, "Partial Content");
    assert!(partial.headers.contains("Content-Length: 6"));
    assert!(partial.headers.contains("Content-Range: bytes 4-9/20"));
    let mut pstream = partial.stream.unwrap();
    let mut pbuf = vec![0u8; 16];
    let pn = pstream.read(&mut pbuf).unwrap();
    assert_eq!(&pbuf[..pn], b"456789");

    // 206 Partial Content with open-ended range bytes=15- (5 bytes: "fghij")
    let open = router
        .route_sync("rustgram-internal://app/media/clip.bin", Some("bytes=15-"))
        .unwrap();
    assert_eq!(open.status_code, 206);
    assert!(open.headers.contains("Content-Length: 5"));
    assert!(open.headers.contains("Content-Range: bytes 15-19/20"));
    let mut ostream = open.stream.unwrap();
    let mut obuf = vec![0u8; 16];
    let on = ostream.read(&mut obuf).unwrap();
    assert_eq!(&obuf[..on], b"fghij");

    // Dynamic prefix handler
    let iv = router
        .route_sync("rustgram-internal://app/iv/article-42", None)
        .unwrap();
    assert_eq!(iv.status_code, 200);
    let mut iv_stream = iv.stream.unwrap();
    let mut iv_buf = vec![0u8; 64];
    let iv_n = iv_stream.read(&mut iv_buf).unwrap();
    assert_eq!(&iv_buf[..iv_n], b"IV page: article-42");

    // Fallback handler
    let fb = router
        .route_sync("rustgram-internal://app/fallback/ok", None)
        .unwrap();
    assert_eq!(fb.status_code, 200);

    // 404 Not Found
    let missing = router
        .route_sync("rustgram-internal://app/does-not-exist", None)
        .unwrap();
    assert_eq!(missing.status_code, 404);
    assert!(missing.stream.is_none());

    // MIME helper
    assert_eq!(mime_from_path("index.html"), "text/html; charset=utf-8");
    assert_eq!(
        mime_from_path("bundle.JS"),
        "application/javascript; charset=utf-8"
    );
    assert_eq!(mime_from_path("video.mp4"), "video/mp4");
}

#[test]
fn range_header_and_memory_stream_edge_cases() {
    assert_eq!(parse_range_header("bytes=0-499"), Some((0, 500)));
    assert_eq!(parse_range_header("bytes=100-199"), Some((100, 100)));
    assert_eq!(parse_range_header("bytes=50-"), Some((50, -1)));
    assert_eq!(parse_range_header("bytes=-500"), Some((0, 501)));
    assert_eq!(parse_range_header("bytes=200-100"), None);
    assert_eq!(parse_range_header("items=0-10"), None);
    assert_eq!(parse_range_header("bytes=abc-def"), None);

    let mut req = DataRequest::new("file.mp4");
    assert!(parse_range_header_for(&mut req, "bytes=10-19"));
    assert_eq!((req.offset, req.limit), (10, 10));
    assert!(!parse_range_header_for(&mut req, "invalid"));

    let mut stream = MemoryDataStream::new(b"hello world".to_vec(), "text/plain");
    assert_eq!(stream.size(), 11);
    assert_eq!(stream.mime(), "text/plain");
    assert_eq!(stream.seek(SeekFrom::Start(6)).unwrap(), 6);
    let mut buf = [0u8; 16];
    let n = stream.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"world");
    assert_eq!(stream.seek(SeekFrom::End(-5)).unwrap(), 6);
    assert_eq!(stream.seek(SeekFrom::Current(-6)).unwrap(), 0);
    assert!(stream.seek(SeekFrom::End(1)).is_err());
    assert!(stream.seek(SeekFrom::Start(12)).is_err());
}

#[test]
fn restricted_origin_csp_navigation_policy_dialogs_and_helpers() {
    // Restricted origin validation
    let restricted = make_restricted_content("https://secure.example.com").unwrap();
    assert!(
        restricted
            .policy
            .contains("connect-src https://secure.example.com wss://secure.example.com")
    );
    assert!(restricted.script.contains("Content-Security-Policy"));
    assert!(restricted.script.contains("RTCPeerConnection"));

    assert!(make_restricted_content("http://insecure.example.com").is_none());
    assert!(make_restricted_content("https://example.com:443").is_none());
    assert!(make_restricted_content("https://example.com/path").is_none());

    assert!(is_allowed_restricted_url(
        "https://secure.example.com/api",
        "https://secure.example.com"
    ));
    assert!(is_allowed_restricted_url(
        "wss://secure.example.com/ws",
        "https://secure.example.com"
    ));
    assert!(!is_allowed_restricted_url(
        "https://other.example.com/api",
        "https://secure.example.com"
    ));

    // Navigation scheme filtering
    assert!(is_allowed_navigation_scheme(
        "https://telegram.org",
        Some("rustgram-internal")
    ));
    assert!(is_allowed_navigation_scheme(
        "tonsite://foundation.ton/",
        Some("rustgram-internal")
    ));
    assert!(is_allowed_navigation_scheme(
        "rustgram-internal://app/index.html",
        Some("rustgram-internal")
    ));
    assert!(!is_allowed_navigation_scheme(
        "javascript:alert(1)",
        Some("rustgram-internal")
    ));
    assert!(!is_allowed_navigation_scheme(
        "file:///C:/Windows/win.ini",
        Some("rustgram-internal")
    ));

    // Origins & colors
    assert_eq!(
        origin_from_url("https://Example.COM/foo"),
        "https://example.com:443"
    );
    assert!(origins_match(
        "https://Example.COM/a",
        "https://example.com:443/b"
    ));
    assert_eq!(
        origin_check_script("https://bot.example.com:443"),
        r#"this.location.origin === "https://bot.example.com""#
    );
    assert_eq!(
        parse_hex_color("#40a7e3"),
        Some(Color::rgb(0x40, 0xa7, 0xe3))
    );
    assert_eq!(parse_hex_color("#40a7e"), None);
    assert_eq!(parse_hex_color("40a7e3"), None);
    let ripple = resolve_ripple_color(Color::rgb(0xff, 0xff, 0xff));
    assert!(ripple.r < 0xff);

    // Dialog conversion & flood guard
    let prompt_popup = dialog_to_popup_args(&DialogArgs {
        dialog_type: DialogType::Prompt,
        value: "default".to_owned(),
        text: "Enter name".to_owned(),
        url: "https://bot.example.com/app".to_owned(),
    });
    assert_eq!(prompt_popup.title, "bot.example.com");
    assert_eq!(prompt_popup.value.as_deref(), Some("default"));
    assert_eq!(prompt_popup.buttons.len(), 2);
    assert_eq!(prompt_popup.buttons[0].button_type, PopupButtonType::Ok);
    assert_eq!(prompt_popup.buttons[1].button_type, PopupButtonType::Cancel);

    let accepted = dialog_result_from_popup(PopupResult {
        id: Some("ok".to_owned()),
        value: Some("Alice".to_owned()),
    });
    assert!(accepted.accepted);
    assert_eq!(accepted.text, "Alice");

    let cancelled = dialog_result_from_popup(PopupResult {
        id: Some("cancel".to_owned()),
        value: Some("ignored".to_owned()),
    });
    assert!(!cancelled.accepted);

    let mut guard = PopupFloodGuard::new();
    let now = Instant::now();
    for i in 0..3 {
        let t = now + Duration::from_millis(i * 100);
        assert!(guard.try_begin(t, false));
        assert!(!guard.try_begin(t, false)); // nested rejected
        guard.finish(t);
    }
    // 4th rapid popup is throttled unless ignore_flood_check is true
    assert!(!guard.try_begin(now + Duration::from_secs(1), false));
    assert!(guard.try_begin(now + Duration::from_secs(1), true));
    guard.finish(now + Duration::from_secs(1));
    // Allowed again after POPUPS_QUICKLY_DELAY
    assert!(guard.try_begin(now + Duration::from_secs(1) + POPUPS_QUICKLY_DELAY, false));

    // Storage dir & scrollbar CSS helpers
    let dir = resolve_user_data_dir(Path::new(r"C:\rustgram-data"), "account/1:main");
    assert_eq!(dir, Path::new(r"C:\rustgram-data\webview\account_1_main"));
    assert_eq!(generate_storage_token().len(), 16);

    let (init_css, eval_css) = scrollbar_theme_scripts(
        Color::rgba(0, 0, 0, 20),
        Color::rgba(0, 0, 0, 40),
        Color::rgba(0, 0, 0, 80),
        Color::rgba(0, 0, 0, 120),
    );
    assert!(init_css.contains("DOMContentLoaded"));
    assert!(eval_css.contains("::-webkit-scrollbar-thumb"));

    // Runtime availability check works headless on Windows
    let avail = availability();
    if cfg!(windows) {
        // On a Windows host with WebView2 Runtime installed, capabilities are reported.
        if avail.is_supported() {
            assert!(avail.custom_scheme_requests);
            assert!(avail.custom_range_requests);
            assert!(avail.browser_version.is_some());
        } else {
            assert!(avail.user_error_text().is_some());
        }
    }
}

#[test]
fn device_and_secure_storage_events_and_validation() {
    let day = Palette::default();
    let now = Instant::now();
    let mut ctrl = MiniAppController::new(MiniAppPanelConfig::default());

    // Save valid key
    let actions = ctrl
        .handle_web_message(
            &WebMessage {
                text: r#"["web_app_device_storage_save_key", {"req_id":1,"key":"k","value":"v"}]"#
                    .to_owned(),
                source_url: "https://bot.example.com".to_owned(),
            },
            &day,
            now,
        )
        .unwrap();
    assert_eq!(
        actions,
        vec![MiniAppAction::DeviceStorageSave {
            req_id: json!(1),
            key: "k".to_owned(),
            value: Some("v".to_owned()),
        }]
    );

    // Invalid key type -> KEY_INVALID
    let actions = ctrl
        .handle_web_message(
            &WebMessage {
                text: r#"["web_app_device_storage_save_key", {"req_id":2,"key":123,"value":"v"}]"#
                    .to_owned(),
                source_url: "https://bot.example.com".to_owned(),
            },
            &day,
            now,
        )
        .unwrap();
    assert_eq!(
        actions,
        vec![MiniAppAction::PostEvent(HostEvent::DeviceStorageFailed {
            req_id: json!(2),
            error: "KEY_INVALID".to_owned(),
        })]
    );

    // Invalid value type -> VALUE_INVALID
    let actions = ctrl
        .handle_web_message(
            &WebMessage {
                text: r#"["web_app_device_storage_save_key", {"req_id":3,"key":"k","value":99}]"#
                    .to_owned(),
                source_url: "https://bot.example.com".to_owned(),
            },
            &day,
            now,
        )
        .unwrap();
    assert_eq!(
        actions,
        vec![MiniAppAction::PostEvent(HostEvent::DeviceStorageFailed {
            req_id: json!(3),
            error: "VALUE_INVALID".to_owned(),
        })]
    );

    // Secure storage always replies UNSUPPORTED on desktop
    let actions = ctrl
        .handle_web_message(
            &WebMessage {
                text: r#"["web_app_secure_storage_get_key", {"req_id":"s1","key":"tok"}]"#
                    .to_owned(),
                source_url: "https://bot.example.com".to_owned(),
            },
            &day,
            now,
        )
        .unwrap();
    assert_eq!(
        actions,
        vec![MiniAppAction::PostEvent(HostEvent::SecureStorageFailed {
            req_id: json!("s1"),
            error: "UNSUPPORTED".to_owned(),
        })]
    );

    // Emoji status validation
    let actions = ctrl
        .handle_web_message(
            &WebMessage {
                text: r#"["web_app_set_emoji_status", {"custom_emoji_id":"0"}]"#.to_owned(),
                source_url: "https://bot.example.com".to_owned(),
            },
            &day,
            now,
        )
        .unwrap();
    assert_eq!(
        actions,
        vec![MiniAppAction::PostEvent(HostEvent::EmojiStatusFailed {
            error: "SUGGESTED_EMOJI_INVALID".to_owned(),
        })]
    );

    // Age verification flooring
    let actions = ctrl
        .handle_web_message(
            &WebMessage {
                text: r#"["web_app_verify_age", {"passed":true,"age":21.9}]"#.to_owned(),
                source_url: "https://bot.example.com".to_owned(),
            },
            &day,
            now,
        )
        .unwrap();
    assert_eq!(actions, vec![MiniAppAction::VerifyAge(21)]);

    // Share to story strips non-HTTPS widget links
    let (_, ev) = parse_mini_app_event(
        r#"["web_app_share_to_story", {"media_url":"https://img/a.png","text":"hi","widget_link":{"url":"javascript:x","name":"bad"}}]"#,
        "https://bot.example.com",
    )
    .unwrap();
    match ev {
        MiniAppEvent::WebAppShareToStory(p) => {
            assert_eq!(p.widget_link.as_ref().unwrap().url, "");
            assert_eq!(p.widget_link.as_ref().unwrap().name, "");
        }
        other => panic!("unexpected event: {other:?}"),
    }

    // Verify Value import used cleanly
    let _: Value = json!({});
}

#[test]
fn mini_app_controller_permission_reply_helpers_match_tdesktop_events() {
    assert_eq!(
        MiniAppController::emoji_status_access_reply(true),
        HostEvent::EmojiStatusAccessRequested {
            status: AccessRequestStatus::Allowed,
        }
    );
    assert_eq!(
        MiniAppController::emoji_status_access_reply(false),
        HostEvent::EmojiStatusAccessRequested {
            status: AccessRequestStatus::Cancelled,
        }
    );
    assert_eq!(
        MiniAppController::file_download_reply(true),
        HostEvent::FileDownloadRequested {
            status: FileDownloadStatus::Downloading,
        }
    );
    assert_eq!(
        MiniAppController::file_download_reply(false),
        HostEvent::FileDownloadRequested {
            status: FileDownloadStatus::Cancelled,
        }
    );
    assert_eq!(
        MiniAppController::requested_chat_reply("r1", None),
        HostEvent::RequestedChatSent {
            req_id: "r1".to_owned(),
        }
    );
    assert_eq!(
        MiniAppController::requested_chat_reply("r1", Some("USER_DECLINED".to_owned())),
        HostEvent::RequestedChatFailed {
            req_id: "r1".to_owned(),
            error: "USER_DECLINED".to_owned(),
        }
    );
    assert_eq!(
        MiniAppController::prepared_message_reply(None),
        HostEvent::PreparedMessageSent
    );
    assert_eq!(
        MiniAppController::prepared_message_reply(Some("MESSAGE_EXPIRED".to_owned())),
        HostEvent::PreparedMessageFailed {
            error: "MESSAGE_EXPIRED".to_owned(),
        }
    );
    assert_eq!(
        MiniAppController::phone_reply(true),
        HostEvent::PhoneRequested {
            status: PhoneRequestStatus::Sent,
        }
    );
}
