//! End-to-end `egui_kittest` tests and before/after screenshot capture for WP-76 (audit #8):
//! 1. tdesktop vector icons on the shared `tg-ui` popup menu (`MenuItem::with_style_icon`) and on
//!    the chat-list / profile menus.
//! 2. Chat-list "Open in new window" opens a separate chat window (`showInNewWindow`).
//! 3. Profile cover buttons in tdesktop's order (Call outside the side column, Gift in it),
//!    Call starting an outgoing call and Gift opening Star Gifts.
//! 4. Emoji search-row mood groups taken from the server list, with the built-in fallback.
//! 5. Media viewer "Save as" defaulting to the Downloads folder instead of `%TEMP%`.
//! 6. Notifications: Calls section, notification tone and "Delete all exceptions" on the
//!    per-type editor; Chat Settings "Choose from gallery" opening the background gallery box.
//! 7. `README.md` "Code signing policy" and "Privacy" links.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use egui::{Pos2, Vec2};
use egui_kittest::kittest::Queryable;
use tg_app::chat_helpers::emoji_list_widget::{
    EMOJI_MOOD_GROUPS, EmojiListWidget, MoodGroup, default_mood_groups,
};
use tg_app::chat_helpers::mock::MockChatHelpersBackend;
use tg_app::dialogs::backend::{DialogListQuery, DialogsBackend};
use tg_app::dialogs::inner::dialog_context_menu_items;
use tg_app::info::backend::InfoBackend;
use tg_app::info::cover::{CoverButton, cover_buttons};
use tg_app::info::mock::{MockBackend, mock_peers};
use tg_app::media::overlay::{MediaOverlayWidget, default_save_directory};
use tg_app::settings::chat::{ChatSettingsState, render_chat_settings_section};
use tg_app::settings::mock::MockSettingsBackend;
use tg_app::settings::notifications::{NotificationsState, render_notifications_section};
use tg_app::settings::theme_bridge::LiveThemeColors;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::FilterId;
use tg_app::themes::{AppThemesHost, ThemesTab};
use tg_app::window::section::{SectionShow, SettingsPage};
use tg_app_core::settings::backend::{NotifyScope, SettingsBackend};
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::theme::ThemeMode;
use tg_core::{CoreSettings, GlobalConfig};
use tg_emoji::EmojiTextureCache;
use tg_lang::tr;
use tg_ui::style::{PopupMenuStyle, ToggleStyle};
use tg_ui::tg_style::st;
use tg_ui::theme::DayPalette;
use tg_ui::widgets::menu::{MenuItem, PopupMenuState};

use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, build_harness, build_wgpu_harness, build_widget_harness,
    right_click_at, settle_layout, step_frames, wait_for_menu_item, wait_until,
};

static WGPU_WP76_LOCK: Mutex<()> = Mutex::new(());

const WORKSPACE_SIZE: Vec2 = Vec2::new(1280.0, 800.0);
const DIALOGS_TOP_BAR_H: f32 = 54.0;
const ROW_H: f32 = 62.0;

fn dialog_row_center(
    harness: &egui_kittest::Harness<'_, AppTestState>,
    visual_row_index: usize,
) -> Pos2 {
    let geo = harness
        .state()
        .controller
        .session()
        .expect("session")
        .layout();
    let x = geo.filters_width as f32 + geo.dialogs_width as f32 * 0.5;
    let y = DIALOGS_TOP_BAR_H + (visual_row_index as f32 + 0.5) * ROW_H;
    Pos2::new(x, y)
}

fn open_dialog_menu(harness: &mut egui_kittest::Harness<'static, AppTestState>) {
    let slice = harness.state().controller.mock_session().dialog_slice(
        DialogListQuery::Filter(FilterId::ALL),
        0,
        20,
    );
    let mira_idx = slice
        .entries
        .iter()
        .position(|d| d.peer_id == session_peers::USER_MIRA)
        .expect("Mira in All Chats");
    let pos = dialog_row_center(harness, 1 + mira_idx);
    right_click_at(harness, pos);
    wait_until(
        harness,
        DEFAULT_MAX_FRAMES,
        "right-clicking a dialog row must open its context menu",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .dialogs_mut()
                .inner
                .is_context_menu_open()
        },
    );
}

