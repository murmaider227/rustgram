//! Rich compose helpers for `ComposeControls`: mention autocomplete popup inserting
//! `EntityType::MentionName`, custom emoji quick picker, rich draft snapshot with entities,
//! and history keyboard shortcuts (`Up` arrow to edit last outgoing message, `Ctrl+Up`/`Down`
//! to cycle reply targets).
//!
//! Port origin:
//! - `Telegram/SourceFiles/chat_helpers/field_autocomplete.cpp`
//! - `Telegram/SourceFiles/chat_helpers/message_field.cpp`
//! - `Telegram/SourceFiles/history/history_widget.cpp` (`editLastMessage`, `replyToPreviousMessage`)

use egui::{
    Align2, Area, Color32, CornerRadius, CursorIcon, FontId, Frame, Id, Key, Margin, Order, Pos2,
    Rect, Sense, Stroke, Ui, Vec2,
};
use tg_chat::autocomplete::{AutocompleteKind, parse_mention_hashtag_bot_command_query};
use tg_chat::types::ComposeFeatures;
use tg_text::{EntityInText, EntityType, TextWithEntities, utf8_to_utf16_offset};
use tg_ui::widgets::rich_input::{FormatAction, RichEditState};

use crate::history::backend::HistoryMessage;
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::types::MsgId;

pub use tg_app_core::core_types::dto::rich_compose::*;

/// Active `@mention` autocomplete popup state above the compose field.
#[derive(Debug, Clone, Default)]
pub struct MentionAutocompleteState {
    /// Currently matched `@query` (empty when popup is hidden).
    pub query: String,
    /// Character range `[start_char, end_char)` in the compose text to replace on selection.
    pub replace_char_range: (usize, usize),
    /// Filtered candidates matching `query`.
    pub matches: Vec<MentionCandidate>,
    /// Highlighted row index for keyboard navigation.
    pub selected_index: usize,
    /// Dismissed token string (prevents re-opening after Escape until user edits).
    pub dismissed_query: Option<String>,
}

