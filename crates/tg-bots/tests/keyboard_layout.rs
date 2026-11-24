//! Known-answer tests for reply markup parsing, forwarding, request-peer criteria,
//! and `ReplyKeyboard` / `BotKeyboard` layout math.

use tg_bots::keyboard_layout::{
    BOT_KB_SCROLL_DELTA_BOTTOM, BOT_KB_SCROLL_DELTA_TOP, BotKeyboardMetrics, ButtonRounding,
    ButtonsLayout, CornerRounding, LayoutButtonInput, Rect,
};
use tg_bots::markup::{
    ButtonColor, ButtonKind, ButtonType, ButtonTypeIcon, ButtonVisual, CreateBotRequest,
    InlineQueryPeerTypeInput, MarkupButton, PeerTypes, PollRequestRestriction, ReplyKeyboardState,
    ReplyMarkupData, ReplyMarkupFlags, ReplyMarkupInput, RequestPeerCandidate, RequestPeerQuery,
    RequestPeerRestriction, RequestPeerType, peer_types_from_inline_query,
};
use tg_bots::stand_in::{ChatAdminRights, DocumentId, FullMsgId, MsgId, PeerId, UserId};

#[test]
fn keyboard_layout_inline_enough_space_known_answer() {
    // 2 buttons in a row, width = 300, inline metrics: skip = 2, padding = 10, height = 36.
    // Callback loads_on_activate -> icon_width = 12 -> min_button_width = max(20, 2*12 + 4*4) = 40.
    // Url icon_width = 10 -> min_button_width = max(20, 2*10 + 4*4) = 36.
    assert_eq!(
        BotKeyboardMetrics::INLINE.min_button_width(ButtonType::Callback),
        40
    );
    assert_eq!(
        BotKeyboardMetrics::INLINE.min_button_width(ButtonType::Url),
        36
    );
    assert_eq!(
        BotKeyboardMetrics::INLINE.min_button_width(ButtonType::Default),
        20
    );

    let rows = vec![
        vec![
            LayoutButtonInput::new(60, ButtonType::Callback),
            LayoutButtonInput::new(50, ButtonType::Url),
        ],
        vec![LayoutButtonInput::new(100, ButtonType::Buy)],
    ];
    let mut layout = ButtonsLayout::new(rows, BotKeyboardMetrics::INLINE);

    // naturalWidth:
    // Row 0: maxMinButtonWidth = max(40, 36) = 40;
    //        rowMaxButtonWidth = max(60 + 40, 50 + 40) = 100;
    //        rowWidth = 2 * 100 + 1 * 2 = 202.
    // Row 1: Buy icon=10 -> min=36; rowMax = 100 + 36 = 136.
    // naturalWidth = max(202, 136) = 202.
    assert_eq!(layout.natural_width(), 202);
    // naturalHeight = (2 - 1) * 2 + 2 * 36 = 74.
    assert_eq!(layout.natural_height(), 74);

    layout.resize(300, 74);
    // Row 0: widthForButtons = 300 - 2 = 298.
    //        widthForText = 298 - 40 - 36 = 222. widthOfText = 110.
    //        exact = false; enough = (298 - 2*40) >= 110 -> 218 >= 110 -> true.
    //        w = 298 / 2 = 149.0 each.
    //        col 0: x=0, w=149 -> rect(0, 0, 149, 36), textw = 149 - 40 = 109 >= 60 (full).
    //        col 1: x=151, w=149 -> rect(151, 0, 149, 36), textw = 149 - 36 = 113 >= 50 (full).
    // Row 1: y = 38.0, w = 300 -> rect(0, 38, 300, 36).
    let r = layout.rows();
    assert_eq!(r[0][0].rect, Rect::new(0, 0, 149, 36));
    assert!(r[0][0].full_displayed);
    assert_eq!(r[0][1].rect, Rect::new(151, 0, 149, 36));
    assert!(r[0][1].full_displayed);
    assert_eq!(r[1][0].rect, Rect::new(0, 38, 300, 36));
    assert!(r[1][0].full_displayed);

    // Hit testing and flattened index lookup.
    assert_eq!(layout.hit_test(10, 10, false), Some((0, 0)));
    assert_eq!(layout.hit_test(150, 10, false), None); // inside 2px skip gap
    assert_eq!(layout.hit_test(160, 10, false), Some((0, 1)));
    assert_eq!(layout.hit_test(50, 45, false), Some((1, 0)));
    // Admin log only allows Url and Callback, rejecting Buy.
    assert_eq!(layout.hit_test(50, 45, true), None);
    assert_eq!(layout.hit_test(160, 10, true), Some((0, 1)));
    assert_eq!(layout.coords_by_index(0), Some((0, 0)));
    assert_eq!(layout.coords_by_index(2), Some((1, 0)));
    assert_eq!(layout.coords_by_index(3), None);
}

