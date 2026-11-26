//! The window side of TGB-2977 / TGB-2978: limit boxes, the Premium screen, boxes with links and
//! a background account's forced logout.

use super::*;
use crate::mock_session::MockSession;
use crate::window::deep_links::DeepLinkOverlayRequest;
use crate::window::layer_stack::{LayerAction, ModalBox};
use tg_app_core::user_error::{LimitBoxKind, SendTarget, UserFacingError};

fn controller() -> WindowController {
    WindowController::with_session(
        MockSession::new(true),
        ShellSettings::default(),
        std::env::temp_dir().join("rustgram_user_errors_w4_test.json"),
    )
}

fn set_premium(ctrl: &mut WindowController, premium: bool) {
    tg_app_core::chat_helpers::backend::ChatHelpersBackend::set_premium(
        &mut *ctrl.session_backend,
        premium,
    );
}

fn premium_requests(ctrl: &mut WindowController) -> Vec<Option<String>> {
    ctrl.take_deep_link_overlay_requests()
        .into_iter()
        .filter_map(|req| match req {
            DeepLinkOverlayRequest::OpenPayments {
                screen: tg_app_info::payments::PaymentsScreen::Premium,
                slug,
            } => Some(slug),
            _ => None,
        })
        .collect()
}

#[test]
fn limit_box_offers_increase_to_free_accounts_and_ok_to_premium() {
    let mut ctrl = controller();
    set_premium(&mut ctrl, false);
    ctrl.show_user_error(UserFacingError::LimitBox(LimitBoxKind::FilterLinks));
    let Some(ModalBox::PremiumLimit {
        title,
        increase_ref,
        ..
    }) = ctrl.layer_stack.top_box()
    else {
        panic!("no limit box");
    };
    assert_eq!(title, &tg_lang::tr::lng_filter_links_limit_title());
    assert_eq!(
        increase_ref.as_deref(),
        Some("double_limits__chatlist_invites")
    );
    assert_eq!(ctrl.layer_stack.toast_text(), None);

    ctrl.handle_layer_action(LayerAction::OpenPremium(
        "double_limits__chatlist_invites".to_owned(),
    ));
    assert_eq!(
        premium_requests(&mut ctrl),
        vec![Some("double_limits__chatlist_invites".to_owned())]
    );

    let mut ctrl = controller();
    set_premium(&mut ctrl, true);
    ctrl.show_user_error(UserFacingError::LimitBox(LimitBoxKind::Channels));
    assert!(matches!(
        ctrl.layer_stack.top_box(),
        Some(ModalBox::PremiumLimit {
            increase_ref: None,
            ..
        })
    ));
}

#[test]
fn premium_required_send_opens_the_premium_screen() {
    let mut ctrl = controller();
    ctrl.show_user_error(tg_app_core::user_error::send_message(
        "RPC error 400: PREMIUM_ACCOUNT_REQUIRED",
        SendTarget::User,
    ));
    assert_eq!(
        premium_requests(&mut ctrl),
        vec![Some("premium_stickers".to_owned())]
    );
    assert_eq!(ctrl.layer_stack.toast_text(), None);
}

#[test]
fn peer_flood_box_link_opens_spambot() {
    let mut ctrl = controller();
    ctrl.show_user_error(tg_app_core::user_error::send_message(
        "RPC error 400: PEER_FLOOD",
        SendTarget::User,
    ));
    let Some(ModalBox::InformRich { body, .. }) = ctrl.layer_stack.top_box() else {
        panic!("no inform box with a link");
    };
    assert!(body.spans.iter().any(|span| span.kind
        == tg_ui::text::TextEntityKind::Link(tg_app_core::user_error::SPAMBOT_LINK.to_owned())));
    assert_eq!(
        ctrl.layer_stack.toast_text(),
        None,
        "the old code toasted PEER_FLOOD"
    );
}

#[test]
fn background_forced_logout_box_logs_out_only_that_account() {
    let mut ctrl = controller();
    ctrl.show_background_forced_logout_box(
        2,
        tg_ui::text::RichText::plain("Your session was terminated."),
    );
    ctrl.show_background_forced_logout_box(2, tg_ui::text::RichText::plain("again"));
    assert!(ctrl.is_forced_logout_shown());
    assert!(
        ctrl.take_background_logouts().is_empty(),
        "not before it closes"
    );
    assert!(
        !ctrl.layer_stack.close_top_by_back(),
        "Escape does not close it"
    );

    ctrl.handle_layer_action(LayerAction::ForcedLogout(Some(2)));
    assert_eq!(ctrl.take_background_logouts(), vec![2]);
    assert!(
        !ctrl.take_logout_requested(),
        "the window's own account stays"
    );
}

#[test]
fn chatlist_join_past_the_limit_shows_the_shared_folders_box() {
    let mut ctrl = controller();
    ctrl.show_user_error(tg_app_core::user_error::join_chatlist(
        "RPC error 400: CHATLISTS_TOO_MUCH",
    ));
    assert!(matches!(
        ctrl.layer_stack.top_box(),
        Some(ModalBox::PremiumLimit { title, .. })
            if *title == tg_lang::tr::lng_filter_shared_limit_title()
    ));
}
