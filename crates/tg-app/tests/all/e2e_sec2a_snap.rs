//! Before/after snapshots for security review #2 (SEC2-A): a `-sendpath` activation while the
//! passcode lock is shown (L1), a `?start=` bot link (L4) and a first-time Mini App link (L5).
//! Run with `RUSTGRAM_SNAPSHOT_DIR=<dir>` and `RUSTGRAM_SEC2A_SNAP_STAGE=before|after`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use egui::Vec2;
use egui_kittest::kittest::Queryable;

use crate::common::{AppTestState, build_wgpu_harness, settle_layout};

// ---------------------------------------------------------------------------------------------

static WGPU_SEC2A_LOCK: Mutex<()> = Mutex::new(());

fn save_png(harness: &mut egui_kittest::Harness<'static, AppTestState>, path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create snap dir");
    }
    let img = harness.render().expect("wgpu render");
    img.save(path).expect("save png");
}

#[test]
#[ignore = "captures before/after PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_sec2a_snapshots() {
    let _guard = WGPU_SEC2A_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stage = std::env::var("RUSTGRAM_SEC2A_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned());
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("sec2a"));
    let file_dir = tempfile::tempdir().expect("tempdir");
    let shared = file_dir.path().join("holiday.png");
    std::fs::write(&shared, b"png").expect("write file");

    for (size_label, size) in [
        ("1440x900", Vec2::new(1440.0, 900.0)),
        ("390x844", Vec2::new(390.0, 844.0)),
    ] {
        // 1. `-sendpath` activation while the passcode lock is shown.
        {
            let mut state = AppTestState::main_workspace();
            let verifier: tg_app::settings::local_passcode::PasscodeVerifierFn =
                Arc::new(|candidate: &str| candidate == "2468");
            state
                .controller
                .set_external_passcode_verifier(Some(verifier));
            state.controller.setup_passcode_lock();
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            let ctx = h.ctx.clone();
            h.state_mut().controller.handle_incoming_activation(
                tg_app::startup::ipc::IncomingActivation {
                    urls: Vec::new(),
                    send_paths: vec![shared.clone()],
                    show: true,
                    quit: false,
                },
                &ctx,
            );
            settle_layout(&mut h);
            save_png(
                &mut h,
                &dir.join(format!("locked_sendpath_{size_label}_{stage}.png")),
            );
        }

        // 2. `?start=` bot link.
        {
            let state = AppTestState::main_workspace();
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            h.state_mut()
                .controller
                .open_link("https://t.me/catalog_demo_bot?start=promo42");
            settle_layout(&mut h);
            save_png(
                &mut h,
                &dir.join(format!("bot_start_link_{size_label}_{stage}.png")),
            );
        }

        // 3. First-time Mini App link.
        {
            let state = AppTestState::main_workspace();
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            h.state_mut()
                .controller
                .open_link("https://t.me/catalog_demo_bot?startapp=promo42");
            settle_layout(&mut h);
            save_png(
                &mut h,
                &dir.join(format!("mini_app_link_{size_label}_{stage}.png")),
            );
            if stage == "after" {
                assert!(
                    h.query_by_label(&tg_lang::tr::lng_view_button_bot_app())
                        .is_some(),
                    "Launch button visible"
                );
            }
        }
    }
}
