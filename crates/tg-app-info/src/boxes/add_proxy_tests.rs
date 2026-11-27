//! Tests for the "Add Proxy" box details table.

use super::*;
use tg_ui::theme::DayPalette;

/// Paints the box body for one frame, as the box host does.
fn paint_body(state: &mut AddProxyBoxState) {
    let ctx = egui::Context::default();
    ctx.set_fonts(tg_ui::fonts::build_tdesktop_font_definitions());
    let palette = DayPalette::new();
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        state.show_body(ui, &palette)
    });
    output.textures_delta.clear();
}

fn secret_row(state: &AddProxyBoxState) -> Option<String> {
    state
        .detail_rows()
        .into_iter()
        .find_map(|(label, value)| (label == "Secret").then_some(value))
}

#[test]
fn non_ascii_secret_from_a_link_paints_without_panic() {
    // 23 ASCII bytes, then a 2-byte char across the 24-byte preview cut.
    let secret = format!("{}éééé", "a".repeat(23));
    let url = format!("tg://proxy?server=1.2.3.4&port=443&secret={secret}");
    let mut state = AddProxyBoxState::from_url(&url).expect("proxy link parses");
    assert_eq!(state.proxy.password, secret);
    paint_body(&mut state);
    assert_eq!(
        secret_row(&state).as_deref(),
        Some(format!("{}é…", "a".repeat(23)).as_str())
    );
}

#[test]
fn ascii_secret_preview_keeps_24_chars() {
    let secret = "ee".repeat(20);
    let url = format!("tg://proxy?server=1.2.3.4&port=443&secret={secret}");
    let state = AddProxyBoxState::from_url(&url).expect("proxy link parses");
    assert_eq!(
        secret_row(&state).as_deref(),
        Some(format!("{}…", &secret[..24]).as_str())
    );
    let short = AddProxyBoxState::from_url("tg://proxy?server=1.2.3.4&port=443&secret=dd0011")
        .expect("proxy link parses");
    assert_eq!(secret_row(&short).as_deref(), Some("dd0011"));
}
