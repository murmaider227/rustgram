//! Quick Replies list, Shortcut Messages editor, and `/` compose trigger preview.
//!
//! Port origin:
//! - `Telegram/SourceFiles/settings/business/settings_quick_replies.cpp`
//! - `Telegram/SourceFiles/settings/business/settings_shortcut_messages.cpp`
//! - `Telegram/SourceFiles/data/business/data_shortcut_messages.cpp`

use std::collections::BTreeMap;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Margin, Pos2, Rect, Sense, Stroke,
    Ui, Vec2,
};

use crate::business::backend::BusinessBackend;
use crate::business::model::{
    BusinessShortcutId, QUICK_REPLIES_LIMIT, QuickReplyAttachment, QuickReplyShortcut,
    SHORTCUT_MESSAGES_LIMIT, SHORTCUT_NAME_LIMIT, match_slash_quick_replies, valid_shortcut_name,
};
use crate::business::ui_helpers::{
    SUCCESS_GREEN, business_section_intro, pill_button, primary_button, section_divider,
    subsection_title, thin_divider, with_alpha,
};
use tg_app_core::stand_in::theme::Palette;

/// Interactive view-model for Quick Replies & Shortcut Messages.
pub struct QuickRepliesView {
    /// All loaded shortcuts keyed by ID.
    pub shortcuts: BTreeMap<BusinessShortcutId, QuickReplyShortcut>,
    /// Currently open shortcut ID when editing its message list (`None` = list screen).
    pub active_shortcut_id: Option<BusinessShortcutId>,
    /// Create-shortcut form visibility.
    pub creating_shortcut: bool,
    /// Draft name in "Add Shortcut" input.
    pub new_shortcut_name: String,
    /// Inline validation error for create/rename shortcut name.
    pub shortcut_name_error: Option<String>,
    /// Rename form active for `active_shortcut_id`.
    pub renaming_shortcut: bool,
    /// Draft name when renaming the active shortcut.
    pub rename_draft: String,
    /// Compose bar draft inside the Shortcut Messages editor.
    pub editor_message_draft: String,
    /// Selected attachment tag for the next shortcut message.
    pub editor_attachment: QuickReplyAttachment,
    /// Currently edited message `(message_id, draft_text)` inside the active shortcut.
    pub editing_message: Option<(i32, String)>,
    /// Interactive `/` compose sandbox input demonstrating quick reply slash-trigger in chats.
    pub compose_preview_input: String,
    /// Simulated sent chat log from the `/` compose preview.
    pub compose_preview_sent: Vec<String>,
}

impl QuickRepliesView {
    /// Creates a [`QuickRepliesView`] initialized with `shortcuts`.
    #[must_use]
    pub fn new(shortcuts: BTreeMap<BusinessShortcutId, QuickReplyShortcut>) -> Self {
        Self {
            shortcuts,
            active_shortcut_id: None,
            creating_shortcut: false,
            new_shortcut_name: String::new(),
            shortcut_name_error: None,
            renaming_shortcut: false,
            rename_draft: String::new(),
            editor_message_draft: String::new(),
            editor_attachment: QuickReplyAttachment::None,
            editing_message: None,
            compose_preview_input: "/".to_owned(),
            compose_preview_sent: vec![
                "Customer: Hi! Do you have pricing and weekend hours?".to_owned(),
            ],
        }
    }

    /// Synchronizes shortcuts from a refreshed snapshot.
    pub fn sync_shortcuts(&mut self, shortcuts: BTreeMap<BusinessShortcutId, QuickReplyShortcut>) {
        self.shortcuts = shortcuts;
        if let Some(id) = self.active_shortcut_id
            && !self.shortcuts.contains_key(&id)
        {
            self.active_shortcut_id = None;
        }
    }

    /// Opens the Shortcut Messages editor for `shortcut_id`.
    pub fn open_shortcut(&mut self, shortcut_id: BusinessShortcutId) {
        self.active_shortcut_id = Some(shortcut_id);
        self.renaming_shortcut = false;
        self.editing_message = None;
        self.shortcut_name_error = None;
    }

    /// Count of non-empty shortcuts (`ShortcutsCount` in `settings_recipients_helper.cpp`).
    #[must_use]
    pub fn active_shortcuts_count(&self) -> usize {
        self.shortcuts
            .values()
            .filter(|sc| !sc.messages.is_empty())
            .count()
    }

