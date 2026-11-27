//! TGB-3105: render tests for the Settings loading states of TGB-3070. A live backend reports a
//! read as pending until the server answers; the screens then show tdesktop's loading state and
//! ignore clicks that would act on the placeholder. `MockSettingsBackend::set_read_pending`
//! reproduces that here.

use egui::{Pos2, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tg_core::CoreSettings;

use super::backend::{NotifyScope, SettingsBackend, SettingsRead};
use super::cloud_password::{
    CloudPasswordStep, CloudPasswordWizard, render_cloud_password_section,
};
use super::mock::MockSettingsBackend;
use super::notifications::{NotificationsState, render_notifications_section};
use super::privacy::{PrivacyEditor, PrivacyOutput, PrivacyState, render_privacy_section};
use super::sessions::{SessionsState, render_sessions_section};
use super::strings;
use super::theme_bridge::LiveThemeColors;

const SIZE: Vec2 = Vec2::new(520.0, 2400.0);

struct Privacy {
    state: PrivacyState,
    backend: MockSettingsBackend,
    out: PrivacyOutput,
}

fn privacy_harness(backend: MockSettingsBackend) -> Harness<'static, Privacy> {
    let colors = LiveThemeColors::default();
    let mut h = Harness::builder()
        .with_size(SIZE)
        .with_context_setup(tg_ui::fonts::register_tdesktop_fonts)
        .build_ui_state(
            move |ui, st: &mut Privacy| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let out =
                        render_privacy_section(ui, &mut st.state, false, &mut st.backend, &colors);
                    st.out.open_cloud_password |= out.open_cloud_password;
                });
            },
            Privacy {
                state: PrivacyState::default(),
                backend,
                out: PrivacyOutput::default(),
            },
        );
    h.run();
    h
}

fn pending(reads: &[SettingsRead]) -> MockSettingsBackend {
    let mut backend = MockSettingsBackend::new();
    for read in reads {
        backend.set_read_pending(*read, true);
    }
    backend
}

#[test]
fn privacy_shows_loading_rows_and_ignores_their_clicks() {
    let mut h = privacy_harness(pending(&[
        SettingsRead::CloudPassword,
        SettingsRead::BlockedUsers,
        SettingsRead::AccountTtl,
    ]));

    // Two-Step Verification: "Loading..." and a click does nothing.
    h.get_by_label("Two-Step Verification, Loading...").click();
    h.run();
    assert!(!h.state().out.open_cloud_password);

    // Blocked users: no count while `contacts.getBlocked` loads.
    assert!(h.query_by_label("Blocked users").is_some());
    assert!(h.query_by_label("Blocked users, 1").is_none());

    // Self-destruction: no period and no box.
    h.get_by_label(strings::IF_AWAY_FOR_TITLE).click();
    h.run();
    assert!(matches!(h.state().state.editor, PrivacyEditor::None));
}

