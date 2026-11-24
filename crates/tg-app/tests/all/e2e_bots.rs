//! End-to-end `egui_kittest` tests for Bot Chat integration inside the real `HistoryWidget`:
//! - Inline keyboard beneath message bubbles (callback spinner + toast, alert box, URL confirm, URL auth)
//! - Reply keyboard replacing the bottom compose drawer (button press sends message, toggle button,
//!   `single_use` auto-hide, placeholder text)
//! - Inline bot results panel (`@catalog_demo_bot query` list & gallery mosaic, pagination, switch_pm,
//!   choosing a result sends it with `via @catalog_demo_bot`)
//! - Bot menu button (Commands vs WebApp) and `/` slash-commands popup
//! - Empty bot chat info header ("What can this bot do?" + START button)
//! - Attach button offering attach-menu Mini App bots

use std::path::PathBuf;

use egui::Vec2;
use egui_kittest::kittest::Queryable;
use tg_app::bots::boxes::BotModalBox;
use tg_app::bots::mock::build_demo_reply_keyboard;
use tg_app::boxes::BoxKind;
use tg_app::boxes::peer_list::PeerListPurpose;
use tg_app::history::backend::HistoryBackend;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_app::window::section::SectionShow;
use tg_bots::commands::BotMenuButton;
use tg_bots::markup::PeerTypes;

use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, build_harness, build_wgpu_harness, settle_layout, type_text,
    wait_for_label_settled, wait_until,
};

const WORKSPACE_SIZE: Vec2 = Vec2::new(1280.0, 820.0);

fn open_peer(harness: &mut egui_kittest::Harness<'_, AppTestState>, peer_id: PeerId) {
    harness
        .state_mut()
        .controller
        .session_mut()
        .expect("session")
        .show_peer_history(peer_id, SectionShow::clear_stack(), MsgId::SHOW_AT_END);
    wait_until(
        harness,
        DEFAULT_MAX_FRAMES,
        "switching to bot peer did not settle",
        |h| {
            !h.state().controller.is_animating()
                && h.state()
                    .controller
                    .session()
                    .is_some_and(|s| s.active_peer() == peer_id)
                && h.state_mut()
                    .controller
                    .main_widget_mut()
                    .history_mut()
                    .active_peer()
                    == peer_id
        },
    );
    settle_layout(harness);
}

fn focus_compose_input(harness: &mut egui_kittest::Harness<'_, AppTestState>) {
    wait_until(
        harness,
        DEFAULT_MAX_FRAMES,
        "compose bar must be rendered before focusing",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .last_compose_rect()
                .is_positive()
        },
    );
    let compose_rect = harness
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .last_compose_rect();
    let node = harness
        .query_all_by_role(egui::accesskit::Role::TextInput)
        .chain(harness.query_all_by_role(egui::accesskit::Role::MultilineTextInput))
        .find(|n| compose_rect.contains(n.rect().center()))
        .expect("compose input TextEdit should be discoverable via AccessKit");
    node.click();
    harness.step();
}

#[test]
fn bot_inline_callback_button_shows_spinner_and_toast() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);
    open_peer(&mut harness, session_peers::BOT_CATALOG);

    // Inline keyboard button under the bot bubble is discoverable and clickable by label.
    wait_for_label_settled(&mut harness, "Refresh Status");
    harness.get_by_label("Refresh Status").click();

    // Spinner loading state is active while callback RPC is in flight.
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "callback button must enter loading spinner state immediately after click",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .inner()
                .inline_kb_state()
                .loading_count()
                > 0
        },
    );

    // Advance deterministically past CALLBACK_SPINNER_DELAY_MS until spinner clears and toast appears.
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "spinner must clear and display callback toast after completion",
        |h| {
            let hist = h.state_mut().controller.main_widget_mut().history_mut();
            hist.inner().inline_kb_state().loading_count() == 0
                && hist
                    .active_toast()
                    .is_some_and(|t| t.contains("Catalog synced"))
        },
    );
}