impl MentionAutocompleteState {
    /// Returns `true` when the popup has active matches to display.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        !self.matches.is_empty()
    }

    /// Clears popup state.
    pub fn clear(&mut self) {
        self.query.clear();
        self.matches.clear();
        self.selected_index = 0;
        self.dismissed_query = None;
    }

    /// Recomputes active `@mention` autocomplete matches at `rich.cursor_char` using
    /// `tg_chat::autocomplete::parse_mention_hashtag_bot_command_query`.
    pub fn update(&mut self, rich: &RichEditState, candidates: &[MentionCandidate]) {
        let (sel_a, sel_b) = rich.normalized_selection();
        let has_selection = sel_a != sel_b;
        let cursor_byte = rich
            .text
            .char_indices()
            .nth(rich.cursor_char)
            .map_or(rich.text.len(), |(b, _)| b);

        let parsed = parse_mention_hashtag_bot_command_query(
            &rich.text,
            cursor_byte,
            0,
            has_selection,
            ComposeFeatures::default(),
        );

        let Some(q) = parsed.filter(|q| q.kind == AutocompleteKind::Mentions) else {
            self.query.clear();
            self.matches.clear();
            self.selected_index = 0;
            return;
        };

        if self.dismissed_query.as_deref() == Some(q.query.as_str()) {
            self.matches.clear();
            return;
        }

        let needle = q.query.trim_start_matches('@').to_lowercase();
        let start_char = rich.text[..q.start_byte].chars().count();
        let end_char = rich.text[..q.end_byte].chars().count();

        self.query = q.query;
        self.replace_char_range = (start_char, end_char);
        self.matches = candidates
            .iter()
            .filter(|c| {
                needle.is_empty()
                    || c.display_name.to_lowercase().contains(&needle)
                    || c.username
                        .as_deref()
                        .is_some_and(|u| u.to_lowercase().contains(&needle))
            })
            .take(6)
            .cloned()
            .collect();
        if self.selected_index >= self.matches.len() {
            self.selected_index = 0;
        }
    }

    /// Handles keyboard navigation (`ArrowUp`/`ArrowDown`/`Tab`/`Enter`/`Escape`) when visible.
    ///
    /// Returns the chosen [`MentionCandidate`] if confirmed.
    pub fn handle_keys(&mut self, ctx: &egui::Context) -> Option<MentionCandidate> {
        if self.matches.is_empty() {
            return None;
        }
        let mut chosen = None;
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, Key::Escape) {
                self.dismissed_query = Some(self.query.clone());
                self.matches.clear();
            } else if i.consume_key(egui::Modifiers::NONE, Key::ArrowDown) {
                self.selected_index = (self.selected_index + 1) % self.matches.len();
            } else if i.consume_key(egui::Modifiers::NONE, Key::ArrowUp) {
                self.selected_index = if self.selected_index == 0 {
                    self.matches.len() - 1
                } else {
                    self.selected_index - 1
                };
            } else if i.consume_key(egui::Modifiers::NONE, Key::Tab)
                || i.consume_key(egui::Modifiers::NONE, Key::Enter)
            {
                chosen = self.matches.get(self.selected_index).cloned();
                self.matches.clear();
            }
        });
        chosen
    }

    /// Renders the floating `@mention` autocomplete popup above `compose_rect`.
    ///
    /// Returns the clicked [`MentionCandidate`] if the user clicked a row.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        compose_rect: Rect,
        colors: &HistoryColors,
    ) -> Option<MentionCandidate> {
        if self.matches.is_empty() {
            return None;
        }
        let row_h = 38.0_f32;
        let popup_h = self.matches.len() as f32 * row_h + 12.0;
        let popup_w = compose_rect.width().clamp(200.0, 320.0);
        let pos = Pos2::new(
            compose_rect.left() + 12.0,
            (compose_rect.top() - popup_h - 4.0).max(8.0),
        );
        let mut clicked_candidate = None;

        Area::new(Id::unique("tg_mention_autocomplete_popup"))
            .order(Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                Frame::NONE
                    .fill(colors.bar_bg)
                    .stroke(Stroke::new(1.0, colors.divider))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(6))
                    .show(ui, |ui| {
                        ui.set_width(popup_w - 12.0);
                        for (idx, cand) in self.matches.iter().enumerate() {
                            let (row_rect, resp) = ui.allocate_exact_size(
                                Vec2::new(popup_w - 12.0, row_h),
                                Sense::click(),
                            );
                            let selected = idx == self.selected_index || resp.hovered();
                            if resp.hovered() {
                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            }
                            if selected {
                                ui.painter().rect_filled(
                                    row_rect,
                                    CornerRadius::same(6),
                                    colors.bar_bg_over,
                                );
                            }
                            let [r, g, b] = cand.avatar_rgb;
                            let av_c = Pos2::new(row_rect.left() + 18.0, row_rect.center().y);
                            ui.painter()
                                .circle_filled(av_c, 13.0, Color32::from_rgb(r, g, b));
                            let initial = cand
                                .display_name
                                .chars()
                                .next()
                                .unwrap_or('?')
                                .to_uppercase()
                                .to_string();
                            ui.painter().text(
                                av_c,
                                Align2::CENTER_CENTER,
                                initial,
                                FontId::proportional(11.5),
                                Color32::WHITE,
                            );

                            ui.painter().text(
                                Pos2::new(row_rect.left() + 38.0, row_rect.top() + 5.0),
                                Align2::LEFT_TOP,
                                &cand.display_name,
                                FontId::proportional(13.0),
                                colors.bold_fg,
                            );
                            let sub = cand
                                .username
                                .as_deref()
                                .map_or_else(|| "Mention by name".to_owned(), |u| format!("@{u}"));
                            ui.painter().text(
                                Pos2::new(row_rect.left() + 38.0, row_rect.top() + 21.0),
                                Align2::LEFT_TOP,
                                sub,
                                FontId::proportional(11.0),
                                colors.sub_text_fg,
                            );

                            if resp.clicked() {
                                clicked_candidate = Some(cand.clone());
                            }
                        }
                    });
            });

        if clicked_candidate.is_some() {
            self.matches.clear();
        }
        clicked_candidate
    }
}

/// Inserts a [`MentionCandidate`] into `rich` at `replace_range`, creating a `MentionName` entity
/// (`EntityType::MentionName` with `"userId.accessHash:selfId"` data).
pub fn insert_mention_candidate(
    rich: &mut RichEditState,
    candidate: &MentionCandidate,
    self_user_id: u64,
    replace_range: (usize, usize),
) {
    rich.insert_mention_name(
        &candidate.display_name,
        candidate.user_id,
        candidate.access_hash,
        self_user_id,
        replace_range,
    );
}

