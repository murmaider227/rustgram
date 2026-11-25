//! TON Connect 2.0 hub, Connect Approval Box, SendTransaction Emulation Preview Box,
//! and SignData Confirmation Box.
//!
//! Port origin: `Telegram/SourceFiles/wallet/wallet_ton_connect_box.cpp` and
//! `Telegram/SourceFiles/wallet/wallet_ton_connect_request_box.cpp`.

use egui::{Color32, CornerRadius, Frame, Label, Margin, RichText, ScrollArea, Stroke, Ui, Vec2};
use tg_lang::tr;
use tg_wallet::emulate::{
    EmulationResult, EmulationWarning, TonConnectActionKind, TonConnectEmulation,
    TonConnectEmulationStatus,
};
use tg_wallet::format_ton;
use tg_wallet::jettons::format_jetton_amount;

use tg_app_core::stand_in::theme::Palette;

use super::backend::{PendingSignDataApproval, PendingTxApproval, WalletBackend};

/// Stateful controller for the TON Connect screen.
#[derive(Debug, Clone, Default)]
pub struct TonConnectUiState {
    /// Pasted `tc://?...` or `tonconnect-...` link input.
    pub link_input: String,
    /// Last validation or signing error message.
    pub error: Option<String>,
    /// `(request_id, content_hash)` of the `sendTransaction` request last rendered to the user.
    pub shown_tx_target: Option<(String, String)>,
    /// `(request_id, content_hash)` of the `signData` request last rendered to the user.
    pub shown_sign_data_target: Option<(String, String)>,
}

/// Outcome returned by [`render_ton_connect`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TonConnectOutcome {
    /// Stay on TON Connect screen.
    None,
    /// Return to Wallet Overview.
    Back,
    /// Emit toast notification.
    Toast(String),
}

fn is_dark_palette(palette: &Palette) -> bool {
    let luma = u16::from(palette.window_bg.r())
        + u16::from(palette.window_bg.g())
        + u16::from(palette.window_bg.b());
    luma < 384
}

fn positive_color(palette: &Palette) -> Color32 {
    if is_dark_palette(palette) {
        Color32::from_rgb(0x52, 0xC8, 0x7A)
    } else {
        Color32::from_rgb(0x1E, 0x82, 0x42)
    }
}

fn warning_fill(palette: &Palette) -> Color32 {
    if is_dark_palette(palette) {
        Color32::from_rgb(0x36, 0x26, 0x1A)
    } else {
        Color32::from_rgb(0xFF, 0xF5, 0xE6)
    }
}

fn warning_stroke(palette: &Palette) -> Color32 {
    if is_dark_palette(palette) {
        Color32::from_rgb(0xC9, 0x82, 0x2E)
    } else {
        Color32::from_rgb(0xD9, 0x77, 0x06)
    }
}

fn short_addr(addr: &str) -> String {
    if addr.len() > 18 {
        format!("{}...{}", &addr[..8], &addr[addr.len() - 6..])
    } else {
        addr.to_owned()
    }
}

fn format_signed_ton_delta(delta_nano: i128) -> String {
    let abs = delta_nano.unsigned_abs().min(i64::MAX as u128) as i64;
    let formatted = format_ton(abs);
    if delta_nano > 0 {
        format!("+{formatted} TON")
    } else if delta_nano < 0 {
        format!("-{formatted} TON")
    } else {
        "0 TON".to_owned()
    }
}

