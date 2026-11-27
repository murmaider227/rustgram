//! Wave 8 SCREENS tests for the window: the profile Mute menu's sound toggle (TGB-2873).

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tg_app_core::stand_in::theme::ThemeMode;
use tg_app_info::dialogs::backend::DialogsBackend;
use tg_app_info::info::InfoWrapWidget;
use tg_app_info::info::colors::InfoColors;

use crate::mock_session::{MockSession, session_peers};
use crate::window::main_widget::show_info_with_sound_toggle;

struct State {
    info: InfoWrapWidget,
    session: MockSession,
}

fn right_click(h: &mut Harness<'_, State>, label: &str) {
    let pos = h.get_by_label(label).rect().center();
    h.input_mut().events.push(egui::Event::PointerMoved(pos));
    h.run();
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        h.run();
    }
}

#[test]
fn mute_menu_sound_toggle_reaches_the_dialog_and_flips_the_menu() {
    let peer = session_peers::USER_MIRA;
    let colors = InfoColors::for_mode(ThemeMode::Day);
    let mut h = Harness::builder()
        .with_size(egui::Vec2::new(420.0, 800.0))
        .with_context_setup(tg_ui::fonts::register_tdesktop_fonts)
        .build_ui_state(
            move |ui, st: &mut State| {
                let _ = show_info_with_sound_toggle(
                    &mut st.info,
                    peer,
                    ui,
                    &mut st.session,
                    false,
                    &colors,
                );
            },
            State {
                info: InfoWrapWidget::for_peer(peer),
                session: MockSession::new(true),
            },
        );
    h.run();
    assert!(!h.state().session.dialog_notify_sound(peer).none);

    right_click(&mut h, "Mute");
    h.get_by_label("Disable sound").click();
    h.run();
    assert!(
        h.state().session.dialog_notify_sound(peer).none,
        "\"Disable sound\" must make the chat silent"
    );

    right_click(&mut h, "Mute");
    assert!(
        h.query_by_label("Enable sound").is_some(),
        "the menu must read \"Enable sound\" for a silent chat"
    );
    h.get_by_label("Enable sound").click();
    h.run();
    assert!(!h.state().session.dialog_notify_sound(peer).none);
}

/// TGB-3111: line widths at 150 % for the test line of TGB-3040, from egui's own layout:
/// 13 pt at 1.5 (19.5 px, today), 19 px (`ConvertScale`), and 19 px with every advance rounded
/// to whole pixels as GDI's and FreeType's hinted renderers do. Prints numbers only; run with
/// `--ignored --nocapture`.
#[test]
#[ignore = "measurement for TGB-3111; prints line widths"]
fn measure_line_widths_at_150_percent() {
    const LINE: &str = "The quick brown fox jumps over the lazy dog, 0123456789 times again.";
    let ctx = egui::Context::default();
    ctx.set_fonts(tg_ui::fonts::build_tdesktop_font_definitions());
    let ppp = 1.5_f32;
    let mut input = egui::RawInput::default();
    input
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .native_pixels_per_point = Some(ppp);
    let mut widths = Vec::new();
    for _ in 0..2 {
        widths.clear();
        ctx.run_ui(input.clone(), |ui| {
            for size_px in [19.5_f32, 19.0] {
                let galley = ui.painter().layout_no_wrap(
                    LINE.to_owned(),
                    egui::FontId::new(size_px / ppp, tg_ui::fonts::family_regular()),
                    egui::Color32::BLACK,
                );
                let glyphs = &galley.rows[0].row.glyphs;
                let unhinted: f32 = glyphs.iter().map(|g| g.advance_width * ppp).sum();
                let rounded: f32 = glyphs.iter().map(|g| (g.advance_width * ppp).round()).sum();
                widths.push((size_px, unhinted, rounded));
            }
        })
        .textures_delta
        .clear();
    }
    for (size, unhinted, rounded) in &widths {
        println!(
            "TGB-3111 {size} px: advance sum unhinted {unhinted:.1} px, rounded {rounded:.1} px"
        );
    }
}

/// Wave 8 capture (TGB-2984, TGB-3111): the "Keep this theme?" and "Restart now?" boxes,
/// written to `$W8_SNAP_DIR/after/<box>_<mode>.png`.
#[test]
#[ignore = "captures the wave 8 boxes under W8_SNAP_DIR"]
fn capture_w8_boxes() {
    use crate::window::layer_stack::{LayerStack, ModalBox};
    use tg_app_core::stand_in::strings::EnglishStrings;
    use tg_app_core::stand_in::theme::Palette;
    use tg_app_core::stand_in::types::ConnectionStatus;

    let Some(dir) = std::env::var_os("W8_SNAP_DIR").map(std::path::PathBuf::from) else {
        return;
    };
    let dir = dir.join("after");
    std::fs::create_dir_all(&dir).expect("create snap dir");
    let boxes = [
        (
            "keep_theme",
            ModalBox::KeepTheme {
                started: tg_app_core::clock::now(),
            },
        ),
        (
            "scale_restart",
            ModalBox::ConfirmScaleRestart { scale: 150 },
        ),
    ];
    for (box_name, modal) in boxes {
        for (mode, name) in [(ThemeMode::Day, "day"), (ThemeMode::Night, "night")] {
            let palette = Palette::for_mode(mode);
            let mut stack = LayerStack::new();
            stack.show_box(modal.clone());
            let mut h = Harness::builder()
                .with_size(egui::Vec2::new(520.0, 360.0))
                .with_context_setup(tg_ui::fonts::register_tdesktop_fonts)
                .wgpu()
                .build_ui_state(
                    move |ui, stack: &mut LayerStack| {
                        ui.painter()
                            .rect_filled(ui.max_rect(), 0.0, palette.window_bg);
                        let _ = stack.show_boxes_and_toasts(
                            ui.ctx(),
                            &palette,
                            &EnglishStrings,
                            ConnectionStatus::Connected,
                            false,
                        );
                    },
                    stack,
                );
            for _ in 0..10 {
                h.step();
            }
            let img = h.render().expect("wgpu render");
            img.save(dir.join(format!("{box_name}_{name}.png")))
                .expect("save png");
        }
    }
}

/// TGB-3122: Ctrl+Q quits; its close must not be turned into a hide to the tray.
#[test]
fn ctrl_q_marks_the_app_as_quitting() {
    use crate::window::controller::WindowController;
    use crate::window::shortcuts_wiring::dispatch_global_shortcuts;
    use tg_app_core::stand_in::settings::ShellSettings;
    use tg_app_media::tray::window_close::is_quitting;

    let mut ctrl = WindowController::with_session(
        MockSession::new(true),
        ShellSettings::default(),
        std::env::temp_dir().join("rustgram_w8_ctrl_q_test.json"),
    );
    let ctx = egui::Context::default();
    let input = egui::RawInput {
        events: vec![egui::Event::Key {
            key: egui::Key::Q,
            physical_key: Some(egui::Key::Q),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        }],
        ..egui::RawInput::default()
    };
    let mut out = ctx.run_ui(input, |ui| {
        dispatch_global_shortcuts(&mut ctrl, ui.ctx(), 1_000)
    });
    out.textures_delta.clear();
    assert!(is_quitting(&ctx));
    let cmds = out
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|v| v.commands.clone())
        .unwrap_or_default();
    assert!(cmds.contains(&egui::ViewportCommand::Close));
}
