//! Read-only TON fiat onramp and swap link handoff UI (`wallet/onramp.rs`).
//!
//! Port origin: `Telegram/SourceFiles/wallet/wallet_onramp.h` and
//! `Telegram/SourceFiles/wallet/wallet_onramp.cpp`.
//!
//! **Hard safety rule**: This module is strictly a read-only external link handoff. It never
//! collects payment credentials, never auto-purchases, and never contacts real onramp or swap
//! providers in tests.

use egui::{CornerRadius, Frame, Margin, RichText, ScrollArea, Stroke, Ui, Vec2};
use tg_wallet::address::TonAddress;

use tg_app_core::stand_in::theme::Palette;

use super::backend::{OnrampHandoffSession, WalletBackend};

/// Common selectable fiat base currencies for onramp link generation.
pub const ONRAMP_BASE_CURRENCIES: &[&str] = &["USD", "EUR", "GBP", "AED", "TRY"];

/// Common asset tickers for read-only swap handoff link preview.
pub const SWAP_ASSETS: &[&str] = &["TON", "USDt", "NOT"];

/// Validates that an onramp/swap handoff URL is strictly `https://` with a non-empty host.
///
/// Rejects `http://`, `javascript:`, `data:`, `file://`, and empty strings.
pub fn validate_handoff_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Handoff URL is empty".to_owned());
    }
    let parsed = reqwest::Url::parse(trimmed).map_err(|e| format!("Invalid handoff URL: {e}"))?;
    if parsed.scheme() != "https" {
        return Err(format!(
            "Refusing non-HTTPS handoff URL scheme: {}",
            parsed.scheme()
        ));
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err("Handoff URL is missing host".to_owned());
    }
    Ok(parsed.to_string())
}

/// Builds a read-only external swap handoff URL for `base_url` (which must be an `https://` URL),
/// encoding `wallet_address`, `from_asset`, and `to_asset` as query parameters.
///
/// Never initiates a transaction or signs anything; strictly produces a URL the user can copy
/// or open in an external browser.
pub fn build_readonly_swap_handoff_url(
    base_url: &str,
    wallet_address: &str,
    from_asset: &str,
    to_asset: &str,
) -> Result<String, String> {
    let validated = validate_handoff_url(base_url)?;
    let mut parsed =
        reqwest::Url::parse(&validated).map_err(|e| format!("Invalid swap base URL: {e}"))?;
    parsed
        .query_pairs_mut()
        .append_pair("address", wallet_address.trim())
        .append_pair("from", from_asset.trim())
        .append_pair("to", to_asset.trim())
        .append_pair("readonly", "1");
    Ok(parsed.to_string())
}

/// State for the read-only Buy / Swap Handoff wallet screen.
#[derive(Debug, Clone)]
pub struct OnrampUiState {
    /// Selected fiat currency index inside [`ONRAMP_BASE_CURRENCIES`].
    pub currency_index: usize,
    /// Selected provider identifier (if user clicked a specific provider).
    pub selected_provider_id: Option<String>,
    /// Most recent read-only handoff session returned by the backend.
    pub generated_session: Option<OnrampHandoffSession>,
    /// Selected source asset index in [`SWAP_ASSETS`] for read-only swap handoff preview.
    pub swap_from_index: usize,
    /// Selected target asset index in [`SWAP_ASSETS`] for read-only swap handoff preview.
    pub swap_to_index: usize,
    /// Optional configured read-only swap portal URL (never hardcoded to a live payment provider).
    pub swap_portal_base_url: String,
    /// Generated read-only swap handoff URL, if prepared.
    pub generated_swap_url: Option<String>,
    /// Inline error message.
    pub error: Option<String>,
}

impl Default for OnrampUiState {
    fn default() -> Self {
        Self {
            currency_index: 0,
            selected_provider_id: None,
            generated_session: None,
            swap_from_index: 0,
            swap_to_index: 1,
            swap_portal_base_url: String::new(),
            generated_swap_url: None,
            error: None,
        }
    }
}

/// Outcome returned by [`render_onramp`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnrampOutcome {
    /// Stay on the onramp/swap screen.
    None,
    /// Return to Wallet Overview.
    Back,
    /// Show a user toast notification.
    Toast(String),
    /// User explicitly clicked "Open in Browser" for a validated read-only `https://` URL.
    OpenExternalUrl(String),
}

