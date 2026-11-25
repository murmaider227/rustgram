//! WP-78: profile details like Telegram Desktop 7.4.6 (`DetailsFiller::setupInfo`).

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tg_app_core::stand_in::theme::ThemeMode;

use crate::info::actions::ProfileActionsView;
use crate::info::backend::{InfoBackend, InfoPeerKind, PeerProfile};
use crate::info::colors::InfoColors;
use crate::info::mock::{MockBackend, mock_peers};

fn bare_user() -> PeerProfile {
    let mut peer = MockBackend::new()
        .peer_profile(mock_peers::USER_MIRA)
        .expect("mock user");
    peer.kind = InfoPeerKind::User;
    peer.phone = None;
    peer.usernames.clear();
    peer.bio.clear();
    peer
}

fn details_harness(peer: PeerProfile) -> Harness<'static, PeerProfile> {
    let colors = InfoColors::for_mode(ThemeMode::Day);
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(360.0, 600.0))
        .build_ui_state(
            move |ui, peer: &mut PeerProfile| {
                let _ = ProfileActionsView::show(ui, peer, true, &colors);
            },
            peer,
        );
    harness.run();
    harness
}

#[test]
fn user_without_phone_username_or_bio_shows_hidden_mobile_line() {
    let harness = details_harness(bare_user());
    assert!(
        harness.query_by_label("Hidden, Mobile").is_some(),
        "PhoneOrHiddenValue shows \"Hidden\" under the Mobile label"
    );
}

#[test]
fn hidden_line_is_dropped_once_the_user_has_a_username_or_bio() {
    let mut with_bio = bare_user();
    with_bio.bio = "Writes Rust".to_owned();
    let harness = details_harness(with_bio);
    assert!(harness.query_by_label("Hidden, Mobile").is_none());

    let mut with_phone = bare_user();
    with_phone.phone = Some("+999 661 0142".to_owned());
    let harness = details_harness(with_phone);
    assert!(harness.query_by_label("+999 661 0142, Mobile").is_some());
    assert!(harness.query_by_label("Hidden, Mobile").is_none());
}

#[test]
fn video_chat_button_follows_group_call_state() {
    use crate::info::backend::ProfileGroupCallState;
    use crate::info::cover::{CoverButton, cover_buttons};

    let mut group = MockBackend::new()
        .peer_profile(mock_peers::USER_MIRA)
        .expect("mock peer");
    group.kind = InfoPeerKind::Supergroup;
    group.can_edit = false;
    group.group_call = ProfileGroupCallState::default();
    assert!(
        !cover_buttons(&group, true)
            .iter()
            .any(|b| matches!(b, CoverButton::VideoChat { .. }))
    );

    group.group_call.can_manage = true;
    assert!(cover_buttons(&group, true).contains(&CoverButton::VideoChat { live_stream: false }));

    let mut channel = group.clone();
    channel.kind = InfoPeerKind::Channel;
    channel.group_call = ProfileGroupCallState {
        active: true,
        ..ProfileGroupCallState::default()
    };
    assert!(cover_buttons(&channel, true).contains(&CoverButton::VideoChat { live_stream: true }));

    // Users never get it from `canManageGroupCall` alone.
    let mut user = bare_user();
    user.group_call.can_manage = true;
    assert!(
        !cover_buttons(&user, true)
            .iter()
            .any(|b| matches!(b, CoverButton::VideoChat { .. }))
    );
}
