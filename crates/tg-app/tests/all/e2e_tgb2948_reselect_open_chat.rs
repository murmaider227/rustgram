//! TGB-2948: the chat list and the history always show the same chat.
//!
//! tdesktop (`Dialogs::InnerWidget::chooseRow` -> `SessionController::showThread`) repaints the
//! list and the history together after a click. The old port painted the list first, with the
//! chat open before the click highlighted, and handled the click afterwards, so the frame of
//! the click showed the old row highlighted next to the new chat's history (on the test servers
//! a screenshot of that frame showed "Saved Messages" beside the highlighted bot row). A click
//! on the open chat's row keeps that chat open.

use egui::{Pos2, Vec2};
use tg_app::dialogs::backend::{DialogListQuery, DialogsBackend};
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::{FilterId, PeerId};

use crate::common::{AppTestState, build_harness, click_at, settle_layout, step_frames};

const WORKSPACE: Vec2 = Vec2::new(1280.0, 800.0);
/// `st::dialogsTopBarHeight`.
const DIALOGS_TOP_BAR_H: f32 = 54.0;
/// `st::defaultDialogRow.height`.
const ROW_H: f32 = 62.0;

type AppHarness = egui_kittest::Harness<'static, AppTestState>;

fn row_center(harness: &AppHarness, peer: PeerId) -> Pos2 {
    let slice = harness.state().controller.mock_session().dialog_slice(
        DialogListQuery::Filter(FilterId::ALL),
        0,
        40,
    );
    let idx = slice
        .entries
        .iter()
        .position(|d| d.peer_id == peer)
        .unwrap_or_else(|| panic!("{peer:?} is not in All Chats"));
    let geo = harness
        .state()
        .controller
        .session()
        .expect("session")
        .layout();
    // Visual row 0 is the Archive row.
    let visual = 1 + idx;
    Pos2::new(
        geo.filters_width as f32 + geo.dialogs_width as f32 * 0.5,
        DIALOGS_TOP_BAR_H + (visual as f32 + 0.5) * ROW_H,
    )
}

fn session_peer(harness: &AppHarness) -> PeerId {
    harness
        .state()
        .controller
        .session()
        .map_or(PeerId::ZERO, |s| s.active_peer())
}

fn history_peer(harness: &mut AppHarness) -> PeerId {
    harness
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .active_peer()
}

#[test]
fn clicking_the_open_chats_row_keeps_that_chat_open() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE);
    settle_layout(&mut harness);
    let open = session_peers::GROUP_CORE;
    assert_eq!(
        session_peer(&harness),
        open,
        "the demo opens the second row"
    );
    assert_eq!(history_peer(&mut harness), open);

    for click in 0..3 {
        let at = row_center(&harness, open);
        click_at(&mut harness, at);
        step_frames(&mut harness, 30);
        assert_eq!(
            session_peer(&harness),
            open,
            "click {click} on the open chat's row switched the session to another chat"
        );
        assert_eq!(
            history_peer(&mut harness),
            open,
            "click {click} on the open chat's row switched the history to another chat"
        );
    }
}

/// The most common colour of `peer`'s row in the last frame: its background.
fn row_background(harness: &mut AppHarness, peer: PeerId) -> [u8; 4] {
    let center = row_center(harness, peer);
    let image = harness.render().expect("render the frame");
    let mut counts = std::collections::HashMap::<[u8; 4], usize>::new();
    for dy in (-24..=24).step_by(3) {
        for dx in (-150..=150).step_by(3) {
            let x = (center.x + dx as f32) as u32;
            let y = (center.y + dy as f32) as u32;
            *counts.entry(image.get_pixel(x, y).0).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|&(_, n)| n)
        .map_or([0; 4], |(c, _)| c)
}

#[test]
fn the_frame_of_the_click_highlights_the_chat_it_opens() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE);
    settle_layout(&mut harness);
    let before = session_peers::GROUP_CORE;
    let target = session_peers::USER_MIRA;
    assert_eq!(session_peer(&harness), before);
    let open_bg = row_background(&mut harness, before);
    assert_ne!(open_bg, row_background(&mut harness, target));

    // `click_at` ends with the release frame: the frame that handles the click.
    let at = row_center(&harness, target);
    click_at(&mut harness, at);
    assert_eq!(session_peer(&harness), target);
    assert_eq!(history_peer(&mut harness), target);
    assert_eq!(
        row_background(&mut harness, target),
        open_bg,
        "the frame that opened the chat painted its row without the open-chat background"
    );
    assert_ne!(
        row_background(&mut harness, before),
        open_bg,
        "the frame that opened the chat still painted the old row as open"
    );
}