/// Navigation shortcut detected in the compose bar for quick editing/replying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeHistoryShortcut {
    /// `Up` arrow pressed in empty compose field -> edit last outgoing message.
    EditLastOutgoing,
    /// `Ctrl+Up` -> reply to previous (older) message.
    ReplyPrevious,
    /// `Ctrl+Down` -> reply to next (newer) message (or cancel reply at bottom).
    ReplyNext,
}

/// Detects `Up` (empty field -> edit last message) and `Ctrl+Up` / `Ctrl+Down` (reply cycling).
pub fn detect_compose_history_shortcut(
    ctx: &egui::Context,
    field_is_empty: bool,
    is_normal_mode: bool,
    autocomplete_open: bool,
) -> Option<ComposeHistoryShortcut> {
    if autocomplete_open {
        return None;
    }
    ctx.input_mut(|i| {
        let ctrl = i.modifiers.ctrl || i.modifiers.command;
        if ctrl && !i.modifiers.shift && !i.modifiers.alt {
            if i.consume_key(i.modifiers, Key::ArrowUp) {
                return Some(ComposeHistoryShortcut::ReplyPrevious);
            }
            if i.consume_key(i.modifiers, Key::ArrowDown) {
                return Some(ComposeHistoryShortcut::ReplyNext);
            }
        }
        if field_is_empty
            && is_normal_mode
            && !ctrl
            && !i.modifiers.shift
            && !i.modifiers.alt
            && i.consume_key(egui::Modifiers::NONE, Key::ArrowUp)
        {
            return Some(ComposeHistoryShortcut::EditLastOutgoing);
        }
        None
    })
}

/// Finds the latest editable outgoing non-service message in `messages` (for `Up` arrow edit).
#[must_use]
pub fn find_last_editable_outgoing(messages: &[HistoryMessage]) -> Option<&HistoryMessage> {
    messages
        .iter()
        .rev()
        .find(|m| m.outgoing && !m.is_service() && (!m.text.is_empty() || m.media.is_some()))
}

/// Steps the reply target message backward (`delta < 0` -> older) or forward (`delta > 0` -> newer)
/// through non-service messages (`HistoryWidget::replyToPreviousMessage` / `replyToNextMessage`).
///
/// Returns:
/// - `Some(Some(msg))` -> switch reply target to `msg`
/// - `Some(None)` -> stepped past newest message (cancel reply mode back to `Normal`)
/// - `None` -> no change (already at oldest message or empty chat)
#[must_use]
pub fn step_reply_target(
    messages: &[HistoryMessage],
    current_reply: Option<MsgId>,
    delta: i32,
) -> Option<Option<&HistoryMessage>> {
    let eligible: Vec<&HistoryMessage> = messages.iter().filter(|m| !m.is_service()).collect();
    if eligible.is_empty() || delta == 0 {
        return None;
    }
    match current_reply {
        None => {
            if delta < 0 {
                // Ctrl+Up from Normal -> reply to newest message.
                Some(eligible.last().copied())
            } else {
                None
            }
        }
        Some(cur_id) => {
            let pos = eligible.iter().position(|m| m.id == cur_id)?;
            if delta < 0 {
                if pos > 0 {
                    Some(Some(eligible[pos - 1]))
                } else {
                    None
                }
            } else if pos + 1 < eligible.len() {
                Some(Some(eligible[pos + 1]))
            } else {
                // Stepping down past newest cancels reply mode.
                Some(None)
            }
        }
    }
}

