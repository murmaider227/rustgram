//! End-to-end adaptive layout UI tests across `OneColumn`, `Normal`, and `ThreeColumn` thresholds.
//!
//! Verifies resizing the window across Telegram Desktop's exact column thresholds:
//! - Vertical folders sidebar width (`72px` when enabled and `width >= 452`)
//! - `OneColumn` (`body_width < 640`)
//! - `Normal` (`640 <= body_width < 932`, or `>= 932` when no third section is open)
//! - `ThreeColumn` (`body_width >= 932` with third section open)

use egui::Vec2;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::window::adaptive::{
    COLUMN_MIN_WIDTH_LEFT, COLUMN_MIN_WIDTH_MAIN, COLUMN_MIN_WIDTH_THIRD, WINDOW_FILTERS_WIDTH,
    WindowLayout,
};
use tg_app::window::section::SectionShow;

use crate::common::{AppTestState, build_harness, settle_layout};

#[test]
fn adaptive_layout_resizes_across_one_column_normal_and_three_column() {
    // Start wide (1360x820) with default vertical folders sidebar enabled (72px).
    let mut harness = build_harness(AppTestState::main_workspace(), Vec2::new(1360.0, 820.0));
    settle_layout(&mut harness);

    // Open peer info in wide layout -> enters ThreeColumn!
    harness
        .state_mut()
        .controller
        .session_mut()
        .expect("session")
        .toggle_peer_info(session_peers::GROUP_CORE);
    settle_layout(&mut harness);

    {
        let session = harness.state().controller.session().expect("session");
        let geo = session.layout();
        assert_eq!(
            geo.window_layout,
            WindowLayout::ThreeColumn,
            "1360px with PeerInfo enabled must use ThreeColumn"
        );
        assert!(session.third_section().is_some());
        assert_eq!(geo.filters_width, WINDOW_FILTERS_WIDTH);
        assert!(geo.dialogs_width >= COLUMN_MIN_WIDTH_LEFT);
        assert!(geo.chat_width >= COLUMN_MIN_WIDTH_MAIN);
        assert!(geo.third_width >= COLUMN_MIN_WIDTH_THIRD);
    }

    // Resize down to 860px (body = 860 - 72 = 788px, between 640 and 932) -> Normal (2 columns).
    harness.set_size(Vec2::new(860.0, 820.0));
    settle_layout(&mut harness);

    {
        let session = harness.state().controller.session().expect("session");
        let geo = session.layout();
        assert_eq!(
            geo.window_layout,
            WindowLayout::Normal,
            "860px (body 788px) must collapse from ThreeColumn to Normal"
        );
        assert_eq!(geo.filters_width, WINDOW_FILTERS_WIDTH);
        assert!(geo.dialogs_width >= COLUMN_MIN_WIDTH_LEFT);
        assert!(geo.chat_width >= COLUMN_MIN_WIDTH_MAIN);
        assert_eq!(geo.third_width, 0, "third column must hide in Normal mode");
    }

    // Resize further down to 520px (body = 520 - 72 = 448px < 640) -> OneColumn!
    harness.set_size(Vec2::new(520.0, 820.0));
    settle_layout(&mut harness);

    {
        let session = harness.state().controller.session().expect("session");
        let geo = session.layout();
        assert_eq!(
            geo.window_layout,
            WindowLayout::OneColumn,
            "520px must switch to OneColumn"
        );
        assert_eq!(geo.third_width, 0);
        // Active chat is open, so OneColumn displays the main column.
        assert!(session.is_main_section_shown());
    }

    // In OneColumn, navigating back returns to the dialogs list!
    harness
        .state_mut()
        .controller
        .session_mut()
        .expect("session")
        .show_back_from_stack(SectionShow::forward());
    settle_layout(&mut harness);
    {
        let session = harness.state().controller.session().expect("session");
        assert!(
            !session.is_main_section_shown(),
            "OneColumn back navigation should return to dialogs list"
        );
    }

    // Resize below 452px (e.g. 420px) -> vertical filters sidebar hides (`filters_width == 0`).
    harness.set_size(Vec2::new(420.0, 760.0));
    settle_layout(&mut harness);
    {
        let session = harness.state().controller.session().expect("session");
        let geo = session.layout();
        assert_eq!(geo.window_layout, WindowLayout::OneColumn);
        assert_eq!(
            geo.filters_width, 0,
            "vertical folders sidebar must collapse below 452px"
        );
    }

    // Resize back to wide (1360px) and re-open the chat -> ThreeColumn restores automatically
    // because `third_section_info_enabled` stayed true across the narrow resize.
    harness.set_size(Vec2::new(1360.0, 820.0));
    harness
        .state_mut()
        .controller
        .session_mut()
        .expect("session")
        .show_peer_history(
            session_peers::GROUP_CORE,
            SectionShow::clear_stack(),
            tg_app::stand_in::types::MsgId::SHOW_AT_END,
        );
    settle_layout(&mut harness);
    {
        let session = harness.state().controller.session().expect("session");
        assert_eq!(
            session.layout().window_layout,
            WindowLayout::ThreeColumn,
            "widening back to 1360px with an active chat should restore ThreeColumn"
        );
        assert!(session.third_section().is_some());
    }
}