#[test]
fn wp76_menu_item_style_icons_render_and_trigger() {
    let action = MenuItem::action("mute", "Mute").with_style_icon(st::menu_icon_mute);
    assert_eq!(action.style_icon(), Some(st::menu_icon_mute));
    let toggle = MenuItem::Toggle {
        id: "t".to_owned(),
        label: "Toggle".to_owned(),
        checked: false,
        icon: None,
    }
    .with_style_icon(st::menu_icon_mute);
    assert_eq!(
        toggle.style_icon(),
        None,
        "style icons apply to actions only"
    );

    struct MenuState {
        menu: PopupMenuState,
        opened: bool,
        triggered: Option<String>,
    }
    let colors = DayPalette::new();
    let style = PopupMenuStyle::from_tg(&st::popup_menu_with_icons, &colors);
    let toggle_style = ToggleStyle::default_toggle(&colors);
    let items = vec![
        MenuItem::action("new_window", tr::lng_context_new_window())
            .with_style_icon(st::menu_icon_new_window),
        MenuItem::Separator,
        MenuItem::action("block", tr::lng_profile_block_user())
            .with_style_icon(st::menu_icon_block),
    ];
    let mut harness = build_widget_harness(
        MenuState {
            menu: PopupMenuState::new(),
            opened: false,
            triggered: None,
        },
        Vec2::new(480.0, 360.0),
        move |ui, st_| {
            if !st_.opened {
                st_.menu.open_at(ui.ctx(), Pos2::new(40.0, 40.0), &style);
                st_.opened = true;
            }
            if let Some(id) = st_.menu.show(
                ui.ctx(),
                egui::Id::unique("wp76_menu"),
                &items,
                &style,
                &toggle_style,
            ) {
                st_.triggered = Some(id);
            }
        },
    );
    step_frames(&mut harness, 20);
    harness.get_by_label(&tr::lng_profile_block_user()).click();
    step_frames(&mut harness, 3);
    assert_eq!(harness.state().triggered.as_deref(), Some("block"));
}

#[test]
fn wp76_dialog_menu_keeps_tdesktop_icons_and_opens_new_window() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);

    let items = dialog_context_menu_items(
        session_peers::USER_MIRA,
        harness.state().controller.mock_session(),
        DialogListQuery::Filter(FilterId::ALL),
    );
    let new_window = items
        .iter()
        .find(|m| matches!(m, MenuItem::Action { id, .. } if id == "new_window"))
        .expect("Open in new window row");
    assert_eq!(new_window.style_icon(), Some(st::menu_icon_new_window));
    assert!(
        items
            .iter()
            .filter(|m| {
                matches!(m, MenuItem::Action { id, .. } if !id.starts_with("folder_toggle:"))
            })
            .all(|m| m.style_icon().is_some()),
        "every chat-list menu action (folder rows only show a check) carries its tdesktop icon"
    );

    open_dialog_menu(&mut harness);
    let label = tr::lng_context_new_window();
    wait_for_menu_item(&mut harness, &label);
    harness.get_by_label(&label).click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Open in new window must open a separate chat window for Mira",
        |h| {
            h.state_mut()
                .controller
                .shell_extras_mut()
                .saved_windows
                .open_windows()
                .iter()
                .any(|w| w.peer_id == session_peers::USER_MIRA.0)
        },
    );
}

#[test]
fn wp76_profile_cover_buttons_follow_tdesktop() {
    let backend = MockBackend::new();
    let user = backend
        .peer_profile(mock_peers::USER_MIRA)
        .expect("user profile");
    assert_eq!(
        cover_buttons(&user, false),
        vec![CoverButton::Message, CoverButton::Mute, CoverButton::Call],
        "full profile: Message, Mute, Call (max three)"
    );
    assert_eq!(
        cover_buttons(&user, true),
        vec![CoverButton::Message, CoverButton::Mute, CoverButton::Gift],
        "side column never shows Call (`!isSide`)"
    );
    let bot = backend
        .peer_profile(mock_peers::BOT_BUILD)
        .expect("bot profile");
    assert!(
        !cover_buttons(&bot, false).contains(&CoverButton::Call),
        "bots cannot be called"
    );
}

