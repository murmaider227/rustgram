//! End-to-end UI tests for the Intro / Login flow driven via `egui_kittest` AccessKit nodes.
//!
//! Covers:
//! - Start -> Start Messaging -> QR -> Phone -> Code -> 2FA Password -> Main window
//! - Non-2FA login (`TEST_PHONE_APP` + `TEST_CODE_APP` -> Main window)
//! - QR step and Back navigation (`← Back` button, Phone <-> QR toggle)
//! - Invalid code / invalid password error display

use egui::{Key, Modifiers, Vec2};
use egui_kittest::kittest::Queryable;
use tg_app::intro::fake_invoker::{
    TEST_2FA_PASSWORD, TEST_CODE_2FA, TEST_CODE_APP, TEST_PHONE_2FA, TEST_PHONE_APP,
};
use tg_app::window::controller::RootScreen;

use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, build_harness, press_key, settle_animations, type_text,
    wait_for_label_contains_settled, wait_for_label_settled, wait_until,
};

const INTRO_SIZE: Vec2 = Vec2::new(960.0, 680.0);

/// Helper to replace the text of a focused AccessKit `TextInput` node via Ctrl+A + Backspace + Text.
fn replace_text_input(
    harness: &mut egui_kittest::Harness<'_, AppTestState>,
    index: usize,
    value: &str,
) {
    let nodes: Vec<_> = harness
        .get_all_by_role(egui::accesskit::Role::TextInput)
        .collect();
    assert!(
        index < nodes.len(),
        "expected at least {} TextInput nodes, found {}",
        index + 1,
        nodes.len()
    );
    nodes[index].click();
    harness.step();
    press_key(harness, Modifiers::COMMAND, Key::A);
    press_key(harness, Modifiers::NONE, Key::Backspace);
    type_text(harness, value);
}

#[test]
fn intro_full_2fa_flow_start_phone_code_password_to_main() {
    let mut harness = build_harness(AppTestState::intro(), INTRO_SIZE);
    wait_for_label_settled(&mut harness, "Rustgram Desktop");

    assert_eq!(harness.state().controller.root_screen(), RootScreen::Intro);

    // 1. Click "Start Messaging" -> opens QR step by default (matching tdesktop).
    harness.get_by_label("Start Messaging").click();
    wait_for_label_settled(&mut harness, "Log in using phone number");

    // 2. Switch from QR step to Phone step.
    harness.get_by_label("Log in using phone number").click();
    wait_for_label_settled(&mut harness, "Your Phone Number");

    // 3. Enter synthetic 2FA test phone number (+999 662 0000).
    // Phone step has 2 TextInputs: [0] country code ("+1"), [1] local phone number.
    let code_prefix = format!("+{}", &TEST_PHONE_2FA[..3]); // "+999"
    let local_digits = &TEST_PHONE_2FA[3..]; // "6620000"
    replace_text_input(&mut harness, 0, &code_prefix);
    replace_text_input(&mut harness, 1, local_digits);

    // Click Next button.
    harness.get_by_label("Next").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Code step should display delivery description",
        |h| {
            !h.state().controller.is_animating()
                && (h.query_by_label_contains("A code was sent").is_some()
                    || h.query_by_label_contains("activation code").is_some())
        },
    );

    // 4. Code step: enter 5-digit code via keyboard (auto-submitted on 5th digit -> PasswordCheck).
    type_text(&mut harness, TEST_CODE_2FA);
    wait_for_label_settled(&mut harness, "Cloud password check");

    // 5. Enter wrong password first to verify error feedback renders.
    let pwd_node = harness.get_by_role(egui::accesskit::Role::PasswordInput);
    pwd_node.click();
    harness.step();
    type_text(&mut harness, "wrong-password");
    harness.get_by_label("Submit").click();
    wait_for_label_contains_settled(&mut harness, "wrong password");
    assert_eq!(
        harness.state().controller.root_screen(),
        RootScreen::Intro,
        "wrong 2FA password must keep user on Intro"
    );

    // Clear and enter the valid SRP-6a password (`rustgram-2fa`).
    let pwd_node = harness.get_by_role(egui::accesskit::Role::PasswordInput);
    pwd_node.click();
    harness.step();
    press_key(&mut harness, Modifiers::COMMAND, Key::A);
    press_key(&mut harness, Modifiers::NONE, Key::Backspace);
    type_text(&mut harness, TEST_2FA_PASSWORD);
    harness.get_by_label("Submit").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "valid 2FA password must sign in and open Main window",
        |h| {
            !h.state().controller.is_animating()
                && h.state().controller.root_screen() == RootScreen::Main
                && h.state().controller.session().is_some()
        },
    );
}

#[test]
fn intro_non_2fa_phone_and_code_reaches_main_window() {
    let mut harness = build_harness(AppTestState::intro(), INTRO_SIZE);
    wait_for_label_settled(&mut harness, "Start Messaging");

    harness.get_by_label("Start Messaging").click();
    wait_for_label_settled(&mut harness, "Log in using phone number");
    harness.get_by_label("Log in using phone number").click();
    wait_for_label_settled(&mut harness, "Your Phone Number");

    let code_prefix = format!("+{}", &TEST_PHONE_APP[..3]);
    let local_digits = &TEST_PHONE_APP[3..];
    replace_text_input(&mut harness, 0, &code_prefix);
    replace_text_input(&mut harness, 1, local_digits);

    harness.get_by_label("Next").click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Code step must appear after submitting phone",
        |h| {
            !h.state().controller.is_animating()
                && (h.query_by_label_contains("A code was sent").is_some()
                    || h.query_by_label_contains("activation code").is_some())
        },
    );

    // Enter wrong code first -> remains on Code step.
    type_text(&mut harness, "99999");
    settle_animations(&mut harness);
    assert_eq!(harness.state().controller.root_screen(), RootScreen::Intro);

    // Backspace all 5 digits and type the valid app code (`12345`).
    for _ in 0..5 {
        press_key(&mut harness, Modifiers::NONE, Key::Backspace);
    }
    type_text(&mut harness, TEST_CODE_APP);
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "valid non-2FA code must open Main window",
        |h| {
            !h.state().controller.is_animating()
                && h.state().controller.root_screen() == RootScreen::Main
        },
    );
}

#[test]
fn intro_qr_step_and_back_navigation() {
    let mut harness = build_harness(AppTestState::intro(), INTRO_SIZE);
    wait_for_label_settled(&mut harness, "Start Messaging");

    // Start -> QR step.
    harness.get_by_label("Start Messaging").click();
    wait_for_label_settled(&mut harness, "Log in using phone number");
    assert!(
        harness.query_by_label_contains("Go back").is_some(),
        "QR screen should show a 'Go back' button"
    );

    // QR -> Phone step.
    harness.get_by_label("Log in using phone number").click();
    wait_for_label_settled(&mut harness, "Your Phone Number");

    // Phone -> QR step via "Quick log in using QR code" link.
    harness.get_by_label("Quick log in using QR code").click();
    wait_for_label_settled(&mut harness, "Log in using phone number");

    // QR -> Start step via Back button.
    harness.get_by_label_contains("Go back").click();
    wait_for_label_settled(&mut harness, "Start Messaging");
    assert_eq!(harness.state().controller.root_screen(), RootScreen::Intro);
}
