//! Checklists / to-do lists: `MessageMedia::ToDo` bubble renderer, `EditTodoListBox` &
//! `AddTodoListTasksBox` modals, validation, pasted-list splitting, and 3-second debounced
//! toggle batching (`messages.toggleTodoCompleted`, `messages.appendTodoList`).
//!
//! Port origin:
//! - `Telegram/SourceFiles/history/view/media/history_view_todo_list.cpp`
//! - `Telegram/SourceFiles/boxes/edit_todo_list_box.cpp`
//! - `Telegram/SourceFiles/api/api_todo_lists.cpp`
//! - `Telegram/SourceFiles/data/data_todo_list.cpp`

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, ScrollArea, Sense, Shape,
    Stroke, TextEdit, Ui, Vec2,
};
use tg_text::TextWithEntities;

use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::types::MsgId;

pub use tg_app_core::core_types::dto::todo::*;

/// Interaction emitted from clicking inside a checklist bubble.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoBubbleAction {
    /// Toggle completion state of task `item_id` in message `msg_id`.
    ToggleItem {
        /// Message id.
        msg_id: MsgId,
        /// Checklist task id.
        item_id: i32,
    },
    /// Open `AddTodoListTasksBox` to append tasks to `msg_id`.
    OpenAddTasks(MsgId),
    /// Open `EditTodoListBox` to edit checklist `msg_id`.
    OpenEdit(MsgId),
}

/// Estimates `(width, height)` in logical pixels for a [`TodoListAttachment`] bubble block.
#[must_use]
pub fn measure_todo_list_size(todo: &TodoListAttachment, max_width: i32) -> (i32, i32) {
    let min_w = 240;
    let clamped_max = max_width.max(min_w);
    let title_w = (todo.title.text.chars().count() as i32) * 8 + 36;
    let max_task_w = todo
        .items
        .iter()
        .map(|it| (it.text.text.chars().count() as i32) * 7 + 56)
        .max()
        .unwrap_or(min_w);
    let width = title_w.max(max_task_w).clamp(min_w, clamped_max);

    let header_h = 46;
    let per_task_h = 34;
    let tasks_h = (todo.items.len() as i32) * per_task_h;
    let footer_h = 36;
    (width, header_h + tasks_h + footer_h)
}

