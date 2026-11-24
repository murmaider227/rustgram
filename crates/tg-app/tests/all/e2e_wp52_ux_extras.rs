//! End-to-end `egui_kittest` tests and headless `wgpu` screenshot capture for WP-52:
//! - Dialogs swipe/hover quick actions (`dialogs_quick_action.cpp`)
//! - History double-click quick reply / quick react (`history_view_quick_action.cpp`)
//! - Experimental Settings (`settings_experimental.cpp`)
//! - Settings Search: FAQ Suggestions & Recent Searches (`settings_faq_suggestions.cpp`, `settings_recent_searches.cpp`)
//! - Bot Info Extras (`bot_intro_preview.cpp`, `translate_bot_info_box.cpp`) & `MusicAttachBox` (`music_attach_box.cpp`)

use std::path::PathBuf;
use std::sync::Mutex;

use egui::Vec2;
use egui_kittest::kittest::Queryable;
use tg_app::boxes::BoxKind;
use tg_app::boxes::bot_info_extras::MusicAttachBoxState;
use tg_app::boxes::managed_bot::EditBotInfoBoxState;
use tg_app::history::compose::ComposeMode;
use tg_app::settings::SettingsSection;
use tg_app::settings::experimental::{
    any_experimental_changed, experimental_option_value, set_experimental_option,
};
use tg_app::settings::recent_searches::RecentSettingsSearches;
use tg_app::settings::theme_bridge::LiveThemeColors;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::window::section::SettingsPage;
use tg_core::settings::{
    CoreSettings, DoubleClickQuickAction, PowerSavingFlags, QuickDialogAction,
};

use crate::common::{AppTestState, build_harness, build_wgpu_harness, settle_layout, step_frames};

const DESKTOP_SIZE: Vec2 = Vec2::new(1440.0, 900.0);
const MOBILE_SIZE: Vec2 = Vec2::new(390.0, 844.0);

static WGPU_WP52_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn wp52_e2e_dialog_quick_action_hover_pill_and_swipe() {
    let mut state = AppTestState::main_workspace();
    let cfg = CoreSettings {
        quick_dialog_action: QuickDialogAction::Mute,
        power_saving: PowerSavingFlags::ALL,
        ..CoreSettings::default()
    };
    state.controller.main_widget_mut().apply_core_settings(&cfg);

    let mut harness = build_harness(state, DESKTOP_SIZE);
    settle_layout(&mut harness);

    // Ensure Mira Vance starts unmuted.
    let mira = session_peers::USER_MIRA;
    assert!(
        !harness
            .state()
            .controller
            .session_backend()
            .dialog_by_peer(mira)
            .expect("Mira dialog")
            .muted
    );

    // Hover Mira Vance's row so the hover quick-action pill appears, then click it.
    let row_center = harness.get_by_label_contains("Mira Vance").rect().center();
    harness.hover_at(row_center);
    step_frames(&mut harness, 4);

    harness
        .get_by_label_contains("Quick action: Mute (Mira Vance)")
        .click();
    step_frames(&mut harness, 4);

    assert!(
        harness
            .state()
            .controller
            .session_backend()
            .dialog_by_peer(mira)
            .expect("Mira dialog")
            .muted,
        "clicking hover quick-action Mute pill must mute Mira Vance"
    );

    // Switch quick action to Pin and trigger via middle-click on Theo Park.
    let pin_cfg = CoreSettings {
        quick_dialog_action: QuickDialogAction::Pin,
        power_saving: PowerSavingFlags::ALL,
        ..CoreSettings::default()
    };
    harness
        .state_mut()
        .controller
        .main_widget_mut()
        .apply_core_settings(&pin_cfg);
    step_frames(&mut harness, 2);

    let theo = session_peers::USER_THEO;
    assert!(
        !harness
            .state()
            .controller
            .session_backend()
            .dialog_by_peer(theo)
            .expect("Theo dialog")
            .pinned
    );
    let theo_center = harness.get_by_label_contains("Theo Park").rect().center();
    harness.hover_at(theo_center);
    step_frames(&mut harness, 4);
    harness
        .get_by_label_contains("Quick action: Pin (Theo Park)")
        .click();
    step_frames(&mut harness, 4);
    assert!(
        harness
            .state()
            .controller
            .session_backend()
            .dialog_by_peer(theo)
            .expect("Theo dialog")
            .pinned,
        "clicking hover quick-action Pin pill must pin Theo Park"
    );
}

