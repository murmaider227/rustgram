//! End-to-end UI tests for Peer Profile (wide vs narrow), Shared Media, and Media Viewer.
//!
//! Covers:
//! - Open profile from the chat header: a layer over the window at any width from 420 px
//!   (`TopBarWidget::infoClicked` -> `Info::LayerWidget`, TGB-3184), never the third column
//! - Open shared media and switch tabs
//! - Open the fullscreen media viewer, navigate gallery with ArrowRight/ArrowLeft, close with Esc

use egui::{Key, Modifiers, Pos2, Vec2};
use tg_app::history::HISTORY_TOP_BAR_HEIGHT;
use tg_app::info::{InfoBackend, SharedMediaTab};
use tg_app::stand_in::mock_session::session_peers;
use tg_app::window::adaptive::WindowLayout;
use tg_app::window::section::SectionMemento;

use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, build_harness, click_at, press_key, settle_layout, wait_until,
};

/// Computes a clickable point inside the chat header title region (triggers `TopBarEvent::ShowInfo`).
fn chat_header_click_pos(harness: &egui_kittest::Harness<'_, AppTestState>) -> Pos2 {
    let geo = harness
        .state()
        .controller
        .session()
        .expect("session")
        .layout();
    let main_left = (geo.filters_width + geo.dialogs_width) as f32;
    // Click around 120px into the main top bar (safely over peer avatar/title, away from right buttons).
    Pos2::new(main_left + 120.0, HISTORY_TOP_BAR_HEIGHT * 0.5)
}

#[test]
fn profile_opens_as_a_layer_from_the_chat_header_at_any_width() {
    // 1. Wide layout (1440x900): the header click opens the profile as a layer over the chat
    // (`showPeerInfo` without `thirdColumn`), not in the third column (TGB-3184).
    let mut wide = build_harness(AppTestState::main_workspace(), Vec2::new(1440.0, 900.0));
    settle_layout(&mut wide);

    let header_pos = chat_header_click_pos(&wide);
    click_at(&mut wide, header_pos);
    wait_until(
        &mut wide,
        DEFAULT_MAX_FRAMES,
        "the header click in a 1440px window must open the profile layer",
        |h| {
            !h.state().controller.is_animating()
                && h.state().controller.session().is_some_and(|s| {
                    matches!(
                        s.main_section(),
                        Some(SectionMemento::PeerInfo { peer_id, .. })
                            if *peer_id == session_peers::GROUP_CORE
                    )
                })
        },
    );
    assert!(
        wide.state()
            .controller
            .session()
            .expect("session")
            .third_section()
            .is_none(),
        "the header click never opens the third column"
    );
    assert!(wide.state().controller.main_widget().info_shown_as_layer());

    // 2. Narrow layout (800x700 -> body 728px < 932px): the same layer.
    let mut narrow = build_harness(AppTestState::main_workspace(), Vec2::new(800.0, 700.0));
    settle_layout(&mut narrow);

    let narrow_header_pos = chat_header_click_pos(&narrow);
    click_at(&mut narrow, narrow_header_pos);
    wait_until(
        &mut narrow,
        DEFAULT_MAX_FRAMES,
        "narrow profile must push PeerInfo onto main_section stack",
        |h| {
            !h.state().controller.is_animating()
                && h.state().controller.session().is_some_and(|s| {
                    s.layout().window_layout == WindowLayout::Normal
                        && matches!(
                            s.main_section(),
                            Some(SectionMemento::PeerInfo { peer_id, .. })
                                if *peer_id == session_peers::GROUP_CORE
                        )
                })
        },
    );
    assert!(
        narrow
            .state()
            .controller
            .session()
            .expect("session")
            .third_section()
            .is_none(),
        "narrow layout must not open a third column"
    );
}

#[test]
fn profile_shared_media_and_media_viewer_arrows_and_escape() {
    let mut harness = build_harness(AppTestState::main_workspace(), Vec2::new(1440.0, 900.0));
    settle_layout(&mut harness);

    // Open profile in the third column by clicking the chat header.
    let header_pos = chat_header_click_pos(&harness);
    click_at(&mut harness, header_pos);
    settle_layout(&mut harness);

    // Verify GROUP_CORE has shared media photos & files in MockSession.
    let photos = harness.state().controller.mock_session().shared_media(
        session_peers::GROUP_CORE,
        SharedMediaTab::PhotosVideos,
        "",
    );
    let files = harness.state().controller.mock_session().shared_media(
        session_peers::GROUP_CORE,
        SharedMediaTab::Files,
        "",
    );
    assert!(
        photos.len() >= 2,
        "GROUP_CORE must have at least 2 shared photos for gallery navigation, found {}",
        photos.len()
    );
    assert!(!files.is_empty(), "GROUP_CORE must have shared files");

    // Open the first shared photo into the fullscreen MediaOverlayWidget.
    let first_photo_id = photos[0].id();
    harness.state_mut().controller.open_shared_media(
        session_peers::GROUP_CORE,
        first_photo_id,
        SharedMediaTab::PhotosVideos,
    );
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "opening shared photo must open fullscreen MediaOverlayWidget",
        |h| h.state().controller.media_overlay().is_open(),
    );

    let gallery_len = harness.state().controller.media_overlay().gallery().len();
    assert!(
        gallery_len >= 2,
        "media gallery must load sibling photos for arrow navigation, got {gallery_len}"
    );
    let start_idx = harness
        .state()
        .controller
        .media_overlay()
        .gallery()
        .current_index();

    // Navigate forward with ArrowRight.
    press_key(&mut harness, Modifiers::NONE, Key::ArrowRight);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "pressing ArrowRight in Media Viewer must advance to the next gallery item",
        |h| {
            h.state()
                .controller
                .media_overlay()
                .gallery()
                .current_index()
                == start_idx + 1
        },
    );

    // Navigate backward with ArrowLeft.
    press_key(&mut harness, Modifiers::NONE, Key::ArrowLeft);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "pressing ArrowLeft in Media Viewer must return to the previous gallery item",
        |h| {
            h.state()
                .controller
                .media_overlay()
                .gallery()
                .current_index()
                == start_idx
        },
    );

    // Close the media viewer with Escape.
    press_key(&mut harness, Modifiers::NONE, Key::Escape);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "pressing Escape must close MediaOverlayWidget",
        |h| !h.state().controller.media_overlay().is_open(),
    );
}
