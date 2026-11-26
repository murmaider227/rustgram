//! Before | after renders of a failed-request toast and of the forced-logout notice
//! (TGB-2957, TGB-2956), from mock data. Writes PNGs to `$TGFORK_SHOTS_DIR`:
//!
//! ```sh
//! TGFORK_SHOTS_DIR=<dir> cargo test -p tg-app-window user_error_shots -- --ignored
//! ```
//!
//! "Before" puts the exact text the old code produced into the toast, bypassing the toast sink
//! filter; "after" goes through the real call path.

use egui::Vec2;
use egui_kittest::HarnessBuilder;

use super::*;
use crate::mock_session::MockSession;

fn harness(ctrl: WindowController) -> egui_kittest::Harness<'static, WindowController> {
    HarnessBuilder::default()
        .with_size(Vec2::new(1000.0, 640.0))
        .with_render_cursor(false)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(tg_ui::fonts::register_tdesktop_fonts)
        .build_ui_state(|ui, ctrl: &mut WindowController| ctrl.show(ui), ctrl)
}

fn controller() -> WindowController {
    WindowController::with_session(
        MockSession::new(true),
        ShellSettings::default(),
        std::env::temp_dir().join("rustgram_user_error_shots.json"),
    )
}

fn save(
    h: &mut egui_kittest::Harness<'static, WindowController>,
    dir: &std::path::Path,
    name: &str,
) {
    for _ in 0..8 {
        h.step();
    }
    let img = h.render().expect("wgpu render");
    img.save(dir.join(name)).expect("save png");
}

#[test]
#[ignore = "writes PNGs; run with TGFORK_SHOTS_DIR set"]
fn user_error_shots() {
    let Some(dir) = std::env::var_os("TGFORK_SHOTS_DIR").map(std::path::PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).expect("shots dir");
    // `InvocationError::Rpc` display, as `tg-api/tests/rpc_error_text.rs` pins it.
    let raw = "RPC error 400: USERS_TOO_MUCH";

    let mut before = controller();
    // The old `InvocationError::Rpc` display (`RPC error {0:?}`) behind the old toast prefix.
    before.layer_stack.show_raw_toast_for_test(
        "Could not join chat: RPC error RpcError { code: 400, name: \"USERS_TOO_MUCH\", value: None }",
    );
    save(&mut harness(before), &dir, "join_chat_toast_before.png");

    let mut after = controller();
    if let Some(mock) = after
        .session_backend
        .as_any_mut()
        .downcast_mut::<MockSession>()
    {
        mock.fail_next_request(raw);
    }
    let ctx = egui::Context::default();
    after.handle_box_event(
        BoxEvent::JoinInviteConfirmed {
            hash: "AbCdEf".to_owned(),
            is_group: true,
        },
        &ctx,
    );
    save(&mut harness(after), &dir, "join_chat_toast_after.png");

    let mut drop_before = controller();
    drop_before
        .layer_stack
        .show_raw_toast_for_test("Session signed out by server (AUTH_KEY_DROP_DUPLICATE).");
    save(&mut harness(drop_before), &dir, "forced_logout_before.png");

    let mut drop_after = controller();
    drop_after.show_forced_logout_box(
        "Your session was terminated because you logged in on too many devices.".to_owned(),
    );
    save(&mut harness(drop_after), &dir, "forced_logout_after.png");
}
