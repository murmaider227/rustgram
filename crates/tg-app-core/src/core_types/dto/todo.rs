//! Pure checklist (todo list) DTOs, validator, and toggle batcher.

use std::collections::{BTreeMap, BTreeSet};

use tg_text::TextWithEntities;

use crate::stand_in::types::{FullMsgId, MsgId, PeerId};

/// Maximum number of tasks allowed in a checklist (`TodoListData::kMaxOptions` / `todoListItemsLimit`).
pub const TODO_MAX_ITEMS: usize = 30;
/// Maximum character count for a checklist title (`todoListTitleLimit`).
pub const TODO_TITLE_LIMIT: usize = 32;
/// Maximum character count for a single checklist task (`todoListItemTextLimit`).
pub const TODO_ITEM_TEXT_LIMIT: usize = 64;
/// Remaining-characters threshold before showing the title warning counter (`kWarnTitleLimit`).
pub const TODO_WARN_TITLE_LIMIT: usize = 12;
/// Remaining-characters threshold before showing a task warning counter (`kWarnTaskLimit`).
pub const TODO_WARN_TASK_LIMIT: usize = 24;
/// Delay in milliseconds before flushing accumulated completion toggles (`kSendTogglesDelay`).
pub const TODO_SEND_TOGGLES_DELAY_MS: u64 = 3_000;

/// Single task inside a [`TodoListAttachment`] (`TodoListItem` in `data/data_todo_list.h`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoTaskItem {
    /// Stable per-list task identifier (`todoItem.id`, 1-based).
    pub id: i32,
    /// Task display text with optional formatting entities.
    pub text: TextWithEntities,
    /// Peer who marked the task completed, if completed.
    pub completed_by: Option<PeerId>,
    /// Display name of the completer for tooltip / subtitle rendering.
    pub completed_by_name: Option<String>,
    /// Unix timestamp when the task was completed (`0` if incomplete).
    pub completion_date: i32,
}

impl TodoTaskItem {
    /// Returns `true` when this task is currently marked as completed.
    #[inline]
    #[must_use]
    pub const fn is_completed(&self) -> bool {
        self.completion_date != 0 || self.completed_by.is_some()
    }
}

/// Collaborative checklist attachment (`MessageMedia::ToDo` / `TodoListData`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TodoListAttachment {
    /// Checklist title with optional entities.
    pub title: TextWithEntities,
    /// Ordered checklist tasks (up to [`TODO_MAX_ITEMS`]).
    pub items: Vec<TodoTaskItem>,
    /// Whether other chat participants may append new tasks (`others_can_append`).
    pub others_can_append: bool,
    /// Whether other chat participants may toggle task completion (`others_can_complete`).
    pub others_can_complete: bool,
}

impl TodoListAttachment {
    /// Returns total number of tasks in the list.
    #[inline]
    #[must_use]
    pub fn total_count(&self) -> usize {
        self.items.len()
    }

    /// Returns count of completed tasks.
    #[must_use]
    pub fn completed_count(&self) -> usize {
        self.items.iter().filter(|i| i.is_completed()).count()
    }

    /// Returns whether all tasks are completed (and list is non-empty).
    #[must_use]
    pub fn all_completed(&self) -> bool {
        !self.items.is_empty() && self.completed_count() == self.items.len()
    }

    /// Formats the subtitle label matching `TodoList::updateTexts` in `history_view_todo_list.cpp`.
    #[must_use]
    pub fn subtitle(&self, peer_is_user: bool) -> &'static str {
        if !self.others_can_complete {
            "Checklist"
        } else if peer_is_user {
            "Shared checklist"
        } else {
            "Group checklist"
        }
    }

    /// Formats the bottom completion status label (`TodoList::updateCompletionStatus`).
    #[must_use]
    pub fn completion_summary(&self) -> String {
        let total = self.total_count();
        let completed = self.completed_count();
        if completed == 0 {
            format!("0 of {total} completed")
        } else {
            format!("{completed} of {total} completed")
        }
    }

    /// Returns `true` if the local user can toggle task completion (`TodoList::canComplete`).
    #[must_use]
    pub const fn can_complete(
        &self,
        outgoing: bool,
        is_self_chat: bool,
        is_forwarded: bool,
    ) -> bool {
        !is_forwarded && (outgoing || is_self_chat || self.others_can_complete)
    }

    /// Returns `true` if the local user can append new tasks to this checklist.
    #[must_use]
    pub fn can_append(&self, outgoing: bool, is_self_chat: bool, is_forwarded: bool) -> bool {
        !is_forwarded
            && self.items.len() < TODO_MAX_ITEMS
            && (outgoing || is_self_chat || self.others_can_append)
    }

    /// Returns the next unused positive task id (`max(existing ids) + 1`).
    #[must_use]
    pub fn next_item_id(&self) -> i32 {
        self.items
            .iter()
            .map(|i| i.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(1)
    }

    /// Applies a `messageActionTodoCompletions` delta in place (`TodoListData::apply`).
    pub fn apply_completions(
        &mut self,
        actor_id: PeerId,
        actor_name: &str,
        date_unix: i32,
        completed: &[i32],
        incompleted: &[i32],
    ) {
        for &id in completed {
            if let Some(task) = self.items.iter_mut().find(|t| t.id == id) {
                task.completed_by = Some(actor_id);
                task.completed_by_name = Some(actor_name.to_owned());
                task.completion_date = date_unix.max(1);
            }
        }
        for &id in incompleted {
            if let Some(task) = self.items.iter_mut().find(|t| t.id == id) {
                task.completed_by = None;
                task.completed_by_name = None;
                task.completion_date = 0;
            }
        }
    }

    /// Appends new tasks up to [`TODO_MAX_ITEMS`], deduplicating by task id (`TodoListData::apply`).
    pub fn append_items(&mut self, incoming: &[TodoItemInput]) {
        for item in incoming {
            if self.items.len() >= TODO_MAX_ITEMS {
                break;
            }
            let id = if item.id > 0 {
                item.id
            } else {
                self.next_item_id()
            };
            if self.items.iter().any(|existing| existing.id == id) {
                continue;
            }
            self.items.push(TodoTaskItem {
                id,
                text: item.text.clone(),
                completed_by: None,
                completed_by_name: None,
                completion_date: 0,
            });
        }
    }
}

