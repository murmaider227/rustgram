//! TON Wallet Collectibles (NFTs) gallery & transfer UI, plus TEP-74 Jetton balances & transfer UI.
//!
//! Port origin: `Telegram/SourceFiles/wallet/wallet_collectibles.cpp`,
//! `Telegram/SourceFiles/wallet/wallet_collectibles_viewport.cpp`,
//! and `Telegram/SourceFiles/wallet/wallet_collectible_record.cpp`.
//!
//! All transfers require explicit user review & confirmation and are signed through the active
//! [`WalletBackend`]; no real funds or real network requests are ever triggered in tests.

use egui::{CornerRadius, Frame, Margin, RichText, ScrollArea, Stroke, Ui, Vec2};
use tg_wallet::format_ton;
use tg_wallet::jettons::{JettonWalletBalance, format_jetton_amount, parse_jetton_amount};
use tg_wallet::toncenter::{NftItem, NftKind};

use tg_app_core::stand_in::theme::Palette;

use super::backend::{JettonTransferQuote, NftTransferQuote, SendReceipt, WalletBackend};
use super::card_editor::{MoveToGiftsOutcome, MoveToGiftsState, render_move_to_gifts_sheet};

/// Active sub-tab inside the Wallet Assets screen (`Collectibles` vs `Jettons`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssetsTab {
    /// NFT Collectibles gallery & TEP-62 transfer flow.
    #[default]
    Collectibles,
    /// TEP-74 Jetton balances & transfer flow.
    Jettons,
}

/// Filter chips for the Collectibles gallery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NftKindFilter {
    /// Show all NFT collectibles.
    #[default]
    All,
    /// Telegram collectible gifts (`NftKind::TelegramGift`).
    Gifts,
    /// Fragment collectible usernames (`NftKind::TelegramUsername`).
    Usernames,
    /// Fragment anonymous phone numbers (`NftKind::TelegramNumber`).
    Numbers,
    /// Generic TON NFTs (`NftKind::Generic`).
    Other,
}

impl NftKindFilter {
    /// Short chip label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Gifts => "Gifts",
            Self::Usernames => "Usernames",
            Self::Numbers => "Numbers",
            Self::Other => "Other",
        }
    }

    /// Returns `true` if `item` passes this filter.
    #[must_use]
    pub const fn matches(self, kind: NftKind) -> bool {
        match self {
            Self::All => true,
            Self::Gifts => matches!(kind, NftKind::TelegramGift),
            Self::Usernames => matches!(kind, NftKind::TelegramUsername),
            Self::Numbers => matches!(kind, NftKind::TelegramNumber),
            Self::Other => matches!(kind, NftKind::Generic),
        }
    }
}

/// Stateful coordinator for the Collectibles (NFT) & Jettons wallet screen.
#[derive(Debug, Clone, Default)]
pub struct CollectiblesUiState {
    /// Active assets tab (`Collectibles` or `Jettons`).
    pub active_tab: AssetsTab,
    /// Active NFT category filter.
    pub nft_filter: NftKindFilter,
    /// Currently selected NFT raw address for detail/transfer view.
    pub selected_nft_address: Option<String>,
    /// Recipient address input for NFT transfer.
    pub nft_recipient_input: String,
    /// Optional comment input for NFT transfer.
    pub nft_comment_input: String,
    /// Pending TEP-62 NFT transfer quote awaiting explicit user confirmation.
    pub pending_nft_quote: Option<NftTransferQuote>,
    /// Currently selected Jetton wallet raw address for transfer form.
    pub selected_jetton_wallet: Option<String>,
    /// Recipient address input for Jetton transfer.
    pub jetton_recipient_input: String,
    /// Decimal token amount input for Jetton transfer.
    pub jetton_amount_input: String,
    /// Optional comment input for Jetton transfer.
    pub jetton_comment_input: String,
    /// Pending TEP-74 Jetton transfer quote awaiting explicit user confirmation.
    pub pending_jetton_quote: Option<JettonTransferQuote>,
    /// Most recent signed & broadcast receipt (for either NFT or Jetton transfer).
    pub last_receipt: Option<SendReceipt>,
    /// Multi-step Move-to-My-Gifts sheet state (`payments.{depositStarGift,displayStarGift}`).
    pub move_to_gifts: MoveToGiftsState,
    /// Inline validation or broadcast error.
    pub error: Option<String>,
}