#[test]
fn keyboard_layout_exact_and_tight_branches_known_answer() {
    // Exact branch: widthForText == widthOfText
    // 2 buttons: text widths 40 and 80, Default (minw = 20 each), skip = 2.
    // widthForButtons = (40 + 20) + (80 + 20) = 160 -> total width = 162.
    let rows = vec![vec![
        LayoutButtonInput::new(40, ButtonType::Default),
        LayoutButtonInput::new(80, ButtonType::Default),
    ]];
    let mut layout = ButtonsLayout::new(rows, BotKeyboardMetrics::INLINE);
    layout.resize(162, 36);
    // In exact branch, each button gets exactly textw + minw: 60 and 100!
    assert_eq!(layout.rows()[0][0].rect, Rect::new(0, 0, 60, 36));
    assert_eq!(layout.rows()[0][1].rect, Rect::new(62, 0, 100, 36));
    assert!(layout.rows()[0][0].full_displayed);
    assert!(layout.rows()[0][1].full_displayed);

    // Tight branch (!exact && !enough):
    // 2 buttons: Callback (minw = 40, text = 100) + Default (minw = 20, text = 100) in width = 150.
    // widthForButtons = 148; widthForText = 148 - 40 - 20 = 88; widthOfText = 200.
    // enough = (148 - 2*40) = 68 >= 200 -> false.
    // textw = 88 / 2 = 44.0.
    // col 0: w = max(40 + 44, 20) = 84.0 -> rect(0, 0, 84, 36), full_displayed = (44 >= 100) false.
    // col 1: x = 86.0, w = max(20 + 44, 20) = 64.0 -> rect(86, 0, 64, 36), full_displayed = false.
    let tight_rows = vec![vec![
        LayoutButtonInput::new(100, ButtonType::Callback),
        LayoutButtonInput::new(100, ButtonType::Default),
    ]];
    let mut tight = ButtonsLayout::new(tight_rows, BotKeyboardMetrics::INLINE);
    tight.resize(150, 36);
    assert_eq!(tight.rows()[0][0].rect, Rect::new(0, 0, 84, 36));
    assert!(!tight.rows()[0][0].full_displayed);
    assert_eq!(tight.rows()[0][1].rect, Rect::new(86, 0, 64, 36));
    assert!(!tight.rows()[0][1].full_displayed);
}

#[test]
fn keyboard_layout_rtl_and_rounding_known_answer() {
    let rows = vec![
        vec![
            LayoutButtonInput::new(40, ButtonType::Default),
            LayoutButtonInput::new(80, ButtonType::Default),
        ],
        vec![LayoutButtonInput::new(50, ButtonType::Default)],
    ];
    let mut layout = ButtonsLayout::new(rows, BotKeyboardMetrics::INLINE);
    layout.set_rtl(true);
    layout.resize(162, 74);
    // Exact LTR rects: col 0 (0, 0, 60, 36), col 1 (62, 0, 100, 36)
    // RTL mirrors x -> width - x - w: col 0 (102, 0, 60, 36), col 1 (0, 0, 100, 36)
    assert_eq!(layout.rows()[0][0].rect, Rect::new(102, 0, 60, 36));
    assert_eq!(layout.rows()[0][1].rect, Rect::new(0, 0, 100, 36));

    // Inline rounding: top corners are always Small; bottom row outer corners take Large.
    let top_left_inline = layout.button_rounding(0, 0, ButtonRounding::ALL_LARGE, true);
    assert_eq!(top_left_inline, ButtonRounding::ALL_SMALL);
    let bottom_inline = layout.button_rounding(1, 0, ButtonRounding::ALL_LARGE, true);
    assert_eq!(bottom_inline.top_left, CornerRounding::Small);
    assert_eq!(bottom_inline.bottom_left, CornerRounding::Large);
    assert_eq!(bottom_inline.bottom_right, CornerRounding::Large);

    // Reply keyboard rounding: top corners also inherit Large.
    let top_left_reply = layout.button_rounding(0, 0, ButtonRounding::ALL_LARGE, false);
    assert_eq!(top_left_reply.top_left, CornerRounding::Large);
    assert_eq!(top_left_reply.top_right, CornerRounding::Small);
}