/// Compact inline formatting & custom-emoji strip rendered above the compose input when
/// formatting is active or toggled (shows active format badges + quick buttons).
pub fn paint_active_format_strip(
    ui: &mut Ui,
    rect: Rect,
    rich: &RichEditState,
    colors: &HistoryColors,
) -> Option<FormatStripAction> {
    let mut action = None;
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, colors.bar_bg);

    let buttons: &[(FormatAction, &str)] = &[
        (FormatAction::Bold, "B"),
        (FormatAction::Italic, "I"),
        (FormatAction::Underline, "U"),
        (FormatAction::StrikeOut, "S"),
        (FormatAction::Monospace, "</>"),
        (FormatAction::Spoiler, "•••"),
        (FormatAction::CreateLink, "Link"),
        (FormatAction::ClearFormatting, "Clear"),
    ];

    let mut x = rect.left() + 10.0;
    let cy = rect.center().y;
    for &(act, label) in buttons {
        let w = if label.len() > 2 { 30.0 } else { 24.0 };
        let btn_rect = Rect::from_center_size(Pos2::new(x + w * 0.5, cy), Vec2::new(w, 20.0));
        let is_on = is_format_active_in_state(rich, act);
        let resp = ui.interact(
            btn_rect,
            Id::unique(("tg_fmt_strip", act.shortcut_hint())),
            Sense::click(),
        );
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if is_on || resp.hovered() {
            ui.painter().rect_filled(
                btn_rect,
                CornerRadius::same(5),
                if is_on {
                    colors.accent
                } else {
                    colors.bar_bg_over
                },
            );
        }
        let fg = if is_on {
            Color32::WHITE
        } else {
            colors.text_fg
        };
        match act {
            FormatAction::CreateLink => {
                tg_app_core::ui_icons::paint_style_tinted(
                    ui.painter(),
                    Rect::from_center_size(btn_rect.center(), Vec2::splat(14.0)),
                    tg_ui::tg_style::st::menu_icon_link,
                    fg,
                );
            }
            FormatAction::ClearFormatting => {
                tg_app_core::ui_icons::paint_close_cross(ui.painter(), btn_rect.center(), 12.0, fg);
            }
            _ => {
                ui.painter().text(
                    btn_rect.center(),
                    Align2::CENTER_CENTER,
                    label,
                    FontId::proportional(11.0),
                    fg,
                );
            }
        }
        if resp
            .on_hover_text(format!("{} ({})", act.label(), act.shortcut_hint()))
            .clicked()
        {
            action = Some(FormatStripAction::Format(act));
        }
        x += w + 4.0;
    }

    // Right side: quick custom-emoji buttons (`✨`, `🚀`, `🔥`).
    x += 6.0;
    for preset in CUSTOM_EMOJI_PRESETS.iter().take(3) {
        if x + 24.0 > rect.right() - 8.0 {
            break;
        }
        let btn_rect = Rect::from_center_size(Pos2::new(x + 11.0, cy), Vec2::new(22.0, 20.0));
        let resp = ui.interact(
            btn_rect,
            Id::unique(("tg_fmt_cemoji", preset.document_id)),
            Sense::click(),
        );
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .rect_filled(btn_rect, CornerRadius::same(5), colors.bar_bg_over);
        }
        ui.painter().text(
            btn_rect.center(),
            Align2::CENTER_CENTER,
            preset.emoji,
            FontId::proportional(12.0),
            colors.text_fg,
        );
        if resp
            .on_hover_text(format!("Insert custom emoji: {}", preset.name))
            .clicked()
        {
            action = Some(FormatStripAction::InsertCustomEmoji(*preset));
        }
        x += 24.0;
    }

    ui.painter().line_segment(
        [
            Pos2::new(rect.left(), rect.bottom()),
            Pos2::new(rect.right(), rect.bottom()),
        ],
        Stroke::new(1.0, colors.divider),
    );
    action
}

/// Action returned by [`paint_active_format_strip`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatStripAction {
    /// Formatting button clicked.
    Format(FormatAction),
    /// Custom emoji preset clicked.
    InsertCustomEmoji(CustomEmojiPreset),
}

fn is_format_active_in_state(rich: &RichEditState, act: FormatAction) -> bool {
    let (a, b) = rich.normalized_selection();
    if a == b {
        return match act {
            FormatAction::Bold => rich.active_formats.bold,
            FormatAction::Italic => rich.active_formats.italic,
            FormatAction::Underline => rich.active_formats.underline,
            FormatAction::StrikeOut => rich.active_formats.strike_out,
            FormatAction::Monospace => rich.active_formats.monospace,
            FormatAction::Spoiler => rich.active_formats.spoiler,
            FormatAction::Blockquote => rich.active_formats.blockquote,
            FormatAction::CreateLink | FormatAction::ClearFormatting => false,
        };
    }
    let bs = rich
        .text
        .char_indices()
        .nth(a)
        .map_or(rich.text.len(), |(p, _)| p);
    let be = rich
        .text
        .char_indices()
        .nth(b)
        .map_or(rich.text.len(), |(p, _)| p);
    let u16_s = utf8_to_utf16_offset(&rich.text, bs).unwrap_or(0) as i32;
    let u16_e = utf8_to_utf16_offset(&rich.text, be).unwrap_or(u16_s as usize) as i32;
    rich.entities.iter().any(|e| {
        let s = e.offset();
        let en = s + e.length();
        s <= u16_s
            && en >= u16_e
            && match act {
                FormatAction::Bold => {
                    matches!(e.entity_type(), EntityType::Bold | EntityType::Semibold)
                }
                FormatAction::Italic => e.entity_type() == EntityType::Italic,
                FormatAction::Underline => e.entity_type() == EntityType::Underline,
                FormatAction::StrikeOut => e.entity_type() == EntityType::StrikeOut,
                FormatAction::Monospace => {
                    matches!(e.entity_type(), EntityType::Code | EntityType::Pre)
                }
                FormatAction::Spoiler => e.entity_type() == EntityType::Spoiler,
                FormatAction::Blockquote => e.entity_type() == EntityType::Blockquote,
                FormatAction::CreateLink => e.entity_type() == EntityType::CustomUrl,
                FormatAction::ClearFormatting => false,
            }
    })
}

