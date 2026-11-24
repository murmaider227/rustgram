//! Unit and known-answer tests for callback queries, URL auth, switch-inline,
//! bot commands/menu/info, attach menu bots, Mini App theme/launch params, games,
//! and deep-link / start-parameter parsing.

use serde_json::Value;

use tg_bots::callback::{
    AcceptUrlAuthDecision, BotButtonAction, ButtonActivationContext, CallbackAnswerAction,
    CallbackAnswerCache, CallbackAnswerInput, CallbackCacheKey, SwitchInlineDecision,
    UrlAuthAcceptOutcome, UrlAuthDecision, UrlAuthErrorAction, UrlAuthRequestData,
    UrlAuthResultInput, UrlAuthSource, UrlAuthUserChoice, activate_button, build_accept_url_auth,
    handle_callback_answer, handle_url_auth_accept_result, handle_url_auth_request_error,
    handle_url_auth_request_result,
};
use tg_bots::commands::{
    BotAppColors, BotCommand, BotCommandScope, BotCommandsEntry, BotInfo, BotInfoUpdate,
    BotMenuButton, BotStatus, ChatBotCommands, CommandResolutionContext, PrivacyPolicyAction,
    ScopedBotCommands, resolve_privacy_policy_action, wrap_command_in_chat,
};
use tg_bots::deep_links::{
    BotDeepLink, StartParamError, is_mini_app_url, parse_bot_deep_link,
    parse_requested_admin_rights, try_convert_url_to_local, validate_start_param,
    validate_startapp_param,
};
use tg_bots::markup::{ButtonKind, MarkupButton, PeerTypes};
use tg_bots::mini_app::{
    AttachMenuBot, AttachMenuStore, MINI_APP_PLATFORM, MiniAppLaunchParams, ShareGameScoreAction,
    WebViewMenuButtons, WebViewPalette, WebViewSource, allow_webview_clipboard_read,
    build_game_share_link, build_theme_params, can_share_game_to_peer,
    compute_webview_menu_buttons, handle_share_game_score, needs_webview_open_confirmation,
    validate_webview_external_link,
};
use tg_bots::stand_in::{ChatAdminRights, FullMsgId, MsgId, PeerId, PeerKind, Rgba, UserId};