fn format_emulation_warning(warning: &EmulationWarning) -> String {
    match warning {
        EmulationWarning::TransactionAborted { account, .. } => {
            format!("Transaction aborted on {}", short_addr(account))
        }
        EmulationWarning::ComputePhaseFailed {
            account, exit_code, ..
        } => {
            format!(
                "Compute phase failed on {} (exit code {exit_code})",
                short_addr(account)
            )
        }
        EmulationWarning::ActionPhaseFailed {
            account,
            result_code,
            ..
        } => {
            format!(
                "Action phase failed on {} (result code {result_code})",
                short_addr(account)
            )
        }
        EmulationWarning::MessageBounced {
            destination,
            value_nano,
            ..
        } => {
            let dest = destination
                .as_deref()
                .map(short_addr)
                .unwrap_or_else(|| "contract".to_owned());
            format!(
                "Message of {} TON bounced from {dest}",
                format_ton(*value_nano as i64)
            )
        }
        EmulationWarning::ActionFailed { action_type, .. } => {
            format!("Emulated action failed: {action_type}")
        }
        EmulationWarning::TraceIncomplete => tr::lng_wallet_connect_request_trace_incomplete(),
    }
}

/// Renders the TON Connect hub including pending Connect, SendTransaction (with emulation preview),
/// and SignData confirmation boxes, plus the list of connected dApps with Disconnect controls.
pub fn render_ton_connect(
    ui: &mut Ui,
    state: &mut TonConnectUiState,
    backend: &mut dyn WalletBackend,
    palette: &Palette,
) -> TonConnectOutcome {
    let mut outcome = TonConnectOutcome::None;
    let wallet_raw = backend
        .overview()
        .address
        .map(|a| a.to_raw())
        .unwrap_or_default();

    ScrollArea::vertical()
        .id_salt("tg_wallet_ton_connect_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("TON Connect")
                        .size(18.0)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Back").clicked() {
                        outcome = TonConnectOutcome::Back;
                    }
                });
            });
            ui.add_space(8.0);

            // 1. Pending Connect Approval Box (`wallet_ton_connect_box.cpp`).
            if let Some(pending) = backend.pending_connect() {
                Frame::NONE
                    .fill(palette.window_bg_over)
                    .stroke(Stroke::new(1.5, palette.window_bg_active))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(14))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.add(
                            Label::new(
                                RichText::new(format!("Connect to {}?", pending.manifest.name))
                                    .size(15.5)
                                    .strong()
                                    .color(palette.window_bold_fg),
                            )
                            .wrap(),
                        );
                        ui.label(
                            RichText::new(format!("Domain: {}", pending.manifest.host()))
                                .size(12.0)
                                .color(palette.window_active_text_fg),
                        );
                        ui.add_space(4.0);
                        let perms = if pending.requests_proof {
                            "Requests: Wallet address & cryptographic ownership proof (ton_proof)"
                        } else {
                            "Requests: Wallet address (ton_addr)"
                        };
                        ui.add(
                            Label::new(
                                RichText::new(perms)
                                    .size(12.0)
                                    .color(palette.window_sub_text_fg),
                            )
                            .wrap(),
                        );
                        ui.add_space(10.0);
                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("Connect")
                                            .strong()
                                            .color(palette.window_fg_active),
                                    )
                                    .fill(palette.window_bg_active)
                                    .stroke(Stroke::NONE)
                                    .corner_radius(CornerRadius::same(6))
                                    .min_size(Vec2::new(120.0, 32.0)),
                                )
                                .clicked()
                            {
                                match backend.approve_ton_connect() {
                                    Ok(card) => {
                                        state.error = None;
                                        outcome = TonConnectOutcome::Toast(format!(
                                            "Connected to {}",
                                            card.manifest.name
                                        ));
                                    }
                                    Err(err) => state.error = Some(err),
                                }
                            }
                            if ui
                                .add(
                                    egui::Button::new(tr::lng_cancel())
                                        .min_size(Vec2::new(80.0, 32.0)),
                                )
                                .clicked()
                            {
                                backend.decline_ton_connect();
                                outcome =
                                    TonConnectOutcome::Toast("Connection declined".to_owned());
                            }
                        });
                    });
                ui.add_space(10.0);
            }

            // 2. Pending SendTransaction Approval Box (`wallet_ton_connect_request_box.cpp`)
            //    with Toncenter `emulate_message` preview (balance deltas, fees, warnings).
            if let Some(pending_tx) = backend.pending_tx_approval() {
                render_pending_tx_box(
                    ui,
                    &pending_tx,
                    &wallet_raw,
                    state,
                    backend,
                    palette,
                    &mut outcome,
                );
                ui.add_space(10.0);
            } else {
                state.shown_tx_target = None;
            }

            // 3. Pending SignData Confirmation Box (`wallet_ton_connect_request_box.cpp:737-900`).
            if let Some(pending_sd) = backend.pending_sign_data() {
                render_pending_sign_data_box(
                    ui,
                    &pending_sd,
                    state,
                    backend,
                    palette,
                    &mut outcome,
                );
                ui.add_space(10.0);
            } else {
                state.shown_sign_data_target = None;
            }

            // 4. Connect a new dApp via tc:// link.
            ui.add(
                Label::new(
                    RichText::new("Connect a dApp (paste tc:// or tonconnect- link):")
                        .size(12.5)
                        .color(palette.window_sub_text_fg),
                )
                .wrap(),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut state.link_input)
                        .desired_width((ui.available_width() - 88.0).max(140.0))
                        .hint_text("tc://?v=2&id=...&r=..."),
                );
                if ui.button("Connect").clicked() {
                    match backend.handle_ton_connect_link(&state.link_input) {
                        Ok(()) => {
                            state.link_input.clear();
                            state.error = None;
                        }
                        Err(err) => state.error = Some(err),
                    }
                }
            });

            if let Some(err) = &state.error {
                ui.add_space(6.0);
                ui.add(
                    Label::new(
                        RichText::new(format!("! {err}"))
                            .color(palette.attention_fg)
                            .size(12.0),
                    )
                    .wrap(),
                );
            }

            // 5. Connected dApps list with Disconnect buttons.
            ui.add_space(14.0);
            ui.label(
                RichText::new("Connected Apps")
                    .size(14.5)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(6.0);

            let sessions = backend.ton_connect_sessions();
            if sessions.is_empty() {
                ui.label(
                    RichText::new("No connected TON Connect apps.")
                        .size(12.5)
                        .color(palette.window_sub_text_fg),
                );
            } else {
                for sess in sessions {
                    Frame::NONE
                        .fill(palette.window_bg_over)
                        .stroke(Stroke::new(1.0, palette.divider))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(
                                        RichText::new(&sess.manifest.name)
                                            .strong()
                                            .size(13.5)
                                            .color(palette.window_bold_fg),
                                    );
                                    ui.label(
                                        RichText::new(sess.manifest.host())
                                            .size(11.5)
                                            .color(palette.window_sub_text_fg),
                                    );
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .button(
                                                RichText::new("Disconnect")
                                                    .color(palette.attention_fg),
                                            )
                                            .clicked()
                                        {
                                            if let Err(err) =
                                                backend.disconnect_ton_connect(sess.session_id)
                                            {
                                                state.error = Some(err);
                                            } else {
                                                outcome = TonConnectOutcome::Toast(format!(
                                                    "Disconnected {}",
                                                    sess.manifest.name
                                                ));
                                            }
                                        }
                                    },
                                );
                            });
                        });
                    ui.add_space(6.0);
                }
            }
        });

    outcome
}

