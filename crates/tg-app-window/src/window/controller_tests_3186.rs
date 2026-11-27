//! TGB-3186: the third column closed with its x stays closed. tdesktop keeps
//! `thirdSectionInfoEnabled` in `Core::App().settings()`, shared by every account's window; in
//! Rustgram each account's [`WindowController`] had its own copy, all saved into one file on exit,
//! so another account's stale copy brought the column back.

use super::*;
use crate::window::section::SectionMemento;

fn window(third_column: bool, file: &str) -> WindowController {
    let mut settings = ShellSettings::default();
    settings.columns.third_section_info_enabled = third_column;
    let mut ctrl = WindowController::with_session(
        MockSession::new(true),
        settings,
        std::env::temp_dir().join(file),
    );
    if let Some(session) = ctrl.session_mut() {
        session.update_layout(1440);
    }
    ctrl
}

fn shows_profile_column(ctrl: &WindowController) -> bool {
    matches!(
        ctrl.session().and_then(|s| s.third_section()),
        Some(SectionMemento::PeerInfo { .. })
    )
}

#[test]
fn third_column_closed_in_one_account_stays_closed_in_the_other() {
    let mut account_a = window(true, "rustgram_tgb3186_a.json");
    let mut account_b = window(true, "rustgram_tgb3186_b.json");
    assert!(shows_profile_column(&account_a) && shows_profile_column(&account_b));

    // The x of account B's column (`InfoWrapEvent::CloseOrBack` -> `closeThirdSection`).
    account_b
        .session_mut()
        .expect("session")
        .close_third_section();
    assert!(!shows_profile_column(&account_b));
    let columns = account_b.saved_columns();
    assert!(!columns.third_section_info_enabled);

    // Switching to account A (or saving on exit) hands over the app-wide columns.
    account_a.adopt_columns(columns);
    assert!(!shows_profile_column(&account_a));
    assert!(!account_a.settings().columns.third_section_info_enabled);
    assert!(!account_a.saved_columns().third_section_info_enabled);
    // The next layout pass and chat do not bring it back.
    let session = account_a.session_mut().expect("session");
    session.update_layout(1440);
    session.show_peer_history(
        tg_app_core::stand_in::types::PeerId::from_user(1002),
        SectionShow::clear_stack(),
        MsgId::SHOW_AT_UNREAD,
    );
    session.update_layout(1440);
    assert!(!shows_profile_column(&account_a));
}

#[test]
fn adopting_an_open_third_column_opens_it() {
    let mut account_a = window(false, "rustgram_tgb3186_c.json");
    assert!(!shows_profile_column(&account_a));
    let mut columns = account_a.saved_columns();
    columns.third_section_info_enabled = true;
    account_a.adopt_columns(columns);
    assert!(shows_profile_column(&account_a));
}