/// Paints an interactive `MessageMedia::ToDo` checklist bubble inside `rect`.
#[allow(clippy::too_many_arguments)]
pub fn paint_todo_list_bubble(
    ui: &mut Ui,
    msg_id: MsgId,
    todo: &TodoListAttachment,
    rect: Rect,
    outgoing: bool,
    is_self_chat: bool,
    is_forwarded: bool,
    peer_is_user: bool,
    colors: &HistoryColors,
) -> Option<TodoBubbleAction> {
    let mut action = None;
    let pad_x = 12.0;
    let inner_w = (rect.width() - pad_x * 2.0).max(80.0);
    let mut y = rect.top() + 8.0;

    let accent = if outgoing {
        colors.msg_out_reply_bar
    } else {
        colors.accent
    };
    let sub_fg = if outgoing {
        colors.msg_out_date_fg
    } else {
        colors.sub_text_fg
    };

    // Title
    let title_text = if todo.title.text.is_empty() {
        "Checklist"
    } else {
        todo.title.text.as_str()
    };
    ui.painter().text(
        Pos2::new(rect.left() + pad_x, y),
        Align2::LEFT_TOP,
        title_text,
        FontId::proportional(14.5),
        colors.bold_fg,
    );
    y += 20.0;

    // Subtitle
    ui.painter().text(
        Pos2::new(rect.left() + pad_x, y),
        Align2::LEFT_TOP,
        todo.subtitle(peer_is_user),
        FontId::proportional(11.5),
        sub_fg,
    );
    y += 20.0;

    let can_complete = todo.can_complete(outgoing, is_self_chat, is_forwarded);
    let can_append = todo.can_append(outgoing, is_self_chat, is_forwarded);

    // Task rows
    for (idx, task) in todo.items.iter().enumerate() {
        let row_h = 32.0;
        let row_rect = Rect::from_min_size(
            Pos2::new(rect.left() + 4.0, y),
            Vec2::new((rect.width() - 8.0).max(40.0), row_h),
        );
        let resp = ui.interact(
            row_rect,
            Id::unique(("tg_todo_task", msg_id.0, task.id, idx)),
            Sense::click(),
        );
        resp.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::Role::CheckBox,
                can_complete,
                task.is_completed(),
                task.text.text.clone(),
            )
        });
        if resp.hovered() && can_complete {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .rect_filled(row_rect, CornerRadius::same(6), colors.bar_bg_over);
        }

        // Circular checkbox (`history_view_todo_list.cpp` paint)
        let center = Pos2::new(rect.left() + pad_x + 10.0, row_rect.center().y);
        let radius = 9.0;
        let completed = task.is_completed();
        if completed {
            ui.painter().circle_filled(center, radius, accent);
            let check_pts = vec![
                Pos2::new(center.x - 4.0, center.y + 0.3),
                Pos2::new(center.x - 1.2, center.y + 3.2),
                Pos2::new(center.x + 4.5, center.y - 3.2),
            ];
            ui.painter()
                .add(Shape::line(check_pts, Stroke::new(1.8, Color32::WHITE)));
        } else {
            let ring_col = if can_complete { accent } else { sub_fg };
            ui.painter()
                .circle_stroke(center, radius, Stroke::new(1.6, ring_col));
        }

        // Task text (strikethrough + muted when completed)
        let text_left = rect.left() + pad_x + 28.0;
        let text_fg = if completed { sub_fg } else { colors.text_fg };
        let galley = ui.painter().layout(
            task.text.text.clone(),
            FontId::proportional(13.0),
            text_fg,
            (inner_w - 30.0).max(40.0),
        );
        let text_pos = Pos2::new(text_left, row_rect.center().y - galley.size().y * 0.5);
        let text_w = galley.size().x;
        let text_mid_y = row_rect.center().y;
        ui.painter().galley(text_pos, galley, text_fg);
        if completed {
            ui.painter().line_segment(
                [
                    Pos2::new(text_left, text_mid_y),
                    Pos2::new(text_left + text_w.min(inner_w - 30.0), text_mid_y),
                ],
                Stroke::new(1.1, sub_fg),
            );
        }

        if resp.clicked() {
            action = Some(TodoBubbleAction::ToggleItem {
                msg_id,
                item_id: task.id,
            });
        }
        y += row_h + 2.0;
    }

    // Bottom completion summary + action buttons ("Add task", "Edit")
    y += 4.0;
    ui.painter().line_segment(
        [
            Pos2::new(rect.left() + pad_x, y),
            Pos2::new(rect.right() - pad_x, y),
        ],
        Stroke::new(1.0, colors.divider),
    );
    y += 6.0;

    let summary_text = todo.completion_summary();
    ui.painter().text(
        Pos2::new(rect.left() + pad_x, y + 4.0),
        Align2::LEFT_TOP,
        summary_text,
        FontId::proportional(11.5),
        sub_fg,
    );

    let mut right_x = rect.right() - pad_x;
    if outgoing && !is_forwarded {
        let edit_w = 38.0;
        let edit_rect =
            Rect::from_min_size(Pos2::new(right_x - edit_w, y), Vec2::new(edit_w, 22.0));
        let edit_resp = ui.interact(
            edit_rect,
            Id::unique(("tg_todo_edit", msg_id.0)),
            Sense::click(),
        );
        edit_resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Edit"));
        if edit_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        ui.painter().text(
            edit_rect.center(),
            Align2::CENTER_CENTER,
            "Edit",
            FontId::proportional(11.5),
            accent,
        );
        if edit_resp.clicked() {
            action = Some(TodoBubbleAction::OpenEdit(msg_id));
        }
        right_x -= edit_w + 6.0;
    }
    if can_append {
        let add_w = 64.0;
        let add_rect = Rect::from_min_size(Pos2::new(right_x - add_w, y), Vec2::new(add_w, 22.0));
        let add_resp = ui.interact(
            add_rect,
            Id::unique(("tg_todo_add", msg_id.0)),
            Sense::click(),
        );
        add_resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Add task"));
        if add_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        ui.painter().text(
            add_rect.center(),
            Align2::CENTER_CENTER,
            "+ Add task",
            FontId::proportional(11.5),
            accent,
        );
        if add_resp.clicked() {
            action = Some(TodoBubbleAction::OpenAddTasks(msg_id));
        }
    }

    action
}