/// Convenience helper for creating a sample [`TextWithEntities`] with explicit substring matches
/// (useful for rich draft tests and screenshots).
#[must_use]
pub fn build_rich_text_with_spans(
    text: &str,
    spans: &[(&str, EntityType, &str)],
) -> TextWithEntities {
    let mut entities = Vec::new();
    for &(needle, ty, data) in spans {
        if let Some(byte_idx) = text.find(needle)
            && let Some(u16_off) = utf8_to_utf16_offset(text, byte_idx)
        {
            let u16_len = needle.encode_utf16().count() as i32;
            entities.push(EntityInText::with_data(ty, u16_off as i32, u16_len, data));
        }
    }
    TextWithEntities::with_entities(text, entities)
}

/// Business quick-reply slash autocomplete state for private chats
/// (`business::model::match_slash_quick_replies`).
///
/// Activated when the user types `/` in a private chat on a Telegram Business account.
#[derive(Debug, Clone, Default)]
pub struct QuickReplyAutocompleteState {
    /// Currently matching quick-reply shortcuts in display order.
    pub matches: Vec<crate::business::model::QuickReplyShortcut>,
    /// Keyboard-highlighted index in [`Self::matches`].
    pub selected_index: usize,
    /// Whether the popup is currently active.
    pub visible: bool,
    /// Last compose text inspected (used to clear dismiss override when text changes).
    dismissed_for_text: Option<String>,
}

impl QuickReplyAutocompleteState {
    /// Recomputes matching business quick-reply shortcuts for `compose_text`.
    ///
    /// Only activates in private chats (`is_private_chat == true`) for Telegram Business accounts
    /// (`is_business_account == true`).
    pub fn update(
        &mut self,
        compose_text: &str,
        is_private_chat: bool,
        is_business_account: bool,
        shortcuts: &[crate::business::model::QuickReplyShortcut],
    ) {
        if !is_private_chat || !is_business_account || shortcuts.is_empty() {
            self.clear();
            return;
        }
        if self.dismissed_for_text.as_deref() == Some(compose_text) {
            self.visible = false;
            self.matches.clear();
            return;
        }
        self.dismissed_for_text = None;

        if let Some(matched) =
            crate::business::model::match_slash_quick_replies(shortcuts, compose_text)
            && !matched.is_empty()
        {
            self.matches = matched.into_iter().cloned().collect();
            self.selected_index = self
                .selected_index
                .min(self.matches.len().saturating_sub(1));
            self.visible = true;
        } else {
            self.matches.clear();
            self.selected_index = 0;
            self.visible = false;
        }
    }