#[test]
fn wp76_emoji_mood_groups_from_server_list() {
    struct EmojiState {
        widget: EmojiListWidget,
        backend: MockChatHelpersBackend,
        cache: EmojiTextureCache,
        palette: Palette,
    }
    let mut widget = EmojiListWidget::new();
    assert_eq!(widget.mood_groups(), default_mood_groups().as_slice());
    assert_eq!(widget.mood_groups().len(), EMOJI_MOOD_GROUPS.len());
    widget.set_mood_groups(vec![
        MoodGroup::new("Sunny", vec!["☀️".to_owned(), "🌞".to_owned()]),
        MoodGroup::new("Party", vec!["🎉".to_owned(), "🥳".to_owned()]),
    ]);
    assert_eq!(widget.mood_groups().len(), 2);
    assert_eq!(widget.mood_groups()[0].icon, "☀️");

    let mut harness = build_widget_harness(
        EmojiState {
            widget,
            backend: MockChatHelpersBackend::new(),
            cache: EmojiTextureCache::new(),
            palette: Palette::for_mode(ThemeMode::Day),
        },
        Vec2::new(420.0, 420.0),
        |ui, s| {
            let _ = s.widget.show(ui, &mut s.backend, &mut s.cache, &s.palette);
        },
    );
    step_frames(&mut harness, 5);
    assert!(harness.query_by_label("Sunny").is_some());
    assert!(harness.query_by_label("Party").is_some());
    assert!(
        harness.query_by_label(EMOJI_MOOD_GROUPS[0].title).is_none(),
        "built-in groups are replaced by the server list"
    );
    harness.get_by_label("Party").click();
    step_frames(&mut harness, 3);
    assert_eq!(harness.state().widget.active_group(), Some(1));

    // An empty server list restores the built-in fallback.
    harness.state_mut().widget.set_mood_groups(Vec::new());
    assert_eq!(
        harness.state().widget.mood_groups().len(),
        EMOJI_MOOD_GROUPS.len()
    );
    assert_eq!(harness.state().widget.active_group(), None);
}

#[test]
fn wp76_media_viewer_saves_to_downloads_by_default() {
    let overlay = MediaOverlayWidget::new();
    assert_eq!(overlay.save_directory(), default_save_directory().as_path());
    assert_ne!(
        overlay.save_directory(),
        std::env::temp_dir().join("rustgram_downloads").as_path(),
        "the viewer must not default to %TEMP%"
    );
    if let Some(home) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        assert_eq!(
            overlay.save_directory(),
            PathBuf::from(home)
                .join("Downloads")
                .join("Rustgram")
                .as_path()
        );
    }
}

#[test]
fn wp76_notifications_calls_section_and_per_type_tone() {
    struct NotifState {
        state: NotificationsState,
        core: CoreSettings,
        backend: MockSettingsBackend,
        colors: DayPalette,
    }
    let mut backend = MockSettingsBackend::new();
    let mut private = backend.scope_notifications(NotifyScope::PrivateChats);
    if private.exceptions.is_empty() {
        let contact = backend
            .contacts()
            .into_iter()
            .find(|c| !c.is_group_or_channel)
            .expect("mock contact");
        private
            .exceptions
            .push(tg_app_core::settings::backend::NotifyException {
                peer: contact,
                muted: true,
                sound: "None".to_owned(),
            });
        backend.set_scope_notifications(NotifyScope::PrivateChats, private);
    }
    let mut harness = build_widget_harness(
        NotifState {
            state: NotificationsState::default(),
            core: CoreSettings::new(),
            backend,
            colors: DayPalette::new(),
        },
        Vec2::new(420.0, 2400.0),
        |ui, s| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let _ = render_notifications_section(
                    ui,
                    &mut s.state,
                    &mut s.core,
                    &mut s.backend,
                    &s.colors,
                );
            });
        },
    );
    step_frames(&mut harness, 5);
    assert!(
        harness
            .query_by_label(&tr::lng_settings_notifications_calls_title())
            .is_some()
    );
    let accept = tr::lng_settings_call_accept_calls();
    assert!(harness.query_by_label_contains(&accept).is_some());
    assert!(
        harness
            .query_by_label_contains(&tr::lng_notification_tone())
            .is_none(),
        "the tone lives on the per-type editor, not the main page"
    );

    harness
        .query_by_label_contains(&accept)
        .expect("row")
        .click();
    step_frames(&mut harness, 3);
    let current = harness
        .state()
        .backend
        .active_sessions()
        .into_iter()
        .find(|s| s.is_current())
        .expect("current session");
    assert!(
        !current.accept_calls,
        "toggle turns off calls on this device"
    );

    harness.state_mut().state.editing_scope = Some(NotifyScope::PrivateChats);
    step_frames(&mut harness, 3);
    assert!(
        harness
            .query_by_label_contains(&tr::lng_notification_tone())
            .is_some()
    );
    let clear = tr::lng_notification_exceptions_clear();
    harness.get_by_label(&clear).click();
    step_frames(&mut harness, 3);
    assert!(
        harness
            .state()
            .backend
            .scope_notifications(NotifyScope::PrivateChats)
            .exceptions
            .is_empty()
    );
}