    /// Inserts all messages from `shortcut_id` into the `/` compose preview transcript.
    pub fn trigger_slash_shortcut(&mut self, shortcut_id: BusinessShortcutId) -> Option<String> {
        let sc = self.shortcuts.get(&shortcut_id)?.clone();
        for msg in &sc.messages {
            let line = match msg.attachment.badge() {
                Some(b) if !msg.text.is_empty() => format!("You (/{}) {b} {}", sc.name, msg.text),
                Some(b) => format!("You (/{}) {b}", sc.name),
                None => format!("You (/{}) {}", sc.name, msg.text),
            };
            self.compose_preview_sent.push(line);
        }
        self.compose_preview_input.clear();
        Some(format!(
            "Inserted {} message(s) from /{}",
            sc.messages.len(),
            sc.name
        ))
    }

    /// Renders either the Quick Replies list or the active Shortcut Messages editor.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn BusinessBackend,
        palette: &Palette,
    ) -> Option<String> {
        if let Some(shortcut_id) = self.active_shortcut_id {
            self.show_shortcut_editor(ui, shortcut_id, backend, palette)
        } else {
            self.show_shortcuts_list(ui, backend, palette)
        }
    }

    fn show_shortcuts_list(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn BusinessBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;

        business_section_intro(
            ui,
            "/R",
            SUCCESS_GREEN,
            "Quick Replies",
            "Create message presets and type /shortcut in any chat compose field to send them instantly.",
            palette,
        );

        Frame::NONE
            .inner_margin(Margin::symmetric(16, 10))
            .show(ui, |ui| {
                let count = self.active_shortcuts_count();
                ui.horizontal(|ui| {
                    subsection_title(
                        ui,
                        &format!("Your Shortcuts ({count}/{QUICK_REPLIES_LIMIT})"),
                        palette,
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if count < QUICK_REPLIES_LIMIT
                            && pill_button(
                                ui,
                                if self.creating_shortcut {
                                    "Cancel"
                                } else {
                                    "+ Add Shortcut"
                                },
                                palette.window_bg_active,
                                palette.window_fg_active,
                                None,
                            )
                            .clicked()
                        {
                            self.creating_shortcut = !self.creating_shortcut;
                            self.shortcut_name_error = None;
                        }
                    });
                });

                if self.creating_shortcut {
                    ui.add_space(8.0);
                    Frame::NONE
                        .fill(palette.window_bg_over)
                        .stroke(Stroke::new(1.0, palette.window_bg_active))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new("New Quick Reply Shortcut")
                                    .size(12.5)
                                    .strong()
                                    .color(palette.window_bold_fg),
                            );
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("/")
                                        .size(14.0)
                                        .strong()
                                        .color(palette.window_active_text_fg),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.new_shortcut_name)
                                        .hint_text("e.g. refund, address, catalog")
                                        .desired_width((ui.available_width() - 96.0).max(120.0)),
                                );
                                let len = self.new_shortcut_name.trim().chars().count();
                                ui.label(
                                    egui::RichText::new(format!("{len}/{SHORTCUT_NAME_LIMIT}"))
                                        .size(10.5)
                                        .color(palette.window_sub_text_fg),
                                );
                            });
                            if let Some(ref err) = self.shortcut_name_error {
                                ui.add_space(3.0);
                                ui.label(
                                    egui::RichText::new(err)
                                        .size(11.0)
                                        .color(palette.attention_fg),
                                );
                            }
                            ui.add_space(6.0);
                            if pill_button(
                                ui,
                                "Create & Edit Messages",
                                palette.window_bg_active,
                                palette.window_fg_active,
                                None,
                            )
                            .clicked()
                            {
                                let candidate = self.new_shortcut_name.trim().to_owned();
                                if !valid_shortcut_name(&candidate) {
                                    self.shortcut_name_error = Some(
                                        "Use 1-32 letters, numbers, or underscores (no spaces)."
                                            .to_owned(),
                                    );
                                } else {
                                    match backend.emplace_shortcut(&candidate) {
                                        Ok(sc) => {
                                            let id = sc.id;
                                            self.shortcuts.insert(id, sc);
                                            self.new_shortcut_name.clear();
                                            self.creating_shortcut = false;
                                            self.shortcut_name_error = None;
                                            self.open_shortcut(id);
                                            toast = Some(format!("Created shortcut /{candidate}"));
                                        }
                                        // local error: name and count checks, no request
                                        Err(e) => self.shortcut_name_error = Some(e), // local error: see above
                                    }
                                }
                            }
                        });
                }

                ui.add_space(8.0);

                // Reverse-order shortcut rows matching `shortcuts.list | ranges::views::reverse`
                let ordered: Vec<QuickReplyShortcut> =
                    self.shortcuts.values().rev().cloned().collect();
                let mut open_id = None;
                let mut delete_id = None;

                for sc in &ordered {
                    let width = ui.available_width().max(220.0);
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::new(width, 54.0), Sense::click());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(8),
                            palette.window_bg_over,
                        );
                    }

                    // Slash badge
                    let badge_rect = Rect::from_min_size(
                        Pos2::new(rect.left() + 6.0, rect.center().y - 16.0),
                        Vec2::splat(32.0),
                    );
                    ui.painter().rect_filled(
                        badge_rect,
                        CornerRadius::same(8),
                        with_alpha(SUCCESS_GREEN, 0.16),
                    );
                    ui.painter().text(
                        badge_rect.center(),
                        Align2::CENTER_CENTER,
                        "/",
                        FontId::proportional(16.0),
                        SUCCESS_GREEN,
                    );

                    // Shortcut command + message count
                    ui.painter().text(
                        Pos2::new(badge_rect.right() + 10.0, rect.top() + 17.0),
                        Align2::LEFT_CENTER,
                        format!("/{}", sc.name),
                        FontId::proportional(13.5),
                        palette.window_bold_fg,
                    );
                    let preview = truncate_preview(&sc.top_message_preview(), 52);
                    ui.painter().text(
                        Pos2::new(badge_rect.right() + 10.0, rect.top() + 36.0),
                        Align2::LEFT_CENTER,
                        preview,
                        FontId::proportional(11.0),
                        palette.window_sub_text_fg,
                    );

                    let msg_lbl = match sc.count() {
                        1 => "1 message".to_owned(),
                        n => format!("{n} messages"),
                    };
                    ui.painter().text(
                        Pos2::new(rect.right() - 22.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        msg_lbl,
                        FontId::proportional(11.5),
                        palette.window_active_text_fg,
                    );
                    ui.painter().text(
                        Pos2::new(rect.right() - 6.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        ">",
                        FontId::proportional(13.0),
                        palette.window_sub_text_fg,
                    );

                    if resp.clicked() {
                        open_id = Some(sc.id);
                    }
                    if resp.secondary_clicked() {
                        delete_id = Some(sc.id);
                    }
                    thin_divider(ui, palette);
                }

                if let Some(id) = open_id {
                    self.open_shortcut(id);
                }
                if let Some(id) = delete_id
                    && backend.remove_shortcut(id).is_ok()
                {
                    self.shortcuts.remove(&id);
                    toast = Some("Quick reply shortcut removed.".to_owned());
                }
            });

        section_divider(ui, palette);

        // Interactive "/" Compose Trigger Preview (`use via "/" in compose`)
        Frame::NONE
            .inner_margin(Margin::symmetric(16, 6))
            .show(ui, |ui| {
                subsection_title(ui, "Try Quick Replies via \"/\" in Compose", palette);
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "Type / followed by a shortcut name below to test live slash-autocomplete in chat compose.",
                    )
                    .size(11.5)
                    .color(palette.window_sub_text_fg),
                );
                ui.add_space(6.0);

                // Mini chat transcript canvas
                Frame::NONE
                    .fill(palette.chat_canvas_bg)
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let start = self.compose_preview_sent.len().saturating_sub(4);
                        for line in &self.compose_preview_sent[start..] {
                            let is_out = line.starts_with("You");
                            let bubble_bg = if is_out {
                                palette.msg_out_bg
                            } else {
                                palette.msg_in_bg
                            };
                            Frame::NONE
                                .fill(bubble_bg)
                                .corner_radius(CornerRadius::same(8))
                                .inner_margin(Margin::symmetric(10, 6))
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new(line)
                                            .size(11.5)
                                            .color(palette.window_fg),
                                    );
                                });
                            ui.add_space(4.0);
                        }
                    });

                ui.add_space(6.0);

                // Slash autocomplete popup when compose input starts with `/`
                let all_shortcuts: Vec<QuickReplyShortcut> =
                    self.shortcuts.values().cloned().collect();
                let mut triggered_id = None;
                if let Some(matches) =
                    match_slash_quick_replies(&all_shortcuts, &self.compose_preview_input)
                {
                    Frame::NONE
                        .fill(palette.window_bg)
                        .stroke(Stroke::new(1.0, palette.window_bg_active))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(6))
                        .show(ui, |ui| {
                            if matches.is_empty() {
                                ui.label(
                                    egui::RichText::new("No matching quick replies.")
                                        .size(11.5)
                                        .color(palette.window_sub_text_fg),
                                );
                            } else {
                                for sc in matches.iter().take(4) {
                                    let w = ui.available_width().max(180.0);
                                    let (r, resp) =
                                        ui.allocate_exact_size(Vec2::new(w, 34.0), Sense::click());
                                    if resp.hovered() {
                                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                        ui.painter().rect_filled(
                                            r,
                                            CornerRadius::same(6),
                                            palette.window_bg_over,
                                        );
                                    }
                                    ui.painter().text(
                                        Pos2::new(r.left() + 8.0, r.center().y),
                                        Align2::LEFT_CENTER,
                                        format!("/{}", sc.name),
                                        FontId::proportional(12.5),
                                        palette.window_active_text_fg,
                                    );
                                    ui.painter().text(
                                        Pos2::new(r.left() + 92.0, r.center().y),
                                        Align2::LEFT_CENTER,
                                        truncate_preview(&sc.top_message_preview(), 40),
                                        FontId::proportional(11.5),
                                        palette.window_sub_text_fg,
                                    );
                                    ui.painter().text(
                                        Pos2::new(r.right() - 8.0, r.center().y),
                                        Align2::RIGHT_CENTER,
                                        format!("{} msg", sc.count()),
                                        FontId::proportional(10.5),
                                        SUCCESS_GREEN,
                                    );
                                    if resp.clicked() {
                                        triggered_id = Some(sc.id);
                                    }
                                }
                            }
                        });
                    ui.add_space(4.0);
                }

                ui.horizontal(|ui| {
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.compose_preview_input)
                            .hint_text("Type /pricing or /thanks...")
                            .desired_width((ui.available_width() - 78.0).max(140.0)),
                    );
                    let send_clicked = pill_button(
                        ui,
                        "Send",
                        palette.window_bg_active,
                        palette.window_fg_active,
                        None,
                    )
                    .clicked();
                    if send_clicked
                        || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    {
                        if let Some(matches) =
                            match_slash_quick_replies(&all_shortcuts, &self.compose_preview_input)
                            && let Some(first) = matches.first()
                        {
                            triggered_id = Some(first.id);
                        } else if !self.compose_preview_input.trim().is_empty() {
                            self.compose_preview_sent
                                .push(format!("You: {}", self.compose_preview_input.trim()));
                            self.compose_preview_input.clear();
                        }
                    }
                });

                if let Some(id) = triggered_id {
                    toast = self.trigger_slash_shortcut(id);
                }
            });

        toast
    }

    fn show_shortcut_editor(
        &mut self,
        ui: &mut Ui,
        shortcut_id: BusinessShortcutId,
        backend: &mut dyn BusinessBackend,
        palette: &Palette,
    ) -> Option<String> {
        let mut toast = None;
        let Some(shortcut) = self.shortcuts.get(&shortcut_id).cloned() else {
            self.active_shortcut_id = None;
            return None;
        };

        // Top shortcut action bar
        Frame::NONE
            .fill(palette.window_bg_over)
            .inner_margin(Margin::symmetric(16, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if pill_button(
                        ui,
                        "< All Shortcuts",
                        palette.window_bg,
                        palette.window_active_text_fg,
                        Some(palette.divider),
                    )
                    .clicked()
                    {
                        self.active_shortcut_id = None;
                    }
                    ui.label(
                        egui::RichText::new(format!(
                            "/{}  ({}/{SHORTCUT_MESSAGES_LIMIT} messages)",
                            shortcut.name,
                            shortcut.count()
                        ))
                        .size(14.0)
                        .strong()
                        .color(palette.window_bold_fg),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if pill_button(
                            ui,
                            "Delete",
                            with_alpha(palette.attention_fg, 0.14),
                            palette.attention_fg,
                            None,
                        )
                        .clicked()
                            && backend.remove_shortcut(shortcut_id).is_ok()
                        {
                            self.shortcuts.remove(&shortcut_id);
                            self.active_shortcut_id = None;
                            toast = Some(format!("Deleted /{}", shortcut.name));
                        }
                        if pill_button(
                            ui,
                            "Rename",
                            palette.window_bg,
                            palette.window_fg,
                            Some(palette.divider),
                        )
                        .clicked()
                        {
                            self.renaming_shortcut = !self.renaming_shortcut;
                            self.rename_draft = shortcut.name.clone();
                            self.shortcut_name_error = None;
                        }
                    });
                });

                if self.renaming_shortcut {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label("/");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.rename_draft)
                                .desired_width((ui.available_width() - 90.0).max(120.0)),
                        );
                        if pill_button(
                            ui,
                            "Save",
                            palette.window_bg_active,
                            palette.window_fg_active,
                            None,
                        )
                        .clicked()
                        {
                            match backend.rename_shortcut(shortcut_id, &self.rename_draft) {
                                Ok(updated) => {
                                    self.shortcuts.insert(shortcut_id, updated.clone());
                                    self.renaming_shortcut = false;
                                    self.shortcut_name_error = None;
                                    toast = Some(format!("Renamed shortcut to /{}", updated.name));
                                }
                                Err(e) => {
                                    self.shortcut_name_error =
                                        tg_app_core::user_error::rename_quick_reply(&e).into_text();
                                }
                            }
                        }
                    });
                    if let Some(ref err) = self.shortcut_name_error {
                        ui.label(
                            egui::RichText::new(err)
                                .size(11.0)
                                .color(palette.attention_fg),
                        );
                    }
                }
            });

        // Message bubbles inside shortcut (`settings_shortcut_messages.cpp`)
        Frame::NONE
            .fill(palette.chat_canvas_bg)
            .inner_margin(Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if shortcut.messages.is_empty() {
                    Frame::NONE
                        .fill(with_alpha(palette.window_bg, 0.92))
                        .corner_radius(CornerRadius::same(12))
                        .inner_margin(Margin::same(14))
                        .show(ui, |ui| {
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "No messages in /{} yet",
                                        shortcut.name
                                    ))
                                    .size(13.5)
                                    .strong()
                                    .color(palette.window_bold_fg),
                                );
                                ui.label(
                                    egui::RichText::new(
                                        "Compose up to 20 text or media messages below.",
                                    )
                                    .size(11.5)
                                    .color(palette.window_sub_text_fg),
                                );
                            });
                        });
                } else {
                    for msg in &shortcut.messages {
                        Frame::NONE
                            .fill(palette.msg_out_bg)
                            .stroke(Stroke::new(1.0, with_alpha(palette.divider, 0.7)))
                            .corner_radius(CornerRadius::same(10))
                            .inner_margin(Margin::same(10))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                if let Some(badge) = msg.attachment.badge() {
                                    ui.label(
                                        egui::RichText::new(badge)
                                            .size(11.0)
                                            .strong()
                                            .color(palette.window_active_text_fg),
                                    );
                                }
                                let is_editing = self
                                    .editing_message
                                    .as_ref()
                                    .is_some_and(|(id, _)| *id == msg.id);
                                if is_editing {
                                    if let Some((_, ref mut draft)) = self.editing_message {
                                        ui.add(
                                            egui::TextEdit::multiline(draft)
                                                .desired_rows(2)
                                                .desired_width(ui.available_width()),
                                        );
                                    }
                                    ui.add_space(4.0);
                                    ui.horizontal(|ui| {
                                        if pill_button(
                                            ui,
                                            "Save Edit",
                                            palette.window_bg_active,
                                            palette.window_fg_active,
                                            None,
                                        )
                                        .clicked()
                                            && let Some((mid, draft)) = self.editing_message.take()
                                        {
                                            match backend.edit_shortcut_message(
                                                shortcut_id,
                                                mid,
                                                &draft,
                                            ) {
                                                Ok(updated) => {
                                                    if let Some(sc) =
                                                        self.shortcuts.get_mut(&shortcut_id)
                                                        && let Some(slot) = sc
                                                            .messages
                                                            .iter_mut()
                                                            .find(|m| m.id == mid)
                                                    {
                                                        *slot = updated;
                                                    }
                                                    toast = Some(
                                                        "Shortcut message updated.".to_owned(),
                                                    );
                                                }
                                                Err(e) => {
                                                    toast = tg_app_core::user_error::edit_quick_reply_message(&e)
                                                        .into_text();
                                                }
                                            }
                                        }
                                        if pill_button(
                                            ui,
                                            "Cancel",
                                            palette.window_bg,
                                            palette.window_sub_text_fg,
                                            Some(palette.divider),
                                        )
                                        .clicked()
                                        {
                                            self.editing_message = None;
                                        }
                                    });
                                } else {
                                    ui.label(
                                        egui::RichText::new(&msg.text)
                                            .size(12.5)
                                            .color(palette.window_fg),
                                    );
                                    ui.add_space(4.0);
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(&msg.time_label)
                                                .size(10.5)
                                                .color(palette.msg_date_fg),
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if ui
                                                    .small_button(
                                                        egui::RichText::new("Delete")
                                                            .color(palette.attention_fg),
                                                    )
                                                    .clicked()
                                                    && backend
                                                        .delete_shortcut_message(
                                                            shortcut_id,
                                                            msg.id,
                                                        )
                                                        .is_ok()
                                                {
                                                    if let Some(sc) =
                                                        self.shortcuts.get_mut(&shortcut_id)
                                                    {
                                                        sc.messages.retain(|m| m.id != msg.id);
                                                    }
                                                    toast = Some("Message deleted.".to_owned());
                                                }
                                                if ui.small_button("Edit").clicked() {
                                                    self.editing_message =
                                                        Some((msg.id, msg.text.clone()));
                                                }
                                            },
                                        );
                                    });
                                }
                            });
                        ui.add_space(6.0);
                    }
                }
            });

        // Bottom compose bar to add messages to the shortcut
        Frame::NONE
            .inner_margin(Margin::symmetric(16, 12))
            .show(ui, |ui| {
                subsection_title(ui, "Add Message to Shortcut", palette);
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new("Attachment:")
                            .size(11.5)
                            .color(palette.window_sub_text_fg),
                    );
                    let kinds = [
                        (QuickReplyAttachment::None, "Text Only"),
                        (QuickReplyAttachment::Photo, "Photo"),
                        (QuickReplyAttachment::Document, "Document"),
                        (QuickReplyAttachment::Voice, "Voice"),
                    ];
                    for (kind, label) in kinds {
                        let active = self.editor_attachment == kind;
                        if pill_button(
                            ui,
                            label,
                            if active {
                                palette.window_bg_active
                            } else {
                                palette.window_bg_over
                            },
                            if active {
                                Color32::WHITE
                            } else {
                                palette.window_fg
                            },
                            Some(palette.divider),
                        )
                        .clicked()
                        {
                            self.editor_attachment = kind;
                        }
                    }
                });
                ui.add_space(6.0);
                ui.add(
                    egui::TextEdit::multiline(&mut self.editor_message_draft)
                        .desired_rows(2)
                        .hint_text("Write a quick reply message...")
                        .desired_width(ui.available_width()),
                );
                ui.add_space(8.0);
                if primary_button(
                    ui,
                    "Add Message to Quick Reply",
                    palette.window_bg_active,
                    palette.window_fg_active,
                    36.0,
                )
                .clicked()
                {
                    match backend.add_shortcut_message(
                        shortcut_id,
                        &self.editor_message_draft,
                        self.editor_attachment,
                    ) {
                        Ok(msg) => {
                            if let Some(sc) = self.shortcuts.get_mut(&shortcut_id) {
                                sc.messages.push(msg);
                            }
                            self.editor_message_draft.clear();
                            self.editor_attachment = QuickReplyAttachment::None;
                            toast = Some("Message added to shortcut.".to_owned());
                        }
                        Err(e) => toast = tg_app_core::user_error::quick_reply_send(&e).into_text(),
                    }
                }
            });

        toast
    }
}

fn truncate_preview(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_owned()
    } else {
        let mut s: String = text.chars().take(max_chars.saturating_sub(1)).collect();
        s.push('…');
        s
    }
}
