//! Wallet Create, Import, Backup Verification, and Passcode Unlock UI views.
//!
//! Port origin: `Telegram/SourceFiles/wallet/wallet_content.cpp`,
//! `Telegram/SourceFiles/wallet/wallet_unlock.cpp`, and
//! `Telegram/SourceFiles/wallet/wallet_phrase_shares.cpp`.

use egui::{CornerRadius, Frame, Margin, RichText, ScrollArea, Stroke, Ui, Vec2};
use tg_wallet::contract::WalletVersion;
use tg_wallet::mnemonic::{
    MNEMONIC_WORD_COUNT, is_wordlist_word, validate_mnemonic, wordlist_suggestions,
};

use tg_app_core::stand_in::theme::Palette;

use super::backend::WalletBackend;

/// Active onboarding sub-step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnboardingStep {
    /// Landing screen with Create / Import buttons.
    #[default]
    Welcome,
    /// Showing generated 24-word recovery phrase.
    CreateShowPhrase,
    /// Setting local passcode for the newly created wallet.
    CreateSetPasscode,
    /// Importing an existing 24-word recovery phrase + passcode.
    ImportPhrase,
    /// Unlocking an already-configured locked vault.
    UnlockVault,
}

/// Stateful controller for Create / Import / Unlock wallet flows.
#[derive(Debug, Clone)]
pub struct OnboardingState {
    /// Current onboarding screen.
    pub step: OnboardingStep,
    /// Selected wallet contract version.
    pub version: WalletVersion,
    /// Newly generated 24-word phrase during Create flow.
    pub generated_words: Vec<String>,
    /// Confirmation check inputs for 3 sample word positions (indices 2, 11, 22 -> words #3, #12, #23).
    pub verify_inputs: [String; 3],
    /// Raw space/newline-separated phrase entered during Import flow.
    pub import_input: String,
    /// Optional TON mnemonic password during Import flow.
    pub import_password: String,
    /// Local vault passcode input.
    pub passcode: String,
    /// Local vault passcode confirmation input.
    pub passcode_confirm: String,
    /// Current validation or backend error message.
    pub error: Option<String>,
}

impl Default for OnboardingState {
    fn default() -> Self {
        Self {
            step: OnboardingStep::Welcome,
            version: WalletVersion::V5R1,
            generated_words: Vec::new(),
            verify_inputs: [String::new(), String::new(), String::new()],
            import_input: String::new(),
            import_password: String::new(),
            passcode: String::new(),
            passcode_confirm: String::new(),
            error: None,
        }
    }
}

/// Outcome returned by [`render_onboarding`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnboardingOutcome {
    /// Stay on onboarding screen.
    None,
    /// Wallet created, imported, or unlocked; navigate to Overview.
    Ready(String),
}

const VERIFY_INDICES: [usize; 3] = [2, 11, 22];

