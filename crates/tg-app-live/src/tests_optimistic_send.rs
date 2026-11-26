//! A sent message shows at once as a pending (local) message and the server reply swaps in the
//! server id (TGB-2931); page reads use the session's range queries (TGB-2938).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tg_data::{PeerId as DataPeerId, UserId as DataUserId};
use tg_text::TextWithEntities;
use tg_tl::{
    Deserializable, Identifiable, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};

use super::tests_reply_targets::{
    FakeRpcClient, PEER_ID, SELF_ID, build_live, tl_message, tl_user_peer, wait_until,
};
use crate::LiveSession;
use crate::convert::{from_data_msg, to_data_msg};
use tg_app_core::history::backend::{
    HistoryBackend, HistoryLiveUpdate, HistoryMessage, SendMessageRequest,
};
use tg_app_core::stand_in::types::{MsgId, PeerId};

/// Simulated network round trip of the fake server.
const ROUND_TRIP: Duration = Duration::from_millis(400);

fn peer() -> PeerId {
    PeerId(DataPeerId::from_user(DataUserId(PEER_ID as u64)).0)
}

fn chat(history: std::ops::RangeInclusive<i32>) -> (FakeRpcClient, LiveSession) {
    build_live(
        DataPeerId(peer().0),
        history
            .map(|id| tl_message(id, tl_user_peer(PEER_ID), PEER_ID, "earlier", None))
            .collect(),
    )
}

fn request(text: &str) -> SendMessageRequest {
    SendMessageRequest {
        text: TextWithEntities::simple(text),
        reply_to: None,
        silent: false,
    }
}

/// Answers each `messages.sendMessage` after [`ROUND_TRIP`] with `updateMessageID` and the
/// server message, numbering server ids from `first_id` in the order the requests arrive.
fn answer_sends(rpc: &FakeRpcClient, first_id: i32) -> Arc<Mutex<Vec<String>>> {
    let received = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&received);
    let next = Arc::new(Mutex::new(first_id));
    rpc.set_handler(move |ctor, bytes| {
        if ctor != tl_fns::messages::SendMessage::CONSTRUCTOR_ID {
            return None;
        }
        let req = tl_fns::messages::SendMessage::from_bytes(bytes).ok()?;
        std::thread::sleep(ROUND_TRIP);
        let id = {
            let mut n = next.lock().ok()?;
            let id = *n;
            *n += 1;
            id
        };
        if let Ok(mut l) = log.lock() {
            l.push(req.message.clone());
        }
        let message = tl_message(id, tl_user_peer(PEER_ID), SELF_ID, &req.message, None);
        Some(
            tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![
                    tl_enums::Update::MessageID(tl_types::UpdateMessageID {
                        id,
                        random_id: req.random_id,
                    }),
                    tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                        message,
                        pts: id - first_id + 2,
                        pts_count: 1,
                    }),
                ],
                users: Vec::new(),
                chats: Vec::new(),
                date: 1_795_340_011,
                seq: 0,
            })
            .to_bytes(),
        )
    });
    received
}

/// Drains history updates until `done` holds for everything collected so far.
fn collect_updates(
    live: &mut LiveSession,
    mut done: impl FnMut(&[HistoryLiveUpdate]) -> bool,
) -> Vec<HistoryLiveUpdate> {
    let mut all = Vec::new();
    let ok = wait_until(|| {
        all.extend(live.drain_history_updates(peer()));
        done(&all)
    });
    assert!(ok, "updates never arrived: {all:?}");
    all
}

