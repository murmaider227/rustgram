//! Headless `egui_kittest` tests and visual inspection helpers for `SubsectionTabsWidget`.

use std::path::PathBuf;
use std::sync::Mutex;

use egui::{
    Event, Modifiers, MouseWheelSource, MouseWheelUnit, PointerButton, Pos2, TouchPhase, Vec2,
};
use egui_kittest::kittest::Queryable;
use image::GenericImageView;
use tg_app::history::mock::MockHistoryStore;
use tg_app::history::sections_model::{FORUM_TOPIC_ICON_COLORS, ForumTopic, SubsectionTabsMode};
use tg_app::history::sections_ui::{
    HistorySectionAction, HistorySectionKind, SUBSECTION_TABS_HORIZONTAL_HEIGHT,
    SUBSECTION_TABS_VERTICAL_WIDTH, SubsectionTabsWidget,
};
use tg_app::history::theme::HistoryColors;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::theme::Palette;
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_core::settings::{CoreSettings, PowerSavingFlags};

use crate::common::{
    AppTestState, build_harness, build_wgpu_harness, build_widget_harness, settle_layout,
    step_frames,
};

static WGPU_SUBTABS_LOCK: Mutex<()> = Mutex::new(());

fn build_forum_harness(size: Vec2) -> egui_kittest::Harness<'static, AppTestState> {
    let mut state = AppTestState::main_workspace();
    let static_settings = CoreSettings {
        loop_animated_stickers: false,
        power_saving: PowerSavingFlags::ALL,
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

fn sample_forum_topics(peer_id: PeerId) -> Vec<ForumTopic> {
    vec![
        ForumTopic {
            root_id: MsgId(1),
            peer_id,
            title: "General".to_owned(),
            icon_color: FORUM_TOPIC_ICON_COLORS[0].0,
            custom_emoji: None,
            is_general: true,
            is_pinned: true,
            is_closed: false,
            muted: false,
            unread_count: 2,
            unread_mentions: 1,
            last_sender: "Mira".to_owned(),
            last_preview: "Hello".to_owned(),
            last_date_unix: 1_795_341_600,
            draft_preview: None,
        },
        ForumTopic {
            root_id: MsgId(201),
            peer_id,
            title: "UI & Theme Architecture".to_owned(),
            icon_color: FORUM_TOPIC_ICON_COLORS[2].0,
            custom_emoji: Some("🎨".to_owned()),
            is_general: false,
            is_pinned: true,
            is_closed: false,
            muted: false,
            unread_count: 3,
            unread_mentions: 0,
            last_sender: "Mira".to_owned(),
            last_preview: "Palette".to_owned(),
            last_date_unix: 1_795_341_500,
            draft_preview: None,
        },
        ForumTopic {
            root_id: MsgId(202),
            peer_id,
            title: "MTProto & Transport".to_owned(),
            icon_color: FORUM_TOPIC_ICON_COLORS[0].0,
            custom_emoji: None,
            is_general: false,
            is_pinned: false,
            is_closed: false,
            muted: false,
            unread_count: 1,
            unread_mentions: 0,
            last_sender: "Theo".to_owned(),
            last_preview: "Keys".to_owned(),
            last_date_unix: 1_795_341_400,
            draft_preview: None,
        },
        ForumTopic {
            root_id: MsgId(203),
            peer_id,
            title: "Audio & Voice Pipeline".to_owned(),
            icon_color: FORUM_TOPIC_ICON_COLORS[3].0,
            custom_emoji: None,
            is_general: false,
            is_pinned: false,
            is_closed: false,
            muted: false,
            unread_count: 0,
            unread_mentions: 0,
            last_sender: "Riley".to_owned(),
            last_preview: "Opus".to_owned(),
            last_date_unix: 1_795_341_300,
            draft_preview: None,
        },
        ForumTopic {
            root_id: MsgId(205),
            peer_id,
            title: "Release 0.1 Archive".to_owned(),
            icon_color: FORUM_TOPIC_ICON_COLORS[1].0,
            custom_emoji: None,
            is_general: false,
            is_pinned: false,
            is_closed: true,
            muted: true,
            unread_count: 0,
            unread_mentions: 0,
            last_sender: "Taylor".to_owned(),
            last_preview: "Done".to_owned(),
            last_date_unix: 1_795_341_200,
            draft_preview: None,
        },
    ]
}

#[test]
fn subsection_tabs_bar_dimensions_match_tdesktop() {
    assert_eq!(
        SUBSECTION_TABS_HORIZONTAL_HEIGHT, 36.0,
        "horizontal subsection tab bar height must match tdesktop st::chatTabsSlider.height (36px)"
    );
    assert_eq!(
        SUBSECTION_TABS_VERTICAL_WIDTH, 64.0,
        "vertical subsection tab sidebar width must match tdesktop st::chatTabsVertical.width (64px)"
    );
}

#[test]
fn subsection_tabs_horizontal_wheel_and_drag_scroll_on_narrow_viewport() {
    struct State {
        tabs: SubsectionTabsWidget,
        backend: MockHistoryStore,
        topics: Vec<ForumTopic>,
        peer_id: PeerId,
        active_topic: Option<MsgId>,
        last_action: Option<HistorySectionAction>,
        outer_height: f32,
        remaining_height: f32,
    }

    let peer_id = PeerId::from_chat(9001);
    let state = State {
        tabs: SubsectionTabsWidget::new(),
        backend: MockHistoryStore::new(),
        topics: sample_forum_topics(peer_id),
        peer_id,
        active_topic: None,
        last_action: None,
        outer_height: 0.0,
        remaining_height: 0.0,
    };

    let mut harness = build_widget_harness(state, Vec2::new(390.0, 200.0), |ui, st| {
        let colors = HistoryColors::from_palette(&Palette::day());
        let outer = ui.max_rect();
        st.outer_height = outer.height();
        let (rem, act) = st.tabs.show_for_forum(
            ui,
            outer,
            st.peer_id,
            st.active_topic,
            &st.topics,
            &mut st.backend,
            &colors,
        );
        st.remaining_height = rem.height();
        if let Some(a) = act {
            if let HistorySectionAction::OpenSection(HistorySectionKind::ForumTopicHistory {
                topic_root_id,
                ..
            }) = a
            {
                st.active_topic = Some(topic_root_id);
            } else if matches!(a, HistorySectionAction::Back) {
                st.active_topic = None;
            }
            st.last_action = Some(a);
        }
    });

    harness.step();
    let consumed = harness.state().outer_height - harness.state().remaining_height;
    assert!(
        (consumed - 36.0).abs() < 0.5,
        "history content below horizontal bar must lose exactly 36px, lost {consumed}"
    );
    assert!(
        harness.state().tabs.max_scroll_offset() > 100.0,
        "5 forum topics at 390px width must overflow horizontally"
    );
    assert_eq!(harness.state().tabs.scroll_offset(), 0.0);

    // 1. Vertical mouse wheel over the tab strip translates into horizontal scroll.
    let bar_mid = Pos2::new(200.0, 18.0);
    harness
        .input_mut()
        .events
        .push(Event::PointerMoved(bar_mid));
    harness.step();
    harness.input_mut().events.push(Event::MouseWheel {
        unit: MouseWheelUnit::Point,
        delta: Vec2::new(0.0, -90.0),
        phase: TouchPhase::Move,
        source: MouseWheelSource::Unknown,
        modifiers: Modifiers::NONE,
    });
    harness.step();
    let after_wheel = harness.state().tabs.scroll_offset();
    assert!(
        after_wheel > 20.0,
        "mouse wheel over horizontal tab bar must scroll right, got {after_wheel}"
    );

    // 2. Dragging left scrolls further right and does not trigger a tab activation.
    harness.state_mut().last_action = None;
    harness.input_mut().events.push(Event::PointerButton {
        pos: Pos2::new(260.0, 18.0),
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    harness.step();
    for x in [240.0, 210.0, 170.0] {
        harness
            .input_mut()
            .events
            .push(Event::PointerMoved(Pos2::new(x, 18.0)));
        harness.step();
    }
    harness.input_mut().events.push(Event::PointerButton {
        pos: Pos2::new(170.0, 18.0),
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    harness.step();
    let after_drag = harness.state().tabs.scroll_offset();
    assert!(
        after_drag > after_wheel,
        "horizontal drag must increase scroll offset ({after_drag} <= {after_wheel})"
    );
    assert!(
        harness.state().last_action.is_none(),
        "drag-scrolling the bar must not accidentally activate a tab on release"
    );
}

#[test]
fn subsection_tabs_wide_and_narrow_interaction_and_layout_modes() {
    let mut harness = build_harness(AppTestState::main_workspace(), Vec2::new(1440.0, 900.0));
    settle_layout(&mut harness);

    assert!(
        harness
            .query_by_label("Subsection Tabs Layout (Tabs on Top)")
            .is_some(),
        "mode toggle button must expose accessible name at 1440x900"
    );
    assert!(harness.query_by_label("All Messages").is_some());
    assert!(harness.query_by_label("General").is_some());
    assert!(harness.query_by_label("UI & Theme Architecture").is_some());

    // Clicking the layout toggle cycles Top -> Left -> Bottom -> Top.
    harness
        .get_by_label("Subsection Tabs Layout (Tabs on Top)")
        .click();
    harness.step();
    harness.step();
    assert!(
        harness
            .query_by_label("Subsection Tabs Layout (Tabs on Left)")
            .is_some()
    );
    harness
        .get_by_label("Subsection Tabs Layout (Tabs on Left)")
        .click();
    harness.step();
    harness.step();
    assert!(
        harness
            .query_by_label("Subsection Tabs Layout (Tabs on Bottom)")
            .is_some()
    );
    harness
        .get_by_label("Subsection Tabs Layout (Tabs on Bottom)")
        .click();
    harness.step();
    harness.step();
    assert!(
        harness
            .query_by_label("Subsection Tabs Layout (Tabs on Top)")
            .is_some()
    );

    // Clicking a tab activates that forum topic.
    harness.get_by_label("General").click();
    harness.step();
    harness.step();
    assert!(
        harness.query_by_label("All Messages").is_some(),
        "subsection tabs bar remains visible inside topic history"
    );

    // Resize down to narrow 390x844 (OneColumn) and verify controls remain accessible.
    harness.set_size(Vec2::new(390.0, 844.0));
    settle_layout(&mut harness);
    assert!(
        harness
            .query_by_label("Subsection Tabs Layout (Tabs on Top)")
            .is_some()
    );
    assert!(harness.query_by_label("All Messages").is_some());
    assert!(harness.query_by_label("General").is_some());
    assert_eq!(SubsectionTabsMode::Top.next(), SubsectionTabsMode::Left);
}

/// Renders the forum chat at 1440x900 and 390x844 and saves fixed-region bar crops
/// to `RUSTGRAM_SNAPSHOT_DIR` (or temp dir) as `wp36_bar_{1440,narrow}_{before,after}.png`.
#[test]
#[ignore = "manual visual crop capture helper"]
fn capture_subsection_tabs_bar_crops() {
    let _guard = WGPU_SUBTABS_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let tag = std::env::var("RUSTGRAM_CAPTURE_TAG")
        .or_else(|_| std::env::var("TGFORK_CAPTURE_TAG"))
        .unwrap_or_else(|_| "after".to_owned());
    let snap_dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap"));
    std::fs::create_dir_all(&snap_dir).expect("create snap dir");

    for (label, size) in [
        ("1440", Vec2::new(1440.0, 900.0)),
        ("narrow", Vec2::new(390.0, 844.0)),
    ] {
        let mut harness = build_forum_harness(size);
        let geo = harness
            .state()
            .controller
            .session()
            .expect("session")
            .layout();
        let img = harness.render().expect("wgpu render");
        let (img_w, img_h) = img.dimensions();
        let chat_left = if geo.window_layout == tg_app::window::adaptive::WindowLayout::OneColumn {
            geo.filters_width.max(0) as u32
        } else {
            (geo.filters_width + geo.dialogs_width).max(0) as u32
        };
        let crop_w = img_w.saturating_sub(chat_left).max(1);
        let crop_y = 140u32.min(img_h.saturating_sub(1));
        let crop_h = 60u32.min(img_h.saturating_sub(crop_y)).max(1);
        let cropped = img.view(chat_left, crop_y, crop_w, crop_h).to_image();
        let out_path = snap_dir.join(format!("wp36_bar_{label}_{tag}.png"));
        cropped.save(&out_path).expect("save bar crop");
        eprintln!(
            "CAPTURED {} -> {} (full={}x{}, crop={}x{} at {},{})",
            label,
            out_path.display(),
            img_w,
            img_h,
            crop_w,
            crop_h,
            chat_left,
            crop_y
        );
    }
}
