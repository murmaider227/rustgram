//! Accessibility (`WidgetInfo` / AccessKit) integration tests via `egui_kittest`.
//!
//! Verifies that custom-painted interactive widgets across `tg-ui` and `tg-app`
//! register proper AccessKit labels/roles so screen readers and UI automation can
//! locate and activate them by label without coordinate math.

use egui::{Color32, Id, Pos2, Vec2};
use egui_kittest::kittest::Queryable;
use tg_app::dialogs::backend::{ChatFolder, FolderIconKind, FolderUnreadState};
use tg_app::dialogs::filters_menu::show_horizontal_folder_tabs;
use tg_app::history::backend::HistoryBackend;
use tg_app::history::theme::HistoryColors;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::theme::Palette;
use tg_app::stand_in::types::{FilterId, MsgId};
use tg_ui::stand_in::{
    CheckStyle, CheckboxStyle, FlatButtonStyle, IconButtonStyle, LinkButtonStyle, PopupMenuStyle,
    RadioStyle, RoundButtonStyle, ToggleStyle,
};
use tg_ui::widgets::buttons::{FlatButton, IconButton, IconKind, LinkButton, RoundButton};
use tg_ui::widgets::checkbox::{Checkbox, Radio, Toggle};
use tg_ui::widgets::menu::{MenuItem, PopupMenuState};

use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, build_harness, build_widget_harness, click_at,
    right_click_at, settle_layout, wait_for_label_settled, wait_for_menu_item, wait_until,
};

const WORKSPACE_SIZE: Vec2 = Vec2::new(1280.0, 800.0);

// =============================================================================
// tg-ui custom-painted widgets
// =============================================================================

#[test]
fn a11y_round_button_by_label() {
    struct State {
        clicked: bool,
    }
    let mut harness = build_widget_harness(
        State { clicked: false },
        Vec2::new(320.0, 120.0),
        |ui, st| {
            let colors = HistoryColors::from_palette(&Palette::day()).ui_colors();
            if RoundButton::new("Continue", RoundButtonStyle::default_active(&colors))
                .show(ui)
                .clicked()
            {
                st.clicked = true;
            }
        },
    );
    harness.step();
    harness.get_by_label("Continue").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "RoundButton click must register",
        |h| h.state().clicked,
    );
}

#[test]
fn a11y_flat_button_by_label() {
    struct State {
        clicked: bool,
    }
    let mut harness = build_widget_harness(
        State { clicked: false },
        Vec2::new(320.0, 120.0),
        |ui, st| {
            let colors = HistoryColors::from_palette(&Palette::day()).ui_colors();
            if FlatButton::new("Save Changes", FlatButtonStyle::default_flat(&colors))
                .show(ui)
                .clicked()
            {
                st.clicked = true;
            }
        },
    );
    harness.step();
    harness.get_by_label("Save Changes").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "FlatButton click must register",
        |h| h.state().clicked,
    );
}

#[test]
fn a11y_icon_button_by_tooltip_and_accessible_name() {
    struct State {
        search_clicks: usize,
        custom_clicks: usize,
        default_clicks: usize,
    }
    let mut harness = build_widget_harness(
        State {
            search_clicks: 0,
            custom_clicks: 0,
            default_clicks: 0,
        },
        Vec2::new(320.0, 120.0),
        |ui, st| {
            let colors = HistoryColors::from_palette(&Palette::day()).ui_colors();
            let style = IconButtonStyle::default_icon_button(&colors);
            ui.horizontal(|ui| {
                if IconButton::new(IconKind::Search, style)
                    .tooltip("Search messages")
                    .show(ui)
                    .clicked()
                {
                    st.search_clicks += 1;
                }
                if IconButton::new(IconKind::Settings, style)
                    .tooltip("Open settings tooltip")
                    .accessible_name("Custom Settings Name")
                    .show(ui)
                    .clicked()
                {
                    st.custom_clicks += 1;
                }
                if IconButton::new(IconKind::Close, style).show(ui).clicked() {
                    st.default_clicks += 1;
                }
            });
        },
    );
    harness.step();
    harness.get_by_label("Search messages").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "search icon click must register",
        |h| h.state().search_clicks == 1,
    );
    harness.get_by_label("Custom Settings Name").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "custom settings icon click must register",
        |h| h.state().custom_clicks == 1,
    );
    harness.get_by_label("Close").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "default close icon click must register",
        |h| h.state().default_clicks == 1,
    );
}

