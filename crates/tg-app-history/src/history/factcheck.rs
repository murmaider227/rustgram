//! Fact checks: batched `messages.getFactCheck` queue, `messages.editFactCheck` /
//! `messages.deleteFactCheck`, bubble footer with expandable note, and `EditFactCheckBox`.
//!
//! Port origin:
//! - `Telegram/SourceFiles/data/components/factchecks.cpp`
//! - `Telegram/SourceFiles/ui/boxes/edit_factcheck_box.cpp`

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, Sense, TextEdit, Ui, Vec2,
};
use tg_text::TextWithEntities;

use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::types::MsgId;

pub use tg_app_core::core_types::dto::factcheck::*;

/// User interaction emitted by [`paint_factcheck_footer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactCheckFooterAction {
    /// Toggle expanded/collapsed state of the fact-check note.
    ToggleExpand(MsgId),
    /// Open `EditFactCheckBox` to edit or remove this fact-check note.
    Edit(MsgId),
}

/// Measures the height needed for the bubble's fact-check footer card.
#[must_use]
pub fn measure_factcheck_footer_height(
    factcheck: &FactCheckData,
    expanded: bool,
    avail_width: f32,
) -> f32 {
    if factcheck.is_empty() {
        return 0.0;
    }
    let body = factcheck
        .text
        .as_ref()
        .map(|t| t.text.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("Loading fact-check note...");
    let shown_chars = if expanded {
        body.chars().count()
    } else {
        body.chars().count().min(FACTCHECK_COLLAPSED_PREVIEW_CHARS)
    };
    let chars_per_line = ((avail_width - 26.0) / 6.8).max(16.0) as usize;
    let lines = shown_chars.div_ceil(chars_per_line).clamp(1, 16) as f32;
    let toggle_h = if factcheck.is_expandable() { 18.0 } else { 0.0 };
    28.0 + lines * 16.0 + toggle_h + 10.0
}

/// Paints the fact-check footer card inside a message bubble with an expandable note.
#[allow(clippy::too_many_arguments)]
pub fn paint_factcheck_footer(
    ui: &mut Ui,
    msg_id: MsgId,
    factcheck: &FactCheckData,
    expanded: bool,
    can_edit: bool,
    rect: Rect,
    outgoing: bool,
    colors: &HistoryColors,
) -> Option<FactCheckFooterAction> {
    if factcheck.is_empty() {
        return None;
    }
    let mut action = None;
    let accent = if outgoing {
        colors.msg_out_reply_bar
    } else {
        Color32::from_rgb(0xE5, 0x8A, 0x2B)
    };
    let bg = if outgoing {
        colors.msg_out_reply_bg
    } else {
        colors.msg_in_reply_bg
    };

    ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
    let stripe = Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height()));
    ui.painter()
        .rect_filled(stripe, CornerRadius::same(2), accent);

    let left = rect.left() + 10.0;
    let inner_w = (rect.width() - 18.0).max(60.0);
    let mut y = rect.top() + 5.0;

    // Header: "Fact-Check (XX)" + optional "Edit" button
    ui.painter().text(
        Pos2::new(left, y),
        Align2::LEFT_TOP,
        factcheck.header_label(),
        FontId::proportional(11.5),
        accent,
    );
    if can_edit {
        let edit_rect = Rect::from_min_size(
            Pos2::new(rect.right() - 40.0, y - 1.0),
            Vec2::new(34.0, 16.0),
        );
        let edit_resp = ui.interact(
            edit_rect,
            Id::unique(("tg_factcheck_edit", msg_id.0)),
            Sense::click(),
        );
        edit_resp
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Edit Fact-Check"));
        if edit_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        ui.painter().text(
            edit_rect.center(),
            Align2::CENTER_CENTER,
            "Edit",
            FontId::proportional(11.0),
            accent,
        );
        if edit_resp.clicked() {
            action = Some(FactCheckFooterAction::Edit(msg_id));
        }
    }
    y += 17.0;

    // Body note (collapsed snippet or full expanded note)
    let raw_body = factcheck
        .text
        .as_ref()
        .map(|t| t.text.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("Loading fact-check note...");
    let display_body = if !expanded && factcheck.is_expandable() {
        let mut s: String = raw_body
            .chars()
            .take(FACTCHECK_COLLAPSED_PREVIEW_CHARS)
            .collect();
        s.push('…');
        s
    } else {
        raw_body.to_owned()
    };
    let galley = ui.painter().layout(
        display_body,
        FontId::proportional(12.0),
        colors.text_fg,
        inner_w,
    );
    let text_h = galley.size().y;
    ui.painter()
        .galley(Pos2::new(left, y), galley, colors.text_fg);
    y += text_h + 3.0;

    // Expandable toggle button ("Show more" / "Show less")
    if factcheck.is_expandable() {
        let toggle_label = if expanded { "Show less" } else { "Show more" };
        let toggle_rect = Rect::from_min_size(Pos2::new(left, y), Vec2::new(72.0, 16.0));
        let toggle_resp = ui.interact(
            toggle_rect,
            Id::unique(("tg_factcheck_expand", msg_id.0)),
            Sense::click(),
        );
        toggle_resp
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, toggle_label));
        if toggle_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        ui.painter().text(
            Pos2::new(left, y),
            Align2::LEFT_TOP,
            toggle_label,
            FontId::proportional(11.0),
            accent,
        );
        if toggle_resp.clicked() {
            action = Some(FactCheckFooterAction::ToggleExpand(msg_id));
        }
    }

    action
}