fn render_pending_tx_box(
    ui: &mut Ui,
    pending_tx: &PendingTxApproval,
    wallet_raw: &str,
    state: &mut TonConnectUiState,
    backend: &mut dyn WalletBackend,
    palette: &Palette,
    outcome: &mut TonConnectOutcome,
) {
    let has_emul_risk = pending_tx
        .emulation
        .as_ref()
        .is_some_and(|e| !e.trace_succeeded || e.is_incomplete || !e.warnings.is_empty());
    let border_color = if has_emul_risk {
        warning_stroke(palette)
    } else {
        palette.window_bg_active
    };

    Frame::NONE
        .fill(palette.window_bg_over)
        .stroke(Stroke::new(1.5, border_color))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                Label::new(
                    RichText::new(format!("Transaction Request · {}", pending_tx.dapp_name))
                        .size(15.5)
                        .strong()
                        .color(palette.window_bold_fg),
                )
                .wrap(),
            );
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("Origin: {}", pending_tx.dapp_domain))
                        .size(12.0)
                        .color(palette.window_active_text_fg),
                );
                if let Some(emul) = &pending_tx.emulation {
                    let (badge_text, badge_color) = if !emul.trace_succeeded {
                        ("! Emulation Failed", palette.attention_fg)
                    } else if emul.is_incomplete || !emul.warnings.is_empty() {
                        ("! Warnings", warning_stroke(palette))
                    } else {
                        ("Emulated", positive_color(palette))
                    };
                    Frame::NONE
                        .fill(palette.window_bg)
                        .stroke(Stroke::new(1.0, badge_color))
                        .corner_radius(CornerRadius::same(5))
                        .inner_margin(Margin::symmetric(7, 1))
                        .show(ui, |ui| {
                            ui.add(
                                Label::new(
                                    RichText::new(badge_text)
                                        .size(11.0)
                                        .strong()
                                        .color(badge_color),
                                )
                                .extend(),
                            );
                        });
                }
            });
            ui.add_space(6.0);

            // Emulation preview panel (`ToncenterApi::emulate_message` deltas, fees, warnings).
            if let Some(emul) = &pending_tx.emulation {
                render_emulation_preview(
                    ui,
                    emul,
                    pending_tx.ton_connect_emulation.as_ref(),
                    wallet_raw,
                    palette,
                );
                ui.add_space(8.0);
            }

            // Raw requested transfer messages summary.
            for (idx, msg) in pending_tx.request.messages.iter().enumerate() {
                ui.add(
                    Label::new(
                        RichText::new(format!(
                            "#{}: {} TON -> {}",
                            idx + 1,
                            format_ton(msg.amount_nano as i64),
                            short_addr(&msg.address)
                        ))
                        .monospace()
                        .size(11.5)
                        .color(palette.window_fg),
                    )
                    .wrap(),
                );
                if let Some(comment) = &msg.decoded_comment {
                    ui.add(
                        Label::new(
                            RichText::new(format!("Comment: {comment}"))
                                .size(12.0)
                                .color(palette.window_sub_text_fg),
                        )
                        .wrap(),
                    );
                }
            }
            ui.add_space(6.0);
            ui.add(
                Label::new(
                    RichText::new(format!(
                        "Total: {} TON  (+ ~{} TON fee)",
                        format_ton(pending_tx.request.total_nano() as i64),
                        format_ton(pending_tx.estimated_fee_nano as i64)
                    ))
                    .strong()
                    .size(12.5)
                    .color(palette.window_bold_fg),
                )
                .wrap(),
            );

            ui.add_space(10.0);
            let current_target = (
                pending_tx.request.request_id.clone(),
                pending_tx.content_hash(),
            );
            let target_for_click = state
                .shown_tx_target
                .clone()
                .unwrap_or_else(|| current_target.clone());
            state.shown_tx_target = Some(current_target);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new("Approve & Sign")
                                .strong()
                                .color(palette.window_fg_active),
                        )
                        .fill(palette.window_bg_active)
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(6))
                        .min_size(Vec2::new(140.0, 32.0)),
                    )
                    .clicked()
                {
                    match backend.approve_ton_connect_tx(&target_for_click.0, &target_for_click.1) {
                        Ok(receipt) => {
                            state.error = None;
                            state.shown_tx_target = None;
                            *outcome = TonConnectOutcome::Toast(format!(
                                "Signed tx {}",
                                &receipt.message_hash_hex[..12.min(receipt.message_hash_hex.len())]
                            ));
                        }
                        Err(err) => state.error = Some(err),
                    }
                }
                if ui
                    .add(egui::Button::new("Decline").min_size(Vec2::new(84.0, 32.0)))
                    .clicked()
                {
                    backend.decline_ton_connect_tx();
                    state.shown_tx_target = None;
                    *outcome = TonConnectOutcome::Toast("Transaction declined".to_owned());
                }
            });
        });
}

