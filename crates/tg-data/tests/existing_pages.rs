//! Messages stored from pages that are not the chat's own bottom (topic and comment pages,
//! search results, unread-mention pages) are `NewMessageType::Existing` in tdesktop
//! (`History::addNewMessage`, `history/history.cpp:669-700`): they are stored but never
//! announced as new, and they never touch the chat's counters or chat-list position (TGB-2967).

use tg_data::*;
use tokio::sync::broadcast::Receiver;

const PEER: u64 = 200;

fn peer() -> PeerId {
    PeerId::from_user(UserId(PEER))
}

fn incoming(id: i64, mentions_me: bool) -> Message {
    let mut m = Message::new_text(
        peer(),
        MsgId(id),
        Some(peer()),
        1_700_000_000 + id as i32,
        TextWithEntities::plain(format!("message {id}")),
    );
    m.flags.set(MessageFlags::MENTIONS_ME, mentions_me);
    m.flags.set(MessageFlags::MEDIA_IS_UNREAD, mentions_me);
    m
}

/// A chat whose bottom page 1000..=1099 is loaded and read, with a known unread count of 0.
fn session_with_read_bottom() -> Session {
    let mut session = Session::new(UserId(1));
    let mut user = User::new(UserId(PEER), false);
    user.first_name = "Peer".to_owned();
    session.upsert_user(user, PeerUpdateFlags::NAME);
    let bottom: Vec<Message> = (1000..1100).map(|id| incoming(id, false)).collect();
    session.add_history_slice(
        peer(),
        bottom,
        MsgRange::new(MsgId(1000), SERVER_MAX_MSG_ID),
        Some(1100),
    );
    let history = session.ensure_history(peer());
    history.inbox_read(MsgId(1099), Some(0));
    history.unread_count = Some(0);
    session
}

fn new_added_ids(rx: &mut Receiver<ChangeEvent>) -> Vec<FullMsgId> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let ChangeEvent::MessageUpdated { id, flags } = ev
            && flags.contains(MessageUpdateFlags::NEW_ADDED)
        {
            out.push(id);
        }
    }
    out
}

#[test]
fn a_page_of_100_older_existing_messages_announces_nothing_and_keeps_counters() {
    let mut session = session_with_read_bottom();
    let before = session.history(peer()).cloned().expect("history");
    let mut rx = session.subscribe();

    // An older page as a topic / search / mentions page brings it: every third message
    // mentions the account and some ids are above the read mark of the chat.
    for id in (900..1000).rev() {
        session.insert_existing_message(incoming(id, id % 3 == 0));
    }
    for id in 1100..1105 {
        session.insert_existing_message(incoming(id, false));
    }

    assert_eq!(new_added_ids(&mut rx), Vec::<FullMsgId>::new());
    let after = session.history(peer()).expect("history");
    assert_eq!(after.unread_count, Some(0), "unread counter untouched");
    assert_eq!(after.unread_mentions_count, before.unread_mentions_count);
    assert_eq!(after.top_message_id, before.top_message_id, "chat-list top");
    assert_eq!(after.chat_list_time_id, before.chat_list_time_id);
    assert_eq!(after.loaded_ranges, before.loaded_ranges, "loaded history");
    for id in (900..1000).chain(1100..1105) {
        assert!(
            session.message(FullMsgId::new(peer(), MsgId(id))).is_some(),
            "message {id} stored"
        );
    }
}

#[test]
fn an_existing_insert_of_a_known_message_repaints_it_as_edited() {
    let mut session = session_with_read_bottom();
    let mut rx = session.subscribe();
    let mut edited = incoming(1050, false);
    edited.edit_date = Some(1_800_000_000);
    session.insert_existing_message(edited);

    let mut flags_seen = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let ChangeEvent::MessageUpdated { id, flags } = ev {
            flags_seen.push((id.msg, flags));
        }
    }
    assert_eq!(flags_seen.len(), 1);
    assert_eq!(flags_seen[0].0, MsgId(1050));
    assert!(flags_seen[0].1.contains(MessageUpdateFlags::EDITED));
    assert!(!flags_seen[0].1.contains(MessageUpdateFlags::NEW_ADDED));
    assert_eq!(
        session
            .message(FullMsgId::new(peer(), MsgId(1050)))
            .and_then(|m| m.edit_date),
        Some(1_800_000_000)
    );
}

#[test]
fn a_live_insert_still_announces_a_new_message() {
    let mut session = session_with_read_bottom();
    let mut rx = session.subscribe();
    session.insert_new_unread_message(incoming(1100, false));
    assert_eq!(
        new_added_ids(&mut rx),
        vec![FullMsgId::new(peer(), MsgId(1100))]
    );
    assert_eq!(
        session.history(peer()).and_then(|h| h.unread_count),
        Some(1)
    );
}
