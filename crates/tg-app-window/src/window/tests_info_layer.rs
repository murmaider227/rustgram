//! TGB-3184: a click on the chat title opens the profile as a layer over the window
//! (`TopBarWidget::infoClicked` -> `showPeerInfo` -> `Info::LayerWidget`); only the info button
//! (`toggleInfoSection`) uses the third column, and only while the window is wide enough.

use tg_app_core::stand_in::settings::SavedWindowColumns;
use tg_app_core::stand_in::types::{MsgId, PeerId};

use crate::window::info_layer::{close_info_layer, info_layer_rect, info_shown_as_layer};
use crate::window::section::{InfoTab, SectionMemento, SectionShow};
use crate::window::session_controller::SessionController;

const PEER: PeerId = PeerId::from_channel(41);

fn chat_open(width: i32, third_enabled: bool) -> SessionController {
    let saved = SavedWindowColumns {
        third_section_info_enabled: third_enabled,
        ..SavedWindowColumns::default()
    };
    let mut ctrl = SessionController::new(saved, false);
    ctrl.update_layout(width);
    ctrl.show_peer_history(PEER, SectionShow::clear_stack(), MsgId::SHOW_AT_UNREAD);
    ctrl.update_layout(width);
    ctrl
}

fn profile() -> Option<SectionMemento> {
    Some(SectionMemento::PeerInfo {
        peer_id: PEER,
        tab: InfoTab::Overview,
    })
}

#[test]
fn title_click_opens_a_layer_even_in_a_wide_window() {
    let mut ctrl = chat_open(1440, false);
    ctrl.show_peer_info_layer(PEER);
    assert_eq!(ctrl.main_section().cloned(), profile());
    assert!(ctrl.third_section().is_none(), "never the third column");
    assert!(!ctrl.columns().third_section_info_enabled());
    assert!(info_shown_as_layer(&ctrl, 1440.0));
    // The chat stays the active chat under the layer; closing returns to it.
    assert_eq!(ctrl.active_peer(), PEER);
    close_info_layer(&mut ctrl);
    assert!(ctrl.main_section().is_none());
    assert_eq!(ctrl.active_peer(), PEER);
}

#[test]
fn title_click_with_the_third_column_on_still_opens_a_layer() {
    let mut ctrl = chat_open(1440, true);
    assert_eq!(ctrl.third_section().cloned(), profile());
    ctrl.show_peer_info_layer(PEER);
    assert_eq!(ctrl.main_section().cloned(), profile());
    assert!(info_shown_as_layer(&ctrl, 1440.0));
}

#[test]
fn narrow_window_shows_the_profile_as_a_section() {
    // `LayerWidget::MinimalSupportedWidth()` = 324 + 2 * 48.
    let mut ctrl = chat_open(400, false);
    ctrl.show_peer_info_layer(PEER);
    assert_eq!(ctrl.main_section().cloned(), profile());
    assert!(!info_shown_as_layer(&ctrl, 400.0));
    assert!(info_shown_as_layer(&ctrl, 420.0));
}

#[test]
fn info_button_toggles_the_third_column_and_remembers_it() {
    let mut ctrl = chat_open(1440, false);
    ctrl.toggle_peer_info(PEER);
    assert_eq!(ctrl.third_section().cloned(), profile());
    assert!(ctrl.main_section().is_none());
    assert!(ctrl.columns().third_section_info_enabled());
    ctrl.toggle_peer_info(PEER);
    assert!(ctrl.third_section().is_none());
    assert!(!ctrl.columns().third_section_info_enabled());
}

#[test]
fn info_button_without_room_for_a_third_column_opens_the_layer() {
    let mut ctrl = chat_open(900, false);
    ctrl.toggle_peer_info(PEER);
    assert!(ctrl.third_section().is_none());
    assert!(!ctrl.columns().third_section_info_enabled());
    assert_eq!(ctrl.main_section().cloned(), profile());
}

#[test]
fn layer_geometry_follows_count_geometry() {
    let body = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1440.0, 900.0));
    let rect = info_layer_rect(body);
    // `infoDesiredWidth` 392, top `900 / 24` = 37, the same margin at the bottom.
    assert_eq!(rect.width(), 392.0);
    assert_eq!(rect.top(), 37.0);
    assert_eq!(rect.bottom(), 900.0 - 37.0);
    assert_eq!(rect.center().x, 720.0);
}