fn render_emulation_preview(
    ui: &mut Ui,
    emul: &EmulationResult,
    tc_emul: Option<&TonConnectEmulation>,
    wallet_raw: &str,
    palette: &Palette,
) {
    let ok_color = positive_color(palette);

    Frame::NONE
        .fill(palette.window_bg)
        .stroke(Stroke::new(1.0, palette.divider))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new("Emulated Balance Impact")
                    .size(12.5)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(4.0);

            // Warnings banner (trace failed / incomplete / aborted / bounced / compute or action failure).
            let show_failed_banner = !emul.trace_succeeded
                || tc_emul.is_some_and(|t| {
                    matches!(
                        t.status,
                        TonConnectEmulationStatus::Failed | TonConnectEmulationStatus::Aborted
                    )
                });
            if show_failed_banner || emul.is_incomplete || !emul.warnings.is_empty() {
                let warn_border = if show_failed_banner {
                    palette.attention_fg
                } else {
                    warning_stroke(palette)
                };
                Frame::NONE
                    .fill(warning_fill(palette))
                    .stroke(Stroke::new(1.0, warn_border))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        if show_failed_banner {
                            ui.add(
                                Label::new(
                                    RichText::new(format!(
                                        "! {}",
                                        tr::lng_wallet_connect_request_trace_failed()
                                    ))
                                    .size(11.5)
                                    .strong()
                                    .color(palette.attention_fg),
                                )
                                .wrap(),
                            );
                        }
                        for w in &emul.warnings {
                            ui.add(
                                Label::new(
                                    RichText::new(format!("• {}", format_emulation_warning(w)))
                                        .size(11.5)
                                        .color(palette.window_bold_fg),
                                )
                                .wrap(),
                            );
                        }
                    });
                ui.add_space(6.0);
            }

            // Per-wallet balance deltas (TON, Jettons, NFTs).
            let wallet_delta = if !wallet_raw.is_empty() {
                emul.delta_for(wallet_raw)
            } else {
                None
            }
            .or_else(|| emul.account_deltas.first());

            if let Some(delta) = wallet_delta {
                let ton_color = if delta.ton_delta_nano > 0 {
                    ok_color
                } else if delta.ton_delta_nano < 0 {
                    palette.attention_fg
                } else {
                    palette.window_sub_text_fg
                };
                let ton_dir = if delta.ton_delta_nano > 0 {
                    "↑"
                } else if delta.ton_delta_nano < 0 {
                    "↓"
                } else {
                    "•"
                };
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("TON")
                            .size(12.0)
                            .color(palette.window_sub_text_fg),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!(
                                "{ton_dir} {}",
                                format_signed_ton_delta(delta.ton_delta_nano)
                            ))
                            .monospace()
                            .strong()
                            .size(12.5)
                            .color(ton_color),
                        );
                    });
                });

                for jetton in &delta.jetton_deltas {
                    let sym = jetton.symbol.as_deref().unwrap_or("Jetton");
                    let decimals = jetton.decimals.unwrap_or(9);
                    let mag = format_jetton_amount(jetton.delta.unsigned_abs(), decimals);
                    let (dir, sign, color) = if jetton.delta >= 0 {
                        ("↑", "+", ok_color)
                    } else {
                        ("↓", "-", palette.attention_fg)
                    };
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(sym)
                                .size(12.0)
                                .color(palette.window_sub_text_fg),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(format!("{dir} {sign}{mag} {sym}"))
                                    .monospace()
                                    .strong()
                                    .size(12.5)
                                    .color(color),
                            );
                        });
                    });
                }

                for nft in &delta.nft_deltas {
                    let (dir, text, color) = if nft.delta > 0 {
                        (
                            "↑",
                            format!("Receive NFT {}", short_addr(&nft.nft_address)),
                            ok_color,
                        )
                    } else {
                        (
                            "↓",
                            format!("Send NFT {}", short_addr(&nft.nft_address)),
                            palette.attention_fg,
                        )
                    };
                    ui.add(
                        Label::new(
                            RichText::new(format!("{dir} {text}"))
                                .monospace()
                                .size(11.5)
                                .color(color),
                        )
                        .wrap(),
                    );
                }
            }

            // Classified actions summary.
            if let Some(tc) = tc_emul
                && !tc.actions.is_empty()
            {
                ui.add_space(4.0);
                ui.separator();
                ui.add_space(2.0);
                for act in &tc.actions {
                    let kind_label = match act.kind {
                        TonConnectActionKind::Withdraw => "Withdraw",
                        TonConnectActionKind::Deposit => "Deposit",
                        TonConnectActionKind::Transfer => "Transfer",
                        TonConnectActionKind::Excess => "Gas refund",
                        TonConnectActionKind::CallContract => "Contract call",
                        TonConnectActionKind::DeployContract => "Contract deploy",
                        TonConnectActionKind::Unknown => "Contract action",
                    };
                    let detail = match act.amount_nano {
                        Some(nano) => format!(
                            "{kind_label}: {} TON ({})",
                            format_ton(nano.abs()),
                            short_addr(&act.counterparty)
                        ),
                        None => format!("{kind_label} ({})", short_addr(&act.counterparty)),
                    };
                    ui.add(
                        Label::new(
                            RichText::new(detail)
                                .size(11.5)
                                .color(palette.window_sub_text_fg),
                        )
                        .wrap(),
                    );
                }
            }

            // Fee breakdown (`total_fees_nano`, `gas`, `storage`, `fwd`).
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(2.0);
            let mut fee_text = format!(
                "Estimated network fee: {} TON",
                format_ton(emul.total_fees_nano as i64)
            );
            if emul.total_gas_fees_nano > 0
                || emul.total_storage_fees_nano > 0
                || emul.total_fwd_fees_nano > 0
            {
                fee_text.push_str(&format!(
                    "  (gas {} · storage {} · fwd {})",
                    format_ton(emul.total_gas_fees_nano as i64),
                    format_ton(emul.total_storage_fees_nano as i64),
                    format_ton(emul.total_fwd_fees_nano as i64)
                ));
            }
            ui.add(
                Label::new(
                    RichText::new(fee_text)
                        .monospace()
                        .size(11.0)
                        .color(palette.window_sub_text_fg),
                )
                .wrap(),
            );
        });
}

