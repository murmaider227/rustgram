//! Shamir `(k, n)` recovery-phrase backup and reconstruction UI (`tg-wallet::phrase_shares`).
//!
//! Port origin: `Telegram/SourceFiles/wallet/wallet_phrase_shares.cpp` and
//! `Telegram/SourceFiles/wallet/wallet_custody.cpp`.
//!
//! Uses [`tg_wallet::phrase_shares::split_phrase`] and
//! [`tg_wallet::phrase_shares::reconstruct_phrase`] over GF(256) with per-share and inner-secret
//! SHA-256 checksums. Never stores unencrypted shares on disk and requires explicit user action
//! to reveal, copy, or import a reconstructed phrase.

use egui::{CornerRadius, Frame, Label, Margin, RichText, ScrollArea, Stroke, Ui, Vec2};
use tg_wallet::contract::WalletVersion;
use tg_wallet::phrase_shares::{
    PHRASE_SHARE_PREFIX, PhraseShare, reconstruct_phrase, split_phrase,
};

use tg_app_core::stand_in::theme::Palette;

use super::backend::WalletBackend;

/// Active tab inside the Shamir Backup & Recovery screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackupTab {
    /// Split the unlocked 24-word phrase into `(k, n)` Shamir shares.
    #[default]
    Split,
    /// Paste `k` or more `tgshare1:...` shares to reconstruct and verify/import the phrase.
    Recover,
}

/// Generated display share kept only in transient UI memory while the Backup screen is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedShareCard {
    /// 1-based share x-coordinate (`1..=total_shares`).
    pub index: u8,
    /// Minimum threshold `k`.
    pub threshold: u8,
    /// Total generated shares `n`.
    pub total_shares: u8,
    /// 4-byte checksum rendered as 8 uppercase hex chars.
    pub checksum_hex: String,
    /// Portable `tgshare1:<base64url>` share token.
    pub encoded: String,
}

/// Stateful controller for [`render_backup`].
#[derive(Debug, Clone)]
pub struct BackupUiState {
    /// Selected tab (`Split` or `Recover`).
    pub tab: BackupTab,
    /// Shamir threshold `k` (`2..=total_shares`).
    pub threshold: u8,
    /// Total shares `n` (`2..=10`).
    pub total_shares: u8,
    /// Generated shares for the current split operation (cleared on user request or screen exit).
    pub generated_shares: Vec<GeneratedShareCard>,
    /// Index of the most recently copied share for immediate inline feedback.
    pub last_copied_share: Option<u8>,
    /// Text inputs for pasting `tgshare1:...` shares on the Recover tab.
    pub recover_inputs: Vec<String>,
    /// Successfully reconstructed 24-word recovery phrase, if any.
    pub reconstructed_words: Option<Vec<String>>,
    /// Whether the inline "Import into Wallet" confirmation form is expanded.
    pub show_import_form: bool,
    /// Target wallet contract version when importing a reconstructed phrase.
    pub import_version: WalletVersion,
    /// Local vault passcode when importing a reconstructed phrase.
    pub import_passcode: String,
    /// Local vault passcode confirmation.
    pub import_passcode_confirm: String,
    /// Whether the inline confirmation card for disabling Telegram Cloud Backup is expanded.
    pub confirm_disable_cloud_backup: bool,
    /// Inline validation or backend error message.
    pub error: Option<String>,
}

impl Default for BackupUiState {
    fn default() -> Self {
        Self {
            tab: BackupTab::Split,
            threshold: 2,
            total_shares: 3,
            generated_shares: Vec::new(),
            last_copied_share: None,
            recover_inputs: vec![String::new(), String::new()],
            reconstructed_words: None,
            show_import_form: false,
            import_version: WalletVersion::V5R1,
            import_passcode: String::new(),
            import_passcode_confirm: String::new(),
            confirm_disable_cloud_backup: false,
            error: None,
        }
    }
}

impl BackupUiState {
    /// Clears sensitive transient shares, reconstructed words, and passcodes from memory.
    pub fn clear_sensitive(&mut self) {
        self.generated_shares.clear();
        self.last_copied_share = None;
        self.reconstructed_words = None;
        self.show_import_form = false;
        self.import_passcode.clear();
        self.import_passcode_confirm.clear();
        self.confirm_disable_cloud_backup = false;
        self.error = None;
    }