#[test]
fn deep_links_start_startgroup_startapp_game_known_answers() {
    // 1. ?start= with referral prefix extraction.
    assert_eq!(
        parse_bot_deep_link("https://t.me/my_bot?start=ref_abc123", &["ref_"]),
        Some(BotDeepLink::StartBot {
            username: "my_bot".into(),
            start_param: "ref_abc123".into(),
            referral: Some("abc123".into()),
        })
    );

    // 2. ?startgroup= with requested admin rights (space/+ separated).
    assert_eq!(
        parse_bot_deep_link(
            "t.me/modbot?startgroup=welcome_payload&admin=post_messages+delete_messages+pin_messages",
            &[]
        ),
        Some(BotDeepLink::StartGroup {
            username: "modbot".into(),
            start_param: Some("welcome_payload".into()),
            admin_rights: ChatAdminRights::POST_MESSAGES
                | ChatAdminRights::DELETE_MESSAGES
                | ChatAdminRights::PIN_MESSAGES,
        })
    );

    // Bare ?startgroup with no value.
    assert_eq!(
        parse_bot_deep_link("https://telegram.me/modbot?startgroup", &[]),
        Some(BotDeepLink::StartGroup {
            username: "modbot".into(),
            start_param: None,
            admin_rights: ChatAdminRights::empty(),
        })
    );

    // 3. ?startchannel&admin=...
    assert_eq!(
        parse_bot_deep_link(
            "https://telegram.dog/postbot?startchannel&admin=post_messages+edit_messages",
            &[]
        ),
        Some(BotDeepLink::StartChannel {
            username: "postbot".into(),
            admin_rights: ChatAdminRights::POST_MESSAGES | ChatAdminRights::EDIT_MESSAGES,
        })
    );

    // 4. ?startapp= (main Mini App) with compact / fullscreen modes.
    assert_eq!(
        parse_bot_deep_link("https://t.me/shopbot?startapp=item_42&mode=compact", &[]),
        Some(BotDeepLink::StartMainApp {
            username: "shopbot".into(),
            start_param: Some("item_42".into()),
            compact: true,
            fullscreen: false,
        })
    );
    assert!(is_mini_app_url("https://t.me/shopbot?startapp=item_42"));

    // 5. Direct named Mini App: t.me/bot/appname?startapp=...&mode=fullscreen
    assert_eq!(
        parse_bot_deep_link(
            "https://t.me/shopbot/catalog?startapp=shoes-99&mode=fullscreen",
            &[]
        ),
        Some(BotDeepLink::DirectMiniApp {
            username: "shopbot".into(),
            app_name: "catalog".into(),
            start_param: Some("shoes-99".into()),
            fullscreen: true,
        })
    );
    assert!(is_mini_app_url("https://t.me/shopbot/catalog"));

    // 6. ?game= short name
    assert_eq!(
        parse_bot_deep_link("https://t.me/gamee?game=Corsairs", &[]),
        Some(BotDeepLink::ShareGame {
            username: "gamee".into(),
            game_short_name: "Corsairs".into(),
        })
    );
    assert!(!is_mini_app_url("https://t.me/gamee?game=Corsairs"));
    assert_eq!(
        build_game_share_link("gamee", "Corsairs"),
        "https://t.me/gamee?game=Corsairs"
    );

    // 7. ?startattach with choose=
    assert_eq!(
        parse_bot_deep_link(
            "https://t.me/durgerkingbot?startattach=menu1&choose=users%20groups",
            &[]
        ),
        Some(BotDeepLink::StartAttach {
            username: "durgerkingbot".into(),
            start_param: Some("menu1".into()),
            choose_types: PeerTypes::USER | PeerTypes::GROUP,
            attach_username: None,
            fullscreen: false,
        })
    );

    // 8. Subdomain syntax `<bot>.t.me?start=...` and `tg://resolve?domain=...`
    assert_eq!(
        try_convert_url_to_local("https://mybot.t.me?start=hello"),
        "tg://resolve?domain=mybot&start=hello"
    );
    assert_eq!(
        parse_bot_deep_link("tg://resolve?domain=mybot&start=hello", &[]),
        Some(BotDeepLink::StartBot {
            username: "mybot".into(),
            start_param: "hello".into(),
            referral: None,
        })
    );

    // 9. `t.me/newbot/manager/suggested?name=My%20Bot` and `t.me/oauth?startapp=tok`
    assert_eq!(
        parse_bot_deep_link("https://t.me/newbot/parentbot/child_bot?name=My%20Bot", &[]),
        Some(BotDeepLink::NewBot {
            manager: "parentbot".into(),
            username: Some("child_bot".into()),
            name: Some("My Bot".into()),
        })
    );
    assert_eq!(
        parse_bot_deep_link("https://t.me/oauth?startapp=secret_tok", &[]),
        Some(BotDeepLink::OAuthLogin {
            token: "secret_tok".into(),
        })
    );

    // 10. Start parameter validation rules.
    assert!(validate_start_param("AbC_123-xyz").is_ok());
    assert_eq!(validate_start_param(""), Err(StartParamError::Empty));
    assert_eq!(
        validate_start_param(&"a".repeat(65)),
        Err(StartParamError::TooLong { len: 65, max: 64 })
    );
    assert!(validate_startapp_param(&"a".repeat(512)).is_ok());
    assert_eq!(
        validate_start_param("bad space"),
        Err(StartParamError::InvalidChar { ch: ' ', offset: 3 })
    );
    assert_eq!(
        parse_requested_admin_rights("manage_chat anonymous manage_topics"),
        ChatAdminRights::OTHER | ChatAdminRights::ANONYMOUS | ChatAdminRights::MANAGE_TOPICS
    );
}