#[test]
fn bot_inline_alert_url_confirm_and_url_auth_modals() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);
    open_peer(&mut harness, session_peers::BOT_CATALOG);

    // 1. Callback alert (`alert = true`) opens CallbackAlert modal box.
    harness.get_by_label("Show Alert").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Show Alert callback must open BotModalBox::CallbackAlert",
        |h| {
            matches!(
                h.state_mut()
                    .controller
                    .main_widget_mut()
                    .history_mut()
                    .active_bot_modal(),
                Some(BotModalBox::CallbackAlert { .. })
            ) && h.query_by_label("OK").is_some()
        },
    );
    harness.get_by_label("OK").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "OK must close CallbackAlert modal",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .active_bot_modal()
                .is_none()
        },
    );

    // 2. External URL button opens UrlConfirm modal box.
    harness.get_by_label("Open Docs").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Open Docs URL button must open BotModalBox::UrlConfirm",
        |h| {
            matches!(
                h.state_mut()
                    .controller
                    .main_widget_mut()
                    .history_mut()
                    .active_bot_modal(),
                Some(BotModalBox::UrlConfirm { .. })
            ) && h.query_by_label("Cancel").is_some()
        },
    );
    harness.get_by_label("Cancel").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Cancel must close UrlConfirm modal",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .active_bot_modal()
                .is_none()
        },
    );

    // 3. Login URL button runs requestUrlAuth and opens BotModalBox::UrlAuth.
    harness.get_by_label("Login via Telegram").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Login via Telegram button must open BotModalBox::UrlAuth",
        |h| {
            matches!(
                h.state_mut()
                    .controller
                    .main_widget_mut()
                    .history_mut()
                    .active_bot_modal(),
                Some(BotModalBox::UrlAuth { .. })
            ) && h.query_by_label("Log In").is_some()
        },
    );
    harness.get_by_label("Log In").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "accepting URL auth must show login confirmation toast",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .active_toast()
                .is_some_and(|t| t.contains("Logged in"))
        },
    );
}

#[test]
fn bot_reply_keyboard_sends_message_toggles_and_honors_single_use() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);
    open_peer(&mut harness, session_peers::BOT_CATALOG);

    // Reply keyboard is active with placeholder text and toggle button.
    {
        let hist = harness
            .state_mut()
            .controller
            .main_widget_mut()
            .history_mut();
        assert!(hist.reply_keyboard().is_shown());
        assert_eq!(
            hist.reply_keyboard().placeholder(),
            Some("Choose a bot command or type a message...")
        );
    }

    let before_len = harness
        .state()
        .controller
        .mock_session()
        .load_slice_around(session_peers::BOT_CATALOG, MsgId::SHOW_AT_END, 50, 0)
        .messages
        .len();

    // Press reply keyboard button "Browse Catalog" and verify sent message appears in chat.
    harness.get_by_label("Browse Catalog").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "reply keyboard button must send 'Browse Catalog'",
        |h| {
            let msgs = h
                .state()
                .controller
                .mock_session()
                .load_slice_around(session_peers::BOT_CATALOG, MsgId::SHOW_AT_END, 50, 0)
                .messages;
            msgs.len() == before_len + 1
                && msgs
                    .last()
                    .is_some_and(|m| m.text.text == "Browse Catalog" && m.outgoing)
        },
    );

    // Toggle button hides and re-shows the reply keyboard.
    harness.get_by_label("Hide bot keyboard").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking Hide bot keyboard must collapse reply keyboard",
        |h| {
            !h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .reply_keyboard()
                .is_shown()
                && h.query_by_label("Show bot keyboard").is_some()
        },
    );
    harness.get_by_label("Show bot keyboard").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking Show bot keyboard must expand reply keyboard",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .reply_keyboard()
                .is_shown()
        },
    );

    // Switch to a single_use reply keyboard and verify pressing a button hides it automatically.
    let single_use_kb = build_demo_reply_keyboard(true, true, false, false);
    harness
        .state_mut()
        .controller
        .mock_session_mut()
        .set_bot_reply_keyboard(session_peers::BOT_CATALOG, single_use_kb);
    // Re-open peer to load the updated single_use markup.
    open_peer(&mut harness, session_peers::USER_MIRA);
    open_peer(&mut harness, session_peers::BOT_CATALOG);
    assert!(
        harness
            .state_mut()
            .controller
            .main_widget_mut()
            .history_mut()
            .reply_keyboard()
            .is_shown()
    );
    harness.get_by_label("My Orders").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "single_use reply keyboard must hide automatically after pressing a button",
        |h| {
            !h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .reply_keyboard()
                .is_shown()
        },
    );
}

