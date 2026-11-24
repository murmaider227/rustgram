//! Wallet Overview: balance card, fiat valuation, quick actions, and transaction history list.
//!
//! Port origin: `Telegram/SourceFiles/wallet/wallet_content.cpp`,
//! `Telegram/SourceFiles/wallet/wallet_own_card.cpp`, and
//! `Telegram/SourceFiles/wallet/wallet_row_address_chips.cpp`.

use egui::{Color32, CornerRadius, Frame, Margin, RichText, ScrollArea, Stroke, Ui, Vec2};
use tg_lang::tr;
use tg_wallet::mtproto_api::GaslessTerms;
use tg_wallet::{NANO_PER_TON, format_ton};

use tg_app_core::stand_in::theme::Palette;

use super::backend::{
    CloudBackupStatus, WalletCardState, WalletOverview, WalletTxRecord, WalletTxStatus,
};
use super::card_editor::card_backdrop_color;

/// User action emitted by [`render_overview`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverviewAction {
    /// No action.
    None,
    /// Open Send TON screen.
    OpenSend,
    /// Open Receive QR screen.
    OpenReceive,
    /// Open TON Connect screen.
    OpenTonConnect,
    /// Open Collectibles (NFTs) & Jettons screen.
    OpenCollectibles,
    /// Open Wallet Card Editor screen (`wallet.setCardStickers`).
    OpenCardEditor,
    /// Open read-only Fiat Onramp & Swap link handoff screen.
    OpenOnramp,
    /// Open Shamir `(k, n)` recovery-phrase & Telegram Cloud Backup screen.
    OpenBackup,
    /// Trigger balance & history refresh.
    Refresh,
    /// Lock the local key vault.
    LockVault,
    /// Open Create / Import flow to replace wallet.
    OpenOnboarding,
    /// Show user toast (e.g. address copied).
    Toast(String),
}