#[test]
fn callback_query_answer_url_confirmation_and_cache() {
    let ctx_unverified = ButtonActivationContext {
        item_id: FullMsgId::new(PeerId(10), MsgId(50)),
        topic_root_id: None,
        bot_id: Some(UserId(900)),
        bot_verified: false,
        bot_inline_return_to: None,
        is_regular: true,
        ephemeral_id: None,
        request_in_flight: false,
    };

    // URL button requires confirmation for unverified bots, skips for verified bots.
    let url_btn = MarkupButton::new(
        "Visit",
        ButtonKind::Url {
            url: "https://example.org".into(),
        },
    );
    assert_eq!(
        activate_button(&url_btn, &ctx_unverified),
        BotButtonAction::OpenUrl {
            url: "https://example.org".into(),
            require_confirmation: true,
        }
    );
    let mut ctx_verified = ctx_unverified.clone();
    ctx_verified.bot_verified = true;
    assert_eq!(
        activate_button(&url_btn, &ctx_verified),
        BotButtonAction::OpenUrl {
            url: "https://example.org".into(),
            require_confirmation: false,
        }
    );

    // SwitchInline: SamePeer, ReturnToSavedPeer, ChooseRecipient.
    let sw_same = MarkupButton::new("Here", ButtonKind::SwitchInlineSame { query: "q1".into() });
    assert_eq!(
        activate_button(&sw_same, &ctx_unverified),
        BotButtonAction::SwitchInline(SwitchInlineDecision::SwitchSamePeer {
            bot_id: UserId(900),
            query: "q1".into(),
            reply_to: MsgId(50),
        })
    );
    let sw_choose = MarkupButton::new(
        "Pick",
        ButtonKind::SwitchInline {
            query: "q2".into(),
            peer_types: PeerTypes::USER | PeerTypes::GROUP,
        },
    );
    assert_eq!(
        activate_button(&sw_choose, &ctx_unverified),
        BotButtonAction::SwitchInline(SwitchInlineDecision::ChooseRecipient {
            bot_id: UserId(900),
            query: "q2".into(),
            peer_types: PeerTypes::USER | PeerTypes::GROUP,
        })
    );
    let mut ctx_return = ctx_unverified.clone();
    ctx_return.bot_inline_return_to = Some(PeerId(777));
    assert_eq!(
        activate_button(&sw_choose, &ctx_return),
        BotButtonAction::SwitchInline(SwitchInlineDecision::ReturnToSavedPeer {
            bot_id: UserId(900),
            query: "q2".into(),
            return_peer: PeerId(777),
        })
    );

    // Answer handling: alert vs toast vs url vs game url vs password layer.
    assert_eq!(
        handle_callback_answer(
            false,
            false,
            false,
            false,
            &CallbackAnswerInput {
                alert: true,
                message: Some("Watch out!".into()),
                url: Some("https://ignored.example".into()),
                ..Default::default()
            }
        ),
        CallbackAnswerAction::ShowAlert {
            message: "Watch out!".into(),
        }
    );
    assert_eq!(
        handle_callback_answer(
            false,
            true,
            false,
            false,
            &CallbackAnswerInput {
                alert: false,
                message: Some("Confirmed".into()),
                ..Default::default()
            }
        ),
        CallbackAnswerAction::ShowToast {
            message: "Confirmed".into(),
            hide_password_layer: true,
        }
    );
    assert_eq!(
        handle_callback_answer(
            true,
            false,
            false,
            false,
            &CallbackAnswerInput {
                url: Some("https://games.example/play".into()),
                ..Default::default()
            }
        ),
        CallbackAnswerAction::OpenGameUrl {
            url: "https://games.example/play".into(),
            is_local_link: false,
            require_confirmation: true,
            send_play_game_progress: true,
        }
    );

    // CallbackAnswerCache honors cache_time.
    let mut cache = CallbackAnswerCache::new();
    let key = CallbackCacheKey {
        item_id: ctx_unverified.item_id,
        is_game: false,
        data: b"btn1".to_vec(),
    };
    cache.insert(
        key.clone(),
        CallbackAnswerInput {
            message: Some("Cached".into()),
            cache_time: 30,
            ..Default::default()
        },
        1000,
    );
    assert!(cache.get(&key, 1029).is_some());
    assert!(cache.get(&key, 1030).is_none());
}

#[test]
fn url_auth_request_and_accept_flows() {
    let source = UrlAuthSource::Button {
        peer_id: PeerId(1),
        msg_id: MsgId(2),
        button_id: 9,
        url: "https://site.example/login".into(),
    };

    let decision = handle_url_auth_request_result(
        source.clone(),
        UrlAuthResultInput::Request(UrlAuthRequestData {
            domain: "site.example".into(),
            bot_id: UserId(5),
            bot_name: "AuthBot".into(),
            request_write_access: true,
            request_phone_number: false,
            match_codes_first: false,
            is_app: false,
            verified_app_name: None,
            user_id_hint: None,
            match_codes: vec!["🚀".into()],
            browser: None,
            platform: None,
            ip: None,
            region: None,
        }),
    );
    match decision {
        UrlAuthDecision::ShowConfirmation(box_data) => {
            assert_eq!(box_data.display_name, "site.example");
            assert_eq!(box_data.write_access_bot_name.as_deref(), Some("AuthBot"));
            assert_eq!(box_data.browser, "Unknown browser");
            assert_eq!(box_data.platform, "Unknown platform");
        }
        other => panic!("expected ShowConfirmation, got {other:?}"),
    }

    // Decline on button opens fallback unauthenticated URL.
    let decline = build_accept_url_auth(
        source.clone(),
        UrlAuthUserChoice {
            auth: false,
            ..Default::default()
        },
    );
    assert_eq!(
        decline,
        AcceptUrlAuthDecision::Decline {
            url: "https://site.example/login".into(),
            open_fallback_url: true,
        }
    );

    // Accept returns OpenUrl with keep_web_auth_tokens = true.
    let outcome = handle_url_auth_accept_result(
        &source,
        UrlAuthResultInput::Accepted {
            url: Some("https://site.example/login?tgAuthResult=abc".into()),
        },
    );
    assert_eq!(
        outcome,
        UrlAuthAcceptOutcome::OpenUrl {
            url: "https://site.example/login?tgAuthResult=abc".into(),
            keep_web_auth_tokens: true,
        }
    );

    // URL_EXPIRED on standalone URL auth shows expired toast.
    let url_source = UrlAuthSource::Url {
        url: "tg://resolve?domain=oauth&startapp=tok".into(),
    };
    assert_eq!(
        handle_url_auth_request_error(&url_source, "URL_EXPIRED"),
        UrlAuthErrorAction::ShowExpiredToast
    );
}

