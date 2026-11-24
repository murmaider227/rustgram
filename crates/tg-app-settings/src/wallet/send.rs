//! Send TON screen with address/link parsing, amount validation, confirmation step, and receipt.
//!
//! Port origin: `Telegram/SourceFiles/wallet/wallet_send_preparation.cpp`,
//! `Telegram/SourceFiles/wallet/wallet_amount_field.cpp`, and
//! `Telegram/SourceFiles/wallet/wallet_comment.cpp`.

use egui::{CornerRadius, Frame, Margin, RichText, ScrollArea, Stroke, Ui, Vec2};
use tg_lang::tr;
use tg_wallet::address::{TonAddress, parse_transfer_link};
use tg_wallet::{NANO_PER_TON, format_ton, parse_ton_amount};

use tg_app_core::stand_in::theme::Palette;

use super::backend::{SendQuote, SendReceipt, WalletBackend};

/// State for the Send TON flow (Form -> Confirm -> Receipt).
#[derive(Debug, Clone, Default)]
pub struct SendState {
    /// Destination address or `ton://transfer/...` link text.
    pub destination_input: String,
    /// Decimal TON amount input (e.g. `"1.5"`).
    pub amount_input: String,
    /// Optional transfer comment.
    pub comment_input: String,
    /// Optional explicit bounceable override.
    pub bounceable: bool,
    /// Quoted transfer awaiting explicit user confirmation.
    pub pending_quote: Option<SendQuote>,
    /// Receipt from the most recent submitted transfer.
    pub last_receipt: Option<SendReceipt>,
    /// Validation or signing error message.
    pub error: Option<String>,
}

impl SendState {
    /// Resets the send form back to its initial empty state.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Pre-fills the form from a `ton://transfer/...` URI if valid.
    pub fn try_apply_transfer_link(&mut self, uri: &str) -> bool {
        let Ok(link) = parse_transfer_link(uri) else {
            return false;
        };
        self.destination_input = link
            .address
            .address
            .to_friendly(link.address.bounceable, link.address.testnet);
        self.bounceable = link.address.bounceable;
        if let Some(amt) = link.amount_nano {
            self.amount_input = format_ton(amt as i64);
        }
        if let Some(text) = link.comment {
            self.comment_input = text;
        }
        self.error = None;
        true
    }
}

/// Outcome returned by [`render_send`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    /// Stay on send screen.
    None,
    /// Return to Wallet Overview.
    Back,
    /// Transfer signed and broadcast; show toast & return to Overview.
    Sent(String),
}

