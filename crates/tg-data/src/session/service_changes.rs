//! Service messages that change another loaded message: checklist completions and appended
//! tasks, poll answers added or deleted (`History::applyServiceChanges`,
//! `history/history.cpp:1538-1600`, tdesktop 7.4.7 `fcf66d9`, `376009b`).

use super::Session;
use crate::changes::MessageUpdateFlags;
use crate::id::FullMsgId;
use crate::message::{
    Message, MessageContent, MessageFlags, MessageMedia, PollAnswer, PollData, ServiceAction,
    TodoCompletionData, TodoItemData, TodoListData,
};

impl TodoListData {
    /// Most tasks a checklist holds (`TodoListData::kMaxOptions`).
    pub const MAX_ITEMS: usize = 32;

    /// Applies `messageActionTodoCompletions` sent by `by` at `date`
    /// (`TodoListData::apply(item, data)`). Ids of tasks the list does not have are ignored.
    /// Returns `true` when a completion changed.
    pub fn apply_completions(
        &mut self,
        by: crate::id::PeerId,
        date: crate::id::TimeId,
        completed: &[i32],
        incompleted: &[i32],
    ) -> bool {
        let mut changed = false;
        for &id in completed {
            if !self.has_item(id) {
                continue;
            }
            match self.completions.iter_mut().find(|c| c.id == id) {
                Some(c) if c.completed_by == by && c.date == date => {}
                Some(c) => {
                    c.completed_by = by;
                    c.date = date;
                    changed = true;
                }
                None => {
                    self.completions.push(TodoCompletionData {
                        id,
                        completed_by: by,
                        date,
                    });
                    changed = true;
                }
            }
        }
        for &id in incompleted {
            if !self.has_item(id) {
                continue;
            }
            let before = self.completions.len();
            self.completions.retain(|c| c.id != id);
            changed |= self.completions.len() != before;
        }
        changed
    }

    /// Applies `messageActionTodoAppendTasks`: adds tasks whose id is new, up to
    /// [`Self::MAX_ITEMS`]. Returns `true` when a task was added.
    pub fn append_tasks(&mut self, list: &[TodoItemData]) -> bool {
        let was = self.items.len();
        for task in list {
            if self.items.len() >= Self::MAX_ITEMS {
                break;
            }
            if !self.has_item(task.id) {
                self.items.push(task.clone());
            }
        }
        self.items.len() != was
    }

    fn has_item(&self, id: i32) -> bool {
        self.items.iter().any(|item| item.id == id)
    }
}

impl PollData {
    /// Applies `messageActionPollAppendAnswer`: adds the answer unless its option exists.
    /// Returns `true` when it was added.
    pub fn append_answer(&mut self, answer: &PollAnswer) -> bool {
        if self.answers.iter().any(|a| a.option == answer.option) {
            return false;
        }
        self.answers.push(answer.clone());
        true
    }

    /// Applies `messageActionPollDeleteAnswer`: removes the answers with `option`.
    /// Returns `true` when one was removed.
    pub fn delete_answer(&mut self, option: &[u8]) -> bool {
        let before = self.answers.len();
        self.answers.retain(|a| a.option != option);
        self.answers.len() != before
    }
}

impl Session {
    /// Applies a service message that arrived as new (`History::addNewMessage` with
    /// `NewMessageType::Unread` -> `applyMessageChanges`) to the loaded message it refers to,
    /// and announces that message as edited so open views repaint it. Messages that come
    /// with history pages are never applied: the target already holds the later state.
    pub(crate) fn apply_service_changes(&mut self, service: &Message) {
        if !service.flags.contains(MessageFlags::HISTORY_ENTRY) {
            return;
        }
        let Some(action) = service.service_action() else {
            return;
        };
        let Some(reply) = service.reply_to.as_ref() else {
            return;
        };
        let Some(target_msg) = reply.reply_to_msg_id else {
            return;
        };
        let target = FullMsgId::new(
            reply.reply_to_peer_id.unwrap_or(service.peer_id),
            target_msg,
        );
        let by = service.from_id.unwrap_or(service.peer_id);
        let Some(MessageContent::Regular { media, .. }) =
            self.messages.get_mut(&target).map(|m| &mut m.content)
        else {
            return;
        };
        let changed = match (action, media) {
            (
                ServiceAction::TodoCompletions {
                    completed,
                    incompleted,
                },
                MessageMedia::TodoList(todo),
            ) => todo.apply_completions(by, service.date, completed, incompleted),
            (ServiceAction::TodoAppendTasks { list }, MessageMedia::TodoList(todo)) => {
                todo.append_tasks(list)
            }
            (ServiceAction::PollAppendAnswer { answer }, MessageMedia::Poll(poll)) => {
                poll.append_answer(answer)
            }
            (ServiceAction::PollDeleteAnswer { answer }, MessageMedia::Poll(poll)) => {
                poll.delete_answer(&answer.option)
            }
            _ => false,
        };
        if changed {
            self.changes
                .message_updated(target, MessageUpdateFlags::EDITED);
        }
    }
}