    /// Generates Shamir `(threshold, total_shares)` shares from `words` using
    /// [`tg_wallet::phrase_shares::split_phrase`].
    pub fn generate_from_words(&mut self, words: &[String]) -> Result<usize, String> {
        let shares =
            split_phrase(words, self.threshold, self.total_shares).map_err(|e| e.to_string())?;
        let mut cards = Vec::with_capacity(shares.len());
        for s in shares {
            let encoded = s.encode_string().map_err(|e| e.to_string())?;
            cards.push(GeneratedShareCard {
                index: s.index,
                threshold: s.threshold,
                total_shares: s.total_shares,
                checksum_hex: format_checksum_hex(s.checksum),
                encoded,
            });
        }
        let count = cards.len();
        self.generated_shares = cards;
        self.last_copied_share = None;
        self.error = None;
        Ok(count)
    }

    /// Attempts to decode all non-empty recovery inputs and reconstruct the 24-word phrase via
    /// [`tg_wallet::phrase_shares::reconstruct_phrase`].
    pub fn try_reconstruct(&mut self) -> Result<Vec<String>, String> {
        let mut decoded = Vec::new();
        for (idx, raw) in self.recover_inputs.iter().enumerate() {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            let share = PhraseShare::decode_string(trimmed)
                .map_err(|e| format!("Share #{}: {e}", idx + 1))?;
            decoded.push(share);
        }
        if decoded.len() < 2 {
            let msg = "Enter at least 2 valid Shamir shares to reconstruct the phrase.".to_owned();
            self.error = Some(msg.clone());
            return Err(msg);
        }
        match reconstruct_phrase(&decoded) {
            Ok(words) => {
                self.reconstructed_words = Some(words.clone());
                self.error = None;
                Ok(words)
            }
            Err(err) => {
                let msg = err.to_string();
                self.error = Some(msg.clone());
                Err(msg)
            }
        }
    }
}

/// Outcome returned by [`render_backup`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupOutcome {
    /// Stay on the Backup screen.
    None,
    /// Return to Wallet Overview.
    Back,
    /// Wallet was replaced/imported from reconstructed shares; return to Overview with toast.
    Imported(String),
    /// Emit toast notification.
    Toast(String),
}

fn format_checksum_hex(bytes: [u8; 4]) -> String {
    format!(
        "{:02X}{:02X}{:02X}{:02X}",
        bytes[0], bytes[1], bytes[2], bytes[3]
    )
}

fn success_color(palette: &Palette) -> egui::Color32 {
    let luma = u16::from(palette.window_bg.r())
        + u16::from(palette.window_bg.g())
        + u16::from(palette.window_bg.b());
    if luma < 384 {
        egui::Color32::from_rgb(0x52, 0xC8, 0x7A)
    } else {
        egui::Color32::from_rgb(0x1E, 0x82, 0x42)
    }
}

fn warning_fill(palette: &Palette) -> egui::Color32 {
    let luma = u16::from(palette.window_bg.r())
        + u16::from(palette.window_bg.g())
        + u16::from(palette.window_bg.b());
    if luma < 384 {
        egui::Color32::from_rgb(0x36, 0x26, 0x1A)
    } else {
        egui::Color32::from_rgb(0xFF, 0xF5, 0xE6)
    }
}

fn warning_stroke(palette: &Palette) -> egui::Color32 {
    let luma = u16::from(palette.window_bg.r())
        + u16::from(palette.window_bg.g())
        + u16::from(palette.window_bg.b());
    if luma < 384 {
        egui::Color32::from_rgb(0xC9, 0x82, 0x2E)
    } else {
        egui::Color32::from_rgb(0xD9, 0x77, 0x06)
    }
}