fn sent_swaps(updates: &[HistoryLiveUpdate]) -> Vec<(MsgId, HistoryMessage)> {
    updates
        .iter()
        .filter_map(|u| match u {
            HistoryLiveUpdate::LocalMessageSent { local_id, message } => {
                Some((*local_id, (**message).clone()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn local_ids_map_one_to_one_and_never_look_like_drafts_or_server_ids() {
    let a = tg_data::MsgId::client_by_index(crate::optimistic_send::UI_FIRST_LOCAL_INDEX)
        .expect("client id");
    let b = tg_data::MsgId::client_by_index(crate::optimistic_send::UI_FIRST_LOCAL_INDEX + 1)
        .expect("client id");
    let (ui_a, ui_b) = (from_data_msg(a), from_data_msg(b));
    assert_ne!(ui_a, ui_b, "two pending messages must not share an id");
    assert!(ui_a < ui_b, "local ids keep the send order");
    assert!(ui_a.is_local() && ui_b.is_local());
    assert!(!ui_a.is_streamed_draft());
    assert!(MsgId(-100_000).is_streamed_draft() && !MsgId(-100_000).is_local());
    assert_eq!(to_data_msg(ui_a), a);
    assert_eq!(to_data_msg(ui_b), b);
    assert_eq!(to_data_msg(MsgId(42)), tg_data::MsgId(42));
    assert_eq!(from_data_msg(tg_data::MsgId(42)), MsgId(42));
}

#[test]
fn sending_shows_the_pending_message_at_once_and_keeps_the_order() {
    let (rpc, mut live) = chat(10..=12);
    let received = answer_sends(&rpc, 500);
    let _ = live.load_slice_around(peer(), MsgId::SHOW_AT_END, 50, 0);

    let started = Instant::now();
    let pending: Vec<HistoryMessage> = ["one", "two", "three"]
        .into_iter()
        .map(|text| live.send_message(peer(), request(text)).expect("send"))
        .collect();
    let ui_wait = started.elapsed();
    eprintln!("3 sends returned after {ui_wait:?} (server round trip {ROUND_TRIP:?} each)");
    assert!(ui_wait < ROUND_TRIP / 2, "the UI waited {ui_wait:?}");
    for (msg, text) in pending.iter().zip(["one", "two", "three"]) {
        assert!(msg.id.is_local(), "{text}: {:?} is not a local id", msg.id);
        assert!(
            msg.is_sending,
            "{text}: the pending message shows the clock"
        );
        assert!(msg.outgoing);
        assert_eq!(msg.text.text, text);
    }
    assert!(pending.windows(2).all(|w| w[0].id < w[1].id));

    // While the sends are in flight a reopened chat shows them at the bottom, in order.
    let reopened = live.load_slice_around(peer(), MsgId::SHOW_AT_END, 50, 0);
    let tail: Vec<MsgId> = reopened
        .messages
        .iter()
        .rev()
        .take(3)
        .map(|m| m.id)
        .collect();
    assert_eq!(
        tail,
        pending.iter().rev().map(|m| m.id).collect::<Vec<_>>(),
        "pending messages follow the newest server message"
    );

    let updates = collect_updates(&mut live, |u| sent_swaps(u).len() == 3);
    let swaps = sent_swaps(&updates);
    assert_eq!(
        swaps.iter().map(|(local, _)| *local).collect::<Vec<_>>(),
        pending.iter().map(|m| m.id).collect::<Vec<_>>()
    );
    assert_eq!(
        swaps.iter().map(|(_, m)| m.id).collect::<Vec<_>>(),
        vec![MsgId(500), MsgId(501), MsgId(502)]
    );
    assert!(swaps.iter().all(|(_, m)| !m.is_sending));
    assert_eq!(
        received.lock().map(|r| r.clone()).unwrap_or_default(),
        vec!["one", "two", "three"],
        "the server got the messages in the order they were sent"
    );

    let after = live.load_slice_around(peer(), MsgId::SHOW_AT_END, 50, 0);
    let ids: Vec<i32> = after.messages.iter().map(|m| m.id.0).collect();
    assert_eq!(ids, vec![10, 11, 12, 500, 501, 502]);
}

#[test]
fn a_failed_send_keeps_the_message_with_the_clock_and_reports_the_error_once() {
    let (rpc, mut live) = chat(10..=11);
    // No answer: the fake drops the request, the runtime reports a send error.
    rpc.set_handler(|_, _| None);
    let _ = live.load_slice_around(peer(), MsgId::SHOW_AT_END, 50, 0);
    let pending = live.send_message(peer(), request("lost")).expect("send");
    assert!(pending.id.is_local() && pending.is_sending && !pending.extra.send_failed);

    let updates = collect_updates(&mut live, |u| {
        u.iter()
            .any(|x| matches!(x, HistoryLiveUpdate::LocalMessageFailed { .. }))
    });
    let failed: Vec<(MsgId, Option<String>)> = updates
        .iter()
        .filter_map(|u| match u {
            HistoryLiveUpdate::LocalMessageFailed { local_id, error } => {
                Some((*local_id, error.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(failed.len(), 1, "{updates:?}");
    assert_eq!(failed[0].0, pending.id);
    assert!(failed[0].1.is_some(), "the error is shown once");

    let after = live.load_slice_around(peer(), MsgId::SHOW_AT_END, 50, 0);
    let last = after.messages.last().expect("failed message stays");
    assert_eq!(last.id, pending.id);
    assert!(last.is_sending, "a failed message keeps the clock");
    assert!(last.extra.send_failed);

    // Deleting it removes it locally; nothing goes to the server.
    live.delete_messages(peer(), &[pending.id], false)
        .expect("delete failed message");
    assert!(rpc.calls::<tl_fns::messages::DeleteMessages>().is_empty());
    let gone = live.load_slice_around(peer(), MsgId::SHOW_AT_END, 50, 0);
    assert!(gone.messages.iter().all(|m| m.id != pending.id));
}

#[test]
fn a_reply_without_update_message_id_still_replaces_the_pending_message() {
    let (rpc, mut live) = chat(10..=10);
    rpc.set_handler(|ctor, bytes| {
        if ctor != tl_fns::messages::SendMessage::CONSTRUCTOR_ID {
            return None;
        }
        let req = tl_fns::messages::SendMessage::from_bytes(bytes).ok()?;
        let message = tl_message(70, tl_user_peer(PEER_ID), SELF_ID, &req.message, None);
        Some(
            tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                    message,
                    pts: 5,
                    pts_count: 1,
                })],
                users: Vec::new(),
                chats: Vec::new(),
                date: 1_795_340_011,
                seq: 0,
            })
            .to_bytes(),
        )
    });
    let _ = live.load_slice_around(peer(), MsgId::SHOW_AT_END, 50, 0);
    let pending = live.send_message(peer(), request("bare")).expect("send");
    let updates = collect_updates(&mut live, |u| !sent_swaps(u).is_empty());
    let swaps = sent_swaps(&updates);
    assert_eq!(swaps[0].0, pending.id);
    assert_eq!(swaps[0].1.id, MsgId(70));
    let after = live.load_slice_around(peer(), MsgId::SHOW_AT_END, 50, 0);
    let ids: Vec<i32> = after.messages.iter().map(|m| m.id.0).collect();
    assert_eq!(ids, vec![10, 70], "no stale pending copy is left");
}

#[test]
fn pages_read_with_range_queries_match_the_loaded_history() {
    let (_rpc, live) = chat(1..=300);
    let end = live.load_slice_around(peer(), MsgId::SHOW_AT_END, 60, 40);
    let ids: Vec<i32> = end.messages.iter().map(|m| m.id.0).collect();
    assert_eq!(ids, (201..=300).collect::<Vec<_>>());
    assert!(end.loaded_at_bottom);
    assert!(
        !end.loaded_at_top,
        "a window that stops above the oldest loaded message is not at the top"
    );
    assert_eq!(end.total_count, 300);

    let around = live.load_slice_around(peer(), MsgId(150), 10, 5);
    let ids: Vec<i32> = around.messages.iter().map(|m| m.id.0).collect();
    assert_eq!(ids, (140..=155).collect::<Vec<_>>());
    assert!(!around.loaded_at_bottom && !around.loaded_at_top);

    let before = live.load_slice_before(peer(), MsgId(140), 20);
    let ids: Vec<i32> = before.messages.iter().map(|m| m.id.0).collect();
    assert_eq!(ids, (120..140).collect::<Vec<_>>());
    let after = live.load_slice_after(peer(), MsgId(290), 20);
    let ids: Vec<i32> = after.messages.iter().map(|m| m.id.0).collect();
    assert_eq!(ids, (291..=300).collect::<Vec<_>>());
}

#[test]
fn opening_a_chat_reports_it_open_so_it_is_never_unloaded() {
    let (_rpc, live) = chat(1..=300);
    live.set_history_open(peer(), true);
    // The report goes through the runtime without waiting; give it a moment to land.
    std::thread::sleep(Duration::from_millis(300));
    let session = Arc::clone(live.runtime().session());
    let data_peer = DataPeerId(peer().0);
    let unloaded = live
        .runtime()
        .tokio_handle()
        .block_on(async move { session.write().await.unload_history_messages(data_peer) });
    assert_eq!(unloaded, 0, "an open chat keeps its messages");
    live.set_history_open(peer(), false);
    let session = Arc::clone(live.runtime().session());
    assert!(wait_until(|| {
        let session = Arc::clone(&session);
        live.runtime()
            .tokio_handle()
            .block_on(async move { session.write().await.unload_history_messages(data_peer) })
            > 0
    }));
}

#[test]
fn a_delete_while_a_send_is_in_flight_goes_out_after_it_and_is_not_dropped() {
    let (rpc, mut live) = chat(10..=12);
    let order = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&order);
    rpc.set_handler(move |ctor, bytes| {
        if let Ok(mut l) = log.lock() {
            l.push(ctor);
        }
        if ctor == tl_fns::messages::SendMessage::CONSTRUCTOR_ID {
            let req = tl_fns::messages::SendMessage::from_bytes(bytes).ok()?;
            std::thread::sleep(ROUND_TRIP);
            let message = tl_message(600, tl_user_peer(PEER_ID), SELF_ID, &req.message, None);
            return Some(
                tl_enums::Updates::Updates(tl_types::Updates {
                    updates: vec![
                        tl_enums::Update::MessageID(tl_types::UpdateMessageID {
                            id: 600,
                            random_id: req.random_id,
                        }),
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
                .to_bytes(),
            );
        }
        (ctor == tl_fns::messages::DeleteMessages::CONSTRUCTOR_ID).then(|| {
            tl_enums::messages::AffectedMessages::AffectedMessages(
                tl_types::messages::AffectedMessages {
                    pts: 3,
                    pts_count: 1,
                },
            )
            .to_bytes()
        })
    });
    let _ = live.load_slice_around(peer(), MsgId::SHOW_AT_END, 50, 0);
    let pending = live
        .send_message(peer(), request("in flight"))
        .expect("send");
    live.delete_messages(peer(), &[MsgId(11)], true)
        .expect("a delete queued behind a send in flight is sent, not dropped");
    let sent = order
        .lock()
        .map(|o| {
            o.iter()
                .copied()
                .filter(|c| {
                    *c == tl_fns::messages::SendMessage::CONSTRUCTOR_ID
                        || *c == tl_fns::messages::DeleteMessages::CONSTRUCTOR_ID
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert_eq!(
        sent,
        vec![
            tl_fns::messages::SendMessage::CONSTRUCTOR_ID,
            tl_fns::messages::DeleteMessages::CONSTRUCTOR_ID
        ],
        "the send / edit / delete lane keeps the user's order"
    );
    let updates = collect_updates(&mut live, |u| !sent_swaps(u).is_empty());
    assert_eq!(sent_swaps(&updates)[0].0, pending.id);
    let after = live.load_slice_around(peer(), MsgId::SHOW_AT_END, 50, 0);
    let ids: Vec<i32> = after.messages.iter().map(|m| m.id.0).collect();
    assert_eq!(ids, vec![10, 12, 600]);
}
