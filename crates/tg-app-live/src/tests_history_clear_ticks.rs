//! The "history cleared" service item displays nothing (TGB-2909) and chat-list ticks follow
//! `HistoryItem::needCheck` in Saved Messages (TGB-2910).

use tg_app_core::dialogs::backend::DialogListQuery;
use tg_app_core::stand_in::types::FilterId;
use tg_data::{
    Message, MessageContent, MessageFlags, MsgId as DataMsgId, PeerId as DataPeerId,
    PeerUpdateFlags, ServiceAction, Session, TextWithEntities as DataText, User,
    UserId as DataUserId,
};
use tg_history::DialogSendState;

use crate::convert::{dialog_item_from_session, history_messages_iter, message_send_state};

const SELF_ID: u64 = 1001;
const PEER_ID: u64 = 2001;
const NOW: i32 = 1_764_000_000;

fn session_with_users() -> Session {
    let mut session = Session::new(DataUserId(SELF_ID));
    let mut me = User::new(DataUserId(SELF_ID), true);
    me.set_name("Self", "User", "", "");
    session.upsert_user(me, PeerUpdateFlags::all());
    let mut peer = User::new(DataUserId(PEER_ID), false);
    peer.set_name("Theo", "Reed", "", "");
    session.upsert_user(peer, PeerUpdateFlags::all());
    session
}

fn self_peer() -> DataPeerId {
    DataPeerId::from_user(DataUserId(SELF_ID))
}

fn note(id: i64, text: &str) -> Message {
    Message::new_text(
        self_peer(),
        DataMsgId(id),
        Some(self_peer()),
        NOW + id as i32,
        DataText::plain(text),
    )
}

fn history_clear(peer: DataPeerId, id: i64) -> Message {
    let mut msg = Message::new_text(
        peer,
        DataMsgId(id),
        Some(self_peer()),
        NOW + 100,
        DataText::plain(""),
    );
    msg.content = MessageContent::Service(Box::new(ServiceAction::HistoryClear));
    msg
}

#[test]
fn history_clear_item_is_not_in_the_history_and_has_an_empty_preview() {
    let mut session = session_with_users();
    session.insert_message(note(1, "kept note"));
    session.insert_message(history_clear(self_peer(), 2));

    let shown: Vec<i64> = history_messages_iter(&session, self_peer())
        .map(|m| m.id.0)
        .collect();
    assert_eq!(shown, vec![1], "the history-clear item draws nothing");

    let row = dialog_item_from_session(
        &session,
        self_peer(),
        DialogListQuery::Filter(FilterId::ALL),
        NOW + 200,
    )
    .expect("saved messages row");
    assert_eq!(row.last_message, "", "no \"Service message\" preview");
    assert_eq!(
        row.last_message_id.0, 0,
        "an item that displays nothing gives the row no date"
    );
    assert!(row.media_preview.is_none(), "no service tint or icon");
    assert_eq!(row.send_state, DialogSendState::None);
}

#[test]
fn history_clear_top_message_in_a_private_chat_shows_no_ticks() {
    let mut session = session_with_users();
    let peer = DataPeerId::from_user(DataUserId(PEER_ID));
    let mut clear = history_clear(peer, 5);
    clear.flags.insert(MessageFlags::OUTGOING);
    session.insert_message(clear);
    let row = dialog_item_from_session(
        &session,
        peer,
        DialogListQuery::Filter(FilterId::ALL),
        NOW + 200,
    )
    .expect("private chat row");
    assert_eq!(row.last_message, "");
    assert_eq!(row.send_state, DialogSendState::None);
}

#[test]
fn saved_messages_row_ticks_follow_need_check() {
    let session = session_with_users();

    let sent = note(1, "confirmed note");
    assert_eq!(
        message_send_state(&session, &sent),
        DialogSendState::None,
        "a confirmed note is not `out`, so needCheck() is false"
    );

    let mut pending = note(tg_data::START_CLIENT_MSG_ID.0 + 1, "pending note");
    pending.flags.insert(MessageFlags::BEING_SENT);
    assert_eq!(
        message_send_state(&session, &pending),
        DialogSendState::Sending,
        "a pending note in the self chat shows the clock"
    );

    let peer = DataPeerId::from_user(DataUserId(PEER_ID));
    let mut out = Message::new_text(
        peer,
        DataMsgId(7),
        Some(self_peer()),
        NOW,
        DataText::plain("hi"),
    );
    out.flags.insert(MessageFlags::OUTGOING);
    assert_ne!(
        message_send_state(&session, &out),
        DialogSendState::None,
        "other chats keep their ticks"
    );
}
