//! Receive TON screen with live QR code painting (`ton://transfer/...`) and address copy controls.
//!
//! Port origin: `Telegram/SourceFiles/wallet/wallet_content.cpp` and
//! `Telegram/SourceFiles/wallet/wallet_address_label.cpp`.

use egui::{
    Color32, CornerRadius, Frame, Margin, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, Ui, Vec2,
};
use qrcode::{Color as QrColor, QrCode};
use tg_wallet::address::{TonAddress, build_transfer_link};
use tg_wallet::parse_ton_amount;

use tg_app_core::stand_in::theme::Palette;

/// Stateful controller for the Receive TON QR screen.
#[derive(Debug, Clone, Default)]
pub struct ReceiveState {
    /// Whether to display the bounceable (`EQ...`) instead of non-bounceable (`UQ...`) address.
    pub bounceable: bool,
    /// Optional requested amount in TON embedded into the QR transfer link.
    pub requested_amount: String,
    /// Optional requested comment embedded into the QR transfer link.
    pub requested_comment: String,
}

/// Outcome returned by [`render_receive`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiveOutcome {
    /// Stay on receive screen.
    None,
    /// Return to Wallet Overview.
    Back,
    /// Show toast message (e.g. copied).
    Toast(String),
}

/// Renders the Receive TON screen with a crisp vector-painted QR code.
pub fn render_receive(
    ui: &mut Ui,
    state: &mut ReceiveState,
    address: Option<TonAddress>,
    testnet: bool,
    palette: &Palette,
) -> ReceiveOutcome {
    let mut outcome = ReceiveOutcome::None;

    let Some(addr) = address else {
        ui.label(RichText::new("No wallet address available.").color(palette.window_sub_text_fg));
        if ui.button("Back").clicked() {
            outcome = ReceiveOutcome::Back;
        }
        return outcome;
    };

    let friendly = addr.to_friendly(state.bounceable, testnet);
    let amount_nano = parse_ton_amount(&state.requested_amount).ok();
    let comment_opt =
        (!state.requested_comment.trim().is_empty()).then_some(state.requested_comment.trim());
    let transfer_uri =
        build_transfer_link(&addr, state.bounceable, testnet, amount_nano, comment_opt);

    ScrollArea::vertical()
        .id_salt("tg_wallet_receive_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.label(
                RichText::new("Receive TON")
                    .size(18.0)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new("Scan this QR code or share your address to receive Toncoin.")
                    .size(12.5)
                    .color(palette.window_sub_text_fg),
            );
            ui.add_space(12.0);

            ui.vertical_centered(|ui| {
                paint_qr_code(ui, &transfer_uri, 188.0, palette.divider);
            });

            ui.add_space(12.0);
            Frame::NONE
                .fill(palette.window_bg_over)
                .stroke(Stroke::new(1.0, palette.divider))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new(&friendly)
                            .monospace()
                            .size(12.5)
                            .color(palette.window_fg),
                    );
                });

            ui.add_space(8.0);
            ui.checkbox(&mut state.bounceable, "Show bounceable address (EQ...)");

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Request amount (TON):")
                        .size(12.0)
                        .color(palette.window_sub_text_fg),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut state.requested_amount)
                        .desired_width(110.0)
                        .hint_text("optional"),
                );
            });
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Request comment:")
                        .size(12.0)
                        .color(palette.window_sub_text_fg),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut state.requested_comment)
                        .desired_width(200.0)
                        .hint_text("optional"),
                );
            });

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Back").clicked() {
                    outcome = ReceiveOutcome::Back;
                }
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new("Copy Address")
                                .strong()
                                .color(palette.window_fg_active),
                        )
                        .fill(palette.window_bg_active),
                    )
                    .clicked()
                {
                    ui.ctx().copy_text(friendly.clone());
                    outcome = ReceiveOutcome::Toast("Address copied to clipboard".to_owned());
                }
                if ui.button("Copy Payment Link").clicked() {
                    ui.ctx().copy_text(transfer_uri.clone());
                    outcome = ReceiveOutcome::Toast("Payment link copied".to_owned());
                }
            });
        });

    outcome
}

/// Renders a crisp quiet-zoned QR code matrix onto an `egui::Painter`.
pub fn paint_qr_code(ui: &mut Ui, payload: &str, size: f32, border_color: Color32) -> bool {
    let Ok(code) = QrCode::new(payload.as_bytes()) else {
        ui.label("Unable to encode QR payload");
        return false;
    };
    let width = code.width();
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(10), Color32::WHITE);
    painter.rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(1.0, border_color),
        egui::StrokeKind::Inside,
    );

    let quiet = 2_usize;
    let total_modules = width + quiet * 2;
    let module_px = size / total_modules as f32;

    for y in 0..width {
        for x in 0..width {
            if code[(x, y)] == QrColor::Dark {
                let min = Pos2::new(
                    rect.left() + (x + quiet) as f32 * module_px,
                    rect.top() + (y + quiet) as f32 * module_px,
                );
                let cell = Rect::from_min_size(min, Vec2::splat(module_px.ceil()));
                painter.rect_filled(cell, CornerRadius::ZERO, Color32::BLACK);
            }
        }
    }
    true
}