    /// Returns `true` when at least one quick-reply shortcut matches and the popup is visible.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.visible && !self.matches.is_empty()
    }

    /// Hides and clears the autocomplete state.
    pub fn clear(&mut self) {
        self.matches.clear();
        self.selected_index = 0;
        self.visible = false;
        self.dismissed_for_text = None;
    }

    /// Dismisses the popup for the current `compose_text` until the user edits the text.
    pub fn dismiss(&mut self, compose_text: &str) {
        self.visible = false;
        self.matches.clear();
        self.selected_index = 0;
        self.dismissed_for_text = Some(compose_text.to_owned());
    }

    /// Processes keyboard navigation (`Up`/`Down`, `Tab`/`Enter` to send, `Escape` to dismiss).
    pub fn handle_keys(
        &mut self,
        ctx: &egui::Context,
        compose_text: &str,
    ) -> Option<crate::business::model::QuickReplyShortcut> {
        if !self.is_visible() {
            return None;
        }
        let mut chosen = None;
        let mut dismissed = false;
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, Key::ArrowDown) {
                self.selected_index = (self.selected_index + 1) % self.matches.len();
            } else if i.consume_key(egui::Modifiers::NONE, Key::ArrowUp) {
                self.selected_index = if self.selected_index == 0 {
                    self.matches.len() - 1
                } else {
                    self.selected_index - 1
                };
            } else if i.consume_key(egui::Modifiers::NONE, Key::Escape) {
                dismissed = true;
            } else if i.consume_key(egui::Modifiers::NONE, Key::Tab)
                || i.consume_key(egui::Modifiers::NONE, Key::Enter)
            {
                chosen = self.matches.get(self.selected_index).cloned();
            }
        });
        if dismissed {
            self.dismiss(compose_text);
            return None;
        }
        if chosen.is_some() {
            self.clear();
        }
        chosen
    }

    /// Paints the floating Quick Replies autocomplete popup anchored above `compose_rect`.
    ///
    /// Returns the chosen [`crate::business::model::QuickReplyShortcut`] when a row is clicked.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        compose_rect: Rect,
        colors: &HistoryColors,
    ) -> Option<crate::business::model::QuickReplyShortcut> {
        if !self.is_visible() {
            return None;
        }
        let mut clicked = None;
        let header_h = 24.0;
        let row_h = 44.0;
        let visible_rows = self.matches.len().min(6);
        let popup_h = header_h + visible_rows as f32 * row_h + 10.0;
        let popup_w = compose_rect.width().clamp(260.0, 420.0);
        let popup_pos = Pos2::new(
            compose_rect.left() + 8.0,
            (compose_rect.top() - popup_h - 6.0).max(8.0),
        );

        egui::Area::new(Id::unique("tg_quick_reply_autocomplete"))
            .order(egui::Order::Foreground)
            .fixed_pos(popup_pos)
            .show(ui.ctx(), |ui| {
                let frame = egui::Frame::NONE
                    .fill(colors.bar_bg)
                    .stroke(Stroke::new(1.0, colors.divider))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(egui::Margin::symmetric(6, 5));
                frame.show(ui, |ui| {
                    ui.set_min_width(popup_w - 12.0);
                    ui.set_max_width(popup_w - 12.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Quick Replies")
                                .size(11.0)
                                .strong()
                                .color(colors.accent),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new("Enter to send")
                                    .size(10.5)
                                    .color(colors.sub_text_fg),
                            );
                        });
                    });
                    for (idx, sc) in self.matches.iter().take(6).enumerate() {
                        let (row_rect, resp) = ui
                            .allocate_exact_size(Vec2::new(popup_w - 12.0, row_h), Sense::click());
                        if resp.hovered() {
                            self.selected_index = idx;
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        }
                        if idx == self.selected_index {
                            ui.painter().rect_filled(
                                row_rect,
                                CornerRadius::same(7),
                                colors.bar_bg_over,
                            );
                        }
                        let slash_label = format!("/{}", sc.name);
                        ui.painter().text(
                            Pos2::new(row_rect.left() + 10.0, row_rect.top() + 13.0),
                            Align2::LEFT_CENTER,
                            &slash_label,
                            FontId::proportional(13.0),
                            colors.accent,
                        );
                        if sc.count() > 1 {
                            let badge = format!("{} msgs", sc.count());
                            ui.painter().text(
                                Pos2::new(row_rect.right() - 10.0, row_rect.top() + 13.0),
                                Align2::RIGHT_CENTER,
                                badge,
                                FontId::proportional(10.5),
                                colors.sub_text_fg,
                            );
                        }
                        let preview = sc.top_message_preview();
                        let clipped: String = if preview.chars().count() > 48 {
                            format!("{}…", preview.chars().take(47).collect::<String>())
                        } else {
                            preview
                        };
                        ui.painter().text(
                            Pos2::new(row_rect.left() + 10.0, row_rect.top() + 31.0),
                            Align2::LEFT_CENTER,
                            clipped,
                            FontId::proportional(11.5),
                            colors.sub_text_fg,
                        );
                        if resp.clicked() {
                            clicked = Some(sc.clone());
                        }
                    }
                });
            });

        if clicked.is_some() {
            self.clear();
        }
        clicked
    }
}