#[test]
fn reply_keyboard_normal_vs_tiny_and_maximize_size() {
    // 4 buttons in one row with text width 60 each:
    // Normal style (margin=10, padding=10): overhead = 3*10 + 4*20 = 110; text = 240 -> needs 350.
    // In outer_width = 300, implWidth = 300 - 10 - 14 = 276 < 350 and row.len() > 3 -> switches to REPLY_TINY!
    let rows = vec![vec![
        LayoutButtonInput::new(60, ButtonType::Default),
        LayoutButtonInput::new(60, ButtonType::Default),
        LayoutButtonInput::new(60, ButtonType::Default),
        LayoutButtonInput::new(60, ButtonType::Default),
    ]];
    let mut layout = ButtonsLayout::new(rows, BotKeyboardMetrics::REPLY_NORMAL);
    let h_resized = layout.resize_reply_keyboard(300, 200, false);
    assert_eq!(layout.metrics(), BotKeyboardMetrics::REPLY_TINY);
    assert_eq!(
        h_resized,
        BOT_KB_SCROLL_DELTA_TOP
            + BOT_KB_SCROLL_DELTA_BOTTOM
            + BotKeyboardMetrics::REPLY_TINY.button_height
    );

    // With maximize_size = true (!RESIZE), height expands to max_outer_height = 200.
    let h_max = layout.resize_reply_keyboard(300, 200, true);
    assert_eq!(h_max, 200);

    // A 3-button row that overflows does NOT switch to tiny (`row.size() > 3` rule in tdesktop).
    let three_rows = vec![vec![
        LayoutButtonInput::new(100, ButtonType::Default),
        LayoutButtonInput::new(100, ButtonType::Default),
        LayoutButtonInput::new(100, ButtonType::Default),
    ]];
    let mut three = ButtonsLayout::new(three_rows, BotKeyboardMetrics::REPLY_NORMAL);
    assert!(three.is_enough_space(200, BotKeyboardMetrics::REPLY_NORMAL));
    let _ = three.resize_reply_keyboard(224, 200, false);
    assert_eq!(three.metrics(), BotKeyboardMetrics::REPLY_NORMAL);
}

#[test]
fn reply_markup_all_flags_forwarding_and_state() {
    // Inline markup with SwitchInlineSame, Auth with forward_text, mini-app URL, etc.
    let markup = ReplyMarkupData::from_input(Some(ReplyMarkupInput::Inline {
        rows: vec![
            vec![
                MarkupButton::new(
                    "Login",
                    ButtonKind::Auth {
                        url: "https://example.com/auth".into(),
                        forward_text: "Open Example".into(),
                        button_id: 42,
                    },
                ),
                MarkupButton::new(
                    "Mini App",
                    ButtonKind::Url {
                        url: "https://t.me/samplebot?startapp=abc".into(),
                    },
                ),
            ],
            vec![
                MarkupButton::new(
                    "Inline Here",
                    ButtonKind::SwitchInlineSame {
                        query: "cats".into(),
                    },
                ),
                MarkupButton::with_visual(
                    "Delete",
                    ButtonVisual {
                        icon_id: DocumentId(99),
                        color: ButtonColor::from_style_flags(true, true, true),
                    },
                    ButtonKind::CallbackWithPassword {
                        data: b"del:1".to_vec(),
                    },
                ),
            ],
        ],
        force_reply: false,
    }));

    assert!(markup.is_inline());
    assert!(!markup.is_null());
    assert!(!markup.defines_reply_keyboard());
    // Mini app URL button upgrades its corner icon to Webview.
    assert_eq!(
        markup.button(0, 1).unwrap().effective_icon_type(),
        ButtonType::WebView
    );
    assert_eq!(
        markup.button(0, 1).unwrap().corner_icon(),
        ButtonTypeIcon::Webview
    );
    // Danger takes precedence over Primary & Success.
    assert_eq!(
        markup.button(1, 1).unwrap().visual.color,
        ButtonColor::Danger
    );

    let forwarded = markup.forwarded();
    assert_eq!(forwarded.button(0, 0).unwrap().text, "Open Example");
    assert!(matches!(
        &forwarded.button(1, 0).unwrap().kind,
        ButtonKind::SwitchInline { query, peer_types } if query == "cats" && peer_types.is_empty()
    ));
    assert!(
        forwarded
            .flags
            .contains(ReplyMarkupFlags::HAS_SWITCH_INLINE)
    );

    // OnlyBuyButton optimization flag.
    let buy_only = ReplyMarkupData::from_input(Some(ReplyMarkupInput::Inline {
        rows: vec![vec![MarkupButton::new("Pay 100", ButtonKind::Buy)]],
        force_reply: false,
    }));
    assert!(buy_only.flags.contains(ReplyMarkupFlags::ONLY_BUY_BUTTON));

    // Reply keyboard state lifecycle: keyboard -> single_use hide -> force_reply -> remove.
    let kb_markup = ReplyMarkupData::from_input(Some(ReplyMarkupInput::Keyboard {
        rows: vec![vec![MarkupButton::new("Hi", ButtonKind::Default)]],
        resize: true,
        single_use: true,
        selective: false,
        persistent: true,
        force_reply: false,
        placeholder: "Pick one...".into(),
    }));
    let msg1 = FullMsgId::new(PeerId(100), MsgId(1));
    let mut state = ReplyKeyboardState::default();
    assert!(state.update_markup(Some(msg1), Some(&kb_markup), false));
    assert!(state.has_visible_rows);
    assert!(!state.maximize_size);
    assert!(state.single_use);
    assert!(state.persistent);
    assert_eq!(state.placeholder, "Pick one...");
    // Updating with same msg id without force returns false.
    assert!(!state.update_markup(Some(msg1), Some(&kb_markup), false));
    // Single-use dismiss clears it.
    assert!(state.hide_if_single_use(msg1));
    assert!(!state.has_visible_rows);

    // ForceReply without rows implicitly sets single_use = true (`bot_keyboard.cpp:361`).
    let fr = ReplyMarkupData::from_input(Some(ReplyMarkupInput::ForceReply {
        single_use: false,
        selective: true,
        placeholder: "Reply here".into(),
    }));
    let msg2 = FullMsgId::new(PeerId(100), MsgId(2));
    assert!(state.update_markup(Some(msg2), Some(&fr), false));
    assert!(state.force_reply);
    assert!(state.single_use);
    assert!(state.selective);
    assert!(!state.has_visible_rows);

    // RemoveKeyboard clears active state.
    let hide =
        ReplyMarkupData::from_input(Some(ReplyMarkupInput::RemoveKeyboard { selective: false }));
    assert!(hide.is_remove_keyboard());
    let msg3 = FullMsgId::new(PeerId(100), MsgId(3));
    assert!(state.update_markup(Some(msg3), Some(&hide), false));
    assert!(state.for_msg_id.is_none());
}

