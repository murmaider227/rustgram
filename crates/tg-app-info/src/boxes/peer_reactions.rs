//! Per-chat allowed reactions editor box (`messages.setChatAvailableReactions`).
//!
//! Port origin: `Telegram/SourceFiles/boxes/peers/edit_peer_reactions.h`
//! and `Telegram/SourceFiles/boxes/peers/edit_peer_reactions.cpp`.

use egui::{Align2, Color32, CornerRadius, CursorIcon, FontId, Role, Sense, Ui, Vec2, WidgetInfo};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, FlatLabelStyle, ToggleStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::checkbox::Toggle;
use tg_ui::widgets::labels::{FlatLabel, divider_bar};

use crate::info::backend::{
    ChatAvailableReactionsMode, InfoBackend, PeerReactionsConfig, default_standard_reactions,
};
use tg_app_core::stand_in::types::PeerId;

/// Minimum allowed unique reactions per message.
pub const MIN_REACTIONS_LIMIT: i32 = 1;
/// Maximum allowed unique reactions per message (`edit_peer_reactions.cpp`).
pub const MAX_REACTIONS_LIMIT: i32 = 11;

/// High-level mode radio choice in [`EditPeerReactionsBoxState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReactionsModeChoice {
    /// All reactions allowed (`chatReactionsAll`).
    All,
    /// Only chosen reactions allowed (`chatReactionsSome`).
    Some,
    /// Reactions disabled (`chatReactionsNone`).
    None,
}

/// Stateful Per-Chat Reactions settings box (`EditPeerReactionsBox`).
#[derive(Debug, Clone)]
pub struct EditPeerReactionsBoxState {
    /// Target group or channel peer ID.
    pub peer_id: PeerId,
    /// Peer display title.
    pub peer_title: String,
    /// Whether this peer is a broadcast channel (shows Paid Star Reactions toggle).
    pub is_channel: bool,
    /// Selected reactions mode.
    pub mode: ReactionsModeChoice,
    /// Whether custom emoji reactions are allowed when `mode == All`.
    pub allow_custom: bool,
    /// Standard emoji list enabled when `mode == Some`.
    pub selected_emojis: Vec<String>,
    /// Custom emoji document IDs enabled when `mode == Some`.
    pub custom_emoji_ids: Vec<i64>,
    /// Maximum unique reactions per message (`1..=11`).
    pub reactions_limit: i32,
    /// Whether paid Star reactions are enabled (`paid_enabled`).
    pub paid_enabled: bool,
    /// Full catalog of available standard reactions.
    pub available_standard: Vec<String>,
    /// Optional validation/RPC error message.
    pub error: Option<String>,
}

impl EditPeerReactionsBoxState {
    /// Initializes the reactions editor from a peer's current [`PeerReactionsConfig`].
    #[must_use]
    pub fn new(
        peer_id: PeerId,
        peer_title: impl Into<String>,
        is_channel: bool,
        config: PeerReactionsConfig,
    ) -> Self {
        let available_standard = if config.available_standard.is_empty() {
            default_standard_reactions()
        } else {
            config.available_standard
        };
        let (mode, allow_custom, selected_emojis, custom_emoji_ids) = match config.mode {
            ChatAvailableReactionsMode::None => (
                ReactionsModeChoice::None,
                false,
                vec!["👍".to_owned(), "👎".to_owned()],
                Vec::new(),
            ),
            ChatAvailableReactionsMode::All { allow_custom } => (
                ReactionsModeChoice::All,
                allow_custom,
                vec!["👍".to_owned(), "👎".to_owned()],
                Vec::new(),
            ),
            ChatAvailableReactionsMode::Some {
                emojis,
                custom_emoji_ids,
            } => (ReactionsModeChoice::Some, false, emojis, custom_emoji_ids),
        };
        Self {
            peer_id,
            peer_title: peer_title.into(),
            is_channel,
            mode,
            allow_custom,
            selected_emojis,
            custom_emoji_ids,
            reactions_limit: config
                .reactions_limit
                .unwrap_or(MAX_REACTIONS_LIMIT)
                .clamp(MIN_REACTIONS_LIMIT, MAX_REACTIONS_LIMIT),
            paid_enabled: config.paid_enabled,
            available_standard,
            error: None,
        }
    }

    /// Toggles inclusion of `emoji` when editing `ReactionsModeChoice::Some`.
    pub fn toggle_emoji(&mut self, emoji: &str) {
        if let Some(pos) = self.selected_emojis.iter().position(|e| e == emoji) {
            self.selected_emojis.remove(pos);
        } else {
            self.selected_emojis.push(emoji.to_owned());
        }
    }