#[test]
fn bot_commands_scopes_wrap_menu_and_privacy_policy() {
    let mut scoped = ScopedBotCommands::new();
    scoped.set(
        BotCommandScope::Default,
        "",
        vec![BotCommand::new("start", "Default start")],
    );
    scoped.set(
        BotCommandScope::Chats,
        "",
        vec![BotCommand::new("rules", "Group rules")],
    );
    scoped.set(
        BotCommandScope::ChatAdmins,
        "en",
        vec![BotCommand::new("ban", "Ban user")],
    );
    scoped.set(
        BotCommandScope::PeerUser {
            peer: PeerId(500),
            user: UserId(42),
        },
        "",
        vec![BotCommand::new("secret", "Owner command")],
    );

    let member_ctx = CommandResolutionContext {
        peer_id: PeerId(500),
        is_private_chat: false,
        is_admin: false,
        self_user_id: UserId(99),
    };
    assert_eq!(scoped.resolve(&member_ctx, "en")[0].command, "rules");

    let admin_ctx = CommandResolutionContext {
        is_admin: true,
        ..member_ctx
    };
    assert_eq!(scoped.resolve(&admin_ctx, "en")[0].command, "ban");

    let owner_ctx = CommandResolutionContext {
        self_user_id: UserId(42),
        ..admin_ctx
    };
    assert_eq!(scoped.resolve(&owner_ctx, "en")[0].command, "secret");

    // ChatBotCommands update tracking.
    let mut chat_cmds = ChatBotCommands::new();
    assert!(chat_cmds.update(vec![BotCommandsEntry {
        user_id: UserId(7),
        commands: vec![BotCommand::new("help", "Help")],
    }]));
    assert!(!chat_cmds.update(vec![BotCommandsEntry {
        user_id: UserId(7),
        commands: vec![BotCommand::new("help", "Help")],
    }]));

    // WrapCommandInChat exact behavior.
    assert_eq!(
        wrap_command_in_chat(BotStatus::HasBots, "/help", "helperbot"),
        "/help@helperbot"
    );
    assert_eq!(
        wrap_command_in_chat(BotStatus::HasBots, "/help@otherbot", "helperbot"),
        "/help@otherbot"
    );
    assert_eq!(
        wrap_command_in_chat(BotStatus::NoBots, "/help", "helperbot"),
        "/help"
    );

    // BotInfo update + privacy policy resolution precedence.
    let mut info = BotInfo::default();
    let res = info.apply_update(BotInfoUpdate {
        description: "A helpful bot".into(),
        commands: vec![BotCommand::new("Privacy", "Show privacy")],
        menu_button: BotMenuButton::WebApp {
            text: "Open App".into(),
            url: "https://app.example".into(),
        },
        ..Default::default()
    });
    assert!(res.description_changed);
    assert!(res.metadata_changed);
    assert!(info.menu_button.is_web_app());
    assert_eq!(
        resolve_privacy_policy_action(&info),
        PrivacyPolicyAction::SendPrivacyCommand("/Privacy".into())
    );
    info.privacy_policy_url = "https://example.com/privacy".into();
    assert_eq!(
        resolve_privacy_policy_action(&info),
        PrivacyPolicyAction::OpenUrl("https://example.com/privacy".into())
    );
}