#[test]
fn request_peer_query_and_peer_types_matching() {
    let query = RequestPeerQuery {
        max_quantity: 2,
        peer_type: RequestPeerType::Group,
        user_is_bot: RequestPeerRestriction::Any,
        user_is_premium: RequestPeerRestriction::Any,
        group_is_forum: RequestPeerRestriction::Yes,
        has_username: RequestPeerRestriction::No,
        am_creator: true,
        is_bot_participant: true,
        my_rights: ChatAdminRights::CHANGE_INFO | ChatAdminRights::PIN_MESSAGES,
        bot_rights: ChatAdminRights::DELETE_MESSAGES,
    };

    let good = RequestPeerCandidate {
        peer_type: RequestPeerType::Group,
        is_forum: true,
        has_username: false,
        am_creator: true,
        is_bot_participant: true,
        my_rights: ChatAdminRights::CHANGE_INFO
            | ChatAdminRights::PIN_MESSAGES
            | ChatAdminRights::ADD_ADMINS,
        bot_rights: ChatAdminRights::DELETE_MESSAGES,
        ..Default::default()
    };
    assert!(query.matches_candidate(&good));

    let mut wrong_forum = good;
    wrong_forum.is_forum = false;
    assert!(!query.matches_candidate(&wrong_forum));

    let mut missing_right = good;
    missing_right.my_rights = ChatAdminRights::CHANGE_INFO;
    assert!(!query.matches_candidate(&missing_right));

    // InlineQueryPeerType mapping (SameBotPm maps to empty, Chat & Megagroup map to GROUP).
    let types = peer_types_from_inline_query(&[
        InlineQueryPeerTypeInput::SameBotPm,
        InlineQueryPeerTypeInput::Pm,
        InlineQueryPeerTypeInput::Megagroup,
        InlineQueryPeerTypeInput::Broadcast,
        InlineQueryPeerTypeInput::BotPm,
    ]);
    assert_eq!(
        types,
        PeerTypes::USER | PeerTypes::GROUP | PeerTypes::BROADCAST | PeerTypes::BOT
    );

    // Ensure CreateBot & UserProfile & CopyText & RequestPoll variants construct cleanly.
    let _ = MarkupButton::new(
        "Create",
        ButtonKind::CreateBot {
            button_id: 7,
            request: CreateBotRequest {
                suggested_name: "Helper".into(),
                suggested_username: "helper_bot".into(),
            },
        },
    );
    let _ = MarkupButton::new(
        "Quiz",
        ButtonKind::RequestPoll {
            restriction: PollRequestRestriction::from_quiz_option(Some(true)),
        },
    );
    let _ = MarkupButton::new(
        "Profile",
        ButtonKind::UserProfile {
            user_id: UserId(12345),
        },
    );
}
