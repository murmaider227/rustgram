//! E2E test verifying that clicking "Share Theme" inside `tg-app-info`'s `PeerQrBox`
//! emits `BoxEvent::OverlayRequest(DeepLinkOverlayRequest::OpenShareTheme { .. })` (a `tg-app-core`
//! event preserving the crate DAG) and opens `tg_app_settings::themes::ShareThemePickerState`
//! via `AppThemesHost::open_share_theme_for_peer`.

use egui::{Pos2, Rect, Vec2};
use egui_kittest::kittest::Queryable;

use tg_app::boxes::peer_qr::PeerQrBoxState;
use tg_app::boxes::{BoxEvent, BoxHost, BoxKind};
use tg_app::info::{InfoBackend, InfoColors};
use tg_app::stand_in::mock_session::{MockSession, session_peers};
use tg_app::stand_in::theme::ThemeMode;
use tg_app::themes::{AppThemesHost, ThemesTab};
use tg_app::window::deep_links::{DeepLinkOverlayRequest, dispatch_themes_deep_link_overlay};

use crate::common::{build_widget_harness, init_headless_env};

struct QrShareThemeState {
    host: BoxHost,
    session: MockSession,
    colors: InfoColors,
    events: Vec<BoxEvent>,
}

#[test]
fn e2e_profile_qr_share_theme_button_opens_share_theme_picker_through_core_event() {
    init_headless_env();
    tg_app::clock::init_test_clock();
    let session = MockSession::new(true);
    let peer = session
        .peer_profile(session_peers::USER_MIRA)
        .expect("USER_MIRA profile");
    let expected_peer_id = peer.id;
    let expected_title = peer.title.clone();
    let qr_state = PeerQrBoxState::from_profile(&peer);

    let state = QrShareThemeState {
        host: BoxHost::new(),
        session,
        colors: InfoColors::for_mode(ThemeMode::Day),
        events: Vec::new(),
    };

    let mut harness = build_widget_harness(state, Vec2::new(820.0, 680.0), |ui, st| {
        let ctx = ui.ctx().clone();
        let bounds = Rect::from_min_size(Pos2::ZERO, ctx.content_rect().size());
        let evs = st.host.show(&ctx, bounds, &mut st.session, &st.colors);
        st.events.extend(evs);
    });

    harness.step();
    let ctx = harness.ctx.clone();
    {
        let st = harness.state_mut();
        st.host.push(&ctx, BoxKind::PeerQr(qr_state), &st.colors);
    }
    for _ in 0..6 {
        harness.step();
    }

    assert!(!AppThemesHost::is_open(&ctx));

    harness.get_by_label("Share Theme").click();
    harness.step();

    let overlay_req = harness
        .state()
        .events
        .iter()
        .find_map(|ev| match ev {
            BoxEvent::OverlayRequest(req @ DeepLinkOverlayRequest::OpenShareTheme { .. }) => {
                Some(req.clone())
            }
            _ => None,
        })
        .expect("PeerQrBox 'Share Theme' button must emit DeepLinkOverlayRequest::OpenShareTheme");

    assert_eq!(
        overlay_req,
        DeepLinkOverlayRequest::OpenShareTheme {
            peer_id: expected_peer_id,
            peer_name: expected_title.clone(),
        }
    );

    assert!(dispatch_themes_deep_link_overlay(&ctx, &overlay_req));
    assert!(AppThemesHost::is_open(&ctx));
    assert_eq!(AppThemesHost::active_tab(&ctx), ThemesTab::ChatThemes);
    assert_eq!(
        AppThemesHost::share_theme_target_peer(&ctx),
        Some((expected_peer_id.0, expected_title))
    );
}