#[test]
fn a11y_link_button_by_label() {
    struct State {
        clicked: bool,
    }
    let mut harness = build_widget_harness(
        State { clicked: false },
        Vec2::new(320.0, 120.0),
        |ui, st| {
            let colors = HistoryColors::from_palette(&Palette::day()).ui_colors();
            if LinkButton::new("Learn more", LinkButtonStyle::default_link(&colors))
                .show(ui)
                .clicked()
            {
                st.clicked = true;
            }
        },
    );
    harness.step();
    harness.get_by_label("Learn more").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "LinkButton click must register",
        |h| h.state().clicked,
    );
}

#[test]
fn a11y_popup_menu_items_by_label() {
    struct State {
        menu: PopupMenuState,
        opened: bool,
        muted: bool,
        last_action: Option<String>,
    }
    let mut harness = build_widget_harness(
        State {
            menu: PopupMenuState::new(),
            opened: false,
            muted: false,
            last_action: None,
        },
        Vec2::new(400.0, 320.0),
        |ui, st| {
            let colors = HistoryColors::from_palette(&Palette::day()).ui_colors();
            let menu_style = PopupMenuStyle::default_popup(&colors);
            let toggle_style = ToggleStyle::default_toggle(&colors);
            if !st.opened {
                st.menu
                    .open_at(ui.ctx(), Pos2::new(24.0, 24.0), &menu_style);
                st.opened = true;
            }
            let items = vec![
                MenuItem::action("pin", "Pin chat"),
                MenuItem::Toggle {
                    id: "mute".to_owned(),
                    label: "Mute notifications".to_owned(),
                    checked: st.muted,
                    icon: None,
                },
            ];
            if let Some(id) = st.menu.show(
                ui.ctx(),
                Id::unique("a11y_popup_menu"),
                &items,
                &menu_style,
                &toggle_style,
            ) {
                if id == "mute" {
                    st.muted = !st.muted;
                }
                st.last_action = Some(id);
            }
        },
    );
    // Wait for menu reveal animation so both rows are within the animated clip rect.
    wait_for_menu_item(&mut harness, "Mute notifications");
    assert!(
        harness.query_by_label("Pin chat").is_some(),
        "PopupMenu action item must be discoverable by label"
    );
    assert!(
        harness.query_by_label("Mute notifications").is_some(),
        "PopupMenu toggle item must be discoverable by label"
    );
    harness.get_by_label("Mute notifications").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "PopupMenu mute toggle did not apply",
        |h| h.state().muted && h.state().last_action.as_deref() == Some("mute"),
    );
}

#[test]
fn a11y_checkbox_radio_and_toggle_by_label() {
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Choice {
        First,
        Second,
    }
    struct State {
        accepted: bool,
        choice: Choice,
        animations: bool,
        unlabeled_toggle: bool,
    }
    let mut harness = build_widget_harness(
        State {
            accepted: false,
            choice: Choice::First,
            animations: false,
            unlabeled_toggle: false,
        },
        Vec2::new(360.0, 260.0),
        |ui, st| {
            let colors = HistoryColors::from_palette(&Palette::day()).ui_colors();
            let cb_style = CheckboxStyle::default_checkbox(&colors);
            let check_style = CheckStyle::default_check(&colors);
            let radio_style = RadioStyle::default_radio(&colors);
            let toggle_style = ToggleStyle::default_toggle(&colors);

            Checkbox::new(&mut st.accepted, "Accept terms", cb_style, check_style).show(ui);
            Radio::new(
                &mut st.choice,
                Choice::First,
                "Option A",
                cb_style,
                radio_style,
            )
            .show(ui);
            Radio::new(
                &mut st.choice,
                Choice::Second,
                "Option B",
                cb_style,
                radio_style,
            )
            .show(ui);
            Toggle::new(&mut st.animations, toggle_style)
                .with_label("Enable animations", Color32::BLACK)
                .show(ui);
            Toggle::new(&mut st.unlabeled_toggle, toggle_style).show(ui);
        },
    );
    harness.step();

    harness.get_by_label("Accept terms").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "checkbox did not toggle",
        |h| h.state().accepted,
    );

    harness.get_by_label("Option B").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "radio did not select Option B",
        |h| h.state().choice == Choice::Second,
    );

    harness.get_by_label("Enable animations").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "labeled toggle did not flip",
        |h| h.state().animations,
    );

    harness.get_by_label("Toggle").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "unlabeled toggle did not flip",
        |h| h.state().unlabeled_toggle,
    );
}