/// Renders the Shamir `(k, n)` phrase backup and recovery UI.
pub fn render_backup(
    ui: &mut Ui,
    state: &mut BackupUiState,
    backend: &mut dyn WalletBackend,
    palette: &Palette,
) -> BackupOutcome {
    let mut outcome = BackupOutcome::None;

    ScrollArea::vertical()
        .id_salt("tg_wallet_shamir_backup_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Header row.
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Shamir Phrase Backup")
                        .size(18.0)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Back").clicked() {
                        outcome = BackupOutcome::Back;
                    }
                });
            });
            ui.add_space(4.0);
            ui.add(
                Label::new(
                    RichText::new(
                        "Split your 24-word recovery phrase into independent shares so no single backup exposes your wallet.",
                    )
                    .size(12.5)
                    .color(palette.window_sub_text_fg),
                )
                .wrap(),
            );
            ui.add_space(10.0);

            // Telegram Encrypted Cloud Backup card (`wallet_custody.cpp` / `wallet.enableBackup`).
            render_cloud_backup_card(ui, state, backend, palette, &mut outcome);
            ui.add_space(12.0);

            // Segmented tab bar.
            ui.horizontal_wrapped(|ui| {
                for (tab, label) in [
                    (BackupTab::Split, "Split Phrase"),
                    (BackupTab::Recover, "Recover from Shares"),
                ] {
                    let active = state.tab == tab;
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
                            egui::Button::new(RichText::new(label).strong().size(13.0).color(fg))
                                .fill(fill)
                                .stroke(Stroke::new(
                                    1.0,
                                    if active {
                                        palette.window_bg_active
                                    } else {
                                        palette.divider
                                    },
                                ))
                                .corner_radius(CornerRadius::same(8))
                                .min_size(Vec2::new(132.0, 32.0)),
                        )
                        .clicked()
                    {
                        state.tab = tab;
                        state.error = None;
                    }
                }
            });
            ui.add_space(10.0);

            match state.tab {
                BackupTab::Split => {
                    render_split_tab(ui, state, backend, palette, &mut outcome);
                }
                BackupTab::Recover => {
                    render_recover_tab(ui, state, backend, palette, &mut outcome);
                }
            }

            if let Some(err) = &state.error {
                ui.add_space(8.0);
                Frame::NONE
                    .fill(palette.window_bg_over)
                    .stroke(Stroke::new(1.0, palette.attention_fg))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.add(
                            Label::new(
                                RichText::new(format!("! {err}"))
                                    .size(12.0)
                                    .color(palette.attention_fg),
                            )
                            .wrap(),
                        );
                    });
            }
        });

    outcome
}