impl CollectiblesUiState {
    /// Clears any open transfer form / quote / error while preserving the active tab.
    pub fn clear_transfer_forms(&mut self) {
        self.selected_nft_address = None;
        self.nft_recipient_input.clear();
        self.nft_comment_input.clear();
        self.pending_nft_quote = None;
        self.selected_jetton_wallet = None;
        self.jetton_recipient_input.clear();
        self.jetton_amount_input.clear();
        self.jetton_comment_input.clear();
        self.pending_jetton_quote = None;
        self.last_receipt = None;
        self.move_to_gifts.close();
        self.error = None;
    }
}

/// Outcome returned by [`render_collectibles`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollectiblesOutcome {
    /// Stay on the Collectibles / Jettons screen.
    None,
    /// Return to Wallet Overview.
    Back,
    /// Show a user toast notification.
    Toast(String),
}

pub use tg_app_core::core_types::misc::nft_display_title;

/// Human-readable category badge for an [`NftKind`].
#[must_use]
pub const fn nft_kind_badge(kind: NftKind) -> &'static str {
    match kind {
        NftKind::TelegramGift => "Gift",
        NftKind::TelegramUsername => "Username",
        NftKind::TelegramNumber => "Number",
        NftKind::Generic => "NFT",
    }
}

/// Renders the Collectibles (NFTs) gallery, Jettons balance list, and transfer flows.
pub fn render_collectibles(
    ui: &mut Ui,
    state: &mut CollectiblesUiState,
    backend: &mut dyn WalletBackend,
    testnet: bool,
    palette: &Palette,
) -> CollectiblesOutcome {
    let mut outcome = CollectiblesOutcome::None;

    // Header navigation bar.
    ui.horizontal(|ui| {
        if tg_app_core::ui_icons::back_button(ui, "Back").clicked() {
            state.clear_transfer_forms();
            outcome = CollectiblesOutcome::Back;
        }
        ui.label(
            RichText::new("Collectibles & Jettons")
                .size(15.0)
                .strong()
                .color(palette.window_bold_fg),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("Refresh").clicked() {
                match backend.refresh_assets() {
                    Ok(()) => {
                        state.error = None;
                        outcome = CollectiblesOutcome::Toast("Assets refreshed".to_owned());
                    }
                    Err(err) => {
                        state.error = Some(err);
                    }
                }
            }
        });
    });

    ui.add_space(6.0);

    // Tab bar: Collectibles (NFTs) vs Jettons (Tokens).
    let nfts = backend.collectibles();
    let jettons = backend.jetton_balances();
    ui.horizontal(|ui| {
        let nft_label = format!("Collectibles ({})", nfts.len());
        let jetton_label = format!("Jettons ({})", jettons.len());
        if tab_button(
            ui,
            &nft_label,
            state.active_tab == AssetsTab::Collectibles,
            palette,
        ) {
            state.active_tab = AssetsTab::Collectibles;
            state.error = None;
        }
        if tab_button(
            ui,
            &jetton_label,
            state.active_tab == AssetsTab::Jettons,
            palette,
        ) {
            state.active_tab = AssetsTab::Jettons;
            state.error = None;
        }
    });

    ui.add_space(6.0);

    if let Some(ref err) = state.error {
        Frame::NONE
            .fill(palette.window_bg_over)
            .stroke(Stroke::new(1.0, palette.attention_fg))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.label(RichText::new(err).size(12.0).color(palette.attention_fg));
            });
        ui.add_space(6.0);
    }

    // Move-to-Gifts confirmation flow (`wallet_move_to_gifts.cpp`).
    if state.move_to_gifts.is_open() {
        ScrollArea::vertical()
            .id_salt("tg_wallet_move_to_gifts_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                match render_move_to_gifts_sheet(ui, &mut state.move_to_gifts, backend, palette) {
                    MoveToGiftsOutcome::None => {}
                    MoveToGiftsOutcome::Closed => {
                        state.selected_nft_address = None;
                    }
                    MoveToGiftsOutcome::Moved(msg) => {
                        state.selected_nft_address = None;
                        outcome = CollectiblesOutcome::Toast(msg);
                    }
                }
            });
        return outcome;
    }

    // Receipt view after a successful Jetton or NFT transfer broadcast.
    if let Some(receipt) = state.last_receipt.clone() {
        render_receipt_card(ui, &receipt, state, &mut outcome, palette);
        return outcome;
    }

    ScrollArea::vertical()
        .id_salt("tg_wallet_assets_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| match state.active_tab {
            AssetsTab::Collectibles => {
                render_nfts_tab(ui, state, backend, &nfts, testnet, &mut outcome, palette);
            }
            AssetsTab::Jettons => {
                render_jettons_tab(ui, state, backend, &jettons, testnet, &mut outcome, palette);
            }
        });

    outcome
}