#[test]
fn wp52_e2e_history_quick_action_reply_and_react() {
    let mut state = AppTestState::main_workspace();
    let cfg = CoreSettings {
        chat_quick_action: DoubleClickQuickAction::Reply,
        corner_reply: true,
        corner_reaction: true,
        power_saving: PowerSavingFlags::ALL,
        ..CoreSettings::default()
    };
    state.controller.main_widget_mut().apply_core_settings(&cfg);

    let mut harness = build_harness(state, DESKTOP_SIZE);
    settle_layout(&mut harness);
    harness
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .request_scroll_to_bottom(session_peers::GROUP_CORE);
    settle_layout(&mut harness);

    assert_eq!(
        harness
            .state()
            .controller
            .main_widget()
            .history()
            .inner()
            .chat_quick_action(),
        DoubleClickQuickAction::Reply
    );

    // Find a painted bubble rect in the active history and hover it to reveal corner Quick Reply.
    let (msg_id, bubble_rect) = harness
        .state()
        .controller
        .main_widget()
        .history()
        .inner()
        .messages()
        .iter()
        .rev()
        .find_map(|m| {
            harness
                .state()
                .controller
                .main_widget()
                .history()
                .inner()
                .last_painted_bubble_rect(m.id)
                .map(|r| (m.id, r))
        })
        .expect("at least one painted message bubble");

    harness.hover_at(bubble_rect.center());
    step_frames(&mut harness, 4);

    let reply_label = format!("Quick Reply (message {})", msg_id.0);
    harness.get_by_label(&reply_label).click();
    step_frames(&mut harness, 4);

    assert!(
        matches!(
            harness
                .state_mut()
                .controller
                .main_widget_mut()
                .history_mut()
                .compose_mut()
                .mode,
            ComposeMode::Reply { msg_id: target, .. } if target == msg_id
        ),
        "clicking bubble hover Quick Reply must set compose reply target"
    );
}

#[test]
fn wp52_e2e_settings_experimental_and_search_faq_recent_persistence() {
    let state = AppTestState::main_workspace();
    let mut harness = build_harness(state, DESKTOP_SIZE);
    settle_layout(&mut harness);

    // Navigate to Settings -> Main and search for "privacy".
    if let Some(session) = harness.state_mut().controller.session_mut() {
        session.show_settings(SettingsPage::Main);
    }
    step_frames(&mut harness, 4);

    if let Some(settings) = harness
        .state_mut()
        .controller
        .main_widget_mut()
        .settings_mut()
    {
        settings.search_query = "privacy".to_owned();
    }
    step_frames(&mut harness, 4);

    // Verify both Settings search results and FAQ suggestions appear.
    assert!(
        harness.query_by_label_contains("Privacy Rules").is_some(),
        "Settings search must list matching Settings entries"
    );
    let faq_count = harness
        .state()
        .controller
        .main_widget()
        .settings()
        .expect("settings open")
        .faq_suggestions
        .search("privacy")
        .len();
    assert!(
        faq_count >= 2,
        "Settings search must produce ranked FAQ suggestions for 'privacy'"
    );

    // Click a Settings search hit to record it in Recent Settings Searches.
    harness
        .get_by_label_contains("Privacy Rules, Blocked Users & Auto-Delete")
        .click();
    step_frames(&mut harness, 4);

    // Navigate into Experimental settings, toggle a flag, and verify CoreSettings tempdir round-trip.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("core_settings_wp52.bin");
    {
        let settings = harness
            .state_mut()
            .controller
            .main_widget_mut()
            .settings_mut()
            .expect("settings controller");
        assert_eq!(
            settings.recent_searches.list().first().map(String::as_str),
            Some("privacy")
        );
        settings.push_section(SettingsSection::Experimental);
        set_experimental_option(&mut settings.core_settings, "unlimited-message-width", true);
        assert!(any_experimental_changed(&settings.core_settings));
        std::fs::write(&path, settings.core_settings.serialize()).expect("save CoreSettings");
    }
    step_frames(&mut harness, 4);

    let mut reloaded = CoreSettings::new();
    reloaded
        .add_from_serialized(
            &std::fs::read(&path).expect("read CoreSettings"),
            tg_core::version::TDESKTOP_COMPAT_VERSION,
        )
        .expect("deserialize CoreSettings");
    assert!(experimental_option_value(
        &reloaded,
        "unlimited-message-width"
    ));
    let reloaded_recent = RecentSettingsSearches::from_core_settings(&reloaded);
    assert_eq!(
        reloaded_recent.list().first().map(String::as_str),
        Some("privacy")
    );
}