#[test]
fn privacy_rows_act_once_their_reads_answer() {
    let mut h = privacy_harness(MockSettingsBackend::new());
    h.get_by_label("Two-Step Verification, Off").click();
    h.run();
    assert!(h.state().out.open_cloud_password);
    assert!(h.query_by_label("Blocked users, 1").is_some());
    let ttl_prefix = format!("{}, ", strings::IF_AWAY_FOR_TITLE);
    let ttl = h.get_by_label_contains(&ttl_prefix).rect().center();
    h.input_mut().events.push(egui::Event::PointerMoved(ttl));
    h.run();
    h.input_mut().events.push(egui::Event::PointerButton {
        pos: ttl,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.input_mut().events.push(egui::Event::PointerButton {
        pos: ttl,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.run();
    assert!(matches!(
        h.state().state.editor,
        PrivacyEditor::AccountTtl { .. }
    ));
}

#[test]
fn active_sessions_show_loading_instead_of_the_list() {
    struct Sessions {
        state: SessionsState,
        core: CoreSettings,
        backend: MockSettingsBackend,
    }
    let colors = LiveThemeColors::default();
    let mut h = Harness::builder()
        .with_size(SIZE)
        .with_context_setup(tg_ui::fonts::register_tdesktop_fonts)
        .build_ui_state(
            move |ui, st: &mut Sessions| {
                let _ = render_sessions_section(
                    ui,
                    &mut st.state,
                    &mut st.core,
                    &mut st.backend,
                    &colors,
                );
            },
            Sessions {
                state: SessionsState::default(),
                core: CoreSettings::new(),
                backend: pending(&[SettingsRead::ActiveSessions]),
            },
        );
    h.run();
    assert!(h.query_by_label("Loading...").is_some());
    let others = h.state().backend.active_sessions();
    assert!(
        others.iter().any(|s| !s.is_current()),
        "mock has other sessions"
    );

    h.state_mut()
        .backend
        .set_read_pending(SettingsRead::ActiveSessions, false);
    h.run();
    assert!(h.query_by_label("Loading...").is_none());
}

#[test]
fn cloud_password_wizard_waits_for_the_real_state() {
    let mut backend = MockSettingsBackend::new();
    backend
        .set_cloud_password(None, "correct horse", "", None)
        .expect("set password");
    backend.set_read_pending(SettingsRead::CloudPassword, true);

    struct Wizard {
        wizard: CloudPasswordWizard,
        backend: MockSettingsBackend,
    }
    let colors = LiveThemeColors::default();
    // `tg_ui::widgets::InputField` gives its password `TextEdit` no accessible name (its
    // placeholder is painted, not a hint); filed separately, not this test's subject.
    let mut h = Harness::builder()
        .with_size(SIZE)
        .with_accessibility_check(false)
        .with_context_setup(tg_ui::fonts::register_tdesktop_fonts)
        .build_ui_state(
            move |ui, st: &mut Wizard| {
                let _ = render_cloud_password_section(ui, &mut st.wizard, &mut st.backend, &colors);
            },
            Wizard {
                wizard: CloudPasswordWizard::default(),
                backend,
            },
        );
    h.run();
    assert_eq!(h.state().wizard.step, CloudPasswordStep::Start);

    h.state_mut()
        .backend
        .set_read_pending(SettingsRead::CloudPassword, false);
    h.run();
    assert_eq!(h.state().wizard.step, CloudPasswordStep::EnterCurrent);
}

#[test]
fn notification_scope_toggles_do_not_save_while_loading() {
    struct Notify {
        state: NotificationsState,
        core: CoreSettings,
        backend: MockSettingsBackend,
    }
    let colors = LiveThemeColors::default();
    let mut h = Harness::builder()
        .with_size(SIZE)
        .with_context_setup(tg_ui::fonts::register_tdesktop_fonts)
        .build_ui_state(
            move |ui, st: &mut Notify| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let _ = render_notifications_section(
                        ui,
                        &mut st.state,
                        &mut st.core,
                        &mut st.backend,
                        &colors,
                    );
                });
            },
            Notify {
                state: NotificationsState::default(),
                core: CoreSettings::new(),
                backend: pending(&[SettingsRead::ScopeNotifications(NotifyScope::Groups)]),
            },
        );
    h.run();

    let toggle_of = |h: &Harness<'_, Notify>, scope: NotifyScope| -> Pos2 {
        let row = h.get_by_label(&scope.localized_title()).rect();
        Pos2::new(SIZE.x - 24.0, row.center().y)
    };
    let click = |h: &mut Harness<'_, Notify>, pos: Pos2| {
        h.input_mut().events.push(egui::Event::PointerMoved(pos));
        h.run();
        for pressed in [true, false] {
            h.input_mut().events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        h.run();
    };

    let groups_before = h.state().backend.scope_notifications(NotifyScope::Groups);
    let pos = toggle_of(&h, NotifyScope::Groups);
    click(&mut h, pos);
    assert_eq!(
        h.state().backend.scope_notifications(NotifyScope::Groups),
        groups_before,
        "a toggle on a loading scope must not save the placeholder"
    );

    // A scope that has loaded saves its toggle.
    let channels_before = h.state().backend.scope_notifications(NotifyScope::Channels);
    let pos = toggle_of(&h, NotifyScope::Channels);
    click(&mut h, pos);
    assert_ne!(
        h.state()
            .backend
            .scope_notifications(NotifyScope::Channels)
            .enabled,
        channels_before.enabled
    );
}