/// Renders the read-only Fiat Onramp & Swap Link Handoff screen.
pub fn render_onramp(
    ui: &mut Ui,
    state: &mut OnrampUiState,
    backend: &mut dyn WalletBackend,
    wallet_address: Option<TonAddress>,
    testnet: bool,
    palette: &Palette,
) -> OnrampOutcome {
    let mut outcome = OnrampOutcome::None;
    let friendly_addr = wallet_address
        .map(|a| a.to_friendly(false, testnet))
        .unwrap_or_default();

    ui.horizontal(|ui| {
        if tg_app_core::ui_icons::back_button(ui, "Back").clicked() {
            outcome = OnrampOutcome::Back;
        }
        ui.label(
            RichText::new("Buy & Swap (Read-Only Handoff)")
                .size(15.0)
                .strong()
                .color(palette.window_bold_fg),
        );
    });
    ui.add_space(6.0);

    // Explicit read-only safety disclaimer.
    Frame::NONE
        .fill(palette.window_bg_over)
        .stroke(Stroke::new(1.0, palette.divider))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.label(
                RichText::new(
                    "Read-only external handoff. Rustgram never processes card payments, collects billing info, or executes purchases automatically.",
                )
                .size(12.0)
                .color(palette.window_sub_text_fg),
            );
        });
    ui.add_space(8.0);

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

    ScrollArea::vertical()
        .id_salt("tg_wallet_onramp_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Receiving address summary.
            ui.label(
                RichText::new("Destination TON Address (prefilled in handoff link)")
                    .size(12.0)
                    .color(palette.window_sub_text_fg),
            );
            ui.label(
                RichText::new(if friendly_addr.is_empty() {
                    "(No wallet initialized)"
                } else {
                    &friendly_addr
                })
                .monospace()
                .size(11.5)
                .color(palette.window_fg),
            );
            ui.add_space(10.0);

            // Section 1: Fiat Onramp Provider Handoff (`payments.getOnrampProviders` -> `createOnrampSession`).
            ui.label(
                RichText::new("Fiat Onramp Providers")
                    .size(14.0)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(4.0);

            // Base currency selector chips.
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("Currency:")
                        .size(12.0)
                        .color(palette.window_sub_text_fg),
                );
                for (idx, code) in ONRAMP_BASE_CURRENCIES.iter().enumerate() {
                    let active = state.currency_index == idx;
                    let fill = if active {
                        palette.window_bg_active
                    } else {
                        palette.window_bg_over
                    };
                    let fg = if active {
                        palette.window_fg_active
                    } else {
                        palette.window_fg
                    };
                    if ui
                        .add(
                            egui::Button::new(RichText::new(*code).size(12.0).color(fg))
                                .fill(fill)
                                .corner_radius(CornerRadius::same(6)),
                        )
                        .clicked()
                    {
                        state.currency_index = idx;
                    }
                }
            });
            ui.add_space(6.0);

            let providers = backend.onramp_providers();
            if providers.is_empty() {
                ui.label(
                    RichText::new("No onramp providers configured for this session.")
                        .size(12.5)
                        .color(palette.window_sub_text_fg),
                );
            } else {
                let selected_currency = ONRAMP_BASE_CURRENCIES
                    .get(state.currency_index)
                    .copied()
                    .unwrap_or("USD");
                for provider in &providers {
                    let selected =
                        state.selected_provider_id.as_deref() == Some(provider.id.as_str());
                    Frame::NONE
                        .fill(if selected {
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
                                    ui.label(
                                        RichText::new(&provider.name)
                                            .strong()
                                            .size(13.5)
                                            .color(palette.window_bold_fg),
                                    );
                                    ui.label(
                                        RichText::new(format!(
                                            "Assets: {}",
                                            provider.crypto_currencies.join(", ")
                                        ))
                                        .size(11.5)
                                        .color(palette.window_sub_text_fg),
                                    );
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new("Get Handoff Link")
                                                        .color(palette.window_fg_active),
                                                )
                                                .fill(palette.window_bg_active),
                                            )
                                            .clicked()
                                        {
                                            state.selected_provider_id = Some(provider.id.clone());
                                            match backend.create_onramp_handoff(
                                                &provider.id,
                                                Some(selected_currency),
                                            ) {
                                                Ok(session) => {
                                                    state.generated_session = Some(session);
                                                    state.error = None;
                                                    outcome = OnrampOutcome::Toast(
                                                        "Read-only onramp handoff link ready"
                                                            .to_owned(),
                                                    );
                                                }
                                                Err(err) => {
                                                    state.error = Some(err);
                                                }
                                            }
                                        }
                                    },
                                );
                            });
                        });
                    ui.add_space(6.0);
                }
            }

            if let Some(session) = state.generated_session.clone() {
                Frame::NONE
                    .fill(palette.window_bg_over)
                    .stroke(Stroke::new(1.0, palette.window_bg_active))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new(format!(
                                "External Handoff Ready · {}",
                                session.provider_name
                            ))
                            .strong()
                            .size(13.5)
                            .color(palette.window_bold_fg),
                        );
                        ui.label(
                            RichText::new(&session.handoff_url)
                                .monospace()
                                .size(11.0)
                                .color(palette.window_active_text_fg),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("Copy Handoff Link").clicked() {
                                ui.ctx().copy_text(session.handoff_url.clone());
                                outcome =
                                    OnrampOutcome::Toast("Onramp handoff URL copied".to_owned());
                            }
                            if ui.button("Open in Browser").clicked() {
                                outcome = OnrampOutcome::OpenExternalUrl(session.handoff_url);
                            }
                        });
                    });
                ui.add_space(10.0);
            }

            // Section 2: Read-only Swap Link Handoff.
            ui.separator();
            ui.add_space(8.0);
            ui.label(
                RichText::new("Read-Only Swap Link Builder")
                    .size(14.0)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new("External Swap Portal URL (HTTPS)")
                    .size(12.0)
                    .color(palette.window_sub_text_fg),
            );
            ui.text_edit_singleline(&mut state.swap_portal_base_url);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("From:").size(12.0));
                for (idx, sym) in SWAP_ASSETS.iter().enumerate() {
                    if ui
                        .selectable_label(state.swap_from_index == idx, *sym)
                        .clicked()
                    {
                        state.swap_from_index = idx;
                    }
                }
                ui.separator();
                ui.label(RichText::new("To:").size(12.0));
                for (idx, sym) in SWAP_ASSETS.iter().enumerate() {
                    if ui
                        .selectable_label(state.swap_to_index == idx, *sym)
                        .clicked()
                    {
                        state.swap_to_index = idx;
                    }
                }
            });
            ui.add_space(6.0);
            if ui
                .add_sized(
                    Vec2::new(170.0, 30.0),
                    egui::Button::new("Build Swap Handoff Link"),
                )
                .clicked()
            {
                let from = SWAP_ASSETS
                    .get(state.swap_from_index)
                    .copied()
                    .unwrap_or("TON");
                let to = SWAP_ASSETS
                    .get(state.swap_to_index)
                    .copied()
                    .unwrap_or("USDt");
                if from == to {
                    state.error = Some("Select two different assets for swap handoff".to_owned());
                } else {
                    match build_readonly_swap_handoff_url(
                        &state.swap_portal_base_url,
                        &friendly_addr,
                        from,
                        to,
                    ) {
                        Ok(url) => {
                            state.generated_swap_url = Some(url);
                            state.error = None;
                            outcome = OnrampOutcome::Toast(
                                "Read-only swap handoff link ready".to_owned(),
                            );
                        }
                        Err(err) => {
                            state.error = Some(err);
                        }
                    }
                }
            }

            if let Some(swap_url) = state.generated_swap_url.clone() {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(&swap_url)
                        .monospace()
                        .size(11.0)
                        .color(palette.window_active_text_fg),
                );
                ui.horizontal(|ui| {
                    if ui.button("Copy Swap Link").clicked() {
                        ui.ctx().copy_text(swap_url.clone());
                        outcome = OnrampOutcome::Toast("Swap handoff URL copied".to_owned());
                    }
                    if ui.button("Open Swap Portal").clicked() {
                        outcome = OnrampOutcome::OpenExternalUrl(swap_url);
                    }
                });
            }
        });

    outcome
}