#[test]
fn bot_inline_query_opens_panel_and_picks_result() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);
    open_peer(&mut harness, session_peers::BOT_CATALOG);

    // Hide reply keyboard so the inline results panel has generous room.
    harness.get_by_label("Hide bot keyboard").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "reply keyboard must collapse before inline query",
        |h| {
            !h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .reply_keyboard()
                .is_shown()
        },
    );

    let before_len = harness
        .state()
        .controller
        .mock_session()
        .load_slice_around(session_peers::BOT_CATALOG, MsgId::SHOW_AT_END, 50, 0)
        .messages
        .len();

    // Type `@catalog_demo_bot catalog` into compose field and wait past the 400 ms debounce.
    focus_compose_input(&mut harness);
    type_text(&mut harness, "@catalog_demo_bot catalog");
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "inline results panel must open with results after typing @catalog_demo_bot catalog",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .inline_results()
                .should_render()
                && h.query_by_label("Open Full Catalog Mini App").is_some()
                && h.query_by_label("Rustgram Bot API Guide").is_some()
        },
    );

    // Pick a result and verify it is sent into the chat with via @catalog_demo_bot.
    harness.get_by_label("Rustgram Bot API Guide").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "picking inline result must send message via @catalog_demo_bot",
        |h| {
            let msgs = h
                .state()
                .controller
                .mock_session()
                .load_slice_around(session_peers::BOT_CATALOG, MsgId::SHOW_AT_END, 50, 0)
                .messages;
            msgs.len() == before_len + 1
                && msgs.last().is_some_and(|m| {
                    m.text.text.contains("Rustgram Bot API Guide")
                        && m.extra.via_bot.as_deref() == Some("catalog_demo_bot")
                })
        },
    );

    // Also verify gallery mosaic mode + pagination with `@catalog_demo_bot photos`.
    focus_compose_input(&mut harness);
    type_text(&mut harness, "@catalog_demo_bot photos");
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "gallery mosaic query must show List view, switch_pm, and pagination button",
        |h| {
            h.query_by_label("List view").is_some()
                && h.query_by_label("Configure Gallery Filters in Bot PM")
                    .is_some()
                && h.query_by_label("Load more results…").is_some()
        },
    );
    let initial_loads = harness
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .inline_results()
        .page_loads;
    harness.get_by_label("Load more results…").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking 'Load more results…' must fetch the next page via next_offset",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .inline_results()
                .page_loads
                > initial_loads
        },
    );
}

#[test]
fn bot_slash_opens_commands_popup_and_executes_command() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);
    open_peer(&mut harness, session_peers::BOT_CATALOG);

    // Bot menu button ("Menu") is visible in the compose bar.
    assert!(
        harness.query_by_label("Menu").is_some(),
        "bot menu button must appear in compose bar of bot chats"
    );

    let before_len = harness
        .state()
        .controller
        .mock_session()
        .load_slice_around(session_peers::BOT_CATALOG, MsgId::SHOW_AT_END, 50, 0)
        .messages
        .len();

    // Typing "/" opens the bot commands popup above the compose field.
    focus_compose_input(&mut harness);
    type_text(&mut harness, "/");
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "typing '/' in a bot chat must open the commands popup with '/order'",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .is_bot_commands_popup_open()
                && h.query_by_label("/order").is_some()
        },
    );

    // Click "/order" in the popup and verify it sends the command and closes the popup.
    harness.get_by_label("/order").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "choosing '/order' must send the command and close the commands popup",
        |h| {
            !h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .is_bot_commands_popup_open()
                && h.state()
                    .controller
                    .mock_session()
                    .load_slice_around(session_peers::BOT_CATALOG, MsgId::SHOW_AT_END, 50, 0)
                    .messages
                    .len()
                    == before_len + 1
        },
    );
    let after = harness
        .state()
        .controller
        .mock_session()
        .load_slice_around(session_peers::BOT_CATALOG, MsgId::SHOW_AT_END, 50, 0)
        .messages;
    assert_eq!(after.last().expect("cmd msg").text.text, "/order");

    // Switching menu button to WebApp shows the custom WebApp label in the compose bar.
    harness
        .state_mut()
        .controller
        .mock_session_mut()
        .set_bot_menu_button(
            session_peers::BOT_CATALOG,
            BotMenuButton::WebApp {
                text: "Open Shop".to_owned(),
                url: "https://shop.example.org/miniapp".to_owned(),
            },
        );
    wait_for_label_settled(&mut harness, "Open Shop");
}