/// Renders the Wallet Overview card and scrollable transaction history.
#[allow(clippy::too_many_arguments)]
pub fn render_overview(
    ui: &mut Ui,
    overview: &WalletOverview,
    card_state: &WalletCardState,
    cloud_backup: CloudBackupStatus,
    gasless: Option<&GaslessTerms>,
    selected_tx_id: &mut Option<String>,
    testnet: bool,
    pending_tc_count: usize,
    palette: &Palette,
) -> OverviewAction {
    let mut action = OverviewAction::None;

    let balance_str = format_ton(overview.balance_nano as i64);
    let fiat_usd = (overview.balance_nano as f64 / NANO_PER_TON as f64) * overview.usd_per_ton;
    let friendly = overview
        .address
        .map(|a| a.to_friendly(false, testnet))
        .unwrap_or_default();
    let short_addr = if friendly.len() > 16 {
        format!("{}...{}", &friendly[..8], &friendly[friendly.len() - 6..])
    } else {
        friendly.clone()
    };

    // Styled balance card (`wallet_own_card.cpp`, `walletCardRadius = 16px`).
    let card_fill = card_backdrop_color(card_state.background_address.as_deref(), palette);
    Frame::NONE
        .fill(card_fill)
        .corner_radius(CornerRadius::same(16))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let pill_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 36);
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("TON Wallet · {}", overview.version.label()))
                        .size(12.5)
                        .color(Color32::from_rgba_unmultiplied(255, 255, 255, 220)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Lock")
                                    .size(12.0)
                                    .color(palette.window_fg_active),
                            )
                            .fill(pill_fill)
                            .stroke(Stroke::NONE)
                            .corner_radius(CornerRadius::same(6)),
                        )
                        .clicked()
                    {
                        action = OverviewAction::LockVault;
                    }
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Refresh")
                                    .size(12.0)
                                    .color(palette.window_fg_active),
                            )
                            .fill(pill_fill)
                            .stroke(Stroke::NONE)
                            .corner_radius(CornerRadius::same(6)),
                        )
                        .clicked()
                    {
                        action = OverviewAction::Refresh;
                    }
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Edit Card")
                                    .size(12.0)
                                    .color(palette.window_fg_active),
                            )
                            .fill(pill_fill)
                            .stroke(Stroke::NONE)
                            .corner_radius(CornerRadius::same(6)),
                        )
                        .clicked()
                    {
                        action = OverviewAction::OpenCardEditor;
                    }
                });
            });
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!("{balance_str} TON"))
                    .size(28.0)
                    .strong()
                    .color(palette.window_fg_active),
            );
            ui.label(
                RichText::new(format!("≈ ${fiat_usd:.2} USD"))
                    .size(13.0)
                    .color(Color32::from_rgba_unmultiplied(255, 255, 255, 208)),
            );
            if !card_state.stickers.is_empty() {
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    for sticker in card_state.stickers.iter().take(4) {
                        Frame::NONE
                            .fill(pill_fill)
                            .corner_radius(CornerRadius::same(10))
                            .inner_margin(Margin::symmetric(8, 3))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(format!("* {}", sticker.title))
                                        .size(11.0)
                                        .color(palette.window_fg_active),
                                );
                            });
                    }
                });
            }
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(&short_addr)
                        .monospace()
                        .size(12.0)
                        .color(Color32::from_rgba_unmultiplied(255, 255, 255, 225)),
                );
                if !friendly.is_empty()
                    && ui
                        .add(
                            egui::Button::new(
                                RichText::new("Copy")
                                    .size(11.5)
                                    .color(palette.window_fg_active),
                            )
                            .fill(pill_fill)
                            .stroke(Stroke::NONE)
                            .corner_radius(CornerRadius::same(6)),
                        )
                        .clicked()
                {
                    ui.ctx().copy_text(friendly.clone());
                    action = OverviewAction::Toast("Wallet address copied".to_owned());
                }
                if cloud_backup.backup_enabled {
                    Frame::NONE
                        .fill(pill_fill)
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::symmetric(8, 2))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new("Cloud Backup On")
                                    .size(11.0)
                                    .color(palette.window_fg_active),
                            );
                        });
                }
                if let Some(terms) = gasless
                    && terms.is_usable()
                {
                    Frame::NONE
                        .fill(pill_fill)
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::symmetric(8, 2))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(format!("{} free today", terms.info.left))
                                    .size(11.0)
                                    .color(palette.window_fg_active),
                            );
                        });
                }
            });
        });

    if let Some(terms) = gasless
        && terms.is_usable()
    {
        ui.add_space(6.0);
        ui.add(
            egui::Label::new(
                RichText::new(tr::lng_wallet_about_fees_text(i64::from(terms.info.left)))
                    .size(11.5)
                    .color(palette.window_sub_text_fg),
            )
            .wrap(),
        );
    }

    ui.add_space(10.0);

    // Quick action bar (wraps cleanly on narrow 390px viewports).
    ui.horizontal_wrapped(|ui| {
        if ui
            .add(
                egui::Button::new(
                    RichText::new("Send")
                        .strong()
                        .color(palette.window_fg_active),
                )
                .fill(palette.window_bg_active)
                .stroke(Stroke::NONE)
                .corner_radius(CornerRadius::same(8))
                .min_size(Vec2::new(84.0, 34.0)),
            )
            .clicked()
        {
            action = OverviewAction::OpenSend;
        }
        if ui
            .add(
                egui::Button::new(RichText::new("Receive").color(palette.window_fg))
                    .fill(palette.window_bg_over)
                    .corner_radius(CornerRadius::same(8))
                    .min_size(Vec2::new(84.0, 34.0)),
            )
            .clicked()
        {
            action = OverviewAction::OpenReceive;
        }
        let tc_label = if pending_tc_count > 0 {
            format!("TON Connect ({pending_tc_count})")
        } else {
            "TON Connect".to_owned()
        };
        if ui
            .add(
                egui::Button::new(RichText::new(tc_label).color(palette.window_fg))
                    .fill(palette.window_bg_over)
                    .corner_radius(CornerRadius::same(8))
                    .min_size(Vec2::new(112.0, 34.0)),
            )
            .clicked()
        {
            action = OverviewAction::OpenTonConnect;
        }
        let backup_label = if cloud_backup.backup_enabled {
            "Backup (Cloud On)"
        } else {
            "Backup"
        };
        if ui
            .add(
                egui::Button::new(RichText::new(backup_label).color(palette.window_fg))
                    .fill(palette.window_bg_over)
                    .corner_radius(CornerRadius::same(8))
                    .min_size(Vec2::new(96.0, 34.0)),
            )
            .clicked()
        {
            action = OverviewAction::OpenBackup;
        }
        if ui
            .add(
                egui::Button::new(RichText::new("Collectibles").color(palette.window_fg))
                    .fill(palette.window_bg_over)
                    .corner_radius(CornerRadius::same(8))
                    .min_size(Vec2::new(96.0, 34.0)),
            )
            .clicked()
        {
            action = OverviewAction::OpenCollectibles;
        }
        if ui
            .add(
                egui::Button::new(RichText::new("Card Editor").color(palette.window_fg))
                    .fill(palette.window_bg_over)
                    .corner_radius(CornerRadius::same(8))
                    .min_size(Vec2::new(96.0, 34.0)),
            )
            .clicked()
        {
            action = OverviewAction::OpenCardEditor;
        }
        if ui
            .add(
                egui::Button::new(RichText::new("Buy / Swap").color(palette.window_fg))
                    .fill(palette.window_bg_over)
                    .corner_radius(CornerRadius::same(8))
                    .min_size(Vec2::new(92.0, 34.0)),
            )
            .clicked()
        {
            action = OverviewAction::OpenOnramp;
        }
        if ui
            .add(
                egui::Button::new(RichText::new("Setup").color(palette.window_sub_text_fg))
                    .fill(palette.window_bg_over)
                    .corner_radius(CornerRadius::same(8))
                    .min_size(Vec2::new(68.0, 34.0)),
            )
            .clicked()
        {
            action = OverviewAction::OpenOnboarding;
        }
    });

    ui.add_space(12.0);
    ui.label(
        RichText::new("Transaction History")
            .size(14.5)
            .strong()
            .color(palette.window_bold_fg),
    );
    ui.add_space(4.0);

    ScrollArea::vertical()
        .id_salt("tg_wallet_history_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if overview.history.is_empty() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("No transactions yet")
                            .size(13.5)
                            .color(palette.window_sub_text_fg),
                    );
                });
                return;
            }

            for tx in &overview.history {
                render_tx_row(ui, tx, selected_tx_id, palette);
                ui.add_space(6.0);
            }
        });

    action
}