fn render_cloud_backup_card(
    ui: &mut Ui,
    state: &mut BackupUiState,
    backend: &mut dyn WalletBackend,
    palette: &Palette,
    outcome: &mut BackupOutcome,
) {
    let status = backend.cloud_backup_status();
    let ok_color = success_color(palette);

    Frame::NONE
        .fill(palette.window_bg_over)
        .stroke(Stroke::new(
            1.0,
            if status.backup_enabled {
                ok_color
            } else {
                palette.divider
            },
        ))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(tg_lang::tr::lng_wallet_backup_section())
                        .size(14.5)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                let (badge_text, badge_color) = if status.backup_enabled {
                    (
                        format!(
                            "Enabled{}",
                            if status.holder_dc_count > 0 {
                                format!(" · {} DCs", status.holder_dc_count)
                            } else {
                                String::new()
                            }
                        ),
                        ok_color,
                    )
                } else if status.can_enable_backup {
                    ("Off".to_owned(), palette.window_sub_text_fg)
                } else {
                    ("Unavailable".to_owned(), palette.window_sub_text_fg)
                };
                Frame::NONE
                    .fill(palette.window_bg)
                    .stroke(Stroke::new(1.0, badge_color))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::symmetric(8, 2))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(badge_text)
                                .size(11.0)
                                .strong()
                                .color(badge_color),
                        );
                    });
            });

            ui.add_space(4.0);
            let about = if status.backup_enabled || status.can_enable_backup {
                tg_lang::tr::lng_wallet_backup_about_on()
            } else {
                tg_lang::tr::lng_wallet_backup_about_unavailable()
            };
            ui.add(
                Label::new(
                    RichText::new(about)
                        .size(12.0)
                        .color(palette.window_sub_text_fg),
                )
                .wrap(),
            );

            ui.add_space(8.0);

            if state.confirm_disable_cloud_backup && status.backup_enabled {
                Frame::NONE
                    .fill(warning_fill(palette))
                    .stroke(Stroke::new(1.0, palette.attention_fg))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new(tg_lang::tr::lng_wallet_backup_disable_title())
                                .size(13.0)
                                .strong()
                                .color(palette.attention_fg),
                        );
                        ui.add_space(4.0);
                        ui.add(
                            Label::new(
                                RichText::new(tg_lang::tr::lng_wallet_backup_disable_text())
                                    .size(12.0)
                                    .color(palette.window_fg),
                            )
                            .wrap(),
                        );
                        ui.add_space(8.0);
                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("Confirm Disable")
                                            .strong()
                                            .color(egui::Color32::WHITE),
                                    )
                                    .fill(palette.attention_fg)
                                    .corner_radius(CornerRadius::same(6)),
                                )
                                .clicked()
                            {
                                match backend.disable_cloud_backup(false) {
                                    Ok(_) => {
                                        state.confirm_disable_cloud_backup = false;
                                        state.error = None;
                                        *outcome = BackupOutcome::Toast(
                                            "Cloud backup disabled".to_owned(),
                                        );
                                    }
                                    Err(err) => {
                                        state.error = Some(err);
                                    }
                                }
                            }
                            if ui.button("Cancel").clicked() {
                                state.confirm_disable_cloud_backup = false;
                            }
                        });
                    });
            } else if status.backup_enabled {
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(tg_lang::tr::lng_wallet_backup_disable())
                                .size(12.5)
                                .color(palette.attention_fg),
                        )
                        .fill(palette.window_bg)
                        .stroke(Stroke::new(1.0, palette.attention_fg))
                        .corner_radius(CornerRadius::same(6)),
                    )
                    .clicked()
                {
                    state.confirm_disable_cloud_backup = true;
                    state.error = None;
                }
            } else if ui
                .add_enabled(
                    status.can_enable_backup,
                    egui::Button::new(
                        RichText::new(tg_lang::tr::lng_wallet_backup_enable())
                            .strong()
                            .size(12.5)
                            .color(palette.window_fg_active),
                    )
                    .fill(palette.window_bg_active)
                    .corner_radius(CornerRadius::same(6)),
                )
                .clicked()
            {
                match backend.enable_cloud_backup() {
                    Ok(_) => {
                        state.error = None;
                        *outcome =
                            BackupOutcome::Toast("Encrypted cloud backup enabled".to_owned());
                    }
                    Err(err) => {
                        state.error = Some(err);
                    }
                }
            }
        });
}