#[test]
fn bot_empty_chat_shows_info_header_and_start_button() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // Clear Catalog Bot's history before opening it so it presents an empty bot chat.
    harness
        .state_mut()
        .controller
        .mock_session_mut()
        .clear_peer_history(session_peers::BOT_CATALOG);
    open_peer(&mut harness, session_peers::BOT_CATALOG);

    wait_for_label_settled(&mut harness, "START");

    // Click START -> sends /start and populates bot welcome messages + keyboards.
    harness.get_by_label("START").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking START must send /start and activate the reply keyboard",
        |h| {
            let has_start = h
                .state()
                .controller
                .mock_session()
                .load_slice_around(session_peers::BOT_CATALOG, MsgId::SHOW_AT_END, 50, 0)
                .messages
                .iter()
                .any(|m| m.text.text == "/start");
            has_start
                && h.state_mut()
                    .controller
                    .main_widget_mut()
                    .history_mut()
                    .reply_keyboard()
                    .is_shown()
        },
    );
}

#[test]
fn bot_attach_button_offers_attach_menu_bots() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);
    open_peer(&mut harness, session_peers::BOT_CATALOG);

    harness.get_by_label("Attach").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking Attach must open the attach-menu bots popup",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .is_bot_attach_popup_open()
                && h.query_by_label("Catalog Shop").is_some()
                && h.query_by_label("Quick Wallet").is_some()
        },
    );

    // Launching a bot that requires a disclaimer opens AttachBotDisclaimer modal.
    harness.get_by_label("Quick Wallet").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "launching disclaimer-gated attach bot must open BotModalBox::AttachBotDisclaimer",
        |h| {
            matches!(
                h.state_mut()
                    .controller
                    .main_widget_mut()
                    .history_mut()
                    .active_bot_modal(),
                Some(BotModalBox::AttachBotDisclaimer { .. })
            )
        },
    );
}

#[test]
fn mini_app_launch_from_inline_reply_menu_and_attach_routes_to_webview_host() {
    let mut harness = build_harness(AppTestState::main_with_webview(), WORKSPACE_SIZE);
    settle_layout(&mut harness);
    open_peer(&mut harness, session_peers::BOT_CATALOG);

    // 1. Inline keyboard `WebView` button -> embedded Mini App panel in AppWebViewHost.
    harness.get_by_label("Open Store App").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "inline WebView button must open embedded Mini App panel",
        |h| {
            h.state()
                .webview_host
                .as_ref()
                .and_then(|host| host.mini_app())
                .is_some_and(|panel| panel.is_open())
        },
    );
    {
        let host = harness.state().webview_host.as_ref().expect("webview host");
        let panel = host.mini_app().expect("mini app panel");
        assert_eq!(panel.bot().id, session_peers::BOT_CATALOG.bare());
        assert!(!panel.bot().is_game);
    }
    {
        let log = harness
            .state()
            .controller
            .mock_session()
            .webview_requests_log();
        let req = log.last().expect("webview request logged");
        assert_eq!(req.bot_id, session_peers::BOT_CATALOG.bare());
        assert_eq!(
            req.url_override.as_deref(),
            Some("https://shop.example.org/miniapp")
        );
        assert!(
            req.theme_params_json.contains("bg_color"),
            "theme_params_json must be populated from active palette"
        );
    }

    // 2. Reply keyboard `SimpleWebView` button -> embedded Mini App panel.
    harness.get_by_label("Quick Web App").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "simple_web_view request must be logged",
        |h| {
            h.state()
                .controller
                .mock_session()
                .webview_requests_log()
                .last()
                .is_some_and(|req| {
                    req.bot_id == session_peers::BOT_CATALOG.bare()
                        && req.url_override.as_deref() == Some("https://shop.example.org/quick")
                })
        },
    );

    // 3. Bot menu "WebApp" button in compose bar -> embedded Mini App panel.
    harness
        .state_mut()
        .controller
        .mock_session_mut()
        .set_bot_menu_button(
            session_peers::BOT_CATALOG,
            BotMenuButton::WebApp {
                text: "Open Shop".to_owned(),
                url: "https://shop.example.org/menu-webapp".to_owned(),
            },
        );
    wait_for_label_settled(&mut harness, "Open Shop");
    harness.get_by_label("Open Shop").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "menu webapp request must be logged",
        |h| {
            h.state()
                .controller
                .mock_session()
                .webview_requests_log()
                .last()
                .is_some_and(|req| {
                    req.bot_id == session_peers::BOT_CATALOG.bare()
                        && req.url_override.as_deref()
                            == Some("https://shop.example.org/menu-webapp")
                })
        },
    );

    // 4. Attach-menu bot -> embedded Mini App panel (both direct and disclaimer-confirmed).
    let before_attach_log = harness
        .state()
        .controller
        .mock_session()
        .webview_requests_log()
        .len();
    harness.get_by_label("Attach").click();
    wait_for_label_settled(&mut harness, "Catalog Shop");
    harness.get_by_label("Catalog Shop").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "attach bot request must be logged and attach popup closed",
        |h| {
            let log = h.state().controller.mock_session().webview_requests_log();
            !h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .is_bot_attach_popup_open()
                && log.len() > before_attach_log
                && log.last().is_some_and(|req| {
                    req.bot_id == session_peers::BOT_CATALOG.bare() && req.url_override.is_none()
                })
        },
    );

    harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "Attach")
        .click();
    wait_for_label_settled(&mut harness, "Quick Wallet");
    harness.get_by_label("Quick Wallet").click();
    wait_for_label_settled(&mut harness, "Continue");
    harness.get_by_label("Continue").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "disclaimer-accepted attach bot must open Quick Wallet Mini App panel",
        |h| {
            h.state()
                .webview_host
                .as_ref()
                .and_then(|host| host.mini_app())
                .is_some_and(|panel| {
                    panel.bot().id == 777_002 && panel.bot().title == "Quick Wallet"
                })
        },
    );

    // None of these Mini App launches should fall back to the external system browser.
    assert!(
        harness
            .state()
            .opened_external_urls
            .lock()
            .expect("lock")
            .is_empty(),
        "Mini App launches must route to AppWebViewHost, not ExternalOpener"
    );
}

