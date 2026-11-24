//! Group Welcome Messages section (`HistoryView::WelcomeMessagesWidget`) and modal host.
//!
//! Port origin:
//! - `Telegram/SourceFiles/history/view/history_view_welcome_messages_section.h` / `.cpp`
//! - `Telegram/SourceFiles/data/components/welcome_messages.h` / `.cpp`

use std::collections::HashSet;

use egui::{CornerRadius, FontId, ScrollArea, TextEdit, Ui};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::Colors;
use tg_ui::tg_style::st;

use crate::info::backend::InfoBackend;
use tg_app_core::stand_in::types::PeerId;

pub use tg_app_core::core_types::dto::welcome_messages::*;

/// Stateful Welcome Messages history section (`history_view_welcome_messages_section.cpp`).
#[derive(Debug, Clone)]
pub struct WelcomeMessagesSectionWidget {
    /// Peer ID of the group whose welcome messages are edited.
    pub peer_id: PeerId,
    /// Group display title.
    pub peer_title: String,
    /// Maximum allowed welcome messages (`ephemeral_welcome_messages_max`).
    pub limit: usize,
    /// Loaded welcome message templates.
    pub messages: Vec<WelcomeMessageItem>,
    /// Whether initial fetch from backend ran.
    pub loaded: bool,
    /// Compose input buffer.
    pub compose_text: String,
    /// If `Some(id)`, the compose bar is currently editing template `id`.
    pub editing_id: Option<i32>,
    /// Multi-selected template IDs for batch deletion.
    pub selected_ids: HashSet<i32>,
    /// Optional error message.
    pub error: Option<String>,
}

impl Default for WelcomeMessagesSectionWidget {
    fn default() -> Self {
        Self::new(PeerId::ZERO, "")
    }
}

impl WelcomeMessagesSectionWidget {
    /// Creates a new [`WelcomeMessagesSectionWidget`] for `peer_id`.
    #[must_use]
    pub fn new(peer_id: PeerId, peer_title: impl Into<String>) -> Self {
        Self {
            peer_id,
            peer_title: peer_title.into(),
            limit: WELCOME_MESSAGES_DEFAULT_LIMIT,
            messages: Vec::new(),
            loaded: false,
            compose_text: String::new(),
            editing_id: None,
            selected_ids: HashSet::new(),
            error: None,
        }
    }

    /// Resets the widget to manage welcome messages for a different peer.
    pub fn open_for(&mut self, peer_id: PeerId, peer_title: impl Into<String>) {
        self.peer_id = peer_id;
        self.peer_title = peer_title.into();
        self.messages.clear();
        self.loaded = false;
        self.compose_text.clear();
        self.editing_id = None;
        self.selected_ids.clear();
        self.error = None;
    }

    /// Loads welcome messages from `backend` (`ephemeral.getWelcomeMessages`).
    pub fn reload(&mut self, backend: &dyn InfoBackend) {
        match backend.welcome_messages(self.peer_id) {
            Ok(list) => {
                self.messages = list;
                self.loaded = true;
                self.error = None;
            }
            Err(err) => {
                self.loaded = true;
                self.error = Some(err);
            }
        }
    }

    /// Returns `true` if the template count reached `self.limit` and we are not editing an existing item.
    #[must_use]
    pub fn at_limit(&self) -> bool {
        self.editing_id.is_none() && self.messages.len() >= self.limit
    }

    /// Starts editing an existing welcome message template.
    pub fn begin_edit(&mut self, id: i32) {
        if let Some(item) = self.messages.iter().find(|m| m.id == id) {
            self.editing_id = Some(id);
            self.compose_text = item.text.clone();
            self.error = None;
        }
    }

    /// Cancels an active edit.
    pub fn cancel_edit(&mut self) {
        self.editing_id = None;
        self.compose_text.clear();
    }