fn render_split_tab(
    ui: &mut Ui,
    state: &mut BackupUiState,
    backend: &mut dyn WalletBackend,
    palette: &Palette,
    outcome: &mut BackupOutcome,
) {
    // Security advisory card.
    Frame::NONE
        .fill(warning_fill(palette))
        .stroke(Stroke::new(1.0, warning_stroke(palette)))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                Label::new(
                    RichText::new(format!(
                        "! Store each share separately offline. Any {} of {} shares can rebuild your full wallet.",
                        state.threshold, state.total_shares
                    ))
                    .size(12.0)
                    .color(palette.window_bold_fg),
                )
                .wrap(),
            );
        });
    ui.add_space(10.0);

    // Threshold / Total shares configuration card.
    Frame::NONE
        .fill(palette.window_bg_over)
        .stroke(Stroke::new(1.0, palette.divider))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new("Backup Scheme")
                    .size(14.0)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(8.0);

            let render_k = |ui: &mut Ui, state: &mut BackupUiState| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Required shares (k):")
                            .size(12.5)
                            .color(palette.window_fg),
                    );
                    if ui
                        .add_enabled(state.threshold > 2, egui::Button::new(" - "))
                        .clicked()
                    {
                        state.threshold = state.threshold.saturating_sub(1).max(2);
                        state.generated_shares.clear();
                    }
                    ui.label(
                        RichText::new(format!("{}", state.threshold))
                            .monospace()
                            .strong()
                            .size(13.5)
                            .color(palette.window_bold_fg),
                    );
                    if ui
                        .add_enabled(
                            state.threshold < state.total_shares,
                            egui::Button::new(" + "),
                        )
                        .clicked()
                    {
                        state.threshold = state.threshold.saturating_add(1).min(state.total_shares);
                        state.generated_shares.clear();
                    }
                });
            };
            let render_n = |ui: &mut Ui, state: &mut BackupUiState| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Total shares (n):")
                            .size(12.5)
                            .color(palette.window_fg),
                    );
                    if ui
                        .add_enabled(state.total_shares > 2, egui::Button::new(" - "))
                        .clicked()
                    {
                        state.total_shares = state.total_shares.saturating_sub(1).max(2);
                        if state.threshold > state.total_shares {
                            state.threshold = state.total_shares;
                        }
                        state.generated_shares.clear();
                    }
                    ui.label(
                        RichText::new(format!("{}", state.total_shares))
                            .monospace()
                            .strong()
                            .size(13.5)
                            .color(palette.window_bold_fg),
                    );
                    if ui
                        .add_enabled(state.total_shares < 10, egui::Button::new(" + "))
                        .clicked()
                    {
                        state.total_shares = state.total_shares.saturating_add(1).min(10);
                        state.generated_shares.clear();
                    }
                });
            };
            if ui.available_width() < 460.0 {
                render_k(ui, state);
                ui.add_space(4.0);
                render_n(ui, state);
            } else {
                ui.horizontal(|ui| {
                    render_k(ui, state);
                    ui.add_space(16.0);
                    render_n(ui, state);
                });
            }

            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(format!(
                                "Generate {}-of-{} Shares",
                                state.threshold, state.total_shares
                            ))
                            .strong()
                            .color(palette.window_fg_active),
                        )
                        .fill(palette.window_bg_active)
                        .corner_radius(CornerRadius::same(8))
                        .min_size(Vec2::new(180.0, 34.0)),
                    )
                    .clicked()
                {
                    match backend.reveal_phrase() {
                        Ok(words) => match state.generate_from_words(&words) {
                            Ok(n) => {
                                *outcome = BackupOutcome::Toast(format!(
                                    "Generated {n} Shamir shares ({}-of-{n})",
                                    state.threshold
                                ));
                            }
                            Err(err) => state.error = Some(err),
                        },
                        Err(err) => state.error = Some(err),
                    }
                }

                if !state.generated_shares.is_empty() {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Verify in Recover Tab")
                                    .color(palette.window_active_text_fg),
                            )
                            .min_size(Vec2::new(0.0, 34.0)),
                        )
                        .clicked()
                    {
                        let k = usize::from(state.threshold);
                        state.recover_inputs = state
                            .generated_shares
                            .iter()
                            .take(k)
                            .map(|s| s.encoded.clone())
                            .collect();
                        state.reconstructed_words = None;
                        state.tab = BackupTab::Recover;
                        state.error = None;
                    }

                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Hide Shares").color(palette.window_sub_text_fg),
                            )
                            .min_size(Vec2::new(0.0, 34.0)),
                        )
                        .clicked()
                    {
                        state.generated_shares.clear();
                        state.last_copied_share = None;
                    }
                }
            });
        });

    if !state.generated_shares.is_empty() {
        ui.add_space(12.0);
        ui.label(
            RichText::new(format!(
                "Generated Shares ({} of {} required)",
                state.threshold, state.total_shares
            ))
            .size(14.0)
            .strong()
            .color(palette.window_bold_fg),
        );
        ui.add_space(6.0);

        let shares_snapshot = state.generated_shares.clone();
        for share in &shares_snapshot {
            Frame::NONE
                .fill(palette.window_bg_over)
                .stroke(Stroke::new(1.0, palette.divider))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "Share {} of {}",
                                share.index, share.total_shares
                            ))
                            .strong()
                            .size(13.5)
                            .color(palette.window_bold_fg),
                        );
                        Frame::NONE
                            .fill(palette.window_bg)
                            .stroke(Stroke::new(1.0, palette.divider))
                            .corner_radius(CornerRadius::same(4))
                            .inner_margin(Margin::symmetric(6, 2))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(format!("CHK {}", share.checksum_hex))
                                        .monospace()
                                        .size(10.5)
                                        .color(palette.window_sub_text_fg),
                                );
                            });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let is_copied = state.last_copied_share == Some(share.index);
                            let copy_lbl = if is_copied { "Copied" } else { "Copy Share" };
                            if ui.small_button(copy_lbl).clicked() {
                                ui.ctx().copy_text(share.encoded.clone());
                                state.last_copied_share = Some(share.index);
                                *outcome = BackupOutcome::Toast(format!(
                                    "Copied Share {} of {}",
                                    share.index, share.total_shares
                                ));
                            }
                        });
                    });
                    ui.add_space(6.0);
                    Frame::NONE
                        .fill(palette.window_bg)
                        .stroke(Stroke::new(1.0, palette.divider))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::symmetric(8, 6))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.add(
                                Label::new(
                                    RichText::new(&share.encoded)
                                        .monospace()
                                        .size(11.0)
                                        .color(palette.window_fg),
                                )
                                .wrap(),
                            );
                        });
                });
            ui.add_space(6.0);
        }
    }
}