#[test]
fn wp76_choose_from_gallery_opens_background_box() {
    struct ChatState {
        state: ChatSettingsState,
        core: CoreSettings,
        global: GlobalConfig,
        live: LiveThemeColors,
        backend: MockSettingsBackend,
        colors: DayPalette,
        opened: bool,
    }
    let mut harness = build_widget_harness(
        ChatState {
            state: ChatSettingsState::default(),
            core: CoreSettings::new(),
            global: GlobalConfig::default(),
            live: LiveThemeColors::default(),
            backend: MockSettingsBackend::new(),
            colors: DayPalette::new(),
            opened: false,
        },
        Vec2::new(420.0, 2400.0),
        |ui, s| {
            let out = render_chat_settings_section(
                ui,
                &mut s.state,
                &mut s.core,
                &mut s.global,
                &mut s.live,
                &mut s.backend,
                &s.colors,
            );
            s.opened |= out.open_theme_studio;
        },
    );
    step_frames(&mut harness, 5);
    assert!(!AppThemesHost::is_open(&harness.ctx));
    harness
        .get_by_label(&tr::lng_settings_bg_from_gallery())
        .click();
    step_frames(&mut harness, 3);
    assert!(harness.state().opened);
    assert!(AppThemesHost::is_open(&harness.ctx));
    assert_eq!(
        AppThemesHost::active_tab(&harness.ctx),
        ThemesTab::Backgrounds
    );
}

#[test]
fn wp76_readme_links_code_signing_policy_and_privacy() {
    let readme_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md");
    let readme = std::fs::read_to_string(readme_path).expect("README.md");
    assert!(readme.contains("[Code signing policy](#code-signing-policy)"));
    assert!(readme.contains("## Code signing policy"));
    assert!(readme.contains("[Privacy](docs/PRIVACY.md)"));
    assert!(readme.contains("## Privacy"));
    for forbidden in ["D:/", "D:\\", "C:/Users", "C:\\Users"] {
        assert!(
            !readme.contains(forbidden),
            "README must not contain {forbidden}"
        );
    }
}

fn save_png(harness: &mut egui_kittest::Harness<'static, AppTestState>, path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create snap dir");
    }
    let img = harness.render().expect("wgpu render");
    img.save(path).expect("save png");
}

/// Captures the chat-list context menu and the profile with the APIs present both before and
/// after WP-76, so the same test produces the "before" shots on master.
#[test]
#[ignore = "captures before/after PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_wp76_snapshots() {
    let _guard = WGPU_WP76_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stage = std::env::var("RUSTGRAM_WP76_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned());
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("wp76"));

    for (size_label, size) in [
        ("1440x900", Vec2::new(1440.0, 900.0)),
        ("390x844", Vec2::new(390.0, 844.0)),
    ] {
        // Chat-list context menu (icons, Open in new window).
        {
            let mut harness = build_wgpu_harness(AppTestState::main_workspace(), size);
            settle_layout(&mut harness);
            if size.x >= 1000.0 {
                open_dialog_menu(&mut harness);
                step_frames(&mut harness, 30);
                save_png(
                    &mut harness,
                    &dir.join(format!("{stage}_dialog_menu_{size_label}.png")),
                );
            }
        }
        // Profile of a user (cover buttons, ⋮ menu icons).
        {
            let mut harness = build_wgpu_harness(AppTestState::main_workspace(), size);
            settle_layout(&mut harness);
            if let Some(session) = harness.state_mut().controller.session_mut() {
                session.show_peer_info(session_peers::USER_MIRA, SectionShow::forward());
            }
            step_frames(&mut harness, 40);
            save_png(
                &mut harness,
                &dir.join(format!("{stage}_profile_{size_label}.png")),
            );
            // Profile ⋮ menu (tdesktop vector icons).
            let more = tg_app::info::strings::TOOLTIP_MORE_ACTIONS;
            if let Some(node) = harness.query_all_by_label(more).last() {
                node.click();
                step_frames(&mut harness, 30);
                save_png(
                    &mut harness,
                    &dir.join(format!("{stage}_profile_menu_{size_label}.png")),
                );
            }
        }
        // Settings > Notifications and Sounds: top, then scrolled to Events / Calls.
        {
            let mut harness = build_wgpu_harness(AppTestState::main_workspace(), size);
            settle_layout(&mut harness);
            if let Some(session) = harness.state_mut().controller.session_mut() {
                session.show_settings(SettingsPage::Notifications);
            }
            settle_layout(&mut harness);
            step_frames(&mut harness, 10);
            save_png(
                &mut harness,
                &dir.join(format!("{stage}_notifications_{size_label}.png")),
            );
            let center = Pos2::new(size.x * 0.5, size.y * 0.6);
            for _ in 0..12 {
                let events = &mut harness.input_mut().events;
                events.push(egui::Event::PointerMoved(center));
                events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: Vec2::new(0.0, -60.0),
                    modifiers: egui::Modifiers::NONE,
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Unknown,
                });
                harness.step();
            }
            step_frames(&mut harness, 20);
            save_png(
                &mut harness,
                &dir.join(format!("{stage}_notifications_calls_{size_label}.png")),
            );
        }
    }
}