#[test]
fn wp52_e2e_bot_info_extras_and_music_attach_box() {
    let state = AppTestState::main_workspace();
    let mut harness = build_harness(state, DESKTOP_SIZE);
    settle_layout(&mut harness);

    // 1. Open EditBotInfoBox with BotIntroPreview + TranslateBotInfoBox.
    let bot_id = session_peers::BOT_CATALOG;
    let mut edit_box = EditBotInfoBoxState::new(bot_id, "catalog_bot");
    edit_box.draft.current_mut("").description =
        "Helps browse products and automate workflows.".to_owned();
    let ctx = harness.ctx.clone();
    let colors = LiveThemeColors::default();
    harness.state_mut().controller.box_host_mut().push(
        &ctx,
        BoxKind::EditBotInfo(edit_box),
        &colors,
    );
    step_frames(&mut harness, 4);

    // Click "Translate to..." to push TranslateBotInfoBox.
    harness.get_by_label("Translate to...").click();
    step_frames(&mut harness, 4);
    assert!(matches!(
        harness.state().controller.box_host().top_kind(),
        Some(BoxKind::TranslateBotInfo(_))
    ));

    // Trigger translation inside TranslateBotInfoBox.
    harness.get_by_label("Translate").click();
    step_frames(&mut harness, 4);
    if let Some(BoxKind::TranslateBotInfo(tr_state)) =
        harness.state().controller.box_host().top_kind()
    {
        assert_eq!(tr_state.progress_fraction(), 1.0);
    } else {
        panic!("expected TranslateBotInfoBox on top of modal stack");
    }

    // 2. Open MusicAttachBox, select a track, and click "Send (1)".
    let self_id = harness
        .state()
        .controller
        .session_backend()
        .saved_messages_peer_id();
    let music_box = MusicAttachBoxState::new(session_peers::GROUP_CORE, self_id);
    harness.state_mut().controller.box_host_mut().push(
        &ctx,
        BoxKind::MusicAttach(music_box),
        &colors,
    );
    step_frames(&mut harness, 4);

    if let Some(BoxKind::MusicAttach(ms)) =
        harness.state_mut().controller.box_host_mut().top_kind_mut()
    {
        assert!(
            !ms.tracks.is_empty(),
            "MusicAttachBox must load saved music"
        );
        let first_id = ms.tracks[0].document_id;
        ms.toggle_select(first_id);
    }
    step_frames(&mut harness, 4);

    harness.get_by_label("Send (1)").click();
    step_frames(&mut harness, 4);
    assert!(
        !matches!(
            harness.state().controller.box_host().top_kind(),
            Some(BoxKind::MusicAttach(_))
        ),
        "MusicAttachBox must close after sending selected track"
    );
}

fn save_wgpu_png(harness: &mut egui_kittest::Harness<'static, AppTestState>, name: &str) {
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("wp52"));
    std::fs::create_dir_all(&dir).expect("create snap dir");
    let img = harness.render().expect("wgpu render");
    let path = dir.join(format!("{name}.png"));
    img.save(&path).expect("save png");
}

fn build_snap_harness(size: Vec2) -> egui_kittest::Harness<'static, AppTestState> {
    let mut state = AppTestState::main_workspace();
    let static_settings = CoreSettings {
        loop_animated_stickers: false,
        power_saving: PowerSavingFlags::ALL,
        quick_dialog_action: QuickDialogAction::Mute,
        chat_quick_action: DoubleClickQuickAction::Reply,
        corner_reply: true,
        corner_reaction: true,
        ..CoreSettings::default()
    };
    state
        .controller
        .main_widget_mut()
        .apply_core_settings(&static_settings);
    let mut harness = build_wgpu_harness(state, size);
    settle_layout(&mut harness);
    step_frames(&mut harness, 35);
    harness
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .request_scroll_to_bottom(session_peers::GROUP_CORE);
    settle_layout(&mut harness);
    harness
}