fn render_tx_row(
    ui: &mut Ui,
    tx: &WalletTxRecord,
    selected_tx_id: &mut Option<String>,
    palette: &Palette,
) {
    let is_selected = selected_tx_id.as_deref() == Some(tx.id.as_str());
    let fill = if is_selected {
        palette.dialogs_bg_active
    } else {
        palette.window_bg_over
    };
    let sign = if tx.incoming { "+" } else { "-" };
    let amount_color = if tx.status == WalletTxStatus::Failure {
        palette.attention_fg
    } else if tx.incoming {
        Color32::from_rgb(0x2E, 0x9B, 0x4F)
    } else {
        palette.window_bold_fg
    };
    let peer_display = if !tx.counterparty_label.is_empty() {
        tx.counterparty_label.clone()
    } else if tx.counterparty_address.len() > 16 {
        format!(
            "{}...{}",
            &tx.counterparty_address[..8],
            &tx.counterparty_address[tx.counterparty_address.len() - 6..]
        )
    } else {
        tx.counterparty_address.clone()
    };

    let resp = Frame::NONE
        .fill(fill)
        .stroke(Stroke::new(1.0, palette.divider))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    let dir = if tx.incoming { "Received" } else { "Sent" };
                    ui.label(
                        RichText::new(format!("{dir} · {peer_display}"))
                            .strong()
                            .size(13.5)
                            .color(palette.window_bold_fg),
                    );
                    if !tx.comment.is_empty() {
                        ui.label(
                            RichText::new(&tx.comment)
                                .size(12.0)
                                .color(palette.window_sub_text_fg),
                        );
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{sign}{} TON", format_ton(tx.amount_nano as i64)))
                            .strong()
                            .size(14.0)
                            .color(amount_color),
                    );
                });
            });

            if is_selected {
                ui.add_space(4.0);
                ui.separator();
                ui.label(
                    RichText::new(format!("Address: {}", tx.counterparty_address))
                        .monospace()
                        .size(11.0)
                        .color(palette.window_sub_text_fg),
                );
                if let Some(fee) = tx.fee_nano {
                    ui.label(
                        RichText::new(format!("Network fee: {} TON", format_ton(fee as i64)))
                            .size(11.5)
                            .color(palette.window_sub_text_fg),
                    );
                }
                let status_label = match tx.status {
                    WalletTxStatus::Success => "Confirmed",
                    WalletTxStatus::Pending => "Pending",
                    WalletTxStatus::Failure => "Failed",
                };
                ui.label(
                    RichText::new(format!("Status: {status_label} · Hash: {}", tx.id))
                        .size(11.0)
                        .color(palette.window_sub_text_fg),
                );
            }
        })
        .response
        .interact(egui::Sense::click());

    if resp.clicked() {
        if is_selected {
            *selected_tx_id = None;
        } else {
            *selected_tx_id = Some(tx.id.clone());
        }
    }
}
