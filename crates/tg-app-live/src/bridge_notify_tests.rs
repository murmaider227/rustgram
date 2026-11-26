//! Live bug (alpha.2): right after login a desktop notification popped up for every message
//! loaded with the chat list. Only live, still-unread incoming messages may notify
//! (`History::newItemAdded` -> `HistoryItem::showNotification`, `history/history.cpp:1688-1699`,
//! `history/history_item.cpp:4641-4671,4900-4908`).

use super::*;
use tg_data::TextWithEntities;
use tg_data::UserId;

const SELF_UID: u64 = 7_001;
const PEER_UID: u64 = 7_002;

fn peer() -> DataPeerId {
    DataPeerId::from_user(UserId(PEER_UID))
}

fn incoming(id: i64) -> tg_data::Message {
    tg_data::Message::new_text(
        peer(),
        DataMsgId(id),
        Some(peer()),
        1_760_000_000,
        TextWithEntities::plain("hello".to_owned()),
    )
}

fn notifications(state: &LiveBridgeState) -> Vec<MsgId> {
    state
        .drain_notify_events()
        .into_iter()
        .filter_map(|ev| match ev {
            IncomingNotifyEvent::Notification(n) => Some(n.msg_id),
            _ => None,
        })
        .collect()
}

/// Inserts `msg` (as history or as a live update) and replays the broadcast change events
/// through the bridge.
async fn insert_and_bridge(
    session: &Arc<RwLock<Session>>,
    state: &LiveBridgeState,
    msg: tg_data::Message,
    live: bool,
) {
    let events = {
        let mut s = session.write().await;
        let mut rx = s.subscribe();
        if live {
            s.insert_new_unread_message(msg);
        } else {
            s.insert_message(msg);
        }
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    };
    for ev in events {
        handle_change_event(session, state, 1, ev).await;
    }
}

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime")
        .block_on(fut)
}

#[test]
fn history_and_dialog_messages_never_notify() {
    block_on(history_and_dialog_messages_never_notify_body());
}

async fn history_and_dialog_messages_never_notify_body() {
    let session = Arc::new(RwLock::new(Session::new(UserId(SELF_UID))));
    let state = LiveBridgeState::new();
    for id in 1..=5 {
        insert_and_bridge(&session, &state, incoming(id), false).await;
    }
    assert!(
        notifications(&state).is_empty(),
        "messages loaded with the chat list or a history slice must not notify"
    );
}

#[test]
fn live_unread_message_notifies_but_read_or_outgoing_do_not() {
    block_on(live_unread_message_notifies_body());
}

async fn live_unread_message_notifies_body() {
    let session = Arc::new(RwLock::new(Session::new(UserId(SELF_UID))));
    let state = LiveBridgeState::new();
    insert_and_bridge(&session, &state, incoming(10), false).await;
    {
        let mut s = session.write().await;
        if let Some(h) = s.history_mut(peer()) {
            h.inbox_read(DataMsgId(20), Some(0));
        }
    }
    // Already read on the server (id <= read inbox watermark).
    insert_and_bridge(&session, &state, incoming(15), true).await;
    // Outgoing.
    let mut out = incoming(21);
    out.flags.insert(tg_data::MessageFlags::OUTGOING);
    insert_and_bridge(&session, &state, out, true).await;
    assert!(
        notifications(&state).is_empty(),
        "read or outgoing: no notification"
    );

    insert_and_bridge(&session, &state, incoming(22), true).await;
    assert_eq!(notifications(&state), vec![MsgId(22)]);
}
