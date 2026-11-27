//! A chat's unread count follows tdesktop (TGB-3160): only a message added as new
//! (`NewMessageType::Unread`) raises it, and a read lowers it as soon as the messages read
//! since the old read mark are loaded (`History::countStillUnreadLocal`).

use tg_data::*;

const PEER: u64 = 300;

fn peer() -> PeerId {
    PeerId::from_user(UserId(PEER))
}

fn incoming(id: i64) -> Message {
    Message::new_text(
        peer(),
        MsgId(id),
        Some(peer()),
        1_700_000_000 + id as i32,
        TextWithEntities::plain(format!("message {id}")),
    )
}

fn outgoing(id: i64) -> Message {
    let mut m = Message::new_text(
        peer(),
        MsgId(id),
        Some(PeerId::from_user(UserId(1))),
        1_700_000_000 + id as i32,
        TextWithEntities::plain(format!("message {id}")),
    );
    m.flags.insert(MessageFlags::OUTGOING);
    m
}

/// A chat in the main list, read up to 999, with `unread` unread messages per the server.
fn session_with_unread(unread: i32) -> Session {
    let mut session = Session::new(UserId(1));
    let mut user = User::new(UserId(PEER), false);
    user.first_name = "Peer".to_owned();
    session.upsert_user(user, PeerUpdateFlags::NAME);
    let history = session.ensure_history(peer());
    history.in_chat_list = true;
    history.folder = Some(None);
    history.inbox_read_before = Some(MsgId(1000));
    history.unread_count = Some(unread);
    session.sync_dialog_lists(peer());
    session
}

fn unread(session: &Session) -> Option<i32> {
    session.history(peer()).and_then(|h| h.unread_count)
}

#[test]
fn a_message_stored_with_a_dialogs_page_keeps_the_server_count() {
    // The dialog says 91 unread; its top message (and the startup catch-up's messages) are
    // stored like history and are already part of that count.
    let mut session = session_with_unread(91);
    session.insert_message(incoming(1091));
    session.insert_message(incoming(1092));
    assert_eq!(unread(&session), Some(91));
}

#[test]
fn a_catch_up_update_adds_like_history_and_keeps_the_count() {
    let mut session = session_with_unread(91);
    session.apply_sent_or_new_message_typed(incoming(1091), false);
    assert_eq!(unread(&session), Some(91));
}

#[test]
fn a_live_new_message_still_raises_the_count() {
    let mut session = session_with_unread(91);
    session.insert_new_unread_message(incoming(1091));
    session.apply_sent_or_new_message_typed(incoming(1092), true);
    // Our own message and a message below the read mark do not count.
    session.insert_new_unread_message(outgoing(1093));
    session.insert_new_unread_message(incoming(900));
    assert_eq!(unread(&session), Some(93));
    // The same message again is not new.
    session.insert_new_unread_message(incoming(1091));
    assert_eq!(unread(&session), Some(93));
}

/// The chat opened at its first unread message: 990..=1039 is loaded, the bottom is not.
fn session_opened_at_first_unread() -> Session {
    let mut session = session_with_unread(91);
    let page: Vec<Message> = (990..1040)
        .map(|id| {
            if id % 5 == 0 {
                outgoing(id)
            } else {
                incoming(id)
            }
        })
        .collect();
    session.add_history_slice(
        peer(),
        page,
        MsgRange::new(MsgId(990), MsgId(1039)),
        Some(400),
    );
    session
}

#[test]
fn reading_loaded_unread_messages_lowers_the_count() {
    let mut session = session_opened_at_first_unread();
    // 1000..=1019 is read: 20 messages, 4 of them ours (1000, 1005, 1010, 1015).
    session.mark_inbox_read(peer(), MsgId(1019), None);
    assert_eq!(unread(&session), Some(91 - 16));
    assert!(session.take_dialog_entry_requests().is_empty());
    // Reading on from there subtracts only what was read since.
    session.mark_inbox_read(peer(), MsgId(1029), None);
    assert_eq!(unread(&session), Some(91 - 16 - 8));
}

#[test]
fn the_local_read_applies_when_it_can_count() {
    let mut session = session_opened_at_first_unread();
    assert!(session.read_inbox_till_local(peer(), MsgId(1019)));
    assert_eq!(unread(&session), Some(75));
    assert_eq!(
        session.history(peer()).map(History::inbox_read_till_id),
        Some(MsgId(1019))
    );
}

#[test]
fn the_local_read_waits_for_the_server_when_it_cannot_count() {
    let mut session = session_opened_at_first_unread();
    // 1039 is loaded, 1060 is past the loaded page: nothing can be counted.
    assert!(!session.read_inbox_till_local(peer(), MsgId(1060)));
    assert_eq!(unread(&session), Some(91));
    assert_eq!(
        session.history(peer()).map(History::inbox_read_till_id),
        Some(MsgId(999))
    );
    assert!(session.take_dialog_entry_requests().is_empty());
}

#[test]
fn a_read_that_cannot_be_counted_requests_the_dialog_entry() {
    let mut session = session_opened_at_first_unread();
    // The server confirmed a read past the loaded page (or another client read the chat).
    session.mark_inbox_read(peer(), MsgId(1060), None);
    assert_eq!(session.take_dialog_entry_requests(), vec![peer()]);
    // A stale read asks for nothing.
    session.mark_inbox_read(peer(), MsgId(1010), None);
    assert!(session.take_dialog_entry_requests().is_empty());
    // A read with the server's count needs no request either.
    session.mark_inbox_read(peer(), MsgId(1070), Some(12));
    assert_eq!(unread(&session), Some(12));
    assert!(session.take_dialog_entry_requests().is_empty());
}

#[test]
fn counting_with_the_bottom_loaded_still_counts_what_is_left() {
    let mut session = session_with_unread(91);
    let bottom: Vec<Message> = (1000..1010).map(incoming).collect();
    session.add_history_slice(
        peer(),
        bottom,
        MsgRange::new(MsgId(1000), SERVER_MAX_MSG_ID),
        Some(10),
    );
    // The server count (91) is wrong for this tiny chat, but the loaded messages from the
    // old read mark show 4 of them were read: tdesktop subtracts them first.
    session.mark_inbox_read(peer(), MsgId(1003), None);
    assert_eq!(unread(&session), Some(87));
}