fn render_nfts_tab(
    ui: &mut Ui,
    state: &mut CollectiblesUiState,
    backend: &mut dyn WalletBackend,
    nfts: &[NftItem],
    testnet: bool,
    outcome: &mut CollectiblesOutcome,
    palette: &Palette,
) {
    // Confirmation step for TEP-62 NFT transfer.
    if let Some(quote) = state.pending_nft_quote.clone() {
        Frame::NONE
            .fill(palette.window_bg_over)
            .stroke(Stroke::new(1.0, palette.window_bg_active))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Confirm NFT Transfer (TEP-62)")
                        .size(15.0)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.add_space(6.0);
                ui.label(format!("Collectible: {}", quote.nft_name));
                if !quote.collection_name.is_empty() {
                    ui.label(format!("Collection: {}", quote.collection_name));
                }
                ui.label(format!(
                    "New owner: {}",
                    quote.new_owner.to_friendly(false, testnet)
                ));
                ui.label(format!(
                    "Attached gas: {} TON · Est. fee: {} TON",
                    format_ton(quote.attached_ton_nano as i64),
                    format_ton(quote.fee_nano as i64),
                ));
                if !quote.comment.is_empty() {
                    ui.label(format!("Comment: {}", quote.comment));
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Confirm NFT Transfer")
                                    .strong()
                                    .color(palette.window_fg_active),
                            )
                            .fill(palette.window_bg_active),
                        )
                        .clicked()
                    {
                        match backend.confirm_nft_transfer(&quote) {
                            Ok(receipt) => {
                                state.pending_nft_quote = None;
                                state.selected_nft_address = None;
                                state.last_receipt = Some(receipt);
                                state.error = None;
                                *outcome = CollectiblesOutcome::Toast(format!(
                                    "NFT transfer signed & submitted ({})",
                                    quote.nft_name
                                ));
                            }
                            Err(err) => {
                                state.error = Some(err);
                            }
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        state.pending_nft_quote = None;
                    }
                });
            });
        return;
    }

    // Filter chips.
    ui.horizontal_wrapped(|ui| {
        for filter in [
            NftKindFilter::All,
            NftKindFilter::Gifts,
            NftKindFilter::Usernames,
            NftKindFilter::Numbers,
            NftKindFilter::Other,
        ] {
            if tab_button(ui, filter.label(), state.nft_filter == filter, palette) {
                state.nft_filter = filter;
            }
        }
    });
    ui.add_space(8.0);

    let filtered: Vec<&NftItem> = nfts
        .iter()
        .filter(|item| state.nft_filter.matches(item.kind))
        .collect();

    if filtered.is_empty() {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new("No collectibles in this category")
                    .size(13.5)
                    .color(palette.window_sub_text_fg),
            );
        });
        return;
    }

    // Responsive 1- or 2-column gallery cards (prevents overlap on 390px viewports).
    let avail_w = ui.available_width();
    let cols = if avail_w < 420.0 { 1 } else { 2 };
    for row in filtered.chunks(cols) {
        ui.horizontal(|ui| {
            let card_w = if cols == 1 {
                ui.available_width()
            } else {
                ((ui.available_width() - 8.0) / 2.0).max(150.0)
            };
            for item in row {
                let selected = state.selected_nft_address.as_deref() == Some(item.address.as_str());
                let fill = if selected {
                    palette.dialogs_bg_active
                } else {
                    palette.window_bg_over
                };
                let resp = Frame::NONE
                    .fill(fill)
                    .stroke(Stroke::new(
                        if selected { 1.5 } else { 1.0 },
                        if selected {
                            palette.window_bg_active
                        } else {
                            palette.divider
                        },
                    ))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width((card_w - 22.0).max(120.0));
                        let badge_fg = if selected {
                            palette.window_fg_active
                        } else {
                            palette.window_active_text_fg
                        };
                        let title_fg = if selected {
                            palette.window_fg_active
                        } else {
                            palette.window_bold_fg
                        };
                        let sub_fg = if selected {
                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 220)
                        } else {
                            palette.window_sub_text_fg
                        };
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(nft_kind_badge(item.kind))
                                        .size(10.5)
                                        .strong()
                                        .color(badge_fg),
                                );
                                if item.on_sale {
                                    ui.label(RichText::new("On Sale").size(10.5).color(
                                        if selected {
                                            palette.window_fg_active
                                        } else {
                                            palette.attention_fg
                                        },
                                    ));
                                }
                            });
                            ui.add(
                                egui::Label::new(
                                    RichText::new(nft_display_title(item))
                                        .size(13.5)
                                        .strong()
                                        .color(title_fg),
                                )
                                .truncate(),
                            );
                            if !item.collection_name.is_empty() {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&item.collection_name)
                                            .size(11.5)
                                            .color(sub_fg),
                                    )
                                    .truncate(),
                                );
                            }
                            ui.label(
                                RichText::new(short_addr(&item.address))
                                    .monospace()
                                    .size(10.5)
                                    .color(sub_fg),
                            );
                        });
                    })
                    .response
                    .interact(egui::Sense::click());
                if resp.clicked() {
                    if selected {
                        state.selected_nft_address = None;
                    } else {
                        state.selected_nft_address = Some(item.address.clone());
                        state.error = None;
                    }
                }
            }
        });
        ui.add_space(6.0);
    }

    // Selected NFT detail & transfer form.
    if let Some(sel_addr) = state.selected_nft_address.clone()
        && let Some(item) = nfts.iter().find(|n| n.address == sel_addr)
    {
        ui.add_space(6.0);
        Frame::NONE
            .fill(palette.window_bg_over)
            .stroke(Stroke::new(1.0, palette.divider))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!("Transfer {}", nft_display_title(item)))
                            .size(14.0)
                            .strong()
                            .color(palette.window_bold_fg),
                    );
                    if ui.small_button("Copy Address").clicked() {
                        ui.ctx().copy_text(item.address.clone());
                        *outcome = CollectiblesOutcome::Toast("NFT address copied".to_owned());
                    }
                });

                if matches!(item.kind, NftKind::TelegramGift) {
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(tg_lang::tr::lng_gift_transfer_to_gifts())
                                        .strong()
                                        .size(12.5)
                                        .color(palette.window_fg_active),
                                )
                                .fill(palette.window_bg_active)
                                .corner_radius(CornerRadius::same(6)),
                            )
                            .clicked()
                        {
                            state.move_to_gifts.open_for(item.clone());
                            state.error = None;
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(
                                        tg_lang::tr::lng_wallet_collectible_show_in_gifts(),
                                    )
                                    .size(12.0)
                                    .color(palette.window_active_text_fg),
                                )
                                .fill(palette.window_bg)
                                .stroke(Stroke::new(1.0, palette.divider))
                                .corner_radius(CornerRadius::same(6)),
                            )
                            .clicked()
                        {
                            match backend.display_star_gift(&item.address, true) {
                                Ok(()) => {
                                    state.error = None;
                                    *outcome = CollectiblesOutcome::Toast(format!(
                                        "{} shown in My Gifts",
                                        nft_display_title(item)
                                    ));
                                }
                                Err(err) => {
                                    state.error = Some(err);
                                }
                            }
                        }
                    });
                    ui.add_space(6.0);
                    ui.separator();
                }

                ui.add_space(6.0);
                ui.label(
                    RichText::new("Recipient TON address")
                        .size(12.0)
                        .color(palette.window_sub_text_fg),
                );
                ui.text_edit_singleline(&mut state.nft_recipient_input);
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Optional comment")
                        .size(12.0)
                        .color(palette.window_sub_text_fg),
                );
                ui.text_edit_singleline(&mut state.nft_comment_input);
                ui.add_space(8.0);
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new("Review NFT Transfer")
                                .strong()
                                .color(palette.window_fg_active),
                        )
                        .fill(palette.window_bg_active),
                    )
                    .clicked()
                {
                    match backend.prepare_nft_transfer(
                        &item.address,
                        &state.nft_recipient_input,
                        &state.nft_comment_input,
                    ) {
                        Ok(quote) => {
                            state.pending_nft_quote = Some(quote);
                            state.error = None;
                        }
                        Err(err) => {
                            state.error = Some(err);
                        }
                    }
                }
            });
    }
}