/// Renders the Send TON form, Confirmation box, or Broadcast Receipt.
pub fn render_send(
    ui: &mut Ui,
    state: &mut SendState,
    backend: &mut dyn WalletBackend,
    balance_nano: u64,
    usd_per_ton: f64,
    testnet: bool,
    palette: &Palette,
) -> SendOutcome {
    let mut outcome = SendOutcome::None;

    ScrollArea::vertical()
        .id_salt("tg_wallet_send_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Receipt view after successful signing & broadcast.
            if let Some(receipt) = state.last_receipt.clone() {
                ui.add_space(16.0);
                Frame::NONE
                    .fill(palette.window_bg_over)
                    .stroke(Stroke::new(1.0, palette.divider))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(16))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("Transfer Submitted")
                                .size(18.0)
                                .strong()
                                .color(palette.window_bold_fg),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(
                                "Signed external BOC was accepted by the configured provider.",
                            )
                            .size(12.5)
                            .color(palette.window_sub_text_fg),
                        );
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(format!("Message hash: {}", receipt.message_hash_hex))
                                .monospace()
                                .size(11.5)
                                .color(palette.window_fg),
                        );
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui.button("Copy Hash").clicked() {
                                ui.ctx().copy_text(receipt.message_hash_hex.clone());
                            }
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("Done")
                                            .strong()
                                            .color(palette.window_fg_active),
                                    )
                                    .fill(palette.window_bg_active),
                                )
                                .clicked()
                            {
                                state.reset();
                                outcome = SendOutcome::Back;
                            }
                        });
                    });
                return;
            }

            // Explicit Confirmation view (`Confirm & Send`).
            if let Some(quote) = state.pending_quote.clone() {
                let dest_friendly = quote.destination.to_friendly(quote.bounce, testnet);
                let total_nano = quote.amount_nano.saturating_add(quote.fee_nano);
                let usd_est = (quote.amount_nano as f64 / NANO_PER_TON as f64) * usd_per_ton;

                ui.label(
                    RichText::new("Confirm Transfer")
                        .size(18.0)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.add_space(8.0);

                Frame::NONE
                    .fill(palette.window_bg_over)
                    .stroke(Stroke::new(1.0, palette.divider))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(14))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new(format!("{} TON", format_ton(quote.amount_nano as i64)))
                                .size(24.0)
                                .strong()
                                .color(palette.window_bold_fg),
                        );
                        ui.label(
                            RichText::new(format!("≈ ${usd_est:.2} USD"))
                                .size(12.5)
                                .color(palette.window_sub_text_fg),
                        );
                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("Recipient:")
                                .size(12.0)
                                .color(palette.window_sub_text_fg),
                        );
                        ui.label(
                            RichText::new(&dest_friendly)
                                .monospace()
                                .size(12.0)
                                .color(palette.window_fg),
                        );
                        ui.add_space(6.0);
                        let gasless_covered = backend.gasless_terms().is_some_and(|terms| {
                            terms.is_eligible_for(
                                quote.amount_nano as i64,
                                &quote.destination.to_raw(),
                            )
                        });
                        if gasless_covered {
                            ui.label(
                                RichText::new(format!(
                                    "Network fee: {}",
                                    tr::lng_wallet_details_fee_free()
                                ))
                                .strong()
                                .size(12.5)
                                .color(palette.window_active_text_fg),
                            );
                        } else {
                            ui.label(
                                RichText::new(format!(
                                    "Estimated network fee: ~{} TON",
                                    format_ton(quote.fee_nano as i64)
                                ))
                                .size(12.5)
                                .color(palette.window_sub_text_fg),
                            );
                        }
                        ui.label(
                            RichText::new(format!(
                                "Total deducted: {} TON",
                                format_ton(total_nano as i64)
                            ))
                            .strong()
                            .size(13.0)
                            .color(palette.window_fg),
                        );
                        if !quote.comment.is_empty() {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(format!("Comment: {}", quote.comment))
                                    .size(12.5)
                                    .color(palette.window_fg),
                            );
                        }
                    });

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Edit").clicked() {
                        state.pending_quote = None;
                        state.error = None;
                    }
                    if ui
                        .add_sized(
                            Vec2::new(160.0, 34.0),
                            egui::Button::new(
                                RichText::new("Confirm & Send")
                                    .strong()
                                    .color(palette.window_fg_active),
                            )
                            .fill(palette.window_bg_active)
                            .corner_radius(CornerRadius::same(8)),
                        )
                        .clicked()
                    {
                        match backend.confirm_send(&quote) {
                            Ok(receipt) => {
                                state.pending_quote = None;
                                state.error = None;
                                let toast =
                                    format!("Sent {} TON", format_ton(quote.amount_nano as i64));
                                state.last_receipt = Some(receipt);
                                outcome = SendOutcome::Sent(toast);
                            }
                            Err(err) => state.error = Some(err),
                        }
                    }
                });

                if let Some(err) = &state.error {
                    ui.add_space(8.0);
                    ui.label(RichText::new(err).color(palette.attention_fg).size(12.5));
                }
                return;
            }

            // Input form view.
            ui.label(
                RichText::new("Send TON")
                    .size(18.0)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(8.0);

            ui.label(
                RichText::new("Recipient address or ton://transfer/ link:")
                    .size(12.5)
                    .color(palette.window_sub_text_fg),
            );
            let addr_resp = ui.add(
                egui::TextEdit::singleline(&mut state.destination_input)
                    .desired_width(f32::INFINITY)
                    .hint_text("UQ... / EQ... / 0:..."),
            );
            if addr_resp.changed() && state.destination_input.trim().starts_with("ton://") {
                let copy = state.destination_input.clone();
                state.try_apply_transfer_link(&copy);
            }
            if !state.destination_input.trim().is_empty() {
                match TonAddress::parse(&state.destination_input) {
                    Ok(parsed) => {
                        let tag = if parsed.bounceable {
                            "bounceable"
                        } else {
                            "non-bounceable"
                        };
                        ui.label(
                            RichText::new(format!("Valid TON address ({tag})"))
                                .size(11.5)
                                .color(palette.window_active_text_fg),
                        );
                    }
                    Err(err) => {
                        ui.label(
                            RichText::new(err.to_string())
                                .size(11.5)
                                .color(palette.attention_fg),
                        );
                    }
                }
            }

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "Amount (Available: {} TON):",
                        format_ton(balance_nano as i64)
                    ))
                    .size(12.5)
                    .color(palette.window_sub_text_fg),
                );
                if ui.small_button("Max").clicked() {
                    let max_send = balance_nano.saturating_sub(10_000_000);
                    state.amount_input = format_ton(max_send as i64);
                }
            });
            ui.add(
                egui::TextEdit::singleline(&mut state.amount_input)
                    .desired_width(f32::INFINITY)
                    .hint_text("0.00"),
            );
            if let Ok(nano) = parse_ton_amount(&state.amount_input) {
                let usd = (nano as f64 / NANO_PER_TON as f64) * usd_per_ton;
                ui.label(
                    RichText::new(format!("≈ ${usd:.2} USD"))
                        .size(11.5)
                        .color(palette.window_sub_text_fg),
                );
            }

            ui.add_space(10.0);
            ui.label(
                RichText::new(format!(
                    "Comment (optional, {}/256 bytes):",
                    state.comment_input.len()
                ))
                .size(12.5)
                .color(palette.window_sub_text_fg),
            );
            ui.add(
                egui::TextEdit::singleline(&mut state.comment_input)
                    .desired_width(f32::INFINITY)
                    .hint_text("Plain text comment"),
            );

            ui.add_space(6.0);
            ui.checkbox(&mut state.bounceable, "Bounceable message flag");

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    state.reset();
                    outcome = SendOutcome::Back;
                }
                if ui
                    .add_sized(
                        Vec2::new(150.0, 34.0),
                        egui::Button::new(
                            RichText::new("Continue")
                                .strong()
                                .color(palette.window_fg_active),
                        )
                        .fill(palette.window_bg_active)
                        .corner_radius(CornerRadius::same(8)),
                    )
                    .clicked()
                {
                    match parse_ton_amount(&state.amount_input) {
                        Ok(amount_nano) => {
                            match backend.prepare_send(
                                &state.destination_input,
                                amount_nano,
                                &state.comment_input,
                                Some(state.bounceable),
                            ) {
                                Ok(quote) => {
                                    state.pending_quote = Some(quote);
                                    state.error = None;
                                }
                                Err(err) => state.error = Some(err),
                            }
                        }
                        Err(err) => state.error = Some(err.to_string()),
                    }
                }
            });

            if let Some(err) = &state.error {
                ui.add_space(8.0);
                ui.label(RichText::new(err).color(palette.attention_fg).size(12.5));
            }
        });

    outcome
}