    /// Converts current UI state into a [`PeerReactionsConfig`].
    #[must_use]
    pub fn to_config(&self) -> PeerReactionsConfig {
        let mode = match self.mode {
            ReactionsModeChoice::None => ChatAvailableReactionsMode::None,
            ReactionsModeChoice::All => ChatAvailableReactionsMode::All {
                allow_custom: self.allow_custom,
            },
            ReactionsModeChoice::Some => {
                if self.selected_emojis.is_empty() && self.custom_emoji_ids.is_empty() {
                    ChatAvailableReactionsMode::None
                } else {
                    ChatAvailableReactionsMode::Some {
                        emojis: self.selected_emojis.clone(),
                        custom_emoji_ids: self.custom_emoji_ids.clone(),
                    }
                }
            }
        };
        PeerReactionsConfig {
            mode,
            reactions_limit: Some(
                self.reactions_limit
                    .clamp(MIN_REACTIONS_LIMIT, MAX_REACTIONS_LIMIT),
            ),
            paid_enabled: self.paid_enabled,
            available_standard: self.available_standard.clone(),
        }
    }

    /// Persists the updated reactions configuration via `messages.setChatAvailableReactions`.
    pub fn submit(&mut self, backend: &mut dyn InfoBackend) -> Result<(), String> {
        let cfg = self.to_config();
        match backend.set_peer_available_reactions(self.peer_id, cfg) {
            Ok(()) => {
                self.error = None;
                Ok(())
            }
            Err(err) => {
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, "Reactions")
            .width(st::box_wide_width.0 as f32)
            .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
            .add_button(BoxButton::default_btn(
                "save_reactions",
                tr::lng_settings_save(),
            ))
    }

    /// Renders mode radio options, per-reaction toggle grid, max reactions limit, and Star reactions toggle.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        FlatLabel::plain(
            &format!("Available Reactions — {}", self.peer_title),
            FlatLabelStyle::subsection_title(colors),
        )
        .id_salt("peer_reactions_hdr")
        .show(ui);
        ui.add_space(6.0);

        for (choice, label, desc) in [
            (
                ReactionsModeChoice::All,
                "All Reactions",
                "Allow every standard reaction in this chat.",
            ),
            (
                ReactionsModeChoice::Some,
                "Some Reactions",
                "Choose specific reactions allowed on messages.",
            ),
            (
                ReactionsModeChoice::None,
                "No Reactions",
                "Disable message reactions in this chat.",
            ),
        ] {
            let selected = self.mode == choice;
            if ui.radio(selected, label).clicked() {
                self.mode = choice;
                self.error = None;
            }
            ui.label(
                egui::RichText::new(desc)
                    .font(FontId::new(11.5, family_regular()))
                    .color(colors.color("windowSubTextFg")),
            );
            ui.add_space(4.0);
        }

        if self.mode == ReactionsModeChoice::All {
            ui.add_space(6.0);
            divider_bar(ui, 6.0, colors);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Allow Custom Emoji Reactions")
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowFg")),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    Toggle::new(&mut self.allow_custom, ToggleStyle::default_toggle(colors))
                        .show(ui);
                });
            });
        }

        if self.mode == ReactionsModeChoice::Some {
            ui.add_space(6.0);
            divider_bar(ui, 6.0, colors);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "Selected ({}/{})",
                        self.selected_emojis.len(),
                        self.available_standard.len()
                    ))
                    .font(FontId::new(12.5, family_semibold()))
                    .color(colors.color("windowSubTextFg")),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Clear").clicked() {
                        self.selected_emojis.clear();
                    }
                    if ui.small_button("Select All").clicked() {
                        self.selected_emojis = self.available_standard.clone();
                    }
                });
            });
            ui.add_space(6.0);
            let catalog = self.available_standard.clone();
            ui.horizontal_wrapped(|ui| {
                for emoji in &catalog {
                    let enabled = self.selected_emojis.iter().any(|e| e == emoji);
                    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(38.0), Sense::click());
                    resp.widget_info(|| {
                        WidgetInfo::selected(
                            Role::Button,
                            true,
                            enabled,
                            format!("Reaction {emoji}"),
                        )
                    });
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    let bg = if enabled {
                        colors.color("windowBgActive")
                    } else if resp.hovered() {
                        colors.color("windowBgOver")
                    } else {
                        Color32::TRANSPARENT
                    };
                    ui.painter().rect_filled(rect, CornerRadius::same(8), bg);
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        emoji,
                        FontId::proportional(20.0),
                        colors.color("windowFg"),
                    );
                    if resp.clicked() {
                        self.toggle_emoji(emoji);
                    }
                }
            });
        }

        if self.mode != ReactionsModeChoice::None {
            ui.add_space(8.0);
            divider_bar(ui, 6.0, colors);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "Max Reactions per Post: {}",
                        self.reactions_limit
                    ))
                    .font(FontId::new(12.5, family_semibold()))
                    .color(colors.color("windowFg")),
                );
                ui.add(
                    egui::Slider::new(
                        &mut self.reactions_limit,
                        MIN_REACTIONS_LIMIT..=MAX_REACTIONS_LIMIT,
                    )
                    .show_value(false),
                );
            });
        }

        if self.is_channel {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Enable Paid Star Reactions")
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowFg")),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    Toggle::new(&mut self.paid_enabled, ToggleStyle::default_toggle(colors))
                        .show(ui);
                });
            });
        }

        if let Some(ref err) = self.error {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        }
    }
}