/// Input task item for creating, editing, or appending to a checklist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItemInput {
    /// Existing task id (or `0` to auto-assign sequentially starting from 1 / next free id).
    pub id: i32,
    /// Task text with optional entities.
    pub text: TextWithEntities,
}

/// Request payload for creating or editing a checklist (`EditTodoListBox::Result`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateTodoListRequest {
    /// Checklist title.
    pub title: TextWithEntities,
    /// Checklist tasks.
    pub items: Vec<TodoItemInput>,
    /// Allow other participants to add tasks.
    pub others_can_append: bool,
    /// Allow other participants to mark tasks as completed.
    pub others_can_complete: bool,
}

/// Validation failure returned by [`validate_todo_list`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoValidationError {
    /// Title is empty after trimming whitespace.
    EmptyTitle,
    /// Title exceeds the configured character limit.
    TitleTooLong {
        /// Actual title length in chars.
        len: usize,
        /// Allowed limit.
        limit: usize,
    },
    /// No non-empty tasks were entered.
    NoTasks,
    /// More than `max_items` tasks were supplied.
    TooManyTasks {
        /// Supplied count.
        count: usize,
        /// Allowed maximum.
        limit: usize,
    },
    /// A task exceeds the configured task character limit.
    TaskTooLong {
        /// Zero-based task index.
        index: usize,
        /// Actual length in chars.
        len: usize,
        /// Allowed limit.
        limit: usize,
    },
}

impl TodoValidationError {
    /// Human-readable error message suitable for toast/inline display.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::EmptyTitle => "Please enter a checklist title.".to_owned(),
            Self::TitleTooLong { len, limit } => {
                format!("Checklist title is too long ({len}/{limit} characters).")
            }
            Self::NoTasks => "Please add at least one task.".to_owned(),
            Self::TooManyTasks { count, limit } => {
                format!("Too many tasks ({count}/{limit} maximum).")
            }
            Self::TaskTooLong { index, len, limit } => {
                format!(
                    "Task #{} is too long ({len}/{limit} characters).",
                    index + 1
                )
            }
        }
    }
}

/// Validates a checklist creation/edit request using tdesktop limits.
pub fn validate_todo_list(
    req: &CreateTodoListRequest,
    title_limit: usize,
    item_limit: usize,
    max_items: usize,
) -> Result<(), TodoValidationError> {
    let title_trimmed = req.title.text.trim();
    if title_trimmed.is_empty() {
        return Err(TodoValidationError::EmptyTitle);
    }
    let title_len = title_trimmed.chars().count();
    if title_len > title_limit {
        return Err(TodoValidationError::TitleTooLong {
            len: title_len,
            limit: title_limit,
        });
    }
    let non_empty: Vec<(usize, &TodoItemInput)> = req
        .items
        .iter()
        .enumerate()
        .filter(|(_, it)| !it.text.text.trim().is_empty())
        .collect();
    if non_empty.is_empty() {
        return Err(TodoValidationError::NoTasks);
    }
    if non_empty.len() > max_items {
        return Err(TodoValidationError::TooManyTasks {
            count: non_empty.len(),
            limit: max_items,
        });
    }
    for (idx, it) in non_empty {
        let len = it.text.text.trim().chars().count();
        if len > item_limit {
            return Err(TodoValidationError::TaskTooLong {
                index: idx,
                len,
                limit: item_limit,
            });
        }
    }
    Ok(())
}

/// Splits multi-line pasted text into individual non-empty trimmed tasks.
///
/// Port of `ParsePastedList` in `boxes/edit_todo_list_box.cpp:198-217`.
#[must_use]
pub fn parse_pasted_todo_lines(raw: &str) -> Vec<String> {
    let mut lines: Vec<&str> = raw.split('\n').map(str::trim).collect();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].is_empty() && i + 1 != lines.len() {
            lines.remove(i);
        } else {
            i += 1;
        }
    }
    if lines.len() < 2 {
        return Vec::new();
    }
    lines
        .into_iter()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Formats `messageActionTodoCompletions` into human-readable service pill text.