#[test]
fn mini_app_theme_params_attach_menu_and_games() {
    let palette = WebViewPalette {
        window_bg: Rgba::rgb(0xFF, 0xFF, 0xFF),
        box_divider_bg: Rgba::rgb(0xF1, 0xF1, 0xF1),
        window_fg: Rgba::rgb(0x00, 0x00, 0x00),
        window_sub_text_fg: Rgba::rgb(0x99, 0x99, 0x99),
        window_active_text_fg: Rgba::rgb(0x16, 0x8A, 0xCD),
        window_bg_active: Rgba::rgb(0x40, 0xA7, 0xE3),
        window_fg_active: Rgba::rgb(0xFF, 0xFF, 0xFF),
        light_button_fg: Rgba::rgb(0x16, 0x8A, 0xCD),
        light_button_bg: Rgba::rgb(0xFF, 0xFF, 0xFF),
        attention_button_fg: Rgba::rgb(0xD1, 0x4E, 0x4E),
        shadow_fg: Rgba::new(0x00, 0x00, 0x00, 0x18), // ~9.4% black over white -> 255 - round(255 * 24/255) = 231 = 0xe7
        scroll_bg: Rgba::new(0, 0, 0, 0x1A),
        scroll_bg_over: Rgba::new(0, 0, 0, 0x2C),
        scroll_bar_bg: Rgba::new(0, 0, 0, 0x53),
        scroll_bar_bg_over: Rgba::new(0, 0, 0, 0x7A),
    };
    let app_colors = BotAppColors {
        title_day: Some(Rgba::from_serialized_rgb(0x123456)),
        title_night: None,
        body_day: None,
        body_night: None,
    };

    let params = build_theme_params(&palette, Some(&app_colors), false);
    assert_eq!(params.title_bg, Rgba::rgb(0x12, 0x34, 0x56));
    assert_eq!(params.body_bg, Rgba::rgb(0xFF, 0xFF, 0xFF));

    let parsed: Value = serde_json::from_str(&params.json).expect("valid json");
    assert_eq!(parsed["bg_color"], "#ffffff");
    assert_eq!(parsed["secondary_bg_color"], "#f1f1f1");
    assert_eq!(parsed["button_color"], "#40a7e3");
    assert_eq!(parsed["destructive_text_color"], "#d14e4e");
    assert_eq!(parsed["section_separator_color"], "#e7e7e7");
    assert_eq!(parsed.as_object().unwrap().len(), 15);

    let launch = MiniAppLaunchParams::new(UserId(1), WebViewSource::MainMenu, params);
    assert_eq!(launch.platform, MINI_APP_PLATFORM);
    assert_eq!(launch.platform, "tdesktop");

    // Attach menu store filtering and badges.
    let mut store = AttachMenuStore::new();
    let bot = AttachMenuBot {
        bot_id: UserId(10),
        bot_peer_id: PeerId(10),
        icon_document_id: None,
        short_name: "Wallet".into(),
        peer_types: PeerTypes::USER | PeerTypes::GROUP,
        inactive: false,
        in_side_menu: true,
        in_attach_menu: true,
        disclaimer_required: true,
        request_write_access: true,
    };
    store.update_bots(12345, vec![bot.clone()]);
    assert!(store.show_main_menu_new_badge(&bot));
    store.mark_disclaimer_accepted(UserId(10));
    assert!(!store.show_main_menu_new_badge(&bot));
    assert_eq!(
        store
            .visible_in_attach_menu(PeerId(20), PeerKind::User)
            .len(),
        1
    );
    assert_eq!(
        store
            .visible_in_attach_menu(PeerId(30), PeerKind::Broadcast)
            .len(),
        0
    );

    let menu_btns = compute_webview_menu_buttons(&WebViewSource::MainMenu, Some(&bot), false, true);
    assert_eq!(
        menu_btns,
        WebViewMenuButtons::OPEN_BOT
            | WebViewMenuButtons::REMOVE_FROM_MAIN
            | WebViewMenuButtons::REPORT
    );
    assert!(allow_webview_clipboard_read(
        &WebViewSource::MainMenu,
        Some(&bot)
    ));
    assert!(!needs_webview_open_confirmation(false, true, false));
    assert!(needs_webview_open_confirmation(true, true, true));
    assert!(validate_webview_external_link("https://example.com", &[]));
    assert!(!validate_webview_external_link("javascript:alert(1)", &[]));

    // Game score sharing.
    let game_src = WebViewSource::Game {
        message_id: FullMsgId::new(PeerId(42), MsgId(99)),
        title: "Corsairs".into(),
    };
    assert_eq!(
        compute_webview_menu_buttons(&game_src, None, false, false),
        WebViewMenuButtons::SHARE_GAME
    );
    assert_eq!(
        handle_share_game_score(&game_src),
        ShareGameScoreAction::ShareMessage {
            message_id: FullMsgId::new(PeerId(42), MsgId(99)),
        }
    );
    assert!(!can_share_game_to_peer(true, true, false));
    assert!(can_share_game_to_peer(false, true, false));
    assert!(can_share_game_to_peer(false, false, true));
}