// ---------------------------------------------------------------------------
// EditFactCheckBox (`ui/boxes/edit_factcheck_box.cpp`)
// ---------------------------------------------------------------------------

/// Edit state machine matching `EditFactcheckBox` (`edit_factcheck_box.cpp:35-49`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditFactCheckPhase {
    /// No modifications yet.
    Initial,
    /// Non-empty edited text ready to save via `messages.editFactCheck`.
    Changed,
    /// Previously non-empty text cleared — ready to remove via `messages.deleteFactCheck`.
    Removed,
}

/// Outcome emitted by [`EditFactCheckBoxState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditFactCheckBoxAction {
    /// Save updated fact-check text (`messages.editFactCheck`).
    Save {
        /// Target message id.
        msg_id: MsgId,
        /// Updated fact-check note.
        text: TextWithEntities,
    },
    /// Remove existing fact-check (`messages.deleteFactCheck`).
    Remove {
        /// Target message id.
        msg_id: MsgId,
    },
    /// User cancelled.
    Cancelled,
}

/// Stateful modal for editing or removing a post's fact-check note (`EditFactcheckBox`).
#[derive(Debug, Clone)]
pub struct EditFactCheckBoxState {
    /// Target message id.
    pub msg_id: MsgId,
    /// Original fact-check text before editing.
    pub original_text: String,
    /// Live input buffer.
    pub draft_text: String,
    /// Maximum allowed length (`factcheck_length_limit`, default 1024).
    pub limit: usize,
    /// Optional inline validation error.
    pub error: Option<String>,
}

impl EditFactCheckBoxState {
    /// Opens an `EditFactCheckBox` for `msg_id` with existing optional text and length limit.
    #[must_use]
    pub fn new(msg_id: MsgId, current: Option<&FactCheckData>, limit: usize) -> Self {
        let original = current
            .and_then(|fc| fc.text.as_ref())
            .map(|t| t.text.clone())
            .unwrap_or_default();
        Self {
            msg_id,
            draft_text: original.clone(),
            original_text: original,
            limit: limit.max(1),
            error: None,
        }
    }

    /// Computes the current [`EditFactCheckPhase`] (`Initial` / `Changed` / `Removed`).
    #[must_use]
    pub fn phase(&self) -> EditFactCheckPhase {
        let now = self.draft_text.trim();
        let orig = self.original_text.trim();
        if now == orig {
            EditFactCheckPhase::Initial
        } else if now.is_empty() {
            if orig.is_empty() {
                EditFactCheckPhase::Initial
            } else {
                EditFactCheckPhase::Removed
            }
        } else {
            EditFactCheckPhase::Changed
        }
    }