#[must_use]
pub fn format_todo_completions_service(
    actor_name: &str,
    completed: &[i32],
    incompleted: &[i32],
) -> String {
    let c = completed.len();
    let u = incompleted.len();
    match (c, u) {
        (0, 0) => format!("{actor_name} updated the checklist"),
        (1, 0) => format!("{actor_name} completed 1 task in the checklist"),
        (n, 0) => format!("{actor_name} completed {n} tasks in the checklist"),
        (0, 1) => format!("{actor_name} marked 1 task as incomplete"),
        (0, n) => format!("{actor_name} marked {n} tasks as incomplete"),
        (cn, un) => format!("{actor_name} completed {cn} and unchecked {un} checklist tasks"),
    }
}

/// Formats `messageActionTodoAppendTasks` into human-readable service pill text.
#[must_use]
pub fn format_todo_append_service(actor_name: &str, titles: &[String]) -> String {
    match titles.len() {
        0 => format!("{actor_name} added tasks to the checklist"),
        1 => format!("{actor_name} added \"{}\" to the checklist", titles[0]),
        n => format!("{actor_name} added {n} tasks to the checklist"),
    }
}

// ---------------------------------------------------------------------------
// 3-second debounced toggle batcher (`api/api_todo_lists.cpp`)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
struct AccumulatedTodoToggle {
    completed: BTreeSet<i32>,
    incompleted: BTreeSet<i32>,
    scheduled_ms: u64,
    in_flight: bool,
}

/// Flushes batched completion toggles after [`TODO_SEND_TOGGLES_DELAY_MS`], cancelling opposite
/// toggles for the same task id before sending `messages.toggleTodoCompleted`.
///
/// Port of `Api::TodoLists` (`api/api_todo_lists.cpp:143-216`).
#[derive(Debug, Default, Clone)]
pub struct TodoToggleBatcher {
    entries: BTreeMap<(i64, i32), AccumulatedTodoToggle>,
}

impl TodoToggleBatcher {
    /// Creates an empty toggle batcher.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    const fn key(item_id: FullMsgId) -> (i64, i32) {
        (item_id.peer.0, item_id.msg.0)
    }

    /// Records a single completion toggle for `(item_id, task_id)` at `now_ms`.
    pub fn toggle(&mut self, item_id: FullMsgId, task_id: i32, completed: bool, now_ms: u64) {
        let entry = self.entries.entry(Self::key(item_id)).or_default();
        let changed = if completed {
            let c1 = entry.completed.insert(task_id);
            let c2 = entry.incompleted.remove(&task_id);
            c1 || c2
        } else {
            let c1 = entry.incompleted.insert(task_id);
            let c2 = entry.completed.remove(&task_id);
            c1 || c2
        };
        if changed {
            entry.scheduled_ms = now_ms;
        }
    }

    /// Returns all ready `(FullMsgId, completed_ids, incompleted_ids)` batches and marks them
    /// in flight. When `force` is `true`, flushes immediately regardless of the 3-second delay.
    pub fn take_ready(&mut self, now_ms: u64, force: bool) -> Vec<(FullMsgId, Vec<i32>, Vec<i32>)> {
        let mut ready = Vec::new();
        for (&(peer_raw, msg_raw), entry) in &mut self.entries {
            if entry.in_flight || (entry.completed.is_empty() && entry.incompleted.is_empty()) {
                continue;
            }
            let due = entry
                .scheduled_ms
                .saturating_add(TODO_SEND_TOGGLES_DELAY_MS);
            if force || now_ms >= due {
                let comp: Vec<i32> = entry.completed.iter().copied().collect();
                let incomp: Vec<i32> = entry.incompleted.iter().copied().collect();
                entry.completed.clear();
                entry.incompleted.clear();
                entry.scheduled_ms = 0;
                entry.in_flight = true;
                ready.push((
                    FullMsgId {
                        peer: PeerId(peer_raw),
                        msg: MsgId(msg_raw),
                    },
                    comp,
                    incomp,
                ));
            }
        }
        ready
    }

    /// Marks the in-flight RPC for `item_id` complete, pruning the entry if no further toggles
    /// accumulated while the request was in flight.
    pub fn mark_finished(&mut self, item_id: FullMsgId) {
        let key = Self::key(item_id);
        let remove = if let Some(entry) = self.entries.get_mut(&key) {
            entry.in_flight = false;
            entry.completed.is_empty() && entry.incompleted.is_empty()
        } else {
            false
        };
        if remove {
            self.entries.remove(&key);
        }
    }

    /// Returns `true` if there are any queued or in-flight toggle operations.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        !self.entries.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Checklist bubble renderer (`history/view/media/history_view_todo_list.cpp`)
// ---------------------------------------------------------------------------