// ---------------------------------------------------------------------------
// EditTodoListBox (`boxes/edit_todo_list_box.cpp`)
// ---------------------------------------------------------------------------

/// Outcome emitted by [`EditTodoListBoxState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditTodoListBoxAction {
    /// Submit a newly created checklist.
    Create(CreateTodoListRequest),
    /// Save edits to an existing checklist message.
    SaveEdit {
        /// Existing checklist message id.
        msg_id: MsgId,
        /// Updated checklist payload.
        request: CreateTodoListRequest,
    },
    /// User cancelled the modal.
    Cancelled,
}

/// Stateful modal for creating or editing a checklist (`EditTodoListBox`).
#[derive(Debug, Clone)]
pub struct EditTodoListBoxState {
    /// `Some(msg_id)` when editing an existing checklist; `None` when creating a new one.
    pub editing_msg_id: Option<MsgId>,
    /// Checklist title text.
    pub title: String,
    /// Editable `(id, text)` task pairs (`id == 0` for newly added rows).
    pub tasks: Vec<(i32, String)>,
    /// Allow other participants to append tasks (`others_can_append`).
    pub others_can_append: bool,
    /// Allow other participants to mark tasks completed (`others_can_complete`).
    pub others_can_complete: bool,
    /// Configured title character limit.
    pub title_limit: usize,
    /// Configured per-task character limit.
    pub item_limit: usize,
    /// Configured max task count.
    pub max_items: usize,
    /// Optional inline validation error message.
    pub error: Option<String>,
}

impl Default for EditTodoListBoxState {
    fn default() -> Self {
        Self::new_create()
    }
}

impl EditTodoListBoxState {
    /// Creates a blank `EditTodoListBox` in Create mode (both collaborative flags default `true`,
    /// matching `edit_todo_list_box.cpp:1078-1085`).
    #[must_use]
    pub fn new_create() -> Self {
        Self {
            editing_msg_id: None,
            title: String::new(),
            tasks: vec![(0, String::new()), (0, String::new())],
            others_can_append: true,
            others_can_complete: true,
            title_limit: TODO_TITLE_LIMIT,
            item_limit: TODO_ITEM_TEXT_LIMIT,
            max_items: TODO_MAX_ITEMS,
            error: None,
        }
    }

    /// Creates an `EditTodoListBox` pre-populated from an existing [`TodoListAttachment`].
    #[must_use]
    pub fn new_edit(msg_id: MsgId, existing: &TodoListAttachment) -> Self {
        let mut tasks: Vec<(i32, String)> = existing
            .items
            .iter()
            .map(|t| (t.id, t.text.text.clone()))
            .collect();
        if tasks.len() < TODO_MAX_ITEMS {
            tasks.push((0, String::new()));
        }
        Self {
            editing_msg_id: Some(msg_id),
            title: existing.title.text.clone(),
            tasks,
            others_can_append: existing.others_can_append,
            others_can_complete: existing.others_can_complete,
            title_limit: TODO_TITLE_LIMIT,
            item_limit: TODO_ITEM_TEXT_LIMIT,
            max_items: TODO_MAX_ITEMS,
            error: None,
        }
    }

    /// Splits any task row that contains pasted newlines into separate task entries.
    pub fn expand_pasted_newlines(&mut self) {
        let mut idx = 0;
        while idx < self.tasks.len() {
            let lines = parse_pasted_todo_lines(&self.tasks[idx].1);
            if !lines.is_empty() {
                let first_id = self.tasks[idx].0;
                self.tasks[idx] = (first_id, lines[0].clone());
                for (offset, extra) in lines.into_iter().skip(1).enumerate() {
                    if self.tasks.len() >= self.max_items {
                        break;
                    }
                    self.tasks.insert(idx + 1 + offset, (0, extra));
                }
            }
            idx += 1;
        }
    }

    /// Ensures there is a trailing empty slot when under `max_items` (`Tasks::checkLastTask`).
    pub fn ensure_trailing_slot(&mut self) {
        let has_trailing_empty = self
            .tasks
            .last()
            .is_some_and(|(_, text)| text.trim().is_empty());
        if !has_trailing_empty && self.tasks.len() < self.max_items {
            self.tasks.push((0, String::new()));
        }
    }