fn render_pending_sign_data_box(
    ui: &mut Ui,
    pending: &PendingSignDataApproval,
    state: &mut TonConnectUiState,
    backend: &mut dyn WalletBackend,
    palette: &Palette,
    outcome: &mut TonConnectOutcome,
) {
    let is_blind = matches!(pending.request.kind.as_str(), "binary" | "cell");
    let border_color = if is_blind {
        warning_stroke(palette)
    } else {
        palette.window_bg_active
    };

    Frame::NONE
        .fill(palette.window_bg_over)
        .stroke(Stroke::new(1.5, border_color))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{} · {}",
                        tr::lng_wallet_connect_sign_title(),
                        pending.dapp_name
                    ))
                    .size(15.5)
                    .strong()
                    .color(palette.window_bold_fg),
                );
                let kind_badge = match pending.request.kind.as_str() {
                    "text" => tr::lng_wallet_connect_sign_data(),
                    "binary" => tr::lng_wallet_connect_sign_binary(),
                    "cell" => "Cell (BOC)".to_owned(),
                    other => other.to_owned(),
                };
                Frame::NONE
                    .fill(palette.window_bg)
                    .stroke(Stroke::new(1.0, border_color))
                    .corner_radius(CornerRadius::same(5))
                    .inner_margin(Margin::symmetric(7, 2))
                    .show(ui, |ui| {
                        ui.add(
                            Label::new(RichText::new(kind_badge).size(11.0).strong().color(
                                if is_blind {
                                    warning_stroke(palette)
                                } else {
                                    palette.window_active_text_fg
                                },
                            ))
                            .extend(),
                        );
                    });
            });
            ui.label(
                RichText::new(format!("Origin: {}", pending.dapp_domain))
                    .size(12.0)
                    .color(palette.window_active_text_fg),
            );
            ui.add_space(6.0);

            if is_blind {
                Frame::NONE
                    .fill(warning_fill(palette))
                    .stroke(Stroke::new(1.0, warning_stroke(palette)))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.add(
                            Label::new(
                                RichText::new(format!("! {}", tr::lng_wallet_connect_sign_blind()))
                                    .size(11.5)
                                    .color(palette.window_bold_fg),
                            )
                            .wrap(),
                        );
                    });
                ui.add_space(6.0);
            } else {
                ui.add(
                    Label::new(
                        RichText::new(tr::lng_wallet_connect_sign_text_about())
                            .size(12.0)
                            .color(palette.window_sub_text_fg),
                    )
                    .wrap(),
                );
                ui.add_space(4.0);
            }

            if let Some(schema) = &pending.request.schema
                && !schema.is_empty()
            {
                ui.add(
                    Label::new(
                        RichText::new(format!("Schema: {schema}"))
                            .monospace()
                            .size(11.5)
                            .color(palette.window_sub_text_fg),
                    )
                    .wrap(),
                );
                ui.add_space(4.0);
            }

            Frame::NONE
                .fill(palette.window_bg)
                .stroke(Stroke::new(1.0, palette.divider))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(
                        Label::new(
                            RichText::new(&pending.request.payload)
                                .monospace()
                                .size(11.5)
                                .color(palette.window_fg),
                        )
                        .wrap(),
                    );
                });

            ui.add_space(10.0);
            let current_target = (pending.request.request_id.clone(), pending.content_hash());
            let target_for_click = state
                .shown_sign_data_target
                .clone()
                .unwrap_or_else(|| current_target.clone());
            state.shown_sign_data_target = Some(current_target);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(tr::lng_wallet_connect_sign_button())
                                .strong()
                                .color(palette.window_fg_active),
                        )
                        .fill(palette.window_bg_active)
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(6))
                        .min_size(Vec2::new(132.0, 32.0)),
                    )
                    .clicked()
                {
                    match backend
                        .approve_ton_connect_sign_data(&target_for_click.0, &target_for_click.1)
                    {
                        Ok(res) => {
                            state.error = None;
                            state.shown_sign_data_target = None;
                            *outcome = TonConnectOutcome::Toast(format!(
                                "{} ({})",
                                tr::lng_wallet_connect_sign_done(),
                                res.domain
                            ));
                        }
                        Err(err) => state.error = Some(err),
                    }
                }
                if ui
                    .add(egui::Button::new(tr::lng_cancel()).min_size(Vec2::new(84.0, 32.0)))
                    .clicked()
                {
                    backend.decline_ton_connect_sign_data();
                    state.shown_sign_data_target = None;
                    *outcome = TonConnectOutcome::Toast("Sign data request declined".to_owned());
                }
                if ui
                    .add(egui::Button::new("Copy Payload").min_size(Vec2::new(0.0, 32.0)))
                    .clicked()
                {
                    ui.ctx().copy_text(pending.request.payload.clone());
                    *outcome = TonConnectOutcome::Toast("Payload copied".to_owned());
                }
            });
        });
}