#[test]
fn switch_inline_to_another_chat_filters_picker_and_prefills_target_compose() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);
    open_peer(&mut harness, session_peers::BOT_CATALOG);

    // Click "Share Inline..." (`ButtonKind::SwitchInline { query: "photos", peer_types: USER | GROUP }`).
    harness.get_by_label("Share Inline...").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Share Inline... must open and settle the filtered PeerList picker modal",
        |h| {
            !h.state().controller.is_animating()
                && matches!(
                    h.state().controller.box_host().top_kind(),
                    Some(BoxKind::PeerList(st))
                        if matches!(
                            &st.purpose,
                            PeerListPurpose::SwitchInline {
                                bot_username,
                                query,
                                peer_types,
                                ..
                            } if bot_username == "catalog_demo_bot"
                                && query == "photos"
                                && *peer_types == (PeerTypes::USER | PeerTypes::GROUP)
                        )
                )
        },
    );
    harness.step();

    // Allowed peer types (USER: Mira Vance, GROUP: Rustgram Core Port) appear inside the centered modal,
    // while disallowed types (CHANNEL: Rustgram Dev Log, BOT: Catalog Bot) are filtered out.
    let modal_center = WORKSPACE_SIZE * 0.5;
    let in_modal = |rect: egui::Rect| (rect.center().x - modal_center.x).abs() < 260.0;

    let mira_btn = harness
        .query_all_by_label("Mira Vance")
        .find(|n| in_modal(n.rect()))
        .expect("allowed USER peer 'Mira Vance' must be listed in switch_inline picker");
    assert!(
        harness
            .query_all_by_label("Rustgram Core Port")
            .any(|n| in_modal(n.rect())),
        "allowed GROUP peer 'Rustgram Core Port' must be listed in switch_inline picker"
    );
    assert!(
        !harness
            .query_all_by_label("Rustgram Dev Log")
            .any(|n| in_modal(n.rect())),
        "disallowed CHANNEL peer 'Rustgram Dev Log' must be filtered out of switch_inline picker"
    );
    assert!(
        !harness
            .query_all_by_label("Catalog Bot")
            .any(|n| in_modal(n.rect())),
        "disallowed BOT peer 'Catalog Bot' must be filtered out of switch_inline picker"
    );

    // Choose Mira Vance and confirm with "Select".
    mira_btn.click();
    wait_for_label_settled(&mut harness, "Select");
    harness.get_by_label("Select").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "confirming a peer must close picker, open Mira Vance, prefill '@bot query', and open inline results",
        |h| {
            if h.state().controller.is_animating()
                || !h.state().controller.box_host().is_empty()
                || h.state()
                    .controller
                    .session()
                    .is_none_or(|s| s.active_peer() != session_peers::USER_MIRA)
            {
                return false;
            }
            let history = h.state_mut().controller.main_widget_mut().history_mut();
            history.compose_mut().text == "@catalog_demo_bot photos"
                && history.inline_results().should_render()
        },
    );
}

