//! Before | after renders of the TGB-2977 / TGB-2978 error boxes, from mock data. Writes PNGs
//! to `$TGFORK_SHOTS_DIR`:
//!
//! ```sh
//! TGFORK_SHOTS_DIR=<dir> cargo test -p tg-app-window user_error_w4_shots -- --ignored
//! ```
//!
//! "Before" shows what the old code put on screen for the same failure (through the toast
//! filter, as it ran); "after" goes through the new call path.

use egui::Vec2;
use egui_kittest::HarnessBuilder;
use tg_app_core::user_error::{self, LimitBoxKind, SendTarget};

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
        std::env::temp_dir().join("rustgram_user_error_w4_shots.json"),
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

/// The `InvocationError::Rpc` display form a live backend hands the UI.
fn rpc(code: i32, name: &str) -> String {
    format!("RPC error {code}: {name}")
}

#[test]
#[ignore = "writes PNGs; run with TGFORK_SHOTS_DIR set"]
fn user_error_w4_shots() {
    let Some(dir) = std::env::var_os("TGFORK_SHOTS_DIR").map(std::path::PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).expect("shots dir");

    // 1. A send to a non-contact under a spam limit (`PEER_FLOOD`).
    let mut before = controller();
    before.layer_stack.show_toast(rpc(400, "PEER_FLOOD"));
    save(&mut harness(before), &dir, "peer_flood_before.png");
    let mut after = controller();
    after.show_user_error(user_error::send_message(
        &rpc(400, "PEER_FLOOD"),
        SendTarget::User,
    ));
    save(&mut harness(after), &dir, "peer_flood_after.png");

    // 2. Joining a shared folder past the limit (`CHATLISTS_TOO_MUCH`).
    let mut before = controller();
    before
        .layer_stack
        .show_toast(rpc(400, "CHATLISTS_TOO_MUCH"));
    save(&mut harness(before), &dir, "chatlists_limit_before.png");
    let mut after = controller();
    after.show_user_error(user_error::UserFacingError::LimitBox(
        LimitBoxKind::ShareableFilters,
    ));
    save(&mut harness(after), &dir, "chatlists_limit_after.png");

    // 3. Joining a channel past the limit (`CHANNELS_TOO_MUCH`): the old inform box.
    let mut before = controller();
    before.show_user_error(user_error::UserFacingError::Inform {
        title: Some(tg_lang::tr::lng_channels_limit_title()),
        text: format!(
            "{} {}",
            tg_lang::tr::lng_channels_limit1(500),
            tg_lang::tr::lng_channels_limit2(1000)
        )
        .replace("**", ""),
    });
    save(&mut harness(before), &dir, "channels_limit_before.png");
    let mut after = controller();
    after.show_user_error(user_error::channels_limit_box());
    save(&mut harness(after), &dir, "channels_limit_after.png");

    // 4. Redeeming a gift code while Premium is active.
    let mut before = controller();
    before
        .layer_stack
        .show_toast(tg_lang::tr::lng_gift_link_already_title());
    save(&mut harness(before), &dir, "already_premium_before.png");
    let mut after = controller();
    after.show_user_error(user_error::apply_gift_code(&rpc(
        400,
        "PREMIUM_SUB_ACTIVE_UNTIL_1798126200",
    )));
    save(&mut harness(after), &dir, "already_premium_after.png");

    // 5. Forced logout whose server text has a link.
    let text = "Your session was terminated for security reasons. Learn more at telegram.org/faq";
    let mut before = controller();
    before.show_forced_logout_box(text.to_owned());
    save(&mut harness(before), &dir, "forced_logout_link_before.png");
    let mut after = controller();
    let at = text.find("telegram.org").unwrap_or(0);
    after.show_forced_logout_box_rich(user_error::text_with_link(
        text,
        &text[at..],
        "https://telegram.org/faq",
    ));
    save(&mut harness(after), &dir, "forced_logout_link_after.png");

    // 6. A background account dropped by the server: before, nothing showed.
    let before = controller();
    save(&mut harness(before), &dir, "background_logout_before.png");
    let mut after = controller();
    after.show_background_forced_logout_box(
        1,
        tg_ui::text::RichText::plain(
            "Your session was terminated because you logged in on too many devices.",
        ),
    );
    save(&mut harness(after), &dir, "background_logout_after.png");
}
