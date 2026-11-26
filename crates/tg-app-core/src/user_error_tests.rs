use super::*;

fn rpc(code: i32, name: &str) -> String {
    // The text a live backend hands the UI: `map_err(|e| e.to_string())` on an `ApiError`.
    tg_api::ApiError::Invocation(tg_api::RpcError::new(code, name).into()).to_string()
}

fn assert_clean(shown: &UserFacingError) {
    if let Some(text) = shown.text() {
        for raw in ["RpcError", "RPC error", "invocation error", "_"] {
            assert!(!text.contains(raw), "raw error text {raw:?} in {text:?}");
        }
    }
}

#[test]
fn import_chat_invite_maps_like_submit_chat_invite() {
    assert_eq!(
        import_chat_invite(&rpc(400, "USERS_TOO_MUCH"), true),
        UserFacingError::Toast(tr::lng_group_invite_no_room())
    );
    assert_eq!(
        import_chat_invite(&rpc(400, "INVITE_HASH_EXPIRED"), true),
        UserFacingError::Toast(tr::lng_group_invite_bad_link())
    );
    assert_eq!(
        import_chat_invite(&rpc(400, "INVITE_REQUEST_SENT"), false),
        UserFacingError::Toast(tr::lng_group_request_sent_channel())
    );
    let limit = import_chat_invite(&rpc(400, "CHANNELS_TOO_MUCH"), true);
    assert_eq!(limit, UserFacingError::LimitBox(LimitBoxKind::Channels));
    assert!(!limit.clone().into_text().unwrap_or_default().contains("**"));
    for shown in [
        import_chat_invite(&rpc(400, "USERS_TOO_MUCH"), true),
        import_chat_invite("transport error: reset", true),
        limit,
    ] {
        assert_clean(&shown);
    }
}

#[test]
fn check_chat_invite_flood_and_bad_link_boxes() {
    assert_eq!(
        check_chat_invite(&rpc(420, "FLOOD_WAIT_30")),
        UserFacingError::Inform {
            title: None,
            text: tr::lng_flood_error(),
        }
    );
    assert_eq!(
        check_chat_invite(&rpc(400, "INVITE_HASH_INVALID")),
        UserFacingError::Inform {
            title: None,
            text: tr::lng_group_invite_bad_link(),
        }
    );
    assert_eq!(
        check_chat_invite(&rpc(500, "INTERNAL")),
        UserFacingError::Silent
    );
}

#[test]
fn error_type_toast_shows_bare_name_or_nothing() {
    assert_eq!(
        error_type_toast("t", &rpc(400, "STICKERSET_INVALID")),
        UserFacingError::Toast("STICKERSET_INVALID".to_owned())
    );
    assert_eq!(
        error_type_toast("t", "Sticker set preview is unavailable."),
        UserFacingError::Silent
    );
}

#[test]
fn toast_sink_reduces_unmapped_rpc_text_to_error_type() {
    let raw = format!(
        "Could not load Telegram Business: {}",
        rpc(400, "PREMIUM_ACCOUNT_REQUIRED")
    );
    assert_eq!(
        visible_toast_text(raw),
        Some("PREMIUM_ACCOUNT_REQUIRED".to_owned())
    );
    let old_debug =
        "Could not join chat: RPC error RpcError { code: 400, name: \"X_Y\", value: None }";
    assert_eq!(
        visible_toast_text(old_debug.to_owned()),
        Some("X_Y".to_owned())
    );
    assert_eq!(visible_toast_text(rpc(406, "CHANNEL_PRIVATE")), None);
    assert_eq!(
        visible_toast_text("Joined chat".to_owned()),
        Some("Joined chat".to_owned())
    );
}