fn render_jettons_tab(
    ui: &mut Ui,
    state: &mut CollectiblesUiState,
    backend: &mut dyn WalletBackend,
    jettons: &[JettonWalletBalance],
    testnet: bool,
    outcome: &mut CollectiblesOutcome,
    palette: &Palette,
) {
    // Confirmation step for TEP-74 Jetton transfer.
    if let Some(quote) = state.pending_jetton_quote.clone() {
        Frame::NONE
            .fill(palette.window_bg_over)
            .stroke(Stroke::new(1.0, palette.window_bg_active))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Confirm Jetton Transfer (TEP-74)")
                        .size(15.0)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.add_space(6.0);
                ui.label(format!(
                    "Amount: {} {}",
                    format_jetton_amount(quote.jetton_amount, quote.decimals),
                    quote.symbol
                ));
                ui.label(format!(
                    "Recipient: {}",
                    quote.destination.to_friendly(false, testnet)
                ));
                ui.label(format!(
                    "Attached gas: {} TON · Est. fee: {} TON",
                    format_ton(quote.attached_ton_nano as i64),
                    format_ton(quote.fee_nano as i64),
                ));
                if !quote.comment.is_empty() {
                    ui.label(format!("Comment: {}", quote.comment));
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Confirm Jetton Transfer")
                                    .strong()
                                    .color(palette.window_fg_active),
                            )
                            .fill(palette.window_bg_active),
                        )
                        .clicked()
                    {
                        match backend.confirm_jetton_transfer(&quote) {
                            Ok(receipt) => {
                                state.pending_jetton_quote = None;
                                state.selected_jetton_wallet = None;
                                state.last_receipt = Some(receipt);
                                state.error = None;
                                *outcome = CollectiblesOutcome::Toast(format!(
                                    "Sent {} {}",
                                    format_jetton_amount(quote.jetton_amount, quote.decimals),
                                    quote.symbol
                                ));
                            }
                            Err(err) => {
                                state.error = Some(err);
                            }
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        state.pending_jetton_quote = None;
                    }
                });
            });
        return;
    }

    if jettons.is_empty() {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new("No Jetton token balances found")
                    .size(13.5)
                    .color(palette.window_sub_text_fg),
            );
        });
        return;
    }

    for token in jettons {
        let is_selected =
            state.selected_jetton_wallet.as_deref() == Some(token.wallet_address.as_str());
        Frame::NONE
            .fill(if is_selected {
                palette.dialogs_bg_active
            } else {
                palette.window_bg_over
            })
            .stroke(Stroke::new(1.0, palette.divider))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        let sym = token.display_symbol();
                        let subtitle = if !token.name.is_empty() && token.name != sym {
                            token.name.clone()
                        } else {
                            short_addr(&token.jetton_master)
                        };
                        ui.label(
                            RichText::new(sym)
                                .size(14.0)
                                .strong()
                                .color(palette.window_bold_fg),
                        );
                        ui.label(
                            RichText::new(subtitle)
                                .size(11.5)
                                .color(palette.window_sub_text_fg),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Send").clicked() {
                            state.selected_jetton_wallet = Some(token.wallet_address.clone());
                            state.jetton_amount_input.clear();
                            state.error = None;
                        }
                        ui.label(
                            RichText::new(format!(
                                "{} {}",
                                token.formatted_balance(),
                                token.display_symbol()
                            ))
                            .strong()
                            .size(13.5)
                            .color(palette.window_bold_fg),
                        );
                    });
                });
            });
        ui.add_space(6.0);
    }

    // Selected Jetton transfer form.
    if let Some(sel_wallet) = state.selected_jetton_wallet.clone()
        && let Some(token) = jettons.iter().find(|t| t.wallet_address == sel_wallet)
    {
        ui.add_space(6.0);
        Frame::NONE
            .fill(palette.window_bg_over)
            .stroke(Stroke::new(1.0, palette.divider))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new(format!(
                        "Send {} (Available: {})",
                        token.display_symbol(),
                        token.formatted_balance()
                    ))
                    .size(14.0)
                    .strong()
                    .color(palette.window_bold_fg),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Recipient TON address")
                        .size(12.0)
                        .color(palette.window_sub_text_fg),
                );
                ui.text_edit_singleline(&mut state.jetton_recipient_input);
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("Amount ({})", token.display_symbol()))
                        .size(12.0)
                        .color(palette.window_sub_text_fg),
                );
                ui.text_edit_singleline(&mut state.jetton_amount_input);
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Optional comment")
                        .size(12.0)
                        .color(palette.window_sub_text_fg),
                );
                ui.text_edit_singleline(&mut state.jetton_comment_input);
                ui.add_space(8.0);
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new("Review Jetton Transfer")
                                .strong()
                                .color(palette.window_fg_active),
                        )
                        .fill(palette.window_bg_active),
                    )
                    .clicked()
                {
                    match parse_jetton_amount(&state.jetton_amount_input, token.decimals) {
                        Ok(raw_amount) => match backend.prepare_jetton_transfer(
                            &token.wallet_address,
                            &state.jetton_recipient_input,
                            raw_amount,
                            &state.jetton_comment_input,
                        ) {
                            Ok(quote) => {
                                state.pending_jetton_quote = Some(quote);
                                state.error = None;
                            }
                            Err(err) => {
                                state.error = Some(err);
                            }
                        },
                        Err(err) => {
                            state.error = Some(err.to_string());
                        }
                    }
                }
            });
    }
}

