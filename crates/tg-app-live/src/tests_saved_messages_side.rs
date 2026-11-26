//! Which side a Saved Messages bubble is drawn on (`Message::hasOutLayout`,
//! `history_view_message.cpp`): own notes on the right, also after the server echo (which comes
//! with `out = false` in the self chat, see `NewMessageFlags`), and a message forwarded from
//! someone else on the left.

use tg_api::convert::message_from_tl;
use tg_data::{PeerId as DataPeerId, UserId as DataUserId};
use tg_text::TextWithEntities;
use tg_tl::{
    Deserializable, Identifiable, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};

use super::tests_reply_targets::{
    PEER_ID, SELF_ID, build_live, tl_message, tl_user_peer, wait_until,
};
use crate::LiveSession;
use tg_app_core::history::backend::{
    HistoryBackend, HistoryLiveUpdate, HistoryMessage, SendMessageRequest,
};
use tg_app_core::stand_in::types::{MsgId, PeerId};

fn self_peer() -> PeerId {
    PeerId(DataPeerId::from_user(DataUserId(SELF_ID as u64)).0)
}

/// The server's copy of a message in the self chat: `out = false`, `saved_peer_id` = self.
fn server_note(id: i32, text: &str, from_id: Option<i64>) -> tl_enums::Message {
    let mut m = tl_message(id, tl_user_peer(SELF_ID), SELF_ID, text, None);
    if let tl_enums::Message::Message(ref mut inner) = m {
        inner.out = false;
        inner.from_id = from_id.map(tl_user_peer);
        inner.saved_peer_id = Some(tl_user_peer(SELF_ID));
    }
    m
}

fn forwarded_from_other(id: i32) -> tl_enums::Message {
    let mut m = server_note(id, "forwarded words", Some(SELF_ID));
    if let tl_enums::Message::Message(ref mut inner) = m {
        inner.saved_peer_id = Some(tl_user_peer(PEER_ID));
        inner.fwd_from = Some(tl_enums::MessageFwdHeader::MessageFwdHeader(
            tl_types::MessageFwdHeader {
                imported: false,
                saved_out: false,
                from_id: Some(tl_user_peer(PEER_ID)),
                from_name: None,
                date: 1_795_000_000,
                channel_post: None,
                post_author: None,
                saved_from_peer: None,
                saved_from_msg_id: None,
                saved_from_id: None,
                saved_from_name: None,
                saved_date: None,
                psa_type: None,
            },
        ));
    }
    m
}

fn send_note(live: &mut LiveSession) -> HistoryMessage {
    live.send_message(
        self_peer(),
        SendMessageRequest {
            text: TextWithEntities::simple("note to self"),
            reply_to: None,
            silent: false,
        },
    )
    .expect("send to Saved Messages")
}

/// Drains updates until message `id` shows up added or edited; returns its last version.
fn wait_for_message(live: &mut LiveSession, id: i32) -> HistoryMessage {
    let mut seen = None;
    let ok = wait_until(|| {
        for update in live.drain_history_updates(self_peer()) {
            if let HistoryLiveUpdate::MessageAdded(m) | HistoryLiveUpdate::MessageEdited(m) = update
                && m.id == MsgId(id)
            {
                seen = Some(*m);
            }
        }
        seen.is_some()
    });
    assert!(ok, "message {id} never reached the open history");
    seen.expect("seen")
}

fn check_note_keeps_its_side(answer: impl Fn(i64) -> tl_enums::Updates + Send + Sync + 'static) {
    let (rpc, mut live) = build_live(
        DataPeerId(self_peer().0),
        vec![server_note(10, "older note", Some(SELF_ID))],
    );
    rpc.set_handler(move |ctor, bytes| {
        (ctor == tl_fns::messages::SendMessage::CONSTRUCTOR_ID).then(|| {
            let req = tl_fns::messages::SendMessage::from_bytes(bytes).expect("decode send");
            answer(req.random_id).to_bytes()
        })
    });
    let _ = live.load_slice_around(self_peer(), MsgId::SHOW_AT_END, 50, 0);
    let older = live.load_slice_around(self_peer(), MsgId::SHOW_AT_END, 50, 0);
    assert!(
        older.messages[0].outgoing,
        "an older own note is drawn on the right"
    );

    let pending = send_note(&mut live);
    assert!(pending.outgoing, "the pending note is drawn on the right");
    let confirmed = wait_for_message(&mut live, 11);
    assert_eq!(
        confirmed.outgoing, pending.outgoing,
        "the confirmed note must stay on the side of the pending one"
    );
    let reopened = live.load_slice_around(self_peer(), MsgId::SHOW_AT_END, 50, 0);
    let last = reopened.messages.last().expect("note in history");
    assert_eq!(last.id, MsgId(11));
    assert!(last.outgoing, "the note stays on the right after reopening");
}