    /// Builds and validates a [`CreateTodoListRequest`] from the current form inputs.
    pub fn build_request(&self) -> Result<CreateTodoListRequest, TodoValidationError> {
        let mut next_id = self
            .tasks
            .iter()
            .map(|(id, _)| *id)
            .max()
            .unwrap_or(0)
            .max(0);
        let mut items = Vec::new();
        for (id, raw) in &self.tasks {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            let assigned_id = if *id > 0 {
                *id
            } else {
                next_id += 1;
                next_id
            };
            items.push(TodoItemInput {
                id: assigned_id,
                text: TextWithEntities::simple(trimmed.to_owned()),
            });
        }
        let req = CreateTodoListRequest {
            title: TextWithEntities::simple(self.title.trim().to_owned()),
            items,
            others_can_append: self.others_can_append,
            others_can_complete: self.others_can_complete,
        };
        validate_todo_list(&req, self.title_limit, self.item_limit, self.max_items)?;
        Ok(req)
    }

    /// Renders the `EditTodoListBox` modal dialog.
    pub fn show(&mut self, ui: &mut Ui, colors: &HistoryColors) -> Option<EditTodoListBoxAction> {
        let mut action = None;
        let heading = if self.editing_msg_id.is_some() {
            "Edit Checklist"
        } else {
            "New Checklist"
        };

        egui::Window::new(heading)
            .id(Id::unique("tg_edit_todo_list_box"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size(Vec2::new(390.0, 440.0))
            .show(ui.ctx(), |ui| {
                ui.label(
                    egui::RichText::new("Title")
                        .strong()
                        .color(colors.active_text_fg),
                );
                let title_len = self.title.chars().count();
                let title_rem = self.title_limit as isize - title_len as isize;
                ui.horizontal(|ui| {
                    ui.add(
                        TextEdit::singleline(&mut self.title)
                            .id(Id::unique("tg_todo_title_input"))
                            .hint_text("Checklist title")
                            .desired_width(310.0),
                    );
                    if title_rem < TODO_WARN_TITLE_LIMIT as isize {
                        let col = if title_rem < 0 {
                            colors.attention_fg
                        } else {
                            colors.sub_text_fg
                        };
                        ui.label(egui::RichText::new(title_rem.to_string()).color(col));
                    }
                });

                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Checklist items")
                        .strong()
                        .color(colors.active_text_fg),
                );

                let mut remove_idx: Option<usize> = None;
                ScrollArea::vertical()
                    .id_salt("tg_todo_items_scroll")
                    .max_height(190.0)
                    .show(ui, |ui| {
                        for (idx, (_, text)) in self.tasks.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                ui.add(
                                    TextEdit::singleline(text)
                                        .id(Id::unique(("tg_todo_item_input", idx)))
                                        .hint_text(format!("Task {}", idx + 1))
                                        .desired_width(285.0),
                                );
                                let rem = self.item_limit as isize - text.chars().count() as isize;
                                if rem < TODO_WARN_TASK_LIMIT as isize {
                                    let col = if rem < 0 {
                                        colors.attention_fg
                                    } else {
                                        colors.sub_text_fg
                                    };
                                    ui.label(egui::RichText::new(rem.to_string()).color(col));
                                }
                                if tg_app_core::ui_icons::small_icon_button(
                                    ui,
                                    "Remove task",
                                    true,
                                    tg_app_core::ui_icons::paint_close_cross,
                                )
                                .clicked()
                                {
                                    remove_idx = Some(idx);
                                }
                            });
                        }
                    });
                if let Some(idx) = remove_idx {
                    if self.tasks.len() > 1 {
                        self.tasks.remove(idx);
                    } else {
                        self.tasks[0].1.clear();
                    }
                }
                self.expand_pasted_newlines();
                self.ensure_trailing_slot();

                let filled_count = self
                    .tasks
                    .iter()
                    .filter(|(_, t)| !t.trim().is_empty())
                    .count();
                let limit_label = if filled_count < self.max_items {
                    format!(
                        "You can add {} more tasks.",
                        self.max_items.saturating_sub(filled_count)
                    )
                } else {
                    "You have added the maximum number of tasks.".to_owned()
                };
                ui.label(
                    egui::RichText::new(limit_label)
                        .size(11.5)
                        .color(colors.sub_text_fg),
                );

                ui.add_space(6.0);
                ui.separator();
                ui.label(
                    egui::RichText::new("Settings")
                        .strong()
                        .color(colors.active_text_fg),
                );
                ui.checkbox(&mut self.others_can_append, "Allow others to add tasks");
                ui.checkbox(
                    &mut self.others_can_complete,
                    "Allow others to mark tasks as completed",
                );

                if let Some(ref err) = self.error {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(err).color(colors.attention_fg));
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        action = Some(EditTodoListBoxAction::Cancelled);
                    }
                    let submit_label = if self.editing_msg_id.is_some() {
                        "Save"
                    } else {
                        "Create"
                    };
                    if ui.button(submit_label).clicked() {
                        match self.build_request() {
                            Ok(request) => {
                                self.error = None;
                                action = Some(match self.editing_msg_id {
                                    Some(msg_id) => {
                                        EditTodoListBoxAction::SaveEdit { msg_id, request }
                                    }
                                    None => EditTodoListBoxAction::Create(request),
                                });
                            }
                            Err(err) => {
                                self.error = Some(err.message());
                            }
                        }
                    }
                });
            });

        action
    }
}