// =============================================================================
// tg-app custom-painted interactive widgets
// =============================================================================

#[test]
fn a11y_main_menu_rows_by_label() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // Open MainMenu via the accessible hamburger button in FiltersMenuWidget.
    let menu_label = tg_lang::tr::lng_main_menu();
    harness.get_by_label(&menu_label).click();
    wait_for_label_settled(&mut harness, "Settings");

    assert!(
        harness.query_by_label("Contacts").is_some(),
        "MainMenu 'Contacts' row must be discoverable via AccessKit"
    );
    assert!(
        harness.query_all_by_label("Saved Messages").count() >= 2,
        "Both Dialog row and MainMenu row for 'Saved Messages' must be discoverable via AccessKit"
    );
    assert!(
        harness.query_by_label("Night Mode").is_some(),
        "MainMenu 'Night Mode' toggle row must be discoverable via AccessKit"
    );

    harness.get_by_label("Settings").click();
    wait_for_label_settled(&mut harness, "Notifications and Sounds");
}

#[test]
fn a11y_dialogs_rows_by_chat_name_and_unread_count() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // `AppTestState::main_workspace()` opens `Rustgram Core Port` in a focused window, so
    // visible unread messages are marked read on settle (tdesktop `HistoryInner::paintEvent` ->
    // `readInboxTill`). Unopened unread chats keep their `<title> (<N> unread)` labels.
    assert!(
        harness
            .query_by_label("Rustgram Dev Log (3 unread)")
            .is_some(),
        "Dialog row with unread messages must expose '<title> (<N> unread)'"
    );
    assert!(
        harness.query_by_label("Mira Vance (1 unread)").is_some(),
        "Mira Vance dialog row must expose chat name + unread count"
    );
    assert!(
        harness.query_by_label("Rustgram Core Port").is_some(),
        "Opened active dialog row must clear unread count once visible messages are read"
    );
    assert!(
        harness.query_by_label("Saved Messages").is_some(),
        "Dialog row with 0 unread must expose chat name"
    );
    assert!(
        harness.query_by_label_contains("Archived").is_some(),
        "Archived chats row must be discoverable via AccessKit"
    );

    harness.get_by_label("Mira Vance (1 unread)").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking Mira Vance row did not switch active peer",
        |h| {
            !h.state().controller.is_animating()
                && h.state()
                    .controller
                    .session()
                    .is_some_and(|s| s.active_peer() == session_peers::USER_MIRA)
        },
    );
}

#[test]
fn a11y_filters_menu_buttons_and_horizontal_tabs_by_label() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // Vertical FiltersMenuWidget sidebar buttons.
    // `Rustgram Core Port` is opened and marked read on startup, leaving 3 unread chats in
    // All Chats (Mira Vance, Rustgram Dev Log, Nightly Builds) and 2 in Work.
    let menu_label = tg_lang::tr::lng_main_menu();
    let edit_label = tg_lang::tr::lng_filters_setup();
    assert!(harness.query_by_label(&menu_label).is_some());
    assert!(
        harness
            .query_by_label(&format!("{} (3 unread)", tg_lang::tr::lng_filters_all()))
            .is_some()
    );
    assert!(harness.query_by_label("Work (2 unread)").is_some());
    assert!(harness.query_by_label(&edit_label).is_some());

    // Click "Work (2 unread)" folder button by AccessKit label.
    harness.get_by_label("Work (2 unread)").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking Work folder did not switch active filter",
        |h| {
            h.state()
                .controller
                .session()
                .is_some_and(|s| s.active_filter() == FilterId(2))
        },
    );

    // Also verify `show_horizontal_folder_tabs` exposes folder labels.
    struct TabState {
        active: FilterId,
    }
    let mut tab_harness = build_widget_harness(
        TabState {
            active: FilterId::ALL,
        },
        Vec2::new(400.0, 64.0),
        |ui, st| {
            let pal = Palette::day();
            let folders = vec![
                ChatFolder {
                    id: FilterId::ALL,
                    title: "All Chats".to_owned(),
                    icon: FolderIconKind::All,
                    color_index: 0,
                    unread: FolderUnreadState::default(),
                    locked: false,
                },
                ChatFolder {
                    id: FilterId(1),
                    title: "Channels".to_owned(),
                    icon: FolderIconKind::Channels,
                    color_index: 1,
                    unread: FolderUnreadState {
                        unread_chats: 2,
                        muted_chats: 0,
                    },
                    locked: false,
                },
            ];
            if let Some(id) = show_horizontal_folder_tabs(ui, &folders, st.active, &pal) {
                st.active = id;
            }
        },
    );
    tab_harness.step();
    tab_harness.get_by_label("Channels (2 unread)").click();
    wait_until(
        &mut tab_harness,
        DEFAULT_MAX_FRAMES,
        "horizontal folder tab click did not select Channels",
        |h| h.state().active == FilterId(1),
    );
}

