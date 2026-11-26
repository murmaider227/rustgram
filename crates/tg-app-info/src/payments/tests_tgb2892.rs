//! TGB-2892 evidence: before (developer overlay) / after (tdesktop parity) renders of the
//! Premium section, the "Send a Gift" box and the profile gifts grid with mock data.

use std::sync::Arc;

use egui_kittest::Harness;
use tg_app_core::chat_helpers::backend::StickerMedia;
use tg_app_core::payments::gift_stickers::{GiftStickerSource, install_gift_sticker_source};
use tg_app_core::stand_in::theme::Palette;

use crate::payments::{PaymentsScreen, PaymentsScreenHost};

/// Stand-in sticker for every gift (repo asset), so cards show an animated sticker offline.
struct CakeStickers;

impl GiftStickerSource for CakeStickers {
    fn gift_sticker_media(&self, _document_id: u64) -> Option<StickerMedia> {
        Some(StickerMedia::LottieTgs(
            include_bytes!("../../../tg-app-history/assets/animations/cake.tgs").to_vec(),
        ))
    }
}

fn snap_dir() -> std::path::PathBuf {
    std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("tgb2892"))
}

fn capture(name: &str, size: egui::Vec2, screen: PaymentsScreen, dev: bool) {
    let mut host = PaymentsScreenHost::with_mock();
    host.set_dev_tabs(dev);
    host.select_screen(screen);
    let palette = Palette::day();
    let mut h = Harness::builder()
        .with_size(size)
        .with_context_setup(tg_ui::fonts::register_tdesktop_fonts)
        .wgpu()
        .build_ui_state(
            move |ui, host: &mut PaymentsScreenHost| {
                let _ = host.show_at(ui, &palette, true, 1_000);
            },
            host,
        );
    for _ in 0..12 {
        h.step();
    }
    let img = h.render().expect("wgpu render");
    img.save(snap_dir().join(format!("{name}.png")))
        .expect("save png");
}

#[test]
#[ignore = "captures TGB-2892 before/after PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_tgb2892_premium_and_gifts() {
    std::fs::create_dir_all(snap_dir()).expect("create snap dir");
    install_gift_sticker_source(Arc::new(CakeStickers));
    let tall = 900.0;
    capture(
        "gifts_before",
        egui::Vec2::new(620.0, tall),
        PaymentsScreen::StarGifts,
        true,
    );
    capture(
        "gifts_after",
        egui::Vec2::new(crate::payments::gift_card::BOX_WIDE_WIDTH, tall),
        PaymentsScreen::StarGifts,
        false,
    );
    capture(
        "premium_before",
        egui::Vec2::new(620.0, tall),
        PaymentsScreen::Premium,
        true,
    );
    capture(
        "premium_after",
        egui::Vec2::new(crate::payments::premium_section::PREMIUM_LAYER_WIDTH, tall),
        PaymentsScreen::Premium,
        false,
    );
}

#[test]
fn parity_screens_hide_developer_tabs_by_default() {
    let host = PaymentsScreenHost::with_mock();
    if crate::payments::dev_overlay_enabled() {
        return;
    }
    assert!(!host.dev_tabs());
    let mut host = host;
    host.select_screen(PaymentsScreen::StarGifts);
    assert_eq!(
        host.panel_width_hint(),
        Some(crate::payments::gift_card::BOX_WIDE_WIDTH)
    );
    host.select_screen(PaymentsScreen::Premium);
    assert_eq!(
        host.panel_width_hint(),
        Some(crate::payments::premium_section::PREMIUM_LAYER_WIDTH)
    );
    host.set_dev_tabs(true);
    assert_eq!(host.panel_width_hint(), None);
}

#[test]
#[ignore = "captures the TGB-2892 profile gifts grid under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_tgb2892_profile_gifts() {
    use crate::payments::backend::PaymentsBackend;
    std::fs::create_dir_all(snap_dir()).expect("create snap dir");
    install_gift_sticker_source(Arc::new(CakeStickers));
    let received = crate::payments::mock::MockPaymentsBackend::new()
        .load_star_gifts()
        .expect("mock gifts")
        .received;
    let mut received = received;
    for (i, item) in received.iter_mut().enumerate() {
        if item.saved.info.sticker_document_id.0 == 0 {
            item.saved.info.sticker_document_id = tg_data::DocumentId::from_tl(2_000 + i as i64);
        }
    }
    let page = crate::info::backend::ProfilePeerGiftsPage {
        total_count: i32::try_from(received.len()).unwrap_or(0),
        gifts: received,
        collections: Vec::new(),
    };
    struct State {
        page: crate::info::backend::ProfilePeerGiftsPage,
        filter: tg_payments::star_gifts::PeerGiftsFilter,
        selected: Option<u64>,
    }
    let colors = tg_ui::theme::DayPalette::new();
    let mut h = Harness::builder()
        .with_size(egui::Vec2::new(392.0, 520.0))
        .with_context_setup(tg_ui::fonts::register_tdesktop_fonts)
        .wgpu()
        .build_ui_state(
            move |ui, st: &mut State| {
                ui.painter()
                    .rect_filled(ui.max_rect(), 0.0, egui::Color32::WHITE);
                let _ = crate::info::profile_tabs::PeerGiftsSection::show(
                    ui,
                    tg_app_core::stand_in::types::PeerId(1),
                    true,
                    &mut st.filter,
                    &mut st.selected,
                    &st.page,
                    &colors,
                );
            },
            State {
                page,
                filter: tg_payments::star_gifts::PeerGiftsFilter::default(),
                selected: None,
            },
        );
    for _ in 0..12 {
        h.step();
    }
    let img = h.render().expect("wgpu render");
    img.save(snap_dir().join("profile_gifts_after.png"))
        .expect("save png");
}