#[test]
fn window_and_deep_link_sites_follow_tdesktop() {
    assert_eq!(
        join_chatlist(&rpc(400, "INVITE_SLUG_EXPIRED")),
        UserFacingError::Toast(tr::lng_group_invite_bad_link())
    );
    assert_eq!(
        join_chatlist(&rpc(420, "FLOOD_WAIT_10")),
        UserFacingError::Toast(tr::lng_flood_error())
    );
    assert_eq!(
        attached_stickers(&rpc(400, "MEDIA_EMPTY")),
        UserFacingError::Inform {
            title: None,
            text: tr::lng_stickers_not_found(),
        }
    );
    assert_eq!(
        forward_media(&rpc(400, "CHAT_WRITE_FORBIDDEN"), "Chat"),
        UserFacingError::Silent
    );
    assert_eq!(
        switch_language(&rpc(400, "LANG_CODE_NOT_SUPPORTED")),
        UserFacingError::Inform {
            title: None,
            text: tr::lng_language_not_found(),
        }
    );
    assert_eq!(
        apply_gift_code(&rpc(400, "GIFT_SLUG_EXPIRED")),
        UserFacingError::Toast("GIFT_SLUG_EXPIRED".to_owned())
    );
    assert_eq!(
        check_gift_code(&rpc(400, "GIFT_SLUG_INVALID")),
        UserFacingError::Toast(tr::lng_gift_link_expired())
    );
    assert_eq!(
        resolve_cloud_theme(&rpc(400, "THEME_INVALID")),
        UserFacingError::Silent
    );
    assert_eq!(
        resolve_business_chat_link(&rpc(400, "LINK_INVALID")),
        UserFacingError::Toast(tr::lng_confirm_phone_link_invalid())
    );
    assert_eq!(
        import_no_paid_token(&rpc(400, "NO_PAID_MESSAGES_TOKEN_INVALID")),
        UserFacingError::Toast(tr::lng_no_paid_token_invalid())
    );
    assert_eq!(
        save_ai_tone(&rpc(406, "TONE_SOMETHING"), false, false),
        UserFacingError::Silent
    );
    assert_eq!(
        save_ai_tone(&rpc(400, "TONES_SAVED_TOO_MANY"), false, true),
        UserFacingError::Toast(tr::lng_ai_compose_tone_saved_limit_final(20).replace("**", ""))
    );
    assert_eq!(
        send_message(&rpc(400, "SCHEDULE_TOO_MUCH"), SendTarget::User),
        UserFacingError::Toast(tr::lng_error_schedule_limit())
    );
    assert_eq!(
        send_message(&rpc(420, "SLOWMODE_WAIT_5"), SendTarget::Group),
        UserFacingError::Silent
    );
    assert_eq!(
        sticker_set_owner_edit(&rpc(400, "STICKERSET_INVALID")),
        UserFacingError::Toast("STICKERSET_INVALID".to_owned())
    );
}

#[test]
fn screen_sites_follow_tdesktop() {
    assert_eq!(
        bot_callback(&rpc(400, "DATA_INVALID")),
        UserFacingError::Silent
    );
    assert_eq!(
        transcribe_audio(&rpc(400, "MSG_VOICE_TOO_LONG")),
        UserFacingError::Silent
    );
    assert_eq!(
        save_cloud_theme(&rpc(400, "THEME_SLUG_OCCUPIED")),
        UserFacingError::Toast(tr::lng_create_channel_link_occupied())
    );
    assert_eq!(
        share_theme_or_qr("t", &rpc(420, "FLOOD_WAIT_3")),
        UserFacingError::Toast(tr::lng_flood_error())
    );
    assert_eq!(
        confirm_phone(&rpc(400, "PHONE_CODE_INVALID")),
        UserFacingError::Toast(tr::lng_bad_code())
    );
    assert_eq!(
        send_payment_form(&rpc(400, "PAYMENT_FAILED")),
        UserFacingError::Toast(tr::lng_payments_payment_failed())
    );
    assert_eq!(
        join_group_call(&rpc(400, "GROUPCALL_FORBIDDEN")),
        UserFacingError::Toast(tr::lng_confcall_not_accessible())
    );
    assert_eq!(
        call_error("USER_PRIVACY_RESTRICTED", "Mira"),
        UserFacingError::Inform {
            title: None,
            text: tr::lng_call_error_not_available("Mira"),
        }
    );
    assert_eq!(call_error("ERROR_UNKNOWN", "Mira"), UserFacingError::Silent);
    assert_eq!(
        business_chatbot(&rpc(400, "BOT_BUSINESS_MISSING")),
        UserFacingError::Toast(tr::lng_chatbots_not_supported())
    );
    assert_eq!(
        business_silent("t", &rpc(400, "LOCATION_INVALID")),
        UserFacingError::Silent
    );
    assert_eq!(
        business_silent("t", "Intro title is too long."),
        UserFacingError::Toast("Intro title is too long.".to_owned())
    );
}