#[test]
fn a11y_history_header_and_compose_buttons_by_label() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // History top bar interactive buttons.
    assert!(
        harness
            .query_by_label("Chat info: Rustgram Core Port")
            .is_some(),
        "History header peer title button must be discoverable via AccessKit"
    );
    // 7.4.6 has no Mute button in the top bar; mute lives in the chat menu.
    assert!(harness.query_by_label("Chat menu").is_some());
    assert!(harness.query_by_label("Search in chat").is_some());
    assert!(harness.query_by_label("Toggle chat info").is_some());

    // Compose bar interactive buttons.
    assert!(harness.query_by_label("Attach").is_some());
    assert!(harness.query_by_label("Emoji, stickers and GIFs").is_some());
    assert!(harness.query_by_label("Record voice message").is_some());

    // Clicking "Search in chat" in history header opens in-chat search.
    harness.get_by_label("Search in chat").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking history header 'Search in chat' button via AccessKit must open chat search",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .is_chat_search_open()
        },
    );
}

#[test]
fn a11y_settings_rows_by_label() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // Open Settings via accessible hamburger + Settings row.
    let menu_label = tg_lang::tr::lng_main_menu();
    let back_label = tg_lang::tr::lng_go_back();
    harness.get_by_label(&menu_label).click();
    wait_for_label_settled(&mut harness, "Settings");
    harness.get_by_label("Settings").click();
    wait_for_label_settled(&mut harness, "Notifications and Sounds");

    // Main settings rows are discoverable and clickable by label. Settings opens as a layer at
    // this width, which (`Info::WrapWidget::willHaveBackButton`) has a close button and no back
    // button until a section is pushed.
    assert!(
        harness
            .query_all_by_label(&tg_lang::tr::lng_close())
            .next()
            .is_some()
    );
    assert!(harness.query_by_label(&back_label).is_none());
    harness.get_by_label("Notifications and Sounds").click();

    // Wait for slide transition and CheckBox rows inside Notifications settings.
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Settings toggle rows must register CheckBox WidgetInfo via AccessKit",
        |h| {
            !h.state().controller.is_animating()
                && h.query_all_by_role(egui::accesskit::Role::CheckBox).count() >= 1
        },
    );
    wait_for_label_settled(&mut harness, &back_label);
    harness.get_by_label(&back_label).click();
    wait_for_label_settled(&mut harness, "Privacy and Security");
}

#[test]
fn a11y_delete_confirm_button_in_compose_boxes_by_label() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    // Open Mira Vance via AccessKit dialog row.
    harness.get_by_label("Mira Vance (1 unread)").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "opening Mira Vance chat did not settle",
        |h| {
            !h.state().controller.is_animating()
                && h.state()
                    .controller
                    .session()
                    .is_some_and(|s| s.active_peer() == session_peers::USER_MIRA)
        },
    );
    settle_layout(&mut harness);

    let before = harness
        .state()
        .controller
        .mock_session()
        .load_slice_around(session_peers::USER_MIRA, MsgId::SHOW_AT_END, 50, 0)
        .messages
        .len();

    // Right-click a message bubble in Mira Vance's chat and click "Delete" via AccessKit!
    let msg_pos = Pos2::new(720.0, WORKSPACE_SIZE.y - 108.0);
    right_click_at(&mut harness, msg_pos);
    wait_for_menu_item(&mut harness, "Delete");

    harness.get_by_label("Delete").click();
    // tdesktop's DeleteMessagesBox has no title: the question is its first label.
    wait_for_label_settled(&mut harness, "Do you want to delete this message?");

    // Click the custom-painted red "Delete" confirm button directly by AccessKit label!
    let delete_btn = harness.get_by_label("Delete");
    let pos = delete_btn.rect().center();
    click_at(&mut harness, pos);

    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "clicking DeleteConfirmBox 'Delete' button via AccessKit must delete the message",
        |h| {
            h.state()
                .controller
                .mock_session()
                .load_slice_around(session_peers::USER_MIRA, MsgId::SHOW_AT_END, 50, 0)
                .messages
                .len()
                == before - 1
        },
    );
}