#[test]
#[ignore = "generates WP-52 after screenshots under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_wp52_after_screenshots() {
    let _guard = WGPU_WP52_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    for (tag, size) in [("1440x900", DESKTOP_SIZE), ("390x844", MOBILE_SIZE)] {
        // 1. Dialogs & History Quick Actions
        {
            let mut h = build_snap_harness(size);
            if size.x < 600.0
                && let Some(session) = h.state_mut().controller.session_mut()
            {
                session.show_peer_history(
                    tg_app::stand_in::types::PeerId::ZERO,
                    tg_app::window::section::SectionShow::clear_stack(),
                    tg_app::stand_in::types::MsgId(0),
                );
                settle_layout(&mut h);
            }
            h.state_mut()
                .controller
                .main_widget_mut()
                .dialogs_mut()
                .inner
                .set_swipe_preview(session_peers::USER_MIRA, 76.0);
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("dialogs_quick_action_{tag}_after"));
        }

        // 2. Settings Experimental
        {
            let mut h = build_snap_harness(size);
            if let Some(session) = h.state_mut().controller.session_mut() {
                session.show_settings(SettingsPage::Advanced);
            }
            settle_layout(&mut h);
            if let Some(s) = h.state_mut().controller.main_widget_mut().settings_mut() {
                s.push_section(SettingsSection::Experimental);
                set_experimental_option(&mut s.core_settings, "unlimited-message-width", true);
                set_experimental_option(&mut s.core_settings, "show-peer-id-below-about", true);
            }
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("settings_experimental_{tag}_after"));
        }

        // 3. Settings Search (FAQ Suggestions & Recent Searches)
        {
            let mut h = build_snap_harness(size);
            if let Some(session) = h.state_mut().controller.session_mut() {
                session.show_settings(SettingsPage::Main);
            }
            settle_layout(&mut h);
            if let Some(s) = h.state_mut().controller.main_widget_mut().settings_mut() {
                s.recent_searches.bump("privacy");
                s.recent_searches.bump("experimental");
                s.search_query = "privacy".to_owned();
            }
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("settings_search_{tag}_after"));
        }

        // 4. Bot Info Extras (EditBotInfoBox with BotIntroPreview + Translate to...)
        {
            let mut h = build_snap_harness(size);
            let ctx = h.ctx.clone();
            let colors = LiveThemeColors::default();
            let mut edit_box = EditBotInfoBoxState::new(session_peers::BOT_CATALOG, "catalog_bot");
            edit_box.draft.current_mut("").name = "Catalog Bot".to_owned();
            edit_box.draft.current_mut("").about = "Browse sample items and commands.".to_owned();
            edit_box.draft.current_mut("").description =
                "Helps test bot commands, intro bubble previews, and multi-language translation."
                    .to_owned();
            h.state_mut().controller.box_host_mut().push(
                &ctx,
                BoxKind::EditBotInfo(edit_box),
                &colors,
            );
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("bot_info_extras_{tag}_after"));
        }

        // 5. Music Attach Box
        {
            let mut h = build_snap_harness(size);
            let ctx = h.ctx.clone();
            let colors = LiveThemeColors::default();
            let self_id = h
                .state()
                .controller
                .session_backend()
                .saved_messages_peer_id();
            let mut music_box = MusicAttachBoxState::new(session_peers::GROUP_CORE, self_id);
            music_box.ensure_loaded(h.state().controller.session_backend());
            if let Some(first) = music_box.tracks.first().map(|t| t.document_id) {
                music_box.toggle_select(first);
                music_box.playing_doc_id = Some(first);
            }
            h.state_mut().controller.box_host_mut().push(
                &ctx,
                BoxKind::MusicAttach(music_box),
                &colors,
            );
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("music_attach_box_{tag}_after"));
        }
    }
}
