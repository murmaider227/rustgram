//! A new service message that changes a loaded message is applied to it, like
//! `History::applyServiceChanges` (`history/history.cpp:1538-1600`): checklist completions and
//! appended tasks (tdesktop 7.4.7 `fcf66d9`, `376009b`), poll answers added or deleted
//! (TGB-3150). Messages that come with history pages are not applied.

use tg_data::*;
use tokio::sync::broadcast::Receiver;

const CHAT: u64 = 300;
const CHECKLIST: i64 = 10;
const POLL: i64 = 11;

fn chat() -> PeerId {
    PeerId::from_chat(ChatId(CHAT))
}

fn me() -> PeerId {
    PeerId::from_user(UserId(1))
}

fn member() -> PeerId {
    PeerId::from_user(UserId(2))
}

fn task(id: i32, title: &str) -> TodoItemData {
    TodoItemData {
        id,
        title: TextWithEntities::plain(title),
    }
}

fn answer(option: u8, text: &str) -> PollAnswer {
    PollAnswer {
        text: TextWithEntities::plain(text),
        option: vec![option],
    }
}

fn with_media(id: i64, media: MessageMedia) -> Message {
    let mut m = Message::new_text(
        chat(),
        MsgId(id),
        Some(me()),
        1_700_000_000,
        TextWithEntities::plain(""),
    );
    m.content = MessageContent::Regular {
        text: TextWithEntities::plain(""),
        media,
    };
    m
}

/// A group whose loaded page holds a checklist with tasks 1 and 2 and a poll with answers 1 and 2.
fn session_with_loaded_checklist_and_poll() -> Session {
    let mut session = Session::new(UserId(1));
    let checklist = with_media(
        CHECKLIST,
        MessageMedia::TodoList(Box::new(TodoListData {
            title: TextWithEntities::plain("Trip"),
            items: vec![task(1, "Tickets"), task(2, "Hotel")],
            others_can_append: true,
            others_can_complete: true,
            ..Default::default()
        })),
    );
    let poll = with_media(
        POLL,
        MessageMedia::Poll(Box::new(PollData {
            question: TextWithEntities::plain("When?"),
            answers: vec![answer(1, "Friday"), answer(2, "Saturday")],
            ..Default::default()
        })),
    );
    session.add_history_slice(
        chat(),
        vec![checklist, poll],
        MsgRange::new(MsgId(1), SERVER_MAX_MSG_ID),
        Some(2),
    );
    session
}

fn service(id: i64, date: TimeId, target: i64, action: ServiceAction) -> Message {
    let mut m = Message::new_service(chat(), MsgId(id), Some(member()), date, action);
    m.reply_to = Some(ReplyInfo {
        reply_to_msg_id: Some(MsgId(target)),
        ..Default::default()
    });
    m
}

fn todo(session: &Session) -> TodoListData {
    match session
        .message(FullMsgId::new(chat(), MsgId(CHECKLIST)))
        .and_then(Message::media)
    {
        Some(MessageMedia::TodoList(todo)) => (**todo).clone(),
        other => panic!("expected a checklist, got {other:?}"),
    }
}

fn poll_options(session: &Session) -> Vec<Vec<u8>> {
    match session
        .message(FullMsgId::new(chat(), MsgId(POLL)))
        .and_then(Message::media)
    {
        Some(MessageMedia::Poll(poll)) => poll.answers.iter().map(|a| a.option.clone()).collect(),
        other => panic!("expected a poll, got {other:?}"),
    }
}

fn edited_ids(rx: &mut Receiver<ChangeEvent>) -> Vec<MsgId> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let ChangeEvent::MessageUpdated { id, flags } = ev
            && flags.contains(MessageUpdateFlags::EDITED)
        {
            out.push(id.msg);
        }
    }
    out
}

#[test]
fn another_members_completions_update_the_loaded_checklist() {
    let mut session = session_with_loaded_checklist_and_poll();
    let mut rx = session.subscribe();

    session.insert_new_unread_message(service(
        20,
        1_700_000_100,
        CHECKLIST,
        ServiceAction::TodoCompletions {
            completed: vec![1, 99],
            incompleted: Vec::new(),
        },
    ));

    assert_eq!(
        todo(&session).completions,
        vec![TodoCompletionData {
            id: 1,
            completed_by: member(),
            date: 1_700_000_100,
        }],
        "task 1 completed by the sender at the service date; unknown task 99 ignored"
    );
    assert_eq!(edited_ids(&mut rx), vec![MsgId(CHECKLIST)]);

    session.insert_new_unread_message(service(
        21,
        1_700_000_200,
        CHECKLIST,
        ServiceAction::TodoCompletions {
            completed: vec![2],
            incompleted: vec![1],
        },
    ));
    let done = todo(&session).completions;
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].id, 2);
    assert_eq!(edited_ids(&mut rx), vec![MsgId(CHECKLIST)]);
}