/// Renders the Create / Import / Unlock onboarding UI.
pub fn render_onboarding(
    ui: &mut Ui,
    state: &mut OnboardingState,
    backend: &mut dyn WalletBackend,
    palette: &Palette,
) -> OnboardingOutcome {
    let mut outcome = OnboardingOutcome::None;

    ScrollArea::vertical()
        .id_salt("tg_wallet_onboarding_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(12.0);
            match state.step {
                OnboardingStep::Welcome => {
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("TON Wallet")
                                .size(22.0)
                                .strong()
                                .color(palette.window_bold_fg),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(
                                "Non-custodial TON wallet with local passcode-encrypted key storage and TON Connect 2.0.",
                            )
                            .size(13.0)
                            .color(palette.window_sub_text_fg),
                        );
                        ui.add_space(16.0);
                        version_selector(ui, &mut state.version, palette);
                        ui.add_space(18.0);

                        if ui
                            .add_sized(
                                Vec2::new(240.0, 38.0),
                                egui::Button::new(
                                    RichText::new("Create New Wallet")
                                        .strong()
                                        .color(palette.window_fg_active),
                                )
                                .fill(palette.window_bg_active)
                                .corner_radius(CornerRadius::same(8)),
                            )
                            .clicked()
                        {
                            match backend.generate_phrase() {
                                Ok(words) => {
                                    state.generated_words = words;
                                    state.verify_inputs = Default::default();
                                    state.passcode.clear();
                                    state.passcode_confirm.clear();
                                    state.error = None;
                                    state.step = OnboardingStep::CreateShowPhrase;
                                }
                                Err(err) => state.error = Some(err),
                            }
                        }

                        ui.add_space(8.0);
                        if ui
                            .add_sized(
                                Vec2::new(240.0, 36.0),
                                egui::Button::new(
                                    RichText::new("Import Existing Wallet")
                                        .color(palette.window_active_text_fg),
                                )
                                .fill(palette.window_bg_over)
                                .corner_radius(CornerRadius::same(8)),
                            )
                            .clicked()
                        {
                            state.import_input.clear();
                            state.import_password.clear();
                            state.passcode.clear();
                            state.passcode_confirm.clear();
                            state.error = None;
                            state.step = OnboardingStep::ImportPhrase;
                        }
                    });
                }
                OnboardingStep::CreateShowPhrase => {
                    ui.label(
                        RichText::new("Your 24-Word Recovery Phrase")
                            .size(17.0)
                            .strong()
                            .color(palette.window_bold_fg),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            "Write down these 24 words in order and keep them offline. Anyone with this phrase controls the wallet.",
                        )
                        .size(12.5)
                        .color(palette.window_sub_text_fg),
                    );
                    ui.add_space(10.0);

                    Frame::NONE
                        .fill(palette.window_bg_over)
                        .stroke(Stroke::new(1.0, palette.divider))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(12))
                        .show(ui, |ui| {
                            egui::Grid::new("tg_wallet_phrase_grid")
                                .num_columns(2)
                                .spacing(Vec2::new(28.0, 6.0))
                                .show(ui, |ui| {
                                    for row in 0..12 {
                                        let left_idx = row;
                                        let right_idx = row + 12;
                                        let left = state
                                            .generated_words
                                            .get(left_idx)
                                            .map(String::as_str)
                                            .unwrap_or("");
                                        let right = state
                                            .generated_words
                                            .get(right_idx)
                                            .map(String::as_str)
                                            .unwrap_or("");
                                        ui.label(
                                            RichText::new(format!("{:>2}. {left}", left_idx + 1))
                                                .monospace()
                                                .color(palette.window_fg),
                                        );
                                        ui.label(
                                            RichText::new(format!("{:>2}. {right}", right_idx + 1))
                                                .monospace()
                                                .color(palette.window_fg),
                                        );
                                        ui.end_row();
                                    }
                                });
                        });

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Copy Phrase").clicked() {
                            ui.ctx().copy_text(state.generated_words.join(" "));
                        }
                        if ui.button("Back").clicked() {
                            state.step = OnboardingStep::Welcome;
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Continue to Passcode")
                                        .strong()
                                        .color(palette.window_fg_active),
                                )
                                .fill(palette.window_bg_active),
                            )
                            .clicked()
                        {
                            state.error = None;
                            state.step = OnboardingStep::CreateSetPasscode;
                        }
                    });
                }
                OnboardingStep::CreateSetPasscode => {
                    ui.label(
                        RichText::new("Verify Backup & Set Local Passcode")
                            .size(17.0)
                            .strong()
                            .color(palette.window_bold_fg),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("Confirm 3 words from your phrase and choose a local passcode to encrypt your key vault.")
                            .size(12.5)
                            .color(palette.window_sub_text_fg),
                    );
                    ui.add_space(10.0);

                    for (slot, &word_idx) in VERIFY_INDICES.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("Word #{}:", word_idx + 1))
                                    .color(palette.window_fg),
                            );
                            ui.text_edit_singleline(&mut state.verify_inputs[slot]);
                        });
                    }

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Passcode (min 4 chars):").color(palette.window_fg));
                        ui.add(egui::TextEdit::singleline(&mut state.passcode).password(true));
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Confirm passcode:").color(palette.window_fg));
                        ui.add(
                            egui::TextEdit::singleline(&mut state.passcode_confirm).password(true),
                        );
                    });

                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("Back").clicked() {
                            state.step = OnboardingStep::CreateShowPhrase;
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Finish & Encrypt Wallet")
                                        .strong()
                                        .color(palette.window_fg_active),
                                )
                                .fill(palette.window_bg_active),
                            )
                            .clicked()
                        {
                            let mut mismatch = None;
                            for (slot, &idx) in VERIFY_INDICES.iter().enumerate() {
                                let expected = state
                                    .generated_words
                                    .get(idx)
                                    .map(String::as_str)
                                    .unwrap_or("");
                                if !state.verify_inputs[slot]
                                    .trim()
                                    .eq_ignore_ascii_case(expected)
                                {
                                    mismatch = Some(format!(
                                        "Word #{} does not match your backup phrase",
                                        idx + 1
                                    ));
                                    break;
                                }
                            }
                            if let Some(err) = mismatch {
                                state.error = Some(err);
                            } else if state.passcode != state.passcode_confirm {
                                state.error = Some("Passcodes do not match".to_owned());
                            } else {
                                match backend.create_wallet(
                                    &state.generated_words,
                                    state.version,
                                    &state.passcode,
                                ) {
                                    Ok(addr) => {
                                        state.generated_words.clear();
                                        state.passcode.clear();
                                        state.passcode_confirm.clear();
                                        state.error = None;
                                        outcome = OnboardingOutcome::Ready(format!(
                                            "Wallet created: {}",
                                            addr.to_friendly(false, backend.app_config().testnet)
                                        ));
                                    }
                                    Err(err) => state.error = Some(err),
                                }
                            }
                        }
                    });
                }
                OnboardingStep::ImportPhrase => {
                    ui.label(
                        RichText::new("Import 24-Word TON Wallet")
                            .size(17.0)
                            .strong()
                            .color(palette.window_bold_fg),
                    );
                    ui.add_space(6.0);
                    version_selector(ui, &mut state.version, palette);
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("Enter your 24 recovery words separated by spaces:")
                            .size(12.5)
                            .color(palette.window_sub_text_fg),
                    );
                    ui.add(
                        egui::TextEdit::multiline(&mut state.import_input)
                            .desired_rows(4)
                            .desired_width(f32::INFINITY)
                            .hint_text("word1 word2 ... word24"),
                    );

                    let words: Vec<String> = state
                        .import_input
                        .split_whitespace()
                        .map(|w| w.to_ascii_lowercase())
                        .collect();
                    let invalid_words: Vec<&str> = words
                        .iter()
                        .filter(|w| !is_wordlist_word(w))
                        .map(String::as_str)
                        .collect();
                    let status_text = if words.is_empty() {
                        format!("0 / {MNEMONIC_WORD_COUNT} words")
                    } else if !invalid_words.is_empty() {
                        let suggestions = invalid_words
                            .first()
                            .map(|w| wordlist_suggestions(w, 3))
                            .unwrap_or_default();
                        if suggestions.is_empty() {
                            format!(
                                "{} / {MNEMONIC_WORD_COUNT} words — unknown: {}",
                                words.len(),
                                invalid_words[0]
                            )
                        } else {
                            format!(
                                "{} / {MNEMONIC_WORD_COUNT} words — unknown `{}` (did you mean: {}?)",
                                words.len(),
                                invalid_words[0],
                                suggestions.join(", ")
                            )
                        }
                    } else if words.len() == MNEMONIC_WORD_COUNT {
                        if validate_mnemonic(&words, &state.import_password) {
                            "24 / 24 words — valid TON phrase".to_owned()
                        } else {
                            "24 / 24 words — checksum mismatch (check word order)".to_owned()
                        }
                    } else {
                        format!("{} / {MNEMONIC_WORD_COUNT} words", words.len())
                    };
                    ui.label(
                        RichText::new(status_text)
                            .size(12.0)
                            .color(palette.window_sub_text_fg),
                    );

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Optional passphrase:").color(palette.window_fg));
                        ui.add(
                            egui::TextEdit::singleline(&mut state.import_password).password(true),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Local vault passcode:").color(palette.window_fg));
                        ui.add(egui::TextEdit::singleline(&mut state.passcode).password(true));
                    });

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Back").clicked() {
                            state.step = OnboardingStep::Welcome;
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Import Wallet")
                                        .strong()
                                        .color(palette.window_fg_active),
                                )
                                .fill(palette.window_bg_active),
                            )
                            .clicked()
                        {
                            match backend.import_wallet(
                                &words,
                                &state.import_password,
                                state.version,
                                &state.passcode,
                            ) {
                                Ok(addr) => {
                                    state.import_input.clear();
                                    state.import_password.clear();
                                    state.passcode.clear();
                                    state.error = None;
                                    outcome = OnboardingOutcome::Ready(format!(
                                        "Wallet imported: {}",
                                        addr.to_friendly(false, backend.app_config().testnet)
                                    ));
                                }
                                Err(err) => state.error = Some(err),
                            }
                        }
                    });
                }
                OnboardingStep::UnlockVault => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(24.0);
                        ui.label(
                            RichText::new("Wallet Locked")
                                .size(19.0)
                                .strong()
                                .color(palette.window_bold_fg),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("Enter your local wallet passcode to unlock signing keys.")
                                .size(13.0)
                                .color(palette.window_sub_text_fg),
                        );
                        ui.add_space(14.0);
                        let resp = ui.add_sized(
                            Vec2::new(220.0, 30.0),
                            egui::TextEdit::singleline(&mut state.passcode)
                                .password(true)
                                .hint_text("Passcode"),
                        );
                        let enter =
                            resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        ui.add_space(10.0);
                        if enter
                            || ui
                                .add_sized(
                                    Vec2::new(160.0, 34.0),
                                    egui::Button::new(
                                        RichText::new("Unlock")
                                            .strong()
                                            .color(palette.window_fg_active),
                                    )
                                    .fill(palette.window_bg_active),
                                )
                                .clicked()
                        {
                            match backend.unlock_vault(&state.passcode) {
                                Ok(()) => {
                                    state.passcode.clear();
                                    state.error = None;
                                    outcome = OnboardingOutcome::Ready("Wallet unlocked".to_owned());
                                }
                                Err(err) => state.error = Some(err),
                            }
                        }
                    });
                }
            }

            if let Some(err) = &state.error {
                ui.add_space(8.0);
                ui.label(RichText::new(err).color(palette.attention_fg).size(12.5));
            }
        });

    outcome
}

fn version_selector(ui: &mut Ui, version: &mut WalletVersion, palette: &Palette) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Contract version:").color(palette.window_sub_text_fg));
        for v in [
            WalletVersion::V5R1,
            WalletVersion::V4R2,
            WalletVersion::V3R2,
        ] {
            ui.selectable_value(version, v, v.label());
        }
    });
}