fn render_receipt_card(
    ui: &mut Ui,
    receipt: &SendReceipt,
    state: &mut CollectiblesUiState,
    outcome: &mut CollectiblesOutcome,
    palette: &Palette,
) {
    Frame::NONE
        .fill(palette.window_bg_over)
        .stroke(Stroke::new(1.0, palette.divider))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.label(
                RichText::new("Asset Transfer Submitted")
                    .size(16.0)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!("Message hash: {}", receipt.message_hash_hex))
                    .monospace()
                    .size(11.5)
                    .color(palette.window_fg),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Copy Hash").clicked() {
                    ui.ctx().copy_text(receipt.message_hash_hex.clone());
                    *outcome = CollectiblesOutcome::Toast("Message hash copied".to_owned());
                }
                if ui.button("Done").clicked() {
                    state.clear_transfer_forms();
                }
            });
        });
}

fn tab_button(ui: &mut Ui, label: &str, active: bool, palette: &Palette) -> bool {
    let (fill, text_color) = if active {
        (palette.window_bg_active, palette.window_fg_active)
    } else {
        (palette.window_bg_over, palette.window_fg)
    };
    ui.add(
        egui::Button::new(RichText::new(label).size(12.5).color(text_color))
            .fill(fill)
            .corner_radius(CornerRadius::same(6))
            .min_size(Vec2::new(64.0, 28.0)),
    )
    .clicked()
}

fn short_addr(addr: &str) -> String {
    if addr.len() > 16 {
        format!("{}...{}", &addr[..8], &addr[addr.len() - 6..])
    } else {
        addr.to_owned()
    }
}