#[test]
fn game_card_renders_play_opens_game_webview_and_share_score_opens_box() {
    let mut harness = build_harness(AppTestState::main_with_webview(), WORKSPACE_SIZE);
    settle_layout(&mut harness);
    open_peer(&mut harness, session_peers::BOT_CATALOG);

    // Collapse the reply keyboard and scroll to top of Catalog Bot history so the game card is visible.
    harness.get_by_label("Hide bot keyboard").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "reply keyboard must collapse before scrolling to game card",
        |h| {
            !h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .reply_keyboard()
                .is_shown()
        },
    );
    harness
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .inner_mut()
        .set_scroll_offset(0.0);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "game card 'Star Hopper' and its inline buttons must render after scrolling to top",
        |h| {
            h.query_by_label("Star Hopper").is_some()
                && h.query_by_label("Play Star Hopper").is_some()
                && h.query_by_label("Share Score").is_some()
        },
    );

    // 1. Clicking Play sends a game callback -> receives game URL -> opens embedded WebView!
    harness.get_by_label("Play Star Hopper").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Play button must show callback loading spinner while fetching game URL",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .inner()
                .inline_kb_state()
                .loading_count()
                > 0
        },
    );

    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Play game callback URL must launch embedded WebView panel",
        |h| {
            h.state()
                .webview_host
                .as_ref()
                .and_then(|host| host.mini_app())
                .is_some_and(|panel| panel.is_open() && panel.bot().is_game)
        },
    );
    {
        let log = harness
            .state()
            .controller
            .mock_session()
            .webview_requests_log();
        let req = log.last().expect("game webview request logged");
        assert_eq!(req.bot_id, session_peers::BOT_CATALOG.bare());
        assert_eq!(
            req.url_override.as_deref(),
            Some("https://games.example.org/star-hopper#score=3950")
        );
    }

    // 2. Clicking the game card itself also triggers the Play callback -> WebView flow.
    let before_reqs = harness
        .state()
        .controller
        .mock_session()
        .webview_requests_log()
        .len();
    harness.get_by_label("Star Hopper").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking the game card itself must also trigger Play -> embedded WebView",
        |h| {
            h.state()
                .controller
                .mock_session()
                .webview_requests_log()
                .len()
                == before_reqs + 1
        },
    );

    // 3. Clicking "Share Score" opens the existing ShareGameScore modal box.
    harness.get_by_label("Share Score").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Share Score button must open BotModalBox::ShareGameScore with Star Hopper leaderboard",
        |h| {
            matches!(
                h.state_mut()
                    .controller
                    .main_widget_mut()
                    .history_mut()
                    .active_bot_modal(),
                Some(BotModalBox::ShareGameScore {
                    game,
                    leaderboard,
                    ..
                }) if game.title == "Star Hopper" && leaderboard.len() == 3
            ) && h.query_by_label("Copy Link").is_some()
        },
    );

    // Clicking "Copy Link" inside ShareGameScore closes the modal and copies the canonical link.
    harness.get_by_label("Copy Link").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Copy Link must close modal and show toast with canonical game share URL",
        |h| {
            let hist = h.state_mut().controller.main_widget_mut().history_mut();
            hist.active_bot_modal().is_none()
                && hist
                    .active_toast()
                    .is_some_and(|t| t.contains("https://t.me/catalog_demo_bot?game=star_hopper"))
        },
    );
}

#[test]
#[ignore = "regenerates crates/tg-app/docs/bots_in_chat.png screenshot"]
fn capture_bots_in_chat_screenshot() {
    let mut harness = build_wgpu_harness(AppTestState::main_workspace(), Vec2::new(1280.0, 820.0));
    settle_layout(&mut harness);
    open_peer(&mut harness, session_peers::BOT_CATALOG);
    settle_layout(&mut harness);

    let img = harness.render().expect("wgpu render bots_in_chat");
    let out_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("bots_in_chat.png");
    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    img.save(&out_path).expect("save bots_in_chat.png");
    assert!(out_path.exists());
}