#[test]
fn completions_from_the_startup_catch_up_are_applied_too() {
    let mut session = session_with_loaded_checklist_and_poll();
    session.apply_sent_or_new_message_typed(
        service(
            20,
            1_700_000_100,
            CHECKLIST,
            ServiceAction::TodoCompletions {
                completed: vec![2],
                incompleted: Vec::new(),
            },
        ),
        false,
    );
    assert_eq!(todo(&session).completions.len(), 1);
}

#[test]
fn a_repeated_completion_does_not_announce_an_edit() {
    let mut session = session_with_loaded_checklist_and_poll();
    let action = || ServiceAction::TodoCompletions {
        completed: vec![1],
        incompleted: vec![2],
    };
    session.insert_new_unread_message(service(20, 1_700_000_100, CHECKLIST, action()));
    let mut rx = session.subscribe();
    // A second service message with the same effect (same sender and date).
    session.insert_new_unread_message(service(21, 1_700_000_100, CHECKLIST, action()));
    assert!(edited_ids(&mut rx).is_empty());
}

#[test]
fn appended_tasks_update_the_loaded_checklist_up_to_the_limit() {
    let mut session = session_with_loaded_checklist_and_poll();
    let mut rx = session.subscribe();

    session.insert_new_unread_message(service(
        20,
        1_700_000_100,
        CHECKLIST,
        ServiceAction::TodoAppendTasks {
            list: vec![task(2, "Duplicate id"), task(3, "Visas")],
        },
    ));
    let items = todo(&session).items;
    let titles: Vec<&str> = items.iter().map(|t| t.title.text.as_str()).collect();
    assert_eq!(titles, ["Tickets", "Hotel", "Visas"]);
    assert_eq!(edited_ids(&mut rx), vec![MsgId(CHECKLIST)]);

    let many: Vec<TodoItemData> = (4..60).map(|id| task(id, "More")).collect();
    session.insert_new_unread_message(service(
        21,
        1_700_000_200,
        CHECKLIST,
        ServiceAction::TodoAppendTasks { list: many },
    ));
    assert_eq!(todo(&session).items.len(), TodoListData::MAX_ITEMS);
}

#[test]
fn poll_answers_added_and_deleted_update_the_loaded_poll() {
    let mut session = session_with_loaded_checklist_and_poll();
    let mut rx = session.subscribe();

    session.insert_new_unread_message(service(
        20,
        1_700_000_100,
        POLL,
        ServiceAction::PollAppendAnswer {
            answer: answer(3, "Sunday"),
        },
    ));
    assert_eq!(poll_options(&session), vec![vec![1], vec![2], vec![3]]);
    assert_eq!(edited_ids(&mut rx), vec![MsgId(POLL)]);

    // An existing option is not added twice.
    session.insert_new_unread_message(service(
        21,
        1_700_000_150,
        POLL,
        ServiceAction::PollAppendAnswer {
            answer: answer(3, "Sunday"),
        },
    ));
    assert_eq!(poll_options(&session).len(), 3);
    assert!(edited_ids(&mut rx).is_empty());

    session.insert_new_unread_message(service(
        22,
        1_700_000_200,
        POLL,
        ServiceAction::PollDeleteAnswer {
            answer: answer(1, "Friday"),
        },
    ));
    assert_eq!(poll_options(&session), vec![vec![2], vec![3]]);
    assert_eq!(edited_ids(&mut rx), vec![MsgId(POLL)]);
}

#[test]
fn service_messages_from_history_pages_are_not_applied() {
    let mut session = session_with_loaded_checklist_and_poll();
    let old = service(
        5,
        1_699_999_000,
        CHECKLIST,
        ServiceAction::TodoCompletions {
            completed: vec![1, 2],
            incompleted: Vec::new(),
        },
    );
    session.add_history_slice(
        chat(),
        vec![old.clone()],
        MsgRange::new(MsgId(1), SERVER_MAX_MSG_ID),
        Some(3),
    );
    session.insert_message(old);
    assert!(todo(&session).completions.is_empty());
}

#[test]
fn a_service_message_that_is_already_stored_is_not_applied_again() {
    let mut session = session_with_loaded_checklist_and_poll();
    let first = service(
        20,
        1_700_000_100,
        CHECKLIST,
        ServiceAction::TodoCompletions {
            completed: vec![1],
            incompleted: Vec::new(),
        },
    );
    session.insert_new_unread_message(first.clone());
    // Someone unticks task 1 later; the first service message is then delivered again.
    session.insert_new_unread_message(service(
        21,
        1_700_000_200,
        CHECKLIST,
        ServiceAction::TodoCompletions {
            completed: Vec::new(),
            incompleted: vec![1],
        },
    ));
    session.insert_new_unread_message(first);
    assert!(todo(&session).completions.is_empty());
}