    /// Submits the compose bar (calls `ephemeral.sendMessage` or `ephemeral.editMessage`).
    pub fn submit_compose(&mut self, backend: &mut dyn InfoBackend) -> Result<(), String> {
        let trimmed = self.compose_text.trim().to_owned();
        if trimmed.is_empty() {
            return Err("Welcome message cannot be empty.".to_owned());
        }
        if let Some(edit_id) = self.editing_id {
            match backend.edit_welcome_message(self.peer_id, edit_id, &trimmed) {
                Ok(updated) => {
                    if let Some(slot) = self.messages.iter_mut().find(|m| m.id == edit_id) {
                        *slot = updated;
                    }
                    self.editing_id = None;
                    self.compose_text.clear();
                    self.error = None;
                    Ok(())
                }
                Err(err) => {
                    self.error = Some(err.clone());
                    Err(err)
                }
            }
        } else {
            if self.messages.len() >= self.limit {
                let msg = format!("You can add up to {} welcome messages.", self.limit);
                self.error = Some(msg.clone());
                return Err(msg);
            }
            match backend.send_welcome_message(self.peer_id, &trimmed) {
                Ok(created) => {
                    self.messages.push(created);
                    self.compose_text.clear();
                    self.error = None;
                    Ok(())
                }
                Err(err) => {
                    self.error = Some(err.clone());
                    Err(err)
                }
            }
        }
    }

    /// Deletes a single welcome message template (`ephemeral.deleteWelcomeMessage`).
    pub fn delete_one(&mut self, id: i32, backend: &mut dyn InfoBackend) -> Result<(), String> {
        backend.delete_welcome_message(self.peer_id, id)?;
        self.messages.retain(|m| m.id != id);
        self.selected_ids.remove(&id);
        if self.editing_id == Some(id) {
            self.cancel_edit();
        }
        self.error = None;
        Ok(())
    }

    /// Deletes all welcome message templates (`ephemeral.deleteAllWelcomeMessages`).
    pub fn delete_all(&mut self, backend: &mut dyn InfoBackend) -> Result<(), String> {
        backend.delete_all_welcome_messages(self.peer_id)?;
        self.messages.clear();
        self.selected_ids.clear();
        self.cancel_edit();
        self.error = None;
        Ok(())
    }