// ---------------------------------------------------------------------------
// AddTodoListTasksBox (`boxes/edit_todo_list_box.cpp:1216-1307`)
// ---------------------------------------------------------------------------

/// Outcome emitted by [`AddTodoListTasksBoxState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddTodoListTasksBoxAction {
    /// Append validated new tasks to `msg_id` (`messages.appendTodoList`).
    Append {
        /// Target checklist message id.
        msg_id: MsgId,
        /// Newly added tasks only.
        items: Vec<TodoItemInput>,
    },
    /// User cancelled.
    Cancelled,
}

/// Modal dialog for appending new tasks to an existing checklist (`AddTodoListTasksBox`).
#[derive(Debug, Clone)]
pub struct AddTodoListTasksBoxState {
    /// Target checklist message id.
    pub msg_id: MsgId,
    /// Locked existing task titles (read-only).
    pub existing_titles: Vec<String>,
    /// Next task id to assign when submitting.
    pub next_item_id: i32,
    /// Editable new task texts.
    pub new_tasks: Vec<String>,
    /// Configured task text limit.
    pub item_limit: usize,
    /// Configured max tasks total.
    pub max_items: usize,
    /// Optional validation error.
    pub error: Option<String>,
}

impl AddTodoListTasksBoxState {
    /// Creates an append-tasks modal for an existing [`TodoListAttachment`].
    #[must_use]
    pub fn for_checklist(msg_id: MsgId, todo: &TodoListAttachment) -> Self {
        Self {
            msg_id,
            existing_titles: todo.items.iter().map(|i| i.text.text.clone()).collect(),
            next_item_id: todo.next_item_id(),
            new_tasks: vec![String::new()],
            item_limit: TODO_ITEM_TEXT_LIMIT,
            max_items: TODO_MAX_ITEMS,
            error: None,
        }
    }

    /// Collects validated new tasks to send via `messages.appendTodoList`.
    pub fn build_appended_items(&self) -> Result<Vec<TodoItemInput>, TodoValidationError> {
        let mut next = self.next_item_id.max(1);
        let mut out = Vec::new();
        for (idx, raw) in self.new_tasks.iter().enumerate() {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            let len = trimmed.chars().count();
            if len > self.item_limit {
                return Err(TodoValidationError::TaskTooLong {
                    index: idx,
                    len,
                    limit: self.item_limit,
                });
            }
            out.push(TodoItemInput {
                id: next,
                text: TextWithEntities::simple(trimmed.to_owned()),
            });
            next += 1;
        }
        if out.is_empty() {
            return Err(TodoValidationError::NoTasks);
        }
        let total = self.existing_titles.len() + out.len();
        if total > self.max_items {
            return Err(TodoValidationError::TooManyTasks {
                count: total,
                limit: self.max_items,
            });
        }
        Ok(out)
    }