fn render_recover_tab(
    ui: &mut Ui,
    state: &mut BackupUiState,
    backend: &mut dyn WalletBackend,
    palette: &Palette,
    outcome: &mut BackupOutcome,
) {
    let ok_color = success_color(palette);
    let mut valid_count = 0usize;
    let mut detected_threshold: Option<u8> = None;

    for raw in &state.recover_inputs {
        let trimmed = raw.trim();
        if !trimmed.is_empty()
            && let Ok(parsed) = PhraseShare::decode_string(trimmed)
        {
            valid_count += 1;
            if detected_threshold.is_none() {
                detected_threshold = Some(parsed.threshold);
            }
        }
    }
    let needed = usize::from(detected_threshold.unwrap_or(2));
    let ready = valid_count >= needed;

    Frame::NONE
        .fill(palette.window_bg_over)
        .stroke(Stroke::new(1.0, palette.divider))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("Paste Shamir Shares")
                        .size(14.0)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                let progress_color = if ready {
                    ok_color
                } else {
                    palette.window_sub_text_fg
                };
                ui.label(
                    RichText::new(format!("· {valid_count} of {needed} valid shares loaded"))
                        .size(12.0)
                        .strong()
                        .color(progress_color),
                );
            });
            ui.add_space(6.0);

            let mut remove_idx: Option<usize> = None;
            let input_count = state.recover_inputs.len();
            for idx in 0..input_count {
                let raw_val = state.recover_inputs[idx].trim().to_owned();
                let validation = if raw_val.is_empty() {
                    None
                } else {
                    Some(PhraseShare::decode_string(&raw_val))
                };

                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("Share slot #{}", idx + 1))
                            .size(12.0)
                            .strong()
                            .color(palette.window_fg),
                    );
                    match &validation {
                        Some(Ok(share)) => {
                            ui.label(
                                RichText::new(format!(
                                    "Valid · Share #{} ({}-of-{} · CHK {})",
                                    share.index,
                                    share.threshold,
                                    share.total_shares,
                                    format_checksum_hex(share.checksum)
                                ))
                                .size(11.5)
                                .color(ok_color),
                            );
                        }
                        Some(Err(err)) => {
                            ui.label(
                                RichText::new(format!("Invalid · {err}"))
                                    .size(11.5)
                                    .color(palette.attention_fg),
                            );
                        }
                        None => {}
                    }
                    if input_count > 2 {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Remove").clicked() {
                                remove_idx = Some(idx);
                            }
                        });
                    }
                });

                let resp = ui.add(
                    egui::TextEdit::singleline(&mut state.recover_inputs[idx])
                        .desired_width(ui.available_width())
                        .font(egui::TextStyle::Monospace)
                        .hint_text(format!("{PHRASE_SHARE_PREFIX}...")),
                );
                if resp.changed() {
                    state.reconstructed_words = None;
                    state.error = None;
                }
                ui.add_space(6.0);
            }
            if let Some(idx) = remove_idx {
                state.recover_inputs.remove(idx);
                state.reconstructed_words = None;
            }

            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        ready,
                        egui::Button::new(
                            RichText::new("Reconstruct Phrase")
                                .strong()
                                .color(palette.window_fg_active),
                        )
                        .fill(palette.window_bg_active)
                        .corner_radius(CornerRadius::same(8))
                        .min_size(Vec2::new(164.0, 34.0)),
                    )
                    .clicked()
                    && state.try_reconstruct().is_ok()
                {
                    *outcome = BackupOutcome::Toast("Recovery phrase reconstructed".to_owned());
                }

                if state.recover_inputs.len() < 10
                    && ui
                        .add(egui::Button::new("+ Add Share Slot").min_size(Vec2::new(0.0, 34.0)))
                        .clicked()
                {
                    state.recover_inputs.push(String::new());
                }

                if ui
                    .add(egui::Button::new("Clear").min_size(Vec2::new(0.0, 34.0)))
                    .clicked()
                {
                    state.recover_inputs = vec![String::new(), String::new()];
                    state.reconstructed_words = None;
                    state.show_import_form = false;
                    state.error = None;
                }
            });
        });

    if let Some(words) = state.reconstructed_words.clone() {
        ui.add_space(12.0);
        Frame::NONE
            .fill(palette.window_bg_over)
            .stroke(Stroke::new(1.5, ok_color))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new("Reconstructed 24-Word Recovery Phrase (Verified)")
                        .size(14.5)
                        .strong()
                        .color(ok_color),
                );
                ui.add_space(8.0);
                let col_spacing = if ui.available_width() < 360.0 {
                    12.0
                } else {
                    28.0
                };
                egui::Grid::new("tg_wallet_shamir_recovered_grid")
                    .num_columns(2)
                    .spacing(Vec2::new(col_spacing, 5.0))
                    .show(ui, |ui| {
                        for row in 0..12 {
                            let left = words.get(row).map(String::as_str).unwrap_or("");
                            let right = words.get(row + 12).map(String::as_str).unwrap_or("");
                            ui.label(
                                RichText::new(format!("{:>2}. {left}", row + 1))
                                    .monospace()
                                    .size(12.0)
                                    .color(palette.window_fg),
                            );
                            ui.label(
                                RichText::new(format!("{:>2}. {right}", row + 13))
                                    .monospace()
                                    .size(12.0)
                                    .color(palette.window_fg),
                            );
                            ui.end_row();
                        }
                    });

                ui.add_space(10.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Copy Phrase").clicked() {
                        ui.ctx().copy_text(words.join(" "));
                        *outcome = BackupOutcome::Toast("Reconstructed phrase copied".to_owned());
                    }
                    if ui.button("Import into Wallet...").clicked() {
                        state.show_import_form = !state.show_import_form;
                        state.error = None;
                    }
                });

                if state.show_import_form {
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new("Contract:")
                                .size(12.0)
                                .color(palette.window_fg),
                        );
                        for v in [
                            WalletVersion::V5R1,
                            WalletVersion::V4R2,
                            WalletVersion::V3R2,
                        ] {
                            ui.selectable_value(&mut state.import_version, v, v.label());
                        }
                    });
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new("Passcode (min 4 chars):")
                                .size(12.0)
                                .color(palette.window_fg),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut state.import_passcode)
                                .password(true)
                                .desired_width(140.0),
                        );
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new("Confirm passcode:")
                                .size(12.0)
                                .color(palette.window_fg),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut state.import_passcode_confirm)
                                .password(true)
                                .desired_width(140.0),
                        );
                    });
                    ui.add_space(8.0);
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Confirm Import & Encrypt")
                                    .strong()
                                    .color(palette.window_fg_active),
                            )
                            .fill(palette.window_bg_active)
                            .corner_radius(CornerRadius::same(8)),
                        )
                        .clicked()
                    {
                        if state.import_passcode.len() < 4 {
                            state.error =
                                Some("Passcode must be at least 4 characters.".to_owned());
                        } else if state.import_passcode != state.import_passcode_confirm {
                            state.error = Some("Passcodes do not match.".to_owned());
                        } else {
                            match backend.import_wallet(
                                &words,
                                "",
                                state.import_version,
                                &state.import_passcode,
                            ) {
                                Ok(_) => {
                                    state.clear_sensitive();
                                    *outcome = BackupOutcome::Imported(
                                        "Wallet restored from Shamir shares".to_owned(),
                                    );
                                }
                                Err(err) => state.error = Some(err),
                            }
                        }
                    }
                }
            });
    }
}