    /// Renders `EditFactCheckBox`.
    pub fn show(&mut self, ui: &mut Ui, colors: &HistoryColors) -> Option<EditFactCheckBoxAction> {
        let mut action = None;
        egui::Window::new("Fact-Check")
            .id(Id::unique("tg_edit_factcheck_box"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size(Vec2::new(380.0, 210.0))
            .show(ui.ctx(), |ui| {
                ui.add(
                    TextEdit::multiline(&mut self.draft_text)
                        .id(Id::unique("tg_factcheck_input"))
                        .hint_text("Enter fact-check note...")
                        .desired_rows(4)
                        .desired_width(350.0),
                );
                // Strip newlines matching `Ui::InputField::Mode::NoNewlines`
                if self.draft_text.contains('\n') || self.draft_text.contains('\r') {
                    self.draft_text = self.draft_text.replace(['\r', '\n'], " ");
                }

                let len = self.draft_text.trim().chars().count();
                let rem = self.limit as isize - len as isize;
                let count_col = if rem < 0 {
                    colors.attention_fg
                } else {
                    colors.sub_text_fg
                };
                ui.label(
                    egui::RichText::new(format!("{len} / {} characters", self.limit))
                        .size(11.5)
                        .color(count_col),
                );

                if let Some(ref err) = self.error {
                    ui.label(egui::RichText::new(err).color(colors.attention_fg));
                }

                ui.add_space(8.0);
                let phase = self.phase();
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        action = Some(EditFactCheckBoxAction::Cancelled);
                    }
                    match phase {
                        EditFactCheckPhase::Removed => {
                            if ui
                                .button(egui::RichText::new("Remove").color(colors.attention_fg))
                                .clicked()
                            {
                                action = Some(EditFactCheckBoxAction::Remove {
                                    msg_id: self.msg_id,
                                });
                            }
                        }
                        EditFactCheckPhase::Initial => {
                            if ui.button("Save").clicked() {
                                if self.original_text.trim().is_empty() {
                                    self.error = Some("Please enter a fact-check note.".to_owned());
                                } else {
                                    action = Some(EditFactCheckBoxAction::Cancelled);
                                }
                            }
                        }
                        EditFactCheckPhase::Changed => {
                            if ui.button("Save").clicked() {
                                let trimmed = self.draft_text.trim();
                                if trimmed.chars().count() > self.limit {
                                    self.error = Some(format!(
                                        "Note exceeds {} character limit.",
                                        self.limit
                                    ));
                                } else {
                                    self.error = None;
                                    action = Some(EditFactCheckBoxAction::Save {
                                        msg_id: self.msg_id,
                                        text: TextWithEntities::simple(trimmed.to_owned()),
                                    });
                                }
                            }
                        }
                    }
                });
            });

        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_app_core::stand_in::types::{FullMsgId, PeerId};

    #[test]
    fn factcheck_batcher_coalesces_same_peer_and_flushes_on_peer_switch() {
        let mut batcher = FactCheckBatcher::new();
        let ch1 = PeerId::from_channel(3001);
        let ch2 = PeerId::from_channel(3002);

        assert!(!batcher.enqueue(
            FullMsgId {
                peer: ch1,
                msg: MsgId(10)
            },
            100
        ));
        assert!(!batcher.enqueue(
            FullMsgId {
                peer: ch1,
                msg: MsgId(11)
            },
            200
        ));
        // Duplicate enqueue ignored
        assert!(!batcher.enqueue(
            FullMsgId {
                peer: ch1,
                msg: MsgId(10)
            },
            300
        ));
        // Not ready before 1000ms
        assert!(batcher.take_ready_batch(900, false).is_none());

        // Switching peer signals immediate flush for ch1
        assert!(batcher.enqueue(
            FullMsgId {
                peer: ch2,
                msg: MsgId(5)
            },
            400
        ));
        let batch1 = batcher.take_ready_batch(400, false);
        assert_eq!(batch1, Some((ch1, vec![MsgId(10), MsgId(11)])));

        let resolved = batcher.finish_batch(Ok(vec![FactCheckData {
            need_check: false,
            country: Some("US".to_owned()),
            text: Some(TextWithEntities::simple("Verified claim")),
            hash: 42,
        }]));
        assert_eq!(resolved.len(), 2);
        assert_eq!(
            resolved[&FullMsgId {
                peer: ch1,
                msg: MsgId(10)
            }]
                .hash,
            42
        );
        // Short server response clears missing item
        assert!(
            resolved[&FullMsgId {
                peer: ch1,
                msg: MsgId(11)
            }]
                .is_empty()
        );

        let batch2 = batcher.take_ready_batch(2_000, false);
        assert_eq!(batch2, Some((ch2, vec![MsgId(5)])));
    }

    #[test]
    fn edit_factcheck_box_transitions_between_initial_changed_and_removed() {
        let existing = FactCheckData {
            need_check: false,
            country: Some("DE".to_owned()),
            text: Some(TextWithEntities::simple("Original fact check")),
            hash: 7,
        };
        let mut box_state =
            EditFactCheckBoxState::new(MsgId(1), Some(&existing), FACTCHECK_DEFAULT_LENGTH_LIMIT);
        assert_eq!(box_state.phase(), EditFactCheckPhase::Initial);

        box_state.draft_text = "Updated fact check".to_owned();
        assert_eq!(box_state.phase(), EditFactCheckPhase::Changed);

        box_state.draft_text = "   ".to_owned();
        assert_eq!(box_state.phase(), EditFactCheckPhase::Removed);
    }
}