    /// Renders the full Welcome Messages section (header, template bubbles, compose bar).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn InfoBackend,
        show_back_button: bool,
        colors: &dyn Colors,
    ) -> Option<WelcomeMessagesAction> {
        if !self.loaded {
            self.reload(backend);
        }
        let mut action = None;

        // Top bar
        ui.horizontal(|ui| {
            if show_back_button && tg_app_core::ui_icons::back_button(ui, "Back").clicked() {
                action = Some(WelcomeMessagesAction::Back);
            }
            ui.label(
                egui::RichText::new("Welcome Messages")
                    .font(FontId::new(15.0, family_semibold()))
                    .color(colors.color("windowBoldFg")),
            );
            ui.label(
                egui::RichText::new(format!("({} / {})", self.messages.len(), self.limit))
                    .font(FontId::new(12.5, family_regular()))
                    .color(colors.color("windowSubTextFg")),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !self.messages.is_empty() && ui.small_button("Delete All").clicked() {
                    match self.delete_all(backend) {
                        Ok(()) => {
                            action = Some(WelcomeMessagesAction::Toast(
                                "Deleted all welcome messages".to_owned(),
                            ));
                        }
                        Err(err) => self.error = Some(err),
                    }
                }
            });
        });

        if !self.peer_title.is_empty() {
            ui.label(
                egui::RichText::new(format!(
                    "Shown privately to new members when they join {}.",
                    self.peer_title
                ))
                .font(FontId::new(12.0, family_regular()))
                .color(colors.color("windowSubTextFg")),
            );
        }
        ui.add_space(8.0);

        // Message list
        let list_h = (ui.available_height() - 96.0).clamp(120.0, 320.0);
        ScrollArea::vertical()
            .id_salt(("welcome_msgs_scroll", self.peer_id.0))
            .max_height(list_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.messages.is_empty() {
                    egui::Frame::NONE
                        .fill(colors.color("windowBgOver"))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(14.0)
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new("No welcome messages yet")
                                    .font(FontId::new(14.0, family_semibold()))
                                    .color(colors.color("windowBoldFg")),
                            );
                            ui.label(
                                egui::RichText::new(
                                    "Add up to 5 welcome messages below. New members will see them privately when they enter the group.",
                                )
                                .font(FontId::new(12.5, family_regular()))
                                .color(colors.color("windowSubTextFg")),
                            );
                        });
                } else {
                    let snapshot = self.messages.clone();
                    for item in snapshot {
                        egui::Frame::NONE
                            .fill(colors.color("msgOutBg"))
                            .corner_radius(CornerRadius::same(10))
                            .inner_margin(10.0)
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(&item.text)
                                        .font(FontId::new(13.5, family_regular()))
                                        .color(colors.color("windowFg")),
                                );
                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!("Template #{}", item.id))
                                            .font(FontId::new(11.0, family_regular()))
                                            .color(colors.color("windowSubTextFg")),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.small_button("Delete").clicked() {
                                                match self.delete_one(item.id, backend) {
                                                    Ok(()) => {
                                                        action =
                                                            Some(WelcomeMessagesAction::Changed);
                                                    }
                                                    Err(err) => self.error = Some(err),
                                                }
                                            }
                                            if ui.small_button("Edit").clicked() {
                                                self.begin_edit(item.id);
                                            }
                                        },
                                    );
                                });
                            });
                        ui.add_space(6.0);
                    }
                }
            });

        ui.add_space(8.0);

        // Editing banner
        if let Some(edit_id) = self.editing_id {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("Editing template #{edit_id}"))
                        .font(FontId::new(12.0, family_semibold()))
                        .color(colors.color("windowActiveTextFg")),
                );
                if ui.small_button("Cancel Edit").clicked() {
                    self.cancel_edit();
                }
            });
        }

        // Bottom compose bar
        let disabled = self.at_limit();
        ui.add_enabled_ui(!disabled, |ui| {
            ui.horizontal(|ui| {
                let input_w = (ui.available_width() - 78.0).max(120.0);
                ui.add_sized(
                    [input_w, 32.0],
                    TextEdit::singleline(&mut self.compose_text)
                        .hint_text("Write a welcome message..."),
                );
                let btn_label = if self.editing_id.is_some() {
                    "Save"
                } else {
                    "Add"
                };
                if ui.button(btn_label).clicked() {
                    match self.submit_compose(backend) {
                        Ok(()) => action = Some(WelcomeMessagesAction::Changed),
                        Err(err) => self.error = Some(err),
                    }
                }
            });
        });

        if disabled {
            ui.label(
                egui::RichText::new(format!(
                    "Limit reached ({} / {}). Delete or edit an existing message.",
                    self.messages.len(),
                    self.limit
                ))
                .font(FontId::new(12.0, family_regular()))
                .color(colors.color("windowSubTextFg")),
            );
        }

        if let Some(ref err) = self.error {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("boxTextFgError")),
            );
        }

        action
    }
}

/// Modal dialog wrapper hosting [`WelcomeMessagesSectionWidget`] from Group/Channel settings.
#[derive(Debug, Clone)]
pub struct WelcomeMessagesBoxState {
    /// Embedded section widget.
    pub section: WelcomeMessagesSectionWidget,
}

impl WelcomeMessagesBoxState {
    /// Creates a new [`WelcomeMessagesBoxState`] for `peer_id`.
    #[must_use]
    pub fn new(peer_id: PeerId, peer_title: impl Into<String>) -> Self {
        Self {
            section: WelcomeMessagesSectionWidget::new(peer_id, peer_title),
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, "Welcome Messages")
            .width(st::box_wide_width.0 as f32 + 40.0)
            .add_button(BoxButton::default_btn("close", tr::lng_close()))
    }

    /// Renders the welcome messages section inside the modal body.
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn InfoBackend,
        colors: &dyn Colors,
    ) -> Option<WelcomeMessagesAction> {
        self.section.show(ui, backend, false, colors)
    }
}