    /// Renders the `AddTodoListTasksBox` modal dialog.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        colors: &HistoryColors,
    ) -> Option<AddTodoListTasksBoxAction> {
        let mut action = None;
        egui::Window::new("Add Tasks")
            .id(Id::unique("tg_add_todo_tasks_box"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size(Vec2::new(370.0, 340.0))
            .show(ui.ctx(), |ui| {
                ScrollArea::vertical()
                    .id_salt("tg_add_todo_scroll")
                    .max_height(220.0)
                    .show(ui, |ui| {
                        for existing in &self.existing_titles {
                            ui.label(
                                egui::RichText::new(format!("• {existing}"))
                                    .color(colors.sub_text_fg),
                            );
                        }
                        if !self.existing_titles.is_empty() {
                            ui.separator();
                        }
                        for (idx, text) in self.new_tasks.iter_mut().enumerate() {
                            ui.add(
                                TextEdit::singleline(text)
                                    .id(Id::unique(("tg_add_todo_new_input", idx)))
                                    .hint_text("Add a task")
                                    .desired_width(320.0),
                            );
                        }
                    });

                let remaining = self
                    .max_items
                    .saturating_sub(self.existing_titles.len() + self.new_tasks.len());
                if self.new_tasks.last().is_some_and(|t| !t.trim().is_empty()) && remaining > 0 {
                    self.new_tasks.push(String::new());
                }

                if let Some(ref err) = self.error {
                    ui.label(egui::RichText::new(err).color(colors.attention_fg));
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        action = Some(AddTodoListTasksBoxAction::Cancelled);
                    }
                    if ui.button("Save").clicked() {
                        match self.build_appended_items() {
                            Ok(items) => {
                                self.error = None;
                                action = Some(AddTodoListTasksBoxAction::Append {
                                    msg_id: self.msg_id,
                                    items,
                                });
                            }
                            Err(err) => {
                                self.error = Some(err.message());
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
    fn parse_pasted_lines_splits_multiline_and_ignores_single_line() {
        assert!(parse_pasted_todo_lines("Single item").is_empty());
        assert_eq!(
            parse_pasted_todo_lines("  Buy milk \n\n  Walk dog \n Ship build "),
            vec!["Buy milk", "Walk dog", "Ship build"]
        );
    }

    #[test]
    fn toggle_batcher_cancels_opposite_toggles_and_respects_delay() {
        let mut batcher = TodoToggleBatcher::new();
        let fid = FullMsgId {
            peer: PeerId::from_chat(10),
            msg: MsgId(5),
        };
        batcher.toggle(fid, 1, true, 1_000);
        batcher.toggle(fid, 2, true, 1_500);
        // Flipping task 2 back to incomplete removes it from `completed`
        batcher.toggle(fid, 2, false, 2_000);

        // Before 3s delay from last toggle (2_000 + 3_000 = 5_000): nothing ready
        assert!(batcher.take_ready(4_999, false).is_empty());

        let ready = batcher.take_ready(5_000, false);
        assert_eq!(ready, vec![(fid, vec![1], vec![2])]);

        // While in flight, new toggles queue without double-sending
        batcher.toggle(fid, 3, true, 5_100);
        assert!(batcher.take_ready(10_000, true).is_empty());

        batcher.mark_finished(fid);
        let ready2 = batcher.take_ready(10_000, false);
        assert_eq!(ready2, vec![(fid, vec![3], vec![])]);
    }

    #[test]
    fn validation_enforces_tdesktop_limits() {
        let empty = CreateTodoListRequest {
            title: TextWithEntities::simple("   "),
            items: vec![],
            others_can_append: true,
            others_can_complete: true,
        };
        assert_eq!(
            validate_todo_list(
                &empty,
                TODO_TITLE_LIMIT,
                TODO_ITEM_TEXT_LIMIT,
                TODO_MAX_ITEMS
            ),
            Err(TodoValidationError::EmptyTitle)
        );

        let no_tasks = CreateTodoListRequest {
            title: TextWithEntities::simple("Release Checklist"),
            items: vec![],
            others_can_append: false,
            others_can_complete: false,
        };
        assert_eq!(
            validate_todo_list(
                &no_tasks,
                TODO_TITLE_LIMIT,
                TODO_ITEM_TEXT_LIMIT,
                TODO_MAX_ITEMS
            ),
            Err(TodoValidationError::NoTasks)
        );
    }
}