fn updates_with(random_id: i64, message: tl_enums::Message) -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![
            tl_enums::Update::MessageID(tl_types::UpdateMessageID { id: 11, random_id }),
            tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                message,
                pts: 2,
                pts_count: 1,
            }),
        ],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_340_011,
        seq: 0,
    })
}

#[test]
fn saved_messages_note_stays_right_when_the_server_echo_has_from_id() {
    check_note_keeps_its_side(|random_id| {
        updates_with(random_id, server_note(11, "note to self", Some(SELF_ID)))
    });
}

#[test]
fn saved_messages_note_stays_right_when_the_server_echo_has_no_from_id() {
    check_note_keeps_its_side(|random_id| {
        updates_with(random_id, server_note(11, "note to self", None))
    });
}

#[test]
fn saved_messages_note_stays_right_after_a_short_sent_answer() {
    check_note_keeps_its_side(|_| {
        tl_enums::Updates::UpdateShortSentMessage(tl_types::UpdateShortSentMessage {
            out: false,
            id: 11,
            pts: 2,
            pts_count: 1,
            date: 1_795_340_011,
            media: None,
            entities: None,
            ttl_period: None,
        })
    });
}

#[test]
fn saved_messages_forward_from_someone_else_is_drawn_on_the_left() {
    let (_rpc, live) = build_live(
        DataPeerId(self_peer().0),
        vec![
            server_note(10, "own note", Some(SELF_ID)),
            forwarded_from_other(12),
        ],
    );
    let slice = live.load_slice_around(self_peer(), MsgId::SHOW_AT_END, 50, 0);
    let side = |id| {
        slice
            .messages
            .iter()
            .find(|m| m.id == MsgId(id))
            .map(|m| m.outgoing)
            .expect("message in slice")
    };
    assert!(side(10), "own note: right");
    assert!(
        !side(12),
        "forwarded from another user: left, as in tdesktop"
    );

    // The same forward with `out = true` (as some servers send it) still goes left.
    let mut out_forward = forwarded_from_other(13);
    if let tl_enums::Message::Message(ref mut inner) = out_forward {
        inner.out = true;
    }
    live.write_session(|s| {
        let self_id = s.self_user_id();
        if let Some(m) = message_from_tl(&out_forward, self_id) {
            s.insert_message(m);
        }
    });
    let slice = live.load_slice_around(self_peer(), MsgId::SHOW_AT_END, 50, 0);
    let fwd = slice
        .messages
        .iter()
        .find(|m| m.id == MsgId(13))
        .expect("forward in slice");
    assert!(!fwd.outgoing);
}

#[test]
fn saved_messages_forward_of_own_outgoing_message_is_drawn_on_the_right() {
    // Forwarded again from a chat where the saved original was ours (`saved_out`):
    // tdesktop's `forwardOfForward` branch draws it on the right.
    let mut fwd = forwarded_from_other(14);
    if let tl_enums::Message::Message(ref mut inner) = fwd
        && let Some(tl_enums::MessageFwdHeader::MessageFwdHeader(ref mut h)) = inner.fwd_from
    {
        h.saved_out = true;
        h.saved_from_name = Some("Somebody".to_owned());
    }
    let (_rpc, live) = build_live(DataPeerId(self_peer().0), vec![fwd]);
    let slice = live.load_slice_around(self_peer(), MsgId::SHOW_AT_END, 50, 0);
    assert!(slice.messages[0].outgoing);
}
